//! Redundant type-check elimination over the dominator tree (C2's type sharpening after a
//! test): once a branch on `x instanceof K` or a `checkcast` to `K` has proved `x` an instance
//! of `K`, a dominated `checkcast` of `x` to `K` or a supertype is `x` itself, and a dominated
//! `instanceof` of `x` to such a class is 1 where the test proved `x` non-null too (a
//! `checkcast` lets null through). `if (o instanceof T) { T t = (T) o; ... }`, the shape of
//! every `equals` and of pattern matching, then tests the class once. A null check of a value
//! such a test proved non-null goes as well.

use super::ir::*;
use crate::klass::Klass;

pub fn eliminate(g: &mut Graph) {
    let super::dom::Dom { rpo, idom, .. } = super::dom::Dom::new(g);
    let mut children = vec![Vec::new(); rpo.len()];
    for i in 1..rpo.len() {
        if idom[i] != usize::MAX {
            children[idom[i]].push(i);
        }
    }
    // Walk the dominator tree with the (value, class, non-null) facts known on entry to each
    // block.
    let mut known: Vec<(V, &'static Klass, bool)> = Vec::new();
    let mut casts: Vec<(V, V)> = Vec::new();
    let mut ones: Vec<V> = Vec::new();
    let mut checks: Vec<V> = Vec::new();
    let mut walk = vec![(0usize, false)];
    let mut marks = Vec::new();
    while let Some((i, done)) = walk.pop() {
        if done {
            known.truncate(marks.pop().unwrap());
            continue;
        }
        marks.push(known.len());
        walk.push((i, true));
        let b = rpo[i];
        known.extend(proved_on_entry(g, b).map(|(x, k)| (x, k, true)));
        known.extend(nonnull_on_entry(g, b).and_then(|x| object_klass().map(|o| (x, o, true))));
        let proves = |known: &[(V, &'static Klass, bool)], x: V, k: &'static Klass, nonnull: bool| {
            let is_nonnull = || known.iter().any(|&(y, _, nn)| y == x && nn);
            known.iter().any(|&(y, kk, nn)| y == x && (nn || !nonnull) && kk.is_subtype_of(k))
                // Every reference is an Object; a field's value an instance of its declared type.
                || (is_object(k) || g.declared_types.get(&x).is_some_and(|d| d.is_subtype_of(k))) && (!nonnull || is_nonnull())
        };
        for &v in &g.blocks[b as usize].insts {
            if g.resolve(v) != v {
                continue;
            }
            // Casts already found redundant stand for what they cast.
            let res = |x: V| {
                let x = g.resolve(x);
                casts.iter().find(|c| c.0 == x).map_or(x, |c| c.1)
            };
            match g.values[v as usize].inst {
                Inst::CheckCast(x, k, _) => {
                    let x = res(x);
                    if proves(&known, x, k, false) {
                        casts.push((v, x));
                    } else {
                        known.push((x, k, false));
                    }
                }
                Inst::InstanceOf(x, k) if proves(&known, res(x), k, true) => ones.push(v),
                Inst::NullCheck(x, _) if known.iter().any(|&(y, _, nn)| nn && y == res(x)) => checks.push(v),
                _ => {}
            }
        }
        walk.extend(children[i].iter().map(|&c| (c, false)));
    }
    if casts.is_empty() && ones.is_empty() && checks.is_empty() {
        return;
    }
    for &(v, x) in &casts {
        g.alias[v as usize] = x;
    }
    for &v in &ones {
        g.values[v as usize].inst = Inst::Const(1);
    }
    for blk in &mut g.blocks {
        blk.insts.retain(|v| !casts.iter().any(|c| c.0 == *v) && !checks.contains(v));
    }
}

fn is_object(k: &'static Klass) -> bool {
    k.super_klass.is_none() && matches!(k.kind, crate::klass::KlassKind::Instance) && !k.is_interface()
}

/// java.lang.Object, once loaded.
fn object_klass() -> Option<&'static Klass> {
    static O: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let a = *O.get_or_init(|| crate::vm::vm().find_loaded(std::ptr::null_mut(), crate::symbol::Symbol::intern("java/lang/Object")).map_or(0, |k| k as *const Klass as usize));
    // SAFETY: classes are immortal.
    (a != 0).then(|| unsafe { &*(a as *const Klass) })
}

/// The value the branch into `b` proved non-null: entered only from a test of it against null
/// on the side where it is not.
fn nonnull_on_entry(g: &Graph, b: B) -> Option<V> {
    let [p] = g.blocks[b as usize].preds[..] else { return None };
    let Term::If { cmp, ty: Ty::A, a, b: y, then, other, .. } = g.blocks[p as usize].term else { return None };
    if then == other {
        return None;
    }
    let (a, y) = (g.resolve(a), g.resolve(y));
    let x = match (g.constant(a), g.constant(y)) {
        (None, Some(0)) => a,
        (Some(0), None) => y,
        _ => return None,
    };
    let holds = match cmp {
        Cmp::Ne => then == b,
        Cmp::Eq => other == b,
        _ => return None,
    };
    holds.then_some(x)
}

/// What the branch into `b` proved: entered only from a test of `x instanceof K` on the side
/// where it holds.
fn proved_on_entry(g: &Graph, b: B) -> Option<(V, &'static Klass)> {
    let [p] = g.blocks[b as usize].preds[..] else { return None };
    let Term::If { cmp, ty: Ty::I, a, b: y, then, other, .. } = g.blocks[p as usize].term else { return None };
    if then == other {
        return None;
    }
    let (a, y) = (g.resolve(a), g.resolve(y));
    let (test, zero) = match (g.inst(a), g.inst(y)) {
        (Inst::InstanceOf(..), _) => (a, y),
        (_, Inst::InstanceOf(..)) => (y, a),
        _ => return None,
    };
    let Inst::InstanceOf(x, k) = *g.inst(test) else { return None };
    if g.constant(zero) != Some(0) {
        return None;
    }
    let holds = match cmp {
        Cmp::Ne => then == b,
        Cmp::Eq => other == b,
        _ => return None,
    };
    holds.then(|| (g.resolve(x), k))
}
