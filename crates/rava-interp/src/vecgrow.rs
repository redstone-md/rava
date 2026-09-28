//! VEC-GROWTH CENSUS (`RAVA_VECGROW_STAT=1`, default OFF, diagnostic only —
//! counts, no walls, contention-immune).
//!
//! The RIP profile puts the allocator cluster at **11.7% of blocks A+B**, with
//! `mi_malloc_aligned` at 5.5% self-time and `RawVecInner::finish_grow` its
//! dominant caller (20%). `finish_grow` is the REALLOCATION path — a Vec that
//! outgrew its capacity — so the cost is not "we allocate buffers", it is "we
//! allocate a buffer and then immediately grow it again".
//!
//! This census sizes that per named site: how many times the site ran, and how
//! many of those runs actually moved the allocation. A site with a high GREW
//! share is re-allocating a buffer whose final size was knowable up front; a
//! site with ~0% is already right and must not be "fixed".
//!
//! Cost when unset: one relaxed load of a lazily-initialised bool. No String is
//! built at note time (the O(string)-per-call bookkeeping defect class); site
//! names are static and resolved only at dump.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// A counted Vec-growth site. Order must match [`NAMES`].
#[derive(Copy, Clone)]
pub(crate) enum Site {
    /// `opcodes::pop_args` — filling the pooled argument buffer from the operand
    /// stack. Grows when the pooled Vec came back with capacity < arg count.
    PopArgs = 0,
    /// `opcodes::invokevirtual` arena-receiver fast path — `locals.insert(0, recv)`.
    InsertRecvFast = 1,
    /// `opcodes::invokevirtual` resolved path — `locals.insert(0, recv)`.
    InsertRecvSlow = 2,
    /// `opcodes::invoke_resolved_virtual` — `locals.insert(0, recv)`.
    InsertRecvResolved = 3,
    /// `expand_locals_into` — the `reserve(args.len() + 2)` that sizes the locals
    /// buffer BEFORE `Frame::new` learns `max_locals`.
    ExpandLocals = 4,
    /// `Frame::new` — `locals.resize(max_locals)`. This is the one the profile
    /// predicts: the buffer was just sized for the ARGUMENTS, then resized to the
    /// method's LOCALS, which is strictly larger for almost every real method.
    FrameLocalsResize = 5,
    /// `Frame::new` — `stack.reserve(max_stack)` on the pooled operand buffer.
    FrameStackReserve = 6,
    /// `jit_run` — the COMPILED path's cell buffer, created with capacity `max_locals`
    /// and then `resize`d to `max_locals + max_stack` for the GC spill area, so it
    /// reallocates on essentially every compiled invocation. GREW ~= CALLS, by design.
    ///
    /// It is counted because that realloc is the top row when host samples are ranked by
    /// growth from one lane worker to six, and REMOVING IT WAS REFUTED: neutral at the
    /// default width, ~3.4% worse at width 6 (numbers at the `jit_run` site). The counter
    /// stays so the refuted lever keeps a live measurement attached to it rather than a
    /// memory, and so anyone who re-reads that profile row finds the answer here.
    JitRunCells = 7,
}

const N: usize = 8;

const NAMES: [&str; N] = [
    "pop_args (arg buffer fill)",
    "invokevirtual fast  insert(0,recv)",
    "invokevirtual slow  insert(0,recv)",
    "invoke_resolved     insert(0,recv)",
    "expand_locals_into  reserve(exact slots)",
    "Frame::new  locals.resize(max_locals)",
    "Frame::new  stack.reserve(max_stack)",
    "jit_run/aot_run cells (compiled)",
];

static CALLS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static GREW: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
/// Total elements present at the moment a grow happened — the memmove the
/// reallocation had to perform, so a site's copy volume is visible too.
static MOVED: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
/// Of the grows, how many started from capacity 0 — i.e. the buffer had NO
/// allocation at all (a pool miss), rather than one that was merely too small.
/// This distinguishes "the pool is empty" from "the pool hands back buffers that
/// are exactly one element too short", which have completely different fixes.
static CAP0: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("RAVA_VECGROW_STAT").is_some();
        // Armed here, in a once-only initializer off the hot path, so the census is readable
        // on a workload that never terminates (see `start_watchdog`).
        if on {
            start_watchdog();
        }
        on
    })
}

/// Pool accounting. A buffer pool only works if takes and gives BALANCE; if a
/// path consumes a checked-out buffer without returning it, the pool drains and
/// every subsequent `take` is a fresh `Vec::new()` that the caller then has to
/// grow — which is exactly the `finish_grow` the RIP profile is pointing at.
static TAKE: AtomicU64 = AtomicU64::new(0);
static TAKE_MISS: AtomicU64 = AtomicU64::new(0);
static GIVE: AtomicU64 = AtomicU64::new(0);
static GIVE_CAP0: AtomicU64 = AtomicU64::new(0);
static GIVE_FULL: AtomicU64 = AtomicU64::new(0);

#[inline]
pub(crate) fn note_take(hit: bool) {
    if !enabled() {
        return;
    }
    TAKE.fetch_add(1, Relaxed);
    if !hit {
        TAKE_MISS.fetch_add(1, Relaxed);
    }
}

/// Free-running event counters, used to localise WHERE a checked-out buffer
/// dies. Index meanings are in [`EVENT_NAMES`].
static EVENTS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

const EVENT_NAMES: [&str; 6] = [
    "Vm::invoke entered",
    "Vm::invoke reached expand_locals (buffer returned)",
    "invokevirtual  -> NATIVE branch (locals dropped)",
    "invokestatic   -> NATIVE branch (locals dropped)",
    "invokespecial  -> NATIVE branch (locals dropped)",
    "invokeinterface-> NATIVE branch (locals dropped)",
];

#[inline]
pub(crate) fn event(i: usize) {
    if !enabled() {
        return;
    }
    EVENTS[i].fetch_add(1, Relaxed);
}

#[inline]
pub(crate) fn note_give(cap: usize, full: bool) {
    if !enabled() {
        return;
    }
    GIVE.fetch_add(1, Relaxed);
    if cap == 0 {
        GIVE_CAP0.fetch_add(1, Relaxed);
    } else if full {
        GIVE_FULL.fetch_add(1, Relaxed);
    }
}

/// Record one execution of `site`, given the buffer capacity before and after
/// the operation and the element count at the time. `after != before` ⇒ the
/// allocation moved (a `finish_grow`).
#[inline]
pub(crate) fn note(site: Site, cap_before: usize, cap_after: usize, len: usize) {
    if !enabled() {
        return;
    }
    let i = site as usize;
    CALLS[i].fetch_add(1, Relaxed);
    if cap_after != cap_before {
        GREW[i].fetch_add(1, Relaxed);
        MOVED[i].fetch_add(len as u64, Relaxed);
        if cap_before == 0 {
            CAP0[i].fetch_add(1, Relaxed);
        }
    }
}

/// Start a background snapshot thread (idempotent). The A+B harness KILLS the
/// process at the `Environment:` marker, so a drop-time report would never print
/// for the window this census exists to measure; periodic snapshots carry an
/// elapsed stamp so the reader can take the one at the window edge.
pub(crate) fn start_sampler() {
    if !enabled() {
        return;
    }
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    std::thread::spawn(|| {
        let t0 = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            eprintln!("[VECGROW] --- t={:.1}s ---", t0.elapsed().as_secs_f64());
            report();
        }
    });
}

/// Print the census. Called on `Heap` drop alongside the other tracers, and
/// periodically by [`start_sampler`].
/// PERIODIC REPORT (`RAVA_VECGROW_DUMP_MS`, default 20 s once the census is armed).
///
/// `report()`'s only caller is `Heap::drop` — teardown — and a server boot never tears down.
/// This census was written to answer "which sites re-allocate a buffer whose final size was
/// knowable", and on the one workload that question matters for it has never been readable.
/// Third instance of this trap in one session (PGO training, `pure_memo`, here): **a diagnostic
/// that reports at teardown reports nothing about a program that does not terminate.**
pub(crate) fn start_watchdog() {
    static W: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let ms = std::env::var("RAVA_VECGROW_DUMP_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(20_000)
            .max(1_000);
        let _ = std::thread::Builder::new()
            .name("rava-vecgrow-dump".to_string())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                report();
            });
    });
}

pub(crate) fn report() {
    if !enabled() {
        return;
    }
    eprintln!("[VECGROW] site                                     calls        grew    grew%     moved      cap0");
    let mut tot_calls = 0u64;
    let mut tot_grew = 0u64;
    for i in 0..N {
        let c = CALLS[i].load(Relaxed);
        let g = GREW[i].load(Relaxed);
        let m = MOVED[i].load(Relaxed);
        let z = CAP0[i].load(Relaxed);
        tot_calls += c;
        tot_grew += g;
        let pct = if c == 0 { 0.0 } else { g as f64 * 100.0 / c as f64 };
        eprintln!(
            "[VECGROW] {:<38} {:>10} {:>11} {:>7.2}% {:>9} {:>9}",
            NAMES[i], c, g, pct, m, z
        );
    }
    let pct = if tot_calls == 0 { 0.0 } else { tot_grew as f64 * 100.0 / tot_calls as f64 };
    eprintln!("[VECGROW] {:<38} {:>10} {:>11} {:>7.2}%", "TOTAL", tot_calls, tot_grew, pct);
    let (t, tm, g, gz, gf) = (
        TAKE.load(Relaxed),
        TAKE_MISS.load(Relaxed),
        GIVE.load(Relaxed),
        GIVE_CAP0.load(Relaxed),
        GIVE_FULL.load(Relaxed),
    );
    let mpct = if t == 0 { 0.0 } else { tm as f64 * 100.0 / t as f64 };
    eprintln!(
        "[VECGROW] POOL take={} miss={} ({:.2}%)  give={} give_cap0={} give_full={}  LEAKED={}",
        t,
        tm,
        mpct,
        g,
        gz,
        gf,
        t.saturating_sub(g)
    );
    for (i, n) in EVENT_NAMES.iter().enumerate() {
        eprintln!("[VECGROW] EVENT {:<52} {:>12}", n, EVENTS[i].load(Relaxed));
    }
}
