//! Is this loop forkable at all: batch size, tree safety, leaf identity.

use crate::*;

impl Vm {
    /// GAP-S01: the exact-dispatch batch proof for the iterator fork. Under the GIL, resolve
    /// EVERY element's runtime class through the SAME `resolve_method` walk serial dispatch
    /// uses, and admit the batch only when each distinct class selects one identical bytecode
    /// method (`Code` pointer identity). Native, abstract, unresolved, null/untyped and
    /// DIFFERING targets all reject to the serial path (`None`). The caller must still run
    /// the per-class effect + compilation proofs over every returned class. Admission reads
    /// no class/method/package/workload identity — names are dispatch inputs only.
    pub(crate) fn fork_batch_target(&mut self, elems: &[Value], name: &str, desc: &str) -> Option<ForkBatchTarget> {
        let mut classes: Vec<String> = Vec::new();
        let mut shared_code: Option<usize> = None;
        for e in elems {
            let cls = self.value_class_name(e);
            if cls.is_empty() || cls == "?" {
                return None; // null / non-object / unknown-class element
            }
            if classes.iter().any(|c| c == &cls) {
                continue; // this exact class already resolved + recorded
            }
            let decl = match self.resolve_method(&cls, name, desc) {
                Ok(Some(d)) => d,
                _ => return None, // unresolved / native / abstract → serial
            };
            let m = decl.file.method(name, desc)?;
            let code = m.code.as_ref()?; // no bytecode (native/abstract) → serial
            let key = code as *const Code as usize;
            match shared_code {
                None => shared_code = Some(key),
                Some(k) if k == key => {}
                Some(_) => return None, // differing exact targets → serial
            }
            classes.push(cls);
        }
        let code = shared_code?; // empty batch → serial
        Some(ForkBatchTarget { classes, code })
    }

    /// F3.1 FORK-EACH PROBE (RAVA_FORK_SHAPES). At a `Collection.forEach(consumer)` site, decide
    /// whether the iteration is a forkable ELEMENT-OWNERSHIP map: the consumer is an unbound
    /// instance method-ref `Element::m` (element is the private receiver) or a static `m(Element)`,
    /// whose per-element method is offloadable (devirtualised against the concrete element type;
    /// `receiver_private` = element is `this`). CURRENTLY REPORTS ONLY — the caller still runs
    /// the serial loop, so behaviour is byte-identical. This validates consumer resolution + the
    /// element-ownership gate on a LIVE boot before the GIL-free fork execution (F3.2) lands.
    /// Returns whether the site is forkable (for the future wiring).
    #[allow(dead_code)]
    pub(crate) fn fork_each_probe(&mut self, f: &Value, arr: u32, n: usize) -> bool {
        let Some(lam) = (match f {
            Value::Ref(h) => self.heap.lambda_of(*h),
            _ => None,
        }) else {
            return false;
        };
        if !lam.captured.is_empty() {
            return false; // capturing lambda — invariant captures are a later increment
        }
        // The per-element method + whether the element itself is the (private) receiver.
        let (owner_hint, recv_private) = match lam.kind {
            5 | 9 => {
                // unbound instance method-ref `Element::m` → element is `this`. GAP-S01: the
                // element class is a dispatch input, so it must hold for EVERY element — a
                // heterogeneous batch may dispatch differently per element and is not forkable
                // from one sample. All elements must share one exact runtime class.
                let e0 = self.heap.array_get(arr, 0).unwrap_or(Value::Null);
                let c0 = self.value_class_name(&e0);
                for i in 1..n {
                    let e = self.heap.array_get(arr, i).unwrap_or(Value::Null);
                    if self.value_class_name(&e) != c0 {
                        return false;
                    }
                }
                (c0, true)
            }
            6 => (lam.owner().to_string(), false), // static method-ref `m(Element)`
            _ => return false,
        };
        if owner_hint.is_empty() || owner_hint == "?" {
            return false;
        }
        let ok = self.is_offloadable_devirt(&owner_hint, &lam.name(), &lam.descriptor(), false, recv_private);
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEEN: AtomicU64 = AtomicU64::new(0);
        static FORKABLE: AtomicU64 = AtomicU64::new(0);
        let s = SEEN.fetch_add(1, Ordering::Relaxed) + 1;
        if ok {
            FORKABLE.fetch_add(1, Ordering::Relaxed);
        }
        if s <= 300 || s % 200 == 0 {
            eprintln!(
                "[FORK-EACH] n={n} elem/owner={owner_hint} m={}{} recv_private={recv_private} \
                 offloadable={ok} (forkable {}/{s})",
                lam.name(), lam.descriptor(), FORKABLE.load(Ordering::Relaxed)
            );
        }
        ok
    }

    /// Is `class.name desc`'s WHOLE bytecode call-tree safe to run on a GIL-free worker with no
    /// collection: no allocation (`new`/`newarray`/`multianewarray`), no shared write
    /// (`putstatic`), no `athrow`/monitor/indy, and every invoke target is itself fork-safe-nogc.
    /// GAP-S02: virtual/interface edges recurse over the EXACT dispatch closure of the loaded
    /// hierarchy (`virtual_dispatch_closure`), not the constant-pool owner alone; static/special
    /// edges recurse their exact owner. A native callee participates only through a worker-pure
    /// contract; anything unresolved/unenumerable → false. Cycle-guarded + depth-capped.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn method_fork_safe(&mut self, class: &str, name: &str, desc: &str, allow_alloc: bool, depth: u32, seen: &mut std::collections::HashSet<usize>) -> bool {
        if depth > 48 {
            return false;
        }
        let decl = match self.resolve_method(class, name, desc) { Ok(Some(d)) => d, _ => return false };
        let Some(code) = decl.file.method(name, desc).and_then(|m| m.code.as_ref()) else { return false };
        let key = code as *const Code as usize;
        if !seen.insert(key) {
            return true; // recursion within the candidate tree → provisionally OK
        }
        let pool = &decl.file.pool;
        let bc = &code.bytecode;
        // Collect callee refs first (avoid borrowing self while iterating decl's pool).
        let mut callees: Vec<(u8, String, String, String)> = Vec::new();
        let mut p = 0usize;
        while p < bc.len() {
            let op = bc[p];
            // Always forbid: putstatic (shared write) / indy / athrow / monitor*.
            if matches!(op, 0xb3 | 0xba | 0xbf | 0xc2 | 0xc3) {
                return false;
            }
            // NOTE (G3.3): reference stores (putfield of an L/[ field, aastore) are PERMITTED.
            // A ref store into a distinct per-iteration element is a disjoint cell write, and the
            // old→young edge it creates is found by the collector's BRUTE-FORCE remembered set
            // (minor_gc scans every tenured object's ref cells — the card table is a dry-run the
            // collector never reads, so dropped concurrent card marks are harmless). No GC runs
            // during the region (workers abort on nursery-full; main is parked), so the write +
            // its later evacuation are race-free.
            // REFERENCE-array / multianewarray always forbidden: anewarray needs the &mut box
            // table + multianewarray is nested. Object `new` (0xbb, arena TLAB bump) and PRIMITIVE
            // `newarray` (0xbc → the G3.5 concurrent parallel-array-region) are permitted only
            // with allow_alloc (G3). Without it (G1/G2a no-GC path) all allocation is forbidden.
            if matches!(op, 0xbd | 0xc5) {
                return false;
            }
            if matches!(op, 0xbb | 0xbc) && !allow_alloc {
                return false;
            }
            if matches!(op, 0xb6 | 0xb7 | 0xb8 | 0xb9) {
                let cp = ((*bc.get(p + 1).unwrap_or(&0) as u16) << 8) | *bc.get(p + 2).unwrap_or(&0) as u16;
                match pool.member_ref(cp) {
                    Some(m) => callees.push((op, m.owner.to_string(), m.name.to_string(), m.descriptor.to_string())),
                    None => return false,
                }
            }
            match jit::insn_len_at(bc, p) { Some(l) => p += l, None => return false }
        }
        // (array-alloc detection uses the separate `fork_tree_has_newarray` scan below)
        for (op, o, n, d) in callees {
            // GAP-S02: the recursion targets are the EXACT dispatch targets, never the
            // declared owner alone. Static/special edges are exact by construction;
            // virtual/interface edges must enumerate the full closure or fail.
            let targets: Vec<String> = if matches!(op, 0xb7 | 0xb8) {
                vec![o.clone()]
            } else {
                match self.virtual_dispatch_closure(&o, &n, &d) {
                    Some(ts) => ts.to_vec(),
                    None => return false, // interface/unloadable owner → not enumerable
                }
            };
            for t in targets {
                // GAP-S03: a native target participates only through its declared contract.
                // The WORKER context requires pure, allocation-free AND callback-free (a
                // worker may not touch the shared heap outside its proven regions or
                // re-enter guest code). Allocating natives (String/Integer.valueOf) stay
                // rejected here even under allow_alloc — their allocations do not go
                // through the proven worker TLAB paths.
                let flags = self.loader.get_or_load(&t).ok().map(|c| c.file.access_flags);
                if crate::native_contract::worker_pure(&t, &n, &d, flags) {
                    continue;
                }
                if !self.method_fork_safe(&t, &n, &d, allow_alloc, depth + 1, seen) {
                    return false;
                }
            }
        }
        true
    }

    /// Does the fork-safe call-tree of `class.name desc` contain a primitive `newarray` (0xbc)?
    /// Used to decide whether to open a parallel-array-region before the fork (skip the reserve
    /// for object-only allocating methods). Assumes the tree already passed `method_fork_safe`.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn fork_tree_has_newarray(&mut self, class: &str, name: &str, desc: &str, depth: u32, seen: &mut std::collections::HashSet<usize>) -> bool {
        if depth > 48 {
            return false;
        }
        let decl = match self.resolve_method(class, name, desc) { Ok(Some(d)) => d, _ => return false };
        let Some(code) = decl.file.method(name, desc).and_then(|m| m.code.as_ref()) else { return false };
        let key = code as *const Code as usize;
        if !seen.insert(key) {
            return false;
        }
        let pool = &decl.file.pool;
        let bc = &code.bytecode;
        let mut callees: Vec<(u8, String, String, String)> = Vec::new();
        let mut p = 0usize;
        while p < bc.len() {
            let op = bc[p];
            if op == 0xbc {
                return true;
            }
            if matches!(op, 0xb6 | 0xb7 | 0xb8 | 0xb9) {
                let cp = ((*bc.get(p + 1).unwrap_or(&0) as u16) << 8) | *bc.get(p + 2).unwrap_or(&0) as u16;
                if let Some(m) = pool.member_ref(cp) {
                    callees.push((op, m.owner.to_string(), m.name.to_string(), m.descriptor.to_string()));
                }
            }
            match jit::insn_len_at(bc, p) { Some(l) => p += l, None => return false }
        }
        for (op, o, n, d) in callees {
            // GAP-S02: allocation detection must OVER-approximate — a virtual edge scans
            // every closure target; an unenumerable closure conservatively counts as
            // allocating (the safety proofs reject such trees anyway).
            let targets: Vec<String> = if matches!(op, 0xb7 | 0xb8) {
                vec![o.clone()]
            } else {
                match self.virtual_dispatch_closure(&o, &n, &d) {
                    Some(ts) => ts.to_vec(),
                    None => return true,
                }
            };
            for t in targets {
                if self.fork_tree_has_newarray(&t, &n, &d, depth + 1, seen) {
                    return true;
                }
            }
        }
        false
    }

    /// Is EVERY method in the call-tree JIT-compiled? The fork runs workers over this tree; the
    /// COMPILED path (per-thread tlcache + direct calls) is worker-safe (G1–G3.5), but any cold
    /// method falls to the INTERPRETER on the worker, which races shared per-class state
    /// (site_cache), the shared gc_protect, and unhandled lambda/native paths ($Lambda ULE). So a
    /// tree with any uncompiled method is NOT worker-safe → don't fork it (stay on baseline).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn fork_tree_all_compiled(&mut self, class: &str, name: &str, desc: &str, depth: u32, seen: &mut std::collections::HashSet<usize>) -> bool {
        if depth > 48 {
            return false; // too deep to verify → treat as not-all-compiled (conservative)
        }
        let decl = match self.resolve_method(class, name, desc) { Ok(Some(d)) => d, _ => return false };
        let Some(m) = decl.file.method(name, desc) else { return false };
        let Some(code) = m.code.as_ref() else {
            // No bytecode. GAP-S03: a NATIVE (ACC_NATIVE) is worker-dispatchable ONLY
            // through a declared contract that is pure, allocation-free and callback-free
            // — arbitrary natives reject. An ABSTRACT/interface method dispatches to an
            // unverifiable runtime target → NOT OK.
            if m.access_flags & 0x0100 == 0 {
                return false;
            }
            let flags = self.loader.get_or_load(class).ok().map(|c| c.file.access_flags);
            return crate::native_contract::worker_pure(class, name, desc, flags);
        };
        let key = code as *const Code as usize;
        if !seen.insert(key) {
            return true; // already visited (recursion) → don't re-fail
        }
        if !matches!(self.jit_cache.get(&key), Some(jit::JitStatus::Compiled(_))) {
            return false; // this method is not compiled → tree not all-compiled
        }
        let pool = &decl.file.pool;
        let bc = &code.bytecode;
        let mut callees: Vec<(u8, String, String, String)> = Vec::new();
        let mut p = 0usize;
        while p < bc.len() {
            let op = bc[p];
            if matches!(op, 0xb6 | 0xb7 | 0xb8 | 0xb9) {
                let cp = ((*bc.get(p + 1).unwrap_or(&0) as u16) << 8) | *bc.get(p + 2).unwrap_or(&0) as u16;
                if let Some(m) = pool.member_ref(cp) {
                    callees.push((op, m.owner.to_string(), m.name.to_string(), m.descriptor.to_string()));
                }
            }
            match jit::insn_len_at(bc, p) { Some(l) => p += l, None => return false }
        }
        for (op, o, n, d) in callees {
            // GAP-S02: virtual/interface edges must have every closure target compiled,
            // not just the declared owner's resolution; unenumerable closures fail.
            let targets: Vec<String> = if matches!(op, 0xb7 | 0xb8) {
                vec![o.clone()]
            } else {
                match self.virtual_dispatch_closure(&o, &n, &d) {
                    Some(ts) => ts.to_vec(),
                    None => return false,
                }
            };
            for t in targets {
                if !self.fork_tree_all_compiled(&t, &n, &d, depth + 1, seen) {
                    return false;
                }
            }
        }
        true
    }

    /// Does the call-tree contain an object `new` (0xbb)? Diagnostic input for the
    /// `RAVA_FORK_TARGET_SCAN` observation report (work-character of a candidate loop).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn fork_tree_has_new(&mut self, class: &str, name: &str, desc: &str, depth: u32, seen: &mut std::collections::HashSet<usize>) -> bool {
        if depth > 48 {
            return false;
        }
        let decl = match self.resolve_method(class, name, desc) { Ok(Some(d)) => d, _ => return false };
        let Some(code) = decl.file.method(name, desc).and_then(|m| m.code.as_ref()) else { return false };
        let key = code as *const Code as usize;
        if !seen.insert(key) {
            return false;
        }
        let pool = &decl.file.pool;
        let bc = &code.bytecode;
        let mut callees: Vec<(u8, String, String, String)> = Vec::new();
        let mut p = 0usize;
        while p < bc.len() {
            let op = bc[p];
            if op == 0xbb {
                return true;
            }
            if matches!(op, 0xb6 | 0xb7 | 0xb8 | 0xb9) {
                let cp = ((*bc.get(p + 1).unwrap_or(&0) as u16) << 8) | *bc.get(p + 2).unwrap_or(&0) as u16;
                if let Some(m) = pool.member_ref(cp) {
                    callees.push((op, m.owner.to_string(), m.name.to_string(), m.descriptor.to_string()));
                }
            }
            match jit::insn_len_at(bc, p) { Some(l) => p += l, None => return false }
        }
        for (op, o, n, d) in callees {
            // GAP-S02: over-approximate across the dispatch closure (see has_newarray).
            let targets: Vec<String> = if matches!(op, 0xb7 | 0xb8) {
                vec![o.clone()]
            } else {
                match self.virtual_dispatch_closure(&o, &n, &d) {
                    Some(ts) => ts.to_vec(),
                    None => return true,
                }
            };
            for t in targets {
                if self.fork_tree_has_new(&t, &n, &d, depth + 1, seen) {
                    return true;
                }
            }
        }
        false
    }

    /// The resolved leaf method's stable `Code`-pointer key (the same key the JIT cache uses).
    /// None if it can't resolve.
    pub(crate) fn fork_leaf_key(&mut self, ecls: &str, mn: &str, md: &str) -> Option<usize> {
        let decl = self.resolve_method(ecls, mn, md).ok().flatten()?;
        let m = decl.file.method(mn, md)?;
        m.code.as_ref().map(|c| c as *const Code as usize)
    }

    /// F3.2 FORK-EACH INVOKE INTERCEPT (RAVA_FORK_SHAPES). Called at the `Collection.forEach(
    /// consumer)` invoke seam BEFORE dispatching to the receiver's (guava/JDK) bytecode. If the
    /// consumer is an offloadable element-ownership per-element method, DIVERT: drain the
    /// receiver in list order and apply the consumer (currently serially — the GIL-free parallel
    /// execution is the next increment), returning `Some(())` so the caller skips the normal
    /// bytecode. If NOT forkable (or not a Collection), return `None` so the caller runs the
    /// normal bytecode UNTOUCHED — preserving exact semantics for datapacks/registries/etc.
    /// GC-safe: roots the receiver + consumer and re-reads the forwarded copies after every
    /// GC-capable step (drain / alloc / apply).
    #[allow(dead_code)]
    pub(crate) fn try_fork_each_invoke(&mut self, recv: &Value, consumer: &Value) -> Result<Option<()>, VmError> {
        // Maps are excluded — their forEach is a 2-arg BiConsumer over key/value pairs, handled
        // by map::dispatch, and draining as a Collection would be wrong.
        match recv {
            Value::Ref(h) => {
                if matches!(self.heap.get_field_syn(*h, crate::heap::Syn::Map), Value::Int(_)) {
                    return Ok(None);
                }
            }
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => {
                if crate::arena_map::enabled() && crate::arena_map::is_arena_map(*p) {
                    return Ok(None);
                }
            }
            _ => return Ok(None),
        }
        // Cheap pre-check: only an unbound instance / static method-ref lambda with NO captures
        // is a candidate (avoids draining for arbitrary consumers). Anything else → normal path.
        let candidate = match consumer {
            Value::Ref(h) => self
                .heap
                .lambda_of(*h)
                .map(|l| l.captured.is_empty() && matches!(l.kind, 5 | 6 | 9))
                .unwrap_or(false),
            _ => false,
        };
        // Diagnostic (RAVA_FORK_TRACE): every forEach invoke reaching this intercept, with the
        // receiver class + consumer shape — to see WHICH forEach do/don't get here.
        if std::env::var_os("RAVA_FORK_TRACE").is_some() {
            let rc = self.value_class_name(recv);
            let cd = match consumer {
                Value::Ref(h) => self
                    .heap
                    .lambda_of(*h)
                    .map(|l| format!("lam kind={} {}.{}{} cap={}", l.kind, l.owner(), l.name(), l.descriptor(), l.captured.len()))
                    .unwrap_or_else(|| "ref(non-lambda)".into()),
                other => format!("{other:?}"),
            };
            eprintln!("[FORK-INV] recv={rc} consumer={cd} candidate={candidate}");
        }
        if !candidate {
            return Ok(None);
        }
        // Root the receiver + consumer for the whole (GC-capable) drain/apply.
        let base = crate::vm::thread_context::root_len(&self.gc_protect);
        crate::vm::thread_context::push_root(&mut self.gc_protect, recv.clone());
        crate::vm::thread_context::push_root(&mut self.gc_protect, consumer.clone());
        let out = (|| -> Result<Option<()>, VmError> {
            // Drain the receiver (read-only). A drain failure → run the normal bytecode.
            let recv_now =
                crate::vm::thread_context::root_index(&self.gc_protect, base);
            let elems = match self.iterate_recv(&recv_now) {
                Ok(e) => e,
                Err(_) => return Ok(None),
            };
            if elems.len() < 2 {
                return Ok(None); // tiny/empty — not worth diverting
            }
            // Materialise into a HEAP array so the collector forwards elements in place.
            let arr = self.heap.alloc_values("Ljava/lang/Object;", elems);
            let n = self.heap.array_len(arr).unwrap_or(0);
            // Element-ownership offloadability gate (samples the concrete element type).
            let consumer_now = crate::vm::thread_context::root_index(&self.gc_protect, base + 1);
            if !self.fork_each_probe(&consumer_now, arr, n) {
                return Ok(None); // not forkable → run the normal (guava) bytecode UNTOUCHED
            }
            // FORKABLE. Apply in LIST ORDER (byte-identical to guava's forEach for a pure
            // per-element method). GIL-free parallel execution is the next increment (F3.3).
            for i in 0..n {
                let e = self.heap.array_get(arr, i).unwrap_or(Value::Null);
                let c = crate::vm::thread_context::root_index(&self.gc_protect, base + 1);
                self.apply(&c, vec![e])?;
            }
            Ok(Some(()))
        })();
        crate::vm::thread_context::truncate_roots(&mut self.gc_protect, base);
        out
    }
}
