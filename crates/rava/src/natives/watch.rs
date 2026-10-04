//! `sun.nio.fs.LinuxWatchService`: inotify behind `java.nio.file.WatchService`.

use super::unixfs::{check, errno, unix_error};
use super::{ret_int, ret_ref, ret_void, NativeRegistry};
use crate::heap::obj;
use crate::runtime as rt;
use crate::types::BasicType;
use crate::vm::vm;

const W: &str = "sun/nio/fs/LinuxWatchService";

pub(super) fn register(r: &mut NativeRegistry) {
    // struct inotify_event { int wd; uint32_t mask; uint32_t cookie; uint32_t len; char name[]; }
    r.add(W, "eventSize", "()I", |_, _| ret_int(std::mem::size_of::<libc::inotify_event>() as i32));
    r.add(W, "eventOffsets", "()[I", |t, _| {
        let offsets = [0i32, 4, 8, 12, std::mem::size_of::<libc::inotify_event>() as i32];
        let arr = rt::alloc_array(t, vm().primitive_array_class(BasicType::Int).expect("[I"), offsets.len() as i32)?;
        // SAFETY: a fresh int[] of the same length.
        unsafe { obj::array_slice::<i32>(arr).copy_from_slice(&offsets) };
        ret_ref(arr)
    });
    r.add(W, "inotifyInit", "()I", |t, _| {
        // SAFETY: a plain system call.
        let fd = check(t, unsafe { libc::inotify_init() } as i64)?;
        ret_int(fd as i32)
    });
    r.add(W, "inotifyAddWatch", "(IJI)I", |t, a| {
        // SAFETY: the path is a NUL-terminated native string (its address).
        let wd = check(t, unsafe { libc::inotify_add_watch(a[0] as i32, a[1] as *const libc::c_char, a[3] as u32) } as i64)?;
        ret_int(wd as i32)
    });
    r.add(W, "inotifyRmWatch", "(II)V", |t, a| {
        // SAFETY: a plain system call.
        check(t, unsafe { libc::inotify_rm_watch(a[0] as i32, a[1] as i32) } as i64)?;
        ret_void()
    });
    r.add(W, "configureBlocking", "(IZ)V", |t, a| {
        let fd = a[0] as i32;
        // SAFETY: plain fcntl calls on the caller's descriptor.
        unsafe {
            let flags = check(t, libc::fcntl(fd, libc::F_GETFL) as i64)? as i32;
            let flags = if a[1] != 0 { flags & !libc::O_NONBLOCK } else { flags | libc::O_NONBLOCK };
            check(t, libc::fcntl(fd, libc::F_SETFL, flags) as i64)?;
        }
        ret_void()
    });
    r.add(W, "socketpair", "([I)V", |t, a| {
        let mut sv = [0i32; 2];
        // SAFETY: a plain system call filling `sv`.
        check(t, unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) } as i64)?;
        // SAFETY: the int[2] argument.
        unsafe { obj::array_slice::<i32>(a[0] as crate::heap::Oop)[..2].copy_from_slice(&sv) };
        ret_void()
    });
    r.add(W, "poll", "(II)I", |t, a| {
        let mut fds = [libc::pollfd { fd: a[0] as i32, events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: a[1] as i32, events: libc::POLLIN, revents: 0 }];
        // SAFETY: two pollfds; blocks outside Java.
        let n = crate::gc::safepoint::blocking(t, || unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) });
        if n == -1 {
            let err = errno();
            if err != libc::EINTR {
                return Err(unix_error(t, err));
            }
            return ret_int(0);
        }
        ret_int(n)
    });
}
