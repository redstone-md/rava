//! Forking an iterator-shaped loop, and the closures its body dispatches through.

use crate::*;

impl Vm {
    /// GAP-S06/S02 substrate: every DISTINCT dispatch target a virtual call
    /// `owner.name desc` can select over the CURRENTLY LOADED hierarchy, as one
    /// representative receiver class per distinct resolution. `None` = the closure is
    /// unprovable and the caller must reject: the owner is unloadable, or an INTERFACE
    /// (lambda implementors are not enumerable from the loader). A concrete subtype
    /// whose resolution has no bytecode stays in the list as its own representative —
    /// the caller's per-class analysis then admits it only through a native contract.
    /// Valid only for the current loader epoch (memoised; cleared on any class load).
    pub(crate) fn virtual_dispatch_closure(&mut self, owner: &str, name: &str, desc: &str) -> Option<Arc<[String]>> {
        let dry_run = crate::ClassLoader::dry_run_loads_active();
        if !dry_run {
            self.offload_epoch_sync();
        }
        let key = (owner.to_string(), name.to_string(), desc.to_string());
        if !dry_run {
            if let Some(hit) = self.dispatch_closure_cache.get(&key) {
                return hit.clone();
            }
        }
        // FIXED POINT: enumerating the closure can itself load classes (super-chain
        // walks), and a class loaded mid-enumeration may be a concrete subtype the
        // snapshot missed. Recompute until the epoch is stable across one full pass;
        // only a stable-epoch result is cached or returned.
        let computed = if dry_run {
            loop {
                let before = self.loader.dry_run_staged_count();
                let pass = self.virtual_dispatch_closure_uncached(owner, name, desc);
                if self.loader.dry_run_staged_count() == before {
                    break pass;
                }
            }
        } else {
            loop {
                let e = self.loader.epoch();
                let pass = self.virtual_dispatch_closure_uncached(owner, name, desc);
                if self.loader.epoch() == e {
                    break pass;
                }
            }
        };
        if !dry_run {
            self.offload_epoch_sync();
            Self::debug_assert_not_gilfree("dispatch_closure_cache");
            self.dispatch_closure_cache.insert(key, computed.clone());
        }
        computed
    }

    pub(crate) fn virtual_dispatch_closure_uncached(&mut self, owner: &str, name: &str, desc: &str) -> Option<Arc<[String]>> {
        const ACC_INTERFACE: u16 = 0x0200;
        const ACC_ABSTRACT: u16 = 0x0400;
        let owner_class = self.loader.get_or_load(owner).ok()?;
        if owner_class.file.access_flags & ACC_INTERFACE != 0 {
            return None; // lambdas can implement interfaces — the closure is not enumerable
        }
        let names = self.loader.loaded_class_names();
        let mut reps: Vec<String> = Vec::new();
        let mut codes: Vec<usize> = Vec::new();
        for cn in names {
            let Ok(c) = self.loader.get_or_load(&cn) else { continue };
            if c.file.access_flags & (ACC_INTERFACE | ACC_ABSTRACT) != 0 {
                continue; // never a receiver's exact runtime class
            }
            if cn != owner && !self.loader.super_chain(&cn).iter().any(|s| &**s == owner) {
                continue; // not a subtype of the declared owner
            }
            match self.resolve_method(&cn, name, desc) {
                Ok(Some(decl)) => {
                    let key = decl
                        .file
                        .method(name, desc)
                        .and_then(|m| m.code.as_ref())
                        .map(|code| code as *const Code as usize)
                        .unwrap_or(0);
                    if key != 0 && codes.contains(&key) {
                        continue; // identical inherited implementation — already represented
                    }
                    codes.push(key);
                    reps.push(cn);
                }
                // No bytecode target: keep the class as its own representative — the
                // caller decides via its native contract (or rejects).
                _ => reps.push(cn),
            }
        }
        Some(reps.into())
    }

    /// GAP-A01 STAGE 1 (unloadable-iface closure, Gate 1): every loaded CONCRETE
    /// class that MAY be a receiver of `owner.name desc` where `owner` is exactly
    /// what `virtual_dispatch_closure` refuses — an interface or an unloadable
    /// name. Mirror of `virtual_dispatch_closure_uncached` with two deltas: the
    /// owner is NEVER loaded, and the subtype filter is the OVER-INCLUSIVE
    /// `opcodes::name_may_subtype` (an inconclusive hierarchy chain keeps the
    /// class IN the closure — an enumeration must over-include; the same pair
    /// direction the refutation walks must answer the other way). Same skeleton
    /// otherwise: concrete classes only, dedup by resolved `Code` pointer, a
    /// no-bytecode resolution stays as its own representative. DIAGNOSTIC-ONLY
    /// in stage 1: consumed exclusively by the drill printer — no admission path
    /// reads it. Memoised per loader epoch under the sibling closures' epoch-
    /// stable fixed-point rule (the may-subtype walks can themselves load).
    pub(crate) fn interface_dispatch_closure(&mut self, owner: &str, name: &str, desc: &str) -> Arc<[String]> {
        let dry_run = crate::ClassLoader::dry_run_loads_active();
        if !dry_run {
            self.offload_epoch_sync();
        }
        let key = (owner.to_string(), name.to_string(), desc.to_string());
        if !dry_run {
            if let Some(hit) = self.iface_closure_cache.get(&key) {
                return hit.clone();
            }
        }
        let computed = if dry_run {
            loop {
                let before = self.loader.dry_run_staged_count();
                let pass = self.interface_dispatch_closure_uncached(owner, name, desc);
                if self.loader.dry_run_staged_count() == before {
                    break pass;
                }
            }
        } else {
            loop {
                let e = self.loader.epoch();
                let pass = self.interface_dispatch_closure_uncached(owner, name, desc);
                if self.loader.epoch() == e {
                    break pass;
                }
            }
        };
        if !dry_run {
            self.offload_epoch_sync();
            Self::debug_assert_not_gilfree("iface_closure_cache");
            self.iface_closure_cache.insert(key, computed.clone());
        }
        computed
    }

    pub(crate) fn interface_dispatch_closure_uncached(&mut self, owner: &str, name: &str, desc: &str) -> Arc<[String]> {
        const ACC_INTERFACE: u16 = 0x0200;
        const ACC_ABSTRACT: u16 = 0x0400;
        let names = self.loader.loaded_class_names();
        let mut reps: Vec<String> = Vec::new();
        let mut codes: Vec<usize> = Vec::new();
        for cn in names {
            let Ok(c) = self.loader.get_or_load(&cn) else { continue };
            if c.file.access_flags & (ACC_INTERFACE | ACC_ABSTRACT) != 0 {
                continue; // never a receiver's exact runtime class
            }
            if !crate::opcodes::name_may_subtype(self, &cn, owner) {
                continue; // PROVABLY not a subtype (inconclusive chains stay in)
            }
            match self.resolve_method(&cn, name, desc) {
                Ok(Some(decl)) => {
                    let key = decl
                        .file
                        .method(name, desc)
                        .and_then(|m| m.code.as_ref())
                        .map(|code| code as *const Code as usize)
                        .unwrap_or(0);
                    if key != 0 && codes.contains(&key) {
                        continue; // identical inherited implementation — already represented
                    }
                    codes.push(key);
                    reps.push(cn);
                }
                // No bytecode target: keep the class as its own representative — the
                // caller decides via its native contract (or rejects).
                _ => reps.push(cn),
            }
        }
        reps.into()
    }

    /// F3.3 ITERATOR-LOOP FORK (RAVA_FORK_SHAPES). Called from the interpreter's invoke dispatch
    /// when a virtual/interface `iterator()` is about to run. If the caller's following bytecode
    /// is the canonical `while(it.hasNext()){ E e=(E)it.next(); e.m(); }` loop AND `e.m()` is an
    /// element-ownership-offloadable void method on the concrete element type, DIVERT: drain the
    /// collection, apply `m` to each element in list order (serially for now — the GIL-free
    /// parallel execution is the next increment), pop the collection receiver, and return the
    /// loop-exit pc so the interpreter SKIPS the (now-redundant) loop — the iterator is never
    /// created and the loop body never runs (it would re-do the work). Returns `None` to run the
    /// normal `iterator()` + loop unchanged (exact semantics preserved). The serial apply is
    /// byte-identical to the loop, so this is bit-exact even before parallelism.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn try_fork_iterator_loop(
        &mut self,
        frame: &mut Frame,
        code: &Code,
        iter_pc: usize,
    ) -> Result<Option<usize>, VmError> {
        if self.trampoline {
            return Ok(None); // green-thread scheduler mode — keep the intercept off the pump
        }
        let bc = &code.bytecode;
        // (1) This invoke must be `iterator()` returning an Iterator.
        let (Some(&hi), Some(&lo)) = (bc.get(iter_pc + 1), bc.get(iter_pc + 2)) else {
            return Ok(None);
        };
        let cp = ((hi as u16) << 8) | lo as u16;
        {
            let pool = &frame.class.file.pool;
            match pool.member_ref(cp) {
                Some(m) if m.name == "iterator" && m.descriptor.ends_with("Iterator;") => {}
                _ => return Ok(None),
            }
        }
        // (2) The following bytecode must be the (possibly-prefixed) iterator for-each skeleton
        //     `for (e : coll) { <prefix(e)>; e.m(); }`. The split matcher subsumes the plain
        //     single-statement loop (prefix empty).
        let Some(m) = jit::match_iterator_foreach_split(bc, iter_pc) else {
            return Ok(None);
        };
        let (ps, pe) = m.prefix_range;
        let e_local = m.e_local;
        if std::env::var_os("RAVA_FORK_TRACE").is_some() {
            let nm = frame.class.file.pool.member_ref(m.elem_cp).map(|e| format!("{}{}", e.name, e.descriptor)).unwrap_or_default();
            eprintln!("[FORK-ITER-DET] in={} m={nm} prefix={ps}..{pe} e={e_local}", frame.class.file.this_class);
        }
        // (2b) The prefix (the per-element sequential statements, e.g. the registry id-assign)
        //      must read ONLY `e` and write NO local, so it can be replayed per element with a
        //      fresh locals frame in list order. Any other local read / any local store / iinc →
        //      bail to the normal loop (exact semantics).
        {
            let mut p = ps;
            while p < pe {
                let op = bc[p];
                let load_local = match op {
                    0x15 | 0x16 | 0x17 | 0x18 | 0x19 => bc.get(p + 1).map(|&b| b as usize),
                    0x1a..=0x1d => Some((op - 0x1a) as usize),
                    0x1e..=0x21 => Some((op - 0x1e) as usize),
                    0x22..=0x25 => Some((op - 0x22) as usize),
                    0x26..=0x29 => Some((op - 0x26) as usize),
                    0x2a..=0x2d => Some((op - 0x2a) as usize),
                    _ => None,
                };
                if matches!(load_local, Some(l) if l != e_local) {
                    return Ok(None); // reads a non-`e` local → not replayable stand-alone
                }
                if matches!(op, 0x36..=0x4e | 0x84) {
                    return Ok(None); // a local store / iinc → cross-iteration state
                }
                match jit::insn_len_at(bc, p) {
                    Some(l) => p += l,
                    None => return Ok(None),
                }
            }
        }
        // (3) Resolve the per-element method; require a no-arg VOID call `e.m()`.
        let (mn, md) = {
            let pool = &frame.class.file.pool;
            match pool.member_ref(m.elem_cp) {
                Some(e) => (e.name.to_string(), e.descriptor.to_string()),
                None => return Ok(None),
            }
        };
        if md != "()V" {
            return Ok(None);
        }
        // (4) The collection is the operand-stack top (pushed for the iterator() call). Peek it;
        //     exclude non-collections + maps (their iteration is not a single-element Consumer).
        let coll = match frame.stack().last() {
            Some(v @ (Value::Ref(_) | Value::Obj(_))) => v.clone(),
            _ => return Ok(None),
        };
        match &coll {
            Value::Ref(h) => {
                if matches!(self.heap.get_field_syn(*h, crate::heap::Syn::Map), Value::Int(_)) {
                    return Ok(None);
                }
            }
            Value::Obj(p) => {
                if crate::arena_map::enabled() && crate::arena_map::is_arena_map(*p) {
                    return Ok(None);
                }
            }
            _ => return Ok(None),
        }
        let caller_class = frame.class.clone();
        let (mx_stack, mx_locals) = (code.max_stack, code.max_locals);
        // Root the collection across the (GC-capable) drain.
        let base = crate::vm::thread_context::root_len(&self.gc_protect);
        crate::vm::thread_context::push_root(&mut self.gc_protect, coll.clone());
        let out = (|| -> Result<Option<usize>, VmError> {
            let coll_now =
                crate::vm::thread_context::root_index(&self.gc_protect, base);
            let elems = match self.iterate_recv(&coll_now) {
                Ok(e) => e,
                Err(_) => return Ok(None),
            };
            if elems.len() < crate::opcodes::fork_min_n() {
                return Ok(None);
            }
            // G1.5 TARGET HUNT: record the loop's scale + soundness properties, then return to the
            // normal serial path (Ok(None)) — pure observation, no fork machinery, no codegen
            // change. The first element's class is a REPORT-ONLY sample; it never routes admission.
            if crate::opcodes::fork_target_scan() {
                let ecls = self.value_class_name(&elems[0]);
                if !(ecls.is_empty() || ecls == "?") {
                    let key = format!("{ecls}.{mn}{md}");
                    if crate::fork_scan::is_new(&key) {
                        let all_compiled = { let mut s = std::collections::HashSet::new(); self.fork_tree_all_compiled(&ecls, &mn, &md, 0, &mut s) };
                        let has_obj = { let mut s = std::collections::HashSet::new(); self.fork_tree_has_new(&ecls, &mn, &md, 0, &mut s) };
                        let has_arr = { let mut s = std::collections::HashSet::new(); self.fork_tree_has_newarray(&ecls, &mn, &md, 0, &mut s) };
                        let fork_safe = { let mut s = std::collections::HashSet::new(); self.method_fork_safe(&ecls, &mn, &md, true, 0, &mut s) };
                        crate::fork_scan::set_props(&key, all_compiled, has_obj, has_arr, fork_safe);
                    }
                    crate::fork_scan::record_n(&key, elems.len() as u64);
                }
                return Ok(None);
            }
            // (5) GAP-S01: resolve the normal runtime dispatch target for EVERY receiver under
            //     the GIL. The batch may fork only when each element's exact runtime class
            //     selects the SAME bytecode method; every distinct class must then pass the
            //     per-class effect + whole-tree-compiled proofs below. Anything unresolved,
            //     native, abstract or differing keeps the unchanged serial path.
            let admission_epoch = self.loader.epoch();
            let Some(batch) = self.fork_batch_target(&elems, &mn, &md) else {
                if std::env::var_os("RAVA_FORK_TRACE").is_some() {
                    eprintln!("[FORK-ITER-SKIP] m={mn}{md} batch dispatch proof failed (heterogeneous/unresolved/native/abstract target)");
                }
                return Ok(None);
            };
            let ecls = batch.classes[0].clone(); // trace representative (same Code for all classes)
            // G1.3 COLD-ROOT EARLY-OUT: only engage the fork machinery once the leaf's ROOT method is
            // compiled. A cold root makes try_parallel_apply_leaf bail (Ok(false)) AFTER materialising +
            // running the PASS-1 prefix, forcing the SLOW per-element interpreter serial PASS-2 over the
            // whole element list (the deopt that made the fork-on boot ~15x). Instead bail to the normal
            // COMPILED loop (Ok(None)) — it runs fast AND warms the root, so a later loop entry forks.
            if !matches!(self.jit_cache.get(&batch.code), Some(jit::JitStatus::Compiled(_))) {
                return Ok(None);
            }
            // The sound gates, run for EVERY distinct receiver class in the batch (GAP-S01: a
            // shared root body does not imply shared nested dispatch — `this`-calls inside the
            // body devirtualise per receiver class, so each class needs its own proof).
            // GAP-S04: there is NO optimistic relaxation — an unproved class always keeps the
            // unchanged serial path. Output hashes are verification outputs only and never
            // participate in admission.
            for cls in &batch.classes {
                // Element-ownership offloadability on the CONCRETE element type: `this`-calls
                // in `m` devirtualise against it.
                if !self.is_offloadable_devirt(cls, &mn, &md, false, true) {
                    if std::env::var_os("RAVA_FORK_TRACE").is_some() {
                        let why = self.offload_reason_devirt(cls, &mn, &md, false).unwrap_or_default();
                        eprintln!("[FORK-ITER-SKIP] elem={cls} m={mn}{md} not-offloadable: {why}");
                    }
                    if Self::offload_drill_gate() {
                        self.offload_drill_print(cls, &mn, &md);
                    }
                    return Ok(None);
                }
                // Fork only a COMPILED per-element call tree. Diverting an UNCOMPILED method to
                // the interpreted per-element apply (a) deoptimises it vs its normal JIT path and
                // (b) runs the interpreter on workers, which races shared per-class state
                // (site_cache) / the shared gc_protect / cold-callee compilation — the compiled
                // path (per-thread tlcache + direct calls) is the one G1–G3.5 proved worker-safe.
                let mut seen = std::collections::HashSet::new();
                if !self.fork_tree_all_compiled(cls, &mn, &md, 0, &mut seen) {
                    if std::env::var_os("RAVA_FORK_TRACE").is_some() {
                        eprintln!("[FORK-ITER-SKIP] elem={cls} m={mn}{md} tree not all-compiled (interpreted worker path unsafe)");
                    }
                    return Ok(None);
                }
            }
            // GAP-S06: the proofs above range over the loaded hierarchy — if deriving them
            // loaded classes (epoch moved), they may have missed a new override. Stay serial
            // for this invocation; the next entry re-proves under the settled hierarchy.
            if self.loader.epoch() != admission_epoch {
                return Ok(None);
            }
            // (6) FORKABLE. Materialise into a heap array (GC forwards its cells in place).
            let arr = self.heap.alloc_values("Ljava/lang/Object;", elems);
            let gbase = crate::vm::thread_context::root_len(&self.gc_protect);
            crate::vm::thread_context::push_root(&mut self.gc_protect, Value::Array(arr));
            let n = self.heap.array_len(arr).unwrap_or(0);
            if std::env::var_os("RAVA_FORK_TRACE").is_some() {
                eprintln!("[FORK-ITER] elem={ecls} m={mn}{md} n={n} prefix={}..{} (serial divert)", ps, pe);
            }
            let mut applied = Ok(());
            // PASS 1 — replay the per-element prefix (e.g. the sequential registry id-assign) in
            // list order via a synthetic frame (prefix bytecode + `return`), `e` in its local.
            // Skipped when the prefix is empty (the plain single-statement loop).
            if pe > ps {
                let mut pbc = bc[ps..pe].to_vec();
                pbc.push(0xb1); // return
                let syn = Code { max_stack: mx_stack.max(8), max_locals: mx_locals.max((e_local + 1) as u16), bytecode: pbc, exceptions: Vec::new(), structural_flags: 0, flat_hint: std::sync::atomic::AtomicU8::new(0), flat_blocker: std::sync::atomic::AtomicU8::new(0), jit_entry_hint: std::sync::atomic::AtomicUsize::new(0) };
                for i in 0..n {
                    let e = self.heap.array_get(arr, i).unwrap_or(Value::Null);
                    let mut locals = vec![Value::Null; syn.max_locals as usize];
                    locals[e_local] = e;
                    let mut f = Frame::new(&syn, locals, caller_class.clone(), Vec::new());
                    if let Err(err) = crate::opcodes::run(self, &syn, &mut f) {
                        applied = Err(err);
                        break;
                    }
                }
            }
            // PASS 2 — apply `e.m()` to each element. GIL-free PARALLEL (G1) when eligible
            // (RAVA_FORK_PARALLEL + a no-alloc leaf method over arena elements): the compiled
            // per-element fn runs on RavaPool-style workers, reading shared-immutable state +
            // writing each element's OWN fields (disjoint). Else serial (byte-identical). The
            // parallel path is gated → default builds are unaffected.
            let mut done_parallel = false;
            if applied.is_ok() && crate::opcodes::fork_parallel() {
                let mut es: Vec<Value> = Vec::with_capacity(n);
                for i in 0..n {
                    es.push(self.heap.array_get(arr, i).unwrap_or(Value::Null));
                }
                match self.try_parallel_apply_leaf(&es, &batch.classes, &mn, &md) {
                    Ok(true) => done_parallel = true,
                    Ok(false) => {}
                    Err(err) => applied = Err(err),
                }
            }
            if applied.is_ok() && !done_parallel {
                for i in 0..n {
                    let e = self.heap.array_get(arr, i).unwrap_or(Value::Null);
                    let ec = self.value_class_name(&e);
                    let r = match self.resolve_method(&ec, &mn, &md) {
                        Ok(Some(decl)) => self.invoke(decl, &mn, &md, vec![e]).map(|_| ()),
                        Ok(None) => crate::natives::try_invoke(self, &ec, &mn, &md, &[e]).map(|_| ()),
                        Err(e) => Err(e),
                    };
                    if let Err(err) = r {
                        applied = Err(err);
                        break;
                    }
                }
            }
            crate::vm::thread_context::truncate_roots(&mut self.gc_protect, gbase);
            applied?;
            Ok(Some(m.loop_end_pc))
        })();
        // On a successful divert, pop the collection receiver (the loop region's net stack
        // effect is zero, so the skipped-to pc expects the pre-`aload coll` stack).
        if matches!(out, Ok(Some(_))) {
            frame.pop_opt();
        }
        crate::vm::thread_context::truncate_roots(&mut self.gc_protect, base);
        out
    }
}
