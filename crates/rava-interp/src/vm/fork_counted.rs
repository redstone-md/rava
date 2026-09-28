//! Forking a counted loop with a simple body.

use crate::*;

impl Vm {
    /// QUARANTINE (GAP-S09, docs/runtime-gap-register.md): an INTERPRETER-ON-WORKER
    /// experiment — workers drive `opcodes::run` over the proven-simple body through
    /// the loud `WorkerVmRef::vm_mut_unsound_interp` `&mut Vm` escape. That aliased
    /// `&mut` is OUTSIDE the closed GAP-S09 soundness claim (which covers the
    /// default-off serial path and the typed compiled-worker surfaces); it is
    /// constructible ONLY through the `RAVA_FORK_NUMERIC` env gate below (default
    /// OFF ⇒ byte-identical serial path; `RAVA_FORK_TARGET_SCAN` is observe-only and
    /// never forks). Residual typed-interpreter work is filed under GAP-A02.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn try_fork_counted_loop(&mut self, frame: &mut Frame, code: &Code, goto_pc: usize) -> Result<bool, VmError> {
        // The env-gate check the quarantine rests on: no gate, no worker, no escape.
        if !crate::opcodes::fork_numeric() && !crate::opcodes::fork_target_scan() {
            return Ok(false);
        }
        let bc = &code.bytecode;
        let Some(m) = crate::jit::match_numeric_range_loop_toptest(bc, goto_pc) else {
            return Ok(false);
        };
        // Body escape analysis: index-partitioned + only OFFLOADABLE callees (pure, no shared
        // writes). The callee closure recurses via the standard offloadability analysis.
        {
            let ok = crate::offload::counted_loop_offloadable(
                code, &frame.class.file.pool, Some(frame.class.file.bootstrap_methods.as_slice()),
                m.iv_local, &mut |q: crate::offload::CalleeQuery| {
                    if q.exact {
                        self.is_offloadable(q.owner, q.name, q.desc, false)
                    } else {
                        match self.virtual_dispatch_closure(q.owner, q.name, q.desc) {
                            Some(ts) => ts.iter().all(|t| self.is_offloadable(t, q.name, q.desc, false)),
                            None => false,
                        }
                    }
                });
            if !ok {
                return Ok(false);
            }
        }
        if !Self::counted_loop_body_simple(bc, m.body_start_pc, m.body_end_pc, m.iv_local) {
            return Ok(false);
        }
        // Any callee in the body must be ALLOC-FREE (this fork opens no per-worker region) and its
        // whole tree COMPILED — a compiled callee resolves fields/methods via the per-thread
        // tlcache (getstatic/resolve), avoiding the shared per-class RefCells an INTERPRETED callee
        // would race on a worker. (The body itself is getstatic/getfield-free via body_simple.)
        {
            let mut q = m.body_start_pc;
            while q < m.body_end_pc {
                let op = bc[q];
                if matches!(op, 0xb6 | 0xb7 | 0xb8 | 0xb9) {
                    let cp = ((*bc.get(q + 1).unwrap_or(&0) as u16) << 8) | *bc.get(q + 2).unwrap_or(&0) as u16;
                    let Some((o, n, d)) = frame.class.file.pool.member_ref(cp)
                        .map(|mr| (mr.owner.to_string(), mr.name.to_string(), mr.descriptor.to_string())) else {
                        return Ok(false);
                    };
                    let mut s1 = std::collections::HashSet::new();
                    let mut s2 = std::collections::HashSet::new();
                    let mut s3 = std::collections::HashSet::new();
                    if self.fork_tree_has_new(&o, &n, &d, 0, &mut s1)
                        || self.fork_tree_has_newarray(&o, &n, &d, 0, &mut s2)
                        || !self.fork_tree_all_compiled(&o, &n, &d, 0, &mut s3)
                    {
                        return Ok(false);
                    }
                }
                match jit::insn_len_at(bc, q) {
                    Some(l) => q += l,
                    None => return Ok(false),
                }
            }
        }
        // Range. Current iv is the NEXT index (iter for the previous iv already ran serially before
        // this back-edge); bound is a local/const. `<=` (inclusive) extends hi by one.
        let cur = match frame.locals().get(m.iv_local) {
            Some(Value::Int(v)) => *v as i64,
            _ => return Ok(false),
        };
        let mut hi = match m.bound {
            crate::jit::RangeBound::Local(l) => match frame.locals().get(l) {
                Some(Value::Int(v)) => *v as i64,
                _ => return Ok(false),
            },
            crate::jit::RangeBound::Const(c) => c as i64,
        };
        if m.inclusive {
            hi += 1;
        }
        // Fork [cur, hi-1); the driver runs the FINAL iteration (hi-1) serially afterwards so its
        // scratch locals end exactly as a serial loop leaves them. Need enough iters to be worth it.
        if hi - 1 <= cur {
            return Ok(false);
        }
        let fork_hi = hi - 1;
        let n = (fork_hi - cur) as usize;
        // G1.5 TARGET HUNT: a counted loop reaching here already passed the SOUND gate (offloadable +
        // body-simple + alloc-free + fully-compiled tree — no garbage-continuation). Record its scale
        // and observe only (don't fork under a scan-only run).
        if crate::opcodes::fork_target_scan() {
            let key = format!("COUNTED-NUM {}@pc{}", frame.class.file.this_class, goto_pc);
            if crate::fork_scan::is_new(&key) {
                crate::fork_scan::set_props(&key, true, false, false, true);
            }
            crate::fork_scan::record_n(&key, n as u64);
            return Ok(false);
        }
        if n < crate::opcodes::fork_min_n() {
            return Ok(false);
        }
        let workers = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(1).saturating_sub(1).max(1).min(n);
        let echunk = n.div_ceil(workers);
        let vmref = crate::parallel::WorkerVmRef::new(self);

        // Share the class Arc across workers. LoadedClass is not Sync (RefCell statics/site_cache),
        // but a `counted_loop_body_simple` body touches NONE of those (no getstatic/getfield/call),
        // so sharing is sound; Arc's refcount itself is atomic. Assert via the SendArc wrapper.
        struct SendArc(std::sync::Arc<crate::class_loader::LoadedClass>);
        unsafe impl Send for SendArc {}
        unsafe impl Sync for SendArc {}
        let class = SendArc(std::sync::Arc::clone(&frame.class));
        let template = frame.locals().to_vec();
        let (bs, be, iv) = (m.body_start_pc, m.body_end_pc, m.iv_local);
        let class = &class;
        let template = &template;
        let code_ref = code;
        if std::env::var_os("RAVA_FORK_TRACE").is_some() {
            eprintln!("[FORK-NUM] in={} iv={iv} range=[{cur},{fork_hi}) n={n} workers={workers} body={bs}..{be} (GIL-free)",
                frame.class.file.this_class);
        }
        std::thread::scope(|scope| {
            for w in 0..workers {
                let wlo = cur + (w * echunk) as i64;
                let whi = (cur + ((w + 1) * echunk) as i64).min(fork_hi);
                if wlo >= whi {
                    continue;
                }
                scope.spawn(move || {
                    vmref.install_current();
                    crate::gil::set_worker();
                    crate::frames::register();
                    crate::parallel::set_gilfree(true);
                    // SAFETY: workers share &mut Vm; a simple body only reads immutable heap +
                    // writes disjoint arr[iv] cells (no alloc/GC, no shared-metadata mutation).
                    let vm = unsafe { vmref.vm_mut_unsound_interp() };
                    let mut wframe = {
                        // ONE buffer: the template IS the locals, and the operand region starts after
                        // them (stage 1 of the contiguous stack).
                        let tpl = template.clone();
                        let n = tpl.len();
                        // The worker runs this method's own body slice.
                        let mut wf = Frame::from_parts(code_ref, tpl, n, std::sync::Arc::clone(&class.0));
                        wf.pc = bs;
                        wf
                    };
                    for i in wlo..whi {
                        if crate::parallel::abort_requested() {
                            break;
                        }
                        wframe.pc = bs;
                        wframe.clear_stack();
                        wframe.locals_mut()[iv] = Value::Int(i as i32);
                        crate::opcodes::run_stop_set(be); // re-arm per index (run() takes it)
                        match crate::opcodes::run(vm, code_ref, &mut wframe) {
                            Ok(crate::opcodes::RunExit::Bounded) => {}
                            _ => {
                                crate::parallel::request_abort();
                                break;
                            }
                        }
                    }
                    crate::opcodes::run_stop_clear();
                    crate::parallel::set_gilfree(false);
                    crate::frames::unregister();
                    crate::jit::set_current_vm(std::ptr::null_mut());
                });
            }
        });
        if crate::parallel::abort_requested() {
            crate::parallel::clear_abort();
            return Ok(false); // an unexpected body exit → fall back to the serial loop
        }
        // Resume: run the FINAL iteration (fork_hi = hi-1) in the driver frame via the normal
        // interpreter (leaves driver scratch locals correct), by re-entering the body at fork_hi.
        frame.locals_mut()[iv] = Value::Int(fork_hi as i32);
        frame.pc = m.body_start_pc;
        Ok(true)
    }

    // The M3 COLUMN/POSITION-level fork PROTOTYPE (`try_fork_column`, RAVA_FORK_COLUMN) and its
    // `body_has_loop_carried` helper that lived here were RETIRED at GAP-S09 stage 5: the
    // prototype ran the full interpreter on gil-free workers over an OPTIMISTIC
    // (hash-arbitrated) purity trust, and the S2 chunk-fill fork superseded it as the sole
    // coordinate-map parallel path (docs/multicore-roadmap.md, docs/runtime-gap-register.md
    // GAP-S09).

    /// The MINIMAL worker-safe counted-loop body: branchless, and every op is arithmetic / a
    /// local load-store (not to `iv`) / a PRIMITIVE array load-store / a const / a stack op /
    /// arraylength / nop. Excludes calls, `new`, field/static access, ref array-store (aastore),
    /// branches, monitors — anything that (a) could alloc/GC, (b) touch a shared per-class RefCell
    /// (statics/site_cache), (c) mutate shared heap metadata, or (d) leave the loop's index range.
    /// Under this, a worker only reads immutable heap + writes disjoint `arr[iv]` cells → the
    /// concurrent GIL-free run is race-free.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn counted_loop_body_simple(bc: &[u8], start: usize, end: usize, iv: usize) -> bool {
        // Helper: the local index a load/store op targets (None if the op isn't a typed local op).
        let load_target = |op: u8, pc: usize| -> Option<usize> {
            match op {
                0x15..=0x19 => bc.get(pc + 1).map(|b| *b as usize), // iload/lload/fload/dload/aload
                0x1a..=0x1d => Some((op - 0x1a) as usize),
                0x1e..=0x21 => Some((op - 0x1e) as usize),
                0x22..=0x25 => Some((op - 0x22) as usize),
                0x26..=0x29 => Some((op - 0x26) as usize),
                0x2a..=0x2d => Some((op - 0x2a) as usize),
                _ => None,
            }
        };
        let store_target = |op: u8, pc: usize| -> Option<usize> {
            match op {
                0x36..=0x3a => bc.get(pc + 1).map(|b| *b as usize),
                0x3b..=0x3e => Some((op - 0x3b) as usize),
                0x3f..=0x42 => Some((op - 0x3f) as usize),
                0x43..=0x46 => Some((op - 0x43) as usize),
                0x47..=0x4a => Some((op - 0x47) as usize),
                0x4b..=0x4e => Some((op - 0x4b) as usize),
                _ => None,
            }
        };
        // Pass 1: the set of locals STORED anywhere in the body.
        let mut stored_anywhere = std::collections::HashSet::new();
        {
            let mut pc = start;
            while pc < end {
                if let Some(l) = store_target(bc[pc], pc) {
                    stored_anywhere.insert(l);
                }
                match jit::insn_len_at(bc, pc) {
                    Some(l) => pc += l,
                    None => return false,
                }
            }
        }
        // Pass 2: op whitelist + iv-immutability + loop-carried-dependency rejection. A local that
        // is stored somewhere but LOADED before its first store in the body carries a value from
        // the previous iteration (an accumulator like `sum += x`) → NOT index-parallelisable.
        let mut stored_so_far = std::collections::HashSet::new();
        let mut pc = start;
        while pc < end {
            let op = bc[pc];
            if store_target(op, pc) == Some(iv) || (op == 0x84 && bc.get(pc + 1).map(|b| *b as usize) == Some(iv)) {
                return false; // storing / iinc-ing the induction var in the body
            }
            if let Some(l) = load_target(op, pc) {
                if l != iv && stored_anywhere.contains(&l) && !stored_so_far.contains(&l) {
                    return false; // loaded-before-stored + stored later → loop-carried
                }
            }
            if let Some(l) = store_target(op, pc) {
                stored_so_far.insert(l);
            }
            let allowed = matches!(op,
                0x00                    // nop
                | 0x02..=0x11           // iconst_m1..sipush (NOT ldc/ldc_w — could be a String/Class ref)
                | 0x14                  // ldc2_w — long/double constant (never a ref)
                | 0x15..=0x35           // int/long/float/double/ref loads + all array LOADS
                | 0x36..=0x52           // local stores + primitive array stores (incl dastore 0x52)
                | 0x54..=0x56           // bastore / castore / sastore  (0x53 aastore EXCLUDED)
                | 0x57..=0x5f           // pop/dup/swap family
                | 0x60..=0x93           // arithmetic + shifts + logic + conversions + cmp
                | 0x84                  // iinc (non-iv, checked above)
                | 0xbe                  // arraylength
                | 0xb6..=0xb9           // invoke* — callee offloadability + all-compiled checked by the caller
            );
            if !allowed {
                return false;
            }
            match jit::insn_len_at(bc, pc) {
                Some(l) => pc += l,
                None => return false,
            }
        }
        true
    }
}
