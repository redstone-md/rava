//! `java.net.NetworkInterface`: the host's interfaces and their addresses, flags, MTU and
//! hardware address, found as the JDK's `NetworkInterface.c` finds them: IPv4 addresses from
//! `getifaddrs`, then IPv6 ones from `/proc/net/if_inet6` where the host has IPv6.

use super::{ret_bool, ret_int, ret_ref, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime::{self as rt, vm_field};
use crate::thread::{JResult, Thread};
use crate::types::BasicType;
use crate::vm::vm;

const NI: &str = "java/net/NetworkInterface";

vm_field!(ni_name, "java/net/NetworkInterface", "name", "Ljava/lang/String;");
vm_field!(ni_display, "java/net/NetworkInterface", "displayName", "Ljava/lang/String;");
vm_field!(ni_index, "java/net/NetworkInterface", "index", "I");
vm_field!(ni_addrs, "java/net/NetworkInterface", "addrs", "[Ljava/net/InetAddress;");
vm_field!(ni_bindings, "java/net/NetworkInterface", "bindings", "[Ljava/net/InterfaceAddress;");
vm_field!(ni_childs, "java/net/NetworkInterface", "childs", "[Ljava/net/NetworkInterface;");
vm_field!(ia_address, "java/net/InterfaceAddress", "address", "Ljava/net/InetAddress;");
vm_field!(ia_broadcast, "java/net/InterfaceAddress", "broadcast", "Ljava/net/Inet4Address;");
vm_field!(ia_mask, "java/net/InterfaceAddress", "maskLength", "S");
vm_field!(i6_holder, "java/net/Inet6Address", "holder6", "Ljava/net/Inet6Address$Inet6AddressHolder;");
vm_field!(h6_address, "java/net/Inet6Address$Inet6AddressHolder", "ipaddress", "[B");
vm_field!(h6_scope, "java/net/Inet6Address$Inet6AddressHolder", "scope_id", "I");
vm_field!(h6_scope_set, "java/net/Inet6Address$Inet6AddressHolder", "scope_id_set", "Z");
vm_field!(h6_ifname, "java/net/Inet6Address$Inet6AddressHolder", "scope_ifname", "Ljava/net/NetworkInterface;");
vm_field!(h6_ifname_set, "java/net/Inet6Address$Inet6AddressHolder", "scope_ifname_set", "Z");

/// One address of an interface, with its prefix length.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Addr {
    /// Address and broadcast address (host byte order).
    V4(u32, u16, Option<u32>),
    /// Address and scope (the interface's index).
    V6([u8; 16], u16, u32),
}

/// One interface: its addresses and `IFF_*` flags.
#[derive(Debug)]
struct Iface {
    name: String,
    index: i32,
    flags: u32,
    addrs: Vec<Addr>,
}

/// Adds `addr` of interface `name` as the JDK's `addif` does: a new interface goes first in the
/// list, and so does each new address within its interface.
fn add(out: &mut Vec<Iface>, name: &str, index: i32, flags: u32, addr: Addr) {
    match out.iter_mut().find(|f| f.name == name) {
        Some(f) => f.addrs.insert(0, addr),
        None => out.insert(0, Iface { name: name.to_owned(), index, flags, addrs: vec![addr] }),
    }
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

fn interfaces() -> Vec<Iface> {
    let mut out: Vec<Iface> = Vec::new();
    let mut flags: Vec<(String, u32)> = Vec::new();
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
            if !flags.iter().any(|(n, _)| *n == name) {
                flags.push((name.clone(), ifa.ifa_flags));
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
            add(&mut out, &name, index, ifa.ifa_flags, Addr::V4(ip, prefix, broadcast));
        }
        libc::freeifaddrs(head);
    }
    if cfg!(target_os = "linux") {
        if ipv6_available() {
            let text = std::fs::read_to_string("/proc/net/if_inet6").unwrap_or_default();
            for (name, index, addr) in parse_if_inet6(&text) {
                let fl = flags.iter().find(|(n, _)| *n == name).map_or(0, |&(_, f)| f);
                add(&mut out, &name, index, fl, addr);
            }
        }
    } else {
        for (name, index, fl, addr) in v6 {
            add(&mut out, &name, index, fl, addr);
        }
    }
    out
}

/// An interface's MTU.
fn mtu(name: &str) -> Option<i32> {
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
fn hardware_address(name: &str) -> Option<Vec<u8>> {
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

fn find(name: &str, index: i32) -> Option<Iface> {
    interfaces().into_iter().find(|f| f.name == name || (name.is_empty() && f.index == index))
}

fn object_array(t: &mut Thread, class: &str, n: usize) -> JResult<Oop> {
    let k = crate::loader::load(class).expect("a java.net class");
    rt::alloc_array(t, vm().array_class_of(k).expect("its array class"), n as i32)
}

/// A `NetworkInterface` for `f`, its fields set as the JDK's native code sets them.
fn interface_object(t: &mut Thread, f: &Iface) -> JResult<Oop> {
    let k = crate::loader::load(NI).expect("NetworkInterface");
    rt::initialize(t, k)?;
    let o = rt::alloc_instance(t, k)?;
    let name = rt::new_string(t, &f.name)?;
    let addrs = object_array(t, "java/net/InetAddress", f.addrs.len())?;
    let bindings = object_array(t, "java/net/InterfaceAddress", f.addrs.len())?;
    let childs = object_array(t, NI, 0)?;
    let ia_class = crate::loader::boot_class!("java/net/InterfaceAddress").expect("InterfaceAddress");
    rt::initialize(t, ia_class)?;
    for (i, &addr) in f.addrs.iter().enumerate() {
        let (a, b, prefix) = match addr {
            Addr::V4(ip, prefix, broadcast) => {
                let b = match broadcast {
                    Some(b) => super::net::inet4(t, None, b)?,
                    None => std::ptr::null_mut(),
                };
                (super::net::inet4(t, None, ip)?, b, prefix)
            }
            Addr::V6(ip, prefix, scope) => (inet6(t, &ip, scope, o)?, std::ptr::null_mut(), prefix),
        };
        let binding = rt::alloc_instance(t, ia_class)?;
        // SAFETY: fresh objects; offsets from their layouts.
        unsafe {
            obj::write_ref(binding, ia_address(), a);
            obj::write_ref(binding, ia_broadcast(), b);
            obj::write::<i16>(binding, ia_mask(), prefix as i16);
            obj::store_elem(addrs, i, a);
            obj::store_elem(bindings, i, binding);
        }
    }
    // SAFETY: a fresh NetworkInterface.
    unsafe {
        obj::write_ref(o, ni_name(), name);
        obj::write_ref(o, ni_display(), name);
        obj::write::<i32>(o, ni_index(), f.index);
        obj::write_ref(o, ni_addrs(), addrs);
        obj::write_ref(o, ni_bindings(), bindings);
        obj::write_ref(o, ni_childs(), childs);
    }
    Ok(o)
}

/// An `Inet6Address` of interface `nif`, scoped to it as the JDK's native code makes them.
fn inet6(t: &mut Thread, ip: &[u8; 16], scope: u32, nif: Oop) -> JResult<Oop> {
    let a = crate::boot::construct(t, "java/net/Inet6Address", "()V", &[])?;
    let bytes = rt::alloc_array(t, vm().primitive_array_class(BasicType::Byte).expect("[B"), 16)?;
    // SAFETY: a fresh Inet6Address, its holder and a fresh byte[16].
    unsafe {
        obj::array_slice::<u8>(bytes).copy_from_slice(ip);
        let h = obj::read_ref(a, i6_holder());
        obj::write_ref(h, h6_address(), bytes);
        if scope != 0 {
            obj::write::<i32>(h, h6_scope(), scope as i32);
            obj::write::<u8>(h, h6_scope_set(), 1);
            obj::write_ref(h, h6_ifname(), nif);
            obj::write::<u8>(h, h6_ifname_set(), 1);
        }
    }
    Ok(a)
}

/// The address of an `InetAddress`, as the interface lists record them.
fn address_of(a: Oop) -> Option<Addr> {
    if a.is_null() {
        return None;
    }
    // SAFETY: a live InetAddress; an Inet6Address has its holder and a byte[16].
    unsafe {
        if obj::klass(a).name.as_str() == "java/net/Inet6Address" {
            let bytes = obj::read_ref(obj::read_ref(a, i6_holder()), h6_address());
            let mut ip = [0u8; 16];
            ip.copy_from_slice(obj::array_slice::<u8>(bytes));
            return Some(Addr::V6(ip, 0, 0));
        }
    }
    Some(Addr::V4(super::net::ipv4_of(a), 0, None))
}

fn maybe(t: &mut Thread, f: Option<Iface>) -> JResult<u64> {
    match f {
        Some(f) => ret_ref(interface_object(t, &f)?),
        None => ret_ref(std::ptr::null_mut()),
    }
}

/// The interface the native was asked about, by name (or index when the name is null).
fn named(a: &[u64]) -> Option<Iface> {
    let name = if a[0] == 0 { String::new() } else { rt::java_string_to_rust(a[0] as Oop) };
    find(&name, a[1] as i32)
}

fn has_flag(a: &[u64], bit: libc::c_int) -> JResult<u64> {
    ret_bool(named(a).is_some_and(|f| f.flags & bit as u32 != 0))
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(NI, "init", "()V", |_, _| ret_void());
    r.add(NI, "getAll", "()[Ljava/net/NetworkInterface;", |t, _| {
        let all = interfaces();
        let arr = object_array(t, NI, all.len())?;
        for (i, f) in all.iter().enumerate() {
            let o = interface_object(t, f)?;
            // SAFETY: `arr` has one element per interface.
            unsafe { obj::store_elem(arr, i, o) };
        }
        ret_ref(arr)
    });
    r.add(NI, "getByName0", "(Ljava/lang/String;)Ljava/net/NetworkInterface;", |t, a| {
        let name = rt::java_string_to_rust(a[0] as Oop);
        let f = interfaces().into_iter().find(|f| f.name == name);
        maybe(t, f)
    });
    r.add(NI, "getByIndex0", "(I)Ljava/net/NetworkInterface;", |t, a| {
        let f = interfaces().into_iter().find(|f| f.index == a[0] as i32);
        maybe(t, f)
    });
    r.add(NI, "getByInetAddress0", "(Ljava/net/InetAddress;)Ljava/net/NetworkInterface;", |t, a| {
        let wanted = address_of(a[0] as Oop);
        let same = |x: &Addr| match (x, wanted) {
            (Addr::V4(ip, ..), Some(Addr::V4(w, ..))) => *ip == w,
            (Addr::V6(ip, ..), Some(Addr::V6(w, ..))) => *ip == w,
            _ => false,
        };
        let f = interfaces().into_iter().find(|f| f.addrs.iter().any(same));
        maybe(t, f)
    });
    // Up and running, as the JDK has it: an interface without a carrier is down.
    r.add(NI, "isUp0", "(Ljava/lang/String;I)Z", |_, a| {
        let both = (libc::IFF_UP | libc::IFF_RUNNING) as u32;
        ret_bool(named(a).is_some_and(|f| f.flags & both == both))
    });
    r.add(NI, "isLoopback0", "(Ljava/lang/String;I)Z", |_, a| has_flag(a, libc::IFF_LOOPBACK));
    r.add(NI, "supportsMulticast0", "(Ljava/lang/String;I)Z", |_, a| has_flag(a, libc::IFF_MULTICAST));
    r.add(NI, "isP2P0", "(Ljava/lang/String;I)Z", |_, a| has_flag(a, libc::IFF_POINTOPOINT));
    r.add(NI, "getMTU0", "(Ljava/lang/String;I)I", |_, a| {
        ret_int(named(a).and_then(|f| mtu(&f.name)).unwrap_or(-1))
    });
    // The hardware address, or null for none (loopback reports all zeros).
    r.add(NI, "getMacAddr0", "([BLjava/lang/String;I)[B", |t, a| {
        let mac = find(&rt::java_string_to_rust(a[1] as Oop), a[2] as i32)
            .and_then(|f| hardware_address(&f.name))
            .filter(|m: &Vec<u8>| !m.is_empty() && m.iter().any(|&b| b != 0));
        let Some(mac) = mac else { return ret_ref(std::ptr::null_mut()) };
        let arr = rt::alloc_array(t, vm().primitive_array_class(BasicType::Byte).expect("[B"), mac.len() as i32)?;
        // SAFETY: a fresh byte[] of the address's length.
        unsafe { obj::array_slice::<u8>(arr).copy_from_slice(&mac) };
        ret_ref(arr)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_addresses_follow_the_jdk_order() {
        let text = "00000000000000000000000000000001 01 80 10 80       lo\n\
                    fe800000000000000000000000000002 02 40 20 80     eth0\n";
        let mut out = Vec::new();
        add(&mut out, "lo", 1, 0, Addr::V4(0x7f00_0001, 8, None));
        add(&mut out, "eth0", 2, 0, Addr::V4(0x0a00_0002, 24, Some(0x0a00_00ff)));
        for (name, index, addr) in parse_if_inet6(text) {
            add(&mut out, &name, index, 0, addr);
        }
        let names: Vec<&str> = out.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["eth0", "lo"]);
        let mut loopback = [0u8; 16];
        loopback[15] = 1;
        assert_eq!(out[1].addrs, [Addr::V6(loopback, 128, 1), Addr::V4(0x7f00_0001, 8, None)]);
        assert!(matches!(out[0].addrs[0], Addr::V6(ip, 64, 2) if ip[0] == 0xfe && ip[1] == 0x80));
    }
}
