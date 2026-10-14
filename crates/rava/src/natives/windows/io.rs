//! `java.io` over Windows handles, as the JDK's `io_util_md.c` for Windows: a `FileDescriptor`
//! holds a `HANDLE` in its `handle` field (the standard streams' come from `GetStdHandle`), files
//! open with `CreateFileW` and transfer with `ReadFile`/`WriteFile`.

use crate::heap::{obj, Oop};
use crate::natives::io_shared::{checked_range, io_error};
use crate::natives::{ret_bool, ret_int, ret_long, ret_void, NativeRegistry};
use crate::runtime::{self as rt, throw_new, vm_field};
use crate::thread::{JResult, Thread};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, GetFileSizeEx, GetFileType, ReadFile, SetEndOfFile, SetFilePointerEx, WriteFile, CREATE_ALWAYS, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL,
    FILE_BEGIN, FILE_CURRENT, FILE_END, FILE_FLAG_WRITE_THROUGH, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
    FILE_TYPE_PIPE, OPEN_ALWAYS, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;

vm_field!(fd_handle, "java/io/FileDescriptor", "handle", "J");
vm_field!(fd_int, "java/io/FileDescriptor", "fd", "I");
vm_field!(fd_append, "java/io/FileDescriptor", "append", "Z");
vm_field!(fis_fd, "java/io/FileInputStream", "fd", "Ljava/io/FileDescriptor;");
vm_field!(fos_fd, "java/io/FileOutputStream", "fd", "Ljava/io/FileDescriptor;");
vm_field!(raf_fd, "java/io/RandomAccessFile", "fd", "Ljava/io/FileDescriptor;");

const FD: &str = "java/io/FileDescriptor";
const FIS: &str = "java/io/FileInputStream";
const FOS: &str = "java/io/FileOutputStream";
const RAF: &str = "java/io/RandomAccessFile";

/// The handle in a stream's `FileDescriptor` (-1 when closed).
fn stream_handle(stream: Oop, field: u32) -> HANDLE {
    // SAFETY: a live stream with a FileDescriptor.
    unsafe {
        let fdo = obj::read_ref(stream, field);
        (if fdo.is_null() { -1 } else { obj::read::<i64>(fdo, fd_handle()) }) as HANDLE
    }
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Opens `path` into the stream's `FileDescriptor`, or throws `FileNotFoundException`.
fn open_into(t: &mut Thread, stream: Oop, field: u32, path: Oop, access: u32, disposition: u32, flags: u32) -> JResult<u64> {
    let name = rt::java_string_to_rust(path);
    let w = wide(&name);
    // SAFETY: a NUL-terminated wide path.
    let h = unsafe {
        CreateFileW(w.as_ptr(), access, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, std::ptr::null(), disposition, FILE_ATTRIBUTE_NORMAL | flags, std::ptr::null_mut())
    };
    if h == INVALID_HANDLE_VALUE {
        let msg = format!("{name} ({})", crate::natives::io_shared::errno_text());
        return Err(throw_new(t, "java/io/FileNotFoundException", &msg));
    }
    // SAFETY: the stream's FileDescriptor, made by its constructor. Its `append` is what the
    // stream passes to each write, which then goes to the end (the JDK's `fileOpen` sets it:
    // with write access a handle opened for appending still writes where its position is).
    unsafe {
        let fd = obj::read_ref(stream, field);
        obj::write::<i64>(fd, fd_handle(), h as i64);
        obj::write::<bool>(fd, fd_append(), access & FILE_APPEND_DATA != 0);
    }
    ret_void()
}

/// Reads into `buf`: the count, or -1 at the end (a closed pipe ends a stream, as in the JDK).
fn read(t: &mut Thread, h: HANDLE, buf: &mut [u8]) -> JResult<u64> {
    if buf.is_empty() {
        return ret_int(0);
    }
    let (ptr, len, hv) = (buf.as_mut_ptr() as usize, buf.len() as u32, h as usize);
    let mut n = 0u32;
    let np = &mut n as *mut u32 as usize;
    // Reads block (a console, a pipe): the thread is outside Java meanwhile; the array is held
    // by the calling frame.
    let ok = crate::gc::safepoint::blocking(t, || unsafe { ReadFile(hv as HANDLE, ptr as *mut u8, len, np as *mut u32, std::ptr::null_mut()) });
    if ok == 0 {
        // SAFETY: a plain query.
        return match unsafe { GetLastError() } {
            ERROR_BROKEN_PIPE | ERROR_HANDLE_EOF => ret_int(-1),
            _ => Err(io_error(t, "Read error")),
        };
    }
    ret_int(if n == 0 { -1 } else { n as i32 })
}

fn write_all(t: &mut Thread, h: HANDLE, append: bool, mut bytes: &[u8]) -> JResult<()> {
    if append {
        // SAFETY: moves the handle's own position.
        unsafe { SetFilePointerEx(h, 0, std::ptr::null_mut(), FILE_END) };
    }
    while !bytes.is_empty() {
        let mut n = 0u32;
        // SAFETY: a valid byte range.
        if unsafe { WriteFile(h, bytes.as_ptr(), bytes.len() as u32, &mut n, std::ptr::null_mut()) } == 0 {
            return Err(io_error(t, "Write error"));
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

fn seek(h: HANDLE, distance: i64, method: u32) -> Option<i64> {
    let mut at = 0i64;
    // SAFETY: moves the handle's own position.
    (unsafe { SetFilePointerEx(h, distance, &mut at, method) } != 0).then_some(at)
}

fn size(h: HANDLE) -> Option<i64> {
    let mut s = 0i64;
    // SAFETY: fills an i64.
    (unsafe { GetFileSizeEx(h, &mut s) } != 0).then_some(s)
}

/// Bytes readable without blocking: the rest of a file, or what a pipe holds.
fn available(h: HANDLE) -> i32 {
    // SAFETY: queries on an open handle.
    unsafe {
        match GetFileType(h) {
            FILE_TYPE_DISK => match (size(h), seek(h, 0, FILE_CURRENT)) {
                (Some(s), Some(p)) => (s - p).clamp(0, i32::MAX as i64) as i32,
                _ => 0,
            },
            FILE_TYPE_PIPE => {
                let mut n = 0u32;
                PeekNamedPipe(h, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut n, std::ptr::null_mut());
                n as i32
            }
            _ => 0,
        }
    }
}

pub(super) fn register(r: &mut NativeRegistry) {
    for c in [FD, FIS, FOS, RAF] {
        r.add(c, "initIDs", "()V", |_, _| ret_void());
    }
    r.add(FD, "getHandle", "(I)J", |_, a| {
        let which = match a[0] as i32 {
            0 => STD_INPUT_HANDLE,
            1 => STD_OUTPUT_HANDLE,
            2 => STD_ERROR_HANDLE,
            _ => return ret_long(-1),
        };
        // SAFETY: a plain query.
        ret_long(unsafe { GetStdHandle(which) } as i64)
    });
    r.add(FD, "getAppend", "(I)Z", |_, _| ret_bool(false));
    r.add(FD, "close0", "()V", |t, a| {
        let fdo = a[0] as Oop;
        // SAFETY: a live FileDescriptor.
        let (h, fd) = unsafe { (obj::read::<i64>(fdo, fd_handle()), obj::read::<i32>(fdo, fd_int())) };
        if h == -1 {
            return ret_void();
        }
        // SAFETY: as above.
        unsafe {
            obj::write::<i64>(fdo, fd_handle(), -1);
            obj::write::<i32>(fdo, fd_int(), -1);
        }
        // The standard streams' handles stay open, as the JDK leaves them.
        if !(0..=2).contains(&fd) && unsafe { CloseHandle(h as HANDLE) } == 0 {
            return Err(io_error(t, "close failed"));
        }
        ret_void()
    });
    r.add(FD, "sync0", "()V", |_, a| {
        // SAFETY: a live FileDescriptor's handle.
        unsafe { FlushFileBuffers(obj::read::<i64>(a[0] as Oop, fd_handle()) as HANDLE) };
        ret_void()
    });
    r.add(FOS, "open0", "(Ljava/lang/String;Z)V", |t, a| {
        let (access, disposition) = if a[2] != 0 { (FILE_APPEND_DATA | FILE_GENERIC_WRITE, OPEN_ALWAYS) } else { (FILE_GENERIC_WRITE, CREATE_ALWAYS) };
        open_into(t, a[0] as Oop, fos_fd(), a[1] as Oop, access, disposition, 0)
    });
    r.add(FOS, "writeBytes", "([BIIZ)V", |t, a| {
        let h = stream_handle(a[0] as Oop, fos_fd());
        let bytes = checked_range(t, a[1] as Oop, a[2] as i32, a[3] as i32)?;
        write_all(t, h, a[4] != 0, bytes)?;
        ret_void()
    });
    r.add(FOS, "write", "(IZ)V", |t, a| {
        write_all(t, stream_handle(a[0] as Oop, fos_fd()), a[2] != 0, &[a[1] as u8])?;
        ret_void()
    });
    r.add(FIS, "open0", "(Ljava/lang/String;)V", |t, a| open_into(t, a[0] as Oop, fis_fd(), a[1] as Oop, FILE_GENERIC_READ, OPEN_EXISTING, 0));
    r.add(FIS, "readBytes", "([BII)I", |t, a| {
        let h = stream_handle(a[0] as Oop, fis_fd());
        let buf = checked_range(t, a[1] as Oop, a[2] as i32, a[3] as i32)?;
        read(t, h, buf)
    });
    r.add(FIS, "read0", "()I", |t, a| {
        let mut b = [0u8];
        match read(t, stream_handle(a[0] as Oop, fis_fd()), &mut b)? as i32 {
            -1 => ret_int(-1),
            _ => ret_int(b[0] as i32),
        }
    });
    r.add(FIS, "available0", "()I", |_, a| ret_int(available(stream_handle(a[0] as Oop, fis_fd()))));
    r.add(FIS, "length0", "()J", |t, a| size(stream_handle(a[0] as Oop, fis_fd())).map_or_else(|| Err(io_error(t, "GetFileSizeEx failed")), ret_long));
    r.add(FIS, "position0", "()J", |t, a| seek(stream_handle(a[0] as Oop, fis_fd()), 0, FILE_CURRENT).map_or_else(|| Err(io_error(t, "Seek failed")), ret_long));
    r.add(FIS, "skip0", "(J)J", |t, a| {
        let h = stream_handle(a[0] as Oop, fis_fd());
        match (seek(h, 0, FILE_CURRENT), seek(h, a[1] as i64, FILE_CURRENT)) {
            (Some(from), Some(to)) => ret_long(to - from),
            _ => Err(io_error(t, "Seek error")),
        }
    });
    r.add(FIS, "isRegularFile0", "(Ljava/io/FileDescriptor;)Z", |_, a| {
        // SAFETY: a live FileDescriptor's handle.
        ret_bool(unsafe { GetFileType(obj::read::<i64>(a[1] as Oop, fd_handle()) as HANDLE) } == FILE_TYPE_DISK)
    });
    register_raf(r);
}

fn register_raf(r: &mut NativeRegistry) {
    // Modes from RandomAccessFile: O_RDONLY 1, O_RDWR 2, O_SYNC 4, O_DSYNC 8.
    r.add(RAF, "open0", "(Ljava/lang/String;I)V", |t, a| {
        let mode = a[2] as i32;
        let (access, disposition) = if mode & 2 != 0 { (FILE_GENERIC_READ | FILE_GENERIC_WRITE, OPEN_ALWAYS) } else { (FILE_GENERIC_READ, OPEN_EXISTING) };
        let flags = if mode & 12 != 0 { FILE_FLAG_WRITE_THROUGH } else { 0 };
        open_into(t, a[0] as Oop, raf_fd(), a[1] as Oop, access, disposition, flags)
    });
    r.add(RAF, "read0", "()I", |t, a| {
        let mut b = [0u8];
        match read(t, stream_handle(a[0] as Oop, raf_fd()), &mut b)? as i32 {
            -1 => ret_int(-1),
            _ => ret_int(b[0] as i32),
        }
    });
    r.add(RAF, "readBytes0", "([BII)I", |t, a| {
        let h = stream_handle(a[0] as Oop, raf_fd());
        let buf = checked_range(t, a[1] as Oop, a[2] as i32, a[3] as i32)?;
        read(t, h, buf)
    });
    r.add(RAF, "write0", "(I)V", |t, a| {
        write_all(t, stream_handle(a[0] as Oop, raf_fd()), false, &[a[1] as u8])?;
        ret_void()
    });
    r.add(RAF, "writeBytes0", "([BII)V", |t, a| {
        let bytes = checked_range(t, a[1] as Oop, a[2] as i32, a[3] as i32)?;
        write_all(t, stream_handle(a[0] as Oop, raf_fd()), false, bytes)?;
        ret_void()
    });
    r.add(RAF, "getFilePointer", "()J", |t, a| seek(stream_handle(a[0] as Oop, raf_fd()), 0, FILE_CURRENT).map_or_else(|| Err(io_error(t, "Seek failed")), ret_long));
    r.add(RAF, "seek0", "(J)V", |t, a| match seek(stream_handle(a[0] as Oop, raf_fd()), a[1] as i64, FILE_BEGIN) {
        Some(_) => ret_void(),
        None => Err(io_error(t, "Seek failed")),
    });
    r.add(RAF, "length0", "()J", |t, a| size(stream_handle(a[0] as Oop, raf_fd())).map_or_else(|| Err(io_error(t, "GetFileSizeEx failed")), ret_long));
    r.add(RAF, "setLength0", "(J)V", |t, a| {
        let h = stream_handle(a[0] as Oop, raf_fd());
        let at = seek(h, 0, FILE_CURRENT);
        // SAFETY: an open handle; the position is restored (clamped to the new end).
        let ok = seek(h, a[1] as i64, FILE_BEGIN).is_some() && unsafe { SetEndOfFile(h) } != 0;
        if let Some(at) = at {
            seek(h, at.min(a[1] as i64), FILE_BEGIN);
        }
        if !ok {
            return Err(io_error(t, "setLength failed"));
        }
        ret_void()
    });
}
