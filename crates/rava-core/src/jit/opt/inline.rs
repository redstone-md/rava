//! Inlining in the graph builder. The policy is the baseline compiler's (`inline_plan.rs`) at
//! tier 2. A callee known statically is built in place of the call; a virtual or interface
//! callee chosen by its vtable entry or the site's receiver profile is guarded: the guard's
//! failure path makes the ordinary call, and both paths meet in a continuation block with a phi
//! for the result. A callee that cannot be built leaves the graph as it was.

use super::build::{Builder, Scope};
use super::ir::*;
use crate::jit::inline_plan::{bound_target, decide};
use crate::klass::Klass;
use crate::method::Method;
use crate::types::BasicType;
use std::rc::Rc;

/// What the inlined body assumes about the receiver.
#[derive(Clone, Copy)]
pub(super) enum Guard {
    /// Nothing: the target is statically bound.
    None,
    /// The receiver's vtable entry `index` is the target.
    Vtable(i32),
    /// The receiver's class is exactly this one.
    Klass(&'static Klass),
}

/// The graph's size and the current block's contents, to undo a failed inlining.
struct Mark {
    values: usize,
    blocks: usize,
    cur: B,
    insts: usize,
    term: Term,
    extent: i32,
    spent: usize,
}

impl Builder {
    /// Builds the invoke at `pc` of the resolved `m` (arguments popped into `args`, `s` the state
    /// after the pop) as an inlined body when possible, else as a call. Returns whether the
    /// current block continues (false when every path through the callee left compiled code).
    pub(super) fn call_or_inline(&mut self, pc: usize, op: u8, m: &'static Method, callee: Callee, args: Vec<V>, s: Rc<State>) -> bool {
        let plan = self.inline_target(pc, op, m);
        self.emit_call(plan, callee, args, s, m.sig.ret)
    }

    /// The call `callee(args)` returning `ret`, with `plan` (a target and what it needs checked)
    /// inlined when the policy agrees. Returns whether the current block continues.
    pub(super) fn emit_call(&mut self, plan: Option<(&'static Method, Guard)>, callee: Callee, args: Vec<V>, s: Rc<State>, ret: BasicType) -> bool {
        let first = args.first().and_then(|&a| self.g.constant(a));
        let plan = plan.filter(|&(t, _)| decide(t, first, &self.chain, 2, self.spent).is_some());
        if std::env::var_os("RAVA_CORE_INLINE_LOG").is_some() {
            let indent = "  ".repeat(self.chain.len());
            eprintln!("[opt-inline] {indent}{} @{} -> {callee:?} {}", self.sc.m, s.bci, if plan.is_some() { "inlined" } else { "call" });
        }
        let Some((target, guard)) = plan else {
            self.call_push(callee, args, s, ret);
            return true;
        };
        let mark = self.mark();
        let k = self.g.new_block(-1);
        let slow = self.guard(guard, target, args.first().copied());
        let Some(mut returns) = self.inline_body(target, &args, s.clone(), k, first) else {
            self.undo(mark);
            self.call_push(callee, args, s, ret);
            return true;
        };
        if let Some(slow) = slow {
            self.start_block(slow);
            let r = self.call(callee, args, s, ret);
            self.g.blocks[slow as usize].term = Term::Goto(k);
            self.g.blocks[k as usize].preds.push(slow);
            returns.push((slow, r));
        }
        if returns.is_empty() {
            return false;
        }
        self.start_block(k);
        if ret != BasicType::Void {
            let v = match returns.as_slice() {
                [(_, v)] => v.expect("a value return"),
                _ => self.add(Inst::Phi(returns.iter().map(|r| r.1.expect("a value return")).collect()), Ty::of(ret)),
            };
            self.push(v);
        }
        true
    }

    /// A call instruction; returns its result.
    fn call(&mut self, callee: Callee, args: Vec<V>, s: Rc<State>, ret: BasicType) -> Option<V> {
        let v = self.add(Inst::Call(callee, args, s), if ret == BasicType::Void { Ty::I } else { Ty::of(ret) });
        (ret != BasicType::Void).then_some(v)
    }

    fn call_push(&mut self, callee: Callee, args: Vec<V>, s: Rc<State>, ret: BasicType) {
        if let Some(v) = self.call(callee, args, s, ret) {
            self.push(v);
        }
    }

    /// The method to inline for this site and what it needs checked (as the baseline compiler
    /// chooses it).
    fn inline_target(&self, pc: usize, op: u8, m: &'static Method) -> Option<(&'static Method, Guard)> {
        if let Some(t) = bound_target(self.sc.m, self.sc.code, pc) {
            return Some((t, Guard::None));
        }
        if op == 0xb6 && m.vtable_index() >= 0 && !m.is_abstract() && !m.holder.is_interface() {
            return Some((m, Guard::Vtable(m.vtable_index())));
        }
        let k = crate::jit::profile::monomorphic(self.sc.m, pc)?;
        let t = crate::link::select_method(k, m).filter(|t| !t.is_abstract())?;
        Some((t, Guard::Klass(k)))
    }

    /// Ends the current block with the guard's test; returns the failure block, and continues in
    /// the success block.
    fn guard(&mut self, guard: Guard, target: &'static Method, receiver: Option<V>) -> Option<B> {
        let (entry, expect) = match guard {
            Guard::None => return None,
            Guard::Klass(k) => (self.klass_of(receiver?), k as *const Klass as i64),
            Guard::Vtable(i) => {
                let k = self.klass_of(receiver?);
                let vt = self.add(Inst::LoadField(k, std::mem::offset_of!(Klass, vtable_ptr) as u32, BasicType::Long), Ty::J);
                (self.add(Inst::LoadField(vt, 8 * i as u32, BasicType::Long), Ty::J), target as *const Method as i64)
            }
        };
        let c = self.konst(Ty::J, expect);
        let (then, other) = (self.g.new_block(-1), self.g.new_block(-1));
        self.g.blocks[then as usize].preds.push(self.cur);
        self.g.blocks[other as usize].preds.push(self.cur);
        self.g.blocks[self.cur as usize].term = Term::If { cmp: Cmp::Eq, ty: Ty::J, a: entry, b: c, then, other };
        self.start_block(then);
        Some(other)
    }

    fn klass_of(&mut self, receiver: V) -> V {
        self.add(Inst::LoadField(receiver, 0, BasicType::Long), Ty::J)
    }

    /// Builds `target` from the current block with `args` as its locals; returns its exits into
    /// the continuation block `k`.
    fn inline_body(&mut self, target: &'static Method, args: &[V], s: Rc<State>, k: B, first: Option<i64>) -> Option<Vec<(B, Option<V>)>> {
        let frame = self.sc.frame + self.sc.m.code?.max_locals as i32 + crate::thread::header::SLOTS as i32 + s.stack.len() as i32;
        let mut sc = Scope::new(target, Some(s), frame)?;
        let mut slot = 0;
        for &a in args {
            sc.locals[slot] = Some(a);
            slot += if self.g.ty(a).wide() { 2 } else { 1 };
        }
        sc.cont = Some(k);
        self.extent = self.extent.max(sc.frame_end());
        self.spent += sc.code.len();
        self.chain.push((target, first));
        let caller = std::mem::replace(&mut self.sc, sc);
        self.enter_scope();
        let ok = self.build_scope();
        let callee = std::mem::replace(&mut self.sc, caller);
        self.chain.pop();
        ok.map(|()| callee.returns)
    }

    /// Ends an inlined scope's path with a return of `v`.
    pub(super) fn inline_return(&mut self, k: B, v: Option<V>) {
        self.g.blocks[self.cur as usize].term = Term::Goto(k);
        self.g.blocks[k as usize].preds.push(self.cur);
        self.sc.returns.push((self.cur, v));
    }

    fn mark(&self) -> Mark {
        let cur = &self.g.blocks[self.cur as usize];
        Mark {
            values: self.g.values.len(),
            blocks: self.g.blocks.len(),
            cur: self.cur,
            insts: cur.insts.len(),
            term: cur.term.clone(),
            extent: self.extent,
            spent: self.spent,
        }
    }

    fn undo(&mut self, m: Mark) {
        self.g.values.truncate(m.values);
        self.g.blocks.truncate(m.blocks);
        let cur = &mut self.g.blocks[m.cur as usize];
        cur.insts.truncate(m.insts);
        cur.term = m.term;
        self.cur = m.cur;
        self.extent = m.extent;
        self.spent = m.spent;
    }
}
