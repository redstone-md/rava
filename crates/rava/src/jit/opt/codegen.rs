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
mod fastexit;
mod params;
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
    /// The registers to keep around the call (`Allocation::live_at`).
    pub preserve: super::regalloc::Preserve,
    /// Where the code after `back` expects the helper's result (it returns it in rax).
    pub result: R,
}

pub(super) struct Gen<'a> {
    pub a: Asm,
    pub g: &'a Graph,
    pub loc: &'a [Loc],
    /// Home slots (`Allocation::home`), written at each value's definition.
    pub(super) home: &'a [Option<Loc>],
    pub max_locals: i32,
    labels: Vec<Label>,
    pub epilogue: Label,
    deopts: HashMap<*const State, Label>,
    deopt_list: Vec<(Label, Rc<State>)>,
    polls: Vec<(Label, Label, super::regalloc::Preserve)>,
    /// The registers runtime calls emitted now must keep (`Allocation::live_at`).
    pub preserve: super::regalloc::Preserve,
    live_at: &'a HashMap<V, super::regalloc::Preserve>,
    live_at_end: &'a [super::regalloc::Preserve],
    edge_stubs: Vec<(Label, B, B)>,
    /// Callee-saved registers the prologue saves (the frame, and those allocated).
    saved: Vec<R>,
    /// Bytes below them: spill slots and alignment.
    frame_bytes: i32,
    /// The body's rsp is 8 mod 16 (a frameless body, not realigned on entry): a helper call
    /// pushes one more slot to reach the 16-byte alignment the callee expects.
    body_misaligned: bool,
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
    /// The prologue published the frame as `Thread::top`, as it stays across calls (each callee
    /// restores it on return or unwind): Java calls then only write the BCI.
    top_published: bool,
    /// A method that calls no Java code writes its frame header (method, caller) only when a
    /// slow path first publishes the frame, and its exit unlinks only a published frame: the
    /// path that stays in compiled code never touches the header or `Thread::top`.
    lazy_header: bool,
    /// Parameters that arrive in registers at the register entry (`jit::abi`), placed there
    /// by the prologue: (value, register, frame slot).
    pub(super) reg_params: Vec<(V, crate::jit::abi::ArgReg, u16)>,
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
    /// Sign and magnitude masks, 16 bytes each (`mask_mem`).
    masks: HashMap<u64, Label>,
    /// Out-of-line runtime calls: (entry, helper, argument, where to continue with rax).
    /// Runtime calls of slow paths, emitted after the body (`codegen/stubs.rs`).
    pub slow_calls: Vec<SlowCall>,
    /// Value-stack slots of the frame, inlined frames included (published as `Thread::sp`).
    frame_slots: i32,
    /// The method compiled.
    pub root: &'static Method,
    /// For a version entered from a running loop (`osr.rs`): the loop header's bci.
    pub osr: Option<u32>,
    /// Reference stores that need no card mark (`barrier.rs`).
    pub unbarriered: Vec<bool>,
    /// Headers of short counted loops, whose back edges do not poll (`counted.rs`).
    short_loops: Vec<bool>,
    /// Null checks left to the access right after them (`implicit_null_checks`), the
    /// deoptimization stub of the one being emitted, and the faulting instructions with theirs.
    pub(super) implicit: std::collections::HashSet<V>,
    /// References kept compressed in their register (`narrow_refs`).
    pub(super) narrow: std::collections::HashSet<V>,
    pub(super) pending_null: Option<Label>,
    /// The instructions after the current one in its block (for `deferred_decode`).
    pub(super) next: [Option<V>; 2],
    /// A compressed reference loaded into its register and not yet decoded, and the
    /// instruction that uses it as a base right after (`[r*8 + d]`); decoded after that one,
    /// so the shift is off the chain of dependent loads.
    pub(super) deferred_decode: Vec<(V, R, V)>,
    /// Loop headers whose test back edges repeat (`rotations`), with their successor in the loop.
    rotations: HashMap<B, B>,
    faults: Vec<(Label, Label)>,
    /// `instanceof` tests of final classes and class-word loads emitted as their block's branch
    /// (`fused_type_tests`).
    pub(super) fused: std::collections::HashSet<V>,
    /// Casts whose block then branches on the result being null: the cast's own null test
    /// takes the branch's null side (block, null successor, non-null successor), and the branch
    /// tests nothing.
    pub(super) cast_branch: HashMap<V, (B, B, B)>,
    /// Values the code uses as arrays (`arrays`): an Unsafe access based on one is in the heap.
    pub(super) arrays: std::collections::HashSet<V>,
}

/// Loop rotation (C2's loop inversion, without copying code): a loop header that only merges
/// values (phis) and tests them sends its back edges through a copy of its test, which
/// branches straight to the body; an iteration then costs one taken branch instead of a jump
/// back plus the header's untaken one. Only where neither successor has phis of its own (the
/// copied branch skips the header's edges into them). Maps each such header to its successor in
/// the loop.
fn rotations(g: &Graph) -> HashMap<B, B> {
    let mut out = HashMap::new();
    for l in super::dom::Dom::new(g).loops(g) {
        let h = &g.blocks[l.header as usize];
        if !h.insts.iter().all(|&v| matches!(g.inst(v), Inst::Phi(_) | Inst::Const(_))) {
            continue;
        }
        let Term::If { ty: Ty::I | Ty::J | Ty::A, then, other, .. } = h.term else { continue };
        let stay = match (l.body[then as usize], l.body[other as usize]) {
            (true, false) => then,
            (false, true) => other,
            _ => continue,
        };
        if phis(g, then).next().is_none() && phis(g, other).next().is_none() {
            out.insert(l.header, stay);
        }
    }
    out
}

/// The null checks whose object the next instruction accesses at a small offset (a field, the
/// array length, a bounds check): a null faults there instead (`jit/fault.rs`). Not where the
/// check has failed before (the recompilation then tests explicitly, as C2 does), nor for a
/// constant.
fn implicit_null_checks(g: &Graph, loc: &[Loc]) -> std::collections::HashSet<V> {
    let mut out = std::collections::HashSet::new();
    for b in &g.blocks {
        for w in b.insts.windows(2) {
            let (v, next) = (w[0], w[1]);
            let Inst::NullCheck(x, s) = g.inst(v) else { continue };
            let o = g.resolve(*x);
            if g.resolve(v) != v || g.resolve(next) != next || matches!(loc[o as usize], Loc::Const(_)) || crate::jit::profile::trapped(s.method, s.bci as usize) {
                continue;
            }
            let same = |y: &V| g.resolve(*y) == o;
            let ok = match g.inst(next) {
                Inst::LoadField(y, off, _) => same(y) && *off < 4096,
                Inst::StoreField(y, off, bt, _) => same(y) && *off < 4096 && *bt != crate::types::BasicType::Boolean,
                Inst::ArrayLength(y) | Inst::BoundsCheck(y, ..) => same(y),
                _ => false,
            };
            if ok {
                out.insert(v);
            }
        }
    }
    out
}

/// Compressed references kept compressed in their register (C2 decodes them in the address of
/// their use): a reference loaded from the heap whose every use follows it in its block as the
/// object of a field load or array length, the value of a reference field store (stored as it
/// is), a null check of it or its block's branch on it being null (a compressed null is 0),
/// with no call, allocation or deoptimization state in between (no collection can see the
/// register, no state reads it).
fn narrow_refs(g: &Graph, al: &Allocation) -> std::collections::HashSet<V> {
    use crate::types::BasicType::Reference;
    let mut out = std::collections::HashSet::new();
    if !crate::heap::COMPRESSED {
        return out;
    }
    // Uses of every value outside instructions (terminators, phi inputs): never narrow.
    let mut escapes = vec![false; g.values.len()];
    // The value a block's branch tests for null, compressed or not alike.
    let null_test = |b: &Block| match b.term {
        Term::If { cmp: Cmp::Eq | Cmp::Ne, ty: Ty::A, a, b: y, .. } => match (g.constant(g.resolve(a)), g.constant(g.resolve(y))) {
            (None, Some(0)) => Some(g.resolve(a)),
            (Some(0), None) => Some(g.resolve(y)),
            _ => None,
        },
        _ => None,
    };
    for b in g.blocks.iter().filter(|b| !matches!(b.term, Term::None)) {
        for u in b.term.inputs() {
            if null_test(b) != Some(g.resolve(u)) {
                escapes[g.resolve(u) as usize] = true;
            }
        }
        for &v in &b.insts {
            if let Inst::Phi(ins) = g.inst(v) {
                ins.iter().for_each(|&u| escapes[g.resolve(u) as usize] = true);
            }
        }
    }
    let mut users: Vec<Vec<(B, usize)>> = vec![Vec::new(); g.values.len()];
    for (bi, b) in g.blocks.iter().enumerate().filter(|(_, b)| !matches!(b.term, Term::None)) {
        for (i, &v) in b.insts.iter().enumerate() {
            if g.resolve(v) != v || matches!(g.inst(v), Inst::Phi(_)) {
                continue;
            }
            for u in g.inst(v).inputs() {
                users[g.resolve(u) as usize].push((bi as B, i));
            }
        }
        // The branch reads after the last instruction.
        if let Some(t) = null_test(b) {
            users[t as usize].push((bi as B, b.insts.len()));
        }
    }
    for (bi, b) in g.blocks.iter().enumerate().filter(|(_, b)| !matches!(b.term, Term::None)) {
        for (i, &v) in b.insts.iter().enumerate() {
            if !matches!(g.inst(v), Inst::LoadField(_, _, Reference) | Inst::LoadIndexed(_, _, Reference)) || g.resolve(v) != v || escapes[v as usize] {
                continue;
            }
            if !matches!(al.loc[v as usize], Loc::Reg(_)) || al.home[v as usize].is_some() || al.state_regs[v as usize] || users[v as usize].is_empty() {
                continue;
            }
            let Some(last) = users[v as usize].iter().map(|&(ub, ui)| (ub == bi as B).then_some(ui)).try_fold(0, |m, u| u.map(|u| m.max(u))) else { continue };
            let base_use = |w: V| match g.inst(w) {
                Inst::LoadField(o, ..) | Inst::ArrayLength(o) | Inst::NullCheck(o, _) => g.resolve(*o) == v,
                Inst::StoreField(o, _, Reference, x) => g.resolve(*x) == v && g.resolve(*o) != v,
                _ => false,
            };
            let quiet = |w: V| {
                let inst = g.inst(w);
                inst.state().is_none() && !matches!(inst, Inst::Call(..) | Inst::Intrinsic(..) | Inst::Monitor(..) | Inst::Phi(_))
            };
            let window = &b.insts[i + 1..last.min(b.insts.len() - 1) + 1];
            let uses_ok = users[v as usize].iter().all(|&(_, ui)| ui == b.insts.len() || base_use(b.insts[ui]));
            let quiet_ok = window.iter().all(|&w| quiet(w) || matches!(g.inst(w), Inst::NullCheck(o, _) if g.resolve(*o) == v));
            if uses_ok && quiet_ok {
                out.insert(v);
            }
        }
    }
    out
}

/// Emits `g` for `m`, whose frame (inlined frames included) takes `frame_slots` value-stack slots;
/// a synchronized method holds `monitor` while it runs. Returns the code and the size of its
/// main part (without the out-of-line stubs after it, as C2 sizes code for inlining).
/// Code, main part's size, implicit null checks' (access, handler) offsets, and with `trace`
/// (`RAVA_JIT_DUMP`) the offset each value's code starts at, described.
/// The code, its main part's length, implicit-null-check fault sites, the trace, and the offset
/// of the register entry (`jit::abi`) when the method has one.
pub(super) fn generate(g: &Graph, al: &Allocation, m: &'static Method, frame_slots: i32, monitor: Option<Monitor>, osr: Option<u32>, trace: bool) -> (crate::jit::asm::Code, usize, Vec<(usize, usize)>, Vec<(usize, String)>, Option<usize>) {
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
        preserve: (0, 0),
        live_at: &al.live_at,
        live_at_end: &al.live_at_end,
        edge_stubs: Vec::new(),
        saved: Vec::new(),
        frame_bytes: 0,
        body_misaligned: false,
        live_gprs: CALLER_SAVED.into_iter().chain(SCRATCH).filter(|r| al.loc.contains(&Loc::Reg(*r))).collect(),
        live_xmms: XMMS.into_iter().filter(|x| al.loc.contains(&Loc::Xmm(*x))).collect(),
        lock: None,
        frameless: monitor.is_none() && osr.is_none() && super::polls::frameless(g),
        top_published: false,
        lazy_header: false,
        base: R::R13,
        ret: m.sig.ret,
        reg_params: Vec::new(),
        saves: &al.saves,
        save_base: al.save_base,
        restore_stubs: Vec::new(),
        consts: HashMap::new(),
        masks: HashMap::new(),
        slow_calls: Vec::new(),
        unbarriered: super::barrier::unbarriered(g),
        short_loops: super::polls::short_loops(g),
        implicit: implicit_null_checks(g, &al.loc),
        narrow: narrow_refs(g, al),
        pending_null: None,
        next: [None; 2],
        deferred_decode: Vec::new(),
        rotations: rotations(g),
        faults: Vec::new(),
        fused: fused_type_tests(g),
        cast_branch: cast_branches(g, &al.loc),
        arrays: arrays(g),
        frame_slots,
        root: m,
        osr,
    };
    // Entry rsp is 8 mod 16: an odd number of pushes realigns it, spill slots come in pairs.
    // r12 holds the thread in all compiled code (Rust enters through `jit::enter_trampoline`).
    gen.lazy_header = !gen.frameless && osr.is_none() && !calls_java(g);
    let leaf = gen.frameless && reads_params_on_entry(g, al);
    if leaf {
        gen.base = R::Rsi;
    }
    gen.saved = (!leaf).then_some(R::R13).into_iter().chain(CALLEE_SAVED.into_iter().filter(|r| al.loc.contains(&Loc::Reg(*r)))).collect();
    let spill_bytes = 8 * (al.spill_slots + (al.spill_slots & 1));
    gen.lock = monitor.map(|mon| (mon, spill_bytes));
    // A frameless body makes no Java call and is not realigned on entry (its hot path stays
    // free), but it may still call a Rust helper (a float remainder's fmod, a saturating float
    // to int conversion, a pure intrinsic, a subtype check): with an even number of saved
    // registers its rsp is 8 mod 16, and each such call pads one more slot (`body_misaligned`).
    // An unaligned helper faults on Windows, whose C runtime spills SSE registers with movaps.
    let align = if gen.frameless { 0 } else if gen.saved.len() % 2 == 0 { 8 } else { 0 };
    gen.body_misaligned = gen.frameless && gen.saved.len() % 2 == 0;
    gen.frame_bytes = spill_bytes + if monitor.is_some() { 16 } else { 0 } + align;
    let overflow = gen.a.new_label();
    if osr.is_some() {
        gen.osr_bridge();
    }
    let reg_entry = gen.register_entry(m, al, leaf, osr.is_none() && monitor.is_none());
    if reg_entry.is_some() {
        gen.fast_exit();
    }
    gen.prologue(frame_slots, overflow);
    // Blocks that only deoptimize are not laid out: branches into them go straight to the
    // deoptimization stub, so the path that stays in compiled code falls through.
    let order = layout(g, &al.order.iter().copied().filter(|&b| !cold_exit(g, b)).collect::<Vec<B>>());
    let mut traced: Vec<(Label, V)> = Vec::new();
    for (i, &b) in order.iter().enumerate() {
        gen.a.bind(gen.labels[b as usize]);
        let mut insts = g.blocks[b as usize].insts.clone();
        if leaf && b == 0 {
            // The parameter that lands in rsi is read last: rsi is the frame base until then.
            let n = insts.iter().take_while(|&&v| matches!(g.inst(v), Inst::Param(_))).count();
            insts[..n].sort_by_key(|&v| al.loc[v as usize] == Loc::Reg(R::Rsi));
        }
        for (k, &v) in insts.iter().enumerate() {
            gen.next = [insts.get(k + 1).copied(), insts.get(k + 2).copied()];
            if trace {
                let at = gen.a.new_label();
                gen.a.bind(at);
                traced.push((at, v));
            }
            let pending = gen.pending_null.is_some();
            gen.preserve = gen.live_at.get(&v).copied().unwrap_or(gen.all_registers());
            gen.inst(v);
            // A load of a reference through another one decodes the first after itself, which
            // may defer its own decode in turn.
            for k in (0..gen.deferred_decode.len()).rev() {
                let (x, r, user) = gen.deferred_decode[k];
                if user == v {
                    gen.deferred_decode.remove(k);
                    if al.loc[v as usize] != Loc::Reg(r) {
                        gen.a.shift_ri(true, crate::jit::asm::Shift::Shl, r, 3);
                    }
                } else {
                    debug_assert!(x == v || matches!(g.inst(v), Inst::NullCheck(..)), "a deferred decode outlives its use");
                }
            }
            // A fused class-word load (and the constants after it) leaves the check to the branch.
            let deferred = gen.fused.contains(&v) && matches!(g.inst(v), Inst::LoadField(..)) || matches!(g.inst(v), Inst::Const(_));
            assert!(!pending || gen.pending_null.is_none() || deferred, "an implicit null check without its access");
            if let (Some(h @ Loc::Stack(_)), true) = (al.home[v as usize], g.resolve(v) == v && !matches!(g.inst(v), Inst::Phi(_))) {
                moves::mov(&mut gen.a, h, al.loc[v as usize]);
            }
        }
        gen.preserve = gen.live_at_end[b as usize];
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
    let mut masks: Vec<(u64, Label)> = gen.masks.drain().collect();
    masks.sort_by_key(|&(bits, _)| bits);
    for (bits, l) in masks {
        gen.a.data16(l, bits);
    }
    let mut marks: Vec<Label> = gen.faults.iter().flat_map(|&(at, to)| [at, to]).collect();
    let nfaults = marks.len();
    marks.extend(traced.iter().map(|&(l, _)| l));
    marks.extend(reg_entry);
    let (code, main, pos) = gen.a.finish_labels(stubs, &marks);
    let reg_entry = reg_entry.map(|_| *pos.last().expect("the register entry's position") as usize);
    let pos = &pos[..pos.len() - reg_entry.is_some() as usize];
    let faults = pos[..nfaults].chunks(2).map(|p| (p[0] as usize, p[1] as usize)).collect();
    // Each value with the bytecode position of the nearest state at or before it.
    let mut last = String::new();
    let trace = traced
        .iter()
        .zip(&pos[nfaults..])
        .map(|(&(_, v), &at)| {
            if let Some(s) = g.inst(v).state() {
                last = format!("{}.{}:{}", s.method.holder.name, s.method.name, s.bci);
            }
            let kind = format!("{:?}", g.inst(v));
            (at as usize, format!("v{v} {} @ {last}", kind.chars().take(60).collect::<String>()))
        })
        .collect();
    (code, main as usize, faults, trace, reg_entry)
}

impl Gen<'_> {
    /// Every caller-saved and SSE register this method allocates.
    fn all_registers(&self) -> super::regalloc::Preserve {
        let g = self.live_gprs.iter().fold(0u16, |m, &r| m | 1 << r as u8);
        let x = self.live_xmms.iter().fold(0u16, |m, &x| m | 1 << x);
        (g, x)
    }

    /// The registers of `preserve` this method allocates, in a fixed order.
    fn preserved(&self) -> (Vec<R>, Vec<u8>) {
        let (g, x) = self.preserve;
        let gprs = self.live_gprs.iter().copied().filter(|&r| g & 1 << r as u8 != 0).collect();
        let xmms = self.live_xmms.iter().copied().filter(|&r| x & 1 << r != 0).collect();
        (gprs, xmms)
    }

    /// Marks the next instruction as the access an implicit null check left its test to.
    pub(super) fn fault_site(&mut self) {
        if let Some(fail) = self.pending_null.take() {
            let at = self.a.new_label();
            self.a.bind(at);
            self.faults.push((at, fail));
        }
    }
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
        if let Some(c) = crate::jit::call_counter() {
            a.alu_mi(true, Alu::Add, crate::jit::asm::abs32(c as *const _ as usize), 1);
        }
        if self.frameless {
            self.place_params();
            return;
        }
        a.lea(R::Rax, mem(R::R13, 8 * frame_slots));
        a.alu_rm(true, Alu::Cmp, R::Rax, mem(R::R12, offsets::LIMIT));
        a.jcc(Cond::A, overflow);
        let h = |f: usize| mem(R::R13, 8 * (self.max_locals + f as i32));
        // The frame is linked to its caller (every caller publishes its innermost header as
        // `Thread::top` before a call), but `top` and `sp` are only published once the method
        // calls out (below for Java calls, else `link`, `anchor`); see the module comment.
        // The BCI slot is written by whatever publishes the frame (`link`, `anchor`), and with
        // a lazy header the whole header.
        if !self.lazy_header {
            a.store(true, h(header::METHOD), R::Rdx);
        }
        // A version entered from a running loop continues a frame its caller already linked.
        if self.osr.is_none() && !self.lazy_header {
            a.load(true, R::Rcx, mem(R::R12, offsets::TOP));
            a.store(true, h(header::CALLER), R::Rcx);
        }
        // A method that calls Java publishes its frame once, not before each call.
        if calls_java(self.g) {
            a.lea(R::Rcx, h(header::METHOD));
            a.store(true, mem(R::R12, offsets::TOP), R::Rcx);
            self.top_published = true;
        }
        // Register arguments go to their places before anything can call out (the poll below
        // keeps every allocated register).
        if !self.reg_params.is_empty() {
            self.place_params();
            self.preserve = self.all_registers();
        }
        // A body without loops or Java calls runs for a bounded time: the polls of its callers
        // bound the time to a safepoint (`polls.rs`). Fallback calls still poll here, so a
        // recursion through them reaches a safepoint at every level.
        if !self.lazy_header || !self.g.fallback_calls.is_empty() || self.g.blocks.iter().any(|b| b.loop_header && !matches!(b.term, Term::None)) {
            self.poll();
        }
        if let Some((mon, at)) = self.lock {
            match mon {
                Monitor::Receiver => self.a.load(true, R::Rcx, self.slot(0)),
                Monitor::Mirror(o) => self.a.mov_ri(R::Rcx, o as i64),
            }
            self.a.store(true, mem(R::Rsp, at), R::Rcx);
            self.method_monitor(true);
        }
    }

    /// The start of a version entered from a running loop: first-tier code jumps here at the
    /// loop's back edge with its frame current in memory (r13 its biased base) and its native
    /// frame as at a call. The bridge calls the body below on that frame, then leaves the method
    /// as first-tier code would (`frame_stubs::leave`) with the result in rax.
    fn osr_bridge(&mut self) {
        let body = self.a.new_label();
        self.a.lea(R::Rsi, mem(R::R13, -crate::jit::compile::FRAME_BIAS));
        self.a.mov_ri(R::Rdx, self.root as *const Method as i64);
        self.a.call_label(body);
        self.a.mov_ri(R::Rcx, self.max_locals as i64);
        let float = matches!(self.ret, crate::types::BasicType::Float | crate::types::BasicType::Double);
        self.a.jmp_abs(crate::jit::frame_stubs::leave(float));
        self.a.bind(body);
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
        // No value is live any more but the result, which the monitor exit keeps itself.
        self.preserve = (0, 0);
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
            let unlinked = self.a.new_label();
            if self.lazy_header {
                self.a.lea(R::Rcx, self.hdr(header::METHOD));
                self.a.alu_rm(true, Alu::Cmp, R::Rcx, mem(R::R12, offsets::TOP));
                self.a.jcc(Cond::Ne, unlinked);
            }
            self.a.load(true, R::Rcx, self.hdr(header::CALLER));
            self.a.store(true, mem(R::R12, offsets::TOP), R::Rcx);
            self.a.bind(unlinked);
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
        let (gprs, xmms) = self.preserved();
        for &r in &gprs {
            self.a.push(r);
        }
        let pad = (gprs.len() + self.body_misaligned as usize) % 2 == 1;
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
        let (gprs, xmms) = self.preserved();
        for &r in &gprs {
            self.a.push(r);
        }
        let pad = (gprs.len() + self.body_misaligned as usize) % 2 == 1;
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
        // A parallel move into the argument registers: a register is written only once no
        // pending move still reads it, and a cycle is broken through r11. Spill slots are
        // rsp-relative, so their offsets follow the pushes.
        let below = 8 * (gprs.len() + pad as usize) as i32 + xbytes;
        let mut pending: Vec<(R, Loc)> = vals.iter().enumerate().map(|(i, &v)| (PARAMS[i], self.loc[v as usize])).filter(|&(d, l)| l != Loc::Reg(d)).collect();
        while !pending.is_empty() {
            let free = pending.iter().position(|&(d, _)| !pending.iter().any(|&(_, l)| l == Loc::Reg(d)));
            match free {
                Some(i) => {
                    let (d, l) = pending.remove(i);
                    match l {
                        Loc::Stack(n) => self.a.load(true, d, mem(R::Rsp, 8 * n + below)),
                        l => moves::mov(&mut self.a, Loc::Reg(d), l),
                    }
                }
                None => {
                    // Every destination is still read: park the first one's value in r11.
                    let d = pending[0].0;
                    self.a.mov_rr(true, R::R11, d);
                    for m in pending.iter_mut().filter(|m| m.1 == Loc::Reg(d)) {
                        m.1 = Loc::Reg(R::R11);
                    }
                }
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

    /// Before a call out: stores in the frame header where the call is (its bytecode index, or
    /// for a call from inlined bodies its `InlinedSite`, so stack walks see the inlined frames
    /// hidden ones aside) and publishes the header as `Thread::top`; with `sp`, also the
    /// frame's end as `Thread::sp` (for runtime calls, which may collect or run Java code at
    /// `sp`). Clobbers rax and r11.
    pub fn link(&mut self, s: &State, sp: bool) {
        let scopes = scopes(s);
        let (outer, inner) = scopes.split_last().expect("a state has a scope");
        let at = self.hdr(header::BCI);
        let frames: Vec<(&'static Method, i32)> = inner.iter().filter(|s| !s.method.is_hidden_frame()).map(|s| (s.method, s.bci as i32)).chain(std::iter::once((outer.method, outer.bci as i32))).collect();
        if frames.len() == 1 {
            self.a.store_imm(at, (outer.bci as i32) << 1);
        } else {
            // In low memory, so the store takes an immediate.
            let site: &'static crate::thread::InlinedSite = crate::low::leak(crate::thread::InlinedSite { frames: frames.into() });
            self.a.store_const(at, site as *const _ as i64, R::Rax);
        }
        // Through r11 and rax only: an allocation's slow path links before it saves the
        // registers, and rcx may hold a value there (`scratch.rs`).
        if !self.top_published {
            self.publish(R::R11, R::Rax);
        }
        if sp {
            self.publish_sp();
        }
    }

    /// Publishes this frame as the innermost one (`Thread::top` and `sp`) for a runtime call
    /// without a state (polls, monitors, deoptimization). Clobbers rax and r11.
    pub fn anchor(&mut self) {
        // No call site to name: the method's entry.
        self.a.store_imm(self.hdr(header::BCI), 0);
        if !self.top_published {
            self.publish(R::Rax, R::R11);
        }
        self.publish_sp();
    }

    /// `Thread::top` := this frame's header (address left in `at`). A lazy header is written
    /// first, unless the frame is already published (by an earlier slow path that returned):
    /// `top` is then this frame, not the caller's. Clobbers `at` and `tmp`.
    fn publish(&mut self, at: R, tmp: R) {
        self.a.lea(at, self.hdr(header::METHOD));
        let done = self.a.new_label();
        if self.lazy_header {
            self.a.alu_rm(true, Alu::Cmp, at, mem(R::R12, offsets::TOP));
            self.a.jcc(Cond::E, done);
            self.a.load(true, tmp, mem(R::R12, offsets::TOP));
            self.a.store(true, self.hdr(header::CALLER), tmp);
            self.a.mov_ri(tmp, self.root as *const Method as i64);
            self.a.store(true, self.hdr(header::METHOD), tmp);
        }
        self.a.store(true, mem(R::R12, offsets::TOP), at);
        self.a.bind(done);
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
        self.polls.push((stub, back, self.preserve));
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
                if let Some(&(_, _, on)) = [a, y].into_iter().find_map(|x| self.cast_branch.get(&self.g.resolve(x))).filter(|(cb, ..)| *cb == b) {
                    // The cast just before already sent null to the other side.
                    return self.edge(b, on, next == Some(on));
                }
                if let Some((x, k)) = [a, y].into_iter().map(|v| self.g.resolve(v)).find(|v| self.fused.contains(v)).and_then(|v| match *self.g.inst(v) {
                    Inst::InstanceOf(x, k) => Some((x, k)),
                    _ => None,
                }) {
                    // `x instanceof k` compared with 0: the test branches by itself.
                    let (yes, no) = if cmp == Cmp::Ne { (then, other) } else { (other, then) };
                    let lno = self.edge_target(b, no);
                    let rx = self.reg(x, R::Rax);
                    self.a.test_rr(true, rx, rx);
                    self.a.jcc(Cond::E, lno);
                    self.a.load(false, R::Rcx, mem(rx, 0));
                    let kk = self.class_operand(k);
                    self.cmp_class(None, kk);
                    self.a.jcc(Cond::Ne, lno);
                    return self.edge(b, yes, next == Some(yes));
                }
                let class_word = [a, y].into_iter().map(|v| self.g.resolve(v)).find(|v| self.fused.contains(v)).and_then(|v| match *self.g.inst(v) {
                    Inst::LoadField(o, 0, _) => Some((v, o)),
                    _ => None,
                });
                let c = if let Some((v, o)) = class_word {
                    // A class guard on the header in place (`fused_type_tests`).
                    let k = [a, y].into_iter().map(|x| self.g.resolve(x)).find(|&x| x != v).and_then(|x| self.g.constant(x)).expect("a class constant");
                    let at = self.field(o, 0);
                    self.fault_site();
                    self.a.alu_mi(false, Alu::Cmp, at, k as i32);
                    cond(cmp)
                } else if matches!(ty, Ty::F | Ty::D) {
                    // ucomis: unordered sets PF (and ZF, CF), ordered results read as unsigned.
                    let xa = self.xreg(a, X(0));
                    self.fcmp_val(ty == Ty::D, xa, y);
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

    /// The test of rotated loop header `h` repeated at the end of a back edge (`rotations`):
    /// on to `stay` in the loop, else to the exit.
    fn rotated(&mut self, h: B, stay: B) {
        let Term::If { cmp, ty, a, b: y, then, other, .. } = self.g.blocks[h as usize].term else { unreachable!("a rotated header tests") };
        let x = self.reg(a, R::Rax);
        self.alu_val(ty != Ty::I, Alu::Cmp, x, y);
        let (c, exit) = if then == stay { (cond(cmp), other) } else { (invert(cond(cmp)), then) };
        let l = self.edge_target(h, stay);
        self.a.jcc(c, l);
        let l = self.edge_target(h, exit);
        self.a.jmp(l);
    }

    /// Leaves `from` for `to`: phi moves, a poll on back-edges, then the jump.
    fn edge(&mut self, from: B, to: B, fallthrough: bool) {
        if let Some(l) = self.cold_target(to) {
            return self.a.jmp(l);
        }
        if self.polls_on(from, to) {
            self.preserve = self.live_at_end[from as usize];
            self.poll();
        }
        let moves: Vec<(Loc, Loc)> =
            phis(self.g, to).map(|p| (self.loc[p as usize], self.loc[phi_input(self.g, p, from, to) as usize])).collect();
        moves::parallel(&mut self.a, &moves);
        if let (Some(&stay), true) = (self.rotations.get(&to), self.back_edge(from, to)) {
            return self.rotated(to, stay);
        }
        if !fallthrough {
            self.a.jmp(self.labels[to as usize]);
        }
    }

}

/// The order to emit `order`'s blocks in (the entry first): each block is followed by its most
/// frequent successor not yet placed, so the hot path falls through and the rarer side of a
/// branch jumps (C2's block layout); a block whose successors are all placed is followed by
/// the most frequent unplaced block (build order among equals), so rarely run code gathers at
/// the end instead of between hot chains. Values keep their locations whatever the order.
fn layout(g: &Graph, order: &[B]) -> Vec<B> {
    let mut rank = vec![usize::MAX; g.blocks.len()];
    for (i, &b) in order.iter().enumerate() {
        rank[b as usize] = i;
    }
    let entry = order.first().copied();
    let mut seeds = order.to_vec();
    seeds.sort_by(|&x, &y| g.blocks[y as usize].freq.partial_cmp(&g.blocks[x as usize].freq).unwrap_or(std::cmp::Ordering::Equal).then(rank[x as usize].cmp(&rank[y as usize])));
    let order = &seeds[..];
    let mut placed = vec![false; g.blocks.len()];
    let (mut out, mut seed) = (Vec::with_capacity(order.len()), 0);
    let mut next = entry;
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

/// `instanceof` tests of a final class whose only reader is their block's branch on them
/// (`if (o instanceof T)`), the last instruction there: the branch tests the class itself
/// instead of materializing a boolean and comparing it with 0.
/// Values read or written as arrays (or allocated as ones): verified bytecode makes them arrays
/// wherever they flow, so an Unsafe access with one as its base addresses an element.
fn arrays(g: &Graph) -> std::collections::HashSet<V> {
    let mut arrays = std::collections::HashSet::new();
    for blk in &g.blocks {
        for &v in &blk.insts {
            match *g.inst(v) {
                Inst::ArrayLength(a) | Inst::LoadIndexed(a, ..) | Inst::StoreIndexed(a, ..) | Inst::BoundsCheck(a, ..) => {
                    arrays.insert(g.resolve(a));
                }
                Inst::NewArray(..) | Inst::NewMulti(..) => {
                    arrays.insert(v);
                }
                _ => {}
            }
        }
    }
    arrays
}

/// Casts (of a final class, a primitive array or a class at a primary depth, whose code tests
/// null inline) that end their block's code before a branch on the result being null, by
/// `Gen::cast_branch`, unless the result is in a register the class test uses.
fn cast_branches(g: &Graph, loc: &[Loc]) -> HashMap<V, (B, B, B)> {
    let mut out = HashMap::new();
    for (bi, blk) in g.blocks.iter().enumerate() {
        let Term::If { cmp: cmp @ (Cmp::Eq | Cmp::Ne), ty: Ty::A, a, b: y, then, other, .. } = blk.term else { continue };
        if then == other {
            continue;
        }
        let (a, y) = (g.resolve(a), g.resolve(y));
        let v = match (g.constant(a), g.constant(y)) {
            (None, Some(0)) => a,
            (Some(0), None) => y,
            _ => continue,
        };
        let Inst::CheckCast(_, k, _) = *g.inst(v) else { continue };
        let inline = matches!(k.kind, crate::klass::KlassKind::TypeArray { .. })
            || matches!(k.kind, crate::klass::KlassKind::Instance) && k.access & crate::klass::ACC_FINAL != 0
            || crate::jit::emit_obj::primary_depth(k).is_some();
        let Some(at) = blk.insts.iter().position(|&x| x == v) else { continue };
        // The result is moved into place before the class test, which loads the class word into
        // rcx and may compare through rdx.
        if matches!(loc[v as usize], Loc::Reg(R::Rcx | R::Rdx)) {
            continue;
        }
        if !inline || g.nonnull_input.contains(&v) || !blk.insts[at + 1..].iter().all(|&x| g.resolve(x) != x || matches!(g.inst(x), Inst::Const(_))) {
            continue;
        }
        let (null, on) = if cmp == Cmp::Eq { (then, other) } else { (other, then) };
        out.insert(v, (bi as B, null, on));
    }
    out
}

fn fused_type_tests(g: &Graph) -> std::collections::HashSet<V> {
    let reads = super::thread::reads(g);
    let mut fused = std::collections::HashSet::new();
    for blk in &g.blocks {
        let last = blk.insts.iter().rev().find(|&&x| g.resolve(x) == x && !matches!(g.inst(x), Inst::Const(_)));
        // A class guard: the class word compared with a class (in low memory), one `cmp` on
        // the object's header.
        if let Term::If { cmp: Cmp::Eq | Cmp::Ne, ty: Ty::J, a, b, .. } = blk.term {
            let (a, b) = (g.resolve(a), g.resolve(b));
            let v = match (g.constant(a), g.constant(b)) {
                (None, Some(k)) if (0..1 << 31).contains(&k) => a,
                (Some(k), None) if (0..1 << 31).contains(&k) => b,
                _ => continue,
            };
            if matches!(g.inst(v), Inst::LoadField(_, 0, crate::types::BasicType::Int)) && last == Some(&v) && reads[v as usize] == 1 {
                fused.insert(v);
            }
            continue;
        }
        let Term::If { cmp: Cmp::Eq | Cmp::Ne, ty: Ty::I, a, b, .. } = blk.term else { continue };
        let (a, b) = (g.resolve(a), g.resolve(b));
        let v = match (g.constant(a), g.constant(b)) {
            (None, Some(0)) => a,
            (Some(0), None) => b,
            _ => continue,
        };
        let Inst::InstanceOf(_, k) = *g.inst(v) else { continue };
        let leaf = matches!(k.kind, crate::klass::KlassKind::Instance) && k.access & crate::klass::ACC_FINAL != 0;
        if leaf && last == Some(&v) && reads[v as usize] == 1 {
            fused.insert(v);
        }
    }
    fused
}

/// Whether the body calls a Java method (an `Inst::Call` left in a reachable block).
/// Whether the method makes Java calls on its usual paths: calls made only when every inlined
/// target's guard failed do not count (their frame is published at the call, `link`).
fn calls_java(g: &Graph) -> bool {
    g.blocks
        .iter()
        .filter(|b| !matches!(b.term, Term::None))
        .flat_map(|b| &b.insts)
        .any(|&v| g.resolve(v) == v && matches!(g.inst(v), Inst::Call(..)) && !g.fallback_calls.contains(&v))
}
