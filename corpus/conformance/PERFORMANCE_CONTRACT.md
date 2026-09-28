# Workload correctness classification (universal-engine mandate)

Gate rule: a workload is PERFORMANCE-VALID only if JVM == RAVA(JIT1,λ-PIC OFF) ==
RAVA(JIT1,λ-PIC ON) on output/checksum/exception/exit, stably across repeats.

## performance-valid (correctness-clean, JVM==OFF==ON verified)
- ComputeBench  (corpus/compute)   — CHECKSUM=5411900725941656235
- StringBench   (corpus/strings)   — CHECKSUM=689452 (order-independent; re-verify
    over several repeats before treating cold-boot numbers as authoritative)
- Minecraft server boot (bench/)   — Done ~114-120s

## correctness-probe (semantic checks only, not for perf numbers)
- conformance/: PicShapes, LambdaIndy, NativeStrings, Reflect, ClassLoad,
  DispatchMatrix, ExcMatrix, ExcXfer, ThrowKind, EscapeField, StreamRepro,
  ConcatRepro, StrOrderIndep  — all JVM==OFF==ON on JIT=1.

## unsupported (excluded from gate + perf corpus)
- Concurrency.java — java.util.concurrent multi-threading: RAVA lacks
  CountDownLatch.await (UnsatisfiedLinkError). OFF==ON (no λ-PIC signal). Guest
  Thread/latch coordination not supported.

## known non-λ-PIC defects (do not block λ-PIC gate; logged for follow-up)
- Reflect on JIT=0 (interpreter) diverges: reflective invoke of an INSTANCE method
  with a null receiver (Method.invoke(null,...)) stops early; JIT=1 (OFF & ON) and
  JVM all correct. Interpreter-only reflection edge case, orthogonal to λ-PIC.

## SCOPE (frozen 2026-07-23)
RAVA performance/correctness claims cover the SINGLE-THREADED SUPPORTED Java subset
ONLY. Guest multithreading / java.util.concurrent — including CountDownLatch.await —
is currently UNSUPPORTED (UnsatisfiedLinkError). Do NOT claim "any Java code/JAR".
Concurrency support is a separate large compatibility/runtime effort, NOT part of
Phase A. Phase A protocol is frozen in docs/phase-a-protocol.md.

## MC SERVER BOOT — default-on optimizations are NEUTRAL (2026-07-23)
A/B same build: defaults-ON (loop-eager + parallel-reduce) Done 120.3s / wall 232s /
peakWS 1019MB  vs  defaults-OFF (RAVA_NO_LOOP_EAGER + RAVA_NO_JIT_PARALLEL) Done
122.1s / wall 230s / 1041MB. Within ~10% Done noise → NO regression (slightly faster
ON). The cold-start optimizations (aimed at short programs) do NOT hurt the long
server boot: methods compile via threshold anyway on a long run (eager only shifts
WHEN; boot is execution-bound, compile ~2s/120s). canonical 8790 bit-exact with both.

## WHY RAVA LOSES ON MC BOOT (17x) DESPITE WINNING BENCHMARKS — root cause (2026-07-23)
RAVA's per-call MEGAMORPHIC dispatch is the killer. Quantified (50M/5M calls, startup-adj):
  4-way polymorphic (PolyCall, fits the 4-way PIC): RAVA ~7ns/call — COMPETITIVE (0.8-1.5x)
  20-way megamorphic (Mega, PIC overflow):          RAVA ~736ns/call — 37.6x SLOWER than JVM
JVM handles megamorphic near-flat (C2 profile-guided devirt / fast vtable); RAVA falls to a
full resolve + rava_invoke callback on every PIC-overflow site (~736ns).
The winning benchmarks (SumHeavy/Loop = pure arithmetic NO dispatch; PolyCall = 4-way fits PIC)
avoid this; MC boot is saturated with megamorphic dispatch (thousands of block-state types,
JDK collections/codecs with many impls, deep call chains) → the ~736ns/call tax x tens of
millions of megamorphic calls = the 17x gap. THE #1 MC-BOOT LEVER = megamorphic dispatch
(wide-PIC exists but regresses on JDK owners, see [[rava-dispatch-pic-upgrade]]); + single-core
GIL vs JVM's multi-threaded bootstrap. Both structural.
