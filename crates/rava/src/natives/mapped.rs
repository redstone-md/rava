//! `java.nio.MappedMemoryUtils`: `MappedByteBuffer.load`, `isLoaded`, `force`, as the JDK's
//! `MappedMemoryUtils.c` does them: `madvise`, `mincore` and `msync` on Unix; on Windows,
//! which tells nothing about residency, `load` and `unload` do nothing, `isLoaded` says false,
//! and `force` flushes the view and then the file's buffers.

use super::{ret_bool, ret_void, NativeRegistry};
use crate::thread::{JResult, Thread};

const C: &str = "java/nio/MappedMemoryUtils";

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(C, "registerNatives", "()V", |_, _| ret_void());
    r.add(C, "isLoaded0", "(JJJ)Z", |t, a| ret_bool(is_loaded(t, a[0] as usize, a[2] as usize, a[4] as usize)?));
    r.add(C, "load0", "(JJ)V", |t, a| {
        advise(t, a[0] as usize, a[2] as usize, true)?;
        ret_void()
    });
    r.add(C, "unload0", "(JJ)V", |t, a| {
        advise(t, a[0] as usize, a[2] as usize, false)?;
        ret_void()
    });
    r.add(C, "force0", "(Ljava/io/FileDescriptor;JJ)V", |t, a| {
        force(t, a[0] as crate::heap::Oop, a[1] as usize, a[3] as usize)?;
        ret_void()
    });
}

#[cfg(unix)]
fn is_loaded(t: &mut Thread, addr: usize, len: usize, pages: usize) -> JResult<bool> {
    let mut vec = vec![0u8; pages];
    // SAFETY: a page-aligned mapping of `len` bytes; `vec` has a byte for each of its pages.
    if unsafe { libc::mincore(addr as *mut libc::c_void, len, vec.as_mut_ptr().cast()) } != 0 {
        return Err(super::io_shared::io_error(t, "mincore failed"));
    }
    Ok(vec.iter().all(|&b| b & 1 != 0))
}

#[cfg(unix)]
fn advise(t: &mut Thread, addr: usize, len: usize, load: bool) -> JResult<()> {
    let advice = if load { libc::MADV_WILLNEED } else { libc::MADV_DONTNEED };
    // SAFETY: advice about a mapping the buffer owns.
    if unsafe { libc::madvise(addr as *mut libc::c_void, len, advice) } != 0 {
        return Err(super::io_shared::io_error(t, "madvise failed"));
    }
    Ok(())
}

#[cfg(unix)]
fn force(t: &mut Thread, _fd: crate::heap::Oop, addr: usize, len: usize) -> JResult<()> {
    // SAFETY: a page-aligned range of a mapping the buffer owns.
    if unsafe { libc::msync(addr as *mut libc::c_void, len, libc::MS_SYNC) } != 0 {
        return Err(super::io_shared::io_error(t, "msync failed"));
    }
    Ok(())
}

#[cfg(windows)]
fn is_loaded(_: &mut Thread, _: usize, _: usize, _: usize) -> JResult<bool> {
    Ok(false)
}

#[cfg(windows)]
fn advise(_: &mut Thread, _: usize, _: usize, _: bool) -> JResult<()> {
    Ok(())
}

#[cfg(windows)]
crate::runtime::vm_field!(fd_handle, "java/io/FileDescriptor", "handle", "J");

#[cfg(windows)]
fn force(t: &mut Thread, fd: crate::heap::Oop, addr: usize, len: usize) -> JResult<()> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_LOCK_VIOLATION, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::FlushFileBuffers;
    use windows_sys::Win32::System::Memory::FlushViewOfFile;
    // A view another process flushes at the same time can refuse with a lock violation; the
    // JDK tries three times.
    let mut ok = 0;
    for _ in 0..3 {
        // SAFETY: a range of a view the buffer owns.
        ok = unsafe { FlushViewOfFile(addr as *const _, len) };
        // SAFETY: a plain query.
        if ok != 0 || unsafe { GetLastError() } != ERROR_LOCK_VIOLATION {
            break;
        }
    }
    if ok == 0 {
        return Err(super::io_shared::io_error(t, "Flush failed"));
    }
    if !fd.is_null() {
        // SAFETY: a live FileDescriptor.
        let h = unsafe { crate::heap::obj::read::<i64>(fd, fd_handle()) };
        // SAFETY: the mapped file's handle.
        if unsafe { FlushFileBuffers(h as HANDLE) } == 0 {
            return Err(super::io_shared::io_error(t, "Flush failed"));
        }
    }
    Ok(())
}
