//! The per-compile environment: the imported callback signatures, the pool, and the analysis results emission reads. One instance lives for one method compile.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use super::*;

/// Tag bit marking a cell as an index into `Vm::jit_box_table`. Used for every
/// REFERENCE-slot value that isn't a heap handle or null: `Str`/`StdOut`/`StdErr`
/// AND boxed primitives. rava keeps boxed Integer/Long/Double/… as the flat
/// `Value::Int/Long/…` (identity boxing), whose bits would otherwise alias null
/// at 0 (losing a boxed `0`/`0.0`) or collide with REF_TAG — so a reference slot
/// must box them, not flatten them. Distinct from REF_TAG (1<<40); the table
/// index sits in the low 32 bits with all other high bits zero. (BOX_TAG is the
/// shared constant from `value.rs`.)
pub(crate) const BOX_TAG_HI: u64 = (BOX_TAG as u64) >> 32;

/// Raw address of `rava_array_fault` as an i64 — smuggled into the LLVM-AOT `consts`
/// array so a compiled array-access site can INDIRECT-call it on null/OOB (an indirect
/// call through a loaded pointer is reloc-free, unlike a direct extern symbol which the
/// `.text`-only extraction can't bind). Sets the same NPE/AIOOBE pending as the callback.
pub fn array_fault_ptr() -> i64 {
    rava_array_fault as usize as i64
}

/// Call a finalized JIT function by raw pointer. `desc_cell` is the array
/// descriptor-base cell pointer (`Heap::array_desc_cell_ptr`). Safety: `ptr` must
/// come from a live `JitFn` (its module keeps the code mapped) and `cells.len()`
/// must be >= the method's max_locals.
pub unsafe fn call_raw(ptr: *const u8, cells: &mut [i64], desc_cell: i64) -> i64 {
    let f: extern "C" fn(*mut i64, i64) -> i64 = std::mem::transmute(ptr);
    f(cells.as_mut_ptr(), desc_cell)
}

/// Shared empty BCE set for inlined callee bodies: an inlined callee's array-load PCs
/// index the CALLEE's bytecode, which the top method's `safe_bce_loads` never analysed,
/// so a numeric PC collision must NOT skip a bounds check. Inlined bodies keep every
/// bounds check (sound); only the top-level method gets loop-BCE.
pub(crate) fn empty_bce() -> &'static std::collections::HashSet<usize> {
    static E: std::sync::OnceLock<std::collections::HashSet<usize>> = std::sync::OnceLock::new();
    E.get_or_init(std::collections::HashSet::new)
}

/// Shared empty primitive-array plan for inlined bodies. Plan PCs belong to one exact `Code`
/// body, so a callee must never consult its caller's numerically colliding bytecode offsets.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn empty_prim_arrays() -> &'static ArenaPrimArrayPlan {
    static E: std::sync::OnceLock<ArenaPrimArrayPlan> = std::sync::OnceLock::new();
    E.get_or_init(ArenaPrimArrayPlan::default)
}

// ⚠**THE cp-INDEXED SIDE MAPS ARE THE CALLER'S, AND A ROOTED SPLICE IS THE FIRST BODY THAT CAN
// READ THEM.** `Env::new_arena`, `ctor_inlines`, `static_intrinsics` and `array_fields` are all
// keyed by CONSTANT-POOL INDEX, and `emit_inline` builds the callee's environment with `..*env`,
// so a spliced body looks its OWN cp indices up in the CALLER's tables. Until this round nothing
// could reach the first two (`is_inlinable_kind` refuses `0xbb`, and `emit_inline` passes
// `cursor_var: None`), and a rooted splice can: cp 8 in `DataResult$Success` would find whatever
// cp 8 means in the caller and allocate the wrong class — silent heap corruption, not a wrong
// value. So a rooted splice swaps all four for the empties below, empty being the CONSERVATIVE
// answer (the generic path, no site plan) — except `ctor_inlines`, swapped for the CALLEE's own
// (`InlineCallee::ctor_inlines`), because collapsing `new X; …; invokespecial X.<init>` into a
// bump plus inline field stores is the entire point of admitting `new`.
//
// ⛔The historic splice keeps reading the caller's tables. That is a latent cp collision this
// round did NOT fix, deliberately: `RAVA_NO_INLINE_GCPOINT=1` must restore the historic emission
// exactly, and changing what every existing splice emits is not that.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn empty_new_arena() -> &'static std::collections::HashMap<u16, (u32, u32)> {
    static E: std::sync::OnceLock<std::collections::HashMap<u16, (u32, u32)>> =
        std::sync::OnceLock::new();
    E.get_or_init(std::collections::HashMap::new)
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn empty_static_intrinsics(
) -> &'static std::collections::HashMap<u16, StaticIntrinsic> {
    static E: std::sync::OnceLock<std::collections::HashMap<u16, StaticIntrinsic>> =
        std::sync::OnceLock::new();
    E.get_or_init(std::collections::HashMap::new)
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn empty_concat_shapes() -> &'static std::collections::HashMap<u16, ConcatShape> {
    static E: std::sync::OnceLock<std::collections::HashMap<u16, ConcatShape>> =
        std::sync::OnceLock::new();
    E.get_or_init(std::collections::HashMap::new)
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn empty_array_fields() -> &'static std::collections::HashSet<u16> {
    static E: std::sync::OnceLock<std::collections::HashSet<u16>> = std::sync::OnceLock::new();
    E.get_or_init(std::collections::HashSet::new)
}

/// A resolved invokestatic target that is small + pure enough to INLINE into the
/// caller's compiled body (true inlining — no runtime call, so no dispatch/GC
/// overhead). Holds the callee's class (for its constant pool) and bytecode.
/// 16.2 phase 2: max chained-inline depth. A DEPTH-1 callee is a pure leaf (no invokes); deeper
/// levels inline a callee whose own invokes all resolve to (recursively) inlinable callees.
pub(crate) const MAX_INLINE_DEPTH: u8 = 3;

pub(crate) const INLINE_MAX_BYTECODE: usize = 35;

pub(crate) fn invoke_cp(bc: &[u8], at: usize) -> Option<u16> {
    if bc.get(at).copied()? != 0xb8 {
        return None;
    }
    Some(((*bc.get(at + 1)? as u16) << 8) | *bc.get(at + 2)? as u16)
}

/// ⭐**ONE CALL BUFFER PER FUNCTION, NOT ONE PER CALL SITE.**
///
/// Every machine-code dispatch site — `emit_direct_mono`, `emit_dispatch_pic` — allocates its own
/// `dispatch_callbuf_slots() * 8` stack slot to hold the callee's locals frame. With the wide
/// buffer default-ON that is **512 bytes per site**, so a compiled body with twenty static calls
/// carries ~10 KB of frame for buffers that are *never simultaneously live*: a call buffer is
/// written immediately before its call and is dead the moment that call returns.
///
/// **WHY THEY ARE PER-SITE TODAY** — nothing structural: the buffer is simply created where it
/// is used. The liveness argument that makes one buffer serve every site is:
///   * the argument cells are POPPED into SSA values before a single store lands, so all nested
///     argument evaluation (which may itself contain calls, hence buffer uses) is already
///     finished when the buffer is written;
///   * between the last store and the call the emitters run only `stack_addr`, `spill_for_gc`
///     (which writes `locals_ptr`, never the buffer) and block bookkeeping;
///   * the callee runs in ITS OWN frame with ITS OWN slots — a compiled callee never re-enters
///     the caller's buffer — so recursion and re-entrancy cannot alias;
///   * an INLINED callee body shares this `Env` through `..*env`, and it is the same Cranelift
///     function, so it is the same frame and the same reasoning applies.
///
/// **AND WHY IT IS SOUND FOR GC, which is the part that is not obvious.** A `deep`
/// (self-registering) callee pushes `(buffer, frame_len)` onto the thread's cell-root stack and
/// the collector scans that range BY RANGE, not by liveness — so stale content left by a previous
/// site would be *followed*. It cannot be: the caller writes `[0, slot_total)` in full (including
/// the explicit zero store into every wide argument's dead pair slot) and the v3 prologue zeroes
/// `[args_slots, frame_len)`, and those two ranges together cover the whole scanned span. The
/// cover is a property of the callee's own frame, not of who used the memory last, so sharing
/// changes nothing about it. A GC-leaf callee registers no range at all.
///
/// **TWO SLOTS, NOT ONE.** A PIC site lays the buffer out in JVM SLOTS for the compiled-callee
/// hit arm and builds a PACKED copy for the cold arms (`rava_dispatch_miss` / the native thunks),
/// and both addresses are live across the same branch. They get separate shared slots so the cold
/// re-marshal can never clobber the hot layout — one extra region for the whole function instead
/// of one extra region per wide site.
///
/// **LAZY ON PURPOSE.** A leaf body with no dispatch site must not grow a kilobyte of frame it
/// never touches, so the slots are created on FIRST USE and memoised in these cells. `Env` is
/// `Copy` and handed out as `&Env`, hence `Cell` rather than a mutable field.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct CallBufs {
    /// The slot-laid buffer the compiled-callee hit arms pass as the callee's locals.
    call: std::cell::Cell<Option<cranelift::codegen::ir::StackSlot>>,
    /// The packed (one cell per argument) copy the cold callback/thunk arms pass.
    packed: std::cell::Cell<Option<cranelift::codegen::ir::StackSlot>>,
    /// ⭐**THE ROOTED-SPLICE WINDOW.** The frame a spliced callee that contains a GC point runs
    /// its `spill_for_gc` through, instead of the CALLER's buffer from offset 0. See
    /// `emit_inline`'s `splice_roots` arm: the splice zeroes this region, registers it on the
    /// thread's cell-root stack for the duration of the body, and pops it on the way out — the
    /// registration half of the prologue the splice elided.
    ///
    /// Shared for the whole function for the same reason `call`/`packed` are: a splice's window
    /// is written immediately before its body and dead the moment the body ends, and the rooted
    /// splice is offered only at depth 0, so two of them are never simultaneously live.
    splice: std::cell::Cell<Option<cranelift::codegen::ir::StackSlot>>,
}

#[cfg(feature = "next-gen-gc")]
impl CallBufs {
    pub(crate) fn new() -> Self {
        CallBufs {
            call: std::cell::Cell::new(None),
            packed: std::cell::Cell::new(None),
            splice: std::cell::Cell::new(None),
        }
    }
}

#[cfg(feature = "next-gen-gc")]
impl Default for CallBufs {
    fn default() -> Self {
        Self::new()
    }
}

/// Bytes in ONE shared call buffer.
///
/// The width is `dispatch_callbuf_slots()` and cannot be trimmed to the site's own argument
/// count: the callee reads this memory as its ENTIRE locals+stack frame, its `frame_len` is only
/// known at run time (the site's cache is filled by `rava_direct_miss`/`rava_dispatch_miss`), and
/// the bound it was admitted against is exactly `dispatch_callbuf_slots()`. So "sized to the
/// largest site" is the budget itself — the win here is the count of buffers, not their width.
///
/// **PLUS ONE GUARD SLOT.** `emit_lambda_shift_adjust` hands the callee `bufaddr + 8` when the
/// cached pointer carries the λ-shift bit, so a callee with a full-width frame indexes one slot
/// PAST a budget-sized region. The per-site slots have always been exactly budget-sized and have
/// always had that one-slot overhang into whatever sat next in the frame; the shared buffer pays
/// 8 bytes ONCE for the whole function to close it.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn shared_callbuf_bytes() -> u32 {
    (dispatch_callbuf_slots() as u32 + 1) * 8
}

/// `RAVA_NO_SHARED_CALLBUF=1` restores a private buffer at every dispatch site (and the
/// unconditional `max_stack`-sized args buffer in the driver). DEFAULT ON, read at EMIT time
/// because it decides codegen, and it also honours the block switch `RAVA_NO_EMIT_MORE`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn shared_callbuf_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_SHARED_CALLBUF").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Imported heap-callback references + the pre-resolved getfield field map
/// (cp-index -> (interned field id, field type)), threaded through emit.
#[derive(Clone, Copy)]
pub(crate) struct Env<'a> {
    /// Local types at every instruction boundary, for building a deopt site's frame state.
    /// `None` when the analysis could not walk the body — then no deopt is emitted anywhere in
    /// this method, which is a refusal and not a failure.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) local_types: Option<&'a crate::jit::analysis::localtypes::LocalTypes>,
    /// The method's return type — a deopt returns from the function, so its result must be
    /// converted to what the signature promises.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ret_jt: Option<crate::jit::JT>,
    pub(crate) getfield: cranelift::codegen::ir::FuncRef,
    pub(crate) putfield: cranelift::codegen::ir::FuncRef,
    pub(crate) putstatic: cranelift::codegen::ir::FuncRef,
    pub(crate) new_obj: cranelift::codegen::ir::FuncRef,
    pub(crate) instanceof: cranelift::codegen::ir::FuncRef,
    pub(crate) checkcast: cranelift::codegen::ir::FuncRef,
    pub(crate) npe_invoke_site: cranelift::codegen::ir::FuncRef,
    pub(crate) newarray: cranelift::codegen::ir::FuncRef,
    pub(crate) anewarray: cranelift::codegen::ir::FuncRef,
    /// GILFREE-RINGS array-store worker barrier — emitted on the COW-armed side
    /// of every `*astore`. The lane is DEFAULT ON, so this arm is emitted in an ordinary build;
    /// `RAVA_NO_GILFREE_RINGS` restores the codegen that predates it. See `rava_ring_store_guard`.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ring_store_guard: cranelift::codegen::ir::FuncRef,
    pub(crate) arrlen: cranelift::codegen::ir::FuncRef,
    pub(crate) aload: cranelift::codegen::ir::FuncRef,
    pub(crate) invoke: cranelift::codegen::ir::FuncRef,
    /// Register-ABI arc (I1b): rava_resolve_reg host callback (static call-site resolver).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) resolve_reg: cranelift::codegen::ir::FuncRef,
    pub(crate) throw: cranelift::codegen::ir::FuncRef,
    /// Fastutil int-list scalar fast-path dispatch (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fu_invoke: cranelift::codegen::ir::FuncRef,
    /// The uncommon-trap exit: leave compiled code and finish in the interpreter.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) deopt: cranelift::codegen::ir::FuncRef,
    /// Enum.ordinal() fast path — $enumOrd read, general-dispatch fallback
    /// (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) enum_ordinal: cranelift::codegen::ir::FuncRef,
    /// java.util.BitSet get/set/clear/isEmpty fast paths — packed `$bits` word
    /// read/write, general-dispatch fallback (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bitset_get: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bitset_set: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bitset_clear: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bitset_isempty: cranelift::codegen::ir::FuncRef,
    pub(crate) getstatic: cranelift::codegen::ir::FuncRef,
    pub(crate) getstatic_cached: cranelift::codegen::ir::FuncRef,
    pub(crate) i32_divrem: cranelift::codegen::ir::FuncRef,
    pub(crate) i64_divrem: cranelift::codegen::ir::FuncRef,
    pub(crate) f32_rem: cranelift::codegen::ir::FuncRef,
    pub(crate) f64_rem: cranelift::codegen::ir::FuncRef,
    pub(crate) iastore: cranelift::codegen::ir::FuncRef,
    pub(crate) dastore: cranelift::codegen::ir::FuncRef,
    /// Diagnostic (RAVA_VERIFY_LOADS): per-native-load interp cross-check.
    pub(crate) aload_verify: cranelift::codegen::ir::FuncRef,
    /// Shadow verify for the bare-deref flip (`RAVA_BARE_VERIFY`), emitted only under that gate.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bare_verify: cranelift::codegen::ir::FuncRef,
    pub(crate) acmp: cranelift::codegen::ir::FuncRef,
    /// Stop-only poll callback; emitted only when the opt-in compile gate is enabled.
    pub(crate) safepoint: Option<cranelift::codegen::ir::FuncRef>,
    pub(crate) poll_backedges: bool,
    /// Inline arena putfield-ref card barrier (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) card_mark: cranelift::codegen::ir::FuncRef,
    /// Inline-TLAB-bump cursor fetch (`() -> i64`, next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) cursor_addr_fn: cranelift::codegen::ir::FuncRef,
    /// Inline-TLAB-bump nursery-full safepoint (`() -> i64`, next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) arena_safepoint_fn: cranelift::codegen::ir::FuncRef,
    /// Bounded exit for the inline-alloc retry loop (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) arena_deopt_alloc_fn: cranelift::codegen::ir::FuncRef,
    /// Machine-code dispatch-IC miss handler (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) dispatch_miss: cranelift::codegen::ir::FuncRef,
    /// 11.3 per-thread IC base fetch `() -> i64` (next-gen-gc only). Reserved for the relative-
    /// addressing dispatch site; only emitted when `ic_parallel_enabled()`.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ic_base_fn: cranelift::codegen::ir::FuncRef,
    /// 23.4 heap-Ref PIC: current `Heap.objects` base fetch `() -> i64` (re-read per dispatch,
    /// never baked) + the 23.4a shadow-verify callback `(recv, machine_key) -> i64` (next-gen-gc).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) objects_base_fn: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) heap_shadow_fn: cranelift::codegen::ir::FuncRef,
    /// 23.9 monomorphic static/special direct-call miss handler `(cache,caller,cp,kind,buf,n)->i64`.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) direct_miss_fn: cranelift::codegen::ir::FuncRef,
    /// Deep-IC v3 self-registration cold paths: the prologue's inline cell-root
    /// push overflow, and the epilogue's box-table unwind (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) cells_slow: cranelift::codegen::ir::FuncRef,
    /// Current thread's `JitThreadCtx` address. Used only by a COW-armed
    /// detached Deep-IC prologue; the normal path stays inline/callback-free.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ctx_addr_fn: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) box_exit: cranelift::codegen::ir::FuncRef,
    /// Deep-IC v3: Some = emit the self-registering prologue/epilogue with this
    /// shape (build_body only; folded/parallel bodies never self-register).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) deep: Option<DeepShape>,
    /// HashMap.put collection intrinsic (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) map_put_fn: cranelift::codegen::ir::FuncRef,
    /// HashMap.get collection intrinsic (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) map_get_fn: cranelift::codegen::ir::FuncRef,
    /// HashMap.remove / containsKey collection intrinsics (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) map_remove_fn: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) map_contains_fn: cranelift::codegen::ir::FuncRef,
    /// Step-3 arena-map intrinsics (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amap_get_fn: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amap_default_fn: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amap_contains_fn: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amap_put_fn: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amap_remove_fn: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) amap_box_value_fn: cranelift::codegen::ir::FuncRef,
    /// Integer box/unbox collection intrinsics (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) box_int_fn: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) unbox_int_fn: cranelift::codegen::ir::FuncRef,
    /// invokedynamic + ldc-ref callbacks (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) indy: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ldc: cranelift::codegen::ir::FuncRef,
    /// String.length/hashCode fallback callbacks (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) str_length_fn: cranelift::codegen::ir::FuncRef,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) str_hash_fn: cranelift::codegen::ir::FuncRef,
    /// `String.startsWith(String)` fallback callback (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) str_startswith_fn: cranelift::codegen::ir::FuncRef,
    /// `String.charAt(int)` fallback callback (next-gen-gc only).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) str_charat_fn: cranelift::codegen::ir::FuncRef,
    /// `new` (0xbb) sites whose class is arena-eligible + already initialised at
    /// compile time → (class_id, nfields). Such a site bump-allocates inline (no
    /// callback); every other `new` stays on the `new_obj` callback. next-gen-gc only.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) new_arena: &'a std::collections::HashMap<u16, (u32, u32)>,
    /// `invokespecial` (0xb7) sites calling a TRIVIAL arena `<init>` → its resolved
    /// inline store plan. Such a site stores the constructor args straight into the
    /// arena object's cells inline (no `rava_invoke` callback). next-gen-gc only.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ctor_inlines: &'a std::collections::HashMap<u16, CtorInline>,
    /// `invokedynamic` (0xba) concat sites resolved to an inline itoa plan (all-int
    /// args + ASCII literals → built straight into a TLAB ArenaString, no
    /// `rava_indy` callback). Every other concat stays on the callback. ngg only.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) concat_shapes: &'a std::collections::HashMap<u16, ConcatShape>,
    /// Adjacent indy→static-consumer structural fusion plans, scoped to this
    /// exact Code body. Nested inlines receive the shared empty map.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) hof_fusions: &'a std::collections::HashMap<usize, crate::hof_fusion::HofFusionPlan>,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) code_key: usize,
    #[cfg(feature = "next-gen-gc")]
    pub(crate) active_sam: Option<ActiveSamBinding<'a>>,
    /// ⭐**AM I ALREADY INSIDE A SPLICED BODY?** `false` in the top-level method, set by
    /// `emit_inline`'s `b_env` for every callee body.
    ///
    /// It exists for ONE invariant, the one the rooted splice rests on: a rooted splice's window
    /// roots the CALLEE's frame and `locals_ptr` roots the CALLER's, and `locals_ptr` is the
    /// compiled method's own frame only when the splice is OUTERMOST. Admission enforces that by
    /// offering the arm at depth 0 only, and a depth-0 entry can reach emission only through the
    /// top-level `inline_targets` map — so this flag should never be true at a rooted site. It is
    /// checked anyway, and REFUSES THE COMPILE if it ever is, because the failure mode of the
    /// alternative is a nested splice silently re-zeroing its parent's live root window.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) in_splice: bool,
    pub(crate) array_fault: cranelift::codegen::ir::FuncRef,
    pub(crate) aastore: cranelift::codegen::ir::FuncRef,
    /// `Arc::as_ptr(&caller_class)` baked as a constant; passed to rava_invoke
    /// for pool/cp resolution.
    pub(crate) class_ptr: i64,
    /// Stack-slot args buffer (max_stack*8 bytes) for marshaling call arguments.
    pub(crate) args_slot: cranelift::codegen::ir::StackSlot,
    /// The FUNCTION's shared dispatch call buffers, created on first use. See `CallBufs`:
    /// one slot-laid region + one packed region for the whole body instead of two per site.
    /// An inlined callee inherits this through `..*env`, which is correct — same frame.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) callbufs: &'a CallBufs,
    /// getfield/putfield sites by cp index → (field-identity id, type, layout
    /// slot). The slot is `Some` only when per-class layout pre-population is
    /// active (RAVA_LAYOUT_ALLOC) so the object's field lives at a class-constant
    /// index → a getfield emits a single constant-offset load (verified, callback
    /// fallback) instead of the id-scan; in an invariant method that load is
    /// readonly → the egraph hoists loop-invariant field reads (field-LICM).
    pub(crate) fields: &'a std::collections::HashMap<u16, (u32, JT, Option<u32>)>,
    /// cp-indices whose field is declared an ARRAY. The bare-deref flip must NOT collapse
    /// the tag classify at these sites: `arraylength`/`aaload` downstream expect an
    /// Array-tagged value, and the descriptor already decides the representation, so the
    /// test costs nothing to keep here and keeping it removes 211,141 cells from the flip's
    /// blocker set. Only fields declared with an object type (202 cells, measured) can hold
    /// either representation and genuinely need the runtime test.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) array_fields: &'a std::collections::HashSet<u16>,
    /// May this compilation bare-deref AT ALL? False whenever the caller had no `Vm` and so
    /// could not answer the declared-type question — `array_fields` is then the descriptor-only
    /// subset, and since `bare_ok` is its COMPLEMENT, a subset authorises MORE bare loads, not
    /// fewer. Refusing the flip outright is the only safe reading of "we could not ask".
    #[cfg(feature = "next-gen-gc")]
    pub(crate) bare_allowed: bool,
    /// **EVERY FIELD ACCESS GOES THROUGH THE HOST CALLBACK, because this class is one the native
    /// layer serves.**
    ///
    /// ⭐The blocker `aot_link.rs` records — "a native-BACKED class's bytecode reads real fields
    /// the native layer never populates, so a compiled body would run a broken one" — is about
    /// the READ, not about compilation. The interpreter runs the same bytecode correctly because
    /// its `getfield` goes through the native path, which synthesises `a`/`size` from the
    /// engine's own backing; compiled code reads the slot directly and gets a sentinel. That is
    /// The observed failure with the namespace relaxation on is a boot dying in compiled code
    /// with `read of 0x100000000`. ⚠**That address is `AL_GUARD_BASE`, the arena-lambda
    /// direct-call GUARD token — not an array descriptor, as a first reading of it assumed.** So
    /// the field routing below is right on its own terms and does not fix that crash; see the
    /// bisect note on `nsb_only`.
    ///
    /// With this set, the emitted body asks the same accessor the interpreter asks, so the two
    /// cannot disagree, and what compilation buys is the frame and the dispatch rather than the
    /// field reads.
    pub(crate) callback_fields: bool,
    pub(crate) static_intrinsics: &'a std::collections::HashMap<u16, StaticIntrinsic>,
    pub(crate) inline_targets: &'a std::collections::HashMap<u16, InlineCallee>,
    /// Primitive getstatic sites that own a value cell: cp index → (cell slot, type).
    /// A native gen-guarded load replaces the callback at these sites.
    pub(crate) statics: &'a std::collections::HashMap<u16, (u32, JT)>,
    /// The method writes no fields/statics, allocates nothing, and makes no real
    /// (non-inlined) call → nothing mutates the heap during the compiled call and
    /// no callback can GC/realloc, so constant-slot field loads are `readonly`
    /// (immutable for the whole call) and the egraph hoists loop-invariant ones.
    pub(crate) invariant_fields: bool,
    /// Weaker: no allocation / non-inlined call (so no GC/heap-move), but putfields ARE
    /// allowed. The object DESCRIPTOR structure (ObjDesc base + ids/cells ptrs + count) is then
    /// immutable — those loads get `readonly` so the egraph CSEs the per-field re-decode — while
    /// the field CELL value load stays mutable (a putfield changes it). See `method_no_alloc`.
    pub(crate) no_alloc: bool,
    /// Static-LICM enabled: invariant method + loop + ≥1 cell-backed primitive
    /// static. getstatic populates the cells once at entry, then reads them as
    /// `readonly` loads in the body (the egraph hoists the loop-invariant ones),
    /// instead of the per-site gen-guarded diamond.
    pub(crate) static_licm: bool,
    /// FIELD-LICM: (pinned local, field) pairs whose cell pointer the prologue
    /// resolves once (see `licm_field_pairs`). Empty when the method isn't
    /// no_alloc / has no loop / no qualifying site.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) licm_pairs: &'a [LicmPair],
    /// PROMO: `Some(written pair set)` when the WHOLE method passed the strict
    /// register-promotion gate (`promo_scan`) — build_body then keeps every
    /// hoisted field's VALUE in a register (no load/store in the loop),
    /// flushing written registers at returns and before slow-arm callbacks.
    /// `None` = base-pointer LICM only.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) promo_written: Option<&'a std::collections::HashSet<(u16, u32, u32)>>,
    /// The interned `$bits` field id of `java.util.BitSet` (0 = the method makes no
    /// `BitSet.get(I)Z` call, so it was never resolved). The inline `BitSet.get`
    /// fast arm reads the backing `long[]` by this id; 0 disables the inline path
    /// (the FFI routing serves instead).
    #[cfg(feature = "next-gen-gc")]
    /// Interned id of `$elems`, resolved once per compile when the body contains an
    /// `ArrayList.get(I)` site. 0 = no such site, and the inline arm is not emitted.
    /// `$elems` id **PLUS ONE** — 0 means "not published", because the real id is legitimately
    /// 0 (it is the first field name any VM interns). Subtract before use.
    pub(crate) elems_fid: i64,
    pub(crate) bits_fid: i64,
    /// V4.1 generic loop-BCE: PCs of array-load ops the counted-loop analysis proved
    /// in-bounds (`safe_bce_loads`); the emitter skips their bounds branch. EMPTY for
    /// inlined callee bodies — their PCs are the callee's, not the top method's.
    /// `getfield` PCs whose direct local receiver was already successfully dereferenced
    /// in the same basic block. Unproven sites retain the null callback branch.
    pub(crate) nonnull_getfields: &'a std::collections::HashSet<usize>,
    pub(crate) bce_safe: &'a std::collections::HashSet<usize>,
    /// Method-confined primitive `newarray` sites, keyed in this exact body's PC space.
    /// Every spliced body receives `empty_prim_arrays` to prevent caller/callee PC collisions.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) prim_arrays: &'a ArenaPrimArrayPlan,
    /// Method-confined reference `anewarray; pop` sites, keyed by exact bytecode PC.
    /// Spliced bodies receive `empty_ref_arrays` because their PCs/CP belong to the callee.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) ref_arrays: &'a ArenaRefArrayPlan,
    /// The 8 primitive array element ids (heap.rs `prim_element_ids`, filled at VM
    /// construction). `native_primitive_load` gates on them: a REF-layout array
    /// (non-primitive element_id) routes to the callback instead of a raw load.
    pub(crate) prim_element_ids: [i64; 8],
}

// `allow(dead_code)`: the consumers are the dispatch emit sites (`emit_direct_mono`,
// `emit_dispatch_pic`, and the packed cold arms in both). Each switches over independently, so
// the accessors must not warn while one of them is still creating its own private slot.
#[cfg(feature = "next-gen-gc")]
#[allow(dead_code)]
impl<'a> Env<'a> {
    /// The SLOT-LAID dispatch call buffer for this function: the region a compiled-callee hit arm
    /// hands over as the callee's locals. Drop-in for the five-line
    /// `create_sized_stack_slot(… dispatch_callbuf_slots()*8 …)` at every emit site — with the
    /// kill switch set it returns a fresh private slot, i.e. exactly today's codegen.
    pub(crate) fn call_buf(&self, b: &mut FunctionBuilder) -> cranelift::codegen::ir::StackSlot {
        if !shared_callbuf_enabled() {
            return Self::fresh_buf(b, (dispatch_callbuf_slots() as u32) * 8);
        }
        if let Some(s) = self.callbufs.call.get() {
            return s;
        }
        let s = Self::fresh_buf(b, shared_callbuf_bytes());
        self.callbufs.call.set(Some(s));
        s
    }

    /// The PACKED dispatch call buffer for this function: the one-cell-per-argument copy the cold
    /// arms build for `rava_dispatch_miss` / `rava_direct_miss` / the `pic_native` thunks, whose
    /// ABI walks the descriptor and consumes one cell per parameter. Deliberately NOT the same
    /// region as `call_buf` — a wide site has both addresses live across one branch.
    pub(crate) fn packed_buf(&self, b: &mut FunctionBuilder) -> cranelift::codegen::ir::StackSlot {
        if !shared_callbuf_enabled() {
            return Self::fresh_buf(b, (dispatch_callbuf_slots() as u32) * 8);
        }
        if let Some(s) = self.callbufs.packed.get() {
            return s;
        }
        let s = Self::fresh_buf(b, shared_callbuf_bytes());
        self.callbufs.packed.set(Some(s));
        s
    }

    /// The ROOTED-SPLICE window for this function (see `CallBufs::splice`). Sized to the same
    /// budget a self-registering callee's own `frame_len` is admitted against, which is exactly
    /// the bound the callee's footprint was admitted against — so a callee that reached emission
    /// always fits. NOT shared with `call_buf`/`packed_buf`: a surviving call inside a spliced
    /// body writes a call buffer while this window holds that body's live references.
    pub(crate) fn splice_window(&self, b: &mut FunctionBuilder) -> cranelift::codegen::ir::StackSlot {
        if let Some(s) = self.callbufs.splice.get() {
            return s;
        }
        let s = Self::fresh_buf(b, (dispatch_callbuf_slots() as u32) * 8);
        self.callbufs.splice.set(Some(s));
        s
    }

    /// With the kill switch set the private slot is `dispatch_callbuf_slots() * 8` — exactly
    /// what each site allocates today, so `RAVA_NO_SHARED_CALLBUF=1` is the pre-round codegen
    /// byte for byte, guard slot included (i.e. not included).
    fn fresh_buf(b: &mut FunctionBuilder, bytes: u32) -> cranelift::codegen::ir::StackSlot {
        b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            bytes,
            3, // 8-byte align (align_shift)
        ))
    }
}

/// Load `array_descs[handle]` natively: returns (data_ptr, len_i32). `arr` is the
/// tagged array cell (caller guarantees non-null on this path). The descriptor
/// base is loaded from the cell each time so a realloc during the method is seen.
/// Address of `arr`'s `ArrayDesc` (16 B: data@0, len@8, element_id@12). Split out of
/// `load_array_desc` because the primitive-load element-type gate needs the SAME
/// address to read `element_id`, and recomputing it there cost a redundant load +
/// band + ishl + iadd on EVERY primitive array load — 92 of them in one
/// `ImprovedNoise.sampleAndLerp` body.
pub(crate) fn array_desc_addr(b: &mut FunctionBuilder, desc_cell: Value, arr: Value) -> Value {
    let base = b
        .ins()
        .load(I64, MemFlags::trusted(), desc_cell, Offset32::new(0));
    let mask = b.ins().iconst(I64, 0xFFFF_FFFF);
    let h = b.ins().band(arr, mask);
    let sixteen = b.ins().iconst(I64, 4); // *16 == <<4 (ArrayDesc is 16 bytes)
    let off = b.ins().ishl(h, sixteen);
    b.ins().iadd(base, off)
}

pub(crate) fn load_array_desc(b: &mut FunctionBuilder, desc_cell: Value, arr: Value) -> (Value, Value) {
    let desc = array_desc_addr(b, desc_cell, arr);
    let data = b.ins().load(I64, MemFlags::trusted(), desc, Offset32::new(0));
    let len = b.ins().load(I32, MemFlags::trusted(), desc, Offset32::new(8));
    (data, len)
}

/// Split-cell bucket stride in bytes (4 cells × 8).
#[cfg(feature = "next-gen-gc")]
pub(crate) const BW_BYTES: i32 = 32;
