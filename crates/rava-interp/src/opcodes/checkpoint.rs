//! The periodic interpreter checkpoint.
//!
//! Runs once every CHECK_INTERVAL opcodes, never on the fast path: restock the countdown,
//! poll the GIL-free abort and safepoint, run the heartbeat, and enforce the memory limit.
//! `None` = keep interpreting, `Some(exit)` = leave `run`.

use crate::opcodes::*;

/// The hot-array/`aastore` work of this round.
///
/// ⚠**DECLARED HERE, NOT IN `opcodes/mod.rs`, ONLY BECAUSE `mod.rs` BELONGS TO ANOTHER EDITOR IN
/// THIS ROUND.** `#[path]` on a module declared in `opcodes/checkpoint.rs` resolves relative to
/// the directory holding that file, so this is literally `opcodes/arrays.rs` and nothing about it
/// is checkpoint-specific. `opcodes/mod.rs` already does `pub(crate) use checkpoint::*;`, so the
/// module still reaches every caller under its proper name, **`crate::opcodes::arrays`** — moving
/// the declaration into `mod.rs` later is a pure two-line move that no call site notices.
#[path = "arrays.rs"]
pub(crate) mod arrays;

/// EVERY interpreted opcode, on EVERY thread. `Vm::op_total` counts only the GIL-holding arm
/// below, so it cannot answer "how much of this boot is interpretation?" — a gil-free worker
/// grinding an interpreted subtree is invisible to it, and that is precisely the population the
/// question is about. One relaxed add per CHECK_INTERVAL (100,000) opcodes is free.
pub(crate) static OPS_ALL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Total interpreted opcodes across all threads, to within CHECK_INTERVAL per live frame.
pub fn ops_all() -> u64 {
    OPS_ALL.load(std::sync::atomic::Ordering::Relaxed)
}

/// The two diagnostics the heartbeat consults, read ONCE instead of once per beat.
///
/// Both were `std::env::var*` calls sitting inside the heartbeat arm — a lookup that allocates
/// (`var` a `String`, `var_os` an `OsString`) and takes the process-wide environment lock to
/// re-answer a question fixed before `main` ran. Nothing here grows with the workload, so this is
/// the cheapest possible entry in the "work repeated for a constant of the PROCESS" column — but
/// it is that shape, it puts the safepoint poll back to a load and a predictable branch, and it
/// takes `checkpoint` out of the allocation census entirely.
///
/// `RAVA_HEARTBEAT_BT` also moves from `var(..).is_ok()` to `var_os(..).is_some()`, which is what
/// every other flag in this engine uses: a presence test has no business failing because the
/// value the user set is not valid UTF-8.
fn heap_dump() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_HEAP_DUMP").is_some())
}

fn heartbeat_bt() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_HEARTBEAT_BT").is_some())
}

/// The transitional `&SharedVm -> &mut Vm` escape for this safepoint's genuinely-`&mut` leaves —
/// the SAME consolidated cast the run loop and the invoke* family spell at their sites (see
/// `run.rs::vm_mut`, `invoke.rs::vm_mut`, `hot.rs::vm_mut`). Everything else in [`checkpoint`]
/// is already `&self`-safe on the shared reference: the GIL-free arm polls only atomics
/// (`abort_requested`, `parallel::safepoint`), and the GIL arm's heartbeat, prof samples, JIT
/// stats, backtrace, mem-limit and heap reads all run through `&self`. Three leaves still need
/// exclusivity until the interpreter loop is `&self`: the `op_total` counter, `Vm::maybe_gc`
/// (the collector), and the network-pump block (`next_pump_op`/`in_pump` writes +
/// `pump_network`, which drives netty bytecode that allocates and collects). The shared writes
/// behind them are GIL-serialized — this arm runs only on the GIL holder; the `gilfree()` arm
/// above never reaches them — which is why this cast is the right bridge rather than a
/// shared-mutable cell.
#[inline(always)]
#[allow(clippy::mut_from_ref, invalid_reference_casting)]
fn vm_mut(vm: &crate::vm::thread_context::SharedVm) -> &mut Vm {
    unsafe {
        &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm)
    }
}

/// ⚠**`#[inline(never)]` IS LOAD-BEARING, AND IT IS ABOUT `run`'s STACK FRAME, NOT ABOUT THIS
/// FUNCTION.** Everything below runs once per CHECK_INTERVAL (100,000) opcodes, so its own cost
/// is unmeasurable — but it carries `format!`, `eprintln!` and a heap-composition dump, and if
/// LLVM folds any of that into the op loop it is the op loop's `sub rsp` that grows, on every
/// interpreted call. This tree measured six `#[inline(never)]`s of exactly that shape at
/// **-21.23% disjoint**. `#[cold]` states the branch weight the call site already has.
#[inline(never)]
#[cold]
pub(crate) fn checkpoint(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    trace_ops: &Option<String>,
    heartbeat: bool,
    mem_limit: usize,
) -> Result<Option<RunExit>, VmError> {
        OPS_ALL.fetch_add(CHECK_INTERVAL as u64, std::sync::atomic::Ordering::Relaxed);
        crate::frames::op_countdown_restock();
        if crate::parallel::gilfree() {
        crate::vm::thread_context::add_op_total(CHECK_INTERVAL as u64);
        // P3-S1 ABORT EXIT (primitive #6): this arm used to poll ONLY the never-armed
        // STOP flag — a locked-exec worker grinding a long interpreted subtree had NO
        // abort exit at all and ran to subtree completion (minutes, serialized), which
        // presented as the rotating "stuck" worker the scope join waited on. Exit via
        // the armored NurseryFull path: the invoke machinery's gilfree guard converts
        // it to a clean region abort and the worker unwinds (results discarded).
        if crate::parallel::abort_requested() {
            return Ok(Some(RunExit::NurseryFull));
        }
        // GIL-free offload worker (parallel worldgen): poll the safepoint
        // ONLY — GC, the network pump, and the GIL hand-off are the GIL
        // holder's job, and this thread must not touch shared Vm state
        // outside its own arena + immutable reads.
        crate::parallel::safepoint();
    } else {
        vm_mut(vm).op_total += CHECK_INTERVAL as u64;
        let n = vm.op_total;
        prof_sample(&frame.class.file.this_class);
        // Spark-style time-weighted profile + heap timeline (RAVA_PROF). Sample the
        // top frame only (cheap — no full-stack walk); the call tree (depth) is
        // gated behind RAVA_PROF_TREE for the deep-recursion cost.
        if crate::prof::enabled() {
            if crate::prof::tree_enabled() {
                vm.prof_tick();
            } else {
                let mem = (
                    (crate::nextgen::arena_old_bytes() / 1024) as u64,
                    (crate::mem_meter::live() / (1024 * 1024)) as u64,
                    crate::nextgen::gc_epoch(),
                );
                crate::prof::sample(
                    &[frame.class.file.this_class.as_str()],
                    CHECK_INTERVAL as u64,
                    mem,
                );
            }
        }
        // Interval is configurable (`RAVA_HEARTBEAT_OPS`, default 20 M) because the
        // DEFAULT never fired on a real boot: a full server boot executes fewer
        // interpreted bytecodes than one heartbeat period, so the op-rate line — the
        // only instrument that can say whether this engine is interpreter-bound —
        // reported nothing at all. Reading a zero from it was reading silence.
        let hb_ops: u64 = {
            static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
            *V.get_or_init(|| {
                std::env::var("RAVA_HEARTBEAT_OPS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .filter(|&v: &u64| v >= crate::opcodes::CHECK_INTERVAL as u64)
                    .unwrap_or(20_000_000)
            })
        };
        if heartbeat && n % hb_ops == 0 {
            let rate = hb_oprate(n);
            // `allM` is every thread's interpreted opcodes; `n` is only this GIL-holding one's.
            // The pair is what says whether interpretation is on the critical path at all.
            eprintln!("[HB] {}M ops (all-threads {}M) ({:.1}M op/s); in {} pc={} | jit={}/{}", n / 1_000_000, ops_all() / 1_000_000, rate / 1e6, frame.class.file.this_class, frame.pc, vm.jit_compiled, vm.jit_cap);
            if n % 100_000_000 == 0 {
                dump_profile();
                // Periodic JIT-STATS too (opt-in flag): a run killed while
                // serving (or crashed) otherwise never dumps, and compile-set
                // diffs need the full list from BOTH runs of an A/B.
                vm.dump_jit_stats();
                // C1 RAM breakdown (RAVA_HEAP_DUMP): the handle-heap composition
                // (objects/arrays/maps by class + bytes) — where RSS goes when the
                // arena tenured is tiny (measured 73MB of a 1.6GB RSS).
                if heap_dump() {
                    eprint!("{}", vm.heap.composition());
                }
            }
            if crate::prof::enabled() && n % crate::prof::dump_every_ops() == 0 {
                crate::prof::dump();
            }
            if vm.jit_enabled {
                let cbs = crate::jit::callback_stats();
                if !cbs.is_empty() {
                    eprintln!("[CB] {cbs}");
                }
            }
            if heartbeat_bt() {
                eprintln!("{}", vm.backtrace());
            }
            // Periodic JIT-stats dump (RAVA_JIT_STATS) so we can read
            // compile/reject reasons from a LONG-RUNNING server (the CLI
            // only dumps on a clean exit, which a server never reaches).
            if vm.trace.jit_stats && n % 200_000_000 == 0 {
                vm.dump_jit_stats();
            }
        }
        // Drive the GC on REAL process memory — the counting allocator's
        // live-byte meter (rava_interp::mem_meter). The old trigger used
        // Heap::approx_bytes()*2, which still undercut true RSS ~2.5x (box
        // tables, Str payloads, Vec slack, allocator overhead) and let the
        // heap reach ~5x the estimate before collecting — so workloads the
        // estimate undercounts (autoboxing, Str-heavy) ballooned (HashMap
        // bench 1.6GB at a tiny live set). Fall back to the estimate only
        // when no meter is installed (embedders/unit tests).
        // Two independent thresholds, because the two pressure shapes need
        // different sensors: the estimate*2 tracks logical Java-heap bytes
        // (fires early on object-count churn so the non-shrinking slot tables
        // stay small — Allocs), while the real meter tracks process bytes
        // (catches boxed/Str the estimate undercounts ~5x — Collections). A
        // single metric ratchets the other up; keeping them separate bounds
        // RAM under both. Either crossing its mark collects.
        // Collect at a safe point only (no re-entrant native holding roots in
        // Rust locals). The threshold check + re-arm now lives in Vm::maybe_gc, so
        // the JIT safepoint (rava_arena_safepoint) polls the SAME policy: this
        // trigger alone is blind under a JIT-heavy workload, which is how 3.64M
        // heap objects accumulated to 2200MB RSS.
        let freed = vm_mut(vm).maybe_gc();
        if freed > 0 && heartbeat {
            let est_post = vm.heap.approx_bytes().saturating_mul(2);
            let real_post = crate::mem_meter::live();
            let (fb, ab) = vm.heap.box_table_len();
            eprintln!("[GC] freed {}MB, est ~{}MB real ~{}MB | box: field={fb} array={ab}", freed / (1024 * 1024), est_post / (1024 * 1024), real_post / (1024 * 1024));
        }
        // Service networking continuously, not only on Thread.sleep, so
        // the engine behaves like a real server's IO thread: connections
        // are accepted/read even while the main thread is busy (e.g.
        // worldgen, which never sleeps). Throttled by op count and guarded
        // against re-entry (the pump runs netty bytecode that re-enters
        // this checkpoint). Only at a safe point (native_depth == 0).
        if crate::frames::native_depth() == 0 && !vm.in_pump && !vm.net_selectors.is_empty()
            && vm.op_total >= vm.next_pump_op
        {
            // The pump's writes (`next_pump_op`, `in_pump`) and `pump_network` itself (netty
            // bytecode that allocates and collects) are this block's genuinely-`&mut` leaves;
            // the guard above stays on the shared reference. One derivation, consumed inside
            // the block — exactly the exclusivity the old `&mut Vm` parameter provided.
            let vm = vm_mut(vm);
            vm.next_pump_op = vm.op_total + 1_000_000;
            vm.in_pump = true;
            crate::net::pump_network(vm);
            vm.in_pump = false;
        }
        // Fairness: with a real daemon OS thread present, periodically hand
        // off the GIL at this safe point so neither thread starves the other
        // (covers a daemon or main that computes long without Thread.sleep).
        // ONLY in recursive mode (!trampoline): the execution-mode state
        // (trampoline/pending/yield_request) is shared, so a hand-off must happen
        // only when it is in the clean recursive state both threads expect —
        // never mid green-thread slice.
        // `RAVA_HANDOFF_STAT` — WHICH of the four conditions blocks the GIL hand-off?
        //
        // ⭐Rows 367-376 spent seven designs on getting a `supplyAsync` body off the calling
        // thread and every one hung, and row 375 concluded "during loading the main thread never
        // parks". **That was an inference.** The hand-off below already exists and fires every
        // CHECK_INTERVAL opcodes — if its guard lets it. Four conditions can veto it, and nobody
        // has ever counted which one does. This tallies each refusal separately, because a guard
        // with four clauses and no census is exactly the shape that hid the lane's compiler gate
        // for a year (row 374).
        {
            let (nd, pump, tramp, thr) = (
                crate::frames::native_depth() == 0,
                !vm.in_pump,
                !vm.trampoline,
                crate::gil::has_real_threads(),
            );
            if crate::gil::handoff_stat() {
                crate::gil::note_handoff(nd, pump, tramp, thr);
            }
            if nd && pump && tramp && thr {
                crate::gil::release();
                std::thread::yield_now();
                crate::gil::acquire();
            }
        }
        // Hard abort if the heap estimate blew past the cap even after GC.
        // Kept on the estimate (not real bytes) so the default cap's
        // effective real headroom is unchanged — a real-bytes cap here would
        // OOM the worldgen burst on the 4GB default. NOT a catchable
        // Throwable — a broad `catch Throwable` (CrashReport.preload) would
        // swallow an OutOfMemoryError and keep allocating.
        if mem_limit != 0 && vm.heap.approx_bytes().saturating_mul(2) > mem_limit {
            eprintln!("[OOM] Rava heap limit (RAVA_XMX_MB) exceeded after GC: ~{} MB live. Raise RAVA_XMX_MB.", vm.heap.approx_bytes().saturating_mul(2) / (1024 * 1024));
            // Name the guest method bursting the heap (native_depth() > 0 means a
            // native op is applying guest bytecode in a loop where the op-loop GC
            // above is gated off — the transient-burst OOM). The backtrace pins the
            // exact stream/collect/scan op to make GC-safe or stream.
            eprintln!("[OOM-SITE] native_depth={} at:\n{}", crate::frames::native_depth(), vm.backtrace());
            eprint!("{}", vm.heap.composition());
            return Err(VmError::UnsupportedNative("heap limit (RAVA_XMX_MB) exceeded".into()));
        }
    }
    Ok(None)
}
