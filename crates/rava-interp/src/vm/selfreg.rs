//! Self-registering entries: leaf probes, receiver guards, the register ABI.

use crate::*;

#[cfg(feature = "next-gen-gc")]
fn register_abi_native_refused(vm: &Vm, class: &str) -> bool {
    vm.jit_native_served_deep(class)
        || crate::owner_bits::facts(class) & crate::owner_bits::FACT_ENGINE_SYNTHETIC != 0
}

impl Vm {
    /// 23.5 Lever-B sizing (RAVA_JIT_DIRECT_PROBE): count this static/special call as DIRECTABLE (its
    /// warmed target is a GC-leaf compiled method a machine-code direct call could serve) or FLOOR.
    /// Memoized per site so the resolve runs once. Inert / read-only.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn direct_leaf_probe(&mut self, caller_ptr: *const LoadedClass, cp: u16, _kind: u8) {
        let site = (caller_ptr as usize, cp);
        let klass = match crate::directprobe::memo_get(site) {
            Some(d) => d,
            None => {
                let d = self.compute_direct_leaf(caller_ptr, cp);
                crate::directprobe::memo_put(site, d);
                d
            }
        };
        crate::directprobe::count(klass);
    }

    /// Classify a static/special site: 2 = LEAF+compiled (no-rooting direct call), 1 = compiled but
    /// NON-leaf (a rooted direct call could serve it, like jit_direct_call), 0 = floor (uncompiled/
    /// native/not-warmed → stays on rava_invoke).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn compute_direct_leaf(&mut self, caller_ptr: *const LoadedClass, cp: u16) -> u8 {
        let site = (caller_ptr as usize, cp);
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let Some(mref) = caller.file.pool.member_ref(cp) else {
            crate::directprobe::why(0);
            return 0;
        };
        let (name, desc) = (mref.name.to_string(), mref.descriptor.to_string());
        // The interpreter-warmed target class for this site (the monomorphic static/special callee).
        let Some(Some(decl)) = crate::tlcache::call_get(site) else {
            crate::directprobe::why(1);
            return 0;
        };
        let cls = decl.file.this_class.to_string();
        if matches!(
            self.dispatch_resolve_cacheable_cls(&cls, &name, &desc),
            DispatchResolve::Compiled(f) if f & 1 == 0
        ) {
            crate::directprobe::why(3);
            return 2; // leaf+compiled
        }
        // Compiled-at-all? (would need rooting — the jit_direct_call ceiling.)
        let compiled = self
            .resolve_method(&cls, &name, &desc)
            .ok()
            .flatten()
            .and_then(|d| d.file.method(&name, &desc).and_then(|m| m.code.as_ref()).map(|c| c as *const Code as usize))
            .map(|key| matches!(self.jit_cache.get(&key), Some(jit::JitStatus::Compiled(_))))
            .unwrap_or(false);
        crate::directprobe::why(if compiled { 3 } else { 2 });
        if compiled { 1 } else { 0 }
    }

    /// 23.7 self_reg-eligibility of a resolved callee: 0=floor(uncompiled/native), 1=leaf, 2=non-leaf
    /// compiled + frame fits the deep callbuf (self_reg-eligible under ic_deep — Option A), 3=non-leaf
    /// compiled but frame TOO BIG (needs Option B or stays on the host). Read-only.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn selfreg_classify(&mut self, cls: &str, name: &str, desc: &str) -> u8 {
        let Ok(Some(decl)) = self.resolve_method(cls, name, desc) else { return 0 };
        let Some(code) = decl.file.method(name, desc).and_then(|m| m.code.as_ref()) else { return 0 };
        let key = code as *const Code as usize;
        if !matches!(self.jit_cache.get(&key), Some(jit::JitStatus::Compiled(_))) {
            return 0; // not compiled
        }
        if jit::is_gc_leaf(&code.bytecode) {
            return 1; // leaf (no rooting needed at all)
        }
        let frame = code.max_locals as usize + code.max_stack as usize;
        if frame <= jit::dispatch_callbuf_slots() { 2 } else { 3 }
    }

    /// static/special self_reg probe (pool SS), memoized per site.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn selfreg_probe_ss(&mut self, caller_ptr: *const LoadedClass, cp: u16) {
        let site = (caller_ptr as usize, cp);
        let k = match crate::selfreg::memo_get(site) {
            Some(k) => k,
            None => {
                let caller: &LoadedClass = unsafe { &*caller_ptr };
                let k = match caller.file.pool.member_ref(cp) {
                    Some(m) => {
                        let (name, desc) = (m.name.to_string(), m.descriptor.to_string());
                        match crate::tlcache::call_get(site) {
                            Some(Some(decl)) => self.selfreg_classify(&decl.file.this_class.to_string(), &name, &desc),
                            _ => 0,
                        }
                    }
                    None => 0,
                };
                crate::selfreg::memo_put(site, k);
                k
            }
        };
        crate::selfreg::count(0, k);
    }

    /// heap-Ref virtual/interface self_reg probe (pool HR), memoized per site.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn selfreg_probe_hr(&mut self, recv: i64, caller_ptr: *const LoadedClass, cp: u16) {
        if (recv as u64) >> 40 != 1 {
            return; // heap Ref only
        }
        let site = (caller_ptr as usize, cp);
        let k = match crate::selfreg::memo_get(site) {
            Some(k) => k,
            None => {
                let handle = (recv & (crate::value::REF_TAG - 1)) as u32;
                let caller: &LoadedClass = unsafe { &*caller_ptr };
                let k = match (caller.file.pool.member_ref(cp), self.heap.class_of(handle).map(|c| c.to_string())) {
                    (Some(m), Some(cls)) => {
                        let (name, desc) = (m.name.to_string(), m.descriptor.to_string());
                        self.selfreg_classify(&cls, &name, &desc)
                    }
                    _ => 0,
                };
                crate::selfreg::memo_put(site, k);
                k
            }
        };
        crate::selfreg::count(1, k);
    }

    /// Stale-receiver forensic (opt-in `RAVA_RECEIVER_GUARD`). The receiver is the
    /// traitor's stale `this` iff it is NOT an instance of the call's static owner —
    /// i.e. a `this` whose slot was reused by an UNRELATED type across a GC (a
    /// DoubleArrayList where a DiscreteVoxelShape was expected). `instance_of` answers
    /// the subtype test correctly for heap handles, arena objects AND lambdas (via
    /// `$fi`), so a legitimate subtype / functional-interface / exact-type receiver
    /// is silent — only a genuinely wrong type is logged with the traitor's identity.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn receiver_guard_check(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        receiver: &Value,
    ) {
        let (caller_class, owner, name, desc) = {
            let caller: &LoadedClass = unsafe { &*caller_ptr };
            let Some(m) = caller.file.pool.member_ref(cp) else { return };
            (
                caller.file.this_class.to_string(),
                m.owner.to_string(),
                m.name.to_string(),
                m.descriptor.to_string(),
            )
        };
        // A JDK/guava owner is served natively (its hierarchy may be incompletely
        // modelled) — skip to avoid false positives; only user-bytecode owners are
        // checked. An interface/array owner the loader can't resolve is inconclusive →
        // instance_of returns true (skip), so no false alarm there either.
        if ["java/", "javax/", "jdk/", "sun/", "com/google/"]
            .iter()
            .any(|p| owner.starts_with(p))
        {
            return;
        }
        // VALID receiver ⇔ it IS-A the static owner (covers any subtype / lambda /
        // exact type). Otherwise the `this` is the wrong type → stale.
        if crate::opcodes::instance_of(self, receiver, &owner) {
            return;
        }
        let actual = match receiver {
            Value::Ref(h) => self.heap.class_of(*h).unwrap_or("?").to_string(),
            Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                .map(|s| s.to_string())
                .unwrap_or_else(|| "?".to_string()),
            other => format!("{other:?}"),
        };
        let disp = match receiver {
            Value::Ref(h) => format!("Ref({h})"),
            Value::Obj(p) => format!("Obj@{:#x}", p.addr()),
            _ => String::new(),
        };
        let holder = match receiver {
            Value::Ref(h) => self
                .heap
                .holder_of(*h)
                .map(|(c, k, fid)| format!("HELD-BY {c} field_id={fid} kind={k}"))
                .unwrap_or_else(|| "HELD-BY <none live>".to_string()),
            _ => String::new(),
        };
        eprintln!(
            "[STALE-RECEIVER] Caller: {caller_class} Callee: {owner}.{name}{desc} \
             Expected: {owner} Actual: {actual} Recv: {disp} {holder}",
        );
    }

    /// Register-ABI arc (I1b): resolve a static (`invokestatic`) call site's target to a
    /// register-ABI entry pointer, or 1 if ineligible (native/synthetic class, unsupported
    /// ABI/body shape, non-static target, uninitialized class, or a compile refusal). The
    /// entry is immortal in the shared JIT module; the compiled caller caches this pointer
    /// in a baked per-site cell (so this resolver runs at most once per site).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_resolve_reg(&mut self, caller_ptr: *const LoadedClass, cp: u16) -> i64 {
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let Some(member) = caller.file.pool.member_ref(cp) else { return 1 };
        let (owner, name, desc) = (
            member.owner.to_string(),
            member.name.to_string(),
            member.descriptor.to_string(),
        );
        let refs_ok = jit::ref_abi_enabled();
        let Some(abi) = jit::reg_abi_desc(&desc, refs_ok) else { return 1 };
        let carries_ref = abi.carries_ref();
        let Ok(Some(decl)) = self.resolve_method(&owner, &name, &desc) else { return 1 };
        if register_abi_native_refused(self, decl.file.this_class.as_str()) {
            return 1;
        }
        let Some(method) = decl.file.method(&name, &desc) else { return 1 };
        if method.access_flags & 0x0008 == 0 {
            return 1; // must be static — a register direct call has no receiver
        }
        if !decl.initialized.load(std::sync::atomic::Ordering::Relaxed) {
            return 1; // <clinit> must have run (a direct register call skips dispatch-time init)
        }
        let Some(code) = method.code.as_ref() else { return 1 };
        // Per-method memo: see `reg_abi_cache` — recursion demands it.
        let key = code as *const Code as usize;
        if let Some(&hit) = self.reg_abi_cache.get(&key) {
            return hit;
        }
        let verdict = self.compile_reg_entry(&decl, code, &desc, carries_ref);
        // DIAGNOSTIC (RAVA_DBG_REFABI): which targets the reference register ABI actually
        // takes over, and which it refuses. Inert unless the var is set; runs once per
        // method thanks to the memo above.
        if std::env::var_os("RAVA_DBG_REFABI").is_some() {
            eprintln!(
                "[REFABI] {}.{}{} leaf={} verdict={}",
                owner,
                name,
                desc,
                jit::is_gc_leaf(&code.bytecode),
                if verdict == 1 { "REFUSED".to_string() } else { format!("{:#x}", verdict) },
            );
        }
        self.reg_abi_cache.insert(key, verdict);
        verdict
    }

    /// Compile (or refuse) one register-ABI entry. Split out so `jit_resolve_reg` is just
    /// the memo + descriptor gate.
    ///
    /// ELIGIBILITY IS THE GC PROOF OBLIGATION. A reference argument that travels in a
    /// register is only safe under one of the two conditions `jit::reg_abi_desc` documents,
    /// and this is where each is actually enforced:
    ///
    ///   * GC-LEAF body (`jit::is_gc_leaf`) — no invoke / new / ldc / static access /
    ///     instanceof / athrow / monitor op, hence no safepoint anywhere inside. Compiled
    ///     with `deep = None` (the historic I1a shape) and returned as a bare pointer; the
    ///     caller then skips its spill entirely, which is where the instructions go.
    ///
    ///   * NON-LEAF body — allowed ONLY with the Deep-IC v3 self-registering prologue, so
    ///     the entry roots its own frame (including the incoming reference args, which the
    ///     register prologue stores into that frame BEFORE the registration push) on
    ///     `frames::cells_all`. Refused unless the compile actually produced that prologue
    ///     (`jf.self_reg`): a non-leaf register entry WITHOUT it would spill references
    ///     into an unregistered stack slot and lose them at the next collection. The old
    ///     `is_prim_leaf_bc` gate existed precisely because `jit::compile` hard-codes
    ///     `deep = None` — `compile_deep` is what makes this arm expressible at all.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn compile_reg_entry(
        &mut self,
        decl: &Arc<LoadedClass>,
        code: &Code,
        desc: &str,
        carries_ref: bool,
    ) -> i64 {
        let leaf = jit::is_gc_leaf(&code.bytecode);
        // Conservative for the primitive-only arc: keep its exact historic predicate so a
        // RAVA_PRIM_ABI run is unchanged. Reference-carrying entries use `is_gc_leaf`, the
        // predicate the machine-code dispatch IC's leaf arm already trusts.
        if !carries_ref && !jit::is_prim_leaf_bc(&code.bytecode) {
            return 1;
        }
        let frame_len = code.max_locals as usize + code.max_stack as usize;
        let deep = if leaf {
            None
        } else {
            if !carries_ref {
                return 1; // primitive-only arc stays leaf-only (byte-neutral)
            }
            if !jit::ic_deep_enabled() || frame_len > jit::dispatch_callbuf_slots() {
                return 1; // no self-registering prologue available → not provable
            }
            Some(jit::DeepShape {
                args_slots: jit::desc_args_slots(desc, true),
                frame_len: (code.max_locals + code.max_stack),
                ctx_slot: self.jit_ctx_slot as *const std::sync::atomic::AtomicUsize as usize,
            })
        };
        let class_ptr = Arc::as_ptr(decl) as i64;
        // Same analysis the normal JIT driver runs for this method — without it a register
        // entry is a strictly worse compile than the memory-ABI one it replaces (see
        // `jit::compile_register_with`). `statics` stays empty on purpose: the getstatic
        // value-cell table needs the driver's reserve/rollback protocol, and it is an
        // optimisation, not a correctness input.
        let static_intrinsics = self.collect_static_intrinsics(code, decl);
        let inline_targets = self.collect_inline_targets(code, decl);
        let mut native_calls = self.collect_compiled_callee_sites(code, decl);
        let new_arena = self.build_new_arena(code, decl);
        let ctor_inlines = self.build_ctor_inlines(code, decl);
        native_calls.extend(ctor_inlines.keys().copied());
        let getter_inlines = self.build_getter_inlines(code, decl);
        let concat_shapes = self.build_concat_shapes(code, decl);
        native_calls.extend(concat_shapes.keys().copied());
        let hof_fusions = self.build_hof_fusions(code, decl, desc, true);
        let prim_arrays = if jit::arena_primitive_arrays_enabled() {
            jit::analyze_arena_primitive_arrays(code)
        } else {
            jit::ArenaPrimArrayPlan::default()
        };
        let statics = std::collections::HashMap::new();
        let pool = &decl.file.pool;
        let f = jit::compile_register_with(
            code,
            desc,
            pool,
            class_ptr,
            false,
            carries_ref,
            &mut |o, n| {
                let id = self.resolve_field_id(o, n);
                let slot = self.layout_slot(o, id);
                (id, slot)
            },
            &static_intrinsics,
            &inline_targets,
            &native_calls,
            &statics,
            &new_arena,
            &ctor_inlines,
            &concat_shapes,
            &hof_fusions,
            &getter_inlines,
            deep,
            &prim_arrays,
        );
        let dbg = std::env::var_os("RAVA_DBG_REFABI").is_some();
        let Some(jf) = f else {
            if dbg {
                eprintln!("[REFABI]   refuse: compile_register returned None");
            }
            return 1;
        };
        if deep.is_some() {
            // The prologue is what roots this entry's frame. Without it a non-leaf entry
            // is unsound, so refuse rather than ship a silent root leak.
            if !jf.self_reg {
                if dbg {
                    eprintln!("[REFABI]   refuse: non-leaf entry without the self-reg prologue");
                }
                return 1;
            }
            (jf.raw() as i64) | 1
        } else {
            let p = jf.raw() as i64;
            debug_assert!(p & 1 == 0, "JIT entry pointers must be even for the deep-bit tag");
            p
        }
    }
}

#[cfg(all(test, feature = "next-gen-gc"))]
mod tests {
    use super::register_abi_native_refused;
    use crate::{class_loader::ClassLoader, Vm};

    #[test]
    fn register_abi_refuses_native_and_synthetic_classes() {
        let vm = Vm::new(ClassLoader::new());
        assert!(register_abi_native_refused(&vm, "java/util/Collections"));
        assert!(register_abi_native_refused(&vm, crate::cache::CACHE_CLASS));
    }
}
