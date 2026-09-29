//! Bytecode to SSA by abstract interpretation over basic blocks (C1's graph builder): the
//! operand stack and locals hold values; blocks reached from several places get a phi per live
//! slot, created on first entry and completed as the other predecessors are built. Exception
//! handlers are not compiled: an exception leaves compiled code through deoptimization.
//!
//! Callees are inlined into the same graph (`inline.rs`): each method being built is a `Scope`,
//! and an inlined scope's returns jump to a continuation block in its caller.

use super::ir::*;
use crate::classfile::ClassFile;
use crate::jit::analysis::{analyze, insn_len, successors, Analysis};
use crate::method::Method;
use crate::thread::header;
use std::collections::HashMap;
use std::rc::Rc;

/// The method being built: its bytecode, blocks and abstract state.
pub(super) struct Scope {
    pub m: &'static Method,
    pub cf: &'static ClassFile,
    pub code: &'static [u8],
    an: Analysis,
    /// The block starting at each leader.
    blocks: HashMap<usize, B>,
    /// Predecessor edges each block will get (from the bytecode).
    npreds: HashMap<B, usize>,
    /// Entry state of each block reached so far.
    entry: HashMap<B, (Vec<Option<V>>, Vec<Option<V>>)>,
    /// The phi of each slot (locals, then stack) of merge blocks.
    phis: HashMap<B, Vec<Option<V>>>,
    /// Blocks with an entry state, not built yet.
    ready: Vec<B>,
    pub locals: Vec<Option<V>>,
    pub stack: Vec<Option<V>>,
    /// The bci of the instruction being built.
    pub bci: usize,
    /// For an inlined scope: the caller's state at the call (arguments popped).
    pub caller: Option<Rc<State>>,
    /// Frame slot of local 0 (the arguments' slots in the caller's operand stack).
    pub frame: i32,
    /// For an inlined scope: the continuation block and the (block, value) of each return.
    pub cont: Option<B>,
    pub returns: Vec<(B, Option<V>)>,
}

impl Scope {
    pub fn new(m: &'static Method, caller: Option<Rc<State>>, frame: i32) -> Option<Scope> {
        let info = m.code?;
        Some(Scope {
            m,
            cf: m.holder.cf.as_ref()?,
            code: info.code,
            an: analyze(m)?,
            blocks: HashMap::new(),
            npreds: HashMap::new(),
            entry: HashMap::new(),
            phis: HashMap::new(),
            ready: Vec::new(),
            locals: vec![None; info.max_locals as usize],
            stack: Vec::new(),
            bci: 0,
            caller,
            frame,
            cont: None,
            returns: Vec::new(),
        })
    }

    /// The frame slot after this scope's frame.
    pub fn frame_end(&self) -> i32 {
        let info = self.m.code.unwrap();
        self.frame + info.max_locals as i32 + header::SLOTS as i32 + info.max_stack as i32 + 2
    }
}

pub(super) struct Builder {
    pub g: Graph,
    pub sc: Scope,
    /// The block being built.
    pub cur: B,
    /// Value-stack slots the frame needs, inlined frames included.
    pub extent: i32,
    /// Bytecode inlined so far.
    pub spent: usize,
    /// The scopes being built, outermost first, with their first argument when constant.
    pub chain: Vec<(&'static Method, Option<i64>)>,
    /// Code order of the next block to build.
    seq: u32,
}

/// Builds the graph of `m`; `None` if some instruction is not supported. Returns the frame size.
pub(super) fn build(m: &'static Method) -> Option<(Graph, i32)> {
    let sc = Scope::new(m, None, 0)?;
    let mut b = Builder { g: Graph::default(), extent: sc.frame_end(), sc, cur: 0, spent: 0, chain: vec![(m, None)], seq: 0 };
    // The entry block reads the parameters, then enters bci 0 like any other edge.
    let start = b.g.new_block(-1);
    b.start_block(start);
    let mut slot = 0u16;
    if !m.is_static() {
        b.sc.locals[0] = Some(b.g.add(start, Inst::Param(0), Ty::A));
        slot = 1;
    }
    for p in m.sig.params.iter() {
        let ty = Ty::of(*p);
        b.sc.locals[slot as usize] = Some(b.g.add(start, Inst::Param(slot), ty));
        slot += p.slots() as u16;
    }
    b.enter_scope();
    b.build_scope()?;
    Some((b.g, b.extent))
}

impl Builder {
    /// Makes `block` the current one, next in code order.
    pub fn start_block(&mut self, block: B) {
        self.cur = block;
        self.g.blocks[block as usize].order = self.seq;
        self.seq += 1;
    }

    /// Creates the scope's blocks and enters its bci 0 from the current block, whose state
    /// (`sc.locals`) holds the arguments.
    pub fn enter_scope(&mut self) {
        self.make_blocks();
        self.goto(0);
    }

    /// Builds the scope's blocks as their first predecessor reaches them (lowest bytecode index
    /// first).
    pub fn build_scope(&mut self) -> Option<()> {
        while !self.sc.ready.is_empty() {
            let ready = &self.sc.ready;
            let i = (0..ready.len()).min_by_key(|&i| self.g.blocks[ready[i] as usize].bci).unwrap();
            let block = self.sc.ready.swap_remove(i);
            let (locals, stack) = self.sc.entry[&block].clone();
            self.start_block(block);
            self.sc.locals = locals;
            self.sc.stack = stack;
            self.build_block(self.g.blocks[block as usize].bci as usize)?;
        }
        Some(())
    }

    /// One block per reachable leader (exception handlers excluded), with its predecessor count.
    fn make_blocks(&mut self) {
        let sc = &mut self.sc;
        let handlers: Vec<usize> = sc.m.code.unwrap().handlers.iter().map(|h| h.handler as usize).collect();
        let mut pc = 0;
        while pc < sc.code.len() {
            if sc.an.depth[pc].is_some() && sc.an.leader[pc] && !handlers.contains(&pc) {
                let b = self.g.new_block(pc as i32);
                sc.blocks.insert(pc, b);
            }
            pc += insn_len(sc.code, pc);
        }
        // Predecessors: the entry edge into bci 0, block ends, and fall-through into leaders.
        *sc.npreds.entry(sc.blocks[&0]).or_default() += 1;
        let mut pc = 0;
        while pc < sc.code.len() {
            let len = insn_len(sc.code, pc);
            if sc.an.depth[pc].is_some() {
                let succ = successors(sc.code, pc, len);
                let ends_block = succ.len() != 1 || succ[0] != pc + len || sc.blocks.contains_key(&(pc + len));
                if ends_block {
                    for s in succ {
                        if let Some(&t) = sc.blocks.get(&s) {
                            *sc.npreds.entry(t).or_default() += 1;
                            if s <= pc {
                                self.g.blocks[t as usize].loop_header = true;
                            }
                        }
                    }
                }
            }
            pc += len;
        }
    }

    /// Builds instructions from `pc` until the block ends.
    fn build_block(&mut self, mut pc: usize) -> Option<()> {
        loop {
            self.sc.bci = pc;
            let len = insn_len(self.sc.code, pc);
            if !self.build_insn(pc)? {
                return Some(());
            }
            pc += len;
            if self.sc.blocks.contains_key(&pc) {
                self.goto(pc);
                return Some(());
            }
        }
    }

    /// Ends the current block with a jump to the block at `bci`.
    pub fn goto(&mut self, bci: usize) {
        let to = self.sc.blocks[&bci];
        self.edge(to);
        self.g.blocks[self.cur as usize].term = Term::Goto(to);
    }

    /// Records the edge from the current block to `to`, passing the current state.
    pub fn edge(&mut self, to: B) {
        let from = self.cur;
        self.g.blocks[to as usize].preds.push(from);
        let slots: Vec<Option<V>> = self.sc.locals.iter().chain(self.sc.stack.iter()).copied().collect();
        if let Some(phis) = self.sc.phis.get(&to).cloned() {
            for (i, phi) in phis.iter().enumerate() {
                let Some(p) = *phi else { continue };
                let ty = self.g.ty(p);
                // A slot dead on this path (or reused for another type) is dead after the merge.
                let input = match slots.get(i).copied().flatten() {
                    Some(v) if self.g.ty(v) == ty => v,
                    _ => self.g.add(from, Inst::Const(0), ty),
                };
                if let Inst::Phi(inputs) = &mut self.g.values[p as usize].inst {
                    inputs.push(input);
                }
            }
            return;
        }
        assert!(!self.sc.entry.contains_key(&to), "a single-predecessor block entered twice");
        self.sc.ready.push(to);
        let merge = self.sc.npreds.get(&to).copied().unwrap_or(0) > 1 || self.g.blocks[to as usize].loop_header;
        if !merge {
            self.sc.entry.insert(to, (self.sc.locals.clone(), self.sc.stack.clone()));
            return;
        }
        let phis: Vec<Option<V>> = slots.iter().map(|s| s.map(|v| self.g.add(to, Inst::Phi(vec![v]), self.g.ty(v)))).collect();
        let (l, s) = phis.split_at(self.sc.locals.len());
        self.sc.entry.insert(to, (l.to_vec(), s.to_vec()));
        self.sc.phis.insert(to, phis);
    }

    /// The block starting at `bci`.
    pub fn block_at(&self, bci: usize) -> B {
        self.sc.blocks[&bci]
    }

    /// The interpreter state before the current instruction.
    pub fn state(&self) -> Rc<State> {
        let sc = &self.sc;
        Rc::new(State { method: sc.m, bci: sc.bci as u32, locals: sc.locals.clone(), stack: sc.stack.clone(), caller: sc.caller.clone(), frame: sc.frame })
    }

    pub fn add(&mut self, inst: Inst, ty: Ty) -> V {
        self.g.add(self.cur, inst, ty)
    }

    pub fn konst(&mut self, ty: Ty, bits: i64) -> V {
        self.add(Inst::Const(bits), ty)
    }

    pub fn push(&mut self, v: V) {
        let wide = self.g.ty(v).wide();
        self.sc.stack.push(Some(v));
        if wide {
            self.sc.stack.push(None);
        }
    }

    pub fn pop(&mut self) -> V {
        match self.sc.stack.pop().expect("operand stack underflow") {
            Some(v) => v,
            None => self.sc.stack.pop().flatten().expect("the value half of a wide slot"),
        }
    }

    pub fn load_local(&mut self, n: usize) {
        let v = self.sc.locals[n].expect("verified bytecode reads an assigned local");
        self.push(v);
    }

    pub fn store_local(&mut self, n: usize) {
        let v = self.pop();
        let locals = &mut self.sc.locals;
        locals[n] = Some(v);
        if self.g.ty(v).wide() {
            locals[n + 1] = None;
        }
        // A wide value starting at n - 1 is overwritten in its second half.
        if n > 0 && locals[n - 1].is_some_and(|p| self.g.ty(p).wide()) {
            locals[n - 1] = None;
        }
    }

    /// Ends the block by leaving compiled code at the current instruction.
    pub fn deopt(&mut self) {
        let s = self.state();
        self.g.blocks[self.cur as usize].term = Term::Deopt(s);
    }
}
