//! `java.net.NetworkInterface`: the host's interfaces and their IPv4 addresses, flags, MTU
//! and hardware address, read with `getifaddrs` and from `/sys/class/net` as the JDK's
//! `NetworkInterface.c` reads them.

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

/// One interface: its IPv4 addresses (address, prefix length, broadcast) and `IFF_*` flags.
struct Iface {
    name: String,
    index: i32,
    flags: u32,
    addrs: Vec<(u32, u16, Option<u32>)>,
}

fn interfaces() -> Vec<Iface> {
    let mut out: Vec<Iface> = Vec::new();
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
            let i = match out.iter().position(|f| f.name == name) {
                Some(i) => i,
                None => {
                    let index = libc::if_nametoindex(ifa.ifa_name) as i32;
                    out.push(Iface { name, index, flags: ifa.ifa_flags, addrs: Vec::new() });
                    out.len() - 1
                }
            };
            if ifa.ifa_addr.is_null() || (*ifa.ifa_addr).sa_family as i32 != libc::AF_INET {
                continue;
            }
            let v4 = |sa: *mut libc::sockaddr| u32::from_be((*(sa as *const libc::sockaddr_in)).sin_addr.s_addr);
            let ip = v4(ifa.ifa_addr);
            let prefix = if ifa.ifa_netmask.is_null() { 0 } else { v4(ifa.ifa_netmask).count_ones() as u16 };
            let broadcast = (ifa.ifa_flags & libc::IFF_BROADCAST as u32 != 0 && !ifa.ifa_ifu.is_null()).then(|| v4(ifa.ifa_ifu));
            out[i].addrs.push((ip, prefix, broadcast));
        }
        libc::freeifaddrs(head);
    }
    // As the JDK lists them: interfaces with an address, the last found first.
    out.retain(|f| !f.addrs.is_empty());
    out.reverse();
    out
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
    for (i, &(ip, prefix, broadcast)) in f.addrs.iter().enumerate() {
        let a = super::net::inet4(t, None, ip)?;
        let b = match broadcast {
            Some(b) => super::net::inet4(t, None, b)?,
            None => std::ptr::null_mut(),
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
        let ip = (a[0] != 0).then(|| super::net::ipv4_of(a[0] as Oop));
        let f = interfaces().into_iter().find(|f| f.addrs.iter().any(|&(x, _, _)| Some(x) == ip));
        maybe(t, f)
    });
    r.add(NI, "isUp0", "(Ljava/lang/String;I)Z", |_, a| has_flag(a, libc::IFF_UP));
    r.add(NI, "isLoopback0", "(Ljava/lang/String;I)Z", |_, a| has_flag(a, libc::IFF_LOOPBACK));
    r.add(NI, "supportsMulticast0", "(Ljava/lang/String;I)Z", |_, a| has_flag(a, libc::IFF_MULTICAST));
    r.add(NI, "isP2P0", "(Ljava/lang/String;I)Z", |_, a| has_flag(a, libc::IFF_POINTOPOINT));
    r.add(NI, "getMTU0", "(Ljava/lang/String;I)I", |_, a| {
        let mtu = named(a).and_then(|f| std::fs::read_to_string(format!("/sys/class/net/{}/mtu", f.name)).ok()?.trim().parse().ok());
        ret_int(mtu.unwrap_or(-1))
    });
    // The hardware address, or null for none (loopback reports all zeros).
    r.add(NI, "getMacAddr0", "([BLjava/lang/String;I)[B", |t, a| {
        let mac: Option<Vec<u8>> = find(&rt::java_string_to_rust(a[1] as Oop), a[2] as i32)
            .and_then(|f| std::fs::read_to_string(format!("/sys/class/net/{}/address", f.name)).ok())
            .map(|s| s.trim().split(':').filter_map(|h| u8::from_str_radix(h, 16).ok()).collect())
            .filter(|m: &Vec<u8>| !m.is_empty() && m.iter().any(|&b| b != 0));
        let Some(mac) = mac else { return ret_ref(std::ptr::null_mut()) };
        let arr = rt::alloc_array(t, vm().primitive_array_class(BasicType::Byte).expect("[B"), mac.len() as i32)?;
        // SAFETY: a fresh byte[] of the address's length.
        unsafe { obj::array_slice::<u8>(arr).copy_from_slice(&mac) };
        ret_ref(arr)
    });
}
