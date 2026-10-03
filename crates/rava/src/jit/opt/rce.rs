//! Range check elimination for counted loops (C2's range check elimination and loop
//! predication): in a loop `for (i = init; i < limit; i++)` whose header tests the condition,
//! the bounds checks at index `i` of an array defined outside the loop leave the body.
//! - When `limit` is that array's length and `init` a constant `>= 0`, the loop's own test
//!   already keeps `i` in bounds.
//! - When `init` and `limit` are defined outside the loop, one predicate in the preheader
//!   (`Inst::RangeGuard`) covers every iteration: if the loop runs, the array is non-null,
//!   `init >= 0` and `limit <= length`, else it deoptimizes to the state on loop entry. The
//!   interpreter then runs the loop with its checks; that counts as a trap at the header, so
//!   the recompilation keeps the checks in the loop.
//!
//! The climate tree's distance loop checked two arrays at `i < 7` every iteration.

use super::dom::Dom;
use super::ir::*;
use std::collections::HashSet;
use std::rc::Rc;

pub fn eliminate(g: &mut Graph) {
    let dom = Dom::new(g);
    let mut block_of = vec![usize::MAX; g.values.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        for &v in &blk.insts {
            block_of[v as usize] = b;
        }
    }
    for l in dom.loops(g) {
        let h = l.header as usize;
        let Some(entry_state) = g.loop_entry.get(&l.header).cloned() else { continue };
        let Term::If { cmp, ty: Ty::I, a, b, then, other, .. } = g.blocks[h].term else { continue };
        let [p0, p1] = g.blocks[h].preds[..] else { continue };
        let entry = match (l.body[p0 as usize], l.body[p1 as usize]) {
            (false, true) => 0,
            (true, false) => 1,
            _ => continue,
        };
        let pre = g.blocks[h].preds[entry];
        if !matches!(g.blocks[pre as usize].term, Term::Goto(t) if t as usize == h) {
            continue;
        }
        let stay = match (l.body[then as usize], l.body[other as usize]) {
            (true, false) => cmp,
            (false, true) => negate(cmp),
            _ => continue,
        };
        let (a, b) = (g.resolve(a), g.resolve(b));
        let (i, limit) = match stay {
            Cmp::Lt => (a, b),
            Cmp::Gt => (b, a),
            _ => continue,
        };
        let header_phi = |v: V| g.blocks[h].insts.contains(&v) && matches!(g.inst(v), Inst::Phi(_));
        if !header_phi(i) {
            continue;
        }
        let Inst::Phi(inputs) = g.inst(i) else { continue };
        let (init, next) = (g.resolve(inputs[entry]), g.resolve(inputs[1 - entry]));
        let increments = matches!(*g.inst(next), Inst::Bin(BinOp::Add, x, c) if g.resolve(x) == i && g.constant(g.resolve(c)) == Some(1));
        let outside = |v: V| g.constant(v).is_some() || block_of.get(v as usize).map_or(true, |&b| b == usize::MAX || !l.body[b]);
        if !increments {
            continue;
        }
        // `i < a.length` from a constant `init >= 0`: the loop's own test is the bounds check.
        let counts_length = |arr: V| matches!(g.inst(limit), Inst::ArrayLength(y) if g.resolve(*y) == arr) && g.constant(init).is_some_and(|c| c >= 0);
        let guarded = outside(init) && outside(limit);
        // The checks to move: in the body past the header's test, at index `i`.
        let mut arrays: Vec<V> = Vec::new();
        for (bi, blk) in g.blocks.iter().enumerate() {
            if !l.body[bi] || bi == h {
                continue;
            }
            for &v in &blk.insts {
                if let Inst::BoundsCheck(arr, idx, _) = g.inst(v) {
                    let arr = g.resolve(*arr);
                    if g.resolve(*idx) == i && outside(arr) && (guarded || counts_length(arr)) && !arrays.contains(&arr) {
                        arrays.push(arr);
                    }
                }
            }
        }
        let (free, predicated): (Vec<V>, Vec<V>) = arrays.into_iter().partition(|&arr| counts_length(arr));
        let ps = if predicated.is_empty() { None } else { entry_state_at(g, &entry_state, h, entry) };
        let predicated = if ps.is_some() { predicated } else { Vec::new() };
        for &arr in &predicated {
            let v = g.add(pre, Inst::RangeGuard(arr, init, limit, ps.clone().unwrap()), Ty::I);
            g.alias.push(v);
        }
        // The predicates cover every iteration: the body's null and bounds checks of these
        // arrays go (not the header's, which also runs when the loop does not).
        let checked: HashSet<V> = free.iter().chain(predicated.iter()).copied().collect();
        let moved: HashSet<V> = predicated.iter().copied().collect();
        if checked.is_empty() {
            continue;
        }
        for (bi, blk) in g.blocks.iter_mut().enumerate() {
            if !l.body[bi] || bi == h {
                continue;
            }
            let values = &g.values;
            let alias = &g.alias;
            let resolve = |mut v: V| {
                while let Some(&a) = alias.get(v as usize).filter(|&&a| a != v) {
                    v = a;
                }
                v
            };
            blk.insts.retain(|&v| match &values[v as usize].inst {
                Inst::NullCheck(x, _) => !moved.contains(&resolve(*x)),
                Inst::BoundsCheck(arr, idx, _) => !(checked.contains(&resolve(*arr)) && resolve(*idx) == i),
                _ => true,
            });
        }
    }
}

/// The loop-entry state with the header's phis replaced by their values on entry: what the
/// interpreter needs to run the loop from its header. None when it involves an allocation
/// removed by escape analysis (whose fields could be the header's phis).
fn entry_state_at(g: &Graph, s: &Rc<State>, h: usize, entry: usize) -> Option<Rc<State>> {
    let map = |x: &Option<V>| -> Option<Option<V>> {
        let Some(x) = *x else { return Some(None) };
        let x = g.resolve(x);
        if g.virtuals.contains_key(&x) {
            return None;
        }
        match g.inst(x) {
            Inst::Phi(inputs) if g.blocks[h].insts.contains(&x) => Some(Some(g.resolve(inputs[entry]))),
            _ => Some(Some(x)),
        }
    };
    let mut caller = s.caller.clone();
    while let Some(c) = caller {
        if c.locals.iter().chain(c.stack.iter()).flatten().any(|&x| g.virtuals.contains_key(&g.resolve(x))) {
            return None;
        }
        caller = c.caller.clone();
    }
    let locals = s.locals.iter().map(map).collect::<Option<Vec<_>>>()?;
    let stack = s.stack.iter().map(map).collect::<Option<Vec<_>>>()?;
    Some(Rc::new(State { method: s.method, bci: s.bci, locals, stack, caller: s.caller.clone(), frame: s.frame }))
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
