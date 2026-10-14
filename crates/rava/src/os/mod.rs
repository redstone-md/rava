//! The operating system under the VM (`docs/platforms-roadmap.md`): the address space, faults,
//! thread stacks, shared libraries and process facts, behind one interface. Everything else in
//! the VM is the same on every system; the JDK's native methods, which follow each system's
//! class library, live in `natives/`.
//!
//! Memory from `reserve` is zero-filled and costs physical memory only where it is touched.
//! Memory from `reserve_uncommitted` (the heap) must also be `commit`ted before use: Windows
//! counts committed memory against a system-wide limit, so the heap commits as it grows.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// What a fault handler decides: resume at an address, or let the fault take its course.
pub type FaultHandler = fn(pc: usize, addr: usize) -> Option<usize>;
/// Reports a fault no handler resumes from, before the process dies of it.
pub type CrashReport = fn(pc: usize, addr: usize);
/// Whether `pc` is in rava's own code (its executable or compiled code): a fault there that
/// the fault handler does not resolve is fatal, whatever handlers other code installed.
pub type OwnCode = fn(pc: usize) -> bool;

/// A file of an enabled diagnostic: an explicit directory, else the platform temporary
/// directory (`TMPDIR` on Unix). Resolve it once, only when a diagnostic writes a file.
/// Directories are not created; a writer reports an invalid or unwritable destination.
pub(crate) fn diagnostic_path(name: &str) -> std::path::PathBuf {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os("RAVA_DIAG_DIR")
            .filter(|dir| !dir.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }).join(name)
}
