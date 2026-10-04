//! UDP: `sun.nio.ch.NativeSocketAddress` (the layout of a native socket address, which Java
//! code reads and writes directly) and `DatagramChannelImpl`'s send and receive. Blocking calls
//! run in the native state so the collector can proceed.

use super::net::{fd, io_status};
use super::{ret_int, ret_void, NativeRegistry};
use crate::runtime::throw_new;
use crate::thread::{JResult, Thread};

const NSA: &str = "sun/nio/ch/NativeSocketAddress";
const DC: &str = "sun/nio/ch/DatagramChannelImpl";

macro_rules! constants {
    ($r:ident, $($name:literal => $v:expr),* $(,)?) => {
        $( $r.add(NSA, $name, "()I", |_, _| ret_int($v as i32)); )*
    };
}

pub(super) fn register(r: &mut NativeRegistry) {
    // struct sockaddr_in { sa_family_t; in_port_t; struct in_addr; ... } and sockaddr_in6
    // { sa_family_t; in_port_t; uint32_t flowinfo; struct in6_addr; uint32_t scope_id; }.
    constants!(r,
        "AFINET" => libc::AF_INET,
        "AFINET6" => libc::AF_INET6,
        "sizeofSockAddr4" => std::mem::size_of::<libc::sockaddr_in>(),
        "sizeofSockAddr6" => std::mem::size_of::<libc::sockaddr_in6>(),
        "sizeofSockAddr" => std::mem::size_of::<libc::sockaddr_in6>(),
        "sizeofFamily" => std::mem::size_of::<libc::sa_family_t>(),
        "offsetFamily" => 0,
        "offsetSin4Port" => 2,
        "offsetSin4Addr" => 4,
        "offsetSin6Port" => 2,
        "offsetSin6FlowInfo" => 4,
        "offsetSin6Addr" => 8,
        "offsetSin6ScopeId" => 24,
    );
    r.add(DC, "disconnect0", "(Ljava/io/FileDescriptor;Z)V", |t, a| {
        // SAFETY: an AF_UNSPEC address dissolves the association.
        let r = unsafe {
            let mut sa: libc::sockaddr = std::mem::zeroed();
            sa.sa_family = libc::AF_UNSPEC as libc::sa_family_t;
            libc::connect(fd(a[0]), &sa, std::mem::size_of::<libc::sockaddr>() as u32)
        };
        if r < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EAFNOSUPPORT) {
            return Err(throw_new(t, "java/io/IOException", &super::io::errno_text()));
        }
        ret_void()
    });
    // receive0(fd, address, len, senderAddress, connected)
    r.add(DC, "receive0", "(Ljava/io/FileDescriptor;JIJZ)I", |t, a| {
        let (f, buf, len, sender) = (fd(a[0]), a[1] as *mut libc::c_void, a[3] as u32 as usize, a[4] as *mut libc::sockaddr);
        let mut alen = std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t;
        // SAFETY: the caller's buffer and native socket address of these sizes.
        let n = crate::gc::safepoint::blocking(t, || unsafe { libc::recvfrom(f, buf, len, 0, sender, &mut alen) });
        finish(t, n, a[5] != 0)
    });
    // send0(fd, address, len, targetAddress, targetAddressLen)
    r.add(DC, "send0", "(Ljava/io/FileDescriptor;JIJI)I", |t, a| {
        let (f, buf, len, target, tlen) = (fd(a[0]), a[1] as *const libc::c_void, a[3] as u32 as usize, a[4] as *const libc::sockaddr, a[6] as u32);
        // SAFETY: the caller's buffer and native socket address of these sizes.
        let n = crate::gc::safepoint::blocking(t, || unsafe { libc::sendto(f, buf, len, 0, target, tlen) });
        finish(t, n, true)
    });
}

/// A datagram call's result: bytes, or an `IOStatus` code, or an exception (a refused port
/// as `PortUnreachableException` where the JDK reports one).
fn finish(t: &mut Thread, n: isize, refusal: bool) -> JResult<u64> {
    if n < 0 && refusal && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECONNREFUSED) {
        return Err(throw_new(t, "java/net/PortUnreachableException", ""));
    }
    match io_status(n, false) {
        i64::MIN => Err(throw_new(t, "java/io/IOException", &super::io::errno_text())),
        v => ret_int(v as i32),
    }
}
