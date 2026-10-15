//! Which instructions code generation may write rcx or rdx in. Both are scratch for division,
//! variable shifts, calls (and every runtime call), type checks, large allocations, monitors
//! and intrinsics; everything else uses rax and r11 only.
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
        // lcmp and float compares work through rax and r11, as do static stores.
        Inst::Compare(..) | Inst::StoreStatic(..) => NONE,
        Inst::NullCheck(..) | Inst::ZeroCheck(..) | Inst::FlagCheck(..) | Inst::BoundsCheck(..) | Inst::RangeGuard(..) | Inst::Guard(..) => NONE,
        Inst::LoadField(..) | Inst::StoreField(..) | Inst::LoadStatic(..) | Inst::ArrayLength(_) | Inst::LoadIndexed(..) => NONE,
        // A byte or non-float element store puts the value through rdx.
        Inst::StoreIndexed(_, _, BasicType::Float | BasicType::Double, _) => NONE,
        Inst::StoreIndexed(..) => (false, true),
        // Through rax, r11 and the SSE registers only.
        Inst::Intrinsic(
            crate::jit::intrinsics::Intrinsic::BitCount(..)
            | crate::jit::intrinsics::Intrinsic::Bits(_)
            | crate::jit::intrinsics::Intrinsic::Round(_)
            | crate::jit::intrinsics::Intrinsic::Sqrt
            | crate::jit::intrinsics::Intrinsic::Fma(_)
            | crate::jit::intrinsics::Intrinsic::MinMax(..)
            | crate::jit::intrinsics::Intrinsic::Abs(_)
            | crate::jit::intrinsics::Intrinsic::CurrentThread
            | crate::jit::intrinsics::Intrinsic::GetClass
            | crate::jit::intrinsics::Intrinsic::Leaf(crate::jit::leaf::Leaf::IsPrimitive | crate::jit::leaf::Leaf::IsInterface),
            ..
        ) => NONE,
        // A cast to a final class or one at a primary depth tests through r11 and rax.
        Inst::CheckCast(_, k, _) if crate::jit::emit_obj::primary_depth(k).is_some() || matches!(k.kind, crate::klass::KlassKind::TypeArray { .. }) || matches!(k.kind, crate::klass::KlassKind::Instance) && k.access & crate::klass::ACC_FINAL != 0 => NONE,
        // An object of up to 128 bytes past its header is cleared without a loop counter; the
        // runtime call of the slow path saves rcx and rdx.
        Inst::New(k, _) if k.instance_size() <= 132 => NONE,
        // A primitive Unsafe access addresses through rax and r11; a store's value goes
        // through rdx.
        Inst::Intrinsic(crate::jit::intrinsics::Intrinsic::UnsafeGet(t, _), ..) if *t != BasicType::Reference => NONE,
        Inst::Intrinsic(crate::jit::intrinsics::Intrinsic::UnsafePut(t, _), ..) if *t != BasicType::Reference => (false, true),
        _ => BOTH,
    }
}
