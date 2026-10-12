//! Interfaces as the JDK's Windows `NetworkInterface` code finds them: every row of
//! `GetIfTable2`, named by `ConvertInterfaceLuidToNameW` (`ethernet_32774`, `loopback_0`), its
//! description as the display name, with the unicast addresses past duplicate detection and the
//! anycast ones from the address tables, listed last first as the JDK links them.

use super::{Addr, Flags, Iface};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    ConvertInterfaceLuidToNameW, FreeMibTable, GetAnycastIpAddressTable, GetIfTable2, GetUnicastIpAddressTable, MIB_ANYCASTIPADDRESS_ROW, MIB_ANYCASTIPADDRESS_TABLE, MIB_IF_ROW2, MIB_IF_TABLE2, MIB_UNICASTIPADDRESS_ROW, MIB_UNICASTIPADDRESS_TABLE,
};
use windows_sys::Win32::NetworkManagement::Ndis::{IfOperStatusUp, NET_IF_ACCESS_POINT_TO_POINT, NET_IF_ADMIN_STATUS_UP};
use windows_sys::Win32::Networking::WinSock::{IpDadStateDeprecated, IpDadStatePreferred, AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_INET};

/// `IF_TYPE_SOFTWARE_LOOPBACK` (ipifcons.h).
const LOOPBACK: u32 = 24;
/// `NDIS_IF_MAX_STRING_SIZE + 1`.
const NAME_CHARS: usize = 257;

/// An interface's facts beyond its addresses.
struct Adapter {
    iface: Iface,
    mtu: u32,
    mac: Vec<u8>,
}

/// A table the IP helper allocated, freed when dropped.
struct Mib<T>(*mut T);

impl<T> Drop for Mib<T> {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a table from GetIfTable2 or Get*IpAddressTable, freed once.
            unsafe { FreeMibTable(self.0 as *const _) };
        }
    }
}

/// The rows of a table laid out as `NumEntries` then the rows.
///
/// SAFETY: `t` is a live MIB table whose rows of `R` follow its count at `offset`.
unsafe fn rows<'a, T, R>(t: *const T, count: u32, offset: usize) -> &'a [R] {
    std::slice::from_raw_parts((t as *const u8).add(offset) as *const R, count as usize)
}

fn text(chars: &[u16]) -> String {
    String::from_utf16_lossy(&chars[..chars.iter().position(|&c| c == 0).unwrap_or(chars.len())])
}

fn name(row: &MIB_IF_ROW2) -> String {
    let mut buf = [0u16; NAME_CHARS];
    // SAFETY: the row's LUID and a buffer of NAME_CHARS characters.
    unsafe { ConvertInterfaceLuidToNameW(&row.InterfaceLuid, buf.as_mut_ptr(), NAME_CHARS) };
    text(&buf)
}

/// An address from a table row; `prefix` None for anycast ones, which carry no prefix.
fn addr(sa: &SOCKADDR_INET, prefix: Option<u8>) -> Option<Addr> {
    // SAFETY: the family says which member of the union is set.
    unsafe {
        match sa.si_family {
            AF_INET => {
                let ip = u32::from_be(sa.Ipv4.sin_addr.S_un.S_addr);
                let broadcast = prefix.map(|p| ip | !(if p == 0 { 0 } else { u32::MAX << (32 - p.min(32) as u32) }));
                Some(Addr::V4(ip, prefix.unwrap_or(0) as u16, broadcast))
            }
            AF_INET6 => Some(Addr::V6(sa.Ipv6.sin6_addr.u.Byte, prefix.unwrap_or(0) as u16, sa.Ipv6.Anonymous.sin6_scope_id)),
            _ => None,
        }
    }
}

fn adapters() -> Vec<Adapter> {
    let (mut ifs, mut uni, mut any): (*mut MIB_IF_TABLE2, *mut MIB_UNICASTIPADDRESS_TABLE, *mut MIB_ANYCASTIPADDRESS_TABLE) = (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: each call allocates its table; the guards free them.
    let ok = unsafe { GetIfTable2(&mut ifs) == 0 && GetUnicastIpAddressTable(AF_UNSPEC, &mut uni) == 0 && GetAnycastIpAddressTable(AF_UNSPEC, &mut any) == 0 };
    let (ifs, uni, any) = (Mib(ifs), Mib(uni), Mib(any));
    if !ok {
        return Vec::new();
    }
    // SAFETY: the tables just filled; rows follow the count at the offset of `Table`.
    let (ifs, uni, any) = unsafe {
        (
            rows::<_, MIB_IF_ROW2>(ifs.0, (*ifs.0).NumEntries, std::mem::offset_of!(MIB_IF_TABLE2, Table)),
            rows::<_, MIB_UNICASTIPADDRESS_ROW>(uni.0, (*uni.0).NumEntries, std::mem::offset_of!(MIB_UNICASTIPADDRESS_TABLE, Table)),
            rows::<_, MIB_ANYCASTIPADDRESS_ROW>(any.0, (*any.0).NumEntries, std::mem::offset_of!(MIB_ANYCASTIPADDRESS_TABLE, Table)),
        )
    };
    ifs.iter()
        .map(|row| {
            // SAFETY: LUIDs are read as their 64-bit value.
            let luid = unsafe { row.InterfaceLuid.Value };
            let unicast = uni.iter().filter(|u| unsafe { u.InterfaceLuid.Value } == luid && (u.DadState == IpDadStatePreferred || u.DadState == IpDadStateDeprecated)).filter_map(|u| addr(&u.Address, Some(u.OnLinkPrefixLength)));
            let anycast = any.iter().filter(|a| unsafe { a.InterfaceLuid.Value } == luid).filter_map(|a| addr(&a.Address, None));
            let mut addrs: Vec<Addr> = unicast.chain(anycast).collect();
            addrs.reverse();
            let flags = Flags {
                up: row.AdminStatus == NET_IF_ADMIN_STATUS_UP && row.OperStatus == IfOperStatusUp,
                loopback: row.Type == LOOPBACK,
                p2p: row.AccessType == NET_IF_ACCESS_POINT_TO_POINT,
                // The JDK assumes it: Windows offers no reliable way to tell.
                multicast: true,
            };
            let mac = row.PhysicalAddress[..(row.PhysicalAddressLength as usize).min(32)].to_vec();
            Adapter { iface: Iface { name: name(row), display: text(&row.Description), index: row.InterfaceIndex as i32, flags, addrs }, mtu: row.Mtu, mac }
        })
        .collect()
}

pub(super) fn interfaces() -> Vec<Iface> {
    adapters().into_iter().map(|a| a.iface).collect()
}

fn adapter(f: &Iface) -> Option<Adapter> {
    adapters().into_iter().find(|a| a.iface.index == f.index)
}

pub(super) fn mtu(f: &Iface) -> Option<i32> {
    adapter(f).map(|a| a.mtu as i32)
}

/// None for interfaces without one, as `getHardwareAddress` reports them.
pub(super) fn hardware_address(f: &Iface) -> Option<Vec<u8>> {
    adapter(f).map(|a| a.mac).filter(|m| !m.is_empty())
}
