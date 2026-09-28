//! Locals and cells: moving a value between a Cranelift variable, a rooted cell and the ABI.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// `iload local` index at `pc`: `0x15`→`bc[pc+1]`; `0x1a..=0x1d`→`op-0x1a`.
pub(crate) fn iload_local(bc: &[u8], pc: usize) -> Option<usize> {
    match *bc.get(pc)? {
        0x15 => Some(*bc.get(pc + 1)? as usize),
        op @ 0x1a..=0x1d => Some((op - 0x1a) as usize),
        _ => None,
    }
}

/// `aload local` index at `pc`: `0x19`→`bc[pc+1]`; `0x2a..=0x2d`→`op-0x2a`.
pub(crate) fn aload_local(bc: &[u8], pc: usize) -> Option<usize> {
    match *bc.get(pc)? {
        0x19 => Some(*bc.get(pc + 1)? as usize),
        op @ 0x2a..=0x2d => Some((op - 0x2a) as usize),
        _ => None,
    }
}

/// `astore local` index at `pc`: `0x3a`→`bc[pc+1]`; `0x4b..=0x4e`→`op-0x4b`;
/// also `wide astore` (`0xc4 0x3a` + u16 index). Used to detect a write to the
/// loop array inside the body (which would invalidate the length invariant).
pub(crate) fn astore_local(bc: &[u8], pc: usize) -> Option<usize> {
    match *bc.get(pc)? {
        0x3a => Some(*bc.get(pc + 1)? as usize),
        op @ 0x4b..=0x4e => Some((op - 0x4b) as usize),
        0xc4 if *bc.get(pc + 1)? == 0x3a => {
            Some(((*bc.get(pc + 2)? as usize) << 8) | *bc.get(pc + 3)? as usize)
        }
        _ => None,
    }
}

/// aload-local at `p` (for the reduce2 return-expr guard): a1oad/aload_0..3 → the local index.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn aload_local_at(bc: &[u8], p: usize) -> Option<usize> {
    match bc[p] { 0x19 => Some(*bc.get(p + 1)? as usize), op @ 0x2a..=0x2d => Some((op - 0x2a) as usize), _ => None }
}

/// Field/element descriptor to `JT` (`I/Z/B/C/S`->Int, `J`->Long, `F`->Float,
/// `D`->Double, anything else (`L...`/`[...`)->Ref).
pub(crate) fn desc_to_jt(desc: &str) -> JT {
    match desc.bytes().next() {
        Some(b'I') | Some(b'Z') | Some(b'B') | Some(b'C') | Some(b'S') => JT::Int,
        Some(b'J') => JT::Long,
        Some(b'F') => JT::Float,
        Some(b'D') => JT::Double,
        _ => JT::Ref,
    }
}

/// cp-indices of getfield/putfield sites whose field is DECLARED an array (`[…`).
///
/// `resolve_fields` collapses the descriptor to a `JT`, and `desc_to_jt` maps both `L…;` and
/// `[…` to `JT::Ref` — correct for choosing a cell width, and exactly the information loss
/// that makes arrays look like flip blockers. A field declared `[I` can never hold a plain
/// object reference, so the bare-deref flip needs no runtime tag test there: measured on a
/// full boot, **211,141 of 211,343 array cells sit in such fields, against 202 in
/// object-typed ones**. Kept as a separate walk rather than widening the field tuple, which
/// is threaded through eight signatures and four call sites.
/// Must a field with this DECLARED descriptor keep its runtime tag test under the flip?
///
/// The flip's precondition is usually stated as "no non-0b00 ref cell may persist", which is
/// a whole-heap property and therefore all-or-nothing. But the emitter does not need a
/// whole-heap property — it needs to know, PER SITE, whether the cell it is about to
/// dereference can hold anything but a bare arena pointer. The declared descriptor answers
/// that statically in three cases:
///
///   `[…`                  an array. Never a plain object reference (211,141 cells measured).
///   `Ljava/lang/Object;`  can hold ANY representation, so the test is real information.
///   a native-backed type  `java/util/ArrayList`, fastutil, gson-internal … are force-handled
///                         at birth *because* their state lives in synthetic `$elems`/`$map`
///                         fields, so a field declared as one always holds a handle.
///
/// Everything else — a field declared as an ordinary jar class — holds an arena object, and
/// that is where the bare load is both sound and worth having. ★This turns the flip from a
/// heap-wide invariant into a per-site question the constant pool already answers, which is
/// what makes it armable at 94.6% coverage instead of needing 100%.
/// Census-visible wrapper: the census must size the residue by the EMITTER's own rule, not by
/// a re-derivation of it, or the two drift and the number stops meaning anything.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn descriptor_keeps_tag_pub(desc: &str) -> bool {
    descriptor_keeps_tag(desc)
}

pub(crate) fn descriptor_keeps_tag(desc: &str) -> bool {
    if desc.starts_with('[') {
        return true;
    }
    let Some(cls) = desc.strip_prefix('L').and_then(|d| d.strip_suffix(';')) else {
        return false; // a primitive descriptor never reaches the ref path
    };
    if cls == "java/lang/Object" {
        return true;
    }
    crate::opcodes::birth_native_backed_alloc(cls)
}

/// Return type of a method descriptor as a `JT` (`None` = void only; a reference
/// return is `Some(JT::Ref)` — the result cell is a tagged handle / box index).
pub(crate) fn ret_jt(descriptor: &str) -> Option<JT> {
    let r = crate::descriptor::ret_desc(descriptor);
    match r.chars().next() {
        Some('I') | Some('Z') | Some('B') | Some('C') | Some('S') => Some(JT::Int),
        Some('J') => Some(JT::Long),
        Some('F') => Some(JT::Float),
        Some('D') => Some(JT::Double),
        Some('V') | None => None, // void
        _ => Some(JT::Ref),       // L... / [... reference return
    }
}

/// Total local slots the incoming arguments occupy for a method with this
/// descriptor (receiver included unless static; wide params take 2). The
/// Deep-IC v3 prologue zeroes everything ABOVE this in its frame buffer.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn desc_args_slots(descriptor: &str, is_static: bool) -> u16 {
    let total: usize = arg_slot_widths(descriptor).iter().sum();
    (if is_static { total - 1 } else { total }) as u16
}

/// JVM slot width per call argument for an INSTANCE call: `[1 /* receiver */,
/// w(p0), w(p1), …]` where a long/double param is 2 (its pair slot stays
/// unused). Array-of-wide is a narrow reference. Used by the dispatch PIC to
/// lay the call buffer out in the callee's LOCAL-SLOT positions.
pub(crate) fn arg_slot_widths(descriptor: &str) -> Vec<usize> {
    let b = descriptor.as_bytes();
    let mut out = vec![1usize]; // receiver
    let mut i = match descriptor.find('(') {
        Some(p) => p + 1,
        None => return out,
    };
    while i < b.len() && b[i] != b')' {
        let was_array = b[i] == b'[';
        while i < b.len() && b[i] == b'[' {
            i += 1;
        }
        match b.get(i) {
            Some(b'L') => {
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                i += 1;
                out.push(1);
            }
            Some(b'J') | Some(b'D') if !was_array => {
                i += 1;
                out.push(2);
            }
            Some(_) => {
                i += 1;
                out.push(1);
            }
            None => break,
        }
    }
    out
}

/// I0 of the register-ABI arc: classify an ALL-PRIMITIVE descriptor into a `RegAbi`, or
/// None if ANY value param OR the return is a reference (`L…`/`[…`) or void. References
/// need the GC-visible buffer ABI (the collector forwards Ref cells in the call buffer in
/// place); primitives are GC-inert, so they may travel in registers. The instance receiver
/// (`this`) is deliberately NOT included — it stays a Ref, dispatched via the normal PIC;
/// only the primitive value args skip the buffer.
pub(crate) fn all_primitive_desc(descriptor: &str) -> Option<RegAbi> {
    reg_abi_desc(descriptor, false)
}

/// REFERENCE-CAPABLE register-ABI classifier. `allow_ref` widens `all_primitive_desc`
/// to accept `L…;` / `[…` value params and a reference return as `PrimKind::Ref`.
///
/// WHY THIS IS GC-SAFE (the half that kept references out of registers).
/// A reference operand in this JIT is an i64 CELL, and the collector finds live cells
/// two ways: `Frame.locals` (interpreted frames) and the registered JIT cell buffers
/// (`frames::cells_all`, pushed by the Deep-IC v3 self-registering prologue). A cell
/// sitting only in a machine register at a safepoint is invisible to both. The
/// register ABI is nevertheless sound for references because every register-resident
/// interval is bounded by one of two proofs:
///
///  (1) GC-LEAF callee (`is_gc_leaf`): the body contains no invoke / new / newarray /
///      ldc / get-putstatic / instanceof / athrow / monitor op, so NO safepoint exists
///      anywhere inside it. A reference may stay in a register for the callee's whole
///      lifetime because nothing can collect while it runs. This is the same predicate
///      the machine-code dispatch IC's leaf HIT arm already relies on.
///
///  (2) DEEP (non-leaf) callee: the register prologue writes every incoming arg cell
///      into its own internal frame buffer and only THEN executes the self-registering
///      prologue that pushes that buffer onto `cells_all`. Between the caller's
///      argument marshalling and that push there is only straight-line code (stores,
///      a fault-flag branch, the non-arg zeroing loop) — no call, no allocation, hence
///      no safepoint. From the push onward the reference is a first-class root and a
///      moving collection forwards it in place, exactly as it forwards a memory-ABI
///      caller-passed buffer. This is not a new mechanism: `RegAbi::has_recv` already
///      passes `this` — a reference — in a register on precisely this argument.
///
/// The transport swap changes nothing about the unrooted window either: the memory-ABI
/// call buffer (`args_slot` / the direct-mono callbuf) is ITSELF not in `cells_all`
/// until the callee's prologue registers it — see `nextgen::forward_if_moved`, the read
/// barrier that exists because in-flight call buffers are unrooted. Registers and that
/// buffer are unrooted over the *same* safepoint-free interval.
///
/// The caller's own obligation is separate and unchanged: an argument is popped off the
/// operand stack, so its register copy is dead after the call and may go stale; every
/// reference the caller still needs is spilled to its rooted cell buffer by
/// `spill_for_gc` and re-read by `reload_after_gc`.
pub(crate) fn reg_abi_desc(descriptor: &str, allow_ref: bool) -> Option<RegAbi> {
    let b = descriptor.as_bytes();
    let open = descriptor.find('(')?;
    let close = descriptor.find(')')?;
    let mut params = Vec::new();
    let mut i = open + 1;
    while i < close {
        match *b.get(i)? {
            b'D' => {
                params.push(PrimKind::F64);
                i += 1;
            }
            b'F' => {
                params.push(PrimKind::F32);
                i += 1;
            }
            b'J' => {
                params.push(PrimKind::I64);
                i += 1;
            }
            b'I' | b'Z' | b'B' | b'C' | b'S' => {
                params.push(PrimKind::I32);
                i += 1;
            }
            b'[' | b'L' => {
                if !allow_ref {
                    return None;
                }
                // An array of anything is ONE narrow reference: consume the `[`s, then the
                // element (a `J`/`D` element must NOT be read as a wide scalar).
                while i < close && b[i] == b'[' {
                    i += 1;
                }
                if i >= close {
                    return None; // trailing `[` with no element
                }
                if b[i] == b'L' {
                    while i < close && b[i] != b';' {
                        i += 1;
                    }
                    if i >= close {
                        return None; // unterminated class name
                    }
                }
                i += 1;
                params.push(PrimKind::Ref);
            }
            _ => return None, // void or an unknown tag in a param position
        }
    }
    if i != close {
        return None; // a malformed descriptor over-ran the ')'
    }
    let ret = match *b.get(close + 1)? {
        b'D' => PrimKind::F64,
        b'F' => PrimKind::F32,
        b'J' => PrimKind::I64,
        b'I' | b'Z' | b'B' | b'C' | b'S' => PrimKind::I32,
        // A reference RETURN falls straight out: the register entry hands back the same
        // i64 cell the memory ABI returns, and `emit_deep_epilogue` still runs first, so
        // the box-table re-homing of a BOX-tagged result is unchanged.
        b'L' | b'[' if allow_ref => PrimKind::Ref,
        _ => return None, // void, or a reference when references are disallowed
    };
    Some(RegAbi { has_recv: false, params, ret })
}

pub(crate) fn cur_reg_abi() -> Option<RegAbi> {
    REG_ABI.with(|c| c.borrow().clone())
}

/// Convert the JIT's i64 result CELL to whatever this function actually returns: the
/// register ABI's return kind when one is active, else the cell itself (the memory ABI
/// always returns i64 bits). A `Ref` return is the cell verbatim.
pub(crate) fn abi_ret_from_cell(b: &mut FunctionBuilder, cell: Value) -> Value {
    match cur_reg_abi().map(|r| r.ret) {
        Some(PrimKind::F64) => b.ins().bitcast(F64, MemFlags::new(), cell),
        Some(PrimKind::F32) => {
            let b32 = b.ins().ireduce(I32, cell);
            b.ins().bitcast(F32, MemFlags::new(), b32)
        }
        Some(PrimKind::I32) => b.ins().ireduce(I32, cell),
        Some(PrimKind::I64) | Some(PrimKind::Ref) | None => cell,
    }
}

/// A correctly-TYPED zero for this function's return.
///
/// The early-return sites (the Deep-IC fault bail, athrow, void return) predate the
/// register ABI and hard-coded an i64 constant. That is fine for the memory ABI, whose
/// result is always i64 bits, and it stayed fine while register entries were restricted
/// to pure primitive LEAVES — a leaf has no fault bail and no athrow. The moment a
/// non-leaf register entry became expressible, an `-> i32` entry emitted `return <i64>`,
/// which the Cranelift verifier rejects; `define_function` then failed and the entry was
/// silently refused (the failure is swallowed by an `.ok()?`).
pub(crate) fn abi_zero(b: &mut FunctionBuilder) -> Value {
    let cell = b.ins().iconst(I64, 0);
    abi_ret_from_cell(b, cell)
}

/// `RAVA_REF_ABI` gate for the REFERENCE extension of the register ABI. Default OFF, so
/// an unflagged build emits byte-identical code (references keep the memory-locals call
/// buffer). With it set, an invokestatic whose descriptor carries reference params
/// and/or a reference return may pass them in registers — see `reg_abi_desc` for the
/// GC-safety argument and `jit_resolve_reg` for the eligibility gate that enforces it.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn ref_abi_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_REF_ABI").is_some())
}

pub(crate) fn jt_ty(t: JT) -> Type {
    match t {
        JT::Int => I32,
        JT::Long | JT::Ref => I64,
        JT::Float => F32,
        JT::Double => F64,
    }
}

pub(crate) fn to_cell(b: &mut FunctionBuilder, sv: SV) -> Value {
    match sv.t {
        JT::Int => b.ins().sextend(I64, sv.v),
        JT::Long => sv.v,
        JT::Float => {
            let bits = b.ins().bitcast(I32, MemFlags::new(), sv.v);
            b.ins().uextend(I64, bits)
        }
        JT::Double => b.ins().bitcast(I64, MemFlags::new(), sv.v),
        JT::Ref => sv.v, // handle already lives in an I64 value
    }
}

pub(crate) fn store_local(b: &mut FunctionBuilder, vars: &[Variable], i: usize, sv: SV) {
    let cell = to_cell(b, sv);
    b.def_var(vars[i], cell);
}

/// Convert an i64 cell (a local slot, or a callback's packed result) to the
/// typed SSA value for `t`.
pub(crate) fn cell_to_sv(b: &mut FunctionBuilder, cell: Value, t: JT) -> SV {
    let v = match t {
        JT::Int => b.ins().ireduce(I32, cell),
        JT::Long => cell,
        JT::Float => {
            let bits = b.ins().ireduce(I32, cell);
            b.ins().bitcast(F32, MemFlags::new(), bits)
        }
        JT::Double => b.ins().bitcast(F64, MemFlags::new(), cell),
        JT::Ref => cell, // handle stays in the I64 cell
    };
    SV {
        v,
        t,
        src: None,
        #[cfg(feature = "next-gen-gc")]
        arena: false,
    }
}

pub(crate) fn load_local(b: &mut FunctionBuilder, vars: &[Variable], i: usize, t: JT) -> SV {
    let cell = b.use_var(vars[i]);
    cell_to_sv(b, cell, t)
}

/// Full return kind incl. references: None = void, Some(JT) otherwise.
pub(crate) fn ret_full(descriptor: &str) -> Option<JT> {
    let r = crate::descriptor::ret_desc(descriptor);
    match r.bytes().next() {
        Some(b'I') | Some(b'Z') | Some(b'B') | Some(b'C') | Some(b'S') => Some(JT::Int),
        Some(b'J') => Some(JT::Long),
        Some(b'F') => Some(JT::Float),
        Some(b'D') => Some(JT::Double),
        Some(b'V') | None => None,
        _ => Some(JT::Ref),
    }
}

thread_local! {
    /// Set for the duration of a single `compile_register` call (single-threaded under the
    /// module lock). Read at the three register-ABI injection points (signature, prologue,
    /// return). None ⇒ the byte-identical memory-locals ABI.
    pub(crate) static REG_ABI: std::cell::RefCell<Option<RegAbi>> = const { std::cell::RefCell::new(None) };
}
