//! Taking a resolved direct call, from a mutator or a worker.

use crate::*;

impl Vm {
    /// Direct JIT-to-JIT call: when the call site resolves to a bytecode method
    /// that is itself compiled, call its native entry directly on the packed
    /// cells — skipping the synthetic frame, Value marshaling, and interpreter
    /// dispatch that dominate compiled-code callback cost (~56M invoke callbacks
    /// in a 240s boot window, 94% of all callback hits).
    ///
    /// invokestatic (kind 0): no receiver special cases; a `call_cache` hit
    /// implies the callee's class is initialized (the first call went through
    /// the full interpreter path).
    ///
    /// invokevirtual/interface (kind 1/3): guarded by the receiver's EXACT class
    /// (interned-name pointer compare — one Arc per class name). Safe because
    /// every interpreter receiver special case is class-determined under that
    /// guard: lambdas are class "$Lambda", synthetic functionals are
    /// "java/util/function/*", and population excludes "java/"-prefixed, "$"-
    /// prefixed, guava-cache, fastutil-int-set, slf4j/CacheBuilder owners and
    /// force_native_collection targets, so a cached entry can only ever be a
    /// plain resolve — exactly what the interpreter would do for that class.
    /// A guard miss (different subclass, null, non-handle cell) falls back to
    /// the general path, which is always correct.
    ///
    /// GAP-S09 stage 4 BORROW SPLIT: the body is factored into two READ-ONLY
    /// (`&self`) moved blocks — `jit_direct_probe` (receiver-guard decode + the
    /// per-site cache scan) and `jit_direct_invoke` (argument packing + the raw
    /// native call) — plus the ONE mutating arm (negative cache + resolve +
    /// insert), which stays here under `&mut self` exactly as before. The
    /// serial instruction path is unchanged: probe → (miss: resolve/insert) →
    /// invoke, in the same order with the same counters; the split is purely
    /// which receiver type each block borrows. Workers never call this — their
    /// call sites select `jit_direct_call_worker` (`&self`) instead, so the
    /// worker path needs no `&mut Vm` at all; the in-body `gilfree()` miss
    /// short-circuit is kept as defense-in-depth for any remaining `&mut`
    /// caller (rava_dispatch_miss's sanctioned envelope).
    /// Install a capture-unpacking thunk into the missed site's 4-way PIC, if this entry is one
    /// the emitter can serve. See `jit::emit::cap_thunk` for the design and for the two wirings
    /// that were measured and removed before this one.
    ///
    /// The four refusals are not caution: copying `raw[1..]` 1:1 killed ChunkFill with
    /// `IllegalArgumentException: The min values need to be smaller or equals to the max values`,
    /// because `wide_mask` gives a long/double a pad slot, `arg0_guard` re-checks the class of
    /// `raw[1]` on every call, and `ret_ref` re-homes a BOX-tagged return. Each is emittable and
    /// none is emitted yet.
    #[cfg(feature = "next-gen-gc")]
    fn install_cap_thunk(
        &self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        entry: &jit::DirectEntry,
        recv_cell: Option<i64>,
        raw: &[i64],
    ) {
        use crate::jit::emit::cap_thunk;
        // ⭐**THE FREE DISCRIMINATOR FIRST.** This runs on EVERY direct HIT, and the overwhelming
        // majority of entries carry no captures at all: `cap_n` is a field of an entry already in
        // registers, while `cap_thunk::enabled()` is a `OnceLock` — an ACQUIRE load and a branch.
        // Reading the flag first bought that load on every non-capturing hit to answer a question
        // a `u16` compare had already settled. Same verdict, same short-circuit, and no counter
        // is touched on either side of the swap (both clauses return before `SEEN`/`REFUSE`).
        if entry.cap_n == 0 || !cap_thunk::enabled() {
            return;
        }
        {
            use std::sync::atomic::Ordering::Relaxed;
            // ⭐**WIDE CAPTURES ARE SERVED; WIDE ARGUMENTS ARE NOT — AND THE GATES DREW THE LINE.**
            // Admitting the whole wide population at once killed ChunkFill with
            // `IllegalArgumentException: The min values need to be smaller or equals to the max
            // values`, so it was bisected: with wides allowed only in CAPTURES all four gates are
            // bit-exact, and with them allowed only in ARGUMENTS every gate dies. The destination
            // arithmetic (`wide_mask` indexes the write order, a wide takes a cell and a pad) is
            // therefore right for captures and wrong for arguments — something about how the
            // caller's operand buffer lays a wide argument out differs from what this reads at
            // `buf[(1+j)*8]`, and that is the next thing to find.
            //
            // `RAVA_CAP_WIDE=args|caps|all` keeps the bisect available; the default serves the
            // half the gates proved.
            let caps_mask = if entry.cap_n >= 64 { u64::MAX } else { (1u64 << entry.cap_n) - 1 };
            let wide_args = entry.wide_mask & !caps_mask;
            // ⭐**IT WAS NEVER THE WIDE PARAMETERS — IT WAS THE VOID RETURN.** Admitting the wide
            // population killed ChunkFill, and the `wide_mask != 0` clause that hid it was
            // therefore masking a different bug. Bisecting by hand named it: the one entry the
            // wide arm admits and the working ones do not is
            // `Shapes$DoubleLineConsumer.consume(DDDDDD)V` — six doubles AND **void**, against
            // `IndexMerger$IndexConsumer.merge(III)Z`. Refusing void with EVERY wide mask allowed
            // (`RAVA_CAP_WIDE=nonvoid`) leaves all four gates bit-exact, and so does allowing wide
            // arguments among non-void callees. The thunk returns the impl's raw bits, which a
            // void body does not define.
            // ⭐**was ONE Win32 ENVIRONMENT-BLOCK QUERY PLUS A `String` ALLOCATION PER CALL, is
            // one per PROCESS.** `install_cap_thunk` runs on EVERY direct hit whose entry has
            // captures — the capturing-lambda dispatch path, the hottest CSG traffic in the
            // engine — and a bisect selector is a constant of the run, not of the call.
            // `std::env::var` takes the env lock, UTF-16-encodes the name and heap-allocates the
            // value; the identical defect is documented 200 lines below in this same file, where
            // the RIP sampler billed `RtlQueryEnvironmentVariable` + `EncodeWide` at **2.5% of
            // wall with the flag UNSET**. Decoded once into a mode ordinal; the arms below are
            // byte-for-byte the arms that were there.
            static CAP_WIDE_MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
            let mode = *CAP_WIDE_MODE.get_or_init(|| {
                match std::env::var("RAVA_CAP_WIDE").ok().as_deref() {
                    Some("args") => 1,
                    Some("args_nonvoid") => 2,
                    Some("nonvoid") => 3,
                    Some("voidzero") => 4,
                    Some("void_small") => 5,
                    Some("all") => 6,
                    Some("none") => 7,
                    _ => 0,
                }
            });
            let allow = match mode {
                // BISECT-2: is it the wide ARGUMENTS or the VOID return? The one entry the wide
                // arm admits and the working ones do not share is
                // `Shapes$DoubleLineConsumer.consume(DDDDDD)V` — six doubles AND void, against
                // `IndexMerger$IndexConsumer.merge(III)Z`.
                1 => entry.wide_mask & caps_mask == 0,
                2 => entry.wide_mask & caps_mask == 0 && entry.ret.is_some(),
                3 => entry.ret.is_some(),
                4 | 6 => true,
                5 => entry.ret.is_some() || entry.wide_mask == 0 || raw.len() <= 4,
                7 => entry.wide_mask == 0,
                // ⛔**AND ALLOWING WIDE CAPTURES BUYS NOTHING.** With `wide_args == 0` as the rule
                // a full boot still reads `refused wide=81,238` — the identical count — so EVERY
                // wide entry in this workload has its wide in the ARGUMENTS. The whole remaining
                // 97% is the half the gates proved broken, and the default stays at the simplest
                // form that is known correct.
                // ⭐**THE UNION OF THE TWO PROVEN SETS.** Each half is gate-verified on its own:
                // a VOID callee with no wide parameters was the original shipped default, and
                // every wide mask among NON-void callees passes all four gates. Only the
                // INTERSECTION — void together with wide parameters — breaks, and
                // `Shapes$DoubleLineConsumer.consume(DDDDDD)V` is the shape that named it. So
                // admit the union and refuse just that corner.
                _ if cap_thunk::hr_capvoid_enabled() => true, // hr_capvoid: the corner is served by the builder's layout arm; `RAVA_NO_HR_CAPVOID=1` falls to the historic union below
                _ => entry.ret.is_some() || entry.wide_mask == 0,
            };
            let _ = wide_args;
            let why = if !allow {
                Some(0) // void AND wide — the one refused corner
            } else if entry.arg0_guard != 0 {
                Some(1)
            } else if entry.ret_ref {
                Some(2)
            } else if !entry.shift {
                Some(3)
            } else {
                None
            };
            if let Some(w) = why {
                cap_thunk::REFUSE[w].fetch_add(1, Relaxed);
                return;
            }
        }
        use std::sync::atomic::Ordering::Relaxed;
        cap_thunk::SEEN.fetch_add(1, Relaxed);
        let Some(cache_ptr) = cap_thunk::site::cache_for(caller_ptr as usize, cp) else {
            return;
        };
        // (One bump, not two — the duplicate made `SITE_OK` read as 2× `SEEN` and cost a second
        // atomic RMW on the capturing-lambda dispatch path for a census that was then wrong.)
        cap_thunk::SITE_OK.fetch_add(1, Relaxed);
        let recv = recv_cell.unwrap_or_else(|| {
            crate::nextgen::forward_if_moved(raw.first().copied().unwrap_or(0))
        });
        // ⛔**`ArenaPtr::new` IS A NULL CHECK, NOT A SHAPE TEST** — it is `NonNull::new`, so this
        // line used to hand `arena_class_id` whatever the receiver cell happened to be and read a
        // class word through it. A capturing-lambda receiver is NOT always an arena object: a
        // heap-`Ref` cell is `REF_TAG | handle` (bit 40), an address in no mapping, and reading
        // its header is a 0xC0000005 that killed **half of all product boots** — three of six,
        // reproducibly, at `DataResult$Success.ifSuccess`, faulting on exactly `0x100_0000_0000 |
        // handle` every time. It was invisible because a faulted boot exits EARLY, so the A/B
        // harness scored it as a short wall and averaged it in as a fast run.
        //
        // `is_arena_obj_addr` is the predicate every other decoder in this tree uses, and
        // `arena_lambda_of_cell` states the reason verbatim: bounding against the live arena
        // window is what makes the header read safe rather than probable. `note_serve` in
        // `jit::dispatch_table` already refuses the same population ("not an arena object: no
        // class id to index by"); this path simply never got the test.
        //
        // REJECT-ONLY. The thunk it declines to install is an optimisation — a non-arena receiver
        // has no arena class id, so `is_lambda_cid` below could never have admitted it anyway,
        // and the call falls to the general path exactly as it does for every other refusal here.
        if !crate::nextgen::is_arena_obj_addr(recv as u64) {
            return;
        }
        // SAFETY: `is_arena_obj_addr` bounded `recv` to the live arena window and proved it
        // 8-aligned, which is precisely the contract `from_addr` asks for.
        let p = unsafe { crate::arena::ArenaPtr::from_addr(recv as u64) };
        let cid = crate::nextgen::arena_class_id(p);
        if !crate::nextgen::lambdas::is_lambda_cid(cid) {
            return;
        }
        let Ok(argc) = u16::try_from(raw.len().max(1) - 1) else { return };
        // Read ONCE, same reason as `RAVA_CAP_WIDE` above: this is a per-CALL site on the
        // capturing-lambda path, and an env query here is a lock plus a UTF-16 encode of the
        // name on every one of them, flag set or not.
        static CAP_TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *CAP_TRACE.get_or_init(|| std::env::var_os("RAVA_CAP_TRACE").is_some()) {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            if entry.wide_mask != 0 && N.fetch_add(1, Relaxed) < 12 {
                let c = unsafe { &*caller_ptr };
                let m = c.file.pool.member_ref(cp).map(|m| format!("{}.{}{}", m.owner, m.name, m.descriptor)).unwrap_or_default();
                eprintln!(
                    "[CAP-TRACE] cap_n={} argc={} raw_len={} wide=0x{:x} buf_len={} ret_ref={} shift={} self_reg={} leaf={} -> {m}",
                    entry.cap_n, argc, raw.len(), entry.wide_mask, entry.buf_len,
                    entry.ret_ref, entry.shift, entry.self_reg, entry.leaf
                );
            }
        }
        // One thunk per (site, lambda site), reused; a way that flips between lambda sites more
        // than twice is refused for good -- see `cap_thunk::site_memo` for the compile loop this
        // closes.
        let site_idx = crate::nextgen::lambdas::arena_lambda_site_idx(p);
        let t = match cap_thunk::site_memo::ask(cache_ptr, cid, site_idx) {
            cap_thunk::site_memo::Verdict::Refuse => return,
            cap_thunk::site_memo::Verdict::Reuse(t) => t,
            cap_thunk::site_memo::Verdict::Build => {
                let Some(t) = cap_thunk::build_site_thunk(
                    entry.fptr,
                    entry.cap_n,
                    entry.cap_ref_mask,
                    entry.wide_mask,
                    entry.buf_len,
                    argc,
                    site_idx,
                    cache_ptr,
                    caller_ptr as i64,
                    i64::from(cp),
                    3, // kind: the lambda convention this arm serves
                    entry.ret.is_none(),
                ) else {
                    return;
                };
                cap_thunk::site_memo::built(cache_ptr, cid, site_idx, t);
                t
            }
        };
        unsafe {
            let cache = cache_ptr as *mut i64;
            let mut slot = None;
            for k in 0..4usize {
                if *cache.add(k * 2 + 1) <= 0 || *cache.add(k * 2) == i64::from(cid) {
                    slot = Some(k);
                    break;
                }
            }
            let k = slot.unwrap_or((cid as usize) & 3);
            *cache.add(k * 2) = i64::from(cid);
            *cache.add(k * 2 + 1) = t as i64;
        }
        cap_thunk::INSTALLED.fetch_add(1, Relaxed);
    }

    /// ⭐**THE RECEIVER PROFILE, WRITTEN FROM COMPILED CODE — because under `RAVA_JIT_TIER0=1`
    /// nothing else ever writes it.**
    ///
    /// `sitemono` is the per-site receiver profile `guarded_inline` reads at emit time, and its
    /// only writers were `tlcache::obj_inline_put` / `ref_inline_put` — the INTERPRETER's inline
    /// caches. Tier-0 compiles a method on its first entry, so by the time the emitter asks, that
    /// method's sites have not been interpreted and the table is empty for every one of them.
    /// This is the same shape the direct-mono fix addressed (a decision sourced from an
    /// interpreter-warmed structure that tier-0 never warms), and the same cure: source it from
    /// the arm that actually runs. A dispatch miss here IS a compiled site observing its receiver.
    ///
    /// **Called only where a NEW cache entry is created** — once per (site, receiver class), the
    /// identical frequency to the interpreter's cache put, and never on a hit. The probe's `Miss`
    /// is NOT rare (a refused site re-probes on every call and the negative cache absorbs it 63
    /// times in 64), so profiling on `Miss` itself would put a hash and a 2 MiB table probe on a
    /// per-call path — precisely the shape two measured rounds demoted to opt-in.
    ///
    /// ⚠**Why only the SUCCESSFUL resolve, and not refusals.** The interpreter's writers sit at
    /// the point where a receiver has fallen through the whole gauntlet to PLAIN bytecode
    /// dispatch — a class whose method the engine serves natively never reaches them, and must
    /// not, because splicing its guest bytecode body would run different code. A `DirectEntry`
    /// carries the same proof from the other side: `jit_direct_resolve` refuses
    /// `route_native_map`, the native-served ladder and the `java/`/`$`/guava/fastutil/slf4j
    /// owners before it can build one, so an entry IN HAND means the receiver's exact class
    /// resolves to guest bytecode. A refusal carries no such proof, so refusals write nothing —
    /// which also keeps this off the every-64th-call re-probe that refusals land on.
    ///
    /// ⚠**And no lambda ever reaches the table.** An arena lambda's class id is its CAPTURE
    /// SIGNATURE, shared by every closure of that shape, so a class-keyed "monomorphic" verdict on
    /// a lambda site lets the emitter splice ANOTHER site's body while reporting a hit. Three
    /// independent refusals: the entry-shape test below (a lambda entry always carries `shift`,
    /// `cap_n`, `arg0_guard`, `lam_fp` or `site_key`), `is_lambda_cid` on the arena receiver, the
    /// interned `$Lambda` pointer on the heap-Ref receiver — and finally `entry.guard` must equal
    /// the class-derived guard computed here, which is exactly the value the entry was cached
    /// under, so a receiver whose site keyed on identity/fingerprint cannot pass.
    #[cfg(feature = "next-gen-gc")]
    fn note_recv_profile(
        &self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
        entry: &jit::DirectEntry,
    ) {
        use crate::sitemono::census as pc;
        pc::bump(&pc::W_FILL);
        // Static (0) and invokespecial (2) have no receiver to profile, and guarded
        // devirtualisation only ever reads virtual/interface sites.
        if kind == 0 || kind == 2 {
            pc::bump(&pc::W_NOTVIRT);
            return;
        }
        if !crate::sitemono::fast_sitemono() {
            pc::bump(&pc::W_OFF);
            return;
        }
        // Lambda-shaped entries, refused by shape before the receiver is even decoded.
        if entry.shift
            || entry.cap_n != 0
            || entry.arg0_guard != 0
            || entry.lam_fp != 0
            || entry.site_key != 0
        {
            // ADDED: `sitemono` refuses a lambda receiver for a stated reason — its key is a CLASS
            // and a closure's class is its capture SIGNATURE. `jit::lambda_site_mono` is the table
            // that keys on the SITE instead, and until now its only writer was the INTERPRETED
            // apply, which tier-0 stops running. This is the same building of a direct entry, so
            // the same once-per-(site, closure-identity) frequency, and it writes the profile the
            // SAM inline chain reads. See `vm::inline_targets::note_sam_site_from_compiled`.
            if let Some(&raw0) = raw.first() {
                crate::vm::inline_targets::note_sam_site_from_compiled(
                    caller_ptr as usize,
                    cp,
                    crate::nextgen::forward_if_moved(raw0),
                );
            }
            pc::bump(&pc::W_LAMBDA);
            return;
        }
        let Some(&raw0) = raw.first() else {
            pc::bump(&pc::W_SHAPE);
            return;
        };
        // Re-barrier: the resolve that produced this entry can load a class, run its `<clinit>`
        // and move the nursery. `raw` was registered in `cells_all` across that resolve, so the
        // cell is forwarded in place — the same recovery the `learn` block beside every call site
        // of this function performs.
        let cell = crate::nextgen::forward_if_moved(raw0);
        if (cell as u64) >> 32 == (crate::value::REF_TAG as u64) >> 32 {
            let g = self.heap.class_name_ptr(cell as u32);
            // `g == 0` is a dead handle; `g == jit_lambda_scratch::get()` is the interned
            // "$Lambda" name every lambda shares.
            if g == 0 {
                pc::bump(&pc::W_DEAD);
                return;
            }
            if g == crate::jit_lambda_scratch::get() {
                pc::bump(&pc::W_LAMBDA);
                return;
            }
            if entry.guard != g {
                pc::bump(&pc::W_GUARD);
                return;
            }
            // ⚠⭐**THE PROFILE AND THE GUARD DID NOT NAME THE SAME WORD — AGAIN, AND FROM THIS
            // SIDE.** `tlcache::ref_inline_put` records the same trap for the interpreter's
            // writer: `class_name_ptr` is `Arc<str>::as_ptr`, the STRING DATA pointer at
            // `ArcInner + 16`, while `sitemono`'s `kind == 2` word is defined as "the heap-Ref
            // class word ... the emitted guard compares" — and `emit_heap_ref_class_key` loads
            // the `Arc`'s FIRST word, `ArcInner + 0` (`Heap::object_class_word`), because that is
            // the one word reachable in a single load off the object. This writer was publishing
            // the data pointer under that name, and the consequences are worse than a guard that
            // cannot hit:
            //
            //   * the two writers disagree about the SAME (site, class), so an interpreter row
            //     and a compiled row for one monomorphic site WIDEN each other to bimorphic —
            //     the compiled writer was actively destroying profiles the interpreter had
            //     already written correctly;
            //   * `note_ref`'s late-fill of `decl` only fires when `word` MATCHES, so a row
            //     claimed by this writer could never be completed by the interpreter, which is
            //     precisely the mechanism the `decl == 0` comment below relies on.
            //
            // `entry.guard`/`jit_lambda_scratch` keep using `g`: those are the direct cache's own
            // key and the interned "$Lambda" NAME, both data pointers, and both unchanged.
            // COLD ARM: an unreadable handle yields word 0, `note_ref` returns without writing,
            // the site stays unprofiled and every reader refuses it — i.e. the ordinary call.
            let w = if crate::sitemono::wide_profile() {
                self.heap.object_class_word(cell as u32).unwrap_or(0) as u64
            } else {
                g as u64 // pre-fix: a word no emitted guard can ever match
            };
            if w == 0 {
                pc::bump(&pc::W_DEAD);
                return;
            }
            // `decl` (the resolved declaring class) is 0: this path observes the receiver but not
            // the resolution, so `mono_ref` stays silent until an interpreter write late-fills it.
            // The observation is still worth making — it is what keeps the polymorphism count
            // honest, so the emitter cannot read "monomorphic" off a site that merely happened to
            // be interpreted once. `sitemono::census::DECL_BLIND` / `Q_NODECL` price exactly how
            // much of the profile this 0 costs; do not guess it, read it.
            crate::sitemono::note_ref(caller_ptr as usize, cp, w, 0);
        } else if cell != 0 && (cell as u64) >> 40 == 0 && cell & 7 == 0 {
            // An ordinary arena object — the shape test the probe uses one screen below.
            let cid = unsafe { crate::nextgen::obj_class(cell as *mut u8) };
            if crate::nextgen::is_lambda_cid(cid) {
                pc::bump(&pc::W_LAMBDA);
                return;
            }
            if entry.guard != cid as usize + 1 {
                pc::bump(&pc::W_GUARD);
                return;
            }
            crate::sitemono::note(caller_ptr as usize, cp, cid);
        } else {
            // Null, boxed, array, or a cell shape neither world claims.
            pc::bump(&pc::W_SHAPE);
        }
    }

    pub(crate) fn jit_direct_call(&mut self, caller_ptr: *const LoadedClass, cp: u16, kind: u8, raw: &[i64]) -> Option<Result<i64, VmError>> {
        // MAIN-THREAD PUBLICATION (`RAVA_MAIN_DT_LEARN`, default OFF).
        //
        // ⭐**Ledger row 330**: the wall sampler bills **JIT_CALLBACK 25.4% of the boot** —
        // **51.3 M PIC misses**, each a machine-code exit to read an answer the host already
        // knows. Row 303 closed that gap for WORKERS by teaching the per-site table from the arm
        // that actually serves (`jit_direct_call_worker(.., learn)`), and measured −20.8% of
        // `Done`. **The main thread has no such arm**: everything in `rava_dispatch_miss` that
        // publishes — the freeze table, the worker direct cache, the WS probes — sits inside
        // `if gil::is_worker()`, and blocks A+B+C (40 s of a 53.5 s boot) are SERIAL. So on the
        // thread that runs most of the boot, every verdict this function reaches is used once and
        // thrown away.
        //
        // ⚠**The site arrives by a TAKE-ONCE breadcrumb, and that is a soundness argument, not a
        // convenience.** `jit_call` is reached from places that are not a dispatch miss, and
        // publishing `(outer site, inner receiver class) -> inner fptr` would make the outer site
        // call another class's method — a wrong answer, not a slow one. `take` means only the
        // FIRST direct call after the miss can publish, which is exactly the call that miss
        // exited for; anything nested finds 0 and stays silent.
        #[cfg(feature = "next-gen-gc")]
        let learn = if main_dt_learn() { crate::jit::take_dispatch_site() } else { 0 };
        #[cfg(not(feature = "next-gen-gc"))]
        let learn = 0i64;
        let _ = learn;
        // `recv_cell` carries the probe's already-barriered receiver forward on a HIT
        // (nothing between the two can collect). The MISS arm resolves first — which
        // loads classes and can MOVE the receiver — so it hands on None and the invoke
        // half re-barriers.
        // ⭐**THE BIGGEST UNPRICED REGION IN THE ENGINE.** The module profile puts `rava::vmstate`
        // at 25.8% of host samples — the largest of any module — and the self-time decomposition
        // reached only 3.4% of it, because the fast paths that hold the rest RUN THE CALLEE and
        // were excluded by hand. The PROBE does not: it is the lookup half, and it is what those
        // 12.6 M early-returning crossings pay to be served.
        // ⭐**A DISARMED METER STILL BRACKETED THE PROBE WITH A SYNCHRONISING LOAD**
        // (`RAVA_NO_FAST_DPROBE=1` puts the unconditional form back).
        // `crate::diag::direct_probe()` is `OnceLock<Meter>::get_or_init`: an ACQUIRE load, a
        // branch and a 24-byte handle copy, after which `Meter::start` builds a `MeterGuard`
        // whose `Drop` stays live across the call — so the exact region this round is trying to
        // shorten was wrapped in an optimisation barrier on every one of those ~12.6 M crossings,
        // with `RAVA_DIAG` UNSET. Whether a meter is armed is decided once, from the environment,
        // before any guest code runs (`diag::meter` reads `selected()` at registration and the
        // flag never changes), so it is a constant of the process and belongs in a latch.
        // Armed, the meter is materialised and started exactly as before — same sampling, same
        // nesting, same numbers — and `probe_metered_latch` still calls `direct_probe()` once, so
        // the meter keeps its registry row in every dump.
        let probe = if probe_metered() {
            let _pr = crate::diag::direct_probe().start();
            self.jit_direct_probe(caller_ptr, cp, kind, raw)
        } else {
            self.jit_direct_probe(caller_ptr, cp, kind, raw)
        };
        let (entry, recv_cell) = match probe {
            jit::DirectProbe::Hit { entry, recv_cell } => {
                #[cfg(feature = "next-gen-gc")]
                if learn != 0 {
                    let recv = recv_cell.unwrap_or_else(|| {
                        crate::nextgen::forward_if_moved(raw.first().copied().unwrap_or(0))
                    });
                    jit::dispatch_table::note_serve_direct(learn, recv, kind, &entry);
                }
                #[cfg(feature = "next-gen-gc")]
                self.install_cap_thunk(caller_ptr, cp, &entry, recv_cell, raw);
                (entry, recv_cell)
            }
            jit::DirectProbe::Undecodable | jit::DirectProbe::Mega => return None,
            jit::DirectProbe::Miss { recv_guard } => {
                let site = (caller_ptr as usize, cp);
                // P3-S1 FROZEN DIRECT CACHE: inside a GIL-free region the direct cache is
                // READ-ONLY — no neg-counter bump, no resolve, no insert. This makes the whole
                // jit_direct_call body free of shared-Vm mutation on a worker (box_ref routes to
                // the worker's arena segment; TLS/heap reads are safe), so a worker may call it
                // WITHOUT the resolve lock and be served by the 16-way per-site cache the serial
                // warmup populated — including the lambda identity/fp entries that the freeze
                // table cannot hold. A miss falls to the locked path (which also does not insert).
                // WORKER-OWNED FILL. The read-only rule above is about the SHARED cache; the
                // resolve was never the problem, and refusing it sends the call to the locked
                // gauntlet. `jit_direct_call_worker` gained this arm first, but the RING lane
                // reaches the `&mut` entry point here (via `rava_dispatch_miss`'s step 0b), so
                // without the same arm here the lane never saw the change at all.
                #[cfg(feature = "next-gen-gc")]
                if crate::parallel::gilfree() {
                    if !crate::worker_direct::enabled() {
                        return None;
                    }
                    let e = match crate::worker_direct::get(site, recv_guard) {
                        Some(None) => return None,
                        Some(Some(e)) => e,
                        None => {
                            frames::cells_push((raw.as_ptr(), raw.len()));
                            let r = self.jit_direct_resolve(caller_ptr, cp, kind, raw, site);
                            frames::cells_pop();
                            crate::worker_direct::put(site, recv_guard, r);
                            let e = r?;
                            // Compiled-code receiver profile, from the worker's own fill. The
                            // table is claim-once and release-published, so a worker writing it
                            // concurrently with the mutator publishes a whole row or none.
                            self.note_recv_profile(caller_ptr, cp, kind, raw, &e);
                            e
                        }
                    };
                    return self.jit_direct_invoke(e, None, caller_ptr, cp, kind, raw);
                }
                // NEGATIVE CACHE first: a previously-refused (site, class) skips the
                // full re-resolve, re-probing every 64th call (a callee that compiles
                // later is picked up within 64 calls; the general path is correct
                // meanwhile). This was 98% of all direct misses = 176M re-resolves.
                let nkey = (caller_ptr as usize, cp, recv_guard);
                // ⭐**THE OTHER HALF OF THE COVERAGE QUESTION, FOR FREE.** `note_recv_profile`
                // runs only where a direct entry is BUILT, so every (site, receiver class) the
                // resolver REFUSES goes unprofiled — and the negative cache's own header says
                // refusals were 98% of all direct misses. Counting the GENUINELY NEW ones (the
                // 1-in-64 re-probe lands here again and must not be tallied twice) puts a number
                // on that, and it costs a stack bool: whether the key already existed is the
                // answer this `get_mut` has already computed.
                let mut neg_seen = false;
                // HR-SYNTH (`RAVA_NO_HR_SYNTH`): this key's verdict is "native-served, and a
                // plan for dispatching it straight to the JDK native layer is on file". Read
                // off the SAME counter word the negative cache already probes — one bit test,
                // no second lookup — and served below, outside the `get_mut` borrow.
                let mut hr_memo = false;
                if let Some(c) = self.jit_direct_neg.get_mut(&nkey) {
                    neg_seen = true;
                    if *c & NEG_NATIVE != 0 {
                        hr_memo = true;
                    } else {
                        // ⭐**was O(1) FULL RE-RESOLVES per 64 calls of a verdict that cannot
                        // change, is 0** — what grows is refused (site, receiver-class) pairs ×
                        // the calls through them. The 1-in-64 re-probe exists for exactly ONE
                        // refusal, `refuse(23)` "callee not compiled yet"; every other refusal
                        // this cache holds is a function of the caller's constant pool and the
                        // receiver's CLASS, and the key already pins both. Re-deriving those was
                        // a `member_ref` decode, a class-name decode and the five-substring
                        // native-served ladder, per 64 calls, forever (row 163: `dmiss_res` is
                        // 99.1% of all direct misses and row 164 puts 87% of them in one clause).
                        // The permanent bit rides in bit 31 of the existing counter, so the table
                        // costs **zero extra bytes** and `pic_fill`'s independent `% 64` discipline
                        // over the same map reads exactly what it read before.
                        if fast_directres() && *c & NEG_PERMANENT != 0 {
                            jit::count_direct_miss(2);
                            return None;
                        }
                        *c = c.wrapping_add(1);
                        if *c % 64 != 0 {
                            jit::count_direct_miss(2);
                            return None;
                        }
                    }
                }
                // The memoised native-served verdict: dispatch to the native from here, with
                // no resolve, no synthetic frame and no `opcodes::invokevirtual`. `None` from
                // the serve (a receiver the plan refuses, an unhandled call, a verdict that
                // flipped) is the general path — which is what every call through this key
                // took before, so a refusal here is never a wrong answer.
                #[cfg(feature = "next-gen-gc")]
                if hr_memo {
                    jit::count_direct_miss(2);
                    return self.hr_synth_serve(nkey, caller_ptr, cp, kind, raw);
                }
                let _ = hr_memo;
                // ROOT the in-flight `raw` args across the resolve: jit_direct_resolve loads
                // the callee class → runs its <clinit> → can MOVE + reset the nursery. The
                // per-arg forward_if_moved below only recovers a still-intact forward word
                // (window A); if the collection reset the young region and RE-BUMPED an arg's
                // slot into another object (window B), the forward word is gone and the arg is
                // unrecoverably stale (the worldgen mappedDF → Holder$Direct.value strand:
                // HolderHolder.mapAll passes the recursive-mapAll result to Holder$Direct.<init>
                // via this path, and the first-time resolve's <clinit> freed it). Registering
                // `raw` in cells_all makes the collection forward it IN PLACE, so the copy below
                // reads the live pointers regardless of window.
                #[cfg(feature = "next-gen-gc")]
                frames::cells_push((raw.as_ptr(), raw.len()));
                let resolved = self.jit_direct_resolve(caller_ptr, cp, kind, raw, site);
                #[cfg(feature = "next-gen-gc")]
                frames::cells_pop();
                match resolved {
                    Some(e) => {
                        Self::debug_assert_not_gilfree("jit_direct_cache");
                        crate::pilot_census::note_direct_fill();
                        self.jit_direct_cache
                            .entry(site)
                            .or_default()
                            .entry(recv_guard)
                            .or_default()
                            .push(e);
                        self.jit_direct_neg.remove(&nkey);
                        #[cfg(feature = "next-gen-gc")]
                        if learn != 0 {
                            let recv =
                                crate::nextgen::forward_if_moved(raw.first().copied().unwrap_or(0));
                            jit::dispatch_table::note_serve_direct(learn, recv, kind, &e);
                        }
                        // THE COMPILED-CODE RECEIVER PROFILE. Once per (site, receiver class), on
                        // the fill that creates the entry — see `note_recv_profile`.
                        #[cfg(feature = "next-gen-gc")]
                        self.note_recv_profile(caller_ptr, cp, kind, raw, &e);
                        // ⭐**THE CAPTURE THUNK WAS INSTALLED ONE CALL LATE, ALWAYS.**
                        // `install_cap_thunk` ran only on a direct HIT, so a site got its PIC
                        // entry on its SECOND call through and a site called exactly once never
                        // got one at all — the same "needs a previous execution" shape tier-0
                        // makes expensive everywhere else. The entry `e` here is byte-for-byte
                        // what the hit arm would hold, and `recv_cell: None` makes the callee
                        // re-barrier `raw[0]` itself, which is required: the resolve above can
                        // have moved it. Every refusal inside `install_cap_thunk` is unchanged,
                        // so a site it would not serve costs one `cap_n == 0` compare.
                        // `RAVA_NO_CAP_THUNK_MISS=1` restores the hit-only wiring.
                        #[cfg(feature = "next-gen-gc")]
                        if cap_thunk_miss() {
                            self.install_cap_thunk(caller_ptr, cp, &e, None, raw);
                        }
                        (e, None)
                    }
                    None => {
                        // DIAGNOSTIC (RAVA_TRACE_DMISS): name each NEWLY-refused direct
                        // call once (before the negative cache absorbs it) — the refused
                        // population IS the per-call dispatch-tax the next lever targets.
                        // Read ONCE: this arm is not cold. The negative cache re-probes
                        // every 64th call and a permanently-unresolvable callee lands
                        // back here each time, so an inline `var_os` here is a Win32
                        // env-block query (lock + UTF-16 encode of the name) on a path
                        // that runs millions of times per boot. Measured on ShapeOr with
                        // the RIP sampler: `RtlQueryEnvironmentVariable` + its
                        // `EncodeWide` were 2.5% of wall, with the flag UNSET.
                        if trace_dmiss_enabled() {
                            let caller: &LoadedClass = unsafe { &*caller_ptr };
                            if let Some(m) = caller.file.pool.member_ref(cp) {
                                eprintln!("[DMISS] {}.{}{}", m.owner, m.name, m.descriptor);
                            }
                        }
                        Self::debug_assert_not_gilfree("jit_direct_neg");
                        // PROFILE COVERAGE: a new (site, receiver class) that produced no entry,
                        // therefore no receiver profile. See `sitemono::census`.
                        if !neg_seen {
                            crate::sitemono::census::bump(
                                &crate::sitemono::census::W_NEWREFUSAL,
                            );
                        }
                        // Count only a GENUINELY new refusal. An existing entry re-probes
                        // every 64th call and lands here again, so an unconditional tally
                        // reads as `gen_calls / 64` and would libel a fully-warm cache as
                        // still discovering cold sites.
                        if crate::pilot_census::enabled() && !self.jit_direct_neg.contains_key(&nkey) {
                            crate::pilot_census::note_direct_neg();
                        }
                        // Stamp the verdict PERMANENT when the refusal that produced it was a
                        // function of the constant pool and the receiver class alone (see
                        // `perm_refusal_note`). Read unconditionally so the thread-local is
                        // consumed on every refusal and cannot leak into the next one.
                        let perm = perm_refusal_take();
                        // Consumed unconditionally, for the same reason as `perm` above: a mark
                        // must never survive into the next refusal on this thread.
                        let hr_hint = hr_native_take();
                        let slot = self.jit_direct_neg.entry(nkey).or_insert(0);
                        if perm && fast_directres() {
                            *slot |= NEG_PERMANENT;
                        }
                        jit::count_direct_miss(2);
                        // HR-SYNTH: the resolver said "native-served". Build the dispatch plan
                        // for this (site, receiver class) — re-deriving the interpreter's own
                        // verdict rather than trusting the mark — and, if one exists, serve
                        // THIS call from it. Every refusal inside is the historic `None`.
                        #[cfg(feature = "next-gen-gc")]
                        if hr_hint != 0 {
                            hr_synth::bump(&hr_synth::HINTS);
                            if let Some(r) = self.hr_synth_fill(nkey, caller_ptr, cp, kind, raw) {
                                return Some(r);
                            }
                        }
                        let _ = hr_hint;
                        return None;
                    }
                }
            }
        };
        self.jit_direct_invoke(entry, recv_cell, caller_ptr, cp, kind, raw)
    }

    /// GAP-S09 stage 4: the WORKER arm of the direct compiled→compiled call — a
    /// SHARED borrow, so the type system proves a gil-free worker's direct call
    /// mutates no `Vm` state. Exactly the gilfree read-only arm of
    /// `jit_direct_call`: a cached hit is packed and invoked (`jit_direct_invoke`
    /// types against `&self`; `box_ref` routes to the worker's TLS box table);
    /// ANY non-hit — undecodable receiver, megamorphic site, or a plain miss —
    /// returns None with NO negative-cache bump, NO resolve and NO insert, which
    /// is byte-for-byte what the `&mut` fn does under `parallel::gilfree()`.
    /// Callers: the S2 chunk-fill worker (via `WorkerVmRef::jit_direct_call_worker`).
    /// `learn` is the compiled site's baked IC address, or 0 for "do not publish". A hit here is a
    /// verdict the machine code could have used and never sees, so — when the dispatch table is
    /// armed — mirror it into the site's table on the way through. See
    /// `dispatch_table::note_serve_direct` for why only some entries qualify.
    pub(crate) fn jit_direct_call_worker(&self, caller_ptr: *const LoadedClass, cp: u16, kind: u8, raw: &[i64], learn: i64) -> Option<Result<i64, VmError>> {
        match self.jit_direct_probe(caller_ptr, cp, kind, raw) {
            jit::DirectProbe::Hit { entry, recv_cell } => {
                #[cfg(feature = "next-gen-gc")]
                if learn != 0 {
                    let recv = recv_cell.unwrap_or_else(|| {
                        crate::nextgen::forward_if_moved(raw.first().copied().unwrap_or(0))
                    });
                    jit::dispatch_table::note_serve_direct(learn, recv, kind, &entry);
                }
                self.jit_direct_invoke(entry, recv_cell, caller_ptr, cp, kind, raw)
            }
            // WORKER-OWNED DIRECT ENTRIES (`RAVA_NO_WORKER_DIRECT_FILL` disables).
            //
            // "Returns None with NO resolve and NO insert" was not a soundness requirement, it was
            // a consequence of the fill mutating the SHARED `jit_direct_cache`. The measured price
            // is the whole reason this lane serialises: static and invokespecial sites are
            // structurally non-PIC-able, so the per-site direct cache is the only mechanism that
            // can serve them, and a worker that may never build an entry falls to the locked
            // gauntlet on every single one — 4.04 M plain invokes per ring region, whatever main
            // warmed, because the sites main happened to fill are not the sites a worker reaches.
            //
            // The resolve itself never needed `&mut`: `jit_direct_resolve` is `&self` now, its
            // static/special resolutions come from the shared publication table, and its virtual
            // ones from per-thread caches. So the worker resolves and caches in ITS OWN table —
            // no shared mutation, no lock, and no visibility to any other thread.
            jit::DirectProbe::Miss { recv_guard } => {
                if !crate::worker_direct::enabled() {
                    return None;
                }
                let site = (caller_ptr as usize, cp);
                let entry = match crate::worker_direct::get(site, recv_guard) {
                    // A cached NEGATIVE. Re-resolving a permanently-unservable callee on every
                    // call would cost more than the locked path it is trying to avoid.
                    Some(None) => return None,
                    Some(Some(e)) => e,
                    None => {
                        // ROOT the in-flight args across the resolve, exactly as the `&mut` arm
                        // does: resolution can load a class and move the nursery under them.
                        #[cfg(feature = "next-gen-gc")]
                        frames::cells_push((raw.as_ptr(), raw.len()));
                        let r = self.jit_direct_resolve(caller_ptr, cp, kind, raw, site);
                        #[cfg(feature = "next-gen-gc")]
                        frames::cells_pop();
                        crate::worker_direct::put(site, recv_guard, r);
                        let e = r?;
                        // As the mutator's fill arm: profile the receiver once per (site, class).
                        #[cfg(feature = "next-gen-gc")]
                        self.note_recv_profile(caller_ptr, cp, kind, raw, &e);
                        e
                    }
                };
                #[cfg(feature = "next-gen-gc")]
                if learn != 0 {
                    let recv = crate::nextgen::forward_if_moved(raw.first().copied().unwrap_or(0));
                    jit::dispatch_table::note_serve_direct(learn, recv, kind, &entry);
                }
                self.jit_direct_invoke(entry, None, caller_ptr, cp, kind, raw)
            }
            _ => None,
        }
    }

    pub(crate) fn jit_direct_probe(&self, caller_ptr: *const LoadedClass, cp: u16, kind: u8, raw: &[i64]) -> jit::DirectProbe {
        let site = (caller_ptr as usize, cp);
        // Receiver-class guard for this call (interned class-name ptr; 0 for
        // static). For virtual/interface a null/array/boxed receiver can't take
        // the direct path → general path.
        // Static (0) and invokespecial (2) are monomorphic per site → no receiver-class
        // guard; the receiver cell (if any) passes through to the compiled callee.
        // Virtual/interface: guard by the receiver's class — MUST match the key
        // jit_direct_resolve computed (forward barrier first, then Ref interned-name
        // pointer OR arena class-id+1).
        // The receiver cell this probe read-barriered, handed to the invoke half on a
        // HIT so the same barrier is not run twice on the same cell (see DirectProbe).
        // A static/special site never barriers one here — the invoke half only needs
        // it to decode a lambda receiver, which those kinds do not have.
        let mut barriered: Option<i64> = None;
        let recv_guard = if kind == 0 || kind == 2 {
            0usize
        } else {
            let Some(&raw0) = raw.first() else {
                return jit::DirectProbe::Undecodable;
            };
            let cell = crate::nextgen::forward_if_moved(raw0);
            barriered = Some(cell);
            if (cell as u64) >> 32 == (crate::value::REF_TAG as u64) >> 32 {
                let g = self.heap.class_name_ptr(cell as u32);
                if g == 0 {
                    return jit::DirectProbe::Undecodable;
                }
                // A DIRECTABLE lambda receiver guards by IDENTITY (every lambda
                // shares the class name "$Lambda"); the interned ptr was learned
                // on the first lambda resolve. Ref handles are stable, so the
                // identity survives collections. Capturing lambdas keep the
                // class-ptr guard — one shared negative entry per site instead
                // of one per closure instance.
                // (`&& g != 0` stood here and was unreachable: `g == 0` returned `Undecodable`
                // eight lines up. It only ever added a compare to the path it claimed to guard.)
                if g == crate::jit_lambda_scratch::get() {
                    // λ-IMPL KEY (RAVA_LAMBDA_IMPL_KEY, gated): ONE key for every lambda
                    // receiver — the pinned call-site identity. This is what lets a
                    // capturing site go monomorphic: the receiver differs per closure, the
                    // implementation does not. Costs one pointer load instead of
                    // `lambda_directable` + a 3-string FNV fold, and is exact rather than
                    // a hash. `lambda_site_pin` (at resolve) makes the address immortal.
                    if crate::jit::lambda_impl_key_enabled() {
                        let k = self.heap.lambda_site_key(cell as u32);
                        if k == 0 {
                            return jit::DirectProbe::Undecodable; // not a live lambda → general path
                        }
                        k as usize
                    } else if self.heap.lambda_directable(cell as u32) {
                        cell as usize // non-capturing singleton → identity guard
                    } else {
                        // V3 (RAVA_CAP_LAMBDA): CAPTURING lambda — key by the content
                        // fingerprint (lambda_fp): identical for every instance of an impl,
                        // distinct per impl, so the site caches each impl as its own N-way
                        // entry (oligomorphic HIT; JIT_PIC_CAP + the negative cache bound
                        // thrash → general path). The hit-path fp re-check reads the REAL
                        // handle, so an fp collision / swept+reused handle falls to general.
                        let fp = self.heap.lambda_fp(cell as u32);
                        if fp == 0 {
                            return jit::DirectProbe::Undecodable; // not a live lambda (anomaly) → general path
                        }
                        fp as usize
                    }
                } else {
                    g
                }
            } else if fast_dprobe() && cell != 0 && (cell as u64) >> 40 == 0 && cell & 7 == 0 {
                // ⭐**ONE HEADER READ, AND THE RARE ARM SECOND** (`RAVA_NO_FAST_DPROBE=1` restores
                // the two arms below, in their original order).
                //
                // The two arms below ask the same object the same question twice, in the wrong
                // order. The arena-LAMBDA arm ran FIRST on every arena receiver, and reaching its
                // verdict costs three loads before the ordinary case is even considered:
                // `arena_lambda_enabled()` (a `OnceLock`, i.e. an ACQUIRE load),
                // `is_arena_obj_addr` (a relaxed load of the shared `NEXT_LOW` plus two compares),
                // and then the object's class word to ask whether that class is a registered
                // lambda. For the overwhelming majority the answer is "no" — and the ordinary-
                // object arm immediately loaded THE SAME class word again. Two of the three are
                // atomics, and their cost here is not the `mov`: it is that LLVM may not hoist or
                // reorder across them, which is the lesson `gc_arena.rs` records verbatim for
                // `NEXT_LOW` and the one that bought −15% when a single relaxed atomic came off
                // the interpreter push path.
                //
                // So load the class word ONCE and let it pick the arm. `arena_lambda_of_cell`'s
                // final gate IS `is_lambda_cid(obj_class(p))` on this exact word, so
                // `!is_lambda_cid(cid)` PROVES it would have returned `None` — i.e. proves the
                // ordinary-object arm is what the old order reaches. When the word DOES name a
                // lambda class, the unmodified `arena_lambda_of_cell` is called and decides, so
                // the flag, the arena-window bound and `RAVA_LM_VERIFY` all keep their exact
                // meaning on the only arm that can take them.
                //
                // ⚠The dereference is neither new nor newly-founded: the ordinary-object arm
                // below already reads this word under exactly this shape test (`cell != 0`, no
                // high tag, 8-aligned), so the set of addresses whose header is read is unchanged.
                // Every address `is_arena_obj_addr` admits also satisfies the shape test — the
                // arena band starts at `ARENA_FLOOR` (8 GiB) and `NEXT_LOW` never reaches 2^40 —
                // so nothing the lambda arm used to serve is lost; and for the impossible converse
                // the original arms are still below, in their original order.
                let cid = unsafe { crate::nextgen::obj_class(cell as *mut u8) };
                if crate::nextgen::is_lambda_cid(cid) {
                    match crate::nextgen::arena_lambda_of_cell(cell) {
                        // RAVA_ARENA_LAMBDA: key on the interned site, matching the guard
                        // `jit_direct_resolve` caches under. The arena CLASS would be wrong
                        // here — it is shared by every site with the same capture kinds.
                        Some((_, si)) => crate::nextgen::arena_lambda_guard(si),
                        // A lambda CLASS whose cell the decoder still refuses (arena-lambda flag
                        // off, or outside the live arena window). The old order produced the
                        // ordinary-object guard here too.
                        None => cid as usize + 1,
                    }
                } else {
                    cid as usize + 1
                }
            } else if let Some((_, si)) = crate::nextgen::arena_lambda_of_cell(cell) {
                // RAVA_ARENA_LAMBDA: key on the interned site, matching the guard
                // `jit_direct_resolve` caches under. The arena CLASS would be wrong
                // here — it is shared by every site with the same capture kinds.
                crate::nextgen::arena_lambda_guard(si)
            } else if cell != 0 && (cell as u64) >> 40 == 0 && cell & 7 == 0 {
                unsafe { crate::nextgen::obj_class(cell as *mut u8) as usize + 1 }
            } else {
                jit::count_direct_miss(0);
                return jit::DirectProbe::Undecodable;
            }
        };
        // Select the cached entry whose guard matches this receiver's class (a
        // lambda entry additionally re-checks the class its virtual target was
        // resolved against on raw[1]). On a miss with room left, resolve+cache
        // for this class. There is no per-site cap on the nested cache (O(1) by
        // class), so the historical RAVA_PIC_CAP knob no longer applies here —
        // its per-call OnceLock read was pure warm-path overhead and is gone.
        // O(1) by receiver-class guard, then a tiny scan of the (usually length-1)
        // arg0-guard/lambda collision list for this class. A site with 100+ classes
        // is served per-class in O(1) — no linear scan, so it stays on the direct
        // path (was: >16 classes fell to the slow general path; raising the flat cap
        // exploded the scan). First call per class Misses → resolves → caches; the
        // rest Hit. No megamorphic "full" state.
        match self
            .jit_direct_cache
            .get(&site)
            .and_then(|by_class| by_class.get(&recv_guard))
            .and_then(|v| {
                v.iter()
                    .find(|e| {
                        e.arg0_guard == 0 || {
                            let a0 = raw.get(1).map(|&c| crate::nextgen::forward_if_moved(c)).unwrap_or(0);
                            if (a0 as u64) >> 32 == (crate::value::REF_TAG as u64) >> 32 {
                                self.heap.class_name_ptr(a0 as u32) == e.arg0_guard
                            } else if a0 != 0 && (a0 as u64) >> 40 == 0 && a0 & 7 == 0 {
                                (unsafe { crate::nextgen::obj_class(a0 as *mut u8) } as usize + 1) == e.arg0_guard
                            } else {
                                false
                            }
                        }
                    })
                    .copied()
            }) {
            // `barriered` is handed on unconditionally — it is a value the probe
            // already holds, so there is no gate to read here; the invoke half
            // decides whether to trust it.
            Some(entry) => jit::DirectProbe::Hit { entry, recv_cell: barriered },
            None => jit::DirectProbe::Miss { recv_guard },
        }
    }
}

/// PERMANENT bit of a `jit_direct_neg` counter. Bit 31 of a `u32` that only ever counts calls
/// between re-probes, so it is unreachable by counting (2^31 calls through ONE refused site) and
/// invisible to the `% 64` re-probe arithmetic — which is what lets `pic_fill`, which shares this
/// map and knows nothing about permanence, keep its own discipline unchanged. **The table grows
/// by zero bytes**: no new map, no new field, no per-site allocation.
pub(crate) const NEG_PERMANENT: u32 = 1 << 31;

/// NATIVE-SERVED bit of a `jit_direct_neg` counter (HR-SYNTH, `RAVA_NO_HR_SYNTH`). Bit 30: like
/// bit 31, unreachable by counting and invisible to the `% 64` re-probe arithmetic that
/// `pic_fill` shares. Set only when a dispatch plan for this key is on file in
/// [`hr_synth::PLANS`]; cleared when that plan is dropped. Never set with the kill
/// switch engaged, so the historic arm below it is byte-for-byte the code that shipped.
pub(crate) const NEG_NATIVE: u32 = 1 << 30;

/// `RAVA_NO_FAST_DIRECTRES=1` (or `RAVA_NO_EMIT_MORE=1`) restores the unconditional 1-in-64
/// re-resolve of every refused (site, receiver class), including the refusals whose verdict is a
/// constant of the constant pool and the receiver's class.
pub(crate) fn fast_directres() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_DIRECTRES").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// `RAVA_NO_FAST_DPROBE=1` (or `RAVA_NO_EMIT_MORE=1`) restores the probe exactly as it stood:
/// the arena-LAMBDA receiver test first — its own `OnceLock` flag read, its own relaxed load of
/// the shared `NEXT_LOW` arena bound and its own class-word load — followed by a SECOND class-word
/// load for the ordinary-object arm; plus the per-call `OnceLock` materialisation of the price
/// meter around every probe. Nothing about the guards or the cached verdicts differs between the
/// two settings: the fast form reaches the same `recv_guard` by asking the object once.
///
/// ⭐**LATCHED IN A RELAXED BYTE RATHER THAN A `OnceLock`, AND THAT IS THE POINT OF THE GATE.**
/// `OnceLock::get_or_init` is an ACQUIRE load, and an acquire is a reordering barrier LLVM has to
/// honour. Spending one to read a switch whose whole job is to remove two atomics from this path
/// would hand the win straight back — the same trap `gc_arena.rs` names when it hoists `NEXT_LOW`
/// out of its per-cell scan ("the cost of an atomic here is not the `mov`, it is that LLVM may not
/// hoist it"), and the one that paid −15% when a single relaxed atomic came off the interpreter
/// push path. A relaxed load is a plain `mov` that LLVM may sink, hoist and CSE freely.
///
/// The latched value is a constant of the process (an env var, read once, before any guest code
/// runs), so two threads racing to latch it compute the SAME byte and the race is benign — this
/// is a memo of a constant, not shared mutable state.
const DPROBE_UNSET: u8 = 2;
static FAST_DPROBE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(DPROBE_UNSET);

/// Out of line so the env query — a Win32 environment-block walk under a lock — never appears in
/// the probe's instruction stream.
#[cold]
#[inline(never)]
fn fast_dprobe_latch() -> bool {
    let on = std::env::var_os("RAVA_NO_FAST_DPROBE").is_none() && !crate::flags::emit_more_disabled();
    FAST_DPROBE.store(u8::from(on), std::sync::atomic::Ordering::Relaxed);
    on
}

#[inline(always)]
pub(crate) fn fast_dprobe() -> bool {
    match FAST_DPROBE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => fast_dprobe_latch(),
    }
}

/// Must `jit_direct_call` materialise and start the `price` meter around the probe?
///
/// Armed ⇒ yes, and the metered path is byte-for-byte the one that shipped. Disarmed ⇒ no, and the
/// probe runs with no guard alive across it at all. With the kill switch set this answers `true`
/// unconditionally, which restores the original per-call `OnceLock` read.
static PROBE_METERED: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(DPROBE_UNSET);

/// Registers the meter (so it keeps its row in every `RAVA_DIAG` dump) and remembers its verdict.
/// `Meter::armed` is fixed at registration from `RAVA_DIAG`, so one read is the whole answer.
#[cold]
#[inline(never)]
fn probe_metered_latch() -> bool {
    let on = !fast_dprobe() || crate::diag::direct_probe().armed();
    PROBE_METERED.store(u8::from(on), std::sync::atomic::Ordering::Relaxed);
    on
}

#[inline(always)]
fn probe_metered() -> bool {
    match PROBE_METERED.load(std::sync::atomic::Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => probe_metered_latch(),
    }
}

// PERMANENCE CHANNEL between `jit_direct_resolve` and the negative cache above.
//
// The resolver returns `Option<DirectEntry>` and its refusal ids are POSITIONAL (see
// `dresolve`), so neither the return type nor the id can carry "this verdict is final" without
// renumbering a diagnostic other rows are read against. A thread-local flag can: it is armed
// once per resolve and consumed once per refusal, both on the miss path that is about to do
// vastly more work than a TLS crossing. It is per-THREAD because a resolve never spans threads —
// worker and mutator each run their own.
thread_local! {
    static PERM_REFUSAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Clear the channel — called at the top of every `jit_direct_resolve`, so a verdict can never
/// be inherited from an earlier call on this thread.
pub(crate) fn perm_refusal_arm() {
    PERM_REFUSAL.with(|c| c.set(false));
}

/// Mark the refusal about to be returned as PERMANENT for this negative-cache key.
///
/// ⚠ The bar for calling this is that the verdict is a function of things the key
/// `(caller class, cp index, receiver-class guard)` already pins: the caller's constant pool
/// entry, the receiver's exact class, and dispatch resolved from those. A refusal that depends
/// on the receiver INSTANCE, on an argument, on compilation state, or on anything that can flip
/// later (notably `class_loader::has_bytecode`) must NOT be marked — it stays on the 1-in-64
/// re-probe. Marking wrongly costs direct-call COVERAGE, never correctness: the general path
/// serves the site and is always correct.
pub(crate) fn perm_refusal_note() {
    PERM_REFUSAL.with(|c| c.set(true));
}

/// Read and clear. Called on every refusal so the flag cannot survive into the next one.
pub(crate) fn perm_refusal_take() -> bool {
    PERM_REFUSAL.with(|c| c.replace(false))
}

// NATIVE-SERVED CHANNEL (HR-SYNTH). The same shape as the permanence channel above, carrying a
// different fact: "the interpreter would hand this call to `natives::try_invoke_chain`". Armed at
// the top of every `jit_direct_resolve` (and at its re-entrancy fence), set by exactly four
// refusals in the virtual/interface arm, consumed once by the miss arm. Non-zero values name the
// refusal for the census only — the consumer never routes on them.
thread_local! {
    static HR_NATIVE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// Clear the channel. Gated on the switch so the cold arm performs no TLS write at all.
#[inline]
pub(crate) fn hr_native_arm() {
    if hr_synth_enabled() {
        HR_NATIVE.with(|c| c.set(0));
    }
}

/// Mark the refusal about to be returned as NATIVE-SERVED.
///
/// ⚠ This is a HINT, not a verdict. The consumer (`Vm::hr_synth_build`) re-derives the
/// interpreter's own routing decision — `resolve_method`, `force_native_collection`,
/// `route_native_map`, the receiver-shape gauntlet — from scratch, so a wrong mark costs one
/// wasted derivation and never a wrong dispatch. With the kill switch set nothing is written.
#[inline]
pub(crate) fn hr_native_note(k: u8) {
    if hr_synth_enabled() {
        HR_NATIVE.with(|c| c.set(k));
    }
}

/// Read and clear. Called on every refusal so a mark cannot survive into the next one.
#[inline]
fn hr_native_take() -> u8 {
    if !hr_synth_enabled() {
        return 0;
    }
    HR_NATIVE.with(|c| c.replace(0))
}

/// `RAVA_NO_HR_SYNTH=1` (or `RAVA_NO_EMIT_MORE=1`) restores the historic route for a
/// native-served callee reached from compiled code: `jit_direct_resolve` refuses, the negative
/// cache absorbs 63 calls in 64, and every call builds the synthetic seam frame and runs
/// `opcodes::invokevirtual`'s whole gauntlet to reach the JDK native layer.
///
/// Resolved ONCE through a `OnceLock` (in the cold latch), then read as a relaxed byte — the
/// `fast_dprobe` discipline, for the reason recorded there: this is read on every refusal and
/// on every negative-cache hit, and an acquire load there is an optimisation barrier.
static HR_SYNTH: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(DPROBE_UNSET);

#[cold]
#[inline(never)]
fn hr_synth_latch() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_HR_SYNTH").is_none() && !crate::flags::emit_more_disabled()
    });
    HR_SYNTH.store(u8::from(on), std::sync::atomic::Ordering::Relaxed);
    on
}

#[inline(always)]
pub(crate) fn hr_synth_enabled() -> bool {
    match HR_SYNTH.load(std::sync::atomic::Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => hr_synth_latch(),
    }
}

/// `RAVA_NO_CAP_THUNK_MISS=1` (or `RAVA_NO_EMIT_MORE=1`) restores the hit-only capture-thunk
/// wiring: a site's 4-way PIC is filled on its SECOND call through, never its first.
///
/// Latched in a relaxed byte, not a `OnceLock` — the same reason recorded at `fast_dprobe`.
/// The gate is read once per direct-cache FILL (once per (site, receiver class)), which is cold,
/// but the latch is free and the `OnceLock` acquire is not.
#[cfg(feature = "next-gen-gc")]
static CAP_THUNK_MISS: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(DPROBE_UNSET);

#[cfg(feature = "next-gen-gc")]
#[cold]
#[inline(never)]
fn cap_thunk_miss_latch() -> bool {
    let on = std::env::var_os("RAVA_NO_CAP_THUNK_MISS").is_none()
        && !crate::flags::emit_more_disabled();
    CAP_THUNK_MISS.store(u8::from(on), std::sync::atomic::Ordering::Relaxed);
    on
}

#[cfg(feature = "next-gen-gc")]
#[inline(always)]
fn cap_thunk_miss() -> bool {
    match CAP_THUNK_MISS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => cap_thunk_miss_latch(),
    }
}

/// `RAVA_MAIN_DT_LEARN` — let the SERIAL path publish its dispatch verdicts, as the worker path
/// already does. Default OFF so the A/B is a same-binary flag comparison.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn main_dt_learn() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_MAIN_DT_LEARN").is_some())
}

/// **HR-SYNTH: A NATIVE-SERVED CALLEE, DISPATCHED FROM THE RESOLVE** (`RAVA_NO_HR_SYNTH`).
///
/// The population. `[PIC-COV]` on the bootstrap bench counts 58,117 synthetic-frame
/// fall-throughs per `jit_direct_resolve` attempt, 39,344 of them (67%) for a callee that is
/// native/java-served and will NEVER have an fptr. Each of those calls used to take the whole
/// seam: `jit_direct_resolve` declines, `jit_call_body` marshals the cells into a `Vec<Value>`,
/// builds a `SeamFrame`, registers it as a root, and hands it to `opcodes::invokevirtual` —
/// which pops the operands back out, roots them AGAIN in `gc_protect`, runs the two inline-cache
/// probes, classifies the site through `gauntlet_mask`, probes the receiver's class for the
/// guava/fastutil predicates, the `$Lambda` test and the synthetic-functional test, derives the
/// dispatch class, `insert(0, receiver)`s, probes `tlcache::inline_get` with a string compare,
/// re-reads the receiver from `gc_protect`, hashes the class name for `super_chain`, and only
/// then reaches `natives::try_invoke_chain`. That is the 2.60% `opcodes::invokevirtual` plus the
/// 1.08% `dispatch_owner_cascade` under compiled bodies.
///
/// The route. When the resolver's refusal is one of the four whose interpreter twin is "no body
/// → the JDK native layer" (the `hr_native_note` marks), the miss arm builds a [`NativePlan`]
/// for this `(site, receiver class)` — the super chain to walk, the site's three names, the
/// per-argument decode kinds and the return kind, all constants of the key — stamps
/// [`NEG_NATIVE`] into the negative-cache counter the key already owns, and serves the call:
/// decode the cells against the plan, root the values exactly as the interpreter does
/// (`push_call_roots`), call `try_invoke_chain` with the same chain / owner / name / descriptor
/// the interpreter would have passed, pack the result as the seam packs it. Every later call
/// through the key finds the bit and does the same with no resolve at all.
///
/// Why the guard cannot admit an unsafe shape. The plan is NOT built from the mark. It is built
/// by re-running the interpreter's own routing decision on the same inputs: `resolve_method`
/// on the receiver's dispatch class (derived exactly as `opcodes::invokevirtual` derives
/// `actual`), then `force_native_collection` / `route_native_map`, and only a "native" verdict
/// yields a plan. Every special-case arm the gauntlet applies BEFORE that decision is mirrored
/// as a REFUSAL: a lambda or synthetic-functional receiver (`is_lambda`, `Syn::FnKind` — the
/// latter re-checked per call, because it is a property of the object and not of the class the
/// key pins), a guava cache or fastutil int-set class, a `$` class, an array-typed owner,
/// `notify`/`notifyAll`, the Waitable/CacheBuilder/slf4j owners, `forEach` under the fork
/// route, and the `AbstractList.add(E)` emulation (refused only when the receiver's class
/// declares `add(int,E)` as bytecode — the exact condition the interpreter emulates on). A
/// `Value::Null`, boxed, string or array receiver never reaches the route (the resolver's
/// receiver-shape arm returns before any mark). A shape the plan cannot prove — wrong arity, an
/// undecodable descriptor, a native that declines (`Ok(None)`) — falls to the general path, which
/// is what every call through this key took before.
///
/// GC. The values are rooted by `push_call_roots` for the whole native call, as the
/// interpreter's are; `try_invoke_chain` additionally roots its argument slice
/// (`NativeArgsGuard`). Plan construction runs under `cells_push(raw)` because `resolve_method`
/// can load a class and collect. Reference cells are `forward_if_moved` before decoding (wide
/// cells are not — their bits alias young addresses). No synthetic frame is built, so nothing is
/// registered in `frames` and nothing has to be popped on the throw path: an `Err` from the
/// native returns through `Some(Err(e))`, which `jit_call_body` propagates exactly as it
/// propagates `dispatch?`.
///
/// What can change. `has_bytecode` for the dispatch class flips false→true if a real classfile is
/// loaded for a native-served name — the one verdict `jit_direct_resolve` refuses to memoise.
/// A plan whose verdict came from "no body found" re-reads that bit per call (one relaxed load
/// while nothing is overridden) and drops itself the moment it flips. (The interpreter's own
/// `tlcache::inline_put` verdict for the same `(site, class)` is sticky across that flip; this
/// route is stricter, not looser.)
#[cfg(feature = "next-gen-gc")]
pub(crate) mod hr_synth {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    /// One dispatch plan per `(caller class, cp, receiver-class guard)`. Everything here is a
    /// constant of that key: the site's names sit in an immutable constant pool, the super
    /// chain of a loaded class never changes, and the argument kinds are read off the descriptor.
    pub(crate) struct NativePlan {
        /// The site's declared owner — `try_invoke_chain`'s label owner, and the extra owner it
        /// tries after the chain when the chain does not already contain it.
        pub(crate) owner: std::sync::Arc<str>,
        pub(crate) name: std::sync::Arc<str>,
        pub(crate) desc: std::sync::Arc<str>,
        /// `Vm::super_chain(actual)`; `chain[0]` IS the dispatch class.
        pub(crate) chain: std::sync::Arc<[Box<str>]>,
        /// `owner` is not in `chain` → pass it as `extra_owner`, as the interpreter does.
        pub(crate) extra: bool,
        /// Per parameter, in raw-cell order: `b'L'` (object ref), `b'['` (array ref) or the
        /// primitive kind byte `jit_arg_kind` returned (`unpack_arg`'s own alphabet).
        pub(crate) kinds: Box<[u8]>,
        /// The return descriptor is `L…;` or `[…` — pack with `box_ref`, else `pack_local`.
        pub(crate) ref_ret: bool,
        /// The verdict came from "no body in the hierarchy" and may flip if a classfile is
        /// loaded for the dispatch class: re-read `has_bytecode(chain[0])` per call.
        pub(crate) recheck: bool,
        /// Zero parameters and a `()Z` / `()Ljava/lang/Object;` descriptor — the only shapes
        /// `jit_iter_shortcut` can serve, so it is offered the call first exactly as
        /// `jit_call_body` offers it before the frame.
        pub(crate) iter_shape: bool,
        /// Bootstrap implementation family bound after method resolution proved this is a native
        /// dispatch. The hot path routes on the semantic id instead of walking owner strings.
        pub(crate) program: Option<crate::natives::NativeProgram>,
    }

    /// The plan table. Per thread: the route runs only on the mutator (the gil-free worker arm
    /// returns before it), and a thread-local needs no lock. Plans describe resolved structure;
    /// an individual native refusal must not change admission for subsequent calls.
    thread_local! {
        pub(crate) static PLANS: std::cell::RefCell<
            crate::FastMap<(usize, u16, usize), std::sync::Arc<NativePlan>>,
        > = std::cell::RefCell::new(crate::FastMap::default());
    }

    /// Outcome of one dispatch attempt against a plan.
    pub(crate) enum Outcome {
        /// The native ran; here is what the seam would have returned.
        Served(Result<i64, crate::VmError>),
        /// This call is refused but the plan stands (a per-call receiver test failed, or the
        /// arity of the crossing does not match the plan).
        Decline,
        /// No native handled this call. Keep the structural plan and use the general path.
        Unserved,
        /// The verdict flipped (`has_bytecode`): drop the plan, let the resolver re-decide.
        Dropped,
    }

    // ── CENSUS: plain cells, never an RMW on the served path ─────────────────────────────
    /// Calls served by a plan (the native ran, or threw).
    pub(crate) static SERVED: AtomicU64 = AtomicU64::new(0);
    /// Of those, calls the iterator/entry shortcut answered before any native.
    pub(crate) static SHORTCUT: AtomicU64 = AtomicU64::new(0);
    /// Marks the resolver handed the miss arm.
    pub(crate) static HINTS: AtomicU64 = AtomicU64::new(0);
    /// Plans built.
    pub(crate) static FILLS: AtomicU64 = AtomicU64::new(0);
    /// Calls not handled by any native; diagnostic only, never an admission input.
    pub(crate) static UNSERVED: AtomicU64 = AtomicU64::new(0);
    /// Plans dropped because `has_bytecode` flipped.
    pub(crate) static DROPPED: AtomicU64 = AtomicU64::new(0);
    /// Build refusals by reason — see [`REFUSE_NAMES`].
    pub(crate) static REFUSE: [AtomicU64; 10] = [const { AtomicU64::new(0) }; 10];
    pub(crate) const REFUSE_NAMES: [&str; 10] = [
        "kind",         // 0: not invokevirtual/invokeinterface
        "gauntlet",     // 1: a name/owner the interpreter special-cases before dispatch
        "receiver",     // 2: lambda / synthetic functional / cache / int-set / `$` / undecodable
        "resolve-err",  // 3: `resolve_method` returned Err (the general path throws it)
        "bytecode",     // 4: the interpreter would run a body, not a native
        "add-emul",     // 5: the AbstractList.add(E) emulation applies
        "shape",        // 6: descriptor undecodable, or arity of the crossing != descriptor
        "array-owner",  // 7: `[`-typed owner
        "site",         // 8: no site ref for this cp
        "fnkind",       // 9: per-call: a synthetic functional under a plain-object plan
    ];

    /// A plain-cell increment: relaxed load, add, relaxed store — two `mov`s, no `lock`.
    /// The route is mutator-only, so there is nothing to race.
    #[inline(always)]
    pub(crate) fn bump(c: &AtomicU64) {
        c.store(c.load(Relaxed).wrapping_add(1), Relaxed);
    }

    /// `[HR-SYNTH]` line for the periodic profiler dump. Empty until anything fired.
    pub(crate) fn report() -> String {
        let served = SERVED.load(Relaxed);
        let hints = HINTS.load(Relaxed);
        let fills = FILLS.load(Relaxed);
        if served + hints + fills == 0 {
            return String::new();
        }
        let refused: Vec<String> = REFUSE
            .iter()
            .zip(REFUSE_NAMES)
            .filter(|(c, _)| c.load(Relaxed) > 0)
            .map(|(c, n)| format!("{n}={}", c.load(Relaxed)))
            .collect();
        format!(
            "[HR-SYNTH] native-served callees dispatched from the resolve: served={served} (iter-shortcut={}) | marks={hints} plans={fills} unserved={} dropped={} | refused: {}",
            SHORTCUT.load(Relaxed),
            UNSERVED.load(Relaxed),
            DROPPED.load(Relaxed),
            if refused.is_empty() { "none".to_string() } else { refused.join(" ") }
        )
    }

    /// ⚠**MIRROR OF THE ARMS `opcodes::invokevirtual` TAKES BEFORE ITS RESOLVED DISPATCH, for a
    /// heap/arena OBJECT receiver.** Each is keyed on the site's constant-pool names exactly as
    /// the gauntlet keys it (`gauntlet_mask_scan`), and each is a REFUSAL here: a site the
    /// interpreter special-cases keeps the interpreter. Arms that only fire on receivers this
    /// route never sees are not listed: `GSON_CREATE` (a primitive receiver), `OBJECT_METHOD`
    /// and `FI_DEFAULT` (a lambda or synthetic-functional receiver, refused by `hr_synth_build`
    /// and re-checked per call). `GET_SHAPE` is a trace. `ADD_OBJ` needs the dispatch class and
    /// is tested in the builder.
    pub(crate) fn gauntlet_special(owner: &str, name: &str) -> bool {
        matches!(name, "notify" | "notifyAll")
            || (name == "get" && owner == "org/bukkit/craftbukkit/v1_12_R1/util/Waitable")
            || (name == "build" && owner == "com/google/common/cache/CacheBuilder")
            || matches!(owner, "org/slf4j/Logger" | "org/apache/logging/log4j/Logger")
            || (name == "forEach" && crate::opcodes::fork_route_foreach())
    }
}

/// Without the arena build there is no route (its rooting and receiver decode are arena
/// contracts); the report is empty so the profiler dump's one line compiles either way.
#[cfg(not(feature = "next-gen-gc"))]
pub(crate) mod hr_synth {
    pub(crate) fn report() -> String {
        String::new()
    }
}

#[cfg(feature = "next-gen-gc")]
impl Vm {
    /// The negative cache said [`NEG_NATIVE`]: serve from the plan on file, or clear the bit.
    pub(crate) fn hr_synth_serve(
        &mut self,
        nkey: (usize, u16, usize),
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> Option<Result<i64, VmError>> {
        let plan = hr_synth::PLANS.with(|t| t.borrow().get(&nkey).cloned());
        match plan {
            Some(p) => self.hr_synth_finish(nkey, &p, caller_ptr, cp, kind, raw),
            // A bit with no live plan behind it: clear it so the
            // key returns to the historic 1-in-64 discipline, and take the general path.
            _ => {
                self.hr_synth_clear_bit(nkey);
                None
            }
        }
    }

    /// The resolver marked this refusal native-served: build the plan and serve this call.
    fn hr_synth_fill(
        &mut self,
        nkey: (usize, u16, usize),
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> Option<Result<i64, VmError>> {
        // ROOT the in-flight cells across the build: `resolve_method` / `super_chain` can load a
        // class, run its `<clinit>` and move the nursery — the same discipline the resolve above
        // this arm runs under.
        frames::cells_push((raw.as_ptr(), raw.len()));
        let plan = self.hr_synth_build(caller_ptr, cp, kind, raw);
        frames::cells_pop();
        let plan = plan?;
        Self::debug_assert_not_gilfree("hr_synth plan");
        hr_synth::PLANS.with(|t| {
            t.borrow_mut().insert(nkey, std::sync::Arc::clone(&plan));
        });
        if let Some(c) = self.jit_direct_neg.get_mut(&nkey) {
            *c |= NEG_NATIVE;
        }
        hr_synth::FILLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.hr_synth_finish(nkey, &plan, caller_ptr, cp, kind, raw)
    }

    /// Dispatch against a plan and apply the outcome to the plan table and the bit.
    pub(crate) fn hr_synth_finish(
        &mut self,
        nkey: (usize, u16, usize),
        plan: &hr_synth::NativePlan,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> Option<Result<i64, VmError>> {
        match self.hr_synth_dispatch(plan, caller_ptr, cp, kind, raw) {
            hr_synth::Outcome::Served(r) => Some(r),
            hr_synth::Outcome::Decline => None,
            hr_synth::Outcome::Unserved => {
                hr_synth::bump(&hr_synth::UNSERVED);
                None
            }
            hr_synth::Outcome::Dropped => {
                hr_synth::PLANS.with(|t| {
                    t.borrow_mut().remove(&nkey);
                });
                self.hr_synth_clear_bit(nkey);
                hr_synth::DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                None
            }
        }
    }

    fn hr_synth_clear_bit(&mut self, nkey: (usize, u16, usize)) {
        if let Some(c) = self.jit_direct_neg.get_mut(&nkey) {
            *c &= !NEG_NATIVE;
        }
    }

    /// Build the plan for `(site, receiver class)` by re-deriving the interpreter's routing
    /// decision. `&self` only: nothing here mutates the Vm. `None` = keep the general path.
    fn hr_synth_build(
        &self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> Option<std::sync::Arc<hr_synth::NativePlan>> {
        use hr_synth::{bump, REFUSE};
        if kind != 1 && kind != 3 {
            bump(&REFUSE[0]);
            return None;
        }
        // SAFETY: as everywhere in this file — the loader holds every `LoadedClass` for the
        // life of the process.
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        let Ok(sr) = crate::opcodes::ref_site_ref(caller, cp) else {
            bump(&REFUSE[8]);
            return None;
        };
        let (owner, name, desc): (&str, &str, &str) = (&sr.owner, &sr.name, &sr.descriptor);
        // An array-typed owner (`[I.clone()`): the seam decodes the receiver as an Array and
        // the interpreter takes its array arm. Not a shape this route describes.
        if owner.as_bytes().first() == Some(&b'[') {
            bump(&REFUSE[7]);
            return None;
        }
        if hr_synth::gauntlet_special(owner, name) {
            bump(&REFUSE[1]);
            return None;
        }
        // THE DISPATCH CLASS, derived as `opcodes::invokevirtual` derives `actual`: a heap
        // handle's interned class name; an arena string is `java/lang/String`; any other arena
        // object its registry class. Decoded through the seam's own `unbox_ref_kind` so a pinned
        // mirror address materialises as the same `Value::Ref` the seam would have marshalled.
        let cell = crate::nextgen::forward_if_moved(*raw.first()?);
        let cls_hold: std::sync::Arc<str>;
        let actual: &str = match jit::unbox_ref_kind(self, cell, false) {
            Value::Ref(h) => {
                // A lambda dispatches through `invoke_lambda`; a synthetic functional
                // (`Syn::FnKind`) through `Vm::apply`. Neither ever reaches the natives.
                if self.heap.is_lambda(h)
                    || matches!(self.heap.get_field_syn(h, crate::heap::Syn::FnKind), Value::Str(_))
                {
                    bump(&REFUSE[2]);
                    return None;
                }
                match self.heap.class_of(h) {
                    Some(c) if !c.is_empty() => c,
                    _ => {
                        bump(&REFUSE[2]);
                        return None;
                    }
                }
            }
            Value::Obj(p) => {
                let cid = crate::nextgen::arena_class_id(p);
                if crate::nextgen::is_lambda_cid(cid) {
                    bump(&REFUSE[2]);
                    return None;
                }
                if crate::nextgen::is_arena_string(p) {
                    "java/lang/String"
                } else {
                    match crate::nextgen::class_name(cid) {
                        Some(n) => {
                            cls_hold = n;
                            &*cls_hold
                        }
                        None => {
                            bump(&REFUSE[2]);
                            return None;
                        }
                    }
                }
            }
            _ => {
                bump(&REFUSE[2]);
                return None;
            }
        };
        // The receiver classes the gauntlet dispatches itself (`cache::dispatch`,
        // `fastutil::dispatch`) and the `$` namespace the resolver refuses without a mark.
        if actual.starts_with('$')
            || crate::cache::is_cache_class(actual)
            || crate::fastutil::is_int_set(actual)
        {
            bump(&REFUSE[2]);
            return None;
        }
        // THE INTERPRETER'S VERDICT, VERBATIM (`opcodes::invokevirtual`, the `resolved` block):
        // a body found in the hierarchy runs as bytecode unless the class forces the native
        // collection layer or the body is inherited from a fastutil object map; no body means
        // the JDK native layer. An `Err` is a throw the general path must raise.
        let r = match self.resolve_method(actual, name, desc) {
            Ok(r) => r,
            Err(_) => {
                bump(&REFUSE[3]);
                return None;
            }
        };
        let forced = crate::opcodes::force_native_collection(self, actual, name, desc);
        let native = forced
            || match &r {
                Some(d) => crate::opcodes::route_native_map(d),
                None => true,
            };
        if !native {
            bump(&REFUSE[4]);
            return None;
        }
        // `AbstractList.add(E)` emulation: the interpreter's native arm first asks whether the
        // receiver's class declares `add(int,E)` AS BYTECODE and, if so, emulates
        // `add(size(), e)` through that body instead of the native. Same question, same answer.
        if name == "add" && desc == "(Ljava/lang/Object;)Z" {
            match self.resolve_method(actual, "add", "(ILjava/lang/Object;)V") {
                Ok(Some(d))
                    if d
                        .file
                        .method("add", "(ILjava/lang/Object;)V")
                        .and_then(|m| m.code.as_ref())
                        .is_some() =>
                {
                    bump(&REFUSE[5]);
                    return None;
                }
                Err(_) => {
                    bump(&REFUSE[3]);
                    return None;
                }
                _ => {}
            }
        }
        // Argument kinds off the descriptor — the same walk `jit_call_body`'s undescribed arm
        // performs per crossing, done once. A descriptor whose type never terminates walks past
        // the end; the seam clamps, this refuses.
        let inner = crate::args_of(desc);
        let bytes = inner.as_bytes();
        let mut kinds: Vec<u8> = Vec::new();
        let mut bi = 0usize;
        while bi < bytes.len() {
            let start = bi;
            let k = crate::jit_arg_kind(bytes, &mut bi);
            if bi > bytes.len() {
                bump(&REFUSE[6]);
                return None;
            }
            kinds.push(if k == b'L' {
                if bytes[start] == b'[' { b'[' } else { b'L' }
            } else {
                k
            });
        }
        // One cell per parameter plus the receiver: the crossing's arity must match the
        // descriptor's, or the plan describes a different call than the one arriving.
        if raw.len() != kinds.len() + 1 {
            bump(&REFUSE[6]);
            return None;
        }
        let rstart = crate::byte_rfind(desc, b')').map(|p| p + 1).unwrap_or(desc.len());
        let ref_ret = matches!(desc.as_bytes().get(rstart), Some(b'L') | Some(b'['));
        let chain = self.super_chain(actual);
        let extra = !chain.iter().any(|c| &**c == owner);
        // Permanent when a class or a resolved body FORCED the native; re-checked per call when
        // the verdict was "no body", which a later classfile load for the dispatch class flips.
        let recheck = !forced && r.is_none();
        let iter_shape = kinds.is_empty() && (desc == "()Z" || desc == "()Ljava/lang/Object;");
        let program = crate::natives::program_for(actual, name, desc);
        Some(std::sync::Arc::new(hr_synth::NativePlan {
            owner: std::sync::Arc::clone(&sr.owner),
            name: std::sync::Arc::clone(&sr.name),
            desc: std::sync::Arc::clone(&sr.descriptor),
            chain,
            extra,
            kinds: kinds.into_boxed_slice(),
            ref_ret,
            recheck,
            iter_shape,
            program,
        }))
    }

    /// Run one call against a plan: decode, root, call the native, pack.
    fn hr_synth_dispatch(
        &mut self,
        plan: &hr_synth::NativePlan,
        caller_ptr: *const LoadedClass,
        cp: u16,
        kind: u8,
        raw: &[i64],
    ) -> hr_synth::Outcome {
        use hr_synth::{bump, Outcome, REFUSE};
        // The one verdict that can change, re-read where the resolver would have re-read it.
        // One relaxed load and a taken-never branch while nothing is overridden.
        if plan.recheck && crate::class_loader::has_bytecode(&plan.chain[0]) {
            return Outcome::Dropped;
        }
        if raw.len() != plan.kinds.len() + 1 {
            bump(&REFUSE[6]);
            return Outcome::Decline;
        }
        // The seam offers `jit_iter_shortcut` the call before it builds a frame; so does this,
        // for exactly the descriptor shapes it can serve. Its refusal is `None` and costs the
        // shortcut's own name test.
        if plan.iter_shape {
            if let Some(bits) = self.jit_iter_shortcut(caller_ptr, cp, kind, raw) {
                bump(&hr_synth::SHORTCUT);
                bump(&hr_synth::SERVED);
                return Outcome::Served(Ok(bits));
            }
        }
        if let Some(bits) = plan
            .program
            .and_then(|program| crate::natives::invoke_program_cells(program, self, raw))
        {
            bump(&hr_synth::SERVED);
            return Outcome::Served(Ok(bits));
        }
        // RECEIVER. The key pinned its class; what the key cannot pin is the per-object
        // synthetic-functional mark, so that one is re-tested on every call.
        let recv = match jit::unbox_ref_kind(self, crate::nextgen::forward_if_moved(raw[0]), false) {
            Value::Ref(h) => {
                if matches!(self.heap.get_field_syn(h, crate::heap::Syn::FnKind), Value::Str(_)) {
                    bump(&REFUSE[9]);
                    return Outcome::Decline;
                }
                Value::Ref(h)
            }
            v @ Value::Obj(_) => v,
            _ => return Outcome::Decline,
        };
        // A no-argument resolved program needs only its receiver. Keep that argument carrier on
        // the stack instead of checking out and reserving a Vec; the ordinary call-root guard
        // still publishes the identical clone in `gc_protect`. Admission depends only on the
        // resolved program id and descriptor arity. A refusal continues through the same chain.
        static ZERO_CELLS_ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if plan.kinds.is_empty()
            && *ZERO_CELLS_ON
                .get_or_init(|| std::env::var_os("RAVA_NO_NATIVE_ZERO_CELLS").is_none())
        {
            if let Some(program) = plan.program {
                let locals = [recv];
                let roots = crate::opcodes::push_call_roots(self, locals.first(), &[]);
                let direct = crate::natives::invoke_program(
                    program,
                    self,
                    &plan.name,
                    &plan.desc,
                    &locals,
                );
                let r = match direct {
                    Ok(Some(ret)) => Ok(Some(ret)),
                    Err(e) => Err(e),
                    Ok(None) => {
                        let extra = if plan.extra { Some(&*plan.owner) } else { None };
                        crate::natives::try_invoke_chain(
                            self,
                            &plan.chain,
                            extra,
                            &plan.owner,
                            &plan.name,
                            &plan.desc,
                            &locals,
                        )
                    }
                };
                drop(roots);
                return self.hr_synth_outcome(plan, r);
            }
        }
        // ARGUMENTS, decoded against the plan into a pooled buffer — the seam's marshal loop with
        // the descriptor walk already done. Reference cells are barriered (a stale young pointer
        // is the arena stale-receiver class of bug); wide cells are not, their bits alias.
        let mut locals = self.take_vec();
        locals.reserve(plan.kinds.len() + 1);
        locals.push(recv);
        for (k, &kb) in plan.kinds.iter().enumerate() {
            let c = raw[1 + k];
            locals.push(match kb {
                b'L' => jit::unbox_ref_kind(self, crate::nextgen::forward_if_moved(c), false),
                b'[' => jit::unbox_ref_kind(self, crate::nextgen::forward_if_moved(c), true),
                prim => jit::unpack_arg(c, prim),
            });
        }
        // `PooledVals` hands the buffer back to the pool on every exit, cleared — the same
        // guard `opcodes::invokevirtual` wraps its own `locals` in.
        let locals = crate::opcodes::PooledVals::new(locals);
        // ROOT receiver and reference arguments in `gc_protect` for the whole native call —
        // `push_call_roots`, exactly as the interpreter roots them before its dispatch. The
        // guard truncates on drop, including on the throw path.
        let roots = crate::opcodes::push_call_roots(self, locals.first(), &locals[1..]);
        let direct = plan.program.map(|program| {
            crate::natives::invoke_program(program, self, &plan.name, &plan.desc, &locals)
        });
        let r = match direct {
            Some(Ok(Some(ret))) => Ok(Some(ret)),
            Some(Err(e)) => Err(e),
            Some(Ok(None)) | None => {
                let extra = if plan.extra { Some(&*plan.owner) } else { None };
                crate::natives::try_invoke_chain(
                    self,
                    &plan.chain,
                    extra,
                    &plan.owner,
                    &plan.name,
                    &plan.desc,
                    &locals,
                )
            }
        };
        drop(roots);
        self.hr_synth_outcome(plan, r)
    }

    fn hr_synth_outcome(
        &mut self,
        plan: &hr_synth::NativePlan,
        r: Result<Option<Option<Value>>, VmError>,
    ) -> hr_synth::Outcome {
        match r {
            Err(e) => {
                hr_synth::bump(&hr_synth::SERVED);
                hr_synth::Outcome::Served(Err(e))
            }
            Ok(Some(ret)) => {
                hr_synth::bump(&hr_synth::SERVED);
                // Packed as the seam packs `invoke*`'s pushed result: a reference through
                // `box_ref` (the compiled caller holds it as a JT::Ref cell), a primitive flat.
                let bits = match ret {
                    Some(v) => {
                        if plan.ref_ret { jit::box_ref(self, &v) } else { jit::pack_local(&v) }
                    }
                    None => 0,
                };
                hr_synth::Outcome::Served(Ok(bits))
            }
            // No owner on the chain claimed the call. The interpreter would raise
            // `UnsatisfiedLinkError` (or `collect_missing`'s default) here; the general path
            // does exactly that. The current outcome is not a fact about future calls.
            Ok(None) => hr_synth::Outcome::Unserved,
        }
    }
}
