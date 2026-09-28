//! Counting refusals, aborts and pending sets.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::gilfree_cf::*;

pub(crate) fn enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_GILFREE_CF").is_some() || rings())
}

/// PROCESS-WIDE PRE-GATE FOR `counters::note_cb` (DEFAULT ON; `RAVA_NO_FAST_LANECB=1` opts out).
///
/// SHAPE BEFORE / AFTER — the whole point of the gate, stated once here.
///
/// `note_cb` runs on EVERY compiled→host callback (~30 M a product boot, and the lane's own
/// census once measured 312,627,349 on a ring-heavy run). Its two neighbours inside
/// `jit::counters::cb` are gate-ordered — `callback_stats_enabled()` short-circuits on a
/// read-once `OnceLock` bool and `s09census::note` tests its own `OnceLock` before touching
/// TLS — but `note_cb` sat between them reaching thread-local `LANE` unconditionally.
///
/// `LANE` is already the CHEAP thread-local shape (`const`-initialised, payload is three Copy
/// scalars, no destructor), so the access lowers to a bare TLS-slot load rather than an
/// out-of-line `LocalKey::with` with a lazy-init state check. On windows-msvc "bare" is still a
/// chain of DEPENDENT loads — `gs:[0x58]` → `_tls_index` → the module's TLS array → the slot —
/// before the `in_lane` byte can even be tested. The gate replaces that chain, on the ~100% of
/// crossings that are main's, with ONE relaxed load of a static (`parallel::any_parallel`, i.e.
/// `IN_REGION > 0`) plus this read-once bool. Two independent static loads instead of four
/// dependent TLS loads; no new table, no memo, no mirror to keep in sync.
///
/// WHY `any_parallel()` IS THE CORRECT PRECONDITION, and why the failure direction is safe.
/// `LaneState::in_lane` is set true in exactly one place — `LaneWorker::enter` (mod.rs) — which
/// calls `parallel::enter_region()` a few statements later and BEFORE `jit::call_raw` runs the
/// compiled body, and `LaneWorker::drop` clears `in_lane` BEFORE `parallel::leave_region()`.
/// So `IN_REGION >= 1` holds across the entire window in which any compiled callback can
/// observe `in_lane == true`; the gate is a strict superset and can only ever ADMIT.
/// An undercount affects diagnostic output only. Region retention depends on the
/// joined abort flag and task outcomes, never callback counts.
///
/// ⚠ DELIBERATELY NOT APPLIED TO `counters::in_lane()`. Same predicate, same proof, but the
/// wrong failure direction: `in_lane()` gates SOUNDNESS barriers (`opcodes/fields.rs` COW
/// ownership, `jit/callbacks/arrays.rs` region checks), where a spurious `false` skips a
/// barrier instead of costing a re-run. A cheaper gate is not worth owning that ordering
/// invariant from a second file, so `in_lane()` keeps its direct TLS read.
#[inline]
pub(crate) fn fast_lanecb() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_LANECB").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Executor pool width for the lane (`RAVA_EXEC_THREADS`, default 6). The
/// stock pool is `min(cores, 2)` — a 2-wide cap on a 120-task phase.
pub(crate) fn worker_cap() -> usize {
    pub(crate) static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        if let Some(n) = std::env::var("RAVA_EXEC_THREADS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
        {
            return n;
        }
        // A QUARTER OF THE MACHINE, not a hardcoded 6.
        //
        // The old default was 6 and it sat past the far side of the optimum — far enough that the
        // lane read as neutral on the boot and its cost read as some mysterious "contention".
        // Measured, one boot per point, on a 16-logical-core box pinned to 12:
        //
        //   workers  serial    1       2       3       4       5       6
        //   Done     31.258  49.393  29.954  23.451  23.414  27.872  31.127
        //   CPU        91.4   105.4   113.6   120.5   138.2   170.5   202.8
        //
        // The optimum is 3-4 and the CPU cost goes superlinear above it — six workers burn 2.2x
        // the CPU of one for the SAME work (identical `cbs_sum`) and hand it all back in
        // contention. Interleaved A/B at four, lane as the only variable:
        //   wall  off min=86.9  on min=79.8   -8.17% ON CLEAN
        //   done  off min=29.518 on min=22.88 -22.49% ON CLEAN
        //
        // RE-MEASURED 2026-07-29 on a much-changed engine (gauntlet shortcut, map-key derivation,
        // argument-buffer recycling, epoch-keyed partial layouts — the boot went from Done ~50s to
        // ~23s in between). The curve did NOT move; the default is still on its optimum:
        //
        //   workers      3       4       5       6
        //   Done min   24.162  23.485  26.857  31.284
        //   wall min    78.3    71.4    75.1   110.1
        //   CPU min    119.0   129.9   157.3   234.1
        //
        // Four is best on BOTH metrics, and the superlinear CPU term above it is intact — six burn
        // 1.8x the CPU of four for the same work and hand it back as contention. That term is still
        // unattributed and is what caps Done; more workers is not the lever.
        // ⇒ Do not re-derive "raise the worker count" from a profile. It was checked here twice.
        //
        // A quarter lands on 3 of 12 and 4 of 16 — both inside the measured optimum — and leaves
        // the machine room for main, the GC and the JIT threads, which is why the fraction is a
        // quarter rather than "all of it". Clamped so a tiny box still forks and a huge one does
        // not walk off the far side of the same curve.
        std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(8)
            .div_euclid(4)
            .clamp(2, 8)
    })
}

/// The three per-thread facts every compiled callback touches, in one cell.
///
/// * `in_lane`   — is this thread inside a gil-free lane task?
/// * `last_cb`   — kind ordinal of the callback most recently entered, so an abort can NAME
///                 where it fired instead of leaving it to a release backtrace to misattribute.
/// * `cb_n`      — diagnostic callback count; never used to select execution.
#[derive(Clone, Copy)]
pub(crate) struct LaneState {
    pub(crate) in_lane: bool,
    pub(crate) last_cb: usize,
    pub(crate) cb_n: u64,
}

// THREAD-LOCAL SHAPE AUDIT for this module. A `thread_local!` lowers to a bare TLS-slot load
// ONLY when it is `const`-initialised AND its payload is destructor-free; anything else becomes
// an out-of-line `LocalKey::with` carrying a lazy-init / destructor-registration state check on
// EVERY access, and a `RefCell` adds a borrow-flag load, a compare, a store on entry and a
// release store on the guard's drop.
//
//   name                shape                        const?  drop?  touched                 verdict
//   LANE                Cell<LaneState> (3 scalars)   yes     no     every compiled callback  cheap; now pre-gated
//   LANE_ABORT          Cell<bool>                    yes     no     1x/task + on abort       cheap, cold
//   LANE_PENDING        Cell<(u32,u32)>               yes     no     guest throw/catch only   cheap, cold
//   LANE_PENDING_CLASS  RefCell<Option<String>>       yes     YES    1x/task + first throw    EXPENSIVE SHAPE, cold — kept
//   LANE_ABORT_CB       Cell<Option<usize>>           yes     no     1x/task + on abort       cheap, cold
//
// LANE_PENDING_CLASS is the one expensive shape here and it STAYS. It owns a `String` because
// the census must NAME the throw (class + `$message`; an `UnsatisfiedLinkError` is only
// actionable once it names the method), and that string is minted on a lane worker — a
// `thread::scope` thread that DIES at the join — so the destructor is what frees it. Its touch
// count is a few hundred per boot (one reset per admitted task, one write per FIRST throw of a
// task, both already behind an `in_lane()` test), i.e. six orders of magnitude below `LANE`.
// Rewriting it into a fixed inline byte buffer would buy nothing measurable and would truncate
// the one field the census exists to print. Left alone on purpose.
thread_local! {
    /// True only while THIS thread runs a rings-lane compiled body. Every
    /// worker barrier that must behave differently for this lane than for the
    /// pre-existing COW forks (chunk-fill / bitstorage, which also arm
    /// `cow_armed` and set `gilfree()`) keys off this flag, so those forks stay
    /// byte-identical with the flag on or off.
    /// LANE STATE IN ONE CELL, because `note_cb` runs on EVERY compiled callback —
    /// 312,627,349 of them in a boot — and it was taking three separate thread-local
    /// crossings to answer one question and bump two counters. The tree has priced this
    /// exact shape before: the FAST FRAME in `jit_direct_invoke` removed 635.8M
    /// `LocalKey::with` crossings for identical work. Same trick, same reason.
    ///
    /// `in_lane` stays a method rather than a field read at the call sites, so the four
    /// existing users are unchanged.
    pub(crate) static LANE: std::cell::Cell<LaneState> =
        const { std::cell::Cell::new(LaneState { in_lane: false, last_cb: usize::MAX, cb_n: 0 }) };
    /// PER-TASK abort latch. `parallel::REGION_ABORT` is a PROCESS-WIDE flag: one
    /// worker's deopt would read as every sibling's deopt, which is wrong now that
    /// the batch runs N tasks CONCURRENTLY in one region. Each lane worker latches
    /// its own task's abort here instead.
    pub(crate) static LANE_ABORT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// PENDING-EXCEPTION CENSUS for the task currently running (diagnostic only).
    /// `(set, taken)`: how many times this task latched a pending exception, and
    /// how many times something consumed one. The pair is what distinguishes a
    /// guest exception the task CATCHES ITSELF (set > 0, taken == set, nothing
    /// pending at return) from one that ESCAPES (set > taken) — the question that
    /// decides whether tearing the region down at set-time is correct at all.
    pub(crate) static LANE_PENDING: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
    /// Class name of this task's FIRST pending exception, so the census names the
    /// throw instead of leaving it to be inferred from a backtrace.
    pub(crate) static LANE_PENDING_CLASS: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    /// The callback kind that was running when this task FIRST latched an abort.
    pub(crate) static LANE_ABORT_CB: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// A pending exception was just latched by THIS lane task (`execution_state::
/// pending_set_first`). Inert off the lane.
///
/// The class name is resolved HERE rather than at the throw site because the
/// throw may arrive as a plain `VmError::Throw` propagated out of the
/// interpreter, which never passes through `jit::set_pending` and so has no
/// literal class name to record.
pub(crate) fn note_pending_set(value: &Value) {
    if !in_lane() {
        return;
    }
    LANE_PENDING.with(|c| {
        let (s, t) = c.get();
        c.set((s + 1, t));
    });
    LANE_PENDING_CLASS.with(|c| {
        let mut slot = c.borrow_mut();
        if slot.is_some() {
            return;
        }
        // Read-only class lookup through the thread's published `&Vm`.
        let Some(vm) = jit::vm_shared() else { return };
        *slot = Some(match value {
            // The MESSAGE matters as much as the class here: an
            // `UnsatisfiedLinkError` is only actionable once it names the method
            // rava refused to link.
            Value::Ref(h) => {
                let class = vm.heap.class_of(*h).unwrap_or("?ref").to_string();
                match vm.heap.get_field(*h, "$message") {
                    Value::Str(m) => format!("{class}: {m}"),
                    _ => class,
                }
            }
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                .map_or_else(|| "?obj".to_string(), |c| c.to_string()),
            other => format!("{other:?}"),
        });
    });
}

pub(crate) fn note_stub_abort(what: &'static str) {
    if !in_lane() {
        return;
    }
    let map = STUB_ABORTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
    let n = m.entry(what).or_insert(0);
    *n += 1;
    if *n <= 2 {
        eprintln!("[GILFREE-RINGS] deopt at worker-illegal `{what}` (first {n})");
    }
}

/// WHICH worker-illegal callback takes the global resolve lock, by KIND.
///
/// `RAVA_LANE_OUTER` attributed **82,382,752 outer acquisitions to one line** —
/// `compile.rs:55`, the `WorkerGate::Locked` arm — out of 86,302,646 lock acquisitions in
/// the whole boot. That is 95.5% of every acquisition from a single gate, and the gate is
/// shared by seven callbacks, so a file:line attribution cannot say which one. The abort
/// path has had a per-kind tally since stage 3; the LOCKED path never did, because when it
/// was written the locked arm was the rare case.
///
/// Diagnostic only, behind the lane probe, and it uses the same `&'static str` keys the
/// abort tally uses so the two are directly comparable.
pub(crate) fn note_locked_gate(what: &'static str) {
    if !crate::lane_probe::enabled() {
        return;
    }
    use std::sync::atomic::Ordering::Relaxed;
    static TALLY: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<&'static str, u64>>> =
        std::sync::OnceLock::new();
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let map = TALLY.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    {
        let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
        *m.entry(what).or_insert(0) += 1;
    }
    // Reporting every N keeps it readable without a hook into the probe's own cadence.
    if N.fetch_add(1, Relaxed) % 500_000 == 499_999 {
        let m = map.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(&&'static str, &u64)> = m.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        let total: u64 = m.values().sum();
        eprintln!(
            "[LANE-LOCKED] {total} locked worker-illegal gates | {}",
            rows.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
        );
    }
}

/// WHICH MEMBER the locked `fu_invoke` gate is serving — the question `note_locked_gate`
/// could not answer, since every one of its 82M entries carries the same kind.
///
/// Keys are the constant-pool member NAMES, which are bytecode, not workload identity.
pub(crate) fn note_locked_member(name: &str) {
    use std::sync::atomic::Ordering::Relaxed;
    static TALLY: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
        std::sync::OnceLock::new();
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let map = TALLY.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    {
        let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
        *m.entry(name.to_string()).or_insert(0) += 1;
    }
    if N.fetch_add(1, Relaxed) % 20_000_000 == 19_999_999 {
        let m = map.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(&String, &u64)> = m.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        let total: u64 = m.values().sum();
        eprintln!(
            "[LANE-MEMBER] {total} locked fu_invoke gates | {}",
            rows.iter().take(14).map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
        );
    }
}

pub(crate) fn report() {
    if !rings() {
        return;
    }
    drain_abort_sites();
    use std::sync::atomic::Ordering::Relaxed;
    eprintln!(
        "[GILFREE-RINGS] lane done={} abort={} | deopt sites: shared-field={} array-store={} array-alloc={} array-region-full={} | claim: nursery-full={}",
        crate::taskgraph_probe::GILFREE_DONE.load(Relaxed),
        crate::taskgraph_probe::GILFREE_ABORT.load(Relaxed),
        ABORT_SHARED_FIELD.load(Relaxed),
        ABORT_ARRAY_STORE.load(Relaxed),
        ABORT_ARRAY_ALLOC.load(Relaxed),
        ABORT_ARRAY_REGION_FULL.load(Relaxed),
        CLAIM_NURSERY_FULL.load(Relaxed),
    );
    // Which pre-lock arm served the lane's worker dispatches, and how many fell
    // through to the ONE global resolve lock. Printed here because the lane has no
    // CHUNK-FILL-PAR line; the counters are the same freeze-resolve ones the S2 fork
    // reports (`direct_hit`/`hit` are gated on RAVA_FORK_STATS, `miss` is not).
    eprintln!(
        "[GILFREE-RINGS] freeze: table_hit={} direct_hit={} miss={} stored={} locked_calls={} dmiss_locks={} | locked-top: {}",
        crate::jit::freeze_resolve::WORKER_HIT.load(Relaxed),
        crate::jit::freeze_resolve::WORKER_DIRECT_HIT.load(Relaxed),
        crate::jit::freeze_resolve::WORKER_MISS.load(Relaxed),
        crate::jit::freeze_resolve::STORED.load(Relaxed),
        crate::jit::freeze_resolve::WORKER_LOCKED_CALLS.load(Relaxed),
        crate::gil::worker_miss_count(),
        crate::jit::freeze_resolve::locked_tally_top(8),
    );
    if let Some(map) = STUB_ABORTS.get() {
        let m = map.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(&&'static str, &u64)> = m.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        let joined: Vec<String> = rows.iter().map(|(w, n)| format!("{w}={n}")).collect();
        eprintln!("[GILFREE-RINGS] worker-illegal deopts: {}", joined.join(" "));
    }
    dump_rejects();
}

/// The admission-rejection census on its own, so a PERIODIC probe can print it.
///
/// It used to print only from the full stats dump, which fires on the CLI's clean
/// exit — and a boot measured to `Done` is then killed, so the one readout that
/// names WHICH predicate refused a task was never available for a boot. The lane
/// probe calls this on its tick.
pub(crate) fn dump_rejects() {
    if let Some(map) = REJECTS.get() {
        let m = map.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(&(String, &'static str), &u64)> = m.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        let total: u64 = rows.iter().map(|(_, n)| **n).sum();
        eprintln!("[GILFREE-RINGS] admission rejections (impl, predicate, count) total={total}:");
        for ((impl_name, why), count) in rows.into_iter().take(24) {
            eprintln!("[GILFREE-RINGS]   {impl_name} rejected-by={why} n={count}");
        }
    }
}
