//! `sun.net.dns.ResolverConfigurationImpl`, as the JDK's Windows `ResolverConfigurationImpl.c`:
//! the search list is the global `SearchList` of the TCP/IP parameters followed by each up
//! adapter's DNS suffix, the name servers are each up adapter's DNS server addresses, both
//! joined by commas into the class's `os_searchlist` and `os_nameservers`.

use crate::heap::Oop;
use crate::natives::{ret_int, ret_void, NativeRegistry};
use crate::runtime as rt;
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use std::net::{Ipv4Addr, Ipv6Addr};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, NotifyAddrChange, GAA_FLAG_INCLUDE_ALL_INTERFACES, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_FRIENDLY_NAME, GAA_FLAG_SKIP_MULTICAST, GAA_FLAG_SKIP_UNICAST, IP_ADAPTER_ADDRESSES_LH,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

const C: &str = "sun/net/dns/ResolverConfigurationImpl";
/// `ERROR_BUFFER_OVERFLOW`: the adapter list outgrew the buffer.
const OVERFLOW: u32 = 111;

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(C, "init0", "()V", |_, _| ret_void());
    r.add(C, "loadDNSconfig0", "()V", |t, _| {
        // As the JDK: on failure the fields keep what an earlier load put there.
        if let Some((search, servers)) = load() {
            set_static(t, "os_searchlist", &search)?;
            set_static(t, "os_nameservers", &servers)?;
        }
        ret_void()
    });
    // Waits, outside Java, for the next change of an interface address: 0, or -1 on failure.
    r.add(C, "notifyAddrChange0", "()I", |t, _| {
        // SAFETY: the synchronous form, with no handle and no overlapped structure.
        let rc = crate::gc::safepoint::blocking(t, || unsafe { NotifyAddrChange(std::ptr::null_mut(), std::ptr::null()) });
        ret_int(if rc == 0 { 0 } else { -1 })
    });
}

fn set_static(t: &mut Thread, name: &str, value: &str) -> JResult<()> {
    let k = crate::loader::boot_class!("sun/net/dns/ResolverConfigurationImpl").expect("ResolverConfigurationImpl");
    let f = k.declared_field(Symbol::intern(name), Symbol::intern("Ljava/lang/String;")).expect("ResolverConfigurationImpl string field");
    let s: Oop = rt::new_string(t, value)?;
    k.statics[f.offset as usize].store(s as u64, std::sync::atomic::Ordering::Release);
    Ok(())
}

fn append(list: &mut String, item: &str) {
    if item.is_empty() {
        return;
    }
    if !list.is_empty() {
        list.push(',');
    }
    list.push_str(item);
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The global search list of the TCP/IP parameters, empty when there is none.
fn global_search_list() -> String {
    let (key, value) = (wide("SYSTEM\\CurrentControlSet\\Services\\Tcpip\\Parameters"), wide("SearchList"));
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: a string value read into `buf`, whose size in bytes is `len`.
    let rc = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
    if rc != 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)])
}

/// The adapters' list, in a buffer that keeps it alive.
fn adapters() -> Option<Vec<u64>> {
    let flags = GAA_FLAG_SKIP_UNICAST | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_FRIENDLY_NAME | GAA_FLAG_INCLUDE_ALL_INTERFACES;
    let mut size: u32 = 16 * 1024;
    for _ in 0..4 {
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        // SAFETY: a buffer of `size` bytes, 8-byte aligned as the list's structures need.
        let rc = unsafe { GetAdaptersAddresses(AF_UNSPEC as u32, flags, std::ptr::null(), buf.as_mut_ptr().cast(), &mut size) };
        match rc {
            0 => return Some(buf),
            OVERFLOW => continue,
            _ => return None,
        }
    }
    None
}

/// A DNS server's address as `WSAAddressToString` writes it (no port: it is zero).
fn server(sa: *const u8) -> Option<String> {
    // SAFETY: a socket address of the family it names.
    unsafe {
        match (*(sa as *const SOCKADDR_IN)).sin_family {
            AF_INET => Some(Ipv4Addr::from(u32::from_be((*(sa as *const SOCKADDR_IN)).sin_addr.S_un.S_addr)).to_string()),
            AF_INET6 => {
                let a = &*(sa as *const SOCKADDR_IN6);
                let ip = Ipv6Addr::from(a.sin6_addr.u.Byte);
                // The deprecated site-local defaults (fec0:0:0:ffff::1-3) Windows lists for an
                // adapter without configured servers; the JDK leaves them out.
                if ip.segments()[0] == 0xfec0 && ip.segments()[3] == 0xffff {
                    return None;
                }
                let scope = a.Anonymous.sin6_scope_id;
                Some(if scope != 0 { format!("{ip}%{scope}") } else { ip.to_string() })
            }
            _ => None,
        }
    }
}

fn load() -> Option<(String, String)> {
    let mut search = global_search_list();
    let mut servers = String::new();
    let buf = adapters()?;
    let mut a = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !a.is_null() {
        // SAFETY: a node of the list in `buf`, which outlives the walk.
        let ad = unsafe { &*a };
        if ad.OperStatus == IfOperStatusUp {
            let mut d = ad.FirstDnsServerAddress;
            while !d.is_null() {
                // SAFETY: a node of the server list in `buf`.
                let node = unsafe { &*d };
                if let Some(s) = server(node.Address.lpSockaddr as *const u8) {
                    append(&mut servers, &s);
                }
                d = node.Next;
            }
            if !ad.DnsSuffix.is_null() {
                // SAFETY: a NUL-terminated wide string in `buf`.
                let suffix = unsafe {
                    let n = (0..).take_while(|&i| *ad.DnsSuffix.add(i) != 0).count();
                    String::from_utf16_lossy(std::slice::from_raw_parts(ad.DnsSuffix, n))
                };
                append(&mut search, &suffix);
            }
        }
        a = ad.Next;
    }
    Some((search, servers))
}
