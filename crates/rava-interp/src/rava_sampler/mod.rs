//! 20.3 native wall-time hotspot sampler (RAVA_PROFILE_WALL). A background thread reads a global
//! "current activity" category every 10 ms and tallies it, yielding a wall-time distribution of the
//! boot across JIT_EXEC / INTERP_EXEC / VM_DISPATCH / CLASS_LOADING / NATIVE_BRIDGES / OTHER.
//!
//! The main thread sets the current category via cheap RAII guards at coarse phase boundaries
//! (`enter(cat)` swaps the global on entry, restores the previous category on drop). Categories nest
//! through the Rust call stack, so the sampler always sees the INNERMOST active activity (self-time).
//! The guard does only relaxed atomic stores — it never reads/writes a guest Value or touches a
//! control-flow path, so it is INERT (hashes converge bit-for-bit). When RAVA_PROFILE_WALL is unset,
//! `enter` returns an inert guard (one atomic-bool load, no swap) and no sampler thread is spawned.


use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub const OTHER: u8 = 0;
pub const JIT_EXEC: u8 = 1;
pub const INTERP_EXEC: u8 = 2;
pub const VM_DISPATCH: u8 = 3;
pub const CLASS_LOADING: u8 = 4;
pub const NATIVE_BRIDGES: u8 = 5;
/// The host callback path OUT of compiled code: `Vm::jit_call` and everything under it — the PIC
/// miss, the resolve, the marshalling — up to the point where the callee re-categorises itself.
///
/// Split out because it was silently inflating JIT_EXEC. `jit_call` armed NO category, so a
/// dispatch miss taken from machine code stayed billed to "JIT_EXEC (Cranelift machine code)" for
/// its whole host excursion, and the tree has measured 289.7M such misses — 59.1% of all dynamic
/// calls out of compiled code. That made the biggest row in the profile a SUM of two things a
/// codegen change affects completely differently: the machine code itself, which a better emitter
/// improves, and the host dispatch path, which it does not touch at all.
///
/// This is exactly why every codegen lever measured 2-6% while JIT_EXEC read ~60% of the serial
/// window. The denominator was never the thing they act on.
pub const JIT_CALLBACK: u8 = 6;
const NCAT: usize = 7;
const NAMES: [&str; NCAT] = [
    "OTHER (idle/GC/setup)",
    "JIT_EXEC (Cranelift machine code)",
    "INTERP_EXEC (bytecode interp)",
    "VM_DISPATCH (resolve/vtable/tier)",
    "CLASS_LOADING (parse/verify/link)",
    "NATIVE_BRIDGES (gson/R2OMAP/IO)",
    "JIT_CALLBACK (PIC miss -> host dispatch)",
];

static CURRENT: AtomicU8 = AtomicU8::new(OTHER);
static SAMPLES: [AtomicU64; NCAT] = [const { AtomicU64::new(0) }; NCAT];
static TOTAL: AtomicU64 = AtomicU64::new(0);

/// The innermost compiled method the VM has ENTERED (0 = none), published by
/// `jit::jit_method_push/pop` around the `call_raw` boundary in `jit_run`. One relaxed
/// store per compiled ENTRY — never per call inside compiled code, and nothing is emitted
/// into a compiled body (a machine-code PIC / direct-mono hit jumps straight to the callee
/// and is deliberately invisible here; billing it to the entry frame is the whole point of
/// "entry self-time"). The sampler reads it only on a JIT_EXEC tick, so the method
/// histogram is an exact decomposition of the JIT_EXEC row.
pub static CURRENT_METHOD: AtomicU32 = AtomicU32::new(0);

/// The bridge whose OUTERMOST frame the current thread is inside, or 0 for "no bridge named".
///
/// Why this exists next to a census that already claims to decompose NATIVE_BRIDGES
/// (`natives::bridgestat`, RAVA_BRIDGE_STAT): that one TIMES each entry with two
/// `Instant::now()` calls, so a bridge called very often for very little work bills the timer
/// overhead as if it were work. This tree has the scar — `charAt` was billed 1.81s, was then
/// removed OUTRIGHT, and the block it supposedly dominated did not move — and the standing rule
/// became "trust the census only when N is small and work-per-call is large", which is exactly
/// the population it is least needed for.
///
/// Sampling has no such bias. A relaxed store per bridge entry does inflate total runtime a
/// little, but it is NOT attributed to the bridge: ticks are taken by an independent thread, so
/// what a row measures is time actually spent inside that bridge. Same design as
/// `CURRENT_METHOD`, and for the same reason.
pub static CURRENT_BRIDGE: AtomicU32 = AtomicU32::new(0);

/// The CALL SITE whose host excursion the current thread is inside: the caller's `LoadedClass`
/// pointer and the constant-pool index, published raw.
///
/// Raw rather than interned because this sits on `Vm::jit_call`, the hottest host entry point in
/// the engine (~453M callbacks on a boot). Interning would hash on the guest path; two relaxed
/// stores do not. The sampler aggregates the pair and resolves names only at dump time.
///
/// The two stores are not atomic as a PAIR, so a sample can in principle catch a torn (class, cp)
/// combination. That is a rare misattribution in a ranked table, not a correctness problem, and it
/// is the price of keeping the guest path free of a lock.
///
/// Dereferencing the class pointer at dump time is sound for the same reason `bridgestat` may key
/// on constant-pool `&str` pointers: the loader never evicts, so an `Arc<LoadedClass>` handed to
/// compiled code lives for the process.
pub static CURRENT_SITE_CLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
pub static CURRENT_SITE_CP: AtomicU32 = AtomicU32::new(0);

static SITES: Mutex<Option<std::collections::HashMap<(usize, u32), u64>>> = Mutex::new(None);

/// The same (caller class, cp) tally, but scoped to NATIVE_BRIDGES ticks instead of JIT_CALLBACK.
///
/// WHY IT EXISTS: the bridge table says WHICH bridge burns the time and how many times it is
/// called, and nothing said WHO CALLS IT. That gap produced a wrong identification once already -
/// a bridge row and a site row were joined on equal call counts, a fast path was built for the
/// resulting "caller", and a first-hit probe then showed that constructor runs ONCE per boot. A
/// bridge's caller has to be measured, not inferred from a coincidence.
///
/// ⚠ ATTRIBUTION LIMIT, stated because it decides how far a row can be trusted: the site is
/// published by `jit_call`, so it names the most recent HOST EXCURSION from compiled code. A
/// bridge entered from compiled code is attributed correctly; a bridge entered from the
/// INTERPRETER inherits whatever site the previous excursion left, which is a stale read. Rows
/// whose bridge is normally called from compiled code are therefore meaningful; a bridge that is
/// mostly interpreter-driven will smear across whatever sites preceded it.
static BRIDGE_SITES: Mutex<Option<std::collections::HashMap<(usize, u32), u64>>> = Mutex::new(None);

/// Enter a category AND publish the call site in ONE gate check.
///
/// `jit_call` is the hottest host entry point in the engine — ~453M callbacks on a boot — and
/// arming the category and the site separately meant TWO `enabled()` reads plus two guard
/// constructions per callback on the DEFAULT path, to serve a diagnostic that is off. One check
/// covers both, so the disabled path is a single cached load and a predicted branch.
#[inline]
pub fn enter_site(cat: u8, cls: usize, cp: u32) -> Guard {
    if !enabled() {
        return Guard { prev: OTHER, armed: false };
    }
    CURRENT_SITE_CLS.store(cls, Relaxed);
    CURRENT_SITE_CP.store(cp, Relaxed);
    enter(cat)
}

static BRIDGE_NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Intern a bridge identity, keyed by the (owner, name) STRING POINTERS. Those come from the
/// constant pool / static natives table and are stable for process lifetime, so the pair is a
/// sound key and no string is hashed. First insert copies the label once.
pub fn register_bridge(owner: &str, name: &str) -> u32 {
    // GATED FIRST. Every native bridge call reaches this, so an ungated global mutex here would
    // be a tax on the DEFAULT path to serve a diagnostic that is off - exactly the shape this
    // tree keeps refuting. `enabled()` is a cached OnceLock read.
    if !enabled() {
        return 0;
    }
    static IDS: OnceLock<Mutex<std::collections::HashMap<(usize, usize), u32>>> = OnceLock::new();
    let key = (owner.as_ptr() as usize, name.as_ptr() as usize);
    let m = IDS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let Ok(mut ids) = m.lock() else { return 0 };
    if let Some(&id) = ids.get(&key) {
        return id;
    }
    let Ok(mut names) = BRIDGE_NAMES.lock() else { return 0 };
    names.push(format!("{owner}#{name}"));
    let id = names.len() as u32;
    ids.insert(key, id);
    id
}

fn bridge_name(bid: u32) -> Option<String> {
    if bid == 0 {
        return None;
    }
    BRIDGE_NAMES.lock().ok()?.get(bid as usize - 1).cloned()
}

/// RAII: publish `bid` as the current bridge, restoring the previous one on drop, so a
/// native -> guest -> native re-entry attributes to the INNER bridge while it runs and back to
/// the outer one afterwards.
pub struct BridgeScope {
    prev: u32,
    armed: bool,
}
impl Drop for BridgeScope {
    fn drop(&mut self) {
        if self.armed {
            CURRENT_BRIDGE.store(self.prev, Relaxed);
        }
    }
}
pub fn bridge_scope(bid: u32) -> BridgeScope {
    if !enabled() {
        return BridgeScope { prev: 0, armed: false };
    }
    BridgeScope { prev: CURRENT_BRIDGE.swap(bid, Relaxed), armed: true }
}

/// How a printed method table is ordered.
#[derive(Clone, Copy)]
enum Rank {
    /// Whole run to date.
    Cumulative,
    /// This dump block only — the per-phase view.
    Block,
}

static METHODS: Mutex<MethodTally> = Mutex::new(MethodTally::new());
/// Same tally shape as METHODS, scoped to NATIVE_BRIDGES ticks instead of JIT_EXEC ones.
static BRIDGES: Mutex<MethodTally> = Mutex::new(MethodTally::new());

/// RAVA_PROFILE_PC implies the sampler: PC sampling runs on the same 10 ms thread and its
/// dump is emitted from the same block, so the finer flag arms the coarser one rather than
/// making the caller remember to set both.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_PROFILE_WALL").is_some() || crate::pc_profile::enabled()
    })
}

/// Periodic-dump period in 10 ms ticks (`RAVA_PROFILE_WALL_EVERY_MS`, default 30 s).
/// Consecutive dumps carry per-method DELTAS, which is what separates one guest phase from
/// the next: a caller reads the guest's own phase timings (e.g. ChunkFill's `SETUP_MS`) and
/// sums the delta blocks either side of that instant. Clamped to >= 1 tick.
fn dump_every_ticks() -> u64 {
    std::env::var("RAVA_PROFILE_WALL_EVERY_MS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|ms| (ms / 10).max(1))
        .unwrap_or(3000)
}

/// Wall clock since the sampler armed — stamps every dump so a dump block can be aligned
/// against phase timings the guest prints on its own stdout.
fn elapsed_s() -> f64 {
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

/// Spawn the 10 ms background sampler exactly once (idempotent).
fn ensure_sampler() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = elapsed_s(); // anchor the dump clock at arm time, not at the first dump
        let _ = std::thread::Builder::new()
            .name("rava-wall-sampler".into())
            .spawn(|| {
                let mut ticks: u64 = 0;
                let every = dump_every_ticks();
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    let c = CURRENT.load(Relaxed) as usize;
                    if c < NCAT {
                        SAMPLES[c].fetch_add(1, Relaxed);
                    }
                    // Method attribution is scoped to JIT_EXEC ticks, so the per-method table
                    // sums EXACTLY to the JIT_EXEC row: compiled code that re-enters the
                    // interpreter switches the category, and that tick is billed to INTERP_EXEC,
                    // not to the compiled frame that is merely on the stack.
                    if c == JIT_EXEC as usize {
                        let m = CURRENT_METHOD.load(Relaxed);
                        if let Ok(mut t) = METHODS.lock() {
                            t.hit(m);
                        }
                    }
                    // SITE attribution for the PIC-miss host path. Scoped to JIT_CALLBACK ticks,
                    // so this table sums to that row and answers the question a category total
                    // cannot: whether a few sites hold the time or it is spread flat.
                    if c == JIT_CALLBACK as usize {
                        let key =
                            (CURRENT_SITE_CLS.load(Relaxed), CURRENT_SITE_CP.load(Relaxed));
                        if key.0 != 0 {
                            if let Ok(mut m) = SITES.lock() {
                                *m.get_or_insert_with(Default::default).entry(key).or_insert(0) += 1;
                            }
                        }
                    }
                    if c == NATIVE_BRIDGES as usize {
                        let key =
                            (CURRENT_SITE_CLS.load(Relaxed), CURRENT_SITE_CP.load(Relaxed));
                        if key.0 != 0 {
                            if let Ok(mut m) = BRIDGE_SITES.lock() {
                                *m.get_or_insert_with(Default::default).entry(key).or_insert(0) += 1;
                            }
                        }
                    }
                    // BRIDGE attribution, scoped to NATIVE_BRIDGES ticks exactly as the method
                    // table is scoped to JIT_EXEC, so this table sums to the NATIVE_BRIDGES row.
                    if c == NATIVE_BRIDGES as usize {
                        let b = CURRENT_BRIDGE.load(Relaxed);
                        if let Ok(mut t) = BRIDGES.lock() {
                            t.hit(b);
                        }
                    }
                    // TRUE self-time inside compiled code (RAVA_PROFILE_PC, inert otherwise).
                    // The tally above is ENTRY self-time and cannot see through a PIC-resolved
                    // subtree; this reads the guest thread's actual instruction pointer, which
                    // can — without emitting anything into a compiled body.
                    crate::pc_profile::sample();
                    TOTAL.fetch_add(1, Relaxed);
                    ticks += 1;
                    // Periodic running dump (~every 30 s) so a long server boot that is KILLED
                    // before a clean exit (no exit-chain dump) still surfaces the distribution.
                    if ticks % every == 0 {
                        dump_now("running");
                        // Same kill-before-exit rationale for the task-graph census:
                        // its report otherwise fires only at Heap drop, which a
                        // hard-killed server never reaches. Inert unless its gate is on.
                        #[cfg(feature = "next-gen-gc")]
                        crate::taskgraph_probe::report();
                        #[cfg(feature = "next-gen-gc")]
                        crate::gilfree_cf::report();
                    }
                }
            });
    });
}

/// RAII: restores the previous category on drop (inert variant carries `armed=false`).
pub struct Guard {
    prev: u8,
    armed: bool,
}
impl Drop for Guard {
    #[inline]
    fn drop(&mut self) {
        if self.armed {
            CURRENT.store(self.prev, Relaxed);
        }
    }
}

/// Enter `cat` for the lifetime of the returned guard. No-op (inert guard) when profiling is off.
#[inline]
pub fn enter(cat: u8) -> Guard {
    if !enabled() {
        return Guard { prev: OTHER, armed: false };
    }
    ensure_sampler();
    // The thread that runs compiled code is by definition the one that raises JIT_EXEC, so
    // this is the natural place to publish the PC-sampling target — and it costs the
    // compiled-entry boundary nothing, because this whole function is already gated off.
    if cat == JIT_EXEC {
        crate::pc_profile::arm_current_thread();
    }
    let prev = CURRENT.swap(cat, Relaxed);
    Guard { prev, armed: true }
}

/// Print one ranked method table. `share_of` is the denominator for the percentage column;
/// `by_block` selects which of the two counters leads.
fn print_method_table(
    header: &str,
    what: &str,
    rows: &[(u32, u64, u64)],
    share_of: u64,
    by_block: bool,
) {
    print_id_table("WALL-METHOD", "method", header, what, rows, share_of, by_block, &|mid| {
        crate::jit::compiled_method_name(mid)
    });
}

pub fn dump() {
    if !enabled() {
        return;
    }
    dump_now("final");
}

/// Phase bracketing (`rava.wall.reset` / `rava.wall.dump` via System.getProperty). The 30s
/// periodic dump is too coarse to isolate a phase that lasts a few seconds, and both it and
/// the final dump are CUMULATIVE — a 12s phase is invisible behind a 55s bootstrap. Zeroing
/// the tallies at a phase boundary makes the next dump that phase's own distribution.
/// Inert unless RAVA_PROFILE_WALL is set.
pub fn reset() {
    if !enabled() {
        return;
    }
    ensure_sampler();
    for s in SAMPLES.iter() {
        s.store(0, Relaxed);
    }
    TOTAL.store(0, Relaxed);
}

pub fn dump_tagged(tag: &str) {
    if !enabled() {
        return;
    }
    dump_now(tag);
}
mod report;
pub(crate) use report::*;
