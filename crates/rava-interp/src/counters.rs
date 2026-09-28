//! Crate-wide counters: census peaks and compile tallies.
//!
//! Re-exported from the parent module, so no call site outside it changed.

/// (peak total bytes, that collection's snapshot).
pub(crate) static CENSUS_PEAK: std::sync::Mutex<(usize, Option<crate::heap::HeapCensus>)> =
    std::sync::Mutex::new((0, None));

/// Peak (by blocking-cell count) cell-census snapshot — a server boot has no clean exit
/// to print from, so keep the worst coverage seen and print only when it worsens.
#[cfg(feature = "next-gen-gc")]
pub(crate) static CELL_CENSUS_PEAK: std::sync::Mutex<u64> = std::sync::Mutex::new(0);
/// Peak LIVE OBJECT count seen by the header census — its own high-water mark, since
/// the cell census's blocking count is already 0 on the bench workloads.
pub(crate) static HDR_CENSUS_PEAK: std::sync::Mutex<u64> = std::sync::Mutex::new(0);

/// Increment 14.1 boot-dispatch inspector (RAVA_PROFILE_BOOT_DISPATCH): per-method invoke tally
/// split by execution tier — INTERPRETED (fell to opcodes::run) vs JIT (a compiled tier, cnp/aot/
/// cranelift, handled it). The dump surfaces the methods that are HOT in the interpreter yet NEVER
/// compiled (interp > 10k, jit == 0) — the real single-thread dispatch hotspots. Recorded at the
/// tier-decision choke point (Vm::invoke); a dispatch-miss total (compiled-code IC fallbacks) is
/// tallied separately for context. Gated (zero cost when off) and side-effect-only ⇒ math is inert.

/// Global mirror of `Vm::jit_compiled`, so JIT COVERAGE is readable from a real boot.
///
/// `jit_compiled` is a `Vm` field whose only reports are `dump_jit_stats` — reached from the
/// interpreter heartbeat, which a boot never triggers (ledger row 167) — and teardown, which a
/// boot never reaches. The number matters now: HotSpot's C1 compiles 8,035 times over 6,675
/// distinct methods in its 5.74 s boot, and rava's JIT multiplies the boot by 2.0x against
/// C1's 8.3x. Coverage is one of the two candidate causes and it was simply unmeasurable.
pub(crate) static COMPILED_TOTAL: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
/// Companions to [`COMPILED_TOTAL`], so "only 48 methods compiled" decomposes instead of
/// standing as a bare number: how many methods REACHED the compile decision, and how many were
/// refused there. Without the split, a low coverage figure could equally mean "nothing gets
/// hot", "everything fails to compile" or "an admission filter eats them", and those have
/// completely different fixes.
pub(crate) static COMPILE_TRIED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
pub(crate) static COMPILE_REFUSED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
/// Why a method fails to reach emitted code. Row 190 measured 42 compiled against C1's 6,675
/// with `refused=1`, so the loss is upstream — and the back-edge census then ruled out the two
/// obvious upstream candidates: only 9.2% of looping is in once-entered methods (so OSR is not
/// the prize) and the loop-heavy methods are entered 526-270,270 times (so the 512 invocation
/// threshold is not the wall either). `[0]` deny-list, `[1]` native-served/cache class, `[2]`
/// exact spline fallback retained when the per-Code HOF plan is empty.
pub(crate) static JIT_GATE: [std::sync::atomic::AtomicUsize; 8] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 8];
/// How far the warm-up counter actually gets. Incremented once per METHOD as its Cold count
/// crosses 64 / 128 / 256 / 512, so the shape of the distribution is visible instead of its mean:
/// 1.26 M bumps over 16,220 methods averages ~78 against a threshold of 512, but an average
/// cannot say whether that is "everything creeps" or "a few race ahead and the rest never move",
/// and those have opposite fixes.
pub(crate) static COLD_LEVEL: [std::sync::atomic::AtomicUsize; 4] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 4];
/// Why the compiled-code native fast path (`fu_fastpath`) declines. Row 197 measured a
/// compiled->native call at 28.4 ns against 0.7 ns for a guest call, and profiling ONE
/// `ArrayList.size()` showed the FULL native gauntlet — `dispatch_owner_cascade`,
/// `try_invoke_chain`, `owner_bits::bits`, `super_chain` — which means the fast path is not
/// serving despite `ArrayList.size()I` being on its own admitted list. `[0]` receiver is a
/// `Value::Ref` whose class did not match, `[1]` receiver is an ARENA `Value::Obj` whose class
/// did not match, `[2]` receiver is neither, `[3]` served.
pub(crate) static FU_WHY: [std::sync::atomic::AtomicUsize; 4] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 4];
/// Guest exceptions constructed by the VM. `_NLG_Return2` — the MSVC unwinder — shows at 1.15%
/// of the CLASS-LOADING window (ledger row 204), which is not where exception machinery belongs
/// unless something is throwing in bulk. A count is the cheapest way to tell "a few, unwound
/// expensively" from "millions, and that IS the phase".
pub(crate) static EXC_MADE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// TEST-ONLY override for `worker_locked_exec_enabled` (0 = unset, 1 = force off, 2 = force on).
///
/// The gate below is a process-wide `OnceLock` over an env var, which is the right shape for a
/// production read-once but leaves the two sides of the GAP-S09 contract untestable in one binary:
/// a worker ABORTS outside the locked envelope and PUBLISHES inside it, and which one a test sees
/// would otherwise depend on how the whole process was launched. Tests hold `parallel::TEST_LOCK`
/// while they set this, so the override is serialised with every other mode-sensitive test.
#[cfg(all(test, feature = "next-gen-gc"))]
pub(crate) static LOCKED_EXEC_TEST_OVERRIDE: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(0);

/// DIAGNOSTIC: RAVA_JIT_DENY=substr[,substr…] — a method whose `Class#method` string
/// contains ANY of the comma-separated substrings is never JIT-compiled/run (stays
/// interpreted). A plain class substring matches as before; an entry with `#` pins a
/// specific method (`ArrayMap$EntrySet$1#next`). Empty/unset = deny nothing. Read
/// once. The list+method form makes miscompile bisects over a candidate set practical.
#[cfg(feature = "next-gen-gc")]
/// DEBUGGER EXTENSION: RAVA_JIT_SENTINEL brackets each compiled call with a class-Arc
/// integrity scan (see jit_run) to NAME the codegen op that corrupts a heap Object.
pub(crate) static JIT_SENTINEL_FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Default-build twin of the sentinel latch (the static above is caught in the
/// feature gate two attributes up; the sentinel itself is feature-independent).
#[cfg(not(feature = "next-gen-gc"))]
pub(crate) static JIT_SENTINEL_FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// ⭐**THE SILENT `?`.** `jit_run` reaches the compile decision, passes the warm-up gate
/// (`JIT_GATE[7]`) and then calls `self.jit_plans(..)?`. When planning returns `None` that `?`
/// returns from the whole function WITHOUT incrementing a counter and WITHOUT marking the cache
/// — so the method is not compiled, not recorded as refused, and passes the same gate again on
/// its next invocation. A product boot reads `passed_warmup=19,838` against `tried=373`, and
/// nothing in the tree said where the other 98% went. `[0]` = planning returned None,
/// `[1]` = distinct methods that hit it (first time only), `[2]` = the cap refused.
/// `[0]` = warm-up passages that produced no compile, `[1]` = distinct methods whose PLANNING
/// failed, `[2]` = refused by the compiled-method cap, `[3]` = passages dropped because the
/// background queue was FULL.
///
/// ⚠`[0]` was shared by two unrelated faults — a planning failure and a full queue — so its
/// value could not be acted on: growing the queue is the fix for one and useless for the other.
/// `[3]` splits the queue half out while `[0]` stays the total.
/// Calls refused compilation because their class is served natively (or is a cache class).
/// This refusal used to be silent, and it is the largest interpreted population in the engine.
pub(crate) static NATIVE_SERVED_REFUSED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub(crate) static PLANS_NONE: [std::sync::atomic::AtomicUsize; 4] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 4];
