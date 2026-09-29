//! The optimizing tier's intermediate representation: a control-flow graph of basic blocks over
//! SSA values. Every value is one 64-bit slot (int-like values sign-extended, float bits in the
//! low half, long/double bits, references as addresses), so one register class serves all.
//!
//! Instructions that may leave compiled code carry the interpreter `State` to deoptimize to:
//! the values of every local and operand-stack slot of each scope (the method and the callers
//! inlined around it).

use crate::jit::intrinsics::Intrinsic;
use crate::klass::Klass;
use crate::method::Method;
use crate::types::BasicType;
use std::rc::Rc;

/// A value (index into `Graph::values`).
pub type V = u32;
/// A block (index into `Graph::blocks`).
pub type B = u32;

/// Value kinds (int-like = `I`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    I,
    J,
    F,
    D,
    A,
}

impl Ty {
    pub fn of(bt: BasicType) -> Ty {
        match bt {
            BasicType::Long => Ty::J,
            BasicType::Float => Ty::F,
            BasicType::Double => Ty::D,
            BasicType::Reference => Ty::A,
            _ => Ty::I,
        }
    }

    pub fn wide(self) -> bool {
        matches!(self, Ty::J | Ty::D)
    }
}

/// The interpreter state of one scope at `bci`: slot values (`None` for dead slots and the
/// second half of long/double values), and the caller's state for an inlined scope.
#[derive(Clone, Debug)]
pub struct State {
    pub method: &'static Method,
    pub bci: u32,
    pub locals: Vec<Option<V>>,
    pub stack: Vec<Option<V>>,
    /// The caller at the call, with the arguments already popped.
    pub caller: Option<Rc<State>>,
    /// Frame slot (from the physical frame base) of this scope's local 0.
    pub frame: i32,
}

/// Arithmetic on two values of the result's type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Ushr,
}

/// A call's target.
#[derive(Clone, Copy, Debug)]
pub enum Callee {
    /// A statically bound method.
    Direct(&'static Method),
    /// Dispatched on the receiver (first argument) through the vtable.
    Virtual(&'static Method),
    /// Dispatched on the receiver through the class's itable (or default methods).
    Interface(&'static Method),
    /// The `Method*` a value holds (method-handle plumbing).
    Method(V),
    /// The implementation of the `Method*` a value holds for the receiver's class
    /// (`linkToVirtual` / `linkToInterface`).
    Select(V),
}

impl Callee {
    /// The value naming the target, for computed targets.
    pub fn input(&self) -> Option<V> {
        match *self {
            Callee::Method(v) | Callee::Select(v) => Some(v),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Inst {
    /// The method's local `slot` on entry (read from the frame).
    Param(u16),
    Const(i64),
    /// Block parameter: one input per predecessor, in `Block::preds` order.
    Phi(Vec<V>),
    Neg(V),
    Bin(BinOp, V, V),
    /// A conversion bytecode (i2l ... i2s) applied to a value.
    Convert(u8, V),
    /// lcmp / fcmpl / fcmpg / dcmpl / dcmpg.
    Compare(u8, V, V),
    NullCheck(V, Rc<State>),
    /// Integer division by zero check.
    ZeroCheck(V, Rc<State>),
    BoundsCheck(V, V, Rc<State>),
    InitCheck(&'static Klass, Rc<State>),
    LoadField(V, u32, BasicType),
    StoreField(V, u32, BasicType, V),
    LoadStatic(usize, BasicType),
    StoreStatic(usize, BasicType, V),
    ArrayLength(V),
    LoadIndexed(V, V, BasicType),
    StoreIndexed(V, V, BasicType, V),
    Call(Callee, Vec<V>, Rc<State>),
    New(&'static Klass, Rc<State>),
    NewArray(&'static Klass, V, Rc<State>),
    /// The value itself, once it is null or a `Klass`.
    CheckCast(V, &'static Klass, Rc<State>),
    InstanceOf(V, &'static Klass),
    /// `aastore`'s check that the value fits the array's element type.
    StoreCheck(V, V, Rc<State>),
    /// `monitorenter` (true) or `monitorexit` of a non-null object.
    Monitor(bool, V, Rc<State>),
    /// A JDK method compiled in place, with the state to deoptimize to (at the call) when it
    /// has cases it does not handle.
    Intrinsic(Intrinsic, Vec<V>, Option<Rc<State>>),
}

impl Inst {
    /// The state a failure of this instruction deoptimizes to.
    pub fn state(&self) -> Option<&Rc<State>> {
        match self {
            Inst::NullCheck(_, s) | Inst::ZeroCheck(_, s) | Inst::BoundsCheck(_, _, s) | Inst::InitCheck(_, s) => Some(s),
            Inst::Call(_, _, s) | Inst::New(_, s) | Inst::NewArray(_, _, s) => Some(s),
            Inst::CheckCast(_, _, s) | Inst::StoreCheck(_, _, s) | Inst::Intrinsic(_, _, Some(s)) | Inst::Monitor(_, _, s) => Some(s),
            _ => None,
        }
    }

    /// The values this instruction reads (not counting its state).
    pub fn inputs(&self) -> Vec<V> {
        match self {
            Inst::Param(_) | Inst::Const(_) | Inst::LoadStatic(..) | Inst::InitCheck(..) | Inst::New(..) => vec![],
            Inst::Call(c, v, _) => v.iter().copied().chain(c.input()).collect(),
            Inst::Phi(v) | Inst::Intrinsic(_, v, _) => v.clone(),
            Inst::Neg(a) | Inst::Convert(_, a) | Inst::NullCheck(a, _) | Inst::ZeroCheck(a, _) | Inst::ArrayLength(a) => vec![*a],
            Inst::LoadField(a, ..) | Inst::StoreStatic(_, _, a) | Inst::NewArray(_, a, _) => vec![*a],
            Inst::CheckCast(a, ..) | Inst::InstanceOf(a, _) | Inst::Monitor(_, a, _) => vec![*a],
            Inst::Bin(_, a, b) | Inst::Compare(_, a, b) | Inst::BoundsCheck(a, b, _) | Inst::LoadIndexed(a, b, _) => vec![*a, *b],
            Inst::StoreCheck(a, b, _) => vec![*a, *b],
            Inst::StoreField(a, _, _, b) => vec![*a, *b],
            Inst::StoreIndexed(a, b, _, c) => vec![*a, *b, *c],
        }
    }
}

/// Integer/reference comparison of a conditional branch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Ge,
    Gt,
    Le,
}

#[derive(Clone, Debug)]
pub enum Term {
    Goto(B),
    /// `a cmp b` on values of type `ty` (I, J or A).
    If { cmp: Cmp, ty: Ty, a: V, b: V, then: B, other: B },
    Switch { key: V, cases: Vec<(i32, B)>, default: B },
    Return(Option<V>),
    /// Leave compiled code: continue in the interpreter at the state.
    Deopt(Rc<State>),
    /// Not yet built.
    None,
}

impl Term {
    pub fn successors(&self) -> Vec<B> {
        match self {
            Term::Goto(b) => vec![*b],
            Term::If { then, other, .. } => vec![*then, *other],
            Term::Switch { cases, default, .. } => cases.iter().map(|c| c.1).chain(std::iter::once(*default)).collect(),
            _ => vec![],
        }
    }

    pub fn inputs(&self) -> Vec<V> {
        match self {
            Term::If { a, b, .. } => vec![*a, *b],
            Term::Switch { key, .. } => vec![*key],
            Term::Return(Some(v)) => vec![*v],
            _ => vec![],
        }
    }
}

#[derive(Debug)]
pub struct Block {
    /// Values defined here, in order (phis first).
    pub insts: Vec<V>,
    pub term: Term,
    pub preds: Vec<B>,
    /// The first bytecode index of the block in its method (-1 for none).
    pub bci: i32,
    /// Loop header: some predecessor comes later in bytecode order.
    pub loop_header: bool,
    /// Position in code order (blocks are laid out in the order they are built).
    pub order: u32,
}

pub struct Value {
    pub inst: Inst,
    pub ty: Ty,
}

#[derive(Default)]
pub struct Graph {
    pub values: Vec<Value>,
    pub blocks: Vec<Block>,
}

impl Graph {
    pub fn new_block(&mut self, bci: i32) -> B {
        self.blocks.push(Block { insts: Vec::new(), term: Term::None, preds: Vec::new(), bci, loop_header: false, order: u32::MAX });
        self.blocks.len() as B - 1
    }

    /// Appends `inst` of type `ty` to block `b`.
    pub fn add(&mut self, b: B, inst: Inst, ty: Ty) -> V {
        self.values.push(Value { inst, ty });
        let v = self.values.len() as V - 1;
        self.blocks[b as usize].insts.push(v);
        v
    }

    pub fn ty(&self, v: V) -> Ty {
        self.values[v as usize].ty
    }

    pub fn inst(&self, v: V) -> &Inst {
        &self.values[v as usize].inst
    }

    /// The constant a value is, if it is one.
    pub fn constant(&self, v: V) -> Option<i64> {
        match self.values[v as usize].inst {
            Inst::Const(c) => Some(c),
            _ => None,
        }
    }
}

impl Graph {
    /// A readable listing (`RAVA_CORE_OPT_IR`).
    pub fn dump(&self) -> String {
        let mut out = String::new();
        let mut order: Vec<usize> = (0..self.blocks.len()).filter(|&b| !matches!(self.blocks[b].term, Term::None)).collect();
        order.sort_by_key(|&b| self.blocks[b].order);
        for b in order {
            let blk = &self.blocks[b];
            out += &format!("B{b} (bci {}, preds {:?}{})\n", blk.bci, blk.preds, if blk.loop_header { ", loop" } else { "" });
            for &v in &blk.insts {
                let inst = match &self.values[v as usize].inst {
                    Inst::Call(c, args, s) => format!("Call({c:?}, {args:?}) @{}", s.bci),
                    i @ (Inst::NullCheck(..) | Inst::ZeroCheck(..) | Inst::BoundsCheck(..) | Inst::InitCheck(..) | Inst::CheckCast(..) | Inst::StoreCheck(..) | Inst::New(..) | Inst::NewArray(..) | Inst::Monitor(..)) => {
                        let s = i.state().unwrap();
                        format!("{} @{}:{}", format!("{i:?}").split(", State").next().unwrap_or(""), s.method.name, s.bci)
                    }
                    i => format!("{i:?}"),
                };
                out += &format!("  v{v}: {:?} = {inst}\n", self.values[v as usize].ty);
            }
            let term = match &blk.term {
                Term::Deopt(s) => format!("Deopt @{}:{}", s.method.name, s.bci),
                t => format!("{t:?}"),
            };
            out += &format!("  {term}\n");
        }
        out
    }
}
