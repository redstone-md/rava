//! The JIT context a worker installs, its fault latch, and the COW bloom over it.
//!
//! Re-exported from the parent module, so no call site outside it changed.
//!
//! ⭐**TLS AUDIT — ALREADY THE CHEAP SHAPE, NOTHING CHANGED HERE.** Every function below reaches
//! exactly one thread-local, `heap::workerstate::WORKER_JIT_CTX`, and it is declared
//! `= const { std::cell::Cell::new(std::ptr::null_mut()) }` — the literal `= const { … }` form
//! the `thread_local!` macro's fast arm matches, holding a `Cell<*mut JitCtx>`, a raw pointer
//! with no destructor. `needs_drop` is false, so std emits a bare `#[thread_local] static` and
//! `.with` lowers to the TLS base read plus a constant displacement: no lazy-initialisation
//! `State` byte, no compare, no branch, no registered destructor. That is the floor.
//!
//! Each entry point costs exactly ONE such read: `active_jit_ctx`, `active_cow_bloom` and
//! `set_active_cow_armed` take it directly, and `set_active_jit_fault`, `cow_bloom_note`,
//! `cow_bloom_remove` and `cow_bloom_maybe` inherit it through one of those. The null test that
//! follows is a register compare against the pointer just loaded, not a second TLS touch. There
//! is no `RefCell`, no `Vec`, no owned payload and no per-lookup repetition to remove — the
//! per-thread COW bloom that this indirection reaches is owned by `WorkerJitCtxGuard`'s `Box`,
//! whose `Drop` balances the shared counters (`parallel::shadow_clear`) and must stay a real
//! destructor on a real guard, NOT in TLS. (`WORKER_JIT_CTX` itself lives in
//! `heap/workerstate.rs`, outside this file's edit scope, and needs no change.)

use crate::heap::*;

#[inline]
pub(crate) fn active_jit_ctx(default: *mut JitCtx) -> *mut JitCtx {
    crate::heap::WORKER_JIT_CTX.with(|slot| {
        let worker = slot.get();
        if worker.is_null() { default } else { worker }
    })
}

#[inline]
pub(crate) fn set_active_jit_fault(default: *mut JitCtx, fault: u32) {
    unsafe { (*active_jit_ctx(default)).fault = fault };
}

#[inline]
pub(crate) fn active_cow_bloom() -> ActiveCowBloom {
    crate::heap::WORKER_JIT_CTX.with(|slot| {
        let worker = slot.get();
        if worker.is_null() {
            ActiveCowBloom::Shared(COW_BLOOM_COUNTS.as_ptr())
        } else {
            ActiveCowBloom::Worker(unsafe { (*worker).cow_bloom_base as *mut u32 })
        }
    })
}

/// GILFREE-RINGS: arm/disarm the compiled COW branch on the ACTIVE JitCtx —
/// the block `WorkerJitCtxGuard` installed for THIS thread. `Heap::set_cow_armed`
/// is the fork COORDINATOR's twin: it writes the SHARED ctx while the main
/// thread is parked. The gilfree-CF lane has no such park (main keeps running
/// guest code under the GIL), so arming must stay strictly thread-private.
/// A no-op off a worker, which keeps the main thread on the inline fast path.
pub(crate) fn set_active_cow_armed(on: bool) {
    crate::heap::WORKER_JIT_CTX.with(|slot| {
        let worker = slot.get();
        if !worker.is_null() {
            unsafe { (*worker).cow_armed = on as u32 };
        }
    });
}

#[cfg(test)]
pub(crate) fn install_worker_jit_ctx_for_test(snapshot: JitCtx) -> WorkerJitCtxGuard {
    WorkerJitCtxGuard::install(snapshot)
}

#[cfg(test)]
pub(crate) fn set_jit_fault_for_test(default: &mut JitCtx, fault: u32) {
    set_active_jit_fault(default, fault);
}

#[cfg(test)]
pub(crate) fn active_jit_fault_for_test(default: &JitCtx) -> u32 {
    unsafe { (*active_jit_ctx(default as *const JitCtx as *mut JitCtx)).fault }
}

#[inline]
pub(crate) fn cow_bloom_note(object: u64, field: u32) {
    let index = cow_bloom_index(object, field);
    match active_cow_bloom() {
        ActiveCowBloom::Shared(base) => {
            unsafe { &*base.add(index) }.fetch_add(1, std::sync::atomic::Ordering::Release);
        }
        ActiveCowBloom::Worker(base) => unsafe {
            let counter = &mut *base.add(index);
            debug_assert_ne!(*counter, u32::MAX, "COW Bloom counter overflow");
            // Saturation cannot create a false negative. Reaching it requires
            // more live shadow entries than the bounded worker heap can hold.
            *counter = counter.saturating_add(1);
        },
    }
}

#[inline]
pub(crate) fn cow_bloom_remove(object: u64, field: u32) {
    let index = cow_bloom_index(object, field);
    let previous = match active_cow_bloom() {
        ActiveCowBloom::Shared(base) => unsafe { &*base.add(index) }
            .fetch_sub(1, std::sync::atomic::Ordering::Release),
        ActiveCowBloom::Worker(base) => unsafe {
            let counter = &mut *base.add(index);
            let previous = *counter;
            *counter = counter.saturating_sub(1);
            previous
        },
    };
    debug_assert!(previous > 0, "unbalanced COW Bloom removal");
}

#[cfg(test)]
pub(crate) fn cow_bloom_maybe(object: u64, field: u32) -> bool {
    let index = cow_bloom_index(object, field);
    match active_cow_bloom() {
        ActiveCowBloom::Shared(base) => unsafe { &*base.add(index) }
            .load(std::sync::atomic::Ordering::Acquire)
            != 0,
        ActiveCowBloom::Worker(base) => unsafe { *base.add(index) != 0 },
    }
}
