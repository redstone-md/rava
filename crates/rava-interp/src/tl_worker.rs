//! `java.lang.ThreadLocal` that is actually thread-local — for the threads that need it.
//!
//! ## The bug this fixes
//!
//! Rava models a `ThreadLocal`'s value as two synthetic fields on the ThreadLocal OBJECT
//! (`$tlval` / `$tlset`, see `reflect::threadlocal`). That is indistinguishable from correct
//! while exactly one thread runs guest code, which was true for the whole engine until the
//! gil-free lane. It is wrong the moment two do: `ThreadLocal` exists precisely so that two
//! threads reading the same object see DIFFERENT values, and MC's worldgen uses it for exactly
//! that — per-thread scratch state.
//!
//! The ring lane runs six workers over one shared graph and the locked-callee census puts
//! `ThreadLocal#get` + `ThreadLocal#set` at 825,246 calls per region. Under the dispatch lock
//! those interleave one at a time, so a task's own `set` → `get` pair still round-trips and the
//! model survives by accident. Remove the lock and six workers clobber one cell — which is a
//! plausible reading of why the unlocked region dies with guest exceptions at a point that moves
//! from run to run.
//!
//! ## Why a per-thread side table and not a rework of the fields
//!
//! The object fields stay exactly as they are, and the non-gil-free path never consults this
//! module — so the single-threaded engine is byte-identical and the GC keeps rooting the value
//! through the object, as it always has.
//!
//! A gil-free worker instead reads and writes here, keyed by the ThreadLocal's identity. The
//! entries live only for the region, and that is what makes them safe without being GC roots: a
//! worker never collects (a nursery overflow aborts the region instead), so nothing can be swept
//! while a value sits in this table. `clear()` runs at region teardown.
//!
//! A `get` that misses falls through to the object fields — the value main published before the
//! region opened is the correct initial value for a worker that has not written its own, and it
//! is read-only from here.

use crate::value::Value;

thread_local! {
    static TL: std::cell::RefCell<crate::FastMap<u32, Value>> =
        std::cell::RefCell::new(crate::FastMap::default());
}

/// This thread's value for ThreadLocal `recv`, if it has set one during this region.
pub(crate) fn get(recv: u32) -> Option<Value> {
    TL.with(|t| t.borrow().get(&recv).cloned())
}

/// Publish this thread's value for ThreadLocal `recv`.
pub(crate) fn set(recv: u32, v: Value) {
    TL.with(|t| {
        t.borrow_mut().insert(recv, v);
    });
}

/// `ThreadLocal.remove()` — drop this thread's entry so the next `get` recomputes.
pub(crate) fn remove(recv: u32) {
    TL.with(|t| {
        t.borrow_mut().remove(&recv);
    });
}

/// Drop this thread's whole table. Called at region teardown: the entries hold handles that are
/// only guaranteed live for the region.
pub(crate) fn clear() {
    TL.with(|t| t.borrow_mut().clear());
}

/// Is the per-thread table armed? `RAVA_NO_TL_PER_THREAD` disables it, restoring the shared-cell
/// model (which is correct only for a single guest thread — see the module doc).
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_TL_PER_THREAD").is_none())
}
