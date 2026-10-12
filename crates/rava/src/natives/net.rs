//! Networking natives over the system's sockets (`sock`, IPv4): `java.net.InetAddress`
//! resolution, `sun.nio.ch.Net` and the socket dispatchers (the selectors are per system).
//! Blocking calls (accept, polls, socket reads) run in the native state so the collector can
//! proceed.

use super::io_shared::{fd_int, io_error};
use super::sock::{self, SockError};
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
pub(super) const INTERRUPTED: i32 = -3;

pub(super) fn fd(fdo: u64) -> i32 {
    // SAFETY: a live java.io.FileDescriptor.
    unsafe { obj::read::<i32>(fdo as Oop, fd_int()) }
}

#[cfg(unix)]
pub(super) fn errno() -> i32 {
    sock::last_code()
}

/// IPv4 address of a `java.net.InetAddress` (host byte order).
pub(super) fn ipv4_of(addr: Oop) -> u32 {
    // SAFETY: a live Inet4Address and its holder.
    unsafe { obj::read::<i32>(obj::read_ref(addr, ia_holder()), holder_address()) as u32 }
}

fn sockaddr(addr: Oop, port: u64) -> sock::SockAddrIn {
    sock::sockaddr_in(ipv4_of(addr), port as u16)
}

/// `new Inet4Address(host, bytes)`.
pub(super) fn inet4(t: &mut Thread, host: Option<&str>, ip: u32) -> JResult<Oop> {
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
    match sock::name(f, peer) {
        Some(sa) => Ok(sock::ip_port(&sa)),
        None => Err(io_error(t, "getsockname failed")),
    }
}

pub(super) fn check(t: &mut Thread, r: i32, what: &str) -> JResult<i32> {
    if r < 0 {
        Err(io_error(t, what))
    } else {
        Ok(r)
    }
}

/// A read or write's result as `IOStatus`: bytes, -1 at the end of a read, `UNAVAILABLE`,
/// `INTERRUPTED`, or `i64::MIN` for an error (the system's error set).
pub(super) fn io_status(n: isize, read: bool) -> i64 {
    match n {
        0 if read => -1,
        n if n >= 0 => n as i64,
        _ => match sock::last_error() {
            SockError::WouldBlock => UNAVAILABLE as i64,
            SockError::Interrupted => INTERRUPTED as i64,
            _ => i64::MIN,
        },
    }
}

pub(super) fn register(r: &mut NativeRegistry) {
    register_inet(r);
    for name in ["isIPv6Available0", "shouldShutdownWriteBeforeClose0", "shouldSetBothIPv4AndIPv6Options0", "canIPv6SocketJoinIPv4Group0", "canJoin6WithIPv4Group0", "canUseIPv6OptionsWithIPv4LocalAddress0"] {
        r.add(NET, name, "()Z", |_, _| ret_bool(false));
    }
    r.add(NET, "isReusePortAvailable0", "()Z", |_, _| ret_bool(cfg!(unix)));
    r.add(NET, "isExclusiveBindAvailable", "()I", |_, _| ret_int(-1));
    r.add(NET, "initIDs", "()V", |_, _| ret_void());
    r.add(NET, "pollinValue", "()S", |_, _| ret_int(sock::POLLIN));
    r.add(NET, "polloutValue", "()S", |_, _| ret_int(sock::POLLOUT));
    r.add(NET, "pollerrValue", "()S", |_, _| ret_int(sock::POLLERR));
    r.add(NET, "pollhupValue", "()S", |_, _| ret_int(sock::POLLHUP));
    r.add(NET, "pollnvalValue", "()S", |_, _| ret_int(sock::POLLNVAL));
    r.add(NET, "pollconnValue", "()S", |_, _| ret_int(sock::POLLOUT));
    r.add(NET, "socket0", "(ZZZZ)I", |t, a| {
        let s = check(t, sock::socket(a[1] != 0), "Socket creation failed")?;
        if a[2] != 0 {
            sock::set_reuse_address(s);
        }
        ret_int(s)
    });
    r.add(NET, "bind0", "(Ljava/io/FileDescriptor;ZZLjava/net/InetAddress;I)V", |t, a| {
        if sock::bind(fd(a[0]), &sockaddr(a[3] as Oop, a[4])) != 0 {
            return Err(throw_new(t, "java/net/BindException", &sock::error_text()));
        }
        ret_void()
    });
    r.add(NET, "listen", "(Ljava/io/FileDescriptor;I)V", |t, a| {
        check(t, sock::listen(fd(a[0]), a[1] as i32), "Listen failed")?;
        ret_void()
    });
    r.add(NET, "connect0", "(ZLjava/io/FileDescriptor;Ljava/net/InetAddress;I)I", |t, a| {
        if sock::connect(fd(a[1]), &sockaddr(a[2] as Oop, a[3])) == 0 {
            return ret_int(1);
        }
        match sock::last_error() {
            // Winsock reports a non-blocking connect under way as "would block".
            SockError::InProgress | SockError::WouldBlock => ret_int(UNAVAILABLE),
            SockError::Interrupted => ret_int(INTERRUPTED),
            _ => Err(throw_new(t, "java/net/ConnectException", &sock::error_text())),
        }
    });
    r.add(NET, "accept", "(Ljava/io/FileDescriptor;Ljava/io/FileDescriptor;[Ljava/net/InetSocketAddress;)I", accept);
    r.add(NET, "shutdown", "(Ljava/io/FileDescriptor;I)V", |_, a| {
        sock::shutdown(fd(a[0]), a[1] as i32);
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
    r.add(NET, "getIntOption0", "(Ljava/io/FileDescriptor;ZII)I", |t, a| match sock::get_int(fd(a[0]), a[2] as i32, a[3] as i32) {
        Some(v) => ret_int(v),
        None => Err(io_error(t, "getsockopt failed")),
    });
    r.add(NET, "setIntOption0", "(Ljava/io/FileDescriptor;ZIIIZ)V", |t, a| {
        let (level, opt, arg) = (a[2] as i32, a[3] as i32, a[4] as i32);
        let r = if level == sock::SOL_SOCKET && opt == sock::SO_LINGER { sock::set_linger(fd(a[0]), arg) } else { sock::set_int(fd(a[0]), level, opt, arg) };
        check(t, r, "setsockopt failed")?;
        ret_void()
    });
    r.add(NET, "poll", "(Ljava/io/FileDescriptor;IJ)I", |t, a| {
        let (f, events, timeout) = (fd(a[0]), a[1] as i32, a[2] as i64 as i32);
        let n = crate::gc::safepoint::blocking(t, || sock::poll(f, events, timeout));
        ret_int(n.max(0))
    });
    r.add(NET, "pollConnect", "(Ljava/io/FileDescriptor;J)Z", |t, a| {
        let (f, timeout) = (fd(a[0]), a[1] as i64 as i32);
        if crate::gc::safepoint::blocking(t, || sock::poll(f, sock::POLLOUT, timeout)) <= 0 {
            return ret_bool(false);
        }
        match sock::pending_error(f) {
            0 => ret_bool(true),
            err => Err(throw_new(t, "java/net/ConnectException", &std::io::Error::from_raw_os_error(err).to_string())),
        }
    });
    r.add(NET, "available", "(Ljava/io/FileDescriptor;)I", |_, a| ret_int(sock::available(fd(a[0]))));
    register_dispatch(r);
}

fn accept(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (listener, newfd, isaa) = (fd(a[0]), a[1] as Oop, a[2] as Oop);
    // SAFETY: an all-zero socket address is valid.
    let mut sa: sock::SockAddrIn = unsafe { std::mem::zeroed() };
    let sa_addr = &mut sa as *mut sock::SockAddrIn as usize;
    // SAFETY: the address outlives the blocking call.
    let s = crate::gc::safepoint::blocking(t, || sock::accept(listener, unsafe { &mut *(sa_addr as *mut sock::SockAddrIn) }));
    if s < 0 {
        return match sock::last_error() {
            SockError::WouldBlock => ret_int(UNAVAILABLE),
            SockError::Interrupted => ret_int(INTERRUPTED),
            _ => Err(io_error(t, "Accept failed")),
        };
    }
    // SAFETY: the new FileDescriptor and the one-element InetSocketAddress[].
    unsafe { obj::write::<i32>(newfd, fd_int(), s) };
    let (ip, port) = sock::ip_port(&sa);
    let ip = inet4(t, None, ip)?;
    let isa = crate::boot::construct(t, "java/net/InetSocketAddress", "(Ljava/net/InetAddress;I)V", &[ip as u64, port as u64])?;
    unsafe { obj::store_elem(isaa, 0, isa) };
    ret_int(1)
}

/// Reads/writes on sockets and datagram sockets, with IOStatus results.
fn register_dispatch(r: &mut NativeRegistry) {
    /// An `io_status` result, checked for the error marker before an int-returning native
    /// narrows it (`i64::MIN` narrowed is 0: a failed write read as nothing written, which the
    /// JDK retries forever on a closed connection).
    fn finish(t: &mut Thread, v: i64, int: bool) -> JResult<u64> {
        if v == i64::MIN {
            return Err(throw_new(t, "java/net/SocketException", &sock::error_text()));
        }
        Ok(if int { v as i32 as i64 as u64 } else { v as u64 })
    }
    // The system's error is read before leaving the blocking state (which may wait).
    for c in ["sun/nio/ch/SocketDispatcher", "sun/nio/ch/DatagramDispatcher"] {
        r.add(c, "read0", "(Ljava/io/FileDescriptor;JI)I", |t, a| {
            let v = crate::gc::safepoint::blocking(t, || io_status(sock::recv(fd(a[0]), a[1] as usize, a[3] as u32 as usize), true));
            finish(t, v, true)
        });
        r.add(c, "readv0", "(Ljava/io/FileDescriptor;JI)J", |t, a| {
            let v = crate::gc::safepoint::blocking(t, || io_status(sock::recv_vec(fd(a[0]), a[1] as usize, a[3] as i32), true));
            finish(t, v, false)
        });
        r.add(c, "write0", "(Ljava/io/FileDescriptor;JI)I", |t, a| {
            let v = crate::gc::safepoint::blocking(t, || io_status(sock::send(fd(a[0]), a[1] as usize, a[3] as u32 as usize), false));
            finish(t, v, true)
        });
        r.add(c, "writev0", "(Ljava/io/FileDescriptor;JI)J", |t, a| {
            let v = crate::gc::safepoint::blocking(t, || io_status(sock::send_vec(fd(a[0]), a[1] as usize, a[3] as i32), false));
            finish(t, v, false)
        });
    }
    r.add("sun/nio/ch/UnixDispatcher", "init", "()V", |_, _| ret_void());
    r.add("sun/nio/ch/UnixDispatcher", "close0", "(Ljava/io/FileDescriptor;)V", |_, a| {
        let f = fd(a[0]);
        if f >= 0 {
            sock::close(f);
        }
        ret_void()
    });
    r.add("sun/nio/ch/UnixDispatcher", "preClose0", "(Ljava/io/FileDescriptor;)V", |_, a| {
        sock::shutdown_both(fd(a[0]));
        ret_void()
    });
    // Windows closes a socket by its value.
    r.add("sun/nio/ch/SocketDispatcher", "close0", "(I)V", |_, a| {
        sock::close(a[0] as i32);
        ret_void()
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
        ret_ref(rt::new_string(t, &sock::hostname())?)
    });
    r.add(I4, "lookupAllHostAddr", "(Ljava/lang/String;)[Ljava/net/InetAddress;", |t, a| {
        let host = rt::java_string_to_rust(a[1] as Oop);
        let ips = resolve(&host);
        if ips.is_empty() {
            return Err(throw_new(t, "java/net/UnknownHostException", &host));
        }
        let ia = crate::loader::boot_class!("java/net/InetAddress").expect("InetAddress");
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
