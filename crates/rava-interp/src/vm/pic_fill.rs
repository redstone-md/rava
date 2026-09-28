//! Filling the inline caches: the heap-reference PIC and the lambda PIC.

use crate::*;

impl Vm {
    /// 23.4b heap-Ref PIC cache fill: resolve a DiscreteVoxelShape heap-Ref receiver's call to a
    /// GC-LEAF compiled fptr and write `(arc_key, fptr)` into the site's 4-way cache — the machine-code
    /// heap-Ref PIC then serves the next same-class call directly. Leaf-only (a plain call_indirect, no
    /// rooting). Large aligned `arc_key`s and small arena `class_id`s coexist without collision.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn heappic_fill(&mut self, cache_ptr: i64, recv: i64, caller_ptr: *const LoadedClass, cp: u16) {
        if (recv as u64) >> 40 != 1 {
            return; // heap Ref only (REF_TAG=1<<40)
        }
        // A site with no cache box has nothing to fill, and every way access below is a RAW load
        // through this word. `pic_native::try_fill` and `try_fill_ref` — the two other writers of
        // this very cache — have always opened with this guard; this fill dereferenced first and
        // asked never. It changes no live path (`emit_dispatch_pic` bakes a real `cache_eff`, and
        // `cap_thunk::build_site_thunk` refuses a site with `cache_ptr == 0` outright), which is
        // exactly why it is worth one predicted compare: it removes the single way a future
        // emitter could turn a missing cache into a null-page read.
        if cache_ptr == 0 {
            return;
        }
        let handle = (recv & (crate::value::REF_TAG - 1)) as u32;
        // ⭐**ONE OBJECT LOOKUP, NOT THREE.** The bail's class NAME, the cache KEY and the
        // resolve's class name are three reads of one `Object`, and this fill did three separate
        // `objects[handle]` walks to get them (`class_of` → `object_class_word` → `class_of`).
        // The borrow is held across the bail and the negative probe — neither touches `self`
        // mutably — and is released by the single `Arc` clone below, which is the last use.
        let Some(obj) = self.heap.get(handle) else { return };
        let cname: &str = &obj.class;
        // FAST NEGATIVE BAIL (V2 Fix A): a `$`-synthetic (lambda) or `java/` receiver class is
        // PERMANENTLY NeverCacheable — dispatch_resolve_cacheable_cls rejects both by an unconditional
        // prefix test, with no other side effect. Screen the BORROWED class name HERE, before the
        // name/desc/cls String allocations + the resolve_method walk. The hot voxel-join
        // IndexConsumer.merge site dispatches on `$Lambda` millions of times; without this early-out each
        // paid 2 to_string()s + a full resolve for a guaranteed-negative verdict (the arena path already
        // skips such sites). Behaviour-identical: heappic_fill only FILLS the site cache, and a `$`/`java/`
        // class can never yield a servable (fp>0) entry, so declining to fill cannot change which method
        // dispatches — only the wasted per-call re-resolution work (and its alloc storm) is removed.
        // ⭐The `java/` half of this bail rests on "this tree carries no `java/util` bytecode",
        // which stops being true the moment a class is overridden by a real classfile — and
        // then the bail denies a machine-code PIC to a receiver that DOES have a compiled
        // body, which is the opposite of what it is for.
        if cname.starts_with('$')
            || (cname.starts_with("java/") && !crate::class_loader::has_bytecode(cname))
        {
            // NAME WHAT THIS BAIL COSTS (picprobe-gated, inert otherwise). The bail is
            // correct — this tree carries no `java/util` bytecode, so a `java/*` receiver has
            // no compiled body a machine-code PIC could call — but it is also 19.4 M of the
            // 19,856,228 refusals `[HEAPPIC-LIVE]` reports, and the clause census in
            // `dispatch_resolve_cacheable_cls` cannot see them BECAUSE of this short-circuit
            // (its `java/` clause reads 0 while `total` reads 434,202). Without a name under
            // the number there is no way to rank which natives are worth an inline-emitted
            // entry, which is the only known cure.
            //
            // ⚠**NOTHING IS ALLOCATED OR CLONED ON THIS ARM, DELIBERATELY.** It is the single
            // busiest path in the file, and `obj.class` is an `Arc<str>`: hoisting the clone
            // above the bail to share it with the resolve would put 19.4 M atomic refcount
            // round-trips on a path that today does two `starts_with` and returns.
            if crate::picprobe::enabled() {
                let caller: &LoadedClass = unsafe { &*caller_ptr };
                if let Some(m) = caller.file.pool.member_ref(cp) {
                    crate::icmiss::note_never_class(6, cname, m.name);
                }
            }
            crate::heappic::note_fill(true);
            return;
        }
        // ⭐**NEGATIVE-CACHE PROBE — THE ARENA PATH HAS HAD ONE ALL ALONG AND THIS ONE DID NOT.**
        //
        // A `-1` fill means PROVEN never-IC-servable for this (site, class). The machine code
        // treats only `fp > 0` as a hit, so a `-1` slot sends the NEXT call right back here — and
        // without this probe "the verdict is permanent" was true of the STORE and false of the
        // WORK: every call re-allocated `name`, `desc` and `cls`, re-read the class word and
        // re-walked `dispatch_resolve_cacheable_cls`, only to write the same `-1` again.
        //
        // `rava_dispatch_miss` (the ARENA twin, `vm/dispatch.rs`) probes exactly this and says
        // why: *"skip the per-call resolve — on an uncacheable hot site that was the dominant
        // dispatch cost."* The handle-heap half never got it, and the handle heap is where the
        // collections live.
        //
        // What it costs, measured rather than argued — `RAVA_CALLBACK_STATS` on a product boot:
        //
        // ```text
        // [ICMISS] total=9,057,565  native_served_deep=8,757,132 (97%)
        // LongOpenHashSet#isEmpty  3,420,033     ReferenceOpenHashSet#isEmpty  933,364
        // LongArrayFIFOQueue#isEmpty 3,107,170   ObjectArrayList#isEmpty       931,380
        // ```
        //
        // **8.39 M of 9.06 M are `isEmpty` on four fastutil collections** — the cheapest question
        // a collection can be asked, paying a full host resolve every single time. The `$`/`java/`
        // bail above cannot catch them: fastutil is neither.
        //
        // Behaviour-identical for the same reason that bail is: `heappic_fill` only FILLS a site
        // cache. Declining to re-derive a verdict already recorded in that very cache cannot
        // change which method dispatches — the EXECUTE half runs the call either way.
        //
        // ⚠**AND IT PROBES EXACTLY FOUR WAYS, WHICH IS THE RIGHT NUMBER HERE AND ONLY HERE.**
        // The arena twin was widened to `site_ways()` (16 on an IC_PARALLEL worker) because the
        // arena FILL writes that many and the emitted arena probe branches into a 16-way arm.
        // The heap-Ref half has neither: `heappic_fill` writes 4 and `emit_refpic_probe` is a
        // fixed 4-way chain for main and worker alike. Widening this probe alone would look for
        // verdicts nothing writes; widening the fill alone would write entries no emitted probe
        // can read. Probe, fill and codegen move together or not at all.
        let Some(arc_key) = self.heap.object_class_word(handle) else { return };
        let known_negative = unsafe {
            let cache = cache_ptr as *const i64;
            (0..4usize).any(|k| *cache.add(k * 2) == arc_key && *cache.add(k * 2 + 1) == -1)
        };
        if known_negative && !pic_neg_disabled() {
            crate::diag::pic_neg_hit().inc();
            crate::heappic::note_fill(true);
            return;
        }
        // METERED: this is the work the probe above skips, and the first thing the new `diag`
        // meter was pointed at — because its price is already known from an independent six-boot
        // CPU A/B (-0.28%, ~10 ns/event), so what the meter reads here validates the meter.
        //
        // ⭐**THREE ALLOCATIONS PER RESOLUTION FOR THREE VALUES THAT ARE ONLY READ** — the same
        // defect, in the same shape, that `dispatch_resolve_cacheable` (the ARENA twin, twenty
        // lines up in `vm/dispatch.rs`) already had removed. `MemberRef` hands back `&str`s
        // straight into the caller's constant pool; `dispatch_resolve_cacheable_cls` takes
        // `&str` and mutates none of them. The two `to_string()`s plus the class-name
        // `to_string()` that stood here were a malloc + a memcpy proportional to the LENGTH of
        // the method name, the descriptor and a 40-60 byte class name, per resolution — and
        // `[HEAPPIC-LIVE]`'s own `FILL_NOTYET` line says a `NotYet` verdict writes nothing and
        // pays all three again on the very next call.
        //
        // Was three allocations and O(len(name)+len(desc)+len(class)) bytes copied; is one `Arc`
        // refcount bump and no copy at all. The borrows line up because neither source belongs to
        // `self`: `caller` comes from a raw pointer, and `cls` is an owned `Arc<str>` local — the
        // clone below is the last use of `obj`, so `self.heap`'s borrow ends there and the
        // `&mut self` resolve is free to run.
        //
        // ⚠**AND THIS IS 0.28% OF THE BOOT, NOT MORE.** `seam.pic_resolve` meters the region
        // below at **9,473,938 events, 10.4 ns/event, est 0.099 s of a 35.2 s wall**. The
        // allocations are a share of that share. Removing them costs nothing and risks nothing,
        // but nobody should expect it above the noise floor — the entry that pays for itself in
        // this file is the eviction rule at the bottom, which decides how many dispatches stay
        // in machine code rather than how fast the host re-derives one that does not.
        // The meter opens BEFORE the clone, exactly where it opened before the three
        // `to_string()`s, so `seam.pic_resolve` keeps measuring the same region and the two
        // numbers stay comparable across this change.
        let _t = crate::diag::pic_resolve().start();
        let cls = obj.class.clone();
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let Some(mref) = caller.file.pool.member_ref(cp) else { return };
        let fill = match self.dispatch_resolve_cacheable_cls(&cls, mref.name, mref.descriptor) {
            // Cache compiled entries in the unified rooted hit arm. The low bit remains
            // target metadata: clear for GC-leaf, set for a self-registering non-leaf
            // admitted under `ic_deep`; machine code masks it before the call. Rooting
            // is safe for both classes and avoids a per-caller routing diamond.
            DispatchResolve::Compiled(fptr) => {
                crate::heappic::note_fill(false);
                fptr as i64
            }
            DispatchResolve::NeverCacheable => {
                crate::heappic::note_fill(true);
                -1
            }
            DispatchResolve::NotYet(_) => {
                crate::vm::pic_fill::pic_not_yet_note(caller_ptr, cp);
                if crate::picprobe::enabled() {
                    crate::heappic::FILL_NOTYET
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                return; // not compiled yet — retry on a later call
            }
        };
        // Slot policy — see `pic_install`. Was "first free way → matching key → evict by key
        // hash", which let a permanently-unservable `-1` displace one of four classes the
        // machine-code arm was serving without ever leaving compiled code.
        pic_install(cache_ptr, arc_key, fill, 3, caller_ptr, cp);
        // P3-S1: mirror the heap-Ref verdict into the freeze-resolve table (same reason as
        // the arena fill — the 4-way evicts, the table remembers every arc_key per site).
        // STABLE key (caller|cp<<48) — see the arena-fill mirror. Run UNCONDITIONALLY, including
        // for a verdict `pic_install` declined to give a way: the decline is about which four
        // classes are worth machine-code service at this site, not about whether the verdict is
        // known, and this table has no way limit. (It is worker-only in production — `publishable`
        // drops main's puts — so on the serial boot this is one thread-local read.)
        crate::jit::freeze_resolve::put((caller_ptr as i64) | ((cp as i64) << 48), arc_key, fill);
    }

    /// λ-PIC cache fill (RAVA_LAMBDA_PIC, default OFF): on a machine-code PIC miss whose
    /// receiver is a NON-CAPTURING directable lambda SINGLETON, cache
    /// `((recv << 1) | 1, fptr | DEEP | SHIFT)` in the site's 4-way cache so the gated
    /// heap-Ref arm (`emit_lambda_key_select`) serves every later call of this
    /// (site, lambda) pair with a direct machine-code call — no `rava_dispatch_miss`.
    /// Fill-only, exactly like `heappic_fill`: the current call proceeds via `jit_call`.
    ///
    /// SOUNDNESS — a machine-code identity hit can never re-validate anything, so every
    /// per-call guard the Rust direct path performs must be made STATIC before filling:
    ///  • IMMORTAL IDENTITY: only handles in `indy_singleton_handles` are filled.
    ///    `indy_singletons` is insert-once per site, never removed, and rooted on every
    ///    collect — the handle can never be swept + reused, which is the only way the
    ///    identity key could alias (this replaces `jit_direct_call`'s per-call lambda_fp
    ///    re-check; a non-capturing lambda's content is immutable, so identity ⇒ target).
    ///  • KEY SHAPE: `(recv << 1) | 1` is odd (class words are 8-aligned ⇒ never odd) and
    ///    injective over receiver cells (`recv | 1` would alias handles 2k and 2k+1).
    ///  • ENTRY SHAPE: identity-guarded static-ref entries only (guard == cell, shift,
    ///    cap_n == 0, arg0_guard == 0). A kind-5/9 unbound method ref re-checks arg0's
    ///    DYNAMIC class per call and a capturing lambda re-fetches its (GC-movable)
    ///    captures per call — machine code can do neither, so both are refused. What
    ///    remains is a kind-6 non-capturing static ref whose impl descriptor EQUALS the
    ///    call-site descriptor (verified by `jit_direct_resolve`), so the slot-laid PIC
    ///    buffer minus the receiver slot IS the callee's locals layout (wide args
    ///    included — both sides use the same slot widths).
    ///  • ROOTING — every admissible entry uses the same rooted hit:
    ///      – `self_reg` NON-LEAF → fptr|1|2. The caller spills its live references
    ///        and the callee's v3 prologue registers and zeroes its shifted frame.
    ///        `ic_deep` remains the admission policy for this class.
    ///      – GC-LEAF → fptr|0|2. The caller performs the same spill/reload even
    ///        though the body has no safepoint. This is redundant but sound:
    ///        `is_gc_leaf` refuses every invoke/alloc/ldc-ref/athrow/monitor/static
    ///        opcode, so no collection can run inside it.
    ///    A body that is neither leaf nor self-registering would need the Rust
    ///    wrapper's `cells_push`, which a machine-code hit skips, and is refused,
    ///    as is a frame that outgrows the call buffer.
    #[cfg(feature = "next-gen-gc")]
    #[allow(clippy::too_many_arguments)]
    /// See `lpic_refused` — the λ-PIC's own admission census.
    pub(crate) fn lambda_pic_fill(
        &mut self,
        cache_ptr: i64,
        recv: i64,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        buf: *const i64,
        n: i64,
    ) {
        if !crate::jit::lambda_pic_enabled() || !matches!(kind, 1 | 3) {
            return;
        }
        // Preserve the established admission policy: a self-registering non-leaf
        // fill requires `ic_deep`; a GC-leaf fill does not.
        let deep_ok = crate::jit::ic_deep_enabled();
        if (recv as u64) >> 40 != 1 {
            // ⚠SUSPECT. A paired A/B of `RAVA_LAMBDA_PIC` reads −0.18% with the arms overlapping
            // and 2/3 signs mixed — a null. Before believing it, count what this fill REFUSES:
            // lambdas moved to the ARENA (`nextgen::lambdas`), an arena object is a raw 0b00
            // pointer, and this is a heap-handle-only test standing in front of the population.
            // `miss_shape` puts **23.2 M of the 51.3 M PIC misses on ARENA receivers**. Row 324's
            // shape, a third time. A null from a mechanism that never fires measures the gate.
            lpic_refused(0);
            return; // heap Ref only (REF_TAG = 1<<40; BOX/ARRAY/ABOX are 2/4/8)
        }
        // GIL-free region: the resolve below mutates Vm (loader / jit_direct_cache) —
        // serial main thread only. Workers keep falling to the (frozen) general path.
        if crate::parallel::gilfree() {
            lpic_refused(1);
            return;
        }
        let handle = (recv & (crate::value::REF_TAG - 1)) as u32;
        match self.heap.lambda_directable_why(handle) {
            0 => {}
            1 => {
                lpic_refused(5); // not a lambda at all — the correct, cheap skip
                return;
            }
            2 => {
                lpic_refused(6); // a CAPTURING lambda
                return;
            }
            _ => {
                lpic_refused(7); // a lambda of a kind this arm does not serve
                return;
            }
        }
        if !self.indy_singleton_handles.contains(&handle) {
            lpic_refused(3);
            return;
        }
        lpic_refused(4);
        // Resolve through the SAME per-site direct-call cache `jit_direct_call` serves —
        // one authoritative resolve path (`jit_direct_resolve`), shared entries, shared
        // negative cache (re-probe every 64th miss so a later-compiling target is
        // picked up), shared megamorphic bound.
        let site = (caller_ptr as usize, cp);
        let guard = recv as usize; // identity guard: the raw receiver cell bits
        let cached = self
            .jit_direct_cache
            .get(&site)
            .and_then(|by_class| by_class.get(&guard))
            .and_then(|v| v.iter().find(|e| e.guard == guard).copied());
        let entry = match cached {
            Some(e) => e,
            None => {
                let nkey = (caller_ptr as usize, cp, guard);
                if let Some(c) = self.jit_direct_neg.get_mut(&nkey) {
                    *c = c.wrapping_add(1);
                    if *c % 64 != 0 {
                        return;
                    }
                }
                // ROOT the operand buf across the resolve (a class load can collect) —
                // same discipline as the arena fill above / jit_direct_call.
                let len = n.max(0) as usize;
                frames::cells_push((buf, len));
                let raw = unsafe { std::slice::from_raw_parts(buf, len) };
                let resolved = self.jit_direct_resolve(caller_ptr, cp, kind, raw, site);
                frames::cells_pop();
                match resolved {
                    Some(e) => {
                        Self::debug_assert_not_gilfree("jit_direct_cache (lambda_pic_fill)");
                        self.jit_direct_cache
                            .entry(site)
                            .or_default()
                            .entry(guard)
                            .or_default()
                            .push(e);
                        self.jit_direct_neg.remove(&nkey);
                        e
                    }
                    None => {
                        Self::debug_assert_not_gilfree("jit_direct_neg (lambda_pic_fill)");
                        self.jit_direct_neg.entry(nkey).or_insert(0);
                        return;
                    }
                }
            }
        };
        // Machine-code-serveable subset only — see the soundness doc above.
        if entry.guard != guard || !entry.shift || entry.cap_n != 0 || entry.arg0_guard != 0 {
            crate::picprobe::lpic_refuse(0); // wrong entry shape for a machine-code hit
            return;
        }
        // ROOTING: pick the arm this callee body can be served on. DEEP wins when both
        // apply (a self_reg body is non-leaf by construction, so they are disjoint in
        // practice); neither ⇒ refuse.
        let deep = entry.self_reg && deep_ok;
        if !deep && !entry.leaf {
            crate::picprobe::lpic_refuse(1); // neither self_reg-deep nor GC-leaf
            return;
        }
        // The shifted frame must fit the PIC call buffer: the deep arm's self-reg
        // prologue registers + zeroes buf_len slots starting at bufaddr + 8. A leaf
        // callee only reads locals[0..max_locals] there (nothing scans the rest), so
        // this is stricter than it needs to be for the leaf arm — deliberately: one
        // bound, and these bodies are far under it.
        if (entry.buf_len as usize) + 1 > crate::jit::dispatch_callbuf_slots() {
            crate::picprobe::lpic_refuse(2); // shifted frame outgrows the call buffer
            return;
        }
        // Publish the interned "$Lambda" class word (the machine-code select is inert
        // while it is 0). The interner pins the Arc for the Vm's lifetime.
        if crate::jit::LAMBDA_CLASS_WORD.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            let arc = self.heap.intern_class("$Lambda");
            let word = unsafe { *(&arc as *const std::sync::Arc<str> as *const i64) };
            crate::jit::LAMBDA_CLASS_WORD.store(word, std::sync::atomic::Ordering::Relaxed);
        }
        let key = (recv << 1) | 1;
        // bit0 classifies DEEP versus LEAF for admission/census and is masked by
        // the rooted hit; bit1 shifts away the lambda receiver slot.
        let fill = (entry.fptr as i64) | if deep { 1 } else { 0 } | 2;
        crate::picprobe::lpic_fill(deep);
        // ONE slot policy for both fills in this file — see `pic_install`. The λ-PIC's victim
        // shift is 1, not 3: its keys are `(recv << 1) | 1`, so bit 3 of an identity key is bit 2
        // of the receiver cell, and hashing from bit 1 spreads consecutive handles across the
        // four ways instead of grouping them in pairs. A λ entry is always servable (`fill > 0`),
        // so the decline arm below is unreachable from here by construction.
        pic_install(cache_ptr, key, fill, 1, caller_ptr, cp);
    }
}

/// **INSTALL `(key, fill)` INTO A SITE'S 4-WAY HEAP-REF CACHE.** Returns `false` only when the
/// write was DECLINED (rule 4). Shared by both fills in this file so the two policies cannot
/// drift apart, and kept next to them because it encodes what the EMITTED probe assumes.
///
/// **4 WAYS, AND DELIBERATELY NOT MORE.** The emitted heap-Ref probe (`emit_refpic_probe`) is a
/// fixed four-compare chain for main and worker alike, unlike the arena probe, which branches
/// into a 16-way arm under IC_PARALLEL. Widening this fill alone would publish entries no
/// emitted probe can read — the mirror image of the arena defect where the PROBE read 4 of the
/// 16 ways its own FILL wrote, and every one of the other 12 permanent verdicts was invisible.
///
/// PRIORITY, in order:
///  1. **A way already holding this key** — refresh in place. This is what keeps the cache free
///     of DUPLICATE keys, and the emitted probe depends on that: it is a branchless select chain
///     in which the LAST matching way wins, so one key in two ways makes *which* entry serves
///     depend on where the fill happened to land. The old scan claimed the first FREE way before
///     it had looked at the later ways at all — sound only while fills land strictly in order,
///     which is true of this fill and NOT true of the IC_PARALLEL backfill in
///     `rava_dispatch_miss` (it evicts by `key & 15`, so it can leave a hole below a live way).
///  2. A free way (`fptr == 0`).
///  3. **Installing a SERVABLE entry (`fill > 0`) — evict an UNSERVABLE way before a live one.**
///     A positive way is served entirely inside compiled code; a negative way only spares the
///     host a re-derivation it can redo in ~10 ns (`seam.pic_resolve`: 9,473,938 events at
///     10.4 ns). Preferring the cheap victim costs nothing — the survey has already visited
///     every way — and it can only ever convert a host excursion into a machine-code hit.
///  4. **Installing a NEGATIVE entry with no free, matching or negative way left — DECLINE.**
///     The old policy evicted by `(key >> shift) & 3`: an incoming COLD class, one this cache has
///     just PROVEN can never be served from machine code, displacing one of four HOT classes that
///     were being served without leaving compiled code — and since the victim is a deterministic
///     function of the incoming key, the same live way was displaced on every call, the displaced
///     class re-filled by evicting another, and the site thrashed all four. `pic_native`'s
///     `try_fill_ref` already refuses exactly this trade on exactly this cache — *"a site whose
///     four ways are all live positive entries is DECLINED ... evicting a real machine-code hit
///     to install a host call is a straight loss"* — but the fill that WRITES the `-1` never
///     adopted the same rule. What a declined pair gives up is the negative-probe short-cut at
///     the top of `heappic_fill`, i.e. one ~10 ns re-derivation per call; what it keeps is a
///     machine-code hit per call for a class that has one.
///
/// SAFETY: `cache_ptr` is the site's baked `[i64; 10]` box (or, under IC_PARALLEL, the thread's
/// own 256 B slice); both callers have already refused a zero. Only the first four `(key, fptr)`
/// pairs are touched, which is inside the smaller of the two.
#[cfg(feature = "next-gen-gc")]
fn pic_install(
    cache_ptr: i64,
    key: i64,
    fill: i64,
    victim_shift: u32,
    caller_ptr: *const LoadedClass,
    cp: u16,
) -> bool {
    const WAYS: usize = 4;
    let cache = cache_ptr as *mut i64;
    let victim = ((key as usize) >> victim_shift) & (WAYS - 1);
    if !fast_picfill2_enabled() {
        // LEGACY SHAPE, word for word: first free way → matching key → evict by key hash.
        unsafe {
            let mut slot = None;
            for k in 0..WAYS {
                if *cache.add(k * 2 + 1) == 0 {
                    slot = Some(k);
                    break;
                }
                if *cache.add(k * 2) == key {
                    slot = Some(k);
                    break;
                }
            }
            let k = slot.unwrap_or(victim);
            if slot.is_none() && fill > 0 && unsafe { *cache.add(k * 2 + 1) > 0 } {
                pic_evict_note(caller_ptr, cp);
            }
            *cache.add(k * 2) = key;
            *cache.add(k * 2 + 1) = fill;
        }
        return true;
    }
    // ONE survey pass, three candidates: this key's own way, the first free way, the first
    // unservable way. `fp == 0` is free (and its key word is 0, which no real key ever is:
    // a class word is a non-null `ArcInner` pointer and a λ identity key is odd).
    let (mut own, mut free, mut dead) = (None, None, None);
    unsafe {
        for k in 0..WAYS {
            let (kk, fp) = (*cache.add(k * 2), *cache.add(k * 2 + 1));
            if fp == 0 {
                if free.is_none() {
                    free = Some(k);
                }
                continue;
            }
            if kk == key {
                own = Some(k);
                break; // rule 1 wins outright; nothing later can beat it
            }
            if fp < 0 && dead.is_none() {
                dead = Some(k); // a `-1` verdict: correct, but machine code can never serve it
            }
        }
    }
    let slot = match own.or(free) {
        Some(k) => k,
        // Rule 3: a servable entry takes an unservable way ahead of a live one.
        None if fill > 0 => {
            let slot = dead.unwrap_or(victim);
            // Count only a real live-for-live replacement: no matching/free/dead way,
            // and both the outgoing and incoming fills are servable.
            if dead.is_none() && unsafe { *cache.add(slot * 2 + 1) > 0 } {
                pic_evict_note(caller_ptr, cp);
            }
            slot
        }
        // Rule 4: an unservable entry never displaces a live one.
        None => return false,
    };
    unsafe {
        *cache.add(slot * 2) = key;
        *cache.add(slot * 2 + 1) = fill;
    }
    true
}

#[cfg(feature = "next-gen-gc")]
struct PicEvictSite {
    evictions: std::sync::atomic::AtomicU64,
    not_yet: std::sync::atomic::AtomicU64,
}

#[cfg(feature = "next-gen-gc")]
fn pic_evict_sites() -> &'static std::sync::Mutex<std::collections::HashMap<(usize, u16), PicEvictSite>> {
    static SITES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<(usize, u16), PicEvictSite>>,
    > = std::sync::OnceLock::new();
    SITES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(feature = "next-gen-gc")]
fn pic_evict_increment(caller_ptr: *const LoadedClass, cp: u16, eviction: bool) {
    let mut sites = pic_evict_sites().lock().unwrap_or_else(|p| p.into_inner());
    let counts = sites.entry((caller_ptr as usize, cp)).or_insert_with(|| PicEvictSite {
        evictions: std::sync::atomic::AtomicU64::new(0),
        not_yet: std::sync::atomic::AtomicU64::new(0),
    });
    let counter = if eviction { &counts.evictions } else { &counts.not_yet };
    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(feature = "next-gen-gc")]
fn pic_evict_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PIC_EVICT").is_some_and(|v| v == "1"))
}

#[cfg(feature = "next-gen-gc")]
fn pic_evict_note(caller_ptr: *const LoadedClass, cp: u16) {
    if !pic_evict_enabled() {
        return;
    }
    pic_evict_increment(caller_ptr, cp, true);
    arm_pic_evict_reporter();
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn pic_not_yet_note(caller_ptr: *const LoadedClass, cp: u16) {
    if !pic_evict_enabled() {
        return;
    }
    pic_evict_increment(caller_ptr, cp, false);
    arm_pic_evict_reporter();
}

#[cfg(feature = "next-gen-gc")]
fn arm_pic_evict_reporter() {
    static ARMED: std::sync::Once = std::sync::Once::new();
    ARMED.call_once(|| {
        std::thread::Builder::new()
            .name("rava-pic-evict-report".into())
            .spawn(|| loop {
                std::thread::sleep(std::time::Duration::from_secs(3));
                let sites = pic_evict_sites().lock().unwrap_or_else(|p| p.into_inner());
                let mut rows: Vec<_> = sites
                    .iter()
                    .map(|(&(caller_ptr, cp), counts)| {
                        (
                            caller_ptr,
                            cp,
                            counts.evictions.load(std::sync::atomic::Ordering::Relaxed),
                            counts.not_yet.load(std::sync::atomic::Ordering::Relaxed),
                        )
                    })
                    .filter(|(_, _, evictions, not_yet)| *evictions != 0 || *not_yet != 0)
                    .collect();
                drop(sites);
                if rows.is_empty() {
                    continue;
                }
                rows.sort_by_key(|row| std::cmp::Reverse(row.2 + row.3));
                for (caller_ptr, cp, evictions, not_yet) in rows.into_iter().take(40) {
                    let caller = unsafe { &*(caller_ptr as *const LoadedClass) };
                    let member = caller.file.pool.member_ref(cp);
                    if let Some(member) = member {
                        eprintln!(
                            "[PIC-EVICT] site={}/{}{} cp={} evictions={} not_yet={}",
                            caller.file.this_class,
                            member.name,
                            member.descriptor,
                            cp,
                            evictions,
                            not_yet
                        );
                    }
                }
            })
            .ok();
    });
}

/// Kill switch for this file's PIC-fill work — `RAVA_NO_FAST_PICFILL2=1`, or the blanket
/// `RAVA_NO_EMIT_MORE`, restores the previous slot policy word for word (`pic_install`'s legacy
/// arm) and the previous emitted probe instruction for instruction (`emit_refpic_probe`'s legacy
/// arm). Read once. The emitter consults it at EMIT time, so with the switch set not one
/// instruction of the new probe shape is generated; the fill consults it per install, which is a
/// `OnceLock` read on a path that has just walked a class hierarchy.
///
/// It does NOT gate the zero-allocation resolve or the single object lookup in `heappic_fill`:
/// those compute the same three values from the same three reads and hand the same `&str`s to
/// the same resolver, so there is no second behaviour to switch back to.
#[inline]
pub(crate) fn fast_picfill2_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_PICFILL2").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// WHY THE λ-PIC MEASURES NULL (`RAVA_JIT_PIC_PROBE`).
///
/// [[ledger row 330]] puts **JIT_CALLBACK at 25.4% of the boot** and 51.3 M PIC misses, and the
/// dispatch table cannot serve any of them that are lambdas because it is keyed by receiver CLASS
/// and every lambda shares `$Lambda`. The λ-PIC is the mechanism that can — it probes by receiver
/// IDENTITY — and a paired A/B of it reads **−0.18%, arms overlapping, 2/3 signs mixed**.
/// A null from a mechanism that never fires is a measurement OF THE GATE, so the gate is counted.
#[cfg(feature = "next-gen-gc")]
pub(crate) static LPIC_REFUSE: [std::sync::atomic::AtomicU64; 8] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

#[cfg(feature = "next-gen-gc")]
#[inline]
fn lpic_refused(which: usize) {
    if crate::picprobe::enabled() {
        LPIC_REFUSE[which].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn lpic_refusal_line() -> String {
    if !crate::picprobe::enabled() {
        return String::new();
    }
    let v: Vec<u64> = LPIC_REFUSE
        .iter()
        .map(|a| a.load(std::sync::atomic::Ordering::Relaxed))
        .collect();
    format!(
        "[LPIC] not_a_heap_handle={} gilfree_worker={} | NOT_A_LAMBDA={} capturing={}          wrong_kind={} | not_an_indy_singleton={} ADMITTED={}",
        v[0], v[1], v[5], v[6], v[7], v[3], v[4]
    )
}

/// Kill switch for the heap-Ref negative-cache probe (`RAVA_NO_PIC_NEG=1` restores the
/// re-resolve-every-call behaviour, for A/B only — it buys nothing).
fn pic_neg_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_NO_PIC_NEG").is_some())
}
