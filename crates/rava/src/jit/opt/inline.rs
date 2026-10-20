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
    /// The receiver's class is a subclass of this one (an interface call's receiver) whose
    /// vtable entry `index` is the target.
    Subclass(&'static Klass, i32),
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
    facts: super::build_cache::Facts,
    caches: super::build_cache::Caches,
}

impl Builder {
    /// Builds the invoke at `pc` of the resolved `m` (arguments popped into `args`, `s` the state
    /// after the pop) as an inlined body when possible, else as a call. Returns whether the
    /// current block continues (false when every path through the callee left compiled code).
    /// `pre` is the state before the invoke, for a guard that deoptimizes.
    pub(super) fn call_or_inline(&mut self, pc: usize, op: u8, m: &'static Method, callee: Callee, args: Vec<V>, s: Rc<State>, pre: Rc<State>) -> bool {
        // A receiver whose class is known exactly dispatches statically (C2's type-based
        // devirtualization): no profile or guard is needed.
        let exact = (op != 0xb8).then(|| args.first().and_then(|&r| self.exact_receiver_class(r))).flatten();
        // A receiver whose exact class does not have the method can only be null (a field of a
        // final class's type cast to an unrelated interface): no static target then.
        let exact = exact.filter(|k| k.is_subtype_of(m.holder));
        let plans = match exact.and_then(|k| crate::link::select_method(k, m)).filter(|t| !t.is_abstract()) {
            Some(t) if matches!(op, 0xb6 | 0xb9) => vec![(t, Guard::None)],
            _ => self.inline_targets(pc, op, m, pre.clone()),
        };
        if plans.is_empty() && crate::jit::inline_plan::log_enabled() {
            let cell = crate::jit::profile::describe(self.sc.m, pc);
            let indent = "  ".repeat(self.chain.len());
            crate::jit::inline_plan::log(format_args!("[opt-inline] {indent}{} @{} -> {m} call (no target: {cell})", self.sc.m, pc));
        }
        // A complete receiver profile (every class the site saw is guarded): a receiver of
        // another class deoptimizes (C2's uncommon trap), noting its class for recompilation;
        // a site that trapped before calls instead.
        // So does a receiver failing the test of the one implementation the site's receivers
        // (or all loaded classes) share: no call is left on the path, so the code around it
        // (a loop over the receivers' fields) has no call to keep its loads in place.
        let shared = matches!(plans.as_slice(), [(_, Guard::Vtable(_) | Guard::Subclass(..))]);
        let profiled = !plans.is_empty()
            && (plans.iter().all(|(_, g)| matches!(g, Guard::Klass(_))) && !crate::jit::profile::megamorphic(self.sc.m, pc) || shared)
            && self.speculates(self.sc.m, pc);
        self.unprofiled_call = plans.is_empty() && matches!(callee, Callee::Virtual(_) | Callee::Interface(_)) && crate::jit::profile::unprofiled(self.sc.m, pc);
        let boxed = (op == 0xb8 && super::build_obj::box_value_of(m)).then(|| args[0]);
        let r = self.emit_miss(plans, callee, args, s, m.sig.ret, profiled.then_some(pre));
        self.unprofiled_call = false;
        if let (true, Some(x)) = (r, boxed) {
            let b = self.g.resolve(*self.sc.stack.last().and_then(|v| v.as_ref()).expect("valueOf's result"));
            self.boxes.insert(b, (m.holder, x));
        }
        r
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
        self.cold_call = false;
        let known = self.facts.clone();
        let first = args.first().and_then(|&a| self.g.constant(a));
        let k = self.g.new_block(-1);
        let mut returns = Vec::new();
        // `open`: the current block (the last guard's failure path) still needs the call.
        let (mut open, mut inlined, mut done) = (true, false, 0);
        // The one class every inlined body's guard required (None once two differ or a body
        // needed no class guard): with no call path left, the receiver has it after the call.
        let mut proved: Option<Option<&'static Klass>> = None;
        let receiver = args.first().copied();
        for (target, guard) in plans {
            // A guarded target the VM compiles in place (`Object.hashCode` for a class that
            // does not override it).
            let intrinsic = crate::jit::intrinsics::intrinsic(target).filter(|k| k.is_stateless());
            let mut freq = self.sc.frequency(s.bci as usize);
            if crate::jit::profile::hot_site(self.sc.m, s.bci as usize) {
                freq = freq.max(1.0);
            }
            let ok = intrinsic.is_some() || decide(target, first, &self.chain, 3, self.spent, freq).is_some();
            if !ok && intrinsic.is_none() && crate::jit::inline_plan::cold_site(target, freq) {
                self.cold_call = true;
            }
            if crate::jit::inline_plan::log_enabled() {
                let indent = "  ".repeat(self.chain.len());
                let why = if ok { String::new() } else { format!(" ({})", crate::jit::inline_plan::refusal(target, &self.chain, 3, self.spent, freq)) };
                crate::jit::inline_plan::log(format_args!("[opt-inline] {indent}{} @{} (freq {freq:.3}) -> {target} {}{why} [{}]", self.sc.m, s.bci, if ok { "inlined" } else { "call" }, crate::jit::profile::describe(self.sc.m, s.bci as usize)));
            }
            if !ok {
                // A receiver class the profile names, whose target is not inlined: tested
                // and called directly (HotSpot's monomorphic inline cache), the dispatch left
                // to the other receivers. So is the only implementation of a method nothing
                // overrides (behind its dependency), without a vtable load.
                let virtual_site = matches!(callee, Callee::Virtual(_) | Callee::Interface(_));
                let class = match &guard {
                    Guard::Klass(c) => Some(Some(*c)),
                    Guard::Unique(_) => Some(None),
                    _ => None,
                };
                if let (Some(c), true) = (class, virtual_site && intrinsic.is_none() && !target.is_abstract()) {
                    let slow = self.guard(guard.clone(), target, receiver);
                    let r = self.call(Callee::Direct(target), args.clone(), s.clone(), ret);
                    self.g.blocks[self.cur as usize].term = Term::Goto(k);
                    self.g.blocks[k as usize].preds.push(self.cur);
                    returns.push((self.cur, r));
                    inlined = true;
                    done += 1;
                    proved = match proved {
                        None => Some(c),
                        Some(p) => Some(p.zip(c).filter(|(p, c)| std::ptr::eq(*p, *c)).map(|(p, _)| p)),
                    };
                    match slow {
                        Some(b) => {
                            self.start_block(b);
                            self.facts.clone_from(&known);
                        }
                        None => {
                            open = false;
                            break;
                        }
                    }
                }
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
                None => {
                    // A body whose own compilation was too large holds to C2's polymorphic
                    // inlining, as does everything it inlines.
                    let shallow = crate::jit::inline_plan::over_large(target, freq);
                    self.shallow += shallow as u32;
                    let body = self.inline_body(target, &args, s.clone(), k, first);
                    self.shallow -= shallow as u32;
                    body
                }
            };
            self.exact.truncate(exact_mark);
            let Some(r) = body else {
                self.undo(mark);
                continue;
            };
            returns.extend(r);
            inlined = true;
            let c = match guard {
                Guard::Klass(c) => Some(c),
                _ => None,
            };
            proved = match proved {
                None => Some(c),
                Some(p) => Some(p.zip(c).filter(|(p, c)| std::ptr::eq(*p, *c)).map(|(p, _)| p)),
            };
            done += 1;
            match slow {
                Some(b) => {
                    self.start_block(b);
                    self.facts.clone_from(&known);
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
            self.fallback_call = inlined;
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
        self.facts = known;
        if let (false, Some(Some(c)), Some(r)) = (open, proved, receiver) {
            self.facts.push_exact(r, c);
        }
        if ret != BasicType::Void {
            let v = match returns.as_slice() {
                [(_, v)] => v.expect("a value return"),
                _ => {
                    let ins: Vec<V> = returns.iter().map(|r| r.1.expect("a value return")).collect();
                    let p = self.add(Inst::Phi(ins.clone()), Ty::of(ret));
                    self.complete_phis.insert(p);
                    // Boxes of one class from every path: a box of the merged values.
                    let boxes: Option<Vec<_>> = ins.iter().map(|&x| self.boxes.get(&self.g.resolve(x)).copied()).collect();
                    if let Some(bs) = boxes.filter(|bs| bs.iter().all(|b| std::ptr::eq(b.0, bs[0].0))) {
                        let ty = self.g.ty(bs[0].1);
                        if bs.iter().all(|b| self.g.ty(b.1) == ty) {
                            let q = self.add(Inst::Phi(bs.iter().map(|b| b.1).collect()), ty);
                            self.boxes.insert(p, (bs[0].0, q));
                        }
                    }
                    p
                }
            };
            self.push(v);
        }
        true
    }

    /// A call instruction; returns its result.
    fn call(&mut self, callee: Callee, args: Vec<V>, s: Rc<State>, ret: BasicType) -> Option<V> {
        let v = self.add(Inst::Call(callee, args, s), if ret == BasicType::Void { Ty::I } else { Ty::of(ret) });
        if self.unprofiled_call {
            self.g.unprofiled.insert(v);
        }
        if std::mem::take(&mut self.cold_call) {
            self.g.cold_calls.insert(v);
        }
        if std::mem::take(&mut self.fallback_call) {
            self.g.fallback_calls.insert(v);
        }
        (ret != BasicType::Void).then_some(v)
    }

    /// The methods to inline for this site and what each needs checked: the statically bound
    /// target, the vtable's current target, or the targets for the receiver classes profiled.
    fn inline_targets(&self, pc: usize, op: u8, m: &'static Method, pre: Rc<State>) -> Vec<(&'static Method, Guard)> {
        if let Some(t) = bound_target(self.sc.m, self.sc.code, pc) {
            return vec![(t, Guard::None)];
        }
        let profiled = || -> Vec<(&'static Method, Guard)> {
            // A megamorphic site whose recorded receivers share their implementation (entity
            // subclasses inheriting one method): that implementation, behind a test that the
            // receiver's class selects it, which admits every class sharing it.
            if crate::jit::profile::megamorphic(self.sc.m, pc) {
                let by_target = self.target_plans(pc, op, m);
                if !by_target.is_empty() {
                    return by_target;
                }
            }
            let mut receivers = crate::jit::profile::receivers(self.sc.m, pc);
            // A megamorphic site still inlines a receiver class it nearly always sees, with a
            // call for the others (C2's major receiver), else the recorded classes that each
            // take a large share of its calls, checked before the generic dispatch.
            if receivers.is_empty() {
                receivers.extend(crate::jit::profile::major_receiver(self.sc.m, pc));
            }
            if receivers.is_empty() && self.shallow == 0 {
                receivers.extend(crate::jit::profile::frequent_receivers(self.sc.m, pc));
            }
            receivers.into_iter().filter_map(|k| crate::link::select_method(k, m).filter(|t| !t.is_abstract()).map(|t| (t, Guard::Klass(k)))).collect()
        };
        if op == 0xb6 && m.vtable_index() >= 0 && !m.holder.is_interface() {
            if !m.is_abstract() && !m.is_overridden() {
                return vec![(m, Guard::Unique(pre))];
            }
            // The receiver classes first-tier code saw, else the declared method for receivers
            // still dispatching to it.
            let mut seen = profiled();
            // `Object.hashCode` of a receiver not overriding it, whatever its class (C2's
            // virtual hashCode intrinsic): the identity hash behind a test of the vtable entry,
            // after the profiled classes, instead of a call into the native method.
            if matches!(crate::jit::intrinsics::intrinsic(m), Some(crate::jit::intrinsics::Intrinsic::IdentityHash)) &&!seen.iter().any(|(_, g)| matches!(g, Guard::Vtable(_))) {
                seen.push((m, Guard::Vtable(m.vtable_index())));
            }
            if !seen.is_empty() {
                return seen;
            }
            // A megamorphic (or unprofiled) site: the one implementation its receivers can
            // have, behind a test of the vtable entry.
            return match super::cha::unique_target(m).or((!m.is_abstract()).then_some(m)) {
                Some(t) => vec![(t, Guard::Vtable(m.vtable_index()))],
                None => Vec::new(),
            };
        }
        let seen = profiled();
        if !seen.is_empty() || op != 0xb9 {
            return seen;
        }
        match super::cha::unique_target(m).and_then(|t| super::cha::interface_guard(t).map(|(c, i)| (t, Guard::Subclass(c, i)))) {
            Some(plan) => vec![plan],
            None => Vec::new(),
        }
    }

    /// The implementation of `m` both receiver classes recorded at this megamorphic site
    /// select, when they agree on one, with a guard testing the receiver's class selects it (a
    /// vtable entry, or for an interface method a class and its vtable entry). Receivers that
    /// differ in class but share an inherited method (entity subclasses) all pass it.
    fn target_plans(&self, pc: usize, op: u8, m: &'static Method) -> Vec<(&'static Method, Guard)> {
        let recorded = crate::jit::profile::recorded_receivers(self.sc.m, pc);
        let targets: Vec<&'static Method> = recorded.iter().filter_map(|&k| crate::link::select_method(k, m)).collect();
        let [a, b] = targets.as_slice() else { return Vec::new() };
        if !std::ptr::eq(*a, *b) || a.is_abstract() {
            return Vec::new();
        }
        let guard = if op == 0xb6 && m.vtable_index() >= 0 && !m.holder.is_interface() {
            Guard::Vtable(m.vtable_index())
        } else {
            match super::cha::interface_guard(a) {
                Some((c, i)) => Guard::Subclass(c, i),
                None => return Vec::new(),
            }
        };
        vec![(*a, guard)]
    }

    /// Ends the current block with the guard's test; returns the failure block, and continues in
    /// the success block.
    fn guard(&mut self, guard: Guard, target: &'static Method, receiver: Option<V>) -> Option<B> {
        let (entry, expect) = match guard {
            Guard::None => return None,
            Guard::Unique(pre) => {
                let flag = *self.g.deps.get_or_insert_with(|| crate::low::leak(std::sync::atomic::AtomicBool::new(false)));
                target.add_dependent(flag);
                let v = self.add(Inst::FlagCheck(flag as *const std::sync::atomic::AtomicBool as usize, pre), Ty::I);
                if let Some(r) = receiver {
                    self.g.flag_receivers.insert(v, r);
                }
                return None;
            }
            Guard::Klass(k) => (self.klass_of(receiver?), k as *const Klass as i64),
            Guard::Subclass(c, i) => {
                // Two tests failing to one block: the class's ancestor at c's depth, then its
                // vtable entry.
                let k = self.klass_of(receiver?);
                let d = crate::jit::emit_obj::primary_depth(c).expect("a guarded class in the display");
                let at = (std::mem::offset_of!(Klass, primary) + 8 * d) as u32;
                let p = self.add(Inst::LoadField(k, at, BasicType::Long), Ty::J);
                let cc = self.konst(Ty::J, c as *const Klass as i64);
                let (mid, other) = (self.g.new_block(-1), self.g.new_block(-1));
                self.g.blocks[mid as usize].preds.push(self.cur);
                self.g.blocks[other as usize].preds.push(self.cur);
                self.g.blocks[self.cur as usize].term = Term::If { cmp: Cmp::Eq, ty: Ty::J, a: p, b: cc, then: mid, other, unordered: false };
                self.start_block(mid);
                let e = self.add(Inst::LoadField(k, (crate::klass::VTABLE_OFFSET + 8 * i as usize) as u32, BasicType::Long), Ty::J);
                let t = self.konst(Ty::J, target as *const Method as i64);
                let then = self.g.new_block(-1);
                self.g.blocks[then as usize].preds.push(self.cur);
                self.g.blocks[other as usize].preds.push(self.cur);
                self.g.blocks[self.cur as usize].term = Term::If { cmp: Cmp::Eq, ty: Ty::J, a: e, b: t, then, other, unordered: false };
                self.start_block(then);
                return Some(other);
            }
            Guard::Vtable(i) => {
                let k = self.klass_of(receiver?);
                (self.add(Inst::LoadField(k, (crate::klass::VTABLE_OFFSET + 8 * i as usize) as u32, BasicType::Long), Ty::J), target as *const Method as i64)
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

    /// The class `v` is known to have exactly here, if any (when it is not null): an
    /// allocation's or a constant object's anywhere (an inlined lambda factory's result
    /// outlives the inlined body), a cast to or an element of a final class, a complete phi
    /// whose inputs agree (a boxing method's cached or fresh box), else what a class guard
    /// established on this path.
    pub(super) fn exact_class(&self, v: V) -> Option<&'static Klass> {
        self.exact_class_within(v, 4, false)
    }

    /// The class a call's receiver has if it is not null: a merge of an allocation with null
    /// (a method returning a new iterator, or null when there is nothing to iterate) is the
    /// allocation's class, as a null receiver throws before any dispatch.
    pub(super) fn exact_receiver_class(&self, v: V) -> Option<&'static Klass> {
        self.exact_class_within(v, 4, true)
    }

    fn exact_class_within(&self, v: V, depth: u32, or_null: bool) -> Option<&'static Klass> {
        let r = self.g.resolve(v);
        let leaf = |k: &'static Klass| matches!(k.kind, crate::klass::KlassKind::Instance) && k.access & crate::klass::ACC_FINAL != 0;
        match self.g.inst(r) {
            Inst::New(k, _) | Inst::NewArray(k, ..) | Inst::NewMulti(k, ..) => return Some(k),
            Inst::CheckCast(x, k, _) => {
                if leaf(k) {
                    return Some(k);
                }
                // Only null passes a cast to a class its exact one is not a subtype of.
                if depth > 0 {
                    if let Some(c) = self.exact_class_within(*x, depth - 1, or_null).filter(|c| c.is_subtype_of(k)) {
                        return Some(c);
                    }
                }
            }
            Inst::LoadIndexed(a, _, BasicType::Reference) if depth > 0 => {
                if let Some(crate::klass::KlassKind::ObjArray { elem }) = self.exact_class_within(*a, depth - 1, false).map(|k| &k.kind) {
                    if leaf(elem) {
                        return Some(elem);
                    }
                }
            }
            // Inputs that agree; else what a guard established for the phi itself (below).
            Inst::Phi(inputs) if depth > 0 && self.complete_phis.contains(&r) => {
                let agreed = (|| {
                    let mut k: Option<&'static Klass> = None;
                    for &x in inputs {
                        if or_null && self.g.ty(r) == Ty::A && self.g.constant_value(x) == Some(0) {
                            continue;
                        }
                        let c = self.exact_class_within(x, depth - 1, or_null)?;
                        if k.is_some_and(|k| !std::ptr::eq(k, c)) {
                            return None;
                        }
                        k = Some(c);
                    }
                    k
                })();
                if agreed.is_some() {
                    return agreed;
                }
            }
            _ => {}
        }
        if let Some(&k) = self.declared.get(&r) {
            return Some(k);
        }
        if let Some(o) = self.g.constant(r).filter(|&o| o != 0 && self.g.ty(r) == Ty::A) {
            return Some(crate::jit::fold::class_of(o as usize));
        }
        self.exact.iter().rev().find(|&&(x, _)| self.g.resolve(x) == r).map(|&(_, k)| k).or_else(|| self.facts.exact(v))
    }

    /// The receiver's class word: 32 bits at offset 0, zero-extended (classes live in low memory).
    fn klass_of(&mut self, receiver: V) -> V {
        self.add(Inst::LoadField(receiver, 0, BasicType::Int), Ty::J)
    }

    /// `target` as intrinsic `i` on `args` in the current block, which then goes to `k`.
    fn intrinsic_body(&mut self, i: crate::jit::intrinsics::Intrinsic, target: &'static Method, args: &[V], k: B) -> Vec<(B, Option<V>)> {
        let ret = target.sig.ret;
        let v = self.intrinsic_inst(i, args.to_vec(), None, if ret == BasicType::Void { Ty::I } else { Ty::of(ret) });
        self.g.blocks[self.cur as usize].term = Term::Goto(k);
        self.g.blocks[k as usize].preds.push(self.cur);
        vec![(self.cur, (ret != BasicType::Void).then_some(v))]
    }

    /// Builds `target` from the current block with `args` as its locals; returns its exits into
    /// the continuation block `k`.
    fn inline_body(&mut self, target: &'static Method, args: &[V], s: Rc<State>, k: B, first: Option<i64>) -> Option<Vec<(B, Option<V>)>> {
        let frame = self.sc.frame + self.sc.m.code?.max_locals as i32 + crate::thread::header::SLOTS as i32 + s.stack.len() as i32;
        let weight = self.sc.frequency(s.bci as usize);
        let mut sc = Scope::new(target, Some(s.clone()), frame)?;
        sc.weight = weight;
        let mut slot = 0;
        for &a in args {
            sc.locals[slot] = Some(a);
            slot += if self.g.ty(a).wide() { 2 } else { 1 };
        }
        sc.cont = Some(k);
        // A throw the body does not catch itself goes to the caller's handler of this call.
        let caught = self.sc.throw_to.get(&(s.bci as usize)).copied().filter(|_| self.sc.code[s.bci as usize] != 0xbf);
        if let Some((handler, catch)) = caught {
            sc.catch_out = Some((self.g.new_block(-1), handler, catch));
        }
        // A synchronized method takes its monitor (the receiver, already null-checked, or the
        // class's mirror) before its body, at the call; each return releases it, and a
        // deoptimization inside releases it once the interpreter finishes the method (`deopt.rs`).
        if target.is_synchronized() {
            let o = if target.is_static() { self.konst(Ty::A, target.holder.mirror() as usize as i64) } else { args[0] };
            self.add(Inst::Monitor(true, o, s.clone()), Ty::I);
            sc.lock = Some(o);
        }
        self.extent = self.extent.max(sc.frame_end());
        self.spent += sc.code.len();
        self.chain.push((target, first));
        let caller = std::mem::replace(&mut self.sc, sc);
        self.enter_scope();
        let ok = self.build_scope();
        let callee = std::mem::replace(&mut self.sc, caller);
        self.chain.pop();
        ok?;
        // The body is built: every merge in it has all its inputs (a ternary's result, say, a
        // new object or null, whose class calls on it may then dispatch by).
        self.complete_phis.extend(callee.phis.values().flatten().flatten().copied());
        if let (Some((exit, handler, _)), false) = (callee.catch_out, callee.catch_exits.is_empty()) {
            self.catch_from_body(exit, handler, &callee.catch_exits, s.bci as usize);
        }
        Some(callee.returns)
    }

    /// The caller's side of an inlined body's exceptional exits: block `exit` merges the
    /// exceptions and jumps to the caller's `handler` with the caller's locals at the call and
    /// the exception alone on its stack. The current block and stack are left as they were.
    fn catch_from_body(&mut self, exit: B, handler: usize, exits: &[(B, V)], bci: usize) {
        let (cur, stack, facts) = (self.cur, self.sc.stack.clone(), self.facts.clone());
        // The current block's caches stay its own (the exit's path knows other things).
        let caches = self.take_caches();
        self.start_block(exit);
        let exc = match exits {
            [(_, v)] => *v,
            _ => {
                let p = self.add(Inst::Phi(exits.iter().map(|e| e.1).collect()), Ty::A);
                self.complete_phis.insert(p);
                p
            }
        };
        self.sc.stack.clear();
        self.push(exc);
        self.facts.push(exc);
        self.sc.bci = bci;
        self.goto(handler);
        let _ = self.take_caches();
        self.exits.remove(&cur);
        self.cur = cur;
        self.sc.stack = stack;
        self.facts = facts;
        self.set_caches(caches);
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
            facts: self.facts.clone(),
            caches: (self.numbered.clone(), self.loads.clone(), self.bounds.clone()),
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
        self.facts = m.facts;
        self.complete_phis.retain(|&v| (v as usize) < m.values);
        self.declared.retain(|&v, _| (v as usize) < m.values);
        self.boxes.retain(|&v, _| (v as usize) < m.values);
        // The abandoned body's values are gone: so are the facts about them.
        self.set_caches(m.caches);
        self.forget_blocks(m.blocks as B);
    }
}
