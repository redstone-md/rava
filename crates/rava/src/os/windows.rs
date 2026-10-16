//! Windows: `VirtualAlloc`, a vectored exception handler, `LoadLibraryW` and the thread and
//! process queries. Memory is reserved and committed together: Windows charges committed memory
//! against its commit limit but still gives physical pages only to the ones touched.

use super::{CrashReport, FaultHandler, OwnCode};
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, HANDLE, HMODULE};
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, RtlCaptureStackBackTrace, SetUnhandledExceptionFilter, EXCEPTION_POINTERS};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleExW, GetProcAddress, LoadLibraryW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, VirtualAlloc, VirtualFree, FILE_MAP_READ, MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    PAGE_READONLY, PAGE_READWRITE,
};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{FlushProcessWriteBuffers, GetCurrentProcess, GetCurrentThread, GetCurrentThreadStackLimits, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};

/// `EXCEPTION_CONTINUE_EXECUTION` and `EXCEPTION_CONTINUE_SEARCH`.
const CONTINUE_EXECUTION: i32 = -1;
const CONTINUE_SEARCH: i32 = 0;

fn alloc(at: usize, len: usize, exec: bool) -> Option<usize> {
    let prot = if exec { PAGE_EXECUTE_READWRITE } else { PAGE_READWRITE };
    // SAFETY: a fresh allocation, at `at` only where that range is free.
    let p = unsafe { VirtualAlloc(at as *const _, len, MEM_RESERVE | MEM_COMMIT, prot) } as usize;
    (p != 0).then_some(p)
}

/// Reserves `len` bytes, executable with `exec`; with `at`, exactly there or nothing.
pub fn reserve(len: usize, at: Option<usize>, exec: bool) -> Option<usize> {
    match at {
        None => alloc(0, len, exec),
        Some(a) => alloc(a, len, exec).filter(|&p| p == a),
    }
}

/// Reserves `len` bytes to `commit` piecewise.
pub fn reserve_uncommitted(len: usize, at: Option<usize>) -> Option<usize> {
    // SAFETY: a fresh reservation, at `at` only where that range is free.
    let p = unsafe { VirtualAlloc(at.unwrap_or(0) as *const _, len, MEM_RESERVE, PAGE_READWRITE) } as usize;
    match at {
        _ if p == 0 => None,
        Some(a) if p != a => {
            release(p, len);
            None
        }
        _ => Some(p),
    }
}

/// Makes a range of a `reserve_uncommitted` reservation usable.
pub fn commit(p: usize, len: usize) {
    // SAFETY: commits pages of the caller's reservation (committed pages stay as they are).
    let ok = unsafe { !VirtualAlloc(p as *const _, len, MEM_COMMIT, PAGE_READWRITE).is_null() };
    assert!(ok, "cannot commit {len} bytes of heap: {}", std::io::Error::last_os_error());
}

/// Ranges committed as they are reached, a megabyte at a time: the value stacks and the code
/// cache. Committed whole, a thread's 64 MiB value stack charged the system's commit limit for
/// memory it almost never touches (16 GiB for the 260 threads of a large modded client), and
/// the code cache a gigabyte.
static GROWING: std::sync::RwLock<Vec<(usize, usize, bool)>> = std::sync::RwLock::new(Vec::new());
const GROW_STEP: usize = 1 << 20;

/// Reserves `len` bytes (executable with `exec`; with `at`, exactly there or nothing), committed
/// as they are first touched.
pub fn reserve_growing(len: usize, at: Option<usize>, exec: bool) -> Option<usize> {
    static GROWER: OnceLock<()> = OnceLock::new();
    GROWER.get_or_init(|| {
        // SAFETY: registers a handler that lives as long as the process.
        unsafe { AddVectoredExceptionHandler(1, Some(on_growing_fault)) };
    });
    let p = reserve_uncommitted(len, at)?;
    GROWING.write().unwrap().push((p, len, exec));
    grow(p, GROW_STEP.min(len), exec);
    Some(p)
}

/// Releases a range from `reserve_growing`.
pub fn release_growing(p: usize, len: usize) {
    GROWING.write().unwrap().retain(|&(b, _, _)| b != p);
    release(p, len);
}

fn grow(p: usize, len: usize, exec: bool) -> bool {
    let prot = if exec { PAGE_EXECUTE_READWRITE } else { PAGE_READWRITE };
    // SAFETY: commits pages of a reservation this module made.
    unsafe { !VirtualAlloc(p as *const _, len, MEM_COMMIT, prot).is_null() }
}

/// Commits the part of a growing range an access just reached, and runs the access again.
unsafe extern "system" fn on_growing_fault(info: *mut EXCEPTION_POINTERS) -> i32 {
    let record = &*(*info).ExceptionRecord;
    if record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION {
        return CONTINUE_SEARCH;
    }
    let addr = record.ExceptionInformation[1];
    let Ok(ranges) = GROWING.try_read() else { return CONTINUE_SEARCH };
    let Some(&(base, len, exec)) = ranges.iter().find(|&&(b, l, _)| addr >= b && addr < b + l) else { return CONTINUE_SEARCH };
    let start = base + (addr - base) / GROW_STEP * GROW_STEP;
    if grow(start, GROW_STEP.min(base + len - start), exec) { CONTINUE_EXECUTION } else { CONTINUE_SEARCH }
}

/// Reserves `len` bytes below 2 GiB, or None.
pub fn reserve_low(len: usize) -> Option<usize> {
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

pub fn release(p: usize, _len: usize) {
    // SAFETY: an allocation the caller made and no longer uses.
    unsafe { VirtualFree(p as *mut _, 0, MEM_RELEASE) };
}

pub fn huge_pages(_p: usize, _len: usize) {}

/// Returns the range's memory to the system, commit charge included: it must be `commit`ted
/// again before use, and reads as zeros then. (Committed again at once, a discarded range kept
/// its charge: an All the Mods 10 client's empty heap pages and freed large blocks held
/// gigabytes of commit that nothing used.)
pub fn discard(p: usize, len: usize) -> bool {
    // SAFETY: decommits the caller's pages, which nothing reads until they are committed again.
    unsafe { VirtualFree(p as *mut _, len, MEM_DECOMMIT) != 0 }
}

pub fn populate(_p: usize, _len: usize) -> bool {
    false
}

/// Maps `len` bytes of `file` read-only.
pub fn map_file(file: &std::fs::File, len: usize) -> Option<usize> {
    use std::os::windows::io::AsRawHandle;
    // SAFETY: a read-only mapping of an open file; the mapping object closes with the view.
    unsafe {
        let m = CreateFileMappingW(file.as_raw_handle() as HANDLE, std::ptr::null(), PAGE_READONLY, 0, 0, std::ptr::null());
        if m.is_null() {
            return None;
        }
        let view = MapViewOfFile(m, FILE_MAP_READ, 0, 0, len);
        windows_sys::Win32::Foundation::CloseHandle(m);
        (!view.Value.is_null()).then_some(view.Value as usize)
    }
}

pub fn unmap_file(p: usize, _len: usize) {
    // SAFETY: a view `map_file` made.
    unsafe { UnmapViewOfFile(windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS { Value: p as *mut _ }) };
}

/// Installed physical memory in bytes.
pub fn physical_memory() -> u64 {
    // SAFETY: GlobalMemoryStatusEx fills a struct whose length is set.
    unsafe {
        let mut m: MEMORYSTATUSEX = std::mem::zeroed();
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut m) == 0 {
            return 0;
        }
        m.ullTotalPhys
    }
}

pub fn trim_malloc() {}

/// The process's page faults so far (Windows does not split minor from major).
pub fn minor_faults() -> i64 {
    // SAFETY: fills a counters struct whose size is passed.
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        GetProcessMemoryInfo(GetCurrentProcess(), &mut c, std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32);
        c.PageFaultCount as i64
    }
}

/// Total and free memory, total and free swap (the page file), in bytes (-1 where unknown).
pub fn memory_status() -> (i64, i64, i64, i64) {
    // SAFETY: GlobalMemoryStatusEx fills a struct whose length is set.
    unsafe {
        let mut m: MEMORYSTATUSEX = std::mem::zeroed();
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut m) == 0 {
            return (-1, -1, -1, -1);
        }
        (m.ullTotalPhys as i64, m.ullAvailPhys as i64, m.ullTotalPageFile as i64, m.ullAvailPageFile as i64)
    }
}

/// Names the calling thread.
/// While held, the system timer ticks every millisecond (HotSpot's `HighResolutionInterval`):
/// a timed wait of a period that is not a multiple of 10 ms would otherwise wake on the
/// default 15.6 ms tick.
pub struct HighResolution(bool);

impl HighResolution {
    pub fn for_millis(ms: u64) -> HighResolution {
        let on = ms % 10 != 0;
        if on {
            // SAFETY: paired with timeEndPeriod in drop.
            unsafe { windows_sys::Win32::Media::timeBeginPeriod(1) };
        }
        HighResolution(on)
    }
}

impl Drop for HighResolution {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: ends the period begun in `for_millis`.
            unsafe { windows_sys::Win32::Media::timeEndPeriod(1) };
        }
    }
}

/// A handle of the calling thread usable from other threads (kept for the thread's life).
pub fn current_thread_handle() -> usize {
    use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
    let mut h: HANDLE = std::ptr::null_mut();
    // SAFETY: duplicates the pseudo-handle of the calling thread into a real one.
    unsafe { DuplicateHandle(GetCurrentProcess(), GetCurrentThread(), GetCurrentProcess(), &mut h, 0, 0, DUPLICATE_SAME_ACCESS) };
    h as usize
}

/// Sets the scheduling priority of thread `handle` (0: the calling thread) for Java priority
/// `p` (1..=10), as HotSpot maps them on Windows: 1-2 lowest, 3-4 below normal, 5-6 normal,
/// 7-8 above normal, 9-10 highest.
pub fn set_java_priority(handle: usize, p: i32) {
    use windows_sys::Win32::System::Threading::{THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_LOWEST, THREAD_PRIORITY_NORMAL};
    let prio = match p {
        ..=2 => THREAD_PRIORITY_LOWEST,
        3 | 4 => THREAD_PRIORITY_BELOW_NORMAL,
        5 | 6 => THREAD_PRIORITY_NORMAL,
        7 | 8 => THREAD_PRIORITY_ABOVE_NORMAL,
        _ => THREAD_PRIORITY_HIGHEST,
    };
    // SAFETY: a thread handle of this process, else the calling thread's pseudo-handle.
    unsafe { SetThreadPriority(if handle == 0 { GetCurrentThread() } else { handle as HANDLE }, prio) };
}

pub fn set_thread_name(name: &std::ffi::CStr) {
    let w: Vec<u16> = name.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: names the calling thread with a NUL-terminated wide string.
    unsafe { windows_sys::Win32::System::Threading::SetThreadDescription(GetCurrentThread(), w.as_ptr()) };
}

/// CPU time the process has used, in nanoseconds (-1 if unknown).
pub fn process_cpu_nanos() -> i64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    // SAFETY: fills four FILETIMEs of the current process.
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) } == 0 {
        return -1;
    }
    let ticks = |f: FILETIME| ((f.dwHighDateTime as i64) << 32) | f.dwLowDateTime as i64;
    (ticks(k) + ticks(u)) * 100
}

/// Configured and online processors.
/// The system's CPU time so far, busy and total, in 100 ns units (`GetSystemTimes`; kernel
/// time includes idle time).
pub fn system_cpu_ticks() -> Option<(u64, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    let ft = |f: FILETIME| (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64;
    let (mut idle, mut kernel, mut user) = (FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }, FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }, FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 });
    // SAFETY: three FILETIME out-parameters.
    if unsafe { windows_sys::Win32::System::Threading::GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
        return None;
    }
    let total = ft(kernel) + ft(user);
    Some((total - ft(idle), total))
}

pub fn cpu_counts() -> (i32, i32) {
    let n = std::thread::available_parallelism().map_or(1, |n| n.get()) as i32;
    (n, n)
}

/// The lowest address and size of the calling thread's stack.
pub fn current_stack() -> (usize, usize) {
    let (mut low, mut high) = (0usize, 0usize);
    // SAFETY: queries the calling thread.
    unsafe { GetCurrentThreadStackLimits(&mut low, &mut high) };
    (low, high - low)
}

pub fn lower_priority(_nice: i32) {
    // SAFETY: changes the calling thread's own priority.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
}

pub fn register_process_barrier() -> bool {
    true
}

/// Makes every thread of the process execute a full memory barrier.
pub fn process_barrier() {
    // SAFETY: a plain call.
    unsafe { FlushProcessWriteBuffers() };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Opens a shared library.
pub fn library_open(path: &str) -> Result<usize, String> {
    let w = wide(path);
    // SAFETY: a NUL-terminated wide path.
    let h = unsafe { LoadLibraryW(w.as_ptr()) } as usize;
    if h == 0 {
        return Err(format!("{path}: {}", std::io::Error::last_os_error()));
    }
    Ok(h)
}

/// The address of `name` in library `h`, or 0.
pub fn library_symbol(h: usize, name: &str) -> usize {
    let Ok(c) = std::ffi::CString::new(name) else { return 0 };
    // SAFETY: a module handle from `library_open`.
    unsafe { GetProcAddress(h as HMODULE, c.as_ptr() as *const u8) }.map_or(0, |f| f as usize)
}

/// The base address of the binary or library holding `pc`, and the length of its file name
/// (without directories, UTF-8) as copied into `name` (cut at its length).
pub fn module(pc: usize, name: &mut [u8]) -> Option<(usize, usize)> {
    let mut m: HMODULE = std::ptr::null_mut();
    // SAFETY: looks up the module containing an address, without a reference.
    let ok = unsafe { GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, pc as *const u16, &mut m) };
    if ok == 0 {
        return None;
    }
    let mut path = [0u16; 260];
    // SAFETY: a buffer of the length passed.
    let len = unsafe { GetModuleFileNameW(m, path.as_mut_ptr(), path.len() as u32) } as usize;
    let file = path[..len].rsplit(|&c| c == u16::from(b'\\') || c == u16::from(b'/')).next().unwrap_or_default();
    let mut n = 0;
    for c in char::decode_utf16(file.iter().copied()).map(|c| c.unwrap_or('?')) {
        let mut b = [0u8; 4];
        let e = c.encode_utf8(&mut b).as_bytes();
        if n + e.len() > name.len() {
            break;
        }
        name[n..n + e.len()].copy_from_slice(e);
        n += e.len();
    }
    Some((m as usize, n))
}

/// Where the executable is loaded (for offsets into it).
pub fn exe_base() -> usize {
    let mut m: HMODULE = std::ptr::null_mut();
    // SAFETY: looks up the module holding this function, without a reference.
    unsafe { GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, exe_base as *const u16, &mut m) };
    m as usize
}

/// Return addresses of the calling thread's native frames, innermost first; how many fit.
pub fn native_frames(out: &mut [usize]) -> usize {
    // SAFETY: captures into a buffer of the length passed.
    unsafe { RtlCaptureStackBackTrace(0, out.len().min(62) as u32, out.as_mut_ptr() as *mut *mut core::ffi::c_void, std::ptr::null_mut()) as usize }
}

/// Writes `bytes` to stderr without allocating (usable in an exception handler).
pub fn write_stderr(bytes: &[u8]) {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    let mut written = 0u32;
    // SAFETY: writes a borrowed buffer to the standard error handle.
    unsafe { WriteFile(GetStdHandle(STD_ERROR_HANDLE), bytes.as_ptr(), bytes.len() as u32, &mut written, std::ptr::null_mut()) };
}

/// The process's resident (working set) and committed (private) bytes.
pub fn process_memory() -> (usize, usize) {
    // SAFETY: fills a counters struct whose size is passed.
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) == 0 {
            return (0, 0);
        }
        (c.WorkingSetSize, c.PagefileUsage)
    }
}

/// Makes `file` the process's standard error, for the VM's diagnostics and Java's `System.err`.
pub fn redirect_stderr(file: std::fs::File) -> bool {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
    // SAFETY: an open handle the process keeps for its lifetime.
    unsafe { SetStdHandle(STD_ERROR_HANDLE, file.into_raw_handle() as HANDLE) != 0 }
}

/// Creates (or empties) the crash report file `name` (NUL-terminated, in the working directory)
/// without allocating; None when it cannot be created.
pub fn create_report(name: &[u8]) -> Option<usize> {
    use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileA, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ};
    // SAFETY: a NUL-terminated name; the handle is closed by `close_report`.
    let h = unsafe { CreateFileA(name.as_ptr(), GENERIC_WRITE, FILE_SHARE_READ, std::ptr::null(), CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut()) };
    (h != INVALID_HANDLE_VALUE).then_some(h as usize)
}

/// Appends `bytes` to a file from `create_report`.
pub fn write_report(f: usize, bytes: &[u8]) {
    let mut written = 0u32;
    // SAFETY: an open handle from `create_report`, a borrowed buffer.
    unsafe { WriteFile(f as HANDLE, bytes.as_ptr(), bytes.len() as u32, &mut written, std::ptr::null_mut()) };
}

/// Closes a file from `create_report`.
pub fn close_report(f: usize) {
    // SAFETY: an open handle from `create_report`, closed once.
    unsafe { windows_sys::Win32::Foundation::CloseHandle(f as HANDLE) };
}

static HANDLER: OnceLock<FaultHandler> = OnceLock::new();
static CRASH: OnceLock<CrashReport> = OnceLock::new();
static OWN: OnceLock<OwnCode> = OnceLock::new();
/// Set by the first report, so a fault reported at once is not reported again as unhandled.
static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn report_once(pc: usize, addr: usize) {
    if !REPORTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        if let Some(crash) = CRASH.get() {
            crash(pc, addr);
        }
    }
}

/// Sends access violations to `handler` first: it returns where to resume, or None to let the
/// exception go on to the other handlers. One that nothing handles goes to `crash` before the
/// process ends (a vectored handler also sees the faults that code catches itself, a driver's
/// probes say, so the report waits until none did). A fault in rava's own code (`own`) is
/// reported at once: no other handler resolves it, and libraries replace the unhandled-exception
/// filter (a client's audio, windowing or graphics driver), so the report would never come.
/// For faults elsewhere, rava takes the filter back once a second (as HotSpot wraps each thread
/// in its own filter) and chains to the one that replaced it.
pub fn install_fault_handler(handler: FaultHandler, crash: CrashReport, own: OwnCode) {
    if HANDLER.set(handler).is_ok() {
        let _ = CRASH.set(crash);
        let _ = OWN.set(own);
        // SAFETY: registers handlers that live as long as the process.
        unsafe {
            AddVectoredExceptionHandler(1, Some(on_exception));
        }
        take_filter();
        let _ = std::thread::Builder::new().name("rava-fault-filter".into()).spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            take_filter();
        });
    }
}

/// The filter rava's replaced, called after the report (0: none).
static PREVIOUS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Makes `on_unhandled` the unhandled-exception filter again, keeping the one it replaces.
fn take_filter() {
    // SAFETY: swaps the process's filter for one that lives as long as the process.
    let prev = unsafe { SetUnhandledExceptionFilter(Some(on_unhandled)) };
    if let Some(f) = prev {
        if f as usize != on_unhandled as *const () as usize {
            PREVIOUS.store(f as usize, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

unsafe extern "system" fn on_exception(info: *mut EXCEPTION_POINTERS) -> i32 {
    let record = &*(*info).ExceptionRecord;
    // The heap's own fatal errors (a corrupted block found, a fail-fast check): no handler
    // recovers from them, so the report is made now, its native frames naming the code that
    // found the damage (a block written past, freed twice or freed by another allocator).
    const HEAP_CORRUPTION: i32 = 0xC0000374_u32 as i32;
    const FAIL_FAST: i32 = 0xC0000409_u32 as i32;
    if record.ExceptionCode == HEAP_CORRUPTION || record.ExceptionCode == FAIL_FAST {
        report_once((*(*info).ContextRecord).Rip as usize, record.ExceptionCode as u32 as usize);
        return CONTINUE_SEARCH;
    }
    if record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION {
        return CONTINUE_SEARCH;
    }
    // A growing range's first touch, whichever of the two handlers runs first.
    if on_growing_fault(info) == CONTINUE_EXECUTION {
        return CONTINUE_EXECUTION;
    }
    let context = &mut *(*info).ContextRecord;
    let addr = record.ExceptionInformation[1];
    match HANDLER.get().and_then(|h| h(context.Rip as usize, addr)) {
        Some(resume) => {
            context.Rip = resume as u64;
            CONTINUE_EXECUTION
        }
        None => {
            // A fault in rava's own code is fatal, so it is reported here, before any library's
            // unhandled-exception filter (which libraries replace). `RAVA_FAULT_EAGER=1` reports
            // every unresolved access violation here, for a native library whose own handler
            // would otherwise end the process before the report; the exception still goes on to
            // the next handler, so a fault the library handles only adds a note on stderr.
            let pc = context.Rip as usize;
            if OWN.get().is_some_and(|own| own(pc)) || eager() {
                report_once(pc, addr);
            }
            CONTINUE_SEARCH
        }
    }
}

/// Whether `RAVA_FAULT_EAGER` is set.
fn eager() -> bool {
    static EAGER: OnceLock<bool> = OnceLock::new();
    *EAGER.get_or_init(|| std::env::var_os("RAVA_FAULT_EAGER").is_some())
}

unsafe extern "system" fn on_unhandled(info: *const EXCEPTION_POINTERS) -> i32 {
    // A filter that replaced this one may chain back to it: run once per crash.
    static ENTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ENTERED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return CONTINUE_SEARCH;
    }
    let record = &*(*info).ExceptionRecord;
    if record.ExceptionCode == EXCEPTION_ACCESS_VIOLATION {
        report_once((*(*info).ContextRecord).Rip as usize, record.ExceptionInformation[1]);
    }
    match PREVIOUS.load(std::sync::atomic::Ordering::SeqCst) {
        0 => CONTINUE_SEARCH,
        f => {
            let f: unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32 = std::mem::transmute(f);
            f(info)
        }
    }
}

static DUMP: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

unsafe extern "system" fn on_console_event(event: u32) -> windows_sys::Win32::Foundation::BOOL {
    match (event, DUMP.get()) {
        (windows_sys::Win32::System::Console::CTRL_BREAK_EVENT, Some(dump)) => {
            dump();
            1
        }
        _ => 0,
    }
}

/// Runs `dump` each time the console sends Ctrl+Break, as HotSpot prints its thread dump (the
/// system runs the handler on a thread of its own).
pub fn on_dump_request(dump: fn()) {
    if DUMP.set(dump).is_ok() {
        // SAFETY: adds a handler for console events; the others pass on to the default.
        unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on_console_event), 1) };
    }
}
