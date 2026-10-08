//! Conditions proved by dominating tests (C2's `IfNode::dominated_by` and range check smearing):
//! a walk of the dominator tree collects what each branch edge, null check and bounds check
//! established, and drops what follows from it: a null check of a value already known non-null
//! (a cast of one too), a bounds check whose index a test kept within the array
//! (`if (i < 0 || i >= a.length) ...` before `a[i]`), and a branch whose outcome is known (the
//! same class test of the same object again, a range test implied by an earlier one).
//!
//! Operands are compared by what they read: a value, an object's class word (immutable: two
//! loads of it are one), an array's length, or a constant.

use super::ir::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Key {
    Val(V),
    Class(V),
    Len(V),
    Const(i64),
}

/// `a cmp b` on values of type `ty`, a constant (if any) on the right.
#[derive(Clone, Copy, Debug)]
struct Fact {
    cmp: Cmp,
    ty: Ty,
    a: Key,
    b: Key,
}

fn key(g: &Graph, v: V) -> Key {
    let v = g.resolve(v);
    match *g.inst(v) {
        Inst::Const(c) => Key::Const(c),
        Inst::LoadField(o, 0, crate::types::BasicType::Int) => Key::Class(g.resolve(o)),
        Inst::ArrayLength(a) => Key::Len(g.resolve(a)),
        _ => Key::Val(v),
    }
}

/// `b cmp a` as `a cmp' b`.
fn mirror(c: Cmp) -> Cmp {
    match c {
        Cmp::Lt => Cmp::Gt,
        Cmp::Gt => Cmp::Lt,
        Cmp::Le => Cmp::Ge,
        Cmp::Ge => Cmp::Le,
        c => c,
    }
}

fn fact(cmp: Cmp, ty: Ty, a: Key, b: Key) -> Fact {
    match a {
        Key::Const(_) => Fact { cmp: mirror(cmp), ty, a: b, b: a },
        _ => Fact { cmp, ty, a, b },
    }
}

/// Whether `f` (on the same operands) decides `q`.
fn decides(f: Cmp, q: Cmp) -> Option<bool> {
    use Cmp::*;
    let holds = |s: &[Cmp]| s.contains(&q);
    match f {
        _ if f == q => Some(true),
        Eq if holds(&[Le, Ge]) => Some(true),
        Eq if holds(&[Ne, Lt, Gt]) => Some(false),
        Lt if holds(&[Le, Ne]) => Some(true),
        Lt if holds(&[Ge, Gt, Eq]) => Some(false),
        Gt if holds(&[Ge, Ne]) => Some(true),
        Gt if holds(&[Lt, Le, Eq]) => Some(false),
        Le if q == Gt => Some(false),
        Ge if q == Lt => Some(false),
        Ne if q == Eq => Some(false),
        _ => None,
    }
}

struct Facts(Vec<Fact>);

impl Facts {
    /// Whether the facts decide `a cmp b`.
    fn decide(&self, cmp: Cmp, ty: Ty, a: Key, b: Key) -> Option<bool> {
        let q = fact(cmp, ty, a, b);
        if let (Key::Const(x), Key::Const(y)) = (q.a, q.b) {
            return Some(super::canon::branch(q.cmp, ty, x, y));
        }
        for f in self.0.iter().rev().filter(|f| f.ty == ty) {
            if f.a == q.a && f.b == q.b {
                if let Some(r) = decides(f.cmp, q.cmp) {
                    return Some(r);
                }
            }
        }
        // Integer ranges against constants.
        let Key::Const(c) = q.b else { return None };
        if !matches!(ty, Ty::I | Ty::J) {
            return None;
        }
        let (mut lo, mut hi) = (i64::MIN, i64::MAX);
        for f in self.0.iter().filter(|f| f.ty == ty && f.a == q.a) {
            let Key::Const(k) = f.b else { continue };
            match f.cmp {
                Cmp::Eq => (lo, hi) = (lo.max(k), hi.min(k)),
                Cmp::Lt => hi = hi.min(k.saturating_sub(1)),
                Cmp::Le => hi = hi.min(k),
                Cmp::Gt => lo = lo.max(k.saturating_add(1)),
                Cmp::Ge => lo = lo.max(k),
                Cmp::Ne => {}
            }
        }
        if lo > hi {
            return None;
        }
        // An ordered test is monotone over the range: its ends decide it.
        let check = |p: &dyn Fn(i64) -> bool| match (p(lo), p(hi)) {
            (true, true) => Some(true),
            (false, false) => Some(false),
            _ => None,
        };
        match q.cmp {
            Cmp::Lt => check(&|x| x < c),
            Cmp::Le => check(&|x| x <= c),
            Cmp::Gt => check(&|x| x > c),
            Cmp::Ge => check(&|x| x >= c),
            Cmp::Eq if lo == hi => Some(lo == c),
            Cmp::Eq | Cmp::Ne if c < lo || c > hi => Some(q.cmp == Cmp::Ne),
            Cmp::Ne if lo == hi => Some(lo != c),
            _ => None,
        }
    }

    fn nonnull(&self, g: &Graph, v: V) -> bool {
        let mut v = g.resolve(v);
        loop {
            if matches!(g.inst(v), Inst::New(..) | Inst::NewArray(..) | Inst::NewMulti(..)) || g.receiver == Some(v) {
                return true;
            }
            if self.decide(Cmp::Ne, Ty::A, Key::Val(v), Key::Const(0)) == Some(true) {
                return true;
            }
            // A cast passes its object through: non-null exactly when that is.
            match *g.inst(v) {
                Inst::CheckCast(x, ..) => v = g.resolve(x),
                _ => return false,
            }
        }
    }
}

pub fn eliminate(g: &mut Graph) {
    let super::dom::Dom { rpo, idom, .. } = super::dom::Dom::new(g);
    let mut children = vec![Vec::new(); rpo.len()];
    for i in 1..rpo.len() {
        if idom[i] != usize::MAX {
            children[idom[i]].push(i);
        }
    }
    let mut facts = Facts(Vec::new());
    let (mut drop, mut folds) = (Vec::new(), Vec::new());
    let mut walk = vec![(0usize, false)];
    let mut marks = Vec::new();
    while let Some((i, done)) = walk.pop() {
        if done {
            facts.0.truncate(marks.pop().unwrap());
            continue;
        }
        marks.push(facts.0.len());
        walk.push((i, true));
        let b = rpo[i];
        if let Some(f) = on_entry(g, b) {
            facts.0.push(f);
        }
        for &v in &g.blocks[b as usize].insts {
            if g.resolve(v) != v {
                continue;
            }
            match *g.inst(v) {
                Inst::NullCheck(x, _) => {
                    if facts.nonnull(g, x) {
                        drop.push(v);
                    } else {
                        // The checked value and what it casts are non-null from here.
                        let mut x = g.resolve(x);
                        loop {
                            facts.0.push(fact(Cmp::Ne, Ty::A, Key::Val(x), Key::Const(0)));
                            match *g.inst(x) {
                                Inst::CheckCast(y, ..) => x = g.resolve(y),
                                _ => break,
                            }
                        }
                    }
                }
                Inst::BoundsCheck(arr, idx, _) => {
                    let (k, len) = (key(g, idx), Key::Len(g.resolve(arr)));
                    if facts.decide(Cmp::Ge, Ty::I, k, Key::Const(0)) == Some(true) && facts.decide(Cmp::Lt, Ty::I, k, len) == Some(true) {
                        drop.push(v);
                    } else {
                        facts.0.push(fact(Cmp::Ge, Ty::I, k, Key::Const(0)));
                        facts.0.push(fact(Cmp::Lt, Ty::I, k, len));
                    }
                }
                _ => {}
            }
        }
        if let Term::If { cmp, ty: ty @ (Ty::I | Ty::J | Ty::A), a, b: y, then, other, .. } = g.blocks[b as usize].term {
            if then != other {
                if let Some(r) = facts.decide(cmp, ty, key(g, a), key(g, y)) {
                    folds.push((b, if r { then } else { other }, if r { other } else { then }));
                }
            }
        }
        walk.extend(children[i].iter().map(|&c| (c, false)));
    }
    if drop.is_empty() && folds.is_empty() {
        return;
    }
    for blk in &mut g.blocks {
        blk.insts.retain(|v| !drop.contains(v));
    }
    for (b, keep, gone) in folds {
        drop_pred(g, gone, b);
        // A block that only deoptimizes is not laid out (branches go to its stub): take its exit.
        let kb = &g.blocks[keep as usize];
        if let (true, Term::Deopt(s)) = (kb.insts.is_empty(), &kb.term) {
            g.blocks[b as usize].term = Term::Deopt(s.clone());
            drop_pred(g, keep, b);
        } else {
            g.blocks[b as usize].term = Term::Goto(keep);
        }
    }
    remove_unreachable(g);
}

/// Empties the blocks no path from the entry reaches any more (a folded branch's other side),
/// taking them out of their successors' predecessors, so none is laid out jumping to a block
/// that is not.
fn remove_unreachable(g: &mut Graph) {
    let mut seen = vec![false; g.blocks.len()];
    let mut stack = vec![0 as B];
    while let Some(b) = stack.pop() {
        if !std::mem::replace(&mut seen[b as usize], true) {
            stack.extend(g.blocks[b as usize].term.successors());
        }
    }
    for b in 0..g.blocks.len() {
        if seen[b] || matches!(g.blocks[b].term, Term::None) {
            continue;
        }
        let term = std::mem::replace(&mut g.blocks[b].term, Term::None);
        g.blocks[b].insts.clear();
        for s in term.successors() {
            while g.blocks[s as usize].preds.contains(&(b as B)) {
                drop_pred(g, s, b as B);
            }
        }
    }
}

/// What the branch into `b` established: entered only from one side of a test.
fn on_entry(g: &Graph, b: B) -> Option<Fact> {
    let [p] = g.blocks[b as usize].preds[..] else { return None };
    let Term::If { cmp, ty: ty @ (Ty::I | Ty::J | Ty::A), a, b: y, then, other, .. } = g.blocks[p as usize].term else { return None };
    if then == other {
        return None;
    }
    let c = if then == b { cmp } else { super::rce::negate(cmp) };
    Some(fact(c, ty, key(g, a), key(g, y)))
}

/// Removes `m` from the predecessors of `s` (and the matching phi inputs).
fn drop_pred(g: &mut Graph, s: B, m: B) {
    if let Some(j) = g.blocks[s as usize].preds.iter().position(|&x| x == m) {
        g.blocks[s as usize].preds.remove(j);
        for v in g.blocks[s as usize].insts.clone() {
            if let Inst::Phi(ins) = &mut g.values[v as usize].inst {
                ins.remove(j);
            }
        }
    }
}
