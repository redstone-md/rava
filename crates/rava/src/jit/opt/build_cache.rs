//! The builder's local caches across blocks: value numbers, field values and bounds checks of
//! the current block, carried into a block whose only predecessor is the one left (an extended
//! basic block: inlined bodies, the returns from them, the paths behind type guards), else
//! started empty.

use super::build::Builder;
use super::ir::*;
use crate::types::BasicType;
use std::collections::HashMap;

/// Value numbers, field values and bounds checks valid at a point (`Builder::numbered`, `loads`,
/// `bounds`).
/// What holds on every path to the current point: values checked non-null, and values whose
/// class a guard proved exactly (a receiver past an inlined call behind one class guard), so a
/// later call on them binds statically however polluted its own profile is.
#[derive(Clone, Default)]
pub struct Facts {
    nonnull: Vec<V>,
    exact: Vec<(V, &'static crate::klass::Klass)>,
}

impl Facts {
    pub fn push(&mut self, v: V) {
        self.nonnull.push(v);
    }

    pub fn contains(&self, v: &V) -> bool {
        self.nonnull.contains(v)
    }

    /// `v` is non-null and exactly of class `k`.
    pub fn push_exact(&mut self, v: V, k: &'static crate::klass::Klass) {
        self.nonnull.push(v);
        self.exact.push((v, k));
    }

    pub fn exact(&self, v: V) -> Option<&'static crate::klass::Klass> {
        self.exact.iter().rev().find(|&&(x, _)| x == v).map(|&(_, k)| k)
    }

    pub fn clear(&mut self) {
        self.nonnull.clear();
        self.exact.clear();
    }

    /// Keeps what `other` (another path in) also has.
    pub fn narrow(&mut self, other: &Facts) {
        self.nonnull.retain(|v| other.nonnull.contains(v));
        self.exact.retain(|&(v, k)| other.exact(v).is_some_and(|o| std::ptr::eq(o, k)));
    }
}

pub(super) type Caches = (HashMap<(u8, u8, V, V, u8), V>, HashMap<(V, u32), V>, Vec<(V, V, V)>);

impl Builder {
    /// Adds `inst` to the current block, or reuses an identical pure instruction already
    /// computed (value numbering over the extended block), keeping the field values in step with
    /// memory effects.
    pub fn add(&mut self, inst: Inst, ty: Ty) -> V {
        let t = ty as u8;
        let key = match inst {
            Inst::Bin(op, a, b) => Some((1, op as u8, a, b, t)),
            Inst::Neg(a) => Some((2, 0, a, 0, t)),
            Inst::Convert(op, a) => Some((3, op, a, 0, t)),
            Inst::Compare(op, a, b) => Some((4, op, a, b, t)),
            _ => None,
        };
        if let Some(&v) = key.and_then(|k| self.numbered.get(&k)) {
            return v;
        }
        match inst {
            Inst::Call(..) | Inst::Monitor(..) | Inst::Fence => self.loads.clear(),
            Inst::Intrinsic(k, ..) if !k.is_pure() => self.loads.clear(),
            // Another object's field at this offset may be the same one.
            Inst::StoreField(o, off, bt, x) => {
                self.loads.retain(|&(_, f), _| f != off);
                // A stored value reads back unchanged unless the field narrows it.
                if matches!(bt, BasicType::Int | BasicType::Long | BasicType::Float | BasicType::Double | BasicType::Reference) {
                    self.loads.insert((o, off), x);
                }
            }
            _ => {}
        }
        let v = self.g.add(self.cur, inst, ty);
        if let Some(k) = key {
            self.numbered.insert(k, v);
        }
        v
    }

    /// Leaves the current block (keeping its caches for its successors) and takes on those of
    /// `only`, the next block's single predecessor if it has one: no other path reaches that
    /// block and an edge has no effects. Any other block starts empty.
    pub(super) fn carry_caches(&mut self, only: Option<B>) {
        let left = self.take_caches();
        self.exits.insert(self.cur, left);
        if let Some(c) = only.and_then(|p| self.exits.get(&p)).cloned() {
            self.set_caches(c);
        }
    }

    pub(super) fn take_caches(&mut self) -> Caches {
        (std::mem::take(&mut self.numbered), std::mem::take(&mut self.loads), std::mem::take(&mut self.bounds))
    }

    pub(super) fn set_caches(&mut self, (numbered, loads, bounds): Caches) {
        (self.numbered, self.loads, self.bounds) = (numbered, loads, bounds);
    }

    /// Forgets the caches of blocks from `first` on (their ids are about to be reused).
    pub(super) fn forget_blocks(&mut self, first: B) {
        self.exits.retain(|&b, _| b < first);
    }

}
