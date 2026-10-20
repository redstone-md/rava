//! Casts proved by the dispatch after them (C2 folds a checkcast into the profiled type test
//! that follows): `((Shape) it.next()).area()` casts the element, then the inlined call tests
//! its class word against each profiled receiver class. When every class the chain of tests
//! admits is a subtype of the cast's, the cast can only fail where the chain falls through, so
//! it moves to the start of that block (before its fallback call or deoptimization) and the
//! object stands for the cast everywhere else. Between the cast and the first test the block
//! holds only the cast value's null check and class word load (safe on any object) and
//! constants.

use super::ir::*;

pub fn sink(g: &mut Graph) {
    if g.klass_consts.is_empty() {
        return;
    }
    if g.alias.len() < g.values.len() {
        let n = g.alias.len();
        g.alias.extend(n as V..g.values.len() as V);
    }
    for b in 0..g.blocks.len() {
        if let Some((cast, x, fall)) = candidate(g, b as B) {
            let Inst::CheckCast(_, k, s) = g.inst(cast).clone() else { unreachable!("a candidate is a cast") };
            g.blocks[b].insts.retain(|&v| v != cast);
            g.alias[cast as usize] = x;
            g.values.push(Value { inst: Inst::CheckCast(x, k, s), ty: Ty::A });
            let moved = g.values.len() as V - 1;
            g.alias.push(moved);
            g.blocks[fall as usize].insts.insert(0, moved);
        }
    }
}

/// `(cast, object, fallthrough block)` of a cast in `b` that the class tests ending `b` prove.
fn candidate(g: &Graph, b: B) -> Option<(V, V, B)> {
    let blk = &g.blocks[b as usize];
    let live: Vec<V> = blk.insts.iter().copied().filter(|&v| g.resolve(v) == v).collect();
    let at = live.iter().rposition(|&v| matches!(g.inst(v), Inst::CheckCast(..)))?;
    let cast = live[at];
    let Inst::CheckCast(x, k, _) = *g.inst(cast) else { return None };
    let x = g.resolve(x);
    // After the cast: its null check, then its class word, and constants.
    let (mut checked, mut word) = (false, None);
    for &v in &live[at + 1..] {
        match *g.inst(v) {
            Inst::Const(_) => {}
            Inst::NullCheck(y, _) if g.resolve(y) == cast && word.is_none() => checked = true,
            Inst::LoadField(y, 0, _) if g.resolve(y) == cast && checked && word.is_none() => word = Some(v),
            _ => return None,
        }
    }
    // The class word, and its reloads along the chain.
    let mut words = vec![word?];
    // The chain of tests of that word, each admitting a subtype of the cast's class.
    let mut cur = b;
    loop {
        let Term::If { cmp, ty: Ty::J, a, b: y, then, other, .. } = g.blocks[cur as usize].term else { break };
        let (a, y) = (g.resolve(a), g.resolve(y));
        let c = if words.contains(&a) { y } else if words.contains(&y) { a } else { break };
        let tested = g.constant(c).and_then(|c| g.klass_consts.get(&c)).copied();
        let (hit, miss) = match cmp {
            Cmp::Eq => (then, other),
            Cmp::Ne => (other, then),
            _ => break,
        };
        if hit == miss || !tested.is_some_and(|t| t.is_subtype_of(k)) {
            break;
        }
        let mb = &g.blocks[miss as usize];
        if mb.preds[..] != [cur] {
            return None;
        }
        cur = miss;
        // A further test only when the block holds nothing else but constants and the word
        // reloaded.
        let mut reloads = Vec::new();
        for &v in mb.insts.iter().filter(|&&v| g.resolve(v) == v) {
            match *g.inst(v) {
                Inst::Const(_) => {}
                Inst::LoadField(o, 0, _) if g.resolve(o) == cast => reloads.push(v),
                _ => return (cur != b).then_some((cast, x, cur)),
            }
        }
        words.extend(reloads);
    }
    (cur != b).then_some((cast, x, cur))
}
