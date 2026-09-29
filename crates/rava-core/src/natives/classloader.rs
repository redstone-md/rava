//! Class-loader natives: defining classes from bytes, boot lookups, `Class.forName0`,
//! and native-library bookkeeping (all VM natives are built in).

use super::{ret_bool, ret_ref, ret_void, NativeRegistry};
use crate::classfile::ClassFile;
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::link::LinkError;
use crate::runtime::{self as rt, throw_new};
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
    let d = rt::alloc_instance(t, k)?;
    let strings = super::string_array(t, &[])?;
    let bools = rt::alloc_array(t, vm().primitive_array_class(crate::types::BasicType::Boolean).expect("[Z"), 0)?;
    for (name, desc, v) in [
        ("classes", "[Ljava/lang/String;", strings),
        ("packages", "[Ljava/lang/String;", strings),
        ("classEnabled", "[Z", bools),
        ("packageEnabled", "[Z", bools),
    ] {
        let f = k.declared_field(Symbol::intern(name), Symbol::intern(desc)).expect("directive field");
        // SAFETY: fresh instance of the declaring class.
        unsafe { obj::write::<Oop>(d, f.offset, v) };
    }
    ret_ref(d)
}
