# RAVA benchmark corpus (universal-engine mandate)

Diverse workload classes for Gate-B bridge decomposition + Phase-A validation.
Run env (all): RAVA_JIT=1 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584 RAVA_JIT_TIER0=1,
P-core affinity 0x0FFF. Profiling adds RAVA_BRIDGE_STAT=1 (profiling runs ONLY).

## 1. minecraft-server (real application startup)
- class loading, worldgen, streams, collections, I/O, large guest compute
- cmd: rava.exe -cp "out;<bench/mc-cp.txt>" net.minecraft.server.Main --nogui
- Done: stdout "Done (N.Ns)"; baseline Done-timer ~114-120s internal
- world restored from world_pristine before EACH run

## 2. compute (integer compute + collections + hashmap + string-build)
- javac --release 21; deterministic, no FP, no threads, no external deps
- cmd: rava.exe -cp "out;corpus/compute" ComputeBench
- Done: exit 0 + "CHECKSUM=5411900725941656235" + "DONE"
- verified BIT-EXACT vs JVM 25 (JVM ref = RAVA)
- RAVA wall ~15s

## KNOWN RAVA CORRECTNESS GAP (differential vs JVM, found building corpus)
- IntStream.range(0,N).mapToObj(Integer::toString).filter(t->t.length()>2)
    .mapToLong(t->(long)t.hashCode()).sum()  →  RAVA returns 0, JVM returns
    483413741200 (N=20000). Isolated via corpus/compute/Bisect.java (S4_stream).
  All other stages (int hash, Random+sort, HashMap.merge+entrySet, StringBuilder
  hashCode) are bit-exact. Universal stream-pipeline bug — logged, not yet fixed.

## TODO corpus
- 3rd class: Renaissance (one self-contained workload) OR a reflection/IO-heavy
  micro-JAR if Renaissance hits compat gaps (record FIRST unsupported feature).

## 3. strings (allocation + toString-heavy) — DIVERGES (correctness-bug workload)
- corpus/strings/StringBench.java: 30K Point objs, String.valueOf ×2 passes, 20K map keys
- RAVA CHECKSUM 4289008 vs JVM 4223750 (DIVERGE) — 2nd differential-correctness gap
  (suspected string-concat / valueOf-toString path). Also: RAVA ~100µs/valueOf,
  and 150K×3 form TIMES OUT >150s vs JVM <1s. Useful as a correctness+perf probe,
  NOT a clean checksum member.
- Analysis: with the dead-proof fix, String.valueOf(o) at native_depth==0 skips the
  proof, yet still ~100µs/call → cost = guest Point.toString() (string concat)
  execution + per-call overhead = GUEST-COMPUTE, not an isolable bridge lever →
  Phase-A (JIT_EXEC/dispatch) territory.

## RAVA CORRECTNESS GAPS FOUND (differential vs JVM 25) — for follow-up
1. IntStream.range().mapToObj(Integer::toString).filter().mapToLong().sum() → RAVA 0
   vs JVM 483413741200 (Bisect.java S4).
2. StringBench toString/concat checksum diverges (4289008 vs 4223750).
Both are universal (no MC specificity). Logged; not on the speed path.

## CORRECTNESS GATE (WS-C1) RESULTS — 2026-07-23
- BUG 1 (IntStream mapToLong.sum → 0): REAL RAVA bug, ROOT CAUSE = stream.rs "sum"
  used as_int() on every element + returned Value::Int; Long/Double elements →
  as_int()==None → 0. FIXED type-aware (infer double/long/int from homogeneous
  elements). Isolated to mapToLong via StreamRepro (mapToInt worked). OFF==ON
  before+after (base bug, not λ-PIC). Now JVM==OFF==ON.
- BUG 2 (StringBench checksum divergence): NOT a RAVA bug — the test interleaved
  `acc += keyLen; acc ^= val` over HashMap.entrySet(), whose iteration order is
  UNSPECIFIED in Java. RAVA's legal (different) order → different order-dependent
  checksum. Fixed the TEST (order-independent accumulators). StrBisect proved all
  order-independent stages already matched. StringBench now JVM==OFF==ON=689452.
- Permanent conformance suite: corpus/conformance/{StreamRepro,ConcatRepro,
  StrOrderIndep}.java + expected/*.txt (JVM-recorded) + run.sh (diffs RAVA
  lambda-PIC OFF & ON vs JVM, CR-normalized). ALL PASS: JVM==OFF==ON.
- Regressions: canonical bit-exact 8790f04ace726ecf; cargo test 360 pass;
  ComputeBench + StringBench bit-exact.

## λ-PIC CORRECTNESS MATRIX (WS-C2) — 2026-07-23: GATE BLOCKED
DispatchMatrix.java categories vs JVM / RAVA lambda-PIC OFF / ON:
- C1 mono/poly/megamorphic virtual+interface dispatch: JVM==OFF==ON ✓
- C2 non-capturing + capturing lambdas + method refs: ✓
- C3 default methods + megamorphic BiFunction: ✓
- C4 invokedynamic string concat (mixed types): ✓
- C5 boxing + primitive streams (Int/Long/Double sum, incl. the mapToLong fix): ✓
- C6 exception thrown from lambda, caught, getMessage().hashCode(): **FAIL (base bug)**
- C7 reflection Class.forName: blocked behind C6 (untested)

BUG 3 (base RAVA, blocks the gate): a caught exception's getMessage() returns NULL
in JIT-COMPILED code → uncaught NullPointerException at String.hashCode. Isolated:
- DM2.java (C2 lambdas + C6 loop) reproduces; DM3/DM4 (single throw) pass.
- DM2 RAVA_JIT=0 PASSES (C6=495152); RAVA_JIT=1 (±tier0) FAILS → JIT CODEGEN bug in
  the exception-catch → getMessage() path. Trigger: main() gets hot (the C2 loop)
  → compiled → the compiled catch mishandles the caught exception's detailMessage.
- OFF==ON (both fail with JIT) → NOT a λ-PIC regression.

DECISION GATE (correctness): BLOCKED. Base bug OFF≠JVM (default JIT config) must be
fixed before the λ-PIC matrix can pass. λ-PIC stays default-OFF/experimental.
Phase A performance matrix NOT unlocked. Next: root-cause the JIT exception-handler
codegen (how the compiled catch materializes the exception object / detailMessage).

## BUG 3 ROOT CAUSE (WS-C3) — JIT compiled-thrower loses exception message
Precise isolation (permanent regression tests ExcMatrix/ExcXfer/ThrowKind):
- Exception bisection (ExcMatrix A-K): JIT=1 diverges ONLY on G (interp callee) &
  H (compiled callee) — exception thrown in a CALLEE, caught in a compiled caller.
  Same-method throw/catch (C_msg) PASSES → getMessage codegen itself is fine.
- ExcXfer: the arriving exception has the CORRECT CLASS (RuntimeException) but
  getMessage()==null once compiled (i<3 correct, i>=2999 null).
- ThrowKind (decisive): compiled thrower with LITERAL msg → null, CONCAT msg →
  null, but PARAMETER msg → CORRECT. RAVA_JIT=0 all correct; RAVA_JIT=1 lit/concat
  fail. Handler-bearing methods are excluded from JIT (jit.rs:~9657) so the
  try/catch method interprets (correct); the THROWER (no handler) compiles.
MECHANISM: a message String created INSIDE the compiled thrower (LDC constant or
StringConcat result) is a frame-local BOXED value (jit_box_table index). It is
NOT materialized into the exception's $message before the thrower's frame unwinds;
jit_run's box_truncate(box_base) then drops the box → $message dangles → null. A
PARAMETER string lives in the caller's box space → survives. rava_putfield (2928)
DOES materialize refs; the native throwable() <init> (natives.rs:3008) stores a
real Value::Str — so the compiled thrower must BYPASS both (inlined ctor / the
message never reaches a materializing store). FIX SITE (next): the compiled
exception-construction / inline-ctor path must materialize the boxed message (and
any boxed ref stored into an escaping object's field) before frame unwind — the
universal rule "a heap field must never hold a frame-local box index". Candidate:
rava_throw (jit.rs:593) deep-materialize the thrown exception's boxed fields
before box_truncate, OR fix the inline-ctor field store to materialize like
rava_putfield. NOT YET FIXED — λ-PIC gate remains BLOCKED.
Status wording: λ-PIC = "no regression observed on current conformance matrix",
NOT globally correctness-safe.

## BUG 3 FIXED (WS-C3) — 2026-07-23
REFINED root cause (premise corrected): the general heap-store path is ALREADY
safe — rava_putfield/aastore materialize via unbox_ref; EscapeField.java proves a
compiled literal/concat String stored into an object FIELD or ARRAY element and
escaping via return is read back correctly on JIT=1 (never broke). The single
unsafe crossing was the NATIVE-CALL ARGUMENT boundary: unbox_ref returns arena
strings as Value::Obj (B5 handle-based), and natives pattern-match Value::Str, so
an arena-string arg was silently missed (Throwable.<init> lost detailMessage →
getMessage()==null; DBG_EXC trace showed <init> receiving Value::Obj(ArenaPtr) for
LITERAL/CONCAT, Value::Str for PARAMETER).
FIX (general, natives.rs try_invoke entry): canonicalize every arena-string arg to
Value::Str at the one native-dispatch boundary → all natives see a uniform string
representation. Zero-cost when no arg is an arena string. NOT exception-specific.
VERIFIED: conformance suite ALL PASS JVM==JIT0==JIT1==λ-PIC-ON (ExcMatrix, ExcXfer,
ThrowKind, EscapeField, DispatchMatrix, StreamRepro, ConcatRepro, StrOrderIndep);
canonical bit-exact 8790f04ace726ecf; cargo test 360 pass; ComputeBench 5411900725941656235;
StringBench 689452; MC smoke boot Done 119.9s (normal range).

## WS-C4 EXPANDED λ-PIC CORRECTNESS MATRIX — 2026-07-23: GATE PASSED (supported)
New standalone deterministic tests (JVM-recorded expected, permanent):
- PicShapes  (mono/poly/mega dispatch, interface, default methods, null-receiver
  NPE, receiver alternation): JVM==JIT0==OFF==ON ✓
- LambdaIndy (capturing/non-capturing λ; static/bound/unbound/ctor method refs;
  nested λ; λ-returning-λ; λ throw across dispatch): ✓
- NativeStrings (literal/concat/param strings into ArrayList/HashMap/HashSet +
  String natives + null + exception-message-from-concat across native boundary): ✓
- Reflect (Method.invoke instance+static, Constructor.newInstance, expected
  NPE/ITE): JVM==OFF==ON ✓ (JIT=0-only edge divergence — see contract, not λ-PIC)
- ClassLoad (static-init order, inheritance, late linkage): JVM==OFF==ON ✓
- Concurrency: UNSUPPORTED (CountDownLatch.await UnsatisfiedLinkError); OFF==ON.
Repeatability: λ-PIC ON over 3 repeats = identical (no flake). Full suite (13
tests) ALL PASS JVM==OFF==ON on JIT=1. See PERFORMANCE_CONTRACT.md for the
performance-valid / correctness-probe / unsupported / known-defect classification.
VERDICT: expanded λ-PIC correctness gate PASSED on all SUPPORTED categories — no
ON≠OFF divergence anywhere. λ-PIC remains default-OFF; Phase A now unblockable.
