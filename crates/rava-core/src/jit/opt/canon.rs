//! Canonicalization (C1's `Canonicalizer`): arithmetic, conversions, comparisons and branches on
//! constants are evaluated at compile time with Java semantics. Values are in slot encoding.

use super::ir::{BinOp, Cmp, Ty};

/// `x op y` on constant ints (`I`, `J`); `None` for division by zero and floating point.
pub fn bin(op: BinOp, ty: Ty, x: i64, y: i64) -> Option<i64> {
    match ty {
        Ty::I => {
            let (a, b) = (x as i32, y as i32);
            Some(match op {
                BinOp::Add => a.wrapping_add(b),
                BinOp::Sub => a.wrapping_sub(b),
                BinOp::Mul => a.wrapping_mul(b),
                BinOp::Div if b != 0 => a.wrapping_div(b),
                BinOp::Rem if b != 0 => a.wrapping_rem(b),
                BinOp::And => a & b,
                BinOp::Or => a | b,
                BinOp::Xor => a ^ b,
                BinOp::Shl => a.wrapping_shl(b as u32 & 31),
                BinOp::Shr => a.wrapping_shr(b as u32 & 31),
                BinOp::Ushr => ((a as u32) >> (b as u32 & 31)) as i32,
                _ => return None,
            } as i64)
        }
        Ty::J => Some(match op {
            BinOp::Add => x.wrapping_add(y),
            BinOp::Sub => x.wrapping_sub(y),
            BinOp::Mul => x.wrapping_mul(y),
            BinOp::Div if y != 0 => x.wrapping_div(y),
            BinOp::Rem if y != 0 => x.wrapping_rem(y),
            BinOp::And => x & y,
            BinOp::Or => x | y,
            BinOp::Xor => x ^ y,
            BinOp::Shl => x.wrapping_shl(y as u32 & 63),
            BinOp::Shr => x.wrapping_shr(y as u32 & 63),
            BinOp::Ushr => ((x as u64) >> (y as u32 & 63)) as i64,
            _ => return None,
        }),
        _ => None,
    }
}

/// An integer conversion bytecode (i2l, l2i, i2b, i2c, i2s) of a constant.
pub fn convert(op: u8, x: i64) -> Option<i64> {
    Some(match op {
        0x85 => x,
        0x88 => x as i32 as i64,
        0x91 => x as i8 as i64,
        0x92 => x as u16 as i64,
        0x93 => x as i16 as i64,
        _ => return None,
    })
}

/// `lcmp` of constants.
pub fn compare(op: u8, x: i64, y: i64) -> Option<i64> {
    (op == 0x94).then(|| x.cmp(&y) as i64)
}

/// Whether the branch `x cmp y` on values of type `ty` (I, J or A) is taken.
pub fn branch(cmp: Cmp, ty: Ty, x: i64, y: i64) -> bool {
    let (x, y) = if ty == Ty::I { (x as i32 as i64, y as i32 as i64) } else { (x, y) };
    match cmp {
        Cmp::Eq => x == y,
        Cmp::Ne => x != y,
        Cmp::Lt => x < y,
        Cmp::Ge => x >= y,
        Cmp::Gt => x > y,
        Cmp::Le => x <= y,
    }
}
