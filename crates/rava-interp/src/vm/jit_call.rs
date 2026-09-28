//! Entering compiled code, with the memo and the iterator shortcut in front of it.

use crate::*;

impl Vm {
    /// The compiled→host call choke. Default build: a straight tail-call to the real
    /// body, so the pure-memo arc costs one static bool load when it is off.
    pub(crate) fn jit_call(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> Result<i64, VmError> {
        // 20.3 wall-profile: the host excursion a PIC MISS takes out of compiled code. Without
        // this the whole excursion stayed billed to JIT_EXEC, so the profile's largest row was
        // machine code and its dispatch tail added together — two things a codegen change affects
        // completely differently. The callee's own tier (interp/jit) and native bridges push their
        // own category and override this for their duration, so JIT_CALLBACK ends up as the pure
        // miss-to-dispatch tax, the same way VM_DISPATCH is for the interpreter's invokes.
        //
        // ⚠ COST ON THE DEFAULT PATH, stated so it is checked rather than assumed. This is the
        // hottest host entry point in the engine — ~453M callbacks on a boot — so even a disabled
        // guard is not free. `enter` early-returns on a cached `OnceLock` read, which should
        // inline to a load, a test and a predicted branch: order 0.2-0.4s, i.e. ~0.3-0.5% of a
        // 66.5s boot. That is small but NOT below this tree's decision threshold (levers have been
        // refuted here at 2-3%), so it must show up in the next interleaved A/B rather than be
        // waved through. If it does cost, the fix is to compile the guard out behind a feature
        // rather than to keep an unmeasured tax.
        //
        // ⭐**AND IT DID COST — THREE TIMES OVER.** The paragraph above priced ONE cached
        // `OnceLock` read. There were three, and the count is what grows with the callback
        // population, not the code:
        //
        //   * `enter_site` opens with `rava_sampler::enabled()`  — one acquire load;
        //   * `pure_memo::armed()` is `enabled() || probe_enabled()`, and neither half is
        //     memoised, so it is **two more** acquire loads, not one.
        //
        // Three atomic acquire loads and three branches, per callback, to discover that all
        // three diagnostics are off — which they are in every production boot, because all
        // three derive from environment variables read once and never written again. At the
        // ~453M callbacks this function's own comment names, that is 1.36 BILLION acquire
        // loads whose answer was fixed before `main` ran.
        //
        // The cost is not the `mov`: it is that an acquire load is an optimisation barrier, so
        // LLVM may not hoist any of them out of the caller's loop nor fold the three branches
        // into one. That is the same shape as the relaxed atomic removed from the interpreter's
        // push path, which measured -15% on its probe.
        //
        // `jit_call_slow_gates()` is the OR of exactly those three, cached in one cell. When it
        // is false — always, in production — this is a direct tail-call to the body and the
        // three loads collapse to one. When ANY of them is armed the original sequence below
        // runs unchanged, so behaviour is identical in both directions by construction.
        // Kill switch `RAVA_NO_FAST_JITCALL=1` forces the gate true, i.e. the old path forever.
        if !jit_call_slow_gates() {
            return self.jit_call_body(caller_ptr, cp, kind, raw);
        }
        // ONE gate check for both the category and the site: see `enter_site`.
        let _wp = crate::rava_sampler::enter_site(
            crate::rava_sampler::JIT_CALLBACK,
            caller_ptr as usize,
            cp as u32,
        );
        #[cfg(feature = "next-gen-gc")]
        if crate::pure_memo::armed() {
            return self.jit_call_memo(caller_ptr, cp, kind, raw);
        }
        self.jit_call_body(caller_ptr, cp, kind, raw)
    }

    /// PURE-CALL MEMO interception. Every call whose (site, receiver class) the rule
    /// admits arrives here because `dispatch_resolve_cacheable_cls` refused to let the
    /// machine-code PIC cache it — so this is the complete view of those calls from
    /// compiled code. Opens/joins the frozen window, serves a recorded result, or runs
    /// the call and records it.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_call_memo(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> Result<i64, VmError> {
        // Instance dispatch only (a receiver is what makes the key an identity), and
        // only while this thread is the sole possible mutator of guest state.
        if !(kind == 1 || kind == 3) || raw.is_empty() || !crate::pure_memo::exclusive() {
            return self.jit_call_body(caller_ptr, cp, kind, raw);
        }
        let recv = crate::nextgen::forward_if_moved(raw[0]);
        let Some(ck) = self.pure_memo_class_key(recv) else {
            return self.jit_call_body(caller_ptr, cp, kind, raw);
        };
        let site = (caller_ptr as i64) | ((cp as i64) << 48);
        let entry = match crate::pure_memo::site_get(site, ck) {
            Some(e) => e,
            None => {
                let caller: &LoadedClass = unsafe { &*caller_ptr };
                let Some(m) = caller.file.pool.member_ref(cp) else {
                    return self.jit_call_body(caller_ptr, cp, kind, raw);
                };
                let (name, desc) = (m.name.to_string(), m.descriptor.to_string());
                let Some(cls) = self.pure_memo_class_name(recv) else {
                    return self.jit_call_body(caller_ptr, cp, kind, raw);
                };
                let e = self.pure_memo_admit(&cls, &name, &desc);
                crate::pure_memo::site_put(site, ck, e);
                e
            }
        };
        if !entry.verdict.intercepts() {
            return self.jit_call_body(caller_ptr, cp, kind, raw);
        }
        let slots = entry.slots as usize;
        let memoizable = crate::pure_memo::enabled()
            && entry.verdict == crate::pure_memo::Verdict::Memo
            && slots > 0
            && raw.len() >= slots
            && (0..slots).all(|i| {
                entry.ref_mask >> i & 1 == 0
                    || self.pure_memo_cell_stable(crate::nextgen::forward_if_moved(raw[i]))
            });
        let mut cells = [0i64; crate::pure_memo::MAX_CELLS];
        if memoizable {
            for (i, c) in cells.iter_mut().enumerate().take(slots) {
                *c = if entry.ref_mask >> i & 1 == 1 {
                    crate::nextgen::forward_if_moved(raw[i])
                } else {
                    raw[i]
                };
            }
        }
        let scope = crate::pure_memo::Scope::enter(
            entry.code,
            if memoizable { &cells[..slots] } else { &[] },
            memoizable,
        );
        if let Some(bits) = scope.lookup() {
            crate::vm::aot_route_diag::fastpath();
            return Ok(bits);
        }
        let r = self.jit_call_body(caller_ptr, cp, kind, raw);
        if let Ok(bits) = &r {
            scope.store(*bits);
        }
        r
    }

    /// Receiver class key — the same discriminator the dispatch PIC uses (arena header
    /// word, or the handle heap's interned class word). `None` for anything else.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn pure_memo_class_key(&self, recv: i64) -> Option<i64> {
        let raw = recv as u64;
        if recv != 0 && raw >> 40 == 0 {
            return Some(unsafe { *(recv as *const u32) } as i64);
        }
        if raw >> 40 == 1 {
            return self.heap.object_class_word((recv & (crate::value::REF_TAG - 1)) as u32);
        }
        None
    }

    /// Concrete class name of a receiver cell (site-table miss path only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn pure_memo_class_name(&self, recv: i64) -> Option<String> {
        let raw = recv as u64;
        if recv != 0 && raw >> 40 == 0 {
            let ap = crate::arena::ArenaPtr::new(recv as *mut u8)?;
            return crate::nextgen::class_name(crate::nextgen::arena_class_id(ap)).map(|c| c.to_string());
        }
        if raw >> 40 == 1 {
            return self
                .heap
                .class_of((recv & (crate::value::REF_TAG - 1)) as u32)
                .map(|c| c.to_string());
        }
        None
    }

    /// Serve `Iterator.hasNext()Z` / `next()Ljava/lang/Object;` for an `$elems`-backed receiver
    /// without building the synthetic frame or entering `opcodes::invoke*`. Returns the packed
    /// result, or None to take the normal path. See the call site for why this sits here.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_iter_shortcut(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> Option<i64> {
        if !jit_iter_shortcut_enabled() {
            return None;
        }
        // virtual (1) / interface (3) only - a static or special call has no receiver here.
        if kind != 1 && kind != 3 {
            return None;
        }
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let m = caller.file.pool.member_ref(cp)?;
        // Narrow on the NAME first: no heap is touched for the overwhelming majority of calls.
        enum Shape {
            HasNext,
            Next,
            Entry(&'static str),
        }
        let shape = match m.name {
            "hasNext" if m.descriptor == "()Z" => Shape::HasNext,
            "next" if m.descriptor == "()Ljava/lang/Object;" => Shape::Next,
            "getKey" if m.descriptor == "()Ljava/lang/Object;" => Shape::Entry("$key"),
            "getValue" if m.descriptor == "()Ljava/lang/Object;" => Shape::Entry("$val"),
            _ => return None,
        };
        // The iterator arm keeps the JDK interface-name check because `$elems` alone would also match
        // an unrelated receiver reached through some other single-arg no-result method; the ENTRY
        // arm deliberately does NOT check the owner at all. Its 3.68M-call caller is a fastutil
        // interface, and naming that class here would adapt the engine to a particular jar - so
        // the admission test is OUR OWN SHAPE instead: the receiver must carry the synthetic
        // `$key`/`$val` fields, a namespace no guest class can enter. That covers the JDK
        // `Map$Entry` and every third-party entry interface at once, by construction.
        if matches!(shape, Shape::HasNext | Shape::Next)
            && !matches!(m.owner, "java/util/Iterator" | "java/util/ListIterator")
        {
            return None;
        }
        let Value::Ref(h) = jit::unbox_ref(self, *raw.first()?, m.owner) else {
            return None;
        };
        if let Shape::Entry(field) = shape {
            static KEY_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            static VAL_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            let cell = if field == "$key" { &KEY_ID } else { &VAL_ID };
            let id = match cell.get() {
                Some(&id) => id,
                None => {
                    let id = self.heap.field_id_opt(field)?;
                    let _ = cell.set(id);
                    id
                }
            };
            // SHAPE TEST, not a value test: `get_field` cannot tell "absent" from "present and
            // null", and an entry whose value IS null must still be served.
            let slot = self.heap.field_slot(h, id, 0)?;
            let (v, _) = self.heap.get_field_hint(h, id, slot);
            if iter_shortcut_stats() {
                use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
                static SERVED: AtomicU64 = AtomicU64::new(0);
                let n = SERVED.fetch_add(1, Relaxed) + 1;
                if n % 1_000_000 == 0 {
                    eprintln!("[ENTRY-SHORTCUT] {n} entry accessors served before the gauntlet");
                }
            }
            return Some(jit::box_ref(self, &v));
        }
        let want_next = matches!(shape, Shape::Next);
        // ONE cell for the PAIR, and it is the same cell the native iterator arms already fill:
        // [`Vm::fu_iter_field_ids`] caches exactly `($elems, $pos)` and resolves both or
        // neither — the identical all-or-nothing rule the two local `OnceLock`s here enforced
        // with `?` on each. Two acquire loads per served call become one, a second copy of the
        // memo stops existing, and the two callers can no longer disagree about what is cached.
        let (eid, pid) = self.fu_iter_field_ids()?;
        // `$elems` MUST decode to a real Array - that is the same guard the native fast path
        // uses, and it is what distinguishes our synthetic iterator from a jar one.
        // HINT 0 IS DELIBERATE - remembering the last slot in a static was BUILT AND REFUTED.
        // `Vm::jit_call_body` is the largest engine symbol in the serial profile (5.0%) and most of
        // it is this shortcut, so caching `$elems`/`$pos`/`$key`/`$val` slots looked obvious. It is
        // not: these synthetic objects carry two or three fields, so the "scan" a hint avoids is one
        // or two compares, while the hint costs two relaxed atomic loads and two stores on every one
        // of ~7M served calls. Measured, 6 reps interleaved: clean-run Done minima 22.926 with the
        // hint vs 22.770 without - slightly AGAINST - and clean means 23.19 vs 23.41, i.e. nothing.
        // Same failure mode as the two memos reverted earlier: a cache in front of an already-short
        // path is a tax. Do not re-derive it from the profile.
        let (ev, _) = self.heap.get_field_hint(h, eid, 0);
        let Value::Array(arr) = ev else { return None };
        let len = self.heap.array_len(arr).unwrap_or(0);
        let (pv, ph) = self.heap.get_field_hint(h, pid, 0);
        let pos = pv.as_int().unwrap_or(0) as usize;
        // SERVED counter: proves the shortcut actually fires, and how often. The fallback tally
        // cannot answer that - its note is taken BEFORE this point, so it counts entries into the
        // fallback path whether or not they are then served here.
        // Gated: an unconditional atomic here would be 7M RMWs in the ARMED arm only, biasing the
        // very A/B this exists to inform. Measured 7,000,000+ served per boot with it on.
        // MEMOISED gate - a raw `env::var_os` here would be an environment-block walk per served
        // call, which is the exact defect this session removed from `Vm::invoke`.
        if iter_shortcut_stats() {
            use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
            static SERVED: AtomicU64 = AtomicU64::new(0);
            let n = SERVED.fetch_add(1, Relaxed) + 1;
            if n % 1_000_000 == 0 {
                eprintln!("[ITER-SHORTCUT] {n} iterator calls served before the gauntlet");
            }
        }
        if !want_next {
            return Some(jit::pack_local(&Value::Int((pos < len) as i32)));
        }
        // Exhaustion must reach the ordinary iterator's throwing path. Do not
        // turn it into a null element or advance the cursor on a failed next().
        if pos >= len { return None; }
        // Preserve the native iterator's fail-fast check before consuming an element.
        // A mismatch falls through to its ordinary exception path with the cursor intact.
        if crate::stream::collections::iterator_source_changed(self, h) { return None; }
        let v = self.heap.array_get(arr, pos)?;
        self.heap.put_field_hint(h, pid, Value::Int(pos as i32 + 1), ph);
        // Reference return: the compiled caller holds it as a JT::Ref cell, exactly as the
        // normal path's `ref_ret` arm does.
        Some(jit::box_ref(self, &v))
    }
}

/// `RAVA_ITER_SHORTCUT_STATS` — ONE cell for ONE environment variable. The entry arm and the
/// iterator arm each carried their own `OnceLock` over the same name, so a served call paid an
/// acquire load to answer a question the other arm had already cached. The counters stay
/// separate (they report different populations); only the gate is shared.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn iter_shortcut_stats() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ITER_SHORTCUT_STATS").is_some())
}

/// `RAVA_NO_FAST_JITCALL=1` (or the block switch `RAVA_NO_EMIT_MORE=1`) restores every
/// pre-existing sequence in the jitcall area byte for byte: the three separate gate reads in
/// [`Vm::jit_call`], the unconditional site hashing and counter reads in `Vm::jit_fu_invoke`,
/// and the copy-then-compare in `Vm::jit_str_startswith`. Read once — a `var_os` per callback
/// is the very defect these removals are about.
pub(crate) fn fast_jitcall_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_JITCALL").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Is ANY of the per-callback slow arms in [`Vm::jit_call`] wired at all?
///
/// The three it ORs together — the wall sampler, the pure-memo server and the pure-memo census —
/// are each a `OnceLock<bool>` over an environment variable, so their disjunction is as constant
/// as they are and may be cached the same way. Reading it costs one acquire load instead of
/// three, and a false answer proves all three arms are dead for the whole process.
///
/// TABLE SIZE: one `OnceLock<bool>`, 16 bytes of static, process-lifetime. It replaces reads, it
/// does not add a lookup — nothing is keyed, so there is no probe to lose against.
#[inline]
fn jit_call_slow_gates() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        // Kill switch: take the original sequence forever, so one binary can A/B both arms.
        if !fast_jitcall_enabled() {
            return true;
        }
        if crate::rava_sampler::enabled() {
            return true;
        }
        // `armed()` is itself `enabled() || probe_enabled()` — the two loads named above.
        // Without next-gen-gc the memo arm is not compiled at all, so nothing else can want
        // the slow path.
        #[cfg(feature = "next-gen-gc")]
        if crate::pure_memo::armed() {
            return true;
        }
        false
    })
}
