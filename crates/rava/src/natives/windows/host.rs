//! What Windows says about itself, as HotSpot's `java_props_md.c` reads it: the version, the
//! user's locale and ANSI code page, the profile directory, user name and temporary directory.

use crate::natives::props::Host;
use windows_sys::Win32::Globalization::{GetACP, GetUserDefaultLocaleName};
use windows_sys::Win32::Storage::FileSystem::GetTempPathW;

#[repr(C)]
struct OsVersionInfo {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    csd: [u16; 128],
}

#[link(name = "ntdll")]
extern "system" {
    fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
}

fn wide_to_string(w: &[u16]) -> String {
    String::from_utf16_lossy(&w[..w.iter().position(|&c| c == 0).unwrap_or(w.len())])
}

pub(in crate::natives) fn host() -> Host {
    // The real version (GetVersionEx answers what the executable's manifest asks for).
    // SAFETY: RtlGetVersion fills a struct whose size is set.
    let v = unsafe {
        let mut v: OsVersionInfo = std::mem::zeroed();
        v.size = std::mem::size_of::<OsVersionInfo>() as u32;
        RtlGetVersion(&mut v);
        v
    };
    let os_name = match (v.major, v.minor, v.build) {
        (10, 0, b) if b >= 22000 => "Windows 11",
        (10, 0, _) => "Windows 10",
        (6, 3, _) => "Windows 8.1",
        (6, 2, _) => "Windows 8",
        _ => "Windows (unknown)",
    };
    let mut name = [0u16; 85];
    // SAFETY: fills a buffer of LOCALE_NAME_MAX_LENGTH.
    unsafe { GetUserDefaultLocaleName(name.as_mut_ptr(), name.len() as i32) };
    let locale = wide_to_string(&name);
    let mut parts = locale.split('-');
    let language = parts.next().unwrap_or("en").to_string();
    let country = parts.last().filter(|c| c.len() == 2 && c.chars().all(|ch| ch.is_ascii_uppercase())).unwrap_or("").to_string();
    // SAFETY: a plain query.
    let acp = unsafe { GetACP() };
    let codeset = if acp == 65001 { "UTF-8".to_string() } else { format!("Cp{acp}") };
    let mut tmp = [0u16; 261];
    // SAFETY: fills a buffer of MAX_PATH + 1.
    let n = unsafe { GetTempPathW(tmp.len() as u32, tmp.as_mut_ptr()) } as usize;
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "?".into());
    Host {
        language,
        country,
        codeset,
        tmpdir: String::from_utf16_lossy(&tmp[..n.min(tmp.len())]),
        os_name: os_name.into(),
        os_version: format!("{}.{}", v.major, v.minor),
        home: env("USERPROFILE"),
        user: env("USERNAME"),
    }
}
