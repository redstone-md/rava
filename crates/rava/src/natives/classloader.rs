//! Class-loader natives: defining classes from bytes, boot lookups, `Class.forName0`,
//! and native-library bookkeeping (all VM natives are built in).

use super::{ret_bool, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::classfile::ClassFile;
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::link::LinkError;
use crate::runtime::{self as rt, throw_new, vm_field};
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use crate::vm::vm;

const CL: &str = "java/lang/ClassLoader";

/// The native libraries of the JDK (21 and 25), whose natives the VM implements.
const JDK_LIBRARIES: &[&str] = &[
    "atk-wrapper", "attach", "awt", "awt_headless", "awt_xawt", "dt_socket", "extnet", "fontmanager", "freetype", "instrument", "j2gss",
    "j2pcsc", "j2pkcs11", "jaas", "java", "javajpeg", "jawt", "jdwp", "jimage", "jli", "jsig", "jsound", "jsvml", "lcms", "le",
    "management", "management_agent", "management_ext", "mlib_image", "net", "nio", "prefs", "rmi", "saproc", "sctp", "simdsort",
    "splashscreen", "syslookup", "verify", "zip",
];

/// The packages (internal form, `java/lang`) of the classes the boot loader has defined from the
/// runtime image.
fn boot_packages() -> std::collections::BTreeSet<String> {
    let classes = vm().classes.read().unwrap();
    let image = &vm().image;
    let mut seen = std::collections::BTreeSet::new();
    for &(loader, name) in classes.keys() {
        let Some((package, _)) = name.as_str().rsplit_once('/') else { continue };
        if loader == 0 && !seen.contains(package) && image.module_of_package(package).is_some() {
            seen.insert(package.to_string());
        }
    }
    seen
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(CL, "registerNatives", "()V", |_, _| ret_void());
    r.add(
        CL,
        "defineClass1",
        "(Ljava/lang/ClassLoader;Ljava/lang/String;[BIILjava/security/ProtectionDomain;Ljava/lang/String;)Ljava/lang/Class;",
        define_class1,
    );
    r.add(
        CL,
        "defineClass0",
        "(Ljava/lang/ClassLoader;Ljava/lang/Class;Ljava/lang/String;[BIILjava/security/ProtectionDomain;ZILjava/lang/Object;)Ljava/lang/Class;",
        define_class0,
    );
    r.add(
        CL,
        "defineClass2",
        "(Ljava/lang/ClassLoader;Ljava/lang/String;Ljava/nio/ByteBuffer;IILjava/security/ProtectionDomain;Ljava/lang/String;)Ljava/lang/Class;",
        |t, a| {
            let (loader, name, buffer, off, len) = (a[0] as Oop, a[1] as Oop, a[2] as Oop, a[3] as i32, a[4] as i32);
            // SAFETY: a direct ByteBuffer: `address` is its base; [off, off+len) was validated.
            let bytes = unsafe {
                let base = obj::read::<i64>(buffer, buffer_address()) as usize;
                std::slice::from_raw_parts((base + off as usize) as *const u8, len as usize)
            };
            let owned = bytes.to_vec();
            let k = define_from_bytes(t, loader, owned, if name.is_null() { None } else { Some(internal_name(name)) })?;
            defined(t, k, a[5] as Oop)
        },
    );
    r.add(CL, "findBootstrapClass", "(Ljava/lang/String;)Ljava/lang/Class;", |t, a| {
        let name = internal_name(a[0] as Oop);
        match vm().load_boot_class(Symbol::intern(&name)) {
            Ok(k) => ret_ref(rt::mirror_of(t, k)?),
            Err(_) => ret_ref(std::ptr::null_mut()),
        }
    });
    r.add(CL, "findLoadedClass0", "(Ljava/lang/String;)Ljava/lang/Class;", |t, a| {
        let name = internal_name(a[1] as Oop);
        match vm().find_loaded(a[0] as Oop, Symbol::intern(&name)) {
            Some(k) => ret_ref(rt::mirror_of(t, k)?),
            None => ret_ref(std::ptr::null_mut()),
        }
    });
    r.add(CL, "retrieveDirectives", "()Ljava/lang/AssertionStatusDirectives;", retrieve_directives);
    // The boot loader's packages, as HotSpot reports them: those it has loaded a class of,
    // located at their module in the runtime image (`jrt:/java.base`). `Class.getPackage` of a
    // JDK class and `Package.getPackages` are built on them.
    r.add("jdk/internal/loader/BootLoader", "getSystemPackageNames", "()[Ljava/lang/String;", |t, _| {
        let names: Vec<Option<String>> = boot_packages().into_iter().map(Some).collect();
        ret_ref(super::string_array(t, &names)?)
    });
    r.add("jdk/internal/loader/BootLoader", "getSystemPackageLocation", "(Ljava/lang/String;)Ljava/lang/String;", |t, a| {
        let name = rt::java_string_to_rust(a[0] as Oop);
        if !boot_packages().contains(&name) {
            return ret_ref(std::ptr::null_mut());
        }
        match vm().image.module_of_package(&name) {
            Some(module) => ret_ref(rt::new_string(t, &format!("jrt:/{module}"))?),
            None => ret_ref(std::ptr::null_mut()),
        }
    });
    r.add("jdk/internal/loader/NativeLibraries", "findBuiltinLib", "(Ljava/lang/String;)Ljava/lang/String;", |_, a| {
        // The JDK's own libraries are built into the VM (rava implements their natives). Any
        // other library is not: loading it fails with UnsatisfiedLinkError, as on a JVM that
        // cannot load it, so libraries with a pure-Java fallback take it at load time.
        let name: String = rt::java_string_to_rust(a[0] as Oop);
        let base = name.strip_prefix("lib").and_then(|n| n.strip_suffix(".so").or_else(|| n.strip_suffix(".dylib"))).or_else(|| name.strip_suffix(".dll")).unwrap_or(&name);
        if JDK_LIBRARIES.contains(&base) {
            return ret_ref(a[0] as Oop);
        }
        ret_ref(std::ptr::null_mut())
    });
    r.add(
        "jdk/internal/loader/NativeLibraries",
        "load",
        "(Ljdk/internal/loader/NativeLibraries$NativeLibraryImpl;Ljava/lang/String;ZZ)Z",
        |t, a| {
            // The JDK's libraries: the VM supplies their natives itself, so the handle is an
            // opaque non-zero one, with the JNI version JNI_OnLoad would have reported (1.8),
            // which JDK 25 checks. Any other library is opened and its JNI_OnLoad run (`jni`).
            let lib = crate::jdk::library();
            let impl_class = "jdk/internal/loader/NativeLibraries$NativeLibraryImpl";
            let (handle, version) = if a[2] != 0 {
                (1, 0x0001_0008)
            } else {
                let path = rt::java_string_to_rust(a[1] as Oop);
                // The class that asked: its loader's native methods bind to this library.
                static FROM: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
                let from = lib.optional_field(&FROM, impl_class, "fromClass", "Ljava/lang/Class;").and_then(|off| {
                    // SAFETY: a live NativeLibraryImpl; the offset from its layout.
                    let mirror = unsafe { obj::read_ref(a[0] as Oop, off) };
                    (!mirror.is_null()).then(|| rt::klass_of_mirror(mirror)).flatten()
                });
                match crate::jni::load_for_java(t, &path, from, a[3] != 0)? {
                    Some((h, v)) => (h as i64, v),
                    None => return ret_bool(false),
                }
            };
            static HANDLE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            static VERSION: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            // SAFETY: a live NativeLibraryImpl; offsets from its layout.
            unsafe {
                if let Some(off) = lib.optional_field(&HANDLE, impl_class, "handle", "J") {
                    obj::write::<i64>(a[0] as Oop, off, handle);
                }
                if let Some(off) = lib.optional_field(&VERSION, impl_class, "jniVersion", "I") {
                    obj::write::<i32>(a[0] as Oop, off, version);
                }
            }
            ret_bool(true)
        },
    );
    r.add("jdk/internal/loader/NativeLibrary", "findEntry0", "(JLjava/lang/String;)J", |_, a| {
        // The VM's own libraries (handle 1) export nothing.
        let name = rt::java_string_to_rust(a[2] as Oop);
        ret_long(if a[0] > 1 { crate::jni::symbol(a[0] as usize, &name) as i64 } else { 0 })
    });
    r.add("jdk/internal/loader/NativeLibraries", "unload", "(Ljava/lang/String;ZJ)V", |_, _| ret_void());
    r.add(
        "java/lang/Class",
        "forName0",
        "(Ljava/lang/String;ZLjava/lang/ClassLoader;Ljava/lang/Class;)Ljava/lang/Class;",
        for_name0,
    );
}

/// `java.lang.Foo` / `[Ljava.lang.Foo;` → internal form.
fn internal_name(s: Oop) -> String {
    rt::java_string_to_rust(s).replace('.', "/")
}

fn define_class1(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (loader, name, bytes, off, len) = (a[0] as Oop, a[1] as Oop, a[2] as Oop, a[3] as i32, a[4] as i32);
    // SAFETY: byte[] argument.
    let data = unsafe { obj::array_slice::<u8>(bytes) };
    if off < 0 || len < 0 || off as usize + len as usize > data.len() {
        return Err(throw_new(t, "java/lang/ArrayIndexOutOfBoundsException", ""));
    }
    let owned = data[off as usize..(off + len) as usize].to_vec();
    let k = define_from_bytes(t, loader, owned, if name.is_null() { None } else { Some(internal_name(name)) })?;
    defined(t, k, a[5] as Oop)
}

vm_field!(protection_domain, "java/lang/Class", "protection_domain", "Ljava/security/ProtectionDomain;");

/// Records a class's protection domain (and in `Class.protectionDomain`, which JDK 24+ reads).
///
/// # Safety
/// `mirror` is a live `java.lang.Class`.
unsafe fn set_protection_domain(mirror: Oop, pd: Oop) {
    obj::write_ref(mirror, protection_domain(), pd);
    if let Some(off) = crate::runtime::mirror_protection_domain_offset() {
        obj::write_ref(mirror, off, pd);
    }
}

/// The mirror of `k`, just defined with protection domain `pd` (`Class.getProtectionDomain0`).
fn defined(t: &mut Thread, k: &'static Klass, pd: Oop) -> JResult<u64> {
    let mirror = rt::mirror_of(t, k)?;
    if !pd.is_null() {
        // SAFETY: a live mirror; the injected field holds a reference.
        unsafe { set_protection_domain(mirror, pd) };
    }
    ret_ref(mirror)
}

/// Parses and defines `bytes` for `loader`, with Java's errors for bad or duplicate classes.
pub(crate) fn define_from_bytes(t: &mut Thread, loader: Oop, bytes: Vec<u8>, expected: Option<String>) -> JResult<&'static Klass> {
    let cf = ClassFile::parse_owned(crate::patch::transform(bytes)).map_err(|e| throw_new(t, "java/lang/ClassFormatError", e.0))?;
    if let Some(n) = expected.filter(|n| cf.name != n.as_str()) {
        let msg = format!("{} (wrong name: {})", n, cf.name);
        return Err(throw_new(t, "java/lang/NoClassDefFoundError", &msg));
    }
    if vm().find_loaded(loader, cf.name).is_some_and(|k| std::ptr::eq(k.loader(), loader)) {
        let msg = format!("attempted duplicate class definition for {}", cf.name.as_str().replace('/', "."));
        return Err(throw_new(t, "java/lang/LinkageError", &msg));
    }
    vm().define_class(t, cf, loader).map_err(|e| rt::link_error(t, e))
}

vm_field!(class_data, "java/lang/Class", "classData", "Ljava/lang/Object;");
vm_field!(buffer_address, "java/nio/Buffer", "address", "J");

/// `Lookup.defineClass` / `defineHiddenClass` (flags: 1 nestmate, 2 hidden).
fn define_class0(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (loader, lookup, bytes, off, len) = (a[0] as Oop, a[1] as Oop, a[3] as Oop, a[4] as i32, a[5] as i32);
    let (initialize, flags, data) = (a[7] != 0, a[8] as i32, a[9] as Oop);
    // SAFETY: byte[] argument.
    let src = unsafe { obj::array_slice::<u8>(bytes) };
    if off < 0 || len < 0 || off as usize + len as usize > src.len() {
        return Err(throw_new(t, "java/lang/ArrayIndexOutOfBoundsException", ""));
    }
    let owned = src[off as usize..(off + len) as usize].to_vec();
    let k = if flags & 2 != 0 {
        let cf = ClassFile::parse_owned(owned).map_err(|e| throw_new(t, "java/lang/ClassFormatError", e.0))?;
        let host = if flags & 1 != 0 { rt::klass_of_mirror(lookup).map(|l| l.nest_host(t)) } else { None };
        vm().define_hidden_class(t, cf, loader, host).map_err(|e| rt::link_error(t, e))?
    } else {
        define_from_bytes(t, loader, owned, None)?
    };
    let mirror = rt::mirror_of(t, k)?;
    // SAFETY: live Class mirror.
    unsafe {
        obj::write_ref(mirror, class_data(), data);
        if !(a[6] as Oop).is_null() {
            set_protection_domain(mirror, a[6] as Oop);
        }
    }
    if initialize {
        rt::initialize(t, k)?;
    }
    ret_ref(mirror)
}

fn for_name0(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (name_obj, initialize, loader) = (a[0] as Oop, a[1] != 0, a[2] as Oop);
    if name_obj.is_null() {
        return Err(rt::null_pointer(t));
    }
    let dotted = rt::java_string_to_rust(name_obj);
    let not_found = |t: &mut Thread| throw_new(t, "java/lang/ClassNotFoundException", &dotted);
    if dotted.contains('/') || dotted.is_empty() {
        return Err(not_found(t));
    }
    let k = match vm().load_class(t, loader, Symbol::intern(&dotted.replace('.', "/"))) {
        Ok(k) => k,
        Err(LinkError::Load(_)) => return Err(not_found(t)),
        Err(LinkError::Thrown(ex)) => {
            // SAFETY: live exception.
            let ncdfe = crate::loader::boot_class!("java/lang/NoClassDefFoundError").expect("NCDFE");
            return Err(if unsafe { obj::klass(ex.0) }.is_subtype_of(ncdfe) { not_found(t) } else { ex });
        }
        Err(e) => return Err(rt::link_error(t, e)),
    };
    if initialize {
        rt::initialize(t, k)?;
    }
    ret_ref(rt::mirror_of(t, k)?)
}

fn retrieve_directives(t: &mut Thread, _: &[u64]) -> JResult<u64> {
    let k = crate::loader::boot_class!("java/lang/AssertionStatusDirectives").expect("AssertionStatusDirectives");
    rt::initialize(t, k)?;
    let assertions = &vm().options.assertions;
    let d = rt::alloc_instance(t, k)?;
    let [(classes, class_enabled), (packages, package_enabled)] = assertions.directives();
    let fields = [("classes", "classEnabled", classes, class_enabled), ("packages", "packageEnabled", packages, package_enabled)];
    for (names_field, enabled_field, names, enabled) in fields {
        let names = super::string_array(t, &names.into_iter().map(Some).collect::<Vec<_>>())?;
        let bools = rt::alloc_array(t, vm().primitive_array_class(crate::types::BasicType::Boolean).expect("[Z"), enabled.len() as i32)?;
        for (i, &e) in enabled.iter().enumerate() {
            // SAFETY: element `i` of the fresh boolean array.
            unsafe { *(bools as *mut u8).add(crate::heap::ARRAY_BASE_OFFSET as usize + i) = e as u8 };
        }
        let field = |name: &str, desc: &str| k.declared_field(Symbol::intern(name), Symbol::intern(desc)).expect("directive field").offset;
        // SAFETY: fields of the fresh instance of the declaring class.
        unsafe {
            obj::write_ref(d, field(names_field, "[Ljava/lang/String;"), names);
            obj::write_ref(d, field(enabled_field, "[Z"), bools);
        }
    }
    let deflt = k.declared_field(Symbol::intern("deflt"), Symbol::intern("Z")).expect("deflt").offset;
    // SAFETY: as above.
    unsafe { obj::write::<u8>(d, deflt, assertions.user_default() as u8) };
    ret_ref(d)
}
