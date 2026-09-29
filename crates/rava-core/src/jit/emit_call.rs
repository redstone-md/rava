//! Call templates: `invoke*` with vtable dispatch, and the method-handle machinery compiled
//! directly — linked `invokedynamic` / signature-polymorphic sites, `invokeBasic`, `linkTo*`.

use super::asm::{mem, Cond, R};
use super::compile::Emitter;
use super::runtime;
use crate::classfile::Cp;
use crate::interp::indy;
use crate::klass::{Klass, ACC_FINAL};
use crate::link::Resolved;
use crate::method::Method;
use crate::natives::mh;
use crate::types::{BasicType, MethodSig};
use std::mem::offset_of;

impl Emitter {
    /// Calls the Method* in rdx with arguments from operand slot `args`; stores the result.
    fn call_rdx(&mut self, pc: usize, args: i32, ret: BasicType) {
        self.set_bci(pc);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.lea(R::Rsi, self.stk(args));
        self.a.call_m(mem(R::Rdx, offset_of!(Method, entry) as i32));
        self.check_pending();
        if ret != BasicType::Void {
            self.a.store(true, self.stk(args), R::Rax);
        }
    }

    /// rdx := implementation of `m` for the receiver in rcx (non-null); `slow` if none.
    fn dispatch(&mut self, pc: usize, m: &'static Method, slow: super::asm::Label) {
        if m.is_private() || m.access & ACC_FINAL != 0 {
            self.a.mov_ri(R::Rdx, m as *const Method as i64);
        } else if m.vtable_index() >= 0 && !m.holder.is_interface() {
            self.a.load(true, R::Rax, mem(R::Rcx, 0));
            self.a.load(true, R::Rax, mem(R::Rax, offset_of!(Klass, vtable_ptr) as i32));
            self.a.load(true, R::Rdx, mem(R::Rax, 8 * m.vtable_index()));
        } else {
            // Inline cache: compare the receiver class with the cached pair, else miss handler.
            let cell = runtime::IcCell::new();
            let (hit, done) = (self.a.new_label(), self.a.new_label());
            self.a.load(true, R::Rdi, mem(R::Rcx, 0));
            self.a.mov_ri(R::Rax, cell as *const runtime::IcCell as i64);
            self.a.load(true, R::Rax, mem(R::Rax, 0));
            self.a.alu_rm(true, super::asm::Alu::Cmp, R::Rdi, mem(R::Rax, 0));
            self.a.jcc(Cond::E, hit);
            self.set_bci(pc);
            self.a.mov_ri(R::Rsi, m as *const Method as i64);
            self.a.mov_ri(R::Rdx, cell as *const runtime::IcCell as i64);
            self.a.call_abs(runtime::ic_miss as *const () as usize);
            self.a.test_rr(true, R::Rax, R::Rax);
            self.a.jcc(Cond::E, slow);
            self.a.mov_rr(true, R::Rdx, R::Rax);
            self.a.jmp(done);
            self.a.bind(hit);
            self.a.load(true, R::Rdx, mem(R::Rax, 8));
            self.a.bind(done);
        }
    }

    pub(super) fn invoke(&mut self, pc: usize, depth: u16, op: u8) {
        let d = depth as i32;
        let index = u16::from_be_bytes([self.code[pc + 1], self.code[pc + 2]]);
        let Some(Resolved::Method(m)) = self.m.holder.resolved[index as usize].get().copied() else {
            return self.defer_unresolved(pc, depth);
        };
        if m.is_signature_polymorphic() {
            let (_, _, desc) = self.cf.member_ref(index).unwrap();
            let sig = indy::site_sig(desc);
            return match (op, m.name.as_str()) {
                (0xb8, _) => self.link_to(pc, depth, m, sig),
                (_, "invokeBasic") => self.invoke_basic(pc, depth, sig),
                _ => self.linked_site(pc, depth, sig, 1),
            };
        }
        if (op == 0xb8) != m.is_static() {
            return self.defer(pc, depth);
        }
        let args = d - m.param_slots() as i32;
        let slow = self.slow_path(pc, depth);
        if op == 0xb8 {
            self.init_check(m.holder, slow);
            self.a.mov_ri(R::Rdx, m as *const Method as i64);
        } else {
            self.a.load(true, R::Rcx, self.stk(args));
            self.a.test_rr(true, R::Rcx, R::Rcx);
            self.a.jcc(Cond::E, slow);
            if op == 0xb7 {
                let target = crate::interp::special_target(self.m.holder, m);
                self.a.mov_ri(R::Rdx, target as *const Method as i64);
            } else {
                self.dispatch(pc, m, slow);
            }
        }
        self.call_rdx(pc, args, m.sig.ret);
    }

    pub(super) fn invokedynamic(&mut self, pc: usize, depth: u16) {
        let index = u16::from_be_bytes([self.code[pc + 1], self.code[pc + 2]]);
        let Cp::InvokeDynamic { nat, .. } = self.cf.pool[index as usize] else { return self.defer(pc, depth) };
        let (_, desc) = self.cf.name_and_type(nat).unwrap();
        self.linked_site(pc, depth, indy::site_sig(desc), 0);
    }

    /// A site the interpreter already linked: call its invoker with the appendix appended.
    fn linked_site(&mut self, pc: usize, depth: u16, sig: &'static MethodSig, receiver: i32) {
        let Some((invoker, appendix)) = indy::linked(self.m, pc) else { return self.defer_unresolved(pc, depth) };
        let d = depth as i32;
        let args = d - sig.arg_slots as i32 - receiver;
        if receiver == 1 {
            let slow = self.slow_path(pc, depth);
            self.a.alu_mi(true, super::asm::Alu::Cmp, self.stk(args), 0);
            self.a.jcc(Cond::E, slow);
        }
        if appendix != 0 {
            // The frame reserves slack slots past max_stack for the trailing appendix.
            self.a.mov_ri(R::Rax, appendix as i64);
            self.a.store(true, self.stk(d), R::Rax);
        }
        self.a.mov_ri(R::Rdx, invoker as *const Method as i64);
        self.call_rdx(pc, args, sig.ret);
    }

    /// `MethodHandle.invokeBasic`: call the receiver's `form.vmentry` target.
    fn invoke_basic(&mut self, pc: usize, depth: u16, sig: &'static MethodSig) {
        let args = depth as i32 - sig.arg_slots as i32 - 1;
        let slow = self.slow_path(pc, depth);
        self.a.load(true, R::Rcx, self.stk(args));
        self.a.test_rr(true, R::Rcx, R::Rcx);
        self.a.jcc(Cond::E, slow);
        self.a.load(true, R::Rax, mem(R::Rcx, indy::mh_form() as i32));
        self.a.load(true, R::Rax, mem(R::Rax, indy::lf_vmentry() as i32));
        self.member_target_rdx();
        self.call_rdx(pc, args, sig.ret);
    }

    /// rdx := the Method* of the MemberName in rax.
    fn member_target_rdx(&mut self) {
        self.a.load(true, R::Rax, mem(R::Rax, mh::mn_method() as i32));
        self.a.load(true, R::Rdx, mem(R::Rax, mh::rmn_vmtarget() as i32));
    }

    /// `MethodHandle.linkTo*(args..., MemberName)`: call the MemberName's target (dispatched on
    /// the receiver for virtual/interface) with the arguments before it.
    fn link_to(&mut self, pc: usize, depth: u16, m: &'static Method, sig: &'static MethodSig) {
        let d = depth as i32;
        let args = d - sig.arg_slots as i32;
        let slow = self.slow_path(pc, depth);
        let kind = m.name.as_str();
        if kind != "linkToStatic" {
            self.a.load(true, R::Rcx, self.stk(args));
            self.a.test_rr(true, R::Rcx, R::Rcx);
            self.a.jcc(Cond::E, slow);
        }
        self.a.load(true, R::Rax, self.stk(d - 1));
        self.member_target_rdx();
        if kind == "linkToVirtual" || kind == "linkToInterface" {
            self.set_bci(pc);
            self.a.load(true, R::Rdi, mem(R::Rcx, 0));
            self.a.mov_rr(true, R::Rsi, R::Rdx);
            self.a.call_abs(runtime::select_linked as *const () as usize);
            self.a.test_rr(true, R::Rax, R::Rax);
            self.a.jcc(Cond::E, slow);
            self.a.mov_rr(true, R::Rdx, R::Rax);
        }
        self.call_rdx(pc, args, sig.ret);
    }
}
