//! The Global Interpreter Lock. Real OS threads share ONE `Vm`; only the lock
//! holder runs bytecode or touches the heap, so the heap stays single-writer
//! (no data races, GC stays stop-the-world) while threads still run truly in
//! parallel during blocking IO (each releases the GIL and blocks on its own
//! core). Held via a thread-local 'static guard so it can be dropped mid-
//! recursion — a `MutexGuard` can't be threaded back up the `&mut Vm` recursive
//! stack — CPython-style. Frames stack LIFO on the shared `Vm::frame_ptrs` (the
//! non-holder is blocked with its frames alive), so GC scanning `frame_ptrs`
//! sees the correct union of every thread's roots with no GC change.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
static GIL: Mutex<()> = Mutex::new(());
/// Count of live real daemon OS threads. While 0 (the single-threaded case)
/// the GIL is uncontended, so the per-Thread.sleep release/yield is pure
/// overhead and is skipped — boot stays identical to the green-thread build.
static REAL_THREADS: AtomicUsize = AtomicUsize::new(0);

/// ⭐**THE SERIAL BOOT ASKS "IS THERE A SECOND THREAD?" MILLIONS OF TIMES AND THE ANSWER IS A
/// PROCESS CONSTANT, SO IT SHOULD COST WHAT A CONSTANT COSTS.**
///
/// `has_real_threads()` is the single most-asked question in the engine: the op-loop checkpoint
/// asks it every CHECK_INTERVAL opcodes, `monitorenter`/`monitorexit` ask it per lock op, the
/// class-init lock asks it per static access on a class that finished initialising twenty seconds
/// ago, and the GIL release path asks it per release. It was a **SeqCst load of a counter that
/// other threads `fetch_add`** — i.e. the one atomic form LLVM may not reorder anything across,
/// on a read-mostly value living in a line that is written by an RMW.
///
/// WAS: one SeqCst load of an RMW-written counter per ask, per ask forever.
/// IS: one **relaxed** load of a **write-once, cache-line-isolated latch** that is `NEVER` for the
/// entire single-threaded boot and, once any real OS thread has ever started, is `THREADED`
/// **for the rest of the process** — after which every ask falls through to the ORIGINAL exact
/// SeqCst count and nothing about the threaded engine changes. The latch is monotone: it never
/// returns to `NEVER`, so it can only ever *withdraw* a fast path, never re-arm one.
///
/// **Why a relaxed read of it is sound, and not an assumption.** The dangerous direction is a
/// stale `NEVER` while a second thread is live. It cannot happen where it would matter, because
/// of what every thread-start site does (`vm/threads.rs`, `task_runtime.rs`): `thread_started()`
/// and only THEN `gil::acquire()`. A second thread therefore cannot execute one guest opcode,
/// touch the heap, enter a monitor or run a `<clinit>` until it holds the GIL. So during any
/// window in which this load can still be stale, the other thread is parked on a mutex and the
/// answer "no other thread is running guest code" is OPERATIONALLY TRUE. The instant the GIL is
/// handed over, the mutex release/acquire pair is a happens-before edge that orders the latch's
/// SeqCst store before the next reader's load, and a load may not read a value older than one a
/// happens-before edge has published — so the first ask after the very first hand-off already
/// sees `THREADED`. ⇒ the relaxation is guarded by a fact that becomes false the instant a
/// second thread can observe anything, which is the requirement.
///
/// ⚠**`release()` deliberately does NOT use the latch** — see the note there: its TLAB-cursor
/// retirement is not a "no-op either way" decision, so it keeps the exact count.
///
/// Total size: 64 bytes of `.bss` (one cache line, on purpose). It is aligned into a line of its
/// own so this read-mostly byte never shares a line with `REAL_THREADS` or the `ACQ_*` census
/// counters, which threads RMW — a read that costs a coherence bounce is not a cheap read.
#[repr(align(64))]
struct ThreadLatch(AtomicU8);
const ST_UNPROBED: u8 = 0;
const ST_NEVER: u8 = 1;
const ST_THREADED: u8 = 2;
static THREAD_STATE: ThreadLatch = ThreadLatch(AtomicU8::new(ST_UNPROBED));

/// `RAVA_NO_FAST_LOCKING=1` (or `RAVA_NO_EMIT_MORE`) → the latch is pinned to `THREADED` at its
/// one and only probe, so every question below takes the historical exact path and this whole
/// area reverts to the pre-round code with no per-ask branch left behind.
pub(crate) fn fast_locking_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_LOCKING").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Cold, runs at most once per process: read the kill switch and settle the latch. The CAS (not a
/// plain store) is what makes a thread starting *during* the probe safe — `thread_started` writes
/// `THREADED` unconditionally, so whichever of the two lands second, the final value is
/// `THREADED`; `NEVER` can only be installed from a still-`UNPROBED` latch.
#[inline(never)]
#[cold]
fn probe_thread_state() -> bool {
    let live = REAL_THREADS.load(Ordering::SeqCst) > 0;
    if live || !fast_locking_enabled() {
        THREAD_STATE.0.store(ST_THREADED, Ordering::SeqCst);
    } else {
        let _ = THREAD_STATE.0.compare_exchange(
            ST_UNPROBED,
            ST_NEVER,
            Ordering::SeqCst,
            Ordering::Relaxed,
        );
    }
    live
}

/// True while NO real OS thread has ever existed in this process (and the fast path is enabled).
/// The one-load form of "nothing in this engine can be concurrent", for callers whose entire
/// body is a no-op in that case — they get to skip building the argument to the no-op too.
#[inline]
pub(crate) fn serial_locking() -> bool {
    THREAD_STATE.0.load(Ordering::Relaxed) == ST_NEVER
}

#[inline]
pub(crate) fn thread_started() {
    REAL_THREADS.fetch_add(1, Ordering::SeqCst);
    // Ordered BEFORE this thread's `gil::acquire()` by program order and by its own SeqCst-ness,
    // and never undone: one real thread is enough to retire the fast path for good.
    THREAD_STATE.0.store(ST_THREADED, Ordering::SeqCst);
}
pub(crate) fn thread_finished() {
    REAL_THREADS.fetch_sub(1, Ordering::SeqCst);
    // The latch is NOT lowered here on purpose. `has_real_threads()` still goes exact below, so
    // the answer is unchanged; what stays retired is only the right to short-circuit — a monitor
    // taken while a worker was live must still be releasable after it dies (`monitor::exit`).
}
#[inline]
pub(crate) fn has_real_threads() -> bool {
    let s = THREAD_STATE.0.load(Ordering::Relaxed);
    if s == ST_NEVER {
        return false;
    }
    if s == ST_UNPROBED {
        return probe_thread_state();
    }
    // Threaded (or the kill switch is set): the ORIGINAL question, unweakened.
    REAL_THREADS.load(Ordering::SeqCst) > 0
}

/// DIAGNOSTIC (RAVA_GILSTAT=1; inert otherwise): direct GIL-contention accounting.
/// [[rava-parity-arithmetic]] measured the real boot at 0.98 cores with MC's chunk
/// pool live and CONCLUDED the pool collapses onto the GIL. That is an inference from
/// a core count, never a measurement of the GIL itself — and the same arc has already
/// spent three refutations on unmeasured premises. These counters test it directly:
///   • acquires that actually CONTEND (a second thread wanted the GIL while held), and
///   • how long the loser blocks.
/// A near-zero contended count REFUTES the thesis outright — it would mean the threads
/// never even reach the GIL together, and the serialisation is somewhere else (e.g. the
/// green scheduler never runs pool threads concurrently in the first place). Which of
/// those two is true decides whether `Vm: &mut self` sharding is worth its blast radius.
static ACQ_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ACQ_CONTENDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ACQ_BLOCKED_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ACQ_MAX_BLOCK_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn gilstat_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_GILSTAT").is_some())
}

/// (acquires, contended, blocked_ns, max_block_ns, live_real_threads)
pub(crate) fn gil_stats() -> (u64, u64, u64, u64, usize) {
    (
        ACQ_TOTAL.load(Ordering::Relaxed),
        ACQ_CONTENDED.load(Ordering::Relaxed),
        ACQ_BLOCKED_NS.load(Ordering::Relaxed),
        ACQ_MAX_BLOCK_NS.load(Ordering::Relaxed),
        REAL_THREADS.load(Ordering::Relaxed),
    )
}

thread_local! {
    /// True on a daemon OS thread. Such a thread must NOT JIT-compile: each
    /// compiled method's `JITModule` mmap is thread-local and is freed when the
    /// thread exits, which would leave `jit_cache` pointers into freed code that
    /// other threads then call (use-after-free). A worker still RUNS methods the
    /// main thread compiled (those live in main's never-freed module).
    static WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
pub(crate) fn set_worker() {
    WORKER.with(|w| w.set(true));
}
pub(crate) fn is_worker() -> bool {
    WORKER.with(|w| w.get())
}

/// DIAGNOSTIC: count of rava_dispatch_miss calls taken on a WORKER thread — i.e. the
/// per-dispatch acquisitions of VM_RESOLVE_LOCK during a parallel region. Tests the
/// force-compile thesis directly: if force-compiling the whole tree really zeroed the
/// PIC misses, this stays ~0 across a parallel FILL. A large count means the megamorphic
/// compute dispatch still evicts the fixed-way PIC and every dispatch serialises on the
/// global lock — which force-compiling the CALLEES cannot fix (the miss is eviction, not
/// an uncompiled target). Always incremented (one relaxed add on the already-slow miss
/// path); read only at census.
static WORKER_DISPATCH_MISS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) fn worker_miss_inc() {
    WORKER_DISPATCH_MISS.fetch_add(1, Ordering::Relaxed);
}
pub(crate) fn worker_miss_count() -> u64 {
    WORKER_DISPATCH_MISS.load(Ordering::Relaxed)
}
thread_local! {
    static HELD: RefCell<Option<MutexGuard<'static, ()>>> = const { RefCell::new(None) };
}
/// `RAVA_HANDOFF_STAT` — per-clause census of the op-loop GIL hand-off (see
/// `opcodes/checkpoint.rs`). Counts the checkpoint visits that took the hand-off and, for the rest,
/// WHICH clause vetoed. Inert unless set.
static HO_TOTAL: AtomicUsize = AtomicUsize::new(0);
static HO_TAKEN: AtomicUsize = AtomicUsize::new(0);
static HO_NATIVE: AtomicUsize = AtomicUsize::new(0);
static HO_PUMP: AtomicUsize = AtomicUsize::new(0);
static HO_TRAMP: AtomicUsize = AtomicUsize::new(0);
static HO_NOTHREADS: AtomicUsize = AtomicUsize::new(0);

pub fn handoff_stat() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_HANDOFF_STAT").is_some())
}

pub(crate) fn note_handoff(no_native: bool, no_pump: bool, no_tramp: bool, threads: bool) {
    // ⚠**PRINTS PERIODICALLY, BECAUSE A MINECRAFT SERVER NEVER EXITS.** This is the SIXTH
    // instrument in this session whose report was wired only to a clean exit and therefore
    // produced nothing on the one workload the project measures (the shape census, the taskgraph
    // probe, the PC profiler, `RAVA_FLAT_STAT`'s first wiring, the first RSS harness, and this).
    // ⇒ **On this benchmark, a census that only reports at exit does not report.** Every new one
    // gets a periodic dump by default.
    {
        let n = HO_TOTAL.load(Ordering::Relaxed);
        if n > 0 && n % 200 == 0 {
            eprintln!("{}", handoff_report());
        }
    }
    HO_TOTAL.fetch_add(1, Ordering::Relaxed);
    if no_native && no_pump && no_tramp && threads {
        HO_TAKEN.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // Every failing clause is counted, not just the first: the question is which are ever true.
    if !no_native { HO_NATIVE.fetch_add(1, Ordering::Relaxed); }
    start_handoff_watchdog();
    if !no_pump { HO_PUMP.fetch_add(1, Ordering::Relaxed); }
    if !no_tramp { HO_TRAMP.fetch_add(1, Ordering::Relaxed); }
    if !threads { HO_NOTHREADS.fetch_add(1, Ordering::Relaxed); }
}

/// ⭐**PRINT IT ON A WATCHDOG, BECAUSE THE RUN THIS IS FOR NEVER EXITS.**
///
/// `handoff_report` is wired to the clean-exit dump, and the question it exists to answer —
/// *which clause vetoes the GIL hand-off while `Util.blockUntilDone` spins?* — can only be asked
/// of a boot that HANGS. Row 394 recorded six instruments that produced nothing for exactly this
/// reason; this is the seventh, fixed rather than rediscovered.
///
/// Armed from the first checkpoint that counts, so it costs nothing when the census is off.
/// `RAVA_HANDOFF_MS` sets the period (default 15 s).
pub fn start_handoff_watchdog() {
    if !handoff_stat() {
        return;
    }
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let ms = std::env::var("RAVA_HANDOFF_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(15_000);
    std::thread::Builder::new()
        .name("rava-handoff-watch".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let r = handoff_report();
            if !r.is_empty() {
                eprintln!("{r}");
            }
        })
        .ok();
}

pub fn handoff_report() -> String {
    if !handoff_stat() {
        return String::new();
    }
    let t = HO_TOTAL.load(Ordering::Relaxed);
    if t == 0 {
        return String::new();
    }
    let pct = |v: usize| v as f64 * 100.0 / t as f64;
    // The hand-off census and the park census answer two halves of one question — how often the
    // GIL is offered, and how often taking it back cost a trip through the scheduler — so they
    // print together on the same watchdog.
    format!(
        "{}
         [HANDOFF] {t} op-loop checkpoints
         [HANDOFF]   took the hand-off {:>12}  {:5.2}%
         [HANDOFF]   vetoed: in a native {:>10}  {:5.2}%
         [HANDOFF]   vetoed: in the pump {:>10}  {:5.2}%
         [HANDOFF]   vetoed: trampoline  {:>10}  {:5.2}%
         [HANDOFF]   vetoed: no threads  {:>10}  {:5.2}%",
        park_census(),
        HO_TAKEN.load(Ordering::Relaxed), pct(HO_TAKEN.load(Ordering::Relaxed)),
        HO_NATIVE.load(Ordering::Relaxed), pct(HO_NATIVE.load(Ordering::Relaxed)),
        HO_PUMP.load(Ordering::Relaxed), pct(HO_PUMP.load(Ordering::Relaxed)),
        HO_TRAMP.load(Ordering::Relaxed), pct(HO_TRAMP.load(Ordering::Relaxed)),
        HO_NOTHREADS.load(Ordering::Relaxed), pct(HO_NOTHREADS.load(Ordering::Relaxed)),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// THE PARK BEFORE THE GIL
// ─────────────────────────────────────────────────────────────────────────────
//
// ⭐**`ZwWaitForAlertByThreadId` IS 1.73% OF THE CALLBACK PROFILE, AND IT IS A THREAD THAT WENT
// TO SLEEP WITH NO SPIN IN FRONT OF IT.** That symbol is not a lock, it is Windows' *blocking*
// primitive: `SRWLOCK`'s contended path (`std::sync::Mutex` on this target) and the condition
// variable both end in `RtlpWaitOnAddress` → `NtWaitForAlertByThreadId`, and the wake is a
// matching `NtAlertThreadByThreadId`. Reaching it costs two ring transitions plus a scheduler
// round trip; on a boot that runs at 1.10 cores that time is not overlapped by anything.
//
// **Where the engine walks into it.** Every one of these is a `release()` … `acquire()` bracket
// whose re-acquire is a blocking wait on a lock another thread may hold for a few hundred
// nanoseconds or for a whole guest method — the caller cannot tell which:
//   * `opcodes/checkpoint.rs` — the fairness hand-off, `release(); yield_now(); acquire();`
//     every CHECK_INTERVAL opcodes whenever a real OS thread exists;
//   * `vm/threads.rs::wait_for_tracked_future` — the async-offload join loop, which wakes on
//     ANY completion epoch and re-acquires to re-read one future's `$done`;
//   * `monitor.rs` — `park_without_gil`, contended `monitorenter`, and `Object.wait`;
//   * `task_runtime.rs::WorkerTaskScope::enter` — every offloaded task's first act.
//
// **What changes here, and what deliberately does not.** Only the WAITING. `acquire()` still
// ends in the same `GIL.lock()` on the same `Mutex`, so the lock's ownership, ordering and
// poisoning behaviour are untouched, the GIL is still the sole heap-mutation gate, and nothing
// about who may run guest code moves. What is inserted in front of the block is a **bounded,
// self-tuning spin**: the short holds (a hand-off nobody was waiting for, a worker that took the
// GIL only to publish a completion) are caught without ever entering the kernel, and the long
// holds are *learned* — the budget halves on every spin that fails, so a regime of whole-method
// holds decays to an almost-immediate park instead of burning cores.
//
// The self-tuning is what makes this safe to leave on. A fixed spin is a bet on the hold-time
// distribution, and this engine has two of them (sub-microsecond hand-offs, and multi-millisecond
// guest bodies) interleaved in the same boot. A budget that only ever grows on a WIN and shrinks
// on a LOSS converges on whichever regime the thread is actually in, per thread, with no shared
// state written on the fast path.

/// `RAVA_NO_FAST_PARK2=1` (or the blanket `RAVA_NO_EMIT_MORE=1`) → `acquire()` takes the exact
/// pre-round path: one straight `GIL.lock()`, no `try_lock`, no spin, no waiter accounting.
fn fast_park2() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_PARK2").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Threads currently BLOCKED in the kernel on the GIL. Touched only on the contended path (an
/// RMW pair per park), never on an uncontended acquire, so the serial boot never writes it.
///
/// It is read for one decision: **do not spin past a thread that is already parked.** If someone
/// is asleep on this lock, the holder's window has already outlived at least one full park, so
/// this thread's spin is very unlikely to win — and winning would mean barging ahead of a thread
/// that has been waiting longer. Going straight to the block is both the cheaper and the fairer
/// answer, and it keeps a burst of arrivals from turning into N spinning cores.
static GIL_WAITERS: AtomicUsize = AtomicUsize::new(0);

/// Contended acquires that the spin resolved without entering the kernel, and those that parked.
/// Two relaxed adds, on the contended path only; reported by `handoff_report`.
static PARK_SPIN_WON: AtomicUsize = AtomicUsize::new(0);
static PARK_BLOCKED: AtomicUsize = AtomicUsize::new(0);

/// Spin budgets, in `pause` instructions (NOT in `try_lock` attempts — see `contended_gil`).
///
/// ⚠**THE UNIT IS A `pause`, AND A `pause` IS NOT A CONSTANT.** It retires in ~10 cycles on
/// pre-Skylake Intel, ~140 on Skylake and later, ~65 on Zen. Every number here is therefore
/// chosen so that the WORST case (the long one) still costs less than the thing it is avoiding:
/// `MAX` = 64 pauses ≈ 9k cycles ≈ 2.6 µs at 3.5 GHz, which is the same order as the park/unpark
/// syscall pair plus the scheduler round trip. That is the invariant that makes a lost spin
/// merely break-even instead of a regression, and it is why `MAX` is not larger.
///
/// `FLOOR` is a deliberate non-zero residue: a thread that has decayed into the long-hold regime
/// still probes for ~2 pauses, which costs nothing and is the only way its budget can climb back
/// out when the regime changes (a decayed budget of 0 could never win again, so could never grow).
const SPIN_START: u32 = 16;
const SPIN_FLOOR: u32 = 2;
const SPIN_MAX: u32 = 64;
/// Largest gap between two `try_lock` attempts. Bounds the CAS attempts inside a full `SPIN_MAX`
/// budget to seven (gaps 1,2,4,8,16,16,16), so the spinner never becomes the holder's problem.
const SPIN_GAP_MAX: u32 = 16;

thread_local! {
    /// This thread's learned spin budget. Thread-local on purpose: the adaptation is a property
    /// of *what this thread waits for* (main waits on short hand-offs, an executor worker waits
    /// on whole guest bodies), and a shared counter would both average those two populations
    /// together and put an RMW on the one path that is supposed to be getting cheaper.
    static SPIN_BUDGET: std::cell::Cell<u32> = const { std::cell::Cell::new(SPIN_START) };
}

/// One non-blocking attempt. Poisoning is recovered exactly as the blocking path does — a
/// panicking guest thread must not turn the GIL into a permanent stop.
#[inline]
fn try_gil() -> Option<MutexGuard<'static, ()>> {
    match GIL.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// The contended tail of `acquire()`: called only after one `try_gil()` has already failed.
///
/// ⚠**THE SPIN IS BACKED OFF BETWEEN ATTEMPTS, NOT HAMMERED.** `try_lock` is an interlocked
/// compare-exchange on the lock word, so a tight `try_lock` loop is a cache-line storm that
/// slows down the very thread whose exit we are waiting for. Attempts therefore happen at
/// exponentially growing gaps (1, 2, 4 … `SPIN_GAP_MAX` pauses), which puts at most seven CAS
/// attempts inside a full budget instead of hundreds.
///
/// Termination does not depend on the lock ever becoming free: the loop is bounded by `budget`
/// and always falls through to the same blocking `GIL.lock()` the pre-round code called. This is
/// an optimisation of the WAIT, never of the ACQUISITION.
#[inline(never)]
fn contended_gil() -> MutexGuard<'static, ()> {
    // Nobody parked ahead of us → the hold may still be short enough to be worth spinning out.
    if GIL_WAITERS.load(Ordering::Relaxed) == 0 {
        let budget = SPIN_BUDGET.with(|c| c.get());
        let mut spent = 0u32;
        let mut gap = 1u32;
        while spent < budget {
            for _ in 0..gap {
                std::hint::spin_loop();
            }
            spent += gap;
            gap = (gap * 2).min(SPIN_GAP_MAX);
            if let Some(g) = try_gil() {
                // WIN: grow the budget by half again (plus a floor step so a budget that has
                // decayed to `SPIN_FLOOR` can actually climb), capped at `SPIN_MAX`.
                SPIN_BUDGET.with(|c| c.set((budget + budget / 2 + SPIN_FLOOR).min(SPIN_MAX)));
                PARK_SPIN_WON.fetch_add(1, Ordering::Relaxed);
                return g;
            }
        }
        // LOSS: halve the budget. Repeated losses decay it to `SPIN_FLOOR` within a handful of
        // waits, which is the whole-guest-body regime parking almost immediately.
        SPIN_BUDGET.with(|c| c.set((budget / 2).max(SPIN_FLOOR)));
    }
    PARK_BLOCKED.fetch_add(1, Ordering::Relaxed);
    // Published BEFORE the block and cleared after, so a later arrival sees "someone is already
    // asleep here" and skips its own spin. Relaxed is right: a stale read costs at most one
    // wasted (bounded) spin or one skipped one — it can never affect who owns the lock.
    GIL_WAITERS.fetch_add(1, Ordering::Relaxed);
    let g = GIL.lock().unwrap_or_else(|e| e.into_inner());
    GIL_WAITERS.fetch_sub(1, Ordering::Relaxed);
    g
}

/// One line of park census: how contended acquires ended. Folded into `handoff_report` so it
/// prints on the same watchdog — the run this is for never exits (see the note there).
fn park_census() -> String {
    let won = PARK_SPIN_WON.load(Ordering::Relaxed);
    let blocked = PARK_BLOCKED.load(Ordering::Relaxed);
    let total = won + blocked;
    format!(
        "[PARK] contended={total} spun-out={won} ({:.1}%) parked={blocked} waiters_now={}",
        if total == 0 { 0.0 } else { won as f64 * 100.0 / total as f64 },
        GIL_WAITERS.load(Ordering::Relaxed),
    )
}

/// Block until this thread owns the GIL. No-op if already held (nested
/// checkpoints / re-entrant acquire are safe).
pub(crate) fn acquire() {
    // P3-S1: a GIL-free region worker must NEVER block on the GIL — MAIN holds it for the
    // whole process lifetime and is PARKED at the scope join, so a worker acquire is a
    // guaranteed deadlock (measured: [WORKER-EXIT] showed 3 of 4 workers exiting and one
    // stuck forever). Under RAVA_M3_WORKER_LOCKED_EXEC the worker already has the exclusivity
    // the GIL would grant (re-entrant resolve lock held + main parked + siblings blocked),
    // so the acquire is a sound no-op inside a region.
    if crate::gil::is_worker() && crate::parallel::gilfree() {
        return;
    }
    HELD.with(|h| {
        if h.borrow().is_some() {
            return;
        }
        // RAVA_GILSTAT: split the lock into try_lock + blocking-lock ONLY under the
        // gate, so an uncontended acquire is measured without changing the default
        // path (no env probe per acquire — OnceLock; no timing when off).
        //
        // The census arm now shares `contended_gil` with the default arm, so the "blocked ns"
        // it reports still covers the whole contended wait — spin included. That keeps the two
        // arms measuring the same thing; a spin that resolves in 300 ns simply shows up as a
        // 300 ns "block" instead of a 3 µs one, which is exactly the effect being priced.
        let g = if gilstat_enabled() {
            ACQ_TOTAL.fetch_add(1, Ordering::Relaxed);
            match try_gil() {
                Some(g) => g,
                None => {
                    ACQ_CONTENDED.fetch_add(1, Ordering::Relaxed);
                    let t0 = std::time::Instant::now();
                    let g = if fast_park2() {
                        contended_gil()
                    } else {
                        GIL.lock().unwrap_or_else(|e| e.into_inner())
                    };
                    let ns = t0.elapsed().as_nanos() as u64;
                    ACQ_BLOCKED_NS.fetch_add(ns, Ordering::Relaxed);
                    ACQ_MAX_BLOCK_NS.fetch_max(ns, Ordering::Relaxed);
                    g
                }
            }
        } else if fast_park2() {
            // One `TryAcquireSRWLockExclusive` for the uncontended case (which is every acquire
            // of the serial boot), then the adaptive spin, then the same block as before.
            match try_gil() {
                Some(g) => g,
                None => contended_gil(),
            }
        } else {
            // KILL SWITCH ARM: byte-identical to the pre-round path.
            GIL.lock().unwrap_or_else(|e| e.into_inner())
        };
        *h.borrow_mut() = Some(g);
        // The executing thread just changed: re-publish THIS thread's
        // cell-root ctx so compiled prologues (Deep-IC v3) push onto the
        // right per-thread stack when this thread resumes a compiled frame
        // mid-callback. CURRENT_VM is set iff a compiled frame is live here.
        // GAP-S09 stage 4: `&Vm` (vm_shared) — `jit_ctx_slot` is an
        // AtomicUsize store; GIL acquisition never runs gil-free anyway.
        if let Some(vm) = crate::jit::vm_shared() {
            vm.jit_ctx_slot.store(
                crate::frames::ctx_addr() as usize,
                Ordering::Relaxed,
            );
        }
    });
}
/// Release the GIL if held (drops the guard so another thread may take it).
pub(crate) fn release() {
    // ⚠**RETIRE THE INLINE BUMP CURSOR FIRST.** Since the arena heap is shared between
    // GIL-serialised threads (`tlab::with_tlab`), this thread's per-thread `[next,end]` cursor
    // stops being valid the moment another thread can allocate — and a stale one rewinds
    // `young.top` over live objects. Only when a second thread actually exists; the
    // single-threaded engine keeps its cursor across the (one) acquire it ever does.
    //
    // ⚠**THIS ONE ASKS THE EXACT COUNT, NOT THE LATCH** — the only reader in the crate that must.
    // Every other caller of `has_real_threads()` is deciding whether to take a lock that would be
    // a no-op anyway, so a momentarily stale "no" costs nothing. Here a stale "no" would leave a
    // bump cursor live across the hand-off and let this thread later rewind `young.top` over
    // objects the other thread allocated — a heap corruption, not a missed optimisation. The
    // release path runs only when the GIL is actually being given up, which the serial boot
    // barely does, so the SeqCst load is bought where it is cheap.
    #[cfg(feature = "next-gen-gc")]
    if REAL_THREADS.load(Ordering::SeqCst) > 0 {
        crate::nextgen::tlab::retire_cursor();
    }
    HELD.with(|h| {
        h.borrow_mut().take();
    });
}

/// Set once when the main thread is about to drop the Vm: a daemon OS thread
/// that wakes after this must NOT touch the (dropping) Vm — it exits instead,
/// preventing a use-after-free at shutdown.
static SHUTDOWN: AtomicUsize = AtomicUsize::new(0);
pub(crate) fn begin_shutdown() {
    SHUTDOWN.store(1, Ordering::SeqCst);
}
pub(crate) fn shutting_down() -> bool {
    SHUTDOWN.load(Ordering::SeqCst) != 0
}

thread_local! {
    /// This OS thread's `java.lang.Thread` handle (0 = unset → fall back to the
    /// shared `Vm::main_thread`, which the main OS thread + green threads use).
    /// A real daemon OS thread sets its own, so `Thread.currentThread()` is
    /// correct per thread and MC's `currentThread()==primaryThread` async
    /// guards don't misfire.
    pub(crate) static CURRENT_THREAD: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
