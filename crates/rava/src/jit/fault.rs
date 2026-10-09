//! Implicit null checks (HotSpot's): optimized code accesses a field or array header of an
//! object it has not tested for null, and a null makes that access fault in the unmapped first
//! page. The SIGSEGV handler finds the faulting instruction among the registered sites and
//! resumes at its deoptimization stub, where an explicit test would have jumped. Any other fault
//! goes on to the handler installed before (the Rust runtime's stack-overflow report).

use std::collections::HashMap;
use std::sync::{Once, OnceLock, RwLock};

/// Faulting instruction -> where to resume.
static SITES: OnceLock<RwLock<HashMap<usize, usize>>> = OnceLock::new();
static PREVIOUS: OnceLock<libc::sigaction> = OnceLock::new();

fn sites() -> &'static RwLock<HashMap<usize, usize>> {
    SITES.get_or_init(Default::default)
}

/// Registers `(faulting instruction, resume address)` pairs of installed code, installing the
/// handler on first use.
pub(super) fn register(pairs: impl IntoIterator<Item = (usize, usize)>) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(install);
    sites().write().unwrap().extend(pairs);
}

fn install() {
    sites();
    // SAFETY: plain sigaction calls; the previous action is kept for chaining.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_NODEFER;
        libc::sigemptyset(&mut action.sa_mask);
        let mut previous: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGSEGV, &action, &mut previous) == 0 {
            let _ = PREVIOUS.set(previous);
        }
    }
}

extern "C" fn handler(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    // SAFETY: the kernel passes a valid siginfo and ucontext to an SA_SIGINFO handler.
    unsafe {
        let uc = ctx as *mut libc::ucontext_t;
        let rip = &mut (*uc).uc_mcontext.gregs[libc::REG_RIP as usize];
        if ((*info).si_addr() as usize) < 4096 {
            // Writers only insert from compiler threads, which never run compiled code.
            if let Some(&resume) = sites().read().unwrap().get(&(*rip as usize)) {
                *rip = resume as i64;
                return;
            }
        }
        report(*rip as usize, (*info).si_addr() as usize);
        let previous = PREVIOUS.get();
        match previous.map(|p| p.sa_sigaction) {
            Some(h) if h != libc::SIG_DFL && h != libc::SIG_IGN => {
                if previous.unwrap().sa_flags & libc::SA_SIGINFO != 0 {
                    let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) = std::mem::transmute(h);
                    f(sig, info, ctx);
                } else {
                    let f: extern "C" fn(libc::c_int) = std::mem::transmute(h);
                    f(sig);
                }
            }
            // The default action: the access runs again and the process dies of it.
            _ => {
                let mut dfl: libc::sigaction = std::mem::zeroed();
                dfl.sa_sigaction = libc::SIG_DFL;
                libc::sigaction(libc::SIGSEGV, &dfl, std::ptr::null_mut());
            }
        }
    }
}

/// Writes the faulting instruction and address to stderr (async-signal-safe: no allocation).
fn report(pc: usize, addr: usize) {
    let mut buf = [0u8; 128];
    let mut n = 0;
    let mut put = |bytes: &[u8]| {
        for &b in bytes {
            if n < buf.len() {
                buf[n] = b;
                n += 1;
            }
        }
    };
    let hex = |v: usize| {
        let mut h = [0u8; 18];
        h[0] = b'0';
        h[1] = b'x';
        for i in 0..16 {
            let d = (v >> (60 - 4 * i)) & 15;
            h[2 + i] = if d < 10 { b'0' + d as u8 } else { b'a' + d as u8 - 10 };
        }
        h
    };
    put(b"rava: SIGSEGV at pc ");
    put(&hex(pc));
    put(b" address ");
    put(&hex(addr));
    // The offset in the binary (or library) holding pc, for addr2line.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: dladdr reads the loader's tables (not async-signal-safe, but this is the crash path).
    if unsafe { libc::dladdr(pc as *const libc::c_void, &mut info) } != 0 && !info.dli_fbase.is_null() {
        put(b" (module offset ");
        put(&hex(pc - info.dli_fbase as usize));
        put(b")");
    }
    put(b"\n");
    // SAFETY: write(2) of a stack buffer.
    unsafe { libc::write(2, buf.as_ptr() as *const libc::c_void, n) };
}
