//! The expression-shaped body: a method whose whole code is one value computation.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

/// Emit the data-parallel loop's per-iteration BODY EXPRESSION (`dp.body_range`)
/// into IR, given the counter value `i_val` (an `i32`). A self-contained
/// stack-machine translator over exactly the pure-integer opcodes
/// `is_parallel_body_op` admits — counter-dependent arithmetic only, no locals
/// except the induction var, no division (div-by-zero would trap in a worker), no
/// calls/allocs/branches. Returns `(value, is_long)` or `None` if the body uses an
/// opcode outside this safe subset (the caller then keeps the serial path).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn emit_body_expr(
    b: &mut FunctionBuilder,
    bc: &[u8],
    range: (usize, usize),
    i_val: Value,
    iv_local: usize,
    scratch: &[(usize, bool)],
    skip: &[usize],
    // `invokestatic Math.max/min` sites (pc → is_max) the dispatch resolved — emitted as
    // signed smax/smin. Empty for the reduce path (it never has an in-body static call).
    math_ops: &[(usize, bool)],
    // `ldc2_w` sites (pc → the long value / double bit-pattern) the dispatch resolved.
    // Empty for the reduce path.
    const_ops: &[(usize, i64)],
    sroa: &SroaCtx,
    array_captures: &[(usize, bool)],
    read_local: &mut dyn FnMut(&mut FunctionBuilder, usize, bool) -> Option<Value>,
    read_array_elem: &mut dyn FnMut(&mut FunctionBuilder, usize, Value, bool) -> Option<Value>,
) -> Option<(Value, bool)> {
    // Isolated Cranelift Variable per scratch slot, zero-initialised at body entry
    // (re-defined each iteration by the body's stores → no cross-iteration carry).
    let mut scratch_vars: std::collections::HashMap<usize, (Variable, bool)> =
        std::collections::HashMap::new();
    // ONE zero per width for the whole seed loop instead of one per scratch slot. The egraph
    // GVNs identical `iconst`s and remats them per block, so the machine code is the same —
    // what shrinks is the number of defs the value numbering and `regalloc2`'s interference
    // walk see. `fast_codegen()` off restores the per-slot constant.
    let fastcg = fast_codegen();
    let (mut z32, mut z64): (Option<Value>, Option<Value>) = (None, None);
    for &(slot, is_long) in scratch {
        let ty = if is_long { I64 } else { I32 };
        let var = b.declare_var(ty);
        let cache = if is_long { &mut z64 } else { &mut z32 };
        let zero = if fastcg {
            *cache.get_or_insert_with(|| b.ins().iconst(ty, 0))
        } else {
            b.ins().iconst(ty, 0)
        };
        b.def_var(var, zero);
        scratch_vars.insert(slot, (var, is_long));
    }
    // SROA: a CONFINED short-lived object is a "virtual object" — a compile-time map
    // field_id → (SSA value, is_long). NO arena allocation; `getfield`/`putfield`
    // route to these registers. `vobjs` holds them; `ref_locals` maps an astore'd
    // ref local → its virtual object; the operand stack holds `BVal::Virt` markers.
    let mut vobjs: Vec<std::collections::HashMap<u32, (Value, bool)>> = Vec::new();
    let mut ref_locals: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut stack: Vec<BVal> = Vec::new();
    // Pop a SCALAR; `None` (→ serial bail) if the top is a virtual-object marker
    // (a confined object can't be an arithmetic operand — its escape would be one).
    fn pop_s(s: &mut Vec<BVal>) -> Option<(Value, bool)> {
        match s.pop()? {
            BVal::Scalar(v, l) => Some((v, l)),
            BVal::Virt(_) | BVal::ArrRef(_) => None,
        }
    }
    let mut pc = range.0;
    while pc < range.1 {
        // Skip instructions the caller excludes from the body value — the reduce's `load
        // acc`, or a MAP's store-target `aload out; iload iv` (resolved separately).
        if skip.contains(&pc) {
            pc += insn_len_at(bc, pc)?;
            continue;
        }
        let op = bc[pc];
        let len = insn_len_at(bc, pc)?;
        let cp = || ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
        match op {
            0x02..=0x08 => stack.push(BVal::Scalar(b.ins().iconst(I32, (op as i64) - 3), false)),
            0x09 | 0x0a => stack.push(BVal::Scalar(b.ins().iconst(I64, (op as i64) - 9), true)),
            0x10 => stack.push(BVal::Scalar(b.ins().iconst(I32, *bc.get(pc + 1)? as i8 as i64), false)),
            0x11 => stack.push(BVal::Scalar(b.ins().iconst(I32, (((*bc.get(pc + 1)? as i8 as i32) << 8) | *bc.get(pc + 2)? as i32) as i64), false)),
            // iload: counter → `i_val`; scratch slot → its Variable; else captured param.
            0x15 | 0x1a..=0x1d => {
                let local = if op == 0x15 { *bc.get(pc + 1)? as usize } else { (op - 0x1a) as usize };
                let v = if local == iv_local {
                    i_val
                } else if let Some(&(var, _)) = scratch_vars.get(&local) {
                    b.use_var(var)
                } else {
                    read_local(b, local, false)?
                };
                stack.push(BVal::Scalar(v, false));
            }
            // lload / dload (a double is read as its i64 bit-pattern, same 8-byte cell).
            0x16 | 0x18 | 0x1e..=0x21 | 0x26..=0x29 => {
                let local = match op {
                    0x16 | 0x18 => *bc.get(pc + 1)? as usize,
                    0x1e..=0x21 => (op - 0x1e) as usize,
                    _ => (op - 0x26) as usize,
                };
                let v = if let Some(&(var, _)) = scratch_vars.get(&local) {
                    b.use_var(var)
                } else {
                    read_local(b, local, true)?
                };
                stack.push(BVal::Scalar(v, true));
            }
            // aload: a captured primitive array (→ ArrRef) or a virtual-object ref.
            0x19 | 0x2a..=0x2d => {
                let local = if op == 0x19 { *bc.get(pc + 1)? as usize } else { (op - 0x2a) as usize };
                if array_captures.iter().any(|(l, _)| *l == local) {
                    stack.push(BVal::ArrRef(local));
                } else {
                    let vid = *ref_locals.get(&local)?; // not a known virtual → bail
                    stack.push(BVal::Virt(vid));
                }
            }
            // iaload / laload: `arr[idx]` from a captured primitive array. The fork gate
            // proved idx < len (trip <= length), so no bounds check is needed in the
            // worker; the index is the counter expression (a scalar). Element width
            // comes from the opcode (iaload=int, laload=long).
            0x2e | 0x2f | 0x31 => {
                let (idx, _) = pop_s(&mut stack)?;
                let local = match stack.pop()? {
                    BVal::ArrRef(l) => l,
                    _ => return None, // load off a non-captured-array ref → serial
                };
                // daload (0x31) reads the double's 8-byte cell as its i64 bit-pattern.
                let is_long = op == 0x2f || op == 0x31;
                let v = read_array_elem(b, local, idx, is_long)?;
                stack.push(BVal::Scalar(v, is_long));
            }
            // istore / lstore: scratch Variable. astore: move a virtual ref to a local.
            0x36 | 0x3b..=0x3e => {
                let local = if op == 0x36 { *bc.get(pc + 1)? as usize } else { (op - 0x3b) as usize };
                let (v, _) = pop_s(&mut stack)?;
                let &(var, _) = scratch_vars.get(&local)?;
                b.def_var(var, v);
            }
            0x37 | 0x3f..=0x42 => {
                let local = if op == 0x37 { *bc.get(pc + 1)? as usize } else { (op - 0x3f) as usize };
                let (v, _) = pop_s(&mut stack)?;
                let &(var, _) = scratch_vars.get(&local)?;
                b.def_var(var, v);
            }
            0x3a | 0x4b..=0x4e => {
                let local = if op == 0x3a { *bc.get(pc + 1)? as usize } else { (op - 0x4b) as usize };
                match stack.pop()? {
                    BVal::Virt(vid) => {
                        ref_locals.insert(local, vid);
                    }
                    _ => return None, // astore of a non-virtual ref (scalar/array) → bail
                }
            }
            // ── SROA materialization: new / ctor / getfield / putfield ──
            0xbb => {
                // CONFINED arena-eligible class → a virtual object, NO allocation.
                if !sroa.new_arena.contains_key(&cp()) {
                    return None; // native / non-arena class → can't scalar-replace
                }
                vobjs.push(std::collections::HashMap::new());
                stack.push(BVal::Virt(vobjs.len() - 1));
            }
            0xb7 => {
                // invokespecial <init> on the virtual object: a trivial ctor's field
                // stores route straight into the virtual object's field registers.
                let ci = sroa.ctor_inlines.get(&cp())?;
                let n = ci.n_params;
                let mut args: Vec<(Value, bool)> = Vec::with_capacity(n);
                for _ in 0..n {
                    args.push(pop_s(&mut stack)?);
                }
                args.reverse(); // popped in reverse → restore arg order
                let vid = match stack.pop()? {
                    BVal::Virt(v) => v,
                    _ => return None,
                };
                for &(arg_index, _slot, field_id, jt) in &ci.stores {
                    let (v, _) = *args.get(arg_index)?;
                    vobjs[vid].insert(field_id, (v, matches!(jt, JT::Long)));
                }
            }
            0xb4 => {
                // getfield: read the virtual object's field register (no memory load).
                let (field_id, jt, _) = *sroa.fields.get(&cp())?;
                let vid = match stack.pop()? {
                    BVal::Virt(v) => v,
                    _ => return None, // getfield on a real object / array → bail
                };
                let &(v, _) = vobjs[vid].get(&field_id)?; // uninitialised field → bail
                stack.push(BVal::Scalar(v, matches!(jt, JT::Long)));
            }
            0xb5 => {
                // putfield onto the virtual object: update its field register.
                let (field_id, jt, _) = *sroa.fields.get(&cp())?;
                let (v, _) = pop_s(&mut stack)?;
                let vid = match stack.pop()? {
                    BVal::Virt(v) => v,
                    _ => return None,
                };
                vobjs[vid].insert(field_id, (v, matches!(jt, JT::Long)));
            }
            0xb6 => {
                // invokevirtual: only a TRIVIAL GETTER on a virtual object is folded —
                // `recv.getF()` becomes a read of the virtual object's field register
                // (synergy of getter inline + SROA). Any other call escapes → bail.
                let &(field_id, jt) = sroa.getters.get(&cp())?;
                let vid = match stack.pop()? {
                    BVal::Virt(v) => v,
                    _ => return None, // getter on a real object / array → serial
                };
                let &(v, _) = vobjs[vid].get(&field_id)?;
                stack.push(BVal::Scalar(v, matches!(jt, JT::Long)));
            }
            0x14 => {
                // ldc2_w long/double literal — the dispatch resolved this pc into `const_ops`
                // (pc → value bits). A double rides its i64 bits; a long is the value.
                let v = const_ops.iter().find(|e| e.0 == pc).map(|e| e.1);
                let Some(v) = v else { return None };
                stack.push(BVal::Scalar(b.ins().iconst(I64, v), true));
            }
            0xb8 => {
                // invokestatic Math.max/min — the dispatch resolved this pc into `math_ops`
                // (pc → is_max); emit signed smax/smin (element-wise clamp/max maps). Any
                // other static call isn't in math_ops → serial.
                let is_max = math_ops.iter().find(|e| e.0 == pc).map(|e| e.1);
                let Some(is_max) = is_max else { return None };
                let (y, yl) = pop_s(&mut stack)?;
                let (x, xl) = pop_s(&mut stack)?;
                let long = xl || yl;
                let (x2, y2) = if long {
                    let x = if xl { x } else { b.ins().sextend(I64, x) };
                    let y = if yl { y } else { b.ins().sextend(I64, y) };
                    (x, y)
                } else {
                    (x, y)
                };
                let v = if is_max { b.ins().smax(x2, y2) } else { b.ins().smin(x2, y2) };
                stack.push(BVal::Scalar(v, long));
            }
            // int binary ops (both operands i32).
            0x60 | 0x64 | 0x68 | 0x7e | 0x80 | 0x82 | 0x78 | 0x7a | 0x7c => {
                let (y, _) = pop_s(&mut stack)?;
                let (x, _) = pop_s(&mut stack)?;
                let v = match op {
                    0x60 => b.ins().iadd(x, y),
                    0x64 => b.ins().isub(x, y),
                    0x68 => b.ins().imul(x, y),
                    0x7e => b.ins().band(x, y),
                    0x80 => b.ins().bor(x, y),
                    0x82 => b.ins().bxor(x, y),
                    0x78 => b.ins().ishl(x, y),
                    0x7a => b.ins().sshr(x, y),
                    _ => b.ins().ushr(x, y),
                };
                stack.push(BVal::Scalar(v, false));
            }
            0x61 | 0x65 | 0x69 | 0x7f | 0x81 | 0x83 | 0x79 | 0x7b | 0x7d => {
                let (y, _) = pop_s(&mut stack)?;
                let (x, _) = pop_s(&mut stack)?;
                let v = match op {
                    0x61 => b.ins().iadd(x, y),
                    0x65 => b.ins().isub(x, y),
                    0x69 => b.ins().imul(x, y),
                    0x7f => b.ins().band(x, y),
                    0x81 => b.ins().bor(x, y),
                    0x83 => b.ins().bxor(x, y),
                    0x79 => b.ins().ishl(x, y),
                    0x7b => b.ins().sshr(x, y),
                    _ => b.ins().ushr(x, y),
                };
                stack.push(BVal::Scalar(v, true));
            }
            // ── double arithmetic (MAP only) — a double rides an i64 cell as its bit
            //    pattern; bitcast in, do the IEEE op, bitcast the result back to i64. ──
            0x63 | 0x67 | 0x6b | 0x6f => {
                let (y, _) = pop_s(&mut stack)?;
                let (x, _) = pop_s(&mut stack)?;
                let xf = b.ins().bitcast(F64, MemFlags::new(), x);
                let yf = b.ins().bitcast(F64, MemFlags::new(), y);
                let rf = match op {
                    0x63 => b.ins().fadd(xf, yf),
                    0x67 => b.ins().fsub(xf, yf),
                    0x6b => b.ins().fmul(xf, yf),
                    _ => b.ins().fdiv(xf, yf),
                };
                stack.push(BVal::Scalar(b.ins().bitcast(I64, MemFlags::new(), rf), true));
            }
            0x77 => {
                // dneg
                let (x, _) = pop_s(&mut stack)?;
                let xf = b.ins().bitcast(F64, MemFlags::new(), x);
                let rf = b.ins().fneg(xf);
                stack.push(BVal::Scalar(b.ins().bitcast(I64, MemFlags::new(), rf), true));
            }
            0x0e | 0x0f => {
                // dconst_0 / dconst_1 → the constant's bit pattern in an i64 cell.
                let bits = if op == 0x0e { 0i64 } else { 1.0f64.to_bits() as i64 };
                stack.push(BVal::Scalar(b.ins().iconst(I64, bits), true));
            }
            0x87 | 0x8a => {
                // i2d / l2d → signed int/long to f64, kept as i64 bits.
                let (x, _) = pop_s(&mut stack)?;
                let f = b.ins().fcvt_from_sint(F64, x);
                stack.push(BVal::Scalar(b.ins().bitcast(I64, MemFlags::new(), f), true));
            }
            // ineg / lneg. WAS `isub(iconst 0, x)` — a materialised zero PER NEGATION plus a
            // subtract, i.e. two IR instructions and one extra def for what x86-64 spells as a
            // single `neg`. IS the `ineg` opcode Cranelift already has, which lowers to exactly
            // the same instruction. `fast_codegen()` off restores the zero-and-subtract form.
            0x74 => {
                let (x, l) = pop_s(&mut stack)?;
                let v = if fastcg {
                    b.ins().ineg(x)
                } else {
                    let z = b.ins().iconst(if l { I64 } else { I32 }, 0);
                    b.ins().isub(z, x)
                };
                stack.push(BVal::Scalar(v, l));
            }
            0x75 => {
                let (x, _) = pop_s(&mut stack)?;
                let v = if fastcg {
                    b.ins().ineg(x)
                } else {
                    let z = b.ins().iconst(I64, 0);
                    b.ins().isub(z, x)
                };
                stack.push(BVal::Scalar(v, true));
            }
            0x85 => {
                let (x, _) = pop_s(&mut stack)?;
                stack.push(BVal::Scalar(b.ins().sextend(I64, x), true)); // i2l
            }
            0x88 => {
                let (x, _) = pop_s(&mut stack)?;
                stack.push(BVal::Scalar(b.ins().ireduce(I32, x), false)); // l2i
            }
            0x91 => {
                let (x, _) = pop_s(&mut stack)?;
                let r = b.ins().ireduce(I8, x);
                stack.push(BVal::Scalar(b.ins().sextend(I32, r), false)); // i2b
            }
            0x92 => {
                let (x, _) = pop_s(&mut stack)?;
                stack.push(BVal::Scalar(b.ins().band_imm(x, 0xFFFF), false)); // i2c
            }
            0x93 => {
                let (x, _) = pop_s(&mut stack)?;
                let sh = b.ins().ishl_imm(x, 16);
                stack.push(BVal::Scalar(b.ins().sshr_imm(sh, 16), false)); // i2s
            }
            0x59 => {
                let top = stack.last()?.clone();
                stack.push(top); // dup (aliases a virtual object — shared idx)
            }
            0x57 => {
                stack.pop()?; // pop (discard — a virtual object just disappears)
            }
            0x58 => match stack.pop()? {
                BVal::Scalar(_, l) => {
                    if !l {
                        stack.pop()?;
                    }
                }
                BVal::Virt(_) | BVal::ArrRef(_) => return None,
            },
            0x5f => {
                let n = stack.len();
                if n < 2 {
                    return None;
                }
                stack.swap(n - 1, n - 2); // swap
            }
            _ => return None, // outside the safe subset → serial
        }
        pc += len;
    }
    if stack.len() != 1 {
        return None;
    }
    pop_s(&mut stack)
}
