//! Out-of-line code of an optimized method: the constant area, saves around calls, and the
//! stubs after the body (slow calls, restores, edge moves, polls, deoptimization).

use super::{scopes, Arg, Gen};
use crate::jit::asm::{mem, Alu, Label, M, R, X};
use crate::jit::opt::ir::*;
use crate::jit::opt::moves;
use crate::jit::opt::regalloc::{phis, Loc, XMMS};
use crate::jit::runtime;

impl Gen<'_> {
    /// A constant in the code's constant area, as a RIP-relative operand.
    pub fn const_mem(&mut self, bits: i64) -> M {
        let a = &mut self.a;
        let l = *self.consts.entry(bits as u64).or_insert_with(|| a.new_label());
        crate::jit::asm::rip(l)
    }

    /// Saves the registers call `v` clobbers that hold values live across it.
    pub fn save_around(&mut self, v: V) {
        for &(l, slot) in self.saves.get(&v).into_iter().flatten() {
            if matches!(slot, Loc::Stack(k) if k >= self.save_base) {
                moves::mov(&mut self.a, slot, l);
            }
        }
    }

    /// Restores what `save_around(v)` saved.
    pub fn restore_around(&mut self, v: V) {
        for &(l, slot) in self.saves.get(&v).into_iter().flatten() {
            moves::mov(&mut self.a, l, slot);
        }
    }

    /// A label that restores call `v`'s saved registers, then goes to `target`.
    pub fn restoring(&mut self, v: V, target: Label) -> Label {
        if self.saves.get(&v).is_none_or(|s| s.is_empty()) {
            return target;
        }
        let l = self.a.new_label();
        self.restore_stubs.push((l, v, target));
        l
    }

    pub(super) fn emit_stubs(&mut self) {
        for c in std::mem::take(&mut self.slow_calls) {
            self.a.bind(c.entry);
            if let Some(s) = &c.link {
                self.link(s, true);
            }
            self.call_preserving(c.addr, &c.args);
            if let Some(f) = c.fail {
                self.a.test_rr(true, R::Rax, R::Rax);
                self.a.jcc(crate::jit::asm::Cond::E, f);
            }
            self.a.jmp(c.back);
        }
        for (l, v, target) in std::mem::take(&mut self.restore_stubs) {
            self.a.bind(l);
            self.restore_around(v);
            self.a.jmp(target);
        }
        while let Some((l, from, to)) = self.edge_stubs.pop() {
            self.a.bind(l);
            self.edge(from, to, false);
        }
        for (stub, back) in std::mem::take(&mut self.polls) {
            self.a.bind(stub);
            self.anchor();
            self.call_preserving(runtime::safepoint as *const () as usize, &[Arg::Thread]);
            self.a.jmp(back);
        }
        let common = self.a.new_label();
        let list = std::mem::take(&mut self.deopt_list);
        for (l, s) in &list {
            self.a.bind(*l);
            self.deopt_stub(s, common);
        }
        if !list.is_empty() {
            self.deopt_common(common);
        }
    }

    /// A deoptimization point: its descriptor (where each frame slot's value is) into rdx, then
    /// the method's common stub.
    fn deopt_stub(&mut self, s: &State, common: Label) {
        use crate::jit::opt::deopt::Remat;
        let scopes = scopes(s);
        let loc_of = |v: V| self.home[v as usize].unwrap_or(self.loc[v as usize]);
        let mut slots = Vec::new();
        let mut remats: Vec<Remat> = Vec::new();
        let mut remat_of: std::collections::HashMap<V, usize> = std::collections::HashMap::new();
        for sc in &scopes {
            let base = Self::stack_base_of(sc);
            let locals = sc.locals.iter().enumerate().map(|(i, v)| (sc.frame + i as i32, v));
            let stack = sc.stack.iter().enumerate().map(|(i, v)| (base + i as i32, v));
            for (slot, &v) in locals.chain(stack).filter_map(|(slot, v)| v.as_ref().map(|v| (slot, v))) {
                let v = self.g.resolve(v);
                match self.g.virtuals.get(&v) {
                    // A replaced object: rebuilt once, its pointer stored into every slot it fills.
                    Some(virt) => match remat_of.get(&v) {
                        Some(&i) => remats[i].slots.push(slot),
                        None => {
                            remat_of.insert(v, remats.len());
                            remats.push(Remat {
                                klass: virt.klass,
                                fields: virt.fields.iter().map(|&(off, ty, fv)| (off, loc_of(self.g.resolve(fv)), ty)).collect(),
                                slots: vec![slot],
                            });
                        }
                    },
                    None => slots.push((slot, loc_of(v), self.g.ty(v) == Ty::I)),
                }
            }
        }
        let frames = scopes.iter().map(|sc| crate::jit::opt::deopt::Frame { method: sc.method, frame: sc.frame, bci: sc.bci, depth: sc.stack.len() as u32 }).collect();
        let desc: &'static crate::jit::opt::deopt::Desc = Box::leak(Box::new(crate::jit::opt::deopt::Desc { frames, slots, remats }));
        // rax: no value lives there (rcx and rdx may hold some, saved by the common part).
        self.a.mov_ri(R::Rax, desc as *const _ as i64);
        self.a.jmp(common);
    }

    /// Saves the allocatable registers where the runtime reads the state's values from, then
    /// finishes the method in the interpreter (`deopt.rs`).
    fn deopt_common(&mut self, common: Label) {
        self.a.bind(common);
        for r in crate::jit::opt::deopt::SAVED {
            self.a.push(r);
        }
        let xbytes = 8 * XMMS.len() as i32;
        self.a.alu_ri(true, Alu::Sub, R::Rsp, xbytes);
        for (i, &x) in XMMS.iter().enumerate() {
            self.a.fstore(true, mem(R::Rsp, 8 * i as i32), X(x));
        }
        self.a.mov_rr(true, R::Rdx, R::Rax);
        // The interpreter resumes the scopes on this frame, linked from its header.
        self.anchor();
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.mov_rr(true, R::Rsi, R::R13);
        self.a.mov_rr(true, R::Rcx, R::Rsp);
        self.a.call_abs(crate::jit::opt::deopt::deopt as *const () as usize);
        self.a.alu_ri(true, Alu::Add, R::Rsp, xbytes + 8 * crate::jit::opt::deopt::SAVED.len() as i32);
        self.float_result_from_rax();
        self.a.jmp(self.epilogue);
    }

    /// Whether `from -> to` goes back in code order (a loop's back edge).
    pub(super) fn back_edge(&self, from: B, to: B) -> bool {
        self.g.blocks[to as usize].order <= self.g.blocks[from as usize].order
    }

    /// Where a conditional jump from `from` to `to` goes: the block itself, or a stub doing the
    /// edge's phi moves and poll first.
    pub(super) fn edge_target(&mut self, from: B, to: B) -> Label {
        if phis(self.g, to).next().is_none() && !self.polls_on(from, to) {
            return self.labels[to as usize];
        }
        let l = self.a.new_label();
        self.edge_stubs.push((l, from, to));
        l
    }

    /// Whether the edge polls: a back edge, unless of a short counted loop.
    pub(super) fn polls_on(&self, from: B, to: B) -> bool {
        self.back_edge(from, to) && !self.short_loops[to as usize]
    }
}
