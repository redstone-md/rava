//! If-conversion (C2's conditional moves): a branch whose arms only compute a few cheap values
//! for a merge becomes straight-line code with a `Select` per merged value. Data-dependent
//! branches such as `l > 0 ? l : Math.max(m, 0)` (the climate tree's parameter distance)
//! mispredict about half the time; a conditional move does not.
//!
//! Shapes: a diamond (`if` to two arms that both go to the merge) or a triangle (one arm, the
//! other edge straight to the merge), where each arm has a single predecessor and at most
//! `MAX_ARM` instructions that cannot trap or touch memory, and every value merged is an int,
//! long or reference (an SSE value has no conditional move here).

use super::ir::*;

const MAX_ARM: usize = 3;

pub fn convert(g: &mut Graph) {
    for b in 0..g.blocks.len() as B {
        if let Some(shape) = shape(g, b) {
            rewrite(g, b, shape);
        }
    }
    fold_branches(g);
}

/// A branch on a selected constant (`if (it.hasNext())` once `hasNext` is inlined: the
/// boolean merged into a `Select` of 1 and 0, then compared with 0) branches on the select's
/// own condition; the select goes if nothing else reads it.
fn fold_branches(g: &mut Graph) {
    let mut reads = vec![0u32; g.values.len()];
    let count = |x: V, reads: &mut Vec<u32>| reads[g.resolve(x) as usize] += 1;
    let mut states = Vec::new();
    for blk in g.blocks.iter().filter(|b| !matches!(b.term, Term::None)) {
        for &v in &blk.insts {
            if g.resolve(v) == v {
                g.inst(v).inputs().into_iter().for_each(|x| count(x, &mut reads));
                states.extend(g.inst(v).state().cloned());
            }
        }
        blk.term.inputs().into_iter().for_each(|x| count(x, &mut reads));
        if let Term::Deopt(s) = &blk.term {
            states.push(s.clone());
        }
    }
    for s in &states {
        let mut xs = Vec::new();
        super::liveness::state_values(g, s, &mut xs);
        xs.into_iter().for_each(|x| count(x, &mut reads));
    }
    let mut gone = Vec::new();
    for b in 0..g.blocks.len() {
        let Term::If { cmp: cmp @ (Cmp::Eq | Cmp::Ne), ty: Ty::I, a, b: y, then, other, .. } = g.blocks[b].term else { continue };
        let (sel, k) = match (g.constant(g.resolve(a)), g.constant(g.resolve(y))) {
            (None, Some(k)) => (g.resolve(a), k),
            (Some(k), None) => (g.resolve(y), k),
            _ => continue,
        };
        let Inst::Select(c, sty, x, z, t, f) = *g.inst(sel) else { continue };
        let (Some(tv), Some(fv)) = (g.constant(g.resolve(t)), g.constant(g.resolve(f))) else { continue };
        let (on_t, on_f) = (super::canon::branch(cmp, Ty::I, tv, k), super::canon::branch(cmp, Ty::I, fv, k));
        if on_t == on_f {
            continue;
        }
        let (then, other) = if on_t { (then, other) } else { (other, then) };
        g.blocks[b].term = Term::If { cmp: c, ty: sty, a: x, b: z, then, other, unordered: false };
        if reads[sel as usize] == 1 {
            gone.push(sel);
        }
    }
    if !gone.is_empty() {
        for blk in &mut g.blocks {
            blk.insts.retain(|v| !gone.contains(v));
        }
    }
}

struct Shape {
    cmp: Cmp,
    ty: Ty,
    a: V,
    b: V,
    /// The arms (blocks moved into the branch block), and the merge.
    arms: Vec<B>,
    merge: B,
    /// Predecessor of the merge on the true path and on the false path.
    on_true: B,
    on_false: B,
}

fn shape(g: &Graph, b: B) -> Option<Shape> {
    let Term::If { cmp, ty: ty @ (Ty::I | Ty::J | Ty::A), a, b: y, then, other, .. } = g.blocks[b as usize].term else { return None };
    if then == other {
        return None;
    }
    let arm = |x: B| -> Option<B> {
        let blk = &g.blocks[x as usize];
        match blk.term {
            Term::Goto(m) if blk.preds[..] == [b] && !blk.loop_header && cheap(g, x) => Some(m),
            _ => None,
        }
    };
    let (arms, merge, on_true, on_false) = match (arm(then), arm(other)) {
        (Some(m1), Some(m2)) if m1 == m2 => (vec![then, other], m1, then, other),
        (Some(m), _) if m == other => (vec![then], m, then, b),
        (_, Some(m)) if m == then => (vec![other], m, b, other),
        _ => return None,
    };
    let mb = &g.blocks[merge as usize];
    let mut preds = mb.preds.clone();
    preds.sort_unstable();
    let mut want = vec![on_true, on_false];
    want.sort_unstable();
    if preds != want || mb.loop_header {
        return None;
    }
    // Every merged value must fit a general register.
    if super::regalloc::phis(g, merge).any(|p| !matches!(g.ty(p), Ty::I | Ty::J | Ty::A)) {
        return None;
    }
    Some(Shape { cmp, ty, a: g.resolve(a), b: g.resolve(y), arms, merge, on_true, on_false })
}

/// Whether block `x` only computes a few values without trapping or touching memory.
fn cheap(g: &Graph, x: B) -> bool {
    let insts = &g.blocks[x as usize].insts;
    let mut work = 0;
    insts.iter().all(|&v| {
        let ok = match g.inst(v) {
            Inst::Const(_) | Inst::Copy(_) => true,
            Inst::Bin(op, ..) => matches!(g.ty(v), Ty::I | Ty::J) && !matches!(op, BinOp::Div | BinOp::Rem),
            Inst::Neg(_) => matches!(g.ty(v), Ty::I | Ty::J),
            Inst::Convert(op, _) => matches!(op, 0x85 | 0x88 | 0x91 | 0x92 | 0x93),
            Inst::Intrinsic(k, _, None) => k.is_pure() && matches!(g.ty(v), Ty::I | Ty::J),
            _ => false,
        };
        if !matches!(g.inst(v), Inst::Const(_)) {
            work += 1;
        }
        ok
    }) && work <= MAX_ARM
}

fn rewrite(g: &mut Graph, b: B, s: Shape) {
    // The arms' instructions run unconditionally in the branch block.
    for &x in &s.arms {
        let insts = std::mem::take(&mut g.blocks[x as usize].insts);
        g.blocks[b as usize].insts.extend(insts);
        g.blocks[x as usize].term = Term::None;
        g.blocks[x as usize].preds.clear();
    }
    let preds = g.blocks[s.merge as usize].preds.clone();
    let (it, if_) = (preds.iter().position(|&p| p == s.on_true).unwrap(), preds.iter().position(|&p| p == s.on_false).unwrap());
    let phis: Vec<V> = super::regalloc::phis(g, s.merge).collect();
    for p in phis {
        let Inst::Phi(inputs) = g.inst(p).clone() else { unreachable!() };
        let (t, f) = (g.resolve(inputs[it]), g.resolve(inputs[if_]));
        g.values[p as usize].inst = if t == f { Inst::Copy(t) } else { Inst::Select(s.cmp, s.ty, s.a, s.b, t, f) };
    }
    // Phis that only alias another value are read through it: they go with the merge.
    let aliased: Vec<V> = g.blocks[s.merge as usize].insts.iter().copied().filter(|&v| matches!(g.inst(v), Inst::Phi(_)) && g.resolve(v) != v).collect();
    g.blocks[s.merge as usize].insts.retain(|v| !aliased.contains(v));
    g.blocks[s.merge as usize].preds = vec![b];
    g.blocks[b as usize].term = Term::Goto(s.merge);
}
