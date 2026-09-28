//! Map and BitSet operations a compiled body calls into.

use crate::*;

impl Vm {
    /// java.util.BitSet fast-path receiver guard (compiled-code callbacks): the
    /// cell must be a heap Ref whose EXACT class is java/util/BitSet (interned
    /// name-pointer compare — a subclass falls back to the general dispatch).
    #[cfg(feature = "next-gen-gc")]
    /// Publish the interned `java/util/ArrayList` class word for the inline `get` guard.
    ///
    /// Same shape and same reason as the BitSet word below: the guard compares the receiver's
    /// class key — the first word of its `Arc<str>`, which is what `emit_heap_ref_class_key`
    /// reads out of `[box_ptr + Object::class_offset]` — against this, and refuses while it is
    /// zero. Called from `fu_fastpath` when it first classifies an ArrayList receiver, i.e.
    /// from the very slow arm the guard falls back to, so nothing has to order the two.
    pub(crate) fn publish_arraylist_word(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        if crate::jit::ARRAYLIST_CLASS_WORD.load(Relaxed) != 0 {
            return;
        }
        let arc = self.heap.intern_class("java/util/ArrayList");
        let word = unsafe { *(&arc as *const std::sync::Arc<str> as *const i64) };
        crate::jit::ARRAYLIST_CLASS_WORD.store(word, Relaxed);
        // The BARE `$elems` id — the one the natives actually store under. `field_id` is the
        // very call they make, so this cannot answer a different id; `field_id_opt` could
        // answer `None` here and leave the arm unemittable forever.
        crate::jit::ELEMS_FID.store(self.heap.field_id("$elems") as i64 + 1, Relaxed);
        // ⛔**AND THE `DoubleArrayList` WORD IS NOT PUBLISHED HERE, DELIBERATELY.** Interning the
        // name at construction produces an `Arc<str>` that is NOT the one the loader later puts
        // in the objects' headers, so the guard compared two unequal words and missed
        // 16,858,961 times out of 16,858,961 — with `dg_cls` proving the compare was reached
        // every time. Worse, storing a wrong-but-NON-ZERO value here made the correct publisher
        // (`fu_fastpath`, which takes the word off a live receiver) skip itself, because it only
        // fires while the word is still zero. Three fixes failed for that reason alone.
        //
        // ⇒ The word now has exactly one publisher, and it reads it from an object that exists.
    }

    pub(crate) fn jit_bitset_recv(&mut self, recv: i64) -> Option<u32> {
        if (recv as u64) >> 32 != (crate::value::REF_TAG as u64) >> 32 {
            return None;
        }
        let h = recv as u32;
        if self.bitset_name_ptr == 0 {
            let arc = self.heap.intern_class("java/util/BitSet");
            self.bitset_name_ptr = arc.as_ptr() as usize;
            // Publish the class KEY (first word of the interned Arc<str> = pointer-to-
            // ArcInner) for the inline `BitSet.get` fast arm's exact-class guard. This is
            // exactly what `Heap::object_class_word` / the machine-code heap-Ref PIC read
            // from `[box_ptr + Object::class_offset]` (a clone shares the ArcInner). Stored
            // once; read at runtime — never baked, so it can't perturb any hash.
            let word = unsafe { *(&arc as *const std::sync::Arc<str> as *const i64) };
            crate::jit::BITSET_CLASS_WORD.store(word, std::sync::atomic::Ordering::Relaxed);
        }
        (self.heap.class_name_ptr(h) == self.bitset_name_ptr).then_some(h)
    }

    /// The `$bits` backing array of a guarded BitSet receiver (None = no words
    /// written yet — a get treats that as all-zero, a set falls back to grow).
    #[cfg(feature = "next-gen-gc")]
    /// The `$bits` backing array of a guarded BitSet receiver (None = no words
    /// written yet — a get treats that as all-zero, a set falls back to grow).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_bitset_arr(&self, h: u32) -> Option<u32> {
        let id = self.heap.field_id_opt("$bits")?;
        match self.heap.get_field_hint(h, id, 0).0 {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The `$bits` backing array of a BitSet receiver in EITHER ENCODING.
    ///
    /// ⭐**Ledger row 324.** `jit_bitset_recv` requires `recv >> 32 == REF_TAG >> 32` — a HEAP
    /// HANDLE. An arena-born BitSet is a raw 0b00 pointer, so every one of these three natives
    /// refused it and fell through to `rava_invoke` and the full owner cascade. Measured: with
    /// `RAVA_ARENA_SYNTH` on, `java/util/BitSet` goes from **497,501 to 10,451,541 native serves
    /// a boot — ×21, and 65% of that flag's whole +52% regression.** The refusal was never in the
    /// native's own logic (`synth_get` takes both encodings by construction); it was in the
    /// receiver GUARD in front of it, here and in the emitter.
    ///
    /// Outer `None` = not a BitSet we can serve (fall back to general dispatch, which is always
    /// correct). Inner `None` = a BitSet with no `$bits` written yet, which a `get` reads as the
    /// empty set — the same split `jit_bitset_recv` + `jit_bitset_arr` had.
    #[cfg(feature = "next-gen-gc")]
    fn jit_bitset_words(&mut self, recv: i64) -> Option<Option<u32>> {
        if let Some(h) = self.jit_bitset_recv(recv) {
            return Some(self.jit_bitset_arr(h));
        }
        // ARENA receiver. `is_arena_obj_addr` is the same test the dual field emitters use; the
        // class is compared by ARENA CLASS ID, resolved once, so the per-call cost is an integer
        // compare rather than the name walk `jit_bitset_recv` avoids on the handle side.
        if !crate::nextgen::arena_synth_enabled() {
            return None;
        }
        if recv <= 0 || !crate::nextgen::is_arena_obj_addr(recv as u64) {
            return None;
        }
        let p = crate::arena::ArenaPtr::new(recv as *mut u8)?;
        let cid = crate::nextgen::arena_class_id(p);
        if self.bitset_arena_cid == u32::MAX {
            // Resolve lazily and remember the ANSWER for this cid either way, so a miss costs
            // one name compare per distinct class id and not one per call.
            match crate::nextgen::class_name(cid).as_deref() {
                Some("java/util/BitSet") => self.bitset_arena_cid = cid,
                _ => return None,
            }
        }
        if cid != self.bitset_arena_cid {
            return None;
        }
        let id = self.heap.field_id_opt("$bits")?;
        // A cell the layout does not have would be an out-of-bounds read; absence is the empty
        // set, which is what a never-written `$bits` reads as on the handle side too.
        if crate::nextgen::slot_of(cid, id).is_none() {
            return Some(None);
        }
        Some(match crate::opcodes::arena_cells::arena_getfield(p, id) {
            Value::Array(a) => Some(a),
            _ => None,
        })
    }

    /// BitSet.get(I)Z from compiled code. `None` = fall back to general dispatch.
    #[cfg(feature = "next-gen-gc")]
    /// BitSet.get(I)Z from compiled code. `None` = fall back to general dispatch.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_bitset_get(&mut self, recv: i64, idx: i32) -> Option<i64> {
        let words = self.jit_bitset_words(recv)?;
        if idx < 0 {
            return None; // IndexOutOfBoundsException path — general dispatch throws
        }
        let bit = match words {
            Some(a) => match self.heap.array_get(a, idx as usize / 64) {
                Some(Value::Long(w)) => (w as u64 >> (idx as usize % 64)) & 1,
                Some(_) => return None, // wrong element shape — never ours
                None => 0,              // past the words → false (JVM semantics)
            },
            None => 0, // no $bits yet → empty set → false
        };
        Some(jit::pack_local(&Value::Int(bit as i32)))
    }

    /// BitSet.set(I[Z])V / clear(I)V from compiled code — in-place word write.
    /// A write past the current words needs growth → general dispatch.
    #[cfg(feature = "next-gen-gc")]
    /// BitSet.set(I[Z])V / clear(I)V from compiled code — in-place word write.
    /// A write past the current words needs growth → general dispatch.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_bitset_put(&mut self, recv: i64, idx: i32, val: bool) -> Option<i64> {
        let words = self.jit_bitset_words(recv)?;
        if idx < 0 {
            return None;
        }
        let w = idx as usize / 64;
        // A clear past the words is a no-op (bits there are already 0) — but the
        // general path also handles it fine; only serve the in-bounds write here.
        let a = words?;
        let cur = match self.heap.array_get(a, w) {
            Some(Value::Long(v)) => v as u64,
            _ => return None, // OOB (growth needed) or wrong shape
        };
        let mask = 1u64 << (idx as usize % 64);
        let nw = if val { cur | mask } else { cur & !mask };
        if nw != cur {
            self.heap.array_set(a, w, Value::Long(nw as i64));
        }
        Some(0) // void
    }

    /// BitSet.isEmpty()Z from compiled code.
    #[cfg(feature = "next-gen-gc")]
    /// BitSet.isEmpty()Z from compiled code.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_bitset_isempty(&mut self, recv: i64) -> Option<i64> {
        let empty = match self.jit_bitset_words(recv)? {
            Some(a) => {
                let n = self.heap.array_len(a)?;
                (0..n).all(|i| matches!(self.heap.array_get(a, i), Some(Value::Long(0))))
            }
            None => true,
        };
        Some(jit::pack_local(&Value::Int(empty as i32)))
    }

    /// COLLECTION INTRINSIC (next-gen-gc): `HashMap.put(k,v)` straight from compiled
    /// code — decode the cells, derive the key, and store via the native map table,
    /// skipping `rava_invoke` → `jit_call` → the synthetic frame + method
    /// re-resolution + the map.rs dispatch routing. Returns the boxed previous value.
    #[cfg(feature = "next-gen-gc")]
    /// COLLECTION INTRINSIC (next-gen-gc): `HashMap.put(k,v)` straight from compiled
    /// code — decode the cells, derive the key, and store via the native map table,
    /// skipping `rava_invoke` → `jit_call` → the synthetic frame + method
    /// re-resolution + the map.rs dispatch routing. Returns the boxed previous value.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_map_put(&mut self, recv: u32, key_cell: i64, val_cell: i64) -> i64 {
        let key = jit::unbox_ref(self, key_cell, "");
        let val = jit::unbox_ref(self, val_cell, "");
        // TRIPWIRE: a NON-NULL cell decoding to Null = a dead box index / corrupt
        // cell — the silent way a map swallows a live value (th=128: a schema
        // supplier stored as null → NPE at the later taggedChoice get). Fatal-class
        // anomaly, so an unconditional print is fine (never fires when healthy).
        if val_cell != 0 && matches!(val, Value::Null) {
            eprintln!(
                "[MAPPUT-NULL] val_cell={val_cell:#x} decoded Null (key={key:?}) — dead box index?"
            );
        }
        self.heap.put_field_syn(recv, crate::heap::Syn::SKey, Value::Null);
        let id = crate::map::map_id(self, recv);
        // FAST PATH (the `HashMap<Integer,_>` autobox worst-path, bench/Collections): an int key
        // hashes via `MapKey::Int` with ZERO allocation in `map_key` (map.rs) — so nothing can
        // move — skipping the per-put `Object[]` rooting shield entirely. Consistent with the
        // put_field/map_id window above, which already trusts no GC-move before the store. Kills
        // the 150k-array-per-run churn that made `map.put` allocation-bound.
        // ⭐**AND A `Value::Str` KEY IS EXACTLY AS SHIELD-FREE AS AN `Int` ONE — SAME PROOF.**
        // The `Object[]` below exists for ONE reason: `map_key` may re-enter guest code (a record
        // key's `toString`), which allocates and therefore MOVES an arena `key`/`val` held across
        // it. `map_key_inner` answers a `Value::Str` from `keyroot::string_first` — the third
        // statement in that function, ahead of the map-class resolve, the identity rule, the combo
        // hash and the structural renderer — and that arm only formats Rust-side bytes
        // (`tagged_exact` into a `Box<str>`). No guest frame is entered, nothing is allocated on
        // the guest heap, nothing can collect, so there is nothing to root. (The object census
        // above it matches only `Obj`/`Ref`, so a `Str` never reaches `value_class_name` either.)
        //
        // Was one guest `Object[]` allocation PER STRING-KEYED PUT — and this jar keys by string
        // everywhere: registries, resource locations, JSON — is zero. Same argument, and the same
        // measured shape, as the `Int` arm that removed the 150k-array-per-run churn.
        // Kill switch RAVA_NO_FAST_ITER; refused ⇒ the shield is taken exactly as before.
        if matches!(key, Value::Int(_))
            || (matches!(key, Value::Str(_)) && super::iterate::fast_iter())
        {
            let mk = match crate::map::map_key(self, recv, &key) {
                Ok(mk) => mk,
                Err(e) => { self.pend_vm_err(e); return 0; }
            };
            let prev = self.heap.map_put(id, mk, key, val).unwrap_or(Value::Null);
            return jit::box_ref(self, &prev);
        }
        // `map_key` runs `value_to_string` → the key's guest `toString()`. For a record
        // key (density functions, Holder$Direct, …) that compiles to an ObjectMethods
        // `makeConcatWithConstants` invokedynamic, which ALLOCATES a String → a moving
        // arena collection mid-`map_key`. Both `key` and `val` are arena Values held
        // across it, so store them naively and a moved young key/val is written back
        // STALE — the worldgen density-tree intern killer: a stale Ap2 stored here is
        // later read as Holder$Direct.value → DensityFunction.minValue ULE. Root both in
        // a heap Object[] (visible to every root scan) across `map_key`, then read them
        // back FORWARDED before the store. Same shield as `map_put_gc_safe`/computeIfAbsent.
        let arr = self.heap.alloc_values("Ljava/lang/Object;", vec![key, val]);
        let key0 = self.heap.array_get(arr, 0).unwrap_or(Value::Null);
        let mk = match crate::map::map_key(self, recv, &key0) {
            Ok(mk) => mk,
            Err(e) => { self.pend_vm_err(e); return 0; }
        };
        let key = self.heap.array_get(arr, 0).unwrap_or(Value::Null);
        let val = self.heap.array_get(arr, 1).unwrap_or(Value::Null);
        let prev = self.heap.map_put(id, mk, key, val).unwrap_or(Value::Null);
        jit::box_ref(self, &prev)
    }

    /// COLLECTION INTRINSIC partner of `jit_map_put`: a native `HashMap.get` over
    /// the `$map` table, returning the boxed value cell (null = 0). The receiver is
    /// a native-backed map handle (the JIT only emits this for a statically
    /// `java/util/HashMap`/`LinkedHashMap`-typed receiver, whose instance always
    /// carries a `$map`), so reading it bypasses the synthetic dispatch frame.
    #[cfg(feature = "next-gen-gc")]
    /// COLLECTION INTRINSIC partner of `jit_map_put`: a native `HashMap.get` over
    /// the `$map` table, returning the boxed value cell (null = 0). The receiver is
    /// a native-backed map handle (the JIT only emits this for a statically
    /// `java/util/HashMap`/`LinkedHashMap`-typed receiver, whose instance always
    /// carries a `$map`), so reading it bypasses the synthetic dispatch frame.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_map_get(&mut self, recv: u32, key_cell: i64) -> i64 {
        let Value::Int(id) = self.heap.get_field_syn(recv, crate::heap::Syn::Map) else {
            // No backing map yet (empty map never `put` into) → absent → null.
            return 0;
        };
        let ordered = self.heap.map_access_order(recv, id as u32);
        // A lookup in an access-ordered backing mutates iteration order. Both
        // legacy and arena-fallback callbacks enter here, so guard it centrally
        // before decoding the key or running guest code.
        let _locked_exec = if ordered {
            match jit::worker_illegal_gate(jit::s09census::IDX_MAP_MUT, "map_get_access") {
                jit::WorkerGate::Serial => None,
                jit::WorkerGate::Locked(g) => Some(g),
                jit::WorkerGate::Abort => return 0,
            }
        } else { None };
        let key = jit::unbox_ref(self, key_cell, "");
        let mk = match crate::map::map_key(self, recv, &key) {
            Ok(mk) => mk,
            Err(e) => { self.pend_vm_err(e); return 0; }
        };
        let v = if ordered {
            self.heap.map_move_to_last(id as u32, &mk)
        } else {
            self.heap.map_get(id as u32, &mk)
        }.unwrap_or(Value::Null);
        jit::box_ref(self, &v)
    }

    /// COLLECTION INTRINSIC: native `HashMap.remove(key)` over the `$map` table,
    /// returning the boxed previous value cell (null = 0). Mutating → invalidate the
    /// memoised structural key, exactly like `jit_map_put`.
    #[cfg(feature = "next-gen-gc")]
    /// COLLECTION INTRINSIC: native `HashMap.remove(key)` over the `$map` table,
    /// returning the boxed previous value cell (null = 0). Mutating → invalidate the
    /// memoised structural key, exactly like `jit_map_put`.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_map_remove(&mut self, recv: u32, key_cell: i64) -> i64 {
        let key = jit::unbox_ref(self, key_cell, "");
        let Value::Int(id) = self.heap.get_field_syn(recv, crate::heap::Syn::Map) else {
            return 0; // never `put` into → nothing to remove.
        };
        self.heap.put_field_syn(recv, crate::heap::Syn::SKey, Value::Null);
        let mk = match crate::map::map_key(self, recv, &key) {
            Ok(mk) => mk,
            Err(e) => { self.pend_vm_err(e); return 0; }
        };
        let v = self.heap.map_remove(id as u32, &mk).unwrap_or(Value::Null);
        jit::box_ref(self, &v)
    }

    /// COLLECTION INTRINSIC: native `HashMap.containsKey(key)` → 1/0. Uses the
    /// presence check (NOT get != null) so a key mapped to a null value still
    /// reports present.
    #[cfg(feature = "next-gen-gc")]
    /// COLLECTION INTRINSIC: native `HashMap.containsKey(key)` → 1/0. Uses the
    /// presence check (NOT get != null) so a key mapped to a null value still
    /// reports present.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_map_contains(&mut self, recv: u32, key_cell: i64) -> i64 {
        let key = jit::unbox_ref(self, key_cell, "");
        let Value::Int(id) = self.heap.get_field_syn(recv, crate::heap::Syn::Map) else {
            return 0;
        };
        let mk = match crate::map::map_key(self, recv, &key) {
            Ok(mk) => mk,
            Err(e) => { self.pend_vm_err(e); return 0; }
        };
        self.heap.map_contains(id as u32, &mk) as i64
    }
}
