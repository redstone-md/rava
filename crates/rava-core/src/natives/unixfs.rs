//! `sun.nio.fs.UnixNativeDispatcher`: the POSIX file-system calls behind `java.nio.file`.
//! Paths arrive as addresses of NUL-terminated native strings; failures throw
//! `sun.nio.fs.UnixException(errno)`.

use super::{ret_int, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime as rt;
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread, Throw};
use crate::types::BasicType;
use crate::vm::vm;
use std::ffi::CStr;

const D: &str = "sun/nio/fs/UnixNativeDispatcher";

fn cstr(addr: u64) -> *const libc::c_char {
    addr as *const libc::c_char
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

/// `new UnixException(errno)`.
fn unix_error(t: &mut Thread, err: i32) -> Throw {
    match crate::boot::construct(t, "sun/nio/fs/UnixException", "(I)V", &[err as u64]) {
        Ok(e) => Throw(e),
        Err(e) => e,
    }
}

/// Checks a POSIX return value (`-1` means failure with errno).
fn check(t: &mut Thread, r: i64) -> JResult<i64> {
    if r == -1 {
        Err(unix_error(t, errno()))
    } else {
        Ok(r)
    }
}

fn bytes(t: &mut Thread, b: &[u8]) -> JResult<Oop> {
    let arr = rt::alloc_array(t, vm().primitive_array_class(BasicType::Byte).expect("[B"), b.len() as i32)?;
    // SAFETY: fresh byte[] of the same length.
    unsafe { obj::array_slice::<u8>(arr).copy_from_slice(b) };
    Ok(arr)
}

fn set_long(o: Oop, class: &str, field: &str, v: i64) {
    let k = crate::loader::load(class).expect("attrs class");
    let f = k.declared_field(Symbol::intern(field), Symbol::intern("J")).unwrap_or_else(|| panic!("{class}.{field}"));
    // SAFETY: live instance of `class`.
    unsafe { obj::write::<i64>(o, f.offset, v) };
}

fn set_int(o: Oop, class: &str, field: &str, v: i32) {
    let k = crate::loader::load(class).expect("attrs class");
    let f = k.declared_field(Symbol::intern(field), Symbol::intern("I")).unwrap_or_else(|| panic!("{class}.{field}"));
    // SAFETY: live instance of `class`.
    unsafe { obj::write::<i32>(o, f.offset, v) };
}

fn fill_attrs(attrs: Oop, st: &libc::stat) {
    const A: &str = "sun/nio/fs/UnixFileAttributes";
    set_int(attrs, A, "st_mode", st.st_mode as i32);
    set_long(attrs, A, "st_ino", st.st_ino as i64);
    set_long(attrs, A, "st_dev", st.st_dev as i64);
    set_long(attrs, A, "st_rdev", st.st_rdev as i64);
    set_int(attrs, A, "st_nlink", st.st_nlink as i32);
    set_int(attrs, A, "st_uid", st.st_uid as i32);
    set_int(attrs, A, "st_gid", st.st_gid as i32);
    set_long(attrs, A, "st_size", st.st_size);
    set_long(attrs, A, "st_atime_sec", st.st_atime);
    set_long(attrs, A, "st_atime_nsec", st.st_atime_nsec);
    set_long(attrs, A, "st_mtime_sec", st.st_mtime);
    set_long(attrs, A, "st_mtime_nsec", st.st_mtime_nsec);
    set_long(attrs, A, "st_ctime_sec", st.st_ctime);
    set_long(attrs, A, "st_ctime_nsec", st.st_ctime_nsec);
}

fn stat_into(t: &mut Thread, r: i32, st: &libc::stat, attrs: Oop) -> JResult<u64> {
    check(t, r as i64)?;
    fill_attrs(attrs, st);
    ret_void()
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(D, "init", "()I", |_, _| ret_int(0));
    r.add(D, "getcwd", "()[B", |t, _| {
        let cwd = std::env::current_dir().map(|p| p.into_os_string().into_encoded_bytes()).unwrap_or_default();
        ret_ref(bytes(t, &cwd)?)
    });
    r.add(D, "dup", "(I)I", |t, a| ret_int(check(t, unsafe { libc::dup(a[0] as i32) } as i64)? as i32));
    r.add(D, "open0", "(JII)I", |t, a| {
        let fd = unsafe { libc::open(cstr(a[0]), a[2] as i32 | libc::O_CLOEXEC, a[3] as u32) };
        ret_int(check(t, fd as i64)? as i32)
    });
    r.add(D, "close0", "(I)V", |t, a| {
        check(t, unsafe { libc::close(a[0] as i32) } as i64)?;
        ret_void()
    });
    r.add(D, "link0", "(JJ)V", |t, a| { check(t, unsafe { libc::link(cstr(a[0]), cstr(a[2])) } as i64)?; ret_void() });
    r.add(D, "unlink0", "(J)V", |t, a| { check(t, unsafe { libc::unlink(cstr(a[0])) } as i64)?; ret_void() });
    r.add(D, "rename0", "(JJ)V", |t, a| { check(t, unsafe { libc::rename(cstr(a[0]), cstr(a[2])) } as i64)?; ret_void() });
    r.add(D, "mkdir0", "(JI)V", |t, a| { check(t, unsafe { libc::mkdir(cstr(a[0]), a[2] as u32) } as i64)?; ret_void() });
    r.add(D, "rmdir0", "(J)V", |t, a| { check(t, unsafe { libc::rmdir(cstr(a[0])) } as i64)?; ret_void() });
    r.add(D, "symlink0", "(JJ)V", |t, a| { check(t, unsafe { libc::symlink(cstr(a[0]), cstr(a[2])) } as i64)?; ret_void() });
    r.add(D, "chmod0", "(JI)V", |t, a| { check(t, unsafe { libc::chmod(cstr(a[0]), a[2] as u32) } as i64)?; ret_void() });
    r.add(D, "fchmod0", "(II)V", |t, a| { check(t, unsafe { libc::fchmod(a[0] as i32, a[1] as u32) } as i64)?; ret_void() });
    r.add(D, "readlink0", "(J)[B", |t, a| {
        let mut buf = vec![0u8; 4096];
        let n = check(t, unsafe { libc::readlink(cstr(a[0]), buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } as i64)?;
        ret_ref(bytes(t, &buf[..n as usize])?)
    });
    r.add(D, "realpath0", "(J)[B", |t, a| {
        let p = unsafe { libc::realpath(cstr(a[0]), std::ptr::null_mut()) };
        if p.is_null() {
            return Err(unix_error(t, errno()));
        }
        let v = unsafe { CStr::from_ptr(p) }.to_bytes().to_vec();
        unsafe { libc::free(p as *mut libc::c_void) };
        ret_ref(bytes(t, &v)?)
    });
    r.add(D, "stat0", "(JLsun/nio/fs/UnixFileAttributes;)I", |_, a| {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::stat(cstr(a[0]), &mut st) } == -1 {
            return ret_int(errno());
        }
        fill_attrs(a[2] as Oop, &st);
        ret_int(0)
    });
    r.add(D, "lstat0", "(JLsun/nio/fs/UnixFileAttributes;)V", |t, a| {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::lstat(cstr(a[0]), &mut st) };
        stat_into(t, r, &st, a[2] as Oop)
    });
    r.add(D, "fstat0", "(ILsun/nio/fs/UnixFileAttributes;)V", |t, a| {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::fstat(a[0] as i32, &mut st) };
        stat_into(t, r, &st, a[1] as Oop)
    });
    r.add(D, "opendir0", "(J)J", |t, a| {
        let d = unsafe { libc::opendir(cstr(a[0])) };
        if d.is_null() {
            return Err(unix_error(t, errno()));
        }
        ret_long(d as i64)
    });
    r.add(D, "fdopendir", "(I)J", |t, a| {
        let d = unsafe { libc::fdopendir(a[0] as i32) };
        if d.is_null() {
            return Err(unix_error(t, errno()));
        }
        ret_long(d as i64)
    });
    r.add(D, "closedir", "(J)V", |t, a| { check(t, unsafe { libc::closedir(a[0] as *mut libc::DIR) } as i64)?; ret_void() });
    r.add(D, "readdir0", "(J)[B", |t, a| loop {
        // SAFETY: a DIR* from opendir0; errno distinguishes the end from a failure.
        unsafe { *libc::__errno_location() = 0 };
        let e = unsafe { libc::readdir(a[0] as *mut libc::DIR) };
        if e.is_null() {
            let err = errno();
            return if err == 0 { ret_ref(std::ptr::null_mut()) } else { Err(unix_error(t, err)) };
        }
        let name = unsafe { CStr::from_ptr((*e).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            return ret_ref(bytes(t, name)?);
        }
    });
    r.add(D, "read0", "(IJI)I", |t, a| {
        ret_int(check(t, unsafe { libc::read(a[0] as i32, a[1] as *mut libc::c_void, a[3] as usize) } as i64)? as i32)
    });
    r.add(D, "write0", "(IJI)I", |t, a| {
        ret_int(check(t, unsafe { libc::write(a[0] as i32, a[1] as *const libc::c_void, a[3] as usize) } as i64)? as i32)
    });
    r.add(D, "access0", "(JI)I", |_, a| {
        ret_int(if unsafe { libc::access(cstr(a[0]), a[2] as i32) } == -1 { errno() } else { 0 })
    });
    r.add(D, "strerror", "(I)[B", |t, a| {
        let msg = std::io::Error::from_raw_os_error(a[0] as i32).to_string();
        let msg = msg.split(" (os error").next().unwrap_or("").to_string();
        ret_ref(bytes(t, msg.as_bytes())?)
    });
    r.add(D, "getpwuid", "(I)[B", |t, a| {
        let pw = unsafe { libc::getpwuid(a[0] as u32) };
        let name = if pw.is_null() { a[0].to_string().into_bytes() } else { unsafe { CStr::from_ptr((*pw).pw_name) }.to_bytes().to_vec() };
        ret_ref(bytes(t, &name)?)
    });
    r.add(D, "getgrgid", "(I)[B", |t, a| {
        let gr = unsafe { libc::getgrgid(a[0] as u32) };
        let name = if gr.is_null() { a[0].to_string().into_bytes() } else { unsafe { CStr::from_ptr((*gr).gr_name) }.to_bytes().to_vec() };
        ret_ref(bytes(t, &name)?)
    });
    r.add(D, "statvfs0", "(JLsun/nio/fs/UnixFileStoreAttributes;)V", |t, a| {
        let mut sv: libc::statvfs = unsafe { std::mem::zeroed() };
        check(t, unsafe { libc::statvfs(cstr(a[0]), &mut sv) } as i64)?;
        const S: &str = "sun/nio/fs/UnixFileStoreAttributes";
        let o = a[2] as Oop;
        set_long(o, S, "f_frsize", sv.f_frsize as i64);
        set_long(o, S, "f_blocks", sv.f_blocks as i64);
        set_long(o, S, "f_bfree", sv.f_bfree as i64);
        set_long(o, S, "f_bavail", sv.f_bavail as i64);
        ret_void()
    });
    r.add("java/security/AccessController", "getProtectionDomain", "(Ljava/lang/Class;)Ljava/security/ProtectionDomain;", |_, _| {
        ret_ref(std::ptr::null_mut())
    });
    r.add("jdk/internal/jimage/NativeImageBuffer", "getNativeMap", "(Ljava/lang/String;)Ljava/nio/ByteBuffer;", |t, a| {
        let path = rt::java_string_to_rust(a[0] as Oop);
        let ours = vm().options.java_home.join("lib/modules");
        if std::path::Path::new(&path) != ours {
            return ret_ref(std::ptr::null_mut());
        }
        let (addr, len) = vm().image.mapping();
        let bb = crate::boot::construct(t, "java/nio/DirectByteBuffer", "(JJ)V", &[addr as u64, 0, len as u64, 0])?;
        ret_ref(bb)
    });
}
