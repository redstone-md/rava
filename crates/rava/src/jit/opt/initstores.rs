//! The fields of a new object its own block stores right after allocating it, before anything
//! that could see the object's memory first: a call, a safepoint, another allocation (a
//! collection), a check that may deoptimize (the interpreter would see the fields before the
//! constructor's stores). Allocation leaves those bytes uncleared (C2's initialization
//! capturing): a vector's `new V(x + a, y + b, z + c)` wrote each field twice.

use super::ir::*;
use std::collections::HashMap;

/// The bytes (a bit each, from the object's start) stored before they can be read, by
/// allocation; objects up to 128 bytes.
pub fn covered(g: &Graph) -> HashMap<V, u128> {
    let mut out = HashMap::new();
    for blk in &g.blocks {
        for (i, &v) in blk.insts.iter().enumerate() {
            if g.resolve(v) != v || !matches!(g.inst(v), Inst::New(..)) {
                continue;
            }
            let mut mask = 0u128;
            for &w in &blk.insts[i + 1..] {
                if g.resolve(w) != w {
                    continue;
                }
                match *g.inst(w) {
                    Inst::Const(_) | Inst::Copy(_) | Inst::Bin(BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::And | BinOp::Or | BinOp::Xor | BinOp::Shl | BinOp::Shr | BinOp::Ushr, ..) | Inst::Neg(_) | Inst::Compare(..) | Inst::Select(..) => {}
                    Inst::Bin(BinOp::Div | BinOp::Rem, ..) if matches!(g.ty(w), Ty::F | Ty::D) => {}
                    Inst::Convert(op, _) if !matches!(op, 0x8b..=0x90) => {}
                    // A load of an object known non-null (a possible null has its own check,
                    // which ends the run) neither faults nor allocates.
                    // (Not of the new object itself: an unstored field reads as zero.)
                    Inst::LoadField(o, ..) if !g.ordered.contains(&w) && g.resolve(o) != v => {}
                    Inst::StoreField(o, off, bt, _) if !g.ordered.contains(&w) => {
                        if g.resolve(o) == v && off as u32 + bt.size() <= 128 {
                            mask |= ((1u128 << bt.size()) - 1) << off;
                        }
                    }
                    _ => break,
                }
            }
            if mask != 0 {
                out.insert(v, mask);
            }
        }
    }
    out
}
