//! WHICH CALLS CROSS THE COMPILED→HOST SEAM (`RAVA_JIT_CB_CENSUS`, default OFF).
//!
//! Current coverage: `jit_call_body` records only calls reaching its general
//! marshalling path, after the direct/native fast returns. This is a fallback
//! population, not all compiled-to-host transitions. Names merge overloads and
//! kinds and are sampled every 256th event; the aggregate counts recorded events.
//! Periodic reports are cumulative and have no capture timestamp, so their last
//! printed value is not an exact launch-to-Done total.
//!
//! Row 438 priced the seam at ~14% of the product boot and counted **57,698,583 callbacks, nine
//! per compiled method entry** — but a count is not a target. A typed direct-call path has to be
//! built for a NAMED family of callees, and nothing in the engine could say which families those
//! are. This says it: `(owner, name, kind)` tallied per callback, sampled into the per-name map
//! the same 1-in-256 way the birth census is (a lock on every one of 57M events is not a probe,
//! it is a different program), with the aggregate exact.
//!
//! ⚠Diagnostic only. Nothing routes on it — it exists to choose what to build.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

static TOTAL: AtomicU64 = AtomicU64::new(0);
static BY_KIND: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// Total crossings, for the diagnostics registry.
pub fn total() -> u64 {
    TOTAL.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_CB_CENSUS").is_some()
            || crate::diag::selected("seam", "crossings"))
}

fn names() -> &'static std::sync::Mutex<crate::FastMap<String, u64>> {
    static M: std::sync::OnceLock<std::sync::Mutex<crate::FastMap<String, u64>>> =
        std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(crate::FastMap::default()))
}

pub(crate) fn note(owner: &str, name: &str, kind: u8) {
    let n = TOTAL.fetch_add(1, Relaxed);
    BY_KIND[(kind as usize).min(3)].fetch_add(1, Relaxed);
    if n % 256 != 0 {
        return;
    }
    // Look up before allocating: `entry(format!(..))` would build a String on every sample.
    let mut g = names().lock().unwrap_or_else(|e| e.into_inner());
    let key = format!("{owner}.{name}");
    match g.get_mut(&key) {
        Some(c) => *c += 1,
        None => {
            g.insert(key, 1);
        }
    }
}

/// The callback population, ranked. `kind`: 0 static, 1 virtual, 2 special, 3 interface.
pub fn report(top: usize) -> String {
    let g = names().lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<(&String, &u64)> = g.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    let sampled: u64 = rows.iter().map(|(_, n)| **n).sum();
    let mut out = format!(
        "[JIT-CB] {} callbacks | static={} virtual={} special={} interface={} | {} distinct callees",
        TOTAL.load(Relaxed),
        BY_KIND[0].load(Relaxed),
        BY_KIND[1].load(Relaxed),
        BY_KIND[2].load(Relaxed),
        BY_KIND[3].load(Relaxed),
        rows.len()
    );
    for (k, n) in rows.iter().take(top) {
        out.push_str(&format!(
            "\n[JIT-CB]   {:>6.2}%  {:>8}  {k}",
            **n as f64 * 100.0 / sampled.max(1) as f64,
            **n
        ));
    }
    out
}
