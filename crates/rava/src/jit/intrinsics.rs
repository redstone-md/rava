//! JDK methods compiled in place, as HotSpot's intrinsics are: `Math` rounding and square root,
//! `Unsafe` memory access, `Class.isInstance`, `Object.getClass`, float bit conversions and the
//! identity hash. Each template reads its
//! arguments from the call's operand slots and leaves the result in the first; a case it does
//! not handle (a null receiver, a class without a mirror yet) takes the call's slow path.

use super::asm::{mem, mem_idx, Alu, Cond, Label, R, X};
use super::compile::Emitter;
use super::emit_obj::load_typed;
use super::runtime;
use crate::klass::{Klass, ACC_ABSTRACT, ACC_INTERFACE};
use crate::method::Method;
use crate::types::BasicType;
use std::mem::offset_of;

#[derive(Clone, Copy, Debug)]
pub(super) enum Intrinsic {
    /// `roundsd` with the given rounding mode (floor 1, ceil 2, rint 0).
    Round(u8),
    Sqrt,
    /// `Unsafe.getX(Object, long)`; any memory order (x86 loads are acquire), `true` for a
    /// volatile or acquire read (later loads must not reuse values read before it).
    UnsafeGet(BasicType, bool),
    /// `Unsafe.putX(Object, long, x)`; `true` for a volatile store (followed by a fence).
    UnsafePut(BasicType, bool),
    IsInstance,
    GetClass,
    /// `Math.min/max` on int, long, float or double (the type), `true` for max.
    MinMax(BasicType, bool),
    Abs(BasicType),
    /// `Unsafe.compareAndSetX(Object, long, expected, x)` (and the weak forms) for int, long and
    /// reference fields: a locked compare-and-exchange.
    CompareAndSet(BasicType),
    /// `Thread.currentThread()`.
    CurrentThread,
    /// `Double.doubleToRawLongBits` / `longBitsToDouble` / `Float.floatToRawIntBits` /
    /// `intBitsToFloat`: the same bits as the result type.
    Bits(BasicType),
    /// `Object.hashCode` / `System.identityHashCode`: the hash in the mark word once assigned
    /// (0 for null), else the runtime assigns it.
    IdentityHash,
    /// `System.arraycopy`: checked and copied by a runtime helper called directly (no native
    /// frame); any check failing leaves compiled code for the interpreter to throw.
    ArrayCopy,
}

impl Intrinsic {
    /// Needs no deoptimization state (usable where the optimizing tier has none at hand).
    pub(super) fn is_stateless(self) -> bool {
        !matches!(self, Intrinsic::GetClass | Intrinsic::ArrayCopy)
    }

    /// Without side effects (usable inside inlined leaf bodies).
    pub(super) fn is_pure(self) -> bool {
        !matches!(self, Intrinsic::UnsafePut(..) | Intrinsic::CompareAndSet(_) | Intrinsic::IdentityHash | Intrinsic::UnsafeGet(_, true) | Intrinsic::ArrayCopy)
    }
}

fn unsafe_type(name: &str) -> Option<BasicType> {
    Some(match name {
        "Reference" => BasicType::Reference,
        "Int" => BasicType::Int,
        "Long" => BasicType::Long,
        "Boolean" => BasicType::Boolean,
        "Byte" => BasicType::Byte,
        "Short" => BasicType::Short,
        "Char" => BasicType::Char,
        "Float" => BasicType::Float,
        "Double" => BasicType::Double,
        _ => return None,
    })
}

/// Splits `getIntVolatile` into ("Int", "Volatile").
fn unsafe_access(rest: &str) -> Option<(BasicType, &str)> {
    ["Volatile", "Acquire", "Release", "Opaque", ""]
        .iter()
        .find_map(|order| rest.strip_suffix(order).and_then(unsafe_type).map(|ty| (ty, *order)))
}

/// The intrinsic a call site whose target is statically `m` compiles to: a virtual method only
/// when no override can be the target (the optimizing tier also uses `intrinsic` behind guards).
pub(super) fn bound_intrinsic(m: &Method) -> Option<Intrinsic> {
    let bound = m.is_static() || m.is_private() || m.access & crate::klass::ACC_FINAL != 0 || m.holder.access & crate::klass::ACC_FINAL != 0;
    intrinsic(m).filter(|_| bound)
}

pub(super) fn intrinsic(m: &Method) -> Option<Intrinsic> {
    let (holder, name, desc) = (m.holder.name.as_str(), m.name.as_str(), m.desc.as_str());
    match holder {
        "java/lang/Math" | "java/lang/StrictMath" if matches!(name, "min" | "max" | "abs") => {
            let ty = match desc {
                "(II)I" | "(I)I" => BasicType::Int,
                "(JJ)J" | "(J)J" => BasicType::Long,
                "(FF)F" | "(F)F" => BasicType::Float,
                "(DD)D" | "(D)D" => BasicType::Double,
                _ => return None,
            };
            Some(if name == "abs" { Intrinsic::Abs(ty) } else { Intrinsic::MinMax(ty, name == "max") })
        }
        "java/lang/Math" | "java/lang/StrictMath" if desc == "(D)D" => match name {
            "sqrt" => Some(Intrinsic::Sqrt),
            _ if !std::arch::is_x86_feature_detected!("sse4.1") => None,
            "floor" => Some(Intrinsic::Round(1)),
            "ceil" => Some(Intrinsic::Round(2)),
            "rint" => Some(Intrinsic::Round(0)),
            _ => None,
        },
        "jdk/internal/misc/Unsafe" if !m.is_static() => {
            let cas = name.strip_prefix("compareAndSet").or_else(|| name.strip_prefix("weakCompareAndSet"));
            if let Some(rest) = cas {
                let ty = ["", "Plain", "Acquire", "Release"].iter().find_map(|o| rest.strip_suffix(o).and_then(unsafe_type))?;
                return (matches!(ty, BasicType::Int | BasicType::Long | BasicType::Reference) && desc.ends_with(")Z")).then_some(Intrinsic::CompareAndSet(ty));
            }
            if let Some((ty, order)) = name.strip_prefix("get").and_then(unsafe_access) {
                return desc.starts_with("(Ljava/lang/Object;J)").then_some(Intrinsic::UnsafeGet(ty, matches!(order, "Volatile" | "Acquire")));
            }
            let (ty, order) = name.strip_prefix("put").and_then(unsafe_access)?;
            (desc.starts_with("(Ljava/lang/Object;J") && desc.ends_with(")V") && m.sig.params.len() == 3).then_some(Intrinsic::UnsafePut(ty, order == "Volatile"))
        }
        "java/lang/Class" if name == "isInstance" && desc == "(Ljava/lang/Object;)Z" => Some(Intrinsic::IsInstance),
        "java/lang/Object" if name == "getClass" && desc == "()Ljava/lang/Class;" => Some(Intrinsic::GetClass),
        "java/lang/System" if name == "arraycopy" && desc == "(Ljava/lang/Object;ILjava/lang/Object;II)V" => Some(Intrinsic::ArrayCopy),
        "java/lang/Thread" if name == "currentThread" && desc == "()Ljava/lang/Thread;" => Some(Intrinsic::CurrentThread),
        "java/lang/Double" | "java/lang/Float" => match (name, desc) {
            ("doubleToRawLongBits", "(D)J") => Some(Intrinsic::Bits(BasicType::Long)),
            ("longBitsToDouble", "(J)D") => Some(Intrinsic::Bits(BasicType::Double)),
            ("floatToRawIntBits", "(F)I") => Some(Intrinsic::Bits(BasicType::Int)),
            ("intBitsToFloat", "(I)F") => Some(Intrinsic::Bits(BasicType::Float)),
            _ => None,
        },
        "java/lang/Object" if name == "hashCode" && desc == "()I" => Some(Intrinsic::IdentityHash),
        "java/lang/System" if name == "identityHashCode" && desc == "(Ljava/lang/Object;)I" => Some(Intrinsic::IdentityHash),
        _ => None,
    }
}

impl Emitter {
    /// `Unsafe.allocateInstance` of a constant, instantiable class: an inline allocation once
    /// the class is initialized. Any other case (and a refill) makes the ordinary call, which
    /// the caller emits next; returns the label to bind after it.
    pub(super) fn allocate_instance(&mut self, m: &'static Method, args: i32, mirror: Option<i64>) -> Option<Label> {
        if m.holder.name != "jdk/internal/misc/Unsafe" || m.name != "allocateInstance" {
            return None;
        }
        let k = crate::runtime::klass_of_mirror(mirror? as crate::heap::Oop)?;
        let instantiable = matches!(k.kind, crate::klass::KlassKind::Instance) && k.access & (ACC_ABSTRACT | ACC_INTERFACE) == 0 && k.name != "java/lang/Class";
        if !instantiable {
            return None;
        }
        let (call, done) = (self.a.new_label(), self.a.new_label());
        self.init_check(k, call);
        self.alloc_inline(k, call);
        self.a.store(true, self.stk(args), R::Rax);
        self.a.jmp(done);
        self.a.bind(call);
        Some(done)
    }

    /// Compiles the call at `pc` to intrinsic `m` in place; false if `m` is not one.
    pub(super) fn try_intrinsic(&mut self, pc: usize, depth: u16, m: &'static Method, args: i32) -> bool {
        let Some(k) = bound_intrinsic(m) else { return false };
        match k {
            // The optimizing tier's only.
            Intrinsic::ArrayCopy => return false,
            Intrinsic::Round(mode) => {
                self.a.fload(true, X(0), self.stk(args));
                self.a.round_sd(X(0), X(0), mode);
                self.a.fstore(true, self.stk(args), X(0));
            }
            Intrinsic::Sqrt => {
                self.a.fload(true, X(0), self.stk(args));
                self.a.fsqrt(X(0), X(0));
                self.a.fstore(true, self.stk(args), X(0));
            }
            Intrinsic::UnsafeGet(ty, _) => {
                // A null base makes the offset an absolute address.
                self.a.load(true, R::Rax, self.stk(args + 1));
                self.a.load(true, R::Rcx, self.stk(args + 2));
                load_typed(&mut self.a, ty, mem_idx(R::Rax, R::Rcx, 1, 0));
                self.a.store(true, self.stk(args), R::Rax);
            }
            Intrinsic::UnsafePut(ty, volatile) => {
                self.a.load(true, R::Rax, self.stk(args + 1));
                self.a.load(true, R::Rcx, self.stk(args + 2));
                self.a.load(true, R::Rdx, self.stk(args + 4));
                super::emit_obj::store_raw(&mut self.a, R::Rdx, ty, mem_idx(R::Rax, R::Rcx, 1, 0));
                if ty == BasicType::Reference {
                    super::emit_obj::card_mark_checked(&mut self.a, mem_idx(R::Rax, R::Rcx, 1, 0));
                }
                if volatile {
                    self.a.storeload();
                }
            }
            Intrinsic::IsInstance => self.is_instance(pc, depth, args),
            Intrinsic::MinMax(ty, max) => self.min_max(ty, max, args),
            Intrinsic::Abs(ty) => self.abs(ty, args),
            Intrinsic::CompareAndSet(ty) => {
                // [unsafe, o, offset (2 slots), expected, x]; a null base makes the offset absolute.
                let wide = ty == BasicType::Long;
                self.a.load(true, R::Rcx, self.stk(args + 1));
                self.a.load(true, R::Rdx, self.stk(args + 2));
                self.a.load(true, R::Rax, self.stk(args + 4));
                self.a.load(true, R::R11, self.stk(args + if wide { 6 } else { 5 }));
                self.a.lock_cmpxchg_w(ty != BasicType::Int, mem_idx(R::Rcx, R::Rdx, 1, 0), R::R11);
                self.a.setcc(Cond::E, R::Rax);
                self.a.movzx8(R::Rax, R::Rax);
                self.a.store(true, self.stk(args), R::Rax);
                if ty == BasicType::Reference {
                    super::emit_obj::card_mark_checked(&mut self.a, mem_idx(R::Rcx, R::Rdx, 1, 0));
                }
            }
            Intrinsic::CurrentThread => {
                self.a.load(true, R::Rax, mem(R::R12, offset_of!(crate::thread::Thread, java_thread) as i32));
                self.a.store(true, self.stk(args), R::Rax);
            }
            // Doubles and longs share the slot encoding; an int is sign-extended, a float's
            // high half is clear.
            Intrinsic::Bits(BasicType::Int) => {
                self.a.load(true, R::Rax, self.stk(args));
                self.a.movsxd(R::Rax, R::Rax);
                self.a.store(true, self.stk(args), R::Rax);
            }
            Intrinsic::Bits(BasicType::Float) => {
                self.a.load(true, R::Rax, self.stk(args));
                self.a.mov_rr(false, R::Rax, R::Rax);
                self.a.store(true, self.stk(args), R::Rax);
            }
            Intrinsic::Bits(_) => {}
            Intrinsic::IdentityHash => {
                let (slow, done) = (self.slow_path(pc, depth), self.a.new_label());
                self.a.load(true, R::Rcx, self.stk(args));
                self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
                self.a.test_rr(true, R::Rcx, R::Rcx);
                self.a.jcc(Cond::E, done);
                identity_hash_fast(&mut self.a, R::Rcx);
                self.a.jcc(Cond::E, slow);
                self.a.bind(done);
                self.a.store(true, self.stk(args), R::Rax);
            }
            Intrinsic::GetClass => {
                let slow = self.slow_path(pc, depth);
                self.a.load(true, R::Rax, self.stk(args));
                self.a.test_rr(true, R::Rax, R::Rax);
                self.a.jcc(Cond::E, slow);
                self.a.load(true, R::Rax, mem(R::Rax, 0));
                self.a.load(true, R::Rax, mem(R::Rax, offset_of!(Klass, mirror) as i32));
                self.a.test_rr(true, R::Rax, R::Rax);
                self.a.jcc(Cond::E, slow);
                self.a.store(true, self.stk(args), R::Rax);
            }
        }
        true
    }

    /// `Math.min/max` with Java semantics: NaN wins, and -0.0 is below +0.0 (for equal floats
    /// the bitwise and/or of the operands picks the right zero).
    fn min_max(&mut self, ty: BasicType, max: bool, args: i32) {
        let wide = matches!(ty, BasicType::Long | BasicType::Double);
        let b = args + if wide { 2 } else { 1 };
        self.a.load(true, R::Rax, self.stk(args));
        self.a.load(true, R::Rcx, self.stk(b));
        if matches!(ty, BasicType::Int | BasicType::Long) {
            self.a.alu_rr(wide, Alu::Cmp, R::Rax, R::Rcx);
            self.a.cmov(true, if max { Cond::L } else { Cond::G }, R::Rax, R::Rcx);
        } else {
            let (nan, a_above, b_above, done) = (self.a.new_label(), self.a.new_label(), self.a.new_label(), self.a.new_label());
            self.a.mov_xr(wide, X(0), R::Rax);
            self.a.mov_xr(wide, X(1), R::Rcx);
            self.a.fcmp(wide, X(0), X(1));
            self.a.jcc(Cond::P, nan);
            self.a.jcc(Cond::A, a_above);
            self.a.jcc(Cond::B, b_above);
            self.a.alu_rr(wide, if max { Alu::And } else { Alu::Or }, R::Rax, R::Rcx);
            self.a.jmp(done);
            self.a.bind(nan);
            self.a.farith_rr(wide, 0x58, X(0), X(1));
            self.a.mov_rx(wide, R::Rax, X(0));
            self.a.jmp(done);
            // The larger operand is kept for max, the other one for min.
            self.a.bind(if max { b_above } else { a_above });
            self.a.mov_rr(true, R::Rax, R::Rcx);
            self.a.bind(if max { a_above } else { b_above });
            self.a.bind(done);
        }
        self.a.store(true, self.stk(args), R::Rax);
    }

    /// `Math.abs`: integers negate when negative (MIN_VALUE stays), floats clear the sign bit.
    fn abs(&mut self, ty: BasicType, args: i32) {
        self.a.load(true, R::Rax, self.stk(args));
        match ty {
            BasicType::Int | BasicType::Long => {
                let wide = ty == BasicType::Long;
                self.a.mov_rr(true, R::Rcx, R::Rax);
                self.a.neg(wide, R::Rcx);
                self.a.cmov(wide, Cond::S, R::Rcx, R::Rax);
                if wide {
                    self.a.mov_rr(true, R::Rax, R::Rcx);
                } else {
                    self.a.movsxd(R::Rax, R::Rcx);
                }
            }
            BasicType::Float => self.a.alu_ri(false, Alu::And, R::Rax, i32::MAX),
            _ => {
                self.a.mov_ri(R::Rcx, i64::MAX);
                self.a.alu_rr(true, Alu::And, R::Rax, R::Rcx);
            }
        }
        self.a.store(true, self.stk(args), R::Rax);
    }

    /// `mirror.isInstance(o)`: null → false, a primitive mirror → false, the exact class →
    /// true, else the subtype check.
    fn is_instance(&mut self, pc: usize, depth: u16, args: i32) {
        let (slow, done) = (self.slow_path(pc, depth), self.a.new_label());
        let klass_field = crate::runtime::mirror_klass_offset() as i32;
        self.a.load(true, R::Rsi, self.stk(args));
        self.a.test_rr(true, R::Rsi, R::Rsi);
        self.a.jcc(Cond::E, slow);
        self.a.alu_rr(false, Alu::Xor, R::Rax, R::Rax);
        self.a.load(true, R::Rdi, self.stk(args + 1));
        self.a.test_rr(true, R::Rdi, R::Rdi);
        self.a.jcc(Cond::E, done);
        self.a.load(true, R::Rsi, mem(R::Rsi, klass_field));
        self.a.test_rr(true, R::Rsi, R::Rsi);
        self.a.jcc(Cond::E, done);
        self.a.load(true, R::Rdi, mem(R::Rdi, 0));
        self.a.mov_ri(R::Rax, 1);
        self.a.alu_rr(true, Alu::Cmp, R::Rdi, R::Rsi);
        self.a.jcc(Cond::E, done);
        self.a.call_abs(runtime::subtype as *const () as usize);
        self.a.bind(done);
        self.a.store(true, self.stk(args), R::Rax);
    }
}

/// eax := the identity hash in the mark word of the non-null object in `o` (not rax), zero
/// flag set when none is assigned yet.
pub(super) fn identity_hash_fast(a: &mut super::asm::Asm, o: R) {
    a.load(true, R::Rax, mem(o, 8));
    a.shift_ri(true, super::asm::Shift::Shr, R::Rax, crate::natives::HASH_SHIFT as u8);
    a.alu_ri(false, Alu::And, R::Rax, crate::natives::HASH_MASK as i32);
}
