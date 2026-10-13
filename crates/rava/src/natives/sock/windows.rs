//! Winsock. The `SOCKET` handle fits the class library's `int`, as in the JDK's own Windows code.

use super::SockError;
use std::sync::Once;
use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};
use windows_sys::Win32::Networking::WinSock as ws;
use windows_sys::Win32::Networking::WinSock::*;

fn startup() {
    static START: Once = Once::new();
    START.call_once(|| {
        // SAFETY: initializes Winsock 2.2 for the process, once.
        let mut data: WSADATA = unsafe { std::mem::zeroed() };
        unsafe { WSAStartup(0x0202, &mut data) };
    });
}

fn s(fd: i32) -> SOCKET {
    fd as u32 as SOCKET
}

pub(crate) fn last_code() -> i32 {
    // SAFETY: a plain query.
    unsafe { WSAGetLastError() }
}

pub(crate) fn last_error() -> SockError {
    match last_code() {
        WSAEWOULDBLOCK => SockError::WouldBlock,
        WSAEINTR => SockError::Interrupted,
        WSAEINPROGRESS => SockError::InProgress,
        // A datagram to a closed port shows as a reset on the next receive.
        WSAECONNREFUSED | WSAECONNRESET => SockError::Refused,
        _ => SockError::Other,
    }
}

pub(crate) const POLLIN: i32 = (POLLRDNORM | POLLRDBAND) as i32;
pub(crate) const POLLOUT: i32 = POLLWRNORM as i32;
pub(crate) const POLLERR: i32 = windows_sys::Win32::Networking::WinSock::POLLERR as i32;
pub(crate) const POLLHUP: i32 = windows_sys::Win32::Networking::WinSock::POLLHUP as i32;
pub(crate) const POLLNVAL: i32 = windows_sys::Win32::Networking::WinSock::POLLNVAL as i32;
pub(crate) const SOL_SOCKET: i32 = windows_sys::Win32::Networking::WinSock::SOL_SOCKET;
pub(crate) const SO_LINGER: i32 = windows_sys::Win32::Networking::WinSock::SO_LINGER;

pub(crate) type SockAddrIn = SOCKADDR_IN;
type SockAddrIn6 = SOCKADDR_IN6;

pub(crate) const LAYOUT: super::Layout = super::Layout {
    af_inet: AF_INET as usize,
    af_inet6: AF_INET6 as usize,
    sizeof_in4: std::mem::size_of::<SOCKADDR_IN>(),
    sizeof_in6: std::mem::size_of::<SOCKADDR_IN6>(),
    sizeof_family: std::mem::size_of::<ADDRESS_FAMILY>(),
    offset_family: std::mem::offset_of!(SOCKADDR_IN, sin_family),
    offset_in4_port: std::mem::offset_of!(SOCKADDR_IN, sin_port),
    offset_in4_addr: std::mem::offset_of!(SOCKADDR_IN, sin_addr),
    offset_in6_port: std::mem::offset_of!(SOCKADDR_IN6, sin6_port),
    offset_in6_flowinfo: std::mem::offset_of!(SOCKADDR_IN6, sin6_flowinfo),
    offset_in6_addr: std::mem::offset_of!(SOCKADDR_IN6, sin6_addr),
    offset_in6_scope_id: std::mem::offset_of!(SOCKADDR_IN6, Anonymous),
};

pub(crate) fn sockaddr_in(ip: u32, port: u16) -> SockAddrIn {
    // SAFETY: an all-zero SOCKADDR_IN is valid; the fields that matter are set below.
    let mut sa: SOCKADDR_IN = unsafe { std::mem::zeroed() };
    sa.sin_family = AF_INET;
    sa.sin_port = port.to_be();
    sa.sin_addr.S_un.S_addr = ip.to_be();
    sa
}

/// The address and port of an IPv4 socket address (host order).
pub(crate) fn ip_port(sa: &SockAddrIn) -> (u32, u16) {
    // SAFETY: every view of the address union is its four bytes.
    (u32::from_be(unsafe { sa.sin_addr.S_un.S_addr }), u16::from_be(sa.sin_port))
}

pub(crate) fn socket(stream: bool) -> i32 {
    startup();
    // SAFETY: a new socket, kept out of child processes as the JDK keeps it.
    unsafe {
        let h = ws::socket(AF_INET as i32, if stream { SOCK_STREAM } else { SOCK_DGRAM }, 0);
        if h == INVALID_SOCKET {
            return -1;
        }
        SetHandleInformation(h as HANDLE, HANDLE_FLAG_INHERIT, 0);
        h as i32
    }
}

pub(crate) fn set_int(fd: i32, level: i32, opt: i32, v: i32) -> i32 {
    // SAFETY: an int option.
    unsafe { setsockopt(s(fd), level, opt, &v as *const i32 as *const u8, 4) }
}

pub(crate) fn get_int(fd: i32, level: i32, opt: i32) -> Option<i32> {
    let (mut v, mut len) = (0i32, 4i32);
    // SAFETY: an int option.
    (unsafe { getsockopt(s(fd), level, opt, &mut v as *mut i32 as *mut u8, &mut len) } == 0).then_some(v)
}

/// A raw option value.
pub(crate) fn set_bytes(fd: i32, level: i32, opt: i32, v: &[u8]) -> i32 {
    // SAFETY: `v` is the option's value.
    unsafe { setsockopt(s(fd), level, opt, v.as_ptr(), v.len() as i32) }
}

/// A raw option value into `v`: its length.
pub(crate) fn get_bytes(fd: i32, level: i32, opt: i32, v: &mut [u8]) -> Option<usize> {
    let mut len = v.len() as i32;
    // SAFETY: `v` has room for `len` bytes.
    (unsafe { getsockopt(s(fd), level, opt, v.as_mut_ptr(), &mut len) } == 0).then_some(len as usize)
}

/// Whether the last call failed for an option or operation the system does not offer.
pub(crate) fn unsupported() -> bool {
    matches!(last_code(), WSAENOPROTOOPT | WSAEOPNOTSUPP)
}

/// `send` / `recv` with `flags` (urgent data).
pub(crate) fn send_flags(fd: i32, buf: &[u8], flags: i32) -> isize {
    // SAFETY: `buf` is readable.
    unsafe { ws::send(s(fd), buf.as_ptr(), buf.len() as i32, flags as SEND_RECV_FLAGS) as isize }
}

pub(crate) fn recv_flags(fd: i32, buf: &mut [u8], flags: i32) -> isize {
    // SAFETY: `buf` is writable.
    unsafe { ws::recv(s(fd), buf.as_mut_ptr(), buf.len() as i32, flags as SEND_RECV_FLAGS) as isize }
}

pub(crate) const MSG_OOB: i32 = ws::MSG_OOB as i32;

pub(crate) fn set_reuse_address(fd: i32) {
    set_int(fd, SOL_SOCKET, SO_REUSEADDR, 1);
}

/// `SO_LINGER` on (seconds) or off (negative).
pub(crate) fn set_linger(fd: i32, seconds: i32) -> i32 {
    let l = LINGER { l_onoff: (seconds >= 0) as u16, l_linger: seconds.max(0) as u16 };
    // SAFETY: a linger option.
    unsafe { setsockopt(s(fd), SOL_SOCKET, SO_LINGER, &l as *const LINGER as *const u8, std::mem::size_of::<LINGER>() as i32) }
}

/// The pending error of a connect (`SO_ERROR`).
pub(crate) fn pending_error(fd: i32) -> i32 {
    get_int(fd, SOL_SOCKET, SO_ERROR).unwrap_or(0)
}

pub(crate) fn bind(fd: i32, sa: &SockAddrIn) -> i32 {
    // SAFETY: an IPv4 address of its size.
    unsafe { ws::bind(s(fd), sa as *const _ as *const SOCKADDR, std::mem::size_of_val(sa) as i32) }
}

pub(crate) fn connect(fd: i32, sa: &SockAddrIn) -> i32 {
    // SAFETY: as above.
    unsafe { ws::connect(s(fd), sa as *const _ as *const SOCKADDR, std::mem::size_of_val(sa) as i32) }
}

/// Dissolves a datagram socket's association.
pub(crate) fn disconnect(fd: i32) -> i32 {
    // SAFETY: an all-zero (AF_UNSPEC) address.
    unsafe {
        let sa: SOCKADDR = std::mem::zeroed();
        ws::connect(s(fd), &sa, std::mem::size_of::<SOCKADDR>() as i32)
    }
}

pub(crate) fn listen(fd: i32, backlog: i32) -> i32 {
    // SAFETY: a plain call.
    unsafe { ws::listen(s(fd), backlog) }
}

pub(crate) fn accept(fd: i32, sa: &mut SockAddrIn) -> i32 {
    let mut len = std::mem::size_of::<SockAddrIn>() as i32;
    // SAFETY: an address buffer of its size; the new socket is kept out of children.
    unsafe {
        let h = ws::accept(s(fd), sa as *mut _ as *mut SOCKADDR, &mut len);
        if h == INVALID_SOCKET {
            return -1;
        }
        SetHandleInformation(h as HANDLE, HANDLE_FLAG_INHERIT, 0);
        h as i32
    }
}

pub(crate) fn shutdown(fd: i32, how: i32) {
    // SAFETY: a plain call.
    unsafe { ws::shutdown(s(fd), how as WINSOCK_SHUTDOWN_HOW) };
}

pub(crate) fn shutdown_both(fd: i32) {
    shutdown(fd, SD_BOTH as i32);
}

/// The local or peer address.
pub(crate) fn name(fd: i32, peer: bool) -> Option<SockAddrIn> {
    // SAFETY: an address buffer of its size.
    unsafe {
        let mut sa: SockAddrIn = std::mem::zeroed();
        let mut len = std::mem::size_of::<SockAddrIn>() as i32;
        let p = &mut sa as *mut SockAddrIn as *mut SOCKADDR;
        let r = if peer { getpeername(s(fd), p, &mut len) } else { getsockname(s(fd), p, &mut len) };
        (r == 0).then_some(sa)
    }
}

/// Waits up to `timeout` ms for `events`: the events that happened, 0 on timeout, -1 on error.
pub(crate) fn poll(fd: i32, events: i32, timeout: i32) -> i32 {
    let mut p = WSAPOLLFD { fd: s(fd), events: events as i16, revents: 0 };
    // SAFETY: one WSAPOLLFD.
    match unsafe { WSAPoll(&mut p, 1, timeout) } {
        n if n > 0 => p.revents as i32,
        n => n,
    }
}

pub(crate) fn available(fd: i32) -> i32 {
    let mut n = 0u32;
    // SAFETY: FIONREAD fills a u32.
    unsafe { ioctlsocket(s(fd), FIONREAD, &mut n) };
    n as i32
}

/// Blocking or non-blocking mode.
pub(crate) fn set_blocking(fd: i32, blocking: bool) -> i32 {
    let mut on = (!blocking) as u32;
    // SAFETY: FIONBIO reads a u32.
    unsafe { ioctlsocket(s(fd), FIONBIO, &mut on) }
}

pub(crate) fn recv(fd: i32, buf: usize, len: usize) -> isize {
    // SAFETY: the caller's buffer.
    unsafe { ws::recv(s(fd), buf as *mut u8, len.min(i32::MAX as usize) as i32, 0) as isize }
}

pub(crate) fn send(fd: i32, buf: usize, len: usize) -> isize {
    // SAFETY: the caller's buffer.
    unsafe { ws::send(s(fd), buf as *const u8, len.min(i32::MAX as usize) as i32, 0) as isize }
}

/// The class library's `{base, length}` pairs (pointer-sized) as Winsock buffers.
fn wsabufs(iov: usize, n: i32) -> Vec<WSABUF> {
    // SAFETY: the caller's array of `n` pairs.
    (0..n as usize).map(|i| unsafe { WSABUF { len: *((iov + i * 16 + 8) as *const usize) as u32, buf: *((iov + i * 16) as *const *mut u8) } }).collect()
}

pub(crate) fn recv_vec(fd: i32, iov: usize, n: i32) -> isize {
    let bufs = wsabufs(iov, n);
    let (mut got, mut flags) = (0u32, 0u32);
    // SAFETY: buffers of the caller's memory.
    let r = unsafe { WSARecv(s(fd), bufs.as_ptr(), bufs.len() as u32, &mut got, &mut flags, std::ptr::null_mut(), None) };
    if r == 0 { got as isize } else { -1 }
}

pub(crate) fn send_vec(fd: i32, iov: usize, n: i32) -> isize {
    let bufs = wsabufs(iov, n);
    let mut sent = 0u32;
    // SAFETY: as above.
    let r = unsafe { WSASend(s(fd), bufs.as_ptr(), bufs.len() as u32, &mut sent, 0, std::ptr::null_mut(), None) };
    if r == 0 { sent as isize } else { -1 }
}

pub(crate) fn recv_from(fd: i32, buf: usize, len: usize, from: usize) -> isize {
    let mut alen = std::mem::size_of::<SockAddrIn6>() as i32;
    // SAFETY: the caller's buffer and native socket address.
    unsafe { recvfrom(s(fd), buf as *mut u8, len as i32, 0, from as *mut SOCKADDR, &mut alen) as isize }
}

pub(crate) fn send_to(fd: i32, buf: usize, len: usize, to: usize, to_len: u32) -> isize {
    // SAFETY: as above.
    unsafe { sendto(s(fd), buf as *const u8, len as i32, 0, to as *const SOCKADDR, to_len as i32) as isize }
}

pub(crate) fn close(fd: i32) {
    // SAFETY: a socket the class library owns.
    unsafe { closesocket(s(fd)) };
}

pub(crate) fn hostname() -> String {
    startup();
    let mut buf = [0u8; 256];
    // SAFETY: fills a buffer of its length.
    unsafe { gethostname(buf.as_mut_ptr(), buf.len() as i32) };
    std::ffi::CStr::from_bytes_until_nul(&buf).map_or("localhost".into(), |c| c.to_string_lossy().into_owned())
}
