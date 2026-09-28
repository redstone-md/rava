//! Live-bytes meter, fed by the binary's global allocator (see rava-cli). Gives
//! the GC scheduler the REAL process allocation instead of `Heap::approx_bytes`,
//! which undercounts true RSS ~5x (Vec capacity slack, box tables, Str payloads,
//! allocator overhead) and so let the heap balloon before a collection fired.
//! When no counting allocator is installed (unit tests, embedders) it stays 0 and
//! the scheduler falls back to the byte estimate.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
static LIVE: AtomicI64 = AtomicI64::new(0);
/// Absolute real-process-byte ceiling. 0 = disabled. Set once at startup from
/// `RAVA_HARD_MB`. This is the LAST-RESORT safety net: the GC + `RAVA_XMX_MB`
/// abort both run only at bytecode checkpoints, so a native Rust function that
/// recurses on a cyclic object graph (toString/equals/structural_key over a
/// self-referential structure) allocates without ever returning to the loop —
/// invisible to those guards. It ate ~31GB and froze the machine once. Because
/// EVERY allocation flows through `add()`, a check here fires even mid-native-
/// recursion and kills the process before it can exhaust system RAM.
static HARD: AtomicI64 = AtomicI64::new(0);
static ABORTING: AtomicBool = AtomicBool::new(false);
/// ⭐**COUNTED: a boot makes 113,497,262 allocations and 110,267,249 frees.** Every one of them
/// used to do a `lock xadd` on `LIVE` — 223.8 MILLION atomic read-modify-writes on ONE global
/// cache line, from every thread, plus a load of `HARD` from the line the RMW had just
/// invalidated. At a few ns each that is seconds of a ~47 s boot, and it is pure accounting:
/// nothing reads `LIVE` more than a few thousand times a boot (the GC scheduler and the hard
/// ceiling), so per-event precision was never needed.
///
/// Each thread now accumulates its own delta and pushes it to the global only when the delta
/// leaves +/-`FLUSH_BYTES`. Allocation and freeing interleave, so the local value hovers near
/// zero and a flush is rare. The ceiling stays armed: a single allocation larger than the
/// threshold crosses it on its own and flushes immediately, which is exactly the runaway-native-
/// recursion case the check exists for.
const FLUSH_BYTES: i64 = 128 * 1024;

thread_local! {
    /// (delta bytes, allocs, frees) for THIS thread since its last flush.
    ///
    /// ⚠`const` init and NO `Drop`, deliberately: this is read from inside the global allocator,
    /// and a lazily-initialised or destructor-carrying TLS would allocate there and recurse
    /// forever. The price is that a thread which exits mid-window abandons under 128 KB of
    /// accounting; rava runs a fixed worker pool, so that is bounded and tiny.
    static LOCAL: std::cell::Cell<(i64, u32, u32)> = const { std::cell::Cell::new((0, 0, 0)) };
}

/// Push a thread's accumulated delta to the shared counters and re-arm the ceiling.
#[inline(never)]
#[cold]
fn flush(delta: i64, allocs: u32, frees: u32) {
    if allocs != 0 {
        ALLOC_N.fetch_add(allocs as u64, Ordering::Relaxed);
    }
    if frees != 0 {
        FREE_N.fetch_add(frees as u64, Ordering::Relaxed);
    }
    let now = LIVE.fetch_add(delta, Ordering::Relaxed) + delta;
    let hard = HARD.load(Ordering::Relaxed);
    if hard != 0 && now > hard && !ABORTING.swap(true, Ordering::Relaxed) {
        hard_abort(now.max(0) as usize);
    }
}

/// `RAVA_NO_ALLOC_BATCH=1` restores the per-event atomic. Exists so the batching can be A/B'd
/// in ONE binary (the only boot instrument this project trusts) and as a kill switch.
///
/// ⛔**NOT a `OnceLock`, and this is not style.** The first version was
/// `OFF.get_or_init(|| env::var_os(..).is_some())` and it HUNG EVERY BOOT — `var_os` allocates,
/// the allocation re-enters the global allocator, `bump` asks `batching_off()` again, and
/// `get_or_init` blocks the thread waiting for an initialisation only that same thread could
/// finish. Both A/B arms timed out at 400 s. **Nothing reachable from the global allocator may
/// allocate, and `OnceLock` turns re-entrancy into a deadlock rather than a stack overflow, so
/// it fails silently.** The 4 gates and 21 probes missed it because they ran on the build one
/// commit earlier.
static BATCH_OFF: AtomicBool = AtomicBool::new(false);

/// Read the allocator-related env flags ONCE, from `main`, where allocating is safe.
/// Allocations before this point simply take the defaults.
pub fn init_from_env() {
    BATCH_OFF.store(std::env::var_os("RAVA_NO_ALLOC_BATCH").is_some(), Ordering::Relaxed);
    crate::alloc_sites::init_from_env();
}

#[inline]
fn batching_off() -> bool {
    BATCH_OFF.load(Ordering::Relaxed)
}

#[inline]
fn bump(d: i64, is_alloc: bool) {
    if batching_off() {
        flush(d, u32::from(is_alloc), u32::from(!is_alloc));
        return;
    }
    // `try_with`, not `with`: during thread teardown the TLS block may already be gone, and a
    // panic there would come out of the global allocator.
    let handled = LOCAL.try_with(|c| {
        let (mut delta, mut a, mut f) = c.get();
        delta += d;
        if is_alloc {
            a += 1;
        } else {
            f += 1;
        }
        if is_alloc && crate::alloc_sites::on() {
            crate::alloc_sites::note(a as u64);
        }
        if is_alloc {
            crate::alloc_sites::note_size(d);
        }
        if delta > FLUSH_BYTES || delta < -FLUSH_BYTES || a == u32::MAX || f == u32::MAX {
            c.set((0, 0, 0));
            flush(delta, a, f);
        } else {
            c.set((delta, a, f));
        }
    });
    if handled.is_err() {
        flush(d, u32::from(is_alloc), u32::from(!is_alloc));
    }
}

/// Allocation/free counts, exact only to the last flush per thread. See `LOCAL`.
pub static ALLOC_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static FREE_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[inline]
pub fn add(n: usize) {
    bump(n as i64, true);
}
#[inline]
pub fn sub(n: usize) {
    bump(-(n as i64), false);
}
/// Net live bytes across the whole process (0 if no meter is installed).
///
/// Stale by at most `FLUSH_BYTES` per live thread — a few hundred KB against a working set of
/// hundreds of MB. Both readers (the GC scheduler's trigger and the RSS reconciliation) compare
/// against multi-MB thresholds, so the slack is far below their resolution.
#[inline]
pub fn live() -> usize {
    LIVE.load(Ordering::Relaxed).max(0) as usize
}

/// Push this thread's pending delta immediately. For a thread that is about to park for a long
/// time, and for the GC before it reads `live()` at a decision point.
pub fn flush_local() {
    let _ = LOCAL.try_with(|c| {
        let (delta, a, f) = c.get();
        if delta != 0 || a != 0 || f != 0 {
            c.set((0, 0, 0));
            flush(delta, a, f);
        }
    });
}
/// Arm the hard ceiling (bytes). 0 disables it.
pub fn set_hard_limit(bytes: usize) {
    HARD.store(bytes as i64, Ordering::Relaxed);
}
/// Real bytes at which the process self-terminates (0 = disabled).
pub fn hard_limit() -> usize {
    HARD.load(Ordering::Relaxed).max(0) as usize
}
/// Crossed the hard ceiling: emit one diagnostic and exit immediately. Reached
/// from inside the global allocator, so it must not depend on further large
/// allocations succeeding — the `ABORTING` latch already stops re-entrant
/// checks from re-firing, and a one-shot `eprintln!` is tolerable (its own
/// alloc re-enters `add()` but the latch makes that a no-op). `exit` runs no
/// Drop glue and returns all memory to the OS at once, so the runaway native
/// recursion is unwound by process teardown rather than by Rust.
#[cold]
#[inline(never)]
fn hard_abort(now: usize) -> ! {
    let mb = now / (1024 * 1024);
    let cap = HARD.load(Ordering::Relaxed).max(0) as usize / (1024 * 1024);
    eprintln!(
        "\n[FATAL] Rava real-memory ceiling exceeded: ~{mb} MB > RAVA_HARD_MB={cap} MB.\n\
         Likely an unbounded native recursion on a cyclic object graph. Terminating\n\
         to protect the host. Raise RAVA_HARD_MB only if this is a legitimate working set."
    );
    std::process::exit(137);
}

/// Actual OS working-set size (true RSS) in bytes — the number the census/malloc/arena
/// figures must reconcile against. 0 on failure / non-Windows.
#[cfg(windows)]
pub fn os_rss() -> usize {
    use std::os::raw::c_void;
    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }
    unsafe {
        let mut c: ProcessMemoryCounters = std::mem::zeroed();
        c.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) != 0 {
            c.working_set_size
        } else {
            0
        }
    }
}
#[cfg(not(windows))]
pub fn os_rss() -> usize {
    0
}
