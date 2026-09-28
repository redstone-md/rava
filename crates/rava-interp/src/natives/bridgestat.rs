//! Counting what the native bridges served.
//!/ RAVA_BRIDGE_STAT (default OFF, inert): per-(owner,name) decomposition of the NATIVE_BRIDGES
//!/ wall bucket by TIMING each entry.
//!/
//!/ ⚠ PREFER THE SAMPLED TABLE for "where does bridge time go": `RAVA_PROFILE_WALL` now prints a
//!/ `[WALL-BRIDGE]` table built the same way the method table is (`rava_sampler::CURRENT_BRIDGE`,
//!/ one relaxed store per entry, ticks taken by an independent thread). Two `Instant::now()` calls
//!/ per entry, as here, bill timer overhead to bridges called often for little work — `charAt` was
//!/ billed 1.81s by THIS census, was then removed outright, and the block it supposedly dominated
//!/ did not move. The standing rule that followed — trust it only when N is small and work-per-call
//!/ is large — makes it least reliable exactly where a bridge population is biggest.
//!/
//!/ What this one still gives that sampling does not: exact CALL COUNTS, and self-vs-inclusive
//!/ split per bridge. Use it to characterise a bridge already identified; use the sampled table to
//!/ identify one.
//!/
//!/ (For the record, the sampled table's answer was that the bucket is two-thirds
//!/ `CompletableFuture#allOf` — the main thread PARKED waiting for the ring — and flat underneath.)
//!/ Times the OUTERMOST bridge entry per thread (nested native→guest→native re-entries attribute to
//!/ the outer bridge = inclusive time, the right shape for "what should become a deeper native").
//!/ Keys by the &str pointers (constant-pool strings — stable for process lifetime, loader never
//!/ evicts); the name is copied once on first insert only.

//! ⚠**TWO LIMITS THIS INSTRUMENT HAS, BOTH FOUND BY DISAGREEING WITH A CLEANER ONE.**
//!
//! 1. **It charges its own accounting to the callee, per call.** Every bridge takes a lock and a
//!    hash update, so a native called 780,000 times carries far more of that than one called 300
//!    — the bias is in the RANKING, not merely in the absolute numbers. `List#iterator` reads
//!    1,872 ns here and **615 ns** on `bench/NativeCost.java`, which isolates one operation in a
//!    compiled loop with no accounting at all. Where the two disagree, believe `NativeCost`.
//!
//! 2. **"self" is not a native's own cost when the native runs guest code.** A product boot puts
//!    `CompletableFuture#thenCompose` at 2.74 s over **2 calls** and `List#forEach` at 2.44 s over
//!    **8** — those are not expensive natives, they are natives that drive the entire datapack
//!    load inside themselves. Only high-count, short-duration rows read as per-op prices, and
//!    those are exactly the rows limit 1 distorts.
//!
//! ⇒ Use it to find WHICH natives the traffic goes to, then price them one at a time somewhere
//! else. Used that way it has earned its keep twice: it named `List#iterator` as 36-38% of a
//! collection-iteration probe, and it named the stream cluster as 28% of the boot's native time.
//!
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

pub fn enabled() -> bool {
    pub(crate) static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_BRIDGE_STAT").is_some())
}
type Tally = HashMap<(usize, usize), (String, u64, u64, u64)>; // (owner_ptr,name_ptr) → (label, calls, self_ns, incl_ns)
fn tally() -> &'static Mutex<Tally> {
    pub(crate) static T: OnceLock<Mutex<Tally>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}
thread_local! {
    /// ns consumed by COMPLETED child bridges of the currently-open bridge frame.
    /// Classic self-time profiler scheme — required here because the whole server
    /// boot runs INSIDE one never-returning bridge (bundler → Thread#start →
    /// vm.apply drives the entire server), so "attribute at top-level only" would
    /// record nothing for 250s. Self-time per bridge, every level, no double count.
    pub(crate) static CHILD_NS: Cell<u64> = const { Cell::new(0) };
}
pub struct Guard {
    key: (usize, usize),
    label: (usize, usize, usize, usize), // (owner_ptr,owner_len,name_ptr,name_len) for first-insert copy
    t0: Instant,
    child_at_enter: u64,
}
#[inline]
pub fn enter(owner: &str, name: &str) -> Option<Guard> {
    if !enabled() {
        return None;
    }
    {
        // one-shot arm beacon: proves the tally is live on THIS run's bridge path.
        static ARMED: std::sync::Once = std::sync::Once::new();
        ARMED.call_once(|| eprintln!("[BRIDGE-STAT] armed (first bridge: {owner}#{name})"));
    }
    // Fire the periodic dump from ENTER (bridges enter constantly even while the
    // outermost one never returns).
    maybe_running_dump();
    Some(Guard {
        key: (owner.as_ptr() as usize, name.as_ptr() as usize),
        label: (owner.as_ptr() as usize, owner.len(), name.as_ptr() as usize, name.len()),
        t0: Instant::now(),
        child_at_enter: CHILD_NS.with(|c| c.replace(0)),
    })
}
/// Counting probe for the bridge-prologue arena-string canonicalization
/// (fix:allocator-traffic): calls that canonicalized, strings copied, bytes
/// copied. Counts, not walls — inert (single load + branch) unless
/// RAVA_BRIDGE_STAT is set; the caller pre-checks `enabled()` on the arm
/// whose tally needs its own pass.
static CANON_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CANON_STRS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CANON_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[inline]
pub fn note_canon(strs: u64, bytes: u64) {
    if !enabled() {
        return;
    }
    use std::sync::atomic::Ordering::Relaxed;
    CANON_CALLS.fetch_add(1, Relaxed);
    CANON_STRS.fetch_add(strs, Relaxed);
    CANON_BYTES.fetch_add(bytes, Relaxed);
}

/// Periodic running dump (~every 60s of wall): a server boot is normally KILLED after the
/// "Done" line (no clean exit-chain), so waiting for `dump()` at exit would lose the tally.
fn maybe_running_dump() {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub(crate) static NEXT: AtomicU64 = AtomicU64::new(0);
    pub(crate) static EPOCH: OnceLock<Instant> = OnceLock::new();
    let t0 = EPOCH.get_or_init(Instant::now);
    let now = t0.elapsed().as_secs();
    // ⚠**A 60 s PERIOD ON A 48 s BOOT DUMPS NOTHING** — the same defect ledger row 393
    // recorded for six other censuses, and the reason this table had never once been read on
    // the workload it exists for. `RAVA_BRIDGE_STAT_S` (default 60, so every existing use is
    // unchanged) makes the period fit the phase being measured.
    let period = {
        static P: OnceLock<u64> = OnceLock::new();
        *P.get_or_init(|| {
            std::env::var("RAVA_BRIDGE_STAT_S")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(60)
        })
    };
    let next = NEXT.load(Ordering::Relaxed);
    if now >= next
        && NEXT
            .compare_exchange(next, now + period, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        && next != 0
    {
        dump_now("running");
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let total = self.t0.elapsed().as_nanos() as u64;
        // SELF time = my wall minus my completed children's wall; then roll my
        // whole wall up into the parent's child-accumulator.
        let my_children = CHILD_NS.with(|c| c.get());
        let ns = total.saturating_sub(my_children);
        CHILD_NS.with(|c| c.set(self.child_at_enter + total));
        if let Ok(mut t) = tally().lock() {
            let e = t.entry(self.key).or_insert_with(|| {
                // SAFETY: constant-pool &str backing — stable address/len for process lifetime.
                let owner = unsafe {
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                        self.label.0 as *const u8,
                        self.label.1,
                    ))
                };
                let name = unsafe {
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                        self.label.2 as *const u8,
                        self.label.3,
                    ))
                };
                (format!("{owner}#{name}"), 0, 0, 0)
            });
            e.1 += 1;
            e.2 += ns; // self (exclusive)
            e.3 += total; // inclusive (self + children)
        }
    }
}
fn dump_now(tag: &str) {
    let t = match tally().lock() {
        Ok(t) => t,
        Err(_) => return,
    };
    let mut rows: Vec<_> = t.values().cloned().collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2));
    let total_ns: u64 = rows.iter().map(|r| r.2).sum();
    eprintln!(
        "[BRIDGE-STAT {tag}] {} distinct bridges, {:.1}s total SELF-time; top 40 (self%, incl, calls):",
        rows.len(),
        total_ns as f64 / 1e9
    );
    for (label, calls, ns, incl) in rows.into_iter().take(40) {
        eprintln!(
            "[BRIDGE-STAT]   self={:>7.2}s {:>6.1}%  incl={:>7.2}s {:>10} calls  {}",
            ns as f64 / 1e9,
            ns as f64 * 100.0 / total_ns.max(1) as f64,
            incl as f64 / 1e9,
            calls,
            label
        );
    }
    {
        use std::sync::atomic::Ordering::Relaxed;
        eprintln!(
            "[BRIDGE-STAT {tag}] canon-args: {} calls copied {} strings / {} bytes (scratch={})",
            CANON_CALLS.load(Relaxed),
            CANON_STRS.load(Relaxed),
            CANON_BYTES.load(Relaxed),
            crate::canon_scratch()
        );
    }
}

/// Top-40 bridges by inclusive time. Called from the CLI exit chain next to the sampler dump.
pub fn dump() {
    if !enabled() {
        return;
    }
    dump_now("final");
}

/// Phase bracketing (`rava.bridge.reset` / `rava.bridge.dump` via System.getProperty):
/// the exit-chain dump covers the WHOLE run, which fuses a 55s bootstrap with the 12s
/// phase actually under study. Clearing the tally at a phase boundary makes the next
/// dump that phase's own composition. Inert unless RAVA_BRIDGE_STAT is set.
pub fn reset() {
    if !enabled() {
        return;
    }
    if let Ok(mut t) = tally().lock() {
        t.clear();
    }
}
pub fn dump_tagged(tag: &str) {
    if !enabled() {
        return;
    }
    dump_now(tag);
}
