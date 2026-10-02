//! Scalar replacement of non-escaping allocations. An object whose identity is never observed
//! and whose fields, after forwarding (`fields.rs`), are read nowhere survives only in
//! deoptimization states. Such an allocation is removed from the hot path and recorded as a
//! `Virtual`; if the compiled code ever deoptimizes, the interpreter rebuilds the object from
//! its field values (`deopt.rs`). This is what lets a captured lambda or a confined temporary
//! cost nothing until the rare deopt, as C2's scalar replacement does.
//!
//! Only a plain `New` with a simple, provably-safe construction qualifies: all its stores sit
//! in one block and dominate every state that mentions the object (so the rebuilt fields are
//! the ones the interpreter would have), the object escapes nowhere else, and no field is itself
//! a replaced object (no nesting). Arrays are left alone — they can raise
//! `NegativeArraySizeException`, a guest-visible effect.
//!
//! Off by default (`RAVA_SCALAR_REPLACE=1` enables it): it pays for a long-running process but
//! costs compile time, and a cold boot — the engine's yardstick — is compile-latency bound, so
//! removing an allocation the interpreter may rebuild is a net loss there.

use super::ir::*;
use std::collections::HashMap;

pub fn eliminate(g: &mut Graph) {
    // Opt-in, and only for methods that allocate something replaceable.
    if env_once!("RAVA_SCALAR_REPLACE").is_none() || !g.values.iter().any(|v| matches!(v.inst, Inst::New(..))) {
        return;
    }
    // A value escapes unless its every use is as the object a field store writes into.
    let mut escapes = vec![false; g.values.len()];
    // Field stores into each object, with the block they are in.
    let mut stores: Vec<Vec<(V, B)>> = vec![Vec::new(); g.values.len()];
    for b in 0..g.blocks.len() as B {
        for &v in &g.blocks[b as usize].insts {
            // An instruction aliased away (a trivial phi, a forwarded load) is dead: its operands
            // are not real uses, so they do not make an object escape.
            if g.resolve(v) != v {
                continue;
            }
            match g.values[v as usize].inst {
                Inst::StoreField(base, _, _, val) => {
                    stores[g.resolve(base) as usize].push((v, b));
                    escapes[g.resolve(val) as usize] = true;
                }
                Inst::StoreIndexed(base, idx, _, val) => {
                    escapes[g.resolve(base) as usize] = true;
                    escapes[g.resolve(idx) as usize] = true;
                    escapes[g.resolve(val) as usize] = true;
                }
                _ => g.values[v as usize].inst.inputs().into_iter().for_each(|x| escapes[g.resolve(x) as usize] = true),
            }
        }
        g.blocks[b as usize].term.inputs().into_iter().for_each(|x| escapes[g.resolve(x) as usize] = true);
    }

    let candidates: Vec<V> = (0..g.values.len() as V)
        .filter(|&v| matches!(g.values[v as usize].inst, Inst::New(..)) && g.resolve(v) == v && !escapes[v as usize] && !stores[v as usize].is_empty())
        .collect();
    if candidates.is_empty() {
        return;
    }
    let mut is_candidate = vec![false; g.values.len()];
    for &v in &candidates {
        is_candidate[v as usize] = true;
    }

    let dom = super::dom::Dom::new(g);
    let mut rpo_index = vec![usize::MAX; g.blocks.len()];
    for (i, &b) in dom.rpo.iter().enumerate() {
        rpo_index[b as usize] = i;
    }
    // Position of each value within its block, for the within-block domination check.
    let mut pos = vec![(usize::MAX, usize::MAX); g.values.len()]; // (block, index in block)
    for (b, blk) in g.blocks.iter().enumerate() {
        for (i, &v) in blk.insts.iter().enumerate() {
            pos[v as usize] = (b, i);
        }
    }

    let mut dead = vec![false; g.values.len()];
    for &v in &candidates {
        // All stores in one block, each field once, no field being another replaced object.
        let blocks: Vec<B> = stores[v as usize].iter().map(|&(_, b)| b).collect();
        let home = blocks[0];
        if blocks.iter().any(|&b| b != home) {
            continue;
        }
        let mut fields: HashMap<u32, (crate::types::BasicType, V)> = HashMap::new();
        let mut last_store = 0usize;
        let mut nested = false;
        for &(s, _) in &stores[v as usize] {
            let Inst::StoreField(_, off, ty, val) = g.values[s as usize].inst else { unreachable!() };
            fields.insert(off, (ty, g.resolve(val)));
            nested |= is_candidate[g.resolve(val) as usize];
            last_store = last_store.max(pos[s as usize].1);
        }
        if nested {
            continue;
        }
        // Every state that mentions v must be dominated by the stores: in a block the home block
        // strictly dominates, or later in the home block than the last store.
        if !states_dominated(g, v, home, &rpo_index, &dom, last_store, &pos) {
            continue;
        }
        let mut field_list: Vec<(u32, crate::types::BasicType, V)> = fields.into_iter().map(|(o, (t, val))| (o, t, val)).collect();
        field_list.sort_by_key(|&(o, ..)| o);
        let Inst::New(klass, _) = g.values[v as usize].inst else { unreachable!() };
        g.virtuals.insert(v, Virtual { klass, fields: field_list });
        dead[v as usize] = true;
        for &(s, _) in &stores[v as usize] {
            dead[s as usize] = true;
        }
    }
    for b in &mut g.blocks {
        b.insts.retain(|&v| !dead[v as usize]);
    }
}

/// Whether every state referencing `v` is dominated by `v`'s stores (block `home`, last store at
/// block index `last_store`). A state in the home block must be past the last store; a state
/// elsewhere must be in a block the home block dominates.
fn states_dominated(g: &Graph, v: V, home: B, rpo_index: &[usize], dom: &super::dom::Dom, last_store: usize, pos: &[(usize, usize)]) -> bool {
    // Raw mention: whether a state (callers included) holds v in a local or stack slot.
    fn mentions(g: &Graph, s: &State, v: V) -> bool {
        s.locals.iter().chain(s.stack.iter()).flatten().any(|&x| g.resolve(x) == v) || s.caller.as_ref().is_some_and(|c| mentions(g, c, v))
    }
    let block_dominated = |b: usize| rpo_index[b] != usize::MAX && dom.dominates(rpo_index[home as usize], rpo_index[b]);
    for (bi, blk) in g.blocks.iter().enumerate() {
        for &user in &blk.insts {
            if let Some(s) = g.values[user as usize].inst.state() {
                if mentions(g, s, v) {
                    let (b, i) = pos[user as usize];
                    let ok = if b == home as usize { i > last_store } else { block_dominated(b) };
                    if !ok {
                        return false;
                    }
                }
            }
        }
        // A terminator runs after its block's instructions, so the home block itself is fine.
        if let Term::Deopt(s) = &blk.term {
            if mentions(g, s, v) && bi != home as usize && !block_dominated(bi) {
                return false;
            }
        }
    }
    true
}
