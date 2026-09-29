//! Shared machine-code routines called from compiled code.

use super::asm::{mem, mem_idx, Alu, Asm, Cond, R};
use super::code_cache;
use crate::klass::Klass;
use std::sync::OnceLock;

/// Interface dispatch without leaving generated code (HotSpot's itable stub): with the
/// receiver's class in rdi, the interface in rsi and the method's index in that interface in
/// rdx, returns the implementation in rax, or 0 when the itable is not built yet or has no
/// implementation (the caller then asks the runtime). Clobbers only rax and r11.
pub(super) fn itable_lookup() -> usize {
    static STUB: OnceLock<usize> = OnceLock::new();
    *STUB.get_or_init(|| {
        let mut a = Asm::default();
        let (scan, found, fail) = (a.new_label(), a.new_label(), a.new_label());
        a.load(true, R::Rax, mem(R::Rdi, std::mem::offset_of!(Klass, itable_raw) as i32));
        a.test_rr(true, R::Rax, R::Rax);
        a.jcc(Cond::E, fail);
        a.load(true, R::R11, mem(R::Rax, 0));
        a.bind(scan);
        a.alu_ri(true, Alu::Add, R::Rax, 16);
        a.alu_ri(true, Alu::Sub, R::R11, 1);
        a.jcc(Cond::L, fail);
        a.alu_rm(true, Alu::Cmp, R::Rsi, mem(R::Rax, -8));
        a.jcc(Cond::Ne, scan);
        a.bind(found);
        a.load(true, R::Rax, mem(R::Rax, 0));
        a.load(true, R::Rax, mem_idx(R::Rax, R::Rdx, 8, 0));
        a.ret();
        a.bind(fail);
        a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
        a.ret();
        code_cache::install(&a.finish())
    })
}
