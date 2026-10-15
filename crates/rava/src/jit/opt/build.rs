//! Bytecode to SSA by abstract interpretation over basic blocks (C1's graph builder): the
//! operand stack and locals hold values; blocks reached from several places get a phi per live
//! slot, created on first entry and completed as the other predecessors are built. Exception
//! handlers are compiled only for an `athrow` the same method catches further on, or its
//! caller at the call it was inlined at (`throw_to`); any other exception leaves compiled code
//! through deoptimization.
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
    /// For each `athrow` a handler of this method after it covers: that handler and the class
    /// it catches (None: any). The throw jumps there once the exception is an instance; else it
    /// deoptimizes, and the interpreter looks for the handler.
    /// Also for each call to a method that throws itself (`athrow` in its own bytecode), which
    /// an inlined body may leave by its exceptional exit (`catch_out`).
    pub throw_to: HashMap<usize, (usize, Option<&'static crate::klass::Klass>)>,
    /// For an inlined scope whose call its caller's handler covers: the caller's block that
    /// takes the exception thrown (`catch_exits`), and that handler's bci and class.
    pub catch_out: Option<(B, usize, Option<&'static crate::klass::Klass>)>,
    /// The paths that leave this inlined scope by `catch_out`, with the exception.
    pub catch_exits: Vec<(B, V)>,
    /// Conditional branches with a direction left to the interpreter (true: the jump), decided
    /// once per scope as its blocks are made, since profiles change while it compiles.
    pub pruned: HashMap<usize, bool>,
    /// Conditional branches of an OSR version first-tier code never executed (C2's
    /// `Reason_unreached`): the code after the loop the version is entered at, which has no
    /// profile yet. They leave for the interpreter, so that code is compiled once it has run
    /// (and got its own OSR version if it loops) instead of now without profiles.
    unreached: std::collections::HashSet<usize>,
    /// The block starting at each leader.
    blocks: HashMap<usize, B>,
    /// Predecessor edges each block will get (from the bytecode).
    npreds: HashMap<B, usize>,
    /// Of those, the forward ones (not a loop's back edge), and how many have been built.
    fwd: HashMap<B, (usize, usize)>,
    /// Where the scope is entered: bci 0, or a loop header for an OSR version (`osr.rs`).
    start: usize,
    /// The edge into `start` has been made (later edges are the scope's own).
    entered: bool,
    /// For each loop header: the locals no instruction of the loop writes (when the loop is
    /// entered by one forward edge only), which keep their entry value instead of a phi.
    invariant: HashMap<B, Vec<bool>>,
    /// Entry state of each block reached so far: locals, stack and the values known non-null.
    entry: HashMap<B, (Vec<Option<V>>, Vec<Option<V>>, super::build_cache::Facts)>,
    /// The phi of each slot (locals, then stack) of merge blocks.
    pub(super) phis: HashMap<B, Vec<Option<V>>>,
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
    /// For an inlined synchronized method: the object whose monitor it holds.
    pub lock: Option<V>,
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
            throw_to: HashMap::new(),
            catch_out: None,
            catch_exits: Vec::new(),
            pruned: HashMap::new(),
            unreached: Default::default(),
            blocks: HashMap::new(),
            npreds: HashMap::new(),
            fwd: HashMap::new(),
            start: 0,
            entered: false,
            invariant: HashMap::new(),
            entry: HashMap::new(),
            phis: HashMap::new(),
            ready: Vec::new(),
            locals: vec![None; info.max_locals as usize],
            stack: Vec::new(),
            bci: 0,
            caller,
            frame,
            cont: None,
            lock: None,
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
    /// `speculates`'s answers so far: trap counts change while compiling, and every pass of
    /// one compilation (block discovery, then building) must see the same answer.
    trapped: std::cell::RefCell<HashMap<(usize, usize), bool>>,
    /// Values known non-null, or of an exact class, at the current point (on every path to it).
    pub facts: super::build_cache::Facts,
    /// Constant arrays read from `@Stable` fields, with their stable dimensions left.
    pub stable: HashMap<i64, u8>,
    /// Pure instructions of the current block, for value numbering within it.
    pub numbered: HashMap<(u8, u8, V, V, u8), V>,
    /// Field values of the current block, by (object, offset), valid until a memory effect.
    pub loads: HashMap<(V, u32), V>,
    /// Values whose class is known exactly where they are used (allocations, and receivers
    /// inside a body inlined behind an exact class check), innermost last.
    pub exact: Vec<(V, &'static crate::klass::Klass)>,
    /// Field loads whose declared type is a final class: that class exactly, when not null
    /// (C2 types a load by its field's declaration).
    pub declared: HashMap<V, &'static crate::klass::Klass>,
    /// Boxes made by `Integer.valueOf` and its siblings (`build_obj::box_value_of`): the box's
    /// class and the value in it, which an unboxing reads back without the box (C2's
    /// `EliminateAutoBox`); a box is never null.
    pub boxes: HashMap<V, (&'static crate::klass::Klass, V)>,
    /// Phis made with all their inputs (the results of inlined bodies with several returns).
    pub complete_phis: std::collections::HashSet<V>,
    /// The call about to be made is at a site without a receiver profile.
    pub unprofiled_call: bool,
    /// The call about to be made was refused inlining as cold.
    pub cold_call: bool,
    /// The next call is the one made when no inlined target's guard admits the receiver.
    pub fallback_call: bool,
    /// Bounds checks of the current block: (array, index, check).
    pub bounds: Vec<(V, V, V)>,
    /// The three caches above as each finished block left them, for a block whose only
    /// predecessor it is (an extended basic block: inlined bodies, their returns, guards).
    pub(super) exits: HashMap<B, super::build_cache::Caches>,
}


/// Builds the graph of `m`; `None` if some instruction is not supported. Returns the frame size.
pub(super) fn build(m: &'static Method) -> Option<(Graph, i32)> {
    let mut b = Builder::new(Scope::new(m, None, 0)?);
    // The entry block reads the parameters, then enters bci 0 like any other edge.
    let start = b.g.new_block(-1);
    b.start_block(start);
    let mut slot = 0u16;
    if !m.is_static() {
        let this = b.g.add(start, Inst::Param(0), Ty::A);
        b.sc.locals[0] = Some(this);
        b.facts.push(this);
        b.g.receiver = Some(this);
        slot = 1;
    }
    for p in m.sig.params.iter() {
        let ty = Ty::of(*p);
        b.sc.locals[slot as usize] = Some(b.g.add(start, Inst::Param(slot), ty));
        slot += p.slots() as u16;
    }
    let params: Vec<V> = b.sc.locals.iter().flatten().copied().collect();
    b.note_declared_params(m, &params);
    b.enter_scope();
    b.build_scope()?;
    Some((b.g, b.extent))
}

/// Builds the version of `m` entered at the loop header `bci` from a running frame: the locals
/// live there (of the kinds `types` gives) are read from their frame slots as parameters, and
/// only what the header reaches is built. `None` if a live local has no single kind or the
/// operand stack is not empty there.
pub(super) fn build_osr(m: &'static Method, bci: usize, types: &[Option<Ty>]) -> Option<(Graph, i32)> {
    let mut sc = Scope::new(m, None, 0)?;
    if sc.an.depth.get(bci).copied().flatten() != Some(0) || !sc.an.leader[bci] {
        return None;
    }
    sc.start = bci;
    let mut b = Builder::new(sc);
    let start = b.g.new_block(-1);
    b.start_block(start);
    for (slot, &ty) in types.iter().enumerate() {
        // The second half of a long or double is live with it.
        let half = slot > 0 && types[slot - 1].is_some_and(Ty::wide);
        if b.sc.live.live(bci, slot) && !half {
            b.sc.locals[slot] = Some(b.g.add(start, Inst::Param(slot as u16), ty?));
        }
    }
    // The receiver, unless the method stores into local 0.
    if let (false, Some(this)) = (m.is_static(), b.sc.locals.first().copied().flatten()) {
        if !super::locals::written(b.sc.code, 0, b.sc.code.len(), 1)[0] {
            b.facts.push(this);
            b.g.receiver = Some(this);
        }
    }
    b.enter_scope();
    b.build_scope()?;
    Some((b.g, b.extent))
}

impl Builder {
    fn new(sc: Scope) -> Builder {
        let m = sc.m;
        let speculate = m.deopt_recompiles.load(std::sync::atomic::Ordering::Relaxed) < super::SPECULATIVE_RECOMPILES;
        Builder { g: Graph::default(), extent: sc.frame_end(), sc, cur: 0, spent: 0, chain: vec![(m, None)], seq: 0, speculate, trapped: Default::default(), facts: Default::default(), stable: HashMap::new(), numbered: HashMap::new(), loads: HashMap::new(), exact: Vec::new(), declared: HashMap::new(), boxes: HashMap::new(), complete_phis: Default::default(), unprofiled_call: false, cold_call: false, fallback_call: false, bounds: Vec::new(), exits: HashMap::new() }
    }

    /// Makes `block` the current one, next in code order, with the caches of its single
    /// predecessor (`build_cache.rs`).
    pub fn start_block(&mut self, block: B) {
        let b = &self.g.blocks[block as usize];
        let only = match b.preds[..] {
            [p] if !b.loop_header && self.sc.npreds.get(&block).map_or(true, |&n| n <= 1) && !self.sc.phis.contains_key(&block) => Some(p),
            _ => None,
        };
        self.carry_caches(only);
        let bci = self.g.blocks[block as usize].bci;
        let freq = if bci >= 0 { self.sc.frequency(bci as usize) } else { self.g.blocks[self.cur as usize].freq };
        self.g.blocks[block as usize].freq = freq;
        self.cur = block;
        self.g.blocks[block as usize].order = self.seq;
        self.seq += 1;
    }

    /// Creates the scope's blocks and enters its start (bci 0) from the current block, whose
    /// state (`sc.locals`) holds the arguments.
    pub fn enter_scope(&mut self) {
        self.make_blocks();
        self.goto(self.sc.start);
        self.sc.entered = true;
    }

    /// Builds the scope's blocks as their first predecessor reaches them (lowest bytecode index
    /// first).
    pub fn build_scope(&mut self) -> Option<()> {
        while !self.sc.ready.is_empty() {
            let ready = &self.sc.ready;
            let i = (0..ready.len()).min_by_key(|&i| self.g.blocks[ready[i] as usize].bci).unwrap();
            let block = self.sc.ready.swap_remove(i);
            let (mut locals, mut stack, mut nonnull) = self.sc.entry[&block].clone();
            // Facts need every forward edge in: one still to come may bring a path without them.
            let waiting = self.sc.fwd.get(&block).is_some_and(|&(expected, arrived)| arrived < expected);
            if waiting {
                nonnull.clear();
            } else if !self.g.blocks[block as usize].loop_header {
                // The merge is complete: a phi of this block all of whose inputs are one value is
                // that value (trivial-phi removal while building, as in Braun et al.'s SSA
                // construction), so what is built here sees it, constants included. Only this
                // block's own phis: one flowing in from elsewhere (a loop header's, its back edge
                // not built yet) may still gain inputs.
                if let Some(own) = self.sc.phis.get(&block) {
                    let own: Vec<V> = own.iter().flatten().copied().collect();
                    for v in locals.iter_mut().chain(stack.iter_mut()).flatten() {
                        if own.contains(v) {
                            *v = self.trivial_phi(*v);
                        }
                    }
                }
            }
            self.start_block(block);
            self.sc.locals = locals;
            self.sc.stack = stack;
            self.facts = nonnull;
            let bci = self.g.blocks[block as usize].bci as usize;
            if self.g.blocks[block as usize].loop_header && self.speculates(self.sc.m, bci) {
                self.sc.bci = bci;
                let s = self.state();
                self.g.loop_entry.insert(block, s);
            }
            self.build_block(self.g.blocks[block as usize].bci as usize)?;
        }
        Some(())
    }

    /// One block per reachable leader (exception handlers excluded, but those a compiled
    /// `athrow` jumps to), with its predecessor count.
    fn make_blocks(&mut self) {
        let sc = &mut self.sc;
        let table = &sc.m.code.unwrap().handlers;
        let handlers: Vec<usize> = table.iter().map(|h| h.handler as usize).collect();
        // The first handler covering each `athrow`, when it comes after it (a handler covering
        // its own rethrow would be a loop) and catches any exception or a resolved class.
        let mut pc = 0;
        while pc < sc.code.len() {
            let throws_itself = |code: &[u8]| {
                let mut at = 0;
                while at < code.len() {
                    if code[at] == 0xbf {
                        return true;
                    }
                    at += insn_len(code, at);
                }
                false
            };
            let call_throws = matches!(sc.code[pc], 0xb6 | 0xb7 | 0xb8)
                && matches!(sc.m.holder.peek_resolved(u16::from_be_bytes([sc.code[pc + 1], sc.code[pc + 2]])), Some(crate::link::Resolved::Method(t)) if t.code.is_some_and(|c| throws_itself(c.code)));
            if (sc.code[pc] == 0xbf || call_throws) && sc.an.depth[pc].is_some() {
                if let Some(h) = table.iter().find(|h| (h.start as usize..h.end as usize).contains(&pc)) {
                    let catch = match h.catch_type {
                        0 => Some(None),
                        i => match sc.m.holder.peek_resolved(i) {
                            Some(crate::link::Resolved::Class(k)) => Some(Some(k)),
                            _ => None,
                        },
                    };
                    if let (Some(catch), true) = (catch, h.handler as usize > pc && sc.an.depth[h.handler as usize].is_some()) {
                        sc.throw_to.insert(pc, (h.handler as usize, catch));
                    }
                }
            }
            pc += insn_len(sc.code, pc);
        }
        let targets: Vec<usize> = sc.throw_to.values().map(|t| t.0).collect();
        let mut pc = 0;
        while pc < sc.code.len() {
            if sc.an.depth[pc].is_some() && sc.an.leader[pc] && (!handlers.contains(&pc) || targets.contains(&pc)) {
                let b = self.g.new_block(pc as i32);
                sc.blocks.insert(pc, b);
            }
            pc += insn_len(sc.code, pc);
        }
        let (pruned, live) = self.reachable();
        let sc = &mut self.sc;
        // Every edge (source instruction start and end, target) for the loops' extents.
        let mut edges: Vec<(usize, usize, usize)> = Vec::new();
        // Predecessors: the entry edge into the start, block ends, and fall-through into leaders.
        *sc.npreds.entry(sc.blocks[&sc.start]).or_default() += 1;
        sc.fwd.entry(sc.blocks[&sc.start]).or_default().0 += 1;
        let mut pc = 0;
        while pc < sc.code.len() {
            let len = insn_len(sc.code, pc);
            if sc.an.depth[pc].is_some() && live[pc] {
                let mut succ = successors(sc.code, pc, len);
                // A call's inlined bodies may each leave by an exception: never fewer than two
                // edges, so the handler merges what arrives.
                let count = if sc.code[pc] == 0xbf { 1 } else { 2 };
                succ.extend(sc.throw_to.get(&pc).into_iter().flat_map(|t| std::iter::repeat(t.0).take(count)));
                // A switch makes one edge per distinct target (`build_ops::switch`).
                if matches!(sc.code[pc], 0xaa | 0xab) {
                    succ.sort_unstable();
                    succ.dedup();
                }
                let ends_block = succ.len() != 1 || succ[0] != pc + len || sc.blocks.contains_key(&(pc + len));
                if ends_block {
                    for s in succ.into_iter().filter(|&s| !prunes(&pruned, sc.code, pc, s)) {
                        edges.push((pc, pc + len, s));
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
        // A loop's extent: from its header to the end of its last back edge, closed under back
        // edges into it from further on. Locals nothing in it writes need no phi at the header,
        // unless a forward edge enters the extent past the header (irreducible flow).
        let max_locals = sc.m.code.unwrap().max_locals as usize;
        let headers: Vec<usize> = edges.iter().filter(|&&(pc, _, t)| t <= pc).map(|&(_, _, t)| t).collect();
        for h in headers {
            let Some(&b) = sc.blocks.get(&h) else { continue };
            if sc.invariant.contains_key(&b) || sc.fwd.get(&b).map_or(0, |f| f.0) != 1 {
                continue;
            }
            let mut end = h;
            loop {
                let grown = edges.iter().filter(|&&(pc, _, t)| t <= pc && t >= h && t <= end.max(h)).map(|&(_, e, _)| e).max().unwrap_or(end).max(end);
                if grown == end {
                    break;
                }
                end = grown;
            }
            // So does an OSR entry inside the loop: its back edges then arrive before its entry.
            if edges.iter().any(|&(pc, _, t)| pc < h && t > h && t < end) || (h < sc.start && sc.start < end) {
                continue;
            }
            let written = super::locals::written(sc.code, h, end, max_locals);
            sc.invariant.insert(b, written.iter().map(|&w| !w).collect());
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
                // A branch to the next instruction (`if (c) {}`) goes there either way: pruning
                // its untaken direction by address would cut the way it does take.
                if target == pc + len {
                    pc += len;
                    continue;
                }
                if self.sc.start != 0 && crate::jit::profile::branch_counts(self.sc.m, pc) == Some((0, 0)) && self.speculates(self.sc.m, pc) {
                    self.sc.unreached.insert(pc);
                } else if let Some(taken) = self.never_taken(pc, target, pc + len) {
                    pruned.insert(pc, taken);
                }
            }
            pc += len;
        }
        let mut live = vec![false; code.len()];
        let mut work = vec![self.sc.start];
        while let Some(pc) = work.pop() {
            if pc >= code.len() || live[pc] {
                continue;
            }
            live[pc] = true;
            if self.sc.throws[pc] && self.sc.blocks.contains_key(&pc) && self.speculates(self.sc.m, pc) || self.sc.unreached.contains(&pc) {
                continue;
            }
            let len = insn_len(code, pc);
            work.extend(successors(code, pc, len).into_iter().filter(|&s| !prunes(&pruned, code, pc, s)));
            work.extend(self.sc.throw_to.get(&pc).map(|t| t.0));
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
            if self.sc.unreached.contains(&pc) {
                self.deopt();
                return Some(());
            }
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
                let known = &self.facts;
                entry.2.narrow(known);
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
        assert!(
            !self.sc.entry.contains_key(&to),
            "a single-predecessor block entered twice: {} bci {} from bci {}",
            self.sc.m,
            self.g.blocks[to as usize].bci,
            self.sc.bci
        );
        self.sc.ready.push(to);
        let merge = self.sc.npreds.get(&to).copied().unwrap_or(0) > 1 || self.g.blocks[to as usize].loop_header;
        if !merge {
            self.sc.entry.insert(to, (self.sc.locals.clone(), self.sc.stack.clone(), self.facts.clone()));
            return;
        }
        let invariant = self.sc.invariant.get(&to).cloned().unwrap_or_default();
        let keep = |i: usize| invariant.get(i).copied().unwrap_or(false);
        let phis: Vec<Option<V>> = slots.iter().enumerate().map(|(i, s)| s.filter(|_| !keep(i)).map(|v| self.g.add(to, Inst::Phi(vec![v]), self.g.ty(v)))).collect();
        let entry: Vec<Option<V>> = slots.iter().enumerate().map(|(i, &s)| if keep(i) { s } else { phis[i] }).collect();
        let (l, s) = entry.split_at(self.sc.locals.len());
        // The first edge's facts; later forward edges narrow them to what holds on every path.
        // A loop header first reached by a back edge (an outer loop of an OSR version's) keeps
        // none: later back edges narrow nothing.
        let facts = if back { Default::default() } else { self.facts.clone() };
        self.sc.entry.insert(to, (l.to_vec(), s.to_vec(), facts));
        self.sc.phis.insert(to, phis);
    }

    /// The block starting at `bci`.
    pub fn block_at(&self, bci: usize) -> B {
        self.sc.blocks[&bci]
    }

    /// The interpreter state before the current instruction.
    pub fn state(&self) -> Rc<State> {
        let sc = &self.sc;
        // Locals the interpreter will not read again from here are dead in the state, except the
        // receiver of an inlined synchronized method: deoptimization releases its monitor.
        let held = sc.lock.is_some() && !sc.m.is_static();
        let locals = sc.locals.iter().enumerate().map(|(i, &v)| v.filter(|_| sc.live.live(sc.bci, i) || held && i == 0)).collect();
        Rc::new(State { method: sc.m, bci: sc.bci as u32, locals, stack: sc.stack.clone(), caller: sc.caller.clone(), frame: sc.frame })
    }

    /// The single value phi `v` merges, if all its inputs are one value; else `v`.
    fn trivial_phi(&self, v: V) -> V {
        let Inst::Phi(inputs) = self.g.inst(v) else { return v };
        let mut only = None;
        for &x in inputs {
            let x = self.g.resolve(x);
            if x == v || only == Some(x) {
                continue;
            }
            if only.is_some() {
                return v;
            }
            only = Some(x);
        }
        only.unwrap_or(v)
    }

    /// A forward edge from the current block to `to` that will never be taken after all (a
    /// branch decided while building): `to` no longer waits for it, so its merge is complete
    /// once the other edges are in.
    pub fn retire_edge(&mut self, to: B) {
        let back = self.sc.entered && self.sc.bci >= self.g.blocks[to as usize].bci as usize;
        if !back {
            self.retire_forward(to);
        }
    }

    /// One counted forward edge into `to` will not come. A block that thereby loses every edge
    /// before any reached it is dead, and so are the forward edges it would have made (a
    /// constant test skipping a whole `if (x == null) { ... }` takes the edges out of its body
    /// too, so the join after it is complete with the one edge that does arrive).
    fn retire_forward(&mut self, to: B) {
        let Some(f) = self.sc.fwd.get_mut(&to) else { return };
        f.0 = f.0.saturating_sub(1);
        if f.0 > 0 || f.1 > 0 || self.g.blocks[to as usize].loop_header || self.sc.entry.contains_key(&to) {
            return;
        }
        // The dead block's last instruction and the edges `make_blocks` counted from it.
        let mut pc = self.g.blocks[to as usize].bci as usize;
        let code = self.sc.code;
        loop {
            let len = insn_len(code, pc);
            let succ = successors(code, pc, len);
            if succ.len() != 1 || succ[0] != pc + len || self.sc.blocks.contains_key(&(pc + len)) || pc + len >= code.len() {
                let mut succ: Vec<usize> = succ.into_iter().filter(|&s| s > pc && !prunes(&self.sc.pruned, code, pc, s)).collect();
                if matches!(code[pc], 0xaa | 0xab) {
                    succ.sort_unstable();
                    succ.dedup();
                }
                for s in succ {
                    if let Some(&t) = self.sc.blocks.get(&s) {
                        self.retire_forward(t);
                    }
                }
                return;
            }
            pc += len;
        }
    }

    /// The constant `v` always is: a constant, or a phi of this scope whose block can gain no
    /// more inputs (not a loop header, every forward edge in) and whose inputs all fold to that
    /// constant. A merge only one of whose edges survived (the others decided while building)
    /// keeps a one-input phi until `phis::simplify`; folding through it here lets decisions
    /// made while building, such as which method a constant method handle names, see the
    /// constant.
    pub fn folded_constant(&self, v: V) -> Option<i64> {
        self.folded_constant_within(v, 6)
    }

    fn folded_constant_within(&self, v: V, depth: u32) -> Option<i64> {
        let v = self.g.resolve(v);
        if let Some(c) = self.g.constant(v) {
            return Some(c);
        }
        let Inst::Phi(inputs) = self.g.inst(v) else { return None };
        if depth == 0 || inputs.is_empty() {
            return None;
        }
        let (&block, _) = self.sc.phis.iter().find(|(_, ps)| ps.contains(&Some(v)))?;
        let complete = !self.g.blocks[block as usize].loop_header && self.sc.fwd.get(&block).map_or(true, |&(expected, arrived)| arrived >= expected);
        if !complete {
            return None;
        }
        let mut folded = None;
        for &x in inputs {
            let c = self.folded_constant_within(x, depth - 1)?;
            if folded.is_some_and(|f| f != c) {
                return None;
            }
            folded = Some(c);
        }
        folded
    }

    pub fn konst(&mut self, ty: Ty, bits: i64) -> V {
        self.add(Inst::Const(bits), ty)
    }

    /// Checks `v` for null unless it is known not to be; it is known afterwards.
    pub fn null_check(&mut self, v: V, s: Rc<State>) {
        if self.nullness(v) == Some(false) {
            return;
        }
        self.add(Inst::NullCheck(v, s), Ty::A);
        self.facts.push(v);
    }

    /// Whether `v` is known to be null (`Some(true)`) or not (`Some(false)`): an allocation's
    /// result never is, wherever it flows (the per-block facts do not survive merges).
    pub fn nullness(&self, v: V) -> Option<bool> {
        match self.g.constant(v) {
            Some(c) => Some(c == 0),
            None if matches!(self.g.inst(self.g.resolve(v)), Inst::New(..) | Inst::NewArray(..) | Inst::NewMulti(..)) => Some(false),
            None if self.boxes.contains_key(&self.g.resolve(v)) => Some(false),
            None => self.facts.contains(&v).then_some(false),
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
        self.speculate && !*self.trapped.borrow_mut().entry((m as *const Method as usize, bci)).or_insert_with(|| crate::jit::profile::trapped(m, bci))
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
