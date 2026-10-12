//! UDP: `sun.nio.ch.NativeSocketAddress` (the layout of a native socket address, which Java
//! code reads and writes directly) and `DatagramChannelImpl`'s send and receive. Blocking calls
//! run in the native state so the collector can proceed.

use super::net::{fd, io_status};
use super::sock;
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
    // The layouts of sockaddr_in and sockaddr_in6 (macOS puts a length byte before a one-byte
    // family).
    // The layouts of the system's sockaddr_in and sockaddr_in6 (macOS puts a length byte before
    // a one-byte family; Windows keeps the IPv6 scope in a union).
    constants!(r,
        "AFINET" => sock::LAYOUT.af_inet,
        "AFINET6" => sock::LAYOUT.af_inet6,
        "sizeofSockAddr4" => sock::LAYOUT.sizeof_in4,
        "sizeofSockAddr6" => sock::LAYOUT.sizeof_in6,
        "sizeofSockAddr" => sock::LAYOUT.sizeof_in6,
        "sizeofFamily" => sock::LAYOUT.sizeof_family,
        "offsetFamily" => sock::LAYOUT.offset_family,
        "offsetSin4Port" => sock::LAYOUT.offset_in4_port,
        "offsetSin4Addr" => sock::LAYOUT.offset_in4_addr,
        "offsetSin6Port" => sock::LAYOUT.offset_in6_port,
        "offsetSin6FlowInfo" => sock::LAYOUT.offset_in6_flowinfo,
        "offsetSin6Addr" => sock::LAYOUT.offset_in6_addr,
        "offsetSin6ScopeId" => sock::LAYOUT.offset_in6_scope_id,
    );
    r.add(DC, "disconnect0", "(Ljava/io/FileDescriptor;Z)V", |t, a| {
        if sock::disconnect(fd(a[0])) < 0 {
            return Err(throw_new(t, "java/io/IOException", &sock::error_text()));
        }
        ret_void()
    });
    // receive0(fd, address, len, senderAddress, connected)
    r.add(DC, "receive0", "(Ljava/io/FileDescriptor;JIJZ)I", |t, a| {
        let (f, buf, len, sender) = (fd(a[0]), a[1] as usize, a[3] as u32 as usize, a[4] as usize);
        let n = crate::gc::safepoint::blocking(t, || sock::recv_from(f, buf, len, sender));
        finish(t, n, a[5] != 0)
    });
    // send0(fd, address, len, targetAddress, targetAddressLen)
    r.add(DC, "send0", "(Ljava/io/FileDescriptor;JIJI)I", |t, a| {
        let (f, buf, len, target, tlen) = (fd(a[0]), a[1] as usize, a[3] as u32 as usize, a[4] as usize, a[6] as u32);
        let n = crate::gc::safepoint::blocking(t, || sock::send_to(f, buf, len, target, tlen));
        finish(t, n, true)
    });
}

/// A datagram call's result: bytes, or an `IOStatus` code, or an exception (a refused port
/// as `PortUnreachableException` where the JDK reports one).
fn finish(t: &mut Thread, n: isize, refusal: bool) -> JResult<u64> {
    if n < 0 && refusal && sock::last_error() == sock::SockError::Refused {
        return Err(throw_new(t, "java/net/PortUnreachableException", ""));
    }
    match io_status(n, false) {
        i64::MIN => Err(throw_new(t, "java/io/IOException", &sock::error_text())),
        v => ret_int(v as i32),
    }
}
