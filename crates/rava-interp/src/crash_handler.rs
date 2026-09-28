//! NAME THE FAULTING INSTRUCTION INSTEAD OF EXITING SILENTLY.
//!
//! ⭐**WHY THIS DID NOT EXIST AND SHOULD HAVE.** An access violation in Rust on Windows is an SEH
//! exception, not a panic: the process dies with `0xC0000005` and prints NOTHING. Ledger row 339
//! met exactly that, could not see where it faulted, and worked around it by raising the nursery
//! so the collector would not run — a workaround that row 419 then showed is load-bearing for the
//! whole compilers-off boot, because it also suppresses the safepoint the GIL hand-off lives at.
//!
//! A vectored exception handler costs nothing until something faults, and the symbolizer the PC
//! profiler already carries turns the faulting address into a function name. That is the
//! difference between "the boot exits at 70 s" and a line to fix.
//!
//! ⚠**It does not swallow the fault.** The handler prints and returns `EXCEPTION_CONTINUE_SEARCH`,
//! so the process still dies exactly as it did — a crash reporter that keeps a corrupted process
//! running is worse than the crash.

#[cfg(windows)]
mod imp {
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
    const EXCEPTION_ACCESS_VIOLATION: u32 = 0xC000_0005;
    const EXCEPTION_STACK_OVERFLOW: u32 = 0xC000_00FD;
    const EXCEPTION_ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;

    extern "system" {
        fn AddVectoredExceptionHandler(first: u32, handler: usize) -> usize;
    }

    /// EXCEPTION_RECORD (x64): code@0, flags@4, record@8, address@16, nparams@24, info@32.
    unsafe extern "system" fn handler(info: *mut u8) -> i32 {
        if info.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // EXCEPTION_POINTERS: ExceptionRecord@0, ContextRecord@8.
        let rec = unsafe { *(info as *const usize) } as *const u8;
        if rec.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let code = unsafe { *(rec as *const u32) };
        if !matches!(
            code,
            EXCEPTION_ACCESS_VIOLATION | EXCEPTION_STACK_OVERFLOW | EXCEPTION_ILLEGAL_INSTRUCTION
        ) {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // Report ONCE. A fault inside the reporting of a fault is a hang, not a diagnosis.
        static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if REPORTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let addr = unsafe { *(rec.add(16) as *const usize) };
        let nparams = unsafe { *(rec.add(24) as *const u32) } as usize;
        let (kind, target) = if code == EXCEPTION_ACCESS_VIOLATION && nparams >= 2 {
            let op = unsafe { *(rec.add(32) as *const usize) };
            let tgt = unsafe { *(rec.add(40) as *const usize) };
            (
                match op {
                    0 => "read",
                    1 => "write",
                    8 => "execute (DEP)",
                    _ => "?",
                },
                tgt,
            )
        } else {
            ("", 0)
        };
        // ⚠**HOST FRAME ONLY.** Walking the guest frame stack from here would read VM state the
        // fault may have just corrupted, and a reporter that faults is worse than no reporter.
        // The symbolizer reads the PDB, which is independent of this process's own heap.
        let (name, line) = crate::pc_profile::fault_site(addr);
        eprintln!("\n[CRASH] {code:#010x} at {addr:#x}  {name}  {line}");
        // ⭐**NAME THE COMPILED METHOD, not just the address.** A fault inside JIT-emitted code
        // symbolises as `<host 0x…>` because those addresses belong to no module — which left a
        // real blocker ("compiling fastutil dies") readable only as a hex number. `jit_enter`
        // brackets every compiled entry with `jit_method_push/pop` unconditionally, so the
        // innermost entry of that stack IS the method executing at the fault.
        for (n_, m_) in crate::jit::jit_method_stack(4).iter().enumerate() {
            eprintln!("[CRASH]   compiled[{n_}] {m_}");
        }
        if !kind.is_empty() {
            eprintln!("[CRASH]   {kind} of {target:#x}");
        }
        use std::io::Write as _;
        let _ = std::io::stderr().flush();
        EXCEPTION_CONTINUE_SEARCH
    }

    /// Install once. `first = 1` so this runs before anything else that might swallow it.
    pub fn install() {
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        unsafe {
            AddVectoredExceptionHandler(1, handler as usize);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn install() {}
}

pub use imp::install;
