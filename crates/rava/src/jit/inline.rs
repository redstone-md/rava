//! Emitting inlined bodies (decisions in `inline_plan.rs`).
//!
//! **Leaf** bodies only read the heap and compute, so they have no side effects. Their locals
//! and stack live in a scratch area above the caller's operand stack (the arguments are copied
//! there). Any slow path inside abandons the attempt and *restarts* the call as an ordinary one,
//! so no frame for the leaf is ever observable.
//!
//! **Framed** bodies run in a real interpreter frame at the arguments, exactly where a call would
//! put it: the header is written and linked from `Thread::top` without a native call. Deferred
//! instructions, slow paths, stack traces and the collector see a normal chain of frames. An
//! inlined body has no exception handlers: an exception unlinks its frame and continues in the
//! caller's dispatch, whose bci is the call site.

use super::analysis::{analyze, Analysis};
use super::asm::{mem, Asm, Label, R};
use super::compile::{slot, Emitter};
use super::inline_plan::{leaf_slots, Kind};
use super::vstack::Slot;
use crate::classfile::ClassFile;
use crate::klass::Klass;
use crate::method::Method;
use crate::thread::{header, offsets};

/// Where an inlined body returns to.
#[derive(Clone, Copy)]
pub(super) struct InlineCtx {
    /// The ordinary call sequence (leaf slow paths, and failed entry checks).
    pub restart: Label,
    pub done: Label,
    /// Absolute slot (from the frame base) receiving the result.
    pub result: i32,
    /// For framed bodies, the absolute slot of their frame header.
    pub frame: Option<i32>,
    /// Whether that frame is visible. Visible inlined frames are linked into `Thread::top`
    /// only around the points that can observe frames (`link_scopes`); hidden frames
    /// (method-handle plumbing, which stack traces and walks leave out) never are.
    pub visible: bool,
}

/// The per-body state swapped while emitting a callee in place.
pub(super) struct Body {
    pub m: &'static Method,
    pub cf: &'static ClassFile,
    pub code: &'static [u8],
    pub an: Analysis,
    pub labels: Vec<Label>,
    pub max_locals: i32,
    pub local_base: i32,
    pub stack_base: i32,
    pub scratch_base: i32,
    pub exc: Label,
    pub inl: Option<InlineCtx>,
    pub vs: Vec<Slot>,
    pub lregs: Vec<Option<R>>,
    pub slow: Vec<super::compile::SlowPath>,
    pub lconst_base: Vec<Option<i64>>,
    pub lconst: Vec<Option<i64>>,
}

/// The checks before an inlined body; any failure takes the ordinary call sequence.
#[derive(Clone, Copy)]
pub(super) enum Entry {
    /// A static callee: its class must be initialized.
    Static(&'static Klass),
    /// A statically bound instance callee: the receiver must not be null.
    Receiver,
    /// A virtual callee: the receiver's vtable entry `index` must be this very method.
    Vtable(i32),
    /// A profiled callee: the receiver must be exactly this class.
    Klass(&'static Klass),
}

impl Emitter {
    /// Whether this body is a leaf (whose slow paths restart the call).
    pub(super) fn in_leaf(&self) -> bool {
        self.inl.is_some_and(|c| c.frame.is_none())
    }

    /// Emits `callee` in place for the call at operand slot `args`, after the entry checks (a
    /// null receiver, an uninitialized class) that branch to the ordinary call sequence, which
    /// the caller emits next. Returns the label to bind after that sequence.
    /// `consts` are the arguments known to be constant; they stay constant in the callee's
    /// locals it never writes.
    pub(super) fn inline_call(&mut self, args: i32, callee: &'static Method, kind: Kind, entry: Entry, consts: &[Option<i64>]) -> Option<Label> {
        let an = analyze(callee)?;
        let info = callee.code?;
        let (restart, done) = (self.a.new_label(), self.a.new_label());
        let unwritten = super::fold::unwritten_locals(callee);
        let lconst_base: Vec<Option<i64>> = (0..info.max_locals as usize).map(|i| consts.get(i).copied().flatten().filter(|_| unwritten[i])).collect();
        // At entry every constant argument is known, written later or not.
        let lconst = (0..info.max_locals as usize).map(|i| consts.get(i).copied().flatten()).collect();
        let known_receiver = consts.first().copied().flatten().is_some_and(|v| v != 0);
        match entry {
            Entry::Static(holder) => self.init_check(holder, restart),
            Entry::Receiver if known_receiver => {}
            Entry::Receiver | Entry::Vtable(_) | Entry::Klass(_) => {
                self.a.load(true, R::Rcx, self.stk(args));
                self.a.test_rr(true, R::Rcx, R::Rcx);
                self.a.jcc(super::asm::Cond::E, restart);
            }
        }
        if let Entry::Klass(k) = entry {
            self.a.mov_ri(R::Rdx, k as *const Klass as i64);
            self.a.alu_rm(false, super::asm::Alu::Cmp, R::Rdx, mem(R::Rcx, 0));
            self.a.jcc(super::asm::Cond::Ne, restart);
        }
        if let Entry::Vtable(index) = entry {
            self.a.load(false, R::Rax, mem(R::Rcx, 0));
            self.a.load(true, R::Rax, mem(R::Rax, std::mem::offset_of!(Klass, vtable_ptr) as i32));
            self.a.mov_ri(R::Rdx, callee as *const Method as i64);
            self.a.alu_rm(true, super::asm::Alu::Cmp, R::Rdx, mem(R::Rax, 8 * index));
            self.a.jcc(super::asm::Cond::Ne, restart);
        }
        let result = self.stack_base + args;
        let (local_base, frame) = match kind {
            Kind::Leaf => {
                let base = self.scratch_base;
                self.extent = self.extent.max(base + leaf_slots(callee));
                for i in 0..callee.param_slots() as i32 {
                    self.a.load(true, R::Rax, self.stk(args + i));
                    self.a.store(true, slot(base + i), R::Rax);
                }
                (base, None)
            }
            Kind::Framed => {
                let hdr = result + info.max_locals as i32;
                if !callee.is_hidden_frame() {
                    self.a.mov_ri(R::Rax, callee as *const Method as i64);
                    self.a.store(true, slot(hdr + header::METHOD as i32), R::Rax);
                    self.a.store_imm(slot(hdr + header::BCI as i32), 0);
                }
                (result, Some(hdr))
            }
        };
        let visible = frame.is_some() && !callee.is_hidden_frame();
        let stack_base = local_base + info.max_locals as i32 + if frame.is_some() { header::SLOTS as i32 } else { 0 };
        self.extent = self.extent.max(stack_base + info.max_stack as i32 + 2);
        let body = Body {
            m: callee,
            cf: callee.holder.cf.as_ref()?,
            code: info.code,
            labels: (0..=info.code.len()).map(|_| self.a.new_label()).collect(),
            an,
            max_locals: info.max_locals as i32,
            local_base,
            stack_base,
            scratch_base: stack_base + info.max_stack as i32 + 2,
            exc: self.a.new_label(),
            inl: Some(InlineCtx { restart, done, result, frame, visible }),
            vs: Vec::new(),
            lregs: Vec::new(),
            slow: Vec::new(),
            lconst_base,
            lconst,
        };
        self.inlined_bytes += info.code.len();
        let caller = self.swap_body(body);
        self.chain.push((callee, consts.first().copied().flatten()));
        if let (Some(hdr), true) = (frame, visible) {
            self.vis_scopes.push(hdr);
        }
        let ok = self.emit_body(None);
        if visible {
            self.vis_scopes.pop();
        }
        self.chain.pop();
        if ok.is_some() && frame.is_some() {
            self.emit_slow_paths();
            // Exceptions leave the inlined frame and continue in the caller's dispatch.
            self.a.bind(self.exc);
            self.a.jmp(caller.exc);
        }
        self.swap_body(caller);
        self.a.bind(restart);
        if ok.is_none() {
            self.failed = true;
        }
        Some(done)
    }

    /// Links the visible inlined frames around the current point into `Thread::top`, outermost
    /// first (before anything that may walk the stack: a call, a deferred instruction, an
    /// allocation that may throw). Their headers already hold method and bci.
    pub(super) fn link_scopes(&mut self) {
        link_scopes(&mut self.a, &self.vis_scopes);
    }

    /// Undoes `link_scopes` (preserves rax).
    pub(super) fn unlink_scopes(&mut self) {
        unlink_scopes(&mut self.a, &self.vis_scopes);
    }

    pub(super) fn swap_body(&mut self, b: Body) -> Body {
        use std::mem::replace;
        Body {
            m: replace(&mut self.m, b.m),
            cf: replace(&mut self.cf, b.cf),
            code: replace(&mut self.code, b.code),
            an: replace(&mut self.an, b.an),
            labels: replace(&mut self.labels, b.labels),
            max_locals: replace(&mut self.max_locals, b.max_locals),
            local_base: replace(&mut self.local_base, b.local_base),
            stack_base: replace(&mut self.stack_base, b.stack_base),
            scratch_base: replace(&mut self.scratch_base, b.scratch_base),
            exc: replace(&mut self.exc, b.exc),
            inl: replace(&mut self.inl, b.inl),
            vs: replace(&mut self.vs, b.vs),
            lregs: replace(&mut self.lregs, b.lregs),
            slow: replace(&mut self.slow, b.slow),
            lconst_base: replace(&mut self.lconst_base, b.lconst_base),
            lconst: replace(&mut self.lconst, b.lconst),
        }
    }

    /// `xreturn` (the value in rax) / `return`: the method epilogue, or the end of an inlined
    /// body.
    pub(super) fn ret(&mut self, value: bool) {
        match self.inl {
            Some(ctx) => {
                if value {
                    self.a.store(true, slot(ctx.result), R::Rax);
                }
                self.a.jmp(ctx.done);
            }
            None => {
                if !value {
                    self.a.alu_rr(false, super::asm::Alu::Xor, R::Rax, R::Rax);
                }
                self.a.jmp(self.epilogue);
            }
        }
    }
}

/// `Emitter::link_scopes` for the frame headers `scopes` (clobbers rcx).
pub(super) fn link_scopes(a: &mut Asm, scopes: &[i32]) {
    if scopes.is_empty() {
        return;
    }
    a.load(true, R::Rcx, mem(R::R12, offsets::TOP));
    for &hdr in scopes {
        a.store(true, slot(hdr + header::CALLER as i32), R::Rcx);
        a.lea(R::Rcx, slot(hdr + header::METHOD as i32));
    }
    a.store(true, mem(R::R12, offsets::TOP), R::Rcx);
}

/// Undoes `link_scopes` (preserves rax).
pub(super) fn unlink_scopes(a: &mut Asm, scopes: &[i32]) {
    let Some(&outer) = scopes.first() else { return };
    a.load(true, R::Rcx, slot(outer + header::CALLER as i32));
    a.store(true, mem(R::R12, offsets::TOP), R::Rcx);
}
