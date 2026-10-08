//! Array accesses: element loads and stores, lengths, and their bounds checks.
//!
//! A bounds check is dropped when the index is provably inside an array of known length (a
//! constant array, or one allocated here with a constant length) or the block already checked
//! it. Checks with constant indices of one array within a block collapse into the first
//! (C2's range-check smearing): it checks the largest index, and when that fails it
//! deoptimizes before anything after it ran, so the interpreter redoes every access in order.

use super::build::Builder;
use super::ir::*;
use crate::types::BasicType;
use std::rc::Rc;

pub(super) const ARRAY_ELEMS: [BasicType; 8] =
    [BasicType::Int, BasicType::Long, BasicType::Float, BasicType::Double, BasicType::Reference, BasicType::Byte, BasicType::Char, BasicType::Short];

impl Builder {
    /// `iaload` .. `saload`.
    pub(super) fn array_load(&mut self, op: u8) {
        let s = self.state();
        let (idx, arr) = (self.pop(), self.pop());
        self.null_check(arr, s.clone());
        self.bounds_check(arr, idx, s);
        let bt = ARRAY_ELEMS[(op - 0x2e) as usize];
        // The element the block already read (or stored) since the last memory effect. Not
        // for narrow types: masking may turn a byte or short load into an unsigned one.
        let key = (arr, super::build_cache::ELEM | idx);
        let wide = matches!(bt, BasicType::Int | BasicType::Long | BasicType::Float | BasicType::Double | BasicType::Reference);
        let v = match self.loads.get(&key) {
            Some(&v) if wide => v,
            _ => {
                let v = self.add(Inst::LoadIndexed(arr, idx, bt), Ty::of(bt));
                if wide {
                    self.loads.insert(key, v);
                }
                v
            }
        };
        self.push(v);
    }

    /// `iastore` .. `sastore`.
    pub(super) fn array_store(&mut self, op: u8) {
        let s = self.state();
        let v = self.pop();
        let (idx, arr) = (self.pop(), self.pop());
        self.null_check(arr, s.clone());
        self.bounds_check(arr, idx, s.clone());
        let bt = ARRAY_ELEMS[(op - 0x4f) as usize];
        if bt == BasicType::Reference {
            self.add(Inst::StoreCheck(arr, v, s), Ty::A);
        }
        self.add(Inst::StoreIndexed(arr, idx, bt, v), Ty::I);
    }

    /// `arraylength`.
    pub(super) fn array_length(&mut self) {
        let s = self.state();
        let arr = self.pop();
        self.null_check(arr, s);
        let v = match self.known_length(arr) {
            Some(n) => self.konst(Ty::I, n),
            None => self.add(Inst::ArrayLength(arr), Ty::I),
        };
        self.push(v);
    }

    fn bounds_check(&mut self, arr: V, idx: V, s: Rc<State>) {
        let range = self.index_range(idx);
        if let (Some(len), Some((lo, hi))) = (self.known_length(arr), range) {
            if lo >= 0 && hi < len {
                return;
            }
        }
        if self.bounds.iter().any(|&(a, i, _)| a == arr && i == idx) {
            return;
        }
        if let Some((c, _)) = range.filter(|&(lo, hi)| lo == hi && lo >= 0) {
            let first = self.bounds.iter().find(|&&(a, i, _)| a == arr && self.g.constant(i).is_some_and(|k| k >= 0)).copied();
            if let Some((_, i, check)) = first {
                if c > self.g.constant(i).unwrap_or(0) {
                    let Inst::BoundsCheck(_, index, _) = &mut self.g.values[check as usize].inst else { unreachable!() };
                    *index = idx;
                    self.bounds.iter_mut().find(|b| b.2 == check).unwrap().1 = idx;
                }
                return;
            }
        }
        let check = self.add(Inst::BoundsCheck(arr, idx, s), Ty::I);
        self.bounds.push((arr, idx, check));
    }

    /// The length of `arr` when this compilation knows it.
    fn known_length(&self, arr: V) -> Option<i64> {
        let arr = self.g.resolve(arr);
        match self.g.inst(arr) {
            // A constant array (a folded static final): only an actual array object counts, as
            // code the verifier would reject (or a dead path) may index anything.
            Inst::Const(a) if *a != 0 && self.g.ty(arr) == Ty::A && crate::vm::vm().heap.is_object(*a as usize) => {
                let o = *a as crate::heap::Oop;
                // SAFETY: a live object (static finals and constants stay reachable).
                let k = unsafe { crate::heap::obj::klass(o) };
                // SAFETY: as above, an array.
                (!matches!(k.kind, crate::klass::KlassKind::Instance)).then(|| unsafe { crate::heap::obj::array_length(o) } as i64)
            }
            Inst::NewArray(_, len, _) => self.g.constant(*len).filter(|&n| n >= 0),
            _ => None,
        }
    }

    /// Bounds on an int index: constants, masks with a non-negative constant, unsigned shifts.
    fn index_range(&self, idx: V) -> Option<(i64, i64)> {
        let idx = self.g.resolve(idx);
        if let Some(c) = self.g.constant(idx) {
            return Some((c, c));
        }
        match *self.g.inst(idx) {
            Inst::Bin(BinOp::And, a, b) => {
                let m = self.g.constant(b).or_else(|| self.g.constant(a))?;
                (m >= 0).then_some((0, m))
            }
            Inst::Bin(BinOp::Ushr, _, b) => {
                let n = self.g.constant(b)? & 31;
                (n > 0).then(|| (0, (1i64 << (32 - n)) - 1))
            }
            _ => None,
        }
    }
}
