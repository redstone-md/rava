//! Class-loader natives: defining classes from bytes, boot lookups, `Class.forName0`,
//! and native-library bookkeeping (all VM natives are built in).

use super::{ret_bool, ret_ref, ret_void, NativeRegistry};
use crate::classfile::ClassFile;
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::link::LinkError;
use crate::runtime::{self as rt, throw_new, vm_field};
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use crate::vm::vm;

const CL: &str = "java/lang/ClassLoader";

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
            let owned: &'static [u8] = Box::leak(bytes.to_vec().into_boxed_slice());
            let k = define_from_bytes(t, loader, owned, if name.is_null() { None } else { Some(internal_name(name)) })?;
            ret_ref(rt::mirror_of(t, k)?)
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
    r.add("jdk/internal/loader/BootLoader", "getSystemPackageNames", "()[Ljava/lang/String;", |t, _| {
        ret_ref(super::string_array(t, &[])?)
    });
    r.add("jdk/internal/loader/BootLoader", "getSystemPackageLocation", "(Ljava/lang/String;)Ljava/lang/String;", |_, _| {
        ret_ref(std::ptr::null_mut())
    });
    r.add("jdk/internal/loader/NativeLibraries", "findBuiltinLib", "(Ljava/lang/String;)Ljava/lang/String;", |_, a| {
        // Every JDK library's natives are built into the VM.
        ret_ref(a[0] as Oop)
    });
    r.add(
        "jdk/internal/loader/NativeLibraries",
        "load",
        "(Ljdk/internal/loader/NativeLibraries$NativeLibraryImpl;Ljava/lang/String;ZZ)Z",
        |_, _| ret_bool(true),
    );
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
    let owned: &'static [u8] = Box::leak(data[off as usize..(off + len) as usize].to_vec().into_boxed_slice());
    let k = define_from_bytes(t, loader, owned, if name.is_null() { None } else { Some(internal_name(name)) })?;
    ret_ref(rt::mirror_of(t, k)?)
}

/// Parses and defines `bytes` for `loader`, with Java's errors for bad or duplicate classes.
pub(crate) fn define_from_bytes(t: &mut Thread, loader: Oop, bytes: &'static [u8], expected: Option<String>) -> JResult<&'static Klass> {
    let cf = ClassFile::parse(bytes).map_err(|e| throw_new(t, "java/lang/ClassFormatError", e.0))?;
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
    let owned: &'static [u8] = Box::leak(src[off as usize..(off + len) as usize].to_vec().into_boxed_slice());
    let k = if flags & 2 != 0 {
        let cf = ClassFile::parse(owned).map_err(|e| throw_new(t, "java/lang/ClassFormatError", e.0))?;
        let host = if flags & 1 != 0 { rt::klass_of_mirror(lookup).map(|l| l.nest_host(t)) } else { None };
        vm().define_hidden_class(t, cf, loader, host).map_err(|e| rt::link_error(t, e))?
    } else {
        define_from_bytes(t, loader, owned, None)?
    };
    let mirror = rt::mirror_of(t, k)?;
    // SAFETY: live Class mirror.
    unsafe { obj::write_ref(mirror, class_data(), data) };
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
            let ncdfe = crate::loader::load("java/lang/NoClassDefFoundError").expect("NCDFE");
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
    let k = crate::loader::load("java/lang/AssertionStatusDirectives").expect("AssertionStatusDirectives");
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
