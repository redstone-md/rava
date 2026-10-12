//! `java.io` file-descriptor natives (the libjava subset the boot needs), over POSIX calls.

pub(crate) use super::io_shared::{checked_range, errno_text, io_error};
use super::{ret_bool, ret_int, ret_long, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime::{self as rt, throw_new, vm_field};
use crate::thread::{JResult, Thread};

vm_field!(fd_int, "java/io/FileDescriptor", "fd", "I");
vm_field!(fis_fd, "java/io/FileInputStream", "fd", "Ljava/io/FileDescriptor;");
vm_field!(fos_fd, "java/io/FileOutputStream", "fd", "Ljava/io/FileDescriptor;");
vm_field!(raf_fd, "java/io/RandomAccessFile", "fd", "Ljava/io/FileDescriptor;");

const FD: &str = "java/io/FileDescriptor";
const FIS: &str = "java/io/FileInputStream";
const FOS: &str = "java/io/FileOutputStream";

const RAF: &str = "java/io/RandomAccessFile";

pub(super) fn register(r: &mut NativeRegistry) {
    register_raf(r);
    for c in [FD, FIS, FOS] {
        r.add(c, "initIDs", "()V", |_, _| ret_void());
    }
    r.add(FD, "getHandle", "(I)J", |_, _| ret_long(-1));
    r.add(FD, "getAppend", "(I)Z", |_, a| {
        // SAFETY: fcntl on a caller-supplied descriptor.
        let flags = unsafe { libc::fcntl(a[0] as i32, libc::F_GETFL) };
        ret_bool(flags != -1 && flags & libc::O_APPEND != 0)
    });
    r.add(FD, "close0", "()V", |t, a| {
        let fd_obj = a[0] as Oop;
        // SAFETY: live FileDescriptor.
        let fd = unsafe { obj::read::<i32>(fd_obj, fd_int()) };
        if fd >= 0 {
            unsafe { obj::write::<i32>(fd_obj, fd_int(), -1) };
            if fd > 2 && unsafe { libc::close(fd) } != 0 {
                return Err(io_error(t, "close failed"));
            }
        }
        ret_void()
    });
    r.add(FD, "sync0", "()V", |_, a| {
        unsafe { libc::fsync(obj::read::<i32>(a[0] as Oop, fd_int())) };
        ret_void()
    });
    r.add(FOS, "writeBytes", "([BIIZ)V", |t, a| {
        let fd = stream_fd(a[0] as Oop, fos_fd());
        let bytes = checked_range(t, a[1] as Oop, a[2] as i32, a[3] as i32)?;
        write_all(t, fd, bytes)?;
        ret_void()
    });
    r.add(FOS, "write", "(IZ)V", |t, a| {
        let fd = stream_fd(a[0] as Oop, fos_fd());
        write_all(t, fd, &[a[1] as u8])?;
        ret_void()
    });
    r.add(FOS, "open0", "(Ljava/lang/String;Z)V", |t, a| {
        let flags = libc::O_WRONLY | libc::O_CREAT | if a[2] != 0 { libc::O_APPEND } else { libc::O_TRUNC };
        open_into(t, a[0] as Oop, fos_fd(), a[1] as Oop, flags)
    });
    r.add(FIS, "open0", "(Ljava/lang/String;)V", |t, a| open_into(t, a[0] as Oop, fis_fd(), a[1] as Oop, libc::O_RDONLY));
    r.add(FIS, "readBytes", "([BII)I", |t, a| {
        let fd = stream_fd(a[0] as Oop, fis_fd());
        let buf = checked_range(t, a[1] as Oop, a[2] as i32, a[3] as i32)?;
        if buf.is_empty() {
            return ret_int(0);
        }
        // Reads block (e.g. on a console): the thread is outside Java meanwhile. The array is
        // held by the calling frame, so the buffer stays valid.
        let (ptr, len) = (buf.as_ptr() as usize, buf.len());
        let n = crate::gc::safepoint::blocking(t, || unsafe { libc::read(fd, ptr as *mut libc::c_void, len) });
        match n {
            0 => ret_int(-1),
            n if n < 0 => Err(io_error(t, "Read error")),
            n => ret_int(n as i32),
        }
    });
    r.add(FIS, "read0", "()I", |t, a| {
        let fd = stream_fd(a[0] as Oop, fis_fd());
        let mut b = 0u8;
        match unsafe { libc::read(fd, &mut b as *mut u8 as *mut libc::c_void, 1) } {
            0 => ret_int(-1),
            n if n < 0 => Err(io_error(t, "Read error")),
            _ => ret_int(b as i32),
        }
    });
    r.add(FIS, "available0", "()I", |_, a| {
        let fd = stream_fd(a[0] as Oop, fis_fd());
        let mut n: libc::c_int = 0;
        unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) };
        ret_int(n.max(0))
    });
    r.add(FIS, "length0", "()J", |t, a| {
        let fd = stream_fd(a[0] as Oop, fis_fd());
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            return Err(io_error(t, "fstat failed"));
        }
        ret_long(st.st_size)
    });
    r.add(FIS, "position0", "()J", |t, a| {
        let fd = stream_fd(a[0] as Oop, fis_fd());
        match unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) } {
            -1 => Err(io_error(t, "lseek failed")),
            p => ret_long(p),
        }
    });
    r.add(FIS, "skip0", "(J)J", |t, a| {
        let fd = stream_fd(a[0] as Oop, fis_fd());
        let cur = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) };
        let end = unsafe { libc::lseek(fd, a[1] as i64, libc::SEEK_CUR) };
        if cur == -1 || end == -1 {
            return Err(io_error(t, "Seek error"));
        }
        ret_long(end - cur)
    });
    r.add(FIS, "isRegularFile0", "(Ljava/io/FileDescriptor;)Z", |_, a| {
        let fd = unsafe { obj::read::<i32>(a[1] as Oop, fd_int()) };
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        ret_bool(unsafe { libc::fstat(fd, &mut st) } == 0 && st.st_mode & libc::S_IFMT == libc::S_IFREG)
    });
}

fn stream_fd(stream: Oop, field: u32) -> i32 {
    // SAFETY: live stream with a FileDescriptor.
    unsafe {
        let fdo = obj::read_ref(stream, field);
        if fdo.is_null() { -1 } else { obj::read::<i32>(fdo, fd_int()) }
    }
}

pub(super) fn write_all(t: &mut Thread, fd: i32, mut bytes: &[u8]) -> JResult<()> {
    while !bytes.is_empty() {
        // SAFETY: valid byte range.
        let n = unsafe { libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io_error(t, "Write error"));
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

pub(super) fn open_into(t: &mut Thread, stream: Oop, field: u32, path: Oop, flags: i32) -> JResult<u64> {
    let name = rt::java_string_to_rust(path);
    let c = std::ffi::CString::new(name.clone()).map_err(|_| throw_new(t, "java/io/FileNotFoundException", &name))?;
    // SAFETY: NUL-terminated path.
    let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC, 0o666) };
    if fd < 0 {
        let msg = format!("{name} ({})", errno_text());
        return Err(throw_new(t, "java/io/FileNotFoundException", &msg));
    }
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } == 0 && st.st_mode & libc::S_IFMT == libc::S_IFDIR {
        unsafe { libc::close(fd) };
        return Err(throw_new(t, "java/io/FileNotFoundException", &format!("{name} (Is a directory)")));
    }
    // SAFETY: live stream whose FileDescriptor was created by the constructor.
    unsafe { obj::write::<i32>(obj::read_ref(stream, field), fd_int(), fd) };
    ret_void()
}

fn register_raf(r: &mut NativeRegistry) {
    r.add(RAF, "initIDs", "()V", |_, _| ret_void());
    r.add(RAF, "open0", "(Ljava/lang/String;I)V", |t, a| {
        let mode = a[2] as i32;
        let flags = if mode & 2 != 0 { libc::O_RDWR | libc::O_CREAT } else { libc::O_RDONLY }
            | if mode & 4 != 0 { libc::O_SYNC } else { 0 }
            | if mode & 8 != 0 { libc::O_DSYNC } else { 0 };
        open_into(t, a[0] as Oop, raf_fd(), a[1] as Oop, flags)
    });
    r.add(RAF, "read0", "()I", |t, a| {
        let fd = stream_fd(a[0] as Oop, raf_fd());
        let mut b = 0u8;
        match unsafe { libc::read(fd, &mut b as *mut u8 as *mut libc::c_void, 1) } {
            0 => ret_int(-1),
            n if n < 0 => Err(io_error(t, "Read error")),
            _ => ret_int(b as i32),
        }
    });
    r.add(RAF, "readBytes0", "([BII)I", |t, a| {
        let fd = stream_fd(a[0] as Oop, raf_fd());
        let buf = checked_range(t, a[1] as Oop, a[2] as i32, a[3] as i32)?;
        if buf.is_empty() {
            return ret_int(0);
        }
        match unsafe { libc::read(fd, buf.as_ptr() as *mut libc::c_void, buf.len()) } {
            0 => ret_int(-1),
            n if n < 0 => Err(io_error(t, "Read error")),
            n => ret_int(n as i32),
        }
    });
    r.add(RAF, "write0", "(I)V", |t, a| {
        write_all(t, stream_fd(a[0] as Oop, raf_fd()), &[a[1] as u8])?;
        ret_void()
    });
    r.add(RAF, "writeBytes0", "([BII)V", |t, a| {
        let bytes = checked_range(t, a[1] as Oop, a[2] as i32, a[3] as i32)?;
        write_all(t, stream_fd(a[0] as Oop, raf_fd()), bytes)?;
        ret_void()
    });
    r.add(RAF, "getFilePointer", "()J", |t, a| {
        match unsafe { libc::lseek(stream_fd(a[0] as Oop, raf_fd()), 0, libc::SEEK_CUR) } {
            -1 => Err(io_error(t, "Seek failed")),
            p => ret_long(p),
        }
    });
    r.add(RAF, "seek0", "(J)V", |t, a| {
        match unsafe { libc::lseek(stream_fd(a[0] as Oop, raf_fd()), a[1] as i64, libc::SEEK_SET) } {
            -1 => Err(io_error(t, "Seek failed")),
            _ => ret_void(),
        }
    });
    r.add(RAF, "length0", "()J", |t, a| {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(stream_fd(a[0] as Oop, raf_fd()), &mut st) } != 0 {
            return Err(io_error(t, "fstat failed"));
        }
        ret_long(st.st_size)
    });
    r.add(RAF, "setLength0", "(J)V", |t, a| {
        if unsafe { libc::ftruncate(stream_fd(a[0] as Oop, raf_fd()), a[1] as i64) } != 0 {
            return Err(io_error(t, "setLength failed"));
        }
        ret_void()
    });
}
