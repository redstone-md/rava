//! Natives of the Linux class library only: the `EPoll` selector and `EventFD`, and
//! `sun.nio.fs.LinuxNativeDispatcher` (the mount table, `posix_fadvise`, kernel-side copies).

use super::io::io_error;
use super::net::{check, errno as net_errno, INTERRUPTED};
use super::unixfs::{cstr, errno, unix_error};
use super::unixfs_ext::UNSUPPORTED_CASE;
use super::{byte_array, ret_int, ret_long, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::thread::{JResult, Thread};

crate::runtime::vm_field!(mnt_name, "sun/nio/fs/UnixMountEntry", "name", "[B");
crate::runtime::vm_field!(mnt_dir, "sun/nio/fs/UnixMountEntry", "dir", "[B");
crate::runtime::vm_field!(mnt_fstype, "sun/nio/fs/UnixMountEntry", "fstype", "[B");
crate::runtime::vm_field!(mnt_opts, "sun/nio/fs/UnixMountEntry", "opts", "[B");
crate::runtime::vm_field!(mnt_dev, "sun/nio/fs/UnixMountEntry", "dev", "J");

const L: &str = "sun/nio/fs/LinuxNativeDispatcher";

pub(super) fn register(r: &mut NativeRegistry) {
    const EP: &str = "sun/nio/ch/EPoll";
    r.add(EP, "eventSize", "()I", |_, _| ret_int(std::mem::size_of::<libc::epoll_event>() as i32));
    r.add(EP, "eventsOffset", "()I", |_, _| ret_int(0));
    r.add(EP, "dataOffset", "()I", |_, _| ret_int(4));
    r.add(EP, "create", "()I", |t, _| ret_int(check(t, unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) }, "epoll_create failed")?));
    r.add(EP, "ctl", "(IIII)I", |_, a| {
        let mut ev = libc::epoll_event { events: a[3] as u32, u64: a[2] as i32 as u64 };
        let r = unsafe { libc::epoll_ctl(a[0] as i32, a[1] as i32, a[2] as i32, &mut ev) };
        ret_int(if r == 0 { 0 } else { net_errno() })
    });
    r.add(EP, "wait", "(IJII)I", |t, a| {
        let n = crate::gc::safepoint::blocking(t, || unsafe { libc::epoll_wait(a[0] as i32, a[1] as *mut libc::epoll_event, a[3] as i32, a[4] as i32) });
        if n < 0 {
            return if net_errno() == libc::EINTR { ret_int(INTERRUPTED) } else { Err(io_error(t, "epoll_wait failed")) };
        }
        ret_int(n)
    });
    r.add("sun/nio/ch/EventFD", "eventfd0", "()I", |t, _| ret_int(check(t, unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) }, "eventfd failed")?));
    r.add("sun/nio/ch/EventFD", "set0", "(I)I", |_, a| {
        let one: u64 = 1;
        ret_int(unsafe { libc::write(a[0] as i32, &one as *const u64 as *const libc::c_void, 8) } as i32)
    });
    r.add(L, "init", "()V", |_, _| ret_void());
    r.add(L, "posix_fadvise", "(IJJI)I", |_, a| {
        ret_int(unsafe { libc::posix_fadvise(a[0] as i32, a[1] as i64, a[3] as i64, a[5] as i32) })
    });
    r.add(L, "directCopy0", "(IIJ)I", direct_copy);
    // The mount table (`FileStore`s): setmntent / getmntent_r / endmntent.
    r.add(L, "setmntent0", "(JJ)J", |t, a| {
        let fp = unsafe { libc::setmntent(cstr(a[0]), cstr(a[2])) };
        if fp.is_null() {
            return Err(unix_error(t, errno()));
        }
        ret_long(fp as i64)
    });
    r.add(L, "getmntent0", "(JLsun/nio/fs/UnixMountEntry;JI)I", get_mount_entry);
    r.add(L, "endmntent", "(J)V", |_, a| {
        unsafe { libc::endmntent(a[0] as *mut libc::FILE) };
        ret_void()
    });
}

/// Reads the next mount entry into `entry`; -1 at the end of the table.
fn get_mount_entry(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (fp, entry, buf, len) = (a[0] as *mut libc::FILE, a[2] as Oop, a[3] as *mut libc::c_char, a[5] as i32);
    let mut m: libc::mntent = unsafe { std::mem::zeroed() };
    if unsafe { libc::getmntent_r(fp, &mut m, buf, len) }.is_null() {
        return ret_int(-1);
    }
    // SAFETY: getmntent_r filled `m` with strings in `buf`.
    let field = |p: *const libc::c_char| unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes().to_vec();
    let (name, dir, fstype, opts) = (field(m.mnt_fsname), field(m.mnt_dir), field(m.mnt_type), field(m.mnt_opts));
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let dev = if unsafe { libc::stat(m.mnt_dir, &mut st) } == 0 { st.st_dev as i64 } else { 0 };
    for (f, v) in [(mnt_name(), name), (mnt_dir(), dir), (mnt_fstype(), fstype), (mnt_opts(), opts)] {
        let arr = byte_array(t, Some(&v))?;
        // SAFETY: a live UnixMountEntry.
        unsafe { obj::write_ref(entry, f, arr) };
    }
    // SAFETY: a live UnixMountEntry.
    unsafe { obj::write::<i64>(entry, mnt_dev(), dev) };
    ret_int(0)
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

