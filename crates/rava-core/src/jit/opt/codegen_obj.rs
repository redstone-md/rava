//! Code generation of field, array, allocation, type-check and call instructions.

use super::codegen::{Arg, Gen};
use super::ir::*;
use super::regalloc::Loc;
use crate::jit::asm::{mem, mem_idx, Alu, Cond, Shift, M, R, X};
use crate::jit::emit_obj::{load_typed_to, normalize_into, store_typed_from};
use crate::jit::runtime::{self, IcCell};
use crate::klass::Klass;
use crate::method::Method;
use crate::thread::offsets;
use crate::types::BasicType;
use std::mem::offset_of;
use std::rc::Rc;

/// Offset of array element 0.
const BASE: i32 = 24;

fn scale(bt: BasicType) -> u8 {
    match bt {
        BasicType::Boolean | BasicType::Byte => 1,
        BasicType::Char | BasicType::Short => 2,
        BasicType::Int | BasicType::Float => 4,
        _ => 8,
    }
}

impl Gen<'_> {
    pub(super) fn memory_inst(&mut self, v: V) {
        match self.g.inst(v).clone() {
            Inst::LoadField(o, off, bt @ (BasicType::Float | BasicType::Double)) => {
                let ro = self.reg(o, R::Rax);
                let r = self.xout(v);
                self.a.fload(bt == BasicType::Double, r, mem(ro, off as i32));
                self.xput(v, r);
            }
            Inst::StoreField(o, off, bt @ (BasicType::Float | BasicType::Double), x) => {
                let a = self.xreg(x, X(0));
                let ro = self.reg(o, R::Rax);
                self.a.fstore(bt == BasicType::Double, mem(ro, off as i32), a);
            }
            Inst::LoadIndexed(arr, idx, bt @ (BasicType::Float | BasicType::Double)) => {
                let at = self.element(arr, idx, bt);
                let r = self.xout(v);
                self.a.fload(bt == BasicType::Double, r, at);
                self.xput(v, r);
            }
            Inst::StoreIndexed(arr, idx, bt @ (BasicType::Float | BasicType::Double), x) => {
                let at = self.element(arr, idx, bt);
                let a = self.xreg(x, X(0));
                self.a.fstore(bt == BasicType::Double, at, a);
            }
            Inst::LoadField(o, off, bt) => {
                let ro = self.reg(o, R::Rax);
                let r = self.out(v);
                load_typed_to(&mut self.a, r, bt, mem(ro, off as i32));
                self.put(v, r);
            }
            Inst::StoreField(o, off, bt, x) => {
                let ro = self.reg(o, R::Rax);
                let rx = self.value_reg(x, bt);
                self.store_typed(v, rx, bt, mem(ro, off as i32));
            }
            Inst::LoadStatic(addr, bt) => {
                self.a.mov_ri(R::Rcx, addr as i64);
                let r = self.out(v);
                load_typed_to(&mut self.a, r, bt, mem(R::Rcx, 0));
                self.put(v, r);
            }
            Inst::StoreStatic(addr, bt, x) => {
                self.load(R::Rdx, x);
                normalize_into(&mut self.a, bt);
                self.a.mov_ri(R::Rcx, addr as i64);
                self.a.store(true, mem(R::Rcx, 0), R::Rdx);
            }
            Inst::ArrayLength(x) => {
                let ra = self.reg(x, R::Rax);
                let r = self.out(v);
                self.a.load_sx(4, r, mem(ra, 16));
                self.put(v, r);
            }
            Inst::LoadIndexed(arr, idx, bt) => {
                let at = self.element(arr, idx, bt);
                let r = self.out(v);
                load_typed_to(&mut self.a, r, bt, at);
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
                self.subtype_check(x, k, done);
                self.a.test_rr(true, R::Rax, R::Rax);
                self.a.jcc(Cond::E, fail);
                self.a.bind(done);
                let (dst, src) = (self.loc[v as usize], self.loc[x as usize]);
                if dst != Loc::None {
                    super::moves::mov(&mut self.a, dst, src);
                }
            }
            Inst::InstanceOf(x, k) => {
                let (done, null) = (self.a.new_label(), self.a.new_label());
                let rx = self.reg(x, R::Rax);
                self.a.test_rr(true, rx, rx);
                self.a.jcc(Cond::E, null);
                self.subtype_check(x, k, done);
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
                self.a.load(true, R::Rcx, mem(rx, 0));
                let ra = self.reg(arr, R::Rax);
                self.a.load(true, R::Rdx, mem(ra, 0));
                self.a.load(true, R::Rdx, mem(R::Rdx, offset_of!(Klass, elem_klass) as i32));
                self.a.alu_rr(true, Alu::Cmp, R::Rcx, R::Rdx);
                self.a.jcc(Cond::E, ok);
                self.call_preserving(runtime::subtype as *const () as usize, &[Arg::Reg(R::Rcx), Arg::Reg(R::Rdx)]);
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

    /// A register with `x` for a store of type `bt` (a copy when the store masks it).
    fn value_reg(&mut self, x: V, bt: BasicType) -> R {
        if bt == BasicType::Boolean {
            self.load(R::Rdx, x);
            return R::Rdx;
        }
        self.reg(x, R::Rdx)
    }

    /// The address of element `idx` of `arr` (a constant index folded into the displacement).
    fn element(&mut self, arr: V, idx: V, bt: BasicType) -> M {
        let ra = self.reg(arr, R::Rax);
        if let Loc::Const(c) = self.loc[idx as usize] {
            if let Some(d) = c.checked_mul(scale(bt) as i64).and_then(|d| i32::try_from(d + BASE as i64).ok()) {
                return mem(ra, d);
            }
        }
        let ri = self.reg(idx, R::Rcx);
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
        let at = self.element(arr, idx, bt);
        if bt != BasicType::Byte {
            let rx = self.value_reg(x, bt);
            return self.store_typed(v, rx, bt, at);
        }
        // bastore: boolean[] stores value & 1, byte[] the low byte.
        let ra = self.reg(arr, R::Rax);
        self.load(R::Rdx, x);
        let bools = crate::vm::vm().primitive_array_class(BasicType::Boolean).ok().map_or(0, |k| k as *const Klass as i64);
        let plain = self.a.new_label();
        self.a.mov_ri(R::R11, bools);
        self.a.alu_rm(true, Alu::Cmp, R::R11, mem(ra, 0));
        self.a.jcc(Cond::Ne, plain);
        self.a.alu_ri(false, Alu::And, R::Rdx, 1);
        self.a.bind(plain);
        self.a.store8(at, R::Rdx);
    }

    /// With `x` non-null: rax := 1 if its class is a subtype of `k` (jumping to `done` on the
    /// inline hits), else the runtime's answer.
    fn subtype_check(&mut self, x: V, k: &'static Klass, done: crate::jit::asm::Label) {
        let cell: &'static std::sync::atomic::AtomicUsize = Box::leak(Box::new(std::sync::atomic::AtomicUsize::new(0)));
        let null_ok = self.a.new_label();
        let rx = self.reg(x, R::Rax);
        self.a.test_rr(true, rx, rx);
        self.a.jcc(Cond::E, null_ok);
        self.a.load(true, R::Rcx, mem(rx, 0));
        self.a.mov_ri(R::Rax, 1);
        self.a.mov_ri(R::Rdx, k as *const Klass as i64);
        self.a.alu_rr(true, Alu::Cmp, R::Rcx, R::Rdx);
        self.a.jcc(Cond::E, done);
        let after = self.a.new_label();
        if let Some(d) = crate::jit::emit_obj::primary_depth(k) {
            // A class: its ancestor at k's depth is k, or the answer is no.
            let at = std::mem::offset_of!(Klass, primary) as i32 + 8 * d as i32;
            self.a.alu_rm(true, Alu::Cmp, R::Rdx, mem(R::Rcx, at));
            self.a.jcc(Cond::E, done);
            self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
            self.a.jmp(after);
        } else {
            self.a.mov_ri(R::Rdx, cell as *const _ as i64);
            self.a.alu_rm(true, Alu::Cmp, R::Rcx, mem(R::Rdx, 0));
            self.a.jcc(Cond::E, done);
            let cell = cell as *const _ as i64;
            self.call_preserving(runtime::subtype_cached as *const () as usize, &[Arg::Reg(R::Rcx), Arg::Imm(k as *const Klass as i64), Arg::Imm(cell)]);
            self.a.jmp(after);
        }
        // null passes a cast (instanceof tests null before calling this).
        self.a.bind(null_ok);
        self.a.mov_ri(R::Rax, 1);
        self.a.jmp(done);
        self.a.bind(after);
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
        self.a.jmp(done);
        self.a.bind(slow);
        self.link(s, true);
        if enter {
            self.call_preserving(runtime::monitor_enter as *const () as usize, &[Arg::Thread, Arg::Val(o)]);
        } else {
            self.call_preserving(runtime::monitor_exit as *const () as usize, &[Arg::Thread, Arg::Val(o)]);
            self.a.test_rr(true, R::Rax, R::Rax);
            self.a.jcc(Cond::E, fail);
        }
        self.a.bind(done);
    }

    fn new_instance(&mut self, v: V, k: &'static Klass, s: &Rc<State>) {
        let fail = self.deopt(s);
        let (done, slow) = (self.a.new_label(), self.a.new_label());
        match crate::gc::bump_layout(k.instance_size() as usize) {
            Some((size, bump, end)) => {
                let at = |off: usize| mem(R::R12, (offset_of!(crate::thread::Thread, alloc) + off) as i32);
                self.a.load(true, R::Rax, at(bump));
                self.a.lea(R::Rdx, mem(R::Rax, size as i32));
                self.a.alu_rm(true, Alu::Cmp, R::Rdx, at(end));
                self.a.jcc(Cond::A, slow);
                self.a.store(true, at(bump), R::Rdx);
                self.a.mov_ri(R::Rdx, k as *const Klass as i64);
                self.a.store(true, mem(R::Rax, 0), R::Rdx);
                self.a.jmp(done);
            }
            None => self.a.jmp(slow),
        }
        self.a.bind(slow);
        self.link(s, true);
        self.call_preserving(runtime::new_instance as *const () as usize, &[Arg::Thread, Arg::Imm(k as *const Klass as i64)]);
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::E, fail);
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
                    self.a.mov_ri(R::Rdx, k as *const Klass as i64);
                    self.a.store(true, mem(R::Rax, 0), R::Rdx);
                    self.a.jmp(done);
                }
                None => self.a.jmp(slow),
            },
            None => {
                self.load(R::R11, len);
                self.a.mov_rr(false, R::R11, R::R11);
                self.a.test_rr(false, R::R11, R::R11);
                self.a.jcc(Cond::S, fail);
                // rcx := the cell size, a multiple of 16; its class's cache is at
                // alloc + classes + (size / 16 - 1) * 32 within the thread.
                let max = (crate::gc::SMALL_STEP_MAX - crate::heap::ARRAY_BASE_OFFSET as usize) / elem;
                self.a.alu_ri(false, Alu::Cmp, R::R11, max as i32);
                self.a.jcc(Cond::A, slow);
                self.a.mov_rr(false, R::Rcx, R::R11);
                self.a.shift_ri(false, Shift::Shl, R::Rcx, elem.trailing_zeros() as u8);
                self.a.alu_ri(false, Alu::Add, R::Rcx, crate::heap::ARRAY_BASE_OFFSET as i32 + 15);
                self.a.alu_ri(false, Alu::And, R::Rcx, -16);
                let (bump, end) = crate::gc::step_cache_offsets();
                let at = |off: usize| mem_idx(R::R12, R::Rcx, 2, (alloc + off) as i32 - 32);
                self.a.load(true, R::Rax, at(bump));
                self.a.lea(R::Rdx, mem_idx(R::Rax, R::Rcx, 1, 0));
                self.a.alu_rm(true, Alu::Cmp, R::Rdx, at(end));
                self.a.jcc(Cond::A, slow);
                self.a.store(true, at(bump), R::Rdx);
                self.a.store(false, mem(R::Rax, crate::heap::ARRAY_LENGTH_OFFSET as i32), R::R11);
                self.a.mov_ri(R::Rdx, k as *const Klass as i64);
                self.a.store(true, mem(R::Rax, 0), R::Rdx);
                self.a.jmp(done);
            }
        }
        self.a.bind(slow);
        self.link(s, true);
        self.call_preserving(runtime::new_array as *const () as usize, &[Arg::Thread, Arg::Imm(k as *const Klass as i64), Arg::Val(len)]);
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::E, fail);
        self.a.bind(done);
        self.put(v, R::Rax);
    }

    /// A Java call: arguments into the frame's operand stack where the callee's locals start,
    /// the target into rdx, then `Method::entry`. Only values in callee-saved registers and
    /// spill slots survive it (`regalloc.rs`).
    fn call(&mut self, v: V, callee: Callee, args: &[V], s: &Rc<State>) {
        let deopt = self.deopt(s);
        let base = Self::stack_base_of(s) + s.stack.len() as i32;
        let mut slot = base;
        for &x in args {
            self.store_frame(self.slot(slot), x);
            slot += if self.g.ty(x).wide() { 2 } else { 1 };
        }
        // From here until the restore, values live across the call are only in their save slots.
        self.save_around(v);
        let fail = self.restoring(v, deopt);
        self.link(s, false);
        match callee {
            Callee::Direct(m) => self.a.mov_ri(R::Rdx, m as *const Method as i64),
            Callee::Virtual(m) => {
                self.load(R::Rcx, args[0]);
                self.a.load(true, R::Rax, mem(R::Rcx, 0));
                self.a.load(true, R::Rax, mem(R::Rax, offset_of!(Klass, vtable_ptr) as i32));
                self.a.load(true, R::Rdx, mem(R::Rax, 8 * m.vtable_index()));
            }
            Callee::Interface(m) => self.interface_target(m, s, args[0], fail),
            Callee::Method(t) => self.load(R::Rdx, t),
            Callee::Select(t) => self.select_target(t, base, fail),
        }
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.lea(R::Rsi, self.slot(base));
        self.a.call_m(mem(R::Rdx, offset_of!(Method, entry) as i32));
        self.restore_around(v);
        self.a.alu_mi(true, Alu::Cmp, mem(R::R12, offsets::PENDING), 0);
        self.a.jcc(Cond::Ne, deopt);
        self.put(v, R::Rax);
    }

    /// rdx := the implementation of the `Method*` in `t` for the receiver at frame slot `base`.
    fn select_target(&mut self, t: V, base: i32, fail: crate::jit::asm::Label) {
        let found = self.a.new_label();
        self.load(R::R11, t);
        self.a.load(true, R::Rcx, self.slot(base));
        self.a.load(true, R::Rdi, mem(R::Rcx, 0));
        self.a.mov_rr(true, R::Rsi, R::R11);
        // The method stays on the stack across the call for the error path.
        self.a.push(R::R11);
        self.a.push(R::R11);
        self.a.call_abs(runtime::select_linked as *const () as usize);
        self.a.pop(R::Rsi);
        self.a.pop(R::Rsi);
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::Ne, found);
        self.publish_sp();
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.call_abs(runtime::no_target as *const () as usize);
        self.a.jmp(fail);
        self.a.bind(found);
        self.a.mov_rr(true, R::Rdx, R::Rax);
    }

    /// rdx := the implementation of interface method `m` for `receiver`:
    /// an inline cache, the itable stub once the site is megamorphic, else the runtime.
    /// Its inline cache is the profile's cell for the call at `s`.
    fn interface_target(&mut self, m: &'static Method, s: &State, receiver: V, fail: crate::jit::asm::Label) {
        let cell = crate::jit::profile::cell(s.method, s.bci as usize) as *const IcCell as i64;
        let (hit, done, found, missing) = (self.a.new_label(), self.a.new_label(), self.a.new_label(), self.a.new_label());
        // The receiver from its register, not the frame slot just written (store forwarding).
        self.load(R::Rcx, receiver);
        self.a.load(true, R::Rdi, mem(R::Rcx, 0));
        // A site known megamorphic goes straight to the itable scan.
        let megamorphic = m.holder.is_interface() && crate::jit::profile::megamorphic(s.method, s.bci as usize);
        if !megamorphic {
            self.a.mov_ri(R::Rax, cell);
            self.a.load(true, R::Rax, mem(R::Rax, 0));
            self.a.alu_rm(true, Alu::Cmp, R::Rdi, mem(R::Rax, 0));
            self.a.jcc(Cond::E, hit);
        }
        if m.holder.is_interface() {
            let learn = self.a.new_label();
            if !megamorphic {
                self.a.mov_ri(R::Rax, cell);
                self.a.alu_mi(false, Alu::Cmp, mem(R::Rax, IcCell::misses_offset()), runtime::IC_MAX_UPDATES as i32);
                self.a.jcc(Cond::B, learn);
            }
            // Megamorphic: scan the receiver class's itable in place (`stubs::itable_lookup`).
            let scan = self.a.new_label();
            self.a.load(true, R::Rax, mem(R::Rdi, offset_of!(Klass, itable_raw) as i32));
            self.a.test_rr(true, R::Rax, R::Rax);
            self.a.jcc(Cond::E, learn);
            self.a.load(true, R::R11, mem(R::Rax, 0));
            self.a.mov_ri(R::Rsi, m.holder as *const Klass as i64);
            self.a.bind(scan);
            self.a.alu_ri(true, Alu::Add, R::Rax, 16);
            self.a.alu_ri(true, Alu::Sub, R::R11, 1);
            self.a.jcc(Cond::L, learn);
            self.a.alu_rm(true, Alu::Cmp, R::Rsi, mem(R::Rax, -8));
            self.a.jcc(Cond::Ne, scan);
            self.a.load(true, R::Rax, mem(R::Rax, 0));
            self.a.load(true, R::Rax, mem(R::Rax, 8 * m.index() as i32));
            self.a.test_rr(true, R::Rax, R::Rax);
            self.a.jcc(Cond::Ne, found);
            self.a.bind(learn);
        }
        self.a.mov_ri(R::Rsi, m as *const Method as i64);
        self.a.mov_ri(R::Rdx, cell);
        self.a.call_abs(runtime::ic_miss as *const () as usize);
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
        self.a.load(true, R::Rdx, mem(R::Rax, 8));
        self.a.bind(done);
    }
}
