//! `sun.nio.fs.WindowsNativeDispatcher`, the file side, as the JDK's `WindowsNativeDispatcher.c`:
//! paths arrive as addresses of NUL-terminated UTF-16 strings, results go into the objects or
//! native buffers the caller passes, and failures throw `WindowsException(GetLastError())`.
//! The security side is in `security`.

use super::args::{check, last_error, string, windows_error, Args};
use crate::heap::Oop;
use crate::natives::fields::{set_int, set_long, set_string};
use crate::natives::{ret_int, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::thread::{JResult, Thread};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_NO_MORE_FILES, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::CreateEventW;
use windows_sys::Win32::System::IO::{CancelIo, CreateIoCompletionPort, DeviceIoControl, GetOverlappedResult, GetQueuedCompletionStatus, PostQueuedCompletionStatus, OVERLAPPED};

const D: &str = "sun/nio/fs/WindowsNativeDispatcher";
const FIRST_FILE: &str = "sun/nio/fs/WindowsNativeDispatcher$FirstFile";
const FIRST_STREAM: &str = "sun/nio/fs/WindowsNativeDispatcher$FirstStream";
const VOLUME: &str = "sun/nio/fs/WindowsNativeDispatcher$VolumeInformation";
const SPACE: &str = "sun/nio/fs/WindowsNativeDispatcher$DiskFreeSpace";
const COMPLETION: &str = "sun/nio/fs/WindowsNativeDispatcher$CompletionStatus";
/// `FSCTL_GET_REPARSE_POINT` and `FSCTL_SET_SPARSE` (winioctl.h).
const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00a8;
const FSCTL_SET_SPARSE: u32 = 0x0009_00c4;

fn handle(a: &mut Args) -> HANDLE {
    a.long() as HANDLE
}

/// A FILETIME pointer for a time the caller gives (-1: leave as is).
fn filetime(v: i64, slot: &mut FILETIME) -> *const FILETIME {
    if v == -1 {
        return std::ptr::null();
    }
    *slot = FILETIME { dwLowDateTime: v as u32, dwHighDateTime: (v >> 32) as u32 };
    slot
}

/// A string result of a call that fills a buffer and returns the length it needs.
fn sized_string(t: &mut Thread, mut fill: impl FnMut(*mut u16, u32) -> u32) -> JResult<u64> {
    let mut buf = vec![0u16; 260];
    loop {
        let n = fill(buf.as_mut_ptr(), buf.len() as u32) as usize;
        if n == 0 {
            return Err(last_error(t));
        }
        if n < buf.len() {
            return ret_ref(string(t, &buf[..n])?);
        }
        buf.resize(n + 1, 0);
    }
}

fn files(r: &mut NativeRegistry) {
    r.add(D, "initIDs", "()V", |_, _| ret_void());
    r.add(D, "CreateFile0", "(JIIJII)J", |t, a| {
        let mut a = Args::new(a);
        let (path, access, share, sa, disposition, flags) = (a.ptr::<u16>(), a.int() as u32, a.int() as u32, a.long(), a.int() as u32, a.int() as u32);
        // SAFETY: arguments as the class library passes them.
        let h = unsafe { CreateFileW(path, access, share, sa as usize as *const _, disposition, flags, std::ptr::null_mut()) };
        if h == INVALID_HANDLE_VALUE {
            return Err(last_error(t));
        }
        ret_long(h as i64)
    });
    r.add(D, "CloseHandle", "(J)V", |_, a| {
        // SAFETY: a handle the class library owns.
        unsafe { CloseHandle(a[0] as HANDLE) };
        ret_void()
    });
    r.add(D, "DeleteFile0", "(J)V", |t, a| {
        // SAFETY: a NUL-terminated path.
        check(t, unsafe { DeleteFileW(a[0] as *const u16) } != 0)?;
        ret_void()
    });
    r.add(D, "CreateDirectory0", "(JJ)V", |t, a| {
        // SAFETY: a path and optional security attributes.
        check(t, unsafe { CreateDirectoryW(a[0] as *const u16, a[2] as *const _) } != 0)?;
        ret_void()
    });
    r.add(D, "RemoveDirectory0", "(J)V", |t, a| {
        // SAFETY: a NUL-terminated path.
        check(t, unsafe { RemoveDirectoryW(a[0] as *const u16) } != 0)?;
        ret_void()
    });
    r.add(D, "DeviceIoControlSetSparse", "(J)V", |t, a| {
        let mut n = 0u32;
        // SAFETY: an open file handle.
        check(t, unsafe { DeviceIoControl(a[0] as HANDLE, FSCTL_SET_SPARSE, std::ptr::null(), 0, std::ptr::null_mut(), 0, &mut n, std::ptr::null_mut()) } != 0)?;
        ret_void()
    });
    r.add(D, "DeviceIoControlGetReparsePoint", "(JJI)V", |t, a| {
        let mut a = Args::new(a);
        let (h, buf, len) = (handle(&mut a), a.ptr::<u8>(), a.int() as u32);
        let mut n = 0u32;
        // SAFETY: an open handle and the caller's buffer of `len` bytes.
        check(t, unsafe { DeviceIoControl(h, FSCTL_GET_REPARSE_POINT, std::ptr::null(), 0, buf as *mut _, len, &mut n, std::ptr::null_mut()) } != 0)?;
        ret_void()
    });
    r.add(D, "FindFirstFile0", "(JLsun/nio/fs/WindowsNativeDispatcher$FirstFile;)V", |t, a| {
        // SAFETY: a NUL-terminated pattern; the data is read before the next call.
        let mut fd: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        let h = unsafe { FindFirstFileW(a[0] as *const u16, &mut fd) };
        if h == INVALID_HANDLE_VALUE {
            return Err(last_error(t));
        }
        let o = a[2] as Oop;
        set_long(o, FIRST_FILE, "handle", h as i64);
        let name = string(t, &fd.cFileName)?;
        set_string(o, FIRST_FILE, "name", name);
        set_int(o, FIRST_FILE, "attributes", fd.dwFileAttributes as i32);
        ret_void()
    });
    r.add(D, "FindFirstFile1", "(JJ)J", |t, a| {
        // SAFETY: a NUL-terminated pattern and the caller's WIN32_FIND_DATAW.
        let h = unsafe { FindFirstFileW(a[0] as *const u16, a[2] as *mut WIN32_FIND_DATAW) };
        if h == INVALID_HANDLE_VALUE {
            return Err(last_error(t));
        }
        ret_long(h as i64)
    });
    r.add(D, "FindNextFile0", "(JJ)Ljava/lang/String;", |t, a| {
        let data = a[2] as *mut WIN32_FIND_DATAW;
        // SAFETY: a find handle and the caller's WIN32_FIND_DATAW.
        if unsafe { FindNextFileW(a[0] as HANDLE, data) } != 0 {
            // SAFETY: just filled.
            return ret_ref(string(t, unsafe { &(*data).cFileName })?);
        }
        // SAFETY: a plain query.
        match unsafe { GetLastError() } {
            ERROR_NO_MORE_FILES => ret_ref(std::ptr::null_mut()),
            e => Err(windows_error(t, e)),
        }
    });
    r.add(D, "FindFirstStream0", "(JLsun/nio/fs/WindowsNativeDispatcher$FirstStream;)V", |t, a| {
        // SAFETY: a NUL-terminated path; the data is read before the next call.
        let mut sd: WIN32_FIND_STREAM_DATA = unsafe { std::mem::zeroed() };
        let h = unsafe { FindFirstStreamW(a[0] as *const u16, FindStreamInfoStandard, &mut sd as *mut _ as *mut _, 0) };
        let o = a[2] as Oop;
        if h == INVALID_HANDLE_VALUE {
            // SAFETY: a plain query.
            return match unsafe { GetLastError() } {
                windows_sys::Win32::Foundation::ERROR_HANDLE_EOF => {
                    set_long(o, FIRST_STREAM, "handle", h as i64);
                    ret_void()
                }
                e => Err(windows_error(t, e)),
            };
        }
        set_long(o, FIRST_STREAM, "handle", h as i64);
        let name = string(t, &sd.cStreamName)?;
        set_string(o, FIRST_STREAM, "name", name);
        ret_void()
    });
    r.add(D, "FindNextStream0", "(J)Ljava/lang/String;", |t, a| {
        // SAFETY: a stream-find handle.
        let mut sd: WIN32_FIND_STREAM_DATA = unsafe { std::mem::zeroed() };
        if unsafe { FindNextStreamW(a[0] as HANDLE, &mut sd as *mut _ as *mut _) } != 0 {
            return ret_ref(string(t, &sd.cStreamName)?);
        }
        // SAFETY: a plain query.
        match unsafe { GetLastError() } {
            windows_sys::Win32::Foundation::ERROR_HANDLE_EOF => ret_ref(std::ptr::null_mut()),
            e => Err(windows_error(t, e)),
        }
    });
    r.add(D, "FindClose", "(J)V", |t, a| {
        // SAFETY: a find handle.
        check(t, unsafe { FindClose(a[0] as HANDLE) } != 0)?;
        ret_void()
    });
    r.add(D, "GetFileInformationByHandle0", "(JJ)V", |t, a| {
        // SAFETY: an open handle and the caller's BY_HANDLE_FILE_INFORMATION.
        check(t, unsafe { GetFileInformationByHandle(a[0] as HANDLE, a[2] as *mut _) } != 0)?;
        ret_void()
    });
    r.add(D, "CopyFileEx0", "(JJIJ)V", |t, a| {
        let mut a = Args::new(a);
        let (from, to, flags, cancel) = (a.ptr::<u16>(), a.ptr::<u16>(), a.int() as u32, a.ptr::<i32>());
        // SAFETY: NUL-terminated paths and the caller's cancel flag.
        check(t, unsafe { CopyFileExW(from, to, None, std::ptr::null(), cancel, flags) } != 0)?;
        ret_void()
    });
    r.add(D, "MoveFileEx0", "(JJI)V", |t, a| {
        let mut a = Args::new(a);
        let (from, to, flags) = (a.ptr::<u16>(), a.ptr::<u16>(), a.int() as u32);
        // SAFETY: NUL-terminated paths.
        check(t, unsafe { MoveFileExW(from, to, flags) } != 0)?;
        ret_void()
    });
    r.add(D, "GetFileAttributes0", "(J)I", |t, a| {
        // SAFETY: a NUL-terminated path.
        let v = unsafe { GetFileAttributesW(a[0] as *const u16) };
        if v == INVALID_FILE_ATTRIBUTES {
            return Err(last_error(t));
        }
        ret_int(v as i32)
    });
    r.add(D, "SetFileAttributes0", "(JI)V", |t, a| {
        // SAFETY: a NUL-terminated path.
        check(t, unsafe { SetFileAttributesW(a[0] as *const u16, a[2] as u32) } != 0)?;
        ret_void()
    });
    r.add(D, "GetFileAttributesEx0", "(JJ)V", |t, a| {
        // SAFETY: a path and the caller's WIN32_FILE_ATTRIBUTE_DATA.
        check(t, unsafe { GetFileAttributesExW(a[0] as *const u16, GetFileExInfoStandard, a[2] as *mut _) } != 0)?;
        ret_void()
    });
    r.add(D, "SetFileTime0", "(JJJJ)V", |t, a| {
        let mut a = Args::new(a);
        let (h, c, l, w) = (handle(&mut a), a.long(), a.long(), a.long());
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut cs, mut ls, mut ws) = (z, z, z);
        // SAFETY: an open handle and times (null: unchanged).
        check(t, unsafe { SetFileTime(h, filetime(c, &mut cs), filetime(l, &mut ls), filetime(w, &mut ws)) } != 0)?;
        ret_void()
    });
    r.add(D, "SetEndOfFile", "(J)V", |t, a| {
        // SAFETY: an open handle.
        check(t, unsafe { SetEndOfFile(a[0] as HANDLE) } != 0)?;
        ret_void()
    });
    r.add(D, "GetFileSizeEx", "(J)J", |t, a| {
        let mut size = 0i64;
        // SAFETY: an open handle.
        check(t, unsafe { GetFileSizeEx(a[0] as HANDLE, &mut size) } != 0)?;
        ret_long(size)
    });
    r.add(D, "CreateSymbolicLink0", "(JJI)V", |t, a| {
        let mut a = Args::new(a);
        let (link, target, flags) = (a.ptr::<u16>(), a.ptr::<u16>(), a.int() as u32);
        // SAFETY: NUL-terminated paths.
        check(t, unsafe { CreateSymbolicLinkW(link, target, flags as SYMBOLIC_LINK_FLAGS) } != 0)?;
        ret_void()
    });
    r.add(D, "CreateHardLink0", "(JJ)V", |t, a| {
        // SAFETY: NUL-terminated paths.
        check(t, unsafe { CreateHardLinkW(a[0] as *const u16, a[2] as *const u16, std::ptr::null()) } != 0)?;
        ret_void()
    });
}

fn volumes_and_paths(r: &mut NativeRegistry) {
    r.add(D, "GetLogicalDrives", "()I", |t, _| {
        // SAFETY: a plain query.
        let v = unsafe { GetLogicalDrives() };
        check(t, v != 0)?;
        ret_int(v as i32)
    });
    r.add(D, "GetVolumeInformation0", "(JLsun/nio/fs/WindowsNativeDispatcher$VolumeInformation;)V", |t, a| {
        let (mut name, mut fs) = ([0u16; 261], [0u16; 261]);
        let (mut serial, mut max, mut flags) = (0u32, 0u32, 0u32);
        // SAFETY: a root path and buffers of their sizes.
        let ok = unsafe { GetVolumeInformationW(a[0] as *const u16, name.as_mut_ptr(), 261, &mut serial, &mut max, &mut flags, fs.as_mut_ptr(), 261) } != 0;
        check(t, ok)?;
        let o = a[2] as Oop;
        let fs_name = string(t, &fs)?;
        set_string(o, VOLUME, "fileSystemName", fs_name);
        let volume = string(t, &name)?;
        set_string(o, VOLUME, "volumeName", volume);
        set_int(o, VOLUME, "volumeSerialNumber", serial as i32);
        set_int(o, VOLUME, "flags", flags as i32);
        ret_void()
    });
    // SAFETY: a NUL-terminated root path.
    r.add(D, "GetDriveType0", "(J)I", |_, a| ret_int(unsafe { GetDriveTypeW(a[0] as *const u16) } as i32));
    r.add(D, "GetDiskFreeSpaceEx0", "(JLsun/nio/fs/WindowsNativeDispatcher$DiskFreeSpace;)V", |t, a| {
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: a NUL-terminated path.
        check(t, unsafe { GetDiskFreeSpaceExW(a[0] as *const u16, &mut avail, &mut total, &mut free) } != 0)?;
        let o = a[2] as Oop;
        set_long(o, SPACE, "freeBytesAvailable", avail as i64);
        set_long(o, SPACE, "totalNumberOfBytes", total as i64);
        set_long(o, SPACE, "totalNumberOfFreeBytes", free as i64);
        ret_void()
    });
    r.add(D, "GetDiskFreeSpace0", "(JLsun/nio/fs/WindowsNativeDispatcher$DiskFreeSpace;)V", |t, a| {
        let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
        // SAFETY: a NUL-terminated path.
        check(t, unsafe { GetDiskFreeSpaceW(a[0] as *const u16, &mut spc, &mut bps, &mut free, &mut total) } != 0)?;
        set_long(a[2] as Oop, SPACE, "bytesPerSector", bps as i64);
        ret_void()
    });
    r.add(D, "GetVolumePathName0", "(J)Ljava/lang/String;", |t, a| {
        let mut buf = vec![0u16; 32768];
        // SAFETY: a path and a buffer of its size.
        check(t, unsafe { GetVolumePathNameW(a[0] as *const u16, buf.as_mut_ptr(), buf.len() as u32) } != 0)?;
        ret_ref(string(t, &buf)?)
    });
    r.add(D, "GetFullPathName0", "(J)Ljava/lang/String;", |t, a| {
        let path = a[0] as *const u16;
        // SAFETY: a path and buffers of the sizes passed.
        sized_string(t, |buf, len| unsafe { GetFullPathNameW(path, len, buf, std::ptr::null_mut()) })
    });
    r.add(D, "GetFinalPathNameByHandle", "(J)Ljava/lang/String;", |t, a| {
        let h = a[0] as HANDLE;
        // SAFETY: an open handle and buffers of the sizes passed.
        sized_string(t, |buf, len| unsafe { GetFinalPathNameByHandleW(h, buf, len, VOLUME_NAME_DOS) })
    });
    r.add(D, "FormatMessage", "(I)Ljava/lang/String;", |t, a| {
        let msg = std::io::Error::from_raw_os_error(a[0] as i32).to_string();
        let text = msg.split(" (os error").next().unwrap_or("").trim_end().to_string();
        ret_ref(crate::runtime::new_string(t, &text)?)
    });
    r.add(D, "LocalFree", "(J)V", |_, a| {
        // SAFETY: memory the system allocated for the class library.
        unsafe { windows_sys::Win32::Foundation::LocalFree(a[0] as *mut _) };
        ret_void()
    });
}

/// Completion ports and directory changes (`WindowsWatchService`), and overlapped I/O.
fn completion(r: &mut NativeRegistry) {
    r.add(D, "CreateIoCompletionPort", "(JJJ)J", |t, a| {
        let mut a = Args::new(a);
        let (file, existing, key) = (handle(&mut a), handle(&mut a), a.long());
        // SAFETY: handles the class library owns.
        let port = unsafe { CreateIoCompletionPort(file, existing, key as usize, 0) };
        if port.is_null() {
            return Err(last_error(t));
        }
        ret_long(port as i64)
    });
    r.add(D, "GetQueuedCompletionStatus0", "(JLsun/nio/fs/WindowsNativeDispatcher$CompletionStatus;)V", |t, a| {
        let (port, o) = (a[0] as usize, a[2] as Oop);
        let (mut bytes, mut key) = (0u32, 0usize);
        let mut ov: *mut OVERLAPPED = std::ptr::null_mut();
        let (bp, kp, op) = (&mut bytes as *mut u32 as usize, &mut key as *mut usize as usize, &mut ov as *mut *mut OVERLAPPED as usize);
        // Waits outside Java for the next completion.
        let ok = crate::gc::safepoint::blocking(t, || unsafe { GetQueuedCompletionStatus(port as HANDLE, bp as *mut u32, kp as *mut usize, op as *mut *mut OVERLAPPED, u32::MAX) });
        // SAFETY: a plain query.
        let error = if ok != 0 { 0 } else { unsafe { GetLastError() } };
        if ok == 0 && ov.is_null() {
            return Err(windows_error(t, error));
        }
        set_int(o, COMPLETION, "error", error as i32);
        set_int(o, COMPLETION, "bytesTransferred", bytes as i32);
        set_long(o, COMPLETION, "completionKey", key as i64);
        ret_void()
    });
    r.add(D, "PostQueuedCompletionStatus", "(JJ)V", |t, a| {
        // SAFETY: a completion port.
        check(t, unsafe { PostQueuedCompletionStatus(a[0] as HANDLE, 0, a[2] as usize, std::ptr::null()) } != 0)?;
        ret_void()
    });
    r.add(D, "ReadDirectoryChangesW", "(JJIZIJJ)V", |t, a| {
        let mut a = Args::new(a);
        let (dir, buf, len, subtree, filter, _bytes, ov) = (handle(&mut a), a.ptr::<u8>(), a.int() as u32, a.boolean(), a.int() as u32, a.long(), a.ptr::<OVERLAPPED>());
        // SAFETY: a directory handle opened for overlapped I/O and the caller's buffers, which
        // outlive the operation.
        let ok = unsafe { ReadDirectoryChangesW(dir, buf as *mut _, len, subtree as i32, filter, std::ptr::null_mut(), ov, None) } != 0;
        check(t, ok)?;
        ret_void()
    });
    r.add(D, "CancelIo", "(J)V", |t, a| {
        // SAFETY: an open handle.
        check(t, unsafe { CancelIo(a[0] as HANDLE) } != 0)?;
        ret_void()
    });
    r.add(D, "GetOverlappedResult", "(JJ)I", |t, a| {
        let mut n = 0u32;
        // SAFETY: a handle and its OVERLAPPED.
        check(t, unsafe { GetOverlappedResult(a[0] as HANDLE, a[2] as *const OVERLAPPED, &mut n, 0) } != 0)?;
        ret_int(n as i32)
    });
    r.add(D, "CreateEvent", "(ZZ)J", |t, a| {
        // SAFETY: an unnamed event.
        let e = unsafe { CreateEventW(std::ptr::null(), a[0] as i32, a[1] as i32, std::ptr::null()) };
        if e.is_null() {
            return Err(last_error(t));
        }
        ret_long(e as i64)
    });
}

pub(super) fn register(r: &mut NativeRegistry) {
    files(r);
    volumes_and_paths(r);
    completion(r);
}
