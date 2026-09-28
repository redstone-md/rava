//! Increment 14.3 IC-miss anatomy (RAVA_IC_VTABLE_FALLBACK): before committing any codegen to the
//! compiled dispatch-miss path, break the 128.8M rava_dispatch_miss re-entries into categories so
//! the fix targets the real cost. Rava has NO slot-indexed vtable — dispatch is class_id→4-way PIC,
//! miss → resolve BY NAME+DESC (tlcache-memoized) → fill PIC → jit_call. So the only fixable fraction
//! is expensive RE-RESOLVES of already-seen (site,class) pairs evicted from the 4-way PIC; the
//! NeverCacheable (java/native-served) majority has no compiled fptr and is irreducible (routes
//! through jit_call regardless). This probe counts each category + distinct (site,class) pairs to
//! quantify re-resolve thrash vs the irreducible floor. Gated, side-effect-only ⇒ math inert.
//! ENUM BIRTH-ROUTE CENSUS (`RAVA_ENUM_BIRTH_CENSUS`, default OFF).
//!
//! `RAVA_ARENA_ENUM` is built, documented, and INERT: armed vs disarmed on a real boot the
//! blocking-cell census is 723,327 vs 724,131 (noise) and the per-target histogram is
//! byte-identical, so not one enum migrates. Both birth sites do consult the flag, which rules
//! out the obvious explanation, and the remaining suspects are the OTHER terms of the same
//! condition — `native_super`, and `!initialized` on the JIT twin.
//!
//! `!initialized` is the strong candidate, because enum CONSTANTS are allocated inside their own
//! class's `<clinit>`, when the class is by definition not yet marked initialised. But that is an
//! argument, and this session has already paid for three of those (a stale-binary frame reading, a
//! single-run inlining "win", and a dispatch table whose price model was wrong). So count which
//! term actually fires before touching the condition.
//!
//! Counts only classes that LOOK like enums (ACC_ENUM, or a direct super of java/lang/Enum), so
//! the tallies are not diluted by ordinary allocation. Prints every `STEP` births because a boot
//! measured to Done is killed and never reaches a clean-exit dump — the third readout in this tree
//! to be wired to a path the boot does not take.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Which term of `is_enum || native_super || !initialized` forced the handle.
pub static BY_ENUM_FLAG: AtomicU64 = AtomicU64::new(0);
pub static BY_NATIVE_SUPER: AtomicU64 = AtomicU64::new(0);
pub static BY_UNINIT: AtomicU64 = AtomicU64::new(0);
/// Reached the arena path — i.e. actually migrated.
pub static ARENA: AtomicU64 = AtomicU64::new(0);
/// Counted as ARENA above and then handed a HANDLE anyway. `note` sits at the birth
/// DECISION, but the arena commit is ~65 lines further down and one escape survives in
/// between: `class_field_layout` only caches a layout when the whole super chain is
/// loadable, and an uncached layout falls back to `Value::Ref`. So "arena" in the route
/// tally means "passed the gate", not "is an arena object" — which is exactly how all four
/// 30,837-cell targets could read as migrated while not one of their cells moved.
pub static BY_LAYOUT: AtomicU64 = AtomicU64::new(0);

pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ENUM_BIRTH_CENSUS").is_some())
}

/// 4, not a large round number. The population is TINY: an enum class has a handful of
/// CONSTANTS, so the 30,837 cells the blocking census attributes to e.g. PushReaction are
/// cells POINTING AT about seven objects. Migrating a few hundred objects is what would flip
/// ~101k cells. A 4096-step printed nothing at all on a full boot, which is how the scale of
/// this population was discovered, and a 32-step still printed nothing for the JIT site --
/// which is a COMPILE-TIME, once-per-`new`-site check, so its population is smaller again
/// (a handful of sites per enum class). Twice now the threshold has hidden the answer.
/// 65536, not 4. It WAS 4, correctly, while this census was filtered to enum-shaped
/// classes — a population of a few thousand births, where a 4096-step printed nothing at
/// all and a 32-step still printed nothing for the JIT site. Unfiltered the same constant
/// means an `eprintln!` every four OBJECT BIRTHS, i.e. millions of formatted lines to a
/// redirected file, and that — not the map lock, and not the substring scan — is what
/// kept the census boot from reaching Done inside a 400 s timeout. ★A threshold is only
/// meaningful against the population it was chosen for; widening the population silently
/// re-tunes every constant that was calibrated on it.
const STEP: u64 = 65536;

/// Distinct CLASS NAMES per route. The counts alone cannot answer the question that matters:
/// 936 enum constants are arena-born, yet the blocking-cell census still attributes 30,837
/// cells to NoteBlockInstrument and 30,837 to PushReaction. Either those classes are NOT among
/// the 936 (so something else mints them), or they ARE and their cells block for an unrelated
/// reason. A name list settles which, and no further hypothesis can.
/// route -> class -> births. A SET of names could only answer "did this class ever take
/// this route", which was enough while the census was filtered to enum-shaped classes (a
/// handful of constants each). Unfiltered it has to rank: `$Lambda`, gson's JsonObject/
/// JsonPrimitive, BitSetDiscreteVoxelShape and the fastutil collections are the remaining
/// 790 k blocking cells, and only a per-class COUNT says which handle-forcing term to
/// attack next.
type RouteMap = std::collections::BTreeMap<&'static str, std::collections::BTreeMap<String, u64>>;
fn names() -> &'static std::sync::Mutex<RouteMap> {
    static M: std::sync::OnceLock<std::sync::Mutex<RouteMap>> = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

/// WHICH super breaks the chain. `class_field_layout` walks to `java/lang/Object`; any
/// super it cannot load leaves the layout PARTIAL, which is uncacheable, which sends the
/// object to a handle — and, for enums, also skips the `$enumName`/`$enumOrd` injection,
/// since that is keyed on the unloadable class appearing in the chain. One name per
/// terminator ranks the whole 68,508-birth fallback by cause instead of by symptom.
fn terminators() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, u64>> {
    static M: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, u64>>> =
        std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

pub fn note_layout_terminator(sup: &str) {
    if !enabled() {
        return;
    }
    let mut g = terminators().lock().unwrap_or_else(|e| e.into_inner());
    match g.get_mut(sup) {
        Some(n) => *n += 1,
        None => {
            g.insert(sup.to_string(), 1);
        }
    }
}

/// Late escape: the class passed the birth gate but its layout was not cached, so it is
/// still handed a handle. Recorded under its own route so the tally distinguishes
/// "gate said arena" from "object is arena".
pub fn note_layout_fallback(class: &str) {
    if !enabled() {
        return;
    }
    {
        let mut g = names().lock().unwrap_or_else(|e| e.into_inner());
        *g.entry("layout_uncached").or_default().entry(class.to_string()).or_insert(0) += 1;
    }
    BY_LAYOUT.fetch_add(1, Relaxed);
}

/// `site` distinguishes the interpreter birth path from its JIT twin, since only the twin
/// carries the `!initialized` term and a difference between them is itself the answer.
pub fn note(
    site: &'static str,
    class: &str,
    by_enum: bool,
    by_native_super: bool,
    by_uninit: bool,
) {
    if !enabled() {
        return;
    }
    let (c, route) = if by_enum {
        (&BY_ENUM_FLAG, "enum_flag")
    } else if by_native_super {
        (&BY_NATIVE_SUPER, "native_super")
    } else if by_uninit {
        (&BY_UNINIT, "uninit")
    } else {
        (&ARENA, "arena")
    };
    // SAMPLE the per-class map. Unfiltered, this census sees EVERY object birth, and a
    // global mutex on each one stopped the boot from reaching Done inside a 400 s
    // timeout — the aggregate counters below are atomics and cost nothing, but the map
    // is a lock. Sampling 1-in-256 preserves the RANKING (uniform sampling is unbiased
    // in the class proportions, which is all this map is read for) while removing 255 of
    // every 256 lock acquisitions. The aggregate route totals stay EXACT.
    let sampled = c.load(Relaxed) % 256 == 0;
    if sampled {
        let mut g = names().lock().unwrap_or_else(|e| e.into_inner());
        // Look up BEFORE allocating. `entry(class.to_string())` allocates a String on
        // every object birth, not just the first — with the census unfiltered that is
        // millions of allocations under a global lock, and it stopped the boot from
        // reaching Done inside the harness timeout at all. A diagnostic that cannot
        // finish the run it is diagnosing measures nothing.
        let e = g.entry(route).or_default();
        match e.get_mut(class) {
            Some(n) => *n += 1,
            None => {
                e.insert(class.to_string(), 1);
            }
        }
    }
    let n = c.fetch_add(1, Relaxed) + 1;
    if n % STEP == 0 {
        eprintln!(
            "[ENUM-BIRTH] {site}: enum_flag={} native_super={} uninit={} arena={} \
             (of which layout_uncached->handle {})",
            BY_ENUM_FLAG.load(Relaxed),
            BY_NATIVE_SUPER.load(Relaxed),
            BY_UNINIT.load(Relaxed),
            ARENA.load(Relaxed),
            BY_LAYOUT.load(Relaxed),
        );
    }
}

/// Dump the distinct class names per route. Called from the cell census, which already prints
/// on a path a killed boot reaches.
pub fn dump_names() {
    if !enabled() {
        return;
    }
    {
        let t = terminators().lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(&String, &u64)> = t.iter().collect();
        rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        eprintln!("[ENUM-BIRTH-CHAIN] {} distinct unloadable supers break a layout chain", rows.len());
        for (sup, n) in rows.iter().take(20) {
            eprintln!("[ENUM-BIRTH-CHAIN]   {n:>8}  {sup}");
        }
    }
    let g = names().lock().unwrap_or_else(|e| e.into_inner());
    for (route, set) in g.iter() {
        let total: u64 = set.values().sum();
        eprintln!("[ENUM-BIRTH-NAMES] {route}: {} distinct classes, {total} births", set.len());
        let mut rows: Vec<(&String, &u64)> = set.iter().collect();
        rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        // Ranked by BIRTHS, and any cap therefore drops the smallest. The earlier version
        // capped an alphabetical BTreeSet at 40, which stopped at
        // `net/minecraft/core/AxisCycle$3` and cut every `net/minecraft/world/**` name —
        // including the two 30,837-cell targets the census existed to settle. An absent
        // name then reads as "never born here" when it only means "sorts late".
        for (n, c) in rows.iter().take(25) {
            eprintln!("[ENUM-BIRTH-NAMES]   {route} {c:>8}  {n}");
        }
    }
}
