//! IPv4 multicast and urgent data on sockets (`sun.nio.ch.Net`), as the JDK's `Net.c` does them:
//! group membership with or without a source, source blocking, the outgoing interface, and
//! out-of-band bytes. Addresses arrive as `int`s in host order.

use super::net::fd;
use super::sock;
use super::{ret_bool, ret_int, ret_void, NativeRegistry};
use crate::runtime::throw_new;
use crate::thread::{JResult, Thread};

const NET: &str = "sun/nio/ch/Net";
const IPPROTO_IP: i32 = 0;
/// `IOStatus.UNAVAILABLE`: the system has no source-specific multicast.
const UNAVAILABLE: i32 = -2;

/// The `IP_*` option numbers, which differ per system.
#[cfg(target_os = "linux")]
mod opt {
    pub const MULTICAST_IF: i32 = 32;
    pub const ADD_MEMBERSHIP: i32 = 35;
    pub const DROP_MEMBERSHIP: i32 = 36;
    pub const UNBLOCK_SOURCE: i32 = 37;
    pub const BLOCK_SOURCE: i32 = 38;
    pub const ADD_SOURCE_MEMBERSHIP: i32 = 39;
    pub const DROP_SOURCE_MEMBERSHIP: i32 = 40;
}
#[cfg(target_os = "macos")]
mod opt {
    pub const MULTICAST_IF: i32 = 9;
    pub const ADD_MEMBERSHIP: i32 = 12;
    pub const DROP_MEMBERSHIP: i32 = 13;
    pub const ADD_SOURCE_MEMBERSHIP: i32 = 70;
    pub const DROP_SOURCE_MEMBERSHIP: i32 = 71;
    pub const BLOCK_SOURCE: i32 = 72;
    pub const UNBLOCK_SOURCE: i32 = 73;
}
#[cfg(windows)]
mod opt {
    pub const MULTICAST_IF: i32 = 9;
    pub const ADD_MEMBERSHIP: i32 = 12;
    pub const DROP_MEMBERSHIP: i32 = 13;
    pub const ADD_SOURCE_MEMBERSHIP: i32 = 15;
    pub const DROP_SOURCE_MEMBERSHIP: i32 = 16;
    pub const BLOCK_SOURCE: i32 = 17;
    pub const UNBLOCK_SOURCE: i32 = 18;
}

/// `struct ip_mreq` (group, interface) or `struct ip_mreq_source`, whose field order differs:
/// Linux puts the interface before the source, BSD and Windows after it.
fn request(group: u32, interf: u32, source: u32) -> Vec<u8> {
    let fields: &[u32] = if source == 0 {
        &[group, interf]
    } else if cfg!(target_os = "linux") {
        &[group, interf, source]
    } else {
        &[group, source, interf]
    };
    fields.iter().flat_map(|a| a.to_be_bytes()).collect()
}

fn socket_error(t: &mut Thread) -> crate::thread::Throw {
    throw_new(t, "java/net/SocketException", &sock::error_text())
}

/// Sets a membership or source-filter option; `UNAVAILABLE` when the system lacks it and
/// `unsupported_ok` (adding, not dropping).
fn membership(t: &mut Thread, f: i32, opt: i32, req: &[u8], unsupported_ok: bool) -> JResult<u64> {
    if sock::set_bytes(f, IPPROTO_IP, opt, req) == 0 {
        return ret_int(0);
    }
    if unsupported_ok && sock::unsupported() {
        return ret_int(UNAVAILABLE);
    }
    Err(socket_error(t))
}

pub(super) fn register(r: &mut NativeRegistry) {
    // joinOrDrop4(join, fd, group, interface, source)
    r.add(NET, "joinOrDrop4", "(ZLjava/io/FileDescriptor;III)I", |t, a| {
        let (join, source) = (a[0] != 0, a[4] as u32);
        let opt = match (join, source != 0) {
            (true, false) => opt::ADD_MEMBERSHIP,
            (false, false) => opt::DROP_MEMBERSHIP,
            (true, true) => opt::ADD_SOURCE_MEMBERSHIP,
            (false, true) => opt::DROP_SOURCE_MEMBERSHIP,
        };
        membership(t, fd(a[1]), opt, &request(a[2] as u32, a[3] as u32, source), join)
    });
    // blockOrUnblock4(block, fd, group, interface, source)
    r.add(NET, "blockOrUnblock4", "(ZLjava/io/FileDescriptor;III)I", |t, a| {
        let block = a[0] != 0;
        let opt = if block { opt::BLOCK_SOURCE } else { opt::UNBLOCK_SOURCE };
        membership(t, fd(a[1]), opt, &request(a[2] as u32, a[3] as u32, a[4] as u32), block)
    });
    r.add(NET, "setInterface4", "(Ljava/io/FileDescriptor;I)V", |t, a| {
        if sock::set_bytes(fd(a[0]), IPPROTO_IP, opt::MULTICAST_IF, &(a[1] as u32).to_be_bytes()) != 0 {
            return Err(socket_error(t));
        }
        ret_void()
    });
    r.add(NET, "getInterface4", "(Ljava/io/FileDescriptor;)I", |t, a| {
        let mut v = [0u8; 4];
        if sock::get_bytes(fd(a[0]), IPPROTO_IP, opt::MULTICAST_IF, &mut v).is_none() {
            return Err(socket_error(t));
        }
        ret_int(u32::from_be_bytes(v) as i32)
    });
    r.add(NET, "sendOOB", "(Ljava/io/FileDescriptor;B)I", |t, a| {
        let n = sock::send_flags(fd(a[0]), &[a[1] as u8], sock::MSG_OOB);
        if n < 0 {
            return Err(socket_error(t));
        }
        ret_int(n as i32)
    });
    // Reads and drops pending urgent data: whether there was any.
    r.add(NET, "discardOOB", "(Ljava/io/FileDescriptor;)Z", |_, a| {
        let (f, mut buf, mut any) = (fd(a[0]), [0u8; 8], false);
        while sock::recv_flags(f, &mut buf, sock::MSG_OOB) > 0 {
            any = true;
        }
        ret_bool(any)
    });
}
