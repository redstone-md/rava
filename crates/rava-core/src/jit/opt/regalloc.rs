//! Linear-scan register allocation over SSA values (Poletto & Sarkar): one contiguous interval
//! per value from block liveness, then a single pass in start order. Values live across a call
//! get callee-saved registers or spill slots on the native stack; constants are never allocated.
//! Spilled values stay in their slot for the whole interval.

use super::ir::*;
use crate::jit::asm::R;

/// Registers holding values across calls, and the ones calls clobber. rax, rcx, rdx and r11 are
/// scratch for code generation; r12 and r13 hold the thread and the frame.
pub const CALLEE_SAVED: [R; 4] = [R::Rbx, R::Rbp, R::R14, R::R15];
pub const CALLER_SAVED: [R; 5] = [R::Rsi, R::Rdi, R::R8, R::R9, R::R10];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Loc {
    /// The value is never read.
    None,
    Reg(R),
    /// Spill slot `n` at `[rsp + 8n]`.
    Stack(i32),
    Const(i64),
}

pub struct Allocation {
    pub loc: Vec<Loc>,
    /// Blocks in code order.
    pub order: Vec<B>,
    pub spill_slots: i32,
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

/// Every value a state reads, callers included.
pub fn state_values(s: &State, out: &mut Vec<V>) {
    out.extend(s.locals.iter().chain(s.stack.iter()).flatten());
    if let Some(c) = &s.caller {
        state_values(c, out);
    }
}

/// The input phi `p` of block `to` takes along the edge from `from`.
pub fn phi_input(g: &Graph, p: V, from: B, to: B) -> V {
    let i = g.blocks[to as usize].preds.iter().position(|&b| b == from).expect("edge into the phi's block");
    match g.inst(p) {
        Inst::Phi(inputs) => inputs[i],
        _ => unreachable!("not a phi"),
    }
}

pub fn phis(g: &Graph, b: B) -> impl Iterator<Item = V> + '_ {
    g.blocks[b as usize].insts.iter().copied().take_while(move |&v| matches!(g.inst(v), Inst::Phi(_)))
}

struct Intervals {
    start: Vec<u32>,
    end: Vec<u32>,
    used: Vec<bool>,
    /// Positions of instructions that clobber caller-saved registers.
    calls: Vec<u32>,
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
    let live_in = liveness(g, &order);
    let mut iv = Intervals { start: vec![u32::MAX; n], end: vec![0; n], used: vec![false; n], calls: Vec::new() };
    for &b in &order {
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
            iv.touch(v, if matches!(g.inst(v), Inst::Phi(_)) { from } else { p });
            let inst = g.inst(v);
            if !matches!(inst, Inst::Phi(_)) {
                inst.inputs().into_iter().for_each(|u| iv.used_at(u, p));
            }
            if let Some(s) = inst.state() {
                sv.clear();
                state_values(s, &mut sv);
                let at = if matches!(inst, Inst::Call(..)) { p + 1 } else { p };
                sv.iter().for_each(|&u| iv.used_at(u, at));
            }
            if matches!(inst, Inst::Call(..)) {
                iv.calls.push(p);
            }
        }
        blk.term.inputs().into_iter().for_each(|u| iv.used_at(u, to - 1));
        if let Term::Deopt(s) = &blk.term {
            sv.clear();
            state_values(s, &mut sv);
            sv.iter().for_each(|&u| iv.used_at(u, to - 1));
        }
    }
    iv.calls.sort_unstable();
    let (loc, spill_slots) = scan(g, &iv);
    Allocation { loc, order, spill_slots }
}

/// Values live on entry to each block (backward dataflow to a fixpoint).
fn liveness(g: &Graph, order: &[B]) -> Vec<Set> {
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
            blk.term.inputs().into_iter().for_each(|u| live.insert(u));
            if let Term::Deopt(s) = &blk.term {
                sv.clear();
                state_values(s, &mut sv);
                sv.iter().for_each(|&u| live.insert(u));
            }
            for &v in blk.insts.iter().rev() {
                live.remove(v);
                let inst = g.inst(v);
                if matches!(inst, Inst::Phi(_)) {
                    continue;
                }
                inst.inputs().into_iter().for_each(|u| live.insert(u));
                if let Some(s) = inst.state() {
                    sv.clear();
                    state_values(s, &mut sv);
                    sv.iter().for_each(|&u| live.insert(u));
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

/// Assigns registers in interval start order, spilling the interval that ends last.
fn scan(g: &Graph, iv: &Intervals) -> (Vec<Loc>, i32) {
    let n = g.values.len();
    let mut loc = vec![Loc::None; n];
    let mut todo = Vec::new();
    for v in 0..n {
        match g.values[v].inst {
            Inst::Const(c) => loc[v] = Loc::Const(c),
            _ if iv.used[v] => todo.push(v),
            _ => {}
        }
    }
    todo.sort_by_key(|&v| (iv.start[v], v));
    let mut active: Vec<usize> = Vec::new();
    let mut spilled = Vec::new();
    for &v in &todo {
        active.retain(|&a| iv.end[a] >= iv.start[v]);
        let crossing = iv.crosses_call(v);
        let taken = |r: R, active: &[usize], loc: &[Loc]| active.iter().any(|&a| loc[a] == Loc::Reg(r));
        let pools: &[&[R]] = if crossing { &[&CALLEE_SAVED] } else { &[&CALLER_SAVED, &CALLEE_SAVED] };
        let free = pools.iter().flat_map(|p| p.iter()).copied().find(|&r| !taken(r, &active, &loc));
        if let Some(r) = free {
            loc[v] = Loc::Reg(r);
            active.push(v);
            continue;
        }
        // Steal from the active interval ending last, if it outlives this one.
        let allowed = |r: R| pools.iter().any(|p| p.contains(&r));
        let victim = active.iter().copied().filter(|&a| matches!(loc[a], Loc::Reg(r) if allowed(r))).max_by_key(|&a| iv.end[a]);
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
    // Spill slots, reused once their interval has ended.
    spilled.sort_by_key(|&v| iv.start[v]);
    let mut slot_end: Vec<u32> = Vec::new();
    for v in spilled {
        let slot = match slot_end.iter().position(|&e| e < iv.start[v]) {
            Some(s) => s,
            None => {
                slot_end.push(0);
                slot_end.len() - 1
            }
        };
        slot_end[slot] = iv.end[v];
        loc[v] = Loc::Stack(slot as i32);
    }
    (loc, slot_end.len() as i32)
}
