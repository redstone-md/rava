//! `sun.nio.ch` file channels over Windows handles, as the JDK's `FileDispatcherImpl.c` for
//! Windows: reads and writes at the handle's position or at an offset (restoring the position,
//! as positional calls must), byte-range locks, mappings, and `FileKey`/`IOUtil`.

use crate::heap::{obj, Oop};
use crate::natives::fields::set_long;
use crate::natives::io_shared::io_error;
use crate::natives::sock;
use crate::natives::{ret_bool, ret_int, ret_long, ret_void, NativeRegistry};
use crate::runtime::vm_field;
use crate::thread::{JResult, Thread};
use windows_sys::Win32::Foundation::{CloseHandle, DuplicateHandle, GetLastError, DUPLICATE_SAME_ACCESS, ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, ERROR_LOCK_VIOLATION, ERROR_NOT_LOCKED, HANDLE};
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Memory::{CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_COPY, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY};
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::System::IO::OVERLAPPED;

vm_field!(fd_handle, "java/io/FileDescriptor", "handle", "J");
vm_field!(fd_int, "java/io/FileDescriptor", "fd", "I");

const FD: &str = "sun/nio/ch/FileDispatcherImpl";
const IOUTIL: &str = "sun/nio/ch/IOUtil";
/// `IOStatus` and `FileDispatcher` results.
const EOF: i32 = -1;
const UNSUPPORTED: i64 = -4;
const NO_LOCK: i32 = -1;
const LOCKED: i32 = 0;

fn handle_of(fdo: u64) -> HANDLE {
    // SAFETY: a live FileDescriptor.
    unsafe { obj::read::<i64>(fdo as Oop, fd_handle()) as HANDLE }
}

fn at(offset: i64) -> OVERLAPPED {
    // SAFETY: an all-zero OVERLAPPED is valid; the offset is set below.
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.Anonymous.Anonymous.Offset = offset as u32;
    ov.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
    ov
}

fn position(h: HANDLE) -> Option<i64> {
    let mut p = 0i64;
    // SAFETY: queries the handle's position.
    (unsafe { SetFilePointerEx(h, 0, &mut p, FILE_CURRENT) } != 0).then_some(p)
}

fn set_position(h: HANDLE, p: i64) {
    // SAFETY: moves the handle's position.
    unsafe { SetFilePointerEx(h, p, std::ptr::null_mut(), FILE_BEGIN) };
}

/// Reads `len` bytes to `address`, at `offset` if given: the count, or `EOF`.
fn read(t: &mut Thread, h: HANDLE, address: usize, len: u32, offset: Option<i64>) -> JResult<i32> {
    let mut ov = offset.map(at);
    let ovp = ov.as_mut().map_or(std::ptr::null_mut(), |o| o as *mut OVERLAPPED) as usize;
    let mut n = 0u32;
    let np = &mut n as *mut u32 as usize;
    let hv = h as usize;
    // SAFETY: the caller's buffer of `len` bytes; reads block outside Java.
    let ok = crate::gc::safepoint::blocking(t, || unsafe { ReadFile(hv as HANDLE, address as *mut u8, len, np as *mut u32, ovp as *mut OVERLAPPED) });
    if ok == 0 {
        // SAFETY: a plain query.
        return match unsafe { GetLastError() } {
            ERROR_HANDLE_EOF | ERROR_BROKEN_PIPE => Ok(EOF),
            _ => Err(io_error(t, "Read failed")),
        };
    }
    Ok(if n == 0 && len > 0 { EOF } else { n as i32 })
}

/// Writes `len` bytes from `address`: at `offset` if given, at the end with `append`.
fn write(t: &mut Thread, h: HANDLE, address: usize, len: u32, offset: Option<i64>, append: bool) -> JResult<i32> {
    // An offset of all ones writes at the end of the file.
    let mut ov = if append { Some(at(-1)) } else { offset.map(at) };
    let ovp = ov.as_mut().map_or(std::ptr::null_mut(), |o| o as *mut OVERLAPPED);
    let mut n = 0u32;
    // SAFETY: the caller's buffer of `len` bytes.
    if unsafe { WriteFile(h, address as *const u8, len, &mut n, ovp) } == 0 {
        return Err(io_error(t, "Write failed"));
    }
    Ok(n as i32)
}

/// Runs `f` at `offset` and puts the handle's position back, as `pread`/`pwrite` leave it.
fn positional(h: HANDLE, f: impl FnOnce() -> JResult<i32>) -> JResult<u64> {
    let saved = position(h);
    let r = f();
    if let Some(p) = saved {
        set_position(h, p);
    }
    ret_int(r?)
}

/// The `{base, length}` pairs of an `IOVecWrapper` (pointer-sized fields).
fn iovecs(address: usize, n: usize) -> Vec<(usize, u32)> {
    // SAFETY: the caller's array of `n` pairs.
    (0..n).map(|i| unsafe { (*((address + i * 16) as *const usize), *((address + i * 16 + 8) as *const usize) as u32) }).collect()
}

fn files(r: &mut NativeRegistry) {
    r.add(FD, "read0", "(Ljava/io/FileDescriptor;JI)I", |t, a| ret_int(read(t, handle_of(a[0]), a[1] as usize, a[3] as u32, None)?));
    r.add(FD, "pread0", "(Ljava/io/FileDescriptor;JIJ)I", |t, a| {
        let h = handle_of(a[0]);
        positional(h, || read(t, h, a[1] as usize, a[3] as u32, Some(a[4] as i64)))
    });
    r.add(FD, "readv0", "(Ljava/io/FileDescriptor;JI)J", |t, a| {
        let h = handle_of(a[0]);
        let mut total = 0i64;
        for (base, len) in iovecs(a[1] as usize, a[3] as usize) {
            let n = read(t, h, base, len, None)?;
            if n == EOF {
                return ret_long(if total == 0 { EOF as i64 } else { total });
            }
            total += n as i64;
            if (n as u32) < len {
                break;
            }
        }
        ret_long(total)
    });
    r.add(FD, "write0", "(Ljava/io/FileDescriptor;JIZ)I", |t, a| ret_int(write(t, handle_of(a[0]), a[1] as usize, a[3] as u32, None, a[4] != 0)?));
    r.add(FD, "pwrite0", "(Ljava/io/FileDescriptor;JIJ)I", |t, a| {
        let h = handle_of(a[0]);
        positional(h, || write(t, h, a[1] as usize, a[3] as u32, Some(a[4] as i64), false))
    });
    r.add(FD, "writev0", "(Ljava/io/FileDescriptor;JIZ)J", |t, a| {
        let h = handle_of(a[0]);
        let mut total = 0i64;
        for (base, len) in iovecs(a[1] as usize, a[3] as usize) {
            let n = write(t, h, base, len, None, a[4] != 0)?;
            total += n as i64;
            if (n as u32) < len {
                break;
            }
        }
        ret_long(total)
    });
    // A negative offset asks for the position.
    r.add(FD, "seek0", "(Ljava/io/FileDescriptor;J)J", |t, a| {
        let h = handle_of(a[0]);
        let mut p = 0i64;
        let (distance, method) = if (a[1] as i64) < 0 { (0, FILE_CURRENT) } else { (a[1] as i64, FILE_BEGIN) };
        // SAFETY: moves or queries the handle's position.
        if unsafe { SetFilePointerEx(h, distance, &mut p, method) } == 0 {
            return Err(io_error(t, "Seek failed"));
        }
        ret_long(p)
    });
    r.add(FD, "force0", "(Ljava/io/FileDescriptor;Z)I", |t, a| {
        // SAFETY: an open handle.
        if unsafe { FlushFileBuffers(handle_of(a[0])) } == 0 {
            return Err(io_error(t, "Force failed"));
        }
        ret_int(0)
    });
    r.add(FD, "truncate0", "(Ljava/io/FileDescriptor;J)I", |t, a| {
        let info = FILE_END_OF_FILE_INFO { EndOfFile: a[1] as i64 };
        // SAFETY: sets the end of an open file without moving its position.
        let ok = unsafe { SetFileInformationByHandle(handle_of(a[0]), FileEndOfFileInfo, &info as *const _ as *const _, std::mem::size_of::<FILE_END_OF_FILE_INFO>() as u32) } != 0;
        if !ok {
            return Err(io_error(t, "Truncation failed"));
        }
        ret_int(0)
    });
    r.add(FD, "size0", "(Ljava/io/FileDescriptor;)J", |t, a| {
        let mut s = 0i64;
        // SAFETY: an open handle.
        if unsafe { GetFileSizeEx(handle_of(a[0]), &mut s) } == 0 {
            return Err(io_error(t, "Size failed"));
        }
        ret_long(s)
    });
    r.add(FD, "lock0", "(Ljava/io/FileDescriptor;ZJJZ)I", |t, a| {
        let (h, blocking, pos, size, shared) = (handle_of(a[0]), a[1] != 0, a[2] as i64, a[4] as i64, a[6] != 0);
        let flags = if shared { 0 } else { LOCKFILE_EXCLUSIVE_LOCK } | if blocking { 0 } else { LOCKFILE_FAIL_IMMEDIATELY };
        let mut ov = at(pos);
        let (hv, ovp) = (h as usize, &mut ov as *mut OVERLAPPED as usize);
        // SAFETY: a byte-range lock on an open handle; a blocking one waits outside Java.
        let ok = crate::gc::safepoint::blocking(t, || unsafe { LockFileEx(hv as HANDLE, flags, 0, size as u32, (size >> 32) as u32, ovp as *mut OVERLAPPED) }) != 0;
        if ok {
            return ret_int(LOCKED);
        }
        // SAFETY: a plain query.
        match unsafe { GetLastError() } {
            ERROR_LOCK_VIOLATION if !blocking => ret_int(NO_LOCK),
            _ => Err(io_error(t, "Lock failed")),
        }
    });
    r.add(FD, "release0", "(Ljava/io/FileDescriptor;JJ)V", |t, a| {
        let (h, pos, size) = (handle_of(a[0]), a[1] as i64, a[3] as i64);
        let mut ov = at(pos);
        // SAFETY: unlocks a range of an open handle.
        if unsafe { UnlockFileEx(h, 0, size as u32, (size >> 32) as u32, &mut ov) } == 0 && unsafe { GetLastError() } != ERROR_NOT_LOCKED {
            return Err(io_error(t, "Release failed"));
        }
        ret_void()
    });
    r.add(FD, "close0", "(Ljava/io/FileDescriptor;)V", |t, a| {
        let h = handle_of(a[0]);
        if h as i64 != -1 {
            // SAFETY: a live FileDescriptor whose handle is closed once.
            unsafe { obj::write::<i64>(a[0] as Oop, fd_handle(), -1) };
            if unsafe { CloseHandle(h) } == 0 {
                return Err(io_error(t, "Close failed"));
            }
        }
        ret_void()
    });
    // Modes from FileChannelImpl: MAP_RO 0, MAP_RW 1, MAP_PV 2.
    r.add(FD, "map0", "(Ljava/io/FileDescriptor;IJJZ)J", |t, a| {
        let (h, mode, pos, len) = (handle_of(a[0]), a[1] as i32, a[2] as i64, a[4] as i64);
        let (protect, access) = match mode {
            0 => (PAGE_READONLY, FILE_MAP_READ),
            1 => (PAGE_READWRITE, FILE_MAP_WRITE),
            _ => (PAGE_WRITECOPY, FILE_MAP_COPY),
        };
        let end = pos + len;
        // SAFETY: a mapping of an open file; the mapping object closes once the view holds it.
        let view = unsafe {
            let m = CreateFileMappingW(h, std::ptr::null(), protect, (end >> 32) as u32, end as u32, std::ptr::null());
            if m.is_null() {
                return Err(io_error(t, "Map failed"));
            }
            let v = MapViewOfFile(m, access, (pos >> 32) as u32, pos as u32, len as usize);
            CloseHandle(m);
            v.Value as usize
        };
        if view == 0 {
            return Err(crate::runtime::throw_new(t, "java/lang/OutOfMemoryError", "Map failed"));
        }
        ret_long(view as i64)
    });
    r.add(FD, "unmap0", "(JJ)I", |t, a| {
        // SAFETY: a view map0 made.
        if unsafe { UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: a[0] as *mut _ }) } == 0 {
            return Err(io_error(t, "Unmap failed"));
        }
        ret_int(0)
    });
    // No kernel copy here: the caller transfers through a buffer.
    r.add(FD, "transferTo0", "(Ljava/io/FileDescriptor;JJLjava/io/FileDescriptor;Z)J", |_, _| ret_long(UNSUPPORTED));
    r.add(FD, "maxDirectTransferSize0", "()I", |_, _| ret_int(i32::MAX));
    r.add(FD, "allocationGranularity0", "()J", |_, _| {
        // SAFETY: fills a SYSTEM_INFO.
        let mut si: SYSTEM_INFO = unsafe { std::mem::zeroed() };
        unsafe { GetSystemInfo(&mut si) };
        ret_long(si.dwAllocationGranularity as i64)
    });
    r.add(FD, "duplicateHandle", "(J)J", |t, a| {
        let mut out: HANDLE = std::ptr::null_mut();
        // SAFETY: duplicates a handle within this process.
        let ok = unsafe { DuplicateHandle(GetCurrentProcess(), a[0] as HANDLE, GetCurrentProcess(), &mut out, 0, 0, DUPLICATE_SAME_ACCESS) } != 0;
        if !ok {
            return Err(io_error(t, "DuplicateHandle failed"));
        }
        ret_long(out as i64)
    });
    r.add(FD, "setDirect0", "(Ljava/io/FileDescriptor;Ljava/nio/CharBuffer;)I", |_, _| ret_int(-1));
    r.add("sun/nio/ch/FileKey", "initIDs", "()V", |_, _| ret_void());
    r.add("sun/nio/ch/FileKey", "init", "(Ljava/io/FileDescriptor;)V", |t, a| {
        // SAFETY: an open handle; the info is copied into the key.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(handle_of(a[1]), &mut info) } == 0 {
            return Err(io_error(t, "GetFileInformationByHandle failed"));
        }
        const K: &str = "sun/nio/ch/FileKey";
        let key = a[0] as Oop;
        set_long(key, K, "dwVolumeSerialNumber", info.dwVolumeSerialNumber as i64);
        set_long(key, K, "nFileIndexHigh", info.nFileIndexHigh as i64);
        set_long(key, K, "nFileIndexLow", info.nFileIndexLow as i64);
        ret_void()
    });
}

fn ioutil(r: &mut NativeRegistry) {
    r.add(IOUTIL, "initIDs", "()V", |_, _| ret_void());
    // SAFETY: a live FileDescriptor.
    r.add(IOUTIL, "fdVal", "(Ljava/io/FileDescriptor;)I", |_, a| ret_int(unsafe { obj::read::<i32>(a[0] as Oop, fd_int()) }));
    r.add(IOUTIL, "setfdVal", "(Ljava/io/FileDescriptor;I)V", |_, a| {
        // SAFETY: as above.
        unsafe { obj::write::<i32>(a[0] as Oop, fd_int(), a[1] as i32) };
        ret_void()
    });
    r.add(IOUTIL, "iovMax", "()I", |_, _| ret_int(16));
    r.add(IOUTIL, "writevMax", "()J", |_, _| ret_long(i32::MAX as i64));
    r.add(IOUTIL, "fdLimit", "()I", |_, _| ret_int(i32::MAX));
    r.add(IOUTIL, "randomBytes", "([B)Z", |_, _| ret_bool(false));
    // Wakeup pipes and selectable channels are sockets on Windows.
    r.add(IOUTIL, "configureBlocking", "(Ljava/io/FileDescriptor;Z)V", |t, a| {
        // SAFETY: a live FileDescriptor.
        let fd = unsafe { obj::read::<i32>(a[0] as Oop, fd_int()) };
        if sock::set_blocking(fd, a[1] != 0) != 0 {
            return Err(io_error(t, "Configure blocking failed"));
        }
        ret_void()
    });
    // As the JDK's Windows IOUtil.c. The wakeup pipe's sockets may be blocking, so these stop at
    // what is there: `drain` returns after a short read instead of reading until nothing is left
    // (which would wait forever on a blocking socket), and each call runs outside Java.
    r.add(IOUTIL, "write1", "(IB)I", |t, a| {
        let (fd, b) = (a[0] as i32, a[1] as u8);
        let n = crate::gc::safepoint::blocking(t, || sock::send(fd, &b as *const u8 as usize, 1));
        if n < 0 && sock::last_error() != sock::SockError::WouldBlock {
            return Err(io_error(t, "send failed"));
        }
        ret_int((n == 1) as i32)
    });
    r.add(IOUTIL, "drain1", "(I)I", |t, a| {
        let (fd, mut b) = (a[0] as i32, 0u8);
        let n = crate::gc::safepoint::blocking(t, || sock::recv(fd, &mut b as *mut u8 as usize, 1));
        if n < 0 {
            if sock::last_error() == sock::SockError::WouldBlock {
                return ret_int(0);
            }
            return Err(io_error(t, "recv failed"));
        }
        ret_int(n as i32)
    });
    r.add(IOUTIL, "drain", "(I)Z", |t, a| {
        let fd = a[0] as i32;
        let mut read = false;
        loop {
            let mut buf = [0u8; 16];
            let n = crate::gc::safepoint::blocking(t, || sock::recv(fd, buf.as_mut_ptr() as usize, buf.len()));
            if n < 0 {
                if sock::last_error() != sock::SockError::WouldBlock {
                    return Err(io_error(t, "recv failed"));
                }
                return ret_bool(read);
            }
            if n == 0 {
                return ret_bool(read);
            }
            if (n as usize) < buf.len() {
                return ret_bool(true);
            }
            read = true;
        }
    });
}

pub(super) fn register(r: &mut NativeRegistry) {
    files(r);
    ioutil(r);
}
