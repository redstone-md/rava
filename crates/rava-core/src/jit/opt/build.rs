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
    /// Instructions from which every path throws (`analysis::throw_paths`).
    throws: Vec<bool>,
    /// Conditional branches with a direction left to the interpreter (true: the jump), decided
    /// once per scope as its blocks are made, since profiles change while it compiles.
    pub pruned: HashMap<usize, bool>,
    /// The block starting at each leader.
    blocks: HashMap<usize, B>,
    /// Predecessor edges each block will get (from the bytecode).
    npreds: HashMap<B, usize>,
    /// Of those, the forward ones (not a loop's back edge), and how many have been built.
    fwd: HashMap<B, (usize, usize)>,
    /// The edge into bci 0 has been made (later edges are the scope's own).
    entered: bool,
    /// Entry state of each block reached so far: locals, stack and the values known non-null.
    entry: HashMap<B, (Vec<Option<V>>, Vec<Option<V>>, Vec<V>)>,
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
    /// Estimated executions of each instruction per entry of this scope (`freq.rs`), and of
    /// this scope per entry of the compiled method.
    freq: Vec<f32>,
    pub weight: f32,
    /// Locals the interpreter may still read at each instruction (`locals.rs`).
    live: super::locals::LiveLocals,
}

impl Scope {
    pub fn new(m: &'static Method, caller: Option<Rc<State>>, frame: i32) -> Option<Scope> {
        let info = m.code?;
        let an = analyze(m)?;
        let freq = super::freq::frequencies(m, info.code, |pc| an.depth[pc].is_some());
        Some(Scope {
            m,
            cf: m.holder.cf.as_ref()?,
            code: info.code,
            an,
            throws: crate::jit::analysis::throw_paths(info.code),
            pruned: HashMap::new(),
            blocks: HashMap::new(),
            npreds: HashMap::new(),
            fwd: HashMap::new(),
            entered: false,
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
            freq,
            weight: 1.0,
            live: super::locals::LiveLocals::compute(info),
        })
    }

    /// Estimated executions of the instruction at `pc` per entry of the compiled method.
    pub fn frequency(&self, pc: usize) -> f32 {
        self.weight * self.freq.get(pc).copied().unwrap_or(0.0)
    }

    /// The frame slot after this scope's frame.
    pub fn frame_end(&self) -> i32 {
        let info = self.m.code.unwrap();
        self.frame + info.max_locals as i32 + header::SLOTS as i32 + info.max_stack as i32 + 2
    }
}

/// Executions a branch needs in baseline code before a direction it never took is trusted
/// to stay untaken.
const MIN_BRANCH_PROFILE: u32 = 64;

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
    /// Whether this compilation may speculate (see `speculates`).
    speculate: bool,
    /// Values known non-null at the current point (checked on every path to it).
    pub nonnull: Vec<V>,
    /// Constant arrays read from `@Stable` fields, with their stable dimensions left.
    pub stable: HashMap<i64, u8>,
    /// Pure instructions of the current block, for value numbering within it.
    pub numbered: HashMap<(u8, u8, V, V, u8), V>,
    /// Field values of the current block, by (object, offset), valid until a memory effect.
    pub loads: HashMap<(V, u32), V>,
    /// Values whose class is known exactly where they are used (allocations, and receivers
    /// inside a body inlined behind an exact class check), innermost last.
    pub exact: Vec<(V, &'static crate::klass::Klass)>,
    /// Bounds checks of the current block: (array, index, check).
    pub bounds: Vec<(V, V, V)>,
    /// The three caches above as each finished block left them, for a block whose only
    /// predecessor it is (an extended basic block: inlined bodies, their returns, guards).
    pub(super) exits: HashMap<B, super::build_cache::Caches>,
}


/// Builds the graph of `m`; `None` if some instruction is not supported. Returns the frame size.
pub(super) fn build(m: &'static Method) -> Option<(Graph, i32)> {
    let sc = Scope::new(m, None, 0)?;
    let mut b = Builder { g: Graph::default(), extent: sc.frame_end(), sc, cur: 0, spent: 0, chain: vec![(m, None)], seq: 0, speculate: m.deopt_recompiles.load(std::sync::atomic::Ordering::Relaxed) < super::SPECULATIVE_RECOMPILES, nonnull: Vec::new(), stable: HashMap::new(), numbered: HashMap::new(), loads: HashMap::new(), exact: Vec::new(), bounds: Vec::new(), exits: HashMap::new() };
    // The entry block reads the parameters, then enters bci 0 like any other edge.
    let start = b.g.new_block(-1);
    b.start_block(start);
    let mut slot = 0u16;
    if !m.is_static() {
        let this = b.g.add(start, Inst::Param(0), Ty::A);
        b.sc.locals[0] = Some(this);
        b.nonnull.push(this);
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
    /// Makes `block` the current one, next in code order, with the caches of its single
    /// predecessor (`build_cache.rs`).
    pub fn start_block(&mut self, block: B) {
        let b = &self.g.blocks[block as usize];
        let only = match b.preds[..] {
            [p] if !b.loop_header && self.sc.npreds.get(&block).map_or(true, |&n| n <= 1) && !self.sc.phis.contains_key(&block) => Some(p),
            _ => None,
        };
        self.carry_caches(only);
        self.cur = block;
        self.g.blocks[block as usize].order = self.seq;
        self.seq += 1;
    }

    /// Creates the scope's blocks and enters its bci 0 from the current block, whose state
    /// (`sc.locals`) holds the arguments.
    pub fn enter_scope(&mut self) {
        self.make_blocks();
        self.goto(0);
        self.sc.entered = true;
    }

    /// Builds the scope's blocks as their first predecessor reaches them (lowest bytecode index
    /// first).
    pub fn build_scope(&mut self) -> Option<()> {
        while !self.sc.ready.is_empty() {
            let ready = &self.sc.ready;
            let i = (0..ready.len()).min_by_key(|&i| self.g.blocks[ready[i] as usize].bci).unwrap();
            let block = self.sc.ready.swap_remove(i);
            let (locals, stack, mut nonnull) = self.sc.entry[&block].clone();
            // Facts need every forward edge in: one still to come may bring a path without them.
            if self.sc.fwd.get(&block).is_some_and(|&(expected, arrived)| arrived < expected) {
                nonnull.clear();
            }
            self.start_block(block);
            self.sc.locals = locals;
            self.sc.stack = stack;
            self.nonnull = nonnull;
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
        let (pruned, live) = self.reachable();
        let sc = &mut self.sc;
        // Predecessors: the entry edge into bci 0, block ends, and fall-through into leaders.
        *sc.npreds.entry(sc.blocks[&0]).or_default() += 1;
        sc.fwd.entry(sc.blocks[&0]).or_default().0 += 1;
        let mut pc = 0;
        while pc < sc.code.len() {
            let len = insn_len(sc.code, pc);
            if sc.an.depth[pc].is_some() && live[pc] {
                let succ = successors(sc.code, pc, len);
                let ends_block = succ.len() != 1 || succ[0] != pc + len || sc.blocks.contains_key(&(pc + len));
                if ends_block {
                    for s in succ.into_iter().filter(|&s| !prunes(&pruned, sc.code, pc, s)) {
                        if let Some(&t) = sc.blocks.get(&s) {
                            *sc.npreds.entry(t).or_default() += 1;
                            if s <= pc {
                                self.g.blocks[t as usize].loop_header = true;
                            } else {
                                sc.fwd.entry(t).or_default().0 += 1;
                            }
                        }
                    }
                }
            }
            pc += len;
        }
    }

    /// The scope's pruned branch directions, and the instructions compiled code can reach given
    /// them and the throw-only blocks left to the interpreter; edges from elsewhere never come.
    fn reachable(&mut self) -> (HashMap<usize, bool>, Vec<bool>) {
        let code = self.sc.code;
        let mut pruned = HashMap::new();
        let mut pc = 0;
        while pc < code.len() {
            let len = insn_len(code, pc);
            if self.sc.an.depth[pc].is_some() && matches!(code[pc], 0x99..=0xa6 | 0xc6 | 0xc7) {
                let target = (pc as i64 + crate::jit::analysis::i2(code, pc + 1) as i64) as usize;
                if let Some(taken) = self.never_taken(pc, target, pc + len) {
                    pruned.insert(pc, taken);
                }
            }
            pc += len;
        }
        let mut live = vec![false; code.len()];
        let mut work = vec![0];
        while let Some(pc) = work.pop() {
            if pc >= code.len() || live[pc] {
                continue;
            }
            live[pc] = true;
            if self.sc.throws[pc] && self.sc.blocks.contains_key(&pc) && self.speculates(self.sc.m, pc) {
                continue;
            }
            let len = insn_len(code, pc);
            work.extend(successors(code, pc, len).into_iter().filter(|&s| !prunes(&pruned, code, pc, s)));
        }
        self.sc.pruned = pruned.clone();
        (pruned, live)
    }

    /// Builds instructions from `pc` until the block ends. An instruction the builder does not
    /// handle (unresolved constants, rare bytecodes) ends the block with a deoptimization to it:
    /// the interpreter runs it, and deoptimizing often recompiles the method (`mod.rs`).
    fn build_block(&mut self, mut pc: usize) -> Option<()> {
        // A block that can only throw is left to the interpreter (C2's uncommon trap for an
        // untaken path) until it has trapped once: `athrow` deoptimizes anyway, and compiling
        // the exception's construction (message concatenation and all) only bloats the code.
        if self.sc.throws[pc] && self.speculates(self.sc.m, pc) {
            self.sc.bci = pc;
            self.deopt();
            return Some(());
        }
        loop {
            self.sc.bci = pc;
            let len = insn_len(self.sc.code, pc);
            match self.build_insn(pc) {
                Some(true) => {}
                Some(false) => return Some(()),
                None => {
                    self.deopt();
                    return Some(());
                }
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
        // A forward edge into a merge keeps only the facts every path to it has; a loop's
        // back edge adds none but takes none away (values checked before the loop stay checked).
        let back = self.sc.entered && self.sc.bci >= self.g.blocks[to as usize].bci as usize;
        if !back {
            self.sc.fwd.entry(to).or_default().1 += 1;
            if let Some(entry) = self.sc.entry.get_mut(&to) {
                let known = &self.nonnull;
                entry.2.retain(|v| known.contains(v));
            }
        }
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
            self.sc.entry.insert(to, (self.sc.locals.clone(), self.sc.stack.clone(), self.nonnull.clone()));
            return;
        }
        let phis: Vec<Option<V>> = slots.iter().map(|s| s.map(|v| self.g.add(to, Inst::Phi(vec![v]), self.g.ty(v)))).collect();
        let (l, s) = phis.split_at(self.sc.locals.len());
        // The first edge's facts; later forward edges narrow them to what holds on every path.
        self.sc.entry.insert(to, (l.to_vec(), s.to_vec(), self.nonnull.clone()));
        self.sc.phis.insert(to, phis);
    }

    /// The block starting at `bci`.
    pub fn block_at(&self, bci: usize) -> B {
        self.sc.blocks[&bci]
    }

    /// The interpreter state before the current instruction.
    pub fn state(&self) -> Rc<State> {
        let sc = &self.sc;
        // Locals the interpreter will not read again from here are dead in the state.
        let locals = sc.locals.iter().enumerate().map(|(i, &v)| v.filter(|_| sc.live.live(sc.bci, i))).collect();
        Rc::new(State { method: sc.m, bci: sc.bci as u32, locals, stack: sc.stack.clone(), caller: sc.caller.clone(), frame: sc.frame })
    }

    pub fn konst(&mut self, ty: Ty, bits: i64) -> V {
        self.add(Inst::Const(bits), ty)
    }

    /// Checks `v` for null unless it is known not to be; it is known afterwards.
    pub fn null_check(&mut self, v: V, s: Rc<State>) {
        if self.nonnull.contains(&v) || self.g.constant(v).is_some_and(|c| c != 0) {
            return;
        }
        self.add(Inst::NullCheck(v, s), Ty::A);
        self.nonnull.push(v);
    }

    /// Whether `v` is known to be null (`Some(true)`) or not (`Some(false)`).
    pub fn nullness(&self, v: V) -> Option<bool> {
        match self.g.constant(v) {
            Some(c) => Some(c == 0),
            None => self.nonnull.contains(&v).then_some(false),
        }
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

    /// A block leaving compiled code for the interpreter at `bci` (C2's uncommon trap), as a
    /// successor of the current block with its state.
    pub fn untaken(&mut self, bci: usize) -> B {
        let at = self.sc.bci;
        self.sc.bci = bci;
        let s = self.state();
        self.sc.bci = at;
        let b = self.g.new_block(-1);
        self.g.blocks[b as usize].preds.push(self.cur);
        self.g.blocks[b as usize].term = Term::Deopt(s);
        self.g.blocks[b as usize].order = self.seq;
        self.seq += 1;
        b
    }

    /// Whether code may leave for the interpreter at `m`'s instruction `bci` when an assumption
    /// from the profile fails there: not where it trapped before, nor anywhere in a method that
    /// kept deoptimizing (HotSpot's per-bytecode and per-method trap limits).
    pub fn speculates(&self, m: &'static Method, bci: usize) -> bool {
        self.speculate && !crate::jit::profile::trapped(m, bci)
    }

    /// Which direction of the branch at `pc` baseline code never took (true: the jump to
    /// `target`, false: the fall-through to `next`), when it ran often enough to tell and the
    /// untaken side has not trapped before.
    pub(super) fn never_taken(&self, pc: usize, target: usize, next: usize) -> Option<bool> {
        let (taken, not) = crate::jit::profile::branch_counts(self.sc.m, pc)?;
        if taken + not < MIN_BRANCH_PROFILE || (taken != 0 && not != 0) {
            return None;
        }
        let cold = if taken == 0 { target } else { next };
        self.speculates(self.sc.m, cold).then_some(taken == 0)
    }

    /// Ends the block by leaving compiled code at the current instruction.
    pub fn deopt(&mut self) {
        let s = self.state();
        self.g.blocks[self.cur as usize].term = Term::Deopt(s);
    }
}

/// Whether the edge from the branch at `pc` to `s` is a pruned direction.
fn prunes(pruned: &HashMap<usize, bool>, code: &[u8], pc: usize, s: usize) -> bool {
    match pruned.get(&pc) {
        Some(&taken) => (s == pc + insn_len(code, pc)) != taken,
        None => false,
    }
}
