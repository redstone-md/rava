//! `sun.nio.ch` file-channel natives: `IOUtil`, `UnixFileDispatcherImpl`, `FileDispatcherImpl`,
//! `NativeThread`, `FileKey`. Results follow `sun.nio.ch.IOStatus` (-1 EOF, -2 unavailable,
//! -3 interrupted); failures throw `IOException`.

use super::io::{fd_int, io_error};
use super::{ret_bool, ret_int, ret_long, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::thread::{JResult, Thread};

const IOUTIL: &str = "sun/nio/ch/IOUtil";
const UFD: &str = "sun/nio/ch/UnixFileDispatcherImpl";
const EOF: i64 = -1;
const UNAVAILABLE: i64 = -2;
const INTERRUPTED: i64 = -3;

fn fd(fdo: u64) -> i32 {
    // SAFETY: a live java.io.FileDescriptor.
    unsafe { obj::read::<i32>(fdo as Oop, fd_int()) }
}

/// Converts a POSIX result into an IOStatus value (0 bytes read means EOF when `eof`).
fn status(t: &mut Thread, n: i64, eof: bool) -> JResult<i64> {
    if n > 0 || (n == 0 && !eof) {
        return Ok(n);
    }
    if n == 0 {
        return Ok(EOF);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EAGAIN) => Ok(UNAVAILABLE),
        Some(libc::EINTR) => Ok(INTERRUPTED),
        _ => Err(io_error(t, "I/O error")),
    }
}

crate::runtime::vm_field!(key_dev, "sun/nio/ch/FileKey", "st_dev", "J");
crate::runtime::vm_field!(key_ino, "sun/nio/ch/FileKey", "st_ino", "J");

pub(super) fn register(r: &mut NativeRegistry) {
    r.add("sun/nio/ch/FileKey", "initIDs", "()V", |_, _| ret_void());
    // JDK 25's `FileChannelImpl.available`: what remains of a regular file past its position,
    // else what a read could return without blocking.
    r.add(UFD, "available0", "(Ljava/io/FileDescriptor;)I", |_, a| {
        let fd = fd(a[0]);
        // SAFETY: plain system calls on a descriptor.
        let n = unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstat(fd, &mut st) == 0 && st.st_mode & libc::S_IFMT == libc::S_IFREG {
                let pos = libc::lseek(fd, 0, libc::SEEK_CUR);
                (st.st_size - pos.max(0)).max(0)
            } else {
                let mut n: libc::c_int = 0;
                libc::ioctl(fd, libc::FIONREAD, &mut n);
                n.max(0) as i64
            }
        };
        super::ret_int(n.min(i32::MAX as i64) as i32)
    });
    r.add("sun/nio/ch/FileKey", "init", "(Ljava/io/FileDescriptor;)V", |t, a| {
        // The identity of the open file (FileLockTable keys locks by it).
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd(a[1]), &mut st) } == -1 {
            return Err(io_error(t, "fstat failed"));
        }
        // SAFETY: a live FileKey.
        unsafe {
            obj::write::<i64>(a[0] as Oop, key_dev(), st.st_dev as i64);
            obj::write::<i64>(a[0] as Oop, key_ino(), st.st_ino as i64);
        }
        ret_void()
    });
    // JDK 25: static, filling `finfo` with {st_dev, st_ino}.
    r.add("sun/nio/ch/FileKey", "init", "(Ljava/io/FileDescriptor;[J)V", |t, a| {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd(a[0]), &mut st) } == -1 {
            return Err(io_error(t, "fstat failed"));
        }
        let info = a[1] as Oop;
        // SAFETY: a long[2] allocated by FileKey.create.
        unsafe {
            let base = info as *mut u8;
            std::ptr::write_unaligned(base.add(crate::heap::ARRAY_BASE_OFFSET as usize) as *mut i64, st.st_dev as i64);
            std::ptr::write_unaligned(base.add(crate::heap::ARRAY_BASE_OFFSET as usize + 8) as *mut i64, st.st_ino as i64);
        }
        ret_void()
    });
    r.add(IOUTIL, "initIDs", "()V", |_, _| ret_void());
    r.add(IOUTIL, "fdVal", "(Ljava/io/FileDescriptor;)I", |_, a| ret_int(fd(a[0])));
    r.add(IOUTIL, "setfdVal", "(Ljava/io/FileDescriptor;I)V", |_, a| {
        // SAFETY: a live FileDescriptor.
        unsafe { obj::write::<i32>(a[0] as Oop, fd_int(), a[1] as i32) };
        ret_void()
    });
    r.add(IOUTIL, "fdLimit", "()I", |_, _| {
        let mut rl: libc::rlimit = unsafe { std::mem::zeroed() };
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) };
        ret_int(rl.rlim_cur.min(i32::MAX as u64) as i32)
    });
    r.add(IOUTIL, "iovMax", "()I", |_, _| ret_int(1024));
    r.add(IOUTIL, "writevMax", "()J", |_, _| ret_long(i32::MAX as i64));
    r.add(IOUTIL, "configureBlocking", "(Ljava/io/FileDescriptor;Z)V", |t, a| {
        let f = fd(a[0]);
        let flags = unsafe { libc::fcntl(f, libc::F_GETFL) };
        let flags = if a[1] != 0 { flags & !libc::O_NONBLOCK } else { flags | libc::O_NONBLOCK };
        if unsafe { libc::fcntl(f, libc::F_SETFL, flags) } < 0 {
            return Err(io_error(t, "Configure blocking failed"));
        }
        ret_void()
    });
    r.add(IOUTIL, "randomBytes", "([B)Z", |_, _| ret_bool(false));
    r.add(IOUTIL, "makePipe", "(Z)J", |t, a| {
        let mut fds = [0i32; 2];
        if super::posix::pipe(&mut fds) != 0 {
            return Err(io_error(t, "Pipe failed"));
        }
        if a[0] == 0 {
            for f in fds {
                unsafe { libc::fcntl(f, libc::F_SETFL, libc::fcntl(f, libc::F_GETFL) | libc::O_NONBLOCK) };
            }
        }
        ret_long((fds[0] as i64) << 32 | fds[1] as i64)
    });
    r.add(IOUTIL, "write1", "(IB)I", |t, a| {
        let b = a[1] as u8;
        let n = unsafe { libc::write(a[0] as i32, &b as *const u8 as *const libc::c_void, 1) } as i64;
        ret_int(status(t, n, false)? as i32)
    });
    r.add(IOUTIL, "drain1", "(I)I", |t, a| {
        let mut b = 0u8;
        let n = unsafe { libc::read(a[0] as i32, &mut b as *mut u8 as *mut libc::c_void, 1) } as i64;
        ret_int(status(t, n, true)? as i32)
    });
    r.add(IOUTIL, "drain", "(I)Z", |_, a| {
        let mut buf = [0u8; 128];
        let mut any = false;
        while unsafe { libc::read(a[0] as i32, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) } > 0 {
            any = true;
        }
        ret_bool(any)
    });
    r.add(UFD, "read0", "(Ljava/io/FileDescriptor;JI)I", |t, a| {
        let n = unsafe { libc::read(fd(a[0]), a[1] as *mut libc::c_void, a[3] as u32 as usize) } as i64;
        ret_int(status(t, n, true)? as i32)
    });
    r.add(UFD, "pread0", "(Ljava/io/FileDescriptor;JIJ)I", |t, a| {
        let n = unsafe { libc::pread(fd(a[0]), a[1] as *mut libc::c_void, a[3] as u32 as usize, a[4] as i64) } as i64;
        ret_int(status(t, n, true)? as i32)
    });
    r.add(UFD, "readv0", "(Ljava/io/FileDescriptor;JI)J", |t, a| {
        let n = unsafe { libc::readv(fd(a[0]), a[1] as *const libc::iovec, a[3] as i32) } as i64;
        ret_long(status(t, n, true)?)
    });
    r.add(UFD, "write0", "(Ljava/io/FileDescriptor;JI)I", |t, a| {
        let n = unsafe { libc::write(fd(a[0]), a[1] as *const libc::c_void, a[3] as u32 as usize) } as i64;
        ret_int(status(t, n, false)? as i32)
    });
    r.add(UFD, "pwrite0", "(Ljava/io/FileDescriptor;JIJ)I", |t, a| {
        let n = unsafe { libc::pwrite(fd(a[0]), a[1] as *const libc::c_void, a[3] as u32 as usize, a[4] as i64) } as i64;
        ret_int(status(t, n, false)? as i32)
    });
    r.add(UFD, "writev0", "(Ljava/io/FileDescriptor;JI)J", |t, a| {
        let n = unsafe { libc::writev(fd(a[0]), a[1] as *const libc::iovec, a[3] as i32) } as i64;
        ret_long(status(t, n, false)?)
    });
    r.add(UFD, "force0", "(Ljava/io/FileDescriptor;Z)I", |_, a| ret_int(unsafe { libc::fsync(fd(a[0])) }));
    r.add(UFD, "seek0", "(Ljava/io/FileDescriptor;J)J", |t, a| {
        let whence = if (a[1] as i64) < 0 { libc::SEEK_CUR } else { libc::SEEK_SET };
        let off = if (a[1] as i64) < 0 { 0 } else { a[1] as i64 };
        match unsafe { libc::lseek(fd(a[0]), off, whence) } {
            -1 => Err(io_error(t, "lseek failed")),
            p => ret_long(p),
        }
    });
    r.add(UFD, "truncate0", "(Ljava/io/FileDescriptor;J)I", |t, a| {
        match unsafe { libc::ftruncate(fd(a[0]), a[1] as i64) } {
            -1 => Err(io_error(t, "Truncation failed")),
            _ => ret_int(0),
        }
    });
    r.add(UFD, "size0", "(Ljava/io/FileDescriptor;)J", |t, a| {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd(a[0]), &mut st) } != 0 {
            return Err(io_error(t, "Size failed"));
        }
        ret_long(st.st_size)
    });
    r.add(UFD, "lock0", "(Ljava/io/FileDescriptor;ZJJZ)I", |_, _| ret_int(0)); // FileDispatcher.LOCKED
    r.add(UFD, "release0", "(Ljava/io/FileDescriptor;JJ)V", |_, _| ret_void());
    r.add(UFD, "closeIntFD", "(I)V", |_, a| {
        unsafe { libc::close(a[0] as i32) };
        ret_void()
    });
    r.add(UFD, "allocationGranularity0", "()J", |_, _| ret_long(4096));
    r.add(UFD, "map0", "(Ljava/io/FileDescriptor;IJJZ)J", |t, a| {
        // MapMode: 0 read-only, 1 read-write, 2 private.
        let (prot, flags) = match a[1] {
            0 => (libc::PROT_READ, libc::MAP_SHARED),
            1 => (libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED),
            _ => (libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE),
        };
        let p = unsafe { libc::mmap(std::ptr::null_mut(), a[4] as usize, prot, flags, fd(a[0]), a[2] as i64) };
        if p == libc::MAP_FAILED {
            return Err(io_error(t, "Map failed"));
        }
        ret_long(p as i64)
    });
    r.add(UFD, "unmap0", "(JJ)I", |_, a| ret_int(unsafe { libc::munmap(a[0] as *mut libc::c_void, a[2] as usize) }));
    r.add(UFD, "setDirect0", "(Ljava/io/FileDescriptor;)I", |_, _| ret_int(-1));
    r.add("sun/nio/ch/FileDispatcherImpl", "init0", "()V", |_, _| ret_void());
    r.add(
        "sun/nio/ch/FileDispatcherImpl",
        "transferTo0",
        "(Ljava/io/FileDescriptor;JJLjava/io/FileDescriptor;Z)J",
        |_, _| ret_long(-4), // IOStatus.UNSUPPORTED: the JDK falls back to read/write
    );
    r.add(
        "sun/nio/ch/FileDispatcherImpl",
        "transferFrom0",
        "(Ljava/io/FileDescriptor;Ljava/io/FileDescriptor;JJZ)J",
        |_, _| ret_long(-4),
    );
    r.add("sun/nio/ch/NativeThread", "init", "()V", |_, _| ret_void());
    r.add("sun/nio/ch/NativeThread", "current0", "()J", |_, _| ret_long(0));
    r.add("sun/nio/ch/NativeThread", "signal0", "(J)V", |_, _| ret_void());
    r.add("sun/nio/ch/NativeThread", "supportPendingSignals0", "()Z", |_, _| ret_bool(false));
    r.add("sun/nio/ch/FileKey", "initIDs", "()V", |_, _| ret_void());
}
