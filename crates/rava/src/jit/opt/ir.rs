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
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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
    /// The same value under another name, so it can live elsewhere (`split.rs`).
    Copy(V),
    /// The StoreLoad barrier after a volatile store (JLS 17.4); the builder also keeps no
    /// memory value across it.
    Fence,
    /// Block parameter: one input per predecessor, in `Block::preds` order.
    Phi(Vec<V>),
    Neg(V),
    Bin(BinOp, V, V),
    /// A conversion bytecode (i2l ... i2s) applied to a value.
    Convert(u8, V),
    /// lcmp / fcmpl / fcmpg / dcmpl / dcmpg.
    Compare(u8, V, V),
    NullCheck(V, Rc<State>),
    /// Deoptimizes once the byte at this address is non-zero (a method a loaded class now
    /// overrides, for code that inlined it unguarded).
    FlagCheck(usize, Rc<State>),
    /// Integer division by zero check.
    ZeroCheck(V, Rc<State>),
    /// A counted loop's bounds-check predicate (`rce.rs`), `(array, init, limit)`: when the loop
    /// runs (`init < limit`), deoptimizes unless the array is non-null, `init >= 0` and
    /// `limit <= length`.
    RangeGuard(V, V, V, Rc<State>),
    /// A loop-invariant test hoisted out of a loop (`guards.rs`): deoptimizes unless
    /// `a cmp b` (of type ty) holds.
    Guard(Cmp, Ty, V, V, Rc<State>),
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
    /// A two-dimensional `multianewarray`: the counts packed into a long, the first in the
    /// low half.
    NewMulti(&'static Klass, V, Rc<State>),
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
    /// `a cmp b ? t : f` with `a` and `b` of type `ty` (I, J or A): a branch if-converted into a
    /// conditional move (`select.rs`).
    Select(Cmp, Ty, V, V, V, V),
}

impl Inst {
    /// Replaces each value this instruction reads (not its state) by `f` of it.
    pub fn map_inputs(&mut self, mut f: impl FnMut(V) -> V) {
        let mut one = |v: &mut V| *v = f(*v);
        match self {
            Inst::Param(_) | Inst::Const(_) | Inst::Fence | Inst::LoadStatic(..) | Inst::InitCheck(..) | Inst::FlagCheck(..) | Inst::New(..) => {}
            Inst::Call(c, v, _) => {
                v.iter_mut().for_each(&mut one);
                if let Callee::Method(t) | Callee::Select(t) = c {
                    one(t);
                }
            }
            Inst::Phi(v) | Inst::Intrinsic(_, v, _) => v.iter_mut().for_each(one),
            Inst::Copy(a) | Inst::Neg(a) | Inst::Convert(_, a) | Inst::NullCheck(a, _) | Inst::ZeroCheck(a, _) | Inst::ArrayLength(a) => one(a),
            Inst::LoadField(a, ..) | Inst::StoreStatic(_, _, a) | Inst::NewArray(_, a, _) | Inst::NewMulti(_, a, _) => one(a),
            Inst::CheckCast(a, ..) | Inst::InstanceOf(a, _) | Inst::Monitor(_, a, _) => one(a),
            Inst::Bin(_, a, b) | Inst::Compare(_, a, b) | Inst::BoundsCheck(a, b, _) | Inst::LoadIndexed(a, b, _) | Inst::StoreCheck(a, b, _) | Inst::StoreField(a, _, _, b) | Inst::Guard(_, _, a, b, _) => {
                one(a);
                one(b);
            }
            Inst::StoreIndexed(a, b, _, c) | Inst::RangeGuard(a, b, c, _) => {
                one(a);
                one(b);
                one(c);
            }
            Inst::Select(_, _, a, b, t, f) => {
                one(a);
                one(b);
                one(t);
                one(f);
            }
        }
    }

    /// Replaces the state this instruction deoptimizes to.
    pub fn set_state(&mut self, new: Rc<State>) {
        match self {
            Inst::NullCheck(_, s) | Inst::ZeroCheck(_, s) | Inst::BoundsCheck(_, _, s) | Inst::InitCheck(_, s) | Inst::FlagCheck(_, s) | Inst::RangeGuard(_, _, _, s) | Inst::Guard(_, _, _, _, s) => *s = new,
            Inst::Call(_, _, s) | Inst::New(_, s) | Inst::NewArray(_, _, s) | Inst::NewMulti(_, _, s) => *s = new,
            Inst::CheckCast(_, _, s) | Inst::StoreCheck(_, _, s) | Inst::Intrinsic(_, _, Some(s)) | Inst::Monitor(_, _, s) => *s = new,
            _ => {}
        }
    }

    /// The state a failure of this instruction deoptimizes to.
    pub fn state(&self) -> Option<&Rc<State>> {
        match self {
            Inst::NullCheck(_, s) | Inst::ZeroCheck(_, s) | Inst::BoundsCheck(_, _, s) | Inst::InitCheck(_, s) | Inst::FlagCheck(_, s) | Inst::RangeGuard(_, _, _, s) | Inst::Guard(_, _, _, _, s) => Some(s),
            Inst::Call(_, _, s) | Inst::New(_, s) | Inst::NewArray(_, _, s) | Inst::NewMulti(_, _, s) => Some(s),
            Inst::CheckCast(_, _, s) | Inst::StoreCheck(_, _, s) | Inst::Intrinsic(_, _, Some(s)) | Inst::Monitor(_, _, s) => Some(s),
            _ => None,
        }
    }

    /// The values this instruction reads (not counting its state).
    pub fn inputs(&self) -> Vec<V> {
        match self {
            Inst::Param(_) | Inst::Const(_) | Inst::Fence | Inst::LoadStatic(..) | Inst::InitCheck(..) | Inst::FlagCheck(..) | Inst::New(..) => vec![],
            Inst::Call(c, v, _) => v.iter().copied().chain(c.input()).collect(),
            Inst::Phi(v) | Inst::Intrinsic(_, v, _) => v.clone(),
            Inst::Copy(a) | Inst::Neg(a) | Inst::Convert(_, a) | Inst::NullCheck(a, _) | Inst::ZeroCheck(a, _) | Inst::ArrayLength(a) => vec![*a],
            Inst::LoadField(a, ..) | Inst::StoreStatic(_, _, a) | Inst::NewArray(_, a, _) | Inst::NewMulti(_, a, _) => vec![*a],
            Inst::CheckCast(a, ..) | Inst::InstanceOf(a, _) | Inst::Monitor(_, a, _) => vec![*a],
            Inst::Bin(_, a, b) | Inst::Compare(_, a, b) | Inst::BoundsCheck(a, b, _) | Inst::LoadIndexed(a, b, _) | Inst::Guard(_, _, a, b, _) => vec![*a, *b],
            Inst::StoreCheck(a, b, _) => vec![*a, *b],
            Inst::StoreField(a, _, _, b) => vec![*a, *b],
            Inst::StoreIndexed(a, b, _, c) | Inst::RangeGuard(a, b, c, _) => vec![*a, *b, *c],
            Inst::Select(_, _, a, b, t, f) => vec![*a, *b, *t, *f],
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
    /// `a cmp b` on values of type `ty` (I, J, F, D or A).
    /// With F or D, `unordered` is the outcome when either value is NaN.
    If { cmp: Cmp, ty: Ty, a: V, b: V, then: B, other: B, unordered: bool },
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

    pub fn map_inputs(&mut self, mut f: impl FnMut(V) -> V) {
        match self {
            Term::If { a, b, .. } => {
                *a = f(*a);
                *b = f(*b);
            }
            Term::Switch { key, .. } => *key = f(*key),
            Term::Return(Some(v)) => *v = f(*v),
            _ => {}
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
    /// Position in build order (which back edges, live ranges and the register allocator
    /// follow).
    pub order: u32,
    /// Estimated executions per entry of the compiled method (from first-tier branch profiles;
    /// a block the builder makes inherits the frequency of the one it started from), for laying
    /// out code so hot paths fall through.
    pub freq: f32,
}

pub struct Value {
    pub inst: Inst,
    pub ty: Ty,
}

/// An object whose allocation was removed because it never escapes the compiled code
/// (`escape.rs`). It survives only in deoptimization states, where it is rebuilt from its
/// field values (rematerialization). `fields` are `(offset, type, value)`, the values still
/// live wherever the object appears in a state.
pub struct Virtual {
    pub klass: &'static Klass,
    pub fields: Vec<(u32, BasicType, V)>,
}

#[derive(Default)]
pub struct Graph {
    pub values: Vec<Value>,
    pub blocks: Vec<Block>,
    /// The value each value stands for (itself, or what a simplified phi equals; `phis.rs`).
    pub alias: Vec<V>,
    /// Field loads with acquire semantics (volatile): they stay where they are (`licm.rs`).
    pub ordered: std::collections::HashSet<V>,
    /// Loads of final instance fields outside constructors: a store to another field at the
    /// same offset (of another class) does not change them (`licm.rs`).
    pub final_loads: std::collections::HashSet<V>,
    /// The receiver of an instance method, never null.
    pub receiver: Option<V>,
    /// Allocations removed as non-escaping, rebuilt on deoptimization (`escape.rs`).
    pub virtuals: std::collections::HashMap<V, Virtual>,
    /// Virtual and interface calls whose site had no receiver profile when compiled: the code
    /// profiles them itself and asks for a recompilation once they turn hot (`codegen_obj.rs`).
    pub unprofiled: std::collections::HashSet<V>,
    /// Calls not inlined because their site looked cold: the code counts them and marks the
    /// site hot (and asks for a recompilation) if it runs often after all.
    pub cold_calls: std::collections::HashSet<V>,
    /// The interpreter state on entering each loop header whose bytecode has not trapped (its
    /// phis as the locals): where predicates hoisted out of the loop deoptimize (`rce.rs`).
    pub loop_entry: std::collections::HashMap<B, std::rc::Rc<State>>,
    /// Casts, instance tests and identity hashes whose object a dominating test proved non-null
    /// (`conds.rs`): their code skips its own null test.
    pub nonnull_input: std::collections::HashSet<V>,
}

impl Graph {
    /// Drops the blocks no path from the entry reaches that end the method (deoptimization
    /// exits whose test became a guard before the loop): their states would keep values live
    /// to their positions in the code order, for nothing. Blocks with successors stay (their
    /// successors' phis count them).
    pub fn prune_unreachable(&mut self) {
        let mut seen = vec![false; self.blocks.len()];
        let mut stack = vec![0 as B];
        while let Some(b) = stack.pop() {
            if std::mem::replace(&mut seen[b as usize], true) {
                continue;
            }
            stack.extend(self.blocks[b as usize].term.successors());
        }
        // Every block the entry does not reach goes, and leaves its successors' predecessors
        // (with the matching phi inputs): a block left jumping to a removed one was laid out
        // once a later pass made it reachable again.
        for b in 0..self.blocks.len() {
            if seen[b] || matches!(self.blocks[b].term, Term::None) {
                continue;
            }
            let term = std::mem::replace(&mut self.blocks[b].term, Term::None);
            self.blocks[b].insts.clear();
            for s in term.successors() {
                while let Some(j) = self.blocks[s as usize].preds.iter().position(|&x| x as usize == b) {
                    self.blocks[s as usize].preds.remove(j);
                    for v in self.blocks[s as usize].insts.clone() {
                        if let Inst::Phi(ins) = &mut self.values[v as usize].inst {
                            ins.remove(j);
                        }
                    }
                }
            }
        }
    }

    /// Merges each block that only its one predecessor jumps to into that predecessor, so
    /// straight-line code is one block (a null check and the loads after it, an inlined body
    /// and its caller's continuation): code generation then sees what follows an instruction.
    pub fn merge_chains(&mut self) {
        for b in 0..self.blocks.len() {
            while let Term::Goto(m) = self.blocks[b].term {
                let mb = &self.blocks[m as usize];
                let real_phi = mb.insts.iter().any(|&v| matches!(self.inst(v), Inst::Phi(_)) && self.resolve(v) == v);
                if m as usize == b || mb.loop_header || mb.preds[..] != [b as B] || matches!(mb.term, Term::None) || real_phi || self.loop_entry.contains_key(&m) {
                    break;
                }
                let insts: Vec<V> = std::mem::take(&mut self.blocks[m as usize].insts).into_iter().filter(|&v| !matches!(self.inst(v), Inst::Phi(_))).collect();
                let term = std::mem::replace(&mut self.blocks[m as usize].term, Term::None);
                self.blocks[m as usize].preds.clear();
                for s in term.successors() {
                    for p in &mut self.blocks[s as usize].preds {
                        if *p == m {
                            *p = b as B;
                        }
                    }
                }
                self.blocks[b].insts.extend(insts);
                self.blocks[b].term = term;
            }
        }
    }

    pub fn new_block(&mut self, bci: i32) -> B {
        self.blocks.push(Block { insts: Vec::new(), term: Term::None, preds: Vec::new(), bci, loop_header: false, order: u32::MAX, freq: 0.0 });
        self.blocks.len() as B - 1
    }

    /// Appends `inst` of type `ty` to block `b`. Once aliases exist (`phis.rs`), every value up
    /// to the new one gets an entry (itself) if it has none yet.
    pub fn add(&mut self, b: B, inst: Inst, ty: Ty) -> V {
        self.values.push(Value { inst, ty });
        let v = self.values.len() as V - 1;
        self.blocks[b as usize].insts.push(v);
        if !self.alias.is_empty() {
            let n = self.alias.len() as V;
            self.alias.extend(n..=v);
        }
        v
    }

    /// The value `v` stands for.
    /// The constant `v` holds, also through the copies that keep a wide constant in a register
    /// (`reload.rs`): for code that only needs to know the value, not where it is.
    pub fn constant_value(&self, v: V) -> Option<i64> {
        let mut v = self.resolve(v);
        while let Inst::Copy(x) = self.values[v as usize].inst {
            v = self.resolve(x);
        }
        self.constant(v)
    }

    /// The edges agree with the predecessor lists: every successor of a live block is live and
    /// lists it, and every predecessor is live and jumps there.
    pub fn verify(&self) -> Result<(), String> {
        let live = |b: B| !matches!(self.blocks[b as usize].term, Term::None);
        for (b, blk) in self.blocks.iter().enumerate().filter(|(b, _)| live(*b as B)) {
            let succ = blk.term.successors();
            for &s in &succ {
                if !live(s) {
                    return Err(format!("B{b} jumps to removed B{s}"));
                }
                // A switch's cases into one block are one predecessor entry.
                if !self.blocks[s as usize].preds.contains(&(b as B)) {
                    return Err(format!("B{b} jumps to B{s}, which does not list it"));
                }
            }
            for &p in &blk.preds {
                if !live(p) || !self.blocks[p as usize].term.successors().contains(&(b as B)) {
                    return Err(format!("B{b} lists B{p}, which does not jump there"));
                }
            }
        }
        Ok(())
    }

    pub fn resolve(&self, mut v: V) -> V {
        while let Some(&a) = self.alias.get(v as usize).filter(|&&a| a != v) {
            v = a;
        }
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
    /// A readable listing (`RAVA_OPT_IR`).
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
                    i @ (Inst::NullCheck(..) | Inst::FlagCheck(..) | Inst::ZeroCheck(..) | Inst::BoundsCheck(..) | Inst::RangeGuard(..) | Inst::Guard(..) | Inst::InitCheck(..) | Inst::CheckCast(..) | Inst::StoreCheck(..) | Inst::New(..) | Inst::NewArray(..) | Inst::NewMulti(..) | Inst::Monitor(..)) => {
                        let s = i.state().unwrap();
                        format!("{} @{}:{}{}", format!("{i:?}").split(", State").next().unwrap_or(""), s.method.name, s.bci, state_values(s))
                    }
                    i => format!("{i:?}"),
                };
                out += &format!("  v{v}: {:?} = {inst}\n", self.values[v as usize].ty);
            }
            let term = match &blk.term {
                Term::Deopt(s) => format!("Deopt @{}:{}{}", s.method.name, s.bci, state_values(s)),
                t => format!("{t:?}"),
            };
            out += &format!("  {term}\n");
        }
        if std::env::var_os("RAVA_OPT_IR_STATES").is_some() {
            let aliases: Vec<String> = (0..self.values.len() as V).filter(|&v| self.resolve(v) != v).map(|v| format!("v{v}->v{}", self.resolve(v))).collect();
            out += &format!("aliases {}\n", aliases.join(" "));
            let mut virtuals: Vec<String> = self.virtuals.iter().map(|(v, x)| format!("v{v}={}{:?}", x.klass.name, x.fields.iter().map(|&(o, _, f)| (o, f)).collect::<Vec<_>>())).collect();
            virtuals.sort();
            out += &format!("virtuals {}\n", virtuals.join(" "));
        }
        out
    }
}

/// With `RAVA_OPT_IR_STATES`, a state's values per scope (locals | stack) for the IR dump.
fn state_values(s: &State) -> String {
    if std::env::var_os("RAVA_OPT_IR_STATES").is_none() {
        return String::new();
    }
    let show = |xs: &[Option<V>]| xs.iter().map(|x| x.map_or("_".into(), |v| format!("v{v}"))).collect::<Vec<_>>().join(",");
    let mut out = String::new();
    let mut sc = Some(s);
    while let Some(s) = sc {
        out += &format!(" [{}@{} {} | {}]", s.method.name, s.bci, show(&s.locals), show(&s.stack));
        sc = s.caller.as_deref();
    }
    out
}
