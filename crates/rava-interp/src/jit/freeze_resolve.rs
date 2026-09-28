//! The freeze table: a lock-free, append-only map from (call site, receiver class word) to a
//! resolved compiled entry, so a gil-free worker can serve a dispatch without taking the resolve
//! lock. Its counters are what price the lane.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::OnceLock;

const CAP: usize = 1 << 17; // 131072 entries × 24B = 3MB, plenty for sites × classes
const MASK: usize = CAP - 1;
const BUSY: i64 = 1; // claim marker in the site word (real sites are heap pointers ≫ 1)
const MAX_PROBE: usize = 64;

struct Slot {
    site: AtomicI64,
    ck: AtomicI64,
    val: AtomicI64,
}
fn table() -> &'static [Slot] {
    static T: OnceLock<Box<[Slot]>> = OnceLock::new();
    T.get_or_init(|| {
        (0..CAP)
            .map(|_| Slot { site: AtomicI64::new(0), ck: AtomicI64::new(0), val: AtomicI64::new(0) })
            .collect()
    })
}

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_M3_FREEZE_RESOLVE").is_none())
}

/// `RAVA_NO_AOT2=1` (or `RAVA_NO_EMIT_MORE`) restores the historical table exactly: every thread
/// publishes every verdict, and the first probe materialises the 3 MB of slots whether or not a
/// row has ever been stored.
fn aot2_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_AOT2").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**MAKE THE PUBLISHER SET EQUAL THE READER SET.**
///
/// `get` has exactly three call sites (`jit/callbacks/dispatch.rs`) and two of them sit inside
/// `if crate::gil::is_worker() && freeze_resolve::enabled()`; the third is the freeze-coverage
/// census, which is default OFF and is an instrument, not a serve path. So **in production this
/// table is read only from a thread `gil::is_worker()` is true on.**
///
/// `put` had no such restriction — it is called from `vm/dispatch.rs`'s PIC fill and
/// `vm/pic_fill.rs`'s heap-Ref fill, which the SERIAL boot walks on every resolving PIC miss. Main
/// therefore paid, per fill, a `slot_of` mix and an open-addressing walk whose first step is a
/// random line in a 3 MB table (a cold line essentially every time), plus — on a row create — a
/// `member_ref` (three constant-pool reads, two UTF-8 decodes) and a descriptor walk, to publish a
/// row **no thread of its own kind will ever read**.
///
/// What that publication is worth is already counted, and it is the whole reason to stop:
/// `freeze: table_hit=8,091` against `direct_hit=174,821,132` (ledger row 302) bounds the value of
/// the ENTIRE table — every row from every thread — at 8,091 lock-free serves, while step 0b, the
/// worker direct cache sitting immediately behind it, absorbs the population.
///
/// And main's share of even that is bounded again, because **a worker publishes its own rows**:
/// the continuation behind these fast paths is `resolve_lock_acquire()` → the locked handler →
/// the same two fill sites, and `vm/dispatch.rs`'s `site_ways()` reads `gil::is_worker()` there
/// precisely because workers run it. A row main pre-warms therefore saves a worker **its first
/// lock take at that (site, class) pair and nothing after it** — the worker re-derives and
/// re-publishes the identical verdict on that miss. Dropping main's publication costs at most one
/// resolve per pair (`stored` was 6,113 on the run that priced this) and cannot change a verdict:
/// a missing row is `None` = "unknown", which is the same answer the table already gives when its
/// probe region saturates, and the locked path is the derivation both agree with.
///
/// The census keeps its rows: when `RAVA_FREEZE_CENSUS` is armed, main is a reader again (it runs
/// the coverage probe at `dispatch.rs:93`), so it publishes again — the instrument measures the
/// coverage ceiling of a warmed table, and arming it must not empty the thing it measures.
///
/// Clause ORDER is the cheap one first: `is_worker()` is a thread-local `Cell<bool>` read and is
/// TRUE on every publication this keeps, so the kill switch's `OnceLock` is consulted only on the
/// main-thread puts that are about to be dropped anyway.
#[inline]
fn publishable() -> bool {
    crate::gil::is_worker() || !aot2_enabled() || crate::pilot_census::freeze_census_on()
}

/// ⚠**AND THE READER BUILT THE TABLE BEFORE ANY WRITER HAD A VERDICT.** `get`'s first act was
/// `table()`, whose `OnceLock` init `collect`s 131,072 `Slot`s = **3 MB touched and zeroed**, on
/// the first probe — even when nothing has ever been stored, which is the state a fresh process is
/// in and, with publication now worker-only, the state most of a boot is in. Same shape as the
/// 28.5 MB of dispatch tables that were allocated before any verdict existed.
///
/// A monotone latch fixes it without a counter on the hot path: `put` sets it once, on the first
/// row it actually creates; `get` reads it and returns `None` while it is clear. Relaxed is sound
/// in the only direction that can go wrong — a stale `false` yields `None` = "unknown", i.e. the
/// locked path, i.e. the correct verdict derived the slow way — and the latch never returns to
/// `false`, so it can only ever WITHDRAW a fast path, never re-arm one (the discipline
/// `gil::THREAD_STATE` is written to). Its own cache line, because the neighbouring statics
/// (`WORKER_MISS` is an UNCONDITIONAL `fetch_add` on this very path) are RMW'd by four workers,
/// and a read that costs a coherence bounce is not a cheap read.
#[repr(align(64))]
struct RowLatch(std::sync::atomic::AtomicBool);
static ANY_ROW: RowLatch = RowLatch(std::sync::atomic::AtomicBool::new(false));

/// Stats gate: the hot-path counters are 30M+ fetch_adds on shared cache lines across 4
/// workers — measurable false-sharing ping-pong. Count only under RAVA_FORK_TRACE.
#[inline]
pub fn stats_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FORK_STATS").is_some())
}

#[inline]
fn slot_of(site: i64, ck: i64) -> usize {
    // fxhash-style mix of both words.
    let h = (site as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .rotate_left(23)
        ^ (ck as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    (h >> 16) as usize & MASK
}

/// Publish/refresh `(site, ck) → val`. Callers run on main OR under VM_RESOLVE_LOCK, but
/// main+worker can race each other — the claim-CAS makes concurrent puts safe.
pub fn put(site: i64, ck: i64, val: i64) {
    if !enabled() || site == 0 {
        return;
    }
    // Publish only where a reader of our own kind exists — see `publishable`. Placed ABOVE the
    // wide-param scan on purpose: under `RAVA_NO_FAST_DCACHE` that scan is eager, and main was
    // paying a `member_ref` + descriptor walk per fill for a row it was then going to store where
    // nothing on main can read it.
    if !publishable() {
        return;
    }
    // NEVER PUBLISH A WIDE-PARAM SITE.
    //
    // The serve arm calls `f(buf, ctx)` with the operand buffer AS-IS. A `long`/`double`
    // parameter arrives as ONE operand cell but occupies TWO local slots in the callee, which
    // is why `jit_direct_resolve` builds a `wide_mask` for the direct path and why the
    // machine-code IC "only fires on no-wide-arg sites". This table mirrored verdicts without
    // that restriction, so a worker served such a site read its arguments out of the wrong
    // slots — and the failure is silent: the callee returns a plausible number.
    //
    // Measured on bench/RingLane.java, which exists because no other gate arms this path:
    // the ring's noise callee is `ImprovedNoise.sampleAndLerp(IIIDDDD)D`, four doubles, and
    // declining these sites takes the lane's kept result from 3782fea2971a062 to the JVM's
    // 37539c6c375d0c3 while still completing every task off-GIL (7/7, keep=true).
    //
    // Refusing at PUBLICATION rather than at serve keeps the table's contract true by
    // construction — no serve path can bypass it.
    //
    // ⭐**AND THE SCAN BELONGS ON THE CREATE PATH, NOT ON EVERY PUT.** The comment this replaces
    // claimed "one descriptor scan per (site, class) first sighting instead of one per hit", and
    // that was not what the code did: the check ran ABOVE the probe, so every REFRESH of a row
    // that already exists paid `member_ref` (three constant-pool reads plus two UTF-8 decodes)
    // and a walk of the descriptor as well. A megamorphic site re-resolves and re-publishes the
    // same (site, class) pairs on every PIC eviction, so that is a per-put cost for a value that
    // is a **constant of the site**. was O(descriptor length) per put, is O(descriptor length)
    // per row CREATED (`RAVA_NO_FAST_DCACHE=1` restores the eager form).
    //
    // The contract is unchanged and still true by construction: the check is evaluated before any
    // slot is CLAIMED, so a wide site can never own a row — which also means the refresh arm can
    // never be reached for one, and `wide_refused` counts what it counted before ACROSS THE PUTS
    // THAT STILL HAPPEN (a wide site finds no row, walks to the first empty slot, and is refused
    // there on every put) — the `publishable` gate above now drops main's puts before this point,
    // so the counter reads the publisher set, which is also the reader set.
    // `wide` is `Some` once the answer is known. The eager arm fills it up front, so the create
    // path below then finds `Some(false)` and both spellings run the scan exactly once per put —
    // the difference is only WHICH puts reach it.
    let mut wide: Option<bool> = None;
    if !super::dispatch_table::fast_dcache() {
        let w = super::site_has_wide_param(site & ((1i64 << 48) - 1), site >> 48);
        if w {
            WIDE_REFUSED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        wide = Some(w);
    }
    let t = table();
    let mut i = slot_of(site, ck);
    for _ in 0..MAX_PROBE {
        let s = t[i].site.load(Ordering::Acquire);
        if s == site && t[i].ck.load(Ordering::Acquire) == ck {
            t[i].val.store(val, Ordering::Release);
            return;
        }
        if s == 0 {
            // ABOUT TO CREATE A ROW — this is the one place the wide verdict is needed.
            let w = match wide {
                Some(w) => w,
                None => {
                    let w = super::site_has_wide_param(site & ((1i64 << 48) - 1), site >> 48);
                    wide = Some(w);
                    w
                }
            };
            if w {
                WIDE_REFUSED.fetch_add(1, Ordering::Relaxed);
                return;
            }
            if t[i]
                .site
                .compare_exchange(0, BUSY, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                t[i].ck.store(ck, Ordering::Release);
                t[i].val.store(val, Ordering::Release);
                t[i].site.store(site, Ordering::Release); // publish LAST
                STORED.fetch_add(1, Ordering::Relaxed);
                // ARM THE PROBE-WORTH LATCH — after the row is fully published, so a reader that
                // observes the latch and then probes cannot find a half-built slot (the slot's own
                // Release/Acquire pair is what orders that; the latch is only "is a probe worth a
                // cache line"). One relaxed store, once per process in the steady state, guarded by
                // a load so a warm table does not keep writing a shared line.
                if !ANY_ROW.0.load(Ordering::Relaxed) {
                    ANY_ROW.0.store(true, Ordering::Relaxed);
                }
                return;
            }
            // lost the claim race — retry this slot (it may now hold our key)
            continue;
        }
        // BUSY or other key → advance
        i = (i + 1) & MASK;
    }
    // table region saturated — drop (derivation stays correct via the locked path)
}

/// Lock-free lookup. `None` = unknown (fall to the locked path).
#[inline]
pub fn get(site: i64, ck: i64) -> Option<i64> {
    if site == 0 {
        return None;
    }
    // NO ROW HAS EVER BEEN STORED ⇒ DO NOT MATERIALISE 3 MB TO LEARN THAT. See `RowLatch`: one
    // relaxed load of a line of its own, and a stale `false` costs the caller the locked path it
    // would have taken on a miss anyway.
    //
    // The latch is tested BEFORE the kill switch, so once the table holds a row this arm is one
    // relaxed load and the `OnceLock` is never touched — the "three process-constant gates read
    // per callback" shape, avoided by putting the state test in front of the flag test rather than
    // behind it. With `RAVA_NO_AOT2` set the latch is still clear only while the table is empty,
    // where the flag then lets the original probe run.
    if !ANY_ROW.0.load(Ordering::Relaxed) && aot2_enabled() {
        return None;
    }
    let t = table();
    let mut i = slot_of(site, ck);
    for _ in 0..MAX_PROBE {
        let s = t[i].site.load(Ordering::Acquire);
        if s == 0 {
            return None;
        }
        if s == site && t[i].ck.load(Ordering::Acquire) == ck {
            return Some(t[i].val.load(Ordering::Acquire));
        }
        i = (i + 1) & MASK;
    }
    None
}

pub static STORED: AtomicU64 = AtomicU64::new(0);
/// Worker fast-path hits served lock-free (the S1 gate metric's counterpart).
pub static WORKER_HIT: AtomicU64 = AtomicU64::new(0);
/// Freeze-table hits declined by the RAVA_FREEZE_NO_WIDE probe (wide-param callee).
pub static WIDE_REFUSED: AtomicU64 = AtomicU64::new(0);
/// Worker probes that missed/were negative → took the lock (should → ~0 as the table warms).
pub static WORKER_MISS: AtomicU64 = AtomicU64::new(0);
/// Probes that FOUND a row whose verdict is NeverCacheable (`fill == -1`). Split out of
/// `WORKER_MISS` because the two demand opposite work: a genuine miss means the table has
/// not warmed for this site, while a negative hit means it HAS and the answer is "this
/// callee cannot be served lock-free". With only `table_hit` and `miss` to look at, both
/// read as "the cache is empty", and they are not the same problem.
pub static WORKER_NEG_HIT: AtomicU64 = AtomicU64::new(0);
/// jit_call worker-guard LOCKED-EXEC entries (each = a serialized non-direct callee moment).
pub static WORKER_LOCKED_CALLS: AtomicU64 = AtomicU64::new(0);
/// Lock-free worker dispatches served by the FROZEN direct cache (step 0).
pub static WORKER_DIRECT_HIT: AtomicU64 = AtomicU64::new(0);
/// Lock-free worker calls served by a pure-read WS-native (spline search, …).
pub static WORKER_WSNATIVE_HIT: AtomicU64 = AtomicU64::new(0);

/// Composition tally of LOCKED-EXEC callees (who still serializes). Callers hold the
/// resolve lock, so the Mutex is uncontended.
fn locked_tally_map() -> &'static std::sync::Mutex<std::collections::HashMap<String, u64>> {
    static T: OnceLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
        OnceLock::new();
    T.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}
pub fn locked_tally(owner: &str, name: &str) {
    if let Ok(mut t) = locked_tally_map().lock() {
        *t.entry(format!("{owner}#{name}")).or_insert(0) += 1;
    }
}
/// Top-k locked callees as one line: "owner#name=N owner#name=N …".
pub fn locked_tally_top(k: usize) -> String {
    let Ok(t) = locked_tally_map().lock() else { return String::new() };
    let mut rows: Vec<_> = t.iter().map(|(s, &n)| (s.clone(), n)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    rows.into_iter().take(k).map(|(s, n)| format!("{s}={n}")).collect::<Vec<_>>().join(" ")
}
