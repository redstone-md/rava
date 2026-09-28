# RAVA_LAMBDA_PIC — status: EXPERIMENTAL (default OFF)

Verdict date: 2026-07-23. RAVA commit at verdict: 94fb506 (Phase A frozen protocol:
docs/phase-a-protocol.md, at commit 0ec12b4). NOT a performance improvement.

## What it is
`RAVA_LAMBDA_PIC` (jit.rs:886, default OFF) serves non-capturing ("directable")
lambda receivers from the machine-code heap-Ref PIC by identity — remaps the shared
"$Lambda" class word to `(recv_cell<<1)|1` and probes the same 4-way cache
(`emit_lambda_key_select` / `Vm::lambda_pic_fill`). Targets the ~13M dispatch-miss
falls on lambda receivers per canonical run.

## Current evidence
- CORRECTNESS: no divergence on the expanded supported conformance matrix
  (JVM == RAVA lambda-PIC OFF == ON), 13 tests, deterministic over repeats
  (corpus/conformance/, run via `bash corpus/conformance/run.sh`). Categories:
  mono/poly/mega dispatch, interface + default methods, null-receiver NPE,
  capturing/non-capturing lambdas, static/bound/unbound/ctor method refs, nested
  lambdas, lambda-returning-lambda, lambda throw across dispatch, invokedynamic
  concat, boxing + primitive streams, reflection, class-init/late-linkage, native
  string boundary.
- PERFORMANCE: no material aggregate win on the Phase A performance corpus. Cold
  boot, ABBA paired, correctness-verified every run:
    * ComputeBench (n=8 pairs): OFF wall median 6124 ms, ON 6059 ms (-1.1%);
      CPU -2.4%. NEUTRAL (< 3% threshold, ~3% CoV). Raw: bench/phaseA_compute.csv.
    * StringBench (n=8 pairs): OFF wall median 5847 ms, ON 5847 ms (0.0%);
      CPU -0.3%. NEUTRAL. Raw: bench/phaseA_strings.csv.
    * Minecraft boot (n=3/4 partial, task stopped): raw ON -2.6% wall / -4.0%
      Done-timer, BUT the ABBA sequence's first run was OFF on a cold page cache
      (wall 231.5 s, the max) — a sequencing confound. Excluding that cold first
      OFF run: OFF* wall median 220.2 s vs ON 221.2 s = +0.47% (NEUTRAL). The
      apparent MC win is a cold-cache artifact, not λ-PIC. Raw: bench/phaseA_mc.csv.
      Consistent with the earlier measurement-based rejection (commit c9947ad).

## Pre-registered thresholds (frozen, not altered after results)
- Material aggregate improvement: >= 3% median on >= 2 performance-valid workload classes.
- Per-workload material regression: >= 2% median slowdown or reproducible p95/p99/RSS regression.
- Result: 0 workload classes reached +3%; no regression >= 2%.

## Decision
- DEFAULT-ON: rejected (threshold not met).
- EXPERIMENTAL / default-OFF: accepted. Flag + code retained for future
  dispatch-heavy SUPPORTED workloads (no regression, correctness-clean).
- REJECT/remove: not required.
Do NOT enable by default; do NOT describe as a performance improvement.

## Machine manifest (Phase A)
Intel Core i5-14400F (6P+4E, 16 logical), 31.8 GB, Windows 11 Pro build 28000,
High-Performance power plan, P-core affinity 0x0FFF. JVM reference Temurin-25.0.3+9
LTS. Rust 1.94.1, cargo release (opt-level=3, lto="thin", target-cpu=native),
features next-gen-gc. Fixed env RAVA_JIT=1 RAVA_PROC_CAP_MB=4096 RAVA_HARD_MB=3584
RAVA_JIT_TIER0=1 (+ RAVA_GSON_NATIVE=1 RAVA_M3_BITSTORAGE=1 for MC); only
RAVA_LAMBDA_PIC varied.

## Immutable commands
- build: `cargo build --release -p rava-cli --features next-gen-gc`
- ComputeBench: `rava.exe -cp "out;corpus/compute" ComputeBench` (expect CHECKSUM=5411900725941656235)
- StringBench:  `rava.exe -cp "out;corpus/strings" StringBench`  (expect CHECKSUM=689452)
- MC boot: net.minecraft.server.Main --nogui, cp=bench/mc-cp.txt, world restored from world_pristine per run
- conformance: `bash corpus/conformance/run.sh`

## Scope
Applies ONLY to the tested SINGLE-THREADED SUPPORTED Java subset — not a general
claim over all Java workloads / JARs / JVMs / multithreaded Java. Phase A raw CSVs
and the frozen protocol are IMMUTABLE evidence; new experiments must use new
CSV/commit and compare explicitly against this baseline (do not overwrite).
