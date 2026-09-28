//! The collector proper: evacuate, remap, and re-root a compiled frame.

use crate::*;

/// `RAVA_TRACE_ARENA` — read ONCE, not once per collection. See `gc::gc_trap_enabled`:
/// `var_os` locks the process environment and walks the whole block, allocating, and
/// this sat on the main line of the JIT collector. Was: O(environment entries) + an
/// allocation per collection. Is: one `OnceLock` load.
#[cfg(feature = "next-gen-gc")]
fn trace_arena_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TRACE_ARENA").is_some())
}

impl Vm {
    /// TASK 2 — Cheney safepoint collection for the RECURSIVE / compiled path (no
    /// trampoline). Unlike `arena_safepoint_gc` (which owns the trampoline frame
    /// vector), this gathers arena roots from every live root source reachable
    /// while a COMPILED frame is suspended on the Rust stack — and crucially
    /// forwards the JIT cell buffers (`cells_all`) IN PLACE, so a compiled frame's
    /// spilled arena pointers survive evacuation. The compiled code then reloads
    /// them via `reload_after_gc`.
    ///
    /// Soundness: runs only under the GIL with every other thread parked, so the
    /// frame/cell reads+writes are synchronous and unaliased in practice (the same
    /// invariant `gc()` and `arena_safepoint_gc` already rely on). The JIT-buffer
    /// scan is conservative for the type-erased local slots (a slot is an arena
    /// root iff 8-aligned, non-null, and inside the live nursery) — a residual
    /// precision gap to close with stack maps; harmless false roots only float
    /// extra garbage to tenured, never corrupt.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn arena_collect_jit(&mut self) {
        // P3-S1: a GIL-free worker must NEVER collect (zero-cap view heap; a major here is a
        // process-aborting panic through extern "C"). Instead of unwinding — the shared fault
        // flag poisons EVERY worker's deep prologues and a result-dependent compiled loop then
        // spins on garbage 0s forever (the measured primitive #7, the G1 garbage-continuation
        // wall) — REFILL the worker's TLAB view from the region's spill pool: the alloc
        // callback retries into the fresh view and the kernel continues normally. Only a fully
        // exhausted pool aborts (fault-free: the abort is polled at the interp checkpoint /
        // freeze fast path / for-index boundary; residual machcode grind is bounded by the
        // worker's finite range).
        #[cfg(feature = "next-gen-gc")]
        if crate::parallel::gilfree() {
            let chunk = crate::parallel::spill_chunk();
            if let Some(base) = crate::parallel::spill_claim(chunk) {
                crate::nextgen::worker_tlab_view(base, chunk);
            } else {
                // NO SPACE AND NONE COMING. Raising the abort is necessary but NOT
                // sufficient: the compiled inline-alloc retry loop that called us polls
                // no flag, so returning here with the view still full is what produced
                // the measured 1.44-billion-callback spin. Latch the starvation so
                // `rava_arena_safepoint` can report it and the compiled overflow arm can
                // take its bounded exit.
                crate::parallel::request_abort_overflow();
                crate::parallel::note_spill_starved();
            }
            return;
        }
        // CLEAR the arena lambda-dedup cache before collecting. It holds raw arena addresses
        // and is not a root, so every entry is about to become either dead or relocated —
        // there is no `retain` predicate that could tell the two apart, and a surviving stale
        // address would hand back a DIFFERENT closure. Its heap twin can prune instead of
        // clear only because a handle is stable and `is_ref_alive` is exact.
        #[cfg(feature = "next-gen-gc")]
        self.arena_lambda_dedup.clear();
        // COLLECTOR ACCOUNTING (RAVA_GC_STATS): time the WHOLE safepoint — root gather,
        // collect, writeback — not just the copy. The root gather (frames, statics,
        // native-collection cells, JIT cell buffers) is paid per COLLECTION regardless
        // of how much survives, which is why collection COUNT dominates GC cost here.
        #[cfg(feature = "next-gen-gc")]
        let _gc_clock = crate::nextgen::gc_stats_enabled().then(std::time::Instant::now);
        // Push EVERY arena object root (young OR old). A MINOR GC no-ops on tenured
        // roots (`copy_obj` returns a non-nursery pointer unchanged); a MAJOR needs
        // the complete set to trace the live tenured graph + reclaim the rest.
        fn gather(v: &mut Value, locs: &mut Vec<*mut Value>, ptrs: &mut Vec<*mut u8>) {
            if let Value::Obj(p) = v {
                ptrs.push(p.as_ptr());
                locs.push(v as *mut Value);
            }
        }
        // ⭐**ONE `arena_should_major` FOR THE WHOLE COLLECTION.** It was called twice —
        // once to pick the heap-root scan's young-only window, once to pick the collector
        // — and each call is a TLS lookup plus a `RefCell` borrow. More importantly the
        // "they agree" invariant the shield's correctness rests on was an ARGUMENT about
        // two reads; hoisting makes it STRUCTURAL, one read used by both.
        let is_major = crate::nextgen::arena_should_major();
        // ⭐**THE ARENA BAND, HOISTED OUT OF THE CELL SCANS** (the twin of the minor path;
        // see `gc.rs` for the full argument). `arena_contains` — a TLS lookup, an
        // `is_worker`/`gilfree` probe, a `RefCell` borrow and two relaxed atomic loads
        // wrapped around two range compares — was asked once per i64 CELL of every
        // registered JIT cell buffer, every pinned-mirror ref cell, every cell of the
        // pinned region walk and every reference-array cell the shield hands back. Those
        // four loops are the collector's largest scans and their cells are overwhelmingly
        // ints, longs, float bits, tagged handles and 0 — all far below the 8 GiB arena
        // floor, so one wrapping subtract rejects them with NO TLS access.
        //
        // MEMBERSHIP IS UNCHANGED: every arena region (young, old, pinned, perm) is carved
        // from the one contiguous low band above `ARENA_FLOOR` and `NEXT_LOW` only grows,
        // so the band test can reject only what `arena_contains` also rejects; the young
        // window is an EXACT accept (it is `young.contains`'s own extent), and anything in
        // the band but outside the nursery still falls through to the original call.
        // Was: O(registered cells) TLS+RefCell round-trips per collection. Is: O(cells
        // that are actually arena pointers), plus two compares each.
        let fast = super::gc::fast_roots_enabled();
        let (ylo, yhi) = if fast {
            crate::nextgen::tlab_young_window()
        } else {
            (0, 0)
        };
        let band = crate::nextgen::arena_high_water()
            .saturating_sub(crate::nextgen::ARENA_FLOOR);
        let in_arena = |raw: u64| -> bool {
            if !fast {
                return crate::nextgen::arena_contains(raw as *mut u8);
            }
            if raw.wrapping_sub(crate::nextgen::ARENA_FLOOR) >= band {
                return false;
            }
            (raw >= ylo && raw < yhi) || crate::nextgen::arena_contains(raw as *mut u8)
        };
        let (mut locs, mut ptrs, mut cell_locs) = crate::frames::gc_gather_take();
        // Interpreter frames on every thread's Rust call stack (read+write via the
        // registry; GIL-synchronous). For a compiled hot loop these usually hold no
        // arena objects, but a mixed interp/JIT chain may — forward them too.
        for &p in &frames::all() {
            let f = unsafe { &mut *(p as *mut Frame) };
            for v in f.locals_mut().iter_mut() {
                gather(v, &mut locs, &mut ptrs);
            }
            for v in f.stack_mut().iter_mut() {
                gather(v, &mut locs, &mut ptrs);
            }
        }
        // Paused trampoline frames (if this path is reached under a stackful driver).
        // EVERY driver's paused trampoline frames — main's, a worker's, a green thread's.
        for ep in crate::frames::exec_all() {
            let frames = unsafe { &mut *(ep as *mut Vec<(Frame, Arc<str>, Arc<str>)>) };
            for (f, _, _) in frames.iter_mut() {
                for v in f.locals_mut().iter_mut() {
                    gather(v, &mut locs, &mut ptrs);
                }
                for v in f.stack_mut().iter_mut() {
                    gather(v, &mut locs, &mut ptrs);
                }
            }
        }
        for gt in self.green_threads.iter_mut() {
            for (f, _, _) in gt.frames.iter_mut() {
                for v in f.locals_mut().iter_mut() {
                    gather(v, &mut locs, &mut ptrs);
                }
                for v in f.stack_mut().iter_mut() {
                    gather(v, &mut locs, &mut ptrs);
                }
            }
        }
        crate::execution_state::visit_pending_roots(|value| {
            gather(value, &mut locs, &mut ptrs)
        });
        self.task_runtime
            .visit_roots(|value| gather(value, &mut locs, &mut ptrs));
        // GILFREE-RINGS deferred lane batch (see the twin site in the minor path).
        self.lane
            .visit_roots(|value| gather(value, &mut locs, &mut ptrs));
        self.loader
            .forward_statics(&mut |v| gather(v, &mut locs, &mut ptrs));
        // Explicit temp-root stack: an in-flight arena value a native call pinned
        // across an allocation (Wall #19 — e.g. the args jit_call passes to a native
        // that then allocates). gc_protect is forwarded by the non-moving HEAP GC but
        // the MOVING arena collector skipped it, so a pinned arena pointer was
        // reclaimed/left-stale out from under the native. Gather it as a Value root.
        // The Vm fallback writeback stays EXPLICIT (contract); registered worker
        // stacks are visited separately below through the registry.
        for v in self.gc_protect.iter_mut() {
            gather(v, &mut locs, &mut ptrs);
        }
        // Include registered worker roots in this moving writeback.
        unsafe {
            crate::vm::thread_context::with_registered_gc_protect_roots_mut(None, |roots| {
                for value in roots {
                    gather(value, &mut locs, &mut ptrs);
                }
            });
        }

        // In-flight native-call args (Wall #19): forward the arg slots a native is
        // holding across its own allocation so the native's `&[Value]` is relocated.
        crate::frames::native_args_gather(&mut |v| gather(v, &mut locs, &mut ptrs));
        // DANGLING SHIELD: arena objects reachable ONLY from native collections
        // (HashMap entries, reference-array cells, boxed object fields) are rooted by
        // NEITHER GC otherwise — the moving collector would reclaim them out from
        // under the collection, leaving a dangling pointer (garbage coordinates).
        // Root + forward them in place. INVARIANT: every `locs` push must precede
        // every `cell_locs` push (the writeback indexes `ptrs` as [locs.., cells..]),
        // so the Value slots are gathered HERE and the i64 cells AFTER the JIT cells.
        // MINOR (the common case) only moves nursery objects, so restrict the heap root
        // scan to young-pointing cells — tenured-pointing cells (the vast majority, e.g.
        // chunk block-state arrays → interned block states) don't move and don't need
        // forwarding. A MAJOR moves tenured too, so it must keep every arena cell.
        // `is_major` is the ONE `arena_should_major()` read hoisted at the top of this
        // function, so the heap-root window and the collector below cannot disagree.
        let coll_young_only = !is_major;
        let (coll_vlocs, coll_clocs) = self.heap.arena_root_locations(coll_young_only);
        for &vl in &coll_vlocs {
            gather(unsafe { &mut *vl }, &mut locs, &mut ptrs);
        }
        // LIVE ARENA-MAP in-flight temporaries (a computed callback value, a `prev`
        // being returned, a key/map held across a guest lambda or a resize). They
        // are Value-slot roots, so gather them HERE — before any `cell_locs` push —
        // to keep the writeback's `ptrs` order [all locs.., all cells..].
        crate::arena_map::forward_temps(&mut |v| gather(v, &mut locs, &mut ptrs));
        // PHASE-A STACK MAPS (`RAVA_STACKMAPS=1`, inert otherwise). Read-only census: compute
        // the roots Cranelift's user stack maps name for the live compiled frames and compare
        // them, per frame, against the cell-root ranges the loop below is about to act on.
        // Nothing here changes the root set — the ranges remain the only thing the collector
        // acts on in this phase.
        //
        // PHASE B RETIRED IT FOR THE FULL ARM: once `RAVA_STACKMAPS=1` deletes the shadow
        // frame, the ranges hold only the prologue's entry image, so comparing the two sides
        // compares nothing. The cross-check therefore runs only in the ATTRIBUTION arm
        // (`RAVA_STACKMAPS=sp`), where the shadow frame is still the rooting protocol.
        #[cfg(feature = "next-gen-gc")]
        if crate::jit::runtime::stackmaps::recording_sp()
            && !crate::jit::runtime::stackmaps::declaring()
        {
            let with_sp = frames::cells_all_sp();
            crate::jit::runtime::stackmaps::cross_check(&with_sp);
        }
        // JIT cell buffers — forwarded IN PLACE.
        // cell_locs is the reused buffer taken with locs/ptrs above (gc_gather_take).
        for (ptr, len) in frames::cells_all() {
            let cells = unsafe { std::slice::from_raw_parts_mut(ptr as *mut i64, len) };
            for c in cells.iter_mut() {
                let raw = *c as u64;
                if *c != 0 && raw & 7 == 0 && in_arena(raw) {
                    if let Some(ap) = crate::arena::ArenaPtr::new(raw as *mut u8) {
                        ptrs.push(ap.as_ptr());
                        cell_locs.push(c as *mut i64);
                    }
                }
            }
        }
        // PHASE-B STACK MAPS (`RAVA_STACKMAPS=1`, inert otherwise) — THE ROOTING PROTOCOL.
        // With the flag on, compiled code emits no `spill_for_gc` and no reload: a live
        // reference in a compiled frame is named by Cranelift's precise stack map instead, and
        // the words the map names are on the compiled frame's own stack. They MOVE like any
        // other root, so they are pushed into `cell_locs` and forwarded IN PLACE by the same
        // writeback that rewrites a cell-root range — `SafepointSpiller` reloads each declared
        // value from that exact slot after the call, so the forwarded word is what the compiled
        // code goes on to use. Shape test identical to the range loop above: only an 8-aligned
        // in-arena word is a movable pointer; a tagged handle/array/box cell does not move.
        #[cfg(feature = "next-gen-gc")]
        if crate::jit::runtime::stackmaps::declaring() {
            crate::jit::runtime::stackmaps::for_each_map_slot(&mut |c| {
                let raw = unsafe { *c } as u64;
                let mut took = false;
                if raw != 0 && raw & 7 == 0 && in_arena(raw) {
                    if let Some(ap) = crate::arena::ArenaPtr::new(raw as *mut u8) {
                        ptrs.push(ap.as_ptr());
                        cell_locs.push(c);
                        took = true;
                    }
                }
                crate::jit::runtime::stackmaps::note_slot(took);
            });
        }
        // B3-collector (flag-gated): pinned-mirror REF cells holding a raw arena
        // pointer are forwarded in place, exactly like the JIT cell buffers — else
        // a moved arena object leaves the mirror at its stale pre-move address.
        // Precise (class ref_mask slots only) so a primitive cell can never be
        // falsely rewritten. Tagged cells (0b01/0b10/0b11 handle/array/box) skip.
        #[cfg(feature = "next-gen-gc")]
        if crate::nextgen::pinned_mirror_enabled() {
            crate::nextgen::pinned_mirror_ref_cells(|c| {
                let raw = unsafe { *c } as u64;
                if raw != 0 && raw & 7 == 0 && in_arena(raw) {
                    if let Some(ap) = crate::arena::ArenaPtr::new(raw as *mut u8) {
                        ptrs.push(ap.as_ptr());
                        cell_locs.push(c);
                    }
                }
            });
        }
        // BORN-PINNED root walk (RAVA_PINNED_ENUM): a born-pinned object (an enum
        // constant) is immortal + never moves, but its ref cells ($enumName, declared
        // ref fields) can hold a raw pointer to a MOVING-arena object. Linearly sweep
        // the contiguous pinned region and forward those cells IN PLACE — exactly like
        // the mirror block above and the JIT cell buffers — so an arena object reachable
        // ONLY through a pinned object stays live + is relocated (the use-after-free the
        // GATE checks for). Precise (class ref_mask slots only); tagged cells
        // (handle/array/box) and non-moving pointers (perm arena strings, other pinned
        // objects) skip via the `raw & 7 == 0 && arena_contains` filter. The region walk
        // is a superset of `pinned_mirror_ref_cells`, so if BOTH flags were ever on a
        // mirror cell would be pushed twice — harmless: `copy_obj` forwarding is
        // idempotent (second forward returns the same evacuated address).
        #[cfg(feature = "next-gen-gc")]
        if crate::nextgen::pinned_enum_enabled() {
            crate::nextgen::pinned_region_root_walk(|c| {
                let raw = unsafe { *c } as u64;
                if raw != 0 && raw & 7 == 0 && in_arena(raw) {
                    if let Some(ap) = crate::arena::ArenaPtr::new(raw as *mut u8) {
                        ptrs.push(ap.as_ptr());
                        cell_locs.push(c);
                    }
                }
            });
        }
        // Reference-array cells holding a raw in-nursery arena pointer (the second
        // half of the DANGLING SHIELD — appended to `cell_locs` AFTER the JIT cells
        // so the `ptrs` order stays [all locs.., all cells..]).
        for &cl in &coll_clocs {
            let raw = unsafe { *cl } as u64;
            if raw != 0 && raw & 7 == 0 && in_arena(raw) {
                if let Some(ap) = crate::arena::ArenaPtr::new(raw as *mut u8) {
                    ptrs.push(ap.as_ptr());
                    cell_locs.push(cl);
                }
            }
        }
        // MAJOR when tenured is under pressure (no minor reclaims tenured → the leak),
        // else a fast MINOR. Both forward `ptrs` in place; the writeback is identical.
        // `is_major` is the single hoisted read taken before the root gather.
        let old_before = crate::nextgen::arena_old_bytes();
        let (evac, tenured) = if is_major {
            let reclaimed = crate::nextgen::major_collect(&mut ptrs) as u64;
            (reclaimed, crate::nextgen::arena_old_bytes())
        } else {
            crate::nextgen::tlab_collect(&mut ptrs)
        };
        // GC ACCOUNTING (RAVA_TRACE_ARENA): per-collection kind + tenured before/after
        // + bytes reclaimed, so a monotonic-growth leak vs a healthy major-collect
        // plateau is visible deterministically. Cheap (one env probe) when off.
        if trace_arena_enabled() {
            use std::io::Write as _;
            let old_after = crate::nextgen::arena_old_bytes();
            // Also report the RUST-side tables: forcing MAJORs at 400MB moved peak RSS by 0MB,
            // so the 2200MB (vs HotSpot's 544MB) is NOT arena — print where it really is.
            let (nobj, narr, rbytes) = self.heap.rust_side_footprint();
            eprintln!(
                "[ARENA-GC] {} old_before={}KB old_after={}KB cap={}KB reclaimed={}KB roots={}obj+{}cells | RUST-SIDE objects={} arrays={} est={}MB",
                if is_major { "MAJOR" } else { "minor" },
                old_before / 1024,
                old_after / 1024,
                crate::nextgen::arena_old_cap() / 1024,
                old_before.saturating_sub(old_after) / 1024,
                locs.len(),
                cell_locs.len(),
                nobj,
                narr,
                rbytes / (1024 * 1024),
            );
            // ADAPTIVE MAJOR POLICY (`RAVA_NO_FAST_GCPAUSE=1` opts out): the decision that
            // picked `is_major` above, printed rather than inferred. `swept` is the tenured
            // bytes the remembered-set walk has linearly re-read since the last compaction —
            // the bill a major is amortised against — `live` the last major's surviving set,
            // and `backoff` the multiplier the policy raised on itself when a major reclaimed
            // little. A boot in which `majors` stays 0 while `swept` climbs into the tens of
            // GB is the pathology this policy exists to name.
            {
                let (swept, live, backoff, majors) = crate::nextgen::major_policy_snapshot();
                eprintln!(
                    "[GC-MAJOR-POLICY] swept_since_major={}MB last_live={}MB backoff={backoff}x majors={majors}",
                    swept / (1024 * 1024),
                    live / (1024 * 1024),
                );
            }
            // Name the classes actually holding the live bytes. Without this, shrinking the
            // Rust heap is guesswork — and every guess this session (putfield, toString keys,
            // GC scheduling) was refuted by measurement.
            let (live, dead) = self.heap.live_dead_counts();
            eprintln!(
                "[LIVE-DEAD] live={live} dead_slots={dead} (nullable dead slots retain handles but release Object storage)"
            );
            for (cls, n, bytes) in self.heap.live_class_histogram(12) {
                eprintln!("[LIVE-CLS] {:>9} objs {:>6}KB  {cls}", n, bytes / 1024);
            }
            let _ = std::io::stderr().flush();
        }
        // COMPONENT CENSUS (RAVA_CENSUS=1, independent of RAVA_TRACE_ARENA). Runs at every
        // arena collection and keeps the PEAK-by-total snapshot: a server boot has no clean
        // exit to print from (same constraint as [INLBAIL]), and the number that matters is
        // the peak, not the last. Re-printed only when a new peak is set, so the log stays
        // readable across a 250s boot instead of one dump per collection.
        // COVERAGE CENSUS (RAVA_CELL_CENSUS=1): how far is the live graph from the
        // 100%-0b00 the bare-deref flip needs, and which classes own the residue?
        #[cfg(feature = "next-gen-gc")]
        self.gc_cell_census();
        if census_enabled() {
            let c = self.heap.census();
            let (abort_raised, ovf_raised, teardown, teardown_ovf) = crate::parallel::abort_stats();
            let mut peak = CENSUS_PEAK.lock().unwrap_or_else(|e| e.into_inner());
            if c.total > peak.0 {
                use std::io::Write as _;
                peak.0 = c.total;
                peak.1 = Some(c.clone());
                eprint!("{}", c.render());
                eprintln!(
                    "[CENSUS] fork: abort_raised={abort_raised} overflow_raised={ovf_raised} regions_torn_down={teardown} (of which overflow={teardown_ovf}) worker_dispatch_miss={}",
                    crate::gil::worker_miss_count()
                );
                // RSS reconciliation: census (Rust-live components) vs the domains OUTSIDE it.
                // rss = malloc_live + mimalloc overhead + arena committed + JIT pages + stacks.
                // `unacc` (rss − malloc_live − arena) ≈ mimalloc overhead + stacks + image — the
                // first-ever printed decomposition of the census↔RSS gap.
                {
                    let (ay, ao, ap, aperm) = crate::nextgen::arena_committed_bytes();
                    let arena = ay + ao + ap + aperm;
                    let mlive = crate::mem_meter::live();
                    let rss = crate::mem_meter::os_rss();
                    let mb = |b: usize| b / (1024 * 1024);
                    eprintln!(
                        "[CENSUS] rss-recon: os_rss={}MB malloc_live={}MB arena_committed={}MB (young={} old={} pooled={} perm={}) unaccounted={}MB",
                        mb(rss), mb(mlive), mb(arena), mb(ay), mb(ao), mb(ap), mb(aperm),
                        mb(rss.saturating_sub(mlive).saturating_sub(arena)),
                    );
                }
                if crate::gil::gilstat_enabled() {
                    let (acq, cont, blocked_ns, max_ns, threads) = crate::gil::gil_stats();
                    eprintln!(
                        "[CENSUS] gil: acquires={acq} contended={cont} ({:.2}%) blocked_total={:.1}ms max_block={:.3}ms real_threads={threads}",
                        if acq == 0 { 0.0 } else { cont as f64 * 100.0 / acq as f64 },
                        blocked_ns as f64 / 1.0e6,
                        max_ns as f64 / 1.0e6,
                    );
                }
                let _ = std::io::stderr().flush();
            }
        }
        let nloc = locs.len();
        for (i, loc) in locs.iter().enumerate() {
            unsafe {
                *(*loc) = Value::Obj(
                    crate::arena::ArenaPtr::new(ptrs[i]).expect("forwarded ptr non-null"),
                );
            }
        }
        for (j, cl) in cell_locs.iter().enumerate() {
            unsafe {
                **cl = ptrs[nloc + j] as i64;
            }
        }
        #[cfg(feature = "next-gen-gc")]
        super::gc_root_validation::validate_writeback(self, &coll_vlocs, &coll_clocs, &cell_locs);
        self.gc_survivor_trap("arena_collect_jit");
        // PHASE-B VERIFIER (`RAVA_STACKMAPS_VERIFY=1`): the writeback is done, so every slot the
        // maps named must now hold a live-region pointer. See `stackmaps::verify_after_collect`.
        #[cfg(feature = "next-gen-gc")]
        crate::jit::runtime::stackmaps::verify_after_collect("arena_collect_jit");
        if self.trace.gt {
            eprintln!(
                "[ARENA-GC-JIT] safepoint: {evac} evacuated, {} frame + {} cell roots forwarded, tenured={}KB",
                nloc,
                cell_locs.len(),
                tenured / 1024
            );
        }
        // Invalidate the getstatic value cache (interp tlcache + JIT static cells):
        // the collection forwarded static `Value::Obj` cells in place, so a cached
        // getstatic would otherwise serve the stale pre-GC pointer. A moved singleton
        // (e.g. a block-state Property) would then read back at its old address while
        // the map holding it was forwarded to the new one → identity `==` fails
        // (StateHolder's "Cannot set property … does not exist").
        self.bump_static_gen();
        crate::frames::gc_gather_put(locs, ptrs, cell_locs);
        // COLLECTOR ACCOUNTING: close the clock, then report the cumulative line every
        // 32nd collection. A server boot has no clean exit to print from (the same
        // constraint [CENSUS]/[INLBAIL] work around), so the log carries the trajectory.
        #[cfg(feature = "next-gen-gc")]
        if let Some(t0) = _gc_clock {
            use std::io::Write as _;
            crate::nextgen::gc_stats_note_nanos(t0.elapsed().as_nanos() as u64);
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if n % 32 == 0 {
                eprintln!("{}", crate::nextgen::gc_stats_line());
                for (cls, objs, bytes) in crate::nextgen::tenured_class_histogram(10) {
                    eprintln!("[GC-STATS]   tenured {objs:>9} objs {:>7}KB  {cls}", bytes / 1024);
                }
                let _ = std::io::stderr().flush();
            }
        }
    }
}
