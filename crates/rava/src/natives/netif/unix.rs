//! Interfaces as the JDK's Unix `NetworkInterface.c` finds them: IPv4 addresses from
//! `getifaddrs`, then IPv6 ones from `/proc/net/if_inet6` (Linux) or `getifaddrs` (macOS);
//! MTU and hardware address from `/sys` or `ioctl` and the link-level entries.

use super::{Addr, Flags, Iface};

/// Adds `addr` of interface `name` as the JDK's `addif` does: a new interface goes first in the
/// list, and so does each new address within its interface.
pub(super) fn add(out: &mut Vec<Iface>, name: &str, index: i32, flags: Flags, addr: Addr) {
    match out.iter_mut().find(|f| f.name == name) {
        Some(f) => f.addrs.insert(0, addr),
        None => out.insert(0, Iface { name: name.to_owned(), display: name.to_owned(), index, flags, addrs: vec![addr] }),
    }
}

/// An interface's flags from its `IFF_*` bits (up means up and running, as in the JDK).
fn flags(bits: u32) -> Flags {
    let has = |b: libc::c_int| bits & b as u32 != 0;
    Flags { up: has(libc::IFF_UP) && has(libc::IFF_RUNNING), loopback: has(libc::IFF_LOOPBACK), p2p: has(libc::IFF_POINTOPOINT), multicast: has(libc::IFF_MULTICAST) }
}

/// The IPv6 addresses `/proc/net/if_inet6` lists: address, interface index, prefix length,
/// scope, flags and interface name per line.
fn parse_if_inet6(text: &str) -> Vec<(String, i32, Addr)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 6 || f[0].len() != 32 {
            continue;
        }
        let mut ip = [0u8; 16];
        for (i, b) in ip.iter_mut().enumerate() {
            *b = u8::from_str_radix(&f[0][2 * i..2 * i + 2], 16).unwrap_or(0);
        }
        let (Ok(index), Ok(prefix)) = (i32::from_str_radix(f[1], 16), u16::from_str_radix(f[2], 16)) else { continue };
        out.push((f[5].to_owned(), index, Addr::V6(ip, prefix, index as u32)));
    }
    out
}

/// Whether the host has IPv6, as the JDK decides before listing IPv6 addresses.
fn ipv6_available() -> bool {
    // SAFETY: a probe socket, closed at once.
    let s = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0) };
    if s < 0 {
        return false;
    }
    unsafe { libc::close(s) };
    std::path::Path::new("/proc/net/if_inet6").exists()
}

pub(super) fn interfaces() -> Vec<Iface> {
    let mut out: Vec<Iface> = Vec::new();
    let mut bits: Vec<(String, u32)> = Vec::new();
    // IPv6 addresses getifaddrs lists (used where there is no /proc), added after the IPv4 ones.
    let mut v6: Vec<(String, i32, u32, Addr)> = Vec::new();
    // SAFETY: getifaddrs allocates a list this walks and frees.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut p = head;
        while !p.is_null() {
            let ifa = &*p;
            p = ifa.ifa_next;
            let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
            if !bits.iter().any(|(n, _)| *n == name) {
                bits.push((name.clone(), ifa.ifa_flags));
            }
            if ifa.ifa_addr.is_null() {
                continue;
            }
            if (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET6 {
                let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                let mut ip = sa.sin6_addr.s6_addr;
                // The KAME stack embeds the scope in a link-local address; the JDK removes it.
                if ip[0] == 0xfe && ip[1] & 0xc0 == 0x80 {
                    ip[2] = 0;
                    ip[3] = 0;
                }
                let prefix = if ifa.ifa_netmask.is_null() {
                    0
                } else {
                    (*(ifa.ifa_netmask as *const libc::sockaddr_in6)).sin6_addr.s6_addr.iter().map(|b| b.count_ones() as u16).sum()
                };
                let index = libc::if_nametoindex(ifa.ifa_name) as i32;
                v6.push((name, index, ifa.ifa_flags, Addr::V6(ip, prefix, index as u32)));
                continue;
            }
            if (*ifa.ifa_addr).sa_family as i32 != libc::AF_INET {
                continue;
            }
            let v4 = |sa: *mut libc::sockaddr| u32::from_be((*(sa as *const libc::sockaddr_in)).sin_addr.s_addr);
            let ip = v4(ifa.ifa_addr);
            let prefix = if ifa.ifa_netmask.is_null() { 0 } else { v4(ifa.ifa_netmask).count_ones() as u16 };
            #[cfg(target_os = "linux")]
            let peer = ifa.ifa_ifu;
            #[cfg(not(target_os = "linux"))]
            let peer = ifa.ifa_dstaddr;
            let broadcast = (ifa.ifa_flags & libc::IFF_BROADCAST as u32 != 0 && !peer.is_null()).then(|| v4(peer));
            let index = libc::if_nametoindex(ifa.ifa_name) as i32;
            add(&mut out, &name, index, flags(ifa.ifa_flags), Addr::V4(ip, prefix, broadcast));
        }
        libc::freeifaddrs(head);
    }
    if cfg!(target_os = "linux") {
        if ipv6_available() {
            let text = std::fs::read_to_string("/proc/net/if_inet6").unwrap_or_default();
            for (name, index, addr) in parse_if_inet6(&text) {
                let fl = bits.iter().find(|(n, _)| *n == name).map_or(0, |&(_, f)| f);
                add(&mut out, &name, index, flags(fl), addr);
            }
        }
    } else {
        for (name, index, fl, addr) in v6 {
            add(&mut out, &name, index, flags(fl), addr);
        }
    }
    out
}

/// An interface's MTU.
pub(super) fn mtu(f: &Iface) -> Option<i32> {
    let name = f.name.as_str();
    #[cfg(target_os = "linux")]
    return std::fs::read_to_string(format!("/sys/class/net/{name}/mtu")).ok()?.trim().parse().ok();
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: an ifreq naming the interface, filled by SIOCGIFMTU on a probe socket.
        unsafe {
            let mut req: libc::ifreq = std::mem::zeroed();
            for (d, s) in req.ifr_name.iter_mut().zip(name.bytes().take(libc::IFNAMSIZ - 1)) {
                *d = s as libc::c_char;
            }
            let s = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
            // `_IOWR('i', 51, struct ifreq)` in <sys/sockio.h>.
            const SIOCGIFMTU: libc::c_ulong = 0xc020_6933;
            let r = libc::ioctl(s, SIOCGIFMTU, &mut req);
            libc::close(s);
            (r == 0).then_some(req.ifr_ifru.ifru_mtu)
        }
    }
}

/// An interface's hardware address.
pub(super) fn hardware_address(f: &Iface) -> Option<Vec<u8>> {
    let name = f.name.as_str();
    #[cfg(target_os = "linux")]
    return std::fs::read_to_string(format!("/sys/class/net/{name}/address"))
        .ok()
        .map(|s| s.trim().split(':').filter_map(|h| u8::from_str_radix(h, 16).ok()).collect());
    #[cfg(not(target_os = "linux"))]
    // SAFETY: walks the list getifaddrs allocates (its link-level entries) and frees it.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return None;
        }
        let mut found = None;
        let mut p = head;
        while !p.is_null() {
            let ifa = &*p;
            p = ifa.ifa_next;
            if ifa.ifa_addr.is_null() || (*ifa.ifa_addr).sa_family as i32 != libc::AF_LINK || std::ffi::CStr::from_ptr(ifa.ifa_name).to_bytes() != name.as_bytes() {
                continue;
            }
            let dl = &*(ifa.ifa_addr as *const libc::sockaddr_dl);
            let data = dl.sdl_data.as_ptr() as *const u8;
            found = Some(std::slice::from_raw_parts(data.add(dl.sdl_nlen as usize), dl.sdl_alen as usize).to_vec());
            break;
        }
        libc::freeifaddrs(head);
        found
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::natives::netif::Flags;

    #[test]
    fn ipv6_addresses_follow_the_jdk_order() {
        let text = "00000000000000000000000000000001 01 80 10 80       lo\n\
                    fe800000000000000000000000000002 02 40 20 80     eth0\n";
        let mut out = Vec::new();
        add(&mut out, "lo", 1, Flags::default(), Addr::V4(0x7f00_0001, 8, None));
        add(&mut out, "eth0", 2, Flags::default(), Addr::V4(0x0a00_0002, 24, Some(0x0a00_00ff)));
        for (name, index, addr) in parse_if_inet6(text) {
            add(&mut out, &name, index, Flags::default(), addr);
        }
        let names: Vec<&str> = out.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["eth0", "lo"]);
        let mut loopback = [0u8; 16];
        loopback[15] = 1;
        assert_eq!(out[1].addrs, [Addr::V6(loopback, 128, 1), Addr::V4(0x7f00_0001, 8, None)]);
        assert!(matches!(out[0].addrs[0], Addr::V6(ip, 64, 2) if ip[0] == 0xfe && ip[1] == 0x80));
    }
}
