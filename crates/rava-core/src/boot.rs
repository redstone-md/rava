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
    create_initial_thread(t, main_group)?;
    set_unsafe_constants(t)?;
    for c in ["java/lang/Module", "java/lang/reflect/Method", "java/lang/ref/Finalizer"] {
        rt::initialize(t, class(c))?;
    }
    call_static(t, "java/lang/System", "initPhase1", "()V", &[])?;
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

/// Resolves the application main class through the JDK launcher and runs `main(args)`.
pub fn run_main(t: &mut Thread, main_class: &str, args: &[String]) -> JResult<()> {
    run_launched(t, main_class, false, args)
}

/// Runs `main` of `target`: a class name, or with `jar` a jar file whose manifest names the
/// main class (`java -jar`), as the JDK launcher does.
pub fn run_launched(t: &mut Thread, target: &str, jar: bool, args: &[String]) -> JResult<()> {
    let name = rt::new_string(t, target)?;
    // LauncherHelper.checkAndLoadMain(printToStderr, mode = LM_CLASS (1) or LM_JAR (2), name)
    let mirror = call_static(
        t,
        "sun/launcher/LauncherHelper",
        "checkAndLoadMain",
        "(ZILjava/lang/String;)Ljava/lang/Class;",
        &[1, if jar { 2 } else { 1 }, name as u64],
    )? as Oop;
    let k = rt::klass_of_mirror(mirror).expect("main class mirror");
    rt::initialize(t, k)?;
    let main = k
        .declared_method(Symbol::intern("main"), Symbol::intern("([Ljava/lang/String;)V"))
        .filter(|m| m.is_static())
        .unwrap_or_else(|| panic!("no static main(String[]) in {target}"));
    let values: Vec<Option<String>> = args.iter().cloned().map(Some).collect();
    let array = crate::natives::string_array(t, &values)?;
    t.call(main, &[array as u64])?;
    Ok(())
}

