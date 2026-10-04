//! rava: the parity execution core. Design: `docs/core/README.md`.

pub mod assertions;
pub mod boot;
pub mod classfile;
pub mod cpool;
pub mod gc;
pub mod heap;
pub mod interp;
pub mod jit;
pub mod jimage;
pub mod klass;
pub mod launcher;
pub mod link;
pub mod loader;
pub mod low;
pub mod method;
pub mod natives;
pub mod parker;
pub mod runtime;
pub mod symbol;
pub mod sync;
pub mod thread;
pub mod types;
pub mod vm;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

static ACTIVE_PROCESSORS: AtomicUsize = AtomicUsize::new(0);

/// The processors the program and the VM's own thread pools size themselves by: HotSpot's
/// `-XX:ActiveProcessorCount=n`, else the CPUs this process may run on.
pub fn processors() -> usize {
    match ACTIVE_PROCESSORS.load(Relaxed) {
        0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
        n => n,
    }
}

pub(crate) fn set_active_processors(n: usize) {
    ACTIVE_PROCESSORS.store(n, Relaxed);
}

// Class metadata is immutable after definition except through its atomics and locks.
unsafe impl Sync for klass::Klass {}
unsafe impl Send for klass::Klass {}
unsafe impl Sync for method::Method {}
unsafe impl Send for method::Method {}

/// Locates the JDK whose `lib/modules` supplies the class library: `JAVA_HOME`, else the
/// installation that owns the `java` launcher on `PATH`.
pub fn java_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("JAVA_HOME").map(PathBuf::from) {
        if home.join("lib/modules").is_file() {
            return Some(home);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("java"))
        .find(|java| java.is_file())
        .and_then(|java| std::fs::canonicalize(java).ok())
        .and_then(|java| java.parent()?.parent().map(PathBuf::from))
        .filter(|home| home.join("lib/modules").is_file())
}
