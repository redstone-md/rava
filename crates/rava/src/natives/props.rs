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
    let env = |k: &str| std::env::var(k).ok();
    Some(match field {
        "display_country" | "format_country" => Some(locale().country.clone()).filter(|c| !c.is_empty())?,
        "display_language" | "format_language" => locale().language.clone(),
        // `native_encoding` is JDK 25's name for `file_encoding`.
        "file_encoding" | "native_encoding" | "sun_jnu_encoding" => locale().codeset.clone(),
        "stdout_encoding" | "stderr_encoding" | "stdin_encoding" => return None,
        "file_separator" => "/".into(),
        "java_io_tmpdir" => "/tmp".into(),
        "line_separator" => "\n".into(),
        "os_arch" => if cfg!(target_arch = "x86_64") { "amd64".into() } else { std::env::consts::ARCH.into() },
        "os_name" => "Linux".into(),
        "os_version" => os_release(),
        "path_separator" => ":".into(),
        "sun_arch_data_model" => "64".into(),
        "sun_cpu_endian" => "little".into(),
        "sun_io_unicode_encoding" => "UnicodeLittle".into(),
        "sun_os_patch_level" => "unknown".into(),
        "user_dir" => std::env::current_dir().ok()?.to_string_lossy().into_owned(),
        "user_home" => env("HOME").unwrap_or_else(|| "?".into()),
        "user_name" => env("USER").unwrap_or_else(|| "?".into()),
        _ => return None,
    })
}

struct Locale {
    language: String,
    country: String,
    codeset: String,
}

/// The process locale as HotSpot's `ParseLocale` reads it: `setlocale(LC_ALL, "")`, the
/// `LC_CTYPE` name (plain `C`/`POSIX` meaning en_US) and `nl_langinfo(CODESET)`.
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

fn os_release() -> String {
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
        ("java.library.path".into(), "/usr/java/packages/lib:/usr/lib64:/lib64:/lib:/usr/lib".into()),
        ("java.class.path".into(), v.options.class_path.clone()),
        ("sun.java.launcher".into(), "SUN_STANDARD".into()),
        ("sun.management.compiler".into(), "rava tiered".into()),
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
