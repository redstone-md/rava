//! The system time zone on Windows, as the JDK's `TimeZone_md.c` finds it: the Windows zone's
//! key name (`W. Europe Standard Time`), mapped to a Java ID through `lib/tzmappings` with the
//! user's country choosing among a zone's regions; `GMT±hh:mm` from the current offset when the
//! zone has no daylight saving rules in use or no mapping.

use windows_sys::Win32::Globalization::{GetGeoInfoW, GetUserGeoID, GEOCLASS_NATION, GEO_ISO2};
use windows_sys::Win32::System::Time::{GetDynamicTimeZoneInformation, GetTimeZoneInformation, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID, TIME_ZONE_INFORMATION};

/// `TIME_ZONE_ID_DAYLIGHT`: daylight saving time is in effect.
const TIME_ZONE_ID_DAYLIGHT_TIME: u32 = 2;

fn text(w: &[u16]) -> String {
    String::from_utf16_lossy(&w[..w.iter().position(|&c| c == 0).unwrap_or(w.len())])
}

/// `GMT±hh:mm` for a bias in minutes (UTC = local + bias), `GMT` for none.
fn offset_id(bias: i32) -> String {
    let offset = -bias;
    if offset == 0 {
        return "GMT".to_string();
    }
    format!("GMT{}{:02}:{:02}", if offset > 0 { '+' } else { '-' }, offset.abs() / 60, offset.abs() % 60)
}

/// The user's country (ISO 3166 two letters), or empty.
fn region() -> String {
    let mut buf = [0u16; 8];
    // SAFETY: a buffer of 8 characters.
    let n = unsafe { GetGeoInfoW(GetUserGeoID(GEOCLASS_NATION), GEO_ISO2, buf.as_mut_ptr(), buf.len() as i32, 0) };
    if n > 0 { text(&buf) } else { String::new() }
}

/// The Java ID `tzmappings` gives Windows zone `name` in `region` (or for every region, `001`).
fn mapped(java_home: &std::path::Path, name: &str, region: &str) -> Option<String> {
    let table = std::fs::read_to_string(java_home.join("lib").join("tzmappings")).ok()?;
    table.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).find_map(|l| {
        let mut items = l.split(':');
        let (win, reg, java) = (items.next()?, items.next()?, items.next()?);
        (win == name && (reg == region || reg == "001")).then(|| java.to_string())
    })
}

/// The current offset from UTC, daylight saving included, as a `GMT±hh:mm` ID.
pub(in crate::natives) fn gmt_offset_id() -> String {
    // SAFETY: an all-zero TIME_ZONE_INFORMATION is valid; the call fills it.
    let mut tzi: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    let kind = unsafe { GetTimeZoneInformation(&mut tzi) };
    let bias = tzi.Bias + if kind == TIME_ZONE_ID_DAYLIGHT_TIME { tzi.DaylightBias } else { tzi.StandardBias };
    offset_id(bias)
}

/// The Java ID of the system time zone.
pub(in crate::natives) fn system_zone(java_home: &std::path::Path) -> String {
    // SAFETY: an all-zero DYNAMIC_TIME_ZONE_INFORMATION is valid; the call fills it.
    let mut dtzi: DYNAMIC_TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { GetDynamicTimeZoneInformation(&mut dtzi) } == TIME_ZONE_ID_INVALID {
        return gmt_offset_id();
    }
    let key = text(&dtzi.TimeZoneKeyName);
    if key.is_empty() {
        return gmt_offset_id();
    }
    if dtzi.DynamicDaylightTimeDisabled != 0 {
        return offset_id(dtzi.Bias);
    }
    mapped(java_home, &key, &region()).unwrap_or_else(gmt_offset_id)
}
