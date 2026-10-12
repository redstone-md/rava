//! Sockets as the networking natives need them, over POSIX sockets or Winsock. A socket is the
//! `int` the class library keeps in `FileDescriptor.fd` (a descriptor, or a Winsock `SOCKET`).
//! Calls return what the system returns (-1 on failure); `last_error` says why, in the terms the
//! natives act on.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;

/// The layout of native socket addresses, which `sun.nio.ch.NativeSocketAddress` reads and
/// writes itself.
pub(crate) struct Layout {
    pub af_inet: usize,
    pub af_inet6: usize,
    pub sizeof_in4: usize,
    pub sizeof_in6: usize,
    pub sizeof_family: usize,
    pub offset_family: usize,
    pub offset_in4_port: usize,
    pub offset_in4_addr: usize,
    pub offset_in6_port: usize,
    pub offset_in6_flowinfo: usize,
    pub offset_in6_addr: usize,
    pub offset_in6_scope_id: usize,
}

/// Why the last socket call failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SockError {
    /// Non-blocking and nothing to do now.
    WouldBlock,
    Interrupted,
    /// A non-blocking connect under way.
    InProgress,
    /// The peer refused (a datagram to a closed port).
    Refused,
    Other,
}

/// The last error's system message.
pub(crate) fn error_text() -> String {
    std::io::Error::from_raw_os_error(last_code()).to_string().split(" (os error").next().unwrap_or("").to_string()
}
