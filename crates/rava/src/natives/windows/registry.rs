//! Registry natives: `java.util.prefs.WindowsPreferences` (the ANSI calls of the JDK's
//! `WindowsPreferences.c`, with names Java already escaped to ASCII and NUL-ended) and
//! `sun.nio.fs.RegistryFileTypeDetector` (a content type from `HKEY_CLASSES_ROOT`).

use crate::heap::{obj, Oop};
use crate::natives::{byte_array, ret_int, ret_ref, NativeRegistry};
use crate::runtime as rt;
use crate::thread::{JResult, Thread};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExA, RegDeleteKeyA, RegDeleteValueA, RegEnumKeyExA, RegEnumValueA, RegFlushKey, RegOpenKeyExA, RegOpenKeyExW, RegQueryInfoKeyA, RegQueryValueExA, RegQueryValueExW, RegSetValueExA, HKEY,
    HKEY_CLASSES_ROOT, KEY_READ, REG_OPTION_NON_VOLATILE, REG_SZ,
};

const P: &str = "java/util/prefs/WindowsPreferences";

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(P, "WindowsRegOpenKey", "(J[BI)[J", |t, a| {
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: a NUL-ended name from Java and an out handle.
        let rc = unsafe { RegOpenKeyExA(key(a[0]), bytes(a[2]).as_ptr(), 0, a[3] as u32, &mut h) };
        ret_ref(longs(t, &[h as i64, rc as i64])?)
    });
    r.add(P, "WindowsRegCloseKey", "(J)I", |_, a| {
        // SAFETY: a key this class opened.
        ret_int(unsafe { RegCloseKey(key(a[0])) } as i32)
    });
    r.add(P, "WindowsRegCreateKeyEx", "(J[B)[J", |t, a| {
        let (mut h, mut disposition): (HKEY, u32) = (std::ptr::null_mut(), 0);
        // SAFETY: a NUL-ended name from Java and out values.
        let rc = unsafe { RegCreateKeyExA(key(a[0]), bytes(a[2]).as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_READ, std::ptr::null(), &mut h, &mut disposition) };
        ret_ref(longs(t, &[h as i64, rc as i64, disposition as i64])?)
    });
    r.add(P, "WindowsRegDeleteKey", "(J[B)I", |_, a| {
        // SAFETY: a NUL-ended name from Java.
        ret_int(unsafe { RegDeleteKeyA(key(a[0]), bytes(a[2]).as_ptr()) } as i32)
    });
    r.add(P, "WindowsRegFlushKey", "(J)I", |_, a| {
        // SAFETY: a key this class opened.
        ret_int(unsafe { RegFlushKey(key(a[0])) } as i32)
    });
    // A string value's bytes, NUL included; null when absent or not a string.
    r.add(P, "WindowsRegQueryValueEx", "(J[B)[B", |t, a| {
        let (hk, name) = (key(a[0]), bytes(a[2]));
        let (mut kind, mut size) = (0u32, 0u32);
        // SAFETY: a size query, then a read into a buffer of that size.
        if unsafe { RegQueryValueExA(hk, name.as_ptr(), std::ptr::null(), &mut kind, std::ptr::null_mut(), &mut size) } != 0 {
            return ret_ref(std::ptr::null_mut());
        }
        let mut buf = vec![0u8; size as usize];
        if unsafe { RegQueryValueExA(hk, name.as_ptr(), std::ptr::null(), &mut kind, buf.as_mut_ptr(), &mut size) } != 0 || kind != REG_SZ {
            return ret_ref(std::ptr::null_mut());
        }
        ret_ref(byte_array(t, Some(&buf[..size as usize]))?)
    });
    r.add(P, "WindowsRegSetValueEx", "(J[B[B)I", |_, a| {
        let data = bytes(a[3]);
        // SAFETY: a NUL-ended name and the value's bytes, NUL included, from Java.
        ret_int(unsafe { RegSetValueExA(key(a[0]), bytes(a[2]).as_ptr(), 0, REG_SZ, data.as_ptr(), data.len() as u32) } as i32)
    });
    r.add(P, "WindowsRegDeleteValue", "(J[B)I", |_, a| {
        // SAFETY: a NUL-ended name from Java.
        ret_int(unsafe { RegDeleteValueA(key(a[0]), bytes(a[2]).as_ptr()) } as i32)
    });
    // { subkeys, error, values, longest subkey name, longest value name }.
    r.add(P, "WindowsRegQueryInfoKey", "(J)[J", |t, a| {
        let (mut subkeys, mut max_subkey, mut values, mut max_value) = (0u32, 0u32, 0u32, 0u32);
        let null = std::ptr::null_mut::<u32>();
        // SAFETY: out counts; the rest not asked for.
        let rc = unsafe { RegQueryInfoKeyA(key(a[0]), null.cast(), null.cast(), std::ptr::null(), &mut subkeys, &mut max_subkey, null.cast(), &mut values, &mut max_value, null.cast(), null.cast(), null.cast()) };
        ret_ref(longs(t, &[subkeys as i64, rc as i64, values as i64, max_subkey as i64, max_value as i64])?)
    });
    // The name at an index, NUL included; null past the end or on an error.
    r.add(P, "WindowsRegEnumKeyEx", "(JII)[B", |t, a| {
        enum_name(t, a[3] as i32, |buf, size| unsafe { RegEnumKeyExA(key(a[0]), a[2] as u32, buf, size, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) })
    });
    r.add(P, "WindowsRegEnumValue", "(JII)[B", |t, a| {
        enum_name(t, a[3] as i32, |buf, size| unsafe { RegEnumValueA(key(a[0]), a[2] as u32, buf, size, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) })
    });
    // The string `name` of `HKEY_CLASSES_ROOT\key`, both wide strings in native memory; null
    // when absent, not a string, or longer than the JDK's 255-byte buffer.
    r.add("sun/nio/fs/RegistryFileTypeDetector", "queryStringValue", "(JJ)Ljava/lang/String;", |t, a| {
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: NUL-ended wide strings the caller allocated.
        if unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, a[0] as *const u16, 0, KEY_READ, &mut h) } != 0 {
            return ret_ref(std::ptr::null_mut());
        }
        let mut data = [0u16; 128];
        let (mut kind, mut size) = (0u32, 255u32);
        // SAFETY: a read into `data`, of at most `size` bytes.
        let rc = unsafe { RegQueryValueExW(h, a[2] as *const u16, std::ptr::null(), &mut kind, data.as_mut_ptr().cast(), &mut size) };
        // SAFETY: the key just opened.
        unsafe { RegCloseKey(h) };
        if rc != 0 || kind != REG_SZ {
            return ret_ref(std::ptr::null_mut());
        }
        let chars = &data[..(size as usize / 2).min(data.len())];
        let text = String::from_utf16_lossy(&chars[..chars.iter().position(|&c| c == 0).unwrap_or(chars.len())]);
        ret_ref(rt::new_string(t, &text)?)
    });
}

/// A key handle as Java holds it: a root's sign-extended constant, or an opened key.
fn key(v: u64) -> HKEY {
    v as usize as HKEY
}

/// A Java `byte[]`'s bytes (names arrive NUL-ended).
fn bytes<'a>(arr: u64) -> &'a [u8] {
    // SAFETY: a live byte[] the caller holds for the call.
    unsafe { obj::array_slice::<u8>(arr as Oop) }
}

fn longs(t: &mut Thread, values: &[i64]) -> JResult<Oop> {
    let class = crate::vm::vm().primitive_array_class(crate::types::BasicType::Long).expect("[J");
    let arr = rt::alloc_array(t, class, values.len() as i32)?;
    // SAFETY: a fresh long[] of the same length.
    unsafe { obj::array_slice::<i64>(arr).copy_from_slice(values) };
    Ok(arr)
}

/// An enumerated name into a buffer of `max` bytes, returned with its NUL as the JDK does.
fn enum_name(t: &mut Thread, max: i32, call: impl FnOnce(*mut u8, *mut u32) -> u32) -> JResult<u64> {
    let mut buf = vec![0u8; max.max(1) as usize + 1];
    let mut size = max.max(0) as u32;
    if call(buf.as_mut_ptr(), &mut size) != 0 {
        return ret_ref(std::ptr::null_mut());
    }
    ret_ref(byte_array(t, Some(&buf[..size as usize + 1]))?)
}
