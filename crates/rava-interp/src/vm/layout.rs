//! Class field layout and the synthetic accessors over it.

use crate::*;

impl Vm {
    pub(crate) fn class_field_layout(&self, class: &str) -> std::sync::Arc<[(u32, u8)]> {
        /// A JDK super with DEFINITIVELY zero instance fields — treated as complete-empty so a
        /// subclass with an unloadable-but-fieldless root still gets a complete arena layout.
        /// Conservative whitelist: ONLY `java/lang/Object` (0 instance fields, guaranteed). NOT
        /// Enum/Record/Number — rava models synthetic heap-only fields for those / routes them
        /// to heap separately, so treating them as fieldless would under-size the layout.
        fn is_fieldless_super(name: &str) -> bool {
            name == "java/lang/Object"
        }
        if let Some(l) = self.runtime_shared().class_layout_get(class) {
            return l;
        }
        // Partial layout computed at this same epoch? Then the walk below would produce exactly
        // this again - the loader has not changed. See the field doc.
        let layout_epoch = self.loader.epoch();
        if let Some((l, e)) = self.runtime_shared().partial_layout_get(class) {
            if e == layout_epoch {
                return l;
            }
        }
        // Super chain, root (java/lang/Object) first.
        let mut chain: Vec<String> = Vec::new();
        let mut cur = class.to_string();
        // `complete` = the chain reached java/lang/Object (super_class None). A break on
        // a get_or_load Err (a super transiently unloadable — re-entrancy / mid-init on
        // another thread) yields a PARTIAL layout missing that super's fields; we must
        // NOT cache it (a later call retries) and the arena-alloc path must fall back to
        // a heap handle rather than commit an object with too few cells (the disk-cache
        // NOP_FallbackServiceProvider putfield id=18 vs partial layout [29,30,31] panic).
        let mut complete = false;
        // Did the walk STOP on an unloadable `java/lang/Enum`? The synthetic-cell injection
        // below keys off `java/lang/Enum` being IN the chain, but a class that fails to load is
        // never pushed — so the one case that needs the injection most is the one case that
        // cannot see it. Carry the observation out of the loop instead.
        #[cfg(feature = "next-gen-gc")]
        let mut enum_super = false;
        for _ in 0..64 {
            match self.loader.get_or_load(&cur) {
                Ok(c) => {
                    chain.push(cur.clone());
                    match &c.file.super_class {
                        Some(sup) => cur = sup.clone(),
                        None => {
                            complete = true;
                            break;
                        }
                    }
                }
                // A KNOWN FIELDLESS JDK super (java/lang/Object) has ZERO instance fields, so it
                // contributes 0 slots and the chain below it is COMPLETE. Treating it as
                // complete-empty lets a plain value class (super = Object, no loadable Object
                // bytecode in the classpath) become an ARENA object (bump) instead of a heap Box
                // malloc — WITHOUT a partial (too-short) layout, since Object adds nothing. Any
                // OTHER unloadable super stays partial (→ heap), as before.
                Err(_) => {
                    crate::enum_birth::note_layout_terminator(&cur);
                    #[cfg(feature = "next-gen-gc")] if self.value_computer_layout_complete(class, &cur) { complete = true; } // VALUECOMPUTER: an admitted pure value-computer's chain is complete at its native-backed unloadable base (vm/native_backing.rs)
                    // Default-ON; RAVA_NO_ARENA_FIELDLESS disables (for A/B / safety fallback).
                    if is_fieldless_super(&cur) && std::env::var_os("RAVA_NO_ARENA_FIELDLESS").is_none() {
                        complete = true;
                    } else if cur == "java/lang/Record"
                        && (crate::nextgen::pinned_mirror_enabled()
                            || crate::nextgen::arena_record_enabled())
                    {
                        // PINNED-MIRROR COVERAGE (RAVA_PINNED_MIRROR, default OFF).
                        // `java/lang/Record` is abstract with ZERO instance fields (the
                        // record's components are declared on the record class itself,
                        // already in `chain`), so a Record-terminated chain's PARTIAL
                        // layout is in fact COMPLETE — nothing below is missing. Without
                        // this, Record-terminated density records (DensityFunctions$Marker/
                        // Mapped/MulOrAdd/ShiftedNoise, Climate$Parameter/ParameterPoint,
                        // …) never get set_layout → never get a pinned mirror (measured
                        // density-handle coverage 0.01%). Gated on the mirror flag so the
                        // default arena-eligibility path stays byte-identical; safe only
                        // with the write-path coherence fix (arena-dual/PROMO no longer
                        // strand the FieldBlob).
                        complete = true;
                    }
                    // ENUM TERMINATOR (RAVA_ARENA_ENUM). `java/lang/Enum` declares exactly two
                    // instance fields, `name` and `ordinal`, and rava does not model them as
                    // heap-only `$fields` — under this flag it models them as the synthetic
                    // `$enumName`/`$enumOrd` arena cells appended right below. So an
                    // Enum-terminated chain is missing NOTHING: the partial layout plus the
                    // injection IS the complete layout, and refusing to cache it is what made
                    // RAVA_ARENA_ENUM inert. Measured: every enum passed the birth gate and was
                    // then handed a handle here, which is why the blocking-cell census did not
                    // move by one cell and why PushReaction still showed 30,837 cells over 4
                    // heap objects. Unlike the collection supers above (AbstractCollection,
                    // ArrayList, HashMap, …) which DO carry native-backed heap-only state and
                    // must keep their subclasses on handles, Enum's state is fully represented.
                    #[cfg(feature = "next-gen-gc")]
                    if cur == "java/lang/Enum" && crate::nextgen::arena_enum_enabled() {
                        complete = true;
                        enum_super = true;
                    }
                    break;
                }
            }
        }
        chain.reverse();
        let mut slots: Vec<(u32, u8)> = Vec::new();
        for cls in &chain {
            // Collect this class's declared INSTANCE fields (skip ACC_STATIC), in
            // declaration order, keyed by their declaring-class identity. The kind
            // is the field's DEFAULT-initialised kind from its descriptor (0/0L/
            // 0.0/null), matching JVM default field init — so a pre-populated but
            // unwritten field reads exactly as the JVM would.
            let decl: Vec<(String, u8)> = match self.loader.get_or_load(cls) {
                Ok(c) => c
                    .file
                    .fields
                    .iter()
                    .filter(|f| f.access_flags & 0x0008 == 0)
                    .map(|f| (f.name.to_string(), default_field_kind(&f.descriptor)))
                    .collect(),
                Err(_) => Vec::new(),
            };
            for (n, kind) in decl {
                // `$`-prefixed names intern BARE here for the same reason `resolve_field_id`
                // does: they are OUR synthetic namespace, natives write them through
                // `heap.field_id(name)`, and a composite `Owner#$elems` id can only ever be a
                // different number than the one the native wrote. With the composite form a
                // guest `java/util/ArrayList` declaring `$elems` got slot id 2 while the native
                // wrote id 0, and the arena putfield aborted:
                // `PHASE5: arena putfield of non-layout field id=0 ... layout=[(2, 6)]`.
                let id = if n.starts_with('$') && crate::class_loader::any_override() {
                    self.heap.intern_field(&n)
                } else {
                    self.heap.intern_field(&format!("{cls}#{n}"))
                };
                slots.push((id, kind));
            }
        }
        // SYNTHETIC-STATE INJECTION (`RAVA_ARENA_SYNTH`, default OFF) — the enum fix,
        // generalised. A class rava implements NATIVELY keeps its state in synthetic `$…`
        // fields that no class file declares, so the layout built above has no cell for them
        // and the object must be a heap handle: an arena putfield to a non-layout field
        // panics. That is why `java/lang/String`, `Optional`, `BitSet` and
        // `LazilyParsedNumber` show up in the blocker census as `[neither]` — handles for a
        // reason the birth rule never states (44,533 cells, 26% of the residue).
        //
        // Enums already solved exactly this: `$enumName`/`$enumOrd` are appended here, and
        // that is what let an enum become an arena object while its natives kept using the
        // ordinary get/putfield path. The set is declared by rava's own native layer, not
        // inferred from any jar, so this is a property of our JDK implementation and no-adapt
        // holds — the same footing the enum injection stands on.
        #[cfg(feature = "next-gen-gc")]
        if crate::nextgen::arena_synth_enabled() {
            for &(fname, kind) in crate::nextgen::synthetic_layout(class) {
                let id = self.heap.intern_field(fname);
                if !slots.iter().any(|&(existing, _)| existing == id) {
                    slots.push((id, kind));
                }
            }
        }
        // ENUM MIGRATION (RAVA_ARENA_ENUM, default OFF): an enum's name/ordinal live
        // in synthetic `$enumName`/`$enumOrd` fields written by the `Enum.<init>`
        // native — they are NOT declared instance fields, so the loop above gives
        // them no arena cell. Append two cells (bare-name ids, matching what the
        // natives put/get) so an arena-born enum can carry them: `$enumName` as a
        // ref cell (kind 6 → traced+forwarded by the moving GC), `$enumOrd` as an int
        // cell (kind 0 → skipped). Keyed off the WHOLE super chain (covers both the
        // enum class and its constant-body subclasses E$1). Flag OFF ⇒ no injection
        // ⇒ byte-identical layout. Dedup-guarded (idempotent if ever re-declared).
        #[cfg(feature = "next-gen-gc")]
        if crate::nextgen::arena_enum_enabled()
            && (enum_super || chain.iter().any(|c| c == "java/lang/Enum"))
        {
            let name_id = self.heap.intern_field("$enumName");
            let ord_id = self.heap.intern_field("$enumOrd");
            if !slots.iter().any(|&(id, _)| id == name_id) {
                slots.push((name_id, 6)); // FK_NULL — ref default (traced)
            }
            if !slots.iter().any(|&(id, _)| id == ord_id) {
                slots.push((ord_id, 0)); // FK_INT
            }
        }
        let arc: std::sync::Arc<[(u32, u8)]> = slots.into();
        // Only a COMPLETE chain is authoritative — cache/publish it. A partial layout is
        // returned to the caller (heap prepopulate tolerates a subset) but NOT cached, so
        // the next call recomputes once the missing super is loadable. new_object_value
        // checks class_layout_cache after this call: absence ⇒ partial ⇒ heap fallback.
        if complete {
            Self::debug_assert_not_gilfree("class_layout_cache");
            self.runtime_shared().class_layout_insert(class, arc.clone());
            self.heap.set_layout(class, arc.clone());
        } else {
            // Remember it AGAINST THE EPOCH only. Not authoritative, not published to the heap,
            // and deliberately not in `class_layout_cache` - its absence is the caller's signal.
            self.runtime_shared().partial_layout_insert(class, arc.clone(), layout_epoch);
        }
        arc
    }

    /// Read a synthetic field from EITHER representation. The natives that implement a JDK
    /// class hold a `u32` handle and call `heap.get_field`, which is exactly what stops those
    /// classes from becoming arena objects. Routing them through a Value-level accessor is
    /// the same coexistence shape `enum_field` already uses, without the enum specialisation.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn synth_get(&mut self, recv: &Value, name: &str) -> Value {
        match recv {
            Value::Obj(p) if crate::nextgen::arena_synth_enabled() => {
                // `field_id_opt`, NOT `intern_field`. Both answer "the id for this name", but
                // `intern_field` is the plain interner path — an `RwLock` read-guard acquire plus
                // an FxHash of the name on EVERY call — while `field_id_opt` goes through the TLS
                // L1 that exists *because* "the natives read their synthetic backing fields
                // (`$elems`, `$map`, `$buf`, `$pos`, …) by NAME on every call". The arena arm of
                // this very accessor was missing the cache built for it; the handle arm below
                // reaches `Heap::get_field`, which does not take this route.
                //
                // Semantics are unchanged: a name no object has ever been given is not interned,
                // so it has no field — which is the `Value::Null` the `slot_of` miss below already
                // returns. Interning it would only mint an id nothing can match.
                let Some(id) = self.heap.field_id_opt(name) else {
                    return Value::Null;
                };
                // A cell the layout does not have would be an out-of-bounds read; absence is
                // reported as Null, which is what a never-written synthetic field reads as on
                // the handle side too.
                //
                // WAS: this READ THE CLASS ID AND PROBED THE LAYOUT TWICE per access — once
                // here to prove the cell exists, then again inside `arena_getfield`, which starts
                // by re-reading the same header and re-resolving the same (class, field). Both
                // questions are about one value that cannot change between the two lines, and the
                // natives this serves read `$elems`/`$map`/`$buf`/`$pos` on EVERY call. IS: one
                // header read and one index probe; `None` is the same absence the scan reported.
                if crate::opcodes::fast_arenacell() {
                    return crate::opcodes::arena_getfield_opt(*p, id).unwrap_or(Value::Null);
                }
                let cid = crate::nextgen::arena_class_id(*p);
                if crate::nextgen::slot_of(cid, id).is_some() {
                    crate::opcodes::arena_getfield(*p, id)
                } else {
                    Value::Null
                }
            }
            Value::Ref(h) => self.heap.get_field(*h, name),
            _ => Value::Null,
        }
    }

    /// Read-only twin of `synth_get`. `synth_get` needs `&mut self` only to INTERN the field
    /// name; a native that already holds `&Vm` (`bitset_words`) cannot call it, and widening
    /// that native to `&mut Vm` would ripple through its whole dispatch. `field_id_opt` is the
    /// read-only lookup — absent id means the field was never written, which reads as Null on
    /// the handle side too, so the two paths agree.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn synth_get_ro(&self, recv: &Value, name: &str) -> Value {
        match recv {
            Value::Obj(p) if crate::nextgen::arena_synth_enabled() => {
                let Some(id) = self.heap.field_id_opt(name) else { return Value::Null };
                // Same double class-id read + double layout probe as `synth_get`; same cure.
                if crate::opcodes::fast_arenacell() {
                    return crate::opcodes::arena_getfield_opt(*p, id).unwrap_or(Value::Null);
                }
                let cid = crate::nextgen::arena_class_id(*p);
                if crate::nextgen::slot_of(cid, id).is_some() {
                    crate::opcodes::arena_getfield(*p, id)
                } else {
                    Value::Null
                }
            }
            Value::Ref(h) => self.heap.get_field(*h, name),
            _ => Value::Null,
        }
    }

    /// Write a synthetic field to EITHER representation. Silently no-ops on a receiver that
    /// is neither, mirroring the handle path's behaviour for an unknown handle.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn synth_put(&mut self, recv: &Value, name: &str, v: Value) {
        match recv {
            Value::Obj(p) if crate::nextgen::arena_synth_enabled() => {
                let id = self.heap.intern_field(name);
                // BOX A PRIMITIVE INTO A REF CELL. A synthetic field like `Optional.$val` is
                // declared ref-kind because it usually holds a reference — but `Optional` of a
                // boxed number legitimately holds an `Int`/`Long`/`Float`/`Double`, and a RAW
                // primitive in a ref-masked cell is (a) decoded back as a reference and (b)
                // read by the moving collector AS A POINTER. Measured symptom before this:
                // 14,576 census cells resolving to `?dead-or-out-of-range`, i.e. bits being
                // mistaken for handles. `encode_capture_cell` is the existing rule for exactly
                // this hazard on lambda captures; reuse it rather than restate it.
                let cid = crate::nextgen::arena_class_id(*p);
                // WAS, per synthetic field WRITE: `slot_of` twice for the same (class, field),
                // plus `class_layout` — which CLONES THE WHOLE CLASS RECORD OUT (two `Arc`
                // refcount pairs, name and layout) only to index one element of it — only to
                // recover the kind byte that lives in the very index entry the first `slot_of`
                // already touched. Three derivations of one 8-byte layout-index entry, all
                // constants of the class. IS: one probe, no clone, no second lookup.
                //
                // The encode also MOVES INSIDE the presence test. `encode_capture_cell` can push
                // a side-table box or mint an arena string; on a class with no such cell that
                // value was built, retained by the box table, and then dropped on the floor
                // because the write never happened. Nothing could reference it, so building it
                // was pure waste and retaining it was worse than useless.
                if crate::opcodes::fast_arenacell() {
                    if let Some((s, k)) = crate::nextgen::slot_kind_of(cid, id) {
                        let cell = crate::opcodes::encode_capture_cell(&v, k >= 4);
                        unsafe { crate::nextgen::set_cell(p.as_ptr(), s, cell) };
                    }
                    return;
                }
                let ref_slot = crate::nextgen::slot_of(cid, id)
                    .and_then(|s| crate::nextgen::class_layout(cid).and_then(|(_, l)| l.get(s).map(|&(_, k)| k)))
                    .is_some_and(|k| k >= 4);
                let cell = crate::opcodes::encode_capture_cell(&v, ref_slot);
                if let Some(s) = crate::nextgen::slot_of(cid, id) {
                    unsafe { crate::nextgen::set_cell(p.as_ptr(), s, cell) };
                }
            }
            Value::Ref(h) => self.heap.put_field(*h, name, v),
            _ => {}
        }
    }

    /// cp-indices of getfield/putfield sites whose field must KEEP its runtime tag under the
    /// bare-deref flip, computed with the loader available.
    ///
    /// `jit::array_field_indices` answers the part decidable from the descriptor alone — an
    /// array, `java/lang/Object`, or a name-matched native-backed type. It cannot answer the
    /// larger part: a field declared as a type whose ANCESTOR is native-backed also always
    /// holds a handle, because such a type can never be arena-born, so keeping the tag there
    /// is both correct and necessary. Measured, that is **14,541 of the 34,733** cells the
    /// census says must otherwise reach 0b00 — the whole `LinkedTreeMap` population, closed
    /// without migrating anything.
    ///
    /// Computed HERE rather than inside the compiler because the compiler has no `Vm`, and
    /// `background_jit` ships jobs to another thread so a closure cannot cross. The set rides
    /// the job exactly as `field_ids` already does.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn tagged_field_indices(
        &self,
        bc: &[u8],
        pool: &rava_classfile::ConstantPool,
    ) -> std::collections::HashSet<u16> {
        let mut out = crate::jit::array_field_indices(bc, pool);
        // The extension exists only to decide bare loads, and the emitter consults it only
        // under `bare_ref_load_enabled`. With the flip off — the default — every loader walk
        // below is compile time spent on an answer nothing reads, on every compiled method.
        // The SHADOW VERIFY needs the same set the flip would use, or it instruments the wrong
        // population: with the extension skipped, `bare_ok` is true at every non-array ref field
        // instead of at the 61 sites the rule actually admits, and a call emitted into all of them
        // slows the boot ~30x (measured: 876 CPU-seconds without reaching `Environment:`).
        if !crate::nextgen::bare_ref_load_enabled() && !crate::nextgen::bare_verify_enabled() {
            return out;
        }
        let mut pc = 0usize;
        while pc < bc.len() {
            let op = bc[pc];
            let Some(len) = crate::jit::insn_len_at(bc, pc) else { break };
            if (op == 0xb4 || op == 0xb5) && !out.contains(&(((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16)) {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                if let Some(m) = pool.member_ref(idx) {
                    if let Some(cls) = m.descriptor.strip_prefix('L').and_then(|d| d.strip_suffix(';')) {
                        if self.keeps_tag_by_declared_type(cls) {
                            out.insert(idx);
                        } else if std::env::var_os("RAVA_BARE_AUDIT").is_some() {
                            // The list of sites the rule ADMITS to the bare load, named where
                            // the pool and the loader both exist. Reading it against the stale
                            // receivers' callers is what turns "some site is wrong" into a
                            // site — cheaper than an emitted per-load check and it cannot
                            // itself perturb what it measures.
                            eprintln!(
                                "[BARE-OK] {}#{} : {}",
                                m.owner, m.name, m.descriptor
                            );
                        }
                    }
                }
            }
            pc += len;
        }
        out
    }

    /// Must a field DECLARED as `cls` keep its runtime tag under the bare-deref flip?
    ///
    /// Keeping a tag is always SOUND — it costs one test, never correctness — so every
    /// clause only has to be a reason instances of `cls` might be handles, never a proof
    /// that they always are. What each clause must be is TIME-INVARIANT, because a compiled
    /// site outlives the moment it was compiled. That rules out the obvious fourth clause,
    /// "the class has no complete layout yet": a class already cached still has instances
    /// born BEFORE it was cached, and those are handles a bare deref would return as a wild
    /// pointer. It was worth 1,208 cells and it is not usable.
    ///
    /// Measured on a full boot, this leaves 37 blocking cells of 3,332,584 — the arc's
    /// census started at 380,922.
    ///
    /// The interface clause is the one that carries it, and it is the one no NAME predicate
    /// could reach: guava's `ImmutableSortedMap` is loadable, concrete and matches no native
    /// name, yet every instance is a handle, because rava's native map layer answers its
    /// construction and hands back a `LinkedHashMap`. What every such class shares is not a
    /// name but the interface it implements — which is precisely what the native layer keys
    /// on. That one clause was 1,171 of the 1,208.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn keeps_tag_by_declared_type(&self, cls: &str) -> bool {
        // MEMOISED PER CLASS NAME, because the answer is a property of the class chain and
        // nothing else — and because the uncached form makes the arc's own tooling unusable.
        //
        // ⭐**Measured.** This is asked once per `getfield`/`putfield` site per COMPILED METHOD,
        // and each miss walks the super chain through `get_or_load`. With `RAVA_BARE_VERIFY`
        // armed a boot logged **107,759 admissions in 120 seconds and never reached
        // `Environment:`**, compiling only 24 bare-eligible sites; the same boot with the rule
        // switched off reaches **`Done` in 12.99 s and emits 3,697 of them, 2,529 admitted**.
        // The instrument was strangling compilation, so the probe hanging off it had almost
        // nothing to run — a measurement that changed its subject beyond recognition.
        //
        // Classes are never redefined here, so a name is a permanent key.
        {
            let hit = self.runtime_shared().tagkeep_get(cls);
            if let Some(v) = hit {
                return v;
            }
        }
        let verdict = self.keeps_tag_by_declared_type_uncached(cls);
        self.runtime_shared().tagkeep_insert(cls, verdict);
        verdict
    }

    /// The verdict itself — the chain walk this memo exists to run once per class.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn keeps_tag_by_declared_type_uncached(&self, cls: &str) -> bool {
        // The WIDE question, deliberately not the birth-guarded one. Birth asks "may I put
        // this in the arena?" and must be narrow; this asks "might this ever be a handle?"
        // and must be wide.
        if self.has_native_backed_ancestor(cls) {
            return true;
        }
        let Ok(loaded) = self.loader.get_or_load(cls) else {
            // Nothing can be proven about a class that will not load.
            return true;
        };
        // ACC_INTERFACE | ACC_ABSTRACT: any implementor may be native-backed.
        if loaded.file.access_flags & 0x0600 != 0 {
            return true;
        }
        // ACC_ENUM, or a direct `java/lang/Enum` super. The enum migration is explicitly a
        // COEXISTENCE: its own gate documents that synthetic JDK enums which `heap.alloc` in
        // natives "stay handles and coexist" with arena-born bytecode enums. So an enum-typed
        // field is MIXED by construction, whatever the migration flag says — and the audit
        // found this family occupying most of the ~90 sites the rule was admitting
        // (Lifecycle, Holder$Reference$Type, PushReaction, Direction, Strictness, MapColor).
        if loaded.file.access_flags & 0x4000 != 0
            || loaded.file.super_class.as_deref() == Some("java/lang/Enum")
        {
            return true;
        }
        drop(loaded);
        // The SAME predicate `has_native_backed_ancestor` applies, over the same closure it
        // should have walked: supers AND INTERFACES. That one omission is the whole gap.
        // `has_native_backed_ancestor` walks `super_chain` only, so a class whose native
        // backing is declared by an interface is invisible to it — and the natives key on
        // exactly those (`is_entry_type` covers `java/util/Map$Entry`, which is not
        // `java/util/Map` and was the single largest name in the stale-receiver flood:
        // `Reference2ObjectMap$Entry.getKey/getValue`, 71,276 reports).
        // Reusing `is_native_backed_alloc` rather than naming interfaces here keeps ONE
        // definition of "the natives back this", which row 96 established is the only way
        // the flag-on and flag-off arms cannot drift.
        let mut queue = vec![cls.to_string()];
        let mut seen: Vec<String> = Vec::new();
        while let Some(cn) = queue.pop() {
            if crate::opcodes::is_native_backed_alloc(&cn) {
                return true;
            }
            if seen.iter().any(|s| *s == cn) || seen.len() > 64 {
                continue;
            }
            seen.push(cn.clone());
            if let Ok(l) = self.loader.get_or_load(&cn) {
                queue.extend(l.file.interfaces.iter().cloned());
                if let Some(s) = l.file.super_class.clone() {
                    queue.push(s);
                }
            }
        }
        false
    }

    /// STRICT field-id resolution for AOT const baking: returns the `declaring#name` id
    /// ONLY if a declaring class is actually found in the super chain. resolve_field_id
    /// falls back to a BARE-name id when the class isn't loaded yet — fine for the interp
    /// heap side-table, but for an ARENA object the layout is keyed by `declaring#name`,
    /// so a baked bare-name id misses the layout (panic 'non-layout field'). Baking must
    /// bail (None → the method runs interp) rather than commit a bare-name id whose value
    /// depends on class-load timing (the disk-cache/worker-thread race that hit
    /// NOP_FallbackServiceProvider: putfield id=18 vs layout [29,30,31]).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn field_declaring_id(&mut self, owner: &str, name: &str) -> Option<u32> {
        let mut cur = owner.to_string();
        for _ in 0..64 {
            let c = self.loader.get_or_load(&cur).ok()?;
            if c.file.fields.iter().any(|f| f.name == name && f.access_flags & 0x0008 == 0) {
                return Some(self.heap.intern_field(&format!("{cur}#{name}")));
            }
            cur = c.file.super_class.clone()?;
        }
        None
    }

    /// The class-constant layout slot of field-identity `id` in `owner`'s
    /// pre-populated objects, or None when layout pre-population is off or the
    /// field isn't in the layout (then the JIT keeps the id-scan getfield). Used
    /// at JIT compile time to emit a constant-offset field load.
    ///
    /// `&self`, not `&mut self`: it only calls `class_field_layout`, which has been `&self` since
    /// the layout caches went behind `RwLock`, so the exclusive borrow was left over rather than
    /// required. It matters because THIS is the closure the Cranelift compile is handed —
    /// `&mut |o, n| (resolve_field_id(o, n), layout_slot(o, id))` — and an `&mut Vm` inside it is
    /// what pins codegen to the thread that owns the VM. `resolve_field_id` was already `&self`;
    /// with this one the closure needs no exclusive access at all, which is the prerequisite for
    /// ever running codegen off the serial critical path (~4.5s, see
    /// `jit::compile_inputs_are_send_except_the_recorded_blocker`).
    pub(crate) fn layout_slot(&self, owner: &str, id: u32) -> Option<u32> {
        if !layout_alloc_enabled() {
            return None;
        }
        // WAS: `class_field_layout` CLONES THE LAYOUT `Arc` OUT of the cache — an atomic
        // refcount increment and a matching decrement — for a slice this function only scans
        // and then drops. The pair is asked once per compiled getfield/putfield site, per compiled
        // method. IS: on the cached (COMPLETE-layout) path the scan runs under the read guard and
        // the `Arc` never moves. A miss falls straight through to the builder, so a PARTIAL layout
        // — which is deliberately never in this cache — answers exactly as before.
        #[cfg(feature = "next-gen-gc")]
        if crate::opcodes::fast_arenacell() {
            if let Some(slot) = self.runtime_shared().class_layout_slot(owner, id) {
                return Some(slot);
            }
        }
        self.class_field_layout(owner)
            .iter()
            .position(|&(i, _)| i == id)
            .map(|p| p as u32)
    }

    pub(crate) fn resolve_field_id(&self, owner: &str, name: &str) -> u32 {
        // Read the diagnostic kill-switch ONCE (not per call): `std::env::var` locks a
        // process-global mutex + allocates a String, and this runs on EVERY field-id
        // resolution (millions of times during registry build) — pure waste before the
        // tlcache. Cache it like VERIFY_LAYOUT below.
        static FIELD_BARE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *FIELD_BARE.get_or_init(|| std::env::var_os("RAVA_FIELD_BARE").is_some()) {
            return self.heap.intern_field(name); // diagnostic kill-switch
        }
        // ⭐**`$`-PREFIXED FIELDS RESOLVE BARE, AND THAT IS WHAT LETS java.util MIGRATE TO
        // BYTECODE.** Every `$name` field is OURS — natives write `$elems`/`$map`/`$pos`
        // through `heap.field_id(name)`, i.e. the bare interned id, and no Java source in the
        // JDK declares a `$`-prefixed field. So the owner walk below can only ever answer a
        // DIFFERENT id than the one the natives wrote, which is exactly what made a guest
        // `java/util/ArrayList` read `null` out of an array a native had just filled
        // (`Arrays.asList("a","b","c").size() == 0`). Under the global `RAVA_FIELD_BARE`
        // kill-switch the same probe answered `size=3 get1=b`; this narrows that to the one
        // namespace where it is sound, leaving real Java field SHADOWING — the reason the
        // owner walk exists — untouched.
        //
        // It also skips the hierarchy walk for these names, which is a small saving on a path
        // the comment above says runs millions of times during registry build.
        if name.starts_with('$') && crate::class_loader::any_override() {
            return self.heap.intern_field(name);
        }
        if let Some(id) = crate::tlcache::field_key_get(owner, name) {
            return id;
        }
        let mut cur = owner.to_string();
        let mut declaring: Option<String> = None;
        for _ in 0..64 {
            match self.loader.get_or_load(&cur) {
                Ok(c) => {
                    if c.file
                        .fields
                        .iter()
                        .any(|f| f.name == name && f.access_flags & 0x0008 == 0)
                    {
                        declaring = Some(cur.clone());
                        break;
                    }
                    match &c.file.super_class {
                        Some(sup) => cur = sup.clone(),
                        None => break,
                    }
                }
                Err(_) => {
                    // Which link stopped the walk, and why: "the jar has no such class" and "our
                    // native layer claims this name so the loader refuses it" are different
                    // diagnoses — a broken jar versus an adaptation defect.
                    if std::env::var_os("RAVA_BAREFIELD_LOG").is_some() {
                        eprintln!("[BAREFIELD-BREAK] walking {owner}.{name}: get_or_load({cur}) FAILED");
                    }
                    break;
                }
            }
        }
        // ⭐**THE BARE-NAME FALLBACK IS THE FAULT'S ORIGIN, AND IT WAS SILENT.** A field id is
        // normally `Declaring#name` — the STORAGE identity, which is what makes a subclass
        // redeclaring a name a distinct field. The `None` arm hands back the UNQUALIFIED name
        // instead, and that only happens when the owner-chain walk broke: the owner would not
        // load, or the chain ran out without anyone declaring the field. Such an id belongs to no
        // class's layout, so any object read with it fails the layout test — which is exactly the
        // vanilla-Minecraft panic, where `registry` printed unqualified on an `amt$a` receiver.
        // `RAVA_BAREFIELD_LOG=1` names the owner it could not resolve.
        if declaring.is_none() && std::env::var_os("RAVA_BAREFIELD_LOG").is_some() {
            static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
                std::sync::OnceLock::new();
            let seen = SEEN.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
            let key = format!("{owner}#{name}");
            if seen.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone()) {
                eprintln!("[BAREFIELD] owner={owner} name={name} — chain broke, id is UNQUALIFIED");
                // WHO asked. `amt$a` has exactly two fields (`a`, `b`) and no `registry`, so the
                // request cannot have come from that class's own constant pool — the guest frame
                // chain names the real origin.
                eprintln!("[BAREFIELD-BT]
{}", self.backtrace());
                // Empty guest backtrace means this ran from COMPILED context, where the field ref
                // was resolved at emit time — so the method being compiled names the pool the
                // `Fieldref` came from, which the receiver's class cannot.
                eprintln!("[BAREFIELD-JIT] compiling: {:?}", crate::jit::jit_method_cur());
            }
        }
        let id = match &declaring {
            Some(d) => self.heap.intern_field(&format!("{d}#{name}")),
            None => self.heap.intern_field(name),
        };
        // Foundation validation (RAVA_VERIFY_LAYOUT): the resolved field id must
        // appear in the class's computed layout — proves `class_field_layout`
        // enumerates exactly the field identities every access resolves to, the
        // correctness precondition before alloc/JIT use fixed slots. Dormant by
        // default (flag off). Only meaningful for a real instance field (declaring
        // Some); a bare/unresolved field has no layout slot.
        static VERIFY_LAYOUT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if declaring.is_some() && *VERIFY_LAYOUT.get_or_init(|| std::env::var("RAVA_VERIFY_LAYOUT").is_ok()) {
            let layout = self.class_field_layout(owner);
            if !layout.iter().any(|&(lid, _)| lid == id) {
                eprintln!("[LAYOUT-MISS] {owner}#{name} id={id} not in layout(len={})", layout.len());
            }
        }
        crate::tlcache::field_key_put(owner, name, id);
        id
    }
}
