# Phase A — λ-PIC performance matrix: FROZEN PROTOCOL
FROZEN 2026-07-23 at RAVA commit 94fb506. Do NOT change workloads, commands, JVM
flags, RAVA flags, affinity, release profile, classpath, dataset, or Done criterion
after the first measurement without a NEW baseline + explicit incompatibility note.

## 1. Environment manifest (immutable)
- RAVA commit: 94fb5062a6d13477e8a2900416e547cb130dadf1
- Rust: rustc 1.94.1 (e408947bf 2026-03-25); cargo 1.94.1
- cargo profile: release (opt-level=3, lto="thin"); features: next-gen-gc;
  build: `cargo build --release -p rava-cli --features next-gen-gc`
  (.cargo/config.toml target-cpu=native applies)
- Binary: target/release/rava.exe (built 2026-07-23 01:27)
- JVM reference: Temurin-25.0.3+9-LTS (64-Bit Server VM), default flags, `--release 21` classes
- OS: Windows 11 Pro build 28000
- CPU: Intel Core i5-14400F (10 cores = 6P+4E, 16 logical)
- Power plan: High Performance (8c5e7fda-...)
- RAM: 31.8 GB
- P-core affinity mask: 0x0FFF (ProcessorAffinity)
- Fixed env: RAVA_JIT=1 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584 RAVA_JIT_TIER0=1
  RAVA_GSON_NATIVE=1 RAVA_M3_BITSTORAGE=1 (MC only). ONLY variable = RAVA_LAMBDA_PIC ∈ {0(OFF),1(ON)}.
- Clean-process: kill only own PID; MC world restored from world_pristine before EACH run.

## 2. Frozen workloads (single-threaded SUPPORTED Java subset ONLY)
- ComputeBench (corpus/compute): cmd `rava -cp "out;corpus/compute" ComputeBench`.
  performance-valid ONLY if every measured run prints CHECKSUM=5411900725941656235.
- StringBench (corpus/strings): cmd `rava -cp "out;corpus/strings" StringBench`.
  performance-valid ONLY after N=5 correctness repeats all == JVM (CHECKSUM=689452),
  and every measured run == 689452.
- Minecraft server boot (bench/): cmd net.minecraft.server.Main --nogui, cp=bench/mc-cp.txt.
  Verify startup outcome (reaches "Done (N.Ns)" + no uncaught fatal), not just timer presence.
- NO workload added/removed during an OFF-vs-ON comparison.

## 3. Experiment families
A. COLD boot: fresh process per run; OFF/ON in ABBA (not all-OFF-then-all-ON);
   ≥10 valid paired runs per workload where practical (5 = preliminary screen only);
   report boot-to-Done, wall, process CPU, JIT_EXEC, peak RSS, allocations if
   reliable, exit/semantic verification. Retain raw per-run data (CSV).
B. WARM/steady-state: repeated deterministic batches with explicit warmup; report
   throughput + per-op p50/p95/p99; establish (not assume) a stable phase; compile
   count/time + GC where available. (ComputeBench/StringBench are single-shot cold
   by construction — a steady-state variant runs the work-unit in a warmed loop.)

## 4. Measurement hygiene
- Every timed run must produce a correctness result. Invalid output / timeout /
  crash / failed verification / unsupported API / contaminated env = INVALID, not a
  datapoint; record each discarded run + reason.
- Primary metric = median; also min/max/p95/p99/n/coefficient-of-variation.
- Paired OFF/ON: per-pair % delta + aggregate median delta.
- No win claim when CIs overlap heavily or improvement < run-to-run noise.
- Wall vs CPU reported separately. MC: Done-timer AND end-to-end wall separate
  (they diverge materially: Done ~114-120s vs wall ~205-244s, 2.1x E-core variance).

## 5. λ-PIC bias guards
- OFF and ON share identical JIT threshold / code cache / GC / feature flags (only
  RAVA_LAMBDA_PIC differs). Fresh process ⇒ fresh PIC state per cold run.
- Verify ON does not merely shift work outside the measured region.
- 4-way PIC counters (hit-by-slot, miss, fill, evict/replace, megamorphic fallback)
  are OBSERVATIONAL ONLY and OFF in final timing runs unless overhead calibrated.

## 6. Pre-registered decision (thresholds FROZEN — do not alter after seeing results)
- Material aggregate improvement: ≥3% median improvement.
- Per-workload material regression: ≥2% median slowdown OR reproducible p95/p99/RSS regression.
- Below thresholds ⇒ NEUTRAL unless stronger evidence.
- DEFAULT ON iff: semantic verification passes every included run; no crash/timeout/
  deadlock/RSS-blowup/material-tail-regression; statistically+practically meaningful
  aggregate improvement on ≥2 distinct performance-valid workload classes; no workload
  materially regresses; improvement > normal variation; PIC counters coherently explain it.
- EXPERIMENTAL default-OFF if: real but narrow-dispatch-restricted; inconsistent;
  a meaningful workload regresses; corpus too small; effect below noise after repeats.
- REJECT if: aggregate neutral/negative; benefit relies on a workload-specific pattern;
  tail/memory/correctness/maintainability worse; fallback/megamorphic makes perf unpredictable.

## 7. Scope statement
Any conclusion applies ONLY to the tested SINGLE-THREADED, SUPPORTED Java subset —
NOT a general claim over all Java workloads/JARs/JVMs. Guest multithreading /
java.util.concurrent (CountDownLatch.await etc.) is UNSUPPORTED and out of scope.
