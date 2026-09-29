//! The rest of `sun.nio.fs.UnixNativeDispatcher` (timestamps, ownership, special files, name
//! lookups) and `sun.nio.fs.LinuxNativeDispatcher` (kernel-side file copies).

use super::unixfs::{check, cstr, errno, unix_error};
use super::{ret_int, ret_void, NativeRegistry};
use crate::thread::{JResult, Thread};

const D: &str = "sun/nio/fs/UnixNativeDispatcher";
const L: &str = "sun/nio/fs/LinuxNativeDispatcher";
/// `sun.nio.ch.IOStatus.UNSUPPORTED_CASE`: the caller falls back to a buffered copy.
const UNSUPPORTED_CASE: i32 = -6;

fn timeval(micros: i64) -> libc::timeval {
    libc::timeval { tv_sec: micros.div_euclid(1_000_000), tv_usec: micros.rem_euclid(1_000_000) }
}

fn timespec(nanos: i64) -> libc::timespec {
    libc::timespec { tv_sec: nanos.div_euclid(1_000_000_000), tv_nsec: nanos.rem_euclid(1_000_000_000) }
}

fn done(t: &mut Thread, r: i32) -> JResult<u64> {
    check(t, r as i64)?;
    ret_void()
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(D, "utimes0", "(JJJ)V", |t, a| {
        let tv = [timeval(a[2] as i64), timeval(a[4] as i64)];
        done(t, unsafe { libc::utimes(cstr(a[0]), tv.as_ptr()) })
    });
    r.add(D, "lutimes0", "(JJJ)V", |t, a| {
        let tv = [timeval(a[2] as i64), timeval(a[4] as i64)];
        done(t, unsafe { libc::lutimes(cstr(a[0]), tv.as_ptr()) })
    });
    r.add(D, "futimes0", "(IJJ)V", |t, a| {
        let tv = [timeval(a[1] as i64), timeval(a[3] as i64)];
        done(t, unsafe { libc::futimes(a[0] as i32, tv.as_ptr()) })
    });
    r.add(D, "futimens0", "(IJJ)V", |t, a| {
        let ts = [timespec(a[1] as i64), timespec(a[3] as i64)];
        done(t, unsafe { libc::futimens(a[0] as i32, ts.as_ptr()) })
    });
    r.add(D, "chown0", "(JII)V", |t, a| done(t, unsafe { libc::chown(cstr(a[0]), a[2] as u32, a[3] as u32) }));
    r.add(D, "lchown0", "(JII)V", |t, a| done(t, unsafe { libc::lchown(cstr(a[0]), a[2] as u32, a[3] as u32) }));
    r.add(D, "fchown0", "(III)V", |t, a| done(t, unsafe { libc::fchown(a[0] as i32, a[1] as u32, a[2] as u32) }));
    r.add(D, "mknod0", "(JIJ)V", |t, a| done(t, unsafe { libc::mknod(cstr(a[0]), a[2] as u32, a[3] as libc::dev_t) }));
    r.add(D, "getpwnam0", "(J)I", |_, a| {
        let pw = unsafe { libc::getpwnam(cstr(a[0])) };
        ret_int(if pw.is_null() { -1 } else { unsafe { (*pw).pw_uid as i32 } })
    });
    r.add(D, "getgrnam0", "(J)I", |_, a| {
        let gr = unsafe { libc::getgrnam(cstr(a[0])) };
        ret_int(if gr.is_null() { -1 } else { unsafe { (*gr).gr_gid as i32 } })
    });
    r.add(L, "init", "()V", |_, _| ret_void());
    r.add(L, "posix_fadvise", "(IJJI)I", |_, a| {
        ret_int(unsafe { libc::posix_fadvise(a[0] as i32, a[1] as i64, a[3] as i64, a[5] as i32) })
    });
    r.add(L, "directCopy0", "(IIJ)I", direct_copy);
}

/// Copies all of `src` to `dst` in the kernel, polling the cancel flag between chunks.
fn direct_copy(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (dst, src, cancel) = (a[0] as i32, a[1] as i32, a[2] as *const i32);
    loop {
        let n = unsafe { libc::copy_file_range(src, std::ptr::null_mut(), dst, std::ptr::null_mut(), 1 << 30, 0) };
        match n {
            0 => return ret_int(0),
            -1 => {
                let err = errno();
                return match err {
                    libc::EINVAL | libc::ENOSYS | libc::EXDEV | libc::EOPNOTSUPP | libc::EPERM => ret_int(UNSUPPORTED_CASE),
                    _ => Err(unix_error(t, err)),
                };
            }
            _ if !cancel.is_null() && unsafe { *cancel } != 0 => return Err(unix_error(t, libc::ECANCELED)),
            _ => {}
        }
    }
}
