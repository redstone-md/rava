//! Global numbering of field loads (C2's memory-aware GVN): a load of an object's field
//! becomes the value a dominating load or store of that field already has, when no path from
//! there to the load passes anything that may change it. Building reuses loads only within an
//! extended block, so the inlined bodies of small methods on one object (a vertex writer's
//! `vertex().color().light()`, a vector's `rotate().mul().add()`), separated by their branches'
//! merges, each read the same fields again.
//!
//! What may change a field: a call, a monitor, a fence, a class initialization, an intrinsic
//! that writes memory (a plain store to off-heap memory through a null base cannot be any
//! object's field), an acquiring (volatile) load, and a store to a field at the same offset
//! unless both fields' classes are known and unrelated (no object has both).

use super::dom::Dom;
use super::ir::*;
use crate::klass::Klass;
use crate::types::BasicType;
use std::collections::{HashMap, HashSet};

/// A field: the object, the offset, the type read.
type Key = (V, u32, BasicType);

/// Where a field's value is known: the instruction's block and position, and the value.
#[derive(Clone, Copy)]
struct Known {
    block: B,
    at: usize,
    value: V,
    holder: Option<&'static Klass>,
}

/// Paths searched before giving up on a load.
const MAX_REGION: usize = 256;
const MAX_CANDIDATES: usize = 4;

pub fn number(g: &mut Graph) {
    if g.alias.len() < g.values.len() {
        let n = g.alias.len();
        g.alias.extend(n as V..g.values.len() as V);
    }
    let dom = Dom::new(g);
    let index = dom.index(g.blocks.len());
    let mut known: HashMap<Key, Vec<Known>> = HashMap::new();
    let mut gone: HashSet<V> = HashSet::new();
    for &b in &dom.rpo {
        let insts = g.blocks[b as usize].insts.clone();
        for (at, &v) in insts.iter().enumerate() {
            if g.resolve(v) != v {
                continue;
            }
            match *g.inst(v) {
                Inst::LoadField(o, off, bt) if !g.ordered.contains(&v) => {
                    let key = (g.resolve(o), off, bt);
                    let holder = g.field_holders.get(&v).copied();
                    let found = known.get(&key).and_then(|list| {
                        list.iter().rev().take(MAX_CANDIDATES).find(|k| {
                            let dominates = if k.block == b { k.at < at } else { dom.dominates(index[k.block as usize], index[b as usize]) };
                            dominates && clear(g, k.block, k.at, b, at, off, holder.or(k.holder))
                        }).map(|k| k.value)
                    });
                    match found {
                        Some(x) if g.ty(x) == g.ty(v) => {
                            g.alias[v as usize] = g.resolve(x);
                            gone.insert(v);
                        }
                        _ => known.entry(key).or_default().push(Known { block: b, at, value: v, holder }),
                    }
                }
                Inst::StoreField(o, off, bt, x) if forwardable(bt) => {
                    let holder = g.field_holders.get(&v).copied();
                    known.entry((g.resolve(o), off, bt)).or_default().push(Known { block: b, at, value: g.resolve(x), holder });
                }
                _ => {}
            }
        }
    }
    if !gone.is_empty() {
        for blk in &mut g.blocks {
            blk.insts.retain(|v| !gone.contains(v));
        }
    }
}

/// A stored value reads back unchanged unless the field narrows it.
fn forwardable(bt: BasicType) -> bool {
    matches!(bt, BasicType::Int | BasicType::Long | BasicType::Float | BasicType::Double | BasicType::Reference)
}

/// Whether instruction `w` may change a field at `off` declared by `holder` (if known).
fn kills(g: &Graph, w: V, off: u32, holder: Option<&'static Klass>) -> bool {
    use crate::jit::intrinsics::Intrinsic;
    match *g.inst(w) {
        Inst::Call(..) | Inst::Monitor(..) | Inst::Fence | Inst::InitCheck(..) => true,
        // A null base, or the raw address `algebra::fold_unsafe_addresses` made the base.
        Inst::Intrinsic(Intrinsic::UnsafePut(_, false), ref args, _) if g.constant_value(args[1]) == Some(0) || g.ty(g.resolve(args[1])) == Ty::J => false,
        Inst::Intrinsic(k, ..) => !k.is_pure(),
        Inst::LoadField(..) => g.ordered.contains(&w),
        Inst::StoreField(_, off2, ..) if off2 != off => false,
        Inst::StoreField(..) => match (holder, g.field_holders.get(&w)) {
            (Some(a), Some(&b)) => a.is_subtype_of(b) || b.is_subtype_of(a),
            _ => true,
        },
        _ => false,
    }
}

/// Whether no path from position `ka` of block `a` to position `vb` of block `b` (which `a`
/// dominates, or precedes it in the same block) changes the field.
fn clear(g: &Graph, a: B, ka: usize, b: B, vb: usize, off: u32, holder: Option<&'static Klass>) -> bool {
    let range_clear = |blk: B, from: usize, to: usize| g.blocks[blk as usize].insts[from..to].iter().all(|&w| g.resolve(w) != w || !kills(g, w, off, holder));
    if a == b {
        return range_clear(a, ka + 1, vb);
    }
    // The blocks between: reachable from `a`'s successors without passing `a`, and reaching `b`.
    let mut fwd: HashSet<B> = HashSet::new();
    let mut stack: Vec<B> = g.blocks[a as usize].term.successors();
    while let Some(x) = stack.pop() {
        if x == a {
            // Back at `a`, the value is defined anew: only paths from its last definition count.
            continue;
        }
        if !fwd.insert(x) {
            continue;
        }
        if fwd.len() > MAX_REGION {
            return false;
        }
        if x != b {
            stack.extend(g.blocks[x as usize].term.successors());
        } else {
            // Past `b`, a path may come back into `b` (a loop around the load).
            stack.extend(g.blocks[x as usize].term.successors().into_iter().filter(|&s| s != a));
        }
    }
    let mut back: HashSet<B> = HashSet::new();
    let mut stack: Vec<B> = g.blocks[b as usize].preds.clone();
    while let Some(x) = stack.pop() {
        if x == a || !fwd.contains(&x) || !back.insert(x) {
            continue;
        }
        stack.extend(g.blocks[x as usize].preds.iter().copied());
    }
    // `b` lies on a cycle of the region: all of it may run before the load.
    let b_cycles = back.contains(&b);
    let n = g.blocks[a as usize].insts.len();
    range_clear(a, ka + 1, n)
        && back.iter().filter(|&&x| x != b).all(|&x| range_clear(x, 0, g.blocks[x as usize].insts.len()))
        && if b_cycles { range_clear(b, 0, g.blocks[b as usize].insts.len()) } else { range_clear(b, 0, vb) }
}
