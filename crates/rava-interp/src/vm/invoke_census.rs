//! ⭐**WHO IS ON THE FAR SIDE OF THE 48.8 MILLION CROSSINGS.**
//!
//! `[CB] invoke=48,845,846` is the exact count of times compiled machine code leaves for the host
//! to perform a call, and it did not move when class-hierarchy analysis admitted 8,240 sites — so
//! the population that matters is not the one the inliner argues about. This names it.
//!
//! Diagnostics only (`RAVA_INVOKE_CENSUS=1`), and inert otherwise: one relaxed bool read on the
//! crossing path. Nothing here is ever consulted by an admission or routing decision — the project
//! rule forbids routing on names, and this exists to be READ by a person, not by the engine.

use std::sync::Mutex;

fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_INVOKE_CENSUS").is_some())
}

static TABLE: Mutex<Option<crate::FastMap<Box<str>, u64>>> = Mutex::new(None);

/// Count one crossing by the `owner.name desc` its constant-pool entry names.
pub(crate) fn note(caller_ptr: i64, cp: u16) {
    if !enabled() || caller_ptr == 0 {
        return;
    }
    // SAFETY: the JIT passes the same `&LoadedClass` pointer every other callback dereferences,
    // and the class outlives the compiled body that names it.
    let caller = unsafe { &*(caller_ptr as *const crate::class_loader::LoadedClass) };
    let Some(m) = caller.file.pool.member_ref(cp) else { return };
    let key = format!("{}.{}{}", m.owner, m.name, m.descriptor);
    let mut g = TABLE.lock().unwrap_or_else(|e| e.into_inner());
    let t = g.get_or_insert_with(Default::default);
    *t.entry(key.into_boxed_str()).or_insert(0) += 1;
}

/// The heaviest callees, most-crossed first. Empty unless armed.
pub(crate) fn report() -> String {
    if !enabled() {
        return String::new();
    }
    let g = TABLE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(t) = g.as_ref() else { return String::new() };
    let mut rows: Vec<(&Box<str>, &u64)> = t.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    let total: u64 = t.values().sum();
    let mut out = format!("[INVCEN] {total} crossings over {} distinct callees\n", t.len());
    for (k, n) in rows.iter().take(30) {
        out.push_str(&format!("[INVCEN] {:>10}  {:>5.2}%  {k}\n", n, 100.0 * **n as f64 / total as f64));
    }
    out
}
