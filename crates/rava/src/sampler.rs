//! A sampling profiler for Windows: `RAVA_SAMPLE=<file>` samples every Java thread's
//! instruction pointer each millisecond while it uses the CPU (its cycle count advanced since
//! the last sample) and writes, every ten seconds, the counts per thread and location:
//! compiled Java methods by name (from the code cache's perf map), the VM's own code and other
//! modules as `module+offset` (resolve the VM's with `llvm-symbolizer --obj=rava.exe` and the
//! PDB of a `release-diag` build).

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Diagnostics::Debug::{GetThreadContext, CONTEXT, CONTEXT_CONTROL_AMD64};
    use windows_sys::Win32::System::Threading::{
        GetCurrentThreadId, GetThreadDescription, OpenThread, ResumeThread, SuspendThread, THREAD_GET_CONTEXT, THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
    };

    #[link(name = "kernel32")]
    extern "system" {
        fn QueryThreadCycleTime(thread: HANDLE, cycles: *mut u64) -> i32;
    }

    static ON: AtomicBool = AtomicBool::new(false);

    /// A sampled thread: its handle, the cycle count at the last sample, and the cycles it used
    /// since sampling started.
    struct Sampled {
        id: u32,
        handle: HANDLE,
        cycles: u64,
        used: u64,
    }

    /// Cycles a thread must have used since the last sample to be sampled: a thread that only
    /// woke from a timed wait in that millisecond is not using the CPU (~0.1 ms at 3 GHz).
    const BUSY_CYCLES: u64 = 300_000;

    // Handles are process-wide.
    unsafe impl Send for Sampled {}

    static THREADS: Mutex<Vec<Sampled>> = Mutex::new(Vec::new());

    #[repr(C, align(16))]
    struct Ctx(CONTEXT);

    pub fn attach_current() {
        if !ON.load(Relaxed) {
            return;
        }
        // SAFETY: plain queries and an open of the calling thread.
        unsafe {
            let id = GetCurrentThreadId();
            let handle = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_LIMITED_INFORMATION, 0, id);
            if !handle.is_null() {
                THREADS.lock().unwrap().push(Sampled { id, handle, cycles: 0, used: 0 });
            }
        }
    }

    pub fn detach_current() {
        if !ON.load(Relaxed) {
            return;
        }
        // SAFETY: the calling thread's id; the handle was opened by `attach_current`.
        let id = unsafe { GetCurrentThreadId() };
        THREADS.lock().unwrap().retain(|s| {
            if s.id == id {
                unsafe { CloseHandle(s.handle) };
                false
            } else {
                true
            }
        });
    }

    fn name(handle: HANDLE) -> String {
        let mut p: *mut u16 = std::ptr::null_mut();
        // SAFETY: the description is returned in a buffer the system allocates (leaked: names
        // are read once per thread per report).
        unsafe {
            if GetThreadDescription(handle, &mut p) < 0 || p.is_null() {
                return "?".into();
            }
            let n = (0..).take_while(|&i| *p.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            if s.is_empty() { "?".into() } else { s }
        }
    }

    pub fn start() {
        let Some(path) = std::env::var_os("RAVA_SAMPLE") else { return };
        ON.store(true, Relaxed);
        // The code cache's names, kept in memory.
        crate::jit::code_cache::keep_names();
        let _ = std::thread::Builder::new().name("rava-sampler".into()).spawn(move || {
            let mut counts: HashMap<(u32, usize, usize, usize), u64> = HashMap::new();
            let stubs = module_ranges(&["ntdll.dll", "win32u.dll"]);
            let system = module_ranges(&["ntdll.dll", "win32u.dll", "KERNELBASE.dll", "kernel32.dll", "user32.dll", "gdi32.dll", "gdi32full.dll"]);
            let mut modules = all_modules();
            let mut names: HashMap<u32, String> = HashMap::new();
            let mut used: HashMap<u32, u64> = HashMap::new();
            let mut last_report = std::time::Instant::now();
            let mut ctx: Box<Ctx> = Box::new(unsafe { std::mem::zeroed() });
            let mut pcs: Vec<(u32, usize, usize, usize)> = Vec::with_capacity(512);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(1));
                pcs.clear();
                {
                    let mut threads = THREADS.lock().unwrap();
                    for s in threads.iter_mut() {
                        let mut cycles = 0u64;
                        // SAFETY: an open handle with query rights.
                        unsafe { QueryThreadCycleTime(s.handle, &mut cycles) };
                        let delta = if s.cycles == 0 { 0 } else { cycles - s.cycles };
                        s.cycles = cycles;
                        s.used += delta;
                        if delta < BUSY_CYCLES {
                            continue;
                        }
                        // SAFETY: suspends the thread just long enough to read its context;
                        // nothing between suspend and resume allocates or locks.
                        unsafe {
                            if SuspendThread(s.handle) == u32::MAX {
                                continue;
                            }
                            ctx.0.ContextFlags = CONTEXT_CONTROL_AMD64;
                            let ok = GetThreadContext(s.handle, &mut ctx.0) != 0;
                            // In a system call stub the return address is on top of the stack: it
                            // names who made the call. When that is a system library's wrapper,
                            // the first return address further up into other code (found by
                            // scanning, as there are no frame pointers) names its caller. Read
                            // while the thread is suspended.
                            let pc = ctx.0.Rip as usize;
                            let within = |ranges: &[(usize, usize)], a: usize| ranges.iter().any(|&(b, e)| a >= b && a < e);
                            let (mut caller, mut origin) = (0, 0);
                            if ok && within(&stubs, pc) {
                                let sp = ctx.0.Rsp as *const usize;
                                caller = *sp;
                                if within(&system, caller) {
                                    origin = (1..48).map(|i| *sp.add(i)).find(|&a| (crate::jit::code_cache::contains(a) || within(&modules, a)) && !within(&system, a)).unwrap_or(0);
                                }
                            }
                            ResumeThread(s.handle);
                            if ok {
                                pcs.push((s.id, pc, caller, origin));
                            }
                        }
                    }
                }
                for &key in &pcs {
                    *counts.entry(key).or_default() += 1;
                }
                if last_report.elapsed().as_secs() >= 10 {
                    last_report = std::time::Instant::now();
                    for s in THREADS.lock().unwrap().iter() {
                        names.entry(s.id).or_insert_with(|| name(s.handle));
                        used.insert(s.id, s.used);
                    }
                    let _ = std::fs::write(&path, report(&counts, &names, &used));
                    // Libraries a program loads later (a game's natives, its graphics driver).
                    modules = all_modules();
                }
            }
        });
    }

    /// The address range of a loaded module.
    fn range_of(m: windows_sys::Win32::Foundation::HMODULE) -> Option<(usize, usize)> {
        use windows_sys::Win32::System::ProcessStatus::{GetModuleInformation, MODULEINFO};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a query of a loaded module.
        unsafe {
            let mut info: MODULEINFO = std::mem::zeroed();
            (!m.is_null() && GetModuleInformation(GetCurrentProcess(), m, &mut info, std::mem::size_of::<MODULEINFO>() as u32) != 0).then(|| {
                let base = info.lpBaseOfDll as usize;
                (base, base + info.SizeOfImage as usize)
            })
        }
    }

    /// The address ranges of the named modules that are loaded.
    fn module_ranges(names: &[&str]) -> Vec<(usize, usize)> {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        names
            .iter()
            .filter_map(|name| {
                let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
                // SAFETY: a lookup of a loaded module, without a reference.
                range_of(unsafe { GetModuleHandleW(wide.as_ptr()) })
            })
            .collect()
    }

    /// The address ranges of every module loaded when sampling starts (later ones, a game's
    /// native libraries, are found per address by `os::module` in the report).
    fn all_modules() -> Vec<(usize, usize)> {
        use windows_sys::Win32::System::ProcessStatus::EnumProcessModules;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let mut mods = vec![std::ptr::null_mut(); 1024];
        let mut needed = 0u32;
        // SAFETY: fills a buffer of the size passed.
        if unsafe { EnumProcessModules(GetCurrentProcess(), mods.as_mut_ptr(), (mods.len() * 8) as u32, &mut needed) } == 0 {
            return Vec::new();
        }
        mods.truncate((needed as usize / 8).min(mods.len()));
        mods.into_iter().filter_map(range_of).collect()
    }

    /// Counts per thread and location, most frequent first.
    fn report(counts: &HashMap<(u32, usize, usize, usize), u64>, names: &HashMap<u32, String>, used: &HashMap<u32, u64>) -> String {
        let code = crate::jit::code_cache::names();
        let mut by: HashMap<(String, String), u64> = HashMap::new();
        let mut buf = [0u8; 128];
        let mut label_of = |pc: usize| match code.range(..=pc).next_back() {
            Some((&start, (size, name))) if pc < start + size => name.clone(),
            _ => match crate::os::module(pc, &mut buf) {
                Some((base, len)) => format!("{}+{:#x}", String::from_utf8_lossy(&buf[..len]), pc - base),
                None => format!("{pc:#x}"),
            },
        };
        for (&(id, pc, caller, origin), &n) in counts {
            let mut label = label_of(pc);
            if caller != 0 {
                label = format!("{label} <- {}", label_of(caller));
            }
            if origin != 0 {
                label = format!("{label} <~ {}", label_of(origin));
            }
            let thread = names.get(&id).cloned().unwrap_or_else(|| id.to_string());
            *by.entry((thread, label)).or_default() += n;
        }
        let mut rows: Vec<_> = by.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        let total: u64 = rows.iter().map(|r| r.1).sum();
        let mut out = format!("{total} samples\n");
        // CPU per thread name, in billions of cycles.
        let mut cpu: HashMap<String, u64> = HashMap::new();
        for (id, &c) in used {
            *cpu.entry(names.get(id).cloned().unwrap_or_else(|| id.to_string())).or_default() += c;
        }
        let mut cpu: Vec<_> = cpu.into_iter().collect();
        cpu.sort_by(|a, b| b.1.cmp(&a.1));
        for (thread, c) in cpu.into_iter().take(40) {
            out += &format!("cpu\t{:.2}\t{thread}\n", c as f64 / 1e9);
        }
        for ((thread, label), n) in rows.into_iter().take(3000) {
            out += &format!("{n}\t{thread}\t{label}\n");
        }
        out
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn attach_current() {}
    pub fn detach_current() {}
    pub fn start() {}
}

pub use imp::{attach_current, detach_current, start};
