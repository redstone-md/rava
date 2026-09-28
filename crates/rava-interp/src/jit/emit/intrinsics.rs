//! Static intrinsics: detection by bytecode shape and the arithmetic each expands to.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;

pub(crate) fn detect_static_intrinsic(
    code: &Code,
    descriptor: &str,
    pool: &ConstantPool,
    nested: &mut dyn FnMut(u16) -> Option<StaticIntrinsic>,
) -> Option<StaticIntrinsic> {
    if !code.exceptions.is_empty() {
        return None;
    }
    let bc = code.bytecode.as_slice();
    match descriptor {
        "(D)I"
            if bc
                == [
                    0x26, 0x8e, 0x3d, 0x26, 0x1c, 0x87, 0x98, 0x9c, 0x00, 0x09, 0x1c, 0x04, 0x64,
                    0xa7, 0x00, 0x04, 0x1c, 0xac,
                ] =>
        {
            Some(StaticIntrinsic::FloorDToI)
        }
        "(D)J"
            if bc
                == [
                    0x26, 0x8f, 0x41, 0x26, 0x20, 0x8a, 0x98, 0x9c, 0x00, 0x09, 0x20, 0x0a, 0x65,
                    0xa7, 0x00, 0x04, 0x20, 0xad,
                ] =>
        {
            Some(StaticIntrinsic::LFloorDToJ)
        }
        "(D)D" if is_smoothstep_shape(bc, pool) => Some(StaticIntrinsic::SmoothstepD),
        "(D)D" if is_smoothstep_derivative_shape(bc, pool) => {
            Some(StaticIntrinsic::SmoothstepDerivativeD)
        }
        "(DDD)D" if bc == [0x28, 0x26, 0x18, 0x04, 0x28, 0x67, 0x6b, 0x63, 0xaf] => {
            Some(StaticIntrinsic::LerpD)
        }
        "(DDDDDD)D" if is_lerp2_shape(bc, nested) => Some(StaticIntrinsic::Lerp2D),
        "(DDDDDDDDDDD)D" if is_lerp3_shape(bc, nested) => Some(StaticIntrinsic::Lerp3D),
        _ => None,
    }
}

pub(crate) fn is_smoothstep_shape(bc: &[u8], pool: &ConstantPool) -> bool {
    bc.len() == 22
        && bc[0..7] == [0x26, 0x26, 0x6b, 0x26, 0x6b, 0x26, 0x26]
        && cp_double_is(pool, bc, 7, 6.0)
        && bc[10] == 0x6b
        && cp_double_is(pool, bc, 11, 15.0)
        && bc[14..16] == [0x67, 0x6b]
        && cp_double_is(pool, bc, 16, 10.0)
        && bc[19..22] == [0x63, 0x6b, 0xaf]
}

pub(crate) fn is_smoothstep_derivative_shape(bc: &[u8], pool: &ConstantPool) -> bool {
    bc.len() == 16
        && cp_double_is(pool, bc, 0, 30.0)
        && bc[3..16]
            == [
                0x26, 0x6b, 0x26, 0x6b, 0x26, 0x0f, 0x67, 0x6b, 0x26, 0x0f, 0x67, 0x6b, 0xaf,
            ]
}

pub(crate) fn is_lerp2_shape(bc: &[u8], nested: &mut dyn FnMut(u16) -> Option<StaticIntrinsic>) -> bool {
    bc.len() == 21
        && bc[0..6] == [0x28, 0x26, 0x18, 0x04, 0x18, 0x06]
        && invoke_cp(bc, 6).is_some_and(|cp| nested(cp) == Some(StaticIntrinsic::LerpD))
        && bc[9..14] == [0x26, 0x18, 0x08, 0x18, 0x0a]
        && invoke_cp(bc, 14).is_some_and(|cp| nested(cp) == Some(StaticIntrinsic::LerpD))
        && invoke_cp(bc, 17).is_some_and(|cp| nested(cp) == Some(StaticIntrinsic::LerpD))
        && bc[20] == 0xaf
}

pub(crate) fn is_lerp3_shape(bc: &[u8], nested: &mut dyn FnMut(u16) -> Option<StaticIntrinsic>) -> bool {
    bc.len() == 32
        && bc[0..12]
            == [
                0x18, 0x04, 0x26, 0x28, 0x18, 0x06, 0x18, 0x08, 0x18, 0x0a, 0x18, 0x0c,
            ]
        && invoke_cp(bc, 12).is_some_and(|cp| nested(cp) == Some(StaticIntrinsic::Lerp2D))
        && bc[15..25] == [0x26, 0x28, 0x18, 0x0e, 0x18, 0x10, 0x18, 0x12, 0x18, 0x14]
        && invoke_cp(bc, 25).is_some_and(|cp| nested(cp) == Some(StaticIntrinsic::Lerp2D))
        && invoke_cp(bc, 28).is_some_and(|cp| nested(cp) == Some(StaticIntrinsic::LerpD))
        && bc[31] == 0xaf
}

/// java.lang.Integer / java.lang.Long bit-twiddling intrinsics that map to a
/// single pure Cranelift op (ctz/clz/popcnt — no heap, no exceptions). These
/// are JDK-native-served, so no direct-call/PIC path exists and each call from
/// compiled code paid the full invoke gauntlet (Integer.numberOfTrailingZeros
/// ~1.4M calls per RingScan setup in the BitSet merge loops). Cranelift defines
/// ctz(0)/clz(0) = bit width — exactly Java's 32/64 contract.
pub(crate) fn intbits_intrinsic(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    owner: &str,
    name: &str,
    desc: &str,
) -> Option<()> {
    // ⚠**THE OWNER TEST WAS HALF A TEST.** `is_long` answered "is this `java/lang/Long`", and the
    // Integer arms below matched on its NEGATION — i.e. on *every other owner in the program*. A
    // guest class declaring `static int bitCount(int)` would have been lowered to `popcnt` and the
    // real method never called. Only the caller's `matches!(owner, "java/lang/Integer" |
    // "java/lang/Long")` kept that from happening, and a predicate whose correctness lives in its
    // caller is one call site away from being wrong. The recogniser now states its own domain.
    if owner != "java/lang/Integer" && owner != "java/lang/Long" {
        return None;
    }
    let is_long = owner == "java/lang/Long";
    match (name, desc, is_long) {
        ("numberOfTrailingZeros", "(I)I", false)
        | ("numberOfLeadingZeros", "(I)I", false)
        | ("bitCount", "(I)I", false) => {
            let a = stack.pop()?;
            let r = match name {
                "numberOfTrailingZeros" => b.ins().ctz(a.v),
                "numberOfLeadingZeros" => b.ins().clz(a.v),
                _ => b.ins().popcnt(a.v),
            };
            push(stack, r, JT::Int);
        }
        ("numberOfTrailingZeros", "(J)I", true)
        | ("numberOfLeadingZeros", "(J)I", true)
        | ("bitCount", "(J)I", true) => {
            let a = stack.pop()?;
            let r64 = match name {
                "numberOfTrailingZeros" => b.ins().ctz(a.v),
                "numberOfLeadingZeros" => b.ins().clz(a.v),
                _ => b.ins().popcnt(a.v),
            };
            let r = b.ins().ireduce(I32, r64);
            push(stack, r, JT::Int);
        }
        // rotateLeft/rotateRight were MISSING from this family while ctz/clz/popcnt
        // were present — the same omission class as the offload baload fix. The
        // fallback tally billed Long.rotateLeft at 654K full invoke-gauntlet
        // callbacks in one RsProbe (Xoroshiro's hot mix). Cranelift rotl/rotr mask
        // the amount to the bit width, exactly Java's "distance mod 32/64" contract
        // (negative distances included).
        ("rotateLeft", "(II)I", false) | ("rotateRight", "(II)I", false) => {
            let d = stack.pop()?;
            let a = stack.pop()?;
            let r = if name == "rotateLeft" {
                b.ins().rotl(a.v, d.v)
            } else {
                b.ins().rotr(a.v, d.v)
            };
            push(stack, r, JT::Int);
        }
        ("rotateLeft", "(JI)J", true) | ("rotateRight", "(JI)J", true) => {
            let d = stack.pop()?;
            let a = stack.pop()?;
            let r = if name == "rotateLeft" {
                b.ins().rotl(a.v, d.v)
            } else {
                b.ins().rotr(a.v, d.v)
            };
            push(stack, r, JT::Long);
        }
        // Integer.toUnsignedLong == zero-extend (x & 0xffffffffL): one machine op,
        // no heap, no exceptions. 215K gauntlet callbacks per RsProbe before this.
        ("toUnsignedLong", "(I)J", false) => {
            let a = stack.pop()?;
            let r = b.ins().uextend(I64, a.v);
            push(stack, r, JT::Long);
        }
        _ => return None,
    }
    Some(())
}

/// java.lang.Math intrinsics that map to a single pure Cranelift op (no heap,
/// no exceptions — fits the numeric model). Returns None for anything else.
pub(crate) fn math_intrinsic(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    name: &str,
    desc: &str,
) -> Option<()> {
    let un = |stack: &mut Vec<SV>, t: JT, v: Value| {
        push(stack, v, t);
    };
    match (name, desc) {
        ("sqrt", "(D)D") => {
            let a = stack.pop()?;
            let v = b.ins().sqrt(a.v);
            un(stack, JT::Double, v);
        }
        ("abs", "(D)D") => {
            let a = stack.pop()?;
            let v = b.ins().fabs(a.v);
            un(stack, JT::Double, v);
        }
        ("abs", "(F)F") => {
            let a = stack.pop()?;
            let v = b.ins().fabs(a.v);
            un(stack, JT::Float, v);
        }
        ("floor", "(D)D") => {
            let a = stack.pop()?;
            let v = b.ins().floor(a.v);
            un(stack, JT::Double, v);
        }
        ("ceil", "(D)D") => {
            let a = stack.pop()?;
            let v = b.ins().ceil(a.v);
            un(stack, JT::Double, v);
        }
        ("abs", "(I)I") => {
            let a = stack.pop()?;
            let neg = b.ins().ineg(a.v);
            let z = b.ins().iconst(I32, 0);
            let isneg = b.ins().icmp(IntCC::SignedLessThan, a.v, z);
            let r = b.ins().select(isneg, neg, a.v);
            push(stack, r, JT::Int);
        }
        ("abs", "(J)J") => {
            let a = stack.pop()?;
            let neg = b.ins().ineg(a.v);
            let z = b.ins().iconst(I64, 0);
            let isneg = b.ins().icmp(IntCC::SignedLessThan, a.v, z);
            let r = b.ins().select(isneg, neg, a.v);
            push(stack, r, JT::Long);
        }
        // max/min — the top refused-direct-call population in the Climate/RTree
        // ring-scan loop (Math.max(JJ)J on node distances). smax/smin are exact;
        // fmax/fmin follow IEEE754-2019 maximum/minimum = Java's NaN-propagating,
        // signed-zero-aware semantics.
        ("max", "(II)I") | ("max", "(JJ)J") => {
            let (y, x) = (stack.pop()?, stack.pop()?);
            let r = b.ins().smax(x.v, y.v);
            push(stack, r, x.t);
        }
        ("min", "(II)I") | ("min", "(JJ)J") => {
            let (y, x) = (stack.pop()?, stack.pop()?);
            let r = b.ins().smin(x.v, y.v);
            push(stack, r, x.t);
        }
        ("max", "(DD)D") | ("max", "(FF)F") => {
            let (y, x) = (stack.pop()?, stack.pop()?);
            let r = b.ins().fmax(x.v, y.v);
            push(stack, r, x.t);
        }
        ("min", "(DD)D") | ("min", "(FF)F") => {
            let (y, x) = (stack.pop()?, stack.pop()?);
            let r = b.ins().fmin(x.v, y.v);
            push(stack, r, x.t);
        }
        // Java round = HALF-UP: floor(x) + (frac >= 0.5), saturating cast for the
        // JDK's NaN→0 / ±inf clamp. The +1 arm only fires on a non-integral input
        // (< 2^53), so the increment can't overflow the saturated extremes.
        ("round", "(D)J") | ("round", "(F)I") => {
            let a = stack.pop()?;
            let (it, jt) = if desc == "(D)J" { (I64, JT::Long) } else { (I32, JT::Int) };
            let f = b.ins().floor(a.v);
            let frac = b.ins().fsub(a.v, f);
            let half = if desc == "(D)J" { b.ins().f64const(0.5) } else { b.ins().f32const(0.5) };
            let up = b.ins().fcmp(FloatCC::GreaterThanOrEqual, frac, half);
            let base = b.ins().fcvt_to_sint_sat(it, f);
            let inc = b.ins().uextend(it, up);
            let r = b.ins().iadd(base, inc);
            push(stack, r, jt);
        }
        _ => return None,
    }
    Some(())
}

pub(crate) fn lerp_value(b: &mut FunctionBuilder, delta: Value, start: Value, end: Value) -> Value {
    let span = b.ins().fsub(end, start);
    let scaled = b.ins().fmul(delta, span);
    b.ins().fadd(start, scaled)
}

pub(crate) fn emit_static_intrinsic(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SV>,
    intrinsic: StaticIntrinsic,
) -> Option<()> {
    match intrinsic {
        StaticIntrinsic::FloorDToI => {
            let x = expect_pop(stack, JT::Double)?;
            let cast = b.ins().fcvt_to_sint_sat(I32, x);
            let cast_d = b.ins().fcvt_from_sint(F64, cast);
            let less = b.ins().fcmp(FloatCC::LessThan, x, cast_d);
            let one = b.ins().iconst(I32, 1);
            let dec = b.ins().isub(cast, one);
            let out = b.ins().select(less, dec, cast);
            push(stack, out, JT::Int);
        }
        StaticIntrinsic::LFloorDToJ => {
            let x = expect_pop(stack, JT::Double)?;
            let cast = b.ins().fcvt_to_sint_sat(I64, x);
            let cast_d = b.ins().fcvt_from_sint(F64, cast);
            let less = b.ins().fcmp(FloatCC::LessThan, x, cast_d);
            let one = b.ins().iconst(I64, 1);
            let dec = b.ins().isub(cast, one);
            let out = b.ins().select(less, dec, cast);
            push(stack, out, JT::Long);
        }
        StaticIntrinsic::SmoothstepD => {
            let x = expect_pop(stack, JT::Double)?;
            let x2 = b.ins().fmul(x, x);
            let x3 = b.ins().fmul(x2, x);
            let six = b.ins().f64const(6.0);
            let fifteen = b.ins().f64const(15.0);
            let ten = b.ins().f64const(10.0);
            let inner = b.ins().fmul(x, six);
            let inner = b.ins().fsub(inner, fifteen);
            let inner = b.ins().fmul(x, inner);
            let inner = b.ins().fadd(inner, ten);
            let out = b.ins().fmul(x3, inner);
            push(stack, out, JT::Double);
        }
        StaticIntrinsic::SmoothstepDerivativeD => {
            let x = expect_pop(stack, JT::Double)?;
            let thirty = b.ins().f64const(30.0);
            let one = b.ins().f64const(1.0);
            let xm1 = b.ins().fsub(x, one);
            let out = b.ins().fmul(thirty, x);
            let out = b.ins().fmul(out, x);
            let out = b.ins().fmul(out, xm1);
            let out = b.ins().fmul(out, xm1);
            push(stack, out, JT::Double);
        }
        StaticIntrinsic::LerpD => {
            let end = expect_pop(stack, JT::Double)?;
            let start = expect_pop(stack, JT::Double)?;
            let delta = expect_pop(stack, JT::Double)?;
            let out = lerp_value(b, delta, start, end);
            push(stack, out, JT::Double);
        }
        StaticIntrinsic::Lerp2D => {
            let x11 = expect_pop(stack, JT::Double)?;
            let x01 = expect_pop(stack, JT::Double)?;
            let x10 = expect_pop(stack, JT::Double)?;
            let x00 = expect_pop(stack, JT::Double)?;
            let dy = expect_pop(stack, JT::Double)?;
            let dx = expect_pop(stack, JT::Double)?;
            let lo = lerp_value(b, dx, x00, x10);
            let hi = lerp_value(b, dx, x01, x11);
            let out = lerp_value(b, dy, lo, hi);
            push(stack, out, JT::Double);
        }
        StaticIntrinsic::Lerp3D => {
            let x111 = expect_pop(stack, JT::Double)?;
            let x011 = expect_pop(stack, JT::Double)?;
            let x101 = expect_pop(stack, JT::Double)?;
            let x001 = expect_pop(stack, JT::Double)?;
            let x110 = expect_pop(stack, JT::Double)?;
            let x010 = expect_pop(stack, JT::Double)?;
            let x100 = expect_pop(stack, JT::Double)?;
            let x000 = expect_pop(stack, JT::Double)?;
            let dz = expect_pop(stack, JT::Double)?;
            let dy = expect_pop(stack, JT::Double)?;
            let dx = expect_pop(stack, JT::Double)?;
            let z0y0 = lerp_value(b, dx, x000, x100);
            let z0y1 = lerp_value(b, dx, x010, x110);
            let z0 = lerp_value(b, dy, z0y0, z0y1);
            let z1y0 = lerp_value(b, dx, x001, x101);
            let z1y1 = lerp_value(b, dx, x011, x111);
            let z1 = lerp_value(b, dy, z1y0, z1y1);
            let out = lerp_value(b, dz, z0, z1);
            push(stack, out, JT::Double);
        }
    }
    Some(())
}
