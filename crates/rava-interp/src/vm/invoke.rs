//! The interpreter invoke: resolve, admit, and enter, for every call kind.
//!
//! OVER THE 500-LINE RULE ON PURPOSE (578). This is the interpreter invoke: it runs on every
//! guest call that is not served by a fast path, and it is the same per-call path where
//! splitting invokevirtual was measured at +4.2% (see opcodes/invoke_virtual.rs). The blocks
//! that could come out are 40-60 lines each, so the split would add three call frames to that
//! path to save 140 lines. Not worth it on the evidence already collected.

use crate::*;

/// Could ANY of the four by-name native intercepts in `invoke_framed` fire for this method
/// body? Decided once per method and remembered; `false` — the answer for the overwhelming
/// majority of methods — collapses the remaining name/descriptor checks into one
/// branch on a register.
///
/// **This is not routing.** The eligibility predicates are unchanged and still evaluated in
/// their own order by their own blocks; what is cached is the answer to a question that has
/// exactly one answer per method body, keyed on the METHOD, not on a name pattern. The
/// predicate here is a deliberate SUPERSET — name and descriptor only, no class-name
/// conjuncts — so a `true` costs nothing but the original tests, and a `false` provably means
/// no block could have matched.
///
/// KEYED ON THE `Code` POINTER, which is this engine's established method identity (the same
/// key `interp_who`, `backedge`, `aot_link`, `fork_probe` and `flat_compile_first_use` use) and is
/// stable for the process: the loader's class cache is never cleared, so a `LoadedClass` and
/// the `Code` inside it outlive every frame that can reach them. For a given `Code` the name
/// and descriptor are fixed — the method was resolved BY them — so the cached answer cannot
/// go stale.
///
/// TABLE SIZE: 128 entries x 16 bytes = **2,048 bytes per thread**, four entries to a cache
/// line, one line touched per probe. Deliberately under the 2.5 KB that measured neutral here
/// — the 14 KB version of a different per-name cache measured **+6.8% WORSE**, and a cache
/// that stops fitting in L1 costs more than the work it removes. A collision is not a
/// correctness question, only a recompute: the slot stores its key and is re-derived on a
/// miss.
#[cfg(feature = "next-gen-gc")]
fn may_intercept(code: &rava_classfile::Code, name: &str, descriptor: &str) -> bool {
    const N: usize = 128;
    std::thread_local! {
        static MEMO: std::cell::UnsafeCell<[(usize, bool); N]> =
            const { std::cell::UnsafeCell::new([(0usize, false); N]) };
    }
    if !crate::vmframe::fast_frame() {
        // Kill switch: every block re-tests everything, exactly as before.
        return true;
    }
    let key = code as *const rava_classfile::Code as usize;
    // `Code` is heap-allocated inside the class file, so the low bits carry no entropy;
    // shifting past the alignment is what makes a direct-mapped table spread at all.
    let slot = (key >> 4) & (N - 1);
    MEMO.with(|m| {
        // SAFETY: a per-thread cell, and the borrow ends before anything else runs — the only
        // code between the read and the write is `str` comparison, which cannot re-enter this
        // function. Same argument (and same shape) as `frames::pool_take`'s `vec_pool`.
        let t = unsafe { &mut *m.get() };
        if t[slot].0 == key {
            return t[slot].1;
        }
        let hit = name == "wrap"
            || (name == "findIntervalStart" && descriptor == "([FF)I")
            || (name == "iterator" && descriptor.starts_with("()L"))
            || (name == "size" && descriptor == "()I")
            || (name == "getInt" && descriptor == "(I)I")
            || (name == "get"
                && (descriptor == "(I)Ljava/lang/Integer;"
                    || descriptor == "(I)Ljava/lang/Object;"));
        t[slot] = (key, hit);
        hit
    })
}

/// ── THE INTERPRETED CALL'S PROCESS CONSTANTS, RESOLVED ONCE PER THREAD ───────────────────
///
/// ⭐**NINE `OnceLock` ACQUIRE LOADS AND TWO GLOBAL RELAXED LOADS SAT ON EVERY ONE OF THE
/// 14.1 M INTERPRETED METHOD BODIES A BOOT.** [`Vm::invoke_framed`] is a single straight-line
/// function and it consulted, in order: `vecgrow::enabled` (the widen/unbox event),
/// `shadow::enabled`, `bootdisp::on`, `codec_flatten::enabled`, `aot_clif_enabled`,
/// `llvm_aot_enabled`, `alloc_sites::regions_armed` (two relaxed loads), `interp_who::enabled`
/// and `scratch::tostring_probe` **twice** — once for the toString-caller trace and again in the
/// `trace` disjunction eight lines later. Every one of them answers a question fixed before the
/// first guest opcode ran, because every one derives from an environment variable read once and
/// never written again.
///
/// **The cost is not the `mov`, it is the BARRIER.** `*OnceLock::get_or_init(..)` reaches its
/// value through an acquire load; on x86 that lowers to a plain `mov`, but as an ATOMIC it is an
/// optimisation barrier — LLVM may not hoist it out of this function's inline tree, may not fold
/// the two `tostring_probe()` reads of one call into one, and must assume memory it can otherwise
/// track is clobbered across it. This tree has priced exactly that shape: ledger row 405 took ONE
/// relaxed atomic off the operand-push path and measured **−15%**, recording the reason as *"an
/// atomic's cost is that LLVM can't HOIST it, not the `mov`"*.
///
/// The word is a `Cell<u16>` with a `const` initialiser and no destructor — the one combination
/// `std` lowers to a bare `#[thread_local]` slot (on windows-msvc: the `_tls_index` load, the
/// `gs:[0x58]` TEB read and a `movzx`), with no `LocalKey::with` call, no lazy-init state check
/// and no destructor registration. Deliberately the same shape as `vmframe::FRAME_GATES`,
/// `opcodes::gates::OP_GATES` and `frames::POOL_GATES`.
///
/// ⚠**NO GATE MOVED AND NO INITIALISER RAN EARLIER.** Every bit is read at the same site, in the
/// same order, guarding the same branch; only the LOAD changed. The one initialiser in the set
/// with a side effect — `vecgrow::enabled` arms a watchdog thread — is already resolved before
/// this word's cold path can run, because `invoke_idx` calls `opcodes::invoke_hooks_armed()` (of
/// which `vecgrow::enabled` is an input) on the line above the only call to `invoke_framed`.
///
/// ⛔**WHAT IS DELIBERATELY LEFT OUT.** `vm::probes::enter` already collapses its six probes into
/// one `any_armed()` read, and that read is private to `vm/probes.rs`, so mirroring it here would
/// mean a second copy of a gate the two files could then disagree about. `vmframe::fast_frame`
/// (read twice per call, here and in [`may_intercept`]) is ALREADY a plain per-thread byte off
/// `FRAME_GATES` and there is nothing left to take. The four remaining by-name
/// native intercepts read their gates behind `mi`, a register test false for
/// almost every method.
///
/// Bit 0 — this thread has resolved. A word of 0 therefore means "not yet", which is what makes
/// the fast read a single compare against zero.
const IG_RESOLVED: u16 = 1 << 0;
/// Bit 1 — the kill switch is engaged: distrust every other bit and call the `OnceLock`s from the
/// sites that used to call them, as the historic code did.
const IG_LIVE: u16 = 1 << 1;
/// Bit 2 — `scratch::tostring_probe` (`RAVA_TOSTRING_STACK_PROBE`), read at two sites.
const IG_TOSTRING: u16 = 1 << 2;
/// Bit 3 — `bootdisp::on` (`RAVA_PROFILE_BOOT_DISPATCH`).
const IG_BOOTDISP: u16 = 1 << 3;
/// Bit 4 — `codec_flatten::enabled` (`RAVA_M4_CODEC_FLATTEN`).
const IG_CODEC: u16 = 1 << 4;
/// Bit 5 — `interp_who::enabled` (`RAVA_INTERP_WHO`).
const IG_INTERP_WHO: u16 = 1 << 5;
/// Bit 6 — `vecgrow::enabled` (`RAVA_VECGROW_STAT`).
const IG_VECGROW: u16 = 1 << 6;
/// Bit 7 — an `alloc_sites` region is armed, so the interpreted-region guard has work to do.
///
/// ⚠**LATE ARMING IS THE ONE HAZARD, AND IT IS COVERED BY CONSTRUCTION** — the same argument
/// `vm::jit_call_body::seam_slow_gates` makes for the same two cells. `regions_armed` reads
/// `RAVA_ALLOC_SIZES`, stored from `main` before any guest code runs, and a region-time flag set
/// by `pc_profile::start_sampler_if_on`, which the interpreter calls lazily but only ever `if
/// pc_profile::enabled()`. Folding that process constant into the bit makes the late store
/// unreachable while the bit is false.
const IG_REGION: u16 = 1 << 7;
/// Bit 8 — `shadow::enabled` (`RAVA_SHADOW_DECODE`).
#[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
const IG_SHADOW: u16 = 1 << 8;
/// Bit 9 — either AOT tier is armed (`RAVA_AOT_CLIF` / `RAVA_LLVM_AOT`). One bit for the pair
/// because the site tests their disjunction; the `||` short-circuit is preserved verbatim in
/// [`aot_any_enabled`], so `llvm_aot_enabled`'s `clang` probe still never runs when the Cranelift
/// tier is on.
#[cfg_attr(not(feature = "next-gen-gc"), allow(dead_code))]
const IG_AOT: u16 = 1 << 9;

thread_local! {
    /// This thread's resolved invoke gates; see the block comment above.
    static INVOKE_GATES: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
}

/// The word. One plain per-thread load; never 0 on return.
#[inline(always)]
fn invoke_gates() -> u16 {
    let g = INVOKE_GATES.get();
    if g != 0 { g } else { invoke_gates_cold() }
}

/// First touch on this thread: read the process constants through their `OnceLock`s — the ONLY
/// place that still happens under the default — and cache the answers. Once per thread, so the
/// environment lookups behind them stay exactly as many as they were.
#[cold]
#[inline(never)]
fn invoke_gates_cold() -> u16 {
    static LIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let live = *LIVE.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_SEAM3").is_some() || crate::flags::emit_more_disabled()
    });
    let mut g = IG_RESOLVED;
    if live {
        // Resolve NOTHING else: with the switch engaged each site must reach its own `OnceLock`
        // from its own position, in its own order, exactly as before.
        g |= IG_LIVE;
        INVOKE_GATES.set(g);
        return g;
    }
    if tostring_probe() {
        g |= IG_TOSTRING;
    }
    if crate::bootdisp::on() {
        g |= IG_BOOTDISP;
    }
    if crate::codec_flatten::enabled() {
        g |= IG_CODEC;
    }
    if crate::interp_who::enabled() {
        g |= IG_INTERP_WHO;
    }
    if crate::vecgrow::enabled() {
        g |= IG_VECGROW;
    }
    if crate::alloc_sites::regions_armed() || crate::pc_profile::enabled() {
        g |= IG_REGION;
    }
    #[cfg(feature = "next-gen-gc")]
    {
        if crate::shadow::enabled() {
            g |= IG_SHADOW;
        }
        if aot_any_enabled() {
            g |= IG_AOT;
        }
    }
    INVOKE_GATES.set(g);
    g
}

/// Read one bit of an ALREADY-LOADED word. `live` is the historic `OnceLock` reader, called only
/// under the kill switch — the `opcodes::gates::op_gate_of` discipline, so a function that needs
/// nine gates takes ONE load rather than nine acquire loads.
#[inline(always)]
fn ig_bit(g: u16, bit: u16, live: fn() -> bool) -> bool {
    if g & IG_LIVE != 0 { live() } else { g & bit != 0 }
}

/// The AOT tier test, as one function so the word can hold it as one bit and the kill switch can
/// still evaluate the ORIGINAL expression — including its short-circuit, which is load-bearing:
/// `llvm_aot_enabled` probes for a `clang` toolchain, and that probe must not run when the
/// Cranelift AOT tier has already answered the question.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn aot_any_enabled() -> bool {
    aot_clif_enabled() || llvm_aot_enabled()
}

impl Vm {
    pub(crate) fn invoke(
        &mut self,
        class: Arc<LoadedClass>,
        name: &str,
        descriptor: &str,
        args: Vec<Value>,
    ) -> Result<Option<Value>, VmError> {
        self.invoke_idx(&class, None, name, descriptor, args)
    }

    /// Request compilation on the first resolved lambda-body use. `flatcall` can serve this body
    /// before it reaches the ordinary first-entry JIT gate, so it needs the same fixed policy.
    /// Memoization is keyed by `Code` identity, never by invocation count or observed hotness.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn flat_compile_first_use(
        &mut self,
        class: &Arc<LoadedClass>,
        name: &str,
        descriptor: &str,
        code: &rava_classfile::Code,
    ) {
        // A worker's JIT module is thread-owned and cannot publish a lasting function pointer.
        // Leave the request unrecorded so the coordinator can apply the same first-use policy.
        if crate::gil::is_worker() {
            return;
        }
        let key = code as *const _ as usize;
        std::thread_local! {
            static REQUESTED: std::cell::RefCell<crate::FastMap<usize, ()>> =
                std::cell::RefCell::new(crate::FastMap::default());
        }
        let first_use = REQUESTED.with(|requests| {
            let mut requests = requests.borrow_mut();
            first_flat_compile_request(&mut requests, key)
        });
        if first_use {
            let cls = class.file.this_class.to_string();
            let _hr_wait = crate::background_jit::hr_wait::DeferScope::enter(
                crate::background_jit::hr_wait::WaitCaller::FlatFirstUse,
            );
            let _ = self.force_compile(&cls, name, descriptor);
        }
    }

    /// `invoke`, with the callee's index in `class.methods` when the caller already knows it.
    ///
    /// An interpreted call site resolves to the same method every time, and the index is the only
    /// thing `ClassFile::method` computes — by hashing the name and memcmp-ing descriptors across
    /// the overload set, on every call. A site that carries the index skips that entirely.
    /// ⚠**TAKES THE CLASS BY REFERENCE, NOT BY VALUE.** It used to take an owned
    /// `Arc<LoadedClass>` while every use inside was a borrow except one — the
    /// `Arc::clone` that `Frame::new` performs for itself. So the owned parameter bought
    /// nothing and charged every caller a refcount increment and a decrement, on a path
    /// row 395 prices at 105 ns total with an `Arc` clone/drop pair measured at 12 ns.
    /// A frame-free call now performs no refcount traffic at all.

    pub(crate) fn invoke_idx(
        &mut self,
        class: &Arc<LoadedClass>,
        midx: Option<u32>,
        name: &str,
        descriptor: &str,
        args: Vec<Value>,
    ) -> Result<Option<Value>, VmError> {
        // 20.3 wall-profile: the invoke machinery (method resolve, IC/vtable, tier decision,
        // marshalling). The inner tier (interp/jit) and native bridges push their own category and
        // override this for their duration, so VM_DISPATCH ends up as the pure dispatch/resolve tax.
        let _wp = crate::rava_sampler::enter(crate::rava_sampler::VM_DISPATCH);
        // ONE branch for the five diagnostic hooks below — see `invoke_hooks_armed`.
        let hooks = crate::opcodes::invoke_hooks_armed();
        if hooks {
            // Counted at BOTH entry points (see `callcensus`): a compiled method never reaches
            // here, so this arm alone would under-count exactly the methods that matter most.
            if crate::callcensus::enabled() {
                crate::callcensus::note(&class.file.this_class, name);
            }
            crate::vecgrow::event(0);
            trace_call_hook(&class.file.this_class, name, descriptor, &args, false);
        }
        let method = match midx {
            Some(i) => class.file.methods.get(i as usize).ok_or_else(|| {
                VmError::MethodNotFound(format!("{}.{name}{descriptor}", class.file.this_class))
            })?,
            None => class.file.method(name, descriptor).ok_or_else(|| {
                VmError::MethodNotFound(format!("{}.{name}{descriptor}", class.file.this_class))
            })?,
        };
        let is_static = method.access_flags & 0x0008 != 0;
        let code = method
            .code
            .as_ref()
            .ok_or_else(|| VmError::NoCode(format!("{name}{descriptor}")))?;
        // `RAVA_SHAPE_CENSUS` — how many interpreted calls have a TRIVIAL body?
        //
        // ⭐**Ledger row 344** profiles the interpreter gate at **39.3% call machinery** and shows
        // its parts are each already optimised, so the remaining move is FEWER FRAMES rather than
        // cheaper ones. The cheapest frame to remove is the one a trivial accessor does not need:
        // `aload_0; getfield idx; ?return` is five bytes and cannot throw except on a null
        // receiver, yet it pays `Frame::new` (two pooled `Vec`s, a `max_locals` resize, a
        // `max_stack` reserve), an `Arc<LoadedClass>` clone, a registry push/pop and a `Frame`
        // drop. **This counts the population before anyone builds the machinery for it** — the
        // rule this ledger keeps restating after pricing mechanisms that had none.
        #[cfg(feature = "next-gen-gc")]
        if hooks && crate::shape_census::enabled() {
            crate::shape_census::note(&code.bytecode);
        }
        // `RAVA_NOISE_CALLER` — WHO ASKS FOR A NOISE SAMPLE? (ledger row 367)
        //
        // The JVM's own `-Xint` boot profile contains essentially no noise frames, so every one of
        // our 251 M `ImprovedNoise#p` entries is work the JVM never does. The call census named the
        // callee chain; only the CALLER chain can name what forces it. Capped at the configured
        // count and gated off by default — one env lookup, memoised.
        if hooks {
            let cap = crate::scratch::noise_caller_probe();
            if cap > 0 && name == "getValue" && class.file.this_class.ends_with("synth/NormalNoise")
            {
                use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
                static N: AtomicUsize = AtomicUsize::new(0);
                let n = N.fetch_add(1, Relaxed);
                if n < cap {
                    eprintln!("[NOISE-CALLER #{n}]\n{}", self.backtrace());
                }
            }
        }
        // FRAME-FREE TRIVIAL CALLEE (`flatcall`, default ON, opt-out `RAVA_NO_FLATCALL`).
        //
        // Row 360 counts 8.7% of interpreted calls as a bare getter or a bare constant — bodies
        // whose entire effect is one pure read, which still pay `Frame::new` (a pooled `Vec` take,
        // a `max_locals` resize, a `max_stack` reserve), an `Arc<LoadedClass>` clone, a depth
        // increment, the `opcodes::run` prologue and dispatch loop, and a `Frame` drop with two
        // pool gives. Served here they never become a frame at all.
        //
        // ⚠**HERE AND NOT IN THE TRAMPOLINE DRIVER.** The first version of this hook sat in
        // `drive_stack`'s `RunExit::Call` arm — which looked like the one place every interpreted
        // call passes through, and is reached only under `RAVA_STACKFUL`. `RAVA_FLAT_STAT` read
        // **all four counters zero** on a full gate and named the mistake in one run. `invoke_idx`
        // is the default machine's call path; the trampoline is opt-in and is left alone.
        //
        // Placed BEFORE the by-name native intercepts below on purpose: those target methods with
        // real bodies (`unpack`, `wrap`, `findIntervalStart`, `iterator`), none of which can
        // classify as a five-byte getter or a two-byte constant, so the order is unobservable.
        //
        // A refusal is invisible — `try_run` performs no store, no allocation and no mutation of
        // `args` — so everything below runs exactly as it did before.
        // ⛔⭐**A TRIVIAL BODY MUST STILL TIER WHEN ITS SITE CANNOT BE INLINED.**
        // Lambda dispatch and direct comparator/key calls request it before entering this path.
        // (Original analysis kept below.)
        //
        // ⛔⭐**A TRIVIAL BODY MUST STILL TIER, or the flat path costs 111x what it saves.**
        //
        // `flatcall` returns before `jit_run`, so a body it serves never reaches the tiering
        // counter and is never compiled — and a callee that is never compiled can never receive
        // a DIRECT entry, so every call from machine code falls all the way back through
        // `jit_call_body` -> synthetic frame -> `invokeinterface`. Measured on
        // `bench/CallShapes.java`: a lambda whose body is `x -> x` (flat-servable) costs
        // **999 ns**; the identical call through a normal class costs **9**, and a lambda whose
        // body has a branch (so flatcall refuses it, so it tiers, so it compiles) costs 134.
        // `RAVA_JIT_PIC_PROBE` names it exactly: `callee not compiled yet (tier lag)` for
        // **218,813 of 218,813** direct-resolve attempts at that site.
        //
        // `invoke_lambda_parts` may request fixed first-use compilation only behind the
        // `RAVA_FLAT_TIER=1` opt-in. With it disabled, this frame-free interpreter path stays
        // unchanged and no lambda-body compile request is made.
        if let Some(v) = crate::flatcall::try_run(self, class, code, descriptor, &args) {
            // Nothing to restore: this hook runs before the trampoline flag is touched, and a
            // body with no call in it cannot stage one.
            self.give_vec(args);
            return Ok(v);
        }

        // ⭐**THE MONOLITH IS BEHIND A CALL NOW.** Everything below this point — the four
        // by-name native intercepts, the probes, the depth check, the shadow/codec hooks, the
        // AOT and JIT entries, `Frame::new` and the interpreter loop, and the error formatting
        // with its `format!`s — used to be the rest of THIS function. A function reserves its
        // whole frame in the PROLOGUE, so a call served frame-free above still paid for all of
        // it: the stack adjustment, and the callee-saved registers this body's register
        // pressure forces to be spilled. Row 395 prices that whole served call at 105 ns
        // against the JVM interpreter's 7, with no frame built and no arguments to marshal.
        self.invoke_framed(class, name, descriptor, args, code, is_static)
    }

    /// The rest of an interpreted call: everything that is NOT served frame-free. Split out of
    /// `invoke_idx` so the fast path does not carry its stack frame — see the note at the call.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn invoke_framed(
        &mut self,
        class: &Arc<LoadedClass>,
        name: &str,
        descriptor: &str,
        args: Vec<Value>,
        code: &rava_classfile::Code,
        is_static: bool,
    ) -> Result<Option<Value>, VmError> {
        // ⛔⭐**THE COVERAGE PAIR USED TO BE COUNTED HERE, AND IT WAS WRONG.**
        //
        // `exec_interpreted` was incremented at THIS point — the top of `invoke_framed` — while
        // the compiled tiers are attempted several hundred lines BELOW, still inside this same
        // function. So every invocation that went on to run as machine code was counted as
        // interpreted as well, and the two halves of the "coverage pair" overlapped instead of
        // partitioning. Every statement of the form "N% of guest invocations are interpreted"
        // derived from it was inflated by exactly the compiled population.
        //
        // It is now counted where the comment "Fell through every compiled tier" is, which is
        // the first point at which the claim is true. `interp_has_handlers` /
        // `interp_no_handlers` and the `RAVA_INTERP_WHO` census moved with it, for the same
        // reason: the census's top rows were all marked COMPILED, which read as "machine code
        // exists and the interpreter ran anyway" and actually meant "this row is not an
        // interpreted entry at all".
        // ⭐**EVERY PROCESS CONSTANT THIS FUNCTION READS, IN ONE PLAIN PER-THREAD LOAD.** See the
        // block comment on [`IG_RESOLVED`] for what was there before and why an acquire load is
        // worth more than its `mov`. Read once at the natural entry, then each site below tests a
        // bit of a word already in a register.
        let ig = invoke_gates();
        // ⭐**THE FOUR BY-NAME INTERCEPTS BELOW ASK THE SAME QUESTION OF EVERY CALL, AND THE
        // ANSWER IS A PROPERTY OF THE METHOD.** Together they perform several name/descriptor checks
        // and two unconditional `OnceLock` reads on a path taken 14.1 M times per boot.
        // Almost every method never matches; its name and descriptor do not change.
        // `may_intercept` decides it once per method body, then each block short-circuits on a register.
        //
        // Semantics are untouched: `mi` is a strict superset of the remaining method-shape predicates
        // (it drops each class-name conjunct, keeping only name+descriptor), so a block that
        // could fire still evaluates its own full test, verbatim, in its own order.
        #[cfg(feature = "next-gen-gc")]
        let mi = may_intercept(code, name, descriptor);
        // Stage 3 / 14.4: native DoubleArrayList.wrap([D)/([DI) — build the list (a=array ref, size)
        // directly, skipping the guest ctor chain wrap→wrap→<init>([DZ)→AbstractDoubleList.<init>→
        // AbstractDoubleCollection.<init> (~1.9M interp entries on boot, the #1 stuck cluster). wrap is
        // NO-COPY (this.a = a), so bit-exact. Default-on; RAVA_NO_DAL_WRAP disables.
        #[cfg(feature = "next-gen-gc")]
        if mi
            && name == "wrap"
            && class.file.this_class == "it/unimi/dsi/fastutil/doubles/DoubleArrayList"
            && crate::dal_wrap_enabled()
        {
            if let Some(v) = self.dal_wrap_native(descriptor, &args) {
                return Ok(Some(v));
            }
        }
        // 19.2 M4: native CubicSpline interval search. Do the binary lower-bound over the float[]
        // locations directly (bit-exact vs Mth.binarySearch - 1), eliminating the ~1.45M interpreted
        // predicate-lambda calls. Paired with the jit_run compile-deny (spline_native_enabled) so the
        // compiled `apply` routes findIntervalStart here instead of a cached compiled fptr.
        #[cfg(feature = "next-gen-gc")]
        if mi
            && crate::spline_native_enabled()
            && name == "findIntervalStart"
            && descriptor == "([FF)I"
            && class.file.this_class == "net/minecraft/util/CubicSpline$Multipoint"
        {
            if let Some(idx) = self.spline_find_interval_start(&args) {
                return Ok(Some(Value::Int(idx)));
            }
        }
        // Stage 3: native fastutil double-list iterator()/size() — kill the DoubleArrayList$1
        // iterator swamp (hasNext/nextDouble/$1.<init>/listIterator(I)/ensureIndex ~470k interp
        // entries, JIT==0) and the size()I read (~83k). iterator() is the forward-only Iterator
        // contract → returning the native $elems/$pos iterator (served by iter_op) is bit-exact;
        // listIterator() (mutation-capable) is NOT touched. Receiver-class gated so it fires on
        // AbstractDoubleList.iterator() called on a DoubleArrayList/DoubleImmutableList receiver.
        // Default-on; RAVA_NO_DAL_ITER disables. Non-matching receivers fall straight through.
        #[cfg(feature = "next-gen-gc")]
        if mi && crate::dal_iter_enabled() {
            if name == "iterator" && descriptor.starts_with("()L") {
                if let Some(recv) = args.first() {
                    if let Some(v) = self.dal_iterator_native(recv) {
                        return Ok(Some(v));
                    }
                }
            } else if name == "size"
                && descriptor == "()I"
                && class.file.this_class == "it/unimi/dsi/fastutil/doubles/DoubleArrayList"
            {
                if let Some(recv) = args.first() {
                    if let Some((_, len)) = self.fu_double_view(recv, true) {
                        return Ok(Some(Value::Int(len as i32)));
                    }
                }
            }
        }
        // Native fastutil int-list accessors — the #2 boot interpreter cluster (every boot incl. the
        // server reload/bootstrap): getInt(I)I (~109k) AND the boxing wrappers get(I)Ljava/lang/Integer;
        // / get(I)Ljava/lang/Object; (~109k each). All are a pure int-array read off the backing `a`
        // (symmetric to the DoubleArrayList intrinsics). getInt returns the primitive; get(I) returns a
        // boxed Integer — but rava's Integer.valueOf is an identity no-op (a boxed Integer IS Value::Int,
        // autobox-transparent), so BOTH return the same allocation-free Value::Int, bit-exact vs the guest
        // chain (Integer.valueOf(getInt(i))). Receiver-class gated (IntArrayList uses its `size`,
        // ImmutableList the whole array); OOB falls through to bytecode (real method throws IOOBE).
        // getInt: default-on RAVA_NO_IL_GET; get(I) boxing: default-on RAVA_NO_IL_BOXING.
        #[cfg(feature = "next-gen-gc")]
        {
            let il_hit = mi
                && ((name == "getInt" && descriptor == "(I)I" && crate::il_get_enabled())
                    || (name == "get"
                        && (descriptor == "(I)Ljava/lang/Integer;"
                            || descriptor == "(I)Ljava/lang/Object;")
                        && crate::il_boxing_enabled()));
            if il_hit {
                if let Some(recv) = args.first() {
                    let is_al = match recv {
                        Value::Ref(h) => match self.heap.class_of(*h) {
                            Some("it/unimi/dsi/fastutil/ints/IntArrayList") => Some(true),
                            Some("it/unimi/dsi/fastutil/ints/IntImmutableList") => Some(false),
                            _ => None,
                        },
                        Value::Obj(p) => {
                            match crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                                .as_deref()
                            {
                                Some("it/unimi/dsi/fastutil/ints/IntArrayList") => Some(true),
                                Some("it/unimi/dsi/fastutil/ints/IntImmutableList") => Some(false),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    if let Some(is_al) = is_al {
                        let idx = args.get(1).and_then(Value::as_int);
                        if let (Some((h, len)), Some(i)) = (self.fu_int_view(recv, is_al), idx) {
                            if i >= 0 && (i as usize) < len {
                                let v = self
                                    .heap
                                    .array_get(h, i as usize)
                                    .and_then(|x| x.as_int())
                                    .unwrap_or(0);
                                return Ok(Some(Value::Int(v)));
                            }
                        }
                    }
                }
            }
        }
        // M3 9.1 worldgen profile probe (read-only, gated): inclusive/self timing of top-level OOP
        // entries. The guard's Drop fires on every return path (cnp/aot/jit/interp), so exit always
        // pairs with enter. Off ⇒ the guard is inert.
        // The five M3 isolation probes, behind one armed check — see `vm::probes`.
        let _probes = crate::vm::probes::enter(self, class, name, code, &args);
        if frames::depth() >= self.max_depth {
            // The depth cap IS this engine's stack limit, so exhausting it is the
            // guest-visible event HotSpot calls StackOverflowError — a catchable
            // Throwable, not a VM shutdown. `UnsupportedNative` killed the process
            // instead, so `try { deepRecursion(); } catch (StackOverflowError e)`
            // (the standard shape for depth-probing and for parser recursion
            // guards) never ran its handler. Unwinding as `Throw` restores it:
            // every intervening `invoke` returns through the `frames::depth_dec()`
            // below, so the handler runs at ITS OWN depth with room to work,
            // exactly as it does after a real stack unwind.
            //
            // The frame list stays a TRACE: HotSpot prints nothing for a caught
            // overflow, and an unconditional line here is guest-visible stderr.
            if self.trace.throw {
                let tail: Vec<String> = crate::vm::thread_context::with_call_stack(
                    &self.call_stack,
                    |call_stack| {
                        call_stack
                            .iter()
                            .rev()
                            .take(12)
                            .map(|(c, m)| format!("{c}.{m}"))
                            .collect()
                    },
                );
                eprintln!(
                    "[STACKOVERFLOW] {} frames; top:\n  {}",
                    frames::depth(),
                    tail.join("\n  ")
                );
            }
            return Err(VmError::Throw(
                self.make_exception("java/lang/StackOverflowError", ""),
            ));
        }
        // Expand long/double parameters to two local slots (JVMS 2.6.1) so the
        // bytecode's local indices line up with the arguments.
        // `event` re-tests its own gate; the bit in front of it means the acquire load behind
        // that test is reached only on a run where the census is actually armed.
        if ig_bit(ig, IG_VECGROW, crate::vecgrow::enabled) {
            crate::vecgrow::event(1);
        }
        let mut args = args;
        // THE COMMON CASE NEEDS NO SECOND BUFFER. `expand_locals_into` exists to widen category-2
        // parameters into two local slots (JVMS 2.6.1) and to unbox boolean parameters; a
        // descriptor with neither — the majority — makes it a straight move of receiver+args from
        // one pooled Vec into another. That is a pool take, a pool give, and n 24-byte moves per
        // call to produce a buffer identical to the one already in hand.
        //
        // The layout is memoised (`arg_layout`), so the test is two integer compares.
        let mut locals = match arg_layout(descriptor) {
            Some(l) if l.wide == 0 && l.unbox == 0 => args,
            _ => {
                let mut out = self.take_vec();
                expand_locals_into(&mut out, is_static, descriptor, &mut args, &self.heap);
                self.give_vec(args);
                out
            }
        };
        #[cfg(feature = "next-gen-gc")]
        let shadow_args = if ig_bit(ig, IG_SHADOW, crate::shadow::enabled)
            && crate::codec_flatten::is_decode(&class.file.this_class, name)
        {
            Some(locals.clone())
        } else {
            None
        };
        #[cfg(feature = "next-gen-gc")]
        let run_shadow_hook = |vm: &mut Self, res: &Result<Option<Value>, VmError>| {
            if let Some(args) = &shadow_args {
                crate::shadow::shadow_decode(vm, &class.file.this_class, args, res);
            }
        };
        // JIT fast path: hot numeric methods run as native code. In verify mode
        // we only double-run methods without callbacks/side effects.
        let verify_unsafe = self.jit_enabled && self.jit_verify && jit::has_verify_unsafe_ops(code);
        if self.jit_enabled && verify_unsafe && self.trace.jit_stats {
            let key = format!("{}.{}{}", class.file.this_class, name, descriptor);
            self.jit_stats.entry(key).or_default().verify_skipped += 1;
        }
        // `invoke` runs a method to completion on the Rust stack, so it must NOT
        // trampoline — and that covers the COMPILED tiers too: cnp_run/jit_run execute
        // native code whose runtime callbacks (rava_invoke → jit_call → the synthetic
        // dispatch) would otherwise see `trampoline=true` and STAGE the callee as
        // pending instead of running it — the callback then returns 0/none while the
        // callee executes later, detached (found as a compiled MappedRegistry.register
        // whose holder.bindValue never ran under RAVA_STACKFUL → every registry holder
        // unbound → 'unbound value ...creative_mode_tab...'). Clear the flag for the
        // WHOLE invoke — compiled tiers AND the interp run — restoring on every exit.
        let saved_trampoline = self.trampoline;
        self.trampoline = false;
        // 14.1 boot-dispatch inspector: build the method key once (only when gated on), then tag the
        // tier that actually ran it. Side-effect-only ⇒ math inert.
        let bd_key = if ig_bit(ig, IG_BOOTDISP, crate::bootdisp::on) {
            Some(format!("{}.{}{}", class.file.this_class, name, descriptor))
        } else {
            None
        };
        // 19.3 release seal: the 18.1/18.3 region inspector + 19.1 kernel-scan per-invoke hooks are
        // unwired here so prod pays zero per-invoke cost (the gated modules stay dormant for re-wiring).
        // 21.1 codec-flatten recon (RAVA_M4_CODEC_FLATTEN, inert): count combinator ctors + measure the
        // runtime decode-tree depth. The decode RAII guard stays live across this call's nested decodes.
        let _codec_decode_guard = if ig_bit(ig, IG_CODEC, crate::codec_flatten::enabled) {
            if name == "<init>" && crate::codec_flatten::is_ctor_target(&class.file.this_class) {
                crate::codec_flatten::note_ctor(&class.file.this_class);
            }
            // 21.2: build the native AST for a field-codec from its ctor args (name, elementCodec).
            // Instance <init>: args = [receiver, name, elementCodec]. The args are fully-built here
            // (the receiver's own fields are not yet set — we use the args, not the receiver).
            #[cfg(feature = "next-gen-gc")]
            if name == "<init>"
                && class.file.this_class == "com/mojang/serialization/codecs/FieldDecoder"
            {
                // Use `locals` (args were already expanded+moved into it above): [receiver, name, inner].
                if let (Some(recv), Some(name_v), Some(inner)) =
                    (locals.first().cloned(), locals.get(1).cloned(), locals.get(2).cloned())
                {
                    let fname = self.value_to_string(&name_v).unwrap_or_default();
                    let sub = self.build_codec_ast(&inner, 1);
                    let node = crate::codec_flatten::CodecNode::Field(fname, Box::new(sub));
                    let addr = match &recv {
                        Value::Ref(h) => *h as u64,
                        Value::Obj(p) => p.addr(),
                        _ => 0,
                    };
                    crate::codec_flatten::register_ast(addr, node);
                }
            }
            if crate::codec_flatten::is_decode(&class.file.this_class, name) {
                Some(crate::codec_flatten::enter_decode())
            } else {
                None
            }
        } else {
            None
        };
        // 24.2b: the RecordAst observer moved to jit_call (the compiled→runtime choke) — the vm.invoke
        // point missed the boot's compiled decode traffic (23.x machcode PIC bypasses vm.invoke).
        // CNP tier-0: a structurally supported method runs as copy-and-patch
        // machine code. Unsupported bytecode falls through unchanged.
        #[cfg(all(windows, target_arch = "x86_64"))]
        if self.cnp_enabled && !self.jit_verify {
            let _wp = crate::rava_sampler::enter(crate::rava_sampler::JIT_EXEC);
            if let Some(v) = self.cnp_run(code, descriptor, is_static, &locals) {
                if let Some(k) = &bd_key { crate::bootdisp::record(k, true); }
                self.give_vec(locals);
                self.trampoline = saved_trampoline;
                crate::vm::aot_route_diag::jit_entry();
                return Ok(Some(v));
            }
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        if self.cnp_enabled && !self.jit_verify {
            let _wp = crate::rava_sampler::enter(crate::rava_sampler::JIT_EXEC);
            if let Some(result) = self.cnp_run(class, code, descriptor, is_static, &locals) {
                if let Some(k) = &bd_key { crate::bootdisp::record(k, true); }
                self.give_vec(locals);
                self.trampoline = saved_trampoline;
                crate::vm::aot_route_diag::jit_entry();
                return result.map(Some);
            }
        }
        // A2 AOT run-path lane (RAVA_AOT_CLIF): execute a covered method as AOT machine
        // code from call #1, before the Cranelift JIT. Uncovered → falls through.
        // The AOT run lane is supported on x86_64 Windows/Linux; elsewhere methods fall through
        // to the Cranelift JIT exactly as uncovered methods do here.
        // Explicit offline objects do not require runtime JIT compilation to be enabled.
        #[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
        if (self.jit_enabled || crate::aot_clif::prepared::enabled())
            && !self.jit_verify && ig_bit(ig, IG_AOT, aot_any_enabled) {
            let _wp = crate::rava_sampler::enter(crate::rava_sampler::JIT_EXEC);
            if let Some(r) = self.aot_run(code, name, descriptor, is_static, class, &locals) {
                if let Some(k) = &bd_key { crate::bootdisp::record(k, true); }
                #[cfg(feature = "next-gen-gc")]
                run_shadow_hook(self, &r);
                self.give_vec(locals);
                self.trampoline = saved_trampoline;
                return r;
            } else {
                crate::vm::aot_route_diag::aot_declined();
            }
        }
        let jit_out = if self.jit_enabled && !verify_unsafe {
            let _wp = crate::rava_sampler::enter(crate::rava_sampler::JIT_EXEC);
            self.jit_run(code, name, descriptor, is_static, class, &locals)
        } else {
            None
        };
        let mut verify_out: Option<Result<Option<Value>, VmError>> = None;
        if let Some(r) = jit_out {
            if !self.jit_verify {
                if let Some(k) = &bd_key { crate::bootdisp::record(k, true); }
                #[cfg(feature = "next-gen-gc")]
                run_shadow_hook(self, &r);
                self.give_vec(locals);
                self.trampoline = saved_trampoline;
                crate::vm::aot_route_diag::jit_entry();
                return r;
            }
            verify_out = Some(r); // verify mode: interpret too and compare below.
        }
        // Fell through every compiled tier → this entry runs in the interpreter.
        //
        // The interpreted half of the coverage pair — see `jit_enter` for the other half, and the
        // note at the top of this function for why it is counted HERE and not there.
        crate::vm::aot_route_diag::interpreted();
        crate::diag::exec_interpreted().inc();
        // `RegionGuard::enter` opens with `regions_armed()` — two relaxed loads of globals — and
        // returns `None` when nothing is armed, which is every production run. The bit answers it
        // from a register; the kill switch reads the same predicate the guard itself would have,
        // so `None` comes back in exactly the cases it came back before.
        let _reg = if ig_bit(ig, IG_REGION, crate::alloc_sites::regions_armed) {
            crate::alloc_sites::RegionGuard::enter(crate::alloc_sites::Region::Interp)
        } else {
            None
        };
        // WHICH methods these are, and what the compiler thinks of each (`RAVA_INTERP_WHO`).
        // The count alone cannot separate "a few hot methods never get compiled" from "thousands
        // called twice each", and those want opposite work.
        if ig_bit(ig, IG_INTERP_WHO, crate::interp_who::enabled) {
            crate::interp_who::note(
                code as *const rava_classfile::Code as usize,
                &class.file.this_class,
                name,
                descriptor,
            );
        }
        // ⭐**HOW MANY INTERPRETED FRAMES ARE REFUSED BY THE COMPILER RATHER THAN BY THE
        // THRESHOLD?** `jit_run` bails unconditionally on `!code.exceptions.is_empty()` — a
        // method with ANY try/catch is never compiled, which is a capability gap, not a tiering
        // policy, and Minecraft's loading code (datapacks, JSON, resources) is full of them.
        if crate::diag::interp_has_handlers().armed() {
            if code.exceptions.is_empty() {
                crate::diag::interp_no_handlers().inc();
            } else {
                crate::diag::interp_has_handlers().inc();
            }
        }
        if let Some(k) = &bd_key { crate::bootdisp::record(k, false); }
        // ⭐**NO SECOND BUFFER.** The frame's locals and operands share one allocation now
        // (row 444), so the operand buffer that used to be checked out here does not exist —
        // and row 444 measured NEUTRAL precisely because this line still checked one out and
        // `Frame::new` handed it straight back. The pool census counted the waste exactly:
        // 84,000,006 checkouts for 42M calls. This is the half of stage 1 that was missing.
        //
        // ⭐**AND THE FRAME NO LONGER TOUCHES THE CLASS REFCOUNT** (`RAVA_NO_FAST_FRAME=1`
        // restores the clone). `class` is a `&Arc<LoadedClass>` PARAMETER of this function, so
        // the borrow checker has already proved the strong count the caller holds is live for
        // every instruction below; `frame` is a local of this same function and is dropped
        // before it returns. The `Arc::clone` was therefore a pair of atomics — increment here,
        // decrement in the frame's drop — guarding a count that provably cannot reach zero
        // inside the window it guards. Row 395 prices that pair at 12 ns of a 105 ns call, on a
        // path taken 14.1 M times per boot.
        //
        // The pointer is unchanged, so nothing downstream can tell: `Arc::as_ptr(&frame.class)`
        // still keys the inline caches to the same site identity, and an `Arc::clone` off the
        // frame still takes a real reference (off the caller's count, which is alive).
        let mut frame = if crate::vmframe::fast_frame() {
            // SAFETY: `class` outlives `frame` by the signature of this function (see above),
            // `frame` never escapes it, and `FrameGuard`'s destructor runs on the unwind path
            // too — so the borrowed `Arc` is never destroyed and never leaks a count.
            unsafe { crate::vmframe::FrameGuard::borrowed(code, locals, class) }
        } else {
            crate::vmframe::FrameGuard::owned(Frame::new(
                code,
                locals,
                Arc::clone(class),
                Vec::new(),
            ))
        };
        // 10.1b(B) toString-stack tracer: on a density/climate/noise toString, print the caller chain
        // (call_stack already holds the callers) to localise who stringifies the graph + how often.
        if ig_bit(ig, IG_TOSTRING, tostring_probe)
            && name == "toString"
            && (class.file.this_class.contains("DensityFunction")
                || class.file.this_class.contains("Climate")
                || class.file.this_class.contains("/synth/"))
        {
            use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Relaxed);
            if n < 6 {
                eprintln!(
                    "[TOSTRING-CALLER #{n}] {} .toString() called from:\n{}",
                    class.file.this_class,
                    self.backtrace()
                );
            }
        }
        // The named call_stack is only built when tracing — the two String
        // allocations per call are otherwise pure hot-path overhead.
        let trace = self.trace.throw || self.trace.item || self.trace.big || self.trace.codec
            || ig_bit(ig, IG_TOSTRING, tostring_probe);
        if trace {
            crate::vm::thread_context::push_call(
                &mut self.call_stack,
                class.file.this_class.clone(),
                format!("{name}{descriptor}"),
            );
        }
        frames::depth_inc();
        // (trampoline already cleared above, before the compiled tiers — `run` only
        // returns `Normal` here.)
        // `and_then`, not `map`: the NurseryFull arm below has to be able to FAIL (a region
        // abort unwinds as an error), and a `map` closure can only produce a value.
        let mut result = opcodes::run(self, code, &mut frame).and_then(|exit| Ok(match exit {
            crate::opcodes::RunExit::Normal(v) => v,
            // These two must never reach a RECURSIVE invoke: `apply` runs a method to
            // completion on the Rust stack with `trampoline = false`. A real daemon OS
            // thread used to break this — it drove the green scheduler from its own
            // Thread.sleep (setting the shared `trampoline`/`yield_request`), so a
            // staged yield surfaced here. The panic message carries thread state for
            // diagnosis (panic output is flushed; a plain eprintln to redirected
            // stderr is buffered and lost on abort).
            crate::opcodes::RunExit::Call => panic!(
                "recursive invoke must not trampoline ({}.{}{} worker={} trampoline={})",
                class.file.this_class, name, descriptor, crate::gil::is_worker(), self.trampoline
            ),
            crate::opcodes::RunExit::Yield => panic!(
                "recursive invoke must not yield ({}.{}{} worker={} trampoline={})",
                class.file.this_class, name, descriptor, crate::gil::is_worker(), self.trampoline
            ),
            #[cfg(feature = "next-gen-gc")]
            crate::opcodes::RunExit::Bounded => unreachable!(
                "recursive invoke: RunExit::Bounded only from the counted-loop fork worker body run"
            ),
            // `NurseryFull` HAS TWO MEANINGS, and this arm long asserted only the first.
            //
            // (1) A `new` found the arena nursery full. A recursive invoke cannot
            //     safepoint — its frames are Rust-stack-borrowed up the chain — so
            //     `new_object_value` hard-exits instead of signalling, and that half of
            //     the claim below still holds.
            //
            // (2) SINCE THE GIL-FREE LANE, the op checkpoint reuses this same variant as
            //     the ARMORED ABORT EXIT (`opcodes.rs`, the `abort_requested()` arm): it
            //     was the only exit a locked-exec worker grinding a long interpreted
            //     subtree had. A recursive invoke reaches that checkpoint like any other
            //     code, so the arm became reachable the moment the second meaning was
            //     added — and it panicked the whole process. Measured: a lane boot died
            //     at 74.2s here, before `Done`.
            //
            // A region abort cannot exit by returning a `RunExit` from here (there is no
            // loop to return to; the caller is a Rust frame deep in the subtree), so it
            // unwinds as an error instead. `RegionAbort` and not a `Throw` precisely
            // because only `Throw` reaches `find_handler`: no guest `catch` can swallow
            // the abort on its way out.
            #[cfg(feature = "next-gen-gc")]
            crate::opcodes::RunExit::NurseryFull => {
                if crate::parallel::gilfree() && crate::parallel::abort_requested() {
                    return Err(VmError::RegionAbort);
                }
                unreachable!(
                    "recursive invoke: new_object_value hard-exits on nursery full, never signals \
                     (and no region abort is pending, so this is not the lane's abort exit)"
                )
            }
            // Recursive mode inits inline (new_object_value calls ensure_init when
            // trampoline=false), so a clinit signal never surfaces here.
            #[cfg(feature = "next-gen-gc")]
            crate::opcodes::RunExit::ClinitPending(_) => unreachable!(
                "recursive invoke: classes are inited inline, never signalled"
            ),
        }));
        // RAVA_TRACE_UNDERFLOW: name each frame in the unwind chain of an operand-stack
        // underflow (the bare [UNDERFLOW] in Frame::pop has only class+pc). The deepest
        // frame names the offending method; the rest give the call chain.
        if matches!(result, Err(VmError::StackUnderflow))
            && std::env::var_os("RAVA_TRACE_UNDERFLOW").is_some()
        {
            // Name the op + live stack depth at this frame's pc. The DEEPEST frame's
            // op is the one that underflowed (popped an empty stack) — invaluable for
            // a tick-path imbalance where the call chain alone isn't enough.
            let op = code.bytecode.get(frame.pc).copied().unwrap_or(0);
            eprintln!(
                "[UNDERFLOW-FRAME] {}.{}{} pc={} op=0x{:02X} stack={}",
                class.file.this_class, name, descriptor, frame.pc, op, frame.stack_len()
            );
        }
        self.trampoline = saved_trampoline;
        frames::depth_dec();
        // Return the frame's ONE buffer to the pool for the next call to reuse, and release the
        // class only if this frame owns a count in it. Both live in `FrameGuard::drop`, at
        // exactly this point in the call, for two reasons: the second `give_vec(Vec::new())`
        // that used to stand here returned a buffer that stage 1 deleted — a thread-local
        // resolve and a census probe to hand the pool a capacity-0 `Vec` it discards — and a
        // destructor is the only form of "return the buffer" that also runs when `opcodes::run`
        // unwinds, which is now load-bearing because the borrowed class must not be dropped.
        drop(frame);
        // Unbox a box returned from a method whose descriptor says PRIMITIVE, so a caller
        // doing ifeq/istore/i2c on the result gets the raw value — the defensive complement to
        // the parameter unboxing in `expand_locals_into`, and the same widening: `Z` has always
        // needed it (a codec-decoded `Boolean` flowing into a record whose `():Z` accessor a
        // caller consumes with `ifeq`), and `crate::boxid` makes `Character`/`Short`/`Byte` real
        // objects that can arrive here the same way.
        //
        // ⚠**The class is CHECKED, not assumed.** `bool_of`-style blind `$value` reads are safe
        // where the descriptor is the only evidence, but here the wrong answer would be silent:
        // a `Ref` that is not the box of THIS return type keeps its handle rather than becoming
        // a zero. The check is also what makes the arm free in the common case — `class_of` is
        // reached only for a `Ref` returned from a primitive-returning method, which is already
        // the anomaly.
        let prim_ret = descriptor.as_bytes().len() >= 2
            && descriptor.as_bytes()[descriptor.len() - 2] == b')';
        if prim_ret {
            let ret = descriptor.as_bytes()[descriptor.len() - 1];
            let want = match ret {
                b'Z' => Some("java/lang/Boolean"),
                b'C' if crate::boxid::enabled() => Some("java/lang/Character"),
                b'S' if crate::boxid::enabled() => Some("java/lang/Short"),
                b'B' if crate::boxid::enabled() => Some("java/lang/Byte"),
                _ => None,
            };
            // BORROWED, not moved: `result` is read again below (the trace arm, then the JIT
            // verifier), and the historic `)Z` form got away with a move only because it was
            // the whole `if` condition.
            if let (Some(want), Ok(Some(Value::Ref(h)))) = (want, &result) {
                let h = *h;
                if self.heap.class_of(h) == Some(want) {
                    result = Ok(Some(Value::Int(
                        self.heap.get_field(h, "$value").as_int().unwrap_or(0),
                    )));
                }
            }
        }
        if trace {
            if let Err(e) = &result {
                if !matches!(e, VmError::Throw(_)) {
                    eprintln!("  in {}.{name}{descriptor}: {e}", class.file.this_class);
                }
            }
            crate::vm::thread_context::pop_call(&mut self.call_stack);
        }
        // Verify mode: compare the JIT result against the interpreter's.
        if let Some(jr) = verify_out {
            match (&jr, &result) {
                (Ok(Some(a)), Ok(Some(b))) if !jit_values_eq(a, b) => {
                    eprintln!(
                        "[JIT-MISMATCH] {}.{name}{descriptor}: jit={a:?} interp={b:?}",
                        class.file.this_class
                    );
                }
                (Ok(jv), Ok(_)) => {
                    let _ = jv;
                }
                (Err(_), Err(_)) => {} // both threw — acceptable
                (jv, iv) => {
                    eprintln!(
                        "[JIT-MISMATCH-CTRL] {}.{name}{descriptor}: jit={:?} interp={:?}",
                        class.file.this_class,
                        jv.as_ref().map(|o| o.is_some()),
                        iv.as_ref().map(|o| o.is_some())
                    );
                }
            }
        }
        // Collect-mode: swallow an uncaught exception (replace with the method's
        // default return) so the boot keeps surfacing downstream blockers.
        if let Err(VmError::Throw(h)) = &result {
            let exc = match h {
                Value::Ref(eh) => self.heap.class_of(*eh).unwrap_or("?").to_string(),
                _ => format!("{h:?}"),
            };
            let sig = format!("{}.{name}{descriptor}", class.file.this_class);
            if crate::opcodes::collect_throw(&sig, &exc) {
                result = Ok(crate::opcodes::default_for_return(descriptor));
            }
        }
        #[cfg(feature = "next-gen-gc")]
        run_shadow_hook(self, &result);
        result
    }
}

#[cfg(feature = "next-gen-gc")]
fn first_flat_compile_request(requested: &mut crate::FastMap<usize, ()>, key: usize) -> bool {
    requested.insert(key, ()).is_none()
}

#[cfg(all(test, feature = "next-gen-gc"))]
mod flat_compile_first_use_tests {
    use super::first_flat_compile_request;
    use crate::FastMap;

    #[test]
    fn requests_each_code_identity_once_without_a_hotness_threshold() {
        let mut requested = FastMap::default();
        assert!(first_flat_compile_request(&mut requested, 11));
        assert!(!first_flat_compile_request(&mut requested, 11));
        assert!(first_flat_compile_request(&mut requested, 12));
    }
}
