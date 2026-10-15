//! Stores overwritten before anything can see them (C2's back-to-back store folding): within a
//! block, a store to a field or static that a later store to the same place replaces goes when
//! nothing between may read memory or leave the compiled code (a call, a trap's deoptimization,
//! an intrinsic, an ordered access). A run of `sink += ...; sink ^= ...;` statements, whose
//! loads numbering already reads from the previous store, keeps only its last store.

use super::ir::*;
use crate::types::BasicType;
use std::collections::{HashMap, HashSet};

/// The place a store writes: a static's slot address, or an object, a field offset (both with
/// the width written).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Place {
    Static(usize, BasicType),
    Field(V, u32, BasicType),
}

pub fn eliminate(g: &mut Graph) {
    let mut gone: HashSet<V> = HashSet::new();
    for b in 0..g.blocks.len() {
        // The last store to each place not yet observable, by place.
        let mut pending: HashMap<Place, V> = HashMap::new();
        for &v in &g.blocks[b].insts {
            if g.resolve(v) != v {
                continue;
            }
            let ordered = g.ordered.contains(&v);
            match *g.inst(v) {
                Inst::StoreStatic(addr, bt, _) if !ordered => {
                    if let Some(old) = pending.insert(Place::Static(addr, bt), v) {
                        gone.insert(old);
                    }
                }
                Inst::StoreField(o, off, bt, _) if !ordered => {
                    // A store to the same offset of another object, which may be this one,
                    // overwrites without reading: earlier stores stay replaceable.
                    if let Some(old) = pending.insert(Place::Field(g.resolve(o), off, bt), v) {
                        gone.insert(old);
                    }
                }
                Inst::LoadStatic(addr, _) if !ordered => pending.retain(|p, _| !matches!(*p, Place::Static(a, _) if a == addr)),
                Inst::LoadField(_, off, _) if !ordered => pending.retain(|p, _| !matches!(*p, Place::Field(_, off2, _) if off2 == off)),
                Inst::Param(_)
                | Inst::Const(_)
                | Inst::Copy(_)
                | Inst::Phi(_)
                | Inst::Neg(_)
                | Inst::Bin(..)
                | Inst::Convert(..)
                | Inst::Compare(..)
                | Inst::ArrayLength(_)
                | Inst::LoadIndexed(..)
                | Inst::StoreIndexed(..)
                | Inst::InstanceOf(..)
                | Inst::Select(..) => {}
                // Anything else may read memory, or leave the code with it as it stands.
                _ => pending.clear(),
            }
        }
    }
    if !gone.is_empty() {
        for blk in &mut g.blocks {
            blk.insts.retain(|v| !gone.contains(v));
        }
    }
}
