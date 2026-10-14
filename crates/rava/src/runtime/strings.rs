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
    let string_class = vm().wk.string.get().copied().unwrap_or_else(|| crate::loader::boot_class!("java/lang/String").unwrap());
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
        obj::write_ref(s, value_offset(), value);
        obj::write::<u8>(s, coder_offset(), if latin1 { LATIN1 } else { UTF16 });
    }
    Ok(s)
}

/// A compact string's contents, borrowed while its backing array cannot move.
#[derive(Clone, Copy)]
pub(crate) struct StringView<'a> {
    bytes: &'a [u8],
    latin1: bool,
}

impl<'a> StringView<'a> {
    pub(crate) fn len(self) -> usize {
        self.bytes.len() / self.width()
    }

    fn width(self) -> usize {
        if self.latin1 { 1 } else { 2 }
    }

    /// Java UTF-16 code units, including unpaired surrogates, without a temporary vector.
    pub(crate) fn units(self) -> impl ExactSizeIterator<Item = u16> + 'a {
        self.bytes.chunks_exact(self.width()).map(move |p| {
            if self.latin1 { p[0] as u16 } else { u16::from_le_bytes([p[0], p[1]]) }
        })
    }

    pub(crate) fn region(self, start: usize, len: usize) -> Option<Self> {
        let from = start.checked_mul(self.width())?;
        let to = start.checked_add(len)?.checked_mul(self.width())?;
        Some(Self { bytes: self.bytes.get(from..to)?, latin1: self.latin1 })
    }
}

/// Borrows the compact contents of a live `java.lang.String`.
///
/// # Safety
/// `s` must be a live String. Its backing array must stay alive and unmoved for `'a`:
/// the caller must stay in the VM and must not poll, allocate Java objects or call Java
/// while the view is used.
pub(crate) unsafe fn string_view<'a>(s: Oop) -> StringView<'a> {
    let value = obj::read_ref(s, value_offset());
    StringView { bytes: obj::array_slice::<u8>(value), latin1: obj::read::<u8>(s, coder_offset()) == LATIN1 }
}

/// The UTF-16 content of a `java.lang.String`.
pub fn string_units(s: Oop) -> Vec<u16> {
    // SAFETY: callers read a live String in the VM; collecting into a Rust vector does not
    // poll or allocate Java objects.
    unsafe { string_view(s) }.units().collect()
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
    let winner = *vm().strings.lock().unwrap().entry(units).or_insert(s as usize) as Oop;
    if winner == s {
        crate::gc::recent::note(s);
    }
    winner
}

#[cfg(test)]
mod tests {
    use super::StringView;

    #[test]
    fn compact_views_preserve_code_units_and_check_regions() {
        let latin1 = StringView { bytes: &[0, b'A', 0x80, 0xff], latin1: true };
        assert_eq!(latin1.len(), 4);
        assert_eq!(latin1.units().collect::<Vec<_>>(), [0, 0x41, 0x80, 0xff]);
        assert_eq!(latin1.region(1, 2).unwrap().units().collect::<Vec<_>>(), [0x41, 0x80]);
        let utf16 = StringView { bytes: &[0, 0, 0, 8, 0, 0xd8, 0xff, 0xdf], latin1: false };
        assert_eq!(utf16.len(), 4);
        assert_eq!(utf16.units().collect::<Vec<_>>(), [0, 0x800, 0xd800, 0xdfff]);
        assert_eq!(utf16.region(2, 2).unwrap().units().collect::<Vec<_>>(), [0xd800, 0xdfff]);
        for view in [latin1, utf16] {
            assert_eq!(view.region(view.len(), 0).unwrap().len(), 0);
            assert!(view.region(view.len() + 1, 0).is_none());
            assert!(view.region(view.len(), 1).is_none());
            assert!(view.region(usize::MAX, 1).is_none());
        }
    }
}
