//! Reading a native's argument slots in order (a `long` takes two), and the exceptions the
//! Windows file-system natives throw.

use crate::heap::Oop;
use crate::runtime as rt;
use crate::thread::{JResult, Thread, Throw};

pub(super) struct Args<'a> {
    slots: &'a [u64],
    at: usize,
}

impl<'a> Args<'a> {
    pub fn new(slots: &'a [u64]) -> Self {
        Args { slots, at: 0 }
    }

    pub fn long(&mut self) -> i64 {
        self.at += 2;
        self.slots[self.at - 2] as i64
    }

    pub fn int(&mut self) -> i32 {
        self.at += 1;
        self.slots[self.at - 1] as i32
    }

    pub fn boolean(&mut self) -> bool {
        self.int() != 0
    }

    /// A reference.
    pub fn object(&mut self) -> Oop {
        self.at += 1;
        self.slots[self.at - 1] as Oop
    }

    /// A `long` holding an address.
    pub fn ptr<T>(&mut self) -> *mut T {
        self.long() as usize as *mut T
    }
}

/// `sun.nio.fs.WindowsException(code)`.
pub(super) fn windows_error(t: &mut Thread, code: u32) -> Throw {
    match crate::boot::construct(t, "sun/nio/fs/WindowsException", "(I)V", &[code as u64]) {
        Ok(e) => Throw(e),
        Err(e) => e,
    }
}

/// The calling thread's last error as a `WindowsException`.
pub(super) fn last_error(t: &mut Thread) -> Throw {
    // SAFETY: a plain query.
    windows_error(t, unsafe { windows_sys::Win32::Foundation::GetLastError() })
}

/// Fails with the last error unless `ok`.
pub(super) fn check(t: &mut Thread, ok: bool) -> JResult<()> {
    if ok {
        Ok(())
    } else {
        Err(last_error(t))
    }
}

/// A Java string from UTF-16 code units up to a NUL (or the end).
pub(super) fn string(t: &mut Thread, w: &[u16]) -> JResult<Oop> {
    let n = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    rt::new_string(t, &String::from_utf16_lossy(&w[..n]))
}

/// A Java string from a NUL-terminated UTF-16 string in native memory (null for a null pointer).
pub(super) fn string_at(t: &mut Thread, p: *const u16) -> JResult<Oop> {
    if p.is_null() {
        return Ok(std::ptr::null_mut());
    }
    // SAFETY: the caller passes a NUL-terminated wide string.
    let w = unsafe {
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        std::slice::from_raw_parts(p, n)
    };
    string(t, w)
}
