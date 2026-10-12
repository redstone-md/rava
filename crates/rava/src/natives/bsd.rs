//! Natives of the macOS class library only: the `KQueue` selector, `sun.nio.fs.BsdNativeDispatcher`
//! (the mount table, `clonefile`, `setattrlist`), path normalization, `F_FULLFSYNC` and the
//! macOS socket options, as the JDK's own C code does them.

use super::io::{fd_int, io_error};
use super::net::{check, errno as net_errno, INTERRUPTED};
use super::unixfs::{cstr, errno, unix_error};
use super::unixfs_ext::UNSUPPORTED_CASE;
use super::{byte_array, ret_bool, ret_int, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime as rt;
use crate::thread::{JResult, Thread};
use crate::types::BasicType;
use crate::vm::vm;

crate::runtime::vm_field!(mnt_name, "sun/nio/fs/UnixMountEntry", "name", "[B");
crate::runtime::vm_field!(mnt_dir, "sun/nio/fs/UnixMountEntry", "dir", "[B");
crate::runtime::vm_field!(mnt_fstype, "sun/nio/fs/UnixMountEntry", "fstype", "[B");
crate::runtime::vm_field!(mnt_opts, "sun/nio/fs/UnixMountEntry", "opts", "[B");
crate::runtime::vm_field!(mnt_dev, "sun/nio/fs/UnixMountEntry", "dev", "J");

const KQ: &str = "sun/nio/ch/KQueue";
const BSD: &str = "sun/nio/fs/BsdNativeDispatcher";
const SOCKOPT: &str = "jdk/net/MacOSXSocketOptions";

/// The mount table `getfsstat` read, walked by `fsstatEntry`.
struct FsStat {
    entries: Vec<libc::statfs>,
    pos: usize,
}

fn c_bytes(s: &[libc::c_char]) -> Vec<u8> {
    s.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect()
}

fn kqueue(r: &mut NativeRegistry) {
    r.add(KQ, "keventSize", "()I", |_, _| ret_int(std::mem::size_of::<libc::kevent>() as i32));
    r.add(KQ, "identOffset", "()I", |_, _| ret_int(std::mem::offset_of!(libc::kevent, ident) as i32));
    r.add(KQ, "filterOffset", "()I", |_, _| ret_int(std::mem::offset_of!(libc::kevent, filter) as i32));
    r.add(KQ, "flagsOffset", "()I", |_, _| ret_int(std::mem::offset_of!(libc::kevent, flags) as i32));
    r.add(KQ, "create", "()I", |t, _| ret_int(check(t, unsafe { libc::kqueue() }, "kqueue failed")?));
    // Returns 0 or the errno of the change.
    r.add(KQ, "register", "(IIII)I", |_, a| {
        // SAFETY: an all-zero kevent is valid; one change and no events.
        let mut ev: libc::kevent = unsafe { std::mem::zeroed() };
        ev.ident = a[1] as i32 as usize;
        ev.filter = a[2] as i16;
        ev.flags = a[3] as u16;
        let r = unsafe { libc::kevent(a[0] as i32, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        ret_int(if r == -1 { net_errno() } else { 0 })
    });
    // Waits up to `timeout` ms (forever when negative) for events into the buffer at `address`.
    r.add(KQ, "poll", "(IJIJ)I", |t, a| {
        let (kq, events, n, timeout) = (a[0] as i32, a[1] as usize, a[3] as i32, a[4] as i64);
        let ts = libc::timespec { tv_sec: timeout / 1000, tv_nsec: (timeout % 1000) * 1_000_000 };
        let tsp = if timeout < 0 { 0 } else { &ts as *const libc::timespec as usize };
        let r = crate::gc::safepoint::blocking(t, || unsafe {
            libc::kevent(kq, std::ptr::null(), 0, events as *mut libc::kevent, n, tsp as *const libc::timespec)
        });
        if r < 0 {
            return if net_errno() == libc::EINTR { ret_int(INTERRUPTED) } else { Err(io_error(t, "kqueue failed")) };
        }
        ret_int(r)
    });
}

fn mount_table(r: &mut NativeRegistry) {
    r.add(BSD, "initIDs", "()V", |_, _| ret_void());
    r.add(BSD, "getfsstat", "()J", |t, _| {
        // SAFETY: a count query, then a fill of that many entries.
        let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        if n < 0 {
            return Err(unix_error(t, errno()));
        }
        let mut entries: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; n as usize];
        let size = (entries.len() * std::mem::size_of::<libc::statfs>()) as libc::c_int;
        let got = unsafe { libc::getfsstat(entries.as_mut_ptr(), size, libc::MNT_NOWAIT) };
        if got < 0 {
            return Err(unix_error(t, errno()));
        }
        entries.truncate(got as usize);
        ret_long(Box::into_raw(Box::new(FsStat { entries, pos: 0 })) as i64)
    });
    r.add(BSD, "fsstatEntry", "(JLsun/nio/fs/UnixMountEntry;)I", |t, a| {
        // SAFETY: an iterator from getfsstat, not yet ended.
        let it = unsafe { &mut *(a[0] as *mut FsStat) };
        let Some(sfs) = it.entries.get(it.pos) else { return ret_int(-1) };
        it.pos += 1;
        let entry = a[2] as Oop;
        let opts: &[u8] = if sfs.f_flags & libc::MNT_RDONLY as u32 != 0 { b"ro" } else { b"rw" };
        let fields = [
            (mnt_name(), c_bytes(&sfs.f_mntfromname)),
            (mnt_dir(), c_bytes(&sfs.f_mntonname)),
            (mnt_fstype(), c_bytes(&sfs.f_fstypename)),
            (mnt_opts(), opts.to_vec()),
        ];
        for (f, v) in fields {
            let arr = byte_array(t, Some(&v))?;
            // SAFETY: a live UnixMountEntry.
            unsafe { obj::write_ref(entry, f, arr) };
        }
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let dev = if unsafe { libc::stat(sfs.f_mntonname.as_ptr(), &mut st) } == 0 { st.st_dev as i64 } else { 0 };
        // SAFETY: a live UnixMountEntry.
        unsafe { obj::write::<i64>(entry, mnt_dev(), dev) };
        ret_int(0)
    });
    r.add(BSD, "endfsstat", "(J)V", |_, a| {
        if a[0] != 0 {
            // SAFETY: the iterator getfsstat leaked, ended once.
            drop(unsafe { Box::from_raw(a[0] as *mut FsStat) });
        }
        ret_void()
    });
    r.add(BSD, "getmntonname0", "(J)[B", |t, a| {
        let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(cstr(a[0]), &mut sfs) } != 0 {
            return Err(unix_error(t, errno()));
        }
        ret_ref(byte_array(t, Some(&c_bytes(&sfs.f_mntonname)))?)
    });
}

/// Where `setattrlist` applies: a path or an open file.
enum Target {
    Path(u64),
    Fd(i32),
}

/// `setattrlist`/`fsetattrlist` of the creation, modification and access times among
/// `commonattr` (nanoseconds), in the order the attribute bits go. `a` holds the attributes,
/// then the modification, access and creation times and the options, each two slots.
fn set_times(t: &mut Thread, target: Target, a: &[u64]) -> JResult<u64> {
    const CRTIME: u32 = 0x0000_0200;
    const MODTIME: u32 = 0x0000_0400;
    const ACCTIME: u32 = 0x0000_1000;
    let commonattr = a[0] as u32;
    let (modtime, acctime, crtime, options) = (a[1] as i64, a[3] as i64, a[5] as i64, a[7] as u32);
    let mut times = Vec::new();
    for (bit, nanos) in [(CRTIME, crtime), (MODTIME, modtime), (ACCTIME, acctime)] {
        if commonattr & bit != 0 {
            times.push(libc::timespec { tv_sec: nanos.div_euclid(1_000_000_000), tv_nsec: nanos.rem_euclid(1_000_000_000) });
        }
    }
    // SAFETY: an all-zero attrlist is valid; the fields that matter are set below.
    let mut list: libc::attrlist = unsafe { std::mem::zeroed() };
    list.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    list.commonattr = commonattr;
    let (buf, len) = (times.as_mut_ptr() as *mut libc::c_void, times.len() * std::mem::size_of::<libc::timespec>());
    let list_ptr = &mut list as *mut libc::attrlist as *mut libc::c_void;
    let r = match target {
        Target::Fd(fd) => unsafe { libc::fsetattrlist(fd, list_ptr, buf, len, options) },
        Target::Path(p) => unsafe { libc::setattrlist(cstr(p), list_ptr, buf, len, options) },
    };
    if r != 0 {
        return Err(unix_error(t, errno()));
    }
    ret_void()
}

fn files(r: &mut NativeRegistry) {
    r.add(BSD, "clonefile0", "(JJI)I", |_, a| {
        let r = unsafe { libc::clonefile(cstr(a[0]), cstr(a[2]), a[4] as u32) };
        ret_int(if r == 0 { 0 } else { errno() })
    });
    r.add(BSD, "setattrlist0", "(JIJJJJ)V", |t, a| set_times(t, Target::Path(a[0]), &a[2..]));
    r.add(BSD, "fsetattrlist0", "(IIJJJJ)V", |t, a| set_times(t, Target::Fd(a[0] as i32), &a[1..]));
    // No kernel-side copy without clonefile: the caller falls back to a buffered copy.
    r.add("sun/nio/fs/BsdFileSystem", "directCopy0", "(IIJ)I", |_, _| ret_int(UNSUPPORTED_CASE));
    r.add("sun/nio/fs/MacOSXNativeDispatcher", "normalizepath", "([CI)[C", normalize_path);
    r.add("sun/nio/fs/UTIFileTypeDetector", "probe0", "(Ljava/lang/String;)Ljava/lang/String;", |_, _| ret_ref(std::ptr::null_mut()));
    // Flushes to the storage itself (fsync alone only reaches the drive's cache).
    r.add("sun/nio/ch/FileDispatcherImpl", "force0", "(Ljava/io/FileDescriptor;Z)I", |t, a| {
        // SAFETY: a live FileDescriptor.
        let fd = unsafe { obj::read::<i32>(a[0] as Oop, fd_int()) };
        let mut r = unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) };
        if r == -1 {
            r = unsafe { libc::fsync(fd) };
        }
        if r == -1 {
            return Err(io_error(t, "Force failed"));
        }
        ret_int(0)
    });
    r.add("sun/awt/PlatformGraphicsInfo", "isInAquaSession", "()Z", |_, _| ret_bool(false));
}

/// `CFRange`.
#[repr(C)]
struct Range {
    location: isize,
    length: isize,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateMutable(alloc: *const libc::c_void, max: isize) -> *mut libc::c_void;
    fn CFStringAppendCharacters(s: *mut libc::c_void, chars: *const u16, n: isize);
    fn CFStringNormalize(s: *mut libc::c_void, form: isize);
    fn CFStringGetLength(s: *const libc::c_void) -> isize;
    fn CFStringGetCharacters(s: *const libc::c_void, range: Range, buf: *mut u16);
    fn CFRelease(p: *const libc::c_void);
}

/// `normalizepath(char[] path, int form)`: Unicode normalization (`CFStringNormalize`).
fn normalize_path(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    // SAFETY: a live char[].
    let path: Vec<u16> = unsafe { obj::array_slice::<u16>(a[0] as Oop) }.to_vec();
    // SAFETY: CoreFoundation calls on a string this function owns and releases.
    let out = unsafe {
        let s = CFStringCreateMutable(std::ptr::null(), 0);
        CFStringAppendCharacters(s, path.as_ptr(), path.len() as isize);
        CFStringNormalize(s, a[1] as i32 as isize);
        let n = CFStringGetLength(s);
        let mut out = vec![0u16; n as usize];
        CFStringGetCharacters(s, Range { location: 0, length: n }, out.as_mut_ptr());
        CFRelease(s);
        out
    };
    let arr = rt::alloc_array(t, vm().primitive_array_class(BasicType::Char).expect("[C"), out.len() as i32)?;
    // SAFETY: a fresh char[] of that length.
    unsafe { obj::array_slice::<u16>(arr).copy_from_slice(&out) };
    ret_ref(arr)
}

fn tcp_option(fd: i32, opt: i32) -> i32 {
    let mut v: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: reads one int option.
    unsafe { libc::getsockopt(fd, libc::IPPROTO_TCP, opt, &mut v as *mut _ as *mut libc::c_void, &mut len) };
    v
}

fn set_tcp_option(t: &mut Thread, fd: i32, opt: i32, v: i32) -> JResult<u64> {
    // SAFETY: writes one int option.
    let r = unsafe { libc::setsockopt(fd, libc::IPPROTO_TCP, opt, &v as *const _ as *const libc::c_void, 4) };
    if r != 0 {
        return Err(io_error(t, "set option failed"));
    }
    ret_void()
}

fn socket_options(r: &mut NativeRegistry) {
    r.add(SOCKOPT, "keepAliveOptionsSupported0", "()Z", |_, _| ret_bool(true));
    r.add(SOCKOPT, "ipDontFragmentSupported0", "()Z", |_, _| ret_bool(false));
    r.add(SOCKOPT, "setTcpKeepAliveTime0", "(II)V", |t, a| set_tcp_option(t, a[0] as i32, libc::TCP_KEEPALIVE, a[1] as i32));
    r.add(SOCKOPT, "setTcpKeepAliveIntvl0", "(II)V", |t, a| set_tcp_option(t, a[0] as i32, libc::TCP_KEEPINTVL, a[1] as i32));
    r.add(SOCKOPT, "setTcpKeepAliveProbes0", "(II)V", |t, a| set_tcp_option(t, a[0] as i32, libc::TCP_KEEPCNT, a[1] as i32));
    r.add(SOCKOPT, "getTcpKeepAliveTime0", "(I)I", |_, a| ret_int(tcp_option(a[0] as i32, libc::TCP_KEEPALIVE)));
    r.add(SOCKOPT, "getTcpKeepAliveIntvl0", "(I)I", |_, a| ret_int(tcp_option(a[0] as i32, libc::TCP_KEEPINTVL)));
    r.add(SOCKOPT, "getTcpKeepAliveProbes0", "(I)I", |_, a| ret_int(tcp_option(a[0] as i32, libc::TCP_KEEPCNT)));
    r.add(SOCKOPT, "getSoPeerCred0", "(I)J", |_, _| ret_long(-1));
}

pub(super) fn register(r: &mut NativeRegistry) {
    kqueue(r);
    mount_table(r);
    files(r);
    socket_options(r);
}
