//! Linear-scan register allocation over SSA values (Poletto & Sarkar): one contiguous interval
//! per value from block liveness, then a single pass in start order. Values live across a call
//! prefer callee-saved registers; in a caller-saved or SSE register, code generation saves them
//! around each such call (`Allocation::saves`). Constants are never allocated. Spilled values
//! stay in their slot for the whole interval.
//!
//! Deoptimization states read values without needing them in registers (C2's debug-info uses):
//! registers are allocated over each value's intervals of real uses only. A value a state reads
//! beyond them gets a home slot, written once at its definition (SSA values never change), and
//! deoptimization reads it from there.

use super::ir::*;
use crate::jit::asm::R;

/// SSE registers for float and double values (all caller-saved; xmm0 and xmm1 are scratch).
pub const XMMS: [u8; 14] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Registers holding values across calls, and the ones calls clobber. rax, rcx, rdx and r11 are
/// scratch for code generation; r12 and r13 hold the thread and the frame.
pub const CALLEE_SAVED: [R; 4] = [R::Rbx, R::Rbp, R::R14, R::R15];
pub const CALLER_SAVED: [R; 5] = [R::Rsi, R::Rdi, R::R8, R::R9, R::R10];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Loc {
    /// The value is never read.
    None,
    Reg(R),
    /// An SSE register (float and double values).
    Xmm(u8),
    /// Spill slot `n` at `[rsp + 8n]`.
    Stack(i32),
    Const(i64),
}

pub struct Allocation {
    pub loc: Vec<Loc>,
    /// Blocks in code order.
    pub order: Vec<B>,
    /// Spill slots, then the save slots of calls.
    pub spill_slots: i32,
    /// For each call, the caller-saved and SSE registers holding values live across it, with
    /// the slot each is kept in meanwhile: its home slot, else one of `save_base..`.
    pub saves: std::collections::HashMap<V, Vec<(Loc, i32)>>,
    pub save_base: i32,
    /// Spill slot a register-allocated value is also stored to at its definition, for states.
    pub home: Vec<Option<i32>>,
}

/// A set of values as a bit vector.
#[derive(Clone, PartialEq)]
struct Set(Vec<u64>);

impl Set {
    fn new(n: usize) -> Set {
        Set(vec![0; n.div_ceil(64)])
    }
    fn insert(&mut self, v: V) {
        self.0[v as usize / 64] |= 1 << (v % 64);
    }
    fn remove(&mut self, v: V) {
        self.0[v as usize / 64] &= !(1 << (v % 64));
    }
    fn union(&mut self, o: &Set) {
        self.0.iter_mut().zip(&o.0).for_each(|(a, b)| *a |= b);
    }
    fn iter(&self) -> impl Iterator<Item = V> + '_ {
        self.0.iter().enumerate().flat_map(|(i, w)| (0..64).filter(move |b| w >> b & 1 == 1).map(move |b| (i * 64 + b) as V))
    }
}

/// Every value a state reads, callers included (aliases resolved).
pub fn state_values(g: &Graph, s: &State, out: &mut Vec<V>) {
    out.extend(s.locals.iter().chain(s.stack.iter()).flatten().map(|&v| g.resolve(v)));
    if let Some(c) = &s.caller {
        state_values(g, c, out);
    }
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
    /// The calls, with their positions.
    call_values: Vec<(u32, V)>,
}

impl Intervals {
    fn touch(&mut self, v: V, p: u32) {
        let i = v as usize;
        self.start[i] = self.start[i].min(p);
        self.end[i] = self.end[i].max(p);
    }
    fn used_at(&mut self, v: V, p: u32) {
        self.used[v as usize] = true;
        self.touch(v, p);
    }
    fn crosses_call(&self, v: usize) -> bool {
        let i = self.calls.partition_point(|&p| p <= self.start[v]);
        self.calls.get(i).is_some_and(|&p| p < self.end[v])
    }
}

pub fn allocate(g: &Graph) -> Allocation {
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
    let full = intervals(g, &order, &pos, &range, true);
    let iv = intervals(g, &order, &pos, &range, false);
    let (mut loc, spill_slots, home) = scan(g, &iv, &full);
    // Registers a call clobbers that hold values live across it.
    let mut saves = std::collections::HashMap::new();
    let mut most = 0;
    for &(p, call) in &iv.call_values {
        // A value with a home slot is already there: it is only restored from it.
        let mut next = 0;
        let live: Vec<(Loc, i32)> = (0..n)
            .filter(|&v| iv.used[v] && iv.start[v] < p && iv.end[v] > p)
            .filter(|&v| matches!(loc[v], Loc::Xmm(_)) || matches!(loc[v], Loc::Reg(r) if CALLER_SAVED.contains(&r)))
            .map(|v| {
                let slot = home[v].unwrap_or_else(|| {
                    next += 1;
                    spill_slots + next - 1
                });
                (loc[v], slot)
            })
            .collect();
        most = most.max(next);
        saves.insert(call, live);
    }
    for v in 0..g.values.len() as V {
        loc[v as usize] = loc[g.resolve(v) as usize];
    }
    let mut home = home;
    for v in 0..g.values.len() as V {
        home[v as usize] = home[g.resolve(v) as usize];
    }
    Allocation { loc, order, spill_slots: spill_slots + most, saves, save_base: spill_slots, home }
}

/// Live intervals from block liveness; with `states`, uses by deoptimization states count
/// (they always do for phis, which get no home slot).
fn intervals(g: &Graph, order: &[B], pos: &[u32], range: &[(u32, u32)], states: bool) -> Intervals {
    let n = g.values.len();
    let live_in = liveness(g, order, states);
    let mut iv = Intervals { start: vec![u32::MAX; n], end: vec![0; n], used: vec![false; n], calls: Vec::new(), call_values: Vec::new() };
    for &b in order {
        let (from, to) = range[b as usize];
        let blk = &g.blocks[b as usize];
        for v in live_in[b as usize].iter() {
            iv.touch(v, from);
        }
        for s in blk.term.successors() {
            for v in live_in[s as usize].iter() {
                iv.touch(v, to);
            }
            for ph in phis(g, s) {
                iv.used_at(phi_input(g, ph, b, s), to);
            }
        }
        let mut sv = Vec::new();
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
                sv.clear();
                state_values(g, s, &mut sv);
                let at = if matches!(inst, Inst::Call(..)) { p + 1 } else { p };
                sv.iter().filter(|&&u| states || is_phi(g, u)).for_each(|&u| iv.used_at(u, at));
            }
            if matches!(inst, Inst::Call(..)) {
                iv.calls.push(p);
                iv.call_values.push((p, v));
            }
        }
        blk.term.inputs().into_iter().for_each(|u| iv.used_at(g.resolve(u), to - 1));
        if let Term::Deopt(s) = &blk.term {
            sv.clear();
            state_values(g, s, &mut sv);
            sv.iter().filter(|&&u| states || is_phi(g, u)).for_each(|&u| iv.used_at(u, to - 1));
        }
    }
    iv.calls.sort_unstable();
    iv
}

fn is_phi(g: &Graph, v: V) -> bool {
    matches!(g.inst(v), Inst::Phi(_))
}


/// Values live on entry to each block (backward dataflow to a fixpoint).
fn liveness(g: &Graph, order: &[B], states: bool) -> Vec<Set> {
    let n = g.values.len();
    let mut live_in = vec![Set::new(n); g.blocks.len()];
    let mut sv = Vec::new();
    loop {
        let mut changed = false;
        for &b in order.iter().rev() {
            let blk = &g.blocks[b as usize];
            let mut live = Set::new(n);
            for s in blk.term.successors() {
                live.union(&live_in[s as usize]);
                for ph in phis(g, s) {
                    live.insert(phi_input(g, ph, b, s));
                }
            }
            blk.term.inputs().into_iter().for_each(|u| live.insert(g.resolve(u)));
            if let Term::Deopt(s) = &blk.term {
                sv.clear();
                state_values(g, s, &mut sv);
                sv.iter().filter(|&&u| states || is_phi(g, u)).for_each(|&u| live.insert(u));
            }
            for &v in blk.insts.iter().rev() {
                live.remove(v);
                let inst = g.inst(v);
                if matches!(inst, Inst::Phi(_)) || g.resolve(v) != v {
                    continue;
                }
                inputs(g, inst).into_iter().for_each(|u| live.insert(u));
                if let Some(s) = inst.state() {
                    sv.clear();
                    state_values(g, s, &mut sv);
                    sv.iter().filter(|&&u| states || is_phi(g, u)).for_each(|&u| live.insert(u));
                }
            }
            if live != live_in[b as usize] {
                live_in[b as usize] = live;
                changed = true;
            }
        }
        if !changed {
            return live_in;
        }
    }
}

/// Assigns registers in interval start order, spilling the interval that ends last. A value
/// may share its first operand's register when that operand dies at its definition.
/// `iv` has the intervals of real uses, `full` those including state uses: a value only states
/// read lives in a spill slot, one read beyond its real uses gets a home slot as well.
fn scan(g: &Graph, iv: &Intervals, full: &Intervals) -> (Vec<Loc>, i32, Vec<Option<i32>>) {
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
    let mut active: Vec<usize> = Vec::new();
    for &v in &todo {
        active.retain(|&a| iv.end[a] >= iv.start[v]);
        let crossing = iv.crosses_call(v);
        let taken = |c: Loc, active: &[usize], loc: &[Loc]| active.iter().any(|&a| loc[a] == c);
        // Candidate registers in preference order: none survives a call but the callee-saved.
        // Across calls, callee-saved registers come first (caller-saved ones cost a save and a
        // restore at each call).
        let cands: Vec<Loc> = match (matches!(g.values[v].ty, Ty::F | Ty::D), crossing) {
            (true, _) => XMMS.iter().map(|&x| Loc::Xmm(x)).collect(),
            (false, true) => CALLEE_SAVED.iter().chain(CALLER_SAVED.iter()).map(|&r| Loc::Reg(r)).collect(),
            (false, false) => CALLER_SAVED.iter().chain(CALLEE_SAVED.iter()).map(|&r| Loc::Reg(r)).collect(),
        };
        let allowed = |c: Loc| cands.contains(&c);
        // Coalescing: the result takes the register of its first operand when that dies here
        // (code generation computes into the first operand's register).
        let first = match g.values[v].inst {
            Inst::Phi(_) => None,
            ref inst => inputs(g, inst).first().copied(),
        };
        let hint = first.filter(|&x| iv.end[x as usize] == iv.start[v]).and_then(|x| {
            let c = loc[x as usize];
            (allowed(c) && !active.iter().any(|&a| a != x as usize && loc[a] == c)).then_some((x as usize, c))
        });
        if let Some((x, c)) = hint {
            loc[v] = c;
            active.retain(|&a| a != x);
            active.push(v);
            continue;
        }
        if let Some(&c) = cands.iter().find(|&&c| !taken(c, &active, &loc)) {
            loc[v] = c;
            active.push(v);
            continue;
        }
        // Steal from the active interval ending last, if it outlives this one.
        let victim = active.iter().copied().filter(|&a| allowed(loc[a])).max_by_key(|&a| iv.end[a]);
        match victim {
            Some(a) if iv.end[a] > iv.end[v] => {
                loc[v] = loc[a];
                spilled.push(a);
                active.retain(|&x| x != a);
                active.push(v);
            }
            _ => spilled.push(v),
        }
    }
    // Home slots of values states read after their register is free.
    let homed: Vec<usize> = (0..n).filter(|&v| matches!(loc[v], Loc::Reg(_) | Loc::Xmm(_)) && full.end[v] > iv.end[v]).collect();
    let mut home = vec![None; n];
    // Spill and home slots, reused once their (full) interval has ended.
    let mut slotted: Vec<(usize, bool)> = spilled.into_iter().map(|v| (v, false)).chain(homed.into_iter().map(|v| (v, true))).collect();
    slotted.sort_by_key(|&(v, _)| full.start[v]);
    let mut slot_end: Vec<u32> = Vec::new();
    for (v, is_home) in slotted {
        let slot = match slot_end.iter().position(|&e| e < full.start[v]) {
            Some(s) => s,
            None => {
                slot_end.push(0);
                slot_end.len() - 1
            }
        };
        slot_end[slot] = full.end[v];
        if is_home {
            home[v] = Some(slot as i32);
        } else {
            loc[v] = Loc::Stack(slot as i32);
        }
    }
    (loc, slot_end.len() as i32, home)
}
