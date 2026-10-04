//! Shared slow-path trampolines of baseline code. A deferral site (an instruction re-executed
//! in the interpreter) is `mov ecx, bci | depth << 16; call T`: the trampoline `T` holds the
//! frame-dependent part (thread, method, locals, linking inlined frames, the helper call and
//! the exception check) once per method and inlining context instead of once per site.

use super::asm::{Alu, Cond, Label, R};
use super::compile::Emitter;
use super::inline::{link_scopes, unlink_scopes};
use crate::method::Method;

/// What a trampoline's code depends on.
#[derive(PartialEq, Eq)]
pub(super) struct Key {
    helper: usize,
    m: *const Method,
    local_base: i32,
    exc: Label,
    scopes: Vec<i32>,
}

impl Emitter {
    /// Calls `helper(thread, m, locals, bci | depth << 16)` for the current body and jumps to
    /// exception dispatch if it returns nonzero.
    pub(super) fn call_trampoline(&mut self, bci: usize, depth: u16, helper: usize) {
        let key = Key { helper, m: self.m, local_base: self.local_base, exc: self.exc, scopes: self.vis_scopes.clone() };
        let l = match self.tramps.iter().find(|(k, _)| *k == key) {
            Some(&(_, l)) => l,
            None => {
                let l = self.a.new_label();
                self.tramps.push((key, l));
                l
            }
        };
        debug_assert!(bci <= 0xffff);
        self.a.mov_ri(R::Rcx, (bci as i64) | (depth as i64) << 16);
        self.a.call_label(l);
    }

    /// Emits the trampolines the body used. Each runs with one extra return address on the
    /// stack: it re-aligns for the helper and drops it before exception dispatch.
    pub(super) fn emit_trampolines(&mut self) {
        for (k, l) in std::mem::take(&mut self.tramps) {
            let a = &mut self.a;
            a.bind(l);
            a.mov_rr(true, R::Rdi, R::R12);
            a.mov_ri(R::Rsi, k.m as i64);
            a.lea(R::Rdx, super::compile::slot(k.local_base));
            if !k.scopes.is_empty() {
                a.mov_rr(false, R::Rax, R::Rcx);
                link_scopes(a, &k.scopes);
                a.mov_rr(false, R::Rcx, R::Rax);
            }
            a.alu_ri(true, Alu::Sub, R::Rsp, 8);
            a.call_abs(k.helper);
            a.alu_ri(true, Alu::Add, R::Rsp, 8);
            unlink_scopes(a, &k.scopes);
            let raise = a.new_label();
            a.test_rr(true, R::Rax, R::Rax);
            a.jcc(Cond::Ne, raise);
            a.ret();
            a.bind(raise);
            a.alu_ri(true, Alu::Add, R::Rsp, 8);
            a.jmp(k.exc);
        }
    }
}
