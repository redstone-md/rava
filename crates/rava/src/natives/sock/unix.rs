//! POSIX sockets.

use super::SockError;

pub(crate) fn last_code() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

pub(crate) fn last_error() -> SockError {
    match last_code() {
        libc::EAGAIN => SockError::WouldBlock,
        libc::EINTR => SockError::Interrupted,
        libc::EINPROGRESS => SockError::InProgress,
        libc::ECONNREFUSED => SockError::Refused,
        _ => SockError::Other,
    }
}

pub(crate) const POLLIN: i32 = libc::POLLIN as i32;
pub(crate) const POLLOUT: i32 = libc::POLLOUT as i32;
pub(crate) const POLLERR: i32 = libc::POLLERR as i32;
pub(crate) const POLLHUP: i32 = libc::POLLHUP as i32;
pub(crate) const POLLNVAL: i32 = libc::POLLNVAL as i32;
pub(crate) const SOL_SOCKET: i32 = libc::SOL_SOCKET;
pub(crate) const SO_LINGER: i32 = libc::SO_LINGER;

pub(crate) type SockAddrIn = libc::sockaddr_in;

pub(crate) const LAYOUT: super::Layout = super::Layout {
    af_inet: libc::AF_INET as usize,
    af_inet6: libc::AF_INET6 as usize,
    sizeof_in4: std::mem::size_of::<libc::sockaddr_in>(),
    sizeof_in6: std::mem::size_of::<libc::sockaddr_in6>(),
    sizeof_family: std::mem::size_of::<libc::sa_family_t>(),
    offset_family: std::mem::offset_of!(libc::sockaddr_in, sin_family),
    offset_in4_port: std::mem::offset_of!(libc::sockaddr_in, sin_port),
    offset_in4_addr: std::mem::offset_of!(libc::sockaddr_in, sin_addr),
    offset_in6_port: std::mem::offset_of!(libc::sockaddr_in6, sin6_port),
    offset_in6_flowinfo: std::mem::offset_of!(libc::sockaddr_in6, sin6_flowinfo),
    offset_in6_addr: std::mem::offset_of!(libc::sockaddr_in6, sin6_addr),
    offset_in6_scope_id: std::mem::offset_of!(libc::sockaddr_in6, sin6_scope_id),
};

pub(crate) fn sockaddr_in(ip: u32, port: u16) -> SockAddrIn {
    crate::natives::posix::sockaddr_in(ip, port)
}

/// The address and port of an IPv4 socket address (host order).
pub(crate) fn ip_port(sa: &SockAddrIn) -> (u32, u16) {
    (u32::from_be(sa.sin_addr.s_addr), u16::from_be(sa.sin_port))
}

pub(crate) fn socket(stream: bool) -> i32 {
    crate::natives::posix::socket(libc::AF_INET, if stream { libc::SOCK_STREAM } else { libc::SOCK_DGRAM })
}

pub(crate) fn set_int(fd: i32, level: i32, opt: i32, v: i32) -> i32 {
    // SAFETY: an int option.
    unsafe { libc::setsockopt(fd, level, opt, &v as *const i32 as *const libc::c_void, 4) }
}

pub(crate) fn get_int(fd: i32, level: i32, opt: i32) -> Option<i32> {
    let (mut v, mut len): (libc::c_int, libc::socklen_t) = (0, 4);
    // SAFETY: an int option.
    (unsafe { libc::getsockopt(fd, level, opt, &mut v as *mut i32 as *mut libc::c_void, &mut len) } == 0).then_some(v)
}

/// A raw option value.
pub(crate) fn set_bytes(fd: i32, level: i32, opt: i32, v: &[u8]) -> i32 {
    // SAFETY: `v` is the option's value.
    unsafe { libc::setsockopt(fd, level, opt, v.as_ptr() as *const libc::c_void, v.len() as libc::socklen_t) }
}

/// A raw option value into `v`: its length.
pub(crate) fn get_bytes(fd: i32, level: i32, opt: i32, v: &mut [u8]) -> Option<usize> {
    let mut len = v.len() as libc::socklen_t;
    // SAFETY: `v` has room for `len` bytes.
    (unsafe { libc::getsockopt(fd, level, opt, v.as_mut_ptr() as *mut libc::c_void, &mut len) } == 0).then_some(len as usize)
}

/// Whether the last call failed for an option or operation the system does not offer.
pub(crate) fn unsupported() -> bool {
    matches!(last_code(), libc::ENOPROTOOPT | libc::EOPNOTSUPP)
}

/// `send` / `recv` with `flags` (urgent data).
pub(crate) fn send_flags(fd: i32, buf: &[u8], flags: i32) -> isize {
    // SAFETY: `buf` is readable.
    unsafe { libc::send(fd, buf.as_ptr() as *const libc::c_void, buf.len(), flags) }
}

pub(crate) fn recv_flags(fd: i32, buf: &mut [u8], flags: i32) -> isize {
    // SAFETY: `buf` is writable.
    unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), flags) }
}

pub(crate) const MSG_OOB: i32 = libc::MSG_OOB;

pub(crate) fn set_reuse_address(fd: i32) {
    set_int(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, 1);
}

/// `SO_LINGER` on (seconds) or off (negative).
pub(crate) fn set_linger(fd: i32, seconds: i32) -> i32 {
    let l = libc::linger { l_onoff: (seconds >= 0) as i32, l_linger: seconds.max(0) };
    // SAFETY: a linger option.
    unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_LINGER, &l as *const _ as *const libc::c_void, std::mem::size_of_val(&l) as u32) }
}

/// The pending error of a connect (`SO_ERROR`).
pub(crate) fn pending_error(fd: i32) -> i32 {
    get_int(fd, libc::SOL_SOCKET, libc::SO_ERROR).unwrap_or(0)
}

pub(crate) fn bind(fd: i32, sa: &SockAddrIn) -> i32 {
    // SAFETY: an IPv4 address of its size.
    unsafe { libc::bind(fd, sa as *const _ as *const libc::sockaddr, std::mem::size_of_val(sa) as u32) }
}

pub(crate) fn connect(fd: i32, sa: &SockAddrIn) -> i32 {
    // SAFETY: as above.
    unsafe { libc::connect(fd, sa as *const _ as *const libc::sockaddr, std::mem::size_of_val(sa) as u32) }
}

/// Dissolves a datagram socket's association.
pub(crate) fn disconnect(fd: i32) -> i32 {
    // SAFETY: an AF_UNSPEC address.
    let r = unsafe {
        let mut sa: libc::sockaddr = std::mem::zeroed();
        sa.sa_family = libc::AF_UNSPEC as libc::sa_family_t;
        libc::connect(fd, &sa, std::mem::size_of::<libc::sockaddr>() as u32)
    };
    if r < 0 && last_code() == libc::EAFNOSUPPORT { 0 } else { r }
}

pub(crate) fn listen(fd: i32, backlog: i32) -> i32 {
    // SAFETY: a plain call.
    unsafe { libc::listen(fd, backlog) }
}

pub(crate) fn accept(fd: i32, sa: &mut SockAddrIn) -> i32 {
    let mut len = std::mem::size_of::<SockAddrIn>() as libc::socklen_t;
    crate::natives::posix::accept(fd, sa as *mut _ as *mut libc::sockaddr, &mut len)
}

pub(crate) fn shutdown(fd: i32, how: i32) {
    // SAFETY: a plain call.
    unsafe { libc::shutdown(fd, how) };
}

/// Shuts both directions (before a close that must wake blocked threads).
pub(crate) fn shutdown_both(fd: i32) {
    shutdown(fd, libc::SHUT_RDWR);
}

/// The local or peer address.
pub(crate) fn name(fd: i32, peer: bool) -> Option<SockAddrIn> {
    // SAFETY: an IPv4 address buffer of its size.
    unsafe {
        let mut sa: SockAddrIn = std::mem::zeroed();
        let mut len = std::mem::size_of::<SockAddrIn>() as libc::socklen_t;
        let p = &mut sa as *mut SockAddrIn as *mut libc::sockaddr;
        let r = if peer { libc::getpeername(fd, p, &mut len) } else { libc::getsockname(fd, p, &mut len) };
        (r == 0).then_some(sa)
    }
}

/// Waits up to `timeout` ms for `events`: the events that happened, 0 on timeout, -1 on error.
pub(crate) fn poll(fd: i32, events: i32, timeout: i32) -> i32 {
    let mut p = libc::pollfd { fd, events: events as i16, revents: 0 };
    // SAFETY: one pollfd.
    match unsafe { libc::poll(&mut p, 1, timeout) } {
        n if n > 0 => p.revents as i32,
        n => n,
    }
}

pub(crate) fn available(fd: i32) -> i32 {
    let mut n: libc::c_int = 0;
    // SAFETY: FIONREAD fills an int.
    unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) };
    n
}

pub(crate) fn recv(fd: i32, buf: usize, len: usize) -> isize {
    // SAFETY: the caller's buffer.
    unsafe { libc::read(fd, buf as *mut libc::c_void, len) }
}

pub(crate) fn send(fd: i32, buf: usize, len: usize) -> isize {
    // SAFETY: the caller's buffer.
    unsafe { libc::write(fd, buf as *const libc::c_void, len) }
}

/// Scatter and gather over the class library's `{base, length}` pairs (`struct iovec`).
pub(crate) fn recv_vec(fd: i32, iov: usize, n: i32) -> isize {
    // SAFETY: the caller's iovec array.
    unsafe { libc::readv(fd, iov as *const libc::iovec, n) }
}

pub(crate) fn send_vec(fd: i32, iov: usize, n: i32) -> isize {
    // SAFETY: as above.
    unsafe { libc::writev(fd, iov as *const libc::iovec, n) }
}

pub(crate) fn recv_from(fd: i32, buf: usize, len: usize, from: usize) -> isize {
    let mut alen = std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t;
    // SAFETY: the caller's buffer and native socket address.
    unsafe { libc::recvfrom(fd, buf as *mut libc::c_void, len, 0, from as *mut libc::sockaddr, &mut alen) }
}

pub(crate) fn send_to(fd: i32, buf: usize, len: usize, to: usize, to_len: u32) -> isize {
    // SAFETY: as above.
    unsafe { libc::sendto(fd, buf as *const libc::c_void, len, 0, to as *const libc::sockaddr, to_len) }
}

pub(crate) fn close(fd: i32) {
    // SAFETY: a socket the class library owns.
    unsafe { libc::close(fd) };
}

pub(crate) fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: fills a buffer of its length.
    unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    std::ffi::CStr::from_bytes_until_nul(&buf).map_or("localhost".into(), |c| c.to_string_lossy().into_owned())
}
