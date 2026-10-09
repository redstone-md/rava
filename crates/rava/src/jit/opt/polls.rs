//! Where optimized code can do without safepoint polls. A body without loops or Java calls (a
//! frameless one among them) needs none at all, not even on entry. Short counted loops need none on their back edges (C2 drops polls from
//! counted loops below its strip-mining length): an int or long induction variable stepping by
//! a constant from a constant start, tested against a constant bound in the loop header, with
//! at most `MAX_TRIPS` iterations. Time to a safepoint stays bounded by the polls around the loop
//! (method entries, enclosing loops) and in any call its body makes.

use super::dom::{Dom, Loop};
use super::ir::*;

const MAX_TRIPS: i64 = 4096;

/// For each block: whether it heads a short counted loop.
pub fn short_loops(g: &Graph) -> Vec<bool> {
    let mut short = vec![false; g.blocks.len()];
    for l in Dom::new(g).loops(g) {
        if trips(g, &l).is_some_and(|n| n <= MAX_TRIPS) {
            short[l.header as usize] = true;
        }
    }
    short
}

/// An upper bound on the iterations of `l` when it has the counted shape.
fn trips(g: &Graph, l: &Loop) -> Option<i64> {
    let h = &g.blocks[l.header as usize];
    let Term::If { cmp, ty: ty @ (Ty::I | Ty::J), a, b, then, other, .. } = h.term else { return None };
    // One entry and one back edge, the back edge's increment of an induction phi.
    let [p0, p1] = h.preds[..] else { return None };
    let entry = match (l.body[p0 as usize], l.body[p1 as usize]) {
        (false, true) => 0,
        (true, false) => 1,
        _ => return None,
    };
    let (a, b) = (g.resolve(a), g.resolve(b));
    // The bound is a constant; the other side the phi or its next value.
    let (var, bound, cmp) = match (g.constant(a), g.constant(b)) {
        (None, Some(n)) => (a, n, cmp),
        (Some(n), None) => (b, n, swap(cmp)),
        _ => return None,
    };
    let (step, init, tests_next) = induction(g, l.header, entry, var)?;
    // The condition under which the loop goes on.
    let stay = if l.body[then as usize] && !l.body[other as usize] {
        cmp
    } else if l.body[other as usize] && !l.body[then as usize] {
        negate(cmp)
    } else {
        return None;
    };
    let (lo, hi) = if ty == Ty::I { (i32::MIN as i64, i32::MAX as i64) } else { (i64::MIN, i64::MAX) };
    // The tested value runs from `first`; the bound must stop it before it could wrap.
    let first = if tests_next { init.checked_add(step)? } else { init };
    let span = match stay {
        Cmp::Lt if step > 0 && bound.checked_add(step)? <= hi => bound - first,
        Cmp::Le if step > 0 && bound.checked_add(step)? <= hi => bound - first + 1,
        Cmp::Gt if step < 0 && bound.checked_add(step)? >= lo => first - bound,
        Cmp::Ge if step < 0 && bound.checked_add(step)? >= lo => first - bound + 1,
        _ => return None,
    };
    Some(if span <= 0 { 0 } else { (span + step.abs() - 1) / step.abs() + 1 })
}

/// `var` as the header phi `i = phi(init, i + step)` (or as `i + step` itself): the step, the
/// constant start, and whether `var` is the next value.
fn induction(g: &Graph, header: B, entry: usize, var: V) -> Option<(i64, i64, bool)> {
    let phi_of = |v: V| -> Option<(V, i64)> {
        let next = |p: V| match g.inst(p) {
            Inst::Phi(inputs) if inputs.len() == 2 => Some(g.resolve(inputs[1 - entry])),
            _ => None,
        };
        match g.inst(v) {
            Inst::Phi(_) if g.blocks[header as usize].insts.contains(&v) => {
                let n = next(v)?;
                Some((v, step_of(g, n, v)?))
            }
            Inst::Bin(BinOp::Add | BinOp::Sub, x, _) => {
                let p = g.resolve(*x);
                let s = step_of(g, v, p)?;
                (g.blocks[header as usize].insts.contains(&p) && next(p) == Some(v)).then_some((p, s))
            }
            _ => None,
        }
    };
    let (phi, step) = phi_of(var)?;
    let Inst::Phi(inputs) = g.inst(phi) else { return None };
    let init = g.constant(g.resolve(inputs[entry]))?;
    Some((step, init, var != phi))
}

/// The constant `c` when `v` is `p + c` or `p - c`.
fn step_of(g: &Graph, v: V, p: V) -> Option<i64> {
    match *g.inst(v) {
        Inst::Bin(BinOp::Add, x, c) if g.resolve(x) == p => g.constant(g.resolve(c)).filter(|&c| c != 0),
        Inst::Bin(BinOp::Sub, x, c) if g.resolve(x) == p => g.constant(g.resolve(c)).filter(|&c| c != 0 && c != i64::MIN).map(|c| -c),
        _ => None,
    }
}

fn swap(c: Cmp) -> Cmp {
    match c {
        Cmp::Lt => Cmp::Gt,
        Cmp::Gt => Cmp::Lt,
        Cmp::Le => Cmp::Ge,
        Cmp::Ge => Cmp::Le,
        c => c,
    }
}

fn negate(c: Cmp) -> Cmp {
    match c {
        Cmp::Eq => Cmp::Ne,
        Cmp::Ne => Cmp::Eq,
        Cmp::Lt => Cmp::Ge,
        Cmp::Ge => Cmp::Lt,
        Cmp::Gt => Cmp::Le,
        Cmp::Le => Cmp::Gt,
    }
}

/// Whether `g` qualifies for a frameless body: straight-line or branching code (no loops,
/// hence no polls) without calls, allocation, checks that throw, monitors or deoptimization.
pub fn frameless(g: &Graph) -> bool {
    g.blocks.iter().filter(|b| !matches!(b.term, Term::None)).all(|b| {
        !b.loop_header
            && matches!(b.term, Term::Goto(_) | Term::If { .. } | Term::Switch { .. } | Term::Return(_))
            && b.insts.iter().all(|&v| match &g.values[v as usize].inst {
                Inst::Param(_) | Inst::Const(_) | Inst::Copy(_) | Inst::Phi(_) | Inst::Neg(_) | Inst::Bin(..) | Inst::Convert(..) | Inst::Compare(..) | Inst::Select(..) => true,
                Inst::LoadField(..) | Inst::StoreField(..) | Inst::LoadStatic(..) | Inst::StoreStatic(..) | Inst::ArrayLength(_) => true,
                Inst::LoadIndexed(..) | Inst::InstanceOf(..) => true,
                Inst::Intrinsic(k, _, s) => s.is_none() && k.is_pure(),
                _ => false,
            })
    })
}

