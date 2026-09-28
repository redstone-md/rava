//! THE ONE IMPLEMENTATION of the pure numeric conversions and float/double arithmetic.
//!
//! ⭐**WHY THIS FILE EXISTS.** `flatbody`'s header refuses conversions and floating-point
//! arithmetic for a stated reason, and it is a good one: *"a second implementation of bytecode
//! semantics is a liability"* — truncation and saturation rules, NaN, the `l`/`g` compare
//! variants. But the same header gives the way out: *"where the machine already exposes an
//! authoritative primitive, THIS FILE CALLS IT rather than restating it."*
//!
//! `RAVA_STRAIGHT_BLOCKERS` (row 402) then priced the refusal. Of 266,277,038 interpreted calls
//! that ChunkFill sends down the FRAMED path — the path measured at 122-138 ns against HotSpot's
//! 7.2 — the first disqualifying opcode is a numeric cast for **19.3%** and a float/double
//! add/sub/mul for **11.9%**. That is the largest addressable block in the table, and refusing it
//! on grounds of "a second implementation" is only correct while a second implementation is what
//! it would take.
//!
//! So there is no second implementation: the hot dispatch loop and the frame-free straight-line
//! executor both call the functions here. `d2i`'s saturation and `dsub`'s NaN behaviour are
//! defined in exactly one place, and a divergence between the two paths is not expressible.
//!
//! ⚠**SATURATION IS RUST'S, AND THAT IS DELIBERATE.** Since Rust 1.45 a float-to-integer `as`
//! cast saturates at the target's bounds and maps NaN to zero — which is precisely what JVMS
//! §6.5 specifies for `d2i`/`d2l`/`f2i`/`f2l`. The expressions below are the ones the dispatch
//! loop has always used; they were moved here, not rewritten.
//!
//! ⚠**PURITY.** Every operation here is total: no allocation, no exception, no `<clinit>`, no
//! heap access. That is what lets the frame-free executor run them with `args` unrooted — see the
//! GC audit in `flatcall`'s header. Adding anything here that can allocate would break that
//! guarantee silently, so: nothing that touches the heap belongs in this file.

use crate::value::Value;

/// Pure unary numeric ops: `fneg`/`dneg` (0x76-0x77) and every conversion (0x85-0x93).
///
/// Returns `None` for an opcode this function does not define, so a caller can never be served a
/// silently-wrong value for something it forgot to exclude.
#[inline(always)]
pub(crate) fn pure_unary(op: u8, v: &Value) -> Option<Value> {
    Some(match op {
        0x76 => Value::Float(-v.as_f32()),         // fneg
        0x77 => Value::Double(-v.as_f64()),        // dneg
        0x85 => Value::Long(v.as_i32() as i64),    // i2l
        0x86 => Value::Float(v.as_i32() as f32),   // i2f
        0x87 => Value::Double(v.as_i32() as f64),  // i2d
        0x88 => Value::Int(v.as_i64() as i32),     // l2i
        0x89 => Value::Float(v.as_i64() as f32),   // l2f
        0x8a => Value::Double(v.as_i64() as f64),  // l2d
        0x8b => Value::Int(v.as_f32() as i32),     // f2i
        0x8c => Value::Long(v.as_f32() as i64),    // f2l
        0x8d => Value::Double(v.as_f32() as f64),  // f2d
        0x8e => Value::Int(v.as_f64() as i32),     // d2i
        0x8f => Value::Long(v.as_f64() as i64),    // d2l
        0x90 => Value::Float(v.as_f64() as f32),   // d2f
        0x91 => Value::Int(v.as_i32() as i8 as i32),  // i2b
        0x92 => Value::Int(v.as_i32() as u16 as i32), // i2c
        0x93 => Value::Int(v.as_i32() as i16 as i32), // i2s
        _ => return None,
    })
}

/// Pure binary float/double arithmetic: `fadd`/`dadd`, `fsub`/`dsub`, `fmul`/`dmul`.
///
/// Division and remainder are NOT here. They cannot throw for floats, so they are safe in
/// principle — they are left out because the census did not ask for them, and this file's value
/// is that everything in it is carrying measured traffic.
///
/// Argument order is `a OP b` with `b` the value popped FIRST, matching the dispatch loop.
#[inline(always)]
pub(crate) fn pure_fp_binary(op: u8, a: &Value, b: &Value) -> Option<Value> {
    Some(match op {
        0x62 => Value::Float(a.as_f32() + b.as_f32()),  // fadd
        0x63 => Value::Double(a.as_f64() + b.as_f64()), // dadd
        0x66 => Value::Float(a.as_f32() - b.as_f32()),  // fsub
        0x67 => Value::Double(a.as_f64() - b.as_f64()), // dsub
        0x6a => Value::Float(a.as_f32() * b.as_f32()),  // fmul
        0x6b => Value::Double(a.as_f64() * b.as_f64()), // dmul
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Value` has no `PartialEq`, so the expectations are destructured. Keeps the assertions
    /// exact rather than comparing debug strings.
    fn int_of(v: Option<Value>) -> i32 {
        match v { Some(Value::Int(i)) => i, other => panic!("expected Int, got {other:?}") }
    }
    fn long_of(v: Option<Value>) -> i64 {
        match v { Some(Value::Long(i)) => i, other => panic!("expected Long, got {other:?}") }
    }
    fn double_of(v: Option<Value>) -> f64 {
        match v { Some(Value::Double(d)) => d, other => panic!("expected Double, got {other:?}") }
    }

    /// The cases `flatbody`'s header named as the reason to refuse conversions. Pinned here so
    /// the now-shared implementation cannot drift from JVMS §6.5.
    #[test]
    fn conversions_saturate_and_truncate_the_way_the_jvm_does() {
        // d2i / d2l saturate at the bounds and send NaN to zero.
        assert_eq!(int_of(pure_unary(0x8e, &Value::Double(f64::NAN))), 0);
        assert_eq!(int_of(pure_unary(0x8e, &Value::Double(1e300))), i32::MAX);
        assert_eq!(int_of(pure_unary(0x8e, &Value::Double(-1e300))), i32::MIN);
        assert_eq!(long_of(pure_unary(0x8f, &Value::Double(f64::NAN))), 0);
        assert_eq!(long_of(pure_unary(0x8f, &Value::Double(1e300))), i64::MAX);
        // Truncation is toward zero, not rounding.
        assert_eq!(int_of(pure_unary(0x8e, &Value::Double(-2.9))), -2);
        assert_eq!(int_of(pure_unary(0x8e, &Value::Double(2.9))), 2);
        // The narrowing int conversions sign- and zero-extend as JVMS specifies.
        assert_eq!(int_of(pure_unary(0x91, &Value::Int(0xFF))), -1);
        assert_eq!(int_of(pure_unary(0x92, &Value::Int(-1))), 0xFFFF);
        assert_eq!(int_of(pure_unary(0x93, &Value::Int(0xFFFF))), -1);
        assert_eq!(int_of(pure_unary(0x88, &Value::Long(0x1_0000_0001))), 1);
        // An opcode this function does not define is refused, never guessed.
        assert!(pure_unary(0x60, &Value::Int(1)).is_none());
    }

    #[test]
    fn fp_arithmetic_propagates_nan_and_refuses_what_it_does_not_define() {
        let nan = Value::Double(f64::NAN);
        assert!(double_of(pure_fp_binary(0x67, &nan, &Value::Double(1.0))).is_nan());
        assert_eq!(double_of(pure_fp_binary(0x63, &Value::Double(1.5), &Value::Double(2.25))), 3.75);
        // Subtraction is `a - b` with `b` the value popped first - the dispatch loop's order.
        assert_eq!(double_of(pure_fp_binary(0x67, &Value::Double(10.0), &Value::Double(4.0))), 6.0);
        // ddiv is deliberately absent - see the note on the function.
        assert!(pure_fp_binary(0x6f, &Value::Double(1.0), &Value::Double(2.0)).is_none());
    }
}
