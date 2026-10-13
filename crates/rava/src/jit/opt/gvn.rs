//! Global value numbering of pure arithmetic (C2's GVN): an instruction computing what a
//! dominating one already computed (same operation on the same operands, constants compared by
//! value, commutative operands in either order) becomes an alias of it. Building numbers values
//! only within an extended block; inlined bodies, unrolled copies and the paths after a merge
//! repeat `a + 1` or `x * 3 + 1` that an earlier block already holds.
//!
//! Division and remainder stay (their zero checks are separate, but a fault site moved by an
//! alias would be one more thing to prove).

use super::dom::Dom;
use super::ir::*;
use std::collections::HashMap;

/// An operand: a value, or a constant by type and value (inlining makes a `Const` per use).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Opnd {
    Value(V),
    Const(u8, i64),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Neg(Ty, Opnd),
    Bin(u8, Ty, Opnd, Opnd),
    Convert(u8, Opnd),
    Compare(u8, Opnd, Opnd),
}

pub fn number(g: &mut Graph) {
    if g.alias.len() < g.values.len() {
        let n = g.alias.len();
        g.alias.extend(n as V..g.values.len() as V);
    }
    let dom = Dom::new(g);
    let index = dom.index(g.blocks.len());
    // Earlier instructions computing each key, with their blocks' rpo positions.
    let mut seen: HashMap<Key, Vec<(usize, V)>> = HashMap::new();
    let mut gone = Vec::new();
    for &b in &dom.rpo {
        let at = index[b as usize];
        for &v in &g.blocks[b as usize].insts {
            if g.resolve(v) != v {
                continue;
            }
            let Some(key) = key(g, v) else { continue };
            let earlier = seen.entry(key).or_default();
            match earlier.iter().find(|&&(d, _)| dom.dominates(d, at)) {
                Some(&(_, leader)) => {
                    g.alias[v as usize] = leader;
                    gone.push(v);
                }
                None => earlier.push((at, v)),
            }
        }
    }
    if !gone.is_empty() {
        let gone: std::collections::HashSet<V> = gone.into_iter().collect();
        for blk in &mut g.blocks {
            blk.insts.retain(|v| !gone.contains(v));
        }
    }
}

fn opnd(g: &Graph, x: V) -> Opnd {
    let x = g.resolve(x);
    match g.constant(x) {
        Some(k) => Opnd::Const(g.ty(x) as u8, k),
        None => Opnd::Value(x),
    }
}

fn key(g: &Graph, v: V) -> Option<Key> {
    let ty = g.ty(v);
    Some(match *g.inst(v) {
        Inst::Neg(x) => Key::Neg(ty, opnd(g, x)),
        Inst::Bin(op, x, y) if !matches!(op, BinOp::Div | BinOp::Rem) => {
            let (mut x, mut y) = (opnd(g, x), opnd(g, y));
            if matches!(op, BinOp::Add | BinOp::Mul | BinOp::And | BinOp::Or | BinOp::Xor) && y < x {
                std::mem::swap(&mut x, &mut y);
            }
            Key::Bin(op as u8, ty, x, y)
        }
        Inst::Convert(op, x) => Key::Convert(op, opnd(g, x)),
        Inst::Compare(op, x, y) => Key::Compare(op, opnd(g, x), opnd(g, y)),
        _ => return None,
    })
}
