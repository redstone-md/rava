//! The rest of `sun.nio.fs.UnixNativeDispatcher`: timestamps, ownership, special files, name
//! lookups, extended attributes and buffered copies.

use super::unixfs::{check, cstr, errno, unix_error};
use super::{ret_int, ret_void, NativeRegistry};
use crate::thread::{JResult, Thread};


const D: &str = "sun/nio/fs/UnixNativeDispatcher";
/// `sun.nio.ch.IOStatus.UNSUPPORTED_CASE`: the caller falls back to a buffered copy.
pub(super) const UNSUPPORTED_CASE: i32 = -6;

/// Extended attributes of open files; macOS's calls take a position and options as well.
mod xattr {
    use libc::{c_char, c_int, c_void, ssize_t};

    pub unsafe fn list(fd: c_int, buf: *mut c_char, size: usize) -> ssize_t {
        #[cfg(target_os = "macos")]
        return libc::flistxattr(fd, buf, size, 0);
        #[cfg(not(target_os = "macos"))]
        return libc::flistxattr(fd, buf, size);
    }

    pub unsafe fn get(fd: c_int, name: *const c_char, value: *mut c_void, size: usize) -> ssize_t {
        #[cfg(target_os = "macos")]
        return libc::fgetxattr(fd, name, value, size, 0, 0);
        #[cfg(not(target_os = "macos"))]
        return libc::fgetxattr(fd, name, value, size);
    }

    pub unsafe fn set(fd: c_int, name: *const c_char, value: *const c_void, size: usize) -> c_int {
        #[cfg(target_os = "macos")]
        return libc::fsetxattr(fd, name, value, size, 0, 0);
        #[cfg(not(target_os = "macos"))]
        return libc::fsetxattr(fd, name, value, size, 0);
    }

    pub unsafe fn remove(fd: c_int, name: *const c_char) -> c_int {
        #[cfg(target_os = "macos")]
        return libc::fremovexattr(fd, name, 0);
        #[cfg(not(target_os = "macos"))]
        return libc::fremovexattr(fd, name);
    }
}

fn timeval(micros: i64) -> libc::timeval {
    libc::timeval { tv_sec: micros.div_euclid(1_000_000), tv_usec: micros.rem_euclid(1_000_000) as libc::suseconds_t }
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
    r.add(D, "mknod0", "(JIJ)V", |t, a| done(t, unsafe { libc::mknod(cstr(a[0]), a[2] as libc::mode_t, a[3] as libc::dev_t) }));
    r.add(D, "getpwnam0", "(J)I", |_, a| {
        let pw = unsafe { libc::getpwnam(cstr(a[0])) };
        ret_int(if pw.is_null() { -1 } else { unsafe { (*pw).pw_uid as i32 } })
    });
    r.add(D, "getgrnam0", "(J)I", |_, a| {
        let gr = unsafe { libc::getgrnam(cstr(a[0])) };
        ret_int(if gr.is_null() { -1 } else { unsafe { (*gr).gr_gid as i32 } })
    });
    // Extended attributes of open files (copied along with a file's attributes).
    r.add(D, "flistxattr", "(IJI)I", |t, a| {
        let n = unsafe { xattr::list(a[0] as i32, a[1] as *mut libc::c_char, a[3] as usize) };
        if n == -1 { Err(unix_error(t, errno())) } else { ret_int(n as i32) }
    });
    r.add(D, "fgetxattr0", "(IJJI)I", |t, a| {
        let n = unsafe { xattr::get(a[0] as i32, a[1] as *const libc::c_char, a[3] as *mut libc::c_void, a[5] as usize) };
        if n == -1 { Err(unix_error(t, errno())) } else { ret_int(n as i32) }
    });
    r.add(D, "fsetxattr0", "(IJJI)V", |t, a| {
        let rc = unsafe { xattr::set(a[0] as i32, a[1] as *const libc::c_char, a[3] as *const libc::c_void, a[5] as usize) };
        if rc == -1 { Err(unix_error(t, errno())) } else { ret_void() }
    });
    r.add(D, "fremovexattr0", "(IJ)V", |t, a| {
        let rc = unsafe { xattr::remove(a[0] as i32, a[1] as *const libc::c_char) };
        if rc == -1 { Err(unix_error(t, errno())) } else { ret_void() }
    });
    r.add("sun/nio/fs/UnixFileSystem", "bufferedCopy0", "(IIJIJ)V", buffered_copy);
    // The length of the stream's next line (sizing the mount table); -1 at its end.
    r.add(D, "getlinelen", "(J)I", |t, a| {
        let fp = a[0] as *mut libc::FILE;
        let (mut line, mut size) = (std::ptr::null_mut(), 0usize);
        let n = unsafe { libc::getline(&mut line, &mut size, fp) };
        let err = errno();
        unsafe { libc::free(line as *mut libc::c_void) };
        if unsafe { libc::feof(fp) } != 0 {
            return ret_int(-1);
        }
        if n == -1 {
            return Err(unix_error(t, err));
        }
        ret_int(n.min(i32::MAX as isize) as i32)
    });
    r.add(D, "rewind", "(J)V", |_, a| {
        unsafe { libc::rewind(a[0] as *mut libc::FILE) };
        ret_void()
    });
}

/// Copies all of `src` to `dst` through the caller's buffer (when the kernel cannot copy
/// directly), polling the cancel flag after each read.
fn buffered_copy(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (dst, src, buf, size, cancel) = (a[0] as i32, a[1] as i32, a[2] as *mut libc::c_void, a[4] as usize, a[5] as *const i32);
    loop {
        let n = unsafe { libc::read(src, buf, size) };
        if n <= 0 {
            match n {
                0 => return ret_void(),
                _ if errno() == libc::EINTR => continue,
                _ => return Err(unix_error(t, errno())),
            }
        }
        if !cancel.is_null() && unsafe { *cancel } != 0 {
            return Err(unix_error(t, libc::ECANCELED));
        }
        let mut done = 0;
        while done < n as usize {
            let w = unsafe { libc::write(dst, (buf as *const u8).add(done) as *const libc::c_void, n as usize - done) };
            if w == -1 {
                if errno() == libc::EINTR {
                    continue;
                }
                return Err(unix_error(t, errno()));
            }
            done += w as usize;
        }
    }
}
