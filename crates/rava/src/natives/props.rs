//! `jdk.internal.util.SystemProps$Raw`: platform defaults and VM-supplied properties.

use super::{ret_ref, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::runtime as rt;
use crate::thread::{JResult, Thread};
use crate::vm::vm;

const RAW: &str = "jdk/internal/util/SystemProps$Raw";

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(RAW, "platformProperties", "()[Ljava/lang/String;", platform_properties);
    r.add(RAW, "vmProperties", "()[Ljava/lang/String;", vm_properties);
}

fn platform_value(field: &str) -> Option<String> {
    let h = host();
    Some(match field {
        "display_country" | "format_country" => Some(h.country.clone()).filter(|c| !c.is_empty())?,
        "display_language" | "format_language" => h.language.clone(),
        // `native_encoding` is JDK 25's name for `file_encoding`.
        "file_encoding" | "native_encoding" | "sun_jnu_encoding" => h.codeset.clone(),
        "stdout_encoding" | "stderr_encoding" | "stdin_encoding" => return None,
        "file_separator" => if cfg!(windows) { "\\".into() } else { "/".into() },
        "java_io_tmpdir" => h.tmpdir.clone(),
        "line_separator" => if cfg!(windows) { "\r\n".into() } else { "\n".into() },
        "os_arch" => if cfg!(target_arch = "x86_64") { "amd64".into() } else { std::env::consts::ARCH.into() },
        "os_name" => h.os_name.clone(),
        "os_version" => h.os_version.clone(),
        "path_separator" => if cfg!(windows) { ";".into() } else { ":".into() },
        "sun_arch_data_model" => "64".into(),
        "sun_cpu_endian" => "little".into(),
        "sun_io_unicode_encoding" => "UnicodeLittle".into(),
        "sun_os_patch_level" => "unknown".into(),
        "user_dir" => std::env::current_dir().ok()?.to_string_lossy().into_owned(),
        "user_home" => h.home.clone(),
        "user_name" => h.user.clone(),
        _ => return None,
    })
}

/// What the host says about itself, as HotSpot reads it.
pub(super) struct Host {
    pub language: String,
    pub country: String,
    pub codeset: String,
    pub tmpdir: String,
    pub os_name: String,
    pub os_version: String,
    pub home: String,
    pub user: String,
}

fn host() -> &'static Host {
    static H: std::sync::OnceLock<Host> = std::sync::OnceLock::new();
    #[cfg(unix)]
    return H.get_or_init(|| {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "?".into());
        let l = locale();
        Host {
            language: l.language.clone(),
            country: l.country.clone(),
            codeset: l.codeset.clone(),
            tmpdir: tmpdir(),
            os_name: if cfg!(target_os = "macos") { "Mac OS X".into() } else { "Linux".into() },
            os_version: os_release(),
            home: env("HOME"),
            user: env("USER"),
        }
    });
    #[cfg(windows)]
    return H.get_or_init(super::windows::host);
}

#[cfg(unix)]
struct Locale {
    language: String,
    country: String,
    codeset: String,
}

/// The process locale as HotSpot's `ParseLocale` reads it: `setlocale(LC_ALL, "")`, the
/// `LC_CTYPE` name (plain `C`/`POSIX` meaning en_US) and `nl_langinfo(CODESET)`.
#[cfg(unix)]
fn locale() -> &'static Locale {
    static L: std::sync::OnceLock<Locale> = std::sync::OnceLock::new();
    L.get_or_init(|| {
        // SAFETY: called once, before Java code can observe the C locale.
        let (name, codeset) = unsafe {
            libc::setlocale(libc::LC_ALL, c"".as_ptr());
            let lc = libc::setlocale(libc::LC_CTYPE, std::ptr::null());
            let name = if lc.is_null() { String::new() } else { std::ffi::CStr::from_ptr(lc).to_string_lossy().into_owned() };
            let cs = libc::nl_langinfo(libc::CODESET);
            (name, if cs.is_null() { String::new() } else { std::ffi::CStr::from_ptr(cs).to_string_lossy().into_owned() })
        };
        let base = name.split(['.', '@']).next().unwrap_or("");
        let (language, country) = match base.split_once('_') {
            _ if name.is_empty() || name == "C" || name == "POSIX" => ("en".to_string(), "US".to_string()),
            // "C.UTF-8" and the like: HotSpot maps the language and leaves no country.
            _ if base == "C" || base == "POSIX" => ("en".to_string(), String::new()),
            Some((l, c)) => (l.to_string(), c.to_string()),
            None => (base.to_string(), String::new()),
        };
        let codeset = match codeset.as_str() {
            "" => "UTF-8".to_string(),
            "646" => "ISO646-US".to_string(),
            _ => codeset,
        };
        Locale { language, country, codeset }
    })
}

/// The temporary directory: `/tmp`, or on macOS the per-user directory HotSpot reports.
#[cfg(unix)]
fn tmpdir() -> String {
    #[cfg(target_os = "macos")]
    {
        let mut buf = [0u8; 1024];
        // SAFETY: confstr writes at most `buf.len()` bytes.
        let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if n > 0 && n <= buf.len() {
            return String::from_utf8_lossy(&buf[..n - 1]).into_owned();
        }
    }
    "/tmp".into()
}

/// The system's version: the kernel release on Linux, the product version on macOS.
#[cfg(unix)]
fn os_release() -> String {
    #[cfg(target_os = "macos")]
    {
        let mut buf = [0u8; 64];
        let mut len = buf.len();
        // SAFETY: sysctlbyname fills at most `len` bytes.
        if unsafe { libc::sysctlbyname(c"kern.osproductversion".as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) } == 0 {
            return String::from_utf8_lossy(&buf[..len.saturating_sub(1)]).into_owned();
        }
    }
    // SAFETY: uname fills a caller-provided struct.
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut u) != 0 {
            return "unknown".into();
        }
        std::ffi::CStr::from_ptr(u.release.as_ptr()).to_string_lossy().into_owned()
    }
}

/// The platform properties in the layout `SystemProps.Raw` declares: its `_<name>_NDX`
/// constants place each one and `FIXED_LENGTH` sizes the array (both differ between JDKs).
fn platform_properties(t: &mut Thread, _: &[u64]) -> JResult<u64> {
    let raw = crate::loader::load(RAW).expect("SystemProps.Raw");
    let constant = |f: &crate::klass::FieldDesc| raw.statics[f.offset as usize].load(std::sync::atomic::Ordering::Relaxed) as i32;
    let statics = || raw.fields.iter().filter(|f| f.is_static() && f.desc.as_str() == "I");
    let len = statics().find(|f| f.name.as_str() == "FIXED_LENGTH").map_or(0, constant).max(0) as usize;
    let mut values: Vec<Option<String>> = vec![None; len];
    for f in statics() {
        let Some(name) = f.name.as_str().strip_prefix('_').and_then(|n| n.strip_suffix("_NDX")) else { continue };
        if let Some(slot) = values.get_mut(constant(f) as usize) {
            *slot = platform_value(name);
        }
    }
    ret_ref(string_array(t, &values)?)
}

/// `java.library.path` as HotSpot builds it for the system: on Linux `LD_LIBRARY_PATH` and then
/// the system directories.
#[cfg(target_os = "linux")]
fn default_library_path() -> String {
    let system = "/usr/java/packages/lib:/usr/lib64:/lib64:/lib:/usr/lib";
    match std::env::var("LD_LIBRARY_PATH") {
        Ok(l) if !l.is_empty() => format!("{l}:{system}"),
        _ => system.into(),
    }
}

/// On macOS `JAVA_LIBRARY_PATH`, the Java extension directories, `/usr/lib/java` and `.`.
#[cfg(target_os = "macos")]
fn default_library_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let dirs = format!("{home}/Library/Java/Extensions:/Library/Java/Extensions:/Network/Library/Java/Extensions:/System/Library/Java/Extensions:/usr/lib/java:.");
    match std::env::var("JAVA_LIBRARY_PATH") {
        Ok(l) if !l.is_empty() => format!("{l}:{dirs}"),
        _ => dirs,
    }
}

/// On Windows LoadLibrary's own order: the launcher's directory, the system-wide Java
/// directory, the system and Windows directories, `PATH`, and `.`.
#[cfg(windows)]
fn default_library_path() -> String {
    use windows_sys::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};
    let dir = |f: unsafe extern "system" fn(*mut u16, u32) -> u32| {
        let mut buf = [0u16; 260];
        // SAFETY: a buffer of its stated length.
        let n = unsafe { f(buf.as_mut_ptr(), buf.len() as u32) } as usize;
        String::from_utf16_lossy(&buf[..n.min(buf.len())])
    };
    let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.display().to_string())).unwrap_or_default();
    let windows = dir(GetWindowsDirectoryW);
    let mut parts = vec![exe, format!("{windows}\\Sun\\Java\\bin"), dir(GetSystemDirectoryW), windows];
    if let Ok(path) = std::env::var("PATH") {
        parts.push(path);
    }
    parts.push(".".into());
    parts.join(";")
}

fn vm_properties(t: &mut Thread, _: &[u64]) -> JResult<u64> {
    let v = vm();
    let home = v.options.java_home.to_string_lossy().into_owned();
    let mut pairs: Vec<(String, String)> = vec![
        ("java.home".into(), home.clone()),
        ("java.vm.specification.name".into(), "Java Virtual Machine Specification".into()),
        ("java.vm.specification.vendor".into(), "Oracle Corporation".into()),
        ("java.vm.specification.version".into(), "21".into()),
        ("java.vm.name".into(), "rava".into()),
        ("java.vm.vendor".into(), "rava".into()),
        ("java.vm.version".into(), env!("CARGO_PKG_VERSION").into()),
        ("java.vm.info".into(), "mixed mode".into()),
        ("jdk.debug".into(), "release".into()),
        ("sun.boot.library.path".into(), format!("{home}/lib")),
        ("java.library.path".into(), default_library_path()),
        ("java.class.path".into(), v.options.class_path.clone()),
        ("sun.java.launcher".into(), "SUN_STANDARD".into()),
        ("sun.management.compiler".into(), "rava tiered".into()),
        // rava has no windowing AWT (no displays, frames or event loop), so it is a headless
        // runtime on every system, as a JDK without a display: programs that would open a
        // window (a Minecraft server's console GUI) take their headless path. -D overrides it.
        ("java.awt.headless".into(), "true".into()),
    ];
    pairs.extend(v.options.properties.iter().cloned());
    let flat: Vec<Option<String>> = pairs.into_iter().flat_map(|(k, v)| [Some(k), Some(v)]).collect();
    ret_ref(string_array(t, &flat)?)
}

/// A `String[]` with the given (possibly null) elements.
pub fn string_array(t: &mut Thread, values: &[Option<String>]) -> JResult<Oop> {
    let string_class = crate::loader::boot_class!("java/lang/String").expect("String");
    let array_class = vm().array_class_of(string_class).expect("String[]");
    let arr = rt::alloc_array(t, array_class, values.len() as i32)?;
    for (i, v) in values.iter().enumerate() {
        if let Some(s) = v {
            let js = rt::new_string(t, s)?;
            // SAFETY: `arr` has values.len() reference elements.
            unsafe { obj::store_elem(arr, i as usize, js) };
        }
    }
    Ok(arr)
}
