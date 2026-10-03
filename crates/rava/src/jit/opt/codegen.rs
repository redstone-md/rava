//! Machine code from an allocated graph: the frame protocol of the baseline compiler (locals,
//! header and outgoing arguments in the value-stack frame at `r13`, `r12` = thread), with SSA
//! values in registers and spill slots on the native stack. Guards and exceptions leave through
//! deoptimization stubs that write the interpreter state into the frame (`deopt.rs`).

use super::ir::*;
use super::moves;
use super::regalloc::{phi_input, phis, Allocation, Loc, CALLEE_SAVED, CALLER_SAVED, SCRATCH, XMMS};
use crate::jit::asm::{mem, Alu, Asm, Cond, Label, M, R, X};
use crate::jit::runtime;
use crate::method::Method;
use crate::thread::{header, offsets};
use std::collections::HashMap;
use std::rc::Rc;

mod divconst;
mod stubs;

/// The monitor a synchronized method holds while it runs.
#[derive(Clone, Copy)]
pub(super) enum Monitor {
    /// The receiver (instance methods).
    Receiver,
    /// The class's mirror (static methods).
    Mirror(usize),
}

/// An argument of a runtime helper call.
#[derive(Clone, Copy)]
pub enum Arg {
    Thread,
    Imm(i64),
    Val(V),
    /// A scratch register already holding the argument (only rcx or rdx).
    Reg(R),
}

/// A slow path out of line: from `entry`, (with the frame linked for `link`'s state) a runtime
/// call of `addr` on `args`, a jump to `fail` if it returned null, then back to `back` with the
/// result in rax.
pub struct SlowCall {
    pub entry: Label,
    pub back: Label,
    pub addr: usize,
    pub args: Vec<Arg>,
    pub link: Option<Rc<State>>,
    pub fail: Option<Label>,
}

pub(super) struct Gen<'a> {
    pub a: Asm,
    pub g: &'a Graph,
    pub loc: &'a [Loc],
    /// Home slots (`Allocation::home`), written at each value's definition.
    home: &'a [Option<Loc>],
    pub max_locals: i32,
    labels: Vec<Label>,
    pub epilogue: Label,
    deopts: HashMap<*const State, Label>,
    deopt_list: Vec<(Label, Rc<State>)>,
    polls: Vec<(Label, Label)>,
    edge_stubs: Vec<(Label, B, B)>,
    /// Callee-saved registers the prologue saves (the frame, and those allocated).
    saved: Vec<R>,
    /// Bytes below them: spill slots and alignment.
    frame_bytes: i32,
    /// Caller-saved registers holding values somewhere in this method (runtime calls from
    /// slow paths preserve them).
    live_gprs: Vec<R>,
    live_xmms: Vec<u8>,
    /// A synchronized method's monitor and where on the native stack (from rsp) it is kept for
    /// the exit (not in the frame: callees' frames start at the outgoing arguments).
    lock: Option<(Monitor, i32)>,
    /// A leaf that cannot stop, throw, deoptimize or call Java: no frame header is written or
    /// linked (nothing walks the stack while it runs), no poll or stack check is needed.
    frameless: bool,
    /// The method's return type.
    ret: crate::types::BasicType,
    /// The register holding the frame base: r13, or for a frameless body that reads its
    /// parameters once on entry, rsi as the caller passed it (r13 is then left alone, and
    /// neither saved nor restored).
    base: R,
    /// Registers kept around each call (`Allocation::saves`) and their slots; slots from
    /// `save_base` are written before the call, home slots already hold the value.
    saves: &'a HashMap<V, Vec<(Loc, Loc)>>,
    save_base: i32,
    /// Stubs restoring a call's saved registers before a deoptimization: (entry, call, target).
    restore_stubs: Vec<(Label, V, Label)>,
    /// Constants placed after the code for RIP-relative operands.
    consts: HashMap<u64, Label>,
    /// Out-of-line runtime calls: (entry, helper, argument, where to continue with rax).
    /// Runtime calls of slow paths, emitted after the body (`codegen/stubs.rs`).
    pub slow_calls: Vec<SlowCall>,
    /// Value-stack slots of the frame, inlined frames included (published as `Thread::sp`).
    frame_slots: i32,
    /// The method compiled.
    pub root: &'static Method,
    /// Reference stores that need no card mark (`barrier.rs`).
    pub unbarriered: Vec<bool>,
    /// Headers of short counted loops, whose back edges do not poll (`counted.rs`).
    short_loops: Vec<bool>,
}

/// Emits `g` for `m`, whose frame (inlined frames included) takes `frame_slots` value-stack slots;
/// a synchronized method holds `monitor` while it runs. Returns the code and the size of its
/// main part (without the out-of-line stubs after it, as C2 sizes code for inlining).
pub(super) fn generate(g: &Graph, al: &Allocation, m: &'static Method, frame_slots: i32, monitor: Option<Monitor>) -> (Vec<u8>, usize) {
    let max_locals = m.code.expect("compiled method has code").max_locals as i32;
    let mut a = Asm::default();
    a.enable_peephole();
    let labels = (0..g.blocks.len()).map(|_| a.new_label()).collect();
    let epilogue = a.new_label();
    let mut gen = Gen {
        a,
        g,
        loc: &al.loc,
        home: &al.home,
        max_locals,
        labels,
        epilogue,
        deopts: HashMap::new(),
        deopt_list: Vec::new(),
        polls: Vec::new(),
        edge_stubs: Vec::new(),
        saved: Vec::new(),
        frame_bytes: 0,
        live_gprs: CALLER_SAVED.into_iter().chain(SCRATCH).filter(|r| al.loc.contains(&Loc::Reg(*r))).collect(),
        live_xmms: XMMS.into_iter().filter(|x| al.loc.contains(&Loc::Xmm(*x))).collect(),
        lock: None,
        frameless: monitor.is_none() && super::polls::frameless(g),
        base: R::R13,
        ret: m.sig.ret,
        saves: &al.saves,
        save_base: al.save_base,
        restore_stubs: Vec::new(),
        consts: HashMap::new(),
        slow_calls: Vec::new(),
        unbarriered: super::barrier::unbarriered(g),
        short_loops: super::polls::short_loops(g),
        frame_slots,
        root: m,
    };
    // Entry rsp is 8 mod 16: an odd number of pushes realigns it, spill slots come in pairs.
    // r12 holds the thread in all compiled code (Rust enters through `jit::enter_trampoline`).
    let leaf = gen.frameless && reads_params_on_entry(g, al);
    if leaf {
        gen.base = R::Rsi;
    }
    gen.saved = (!leaf).then_some(R::R13).into_iter().chain(CALLEE_SAVED.into_iter().filter(|r| al.loc.contains(&Loc::Reg(*r)))).collect();
    let spill_bytes = 8 * (al.spill_slots + (al.spill_slots & 1));
    gen.lock = monitor.map(|mon| (mon, spill_bytes));
    // A frameless body calls nothing, so needs no call alignment.
    let align = if gen.frameless { 0 } else if gen.saved.len() % 2 == 0 { 8 } else { 0 };
    gen.frame_bytes = spill_bytes + if monitor.is_some() { 16 } else { 0 } + align;
    let overflow = gen.a.new_label();
    gen.prologue(frame_slots, overflow);
    // Blocks that only deoptimize are not laid out: branches into them go straight to the
    // deoptimization stub, so the path that stays in compiled code falls through.
    let order = layout(g, &al.order.iter().copied().filter(|&b| !cold_exit(g, b)).collect::<Vec<B>>());
    for (i, &b) in order.iter().enumerate() {
        gen.a.bind(gen.labels[b as usize]);
        let mut insts = g.blocks[b as usize].insts.clone();
        if leaf && b == 0 {
            // The parameter that lands in rsi is read last: rsi is the frame base until then.
            let n = insts.iter().take_while(|&&v| matches!(g.inst(v), Inst::Param(_))).count();
            insts[..n].sort_by_key(|&v| al.loc[v as usize] == Loc::Reg(R::Rsi));
        }
        for &v in &insts {
            gen.inst(v);
            if let (Some(h @ Loc::Stack(_)), true) = (al.home[v as usize], g.resolve(v) == v && !matches!(g.inst(v), Inst::Phi(_))) {
                moves::mov(&mut gen.a, h, al.loc[v as usize]);
            }
        }
        gen.term(b, order.get(i + 1).copied());
    }
    let stubs = gen.a.new_label();
    gen.a.bind(stubs);
    gen.emit_stubs();
    gen.emit_epilogue(overflow);
    let mut pool: Vec<(u64, Label)> = gen.consts.drain().collect();
    pool.sort_by_key(|&(bits, _)| bits);
    for (bits, l) in pool {
        gen.a.data8(l, bits);
    }
    let (code, main) = gen.a.finish_at(stubs);
    (code, main as usize)
}

impl Gen<'_> {
    pub fn hdr(&self, field: usize) -> M {
        mem(self.base, 8 * (self.max_locals + field as i32))
    }

    /// Frame slot `n` (from the frame base).
    pub fn slot(&self, n: i32) -> M {
        mem(self.base, 8 * n)
    }

    fn prologue(&mut self, frame_slots: i32, overflow: Label) {
        let a = &mut self.a;
        for &r in &self.saved {
            a.push(r);
        }
        if self.frame_bytes > 0 {
            a.alu_ri(true, Alu::Sub, R::Rsp, self.frame_bytes);
        }
        if self.base == R::R13 {
            a.mov_rr(true, R::R13, R::Rsi);
        }
        if self.frameless {
            return;
        }
        a.lea(R::Rax, mem(R::R13, 8 * frame_slots));
        a.alu_rm(true, Alu::Cmp, R::Rax, mem(R::R12, offsets::LIMIT));
        a.jcc(Cond::A, overflow);
        let h = |f: usize| mem(R::R13, 8 * (self.max_locals + f as i32));
        // The frame is linked to its caller (every caller publishes its innermost header as
        // `Thread::top` before a call), but `top` and `sp` are only published here once the
        // method calls out (`link`, `anchor`); see the module comment.
        a.store(true, h(header::METHOD), R::Rdx);
        a.store_imm(h(header::BCI), 0);
        a.load(true, R::Rcx, mem(R::R12, offsets::TOP));
        a.store(true, h(header::CALLER), R::Rcx);
        self.poll();
        if let Some((mon, at)) = self.lock {
            match mon {
                Monitor::Receiver => self.a.load(true, R::Rcx, self.slot(0)),
                Monitor::Mirror(o) => self.a.mov_ri(R::Rcx, o as i64),
            }
            self.a.store(true, mem(R::Rsp, at), R::Rcx);
            self.method_monitor(true);
        }
    }

    /// Enters or exits the synchronized method's monitor in rcx: the thin-lock fast path, else
    /// the runtime.
    fn method_monitor(&mut self, enter: bool) {
        let (slow, done) = (self.a.new_label(), self.a.new_label());
        if enter {
            crate::jit::emit_obj::monitor_enter_fast(&mut self.a, R::Rcx, slow);
        } else {
            crate::jit::emit_obj::monitor_exit_fast(&mut self.a, R::Rcx, slow);
        }
        self.a.jmp(done);
        self.a.bind(slow);
        self.anchor();
        let helper = if enter { runtime::monitor_enter as *const () as usize } else { runtime::monitor_exit as *const () as usize };
        self.call_preserving(helper, &[Arg::Thread, Arg::Reg(R::Rcx)]);
        self.a.bind(done);
    }

    /// For a float or double method, xmm0 := the result in rax (paths that end with it there).
    pub fn float_result_from_rax(&mut self) {
        if matches!(self.ret, crate::types::BasicType::Float | crate::types::BasicType::Double) {
            self.a.mov_xr(true, X(0), R::Rax);
        }
    }

    fn emit_epilogue(&mut self, overflow: Label) {
        let restore = self.a.new_label();
        self.a.bind(self.epilogue);
        if let Some((_, at)) = self.lock {
            // Every exit (return, exception, deoptimization) releases the monitor; the result
            // stays in rax, and an IllegalMonitorStateException replaces it.
            self.a.load(true, R::Rcx, mem(R::Rsp, at));
            self.a.push(R::Rax);
            self.a.push(R::Rax);
            self.method_monitor(false);
            self.a.pop(R::Rax);
            self.a.pop(R::Rax);
            // The runtime call may have clobbered xmm0.
            self.float_result_from_rax();
        }
        if !self.frameless {
            // The caller's frame is innermost again; `sp` is left as published (callers do
            // not rely on it, see the module comment).
            self.a.load(true, R::Rcx, self.hdr(header::CALLER));
            self.a.store(true, mem(R::R12, offsets::TOP), R::Rcx);
        }
        self.a.bind(restore);
        if self.frame_bytes > 0 {
            self.a.alu_ri(true, Alu::Add, R::Rsp, self.frame_bytes);
        }
        for &r in self.saved.iter().rev() {
            self.a.pop(r);
        }
        self.a.ret();
        // The frame was never linked: raise, then return without unlinking.
        self.a.bind(overflow);
        // The caller's frame is innermost; its arguments end where this frame would start.
        self.a.mov_rr(true, R::Rax, R::R13);
        self.raise_sp();
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.call_abs(runtime::stack_overflow as *const () as usize);
        self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
        self.a.jmp(restore);
    }

    // ---- calls into the runtime ----

    /// Calls a Rust helper with the allocatable caller-saved registers preserved (pushed where
    /// the collector's stack scan sees them). The result is in rax.
    pub fn call_preserving(&mut self, addr: usize, args: &[Arg]) {
        const SCRATCH: [R; 3] = [R::R11, R::Rcx, R::Rdx];
        const PARAMS: [R; 3] = [R::Rdi, R::Rsi, R::Rdx];
        for (i, arg) in args.iter().enumerate() {
            match *arg {
                Arg::Val(v) => self.load(SCRATCH[i], v),
                Arg::Reg(r) if r != SCRATCH[i] => self.a.mov_rr(true, SCRATCH[i], r),
                _ => {}
            }
        }
        let gprs = self.live_gprs.clone();
        let xmms = self.live_xmms.clone();
        for &r in &gprs {
            self.a.push(r);
        }
        let pad = gprs.len() % 2 == 1;
        if pad {
            self.a.push(R::Rax);
        }
        let xbytes = (8 * xmms.len() as i32 + 15) & !15;
        if xbytes > 0 {
            self.a.alu_ri(true, Alu::Sub, R::Rsp, xbytes);
            for (i, &x) in xmms.iter().enumerate() {
                self.a.fstore(true, mem(R::Rsp, 8 * i as i32), X(x));
            }
        }
        for (i, arg) in args.iter().enumerate() {
            match *arg {
                Arg::Thread => self.a.mov_rr(true, PARAMS[i], R::R12),
                Arg::Imm(c) => self.a.mov_ri(PARAMS[i], c),
                _ if PARAMS[i] != SCRATCH[i] => self.a.mov_rr(true, PARAMS[i], SCRATCH[i]),
                _ => {}
            }
        }
        self.a.call_abs(addr);
        if xbytes > 0 {
            for (i, &x) in xmms.iter().enumerate() {
                self.a.fload(true, X(x), mem(R::Rsp, 8 * i as i32));
            }
            self.a.alu_ri(true, Alu::Add, R::Rsp, xbytes);
        }
        if pad {
            self.a.pop(R::R11);
        }
        for r in gprs.iter().rev() {
            self.a.pop(*r);
        }
    }

    /// Calls a Rust helper with up to six values as its arguments, the allocatable caller-saved
    /// registers preserved as in `call_preserving` (and the helper must not collect: nothing
    /// links the frame). The result is in rax.
    pub fn call_preserving_vals(&mut self, addr: usize, vals: &[V]) {
        const PARAMS: [R; 6] = [R::Rdi, R::Rsi, R::Rdx, R::Rcx, R::R8, R::R9];
        let gprs = self.live_gprs.clone();
        let xmms = self.live_xmms.clone();
        for &r in &gprs {
            self.a.push(r);
        }
        let pad = gprs.len() % 2 == 1;
        if pad {
            self.a.push(R::Rax);
        }
        let xbytes = (8 * xmms.len() as i32 + 15) & !15;
        if xbytes > 0 {
            self.a.alu_ri(true, Alu::Sub, R::Rsp, xbytes);
            for (i, &x) in xmms.iter().enumerate() {
                self.a.fstore(true, mem(R::Rsp, 8 * i as i32), X(x));
            }
        }
        // Through the stack, so no argument register is written before every value is read;
        // spill slots are rsp-relative, so their offsets follow the pushes.
        let mut below = 8 * (gprs.len() + pad as usize) as i32 + xbytes;
        for &v in vals {
            match self.loc[v as usize] {
                Loc::Stack(n) => self.a.load(true, R::R11, mem(R::Rsp, 8 * n + below)),
                l => moves::mov(&mut self.a, Loc::Reg(R::R11), l),
            }
            self.a.push(R::R11);
            below += 8;
        }
        for i in (0..vals.len()).rev() {
            self.a.pop(PARAMS[i]);
        }
        self.a.call_abs(addr);
        if xbytes > 0 {
            for (i, &x) in xmms.iter().enumerate() {
                self.a.fload(true, X(x), mem(R::Rsp, 8 * i as i32));
            }
            self.a.alu_ri(true, Alu::Add, R::Rsp, xbytes);
        }
        if pad {
            self.a.pop(R::R11);
        }
        for r in gprs.iter().rev() {
            self.a.pop(*r);
        }
    }

    /// Before a call out: stores in the frame header where the call is (its bytecode index, or
    /// for a call from inlined bodies its `InlinedSite`, so stack walks see the inlined frames
    /// hidden ones aside) and publishes the header as `Thread::top`; with `sp`, also the
    /// frame's end as `Thread::sp` (for runtime calls, which may collect or run Java code at
    /// `sp`). Clobbers rax and rcx.
    pub fn link(&mut self, s: &State, sp: bool) {
        let scopes = scopes(s);
        let (outer, inner) = scopes.split_last().expect("a state has a scope");
        let at = self.hdr(header::BCI);
        let frames: Vec<(&'static Method, i32)> = inner.iter().filter(|s| !s.method.is_hidden_frame()).map(|s| (s.method, s.bci as i32)).chain(std::iter::once((outer.method, outer.bci as i32))).collect();
        if frames.len() == 1 {
            self.a.store_imm(at, (outer.bci as i32) << 1);
        } else {
            let site: &'static crate::thread::InlinedSite = Box::leak(Box::new(crate::thread::InlinedSite { frames: frames.into() }));
            self.a.mov_ri(R::Rax, site as *const _ as i64);
            self.a.store(true, at, R::Rax);
        }
        self.a.lea(R::Rcx, self.hdr(header::METHOD));
        self.a.store(true, mem(R::R12, offsets::TOP), R::Rcx);
        if sp {
            self.publish_sp();
        }
    }

    /// Publishes this frame as the innermost one (`Thread::top` and `sp`) for a runtime call
    /// without a state (polls, monitors, deoptimization). Clobbers rax.
    pub fn anchor(&mut self) {
        self.a.lea(R::Rax, self.hdr(header::METHOD));
        self.a.store(true, mem(R::R12, offsets::TOP), R::Rax);
        self.publish_sp();
    }

    /// Raises `Thread::sp` to the end of this frame. Clobbers rax and the flags.
    pub fn publish_sp(&mut self) {
        self.a.lea(R::Rax, self.slot(self.frame_slots));
        self.raise_sp();
    }

    /// `Thread::sp := max(sp, rax)`: `sp` only ever rises while optimized code runs, as code
    /// that called it (baseline code, the interpreter) needs it at or above its own frame's end
    /// when the call returns (it runs Java code at `sp`).
    fn raise_sp(&mut self) {
        let keep = self.a.new_label();
        self.a.alu_rm(true, Alu::Cmp, R::Rax, mem(R::R12, offsets::SP));
        self.a.jcc(Cond::Be, keep);
        self.a.store(true, mem(R::R12, offsets::SP), R::Rax);
        self.a.bind(keep);
    }

    /// Frame slot of operand-stack slot 0 of the scope of `s`.
    pub fn stack_base_of(s: &State) -> i32 {
        s.frame + max_locals(s.method) + header::SLOTS as i32
    }

    /// Polls for a stop-the-world request.
    fn poll(&mut self) {
        let (stub, back) = (self.a.new_label(), self.a.new_label());
        self.a.cmp8_mi(mem(R::R12, offsets::POLL), 0);
        self.a.jcc(Cond::Ne, stub);
        self.a.bind(back);
        self.polls.push((stub, back));
    }

    /// The deoptimization stub of `s` (shared by every guard with that state).
    /// The deoptimization stub an edge into `to` can jump to, if `to` only deoptimizes.
    pub(super) fn cold_target(&mut self, to: B) -> Option<Label> {
        if !cold_exit(self.g, to) {
            return None;
        }
        let Term::Deopt(s) = self.g.blocks[to as usize].term.clone() else { unreachable!() };
        Some(self.deopt(&s))
    }

    pub fn deopt(&mut self, s: &Rc<State>) -> Label {
        if let Some(&l) = self.deopts.get(&Rc::as_ptr(s)) {
            return l;
        }
        let l = self.a.new_label();
        self.deopts.insert(Rc::as_ptr(s), l);
        self.deopt_list.push((l, s.clone()));
        l
    }

    // ---- control flow ----

    fn term(&mut self, b: B, next: Option<B>) {
        match self.g.blocks[b as usize].term.clone() {
            Term::Goto(t) => self.edge(b, t, next == Some(t)),
            Term::If { cmp, ty, a, b: y, then, other, unordered } => {
                if then == other {
                    return self.edge(b, then, next == Some(then));
                }
                let c = if matches!(ty, Ty::F | Ty::D) {
                    // ucomis: unordered sets PF (and ZF, CF), ordered results read as unsigned.
                    let (xa, xb) = (self.xreg(a, X(0)), self.xreg(y, X(1)));
                    self.a.fcmp(ty == Ty::D, xa, xb);
                    let nan = self.edge_target(b, if unordered { then } else { other });
                    self.a.jcc(Cond::P, nan);
                    unsigned(cmp)
                } else {
                    let x = self.reg(a, R::Rax);
                    self.alu_val(ty != Ty::I, Alu::Cmp, x, y);
                    cond(cmp)
                };
                let (c, taken, fall) = if next == Some(then) { (invert(c), other, then) } else { (c, then, other) };
                let l = self.edge_target(b, taken);
                self.a.jcc(c, l);
                self.edge(b, fall, next == Some(fall));
            }
            Term::Switch { key, cases, default } => {
                self.load(R::Rax, key);
                let mut targets = HashMap::new();
                for (k, t) in cases {
                    let l = *targets.entry(t).or_insert_with(|| self.edge_target(b, t));
                    self.a.alu_ri(false, Alu::Cmp, R::Rax, k);
                    self.a.jcc(Cond::E, l);
                }
                self.edge(b, default, next == Some(default));
            }
            Term::Return(v) => {
                if let Some(v) = v {
                    self.load(R::Rax, v);
                    if self.g.ty(v) == Ty::I {
                        self.a.movsxd(R::Rax, R::Rax);
                    }
                    // A float or double result is also in xmm0, where compiled callers take it
                    // (`jit::float_result_stub`).
                    if matches!(self.g.ty(v), Ty::F | Ty::D) {
                        self.xload(X(0), v);
                    }
                }
                self.a.jmp(self.epilogue);
            }
            Term::Deopt(s) => {
                let l = self.deopt(&s);
                self.a.jmp(l);
            }
            Term::None => unreachable!("an unbuilt block in the code order"),
        }
    }

    /// Leaves `from` for `to`: phi moves, a poll on back-edges, then the jump.
    fn edge(&mut self, from: B, to: B, fallthrough: bool) {
        if let Some(l) = self.cold_target(to) {
            return self.a.jmp(l);
        }
        if self.polls_on(from, to) {
            self.poll();
        }
        let moves: Vec<(Loc, Loc)> =
            phis(self.g, to).map(|p| (self.loc[p as usize], self.loc[phi_input(self.g, p, from, to) as usize])).collect();
        moves::parallel(&mut self.a, &moves);
        if !fallthrough {
            self.a.jmp(self.labels[to as usize]);
        }
    }

}

/// The order to emit `order`'s blocks in (the entry first): each block is followed by its most
/// frequent successor not yet placed, so the hot path falls through and the rarer side of a
/// branch jumps (C2's block layout); a block whose successors are all placed is followed by
/// the next unplaced block in build order. Values keep their locations whatever the order.
fn layout(g: &Graph, order: &[B]) -> Vec<B> {
    let mut rank = vec![usize::MAX; g.blocks.len()];
    for (i, &b) in order.iter().enumerate() {
        rank[b as usize] = i;
    }
    let mut placed = vec![false; g.blocks.len()];
    let (mut out, mut seed) = (Vec::with_capacity(order.len()), 0);
    let mut next = order.first().copied();
    while out.len() < order.len() {
        let b = match next {
            Some(b) => b,
            None => {
                while placed[order[seed] as usize] {
                    seed += 1;
                }
                order[seed]
            }
        };
        placed[b as usize] = true;
        out.push(b);
        next = g.blocks[b as usize]
            .term
            .successors()
            .into_iter()
            .filter(|&s| rank[s as usize] != usize::MAX && !placed[s as usize])
            .min_by(|&x, &y| {
                let (fx, fy) = (g.blocks[x as usize].freq, g.blocks[y as usize].freq);
                fy.partial_cmp(&fx).unwrap_or(std::cmp::Ordering::Equal).then(rank[x as usize].cmp(&rank[y as usize]))
            });
    }
    out
}

/// Whether block `b` does nothing but deoptimize (no instructions, no phis): edges into it
/// target the deoptimization stub directly.
fn cold_exit(g: &Graph, b: B) -> bool {
    let blk = &g.blocks[b as usize];
    b != 0 && blk.insts.is_empty() && matches!(blk.term, Term::Deopt(_))
}

/// The scopes of a state, innermost first.
fn scopes(s: &State) -> Vec<&State> {
    let mut out = vec![s];
    while let Some(c) = &out.last().unwrap().caller {
        out.push(c);
    }
    out
}

fn max_locals(m: &Method) -> i32 {
    m.code.map_or(0, |c| c.max_locals as i32)
}

pub(super) fn cond(c: Cmp) -> Cond {
    match c {
        Cmp::Eq => Cond::E,
        Cmp::Ne => Cond::Ne,
        Cmp::Lt => Cond::L,
        Cmp::Ge => Cond::Ge,
        Cmp::Gt => Cond::G,
        Cmp::Le => Cond::Le,
    }
}

/// The condition of `cmp` on flags set by an unsigned-style comparison (ucomisd).
fn unsigned(c: Cmp) -> Cond {
    match c {
        Cmp::Eq => Cond::E,
        Cmp::Ne => Cond::Ne,
        Cmp::Lt => Cond::B,
        Cmp::Ge => Cond::Ae,
        Cmp::Gt => Cond::A,
        Cmp::Le => Cond::Be,
    }
}

pub fn invert(c: Cond) -> Cond {
    // x86 condition codes come in complementary pairs differing in the low bit.
    // SAFETY: `Cond` is `repr(u8)` with all 16 codes.
    unsafe { std::mem::transmute::<u8, Cond>(c as u8 ^ 1) }
}

/// Whether every parameter is read into a register by the entry block's leading instructions
/// (none is used from its frame slot later), so the frame base is needed only there.
fn reads_params_on_entry(g: &Graph, al: &Allocation) -> bool {
    if al.loc.iter().any(|l| matches!(l, Loc::Frame(_))) {
        return false;
    }
    g.blocks.iter().enumerate().filter(|(_, b)| !matches!(b.term, Term::None)).all(|(i, b)| {
        let lead = if i == 0 { b.insts.iter().take_while(|&&v| matches!(g.inst(v), Inst::Param(_))).count() } else { 0 };
        b.insts[lead..].iter().all(|&v| !matches!(g.inst(v), Inst::Param(_)))
    })
}
