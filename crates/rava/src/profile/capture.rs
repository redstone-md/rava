//! Capturing samples on Windows: each registered Java thread is suspended just long enough to
//! read its registers and walk its frame headers; everything that may change once it runs on
//! (an interpreted frame's live bytecode index) is copied while it is suspended. Resolving the
//! words into frames happens afterwards, on the recorder's thread (`recorder.rs`).

use std::sync::Mutex;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::{GetThreadContext, ReadProcessMemory, CONTEXT, CONTEXT_CONTROL_AMD64};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThreadId, GetThreadDescription, OpenThread, ResumeThread, SuspendThread, THREAD_GET_CONTEXT, THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
};

#[link(name = "kernel32")]
extern "system" {
    fn QueryThreadCycleTime(thread: HANDLE, cycles: *mut u64) -> i32;
}

/// A registered thread: its handle, its `Thread`, and its cycle count at the last sample.
struct Sampled {
    id: u32,
    handle: HANDLE,
    thread: usize,
    cycles: u64,
}

// Handles are process-wide.
unsafe impl Send for Sampled {}

static THREADS: Mutex<Vec<Sampled>> = Mutex::new(Vec::new());

/// Threads that exited since the recorder last asked: id, name and total cycles.
static RETIRED: Mutex<Vec<(u32, String, u64)>> = Mutex::new(Vec::new());

/// Cycles a thread must have used since its last sample to count as running in `cpu` mode: a
/// thread that only woke from a timed wait in that interval is not using the CPU (~0.1 ms).
const BUSY_CYCLES: u64 = 300_000;

/// Frame headers walked at most per sample.
const MAX_HEADERS: usize = 1024;

pub fn attach_current(thread: usize) {
    // SAFETY: plain queries and an open of the calling thread.
    unsafe {
        let id = GetCurrentThreadId();
        let handle = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_LIMITED_INFORMATION, 0, id);
        if !handle.is_null() {
            THREADS.lock().unwrap().push(Sampled { id, handle, thread, cycles: 0 });
        }
    }
}

pub fn detach_current() {
    // SAFETY: the calling thread's id; the handle was opened by `attach_current`.
    let id = unsafe { GetCurrentThreadId() };
    let mut gone = None;
    THREADS.lock().unwrap().retain(|s| {
        if s.id == id {
            let mut cycles = 0u64;
            // SAFETY: opened by `attach_current`, queried and closed once.
            unsafe { QueryThreadCycleTime(s.handle, &mut cycles) };
            gone = Some((s.id, name(s.handle), cycles));
            unsafe { CloseHandle(s.handle) };
            false
        } else {
            true
        }
    });
    RETIRED.lock().unwrap().extend(gone);
}

/// One thread's sample, unresolved.
pub struct Raw {
    pub thread: u32,
    /// Cycles the thread used since its previous sample.
    pub cycles: u64,
    pub pc: usize,
    /// The return address on top of the stack, for a pc in a system call stub (who made it).
    pub top: usize,
    /// Return addresses further up (scanned), the first into compiled code or a non-system
    /// module: who called the system library or the VM's code.
    pub scanned: Vec<usize>,
    /// Each frame header's method, bytecode-index word and whether it is an interpreted frame
    /// (whose index is then already read), from the innermost out.
    pub headers: Vec<(usize, u64, bool)>,
}

#[repr(C, align(16))]
struct Ctx(CONTEXT);

impl Raw {
    fn new() -> Raw {
        Raw { thread: 0, cycles: 0, pc: 0, top: 0, scanned: Vec::with_capacity(SCAN_WORDS), headers: Vec::with_capacity(MAX_HEADERS) }
    }
}

/// Words scanned above the stack pointer at most.
const SCAN_WORDS: usize = 512;

/// Samples every registered thread (in `cpu` mode only those that used the CPU since their
/// last sample) into the first entries of `pool`, and returns how many. The entries' buffers
/// are reused and never grow while a thread is suspended: the suspended thread may hold the
/// allocator's lock.
pub fn sample(wall: bool, pool: &mut Vec<Raw>) -> usize {
    let mut ctx: Box<Ctx> = Box::new(unsafe { std::mem::zeroed() });
    let mut threads = THREADS.lock().unwrap();
    let mut n = 0;
    for s in threads.iter_mut() {
        let mut cycles = 0u64;
        // SAFETY: an open handle with query rights.
        unsafe { QueryThreadCycleTime(s.handle, &mut cycles) };
        let delta = if s.cycles == 0 { 0 } else { cycles.saturating_sub(s.cycles) };
        s.cycles = cycles;
        if !wall && delta < BUSY_CYCLES {
            continue;
        }
        if pool.len() == n {
            pool.push(Raw::new());
        }
        let raw = &mut pool[n];
        (raw.thread, raw.cycles, raw.pc, raw.top) = (s.id, delta, 0, 0);
        raw.scanned.clear();
        raw.headers.clear();
        // SAFETY: the thread is suspended between these calls, and nothing in between allocates
        // or takes a lock it could hold (the buffers are reserved before).
        unsafe {
            if SuspendThread(s.handle) == u32::MAX {
                continue;
            }
            ctx.0.ContextFlags = CONTEXT_CONTROL_AMD64;
            let ok = GetThreadContext(s.handle, &mut ctx.0) != 0;
            if ok {
                raw.pc = ctx.0.Rip as usize;
                let sp = ctx.0.Rsp as usize;
                let mut word = [0usize; 1];
                if copy(sp, &mut word) {
                    raw.top = word[0];
                }
                if !crate::jit::code_cache::contains(raw.pc) {
                    scan(sp, &mut raw.scanned);
                }
                if s.thread != 0 {
                    walk_headers(s.thread, &mut raw.headers);
                }
            }
            ResumeThread(s.handle);
            if ok {
                n += 1;
            }
        }
    }
    n
}

/// Each registered thread's id, name and total CPU cycles, and those of the threads that
/// exited since the last call (their last count).
pub fn threads() -> Vec<(u32, String, u64)> {
    let mut out = std::mem::take(&mut *RETIRED.lock().unwrap());
    let threads = THREADS.lock().unwrap();
    out.extend(threads.iter().map(|s| {
        let mut cycles = 0u64;
        // SAFETY: an open handle with query rights.
        unsafe { QueryThreadCycleTime(s.handle, &mut cycles) };
        (s.id, name(s.handle), cycles)
    }));
    out
}

fn name(handle: HANDLE) -> String {
    let mut p: *mut u16 = std::ptr::null_mut();
    // SAFETY: the system allocates the description; it is freed after copying.
    unsafe {
        if GetThreadDescription(handle, &mut p) < 0 || p.is_null() {
            return "?".into();
        }
        let n = (0..).take_while(|&i| *p.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
        windows_sys::Win32::Foundation::LocalFree(p as _);
        if s.is_empty() { "?".into() } else { s }
    }
}

/// Copies words at `at` without faulting on unmapped memory.
pub fn copy(at: usize, out: &mut [usize]) -> bool {
    let mut got = 0usize;
    // SAFETY: the system checks the source range; the destination is `out`.
    unsafe { ReadProcessMemory(GetCurrentProcess(), at as *const _, out.as_mut_ptr() as *mut _, out.len() * 8, &mut got) != 0 && got == out.len() * 8 }
}

/// The words above `sp` that may be return addresses, within the stack's committed memory and
/// at most 4 KiB (there are no frame pointers; the recorder keeps the first that lands in code
/// it knows).
fn scan(sp: usize, out: &mut Vec<usize>) {
    use windows_sys::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT};
    let mut words = [0usize; SCAN_WORDS];
    // SAFETY: a query of an address in this process.
    let committed = unsafe {
        let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        if VirtualQuery(sp as *const _, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) == 0 || info.State != MEM_COMMIT {
            return;
        }
        info.BaseAddress as usize + info.RegionSize - sp
    };
    let n = (committed / 8).saturating_sub(1).min(words.len());
    if copy(sp + 8, &mut words[..n]) {
        out.extend(words[..n].iter().copied().filter(|&w| w > 0x10000));
    }
}

/// Appends each frame header's method and bytecode-index word, from the thread's `top`
/// outwards; an interpreted frame's tagged pointer to its live index is read now. A header
/// whose caller link does not lead outwards (the value stack grows up) ends the walk.
unsafe fn walk_headers(thread: usize, out: &mut Vec<(usize, u64, bool)>) {
    use crate::thread::{header, offsets};
    let stack = (*(thread as *const crate::thread::Thread)).value_stack();
    let mut h = *((thread + offsets::TOP as usize) as *const usize);
    for _ in 0..MAX_HEADERS {
        if h % 8 != 0 || !stack.contains(&h) || !stack.contains(&(h + 8 * header::SLOTS)) {
            return;
        }
        let p = h as *const usize;
        let mut bci = *p.add(header::BCI) as u64;
        let interpreted = bci & 1 == 1;
        if interpreted {
            let mut live = [0usize; 1];
            bci = if copy((bci & !1) as usize, &mut live) { (live[0] as u64) << 1 } else { u64::MAX };
        }
        out.push((*p.add(header::METHOD), bci, interpreted));
        let c = *p.add(header::CALLER);
        if c >= h || h - c > 64 << 20 {
            return;
        }
        h = c;
    }
}

/// An allocation sample of the calling thread, unresolved: its thread id, the class noted
/// (0 if none), the bytes, and its frame headers as `Raw::headers`.
pub struct Allocation {
    pub thread: u32,
    pub class: usize,
    pub bytes: u64,
    pub headers: Vec<(usize, u64, bool)>,
}

/// Allocation samples taken since the recorder last looked.
pub static ALLOCATIONS: Mutex<Vec<Allocation>> = Mutex::new(Vec::new());

/// Takes an allocation sample of the calling thread `t`, walking its own headers.
pub fn allocation(t: &crate::thread::Thread, class: usize, bytes: usize) {
    let mut headers = Vec::with_capacity(32);
    // SAFETY: the calling thread's own `Thread` and frames, which stay put while it walks them.
    unsafe { walk_headers(t as *const _ as usize, &mut headers) };
    // SAFETY: a plain query.
    let thread = unsafe { GetCurrentThreadId() };
    ALLOCATIONS.lock().unwrap().push(Allocation { thread, class, bytes: bytes as u64, headers });
}