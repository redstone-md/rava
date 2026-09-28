//! Base-address bridges: constants the compiled prologue asks for once, not per operation.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::jit::*;


/// Detached Deep-IC context lookup. The normal GIL-held path uses the baked
/// per-VM slot with no callback; a COW-armed worker calls this to obtain its own
/// TLS root stack instead of racing that shared publication slot.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_jit_ctx_addr() -> i64 {
    crate::frames::ctx_addr() as i64
}
/// Inline-TLAB-bump cursor address (next-gen-gc). Called ONCE at the entry of a
/// compiled method that inline-allocates: ensures the thread TLAB exists, syncs the
/// `[next,end]` cursor to the live nursery extent, and returns its stable address.
/// No GC, no allocation — pure setup, so it is NOT counted as a per-op callback.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_jit_cursor_addr() -> i64 {
    crate::nextgen::jit_cursor_addr()
}

/// 11.3 — the compiled dispatch site's per-thread IC base fetch. Returns this thread's private IC
/// table base, or 0 on the main thread / outside a region (→ the site falls back to the baked global
/// cache). A pure thread-local read; no `vm_ref()`. Called once per site (top of the PIC block) only
/// when `ic_parallel_enabled()`.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_ic_base() -> i64 {
    per_thread_ic_base() as i64
}

/// 23.4 heap-Ref PIC: the CURRENT base of `Heap.objects` (`Vec<Box<Object>>`). Called by the machine-
/// code PIC each dispatch (never baked) so a realloc / VM relocation is always reflected — the memory-
/// safety guarantee the user mandates. Leaf, no side effects, no allocation.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_heap_objects_base() -> i64 {
    // GAP-S09 stage 2: `&Vm` (vm_shared) — the body is one `&self` base-address
    // read (`Heap::objects_base_ptr`), no fault path, no allocation.
    match vm_shared() {
        Some(vm) => vm.heap.objects_base_ptr(),
        None => 0,
    }
}

/// 23.4a heap-Ref PIC SHADOW callback (inert): verify the machine-code-extracted class key against the
/// authoritative Rust `Object.class` word. `recv` is the raw Ref cell (handle | REF_TAG). Read-only;
/// never influences dispatch.
#[cfg(feature = "next-gen-gc")]
pub(crate) extern "C" fn rava_heap_pic_shadow(recv: i64, machine_key: i64) -> i64 {
    // GAP-S09 stage 2: `&Vm` (vm_shared) — `shadow_check` takes `&Heap` and only
    // compares/logs; the doc contract above ("Read-only; never influences
    // dispatch") is now signature-enforced.
    if crate::heappic::shadow_enabled() {
        if let Some(vm) = vm_shared() {
            let handle = (recv & (crate::value::REF_TAG - 1)) as u32;
            crate::heappic::shadow_check(&vm.heap, handle, machine_key);
        }
    }
    0
}
