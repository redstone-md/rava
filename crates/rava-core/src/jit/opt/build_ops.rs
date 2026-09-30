//! Per-instruction graph building: each bytecode's effect on the abstract operand stack and
//! locals, as instructions of the current block.

use super::build::Builder;
use super::canon;
use super::ir::*;
use crate::classfile::Cp;
use crate::jit::analysis::{i2, i4, insn_len};
use crate::klass::{ACC_ABSTRACT, ACC_INTERFACE};
use crate::link::Resolved;
use crate::types::BasicType;

const ARRAY_ELEMS: [BasicType; 8] =
    [BasicType::Int, BasicType::Long, BasicType::Float, BasicType::Double, BasicType::Reference, BasicType::Byte, BasicType::Char, BasicType::Short];

impl Builder {
    pub(super) fn u2(&self, pc: usize) -> u16 {
        u16::from_be_bytes([self.sc.code[pc + 1], self.sc.code[pc + 2]])
    }

    /// An element load from a constant `@Stable` array at a constant index, when the element is
    /// set: the element as a constant (HotSpot's stable-array folding).
    fn stable_load(&mut self, op: u8) -> Option<()> {
        let n = self.sc.stack.len();
        let (arr, idx) = (self.sc.stack.get(n.checked_sub(2)?).copied().flatten()?, self.sc.stack[n - 1]?);
        let (a, i) = (self.g.constant(arr)?, self.g.constant(idx)?);
        let dims = *self.stable.get(&a)?;
        let bt = ARRAY_ELEMS[(op - 0x2e) as usize];
        let e = crate::jit::fold::stable_element(a as usize, i, bt)?;
        self.pop();
        self.pop();
        if dims > 1 && bt == BasicType::Reference {
            self.stable.insert(e, dims - 1);
        }
        self.push_const(Ty::of(bt), e);
        Some(())
    }

    pub(super) fn resolved(&self, index: u16) -> Option<Resolved> {
        self.sc.m.holder.peek_resolved(index)
    }

    /// Builds the instruction at `pc`: `Some(true)` to continue with the next one, `Some(false)`
    /// once it ended the block, `None` if it is not supported (before changing any state).
    pub(super) fn build_insn(&mut self, pc: usize) -> Option<bool> {
        let op = self.sc.code[pc];
        match op {
            0x00 => {}
            0x01 => self.push_const(Ty::A, 0),
            0x02..=0x08 => self.push_const(Ty::I, op as i64 - 3),
            0x09 | 0x0a => self.push_const(Ty::J, (op - 0x09) as i64),
            0x0b..=0x0d => self.push_const(Ty::F, ((op - 0x0b) as f32).to_bits() as i64),
            0x0e | 0x0f => self.push_const(Ty::D, ((op - 0x0e) as f64).to_bits() as i64),
            0x10 => self.push_const(Ty::I, self.sc.code[pc + 1] as i8 as i64),
            0x11 => self.push_const(Ty::I, i2(self.sc.code, pc + 1) as i64),
            0x12..=0x14 => self.ldc(pc)?,
            0x15..=0x19 => self.load_local(self.sc.code[pc + 1] as usize),
            0x1a..=0x2d => self.load_local(((op - 0x1a) % 4) as usize),
            0x2e..=0x35 if self.stable_load(op).is_some() => {}
            0x2e..=0x35 => {
                let s = self.state();
                let (idx, arr) = (self.pop(), self.pop());
                self.null_check(arr, s.clone());
                self.add(Inst::BoundsCheck(arr, idx, s), Ty::I);
                let bt = ARRAY_ELEMS[(op - 0x2e) as usize];
                let v = self.add(Inst::LoadIndexed(arr, idx, bt), Ty::of(bt));
                self.push(v);
            }
            0x36..=0x3a => self.store_local(self.sc.code[pc + 1] as usize),
            0x3b..=0x4e => self.store_local(((op - 0x3b) % 4) as usize),
            0x4f..=0x56 => self.array_store(op),
            0x57 => {
                self.sc.stack.pop();
            }
            0x58 => {
                self.sc.stack.pop();
                self.sc.stack.pop();
            }
            0x59..=0x5f => self.shuffle(op),
            0x60..=0x83 => self.arith(op)?,
            0x84 => {
                let (n, delta) = (self.sc.code[pc + 1] as usize, self.sc.code[pc + 2] as i8 as i64);
                self.iinc(n, delta);
            }
            0x85..=0x93 => {
                let v = self.pop();
                let ty = match op {
                    0x85 | 0x8c | 0x8f => Ty::J,
                    0x86 | 0x89 | 0x90 => Ty::F,
                    0x87 | 0x8a | 0x8d => Ty::D,
                    _ => Ty::I,
                };
                match self.g.constant(v).and_then(|c| canon::convert(op, c)) {
                    Some(c) => self.push_const(ty, c),
                    None => {
                        let r = self.add(Inst::Convert(op, v), ty);
                        self.push(r);
                    }
                }
            }
            0x94..=0x98 => {
                let (b, a) = (self.pop(), self.pop());
                match (self.g.constant(a), self.g.constant(b)) {
                    (Some(x), Some(y)) if op == 0x94 => self.push_const(Ty::I, canon::compare(op, x, y).unwrap()),
                    _ => {
                        let r = self.add(Inst::Compare(op, a, b), Ty::I);
                        self.push(r);
                    }
                }
            }
            0x99..=0xa6 | 0xc6 | 0xc7 => {
                self.branch(pc, op);
                return Some(false);
            }
            0xa7 | 0xc8 => {
                let off = if op == 0xa7 { i2(self.sc.code, pc + 1) as i32 } else { i4(self.sc.code, pc + 1) };
                let target = (pc as i64 + off as i64) as usize;
                // A jump to the next instruction is a fall-through: it starts no block.
                if target == pc + insn_len(self.sc.code, pc) {
                    return Some(true);
                }
                self.goto(target);
                return Some(false);
            }
            0xaa | 0xab => {
                self.switch(pc, op);
                return Some(false);
            }
            0xac..=0xb1 => {
                let v = (op != 0xb1).then(|| self.pop());
                match self.sc.cont {
                    Some(k) => self.inline_return(k, v),
                    None => self.g.blocks[self.cur as usize].term = Term::Return(v),
                }
                return Some(false);
            }
            0xb2..=0xb5 => self.field(pc, op)?,
            0xb6..=0xb9 | 0xba => {
                let continues = if op == 0xba { self.invokedynamic(pc)? } else { self.invoke(pc, op)? };
                if !continues {
                    return Some(false);
                }
            }
            0xbb => {
                let Some(Resolved::Class(k)) = self.resolved(self.u2(pc)) else { return None };
                if k.access & (ACC_ABSTRACT | ACC_INTERFACE) != 0 {
                    return None;
                }
                let s = self.state();
                self.init_check(k, &s);
                let v = self.add(Inst::New(k, s), Ty::A);
                self.exact.push((v, k));
                self.nonnull.push(v);
                self.push(v);
            }
            0xbc | 0xbd => {
                let vm = crate::vm::vm();
                let k = if op == 0xbc {
                    BasicType::from_newarray_code(self.sc.code[pc + 1]).and_then(|t| vm.primitive_array_class(t).ok())?
                } else {
                    let Some(Resolved::Class(e)) = self.resolved(self.u2(pc)) else { return None };
                    vm.array_class_of(e).ok()?
                };
                let s = self.state();
                let len = self.pop();
                let v = self.add(Inst::NewArray(k, len, s), Ty::A);
                self.push(v);
            }
            0xc5 if self.sc.code[pc + 3] == 2 => {
                let Some(Resolved::Class(k)) = self.resolved(self.u2(pc)) else { return None };
                let s = self.state();
                let (second, first) = (self.pop(), self.pop());
                let first = self.add(Inst::Convert(0x85, first), Ty::J);
                let mask = self.konst(Ty::J, 0xffff_ffff);
                let low = self.add(Inst::Bin(BinOp::And, first, mask), Ty::J);
                let second = self.add(Inst::Convert(0x85, second), Ty::J);
                let by = self.konst(Ty::I, 32);
                let high = self.add(Inst::Bin(BinOp::Shl, second, by), Ty::J);
                let counts = self.add(Inst::Bin(BinOp::Or, low, high), Ty::J);
                let v = self.add(Inst::NewMulti(k, counts, s), Ty::A);
                self.push(v);
            }
            0xbe => {
                let s = self.state();
                let arr = self.pop();
                self.null_check(arr, s);
                let v = self.add(Inst::ArrayLength(arr), Ty::I);
                self.push(v);
            }
            0xbf => {
                self.deopt();
                return Some(false);
            }
            0xc0 | 0xc1 => {
                let Some(Resolved::Class(k)) = self.resolved(self.u2(pc)) else { return self.unresolved_type_check(op) };
                let s = self.state();
                let v = self.pop();
                if let Some(o) = self.g.constant(v) {
                    let is = o != 0 && crate::jit::fold::class_of(o as usize).is_subtype_of(k);
                    match op {
                        0xc1 => self.push_const(Ty::I, is as i64),
                        _ if o == 0 || is => self.push(v),
                        _ => {
                            self.push(v);
                            self.deopt();
                            return Some(false);
                        }
                    }
                    return Some(true);
                }
                let r = if op == 0xc0 { self.add(Inst::CheckCast(v, k, s), Ty::A) } else { self.add(Inst::InstanceOf(v, k), Ty::I) };
                self.push(r);
            }
            0xc2 | 0xc3 => {
                let s = self.state();
                let o = self.pop();
                self.null_check(o, s.clone());
                self.add(Inst::Monitor(op == 0xc2, o, s), Ty::I);
            }
            0xc4 => {
                let n = u16::from_be_bytes([self.sc.code[pc + 2], self.sc.code[pc + 3]]) as usize;
                match self.sc.code[pc + 1] {
                    0x15..=0x19 => self.load_local(n),
                    0x36..=0x3a => self.store_local(n),
                    0x84 => self.iinc(n, i2(self.sc.code, pc + 4) as i64),
                    _ => return None,
                }
            }
            _ => return None,
        }
        Some(true)
    }

    pub(super) fn push_const(&mut self, ty: Ty, bits: i64) {
        let v = self.konst(ty, bits);
        self.push(v);
    }

    fn ldc(&mut self, pc: usize) -> Option<()> {
        let index = if self.sc.code[pc] == 0x12 { self.sc.code[pc + 1] as u16 } else { self.u2(pc) };
        let (ty, bits) = match self.sc.cf.pool[index as usize] {
            Cp::Int(v) => (Ty::I, v as i64),
            Cp::Float(v) => (Ty::F, v.to_bits() as i64),
            Cp::Long(v) => (Ty::J, v),
            Cp::Double(v) => (Ty::D, v.to_bits() as i64),
            Cp::String(_) | Cp::MethodType(_) | Cp::MethodHandle { .. } => match self.resolved(index)? {
                Resolved::String(o) | Resolved::Constant(o) => (Ty::A, o as i64),
                _ => return None,
            },
            Cp::Class(_) => match self.resolved(index)? {
                Resolved::Class(k) if !k.mirror().is_null() => (Ty::A, k.mirror() as i64),
                _ => return None,
            },
            _ => return None,
        };
        self.push_const(ty, bits);
        Some(())
    }

    fn iinc(&mut self, n: usize, delta: i64) {
        let v = self.sc.locals[n].expect("iinc of an assigned local");
        let c = self.konst(Ty::I, delta);
        self.sc.locals[n] = Some(self.add(Inst::Bin(BinOp::Add, v, c), Ty::I));
    }

    fn array_store(&mut self, op: u8) {
        let s = self.state();
        let v = self.pop();
        let (idx, arr) = (self.pop(), self.pop());
        self.null_check(arr, s.clone());
        self.add(Inst::BoundsCheck(arr, idx, s.clone()), Ty::I);
        let bt = ARRAY_ELEMS[(op - 0x4f) as usize];
        if bt == BasicType::Reference {
            self.add(Inst::StoreCheck(arr, v, s), Ty::A);
        }
        self.add(Inst::StoreIndexed(arr, idx, bt, v), Ty::I);
    }

    /// dup / dup_x1 / dup_x2 / dup2 / dup2_x1 / dup2_x2 / swap on raw slots.
    fn shuffle(&mut self, op: u8) {
        let (take, order): (usize, &[usize]) = match op {
            0x59 => (1, &[0, 0]),
            0x5a => (2, &[1, 0, 1]),
            0x5b => (3, &[2, 0, 1, 2]),
            0x5c => (2, &[0, 1, 0, 1]),
            0x5d => (3, &[1, 2, 0, 1, 2]),
            0x5e => (4, &[2, 3, 0, 1, 2, 3]),
            _ => (2, &[1, 0]),
        };
        let top: Vec<Option<V>> = self.sc.stack.split_off(self.sc.stack.len() - take);
        for &i in order {
            self.sc.stack.push(top[i]);
        }
    }

    fn arith(&mut self, op: u8) -> Option<()> {
        let ty = match op {
            0x60..=0x77 => [Ty::I, Ty::J, Ty::F, Ty::D][((op - 0x60) % 4) as usize],
            _ => if op % 2 == 0 { Ty::I } else { Ty::J },
        };
        if (0x74..=0x77).contains(&op) {
            let a = self.pop();
            match self.g.constant(a).and_then(|c| canon::bin(BinOp::Sub, ty, 0, c)) {
                Some(c) => self.push_const(ty, c),
                None => {
                    let r = self.add(Inst::Neg(a), ty);
                    self.push(r);
                }
            }
            return Some(());
        }
        let bin = match op {
            0x60..=0x63 => BinOp::Add,
            0x64..=0x67 => BinOp::Sub,
            0x68..=0x6b => BinOp::Mul,
            0x6c..=0x6f => BinOp::Div,
            0x70..=0x73 => BinOp::Rem,
            0x78 | 0x79 => BinOp::Shl,
            0x7a | 0x7b => BinOp::Shr,
            0x7c | 0x7d => BinOp::Ushr,
            0x7e | 0x7f => BinOp::And,
            0x80 | 0x81 => BinOp::Or,
            _ => BinOp::Xor,
        };
        let s = matches!(bin, BinOp::Div | BinOp::Rem).then(|| self.state());
        let (b, a) = (self.pop(), self.pop());
        if let (Some(s), Ty::I | Ty::J) = (s, ty) {
            if self.g.constant(b).map_or(true, |c| c == 0) {
                self.add(Inst::ZeroCheck(b, s), ty);
            }
        }
        if let Some(c) = self.g.constant(a).zip(self.g.constant(b)).and_then(|(x, y)| canon::bin(bin, ty, x, y)) {
            self.push_const(ty, c);
            return Some(());
        }
        // `a[i] & 0xff` of a byte array (and `& 0xffff` of a short array) is a zero-extending load.
        if let (BinOp::And, Some(mask), Ty::I) = (bin, self.g.constant(b), ty) {
            if let Some(v) = self.unsigned_load(a, mask) {
                self.push(v);
                return Some(());
            }
        }
        // Division by a power of two is multiplication by its (exact) reciprocal.
        if let (BinOp::Div, Some(recip)) = (bin, self.g.constant(b).and_then(|c| exact_reciprocal(ty, c))) {
            let c = self.konst(ty, recip);
            let r = self.add(Inst::Bin(BinOp::Mul, a, c), ty);
            self.push(r);
            return Some(());
        }
        let r = self.add(Inst::Bin(bin, a, b), ty);
        self.push(r);
        Some(())
    }

    fn branch(&mut self, pc: usize, op: u8) {
        let target = (pc as i64 + i2(self.sc.code, pc + 1) as i64) as usize;
        let next = pc + insn_len(self.sc.code, pc);
        let (cmp, ty, a, b) = match op {
            0x99..=0x9e => {
                let a = self.pop();
                let zero = self.konst(Ty::I, 0);
                (int_cmp(op - 0x99), Ty::I, a, zero)
            }
            0x9f..=0xa4 => {
                let (b, a) = (self.pop(), self.pop());
                (int_cmp(op - 0x9f), Ty::I, a, b)
            }
            0xa5 | 0xa6 => {
                let (b, a) = (self.pop(), self.pop());
                (if op == 0xa5 { Cmp::Eq } else { Cmp::Ne }, Ty::A, a, b)
            }
            _ => {
                let a = self.pop();
                let null = self.konst(Ty::A, 0);
                (if op == 0xc6 { Cmp::Eq } else { Cmp::Ne }, Ty::A, a, null)
            }
        };
        let (then, other) = (self.block_at(target), self.block_at(next));
        if let Some(taken) = self.known_branch(cmp, ty, a, b) {
            let to = if taken { then } else { other };
            self.edge(to);
            self.g.blocks[self.cur as usize].term = Term::Goto(to);
            return;
        }
        let (ty, a, b, unordered) = self.fuse_compare(cmp, ty, a, b);
        let (then, other) = match self.sc.pruned.get(&pc).copied() {
            Some(true) => (self.untaken(target), {
                self.edge(other);
                other
            }),
            Some(false) => {
                self.edge(then);
                (then, self.untaken(next))
            }
            None => {
                self.edge(then);
                self.edge(other);
                (then, other)
            }
        };
        self.g.blocks[self.cur as usize].term = Term::If { cmp, ty, a, b, then, other, unordered };
    }

    fn switch(&mut self, pc: usize, op: u8) {
        let key = self.pop();
        let p = (pc + 4) & !3;
        let rel = |off: i32| (pc as i64 + off as i64) as usize;
        let mut cases = Vec::new();
        if op == 0xaa {
            let (low, high) = (i4(self.sc.code, p + 4), i4(self.sc.code, p + 8));
            for (i, k) in (low..=high).enumerate() {
                cases.push((k, rel(i4(self.sc.code, p + 12 + 4 * i))));
            }
        } else {
            for i in 0..i4(self.sc.code, p + 4) as usize {
                cases.push((i4(self.sc.code, p + 8 + 8 * i), rel(i4(self.sc.code, p + 12 + 8 * i))));
            }
        }
        let default = self.block_at(rel(i4(self.sc.code, p)));
        let mut targets: Vec<B> = cases.iter().map(|c| self.block_at(c.1)).chain(std::iter::once(default)).collect();
        targets.sort_unstable();
        targets.dedup();
        for t in targets {
            self.edge(t);
        }
        let cases = cases.into_iter().map(|(k, bci)| (k, self.block_at(bci))).collect();
        self.g.blocks[self.cur as usize].term = Term::Switch { key, cases, default };
    }

    /// The outcome of a branch decided at compile time: constant operands, or a null test of
    /// a value known to be non-null.
    /// Replaces the signed byte or short array load `a`, just made and held nowhere else, by
    /// the zero-extending load `a & mask` amounts to.
    fn unsigned_load(&mut self, a: V, mask: i64) -> Option<V> {
        let Inst::LoadIndexed(arr, idx, bt) = self.g.values[a as usize].inst else { return None };
        let unsigned = match (bt, mask) {
            (BasicType::Byte, 0xff) => BasicType::Boolean,
            (BasicType::Short, 0xffff) => BasicType::Char,
            _ => return None,
        };
        let held = self.sc.stack.iter().chain(self.sc.locals.iter()).any(|&s| s == Some(a));
        if self.g.blocks[self.cur as usize].insts.last() != Some(&a) || held {
            return None;
        }
        self.g.values[a as usize].inst = Inst::LoadIndexed(arr, idx, unsigned);
        Some(a)
    }

    /// `lcmp`/`fcmp`/`dcmp` just computed and tested against zero becomes a comparison of its
    /// operands: `(ty, a, b, unordered)` of the branch. The compare is dropped when nothing
    /// else holds its result.
    fn fuse_compare(&mut self, cmp: Cmp, ty: Ty, a: V, b: V) -> (Ty, V, V, bool) {
        let block = &self.g.blocks[self.cur as usize];
        // The compare is the block's last instruction but for the constant zero after it.
        let mut tail = block.insts.iter().rev().skip_while(|&&v| v == b && self.g.constant(v).is_some());
        let (Ty::I, Some(0), Some(&last)) = (ty, self.g.constant(b), tail.next()) else { return (ty, a, b, false) };
        let Inst::Compare(op, x, y) = self.g.values[a as usize].inst else { return (ty, a, b, false) };
        let held = self.sc.stack.iter().chain(self.sc.locals.iter()).any(|&s| s == Some(a));
        if last != a || held {
            return (ty, a, b, false);
        }
        self.g.blocks[self.cur as usize].insts.retain(|&v| v != a);
        self.numbered.retain(|_, &mut v| v != a);
        let operand = match op {
            0x94 => Ty::J,
            0x95 | 0x96 => Ty::F,
            _ => Ty::D,
        };
        // fcmpl/dcmpl give -1 for NaN, fcmpg/dcmpg 1.
        let nan = if matches!(op, 0x95 | 0x97) { -1 } else { 1 };
        (operand, x, y, canon::branch(cmp, Ty::I, nan, 0))
    }

    fn known_branch(&self, cmp: Cmp, ty: Ty, a: V, b: V) -> Option<bool> {
        if let (Some(x), Some(y)) = (self.g.constant(a), self.g.constant(b)) {
            return Some(canon::branch(cmp, ty, x, y));
        }
        if ty != Ty::A || !matches!(cmp, Cmp::Eq | Cmp::Ne) {
            return None;
        }
        let null = match (self.g.constant(a), self.g.constant(b)) {
            (_, Some(0)) => self.nullness(a)?,
            (Some(0), _) => self.nullness(b)?,
            _ => return None,
        };
        Some(null == (cmp == Cmp::Eq))
    }
}

/// The reciprocal of a float or double constant (slot encoding) that is a power of two whose
/// reciprocal is a normal number, so `x / c == x * (1 / c)` for every `x`.
fn exact_reciprocal(ty: Ty, bits: i64) -> Option<i64> {
    match ty {
        Ty::D => {
            let c = f64::from_bits(bits as u64);
            let r = 1.0 / c;
            (c.is_normal() && c.to_bits() & 0x000f_ffff_ffff_ffff == 0 && r.is_normal()).then(|| r.to_bits() as i64)
        }
        Ty::F => {
            let c = f32::from_bits(bits as u32);
            let r = 1.0 / c;
            (c.is_normal() && c.to_bits() & 0x007f_ffff == 0 && r.is_normal()).then(|| r.to_bits() as i64)
        }
        _ => None,
    }
}

fn int_cmp(c: u8) -> Cmp {
    [Cmp::Eq, Cmp::Ne, Cmp::Lt, Cmp::Ge, Cmp::Gt, Cmp::Le][c as usize]
}
