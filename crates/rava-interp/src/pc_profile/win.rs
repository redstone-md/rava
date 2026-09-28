//! The Windows sampling backend.
//!
//! Re-exported from the parent module, so no call site outside it changed.
use crate::pc_profile::*;

/// `CONTEXT` for x86-64: 1232 bytes, 16-byte aligned. Only two fields are touched —
/// `ContextFlags` at 0x30 (written before the call) and `Rip` at 0xF8 (read after) — so the
/// struct is kept as an opaque aligned buffer rather than transcribing 40 fields that
/// would then have to be kept in sync with the SDK.
#[repr(C, align(16))]
struct Context([u8; 1232]);

const OFF_CONTEXT_FLAGS: usize = 0x30;
const OFF_RSP: usize = 0x98;
const OFF_RIP: usize = 0xF8;
/// CONTEXT_AMD64 | CONTEXT_CONTROL — RIP/RSP/EFlags only, the cheapest capture.
const CONTEXT_CONTROL_AMD64: u32 = 0x0010_0001;
const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;

/// `SYMBOL_INFO` is a variable-length record: an 88-byte fixed head followed by the
/// name bytes, with `SizeOfStruct` describing only the head. Only four fields are
/// touched, so it is kept as an aligned byte buffer rather than transcribed.
const SYMINFO_HEAD: usize = 88;
const OFF_SIZE_OF_STRUCT: usize = 0;
const OFF_MAX_NAME_LEN: usize = 80;
const OFF_NAME_LEN: usize = 76;
const OFF_NAME: usize = 84;
const NAME_CAP: u32 = 512;
/// SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_FAIL_CRITICAL_ERRORS.
const SYM_OPTIONS: u32 = 0x0000_0002 | 0x0000_0004 | 0x0000_0200;

#[link(name = "dbghelp")]
extern "system" {
    fn SymInitialize(process: isize, search_path: *const u8, invade: i32) -> i32;
    fn SymSetOptions(options: u32) -> u32;
    fn SymFromAddr(process: isize, addr: u64, displacement: *mut u64, info: *mut u8) -> i32;
    fn SymGetLineFromAddr64(process: isize, addr: u64, disp: *mut u32, line: *mut u8) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThread() -> isize;
    fn GetCurrentProcess() -> isize;
    fn DuplicateHandle(
        src_process: isize,
        src_handle: isize,
        dst_process: isize,
        dst_handle: *mut isize,
        desired_access: u32,
        inherit: i32,
        options: u32,
    ) -> i32;
    fn GetCurrentThreadId() -> u32;
    fn GetCurrentProcessId() -> u32;
    fn CloseHandle(h: usize) -> i32;
    fn OpenThread(access: u32, inherit: i32, tid: u32) -> usize;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> usize;
    fn Thread32First(snap: usize, entry: *mut u8) -> i32;
    fn Thread32Next(snap: usize, entry: *mut u8) -> i32;
    fn SuspendThread(thread: isize) -> u32;
    fn ResumeThread(thread: isize) -> u32;
    fn GetThreadContext(thread: isize, context: *mut u8) -> i32;
    fn ReadProcessMemory(
        process: isize,
        base: *const u8,
        buf: *mut u8,
        len: usize,
        read: *mut usize,
    ) -> i32;
}

/// A real (non-pseudo) handle to the calling thread, valid from any other thread.
/// This thread's OS id — the dedup key for a multi-target slot claim. A pseudo-handle
/// cannot serve: `GetCurrentThread()` is the same constant on every thread.
pub(super) fn current_thread_id() -> u32 {
    unsafe { GetCurrentThreadId() }
}

/// Every thread id in THIS process, from a toolhelp snapshot. Returns an empty vec on
/// failure — a refresh that finds nothing simply leaves the current target set in place.
pub(super) fn process_thread_ids() -> Vec<u32> {
    const TH32CS_SNAPTHREAD: u32 = 0x0000_0004;
    const ENTRY_BYTES: usize = 28; // THREADENTRY32
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == usize::MAX || snap == 0 {
            return out;
        }
        let pid = GetCurrentProcessId();
        let mut e = [0u8; ENTRY_BYTES];
        e[0..4].copy_from_slice(&(ENTRY_BYTES as u32).to_le_bytes());
        let mut ok = Thread32First(snap, e.as_mut_ptr());
        while ok != 0 {
            let tid = u32::from_le_bytes([e[8], e[9], e[10], e[11]]);
            let owner = u32::from_le_bytes([e[12], e[13], e[14], e[15]]);
            if owner == pid {
                out.push(tid);
            }
            e[0..4].copy_from_slice(&(ENTRY_BYTES as u32).to_le_bytes());
            ok = Thread32Next(snap, e.as_mut_ptr());
        }
        CloseHandle(snap);
    }
    out
}

/// A handle with just enough rights to freeze a thread and read its RIP.
pub(super) fn open_thread(tid: u32) -> usize {
    const THREAD_GET_CONTEXT: u32 = 0x0008;
    const THREAD_SUSPEND_RESUME: u32 = 0x0002;
    unsafe { OpenThread(THREAD_GET_CONTEXT | THREAD_SUSPEND_RESUME, 0, tid) }
}

pub(super) fn duplicate_current_thread() -> usize {
    let mut out: isize = 0;
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            GetCurrentThread(),
            GetCurrentProcess(),
            &mut out,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok != 0 { out as usize } else { 0 }
}

/// Resolve a host address to `symbol+0xNN` against the process's loaded modules and the
/// shipped PDB. Dump-time only. Falls back to the raw address when dbghelp has nothing.
pub(super) fn symbolize(pc: usize) -> String {
    static INIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ok = *INIT.get_or_init(|| unsafe {
        SymSetOptions(SYM_OPTIONS);
        // The default search path is the CURRENT DIRECTORY plus the image directory, and
        // a bench harness runs the exe from elsewhere — so name the exe's own directory
        // explicitly or the .pdb next to the binary is never found and every Rust-host
        // address degrades to a bare pointer.
        let dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let path = std::ffi::CString::new(dir).unwrap_or_default();
        let p = if path.as_bytes().is_empty() {
            std::ptr::null()
        } else {
            path.as_ptr().cast::<u8>()
        };
        SymInitialize(GetCurrentProcess(), p, 1) != 0
    });
    if !ok {
        return format!("<host {pc:#x}>");
    }
    let mut buf = vec![0u8; SYMINFO_HEAD + NAME_CAP as usize];
    unsafe {
        let p = buf.as_mut_ptr();
        p.add(OFF_SIZE_OF_STRUCT).cast::<u32>().write(SYMINFO_HEAD as u32);
        p.add(OFF_MAX_NAME_LEN).cast::<u32>().write(NAME_CAP);
        let mut disp: u64 = 0;
        if SymFromAddr(GetCurrentProcess(), pc as u64, &mut disp, p) == 0 {
            return format!("<host {pc:#x}>");
        }
        let len = p.add(OFF_NAME_LEN).cast::<u32>().read() as usize;
        let name = std::slice::from_raw_parts(p.add(OFF_NAME), len.min(NAME_CAP as usize));
        format!("{}+{:#x}", String::from_utf8_lossy(name), disp)
    }
}

/// ⭐**SOURCE FILE AND LINE for an address**, from the same PDB the symbolizer already loads.
///
/// A per-instruction histogram gives offsets like `run_hot+0x7a5`, and turning those into
/// something actionable otherwise means matching a runtime address against a disassembly through
/// two layers of relocation. The PDB already holds the mapping; `release-diag` keeps it and is
/// codegen-identical to `release` by construction. Empty when the line is unknown (an inlined
/// body without line info, or a `release` build with symbols stripped).
pub(super) fn source_line(pc: usize) -> String {
    // IMAGEHLP_LINE64: DWORD SizeOfStruct; PVOID Key; DWORD LineNumber; PCHAR FileName; DWORD64 Addr
    #[repr(C)]
    struct Line64 {
        size_of_struct: u32,
        _pad: u32,
        key: *mut u8,
        line_number: u32,
        _pad2: u32,
        file_name: *const u8,
        address: u64,
    }
    unsafe {
        let mut l = Line64 {
            size_of_struct: std::mem::size_of::<Line64>() as u32,
            _pad: 0,
            key: std::ptr::null_mut(),
            line_number: 0,
            _pad2: 0,
            file_name: std::ptr::null(),
            address: 0,
        };
        let mut disp: u32 = 0;
        if SymGetLineFromAddr64(
            GetCurrentProcess(),
            pc as u64,
            &mut disp,
            (&mut l as *mut Line64).cast::<u8>(),
        ) == 0
            || l.file_name.is_null()
        {
            return String::new();
        }
        let mut n = 0usize;
        while n < 512 && *l.file_name.add(n) != 0 {
            n += 1;
        }
        let f = String::from_utf8_lossy(std::slice::from_raw_parts(l.file_name, n)).into_owned();
        let short = f.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(&f).to_string();
        format!("{short}:{}", l.line_number)
    }
}

/// Entry address of the function containing `pc`, from the same PDB lookup `symbolize`
/// uses (`pc` minus dbghelp's displacement). 0 when nothing is known about the address.
pub(super) fn symbol_base(pc: usize) -> usize {
    if symbolize(pc).starts_with("<host ") {
        return 0;
    }
    let mut buf = vec![0u8; SYMINFO_HEAD + NAME_CAP as usize];
    unsafe {
        let p = buf.as_mut_ptr();
        p.add(OFF_SIZE_OF_STRUCT).cast::<u32>().write(SYMINFO_HEAD as u32);
        p.add(OFF_MAX_NAME_LEN).cast::<u32>().write(NAME_CAP);
        let mut disp: u64 = 0;
        if SymFromAddr(GetCurrentProcess(), pc as u64, &mut disp, p) == 0 {
            return 0;
        }
        pc.saturating_sub(disp as usize)
    }
}

/// Freeze the target just long enough to read its instruction pointer. The thread is
/// resumed before this function does anything that could block, so it can never be left
/// suspended and can never be frozen while the sampler waits on a lock it holds.
pub(super) fn sample_rip(handle: usize) -> Option<usize> {
    let h = handle as isize;
    unsafe {
        if SuspendThread(h) == u32::MAX {
            return None;
        }
        let mut ctx = Context([0u8; 1232]);
        let base = ctx.0.as_mut_ptr();
        base.add(OFF_CONTEXT_FLAGS).cast::<u32>().write(CONTEXT_CONTROL_AMD64);
        let ok = GetThreadContext(h, base);
        let rip = base.add(OFF_RIP).cast::<u64>().read();
        ResumeThread(h);
        if ok != 0 && rip != 0 { Some(rip as usize) } else { None }
    }
}

/// As `sample_rip`, plus a copy of the top of the target's stack taken inside the same
/// suspend window (the stack below RSP is overwritten the instant the thread resumes, so
/// the copy cannot be deferred the way symbol resolution is).
///
/// The copy goes through `ReadProcessMemory` rather than a plain load for two reasons:
/// it is a SYSCALL, so it cannot block on a user-mode lock the frozen thread holds — the
/// deadlock this module's design note rules out — and it FAILS at the end of the stack
/// instead of faulting. A short read near the stack base is normal, hence the halving
/// retry; a thread with only a few hundred live bytes still yields a usable window.
/// Returns `(rip, bytes_copied)`.
pub(super) fn sample_rip_stack(handle: usize, buf: &mut [u8]) -> Option<(usize, usize)> {
    let h = handle as isize;
    unsafe {
        if SuspendThread(h) == u32::MAX {
            return None;
        }
        let mut ctx = Context([0u8; 1232]);
        let base = ctx.0.as_mut_ptr();
        base.add(OFF_CONTEXT_FLAGS).cast::<u32>().write(CONTEXT_CONTROL_AMD64);
        let ok = GetThreadContext(h, base);
        let rip = base.add(OFF_RIP).cast::<u64>().read() as usize;
        let rsp = base.add(OFF_RSP).cast::<u64>().read() as usize;
        let mut got: usize = 0;
        if ok != 0 && rsp != 0 {
            let mut want = buf.len();
            while want >= 512 {
                let mut n: usize = 0;
                let r = ReadProcessMemory(
                    GetCurrentProcess(),
                    rsp as *const u8,
                    buf.as_mut_ptr(),
                    want,
                    &mut n,
                );
                if r != 0 || n != 0 {
                    got = n.min(want);
                    break;
                }
                want /= 2;
            }
        }
        ResumeThread(h);
        if ok != 0 && rip != 0 { Some((rip, got)) } else { None }
    }
}

/// True when `addr` is committed and executable. Used to tell a return address on the
/// stack from an ordinary data word; called only after the target has been resumed.
pub(super) fn is_executable(addr: usize) -> bool {
    #[repr(C)]
    struct MemInfo {
        base: usize,
        alloc_base: usize,
        alloc_protect: u32,
        _pad0: u32,
        region: usize,
        state: u32,
        protect: u32,
        kind: u32,
        _pad1: u32,
    }
    const MEM_COMMIT: u32 = 0x1000;
    const PAGE_EXEC_ANY: u32 = 0xF0; // EXECUTE | EXECUTE_READ | EXECUTE_READWRITE | EXECUTE_WRITECOPY
    const PAGE_GUARD: u32 = 0x100;
    let mut mi = MemInfo {
        base: 0,
        alloc_base: 0,
        alloc_protect: 0,
        _pad0: 0,
        region: 0,
        state: 0,
        protect: 0,
        kind: 0,
        _pad1: 0,
    };
    let n = unsafe {
        VirtualQuery(
            addr as *const u8,
            (&mut mi as *mut MemInfo).cast::<u8>(),
            core::mem::size_of::<MemInfo>(),
        )
    };
    n != 0
        && mi.state == MEM_COMMIT
        && mi.protect & PAGE_EXEC_ANY != 0
        && mi.protect & PAGE_GUARD == 0
}

#[link(name = "kernel32")]
extern "system" {
    fn VirtualQuery(addr: *const u8, buf: *mut u8, len: usize) -> usize;
}
