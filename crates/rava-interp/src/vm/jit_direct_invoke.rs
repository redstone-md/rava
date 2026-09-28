//! The direct invoke path itself.

use crate::*;

impl Vm {
    /// GAP-S09 stage 4 moved block (read-only): the invoke half of
    /// `jit_direct_call` — argument slot layout, capture re-fetch, `call_raw`
    /// and box/return re-homing — verbatim, re-typed against `&self` (every
    /// helper it uses is already a shared borrow: `box_ref`/`unbox_ref` take
    /// `&Vm`, the heap accessors are `&self`, the cell buffers and box table
    /// are `frames::` TLS). Shared by the serial and worker call arms.
    /// (`caller_ptr`/`cp`/`kind` feed only the next-gen-gc STALE-RETURN forensic.)
    #[cfg_attr(not(feature = "next-gen-gc"), allow(unused_variables))]
    pub(crate) fn jit_direct_invoke(
        &self,
        entry: jit::DirectEntry,
        recv_cell_in: Option<i64>,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> Option<Result<i64, VmError>> {
        let jit::DirectEntry {
            fptr, buf_len, wide_mask, ret_ref, shift, lam_fp, site_key, self_reg, cap_n, cap_ref_mask, ..
        } = entry;
        // A lambda entry drops raw[0] (the lambda object): raw[1..] map directly
        // onto the target's locals (its receiver for an unbound instance ref,
        // params 1:1 for a static ref). The identity guard alone can be spoofed
        // by a swept+REUSED handle — re-check the lambda's content fingerprint.
        // V3: read the LIVE receiver handle from raw[0] (forward-barriered). recv_guard is
        // a lam_fp (capturing) / identity cell / class-ptr, so lambda_fp + the captured
        // re-fetch MUST use the real handle — the historic bug fed recv_guard (a class-ptr)
        // into lambda_fp/lambda_of, truncating a pointer as a handle → garbage / corruption.
        // Reuse the probe's barriered cell when it handed one on (HIT path); otherwise
        // barrier it here exactly as before. Only `DirectProbe::Hit` supplies one, and
        // nothing between that hit and here can collect — the MISS arm resolves first
        // (a class load can move the receiver) and deliberately hands on None.
        let recv_cell = match recv_cell_in {
            Some(c) => c,
            None => crate::nextgen::forward_if_moved(raw.first().copied().unwrap_or(0)),
        };
        let lam_handle: u32 = {
            if (recv_cell as u64) >> 32 == (crate::value::REF_TAG as u64) >> 32 { recv_cell as u32 } else { 0 }
        };
        // RAVA_ARENA_LAMBDA receiver: the closure MOVED if a GC ran since the caller
        // computed this cell, so the barrier above must run before the header read.
        // Only a `shift` entry has a lambda in raw[0]; on a static entry raw[0] is the
        // first ARGUMENT, and asking about it would read a class word out of an integer.
        #[cfg(feature = "next-gen-gc")]
        let arena_lam = if shift { crate::nextgen::arena_lambda_of_cell(recv_cell) } else { None };
        #[cfg(not(feature = "next-gen-gc"))]
        let arena_lam: Option<(crate::arena::ArenaPtr, u32)> = None;
        // λ-IMPL KEY (gated): re-check the pinned call-site identity instead of the
        // fingerprint. Same purpose (a swept+REUSED handle must not be trusted just
        // because it matched the cache key) and a strictly stronger proof: equal site
        // pointer ⇒ the live receiver was born at the SAME invokedynamic ⇒ same impl
        // method, same MethodHandle kind, same capture arity — everything the entry
        // baked. What it does NOT prove is the capture VALUES, which is why they are
        // re-read from this receiver below and never cached.
        if shift {
            match arena_lam {
                // An arena entry's token IS the interned site index (+1): equal index
                // ⇒ same invokedynamic ⇒ same impl, same kind, same capture arity and
                // kinds — everything the entry baked. Unlike `lambda_fp` this is an
                // identity, not a hash, so it cannot collide; and unlike a handle it
                // cannot be spoofed by slot reuse, because the index names a
                // process-lifetime interned site rather than a recyclable address.
                Some((_, si)) => {
                    if si as u64 + 1 != lam_fp {
                        return None;
                    }
                }
                None if site_key != 0 => {
                    if self.heap.lambda_site_key(lam_handle) != site_key {
                        return None;
                    }
                }
                None => {
                    if self.heap.lambda_fp(lam_handle) != lam_fp {
                        return None;
                    }
                }
            }
        }
        // Box table base captured HERE (before packing) so a CAPTURING lambda's
        // box_ref'd captures unwind with the frame (box_truncate below). For
        // non-capturing entries box_len is unchanged through packing → no-op.
        // `ctx.box_len` is the exact mirror of `BOXES.len()` (see frames), so the
        // fast path reads the base off the handle it already holds.
        //
        // FAST FRAME: one thread-local crossing for the whole synthetic frame instead of
        // five. Was `RAVA_DISPATCH_FASTFRAME`; the flag is gone because the census that
        // decided it against `RAVA_TLS_CLUSTER` also showed it is not a lever with two
        // sides — over a full boot this shape removes 635.8M of the engine's 1.654G
        // `LocalKey::with` crossings (578.6M of them in the Done window alone, 91.6% of
        // every crossing `frames` performs there), for identical work: the two arms
        // reached `Done` having taken 222,173,810 vs 222,173,829 direct hits. There is no
        // arm in which the old OFF path is wanted, and keeping it meant four `is_null()`
        // diamonds in the hottest function on the map. Taken HERE and not at entry so a
        // call the lambda guard above refuses still pays nothing for it.
        let ctx: *mut frames::JitThreadCtx = frames::ctx_addr();
        // SAFETY: `ctx` came from `ctx_addr()` on this thread, immediately above.
        let box_base = unsafe { frames::ctx_box_len(ctx) };
        // Lay the cells out by JVM local slot: wide values own 2 slots (the
        // compiled callee indexes locals by slot; the second slot is a pad).
        // Hot tiny callees dominate this path, so avoid a per-call malloc: a
        // stack buffer serves frames up to 64 locals, the heap only beyond.
        let n = buf_len as usize;
        // The buffer is a GC ROOT, so every slot the callee's frame occupies must
        // read as 0 before the call — but only those `n` slots are ever exposed
        // (`cells` is `[..n]`, and that is what `cells_push` registers). Zeroing
        // the whole 64-slot array was a 512-byte memset on every warm dispatch.
        let mut stack_buf = std::mem::MaybeUninit::<[i64; 64]>::uninit();
        let mut heap_buf: Vec<i64>;
        let cells: &mut [i64] = if n <= 64 {
            let p = stack_buf.as_mut_ptr() as *mut i64;
            unsafe {
                std::ptr::write_bytes(p, 0, n);
                std::slice::from_raw_parts_mut(p, n)
            }
        } else {
            heap_buf = vec![0i64; n];
            &mut heap_buf[..]
        };
        // ⛔A BATCHED FORWARDING SNAPSHOT WAS BUILT HERE AND MEASURED OUT. `forward_if_moved`
        // crosses a thread-local and takes a `RefCell` borrow per cell, and this loop asks it
        // the same question about the same two address ranges once per argument — on the
        // profile's #1 host row (17.63% of live ring samples) with ~200,000,000 direct hits a
        // boot. Hoisting the ranges into one snapshot per invoke is strictly fewer crossings.
        //
        // Flag A/B (`RAVA_NO_FWD_BATCH`), ABBA n=6: the per-cell path came out **−1.06% wall,
        // −1.82% done, 3/6 both ways** — a coin flip with the sign against the change. The
        // first pair had read +9%/+16% for the snapshot, which is the third time this session
        // a single pair pointed the opposite way from its batch.
        //
        // ⭐The mechanism for the sign is visible once the numbers force the question: the
        // snapshot is built UNCONDITIONALLY, while `forward_if_moved` early-outs on
        // `cell == 0 || a & 7 != 0` before touching anything. Most direct calls carry zero or
        // one reference argument, so batching pays a fixed cost to avoid one that was already
        // near-free. **A hoist only wins when the loop actually runs.**
        let mut w = 0usize;
        // Write one effective argument into its JVM local slot. Captures and SAM
        // arguments share wide_mask, whose indices describe their final
        // concatenated order in the implementation descriptor.
        let mut write_arg = |i: usize, c: i64| -> Option<()> {
            if w >= n {
                return None; // malformed arity vs max_locals — general path
            }
            let wide = i < 64 && wide_mask & (1 << i) != 0;
            // FIX: read barrier on the receiver AND every non-wide arg (the in-flight
            // `raw` buffer is the caller's args_slot — NOT in the rooted cells_all
            // set — and the PIC-miss resolve above (class load) can itself allocate
            // and COLLECT before this copy, leaving every ref in it stale young).
            // Barriering only cells[0] let a stale ARG through: a compiled callee
            // stored it (e.g. map.put(key, advancement)) and the interpreter later
            // dispatched on the freed slot — the flaky post-recipes boot ULE.
            // Wide (long/double) slots are exempt — their bit patterns can alias a
            // young address; int/float cells can't (sign-extended 32-bit never
            // reaches the young window), so barriering non-wide cells is safe.
            #[cfg(feature = "next-gen-gc")]
            {
                cells[w] = if wide { c } else { crate::nextgen::forward_if_moved(c) };
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                cells[w] = c;
            }
            w += 1;
            if wide {
                w += 1;
            }
            Some(())
        };
        // CAPTURING lambda: re-fetch the captures from the LIVE lambda on every
        // call (heap references may move), but pack them straight into the final
        // bounded cell buffer. The old path cloned the whole Vec<Value> and
        // allocated a second Vec<i64> for every SAM invocation.
        if cap_n > 0 {
            // RAVA_ARENA_LAMBDA: the captures are FLAT INLINE CELLS at fixed offsets in
            // the closure object — the whole point of the representation. Read them
            // straight out of the receiver: no `Vec<Value>` clone, no box-table walk,
            // no per-call malloc. (This is also the shape a machine-code hit arm would
            // have to emit; doing it here first proves the offsets and the barrier.)
            #[cfg(feature = "next-gen-gc")]
            if let Some((p, _)) = arena_lam {
                if crate::nextgen::arena_lambda_cap_n(p) != cap_n as usize {
                    return None; // arity disagrees with the entry — general path
                }
                if crate::picprobe::enabled() {
                    crate::picprobe::note_cap_arm(entry.leaf, self_reg);
                }
                for i in 0..cap_n as usize {
                    let (raw_cell, kind) =
                        crate::nextgen::arena_lambda_cell(p, crate::nextgen::AL_HDR_CELLS + i);
                    // Feasibility census: classify the cell EXACTLY as decode_arena_cell
                    // discriminates it, so the tally names which arms a machine-code copy
                    // would have to emit and whether the un-emittable one (BOXIDX) occurs.
                    if crate::picprobe::enabled() {
                        crate::picprobe::note_cap_cell(if kind < 4 {
                            0 // prim: identity copy
                        } else if raw_cell == 0 {
                            1 // null
                        } else {
                            match raw_cell & 3 {
                                1 => 2, // Ref handle   -> (cell>>2)|REF_TAG
                                2 => 3, // Array handle -> (cell>>2)|ARRAY_TAG
                                3 => 5, // arena_box index -> box_ref, NOT emittable
                                _ => 4, // raw arena pointer -> identity
                            }
                        });
                    }
                    let cv = crate::opcodes::decode_arena_cell(raw_cell, kind);
                    let c = if cap_ref_mask & (1u64 << i) != 0 {
                        jit::box_ref(self, &cv)
                    } else {
                        jit::pack_local(&cv)
                    };
                    write_arg(i, c)?;
                }
            }
            if arena_lam.is_none() {
                // λ-IMPL KEY (gated): BORROW the captures. `lambda_of` deep-clones the whole
                // `Lambda` — a fresh `Vec<Value>` allocation on every capturing-lambda
                // dispatch, i.e. one malloc/free per call on the hottest CSG path.
                let owned;
                let captured: &[Value] = if site_key != 0 {
                    self.heap.lambda_captures(lam_handle)?
                } else {
                    owned = self.heap.lambda_of(lam_handle)?;
                    &owned.captured
                };
                if captured.len() != cap_n as usize {
                    return None; // arity changed under a reused handle — general path
                }
                for (i, cv) in captured.iter().enumerate() {
                    let c = if cap_ref_mask & (1u64 << i) != 0 {
                        jit::box_ref(self, cv)
                    } else {
                        jit::pack_local(cv)
                    };
                    write_arg(i, c)?;
                }
            }
        }
        let args = if shift { raw.get(1..)? } else { raw };
        for (i, &c) in args.iter().enumerate() {
            write_arg(cap_n as usize + i, c)?;
        }
        jit::count_direct_hit();
        // Same call wrapper as jit_run: GC-root the cell buffer (the callee may
        // allocate/collect), unwind the box table afterwards — re-homing a
        // BOX-tagged reference return first so the unwind can't drop it.
        // (box_base was captured above, before any capturing-lambda box_ref.)
        let desc_cell = self.heap.array_desc_cell_ptr();
        // Deep-IC v3: a self-registering callee roots its own frame; skip the
        // redundant wrapper-side registration (2 TLS crossings on the hottest
        // compiled→compiled path).
        if !self_reg {
            // SAFETY: `ctx` came from `ctx_addr()` on this thread, above.
            unsafe { frames::ctx_cells_push(ctx, (cells.as_ptr(), cells.len())) };
        }
        // 20.3 wall-profile: THE CALLEE'S MACHINE CODE RUNS HERE, so it must be billed to
        // JIT_EXEC and not to the JIT_CALLBACK excursion that reached it. Without this guard the
        // direct-call path — which serves ~92% of all lambda host falls — attributed the callee's
        // ENTIRE compiled body to "PIC miss -> host dispatch", making a category meant to measure
        // the boundary tax read as boundary tax PLUS every body it dispatched to. That misreading
        // survived one published claim before the call site was checked; it is the same
        // entry-vs-self confusion the method table's `CURRENT_METHOD` doc warns about.
        let bits = {
            let _wp = crate::rava_sampler::enter(crate::rava_sampler::JIT_EXEC);
            // ⭐**THE DIRECT PATH ENTERS COMPILED CODE WITHOUT `jit_enter`, so it must set the
            // region itself.** Without this the callee's execution is charged to whatever region
            // the CALLER was in — which put `jit_bitset_isempty` and other compiled-code
            // callbacks inside the seam's own marshalling and inflated it. Same lesson the
            // allocation census paid three times: a region is only as true as its narrowest
            // boundary.
            let _reg = crate::alloc_sites::RegionGuard::enter(crate::alloc_sites::Region::Compiled);
            unsafe { jit::call_raw(fptr as *const u8, cells, desc_cell) }
        };
        // STALE-RETURN forensic: did this compiled callee RETURN a stale young pointer (its
        // result freed by a collection during the call, on the way out)? Checked while `cells`
        // is still rooted (before cells_pop). Distinguishes a stale RECURSIVE-mapAll result
        // (bug is in the callee's return) from a downstream strand.
        #[cfg(feature = "next-gen-gc")]
        if jit::stale_store_trace() {
            if let Some(fwd) = jit::stale_young_ptr(bits) {
                let caller: &LoadedClass = unsafe { &*caller_ptr };
                let nm = caller.file.pool.member_ref(cp).map(|m| m.name.to_string());
                // Decode the receiver's actual class (raw[0] for a non-static call) — names the
                // exact DensityFunction whose compiled mapAll returned the stale result, so its
                // CLIF can be dumped and the un-spilled areturn/apply value found.
                let recv_cls = if kind != 0 {
                    let r = *raw.first().unwrap_or(&0);
                    let rf = crate::nextgen::forward_if_moved(r);
                    if rf != 0 && (rf as u64) >> 40 == 0 && rf & 7 == 0 {
                        crate::nextgen::class_name(unsafe { crate::nextgen::obj_class(rf as *mut u8) })
                            .map(|s| s.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                };
                eprintln!(
                    "[STALE-RETURN] bits={bits:#x} fwd={fwd:#x} cp={cp} callee={nm:?} recv_class={recv_cls:?} caller={} jit={:?}",
                    caller.file.this_class,
                    jit::jit_method_cur()
                );
            }
        }
        if !self_reg {
            // SAFETY: `ctx` came from `ctx_addr()` on this thread, above.
            unsafe { frames::ctx_cells_pop(ctx) };
        }
        // SAFETY (both arms): as above — `ctx` is this thread's, from `ctx_addr()`.
        let out = if ret_ref && jit::is_box_cell(bits) {
            let v = jit::unbox_ref(self, bits, "");
            unsafe { frames::box_truncate_via(ctx, box_base) };
            jit::box_ref(self, &v)
        } else {
            unsafe { frames::box_truncate_via(ctx, box_base) };
            bits
        };
        // A callee fault (jit_pending set by its callbacks) propagates: return 0
        // like the general path; the caller's subsequent callbacks short-circuit
        // and its jit_run converts the pending exception to a throw.
        if self.jit_pending_is_set() {
            return Some(Ok(0));
        }
        Some(Ok(out))
    }
}
