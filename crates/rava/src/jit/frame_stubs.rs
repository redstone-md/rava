//! Prologue, epilogue and exception-dispatch code shared by all baseline methods. A method
//! starts with `mov ecx, max_locals | frame_slots << 16; call enter` and leaves through
//! `mov ecx, max_locals; jmp leave`, so the frame linking that is the same for every method is
//! emitted once per process instead of once per method.

use super::asm::{mem, mem_idx, Alu, Asm, Cond, R, X};
use super::compile::{restore_registers, save_registers, FRAME_BIAS};
use super::{code_cache, runtime, tier};
use crate::method::Method;
use crate::thread::{header, offsets};
use std::sync::OnceLock;

/// Entered with rdi = thread, rsi = frame base, rdx = method, ecx = max_locals |
/// frame_slots << 16: saves the callee-saved registers under the method's return address, sets
/// up r12/r13, checks the stack limit, links the frame, polls and (`counted`) counts the
/// invocation toward tier 2. Returns with the frame ready; on overflow it raises and returns
/// from the method itself.
pub(super) fn enter(counted: bool) -> usize {
    static S: [OnceLock<usize>; 2] = [OnceLock::new(), OnceLock::new()];
    *S[counted as usize].get_or_init(|| emit_enter(counted))
}

/// Jumped to with rcx = max_locals and the result in rax: unlinks the frame, restores the
/// registers and returns (`float`: the result also in xmm0).
pub(super) fn leave(float: bool) -> usize {
    static S: [OnceLock<usize>; 2] = [OnceLock::new(), OnceLock::new()];
    *S[float as usize].get_or_init(|| {
        let mut a = Asm::default();
        let h = |f: usize| mem(R::Rcx, 8 * f as i32);
        a.lea(R::Rcx, mem_idx(R::R13, R::Rcx, 8, -FRAME_BIAS));
        a.load(true, R::Rdx, h(header::CALLER));
        a.store(true, mem(R::R12, offsets::TOP), R::Rdx);
        a.load(true, R::Rdx, h(header::SAVED_SP));
        a.store(true, mem(R::R12, offsets::SP), R::Rdx);
        restore_registers(&mut a);
        if float {
            // Compiled callers take a float or double result from xmm0 (`jit::float_result_stub`).
            a.mov_xr(true, X(0), R::Rax);
        }
        a.ret();
        code_cache::install(&a.finish())
    })
}

/// Called from a method's exception label: continues at the handler for the pending exception
/// in this frame, or returns (to unwind the frame).
pub(super) fn dispatch() -> usize {
    static S: OnceLock<usize> = OnceLock::new();
    *S.get_or_init(|| {
        let mut a = Asm::default();
        let unwind = a.new_label();
        a.alu_ri(true, Alu::Sub, R::Rsp, 8);
        a.mov_rr(true, R::Rdi, R::R12);
        a.lea(R::Rsi, mem(R::R13, -FRAME_BIAS));
        a.call_abs(runtime::handle_exception as *const () as usize);
        a.alu_ri(true, Alu::Add, R::Rsp, 8);
        a.test_rr(true, R::Rax, R::Rax);
        a.jcc(Cond::E, unwind);
        // The handler runs in the method's own frame: drop the return address.
        a.alu_ri(true, Alu::Add, R::Rsp, 8);
        a.jmp_r(R::Rax);
        a.bind(unwind);
        a.ret();
        code_cache::install(&a.finish())
    })
}

fn emit_enter(counted: bool) -> usize {
    let mut a = Asm::default();
    let (overflow, poll, polled) = (a.new_label(), a.new_label(), a.new_label());
    // The method's return address is pushed back last, so `ret` pairs with the call.
    a.pop(R::R11);
    save_registers(&mut a);
    a.mov_rr(true, R::R12, R::Rdi);
    a.lea(R::R13, mem(R::Rsi, FRAME_BIAS));
    a.mov_rr(false, R::Rax, R::Rcx);
    a.shift_ri(false, super::asm::Shift::Shr, R::Rax, 16);
    a.lea(R::Rax, mem_idx(R::Rsi, R::Rax, 8, 0));
    a.alu_rm(true, Alu::Cmp, R::Rax, mem(R::R12, offsets::LIMIT));
    a.jcc(Cond::A, overflow);
    a.movzx16_rr(R::Rcx, R::Rcx);
    a.lea(R::Rcx, mem_idx(R::Rsi, R::Rcx, 8, 0));
    let h = |f: usize| mem(R::Rcx, 8 * f as i32);
    a.store(true, h(header::METHOD), R::Rdx);
    a.store_imm(h(header::BCI), 0);
    a.load(true, R::R10, mem(R::R12, offsets::TOP));
    a.store(true, h(header::CALLER), R::R10);
    a.load(true, R::R10, mem(R::R12, offsets::SP));
    a.store(true, h(header::SAVED_SP), R::R10);
    a.lea(R::R10, h(header::METHOD));
    a.store(true, mem(R::R12, offsets::TOP), R::R10);
    a.store(true, mem(R::R12, offsets::SP), R::Rax);
    a.cmp8_mi(mem(R::R12, offsets::POLL), 0);
    a.jcc(Cond::Ne, poll);
    a.bind(polled);
    let done = a.new_label();
    let request = a.new_label();
    let threshold = tier::invocation_threshold().filter(|_| counted);
    if let Some(t) = threshold {
        // From the threshold on, ask once every RETRY counts (as `Emitter::past_threshold`).
        let count = mem(R::Rdx, std::mem::offset_of!(Method, invocations) as i32);
        a.alu_mi(false, Alu::Add, count, 1);
        a.load(false, R::Rax, count);
        a.alu_ri(false, Alu::Cmp, R::Rax, t);
        a.jcc(Cond::Ge, request);
    }
    a.bind(done);
    a.push(R::R11);
    a.ret();
    if threshold.is_some() {
        a.bind(request);
        a.alu_ri(false, Alu::Sub, R::Rax, threshold.unwrap());
        a.alu_ri(false, Alu::And, R::Rax, tier::RETRY - 1);
        a.jcc(Cond::Ne, done);
        a.store(true, mem(R::Rsp, 0), R::R11);
        a.mov_rr(true, R::Rdi, R::Rdx);
        a.call_abs(tier::tier_up as *const () as usize);
        a.load(true, R::R11, mem(R::Rsp, 0));
        a.jmp(done);
    }
    // A safepoint with the frame linked; the method comes back from the frame header.
    a.bind(poll);
    a.store(true, mem(R::Rsp, 0), R::R11);
    a.mov_rr(true, R::Rdi, R::R12);
    a.call_abs(runtime::safepoint as *const () as usize);
    a.load(true, R::R11, mem(R::Rsp, 0));
    a.load(true, R::Rdx, mem(R::R12, offsets::TOP));
    a.load(true, R::Rdx, mem(R::Rdx, 0));
    a.jmp(polled);
    // The frame was never linked: raise, then return from the method without unlinking.
    a.bind(overflow);
    a.mov_rr(true, R::Rdi, R::R12);
    a.call_abs(runtime::stack_overflow as *const () as usize);
    a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
    restore_registers(&mut a);
    a.ret();
    code_cache::install(&a.finish())
}
