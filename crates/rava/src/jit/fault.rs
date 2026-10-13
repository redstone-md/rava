//! Implicit null checks (HotSpot's): optimized code accesses a field or array header of an
//! object it has not tested for null, and a null makes that access fault in the unmapped first
//! page. The fault handler (`os::install_fault_handler`) finds the faulting instruction among the registered sites and
//! resumes at its deoptimization stub, where an explicit test would have jumped. Any other fault
//! is reported (`report`) and goes on to the handler installed before (the Rust runtime's
//! stack-overflow report).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Once, OnceLock, RwLock};

/// Faulting instruction -> where to resume.
static SITES: OnceLock<RwLock<HashMap<usize, usize>>> = OnceLock::new();

fn sites() -> &'static RwLock<HashMap<usize, usize>> {
    SITES.get_or_init(Default::default)
}

/// Registers `(faulting instruction, resume address)` pairs of installed code, installing the
/// handler on first use.
pub(super) fn register(pairs: impl IntoIterator<Item = (usize, usize)>) {
    install();
    sites().write().unwrap().extend(pairs);
}

/// Installs the fault handler (once): implicit null checks, and the report of a crash.
pub fn install() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        sites();
        // glibc's backtrace loads its unwinder on first use, which allocates: not in a handler.
        crate::os::native_frames(&mut [0; 4]);
        // rava's own module, found before any fault (the lookup in the handler then only
        // compares).
        own_module();
        crate::os::install_fault_handler(on_fault, report, own_code);
    });
}

/// The base of the module holding rava's code.
fn own_module() -> Option<usize> {
    static BASE: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    *BASE.get_or_init(|| crate::os::module(install as *const () as usize, &mut [0u8; 128]).map(|(base, _)| base))
}

/// Whether `pc` is compiled code or in rava's own executable.
fn own_code(pc: usize) -> bool {
    super::code_cache::contains(pc) || own_module().is_some_and(|base| crate::os::module(pc, &mut [0u8; 128]).is_some_and(|(b, _)| b == base))
}

fn on_fault(pc: usize, addr: usize) -> Option<usize> {
    if addr < 4096 {
        // Writers only insert from compiler threads, which never run compiled code.
        if let Some(&resume) = sites().read().unwrap().get(&pc) {
            return Some(resume);
        }
    }
    None
}

/// Formats into a fixed buffer, written to stderr whenever it fills: reports run in a fault
/// handler, where allocating can deadlock (the fault may be inside the allocator).
struct Stderr {
    buf: [u8; 1024],
    n: usize,
}

impl Stderr {
    fn flush(&mut self) {
        crate::os::write_stderr(&self.buf[..self.n]);
        self.n = 0;
    }
}

impl std::fmt::Write for Stderr {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        for chunk in s.as_bytes().chunks(self.buf.len()) {
            if self.n + chunk.len() > self.buf.len() {
                self.flush();
            }
            self.buf[self.n..self.n + chunk.len()].copy_from_slice(chunk);
            self.n += chunk.len();
        }
        Ok(())
    }
}

/// Writes `pc` as the module holding it and the offset there (for addr2line or a debugger).
fn place(w: &mut Stderr, pc: usize) {
    let mut name = [0u8; 128];
    let _ = match crate::os::module(pc, &mut name) {
        Some((base, len)) => write!(w, "{pc:#x} {}+{:#x}", std::str::from_utf8(&name[..len]).unwrap_or("?"), pc - base),
        None => write!(w, "{pc:#x}"),
    };
}

/// Writes what the process knows of a fatal fault to stderr: the faulting instruction and
/// address, the native frames, and the Java frames of the thread, without allocating.
fn report(pc: usize, addr: usize) {
    let mut w = Stderr { buf: [0; 1024], n: 0 };
    let _ = write!(w, "rava: memory fault at pc ");
    place(&mut w, pc);
    let _ = writeln!(w, " address {addr:#x}");
    let mut frames = [0usize; 48];
    let n = crate::os::native_frames(&mut frames);
    let _ = writeln!(w, "native frames (innermost first, the fault handler's own included):");
    for &f in frames[..n].iter().take_while(|&&f| f != 0) {
        let _ = write!(w, "    ");
        place(&mut w, f);
        let _ = writeln!(w);
    }
    // The Java side last: walking the frames of a broken thread may fault again.
    w.flush();
    crate::dump::write_current_thread(&mut w);
    w.flush();
}
