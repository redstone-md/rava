//! Where the process's memory goes. The VM's own allocations (class metadata, compiler data,
//! JNI buffers...) are counted by the global allocator wrapper `Counting`; `RAVA_MEM_LOG=<seconds>`
//! prints a line at that interval: resident and committed memory, the heap, the VM's
//! allocations, the low arena (classes, methods) and the code cache.

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};

/// Bytes the VM holds from its allocator, counted only while RAVA_MEM_LOG reports: one
/// counter every thread updates on each allocation cost a Paper start 30% (7.7 -> 10.0 s).
static ALLOCATED: AtomicIsize = AtomicIsize::new(0);

/// The bytes the allocator itself holds committed, when the binary can say (`main.rs` sets it):
/// with many short-lived threads it can be far more than what the VM holds from it.
pub static ALLOCATOR_COMMITTED: std::sync::OnceLock<fn() -> usize> = std::sync::OnceLock::new();
static COUNTING: AtomicBool = AtomicBool::new(false);

/// A global allocator counting the bytes held through `A`.
pub struct Counting<A>(pub A);

// SAFETY: forwards every call to `A`, which upholds the contract.
unsafe impl<A: GlobalAlloc> GlobalAlloc for Counting<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = self.0.alloc(layout);
        if !p.is_null() && COUNTING.load(Relaxed) {
            ALLOCATED.fetch_add(layout.size() as isize, Relaxed);
            crate::nativeprof::allocated(p as usize, layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = self.0.alloc_zeroed(layout);
        if !p.is_null() && COUNTING.load(Relaxed) {
            ALLOCATED.fetch_add(layout.size() as isize, Relaxed);
            crate::nativeprof::allocated(p as usize, layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNTING.load(Relaxed) {
            ALLOCATED.fetch_sub(layout.size() as isize, Relaxed);
            crate::nativeprof::freed(ptr as usize);
        }
        self.0.dealloc(ptr, layout);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = self.0.realloc(ptr, layout, new_size);
        if !p.is_null() && COUNTING.load(Relaxed) {
            ALLOCATED.fetch_add(new_size as isize - layout.size() as isize, Relaxed);
            crate::nativeprof::freed(ptr as usize);
            crate::nativeprof::allocated(p as usize, new_size);
        }
        p
    }
}

/// Counts the VM's allocations from the start of the process when `RAVA_MEM_LOG` is set, so
/// that what creating the VM allocates is counted too (the report starts later).
pub fn count_from_start() {
    if std::env::var_os("RAVA_MEM_LOG").is_some() {
        crate::nativeprof::init();
        COUNTING.store(true, Relaxed);
    }
}

/// Starts the report thread when `RAVA_MEM_LOG` asks for one.
pub fn start() {
    let Some(secs) = std::env::var("RAVA_MEM_LOG").ok().and_then(|v| v.parse::<u64>().ok()).filter(|&s| s > 0) else { return };
    COUNTING.store(true, Relaxed);
    let _ = std::thread::Builder::new().name("rava-mem-log".into()).spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(secs));
        let mb = |b: usize| b >> 20;
        let (rss, committed) = crate::os::process_memory();
        let vm = crate::vm::vm();
        eprintln!(
            "[mem] resident {} MiB, committed {} MiB | heap {} MiB (live after last gc {} MiB) | vm allocations {} MiB (allocator committed {} MiB) | low arena {} MiB | code {} MiB | {} classes, {} threads | monitors inflated {}, thin-lock sleeps {}",
            mb(rss),
            mb(committed),
            mb(vm.heap.used_bytes()),
            mb(vm.heap.live_after_gc()),
            mb(ALLOCATED.load(Relaxed).max(0) as usize),
            mb(ALLOCATOR_COMMITTED.get().map_or(0, |f| f())),
            mb(crate::low::reserved_bytes()),
            mb(crate::jit::code_cache::used_bytes()),
            vm.loaded_class_count(),
            crate::gc::safepoint::all_threads().len(),
            crate::sync::INFLATIONS.load(Relaxed),
            crate::sync::THIN_SLEEPS.load(Relaxed),
        );
        for line in crate::nativeprof::report() {
            eprintln!("{line}");
        }
    });
}
