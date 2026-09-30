//! Graph building for `invokedynamic` and signature-polymorphic calls (as the baseline compiler
//! does in `emit_call.rs`): a linked site calls its invoker with the appendix appended, and
//! `invokeBasic` / `linkTo*` on a constant handle or member name become ordinary calls to the
//! method it names, inlined like any other. Non-constant ones read the target from the handle.

use super::build::Builder;
use super::inline::Guard;
use super::ir::*;
use crate::classfile::Cp;
use crate::interp::indy;
use crate::jit::fold;
use crate::klass::ACC_FINAL;
use crate::method::Method;
use crate::natives::mh;
use crate::types::{BasicType, MethodSig};

impl Builder {
    pub(super) fn invokedynamic(&mut self, pc: usize) -> Option<bool> {
        let index = u16::from_be_bytes([self.sc.code[pc + 1], self.sc.code[pc + 2]]);
        let Cp::InvokeDynamic { nat, .. } = self.sc.cf.pool[index as usize] else { return None };
        let (_, desc) = self.sc.cf.name_and_type(nat)?;
        self.linked_site(pc, indy::site_sig(desc), 0)
    }

    /// A signature-polymorphic invoke of `m` at `pc` (site descriptor `desc`).
    pub(super) fn polymorphic(&mut self, pc: usize, op: u8, m: &'static Method, desc: crate::symbol::Symbol) -> Option<bool> {
        let sig = indy::site_sig(desc);
        match (op, m.name.as_str()) {
            (0xb8, kind) => self.link_to(kind, sig),
            (_, "invokeBasic") => Some(self.invoke_basic(sig)),
            _ => self.linked_site(pc, sig, 1),
        }
    }

    /// Pops a call's arguments (`n` values), returning them with the states before and after.
    fn pop_args(&mut self, n: usize) -> (Vec<V>, std::rc::Rc<State>, std::rc::Rc<State>) {
        let pre = self.state();
        let mut args: Vec<V> = (0..n).map(|_| self.pop()).collect();
        args.reverse();
        (args, pre, self.state())
    }

    /// A site the interpreter already linked: its invoker with the appendix as last argument.
    fn linked_site(&mut self, pc: usize, sig: &'static MethodSig, receiver: usize) -> Option<bool> {
        let (invoker, appendix) = indy::linked(self.sc.m, pc)?;
        let (mut args, pre, s) = self.pop_args(sig.params.len() + receiver);
        if receiver == 1 {
            self.null_check(args[0], pre);
        }
        if appendix != 0 {
            args.push(self.konst(Ty::A, appendix as i64));
        }
        Some(self.emit_call(vec![(invoker, Guard::None)], Callee::Direct(invoker), args, s, sig.ret))
    }

    /// `MethodHandle.invokeBasic`: the receiver's `form.vmentry` target, with the handle as its
    /// first argument.
    fn invoke_basic(&mut self, sig: &'static MethodSig) -> bool {
        let (args, pre, s) = self.pop_args(sig.params.len() + 1);
        if let Some(target) = self.g.constant(args[0]).and_then(|h| fold::handle_target(h as usize)) {
            return self.emit_call(vec![(target, Guard::None)], Callee::Direct(target), args, s, sig.ret);
        }
        self.null_check(args[0], pre);
        let form = self.add(Inst::LoadField(args[0], indy::mh_form(), BasicType::Reference), Ty::A);
        let entry = self.add(Inst::LoadField(form, indy::lf_vmentry(), BasicType::Reference), Ty::A);
        let target = self.member_method(entry);
        self.emit_call(Vec::new(), Callee::Method(target), args, s, sig.ret)
    }

    /// The `Method*` of the `MemberName` value `mn`.
    fn member_method(&mut self, mn: V) -> V {
        let rmn = self.add(Inst::LoadField(mn, mh::mn_method(), BasicType::Reference), Ty::A);
        self.add(Inst::LoadField(rmn, mh::rmn_vmtarget(), BasicType::Long), Ty::J)
    }

    /// `MethodHandle.linkTo*(args..., MemberName)`: the member's target (dispatched on the
    /// receiver for virtual and interface kinds) with the arguments before it.
    fn link_to(&mut self, kind: &str, sig: &'static MethodSig) -> Option<bool> {
        let (mut args, pre, s) = self.pop_args(sig.params.len());
        let mn = args.pop()?;
        let is_static = kind == "linkToStatic";
        if !is_static {
            self.null_check(args[0], pre.clone());
        }
        if let Some(target) = self.g.constant(mn).and_then(|c| fold::member_target(c as usize)) {
            if is_static {
                self.init_check(target.holder, &pre);
            }
            let bound = is_static || kind == "linkToSpecial" || target.is_private() || target.access & ACC_FINAL != 0 || target.holder.access & ACC_FINAL != 0;
            let callee = match () {
                _ if bound => Callee::Direct(target),
                _ if target.vtable_index() >= 0 && !target.holder.is_interface() => Callee::Virtual(target),
                _ => Callee::Interface(target),
            };
            let plan = if bound { vec![(target, Guard::None)] } else { Vec::new() };
            return Some(self.emit_call(plan, callee, args, s, sig.ret));
        }
        let target = self.member_method(mn);
        let callee = if is_static || kind == "linkToSpecial" { Callee::Method(target) } else { Callee::Select(target) };
        Some(self.emit_call(Vec::new(), callee, args, s, sig.ret))
    }
}
