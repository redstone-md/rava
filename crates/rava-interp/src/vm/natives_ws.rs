//! Well-known JDK shapes served natively, and the three prewarm scans.

use crate::*;

impl Vm {
    /// A WS-native's receiver, in EITHER representation, gated on its EXACT class.
    ///
    /// ⭐**Ledger rows 324/327.** Every native in this file opened with
    /// `if (recv_cell as u64) >> 32 != REF_TAG >> 32 { return None }` — a HEAP HANDLE — and an
    /// arena-born object is a raw 0b00 pointer, so all five refused it and the call fell through
    /// to `rava_invoke` and the full owner cascade. The served census across `RAVA_ARENA_SYNTH`
    /// names them one for one: **ArrayList +3,874,571 · ThreadLocal +832,085 · AtomicLong
    /// +316,710 · LinkedHashSet +204,142**, summing to 5.23 M against a measured excess of
    /// 5.22 M — i.e. these five guards ARE the whole remainder once `jit_bitset_words` removed
    /// BitSet's.
    ///
    /// EXACT class, not a subtype: every caller here reads a synthetic backing field whose
    /// meaning a subclass may change (`AtomicInteger` shares `$val` and returns an int), so the
    /// test that was right for handles stays right here.
    ///
    /// `None` means "not a receiver this arm can serve" and the caller falls through to the
    /// locked general path — a gap is a missed optimisation, never a wrong answer.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn ws_recv_of_class(&self, recv_cell: i64, class: &str) -> Option<Value> {
        if (recv_cell as u64) >> 32 == (crate::value::REF_TAG as u64) >> 32 {
            let h = recv_cell as u32;
            return (self.heap.class_of(h) == Some(class)).then_some(Value::Ref(h));
        }
        if !crate::nextgen::arena_synth_enabled()
            || recv_cell <= 0
            || !crate::nextgen::is_arena_obj_addr(recv_cell as u64)
        {
            return None;
        }
        let p = crate::arena::ArenaPtr::new(recv_cell as *mut u8)?;
        let cid = crate::nextgen::arena_class_id(p);
        (crate::nextgen::class_name(cid).as_deref() == Some(class)).then_some(Value::Obj(p))
    }
    /// WS-native pre-lock probe for `AtomicLong.get()J` — the PRIMITIVE-returning twin of
    /// `ws_arraylist_get`, and 19% of the ring's locked callee population (`AtomicLong#get`
    /// 200,302 lock takes per boot beside `Predicate#test`/`Function#apply`).
    ///
    /// ⚠ THE TEMPLATE DOES NOT TRANSFER BY SHAPE, ONLY BY RETURN TYPE. `ws_arraylist_get` ends
    /// in `jit::box_ref`, which encodes `Ref`/`Array`/`Null`/`Obj` natively and sends anything
    /// else through `box_push | BOX_TAG` — a side-table index. Copying that for a `()J` result
    /// would hand a compiled caller a table index where it expects a number, and the failure is
    /// silent. Checked the ABI before writing this rather than after: `abi_ret_from_cell` says
    /// "the memory ABI always returns i64 bits" and passes `PrimKind::I64` through unchanged,
    /// symmetric with `unpack_arg`'s `b'J' => JVal::Long(cell)`. **So a `J` return IS the raw
    /// cell, and this returns it directly — no boxing, which is why it is safe.**
    ///
    /// Pure read, exact-class gated, and it mirrors the native's own `Null ⇒ 0` convention
    /// (`atomic_method`: a never-written `$val` reads as zero). Any mismatch returns `None` and
    /// the caller falls through to the locked general path, so a gap is a missed optimisation
    /// and never a wrong answer.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn ws_atomic_long_get(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        raw: &[i64],
    ) -> Option<i64> {
        let recv_cell = crate::nextgen::forward_if_moved(raw.first().copied()?);
        // EXACT class: `AtomicInteger` shares `$val` but returns an int, and
        // `AtomicReference` holds a reference — both would be wrong through this arm.
        let recv = self.ws_recv_of_class(recv_cell, "java/util/concurrent/atomic/AtomicLong")?;
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let m = caller.file.pool.member_ref(cp)?;
        if m.name != "get" || m.descriptor != "()J" {
            return None;
        }
        match self.synth_get_ro(&recv, "$val") {
            Value::Long(l) => Some(l),
            Value::Null => Some(0),
            _ => None,
        }
    }

    /// P3-S1 WS-native (lock-free worker fast path): `java/util/ArrayList.get(I)` as a pure
    /// read — exact-class gated ($elems + array_get are &self; box_ref routes to the worker's
    /// arena segment). None on any mismatch/OOB → caller falls to the locked general path.
    /// Measured: List#get was 914K of the region's locked traffic. Shared by the jit_call
    /// worker guard AND the rava_dispatch_miss step-0.5 (pre-lock) probe.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn ws_arraylist_get(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        raw: &[i64],
    ) -> Option<i64> {
        let recv_cell = crate::nextgen::forward_if_moved(raw.first().copied()?);
        let recv = self.ws_recv_of_class(recv_cell, "java/util/ArrayList")?;
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let m = caller.file.pool.member_ref(cp)?;
        if m.name != "get" || m.descriptor != "(I)Ljava/lang/Object;" {
            return None;
        }
        let Value::Array(ea) = self.synth_get_ro(&recv, "$elems") else { return None };
        let idx = raw.get(1).copied().unwrap_or(-1) as i32;
        let len = self.heap.array_len(ea).unwrap_or(0);
        if idx < 0 || (idx as usize) >= len {
            return None; // OOB → locked general path (exception semantics)
        }
        let v = self.heap.array_get(ea, idx as usize)?;
        if crate::jit::freeze_resolve::stats_on() {
            crate::jit::freeze_resolve::WORKER_WSNATIVE_HIT
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Some(jit::box_ref(self, &v))
    }

    /// P3-S1 WS-native: `contains(Object)` on a list-backed receiver as a PURE READ, for the
    /// gil-free worker's pre-lock arms.
    ///
    /// WHY THIS ONE. The per-site freeze-coverage census puts `java/util/Set.contains` at 3249
    /// calls per ring task, every one of them `unknown` — i.e. every one falls through to the ONE
    /// global resolve lock, and the span each fall-through anchors is what keeps eight workers
    /// running one-wide.
    ///
    /// WHY THE RECEIVER IS A LIST. The census names the SITE, whose declared owner is the `Set`
    /// interface; the concrete receiver measured in the ring is `java/util/ArrayList`. A first
    /// version of this arm gated on `class_of(..).contains("Set")` and therefore served ~3% of
    /// what it was written for. The gate now matches what actually arrives.
    ///
    /// WHY IT IS SOUND TO SERVE UNLOCKED. `&self`, deliberately — the signature is the proof that
    /// nothing shared is mutated. For a list-backed receiver the locked general path is the linear
    /// scan in `stream.rs` (`elems.iter().any(ekey_eq(..))`), and both `ekey` and `ekey_eq` take
    /// `&Vm`. This arm runs that same scan with the same key function over the same array, so the
    /// answer is bit-identical by construction; the only difference is which branch ran. The
    /// index-backed `Set` path is deliberately NOT served: it rebuilds `Vm::set_index` when the
    /// stamp is stale, and a rebuild is exactly the shared mutation this exists to avoid.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn ws_list_contains(
        &self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        raw: &[i64],
    ) -> Option<i64> {
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let m = caller.file.pool.member_ref(cp)?;
        if m.name != "contains" || m.descriptor != "(Ljava/lang/Object;)Z" {
            return None;
        }
        let recv_cell = crate::nextgen::forward_if_moved(raw.first().copied()?);
        // EXACT class, like `ws_arraylist_get`: a subclass could override `contains`, and a
        // `$elems`-backed *Set* takes the index path in the general arm rather than this scan.
        let recv = self.ws_recv_of_class(recv_cell, "java/util/ArrayList")?;
        let Value::Array(arr) = self.synth_get_ro(&recv, "$elems") else { return None };
        let needle = jit::unbox_ref(self, raw.get(1).copied().unwrap_or(0), "");
        let key = crate::stream::ekey(self, &needle);
        // The general path keys elements with `ekey_mut`, which escalates an
        // identity-fallback key (`@h`) to the value-equality derivation
        // (`o:{toString}`, a GUEST frame) — impossible behind `&self`. Serve only a
        // needle whose key is in the guest-free namespace: a primitive (`i:`/`j:`/
        // float-or-double text), a string (`s:`) or `null`. `ekey_mut` is then the
        // IDENTITY on that needle, and no element key it could escalate to
        // (`o:`/`m:`/`c:`/`@`) can collide with those tags — so the answer is still
        // bit-identical to the locked scan, which is the only reason this arm may run
        // unlocked. An object needle falls through to it.
        if key.starts_with('@')
            || key.starts_with("o:")
            || key.starts_with("m:")
            || key.starts_with("c:")
        {
            return None;
        }
        let len = self.heap.array_len(arr)?;
        let mut present = false;
        for i in 0..len {
            let e = self.heap.array_get(arr, i)?;
            if crate::stream::ekey_eq(self, &needle, &key, &e) {
                present = true;
                break;
            }
        }
        if crate::jit::freeze_resolve::stats_on() {
            crate::jit::freeze_resolve::WORKER_WSNATIVE_HIT
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Some(jit::pack_local(&Value::Int(present as i32)))
    }

    /// `ThreadLocal.get` / `ThreadLocal.set` served WITHOUT the dispatch lock.
    ///
    /// These are 825,246 of a ring region's 3.86 M locked acquisitions — the second-largest block
    /// after the density-function constructors — and they are locked for no reason at all once
    /// the value lives per-thread (`crate::tl_worker`): a `set` is an insert into this thread's
    /// own map and a `get` is a lookup in it. Nothing shared is read or written, so nothing needs
    /// excluding.
    ///
    /// A `get` that MISSES this thread's table falls through to the general path, because the
    /// answer then depends on the object's fields and possibly on running a supplier — guest code.
    /// `get`/`set` are final on `java.lang.ThreadLocal`, so a subclass cannot change what they
    /// mean; the class gate is only here to keep an unrelated `get()`/`set(Object)` from matching.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn ws_threadlocal(
        &self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        raw: &[i64],
    ) -> Option<i64> {
        // Ordinary native remove uses object fields; never split its state into the worker table.
        if !crate::parallel::gilfree() || !crate::tl_worker::enabled() {
            return None;
        }
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let m = caller.file.pool.member_ref(cp)?;
        let recv_cell = crate::nextgen::forward_if_moved(raw.first().copied()?);
        // ⚠HANDLE-ONLY, AND NOT AN OVERSIGHT — the one of these five that does not convert.
        // Its state is not a field on the receiver: `tl_worker` keys a SIDE TABLE by the handle.
        // An arena object has no handle, and its ADDRESS cannot stand in because the Cheney
        // collector moves it, so the entry would be looked up under a stale key after the first
        // collection. A migrated `ThreadLocal` therefore falls through to the general path, and
        // the served census bills it **0 → 832,085** across `RAVA_ARENA_SYNTH`. Retiring handles
        // needs a stable identity for exactly this shape — see the pinned region, which does not
        // move. Recorded rather than patched.
        if (recv_cell as u64) >> 32 != (crate::value::REF_TAG as u64) >> 32 {
            return None;
        }
        let h = recv_cell as u32;
        if !matches!(
            self.heap.class_of(h),
            Some("java/lang/ThreadLocal") | Some("java/lang/InheritableThreadLocal")
        ) {
            return None;
        }
        let out = match (m.name, m.descriptor) {
            ("get", "()Ljava/lang/Object;") => jit::box_ref(self, &crate::tl_worker::get(h)?),
            ("set", "(Ljava/lang/Object;)V") => {
                let v = jit::unbox_ref(self, raw.get(1).copied().unwrap_or(0), "java/lang/Object");
                crate::tl_worker::set(h, v);
                0
            }
            _ => return None,
        };
        if crate::jit::freeze_resolve::stats_on() {
            crate::jit::freeze_resolve::WORKER_WSNATIVE_HIT
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Some(out)
    }

    /// `Optional.isPresent` / `isEmpty` / `get` served WITHOUT the dispatch lock.
    ///
    /// The freeze-coverage census (`RAVA_FREEZE_CENSUS`) on a ring task puts these at
    /// **113,117 calls EACH, all classified `unknown`** — neither the freeze-resolve table nor
    /// the frozen direct cache can serve them, because the methods are native-served and have no
    /// compiled body to publish a direct target for. So every one of them falls through to the
    /// single global resolve lock, and that lock is where the lane's workers spend ~28% of their
    /// ring time (`bench/lane_lock_census.ps1`: 82.5 M outer acquisitions, 33.3 s of aggregate
    /// wait across 4 workers). Two accessors are ~20% of a task's entire locked-miss population.
    ///
    /// Nothing here is shared-mutable: `$present` and `$val` are written once when the Optional
    /// is constructed and never again — `java.util.Optional` is final and immutable, and
    /// `stream::make_optional` is the only writer. A worker reading them reads a frozen object,
    /// exactly like the `$elems` read `ws_arraylist_get` already does.
    ///
    /// EMPTY `get()` deliberately falls through to the locked path: it must raise
    /// `NoSuchElementException`, which is guest-visible control flow rather than a field read.
    /// The class gate is exact (`java/util/Optional`) so an unrelated `get()`/`isPresent()` on
    /// some other receiver can never match.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn ws_optional(
        &self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        raw: &[i64],
    ) -> Option<i64> {
        if !ws_optional_enabled() {
            return None;
        }
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let m = caller.file.pool.member_ref(cp)?;
        // Cheap name/descriptor gate BEFORE touching the heap: the overwhelming majority of
        // general-path callees are not these three, and this arm sits on the hot miss path.
        let want_value = match (m.name, m.descriptor) {
            ("isPresent", "()Z") | ("isEmpty", "()Z") => false,
            ("get", "()Ljava/lang/Object;") => true,
            _ => return None,
        };
        let recv_cell = crate::nextgen::forward_if_moved(raw.first().copied()?);
        let recv = self.ws_recv_of_class(recv_cell, "java/util/Optional")?;
        let present = self.synth_get_ro(&recv, "$present").as_int().unwrap_or(0) != 0;
        let out = if want_value {
            if !present {
                return None; // empty get() throws — that is the locked path's business
            }
            let v = self.synth_get_ro(&recv, "$val");
            jit::box_ref(self, &v)
        } else {
            let b = if m.name == "isPresent" { present } else { !present };
            jit::pack_local(&Value::Int(b as i32))
        };
        if crate::jit::freeze_resolve::stats_on() {
            crate::jit::freeze_resolve::WORKER_WSNATIVE_HIT
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Some(out)
    }

    /// 19.2 M4 native CubicSpline interval search — the body of `findIntervalStart([F locations, F value)`:
    /// `Mth.binarySearch(0, locations.length, i -> value < locations[i]) - 1`. Replicated EXACTLY in Rust
    /// (same lower-bound loop as Mth.binarySearch; `value < x` is false for NaN, matching the lambda's
    /// `fcmpg;ifge`) over the guest `float[]` read via its raw base pointer — so the ~1.45M interpreted
    /// predicate-lambda calls disappear. Returns None (⇒ bytecode) if the args don't match the shape.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn spline_find_interval_start(&self, args: &[Value]) -> Option<i32> {
        let arr = match args.first() {
            Some(Value::Array(a)) => *a,
            _ => return None,
        };
        let value = match args.get(1) {
            Some(Value::Float(f)) => *f,
            _ => return None,
        };
        // The `float[]` backing is `len` contiguous f32 at `base` (a Ref array ⇒ None ⇒ fall back).
        let (base, len) = self.heap.array_base_ptr(arr)?;
        // SAFETY: findIntervalStart's declared arg IS a float[] (descriptor-gated), so the buffer is
        // `len` aligned f32; the borrow is read-only and lives only for this synchronous call.
        let locs = unsafe { std::slice::from_raw_parts(base as *const f32, len) };
        // Mth.binarySearch lower-bound: smallest `min` in [0,len] with `value < locs[min]`, else len.
        let mut min = 0usize;
        let mut width = len;
        while width > 0 {
            let half = width / 2;
            let mid = min + half;
            if value < locs[mid] {
                width = half;
            } else {
                min = mid + 1;
                width -= half + 1;
            }
        }
        Some(min as i32 - 1)
    }

    /// 18.4 M4 parallel tokenize-prepass. On the first datapack parse, natively enumerate
    /// `/data/**.json` (generic — any resource root, no hard-coded list), read every file, and
    /// tokenize them ALL in parallel across pool-many PURE-RUST threads into `Json` ASTs cached by
    /// content. Tokenize touches neither the guest heap, the NurseryView, Shadow-COW, nor the GIL —
    /// it is lock-free over disjoint result slots, so no worker can race or re-enter the VM. One-time.
    /// The serial guest `build` then pulls each file's AST from the cache (identical AST ⇒ identical
    /// JsonElement ⇒ identical DataPack_HASH, deterministic by construction). Gated + inert when off.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn datapack_prewarm_tokenize(&mut self) {
        if !crate::datapack_cache::enabled() || !crate::datapack_cache::claim_prewarm() {
            return;
        }
        // ENUMERATE natively (generic /data resource tree, jars + dirs; no jar-specific file list).
        let mut paths: Vec<String> = self
            .loader
            .list_resources("data")
            .into_iter()
            .filter(|p| p.ends_with(".json"))
            .collect();
        paths.sort();
        // READ bytes serially (the jar reader is &mut; a read is cheap I/O vs the tokenize CPU cost).
        let mut texts: Vec<String> = Vec::with_capacity(paths.len());
        for p in &paths {
            if let Some(bytes) = self.loader.read_resource(p) {
                if let Ok(t) = String::from_utf8(bytes) {
                    texts.push(t);
                }
            }
        }
        let n = texts.len();
        // TOKENIZE in parallel — pure Rust, lock-free over DISJOINT result slots. No VM/heap/nursery/COW.
        let workers = crate::parallel_pool::RavaPool::global().workers().max(1).min(n.max(1));
        let mut results: Vec<Option<crate::gson_native::Json>> = (0..n).map(|_| None).collect();
        let t0 = std::time::Instant::now();
        if n > 0 {
            let rptr = results.as_mut_ptr() as usize;
            let texts_ref = &texts;
            let chunk = n.div_ceil(workers).max(1);
            std::thread::scope(|scope| {
                for w in 0..workers {
                    let lo = w * chunk;
                    let hi = ((w + 1) * chunk).min(n);
                    if lo >= hi {
                        break;
                    }
                    scope.spawn(move || {
                        let base = rptr as *mut Option<crate::gson_native::Json>;
                        for i in lo..hi {
                            let tree = crate::gson_native::parse(&texts_ref[i]);
                            // SAFETY: worker index ranges are disjoint ⇒ writes never alias.
                            unsafe { *base.add(i) = tree; }
                        }
                    });
                }
            });
        }
        let us = t0.elapsed().as_micros() as u64;
        // BUILD the content-keyed cache serially, after the join.
        let mut map = std::collections::HashMap::with_capacity(n);
        let mut ok = 0u64;
        for (t, tree) in texts.into_iter().zip(results) {
            if let Some(j) = tree {
                ok += 1;
                map.entry(t).or_insert(j);
            }
        }
        crate::datapack_cache::store(map, ok, us);
    }

}
