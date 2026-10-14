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
            let mut counts: HashMap<(u32, usize), u64> = HashMap::new();
            let mut names: HashMap<u32, String> = HashMap::new();
            let mut used: HashMap<u32, u64> = HashMap::new();
            let mut last_report = std::time::Instant::now();
            let mut ctx: Box<Ctx> = Box::new(unsafe { std::mem::zeroed() });
            let mut pcs: Vec<(u32, usize)> = Vec::with_capacity(512);
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
                            ResumeThread(s.handle);
                            if ok {
                                pcs.push((s.id, ctx.0.Rip as usize));
                            }
                        }
                    }
                }
                for &(id, pc) in &pcs {
                    *counts.entry((id, pc)).or_default() += 1;
                }
                if last_report.elapsed().as_secs() >= 10 {
                    last_report = std::time::Instant::now();
                    for s in THREADS.lock().unwrap().iter() {
                        names.entry(s.id).or_insert_with(|| name(s.handle));
                        used.insert(s.id, s.used);
                    }
                    let _ = std::fs::write(&path, report(&counts, &names, &used));
                }
            }
        });
    }

    /// Counts per thread and location, most frequent first.
    fn report(counts: &HashMap<(u32, usize), u64>, names: &HashMap<u32, String>, used: &HashMap<u32, u64>) -> String {
        let code = crate::jit::code_cache::names();
        let mut by: HashMap<(String, String), u64> = HashMap::new();
        let mut buf = [0u8; 128];
        for (&(id, pc), &n) in counts {
            let label = match code.range(..=pc).next_back() {
                Some((&start, (size, name))) if pc < start + size => name.clone(),
                _ => match crate::os::module(pc, &mut buf) {
                    Some((base, len)) => format!("{}+{:#x}", String::from_utf8_lossy(&buf[..len]), pc - base),
                    None => format!("{pc:#x}"),
                },
            };
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
