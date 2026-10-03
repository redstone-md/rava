//! Networking natives over POSIX sockets (IPv4): `java.net.InetAddress` resolution,
//! `sun.nio.ch.Net`, socket dispatchers, `EPoll` and `EventFD`. Blocking calls (accept, epoll
//! waits, socket reads) run in the native state so the collector can proceed.

use super::io::{fd_int, io_error};
use super::{ret_bool, ret_int, ret_ref, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime::{self as rt, throw_new, vm_field};
use crate::thread::{JResult, Thread};
use crate::types::BasicType;
use crate::vm::vm;

vm_field!(ia_holder, "java/net/InetAddress", "holder", "Ljava/net/InetAddress$InetAddressHolder;");
vm_field!(holder_address, "java/net/InetAddress$InetAddressHolder", "address", "I");

const NET: &str = "sun/nio/ch/Net";
const UNAVAILABLE: i32 = -2;
const INTERRUPTED: i32 = -3;

fn fd(fdo: u64) -> i32 {
    // SAFETY: a live java.io.FileDescriptor.
    unsafe { obj::read::<i32>(fdo as Oop, fd_int()) }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

/// IPv4 address of a `java.net.InetAddress` (host byte order).
fn ipv4_of(addr: Oop) -> u32 {
    // SAFETY: a live Inet4Address and its holder.
    unsafe { obj::read::<i32>(obj::read_ref(addr, ia_holder()), holder_address()) as u32 }
}

fn sockaddr(addr: Oop, port: u64) -> libc::sockaddr_in {
    libc::sockaddr_in {
        sin_family: libc::AF_INET as u16,
        sin_port: (port as u16).to_be(),
        sin_addr: libc::in_addr { s_addr: ipv4_of(addr).to_be() },
        sin_zero: [0; 8],
    }
}

/// `new Inet4Address(host, bytes)`.
fn inet4(t: &mut Thread, host: Option<&str>, ip: u32) -> JResult<Oop> {
    let bytes = rt::alloc_array(t, vm().primitive_array_class(BasicType::Byte).expect("[B"), 4)?;
    // SAFETY: fresh byte[4].
    unsafe { obj::array_slice::<u8>(bytes).copy_from_slice(&ip.to_be_bytes()) };
    let name = match host {
        Some(h) => rt::new_string(t, h)?,
        None => std::ptr::null_mut(),
    };
    crate::boot::construct(t, "java/net/Inet4Address", "(Ljava/lang/String;[B)V", &[name as u64, bytes as u64])
}

fn socket_name(t: &mut Thread, f: i32, peer: bool) -> JResult<(u32, u16)> {
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let p = &mut sa as *mut libc::sockaddr_in as *mut libc::sockaddr;
    let r = unsafe { if peer { libc::getpeername(f, p, &mut len) } else { libc::getsockname(f, p, &mut len) } };
    if r != 0 {
        return Err(io_error(t, "getsockname failed"));
    }
    Ok((u32::from_be(sa.sin_addr.s_addr), u16::from_be(sa.sin_port)))
}

fn check(t: &mut Thread, r: i32, what: &str) -> JResult<i32> {
    if r < 0 {
        Err(io_error(t, what))
    } else {
        Ok(r)
    }
}

pub(super) fn register(r: &mut NativeRegistry) {
    register_inet(r);
    for name in ["isIPv6Available0", "shouldShutdownWriteBeforeClose0", "shouldSetBothIPv4AndIPv6Options0", "canIPv6SocketJoinIPv4Group0", "canJoin6WithIPv4Group0", "canUseIPv6OptionsWithIPv4LocalAddress0"] {
        r.add(NET, name, "()Z", |_, _| ret_bool(false));
    }
    r.add(NET, "isReusePortAvailable0", "()Z", |_, _| ret_bool(true));
    r.add(NET, "isExclusiveBindAvailable", "()I", |_, _| ret_int(-1));
    r.add(NET, "initIDs", "()V", |_, _| ret_void());
    r.add(NET, "pollinValue", "()S", |_, _| ret_int(libc::POLLIN as i32));
    r.add(NET, "polloutValue", "()S", |_, _| ret_int(libc::POLLOUT as i32));
    r.add(NET, "pollerrValue", "()S", |_, _| ret_int(libc::POLLERR as i32));
    r.add(NET, "pollhupValue", "()S", |_, _| ret_int(libc::POLLHUP as i32));
    r.add(NET, "pollnvalValue", "()S", |_, _| ret_int(libc::POLLNVAL as i32));
    r.add(NET, "pollconnValue", "()S", |_, _| ret_int(libc::POLLOUT as i32));
    r.add(NET, "socket0", "(ZZZZ)I", |t, a| {
        let ty = if a[1] != 0 { libc::SOCK_STREAM } else { libc::SOCK_DGRAM };
        let s = check(t, unsafe { libc::socket(libc::AF_INET, ty | libc::SOCK_CLOEXEC, 0) }, "Socket creation failed")?;
        if a[2] != 0 {
            let one: libc::c_int = 1;
            unsafe { libc::setsockopt(s, libc::SOL_SOCKET, libc::SO_REUSEADDR, &one as *const _ as *const libc::c_void, 4) };
        }
        ret_int(s)
    });
    r.add(NET, "bind0", "(Ljava/io/FileDescriptor;ZZLjava/net/InetAddress;I)V", |t, a| {
        let sa = sockaddr(a[3] as Oop, a[4]);
        let r = unsafe { libc::bind(fd(a[0]), &sa as *const _ as *const libc::sockaddr, std::mem::size_of_val(&sa) as u32) };
        if r != 0 {
            return Err(throw_new(t, "java/net/BindException", &super::io::errno_text()));
        }
        ret_void()
    });
    r.add(NET, "listen", "(Ljava/io/FileDescriptor;I)V", |t, a| {
        check(t, unsafe { libc::listen(fd(a[0]), a[1] as i32) }, "Listen failed")?;
        ret_void()
    });
    r.add(NET, "connect0", "(ZLjava/io/FileDescriptor;Ljava/net/InetAddress;I)I", |t, a| {
        let sa = sockaddr(a[2] as Oop, a[3]);
        let r = unsafe { libc::connect(fd(a[1]), &sa as *const _ as *const libc::sockaddr, std::mem::size_of_val(&sa) as u32) };
        match (r, errno()) {
            (0, _) => ret_int(1),
            (_, libc::EINPROGRESS) => ret_int(UNAVAILABLE),
            (_, libc::EINTR) => ret_int(INTERRUPTED),
            _ => Err(throw_new(t, "java/net/ConnectException", &super::io::errno_text())),
        }
    });
    r.add(NET, "accept", "(Ljava/io/FileDescriptor;Ljava/io/FileDescriptor;[Ljava/net/InetSocketAddress;)I", accept);
    r.add(NET, "shutdown", "(Ljava/io/FileDescriptor;I)V", |_, a| {
        unsafe { libc::shutdown(fd(a[0]), a[1] as i32) };
        ret_void()
    });
    r.add(NET, "localPort", "(Ljava/io/FileDescriptor;)I", |t, a| ret_int(socket_name(t, fd(a[0]), false)?.1 as i32));
    r.add(NET, "remotePort", "(Ljava/io/FileDescriptor;)I", |t, a| ret_int(socket_name(t, fd(a[0]), true)?.1 as i32));
    r.add(NET, "localInetAddress", "(Ljava/io/FileDescriptor;)Ljava/net/InetAddress;", |t, a| {
        let (ip, _) = socket_name(t, fd(a[0]), false)?;
        ret_ref(inet4(t, None, ip)?)
    });
    r.add(NET, "remoteInetAddress", "(Ljava/io/FileDescriptor;)Ljava/net/InetAddress;", |t, a| {
        let (ip, _) = socket_name(t, fd(a[0]), true)?;
        ret_ref(inet4(t, None, ip)?)
    });
    r.add(NET, "getIntOption0", "(Ljava/io/FileDescriptor;ZII)I", |t, a| {
        let (mut v, mut len): (libc::c_int, libc::socklen_t) = (0, 4);
        let r = unsafe { libc::getsockopt(fd(a[0]), a[2] as i32, a[3] as i32, &mut v as *mut _ as *mut libc::c_void, &mut len) };
        check(t, r, "getsockopt failed")?;
        ret_int(v)
    });
    r.add(NET, "setIntOption0", "(Ljava/io/FileDescriptor;ZIIIZ)V", |t, a| {
        let (level, opt, arg) = (a[2] as i32, a[3] as i32, a[4] as i32);
        let r = if level == libc::SOL_SOCKET && opt == libc::SO_LINGER {
            let l = libc::linger { l_onoff: (arg >= 0) as i32, l_linger: arg.max(0) };
            unsafe { libc::setsockopt(fd(a[0]), level, opt, &l as *const _ as *const libc::c_void, std::mem::size_of_val(&l) as u32) }
        } else {
            unsafe { libc::setsockopt(fd(a[0]), level, opt, &arg as *const _ as *const libc::c_void, 4) }
        };
        check(t, r, "setsockopt failed")?;
        ret_void()
    });
    r.add(NET, "poll", "(Ljava/io/FileDescriptor;IJ)I", |t, a| {
        let mut p = libc::pollfd { fd: fd(a[0]), events: a[1] as i16, revents: 0 };
        let n = crate::gc::safepoint::blocking(t, || unsafe { libc::poll(&mut p, 1, a[2] as i64 as i32) });
        ret_int(if n > 0 { p.revents as i32 } else { 0 })
    });
    r.add(NET, "pollConnect", "(Ljava/io/FileDescriptor;J)Z", |t, a| {
        let mut p = libc::pollfd { fd: fd(a[0]), events: libc::POLLOUT, revents: 0 };
        let n = crate::gc::safepoint::blocking(t, || unsafe { libc::poll(&mut p, 1, a[1] as i64 as i32) });
        if n <= 0 {
            return ret_bool(false);
        }
        let (mut err, mut len): (libc::c_int, libc::socklen_t) = (0, 4);
        unsafe { libc::getsockopt(fd(a[0]), libc::SOL_SOCKET, libc::SO_ERROR, &mut err as *mut _ as *mut libc::c_void, &mut len) };
        if err != 0 {
            return Err(throw_new(t, "java/net/ConnectException", &std::io::Error::from_raw_os_error(err).to_string()));
        }
        ret_bool(true)
    });
    r.add(NET, "available", "(Ljava/io/FileDescriptor;)I", |_, a| {
        let mut n: libc::c_int = 0;
        unsafe { libc::ioctl(fd(a[0]), libc::FIONREAD, &mut n) };
        ret_int(n)
    });
    register_dispatch(r);
}

fn accept(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (listener, newfd, isaa) = (fd(a[0]), a[1] as Oop, a[2] as Oop);
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let sa_addr = &mut sa as *mut libc::sockaddr_in as usize;
    let len_addr = &mut len as *mut libc::socklen_t as usize;
    let s = crate::gc::safepoint::blocking(t, || unsafe {
        libc::accept4(listener, sa_addr as *mut libc::sockaddr, len_addr as *mut libc::socklen_t, libc::SOCK_CLOEXEC)
    });
    if s < 0 {
        return match errno() {
            libc::EAGAIN => ret_int(UNAVAILABLE),
            libc::EINTR => ret_int(INTERRUPTED),
            _ => Err(io_error(t, "Accept failed")),
        };
    }
    // SAFETY: the new FileDescriptor and the one-element InetSocketAddress[].
    unsafe { obj::write::<i32>(newfd, fd_int(), s) };
    let ip = inet4(t, None, u32::from_be(sa.sin_addr.s_addr))?;
    let isa = crate::boot::construct(t, "java/net/InetSocketAddress", "(Ljava/net/InetAddress;I)V", &[ip as u64, u16::from_be(sa.sin_port) as u64])?;
    unsafe { obj::store_elem(isaa, 0, isa) };
    ret_int(1)
}

/// Reads/writes on sockets and datagram sockets, with IOStatus results.
fn register_dispatch(r: &mut NativeRegistry) {
    fn status(n: isize, read: bool) -> i64 {
        match n {
            0 if read => -1,
            n if n >= 0 => n as i64,
            _ => match errno() {
                libc::EAGAIN => UNAVAILABLE as i64,
                libc::EINTR => INTERRUPTED as i64,
                _ => i64::MIN,
            },
        }
    }
    fn finish(t: &mut Thread, v: i64) -> JResult<u64> {
        if v == i64::MIN {
            let msg = super::io::errno_text();
            return Err(throw_new(t, "java/net/SocketException", &msg));
        }
        Ok(v as u64)
    }
    for c in ["sun/nio/ch/SocketDispatcher", "sun/nio/ch/DatagramDispatcher"] {
        r.add(c, "read0", "(Ljava/io/FileDescriptor;JI)I", |t, a| {
            let n = crate::gc::safepoint::blocking(t, || unsafe { libc::read(fd(a[0]), a[1] as *mut libc::c_void, a[3] as u32 as usize) });
            finish(t, status(n, true) as i32 as i64)
        });
        r.add(c, "readv0", "(Ljava/io/FileDescriptor;JI)J", |t, a| {
            let n = crate::gc::safepoint::blocking(t, || unsafe { libc::readv(fd(a[0]), a[1] as *const libc::iovec, a[3] as i32) });
            finish(t, status(n, true))
        });
        r.add(c, "write0", "(Ljava/io/FileDescriptor;JI)I", |t, a| {
            let n = unsafe { libc::write(fd(a[0]), a[1] as *const libc::c_void, a[3] as u32 as usize) };
            finish(t, status(n, false) as i32 as i64)
        });
        r.add(c, "writev0", "(Ljava/io/FileDescriptor;JI)J", |t, a| {
            let n = unsafe { libc::writev(fd(a[0]), a[1] as *const libc::iovec, a[3] as i32) };
            finish(t, status(n, false))
        });
    }
    r.add("sun/nio/ch/UnixDispatcher", "init", "()V", |_, _| ret_void());
    r.add("sun/nio/ch/UnixDispatcher", "close0", "(Ljava/io/FileDescriptor;)V", |_, a| {
        let f = fd(a[0]);
        if f >= 0 {
            unsafe { libc::close(f) };
        }
        ret_void()
    });
    r.add("sun/nio/ch/UnixDispatcher", "preClose0", "(Ljava/io/FileDescriptor;)V", |_, a| {
        unsafe { libc::shutdown(fd(a[0]), libc::SHUT_RDWR) };
        ret_void()
    });
    const EP: &str = "sun/nio/ch/EPoll";
    r.add(EP, "eventSize", "()I", |_, _| ret_int(std::mem::size_of::<libc::epoll_event>() as i32));
    r.add(EP, "eventsOffset", "()I", |_, _| ret_int(0));
    r.add(EP, "dataOffset", "()I", |_, _| ret_int(4));
    r.add(EP, "create", "()I", |t, _| ret_int(check(t, unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) }, "epoll_create failed")?));
    r.add(EP, "ctl", "(IIII)I", |_, a| {
        let mut ev = libc::epoll_event { events: a[3] as u32, u64: a[2] as i32 as u64 };
        let r = unsafe { libc::epoll_ctl(a[0] as i32, a[1] as i32, a[2] as i32, &mut ev) };
        ret_int(if r == 0 { 0 } else { errno() })
    });
    r.add(EP, "wait", "(IJII)I", |t, a| {
        let n = crate::gc::safepoint::blocking(t, || unsafe { libc::epoll_wait(a[0] as i32, a[1] as *mut libc::epoll_event, a[3] as i32, a[4] as i32) });
        if n < 0 {
            return if errno() == libc::EINTR { ret_int(INTERRUPTED) } else { Err(io_error(t, "epoll_wait failed")) };
        }
        ret_int(n)
    });
    r.add("sun/nio/ch/EventFD", "eventfd0", "()I", |t, _| ret_int(check(t, unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) }, "eventfd failed")?));
    r.add("sun/nio/ch/EventFD", "set0", "(I)I", |_, a| {
        let one: u64 = 1;
        ret_int(unsafe { libc::write(a[0] as i32, &one as *const u64 as *const libc::c_void, 8) } as i32)
    });
}

fn register_inet(r: &mut NativeRegistry) {
    r.add("java/net/InetAddress", "init", "()V", |_, _| ret_void());
    r.add("java/net/InetAddress", "isIPv4Available", "()Z", |_, _| ret_bool(true));
    r.add("java/net/InetAddress", "isIPv6Supported", "()Z", |_, _| ret_bool(false));
    r.add("java/net/Inet4Address", "init", "()V", |_, _| ret_void());
    r.add("java/net/Inet6Address", "init", "()V", |_, _| ret_void());
    const I4: &str = "java/net/Inet4AddressImpl";
    r.add(I4, "getLocalHostName", "()Ljava/lang/String;", |t, _| {
        let mut buf = [0u8; 256];
        unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        let name = std::ffi::CStr::from_bytes_until_nul(&buf).map_or("localhost".into(), |c| c.to_string_lossy().into_owned());
        ret_ref(rt::new_string(t, &name)?)
    });
    r.add(I4, "lookupAllHostAddr", "(Ljava/lang/String;)[Ljava/net/InetAddress;", |t, a| {
        let host = rt::java_string_to_rust(a[1] as Oop);
        let ips = resolve(&host);
        if ips.is_empty() {
            return Err(throw_new(t, "java/net/UnknownHostException", &host));
        }
        let ia = crate::loader::load("java/net/InetAddress").expect("InetAddress");
        let arr = rt::alloc_array(t, vm().array_class_of(ia).expect("InetAddress[]"), ips.len() as i32)?;
        for (i, ip) in ips.into_iter().enumerate() {
            let o = inet4(t, Some(&host), ip)?;
            // SAFETY: `arr` has one element per address.
            unsafe { obj::store_elem(arr, i as usize, o) };
        }
        ret_ref(arr)
    });
    r.add(I4, "getHostByAddr", "([B)Ljava/lang/String;", |t, _| Err(throw_new(t, "java/net/UnknownHostException", "")));
    r.add(I4, "isReachable0", "([BI[BI)Z", |_, _| ret_bool(false));
    r.add("java/net/NetworkInterface", "init", "()V", |_, _| ret_void());
    r.add("java/net/NetworkInterface", "getAll", "()[Ljava/net/NetworkInterface;", |t, _| {
        let ni = crate::loader::load("java/net/NetworkInterface").expect("NetworkInterface");
        ret_ref(rt::alloc_array(t, vm().array_class_of(ni).expect("NetworkInterface[]"), 0)?)
    });
    for (name, desc) in [
        ("getByName0", "(Ljava/lang/String;)Ljava/net/NetworkInterface;"),
        ("getByIndex0", "(I)Ljava/net/NetworkInterface;"),
        ("getByInetAddress0", "(Ljava/net/InetAddress;)Ljava/net/NetworkInterface;"),
    ] {
        r.add("java/net/NetworkInterface", name, desc, |_, _| ret_ref(std::ptr::null_mut()));
    }
    r.add("java/net/NetworkInterface", "boundInetAddress0", "(Ljava/net/InetAddress;)Z", |_, _| ret_bool(true));
    // Linux-specific extended socket options are reported unsupported.
    for name in ["keepAliveOptionsSupported0", "quickAckSupported0", "incomingNapiIdSupported0"] {
        r.add("jdk/net/LinuxSocketOptions", name, "()Z", |_, _| ret_bool(false));
    }
}

/// IPv4 addresses of `host` (numeric or via the system resolver).
fn resolve(host: &str) -> Vec<u32> {
    use std::net::ToSocketAddrs;
    if host.is_empty() || host == "localhost" {
        return vec![0x7f00_0001];
    }
    (host, 0)
        .to_socket_addrs()
        .map(|it| {
            it.filter_map(|a| match a {
                std::net::SocketAddr::V4(v4) => Some(u32::from(*v4.ip())),
                _ => None,
            })
            .collect()
        })
        .unwrap_or_default()
}
