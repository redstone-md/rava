//! Implicit null checks (HotSpot's): optimized code accesses a field or array header of an
//! object it has not tested for null, and a null makes that access fault in the unmapped first
//! page. The fault handler (`os::install_fault_handler`) finds the faulting instruction among the registered sites and
//! resumes at its deoptimization stub, where an explicit test would have jumped. Any other fault
//! is reported (`crash::fault`) and goes on to the handler installed before (the Rust runtime's
//! stack-overflow report).

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
        crate::os::install_fault_handler(on_fault, crate::crash::fault, own_code);
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
