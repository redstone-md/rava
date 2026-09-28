//! The generational heap itself: nursery, tenured space, and the bump allocation over both.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nextgen::*;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

/// Words of reference bitmask `dfs_drain`'s borrowing arm copies onto its stack — 4 × 64 = the
/// first 256 declared cells of a class.
///
/// The buffer exists so the mask can be read WITHOUT cloning its `Arc`: the copy is 32 bytes of
/// stack stores against two atomic read-modify-writes per traced object, and it ends the borrow
/// of the `ClassMirror` before the loop calls back into `&mut self`. A class needing more words
/// falls through to the `ref_mask_and_uniform` arm, which is the code that ran before. The wide
/// objects on this heap are the arena arrays, and those are `all_refs` with an EMPTY mask, so
/// they take the uniform branch and never consult these words at all.
const DFS_MASK_WORDS: usize = 4;

/// Generational heap: young nursery (bump) + tenured space, Cheney copy on a full
/// nursery. A per-thread instance IS the TLAB (no shared state on the alloc path).
pub struct GenHeap {
    pub young: Region,
    pub old: Region,
    pub shapes: Vec<ClassShape>,
    pub minor_gcs: u64,
    pub copied: u64,
}

impl GenHeap {
    pub fn new(young_cap: usize, old_cap: usize, shapes: Vec<ClassShape>) -> Self {
        GenHeap {
            young: Region::new(young_cap),
            old: Region::new_reserved(old_cap),
            shapes,
            minor_gcs: 0,
            copied: 0,
        }
    }

    /// G3 — a fork WORKER heap whose YOUNG is a NON-OWNING VIEW over `[base, base+cap)`, a disjoint
    /// sub-range of MAIN's nursery. Worker allocations bump into main's persistent heap (so an
    /// escaping object survives the region). OLD is an empty view — a worker NEVER promotes/
    /// collects (it aborts the region on young-full, see `tlab_try_alloc`'s gilfree branch), so
    /// old is never touched. Shapes are empty: the bump paths take `nfields` explicitly.
    pub fn view_young(base: *mut u8, cap: usize) -> Self {
        GenHeap {
            young: Region::view(base, cap),
            old: Region::view(std::ptr::null_mut(), 0),
            shapes: Vec::new(),
            minor_gcs: 0,
            copied: 0,
        }
    }

    /// Thread-local bump allocate. Caller initialises every cell before the next
    /// GC (matches a JVM TLAB bump + `<init>`). `roots` are forwarded if a minor
    /// GC fires.
    #[inline]
    pub fn alloc(&mut self, class_id: u32, roots: &mut [*mut u8]) -> *mut u8 {
        let nf = self.shapes[class_id as usize].nfields;
        let size = obj_size(nf);
        if self.young.top + size > self.young.cap() {
            self.minor_gc(roots);
            if self.young.top + size > self.young.cap() {
                panic!("nursery cannot fit a single {size}-byte object");
            }
        }
        let p = unsafe { self.young.base().add(self.young.top) };
        self.young.top += size;
        unsafe {
            *(p as *mut u64) = (nf as u64) << 32 | class_id as u64;
            obj_init_hdr_tail(p); // forwarding + identity hash unset
        }
        p
    }

    /// Cheney copy nursery → tenured. Only young objects move.
    pub fn minor_gc(&mut self, roots: &mut [*mut u8]) {
        self.minor_gcs += 1;
        // M5.3 ESCAPE-TARGET SCAN (inert): before Cheney, linear-walk the dense young bump region and
        // tally every object's class — the COMPLETE per-epoch allocation histogram (inline-JIT + Rust).
        // Read-only; a plausibility guard stops the walk on any implausible header so a bad nfields can
        // never drive an out-of-bounds read. The young resets below regardless, so this changes nothing.
        if crate::escape_scan::enabled() {
            let nclasses = registry_len();
            let base = self.young.base();
            let top = self.young.top;
            let mut scan = 0usize;
            while scan < top {
                let p = unsafe { base.add(scan) };
                let cid = unsafe { obj_class(p) };
                let nf = unsafe { obj_nfields(p) };
                if cid >= nclasses || nf as usize > (top - scan) / 8 {
                    break; // implausible header → stop (don't risk an OOB read on a garbage nfields)
                }
                crate::escape_scan::note(cid);
                scan += obj_size(nf);
            }
            crate::escape_scan::walk_done();
        }
        // Same header-plausibility guard the MAJOR collector (`major_gc`) already
        // applies at every evacuation edge (root / rem-set-cell / to-space-cell).
        // minor_gc was missed: a ref cell (or root) holding a STALE young pointer with
        // a garbage header — a primitive that leaked into a ref-masked cell, or a
        // conservative slot left dangling into the freed nursery — was evacuated with
        // `copy_obj`, which read its bogus `nfields` and copied a wrong-sized blob into
        // tenured; the scan loop then drifted by `obj_size(garbage_nf)` to a MISALIGNED
        // offset and `obj_class` aborted ("misaligned pointer dereference … 0x2000…").
        // Guarding here skips the implausible pointer (it is dead; young resets below).
        // `GenHeap` owns this shape table.  In particular, the small standalone
        // heaps used by worker/test callers allocate class id 0 before the global
        // arena registry has necessarily loaded anything; validating those local
        // headers against `registry_len()` would silently treat a live root as a
        // stale pointer and discard it.  The scans below index `self.shapes` too,
        // so this is the one authoritative bound for this heap.
        let nclasses = self.shapes.len() as u32;
        for r in roots.iter_mut() {
            if !r.is_null() {
                *r = self.copy_obj_guarded(*r, nclasses, false, "minor-root");
            }
        }
        // REMEMBERED SET (brute-force): scan EVERY tenured object's ref cells, not just the
        // ones promoted in THIS collection. The card table (shadow().card_mark) is a dry-run
        // that the collector never reads, so an OLD object holding a YOUNG child — e.g. an
        // already-tenured Holder$Direct whose `value` points at a young DensityFunction created
        // after it was promoted — was never scanned: the young child evacuated via other roots
        // (its heap map/array holders) but the old cell kept the stale pre-move pointer, and
        // Holder.value() then returned a freed slot (the worldgen minValue UnsatisfiedLinkError).
        // Scanning from 0 forwards those old→young edges too. copy_obj on an already-tenured
        // child is a no-op (not young), so re-scanning old objects is idempotent + safe.
        let mut scan = 0usize;
        while scan < self.old.top {
            let obj = unsafe { self.old.base().add(scan) };
            let nf = unsafe { obj_nfields(obj) };
            let mask = self.shapes[unsafe { obj_class(obj) } as usize].ref_mask;
            // WIDTH AUDIT (row 77). This mask is a `u32` while its `ArenaClass` counterpart is
            // an `Arc<[u64]>` word bitset — so `1 << i` below exceeds the type width once
            // `nf > 32` and WRAPS in release, testing a foreign bit rather than missing one.
            // Whether that is reachable is an empirical question, not a reading one: count it.
            // Unconditional (two relaxed atomics on a path that already dereferences the
            // object header) so it cannot read zero merely because a gate was left unarmed.
            if nf > 32 {
                SHAPE_WIDE_SCANS.fetch_add(1, Ordering::Relaxed);
                SHAPE_MAX_NF.fetch_max(nf as u64, Ordering::Relaxed);
            }
            if mask != 0 {
                for i in 0..nf as usize {
                    if mask & (1 << i) != 0 {
                        let raw = unsafe { cell(obj, i) };
                        if raw & 3 != 0 {
                            continue; // tagged cross-gen handle (Ref/Array), not an arena ptr
                        }
                        let child = raw as usize as *mut u8;
                        if !child.is_null() {
                            let nc = self.copy_obj_guarded(child, nclasses, false, "minor-cell");
                            unsafe { set_cell(obj, i, nc as i64) };
                        }
                    }
                }
            }
            scan += obj_size(nf);
        }
        self.young.top = 0;
    }

    #[inline]
    pub(crate) fn copy_obj(&mut self, p: *mut u8) -> *mut u8 {
        if !self.young.contains(p) {
            return p;
        }
        let fwd = unsafe { obj_fwd(p) };
        if !fwd.is_null() {
            return fwd;
        }
        let nf = unsafe { obj_nfields(p) };
        let size = obj_size(nf);
        if self.old.top + size > self.old.cap() {
            let cid = unsafe { obj_class(p) };
            panic!(
                "tenured exhausted (RAVA_HARD_MB) — abort, host safe (old.top={}KB cap={}KB \
                 obj={:#x} nf={} size={} class_id={} class={:?} young_contains={})",
                self.old.top / 1024,
                self.old.cap() / 1024,
                p as usize,
                nf,
                size,
                cid,
                class_name(cid),
                self.young.contains(p),
            );
        }
        self.old.ensure(self.old.top + size);
        let np = unsafe { self.old.base().add(self.old.top) };
        self.old.top += size;
        unsafe {
            std::ptr::copy_nonoverlapping(p, np, size);
            obj_set_fwd(p, np);
        }
        if fwd_log_enabled() {
            record_fwd(p as u64, np as u64);
        }
        self.copied += 1;
        np
    }

    /// APPROXIMATELY-DEPTH-FIRST DRAIN (Moon) — placement, not work.
    ///
    /// Cheney evacuates BREADTH-first, so a parent and the children it points at end up
    /// arbitrarily far apart. Measured on this workload (`edge_locality_census`): of
    /// 1.27M live arena→arena edges, **82.3% span ≥4096 cache lines** and only 1.6% land
    /// within two — i.e. essentially every reference dereference is a fresh cache/TLB
    /// miss. That is invisible today because the value model's tag-discrimination ALU
    /// sits on top of every deref and hides the latency; after the bare-deref flip
    /// (measured 2.43× on ref-dense code) a deref is NOTHING but `load.i64`, and this
    /// distance becomes the residual cost.
    ///
    /// The drain copies a freshly-evacuated object's referents IMMEDIATELY, LIFO, so
    /// chains and tree spines land contiguously. It changes no copy COUNT and no
    /// invariant: the linear Cheney scan stays as the completeness sweep (it then finds
    /// every child already forwarded), the same plausibility gate is used, the same
    /// `raw & 3 != 0` tagged-cell skip applies, and forwarding-word idempotence makes a
    /// double push harmless. Freshness is read off `old.top` advancing — no change to
    /// `copy_obj`. The cap bounds the stack; anything beyond it is simply left to the
    /// linear sweep, so the result is always complete.
    pub(crate) fn dfs_drain(&mut self, stack: &mut Vec<*mut u8>, nclasses: u32, log: bool) {
        while let Some(obj) = stack.pop() {
            let nf = unsafe { obj_nfields(obj) } as usize;
            let cid = unsafe { obj_class(obj) };
            // ⭐**BIRTH 3, COLLECTOR HALF — BORROW THE MASK, AND WALK ITS SET BITS.**
            //
            // Two costs per TRACED OBJECT, i.e. once per surviving object per collection, for
            // ever — which is exactly what an object promoted early keeps charging:
            //
            //   1. `ref_mask_and_uniform` hands the mask out BY VALUE, so every object paid an
            //      `Arc<[u64]>` clone (`fetch_add`) here and its matching drop (`fetch_sub`, plus
            //      the last-reference branch) at the bottom of the iteration — two atomic RMWs to
            //      read two per-class CONSTANTS out of a `ClassMirror` that `mirror_store()` owns
            //      for the whole process and never mutates. A borrow answers the same question
            //      with a shared reference and no refcount traffic at all.
            //   2. the trace loop ran `0..nf` and asked `ref_bit` per cell — a bounds-checked
            //      slice index, a variable shift and a test — so a 40-field class with 3 reference
            //      cells did 40 iterations to find 3. Walking the mask word by word with
            //      `trailing_zeros` visits exactly the set bits: popcount iterations, not `nf`.
            //
            // ⚠**THE VISIT SET AND ITS ORDER ARE UNCHANGED.** Both arms visit
            // `{ i < nf : uniform || ref_bit(mask, i) }` in ASCENDING `i`, so the evacuation order
            // — and therefore the byte layout the survivors land in, and every address downstream
            // — is identical to the loop this replaces. That is what keeps the boot hashes exact.
            //
            // `uniform` (an array class) means EVERY cell is traced and the mask is empty BY
            // DESIGN, so it must be tested before any emptiness short-circuit: skipping on an
            // empty mask there would silently drop every element of every reference array.
            //
            // Cold arms, both falling through to the `ref_mask_and_uniform` code verbatim: the
            // kill switch (`RAVA_NO_FAST_BIRTH3=1`, or `RAVA_NO_FAST_LAYOUT=1` which is what
            // decides whether class layout may be read from the mirrors at all — see
            // `fast_trace3`), and a class whose mask is wider than `DFS_MASK_WORDS`, i.e. more
            // than 256 declared cells. The huge-`nf` objects are the arrays, and they are uniform
            // with an EMPTY mask, so they never reach that arm.
            if fast_trace3() {
                let mut words = [0u64; DFS_MASK_WORDS];
                // `None` = class id not registered. `ref_mask_and_uniform` answers that case with
                // `(empty_mask(), false)`, which `mask_empty` then turns into a skip — so skip
                // directly. `usize::MAX` = wider than the inline buffer; fall through.
                let shape = with_mirror(cid, |c| {
                    let n = c.ref_mask.len();
                    if n <= DFS_MASK_WORDS {
                        words[..n].copy_from_slice(&c.ref_mask);
                        (c.all_refs, n)
                    } else {
                        (c.all_refs, usize::MAX)
                    }
                });
                match shape {
                    None => continue,
                    Some((uniform, n)) if n != usize::MAX => {
                        if uniform {
                            for i in 0..nf {
                                self.dfs_cell(obj, i, nclasses, log, stack);
                            }
                        } else {
                            'words: for (wi, &word) in words[..n].iter().enumerate() {
                                let mut bits = word;
                                while bits != 0 {
                                    let i = wi * 64 + bits.trailing_zeros() as usize;
                                    bits &= bits - 1;
                                    // Set bits arrive in ascending order within a word, and words
                                    // in ascending order, so the first index past the object's
                                    // own field count ends the WHOLE walk, not just this word.
                                    if i >= nf {
                                        break 'words;
                                    }
                                    self.dfs_cell(obj, i, nclasses, log, stack);
                                }
                            }
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            let (mask, uniform) = ref_mask_and_uniform(cid);
            if !uniform && mask_empty(&mask) {
                continue;
            }
            for i in 0..nf {
                if !uniform && !ref_bit(&mask, i) {
                    continue;
                }
                self.dfs_cell(obj, i, nclasses, log, stack);
            }
        }
    }

    /// The per-cell body of `dfs_drain`: evacuate the child in cell `i` of `obj`, forward the
    /// cell to its new address, and queue the copy so its own referents follow it immediately.
    ///
    /// Lifted out so the two index arms above differ ONLY in which `i` they visit. Both arms ran
    /// this code longhand before the split; one definition is what stops the mask-walk arm from
    /// drifting away from the `ref_bit` arm it must stay bit-identical to.
    ///
    /// Unchanged from the loop it came out of: `raw & 3 != 0` skips a tagged cross-generation
    /// handle (Ref/Array), which is not an arena pointer; a null or already-tenured child is left
    /// alone; freshness is read off `old.top` advancing rather than out of `copy_obj`; and the
    /// push is capped so the stack stays bounded, with anything past the cap left to the linear
    /// Cheney sweep that follows — which is why the result is complete either way.
    #[inline]
    fn dfs_cell(&mut self, obj: *mut u8, i: usize, nclasses: u32, log: bool, stack: &mut Vec<*mut u8>) {
        const DFS_STACK_CAP: usize = 8192;
        let raw = unsafe { cell(obj, i) };
        if raw & 3 != 0 {
            return; // tagged cross-gen handle (Ref/Array), not an arena ptr
        }
        let child = raw as usize as *mut u8;
        if child.is_null() || !self.young.contains(child) {
            return;
        }
        let before = self.old.top;
        let nc = self.copy_obj_guarded(child, nclasses, log, "dfs-cell");
        unsafe { set_cell(obj, i, nc as i64) };
        if self.old.top != before && stack.len() < DFS_STACK_CAP {
            stack.push(nc);
        }
    }

    /// `copy_obj` with a header-plausibility guard. A YOUNG pointer whose header is
    /// garbage (class id ≥ `nclasses`, or an absurd field count) is NOT a real live
    /// object — a stale/dangling root or ref cell left pointing into the freed
    /// nursery. Evacuating it would read its bogus `nfields` as a multi-GB object and
    /// FALSE-panic "tenured exhausted". Leave such a pointer unchanged (it is dead;
    /// the program must not deref it — the young region is reset right after). `src`
    /// names the GC edge for the RAVA_TRACE_STALEROOT hunt.
    ///
    /// ⭐**THE GUARD ASKED THE SAME TWO QUESTIONS UP TO FIVE TIMES PER EDGE.** This runs once
    /// per root, once per remembered-set cell and once per to-space cell of every collection —
    /// i.e. once per live REFERENCE in the heap — and it evaluated `young.contains(p)` three
    /// times and `arena_obj_header_plausible(p, ..)` twice, the second of which is a second
    /// load of the object's header word. On a from-space object that header is a cold line, so
    /// the duplicate is a cache miss re-taken for an answer already in a register. Both are now
    /// computed once, and the not-young case (the majority — every already-tenured child the
    /// remembered-set sweep walks past) returns before either header read happens at all.
    ///
    /// The two WATCH hunts moved behind one combined gate into a cold, never-inlined helper.
    /// They are unchanged when armed (same predicates, same order, same text); what changes is
    /// that an unarmed build no longer carries their `class_name` / `Backtrace` / formatting
    /// machinery inside the hot function, where it costs register pressure on every edge.
    pub(crate) fn copy_obj_guarded(&mut self, p: *mut u8, nclasses: u32, log: bool, src: &str) -> *mut u8 {
        if watch_any() {
            self.watch_report(p, nclasses, src);
        }
        // NOT YOUNG ⇒ nothing to forward and nothing to guard. `copy_obj` opens with exactly
        // this test and returns `p`; taking it here as well is what lets the plausibility read
        // below be reached only by pointers that can actually move.
        if !self.young.contains(p) {
            return p;
        }
        if !arena_obj_header_plausible(p, nclasses) {
            if log {
                let (cid, nf) = unsafe { (obj_class(p), obj_nfields(p)) };
                eprintln!("[STALEROOT] {src} obj={:#x} class_id={cid} nf={nf} — skipped (stale young root)", p as usize);
            }
            return p;
        }
        self.copy_obj(p)
    }

    /// The RAVA_WATCH_ADDR / RAVA_WATCH_CLASS hunts, lifted verbatim out of `copy_obj_guarded`.
    /// `#[cold]` + `#[inline(never)]`: it is entered only when one of the two env vars is set,
    /// and keeping its string formatting out of line is the point of the split.
    #[cold]
    #[inline(never)]
    fn watch_report(&self, p: *mut u8, nclasses: u32, src: &str) {
        let wa = watch_addr();
        if wa != 0 && p as u64 == wa {
            let (cid, nf) = unsafe { (obj_class(p), obj_nfields(p)) };
            let plaus = arena_obj_header_plausible(p, nclasses);
            eprintln!(
                "[WATCH] copy_obj_guarded src={src} addr={:#x} young={} plausible={plaus} class_id={cid} ({:?}) nf={nf} epoch={}",
                p as u64,
                self.young.contains(p),
                class_name(cid),
                GC_EPOCH.load(std::sync::atomic::Ordering::Relaxed)
            );
            // The Rust call path of THIS collection names the window that held the
            // watched value un-rooted (the trigger's caller is the suspect native /
            // callback). Diagnostic-only cost: fires per matching copy, never on the
            // mutator path.
            eprintln!("[WATCH-BT] {}", std::backtrace::Backtrace::force_capture());
        }
        if self.young.contains(p) && arena_obj_header_plausible(p, nclasses) {
            if let Some(want) = watch_class() {
                let cid = unsafe { obj_class(p) };
                let ep = GC_EPOCH.load(std::sync::atomic::Ordering::Relaxed);
                if ep >= 40 && class_name(cid).is_some_and(|c| c.contains(want)) {
                    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if n < 200 {
                        eprintln!(
                            "[WATCH-CLASS] copy src={src} addr={:#x} class={:?} epoch={ep} jit={:?} #{n}",
                            p as u64,
                            class_name(cid),
                            crate::jit::jit_method_cur(),
                        );
                        // First few: capture the collection's Rust call path — names the native /
                        // safepoint that ran THIS collection, i.e. the trigger whose caller holds
                        // the in-flight Ap2 SSA copy that stays stale (create's `new Ap2` bump
                        // safepoint? a dispatch resolve? arena_collect_jit?).
                        if n < 6 {
                            eprintln!("[WATCH-CLASS-STK] {:?}", crate::jit::jit_method_stack(8));
                        }
                    }
                }
            }
        }
    }
}

/// Is EITHER address watch armed? Both are `OnceLock`-cached env reads, but `copy_obj_guarded`
/// consulted them per evacuation edge; folding them into one bool leaves a single predictable
/// load in front of a cold call.
#[inline]
fn watch_any() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| watch_addr() != 0 || watch_class().is_some())
}

// ═══════════════ Phase 2: TLAB + class registry + arena field access ═══════════════
// `new_object` (under the flag) allocates via a thread-local pointer bump and
// returns Value::Obj(ArenaPtr). getfield/putfield dual-dispatch: an ArenaPtr derefs
// straight to its inline cells. Cheney evacuation is Phase 3 — here a full TLAB
// PANICS (RAVA_HARD_MB hard stop, host-safe), so Phase 2 only proves alloc +
// addressing correctness.

use crate::arena::ArenaPtr;
use std::cell::RefCell;
use std::sync::Arc;
