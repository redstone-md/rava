//! Turning a guest value into a string, and the constant pool loads that make them.

use crate::*;

/// ⭐**FAST STRINGS** — `RAVA_NO_FAST_STRINGS=1` opts out, and the block-wide
/// `RAVA_NO_EMIT_MORE` disables it with everything else. Covers the two string
/// questions this file asked the expensive way on hot paths: resolving a String
/// constant (an allocation PAIR per `ldc` execution) and finding an object's
/// string-carrying synthetic field (four field lookups BY NAME per `String.valueOf`).
///
/// ⚠A TWIN of this predicate lives in `nextgen::strings` and reads the same variable.
/// Not laziness: `vm::strings` is a private module of `vm`, so `nextgen` cannot see
/// this one, and the shared home for it — `flags.rs` — is owned by another change in
/// flight. Both fold `emit_more_disabled` into the same `OnceLock`, so the two always
/// agree and neither costs more than one relaxed load. Merge them into `flags.rs` when
/// that file is next open.
pub(crate) fn fast_strings_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        crate::flags::fast_natives()
            && std::env::var_os("RAVA_NO_FAST_STRINGS").is_none()
            && !crate::flags::emit_more_disabled()
    })
}

// ═══════════════ Interned `ldc` String constants ═══════════════
//
// ⭐**AN ALLOCATION PAIR PER EXECUTION, FOR A VALUE THE SPEC CALLS A CONSTANT.**
// `Value::Str` is a thin `Arc<String>`, so `RStr::from(&str)` is TWO heap blocks —
// the `String` buffer and the Arc control block — plus a `memcpy`. `ldc` ran both on
// EVERY execution naming a `CONSTANT_String`, although JVMS §5.1 specifies that
// resolving one yields the INTERNED string and that the result is CACHED in the
// run-time constant pool: a real JVM hands back the same reference forever and
// allocates nothing after the first time.
//
// The copies cost twice. They allocate, and they defeat `ref_equal`'s
// `RStr::ptr_eq` fast path — two loads of the same literal were distinct `Arc`s, so
// `==` on them fell through to a byte compare. Interning makes literal identity a
// pointer compare, which is what the JVM's own literal interning guarantees.
//
// **L2** is the process-wide intern set. `RStr` already implements `Borrow<str>`,
// `Hash` and `Eq` by CONTENT, so the set stores the `RStr` itself and keeps NO
// separate key — the shape that made the field-name interner allocate a copy of the
// very name it was caching (ledger row 448) is structurally impossible here. Read
// lock first, re-check under the write lock, exactly like `Heap::intern`.
//
// **L1** is a thread-local direct-mapped table keyed by the constant pool's own
// UTF-8 address, which is what keeps the repeat case lock-free and hash-free: the
// index is one shift and one mask, with no fold over the bytes.
//   **TOTAL TABLE SIZE: 128 slots x 16 B = 2 KB** — deliberately inside L1d. A cache
//   in this engine that grew to 14 KB measured **+6.8% WORSE** than the allocation it
//   was removing, so the slot is a bare `(key, RStr)` and nothing else.
//
// **SOUNDNESS.** The pool address is only a HINT for which slot to read. A hit is
// accepted only after comparing the STORED STRING'S CONTENT with the constant's
// bytes, so an address recycled by a later class can never hand back an earlier
// class's literal. The guard is on observable shape, never on the address alone.
const LDC_L1_SLOTS: usize = 128;

/// Resolve a String constant to the one shared `RStr` for its content.
fn ldc_intern(key: *const u8, s: &str) -> crate::value::RStr {
    use crate::value::RStr;
    std::thread_local! {
        // (pool-utf8 address, the interned constant). `key == 0` = empty slot.
        static L1: std::cell::RefCell<Vec<(usize, Option<RStr>)>> =
            std::cell::RefCell::new(vec![(0usize, None); LDC_L1_SLOTS]);
    }
    // Pool text is `String`-backed, so the low bits carry no alignment; fold the
    // whole address into the index rather than shifting bits away.
    let key = key as usize;
    let slot = (key ^ (key >> 11)) & (LDC_L1_SLOTS - 1);
    let hit = L1.with(|t| {
        let t = t.borrow();
        let e = &t[slot];
        // CONTENT-VERIFIED: an address match is not trusted on its own.
        match &e.1 {
            Some(v) if e.0 == key && v.as_str() == s => Some(v.clone()),
            _ => None,
        }
    });
    if let Some(v) = hit {
        return v;
    }
    static POOL: std::sync::OnceLock<std::sync::RwLock<FastSet<RStr>>> = std::sync::OnceLock::new();
    let pool = POOL.get_or_init(|| std::sync::RwLock::new(FastSet::default()));
    let interned = {
        let r = pool.read().unwrap_or_else(|e| e.into_inner());
        r.get(s).cloned()
    };
    let interned = match interned {
        Some(v) => v,
        None => {
            let mut w = pool.write().unwrap_or_else(|e| e.into_inner());
            // Re-check: two threads sighting the same literal must agree on ONE
            // `Arc`, or `ptr_eq` would answer "different" for the same constant.
            match w.get(s).cloned() {
                Some(v) => v,
                None => {
                    let v: RStr = s.into();
                    w.insert(v.clone());
                    v
                }
            }
        }
    };
    L1.with(|t| t.borrow_mut()[slot] = (key, Some(interned.clone())));
    interned
}

impl Vm {
    /// Is `class` a `java.util.Map`? By the type closure — which, for a class whose ancestor is
    /// a JDK class with NO bytecode here, stops AT that ancestor's name: `type_closure` records
    /// `java/util/AbstractMap` but cannot open it to learn that it implements `java/util/Map`.
    /// So `implements_iface(_, "java/util/Map")` was `false` for every `AbstractMap` subclass —
    /// gson's `LinkedTreeMap`, guava's, a guest's own — and each was refused as "not a map".
    /// `AbstractMap` IS a `Map` by definition, so its presence in the closure is the same answer.
    pub(crate) fn is_map_class(&mut self, class: &str) -> bool {
        self.implements_iface(class, "java/util/Map") || self.implements_iface(class, "java/util/AbstractMap")
    }

    /// Materialise a Map's entries. A synthetic map exposes `$map`; a real (jar)
    /// map (e.g. fastutil) is drained through its own `entrySet().iterator()`
    /// bytecode. Returns `None` if `obj` is not a map.
    pub(crate) fn drain_map(&mut self, obj: u32) -> Result<Option<Vec<(Value, Value)>>, VmError> {
        if let Value::Int(id) = self.heap.get_field_syn(obj, crate::heap::Syn::Map) {
            return Ok(Some(self.heap.map_entries(id as u32)));
        }
        let cls = self.heap.class_of(obj).unwrap_or("").to_string();
        if cls.is_empty() || !self.is_map_class(&cls) {
            return Ok(None);
        }
        // Fast path: an array-backed map (e.g. fastutil's Reference2ObjectArrayMap)
        // keeps parallel `key`/`value` arrays + a `size`. Reading them directly
        // avoids running its entrySet().iterator() bytecode — a major hotspot,
        // since block-state neighbour building drains combo maps constantly.
        // Falls through to the generic bytecode drain when this layout is absent.
        let ids = self
            .heap
            .class_of(obj)
            .map(|c| c.to_string())
            .map(|c| (self.resolve_field_id(&c, "key"), self.resolve_field_id(&c, "value"), self.resolve_field_id(&c, "size")));
        let (kid, vid, sid) = ids.unwrap_or((u32::MAX, u32::MAX, u32::MAX));
        if let (Value::Array(ka), Value::Array(va)) = (
            self.heap.get_field_hint(obj, kid, 0).0,
            self.heap.get_field_hint(obj, vid, 0).0,
        ) {
            let keys = self.heap.array_values(ka).unwrap_or_default();
            let vals = self.heap.array_values(va).unwrap_or_default();
            let n = match self.heap.get_field_hint(obj, sid, 0).0 {
                Value::Int(s) => (s.max(0) as usize).min(keys.len()).min(vals.len()),
                _ => keys.len().min(vals.len()),
            };
            return Ok(Some(keys.into_iter().take(n).zip(vals).collect()));
        }
        // ⛔**THIS TAIL USED `iterate_object`, WHICH SEES NOTHING IN A JAR COLLECTION.** It ran
        // the receiver's own `entrySet()` and then handed the resulting Set to the GENERIC drain
        // — which reads an `$elems` array and runs no `iterator()` at all. gson's
        // `LinkedTreeMap$EntrySet` has no `$elems`; it links its nodes through `next` from a
        // header sentinel. So the walk found ZERO entries in a map that answered `size() == 1`
        // one call earlier, and the `AbstractMap.equals`/`hashCode` arm built on it
        // (`map/dispatch.rs`) concluded that `{"k":1}.equals({})` — every `JsonObject` against
        // every other — was TRUE, because both drained to nothing and both hashed to 0. That is
        // the arm DFU's `ExtraCodecs.dispatchOptionalValue` consults to decide whether an
        // encoded value is the empty map (bench/CompSem.java `json.eq`).
        //
        // The call-by-call walk in `drain_entry_set` is the fix and was already written for the
        // arena branch of `drain_map_any` — its comment names this exact receiver. Both
        // representations now share it, so neither can drift into the other's bug.
        self.drain_entry_set(&Value::Ref(obj))
    }

    /// `for (e : recv.entrySet())` — the JDK's own shape, driven CALL BY CALL so a jar
    /// collection's OWN iterator bytecode does the walking. Every call resolves bytecode first
    /// and falls to the natives when the receiver is one of ours (a native map's entry view, a
    /// native iterator, a `$key`/`$value` entry).
    ///
    /// ⚠**EVERY GUEST CALL HERE CAN ALLOCATE AND THEREFORE MOVE ARENA OBJECTS**, so the drained
    /// set, the iterator and the keys/values already read ride in `gc_protect` — a root the
    /// collector forwards in place — and are read back from there. A bare Rust `Vec` of arena
    /// entries would go stale at the first `getKey` that triggered a minor collection.
    ///
    /// `Ok(Some(vec![]))` for a receiver whose `entrySet()` is null or unserved; the caller has
    /// already established that it IS a map.
    pub(crate) fn drain_entry_set(&mut self, recv: &Value) -> Result<Option<Vec<(Value, Value)>>, VmError> {
        let base = crate::vm::thread_context::root_len(&self.gc_protect);
        let _scope = crate::stream::GpScope(&mut self.gc_protect as *mut _, base);
        // gc_protect layout: [set, iterator, (entry, k, v)*] — every guest object this walk
        // holds across a guest call lives here, forwarded in place by a collection.
        let set = self.call_by_name(recv.clone(), "entrySet", "()Ljava/util/Set;", vec![])?;
        if matches!(set, Value::Null) {
            return Ok(Some(Vec::new()));
        }
        crate::vm::thread_context::push_root(&mut self.gc_protect, set);
        let it = {
            let set = crate::vm::thread_context::root_index(&self.gc_protect, base);
            self.call_by_name(set, "iterator", "()Ljava/util/Iterator;", vec![])?
        };
        if matches!(it, Value::Null) {
            return Ok(Some(Vec::new()));
        }
        crate::vm::thread_context::push_root(&mut self.gc_protect, it);
        let mut n = 0usize;
        loop {
            let it = crate::vm::thread_context::root_index(&self.gc_protect, base + 1);
            let more = self.call_by_name(it, "hasNext", "()Z", vec![])?.as_int().unwrap_or(0) != 0;
            if !more {
                break;
            }
            let it = crate::vm::thread_context::root_index(&self.gc_protect, base + 1);
            let e = self.call_by_name(it, "next", "()Ljava/lang/Object;", vec![])?;
            crate::vm::thread_context::push_root(&mut self.gc_protect, e.clone());
            let slot = crate::vm::thread_context::root_len(&self.gc_protect) - 1;
            let k = self.call_by_name(e, "getKey", "()Ljava/lang/Object;", vec![])?;
            crate::vm::thread_context::push_root(&mut self.gc_protect, k);
            let e = crate::vm::thread_context::root_index(&self.gc_protect, slot); // forwarded across getKey
            let v = self.call_by_name(e, "getValue", "()Ljava/lang/Object;", vec![])?;
            crate::vm::thread_context::push_root(&mut self.gc_protect, v);
            n += 1;
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let k = crate::vm::thread_context::root_get(
                &self.gc_protect,
                base + 2 + 3 * i + 1,
            )
            .unwrap_or(Value::Null);
            let v = crate::vm::thread_context::root_get(
                &self.gc_protect,
                base + 2 + 3 * i + 2,
            )
            .unwrap_or(Value::Null);
            out.push((k, v));
        }
        Ok(Some(out))
    }

    /// `drain_map` for ANY receiver representation. A heap handle takes `drain_map`; an arena
    /// object is an arena map, a packed ArrayMap, or a jar map drained through its own
    /// `entrySet()` bytecode — gson's `LinkedTreeMap` is the one that mattered (see
    /// `map::dispatch`'s `AbstractMap.equals` arm).
    ///
    /// Every guest call here can allocate and therefore MOVE arena objects, so the drained set
    /// elements and the keys/values already read ride in `gc_protect` (a root the collector
    /// forwards in place) and are read back from there — a bare Rust `Vec` of arena entries
    /// would go stale at the first `getKey` that triggered a minor.
    pub(crate) fn drain_map_any(&mut self, recv: &Value) -> Result<Option<Vec<(Value, Value)>>, VmError> {
        match recv {
            Value::Ref(h) => self.drain_map(*h),
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => {
                if crate::arena_map::enabled() && crate::arena_map::is_arena_map(*p) {
                    return Ok(Some(crate::arena_map::entries(*p)));
                }
                if let Some(es) = crate::map::drain_arena_array_map(self, *p) {
                    return Ok(Some(es));
                }
                let cls = self.value_class_name(recv);
                if cls.is_empty() || !self.is_map_class(&cls) {
                    return Ok(None);
                }
                // ONE walk for both representations — see `drain_entry_set`. It used to live
                // here, which is how the handle branch above kept its own (broken) copy.
                self.drain_entry_set(recv)
            }
            _ => Ok(None),
        }
    }

    /// Invoke `name` on `recv` the way `invokevirtual` would from bytecode: the receiver's own
    /// bytecode when its class declares (or inherits) one, else the native layer over the
    /// receiver's class chain. Returns `Null` for a void or unserved call.
    pub(crate) fn call_by_name(
        &mut self,
        recv: Value,
        name: &str,
        desc: &str,
        mut args: Vec<Value>,
    ) -> Result<Value, VmError> {
        let cls = self.value_class_name(&recv);
        if cls.is_empty() {
            return Ok(Value::Null);
        }
        let mut locals = Vec::with_capacity(args.len() + 1);
        locals.push(recv);
        locals.append(&mut args);
        if let Some(decl) = self.resolve_method(&cls, name, desc)? {
            if decl.file.method(name, desc).map(|m| m.code.is_some()).unwrap_or(false) {
                return Ok(self.invoke(decl, name, desc, locals)?.unwrap_or(Value::Null));
            }
        }
        let chain = self.super_chain(&cls);
        #[cfg(feature = "next-gen-gc")]
        {
            Ok(crate::natives::try_invoke_chain(self, &chain, None, &cls, name, desc, &locals)?
                .flatten()
                .unwrap_or(Value::Null))
        }
        #[cfg(not(feature = "next-gen-gc"))]
        {
            for cn in chain.iter() {
                if let Some(r) = crate::natives::try_invoke(self, cn, name, desc, &locals)? {
                    return Ok(r.unwrap_or(Value::Null));
                }
            }
            Ok(Value::Null)
        }
    }

    /// Resolve an `ldc`-style constant in `class`'s pool to a stack value.
    /// ⚠**`#[inline(never)]` DELIBERATELY** — row 397. At opt-level 3 LLVM folds this into
    /// `opcodes::run`, and an inlined body's locals are reserved by `run`'s PROLOGUE, which
    /// EVERY interpreted call pays before the callee's first opcode. `run` measured a **2136-byte**
    /// stack frame and a framed call ~6.7 KB across the path, against a measured frame cost of
    /// **160 ns vs HotSpot's 7.2**. Keeping this out of line trades a call on a cold path for a
    /// smaller prologue on the hot one.
    #[inline(never)]
pub(crate) fn ldc(&mut self, class: &LoadedClass, index: u16) -> Result<Value, VmError> {
        match class.file.pool.get(index) {
            Some(Constant::Integer(v)) => Ok(Value::Int(*v)),
            Some(Constant::Float(v)) => Ok(Value::Float(*v)),
            Some(Constant::Long(v)) => Ok(Value::Long(*v)),
            Some(Constant::Double(v)) => Ok(Value::Double(*v)),
            Some(Constant::String { utf8_index }) => {
                let s = class
                    .file
                    .pool
                    .utf8(*utf8_index)
                    .ok_or(VmError::BadConstant(index))?;
                if fast_strings_enabled() {
                    // One shared `RStr` per literal, for the process — see `ldc_intern`.
                    return Ok(Value::Str(ldc_intern(s.as_ptr(), s)));
                }
                Ok(Value::Str(s.into()))
            }
            // Class literal (e.g. `String.class`, `String[].class`) → Class object.
            Some(Constant::Class { name_index }) => {
                // Borrowed, not `to_string()`d: `class` is a distinct borrow from
                // `self`, so the pool's own text can be handed straight to
                // `make_class` — which interns the mirror by name anyway, so the
                // owned copy was allocated only to be looked up and dropped.
                let name = class
                    .file
                    .pool
                    .utf8(*name_index)
                    .ok_or(VmError::BadConstant(index))?;
                Ok(self.make_class(name))
            }
            _ => Err(VmError::BadConstant(index)),
        }
    }

    /// True when `decl`'s `toString()` is EXACTLY the accessor that
    /// `java/lang/runtime/ObjectMethods.bootstrap` synthesises for a record —
    /// `aload_0; invokedynamic; areturn`, nothing else. Rava serves that call site
    /// itself (`opcodes::record_object_method`): it reads the receiver's own final
    /// components and builds ONE fresh String, publishing nothing. That is the same
    /// "pure-alloc expression" shape the offload analyzer already models for
    /// `StringConcatFactory.makeConcatWithConstants`; every OTHER bootstrap keeps a
    /// hard reject there, so a record's `toString` was previously unprovable and the
    /// per-call stringify memo could never cache one. On a DAG-shaped value graph
    /// that costs O(flattened text) instead of O(distinct nodes).
    ///
    /// The transitive side condition (each REFERENCE component's own `toString` must
    /// also be pure) is checked DYNAMICALLY by the caller with the impurity mark, over
    /// the actual component values — not statically over the declared types.
    pub(crate) fn record_synth_tostring(&mut self, decl: &Arc<LoadedClass>) -> bool {
        use std::cell::RefCell;
        thread_local! {
            static CACHE: RefCell<std::collections::HashMap<usize, bool>> =
                RefCell::new(std::collections::HashMap::new());
        }
        let key = Arc::as_ptr(decl) as usize;
        if let Some(hit) = CACHE.with(|c| c.borrow().get(&key).copied()) {
            return hit;
        }
        let verdict = (|| {
            let code = decl
                .file
                .method("toString", "()Ljava/lang/String;")?
                .code
                .as_ref()?;
            let bc = &code.bytecode;
            // aload_0 (2a); invokedynamic (ba) idx idx 00 00; areturn (b0).
            if bc.len() != 7 || bc[0] != 0x2a || bc[1] != 0xba || bc[4] != 0 || bc[5] != 0 || bc[6] != 0xb0
            {
                return None;
            }
            let idx = u16::from_be_bytes([bc[2], bc[3]]);
            let (bootstrap, name, _) = decl.file.pool.invoke_dynamic(idx)?;
            if name != "toString" {
                return None;
            }
            let bm = decl.file.bootstrap_methods.get(bootstrap as usize)?;
            let handle = decl.file.pool.method_handle(bm.handle_index)?;
            (handle.owner == "java/lang/runtime/ObjectMethods" && handle.name == "bootstrap")
                .then_some(())
        })()
        .is_some();
        CACHE.with(|c| c.borrow_mut().insert(key, verdict));
        verdict
    }

    /// Render a value as `String.valueOf` would: primitives directly, objects
    /// via their `toString()` (real bytecode, else the JDK native). Synthetic
    /// string-carrying objects ($str/$path/$name/$sb) short-circuit.
    pub(crate) fn value_to_string(&mut self, v: &Value) -> Result<String, VmError> {
        // ⭐**THE ANSWER THAT NEEDS NO BOOKKEEPING, ASKED FIRST.** Rendering a value
        // that IS ALREADY A STRING is the most frequent thing this function is asked
        // to do — `String.valueOf(s)`, every non-constant concat operand — and it sat
        // BEHIND `StringifyMemo::enter` and `memo.lookup()`. An arena string is
        // memo-KEYED (`memo_key` maps `Value::Obj` to `MemoKey::Arena`), so that
        // lookup was two full `HashMap` probes — the call-scoped cache and the
        // cross-call `PERSIST` map — to answer a question whose answer is the
        // object's own bytes. This is the same hoist the token-probe comment below
        // records: the memo made the RENDERING cheap and left the BOOKKEEPING at
        // O(edges).
        //
        // Nothing is skipped by returning early. No arena-string render is ever
        // STORED (this arm returns before any `store_if_proven`), so the probe could
        // only ever hit on an entry left by a DIFFERENT object that once occupied the
        // address — which would be a wrong answer, not a saved one. And `DEPTH` stays
        // balanced because the guard is never constructed: a nested call leaves the
        // outer frame's depth alone, and the next outermost frame still clears the
        // cache on entry.
        //
        // The non-keyable forms (primitives, `Value::Str`, null) fall out for the
        // weaker reason: `memo_key` returns `None` for them, so `lookup` cannot hit,
        // and entering only bumped a depth counter and, on the outermost frame,
        // cleared a cache nobody consulted.
        if fast_strings_enabled() {
            match v {
                Value::Ref(_) => {} // memo-keyed and genuinely needs the render below
                #[cfg(feature = "next-gen-gc")]
                Value::Obj(p) => {
                    if crate::nextgen::is_arena_string(*p) {
                        return Ok(String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p))
                            .into_owned());
                    }
                }
                Value::Null => return Ok("null".to_string()),
                other => return Ok(other.display()),
            }
        }
        let memo = stringify_memo::StringifyMemo::enter(v);
        if let Some(rendered) = memo.lookup() {
            return Ok(rendered);
        }
        // Arena object: no synthetic $-fields, so run its real toString() bytecode
        // (or a JDK native) over the Obj receiver — e.g. MinecraftKey.toString()
        // reads its namespace/path arena cells and yields "minecraft:ambient.cave".
        // ArenaString: its content IS the string — no toString bytecode to run.
        #[cfg(feature = "next-gen-gc")]
        if let Value::Obj(p) = v {
            if crate::nextgen::is_arena_string(*p) {
                return Ok(String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p)).into_owned());
            }
        }
        #[cfg(feature = "next-gen-gc")]
        if let Value::Obj(p) = v {
            let sig = "()Ljava/lang/String;";
            // GAP-S06: the purity proof ranges over the loaded hierarchy — cache the
            // rendered string only if executing toString loaded no new class (epoch
            // unchanged), otherwise the proof may have missed a new override.
            let admission_epoch = self.loader.epoch();
            let probe_first = key_intern::probe_first();
            // Tag read BEFORE anything else on this path, so no guest code can run
            // between the read and the probe. Reading it early can only RETIRE entries
            // early (a collection during the prologue now also invalidates the later
            // `token_memo_put`), which is the conservative direction. The LOADER half
            // belongs to the hoist and is therefore only armed with it: retiring the
            // memo on a class load is what makes "an entry witnesses a proven-pure body"
            // exact, and with the flag off the tag stays the historic GC epoch alone.
            let token_epoch = key_intern::TokenEpoch {
                gc: crate::nextgen::gc_epoch(),
                loader: if probe_first { admission_epoch } else { 0 },
            };
            // HOISTED TOKEN PROBE (`RAVA_TOKEN_PROBE_FIRST`). The lookup below is O(1)
            // and answers "this node's subtree is already derived" — but historically it
            // sat AFTER the class-name `to_string`, the transitive purity proof and the
            // name-keyed `resolve_method`, so every already-derived DAG edge paid all
            // three to discover that it needed none of them. The memo made the RENDERING
            // O(distinct nodes) and left the BOOKKEEPING at O(edges).
            //
            // Probing first is exact, not optimistic: an entry exists only where
            // `token_memo_put` accepted one, and that call is guarded by the full
            // `should_substitute` — so the entry itself witnesses that this object's body
            // was proven pure under this loader epoch. Only the CONTEXT half (in a key
            // derivation, nested, pure chain — three thread-local `Cell` reads) has to be
            // rechecked here, and the body half is recomputed on a miss exactly as before.
            // `probe_first` FIRST, deliberately: key interning is default-ON, so
            // `substitution_context()` is reachable on every arena render, and evaluating
            // it unconditionally put three thread-local `Cell` reads on a hot path in BOTH
            // A/B arms — for a value that is dead unless the flag is set. The flag's own
            // measurement was partly pricing that. Short-circuit order is the whole fix.
            let context = probe_first && key_intern::substitution_context();
            if context {
                if let Some(hit) = key_intern::token_memo_get(p.addr(), token_epoch) {
                    return Ok(hit);
                }
            }
            let actual = crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                .map(|a| a.to_string())
                .unwrap_or_else(|| "java/lang/Object".to_string());
            let probe = residue_probe::Scope::enter(&actual);
            // Only run the transitive purity proof if its answer is used: the memo
            // could store the result, or a key derivation may substitute this render
            // for a token. Otherwise it is dead work (the common native_depth==0
            // String.valueOf path). Behaviour-identical either way.
            let statically_pure = (memo.is_active() || key_intern::in_key_scope())
                && self.is_offloadable(&actual, "toString", sig, false);
            let decl = self.resolve_method(&actual, "toString", sig)?;
            let record_accessor = !statically_pure
                && record_tostring_pure_enabled()
                && decl.as_ref().is_some_and(|d| {
                    let d = Arc::clone(d);
                    self.record_synth_tostring(&d)
                });
            let body_pure = statically_pure || record_accessor;
            // Recomputed rather than reusing `context`: the prologue between the two can
            // load a class, and a class load runs guest code. The guards that own these
            // thread-locals are RAII and restore them, so the two agree — but this path
            // is the one that decides whether a token may escape, and it costs three
            // `Cell` reads to not depend on that argument.
            let substitute = key_intern::should_substitute(body_pure);
            // A token already derived for this object in this epoch skips the whole
            // subtree render — the step that takes the derivation from O(roots x subtree)
            // down to O(distinct nodes). Already probed above when hoisted.
            if substitute && !probe_first {
                if let Some(hit) = key_intern::token_memo_get(p.addr(), token_epoch) {
                    return Ok(hit);
                }
            }
            let render = key_intern::RenderGuard::enter(body_pure);
            let mark = stringify_memo::impure_mark();
            let result = match decl {
                Some(decl) => self.invoke(decl, "toString", sig, vec![v.clone()])?,
                None => {
                    let mut r = None;
                    for cn in self.super_chain(&actual).iter() {
                        if let Some(ret) =
                            natives::try_invoke(self, cn, "toString", sig, &[v.clone()])?
                        {
                            r = ret;
                            break;
                        }
                    }
                    r
                }
            };
            let rendered = match result {
                Some(Value::Str(s)) => s.to_string(),
                Some(Value::Ref(rh)) => self.value_to_string(&Value::Ref(rh))?,
                Some(other) => other.display(),
                None => format!("{}@arena", actual.replace('/', ".")),
            };
            drop(render);
            probe.finish(rendered.len());
            let proven_pure =
                statically_pure || (record_accessor && stringify_memo::impure_mark() == mark);
            if !proven_pure {
                stringify_memo::note_impure();
            }
            let out = if substitute && key_intern::worth_interning(&rendered) {
                key_intern::token(&rendered)
            } else {
                rendered
            };
            // Memoise whatever this node produces, token or text: the size threshold
            // only decides whether the GLOBAL intern table is worth locking, while the
            // epoch memo is what skips the render — and the render is the expensive part.
            if substitute && crate::nextgen::gc_epoch() == token_epoch.gc {
                key_intern::token_memo_put(p.addr(), token_epoch, &out);
            }
            memo.store_if_proven(proven_pure && self.loader.epoch() == admission_epoch, &out);
            return Ok(out);
        }
        let h = match v {
            Value::Ref(h) => *h,
            Value::Null => return Ok("null".to_string()),
            other => return Ok(other.display()),
        };
        // A `java/lang/Class` mirror carries its binary name in `$name`, and the
        // shortcut below would hand that BACK as its toString — so concatenating a
        // Class printed "java/lang/String" (slashes and all) where the JVM prints
        // "class java.lang.String". Class has a real toString; render it.
        if self.heap.class_of(h) == Some("java/lang/Class") {
            if let Value::Str(n) = self.heap.get_field_syn(h, crate::heap::Syn::Name) {
                let n = n.to_string();
                return Ok(crate::reflect::class_display_name(self, &n));
            }
        }
        // ⭐**FOUR FIELD LOOKUPS BY NAME, ON THE `String.valueOf` PATH.**
        // `get_field(h, "$str")` runs `field_id_existing` — the interner's L1 probe and,
        // on a miss, a hash of the name — for a question with a CONSTANT answer. `Syn`
        // exists precisely so a hot path names a synthetic field by discriminant: its
        // id lives in a thread-local array indexed by the discriminant, so the four
        // name folds become four array reads. The reload phase's field-name interner
        // cluster is 11.6% of that phase, and this path is walked for every object
        // rendered into a string.
        //
        // Same fields, same order, same `$sb`-only char-array arm — only the way the
        // id is reached changes.
        if fast_strings_enabled() {
            use crate::heap::Syn;
            for (i, syn) in [Syn::Str, Syn::Path, Syn::Name, Syn::Sb].into_iter().enumerate() {
                match self.heap.get_field_syn(h, syn) {
                    Value::Str(s) => return Ok(s.to_string()),
                    // i == 3 is `$sb` — a StringBuilder's char[] backing store.
                    Value::Array(a) if i == 3 => return Ok(self.heap.chars_array_to_string(a)),
                    _ => {}
                }
            }
        } else {
            for f in ["$str", "$path", "$name", "$sb"] {
                match self.heap.get_field(h, f) {
                    Value::Str(s) => return Ok(s.to_string()),
                    Value::Array(a) if f == "$sb" => return Ok(self.heap.chars_array_to_string(a)),
                    _ => {}
                }
            }
        }
        let actual = self
            .heap
            .class_of(h)
            .unwrap_or("java/lang/Object")
            .to_string();
        let sig = "()Ljava/lang/String;";
        // GAP-S06: cache only under an unmoved hierarchy epoch (see the arena arm above).
        let admission_epoch = self.loader.epoch();
        let probe = residue_probe::Scope::enter(&actual);
        // Dead-work elimination: skip the purity proof when neither the memo nor a
        // key derivation would use it (see the arena arm above) — behaviour-identical.
        let statically_pure = (memo.is_active() || key_intern::in_key_scope())
            && self.is_offloadable(&actual, "toString", sig, false);
        let decl = self.resolve_method(&actual, "toString", sig)?;
        let record_accessor = !statically_pure
            && record_tostring_pure_enabled()
            && decl.as_ref().is_some_and(|d| {
                let d = Arc::clone(d);
                self.record_synth_tostring(&d)
            });
        let body_pure = statically_pure || record_accessor;
        let substitute = key_intern::should_substitute(body_pure);
        let render = key_intern::RenderGuard::enter(body_pure);
        let mark = stringify_memo::impure_mark();
        let result = match decl {
            Some(decl) => self.invoke(decl, "toString", sig, vec![Value::Ref(h)])?,
            None => {
                let mut r = None;
                for cn in self.super_chain(&actual).iter() {
                    if let Some(ret) =
                        natives::try_invoke(self, cn, "toString", sig, &[Value::Ref(h)])?
                    {
                        r = ret;
                        break;
                    }
                }
                r
            }
        };
        let rendered = match result {
            Some(Value::Str(s)) => s.to_string(),
            Some(other) => other.display(),
            None => format!("{}@{:x}", actual.replace('/', "."), h),
        };
        drop(render);
        probe.finish(rendered.len());
        let proven_pure =
            statically_pure || (record_accessor && stringify_memo::impure_mark() == mark);
        if !proven_pure {
            stringify_memo::note_impure();
        }
        let out = if substitute && key_intern::worth_interning(&rendered) {
            key_intern::token(&rendered)
        } else {
            rendered
        };
        // CROSS-CALL admission: if every instance field of `h`'s concrete class is ACC_FINAL, the
            // verifier forbids any `putfield` to it outside `<init>`, so this render is fixed for the
            // object's lifetime and may outlive the call-scoped cache. `Direction` alone is rendered
            // 219,142 times a boot for six distinct instances, at 952 ns each.
            let immutable = crate::map::class_all_final_word(self, h);
            memo.store_if_proven_ext(
                proven_pure && self.loader.epoch() == admission_epoch,
                immutable,
                &out,
            );
        Ok(out)
    }
}
