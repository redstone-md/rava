//! WHICH REFUSAL KEEPS A METHOD OUT OF THE COMPILER (`compile_bail`, always on, one relaxed add).
//!
//! ⭐A product boot enters **15,212,176 interpreted method bodies against 7,712,120 compiled
//! ones** and compiles only ~1,500 distinct methods at a cap of 4,096. The cap is therefore not
//! what binds — a refusal is, and until this existed the refusals were printed one line at a time
//! under a debug gate and never counted. Reported from the periodic sampler dump beside the
//! other censuses, never from a teardown a server boot cannot reach.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub(crate) const STAGES: [&str; 6] =
    ["exceptions", "callback_heavy", "find_leaders", "resolve_fields", "emit_body", "ir_budget"];
static N: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
static OK: AtomicU64 = AtomicU64::new(0);

#[inline]
pub(crate) fn note(stage: &str) {
    if let Some(i) = STAGES.iter().position(|s| *s == stage) {
        N[i].fetch_add(1, Relaxed);
    }
}

/// A compile that reached the end and produced a body.
#[inline]
pub(crate) fn note_ok() {
    OK.fetch_add(1, Relaxed);
}

/// Methods that reached `jit_run` at all, and the two refusals that turn them away BEFORE a
/// compile is ever attempted. ⭐15,038 distinct methods have entries in a product boot and the
/// compiler is asked 1,951 times; these say where the other thirteen thousand go.
pub(crate) static SEEN: AtomicU64 = AtomicU64::new(0);

#[inline]
pub(crate) fn note_seen() {
    SEEN.fetch_add(1, Relaxed);
}

pub fn report() -> String {
    let ok = OK.load(Relaxed);
    let bail: u64 = N.iter().map(|c| c.load(Relaxed)).sum();
    if ok + bail == 0 {
        return String::new();
    }
    let tot = ok + bail;
    let mut out = format!(
        "[CBAIL] {} jit_run passages, {} refused before any attempt (native-served/cache-class) | {tot} compile attempts, {ok} COMPILED ({:.1}%), {bail} refused",
        SEEN.load(Relaxed),
        crate::NATIVE_SERVED_REFUSED.load(Relaxed) as u64,
        ok as f64 * 100.0 / tot as f64
    );
    let mut rows: Vec<(u64, &str)> =
        N.iter().zip(STAGES).map(|(c, s)| (c.load(Relaxed), s)).filter(|r| r.0 > 0).collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    for (n, s) in rows {
        out.push_str(&format!(
            "
[CBAIL]   {:>6.2}%  {n:>8}  {s}",
            n as f64 * 100.0 / tot as f64
        ));
    }
    out
}
