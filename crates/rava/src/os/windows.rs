//! Windows: `VirtualAlloc`, a vectored exception handler, `LoadLibraryW` and the thread and
//! process queries. Memory is reserved and committed together: Windows charges committed memory
//! against its commit limit but still gives physical pages only to the ones touched.

use super::FaultHandler;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, HANDLE, HMODULE};
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleExW, GetProcAddress, LoadLibraryW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT};
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

/// Returns the range's memory to the system; it reads as zeros afterwards.
pub fn discard(p: usize, len: usize) -> bool {
    // SAFETY: decommitting and recommitting the caller's pages zero-fills them.
    unsafe { VirtualFree(p as *mut _, len, MEM_DECOMMIT) != 0 && !VirtualAlloc(p as *const _, len, MEM_COMMIT, PAGE_READWRITE).is_null() }
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
pub fn set_thread_name(name: &std::ffi::CStr) {
    let w: Vec<u16> = name.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: names the calling thread with a NUL-terminated wide string.
    unsafe { windows_sys::Win32::System::Threading::SetThreadDescription(GetCurrentThread(), w.as_ptr()) };
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

/// The base address of the binary or library holding `pc`.
pub fn module_base(pc: usize) -> Option<usize> {
    let mut m: HMODULE = std::ptr::null_mut();
    // SAFETY: looks up the module containing an address, without a reference.
    let ok = unsafe { GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, pc as *const u16, &mut m) };
    (ok != 0).then_some(m as usize)
}

/// Writes `bytes` to stderr without allocating (usable in an exception handler).
pub fn write_stderr(bytes: &[u8]) {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    let mut written = 0u32;
    // SAFETY: writes a borrowed buffer to the standard error handle.
    unsafe { WriteFile(GetStdHandle(STD_ERROR_HANDLE), bytes.as_ptr(), bytes.len() as u32, &mut written, std::ptr::null_mut()) };
}

static HANDLER: OnceLock<FaultHandler> = OnceLock::new();

/// Sends access violations to `handler` first: it returns where to resume, or None to let the
/// exception go on to the other handlers.
pub fn install_fault_handler(handler: FaultHandler) {
    if HANDLER.set(handler).is_ok() {
        // SAFETY: registers a handler that lives as long as the process.
        unsafe { AddVectoredExceptionHandler(1, Some(on_exception)) };
    }
}

unsafe extern "system" fn on_exception(info: *mut EXCEPTION_POINTERS) -> i32 {
    let record = &*(*info).ExceptionRecord;
    if record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION {
        return CONTINUE_SEARCH;
    }
    let context = &mut *(*info).ContextRecord;
    let addr = record.ExceptionInformation[1];
    match HANDLER.get().and_then(|h| h(context.Rip as usize, addr)) {
        Some(resume) => {
            context.Rip = resume as u64;
            CONTINUE_EXECUTION
        }
        None => CONTINUE_SEARCH,
    }
}
