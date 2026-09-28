//! What a dispatch miss resolves to, and which resolutions may be cached.

use crate::*;

/// Kill switch (DEFAULT ON, `RAVA_NO_FAST_DRESOLVE=1` restores the old shape exactly) for the
/// three complexity fixes on the dispatch-resolve path. None of them changes a verdict; each
/// removes work that is proportional to something the verdict does not depend on.
///
///  1. **The negative probe scanned 4 ways of a 16-way slice.** `dispatch_miss_resolve` publishes
///     into `IC_WORKER_WAYS`(=16) ways on a gil-free worker but re-probed only the first 4, so a
///     `-1` landing in ways 4..15 was invisible and its (site, class) pair paid a FULL
///     `dispatch_resolve_cacheable` — `member_ref`, the exclusion chain, `resolve_method` — on
///     every subsequent call, forever. Was O(1) probe over 4/16 entries (12 of 16 permanent
///     verdicts unreachable), is a probe over exactly the ways the fill writes.
///  2. **Three `String`s per resolution.** The resolver copied the method name, the descriptor and
///     the class name onto the heap purely to hand `&str`s to a function that only reads them.
///     Was 3 allocations + O(len(name) + len(desc) + len(class)) bytes memcpy'd per resolution
///     — and resolutions grow with dynamic calls — is zero allocations and zero copies (the
///     constant-pool entries are already `&str`; the class name is an `Arc<str>` clone).
///  3. **The native-served verdict was keyed by the class NAME.** It is a property of the class,
///     it never changes, and the receiver already arrives as a dense arena class id — so hashing
///     a 40-60 byte name for it was work proportional to the name length on every ask. Was
///     O(len(class)) per ask, is one indexed byte load (`jit_native_served_deep_id`).
pub(crate) fn fast_dresolve() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_DRESOLVE").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Kill switch (DEFAULT ON, `RAVA_NO_MONO_FILL=1` restores the old shape exactly) for resolving a
/// COLD direct-mono site from the constant pool instead of only from the interpreter-warmed call
/// cache. Consulted at the fill itself rather than at emit time because it changes no codegen —
/// `emit_direct_mono` bakes the same cache word either way; all this decides is whether that word
/// can ever become non-zero on a site the interpreter never ran. See `Vm::mono_fill_resolve`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn mono_fill() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_MONO_FILL").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// What a cold-site resolve concluded. `Never` is a PERMANENT property of the site's constant-pool
/// entry (`-1` into the baked cache, so the resolve never re-runs); `NotYet` leaves the cache 0 so
/// a later call re-asks — the only two things the fill is allowed to say short of a target.
#[cfg(feature = "next-gen-gc")]
enum MonoFill {
    Decl(std::sync::Arc<LoadedClass>),
    Never,
    NotYet,
}

/// Opt-out for the event-driven pending-compile memo.
#[cfg(feature = "next-gen-gc")]
fn notyet_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_NOTYET_MEMO").is_none())
}

#[cfg(feature = "next-gen-gc")]
thread_local! {
    static NOTYET: std::cell::RefCell<crate::FastMap<(i64, u32), usize>> =
        std::cell::RefCell::new(crate::FastMap::default());
}

/// A pending target can skip resolution only while its structural JIT state is `Compiling`.
#[cfg(feature = "next-gen-gc")]
fn notyet_pending_key(cache_ptr: i64, class_id: u32) -> Option<usize> {
    notyet_enabled()
        .then(|| NOTYET.with(|memo| memo.borrow().get(&(cache_ptr, class_id)).copied()))
        .flatten()
}

#[cfg(feature = "next-gen-gc")]
fn notyet_forget(cache_ptr: i64, class_id: u32) {
    NOTYET.with(|memo| {
        memo.borrow_mut().remove(&(cache_ptr, class_id));
    });
}

#[cfg(feature = "next-gen-gc")]
fn notyet_note(cache_ptr: i64, class_id: u32, code_key: usize) {
    if !notyet_enabled() {
        return;
    }
    NOTYET.with(|memo| {
        let mut memo = memo.borrow_mut();
        // Bounded: every key is re-derivable, so a flush loses no verdict.
        if memo.len() >= 65_536 {
            memo.clear();
        }
        memo.insert((cache_ptr, class_id), code_key);
    });
}

impl Vm {
    /// MACHINE-CODE DISPATCH IC miss handler (next-gen-gc). The compiled 4-way PIC
    /// at an `invokevirtual`/`invokeinterface` site missed (or the receiver isn't an
    /// arena object). Resolve + run the call the general way, and — when the receiver
    /// IS an arena object and the target is a compiled GC-leaf method whose locals
    /// fit the call buffer — write `(class_id, fptr)` into a free/evicted cache slot
    /// so the next call with that class hits in machine code (no callback). `buf` is
    /// the slot-laid-out cells (== raw operands, the IC only fires on no-wide-arg
    /// sites), `cache_ptr` the site's 4-entry `[(class_id i64, fptr i64); 4]` buffer.
    #[cfg(feature = "next-gen-gc")]
    /// The RESOLVE + PUBLISH half of a dispatch miss, split from the EXECUTE half
    /// (the tail `jit_call` its sole caller `rava_dispatch_miss` runs next) so a
    /// gil-free worker can scope the global resolve lock to the part that needs it.
    ///
    /// Everything here mutates state shared with other workers — it loads the target
    /// class and runs its `<clinit>`, compiles, writes the site's N-way IC cache and
    /// mirrors the verdict into the append-only freeze-resolve table, plus the
    /// λ-PIC / heap-Ref PIC fills. The EXECUTE half needs none of that: it re-derives
    /// everything from `(caller_ptr, cp, kind, buf)` and takes its own envelope
    /// (`jit_call`'s `_worker_exec_guard`) if and only if it reaches the general path.
    ///
    /// Serial behaviour is unchanged — `dispatch_miss` still calls the two halves
    /// back to back in the original order.
    pub(crate) fn dispatch_miss_resolve(
        &mut self,
        cache_ptr: i64,
        recv: i64,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        buf: *const i64,
        n: i64,
    ) {
        let _ = kind;
        let icm = crate::icmiss::on();
        if icm {
            crate::icmiss::ENTRY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // Populate the IC if the receiver is an arena object with a cacheable target.
        let raw_u = recv as u64;
        if recv != 0 && raw_u >> 40 == 0 {
            let class_id = unsafe { *(recv as *const u32) };
            // WAYS AT THIS SITE — worker per-thread slices are 16-way (IC_WORKER_WAYS), the
            // global box stays 4. ONE definition, used by both the negative probe below and the
            // fill further down; that they must agree is the point, and they did not. Kept as a
            // closure so the common case (main thread, negative entry inside the first four ways)
            // still answers without touching `is_worker`.
            let site_ways = || {
                if crate::gil::is_worker() && crate::jit::ic_parallel_enabled() {
                    crate::jit::IC_WORKER_WAYS
                } else {
                    4
                }
            };
            // NEGATIVE-CACHE probe: a prior miss proved this (site, class) can
            // NEVER be IC-served (fptr = -1) — skip the per-call resolve (it
            // allocates two Strings + walks resolve_method; on an uncacheable
            // hot site that was the dominant dispatch cost).
            //
            // ⭐**AND IT ONLY LOOKED AT A QUARTER OF THE SLICE IT WRITES.** The fill below has
            // published into 16 ways on a gil-free worker since IC_PARALLEL landed; this probe
            // stayed at the 4 it was written against. A `-1` evicted into way 4..15 was therefore
            // unreachable, and its (site, class) pair re-ran the whole resolve on EVERY call for
            // the life of the process — the exact re-resolve loop the negative cache exists to
            // end, silently reinstated for 12 of every 16 permanent verdicts. Reading exactly
            // `ways` is safe by construction: it is the same expression the fill writes with, so
            // the probe can never run past the buffer the fill was allowed to touch, and the
            // widened half is an EMPTY RANGE wherever the slice really is 4-way, so the global
            // box reads exactly the four words it always did.
            let known_negative = unsafe {
                let cache = cache_ptr as *const i64;
                let neg = |k: usize| {
                    *cache.add(k * 2) == class_id as i64 && *cache.add(k * 2 + 1) == -1
                };
                (0..4usize).any(&neg) || (fast_dresolve() && (4..site_ways()).any(&neg))
            };
            if known_negative {
                if icm {
                    crate::icmiss::NEG_HIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                return; // nothing to publish; the EXECUTE half runs the call
            }
            // ROOT the operand `buf` (receiver + args) across both completion publication and
            // resolution: draining a background result or loading the target class can allocate
            // and MOVE + reset the nursery. `buf` is the compiled frame's operand cells (NOT in
            // cells_all), and is re-read into `raw` below for jit_call. Without rooting it, a
            // collection during either operation can leave a stale receiver pointer. This is the
            // worldgen minValue strand: HolderHolder.minValue dispatches DensityFunction.minValue
            // on a Holder.value() result that was fresh at the getfield but went stale before the
            // eventual call. The interface-IC twin of the jit_direct_call resolve fix.
            frames::cells_push((buf, n.max(0) as usize));
            // While an eligible callee is queued, keep the site on its safe fallback without
            // re-walking the constant pool. Re-resolve when the owning VM publishes a completion
            // or the queue declines the job. This keys the wait to `JitStatus::Compiling`, not a
            // periodic miss count.
            if let Some(key) = notyet_pending_key(cache_ptr, class_id) {
                if !crate::gil::is_worker()
                    && !crate::parallel::gilfree()
                    && self.background_jit.as_ref().is_some_and(|bg| bg.results_ready())
                {
                    self.drain_background_jit();
                }
                if matches!(self.jit_cache.get(&key), Some(jit::JitStatus::Compiling)) {
                    frames::cells_pop();
                    return;
                }
                notyet_forget(cache_ptr, class_id);
            }

            let resolved = self.dispatch_resolve_cacheable(class_id, caller_ptr, cp, n as usize);
            frames::cells_pop();
            if let DispatchResolve::NotYet(Some(key)) = &resolved {
                if matches!(self.jit_cache.get(key), Some(jit::JitStatus::Compiling)) {
                    notyet_note(cache_ptr, class_id, *key);
                }
            }
            if icm {
                use std::sync::atomic::Ordering::Relaxed;
                crate::icmiss::seen_pair(cache_ptr, class_id);
                match resolved {
                    DispatchResolve::Compiled(_) => crate::icmiss::R_COMPILED.fetch_add(1, Relaxed),
                    DispatchResolve::NeverCacheable => crate::icmiss::R_NEVER.fetch_add(1, Relaxed),
                    DispatchResolve::NotYet(_) => crate::icmiss::R_NOTYET.fetch_add(1, Relaxed),
                };
            }
            // fill: >0 = compiled entry, -1 = proven never-cacheable (negative
            // entry, machine code skips it via fp > 0), None = not compiled YET
            // (leave the slot alone so a later call can cache the real pointer).
            let fill = match resolved {
                DispatchResolve::Compiled(fptr) => Some(fptr as i64),
                DispatchResolve::NeverCacheable => Some(-1),
                DispatchResolve::NotYet(_) => None,
            };
            if let Some(fill) = fill {
                let ways = site_ways();
                unsafe {
                    let cache = cache_ptr as *mut i64;
                    let mut slot = None;
                    for k in 0..ways {
                        if *cache.add(k * 2 + 1) == 0 {
                            slot = Some(k);
                            break;
                        }
                        if *cache.add(k * 2) == class_id as i64 {
                            slot = Some(k); // refresh an existing entry
                            break;
                        }
                    }
                    let k = slot.unwrap_or((class_id as usize) & (ways - 1)); // evict by class
                    *cache.add(k * 2) = class_id as i64;
                    *cache.add(k * 2 + 1) = fill;
                }
                // The resolved positive fill is sufficient to populate the table; its fixed
                // byte budget bounds one-off and polymorphic sites alike.
                #[cfg(feature = "next-gen-gc")]
                crate::jit::dispatch_table::note(cache_ptr, class_id, fill);
                // P3-S1: mirror every verdict into the append-only freeze-resolve table —
                // no way limit, so a worker's megamorphic steady-state stays lock-free even
                // after this 4-way cache evicts the entry. STABLE key (caller|cp<<48):
                // cache_ptr is per-thread under IC_PARALLEL and never matches across threads.
                crate::jit::freeze_resolve::put(
                    (caller_ptr as i64) | ((cp as i64) << 48),
                    class_id as i64,
                    fill,
                );
            }
        } else {
            if icm {
                crate::icmiss::NON_ARENA.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // λ-PIC sizing (RAVA_JIT_PIC_PROBE, inert otherwise): how many host falls carry a
            // LAMBDA receiver — the population a lambda-identity PIC arm could serve.
            if crate::picprobe::enabled() {
                if let Value::Ref(h) = jit::unbox_ref(self, recv, "") {
                    if self.heap.is_lambda(h) {
                        crate::picprobe::LAMBDA_MISS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if self.heap.lambda_directable(h) {
                            crate::picprobe::LAMBDA_DIRECTABLE_MISS
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            }
            // λ-PIC (RAVA_LAMBDA_PIC, default OFF): a directable lambda SINGLETON receiver
            // fills an IDENTITY-keyed entry the gated machine-code select can serve (all
            // lambdas share the "$Lambda" class word, so heappic_fill below can never help
            // them). Fill-only; this call still runs via jit_call below (unchanged).
            if crate::jit::lambda_pic_enabled() {
                self.lambda_pic_fill(cache_ptr, recv, caller_ptr, cp, kind, buf, n);
            }
            // 23.4b: on a heap-Ref (non-arena) miss at a DiscreteVoxelShape site, cache (arc_key,
            // leaf-fptr) so the machine-code heap-Ref PIC serves the NEXT call with this class directly
            // (no rava_dispatch_miss). Fill-only; this call still runs via jit_call below (unchanged).
            if crate::heappic::live_enabled() {
                self.heappic_fill(cache_ptr, recv, caller_ptr, cp);
            }
            // 23.7 self_reg coverage of the heap-Ref virtual/interface pool (Option A sizing). Run with
            // RAVA_NO_JIT_HEAPPIC=1 so every heap-Ref call reaches here (uncached) and is classified.
            if crate::selfreg::enabled() {
                self.selfreg_probe_hr(recv, caller_ptr, cp);
            }
        }
    }

    /// 23.9 monomorphic static/special direct-call resolve. Returns the value to write into the site's
    /// single-fptr cache: fptr|0 (GC-leaf), fptr|1 (self_reg non-leaf, ic_deep only), -1 (never), or
    /// None (NotYet — leave the cache 0 to retry). The target is monomorphic (from the interpreter-
    /// warmed call_cache), so no class guard — the cached fptr is always the right callee.
    // (worker_locked_exec_enabled lives at module scope below jit_call's use site.)
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn direct_mono_resolve(&mut self, caller_ptr: *const LoadedClass, cp: u16) -> Option<i64> {
        let site = (caller_ptr as usize, cp);
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let mref = caller.file.pool.member_ref(cp)?;
        // NO COPIES. `MemberRef` is already `(&str, &str)` into the caller's constant pool and
        // `decl` owns the class name — the callee only READS all three. The three `to_string()`s
        // that used to stand here were 3 allocations + O(len(name)+len(desc)+len(class)) bytes
        // memcpy'd per site fill, for values nobody mutates. `caller` is derived from a raw
        // pointer, so its borrow is independent of `&mut self` and the refs stay live.
        let (name, desc) = (mref.name, mref.descriptor);
        // Monomorphic target class. The interpreter-warmed call_cache is the FAST source (same one
        // jit_direct_resolve uses for static/special) — never resolved by dynamic dispatch. When it
        // is empty the site has simply never been interpreted, which under `RAVA_JIT_TIER0=1` is
        // EVERY site in the program; `mono_fill_resolve` then computes the same answer the
        // interpreter would have computed, from the constant pool.
        let decl = match crate::tlcache::call_get(site) {
            // WARM — unchanged, byte for byte.
            Some(Some(decl)) => decl,
            // The interpreter RAN this site and resolved it to NOTHING (a force-native owner, or a
            // class/method the loader cannot serve with bytecode). Left exactly as it was: `None`,
            // i.e. leave the cache 0 and re-ask. Not widened to `-1` here because this arm is
            // shipped behaviour and the cold-resolve arm below is where the population is.
            Some(None) => return None,
            // COLD.
            None => match self.mono_fill_resolve(mref.owner, name, desc) {
                MonoFill::Decl(d) => d,
                MonoFill::Never => return Some(-1),
                MonoFill::NotYet => return None,
            },
        };
        let cls: &str = &decl.file.this_class;
        match self.dispatch_resolve_cacheable_cls(cls, name, desc) {
            DispatchResolve::Compiled(f) => Some(f as i64), // |0 leaf or |1 self_reg deep
            DispatchResolve::NeverCacheable => Some(-1),
            DispatchResolve::NotYet(_) => None,
        }
    }

    /// ⭐**THE BOOTSTRAP TRAP, THIRD OF ITS KIND IN THIS TREE.** `direct_mono_resolve` used to
    /// answer `None` the instant `tlcache::call_get(site)` was empty — and the product flag set
    /// runs `RAVA_JIT_TIER0=1`, where a method compiles on its FIRST ENTRY. Its call sites are
    /// therefore never interpreted, so nothing ever calls `call_put` for them, so `call_get` stays
    /// empty, so the site's baked single-fptr cache stays 0 **for the life of the process** and
    /// every invokestatic/invokespecial at it re-enters `rava_direct_miss` and takes the
    /// synthetic-frame host path. The waited-for value was published by exactly the calls the arm
    /// was meant to serve; "not ready ⇒ do not emit" cannot converge against that.
    ///
    /// The cure is the one the other two took: the interpreter cache is a SHORTCUT, not the source
    /// of truth. A static or special target is fixed by the caller's constant pool and JVMS
    /// resolution — receiver-independent, no dynamic dispatch — so it can be computed without ever
    /// having executed the site. These are the interpreter's own two lines (`opcodes/invoke.rs`,
    /// both `invokestatic` and `invokespecial`), in order and with the same inputs:
    ///
    ///   * a `force_native_collection(owner, name, descriptor)` owner resolves to NOTHING — the
    /// interpreter caches `None` for it and falls through to the native layer. A pure function of
    /// `(owner, name, descriptor)`, all constants of the site.
    ///   * otherwise `resolve_method(owner, name, desc)`, which is what the interpreter calls and
    ///     what `call_put` stores. `invokespecial` uses the symbolic owner unadjusted here too, so
    ///     the two agree on super-calls as well.
    ///
    /// ⚠**CLASS INITIALISATION IS THE ONE THING RESOLUTION CANNOT ANSWER, AND IT IS WHY THIS IS A
    /// GATE AND NOT JUST A RESOLVE.** Baking an fptr takes the callee's `<clinit>` check out of the
    /// call forever (machine code `call_indirect`, no host crossing), and JVMS §5.5 says the first
    /// execution of a static method initialises its declaring class. Running `<clinit>` HERE, at
    /// fill time, would run it at a point of our choosing rather than the program's — a different
    /// initialisation ORDER, and on a class whose `<clinit>` throws, an initialisation that should
    /// have raised `NoClassDefFoundError` at a specific call.
    ///
    /// So this fills only a target whose class is ALREADY INITIALISED (`clinit_done`, the latch
    /// `ensure_init` sets on completion — after the super chain, and also for a class with no
    /// `<clinit>` body at all). Nothing here can trigger an initialisation: `resolve_method` walks
    /// the loader, which LOADS, and loading is not initialising. The fill therefore lands on the
    /// site's SECOND call at the earliest: call 1 resolves, sees the latch clear, returns `NotYet`,
    /// and the callback's `jit_call` below it runs the real call — which is the one that runs
    /// `ensure_init` in program order; call 2 finds the latch set and bakes. That is both cures the
    /// spec allows at once ("after the first real call has run" ∧ "only an initialised target"),
    /// and it costs exactly one extra callback per site. A class whose `<clinit>` threw is
    /// ERRONEOUS: `clinit_done` never latches, so this never fills it and every call keeps going
    /// through the host path that throws — the correct behaviour, preserved by construction.
    /// (`invokespecial`'s own resolved-site replay in `opcodes/invoke.rs` gates on this same latch
    /// for this same reason.)
    ///
    /// ⚠**AND A GIL-FREE WORKER MUST NOT COME THROUGH HERE.** `resolve_method` loads classes and
    /// publishes into the shared resolution cache, and `ensure_init` on a worker deliberately
    /// leaves a class UNINITIALISED (the clinit barrier) — so a worker reading `clinit_done` could
    /// see it clear for a class main has finished, and worse, could publish. The caller already
    /// refuses (`jit/callbacks/dispatch.rs` freezes the site cache off-GIL); this is the second
    /// lock on the same door, because the refusal is load-bearing and lives in another file.
    ///
    /// Kill switch `RAVA_NO_MONO_FILL=1` (default ON) restores the old shape exactly: cold ⇒
    /// `NotYet` ⇒ the site never fills.
    #[cfg(feature = "next-gen-gc")]
    fn mono_fill_resolve(&self, owner: &str, name: &str, desc: &str) -> MonoFill {
        if !mono_fill() || crate::parallel::gilfree() {
            return MonoFill::NotYet;
        }
        // Permanent, and a pure function of two site constants — so `-1` rather than a re-ask on
        // every call. The whole `java/lang/*` static population (`Math`, `System`, `Objects`,
        // `StringBuilder`) lands here and in the `Ok(None)` arm below, and the difference between
        // the two verdicts is whether those sites pay a resolve once or once per call.
        if crate::opcodes::force_native_collection(self, owner, name, desc) {
            return MonoFill::Never;
        }
        match self.resolve_method(owner, name, desc) {
            // No bytecode body anywhere in the hierarchy: a native, an abstract, or a class the
            // loader cannot serve. Permanent — `resolve_method` memoises the same `None` — and the
            // host path handles it correctly, so mark the site `-1` and stop asking.
            Ok(None) => MonoFill::Never,
            // NOT initialised yet ⇒ NotYet, never a fill. See the §5.5 note above.
            Ok(Some(d)) if !d.clinit_done.load(std::sync::atomic::Ordering::Acquire) => {
                MonoFill::NotYet
            }
            Ok(Some(d)) => MonoFill::Decl(d),
            // A region abort or a load error is NOT a verdict about the site — re-ask.
            Err(_) => MonoFill::NotYet,
        }
    }

    /// Resolve a virtual/interface call on an ARENA receiver class to a compiled,
    /// IC-cacheable native entry. Cacheable ⇔ the resolved target is a
    /// pure user bytecode method (not a `java/`/`$`/native-collection special case),
    /// is GC-LEAF (never spills → safe on a non-zeroed buffer), has `max_locals` ≤
    /// the call buffer, and is already compiled. `NeverCacheable` verdicts are
    /// PERMANENT for the (site, class) pair; `NotYet(code_key)` leaves the slot open.
    /// A known code key is skipped only while its JIT cache entry remains `Compiling`.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn dispatch_resolve_cacheable(
        &mut self,
        class_id: u32,
        caller_ptr: *const LoadedClass,
        cp: u16,
        n: usize,
    ) -> DispatchResolve {
        use DispatchResolve::NotYet;
        let _ = n;
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let Some(mref) = caller.file.pool.member_ref(cp) else { return DispatchResolve::NeverCacheable };
        // ⭐**THREE ALLOCATIONS PER RESOLUTION FOR THREE VALUES THAT ARE ONLY READ.** `MemberRef`
        // hands back `&str`s straight into the caller's constant pool and `class_name` an
        // `Arc<str>` out of the append-only arena registry; `dispatch_resolve_cacheable_cls` takes
        // `&str` and mutates none of them. The `to_string()`s that stood here therefore did a
        // malloc + memcpy of the method name, the descriptor and the class name — work
        // proportional to their LENGTH — on a path this tree measured at **4,349,186 resolutions
        // over 121 distinct (site, class) pairs** on one bench. Was 3 allocations and
        // O(len(name)+len(desc)+len(class)) bytes copied per resolution, is one `Arc` refcount
        // bump and no copy at all. (`caller` comes from a raw pointer, so its borrow is
        // independent of `&mut self`; `cls` is an owned `Arc<str>` local, likewise.)
        let Some(cls) = crate::nextgen::class_name(class_id) else { return NotYet(None) };
        self.dispatch_resolve_cacheable_cls_id(Some(class_id), &cls, mref.name, mref.descriptor)
    }

    /// The class-name body of `dispatch_resolve_cacheable`, shared by the arena path (class_id →
    /// class_name) and the 23.4b heap-Ref path (handle → `Heap::class_of`). Same exclusions + GC-leaf/
    /// deep + compiled-fptr determination. A leaf fptr is returned bare; a deep one carries `|1`.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn dispatch_resolve_cacheable_cls(&mut self, cls: &str, name: &str, desc: &str) -> DispatchResolve {
        // Callers that reach here from a HANDLE (the 23.4b heap-Ref fill, `selfreg`, the
        // direct-mono site fill, `directprobe`) have a name and no arena class id — they get the
        // name-keyed path, unchanged. The arena caller below carries the id.
        self.dispatch_resolve_cacheable_cls_id(None, cls, name, desc)
    }

    /// `dispatch_resolve_cacheable_cls` with the receiver's DENSE ARENA CLASS ID, when the caller
    /// happens to have one. The id changes exactly one thing: the native-served verdict — a
    /// property of the CLASS that never changes for the life of the process — is answered by an
    /// indexed byte load instead of by hashing the 40-60 byte class NAME. Every other clause,
    /// and every verdict, is identical; `None` reproduces the old behaviour byte for byte.
    /// `jit_native_served_deep`, answered by the dense class id when the caller has one and by
    /// the class name when it does not. Its own method purely so the `else if` chain below stays
    /// a chain of calls — the arm must remain LAZY (the name path can load classes), so it
    /// cannot be hoisted into a `let`.
    #[cfg(feature = "next-gen-gc")]
    fn native_served_deep_keyed(&self, class_id: Option<u32>, cls: &str) -> bool {
        match class_id {
            Some(id) => self.jit_native_served_deep_id(id, cls),
            None => self.jit_native_served_deep(cls),
        }
    }

    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn dispatch_resolve_cacheable_cls_id(
        &mut self,
        class_id: Option<u32>,
        cls: &str,
        name: &str,
        desc: &str,
    ) -> DispatchResolve {
        use DispatchResolve::{Compiled, NeverCacheable, NotYet};
        // Mirror jit_direct_resolve's exclusions: anything the interpreter could
        // special-case by class must stay on the general path.
        // WHICH clause refuses. Split because the verdict below is PERMANENT: `dispatch_miss`
        // writes a -1 negative entry and this resolve never re-runs for the pair, so an
        // over-approximating clause is paid by every later call of every site it touches.
        // ⚠**THIS CLAUSE IS A SHORTCUT FOR A VERDICT THE CODE BELOW ALREADY REACHES.** Its
        // premise is that a `java/*` class has no bytecode in this tree — true today, because
        // java.util is served by Rust natives — and the checks further down (`resolve_method`,
        // then `method(..).code`) return `NeverCacheable` for exactly that reason. So with the
        // premise HOLDING, the clause only saves work; with the premise BROKEN — real JDK
        // bytecode on the classpath — it silently makes every such call permanently
        // uncacheable, which is the difference between a PIC hit and a full host resolve.
        //
        // `RAVA_JAVA_PIC=1` drops the shortcut and lets the real checks decide. It exists
        // because the experiment that put real `java.util` bytecode on the classpath measured
        // `List.size` at 369 ns against the native's 65 and was read as "JDK bytecode is slower
        // here" — a reading taken with this clause forcing the slow path on every one of those
        // calls.
        let java_pic = {
            static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            *ON.get_or_init(|| std::env::var("RAVA_JAVA_PIC").ok().as_deref() == Some("1"))
        };
        // ⭐And the premise is no longer "true today": a class with real bytecode loaded for it
        // is served by that bytecode (`native_served_namespace` declines it), so refusing to
        // cache its call sites is exactly the silent slow path this comment warns about.
        // MEASURED: with a guest `java/util/ArrayList` on the classpath and this clause still
        // firing, the list benchmark read **3,934 ns/element**; the same build with the clause
        // aware of the override reads what the rest of this file would have given it anyway.
        let never = if cls.starts_with("java/")
            && !java_pic
            && !crate::class_loader::has_bytecode(&cls)
        {
            Some(0)
        } else if cls.starts_with('$') {
            Some(1)
        } else if crate::cache::is_cache_class(cls) {
            Some(2)
        } else if crate::fastutil::is_int_set(cls) {
            Some(3)
        } else if opcodes::force_native_collection(self, cls, name, desc) {
            Some(4)
        } else if (crate::opcodes::native_served_prefix(cls)
            && !crate::class_loader::has_bytecode(cls))
            // ⭐**KEYED BY THE CLASS ID, NOT BY ITS NAME.** `jit_native_served_deep`'s memo is a
            // `FastMap<Box<str>, bool>`, so every ask hashes the whole class name for an answer
            // that is a constant of the class. The arena receiver already arrived as a dense
            // integer id; `_id` fronts the same memo with a byte indexed by it. Same answer, same
            // staleness (the registry is append-only, so an id names one name forever) — the only
            // thing removed is the hash. Six `starts_with` that fail on the first byte are
            // cheaper than one hash of the name, which is why the PREFIX test above keeps its
            // string form and only this arm moves.
            || self.native_served_deep_keyed(class_id, cls)
        {
            // ⭐**STRICT HERE — this is the site the fastutil crash actually came through.**
            // Caching a deep/direct entry binds a compiled caller to a compiled callee and takes
            // the dispatch out of the call. That is a different question from "may this body be
            // compiled", and relaxing the shared predicate answered both: with
            // `RAVA_JIT_NS_BYTECODE=1` the repro dies in compiled
            // `StateDefinition$Builder.add`, and `RAVA_NO_IC_DEEP=1` makes it run clean — which
            // is what named the deep IC as the channel.
            //
            // Fifth caller of one predicate answering a different question (compile / inline /
            // AOT link / PIC-cacheability), and the fix is the same each time: split the
            // question, do not change the answer.
            Some(5)
        } else {
            None
        };
        if let Some(c) = never {
            if crate::jit::freeze_resolve::stats_on() || crate::dresolve::enabled() {
                crate::icmiss::NEVER_WHY[c].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                crate::icmiss::note_never_class(c, cls, name);
            }
            crate::jit::dispatch_diag::resolution_for_current(
                cls, cls, name, desc, None, "never-cacheable", "class-exclusion",
            );
            return NeverCacheable;
        }
        // PURE-CALL MEMO: a site the memo layer must SEE cannot be served by a
        // machine-code PIC hit (that path never re-enters the host). Refusing to cache
        // it routes every call through `jit_call`, where the window/lookup lives. The
        // verdict is computed once per (site, class) — the negative entry this returns
        // is permanent, so the proof never re-runs on the hot path.
        if crate::pure_memo::armed() && self.pure_memo_admit(&cls, &name, &desc).verdict.intercepts() {
            crate::jit::dispatch_diag::resolution_for_current(
                cls, cls, name, desc, None, "never-cacheable", "pure-memo",
            );
            return NeverCacheable;
        }
        let decl = match self.resolve_method(&cls, &name, &desc) {
            Ok(Some(decl)) => decl,
            _ => {
                crate::jit::dispatch_diag::resolution_for_current(
                    cls, cls, name, desc, None, "never-cacheable", "method-resolution",
                );
                return NeverCacheable;
            }
        };
        if opcodes::route_native_map(&decl) {
            crate::jit::dispatch_diag::resolution_for_current(
                cls, &decl.file.this_class, name, desc, None, "never-cacheable", "native-route",
            );
            return NeverCacheable;
        }
        let Some(code) = decl.file.method(&name, &desc).and_then(|m| m.code.as_ref()) else {
            crate::jit::dispatch_diag::resolution_for_current(
                cls, &decl.file.this_class, name, desc, None, "never-cacheable", "no-bytecode",
            );
            return NeverCacheable;
        };
        // GC-leaf + locals fit the call buffer: a leaf callee never spills, so the
        // buffer is never scanned and only its local loads [0..max_locals] must stay
        // in bounds (the caller checks the arg count fits before caching).
        let gc_leaf = jit::is_gc_leaf(&code.bytecode);
        let leaf = gc_leaf && code.max_locals as usize <= jit::dispatch_callbuf_slots();
        // DEEP entry (opt-in RAVA_IC_DEEP=1): a NON-leaf compiled callee whose
        // frame (max_locals + max_stack spill area) fits the call buffer is
        // served by the machine-code IC's DEEP hit arm. V3: the callee body
        // carries the self-registering prologue (fault check, non-arg zeroing,
        // inline cell-root push, box discipline), so the caller only spills its
        // own frame and calls directly. Encoded as fptr|1 (fptrs are
        // 8-aligned); the whole worldgen density graph (Ap2/PureTransformer/
        // ShiftedNoise compute chains call their children → never gc-leaf)
        // previously paid the full dispatch_miss → jit_call gauntlet per call.
        let deep = !leaf
            && jit::ic_deep_enabled()
            && (code.max_locals as usize).saturating_add(code.max_stack as usize)
                <= jit::dispatch_callbuf_slots();
        if !leaf && !deep {
            crate::jit::dispatch_diag::resolution_for_current(
                cls, &decl.file.this_class, name, desc, Some(gc_leaf),
                "never-cacheable", "non-leaf-or-frame-too-large",
            );
            return NeverCacheable;
        }
        let key = code as *const Code as usize;
        match self.jit_cache.get(&key) {
            Some(jit::JitStatus::Compiled(f)) => {
                // A deep entry is only safe when the body carries the v3
                // self-registering prologue (a folded/parallel body compiled
                // without it must never be entered registration-free).
                if deep && !f.self_reg {
                    crate::jit::dispatch_diag::resolution_for_current(
                        cls, &decl.file.this_class, name, desc, Some(gc_leaf),
                        "never-cacheable", "deep-entry-not-self-registering",
                    );
                    return NeverCacheable;
                }
                let result = Compiled(f.raw() as usize | if deep { 1 } else { 0 });
                crate::jit::dispatch_diag::resolution_for_current(
                    cls, &decl.file.this_class, name, desc, Some(gc_leaf),
                    if deep { "compiled-deep" } else { "compiled-leaf" }, "compiled",
                );
                result
            }
            Some(jit::JitStatus::Failed) => {
                crate::jit::dispatch_diag::resolution_for_current(
                    cls, &decl.file.this_class, name, desc, Some(gc_leaf),
                    "never-cacheable", "compile-failed",
                );
                NeverCacheable
            }
            _ => {
                // Previously, compiled callers could leave an eligible callee permanently `NotYet`
                // because it did not accumulate interpreted method entries. A per-callee counter
                // eventually closed that loop, but its 64-miss threshold was history-based and
                // measured load-bearing; compiling synchronously at miss 1 regressed boot wall
                // 7.1% (ledger rows 638–639). This path now requests the same structurally admitted
                // body through the bounded background queue on its first resolved dispatch.
                // A resolved dispatch is already the structural trigger: the receiver class,
                // callee bytecode and leaf/deep call-buffer proof are known here. Do not gather
                // per-callee miss history. Queue the first eligible target through the existing
                // bounded compiler pool; while queued, `NotYet` is the call site's safe fallback.
                // A full queue is only backpressure, not a permanent admission decision, so a
                // later miss may retry. The JIT cache's Compiling state prevents duplicate jobs.
                static TIER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                let _hr_wait = crate::background_jit::hr_wait::DeferScope::enter(
                    crate::background_jit::hr_wait::WaitCaller::DispatchTier,
                );
                if *TIER.get_or_init(|| std::env::var_os("RAVA_NO_DISPATCH_TIER").is_none())
                    && self.request_dispatch_compile(&decl, &name, &desc)
                {
                    if let Some(jit::JitStatus::Compiled(f)) = self.jit_cache.get(&key) {
                        if !(deep && !f.self_reg) {
                            crate::jit::dispatch_diag::resolution_for_current(
                                cls,
                                &decl.file.this_class,
                                name,
                                desc,
                                Some(gc_leaf),
                                if deep { "compiled-deep" } else { "compiled-leaf" },
                                "dispatch-first-use-compile",
                            );
                            return Compiled(f.raw() as usize | if deep { 1 } else { 0 });
                        }
                    }
                }
                crate::jit::dispatch_diag::resolution_for_current(
                    cls, &decl.file.this_class, name, desc, Some(gc_leaf),
                    "not-yet", "not-compiled",
                );
                NotYet(Some(key)) // not compiled yet — recheck when the queued target publishes
            }
        }
    }
}

#[cfg(all(test, feature = "next-gen-gc"))]
mod notyet_memo_tests {
    use super::{notyet_forget, notyet_note, notyet_pending_key};

    #[test]
    fn pending_compile_memo_keeps_code_key_until_forgotten() {
        let site = i64::MIN + 17;
        let class_id = u32::MAX - 17;
        notyet_forget(site, class_id);

        notyet_note(site, class_id, 0xfeed);
        assert_eq!(notyet_pending_key(site, class_id), Some(0xfeed));

        notyet_forget(site, class_id);
        assert_eq!(notyet_pending_key(site, class_id), None);
    }
}
