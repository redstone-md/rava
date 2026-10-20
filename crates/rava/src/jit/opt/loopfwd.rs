//! Loop-carried field values (C2's memory phis): a field of an object defined outside a loop that
//! the loop stores to and reads again (an accumulator kept in a field: a stream's reducing sink,
//! a builder's count, `this.total += x`) is read from the value the previous iteration stored, not
//! from memory. Its stores stay, so memory holds the value at every exit, deoptimization and
//! safepoint; only the loads go, and with them the store-to-load round trip through memory that
//! each iteration waited for.
//!
//! The loop must not call, take a monitor, fence, initialize a class, write memory through an
//! intrinsic or make an ordered access; every other store at the field's offset must be to the
//! same object or to a field no object shares with it. The value at the header is a phi of the
//! field read once in the preheader (the object known non-null there) and of what each back
//! edge carries.

use super::dom::Dom;
use super::ir::*;
use crate::types::BasicType;
use std::collections::{HashMap, HashSet};

pub fn forward(g: &mut Graph) {
    if g.alias.len() < g.values.len() {
        let n = g.alias.len();
        g.alias.extend(n as V..g.values.len() as V);
    }
    let dom = Dom::new(g);
    let index = dom.index(g.blocks.len());
    let nonnull = super::licm::nonnull_by_block(g, &dom);
    let loops = dom.loops(g);
    let mut changed = false;
    // The block of each instruction (what this pass adds is never an object it carries).
    let mut block_of = vec![usize::MAX; g.values.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        blk.insts.iter().for_each(|&v| block_of[v as usize] = b);
    }
    for l in &loops {
        let outside: Vec<B> = g.blocks[l.header as usize].preds.iter().copied().filter(|&p| !l.body[p as usize]).collect();
        let [pre] = outside[..] else { continue };
        if !matches!(g.blocks[pre as usize].term, Term::Goto(t) if t == l.header) || index[pre as usize] == usize::MAX {
            continue;
        }
        let inside = |v: V| block_of.get(g.resolve(v) as usize).is_some_and(|&b| b != usize::MAX && l.body[b]);
        for key in candidates(g, l, &inside, &nonnull[index[pre as usize]]) {
            changed |= carry(g, &dom, l, pre, key);
        }
    }
    if changed {
        super::phis::fold_trivial(g);
    }
}

/// A field: the object, the offset, the type.
type Key = (V, u32, BasicType);

/// The fields loop `l` stores to and may carry, if its memory effects allow any.
fn candidates(g: &Graph, l: &super::dom::Loop, inside: &dyn Fn(V) -> bool, known: &HashSet<V>) -> Vec<Key> {
    let mut stores: Vec<(V, Key)> = Vec::new();
    for b in (0..g.blocks.len()).filter(|&b| l.body[b]) {
        for &v in &g.blocks[b].insts {
            if g.resolve(v) != v {
                continue;
            }
            match *g.inst(v) {
                Inst::Call(..) | Inst::Monitor(..) | Inst::Fence | Inst::InitCheck(..) => return Vec::new(),
                Inst::Intrinsic(..) if g.is_raw_put(v) => {}
                Inst::Intrinsic(k, ..) if !k.is_pure() => return Vec::new(),
                Inst::LoadField(..) | Inst::LoadStatic(..) | Inst::StoreField(..) if g.ordered.contains(&v) => return Vec::new(),
                Inst::StoreField(o, off, bt, _) => stores.push((v, (g.resolve(o), off, bt))),
                _ => {}
            }
        }
    }
    let mut keys: Vec<Key> = stores.iter().map(|&(_, k)| k).collect();
    keys.sort_unstable_by_key(|k| (k.0, k.1));
    keys.dedup();
    keys.retain(|&(o, off, bt)| {
        let holder = stores.iter().find(|(_, k)| *k == (o, off, bt)).and_then(|(s, _)| g.field_holders.get(s).copied());
        matches!(bt, BasicType::Int | BasicType::Long | BasicType::Float | BasicType::Double | BasicType::Reference)
            && !inside(o)
            && nonnull(g, known, o)
            // Every other store at this offset writes another field or this object's.
            && stores.iter().all(|&(s, (o2, off2, bt2))| off2 != off || (o2 == o && bt2 == bt) || !Graph::fields_may_alias(holder, g.field_holders.get(&s).copied()))
    });
    keys
}

fn nonnull(g: &Graph, known: &HashSet<V>, mut x: V) -> bool {
    loop {
        if known.contains(&x) {
            return true;
        }
        match *g.inst(x) {
            Inst::CheckCast(y, ..) => x = g.resolve(y),
            _ => return false,
        }
    }
}

/// Carries field `key` through loop `l`: its loads in the loop become the value last stored or
/// carried in. Returns whether any load went.
fn carry(g: &mut Graph, dom: &Dom, l: &super::dom::Loop, pre: B, key: Key) -> bool {
    let (o, off, bt) = key;
    let ty = Ty::of(bt);
    let order: Vec<B> = dom.rpo.iter().copied().filter(|&b| l.body[b as usize]).collect();
    let initial = g.add(pre, Inst::LoadField(o, off, bt), ty);
    // The value at the end of each block done; phis waiting for inputs from later blocks.
    let mut out: HashMap<B, V> = HashMap::new();
    let mut pending: Vec<(V, B)> = Vec::new();
    let mut gone: Vec<V> = Vec::new();
    let mut phis: Vec<V> = Vec::new();
    for &b in &order {
        let preds = g.blocks[b as usize].preds.clone();
        let ins: Vec<Option<V>> = preds.iter().map(|&p| if p == pre { Some(initial) } else { out.get(&p).copied() }).collect();
        let mut cur = match ins.as_slice() {
            [Some(x), rest @ ..] if rest.iter().all(|y| *y == Some(*x)) => *x,
            _ => {
                let p = g.add(b, Inst::Phi(vec![initial; preds.len()]), ty);
                let blk = &mut g.blocks[b as usize].insts;
                blk.pop();
                blk.insert(0, p);
                pending.push((p, b));
                phis.push(p);
                p
            }
        };
        for v in g.blocks[b as usize].insts.clone() {
            if g.resolve(v) != v {
                continue;
            }
            match *g.inst(v) {
                Inst::LoadField(o2, off2, bt2) if (g.resolve(o2), off2, bt2) == key && g.ty(v) == ty => {
                    g.alias[v as usize] = cur;
                    gone.push(v);
                }
                Inst::StoreField(o2, off2, bt2, x) if (g.resolve(o2), off2, bt2) == key && g.ty(g.resolve(x)) == ty => cur = g.resolve(x),
                _ => {}
            }
        }
        out.insert(b, cur);
    }
    if gone.is_empty() {
        // Nothing to carry: undo the preheader load and the phis.
        g.blocks[pre as usize].insts.retain(|&v| v != initial);
        let dead: HashSet<V> = phis.into_iter().collect();
        g.blocks.iter_mut().for_each(|blk| blk.insts.retain(|v| !dead.contains(v)));
        return false;
    }
    for (p, b) in pending {
        let inputs: Vec<V> = g.blocks[b as usize].preds.iter().map(|&q| if q == pre { initial } else { out.get(&q).copied().unwrap_or(initial) }).collect();
        if let Inst::Phi(ins) = &mut g.values[p as usize].inst {
            *ins = inputs;
        }
    }
    let gone: HashSet<V> = gone.into_iter().collect();
    g.blocks.iter_mut().for_each(|blk| blk.insts.retain(|v| !gone.contains(v)));
    true
}
