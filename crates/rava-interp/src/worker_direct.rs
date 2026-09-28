//! A gil-free worker's OWN direct-call cache.
//!
//! ## Why a worker had none
//!
//! `jit_direct_call`'s miss arm opens with `if gilfree() { return None }`, and the worker arm
//! (`jit_direct_call_worker`) mirrors it: probe the shared per-site cache, and on anything but a
//! hit, give up. The stated reason is that the fill mutates the shared `jit_direct_cache`, which a
//! worker may not touch. That is true of the FILL. It was never true of the RESOLVE.
//!
//! ## What it costs
//!
//! Static and invokespecial sites are *structurally* non-PIC-able — there is no receiver to guard
//! on, so the machine-code polymorphic cache cannot serve them at all and the per-site direct
//! cache is the only mechanism that can. A worker that may never build an entry therefore falls
//! into the `jit_call` gauntlet for every one of them, and off-GIL that means taking the global
//! dispatch lock. On a completing ring region the census reads **4.04 M plain invokes**
//! (`static 2195771`, `special 1844015`), and the three biggest locked callees —
//! `Climate#target`, `DensityFunction$SinglePointContext#<init>`,
//! `DensityFunctions$Spline$Point#<init>` — are exactly this shape.
//!
//! Warming main does not fix it, and that is measured rather than assumed: `RAVA_PILOT_N=4`, four
//! whole ring tasks executed serially before the region opens, leaves the locked-callee census
//! unchanged. Main fills the sites MAIN reaches.
//!
//! ## Why this is sound
//!
//! Everything the fill needs is now `&self` or per-thread:
//!
//! * `jit_direct_resolve` takes `&self` (its last two `&mut` uses were `resolve_method`, which only
//!   consulted the shared-readable loader and a thread-local cache, and a per-call lambda scratch
//!   field that has moved to `jit_lambda_scratch`);
//! * static/invokespecial resolutions come from `crate::shared_call_cache`, published once and
//!   readable everywhere;
//! * virtual resolutions come from the per-thread resolve caches.
//!
//! The table itself is `thread_local!`, so a worker's entries are invisible to every other thread
//! and there is nothing to publish, invalidate or race. A `DirectEntry` is plain `Copy` data whose
//! only pointer is a compiled function address — immortal, since main's `JITModule` never frees.
//!
//! Cleared when a worker finishes a task, for the same reason `tl_worker` is: the entries name
//! compiled bodies that are valid for the region, and a bounded lifetime is what keeps them from
//! outliving anything they depend on.

use std::cell::RefCell;

use crate::jit::DirectEntry;

thread_local! {
    /// `(site, receiver guard)` → the resolved entry, or `None` for a resolution that refused.
    /// The negative is cached too: re-resolving a permanently-unservable callee on every call
    /// would cost more than the locked path this exists to avoid.
    static CACHE: RefCell<crate::FastMap<((usize, u16), usize), Option<DirectEntry>>> =
        RefCell::new(crate::FastMap::default());
}

pub(crate) fn get(site: (usize, u16), guard: usize) -> Option<Option<DirectEntry>> {
    CACHE.with(|c| c.borrow().get(&(site, guard)).copied())
}

pub(crate) fn put(site: (usize, u16), guard: usize, entry: Option<DirectEntry>) {
    CACHE.with(|c| {
        let mut m = c.borrow_mut();
        // Bounded: a worker runs a handful of task shapes, so this saturates in the low
        // thousands. The cap is a runaway guard, not a policy — dropping an entry costs a
        // locked dispatch, never correctness.
        if m.len() < 1 << 16 {
            m.insert((site, guard), entry);
        }
    });
}

/// Drop this thread's table. Called when a worker finishes a task.
pub(crate) fn clear() {
    CACHE.with(|c| c.borrow_mut().clear());
}

/// `RAVA_NO_WORKER_DIRECT_FILL` restores the previous behaviour — a worker probes the shared cache
/// and gives up on a miss.
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_WORKER_DIRECT_FILL").is_none())
}
