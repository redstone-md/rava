//! The shared JITModule and the guard that puts it back on every exit path.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

pub(crate) fn build_shared_module() -> JITModule {
    let mut flag_builder = settings::builder();
    flag_builder.set("use_colocated_libcalls", "false").unwrap();
    flag_builder.set("is_pic", "false").unwrap();
    // opt_level "speed" (Cranelift's own default is "none"): better regalloc/isel toward C2.
    //
    // `RAVA_JIT_OPT_LEVEL=none|speed|speed_and_size` overrides it; default unchanged, so an
    // ordinary build is byte-identical. The knob exists because this setting is a TRADE and only
    // one side of it has ever been measured. Cranelift compile time is ~4.5s of the boot's 54s
    // SERIAL critical path (RAVA_COMPILE_TIME: 6.37s over 2000 compiles on an inflated run,
    // ~4.5s adjusted), and the boot COMPILES ONCE AND MOVES ON — most of those ~2000 bodies run a
    // handful of times, so paying "speed" codegen for them is pure serial cost with nothing to
    // amortise it. The ring is the opposite case: a few bodies re-entered millions of times, where
    // "speed" is exactly right. Whether the boot as a whole is better off is an empirical question
    // this could not previously ask.
    //
    // ASKED AND ANSWERED: "none" IS NOT A WIN. Two interleaved boot A/Bs disagree in SIGN, which is
    // what noise looks like:
    //
    //   batch 1 (6 runs, one flagged contended): excluding the bad run every ON beat every OFF -
    //           but that left n=2 on the OFF side, so it was never a result.
    //   batch 2 (8 runs, none flagged): off 25.304/25.701/27.185/51.420
    //                                   on  26.112/27.156/27.253/50.434  => +3.19%, OVERLAP,
    //           and dropping the two ~50s outliers (which pair with the two elevated walls) leaves
    //           ON consistently ~3% WORSE.
    //
    // A lever whose two batches point opposite ways has no effect above this workload's noise
    // floor. The default stays "speed". The knob stays because the question was worth asking and
    // the answer is worth not re-deriving.
    //
    // The RingScan probe that motivated this (setup 49971 vs 51782, ring unchanged) was ONE run per
    // arm. Single runs have now produced two false positives here - this, and the inline-depth
    // "-15%" that three reps dissolved.
    // `RAVA_JIT_OPT_NONE` is the flag-shaped alias, so the standard on/off boot A/B harness can
    // drive this directly instead of needing a bespoke runner for a valued setting.
    let opt = if std::env::var_os("RAVA_JIT_OPT_NONE").is_some() {
        "none".to_string()
    } else {
        std::env::var("RAVA_JIT_OPT_LEVEL").unwrap_or_else(|_| "speed".to_string())
    };
    let opt = match opt.trim() {
        "none" => "none",
        "speed_and_size" => "speed_and_size",
        _ => "speed",
    };
    flag_builder.set("opt_level", opt).unwrap();
    // ⭐**`regalloc2::ion` IS THE SINGLE BIGGEST NAMED ROW OF THE COMPILER, AND IT HAS ITS OWN
    // FLAG — WHICH IS NOT `opt_level`.**
    //
    // Phase A profile, three rows, one thread at ~99%: `regalloc2::ion` 1.06 + 0.94 + 0.82 =
    // **2.82% of all samples**, against **4.1% of samples inside emitted machine code at all**.
    // Register allocation is the dominant phase of Cranelift and the boot puts ~1,950 bodies
    // through it on the critical path.
    //
    // Cranelift 0.132 exposes `regalloc_algorithm`, which is a DIFFERENT knob from `opt_level`
    // (measured above and settled: "none" is not a win, "speed_and_size" is within noise) —
    // `opt_level` steers the mid-end, this picks the allocator itself:
    //
    //   backtracking (default, what we ship) -> regalloc2 `Algorithm::Ion`: range splitting,
    //                                           more expensive, better code — the profiled row.
    //   single_pass                          -> regalloc2 `Algorithm::Fastalloc`: one pass,
    //                                           quick, more spills and moves in the result.
    //
    // ⚠**IT IS OPT-IN, AND IT HAS TO BE, BECAUSE THE PRECONDITION IS NOT PROVABLE HERE.** The
    // trade only pays for a body that is compiled and then executed a handful of times; it loses
    // on the ring, where a few bodies are re-entered millions of times. Choosing per body would
    // need the flag per FUNCTION, and it is per ISA — one `TargetIsa` per `JITModule`, and this
    // engine deliberately runs ONE module because compiled bodies call each other by direct
    // symbol (see `ModuleGuard` below: rotating modules broke `ringscan`/`ringlane`/`ringlanei`
    // in one run). A second module for cold methods is the same linkage work that note describes,
    // not a flag change. Deciding by execution count is the tier threshold, and both sides of the
    // default have been measured collapsing the boot (8192 -> 336 s, 32 -> 400 s); deciding by
    // measured time is forbidden as a routing input.
    //
    // So what ships is the default, unchanged and byte-identical, plus the lever a boot A/B can
    // pull: `RAVA_JIT_REGALLOC_FAST=1`. The analysis is here so the next round starts from the
    // flag rather than rediscovering it.
    if std::env::var_os("RAVA_JIT_REGALLOC_FAST").is_some() {
        flag_builder.set("regalloc_algorithm", "single_pass").unwrap();
    }
    // ⭐**CRANELIFT'S VERIFIER DEFAULTS TO ON, AND WE WERE PAYING IT IN PRODUCTION.** It
    // re-typechecks every function the JIT emits — a self-check for compiler development, not
    // something a shipped build needs. Measured on a boot with the compile threshold lowered
    // (which makes the compiler itself the profile): `cranelift_codegen::verifier::Verifier::run`
    // at **4.3% of all samples**, beside `regalloc2::ion::run` at 7.0%.
    //
    // `RAVA_JIT_VERIFY=1` puts it back — it is exactly the switch you want when a codegen change
    // starts producing wrong machine code.
    if std::env::var_os("RAVA_JIT_VERIFY").is_none() {
        flag_builder.set("enable_verifier", "false").unwrap();
    }
    // R1 (Flat-Floor): reserve R15 as Cranelift's pinned register (x86_64 PINNED_REG=R15;
    // excluded from the regalloc MachineEnv, made non-callee-save, on both SysV and Windows
    // fastcall). INERT PROTOTYPE gate RAVA_R1_PINNED_CONTEXT=1: reserves the register WITHOUT
    // emitting any get/set_pinned_reg use, so computation is byte-identical (regalloc merely
    // loses one GP reg → possibly more spilling) — validates determinism + measures the
    // register-pressure cost before any WorkerContext-in-R15 logic is wired.
    //
    // ⛔**AND IT MUST STAY INERT: THE PROTOTYPE CANNOT BE WIRED FROM THE COMPILED SIDE.** The
    // attractive move — have the deep prologue `set_pinned_reg` the `JitThreadCtx` address it
    // already loads, then let every emitter and eventually every callback READ R15 instead of
    // re-deriving the pointer — is unsound here, and the reason is in Cranelift's own ABI, not in
    // our emission. In `cranelift-codegen-0.132.1`, `isa/x64/abi.rs`:
    //
    //     fn is_callee_save_systemv(r, enable_pinned_reg)  { .. R15 => !enable_pinned_reg .. }
    //     fn is_callee_save_fastcall(r, enable_pinned_reg) { .. R15 => !enable_pinned_reg .. }
    //
    // i.e. turning the flag on does not merely take R15 out of the allocator's pool, it also takes
    // it out of the **callee-save set** — on Windows fastcall, which is our ABI, and where R15 is
    // callee-saved by the platform contract. A body that writes R15 therefore emits no save and no
    // restore, and every Rust frame below it on the stack that LLVM happened to give a live value
    // in R15 is silently corrupted on return. That is a wrong-answer bug with no fault to catch it.
    //
    // Making it sound needs the WRITE to happen at the Rust→compiled seam, not inside the body:
    // an asm shim that stores R15, establishes the ctx, calls the compiled entry and restores R15,
    // with compiled code only ever READING the register. The engine has eight such edges and none
    // of them is in this crate's JIT-runtime layer — `jit/env.rs` (`call_raw`),
    // `jit/model/handles.rs`, `jit/callbacks/dispatch.rs` (two), `jit/emit/cap_thunk.rs` (two),
    // `vm/aot_link.rs`, plus the `jit/parallel/*` body entries. The alternative — restoring inside
    // the body — has to cover ~40 `return_` emission sites across 14 files, and MISSING ONE is the
    // same silent corruption. Neither is a change to this file, and neither is a change to the
    // prologue alone; both are cross-cutting linkage work that has to land as one piece.
    //
    // WHAT WAS DONE INSTEAD, because the underlying observation is right: the value a pinned
    // register would carry is a per-thread CONSTANT (the `Box<JitThreadCtx>` is created on first
    // use and never replaced or moved), so it is memoised in software, one word inside the
    // thread-local cluster the crossing already loads — see `frames::ctx_addr` and
    // `frames::Locals::ctx_ptr`. That removes the `RefCell` borrow/release pair and the `Option`
    // branch from every warm compiled dispatch without touching the ABI at all.
    //
    // `RAVA_NO_PINNED_CTX=1` (and the block switch `RAVA_NO_EMIT_MORE=1`) also suppress the
    // reservation, so one variable turns the whole pinned-context line off in a single run.
    if std::env::var_os("RAVA_R1_PINNED_CONTEXT").is_some()
        && std::env::var_os("RAVA_NO_PINNED_CTX").is_none()
        && !crate::flags::emit_more_disabled()
    {
        flag_builder.set("enable_pinned_reg", "true").unwrap();
    }
    let isa = cranelift_native::builder()
        .unwrap()
        .finish(settings::Flags::new(flag_builder))
        .unwrap();
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    // Arena memory provider: one up-front PROT_NONE reservation that code segments
    // are carved out of, instead of the default SystemMemoryProvider's fresh mmaps
    // per finalize (whole pages per method across 3 pools — RSS fragmentation).
    // Pages enter the working set only when code is actually written, so resident
    // memory tracks real code size (~KB/method). If the arena ever fills, segment
    // allocation fails cleanly and further methods just stay interpreted.
    if let Ok(arena) = cranelift_jit::ArenaMemoryProvider::new_with_size(256 << 20) {
        builder.memory_provider(Box::new(arena));
    }
    #[cfg(feature = "next-gen-gc")]
    let (getfield_callback, putfield_callback) = if field_repr_census_enabled() {
        (
            rava_getfield_census as *const u8,
            rava_putfield_census as *const u8,
        )
    } else {
        (rava_getfield as *const u8, rava_putfield as *const u8)
    };
    #[cfg(not(feature = "next-gen-gc"))]
    let (getfield_callback, putfield_callback) =
        (rava_getfield as *const u8, rava_putfield as *const u8);
    builder.symbol("rava_getfield", getfield_callback);
    builder.symbol("rava_putfield", putfield_callback);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_ring_store_guard", rava_ring_store_guard as *const u8);
    builder.symbol("rava_arraylength", rava_arraylength as *const u8);
    builder.symbol(
        "rava_cap_cell_slow",
        crate::jit::emit::cap_thunk::rava_cap_cell_slow as *const u8,
    );
    builder.symbol("rava_aload", rava_aload as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_array_base_len", rava_array_base_len as *const u8);
    builder.symbol("rava_invoke", rava_invoke as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_resolve_reg", rava_resolve_reg as *const u8);
    builder.symbol("rava_throw", rava_throw as *const u8);
    builder.symbol("rava_getstatic", rava_getstatic as *const u8);
    builder.symbol("rava_getstatic_cached", rava_getstatic_cached as *const u8);
    builder.symbol("rava_putstatic", rava_putstatic as *const u8);
    builder.symbol("rava_new", rava_new as *const u8);
    builder.symbol("rava_instanceof", rava_instanceof as *const u8);
    builder.symbol("rava_checkcast", rava_checkcast as *const u8);
    builder.symbol("rava_npe_invoke_site", rava_npe_invoke_site as *const u8);
    builder.symbol("rava_newarray", rava_newarray as *const u8);
    builder.symbol("rava_anewarray", rava_anewarray as *const u8);
    builder.symbol("rava_i32_divrem", rava_i32_divrem as *const u8);
    builder.symbol("rava_i64_divrem", rava_i64_divrem as *const u8);
    builder.symbol("rava_f32_rem", rava_f32_rem as *const u8);
    builder.symbol("rava_f64_rem", rava_f64_rem as *const u8);
    builder.symbol("rava_iastore", rava_iastore as *const u8);
    builder.symbol("rava_dastore", rava_dastore as *const u8);
    builder.symbol("rava_acmp", rava_acmp as *const u8);
    builder.symbol("rava_jit_safepoint", rava_jit_safepoint as *const u8);
    builder.symbol("rava_array_fault", rava_array_fault as *const u8);
    builder.symbol("rava_aload_verify", rava_aload_verify as *const u8);
    builder.symbol("rava_aastore", rava_aastore as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_card_mark", rava_card_mark as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_jit_cursor_addr", rava_jit_cursor_addr as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_arena_safepoint", rava_arena_safepoint as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_arena_deopt_alloc", rava_arena_deopt_alloc as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_dispatch_miss", rava_dispatch_miss as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_ic_base", rava_ic_base as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_heap_objects_base", rava_heap_objects_base as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_heap_pic_shadow", rava_heap_pic_shadow as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_direct_miss", rava_direct_miss as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_fu_invoke", rava_fu_invoke as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol(
        "rava_deopt",
        crate::jit::callbacks::deopt::rava_deopt as *const u8,
    );
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_enum_ordinal", rava_enum_ordinal as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_cells_push_slow", rava_cells_push_slow as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_jit_ctx_addr", rava_jit_ctx_addr as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_box_exit", rava_box_exit as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_bitset_get", rava_bitset_get as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_bitset_set", rava_bitset_set as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_bitset_clear", rava_bitset_clear as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_bitset_isempty", rava_bitset_isempty as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_map_put", rava_map_put as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_map_get", rava_map_get as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_map_remove", rava_map_remove as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_map_contains", rava_map_contains as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_amap_get", rava_amap_get as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_amap_default", rava_amap_default as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_amap_contains", rava_amap_contains as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_amap_put", rava_amap_put as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_amap_remove", rava_amap_remove as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_amap_box_value", rava_amap_box_value as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_box_int", rava_box_int as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_unbox_int", rava_unbox_int as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_str_startswith", rava_str_startswith as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_parallel_reduce", rava_parallel_reduce as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_parallel_map", rava_parallel_map as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_parallel_scan", rava_parallel_scan as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_parallel_filter", rava_parallel_filter as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_parallel_count", rava_parallel_count as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_parallel_argmax", rava_parallel_argmax as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_parallel_reduce2", rava_parallel_reduce2 as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_lastore", rava_lastore as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_indy", rava_indy as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_ldc", rava_ldc as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_str_length", rava_str_length as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_str_hash", rava_str_hash as *const u8);
    #[cfg(feature = "next-gen-gc")]
    builder.symbol("rava_str_charat", rava_str_charat as *const u8);
    JITModule::new(builder)
}

/// Borrows the shared module out of the thread-local for a compile and restores
/// it on drop — so every exit path (including the emit `?` bails) keeps the
/// already-finalized code mapped.
/// ⛔**RETIRING THE MODULE IN BATCHES DOES NOT WORK, AND THE GATES SAID SO IMMEDIATELY.**
///
/// The cost being chased is real: `finalize_definitions()` is not incremental — cranelift-jit
/// re-protects every region the module owns on each call, and we call it PER METHOD, so the Nth
/// compile walks N regions. The collapsed profiles show it as `ArenaMemoryProvider` at
/// **11.3% + 4.3% of all samples** beside `ZwProtectVirtualMemory`.
///
/// Rotating to a fresh `JITModule` every 64 definitions bounds that walk. It also breaks the
/// program: compiled bodies call each other DIRECTLY, and a symbol defined in a retired module
/// is not visible from the next one. Measured in one run — `ringscan`, `ringlane` and
/// `ringlanei` all MISMATCH and the boot never reaches `Done`.
///
/// Doing this properly means declaring cross-module callees as imports and resolving them
/// against the retired modules' finalized addresses, which is a real piece of linkage work, not
/// a batching tweak. Left here as a signpost so the next attempt starts from the right problem.
pub(crate) struct ModuleGuard {
    pub(crate) module: Option<JITModule>,
}
impl Drop for ModuleGuard {
    fn drop(&mut self) {
        if let Some(m) = self.module.take() {
            SHARED_MODULE.with(|c| *c.borrow_mut() = Some(m));
        }
    }
}
