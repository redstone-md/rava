//! 18.4 M4 DATAPACK PARALLEL TOKENIZE-PREPASS (RAVA_M4_DATAPACK_PARALLEL). The 18.3 landscape proved
//! the datapack parse phase does ZERO steady-state static writes and that JSON tokenization is a PURE
//! function of the file text (no guest heap / no VM). So the safe, deterministic-BY-CONSTRUCTION split
//! is: on the first datapack parse, natively enumerate `/data/**.json`, read every file, and tokenize
//! them ALL in parallel across pool-many pure-Rust threads into `Json` ASTs cached by content; the guest
//! loop's serial per-file `build` then pulls its pre-tokenized AST from the cache (identical AST ⇒
//! identical `JsonElement` ⇒ identical DataPack_HASH). Tokenize touches neither the guest heap, the
//! NurseryView, Shadow-COW, nor the GIL — it is lock-free over disjoint result slots, so no worker can
//! race or re-enter the VM. The heap-allocating `build` stays strictly serial on the guest thread.

use super::gson_native::Json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_M4_DATAPACK_PARALLEL").is_some())
}
static PREWARMED: AtomicBool = AtomicBool::new(false);
static CACHE: Mutex<Option<HashMap<String, Json>>> = Mutex::new(None);
static FILES: AtomicU64 = AtomicU64::new(0); // json files tokenized in the prepass
static PREPASS_US: AtomicU64 = AtomicU64::new(0); // parallel-tokenize wall time (µs)
static HITS: AtomicU64 = AtomicU64::new(0); // serial builds served from the prepass cache
static MISSES: AtomicU64 = AtomicU64::new(0); // builds that fell back to inline tokenize

/// Set once so the prepass runs a single time. Returns true the FIRST caller (which then runs it).
pub fn claim_prewarm() -> bool {
    PREWARMED
        .compare_exchange(false, true, Relaxed, Relaxed)
        .is_ok()
}
pub fn store(map: HashMap<String, Json>, files: u64, prepass_us: u64) {
    FILES.store(files, Relaxed);
    PREPASS_US.store(prepass_us, Relaxed);
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(map);
}
/// Take the pre-tokenized AST for `text` (removed so memory frees as the serial loop drains it).
/// `None` ⇒ not in the cache (a non-/data file or a repeat) ⇒ caller tokenizes inline.
pub fn take(text: &str) -> Option<Json> {
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let hit = g.as_mut().and_then(|m| m.remove(text));
    if hit.is_some() {
        HITS.fetch_add(1, Relaxed);
    } else {
        MISSES.fetch_add(1, Relaxed);
    }
    hit
}
pub fn dump_if_on() {
    if !enabled() {
        return;
    }
    eprintln!(
        "[DATAPACK-PREPASS] files_tokenized={} parallel_tokenize_ms={} serial_builds: cache-HIT={} inline-MISS={}",
        FILES.load(Relaxed),
        PREPASS_US.load(Relaxed) / 1000,
        HITS.load(Relaxed),
        MISSES.load(Relaxed),
    );
}
