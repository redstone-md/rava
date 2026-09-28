//! POPULATION CENSUS for the two paths a boot bisect priced at ~11% of `Done`
//! each — the element-key derivation (`stream::helpers::ekey_mut`) and the
//! unmodifiable-contract probe (`immutable::kind_of`).
//!
//! WHY A CENSUS AND NOT A PROFILE. Ledger row 250 and `docs/how-to-measure.md`
//! rule 7: attribution does not predict wall. Three separate profile rows in this
//! tree named a symptom that several callers shared, and the obvious fix at each
//! was worth zero. What is NOT a guess is how many times a path runs and what
//! fraction of those runs do real work — a ratio decides whether a memo is worth
//! building before anything is built.
//!
//! The two questions this answers:
//!
//!   * `kind_of` probes a synthetic field that only the JDK's unmodifiable
//!     factories ever stamp. Once ANY `List.of` has run the field name is
//!     interned, so from then on every guarded call pays a real field probe. If
//!     the marked population is a rounding error against the probed one, the
//!     probe is a miss engine and wants a negative cache.
//!   * `ekey_mut` derives the cheap key BEFORE it consults its memo, so a memo
//!     hit throws a freshly built `String` away. If hits dominate, that
//!     derivation is pure waste and the order is the bug.
//!
//! Inert unless `RAVA_CONTRACT_STAT` is set, and the counters are `Relaxed`
//! atomics on paths that already touch the heap — `stream_map_stat` records what
//! happens when a census is not cheap enough (the boot went to 149 s and stopped
//! being the run anyone meant).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// COMPILE-TIME FIRST, then the env var. `classify` alone runs 4.75M times in a
/// boot, so even a `OnceLock` read and a branch is a cost the shipped product
/// should not carry for an instrument it is not using. `cfg!` is a constant here,
/// so without the feature LLVM folds this to `false` and deletes every counter,
/// every atomic and every call site body — while `--features contract-stat` still
/// gives back the full census for the price of one build.
#[inline(always)]
pub(crate) fn enabled() -> bool {
    if !cfg!(feature = "contract-stat") {
        return false;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_CONTRACT_STAT").is_some())
}

/// How often the census prints. The LAST line of each family is the answer; a
/// family that never prints has a population under this, which against the tens
/// of millions the other one reports is itself the finding. Low enough that a
/// short run still reports, high enough that the `eprintln` is not the census.
const EVERY: u64 = 250_000;

// --- immutable::kind_of ---------------------------------------------------------
static KIND_CALLS: AtomicU64 = AtomicU64::new(0);
/// Calls that happened after some factory interned the marker, i.e. the ones that
/// actually pay a field probe rather than a `OnceLock` read and a return.
static KIND_ARMED: AtomicU64 = AtomicU64::new(0);
/// Calls that found a contract. Everything else is a probe that missed.
static KIND_MARKED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn kind_of_probe(armed: bool, marked: bool) {
    if !enabled() {
        return;
    }
    let c = KIND_CALLS.fetch_add(1, Relaxed) + 1;
    if armed {
        KIND_ARMED.fetch_add(1, Relaxed);
    }
    if marked {
        KIND_MARKED.fetch_add(1, Relaxed);
    }
    if c % EVERY == 0 {
        let armed = KIND_ARMED.load(Relaxed);
        let marked = KIND_MARKED.load(Relaxed);
        eprintln!(
            "[CONTRACT] kind_of {c} calls | armed {armed} ({:.1}%) | marked {marked} ({:.4}% of armed)",
            armed as f64 * 100.0 / c as f64,
            if armed == 0 { 0.0 } else { marked as f64 * 100.0 / armed as f64 },
        );
    }
}

// --- stream::helpers::ekey_mut --------------------------------------------------
static EKEY_CALLS: AtomicU64 = AtomicU64::new(0);
/// The cheap key already answered — no escalation, no memo, no derivation.
static EKEY_CHEAP: AtomicU64 = AtomicU64::new(0);
/// The memo answered, and the cheap key built on the way in was discarded.
static EKEY_MEMO_HIT: AtomicU64 = AtomicU64::new(0);
/// A full `structural_key`/`identity_ekey` derivation ran.
static EKEY_DERIVED: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub(crate) enum EkeyOutcome {
    Cheap,
    MemoHit,
    Derived,
}

pub(crate) fn ekey_outcome(outcome: EkeyOutcome) {
    if !enabled() {
        return;
    }
    let c = EKEY_CALLS.fetch_add(1, Relaxed) + 1;
    match outcome {
        EkeyOutcome::Cheap => &EKEY_CHEAP,
        EkeyOutcome::MemoHit => &EKEY_MEMO_HIT,
        EkeyOutcome::Derived => &EKEY_DERIVED,
    }
    .fetch_add(1, Relaxed);
    if c % EVERY == 0 {
        let (cheap, hit, der) = (
            EKEY_CHEAP.load(Relaxed),
            EKEY_MEMO_HIT.load(Relaxed),
            EKEY_DERIVED.load(Relaxed),
        );
        eprintln!(
            "[CONTRACT] ekey_mut {c} calls | cheap {cheap} ({:.1}%) | memo-hit {hit} ({:.1}%, each discards a built key) | derived {der} ({:.1}%)",
            cheap as f64 * 100.0 / c as f64,
            hit as f64 * 100.0 / c as f64,
            der as f64 * 100.0 / c as f64,
        );
    }
}

// --- immutable::classify --------------------------------------------------------
// Runs on EVERY collection and map call to decide whether the call is guarded at
// all, so its population is strictly larger than `kind_of`'s. Row 261 left it as
// the next candidate rather than the next guess.
static CLASSIFY_CALLS: AtomicU64 = AtomicU64::new(0);
/// Calls that named a mutator. Everything else walked the match to reach `None`.
static CLASSIFY_SOME: AtomicU64 = AtomicU64::new(0);

pub(crate) fn classify_call(matched: bool) {
    if !enabled() {
        return;
    }
    let c = CLASSIFY_CALLS.fetch_add(1, Relaxed) + 1;
    if matched {
        CLASSIFY_SOME.fetch_add(1, Relaxed);
    }
    if c % EVERY == 0 {
        let some = CLASSIFY_SOME.load(Relaxed);
        eprintln!(
            "[CONTRACT] classify {c} calls | mutator {some} ({:.2}%) | reached None {} ({:.2}%)",
            some as f64 * 100.0 / c as f64,
            c - some,
            (c - some) as f64 * 100.0 / c as f64,
        );
    }
}

// --- stream::helpers::ekey ------------------------------------------------------
// `c38b520` routed the element key through `map::key_str`, which is the map side's
// full type-tagged derivation. Row 260 priced that commit at +11.75% of Done and
// row 261's fix reached only the `ekey_mut` memo above it, so what remains is how
// often this runs and how much text it builds.
static EKEY_CALLS_RAW: AtomicU64 = AtomicU64::new(0);
static EKEY_BYTES: AtomicU64 = AtomicU64::new(0);

pub(crate) fn ekey_built(len: usize) {
    if !enabled() {
        return;
    }
    let c = EKEY_CALLS_RAW.fetch_add(1, Relaxed) + 1;
    EKEY_BYTES.fetch_add(len as u64, Relaxed);
    if c % EVERY == 0 {
        let b = EKEY_BYTES.load(Relaxed);
        eprintln!(
            "[CONTRACT] ekey {c} calls | {b} bytes of key text built ({:.1} B/call, {:.1} MB total)",
            b as f64 / c as f64,
            b as f64 / (1024.0 * 1024.0),
        );
    }
}

// --- immutable::republish_backing -----------------------------------------------
// Sits on the GENERAL collection write path (`stream::store_elems`), and probes
// `$views` the same way `kind_of` probes `$immutable`. Same question, same shape
// of answer: if the population with views is a rounding error, the probe wants
// the same negative filter.
static VIEWS_CALLS: AtomicU64 = AtomicU64::new(0);
static VIEWS_ARMED: AtomicU64 = AtomicU64::new(0);
static VIEWS_FOUND: AtomicU64 = AtomicU64::new(0);

pub(crate) fn views_probe(armed: bool, found: bool) {
    if !enabled() {
        return;
    }
    let c = VIEWS_CALLS.fetch_add(1, Relaxed) + 1;
    if armed {
        VIEWS_ARMED.fetch_add(1, Relaxed);
    }
    if found {
        VIEWS_FOUND.fetch_add(1, Relaxed);
    }
    if c % EVERY == 0 {
        let (armed, found) = (VIEWS_ARMED.load(Relaxed), VIEWS_FOUND.load(Relaxed));
        eprintln!(
            "[CONTRACT] republish {c} calls | armed {armed} ({:.1}%) | has-views {found} ({:.4}% of armed)",
            armed as f64 * 100.0 / c as f64,
            if armed == 0 { 0.0 } else { found as f64 * 100.0 / armed as f64 },
        );
    }
}

// --- Objects.equals ---------------------------------------------------------------
// Ledger row 279 attributed +13.89% of the boot's FIRST BLOCK to the commit that made
// this re-enter the interpreter, and the `equals_kind` fastpath aimed at it measured
// ZERO (row 281). Before guessing why, count: how many calls reach here, how many the
// cheap tests already answer, how many the Identity fastpath takes, and how many still
// run a guest frame.
static OE_CALLS: AtomicU64 = AtomicU64::new(0);
/// Print period for the `Objects.equals` line. Fixed at 50,000 it never fired on
/// `bench/ShapeOr.java` — a run whose bridge table blames that method for 68% of its native
/// time — so the one measurement that would have named the branch was the one not taken.
/// `RAVA_CONTRACT_EVERY=<n>` lowers it.
fn objects_equals_every() -> u64 {
    static N: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_CONTRACT_EVERY")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(50_000)
    })
}

static OE_QUICK: AtomicU64 = AtomicU64::new(0);
static OE_IDENTITY: AtomicU64 = AtomicU64::new(0);
static OE_GUEST: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub(crate) enum EqOutcome {
    Quick,
    Identity,
    Guest,
}

pub(crate) fn objects_equals(outcome: EqOutcome) {
    if !enabled() {
        return;
    }
    let c = OE_CALLS.fetch_add(1, Relaxed) + 1;
    match outcome {
        EqOutcome::Quick => &OE_QUICK,
        EqOutcome::Identity => &OE_IDENTITY,
        EqOutcome::Guest => &OE_GUEST,
    }
    .fetch_add(1, Relaxed);
    if c % objects_equals_every() == 0 {
        eprintln!(
            "[CONTRACT] Objects.equals {c} calls | quick {} ({:.1}%) | identity-fastpath {} ({:.1}%) | GUEST FRAME {} ({:.1}%)",
            OE_QUICK.load(Relaxed), OE_QUICK.load(Relaxed) as f64 * 100.0 / c as f64,
            OE_IDENTITY.load(Relaxed), OE_IDENTITY.load(Relaxed) as f64 * 100.0 / c as f64,
            OE_GUEST.load(Relaxed), OE_GUEST.load(Relaxed) as f64 * 100.0 / c as f64,
        );
    }
}

// --- synthetic field stamps --------------------------------------------------------
// Row 282: the product spends LESS cpu than before the session and MORE wall, with
// rss +127 MB beside it — which points at memory rather than computation. The
// correctness work stamps synthetic fields (`$immutable`, `$blen`, `$started`,
// `$interrupted`, `$uncaught`, the contract markers), and each one widens an object.
// Count them by NAME before assuming which.
static SYN_CALLS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn synth_stamp(name: &str) {
    if !enabled() || !name.starts_with('$') {
        return;
    }
    use std::sync::Mutex;
    static TALLY: std::sync::OnceLock<Mutex<std::collections::BTreeMap<String, u64>>> =
        std::sync::OnceLock::new();
    let tally = TALLY.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()));
    {
        let mut t = tally.lock().unwrap_or_else(|e| e.into_inner());
        *t.entry(name.to_string()).or_insert(0) += 1;
    }
    let c = SYN_CALLS.fetch_add(1, Relaxed) + 1;
    if c % 2_000_000 == 0 {
        let t = tally.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(&String, &u64)> = t.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        let top: Vec<String> =
            rows.iter().take(30).map(|(k, v)| format!("{k}={v}")).collect();
        eprintln!("[CONTRACT] synthetic stamps {c} | distinct {} | top: {}", t.len(), top.join(" "));
    }
}

// --- class loading -----------------------------------------------------------------
// The boot's FIRST BLOCK is class loading and static initialisation, and rows 279-283
// have priced two things inside it without accounting for the block. Before optimising
// anything else there, count what it actually does: how many `get_or_load` calls, how
// many of them are CACHE HITS, and how many distinct classes are really parsed.
static CL_CALLS: AtomicU64 = AtomicU64::new(0);
static CL_MISS: AtomicU64 = AtomicU64::new(0);

/// THREE OUTCOMES, not two. A cache miss is either a class about to be PARSED — rare,
/// bounded by the jar — or a name already KNOWN ABSENT, which can repeat without limit.
/// Counting them together said "41% miss" and meant nothing.
static CL_ABSENT: AtomicU64 = AtomicU64::new(0);

pub(crate) fn class_lookup(miss: bool) {
    let _ = miss;
}

/// WHICH names are re-probed. 4.5M absent lookups against 4,499 real classes means the
/// same handful of names is asked for again and again; naming them says WHICH CALLER to
/// give a memo, which is worth more than making each probe cheaper (row 284 measured
/// that and it lost).
pub(crate) fn class_absent_name(name: &str) {
    if !enabled() {
        return;
    }
    use std::sync::Mutex;
    static TALLY: std::sync::OnceLock<Mutex<std::collections::BTreeMap<String, u64>>> =
        std::sync::OnceLock::new();
    let tally = TALLY.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()));
    let mut t = tally.lock().unwrap_or_else(|e| e.into_inner());
    let e = t.entry(name.to_string()).or_insert(0);
    *e += 1;
    let total: u64 = if *e % 200_000 == 0 { t.values().sum() } else { 0 };
    if total > 0 {
        let mut rows: Vec<(&String, &u64)> = t.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        let top: Vec<String> = rows.iter().take(12).map(|(k, v)| format!("{k}={v}")).collect();
        eprintln!("[CONTRACT] absent names: distinct {} total {total} | top: {}", t.len(), top.join(" "));
    }
}

/// WHICH CALLER re-probes. The name tally says WHAT is asked for 4.5M times a boot and
/// the answer looked like one hierarchy walk repeated, but a name does not name a caller
/// and every fix aimed at the probe itself has lost (rows 284, 285). So sample the stack
/// instead: one capture in every 10,000 absent probes is ~450 stacks a boot, which is
/// plenty to rank callers and cheap enough that a census build does not distort.
///
/// A backtrace costs microseconds. It is only ever taken behind `enabled()`, which is
/// itself behind `#[cfg(feature = "contract-stat")]`, so the product pays nothing.
pub(crate) fn class_absent_site() {
    if !enabled() {
        return;
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEEN: AtomicU64 = AtomicU64::new(0);
    let n = SEEN.fetch_add(1, Ordering::Relaxed);
    if n % 10_000 != 0 {
        return;
    }
    let bt = format!("{}", std::backtrace::Backtrace::force_capture());
    // Keep the first few rava frames above the loader. The interesting frame is the one
    // that decided to ask, not `get_or_load` itself, and not the interpreter loop below it.
    let mut frames: Vec<String> = Vec::new();
    for line in bt.lines() {
        let l = line.trim();
        let Some(at) = l.split_once("rava_interp::") else { continue };
        let name = at.1.split("::{{").next().unwrap_or(at.1);
        let name = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == 'h' || c == ':');
        if name.contains("contract_stat") || name.contains("class_loader::loader::Loader::get_or_load") {
            continue;
        }
        frames.push(name.to_string());
        if frames.len() == 4 {
            break;
        }
    }
    use std::sync::Mutex;
    static SITES: std::sync::OnceLock<Mutex<std::collections::BTreeMap<String, u64>>> =
        std::sync::OnceLock::new();
    let sites = SITES.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()));
    let mut t = sites.lock().unwrap_or_else(|e| e.into_inner());
    *t.entry(frames.join(" < ")).or_insert(0) += 1;
    let total: u64 = t.values().sum();
    if total % 50 == 0 {
        let mut rows: Vec<(&String, &u64)> = t.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        eprintln!("[CONTRACT] absent sites: {total} samples x10k, distinct {}", t.len());
        for (k, v) in rows.iter().take(10) {
            eprintln!("[CONTRACT]   {v:>5}  {k}");
        }
    }
}

pub(crate) fn class_lookup3(hit: bool, known_absent: bool) {
    if !enabled() {
        return;
    }
    let c = CL_CALLS.fetch_add(1, Relaxed) + 1;
    if !hit {
        if known_absent {
            CL_ABSENT.fetch_add(1, Relaxed);
        } else {
            CL_MISS.fetch_add(1, Relaxed);
        }
    }
    if c % 500_000 == 0 {
        let (parsed, absent) = (CL_MISS.load(Relaxed), CL_ABSENT.load(Relaxed));
        eprintln!(
            "[CONTRACT] class lookups {c} | hits {} ({:.1}%) | KNOWN-ABSENT re-probes {absent} ({:.1}%) | parsed {parsed} ({:.4}%)",
            c - parsed - absent,
            (c - parsed - absent) as f64 * 100.0 / c as f64,
            absent as f64 * 100.0 / c as f64,
            parsed as f64 * 100.0 / c as f64,
        );
    }
}
