//! `jdk.jfr.internal.JVM`: Flight Recorder's VM interface. Event classes register, carry their
//! configuration and get type ids, so the `jdk.jfr` API works; recording itself is not
//! supported: the native recorder cannot be created, so starting a recording fails with the
//! JDK's own "Unable to start Jfr", and events stay uninstrumented (committing does nothing).
//!
//! Event classes register themselves as they initialize, unless annotated `@Registered(false)`:
//! HotSpot inserts that call at the start of their static initializer when it loads them.
//!
//! JDK 21 declares most of these as instance methods and later JDKs as static ones; `args`
//! drops the receiver so each native reads the same arguments either way.

use super::{ret_bool, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::heap::{obj, Oop};
use crate::klass::Klass;
use crate::runtime::{self as rt, vm_field};
use crate::symbol::Symbol;
use crate::thread::Thread;
use std::collections::HashMap;
use std::sync::Mutex;

const J: &str = "jdk/jfr/internal/JVM";
vm_field!(tid_offset, "java/lang/Thread", "tid", "J");

/// Per-class state the VM keeps for Flight Recorder.
#[derive(Default)]
struct Registry {
    /// Type ids of classes (by `Klass` address) and of types known only by name.
    ids: HashMap<usize, i64>,
    named: HashMap<String, i64>,
    next: i64,
    /// Each event class's `EventConfiguration`: collector roots.
    configurations: HashMap<usize, usize>,
}

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    static R: std::sync::LazyLock<Mutex<Registry>> = std::sync::LazyLock::new(|| Mutex::new(Registry { next: FIRST_ID, ..Default::default() }));
    R.lock().unwrap()
}

/// Type ids above those of the recorder's own metadata types.
const FIRST_ID: i64 = 1 << 20;

impl Registry {
    fn fresh(&mut self) -> i64 {
        self.next += 1;
        self.next
    }

    fn class_id(&mut self, k: &'static Klass) -> i64 {
        let key = k as *const Klass as usize;
        if let Some(&id) = self.ids.get(&key) {
            return id;
        }
        let id = self.fresh();
        self.ids.insert(key, id);
        id
    }
}

pub(crate) fn roots(precise: &mut dyn FnMut(Oop)) {
    registry().configurations.values().for_each(|&o| precise(o as Oop));
}

/// The arguments without the `JVM` receiver of an instance native.
fn args(a: &[u64]) -> &[u64] {
    // SAFETY: a non-null first argument is a live object.
    let receiver = a.first().is_some_and(|&o| o != 0 && unsafe { obj::klass(o as Oop) }.name == J);
    if receiver { &a[1..] } else { a }
}

fn class_arg(a: &[u64]) -> Option<&'static Klass> {
    rt::klass_of_mirror(args(a)[0] as Oop)
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add(J, "registerNatives", "()V", |_, _| ret_void());
    r.add(J, "isAvailable", "()Z", |_, _| ret_bool(true));
    r.add(J, "createJFR", "(Z)Z", |_, _| ret_bool(false));
    r.add(J, "destroyJFR", "()Z", |_, _| ret_bool(true));
    r.add(J, "isRecording", "()Z", |_, _| ret_bool(false));
    r.add(J, "counterTime", "()J", |_, _| ret_long(now_nanos()));
    r.add(J, "getTicksFrequency", "()J", |_, _| ret_long(1_000_000_000));
    r.add(J, "getTimeConversionFactor", "()D", |_, _| Ok(1.0f64.to_bits()));
    r.add(J, "getChunkStartNanos", "()J", |_, _| ret_long(0));
    r.add(J, "getPid", "()Ljava/lang/String;", |t, _| ret_ref(rt::new_string(t, &std::process::id().to_string())?));
    r.add(J, "getThreadId", "(Ljava/lang/Thread;)J", |_, a| {
        let jt = args(a)[0] as Oop;
        // SAFETY: a live Thread (or null).
        ret_long(if jt.is_null() { 0 } else { unsafe { obj::read::<i64>(jt, tid_offset()) } })
    });
    r.add(J, "getStackTraceId", "(I)J", |_, _| ret_long(0));
    r.add(J, "getTypeId", "(Ljava/lang/Class;)J", |_, a| ret_long(class_arg(a).map_or(0, |k| registry().class_id(k))));
    r.add(J, "getClassId", "(Ljava/lang/Class;)J", |_, a| ret_long(class_arg(a).map_or(0, |k| registry().class_id(k))));
    r.add(J, "getTypeId", "(Ljava/lang/String;)J", |t, a| ret_long(named_type_id(t, args(a)[0] as Oop)));
    r.add(J, "getConfiguration", "(Ljava/lang/Class;)Ljava/lang/Object;", |_, a| {
        let key = class_arg(a).map_or(0, |k| k as *const Klass as usize);
        ret_ref(registry().configurations.get(&key).copied().unwrap_or(0) as Oop)
    });
    r.add(J, "setConfiguration", "(Ljava/lang/Class;Ljdk/jfr/internal/event/EventConfiguration;)Z", |_, a| {
        let Some(k) = class_arg(a) else { return ret_bool(false) };
        registry().configurations.insert(k as *const Klass as usize, args(a)[1] as usize);
        ret_bool(true)
    });
    r.add(J, "isExcluded", "(Ljava/lang/Class;)Z", |_, _| ret_bool(false));
    r.add(J, "isExcluded", "(Ljava/lang/Thread;)Z", |_, _| ret_bool(false));
    r.add(J, "isInstrumented", "(Ljava/lang/Class;)Z", |_, _| ret_bool(false));
    r.add(J, "getAllowedToDoEventRetransforms", "()Z", |_, _| ret_bool(false));
    r.add(J, "getUnloadedEventClassCount", "()J", |_, _| ret_long(0));
    r.add(J, "getAllEventClasses", "()Ljava/util/List;", |t, _| {
        ret_ref(crate::boot::construct(t, "java/util/ArrayList", "()V", &[])?)
    });
    r.add(J, "isContainerized", "()Z", |_, _| ret_bool(false));
    r.add(J, "hostTotalMemory", "()J", |_, _| {
        ret_long(crate::os::memory_status().0)
    });
    // Settings only matter to a recording.
    for (name, desc) in [("setEnabled", "(JZ)V"), ("setStackTraceEnabled", "(JZ)V"), ("setMethodSamplingPeriod", "(JJ)V"), ("setFileNotification", "(J)V"),
        ("setGlobalBufferCount", "(J)V"), ("setGlobalBufferSize", "(J)V"), ("setMemorySize", "(J)V"), ("setThreadBufferSize", "(J)V"),
        ("setForceInstrumentation", "(Z)V"), ("setCompressedIntegers", "(Z)V"), ("setStackDepth", "(I)V"), ("storeMetadataDescriptor", "([B)V"),
        ("setRepositoryLocation", "(Ljava/lang/String;)V"), ("setDumpPath", "(Ljava/lang/String;)V"), ("setOutput", "(Ljava/lang/String;)V"),
        ("subscribeLogLevel", "(Ljdk/jfr/internal/LogTag;I)V"), ("log", "(IILjava/lang/String;)V"), ("logEvent", "(I[Ljava/lang/String;Z)V"),
        ("exclude", "(Ljava/lang/Thread;)V"), ("include", "(Ljava/lang/Thread;)V"), ("markChunkFinal", "()V"), ("flush", "()V"),
        ("uncaughtException", "(Ljava/lang/Thread;Ljava/lang/Throwable;)V"), ("emitDataLoss", "(J)V")] {
        r.add(J, name, desc, |_, _| ret_void());
    }
    for (name, desc) in [("setThreshold", "(JJ)Z"), ("setCutoff", "(JJ)Z"), ("setThrottle", "(JJJ)Z")] {
        r.add(J, name, desc, |_, _| ret_bool(true));
    }
    r.add(J, "emitEvent", "(JJJ)Z", |_, _| ret_bool(false));
    r.add(J, "shouldRotateDisk", "()Z", |_, _| ret_bool(false));
    r.add(J, "getDumpPath", "()Ljava/lang/String;", |_, _| ret_ref(std::ptr::null_mut()));
    r.add(J, "addStringConstant", "(JLjava/lang/String;)Z", |_, _| ret_bool(false));
    r.add(J, "retransformClasses", "([Ljava/lang/Class;)V", |_, _| ret_void());
    r.add(J, "abort", "(Ljava/lang/String;)V", |_, a| {
        eprintln!("Flight Recorder aborted: {}", rt::java_string_to_rust(args(a)[0] as Oop));
        std::process::exit(1)
    });
}

/// Before the static initializer of a concrete `jdk.jfr.Event` subclass runs: registers it with
/// `FlightRecorder.register` unless the nearest `@Registered` in its hierarchy says otherwise.
pub(crate) fn register_on_init(t: &mut Thread, k: &'static Klass) -> crate::thread::JResult<()> {
    if k.access & 0x0400 != 0 || k.is_interface() || !is_event_subclass(k) || !registered(k) {
        return Ok(());
    }
    let mirror = rt::mirror_of(t, k)?;
    crate::boot::call_static(t, "jdk/jfr/FlightRecorder", "register", "(Ljava/lang/Class;)V", &[mirror as u64])?;
    Ok(())
}

fn is_event_subclass(k: &'static Klass) -> bool {
    std::iter::successors(k.super_klass, |s| s.super_klass).any(|s| s.name == "jdk/jfr/Event")
}

/// The value of the nearest `@jdk.jfr.Registered` on `k` or a superclass (it is `@Inherited`).
fn registered(k: &'static Klass) -> bool {
    for c in std::iter::successors(Some(k), |c| c.super_klass) {
        let Some(cf) = c.cf.as_ref() else { continue };
        let Some(attr) = cf.attr("RuntimeVisibleAnnotations") else { continue };
        let body = cf.attr_bytes(attr);
        let is_registered = |i: u16| cf.utf8(i).is_ok_and(|s| s == "Ljdk/jfr/Registered;");
        if crate::classfile::annotation_types(body).into_iter().any(is_registered) {
            let value = crate::classfile::annotation_constants(body).into_iter().find(|&(ty, name, _, _)| is_registered(ty) && cf.utf8(name).is_ok_and(|s| s == "value"));
            return value.map_or(true, |(_, _, _, v)| !matches!(cf.pool.get(v as usize), Some(crate::classfile::Cp::Int(0))));
        }
    }
    true
}

/// Monotonic nanoseconds (from the first call).
fn now_nanos() -> i64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_nanos() as i64
}

/// The type id of a type named in binary form: a boot class's own id, else one for the name.
fn named_type_id(_: &mut Thread, name: Oop) -> i64 {
    let name = rt::java_string_to_rust(name);
    if let Ok(k) = crate::vm::vm().load_boot_class(Symbol::intern(&name.replace('.', "/"))) {
        return registry().class_id(k);
    }
    let mut r = registry();
    if let Some(&id) = r.named.get(&name) {
        return id;
    }
    let id = r.fresh();
    r.named.insert(name, id);
    id
}
