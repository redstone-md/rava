//! Compile entry points: what a caller asks for, and the register/deep variants.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// GAP-S09 stage 3 (LIVE HOLE 1 closure): triage for a worker-ILLEGAL callback —
/// one whose service PUBLISHES into shared `Vm` state that is not COW-shadowed
/// (statics, `indy_singletons`, `class_objects`, native `$map` tables, BitSet
/// `$bits` words, the shared array Vec). Exactly the `rava_getstatic_cached`
/// exception pattern (see the worker branch there):
/// - not gil-free (GIL-held / serial caller) → `Serial`: the normal body runs
///   unchanged — the byte-identical default path;
/// - gil-free + `RAVA_M3_WORKER_LOCKED_EXEC` → `Locked(guard)`: the normal body
///   runs while the re-entrant resolve lock is held (main is parked + siblings
///   serialize on the same lock ⇒ an exclusive `&mut Vm` envelope — the S2
///   acceptance path);
/// - gil-free otherwise → `Abort`: the region abort is requested here, BEFORE
///   any shared mutation, and the callback must return its neutral value NOW
///   (the armored-abort pattern of jit.rs 11.8: the region tears down at the
///   next abort check and the driver recomputes serially, so the discarded
///   neutral value is never observed).
#[cfg(feature = "next-gen-gc")]
pub(crate) enum WorkerGate {
    Serial,
    Locked(ResolveGuard),
    Abort,
}

/// The abort half of the stage-3 stub, split out for the callbacks whose gate has
/// a pre-abort serving arm (warm `ldc`, in-region `anewarray` claim). Tallies the
/// census ordinal, requests the region abort, and names the first few sites.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn worker_abort_stub(census_kind: usize, what: &'static str) {
    s09census::note_abort(census_kind);
    // GILFREE-RINGS: attribute the deopt. Every S09 worker-illegal callback
    // funnels through here, so this one line turns "the lane aborted" into
    // "the lane aborted at `indy` / `putstatic` / `map_put` / ...".
    crate::gilfree_cf::note_stub_abort(what);
    crate::parallel::request_abort();
    static DIAG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if DIAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 4 {
        eprintln!(
            "[RAVA-ABORT-DIAG] worker-illegal `{what}` callback on a gil-free worker — shared publication would race; aborting to serial."
        );
    }
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn worker_illegal_gate(census_kind: usize, what: &'static str) -> WorkerGate {
    if !crate::parallel::gilfree() {
        return WorkerGate::Serial;
    }
    if crate::worker_locked_exec_enabled() {
        // NAME THE KIND. This one line is 95.5% of every resolve-lock acquisition in a boot
        // (82,382,752 of 86,302,646, `RAVA_LANE_OUTER`), and seven callbacks share it.
        crate::gilfree_cf::note_locked_gate(what);
        return WorkerGate::Locked(resolve_lock_acquire());
    }
    worker_abort_stub(census_kind, what);
    WorkerGate::Abort
}

#[allow(dead_code)]
pub fn kernel_profile(bc: &[u8], pool: &ConstantPool) -> KernelProfile {
    let mut p = KernelProfile { code_len: bc.len(), ..Default::default() };
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { break };
        match op {
            0xbb | 0xbc | 0xbd | 0xc5 => p.allocs += 1, // new / newarray / anewarray / multianewarray
            0xb6 | 0xb7 | 0xb8 | 0xb9 => {
                // invoke virtual/special/static/interface — java/lang/Math is the only allowed callee
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                let is_math = pool
                    .member_ref(idx)
                    .map(|m| m.owner == "java/lang/Math" || m.owner == "java/lang/StrictMath")
                    .unwrap_or(false);
                if is_math {
                    p.math_calls += 1;
                } else {
                    p.nonmath_calls += 1;
                }
            }
            0xba => p.nonmath_calls += 1, // invokedynamic
            0x30 | 0x31 | 0x51 | 0x52 => p.dfloat_array_ops += 1, // faload/daload/fastore/dastore
            0x2e | 0x2f | 0x4f | 0x50 => p.int_array_ops += 1,    // iaload/laload/iastore/lastore
            // float/double arithmetic (fadd/dadd/fsub/dsub/fmul/dmul/fdiv/ddiv/frem/drem/fneg/dneg)
            0x62 | 0x63 | 0x66 | 0x67 | 0x6a | 0x6b | 0x6e | 0x6f | 0x72 | 0x73 | 0x76 | 0x77 => {
                p.fp_arith += 1
            }
            // conditional/goto branches — a backward target means a loop
            0x99..=0xa7 | 0xc6 | 0xc7 => {
                let off = i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32;
                if off < 0 {
                    p.has_loop = true;
                }
            }
            0xc8 => {
                // goto_w (4-byte signed)
                let off = i32::from_be_bytes([bc[pc + 1], bc[pc + 2], bc[pc + 3], bc[pc + 4]]);
                if off < 0 {
                    p.has_loop = true;
                }
            }
            _ => {}
        }
        pc += len;
    }
    p
}

/// Compile a method to native code, or `None` if it uses any opcode outside the
/// supported set or has exception handlers. Numeric and reference returns are
/// both compiled (a reference result is a tagged handle / box index).
pub fn compile(
    code: &Code,
    descriptor: &str,
    pool: &ConstantPool,
    class_ptr: i64,
    intern: &mut dyn FnMut(&str, &str) -> (u32, Option<u32>),
) -> Option<JitFn> {
    #[cfg(feature = "next-gen-gc")]
    {
        compile_deep(code, descriptor, pool, class_ptr, intern, None)
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        compile_bare(code, descriptor, pool, class_ptr, intern)
    }
}

/// `compile` with an explicit Deep-IC v3 shape. The register ABI needs this: a NON-leaf
/// register entry builds its own internal frame buffer and must carry the
/// self-registering prologue, or the references it spills there would be invisible to
/// the collector (`compile` alone always passes `None`, which is why the register arc
/// was leaf-only).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn compile_deep(
    code: &Code,
    descriptor: &str,
    pool: &ConstantPool,
    class_ptr: i64,
    intern: &mut dyn FnMut(&str, &str) -> (u32, Option<u32>),
    deep: Option<DeepShape>,
) -> Option<JitFn> {
    compile_inner(code, descriptor, pool, class_ptr, intern, deep)
}

#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn compile_bare(
    code: &Code,
    descriptor: &str,
    pool: &ConstantPool,
    class_ptr: i64,
    intern: &mut dyn FnMut(&str, &str) -> (u32, Option<u32>),
) -> Option<JitFn> {
    compile_inner(code, descriptor, pool, class_ptr, intern)
}

pub(crate) fn compile_inner(
    code: &Code,
    descriptor: &str,
    pool: &ConstantPool,
    class_ptr: i64,
    intern: &mut dyn FnMut(&str, &str) -> (u32, Option<u32>),
    #[cfg(feature = "next-gen-gc")] deep: Option<DeepShape>,
) -> Option<JitFn> {
    let static_intrinsics = std::collections::HashMap::new();
    let inline_targets = std::collections::HashMap::new();
    let native_calls = std::collections::HashSet::new();
    let statics = std::collections::HashMap::new();
    #[cfg(feature = "next-gen-gc")]
    let new_arena = std::collections::HashMap::new();
    #[cfg(feature = "next-gen-gc")]
    let ctor_inlines = std::collections::HashMap::new();
    #[cfg(feature = "next-gen-gc")]
    let concat_shapes = std::collections::HashMap::new();
    #[cfg(feature = "next-gen-gc")]
    let hof_fusions = std::collections::HashMap::new();
    #[cfg(feature = "next-gen-gc")]
    let getter_inlines = std::collections::HashMap::new();
    #[cfg(feature = "next-gen-gc")]
    let prim_arrays = if arena_primitive_arrays_enabled() {
        analyze_arena_primitive_arrays(code)
    } else {
        ArenaPrimArrayPlan::default()
    };
    compile_with_static_intrinsics(
        code,
        descriptor,
        pool,
        class_ptr,
        intern,
        &static_intrinsics,
        &inline_targets,
        &native_calls,
        &statics,
        #[cfg(feature = "next-gen-gc")]
        &new_arena,
        #[cfg(feature = "next-gen-gc")]
        &ctor_inlines,
        #[cfg(feature = "next-gen-gc")]
        &concat_shapes,
        #[cfg(feature = "next-gen-gc")]
        &hof_fusions,
        #[cfg(feature = "next-gen-gc")]
        &getter_inlines,
        #[cfg(feature = "next-gen-gc")]
        deep,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        &prim_arrays,
    )
}

/// I1a: compile a pure-primitive-LEAF method with the register-ABI entry
/// (`fn(array_desc_cell, prim_args...) -> prim_ret`) instead of the memory-locals ABI.
/// Returns None if the descriptor isn't all-primitive or the body isn't a pure leaf —
/// the caller then falls back to the normal `compile`. The register shape rides on the
/// returned `JitFn.reg`. Sets the `REG_ABI` thread-local across the compile so the three
/// injection points switch ABI; the default (memory) path is untouched.
pub(crate) fn compile_register(
    code: &Code,
    descriptor: &str,
    pool: &ConstantPool,
    class_ptr: i64,
    has_recv: bool,
    intern: &mut dyn FnMut(&str, &str) -> (u32, Option<u32>),
) -> Option<JitFn> {
    let mut abi = all_primitive_desc(descriptor)?;
    abi.has_recv = has_recv;
    // Eligibility (leaf vs non-leaf) is the resolver's call: a static leaf (I1b) and a
    // non-leaf instance method (I2, self-reg + `this` in the buffer) both compile here. The
    // body emit still bails (→ None) on anything it can't handle.
    REG_ABI.with(|c| *c.borrow_mut() = Some(abi.clone()));
    let r = compile(code, descriptor, pool, class_ptr, intern);
    REG_ABI.with(|c| *c.borrow_mut() = None);
    r.map(|mut f| {
        f.reg = Some(abi);
        f
    })
}

/// `compile_register` with the JIT driver's FULL analysis (inline targets, static
/// intrinsics, arena `new` sites, ctor/getter inlining, concat shapes, HOF fusions).
///
/// This exists because the register arc's original entry point routes through `compile`,
/// which hard-codes every analysis map to EMPTY. That was invisible while register
/// entries were pure primitive leaves — a two-instruction arithmetic body has nothing to
/// inline — but a reference-carrying callee is ordinary object code, and compiling it
/// without the maps costs far more than the calling convention saves: every small callee
/// becomes a real dispatch, every `new` a callback, every trivial getter a call. Measured
/// on ChunkFill the impoverished path cost +35% SETUP / +58% FILL, against +7.6% on
/// BigGraph, whose `sum` happens to have nothing inlinable — which is exactly the
/// signature of a missing-inlining regression.
#[cfg(feature = "next-gen-gc")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_register_with(
    code: &Code,
    descriptor: &str,
    pool: &ConstantPool,
    class_ptr: i64,
    has_recv: bool,
    allow_ref: bool,
    intern: &mut dyn FnMut(&str, &str) -> (u32, Option<u32>),
    static_intrinsics: &std::collections::HashMap<u16, StaticIntrinsic>,
    inline_targets: &std::collections::HashMap<u16, InlineCallee>,
    native_calls: &std::collections::HashSet<u16>,
    statics: &std::collections::HashMap<u16, (u32, JT)>,
    new_arena: &std::collections::HashMap<u16, (u32, u32)>,
    ctor_inlines: &std::collections::HashMap<u16, CtorInline>,
    concat_shapes: &std::collections::HashMap<u16, ConcatShape>,
    hof_fusions: &std::collections::HashMap<usize, crate::hof_fusion::HofFusionPlan>,
    getter_inlines: &std::collections::HashMap<u16, (u32, JT)>,
    deep: Option<DeepShape>,
    prim_arrays: &ArenaPrimArrayPlan,
) -> Option<JitFn> {
    let mut abi = reg_abi_desc(descriptor, allow_ref)?;
    abi.has_recv = has_recv;
    REG_ABI.with(|c| *c.borrow_mut() = Some(abi.clone()));
    let r = compile_with_static_intrinsics(
        code,
        descriptor,
        pool,
        class_ptr,
        intern,
        static_intrinsics,
        inline_targets,
        native_calls,
        statics,
        new_arena,
        ctor_inlines,
        concat_shapes,
        hof_fusions,
        getter_inlines,
        deep,
        #[cfg(feature = "next-gen-gc")]
        None,
        #[cfg(feature = "next-gen-gc")]
        prim_arrays,
    );
    REG_ABI.with(|c| *c.borrow_mut() = None);
    r.map(|mut f| {
        f.reg = Some(abi);
        f
    })
}

/// Build the shared module's ISA + register every heap callback symbol (done once
/// per thread, lazily on first compile).
/// AOT symbol table (aot_clif tier): the ordered `(name, address)` list a compiled
/// method's Abs8 relocs resolve against. The INDEX is the stable symbol number baked
/// into a reloc; resolution at link time maps index → this-process address. Order is
/// load-bearing — a reorder invalidates persisted AOT code, so it is gated by
/// cnp::AOT_FORMAT (bump on any change here). Only the callbacks the AOT translator
/// emits are listed; extend alongside the translator's opcode coverage.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn aot_symtab() -> &'static [(&'static str, usize)] {
    static T: std::sync::OnceLock<Vec<(&'static str, usize)>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        vec![
            ("rava_getfield", rava_getfield as usize),
            ("rava_putfield", rava_putfield as usize),
            ("rava_ring_store_guard", rava_ring_store_guard as usize),
            ("rava_invoke", rava_invoke as usize),
            ("rava_fu_invoke", rava_fu_invoke as usize),
            ("rava_ldc", rava_ldc as usize),
            ("rava_aastore", rava_aastore as usize),
            ("rava_iastore", rava_iastore as usize),
            ("rava_i32_divrem", rava_i32_divrem as usize),
            ("rava_i64_divrem", rava_i64_divrem as usize),
            ("rava_f32_rem", rava_f32_rem as usize),
            ("rava_f64_rem", rava_f64_rem as usize),
            ("rava_array_fault", rava_array_fault as usize),
            ("rava_aload_verify", rava_aload_verify as usize),
            ("rava_card_mark", rava_card_mark as usize),
            ("rava_throw", rava_throw as usize),
            ("rava_dispatch_miss", rava_dispatch_miss as usize),
            ("rava_indy", rava_indy as usize),
            ("rava_new", rava_new as usize),
            ("rava_newarray", rava_newarray as usize),
            ("rava_anewarray", rava_anewarray as usize),
            ("rava_getstatic_cached", rava_getstatic_cached as usize),
            ("rava_putstatic", rava_putstatic as usize),
            ("rava_instanceof", rava_instanceof as usize),
            ("rava_aload", rava_aload as usize),
            ("rava_arraylength", rava_arraylength as usize),
            // Appended (stable indices — never reorder above this line without bumping
            // cnp::AOT_FORMAT, which invalidates persisted AOT code by key).
            ("rava_getstatic", rava_getstatic as usize),
            // AOT callback slots are populated on x86_64 Windows/Linux. Other targets have no
            // AOT writer/reader, so keep the stable symbol indices with zero placeholders.
            ("rava_aot_null_fault", { #[cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))] { crate::aot_clif::runtime_context::null_fault as usize } #[cfg(not(all(any(windows, target_os = "linux"), target_arch = "x86_64")))] { 0usize } }),
            ("rava_aot_checkcast", { #[cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))] { crate::aot_clif::runtime_context::checkcast as usize } #[cfg(not(all(any(windows, target_os = "linux"), target_arch = "x86_64")))] { 0usize } }),
            ("rava_aot_primitive_store", { #[cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))] { crate::aot_clif::runtime_context::primitive_store as usize } #[cfg(not(all(any(windows, target_os = "linux"), target_arch = "x86_64")))] { 0usize } }),
            ("rava_acmp", rava_acmp as usize),
            ("rava_aot_pending", { #[cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))] { crate::aot_clif::runtime_context::pending as usize } #[cfg(not(all(any(windows, target_os = "linux"), target_arch = "x86_64")))] { 0usize } }),
            ("rava_aot_progress", { #[cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))] { crate::aot_clif::progress::poll as usize } #[cfg(not(all(any(windows, target_os = "linux"), target_arch = "x86_64")))] { 0usize } }),
            ("rava_checkcast", rava_checkcast as usize),
            ("rava_npe_invoke_site", rava_npe_invoke_site as usize),
        ]
    })
}

/// Resolve an AOT symbol index → this-process callback address (the reloc resolver).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn aot_symbol_addr(idx: u32) -> usize {
    aot_symtab().get(idx as usize).map(|&(_, a)| a).unwrap_or(0)
}
