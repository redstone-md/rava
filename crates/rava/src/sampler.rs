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
        /// The `Thread` (its frame headers, for RAVA_SAMPLE_STACK).
        thread: usize,
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

    pub fn attach_current(thread: usize) {
        if !ON.load(Relaxed) {
            return;
        }
        // SAFETY: plain queries and an open of the calling thread.
        unsafe {
            let id = GetCurrentThreadId();
            let handle = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_LIMITED_INFORMATION, 0, id);
            if !handle.is_null() {
                THREADS.lock().unwrap().push(Sampled { id, handle, thread, cycles: 0, used: 0 });
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
            // RAVA_SAMPLE_WINDOW: each ten seconds' samples also on their own (`<file>.w<secs>`),
            // to see a steady state apart from loading.
            let windowed = std::env::var_os("RAVA_SAMPLE_WINDOW").is_some();
            let mut window: HashMap<(u32, usize, usize, usize), u64> = HashMap::new();
            let stubs = module_ranges(&["ntdll.dll", "win32u.dll"]);
            let system = module_ranges(&["ntdll.dll", "win32u.dll", "KERNELBASE.dll", "kernel32.dll", "user32.dll", "gdi32.dll", "gdi32full.dll"]);
            let mut modules = all_modules();
            let mut names: HashMap<u32, String> = HashMap::new();
            let mut used: HashMap<u32, u64> = HashMap::new();
            let mut last_report = std::time::Instant::now();
            let started = last_report;
            let mut last_used: HashMap<u32, u64> = HashMap::new();
            let mut last_process = process_cpu();
            let mut last_compiles = 0;
            let mut last_calls = 0;
            let mut ctx: Box<Ctx> = Box::new(unsafe { std::mem::zeroed() });
            let mut pcs: Vec<(u32, usize, usize, usize)> = Vec::with_capacity(512);
            // RAVA_SAMPLE_STACK: also count each sample's Java stack from the frame headers,
            // inlined frames at call sites included (`<file>.stacks.w<secs>`, folded).
            let stacks = std::env::var_os("RAVA_SAMPLE_STACK").is_some();
            let mut stacked = Stacks::default();
            let mut words: Vec<usize> = Vec::new();
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
                            } else if ok && !crate::jit::code_cache::contains(pc) {
                                // In native code (the VM's own, the C runtime's): the first
                                // return address into compiled code further up names the Java
                                // method that called into it (scanned within the stack's
                                // committed region).
                                let sp = ctx.0.Rsp as usize;
                                let words = committed_above(sp).min(4096) / 8;
                                origin = (1..words).map(|i| *(sp as *const usize).add(i)).find(|&a| crate::jit::code_cache::contains(a)).unwrap_or(0);
                            }
                            if ok && stacks && s.thread != 0 {
                                words.clear();
                                words.push(pc);
                                walk_headers(s.thread, &mut words);
                            }
                            ResumeThread(s.handle);
                            if ok {
                                pcs.push((s.id, pc, caller, origin));
                                if stacks {
                                    stacked.add(s.id, &words);
                                }
                            }
                        }
                    }
                }
                for &key in &pcs {
                    *counts.entry(key).or_default() += 1;
                    if windowed {
                        *window.entry(key).or_default() += 1;
                    }
                }
                if last_report.elapsed().as_secs() >= 10 {
                    last_report = std::time::Instant::now();
                    for s in THREADS.lock().unwrap().iter() {
                        names.entry(s.id).or_insert_with(|| name(s.handle));
                        used.insert(s.id, s.used);
                    }
                    let _ = std::fs::write(&path, report(&counts, &names, &used));
                    if stacks {
                        // Each ten seconds' stacks on their own (all of a run's would not fit).
                        let _ = std::fs::write(format!("{}.stacks.w{}", path.to_string_lossy(), started.elapsed().as_secs()), stacked.report(&names));
                        stacked.refresh();
                    }
                    if windowed {
                        let _ = std::fs::write(format!("{}.w{}", path.to_string_lossy(), started.elapsed().as_secs()), report(&window, &names, &used));
                        window.clear();
                    }
                    // The CPU each thread used in this report's ten seconds (`<file>.cpu`), to
                    // tell a steady state from loading.
                    let process = process_cpu();
                    let compiles = crate::jit::OPT_COMPILES.load(Relaxed);
                    let mut line = format!("{}	process={:.2}	opt-compiles={}", started.elapsed().as_secs(), process - last_process, compiles - last_compiles);
                    last_compiles = compiles;
                    if let Some(c) = crate::jit::call_counter() {
                        let n = c.load(Relaxed);
                        line += &format!("	calls={}", n - last_calls);
                        last_calls = n;
                    }
                    last_process = process;
                    for (id, &u) in &used {
                        let d = u - last_used.get(id).copied().unwrap_or(0);
                        if d > 0 {
                            line += &format!("	{}={:.2}", names.get(id).map_or("?", |s| s.as_str()), d as f64 / 3.0e9);
                        }
                    }
                    last_used = used.clone();
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{}.cpu", path.to_string_lossy())) {
                        let _ = writeln!(f, "{line}");
                    }
                    // Libraries a program loads later (a game's natives, its graphics driver).
                    modules = all_modules();
                }
            }
        });
    }

    /// Stack counts of the last ten seconds: per thread and folded stack (Java frames from the
    /// headers, inlined ones at call sites included; the pc's compiled method named by the code
    /// cache as of the last report, `?` for code compiled since).
    #[derive(Default)]
    struct Stacks {
        code: std::collections::BTreeMap<usize, (usize, String)>,
        window: HashMap<(u32, String), u64>,
        seen: Vec<String>,
        /// Each loaded class's methods: (address, count), sorted.
        methods: Vec<(usize, usize)>,
    }

    // `InlinedSite` is read as the (pointer, length) of its frames, each a (method, bci) pair.
    const _: () = assert!(std::mem::size_of::<crate::thread::InlinedSite>() == 16 && std::mem::size_of::<(&crate::method::Method, i32)>() == 16);

    /// Copies words at `at` without faulting on unmapped memory.
    fn copy(at: usize, out: &mut [usize]) -> bool {
        use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let mut got = 0usize;
        // SAFETY: the system checks the source range; the destination is `out`.
        unsafe { ReadProcessMemory(GetCurrentProcess(), at as *const _, out.as_mut_ptr() as *mut _, out.len() * 8, &mut got) != 0 && got == out.len() * 8 }
    }

    impl Stacks {
        /// `words`: the pc, then `walk_headers`' pairs.
        fn add(&mut self, thread: u32, words: &[usize]) {
            use crate::thread::header;
            self.seen.clear();
            let pc = words[0];
            let leaf = match self.code.range(..=pc).next_back() {
                Some((&start, (size, name))) if pc < start + size => name.clone(),
                _ => "?".into(),
            };
            self.seen.push(leaf);
            let push = |seen: &mut Vec<String>, m: &crate::method::Method| seen.push(format!("{m}"));
            // `top` stays at the last header published: callees that returned since (the pc's
            // method called them) are dead. Their chain leads back to the pc's own frame.
            let key = self.seen[0].split(' ').next().unwrap_or("").to_string();
            let pairs = &words[1..];
            let live = pairs.chunks_exact(2).take(8).position(|p| self.is_method(p[0]) && format!("{}", unsafe { &*(p[0] as *const crate::method::Method) }) == key);
            // The pc's own frame is not in a call: its BCI slot names the last call it made.
            for (i, pair) in pairs[2 * live.unwrap_or(0)..].chunks_exact(2).enumerate() {
                let (m, bci) = (pair[0], pair[1] as u64);
                if m == 0 || m % 8 != 0 {
                    continue;
                }
                // A header past `top`'s live frames holds stale words: only methods of loaded
                // classes are read, and site pointers through fault-safe reads.
                if !self.is_method(m) {
                    continue;
                }
                // SAFETY: checked to be a method; the site's words are copied, not dereferenced.
                unsafe {
                    let mut inlined = false;
                    if !(i == 0 && live.is_some()) && (header::SITE_MIN..header::SITE_END).contains(&bci) && bci % 8 == 0 {
                        let mut fat = [0usize; 2];
                        if copy(bci as usize, &mut fat) && fat[1] > 0 && fat[1] < 256 {
                            let mut frames = vec![0usize; 2 * fat[1]];
                            if copy(fat[0], &mut frames) && frames.chunks_exact(2).all(|f| self.is_method(f[0])) {
                                for f in frames.chunks_exact(2) {
                                    push(&mut self.seen, &*(f[0] as *const crate::method::Method));
                                }
                                inlined = true;
                            }
                        }
                    }
                    // The pc's own frame is the leaf already.
                    if !inlined && !(i == 0 && live.is_some()) {
                        push(&mut self.seen, &*(m as *const crate::method::Method));
                    }
                }
            }
            // Folded, outermost first (a flame graph's input).
            let mut folded = String::new();
            for (i, name) in self.seen.iter().rev().enumerate() {
                if i > 0 {
                    folded.push(';');
                }
                folded += name;
            }
            *self.window.entry((thread, folded)).or_default() += 1;
        }

        fn refresh(&mut self) {
            self.code = crate::jit::code_cache::names();
            self.methods = crate::vm::vm().all_classes.lock().unwrap().iter().map(|k| k.methods()).filter(|m| !m.is_empty()).map(|m| (m.as_ptr() as usize, m.len())).collect();
            self.methods.sort_unstable();
            self.window.clear();
        }

        /// Whether `m` is a method of a class loaded by the last report.
        fn is_method(&self, m: usize) -> bool {
            let size = std::mem::size_of::<crate::method::Method>();
            let i = self.methods.partition_point(|&(start, _)| start <= m);
            i > 0 && {
                let (start, n) = self.methods[i - 1];
                m < start + n * size && (m - start) % size == 0
            }
        }

        fn report(&self, names: &HashMap<u32, String>) -> String {
            let mut rows: Vec<_> = self.window.iter().collect();
            rows.sort_by(|a, b| b.1.cmp(a.1));
            let mut out = String::new();
            for ((id, name), n) in rows.into_iter() {
                out += &format!("{n}\t{}\t{name}\n", names.get(id).map_or("?", |s| s.as_str()));
            }
            out
        }
    }

    /// Appends, for each frame header from `thread`'s `top` outwards, its method and BCI slot
    /// (a pair of words). Called while the thread is suspended; a header whose caller link
    /// does not lead outwards (the value stack grows up) ends the walk.
    unsafe fn walk_headers(thread: usize, out: &mut Vec<usize>) {
        use crate::thread::{header, offsets};
        let stack = (*(thread as *const crate::thread::Thread)).value_stack();
        let mut h = *((thread + offsets::TOP as usize) as *const usize);
        for _ in 0..4096 {
            if h % 8 != 0 || !stack.contains(&h) || !stack.contains(&(h + 8 * header::SLOTS)) {
                return;
            }
            let p = h as *const usize;
            out.push(*p.add(header::METHOD));
            out.push(*p.add(header::BCI));
            let c = *p.add(header::CALLER);
            if c >= h || h - c > 64 << 20 {
                return;
            }
            h = c;
        }
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

    /// Seconds of CPU the whole process has used (user and kernel).
    fn process_cpu() -> f64 {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let z = || FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut c, mut e, mut k, mut u) = (z(), z(), z(), z());
        // SAFETY: a query of this process into locals.
        unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
        t(k) + t(u)
    }

    /// Bytes from `at` to the end of the committed memory region holding it (a stack's).
    fn committed_above(at: usize) -> usize {
        use windows_sys::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT};
        // SAFETY: a query of an address in this process.
        unsafe {
            let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            if VirtualQuery(at as *const _, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) == 0 || info.State != MEM_COMMIT {
                return 0;
            }
            info.BaseAddress as usize + info.RegionSize - at
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
        // RAVA_SAMPLE_DETAIL=<text>: compiled methods whose name contains it are counted per
        // instruction offset (for RAVA_JIT_DUMP's code), to find the instructions that take the time.
        let detail = std::env::var("RAVA_SAMPLE_DETAIL").ok();
        let mut label_of = |pc: usize| match code.range(..=pc).next_back() {
            Some((&start, (size, name))) if pc < start + size => match &detail {
                // With the instruction's bytes (to classify instructions without a dump).
                Some(d) if name.contains(d.as_str()) => {
                    let mut w = [0usize; 2];
                    let bytes = if copy(pc, &mut w) { w.iter().flat_map(|x| x.to_le_bytes()).map(|b| format!("{b:02x}")).collect::<String>() } else { String::new() };
                    format!("{name} @{start:#x}+{:#x} #{bytes}", pc - start)
                }
                _ => name.clone(),
            },
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
        for ((thread, label), n) in rows.into_iter().take(if detail.is_some() { 1 << 20 } else { 3000 }) {
            out += &format!("{n}\t{thread}\t{label}\n");
        }
        out
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn attach_current(_: usize) {}
    pub fn detach_current() {}
    pub fn start() {}
}

pub use imp::{attach_current, detach_current, start};
