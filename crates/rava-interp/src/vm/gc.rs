//! Safepoints and collection entry points.

use crate::*;

/// `RAVA_NO_FAST_ROOTS=1` restores the previous shape of the root scan EXACTLY: the
/// per-root `tlab_in_nursery` / per-cell `arena_contains` calls, and an unfiltered,
/// freshly-allocated mark-sweep root vector. Folded into `RAVA_NO_EMIT_MORE` so the
/// whole block bisects in one run, per the house convention (`fast_gc_enabled`).
///
/// Reachability is IDENTICAL on both arms — every change under this switch is a
/// cheaper way to compute the same membership question, never a different one.
pub(crate) fn fast_roots_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        crate::flags::round7() && std::env::var_os("RAVA_NO_FAST_ROOTS").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// `RAVA_GC_TRAP` — read ONCE, not once per collection. `var_os` is not a flag test: it
/// takes the process environment lock and walks the whole environment block comparing
/// names, allocating as it goes, and this was on the entry of a function called at EVERY
/// collection on both collector paths. Was: O(environment entries) + an allocation per
/// collection. Is: one relaxed load of a `OnceLock`. (The rest of the engine's gates are
/// already `OnceLock`s — `fwd_log_enabled`, `census_enabled`, `pool_census`; these two
/// were the outliers.)
#[cfg(feature = "next-gen-gc")]
pub(crate) fn gc_trap_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_GC_TRAP").is_some())
}

thread_local! {
    /// REUSED mark-sweep root buffer. `Vm::gc` built a fresh `Vec<Value>` per collection
    /// and let it double from zero — log2(roots) reallocs plus a memcpy of everything
    /// gathered so far, on every collection. Kept between collections instead (the same
    /// discipline as `frames::gc_gather_take` and the heap's retained `MarkBits`).
    ///
    /// TABLE SIZE: `Value` is 24 bytes and this holds only the ref-carrying slots, so a
    /// product boot's retained capacity is tens of thousands of entries — well under
    /// 1 MB, and it replaces a buffer that was live across the whole pass anyway (peak
    /// RSS is where it already sat). Cleared on return so no `Value::Str` refcount and
    /// no stale handle is held across the gap between collections.
    static MARK_ROOTS: std::cell::RefCell<Vec<Value>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Take the reusable mark-sweep root buffer (cleared, capacity retained). `mem::take`
/// leaves an empty `Vec` behind, so a re-entrant collection cannot alias this one.
fn mark_roots_take() -> Vec<Value> {
    MARK_ROOTS.with(|c| {
        let mut slot = c.borrow_mut();
        let mut v = std::mem::take(&mut *slot);
        v.clear();
        v
    })
}

/// Return the root buffer, cleared, so its capacity is reused next collection.
fn mark_roots_put(mut v: Vec<Value>) {
    v.clear();
    MARK_ROOTS.with(|c| *c.borrow_mut() = v);
}
/// Phase 2 tracer: thread-local roots via `ThreadContext`.
pub(crate) fn visit_roots(
    vm: &crate::vm::thread_context::SharedVm,
    ctx: Option<&crate::vm::thread_context::ThreadContext<'_>>,
    visitor: &mut impl FnMut(Value),
) {
    if let Some(c) = ctx {
        for v in &c.gc_protect {
            visitor(v.clone());
        }
        for gt in &c.green_threads {
            for (f, _, _) in &gt.frames {
                for v in f.locals().iter().chain(f.stack().iter()) {
                    visitor(v.clone());
                }
            }
        }
    } else {
        // The Vm fallback scan stays EXPLICIT here (contract: the coordinator's own
        // root stack is walked directly; registered worker roots are scanned separately
        // below through the registry, not re-routed through the facade).
        for v in vm.gc_protect.iter() {
            visitor(v.clone());
        }
        for gt in &vm.green_threads {
            for (f, _, _) in &gt.frames {
                for v in f.locals().iter().chain(f.stack().iter()) {
                    visitor(v.clone());
                }
            }
        }
    }
    // Registered worker contexts are distinct from the coordinator's Vm roots.
    // This is read-only marking; moving collectors also write these roots back.
    unsafe {
        crate::vm::thread_context::with_registered_gc_protect_roots_mut(None, |roots| {
            for value in roots.iter() {
                visitor(value.clone());
            }
        });
    }

    // Shared roots — always from Vm.
    for &h in vm.class_objects.values() {
        visitor(Value::Ref(h));
    }
    for &h in vm.indy_singletons.values() {
        visitor(Value::Ref(h));
    }
    for &h in vm.bool_cache.iter().flatten() {
        visitor(Value::Ref(h));
    }
    for h in vm.box_cache.handles() {
        visitor(Value::Ref(h));
    }
    if let Some(r) = crate::optlite::root() {
        visitor(Value::Ref(r));
    }
    if let Some(h) = vm.main_thread {
        visitor(Value::Ref(h));
    }
}

/// Can the mark loop FOLLOW this value? `Heap::gc`'s `grey!(val)` matches exactly
/// `Ref | Array | Obj` and sends every other variant to `_ => {}`. A bare `Value::Int`
/// only ever denotes a map id in an object's `$map` FIELD — never in the root set — so
/// dropping the non-ref variants here is bit-for-bit the same reachable set.
#[inline(always)]
fn mark_reachable_from(v: &Value) -> bool {
    matches!(v, Value::Ref(_) | Value::Array(_) | Value::Obj(_))
}

impl Vm {
    /// SAFEPOINT arena collection (next-gen-gc). Called from a driver loop where
    /// the whole `frames` Vec is owned and NO frame is op-borrowed — so forwarding
    /// pointers inside frames is sound (no aliasing). Gathers every young
    /// `Value::Obj` root (frame locals/stacks + parked green threads + statics),
    /// evacuates the nursery via Cheney (`tlab_collect`, which also forwards
    /// arena→arena refs + the remembered set), and writes each forwarded pointer
    /// back to its root location.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn arena_safepoint_gc(&mut self, frames: &mut Vec<(Frame, Arc<str>, Arc<str>)>) {
        // P3-S1: a GIL-free worker must NEVER collect — its GenHeap is a zero-cap VIEW
        // (old=Region::view(null,0)); a major here is an instant "tenured exhausted"
        // process abort (nextgen.rs panic → cannot-unwind through the extern "C"
        // callback frames). Raise the overflow abort instead; the region tears down to
        // the bit-exact serial retry and every worker result is discarded.
        #[cfg(feature = "next-gen-gc")]
        if crate::parallel::gilfree() {
            crate::parallel::request_abort_overflow();
            return;
        }
        let fast = fast_roots_enabled();
        // ⭐**THE NURSERY WINDOW, HOISTED OUT OF THE ROOT SCAN.** `tlab_in_nursery` was
        // asked ONCE PER `Value::Obj` ROOT — every frame local, every operand-stack slot,
        // every static, every lane/task/native-arg capture — and each call is a TLS block
        // lookup, an `is_worker`/`gilfree` probe, a `RefCell` try-borrow (a flag write and
        // a write-back) and two relaxed atomic loads WRAPPED AROUND the two range compares
        // that are the actual question. The nursery extent changes only when a region is
        // (re)mapped, which happens inside `tlab_collect` BELOW this scan and nowhere
        // within it, so one snapshot taken here is bit-for-bit the value every per-root
        // call would have read — the same argument `arena_root_locations` already makes
        // for hoisting `NEXT_LOW`. Was: O(arena roots) TLS+RefCell round-trips per
        // collection. Is: one, plus two compares per root.
        let (ylo, yhi) = if fast {
            crate::nextgen::tlab_young_window()
        } else {
            (0, 0)
        };
        let gather = |v: &mut Value, locs: &mut Vec<*mut Value>, ptrs: &mut Vec<*mut u8>| {
            if let Value::Obj(p) = v {
                let young = if fast {
                    let a = p.addr();
                    a >= ylo && a < yhi
                } else {
                    crate::nextgen::tlab_in_nursery(*p)
                };
                if young {
                    ptrs.push(p.as_ptr());
                    locs.push(v as *mut Value);
                }
            }
        };
        // ⭐**THE ARENA BAND, HOISTED OUT OF THE CELL SCANS.** Same lesson, larger
        // population: `arena_contains` is a TLS lookup + `RefCell` borrow + two region
        // compares, and it was asked once per i64 CELL of every registered JIT buffer and
        // once per reference-array cell the dangling shield hands back — the biggest scans
        // the collector runs. Nearly every such cell holds an int, a long, a float's bits,
        // a tagged handle or 0, all far below the 8 GiB arena floor, so ONE wrapping
        // subtract rejects them without touching TLS at all. The young half is then an
        // EXACT accept (`tlab_young_window` is literally `young.contains`'s extent), and
        // only a cell inside the band but outside the nursery still pays the original call
        // to decide tenured-vs-perm.
        //
        // MEMBERSHIP IS UNCHANGED, and that is the whole safety argument: every arena
        // region — young, old, pinned, perm — is carved from the one contiguous low band
        // starting at `ARENA_FLOOR`, and `NEXT_LOW` only ever GROWS, so an address
        // `arena_contains` accepts was below the high-water mark when its region was
        // mapped and is therefore below any later reading of it. The prefilter can only
        // reject what `arena_contains` would also reject.
        let hw = crate::nextgen::arena_high_water();
        let band = hw.saturating_sub(crate::nextgen::ARENA_FLOOR);
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
        for (f, _, _) in frames.iter_mut() {
            for v in f.locals_mut().iter_mut() {
                gather(v, &mut locs, &mut ptrs);
            }
            for v in f.stack_mut().iter_mut() {
                gather(v, &mut locs, &mut ptrs);
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
        // EVERY registered Frame on this thread's Rust call stack — the per-thread
        // `frames::all()` registry, NOT just the trampoline `frames` arg. A SYNTHETIC
        // dispatch frame (`jit_call` / `jit_indy` push one holding the unmarshalled
        // call args while running invoke* / makeConcat) lives ONLY here. `arena_collect_jit`
        // already scans it; the safepoint path must too, or an arena arg (e.g. the
        // `g` passed to `String.valueOf` → `Sum.toString`) goes stale across a moving
        // safepoint the nested guest `toString()` triggers → `arena getfield ... class
        // None`. Frames double-covered by the `frames` arg above re-forward idempotently
        // (a moved pointer's slot already holds the tenured address — copy_obj no-ops).
        for &fp in &frames::all() {
            let f = unsafe { &mut *(fp as *mut Frame) };
            for v in f.locals_mut().iter_mut() {
                gather(v, &mut locs, &mut ptrs);
            }
            for v in f.stack_mut().iter_mut() {
                gather(v, &mut locs, &mut ptrs);
            }
        }
        crate::execution_state::visit_pending_roots(|value| {
            gather(value, &mut locs, &mut ptrs)
        });
        self.task_runtime
            .visit_roots(|value| gather(value, &mut locs, &mut ptrs));
        // GILFREE-RINGS: suppliers/futures deferred to the off-GIL lane. Any number
        // of collections can fall between `defer` and the park, and a moved capture
        // would be packed into a worker frame as a stale pointer.
        self.lane
            .visit_roots(|value| gather(value, &mut locs, &mut ptrs));
        self.loader
            .forward_statics(&mut |v| gather(v, &mut locs, &mut ptrs));
        // Explicit temp-root stack (see arena_collect_jit): pinned in-flight arena
        // values a native call holds across an allocation (Wall #19). The Vm fallback
        // writeback stays EXPLICIT (contract); registered worker stacks are visited
        // separately below through the registry.
        for v in self.gc_protect.iter_mut() {
            gather(v, &mut locs, &mut ptrs);
        }
        // Collection runs only after registered worker owners have stopped;
        // forward their stable boxed roots in place.
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
        // CRITICAL — the same non-frame roots `arena_collect_jit` forwards. Without
        // these a MOVING safepoint that fires while a COMPILED frame is on the stack
        // leaves its spilled locals (JIT cells), a native-held arena temp
        // (forward_temps), or an arena collection element (dangling shield) DANGLING:
        // a stale young receiver reused as fresh bytes → `arena getfield ... class
        // None` (e.g. compiled `Sum.toString` reloads `this` from a cell this pass
        // never forwarded). INVARIANT: every Value-slot (`locs`) push precedes every
        // i64-cell (`cell_locs`) push so the writeback indexes `ptrs` as [locs.., cells..].
        // arena_safepoint_gc only ever runs a MINOR (tlab_collect) — so the heap root
        // scan can restrict to nursery-pointing cells (tenured cells don't move).
        let (coll_vlocs, coll_clocs) = self.heap.arena_root_locations(true);
        for &vl in &coll_vlocs {
            gather(unsafe { &mut *vl }, &mut locs, &mut ptrs);
        }
        crate::arena_map::forward_temps(&mut |v| gather(v, &mut locs, &mut ptrs));
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
        for &cl in &coll_clocs {
            let raw = unsafe { *cl } as u64;
            if raw != 0 && raw & 7 == 0 && in_arena(raw) {
                if let Some(ap) = crate::arena::ArenaPtr::new(raw as *mut u8) {
                    ptrs.push(ap.as_ptr());
                    cell_locs.push(cl);
                }
            }
        }

        let nloc = locs.len();
        let (evac, tenured) = crate::nextgen::tlab_collect(&mut ptrs);
        // Write each forwarded pointer back to its root location (Value slots first,
        // then the i64 cells — matching the `ptrs` order [locs.., cells..]).
        for (i, loc) in locs.iter().enumerate() {
            unsafe {
                *(*loc) = Value::Obj(crate::arena::ArenaPtr::new(ptrs[i]).expect("forwarded ptr non-null"));
            }
        }
        for (j, cl) in cell_locs.iter().enumerate() {
            unsafe {
                **cl = ptrs[nloc + j] as i64;
            }
        }
        self.gc_survivor_trap("arena_safepoint_gc");
        #[cfg(feature = "next-gen-gc")]
        crate::jit::runtime::stackmaps::verify_after_collect("arena_safepoint_gc");
        if self.trace.gt {
            eprintln!(
                "[ARENA-GC] safepoint: {evac} evacuated, {} roots forwarded, tenured={}KB",
                locs.len(),
                tenured / 1024
            );
        }
        // Invalidate the getstatic value cache: the collection forwarded static
        // `Value::Obj` cells in place, so a cached getstatic would serve the stale
        // pre-GC pointer (a moved block-state Property → StateHolder "Cannot set
        // property … does not exist"). See the matching bump in `arena_collect_jit`.
        self.bump_static_gen();
        crate::frames::gc_gather_put(locs, ptrs, cell_locs);
    }

    /// SURVIVOR TRAP (RAVA_GC_TRAP): immediately after a collection's writeback, scan
    /// every registered JIT cell buffer. `young.top` is now 0 (every live young object
    /// was evacuated), so ANY cell still holding a raw young-range pointer is an
    /// UN-FORWARDED survivor — the collector that just ran missed that cell. Panic with
    /// the collector name + buffer dump so the leak is attributed precisely. Cheap when
    /// off (one env probe); bounded buffer dump when it fires.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn gc_survivor_trap(&self, collector: &str) {
        if !gc_trap_enabled() {
            return;
        }
        let (ylo, yhi) = crate::nextgen::young_bounds();
        if ylo == 0 && yhi == 0 {
            return;
        }
        for (ptr, len) in frames::cells_all() {
            let cells = unsafe { std::slice::from_raw_parts(ptr, len) };
            for (i, &c) in cells.iter().enumerate() {
                let raw = c as u64 as usize;
                if c != 0 && (raw & 7) == 0 && raw >= ylo && raw < yhi {
                    let dump: Vec<String> =
                        cells.iter().map(|&x| format!("{:#x}", x as u64)).collect();
                    panic!(
                        "[GC-SURVIVOR-TRAP] collector={collector} un-forwarded young ptr \
                         {raw:#x} in cell-buffer {:#x} (len={len}) idx={i} young=[{ylo:#x},{yhi:#x})\nbuf={dump:?}",
                        ptr as usize
                    );
                }
            }
        }
        // ALSO scan EVERY cell of every arena object RAW (NOT ref_mask-filtered): if
        // the Cheney/remembered-set scan skipped a cell because the class `ref_mask`
        // omits it (a kind-misclassified ref field), that cell still points into the
        // now-evacuated nursery — and a `ref_mask`-based scan (like arena_trace_refs)
        // would be BLIND to exactly that gap. Flag any 8-aligned young-range cell whose
        // target has a plausible arena header, and report whether ref_mask covered the
        // slot: `ref_masked=false` ⇒ the ref_mask gap that strands `Sum.g`. young.top
        // is 0 post-collection, so only `old` is walked.
        let nclasses = crate::nextgen::registry_len();
        crate::nextgen::arena_for_each_obj(&mut |p| {
            let cid = unsafe { crate::nextgen::obj_class(p) };
            let nf = unsafe { crate::nextgen::obj_nfields(p) } as usize;
            let (mask, uniform) = crate::nextgen::ref_mask_and_uniform(cid);
            for i in 0..nf {
                let c = unsafe { crate::nextgen::cell(p, i) };
                let raw = c as u64 as usize;
                // BLINDFOLD OFF: flag ANY 8-aligned young-range cell, regardless of
                // whether a plausible header (or DEADBEEF poison) sits at the target.
                // The plausibility gate hid the very gap we hunt — a stranded edge
                // whose child slot was already poisoned/reused has NO valid header.
                // Report `header_ok` + `ref_masked` so the dump distinguishes a
                // ref_mask gap (masked=false) from a scan-order miss (masked=true).
                if c != 0 && (raw & 7) == 0 && raw >= ylo && raw < yhi {
                    let masked = uniform || crate::nextgen::ref_bit(&mask, i);
                    let header_ok =
                        crate::nextgen::arena_obj_header_plausible(raw as *mut u8, nclasses);
                    let cls = crate::nextgen::class_name(cid)
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    let (fid, fkind) = crate::nextgen::field_id_at(cid, i).unwrap_or((0, 0));
                    panic!(
                        "[GC-SURVIVOR-TRAP] collector={collector} un-forwarded young ARENA-CELL \
                         child {raw:#x} in holder={cls} ptr={:#x} slot={i} field_id={fid} \
                         field_kind={fkind} ref_masked={masked} header_ok={header_ok} \
                         young=[{ylo:#x},{yhi:#x})",
                        p as usize
                    );
                }
            }
        });
    }

    /// Collect GC roots and run a mark-sweep over the heap. Roots are class
    /// statics plus every live frame's locals + operand stack (read through the
    /// `frame_ptrs` registry). Only called with `native_depth == 0`, so no
    /// native holds unreachable `Value`s in Rust locals. Returns bytes freed.
    /// Threshold-gated Rust-heap mark-sweep. Returns bytes freed (0 if it did not run).
    ///
    /// The sweep's ONLY trigger used to be the interpreter's op-counter safepoint
    /// (`opcodes.rs`, every CHECK_INTERVAL ops). A JIT-heavy workload barely advances that
    /// counter — a Minecraft boot runs 79% in compiled code and only 4% in the interpreter —
    /// so the sweep almost never fired and heap objects accumulated without bound: 3.64M live
    /// `Box<Object>` at peak, 2200MB RSS versus HotSpot's 544MB on the identical boot. (The
    /// arena is NOT the problem: tenured peaks at ~108MB of a 1792MB cap.) Hoisted out of the
    /// interpreter so the JIT safepoint can poll the SAME thresholds through the SAME code —
    /// two divergent copies of a GC trigger is how the next silent leak gets written.
    ///
    /// Callers must be at a real safepoint: `native_depth() == 0` (no re-entrant native
    /// holding roots in Rust locals) and every compiled frame's refs spilled into
    /// `Frame.locals`, which is the convention `arena_collect_jit` already relies on — `gc()`
    /// roots from `frames::all()`, so an unspilled ref would be freed while live, and
    /// `Heap::gc` RECYCLES handles into `free_obj`, making that a use-after-free rather than
    /// a mere leak.
    pub(crate) fn maybe_gc(&mut self) -> usize {
        let est = self.heap.approx_bytes().saturating_mul(2);
        let real = crate::mem_meter::live();
        let (gc_next, gc_next_real) = crate::vm::thread_context::gc_thresholds(self);
        let pressure_exceeded = est > gc_next || real > gc_next_real;
        if !(pressure_exceeded && crate::frames::native_depth() == 0) {
            return 0;
        }
        let freed = self.gc();
        let est_post = self.heap.approx_bytes().saturating_mul(2);
        let real_post = crate::mem_meter::live();
        // One authoritative rearm policy for both logical and allocator-live pressure.
        // The measured default is ~1.5x post-GC; RAVA_GC_REARM_TIGHT=0 restores
        // the historical ~2x policy for bisection. This changes collection cadence
        // only, never reachability semantics.
        let gc_next = crate::gc_policy::rearm_target(est_post);
        let gc_next_real = crate::gc_policy::rearm_target(real_post);
        crate::vm::thread_context::set_gc_thresholds(self, gc_next, gc_next_real);
        if crate::gc_policy::trace_enabled() {
            eprintln!(
                "[GC-REARM] tight={} freed={}MB post_est={}MB next_est={}MB post_real={}MB next_real={}MB",
                crate::gc_policy::tight_enabled(),
                freed / (1024 * 1024),
                est_post / (1024 * 1024),
                gc_next / (1024 * 1024),
                real_post / (1024 * 1024),
                gc_next_real / (1024 * 1024),
            );
        }
        freed
    }

    /// GC entry with ThreadContext: thread-local roots come from `ctx`.
    pub(crate) fn gc_with_ctx(
        &mut self,
        ctx: Option<&crate::vm::thread_context::ThreadContext<'_>>,
    ) -> usize {
        self.gc_impl(ctx)
    }

    pub(crate) fn gc(&mut self) -> usize {
        self.gc_impl(None)
    }

    fn gc_impl(
        &mut self,
        ctx: Option<&crate::vm::thread_context::ThreadContext<'_>>,
    ) -> usize {
        crate::vm::thread_context::sync_heap_view_to_tlab();
        // Drop the getstatic value cache so a cached Value::Ref can never outlive
        // its object (the live value stays rooted via static_roots below).
        crate::tlcache::getstatic_clear();
        let fast = fast_roots_enabled();
        // ⭐**THE ROOT SET CARRIES ONLY WHAT THE MARK LOOP CAN FOLLOW.** This gather CLONED
        // EVERY SLOT of every frame's locals and operand stack on every registered thread,
        // every paused trampoline frame and every green thread — ints, longs, doubles,
        // nulls, the dead tail of an over-wide `max_locals` — into one `Vec<Value>` at 24
        // bytes each, and `Heap::gc` then threw all of them away (`grey!(val)` matches
        // `Ref | Array | Obj` and nothing else). A boot's frames are overwhelmingly
        // primitive slots, so the vector was several times the size of the set that
        // actually seeds the mark. Filtering at the PUSH also kills the one clone in
        // `Value` that owns anything: a `Value::Str` slot was an `Arc` increment here and a
        // decrement at the end of the collection, purely to be ignored in between.
        // Was: O(all live stack slots + all statics) `Value` clones and 24-byte pushes per
        // collection. Is: O(ref-carrying slots).
        //
        // The buffer itself is retained across collections (see `MARK_ROOTS`) instead of
        // doubling up from zero every time.
        let mut roots: Vec<Value> = if fast { mark_roots_take() } else { Vec::new() };
        for v in self.loader.static_roots() {
            if !fast || mark_reachable_from(&v) {
                roots.push(v);
            }
        }
        // Push a borrowed slot iff the mark loop could follow it (identity when !fast).
        macro_rules! seed {
            ($it:expr) => {
                for v in $it {
                    if !fast || mark_reachable_from(v) {
                        roots.push(v.clone());
                    }
                }
            };
        }
        // Every pointer refers to a `Frame` live on some thread's Rust call stack.
        // GC runs only while holding the GIL, so every OTHER thread is parked at
        // `gil::acquire` with its frames alive and unmutated; we read all threads'
        // frame stacks via the per-thread registry. SAFETY rests on that GIL
        // invariant + frames being pushed/popped in `run`.
        for &p in &frames::all() {
            let f = unsafe { &*(p as *const Frame) };
            seed!(f.locals().iter());
            seed!(f.stack().iter());
        }
        crate::execution_state::visit_pending_roots(|value| {
            if !fast || mark_reachable_from(value) {
                roots.push(value.clone());
            }
        });
        self.task_runtime.visit_roots(|value| {
            if !fast || mark_reachable_from(value) {
                roots.push(value.clone());
            }
        });
        // Paused explicit-stack frames (execute_stackful) — not on the Rust stack,
        // so scan them through the registered frame-vector pointer.
        for ep in crate::frames::exec_all() {
            let frames = unsafe { &*(ep as *const Vec<(Frame, Arc<str>, Arc<str>)>) };
            for (f, _, _) in frames {
                seed!(f.locals().iter());
                seed!(f.stack().iter());
            }
        }
        // Phase 2 tracer: gc_protect, green_threads, and shared singletons
        // route through visit_roots. Pass None as ctx for transition;
        // callers with ThreadContext will use gc_ctx later.
        {
            let mut push = |v: Value| {
                if !fast || mark_reachable_from(&v) {
                    roots.push(v);
                }
            };
            visit_roots(self, ctx, &mut push);
        }
        // JIT cell buffers — live compiled frames.
        for (ptr, len) in frames::cells_all() {
            let cells = unsafe { std::slice::from_raw_parts(ptr, len) };
            roots.extend(cells.iter().filter_map(|&c| jit::cell_to_root(c)));
        }
        #[cfg(feature = "next-gen-gc")]
        if crate::jit::runtime::stackmaps::declaring() {
            crate::jit::runtime::stackmaps::for_each_map_slot(&mut |c| {
                if let Some(r) = jit::cell_to_root(unsafe { *c }) {
                    roots.push(r);
                }
            });
        }
        frames::boxes_gather(&mut roots);
        self.lane.gather_roots(&mut roots);
        // In-flight native-call args: a heap Ref/Array a native holds across an allocation
        // that triggers THIS mark-sweep is otherwise unrooted (native_args are forwarded by
        // the ARENA collector but were NOT seeded here) → swept + its slot reused under a
        // new type. This is the StateHolder.values Reference2ObjectArrayMap use-after-free
        // (a fastutil map op holds the map as a native arg across a heap GC). Root them.
        frames::native_args_gather(&mut |v| {
            if !fast || mark_reachable_from(v) {
                roots.push(v.clone());
            }
        });
        // Cross-GC rooting: a handle reachable ONLY from an arena object cell is
        // invisible to this mark-sweep otherwise. UNIFIED MARK: the `Value::Obj` roots
        // already in `roots` (frame locals / statics / map entries) seed `Mark::Arena`
        // traversal in `heap.gc`, which walks LIVE arena objects' ref cells and marks
        // the handles they hold. This replaces the old `arena_handle_roots` blind
        // full-tenured scan, which rooted handles from DEAD tenured arena objects too —
        // pinning dead handle↔arena reference cycles (the codec-build retention that
        // grew tenured unbounded → `tenured exhausted`).
        // SHADOW root discovery (dry-run): the copying GC would walk exactly this
        // root set. Validate every root points at a live, aligned object (what the
        // mark phase relies on); count + log without mutating. A miss/garbage root
        // ⇒ SHADOW_GC_MISMATCH, but the live engine continues on mark-sweep below.
        #[cfg(feature = "next-gen-gc")]
        if crate::nextgen::shadow_enabled() {
            let mut total = 0u64;
            let mut valid = 0u64;
            for r in &roots {
                if matches!(r, Value::Ref(_) | Value::Array(_)) {
                    total += 1;
                    if self.heap.shadow_validate(r) {
                        valid += 1;
                    }
                }
            }
            crate::nextgen::shadow().record_cycle(total, valid);
        }
        let freed = self.heap.gc(&roots);
        // Return the root buffer (cleared) so the next collection reuses its capacity
        // instead of doubling up from zero again.
        if fast {
            mark_roots_put(roots);
        }
        // PRUNE the lambda-dedup cache: it is NOT a GC root, so an entry whose
        // handle was just swept must be dropped — else a later hit would hand back a reused (wrong)
        // handle. After this, every surviving entry is a LIVE closure; between GCs nothing is swept, so
        // hits stay valid. Cheap no-op when the cache is empty (no eligible lambdas / kill switch).
        if !self.lambda_dedup.is_empty() {
            let heap = &self.heap;
            self.lambda_dedup.retain(|_, &mut h| heap.is_ref_alive(h));
        }
        // Leak diagnostic: periodically dump heap composition so the growth RATE
        // per class is visible (not just the end state at OOM). RAVA_TRACE_GT.
        if self.trace.gt {
            self.gc_calls += 1;
            if self.gc_calls % 25 == 0 {
                eprintln!("[GT-COMP] gc#{} green={} est~{}MB",
                    self.gc_calls, self.green_threads.len(),
                    self.heap.approx_bytes() / (1024 * 1024));
                eprint!("{}", self.heap.composition());
            }
        }
        crate::vm::thread_context::refresh_heap_view_from_tlab();
        freed
    }
}
