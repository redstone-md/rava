//! Which instructions code generation may write rcx or rdx in. Both are scratch for division,
//! variable shifts, `lcmp` and float compares, calls (and every runtime call), type checks,
//! allocation, monitors, intrinsics and static stores; everything else uses rax and r11 only.
//! The register allocator hands rcx and rdx to values whose whole interval avoids every
//! instruction that may write them (`regalloc.rs`).

use super::ir::*;
use crate::types::BasicType;

/// Whether the code for instruction `v` may write (rcx, rdx).
pub fn clobbers(g: &Graph, v: V) -> (bool, bool) {
    const NONE: (bool, bool) = (false, false);
    const BOTH: (bool, bool) = (true, true);
    match g.inst(v) {
        Inst::Param(_) | Inst::Const(_) | Inst::Copy(_) | Inst::Fence | Inst::Phi(_) => NONE,
        Inst::Neg(_) | Inst::Convert(..) | Inst::Select(..) => NONE,
        Inst::Bin(op, _, y) => match (g.ty(v), op) {
            (Ty::F | Ty::D, BinOp::Rem) => BOTH,
            (Ty::F | Ty::D, _) => NONE,
            (_, BinOp::Div | BinOp::Rem) => BOTH,
            (_, BinOp::Shl | BinOp::Shr | BinOp::Ushr) if g.constant(*y).is_none() => (true, false),
            _ => NONE,
        },
        // lcmp and float compares set rdx; lcmp also reads its first operand through rcx.
        Inst::Compare(..) => BOTH,
        Inst::NullCheck(..) | Inst::ZeroCheck(..) | Inst::FlagCheck(..) | Inst::BoundsCheck(..) | Inst::RangeGuard(..) | Inst::Guard(..) => NONE,
        Inst::LoadField(..) | Inst::StoreField(..) | Inst::LoadStatic(..) | Inst::ArrayLength(_) | Inst::LoadIndexed(..) => NONE,
        // A byte or non-float element store puts the value through rdx.
        Inst::StoreIndexed(_, _, BasicType::Float | BasicType::Double, _) => NONE,
        Inst::StoreIndexed(..) => (false, true),
        Inst::Intrinsic(crate::jit::intrinsics::Intrinsic::BitCount(..), ..) => NONE,
        // A primitive Unsafe access addresses through rax and r11; a store's value goes
        // through rdx.
        Inst::Intrinsic(crate::jit::intrinsics::Intrinsic::UnsafeGet(t, _), ..) if *t != BasicType::Reference => NONE,
        Inst::Intrinsic(crate::jit::intrinsics::Intrinsic::UnsafePut(t, _), ..) if *t != BasicType::Reference => (false, true),
        _ => BOTH,
    }
}
