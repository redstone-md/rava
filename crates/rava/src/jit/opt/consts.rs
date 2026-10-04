//! Wide constants enter loops in registers: a constant that does not fit an instruction's
//! 32-bit immediate (an object, such as a lookup table's array, or a 64-bit multiplier) would be
//! rematerialized with a 10-byte `movabs` at each use; inside an innermost loop, its uses read a
//! copy made once in the preheader instead, which the allocator keeps in a register (as C2 keeps
//! a hoisted constant). The biome lookup's hashing loop loaded its two multipliers six times an
//! iteration. Copies are per constant value: inlining makes a `Const` per use (eight of one
//! gradient table in a noise loop), and one copy each would take eight registers and spill.

use super::dom::Dom;
use super::ir::*;
use std::collections::HashMap;

pub fn hoist(g: &mut Graph) {
    let loops = Dom::new(g).loops(g);
    for (li, l) in loops.iter().enumerate() {
        // Innermost: no other loop's header inside.
        if loops.iter().enumerate().any(|(j, o)| j != li && o.header != l.header && l.body[o.header as usize]) {
            continue;
        }
        let h = l.header as usize;
        let outside: Vec<B> = g.blocks[h].preds.iter().copied().filter(|&p| !l.body[p as usize]).collect();
        let [pre] = outside[..] else { continue };
        if !matches!(g.blocks[pre as usize].term, Term::Goto(t) if t as usize == h) {
            continue;
        }
        let wide = |g: &Graph, v: V| matches!(g.ty(v), Ty::I | Ty::J | Ty::A) && g.constant(v).is_some_and(|c| i32::try_from(c).is_err());
        let mut copies: HashMap<V, V> = HashMap::new();
        let mut by_value: HashMap<(Ty, i64), V> = HashMap::new();
        let body: Vec<usize> = (0..g.blocks.len()).filter(|&b| l.body[b]).collect();
        for &b in &body {
            for i in 0..g.blocks[b].insts.len() {
                let v = g.blocks[b].insts[i];
                if matches!(g.inst(v), Inst::Phi(_)) {
                    continue;
                }
                for x in g.inst(v).inputs() {
                    let x = g.resolve(x);
                    if wide(g, x) && !copies.contains_key(&x) {
                        let c = copy_of(g, pre, x, &mut by_value);
                        copies.insert(x, c);
                    }
                }
            }
            for x in g.blocks[b].term.inputs() {
                let x = g.resolve(x);
                if wide(g, x) && !copies.contains_key(&x) {
                    let c = copy_of(g, pre, x, &mut by_value);
                    copies.insert(x, c);
                }
            }
        }
        if copies.is_empty() {
            continue;
        }
        let alias = g.alias.clone();
        let resolve = |mut x: V| {
            while let Some(&a) = alias.get(x as usize).filter(|&&a| a != x) {
                x = a;
            }
            x
        };
        let swap = |x: V| copies.get(&resolve(x)).copied().unwrap_or(x);
        for &b in &body {
            for i in 0..g.blocks[b].insts.len() {
                let v = g.blocks[b].insts[i] as usize;
                if !matches!(g.values[v].inst, Inst::Phi(_)) {
                    g.values[v].inst.map_inputs(swap);
                }
            }
            g.blocks[b].term.map_inputs(swap);
        }
    }
}

/// The preheader's copy of constant `x`, one per (type, value).
fn copy_of(g: &mut Graph, pre: B, x: V, by_value: &mut HashMap<(Ty, i64), V>) -> V {
    let key = (g.ty(x), g.constant(x).expect("a constant"));
    if let Some(&c) = by_value.get(&key) {
        return c;
    }
    let c = g.add(pre, Inst::Copy(x), g.ty(x));
    by_value.insert(key, c);
    c
}
