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
    /// The value in box `o` when the field read at `pc` is its `value` (`Builder::boxes`).
    fn unboxed(&self, o: V, pc: usize) -> Option<V> {
        let &(k, x) = self.boxes.get(&self.g.resolve(o))?;
        let (holder, f) = crate::jit::fold::field_decl(self.sc.m, self.u2(pc))?;
        (std::ptr::eq(holder, k) && f.name.as_str() == "value").then_some(x)
    }

    /// Records the class a field load at `pc` has exactly when its field is declared with a
    /// final class already loaded, or an array of one (`Builder::declared`).
    fn note_declared(&mut self, v: V, pc: usize) {
        let Some((holder, f)) = crate::jit::fold::field_decl(self.sc.m, self.u2(pc)) else { return };
        if let Some(k) = declared_class(holder, f.desc.as_str()) {
            self.declared.insert(v, k);
        }
    }

    /// Records the exact classes of the compiled method `m`'s parameters (`args`, the receiver
    /// first unless static) declared with a final class or an array of one, as C2 types
    /// parameters by the signature. Only the root's: an inlined body's parameter is a value of
    /// its caller, whose type there may hold only on the guarded path the body is inlined on
    /// (a receiver passed on as `this`).
    pub(super) fn note_declared_params(&mut self, m: &'static Method, args: &[V]) {
        let desc = m.desc.as_str();
        let Some(params) = desc.strip_prefix('(').and_then(|d| d.split(')').next()) else { return };
        let mut rest = params;
        let mut args = args.iter().skip(usize::from(!m.is_static()));
        while !rest.is_empty() {
            let dims = rest.len() - rest.trim_start_matches('[').len();
            let end = match rest.as_bytes()[dims] {
                b'L' => match rest.find(';') {
                    Some(i) => i + 1,
                    None => return,
                },
                _ => dims + 1,
            };
            let (one, tail) = rest.split_at(end);
            rest = tail;
            let Some(&v) = args.next() else { return };
            if let Some(k) = declared_class(m.holder, one) {
                self.declared.insert(self.g.resolve(v), k);
            }
        }
    }

    pub fn init_check(&mut self, k: &'static Klass, s: &Rc<State>) {
        if k.init.state() != InitState::Initialized {
            self.add(Inst::InitCheck(k, s.clone()), Ty::I);
        }
    }

    /// Adds intrinsic `k` on `args`. `Class.isInstance` of a constant class is that class's
    /// `instanceof` (a method handle's guard, as a record's `equals` has): the inline class
    /// tests and the passes that sharpen and fuse them apply.
    pub(super) fn intrinsic_inst(&mut self, k: crate::jit::intrinsics::Intrinsic, args: Vec<V>, s: Option<Rc<State>>, ty: Ty) -> V {
        if let crate::jit::intrinsics::Intrinsic::IsInstance = k {
            let class = self.constant_object(args[0]).filter(|&m| m != 0).and_then(|m| crate::runtime::klass_of_mirror(m as crate::heap::Oop));
            if let Some(class) = class {
                return self.add(Inst::InstanceOf(args[1], class), Ty::I);
            }
        }
        // A plain Unsafe read of an allocation of this compilation at a constant offset is that
        // field's load (C2 does the same): escape analysis can then replace the object, as for
        // the getters of a record's `equals` and `hashCode` (`ObjectMethods`).
        if let (crate::jit::intrinsics::Intrinsic::UnsafeGet(bt, false), [_, base, off]) = (k, &args[..]) {
            let base = self.g.resolve(*base);
            if let (Inst::New(class, _), Some(off)) = (self.g.inst(base), self.g.constant(self.g.resolve(*off))) {
                let class: &'static crate::klass::Klass = class;
                let declared = std::iter::successors(Some(class), |c| c.super_klass).flat_map(|c| c.fields.iter()).any(|f| !f.is_static() && f.offset as i64 == off && f.ty == bt);
                if declared {
                    return self.add(Inst::LoadField(base, off as u32, bt), ty);
                }
            }
        }
        self.add(Inst::Intrinsic(k, args, s), ty)
    }

    /// The constant object `v` is, looking through casts of it (a lambda form casts its bound
    /// handle to the species class before reading the bound values).
    pub(super) fn constant_object(&self, v: V) -> Option<i64> {
        let mut v = self.g.resolve(v);
        for _ in 0..4 {
            match *self.g.inst(v) {
                Inst::Const(c) => return Some(c),
                Inst::CheckCast(x, ..) => v = self.g.resolve(x),
                _ => return None,
            }
        }
        None
    }

    pub(super) fn field(&mut self, pc: usize, op: u8) -> Option<()> {
        // Constant fields fold (`jit/fold.rs`).
        let index = self.u2(pc);
        let resolved = self.resolved(index)?;
        let folded = match (op, resolved) {
            (0xb2, Resolved::StaticField { .. }) => crate::jit::fold::static_field(self.sc.m, index),
            (0xb4, Resolved::InstanceField { .. }) => {
                let o = self.sc.stack.last().copied().flatten().and_then(|v| self.constant_object(v));
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
                self.note_declared(v, pc);
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
                if let Some(x) = self.unboxed(o, pc) {
                    self.push(x);
                    return Some(());
                }
                self.null_check(o, s);
                // A load the block already did (or a store it made) since the last call, store
                // to a field at this offset, or other memory effect, is reused.
                let v = match self.loads.get(&(o, offset)) {
                    Some(&v) if !volatile => v,
                    _ => {
                        let v = self.add(Inst::LoadField(o, offset, ty), Ty::of(ty));
                        self.note_declared(v, pc);
                        let in_init = self.chain.iter().any(|(m, _)| m.name == "<init>");
                        if !in_init && crate::jit::fold::field_decl(self.sc.m, self.u2(pc)).is_some_and(|(_, f)| f.access & ACC_FINAL != 0) {
                            self.g.final_loads.insert(v);
                        }
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
        if self.allocate_instance(m, &args, &pre, &s) || self.new_array_reflective(m, &args, &pre) || self.fold_class_query(m, &args) || self.floor_pow2(m, &args) || self.caller_class(m) || self.mh_hints(m, &args) {
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
            // A copy into a primitive array of a known class copies inline (`codegen_copy`).
            let copy = matches!(k, crate::jit::intrinsics::Intrinsic::ArrayCopy);
            let into = copy.then(|| self.exact_class(args[2])).flatten();
            let from = copy.then(|| self.exact_class(args[0])).flatten();
            let stable_src = copy && self.constant_object(args[0]).is_some_and(|c| self.stable.get(&c).is_some_and(|&d| d >= 1));
            let v = self.intrinsic_inst(k, args, s, if ret == BasicType::Void { Ty::I } else { Ty::of(ret) });
            if let Some(into) = into.filter(|k| matches!(k.kind, crate::klass::KlassKind::TypeArray { .. })) {
                self.g.copy_into.insert(v, into);
            }
            if let Some(from) = from {
                self.g.copy_from.insert(v, from);
            }
            if stable_src {
                self.g.copy_stable.insert(v);
            }
            if ret != BasicType::Void {
                self.push(v);
            }
            return Some(true);
        }
        Some(self.call_or_inline(pc, op, m, callee, args, s, pre))
    }

    /// The method-handle runtime's hints to the compiler (C2's intrinsics): `profileBoolean`
    /// counts a guard's outcomes for the compiler and returns the outcome, so compiled code takes
    /// the outcome without counting (two read-modify-writes of a shared array per inlined
    /// `guardWithTest`, such as every record `equals`); `isCompileConstant` is whether its
    /// argument is a constant of this compilation.
    fn mh_hints(&mut self, m: &'static Method, args: &[V]) -> bool {
        if m.holder.name != "java/lang/invoke/MethodHandleImpl" {
            return false;
        }
        let v = match (m.name.as_str(), m.desc.as_str()) {
            ("profileBoolean", "(Z[I)Z") => args[0],
            ("isCompileConstant", "(Ljava/lang/Object;)Z") => {
                let c = self.g.constant(self.g.resolve(args[0])).is_some();
                self.konst(Ty::I, c as i64)
            }
            _ => return false,
        };
        self.push(v);
        true
    }

    /// `Reflection.getCallerClass()` in a body inlined into its caller's caller (C2's intrinsic):
    /// the frames above it are this compilation's inline chain, so the class it would find by
    /// walking the stack (the first frame past the caller-sensitive method that is not
    /// reflection machinery, `natives::reflect::caller_class`) is a constant. Each
    /// `MethodHandles.lookup()` otherwise walked the stack.
    fn caller_class(&mut self, m: &'static Method) -> bool {
        if m.holder.name != "jdk/internal/reflect/Reflection" || m.name != "getCallerClass" || m.desc != "()Ljava/lang/Class;" {
            return false;
        }
        let n = self.chain.len();
        let Some(&(caller, _)) = self.chain[..n.saturating_sub(1)].iter().rev().find(|(c, _)| !crate::natives::reflect::is_reflection_frame(c.holder)) else { return false };
        let mirror = caller.holder.mirror();
        if mirror.is_null() {
            return false;
        }
        let c = self.konst(Ty::A, mirror as usize as i64);
        self.push(c);
        true
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
            // An object array's element class (a primitive array's is left to the call).
            ("getComponentType", "()Ljava/lang/Class;") => match k.map(|k| &k.kind) {
                Some(crate::klass::KlassKind::ObjArray { elem }) if !elem.mirror().is_null() => (elem.mirror() as usize as i64, Ty::A),
                Some(crate::klass::KlassKind::Instance) | None => (0, Ty::A),
                _ => return false,
            },
            _ => return false,
        };
        let c = self.konst(ty, v);
        self.push(c);
        true
    }

    /// `Array.newArray` (behind `Array.newInstance` and `Arrays.copyOf`) of a constant class whose
    /// array class exists: an ordinary allocation (C2's `_newArray`), deoptimizing to the call for
    /// a negative length. `Unsafe.allocateUninitializedArray0` of a primitive class (string
    /// concatenation's buffer, which its caller fills completely) is one whose elements are not
    /// cleared (C2's `_allocateUninitializedArray`).
    fn new_array_reflective(&mut self, m: &'static Method, args: &[V], pre: &Rc<State>) -> bool {
        let uninit = m.holder.name == "jdk/internal/misc/Unsafe" && m.name == "allocateUninitializedArray0" && args.len() == 3;
        if !uninit && (m.holder.name != "java/lang/reflect/Array" || m.name != "newArray" || args.len() != 2) {
            return false;
        }
        let (class, len) = (args[args.len() - 2], args[args.len() - 1]);
        let Some(mirror) = self.g.constant(class).filter(|&c| c != 0).map(|c| c as crate::heap::Oop) else { return false };
        let k = if uninit {
            // A primitive class's array class (any other class is left to the call).
            let Some(t) = crate::runtime::primitive_type_of_mirror(mirror).filter(|&t| t != BasicType::Void) else { return false };
            let Ok(k) = crate::vm::vm().primitive_array_class(t) else { return false };
            k
        } else {
            let Some(elem) = crate::runtime::klass_of_mirror(mirror).filter(|e| e.name != "void") else { return false };
            let Some(&k) = elem.array_klass.get() else { return false };
            k
        };
        let v = self.add(Inst::NewArray(k, len, pre.clone()), Ty::A);
        if uninit {
            self.g.uninit.insert(v);
        }
        self.exact.push((v, k));
        self.push(v);
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
        self.facts.push(v);
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

/// `Integer.valueOf(int)` and its siblings for long, short, byte and char: a box (cached or
/// new) holding exactly the argument.
pub(super) fn box_value_of(m: &'static Method) -> bool {
    let desc = match m.holder.name.as_str() {
        "java/lang/Integer" => "(I)Ljava/lang/Integer;",
        "java/lang/Long" => "(J)Ljava/lang/Long;",
        "java/lang/Short" => "(S)Ljava/lang/Short;",
        "java/lang/Byte" => "(B)Ljava/lang/Byte;",
        "java/lang/Character" => "(C)Ljava/lang/Character;",
        _ => return false,
    };
    m.is_static() && m.name.as_str() == "valueOf" && m.desc.as_str() == desc
}

/// The class a value declared with type `desc` in `holder` has exactly when not null: a final
/// class already loaded, a primitive array, or an array of either whose array class exists (no
/// other class is assignable to any of them).
fn declared_class(holder: &'static Klass, desc: &str) -> Option<&'static Klass> {
    let elem = desc.trim_start_matches('[');
    let mut dims = desc.len() - elem.len();
    let mut k = match elem.strip_prefix('L').and_then(|d| d.strip_suffix(';')) {
        Some(name) => {
            let k = crate::vm::vm().find_loaded(holder.loader(), crate::symbol::Symbol::intern(name))?;
            if !matches!(k.kind, crate::klass::KlassKind::Instance) || k.access & ACC_FINAL == 0 {
                return None;
            }
            k
        }
        None if dims > 0 && elem.len() == 1 && "ZCFDBSIJ".contains(elem) => {
            dims -= 1;
            crate::vm::vm().primitive_array_class(BasicType::from_desc(elem.as_bytes()[0])).ok()?
        }
        None => return None,
    };
    for _ in 0..dims {
        k = k.array_klass.get().copied()?;
    }
    Some(k)
}
