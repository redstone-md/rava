# Roadmap — Real OS threads → lock-free CPU multicore (the moonshot)

Goal (user, 2026-06-17): real CPU multicore bytecode execution — "faster than the
JVM" on parallel workloads. Endpoint = **lock-free** (concurrent bytecode on a
shared heap). Path = **GIL-first foundation, then relax to lock-free** (lock-free
without a working real-threads foundation = instant heap races = the user's #1
fear, the 31GB memory trauma).

## Honest scope + payoff
- MC 1.12.2 tick is SERIAL + no-adapt forbids intercepting MC → multicore gives
  **~0 MC-1.12.2 speedup**. Value = GENERIC capability (any multi-threaded jar,
  future MC, the architectural goal). Accepted by the user.
- GIL phase: real threads, but one bytecode thread at a time → IO/netty overlap
  only (CPU serial). Lock-free phase: real CPU parallelism → the "beats JVM" part.
- Multi-WEEK effort over the most memory-critical subsystem (GC + shared heap).
  Each increment gated; never destabilize main. Branch `feat/os-threads`.

## Why delicate (the core constraint)
Interpreter is `&mut Vm` recursive. A MutexGuard can't drop mid-recursion (the
`&mut` lives up the Rust stack). So the GIL = a manual lock over a raw `*mut Vm`,
CPython-style — REUSE the JIT's existing `CURRENT_VM: Cell<*mut Vm>` +
`set_current_vm`/`vm_ref` (jit.rs:451-461). GC reads raw frame pointers; with
threads they live on N Rust stacks → need a per-thread root registry.

## Hard safety gates (abort increment if any fail)
- `RAVA_HARD_MB` backstop stays; RSS ≤ green-thread baseline (~1GB no-player).
- After each increment: boot to Done + McProbe (join/play/disconnect/reconnect)
  + GC-est flat over a soak (no leak) + no crash. A GC-root bug shows as a fast
  boot crash (frees live objects) → boot-to-Done IS the root-correctness test.
- Keep green threads working until real threads fully replace them (additive).

## Increments (each independently verifiable + committed)
1. **Per-thread root registry (single-threaded, no behavior change).** Convert the
   single `Vm::frame_ptrs: Vec<usize>` (lib.rs:348; push/pop in opcodes.rs run()
   ~257 via FramePtrGuard, lib.rs:1810 jit_invoke; scanned gc() lib.rs:1072;
   backtrace lib.rs:2893) into a `thread_local FRAME_PTRS` + a global
   `Mutex<Vec<*const Vec<usize>>>` registry. gc() scans the registry (one entry
   now → identical roots). Also register exec_frames_ptr + green_threads roots
   per-thread eventually. **Gate: boot byte-identical behavior.** This de-risks
   multi-thread GC FIRST, alone, before any real thread exists.
2. **GIL primitive + one daemon on a real `std::thread`.** Global `VM_GIL:
   Mutex<()>`. Route ONE daemon from `spawn_green_thread` (lib.rs:869) to a
   std::thread that: registers its FRAME_PTRS, acquires VM_GIL, sets CURRENT_VM,
   runs run() via `*mut Vm`, releases. GC runs only under the GIL (stop-the-world,
   safe). Prove cross-thread roots + lock hand-off. **Gate: boot + leak.**
3. **Release GIL on blocking native IO** (net.rs socket read/write, nio file IO,
   Thread.sleep): drop the guard, do the syscall, re-acquire → real IO overlap
   (ChunkIOExecutor/netty on cores). **Gate: IO overlap visible, no races.**
4. **All daemons → real threads; retire green-thread scheduler** (delete
   GreenThread / run_green_threads_slice once 2-3 stable).
5. **Relax GIL → lock-free CPU parallelism (the endpoint).** The dangerous part —
   only after 1-4 are rock-solid. Needs: heap mutation sync (Mutex/sharded heap or
   per-thread nurseries), `RefCell` class statics → `Mutex`/atomics
   (class_loader.rs:16-38), a CONCURRENT or write-barrier GC (current is
   stop-the-world; concurrent mark needs barriers on putfield/aastore/putstatic),
   atomic `native_depth`. ~50 sync sites. Do per-subsystem, each behind a gate +
   a stress bench (parallel-heavy jar with a SUM check). This is where "beats JVM"
   on parallel workloads materializes.

## PROGRESS (2026-06-17, branch feat/os-threads)
- **Sub-step 1 DONE + verified (commit 075f62c): GIL primitive.** `lib.rs::gil`
  (Mutex + thread-local 'static guard; acquire/release; REAL_THREADS counter +
  has_real_threads). Main acquires at `run_main_with_args`; Thread.sleep
  releases/yields/re-acquires ONLY when has_real_threads (guard → single-thread
  boot identical, 248s). KEY SIMPLIFICATION found: frames stack LIFO on the
  SHARED `Vm::frame_ptrs` (non-holder blocked, its frames alive) → GC scans the
  union with **NO GC change** → big risk removed. Inert until daemons go real.
- **Sub-step 2 (NEXT, the dangerous concurrent leap — do FRESH, stress-test):**
  route the FIRST daemon (reflect.rs Thread.start `is_daemon` branch, ~2199,
  instead of spawn_green_thread) to a real `std::thread`. INTERLOCKING correctness
  (all required together):
  1. `spawn_os_thread(recv_handle, thread_obj)`: std::thread capturing a
     `SendPtr(*mut Vm)` (unsafe Send); in-thread: gil::thread_started();
     gil::acquire(); jit::set_current_vm(p); set thread-local currentThread =
     thread_obj; `(&mut *p).apply(&Value::Ref(recv), vec![])`; gil::release();
     gil::thread_finished().
  2. **Per-thread `currentThread()` MUST become thread_local** (today it's the
     shared `Vm::main_thread`). Else the daemon's currentThread returns the server
     thread → MC's `currentThread()==primaryThread` async-guards MISFIRE → daemon
     runs primary-only ops off-thread → corruption. Set TL currentThread for main
     (server thread) + each daemon.
  3. **Shutdown UAF flag** (gil module): main sets it before run_main returns;
     daemon checks after acquire → if shutting down, release + exit WITHOUT
     touching the (dropping) Vm.
  4. Deadlock watch: a thread holding the GIL must never BUSY-wait on another
     thread (must Thread.sleep, which releases). MC wait-loops use sleep → OK, but
     VERIFY (boot + McProbe + a /stop + reconnect + soak; watch for hang).
  5. Keep other daemons green for this milestone (first daemon real only) to bound
     blast radius; convert the rest (sub-step 4) once stable.

## Parallel safe track (independent, already started)
Native auto-parallelism of pure-data JDK ops (no per-element bytecode lambda):
Arrays.sort done (commit 50e0f53). Extend to primitive stream reduce / big
fill/equals as found — SAFE (rust-side, no heap race), generic, no GIL needed.

See memory [[rava-perf-multicore]] [[rava-threading]].
