//! Tier-neutral execution kernel, starting with the ABI-stable Cranelift entry.
//!
//! Phase 1 borrows the current VM while it owns the compiled call protocol. The next
//! rewrite stages can move per-thread activation state behind this seam without changing
//! the native entry ABI.

use super::jit_enter::{
    eg_bit, enter_gates, hr_unwind, materialize_arena_array_return, prof_crumb_enter,
    sentinel_report, stat_note_run, EG_FAST_STUB_OFF, EG_HR_UNWIND, EG_INSTR, EG_LIVE,
};
use crate::class_loader::LoadedClass;
use crate::*;
use rava_classfile::Code;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

pub(crate) struct ExecKernel<'vm> {
    vm: &'vm mut Vm,
}

impl<'vm> ExecKernel<'vm> {
    pub(crate) fn new(vm: &'vm mut Vm) -> Self {
        Self { vm }
    }
}

impl Deref for ExecKernel<'_> {
    type Target = Vm;

    fn deref(&self) -> &Self::Target {
        self.vm
    }
}

impl DerefMut for ExecKernel<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.vm
    }
}
/// Owns the GC registration for a compiled activation's stack-resident cells.
///
/// The caller keeps the activation buffer alive until this guard is dropped.
/// A null context preserves the historical TLS lookup arm for an unpublished
/// entry context.
struct CellRootGuard {
    ctx: *mut frames::JitThreadCtx,
    registered: bool,
}

impl CellRootGuard {
    #[inline]
    fn register(ctx: *mut frames::JitThreadCtx, cells: &[i64], self_reg: bool) -> Self {
        if !self_reg {
            if ctx.is_null() {
                frames::cells_push((cells.as_ptr(), cells.len()));
            } else {
                // SAFETY: `ctx` was resolved on this thread for the current activation.
                unsafe { frames::ctx_cells_push(ctx, (cells.as_ptr(), cells.len())) };
            }
        }
        Self {
            ctx,
            registered: !self_reg,
        }
    }
}

impl Drop for CellRootGuard {
    #[inline]
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        self.registered = false;
        if self.ctx.is_null() {
            frames::cells_pop();
        } else {
            // SAFETY: this is the matching pop for the same thread-local context.
            unsafe { frames::ctx_cells_pop(self.ctx) };
        }
    }
}

impl ExecKernel<'_> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn enter_jit(
        &mut self,
        ptr: *const u8,
        ret: Option<jit::JT>,
        self_reg: bool,
        mid: u32,
        code: &Code,
        class: &Arc<LoadedClass>,
        name: &str,
        descriptor: &str,
        is_static: bool,
        locals: &[Value],
        stat_key: Option<&str>,
    ) -> Option<Result<Option<Value>, VmError>> {
        // GILFREE-RINGS item-1: force-compile path. The body is now cached
        // (Cold→compile above, or already Compiled) — return WITHOUT running it
        // on the empty `locals` the force_compile caller passed. Breaks the
        // compile-needs-run / lane-run-needs-compile cycle for supplyAsync tasks.
        #[cfg(feature = "next-gen-gc")]
        if COMPILE_ONLY.with(|c| c.get()) {
            let _ = (ptr, ret, self_reg, mid);
            return None;
        }
        // ⭐**COVERAGE, THE NUMBER THE ENGINE NEVER HAD.** The host profile says 89.9% of samples
        // are in Rust and ~10% inside compiled guest code, against a JVM that runs this program
        // essentially all-compiled — but nothing counted how many guest INVOCATIONS each side
        // serves, and a sample share is not a coverage share. This pair is that number.
        crate::diag::exec_compiled().inc();
        // ⭐**THE ENTRY'S PROCESS CONSTANTS, DERIVED ONCE PER THREAD INSTEAD OF FIVE TIMES PER
        // ENTRY.** See the module header: four of the five below were four spellings of one
        // fact ("nothing is instrumented"), and the fifth (`fast_stub_off`) is the entry's one
        // behaviour gate. One plain per-thread byte answers all of them; every consumer below
        // is a test of a register.
        let eg = enter_gates();
        let instr = eg & EG_LIVE != 0 || eg & EG_INSTR != 0;
        // Allocation attribution: a compiled body's object births belong to it, not to the
        // interpreted frame that happened to call it.
        //
        // `instr` DOMINATES this: `regions_armed()` is one of the gates [`EG_INSTR`] ORs, so a
        // clear bit proves `RegionGuard::enter` would have returned `None`, and the two relaxed
        // atomic loads plus the thread-local region swap disappear with it. Set (or the kill
        // switch) runs the original call unchanged.
        let _reg = if instr {
            crate::alloc_sites::RegionGuard::enter(crate::alloc_sites::Region::Compiled)
        } else {
            None
        };
        if let Some(k) = stat_key {
            stat_note_run(self, k);
        }
        // ⭐**THE ENTRY PLAN (kill switch `RAVA_NO_ENTRY_PLAN=1`).** Everything this
        // function used to derive from the DESCRIPTOR STRING on every one of 7.6M boot
        // entries — which argument slot is a reference, where the wide params sit, whether
        // the return type is an array — is a constant of the method. `jit::entry_plan`
        // memoizes it against the method's interned id (`mid`), which is already in hand
        // here. `None` ⇒ take the original walk unchanged (switch off, or an unpublished
        // `mid`), so the slow path stays byte-identical and is what the kill switch
        // restores. See `jit/abi.rs` for the record and the key's uniqueness argument.
        let plan = jit::entry_plan(mid, descriptor, is_static);
        // ONE thread-local crossing for the whole entry instead of four. `box_len`,
        // `cells_push`, `cells_pop` and `box_truncate` each resolved this thread's
        // `frames` cluster independently — a `LocalKey::with` lazy-init check plus a
        // `RefCell` borrow apiece, for what is one thread's one struct. `jit_direct_invoke`
        // already pays for the address once and reuses it; a `RAVA_TLS_STAT` census of a
        // real boot priced that move at 635.8M of the engine's 1.654G crossings. The
        // aliasing story is the established one: the ctx `Box` is allocated on first use
        // and never replaced or moved, and no Rust borrow of it is held across the guest
        // call. Tied to `plan` so the kill switch reverts both halves in one run.
        let ctx = match plan {
            Some(_) => frames::ctx_addr(),
            None => std::ptr::null_mut(),
        };
        // The `RAVA_NO_FAST_ENTER3` gate, read out of the plan record this entry has just
        // dereferenced anyway rather than out of a separate process-constant `OnceLock` —
        // the previous round's finding was that per-callback gate READS are themselves the
        // cost, so a switch that rides an already-loaded cache line is free. `false` when
        // there is no plan at all, which keeps every skip below off the unplanned path.
        let fast3 = matches!(plan, Some(p) if p.fast3);
        // Pack the incoming locals into cells. REFERENCE slots (`this` + ref/array
        // params) go through `box_ref` so a boxed primitive or Str keeps its exact
        // Value (a flat pack would lose a boxed `0`/`0.0` as null); PRIMITIVE slots
        // pack flat. The walk mirrors `expand_locals_into` (long/double = 2 slots).
        let box_base = if ctx.is_null() {
            frames::box_len()
        } else {
            // SAFETY: `ctx` came from `frames::ctx_addr()` on this thread, two lines up.
            // `ctx.box_len` is an exact mirror of `BOXES.len()` (only `box_push` and
            // `box_truncate` write either, and both write both).
            unsafe { frames::ctx_box_len(ctx) }
        };
        // ── THE CELL BUFFER (kill switch `RAVA_NO_FAST_STUB=1`) ────────────────────────
        //
        // The buffer handed to compiled code is ALWAYS exactly `max_locals + max_stack`
        // cells: `max_locals` of argument/local slots plus the GC spill area compiled code
        // writes live refs into before any GC-capable callback (see `jit::spill_for_gc`),
        // which is why the registered root length must span both. The old construction
        // reached that length in three steps — `Vec::with_capacity(total)`, one `push` per
        // argument slot, then `resize(total, 0)` to zero the spill tail — and paid a
        // malloc/free pair for it on every one of ~7.6 M compiled entries a boot.
        //
        // FAST ARM: for `total <= INLINE_CELLS` the cells live in this function's own stack
        // frame. No allocator call at all, the memory is already hot, and the pack writes by
        // index instead of through `Vec::push`'s capacity check. Above the inline ceiling a
        // single exact-size zeroed heap buffer replaces capacity+push+resize, so even the
        // large-frame arm makes one allocator call instead of one plus a zero-fill pass.
        //
        // BYTE-IDENTITY: `resize` both zero-extends and TRUNCATES, so the old buffer's final
        // contents were "packed cell `i` for `i < min(pushed, total)`, 0 above". Pre-zeroing
        // `total` cells and writing only in range reproduces that array exactly; see
        // `jit::marshal_cells` for the per-slot argument.
        //
        // ⛔THE REFUTED NEIGHBOUR, KEPT ON THE RECORD SO IT IS NOT REBUILT: reserving
        // `max_locals + max_stack` up front (rather than `max_locals` and letting `resize`
        // grow it) was the lever the lane-contention profile pointed at — `finish_grow` runs
        // 1.55% -> 6.76% from one lane worker to six, with seven mimalloc rows behind it, and
        // caller attribution named THIS site on two verified chains. Two binaries, interleaved,
        // P-cores: neutral at the default width (-1.22% on the minima, arms OVERLAP) and
        // +3.42% WORSE at width 6. A count of removed work is necessary and never sufficient;
        // a row that grows with worker count is a CANDIDATE for the contention, not the
        // contention. What is done here is different in kind — it removes the ALLOCATION,
        // not the growth — but the same rule applies to it and the A/B is what decides.
        //
        // `Site::JitRunCells` below keeps the census attached to the site.
        // The entry's one BEHAVIOUR gate, read off the per-thread word rather than through its
        // own `OnceLock` acquire load on every one of ~7.6 M entries. `eg_bit` calls the live
        // reader under the kill switch, so the OFF arm is the identical `OnceLock` probe from
        // the identical position.
        let fast = !eg_bit(eg, EG_FAST_STUB_OFF, jit::fast_stub_off);
        let total = code.max_locals as usize + code.max_stack as usize;
        let mut inline_cells: [std::mem::MaybeUninit<i64>; jit::INLINE_CELLS] =
            [const { std::mem::MaybeUninit::uninit() }; jit::INLINE_CELLS];
        let mut heap_cells: Vec<i64>;
        let cells: &mut [i64] = if fast && total <= jit::INLINE_CELLS {
            // SAFETY: at least `total <= INLINE_CELLS` elements are zeroed and then handed out
            // as initialised `i64`s, which have no invalid bit patterns; the array outlives
            // the borrow (it is dropped at the end of this function, after `call_raw` and
            // after the root de-registration below).
            unsafe {
                let p = inline_cells.as_mut_ptr() as *mut i64;
                if eg & EG_HR_UNWIND != 0 {
                    // HR-UNWIND (module header): a CONSTANT-length zero of the whole inline
                    // array — eight vector stores, no `memset` libcall. The cells past `total`
                    // are never read: the slice below, the root registration and the marshal
                    // all take `total`, and the array is this frame's own stack.
                    std::ptr::write_bytes(p, 0, jit::INLINE_CELLS);
                    hr_unwind::note();
                } else {
                    // Historic shape, verbatim: zero exactly `total` cells — a run-time length,
                    // so a `memset` call.
                    std::ptr::write_bytes(p, 0, total);
                }
                std::slice::from_raw_parts_mut(p, total)
            }
        } else {
            heap_cells = vec![0i64; total];
            &mut heap_cells[..]
        };
        // HR-UNWIND: both zeroing arms must hand `marshal_cells` the same buffer — `total`
        // cells, every one zero. Debug-only, so the production path is unchanged.
        debug_assert!(
            cells.iter().all(|&c| c == 0),
            "cell buffer not zero before marshal"
        );
        jit::marshal_cells(self, plan, descriptor, is_static, locals, cells);
        // Exact-size from the start now, on both arms — GREW is 0 by construction.
        //
        // `vecgrow::enabled()` is one of the gates [`EG_INSTR`] ORs and `note` opens with that
        // same read, so a clear bit proves the call would have returned on its first line.
        if instr {
            crate::vecgrow::note(crate::vecgrow::Site::JitRunCells, total, total, total);
        }
        // Stash the VM for the heap callbacks; register the cell buffer as GC
        // roots (a callee may allocate/collect); run; restore.
        //
        // ONE thread-local resolution of the ACTIVE JitCtx instead of two.
        // `array_desc_cell_ptr()` and `clear_jit_fault()` each ran `active_jit_ctx`, whose
        // body is a `WORKER_JIT_CTX.with(..)` crossing that picks this thread's worker ctx
        // over the Vm's shared one — the same answer, resolved twice, on a 7.6 M-entry path.
        // `array_desc_cell_ptr` RETURNS that ctx pointer (the descriptor base is field 0 of
        // `JitCtx`, so the cell address and the struct address are the same word), and
        // `clear_jit_fault` is defined as `(*that ctx).fault = 0`. Doing the store through
        // the pointer already in hand is the identical write to the identical address, in the
        // identical order.
        let desc_cell = self.heap.array_desc_cell_ptr();
        if fast {
            // SAFETY: `desc_cell` is `active_jit_ctx(&self.jit_ctx)` cast to i64 — a live
            // `JitCtx` owned either by this thread's worker block or by `self.heap` — and we
            // hold `&mut self`, so no other borrow of it is live. This is exactly what
            // `Heap::clear_jit_fault` performs.
            unsafe { (*(desc_cell as *mut crate::heap::JitCtx)).fault = 0 };
        } else {
            self.heap.clear_jit_fault();
        }
        // ── SKIP 3 (kill switch `RAVA_NO_FAST_ENTER3=1`): THE VM PUBLICATION ALREADY HAS
        // THE ADDRESS IT WAS ABOUT TO GO AND FETCH. ─────────────────────────────────────
        // `jit::set_current_vm` does two things: it stamps `Vm::jit_ctx_slot` with
        // `frames::ctx_addr()` — the baked cell-root base an unarmed compiled prologue
        // loads — and it replaces the `CURRENT_VM` thread-local. The first of those is a
        // THIRD resolution of this thread's `frames` cluster on a path that already
        // resolved it into `ctx` ~90 lines above (one `LocalKey::with` lazy-init check
        // plus a `RefCell::borrow_mut` with its borrow-flag store, unborrow store and
        // panic edge, plus the `get_or_insert_with` null test).
        //
        // The value is provably the same word. `ctx` IS `frames::ctx_addr()` on this
        // thread, and the ctx `Box` is allocated on first use and never replaced or moved
        // — the invariant this function already relies on to keep using `ctx` across
        // `call_raw` for the cells pop and the box unwind. So storing `ctx` here is the
        // identical store of the identical value to the identical address, and
        // `set_current_vm_worker` performs the remaining half (`CURRENT_VM.replace`)
        // unchanged, returning the same previous pointer.
        //
        // COLD ARM: no plan (so no `ctx` — the body was published before
        // `register_compiled_method` stamped a `mid`, or `RAVA_NO_ENTRY_PLAN` is set), or
        // `RAVA_NO_FAST_ENTER3` is set. Both fall straight back to `set_current_vm`, which
        // resolves the address itself exactly as before.
        let prev = if fast3 {
            self.jit_ctx_slot
                .store(ctx as usize, std::sync::atomic::Ordering::Relaxed);
            jit::set_current_vm_worker(self.vm as *mut Vm)
        } else {
            jit::set_current_vm(self.vm as *mut Vm)
        };
        // Deep-IC v3: a self-registering callee pushes/pops its own frame in its
        // prologue/epilogue — the wrapper-side registration would be redundant.
        // ⛔REFUSED, AND THE REFUSAL IS THE POINT: this registration is NOT skipped for a
        // body the compiler proved GC-leaf. A GC-leaf body is allocation-free in its
        // NORMAL path only — a getfield/aaload on a null receiver still routes to a host
        // callback that CONSTRUCTS the pending exception, which allocates and can move the
        // arena, and the body then runs on to its return with the fault latched (not every
        // downstream site is fault-gated). With the cells unregistered the moving collector
        // cannot forward them, so that window leaves stale arena pointers in a buffer
        // compiled code may still dereference — the exact class of bug the `forward_if_moved`
        // barriers in `jit::marshal_cells` exist to close.
        //
        // ⛔THE NARROWER VERSION WAS ALSO WEIGHED AND REFUSED, ON PRICE RATHER THAN ON RISK,
        // AND RE-WEIGHED SINCE — THE PRICE ONLY GOT SMALLER. The plan CAN prove a buffer that
        // no reference can ever reach: `ref_mask == 0` (now precomputed as `EntryPlan::
        // all_prim`) plus `is_gc_leaf` (which forbids ldc/getstatic/new/newarray/invoke/
        // instanceof/athrow) means the body has no source of a reference at all, so
        // registering the buffer finds nothing and forwards nothing. What that proof buys was
        // priced at `ctx_cells_push` + `ctx_cells_pop` + the `box_truncate_via` mirror test,
        // about fourteen instructions. The truncate third of that has since become free on
        // its own — the mirror test at the exit below already skips it DYNAMICALLY, for a
        // strictly larger population than any static rule can name — so what a GC-leaf proof
        // would still remove is `push` + `pop`: two stores, an increment, a decrement and one
        // capacity compare into a cache line this entry has already touched. That is roughly
        // ten instructions, bought with a memoized bytecode scan and a NEW GC-correctness
        // argument, on a path whose real costs are thread-local resolutions and the guest
        // call itself. It is not worth the argument, and it is still refused. The crossings
        // are where the entry spends; three of them are what this round removes.
        let cell_roots = CellRootGuard::register(ctx, cells, self_reg);
        // DEBUGGER EXTENSION (RAVA_JIT_SENTINEL): bracket the compiled call with a class-Arc
        // integrity scan. If this compiled method (or a nested compiled callee that returns
        // first) corrupts a heap Object's class, `after > before` NAMES it — the culprit the
        // instrumentation-sensitive Heisenbug otherwise hides. The innermost jit_run returns
        // first, so the FIRST report is the actual corrupter. Latch to report once.
        let sentinel_before = if instr && jit_sentinel_enabled() {
            Some(self.heap.corrupt_class_probe())
        } else {
            None
        };
        // Compiled-aware profiler (RAVA_PROF): publish the class about to run so the
        // background wall-clock sampler can attribute compiled time by class (the
        // flat op-loop sampler is blind to compiled code). No-op fast when PROF off.
        // Method-level crumb under RAVA_PROF (the `format!` lives in an out-of-line cold
        // helper — no allocation and no frame slots when the profiler is off) so the
        // compiled-wall sampler pinpoints the hot METHOD, not just the class.
        let _prof_crumb = if instr && crate::prof::enabled() {
            prof_crumb_enter(&class.file.this_class, name)
        } else {
            0
        };
        jit::jit_method_push(mid);
        let bits = unsafe { jit::call_raw(ptr, cells, desc_cell) };
        jit::jit_method_pop();
        crate::prof::crumb_exit(_prof_crumb);
        if let Some((c0, _)) = sentinel_before {
            let (c1, h1) = self.heap.corrupt_class_probe();
            if c1 > c0 {
                sentinel_report(class, name, descriptor, locals, c0, c1, h1);
            }
        }
        drop(cell_roots);
        // Decode the result BEFORE reclaiming the box table: a reference return
        // may be a BOX_TAG index into jit_box_table (Str / boxed primitive) that
        // the truncate below would drop. The return descriptor disambiguates
        // array-vs-object for a bare tagged handle.
        //
        // ⭐**"A PRIMITIVE-ONLY RETURN NEEDS NO DESCRIPTOR INSPECTION" IS ALREADY TRUE AND
        // COSTS NOTHING — MEASURED BY READING, NOT BY BUILDING.** Every non-`Ref` return
        // takes the `other` arm below, which is `unpack_ret`: a match on the `JT` the
        // compile already resolved. No branch of it touches `descriptor`, so an int/long/
        // float/double/void return has not read a descriptor byte here since the `ret`
        // parameter existed. There is no work to skip and nothing was changed for it.
        let (result, returns_array) = match ret {
            Some(jit::JT::Ref) => {
                // The return descriptor was only ever consulted for ONE BIT: array vs
                // object for a bare tagged handle. The plan carries it; without a plan,
                // re-derive it exactly as before.
                let ret_array = match plan {
                    Some(p) => p.ret_array,
                    None => crate::descriptor::ret_desc(descriptor).starts_with('['),
                };
                (Some(jit::unbox_ref_kind(self, bits, ret_array)), ret_array)
            }
            other => (jit::unpack_ret(other, bits), false),
        };
        // The box table is truncated on EVERY exit, and on the overwhelming majority of
        // entries there is nothing to truncate: no argument boxed (only a Str, a StdOut
        // sentinel or a boxed primitive takes the `box_push` arm of `box_ref`) and the
        // callee pushed none either. `ctx.box_len` is an exact mirror of `BOXES.len()`, so
        // `box_len <= box_base` PROVES that — the `Vec::truncate` would be a no-op and the
        // mirror rewrite would store the value it already holds. This is the same box
        // discipline the self-registering compiled epilogue already runs on itself.
        //
        // ⭐**AND THAT IS ALREADY THE "LEAF CALLEE NEEDS NO BOX-TABLE TRUNCATE" SKIP,
        // PROVED BETTER THAN A PLAN COULD PROVE IT.** A plan-based version would have to
        // reason statically — no reference arguments AND a body that cannot box — and would
        // still be an approximation. `box_truncate_via`'s mirror test is the DYNAMIC fact:
        // it compares what the box table actually holds against the base this entry took,
        // so it fires for every entry that boxed nothing, including ones no static rule
        // would have cleared. Two loads and a compare, no thread-local crossing, no
        // `Vec::truncate`. Nothing was added here because the cheaper thing already runs.
        if ctx.is_null() {
            frames::box_truncate(box_base);
        } else {
            // SAFETY: as the push above.
            unsafe { frames::box_truncate_via(ctx, box_base) };
        }
        // The RESTORE half of the publication skip above, same argument: `set_current_vm`
        // re-resolves `frames::ctx_addr()` to stamp the OUTER Vm's `jit_ctx_slot`, and this
        // thread's ctx address is already in `ctx`. A null `prev` (the outermost entry)
        // makes `set_current_vm` skip the store entirely, so the fast arm skips it too and
        // the two are the same sequence of writes.
        //
        // COLD ARM: no plan, or `RAVA_NO_FAST_ENTER3` — straight back to `set_current_vm`.
        if fast3 {
            if !prev.is_null() {
                // SAFETY: `prev` is the `CURRENT_VM` this entry displaced — a `Vm` still
                // live on this thread's stack (its `jit_enter` frame is below ours), and
                // `jit_ctx_slot` is a `&'static AtomicUsize`. This is exactly the deref
                // `set_current_vm` performs on the same pointer.
                unsafe { &*prev }
                    .jit_ctx_slot
                    .store(ctx as usize, std::sync::atomic::Ordering::Relaxed);
            }
            jit::set_current_vm_worker(prev);
        } else {
            jit::set_current_vm(prev);
        }
        if let Some(error) = crate::execution_state::pending_error_take() {
            return Some(Err(error));
        }
        let result = if returns_array {
            result.map(|value| materialize_arena_array_return(self, value))
        } else {
            result
        };
        Some(Ok(result))
    }
}

#[cfg(test)]
mod tests {
    use super::CellRootGuard;
    use crate::frames;

    #[test]
    fn cell_root_guard_owns_register_and_unregister_lifetime() {
        let ctx = frames::ctx_addr();
        let cells = [11i64, 22];
        let before = unsafe { (*ctx).top };

        {
            let roots = CellRootGuard::register(ctx, &cells, false);
            assert_eq!(unsafe { (*ctx).top }, before + 1);
            drop(roots);
        }
        assert_eq!(unsafe { (*ctx).top }, before);

        {
            let _roots = CellRootGuard::register(std::ptr::null_mut(), &cells, false);
            assert_eq!(unsafe { (*ctx).top }, before + 1);
        }
        assert_eq!(unsafe { (*ctx).top }, before);

        {
            let _roots = CellRootGuard::register(ctx, &cells, true);
            assert_eq!(unsafe { (*ctx).top }, before);
        }
    }
}
