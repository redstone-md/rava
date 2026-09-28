# Green threads (cooperative) — roadmap

## Goal
Run Java threads (`Thread.start`) as **cooperative green threads on the explicit
`execute_stackful` stack** — single OS thread, no GIL, no `unsafe` aliasing,
bounded memory. Immediate driver: MC's `FileIOThread` async-chunk-save daemon
deadlocks the single-threaded inline model (`b()` spins on `while(queued!=written)
Thread.sleep`, daemon never runs) → client stuck at "Loading terrain".

## Why cooperative, not real-threads+GIL
A GIL serializes all bytecode (Python-style) → zero speedup for MC's
bytecode-heavy worldgen, i.e. does not address the one-core pain. Both models
need the SAME prerequisite (`execute_stackful` primary, so no `&mut Vm` is held
across a suspend point); cooperative just adds a scheduler, real-threads adds
cross-thread GC roots + lock plumbing + per-thread stacks. Cooperative is
strictly safer and less work for the same deadlock fix. True multi-core
(GIL-free parallel heaps) stays a separate later effort.

## Foundation (validated)
`RAVA_STACKFUL=1` routes `main()` through `execute_stackful`. Empirically the
explicit stack survives the full Paper 1.12.2 boot — advancement load (caught
exceptions → explicit-stack unwind works), worldgen (lambdas, JIT). So the
explicit stack is viable as the primary path.

## Design
1. `RunExit::Yield` — new variant alongside `Normal`/`Call`. In trampoline mode
   `invokestatic Thread.sleep` / `invokevirtual Object.wait` advance pc, set
   `vm.yield_request`, and return; the `run` loop top returns `Yield` (mirrors
   the existing `vm.pending` → `RunExit::Call` suspend at opcodes.rs:267).
2. Scheduler — `drive_stack` generalized to hold a `Vec` of green threads, each
   = its own `frames` vec + run state + thread heap-handle. On `Yield`: park the
   current coroutine, round-robin to the next ready one; resuming a slept
   coroutine just continues past the (already pc-advanced) sleep.
3. `Thread.start` — instead of inline `vm.apply`, register a new green thread
   (build the runnable's entry frame) for the scheduler to run.
4. GC — `exec_frames_ptr` (single) becomes a registry of ALL parked coroutines'
   frame-vec pointers so mark-sweep reaches every green thread's roots.

## Memory safety
One OS thread → no `*mut Vm` aliasing, no data races. Frames are heap `Vec`s
already counted by `mem_meter`. Bounded: #coroutines ≈ #live Java threads; each
stack capped by `RAVA_MAX_DEPTH`. `RAVA_HARD_MB` allocator ceiling remains the
backstop.

## Gap
Natives that call a Java callback and then block can't yield mid-native (their
stack isn't on the explicit stack). Fine for `FileIOThread.run`/`b()` which call
`Thread.sleep` directly at a frame boundary. Revisit if a real case needs it.

## Then
Remove the no-adapt-violating daemon skips (reflect.rs FileIOThread/Watchdog/
Snooper/Infinisleeper; net.rs FileIOThread neutralization) once daemons run for
real. Keep io/netty handling (3rd-party lib).
