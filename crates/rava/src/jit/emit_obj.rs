//! Templates for fields, type checks and allocation.

use super::asm::{mem, Alu, Asm, Cond, Label, Shift, M, R};
use super::compile::Emitter;
use super::runtime;
use crate::klass::{InitState, Klass, ACC_ABSTRACT, ACC_INTERFACE};
use crate::link::Resolved;
use crate::types::BasicType;
use std::mem::offset_of;

impl Emitter {
    pub(super) fn cp_index(&self, pc: usize) -> u16 {
        u16::from_be_bytes([self.code[pc + 1], self.code[pc + 2]])
    }

    fn resolved_class(&self, pc: usize) -> Option<&'static Klass> {
        match self.m.holder.resolved[self.cp_index(pc) as usize].get() {
            Some(Resolved::Class(k)) => Some(k),
            _ => None,
        }
    }

    pub(super) fn field(&mut self, pc: usize, depth: u16, op: u8) {
        let d = depth as i32;
        match (op, self.m.holder.resolved[self.cp_index(pc) as usize].get()) {
            (0xb4, Some(Resolved::InstanceField { offset, ty, .. })) => {
                let slow = self.slow_path(pc, depth);
                self.a.load(true, R::Rax, self.stk(d - 1));
                self.a.test_rr(true, R::Rax, R::Rax);
                self.a.jcc(Cond::E, slow);
                load_typed(&mut self.a, ty, mem(R::Rax, offset as i32));
                self.a.store(true, self.stk(d - 1), R::Rax);
            }
            (0xb5, Some(Resolved::InstanceField { offset, ty, volatile })) => {
                let slots = ty.slots() as i32;
                let slow = self.slow_path(pc, depth);
                self.a.load(true, R::Rax, self.stk(d - 1 - slots));
                self.a.test_rr(true, R::Rax, R::Rax);
                self.a.jcc(Cond::E, slow);
                self.a.load(true, R::Rdx, self.stk(d - slots));
                store_typed(&mut self.a, ty, mem(R::Rax, offset as i32));
                if volatile {
                    self.a.storeload();
                }
            }
            (0xb2 | 0xb3, Some(Resolved::StaticField { holder, slot, ty, volatile })) => {
                let slow = self.slow_path(pc, depth);
                self.init_check(holder, slow);
                self.a.mov_ri(R::Rcx, slot as *const _ as i64);
                if op == 0xb2 {
                    load_static_to(&mut self.a, R::Rax, ty, mem(R::Rcx, 0));
                    self.a.store(true, self.stk(d), R::Rax);
                } else {
                    self.a.load(true, R::Rdx, self.stk(d - ty.slots() as i32));
                    normalize_into(&mut self.a, ty);
                    self.a.store(true, mem(R::Rcx, 0), R::Rdx);
                    if volatile {
                        self.a.storeload();
                    }
                }
            }
            _ => self.defer_unresolved(pc, depth),
        }
    }

    /// Jumps to `slow` unless `k` is initialized (the slow path initializes it).
    pub(super) fn init_check(&mut self, k: &'static Klass, slow: Label) {
        init_check(&mut self.a, k, R::Rsi, slow);
    }

    /// With the object's class in `rdi`: sets `rax` to 1 if it is a subtype of `k`, else 0.
    /// The exact class and the last class that passed at this site are checked inline.
    fn subtype_rdi(&mut self, k: &'static Klass) {
        let cell: &'static std::sync::atomic::AtomicUsize = Box::leak(Box::new(std::sync::atomic::AtomicUsize::new(0)));
        let done = self.a.new_label();
        self.a.mov_ri(R::Rsi, k as *const Klass as i64);
        self.a.mov_ri(R::Rax, 1);
        self.a.alu_rr(true, Alu::Cmp, R::Rdi, R::Rsi);
        self.a.jcc(Cond::E, done);
        if let Some(d) = primary_depth(k) {
            // A class: its ancestor at k's depth is k, or the answer is no.
            self.a.alu_rm(true, Alu::Cmp, R::Rsi, mem(R::Rdi, offset_of!(Klass, primary) as i32 + 8 * d as i32));
            self.a.jcc(Cond::E, done);
            self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
            self.a.bind(done);
            return;
        }
        self.a.mov_ri(R::Rdx, cell as *const _ as i64);
        self.a.alu_rm(true, Alu::Cmp, R::Rdi, mem(R::Rdx, 0));
        self.a.jcc(Cond::E, done);
        self.a.call_abs(runtime::subtype_cached as *const () as usize);
        self.a.bind(done);
    }

    /// `monitorenter` / `monitorexit`: the thin-lock fast paths; anything else (null,
    /// recursion, contention) runs in the interpreter.
    pub(super) fn monitor(&mut self, pc: usize, depth: u16, enter: bool) {
        let slow = self.slow_path(pc, depth);
        self.a.load(true, R::Rcx, self.stk(depth as i32 - 1));
        self.a.test_rr(true, R::Rcx, R::Rcx);
        self.a.jcc(Cond::E, slow);
        if enter {
            monitor_enter_fast(&mut self.a, R::Rcx, slow);
        } else {
            monitor_exit_fast(&mut self.a, R::Rcx, slow);
        }
    }

    pub(super) fn checkcast(&mut self, pc: usize, depth: u16) {
        let Some(k) = self.resolved_class(pc) else { return self.defer_unresolved(pc, depth) };
        let (done, slow) = (self.a.new_label(), self.slow_path(pc, depth));
        self.a.load(true, R::Rax, self.stk(depth as i32 - 1));
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::E, done);
        self.a.load(false, R::Rdi, mem(R::Rax, 0));
        self.subtype_rdi(k);
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::E, slow);
        self.a.bind(done);
    }

    pub(super) fn instanceof(&mut self, pc: usize, depth: u16) {
        let Some(k) = self.resolved_class(pc) else { return self.defer_unresolved(pc, depth) };
        let slot = self.stk(depth as i32 - 1);
        let done = self.a.new_label();
        self.a.load(true, R::Rax, slot);
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::E, done);
        self.a.load(false, R::Rdi, mem(R::Rax, 0));
        self.subtype_rdi(k);
        self.a.bind(done);
        self.a.store(true, slot, R::Rax);
    }

    pub(super) fn new_object(&mut self, pc: usize, depth: u16) {
        let k = match self.resolved_class(pc) {
            Some(k) if k.access & (ACC_ABSTRACT | ACC_INTERFACE) == 0 => k,
            _ => return self.defer_unresolved(pc, depth),
        };
        let slow = self.slow_path(pc, depth);
        self.init_check(k, slow);
        let (done, refill) = (self.a.new_label(), self.a.new_label());
        self.alloc_inline(k, refill);
        self.a.jmp(done);
        self.a.bind(refill);
        self.set_bci(pc);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.mov_ri(R::Rsi, k as *const Klass as i64);
        self.link_scopes();
        self.a.call_abs(runtime::new_instance as *const () as usize);
        self.unlink_scopes();
        self.check_pending();
        self.a.bind(done);
        self.a.store(true, self.stk(depth as i32), R::Rax);
    }

    /// rax := a new instance of `k` bumped from this thread's allocation cache (zeroed memory,
    /// class word written), or a jump to `slow` when the cache's bump range is exhausted.
    pub(super) fn alloc_inline(&mut self, k: &'static Klass, slow: Label) {
        let Some((size, bump, end)) = crate::gc::bump_layout(k.instance_size() as usize) else { return self.a.jmp(slow) };
        let at = |off: usize| mem(R::R12, (std::mem::offset_of!(crate::thread::Thread, alloc) + off) as i32);
        self.a.load(true, R::Rax, at(bump));
        self.a.lea(R::Rdx, mem(R::Rax, size as i32));
        self.a.alu_rm(true, Alu::Cmp, R::Rdx, at(end));
        self.a.jcc(Cond::A, slow);
        self.a.store(true, at(bump), R::Rdx);
        self.a.mov_ri(R::Rdx, k as *const Klass as i64);
        self.a.store(false, mem(R::Rax, 0), R::Rdx);
    }

    /// `newarray` / `anewarray`: a negative length takes the interpreter's exception path.
    pub(super) fn new_array(&mut self, pc: usize, depth: u16, op: u8) {
        let vm = crate::vm::vm();
        let k = if op == 0xbc {
            BasicType::from_newarray_code(self.code[pc + 1]).and_then(|t| vm.primitive_array_class(t).ok())
        } else {
            self.resolved_class(pc).and_then(|e| vm.array_class_of(e).ok())
        };
        let Some(k) = k else { return self.defer_unresolved(pc, depth) };
        let slot = self.stk(depth as i32 - 1);
        let slow = self.slow_path(pc, depth);
        self.a.load(false, R::Rdx, slot);
        self.a.test_rr(false, R::Rdx, R::Rdx);
        self.a.jcc(Cond::S, slow);
        self.set_bci(pc);
        self.a.mov_rr(true, R::Rdi, R::R12);
        self.a.mov_ri(R::Rsi, k as *const Klass as i64);
        self.link_scopes();
        self.a.call_abs(runtime::new_array as *const () as usize);
        self.unlink_scopes();
        self.check_pending();
        self.a.store(true, slot, R::Rax);
    }

    /// `aastore` store check for a non-null value in `rdx` into the array in `rax`: falls
    /// through when assignable (registers reloaded), else jumps to `slow`.
    pub(super) fn aastore_check(&mut self, array: M, index: M, value: M, slow: Label) {
        let ok = self.a.new_label();
        self.a.load(false, R::Rsi, mem(R::Rax, 0));
        self.a.load(true, R::Rsi, mem(R::Rsi, offset_of!(Klass, elem_klass) as i32));
        self.a.load(false, R::Rdi, mem(R::Rdx, 0));
        self.a.alu_rr(true, Alu::Cmp, R::Rdi, R::Rsi);
        self.a.jcc(Cond::E, ok);
        // Anything goes into an Object[] (ArrayList's and HashMap's arrays).
        if let Some(&object) = crate::vm::vm().wk.object.get() {
            self.a.mov_ri(R::R11, object as *const Klass as i64);
            self.a.alu_rr(true, Alu::Cmp, R::Rsi, R::R11);
            self.a.jcc(Cond::E, ok);
        }
        self.a.call_abs(runtime::subtype as *const () as usize);
        self.a.test_rr(true, R::Rax, R::Rax);
        self.a.jcc(Cond::E, slow);
        self.a.load(true, R::Rax, array);
        self.a.load(false, R::Rcx, index);
        self.a.load(true, R::Rdx, value);
        self.a.bind(ok);
    }
}

/// Jumps to `slow` unless `k` is initialized or being initialized by this thread (`r12`), using
/// `scratch`.
pub(super) fn init_check(a: &mut Asm, k: &'static Klass, scratch: R, slow: Label) {
    if k.init.state() == InitState::Initialized {
        return;
    }
    let lock = offset_of!(Klass, init);
    let ok = a.new_label();
    a.mov_ri(scratch, k as *const Klass as i64);
    a.cmp8_mi(mem(scratch, (lock + crate::klass::InitLock::state_offset()) as i32), InitState::Initialized as u8);
    a.jcc(Cond::E, ok);
    a.alu_rm(true, Alu::Cmp, R::R12, mem(scratch, (lock + crate::klass::InitLock::initializer_offset()) as i32));
    a.jcc(Cond::Ne, slow);
    a.bind(ok);
}

/// Takes the thin lock of the non-null object in `o` (not rax, rdx or r11) when it is unlocked,
/// else jumps to `slow` (recursion, contention, inflated monitors).
pub(super) fn monitor_enter_fast(a: &mut Asm, o: R, slow: Label) {
    use crate::sync::{HASH_BITS, OWNER_SHIFT, THIN};
    let mark = mem(o, 8);
    a.load(true, R::Rax, mark);
    a.mov_rr(false, R::Rdx, R::Rax);
    a.alu_ri(false, Alu::And, R::Rdx, 3);
    a.jcc(Cond::Ne, slow);
    a.load(true, R::Rdx, mem(R::R12, offset_of!(crate::thread::Thread, lock_token) as i32));
    a.shift_ri(true, super::asm::Shift::Shl, R::Rdx, OWNER_SHIFT as u8);
    a.alu_ri(true, Alu::Or, R::Rdx, THIN as i32);
    a.mov_ri(R::R11, HASH_BITS as i64);
    a.alu_rr(true, Alu::And, R::R11, R::Rax);
    a.alu_rr(true, Alu::Or, R::Rdx, R::R11);
    a.lock_cmpxchg(mark, R::Rdx);
    a.jcc(Cond::Ne, slow);
}

/// Releases the thin lock this thread holds once on the non-null object in `o` (not rax or
/// rdx), else jumps to `slow`.
pub(super) fn monitor_exit_fast(a: &mut Asm, o: R, slow: Label) {
    use crate::sync::{HASH_BITS, OWNER_SHIFT, THIN};
    let mark = mem(o, 8);
    a.load(true, R::Rax, mark);
    a.mov_rr(false, R::Rdx, R::Rax);
    a.alu_ri(false, Alu::And, R::Rdx, 0xff);
    a.alu_ri(false, Alu::Cmp, R::Rdx, THIN as i32);
    a.jcc(Cond::Ne, slow);
    a.mov_rr(true, R::Rdx, R::Rax);
    a.shift_ri(true, super::asm::Shift::Shr, R::Rdx, OWNER_SHIFT as u8);
    a.alu_rm(true, Alu::Cmp, R::Rdx, mem(R::R12, offset_of!(crate::thread::Thread, lock_token) as i32));
    a.jcc(Cond::Ne, slow);
    a.mov_ri(R::Rdx, HASH_BITS as i64);
    a.alu_rr(true, Alu::And, R::Rdx, R::Rax);
    a.lock_cmpxchg(mark, R::Rdx);
    a.jcc(Cond::Ne, slow);
}

/// Loads a field of type `ty` at `m` into rax in slot encoding.
pub(super) fn load_typed(a: &mut Asm, ty: BasicType, m: M) {
    load_typed_to(a, R::Rax, ty, m)
}

/// Loads a field or element of type `ty` at `m`, in a heap object, into `dst` in slot encoding
/// (a compressed reference decoded). The load is the first instruction (a fault site).
pub(crate) fn load_typed_to(a: &mut Asm, dst: R, ty: BasicType, m: M) {
    if ty == BasicType::Reference && crate::heap::COMPRESSED {
        a.load(false, dst, m);
        a.shift_ri(true, Shift::Shl, dst, crate::heap::NARROW_SHIFT as u8);
        return;
    }
    load_static_to(a, dst, ty, m)
}

/// Loads a static slot (or other full-width storage) of type `ty` at `m` into `dst` in slot
/// encoding.
pub(crate) fn load_static_to(a: &mut Asm, dst: R, ty: BasicType, m: M) {
    match ty {
        // Booleans hold 0 or 1, so their loads zero-extend, which also serves `b[i] & 0xff`.
        BasicType::Boolean => a.load_zx(1, dst, m),
        BasicType::Byte => a.load_sx(1, dst, m),
        BasicType::Char => a.load_zx(2, dst, m),
        BasicType::Short => a.load_sx(2, dst, m),
        BasicType::Int => a.load_sx(4, dst, m),
        BasicType::Float => a.load(false, dst, m),
        _ => a.load(true, dst, m),
    }
}

/// Stores rdx (slot encoding) into a field of type `ty` at `m` (rdx is encoded first: a
/// reference store clobbers it).
pub(super) fn store_typed(a: &mut Asm, ty: BasicType, m: M) {
    if ty == BasicType::Reference {
        encode_ref(a, R::Rdx, R::Rdx);
    }
    store_typed_from(a, R::Rdx, ty, m)
}

/// The register holding reference `src` in its stored form (`heap::encode`): `src` itself
/// unless references are compressed, else `tmp` (which may be `src`) with the compressed value.
pub(crate) fn encode_ref(a: &mut Asm, src: R, tmp: R) -> R {
    if !crate::heap::COMPRESSED {
        return src;
    }
    if src != tmp {
        a.mov_rr(true, tmp, src);
    }
    a.shift_ri(true, Shift::Shr, tmp, crate::heap::NARROW_SHIFT as u8);
    tmp
}

/// Stores `src` (slot encoding; a reference in its stored form, `encode_ref`) into a field of
/// type `ty` at `m`, which lies in a heap object; a boolean store masks `src`, a reference store
/// marks its card. The store is the first instruction (a fault site). Clobbers r11.
pub(crate) fn store_typed_from(a: &mut Asm, src: R, ty: BasicType, m: M) {
    store_raw(a, src, ty, m);
    if ty == BasicType::Reference {
        card_mark(a, m);
    }
}

/// The write barrier of a reference store to `m`, inside the heap: marks its card
/// (`gc::cards`). Clobbers r11; `m` must not use r11.
pub(crate) fn card_mark(a: &mut Asm, m: M) {
    a.lea(R::R11, m);
    a.shift_ri(true, Shift::Shr, R::R11, crate::gc::cards::CARD_SHIFT as u8);
    a.alu_rm(true, Alu::Add, R::R11, mem(R::R12, crate::thread::offsets::CARDS));
    a.store8_imm(mem(R::R11, 0), 1);
}

/// The write barrier of a reference store to `m`, which may be off-heap (Unsafe with a null
/// base addresses static fields). Clobbers r11 and the flags.
pub(crate) fn card_mark_checked(a: &mut Asm, m: M) {
    use crate::thread::offsets;
    let skip = a.new_label();
    a.lea(R::R11, m);
    a.alu_rm(true, Alu::Sub, R::R11, mem(R::R12, offsets::HEAP_BASE));
    a.alu_rm(true, Alu::Cmp, R::R11, mem(R::R12, offsets::HEAP_SPAN));
    a.jcc(Cond::Ae, skip);
    a.shift_ri(true, Shift::Shr, R::R11, crate::gc::cards::CARD_SHIFT as u8);
    a.alu_rm(true, Alu::Add, R::R11, mem(R::R12, offsets::CARD_TABLE));
    a.store8_imm(mem(R::R11, 0), 1);
    a.bind(skip);
}

/// Unsafe access to a reference at `m`: `heap` emits it for an address in the heap (a field or
/// element), `slot` for one outside it (a static slot, full width: a null base with an absolute
/// address, or the class mirror with an offset from it, as HotSpot's static field bases are).
/// With uncompressed references the two are the same code and only `heap` is emitted.
pub(crate) fn unsafe_ref(a: &mut Asm, m: M, heap: impl FnOnce(&mut Asm), slot: impl FnOnce(&mut Asm)) {
    use crate::thread::offsets;
    if !crate::heap::COMPRESSED {
        return heap(a);
    }
    let (absolute, done) = (a.new_label(), a.new_label());
    // rsi is borrowed for the address (pop keeps the flags), so no register is needed free.
    a.push(R::Rsi);
    a.lea(R::Rsi, m);
    a.alu_rm(true, Alu::Sub, R::Rsi, mem(R::R12, offsets::HEAP_BASE));
    a.alu_rm(true, Alu::Cmp, R::Rsi, mem(R::R12, offsets::HEAP_SPAN));
    a.pop(R::Rsi);
    a.jcc(Cond::Ae, absolute);
    heap(a);
    a.jmp(done);
    a.bind(absolute);
    slot(a);
    a.bind(done);
}

/// An Unsafe load of type `ty` at `m` into `dst`.
pub(crate) fn unsafe_load(a: &mut Asm, dst: R, ty: BasicType, m: M) {
    if ty != BasicType::Reference {
        return load_static_to(a, dst, ty, m);
    }
    unsafe_ref(a, m, |a| load_typed_to(a, dst, ty, m), |a| load_static_to(a, dst, ty, m));
}

/// An Unsafe store of `src` (slot encoding) of type `ty` at `m`, with the write barrier of a
/// reference. `m` must not use r11; clobbers r11.
pub(crate) fn unsafe_store(a: &mut Asm, src: R, ty: BasicType, m: M) {
    if ty != BasicType::Reference {
        return store_raw(a, src, ty, m);
    }
    unsafe_ref(
        a,
        m,
        |a| {
            let enc = encode_ref(a, src, R::R11);
            store_raw(a, enc, ty, m);
        },
        |a| a.store(true, m, src),
    );
    card_mark_checked(a, m);
}

/// An Unsafe compare-and-set of type `ty` at `m`:
/// expected value in rax, new one in r11 (both clobbered); leaves the flags (ZF: done) and, for
/// a reference, marks the card.
pub(crate) fn unsafe_cas(a: &mut Asm, ty: BasicType, m: M) {
    if ty != BasicType::Reference {
        return a.lock_cmpxchg_w(ty != BasicType::Int, m, R::R11);
    }
    unsafe_ref(
        a,
        m,
        |a| {
            encode_ref(a, R::Rax, R::Rax);
            encode_ref(a, R::R11, R::R11);
            a.lock_cmpxchg_w(!crate::heap::COMPRESSED, m, R::R11);
        },
        |a| a.lock_cmpxchg_w(true, m, R::R11),
    );
}

/// Stores `src` into a field of type `ty` at `m` without a write barrier (a reference in its
/// stored form, `encode_ref`).
pub(crate) fn store_raw(a: &mut Asm, src: R, ty: BasicType, m: M) {
    match ty {
        BasicType::Reference if crate::heap::COMPRESSED => a.store(false, m, src),
        BasicType::Boolean => {
            a.alu_ri(false, Alu::And, src, 1);
            a.store8(m, src);
        }
        BasicType::Byte => a.store8(m, src),
        BasicType::Char | BasicType::Short => a.store16(m, src),
        BasicType::Int | BasicType::Float => a.store(false, m, src),
        _ => a.store(true, m, src),
    }
}

/// Normalizes rdx to the full-slot representation of a static field of type `ty`.
pub(super) fn normalize_into(a: &mut Asm, ty: BasicType) {
    match ty {
        BasicType::Boolean => a.alu_ri(false, Alu::And, R::Rdx, 1),
        BasicType::Byte => a.movsx_rr(1, R::Rdx, R::Rdx),
        BasicType::Char => a.movzx16_rr(R::Rdx, R::Rdx),
        BasicType::Short => a.movsx_rr(2, R::Rdx, R::Rdx),
        BasicType::Int => a.movsxd(R::Rdx, R::Rdx),
        BasicType::Float => a.mov_rr(false, R::Rdx, R::Rdx),
        _ => {}
    }
}

/// The depth of `k` when subtype checks against it can use the primary supers display (a class
/// that is neither an interface nor an array, shallow enough): `Klass::primary`.
pub(crate) fn primary_depth(k: &'static Klass) -> Option<usize> {
    let class = k.access & crate::klass::ACC_INTERFACE == 0 && matches!(k.kind, crate::klass::KlassKind::Instance);
    (class && (k.depth as usize) < crate::klass::PRIMARY).then_some(k.depth as usize)
}
