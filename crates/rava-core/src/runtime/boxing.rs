//! Boxing and unboxing between slot values and `java.lang.Integer`-style wrappers, with the
//! widening conversions reflection permits (JLS 5.1.2).

use super::{alloc_instance, initialize, throw_new};
use crate::heap::{obj, Oop};
use crate::interp::{read_field, write_field};
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use crate::types::BasicType;

fn wrapper_name(ty: BasicType) -> &'static str {
    match ty {
        BasicType::Boolean => "java/lang/Boolean",
        BasicType::Char => "java/lang/Character",
        BasicType::Float => "java/lang/Float",
        BasicType::Double => "java/lang/Double",
        BasicType::Byte => "java/lang/Byte",
        BasicType::Short => "java/lang/Short",
        BasicType::Int => "java/lang/Integer",
        BasicType::Long => "java/lang/Long",
        _ => "",
    }
}

fn value_offset(wrapper: &'static crate::klass::Klass, ty: BasicType) -> u32 {
    let desc = ty.desc_char().to_string();
    wrapper.declared_field(Symbol::intern("value"), Symbol::intern(&desc)).expect("wrapper value field").offset
}

/// Boxes a slot value of type `ty` (a fresh wrapper, as `Integer.valueOf` would for uncached values).
pub fn box_value(t: &mut Thread, ty: BasicType, v: u64) -> JResult<Oop> {
    if ty == BasicType::Reference {
        return Ok(v as Oop);
    }
    if ty == BasicType::Void {
        return Ok(std::ptr::null_mut());
    }
    let k = crate::loader::load(wrapper_name(ty)).expect("wrapper class");
    initialize(t, k)?;
    let valueof_desc = format!("({})L{};", ty.desc_char(), wrapper_name(ty));
    if let Some(m) = k.declared_method(Symbol::intern("valueOf"), Symbol::intern(&valueof_desc)) {
        let args: &[u64] = if ty.slots() == 2 { &[v, 0] } else { &[v] };
        return Ok(t.call(m, args)? as Oop);
    }
    let o = alloc_instance(t, k)?;
    // SAFETY: fresh wrapper; its `value` field has type `ty`.
    unsafe { write_field(o, value_offset(k, ty), ty, v) };
    Ok(o)
}

/// Unboxes `o` to a slot value of type `target`, applying primitive widening.
pub fn unbox_value(t: &mut Thread, o: Oop, target: BasicType) -> JResult<u64> {
    if target == BasicType::Reference {
        return Ok(o as u64);
    }
    let bad = |t: &mut Thread| throw_new(t, "java/lang/IllegalArgumentException", "argument type mismatch");
    if o.is_null() {
        return Err(bad(t));
    }
    // SAFETY: live object.
    let name = unsafe { obj::klass(o) }.name.as_str();
    let src = [
        BasicType::Boolean, BasicType::Char, BasicType::Float, BasicType::Double,
        BasicType::Byte, BasicType::Short, BasicType::Int, BasicType::Long,
    ]
    .into_iter()
    .find(|&b| wrapper_name(b) == name)
    .ok_or_else(|| bad(t))?;
    let k = crate::loader::load(name).expect("wrapper");
    // SAFETY: live wrapper of type `src`.
    let raw = unsafe { read_field(o, value_offset(k, src), src) };
    widen(src, target, raw).ok_or_else(|| bad(t))
}

fn widen(src: BasicType, dst: BasicType, v: u64) -> Option<u64> {
    use BasicType::*;
    if src == dst {
        return Some(v);
    }
    let as_long = match src {
        Byte | Short | Char | Int => v as i64,
        _ => return match (src, dst) {
            (Long, Float) => Some((v as i64 as f32).to_bits() as u64),
            (Long, Double) => Some((v as i64 as f64).to_bits()),
            (Float, Double) => Some((f32::from_bits(v as u32) as f64).to_bits()),
            _ => None,
        },
    };
    let ok = match src {
        Byte => matches!(dst, Short | Int | Long | Float | Double),
        Short | Char => matches!(dst, Int | Long | Float | Double),
        Int => matches!(dst, Long | Float | Double),
        _ => false,
    };
    ok.then(|| match dst {
        Float => (as_long as f32).to_bits() as u64,
        Double => (as_long as f64).to_bits(),
        _ => as_long as u64,
    })
}
