//! Linux and macOS: `mmap`, signals, `dlopen` and pthreads, with the differences between the two
//! kept to the functions that differ.

use super::{CrashReport, FaultHandler, OwnCode};
use std::sync::OnceLock;

mod dns;
pub(crate) use dns::fallback_domain;
mod prefs;
pub(crate) use prefs::{prefs_chmod, prefs_lock, prefs_unlock};

#[cfg(target_os = "linux")]
const NORESERVE: libc::c_int = libc::MAP_NORESERVE;
#[cfg(not(target_os = "linux"))]
const NORESERVE: libc::c_int = 0;

fn mmap(at: usize, len: usize, prot: libc::c_int, flags: libc::c_int) -> Option<usize> {
    // SAFETY: an anonymous private mapping; callers pass MAP_FIXED only over their own ranges.
    let p = unsafe { libc::mmap(at as *mut libc::c_void, len, prot, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | NORESERVE | flags, -1, 0) };
    (p != libc::MAP_FAILED).then_some(p as usize)
}

/// Reserves `len` bytes, executable with `exec`; with `at`, exactly there or nothing (an
/// existing mapping is never replaced).
pub fn reserve(len: usize, at: Option<usize>, exec: bool) -> Option<usize> {
    let prot = libc::PROT_READ | libc::PROT_WRITE | if exec { libc::PROT_EXEC } else { 0 };
    match at {
        None => mmap(0, len, prot, 0),
        #[cfg(target_os = "linux")]
        Some(a) => mmap(a, len, prot, libc::MAP_FIXED_NOREPLACE),
        #[cfg(not(target_os = "linux"))]
        Some(a) => {
            // A hint the kernel honors only where the range is free.
            let p = mmap(a, len, prot, 0)?;
            if p != a {
                release(p, len);
                return None;
            }
            Some(p)
        }
    }
}

/// Reserves `len` bytes to `commit` piecewise (here every reservation is usable at once).
pub fn reserve_uncommitted(len: usize, at: Option<usize>) -> Option<usize> {
    reserve(len, at, false)
}

/// Makes a range of a `reserve_uncommitted` reservation usable.
#[inline]
pub fn commit(_p: usize, _len: usize) {}

/// Reserves `len` bytes below 2 GiB, or None.
pub fn reserve_low(len: usize) -> Option<usize> {
    #[cfg(target_os = "linux")]
    return mmap(0, len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_32BIT);
    #[cfg(not(target_os = "linux"))]
    {
        // Free ranges between 4 MiB and 2 GiB, tried in turn (the binary is linked with a small
        // `__PAGEZERO`, so this range is the process's own).
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(4 << 20);
        loop {
            let at = NEXT.fetch_add(len, std::sync::atomic::Ordering::Relaxed);
            if at + len > 2 << 30 {
                return None;
            }
            if let Some(p) = reserve(len, Some(at), false) {
                return Some(p);
            }
        }
    }
}

pub fn release(p: usize, len: usize) {
    // SAFETY: a mapping the caller made and no longer uses.
    unsafe { libc::munmap(p as *mut libc::c_void, len) };
}

/// Asks for transparent huge pages over the range (where the system has them).
pub fn huge_pages(p: usize, len: usize) {
    #[cfg(target_os = "linux")]
    // SAFETY: advice on the caller's mapping; failure leaves ordinary pages.
    unsafe {
        libc::madvise(p as *mut libc::c_void, len, libc::MADV_HUGEPAGE);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (p, len);
}

/// Returns the range's memory to the system; it reads as zeros afterwards.
pub fn discard(p: usize, len: usize) -> bool {
    #[cfg(target_os = "linux")]
    // SAFETY: private anonymous memory of the caller's, which DONTNEED leaves zero-filled.
    return unsafe { libc::madvise(p as *mut libc::c_void, len, libc::MADV_DONTNEED) } == 0;
    #[cfg(not(target_os = "linux"))]
    // Elsewhere DONTNEED keeps contents: a fresh mapping in place zeroes and frees the range.
    return mmap(p, len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_FIXED) == Some(p);
}

/// Faults the range in ahead of use; false where the system cannot.
pub fn populate(p: usize, len: usize) -> bool {
    #[cfg(target_os = "linux")]
    {
        const MADV_POPULATE_WRITE: libc::c_int = 23;
        // SAFETY: populating changes no contents.
        return unsafe { libc::madvise(p as *mut libc::c_void, len, MADV_POPULATE_WRITE) } == 0;
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (p, len);
        false
    }
}

/// Maps `len` bytes of `file` read-only.
pub fn map_file(file: &std::fs::File, len: usize) -> Option<usize> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: a read-only private mapping of an open file.
    let p = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0) };
    (p != libc::MAP_FAILED).then_some(p as usize)
}

pub fn unmap_file(p: usize, len: usize) {
    release(p, len);
}

/// Installed physical memory in bytes.
pub fn physical_memory() -> u64 {
    #[cfg(target_os = "macos")]
    {
        let mut v: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: sysctlbyname fills a u64.
        unsafe { libc::sysctlbyname(c"hw.memsize".as_ptr(), &mut v as *mut u64 as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) };
        v
    }
    #[cfg(not(target_os = "macos"))]
    // SAFETY: plain sysconf queries.
    unsafe {
        (libc::sysconf(libc::_SC_PHYS_PAGES) as u64) * (libc::sysconf(libc::_SC_PAGESIZE) as u64)
    }
}

/// Gives the C allocator's free memory back to the system, where it can.
pub fn trim_malloc() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: a plain call.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// The process's minor page faults so far.
pub fn minor_faults() -> i64 {
    // SAFETY: getrusage fills the struct.
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut u);
        u.ru_minflt as i64
    }
}

/// Total and free memory, total and free swap, in bytes (-1 where unknown).
pub fn memory_status() -> (i64, i64, i64, i64) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: sysinfo fills the struct.
        let mut si: libc::sysinfo = unsafe { std::mem::zeroed() };
        if unsafe { libc::sysinfo(&mut si) } != 0 {
            return (-1, -1, -1, -1);
        }
        let unit = si.mem_unit.max(1) as i64;
        (si.totalram as i64 * unit, si.freeram as i64 * unit, si.totalswap as i64 * unit, si.freeswap as i64 * unit)
    }
    #[cfg(target_os = "macos")]
    {
        let total = crate::os::physical_memory() as i64;
        // Free pages from the VM statistics; swap usage from vm.swapusage.
        let mut free = -1;
        // SAFETY: host_statistics64 fills a vm_statistics64 of the given count.
        unsafe {
            let mut stats: libc::vm_statistics64 = std::mem::zeroed();
            let mut count = (std::mem::size_of::<libc::vm_statistics64>() / std::mem::size_of::<libc::integer_t>()) as libc::mach_msg_type_number_t;
            #[allow(deprecated)]
            let host = libc::mach_host_self();
            if libc::host_statistics64(host, libc::HOST_VM_INFO64, &mut stats as *mut _ as libc::host_info64_t, &mut count) == 0 {
                free = (stats.free_count as i64 + stats.inactive_count as i64) * libc::sysconf(libc::_SC_PAGESIZE) as i64;
            }
        }
        let mut swap: libc::xsw_usage = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::xsw_usage>();
        // SAFETY: sysctlbyname fills an xsw_usage.
        let ok = unsafe { libc::sysctlbyname(c"vm.swapusage".as_ptr(), &mut swap as *mut _ as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) } == 0;
        let (st, sf) = if ok { (swap.xsu_total as i64, swap.xsu_avail as i64) } else { (-1, -1) };
        (total, free, st, sf)
    }
}

/// Names the calling thread (the first 15 bytes, the limit on Linux).
pub fn set_thread_name(name: &std::ffi::CStr) {
    // SAFETY: names the calling thread with a NUL-terminated string.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::pthread_setname_np(libc::pthread_self(), name.as_ptr());
    }
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_setname_np(name.as_ptr());
    }
}

/// CPU time the process has used, in nanoseconds (-1 if unknown).
pub fn process_cpu_nanos() -> i64 {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: fills a timespec.
    let ok = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) } == 0;
    if ok { ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64 } else { -1 }
}

/// Configured and online processors.
/// The system's CPU time so far, busy and total, in the kernel's ticks (None where it is not
/// read: macOS).
pub fn system_cpu_ticks() -> Option<(u64, u64)> {
    #[cfg(target_os = "linux")]
    {
        // The first line of /proc/stat: user nice system idle iowait irq softirq steal ...
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let v: Vec<u64> = stat.lines().next()?.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
        let total: u64 = v.iter().take(8).sum();
        let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
        Some((total - idle, total))
    }
    #[cfg(not(target_os = "linux"))]
    None
}

pub fn cpu_counts() -> (i32, i32) {
    // SAFETY: plain sysconf queries.
    unsafe { (libc::sysconf(libc::_SC_NPROCESSORS_CONF) as i32, libc::sysconf(libc::_SC_NPROCESSORS_ONLN) as i32) }
}

/// The lowest address and size of the calling thread's stack.
pub fn current_stack() -> (usize, usize) {
    #[cfg(target_os = "macos")]
    // SAFETY: queries about the calling thread; its stack grows down from stackaddr.
    unsafe {
        let me = libc::pthread_self();
        let top = libc::pthread_get_stackaddr_np(me) as usize;
        let size = libc::pthread_get_stacksize_np(me);
        (top - size, size)
    }
    #[cfg(not(target_os = "macos"))]
    // SAFETY: queries the attributes of the calling thread.
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        libc::pthread_getattr_np(libc::pthread_self(), &mut attr);
        let (mut addr, mut size) = (std::ptr::null_mut(), 0usize);
        libc::pthread_attr_getstack(&attr, &mut addr, &mut size);
        libc::pthread_attr_destroy(&mut attr);
        (addr as usize, size)
    }
}

/// Runs the calling thread at a lower priority (a nice value above 0).
pub fn lower_priority(nice: i32) {
    #[cfg(target_os = "linux")]
    // SAFETY: changes the calling thread's own priority.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, nice);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = nice;
}

#[cfg(target_os = "linux")]
const MEMBARRIER_PRIVATE_EXPEDITED: i32 = 8;
#[cfg(target_os = "linux")]
const MEMBARRIER_REGISTER_PRIVATE_EXPEDITED: i32 = 16;

/// Prepares `process_barrier`; false where the system has none.
pub fn register_process_barrier() -> bool {
    #[cfg(target_os = "linux")]
    // SAFETY: a membarrier command without pointers.
    return unsafe { libc::syscall(libc::SYS_membarrier, MEMBARRIER_REGISTER_PRIVATE_EXPEDITED, 0, 0) } == 0;
    #[cfg(not(target_os = "linux"))]
    return false;
}

/// Makes every thread of the process execute a full memory barrier.
pub fn process_barrier() {
    #[cfg(target_os = "linux")]
    // SAFETY: a registered membarrier command without pointers.
    unsafe {
        libc::syscall(libc::SYS_membarrier, MEMBARRIER_PRIVATE_EXPEDITED, 0, 0);
    }
}

/// Opens a shared library.
pub fn library_open(path: &str) -> Result<usize, String> {
    let c = std::ffi::CString::new(path).map_err(|e| e.to_string())?;
    // SAFETY: dlopen of a NUL-terminated path; dlerror describes a failure.
    unsafe {
        let h = libc::dlopen(c.as_ptr(), libc::RTLD_LAZY) as usize;
        if h == 0 {
            return Err(std::ffi::CStr::from_ptr(libc::dlerror()).to_string_lossy().into_owned());
        }
        Ok(h)
    }
}

/// The address of `name` in library `h`, or 0.
pub fn library_symbol(h: usize, name: &str) -> usize {
    let Ok(c) = std::ffi::CString::new(name) else { return 0 };
    // SAFETY: a handle from `library_open`.
    unsafe { libc::dlsym(h as *mut libc::c_void, c.as_ptr()) as usize }
}

/// The base address of the binary or library holding `pc`, and the length of its file name
/// (without directories) as copied into `name` (cut at its length).
pub fn module(pc: usize, name: &mut [u8]) -> Option<(usize, usize)> {
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: dladdr reads the loader's tables; the name it gives lives as long as the module.
    if unsafe { libc::dladdr(pc as *const libc::c_void, &mut info) } == 0 || info.dli_fbase.is_null() {
        return None;
    }
    let path = if info.dli_fname.is_null() { &[][..] } else { unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }.to_bytes() };
    let file = path.rsplit(|&b| b == b'/').next().unwrap_or_default();
    let n = file.len().min(name.len());
    name[..n].copy_from_slice(&file[..n]);
    Some((info.dli_fbase as usize, n))
}

/// Return addresses of the calling thread's native frames, innermost first; how many fit.
pub fn native_frames(out: &mut [usize]) -> usize {
    // SAFETY: backtrace(3) into a buffer of the length passed.
    unsafe { libc::backtrace(out.as_mut_ptr() as *mut *mut libc::c_void, out.len() as libc::c_int).max(0) as usize }
}

/// Writes `bytes` to stderr without allocating (usable in a fault handler).
pub fn write_stderr(bytes: &[u8]) {
    // SAFETY: write(2) of a borrowed buffer.
    unsafe { libc::write(2, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
}

/// Creates (or empties) the crash report file `name` (NUL-terminated, in the working directory)
/// without allocating; None when it cannot be created.
pub fn create_report(name: &[u8]) -> Option<usize> {
    // SAFETY: open(2) of a NUL-terminated name; the descriptor is closed by `close_report`.
    let fd = unsafe { libc::open(name.as_ptr() as *const libc::c_char, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC, 0o644) };
    (fd >= 0).then_some(fd as usize)
}

/// Appends `bytes` to a file from `create_report`.
pub fn write_report(f: usize, bytes: &[u8]) {
    // SAFETY: write(2) to an open descriptor from `create_report`, a borrowed buffer.
    unsafe { libc::write(f as libc::c_int, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
}

/// Closes a file from `create_report`.
pub fn close_report(f: usize) {
    // SAFETY: close(2) of a descriptor from `create_report`, closed once.
    unsafe { libc::close(f as libc::c_int) };
}

static HANDLER: OnceLock<FaultHandler> = OnceLock::new();
static CRASH: OnceLock<CrashReport> = OnceLock::new();
static PREVIOUS: OnceLock<Vec<(libc::c_int, libc::sigaction)>> = OnceLock::new();

/// The signals an access to unmapped memory raises.
#[cfg(target_os = "macos")]
const FAULTS: [libc::c_int; 2] = [libc::SIGSEGV, libc::SIGBUS];
#[cfg(not(target_os = "macos"))]
const FAULTS: [libc::c_int; 1] = [libc::SIGSEGV];

/// Sends memory faults to `handler` first: it returns where to resume, or None to report the
/// fault with `crash` and pass it on to the handler installed before (the Rust runtime's
/// stack-overflow report). Every fault the handler leaves is reported, so `_own` is not needed.
pub fn install_fault_handler(handler: FaultHandler, crash: CrashReport, _own: OwnCode) {
    if HANDLER.set(handler).is_err() {
        return;
    }
    let _ = CRASH.set(crash);
    let mut previous = Vec::new();
    // SAFETY: plain sigaction calls; the previous actions are kept for chaining.
    unsafe {
        for sig in FAULTS {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_fault as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_NODEFER;
            libc::sigemptyset(&mut action.sa_mask);
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, &action, &mut old) == 0 {
                previous.push((sig, old));
            }
        }
    }
    let _ = PREVIOUS.set(previous);
}

/// The interrupted instruction pointer in a signal's context.
unsafe fn pc_of(ctx: *mut libc::c_void) -> *mut u64 {
    let uc = ctx as *mut libc::ucontext_t;
    #[cfg(target_os = "linux")]
    return &mut (*uc).uc_mcontext.gregs[libc::REG_RIP as usize] as *mut i64 as *mut u64;
    #[cfg(target_os = "macos")]
    return &mut (*(*uc).uc_mcontext).__ss.__rip;
}

extern "C" fn on_fault(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    // SAFETY: the kernel passes a valid siginfo and context to an SA_SIGINFO handler.
    unsafe {
        let pc = pc_of(ctx);
        if let Some(resume) = HANDLER.get().and_then(|h| h(*pc as usize, (*info).si_addr() as usize)) {
            *pc = resume as u64;
            return;
        }
        // Once: a fault inside the report itself (walking a broken thread's frames) goes
        // straight on to the previous handler.
        static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if let Some(crash) = CRASH.get().filter(|_| !REPORTED.swap(true, std::sync::atomic::Ordering::SeqCst)) {
            crash(*pc as usize, (*info).si_addr() as usize);
        }
        let previous = PREVIOUS.get().and_then(|p| p.iter().find(|(s, _)| *s == sig)).map(|(_, a)| *a);
        match previous {
            Some(p) if p.sa_sigaction != libc::SIG_DFL && p.sa_sigaction != libc::SIG_IGN => {
                if p.sa_flags & libc::SA_SIGINFO != 0 {
                    let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) = std::mem::transmute(p.sa_sigaction);
                    f(sig, info, ctx);
                } else {
                    let f: extern "C" fn(libc::c_int) = std::mem::transmute(p.sa_sigaction);
                    f(sig);
                }
            }
            // The default action: the access runs again and the process dies of it.
            _ => {
                let mut dfl: libc::sigaction = std::mem::zeroed();
                dfl.sa_sigaction = libc::SIG_DFL;
                libc::sigaction(sig, &dfl, std::ptr::null_mut());
            }
        }
    }
}

/// The write end of the pipe `SIGQUIT` wakes the dump thread through.
static QUIT_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn on_quit(_: libc::c_int) {
    let fd = QUIT_PIPE.load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: write(2) is async-signal-safe; a full pipe drops the request.
    unsafe { libc::write(fd, b"q".as_ptr() as *const libc::c_void, 1) };
}

/// Runs `dump` on a service thread each time the process gets `SIGQUIT` (`kill -3`, Ctrl+\),
/// as HotSpot prints its thread dump.
pub fn on_dump_request(dump: fn()) {
    let mut fds = [0; 2];
    // SAFETY: a new pipe; its write end is non-blocking for the handler.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return;
        }
        libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK);
    }
    QUIT_PIPE.store(fds[1], std::sync::atomic::Ordering::Relaxed);
    let read = fds[0];
    let spawned = std::thread::Builder::new().name("Signal Dispatcher".into()).spawn(move || loop {
        let mut b = 0u8;
        // SAFETY: reads one byte into `b`.
        match unsafe { libc::read(read, &mut b as *mut u8 as *mut libc::c_void, 1) } {
            1 => dump(),
            -1 if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
            _ => return,
        }
    });
    if spawned.is_ok() {
        // SAFETY: a plain handler for SIGQUIT.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_quit as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(libc::SIGQUIT, &action, std::ptr::null_mut());
        }
    }
}
