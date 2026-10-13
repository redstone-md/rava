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
