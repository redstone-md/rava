//! Code generation of field, array, allocation, type-check and call instructions.

use super::codegen::{Arg, Gen, SlowCall};
use super::ir::*;
use super::regalloc::Loc;
use crate::jit::asm::{mem, mem_idx, Alu, Cond, Shift, M, R, X};
use crate::jit::emit_obj::{load_typed_to, normalize_reg, store_typed_from};
use crate::jit::runtime::{self, IcCell};
use crate::klass::Klass;
use crate::method::Method;
use crate::thread::offsets;
use crate::types::BasicType;
use std::mem::offset_of;
use std::rc::Rc;

/// Offset of array element 0.
const BASE: i32 = crate::heap::ARRAY_BASE_OFFSET as i32;

fn scale(bt: BasicType) -> u8 {
    bt.size() as u8
}

impl Gen<'_> {
    pub(super) fn memory_inst(&mut self, v: V) {
        match self.g.inst(v).clone() {
            Inst::LoadField(o, off, bt @ (BasicType::Float | BasicType::Double)) => {
                let at = self.field(o, off as i32);
                let r = self.xout(v);
                self.fault_site();
                self.a.fload(bt == BasicType::Double, r, at);
                self.xput(v, r);
            }
            Inst::StoreField(o, off, bt @ (BasicType::Float | BasicType::Double), x) => {
                let a = self.xreg(x, X(0));
                let ro = self.reg(o, R::Rax);
                self.fault_site();
                self.a.fstore(bt == BasicType::Double, mem(ro, off as i32), a);
            }
            Inst::LoadIndexed(arr, idx, bt @ (BasicType::Float | BasicType::Double)) => {
                let at = self.element(arr, idx, bt);
                let r = self.xout(v);
                self.a.fload(bt == BasicType::Double, r, at);
                self.xput(v, r);
            }
            Inst::StoreIndexed(arr, idx, bt @ (BasicType::Float | BasicType::Double), x) => {
                // A constant goes through r11, which the element's address may need for a
                // spilled index: the value first.
                let a = self.xreg(x, X(0));
                let at = self.element(arr, idx, bt);
                self.a.fstore(bt == BasicType::Double, at, a);
            }
            Inst::LoadStatic(addr, bt @ (BasicType::Float | BasicType::Double)) if matches!(self.loc[v as usize], Loc::Xmm(_)) => {
                let at = self.absolute(addr, R::R11);
                let r = self.xout(v);
                self.a.fload(bt == BasicType::Double, r, at);
            }
            Inst::LoadField(..) if self.fused.contains(&v) => {}
            Inst::LoadField(o, off, bt) => {
                let at = self.field(o, off as i32);
                let r = self.out(v);
                self.fault_site();
                if self.narrow.contains(&v) {
                    self.a.load(false, r, at);
                } else if let Some(user) = self.decode_later(v, bt, r) {
                    self.a.load(false, r, at);
                    self.deferred_decode.push((v, r, user));
                } else {
                    load_typed_to(&mut self.a, r, bt, at);
                }
                self.put(v, r);
            }
            Inst::StoreField(o, off, bt, x) => {
                let ro = self.reg(o, R::Rax);
                let mut rx = self.value_reg(x, bt, R::R11);
                if bt == BasicType::Reference && !self.narrow.contains(&self.g.resolve(x)) {
                    rx = crate::jit::emit_obj::encode_ref(&mut self.a, rx, R::R11);
                }
                self.fault_site();
                self.store_typed(v, rx, bt, mem(ro, off as i32));
            }
            Inst::LoadStatic(addr, bt) => {
                let at = self.absolute(addr, R::R11);
                let r = self.out(v);
                crate::jit::emit_obj::load_static_to(&mut self.a, r, bt, at);
                self.put(v, r);
            }
            // Through rax and r11 (`scratch.rs`).
            Inst::StoreStatic(addr, bt, x) => {
                self.load(R::Rax, x);
                normalize_reg(&mut self.a, bt, R::Rax);
                let at = self.absolute(addr, R::R11);
                self.a.store(true, at, R::Rax);
            }
            Inst::ArrayLength(x) => {
                let at = self.field(x, crate::heap::ARRAY_LENGTH_OFFSET as i32);
                let r = self.out(v);
                self.fault_site();
                self.a.load_sx(4, r, at);
                self.put(v, r);
            }
            Inst::LoadIndexed(arr, idx, bt) => {
                let at = self.element(arr, idx, bt);
                let r = self.out(v);
                if self.narrow.contains(&v) {
                    self.a.load(false, r, at);
                } else {
                    load_typed_to(&mut self.a, r, bt, at);
                }
                self.put(v, r);
            }
            Inst::StoreIndexed(arr, idx, bt, x) => self.store_indexed(v, arr, idx, bt, x),
            Inst::New(k, s) => self.new_instance(v, k, &s),
            Inst::NewArray(k, len, s) => self.new_array(v, k, len, &s),
            Inst::NewMulti(k, counts, s) => {
                let fail = self.deopt(&s);
                self.link(&s, true);
                self.call_preserving(runtime::new_multi2 as *const () as usize, &[Arg::Thread, Arg::Imm(k as *const Klass as i64), Arg::Val(counts)]);
                self.a.test_rr(true, R::Rax, R::Rax);
                self.a.jcc(Cond::E, fail);
                self.put(v, R::Rax);
            }
            Inst::CheckCast(x, k, s) => {
                let (fail, done) = (self.deopt(&s), self.a.new_label());
                let exact = matches!(k.kind, crate::klass::KlassKind::TypeArray { .. })
                    || matches!(k.kind, crate::klass::KlassKind::Instance) && k.access & crate::klass::ACC_FINAL != 0;
                let depth = crate::jit::emit_obj::primary_depth(k);
                if exact || depth.is_some() {
                    // A final class or primitive array is only itself; a class at a primary
                    // depth is the object's class or its ancestor there. Null passes.
                    let rx = self.reg(x, R::Rax);
                    let branch = self.cast_branch.get(&v).copied();
                    if let Some((b, null, _)) = branch {
                        // The result is in place before null leaves for the branch's null side.
                        let dst = self.loc[v as usize];
                        if dst != Loc::None {
                            super::moves::mov(&mut self.a, dst, Loc::Reg(rx));
                        }
                        // So is its home slot, which states on the null side may read.
                        if let Some(h @ Loc::Stack(_)) = self.home[v as usize] {
                            super::moves::mov(&mut self.a, h, Loc::Reg(rx));
                        }
                        let to = self.edge_target(b, null);
                        self.a.test_rr(true, rx, rx);
                        self.a.jcc(Cond::E, to);
                    } else if !self.g.nonnull_input.contains(&v) {
                        self.a.test_rr(true, rx, rx);
                        self.a.jcc(Cond::E, done);
                    }
                    // Through r11 and rax only: rcx and rdx stay with the register allocator
                    // (`scratch.rs`).
                    self.a.load(false, R::R11, mem(rx, 0));
                    let addr = k as *const Klass as usize;
                    let low = crate::low::is_low(addr);
                    if !low {
                        self.a.mov_ri(R::Rax, addr as i64);
                    }
                    let cmp = |a: &mut crate::jit::asm::Asm, m: Option<M>| match (m, low) {
                        (None, true) => a.alu_ri(true, Alu::Cmp, R::R11, addr as i32),
                        (None, false) => a.alu_rr(true, Alu::Cmp, R::R11, R::Rax),
                        (Some(m), true) => a.alu_mi(true, Alu::Cmp, m, addr as i32),
                        (Some(m), false) => a.alu_rm(true, Alu::Cmp, R::Rax, m),
                    };
                    cmp(&mut self.a, None);
                    match depth.filter(|_| !exact) {
                        None => self.a.jcc(Cond::Ne, fail),
                        Some(d) => {
                            self.a.jcc(Cond::E, done);
                            let at = std::mem::offset_of!(Klass, primary) as i32 + 8 * d as i32;
                            cmp(&mut self.a, Some(mem(R::R11, at)));
                            self.a.jcc(Cond::Ne, fail);
                        }
                    }
                } else {
                    self.subtype_check(x, k, done, false, self.g.nonnull_input.contains(&v), Some((s.method, s.bci as usize)));
                    self.a.test_rr(true, R::Rax, R::Rax);
                    self.a.jcc(Cond::E, fail);
                }
                self.a.bind(done);
                let (dst, src) = (self.loc[v as usize], self.loc[x as usize]);
                if dst != Loc::None && !self.cast_branch.contains_key(&v) {
                    super::moves::mov(&mut self.a, dst, src);
                }
            }
            Inst::InstanceOf(..) if self.fused.contains(&v) => {}
            Inst::InstanceOf(x, k) => {
                let (done, null) = (self.a.new_label(), self.a.new_label());
                let nonnull = self.g.nonnull_input.contains(&v);
                if !nonnull {
                    let rx = self.reg(x, R::Rax);
                    self.a.test_rr(true, rx, rx);
                    self.a.jcc(Cond::E, null);
                }
                self.subtype_check(x, k, done, true, true, None);
                self.a.jmp(done);
                self.a.bind(null);
                self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
                self.a.bind(done);
                self.put(v, R::Rax);
            }
            Inst::StoreCheck(arr, x, s) => {
                let (fail, ok) = (self.deopt(&s), self.a.new_label());
                let rx = self.reg(x, R::Rdx);
                self.a.test_rr(true, rx, rx);
                self.a.jcc(Cond::E, ok);
                self.a.load(false, R::Rcx, mem(rx, 0));
                let ra = self.reg(arr, R::Rax);
                self.a.load(false, R::Rdx, mem(ra, 0));
                self.a.load(true, R::Rdx, mem(R::Rdx, offset_of!(Klass, elem_klass) as i32));
                self.a.alu_rr(true, Alu::Cmp, R::Rcx, R::Rdx);
                self.a.jcc(Cond::E, ok);
                // Anything goes into an Object[] (ArrayList's and HashMap's arrays).
                if let Some(&object) = crate::vm::vm().wk.object.get() {
                    let addr = object as *const Klass as usize;
                    if crate::low::is_low(addr) {
                        self.a.alu_ri(true, Alu::Cmp, R::Rdx, addr as i32);
                    } else {
                        self.a.mov_ri(R::R11, addr as i64);
                        self.a.alu_rr(true, Alu::Cmp, R::Rdx, R::R11);
                    }
                    self.a.jcc(Cond::E, ok);
                }
                self.dynamic_subtype(R::Rcx, R::Rdx, ok);
                self.call_preserving(runtime::subtype_noted as *const () as usize, &[Arg::Reg(R::Rcx), Arg::Reg(R::Rdx)]);
                self.a.test_rr(true, R::Rax, R::Rax);
                self.a.jcc(Cond::E, fail);
                self.a.bind(ok);
            }
            Inst::Call(callee, args, s) => self.call(v, callee, &args, &s),
            Inst::Monitor(enter, o, s) => self.monitor(enter, o, &s),
            Inst::Intrinsic(k, args, s) => self.intrinsic(v, k, &args, s.as_ref()),
            other => unreachable!("not a memory instruction: {other:?}"),
        }
    }

    /// A register with `x` for a store of type `bt` (a copy in `scratch` when the store masks
    /// it).
    fn value_reg(&mut self, x: V, bt: BasicType, scratch: R) -> R {
        if bt == BasicType::Boolean {
            self.load(scratch, x);
            return scratch;
        }
        self.reg(x, scratch)
    }

    /// The address of element `idx` of `arr` (a constant index folded into the displacement).
    /// The field at `off` of object `o`, through its compressed reference if it is kept
    /// compressed (`codegen::narrow_refs`).
    /// The instruction right after reference load `v` (past an implicit null check of it) that
    /// reads it only as the object of a field load or array length, when `v`'s register may
    /// hold it compressed until then (no home slot or state written meanwhile).
    fn decode_later(&self, v: V, bt: BasicType, r: R) -> Option<V> {
        if bt != BasicType::Reference || !crate::heap::COMPRESSED || self.loc[v as usize] != Loc::Reg(r) || self.home[v as usize].is_some() {
            return None;
        }
        let base_of = |w: V| match *self.g.inst(w) {
            Inst::LoadField(o, ..) => self.g.resolve(o) == v && !self.fused.contains(&w),
            Inst::ArrayLength(o) => self.g.resolve(o) == v,
            _ => false,
        };
        match self.next {
            [Some(w), _] if base_of(w) => Some(w),
            [Some(n), Some(w)] if matches!(*self.g.inst(n), Inst::NullCheck(o, _) if self.g.resolve(o) == v) && self.implicit.contains(&n) && base_of(w) => Some(w),
            _ => None,
        }
    }

    pub(super) fn field(&mut self, o: V, off: i32) -> M {
        if let Some(&(_, r, _)) = self.deferred_decode.iter().find(|&&(x, ..)| x == self.g.resolve(o)) {
            return crate::jit::asm::mem_narrow(r, off);
        }
        let ro = self.reg(o, R::Rax);
        if self.narrow.contains(&self.g.resolve(o)) {
            crate::jit::asm::mem_narrow(ro, off)
        } else {
            mem(ro, off)
        }
    }

    fn element(&mut self, arr: V, idx: V, bt: BasicType) -> M {
        let ra = self.reg(arr, R::Rax);
        if let Loc::Const(c) = self.loc[idx as usize] {
            if let Some(d) = c.checked_mul(scale(bt) as i64).and_then(|d| i32::try_from(d + BASE as i64).ok()) {
                return mem(ra, d);
            }
        }
        let ri = self.reg(idx, R::R11);
        mem_idx(ra, ri, scale(bt), BASE)
    }

    /// The store `v` of `src` to `m`, with a card mark unless it initializes a young object.
    fn store_typed(&mut self, v: V, src: R, bt: BasicType, m: M) {
        if self.unbarriered[v as usize] {
            crate::jit::emit_obj::store_raw(&mut self.a, src, bt, m);
        } else {
            store_typed_from(&mut self.a, src, bt, m);
        }
    }

    fn store_indexed(&mut self, v: V, arr: V, idx: V, bt: BasicType, x: V) {
        if bt != BasicType::Byte {
            let at = self.element(arr, idx, bt);
            let mut rx = self.value_reg(x, bt, R::Rdx);
            if bt == BasicType::Reference {
                rx = crate::jit::emit_obj::encode_ref(&mut self.a, rx, R::Rdx);
            }
            return self.store_typed(v, rx, bt, at);
        }
        // bastore: boolean[] stores value & 1, byte[] the low byte. The class test uses r11
        // before the element's address may need it for a spilled index. An array of a known
        // class (allocated here, or a constant), or a value of 0 or 1, needs no test.
        self.load(R::Rdx, x);
        let ra = self.reg(arr, R::Rax);
        let bools = crate::vm::vm().primitive_array_class(BasicType::Boolean).ok().map_or(0, |k| k as *const Klass as i64);
        let known = self.known_array_class(arr).map(|k| k as *const Klass as i64);
        let bit = self.g.constant(self.g.resolve(x)).is_some_and(|c| c == 0 || c == 1);
        if known == Some(bools) && !bit {
            self.a.alu_ri(false, Alu::And, R::Rdx, 1);
        } else if known.is_none() && !bit {
            let plain = self.a.new_label();
            self.a.mov_ri(R::R11, bools);
            self.a.alu_rm(false, Alu::Cmp, R::R11, mem(ra, 0));
            self.a.jcc(Cond::Ne, plain);
            self.a.alu_ri(false, Alu::And, R::Rdx, 1);
            self.a.bind(plain);
        }
        let at = self.element(arr, idx, bt);
        self.a.store8(at, R::Rdx);
    }

    /// The class of array `a` when known here: allocated in this code, or a constant.
    fn known_array_class(&self, a: V) -> Option<&'static Klass> {
        let mut r = self.g.resolve(a);
        if let Inst::CheckCast(x, ..) = *self.g.inst(r) {
            r = self.g.resolve(x);
        }
        match *self.g.inst(r) {
            Inst::NewArray(k, ..) => Some(k),
            // A field declared as an array of a primitive type holds exactly that class (a
            // `byte[]` field never a `boolean[]`).
            _ if self.g.declared_types.get(&r).is_some_and(|k| matches!(k.kind, crate::klass::KlassKind::TypeArray { .. })) => self.g.declared_types.get(&r).copied(),
            _ => self.g.constant_value(r).filter(|&o| o != 0 && self.g.ty(r) == Ty::A).map(|o| crate::jit::fold::class_of(o as usize)),
        }
    }

    /// The memory at `addr`: an absolute operand in low memory, else through `scratch`.
    fn absolute(&mut self, addr: usize, scratch: R) -> M {
        if crate::low::is_low(addr) {
            return crate::jit::asm::abs32(addr);
        }
        self.a.mov_ri(scratch, addr as i64);
        mem(scratch, 0)
    }

    /// Class `k` as an immediate when it is in low memory (`crate::low`), else loaded into rdx.
    pub(super) fn class_operand(&mut self, k: &'static Klass) -> Option<i32> {
        let addr = k as *const Klass as usize;
        if crate::low::is_low(addr) {
            return Some(addr as i32);
        }
        self.a.mov_ri(R::Rdx, addr as i64);
        None
    }

    /// Compares the class word at `m` (else the class in rcx) with `k` (`class_operand`).
    pub(super) fn cmp_class(&mut self, m: Option<M>, k: Option<i32>) {
        match (m, k) {
            (None, Some(imm)) => self.a.alu_ri(true, Alu::Cmp, R::Rcx, imm),
            (None, None) => self.a.alu_rr(true, Alu::Cmp, R::Rcx, R::Rdx),
            (Some(m), Some(imm)) => self.a.alu_mi(true, Alu::Cmp, m, imm),
            (Some(m), None) => self.a.alu_rm(true, Alu::Cmp, R::Rdx, m),
        }
    }

    /// rax := 1 if `x`'s class is a subtype of `k` (jumping to `done` on the inline hits, and
    /// for null), else the runtime's answer. Without `answer`, the jumps to `done` leave rax
    /// as it is (a cast only tests rax on the way that falls through).
    /// `nonnull`: `x` is known not to be null here, so the null test is left out.
    /// `site`: the check's method and bytecode index, whose first-tier profile may name a class
    /// to compare first (`profile::cast_seen`).
    fn subtype_check(&mut self, x: V, k: &'static Klass, done: crate::jit::asm::Label, answer: bool, nonnull: bool, site: Option<(&'static Method, usize)>) {
        // The class seen last (in low memory, compared as an absolute operand).
        let cell: &'static std::sync::atomic::AtomicUsize = crate::low::leak(std::sync::atomic::AtomicUsize::new(0));
        let null_ok = self.a.new_label();
        let rx = self.reg(x, R::Rax);
        if !nonnull {
            self.a.test_rr(true, rx, rx);
            self.a.jcc(Cond::E, null_ok);
        }
        self.a.load(false, R::Rcx, mem(rx, 0));
        if answer {
            self.a.mov_ri(R::Rax, 1);
        }
        let kk = self.class_operand(k);
        // An object's class is never an interface; the class the site's profile saw pass is
        // compared before the class's own metadata is read.
        if !k.is_interface() {
            self.cmp_class(None, kk);
            self.a.jcc(Cond::E, done);
        } else if let Some(seen) = site.and_then(|(m, bci)| crate::jit::profile::cast_seen(m, bci)).filter(|p| p.is_subtype_of(k)) {
            self.a.alu_ri(true, Alu::Cmp, R::Rcx, seen as *const Klass as usize as i32);
            self.a.jcc(Cond::E, done);
        }
        let after = self.a.new_label();
        let leaf = matches!(k.kind, crate::klass::KlassKind::Instance) && k.access & crate::klass::ACC_FINAL != 0;
        if leaf {
            // A final class has no subclasses: only itself passed the test above.
            self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
            self.a.jmp(after);
        } else if let Some(d) = crate::jit::emit_obj::primary_depth(k) {
            // A class: its ancestor at k's depth is k, or the answer is no.
            let at = std::mem::offset_of!(Klass, primary) as i32 + 8 * d as i32;
            self.cmp_class(Some(mem(R::Rcx, at)), kk);
            self.a.jcc(Cond::E, done);
            self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
            self.a.jmp(after);
        } else {
            if k.is_interface() {
                // An interface: one of the class's interfaces, all of which (inherited ones
                // included) its flattened itable lists; the runtime only for a class whose
                // itable is not built yet.
                let (scan, no, slow) = (self.a.new_label(), self.a.new_label(), self.a.new_label());
                let hit = std::mem::offset_of!(Klass, super_hit) as i32;
                self.cmp_class(Some(mem(R::Rcx, hit)), kk);
                self.a.jcc(Cond::E, done);
                self.a.load(true, R::R11, mem(R::Rcx, std::mem::offset_of!(Klass, itable_raw) as i32));
                self.a.test_rr(true, R::R11, R::R11);
                self.a.jcc(Cond::E, slow);
                self.a.load(true, R::Rax, mem(R::R11, 0));
                self.a.bind(scan);
                self.a.alu_ri(true, Alu::Add, R::R11, 16);
                self.a.alu_ri(true, Alu::Sub, R::Rax, 1);
                self.a.jcc(Cond::L, no);
                self.cmp_class(Some(mem(R::R11, -8)), kk);
                self.a.jcc(Cond::Ne, scan);
                match kk {
                    Some(imm) => self.a.store_imm(mem(R::Rcx, hit), imm),
                    None => self.a.store(true, mem(R::Rcx, hit), R::Rdx),
                }
                if answer {
                    self.a.mov_ri(R::Rax, 1);
                }
                self.a.jmp(done);
                self.a.bind(no);
                self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
                self.a.jmp(after);
                self.a.bind(slow);
                if answer {
                    self.a.mov_ri(R::Rax, 1);
                }
            }
            let at = cell as *const _ as usize;
            if crate::low::is_low(at) {
                self.a.alu_rm(true, Alu::Cmp, R::Rcx, crate::jit::asm::abs32(at));
            } else {
                self.a.mov_ri(R::Rdx, at as i64);
                self.a.alu_rm(true, Alu::Cmp, R::Rcx, mem(R::Rdx, 0));
            }
            self.a.jcc(Cond::E, done);
            let (cell, slow) = (cell as *const _ as i64, self.a.new_label());
            let args = vec![Arg::Reg(R::Rcx), Arg::Imm(k as *const Klass as i64), Arg::Imm(cell)];
            self.slow_calls.push(SlowCall { entry: slow, back: after, addr: runtime::subtype_cached as *const () as usize, args, link: None, fail: None, preserve: self.preserve, result: R::Rax });
            self.a.jmp(slow);
        }
        // null passes a cast (instanceof tests null before calling this).
        self.a.bind(null_ok);
        if answer {
            self.a.mov_ri(R::Rax, 1);
        }
        self.a.jmp(done);
        self.a.bind(after);
    }

    /// Jumps to `yes` when the class in `sub` is a subtype of the class in `target` by the
    /// checks HotSpot makes inline for a target known only at run time: the ancestor at the
    /// target's depth (the primary supers display), then the interface `sub` matched last
    /// (`Klass::super_hit`); falls through otherwise, for the runtime to answer. r11 is
    /// scratch. (`sub == target` is the caller's.)
    pub(super) fn dynamic_subtype(&mut self, sub: R, target: R, yes: crate::jit::asm::Label) {
        let skip = self.a.new_label();
        self.a.load(false, R::R11, mem(target, std::mem::offset_of!(Klass, depth) as i32));
        self.a.alu_ri(false, Alu::Cmp, R::R11, crate::klass::PRIMARY as i32);
        self.a.jcc(Cond::Ae, skip);
        self.a.load(true, R::R11, mem_idx(sub, R::R11, 8, std::mem::offset_of!(Klass, primary) as i32));
        self.a.alu_rr(true, Alu::Cmp, R::R11, target);
        self.a.jcc(Cond::E, yes);
        self.a.bind(skip);
        self.a.alu_rm(true, Alu::Cmp, target, mem(sub, std::mem::offset_of!(Klass, super_hit) as i32));
        self.a.jcc(Cond::E, yes);
    }

    /// The thin-lock fast path inline; the runtime otherwise (blocking, inflated monitors, or
    /// an exit that throws).
    fn monitor(&mut self, enter: bool, o: V, s: &Rc<State>) {
        let fail = self.deopt(s);
        let (slow, done) = (self.a.new_label(), self.a.new_label());
        let ro = self.reg(o, R::Rcx);
        if enter {
            crate::jit::emit_obj::monitor_enter_fast(&mut self.a, ro, slow);
        } else {
            crate::jit::emit_obj::monitor_exit_fast(&mut self.a, ro, slow);
        }
        let (addr, fail) = if enter { (runtime::monitor_enter as *const () as usize, None) } else { (runtime::monitor_exit as *const () as usize, Some(fail)) };
        self.slow_calls.push(SlowCall { entry: slow, back: done, addr, args: vec![Arg::Thread, Arg::Val(o)], link: Some(s.clone()), fail, preserve: self.preserve, result: R::Rax });
        self.a.bind(done);
    }

    fn new_instance(&mut self, v: V, k: &'static Klass, s: &Rc<State>) {
        let fail = self.deopt(s);
        let (done, slow) = (self.a.new_label(), self.a.new_label());
        match crate::gc::bump_layout(k.instance_size() as usize) {
            Some((size, bump, end)) => {
                let at = |off: usize| mem(R::R12, (offset_of!(crate::thread::Thread, alloc) + off) as i32);
                // Through r11 (and rcx to clear a large object, `scratch.rs`).
                self.a.load(true, R::Rax, at(bump));
                self.a.lea(R::R11, mem(R::Rax, size as i32));
                self.a.alu_rm(true, Alu::Cmp, R::R11, at(end));
                self.a.jcc(Cond::A, slow);
                self.a.store(true, at(bump), R::R11);
                match self.inited.get(&v) {
                    Some(&covered) if k.instance_size() <= 128 => crate::jit::emit_obj::clear_uncovered(&mut self.a, R::Rax, 4, k.instance_size() as i32, R::R11, covered),
                    _ => crate::jit::emit_obj::clear(&mut self.a, R::Rax, 4, k.instance_size() as i32, R::R11, R::Rcx),
                }
                self.a.store_imm32(mem(R::Rax, 0), crate::heap::klass_word(k) as i32);
            }
            None => self.a.jmp(slow),
        }
        let args = vec![Arg::Thread, Arg::Imm(k as *const Klass as i64)];
        self.slow_calls.push(SlowCall { entry: slow, back: done, addr: runtime::new_instance as *const () as usize, args, link: Some(s.clone()), fail: Some(fail), preserve: self.preserve, result: R::Rax });
        self.a.bind(done);
        self.put(v, R::Rax);
    }

    /// An array of class `k` with `len` elements: bumped from the thread's cache of the size
    /// class when the size is known (a constant length) or small enough for the 16-byte classes
    /// (at most 256 bytes, whose class follows from the size), else allocated by the runtime.
    fn new_array(&mut self, v: V, k: &'static Klass, len: V, s: &Rc<State>) {
        let fail = self.deopt(s);
        let (done, slow) = (self.a.new_label(), self.a.new_label());
        let elem = k.elem_size() as usize;
        let uninit = self.g.uninit.contains(&v);
        let alloc = offset_of!(crate::thread::Thread, alloc);
        match self.g.constant(len).map(|n| n as i32) {
            Some(n) if n < 0 => self.a.jmp(fail),
            Some(n) => match crate::gc::bump_layout(crate::heap::array_bytes(elem as u32, n)) {
                Some((size, bump, end)) => {
                    let at = |off: usize| mem(R::R12, (alloc + off) as i32);
                    self.a.load(true, R::Rax, at(bump));
                    self.a.lea(R::Rdx, mem(R::Rax, size as i32));
                    self.a.alu_rm(true, Alu::Cmp, R::Rdx, at(end));
                    self.a.jcc(Cond::A, slow);
                    self.a.store(true, at(bump), R::Rdx);
                    self.a.store_imm32(mem(R::Rax, crate::heap::ARRAY_LENGTH_OFFSET as i32), n);
                    let to = if uninit { crate::heap::ARRAY_BASE_OFFSET as i32 } else { crate::heap::array_bytes(elem as u32, n).next_multiple_of(8) as i32 };
                    crate::jit::emit_obj::clear(&mut self.a, R::Rax, 8, to, R::Rdx, R::Rcx);
                    self.a.store_imm32(mem(R::Rax, 0), crate::heap::klass_word(k) as i32);
                    self.a.jmp(done);
                }
                None => self.a.jmp(slow),
            },
            None => {
                self.load(R::R11, len);
                self.a.mov_rr(false, R::R11, R::R11);
                self.a.test_rr(false, R::R11, R::R11);
                self.a.jcc(Cond::S, fail);
                // rcx := the cell size, a multiple of 8; its class's cache is at
                // alloc + classes + (size / 8 - 2) * 32 within the thread.
                let max = (crate::gc::SMALL_STEP_MAX - crate::heap::ARRAY_BASE_OFFSET as usize) / elem;
                self.a.alu_ri(false, Alu::Cmp, R::R11, max as i32);
                self.a.jcc(Cond::A, slow);
                self.a.mov_rr(false, R::Rcx, R::R11);
                if elem > 1 {
                    self.a.shift_ri(false, Shift::Shl, R::Rcx, elem.trailing_zeros() as u8);
                }
                self.a.alu_ri(false, Alu::Add, R::Rcx, crate::heap::ARRAY_BASE_OFFSET as i32 + 7);
                self.a.alu_ri(false, Alu::And, R::Rcx, -8);
                let (bump, end) = crate::gc::step_cache_offsets();
                let at = |off: usize| mem_idx(R::R12, R::Rcx, 4, (alloc + off) as i32 - 64);
                self.a.load(true, R::Rax, at(bump));
                self.a.lea(R::Rdx, mem_idx(R::Rax, R::Rcx, 1, 0));
                self.a.alu_rm(true, Alu::Cmp, R::Rdx, at(end));
                self.a.jcc(Cond::A, slow);
                self.a.store(true, at(bump), R::Rdx);
                self.a.store(false, mem(R::Rax, crate::heap::ARRAY_LENGTH_OFFSET as i32), R::R11);
                if uninit {
                    // Only the mark word (`ARRAY_BASE_OFFSET` is 16).
                    crate::jit::emit_obj::prefetch_ahead(&mut self.a, mem(R::Rdx, 0));
                    self.a.store_imm(mem(R::Rax, 8), 0);
                } else {
                    // The whole cell from the mark word on (rdx is its end).
                    self.a.lea(R::Rcx, mem(R::Rax, 8));
                    crate::jit::emit_obj::clear_to(&mut self.a, R::Rcx, R::Rdx, R::R11);
                }
                self.a.store_imm32(mem(R::Rax, 0), crate::heap::klass_word(k) as i32);
                self.a.jmp(done);
            }
        }
        let args = vec![Arg::Thread, Arg::Imm(k as *const Klass as i64), Arg::Val(len)];
        self.slow_calls.push(SlowCall { entry: slow, back: done, addr: runtime::new_array as *const () as usize, args, link: Some(s.clone()), fail: Some(fail), preserve: self.preserve, result: R::Rax });
        self.a.bind(done);
        self.put(v, R::Rax);
    }

    /// Calls the method in rdx (found for the receiver) in the register convention, with the
    /// arguments in place: through its `Method::reg_call`, made by a stub the first time a site
    /// finds it missing.
    fn reg_call_found(&mut self, base: i32) {
        self.a.load(true, R::R11, mem(R::Rdx, offset_of!(Method, reg_call) as i32));
        self.a.test_rr(true, R::R11, R::R11);
        let ready = self.a.new_label();
        self.a.jcc(Cond::Ne, ready);
        self.a.call_abs(crate::jit::code_cache::reg_call_stub());
        self.a.mov_rr(true, R::R11, R::Rax);
        self.a.bind(ready);
        self.a.lea(R::Rsi, self.slot(base));
        self.a.call_r(R::R11);
    }

    /// Saves the argument registers of the register convention (`jit::abi`) around a runtime
    /// call made while they are loaded (16 bytes of stack per pair, keeping the alignment).
    fn save_arg_regs(&mut self) {
        use crate::jit::abi::{ARG_GPRS, ARG_XMMS};
        for r in ARG_GPRS {
            self.a.push(r);
        }
        self.a.alu_ri(true, Alu::Sub, R::Rsp, 8 * ARG_XMMS.len() as i32);
        for (i, &x) in ARG_XMMS.iter().enumerate() {
            self.a.fstore(true, mem(R::Rsp, 8 * i as i32), X(x));
        }
    }

    /// Restores what `save_arg_regs` saved.
    fn restore_arg_regs(&mut self) {
        use crate::jit::abi::{ARG_GPRS, ARG_XMMS};
        for (i, &x) in ARG_XMMS.iter().enumerate() {
            self.a.fload(true, X(x), mem(R::Rsp, 8 * i as i32));
        }
        self.a.alu_ri(true, Alu::Add, R::Rsp, 8 * ARG_XMMS.len() as i32);
        for r in ARG_GPRS.iter().rev() {
            self.a.pop(*r);
        }
    }


    /// A Java call: arguments into the frame's operand stack where the callee's locals start,
    /// the target into rdx, then `Method::entry`. Only values in callee-saved registers and
    /// spill slots survive it (`regalloc.rs`).
    fn call(&mut self, v: V, callee: Callee, args: &[V], s: &Rc<State>) {
        let deopt = self.deopt(s);
        let base = Self::stack_base_of(s) + s.stack.len() as i32;
        let (unprofiled, cold) = (self.g.unprofiled.contains(&v), self.g.cold_calls.contains(&v));
        // A direct call to a method reachable through its register trampoline passes the first
        // arguments in registers (`jit::abi`); sites that profile themselves keep the frame
        // protocol (their runtime calls could clobber the argument registers).
        // A vtable or interface call does too, through the `Method::reg_call` of the method it
        // finds (every implementation has the signature, so the same argument registers).
        let reg_call = match callee {
            Callee::Direct(m) if !unprofiled && !cold => crate::jit::code_cache::reg_entry(m).map(|t| (Some(t), crate::jit::abi::arg_regs(m))),
            Callee::Virtual(m) | Callee::Interface(m) if !unprofiled && !cold && crate::jit::code_cache::near() => Some((None, crate::jit::abi::arg_regs(m))),
            _ => None,
        };
        let in_reg = |slot: i32| reg_call.as_ref().and_then(|(_, regs)| regs.iter().find(|r| r.0 as i32 == slot - base).map(|r| r.1));
        let mut slot = base;
        let mut moves: Vec<(Loc, Loc)> = Vec::new();
        // Int-like arguments leave in slot encoding (sign-extended), as `store_frame` writes
        // them: 32-bit arithmetic leaves the upper half zero, and a baseline callee (through
        // its adapter) reads the whole slot.
        let mut ints: Vec<R> = Vec::new();
        for &x in args {
            match in_reg(slot) {
                Some(crate::jit::abi::ArgReg::Gpr(r)) => {
                    if self.g.ty(x) == Ty::I {
                        ints.push(r);
                    }
                    moves.push((Loc::Reg(r), self.loc[x as usize]))
                }
                Some(crate::jit::abi::ArgReg::Xmm(r)) => moves.push((Loc::Xmm(r), self.loc[x as usize])),
                None => self.store_frame(self.slot(slot), x),
            }
            slot += if self.g.ty(x).wide() { 2 } else { 1 };
        }
        // From here until the restore, values live across the call are only in their save slots.
        self.save_around(v);
        if let Some((trampoline, _)) = reg_call {
            // The argument registers are filled after the saves (they may hold saved values) and
            // before the link, which clobbers only rax and r11.
            super::moves::parallel(&mut self.a, &moves);
            for &r in &ints {
                self.a.movsxd(r, r);
            }
            self.link(s, false);
            match (callee, trampoline) {
                (Callee::Direct(m), Some(t)) => {
                    self.a.mov_ri(R::Rdx, m as *const Method as i64);
                    self.a.lea(R::Rsi, self.slot(base));
                    self.a.call_abs(t);
                }
                (Callee::Virtual(m), _) => {
                    let receiver = crate::jit::abi::ARG_GPRS[0];
                    self.a.load(false, R::Rax, mem(receiver, 0));
                    self.a.load(true, R::Rdx, mem(R::Rax, crate::klass::VTABLE_OFFSET as i32 + 8 * m.vtable_index()));
                    self.reg_call_found(base);
                }
                (Callee::Interface(m), _) => {
                    let fail = self.restoring(v, deopt);
                    self.interface_target(m, s, args[0], fail, true);
                    self.reg_call_found(base);
                }
                _ => unreachable!("register calls are direct or through the vtable"),
            }
            self.restore_around(v);
            self.a.alu_mi(true, Alu::Cmp, mem(R::R12, offsets::PENDING), 0);
            self.a.jcc(Cond::Ne, deopt);
            if matches!(self.g.ty(v), Ty::F | Ty::D) {
                self.xput(v, crate::jit::asm::X(0));
            } else {
                self.put(v, R::Rax);
            }
            return;
        }
        let fail = self.restoring(v, deopt);
        self.link(s, false);
        if unprofiled || cold {
            self.profile_call(s, unprofiled.then(|| args[0]), cold);
        }
        match callee {
            Callee::Direct(m) => self.a.mov_ri(R::Rdx, m as *const Method as i64),
            Callee::Virtual(m) => {
                self.load(R::Rcx, args[0]);
                self.a.load(false, R::Rax, mem(R::Rcx, 0));
                self.a.load(true, R::Rdx, mem(R::Rax, crate::klass::VTABLE_OFFSET as i32 + 8 * m.vtable_index()));
            }
            Callee::Interface(m) => self.interface_target(m, s, args[0], fail, false),
            Callee::Method(t) => self.load(R::Rdx, t),
            Callee::Select(t) => self.select_target(t, base, fail),
        }
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.lea(R::Rsi, self.slot(base));
        // A known target is called directly through its trampoline (`code_cache::direct_entry`).
        match match callee {
            Callee::Direct(m) => crate::jit::code_cache::direct_entry(m),
            _ => None,
        } {
            Some(t) => self.a.call_abs(t),
            None => self.a.call_m(mem(R::Rdx, offset_of!(Method, entry) as i32)),
        }
        self.restore_around(v);
        self.a.alu_mi(true, Alu::Cmp, mem(R::R12, offsets::PENDING), 0);
        self.a.jcc(Cond::Ne, deopt);
        if matches!(self.g.ty(v), Ty::F | Ty::D) {
            self.xput(v, crate::jit::asm::X(0));
        } else {
            self.put(v, R::Rax);
        }
    }

    /// A call compiled without a receiver profile (`receiver`), or not inlined because its site
    /// looked cold (`cold`): records the receiver's class in the site's profile (as first-tier
    /// code does) and counts down; at zero a cold site is marked hot and the method is
    /// recompiled, so a path that was cold when it was compiled gets its calls inlined once it
    /// runs.
    fn profile_call(&mut self, s: &State, receiver: Option<V>, cold: bool) {
        let cell = crate::jit::profile::cell(s.method, s.bci as usize) as *const IcCell as i64;
        let calls = if receiver.is_some() { runtime::REPROFILE_CALLS } else { runtime::HOT_COLD_CALLS };
        let left: &'static std::sync::atomic::AtomicU32 = crate::low::leak(std::sync::atomic::AtomicU32::new(calls));
        let (done, out) = (self.a.new_label(), self.a.new_label());
        let at = self.absolute(left as *const _ as usize, R::Rax);
        self.a.alu_mi(false, Alu::Cmp, at, 0);
        self.a.jcc(Cond::E, done);
        self.a.alu_mi(false, Alu::Sub, at, 1);
        match receiver {
            Some(receiver) => {
                self.load(R::Rcx, receiver);
                self.a.load(false, R::Rcx, mem(R::Rcx, 0));
                self.call_preserving(runtime::profile_call as *const () as usize, &[Arg::Reg(R::Rcx), Arg::Imm(cell), Arg::Imm(left as *const _ as i64)]);
                self.a.test_rr(false, R::Rax, R::Rax);
                self.a.jcc(Cond::E, done);
            }
            None => self.a.jcc(Cond::Ne, done),
        }
        self.a.bind(out);
        let site = if cold { Arg::Imm(s.method as *const Method as i64) } else { Arg::Imm(0) };
        self.call_preserving(runtime::request_reprofile as *const () as usize, &[Arg::Imm(self.root as *const Method as i64), site, Arg::Imm(s.bci as i64)]);
        self.a.bind(done);
    }

    /// rdx := the implementation of the `Method*` in `t` for the receiver at frame slot `base`:
    /// the receiver class's vtable entry for a method with a vtable slot, the entry of the
    /// class's last itable hit for an interface's method of that interface (as interface calls
    /// check it, `interface_target`), else the runtime's answer.
    fn select_target(&mut self, t: V, base: i32, fail: crate::jit::asm::Label) {
        let (found, slow, iface, have) = (self.a.new_label(), self.a.new_label(), self.a.new_label(), self.a.new_label());
        self.load(R::R11, t);
        self.a.load(true, R::Rcx, self.slot(base));
        self.a.load(false, R::Rdi, mem(R::Rcx, 0));
        // `Method::vtable_index`: a vtable slot, -1, or -2 - an interface method's itable slot.
        self.a.load_sx(4, R::Rax, mem(R::R11, offset_of!(Method, vtable_index) as i32));
        self.a.test_rr(false, R::Rax, R::Rax);
        self.a.jcc(Cond::S, iface);
        self.a.load(true, R::Rdx, crate::jit::asm::mem_idx(R::Rdi, R::Rax, 8, crate::klass::VTABLE_OFFSET as i32));
        self.a.jmp(have);
        self.a.bind(iface);
        self.a.alu_ri(false, Alu::Cmp, R::Rax, -1);
        self.a.jcc(Cond::E, slow);
        self.a.load(true, R::Rsi, mem(R::Rdi, offset_of!(Klass, itable_hit) as i32));
        self.a.load(true, R::Rdx, mem(R::R11, offset_of!(Method, holder) as i32));
        self.a.alu_rm(true, Alu::Cmp, R::Rdx, mem(R::Rsi, -8));
        self.a.jcc(Cond::Ne, slow);
        self.a.neg(true, R::Rax);
        self.a.load(true, R::Rdx, crate::jit::asm::mem_idx(R::Rsi, R::Rax, 8, -16));
        self.a.bind(have);
        self.a.test_rr(true, R::Rdx, R::Rdx);
        self.a.jcc(Cond::Ne, found);
        self.a.bind(slow);
        self.a.mov_rr(true, R::Rsi, R::R11);
        // The method stays on the stack across the call for the error path.
        self.a.push(R::R11);
        self.a.push(R::R11);
        self.a.call_abs(runtime::select_linked as *const () as usize);
        self.a.pop(R::Rsi);
        self.a.pop(R::Rsi);
        let done_rax = self.a.new_label();
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::Ne, done_rax);
        self.publish_sp();
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.call_abs(runtime::no_target as *const () as usize);
        self.a.jmp(fail);
        self.a.bind(done_rax);
        self.a.mov_rr(true, R::Rdx, R::Rax);
        self.a.bind(found);
    }

    /// rdx := the implementation of interface method `m` for `receiver`:
    /// an inline cache, the itable stub once the site is megamorphic, else the runtime.
    /// Its inline cache is the profile's cell for the call at `s`.
    fn interface_target(&mut self, m: &'static Method, s: &State, receiver: V, fail: crate::jit::asm::Label, reg: bool) {
        let cell = crate::jit::profile::cell(s.method, s.bci as usize) as *const IcCell as i64;
        let (hit, done, found, missing) = (self.a.new_label(), self.a.new_label(), self.a.new_label(), self.a.new_label());
        // The receiver's class in `k`. For a call in the register convention the receiver is
        // already in its argument register, and the lookup keeps every argument register.
        let k = if reg { R::Rcx } else { R::Rdi };
        if reg {
            self.a.load(false, k, mem(crate::jit::abi::ARG_GPRS[0], 0));
        } else {
            // The receiver from its register, not the frame slot just written (store forwarding).
            self.load(R::Rcx, receiver);
            self.a.load(false, k, mem(R::Rcx, 0));
        }
        // A site known megamorphic goes straight to the itable scan.
        let megamorphic = m.holder.is_interface() && crate::jit::profile::megamorphic(s.method, s.bci as usize);
        // The cell is in low memory, addressed absolutely (else through rax); rdx holds its
        // cached pair until the hit loads the target.
        let field = |this: &mut Self, off: i32| this.absolute((cell + off as i64) as usize, R::Rax);
        if !megamorphic {
            let m_pair = field(self, IcCell::pair_offset());
            self.a.load(true, R::Rdx, m_pair);
            self.a.alu_rm(true, Alu::Cmp, k, mem(R::Rdx, IcCell::pair_klass_offset()));
            self.a.jcc(Cond::E, hit);
        }
        if m.holder.is_interface() {
            let learn = self.a.new_label();
            if !megamorphic {
                let m_misses = field(self, IcCell::misses_offset());
                self.a.alu_mi(false, Alu::Cmp, m_misses, runtime::IC_MAX_UPDATES as i32);
                self.a.jcc(Cond::B, learn);
            }
            // Megamorphic: the class's last itable hit (a block, its interface in the word
            // before it: the method loads as the interface compares), else a scan of its itable
            // in place (`stubs::itable_lookup`).
            let (scan, block) = (self.a.new_label(), self.a.new_label());
            let hit = offset_of!(Klass, itable_hit) as i32;
            self.a.mov_ri(R::Rsi, m.holder as *const Klass as i64);
            self.a.load(true, R::Rax, mem(k, hit));
            self.a.alu_rm(true, Alu::Cmp, R::Rsi, mem(R::Rax, -8));
            self.a.jcc(Cond::E, block);
            self.a.load(true, R::Rax, mem(k, offset_of!(Klass, itable_raw) as i32));
            self.a.test_rr(true, R::Rax, R::Rax);
            self.a.jcc(Cond::E, learn);
            self.a.load(true, R::R11, mem(R::Rax, 0));
            self.a.bind(scan);
            self.a.alu_ri(true, Alu::Add, R::Rax, 16);
            self.a.alu_ri(true, Alu::Sub, R::R11, 1);
            self.a.jcc(Cond::L, learn);
            self.a.alu_rm(true, Alu::Cmp, R::Rsi, mem(R::Rax, -8));
            self.a.jcc(Cond::Ne, scan);
            self.a.load(true, R::Rax, mem(R::Rax, 0));
            self.a.store(true, mem(k, hit), R::Rax);
            self.a.bind(block);
            self.a.load(true, R::Rax, mem(R::Rax, 8 * m.index() as i32));
            self.a.test_rr(true, R::Rax, R::Rax);
            self.a.jcc(Cond::Ne, found);
            self.a.bind(learn);
        }
        if reg {
            self.save_arg_regs();
            self.a.mov_rr(true, R::Rdi, k);
        }
        self.a.mov_ri(R::Rsi, m as *const Method as i64);
        self.a.mov_ri(R::Rdx, cell);
        self.a.call_abs(runtime::ic_miss as *const () as usize);
        if reg {
            self.restore_arg_regs();
        }
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::E, missing);
        self.a.bind(found);
        self.a.mov_rr(true, R::Rdx, R::Rax);
        self.a.jmp(done);
        self.a.bind(missing);
        self.publish_sp();
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.mov_ri(R::Rsi, m as *const Method as i64);
        self.a.call_abs(runtime::no_target as *const () as usize);
        self.a.jmp(fail);
        self.a.bind(hit);
        self.a.load(true, R::Rdx, mem(R::Rdx, IcCell::pair_target_offset()));
        self.a.bind(done);
    }
}
