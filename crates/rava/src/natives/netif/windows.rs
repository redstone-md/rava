//! Interfaces as the JDK's Windows `NetworkInterface` code finds them: every adapter
//! `GetAdaptersAddresses` lists, named by its type and a count (`eth0`, `wlan0`, `lo`, `net3`),
//! its description as the display name, its unicast addresses with their prefixes.

use super::{Addr, Flags, Iface};
use windows_sys::Win32::NetworkManagement::IpHelper::{GetAdaptersAddresses, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6};

/// `IF_TYPE_*` (ipifcons.h) and `IP_ADAPTER_NO_MULTICAST`.
const ETHERNET: u32 = 6;
const TOKEN_RING: u32 = 9;
const FDDI: u32 = 15;
const PPP: u32 = 23;
const LOOPBACK: u32 = 24;
const SLIP: u32 = 28;
const WIFI: u32 = 71;
const NO_MULTICAST: u32 = 0x10;
const OPER_UP: i32 = 1;

/// An adapter's facts beyond its addresses.
struct Adapter {
    iface: Iface,
    mtu: u32,
    mac: Vec<u8>,
}

fn wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: a NUL-terminated string in the adapters' buffer.
    unsafe {
        let n = (0..).take_while(|&i| *p.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

fn adapters() -> Vec<Adapter> {
    let mut size = 16u32 << 10;
    let mut buf: Vec<u64> = Vec::new();
    // SAFETY: the list goes into a buffer of `size` bytes, grown while the call asks for more.
    let ok = loop {
        buf.resize(size as usize / 8 + 1, 0);
        let r = unsafe { GetAdaptersAddresses(AF_UNSPEC as u32, GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER, std::ptr::null(), buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH, &mut size) };
        match r {
            0 => break true,
            111 /* ERROR_BUFFER_OVERFLOW */ => continue,
            _ => break false,
        }
    };
    let mut out = Vec::new();
    if !ok {
        return out;
    }
    let mut counts = std::collections::HashMap::new();
    let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    // SAFETY: walks the linked lists GetAdaptersAddresses filled.
    unsafe {
        while !p.is_null() {
            let a = &*p;
            p = a.Next;
            let prefix = match a.IfType {
                ETHERNET => "eth",
                TOKEN_RING => "tr",
                FDDI => "fddi",
                PPP => "ppp",
                SLIP => "sl",
                LOOPBACK => "lo",
                WIFI => "wlan",
                _ => "net",
            };
            let n = counts.entry(prefix).or_insert(0);
            let name = if prefix == "lo" && *n == 0 { "lo".to_string() } else { format!("{prefix}{n}") };
            *n += 1;
            let index = if a.Anonymous1.Anonymous.IfIndex != 0 { a.Anonymous1.Anonymous.IfIndex } else { a.Ipv6IfIndex } as i32;
            let mut addrs = Vec::new();
            let mut u = a.FirstUnicastAddress;
            while !u.is_null() {
                let ua = &*u;
                u = ua.Next;
                let sa = ua.Address.lpSockaddr;
                let prefix_len = ua.OnLinkPrefixLength as u16;
                match (*sa).sa_family {
                    AF_INET => {
                        let ip = u32::from_be((*(sa as *const SOCKADDR_IN)).sin_addr.S_un.S_addr);
                        let mask = if prefix_len == 0 { 0 } else { u32::MAX << (32 - prefix_len.min(32)) };
                        let broadcast = (a.IfType != LOOPBACK).then_some(ip | !mask);
                        addrs.push(Addr::V4(ip, prefix_len, broadcast));
                    }
                    AF_INET6 => {
                        let ip = (*(sa as *const SOCKADDR_IN6)).sin6_addr.u.Byte;
                        addrs.push(Addr::V6(ip, prefix_len, a.Ipv6IfIndex));
                    }
                    _ => {}
                }
            }
            let flags = Flags { up: a.OperStatus == OPER_UP, loopback: a.IfType == LOOPBACK, p2p: a.IfType == PPP || a.IfType == SLIP, multicast: a.Anonymous2.Flags & NO_MULTICAST == 0 };
            let mac = a.PhysicalAddress[..a.PhysicalAddressLength as usize].to_vec();
            out.push(Adapter { iface: Iface { name, display: wide(a.Description), index, flags, addrs }, mtu: a.Mtu, mac });
        }
    }
    out
}

pub(super) fn interfaces() -> Vec<Iface> {
    adapters().into_iter().map(|a| a.iface).collect()
}

fn adapter(f: &Iface) -> Option<Adapter> {
    adapters().into_iter().find(|a| a.iface.name == f.name)
}

pub(super) fn mtu(f: &Iface) -> Option<i32> {
    adapter(f).map(|a| a.mtu as i32)
}

pub(super) fn hardware_address(f: &Iface) -> Option<Vec<u8>> {
    adapter(f).map(|a| a.mac)
}
