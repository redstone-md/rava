//! Call templates: `invoke*` with vtable dispatch, and the method-handle machinery compiled
//! directly — linked `invokedynamic` / signature-polymorphic sites, `invokeBasic`, `linkTo*`.

use super::asm::{mem, Cond, Label, R};
use super::compile::Emitter;
use super::inline::Entry;
use super::inline_plan::{bound_target, decide};
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
    /// Compiles the call at `pc` to the known `target` in place when it qualifies. Returns the
    /// label to bind after the ordinary call sequence the caller emits next (the restart path).
    /// `consts` overrides the site's constant arguments.
    fn try_inline(&mut self, pc: usize, args: i32, target: &'static Method, entry: Entry, consts: Option<Vec<Option<i64>>>) -> Option<Label> {
        if self.in_leaf() {
            return None;
        }
        let n = target.param_slots() as usize;
        let consts = consts.unwrap_or_else(|| (0..n).map(|i| self.site_consts.get(args as usize + i).copied().flatten()).collect());
        let kind = decide(target, consts.first().copied().flatten(), &self.chain, self.tier, self.inlined_bytes, 1.0);
        if env_once!("RAVA_INLINE_LOG").is_some() {
            let indent = "  ".repeat(self.chain.len());
            eprintln!("[inline] {indent}{} @{pc} -> {target} {}", self.m, if kind.is_some() { "inlined" } else { "call" });
        }
        let kind = kind?;
        self.set_bci(pc);
        self.inline_call(args, target, kind, entry, &consts)
    }

    /// The constant at operand slot `i` of the current call site, if any.
    fn site_const(&self, i: i32) -> Option<i64> {
        self.site_consts.get(i as usize).copied().flatten()
    }

    /// Calls the Method* in rdx with arguments from operand slot `args`; stores the result.
    fn call_rdx(&mut self, pc: usize, args: i32, ret: BasicType) {
        self.set_bci(pc);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.lea(R::Rsi, self.stk(args));
        self.link_scopes();
        self.a.call_m(mem(R::Rdx, offset_of!(Method, entry) as i32));
        self.unlink_scopes();
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
            if self.tier == 1 {
                self.profile_receiver(pc);
            }
            self.a.load(true, R::Rax, mem(R::Rcx, 0));
            self.a.load(true, R::Rax, mem(R::Rax, offset_of!(Klass, vtable_ptr) as i32));
            self.a.load(true, R::Rdx, mem(R::Rax, 8 * m.vtable_index()));
        } else {
            if self.tier == 1 && m.holder.is_interface() {
                self.profile_receiver(pc);
            }
            // Inline cache: compare the receiver class with the cached pair, else miss handler.
            let cell = super::profile::cell(self.m, pc);
            let (hit, done, found) = (self.a.new_label(), self.a.new_label(), self.a.new_label());
            self.a.load(true, R::Rdi, mem(R::Rcx, 0));
            self.a.mov_ri(R::Rax, cell as *const runtime::IcCell as i64);
            self.a.load(true, R::Rax, mem(R::Rax, 0));
            self.a.alu_rm(true, super::asm::Alu::Cmp, R::Rdi, mem(R::Rax, 0));
            self.a.jcc(Cond::E, hit);
            if m.holder.is_interface() {
                // Megamorphic interface sites scan the itable in generated code.
                let learn = self.a.new_label();
                self.a.mov_ri(R::Rax, cell as *const runtime::IcCell as i64);
                self.a.alu_mi(false, super::asm::Alu::Cmp, mem(R::Rax, runtime::IcCell::misses_offset()), runtime::IC_MAX_UPDATES as i32);
                self.a.jcc(Cond::B, learn);
                self.a.mov_ri(R::Rsi, m.holder as *const Klass as i64);
                self.a.mov_ri(R::Rdx, m.index() as i64);
                self.a.call_abs(super::stubs::itable_lookup());
                self.a.test_rr(true, R::Rax, R::Rax);
                self.a.jcc(Cond::Ne, found);
                self.a.bind(learn);
            }
            self.set_bci(pc);
            self.a.mov_ri(R::Rsi, m as *const Method as i64);
            self.a.mov_ri(R::Rdx, cell as *const runtime::IcCell as i64);
            self.a.call_abs(runtime::ic_miss as *const () as usize);
            self.a.test_rr(true, R::Rax, R::Rax);
            self.a.jcc(Cond::E, slow);
            self.a.bind(found);
            self.a.mov_rr(true, R::Rdx, R::Rax);
            self.a.jmp(done);
            self.a.bind(hit);
            self.a.load(true, R::Rdx, mem(R::Rax, 8));
            self.a.bind(done);
        }
    }

    /// Records the class of the receiver in rcx in the call's profile (HotSpot's
    /// `VirtualCallData`), for the optimizing tier's guarded inlining: two classes are kept,
    /// a third marks the site megamorphic. Only unseen classes leave the inline checks.
    fn profile_receiver(&mut self, pc: usize) {
        let cell = super::profile::cell(self.m, pc);
        let (other, done) = (self.a.new_label(), self.a.new_label());
        self.a.load(true, R::Rdi, mem(R::Rcx, 0));
        self.a.mov_ri(R::Rax, cell as *const runtime::IcCell as i64);
        for i in 0..2 {
            let next = self.a.new_label();
            self.a.alu_rm(true, super::asm::Alu::Cmp, R::Rdi, mem(R::Rax, runtime::IcCell::seen_offset() + 8 * i));
            self.a.jcc(Cond::Ne, next);
            self.a.alu_mi(false, super::asm::Alu::Add, mem(R::Rax, runtime::IcCell::counts_offset() + 4 * i), 1);
            self.a.jmp(done);
            self.a.bind(next);
        }
        self.a.bind(other);
        self.a.alu_mi(false, super::asm::Alu::Add, mem(R::Rax, runtime::IcCell::others_offset()), 1);
        self.a.cmp8_mi(mem(R::Rax, runtime::IcCell::more_offset()), 0);
        self.a.jcc(Cond::Ne, done);
        self.a.push(R::Rcx);
        self.a.push(R::Rcx);
        self.a.mov_rr(true, R::Rsi, R::Rax);
        self.a.call_abs(runtime::note_receiver as *const () as usize);
        self.a.pop(R::Rcx);
        self.a.pop(R::Rcx);
        self.a.bind(done);
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
        if (op == 0xb8 || op == 0xb6) && self.try_intrinsic(pc, depth, m, args) {
            return;
        }
        let entry = if op == 0xb8 { Entry::Static(m.holder) } else { Entry::Receiver };
        let fast_alloc = self.allocate_instance(m, args, self.site_const(args + 1));
        let inlined = match bound_target(self.m, self.code, pc) {
            _ if fast_alloc.is_some() => fast_alloc,
            Some(t) => self.try_inline(pc, args, t, entry, None),
            // A virtual call to a concrete method: inline it for receivers that still dispatch
            // to it (checked on the vtable, so later subclasses stay correct).
            None if op == 0xb6 && m.vtable_index() >= 0 && !m.is_abstract() && !m.holder.is_interface() => {
                self.try_inline(pc, args, m, Entry::Vtable(m.vtable_index()), None)
            }
            // Tier 2: a site tier 1 only saw one receiver class at inlines that class's target.
            None if self.tier >= 2 && matches!(op, 0xb6 | 0xb9) => super::profile::monomorphic(self.m, pc)
                .and_then(|k| crate::link::select_method(k, m).filter(|t| !t.is_abstract()).map(|t| (k, t)))
                .and_then(|(k, t)| self.try_inline(pc, args, t, Entry::Klass(k), None)),
            None => None,
        };
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
        if let Some(done) = inlined {
            self.a.bind(done);
        }
    }

    pub(super) fn invokedynamic(&mut self, pc: usize, depth: u16) {
        let index = u16::from_be_bytes([self.code[pc + 1], self.code[pc + 2]]);
        let Cp::InvokeDynamic { nat, .. } = self.cf.pool[index as usize] else { return self.defer(pc, depth) };
        let (_, desc) = self.cf.name_and_type(nat).unwrap();
        self.linked_site(pc, depth, indy::site_sig(desc), 0);
    }

    /// A site the interpreter already linked: call its invoker with the appendix appended. The
    /// invoker and appendix are constants, so the invoker is inlined like any known callee, with
    /// the appendix as a constant argument.
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
        let mut consts: Vec<Option<i64>> = (args..d).map(|i| self.site_const(i)).collect();
        if appendix != 0 {
            consts.push(Some(appendix as i64));
        }
        let inlined = self.try_inline(pc, args, invoker, Entry::Static(invoker.holder), Some(consts));
        self.a.mov_ri(R::Rdx, invoker as *const Method as i64);
        self.call_rdx(pc, args, sig.ret);
        if let Some(done) = inlined {
            self.a.bind(done);
        }
    }

    /// `MethodHandle.invokeBasic`: call the receiver's `form.vmentry` target. A constant
    /// receiver names its target now (fold.rs), which is then called directly or inlined.
    fn invoke_basic(&mut self, pc: usize, depth: u16, sig: &'static MethodSig) {
        let args = depth as i32 - sig.arg_slots as i32 - 1;
        if let Some(target) = self.site_const(args).and_then(|h| super::fold::handle_target(h as usize)) {
            let inlined = self.try_inline(pc, args, target, Entry::Static(target.holder), None);
            self.a.mov_ri(R::Rdx, target as *const Method as i64);
            self.call_rdx(pc, args, sig.ret);
            if let Some(done) = inlined {
                self.a.bind(done);
            }
            return;
        }
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
    /// the receiver for virtual/interface) with the arguments before it. A constant MemberName
    /// makes this an ordinary static, special or virtual call.
    fn link_to(&mut self, pc: usize, depth: u16, m: &'static Method, sig: &'static MethodSig) {
        let d = depth as i32;
        let args = d - sig.arg_slots as i32;
        let kind = m.name.as_str();
        if let Some(target) = self.site_const(d - 1).and_then(|mn| super::fold::member_target(mn as usize)) {
            return self.link_to_known(pc, depth, args, kind, target, sig);
        }
        let slow = self.slow_path(pc, depth);
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

    fn link_to_known(&mut self, pc: usize, depth: u16, args: i32, kind: &str, target: &'static Method, sig: &'static MethodSig) {
        let bound = kind == "linkToStatic" || kind == "linkToSpecial" || target.is_private() || target.access & ACC_FINAL != 0;
        let entry = if kind == "linkToStatic" { Entry::Static(target.holder) } else { Entry::Receiver };
        let inlined = if bound { self.try_inline(pc, args, target, entry, None) } else { None };
        let slow = self.slow_path(pc, depth);
        if kind != "linkToStatic" {
            self.a.load(true, R::Rcx, self.stk(args));
            self.a.test_rr(true, R::Rcx, R::Rcx);
            self.a.jcc(Cond::E, slow);
        }
        if bound {
            self.a.mov_ri(R::Rdx, target as *const Method as i64);
        } else {
            self.dispatch(pc, target, slow);
        }
        self.call_rdx(pc, args, sig.ret);
        if let Some(done) = inlined {
            self.a.bind(done);
        }
    }
}
