//! VM-state natives: `jdk.internal.misc.VM`, signals, references, runtime and shutdown.

use super::{ret_bool, ret_int, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::heap::Oop;
use crate::runtime::{self as rt};
use crate::symbol::Symbol;
use crate::thread::{JResult, Thread};
use crate::vm::vm;
use std::sync::atomic::Ordering;

const VM: &str = "jdk/internal/misc/VM";

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(VM, "initialize", "()V", |_, _| ret_void());
    r.add(VM, "latestUserDefinedLoader0", "()Ljava/lang/ClassLoader;", latest_user_defined_loader);
    r.add(VM, "getuid", "()J", |_, _| ret_long(unsafe { libc::getuid() } as i64));
    r.add(VM, "geteuid", "()J", |_, _| ret_long(unsafe { libc::geteuid() } as i64));
    r.add(VM, "getgid", "()J", |_, _| ret_long(unsafe { libc::getgid() } as i64));
    r.add(VM, "getegid", "()J", |_, _| ret_long(unsafe { libc::getegid() } as i64));
    r.add(VM, "getNanoTimeAdjustment", "(J)J", |_, a| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let offset = a[0] as i64;
        let secs = now.as_secs() as i64 - offset;
        if !(-(1i64 << 32)..(1i64 << 32)).contains(&secs) {
            return ret_long(-1);
        }
        ret_long(secs * 1_000_000_000 + now.subsec_nanos() as i64)
    });
    r.add(VM, "getRuntimeArguments", "()[Ljava/lang/String;", |t, _| ret_ref(super::string_array(t, &[])?));
    r.add("jdk/internal/misc/Signal", "findSignal0", "(Ljava/lang/String;)I", |_, a| {
        let n = match rt::java_string_to_rust(a[0] as Oop).as_str() {
            "HUP" => 1,
            "INT" => 2,
            "QUIT" => 3,
            "TERM" => 15,
            _ => -1,
        };
        ret_int(n)
    });
    r.add("jdk/internal/misc/Signal", "handle0", "(IJ)J", |_, _| ret_long(0));
    r.add("java/lang/ref/Reference", "getAndClearReferencePendingList", "()Ljava/lang/ref/Reference;", |_, _| {
        ret_ref(std::ptr::null_mut())
    });
    r.add("java/lang/ref/Reference", "hasReferencePendingList", "()Z", |_, _| ret_bool(false));
    r.add("java/lang/ref/Reference", "waitForReferencePendingList", "()V", |_, _| loop {
        // The collector does not discover references yet; the handler thread sleeps.
        std::thread::park();
    });
    r.add("java/lang/ref/Reference", "refersTo0", "(Ljava/lang/Object;)Z", refers_to);
    r.add("java/lang/ref/PhantomReference", "refersTo0", "(Ljava/lang/Object;)Z", refers_to);
    r.add("java/lang/ref/Reference", "clear0", "()V", |_, a| {
        // SAFETY: live Reference.
        unsafe { crate::heap::obj::write::<Oop>(a[0] as Oop, referent_offset(), std::ptr::null_mut()) };
        ret_void()
    });
    r.add("java/lang/ref/Finalizer", "isFinalizationEnabled", "()Z", |_, _| ret_bool(false));
    r.add("java/lang/Runtime", "maxMemory", "()J", |_, _| ret_long(vm().options.heap_reserve as i64));
    r.add("java/lang/Runtime", "totalMemory", "()J", |_, _| ret_long(vm().heap.used_bytes().max(64 << 20) as i64));
    r.add("java/lang/Runtime", "freeMemory", "()J", |_, _| ret_long(32 << 20));
    r.add("java/lang/Runtime", "gc", "()V", |_, _| ret_void());
    r.add("java/lang/Shutdown", "beforeHalt", "()V", |_, _| ret_void());
    r.add("java/lang/Shutdown", "halt0", "(I)V", |_, a| std::process::exit(a[0] as i32));
    r.add("java/lang/System", "setIn0", "(Ljava/io/InputStream;)V", |_, a| set_system_stream("in", "Ljava/io/InputStream;", a[0]));
    r.add("java/lang/System", "setOut0", "(Ljava/io/PrintStream;)V", |_, a| set_system_stream("out", "Ljava/io/PrintStream;", a[0]));
    r.add("java/lang/System", "setErr0", "(Ljava/io/PrintStream;)V", |_, a| set_system_stream("err", "Ljava/io/PrintStream;", a[0]));
    r.add("java/lang/System", "mapLibraryName", "(Ljava/lang/String;)Ljava/lang/String;", |t, a| {
        let name = rt::java_string_to_rust(a[0] as Oop);
        ret_ref(rt::new_string(t, &format!("lib{name}.so"))?)
    });
    r.add("java/security/AccessController", "getStackAccessControlContext", "()Ljava/security/AccessControlContext;", |_, _| {
        ret_ref(std::ptr::null_mut())
    });
    r.add("java/security/AccessController", "getInheritedAccessControlContext", "()Ljava/security/AccessControlContext;", |_, _| {
        ret_ref(std::ptr::null_mut())
    });
    r.add("java/security/AccessController", "ensureMaterializedForStackWalk", "(Ljava/lang/Object;)V", |_, _| ret_void());
    r.add("jdk/internal/misc/ScopedMemoryAccess", "registerNatives", "()V", |_, _| ret_void());
}

crate::runtime::vm_field!(referent_offset, "java/lang/ref/Reference", "referent", "Ljava/lang/Object;");

fn refers_to(_: &mut Thread, a: &[u64]) -> JResult<u64> {
    // SAFETY: live Reference.
    let referent = unsafe { crate::heap::obj::read::<u64>(a[0] as Oop, referent_offset()) };
    ret_bool(referent == a[1])
}

fn set_system_stream(name: &str, desc: &str, value: u64) -> JResult<u64> {
    let system = crate::loader::load("java/lang/System").expect("System");
    let f = system.declared_field(Symbol::intern(name), Symbol::intern(desc)).expect("System stream field");
    system.statics[f.offset as usize].store(value, Ordering::Relaxed);
    ret_void()
}

/// The defining loader of the innermost frame whose class has a non-boot loader.
fn latest_user_defined_loader(t: &mut Thread, _: &[u64]) -> JResult<u64> {
    let loader = (0..t.frame_count())
        .filter_map(|n| t.caller_frame(n))
        .map(|(m, _)| m.holder.loader())
        .find(|l| !l.is_null())
        .unwrap_or(std::ptr::null_mut());
    ret_ref(loader)
}
