//! `b & 1` of a value that is always 0 or 1 is that value. Lambda forms normalize every boolean
//! so (a guard's outcome, a merged `equals` result), and the mask hid the test from what reads
//! the branch on it: if-conversion of a merged 0/1 into a branch on its condition, type tests
//! proved by a branch and branches fused with them. The builder folds masks of values it
//! already knows (`build_cache.rs`); merges of 0 and 1 are only complete here.

use super::ir::*;

pub fn fold_masks(g: &mut Graph) {
    let mut gone = Vec::new();
    for v in 0..g.values.len() as V {
        if g.resolve(v) != v {
            continue;
        }
        let Inst::Bin(BinOp::And, a, b) = *g.inst(v) else { continue };
        if g.ty(v) != Ty::I {
            continue;
        }
        let (a, b) = (g.resolve(a), g.resolve(b));
        let keep = match (g.constant(a), g.constant(b)) {
            (_, Some(1)) if is_boolean(g, a, 4) => a,
            (Some(1), _) if is_boolean(g, b, 4) => b,
            _ => continue,
        };
        g.alias[v as usize] = keep;
        gone.push(v);
    }
    if gone.is_empty() {
        return;
    }
    for blk in &mut g.blocks {
        blk.insts.retain(|v| !gone.contains(v));
    }
    // A loop's phi may now merge only itself and one value.
    super::phis::fold_trivial(g);
}

/// Whether int `v` is always 0 or 1.
fn is_boolean(g: &Graph, v: V, depth: u32) -> bool {
    let v = g.resolve(v);
    let bit = |x: V| matches!(g.constant(g.resolve(x)), Some(0 | 1));
    match g.inst(v) {
        Inst::Const(c) => *c == 0 || *c == 1,
        Inst::InstanceOf(..) | Inst::Intrinsic(crate::jit::intrinsics::Intrinsic::IsInstance, ..) => true,
        Inst::Select(.., t, f) => bit(*t) && bit(*f),
        Inst::Phi(ins) if depth > 0 => ins.iter().all(|&x| g.resolve(x) == v || is_boolean(g, x, depth - 1)),
        Inst::Bin(BinOp::And, a, b) if depth > 0 => is_boolean(g, *a, depth - 1) || is_boolean(g, *b, depth - 1),
        Inst::Bin(BinOp::Or | BinOp::Xor, a, b) if depth > 0 => is_boolean(g, *a, depth - 1) && is_boolean(g, *b, depth - 1),
        _ => false,
    }
}
