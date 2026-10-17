//! If-conversion (C2's conditional moves): a branch whose arms only compute a few cheap values
//! for a merge becomes straight-line code with a `Select` per merged value. Data-dependent
//! branches such as `l > 0 ? l : Math.max(m, 0)` (the climate tree's parameter distance)
//! mispredict about half the time; a conditional move does not.
//!
//! Shapes: a diamond (`if` to two arms that both go to the merge) or a triangle (one arm, the
//! other edge straight to the merge), where each arm has a single predecessor and at most
//! `MAX_ARM` instructions that cannot trap or touch memory, and every value merged is an int,
//! long or reference (an SSE value has no conditional move here). Converted shapes fold into
//! their branch block and the pass repeats, so nested conditionals (`a > 0 ? a * a : b > 0 ?
//! b * b : 0`) convert from the inside out.
//!
//! The branch may compare floats or doubles by order (`<`, `<=`, `>`, `>=`): such a `Select`
//! takes its false value for NaN, so a branch sending NaN to its true side selects on the
//! negated condition with the values swapped. Minecraft's `Mth.floor`, `int i = (int) v; return
//! v < i ? i - 1 : i`, mispredicted on coordinates of either sign: 5.3 ns against C2's 0.8.

use super::ir::*;

const MAX_ARM: usize = 3;

pub fn convert(g: &mut Graph) {
    loop {
        let mut changed = false;
        for b in 0..g.blocks.len() as B {
            if let Some(shape) = shape(g, b) {
                rewrite(g, b, shape);
                changed = true;
            }
        }
        if !changed {
            break;
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
    let Term::If { cmp, ty, a, b: y, then, other, unordered } = g.blocks[b as usize].term else { return None };
    let float = match (ty, cmp) {
        (Ty::I | Ty::J | Ty::A, _) => false,
        (Ty::F | Ty::D, Cmp::Lt | Cmp::Le | Cmp::Gt | Cmp::Ge) => true,
        _ => return None,
    };
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
    let (Some(it), Some(if_)) = (mb.preds.iter().position(|&p| p == on_true), mb.preds.iter().position(|&p| p == on_false)) else { return None };
    if mb.loop_header || mb.preds.iter().filter(|&&p| p == on_true || p == on_false).count() != 2 {
        return None;
    }
    // Every value selected must fit a general register.
    let differs = |p: V| matches!(g.inst(p), Inst::Phi(i) if g.resolve(i[it]) != g.resolve(i[if_]));
    if super::regalloc::phis(g, merge).any(|p| differs(p) && !matches!(g.ty(p), Ty::I | Ty::J | Ty::A)) {
        return None;
    }
    // A float select reads NaN as false: NaN sent to the true side is the negated order's false.
    let (cmp, on_true, on_false) = if float && unordered { (super::rce::negate(cmp), on_false, on_true) } else { (cmp, on_true, on_false) };
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
            Inst::Select(..) => matches!(g.ty(v), Ty::I | Ty::J | Ty::A),
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
    let mut preds = g.blocks[s.merge as usize].preds.clone();
    let (it, if_) = (preds.iter().position(|&p| p == s.on_true).unwrap(), preds.iter().position(|&p| p == s.on_false).unwrap());
    let phis: Vec<V> = super::regalloc::phis(g, s.merge).collect();
    if preds.len() > 2 {
        // The merge has other entries: the two paths become one edge from `b`, carrying a
        // select per value that differs between them (every phi loses an input, aliased ones
        // included).
        let all: Vec<V> = g.blocks[s.merge as usize].insts.iter().copied().filter(|&v| matches!(g.values[v as usize].inst, Inst::Phi(_))).collect();
        for p in all {
            let Inst::Phi(mut inputs) = g.values[p as usize].inst.clone() else { unreachable!() };
            let (t, f) = (g.resolve(inputs[it]), g.resolve(inputs[if_]));
            let live = phis.contains(&p);
            inputs[it] = if t == f || !live { t } else { g.add(b, Inst::Select(s.cmp, s.ty, s.a, s.b, t, f), g.ty(p)) };
            inputs.remove(if_);
            g.values[p as usize].inst = Inst::Phi(inputs);
        }
        preds[it] = b;
        preds.remove(if_);
        g.blocks[s.merge as usize].preds = preds;
        g.blocks[b as usize].term = Term::Goto(s.merge);
        return;
    }
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
    absorb(g, b, s.merge);
}

/// Folds the merge `m`, now entered only from `b`, into `b`: an enclosing branch then sees one
/// straight-line arm.
fn absorb(g: &mut Graph, b: B, m: B) {
    let mb = &g.blocks[m as usize];
    if m == b || mb.loop_header || mb.preds[..] != [b] || matches!(mb.term, Term::None) {
        return;
    }
    let insts = std::mem::take(&mut g.blocks[m as usize].insts);
    let term = std::mem::replace(&mut g.blocks[m as usize].term, Term::None);
    g.blocks[m as usize].preds.clear();
    for s in term.successors() {
        for p in &mut g.blocks[s as usize].preds {
            if *p == m {
                *p = b;
            }
        }
    }
    g.blocks[b as usize].insts.extend(insts);
    g.blocks[b as usize].term = term;
}
