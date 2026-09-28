//! Counting the arena: headers, cells, classes, edges and the GC statistics line.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use crate::arena::ArenaPtr;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Record a completed collection's wall time (the whole `arena_collect_jit` body).
#[inline]
pub fn gc_stats_note_nanos(ns: u64) {
    crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::NANOS, ns);
}

/// Note a closure-minting outcome. Exactly one call per `Plan::Lambda` execution.
#[inline]
pub fn gc_stats_note_lambda(outcome: LambdaBirth) {
    crate::nextgen::header::gcs::add(
        match outcome {
            LambdaBirth::DedupHit => &crate::nextgen::header::gcs::LAMBDA_DEDUP_HIT,
            LambdaBirth::Handle => &crate::nextgen::header::gcs::LAMBDA_HANDLE,
            LambdaBirth::ArenaMoving => &crate::nextgen::header::gcs::LAMBDA_MOVING,
            LambdaBirth::ArenaPinned => &crate::nextgen::header::gcs::LAMBDA_PINNED,
        },
        1,
    );
}

/// How one execution of the closure-minting arm ended.
#[derive(Clone, Copy)]
pub enum LambdaBirth {
    DedupHit,
    Handle,
    ArenaMoving,
    ArenaPinned,
}

/// RAII stopwatch over the whole closure-minting arm. The arm has several early
/// returns (dedup hit, arena birth, handle birth), so a guard is the only way to
/// time ALL of them without duplicating the accounting at each exit.
pub struct LambdaBirthClock(Option<std::time::Instant>);

impl Drop for LambdaBirthClock {
    fn drop(&mut self) {
        if let Some(t) = self.0 {
            crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::LAMBDA_BIRTH_NS, t.elapsed().as_nanos() as u64);
        }
    }
}

/// Start the closure-minting stopwatch (inert unless `RAVA_GC_STATS`).
#[inline]
pub fn lambda_birth_clock() -> LambdaBirthClock {
    LambdaBirthClock(gc_stats_enabled().then(std::time::Instant::now))
}

/// One-line cumulative collector report. `MB` for volumes, `ms` for time.
pub fn gc_stats_line() -> String {
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
    format!(
        "[GC-STATS] minor={} major={} gc_time={:.1}ms | remset_swept={:.0}MB promoted={:.0}MB ({} objs) \
         | pinned_swept={:.0}MB cells={} | tenured_now={:.1}MB pinned_now={:.1}MB | arena-lambda born moving={} pinned={}",
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::MINORS),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::MAJORS),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::NANOS) as f64 / 1.0e6,
        mb(crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::REMSET_BYTES)),
        mb(crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::PROMOTED_BYTES)),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::COPIED_OBJS),
        mb(crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::PINNED_BYTES)),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::PINNED_CELLS),
        mb(arena_old_bytes() as u64),
        mb(pinned_used_bytes() as u64),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::LAMBDA_MOVING) + crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::LAMBDA_PINNED),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::LAMBDA_PINNED),
    ) + &format!(
        "\n[GC-STATS] closure minting: {:.1}ms total | dedup_hit={} handle={} arena_moving={} arena_pinned={} \
         | site_intern={:.1}ms class_id={:.1}ms",
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::LAMBDA_BIRTH_NS) as f64 / 1.0e6,
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::LAMBDA_DEDUP_HIT),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::LAMBDA_HANDLE),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::LAMBDA_MOVING),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::LAMBDA_PINNED),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::LAMBDA_INTERN_NS) as f64 / 1.0e6,
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::LAMBDA_CID_NS) as f64 / 1.0e6,
    ) + &format!(
        "\n[GC-STATS] closure snapshots: arena {}x {:.1}ms (site_get {:.1}ms) | handle {}x {:.1}ms",
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::SNAP_ARENA_N),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::SNAP_ARENA_NS) as f64 / 1.0e6,
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::SITE_GET_NS) as f64 / 1.0e6,
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::SNAP_HANDLE_N),
        crate::nextgen::header::gcs::get(&crate::nextgen::header::gcs::SNAP_HANDLE_NS) as f64 / 1.0e6,
    )
}

/// Record a closure-snapshot call. `arena` selects the representation bucket.
#[inline]
pub fn gc_stats_note_snapshot(arena: bool, ns: u64) {
    if arena {
        crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::SNAP_ARENA_N, 1);
        crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::SNAP_ARENA_NS, ns);
    } else {
        crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::SNAP_HANDLE_N, 1);
        crate::nextgen::header::gcs::add(&crate::nextgen::header::gcs::SNAP_HANDLE_NS, ns);
    }
}

/// Live tenured population by class: `(name, objects, bytes)`, biggest first. This is
/// what the per-minor remembered-set sweep actually walks, so it names the classes
/// paying for collection COUNT — the measurement lifetime routing has to move.
pub fn tenured_class_histogram(top_n: usize) -> Vec<(String, u64, u64)> {
    let nclasses = registry_len();
    let mut per: std::collections::HashMap<u32, (u64, u64)> = std::collections::HashMap::new();
    crate::nextgen::tlab::with_tlab(|slot| {
        let Some(h) = slot.as_ref() else { return };
        let (base, top) = (h.old.base(), h.old.top);
        let mut scan = 0usize;
        while scan < top {
            let obj = unsafe { base.add(scan) };
            let cid = unsafe { obj_class(obj) };
            let nf = unsafe { obj_nfields(obj) };
            let sz = obj_size(nf);
            if cid >= nclasses || scan + sz > top {
                break; // malformed header → stop rather than risk an OOB read
            }
            let e = per.entry(cid).or_insert((0, 0));
            e.0 += 1;
            e.1 += sz as u64;
            scan += sz;
        }
    });
    let mut rows: Vec<(String, u64, u64)> = per
        .into_iter()
        .map(|(cid, (n, b))| {
            (class_name(cid).map(|s| s.to_string()).unwrap_or_else(|| format!("cid#{cid}")), n, b)
        })
        .collect();
    rows.sort_by_key(|(_, _, b)| std::cmp::Reverse(*b));
    rows.truncate(top_n);
    rows
}

/// Identity hash of an arena object (System.identityHashCode / acmp identity).
/// Lazily STAMPED into the object header on first request and read back forever
/// after — stable across Cheney evacuation (the hash word rides along in the
/// copied header). 0 in the header means "unstamped"; a stamped value is always
/// non-zero and non-negative (mask 0x7fff_ffff), matching the JVM convention and
/// keeping MC's RegistryID slot index (`hash & mask`) well-behaved.
pub fn arena_identity_hash(p: ArenaPtr) -> i32 {
    let ptr = p.as_ptr();
    // The header word is read ONCE and shared with the probe below. It used to be read twice
    // (the probe's `obj_hash(ptr) == 0` and then `existing`) and the probe's `gilfree()` call
    // ran on EVERY identity-hash request whether or not the probe was armed — an unarmed
    // diagnostic paying per CALL. WAS: two header loads + a `gilfree()` per request. IS: one
    // header load, and the disarmed probe is a single `OnceLock` load that short-circuits first.
    let existing = unsafe { obj_hash(ptr) };
    // RAVA_IDHASH_PROBE (diagnostic): count id-hash STAMPS taken inside a gilfree region — does
    // the parallel FILL compute even consume identityHashCode? If 0 during the fork, the shared
    // monotonic counter's race-order is NOT the divergence source and the id-hash suspect is out.
    #[cfg(feature = "next-gen-gc")]
    if existing == 0 {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *ON.get_or_init(|| std::env::var_os("RAVA_IDHASH_PROBE").is_some())
            && crate::parallel::gilfree()
        {
            crate::parallel::CNT_IDHASH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if existing != 0 {
        return existing as i32;
    }
    // MONOTONIC counter stamp (was xorshift-random). Java permits any stable value, and a
    // counter makes the stamp a process-lifetime-UNIQUE object id: the header word moves with
    // the object (evacuator copies it) and no two objects ever share one — random stamps
    // collided at birthday rate, which both aliased identity-map keys (latent) and blocks
    // using the stamp as a memo key (the RAVA_VALKEY_LIFETIME map-key memo relies on
    // uniqueness). 31-bit wrap is unreachable (only on-demand stamps consume ids).
    static IDHASH_NEXT: AtomicU32 = AtomicU32::new(1);
    let mut h = IDHASH_NEXT.fetch_add(1, Ordering::Relaxed) & 0x7fff_ffff;
    if h == 0 {
        h = IDHASH_NEXT.fetch_add(1, Ordering::Relaxed) & 0x7fff_ffff; // skip the sentinel on wrap
    }
    unsafe { obj_set_hash(ptr, h) };
    h as i32
}

/// GC bridge: every HEAP handle (Value::Ref / Value::Array) reachable from a live
/// arena object's cells. The handle-based mark-sweep GC scans frames, statics and
/// handle-object fields — but NOT arena cells — so a handle held ONLY by an arena
/// object (e.g. RegistryID's keys/values arrays inside an arena RegistryID) would
/// be collected as unreachable, leaving a dangling handle.
///
/// SUPERSEDED by the unified mark (`arena_trace_refs` + `Mark::Arena`): this blind
/// full-tenured scan also surfaced handles held by DEAD tenured arena objects (a
/// minor never reclaims tenured), pinning dead handle↔arena cycles → `tenured
/// exhausted`. Retained only for its regression test (the tagged-handle decoding the
/// unified walk shares); no production caller.
#[allow(dead_code)]
pub fn arena_handle_roots(out: &mut Vec<crate::value::Value>) {
    crate::nextgen::tlab::with_tlab(|slot| {
        let Some(h) = slot.as_ref() else { return };
        for region in [&h.young, &h.old] {
            let base = region.base();
            let mut off = 0usize;
            while off < region.top {
                let obj = unsafe { base.add(off) };
                let nf = unsafe { obj_nfields(obj) };
                for i in trace_shape(unsafe { obj_class(obj) }).ref_cells(nf) {
                    let raw = unsafe { cell(obj, i) };
                    match raw & 3 {
                        1 => out.push(crate::value::Value::Ref((raw >> 2) as u32)),
                        2 => out.push(crate::value::Value::Array((raw >> 2) as u32)),
                        _ => {} // arena ptr / null / boxed Str (box holds its own Arc)
                    }
                }
                off += obj_size(nf);
            }
        }
    });
}

/// Trace a LIVE arena object's REFERENCE cells for the unified legacy-GC mark:
/// surface each tagged heap handle it holds (`on_handle` with Ref/Array) and each
/// untagged arena CHILD pointer (`on_child`). The mark reaches a handle ONLY through
/// arena objects that are themselves reachable from a real root — replacing
/// `arena_handle_roots`'s blind full-tenured scan, which kept dead handle↔arena
/// reference cycles alive (the codec-build StringBuilder leak / tenured exhaustion).
/// A child that fails the strict arena-pointer / header-plausibility check is a
/// primitive in a ref cell (defensive) and skipped — never dereferenced.
pub fn arena_trace_refs(
    p: *mut u8,
    nclasses: u32,
    on_handle: &mut dyn FnMut(crate::value::Value),
    on_child: &mut dyn FnMut(*mut u8),
) {
    // Guard: only a plausible live arena object is safe to read. A stale/false
    // `Value::Obj` (a primitive that aliases a young address) fails this and is
    // skipped, so the mark never derefs a garbage header.
    // ⚠**AND THE ADDRESS MUST BE COMMITTED, WHICH THE RANGE CHECK DOES NOT ESTABLISH** (row 420).
    // `is_arena_obj_addr` bounds by `arena_high_water()`, which on Windows includes every region's
    // RESERVED-but-uncommitted tail — reading a header there is the `0xC0000005` this pass took.
    if !is_arena_obj_addr(p as u64)
        || crate::nextgen::region::in_uncommitted_tail(p as u64)
        || !arena_obj_header_plausible(p, nclasses)
    {
        return;
    }
    let nf = unsafe { obj_nfields(p) };
    // `uniform` FIRST, for the same reason as the two collectors: an array class carries an
    // empty mask BY DESIGN, so `mask_empty` would return here and every element of every
    // reference array would go unrooted. This is the ROOTING walk — the one that keeps a heap
    // object alive when an arena cell is its only reference — so a miss here is a dangling
    // handle rather than a slow trace. Step (a) claimed "all three mask-driven loops consult
    // it"; there are FOUR, and this was the one it missed.
    //
    // FAST TRACE (`RAVA_NO_FAST_TRACE=1`): the `(mask, uniform)` pair, the `mask_empty` verdict
    // and the set-bit walk now come out of the per-thread trace-shape mirror instead of being
    // re-derived here. WAS, per LIVE ARENA OBJECT the unified mark visits: an `Arc` clone
    // (refcount pair), a `mask_empty` loop over the class's mask words, and a scan of all `nf`
    // cells. IS: one direct-indexed thread-local read plus one step per REFERENCE cell.
    // `ref_cells` yields the same indices in the same ascending order, so the sequence of
    // `on_handle` / `on_child` calls — and therefore the mark's traversal order — is unchanged.
    for i in trace_shape(unsafe { obj_class(p) }).ref_cells(nf) {
        let raw = unsafe { cell(p, i) };
        match raw & 3 {
            0 if raw != 0
                && is_arena_obj_addr(raw as u64)
                && !crate::nextgen::region::in_uncommitted_tail(raw as u64) =>
            {
                on_child(raw as usize as *mut u8)
            }
            1 => on_handle(crate::value::Value::Ref((raw >> 2) as u32)),
            2 => on_handle(crate::value::Value::Array((raw >> 2) as u32)),
            _ => {} // null / boxed Str (owns its own Arc) / non-arena primitive
        }
    }
}

/// Walk every LIVE arena object (young + old regions) once, calling `f` with its
/// pointer. Under the GIL at a safepoint the regions are quiescent, so a linear
/// header-walk is sound. Used by the dangling-edge validator (RAVA_GC_VALIDATE) to
/// audit the unified mark's COMPLETENESS — every arena object whose ref cell points
/// at a handle the legacy mark is about to sweep is a rooting gap.
pub fn arena_for_each_obj(f: &mut dyn FnMut(*mut u8)) {
    crate::nextgen::tlab::with_tlab(|slot| {
        let Some(h) = slot.as_ref() else { return };
        for region in [&h.young, &h.old] {
            let base = region.base();
            let mut off = 0usize;
            while off < region.top {
                let obj = unsafe { base.add(off) };
                let nf = unsafe { obj_nfields(obj) };
                f(obj);
                off += obj_size(nf);
            }
        }
    });
}

/// HEADER CENSUS — the honest ceiling for any OBJECT-LAYOUT proposal. Walks the live
/// arena and returns `(objects, live_bytes, cells, ref_cells, narrow_ref_saving)`.
/// `live_bytes` is the exact `sum(obj_size(nf))` the collector itself strides by, so
/// `objects * 8` is precisely what a 24→16 header would remove and `objects * 16` what
/// a 24→8 one would. Reported alongside the cell census so a layout proposal is argued
/// from the measured live set rather than from a guess about object shape.
///
/// `narrow_ref_saving` is the ceiling for the OTHER half of the object — 4-byte
/// REFERENCE cells (compressed-oops style) beside 8-byte primitive cells. It is NOT
/// `4 * ref_cells`: objects must stay 8-ALIGNED (`is_arena_obj_addr` asserts `a & 7 == 0`,
/// the forwarding word is `(addr - ARENA_FLOOR) >> 3`, and the 0b00 cell tag needs the
/// low 3 bits of every arena address clear), so an object whose ref cells are packed
/// contiguously still rounds up to a multiple of 8. Per object that is exactly
/// `save = nf*8 - (nprim*8 + align8(nref*4))` = `8 * floor(nref / 2)`,
/// i.e. **an object with an ODD number of ref cells wastes the last 4 bytes on padding,
/// and an object with exactly ONE ref cell saves NOTHING AT ALL.** Counting it any other
/// way overstates the prize by the padding, which on a graph of small nodes is most of it.
pub fn header_census(nclasses: u32) -> (u64, u64, u64, u64, u64) {
    let (mut objs, mut bytes, mut cells) = (0u64, 0u64, 0u64);
    let (mut ref_cells, mut narrow_saving) = (0u64, 0u64);
    arena_for_each_obj(&mut |p| {
        if !arena_obj_header_plausible(p, nclasses) {
            return;
        }
        let nf = unsafe { obj_nfields(p) } as u64;
        objs += 1;
        cells += nf;
        bytes += obj_size(nf as u32) as u64;
        // The traced-cell COUNT straight off the shape's set-bit walk: same predicate, and a
        // ref-free class yields 0, which is the `mask_empty` early-out it replaces (0 ref cells
        // and 0 narrow saving are what that return contributed).
        let nref = trace_shape(unsafe { obj_class(p) }).ref_cells(nf as u32).count() as u64;
        ref_cells += nref;
        narrow_saving += 8 * (nref / 2);
    });
    (objs, bytes, cells, ref_cells, narrow_saving)
}

/// COVERAGE CENSUS (RAVA_CELL_CENSUS) — the value-model rewrite's only open question.
/// Step 6's bare-deref flip (worth a MEASURED 2.43× on ref-deref-dense code, see
/// docs/value-model-rewrite-plan.md) is sound only when EVERY reference cell in the
/// live graph is 0b00. Walk every live arena object and histogram its ref-kind cells
/// by low-2-bit tag, grouped by owning class: `[n0b00, n0b01_handle, n0b10_array,
/// n0b11_box]`. The residue buckets ARE the migration worklist, ranked by how much
/// they block the flip — measured, not guessed.
/// `on_handle` / `on_array` / `on_box` receive every blocking cell's payload, so the
/// caller (which owns the heap and the box table) can histogram what each bucket is
/// actually MADE OF. The holder tells you where the blocking cells live; the content
/// tells you which mechanism must change to clear them — and the two buckets both
/// turned out to be mixtures, not monoliths: the array bucket contains internal
/// growable storage (`$elems`/`$sb`/`$buf`) that is not a Java array at all, and the
/// box bucket mixes interned strings, boxed primitives and unrepresentable sentinels.
pub fn cell_census(
    nclasses: u32,
    // The handle arm gets its HOLDER too (class id, cell slot), for the same reason the array
    // arm does: a cell in a SYNTHETIC `$…` field is never bare-dereferenced — no bytecode
    // names such a field, so the JIT emits no getfield for it, and the natives that do read it
    // go through `get_field`/`arena_getfield`, which understand tags. Counting those as
    // blockers over-states the flip's remaining work.
    on_handle: &mut dyn FnMut(u32, u32, usize),
    // The array arm also gets its HOLDER (class id, cell slot). Row 79: the census's own
    // comment says this bucket is mostly `$elems`/`$sb`/`$buf` — the synthetic growable
    // storage of native-backed collections — not guest arrays, and two rows were scoped
    // wrongly by reading the bucket's TAG as its meaning. Only the holder can tell a
    // collection's internal buffer from a real `boolean[]` in a block-state cache.
    on_array: &mut dyn FnMut(u32, u32, usize),
    on_box: &mut dyn FnMut(usize),
) -> Vec<(Arc<str>, [u64; 4])> {
    let mut per_class: std::collections::HashMap<u32, [u64; 4]> = std::collections::HashMap::new();
    // TENURED-ONLY TALLY, beside the full one. `arena_for_each_obj` strides the bump regions
    // linearly, so it visits every object ever ALLOCATED there — including garbage awaiting
    // collection. Every absolute number this census has ever produced therefore includes dead
    // objects' cells. An object in the OLD region has survived at least one collection, so a
    // tenured-only count is a floor made of definitely-live cells, and the pair (tenured,
    // total) brackets the truth instead of asserting the upper bound as if it were exact.
    let mut tenured_cells = [0u64; 4];
    arena_for_each_obj(&mut |p| {
        if !arena_obj_header_plausible(p, nclasses) {
            return;
        }
        let cid = unsafe { obj_class(p) };
        let nf = unsafe { obj_nfields(p) };
        let sh = trace_shape(cid);
        // The early-out is kept EXPLICIT (rather than falling out of an empty iterator) because
        // it also decides whether a `per_class` row exists at all: a ref-free class must not
        // appear in the report as a row of zeros.
        if sh.traces_nothing() {
            return;
        }
        let tenured = ArenaPtr::new(p).map_or(false, |ap| !tlab_in_nursery(ap));
        let slot = per_class.entry(cid).or_insert([0; 4]);
        for i in sh.ref_cells(nf) {
            let raw = unsafe { cell(p, i) };
            slot[(raw & 3) as usize] += 1;
            if tenured {
                tenured_cells[(raw & 3) as usize] += 1;
            }
            match raw & 3 {
                1 => on_handle((raw >> 2) as u32, cid, i),
                2 => on_array((raw >> 2) as u32, cid, i),
                3 => on_box((raw >> 2) as usize),
                _ => {}
            }
        }
    });
    let mut out: Vec<(Arc<str>, [u64; 4])> = per_class
        .into_iter()
        .map(|(cid, counts)| {
            let name = class_layout(cid)
                .map(|(n, _)| n)
                .unwrap_or_else(|| Arc::from(format!("[class#{cid}]").as_str()));
            (name, counts)
        })
        .collect();
    // Rank by the cells that BLOCK the flip (everything that is not 0b00).
    out.sort_by_key(|(_, c)| std::cmp::Reverse(c[1] + c[2] + c[3]));
    TENURED_CELLS.with(|t| t.set(tenured_cells));
    out
}

/// The tenured-only tally from the last `cell_census`, as `[0b00, handle, array, box]`.
/// Reported beside the full one so an absolute claim can be bracketed: tenured is a floor of
/// definitely-live cells, the full count an upper bound that includes uncollected garbage.
pub fn last_tenured_cells() -> [u64; 4] {
    TENURED_CELLS.with(|t| t.get())
}
thread_local! {
    static TENURED_CELLS: std::cell::Cell<[u64; 4]> = const { std::cell::Cell::new([0; 4]) };
}

/// LOCALITY CENSUS — the other half of the bare-deref story. Once the tag
/// discrimination is gone (plan step 6) a reference deref is NOTHING BUT
/// `load.i64`, so its cost is decided entirely by whether the child object sits
/// in the same cache line region as the parent that points at it. The Cheney
/// collector evacuates BREADTH-first (a linear scan of to-space), which places
/// siblings together and pushes a parent's referents arbitrarily far away;
/// approximately-depth-first copying (Moon) places a parent next to the child it
/// points at instead. Before building any of that, MEASURE the distance that
/// actually exists: histogram |child − parent| over every live arena→arena edge,
/// bucketed by 64-byte cache lines. If the mass already sits in the first buckets
/// the whole idea is dead and costs nothing to have asked.
///
/// Buckets: [same line, ≤2, ≤4, ≤8, ≤16, ≤64, ≤256, ≤1024, ≤4096, farther].
pub fn edge_locality_census(nclasses: u32) -> [u64; 10] {
    const EDGES: [u64; 9] = [1, 2, 4, 8, 16, 64, 256, 1024, 4096];
    let mut hist = [0u64; 10];
    arena_for_each_obj(&mut |p| {
        if !arena_obj_header_plausible(p, nclasses) {
            return;
        }
        let cid = unsafe { obj_class(p) };
        let nf = unsafe { obj_nfields(p) };
        for i in trace_shape(cid).ref_cells(nf) {
            let raw = unsafe { cell(p, i) };
            // arena→arena edges only: a handle/array/box cell has no address to
            // be near, and after the migration none of them will exist.
            if raw & 3 != 0 || raw == 0 || !is_arena_obj_addr(raw as u64) {
                continue;
            }
            let lines = (p as u64).abs_diff(raw as u64) / 64;
            let b = EDGES.iter().position(|&e| lines < e).unwrap_or(EDGES.len());
            hist[b] += 1;
        }
    });
    hist
}
