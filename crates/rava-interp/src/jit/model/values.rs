//! The compiler's value vocabulary: JVM types, stack values, the register ABI and cell tags.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StaticIntrinsic {
    FloorDToI,
    LFloorDToJ,
    SmoothstepD,
    SmoothstepDerivativeD,
    LerpD,
    Lerp2D,
    Lerp3D,
}

/// Reference-cell tags are the shared ABI in `value.rs` (REF_TAG=object,
/// ARRAY_TAG=array, BOX_TAG=transient box-table index); these are the high-word
/// forms used to decode a cell by its tag.

/// JVM computational type of an operand-stack entry. `Ref` is an object/array
/// handle (u32) stored in the low 32 bits of the i64 cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JT {
    Int,
    Long,
    Float,
    Double,
    Ref,
}

// --- Stage 2: heap-access callbacks (read-only) -----------------------------
//
// Compiled methods that read fields/arrays call back into the VM. The VM is
// single-threaded, so rather than thread a `*mut Vm` through every compiled
// signature we stash it in a thread-local around the call. Callbacks reconstruct
// `&mut Vm`, perform the read, and on a null/out-of-bounds fault set
// `vm.jit_pending` (first fault wins) and return 0 — the read-only method then
// runs to completion harmlessly and `jit_run` propagates the exception.

/// One segment of a resolved StringConcatFactory recipe whose dynamic args are
/// ALL `int` (the case the JIT assembles inline with native itoa — no callback).
#[cfg(feature = "next-gen-gc")]
#[derive(Clone)]
pub(crate) enum ConcatSeg {
    /// A literal ASCII byte run (recipe text + zero-arg constants), baked.
    Lit(Vec<u8>),
    /// The `idx`-th dynamic int argument (in operand-stack push order), itoa'd.
    IntArg(usize),
}

/// A compile-time-resolved `invokedynamic` concat whose result the JIT builds
/// inline: literal bytes are baked, each int arg is shredded to decimal digits
/// straight into a TLAB-bumped ArenaString, with NO `rava_indy` callback. Only
/// emitted for `makeConcat[WithConstants]` returning String with all-`int` args
/// and ASCII literals (every other shape stays on the callback).
#[cfg(feature = "next-gen-gc")]
#[derive(Clone)]
pub(crate) struct ConcatShape {
    pub segs: Vec<ConcatSeg>,
    /// Number of dynamic int args (each one operand-stack slot).
    pub nargs: usize,
}

/// Register-ABI param/return machine kind (docs/register-abi-arc.md). Narrow integral
/// primitives (I/Z/B/C/S) are `I32` (1 local slot); `J` is `I64` (2 slots); `F32`/`F64`
/// are float/double (double = 2 slots).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PrimKind {
    I32,
    I64,
    F32,
    F64,
    /// A REFERENCE operand (`L…;` / `[…`). The value travelling in the register is the
    /// JIT's operand CELL verbatim — a raw arena pointer (low 2 bits 00), a REF_TAG heap
    /// handle, an ARRAY_TAG array handle, a BOX index, or 0 for null — exactly the i64
    /// `to_cell`/`cell_to_sv` already move through the memory-locals buffer. No encoding
    /// change: only the transport differs. See `reg_abi_desc` for the GC argument.
    Ref,
}

impl PrimKind {
    /// Cranelift ABI register type for this kind.
    pub(crate) fn cl_type(self) -> cranelift::codegen::ir::Type {
        match self {
            PrimKind::I32 => I32,
            PrimKind::I64 | PrimKind::Ref => I64,
            PrimKind::F32 => F32,
            PrimKind::F64 => F64,
        }
    }
    /// JVM local-slot width: long/double take 2, others 1 (a reference is narrow).
    pub(crate) fn slots(self) -> usize {
        match self {
            PrimKind::I64 | PrimKind::F64 => 2,
            _ => 1,
        }
    }
    /// Is this a GC-relevant (reference) operand?
    pub(crate) fn is_ref(self) -> bool {
        matches!(self, PrimKind::Ref)
    }
}

/// A resolved register-ABI shape: the ordered primitive value params + the return kind.
/// Stored on a `JitFn` whose entry was compiled register-ABI (`fn(array_desc_cell,
/// prim_args...) -> prim_ret`) rather than the uniform `fn(*i64 locals, i64) -> i64`.
#[derive(Clone, Debug)]
pub(crate) struct RegAbi {
    /// True for an INSTANCE method: the register entry takes `this` (a Ref, passed as an
    /// i64) as the first value param before the primitive args, occupying local slot 0.
    /// Set by the resolver (the descriptor alone doesn't encode static-ness).
    pub has_recv: bool,
    pub params: Vec<PrimKind>,
    pub ret: PrimKind,
}

impl RegAbi {
    /// Does any register-resident operand (param or return) hold a REFERENCE? Only such a
    /// shape needs the GC reasoning in `reg_abi_desc`; an all-primitive shape is the
    /// original, GC-inert arc and keeps its historic eligibility rules.
    pub(crate) fn carries_ref(&self) -> bool {
        self.ret.is_ref() || self.params.iter().any(|k| k.is_ref())
    }
}

/// A value on the compile-time operand stack: the SSA value plus its JVM type.
#[derive(Clone, Copy)]
pub(crate) struct SV {
    pub(crate) v: Value,
    pub(crate) t: JT,
    /// FIELD-LICM provenance: the OUTERMOST method-local slot this value is a
    /// direct `aload` of (threaded through inline-callee arg binding), or None.
    /// Only meaningful when that slot is invariant (never stored) — see
    /// `licm_field_pairs`. Set solely by the aload arms; every other producer
    /// leaves it None (conservative: the value then just misses the hoist).
    pub(crate) src: Option<u16>,
    /// ARENA PROOF: this value is a receiver the METHOD-ENTRY UNSWITCH already proved
    /// to be a raw arena pointer, so a field access on it needs NO `emit_is_arena`
    /// diamond — it straight-lines. True only inside the unswitch's arena body, only
    /// for values `aload`ed from the PINNED slot it branched on, and only while that
    /// proof still holds (see `flat_unswitch_slot` for the rule). Carried into inlined
    /// callees by the arg binding, so a proven receiver keeps its proof one frame down.
    /// Deliberately SEPARATE from `src`: that field is FIELD-LICM's pair key and is
    /// only populated when a `Licm` exists, which is the loop-shaped minority.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) arena: bool,
}

/// A value on the body emitter's operand stack: a scalar SSA value, or a marker for
/// a SCALAR-REPLACED virtual object (an index into `vobjs` — its fields live in
/// registers, it was never allocated).
#[cfg(feature = "next-gen-gc")]
#[derive(Clone)]
pub(crate) enum BVal {
    Scalar(Value, bool), // (value, is_long)
    Virt(usize),         // virtual-object index
    ArrRef(usize),       // captured primitive-array local index (for iaload/laload)
}
