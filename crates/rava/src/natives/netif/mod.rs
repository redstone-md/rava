//! `java.net.NetworkInterface`: the host's interfaces and their addresses, flags, MTU and
//! hardware address, as the JDK's own `NetworkInterface` code reports them. Finding them is per
//! system (`unix`: `getifaddrs` and `/proc`; `windows`: `GetAdaptersAddresses`); making the Java
//! objects is shared.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix::{hardware_address, interfaces, mtu};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::{hardware_address, interfaces, mtu};

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

/// What an interface is: up (and running), the loopback, point-to-point, multicast-capable.
#[derive(Clone, Copy, Debug, Default)]
struct Flags {
    up: bool,
    loopback: bool,
    p2p: bool,
    multicast: bool,
}

/// One interface: its addresses and flags.
#[derive(Debug)]
struct Iface {
    name: String,
    /// What `NetworkInterface.getDisplayName` reports.
    display: String,
    index: i32,
    flags: Flags,
    addrs: Vec<Addr>,
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
    let display = rt::new_string(t, &f.display)?;
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
        obj::write_ref(o, ni_display(), display);
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

fn has_flag(a: &[u64], flag: fn(&Flags) -> bool) -> JResult<u64> {
    ret_bool(named(a).is_some_and(|f| flag(&f.flags)))
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
    r.add(NI, "isUp0", "(Ljava/lang/String;I)Z", |_, a| has_flag(a, |f| f.up));
    r.add(NI, "isLoopback0", "(Ljava/lang/String;I)Z", |_, a| has_flag(a, |f| f.loopback));
    r.add(NI, "supportsMulticast0", "(Ljava/lang/String;I)Z", |_, a| has_flag(a, |f| f.multicast));
    r.add(NI, "isP2P0", "(Ljava/lang/String;I)Z", |_, a| has_flag(a, |f| f.p2p));
    r.add(NI, "getMTU0", "(Ljava/lang/String;I)I", |_, a| {
        ret_int(named(a).and_then(|f| mtu(&f)).unwrap_or(-1))
    });
    // The hardware address, or null for none (loopback reports all zeros).
    r.add(NI, "getMacAddr0", "([BLjava/lang/String;I)[B", |t, a| {
        let mac = find(&rt::java_string_to_rust(a[1] as Oop), a[2] as i32)
            .and_then(|f| hardware_address(&f))
            .filter(|m: &Vec<u8>| !m.is_empty() && m.iter().any(|&b| b != 0));
        let Some(mac) = mac else { return ret_ref(std::ptr::null_mut()) };
        let arr = rt::alloc_array(t, vm().primitive_array_class(BasicType::Byte).expect("[B"), mac.len() as i32)?;
        // SAFETY: a fresh byte[] of the address's length.
        unsafe { obj::array_slice::<u8>(arr).copy_from_slice(&mac) };
        ret_ref(arr)
    });
}
