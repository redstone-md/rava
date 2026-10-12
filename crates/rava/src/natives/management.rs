//! `java.lang.management` and `com.sun.management` natives: the VM, memory and operating-system
//! beans, and the container metrics probe. Values come from the heap and from the kernel
//! (`sysinfo`, `/proc`), as HotSpot's own implementations read them.

use crate::heap::{obj, Oop};
use super::{ret_bool, ret_int, ret_long, ret_ref, ret_void, NativeRegistry};
use crate::runtime::{self as rt};
use crate::thread::{JResult, Thread};
use crate::vm::vm;

pub(super) fn register(r: &mut NativeRegistry) {
    register_vm(r);
    register_memory(r);
    register_os(r);
    register_flags(r);
    // No container limits are applied, so the JDK falls back to the host's values.
    r.add("jdk/internal/platform/CgroupMetrics", "isUseContainerSupport", "()Z", |_, _| ret_bool(false));
    r.add("jdk/internal/platform/CgroupMetrics", "isContainerized0", "()Z", |_, _| ret_bool(false));
    r.add("jdk/internal/platform/CgroupMetrics", "getTotalMemorySize0", "()J", |_, _| ret_long(sys().0));
    r.add("jdk/internal/platform/CgroupMetrics", "getTotalSwapSize0", "()J", |_, _| ret_long(sys().2));
}

/// `sun.management.VMManagementImpl`: what `java.lang.management` reports about this VM.
fn register_vm(r: &mut NativeRegistry) {
    const M: &str = "sun/management/VMManagementImpl";
    fn started() -> &'static std::time::Instant {
        static S: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        S.get_or_init(std::time::Instant::now)
    }
    started();
    r.add(M, "getVersion0", "()Ljava/lang/String;", |t, _| ret_ref(rt::new_string(t, "4.0")?));
    r.add(M, "initOptionalSupportFields", "()V", |_, _| ret_void());
    for name in ["isThreadContentionMonitoringEnabled", "isThreadCpuTimeEnabled", "isThreadAllocatedMemoryEnabled", "getVerboseClass", "getVerboseGC"] {
        r.add(M, name, "()Z", |_, _| ret_bool(false));
    }
    r.add(M, "getProcessId", "()I", |_, _| ret_int(std::process::id() as i32));
    r.add(M, "getVmArguments0", "()[Ljava/lang/String;", |t, _| ret_ref(super::string_array(t, &[])?));
    r.add(M, "getStartupTime", "()J", |_, _| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        ret_long(now - started().elapsed().as_millis() as i64)
    });
    r.add(M, "getUptime0", "()J", |_, _| ret_long(started().elapsed().as_millis() as i64));
    r.add(M, "getAvailableProcessors", "()I", |_, _| ret_int(crate::processors() as i32));
    r.add(M, "getTotalClassCount", "()J", |_, _| ret_long(vm().loaded_class_count() as i64));
    r.add(M, "getLiveThreadCount", "()I", |_, _| ret_int(crate::gc::safepoint::all_threads().len() as i32));
    for name in ["getUnloadedClassCount", "getTotalCompileTime", "getTotalThreadCount", "getSafepointCount", "getTotalSafepointTime", "getSafepointSyncTime", "getTotalApplicationNonStoppedTime", "getLoadedClassSize", "getUnloadedClassSize", "getClassLoadingTime", "getMethodDataSize", "getInitializedClassCount", "getClassInitializationTime", "getClassVerificationTime"] {
        r.add(M, name, "()J", |_, _| ret_long(0));
    }
    for name in ["getPeakThreadCount", "getDaemonThreadCount"] {
        r.add(M, name, "()I", |_, _| ret_int(0));
    }
}

fn register_memory(r: &mut NativeRegistry) {
    const M: &str = "sun/management/MemoryImpl";
    r.add(M, "getMemoryPools0", "()[Ljava/lang/management/MemoryPoolMXBean;", |t, _| empty_array(t, "java/lang/management/MemoryPoolMXBean"));
    r.add(M, "getMemoryManagers0", "()[Ljava/lang/management/MemoryManagerMXBean;", |t, _| empty_array(t, "java/lang/management/MemoryManagerMXBean"));
    r.add(M, "setVerboseGC", "(Z)V", |_, _| ret_void());
    r.add(M, "getMemoryUsage0", "(Z)Ljava/lang/management/MemoryUsage;", |t, a| {
        // MemoryUsage(init, used, committed, max); the non-heap areas are not accounted.
        let usage = if a[1] != 0 {
            let max = vm().heap.max_bytes() as i64;
            let used = (vm().heap.used_bytes() as i64).min(max);
            [0, used, used.max(64 << 20).min(max), max]
        } else {
            [0, 0, 0, -1]
        };
        // Each long takes two argument slots.
        let args: Vec<u64> = usage.iter().flat_map(|&v| [v as u64, 0]).collect();
        ret_ref(crate::boot::construct(t, "java/lang/management/MemoryUsage", "(JJJJ)V", &args)?)
    });
}

/// A VM flag's value as `com.sun.management.internal.Flag` holds it.
enum FlagValue {
    Bool(bool),
    Long(i64),
    Double(f64),
    Str(String),
}

/// The VM's flags: a few defaults HotSpot reports for any VM, then the command line's `-XX:`
/// options (later ones win), with `VMOption.Origin` names.
fn vm_flags() -> Vec<(String, FlagValue, &'static str)> {
    let mut flags: Vec<(String, FlagValue, &'static str)> = vec![
        ("MaxHeapSize".into(), FlagValue::Long(vm().heap.max_bytes() as i64), "ERGONOMIC"),
        ("UseCompressedOops".into(), FlagValue::Bool(crate::heap::COMPRESSED), "ERGONOMIC"),
        ("DisableExplicitGC".into(), FlagValue::Bool(false), "DEFAULT"),
        ("ActiveProcessorCount".into(), FlagValue::Long(-1), "DEFAULT"),
    ];
    for o in crate::vm_options() {
        let (name, value) = if let Some(n) = o.strip_prefix('+') {
            (n.to_string(), FlagValue::Bool(true))
        } else if let Some(n) = o.strip_prefix('-') {
            (n.to_string(), FlagValue::Bool(false))
        } else if let Some((n, v)) = o.split_once('=') {
            let value = match crate::launcher::parse_vm_size(v) {
                Some(x) if v.chars().next().is_some_and(|c| c.is_ascii_digit()) => FlagValue::Long(x as i64),
                _ => v.parse::<i64>().map(FlagValue::Long).or_else(|_| v.parse::<f64>().map(FlagValue::Double)).unwrap_or_else(|_| FlagValue::Str(v.to_string())),
            };
            (n.to_string(), value)
        } else {
            continue;
        };
        flags.retain(|f| f.0 != name);
        flags.push((name, value, "VM_CREATION"));
    }
    flags
}

/// The value of static reference field `name` of `class` (initialized first).
fn static_ref(t: &mut Thread, class: &str, name: &str, desc: &str) -> JResult<Oop> {
    let k = crate::loader::load(class).map_err(|e| crate::runtime::fatal(&format!("cannot load {class}: {e:?}")))?;
    rt::initialize(t, k)?;
    let f = k.declared_field(crate::symbol::Symbol::intern(name), crate::symbol::Symbol::intern(desc)).ok_or_else(|| crate::runtime::fatal(&format!("{class}.{name}")))?;
    Ok(k.statics[f.offset as usize].load(std::sync::atomic::Ordering::Acquire) as Oop)
}

fn register_flags(r: &mut NativeRegistry) {
    const FLAG: &str = "com/sun/management/internal/Flag";
    r.add(FLAG, "initialize", "()V", |_, _| ret_void());
    r.add(FLAG, "getInternalFlagCount", "()I", |_, _| ret_int(vm_flags().len() as i32));
    r.add(FLAG, "getAllFlagNames", "()[Ljava/lang/String;", |t, _| {
        let names: Vec<Option<String>> = vm_flags().into_iter().map(|f| Some(f.0)).collect();
        ret_ref(super::string_array(t, &names)?)
    });
    // getFlags(names, flags, count): a Flag for each named flag the VM has (all with null
    // names), null for the others; returns how many it set.
    r.add(FLAG, "getFlags", "([Ljava/lang/String;[Lcom/sun/management/internal/Flag;I)I", |t, a| {
        let (names, out, count) = (a[0] as Oop, a[1] as Oop, a[2] as i32);
        let flags = vm_flags();
        let wanted: Vec<Option<String>> = if names.is_null() {
            flags.iter().map(|f| Some(f.0.clone())).collect()
        } else {
            // SAFETY: a String[] argument.
            (0..unsafe { obj::array_length(names) }).map(|i| {
                let s = unsafe { obj::load_elem(names, i as usize) };
                (!s.is_null()).then(|| rt::java_string_to_rust(s))
            }).collect()
        };
        let mut set = 0;
        for (i, name) in wanted.iter().enumerate().take(count.max(0) as usize) {
            let Some((name, value, origin)) = name.as_ref().and_then(|n| flags.iter().find(|f| &f.0 == n)) else { continue };
            let boxed = match value {
                FlagValue::Bool(b) => crate::boot::construct(t, "java/lang/Boolean", "(Z)V", &[*b as u64])?,
                FlagValue::Long(x) => crate::boot::construct(t, "java/lang/Long", "(J)V", &[*x as u64, 0])?,
                FlagValue::Double(x) => crate::boot::construct(t, "java/lang/Double", "(D)V", &[x.to_bits(), 0])?,
                FlagValue::Str(s) => rt::new_string(t, s)?,
            };
            let jname = rt::new_string(t, name)?;
            let o = static_ref(t, "com/sun/management/VMOption$Origin", origin, "Lcom/sun/management/VMOption$Origin;")?;
            let flag = crate::boot::construct(
                t,
                "com/sun/management/internal/Flag",
                "(Ljava/lang/String;Ljava/lang/Object;ZZLcom/sun/management/VMOption$Origin;)V",
                &[jname as u64, boxed as u64, 0, 1, o as u64],
            )?;
            // SAFETY: an element of the Flag[] argument.
            unsafe { obj::store_elem(out, i, flag) };
            set += 1;
        }
        ret_int(set)
    });
    for (name, desc) in [("setLongValue", "(Ljava/lang/String;J)V"), ("setDoubleValue", "(Ljava/lang/String;D)V"), ("setBooleanValue", "(Ljava/lang/String;Z)V"), ("setStringValue", "(Ljava/lang/String;Ljava/lang/String;)V")] {
        r.add(FLAG, name, desc, |_, _| ret_void());
    }
}

fn register_os(r: &mut NativeRegistry) {
    const OS: &str = "com/sun/management/internal/OperatingSystemImpl";
    r.add(OS, "initialize0", "()V", |_, _| ret_void());
    r.add(OS, "getTotalMemorySize0", "()J", |_, _| ret_long(sys().0));
    r.add(OS, "getFreeMemorySize0", "()J", |_, _| ret_long(sys().1));
    r.add(OS, "getTotalSwapSpaceSize0", "()J", |_, _| ret_long(sys().2));
    r.add(OS, "getFreeSwapSpaceSize0", "()J", |_, _| ret_long(sys().3));
    r.add(OS, "getCommittedVirtualMemorySize0", "()J", |_, _| {
        let pages = std::fs::read_to_string("/proc/self/statm").ok().and_then(|s| s.split_whitespace().next()?.parse::<i64>().ok());
        ret_long(pages.map_or(-1, |p| p * unsafe { libc::sysconf(libc::_SC_PAGESIZE) }))
    });
    r.add(OS, "getMaxFileDescriptorCount0", "()J", |_, _| {
        let mut rl: libc::rlimit = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } == 0;
        ret_long(if ok { rl.rlim_cur as i64 } else { -1 })
    });
    r.add(OS, "getOpenFileDescriptorCount0", "()J", |_, _| {
        ret_long(std::fs::read_dir("/proc/self/fd").map_or(-1, |d| d.count() as i64))
    });
    r.add(OS, "getProcessCpuTime0", "()J", |_, _| {
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) } == 0;
        ret_long(if ok { ts.tv_sec * 1_000_000_000 + ts.tv_nsec } else { -1 })
    });
    // Loads are unavailable (-1), which the bean reports as such.
    r.add(OS, "getProcessCpuLoad0", "()D", |_, _| Ok((-1.0f64).to_bits()));
    r.add(OS, "getCpuLoad0", "()D", |_, _| Ok((-1.0f64).to_bits()));
    r.add(OS, "getSingleCpuLoad0", "(I)D", |_, _| Ok((-1.0f64).to_bits()));
    r.add(OS, "getHostConfiguredCpuCount0", "()I", |_, _| ret_int(unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) } as i32));
    r.add(OS, "getHostOnlineCpuCount0", "()I", |_, _| ret_int(unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) } as i32));
    r.add(OS, "getHostTotalCpuTicks0", "()J", |_, _| {
        // The first line of /proc/stat: the jiffies all CPUs spent in each state.
        let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
        let line = stat.lines().next().unwrap_or("");
        ret_long(line.split_whitespace().skip(1).filter_map(|v| v.parse::<i64>().ok()).sum())
    });
}

/// Total and free memory, total and free swap, in bytes.
fn sys() -> (i64, i64, i64, i64) {
    super::posix::memory()
}

fn empty_array(t: &mut Thread, elem: &str) -> JResult<u64> {
    let k = crate::loader::load(elem).expect("a management interface");
    let array_class = vm().array_class_of(k).expect("its array class");
    ret_ref(rt::alloc_array(t, array_class, 0)?)
}
