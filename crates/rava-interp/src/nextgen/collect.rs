//! Collection: the young Cheney pass, the major copy, and the forwarding log.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use crate::arena::ArenaPtr;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::Ordering;


thread_local! {
    pub(crate) static FWD_LOG: RefCell<std::collections::HashMap<u64, u64>> =
        RefCell::new(std::collections::HashMap::new());
}

/// The transient guest-collection kinds Variant B targets, ranked by 9.2c.2a alloc share.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionKind {
    ObjectArray, // [Ljava/lang/Object;  (29%)
    IntArray,    // [int                 (26%)
    DoubleArray, // [double              (13%)
    MapEntry,    // java/util/Map$Entry  (11%)
    ArrayList,   // java/util/ArrayList  (5%)
    Iterator,    // java/util/Iterator   (5%)
    HashMap,     // java/util/HashMap
}

/// INERT scaffold: try to bump-allocate a transient `kind` of `n` elements into THIS thread's arena
/// TLAB instead of the shared heap. Returns `None` today (not wired) → the caller uses the heap path.
/// The real impl (later increment) needs: (1) the arena cell layout for each kind, (2) an ESCAPE check
/// proving the collection is transient (never published past the phase — else it must stay heap/shared).
#[allow(dead_code)]
pub fn try_arena_collection(_kind: CollectionKind, _n: usize) -> Option<ArenaPtr> {
    None
}

/// RAVA_WATCH_ADDR — a single arena address (hex) to trace through alloc + every
/// collection edge (Wall #19 deterministic-address watchpoint). 0 = disabled.
pub fn watch_addr() -> u64 {
    use std::sync::OnceLock;
    static W: OnceLock<u64> = OnceLock::new();
    *W.get_or_init(|| {
        std::env::var("RAVA_WATCH_ADDR")
            .ok()
            .and_then(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(0)
    })
}

/// RAVA_WATCH_CLASS — a class-name substring: copy_obj_guarded logs every evacuation of a
/// matching object (regardless of its per-run-varying address), so the worldgen strand's
/// Holder$Direct / DensityFunction being copied (or NOT copied ⇒ silently freed) is visible
/// without knowing the exact address ahead of time.
pub fn watch_class() -> Option<&'static str> {
    use std::sync::OnceLock;
    static W: OnceLock<Option<String>> = OnceLock::new();
    W.get_or_init(|| std::env::var("RAVA_WATCH_CLASS").ok()).as_deref()
}

pub fn record_fwd(old: u64, new: u64) {
    FWD_LOG.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() > 4_000_000 {
            m.clear();
        }
        m.insert(old, new);
    });
    FWD_THIS.with(|s| s.borrow_mut().insert(old, new));
}
pub fn lookup_fwd(old: u64) -> Option<u64> {
    FWD_LOG.with(|m| m.borrow().get(&old).copied())
}
// Per-collection forward set (cleared at each tlab_collect start) for the
// post-collection frame writeback-miss validator: a frame slot still holding an
// addr forwarded THIS collection was not gathered — no slot-reuse false positives
// within the writeback window. Separate from the accumulating FWD_LOG classifier.
thread_local! {
    static FWD_THIS: RefCell<std::collections::HashMap<u64, u64>> =
        RefCell::new(std::collections::HashMap::new());
}
pub fn fwd_this_clear() {
    FWD_THIS.with(|s| s.borrow_mut().clear());
}
pub fn fwd_this_lookup(old: u64) -> Option<u64> {
    FWD_THIS.with(|s| s.borrow().get(&old).copied())
}

// ══════════════════════════════════════════════════════════════════════════════════════════
//  FAST TRACE — the per-class constants the collector was re-deriving per traced OBJECT.
// ══════════════════════════════════════════════════════════════════════════════════════════

/// ⭐**FAST TRACE** (`RAVA_NO_FAST_TRACE=1` opts out, and the whole rewrite block opts out
/// through `flags::emit_more_disabled()` so it prices as ONE A/B).
///
/// Every mask-driven walk in this file opened with `ref_mask_of(obj_class(obj))` followed by
/// `mask_empty(&mask)`, then `for i in 0..nf { if ref_bit(&mask, i) }`. Three costs, all of them
/// answers to questions about the CLASS, all of them paid once per traced OBJECT:
///
///   * `ref_mask_of` hands the mask OUT, so it clones an `Arc` — **two atomic RMWs per object**
///     (increment on the way out, decrement when the borrow ends) for a pointer that is only
///     read. The previous round removed the GLOBAL MUTEX behind this lookup and could not touch
///     what the lookup then did, because these files were not its. WAS: O(1) but with a refcount
///     pair per traced object. IS: zero atomics — the mask word is COPIED into a per-thread slot.
///   * `mask_empty` is `mask.iter().all(|w| *w == 0)` — a loop over the class's mask words, run
///     per traced object to answer a question whose input never changes. WAS: recomputed per
///     TRACED OBJECT. IS: one bool, computed once per (thread, class).
///   * the cell loop visited every CELL and tested a bit. WAS: O(cells per object). IS:
///     O(REFERENCE cells per object) — `trailing_zeros` walks the set bits directly, ascending,
///     so the visit ORDER and the visited SET are bit-for-bit what the old loop produced.
///
/// SOUNDNESS OF CACHING: the class registry is APPEND-ONLY. `register_class` either pushes a new
/// entry or returns an existing id, and `ref_mask` / `all_refs` are written once at that push and
/// never mutated (the only other producer, `register_uniform_ref_class`, panics unconditionally).
/// So a REGISTERED class id names the same mask forever. An id BEYOND the registry is one that
/// has not been registered YET — its miss is deliberately NOT cached below, because freezing an
/// "empty mask" answer for a class about to be registered with reference fields would leave every
/// future object of it untraced. That is the one way this cache could lose an object, and it is
/// closed by construction rather than by argument.
pub(crate) fn fast_trace() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        crate::flags::round7() && std::env::var_os("RAVA_NO_FAST_TRACE").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// One class's whole trace shape: which cells are references, whether ANY are, and whether ALL
/// are. Built from `ref_mask_and_uniform` once per (thread, class).
#[derive(Clone)]
pub(crate) struct TraceShape {
    /// The ref bitmask when the class fits in ONE mask word (≤64 declared cells) — every arena
    /// class this boot registers. Held BY VALUE, so reading it costs no refcount traffic.
    w0: u64,
    /// The >64-cell case only: the registry's own `Arc`, borrowed rather than copied. `None` on
    /// the narrow path ⇒ cloning a `TraceShape` is a plain 24-byte move with NO atomics.
    wide: Option<std::sync::Arc<[u64]>>,
    /// `mask_empty(mask)` — ⭐the named hand-off, now a constant of the class.
    empty: bool,
    /// `all_refs`: every cell is traced whatever the object's cell COUNT (array classes). Inert
    /// today — its constructor panics — but carried here so the three walks below are converted
    /// rather than left as the "unconverted `ref_mask_of` walk" debt `register_uniform_ref_class`
    /// refuses to be armed against.
    uniform: bool,
    /// False in a slot that has never been filled (the `Vec` is grown with these).
    filled: bool,
}

impl TraceShape {
    /// The answer for a class id that is not (yet) registered: trace nothing — exactly what
    /// `ref_mask_of`'s empty-mask fallback plus `mask_empty` produced.
    #[inline]
    fn unfilled() -> TraceShape {
        TraceShape { w0: 0, wide: None, empty: true, uniform: false, filled: false }
    }

    fn from_mask(mask: std::sync::Arc<[u64]>, uniform: bool) -> TraceShape {
        let empty = mask_empty(&mask);
        if mask.len() <= 1 {
            // Narrow (the whole boot): the single word is copied out and the `Arc` dropped here,
            // once per (thread, class), instead of being cloned once per traced object.
            TraceShape { w0: mask.first().copied().unwrap_or(0), wide: None, empty, uniform, filled: true }
        } else {
            // >64 declared cells: keep the registry's OWN allocation (moved, not copied).
            TraceShape { w0: 0, wide: Some(mask), empty, uniform, filled: true }
        }
    }

    /// Does this class have NO traced cells at all? Exactly the `mask_empty(&mask)` early-out
    /// the walks used, with the `all_refs` case excluded as `ref_mask_and_uniform`'s callers do.
    #[inline]
    pub(crate) fn traces_nothing(&self) -> bool {
        self.empty && !self.uniform
    }

    /// Is cell `i` a traced reference? Identical to `ref_bit(&mask, i)`: a narrow mask answers
    /// `false` for every `i >= 64`, which is what `mask.get(i / 64)` returned.
    #[inline]
    pub(crate) fn ref_bit(&self, i: usize) -> bool {
        match &self.wide {
            None => i < 64 && (self.w0 >> i) & 1 != 0,
            Some(m) => crate::nextgen::ref_bit(m, i),
        }
    }

    /// The traced cell indices of an object with `nf` cells, ASCENDING — the same set and the
    /// same order as `(0..nf).filter(|i| uniform || ref_bit(&mask, i))`, which is what makes the
    /// observable side-effect sequence (evacuation order, forwarding-log order) unchanged.
    #[inline]
    pub(crate) fn ref_cells(self, nf: u32) -> RefCells {
        let nf = nf as usize;
        if self.uniform {
            return RefCells { w: 0, i: 0, nf, wide: None, all: true };
        }
        if self.empty {
            return RefCells { w: 0, i: nf, nf, wide: None, all: false }; // spent: nothing to trace
        }
        match self.wide {
            // Mask off bits at or above `nf`: the old loop bounded `i` by the object's cell
            // count, and a class mask can be wider than a given object's cells.
            None => {
                let w = if nf < 64 { self.w0 & ((1u64 << nf) - 1) } else { self.w0 };
                RefCells { w, i: 0, nf, wide: None, all: false }
            }
            Some(m) => RefCells { w: 0, i: 0, nf, wide: Some(m), all: false },
        }
    }
}

/// Cursor over an object's traced cells. Owns its state (never borrows the shape table), so a
/// collector loop can evacuate — and mutate the heap — while iterating.
pub(crate) struct RefCells {
    w: u64,
    i: usize,
    nf: usize,
    wide: Option<std::sync::Arc<[u64]>>,
    all: bool,
}

impl Iterator for RefCells {
    type Item = usize;
    #[inline]
    fn next(&mut self) -> Option<usize> {
        if self.all {
            if self.i >= self.nf {
                return None;
            }
            let i = self.i;
            self.i += 1;
            return Some(i);
        }
        if let Some(m) = &self.wide {
            while self.i < self.nf {
                let i = self.i;
                self.i += 1;
                if crate::nextgen::ref_bit(m, i) {
                    return Some(i);
                }
            }
            return None;
        }
        if self.w == 0 {
            return None;
        }
        let i = self.w.trailing_zeros() as usize;
        self.w &= self.w - 1; // clear the lowest set bit
        Some(i)
    }
}

thread_local! {
    /// TRACE-SHAPE MIRROR, indexed by class id, filled on first touch and RETAINED for the life
    /// of the thread — never rebuilt per collection.
    ///
    /// SIZE: 32 B per entry (`u64` word + a niched `Option<Arc>` + three bools, padded) × the
    /// number of registered arena classes. A boot registering ~1,600 of them holds **~50 KB per
    /// collecting thread**, but a collection only ever TOUCHES the entries of classes actually
    /// present in the heap it walks, so the resident footprint is (live classes × 32 B) — a few
    /// KB, well inside L1, against the `Arc` + `mask_empty` pair it replaces. The table is
    /// indexed directly (no hashing, no probing): the class id IS the slot.
    static SHAPES: RefCell<Vec<TraceShape>> = const { RefCell::new(Vec::new()) };
}

/// The trace shape of `class_id` — one direct-indexed thread-local read on the hot path.
#[inline]
pub(crate) fn trace_shape(class_id: u32) -> TraceShape {
    if !fast_trace() {
        // ── kill-switch arm: the pre-round lookup, `Arc` clone and `mask_empty` included, run
        // per traced object exactly as before. Kept byte-for-byte so the A/B compares this round
        // against the previous one and not against a straw man.
        let (mask, uniform) = ref_mask_and_uniform(class_id);
        return TraceShape::from_mask(mask, uniform);
    }
    SHAPES.with(|s| {
        let i = class_id as usize;
        {
            let v = s.borrow();
            if let Some(sh) = v.get(i) {
                if sh.filled {
                    return sh.clone();
                }
            }
        }
        // Miss: once per (thread, class). The registry read happens WITHOUT the mirror borrow
        // held, so a re-entrant reader inside it cannot deadlock against this thread's own TLS.
        let (mask, uniform) = ref_mask_and_uniform(class_id);
        let sh = TraceShape::from_mask(mask, uniform);
        // ⚠CACHE ONLY A REGISTERED ID. Beyond the registry the answer is "not registered YET",
        // and it will change the moment the class is registered — see `fast_trace`'s soundness
        // note. `registry_len` takes the global lock, which is why it is on the MISS path only.
        if class_id < registry_len() {
            let mut v = s.borrow_mut();
            if v.len() <= i {
                v.resize_with(i + 1, TraceShape::unfilled);
            }
            v[i] = sh.clone();
        }
        sh
    })
}

thread_local! {
    /// The `RAVA_GC_DFS_COPY` worklist, RETAINED across collections on this thread. It is always
    /// handed back EMPTY (`dfs_drain` pops to exhaustion, and `dfs_stack_put` clears), so it can
    /// never hold a from-space pointer between collections — the capacity is what is reused, not
    /// the contents. SIZE: one `Vec` header per thread plus whatever the deepest drain reached.
    static DFS_STACK: RefCell<Vec<*mut u8>> = const { RefCell::new(Vec::new()) };
}
fn dfs_stack_take() -> Vec<*mut u8> {
    DFS_STACK.with(|s| {
        let mut v = std::mem::take(&mut *s.borrow_mut());
        v.clear();
        v.reserve(1024);
        v
    })
}
fn dfs_stack_put(mut v: Vec<*mut u8>) {
    v.clear();
    DFS_STACK.with(|s| *s.borrow_mut() = v);
}

/// RAVA_TRACE_STALEROOT / RAVA_TRACE_REFMASK, read ONCE per process instead of once per
/// COLLECTION. `env::var_os` walks the whole process environment block and allocates; both of
/// these are diagnostic switches, i.e. process constants, and `tlab_collect` was re-deriving
/// them on every minor GC. WAS: O(environment entries) + an allocation per collection.
/// IS: one `OnceLock` load.
pub(crate) fn staleroot_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TRACE_STALEROOT").is_some())
}
pub(crate) fn refmask_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TRACE_REFMASK").is_some())
}

// ══════════════════════════════════════════════════════════════════════════════════════════
//  FAST GC PAUSE — the sweep this collector repeats, and the compaction it never runs.
// ══════════════════════════════════════════════════════════════════════════════════════════

/// ⭐**`RAVA_NO_FAST_GCPAUSE=1` opts out**, and the whole block additionally opts out through
/// `flags::emit_more_disabled()`, so it prices as ONE A/B.
///
/// Two things in this file are charged against a serial boot, where a collection is time
/// nothing else can overlap:
///
///   1. **Every MINOR linearly sweeps the WHOLE tenured extent** (`tlab_collect`'s
///      remembered-set loop runs `0..old.top`), because the card table is a dry run nothing
///      reads. `nursery_cap`'s own note already names this as the dominant GC cost, and its
///      only mitigation — a bigger nursery — is saturated (4→32 MiB bought 166→154 s, 128 MiB
///      bought nothing more).
///   2. **Tenured is never compacted during a boot.** `arena_should_major` fires at 75% of
///      `tenured_cap()`, which is `RAVA_HARD_MB/2` = **2 GiB reserved**, i.e. a 1.5 GiB
///      trigger on a boot whose entire peak RSS is ~1 GB. So the major arm is, in practice,
///      unreachable — tenured only ever grows, dead promoted objects are never reclaimed, and
///      point 1 therefore re-sweeps the boot's accumulated GARBAGE on every single minor,
///      forever. The threshold does not reflect what the mutator is doing; it reflects the
///      size of a VA reservation.
///
/// This block does not touch what survives. It changes only (a) how a class's trace shape is
/// re-derived inside one sweep and (b) WHEN the already-correct `major_collect` runs.
pub(crate) fn fast_gcpause() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_GCPAUSE").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// A sweep-local last-class shape reuse — **two registers, no table, no allocation**.
///
/// The three linear sweeps in this file (`tlab_collect`'s remembered-set walk and its to-space
/// Cheney walk, `major_collect`'s Cheney walk) call `trace_shape(obj_class(obj))` once per
/// OBJECT. `trace_shape`'s fast mirror is behind `round7()`, which is OPT-IN, so the shipped
/// build takes the fallback arm every time: `ref_mask_and_uniform` → a `ClassMirror` lookup and
/// an **`Arc` clone (an atomic increment on the way out and a decrement when the shape drops)**,
/// then `mask_empty`, a loop over the mask words. Three answers about the CLASS, re-derived per
/// OBJECT, on the loop the note above calls the dominant GC cost.
///
/// ⛔This is deliberately NOT the demoted shape (a thread-local mirror indexed by class id,
/// retained across collections). That block measured slower and its table is 32 B × every
/// registered class. This holds ONE id and ONE shape, lives on the sweep's own stack frame, and
/// dies with it: a run of same-class objects — which is what a bump-allocated, Cheney-copied
/// region is full of (interned strings, arrays, an entry table's nodes) — pays a `u32` compare
/// instead of the lookup, and a miss pays exactly what today pays plus that compare.
///
/// SOUNDNESS: a class id names the same mask forever (the registry is append-only and
/// `ref_mask`/`all_refs` are written once at the push — the argument `fast_trace` sets out),
/// and in addition **no class can be registered while a sweep runs**: the collector holds the
/// GIL, executes no guest code, and nothing it calls registers a class. So reuse inside one
/// sweep cannot go stale even under the weaker premise. The sentinel id `u32::MAX` is safe
/// against a garbage header claiming that id: `TraceShape::unfilled()` is `w0=0, wide=None,
/// empty=true, uniform=false`, bit-for-bit what `trace_shape(u32::MAX)` builds today from the
/// registry's empty-mask fallback, so a false hit on it yields the identical (empty) cell set.
pub(crate) struct SweepShapes {
    cid: u32,
    sh: TraceShape,
    on: bool,
}

impl SweepShapes {
    #[inline]
    fn new() -> SweepShapes {
        SweepShapes { cid: u32::MAX, sh: TraceShape::unfilled(), on: fast_gcpause() }
    }
    /// The trace shape of `cid`. Identical in value to `trace_shape(cid)` on both arms.
    #[inline]
    fn get(&mut self, cid: u32) -> TraceShape {
        if !self.on {
            // ── kill-switch arm: the per-object lookup exactly as it runs today.
            return trace_shape(cid);
        }
        if cid != self.cid {
            self.sh = trace_shape(cid);
            self.cid = cid;
        }
        self.sh.clone()
    }
}

/// ⭐⭐**THE MAJOR TRIGGER, PRICED IN THE UNIT IT COMPETES IN — BYTES SWEPT.**
///
/// A major is worth running exactly when the remembered-set sweeping it will save exceeds the
/// copying it costs, and BOTH sides are bytes this collector already measures:
///
///   * the sweep it saves — `SWEPT_SINCE` accumulates `old.top` at every minor since the last
///     major, i.e. the tenured bytes actually walked;
///   * the copy it costs — after a major we know `LAST_LIVE` (`to.top`), and the next major
///     reads the from-space once and writes + Cheney-walks the live set, so its cost estimate
///     is `old.top + 2 × LAST_LIVE`. With no major yet (`LAST_LIVE == 0`) that is `old.top`:
///     the first major fires once tenured has been swept `MAJOR_AMORTISE` times over.
///
/// **The harm is bounded by construction, not by a hope:** the arm cannot fire until the
/// collector has already spent `MAJOR_AMORTISE ×` the estimated cost of the major on sweeping,
/// so major work can never exceed ~1/8 of the sweep work it is competing with. The upside is
/// unbounded in the other direction: after a compaction every subsequent minor sweeps `live`
/// instead of the boot's tenured high-water.
///
/// **And it retires itself on a workload it cannot help.** `BACKOFF` doubles (to a ceiling of
/// 64, i.e. an amortisation of 512) whenever a major reclaims less than a quarter of tenured —
/// which is the measurement that says the mutator's live set really is that big and compaction
/// is not paying — and resets to 1 when a major reclaims half or more. So a boot whose tenured
/// is genuinely live stops majoring after one or two, having paid for exactly the evidence it
/// used to stop.
///
/// ⚠**A major changes NOTHING about reachability or identity relative to a minor.**
/// `major_collect` is the existing, already-correct full collection; `arena_collect_jit` gathers
/// the FULL root set whenever its one hoisted `arena_should_major()` read says major, and every
/// address-keyed cache is invalidated on every collection regardless of kind (`GC_EPOCH`,
/// `arena_lambda_dedup.clear()`, `bump_static_gen()`). Running it at a different tenured
/// occupancy cannot change which objects survive — only when the survivors move.
mod major_policy {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    /// Σ tenured bytes linearly swept by the remembered-set walk since the last major. Bumped
    /// ONCE PER COLLECTION (never per object), and — unlike the `gcs` counters — bumped
    /// unconditionally, because the policy that reads it must work in the default build where
    /// `RAVA_GC_STATS` is off.
    pub(super) static SWEPT_SINCE: AtomicU64 = AtomicU64::new(0);
    /// `to.top` after the last major — the live set it found. 0 = no major has run.
    pub(super) static LAST_LIVE: AtomicU64 = AtomicU64::new(0);
    /// Amortisation multiplier; doubles when a major reclaims little (see above).
    pub(super) static BACKOFF: AtomicU64 = AtomicU64::new(1);
    /// Majors this policy asked for (diagnostic only, printed by RAVA_TRACE_ARENA).
    pub(super) static FIRED: AtomicU64 = AtomicU64::new(0);

    /// Sweeping must have covered this multiple of the next major's estimated cost.
    ///
    /// 8 is a two-sided choice, not a knob turned for speed. Larger is safer per major but
    /// the arm has to FIRE to be worth anything: a boot is tens of collections long, and a
    /// constant that only clears on the last one buys nothing measurable while still costing
    /// its own compaction. Smaller cannot be justified from here, because the estimate counts
    /// only the ARENA copy — a major additionally forces `arena_root_locations(false)`, the
    /// full Rust-heap arena-cell walk a minor restricts to the nursery window, and that walk
    /// is not measurable from this file.
    ///
    /// So the FIRST major is bought at 8× and then `BACKOFF` — which is the load-bearing half
    /// of the policy — re-prices it from what that major actually reclaimed. A compaction that
    /// frees less than a quarter of tenured doubles the constant immediately, so a bad first
    /// bet costs exactly one major and is never repeated at the same price.
    pub(super) const MAJOR_AMORTISE: u64 = 8;
    /// Below this, a full tenured sweep is a few milliseconds of streaming reads and a major's
    /// full-heap root gather is not worth paying — so the policy stays silent and the original
    /// 75%-of-cap rule is the only trigger.
    pub(super) const MAJOR_FLOOR: usize = 64 * 1024 * 1024;

    #[inline]
    pub(super) fn note_swept(bytes: u64) {
        SWEPT_SINCE.fetch_add(bytes, Relaxed);
    }

    /// Close the books on a major: spend the accumulated sweep evidence, record the live set
    /// the next estimate is built from, and move the backoff on what was actually reclaimed.
    ///
    /// ⚠`reclaimed` is deliberately measured as the shrink in the **SWEEP EXTENT**
    /// (`before_old → live`), not as garbage found. `live` includes the young survivors this
    /// major promoted, so a collection that frees plenty of dead tenured but promotes just as
    /// much from a 256 MiB nursery scores ZERO here — and that is correct, because every
    /// subsequent minor will sweep `live` regardless of where those bytes came from. The
    /// policy buys a smaller sweep; this is the number that says whether it got one.
    pub(super) fn note_major(before_old: u64, live: u64) {
        SWEPT_SINCE.store(0, Relaxed);
        LAST_LIVE.store(live, Relaxed);
        let reclaimed = before_old.saturating_sub(live);
        if before_old != 0 {
            if reclaimed * 2 >= before_old {
                BACKOFF.store(1, Relaxed); // ≥50% garbage — compaction is paying; stay eager.
            } else if reclaimed * 4 < before_old {
                // <25% garbage — the live set really is this big. Back off geometrically; at
                // the ceiling the arm is effectively retired for the rest of the process.
                BACKOFF.store((BACKOFF.load(Relaxed) * 2).min(64), Relaxed);
            }
        }
    }

    /// Diagnostic snapshot: (swept since last major, last live set, backoff, majors fired).
    pub(super) fn snapshot() -> (u64, u64, u64, u64) {
        (
            SWEPT_SINCE.load(Relaxed),
            LAST_LIVE.load(Relaxed),
            BACKOFF.load(Relaxed),
            FIRED.load(Relaxed),
        )
    }
}

/// Diagnostic view of the adaptive major policy — `(swept_since_major, last_live, backoff,
/// majors_fired)`. Read by `RAVA_TRACE_ARENA`'s per-collection line so the decision is
/// observable rather than inferred.
pub fn major_policy_snapshot() -> (u64, u64, u64, u64) {
    major_policy::snapshot()
}

pub fn tlab_collect(roots: &mut [*mut u8]) -> (u64, usize) {
    GC_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    crate::nextgen::tlab::with_tlab(|slot| {
        tlab_init_if_needed(slot);
        let h = slot.as_mut().unwrap();
        // Account for objects the JIT bump-allocated inline (cursor ahead of top):
        // sync so the nursery extent the collector reclaims covers them.
        sync_young_from_cursor(h);
        // M5.3 ESCAPE-TARGET SCAN (inert): tally the dense young region — the COMPLETE per-epoch alloc
        // histogram, now including the inline-JIT bump allocs picked up by sync_young_from_cursor. This
        // is the boot's real collector (tlab_collect), unlike the rarely-hit GenHeap::minor_gc. RO.
        if crate::escape_scan::enabled() {
            let base = h.young.base();
            let top = h.young.top;
            let nclasses = registry_len();
            let mut scan = 0usize;
            while scan < top {
                let p = unsafe { base.add(scan) };
                let cid = unsafe { obj_class(p) };
                let nf = unsafe { obj_nfields(p) };
                if cid >= nclasses || nf as usize > (top - scan) / 8 {
                    break; // implausible header → stop (never risk an OOB read)
                }
                crate::escape_scan::note(cid);
                scan += obj_size(nf);
            }
            crate::escape_scan::walk_done();
        }
        let before = h.copied;
        let scan_start = h.old.top;
        let nclasses = registry_len();
        let slog = staleroot_trace_enabled();
        // Copy-order placement (RAVA_GC_DFS_COPY, default OFF) — see `dfs_drain`.
        let dfs = dfs_copy_enabled();
        // RETAINED PER THREAD, not allocated per collection. WAS: a fresh 8 KB
        // `Vec::with_capacity(1024)` malloc'd and freed on every collection whenever
        // `RAVA_GC_DFS_COPY` is armed. IS: one allocation per THREAD, taken out of its cell for
        // the duration of the collection and put back empty (so nothing can observe, or root
        // off, a half-drained stack between collections).
        let mut dfs_stack: Vec<*mut u8> = if dfs { dfs_stack_take() } else { Vec::new() };
        if fwd_log_enabled() {
            fwd_this_clear();
        }

        // Remembered set (REMEMBERED-SET as roots): a tenured object may hold an
        // old→young pointer (recorded by the active card barrier). Scan tenured
        // objects' ref cells; any pointing into the nursery is an extra root, so a
        // young object kept alive ONLY by an old→young edge survives. (Conservative
        // full-tenured scan = correct superset; card-table filtering is a pure
        // speed refinement that does not change which objects survive.)
        let mut rem = 0usize;
        // FAST GC PAUSE: one `u32` compare instead of a `ClassMirror` lookup + `Arc` clone +
        // `mask_empty` for each object in a run of the same class. Sweep-local, so it cannot
        // outlive the GIL window in which the class registry is frozen (see `SweepShapes`).
        let mut shapes = SweepShapes::new();
        while rem < scan_start {
            let obj = unsafe { h.old.base().add(rem) };
            let nf = unsafe { obj_nfields(obj) };
            // FAST TRACE: the class's mask, its emptiness and its uniform flag in one
            // direct-indexed thread-local read (see `fast_trace`). The iterator yields only the
            // REFERENCE cells, ascending — the old `0..nf` + `ref_bit` filter without the
            // per-cell bitmask load. A ref-free class yields nothing, which is the `mask_empty`
            // early-out it replaces.
            for i in shapes.get(unsafe { obj_class(obj) }).ref_cells(nf) {
                let raw = unsafe { cell(obj, i) };
                if raw & 3 != 0 {
                    continue; // tagged cross-gen handle (Ref/Array), not an arena ptr
                }
                let child = raw as usize as *mut u8;
                if !child.is_null() && h.young.contains(child) {
                    let before = h.old.top;
                    let nc = h.copy_obj_guarded(child, nclasses, slog, "rem-set-cell");
                    unsafe { set_cell(obj, i, nc as i64) };
                    if dfs && h.old.top != before {
                        dfs_stack.push(nc);
                        h.dfs_drain(&mut dfs_stack, nclasses, slog);
                    }
                }
            }
            rem += obj_size(nf);
        }

        // Forward the explicit roots (copy young→tenured, update the slice in place).
        for r in roots.iter_mut() {
            if !r.is_null() {
                let before = h.old.top;
                *r = h.copy_obj_guarded(*r, nclasses, slog, "root");
                if dfs && h.old.top != before {
                    dfs_stack.push(*r);
                    h.dfs_drain(&mut dfs_stack, nclasses, slog);
                }
            }
        }
        // Cheney scan the to-space (everything copied this cycle).
        let mut scan = scan_start;
        // Fresh sweep-local reuse (the remembered-set walk's is spent). The to-space is Cheney
        // copy order, so same-class runs are if anything denser here than in tenured.
        let mut shapes = SweepShapes::new();
        while scan < h.old.top {
            let obj = unsafe { h.old.base().add(scan) };
            let nf = unsafe { obj_nfields(obj) };
            for i in shapes.get(unsafe { obj_class(obj) }).ref_cells(nf) {
                let raw = unsafe { cell(obj, i) };
                if raw & 3 != 0 {
                    continue; // tagged cross-gen handle (Ref/Array), not an arena ptr
                }
                let child = raw as usize as *mut u8;
                if !child.is_null() && h.young.contains(child) {
                    let nc = h.copy_obj_guarded(child, nclasses, slog, "to-space-cell");
                    unsafe { set_cell(obj, i, nc as i64) };
                }
            }
            scan += obj_size(nf);
        }
        if dfs {
            dfs_stack_put(dfs_stack);
        }
        // RAVA_TRACE_REFMASK — read-only diagnostic (runs AFTER the masked-ref scans,
        // BEFORE the nursery reset): any cell NOT marked as a reference by its class
        // ref_mask that STILL holds a plausible young pointer is a REF_MASK GAP — the
        // collector skipped it, so its young child is about to be reclaimed under a live
        // reference (Wall #19 stale receiver). Report the holder class once per (class,
        // cell) so the offending shape is identifiable.
        if refmask_trace_enabled() {
            let mut seen: std::collections::HashSet<(u32, usize)> = std::collections::HashSet::new();
            let mut d = 0usize;
            while d < h.old.top {
                let obj = unsafe { h.old.base().add(d) };
                let nf = unsafe { obj_nfields(obj) };
                // ⚠SKIP ARENA STRINGS. Their cells are not fields: cell[0] packs the byte/char
                // lengths, cell[1] the hash, and everything after is raw UTF-8. Scanning them
                // for "unmasked cell that looks like a young pointer" reports TEXT, and it
                // reports it convincingly — 11 of 11 hits on the offload run were strings whose
                // bytes happened to pass the plausible-header test. A validator whose output is
                // dominated by false positives does not get read.
                if unsafe { obj_class(obj) } == crate::nextgen::string_class_id() {
                    d += obj_size(nf);
                    continue;
                }
                let sh = trace_shape(unsafe { obj_class(obj) });
                for i in 0..nf as usize {
                    let masked = sh.uniform || sh.ref_bit(i);
                    let raw = unsafe { cell(obj, i) };
                    if raw == 0 || raw & 7 != 0 {
                        continue;
                    }
                    let child = raw as usize as *mut u8;
                    if h.young.contains(child) && arena_obj_header_plausible(child, nclasses) {
                        let cls = unsafe { obj_class(obj) };
                        if seen.insert((cls, i)) {
                            let cn = class_name(cls).map(|s| s.to_string()).unwrap_or_default();
                            let kn = class_name(unsafe { obj_class(child) })
                                .map(|s| s.to_string())
                                .unwrap_or_default();
                            // masked cell still holding a young ptr after BOTH the rem-set
                            // and to-space scans = the Cheney walk SKIPPED it (the object
                            // holding it was never reached/scanned); non-masked = ref_mask
                            // gap. Both leave the young child un-forwarded under a live ref.
                            let tag = if masked { "CHENEY-MISS(masked,unreached)" } else { "REFMASK-GAP(unmasked)" };
                            eprintln!(
                                "[{tag}] holder={cn} (cid={cls}, nf={nf}) cell[{i}] -> young {kn} — un-forwarded"
                            );
                        }
                    }
                }
                d += obj_size(nf);
            }
        }
        // MAJOR POLICY EVIDENCE: `scan_start` is exactly the tenured extent the
        // remembered-set sweep above walked, so this is the sweep bill the adaptive major
        // trigger amortises a compaction against. One relaxed `fetch_add` per COLLECTION —
        // not per object — and unconditional, because `gc_stats_enabled()` is off in the
        // build the policy has to work in.
        major_policy::note_swept(scan_start as u64);
        // COLLECTOR ACCOUNTING: one bump per collection (see `gcs`). `scan_start` is
        // exactly the tenured extent the remembered-set sweep above walked, and
        // `old.top - scan_start` is this cycle's Cheney promotion volume.
        if gc_stats_enabled() {
            crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::MINORS, 1);
            crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::REMSET_BYTES, scan_start as u64);
            crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::PROMOTED_BYTES, (h.old.top - scan_start) as u64);
            crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::COPIED_OBJS, h.copied - before);
        }
        h.young.top = 0; // reclaim nursery in O(1)
        poison_young(h); // RAVA_GC_POISON: stamp the freed nursery so a stale ptr faults
        // Republish the now-empty nursery extent so a post-collect inline bump
        // restarts at base.
        refresh_cursor(h);
        (h.copied - before, h.old.top)
    })
}

pub(crate) fn poison_young(h: &GenHeap) {
    if !poison_enabled() {
        return;
    }
    let base = h.young.base();
    let cap = h.young.cap();
    let words = cap / 8;
    let p = base as *mut u64;
    for i in 0..words {
        unsafe { *p.add(i) = 0xDEAD_BEEF_DEAD_BEEF };
    }
}

/// Is `p` a live arena object pointer in THIS thread's young or old space? (Permanent
/// interned strings live outside both and are intentionally excluded — they never
/// move, so a cell holding one needs no forwarding.) Used by the collector's root
/// scan to tell an arena pointer from a tagged handle / primitive precisely.
pub fn arena_contains(p: *mut u8) -> bool {
    crate::nextgen::tlab::with_tlab(|slot| {
        slot
            .as_ref()
            .map(|h| h.young.contains(p) || h.old.contains(p))
            .unwrap_or(false)
    })
}

/// SURVIVOR TRAP (diagnostic): this thread's young region extent `[base, base+cap)`.
/// After a collection `young.top` is 0 (every live young object evacuated), so any
/// registered cell still holding a pointer in this range is an UN-FORWARDED survivor
/// — the collector missed a root. Returns (0,0) if the TLAB is not yet initialised.
pub fn young_bounds() -> (usize, usize) {
    crate::nextgen::tlab::with_tlab(|slot| {
        slot
            .as_ref()
            .map(|h| {
                let b = h.young.base() as usize;
                (b, b + h.young.cap())
            })
            .unwrap_or((0, 0))
    })
}

/// READ BARRIER: if `cell` is a YOUNG arena pointer whose object was already
/// evacuated this run — its header forwarding word points at a live tenured copy —
/// return that tenured address; otherwise pass `cell` through unchanged. A live
/// young object has a null forwarding word (zeroed at alloc), so it passes through;
/// an evacuated-but-not-yet-reused slot still holds the forwarding word `copy_obj`
/// wrote. This corrects a stale young receiver/arg that an in-flight CALL BUFFER
/// (args_slot / dispatch callbuf — not in the rooted `cells_all` set) carried across
/// a Cheney collection that moved the object: the compiled callee would otherwise
/// dereference the freed slot (poison once a later alloc re-bumps it). Cheap: one
/// young-range test + (on a hit) a fwd-word load validated against tenured extent.
/// ⛔ HOISTING THIS BARRIER'S THREAD-LOCAL OUT OF THE PER-ARGUMENT LOOP WAS
/// BUILT, MEASURED AND REVERTED (2026-08-15).
///
/// `jit_direct_invoke` calls this once per ARGUMENT, and each call enters a
/// `RefCell<Option<Box<Heap>>>` thread-local — a borrow flag on a
/// destructor-registered slot. Reading the young/old extents once per CALL and
/// testing against them is the same test with fewer entries, so it looked free.
///
/// ⚠IT IS NOT MEASURED AS A LOSS, AND THE FIRST WRITE-UP OF IT HERE SAID SO
/// WRONGLY. That claim (`+6.2%`) came from comparing `FILL_MS` 6,593 in one gate
/// batch against 6,207 in another — and the SAME binary measured 6,207 and 5,934
/// in two batches, a 4.6% drift. Only WITHIN-batch pairs are comparable; the
/// cross-batch difference was smaller than the noise it was drawn from.
///
/// What is measured is that the change it was stacked with — the dispatch table —
/// carries the whole win (−19.9%, 9/9 pairs, within one batch), and that this
/// hoist is not separable from zero.
///
/// ★It is reverted anyway, on a rule rather than on a number: it carries a
/// standing obligation the reverted version does not — the extents may not be
/// held across anything that can allocate — and a change that is not measurably
/// positive does not get to add a soundness rule.

pub fn forward_if_moved(cell: i64) -> i64 {
    let a = cell as u64;
    if cell == 0 || a & 7 != 0 {
        return cell;
    }
    crate::nextgen::tlab::with_tlab(|slot| {
        let Some(h) = slot.as_ref() else { return cell };
        let p = a as *mut u8;
        if !h.young.contains(p) {
            return cell; // only young-region pointers can be a stale evacuated slot
        }
        let fwd = unsafe { obj_fwd(p) };
        // Forwarded iff the slot's fwd word names a live tenured object (a reused
        // slot's fwd word is null — alloc zeroes it — so it passes through here).
        if !fwd.is_null() && h.old.contains(fwd) {
            fwd as i64
        } else {
            cell
        }
    })
}

/// Tenured bytes currently in use (diagnostic + major-GC trigger).
pub fn arena_old_bytes() -> usize {
    crate::nextgen::tlab::with_tlab(|slot| slot.as_ref().map(|h| h.old.top).unwrap_or(0))
}

/// Tenured region capacity (diagnostic).
pub fn arena_old_cap() -> usize {
    crate::nextgen::tlab::with_tlab(|slot| slot.as_ref().map(|h| h.old.cap()).unwrap_or(0))
}

/// Should the next collection be a MAJOR (full tenured compaction)? True once
/// tenured passes 75% of its cap — minor GC never reclaims tenured, so without this
/// any object surviving one minor leaks permanently. `RAVA_ARENA_MAJOR_KB` lowers
/// the trigger (tests / tuning): major fires once `old.top` exceeds that many KiB.
///
/// ⭐**AND — `RAVA_NO_FAST_GCPAUSE=1` opts out — once the remembered-set sweeping already
/// paid since the last major exceeds `MAJOR_AMORTISE ×` the estimated cost of the next one.**
/// The 75%-of-cap rule is a threshold on a VA RESERVATION (`tenured_cap()` is
/// `RAVA_HARD_MB/2` = 2 GiB, so it wants 1.5 GiB of tenured), which a boot peaking near 1 GB
/// of TOTAL RSS never reaches. It therefore never fires, tenured never compacts, and the
/// O(tenured) sweep every minor pays is charged against the boot's accumulated garbage for
/// the rest of the process. The new arm is a threshold on what the mutator actually did — see
/// `major_policy` for the amortisation bound and the self-retiring backoff.
///
/// `RAVA_ARENA_MAJOR_KB`, when set, remains the WHOLE policy: it is the manual handle used to
/// force majors for tests and A/Bs, and an adaptive arm underneath it would make that handle
/// mean something else.
pub fn arena_should_major() -> bool {
    // `RAVA_ARENA_MAJOR_KB` is a PROCESS CONSTANT that was re-read on EVERY call — and this is
    // called twice per collection (`gc_collect`'s major/minor choice, then its accounting).
    // `env::var` walks the whole environment block and allocates a `String` to answer it.
    // WAS: O(environment entries) + one allocation + a parse, per call.
    // IS: one `OnceLock` load.
    use std::sync::OnceLock;
    static LO: OnceLock<Option<usize>> = OnceLock::new();
    let lo = *LO.get_or_init(|| {
        std::env::var("RAVA_ARENA_MAJOR_KB").ok().and_then(|s| s.parse::<usize>().ok())
    });
    crate::nextgen::tlab::with_tlab(|slot| {
        slot
            .as_ref()
            .map(|h| {
                if let Some(kb) = lo {
                    return h.old.top > kb * 1024;
                }
                // Unchanged safety trigger: tenured must never be allowed to reach its cap,
                // because `copy_obj`'s overflow is a hard, host-safe process exit.
                if h.old.top * 4 >= h.old.cap() * 3 {
                    return true;
                }
                if !fast_gcpause() || h.old.top < major_policy::MAJOR_FLOOR {
                    return false;
                }
                // Estimated cost of the NEXT major, in bytes touched: read the from-space
                // once (`old.top`), then write AND Cheney-walk the live set. `LAST_LIVE` is 0
                // until a major has run, which makes the first estimate `old.top` — i.e. the
                // first compaction is bought once tenured has been swept MAJOR_AMORTISE times
                // over, the most conservative starting point available without a measurement.
                let est = h.old.top as u64
                    + 2 * major_policy::LAST_LIVE.load(std::sync::atomic::Ordering::Relaxed);
                let budget = est
                    .saturating_mul(major_policy::MAJOR_AMORTISE)
                    .saturating_mul(major_policy::BACKOFF.load(std::sync::atomic::Ordering::Relaxed));
                major_policy::SWEPT_SINCE.load(std::sync::atomic::Ordering::Relaxed) >= budget
            })
            .unwrap_or(false)
    })
}

/// Evacuate one source object (in young OR old) into the major-GC to-space,
/// idempotent via the header forwarding slot. Pointers outside both spaces (perm
/// interned strings, primitives misread as pointers) pass through unchanged.
#[inline]
pub(crate) fn major_copy(src: *mut u8, young: &Region, old: &Region, to: &mut Region) -> *mut u8 {
    if !young.contains(src) && !old.contains(src) {
        return src; // perm / external → never moves
    }
    let fwd = unsafe { obj_fwd(src) };
    if !fwd.is_null() {
        return fwd; // already evacuated this cycle
    }
    let nf = unsafe { obj_nfields(src) };
    let size = obj_size(nf);
    if to.top + size > to.cap() {
        panic!("tenured exhausted during MAJOR GC (RAVA_HARD_MB) — abort, host safe");
    }
    to.ensure(to.top + size);
    let dst = unsafe { to.base().add(to.top) };
    to.top += size;
    unsafe {
        std::ptr::copy_nonoverlapping(src, dst, size);
        obj_set_fwd(src, dst);
    }
    dst
}

/// MAJOR collection (full tenured compaction). Minor GC promotes survivors into
/// tenured and NEVER reclaims it, so a tick object that lives through one minor
/// leaks forever. This evacuates the COMPLETE live set (reachable from `roots` +
/// transitively, across BOTH young and old) into a fresh to-space, forwards every
/// pointer, then swaps — dead tenured is dropped, `old.top` compacts to the live
/// size. `roots` is forwarded IN PLACE. Caller supplies the FULL root set (frames,
/// statics, JIT cells, AND native-collection slots — young or old). Returns tenured
/// bytes reclaimed. STW under the GIL (single-threaded TLAB access is sound).
///
/// Note: copying needs a transient second tenured region (peak ~2× tenured during
/// the collect); the old region is unmapped immediately after the swap. A sliding
/// mark-compact would avoid the peak — a future refinement.
pub fn major_collect(roots: &mut [*mut u8]) -> usize {
    // P3-S1 belt-and-braces: NEVER run a major on a GIL-free worker (its GenHeap old is a
    // zero-cap view → instant "tenured exhausted" panic that cannot unwind through the
    // extern "C" callback frames → process abort). Raise the region abort and return 0 —
    // callers see nothing reclaimed; the worker's allocation path then aborts to serial.
    if crate::parallel::gilfree() {
        crate::parallel::request_abort_overflow();
        return 0;
    }
    // A major moves TENURED objects too (the region is swapped wholesale below), so
    // every address-keyed cache must be invalidated here exactly as it is for a minor.
    // Bumped after the gilfree early-return, which collects nothing.
    GC_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    crate::nextgen::tlab::with_tlab(|slot| {
        tlab_init_if_needed(slot);
        let h = slot.as_mut().unwrap();
        sync_young_from_cursor(h);
        let before_old = h.old.top;
        if gc_stats_enabled() {
            crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::MAJORS, 1);
        }

        let mut to = Region::new_reserved(h.old.cap());
        // Forward the explicit roots (young OR old → to-space).
        for r in roots.iter_mut() {
            if !r.is_null() {
                *r = major_copy(*r, &h.young, &h.old, &mut to);
            }
        }
        // Cheney scan the to-space: trace each copied object's ref cells, evacuating
        // children (from young or old) and rewriting the cell to the new address.
        let mut scan = 0usize;
        // FAST GC PAUSE: sweep-local last-class shape reuse (see `SweepShapes`) — the same
        // one-compare substitution the minor's two walks take, on the walk that touches the
        // ENTIRE live set.
        let mut shapes = SweepShapes::new();
        while scan < to.top {
            let obj = unsafe { to.base().add(scan) };
            let nf = unsafe { obj_nfields(obj) };
            for i in shapes.get(unsafe { obj_class(obj) }).ref_cells(nf) {
                let raw = unsafe { cell(obj, i) };
                if raw == 0 || raw & 3 != 0 {
                    continue; // null / tagged cross-gen handle (not an arena ptr)
                }
                let child = raw as usize as *mut u8;
                let nc = major_copy(child, &h.young, &h.old, &mut to);
                if nc != child {
                    unsafe { set_cell(obj, i, nc as i64) };
                }
            }
            scan += obj_size(nf);
        }
        // RAVA_TRACE_REFMASK — same ref_mask-gap detector as tlab_collect, but for the
        // MAJOR collection: after the Cheney scan, any cell in the new tenured space NOT
        // marked as a reference yet still holding a plausible young pointer is a gap that
        // left its child un-evacuated (Wall #19). Runs before the nursery reset below.
        if refmask_trace_enabled() {
            let nclasses = registry_len();
            let mut seen: std::collections::HashSet<(u32, usize)> = std::collections::HashSet::new();
            let mut d = 0usize;
            while d < to.top {
                let obj = unsafe { to.base().add(d) };
                let nf = unsafe { obj_nfields(obj) };
                let sh = trace_shape(unsafe { obj_class(obj) });
                for i in 0..nf as usize {
                    if sh.uniform || sh.ref_bit(i) {
                        continue;
                    }
                    let raw = unsafe { cell(obj, i) };
                    if raw == 0 || raw & 7 != 0 {
                        continue;
                    }
                    let child = raw as usize as *mut u8;
                    if h.young.contains(child) && arena_obj_header_plausible(child, nclasses) {
                        let cls = unsafe { obj_class(obj) };
                        if seen.insert((cls, i)) {
                            let cn = class_name(cls).map(|s| s.to_string()).unwrap_or_default();
                            let kn = class_name(unsafe { obj_class(child) })
                                .map(|s| s.to_string())
                                .unwrap_or_default();
                            eprintln!(
                                "[REFMASK-GAP-MAJOR] holder={cn} (cid={cls}, nf={nf}) cell[{i}] -> young {kn} — un-forwarded"
                            );
                        }
                    }
                }
                d += obj_size(nf);
            }
        }
        let live = to.top;
        // MAJOR POLICY: close the books. The accumulated sweep evidence is SPENT (a
        // compaction has now been bought with it), `live` becomes the cost estimate the next
        // decision is built from, and the reclaimed fraction moves the backoff — so a heap
        // whose tenured turns out to be genuinely live stops paying for majors after one or
        // two, having used the majors themselves as the measurement. Recorded here, after the
        // gilfree early-return above, so a collection that reclaimed nothing never counts.
        major_policy::FIRED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        major_policy::note_major(before_old as u64, live as u64);
        // Swap: `to` becomes tenured (old region dropped → unmapped); nursery cleared.
        h.old = to;
        h.young.top = 0;
        poison_young(h); // RAVA_GC_POISON: stamp the freed nursery (see tlab_collect)
        refresh_cursor(h);
        before_old.saturating_sub(live)
    })
}
