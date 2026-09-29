//! Leaf inlining and JDK intrinsics.
//!
//! A call whose target is statically known (static, private, final, `invokespecial`) to a small
//! *leaf* method is compiled in place. A leaf may only read the heap (`getfield`, `getstatic`,
//! array loads) and compute; it has no stores, calls (other than intrinsics), allocation, locks
//! or loops. Its body therefore has no side effects, so any slow path inside it (a null, an
//! index out of bounds, an uninitialized class) abandons the inlined attempt and *restarts* the
//! call as an ordinary one, which then behaves exactly as if nothing had been inlined. No frame
//! is ever observable for the inlined method: it cannot throw, reach a safepoint or allocate.
//!
//! The callee's locals and operand stack live in a scratch area above the caller's operand
//! stack; the arguments are copied there, so the caller's slots stay intact for the restart.

use super::analysis::{analyze, i2, i4, insn_len, Analysis};
use super::asm::{mem, Label, R, X};
use super::compile::Emitter;
use crate::classfile::{ClassFile, Cp};
use crate::klass::{Klass, ACC_FINAL};
use crate::link::Resolved;
use crate::method::Method;
use crate::types::BasicType;
use std::sync::atomic::Ordering;

/// HotSpot's `MaxInlineSize`: always inline leaves this small.
const MAX_INLINE_SIZE: usize = 35;
/// Larger leaves are inlined once the callee itself is hot (cf. `FreqInlineSize`).
const HOT_INLINE_SIZE: usize = 128;
const HOT_CALLEE: u32 = 1000;
/// Scratch slots one inlined body may use.
const MAX_INLINE_SLOTS: usize = 48;

/// Where an inlined body returns to.
#[derive(Clone, Copy)]
pub(super) struct InlineCtx {
    pub restart: Label,
    pub done: Label,
    /// Absolute slot (from the frame base) receiving the result.
    pub result: i32,
}

/// The per-method state swapped when emitting a callee's body in place.
struct Body {
    m: &'static Method,
    cf: &'static ClassFile,
    code: &'static [u8],
    an: Analysis,
    labels: Vec<Label>,
    local_base: i32,
    stack_base: i32,
    inl: Option<InlineCtx>,
}

/// A JDK method compiled to a single instruction (as HotSpot's intrinsics do).
#[derive(Clone, Copy)]
enum Intrinsic {
    /// `roundsd` with the given rounding mode (floor 1, ceil 2, rint 0).
    Round(u8),
    Sqrt,
}

fn intrinsic(m: &Method) -> Option<Intrinsic> {
    let holder = m.holder.name.as_str();
    if !(holder == "java/lang/Math" || holder == "java/lang/StrictMath") || m.desc != "(D)D" {
        return None;
    }
    match m.name.as_str() {
        "sqrt" => Some(Intrinsic::Sqrt),
        _ if !std::arch::is_x86_feature_detected!("sse4.1") => None,
        "floor" => Some(Intrinsic::Round(1)),
        "ceil" => Some(Intrinsic::Round(2)),
        "rint" => Some(Intrinsic::Round(0)),
        _ => None,
    }
}

/// The statically bound target of the call at `pc`, if the call can be inlined.
pub(super) fn inline_target(caller: &'static Method, code: &[u8], pc: usize) -> Option<&'static Method> {
    let op = code[pc];
    let index = u16::from_be_bytes([code[pc + 1], code[pc + 2]]);
    let Some(Resolved::Method(m)) = caller.holder.resolved[index as usize].get().copied() else { return None };
    if m.is_signature_polymorphic() || m.is_abstract() || intrinsic(m).is_some() {
        return None;
    }
    let target = match op {
        0xb8 if m.is_static() => m,
        0xb7 if !m.is_static() => crate::interp::special_target(caller.holder, m),
        0xb6 if !m.is_static() && (m.is_private() || m.access & ACC_FINAL != 0 || m.holder.access & ACC_FINAL != 0) => m,
        _ => return None,
    };
    is_leaf(target).then_some(target)
}

/// Scratch slots needed by an inlined `m`.
pub(super) fn inline_slots(m: &Method) -> i32 {
    m.code.map_or(0, |c| c.max_locals as i32 + c.max_stack as i32)
}

fn is_leaf(m: &'static Method) -> bool {
    let Some(info) = m.code else { return false };
    let limit = if m.invocations.load(Ordering::Relaxed) >= HOT_CALLEE { HOT_INLINE_SIZE } else { MAX_INLINE_SIZE };
    if m.is_native() || m.is_synchronized() || m.is_abstract() || !info.handlers.is_empty() || info.code.len() > limit {
        return false;
    }
    if info.max_locals as usize + info.max_stack as usize > MAX_INLINE_SLOTS {
        return false;
    }
    let (code, resolved) = (info.code, &m.holder.resolved);
    let is = |pc: usize, f: fn(Resolved) -> bool| resolved[u16::from_be_bytes([code[pc + 1], code[pc + 2]]) as usize].get().copied().is_some_and(f);
    let mut pc = 0;
    while pc < code.len() {
        let ok = match code[pc] {
            0x00..=0x11 | 0x15..=0x35 | 0x36..=0x4e | 0x57..=0x98 | 0xac..=0xb1 | 0xbe => true,
            0x12..=0x14 => ldc_is_constant(m, code, pc),
            0x99..=0xa7 | 0xc6 | 0xc7 => i2(code, pc + 1) > 0,
            0xaa | 0xab => switch_targets(code, pc).iter().all(|&t| t > pc),
            0xb2 => is(pc, |r| matches!(r, Resolved::StaticField { .. })),
            0xb4 => is(pc, |r| matches!(r, Resolved::InstanceField { .. })),
            0xb8 => is(pc, |r| matches!(r, Resolved::Method(t) if t.is_static() && intrinsic(t).is_some())),
            _ => false,
        };
        if !ok {
            return false;
        }
        pc += insn_len(code, pc);
    }
    true
}

fn ldc_is_constant(m: &Method, code: &[u8], pc: usize) -> bool {
    let index = if code[pc] == 0x12 { code[pc + 1] as u16 } else { u16::from_be_bytes([code[pc + 1], code[pc + 2]]) };
    let Some(cf) = m.holder.cf.as_ref() else { return false };
    match cf.pool[index as usize] {
        Cp::Int(_) | Cp::Float(_) | Cp::Long(_) | Cp::Double(_) => true,
        Cp::String(_) => matches!(m.holder.resolved[index as usize].get(), Some(Resolved::String(_))),
        _ => false,
    }
}

fn switch_targets(code: &[u8], pc: usize) -> Vec<usize> {
    let p = (pc + 4) & !3;
    let rel = |off: i32| (pc as i64 + off as i64) as usize;
    let mut out = vec![rel(i4(code, p))];
    if code[pc] == 0xaa {
        let n = (i4(code, p + 8) - i4(code, p + 4) + 1) as usize;
        out.extend((0..n).map(|i| rel(i4(code, p + 12 + 4 * i))));
    } else {
        out.extend((0..i4(code, p + 4) as usize).map(|i| rel(i4(code, p + 12 + 8 * i))));
    }
    out
}

impl Emitter {
    /// Emits `callee` in place for the call at operand slot `args`; returns the label to bind
    /// after the ordinary call sequence, which follows as the restart path.
    pub(super) fn inline_call(&mut self, args: i32, callee: &'static Method, receiver: bool, holder: &'static Klass) -> Option<Label> {
        let an = analyze(callee)?;
        let info = callee.code?;
        let (restart, done) = (self.a.new_label(), self.a.new_label());
        if receiver {
            self.a.load(true, R::Rcx, self.stk(args));
            self.a.test_rr(true, R::Rcx, R::Rcx);
            self.a.jcc(super::asm::Cond::E, restart);
        } else {
            self.init_check(holder, restart);
        }
        let base = self.inline_base;
        for i in 0..callee.param_slots() as i32 {
            self.a.load(true, R::Rax, self.stk(args + i));
            self.a.store(true, mem(R::R13, 8 * (base + i)), R::Rax);
        }
        let result = self.stack_base + args;
        let labels = (0..=info.code.len()).map(|_| self.a.new_label()).collect();
        let body = Body {
            m: callee,
            cf: callee.holder.cf.as_ref()?,
            code: info.code,
            an,
            labels,
            local_base: base,
            stack_base: base + info.max_locals as i32,
            inl: Some(InlineCtx { restart, done, result }),
        };
        let saved = self.swap_body(body);
        let mut pc = 0;
        let mut ok = Some(());
        while pc < self.code.len() && ok.is_some() {
            self.a.bind(self.labels[pc]);
            if let Some(depth) = self.an.depth[pc] {
                ok = self.emit(pc, depth);
            }
            pc += insn_len(self.code, pc);
        }
        self.swap_body(saved);
        ok?;
        self.a.bind(restart);
        Some(done)
    }

    fn swap_body(&mut self, b: Body) -> Body {
        Body {
            m: std::mem::replace(&mut self.m, b.m),
            cf: std::mem::replace(&mut self.cf, b.cf),
            code: std::mem::replace(&mut self.code, b.code),
            an: std::mem::replace(&mut self.an, b.an),
            labels: std::mem::replace(&mut self.labels, b.labels),
            local_base: std::mem::replace(&mut self.local_base, b.local_base),
            stack_base: std::mem::replace(&mut self.stack_base, b.stack_base),
            inl: std::mem::replace(&mut self.inl, b.inl),
        }
    }

    /// `xreturn` / `return`: the method epilogue, or the end of an inlined body.
    pub(super) fn ret(&mut self, value: Option<i32>) {
        match (self.inl, value) {
            (Some(ctx), Some(slot)) => {
                self.a.load(true, R::Rax, self.stk(slot));
                self.a.store(true, mem(R::R13, 8 * ctx.result), R::Rax);
                self.a.jmp(ctx.done);
            }
            (Some(ctx), None) => self.a.jmp(ctx.done),
            (None, Some(slot)) => {
                self.a.load(true, R::Rax, self.stk(slot));
                self.a.jmp(self.epilogue);
            }
            (None, None) => {
                self.a.alu_rr(false, super::asm::Alu::Xor, R::Rax, R::Rax);
                self.a.jmp(self.epilogue);
            }
        }
    }

    /// Compiles a call to an intrinsic JDK method in place; false if `m` is not one.
    pub(super) fn try_intrinsic(&mut self, m: &'static Method, args: i32) -> bool {
        let Some(k) = intrinsic(m) else { return false };
        debug_assert_eq!(m.sig.ret, BasicType::Double);
        self.a.fload(true, X(0), self.stk(args));
        match k {
            Intrinsic::Round(mode) => self.a.round_sd(X(0), X(0), mode),
            Intrinsic::Sqrt => self.a.fsqrt(X(0), X(0)),
        }
        self.a.fstore(true, self.stk(args), X(0));
        true
    }
}
