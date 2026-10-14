//! VM startup, in HotSpot's `create_vm` order: core classes, the initial thread group and
//! thread, then `System.initPhase1/2/3`, then the launcher.

use crate::heap::Oop;
use crate::klass::Klass;
use crate::natives::thread as jthread;
use crate::runtime as rt;
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use std::sync::atomic::Ordering;

fn class(name: &str) -> &'static Klass {
    crate::loader::load(name).unwrap_or_else(|e| panic!("boot class {name}: {e:?}"))
}

fn method(k: &'static Klass, name: &str, desc: &str) -> &'static crate::method::Method {
    k.declared_method(Symbol::intern(name), Symbol::intern(desc))
        .unwrap_or_else(|| panic!("boot method {}.{name}{desc}", k.name))
}

/// Calls a static method of a boot class.
/// The runtime is rava (its VM runs the class library), so the properties naming the runtime
/// and its vendor say so instead of what the class library's build wrote into `VersionProps`;
/// the Java version and the library's build string stay as they are.
const BRANDING: [(&str, &str); 4] = [
    ("java.runtime.name", "Rava Runtime Environment"),
    ("java.vendor", "rava"),
    ("java.vendor.url", "https://github.com/redstone-md/rava"),
    ("java.vendor.url.bug", "https://github.com/redstone-md/rava/issues"),
];

fn brand(t: &mut Thread) -> JResult<()> {
    for (key, value) in BRANDING {
        let (k, v) = (rt::new_string(t, key)?, rt::new_string(t, value)?);
        call_static(t, "java/lang/System", "setProperty", "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;", &[k as u64, v as u64])?;
    }
    Ok(())
}

/// The system property `key` (empty when unset).
pub fn property(t: &mut Thread, key: &str) -> JResult<String> {
    let k = rt::new_string(t, key)?;
    let v = call_static(t, "java/lang/System", "getProperty", "(Ljava/lang/String;)Ljava/lang/String;", &[k as u64])? as Oop;
    Ok(if v.is_null() { String::new() } else { rt::java_string_to_rust(v) })
}

pub fn call_static(t: &mut Thread, class_name: &str, name: &str, desc: &str, args: &[u64]) -> JResult<u64> {
    let k = class(class_name);
    rt::initialize(t, k)?;
    t.call(method(k, name, desc), args)
}

/// `new C(args)` for a boot class.
pub fn construct(t: &mut Thread, class_name: &str, desc: &str, args: &[u64]) -> JResult<Oop> {
    let k = class(class_name);
    rt::initialize(t, k)?;
    let o = rt::alloc_instance(t, k)?;
    let mut all = vec![o as u64];
    all.extend_from_slice(args);
    t.call(method(k, "<init>", desc), &all)?;
    Ok(o)
}

static MAIN_GROUP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The `main` thread group, once the VM has made it.
pub fn main_thread_group() -> Option<Oop> {
    let g = MAIN_GROUP.load(std::sync::atomic::Ordering::Acquire);
    (g != 0).then_some(g as Oop)
}

/// Brings the JDK up to the point where application classes can be loaded and run.
pub fn boot(t: &mut Thread) -> JResult<()> {
    for c in ["java/lang/Object", "java/lang/String", "java/lang/System", "java/lang/Class", "java/lang/ThreadGroup"] {
        rt::initialize(t, class(c))?;
    }
    let system_group = construct(t, "java/lang/ThreadGroup", "()V", &[])?;
    let main_name = rt::new_string(t, "main")?;
    let main_group = construct(
        t,
        "java/lang/ThreadGroup",
        "(Ljava/lang/ThreadGroup;Ljava/lang/String;)V",
        &[system_group as u64, main_name as u64],
    )?;
    // Kept for threads native code attaches later, which join it as HotSpot's do.
    MAIN_GROUP.store(crate::jni::new_global(main_group) as usize, std::sync::atomic::Ordering::Release);
    create_initial_thread(t, main_group)?;
    set_unsafe_constants(t)?;
    for c in ["java/lang/Module", "java/lang/reflect/Method", "java/lang/ref/Finalizer"] {
        rt::initialize(t, class(c))?;
    }
    call_static(t, "java/lang/System", "initPhase1", "()V", &[])?;
    brand(t)?;
    // The errors the VM itself throws are initialized now, as HotSpot does after phase 1: one
    // first initialized where it is thrown (at a stack's end, for StackOverflowError) could fail
    // there and be erroneous for good, each later throw of it becoming another error to build.
    for c in [
        "java/lang/OutOfMemoryError",
        "java/lang/NullPointerException",
        "java/lang/ClassCastException",
        "java/lang/ArrayStoreException",
        "java/lang/ArithmeticException",
        "java/lang/StackOverflowError",
        "java/lang/IllegalMonitorStateException",
        "java/lang/IllegalArgumentException",
        "java/lang/NoClassDefFoundError",
        "java/lang/ExceptionInInitializerError",
    ] {
        rt::initialize(t, class(c))?;
    }
    // java.lang.invoke's core, as HotSpot's initialize_jsr292_core_classes before phase 2:
    // MethodHandleNatives marks java.lang.invoke ready, which core reflection's field accessors
    // require (else a program's first Field.get before any lambda throws InternalError).
    for c in ["java/lang/invoke/MethodHandle", "java/lang/invoke/ResolvedMethodName", "java/lang/invoke/MemberName", "java/lang/invoke/MethodHandleNatives"] {
        rt::initialize(t, class(c))?;
    }
    let status = call_static(t, "java/lang/System", "initPhase2", "(ZZ)I", &[1, 1])? as i32;
    if status != 0 {
        panic!("System.initPhase2 failed with status {status}");
    }
    call_static(t, "java/lang/System", "initPhase3", "()V", &[])?;
    Ok(())
}

fn create_initial_thread(t: &mut Thread, group: Oop) -> JResult<()> {
    let thread_class = class("java/lang/Thread");
    rt::initialize(t, thread_class)?;
    let jt = rt::alloc_instance(t, thread_class)?;
    t.java_thread = jt;
    jthread::mark_started(t, jt);
    let name = rt::new_string(t, "main")?;
    t.call(
        method(thread_class, "<init>", "(Ljava/lang/ThreadGroup;Ljava/lang/String;)V"),
        &[jt as u64, group as u64, name as u64],
    )?;
    jthread::mark_started(t, jt);
    Ok(())
}

/// `jdk.internal.misc.UnsafeConstants` holds values the VM injects after its trivial `<clinit>`.
fn set_unsafe_constants(t: &mut Thread) -> JResult<()> {
    let k = class("jdk/internal/misc/UnsafeConstants");
    rt::initialize(t, k)?;
    let values: [(&str, u64); 5] = [
        ("ADDRESS_SIZE0", 8),
        ("PAGE_SIZE", 4096),
        ("BIG_ENDIAN", cfg!(target_endian = "big") as u64),
        ("UNALIGNED_ACCESS", 1),
        ("DATA_CACHE_LINE_FLUSH_SIZE", 0),
    ];
    for (name, v) in values {
        let f = k.fields.iter().find(|f| f.name == name && f.is_static()).expect("UnsafeConstants field");
        k.statics[f.offset as usize].store(v, Ordering::Relaxed);
    }
    Ok(())
}

/// Ends the VM as the launcher's `DestroyJavaVM` does: waits for the non-daemon threads, then
/// runs the shutdown sequence (the shutdown hooks).
pub fn destroy(t: &mut Thread) -> JResult<()> {
    // The main thread ends as the launcher's `DetachCurrentThread` ends it.
    let main = t.java_thread;
    if !main.is_null() {
        crate::natives::thread::terminate(t, main);
    }
    crate::natives::thread::await_non_daemon(t);
    call_static(t, "java/lang/Shutdown", "shutdown", "()V", &[])?;
    Ok(())
}
