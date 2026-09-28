//! Building an LLVM module from bytecode, and what is emittable at all.

use crate::*;

impl Vm {
    /// cnp tier-0: run a pure-int STATIC method as inline copy-and-patch native code.
    /// `None` => not eligible / unsupported shape → the caller keeps its normal path.
    /// Eligibility (all-int static signature) + a successful `cnp::compile` (which
    /// returns None on any unsupported opcode) fully gate correctness; the result is a
    /// plain `Value::Int`, so there is no heap/GC interaction to get wrong.
    #[cfg(all(windows, target_arch = "x86_64"))]
    pub(crate) fn cnp_run(
        &mut self,
        code: &Code,
        descriptor: &str,
        is_static: bool,
        locals: &[Value],
    ) -> Option<Value> {
        if !is_static {
            return None;
        }
        let nargs = all_int_static_sig(descriptor)?;
        let key = code as *const Code as usize;
        // CRANELIFT-AOT numeric tier (opt-in RAVA_AOT_CLIF): for the same pure-int
        // static shape, compile via aot_clif (Cranelift, through the AOT disk cache)
        // instead of the cnp x86 emitter — the integration seam that routes jit_run
        // through the AOT cache with Cranelift-quality code. Same int-locals ABI
        // (run_numeric takes the marshalled i64 slot buffer). Gated → default JIT is
        // byte-identical; both emitters are differentially covered by the cnp tests.
        #[cfg(feature = "next-gen-gc")]
        if aot_clif_enabled() && crate::aot_clif::numeric_eligible(&code.bytecode) {
            if !self.aot_clif_cache.contains_key(&key) {
                let c = crate::aot_clif::compile_numeric_aot(&code.bytecode);
                Self::debug_assert_not_gilfree("aot_clif_cache");
                self.aot_clif_cache.insert(key, c);
            }
            if let Some(f) = self.aot_clif_cache.get(&key).and_then(|o| o.as_ref()) {
                let mut int_locals = vec![0i64; (code.max_locals as usize).max(nargs)];
                for (i, slot) in int_locals.iter_mut().enumerate().take(nargs) {
                    match locals.get(i) {
                        Some(Value::Int(x)) => *slot = *x as i64,
                        _ => return None,
                    }
                }
                let r = unsafe { f.run_numeric(int_locals.as_mut_ptr()) };
                return Some(Value::Int(r as i32));
            }
        }
        if !self.cnp_cache.contains_key(&key) {
            // Through the on-disk AOT cache when RAVA_AOT_DIR is set (a later process
            // links the persisted machine code instead of recompiling); identical to
            // compile_link otherwise.
            let compiled = cnp::compile_link_aot(&code.bytecode);
            Self::debug_assert_not_gilfree("cnp_cache");
            self.cnp_cache.insert(key, compiled);
        }
        // Borrow-safely: the cache owns the CompiledFn; run it against caller-owned
        // buffers. `None` cached => ineligible (never recompiled).
        let f = self.cnp_cache.get(&key)?.as_ref()?;
        let mut int_locals = vec![0i64; (code.max_locals as usize).max(nargs)];
        for (i, slot) in int_locals.iter_mut().enumerate().take(nargs) {
            match locals.get(i) {
                Some(Value::Int(x)) => *slot = *x as i64,
                _ => return None, // unexpected non-int arg → fall back
            }
        }
        let mut stack = vec![0i64; code.max_stack as usize + 2];
        let mut frame = cnp::CnpFrame {
            stack: stack.as_mut_ptr(),
            sp: 0,
            locals: int_locals.as_mut_ptr(),
        };
        let r = unsafe { f.run(&mut frame) };
        if self.trace.jit_stats {
            use std::sync::atomic::{AtomicU64, Ordering};
            static HITS: AtomicU64 = AtomicU64::new(0);
            let n = HITS.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 1_000_000 == 0 {
                eprintln!("[CNP] {n} tier-0 native calls");
            }
        }
        Some(Value::Int(r as i32))
    }

    /// M3 single-thread / inc 6.3 — build a WHOLE-TREE LLVM module for `root_code`: the root plus
    /// every transitively-reachable LLVM-eligible static callee, as ONE module (root `@f0` external,
    /// callees `@fN` internal alwaysinline → clang inlines them into the root + cross-vectorises,
    /// and DCEs them so the root is the only .text function at offset 0 = the aot_run entry). Shares
    /// one %consts (each fn's constants at growing absolute offsets; array-ABI [0]/[1] reserved once).
    /// Returns (module IR, cvec with [0]/[1] filled), or None if the root or any callee isn't
    /// eligible (all-numeric-static + in-subset) → the caller falls to the Cranelift AOT translator.
    /// Devirt-armability probe (8.1c gate): can the method `(owner,name,desc)` — the fast-path
    /// body of a devirt guard — be emitted by the numeric-subset LLVM tier? Resolves M and dry-runs
    /// `emit_llvm_fn` into throwaway buffers (empty callee map ⇒ any invokestatic in M bails, which is
    /// correct: such an M isn't self-contained). Read-only; used only under a devirt probe.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn llvm_method_emittable(&mut self, owner: &str, name: &str, desc: &str) -> bool {
        // 8.2b: full-cascade mode does a recursive nested-mono-devirt walk (own memo + cycle guard).
        if llvm_cascade_probe() {
            let mut visited = std::collections::HashSet::new();
            return self.cascade_emittable(owner, name, desc, &mut visited, 0);
        }
        let key = crate::devirt::emit_memo_key(owner, name, desc);
        if let Some(v) = crate::devirt::emit_memo_get(key) {
            return v; // recurring virtual target — skip the hierarchy walk + rescan
        }
        // Cheap: numeric signature + every callee opcode in-subset (getfield primitive-only under
        // RAVA_LLVM_FIELD_PROBE). No IR-gen — a structural upper bound on the devirt fast-path.
        let v = (|| {
            crate::llvm_aot::llvm_desc_args_ret(desc)?;
            let decl = self.resolve_method(owner, name, desc).ok().flatten()?;
            let method = decl.file.methods.iter().find(|mm| mm.name == name && mm.descriptor == desc)?;
            let ccode = method.code.as_ref()?; // abstract/native → None
            Some(Self::llvm_bytecode_emittable(&decl, &ccode.bytecode))
        })()
        .unwrap_or(false);
        crate::devirt::emit_memo_put(key, v);
        v
    }

    /// 8.2b FULL-CASCADE emittability: numeric signature + a body of {subset ops, ANY getfield/
    /// putfield, monomorphic nested invokevirtual/interface whose target is RECURSIVELY cascade-
    /// emittable}. Cycle guard (`visited` → optimistic true on a back-edge) + depth cap. This is the
    /// ceiling an object-aware + devirt-inlining LLVM tier could reach. Read-only; emit still bails.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn cascade_emittable(
        &mut self,
        owner: &str,
        name: &str,
        desc: &str,
        visited: &mut std::collections::HashSet<u64>,
        depth: usize,
    ) -> bool {
        const CAP: usize = 64;
        let key = crate::devirt::emit_memo_key(owner, name, desc);
        if let Some(v) = crate::devirt::cascade_memo_get(key) {
            return v;
        }
        if visited.contains(&key) {
            return true; // recursion back-edge — assume it resolves (optimistic ceiling)
        }
        if depth > CAP {
            return false; // too deep to inline
        }
        // Numeric signature still required — a ref ARG/RETURN can't cross the numeric ABI (this is
        // the honest wall: methods taking/returning objects, e.g. DensityFunction.compute(ctx), bail).
        if crate::llvm_aot::llvm_desc_args_ret(desc).is_none() {
            return false;
        }
        visited.insert(key);
        let resolved = self.resolve_method(owner, name, desc).ok().flatten();
        let v = if let Some(decl) = resolved.as_ref() {
            let body = decl
                .file
                .methods
                .iter()
                .find(|mm| mm.name == name && mm.descriptor == desc)
                .and_then(|m| m.code.as_ref())
                .map(|c| c.bytecode.clone());
            match body {
                Some(bc) => self.cascade_walk(decl, &bc, visited, depth),
                None => false, // abstract/native
            }
        } else {
            false
        };
        visited.remove(&key);
        crate::devirt::cascade_memo_put(key, v);
        if v {
            if let Some(decl) = resolved.as_ref() {
                crate::devirt::record_emittable(format!("{}#{name}", decl.file.this_class));
            }
        }
        v
    }

    /// Walk `bc` under full-cascade rules. Returns false on the first op that no object-aware +
    /// devirt-inlining LLVM tier could handle (non-mono/dynamic invoke, alloc, athrow, float, etc.).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn cascade_walk(
        &mut self,
        class: &Arc<LoadedClass>,
        bc: &[u8],
        visited: &mut std::collections::HashSet<u64>,
        depth: usize,
    ) -> bool {
        let mut pc = 0;
        while pc < bc.len() {
            let op = bc[pc];
            let Some(len) = crate::jit::insn_len_at(bc, pc) else { return false };
            let ok = if crate::llvm_aot::llvm_subset_op(op) {
                true
            } else if op == 0xb4 || op == 0xb5 {
                true // ANY field (primitive OR ref) — full-cascade assumes object field access
            } else if (op == 0xb6 || op == 0xb9) && pc + 2 < bc.len() {
                // Nested virtual/interface: inlinable ONLY if monomorphic (per warmup SITES) AND the
                // resolved target is itself recursively cascade-emittable.
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                let site = (Arc::as_ptr(class) as usize, idx);
                if crate::devirt::site_type_count(site) == Some(1) {
                    match class
                        .file
                        .pool
                        .member_ref(idx)
                        .map(|m| (m.owner.to_string(), m.name.to_string(), m.descriptor.to_string()))
                    {
                        Some((o, n, d)) => self.cascade_emittable(&o, &n, &d, visited, depth + 1),
                        None => false,
                    }
                } else {
                    false // polymorphic/unrecorded nested call — not inlinable
                }
            } else {
                false // alloc/dynamic/athrow/checkcast/float/etc. — outside any plausible subset
            };
            if !ok {
                return false;
            }
            pc += len;
        }
        true
    }

    /// Is every opcode in `bc` inside the LLVM numeric subset? Under RAVA_LLVM_FIELD_PROBE (8.2a),
    /// primitive getfield/putfield (int/long/double, from `owner`'s pool) also count as in-subset —
    /// a COVERAGE probe of how far object field-access support would extend the tier. The real
    /// `emit_llvm_fn` still bails on 0xb4/0xb5 (byte-identical); this only measures.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn llvm_bytecode_emittable(owner: &Arc<LoadedClass>, bc: &[u8]) -> bool {
        let field_probe = llvm_field_probe();
        let mut pc = 0;
        while pc < bc.len() {
            let op = bc[pc];
            let in_subset = crate::llvm_aot::llvm_subset_op(op)
                || (field_probe
                    && (op == 0xb4 || op == 0xb5)
                    && pc + 2 < bc.len()
                    && {
                        // Primitive-typed field (I/J/D) with a resolvable offset ⇒ a plain arena
                        // cell load/store. Ref/float fields stay out (GC/float outside the subset).
                        let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                        owner
                            .file
                            .pool
                            .member_ref(idx)
                            .map(|r| matches!(r.descriptor.as_bytes().first(), Some(b'I' | b'J' | b'D')))
                            .unwrap_or(false)
                    });
            if !in_subset {
                return false;
            }
            match crate::jit::insn_len_at(bc, pc) {
                Some(l) => pc += l,
                None => return false,
            }
        }
        true
    }

    /// Scan a method's bytecode for monomorphic invokevirtual/interface sites (per warmup
    /// `devirt::SITES`) and tally how many have an LLVM-emittable target M (= armable). Runs at
    /// JIT-compile time on the NORMAL fast boot (not the LLVM-AOT path, which never reaches
    /// worldgen). Read-only, gated by a devirt probe; the memo keeps it cheap.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn scan_devirt_sites(&mut self, class: &Arc<LoadedClass>, bc: &[u8]) {
        let (mut elig, mut armable) = (0usize, 0usize);
        let mut pc = 0;
        while pc < bc.len() {
            let op = bc[pc];
            let Some(len) = crate::jit::insn_len_at(bc, pc) else { break };
            if (op == 0xb6 || op == 0xb9) && pc + 2 < bc.len() {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                let site = (Arc::as_ptr(class) as usize, idx);
                if crate::devirt::site_type_count(site) == Some(1) {
                    elig += 1;
                    let arm = class
                        .file
                        .pool
                        .member_ref(idx)
                        .map(|m| (m.owner.to_string(), m.name.to_string(), m.descriptor.to_string()))
                        .map(|(o, n, d)| self.llvm_method_emittable(&o, &n, &d))
                        .unwrap_or(false);
                    if arm {
                        armable += 1;
                    }
                }
            }
            pc += len;
        }
        if elig > 0 {
            crate::devirt::note_llvm_eligible(elig, armable);
        }
    }

    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn build_llvm_module(&mut self, root_code: &Code, root_class: &Arc<LoadedClass>) -> Option<(String, Vec<i64>)> {
        struct Node {
            class: Arc<LoadedClass>,
            bc: Vec<u8>,
            max_locals: u32,
            fn_name: String,
        }
        // 8.1b DEVIRT SCAFFOLD (inert). A monomorphic invokevirtual/interface site the LLVM
        // tier MAY later devirtualise: `node` = the caller fn, `bc_index` = the invoke's cp
        // index, `class_key` = the sole receiver class recorded by the warmup probe.
        #[allow(dead_code)]
        struct LlvmDevirtSite {
            node: usize,
            bc_index: u16,
            class_key: u64,
            /// The target method M is itself LLVM-emittable (numeric subset). Only these can arm:
            /// the fast-path body IS `emit_llvm_fn(M)`, which bails on getfield/ref/nested-invoke.
            armable: bool,
        }
        // 8.1c will emit the LLVM class-guard here:
        //     %cid  = load i32, i32* <recv+0>          ; arena class_id
        //     %hit  = icmp eq i32 %cid, <C>            ; the monomorphic class
        //     br i1 %hit, label %fast, label %slow
        //   fast:  <inlined callee M body>  ; br %merge
        //   slow:  %r = call i64 @rava_invoke(...)     ; deopt to dynamic dispatch
        //   merge: %v = phi ...
        // INERT stub — returns None so any module containing a 0xb6/0xb9 site still bails to
        // Cranelift (byte-identical AOT). Signature frozen for 8.1c.
        #[allow(dead_code)]
        fn llvm_emit_devirt_guard(_out: &mut String, _site: &LlvmDevirtSite) -> Option<()> {
            None
        }
        let mut nodes: Vec<Node> = vec![Node {
            class: Arc::clone(root_class),
            bc: root_code.bytecode.clone(),
            max_locals: root_code.max_locals as u32,
            fn_name: "f0".into(),
        }];
        let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut callee_maps: Vec<std::collections::HashMap<u16, crate::llvm_aot::LlvmCallee>> =
            vec![std::collections::HashMap::new()];

        // BFS: discover the eligible static-call tree. Any ineligible callee → the whole module bails.
        // Devirt scaffold only walks when a probe is armed (zero overhead otherwise).
        let devirt_probe = crate::devirt::enabled() || crate::devirt::steady_enabled();
        let mut eligible: Vec<LlvmDevirtSite> = Vec::new();
        let mut wi = 0;
        while wi < nodes.len() {
            let bc = nodes[wi].bc.clone();
            let class = Arc::clone(&nodes[wi].class);
            let mut pc = 0;
            let mut resolved: Vec<(u16, crate::llvm_aot::LlvmCallee)> = Vec::new();
            while pc < bc.len() {
                let op = bc[pc];
                let len = crate::jit::insn_len_at(&bc, pc)?;
                // 8.1b: recognise virtual/interface sites. Instead of letting the emitter blindly
                // bail (it still does, below), mark monomorphic sites LLVM_Devirt_Eligible so 8.1c
                // can seed a class-guard. Pure discovery — NEVER returns None here (stays inert).
                if devirt_probe && (op == 0xb6 || op == 0xb9) {
                    let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                    let site = (Arc::as_ptr(&class) as usize, idx);
                    if crate::devirt::site_type_count(site) == Some(1) {
                        if let Some(keys) = crate::devirt::site_keys(site) {
                            if let Some(&class_key) = keys.first() {
                                // Measure armability: is the target method M LLVM-emittable? (the
                                // fast-path body would be emit_llvm_fn(M)). This is the gate for 8.1c.
                                let armable = class
                                    .file
                                    .pool
                                    .member_ref(idx)
                                    .map(|m| (m.owner.to_string(), m.name.to_string(), m.descriptor.to_string()))
                                    .map(|(o, n, d)| self.llvm_method_emittable(&o, &n, &d))
                                    .unwrap_or(false);
                                eligible.push(LlvmDevirtSite { node: wi, bc_index: idx, class_key, armable });
                            }
                        }
                    }
                }
                if op == 0xb8 {
                    let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                    let m = class.file.pool.member_ref(idx)?;
                    let (owner, mname, mdesc) =
                        (m.owner.to_string(), m.name.to_string(), m.descriptor.to_string());
                    // Numeric-only signature (I/J/D/V) — else not LLVM-eligible.
                    crate::llvm_aot::llvm_desc_args_ret(&mdesc)?;
                    let decl = self.resolve_method(&owner, &mname, &mdesc).ok().flatten()?;
                    let method = decl
                        .file
                        .methods
                        .iter()
                        .find(|mm| mm.name == mname && mm.descriptor == mdesc)?;
                    if method.access_flags & 0x0008 == 0 {
                        return None; // not static → not this tier
                    }
                    let ccode = method.code.as_ref()?;
                    let key = format!("{}#{mname}#{mdesc}", decl.file.this_class);
                    let cidx = match index.get(&key) {
                        Some(&i) => i,
                        None => {
                            let fn_name = format!("f{}", nodes.len());
                            nodes.push(Node {
                                class: Arc::clone(&decl),
                                bc: ccode.bytecode.clone(),
                                max_locals: ccode.max_locals as u32,
                                fn_name,
                            });
                            callee_maps.push(std::collections::HashMap::new());
                            let ni = nodes.len() - 1;
                            index.insert(key, ni);
                            ni
                        }
                    };
                    resolved.push((
                        idx,
                        crate::llvm_aot::LlvmCallee {
                            name: nodes[cidx].fn_name.clone(),
                            max_locals: nodes[cidx].max_locals,
                            desc: mdesc,
                        },
                    ));
                }
                pc += len;
            }
            for (cp, cal) in resolved {
                callee_maps[wi].insert(cp, cal);
            }
            wi += 1;
        }
        // 8.1b: report devirt-eligible sites in this module tree, then stay inert — the stub
        // returns None, so the emitter below still bails on 0xb6/0xb9 (byte-identical AOT).
        if devirt_probe && !eligible.is_empty() {
            // The emitter (`llvm_emit_devirt_guard`) is an inert stub returning None, so these
            // sites are recognised + tallied but never armed — the module still bails below.
            let armable = eligible.iter().filter(|s| s.armable).count();
            crate::devirt::note_llvm_eligible(eligible.len(), armable);
        }

        // Emit the module. Reserve the array-ABI consts once if ANY function touches arrays.
        let any_arrays = nodes.iter().any(|n| crate::llvm_aot::llvm_uses_arrays(&n.bc));
        let mut out = String::new();
        let mut consts: Vec<i64> = Vec::new();
        if any_arrays {
            consts.push(0);
            consts.push(0);
        }
        for (i, n) in nodes.iter().enumerate() {
            let cls = Arc::clone(&n.class);
            let ldc = |cp: u16| -> Option<crate::aot_clif::LdcConst> {
                match cls.file.pool.get(cp)? {
                    Constant::Integer(v) => Some(crate::aot_clif::LdcConst::Int(*v)),
                    Constant::Long(v) => Some(crate::aot_clif::LdcConst::Long(*v)),
                    Constant::Double(d) => Some(crate::aot_clif::LdcConst::Double(d.to_bits())),
                    _ => None,
                }
            };
            crate::llvm_aot::emit_llvm_fn(
                &mut out,
                &mut consts,
                &n.fn_name,
                i != 0, // callees are internal alwaysinline
                &n.bc,
                n.max_locals,
                &callee_maps[i],
                &ldc,
            )?;
        }
        if any_arrays {
            out.push_str("!0 = !{}\n");
            if consts.len() >= 2 {
                consts[0] = self.heap.array_desc_cell_ptr();
                consts[1] = crate::jit::array_fault_ptr();
            }
        }
        if nodes.len() > 1 {
            out.push_str("attributes #0 = { alwaysinline }\n");
        }
        Some((out, consts))
    }
}
