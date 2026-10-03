//! Module-system natives. The VM records which module defines each (loader, package) so every
//! class mirror can carry its `Class.module`, as HotSpot's module entries do. Mirrors created
//! before `java.base` is defined are patched when it is.

use super::{ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::runtime::{self as rt, vm_field};
use crate::thread::{JResult, Thread};
use crate::vm::vm;
use std::collections::HashMap;
use std::sync::Mutex;

vm_field!(class_module, "java/lang/Class", "module", "Ljava/lang/Module;");
vm_field!(loader_unnamed, "java/lang/ClassLoader", "unnamedModule", "Ljava/lang/Module;");

#[derive(Default)]
struct Registry {
    /// (loader address, package in internal form) → Module.
    packages: HashMap<(usize, String), usize>,
    boot_unnamed: usize,
    java_base: usize,
}

fn registry() -> &'static Mutex<Registry> {
    static R: std::sync::OnceLock<Mutex<Registry>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

const M: &str = "java/lang/Module";

pub(super) fn register(r: &mut NativeRegistry) {
    r.add("jdk/internal/loader/BootLoader", "setBootLoaderUnnamedModule0", "(Ljava/lang/Module;)V", |_, a| {
        registry().lock().unwrap().boot_unnamed = a[0] as usize;
        ret_void()
    });
    r.add(M, "defineModule0", "(Ljava/lang/Module;ZLjava/lang/String;Ljava/lang/String;[Ljava/lang/Object;)V", define_module);
    r.add(M, "addReads0", "(Ljava/lang/Module;Ljava/lang/Module;)V", |_, _| ret_void());
    r.add(M, "addExports0", "(Ljava/lang/Module;Ljava/lang/String;Ljava/lang/Module;)V", |_, _| ret_void());
    r.add(M, "addExportsToAll0", "(Ljava/lang/Module;Ljava/lang/String;)V", |_, _| ret_void());
    r.add(M, "addExportsToAllUnnamed0", "(Ljava/lang/Module;Ljava/lang/String;)V", |_, _| ret_void());
}

vm_field!(module_loader, "java/lang/Module", "loader", "Ljava/lang/ClassLoader;");

fn define_module(t: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (module, packages) = (a[0] as Oop, a[4] as Oop);
    // SAFETY: live Module; `pns` is an Object[] of package-name Strings.
    let loader = unsafe { obj::read_ref(module, module_loader()) } as usize;
    let names: Vec<String> = if packages.is_null() {
        Vec::new()
    } else {
        unsafe { obj::ref_elems(packages) }.iter().map(|&s| rt::java_string_to_rust(s).replace('.', "/")).collect()
    };
    let is_java_base = names.iter().any(|p| p == "java/lang") && loader == 0;
    {
        let mut reg = registry().lock().unwrap();
        for p in names {
            reg.packages.insert((loader, p), module as usize);
        }
        if is_java_base {
            reg.java_base = module as usize;
        }
    }
    if is_java_base {
        patch_existing_mirrors(t);
    }
    ret_void()
}

/// The module of a class: its package's module for its loader, else the loader's unnamed module.
pub(crate) fn module_of(k: &'static Klass) -> Oop {
    let elem = bottom_element(k);
    let loader = elem.loader() as usize;
    let reg = registry().lock().unwrap();
    if matches!(elem.kind, crate::klass::KlassKind::TypeArray { .. }) {
        return reg.java_base as Oop;
    }
    if let Some(&m) = reg.packages.get(&(loader, elem.package().to_string())) {
        return m as Oop;
    }
    if loader == 0 {
        return reg.boot_unnamed as Oop;
    }
    drop(reg);
    // SAFETY: live ClassLoader.
    unsafe { obj::read_ref(loader as Oop, loader_unnamed()) }
}

fn bottom_element(mut k: &'static Klass) -> &'static Klass {
    while let crate::klass::KlassKind::ObjArray { elem } = k.kind {
        k = elem;
    }
    k
}

/// Sets `Class.module` on a freshly created mirror.
pub(crate) fn set_mirror_module(mirror: Oop, k: &'static Klass) {
    let m = module_of(k);
    if !m.is_null() {
        // SAFETY: live Class mirror.
        unsafe { obj::write_ref(mirror, class_module(), m) };
    }
}

fn patch_existing_mirrors(_t: &mut Thread) {
    let classes: Vec<&'static Klass> = vm().classes.read().unwrap().values().copied().collect();
    for k in classes {
        let mirror = k.mirror();
        if !mirror.is_null() {
            set_mirror_module(mirror, k);
        }
    }
    for m in rt::primitive_mirrors() {
        set_primitive_mirror_module(m);
    }
}

/// Modules recorded by the registry (collector roots).
pub(crate) fn roots(f: &mut dyn FnMut(Oop)) {
    let reg = registry().lock().unwrap();
    reg.packages.values().for_each(|&m| f(m as Oop));
    f(reg.boot_unnamed as Oop);
    f(reg.java_base as Oop);
}

/// Primitive mirrors belong to java.base (patched once it is defined).
pub(crate) fn set_primitive_mirror_module(mirror: Oop) {
    let base = registry().lock().unwrap().java_base as Oop;
    if !base.is_null() {
        // SAFETY: live Class mirror.
        unsafe { obj::write_ref(mirror, class_module(), base) };
    }
}
