//! The compile driver: the fixed sequence a method goes through to become machine code.
//!
//! Refuse, resolve, import, analyse, try a fork, emit a body, finish. Each step is one
//! call now, and the order of the steps IS the compiler.

use crate::jit::*;

// ⭐**THE COMPILER'S SCRATCH SPACE WAS BUILT AND THROWN AWAY ONCE PER METHOD.**
//
// The profile row this comes out of is our own compiler: `regalloc2::ion` at 1.06 + 0.94 + 0.82
// = **2.82% of all samples across three rows** during phase A, on a second thread sitting at
// ~99% for the whole of it, while only 4.1% of live samples are inside emitted machine code.
// The boot asks Cranelift for ~1,950 bodies and it accepts 98.2% of them, so this is a serial
// per-method cost paid ~1,950 times on the critical path.
//
// Cranelift's own `context` module opens with the instruction we were not following: *"When
// compiling many small functions, it is important to avoid repeatedly allocating and
// deallocating the data structures needed for compilation. The `Context` struct is used to hold
// on to memory allocations between function compilations."* This driver called
// `module.make_context()` and `FunctionBuilderContext::new()` at the top of every compile and
// dropped both at the bottom.
//
// What that costs is not one allocation — it is every GROWTH, from empty, of every entity map
// inside them, on every method: `dfg.insts` / `results` / `values` / `value_lists`, the
// `layout`, `stack_slots`, the **~70 imported `signatures` and `ext_funcs` the `Env` below
// declares unconditionally** (`declare_func_in_func` clones a whole `ir::Signature`, Vec and
// all, for each), `ControlFlowGraph`'s per-block pred/succ lists, the `DominatorTree`, the
// `LoopAnalysis`, and `FunctionBuilderContext`'s SSA-construction maps. `Context::clear()`
// clears all of them WITHOUT releasing capacity, so a parked context serves the next method out
// of buffers that are already the right size: the allocator sees one shape per compiler thread
// instead of ~1,950 build-ups and tear-downs. That is the `mi_malloc_aligned` row (1.64%, the
// top host row of phase A) charged where the compiler is standing.
//
// ⭐**THIS CHANGES NO EMITTED BYTE, AND THAT IS PROVABLE RATHER THAN HOPED.** A cleared
// `Context` is by construction identical to a fresh one for everything a compile reads:
// `clear()` resets the function (including `func.params`, where `declare_func_in_func` interns
// its `UserExternalName`s), the CFG, the dominator tree, the loop analysis, `compiled_code` and
// `want_disasm` (which `finish_compile` sets under `RAVA_JIT_DUMP`). The single field it leaves
// at a stale value is `func.signature`'s call conv — and that whole signature is overwritten
// unconditionally forty lines below from `module.make_signature()`, so even that cannot be
// observed.
//
// ⚠**WHAT IS NOT POOLED, AND WHY.** `cranelift-frontend` keeps `FunctionBuilderContext::clear`
// and `is_empty` PRIVATE; `FunctionBuilder::finalize()` is the only public route back to empty,
// and `FunctionBuilder::new` debug-asserts emptiness. So a builder context is parked only on the
// path that actually finalized. The emit-bail path drops a dirty one on the floor exactly as
// before — a refusal is rare (1.8% of attempts) and correctness beats reuse.
thread_local! {
    /// One parked compile context per compiler thread (`SHARED_MODULE` is thread-local too, so
    /// this matches its lifetime). `None` before the first compile and after a monster body.
    static CTX_POOL: std::cell::RefCell<Option<cranelift::codegen::Context>> =
        const { std::cell::RefCell::new(None) };
    /// The SSA-construction scratch, parked only after a successful `finalize()`.
    static FBCTX_POOL: std::cell::RefCell<Option<FunctionBuilderContext>> =
        const { std::cell::RefCell::new(None) };
}

/// A body big enough that parking its peak buffers would pin real memory on this thread for the
/// rest of the process. Measured in CLIF instructions BEFORE `clear()` (which zeroes the counts
/// but keeps the capacity). A 64 KB method lowers to a few hundred thousand instructions at the
/// extreme; ordinary boot methods are two or three orders of magnitude below this, so the cap
/// never fires on the population it is meant to serve and only exists so the pool cannot become
/// a leak.
const CTX_POOL_MAX_INSTS: usize = 96_000;

// ⭐**THE DRIVER READ THE PROCESS ENVIRONMENT TWICE, AND ITS OWN POOL SWITCH FOUR TIMES, ON
// EVERY COMPILE ATTEMPT.**
//
// SHAPE BEFORE, at the top of `compile_with_static_intrinsics`:
//   * `std::env::var("RAVA_DBG_CBAIL").is_ok()` — a RAW read, no memo.
//   * `std::env::var_os("RAVA_VERIFY_LOADS").is_some()` — a RAW read, no memo.
// On Windows/MSVC each of those is `RtlQueryEnvironmentVariable`: the key is UTF-16-encoded into
// a fresh heap buffer, the WHOLE process environment block is walked linearly under a lock, and
// on a hit the value is decoded into an owned `String`/`OsString` that is dropped one line later.
// Both name DIAGNOSTIC DUMPS that are off in every run that is not being debugged, so the entire
// cost is paid to learn "no" — twice per attempt, forever.
//
// And `fast_compile2_enabled()` (a `OnceLock<bool>`) was read FOUR times per attempt: in
// `CtxGuard::take`, again in `CtxGuard::drop`, again at the `FunctionBuilderContext` take, and
// again at the park. Each read is an ACQUIRE-ordered load, which is an optimisation barrier LLVM
// may not hoist or forward across the ~700 lines of analysis and emission sitting between two of
// them, so four independent barriers survived to answer one immutable question.
//
// SHAPE AFTER: one `OnceLock<u32>` holding all four constants as bits, read ONCE at the natural
// entry (`driver_env()` in `compile_with_static_intrinsics`) and bit-tested against constant
// masks from there on. The pool switch is additionally CARRIED IN `CtxGuard`, so take and drop
// can no longer disagree about whether this context came from the pool.
//
// POPULATION: one per JIT compile ATTEMPT — ~1,900 a product boot (the boot asks Cranelift for
// ~1,950 bodies and accepts 98.2%). That is ~3,800 environment-block walks with their allocations
// and ~7,600 acquire loads, replaced by ~1,900 loads of one word.
//
// ⚠ POSITION IS NOT LOAD-BEARING FOR ANY BIT HERE. All four are pure `env` tests naming dumps or
// a kill switch; none arms a thread, none decides whether a later side effect fires, and the only
// writer of this process's environment is the CLI's config apply, which completes before the VM
// exists. Latching them at the first compile therefore cannot reorder anything observable.
//
// KILL SWITCH `RAVA_NO_FAST_DRIVERENV=1` (and the block switch `RAVA_NO_EMIT_MORE`) restores the
// exact pre-fold shape: the two diagnostics go back to a RAW read at their original statement and
// the pool switch goes back to its own private `OnceLock`.
/// Bit ordinals in the driver's process-constant word.
const DE_LEGACY: usize = 0;
const DE_DBG_CBAIL: usize = 1;
const DE_VERIFY_LOADS: usize = 2;
const DE_FAST_COMPILE2: usize = 3;
const DE_N: usize = 4;

#[inline]
const fn de_bit(i: usize) -> u32 {
    1u32 << i
}

/// THE SINGLE SOURCE OF TRUTH for each bit. Both the folded word and the opt-out cells call this,
/// so the two shapes cannot drift into disagreeing about a gate.
fn raw_driver_env(i: usize) -> bool {
    match i {
        DE_LEGACY => {
            std::env::var_os("RAVA_NO_FAST_DRIVERENV").is_some()
                || crate::flags::emit_more_disabled()
        }
        // Gated compile-bailout tracer: prints which stage returned None plus a bytecode prefix.
        // `var(..).is_ok()` rather than `var_os(..).is_some()` — kept EXACTLY as it was, so a
        // non-UTF-8 value still reads false in both arms.
        DE_DBG_CBAIL => std::env::var("RAVA_DBG_CBAIL").is_ok(),
        DE_VERIFY_LOADS => std::env::var_os("RAVA_VERIFY_LOADS").is_some(),
        DE_FAST_COMPILE2 => {
            std::env::var_os("RAVA_NO_FAST_COMPILE2").is_none()
                && !crate::flags::emit_more_disabled()
        }
        _ => false,
    }
}

/// All four driver constants as one word, computed once per process.
#[inline]
pub(crate) fn driver_env() -> u32 {
    static W: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        // OPT-OUT: publish ONLY the legacy bit and latch nothing else, so the two diagnostics go
        // back to a RAW per-attempt read and the pool switch back to its own cell — byte-for-byte
        // the pre-fold shape, which is the only thing an A/B of this switch is for.
        if raw_driver_env(DE_LEGACY) {
            return de_bit(DE_LEGACY);
        }
        let mut w = 0u32;
        for i in 1..DE_N {
            if raw_driver_env(i) {
                w |= de_bit(i);
            }
        }
        w
    })
}

/// OPT-OUT SHAPE (`RAVA_NO_FAST_DRIVERENV=1`): the two diagnostics go back to a RAW per-attempt
/// environment read and the pool switch back to a private `OnceLock` — byte-for-byte what each
/// site owned before the fold, so an A/B measures the SHAPE and nothing else. Cold and
/// out-of-line: a default run never enters it and must not pay it a byte of I-cache.
#[cold]
#[inline(never)]
fn legacy_driver_env(i: usize) -> bool {
    if i == DE_FAST_COMPILE2 {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        return *ON.get_or_init(|| raw_driver_env(DE_FAST_COMPILE2));
    }
    raw_driver_env(i)
}

/// One bit out of an ALREADY-READ word. This is the form the hot callers use: the acquire load
/// happens once at the entry, every use after it is a test against a constant mask.
#[inline]
fn de_of(w: u32, i: usize) -> bool {
    if w & de_bit(DE_LEGACY) != 0 {
        return legacy_driver_env(i);
    }
    w & de_bit(i) != 0
}

/// Kill switch, DEFAULT ON: `RAVA_NO_FAST_COMPILE2=1` restores the fresh-per-compile contexts.
/// Also consults the block switch, so one A/B prices the whole "emit more, allocate less" set.
/// Now a bit of `driver_env()`; the in-function uses read the word once and test this bit.
/// Kept as the NAMED accessor for the switch (the fold is an implementation detail of this file)
/// even though the driver itself now threads the hoisted `fast_compile2` local instead.
#[allow(dead_code)]
#[inline]
pub(crate) fn fast_compile2_enabled() -> bool {
    de_of(driver_env(), DE_FAST_COMPILE2)
}

/// Borrows the compile context out of the thread-local pool and puts it back CLEARED on every
/// exit path — which is the whole reason this is a guard and not a pair of statements: the
/// compile can leave through the `declare_imports` bail, the `emit_body` refusal, or either of
/// the two `?`s inside `finish_compile`, and a context that escaped uncleared would be handed to
/// the next method with another function still in it.
struct CtxGuard {
    ctx: Option<cranelift::codegen::Context>,
    /// The pool switch as decided at `take`. Carried rather than re-read so `drop` cannot reach a
    /// different verdict than the take that paired with it — and so the whole guard costs the
    /// caller's ONE word read instead of two acquire loads of its own.
    pooling: bool,
}
impl CtxGuard {
    fn take(module: &JITModule, pooling: bool) -> Self {
        let pooled = if pooling {
            CTX_POOL.with(|c| c.borrow_mut().take())
        } else {
            None
        };
        // COLD ARM — first compile on this thread, a body that blew the cap, or the kill switch:
        // exactly the statement this replaced.
        CtxGuard { ctx: Some(pooled.unwrap_or_else(|| module.make_context())), pooling }
    }
}
impl Drop for CtxGuard {
    fn drop(&mut self) {
        let Some(mut c) = self.ctx.take() else { return };
        if !self.pooling {
            return; // dropped here, same as before
        }
        if c.func.dfg.num_insts() > CTX_POOL_MAX_INSTS {
            return; // too big to keep resident; the next compile allocates fresh
        }
        // Cleared BEFORE it is parked, never after it is taken: `FunctionBuilder` and
        // `define_function` must not be able to observe a context that still holds a previous
        // method, whatever order future edits put the take/park in.
        c.clear();
        CTX_POOL.with(|p| *p.borrow_mut() = Some(c));
    }
}

/// ⭐**THE STATIC FLAG, RECOVERED BY IDENTITY — and the bug that made it necessary.**
///
/// `localtypes::compute` needs to know whether local 0 is `this`, because every parameter after
/// it sits one slot lower in a STATIC method. The driver never carried the flag, so both call
/// sites passed `false`. At the feasibility counter that was harmless and documented (an
/// instance-shaped guess only makes the count a lower bound). At the PRODUCTION map it was a
/// silent miscompile: a static method's parameter types were each shifted one slot, and a deopt
/// then rebuilt the interpreter's frame with an int decoded as a reference and a reference
/// decoded as an int.
///
/// Measured: `VegetationFeatures.segmentedBlockPatchBuilder` is `(Block,I,I,IntegerProperty,
/// EnumProperty)`, so its locals are `[Ref,Int,Int,Ref,Ref,Ref,Int,Ref,Ref]` — and the emitted
/// tags read `[Ref,Ref,Int,Int,Ref,Ref,Int,Ref,Ref]`, the same list with a phantom `this` pushed
/// in front. **ONE** deopt at that site turned four bit-exact gates red, and it hid for as long
/// as it did because an all-reference frame survives the shift unchanged: references shift onto
/// references. That is why 6.65 M forced deopts passed and why refusing primitive frames
/// (`RAVA_DEOPT_REFONLY`) went green.
///
/// `Code` is stored inline in its `MemberInfo`, so the owning method is found by POINTER
/// IDENTITY — exact, and needing no new parameter through eleven call sites. `None` when the
/// class is absent (unit tests pass `class_ptr = 0`) or the code does not belong to it, and the
/// caller then computes no map at all: a deopt site is never emitted on a guess.
#[cfg(feature = "next-gen-gc")]
fn method_is_static(class_ptr: i64, code: &Code) -> Option<bool> {
    if class_ptr == 0 {
        return None;
    }
    let class = unsafe { &*(class_ptr as *const crate::class_loader::LoadedClass) };
    let want = code as *const Code;
    for m in &class.file.methods {
        if let Some(c) = m.code.as_ref() {
            if std::ptr::eq(c as *const Code, want) {
                return Some(m.access_flags & 0x0008 != 0);
            }
        }
    }
    None
}

pub(crate) fn compile_with_static_intrinsics(
    code: &Code,
    descriptor: &str,
    pool: &ConstantPool,
    class_ptr: i64,
    intern: &mut dyn FnMut(&str, &str) -> (u32, Option<u32>),
    static_intrinsics: &std::collections::HashMap<u16, StaticIntrinsic>,
    inline_targets: &std::collections::HashMap<u16, InlineCallee>,
    native_calls: &std::collections::HashSet<u16>,
    statics: &std::collections::HashMap<u16, (u32, JT)>,
    #[cfg(feature = "next-gen-gc")] new_arena: &std::collections::HashMap<u16, (u32, u32)>,
    #[cfg(feature = "next-gen-gc")] ctor_inlines: &std::collections::HashMap<u16, CtorInline>,
    #[cfg(feature = "next-gen-gc")] concat_shapes: &std::collections::HashMap<u16, ConcatShape>,
    #[cfg(feature = "next-gen-gc")] hof_fusions: &std::collections::HashMap<usize, crate::hof_fusion::HofFusionPlan>,
    #[cfg(feature = "next-gen-gc")] getter_inlines: &std::collections::HashMap<u16, (u32, JT)>,
    #[cfg(feature = "next-gen-gc")] deep: Option<DeepShape>,
    // cp-indices whose field must keep its runtime tag under the flip, precomputed by a
    // caller that has a `Vm`. `None` = descriptor-only fallback (see the use site).
    #[cfg(feature = "next-gen-gc")] tagged_fields: Option<&std::collections::HashSet<u16>>,
    #[cfg(feature = "next-gen-gc")] prim_arrays: &ArenaPrimArrayPlan,
) -> Option<JitFn> {
    // ⭐**WHAT DOES COMPILING COST, AS A SHARE OF THE BOOT?** The module profile puts Cranelift at
    // 7.4% of host samples — nearly twice the entire compiled→host seam — and it is not guest
    // work at all: it is the price of having a JIT. Named populations account for ~29% of a boot
    // and this is the largest single unnamed one.
    //
    // Self time, so a compile that recursively resolves and loads classes charges those to
    // whoever meters them, not to the code generator.
    let _ct = crate::diag::compile_self().start();
    let compile_trace = crate::jit::compile_phase_trace::CompileTrace::new(code.bytecode.len());
    // ⭐**VALIDATE THE DEOPT TYPE MAP BEFORE ANYTHING DEPENDS ON IT** (inert, `RAVA_DIAG=deopt`).
    // Deoptimisation needs the type of every local at the pc it resumes from, and this tree does
    // not parse `StackMapTable`, so the map is recovered by walking the bytecode. Whether that
    // walk actually PROVES the types on real code decides if the feature is possible at all —
    // and it is one counter, not a rewrite.
    if crate::diag::deopt_pc_known().armed() {
        if let Some(lt) = crate::jit::analysis::localtypes::compute(
            code,
            descriptor,
            // The flag is recovered by identity now (see `method_is_static`). It used to be
            // hardcoded `false` here, which made this count a lower bound — and the same `false`
            // was copied to the production map below, where it silently mistyped every static
            // method's parameters instead of merely under-counting.
            method_is_static(class_ptr, code).unwrap_or(false),
        ) {
            let n = code.max_locals as usize;
            let mut pc = 0usize;
            while pc < code.bytecode.len() {
                let Some(len) = crate::jit::insn_len_at(&code.bytecode, pc) else { break };
                if lt.at(pc).is_some() {
                    if lt.all_known(pc, n) {
                        crate::diag::deopt_pc_known().inc();
                    } else {
                        crate::diag::deopt_pc_unknown().inc();
                    }
                }
                pc += len;
            }
        }
    }
    // ⭐**IS IT ON THE CRITICAL PATH?** 4.37% of CPU is only 4.37% of WALL if the compile runs on
    // the thread the boot is waiting for. This tree records compilation as queued and overlapping
    // execution; a single split says whether that is true today.
    if crate::diag::on_main_thread() {
        crate::diag::compile_main().inc();
    } else {
        crate::diag::compile_worker().inc();
    }
    // ⭐**THE NATURAL ENTRY: ONE WORD, READ ONCE PER COMPILE ATTEMPT.** See `driver_env` above for
    // what this replaces (two raw `RtlQueryEnvironmentVariable` walks plus four `OnceLock` acquire
    // loads, per attempt, ~1,900 attempts a boot) and for the kill switch.
    let denv = driver_env();
    // Gated compile-bailout tracer (RAVA_DBG_CBAIL): prints which stage returned None
    // + a bytecode prefix so a specific failing method (e.g. getDouble opens
    // 1b 2a b4) can be pinned. No effect unless the env var is set.
    let dbg_bail = de_of(denv, DE_DBG_CBAIL);
    // The context/builder pool switch, decided ONCE here and threaded to all four of its uses
    // (`CtxGuard::take`, the guard's `Drop`, the `FunctionBuilderContext` take, and its park).
    let fast_compile2 = de_of(denv, DE_FAST_COMPILE2);
    if de_of(denv, DE_VERIFY_LOADS) {
        let cls = if class_ptr != 0 {
            unsafe { &*(class_ptr as *const crate::class_loader::LoadedClass) }
                .file
                .this_class
                .as_str()
        } else {
            "?"
        };
        eprintln!("[LOADSITE] method={cls}.{descriptor}");
    }
    macro_rules! bail {
        ($stage:expr) => {{
            // ⭐**AGGREGATE, NOT JUST PRINT.** `[CBAIL]` printed one line per refusal under a debug
            // gate and nothing counted them, so "which refusal keeps two thirds of all method
            // entries in the interpreter?" had no answer. A product boot enters 15,212,176
            // interpreted method bodies against 7,712,120 compiled ones and compiles only ~1,500
            // methods at a cap of 4,096 — so the cap is not what binds, a refusal is. This says
            // which. Always on (one relaxed add on a path taken once per method per compile
            // attempt, never per call).
            crate::jit::compile_bail::note($stage);
            if dbg_bail {
                let b = &code.bytecode;
                // Class name via the compile-owned class_ptr (valid for the compile's duration).
                let cls = if class_ptr != 0 {
                    unsafe { &*(class_ptr as *const crate::class_loader::LoadedClass) }
                        .file
                        .this_class
                        .as_str()
                } else {
                    "?"
                };
                eprintln!(
                    "[CBAIL] stage={} class={} desc={} len={} bc0={:02x}{:02x}{:02x}",
                    $stage,
                    cls,
                    descriptor,
                    b.len(),
                    b.first().copied().unwrap_or(0),
                    b.get(1).copied().unwrap_or(0),
                    b.get(2).copied().unwrap_or(0),
                );
            }
            return None;
        }};
    }
    if !code.exceptions.is_empty() {
        bail!("exceptions");
    }
    // A class the native layer serves keeps its field access on the host accessor — see
    // `Env::callback_fields`. The name is the class being compiled, which is exactly the
    // question `native_served_prefix` answers.
    let callback_fields = {
        let cls = if class_ptr != 0 {
            unsafe { &*(class_ptr as *const crate::class_loader::LoadedClass) }
                .file
                .this_class
                .as_str()
        } else {
            ""
        };
        crate::opcodes::native_served_prefix(cls)
    };
    // `ret` is None only for void; reference returns are Some(JT::Ref) and are
    // compiled — the result cell is a tagged handle / box index the caller
    // decodes via `unbox_ref` (see `jit_run`).
    let ret = ret_jt(descriptor);
    let bc = &code.bytecode;
    // Don't compile callback-dominated methods — native code wouldn't beat the
    // interpreter for them (callbacks are the cost), so compiling only adds
    // overhead. They keep running interpreted.
    // 20.1 Tier-0: this is a PERFORMANCE heuristic, not a correctness gate (the codegen
    // handles invoke-dense bodies fine — every invoke lowers to jit_call). Under
    // RAVA_JIT_TIER0 we skip it so the invoke-tree DFU/Codec reload compiles (interp
    // frame overhead × millions of calls saved); bit-exact via the identical codegen.
    if callback_heavy(
        bc,
        pool,
        inline_targets,
        native_calls,
        #[cfg(feature = "next-gen-gc")]
        hof_fusions,
    ) && !crate::jit_tier0()
    {
        bail!("callback_heavy");
    }
    let Some(leaders) = find_leaders(bc) else {
        bail!("find_leaders");
    };
    // Pre-resolve every getfield's (interned id, field type) by cp index.
    let Some(fields) = resolve_fields(bc, pool, intern) else {
        bail!("resolve_fields");
    };
    // Inline BitSet reads: resolve the synthetic `$bits` field id ONLY when the
    // method actually calls `java.util.BitSet.get(I)Z` or `isEmpty()Z` — so an unrelated compile
    // never forces `java/util/BitSet` to load early (a class-init-order perturbation
    // that could break the determinism gates). `$bits` is a bare synthetic field, so
    // `intern` returns the same id `heap.field_id("$bits")` uses (the by-id read the
    // fast arm emits). 0 when the method has no such call → the inline path is off.
    #[cfg(feature = "next-gen-gc")]
    let bits_fid: i64 = {
        let mut uses = false;
        let mut pc = 0usize;
        while pc < bc.len() {
            let op = bc[pc];
            let Some(len) = insn_len_at(bc, pc) else { break };
            if op == 0xb6 {
                let cpi = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                if let Some(m) = pool.member_ref(cpi) {
                    if m.owner == "java/util/BitSet"
                        && ((m.name == "get" && m.descriptor == "(I)Z")
                            || (m.name == "isEmpty" && m.descriptor == "()Z"))
                    {
                        uses = true;
                        break;
                    }
                }
            }
            pc += len;
        }
        if uses { intern("java/util/BitSet", "$bits").0 as i64 } else { 0 }
    };
    // Same scan for `java/util/ArrayList.get(I)Ljava/lang/Object;`, so the inline arm can read
    // `$elems` by id. `bench/OpCost2.java` prices the alternative: a `list.get(i)` loop costs
    // 187 ns per element against the JVM's whole-loop 13, and it costs that even though
    // `fu_fastpath` already serves `ArrayList.get` — because reaching `fu_fastpath` is a host
    // call, and a cheaper host call is still a host call.
    let elems_fid: i64 = {
        let mut uses = false;
        let mut pc = 0usize;
        while pc < bc.len() {
            let op = bc[pc];
            let Some(len) = insn_len_at(bc, pc) else { break };
            if op == 0xb6 || op == 0xb9 {
                let cpi = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                if let Some(m) = pool.member_ref(cpi) {
                    // ⚠**THIS SCAN GATES EVERY `$elems` INLINE ARM, AND IT ONLY LOOKED FOR
                    // `get`.** A method that calls `size()` and never `get(i)` came through with
                    // `elems_fid = 0`, so the size arm refused every site — it fired ZERO times
                    // on a boot while `size` kept arriving 4.46 M times, and the reason was
                    // three files away from the arm itself.
                    //
                    // `size` is not keyed on the declared owner for the same reason the arm is
                    // not: real code calls it through a dozen interfaces, and the class-word
                    // guard is what makes the read correct.
                    let is_get = (m.owner == "java/util/ArrayList" || m.owner == "java/util/List")
                        && m.name == "get"
                        && m.descriptor == "(I)Ljava/lang/Object;";
                    let is_size = m.name == "size" && m.descriptor == "()I";
                    let is_dget = m.name == "getDouble" && m.descriptor == "(I)D";
                    if is_get || is_size || is_dget {
                        uses = true;
                        break;
                    }
                }
            }
            pc += len;
        }
        // ⛔**AND THE RESOLVE ITSELF IS NOT FREE — MEASURED AT 13x.** Calling
        // `intern("java/util/ArrayList", "$elems")` at COMPILE time took
        // `bench/OpCost2.java`'s index loop from 3,357 to 43,000 ns with the inline arm still
        // DISABLED and all four gates bit-exact — i.e. merely asking for the id perturbs how
        // `$elems` resolves afterwards, globally. (`Vm::resolve_field_id` walks the owner's
        // hierarchy through `get_or_load`, and `java/util/ArrayList` has no bytecode in this
        // tree.) The same call for `java/util/BitSet` is invisible because almost nothing
        // touches BitSet; ArrayList is touched constantly.
        //
        // So the resolve happens only when the arm that needs it is armed. The default path
        // never makes the call and is byte-identical to before.
        // ⚠**AND THIS GATE IS WHY THE SIZE ARM FIRED ZERO TIMES.** `alist_get_inline_enabled()`
        // is opt-in, so `elems_fid` stayed 0 on every default run and every `$elems` arm refused
        // silently — including one written three files away that had nothing to do with `get`.
        // The 13x warning above is about the OLD resolve (`intern` at compile time, which
        // perturbed `$elems` globally); what runs now is a relaxed load of the id the natives
        // publish, which costs nothing, so a second arm may enable it on its own terms.
        if uses
            && (crate::jit::alist_get_inline_enabled()
                || crate::jit::alist_size_inline_enabled()
                || crate::jit::dalist_get_inline_enabled())
        {
            // A plain relaxed load of the id the NATIVES published — never the driver's
            // owner-qualified `intern`, which answers a different id and poisons `$elems`
            // resolution on the way (see `jit::ELEMS_FID`). Zero until the first ArrayList is
            // touched, and zero means the arm is simply not emitted, so this bootstraps itself.
            crate::jit::ELEMS_FID.load(std::sync::atomic::Ordering::Relaxed)
        } else {
            0
        }
    };

    // Borrow the thread-local shared module (lazily built). The guard restores it
    // V4.1 generic loop-BCE: PCs of array loads proven in-bounds by a counted loop
    // (`for i in 0..arr.length`), whose per-element bounds branch the emitter skips.
    let bce_safe = safe_bce_loads(bc);
    let nonnull_getfields = if nonnull_getfield_elision_enabled() {
        repeated_nonnull_getfields(bc)
    } else {
        std::collections::HashSet::new()
    };
    // Reference-array raw cells are admitted only for an exact `anewarray; pop`
    // site.  The plan is local to this Code body and remains empty by default.
    #[cfg(feature = "next-gen-gc")]
    let ref_arrays = analyze_arena_reference_arrays(code, pool);
    // on every exit path so emit bails keep the accumulated code.
    let mut guard = ModuleGuard {
        module: Some(
            SHARED_MODULE
                .with(|c| c.borrow_mut().take())
                .unwrap_or_else(build_shared_module),
        ),
    };
    let module = guard.module.as_mut().unwrap();

    // Pooled per compiler thread (see `CtxGuard`): the guard hands back a context that is
    // observationally a fresh one and re-parks it cleared on every exit path.
    let mut ctxg = CtxGuard::take(module, fast_compile2);
    let ctx = ctxg.ctx.as_mut().unwrap();
    let mut sig = module.make_signature();
    // Register-ABI arc (I1a): when compile_register set REG_ABI, the entry takes the
    // array-desc cell + the primitive value args in registers and returns the raw
    // primitive. Otherwise the byte-identical memory-locals ABI (locals ptr, array-base
    // cell) -> i64 bits.
    let reg_abi = cur_reg_abi();
    if let Some(r) = &reg_abi {
        sig.params.push(AbiParam::new(I64)); // array descriptor-base cell pointer (param 0)
        if r.has_recv {
            sig.params.push(AbiParam::new(I64)); // `this` (Ref cell), param 1, local slot 0
        }
        for k in &r.params {
            sig.params.push(AbiParam::new(k.cl_type()));
        }
        sig.returns.push(AbiParam::new(r.ret.cl_type()));
    } else {
        sig.params.push(AbiParam::new(I64)); // locals pointer
        sig.params.push(AbiParam::new(I64)); // array descriptor-base cell pointer
        sig.returns.push(AbiParam::new(I64)); // result bits
    }
    ctx.func.signature = sig.clone();
    // The function is declared just before `define_function` below: `func_id` is
    // never referenced by the emit, so declaring late keeps the shared module
    // untouched if the emit bails (no declared-but-undefined function to break a
    // later finalize).

    let poll_backedges = crate::jit::jit_backedge_poll_enabled();

    let Imports {
        gf_id,
        pf_id,
        #[cfg(feature = "next-gen-gc")]
        rsg_id,
        al_id,
        ld_id,
        gs_id,
        gsc_id,
        ps_id,
        new_id,
        iof_id,
        cast_id,
        npe_site_id,
        na_id,
        ana_id,
        dr_id,
        ldr_id,
        frem_id,
        drem_id,
        ias_id,
        das_id,
        ac_id,
        safepoint_id,
        #[cfg(feature = "next-gen-gc")]
        cm_id,
        #[cfg(feature = "next-gen-gc")]
        cur_id,
        #[cfg(feature = "next-gen-gc")]
        jca_id,
        #[cfg(feature = "next-gen-gc")]
        asp_id,
        #[cfg(feature = "next-gen-gc")]
        dpa_id,
        #[cfg(feature = "next-gen-gc")]
        dm_id,
        #[cfg(feature = "next-gen-gc")]
        ic_id,
        #[cfg(feature = "next-gen-gc")]
        hob_id,
        #[cfg(feature = "next-gen-gc")]
        hps_id,
        #[cfg(feature = "next-gen-gc")]
        dmm_id,
        #[cfg(feature = "next-gen-gc")]
        ce_id,
        #[cfg(feature = "next-gen-gc")]
        cx_id,
        #[cfg(feature = "next-gen-gc")]
        mp_id,
        #[cfg(feature = "next-gen-gc")]
        mg_id,
        #[cfg(feature = "next-gen-gc")]
        mr_id,
        #[cfg(feature = "next-gen-gc")]
        mc_id,
        #[cfg(feature = "next-gen-gc")]
        amg_id,
        #[cfg(feature = "next-gen-gc")]
        amc_id,
        #[cfg(feature = "next-gen-gc")]
        amp_id,
        #[cfg(feature = "next-gen-gc")]
        amr_id,
        #[cfg(feature = "next-gen-gc")]
        amb_id,
        #[cfg(feature = "next-gen-gc")]
        amd_id,
        #[cfg(feature = "next-gen-gc")]
        bi_id,
        #[cfg(feature = "next-gen-gc")]
        ui_id,
        #[cfg(feature = "next-gen-gc")]
        indy_id,
        #[cfg(feature = "next-gen-gc")]
        ldc_id,
        #[cfg(feature = "next-gen-gc")]
        sl_id,
        #[cfg(feature = "next-gen-gc")]
        sh_id,
        #[cfg(feature = "next-gen-gc")]
        ssw_id,
        #[cfg(feature = "next-gen-gc")]
        sca_id,
        af_id,
        alv_id,
        #[cfg(feature = "next-gen-gc")]
        bv_id,
        aas_id,
        inv_id,
        #[cfg(feature = "next-gen-gc")]
        resolve_reg_id,
        throw_id,
        #[cfg(feature = "next-gen-gc")]
        fu_inv_id,
        #[cfg(feature = "next-gen-gc")]
        deopt_id,
        #[cfg(feature = "next-gen-gc")]
        enum_ord_id,
        #[cfg(feature = "next-gen-gc")]
        bs_get_id,
        #[cfg(feature = "next-gen-gc")]
        bs_set_id,
        #[cfg(feature = "next-gen-gc")]
        bs_clear_id,
        #[cfg(feature = "next-gen-gc")]
        bs_isempty_id,
    } = declare_imports(module, poll_backedges)?;

    // Same pooling as the compile context, but with a stricter park rule — see the module-level
    // note: `clear`/`is_empty` are private to cranelift-frontend, so only `finalize()` can
    // return one of these to the empty state `FunctionBuilder::new` asserts on.
    let mut fb_ctx = if fast_compile2 {
        FBCTX_POOL
            .with(|c| c.borrow_mut().take())
            .unwrap_or_else(FunctionBuilderContext::new)
    } else {
        FunctionBuilderContext::new()
    };
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
    // ⭐**THE ARGS BUFFER IS A FEATURE, AND IN A CALL-FREE BODY THE FEATURE IS OFF.**
    //
    // `args_slot` is the marshalling area for the host-callback ABIs (`rava_invoke`, `rava_indy`,
    // the collection/iterator fast arms, `emit_direct_static`'s fallback), and it has always been
    // reserved unconditionally at `max_stack * 8` bytes — including in a body that never marshals
    // anything. Cranelift lays out every sized stack slot whether or not an instruction names it,
    // so those bytes are real frame on every entry.
    //
    // PROVED, not guessed: every writer of `env.args_slot` in the tree is reached from an
    // invoke-family opcode (`emit_invoke`/`emit_invoke_via`, `emit_direct_static`'s buffer arm,
    // the `collections`/`iterator_inline` arms behind `invokevirtual`/`invokeinterface`,
    // `lambda_mint` and `invoke_dynamic` behind `invokedynamic`) — and so is INLINING, which is
    // the one way a body other than this one can marshal into this slot. A body containing no
    // 0xb6..=0xba therefore cannot reach the slot at all, and one cell is enough to keep the
    // `StackSlotData` legal.
    //
    // The walk decodes instruction lengths rather than scanning raw bytes (an operand byte can
    // read as an opcode), and an UNDECODABLE body is treated as "marshals" — the conservative
    // answer is always available and costs only the old size.
    #[cfg(feature = "next-gen-gc")]
    let marshals_args = {
        let mut found = false;
        let mut pc = 0usize;
        while pc < bc.len() {
            let Some(len) = insn_len_at(bc, pc) else {
                found = true; // cannot decode ⇒ cannot rule the slot out
                break;
            };
            if (0xb6..=0xba).contains(&bc[pc]) || len == 0 {
                // `len == 0` would spin this walk forever; it is also "cannot decode".
                found = true;
                break;
            }
            pc += len;
        }
        found
    };
    // Folded onto the shared-callbuf switch on purpose: both changes are "the frame is smaller",
    // and one flag has to bisect the whole frame-size delta or the A/B measures two things.
    #[cfg(feature = "next-gen-gc")]
    let args_elide = !marshals_args && shared_callbuf_enabled();
    // The default (non-ngg) JIT keeps its codegen byte-identical: no scan, no elision.
    #[cfg(not(feature = "next-gen-gc"))]
    let args_elide = false;
    let args_slot = b.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        if args_elide { 8 } else { (code.max_stack as u32).max(1) * 8 },
        3, // 8-byte align (align_shift)
    ));
    // The function's ONE pair of dispatch call buffers, created lazily by the first emit site that
    // asks for one (see `CallBufs`). Declared before `env` so it outlives the borrow.
    #[cfg(feature = "next-gen-gc")]
    let callbufs = CallBufs::new();
    let Hoists {
        licm_pairs,
        #[cfg(feature = "next-gen-gc")]
        promo_written,
        #[cfg(feature = "next-gen-gc")]
        array_fields,
        #[cfg(feature = "next-gen-gc")]
        bare_allowed,
    } = prepare_hoists(
        bc,
        code,
        &fields,
        pool,
        inline_targets,
        static_intrinsics,
        #[cfg(feature = "next-gen-gc")]
        tagged_fields,
    );
    // The deopt type map, computed once per compile and borrowed by the emitter. Only used when
    // a deopt site is actually emitted; computing it is a linear walk of the body.
    #[cfg(feature = "next-gen-gc")]
    let lt_owned = if crate::jit::deopt_enabled() {
        // No provable static flag → no map → no deopt sites. Refusing is always available.
        method_is_static(class_ptr, code)
            .and_then(|is_static| crate::jit::analysis::localtypes::compute(code, descriptor, is_static))
    } else {
        None
    };
    let env = Env {
        #[cfg(feature = "next-gen-gc")]
        local_types: lt_owned.as_ref(),
        #[cfg(feature = "next-gen-gc")]
        ret_jt: crate::jit::ret_jt(descriptor),
        safepoint: safepoint_id.map(|id| module.declare_func_in_func(id, b.func)),
        poll_backedges,
        getfield: module.declare_func_in_func(gf_id, b.func),
        putfield: module.declare_func_in_func(pf_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        ring_store_guard: module.declare_func_in_func(rsg_id, b.func),
        arrlen: module.declare_func_in_func(al_id, b.func),
        aload: module.declare_func_in_func(ld_id, b.func),
        invoke: module.declare_func_in_func(inv_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        resolve_reg: module.declare_func_in_func(resolve_reg_id, b.func),
        throw: module.declare_func_in_func(throw_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        fu_invoke: module.declare_func_in_func(fu_inv_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        deopt: module.declare_func_in_func(deopt_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        enum_ordinal: module.declare_func_in_func(enum_ord_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        bitset_get: module.declare_func_in_func(bs_get_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        bitset_set: module.declare_func_in_func(bs_set_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        bitset_clear: module.declare_func_in_func(bs_clear_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        bitset_isempty: module.declare_func_in_func(bs_isempty_id, b.func),
        getstatic: module.declare_func_in_func(gs_id, b.func),
        getstatic_cached: module.declare_func_in_func(gsc_id, b.func),
        putstatic: module.declare_func_in_func(ps_id, b.func),
        new_obj: module.declare_func_in_func(new_id, b.func),
        instanceof: module.declare_func_in_func(iof_id, b.func),
        checkcast: module.declare_func_in_func(cast_id, b.func),
        npe_invoke_site: module.declare_func_in_func(npe_site_id, b.func),
        newarray: module.declare_func_in_func(na_id, b.func),
        anewarray: module.declare_func_in_func(ana_id, b.func),
        i32_divrem: module.declare_func_in_func(dr_id, b.func),
        i64_divrem: module.declare_func_in_func(ldr_id, b.func),
        f32_rem: module.declare_func_in_func(frem_id, b.func),
        f64_rem: module.declare_func_in_func(drem_id, b.func),
        iastore: module.declare_func_in_func(ias_id, b.func),
        dastore: module.declare_func_in_func(das_id, b.func),
        acmp: module.declare_func_in_func(ac_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        card_mark: module.declare_func_in_func(cm_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        cursor_addr_fn: module.declare_func_in_func(cur_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        arena_safepoint_fn: module.declare_func_in_func(asp_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        arena_deopt_alloc_fn: module.declare_func_in_func(dpa_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        dispatch_miss: module.declare_func_in_func(dm_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        ic_base_fn: module.declare_func_in_func(ic_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        objects_base_fn: module.declare_func_in_func(hob_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        heap_shadow_fn: module.declare_func_in_func(hps_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        direct_miss_fn: module.declare_func_in_func(dmm_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        cells_slow: module.declare_func_in_func(ce_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        ctx_addr_fn: module.declare_func_in_func(jca_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        box_exit: module.declare_func_in_func(cx_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        deep,
        #[cfg(feature = "next-gen-gc")]
        map_put_fn: module.declare_func_in_func(mp_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        map_get_fn: module.declare_func_in_func(mg_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        map_remove_fn: module.declare_func_in_func(mr_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        map_contains_fn: module.declare_func_in_func(mc_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        amap_get_fn: module.declare_func_in_func(amg_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        amap_default_fn: module.declare_func_in_func(amd_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        amap_contains_fn: module.declare_func_in_func(amc_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        amap_put_fn: module.declare_func_in_func(amp_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        amap_remove_fn: module.declare_func_in_func(amr_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        amap_box_value_fn: module.declare_func_in_func(amb_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        box_int_fn: module.declare_func_in_func(bi_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        unbox_int_fn: module.declare_func_in_func(ui_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        indy: module.declare_func_in_func(indy_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        ldc: module.declare_func_in_func(ldc_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        str_length_fn: module.declare_func_in_func(sl_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        str_hash_fn: module.declare_func_in_func(sh_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        str_startswith_fn: module.declare_func_in_func(ssw_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        str_charat_fn: module.declare_func_in_func(sca_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        new_arena,
        #[cfg(feature = "next-gen-gc")]
        ctor_inlines,
        #[cfg(feature = "next-gen-gc")]
        concat_shapes,
        #[cfg(feature = "next-gen-gc")]
        hof_fusions,
        #[cfg(feature = "next-gen-gc")]
        code_key: code as *const Code as usize,
        #[cfg(feature = "next-gen-gc")]
        active_sam: None,
        array_fault: module.declare_func_in_func(af_id, b.func),
        aload_verify: module.declare_func_in_func(alv_id, b.func),
        #[cfg(feature = "next-gen-gc")]
        bare_verify: module.declare_func_in_func(bv_id, b.func),
        aastore: module.declare_func_in_func(aas_id, b.func),
        class_ptr,
        args_slot,
        #[cfg(feature = "next-gen-gc")]
        in_splice: false,
        #[cfg(feature = "next-gen-gc")]
        callbufs: &callbufs,
        fields: &fields,
        #[cfg(feature = "next-gen-gc")]
        array_fields: &array_fields,
        // ⭐**A NATIVE-BACKED CLASS GETS NO HOISTED FIELD PATHS EITHER.** `callback_fields`
        // routes `getfield`/`putfield` to the host accessor, but FIELD-LICM and the bare-ref
        // flip read slots DIRECTLY, bypassing it — and a class whose data the natives keep
        // outside the field table has no such slot. That is the observed crash: `read of
        // 0x100000000`, which is an arena HEADER (`nfields = 1`, `class_id = 0`) used as a
        // pointer — a slot read that never applied the `HDR` offset because the layout it
        // indexed is not where the values live.
        #[cfg(feature = "next-gen-gc")]
        bare_allowed: bare_allowed && !callback_fields,
        callback_fields,
        static_intrinsics,
        inline_targets,
        statics,
        invariant_fields: field_loads_invariant(bc, inline_targets, static_intrinsics),
        no_alloc: method_no_alloc(bc, inline_targets, static_intrinsics),
        #[cfg(feature = "next-gen-gc")]
        licm_pairs: if callback_fields { &[] } else { &licm_pairs },
        #[cfg(feature = "next-gen-gc")]
        promo_written: promo_written.as_ref(),
        #[cfg(feature = "next-gen-gc")]
        elems_fid,
        bits_fid,
        bce_safe: &bce_safe,
        nonnull_getfields: &nonnull_getfields,
        #[cfg(feature = "next-gen-gc")]
        prim_arrays,
        #[cfg(feature = "next-gen-gc")]
        ref_arrays: &ref_arrays,
        prim_element_ids: crate::heap::prim_element_ids().map(|v| v as i64),
        // Static-LICM: an invariant method (no putstatic/putfield/alloc/non-inlined
        // call → no callback can mutate a static or GC mid-call) with a LOOP can
        // read its statics from the value cells as `readonly` loads after a one-time
        // entry populate — the egraph then hoists the loop-invariant statics out
        // (what C2 does), collapsing repeated getstatic to near-free. Gated so the
        // default (next-gen-gc OFF) JIT stays byte-identical.
        static_licm: {
            #[cfg(feature = "next-gen-gc")]
            {
                field_loads_invariant(bc, inline_targets, static_intrinsics)
                    && has_back_edge(bc)
                    && !statics.is_empty()
            }
            #[cfg(not(feature = "next-gen-gc"))]
            {
                false
            }
        },
    };
    let ForkOutcome { fold, parallel_done } = plan_and_emit_fork(
        &mut b, module, bc, ret, class_ptr, pool, new_arena, ctor_inlines, &fields,
        getter_inlines,
    );

    // Register-ABI arc (I1a): a register entry must return the raw primitive matching its
    // signature, so it can only come from build_body (the fold/parallel emitters return
    // i64 bits). compile_register only sets REG_ABI for a pure leaf, which is never a
    // fold/parallel candidate anyway — this just makes the invariant explicit + safe.
    #[cfg(feature = "next-gen-gc")]
    let ok = if cur_reg_abi().is_some() {
        build_body(&mut b, bc, code.max_locals as usize, &leaders, pool, &env).is_some()
    } else {
        match parallel_done {
            Some(v) => v,
            None => match &fold {
                Some(plan) => build_folded_body(&mut b, bc, code.max_locals as usize, pool, &env, plan).is_some(),
                None => build_body(&mut b, bc, code.max_locals as usize, &leaders, pool, &env).is_some(),
            },
        }
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let ok = build_body(&mut b, bc, code.max_locals as usize, &leaders, pool, &env).is_some();
    // Deep-IC v3: only a body emitted by build_body carries the self-registering
    // prologue — folded/parallel emitters never do, whatever `deep` says.
    #[cfg(feature = "next-gen-gc")]
    let self_reg = deep.is_some() && parallel_done.is_none() && fold.is_none();
    #[cfg(not(feature = "next-gen-gc"))]
    let self_reg = false;
    if ok {
        compile_trace.note(b.func, "body.done");
        if !crate::jit::ir_budget::allows(b.func) {
            compile_trace.note(b.func, "budget.body.refused");
            drop(b);
            module.clear_context(&mut *ctx);
            crate::jit::compile_bail::note("ir_budget");
            return None;
        }
        b.finalize();
        compile_trace.note(&ctx.func, "finalize.done");
        if !crate::jit::ir_budget::allows(&ctx.func) {
            compile_trace.note(&ctx.func, "budget.finalize.refused");
            module.clear_context(&mut *ctx);
            crate::jit::compile_bail::note("ir_budget");
            return None;
        }
        // `finalize()` has just cleared it — the only public route back to empty — so this is
        // the ONE path on which the builder scratch may be parked. Its SSA maps, sized for the
        // method just emitted, then serve the next one without re-growing from zero.
        if fast_compile2 {
            FBCTX_POOL.with(|p| *p.borrow_mut() = Some(fb_ctx));
        }
    } else {
        // Refusal: `b` is dropped without finalizing, so `fb_ctx` still holds this method's SSA
        // state. It is dropped with the frame, exactly as before — never parked.
        std::mem::drop(b);
    }
    if !ok {
        let stage = if !crate::jit::ir_budget::allows(&ctx.func) {
            compile_trace.note(&ctx.func, "budget.emit.refused");
            "ir_budget"
        } else {
            "emit_body"
        };
        module.clear_context(&mut *ctx);
        // Counted unconditionally: the `bail!` below is inside `dbg_bail`, so leaving the tally to
        // it would have made `emit_body` read zero on every ordinary run — the one refusal a
        // debug-gated census cannot see.
        crate::jit::compile_bail::note(stage);
        if dbg_bail {
            bail!("emit_body");
        }
        return None;
    }

    let done = finish_compile(module, &mut *ctx, &sig, class_ptr, descriptor, ret, self_reg);
    compile_trace.note(&ctx.func, "codegen.done");
    if done.is_some() {
        crate::jit::compile_bail::note_ok();
    }
    done
}
