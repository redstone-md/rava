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
#[derive(Clone)]
pub(super) enum Guard {
    /// Nothing: the target is statically bound.
    None,
    /// No loaded class overrides the target (class hierarchy analysis): only its overridden
    /// flag is tested, and once set the code deoptimizes to the invoke (this state, with the
    /// arguments still on the stack) instead of calling.
    Unique(Rc<State>),
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
    nonnull: Vec<V>,
}

impl Builder {
    /// Builds the invoke at `pc` of the resolved `m` (arguments popped into `args`, `s` the state
    /// after the pop) as an inlined body when possible, else as a call. Returns whether the
    /// current block continues (false when every path through the callee left compiled code).
    /// `pre` is the state before the invoke, for a guard that deoptimizes.
    pub(super) fn call_or_inline(&mut self, pc: usize, op: u8, m: &'static Method, callee: Callee, args: Vec<V>, s: Rc<State>, pre: Rc<State>) -> bool {
        // A receiver whose class is known exactly dispatches statically (C2's type-based
        // devirtualization): no profile or guard is needed.
        let exact = (op != 0xb8).then(|| args.first().and_then(|&r| self.exact_class(r))).flatten();
        let plans = match exact.and_then(|k| crate::link::select_method(k, m)).filter(|t| !t.is_abstract()) {
            Some(t) if matches!(op, 0xb6 | 0xb9) => vec![(t, Guard::None)],
            _ => self.inline_targets(pc, op, m, pre.clone()),
        };
        if plans.is_empty() && env_once!("RAVA_CORE_INLINE_LOG").is_some() {
            let cell = crate::jit::profile::describe(self.sc.m, pc);
            let indent = "  ".repeat(self.chain.len());
            eprintln!("[opt-inline] {indent}{} @{} -> {m} call (no target: {cell})", self.sc.m, pc);
        }
        // A complete receiver profile (every class the site saw is guarded): a receiver of
        // another class deoptimizes (C2's uncommon trap), noting its class for recompilation;
        // a site that trapped before calls instead.
        let profiled = !plans.is_empty()
            && plans.iter().all(|(_, g)| matches!(g, Guard::Klass(_)))
            && self.speculates(self.sc.m, pc);
        self.emit_miss(plans, callee, args, s, m.sig.ret, profiled.then_some(pre))
    }

    /// The call `callee(args)` returning `ret`, with each of `plans` (a target and what it
    /// needs checked) inlined when the policy agrees; guarded plans are tried in turn and the
    /// call is made when none applies. Returns whether the current block continues.
    pub(super) fn emit_call(&mut self, plans: Vec<(&'static Method, Guard)>, callee: Callee, args: Vec<V>, s: Rc<State>, ret: BasicType) -> bool {
        self.emit_miss(plans, callee, args, s, ret, None)
    }

    /// `emit_call`, where with `miss` (the state before the call) a receiver no guard admits
    /// deoptimizes instead of calling, provided every plan was inlined.
    fn emit_miss(&mut self, plans: Vec<(&'static Method, Guard)>, callee: Callee, args: Vec<V>, s: Rc<State>, ret: BasicType, miss: Option<Rc<State>>) -> bool {
        let all = plans.len();
        // What is known at the call holds after it, on every path through the callee.
        let known = self.nonnull.clone();
        let first = args.first().and_then(|&a| self.g.constant(a));
        let k = self.g.new_block(-1);
        let mut returns = Vec::new();
        // `open`: the current block (the last guard's failure path) still needs the call.
        let (mut open, mut inlined, mut done) = (true, false, 0);
        for (target, guard) in plans {
            // A guarded target the VM compiles in place (`Object.hashCode` for a class that
            // does not override it).
            let intrinsic = crate::jit::intrinsics::intrinsic(target).filter(|k| k.is_stateless());
            let ok = intrinsic.is_some() || decide(target, first, &self.chain, 3, self.spent).is_some();
            if env_once!("RAVA_CORE_INLINE_LOG").is_some() {
                let indent = "  ".repeat(self.chain.len());
                let why = if ok { String::new() } else { format!(" ({})", crate::jit::inline_plan::refusal(target, &self.chain, 3, self.spent)) };
                eprintln!("[opt-inline] {indent}{} @{} -> {target} {}{why}", self.sc.m, s.bci, if ok { "inlined" } else { "call" });
            }
            if !ok {
                continue;
            }
            let mark = self.mark();
            let slow = self.guard(guard.clone(), target, args.first().copied());
            // Behind an exact class check the body's receiver is of that class.
            let exact_mark = self.exact.len();
            if let (Guard::Klass(c), Some(&r)) = (&guard, args.first()) {
                self.exact.push((r, c));
            }
            let body = match intrinsic {
                Some(i) => Some(self.intrinsic_body(i, target, &args, k)),
                None => self.inline_body(target, &args, s.clone(), k, first),
            };
            self.exact.truncate(exact_mark);
            let Some(r) = body else {
                self.undo(mark);
                continue;
            };
            returns.extend(r);
            inlined = true;
            done += 1;
            match slow {
                Some(b) => {
                    self.start_block(b);
                    self.nonnull.clone_from(&known);
                }
                None => {
                    open = false;
                    break;
                }
            }
        }
        if let (true, Some(pre), true) = (open, &miss, done == all) {
            self.g.blocks[self.cur as usize].term = Term::Deopt(pre.clone());
            open = false;
        }
        if open {
            let r = self.call(callee, args, s, ret);
            if !inlined {
                if let Some(v) = r {
                    self.push(v);
                }
                return true;
            }
            self.g.blocks[self.cur as usize].term = Term::Goto(k);
            self.g.blocks[k as usize].preds.push(self.cur);
            returns.push((self.cur, r));
        }
        if returns.is_empty() {
            return false;
        }
        self.start_block(k);
        self.nonnull = known;
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

    /// The methods to inline for this site and what each needs checked: the statically bound
    /// target, the vtable's current target, or the targets for the receiver classes profiled.
    fn inline_targets(&self, pc: usize, op: u8, m: &'static Method, pre: Rc<State>) -> Vec<(&'static Method, Guard)> {
        if let Some(t) = bound_target(self.sc.m, self.sc.code, pc) {
            return vec![(t, Guard::None)];
        }
        let profiled = || -> Vec<(&'static Method, Guard)> {
            let receivers = crate::jit::profile::receivers(self.sc.m, pc).into_iter();
            receivers.filter_map(|k| crate::link::select_method(k, m).filter(|t| !t.is_abstract()).map(|t| (t, Guard::Klass(k)))).collect()
        };
        if op == 0xb6 && m.vtable_index() >= 0 && !m.holder.is_interface() {
            if !m.is_abstract() && !m.is_overridden() {
                return vec![(m, Guard::Unique(pre))];
            }
            // The receiver classes first-tier code saw, else the declared method for receivers
            // still dispatching to it.
            let seen = profiled();
            if !seen.is_empty() || m.is_abstract() {
                return seen;
            }
            return vec![(m, Guard::Vtable(m.vtable_index()))];
        }
        profiled()
    }

    /// Ends the current block with the guard's test; returns the failure block, and continues in
    /// the success block.
    fn guard(&mut self, guard: Guard, target: &'static Method, receiver: Option<V>) -> Option<B> {
        let (entry, expect) = match guard {
            Guard::None => return None,
            Guard::Unique(pre) => {
                self.add(Inst::FlagCheck(target.overridden_flag(), pre), Ty::I);
                return None;
            }
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
        self.g.blocks[self.cur as usize].term = Term::If { cmp: Cmp::Eq, ty: Ty::J, a: entry, b: c, then, other, unordered: false };
        self.start_block(then);
        Some(other)
    }

    /// The class `v` is known to have exactly here, if any.
    pub(super) fn exact_class(&self, v: V) -> Option<&'static Klass> {
        self.exact.iter().rev().find(|&&(x, _)| x == v).map(|&(_, k)| k)
    }

    fn klass_of(&mut self, receiver: V) -> V {
        self.add(Inst::LoadField(receiver, 0, BasicType::Long), Ty::J)
    }

    /// `target` as intrinsic `i` on `args` in the current block, which then goes to `k`.
    fn intrinsic_body(&mut self, i: crate::jit::intrinsics::Intrinsic, target: &'static Method, args: &[V], k: B) -> Vec<(B, Option<V>)> {
        let ret = target.sig.ret;
        let v = self.add(Inst::Intrinsic(i, args.to_vec(), None), if ret == BasicType::Void { Ty::I } else { Ty::of(ret) });
        self.g.blocks[self.cur as usize].term = Term::Goto(k);
        self.g.blocks[k as usize].preds.push(self.cur);
        vec![(self.cur, (ret != BasicType::Void).then_some(v))]
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
            nonnull: self.nonnull.clone(),
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
        self.nonnull = m.nonnull;
    }
}
