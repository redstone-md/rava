//! Which reflection family serves a call.
//!
//! Re-exported from the parent module, so no call site outside it changed.
//!
//! ⭐**THE OWNER LADDER WAS WALKED IN FULL BY EVERY CALL THAT THIS STAGE DECLINES.**
//! `natives::dispatch_owner` admits a call here whenever `owner_bits::S_REFLECT` is set, and
//! that stage bit is a deliberate SUPERSET: `java/lang/`, `java/util/`, `java/net/`, `javax/`,
//! `sun/`, `jdk/`, `com/google/common/` and the atomic/executor/lock substrings all enter. So
//! `java/lang/String`, `java/util/ArrayList` and `java/lang/StringBuilder` — three of the
//! busiest owners in a boot — enter this function, and a `match owner { "lit" => .. }` over
//! 54 string literals is lowered as a CHAIN: one length compare per literal (plus a `bcmp`
//! wherever the length collides) before the arm that finally declines. **The quantity that
//! grows is the number of reflection families this module serves** — every family added made
//! every declining call longer, and the answer is a pure function of `owner` alone.
//!
//! Was O(FAMILIES) string tests per call, is O(1): the literals live in one static [`OWNERS`]
//! table, a 256-slot open-addressed index over it is built once per process, and the arms
//! become a `match` on a one-byte family id — a jump table. The probe is deliberately NOT a
//! full string hash (this tree has measured a whole-name hash costing about what the chain it
//! replaced cost): the key is the name's LAST EIGHT BYTES mixed with its LENGTH, one unaligned
//! load and one multiply, and the slot is verified by a full string compare before it is
//! believed. A miss on a name that is not in the table usually reads ONE empty slot and stops.
//!
//! MEMORY: index `[u16; 256]` = 512 bytes, plus 54 `(&'static str, Fam)` = ~1.3 KB of rodata.
//! Process-wide, read-only after the first call, never per-thread and never per-site — this is
//! not the kind of table the +6.8% front-cache regression came from.
//!
//! ORDER IS PRESERVED, NOT ASSUMED. The historic match interleaved one substring GUARD (the
//! `MXBean` arm) among the literals; every literal that follows it was checked to contain
//! neither `MXBean` nor to be `java/lang/management/MemoryUsage`, so hoisting all literals
//! ahead of all guards is the same function. The two arms that a literal could otherwise
//! shadow — `Executors` and `Executors$RunnableAdapter`, both of which contain `Executor` —
//! were already ahead of the executor guard and still are.
//!
//! `RAVA_NO_FAST_REFLECT2=1` (and `RAVA_NO_EMIT_MORE=1`) restores the linear walk. The off arm
//! scans the SAME [`OWNERS`] table in source order, so the two arms cannot drift and an A/B
//! measures the index rather than a changed question.

use crate::reflect::*;

/// Which family of reflection natives an owner belongs to. One variant per historic arm of the
/// literal ladder; the id is what turns 54 string tests into a jump table.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Fam {
    Class,
    Modifier,
    Field,
    RecordComponent,
    Method,
    Ctor,
    Loader,
    UrlConnection,
    Thread,
    ThreadLocal,
    StackWalker,
    ParameterizedType,
    TypeVariable,
    SunReflection,
    Runtime,
    Package,
    InetAddress,
    ManagementFactory,
    Logger,
    TimeUnit,
    Charset,
    ProcessHandle,
    MethodHandles,
    VarHandle,
    MethodType,
    MethodHandleObj,
    Comparator,
    FnCombinator,
    EnumSet,
    ServiceLoader,
    Provider,
    ExecutorsStatic,
    ForkJoinPool,
    RunnableAdapter,
    ThreadFactory,
    Future,
    Adder,
}

/// Every owner this module names by literal, in the order the ladder tested them. This is now
/// the ONLY place those literals live: `lookup_indexed` builds its index from it and the
/// kill-switch arm `lookup_scan` walks it, so the fast and slow paths answer from one table.
static OWNERS: &[(&str, Fam)] = &[
    ("java/lang/Class", Fam::Class),
    ("java/lang/reflect/Modifier", Fam::Modifier),
    ("java/lang/reflect/Field", Fam::Field),
    ("java/lang/reflect/RecordComponent", Fam::RecordComponent),
    ("java/lang/reflect/Method", Fam::Method),
    ("java/lang/reflect/Constructor", Fam::Ctor),
    ("java/net/URLClassLoader", Fam::Loader),
    ("java/lang/ClassLoader", Fam::Loader),
    ("java/net/URI", Fam::Loader),
    ("java/net/URL", Fam::Loader),
    ("java/net/URLConnection", Fam::UrlConnection),
    ("java/net/HttpURLConnection", Fam::UrlConnection),
    ("javax/net/ssl/HttpsURLConnection", Fam::UrlConnection),
    ("java/lang/Thread", Fam::Thread),
    ("java/lang/ThreadLocal", Fam::ThreadLocal),
    ("java/lang/InheritableThreadLocal", Fam::ThreadLocal),
    ("java/lang/StackWalker", Fam::StackWalker),
    ("java/lang/reflect/ParameterizedType", Fam::ParameterizedType),
    ("java/lang/reflect/TypeVariable", Fam::TypeVariable),
    ("sun/reflect/Reflection", Fam::SunReflection),
    ("jdk/internal/reflect/Reflection", Fam::SunReflection),
    ("java/lang/Runtime", Fam::Runtime),
    ("java/lang/Package", Fam::Package),
    ("java/net/InetAddress", Fam::InetAddress),
    ("java/net/Inet4Address", Fam::InetAddress),
    ("java/net/Inet6Address", Fam::InetAddress),
    ("java/lang/management/ManagementFactory", Fam::ManagementFactory),
    ("java/util/logging/Logger", Fam::Logger),
    ("java/util/concurrent/TimeUnit", Fam::TimeUnit),
    ("java/nio/charset/Charset", Fam::Charset),
    ("java/lang/ProcessHandle", Fam::ProcessHandle),
    ("java/lang/invoke/MethodHandles", Fam::MethodHandles),
    ("java/lang/invoke/MethodHandles$Lookup", Fam::MethodHandles),
    ("java/lang/invoke/VarHandle", Fam::VarHandle),
    ("java/lang/invoke/MethodType", Fam::MethodType),
    ("java/lang/invoke/MethodHandle", Fam::MethodHandleObj),
    ("java/util/Comparator", Fam::Comparator),
    ("java/util/function/Function", Fam::FnCombinator),
    ("java/util/function/UnaryOperator", Fam::FnCombinator),
    ("java/util/EnumSet", Fam::EnumSet),
    ("java/util/ServiceLoader", Fam::ServiceLoader),
    ("java/util/ServiceLoader$Provider", Fam::Provider),
    ("java/util/concurrent/Executors", Fam::ExecutorsStatic),
    ("java/util/concurrent/ForkJoinPool", Fam::ForkJoinPool),
    ("java/util/concurrent/Executors$RunnableAdapter", Fam::RunnableAdapter),
    ("java/util/concurrent/ThreadFactory", Fam::ThreadFactory),
    ("java/util/concurrent/Future", Fam::Future),
    ("java/util/concurrent/FutureTask", Fam::Future),
    ("java/util/concurrent/CompletableFuture", Fam::Future),
    // guava futures: ListenableFutureTask.create(..).run() backs MC's
    // SystemUtils main-thread task queue; SettableFuture/AbstractFuture share
    // the same complete/get/listener model.
    ("com/google/common/util/concurrent/ListenableFutureTask", Fam::Future),
    ("com/google/common/util/concurrent/SettableFuture", Fam::Future),
    ("com/google/common/util/concurrent/AbstractFuture", Fam::Future),
    ("java/util/concurrent/atomic/LongAdder", Fam::Adder),
    ("java/util/concurrent/atomic/DoubleAdder", Fam::Adder),
];

/// Power of two, ~21% load at 54 entries — chosen so the overwhelmingly common case (an owner
/// that belongs to NO family) reads one empty slot and returns. 512 bytes of `u16`.
const SLOTS: usize = 256;
/// No entry. `OWNERS` can never reach 65535, and a `u16` keeps the whole index in 512 bytes
/// (eight lines) rather than the 2 KB a `usize` slot would take.
const EMPTY: u16 = u16::MAX;

/// The slot key: the name's last eight bytes mixed with its length, then one multiply and the
/// top eight bits. A class name's TAIL is its simple name, which is where these owners differ;
/// hashing all 40-60 bytes would cost about what the chain being replaced cost. Correctness
/// does not rest on this — every candidate slot is verified with a full string compare.
#[inline]
fn slot_of(owner: &str) -> usize {
    let b = owner.as_bytes();
    let tail = u64::from_le_bytes(b[b.len() - 8..].try_into().unwrap());
    let k = tail ^ ((b.len() as u64) << 3);
    (k.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as usize
}

/// The index, built once per process from [`OWNERS`] and immutable thereafter — so the read
/// path takes no lock and touches no thread-local.
fn index() -> &'static [u16; SLOTS] {
    static IDX: std::sync::OnceLock<[u16; SLOTS]> = std::sync::OnceLock::new();
    IDX.get_or_init(|| {
        let mut t = [EMPTY; SLOTS];
        for (i, (o, _)) in OWNERS.iter().enumerate() {
            let mut s = slot_of(o);
            while t[s] != EMPTY {
                s = (s + 1) & (SLOTS - 1);
            }
            t[s] = i as u16;
        }
        t
    })
}

/// DEFAULT-ON. `RAVA_NO_FAST_REFLECT2=1` restores the linear walk of [`OWNERS`] and the eager
/// executor-capability probe in `concurrent::executor_method`, in one switch.
pub(crate) fn fast_reflect2() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_FAST_REFLECT2").is_none()
    })
}

/// Which family serves `owner`, or `None` if no literal arm names it — O(1).
#[inline]
fn lookup_indexed(owner: &str) -> Option<Fam> {
    // Shorter than the key: no owner in the table is, so this is a proof of absence and it
    // also keeps `slot_of`'s tail load in bounds.
    if owner.len() < 8 {
        return None;
    }
    let t = index();
    let mut s = slot_of(owner);
    loop {
        let e = t[s];
        if e == EMPTY {
            return None;
        }
        let (n, f) = OWNERS[e as usize];
        if n == owner {
            return Some(f);
        }
        s = (s + 1) & (SLOTS - 1);
    }
}

/// The same question the historic ladder asked, one `==` per literal in source order — the
/// kill-switch arm. Same table, so the two arms answer identically by construction.
fn lookup_scan(owner: &str) -> Option<Fam> {
    OWNERS.iter().find(|(n, _)| *n == owner).map(|&(_, f)| f)
}

#[inline]
fn family(owner: &str) -> Option<Fam> {
    if fast_reflect2() { lookup_indexed(owner) } else { lookup_scan(owner) }
}

pub(crate) fn dispatch(vm: &mut Vm, owner: &str, name: &str, args: &[Value]) -> Handled {
    match family(owner) {
        Some(f) => by_family(vm, f, owner, name, args),
        None => tail(vm, owner, name, args),
    }
}

/// The bodies of the literal arms, selected by id instead of by name.
fn by_family(vm: &mut Vm, fam: Fam, owner: &str, name: &str, args: &[Value]) -> Handled {
    match fam {
        Fam::Class => class_method(vm, name, args),
        // java.lang.reflect.Modifier flag tests — classfile access_flags use the
        // same bit values, so these are direct mask checks.
        Fam::Modifier => {
            let m = args.first().and_then(Value::as_int).unwrap_or(0);
            let bit = match name {
                "isPublic" => 0x1, "isPrivate" => 0x2, "isProtected" => 0x4, "isStatic" => 0x8,
                "isFinal" => 0x10, "isSynchronized" => 0x20, "isVolatile" => 0x40, "isTransient" => 0x80,
                "isNative" => 0x100, "isInterface" => 0x200, "isAbstract" => 0x400, "isStrict" => 0x800,
                _ => return Ok(None),
            };
            Ok(Some(Some(Value::Int(((m & bit) != 0) as i32))))
        }
        Fam::Field => field_method(vm, name, args),
        // A record component answers its name, its type and the record that declares it.
        // `getAccessor` is the component's reader method, which has the component's own
        // name and no arguments.
        Fam::RecordComponent => {
            let Some(Value::Ref(h)) = args.first() else { return Ok(Some(Some(Value::Null))) };
            let h = *h;
            let sf = |vm: &mut Vm, k: &str| match vm.heap.get_field(h, k) {
                Value::Str(s) => s.to_string(),
                _ => String::new(),
            };
            let v = match name {
                "getName" | "toString" => Value::Str(sf(vm, "$name").into()),
                "getType" => {
                    let d = sf(vm, "$desc");
                    vm.make_class(&descriptor_to_name(&d))
                }
                "getDeclaringRecord" => {
                    let o = sf(vm, "$owner");
                    vm.make_class(&o)
                }
                "getGenericType" => {
                    let d = sf(vm, "$desc");
                    vm.make_class(&descriptor_to_name(&d))
                }
                "getAccessor" => {
                    let (o, n) = (sf(vm, "$owner"), sf(vm, "$name"));
                    make_method(vm, &o, &n, "")
                }
                _ => return Ok(None),
            };
            Ok(Some(Some(v)))
        }
        Fam::Method => method_method(vm, name, args),
        Fam::Ctor => ctor_method(vm, name, args),
        Fam::Loader => loader_method(vm, owner, name, args),
        Fam::UrlConnection => urlconnection_method(vm, name, args),
        Fam::Thread => thread_method(vm, name, args),
        Fam::ThreadLocal => threadlocal(vm, name, args),
        Fam::StackWalker => stackwalker(vm, name, args),
        Fam::ParameterizedType => parameterized_type(vm, name, args),
        Fam::TypeVariable => match name {
            "getName" | "toString" | "getTypeName" => Ok(Some(Some(Value::Str(
                field_str(vm, args.first(), "$name").unwrap_or_default().into())))),
            "getBounds" => Ok(Some(Some(Value::Array(vm.heap.alloc_values("Ljava/lang/reflect/Type;", Vec::new()))))),
            // The Class that declares this type variable (e.g. List for List<E>'s E).
            // gson's $Gson$Types.resolveTypeVariable needs it (declaringClassOf) to
            // walk to the supplier ParameterizedType. Null when unset (signature TVs).
            "getGenericDeclaration" => {
                let d = args.first().map(|r| if let Value::Ref(h) = r { vm.heap.get_field(*h, "$decl") } else { Value::Null }).unwrap_or(Value::Null);
                Ok(Some(Some(d)))
            }
            // Identity by (name, declaring class) — gson's indexOf(typeParams, tv)
            // does `tv.equals(typeParams[i])`, so the E in `List.getGenericInterfaces`
            // (Collection<E>) must equal the E in `List.getTypeParameters`.
            "equals" => {
                let same = match (args.first(), args.get(1)) {
                    (Some(Value::Ref(a)), Some(Value::Ref(b))) => {
                        vm.heap.class_of(*b) == Some("java/lang/reflect/TypeVariable")
                            && field_str(vm, Some(&Value::Ref(*a)), "$name") == field_str(vm, Some(&Value::Ref(*b)), "$name")
                            && decl_name(vm, *a) == decl_name(vm, *b)
                    }
                    _ => false,
                };
                Ok(Some(Some(Value::Int(same as i32))))
            }
            "hashCode" => {
                let n = field_str(vm, args.first(), "$name").unwrap_or_default();
                Ok(Some(Some(Value::Int(n.bytes().fold(0i32, |a, b| a.wrapping_mul(31).wrapping_add(b as i32))))))
            }
            _ => Ok(Some(Some(Value::Null))),
        },
        // JDK-8-era caller introspection (log4j 2.x's StackLocator hard-depends
        // on it). Depths count from the Reflection frame itself, which we don't
        // model (it's native here): depth 1 = the method calling getCallerClass,
        // 2 = its caller, etc. Out-of-range yields null (callers iterate to null).
        Fam::SunReflection => match name {
            "getCallerClass" => {
                let frames = vm.live_frame_classes();
                let cls = match args.first() {
                    Some(Value::Int(d)) => {
                        let d = *d as usize;
                        if d == 0 {
                            Some("sun/reflect/Reflection".to_string())
                        } else {
                            frames.len().checked_sub(d).and_then(|i| frames.get(i).cloned())
                        }
                    }
                    // No-arg form: the caller of the method that called us.
                    _ => frames.len().checked_sub(2).and_then(|i| frames.get(i).cloned()),
                };
                Ok(Some(Some(match cls {
                    Some(c) => vm.make_class(&c),
                    None => Value::Null,
                })))
            }
            _ => Ok(Some(Some(Value::Null))),
        },
        Fam::Runtime => runtime_method(vm, name),
        Fam::Package => match name {
            "getName" => Ok(Some(Some(Value::Str(field_str(vm, args.first(), "$name").unwrap_or_default().into())))),
            "getImplementationVersion" | "getSpecificationVersion" | "getImplementationTitle"
            | "getImplementationVendor" | "getSpecificationTitle" => Ok(Some(Some(Value::Null))),
            "isSealed" => Ok(Some(Some(Value::Int(0)))),
            _ => Ok(Some(Some(Value::Null))),
        },
        Fam::InetAddress => match name {
            "getLocalHost" | "getByName" | "getByAddress" | "getLoopbackAddress" => {
                let h = vm.heap.alloc("java/net/InetAddress");
                let host = match args.first() { Some(Value::Str(s)) => s.to_string(), _ => "localhost".to_string() };
                vm.heap.put_field(h, "$host", Value::Str(host.into()));
                vm.heap.put_field(h, "$addr", Value::Str("127.0.0.1".into()));
                Ok(Some(Some(Value::Ref(h))))
            }
            "getHostAddress" => Ok(Some(Some(Value::Str(field_str(vm, args.first(), "$addr").unwrap_or_else(|| "127.0.0.1".into()).into())))),
            "getHostName" | "getCanonicalHostName" => Ok(Some(Some(Value::Str(field_str(vm, args.first(), "$host").unwrap_or_else(|| "localhost".into()).into())))),
            "isLoopbackAddress" | "isAnyLocalAddress" => Ok(Some(Some(Value::Int(1)))),
            "toString" => Ok(Some(Some(Value::Str(format!("{}/{}", field_str(vm, args.first(), "$host").unwrap_or_default(), field_str(vm, args.first(), "$addr").unwrap_or_default()).into())))),
            _ => Ok(Some(Some(Value::Null))),
        },
        Fam::ManagementFactory => {
            let cls = match name {
                "getRuntimeMXBean" => "java/lang/management/RuntimeMXBean",
                "getMemoryMXBean" => "java/lang/management/MemoryMXBean",
                "getThreadMXBean" => "java/lang/management/ThreadMXBean",
                "getOperatingSystemMXBean" => "java/lang/management/OperatingSystemMXBean",
                "getClassLoadingMXBean" => "java/lang/management/ClassLoadingMXBean",
                "getPlatformMBeanServer" => "javax/management/MBeanServer",
                "getGarbageCollectorMXBeans" | "getMemoryPoolMXBeans" => {
                    let arr = vm.heap.alloc_values("Ljava/lang/Object;", Vec::new());
                    let list = vm.heap.alloc("java/util/ArrayList");
                    vm.heap.put_field_syn(list, crate::heap::Syn::Elems, Value::Array(arr));
                    return Ok(Some(Some(Value::Ref(list))));
                }
                _ => return Ok(Some(Some(Value::Null))),
            };
            Ok(Some(Some(Value::Ref(vm.heap.alloc(cls)))))
        }
        Fam::Logger => match name {
            "getLogger" | "getGlobal" | "getAnonymousLogger" => Ok(Some(Some(Value::Ref(vm.heap.alloc("java/util/logging/Logger"))))),
            "isLoggable" => Ok(Some(Some(Value::Int(0)))),
            "getName" => Ok(Some(Some(args.first().cloned().unwrap_or(Value::Null)))),
            // getHandlers MUST be a (possibly empty) array, never null —
            // DedicatedServer.init() does `getHandlers().length` and a null
            // would NPE on arraylength.
            "getHandlers" => Ok(Some(Some(Value::Array(vm.heap.alloc_values("Ljava/util/logging/Handler;", Vec::new()))))),
            "getLevel" | "getParent" | "getResourceBundle" | "getFilter" => Ok(Some(Some(Value::Null))),
            _ => Ok(Some(None)), // all the log/set/add methods are void no-ops
        },
        Fam::TimeUnit => timeunit_method(vm, name, args),
        Fam::Charset => match name {
            "forName" | "defaultCharset" => {
                let cs = match args.first() { Some(Value::Str(s)) => s.to_string(), _ => "UTF-8".to_string() };
                let h = vm.heap.alloc("java/nio/charset/Charset");
                vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str(cs.into()));
                Ok(Some(Some(Value::Ref(h))))
            }
            "isSupported" => Ok(Some(Some(Value::Int(1)))),
            "name" | "displayName" | "toString" => Ok(Some(Some(
                Value::Str(field_str(vm, args.first(), "$name").unwrap_or_else(|| "UTF-8".into()).into())))),
            _ => Ok(None),
        },
        Fam::ProcessHandle => match name {
            // current()/parent() yield a ProcessHandle; pid()/getPid() the OS pid.
            "current" => Ok(Some(Some(Value::Ref(vm.heap.alloc("java/lang/ProcessHandle"))))),
            "pid" | "getPid" => Ok(Some(Some(Value::Long(std::process::id() as i64)))),
            "isAlive" => Ok(Some(Some(Value::Int(1)))),
            "parent" => Ok(Some(Some(Value::Null))), // Optional.empty handled by caller
            _ => Ok(Some(Some(Value::Null))),
        },
        Fam::MethodHandles => methodhandles(vm, name, args),
        Fam::VarHandle => varhandle(vm, name, args),
        Fam::MethodType => methodtype(vm, name, args),
        Fam::MethodHandleObj => methodhandle_obj(vm, name, args),
        Fam::Comparator => comparator(vm, name, args),
        Fam::FnCombinator => fn_combinator(vm, name, args),
        Fam::EnumSet => enum_set(vm, name, args),
        Fam::ServiceLoader => service_loader(vm, name, args),
        Fam::Provider => provider(vm, name, args),
        Fam::ExecutorsStatic => executors_static(vm, name, args),
        // `ForkJoinPool.commonPool()` is what the JDK's own `supplyAsync(fn, pool)` and
        // every parallel stream reach for, and it did not exist — an UnsatisfiedLinkError
        // on a line as ordinary as `supplyAsync(() -> x, ForkJoinPool.commonPool())`.
        // A pool from here carries the same resident-task capability an `Executors`
        // factory stamps, so tasks handed to it take the same route.
        //
        // THE ONE ARM WITH A NAME GUARD, so it is the one arm that can FALL THROUGH: any
        // other ForkJoinPool method continued down the ladder before, and continues into
        // the same guard tail now.
        Fam::ForkJoinPool => {
            if name == "getCommonPoolParallelism" {
                let n = std::thread::available_parallelism().map(|p| p.get() as i32).unwrap_or(1);
                return Ok(Some(Some(Value::Int((n - 1).max(1)))));
            }
            if name != "commonPool" {
                return tail(vm, owner, name, args);
            }
            let h = vm.heap.alloc("java/util/concurrent/ForkJoinPool");
            vm.heap.put_field(h, RESIDENT_TASK_CAPABILITY, Value::Int(1));
            Ok(Some(Some(Value::Ref(h))))
        }
        Fam::RunnableAdapter => runnable_adapter(vm, name, args),
        Fam::ThreadFactory => {
            if name == "newThread" {
                let t = vm.heap.alloc("java/lang/Thread");
                if let Some(r) = args.get(1).cloned() { vm.heap.put_field(t, "$runnable", r); }
                Ok(Some(Some(Value::Ref(t))))
            } else { Ok(None) }
        }
        Fam::Future => future_method(vm, name, args),
        Fam::Adder => adder_method(vm, name, args),
    }
}

/// The substring/suffix guards, in the order the ladder reached them. These are the arms whose
/// question is not an exact name, so they cannot be keyed — but each is already ONE bit test
/// against the per-name `owner_bits` memo rather than the `str::contains` it replaced (that
/// memo exists because these six guards were 30.3% of all `str::contains` in a real boot).
fn tail(vm: &mut Vm, owner: &str, name: &str, args: &[Value]) -> Handled {
    let o = owner;
    if ob::contains_any(o, ob::MXBEAN) || o == "java/lang/management/MemoryUsage" {
        return mxbean_method(vm, name, args);
    }
    if ob::contains_any(o, ob::ATOMIC) && o.ends_with("Array") {
        return atomic_array(vm, name, args);
    }
    if ob::contains_any(o, ob::FIELD_UPDATER) {
        return field_updater(vm, name, args);
    }
    if ob::contains_any(o, ob::ATOMIC) {
        return atomic_method(vm, name, args);
    }
    if ob::contains_any(o, ob::EXECUTOR | ob::MAILBOX)
        || o.ends_with("EventLoop")
        || o.ends_with("ConsecutiveExecutor")
        || o.ends_with("Scheduler")
    {
        return executor_method(vm, name, args);
    }
    if ob::contains_any(o, ob::LOCK) || o.ends_with("Condition") || o.ends_with("Semaphore") {
        return locks(vm, name, args);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The index must answer for every literal the ladder named, and the kill-switch scan must
    /// answer identically — the drift check that lets the switch be a performance switch only.
    #[test]
    fn index_and_scan_agree_on_every_owner() {
        for (o, f) in OWNERS {
            assert!(lookup_indexed(o) == Some(*f), "{o} indexed to the wrong family");
            assert!(lookup_scan(o) == Some(*f), "{o} scanned to the wrong family");
        }
        // No duplicate literal: a second entry for a name already present would be dead in the
        // scan and unreachable in the index, i.e. a silently ignored arm.
        for (i, (o, _)) in OWNERS.iter().enumerate() {
            assert!(!OWNERS[..i].iter().any(|(p, _)| p == o), "{o} listed twice");
        }
    }

    /// A name no arm claims must miss, on both arms.
    #[test]
    fn foreign_owners_miss() {
        for o in ["java/lang/String", "java/util/ArrayList", "java/lang/StringBuilder", "x", ""] {
            assert!(lookup_indexed(o).is_none(), "{o} wrongly claimed a family");
            assert!(lookup_scan(o).is_none(), "{o} wrongly claimed a family");
        }
    }

    /// Hoisting every literal ahead of every guard is only the same function while no literal
    /// could have been SERVED by a guard that used to precede it. Two guards did: the `MXBean`
    /// arm (ahead of everything from `java/util/logging/Logger` down) and the atomic/updater
    /// trio (ahead of the two `Adder` literals). Assert it rather than trust the reading.
    #[test]
    fn no_literal_is_shadowed_by_a_hoisted_guard() {
        for (o, _) in OWNERS {
            assert!(!o.contains("MXBean"), "{o} would have gone to mxbean_method");
            assert_ne!(*o, "java/lang/management/MemoryUsage");
            assert!(!o.contains("/atomic/Atomic"), "{o} would have gone to the atomic arms");
            assert!(!o.contains("FieldUpdater"), "{o} would have gone to field_updater");
        }
    }
}
