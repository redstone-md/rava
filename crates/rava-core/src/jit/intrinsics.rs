//! JDK methods compiled in place, as HotSpot's intrinsics are: `Math` rounding and square root,
//! `Unsafe` memory access, `Class.isInstance` and `Object.getClass`. Each template reads its
//! arguments from the call's operand slots and leaves the result in the first; a case it does
//! not handle (a null receiver, a class without a mirror yet) takes the call's slow path.

use super::asm::{mem, mem_idx, Alu, Cond, R, X};
use super::compile::Emitter;
use super::emit_obj::{load_typed, store_typed};
use super::runtime;
use crate::klass::Klass;
use crate::method::Method;
use crate::types::BasicType;
use std::mem::offset_of;

#[derive(Clone, Copy)]
pub(super) enum Intrinsic {
    /// `roundsd` with the given rounding mode (floor 1, ceil 2, rint 0).
    Round(u8),
    Sqrt,
    /// `Unsafe.getX(Object, long)`; any memory order (x86 loads are acquire).
    UnsafeGet(BasicType),
    /// `Unsafe.putX(Object, long, x)`; `true` for a volatile store (followed by a fence).
    UnsafePut(BasicType, bool),
    IsInstance,
    GetClass,
}

impl Intrinsic {
    /// Without side effects (usable inside inlined leaf bodies).
    pub(super) fn is_pure(self) -> bool {
        !matches!(self, Intrinsic::UnsafePut(..))
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

pub(super) fn intrinsic(m: &Method) -> Option<Intrinsic> {
    let (holder, name, desc) = (m.holder.name.as_str(), m.name.as_str(), m.desc.as_str());
    match holder {
        "java/lang/Math" | "java/lang/StrictMath" if desc == "(D)D" => match name {
            "sqrt" => Some(Intrinsic::Sqrt),
            _ if !std::arch::is_x86_feature_detected!("sse4.1") => None,
            "floor" => Some(Intrinsic::Round(1)),
            "ceil" => Some(Intrinsic::Round(2)),
            "rint" => Some(Intrinsic::Round(0)),
            _ => None,
        },
        "jdk/internal/misc/Unsafe" if !m.is_static() => {
            if let Some((ty, _)) = name.strip_prefix("get").and_then(unsafe_access) {
                return desc.starts_with("(Ljava/lang/Object;J)").then_some(Intrinsic::UnsafeGet(ty));
            }
            let (ty, order) = name.strip_prefix("put").and_then(unsafe_access)?;
            (desc.starts_with("(Ljava/lang/Object;J") && desc.ends_with(")V") && m.sig.params.len() == 3).then_some(Intrinsic::UnsafePut(ty, order == "Volatile"))
        }
        "java/lang/Class" if name == "isInstance" && desc == "(Ljava/lang/Object;)Z" => Some(Intrinsic::IsInstance),
        "java/lang/Object" if name == "getClass" && desc == "()Ljava/lang/Class;" => Some(Intrinsic::GetClass),
        _ => None,
    }
}

impl Emitter {
    /// Compiles the call at `pc` to intrinsic `m` in place; false if `m` is not one.
    pub(super) fn try_intrinsic(&mut self, pc: usize, depth: u16, m: &'static Method, args: i32) -> bool {
        let Some(k) = intrinsic(m) else { return false };
        match k {
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
            Intrinsic::UnsafeGet(ty) => {
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
                store_typed(&mut self.a, ty, mem_idx(R::Rax, R::Rcx, 1, 0));
                if volatile {
                    self.a.mfence();
                }
            }
            Intrinsic::IsInstance => self.is_instance(pc, depth, args),
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
