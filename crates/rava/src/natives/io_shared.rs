//! What the `java.io` natives of every system share: Java's checks on a byte range, and the
//! system's last error as Java's exceptions word it.

use crate::heap::{obj, Oop};
use crate::runtime::{self as rt, throw_new};
use crate::thread::{JResult, Thread, Throw};

/// The `[off, off+len)` byte range of a Java `byte[]`, with Java's exceptions.
pub(crate) fn checked_range<'a>(t: &mut Thread, arr: Oop, off: i32, len: i32) -> JResult<&'a mut [u8]> {
    if arr.is_null() {
        return Err(rt::null_pointer(t));
    }
    // SAFETY: byte[] array.
    let data = unsafe { obj::array_slice::<u8>(arr) };
    if off < 0 || len < 0 || off as usize + len as usize > data.len() {
        return Err(throw_new(t, "java/lang/IndexOutOfBoundsException", ""));
    }
    Ok(&mut data[off as usize..off as usize + len as usize])
}

pub(crate) fn errno_text() -> String {
    std::io::Error::last_os_error().to_string().split(" (os error").next().unwrap_or("").to_string()
}

pub(crate) fn io_error(t: &mut Thread, what: &str) -> Throw {
    let msg = format!("{what}: {}", errno_text());
    throw_new(t, "java/io/IOException", &msg)
}

