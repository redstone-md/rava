//! Scratch buffers, descriptor helpers and the diagnostic dumps the CLI calls on exit.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::value::Value;
use crate::*;
/// Default-OFF (`RAVA_ANEWARRAY_SCRATCH`) — build the `L<name>;` reference-array
/// element descriptor in a stack buffer instead of TWO heap `String`s per
/// ⛔ MEASURED NULL 2026-07-29. Interleaved A/B, 8 runs, ALL clean and the tightest batch of the
/// session (71.6-72.7s wall):
///   on   Done 22.395 22.452 22.823 23.100  mean 22.693   wall mean 72.13
///   off  Done 22.223 22.447 22.586 22.921  mean 22.544   wall mean 72.00
/// Default is marginally AHEAD on both Done minimum and mean; wall is indistinguishable. The
/// descriptor build it removes is real - `jit_anewarray` is 2.3-2.9% of every real-boot window -
/// but removing it does not show. Keep OFF.
///
/// allocation (`class_name_at`'s `to_string` + the `format!`). The RIP profiler
/// names `Vm::jit_anewarray` at 2.3-2.9% of samples in every real-boot window;
/// the descriptor build is the avoidable per-call half of it (the array
/// allocation itself is the work). Same defect class as the field-key memo whose
/// hit allocated: per-call host bookkeeping that is O(string) and O(malloc).
pub(crate) fn anewarray_scratch() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ANEWARRAY_SCRATCH").is_some())
}

/// Default-OFF (`RAVA_CANON_SCRATCH`) — serve the native-bridge arena-string
/// canonicalization (`canon_arena_strings`) from a pooled `Vec` with ONE copy
/// per string argument instead of a fresh `Vec` plus TWO copies per string
/// (`from_utf8_lossy().into_owned()` builds a `String`, `.into()` then copies it
/// again into the `Arc<str>`). The buffer is checked out of `frames::pool_take`
/// and returned CLEARED on drop, after the `NativeArgsGuard` unroots it — no
/// `Value` is held across a GC point that isn't rooted exactly as before.
pub(crate) fn canon_scratch() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_CANON_SCRATCH").is_some())
}

/// Exit-time summary for the RavaPool candidate scan (RAVA_POOL_SCAN). Thin pub wrapper so the
/// binary crate can trigger it without exposing the private `jit` module.
#[cfg(feature = "next-gen-gc")]
pub fn pool_scan_dump_if_on() {
    if jit::pool_scan_enabled() {
        jit::pool_scan_dump();
    }
}
#[cfg(not(feature = "next-gen-gc"))]
pub fn pool_scan_dump_if_on() {}

/// COLLECTOR ACCOUNTING (RAVA_GC_STATS) — final cumulative report for a cleanly
/// exiting program (a bench). `nextgen` is a private module, so this is the public
/// door the CLI's teardown calls; a no-op when the flag is off or the arena feature
/// is not compiled in.
pub fn gc_stats_dump() {
    #[cfg(feature = "next-gen-gc")]
    if crate::nextgen::gc_stats_enabled() {
        eprintln!("{}", crate::nextgen::gc_stats_line());
        for (cls, objs, bytes) in crate::nextgen::tenured_class_histogram(12) {
            eprintln!("[GC-STATS]   tenured {objs:>9} objs {:>7}KB  {cls}", bytes / 1024);
        }
    }
}

/// Natural ordering of two values (numeric if both numeric, else by string),
/// returned as -1/0/1.
pub(crate) fn natural_cmp(a: &Value, b: &Value) -> i32 {
    let ord = match (a.as_long(), b.as_long()) {
        (Some(x), Some(y)) => x.cmp(&y),
        _ => a.display().cmp(&b.display()),
    };
    match ord {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// 8.2a coverage probe: treat primitive getfield/putfield as LLVM-subset-eligible in the armability
/// measurement (NOT in the emitter — that still bails, byte-identical). Read-only, gated.
#[cfg(feature = "next-gen-gc")]
pub fn llvm_field_probe() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_LLVM_FIELD_PROBE").is_some())
}

/// 8.2b FULL-CASCADE coverage probe: primitive + REF getfield/putfield, PLUS recursive
/// devirt-inline of monomorphic nested invokevirtual/interface (per devirt::SITES). Measures the
/// ceiling if a fully object-aware LLVM tier existed. Read-only; the emitter still bails.
#[cfg(feature = "next-gen-gc")]
pub fn llvm_cascade_probe() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_LLVM_FULL_CASCADE_PROBE").is_some())
}

/// 10.1b(B) toString-stack tracer: print the CALLER chain of the first few DensityFunction/Climate/
/// Noise `toString()` invocations (who stringifies the density graph, and is the result used?).
/// Read-only; forces call_stack tracking so backtrace() works.
pub fn tostring_probe() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TOSTRING_STACK_PROBE").is_some())
}


/// `RAVA_NOISE_CALLER=<n>` — print the guest backtrace the first `n` times a noise sample is taken.
///
/// ⭐**WHY THIS EXISTS, stated so the next reader does not repeat my mistake.** Ledger row 367: a
/// JFR profile of `java -Xint` booting the same jar shows **`ImprovedNoise` in 1 execution sample,
/// `PerlinNoise` in 0, `RandomState` in 0**, against `Shapes.joinIsNotEmpty` 414 and
/// `BlockStateBase$Cache.<init>` 257 — the JVM does essentially NO noise sampling during boot,
/// while rava enters `ImprovedNoise#p` **251,523,925** times. That is 91% of our wall, in a phase
/// where we are **714×** the JVM, doing work the JVM never does.
///
/// Counting the calls said WHAT; only the caller chain can say WHY. This is the same shape as the
/// `RAVA_TOSTRING_STACK_PROBE` hook next to it: capped, gated, and off unless asked for.
///
/// ⚠**DIAGNOSTIC ONLY.** It prints and returns; nothing downstream branches on it, so it routes no
/// production behaviour by name — the standing no-adapt rule.
pub fn noise_caller_probe() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_NOISE_CALLER")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    })
}


/// `RAVA_LAZY_CF=1` — do not run a `supplyAsync` supplier on the CALLING THREAD.
///
/// ⭐**WHAT THIS IS FOR (ledger row 368).** `CompletableFuture.supplyAsync` currently falls back to
/// `vm.apply(task)` inline when neither the off-GIL lane nor the guest-task queue admits the body,
/// and the source comment states it plainly: *"We run everything inline (single-threaded), so the
/// returned future is already complete; \*Async just runs the supplier now."* That is not what
/// `supplyAsync` promises, and on the Minecraft boot it is measured as **91% of the wall**:
/// `ServerLevel.<init>` → `ChunkGeneratorStructureState.ensureStructuresGenerated` →
/// `generateRingPositions(...)CompletableFuture` → `BiomeSource.findBiomeHorizontal` → 31,961
/// climate samples → 251 M `ImprovedNoise#p` entries. A JFR profile of `java -Xint` booting the
/// same jar contains **zero** samples in `ChunkGeneratorStructureState`, `findBiomeHorizontal`,
/// `generateRingPositions` or `placeStronghold` — the JVM never does this work during boot,
/// because the future it hands back is never observed there.
///
/// With this set, the fallback stores the supplier on the future and runs it on FIRST OBSERVATION
/// (any instance method of the future forces it first, which is a single choke point). A future
/// nobody ever looks at costs nothing — exactly the JVM's outcome.
///
/// ⚠**OPT-IN WHILE IT IS UNPROVEN.** Deferring a side-effecting supplier changes WHEN its effects
/// land, and this tree has already lost a boot to a deferred `<clinit>` leaving registry holders
/// unbound. There is deliberately NO drain for a future that is never observed: that gap is the
/// first thing the experiment will expose if it matters.
pub fn lazy_cf() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_LAZY_CF").is_some())
}


/// `RAVA_ASYNC_ANY_EXECUTOR=1` — accept ANY executor argument to `supplyAsync`/`runAsync` for the
/// guest-task queue, instead of falling back to running the supplier on the calling thread.
///
/// See [`lazy_cf`] for the measurement that motivates this, and ledger row 368 for the caller
/// chain. The difference between the two: `lazy_cf` DEFERS the body until someone observes the
/// future — which stalls, because a body that is *not started* is not the same as one that is
/// *not done* — whereas this makes the body actually RUN, on the task runtime, so every observer
/// still completes.
pub fn lazy_cf_trace() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("RAVA_LAZY_CF_TRACE").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
    })
}

/// ✅**DEFAULT-ON since row 427** — opt OUT with `RAVA_NO_ASYNC_OFFLOAD`.
///
/// ⭐**Because running a `supplyAsync` body on the CALLING thread is not a slow implementation of
/// the contract, it is a different contract**, and on the project's own benchmark it costs 10×:
/// paired compilers-off boots on default settings, same binary back to back, read
/// **`wall 1,420 s / Done 1295.998` against `wall 138 s / Done 0.586`**. The 22.6 minutes the
/// old behaviour spends inside `Preparing level` is Minecraft's stronghold search, which it hands
/// to `Util.backgroundExecutor()` and never observes — **the JVM does not run it during boot
/// either**, so this is parity, not a shortcut.
///
/// It took rows 421-426 to make it correct rather than merely fast: real monitors, four GC
/// root-set holes, a JVMS §5.5 class-init lock, and finally one heap under the GIL instead of a
/// per-thread heap per thread. It is default-ON now because the evidence is in — 4 gates on their
/// hashes, 427 tests, 18 differential probes against `java -Xint`, and a 300 s post-`Done` soak
/// with zero new throws on a ticking server.
///
/// ⚠**What the kill switch is for.** This is the flag that lets guest code run on a second thread
/// at all, so it is also the blast radius for anything still single-thread-assuming. Anyone who
/// hits a concurrency fault should set `RAVA_NO_ASYNC_OFFLOAD=1` and report it — that reverts to
/// running every async body inline, which is slow and was never wrong.
/// `RAVA_OFFLOAD_FIRST=1` — try the task runtime BEFORE the compiler-gated deferral lane.
///
/// ⭐**On the boot this is worth 2.13×** (product binary, same-binary flag A/B: wall 68 s /
/// `Done 0.766` against 145 s / `Done 82.677`), because the lane claims 134 of 135 `supplyAsync`
/// bodies and then runs the stronghold search back on the critical path.
///
/// ⛔**And it costs 35× on the ring workload**: `RingLane` `LANE_MS` 4,011 → 141,011, hash
/// unchanged. The lane is what that workload IS — it batches small bodies and runs them off-GIL
/// in parallel, while the task runtime serialises them under the GIL.
///
/// ⇒ **So the order is not the answer and this stays OPT-IN.** Two different workloads want two
/// different homes for a deferred body, and picking one globally just moves the loss. The real
/// fix is not an order at all.
///
/// ⛔**TWO DISCRIMINATORS WERE BUILT AND BOTH FAILED, which is what makes this flag honest.**
/// (1) *A batch of one has nothing to fan out* — route a single-task drain to the runtime. The
/// ring kept its 4.8 s, and the boot did not move: its batches are not size-1. (2) *The first
/// body of a batch has no peers yet* — route it and let the rest batch. The boot went to
/// `wall 66 s / Done 0.588`, and **the ring collapsed to 137,610 ms again**: on that workload
/// every body also arrives at an empty batch, because the drain runs between arrivals. ⇒ **The
/// two workloads are indistinguishable by the shape of the work; they differ in whether the
/// CALLER HAS ANYTHING ELSE TO DO**, and the engine cannot see that at the deferral point.
///
/// ⇒ The principled fix is to remove the choice: make the lane's drain stop parking main — run
/// the batch on workers and let the caller continue — which is a change to the isolation
/// invariant the lane is built on (`drain` holds main inside a native for the whole region on
/// purpose), and needs its own arc.
pub fn offload_first() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_OFFLOAD_FIRST").is_some())
}

pub fn async_any_executor() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ASYNC_OFFLOAD").is_none())
}


/// `RAVA_CF_SUBMIT=1` — hand a `supplyAsync` supplier to the EXECUTOR IT WAS GIVEN, instead of
/// running it on the calling thread.
///
/// ⭐**This is the contract, and the measurement that demands it** (ledger rows 367-371). Today
/// `supplyAsync(supplier, executor)` falls through to `vm.apply(supplier)` inline once the off-GIL
/// lane and the guest-task queue refuse it, and a JFR profile of `java -Xint` shows the JVM never
/// runs the resulting work during boot at all: **`ChunkGeneratorStructureState` 0 samples,
/// `findBiomeHorizontal` 0, `generateRingPositions` 0**, while rava enters `ImprovedNoise#p`
/// **251,523,925** times inside `ServerLevel.<init>`. That is **91% of the compilers-off wall**.
///
/// ⛔**And the executor is not decoration.** `Util.blockUntilDone(fn, donePredicate)` passes MC its
/// OWN queue-backed executor and then drains that queue itself in a loop. Parking the supplier
/// (`RAVA_LAZY_CF`) leaves the queue empty, the predicate never turns true, and the boot spins
/// forever — which is exactly what four attempts measured before a park-time backtrace named the
/// bodies as `ReloadableServerRegistries.scheduleRegistryLoad` under `blockUntilDone`.
/// ⇒ Submitting is the one behaviour that satisfies BOTH: the queue-backed executor gets its task
/// and drains it; a real background executor gets its task and boot never waits on it.
pub fn cf_submit() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_CF_SUBMIT").is_some())
}

pub(crate) fn default_field_kind(descriptor: &str) -> u8 {
    match descriptor.as_bytes().first() {
        Some(b'J') => 1,                       // FK_LONG
        Some(b'F') => 2,                       // FK_FLOAT
        Some(b'D') => 3,                       // FK_DOUBLE
        Some(b'L') | Some(b'[') => 6,          // FK_NULL (ref default = null)
        _ => 0,                                // FK_INT (I/Z/B/C/S)
    }
}

/// `str::find(char)` builds a `CharSearcher` (UTF-8 encode + a char-boundary
/// re-check loop over memchr) per call; a descriptor is pure ASCII and 3-60 bytes
/// long, so the searcher costs more than the search. RIP sampling of block C puts
/// `core::str::next_match` at 1.5% self with `args_of` its 68% caller — this is
/// that searcher, called on EVERY `jit_call_body` / `jit_indy` callback that needs
/// an arity. A plain byte scan answers the same question with no setup.
/// Semantics are unchanged: FIRST `(` and FIRST `)`, both required, `(` first.
#[inline]
pub(crate) fn byte_find(s: &str, b: u8) -> Option<usize> {
    s.as_bytes().iter().position(|&c| c == b)
}

#[inline]
pub(crate) fn byte_rfind(s: &str, b: u8) -> Option<usize> {
    s.as_bytes().iter().rposition(|&c| c == b)
}

/// Argument-list slice of a descriptor: `(II)V` → `II`.
pub(crate) fn args_of(descriptor: &str) -> &str {
    match (byte_find(descriptor, b'('), byte_find(descriptor, b')')) {
        (Some(a), Some(b)) if a < b => &descriptor[a + 1..b],
        _ => "",
    }
}

/// The return-type substring (everything after ')'), for exact descriptor match.
pub(crate) fn ret_of(descriptor: &str) -> &str {
    match byte_rfind(descriptor, b')') {
        Some(p) => &descriptor[p + 1..],
        None => "",
    }
}
