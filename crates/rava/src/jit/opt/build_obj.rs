//! Graph building of field access, calls and allocation (the parts of `build_ops.rs` that
//! resolve constant-pool references).

use super::build::Builder;
use super::ir::*;
use crate::klass::{InitState, Klass, ACC_ABSTRACT, ACC_FINAL, ACC_INTERFACE};
use crate::link::Resolved;
use crate::method::Method;
use crate::types::BasicType;
use std::rc::Rc;

impl Builder {
    pub fn init_check(&mut self, k: &'static Klass, s: &Rc<State>) {
        if k.init.state() != InitState::Initialized {
            self.add(Inst::InitCheck(k, s.clone()), Ty::I);
        }
    }

    pub(super) fn field(&mut self, pc: usize, op: u8) -> Option<()> {
        // Constant fields fold (`jit/fold.rs`).
        let index = self.u2(pc);
        let resolved = self.resolved(index)?;
        let folded = match (op, resolved) {
            (0xb2, Resolved::StaticField { .. }) => crate::jit::fold::static_field(self.sc.m, index),
            (0xb4, Resolved::InstanceField { .. }) => {
                let o = self.sc.stack.last().copied().flatten().and_then(|v| self.g.constant(v));
                o.and_then(|o| crate::jit::fold::instance_field(self.sc.m, index, o as usize))
            }
            _ => None,
        };
        if let (Some(c), Resolved::StaticField { ty, .. } | Resolved::InstanceField { ty, .. }) = (folded, resolved) {
            if op == 0xb4 {
                self.pop();
            }
            let dims = crate::jit::fold::stable_dims(self.sc.m, index);
            if dims > 0 && c != 0 {
                self.stable.insert(c, dims);
            }
            self.push_const(Ty::of(ty), c);
            return Some(());
        }
        match (op, resolved) {
            (0xb2, Resolved::StaticField { holder, slot, ty, volatile }) => {
                let s = self.state();
                self.init_check(holder, &s);
                let v = self.add(Inst::LoadStatic(slot as *const _ as usize, ty), Ty::of(ty));
                if volatile {
                    // Acquire: later loads read memory again.
                    self.loads.clear();
                }
                self.push(v);
            }
            (0xb3, Resolved::StaticField { holder, slot, ty, volatile }) => {
                let s = self.state();
                let v = self.pop();
                self.init_check(holder, &s);
                self.add(Inst::StoreStatic(slot as *const _ as usize, ty, v), Ty::I);
                if volatile {
                    self.add(Inst::Fence, Ty::I);
                }
            }
            (0xb4, Resolved::InstanceField { offset, ty, volatile }) => {
                let s = self.state();
                let o = self.pop();
                self.null_check(o, s);
                // A load the block already did (or a store it made) since the last call, store
                // to a field at this offset, or other memory effect, is reused.
                let v = match self.loads.get(&(o, offset)) {
                    Some(&v) if !volatile => v,
                    _ => {
                        let v = self.add(Inst::LoadField(o, offset, ty), Ty::of(ty));
                        if volatile {
                            // Acquire: later loads read memory again.
                            self.loads.clear();
                            self.g.ordered.insert(v);
                        } else {
                            self.loads.insert((o, offset), v);
                        }
                        v
                    }
                };
                self.push(v);
            }
            (0xb5, Resolved::InstanceField { offset, ty, volatile }) => {
                let s = self.state();
                let v = self.pop();
                let o = self.pop();
                self.null_check(o, s);
                self.add(Inst::StoreField(o, offset, ty, v), Ty::I);
                if volatile {
                    self.add(Inst::Fence, Ty::I);
                }
            }
            _ => return None,
        }
        Some(())
    }

    /// Returns whether the current block continues after the call.
    pub(super) fn invoke(&mut self, pc: usize, op: u8) -> Option<bool> {
        let Resolved::Method(m) = self.resolved(self.u2(pc))? else { return None };
        if m.is_signature_polymorphic() {
            let (_, _, desc) = self.sc.cf.member_ref(self.u2(pc))?;
            return self.polymorphic(pc, op, m, desc);
        }
        if (op == 0xb8) != m.is_static() {
            return None;
        }
        // Checks fail before the call (arguments still on the stack); an exception thrown by
        // the callee resumes at the invoke with only the handler search left.
        let pre = self.state();
        let n = m.sig.params.len() + (op != 0xb8) as usize;
        let mut args: Vec<V> = (0..n).map(|_| self.pop()).collect();
        args.reverse();
        let s = self.state();
        let callee = match op {
            0xb8 => {
                self.init_check(m.holder, &pre);
                Callee::Direct(m)
            }
            0xb7 => Callee::Direct(crate::interp::special_target(self.sc.m.holder, m)),
            _ if m.is_private() || m.access & ACC_FINAL != 0 || m.holder.access & ACC_FINAL != 0 => Callee::Direct(m),
            0xb6 if m.vtable_index() >= 0 && !m.holder.is_interface() => Callee::Virtual(m),
            _ => Callee::Interface(m),
        };
        if op != 0xb8 {
            self.null_check(args[0], pre.clone());
        }
        if self.allocate_instance(m, &args, &pre, &s) || self.fold_class_query(m, &args) || self.floor_pow2(m, &args) {
            return Some(true);
        }
        if let (0xb6 | 0xb8, Some(k)) = (op, crate::jit::intrinsics::bound_intrinsic(m)) {
            let ret = m.sig.ret;
            // The class of an object whose class is known exactly is a constant.
            if matches!(k, crate::jit::intrinsics::Intrinsic::GetClass) {
                let mirror = self.exact_class(args[0]).map(|c| c.mirror()).filter(|m| !m.is_null());
                if let Some(mirror) = mirror {
                    let c = self.konst(Ty::A, mirror as usize as i64);
                    self.push(c);
                    return Some(true);
                }
            }
            if let crate::jit::intrinsics::Intrinsic::Leaf(l) = k {
                for &i in l.array_args() {
                    self.null_check(args[i], pre.clone());
                }
            }
            let s = matches!(k, crate::jit::intrinsics::Intrinsic::GetClass | crate::jit::intrinsics::Intrinsic::ArrayCopy).then_some(pre);
            let v = self.add(Inst::Intrinsic(k, args, s), if ret == BasicType::Void { Ty::I } else { Ty::of(ret) });
            if ret != BasicType::Void {
                self.push(v);
            }
            return Some(true);
        }
        Some(self.call_or_inline(pc, op, m, callee, args, s, pre))
    }

    /// `Math.floorMod` and `Math.floorDiv` by a constant positive power of two `2^k` are
    /// `x & (2^k - 1)` and `x >> k` in two's complement, with no division and no sign fix-up
    /// branch (which mispredicts on hashed operands).
    fn floor_pow2(&mut self, m: &'static Method, args: &[V]) -> bool {
        if m.holder.name != "java/lang/Math" || args.len() != 2 {
            return false;
        }
        let (x, y) = (args[0], args[1]);
        let Some(c) = self.g.constant(y).filter(|&c| c > 0 && c & (c - 1) == 0) else { return false };
        let (xt, rt) = match m.desc.as_str() {
            "(II)I" => (Ty::I, Ty::I),
            "(JI)I" if m.name == "floorMod" => (Ty::J, Ty::I),
            "(JI)J" if m.name == "floorDiv" => (Ty::J, Ty::J),
            "(JJ)J" => (Ty::J, Ty::J),
            _ => return false,
        };
        let r = match m.name.as_str() {
            "floorMod" => {
                let mask = self.konst(xt, c - 1);
                let r = self.add(Inst::Bin(BinOp::And, x, mask), xt);
                if rt == xt { r } else { self.add(Inst::Convert(0x88, r), Ty::I) }
            }
            "floorDiv" => {
                let k = self.konst(Ty::I, c.trailing_zeros() as i64);
                self.add(Inst::Bin(BinOp::Shr, x, k), xt)
            }
            _ => return false,
        };
        self.push(r);
        true
    }

    /// `Class` queries on a constant class (as `Enum.getDeclaringClass` asks of an enum
    /// constant's class) are constants: `getSuperclass`, `isInterface`, `isArray`,
    /// `isPrimitive`.
    fn fold_class_query(&mut self, m: &'static Method, args: &[V]) -> bool {
        if m.holder.name != "java/lang/Class" || args.len() != 1 {
            return false;
        }
        let Some(mirror) = self.g.constant(args[0]).filter(|&c| c != 0) else { return false };
        let k = crate::runtime::klass_of_mirror(mirror as crate::heap::Oop);
        let (v, ty) = match (m.name.as_str(), m.desc.as_str()) {
            ("getSuperclass", "()Ljava/lang/Class;") => {
                let sup = match k {
                    Some(k) if k.is_interface() => std::ptr::null_mut(),
                    Some(k) => k.super_klass.map_or(std::ptr::null_mut(), |s| s.mirror()),
                    None => std::ptr::null_mut(),
                };
                // A superclass whose mirror does not exist yet is left to the call.
                if sup.is_null() && k.is_some_and(|k| !k.is_interface() && k.super_klass.is_some()) {
                    return false;
                }
                (sup as usize as i64, Ty::A)
            }
            ("isInterface", "()Z") => (k.is_some_and(|k| k.is_interface()) as i64, Ty::I),
            ("isArray", "()Z") => (k.is_some_and(|k| !matches!(k.kind, crate::klass::KlassKind::Instance)) as i64, Ty::I),
            ("isPrimitive", "()Z") => (k.is_none() as i64, Ty::I),
            _ => return false,
        };
        let c = self.konst(ty, v);
        self.push(c);
        true
    }

    /// `Unsafe.allocateInstance` of a constant, instantiable class (as method-handle plumbing
    /// creates lambda objects): an ordinary `new`.
    fn allocate_instance(&mut self, m: &'static Method, args: &[V], pre: &Rc<State>, s: &Rc<State>) -> bool {
        if m.holder.name != "jdk/internal/misc/Unsafe" || m.name != "allocateInstance" || args.len() != 2 {
            return false;
        }
        let Some(k) = self.g.constant(args[1]).and_then(|c| crate::runtime::klass_of_mirror(c as crate::heap::Oop)) else { return false };
        let instantiable = matches!(k.kind, crate::klass::KlassKind::Instance) && k.access & (ACC_ABSTRACT | ACC_INTERFACE) == 0 && k.name != "java/lang/Class";
        if !instantiable {
            return false;
        }
        self.init_check(k, pre);
        let v = self.add(Inst::New(k, s.clone()), Ty::A);
        self.exact.push((v, k));
        self.nonnull.push(v);
        self.push(v);
        true
    }

    /// `checkcast` / `instanceof` of a class not resolved yet: null passes (without resolving,
    /// as in the interpreter), anything else deoptimizes to resolve it.
    pub(super) fn unresolved_type_check(&mut self, op: u8) -> Option<bool> {
        let s = self.state();
        let v = self.pop();
        let null = self.konst(Ty::A, 0);
        let (ok, resolve) = (self.g.new_block(-1), self.g.new_block(-1));
        self.g.blocks[ok as usize].preds.push(self.cur);
        self.g.blocks[resolve as usize].preds.push(self.cur);
        self.g.blocks[self.cur as usize].term = Term::If { cmp: Cmp::Eq, ty: Ty::A, a: v, b: null, then: ok, other: resolve, unordered: false };
        self.start_block(resolve);
        self.g.blocks[resolve as usize].term = Term::Deopt(s);
        self.start_block(ok);
        if op == 0xc0 {
            self.push(v);
        } else {
            self.push_const(Ty::I, 0);
        }
        Some(true)
    }
}
