//! Faulting in heap memory ahead of allocation, off the allocating threads. The first touch of
//! a fresh page costs a page fault and the kernel's clearing of it; taken by whichever thread
//! bump-allocates there, it lands on the critical path (world generation spent more than a
//! second of its CPU there). A background thread keeps the next `AHEAD` bytes past each fresh
//! frontier (small-object pages, the large-object area) populated with `MADV_POPULATE_WRITE`,
//! which faults pages in writable without touching their contents, so it is safe while
//! allocation reaches into the same range.

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::OnceLock;

/// How far past the frontier memory is kept populated, and the step of each `madvise`.
const AHEAD: usize = 64 << 20;
const STEP: usize = 2 << 20;
/// Linux 5.14's `MADV_POPULATE_WRITE`.
const MADV_POPULATE_WRITE: libc::c_int = 23;

/// The small-object pages and the large-object area.
#[derive(Clone, Copy)]
pub enum Region {
    Small = 0,
    Large = 1,
}

struct Frontier {
    /// Where allocation has reached.
    fresh: AtomicUsize,
    /// Populated up to here.
    ready: AtomicUsize,
    end: usize,
}

struct Prefaulter {
    regions: [Frontier; 2],
    thread: std::thread::Thread,
}

static PREFAULTER: OnceLock<Prefaulter> = OnceLock::new();

/// Starts the background thread for the two regions `[base, end)` (unless `RAVA_CORE_PREFAULT=0`).
pub fn start(small: (usize, usize), large: (usize, usize)) {
    if std::env::var("RAVA_CORE_PREFAULT").is_ok_and(|v| v == "0") {
        return;
    }
    let frontier = |(base, end): (usize, usize)| Frontier { fresh: AtomicUsize::new(base), ready: AtomicUsize::new(base), end };
    let regions = [frontier(small), frontier(large)];
    let Ok(handle) = std::thread::Builder::new().name("rava-prefault".into()).spawn(|| {
        while PREFAULTER.get().is_none() {
            std::thread::park();
        }
        let p = PREFAULTER.get().unwrap();
        loop {
            p.regions.iter().for_each(Frontier::populate);
            std::thread::park();
        }
    }) else {
        return;
    };
    let thread = handle.thread().clone();
    if PREFAULTER.set(Prefaulter { regions, thread }).is_ok() {
        PREFAULTER.get().unwrap().thread.unpark();
    }
}

/// Allocation in `region` has reached `fresh`: wakes the thread once the populated margin is
/// down to half.
#[inline]
pub fn advanced(region: Region, fresh: usize) {
    let Some(p) = PREFAULTER.get() else { return };
    let f = &p.regions[region as usize];
    f.fresh.store(fresh, Relaxed);
    if fresh + AHEAD / 2 > f.ready.load(Relaxed) {
        p.thread.unpark();
    }
}

impl Frontier {
    fn populate(&self) {
        loop {
            let fresh = self.fresh.load(Relaxed);
            let goal = (fresh + AHEAD).min(self.end);
            let at = self.ready.load(Relaxed).max(fresh);
            if at >= goal {
                return;
            }
            let len = STEP.min(goal - at);
            // SAFETY: inside the heap reservation; populating changes no contents.
            if unsafe { libc::madvise(at as *mut libc::c_void, len, MADV_POPULATE_WRITE) } != 0 {
                // Unsupported (older kernels): allocation faults pages in itself.
                self.ready.store(self.end, Relaxed);
                return;
            }
            self.ready.store(at + len, Relaxed);
        }
    }
}
