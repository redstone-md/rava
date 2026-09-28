//! Lambda values: identity, capture splicing, and the four ways one is invoked.

use crate::*;

// ─────────────────────────────────────────────────────────────────────────────
// THE LAMBDA APPLY PLAN
//
// ⭐**A CALL SITE'S LINKAGE IS A CONSTANT OF THE SITE (JVMS §5.4.3.6), AND THE APPLY WAS
// RE-DERIVING IT ON EVERY EXECUTION.**
//
// A lambda's implementation target is `(site.owner, site.name, site.descriptor)` — three
// INTERNED `Arc<str>` fixed when the `invokedynamic` linked its bootstrap, and never mutated
// afterwards (`opcodes::indy::LinkedSite` is built on exactly that fact). From those three
// strings `invoke_lambda_parts` re-derived, on EVERY apply:
//
//   * `force_native_collection(owner, name)` — the fourteen-workaround predicate, entered
//     through four prefix tests over a ~44-character class name;
//   * `resolve_method(owner, name, desc)` — which lands in `tlcache::resolve_get`, **1.33% of
//     the boot's host-callback samples**: hash THREE strings, then compare them, to answer a
//     question none of whose inputs can change;
//   * `decl.file.method(name, desc)` in the flat-tier hot-note — `ClassFile::method`, **0.97%
//     of callback samples**: hash the name, then memcmp the descriptor across the overload set;
//   * and then `Vm::invoke` ran that SAME lookup a THIRD time, because it passes `midx: None`
//     while `invoke_idx` exists precisely so a site that already knows the slot can skip it.
//
// Three string-keyed lookups and a prefix scan per apply, for one pointer-keyed constant.
// `invoke_lambda_parts` is the funnel every lambda application passes through (its own row is
// 0.91% of callback samples, and both rows above are largely its callees), and one traced boot
// puts 2,007,617 applies through the arena general path alone. The plan holds the answers.
// ─────────────────────────────────────────────────────────────────────────────

/// The per-site constants of one lambda apply. Insert-once; never mutated after publication.
pub(crate) struct ApplyPlan {
    /// ⚠**THE KEY IS AN ADDRESS, SO THE TABLE MUST OWN WHAT IT POINTS AT.** Holding the `Arc`
    /// means the `LambdaSite` this entry was derived from cannot be freed while the entry
    /// lives, so its address can never be recycled by a DIFFERENT site and then served this
    /// plan. That is the exact failure this tree names for lambda guards — key on the SITE, or
    /// it splices another site's body while reporting a hit — and owning the key removes the
    /// question instead of answering it.
    _site: Arc<heap::LambdaSite>,
    /// `resolve_method(owner, name, desc)`, with a `force_native_collection` hit folded in as
    /// `None`. Both mean "take the natives arm", and nothing downstream can tell them apart, so
    /// one field records both and the gate is not re-evaluated either.
    ///
    /// ⚠**CACHING A `None` MAKES NO NEW ASSUMPTION.** `resolve_method` already memoises its own
    /// `None` under `tlcache::resolve_put` with the same key and the same lifetime argument
    /// ("a pure function of immutable post-boot loader state"); this is a cheaper key over the
    /// identical memo, not a longer-lived one.
    decl: Option<Arc<LoadedClass>>,
    /// The callee's index in `decl.file.methods`, when `tlcache::method_index` could name it.
    /// `None` reproduces the historic `midx: None` call — the lookup then happens exactly where
    /// and as often as it always did.
    midx: Option<u32>,
}

/// Front width. `Option<(usize, Rc<ApplyPlan>)>` is 16 bytes (the `Rc` supplies the niche), so
/// 64 slots is **1 KB per thread** — deliberately the same budget as `indy.rs`'s `INDY_L1`, and
/// for the reason recorded there: a per-thread table that grew 2.5 KB → 14 KB measured **+6.8%
/// WORSE** than the work it removed, because it stopped fitting in L1d. The map behind absorbs
/// capacity instead of the front growing.
const APPLY_L1: usize = 64;

thread_local! {
    /// Direct-mapped front. The slot carries the FULL key and is compared against it, so a
    /// collision MISSES rather than serving another site's plan.
    static APPLY_FRONT: std::cell::RefCell<Vec<Option<(usize, std::rc::Rc<ApplyPlan>)>>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// The plans themselves. Insert-once, never removed, never mutated after publication — so
    /// an `Rc` handed out of it stays valid for as long as this thread does.
    ///
    /// THREAD-LOCAL for the reason `indy::LINKED` gives: the canonical `Arc<LambdaSite>` is
    /// address-keyed by dispatch caches and a worker thread's exit must not free one another
    /// thread still names. Sharing this table across threads would need that argument re-made;
    /// keeping it per-thread inherits it.
    ///
    /// RETENTION: bounded by the number of distinct lambda call sites this thread applies, and
    /// it keeps nothing alive that was not already immortal — a `LambdaSite` is pinned for the
    /// process in `nextgen::lambda_sites`, and a `LoadedClass` in the loader. The entries are
    /// the two refcount bumps and the map node, nothing more.
    static APPLY_MAP: std::cell::RefCell<crate::FastMap<usize, std::rc::Rc<ApplyPlan>>> =
        std::cell::RefCell::new(crate::FastMap::default());
    /// This thread's `RAVA_NO_FAST_INDYFAM` answer: 0 = not yet asked, 1 = on, 2 = off.
    ///
    /// `Cell<u8>` with a `const` initialiser and no destructor — the ONLY shape that lowers to
    /// a single TLS-slot load. The gate is read once per lambda apply and once per capturing
    /// arena-closure birth, populations in the millions, and at that rate an acquire `OnceLock`
    /// read is an OPTIMISATION BARRIER rather than a load; that defect has been fixed four
    /// times in this tree (`diag::NONE_ARMED`, `opcodes::OP_GATES`, `frames::POOL_GATES`).
    static INDYFAM_GATE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// The front's index. Same mix as `indy::l1_slot` — one definition of "how a site scatters".
#[inline]
fn apply_slot(key: usize) -> usize {
    ((key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 19) as usize & (APPLY_L1 - 1)
}

#[inline]
fn apply_front_fill(key: usize, p: &std::rc::Rc<ApplyPlan>) {
    let i = apply_slot(key);
    APPLY_FRONT.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_empty() {
            c.resize(APPLY_L1, None);
        }
        c[i] = Some((key, p.clone()));
    });
}

/// The plan for `key` (a `LambdaSite` address), if this thread has already built one.
#[inline]
fn apply_plan_get(key: usize) -> Option<std::rc::Rc<ApplyPlan>> {
    let i = apply_slot(key);
    let front = APPLY_FRONT.with(|c| match c.borrow().get(i) {
        Some(Some((k, p))) if *k == key => Some(p.clone()),
        _ => None,
    });
    if front.is_some() {
        return front;
    }
    let hit = APPLY_MAP.with(|m| m.borrow().get(&key).cloned())?;
    apply_front_fill(key, &hit);
    Some(hit)
}

/// Publish a freshly derived plan and hand it back. Cold — once per (thread, site).
#[cold]
#[inline(never)]
fn apply_plan_put(key: usize, plan: ApplyPlan) -> std::rc::Rc<ApplyPlan> {
    let rc = std::rc::Rc::new(plan);
    APPLY_MAP.with(|m| {
        m.borrow_mut().insert(key, rc.clone());
    });
    apply_front_fill(key, &rc);
    rc
}

impl Vm {
    /// **DEFAULT ON — kill switch `RAVA_NO_FAST_INDYFAM=1`**, which also consults
    /// `flags::emit_more_disabled()`. With it engaged, `invoke_lambda_parts` re-derives the
    /// linkage per apply exactly as it did, and the capturing arena-closure birth in
    /// `opcodes::indy` roots its captures by clone rather than by move. One `u8` per thread —
    /// see `INDYFAM_GATE` for why it is not an `OnceLock` read.
    #[inline(always)]
    pub(crate) fn fast_indyfam_enabled() -> bool {
        match INDYFAM_GATE.with(|c| c.get()) {
            1 => true,
            2 => false,
            _ => Self::fast_indyfam_cold(),
        }
    }

    /// First touch on this thread: read the process constant through its `OnceLock` — the only
    /// place that still happens — and cache the byte.
    #[cold]
    #[inline(never)]
    fn fast_indyfam_cold() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let on = *ON.get_or_init(|| {
            !crate::flags::emit_more_disabled()
                && std::env::var_os("RAVA_NO_FAST_INDYFAM").is_none()
        });
        INDYFAM_GATE.with(|c| c.set(if on { 1 } else { 2 }));
        on
    }

    /// Value-level lambda snapshot: works for BOTH a 0b01 handle (`Value::Ref`) and a
    /// RAVA_ARENA_LAMBDA 0b00 arena object (`Value::Obj`). The single accessor every
    /// consumer arm routes through, so an arena-lambda is dispatched exactly like a
    /// handle-lambda without the callsite needing to know the representation.
    pub(crate) fn value_lambda_of(&self, v: &Value) -> Option<heap::Lambda> {
        match v {
            Value::Ref(h) => self.heap.lambda_of(*h),
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) if crate::nextgen::is_arena_lambda(*p) => {
                crate::opcodes::arena_lambda_to_struct(*p)
            }
            _ => None,
        }
    }

    /// Value-level `is_lambda` (existence, no clone) for both representations.
    pub(crate) fn value_is_lambda(&self, v: &Value) -> bool {
        match v {
            Value::Ref(h) => self.heap.is_lambda(*h),
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => crate::nextgen::is_arena_lambda(*p),
            _ => false,
        }
    }

    /// Value-level functional-interface class name ($fi), for both representations.
    pub(crate) fn value_lambda_fi(&self, v: &Value) -> Option<String> {
        match v {
            Value::Ref(h) => match self.heap.get_field(*h, "$fi") {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            },
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) if crate::nextgen::is_arena_lambda(*p) => {
                match crate::opcodes::arena_lambda_fi(*p) {
                    Value::Str(s) => Some(s.to_string()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// DEFAULT-OFF pending its A/B (`RAVA_LAMBDA_SPLICE` enables). The track that built it
    /// promoted it to default-ON on a structural argument - a pure allocation-count change
    /// whose argument vector is element-for-element identical in both arms - but stated
    /// plainly that no wall A/B was run, walls being forbidden in its phase. This project's
    /// standard is that only interleaved separation ships a default, and it has been burned
    /// by plausible-but-unmeasured changes repeatedly (a 9.6x microbench win that moved the
    /// boot by zero). Flip it back once measured.
    ///
    /// Gates the borrow-and-splice argument build in the lambda apply path. Pure
    /// allocation-count change — the argument vector handed to the impl body is
    /// element-for-element identical either way — so the ChunkFill/RingScan gates
    /// arbitrate the whole of the risk it carries.
    pub(crate) fn lambda_splice_enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_LAMBDA_SPLICE").is_some())
    }

    /// RAVA_LAMBDA_STAT census. Per apply it records the capture arity and the number of
    /// ARGUMENT-VECTOR ALLOCATIONS the argument build actually performed, so an ON/OFF
    /// pair prices this change directly in allocations rather than by inference from a
    /// wall. Counts only — no timing — so it stays contention-tolerant. Off by default
    /// and behind one `OnceLock` bool, so a default run pays a predicted-not-taken branch.
    #[inline]
    pub(crate) fn lambda_stat_note(captured_len: usize, argvec_allocs: u32) {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ON.get_or_init(|| std::env::var_os("RAVA_LAMBDA_STAT").is_some()) {
            return;
        }
        Self::lambda_stat_bump(captured_len, argvec_allocs);
    }

    /// Reports on STDOUT so the tally interleaves with a phase harness's own step
    /// markers in ONE stream — that is what makes a count attributable to a WINDOW
    /// (blocks A+B vs block C) rather than to the whole process. `RAVA_LAMBDA_STAT=<n>`
    /// sets the report cadence in applies (default 1,000,000).
    #[cold]
    pub(crate) fn lambda_stat_bump(captured_len: usize, argvec_allocs: u32) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static APPLIES: AtomicU64 = AtomicU64::new(0);
        static NONCAP: AtomicU64 = AtomicU64::new(0);
        static CAPTURES: AtomicU64 = AtomicU64::new(0);
        static ALLOCS: AtomicU64 = AtomicU64::new(0);
        static EVERY: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        let every = *EVERY.get_or_init(|| {
            std::env::var("RAVA_LAMBDA_STAT")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(1_000_000)
        });
        let n = APPLIES.fetch_add(1, Ordering::Relaxed) + 1;
        CAPTURES.fetch_add(captured_len as u64, Ordering::Relaxed);
        ALLOCS.fetch_add(argvec_allocs as u64, Ordering::Relaxed);
        if captured_len == 0 {
            NONCAP.fetch_add(1, Ordering::Relaxed);
        }
        if n % every == 0 {
            println!(
                "[LAMBDA-STAT] applies={} noncapturing={} captured_values={} argvec_allocs={}",
                n,
                NONCAP.load(Ordering::Relaxed),
                CAPTURES.load(Ordering::Relaxed),
                ALLOCS.load(Ordering::Relaxed)
            );
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }

    /// Invoke a lambda by VALUE (handle or arena-lambda) — dispatches without snapshotting.
    /// `LambdaMetafactory`'s RETURN adaptation, for a SAM call made from bytecode.
    ///
    /// ⛔**A `boolean` implementation reached an `Object`-returning SAM as a bare `int`.** The
    /// metafactory adapts the implementation's return to the SAM's: `R::a` (`()Z`) behind
    /// `Function<R,Object>.apply` yields a `java.lang.Boolean`. This engine invoked the
    /// implementation and pushed its `Value::Int(1)` verbatim, which reads as an `Integer`, so
    /// DFU's `optionalFieldOf(name, default)` — `Objects.equals(getter.apply(x), Boolean.TRUE)`
    /// — was `false` for every boolean record field and every default was emitted
    /// (`"sends_telemetry_event":false` on 1,458 of 1,584 advancements in bench/ReloadBench.java;
    /// the standalone form is bench/CompSem.java `opt.*`). Numbers are unaffected: `Integer` IS
    /// `Value::Int` here, so `()I` behind `apply` already reads as the box. `Boolean` is the one
    /// primitive this engine boxes to a REAL object (`Boolean.valueOf` in `cascade_table`), so it
    /// is the one return that must be converted at the SAM boundary.
    ///
    /// Applied only where the SITE descriptor says the SAM returns a reference and the
    /// implementation returns `Z`: a `Predicate.test` site (`)Z`) keeps the raw int its `ifeq`
    /// expects, and the natives' own `Vm::apply` (which reads results with `as_int`) never comes
    /// through here. `RAVA_NO_SAM_BOX=1` restores the verbatim push.
    pub(crate) fn sam_box_return(&mut self, ret: Value, site_desc: &str, impl_desc: &str) -> Value {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ON.get_or_init(|| std::env::var_os("RAVA_NO_SAM_BOX").is_none()) {
            return ret;
        }
        fn ret_of(d: &str) -> &str {
            d.rfind(')').map(|i| &d[i + 1..]).unwrap_or("")
        }
        let site_ret = ret_of(site_desc);
        if !(site_ret.starts_with('L') || site_ret.starts_with('[')) || ret_of(impl_desc) != "Z" {
            return ret;
        }
        match ret {
            Value::Int(b) => self.boolean_obj(b != 0),
            other => other,
        }
    }

    /// The implementation descriptor of the closure `v` (handle or arena lambda), for
    /// `sam_box_return`. `None` for anything that is not a closure.
    pub(crate) fn lambda_impl_desc(&self, v: &Value) -> Option<Arc<str>> {
        match v {
            Value::Ref(h) => self.heap.lambda_header(*h).map(|(_, site, _)| Arc::clone(&site.descriptor)),
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) if crate::nextgen::is_arena_lambda(*p) => {
                crate::nextgen::lambda_site_get(crate::nextgen::arena_lambda_site_idx(*p))
                    .map(|site| Arc::clone(&site.descriptor))
            }
            _ => None,
        }
    }

    pub(crate) fn invoke_lambda_value(
        &mut self,
        v: &Value,
        sam_args: Vec<Value>,
    ) -> Result<Option<Value>, VmError> {
        // A handle receiver takes the zero-snapshot path (`lambda_header`); only the
        // arena representation still has to materialise a `Lambda`, and that one is
        // CONSUMED rather than re-copied.
        if let Value::Ref(h) = v {
            return self.invoke_lambda(*h, sam_args);
        }
        // ARENA FAST PATH — build the CALL vector, not a `Lambda`. Traced on a real boot:
        // arena lambdas reach the direct-call entry build 11,532 times and this general path
        // 2,007,617 times, so whatever this path allocates is paid two million times. The
        // struct route allocates the capture `Vec` at exactly `n` (that is what
        // `Vec::with_capacity(n)` in `arena_lambda_to_struct_inner` does), and
        // `invoke_lambda_owned` then has to grow it by the SAM arity — a second malloc and a
        // memcpy of the captures. Decoding the cells straight into a vector sized
        // `n + sam_args.len()` gives the identical call vector in ONE allocation, and for a
        // NON-capturing arena lambda `sam_args` already IS the call vector, so it allocates
        // nothing at all. Same decode (`decode_arena_cell`), same order, same `invoke_lambda_parts`.
        //
        // Falls through to the struct route whenever a diagnostic is armed, so
        // RAVA_GC_STATS's snapshot accounting and RAVA_LAMBDA_STAT's allocation census keep
        // measuring the path they were written against instead of silently changing meaning.
        #[cfg(feature = "next-gen-gc")]
        if let Value::Obj(p) = v {
            if crate::nextgen::is_arena_lambda(*p) && !crate::nextgen::gc_stats_enabled() {
                let kind = crate::nextgen::arena_lambda_kind(*p);
                if let Some(site) =
                    crate::nextgen::lambda_site_get(crate::nextgen::arena_lambda_site_idx(*p))
                {
                    let n = crate::nextgen::arena_lambda_cap_n(*p);
                    let call_args = if n == 0 {
                        Self::lambda_stat_note(0, 0);
                        sam_args
                    } else {
                        Self::lambda_stat_note(n, 1);
                        let mut a = Vec::with_capacity(n + sam_args.len());
                        for i in 0..n {
                            let (c, k) = crate::nextgen::arena_lambda_cell(
                                *p,
                                crate::nextgen::AL_HDR_CELLS + i,
                            );
                            a.push(crate::opcodes::decode_arena_cell(c, k));
                        }
                        a.extend(sam_args);
                        a
                    };
                    return self.invoke_lambda_parts(kind, &site, call_args);
                }
            }
        }
        let lam = self.value_lambda_of(v).ok_or(VmError::StackUnderflow)?;
        self.invoke_lambda_owned(lam, sam_args)
    }

    /// Splice a closure's captured values in front of its SAM arguments, in ONE
    /// exact-capacity allocation.
    ///
    /// The old shape was `captured.clone()` followed by `.extend(sam_args)`. `Vec::clone`
    /// allocates at EXACTLY `len`, so the `extend` then always had to grow — every
    /// capturing apply paid a malloc, a realloc and a memcpy of the captures. And a
    /// NON-capturing apply (the majority: `lambda$foo$0` compiles to a static, and DFU's
    /// codec chain is built from them) allocated a fresh `Vec` only to copy `sam_args`
    /// into it. `sam_args` is already an owned `Vec` the caller hands over, so with no
    /// captures it simply IS the call vector — zero allocations.
    #[inline]
    pub(crate) fn splice_call_args(captured: &[Value], sam_args: Vec<Value>) -> Vec<Value> {
        if captured.is_empty() {
            return sam_args;
        }
        let mut call_args = Vec::with_capacity(captured.len() + sam_args.len());
        call_args.extend_from_slice(captured);
        call_args.extend(sam_args);
        call_args
    }

    /// Invoke a lambda closure's implementation: captured variables first, then
    /// the SAM call arguments (covers static, bound, and unbound forms).
    ///
    /// PER-CALL COPYING (block C is one long DFU codec lambda chain, so this runs on
    /// every link): this used to `lambda_of` the receiver — cloning the whole `Lambda`,
    /// i.e. a `Vec<Value>` allocation — and then clone the captures a SECOND time inside
    /// the snapshot, and then grow that clone to append the SAM args. Three heap
    /// operations to build one argument vector whose contents are already sitting in the
    /// heap object. `lambda_header` borrows the header in place instead, and the splice
    /// below is the only allocation left (none at all when the closure is non-capturing).
    ///
    /// GC-SAFETY: the `&[Value]` capture borrow is confined to the block below. Every
    /// GC-capable operation on the apply path takes `&mut self`, so borrowck rejects any
    /// version of this in which the borrow outlives the copy-out — the borrow provably
    /// cannot span a collection. The resulting `call_args` is then rooted for the whole
    /// apply by `push_call_roots`, exactly as before (see `invoke_lambda_parts`).
    pub(crate) fn invoke_lambda(
        &mut self,
        handle: u32,
        sam_args: Vec<Value>,
    ) -> Result<Option<Value>, VmError> {
        if !Self::lambda_splice_enabled() {
            // LEGACY SHAPE, spelled out rather than delegated so the OFF arm is exactly
            // the code this replaced AND its allocation ledger is exact: `lambda_of`
            // clones the whole `Lambda` (Vec #1), the snapshot clones the captures again
            // (Vec #2), and the `extend` grows that exact-capacity clone (Vec #3).
            let lam = self.heap.lambda_of(handle).ok_or(VmError::StackUnderflow)?;
            let (c, n) = (lam.captured.len(), sam_args.len());
            let mut call_args = lam.captured.clone();
            call_args.extend(sam_args);
            Self::lambda_stat_note(c, u32::from(c > 0) * 2 + u32::from(n > 0));
            return self.invoke_lambda_parts(lam.kind, &lam.site, call_args);
        }
        let (kind, site, call_args) = match self.heap.lambda_header(handle) {
            Some((kind, site, captured)) => {
                let c = captured.len();
                Self::lambda_stat_note(c, u32::from(c > 0));
                (kind, Arc::clone(site), Self::splice_call_args(captured, sam_args))
            }
            None => return Err(VmError::StackUnderflow),
        };
        self.invoke_lambda_parts(kind, &site, call_args)
    }

    /// Apply a `Lambda` the caller already owns, CONSUMING its captures instead of
    /// copying them again. The arena representation has to build the struct
    /// (`arena_lambda_to_struct` decodes the cells), so the only saving available there
    /// is not to duplicate the `Vec` a second time — which is what this does.
    pub(crate) fn invoke_lambda_owned(
        &mut self,
        lam: heap::Lambda,
        sam_args: Vec<Value>,
    ) -> Result<Option<Value>, VmError> {
        let heap::Lambda { kind, site, captured } = lam;
        let (c, n) = (captured.len(), sam_args.len());
        let call_args = if !Self::lambda_splice_enabled() {
            Self::lambda_stat_note(c, u32::from(n > 0));
            let mut v = captured;
            v.extend(sam_args);
            v
        } else if c == 0 {
            Self::lambda_stat_note(c, 0);
            sam_args
        } else {
            Self::lambda_stat_note(c, u32::from(n > 0));
            let mut v = captured;
            v.reserve_exact(n);
            v.extend(sam_args);
            v
        };
        self.invoke_lambda_parts(kind, &site, call_args)
    }

    /// Invoke an immutable lambda snapshot captured by a short-lived structural
    /// execution plan. This is the same authoritative path as `invoke_lambda`;
    /// it only avoids cloning the heap lambda metadata again for every callback.
    pub(crate) fn invoke_lambda_snapshot(
        &mut self,
        lam: &heap::Lambda,
        sam_args: Vec<Value>,
    ) -> Result<Option<Value>, VmError> {
        let (c, n) = (lam.captured.len(), sam_args.len());
        let call_args = if Self::lambda_splice_enabled() {
            Self::lambda_stat_note(c, u32::from(c > 0));
            Self::splice_call_args(&lam.captured, sam_args)
        } else {
            Self::lambda_stat_note(c, u32::from(c > 0) + u32::from(n > 0));
            let mut v = lam.captured.clone();
            v.extend(sam_args);
            v
        };
        self.invoke_lambda_parts(lam.kind, &lam.site, call_args)
    }

    /// The authoritative lambda apply, over the three things a dispatch actually needs:
    /// the MethodHandle `kind`, the interned call-site identity, and the already-spliced
    /// argument vector. Every entry point above funnels here, so the dispatch semantics
    /// live in exactly one place regardless of how the caller obtained the closure.
    pub(crate) fn invoke_lambda_parts(
        &mut self,
        kind: u8,
        site: &Arc<heap::LambdaSite>,
        call_args: Vec<Value>,
    ) -> Result<Option<Value>, VmError> {
        // GC-SAFETY: call_args (the captured values + apply args) live in this Rust Vec across
        // the GC-capable resolve/dispatch below. Unlike the invoke opcodes (push_call_roots),
        // the lambda-apply path did NOT root them — so an arena receiver/captured that was
        // reachable elsewhere (e.g. a `this.field`) got EVACUATED + that root updated, while
        // this un-rooted copy stayed stale → the lambda body derefed the old young ptr →
        // `PHASE5 arena getfield class None` in worldgen (RAVA_PROBE_STALE: WAS FORWARDED,
        // 0 scannable holders). Root them for the whole apply; the Drop guard truncates.
        #[cfg(feature = "next-gen-gc")]
        let _gc_root = crate::opcodes::push_call_roots(self, None, &call_args);

        // The call-site identity is interned and immutable, and `site` is an owned `Arc`
        // held by the CALLER — it does not borrow `self`, so these three `&str` views stay
        // valid across every `&mut self` step below without a per-call `to_string()`.
        let l_owner: &str = &site.owner;
        let l_name: &str = &site.name;
        let l_desc: &str = &site.descriptor;

        // REF_newInvokeSpecial (kind 8) — a constructor reference (`Foo::new`):
        // allocate a fresh instance, run <init> on it, and return it.
        if kind == 8 {
            if let Ok(c) = self.loader.get_or_load(l_owner) {
                self.ensure_init(&c)?;
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                let obj = self.heap.alloc(l_owner);
                let mut locals = vec![Value::Ref(obj)];
                locals.extend(call_args);
                match self.resolve_method(l_owner, "<init>", l_desc)? {
                    Some(decl) => {
                        self.invoke(decl, "<init>", l_desc, locals)?;
                    }
                    None => {
                        natives::try_invoke(self, l_owner, "<init>", l_desc, &locals)?;
                    }
                }
                return Ok(Some(Value::Ref(obj)));
            }
            #[cfg(feature = "next-gen-gc")]
            {
                // Arena-eligible receiver → Value::Obj (bump). An arena ptr MOVES under GC, and
                // <init> (below) plus resolve_method can collect, so ROOT the receiver in gc_protect
                // and re-read its FORWARDED value both after resolve_method (into locals[0]) and
                // after <init> (the return) — the invokevirtual gc_base idiom (opcodes.rs:3251).
                let recv0 = self.alloc_ctor_receiver(l_owner);
                let gc_base = crate::vm::thread_context::root_len(&self.gc_protect);
                crate::vm::thread_context::push_root(&mut self.gc_protect, recv0.clone());
                let decl = self.resolve_method(l_owner, "<init>", l_desc)?;
                let recv = crate::vm::thread_context::root_index(&self.gc_protect, gc_base); // forwarded across resolve_method
                let mut locals = vec![recv];
                locals.extend(call_args);
                match decl {
                    Some(decl) => {
                        self.invoke(decl, "<init>", l_desc, locals)?;
                    }
                    None => {
                        natives::try_invoke(self, l_owner, "<init>", l_desc, &locals)?;
                    }
                }
                let ret = crate::vm::thread_context::root_index(&self.gc_protect, gc_base); // forwarded across <init>
                crate::vm::thread_context::truncate_roots(&mut self.gc_protect, gc_base);
                return Ok(Some(ret));
            }
        }

        // Instance method references (kind 5 invokevirtual / 9 invokeinterface)
        // dispatch virtually on the receiver's actual class, not the class the
        // reference was declared against (which may be abstract). When the
        // receiver is *itself* a functional object (a lambda or a synthetic
        // $fnkind combinator) — an unbound interface-method ref like
        // `Function::apply` invoked on a lambda — its SAM is the lambda's own
        // body, so delegate to `apply` with the remaining arguments.
        if matches!(kind, 5 | 9) {
            // The receiver (call_args[0]) may itself be a functional object — a handle
            // lambda, an arena-lambda (RAVA_ARENA_LAMBDA), or a $fnkind combinator.
            //
            // ⭐**TESTED BY BORROW, OWNED ONLY WHERE OWNERSHIP IS CONSUMED.** This used to
            // `.cloned()` the receiver BEFORE the test, so every kind-5/9 apply paid a `Value`
            // clone and its drop — a tag test, a 16-byte copy, and an `Arc` bump for a `Str`
            // receiver — to answer a predicate that needs no ownership at all, and then threw
            // the copy away on the "not a functional object" arm, which is the common one.
            //     was: one Value clone+drop per virtual method-ref apply;
            //     is:  one per DELEGATED apply (the `self.apply` arm, which consumes it).
            let is_fn = match call_args.first() {
                Some(v) => {
                    self.value_is_lambda(v)
                        || matches!(v, Value::Ref(rh) if matches!(self.heap.get_field_syn(*rh, crate::heap::Syn::FnKind), Value::Str(_)))
                }
                None => false,
            };
            if is_fn {
                // An unbound interface-method ref applied to a functional
                // object. If it targets a CONCRETE default method on the
                // declared interface (e.g. DFU's Function11.curry2, which
                // RecordCodecBuilder uses to build the field product), run
                // that default's bytecode on the receiver. Only the abstract
                // SAM (no Code anywhere) means "invoke the closure", so then
                // delegate to apply.
                //
                // NOT PLANNED: this resolve is against `l_owner` (the site's DECLARED
                // interface) and reaches only the receivers that are themselves functional
                // objects, which is a small slice of a small kind — so it keeps the direct
                // `resolve_method`, whose own `tlcache` memo already serves it.
                if let Some(decl) = self.resolve_method(l_owner, l_name, l_desc)? {
                    return self.invoke(decl, l_name, l_desc, call_args);
                }
                // `is_fn` is only ever true when `first()` was `Some`, so index 0 exists.
                let recv_val = call_args[0].clone();
                let rest = call_args[1..].to_vec();
                return self.apply(&recv_val, rest);
            }
        }
        // The dispatch owner is an `Arc<str>`, not a `String`: for a static/special
        // reference (and for a method ref with no usable receiver) it is the SITE's own
        // interned owner, so the common arm is an `Arc` bump instead of a per-call copy
        // of a ~44-character class name. Only the virtual arms, which must read the
        // RECEIVER's concrete class, still materialise one.
        let owner_arc: Arc<str> = if matches!(kind, 5 | 9) {
            match call_args.first() {
                // ⭐**THE `Arc<str>` WAS ALREADY THERE; THIS BUILT A SECOND COPY OF IT.**
                // A heap `Object` stores its class name as an interned `Arc<str>` precisely
                // because "millions of objects share the same few hundred class names" — and
                // `class_of` hands out a `&str` view of it, so `Arc::from` MALLOC'D AND MEMCPY'D
                // the name back into a private allocation, on every virtual method-ref apply,
                // to be freed at the end of the same call. Cloning the interned one is a
                // refcount bump and gives the identical bytes.
                //     was: one allocation the length of the class name (~44 bytes for this
                //          jar's names) per method-ref apply; is: none.
                // `Heap::class_of` IS `get(..).map(|o| o.class.as_ref())`, so `Some`/`None`
                // partition exactly as before.
                Some(Value::Ref(h)) => match self.heap.get(*h) {
                    Some(o) => Arc::clone(&o.class),
                    None => Arc::clone(&site.owner),
                },
                // An arena receiver (next-gen-gc): e.g. `holder::value` on a
                // Holder$Reference. Its concrete class lives in the arena header — read
                // it so the ref dispatches virtually, not against the abstract interface
                // `site.owner` (which has no code → a spurious lambda-ULE).
                // An ARENA STRING's registered class is the synthetic "[arena-string]",
                // which no loader and no native owner arm knows — so a method ref BOUND
                // to a runtime-built string (`s::toUpperCase`, `s::trim`) dispatched
                // against that name, found nothing, and threw a lambda ULE. Its class is
                // java/lang/String everywhere else in the VM (getClass, instanceof); say
                // so here too.
                //
                // BUILT ONCE FOR THE PROCESS. The name is a literal — a constant of this arm,
                // not of the receiver — and `Arc::from(&str)` allocates a fresh 16-byte header
                // plus the bytes every time it is evaluated. One `OnceLock<Arc<str>>` (16 bytes
                // of static, one refcount bump per use) says the same thing.
                //     was: one allocation per bound-method-ref apply on a runtime-built string;
                //     is:  none.
                #[cfg(feature = "next-gen-gc")]
                Some(Value::Obj(p)) if crate::nextgen::is_arena_string(*p) => {
                    static STRING_OWNER: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
                    Arc::clone(STRING_OWNER.get_or_init(|| Arc::from("java/lang/String")))
                }
                #[cfg(feature = "next-gen-gc")]
                Some(Value::Obj(p)) => {
                    match crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)) {
                        Some(s) => Arc::from(s),
                        None => Arc::clone(&site.owner),
                    }
                }
                _ => Arc::clone(&site.owner),
            }
        } else {
            Arc::clone(&site.owner)
        };
        let owner: &str = &owner_arc;
        // MEMOISED (`env_flag!`), not queried per apply. On Windows `std::env::var` is an
        // environment-block query behind a lock plus a UTF-16 encode of the name and a `String`
        // for the value — the defect `flags::dal_iter_enabled` records at 1.9% of a serial boot
        // block. The `$Lambda` compare short-circuits it in the common case, but the environment
        // is written only by the CLI's config apply, which completes before the VM exists, so
        // there is nothing to re-read even when it does not.
        if owner == "$Lambda" && crate::env_flag!("RAVA_TRACE_THROW") {
            if let Some(Value::Ref(h)) = call_args.first() {
                if let Some(inner) = self.heap.lambda_of(*h) {
                    eprintln!(
                        "[MISFED-LAMBDA] {}.{} called as receiver of {}; inner impl {}.{}",
                        owner, l_name, l_name, inner.owner(), inner.name()
                    );
                }
            }
        }
        // ⭐**THE SITE PLAN** — see the block comment at the head of this file for what it
        // removes and why the answer is a constant.
        //
        // GUARD: the plan is keyed by the SITE alone, so it may only be consulted when the
        // dispatch owner IS the site's own interned owner. That holds for every
        // REF_invokeStatic (6) and REF_invokeSpecial (7) reference — the shapes javac emits for
        // a lambda body (`lambda$foo$0`), which is the bulk of this traffic — and for a kind
        // 5/9 method ref whose receiver's concrete class is the declared one. A virtual
        // reference dispatching on some OTHER class resolves against THAT class, is not a
        // constant of the site, and takes the cold arm below verbatim.
        //
        // `Arc::ptr_eq`, not a string compare: `site.owner` came from `Heap::intern_class` and
        // so did the receiver's `o.class`, so pointer identity is both the cheap test and the
        // exact one. (The arena receiver arm builds a FRESH `Arc` per apply and therefore never
        // matches — correctly: it is exactly the case the plan may not serve.)
        //
        // COLD ARM: a miss — and the whole of the kill-switched path — runs
        // `force_native_collection` and `resolve_method` exactly as before.
        let planned = Self::fast_indyfam_enabled() && Arc::ptr_eq(&owner_arc, &site.owner);
        let (resolved, midx) = if planned {
            let key = Arc::as_ptr(site) as usize;
            let plan = match apply_plan_get(key) {
                Some(p) => p,
                None => {
                    // A method ref bypasses the invokevirtual force-native check, so apply it
                    // here too (e.g. `idToValue::get` on a guava HashBiMap served natively).
                    // Folded into `decl: None` — both arms mean "the natives path" downstream.
                    let decl = if crate::opcodes::force_native_collection(self, owner, l_name, l_desc) {
                        None
                    } else {
                        self.resolve_method(owner, l_name, l_desc)?
                    };
                    // The slot inside `decl`, so `invoke_idx` and the flat-tier note below both
                    // index instead of looking the method up by name. `None` (fast-members off,
                    // or an illegal duplicate signature) simply reproduces the old call.
                    let midx = decl
                        .as_ref()
                        .and_then(|d| crate::tlcache::method_index(d, l_name, l_desc));
                    apply_plan_put(key, ApplyPlan { _site: Arc::clone(site), decl, midx })
                }
            };
            (plan.decl.clone(), plan.midx)
        } else {
            // A method ref bypasses the invokevirtual force-native check, so apply it
            // here too (e.g. `idToValue::get` on a guava HashBiMap served natively).
            let r = if crate::opcodes::force_native_collection(self, owner, l_name, l_desc) {
                None
            } else {
                self.resolve_method(owner, l_name, l_desc)?
            };
            (r, None)
        };
        if crate::env_flag!("RAVA_LAMBDA_TRACE") {
            eprintln!(
                "[LAM] kind={kind} owner={l_owner} name={l_name} desc={l_desc} resolved={}",
                resolved.is_some()
            );
        }
        // `flatcall` serves simple bodies before the normal first-entry compiler gate, so
        // request compilation once on this body's first resolved lambda use. The fixed policy
        // replaces the previous 512-call hotness trigger.
        #[cfg(feature = "next-gen-gc")]
        if crate::flatcall::lambda_compilation_enabled() {
            if let Some(decl) = resolved.as_ref() {
                // INDEXED WHEN THE PLAN NAMED THE SLOT. `ClassFile::method` hashes the name and
                // memcmps the descriptor across the overload set — 0.97% of the boot's callback
                // samples — and this site ran it on every apply to reach a method the plan had
                // already located. Identical `&MemberInfo` either way: `method_index` derives
                // the index from the OFFSET of the reference `ClassFile::method` returns.
                let m = match midx {
                    Some(i) => decl.file.methods.get(i as usize),
                    None => decl.file.method(l_name, l_desc),
                };
                if let Some(c) = m.and_then(|m| m.code.as_ref()) {
                    self.flat_compile_first_use(decl, l_name, l_desc, c);
                }
            }
        }
        let r = match resolved {
            // `invoke_idx` with the planned slot, not `invoke` with `None`: the third
            // name-keyed lookup of the same method on this apply, removed.
            Some(decl) => self.invoke_idx(&decl, midx, l_name, l_desc, call_args),
            None => {
                // Try the owner, then its superclass chain (Enum.ordinal, Object.*, ...).
                let mut handled = natives::try_invoke(self, owner, l_name, l_desc, &call_args)?;
                if handled.is_none() {
                    for cn in self.super_chain(owner).iter() {
                        if &**cn == owner {
                            continue;
                        }
                        handled = natives::try_invoke(self, cn, l_name, l_desc, &call_args)?;
                        if handled.is_some() {
                            break;
                        }
                    }
                }
                match handled {
                    Some(ret) => Ok(ret),
                    None => {
                        if self.trace.frontier {
                            eprintln!(
                                "[FRONTIER] lambda impl {}.{}{} (kind={})\n{}",
                                owner,
                                l_name,
                                l_desc,
                                kind,
                                self.backtrace()
                            );
                        }
                        // Unresolvable lambda/method-ref target at apply time is a
                        // linkage failure, not a VM abort. Per JVMS §5.4.3 the
                        // dynamic call site fails to link → throw a catchable
                        // LinkageError into the guest stack (same contract as
                        // d7e86a7's missing-native ULE) so callers that wrap the
                        // call (oshi's Memoizer suppliers, MC's SystemReport probe)
                        // can try/catch and degrade to a default backend instead of
                        // killing the process. Carries the impl signature.
                        let msg =
                            format!("lambda impl {}.{}{} (kind={})", owner, l_name, l_desc, kind);
                        Err(VmError::Throw(
                            self.make_exception("java/lang/UnsatisfiedLinkError", &msg),
                        ))
                    }
                }
            }
        };
        if self.trace.lambda {
            eprintln!(
                "[LAMBDA] kind={} {}.{}{} -> {:?}",
                kind,
                l_owner,
                l_name,
                l_desc,
                r.as_ref().map(|o| o.is_some())
            );
        }
        r
    }
}
