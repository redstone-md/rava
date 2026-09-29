//! `java.lang.String` construction, reading and interning. Strings use the JDK's own compact
//! layout: a `byte[] value` holding LATIN1 bytes or little-endian UTF-16 pairs, and a `coder`.

use super::{alloc_array, alloc_instance, initialize, vm_field};
use crate::heap::{obj, Oop};
use crate::symbol::decode_modified_utf8;
use crate::thread::{JResult, Thread};
use crate::types::BasicType;
use crate::vm::vm;

vm_field!(value_offset, "java/lang/String", "value", "[B");
vm_field!(coder_offset, "java/lang/String", "coder", "B");

const LATIN1: u8 = 0;
const UTF16: u8 = 1;

pub fn new_string(t: &mut Thread, s: &str) -> JResult<Oop> {
    let units: Vec<u16> = s.encode_utf16().collect();
    new_string_utf16(t, &units)
}

pub fn new_string_utf16(t: &mut Thread, units: &[u16]) -> JResult<Oop> {
    let string_class = vm().wk.string.get().copied().unwrap_or_else(|| crate::loader::load("java/lang/String").unwrap());
    initialize(t, string_class)?;
    let bytes_class = vm().primitive_array_class(BasicType::Byte).expect("[B");
    let latin1 = units.iter().all(|&u| u < 256);
    let len = if latin1 { units.len() } else { units.len() * 2 };
    let value = alloc_array(t, bytes_class, len as i32)?;
    // SAFETY: `value` is a fresh byte[] of `len` elements.
    unsafe {
        let data = obj::array_slice::<u8>(value);
        if latin1 {
            data.iter_mut().zip(units).for_each(|(d, &u)| *d = u as u8);
        } else {
            for (i, &u) in units.iter().enumerate() {
                data[2 * i..2 * i + 2].copy_from_slice(&u.to_le_bytes());
            }
        }
    }
    let s = alloc_instance(t, string_class)?;
    // SAFETY: `s` is a fresh String; offsets come from its layout.
    unsafe {
        obj::write::<Oop>(s, value_offset(), value);
        obj::write::<u8>(s, coder_offset(), if latin1 { LATIN1 } else { UTF16 });
    }
    Ok(s)
}

/// The UTF-16 content of a `java.lang.String`.
pub fn string_units(s: Oop) -> Vec<u16> {
    // SAFETY: `s` is a live String whose `value` is a byte[].
    unsafe {
        let value = obj::read::<Oop>(s, value_offset());
        let data = obj::array_slice::<u8>(value);
        if obj::read::<u8>(s, coder_offset()) == LATIN1 {
            data.iter().map(|&b| b as u16).collect()
        } else {
            data.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect()
        }
    }
}

pub fn java_string_to_rust(s: Oop) -> String {
    if s.is_null() {
        return "null".into();
    }
    String::from_utf16_lossy(&string_units(s))
}

/// The canonical String with the given content (`String.intern`).
pub fn intern(_t: &mut Thread, s: Oop) -> JResult<Oop> {
    let units = string_units(s);
    if let Some(&hit) = vm().strings.lock().unwrap().get(units.as_slice()) {
        return Ok(hit as Oop);
    }
    Ok(insert(units.into_boxed_slice(), s))
}

/// The interned String with the content of `s`.
pub fn intern_str(t: &mut Thread, s: &str) -> JResult<Oop> {
    let units: Vec<u16> = s.encode_utf16().collect();
    if let Some(&hit) = vm().strings.lock().unwrap().get(units.as_slice()) {
        return Ok(hit as Oop);
    }
    let o = new_string_utf16(t, &units)?;
    Ok(insert(units.into_boxed_slice(), o))
}

/// The interned String for a class-file literal (modified UTF-8).
pub fn intern_modified_utf8(t: &mut Thread, bytes: &[u8]) -> JResult<Oop> {
    let units = decode_modified_utf8(bytes);
    if let Some(&hit) = vm().strings.lock().unwrap().get(units.as_slice()) {
        return Ok(hit as Oop);
    }
    let s = new_string_utf16(t, &units)?;
    Ok(insert(units.into_boxed_slice(), s))
}

fn insert(units: Box<[u16]>, s: Oop) -> Oop {
    *vm().strings.lock().unwrap().entry(units).or_insert(s as usize) as Oop
}
