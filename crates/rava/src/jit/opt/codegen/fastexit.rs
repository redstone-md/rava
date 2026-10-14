//! Shrink-wrapping in small: a fast exit at the register entry, before the prologue. When the
//! entry block ends in a test whose one side only computes a little integer arithmetic and
//! returns (or reads a field of the receiver), that test and that side run first, on the arguments still in their registers and
//! with scratch registers only: the early return (a recursion's base case, a cached value) pays
//! no callee-saved pushes, stack check, frame header or entry poll. Otherwise the code goes on
//! into the prologue with every entry register as it came, and the method runs as compiled.
//!
//! The fast exit neither allocates, calls, throws nor deoptimizes, as a frameless leaf.

use super::{cond, invert, Gen};
use crate::jit::abi::ArgReg;
use crate::jit::asm::{mem, Alu, R};
use crate::jit::emit_obj::load_typed_to;
use crate::types::BasicType;
use crate::jit::opt::ir::{BinOp, Graph, Inst, Term, Ty, V};

/// Registers the fast exit computes in: none carries an argument, the frame base (rsi), the
/// method (rdx) or the thread.
const TEMPS: [R; 3] = [R::Rax, R::Rcx, R::R11];

/// Where a value of the fast exit is.
#[derive(Clone, Copy)]
enum Place {
    Reg(R),
    Imm(i64),
}

impl Gen<'_> {
    /// Emits the fast exit, if the method has one, at the register entry.
    pub(super) fn fast_exit(&mut self) {
        if self.frameless || self.reg_params.is_empty() {
            return;
        }
        let g = self.g;
        let Term::If { cmp, ty, a, b, then, other, .. } = g.blocks[0].term else { return };
        if !matches!(ty, Ty::I | Ty::J | Ty::A) {
            return;
        }
        // The side that only returns: blocks of simple values (and phis, which take what the
        // path passes) joined by jumps, from the entry block to a return; with each block, the
        // block it is entered from.
        let ok = |v: V| g.resolve(v) != v || matches!(g.values[v as usize].inst, Inst::Phi(_)) || simple(g, v);
        let path = |x: u32| -> Option<Vec<(u32, u32)>> {
            let (mut at, mut from, mut out) = (x, 0, Vec::new());
            for _ in 0..4 {
                let blk = &g.blocks[at as usize];
                if at == 0 || blk.preds.iter().filter(|&&p| p == from).count() != 1 || !blk.insts.iter().all(|&v| ok(v)) {
                    return None;
                }
                out.push((at, from));
                match blk.term {
                    Term::Return(_) => return Some(out),
                    Term::Goto(next) => (from, at) = (at, next),
                    _ => return None,
                }
            }
            None
        };
        let (path, slow_on) = match (path(then), path(other)) {
            (Some(p), _) => (p, invert(cond(cmp))),
            (None, Some(p)) => (p, cond(cmp)),
            _ => return,
        };
        // Values standing for others (`Graph::resolve`) have no code of their own.
        let own = |b: u32| g.blocks[b as usize].insts.iter().copied().filter(|&v| g.resolve(v) == v).map(move |v| (v, b));
        let entry: Vec<V> = own(0).map(|(v, _)| v).collect();
        let tail: Vec<(V, u32)> = path.iter().flat_map(|&(b, _)| own(b)).collect();
        if !entry.iter().all(|&v| simple(g, v)) {
            return;
        }
        let last = path.last().expect("a path").0;
        let Term::Return(result) = g.blocks[last as usize].term else { return };
        if result.is_some_and(|r| !matches!(g.ty(r), Ty::I | Ty::J | Ty::A)) {
            return;
        }
        // Each value's place, scratch registers taken in order (no reuse: a few values only).
        let mut place: std::collections::HashMap<V, Place> = std::collections::HashMap::new();
        let mut free = TEMPS.iter().copied();
        let body: Vec<(V, u32)> = entry.iter().map(|&v| (v, 0)).chain(tail.iter().copied()).collect();
        for &(v, blk) in &body {
            let p = match g.values[v as usize].inst {
                Inst::Param(slot) => match self.reg_params.iter().find(|p| p.2 == slot).map(|p| p.1) {
                    Some(ArgReg::Gpr(r)) => Place::Reg(r),
                    _ => return,
                },
                Inst::Const(c) => Place::Imm(c),
                // A copy (`split.rs`) is wherever its value is: nothing changes either; so is a
                // phi's selected input along the fast path.
                Inst::Copy(x) => match place_of(g, &place, x) {
                    Some(p) => p,
                    None => return,
                },
                Inst::Phi(ref ins) => {
                    let pred = path.iter().find(|p| p.0 == blk).expect("a block of the path").1;
                    let from = g.blocks[blk as usize].preds.iter().position(|&p| p == pred).expect("entered along the path");
                    match place_of(g, &place, ins[from]) {
                        Some(p) => p,
                        None => return,
                    }
                }
                _ => match free.next() {
                    Some(r) => Place::Reg(r),
                    None => return,
                },
            };
            place.insert(v, p);
        }
        let operand = |v: V| place_of(g, &place, v);
        let (Some(Place::Reg(x)), Some(y)) = (operand(a), operand(b)) else { return };
        if result.is_some_and(|r| operand(r).is_none()) {
            return;
        }
        // Computed operands must have a place, including constants made outside any block.
        // Copies and phis emit nothing; their selected source was checked above. A phi's
        // other inputs belong to paths this exit never takes and need no place here.
        if !body.iter().all(|&(v, _)| match &g.values[v as usize].inst {
            Inst::Copy(_) | Inst::Phi(_) => true,
            inst => inst.inputs().into_iter().all(|x| operand(x).is_some()),
        }) {
            return;
        }
        let slow = self.a.new_label();
        for &v in &entry {
            self.fast_value(v, &place);
        }
        let w = ty != Ty::I;
        match y {
            Place::Reg(r) => self.a.alu_rr(w, Alu::Cmp, x, r),
            Place::Imm(c) => self.a.alu_ri(w, Alu::Cmp, x, c as i32),
        }
        self.a.jcc(slow_on, slow);
        for &(v, _) in &tail {
            self.fast_value(v, &place);
        }
        match result.map(|r| (g.ty(r), operand(r).expect("placed"))) {
            Some((Ty::I, Place::Reg(r))) => self.a.movsxd(R::Rax, r),
            Some((_, Place::Reg(r))) => self.a.mov_rr(true, R::Rax, r),
            Some((_, Place::Imm(c))) => self.a.mov_ri(R::Rax, c),
            None => {}
        }
        self.a.ret();
        self.a.bind(slow);
    }

    /// Computes non-parameter, non-constant value `v` into its place.
    fn fast_value(&mut self, v: V, place: &std::collections::HashMap<V, Place>) {
        let g = self.g;
        let get = |x: V| place_of(g, place, x).expect("checked before emitting");
        let Some(&Place::Reg(dst)) = place.get(&v) else { return };
        let w = g.ty(v) != Ty::I;
        match g.values[v as usize].inst {
            Inst::Param(_) | Inst::Const(_) | Inst::Copy(_) | Inst::Phi(_) => {}
            Inst::LoadField(o, off, bt) => {
                let Place::Reg(base) = get(o) else { unreachable!("the receiver is in a register") };
                load_typed_to(&mut self.a, dst, bt, mem(base, off as i32));
            }
            Inst::Bin(op, x, y) => {
                let alu = match op {
                    BinOp::Add => Alu::Add,
                    BinOp::Sub => Alu::Sub,
                    BinOp::And => Alu::And,
                    BinOp::Or => Alu::Or,
                    BinOp::Xor => Alu::Xor,
                    _ => unreachable!("`simple` admits these only"),
                };
                self.fast_mov(w, dst, get(x));
                match get(y) {
                    Place::Reg(r) => self.a.alu_rr(w, alu, dst, r),
                    Place::Imm(c) => self.a.alu_ri(w, alu, dst, c as i32),
                }
            }
            _ => unreachable!("`simple` admits these only"),
        }
    }

    fn fast_mov(&mut self, w: bool, dst: R, p: Place) {
        match p {
            Place::Reg(r) if r != dst => self.a.mov_rr(w, dst, r),
            Place::Reg(_) => {}
            Place::Imm(c) => self.a.mov_ri(dst, c),
        }
    }
}

/// Whether the fast exit computes value `v`: parameters, small constants, copies, integer
/// arithmetic that cannot fault, and non-floating fields of the receiver (never null).
fn simple(g: &Graph, v: V) -> bool {
    match g.values[v as usize].inst {
        Inst::Param(_) | Inst::Copy(_) => true,
        Inst::Const(c) => i32::try_from(c).is_ok(),
        Inst::Bin(BinOp::Add | BinOp::Sub | BinOp::And | BinOp::Or | BinOp::Xor, ..) => matches!(g.ty(v), Ty::I | Ty::J),
        Inst::LoadField(o, _, bt) => !matches!(bt, BasicType::Float | BasicType::Double) && g.receiver.is_some_and(|r| g.resolve(r) == origin(g, o)),
        _ => false,
    }
}

/// Where operand `v` is: its place, or an immediate for a constant that fits one.
fn place_of(g: &Graph, place: &std::collections::HashMap<V, Place>, v: V) -> Option<Place> {
    let v = g.resolve(v);
    place.get(&v).copied().or_else(|| g.constant(v).filter(|&c| i32::try_from(c).is_ok()).map(Place::Imm))
}

/// The value `v` copies (through `split.rs`'s copies), or `v`.
fn origin(g: &Graph, mut v: V) -> V {
    v = g.resolve(v);
    while let Inst::Copy(x) = g.values[v as usize].inst {
        v = g.resolve(x);
    }
    v
}
