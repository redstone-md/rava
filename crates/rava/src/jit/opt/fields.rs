//! Final fields of objects allocated in the compilation read back what their constructor stored
//! (C2 gets this from escape analysis and its memory graph): a load of a final field of a `New`
//! whose only store to that field dominates the load becomes that stored value, and a load of
//! such an object's class word becomes the class. Inlined lambdas read their captured values
//! this way instead of reloading them from the lambda object on every call or loop iteration.

use super::dom::Dom;
use super::ir::*;
use crate::klass::Klass;
use crate::types::BasicType;
use std::collections::HashMap;

pub fn forward(g: &mut Graph) {
    let news: HashMap<V, &'static Klass> =
        g.values.iter().enumerate().filter_map(|(v, x)| if let Inst::New(k, _) = x.inst { Some((v as V, k)) } else { None }).collect();
    if news.is_empty() {
        return;
    }
    let dom = Dom::new(g);
    let index = dom.index(g.blocks.len());
    // Where each instruction is: (rpo position of its block, position in the block).
    let mut at = vec![(usize::MAX, 0usize); g.values.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        for (i, &v) in blk.insts.iter().enumerate() {
            at[v as usize] = (index[b], i);
        }
    }
    let reachable = |v: V| at[v as usize].0 != usize::MAX;
    // The stores to each (object, offset) of the allocations; several make the field unknown.
    let mut stores: HashMap<(V, u32), Option<(V, V)>> = HashMap::new();
    for v in 0..g.values.len() as V {
        if let Inst::StoreField(o, off, bt, x) = g.values[v as usize].inst {
            let o = g.resolve(o);
            if news.contains_key(&o) && reachable(v) {
                let e = stores.entry((o, off)).or_insert(Some((v, x)));
                if e.is_some_and(|(s, _)| s != v) {
                    *e = None;
                }
                if !forwardable(bt) {
                    *e = None;
                }
            }
        }
    }
    let dominates = |a: V, b: V| {
        let ((ba, ia), (bb, ib)) = (at[a as usize], at[b as usize]);
        if ba == bb { ia < ib } else { dom.dominates(ba, bb) }
    };
    let mut forwarded: Vec<(V, V)> = Vec::new();
    let mut klass_loads: Vec<(V, &'static Klass)> = Vec::new();
    for v in 0..g.values.len() as V {
        let Inst::LoadField(o, off, bt) = g.values[v as usize].inst else { continue };
        let o = g.resolve(o);
        let Some(&k) = news.get(&o) else { continue };
        if !reachable(v) || g.resolve(v) != v {
            continue;
        }
        if off == 0 && bt == BasicType::Long {
            klass_loads.push((v, k));
            continue;
        }
        if !forwardable(bt) || !is_final(k, off) {
            continue;
        }
        if let Some(Some((s, x))) = stores.get(&(o, off)) {
            if dominates(*s, v) {
                forwarded.push((v, g.resolve(*x)));
            }
        }
    }
    if forwarded.is_empty() && klass_loads.is_empty() {
        return;
    }
    if g.alias.len() < g.values.len() {
        let n = g.alias.len();
        g.alias.extend(n as V..g.values.len() as V);
    }
    let mut gone = vec![false; g.values.len()];
    for (v, x) in forwarded {
        g.alias[v as usize] = x;
        gone[v as usize] = true;
    }
    for (v, k) in klass_loads {
        let c = g.add(0, Inst::Const(k as *const Klass as i64), Ty::J);
        g.alias[v as usize] = c;
        gone[v as usize] = true;
    }
    // The loads are now their values: nothing left to emit.
    for b in &mut g.blocks {
        b.insts.retain(|&v| !gone.get(v as usize).copied().unwrap_or(false));
    }
}

/// Types whose stored value reads back unchanged (no narrowing on load).
fn forwardable(bt: BasicType) -> bool {
    matches!(bt, BasicType::Int | BasicType::Long | BasicType::Float | BasicType::Double | BasicType::Reference)
}

/// Whether the instance field at `off` of `k` (or a superclass) is final.
fn is_final(k: &'static Klass, off: u32) -> bool {
    std::iter::successors(Some(k), |c| c.super_klass)
        .flat_map(|c| c.fields.iter())
        .find(|f| !f.is_static() && f.offset == off)
        .is_some_and(|f| f.access & crate::klass::ACC_FINAL != 0)
}
