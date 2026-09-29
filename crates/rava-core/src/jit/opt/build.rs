//! Bytecode to SSA by abstract interpretation over basic blocks (C1's graph builder): the
//! operand stack and locals hold values; blocks reached from several places get a phi per live
//! slot, created on first entry and completed as the other predecessors are built. Exception
//! handlers are not compiled: an exception leaves compiled code through deoptimization.

use super::ir::*;
use crate::classfile::ClassFile;
use crate::jit::analysis::{analyze, insn_len, successors, Analysis};
use crate::method::Method;
use std::collections::HashMap;
use std::rc::Rc;

pub(super) struct Builder {
    pub g: Graph,
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
    /// The block being built and its current state.
    pub cur: B,
    pub locals: Vec<Option<V>>,
    pub stack: Vec<Option<V>>,
    /// The bci of the instruction being built.
    pub bci: usize,
}

/// Builds the graph of `m`; `None` if some instruction is not supported.
pub(super) fn build(m: &'static Method) -> Option<Graph> {
    let info = m.code?;
    let an = analyze(m)?;
    let mut b = Builder {
        g: Graph::default(),
        m,
        cf: m.holder.cf.as_ref()?,
        code: info.code,
        an,
        blocks: HashMap::new(),
        npreds: HashMap::new(),
        entry: HashMap::new(),
        phis: HashMap::new(),
        ready: Vec::new(),
        cur: 0,
        locals: vec![None; info.max_locals as usize],
        stack: Vec::new(),
        bci: 0,
    };
    b.make_blocks();
    // The entry block reads the parameters, then enters bci 0 like any other edge.
    let start = b.g.new_block(-1);
    b.cur = start;
    let mut slot = 0u16;
    if !m.is_static() {
        b.locals[0] = Some(b.g.add(start, Inst::Param(0), Ty::A));
        slot = 1;
    }
    for p in m.sig.params.iter() {
        let ty = Ty::of(*p);
        b.locals[slot as usize] = Some(b.g.add(start, Inst::Param(slot), ty));
        slot += p.slots() as u16;
    }
    b.goto(0);
    // Build blocks as their first predecessor reaches them (lowest bytecode index first).
    while !b.ready.is_empty() {
        let i = (0..b.ready.len()).min_by_key(|&i| b.g.blocks[b.ready[i] as usize].bci).unwrap();
        let block = b.ready.swap_remove(i);
        let (locals, stack) = b.entry[&block].clone();
        b.cur = block;
        b.locals = locals;
        b.stack = stack;
        b.build_block(b.g.blocks[block as usize].bci as usize)?;
    }
    Some(b.g)
}

impl Builder {
    /// One block per reachable leader (exception handlers excluded), with its predecessor count.
    fn make_blocks(&mut self) {
        let handlers: Vec<usize> = self.m.code.unwrap().handlers.iter().map(|h| h.handler as usize).collect();
        let mut pc = 0;
        while pc < self.code.len() {
            if self.an.depth[pc].is_some() && self.an.leader[pc] && !handlers.contains(&pc) {
                let b = self.g.new_block(pc as i32);
                self.blocks.insert(pc, b);
            }
            pc += insn_len(self.code, pc);
        }
        // Predecessors: the entry edge into bci 0, block ends, and fall-through into leaders.
        *self.npreds.entry(self.blocks[&0]).or_default() += 1;
        let mut pc = 0;
        while pc < self.code.len() {
            let len = insn_len(self.code, pc);
            if self.an.depth[pc].is_some() {
                let succ = successors(self.code, pc, len);
                let ends_block = succ.len() != 1 || succ[0] != pc + len || self.blocks.contains_key(&(pc + len));
                if ends_block {
                    for s in succ {
                        if let Some(&t) = self.blocks.get(&s) {
                            *self.npreds.entry(t).or_default() += 1;
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
            self.bci = pc;
            let len = insn_len(self.code, pc);
            if !self.build_insn(pc)? {
                return Some(());
            }
            pc += len;
            if self.blocks.contains_key(&pc) {
                self.goto(pc);
                return Some(());
            }
        }
    }

    /// Ends the current block with a jump to the block at `bci`.
    pub fn goto(&mut self, bci: usize) {
        let to = self.blocks[&bci];
        self.edge(to);
        self.g.blocks[self.cur as usize].term = Term::Goto(to);
    }

    /// Records the edge from the current block to `to`, passing the current state.
    pub fn edge(&mut self, to: B) {
        let from = self.cur;
        self.g.blocks[to as usize].preds.push(from);
        let slots: Vec<Option<V>> = self.locals.iter().chain(self.stack.iter()).copied().collect();
        if let Some(phis) = self.phis.get(&to).cloned() {
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
        if self.entry.contains_key(&to) {
            unreachable!("a single-predecessor block entered twice");
        }
        self.ready.push(to);
        let merge = self.npreds.get(&to).copied().unwrap_or(0) > 1 || self.g.blocks[to as usize].loop_header;
        if !merge {
            self.entry.insert(to, (self.locals.clone(), self.stack.clone()));
            return;
        }
        let phis: Vec<Option<V>> = slots.iter().map(|s| s.map(|v| self.g.add(to, Inst::Phi(vec![v]), self.g.ty(v)))).collect();
        let (l, s) = phis.split_at(self.locals.len());
        self.entry.insert(to, (l.to_vec(), s.to_vec()));
        self.phis.insert(to, phis);
    }

    /// The block starting at `bci`.
    pub fn block_at(&self, bci: usize) -> B {
        self.blocks[&bci]
    }

    /// The interpreter state before the current instruction.
    pub fn state(&self) -> Rc<State> {
        Rc::new(State { method: self.m, bci: self.bci as u32, locals: self.locals.clone(), stack: self.stack.clone(), caller: None, frame: 0 })
    }

    pub fn add(&mut self, inst: Inst, ty: Ty) -> V {
        self.g.add(self.cur, inst, ty)
    }

    pub fn konst(&mut self, ty: Ty, bits: i64) -> V {
        self.add(Inst::Const(bits), ty)
    }

    pub fn push(&mut self, v: V) {
        let wide = self.g.ty(v).wide();
        self.stack.push(Some(v));
        if wide {
            self.stack.push(None);
        }
    }

    pub fn pop(&mut self) -> V {
        match self.stack.pop().expect("operand stack underflow") {
            Some(v) => v,
            None => self.stack.pop().flatten().expect("the value half of a wide slot"),
        }
    }

    pub fn load_local(&mut self, n: usize) {
        let v = self.locals[n].expect("verified bytecode reads an assigned local");
        self.push(v);
    }

    pub fn store_local(&mut self, n: usize) {
        let v = self.pop();
        self.locals[n] = Some(v);
        if self.g.ty(v).wide() {
            self.locals[n + 1] = None;
        }
        // A wide value starting at n - 1 is overwritten in its second half.
        if n > 0 && self.locals[n - 1].is_some_and(|p| self.g.ty(p).wide()) {
            self.locals[n - 1] = None;
        }
    }

    /// Ends the block by leaving compiled code at the current instruction.
    pub fn deopt(&mut self) {
        let s = self.state();
        self.g.blocks[self.cur as usize].term = Term::Deopt(s);
    }
}
