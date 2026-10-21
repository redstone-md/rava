//! Linear-scan register allocation over SSA values (Poletto & Sarkar): one contiguous interval
//! per value from block liveness, then a single pass in start order. Values live across a call
//! prefer callee-saved registers; in a caller-saved or SSE register, code generation saves them
//! around each such call (`Allocation::saves`). rcx and rdx go to values no instruction writing
//! them interrupts (`scratch.rs`). Constants are never allocated. Spilled values
//! stay in their slot for the whole interval; `split.rs` cuts intervals at loop boundaries so
//! that a hot loop's part of a value can stay in a register.
//!
//! Deoptimization states read values without needing them in registers (C2's debug-info uses):
//! registers are allocated over each value's intervals of real uses only. A value a state reads
//! beyond them gets a home slot, written once at its definition (SSA values never change), and
//! deoptimization reads it from there.

use super::ir::*;
use super::liveness::{liveness, StateReads};
use super::phi_share;
use crate::jit::asm::R;

/// SSE registers for float and double values (all caller-saved; xmm0 and xmm1 are scratch).
pub const XMMS: [u8; 14] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Registers holding values across calls, and the ones calls clobber. rax, rcx, rdx and r11 are
/// scratch for code generation; r12 and r13 hold the thread and the frame.
pub const CALLEE_SAVED: [R; 4] = [R::Rbx, R::Rbp, R::R14, R::R15];
pub const CALLER_SAVED: [R; 5] = [R::Rsi, R::Rdi, R::R8, R::R9, R::R10];
/// Scratch registers of some instructions (`scratch.rs`), handed only to values whose interval
/// avoids every instruction that may write them; tried last.
pub const SCRATCH: [R; 2] = [R::Rcx, R::Rdx];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Loc {
    /// The value is never read.
    None,
    Reg(R),
    /// An SSE register (float and double values).
    Xmm(u8),
    /// Spill slot `n` at `[rsp + 8n]`.
    Stack(i32),
    /// Frame slot `n` at `[r13 + 8n]`: where a parameter already is (read only).
    Frame(i32),
    Const(i64),
}

/// Registers to keep around a runtime call: general purpose by `R` number, then SSE.
pub type Preserve = (u16, u16);

pub struct Allocation {
    pub loc: Vec<Loc>,
    /// Blocks in code order.
    pub order: Vec<B>,
    /// Spill slots, then the save slots of calls.
    pub spill_slots: i32,
    /// For each call, the caller-saved and SSE registers holding values live across it, with
    /// the slot each is kept in meanwhile: its home slot, else one of `save_base..`.
    pub saves: std::collections::HashMap<V, Vec<(Loc, Loc)>>,
    /// Kept around each call for its own deoptimization only: restored on that path alone.
    pub state_saves: std::collections::HashMap<V, Vec<(Loc, Loc)>>,
    pub save_base: i32,
    /// The caller-saved and SSE registers (as masks by register number) holding values live at
    /// each instruction and at the end of each block: what a runtime call from the
    /// instruction's slow path, or from a poll on the block's way out, must preserve.
    pub live_at: std::collections::HashMap<V, Preserve>,
    pub live_at_end: Vec<Preserve>,
    /// Where a register-allocated value is also kept for states: a spill slot it is stored to
    /// at its definition, or for a parameter the frame slot it arrived in.
    pub home: Vec<Option<Loc>>,
    /// Values kept in their register past their last real use for deoptimization states to read
    /// there (`extend_for_states`): such a register must hold the full reference.
    pub state_regs: Vec<bool>,
}

/// The values an instruction reads, aliases resolved.
fn inputs(g: &Graph, inst: &Inst) -> Vec<V> {
    inst.inputs().into_iter().map(|v| g.resolve(v)).collect()
}

/// The input phi `p` of block `to` takes along the edge from `from` (alias resolved).
pub fn phi_input(g: &Graph, p: V, from: B, to: B) -> V {
    let i = g.blocks[to as usize].preds.iter().position(|&b| b == from).expect("edge into the phi's block");
    match g.inst(p) {
        Inst::Phi(inputs) => g.resolve(inputs[i]),
        _ => unreachable!("not a phi"),
    }
}

/// The phis of block `b` that are not aliases of another value.
pub fn phis(g: &Graph, b: B) -> impl Iterator<Item = V> + '_ {
    g.blocks[b as usize].insts.iter().copied().take_while(move |&v| matches!(g.inst(v), Inst::Phi(_))).filter(move |&v| g.resolve(v) == v)
}

struct Intervals {
    start: Vec<u32>,
    end: Vec<u32>,
    used: Vec<bool>,
    /// Positions of instructions that clobber caller-saved registers.
    calls: Vec<u32>,
    /// Estimated executions of each call (its block's profile frequency), in `calls` order.
    call_freq: Vec<f32>,
    /// The calls, with their positions.
    call_values: Vec<(u32, V)>,
    /// Real uses weighted by loop depth (8 per level), and the weight of the block at hand.
    weight: Vec<f32>,
    block_weight: f32,
    /// Real uses by estimated executions (block profile frequencies), and the block at hand's.
    reads: Vec<f32>,
    block_freq: f32,
    /// Real uses of each value (`reads[v] / uses[v]` is how often one runs on average).
    uses: Vec<u32>,
    /// Blocks with real uses of each value (for `LoopMap::idle`), and the block at hand.
    use_blocks: Vec<Vec<B>>,
    block: B,
    /// Positions of instructions that may write rcx, and rdx (`scratch.rs`), ascending.
    dirty: [Vec<u32>; 2],
}

impl Intervals {
    fn touch(&mut self, v: V, p: u32) {
        let i = v as usize;
        self.start[i] = self.start[i].min(p);
        self.end[i] = self.end[i].max(p);
    }
    fn used_at(&mut self, v: V, p: u32) {
        self.weight[v as usize] += self.block_weight;
        self.reads[v as usize] += self.block_freq;
        self.uses[v as usize] += 1;
        let blocks = &mut self.use_blocks[v as usize];
        if blocks.last() != Some(&self.block) {
            blocks.push(self.block);
        }
        self.read_at(v, p);
    }
    /// A read that needs no register (a state's, a phi's own back-edge input): no weight.
    fn read_at(&mut self, v: V, p: u32) {
        self.used[v as usize] = true;
        self.touch(v, p);
    }
    /// Whether `v` may live in scratch register `r` (rcx or rdx): no instruction that may write
    /// it lies within the interval, its ends included.
    fn scratch_free(&self, v: usize, r: R) -> bool {
        let d = &self.dirty[(r == R::Rdx) as usize];
        let i = d.partition_point(|&p| p < self.start[v]);
        d.get(i).map_or(true, |&p| p > self.end[v])
    }
    fn crosses_call(&self, v: usize) -> bool {
        let i = self.calls.partition_point(|&p| p <= self.start[v]);
        self.calls.get(i).is_some_and(|&p| p < self.end[v])
    }
    /// Estimated executions of the calls `v` lives across.
    fn calls_crossed(&self, v: usize) -> f32 {
        let i = self.calls.partition_point(|&p| p <= self.start[v]);
        let j = self.calls.partition_point(|&p| p < self.end[v]);
        self.call_freq[i..j.max(i)].iter().sum()
    }
}

/// Loops by position, for spill choices.
struct LoopMap {
    /// Loop nesting depth of each block.
    depth: Vec<u32>,
    /// The innermost loop of each block, as a body mask.
    innermost: Vec<Option<usize>>,
    bodies: Vec<Vec<bool>>,
    /// The block of each position.
    block_at: Vec<B>,
}

impl LoopMap {
    fn new(g: &Graph, order: &[B], range: &[(u32, u32)]) -> LoopMap {
        let loops = super::dom::Dom::new(g).loops(g);
        let n = g.blocks.len();
        let mut depth = vec![0u32; n];
        let mut innermost: Vec<Option<usize>> = vec![None; n];
        let sizes: Vec<usize> = loops.iter().map(|l| l.body.iter().filter(|&&x| x).count()).collect();
        for (i, l) in loops.iter().enumerate() {
            for b in (0..n).filter(|&b| l.body[b]) {
                depth[b] += 1;
                if innermost[b].map_or(true, |j| sizes[j] > sizes[i]) {
                    innermost[b] = Some(i);
                }
            }
        }
        let end = order.last().map_or(0, |&b| range[b as usize].1 as usize + 2);
        let mut block_at = vec![0; end];
        for &b in order {
            let (from, to) = range[b as usize];
            block_at[from as usize..=(to as usize).min(end - 1)].fill(b);
        }
        LoopMap { depth, innermost, bodies: loops.into_iter().map(|l| l.body).collect(), block_at }
    }

    /// Whether `v` sits idle through the innermost loop around position `p`: no real use of it
    /// is in that loop (spilling it there costs nothing in the loop).
    fn idle(&self, iv: &Intervals, v: usize, p: u32) -> bool {
        let Some(&b) = self.block_at.get(p as usize) else { return false };
        let Some(l) = self.innermost[b as usize] else { return false };
        !iv.use_blocks[v].iter().any(|&u| self.bodies[l][u as usize])
    }
}

pub fn allocate(g: &Graph, m: &crate::method::Method) -> Allocation {
    let mut order: Vec<B> = (0..g.blocks.len() as B).filter(|&b| !matches!(g.blocks[b as usize].term, Term::None)).collect();
    order.sort_by_key(|&b| g.blocks[b as usize].order);
    let n = g.values.len();
    // Positions: two per instruction (reads at p, a call's state reads at p + 1).
    let mut pos = vec![0u32; n];
    let mut range = vec![(0u32, 0u32); g.blocks.len()];
    let mut p = 0;
    for &b in &order {
        let from = p;
        for &v in &g.blocks[b as usize].insts {
            pos[v as usize] = p;
            p += 2;
        }
        range[b as usize] = (from, p + 1);
        p += 2;
    }
    let loops = LoopMap::new(g, &order, &range);
    let mut reads = StateReads::new();
    let full = intervals(g, &order, &pos, &range, &loops.depth, &mut reads, true);
    let mut iv = intervals(g, &order, &pos, &range, &loops.depth, &mut reads, false);
    let hints = super::hints::abi_hints(g, m, &pos, &iv.end);
    let (mut loc, spill_slots, home, state_regs) = scan(g, &mut iv, &full, &loops, &hints, &mut reads);
    phi_share::into_slots(g, &mut loc, &mut reads, &full.start, &full.end);
    // Registers a call clobbers that hold values live across it: restored after it if a later
    // instruction reads them, else only on the way to its deoptimization if its state does,
    // else not kept at all (the interval spans a path the call is not on).
    let live_in = super::liveness::liveness(g, &order, &mut reads, true);
    let after = super::liveness::live_after_calls(g, &order, &mut reads, &live_in);
    let mut saves = std::collections::HashMap::new();
    let mut state_saves = std::collections::HashMap::new();
    let mut most = 0;
    for &(p, call) in &iv.call_values {
        let own: Vec<V> = g.inst(call).state().map_or(Vec::new(), |s| reads.of(g, s).to_vec());
        // A value with a home slot is already there: it is only restored from it.
        let mut next = 0;
        let (mut live, mut for_state): (Vec<(Loc, Loc)>, Vec<(Loc, Loc)>) = (Vec::new(), Vec::new());
        for v in (0..n).filter(|&v| iv.used[v] && iv.start[v] < p && iv.end[v] > p) {
            if !(matches!(loc[v], Loc::Xmm(_)) || matches!(loc[v], Loc::Reg(r) if CALLER_SAVED.contains(&r))) {
                continue;
            }
            let later = after.get(&call).is_none_or(|a| a.contains(v as V));
            if !later && !own.contains(&(v as V)) {
                continue;
            }
            let slot = home[v].unwrap_or_else(|| {
                next += 1;
                Loc::Stack(spill_slots + next - 1)
            });
            if later { live.push((loc[v], slot)) } else { for_state.push((loc[v], slot)) }
        }
        most = most.max(next);
        saves.insert(call, live);
        state_saves.insert(call, for_state);
    }
    let (live_at, live_at_end) = live_registers(g, &order, &pos, &range, &iv, &loc);
    for v in 0..g.values.len() as V {
        loc[v as usize] = loc[g.resolve(v) as usize];
    }
    let mut home = home;
    for v in 0..g.values.len() as V {
        home[v as usize] = home[g.resolve(v) as usize];
    }
    let state_regs = (0..g.values.len()).map(|v| state_regs[g.resolve(v as V) as usize]).collect();
    Allocation { loc, order, spill_slots: spill_slots + most, saves, state_saves, save_base: spill_slots, home, live_at, live_at_end, state_regs }
}

/// Keeps in its register a value that only deoptimization states read after its last real use,
/// up to the last of those reads, where nothing else takes that register in between (and no
/// call clobbers it, nor an instruction writing it as scratch): the states read it there, and
/// the store to a home slot at its definition, on the path that does not deoptimize, goes.
fn extend_for_states(g: &Graph, iv: &mut Intervals, full: &Intervals, loc: &[Loc]) -> Vec<bool> {
    let mut extended = vec![false; g.values.len()];
    let n = g.values.len();
    let held: Vec<usize> = (0..n).filter(|&v| iv.used[v] && matches!(loc[v], Loc::Reg(_) | Loc::Xmm(_))).collect();
    for &v in &held {
        let (from, to) = (iv.end[v], full.end[v]);
        if to <= from {
            continue;
        }
        let caller_saved = !matches!(loc[v], Loc::Reg(r) if CALLEE_SAVED.contains(&r));
        if caller_saved && iv.calls.iter().any(|&p| p >= from && p < to) {
            continue;
        }
        if let Loc::Reg(r @ (R::Rcx | R::Rdx)) = loc[v] {
            if iv.dirty[(r == R::Rdx) as usize].iter().any(|&p| p >= from && p <= to) {
                continue;
            }
        }
        if held.iter().any(|&u| u != v && loc[u] == loc[v] && iv.start[u] <= to && iv.end[u] >= from) {
            continue;
        }
        iv.end[v] = to;
        extended[v] = true;
    }
    extended
}

/// The registers runtime calls must preserve at each instruction and block end (`Allocation::
/// live_at`): those of every value live there, its own result and the inputs it reads last
/// included (a slow path runs in the middle of the instruction). One sweep in position order.
fn live_registers(g: &Graph, order: &[B], pos: &[u32], range: &[(u32, u32)], iv: &Intervals, loc: &[Loc]) -> (std::collections::HashMap<V, Preserve>, Vec<Preserve>) {
    enum At {
        Inst(V),
        End(B),
    }
    let mut queries: Vec<(u32, At)> = Vec::new();
    for &b in order {
        for &v in &g.blocks[b as usize].insts {
            if g.resolve(v) == v && !matches!(g.inst(v), Inst::Phi(_) | Inst::Const(_) | Inst::Param(_) | Inst::Copy(_)) {
                queries.push((pos[v as usize], At::Inst(v)));
            }
        }
        queries.push((range[b as usize].1, At::End(b)));
    }
    queries.sort_by_key(|q| q.0);
    let bit = |l: Loc| -> Preserve {
        match l {
            Loc::Reg(r) if CALLER_SAVED.contains(&r) || SCRATCH.contains(&r) => (1 << r as u8, 0),
            Loc::Xmm(x) => (0, 1 << x),
            _ => (0, 0),
        }
    };
    let mut by_start: Vec<usize> = (0..g.values.len()).filter(|&v| iv.used[v] && bit(loc[v]) != (0, 0)).collect();
    by_start.sort_by_key(|&v| iv.start[v]);
    let (mut next, mut active) = (0, Vec::new());
    let mut at = std::collections::HashMap::new();
    let mut ends = vec![(0, 0); g.blocks.len()];
    for (p, q) in queries {
        while next < by_start.len() && iv.start[by_start[next]] <= p {
            active.push(by_start[next]);
            next += 1;
        }
        active.retain(|&a| iv.end[a] >= p);
        let mask = active.iter().fold((0u16, 0u16), |m, &a| {
            let b = bit(loc[a]);
            (m.0 | b.0, m.1 | b.1)
        });
        match q {
            At::Inst(v) => {
                at.insert(v, mask);
            }
            At::End(b) => ends[b as usize] = mask,
        }
    }
    (at, ends)
}

/// Live intervals from block liveness; with `states`, uses by deoptimization states count
/// (they always do for phis, which get no home slot).
fn intervals(g: &Graph, order: &[B], pos: &[u32], range: &[(u32, u32)], depth: &[u32], reads: &mut StateReads, states: bool) -> Intervals {
    let n = g.values.len();
    let live_in = liveness(g, order, reads, states);
    let mut iv = Intervals { start: vec![u32::MAX; n], end: vec![0; n], used: vec![false; n], calls: Vec::new(), call_freq: Vec::new(), call_values: Vec::new(), weight: vec![0.0; n], block_weight: 1.0, reads: vec![0.0; n], block_freq: 0.0, uses: vec![0; n], use_blocks: vec![Vec::new(); n], block: 0, dirty: [Vec::new(), Vec::new()] };
    for &b in order {
        let (from, to) = range[b as usize];
        iv.block_weight = 8f32.powi(depth[b as usize].min(5) as i32);
        iv.block_freq = g.blocks[b as usize].freq;
        iv.block = b;
        let blk = &g.blocks[b as usize];
        for v in live_in[b as usize].iter() {
            iv.touch(v, from);
        }
        for s in blk.term.successors() {
            for v in live_in[s as usize].iter() {
                iv.touch(v, to);
            }
            for ph in phis(g, s) {
                match phi_input(g, ph, b, s) {
                    x if x == ph => iv.read_at(x, to),
                    x => {
                        iv.used_at(x, to);
                        // The phi is written there too: a phi in a slot stores on every pass
                        // of the edge (a loop's accumulator, once per iteration).
                        iv.weight[ph as usize] += iv.block_weight;
                    }
                }
            }
        }
        for &v in &blk.insts {
            let p = pos[v as usize];
            if g.resolve(v) != v {
                continue;
            }
            iv.touch(v, if matches!(g.inst(v), Inst::Phi(_)) { from } else { p });
            let inst = g.inst(v);
            if !matches!(inst, Inst::Phi(_)) {
                inputs(g, inst).into_iter().for_each(|u| iv.used_at(u, p));
            }
            if let Some(s) = inst.state() {
                let at = if matches!(inst, Inst::Call(..)) { p + 1 } else { p };
                reads.of(g, s).iter().filter(|&&u| states || is_phi(g, u)).for_each(|&u| iv.read_at(u, at));
            }
            if matches!(inst, Inst::Call(..)) {
                iv.calls.push(p);
                iv.call_freq.push(iv.block_freq);
                iv.call_values.push((p, v));
            }
            let (cx, dx) = super::scratch::clobbers(g, v);
            for (d, on) in iv.dirty.iter_mut().zip([cx, dx]) {
                if on {
                    d.push(p);
                }
            }
        }
        blk.term.inputs().into_iter().for_each(|u| iv.used_at(g.resolve(u), to - 1));
        if let Term::Deopt(s) = &blk.term {
            reads.of(g, s).iter().filter(|&&u| states || is_phi(g, u)).for_each(|&u| iv.read_at(u, to - 1));
        }
    }
    // Blocks come in position order, so the calls are sorted already.
    debug_assert!(iv.calls.windows(2).all(|w| w[0] < w[1]));
    iv.dirty.iter_mut().for_each(|d| d.sort_unstable());
    iv
}

fn is_phi(g: &Graph, v: V) -> bool {
    matches!(g.inst(v), Inst::Phi(_))
}


fn few_regs() -> Option<usize> {
    static N: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("RAVA_OPT_FEW_REGS").ok().and_then(|v| v.parse().ok()))
}

/// Assigns registers in interval start order; out of registers, it spills whichever of the new
/// interval and the active ones has the fewest loop-weighted uses per position covered. A value
/// may share its first operand's register when that operand dies at its definition.
/// `iv` has the intervals of real uses, `full` those including state uses: a value only states
/// read lives in a spill slot, one read beyond its real uses gets a home slot as well.
fn scan(g: &Graph, iv: &mut Intervals, full: &Intervals, loops: &LoopMap, hints: &[Option<Loc>], reads: &mut StateReads) -> (Vec<Loc>, i32, Vec<Option<Loc>>, Vec<bool>) {
    let n = g.values.len();
    let mut loc = vec![Loc::None; n];
    let mut todo = Vec::new();
    let mut spilled = Vec::new();
    for v in 0..n {
        match g.values[v].inst {
            Inst::Const(c) => loc[v] = Loc::Const(c),
            _ if iv.used[v] => todo.push(v),
            _ if full.used[v] => spilled.push(v),
            _ => {}
        }
    }
    todo.sort_by_key(|&v| (iv.start[v], v));
    // The phis each value flows into: taking a phi's register when the phi dies where the value
    // is defined (a loop's `s = s + x`) saves the move on the edge into the phi.
    let mut phi_users: Vec<Vec<V>> = vec![Vec::new(); n];
    for (p, val) in g.values.iter().enumerate() {
        if let Inst::Phi(ins) = &val.inst {
            if g.resolve(p as V) == p as V {
                for &i in ins {
                    let i = g.resolve(i) as usize;
                    if i != p && !phi_users[i].contains(&(p as V)) {
                        phi_users[i].push(p as V);
                    }
                }
            }
        }
    }
    let block_of = phi_share::blocks_of(g);
    let mut active: Vec<usize> = Vec::new();
    for &v in &todo {
        active.retain(|&a| iv.end[a] >= iv.start[v]);
        let crossing = iv.crosses_call(v);
        let taken = |c: Loc, active: &[usize], loc: &[Loc]| active.iter().any(|&a| loc[a] == c);
        // Candidate registers in preference order: none survives a call but the callee-saved.
        // Across calls, callee-saved registers come first; a caller-saved one costs a save and
        // a restore at each call, so only a value read at least as often as that (by profile
        // frequency: a cold call in a loop costs little) takes one. A slot costs a load per
        // read, and `reload.rs` keeps repeated reads between calls in a register.
        let saving = !crossing || iv.reads[v] >= 2.0 * iv.calls_crossed(v);
        let mut cands: Vec<Loc> = match (matches!(g.values[v].ty, Ty::F | Ty::D), crossing) {
            (true, _) => XMMS.iter().filter(|_| saving).map(|&x| Loc::Xmm(x)).collect(),
            (false, true) => CALLEE_SAVED.iter().chain(CALLER_SAVED.iter().filter(|_| saving)).map(|&r| Loc::Reg(r)).collect(),
            (false, false) => CALLER_SAVED.iter().chain(CALLEE_SAVED.iter()).chain(SCRATCH.iter()).filter(|&&r| !SCRATCH.contains(&r) || iv.scratch_free(v, r)).map(|&r| Loc::Reg(r)).collect(),
        };
        // `RAVA_OPT_FEW_REGS=n`: only the first n candidates, so that most values spill
        // (a test mode for code generation's use of scratch registers around spilled operands).
        if let Some(k) = few_regs() {
            cands.truncate(k);
        }
        let allowed = |c: Loc| cands.contains(&c);
        // Coalescing: the result takes the register of its first operand when that dies here
        // (code generation computes into the first operand's register), or of a commutative
        // operation's second operand (computed into it the other way round).
        // (Not for a call, whose result comes in rax.)
        let operands = match g.values[v].inst {
            Inst::Phi(_) | Inst::Call(..) => Vec::new(),
            ref inst => inputs(g, inst),
        };
        let commutes = matches!(g.values[v].inst, Inst::Bin(BinOp::Add | BinOp::Mul | BinOp::And | BinOp::Or | BinOp::Xor, ..));
        let reuse = |x: V| {
            let c = loc[x as usize];
            (iv.end[x as usize] == iv.start[v] && allowed(c) && !active.iter().any(|&a| a != x as usize && loc[a] == c)).then_some((x as usize, c))
        };
        let hint = phi_users[v].iter().find_map(|&p| reuse(p));
        // A phi still open but never read again before it takes this value (`phi_share.rs`):
        // both keep the register. The phi may be an operand only where the result is computed
        // into it.
        let at = |x: V| operands.iter().position(|&o| o == x);
        let share = phi_users[v].iter().copied().filter(|_| hint.is_none()).find(|&p| {
            let c = loc[p as usize];
            matches!(c, Loc::Reg(_) | Loc::Xmm(_))
                && allowed(c)
                && active.contains(&(p as usize))
                && !active.iter().any(|&a| a != p as usize && loc[a] == c)
                && at(p).is_none_or(|i| i == 0 || i == 1 && commutes)
                && phi_share::dead_after(g, reads, &block_of, p, v as V)
        });
        if let Some(p) = share {
            loc[v] = loc[p as usize];
            active.push(v);
            continue;
        }
        let hint = hint
            .or_else(|| operands.first().and_then(|&x| reuse(x)))
            .or_else(|| operands.get(1).filter(|_| commutes).and_then(|&y| reuse(y)));
        if let Some((x, c)) = hint {
            loc[v] = c;
            active.retain(|&a| a != x);
            active.push(v);
            continue;
        }
        // A register free here that a phi this value flows into, or (for a phi) one of its
        // inputs, holds before or after it: the move on that edge goes (a loop's join phi
        // takes the loop phi's register once that one is dead).
        let related = match &g.values[v].inst {
            Inst::Phi(ins) => ins.iter().map(|&i| g.resolve(i)).filter(|&i| i as usize != v).collect(),
            _ => Vec::new(),
        };
        // Across a call only a callee-saved one, as the candidates prefer.
        let keeps = |c: Loc| !crossing || matches!(c, Loc::Reg(r) if CALLEE_SAVED.contains(&r));
        let free = phi_users[v].iter().chain(&related).map(|&x| loc[x as usize]).find(|&c| matches!(c, Loc::Reg(_) | Loc::Xmm(_)) && keeps(c) && allowed(c) && !taken(c, &active, &loc));
        // The register the calling convention brings or takes the value in (`hints.rs`) first.
        let abi = hints[v].filter(|&c| !crossing && allowed(c) && !taken(c, &active, &loc));
        if let Some(c) = abi.or(free).or_else(|| cands.iter().copied().find(|&c| !taken(c, &active, &loc))) {
            loc[v] = c;
            active.push(v);
            continue;
        }
        // Spill the cheapest of this interval and the active ones in its register class: one
        // idle through the loop here first; then, in a rarely run block, one that is not hot
        // (its reads run on average at least four times as often as this block) before one
        // that is; else the fewest loop-weighted uses per position, which frees the register
        // for the most later intervals. Uses per position alone spilled a hot loop's counter
        // and bounds (long intervals) for the temporaries of a rarely taken branch in it
        // (short ones), and every iteration reloaded them.
        let p = iv.start[v];
        let here = loops.block_at.get(p as usize).map_or(0.0, |&b| g.blocks[b as usize].freq);
        let hot = |x: usize| iv.uses[x] > 0 && iv.reads[x] > 4.0 * here * iv.uses[x] as f32;
        let cost = |x: usize| (!loops.idle(iv, x, p), hot(x), iv.weight[x] / (iv.end[x] - iv.start[x] + 1) as f32);
        let cheaper = |a: usize, b: usize| {
            let (ca, cb) = (cost(a), cost(b));
            ca.0.cmp(&cb.0).then(ca.1.cmp(&cb.1)).then(ca.2.total_cmp(&cb.2)).then(iv.end[b].cmp(&iv.end[a]))
        };
        // Not one sharing its register with a phi (phi_share.rs): the other would keep it.
        let alone = |a: usize| active.iter().filter(|&&b| loc[b] == loc[a]).count() == 1;
        let victim = active.iter().copied().filter(|&a| allowed(loc[a]) && alone(a)).min_by(|&a, &b| cheaper(a, b));
        match victim {
            Some(a) if cheaper(a, v) == std::cmp::Ordering::Less => {
                loc[v] = loc[a];
                spilled.push(a);
                active.retain(|&x| x != a);
                active.push(v);
            }
            _ => spilled.push(v),
        }
    }
    // A value spilled for another one gives up its register (its slot, assigned below, is
    // where the states read it too).
    for &v in &spilled {
        loc[v] = Loc::None;
    }
    // Home slots of values states read after their register is free.
    let read_by_states = extend_for_states(g, iv, full, &loc);
    let homed: Vec<usize> = (0..n).filter(|&v| matches!(loc[v], Loc::Reg(_) | Loc::Xmm(_)) && full.end[v] > iv.end[v]).collect();
    let mut home = vec![None; n];
    // Parameters stay in their frame slots, which optimized code never writes: those are their
    // homes, and spilled parameters are read from there.
    let param = |v: usize| match g.values[v].inst {
        Inst::Param(slot) => Some(Loc::Frame(slot as i32)),
        _ => None,
    };
    let (homed, spilled): (Vec<usize>, Vec<usize>) = {
        let mut spilled = spilled;
        for &v in &homed {
            home[v] = param(v);
        }
        for &v in &spilled {
            if let Some(f) = param(v) {
                loc[v] = f;
            }
        }
        spilled.retain(|&v| param(v).is_none());
        (homed.into_iter().filter(|&v| home[v].is_none()).collect(), spilled)
    };
    // Spill and home slots, reused once their (full) interval has ended.
    let mut slotted: Vec<(usize, bool)> = spilled.into_iter().map(|v| (v, false)).chain(homed.into_iter().map(|v| (v, true))).collect();
    slotted.sort_by_key(|&(v, _)| full.start[v]);
    let mut slot_end: Vec<u32> = Vec::new();
    for (v, is_home) in slotted {
        // A spilled copy (`split.rs` cuts a value at a loop) of a value already in a slot takes
        // that slot: it holds the same value, which nothing rewrites (no phi), and the copy
        // moves nothing. The source is live where the copy starts, so its slot is not reused
        // before; it stays taken until the copy ends too.
        if let (false, Inst::Copy(x)) = (is_home, &g.values[v].inst) {
            let x = g.resolve(*x) as usize;
            let held = match (loc[x], home[x]) {
                (Loc::Stack(s), _) | (_, Some(Loc::Stack(s))) => Some(Loc::Stack(s)),
                (Loc::Frame(s), _) | (_, Some(Loc::Frame(s))) => Some(Loc::Frame(s)),
                _ => None,
            };
            if let (Some(at), false) = (held, matches!(g.values[x].inst, Inst::Phi(_))) {
                if let Loc::Stack(s) = at {
                    slot_end[s as usize] = slot_end[s as usize].max(full.end[v]);
                }
                loc[v] = at;
                continue;
            }
        }
        let slot = match slot_end.iter().position(|&e| e < full.start[v]) {
            Some(s) => s,
            None => {
                slot_end.push(0);
                slot_end.len() - 1
            }
        };
        slot_end[slot] = full.end[v];
        if is_home {
            home[v] = Some(Loc::Stack(slot as i32));
        } else {
            loc[v] = Loc::Stack(slot as i32);
        }
    }
    (loc, slot_end.len() as i32, home, read_by_states)
}
