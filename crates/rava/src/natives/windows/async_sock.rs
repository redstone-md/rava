//! Asynchronous sockets as the JDK's Windows natives start them (`WindowsAsynchronousSocketChannelImpl`,
//! `WindowsAsynchronousServerSocketChannelImpl`): overlapped `ConnectEx`, `AcceptEx`, `WSARecv` and
//! `WSASend`, each completing on the port the socket is bound to (`iocp`).

use super::args::Args;
use super::iocp::{IOS_EOF, IOS_UNAVAILABLE};
use crate::natives::io_shared::io_error;
use crate::natives::{ret_int, ret_void, NativeRegistry};
use crate::thread::{JResult, Thread};
use std::sync::OnceLock;
use windows_sys::core::GUID;
use windows_sys::Win32::Networking::WinSock::*;
use windows_sys::Win32::System::IO::OVERLAPPED;

const SOCK: &str = "sun/nio/ch/WindowsAsynchronousSocketChannelImpl";
const SERVER: &str = "sun/nio/ch/WindowsAsynchronousServerSocketChannelImpl";
/// Room AcceptEx needs for each address: `sizeof(SOCKETADDRESS) + 16`.
const ADDRESS_ROOM: u32 = std::mem::size_of::<SOCKADDR_IN6>() as u32 + 16;

/// A Winsock extension function, looked up once on a scratch socket.
fn extension(cell: &OnceLock<usize>, guid: GUID) -> usize {
    *cell.get_or_init(|| {
        let fd = crate::natives::sock::socket(true);
        let (mut f, mut n) = (0usize, 0u32);
        // SAFETY: asks the provider for the function's address into `f`.
        unsafe {
            WSAIoctl(fd as u32 as SOCKET, SIO_GET_EXTENSION_FUNCTION_POINTER, &guid as *const GUID as *const _, std::mem::size_of::<GUID>() as u32, &mut f as *mut usize as *mut _, 8, &mut n, std::ptr::null_mut(), None);
        }
        crate::natives::sock::close(fd);
        f
    })
}

fn connect_ex() -> LPFN_CONNECTEX {
    static F: OnceLock<usize> = OnceLock::new();
    // SAFETY: the address WSAIoctl returned for WSAID_CONNECTEX (or 0, None).
    unsafe { std::mem::transmute::<usize, LPFN_CONNECTEX>(extension(&F, WSAID_CONNECTEX)) }
}

fn accept_ex() -> LPFN_ACCEPTEX {
    static F: OnceLock<usize> = OnceLock::new();
    // SAFETY: as for ConnectEx.
    unsafe { std::mem::transmute::<usize, LPFN_ACCEPTEX>(extension(&F, WSAID_ACCEPTEX)) }
}

fn zeroed(ov: *mut OVERLAPPED) -> *mut OVERLAPPED {
    // SAFETY: the OVERLAPPED the class library allocated for this operation.
    unsafe { ov.write_bytes(0, 1) };
    ov
}

/// An overlapped start's result: 0 when it completed at once (`done`), `UNAVAILABLE` while
/// pending, `EOF` on a shut-down direction where `shutdown_eof`, else an exception.
fn started(t: &mut Thread, ok: bool, done: i32, shutdown_eof: bool, what: &str) -> JResult<u64> {
    if ok {
        return ret_int(done);
    }
    // SAFETY: a plain query.
    match unsafe { WSAGetLastError() } {
        WSA_IO_PENDING => ret_int(IOS_UNAVAILABLE),
        WSAESHUTDOWN if shutdown_eof => ret_int(IOS_EOF),
        _ => Err(io_error(t, what)),
    }
}

fn checked(t: &mut Thread, r: i32, what: &str) -> JResult<u64> {
    if r == SOCKET_ERROR {
        return Err(io_error(t, what));
    }
    ret_void()
}

fn close(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    // SAFETY: a socket the channel owns, closed once.
    checked(t, unsafe { closesocket(a[0] as SOCKET) }, "closesocket failed")
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(SOCK, "initIDs", "()V", |_, _| {
        connect_ex();
        ret_void()
    });
    // connect0(socket, preferIPv6, address, port, overlapped)
    r.add(SOCK, "connect0", "(JZLjava/net/InetAddress;IJ)I", |t, a| {
        let mut args = Args::new(a);
        let (s, _prefer6) = (args.long() as SOCKET, args.boolean());
        let addr = args.object();
        let (port, ov) = (args.int(), args.ptr::<OVERLAPPED>());
        let sa = crate::natives::net::sockaddr(addr, port as u64);
        let Some(f) = connect_ex() else { return Err(io_error(t, "ConnectEx unavailable")) };
        // SAFETY: the socket, an address of its size, and the operation's OVERLAPPED.
        let ok = unsafe { f(s, &sa as *const _ as *const SOCKADDR, std::mem::size_of_val(&sa) as i32, std::ptr::null(), 0, std::ptr::null_mut(), zeroed(ov)) } != 0;
        started(t, ok, 0, false, "ConnectEx failed")
    });
    r.add(SOCK, "updateConnectContext", "(J)V", |t, a| {
        // SAFETY: completes a ConnectEx connection's socket state.
        checked(t, unsafe { setsockopt(a[0] as SOCKET, SOL_SOCKET, SO_UPDATE_CONNECT_CONTEXT, std::ptr::null(), 0) }, "setsockopt failed")
    });
    r.add(SOCK, "shutdown0", "(JI)V", |t, a| {
        // SAFETY: a connected socket.
        checked(t, unsafe { shutdown(a[0] as SOCKET, a[2] as i32) }, "shutdown failed")
    });
    r.add(SOCK, "closesocket0", "(J)V", close);
    // read0 / write0(socket, buffers, WSABUF array, overlapped)
    r.add(SOCK, "read0", "(JIJJ)I", |t, a| {
        let mut a = Args::new(a);
        let (s, n, bufs, ov) = (a.long() as SOCKET, a.int(), a.ptr::<WSABUF>(), a.ptr::<OVERLAPPED>());
        let mut flags = 0u32;
        // SAFETY: `n` WSABUFs over buffers kept alive until the completion.
        let ok = unsafe { WSARecv(s, bufs, n as u32, std::ptr::null_mut(), &mut flags, zeroed(ov), None) } != SOCKET_ERROR;
        started(t, ok, IOS_UNAVAILABLE, true, "WSARecv failed")
    });
    r.add(SOCK, "write0", "(JIJJ)I", |t, a| {
        let mut a = Args::new(a);
        let (s, n, bufs, ov) = (a.long() as SOCKET, a.int(), a.ptr::<WSABUF>(), a.ptr::<OVERLAPPED>());
        // SAFETY: as for read0.
        let ok = unsafe { WSASend(s, bufs, n as u32, std::ptr::null_mut(), 0, zeroed(ov), None) } != SOCKET_ERROR;
        started(t, ok, IOS_UNAVAILABLE, true, "WSASend failed")
    });
    r.add(SERVER, "initIDs", "()V", |_, _| {
        accept_ex();
        ret_void()
    });
    // accept0(listener, accepted, overlapped, address buffer)
    r.add(SERVER, "accept0", "(JJJJ)I", |t, a| {
        let mut a = Args::new(a);
        let (listener, accepted, ov, buf) = (a.long() as SOCKET, a.long() as SOCKET, a.ptr::<OVERLAPPED>(), a.ptr::<u8>());
        let Some(f) = accept_ex() else { return Err(io_error(t, "AcceptEx unavailable")) };
        let mut n = 0u32;
        // SAFETY: both sockets, a buffer with room for two addresses, the operation's OVERLAPPED.
        let ok = unsafe { f(listener, accepted, buf as *mut _, 0, ADDRESS_ROOM, ADDRESS_ROOM, &mut n, zeroed(ov)) } != 0;
        started(t, ok, 0, false, "AcceptEx failed")
    });
    r.add(SERVER, "updateAcceptContext", "(JJ)V", |t, a| {
        let listener = a[0] as SOCKET;
        // SAFETY: gives the accepted socket the listener's properties.
        checked(t, unsafe { setsockopt(a[2] as SOCKET, SOL_SOCKET, SO_UPDATE_ACCEPT_CONTEXT, &listener as *const SOCKET as *const u8, std::mem::size_of::<SOCKET>() as i32) }, "setsockopt failed")
    });
    r.add(SERVER, "closesocket0", "(J)V", close);
}
