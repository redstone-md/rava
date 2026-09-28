//! Pure functions over raw JVM bytecode.
//!
//! Extracted from `jit.rs` (27,213 lines) as the first step of the module split. Everything here
//! reads a `&[u8]` and returns a fact about it: instruction lengths, branch targets, the constant
//! pool indices an opcode family references, a load's shape, a stack effect. **No VM state, no
//! codegen, no globals** — which is exactly why this is the safe first cut.
//!
//! Re-exported from `jit::*`, so no call site changed.

use rava_classfile::Code;
use super::JT;

/// Length in bytes of a supported instruction, or `None` if the opcode is
/// outside the Stage-1 set (caller then bails out of compilation).
pub(crate) fn insn_len(op: u8) -> Option<usize> {
    Some(match op {
        0x00 | 0x01 => 1,
        0x02..=0x0f => 1, // *const_*
        0x10 => 2,        // bipush
        0x11 => 3,        // sipush
        0x12 => 2,        // ldc
        0x13 | 0x14 => 3, // ldc_w / ldc2_w
        0x15..=0x19 => 2, // *load (incl. aload)
        0x1a..=0x2d => 1, // *load_0..3 (incl. aload_0..3)
        0x2e..=0x35 => 1, // array loads iaload..saload (read-only, via callback)
        // array stores (native): iastore/lastore/fastore/dastore/aastore/bastore/
        // castore/sastore.
        0x4f..=0x56 => 1,
        0x36..=0x3a => 2, // *store (incl. astore)
        0x3b..=0x4e => 1, // *store_0..3 (incl. astore_0..3)
        0xbe => 1,        // arraylength
        0xb4 => 3,        // getfield (read-only, via callback)
        0xb5 => 3,        // putfield (mutating, via callback)
        0xb2 => 3,        // getstatic (read-only, via callback)
        0xb3 => 3,        // putstatic (mutating, via callback)
        0x60..=0x77 => 1, // add/sub/mul/div/neg (i,l,f,d) — int div/rem excluded below
        0x78..=0x83 => 1, // shifts + and/or/xor (i,l)
        0x85..=0x93 => 1, // conversions i2l..i2s
        0x94..=0x98 => 1, // lcmp, fcmpl/g, dcmpl/g
        0x57..=0x5f => 1, // pop..swap
        0x84 => 3,        // iinc
        0x99..=0x9e => 3, // if<cond>
        0x9f..=0xa4 => 3, // if_icmp<cond>
        0xa5 | 0xa6 => 3, // if_acmpeq / if_acmpne (reference identity, via callback)
        0xa7 => 3,        // goto
        0xac..=0xb1 => 1, // ireturn..return
        0xb8 => 3,        // invokestatic (intrinsics accepted in emit, else callback)
        0xb6 | 0xb7 => 3, // invokevirtual / invokespecial
        0xb9 => 5,        // invokeinterface (index + count + 0)
        #[cfg(feature = "next-gen-gc")]
        0xba => 5,
        0xbb => 3,        // new (via callback)
        0xbc => 2,        // newarray (primitive, via callback)
        0xbd => 3,        // anewarray (reference, via callback)
        0xbf => 1,        // athrow (emit_op raises via rava_throw)
        0xc0 | 0xc1 => 3, // checkcast / instanceof
        0xc6 | 0xc7 => 3, // ifnull / ifnonnull
        _ => return None,
    })
}

/// Big-endian i32 at `at`, bounds-checked.
pub(crate) fn be_i32(bc: &[u8], at: usize) -> Option<i32> {
    if at + 4 <= bc.len() {
        Some(i32::from_be_bytes([bc[at], bc[at + 1], bc[at + 2], bc[at + 3]]))
    } else {
        None
    }
}

/// Instruction length INCLUDING the variable-length switches (0xaa tableswitch /
/// 0xab lookupswitch), whose size depends on 4-byte alignment padding + the case
/// count — so it needs `pc`, unlike `insn_len`. Every bytecode walk over a method
/// that may contain a switch must use this (not `insn_len`). The switch payload
/// starts at the next 4-byte boundary after `pc` (JVM spec: defaultbyte is aligned
/// to a multiple of 4 from method start).
pub(crate) fn insn_len_at(bc: &[u8], pc: usize) -> Option<usize> {
    match bc[pc] {
        0xaa => {
            let base = (pc & !3) + 4; // padded to 4-byte boundary
            let low = be_i32(bc, base + 4)?;
            let high = be_i32(bc, base + 8)?;
            if high < low {
                return None;
            }
            let n = (high as i64 - low as i64 + 1) as usize;
            Some((base - pc) + 12 + n * 4)
        }
        0xab => {
            let base = (pc & !3) + 4;
            let npairs = be_i32(bc, base + 4)?;
            if npairs < 0 {
                return None;
            }
            Some((base - pc) + 8 + npairs as usize * 8)
        }
        op => insn_len(op),
    }
}

/// Decode a switch at `pc` into `(default target, [(match value, target)])`, all
/// targets ABSOLUTE pcs (`pc + offset`, the offset being relative to the switch
/// opcode per the JVM spec). `None` if malformed/out of range.
pub(crate) fn switch_targets(bc: &[u8], pc: usize) -> Option<(usize, Vec<(i32, usize)>)> {
    let base = (pc & !3) + 4;
    let abs = |off: i32| -> usize { (pc as i64 + off as i64) as usize };
    match bc[pc] {
        0xaa => {
            let default_t = abs(be_i32(bc, base)?);
            let low = be_i32(bc, base + 4)?;
            let high = be_i32(bc, base + 8)?;
            if high < low {
                return None;
            }
            let mut cases = Vec::new();
            for i in 0..=(high - low) {
                let off = be_i32(bc, base + 12 + i as usize * 4)?;
                cases.push((low + i, abs(off)));
            }
            Some((default_t, cases))
        }
        0xab => {
            let default_t = abs(be_i32(bc, base)?);
            let npairs = be_i32(bc, base + 4)?;
            if npairs < 0 {
                return None;
            }
            let mut cases = Vec::new();
            for k in 0..npairs as usize {
                let p = base + 8 + k * 8;
                let m = be_i32(bc, p)?;
                let off = be_i32(bc, p + 4)?;
                cases.push((m, abs(off)));
            }
            Some((default_t, cases))
        }
        _ => None,
    }
}

/// Opcodes deliberately not compiled. Previously ldiv/lrem (trap on zero/MIN-1)
/// and frem/drem (need a libcall) bailed the whole method; they now compile via
/// the i64-divrem and float-rem libcalls, so nothing is excluded. Kept as the
/// single gate so a future trapping/unsupported op can be re-added in one place.
pub(crate) fn is_excluded(_op: u8) -> bool {
    false
}

/// Methods with callbacks or side-effecting bytecodes cannot be run in
/// JIT-verify mode: verify executes the interpreter after the compiled body,
/// which would double-apply mutations or nested call effects.
pub(crate) fn has_verify_unsafe_ops(code: &Code) -> bool {
    let bc = &code.bytecode;
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        if matches!(
            op,
            0x4f..=0x56 // array stores
                | 0xb2  // getstatic can run class initialization
                | 0xb3  // putstatic
                | 0xb5  // putfield
                | 0xb6  // invokevirtual
                | 0xb7  // invokespecial
                | 0xb8  // invokestatic
                | 0xb9  // invokeinterface
                | 0xba  // invokedynamic
                | 0xbb..=0xbd // new / newarray / anewarray (allocate — a 2nd verify run would alloc again)
        ) {
            return true;
        }
        let Some(len) = insn_len_at(bc, pc) else {
            return false;
        };
        pc += len;
    }
    false
}

pub(crate) fn invokestatic_indices(code: &Code) -> Vec<u16> {
    let bc = &code.bytecode;
    let mut out = Vec::new();
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return out };
        if op == 0xb8 && pc + 2 < bc.len() {
            out.push(((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16);
        }
        pc += len;
    }
    out
}

/// (cp index, opcode) of every invoke site (0xb6 virtual / 0xb7 special /
/// 0xb8 static / 0xb9 interface). Used to credit invokes whose callee is already
/// compiled as native work in the callback_heavy gate.
pub(crate) fn invoke_indices(code: &Code) -> Vec<(u16, u8)> {
    let bc = &code.bytecode;
    let mut out = Vec::new();
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return out };
        if matches!(op, 0xb6..=0xb9) && pc + 2 < bc.len() {
            out.push((((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16, op));
        }
        pc += len;
    }
    out
}

/// Constant-pool indices of every `invokespecial` (0xb7) site — the inline
/// collector resolves each to its statically-bound instance callee.
pub(crate) fn invokespecial_indices(code: &Code) -> Vec<u16> {
    let bc = &code.bytecode;
    let mut out = Vec::new();
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = insn_len_at(bc, pc) else { return out };
        if op == 0xb7 && pc + 2 < bc.len() {
            out.push(((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16);
        }
        pc += len;
    }
    out
}

/// One inline field store extracted from a trivial constructor: a constructor
/// argument written straight into a receiver field. `arg_index` is the operand-
/// stack argument (0-based, descriptor order); the field is identified by its
/// declaring owner/name (resolved to id + layout slot by the caller).
#[cfg(feature = "next-gen-gc")]
/// Decode a single local-load opcode at `pc` → (local index, type, length), or
/// `None` if it is not a plain load. Covers the short forms (`*load_0..3`) and the
/// indexed forms (`*load idx`). Used by the trivial-constructor analyzer.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn decode_load(bc: &[u8], pc: usize) -> Option<(usize, JT, usize)> {
    let op = *bc.get(pc)?;
    match op {
        0x1a..=0x1d => Some(((op - 0x1a) as usize, JT::Int, 1)),
        0x1e..=0x21 => Some(((op - 0x1e) as usize, JT::Long, 1)),
        0x22..=0x25 => Some(((op - 0x22) as usize, JT::Float, 1)),
        0x26..=0x29 => Some(((op - 0x26) as usize, JT::Double, 1)),
        0x2a..=0x2d => Some(((op - 0x2a) as usize, JT::Ref, 1)),
        0x15 => Some((*bc.get(pc + 1)? as usize, JT::Int, 2)),
        0x16 => Some((*bc.get(pc + 1)? as usize, JT::Long, 2)),
        0x17 => Some((*bc.get(pc + 1)? as usize, JT::Float, 2)),
        0x18 => Some((*bc.get(pc + 1)? as usize, JT::Double, 2)),
        0x19 => Some((*bc.get(pc + 1)? as usize, JT::Ref, 2)),
        _ => None,
    }
}

/// ESCAPE ANALYSIS (next-gen-gc): for each object created at a `new` (0xbb) site,
/// decide whether its reference ESCAPES the method — stored where the GC or another
/// thread can still reach it after the call (a field of ANOTHER object, a static, an
/// array, the return value, an exception, a monitor, or an argument passed OUT to a
/// call). A NON-escaping object is a Scalar-Replacement-of-Aggregates (SROA)
/// candidate: its fields can live in registers / on the stack with NO arena alloc.
///
/// Sound + conservative. Control flow (any branch/switch/back-edge) → every object
/// is marked ESCAPED (the straight-line taint walk cannot track merges). The walk
/// taints the newborn reference and follows it through `dup`/`astore`/`aload`; the
/// ctor's own `invokespecial <init>` and field reads/writes on the object itself are
/// safe consumers — anything else that consumes a tainted reference escapes.
/// Returns `(new_pc, escapes)` per site.
/// Why a `new`-site object is (not) a scalar-replacement candidate. Distinguishing
/// CONTROL-FLOW (the straight-line walk gives up, but the object might still be
/// confined under a CF-aware analysis) from a real SINK escape is the key VOA metric:
/// it measures how much of the opportunity is blocked by the analysis's own
/// straight-line limitation vs a genuine escape.
#[cfg(feature = "next-gen-gc")]
/// (pops, pushes_one) for an opcode that neither moves nor sinks a reference taint
/// (its result is always clean). `pops == usize::MAX` ⇒ unmodelled (caller is
/// conservative). Long/double count as ONE value here (taint tracking only needs ref
/// slots, which are single — pop COUNTS must just be self-consistent).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn clean_stack_effect(op: u8, bc: &[u8], pc: usize) -> (usize, bool) {
    match op {
        0x00 => (0, false),                                   // nop
        0x01..=0x0f => (0, true),                             // *const_*
        0x10 | 0x11 | 0x12 => (0, true),                      // bipush/sipush/ldc
        0x13 | 0x14 => (0, true),                             // ldc_w / ldc2_w
        0x15..=0x18 => (0, true),                             // iload/lload/fload/dload
        0x1a..=0x29 => (0, true),                             // *load_0..3 (non-ref)
        0x36..=0x39 => (1, false),                            // istore/lstore/fstore/dstore
        0x3b..=0x4a => (1, false),                            // i/l/f/d store_0..3
        0x57 => (1, false),                                   // pop
        0x58 => (2, false),                                   // pop2
        0x2e..=0x35 => (2, true),                             // *aload (arr,idx)→val
        0x60..=0x73 => (2, true),                             // add/sub/mul/div/rem (binary)
        0x74..=0x77 => (1, true),                             // ineg/lneg/fneg/dneg
        0x78..=0x83 => (2, true),                             // sh*/and/or/xor
        0x85..=0x93 => (1, true),                             // i2l..i2s conversions
        0x94..=0x98 => (2, true),                             // lcmp/fcmp*/dcmp*
        0xb2 => (0, true),                                    // getstatic
        0xbc => (1, true),                                    // newarray
        0xbd => (1, true),                                    // anewarray
        0xbe => (1, true),                                    // arraylength
        0xc1 => (1, true),                                    // instanceof
        0xb1 => (0, false),                                   // return (void)
        0xac | 0xae => (1, false),                            // ireturn/freturn
        0xad | 0xaf => (1, false),                            // lreturn/dreturn
        0x84 => (0, false),                                   // iinc
        _ => {
            let _ = (bc, pc);
            (usize::MAX, false)
        }
    }
}

/// TRIVIAL CONSTRUCTOR ANALYZER (next-gen-gc). Recognise an `<init>` whose entire
/// body is `aload_0; invokespecial Object.<init>()V; (aload_0; *load argX;
/// putfield f)* ; return` — a pure field-setter constructor. Returns the ordered
/// store plan (each constructor arg → the field it writes), or `None` for anything
/// non-trivial (branches, other ops, a non-Object super, exception handlers).
///
/// The JIT then inlines such a constructor at its `invokespecial` site: the args
/// are stored straight into the freshly bump-allocated arena object's cells with
/// NO `rava_invoke` callback — closing the last per-allocation callback on
/// value-class construction (e.g. `Allocs.Point`).
/// TRIVIAL GETTER ANALYZER (next-gen-gc, synergy Layer 1). Recognise a method whose
/// whole body is `aload_0; getfield F; Xreturn` — a pure field reader — and return
/// `(field owner, field name, type)`. The speculative inliner folds such a call to a
/// direct field read at the call site, which (when the receiver is a scalar-replaced
/// virtual object) becomes a register read with no allocation and no callback.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn s16(bc: &[u8], pc: usize) -> i32 {
    ((bc[pc] as i16) << 8 | bc[pc + 1] as i16) as i32
}
