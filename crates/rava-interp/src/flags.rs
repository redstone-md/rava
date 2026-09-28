//! Environment flags: every RAVA_* gate the crate reads, memoised on first use.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::value::Value;
use crate::*;
/// Is the JIT on for this process? Value-checked exactly as `Vm::new` checks it, so a caller
/// that has no `Vm` in hand asks the same question and cannot drift from it.
///
/// This exists because the GIL-free ring lane needs the answer. A worker thread never compiles
/// (its JITModule would be freed on thread exit, dangling the shared cache), so with the JIT off
/// every task the ring admits is guaranteed to take a direct miss on its first call.
pub(crate) fn jit_enabled_env() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("RAVA_JIT")
            .ok()
            .map(|v| !matches!(v.trim(), "0" | "false" | "no" | "off" | ""))
            .unwrap_or(false)
    })
}

/// RAVA_CENSUS=1: run `Heap::census` at every arena collection and report the PEAK-by-total
/// snapshot. Inert (one OnceLock read on a path that already walks the whole heap) when off.
pub(crate) fn census_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_CENSUS").is_some())
}
/// RAVA_CELL_CENSUS=1: at every arena collection, histogram the live graph's REFERENCE
/// CELLS by low-2-bit tag per owning class (`nextgen::cell_census`). This is the value-model
/// rewrite's COVERAGE meter: step 6's bare-deref flip (measured 2.43× on ref-deref-dense
/// code) needs 100% 0b00, and the ranked non-0b00 residue is the migration worklist.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn cell_census_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_CELL_CENSUS").is_some())
}
/// `RAVA_RECORD_SEMANTICS` (default OFF) — serve a record's synthesised
/// `hashCode`/`equals` (the `java/lang/runtime/ObjectMethods` bootstrap) with the
/// real JVM semantics: hashCode folds `31*h + Objects.hashCode(component)` over the
/// COMPONENT VALUES (wrapper hash rules for primitive cells, virtual `hashCode()`
/// for references — records recurse naturally through their own indy), and equals
/// compares components recursively (`==` for primitives, `Float/Double.compare`
/// bit rules for floats, virtual `equals()` for references). The legacy path
/// instead hashed/compared `value_to_string` RENDERS — semantically wrong vs the
/// JVM and O(flattened text) per call (the recorded reason GAP-P04 salt keys
/// measured neutral). A defensive depth cap in `opcodes::record_object_method`
/// falls back to the legacy path, so a pathological reference cycle cannot
/// overflow the host stack.
pub(crate) fn record_semantics_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_RECORD_SEMANTICS").is_some())
}

/// Stage-3 fastutil double-list iterator/size natives (`RAVA_NO_DAL_ITER` opts out).
///
/// MEMOISED because its call site is the FIRST condition in `Vm::invoke` and therefore ran on
/// EVERY invoke in the VM. On Windows `std::env::var_os` is `RtlQueryEnvironmentVariable` — a
/// linear walk of the process environment block, plus an allocation — so the gate cost more than
/// the natives it guards could save. RIP sampling put `RtlQueryEnvironmentVariable` at 1.9% of a
/// serial boot block with the caller chain running through `Vm::invoke` / `Vm::resolve_method`.
///
/// This is the same defect `jit::env_gate` was introduced for, which shipped at min −7.4% on
/// blocks A+B; that pass simply did not reach this site. Sound for the same reason: the only
/// writer of the environment is the CLI's config apply, which completes before the VM exists.
pub(crate) fn dal_iter_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_DAL_ITER").is_none())
}

/// DEFAULT-ON since 2026-07-29 (`RAVA_NO_JIT_ITER_SHORTCUT` opts out). Serves the `$elems`
/// Iterator hasNext/next at the compiled->interpreter boundary instead of through
/// `opcodes::invoke*`.
///
/// MEASURED: 16 boots, two batches with REVERSED order, pcore, interleaved.
///
/// ```text
/// clean wall  ON : 77.9  77.9  78.3  78.4  79.9
/// clean wall  OFF: 80.4  80.4  80.5  80.8  80.9  81.4
/// ```
///
/// EVERY clean ON run below EVERY clean OFF run - ON CLEAN - and min-of-N over all 16 gives
/// 77.9 vs 80.4 = **-3.1% wall**. Minima are the robust statistic here because contention can only
/// inflate a run, and the contended runs (wall > 90) appeared in BOTH arms.
///
/// Done is UNCHANGED (~22.4-24.2s either way), which is what the mechanism predicts: the iterator
/// loops are block-state/property iteration in the SERIAL pre-ring phase, exactly where the leaf
/// profile found them, while Done measures the ring.
///
/// It is the first non-null of six consecutive work-removal attempts, and the difference is that
/// it removes a PATH rather than making a step on that path cheaper: 7,000,000+ calls per boot no
/// longer build a synthetic Frame, register a GC root, or run `ref_parts` / `super_chain` /
/// the owner cascade (those four invoke-path leaves are 6.5-7.2% of wall, 81-83% reached from
/// `opcodes::invokevirtual`).
pub(crate) fn jit_iter_shortcut_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_JIT_ITER_SHORTCUT").is_none())
}

pub(crate) fn spline_native_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // 19.3: default-ON (bit-exact, cuts the 1.45M CubicSpline predicate-lambda dispatches).
    // Opt out with RAVA_NO_SPLINE_NATIVE for A/B or deopt.
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_SPLINE_NATIVE").is_none())
}

/// RAVA_TRACE_DMISS — name every newly-refused direct call. Read ONCE: its only test site sits
/// on the negative cache's 64-call re-probe, which a permanently-unresolvable callee reaches
/// millions of times per boot, and `std::env::var_os` on Windows is an env-block query behind a
/// lock plus a UTF-16 encode of the name.
pub(crate) fn trace_dmiss_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_TRACE_DMISS").is_some())
}

/// 20.1 Tier-0 JIT gate (RAVA_JIT_TIER0=1, default OFF). When on: (1) the compile threshold drops to
/// 16 so cold invoke-dense code (the DFU/Codec datapack reload) tiers up early, and (2) the
/// `callback_heavy` PERFORMANCE heuristic is skipped in the compiler — those methods compile with the
/// EXISTING correct codegen (invokes → jit_call), so it is bit-exact by construction. The CORRECTNESS
/// bails (exceptions / find_leaders / resolve_fields / per-opcode emit_body) are UNTOUCHED, so a method
/// with try/catch or an unsupported opcode still stays interpreted — no codegen regression.
pub(crate) fn jit_tier0() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_TIER0").is_some())
}

/// Native fastutil IntList.getInt gate (default-ON; opt-out RAVA_NO_IL_GET). Cached in a OnceLock
/// so the hot Vm::invoke intercept does NOT do a per-call getenv (the env read happens once).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn il_get_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_IL_GET").is_none())
}

/// 20.2 gate for the fastutil IntList.get(I)->Integer boxing intrinsic (default-ON; opt-out
/// RAVA_NO_IL_BOXING). Cached in a OnceLock so the hot Vm::invoke intercept does no per-call getenv.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn il_boxing_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_IL_BOXING").is_none())
}

/// M3 chunk-fill fork gate (RAVA_M3_CHUNK_FILL_READY=1). Default OFF ⇒ `try_fork_chunk_fill_loop` is
/// inert (byte-identical serial bytecode). Flipped only once the coordinate-map detector + parallel
/// executor land (10.2), on the ChunkFill polygon proven independent + on the 11× RavaPool.
#[allow(dead_code)]
pub(crate) fn chunk_fill_ready() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RAVA_M3_CHUNK_FILL_READY").ok().as_deref() == Some("1"))
}

/// ⚠ `RAVA_LANE_NOLOCK` (default OFF) — skip the worker's resolve-lock acquisition entirely.
/// DELIBERATELY UNSOUND: it exists to price the gil-free arc before paying for it, and must never
/// become a default. Results may be wrong and the run may crash; the wall is still informative,
/// because it measures how fast eight workers CAN go. See the call site in `jit_call`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn lane_nolock_experiment() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_LANE_NOLOCK").is_some())
}

/// P3-S1/S2-lite gate (RAVA_M3_WORKER_LOCKED_EXEC=1, default OFF): a GIL-free worker reaching a
/// non-direct callee in jit_call EXECUTES it under the re-entrant resolve lock instead of
/// aborting the whole region (see the guard in jit_call). Pairs with the freeze-resolve table.
/// Populate the locked-callee composition tally (`RAVA_LANE_PROBE` implies it).
/// Read once — this sits on the resolve-lock acquisition path.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn lane_locked_tally_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_LANE_PROBE").is_some() || std::env::var_os("RAVA_FORK_TRACE").is_some()
    })
}

/// DEFAULT ON (`RAVA_NO_WS_SET_CONTAINS` opts out) — let a gil-free worker serve
/// `Set.contains` from an already-synced membership index instead of falling
/// through to the global resolve lock. Read once; the arm itself is `&self`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn ws_set_contains_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_WS_SET_CONTAINS").is_none())
}

/// OPT-IN (`RAVA_WS_OPTIONAL=1`) — let a gil-free worker read an immutable
/// `java.util.Optional`'s `$present`/`$val` instead of falling through to the global
/// resolve lock. Read once; the arm itself is `&self`.
///
/// ⚠ NOT default-ON, and the reason is the measurement, not the mechanism. 8 interleaved
/// ABBA pairs on the full boot: 6 favour ON, 2 favour OFF, mean 74.47 → 72.33 s
/// (−2.9%), process CPU −4.0%. The ledger's ACCEPT bar is "ON repeatably below OFF in
/// EVERY pair with no overlap", which this misses — the same verdict the ring lane
/// itself got before it earned its default. The arm removes ~226 k locked acquisitions
/// per ring task (≈28.7 M of the region's 82.5 M) and buys ~3%, which is one more
/// instance of the tree's standing lesson that a lock/callback COUNT does not price
/// host time.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn ws_optional_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_WS_OPTIONAL").is_some())
}

/// EXPERIMENTAL sub-gate (RAVA_M3_CHUNK_FILL_PARALLEL=1, default OFF). When OFF, the chunk-fill
/// executor runs its density map STRICTLY SERIALLY (the 10.2b path — bit-exact, no crash). When ON,
/// it fans the map across GIL-free workers. ⚠️ The parallel path is currently UNSAFE for the real
/// DensityFunction.compute tree: it re-enters `&mut Vm` per polymorphic call (dispatch_miss torn-writes
/// the SHARED baked inline-cache pair on the megamorphic interface site → bogus dispatch → abort). This
/// is the G5 `Vm &mut self` wall — bit-exact parallel needs per-thread dispatch/IC state, a separate
/// infra increment. Kept as an opt-in so the executor + scaling harness can be iterated safely.
#[allow(dead_code)]
pub(crate) fn chunk_fill_parallel() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RAVA_M3_CHUNK_FILL_PARALLEL").ok().as_deref() == Some("1"))
}

/// Count argument values in a method descriptor (one per arg, slots ignored).
/// Default-initialised field kind (FK_*) for a field of the given descriptor —
/// the kind a JVM-default (0 / 0L / 0.0f / 0.0 / null) value of that type has.
/// Used to pre-populate per-class layout slots so an unwritten field decodes
/// exactly as the JVM's default. Cell value is always 0; only the kind varies.
/// Whether per-class layout pre-population (fixed object field slots) is active.
/// Cached one-time env read. Gates both alloc pre-population (opcodes) and the
/// JIT constant-slot get/putfield (correct-and-fast when objects carry their
/// full layout; a verify+callback guard keeps non-pre-populated objects correct).
/// Default ON (boot-verified: Done + McProbe play, 0 new errors, RAM +2.4%, boot
/// time unchanged). Opt out with RAVA_NO_LAYOUT_ALLOC=1 if a gameplay path
/// regresses; RAVA_LAYOUT_ALLOC=1 also still forces it on.
pub(crate) fn layout_alloc_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RAVA_NO_LAYOUT_ALLOC").is_err())
}

/// Cranelift-AOT numeric tier (RAVA_AOT_CLIF): route eligible pure-int statics through
/// aot_clif + the AOT disk cache instead of the cnp x86 emitter. Opt-in — the
/// integration seam for the Cranelift AOT tier, off by default until the translator
/// covers the callback-carrying shapes that actually dominate boot.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn aot_clif_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // Relocatable Cranelift AOT is available on x86-64 Windows/Linux.
    #[cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))]
    {
        *ON.get_or_init(|| std::env::var_os("RAVA_AOT_CLIF").is_some()
            || crate::aot_clif::prepared::enabled())
    }
    #[cfg(not(all(any(windows, target_os = "linux"), target_arch = "x86_64")))]
    {
        *ON.get_or_init(|| false)
    }
}

/// The LLVM-AOT tier (RAVA_LLVM_AOT): compile hot numeric methods with clang -O2 for
/// near-C2 codegen. Requires a `clang` toolchain (build-time only); off if absent.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn llvm_aot_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // `llvm_aot` consumes `cnp`, which is compiled only on windows-x86_64, so the probe that asks
    // whether clang is present cannot even be NAMED on another target. Off by construction there.
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        *ON.get_or_init(|| std::env::var_os("RAVA_LLVM_AOT").is_some() && crate::llvm_aot::clang_available())
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    {
        *ON.get_or_init(|| false)
    }
}

/// Opt-in `RAVA_RECEIVER_GUARD` — arms the stale-arena-receiver forensic trap in
/// `jit_call` (zero cost when unset; never on a default build).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn receiver_guard_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_RECEIVER_GUARD").is_some())
}

pub(crate) fn jit_sentinel_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_SENTINEL").is_some())
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn jit_deny_class(class_name: &str) -> bool {
    let (subs, _) = deny_tables();
    subs.iter().any(|sub| class_name.contains(sub.as_str()))
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn jit_deny_method(class_name: &str, method: &str) -> bool {
    let (_, exact) = deny_tables();
    !exact.is_empty() && exact.contains(&format!("{class_name}#{method}"))
}

/// Capturing-lambda direct-call. A capturing kind-6 static-ref lambda (e.g.
/// `IndexMerger$IndexConsumer.merge`, the #1 refused direct on the boot shape-merge
/// path) is served by calling its synthetic impl method directly, prepending the
/// captured values as the leading locals. V3: DEFAULT-ON (kill-switch
/// `RAVA_NO_CAP_LAMBDA=1`). Keyed by `lam_fp` so an oligomorphic site (merge sees
/// several impls) caches each impl as its own N-way entry, with the hit-path fp
/// re-check reading the LIVE handle (the historic guard bug fed the class-ptr into
/// `lambda_fp`/`lambda_of`). Verified bit-exact: RingScan 109205b3, ChunkFill
/// 8790f04ace726ecf, ngg 228; measured IndexConsumer.merge 0%→99% reducible.
pub(crate) fn cap_lambda_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_CAP_LAMBDA").is_none())
}

/// ⭐**ONE SWITCH FOR THE WHOLE "EMIT MORE, CROSS LESS" BLOCK** (`RAVA_NO_EMIT_MORE=1`).
///
/// Seven families of compiled-code host callbacks were replaced by emitted machine code in one
/// pass, each with its own kill switch. Those switches bisect a REGRESSION; this one prices the
/// BLOCK, and the block is what has to be priced: the whole premise of doing seven at once is that
/// the host side is flat (547 callees, top row 5.06%) and no single family is worth more than ~1%
/// of the boot — thirteen consecutive single-family optimisations measured zero for exactly that
/// reason. A per-family A/B would therefore report seven more zeros and miss their sum.
///
/// Every one of the seven consults this in addition to its own switch, so a single same-binary
/// boot A/B against this flag is the block's price.
pub(crate) fn emit_more_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_NO_EMIT_MORE").is_some())
}

/// ⭐**ROUND 3 IS OPT-IN (`RAVA_FAST_NATIVES=1`), BECAUSE THE BLOCK MEASURES SLOWER.**
///
/// The third rewrite round made this engine's own `java.util` natives cheaper per operation — the
/// map key, the map table, arrays, strings, streams — and each area, switched off alone on single
/// boots, looked like it was earning 4-13%. An ABBA block test says otherwise: three boots each,
/// alternating, CPU-s median **41.6 with the block ON against 40.4 with it OFF**, i.e. ~3% SLOWER,
/// and the single-run per-area readings were noise (one of them, `MAPKEY`, read 45.7 s against a
/// 40.0 s default in the same loop — a 14% swing no code change in that file can produce).
///
/// It is kept, not deleted, for two measured reasons: `Done` is BETTER with the block on (0.465
/// against 0.489 median), so the work helps the steady-state tick while costing the one-shot load;
/// and every area is bit-exact on 5 hash gates and matches `java -Xint` on 21 differential probes,
/// so it is correct code waiting for a workload that wants it.
pub(crate) fn fast_natives() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_FAST_NATIVES").is_some())
}

/// ⭐**ROUND 7 IS OPT-IN (`RAVA_ROUND7=1`), BECAUSE THE BLOCK MEASURES SLOWER — THE SECOND TIME
/// THAT HAS HAPPENED, AND THE TWO CASES HAVE THE SAME SHAPE.**
///
/// Rounds 4 and 6 removed WRONG COMPLEXITY from paths that were genuinely doing too much work
/// (a linear scan over ~40 classpath sources per resource query; an `Arc<str>` built 24 million
/// times a boot; a registry dedup that made class registration O(N^2)) and both measured DISJOINT
/// wins. Rounds 3 and 7 added MEMOS, MIRRORS AND GATE RECORDS to paths that were already cheap —
/// and both measured slower: round 3 by ~3%, this one by 2.3% median / 3.7% on the minimum
/// (ABBA, three boots per arm: ON 39.5/39.5/40.3 against OFF 38.6/39.9/38.1).
///
/// ⇒ The rule this pays for: **a probe into a new table is not free, and on a path whose scan is
/// two to four entries over an array the caller already loaded, the table loses.** This tree had
/// already measured exactly that once (30.7 ns for a plain scan against 34.1 for a 196 KB memo and
/// 35.9 for a 6 KB one) and round 5 honoured it with a `FAST_MIN` threshold. Rounds 3 and 7 did
/// not, and paid.
///
/// Kept rather than reverted: every area is bit-exact on 5 hash gates and matches `java -Xint` on
/// 21 differential probes, and several of the fixes are algorithmically right (the collector's
/// cell loop is now O(reference cells) instead of O(cells), the arena field access resolves slot
/// and kind in one probe instead of two). They are correct code waiting for a workload whose
/// objects are wide enough for the table to win.
pub(crate) fn round7() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ROUND7").is_some())
}
