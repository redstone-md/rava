//! Turning an emitted function into a callable pointer.
//!
//! Everything after the last point a compile can bail: declare, define, price the compile,
//! optionally dump CLIF and machine code, finalize, and hand back the JitFn.

use crate::jit::*;

/// Cumulative nanoseconds inside `finalize_definitions`, reported beside the compile total.
pub(crate) static FINALIZE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How `RAVA_JIT_DUMP` is answered for the life of the process.
///
/// **SHAPE BEFORE:** `finish_compile` ran a raw `std::env::var("RAVA_JIT_DUMP")` on EVERY
/// compiled method — ~1,500 a product boot, and the last raw per-compile environment read left
/// in the JIT. On Windows `std::env::var*` is `RtlQueryEnvironmentVariable`: the key is UTF-16
/// encoded into a fresh heap buffer, the whole process environment block is walked under a lock,
/// and a hit is decoded back into an owned `String`. All of it for a CLIF/VCode dump that is
/// unset in every run nobody is debugging.
///
/// **SHAPE AFTER:** one acquire load of this cell per compile, then a null test. Nothing is
/// walked and nothing is allocated when the dump is off, and the class-name `to_string()` that
/// the old expression reached for is never built.
///
/// **SOUND** for the reason `jit::env_gate` shipped on (min −7.4% on blocks A+B): the only
/// writer of this process's environment is the CLI's config apply, which completes before the
/// VM — and therefore before any method can compile — exists, so the answer is a process
/// constant. `env_gate` itself cannot serve this site: it answers a `bool` and this gate carries
/// a SUBSTRING, so the memo has to hold the string.
#[cfg(feature = "next-gen-gc")]
enum DumpFilter {
    /// Default: the substring as it stood the first time a method finished compiling.
    Memo(Option<Box<str>>),
    /// `RAVA_NO_FAST_FINISH=1` (or the crate-wide `RAVA_NO_EMIT_MORE`): the legacy shape, one
    /// environment walk per compiled method, so an A/B measures the SHAPE and nothing else.
    Raw,
}

#[cfg(feature = "next-gen-gc")]
#[inline]
fn dump_filter() -> &'static DumpFilter {
    static F: std::sync::OnceLock<DumpFilter> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        if std::env::var_os("RAVA_NO_FAST_FINISH").is_some() || crate::flags::emit_more_disabled() {
            return DumpFilter::Raw;
        }
        DumpFilter::Memo(raw_dump_want())
    })
}

/// The one place the environment is actually read: once at first compile (default), or once per
/// compile in the kill-switched arm.
#[cfg(feature = "next-gen-gc")]
fn raw_dump_want() -> Option<Box<str>> {
    std::env::var("RAVA_JIT_DUMP")
        .ok()
        .filter(|w| !w.is_empty())
        .map(String::into_boxed_str)
}

/// The compiling method's class name iff it matches the dump filter — i.e. `None` in every run
/// that is not dumping, reached without touching the environment or the heap.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn dump_target(class_ptr: i64) -> Option<String> {
    let want: &str = match dump_filter() {
        DumpFilter::Memo(w) => w.as_deref()?,
        DumpFilter::Raw => return raw_dump_want().and_then(|w| match_class(class_ptr, &w)),
    };
    match_class(class_ptr, want)
}

#[cfg(feature = "next-gen-gc")]
fn match_class(class_ptr: i64, want: &str) -> Option<String> {
    // A bailed/synthetic compile can carry a null class pointer (the `RAVA_VERIFY_LOADS` tracer in
    // `driver` guards for exactly that), and this arm only ever runs while dumping, so refuse
    // rather than deref it.
    if class_ptr == 0 {
        return None;
    }
    // SAFETY: the caller compiled from this `&LoadedClass`; it outlives the compile.
    let cls = unsafe { &*(class_ptr as *const crate::class_loader::LoadedClass) };
    let cn = cls.file.this_class.to_string();
    if cn.contains(want) { Some(cn) } else { None }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_compile(
    module: &mut JITModule,
    mut ctx: &mut cranelift::codegen::Context,
    sig: &cranelift::codegen::ir::Signature,
    class_ptr: i64,
    descriptor: &str,
    ret: Option<JT>,
    self_reg: bool,
) -> Option<JitFn> {
    // DIAGNOSTIC (RAVA_JIT_DUMP=substr): dump the CLIF (pre-compile IR) AND the final
    // machine code (VCode) for a compiled method whose class name contains the substring.
    // Purely diagnostic — no control-flow effect on non-dumped methods, so hashes converge
    // bit-for-bit. Isolate a single method by grepping clif.txt for its `.{descriptor}` header.
    //
    // The gate is now `dump_target` (see `DumpFilter`): one acquire load per compile instead of
    // an environment-block walk plus a heap allocation per compile. Kill switch
    // `RAVA_NO_FAST_FINISH=1`.
    #[cfg(feature = "next-gen-gc")]
    let dump_match: Option<String> = dump_target(class_ptr);
    #[cfg(feature = "next-gen-gc")]
    if let Some(cn) = &dump_match {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("clif.txt") {
            let _ = writeln!(f, "==== CLIF {cn}.{descriptor} ====\n{}\n==== END ====", ctx.func.display());
            let bc = block_census::report();
            if !bc.is_empty() {
                let _ = writeln!(f, "==== BLOCK-CENSUS {bc} ====");
            }
        }
        ctx.set_disasm(true); // request VCode/machcode capture for the define below
    }

    // Declare now (after the emit can no longer bail) as an anonymous function so
    // many compiled methods coexist in the one shared module.
    let func_id = module.declare_anonymous_function(&sig).ok()?;
    let compile_t0 = std::time::Instant::now();
    module.define_function(func_id, &mut ctx).ok()?;
    #[cfg(feature = "next-gen-gc")]
    crate::jit::block_census::note_method_clif_insts(ctx.func.dfg.num_insts() as u64);
    // Code-size / compile-time census (see `code_size_stats`): two clock reads per
    // compiled method against a Cranelift compile — unmeasurable, and the only way
    // to price an unswitch's doubled body without fighting the wall-clock noise.
    //
    // RE-CHECKED 2026-09-02 and the argument still holds, so this is DELIBERATELY LEFT ALONE.
    // Priced with this round's own reckoning rule (a term's share of boot, not of its own
    // region): the whole census here is 2 `Instant` reads and 4 relaxed atomic RMWs
    // (COMPILE_NS / COMPILED_FNS / CODE_BYTES / FINALIZE_NS) ≈ 200 ns, on a population of
    // ~1,500 compiles ⇒ ~0.3 ms of a ~28.9 s boot = ~0.001%. The counters it feeds are what
    // priced compilation at 2.23 s of a boot and `finalize_definitions` at 0.4% of that — the
    // measurements that closed two redesigns. Removing them buys nothing and costs the evidence.
    // The env read above was a different animal entirely: a syscall-shaped env-block walk plus
    // an allocation, for output nobody consumes.
    COMPILE_NS.fetch_add(
        compile_t0.elapsed().as_nanos() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    COMPILED_FNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut code_len: u32 = 0;
    if let Some(cc) = ctx.compiled_code() {
        code_len = cc.code_info().total_size;
        CODE_BYTES.fetch_add(u64::from(code_len), std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(feature = "next-gen-gc")]
    if let Some(cn) = &dump_match {
        use std::io::Write as _;
        if let Some(vcode) = ctx.compiled_code().and_then(|cc| cc.vcode.as_ref()) {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("clif.txt") {
                let _ = writeln!(f, "==== MACHCODE {cn}.{descriptor} ====\n{}\n==== END ====", vcode);
            }
        }
    }
    // PHASE-A STACK MAPS (`RAVA_STACKMAPS=1`). This is the last point the `CompiledCode` is
    // alive — `clear_context` below drops it — and the finalized address is only known after
    // `finalize_definitions`, so the maps are TAKEN here and registered against the code
    // range a few lines down. `MachBufferFinalized::user_stack_maps()` is
    // `cranelift-codegen-0.132.1/src/machinst/buffer.rs:1918` and yields
    // `(return-address code offset, FrameLayout::active_size(), UserStackMap)`.
    #[cfg(feature = "next-gen-gc")]
    let smaps: Option<Vec<(u32, u32, cranelift::codegen::ir::UserStackMap)>> =
        if crate::jit::runtime::stackmaps::enabled() {
            // How many safepoints the function HAS, against how many got a map. Cranelift
            // records a map only where a declared value was live across the call, so the
            // difference is "safepoints Cranelift determined had nothing to root" — the
            // number that decides whether an absent map at a return address is a hole or an
            // answer. `ctx.func` is the post-`SafepointSpiller` CLIF and is still intact
            // here; `clear_context` below is what drops it.
            let mut sp = 0u64;
            for blk in ctx.func.layout.blocks() {
                for inst in ctx.func.layout.block_insts(blk) {
                    if ctx.func.dfg.insts[inst].opcode().is_safepoint() {
                        sp += 1;
                    }
                }
            }
            crate::jit::runtime::stackmaps::note_safepoints(sp);
            ctx.compiled_code().map(|cc| cc.buffer.user_stack_maps().to_vec())
        } else {
            None
        };
    module.clear_context(&mut ctx);
    // ⛔**MEASURED, AND IT IS NOT THE COST: 0.01s of 2.23s over 1000 compiles = 0.4%.**
    //
    // `finalize_definitions` is not incremental — it re-protects every region the module owns —
    // so it looked like the term that grows with the number of methods already compiled, and a
    // batching redesign was about to be built around it. It is noise. The `ArenaMemoryProvider`
    // weight in the collapsed profiles is code-space ALLOCATION inside `define_function`, i.e.
    // compiling itself, not protection.
    //
    // The timer stays because it is the evidence, and it costs one `Instant::now()` per compile
    // (~1200 a boot).
    let __fin = std::time::Instant::now();
    module.finalize_definitions().ok()?;
    FINALIZE_NS.fetch_add(
        __fin.elapsed().as_nanos() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    let ptr = module.get_finalized_function(func_id);
    #[cfg(feature = "next-gen-gc")]
    if let Some(m) = smaps {
        crate::jit::runtime::stackmaps::register(ptr as usize, code_len as usize, &m);
    }
    Some(JitFn { ptr, ret, self_reg, reg: None, mid: 0, code_len })
}
