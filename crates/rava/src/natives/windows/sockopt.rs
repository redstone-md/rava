//! `jdk.net.WindowsSocketOptions`: TCP keep-alive tuning and IP don't-fragment, through the
//! Winsock options of the same meaning.

use crate::natives::sock;
use crate::natives::{ret_bool, ret_int, ret_void, NativeRegistry};
use crate::runtime::throw_new;
use crate::thread::{JResult, Thread};

const C: &str = "jdk/net/WindowsSocketOptions";
/// ws2ipdef.h: `IPPROTO_*`, `TCP_KEEPIDLE`, `TCP_KEEPCNT`, `TCP_KEEPINTVL`, `IP_DONTFRAGMENT`
/// (IPv4 and IPv6 share the number).
const IPPROTO_IP: i32 = 0;
const IPPROTO_TCP: i32 = 6;
const IPPROTO_IPV6: i32 = 41;
const TCP_KEEPIDLE: i32 = 3;
const TCP_KEEPCNT: i32 = 16;
const TCP_KEEPINTVL: i32 = 17;
const IP_DONTFRAGMENT: i32 = 14;

fn set(t: &mut Thread, fd: i32, level: i32, opt: i32, v: i32) -> JResult<u64> {
    if sock::set_int(fd, level, opt, v) != 0 {
        return Err(throw_new(t, "java/net/SocketException", &sock::error_text()));
    }
    ret_void()
}

fn get(t: &mut Thread, fd: i32, level: i32, opt: i32) -> JResult<i32> {
    sock::get_int(fd, level, opt).ok_or_else(|| throw_new(t, "java/net/SocketException", &sock::error_text()))
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(C, "keepAliveOptionsSupported0", "()Z", |_, _| ret_bool(true));
    r.add(C, "setTcpKeepAliveTime0", "(II)V", |t, a| set(t, a[0] as i32, IPPROTO_TCP, TCP_KEEPIDLE, a[1] as i32));
    r.add(C, "setTcpKeepAliveIntvl0", "(II)V", |t, a| set(t, a[0] as i32, IPPROTO_TCP, TCP_KEEPINTVL, a[1] as i32));
    r.add(C, "setTcpKeepAliveProbes0", "(II)V", |t, a| set(t, a[0] as i32, IPPROTO_TCP, TCP_KEEPCNT, a[1] as i32));
    r.add(C, "getTcpKeepAliveTime0", "(I)I", |t, a| ret_int(get(t, a[0] as i32, IPPROTO_TCP, TCP_KEEPIDLE)?));
    r.add(C, "getTcpKeepAliveIntvl0", "(I)I", |t, a| ret_int(get(t, a[0] as i32, IPPROTO_TCP, TCP_KEEPINTVL)?));
    r.add(C, "getTcpKeepAliveProbes0", "(I)I", |t, a| ret_int(get(t, a[0] as i32, IPPROTO_TCP, TCP_KEEPCNT)?));
    // (fd, isIPv6, value)
    r.add(C, "setIpDontFragment0", "(IZZ)V", |t, a| set(t, a[0] as i32, if a[1] != 0 { IPPROTO_IPV6 } else { IPPROTO_IP }, IP_DONTFRAGMENT, a[2] as i32));
    r.add(C, "getIpDontFragment0", "(IZ)Z", |t, a| ret_bool(get(t, a[0] as i32, if a[1] != 0 { IPPROTO_IPV6 } else { IPPROTO_IP }, IP_DONTFRAGMENT)? != 0));
}
