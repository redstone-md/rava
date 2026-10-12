//! Implicit null checks (HotSpot's): optimized code accesses a field or array header of an
//! object it has not tested for null, and a null makes that access fault in the unmapped first
//! page. The fault handler (`os::install_fault_handler`) finds the faulting instruction among the registered sites and
//! resumes at its deoptimization stub, where an explicit test would have jumped. Any other fault
//! goes on to the handler installed before (the Rust runtime's stack-overflow report).

use std::collections::HashMap;
use std::sync::{Once, OnceLock, RwLock};

/// Faulting instruction -> where to resume.
static SITES: OnceLock<RwLock<HashMap<usize, usize>>> = OnceLock::new();

fn sites() -> &'static RwLock<HashMap<usize, usize>> {
    SITES.get_or_init(Default::default)
}

/// Registers `(faulting instruction, resume address)` pairs of installed code, installing the
/// handler on first use.
pub(super) fn register(pairs: impl IntoIterator<Item = (usize, usize)>) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        sites();
        crate::os::install_fault_handler(on_fault);
    });
    sites().write().unwrap().extend(pairs);
}

fn on_fault(pc: usize, addr: usize) -> Option<usize> {
    if addr < 4096 {
        // Writers only insert from compiler threads, which never run compiled code.
        if let Some(&resume) = sites().read().unwrap().get(&pc) {
            return Some(resume);
        }
    }
    report(pc, addr);
    None
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
    put(b"rava: memory fault at pc ");
    put(&hex(pc));
    put(b" address ");
    put(&hex(addr));
    // The offset in the binary (or library) holding pc, for addr2line.
    // (Reading the loader's tables is not async-signal-safe, but this is the crash path.)
    if let Some(base) = crate::os::module_base(pc) {
        put(b" (module offset ");
        put(&hex(pc - base));
        put(b")");
    }
    put(b"\n");
    crate::os::write_stderr(&buf[..n]);
}
