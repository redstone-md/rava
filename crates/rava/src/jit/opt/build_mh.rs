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
        let Cp::InvokeDynamic { nat, .. } = self.sc.cf.pool.at(index as usize) else { return None };
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
        let (mut args, pre, s) = self.pop_args(sig.params.len() + 1);
        if let Some(h) = self.folded_constant(args[0]).map(|h| fold::through_counting(h as usize)).filter(|&h| h != 0) {
            if let Some(target) = fold::handle_target(h) {
                args[0] = self.konst(Ty::A, h as i64);
                return self.emit_call(vec![(target, Guard::None)], Callee::Direct(target), args, s, sig.ret);
            }
        }
        self.null_check(args[0], pre);
        let handles = self.constant_handles(args[0]);
        if !handles.is_empty() {
            return self.invoke_basic_split(handles, args, s, sig.ret);
        }
        let form = self.add(Inst::LoadField(args[0], indy::mh_form(), BasicType::Reference), Ty::A);
        let entry = self.add(Inst::LoadField(form, indy::lf_vmentry(), BasicType::Reference), Ty::A);
        let target = self.member_method(entry);
        self.emit_call(Vec::new(), Callee::Method(target), args, s, sig.ret)
    }

    /// The constant handles (with their targets) a merge of a few of them can be: the handle
    /// `MethodHandleImpl.selectAlternative` picks in a `guardWithTest` form, once inlined.
    fn constant_handles(&self, v: V) -> Vec<(i64, &'static Method)> {
        let Inst::Phi(inputs) = self.g.inst(self.g.resolve(v)) else { return Vec::new() };
        let mut out: Vec<(i64, &'static Method)> = Vec::new();
        for &x in inputs {
            let Some(c) = self.folded_constant(x).filter(|&c| c != 0) else { return Vec::new() };
            let Some(t) = fold::handle_target(fold::through_counting(c as usize)) else { return Vec::new() };
            if !out.iter().any(|&(o, _)| o == c) {
                out.push((c, t));
            }
        }
        if out.len() > 4 {
            return Vec::new();
        }
        out
    }

    /// `invokeBasic` on one of `handles`: a test for each, with the call inlined behind it on
    /// that constant handle (so its form's reads of the handle fold too), then the generic call.
    fn invoke_basic_split(&mut self, handles: Vec<(i64, &'static Method)>, args: Vec<V>, s: std::rc::Rc<State>, ret: BasicType) -> bool {
        let k = self.g.new_block(-1);
        let known = self.nonnull.clone();
        let mut returns: Vec<(B, Option<V>)> = Vec::new();
        for (c, target) in handles {
            let cv = self.konst(Ty::A, c);
            let (then, other) = (self.g.new_block(-1), self.g.new_block(-1));
            self.g.blocks[then as usize].preds.push(self.cur);
            self.g.blocks[other as usize].preds.push(self.cur);
            self.g.blocks[self.cur as usize].term = Term::If { cmp: Cmp::Eq, ty: Ty::A, a: args[0], b: cv, then, other, unordered: false };
            self.start_block(then);
            let mut a = args.clone();
            a[0] = self.konst(Ty::A, fold::through_counting(c as usize) as i64);
            if self.emit_call(vec![(target, Guard::None)], Callee::Direct(target), a, s.clone(), ret) {
                let v = (ret != BasicType::Void).then(|| self.pop());
                self.g.blocks[self.cur as usize].term = Term::Goto(k);
                self.g.blocks[k as usize].preds.push(self.cur);
                returns.push((self.cur, v));
            }
            self.start_block(other);
            self.nonnull.clone_from(&known);
        }
        let entry = self.add(Inst::LoadField(args[0], indy::mh_form(), BasicType::Reference), Ty::A);
        let entry = self.add(Inst::LoadField(entry, indy::lf_vmentry(), BasicType::Reference), Ty::A);
        let target = self.member_method(entry);
        if self.emit_call(Vec::new(), Callee::Method(target), args, s, ret) {
            let v = (ret != BasicType::Void).then(|| self.pop());
            self.g.blocks[self.cur as usize].term = Term::Goto(k);
            self.g.blocks[k as usize].preds.push(self.cur);
            returns.push((self.cur, v));
        }
        if returns.is_empty() {
            return false;
        }
        self.start_block(k);
        self.nonnull = known;
        if ret != BasicType::Void {
            let v = match returns.as_slice() {
                [(_, v)] => v.expect("a value return"),
                _ => {
                    let p = self.add(Inst::Phi(returns.iter().map(|r| r.1.expect("a value return")).collect()), Ty::of(ret));
                    self.complete_phis.insert(p);
                    p
                }
            };
            self.push(v);
        }
        true
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
        if let Some(target) = self.folded_constant(mn).and_then(|c| fold::member_target(c as usize)) {
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
