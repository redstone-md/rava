//! I/O completion ports as the JDK's `Iocp.c` uses them for asynchronous channels
//! (`sun.nio.ch.Iocp`), and the overlapped file operations of `WindowsAsynchronousFileChannelImpl`:
//! an operation starts with its `OVERLAPPED` and completes on the port the handle is bound to.

use super::args::Args;
use crate::heap::Oop;
use crate::natives::fields::{set_int, set_long};
use crate::natives::io_shared::io_error;
use crate::natives::{ret_int, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::runtime as rt;
use crate::thread::{JResult, Thread};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_HANDLE_EOF, ERROR_IO_PENDING, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{LockFileEx, ReadFile, WriteFile, LOCKFILE_EXCLUSIVE_LOCK};
use windows_sys::Win32::System::Diagnostics::Debug::{FormatMessageW, FORMAT_MESSAGE_FROM_SYSTEM};
use windows_sys::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, PostQueuedCompletionStatus, OVERLAPPED};
use windows_sys::Win32::System::Threading::INFINITE;

const IOCP: &str = "sun/nio/ch/Iocp";
const STATUS: &str = "sun/nio/ch/Iocp$CompletionStatus";
const FILE: &str = "sun/nio/ch/WindowsAsynchronousFileChannelImpl";
/// `IOStatus` values.
pub(super) const IOS_EOF: i32 = -1;
pub(super) const IOS_UNAVAILABLE: i32 = -2;

/// Sets an operation's position in its `OVERLAPPED` (no event: completion goes to the port).
fn at(ov: *mut OVERLAPPED, offset: i64) -> *mut OVERLAPPED {
    // SAFETY: the OVERLAPPED the class library allocated for this operation.
    unsafe {
        (*ov).Anonymous.Anonymous.Offset = offset as u32;
        (*ov).Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
        (*ov).hEvent = std::ptr::null_mut();
    }
    ov
}

/// An overlapped call's result: started (completion comes through the port), or an exception.
fn started(t: &mut Thread, ok: bool, what: &str, eof: bool) -> JResult<u64> {
    if ok {
        return ret_int(IOS_UNAVAILABLE);
    }
    // SAFETY: a plain query.
    match unsafe { GetLastError() } {
        ERROR_IO_PENDING => ret_int(IOS_UNAVAILABLE),
        ERROR_HANDLE_EOF if eof => ret_int(IOS_EOF),
        _ => Err(io_error(t, what)),
    }
}

/// `getQueuedCompletionStatus(port, status)`: waits for the next completion outside Java.
fn queued(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let port = a[0] as usize;
    let status = a[2] as Oop;
    let (mut bytes, mut key, mut ov) = (0u32, 0usize, 0usize);
    let (ok, error) = crate::gc::safepoint::blocking(t, || {
        // SAFETY: the port and out-parameters live across the wait.
        let ok = unsafe { GetQueuedCompletionStatus(port as HANDLE, &mut bytes, &mut key, &mut ov as *mut usize as *mut *mut OVERLAPPED, INFINITE) } != 0;
        (ok, if ok { 0 } else { unsafe { GetLastError() } })
    });
    if !ok && ov == 0 {
        return Err(io_error(t, "GetQueuedCompletionStatus failed"));
    }
    set_int(status, STATUS, "error", error as i32);
    set_int(status, STATUS, "bytesTransferred", bytes as i32);
    set_int(status, STATUS, "completionKey", key as i32);
    set_long(status, STATUS, "overlapped", ov as i64);
    ret_void()
}

/// The system's message for `code`, without the final period and line end.
fn message(t: &mut Thread, code: u32) -> JResult<u64> {
    let mut buf = [0u16; 255];
    // SAFETY: a buffer of 255 characters.
    let n = unsafe { FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM, std::ptr::null(), code, 0, buf.as_mut_ptr(), 255, std::ptr::null()) } as usize;
    if n == 0 {
        return ret_ref(std::ptr::null_mut());
    }
    let mut text = String::from_utf16_lossy(&buf[..n]);
    if n > 3 {
        for end in ['\n', '\r', '.'] {
            if text.ends_with(end) {
                text.pop();
            }
        }
    }
    ret_ref(rt::new_string(t, &text)?)
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(IOCP, "initIDs", "()V", |_, _| ret_void());
    r.add(IOCP, "createIoCompletionPort", "(JJII)J", |t, a| {
        let mut a = Args::new(a);
        let (handle, existing, key, concurrency) = (a.ptr::<core::ffi::c_void>(), a.ptr::<core::ffi::c_void>(), a.int(), a.int());
        // SAFETY: binds a handle to a port, or makes a port when the handle is invalid.
        let port = unsafe { CreateIoCompletionPort(handle, existing, key as u32 as usize, concurrency as u32) };
        if port.is_null() {
            return Err(io_error(t, "CreateIoCompletionPort failed"));
        }
        ret_long(port as i64)
    });
    r.add(IOCP, "close0", "(J)V", |_, a| {
        // SAFETY: a port the class library made.
        unsafe { CloseHandle(a[0] as HANDLE) };
        ret_void()
    });
    r.add(IOCP, "getQueuedCompletionStatus", "(JLsun/nio/ch/Iocp$CompletionStatus;)V", queued);
    r.add(IOCP, "postQueuedCompletionStatus", "(JI)V", |t, a| {
        // SAFETY: wakes a waiter on the port with the key.
        if unsafe { PostQueuedCompletionStatus(a[0] as HANDLE, 0, a[2] as u32 as usize, std::ptr::null()) } == 0 {
            return Err(io_error(t, "PostQueuedCompletionStatus"));
        }
        ret_void()
    });
    r.add(IOCP, "getErrorMessage", "(I)Ljava/lang/String;", |t, a| message(t, a[0] as u32));
    // readFile / writeFile(handle, address, len, offset, overlapped)
    r.add(FILE, "readFile", "(JJIJJ)I", |t, a| {
        let mut a = Args::new(a);
        let (h, buf, len, offset, ov) = (a.ptr::<core::ffi::c_void>(), a.ptr::<u8>(), a.int(), a.long(), a.ptr::<OVERLAPPED>());
        // SAFETY: the caller's buffer of `len` bytes, kept alive until the completion.
        let ok = unsafe { ReadFile(h, buf, len as u32, std::ptr::null_mut(), at(ov, offset)) } != 0;
        started(t, ok, "ReadFile failed", true)
    });
    r.add(FILE, "writeFile", "(JJIJJ)I", |t, a| {
        let mut a = Args::new(a);
        let (h, buf, len, offset, ov) = (a.ptr::<core::ffi::c_void>(), a.ptr::<u8>(), a.int(), a.long(), a.ptr::<OVERLAPPED>());
        // SAFETY: as for readFile.
        let ok = unsafe { WriteFile(h, buf, len as u32, std::ptr::null_mut(), at(ov, offset)) } != 0;
        started(t, ok, "WriteFile failed", false)
    });
    // lockFile(handle, position, size, shared, overlapped): 0 when locked at once.
    r.add(FILE, "lockFile", "(JJJZJ)I", |t, a| {
        let mut a = Args::new(a);
        let (h, pos, size, shared, ov) = (a.ptr::<core::ffi::c_void>(), a.long(), a.long(), a.boolean(), a.ptr::<OVERLAPPED>());
        let flags = if shared { 0 } else { LOCKFILE_EXCLUSIVE_LOCK };
        // SAFETY: a file handle and the operation's OVERLAPPED.
        if unsafe { LockFileEx(h, flags, 0, size as u32, (size >> 32) as u32, at(ov, pos)) } != 0 {
            return ret_int(0);
        }
        started(t, false, "LockFile failed", false)
    });
}
