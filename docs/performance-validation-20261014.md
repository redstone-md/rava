# Runtime performance validation, 2026-10-14

This report measures generic JNI string operations and compiled calls against baseline
`bed0e219`. It does not establish a client FPS, server MSPT or whole-application throughput
improvement. All processes start cold; the changes introduce no application-specific
policies, class or heap snapshots, or persistent compiled-code caches.

## Artifact identity

The final local Linux release measurements use these exact binaries, built with the same
Rust release profile and Temurin JDK/class library 21.0.12.1+1:

| Artifact | SHA-256 |
|---|---|
| Baseline, `bed0e219` | `3a02afa064b01befe2e0b434f9f16bbe7b14bc485578e648e90cd019935cc3db` |
| Final candidate | `6d0c79c3c922aa4d542f37ecd49ed75eec644d36a9eb6a3977c6209530b66fac` |

The candidate includes compact-string JNI access, shared-return fast exits, register ABI
hints for eligible virtual/interface calls, and platform-native compatibility fixes.
[Pre-commit CI run 38506857225](https://github.com/redstone-md/rava/actions/runs/38506857225)
passed the semantic, JNI and patch checks on Linux, Windows, macOS Intel and macOS ARM64
with the pinned patch. The four documented helpful-NPE-message probes remain expected
failures. CI-built Paper executables have their separate artifact identities below.

[PR CI run 38510747236](https://github.com/redstone-md/rava/actions/runs/38510747236)
also passed all four platforms: 149 semantic probes passed plus four expected NPE-message
failures; JNI, patches and unit tests passed. It validates synthetic merge `b8d7e114`,
including newer main `7061` and PR head `51feb88`. The performance measurements below use
the earlier `bed0e219` source base, not the newer merged collector.

## JNI string getters

Ten cases, five cold processes per case and VM: **150 process samples**. Each process warms
the native loop for 2,000 iterations, then measures 20,000 calls. VM order alternates between
samples; every checksum is checked. JNI sample metadata does not record CPU affinity.
Values below are median nanoseconds per call, including the corresponding release for
character-buffer getters.

| Operation | UTF-16 units | Content | Baseline | Candidate | HotSpot |
|---|---:|---|---:|---:|---:|
| GetStringLength | 64 | ASCII | 15.24 | 9.39 | 5.78 |
| GetStringLength | 64 | UTF-16 | 15.92 | 9.37 | 5.77 |
| GetStringLength | 16,384 | ASCII | 472.89 | 10.68 | 5.78 |
| GetStringLength | 16,384 | UTF-16 | 650.01 | 13.74 | 5.78 |
| GetStringUTFLength | 4,096 | ASCII | 2,607.07 | 118.14 | 2,476.99 |
| GetStringUTFLength | 4,096 | UTF-16 | 7,605.95 | 228.73 | 3,406.28 |
| GetStringChars | 4,096 | ASCII | 203.02 | 108.43 | 2,433.91 |
| GetStringChars | 4,096 | UTF-16 | 222.48 | 122.74 | 269.43 |
| GetStringUTFChars | 4,096 | ASCII | 2,526.58 | 1,853.33 | 4,765.45 |
| GetStringUTFChars | 4,096 | UTF-16 | 8,188.51 | 6,913.03 | 8,185.58 |

GetStringLength avoids decoding the backing array merely to obtain its length. Other
getters avoid unnecessary intermediate UTF-16 vectors. HotSpot remains faster for length
queries in these cases; the UTF-length and character-buffer results concern these exact
inputs and APIs. Functional JNI tests also cover modified UTF-8 and checked regions; region
APIs are not independently timed here. Borrowed string contents must not cross Java
allocation, calls or safepoints.

## Shared-return fast exits

FastReturnBench uses recursive calls whose fast and slow results join at one return phi.
There are three cold processes per VM, with VM order rotated between processes. Each
process has three warm-up and three measured rounds; the reported value is the median of
the three per-process best rounds. These runs are pinned to available CPU 8.

| Nanoseconds per call | Baseline | Candidate | HotSpot |
|---|---:|---:|---:|
| Arithmetic result through shared return | 1.83 | 1.39 | 1.06 |
| Constant result through shared return | 1.75 | 1.33 | 1.02 |

All nine processes return checksum `141557760`. Candidate runtime is about 24% lower than
baseline in both cases, while remaining about 31% and 30% above HotSpot respectively.
The theoretical recursive-call count includes calls eliminated by inlining.

Generated code confirms the mechanism: the arithmetic fast path returns at offset `0x1e`
before the first prologue push at `0x1f`; the constant path returns at `0x1a` before its push
at `0x1b`. Baseline starts the prologue at `0x10`. Slow branches still enter the prologue.
Dump inspection used disposable binaries with only an equal-length dump-path replacement
because `/tmp` was unavailable; timing used the unmodified release binaries above.
FastExitSem matches HotSpot in default, optimizing-all and few-register modes, with graph
verification enabled for the latter two.

## ABI hints and rejected collector experiment

An isolated generated-code comparison proves that eligible virtual/interface argument
hints remove two register moves, six bytes, from a generic field-argument call site.
Profiled/cold sites and values live across calls retain their previous handling. Dispatch
timings are highly variable, with tier/inlining changes between processes; a stable runtime
benefit and a HotSpot dispatch advantage have not been established.

The proposed task-wide dirty-card drain was rejected. It regressed a focused scanner
comparison over hot metadata, while whole-VM allocation results stayed near noise. The
existing scanner remains; DirtyCardsSem and YoungCardsBench are retained for future work.

## Verified CI Paper startup

[Paper gate run 38509430246](https://github.com/redstone-md/rava/actions/runs/38509430246)
completed successfully. The downloaded [artifact 11869516741](https://github.com/redstone-md/rava/actions/runs/38509430246/artifacts/11869516741)
was checked against its source manifest and binary hashes. It applies patch SHA-256
`5f8f21d022298fc8bdde112601e12a7e94140862f088311ba48c8b15c15fef25` to `bed0e219`.
These x86-64-v2 CI executables are distinct from the local microbenchmark binaries:

| CI executable | SHA-256 |
|---|---|
| Baseline | `59ed7c3c5ede5029d5d3822a82c11a63b5770d886565b99d3db9e63543b273c0` |
| Candidate | `21ae929e940efffda4d5d15c1221079721d13b1b13afb0a184095ade963e2e28` |

Three fresh-world starts per VM, with order reversed in the second round: all nine compressed
logs contain a real `Done` line. The host was a four-vCPU AMD EPYC 7763 CI runner, using
Paper 1.21.1 build 133, seed `20260930`, view/simulation distance 10 and port 25599.

| Median | Baseline | Candidate | HotSpot |
|---|---:|---:|---:|
| Harness elapsed to readiness, seconds | 23.30947 | 23.02268 | 28.28892 |
| Paper logged Done duration, seconds | 22.817 | 22.573 | 27.774 |
| Peak resident memory, MB | 1,167 | 1,170 | 1,096 |

The candidate median elapsed time is 1.23% below baseline: three samples do not establish a
strong end-to-end gain. It is 18.62% below HotSpot, but most of that gap already exists in
baseline RAVA and cannot be attributed to this patch. Earlier local `03aa1a57` Paper figures
are superseded by this verified artifact comparison. Startup does not establish tick
performance. Client validation still needs a real modpack and graphics-capable host;
no client FPS or server MSPT improvement is claimed.

## Local tick-sprint follow-up

The existing `bench/tick_sprint.py` mobs scenario was run in six cold processes, ordered
baseline/candidate/HotSpot then HotSpot/candidate/baseline. Each process completed three
1,000-tick sprints. Binary hashes match the local baseline `3a02afa0`, candidate `6d0c79c3`
and HotSpot `2a207f5e7d075afa01d97f8048389a64432a44c4a5af0f5e77d6e286ec5f401d`.
This is another measurement of `bed0e219` plus the candidate patch, not main `7061` after merge.

| Reported average MSPT | Process 1: three sprints | Process 2: three sprints | Mean of six averages | Median of six averages |
|---|---|---|---:|---:|
| Baseline | 4.18, 3.97, 3.75 | 4.13, 3.83, 3.79 | 3.9417 | 3.90 |
| Candidate | 4.49, 3.75, 3.77 | 4.08, 3.78, 3.92 | 3.9650 | 3.85 |
| HotSpot | 3.63, 3.07, 2.98 | 3.19, 3.37, 2.82 | 3.1767 | 3.13 |

Candidate mean MSPT is 0.59% above baseline and 24.82% above HotSpot. Its median of sprint
averages is slightly below baseline, so the summary statistic changes the direction of that
small difference: **no stable baseline win is established**. HotSpot has lower reported
averages in this scenario. These medians are not tick p50; no per-tick p50/p95 was collected.

All six servers reached `Done`, acknowledged 350 summons and a `Spread 350` command, and
stopped gracefully with exit 0. The arena loads 64 chunks, with 250 cows and 100 villagers,
and zero connected players. Forceloading without players may suppress activation-dependent
AI; command acknowledgements do not verify live entity counts or simulation-state equality.
No entity/state checksum was collected. Profiler startup warnings occurred under RAVA and
HotSpot, so there is no validated hot-method profile for bottleneck attribution.

The seed remained `20260930`; complete configuration snapshots and hashes were retained.
Only the overworld was regenerated; copied base Nether/End worlds remained. The safe direct
process harness retained the scenario commands and waits: 20 seconds after `Done`, 30 after
arena preparation and 10 after spawning mobs. Exact JVM arguments were
`-jar paper-1.21.1-133.jar --nogui`, using defaults without JVM/GC tuning. Inherited CPU
affinity was 0–8, without individual-core pinning; port was `127.0.0.1:25599`, query/RCON off.
The validation bundle's `tick_validation/analysis.json`, `results.json`, `metadata.json` and
`host-and-config.json` retain raw commands, configuration/binary hashes and hashed server logs.

## Reproducing the generic checks

Set RAVA_BASE and RAVA_CANDIDATE to separately built release executables, and JAVA_HOME to
JDK 21. Check executable hashes before running; use the same host and rotate process order.
CPU pinning is optional where unavailable; record the choice rather than silently changing it.
PAPER_BASE denotes the fixed server directory with the seed/configuration and port above.

```sh
cargo build --release
cargo test --release
R="$RAVA_CANDIDATE" tests/check.sh all
mkdir -p bench/results/fast-return/classes bench/results/jni
javac -d bench/results/fast-return/classes bench/java/FastReturnBench.java
taskset -c 8 "$RAVA_CANDIDATE" -cp bench/results/fast-return/classes FastReturnBench 3
RAVA_OPT=all RAVA_OPT_VERIFY=1 RAVA_OPT_FEW_REGS=2 \
  "$RAVA_CANDIDATE" -cp bench/results/fast-return/classes FastReturnBench 3
javac -d bench/results/jni tests/jni/JniTest.java
cc -shared -fPIC -O2 -I"$JAVA_HOME/include" -I"$JAVA_HOME/include/linux" \
  -o bench/results/jni/libjnitest.so tests/jni/jnitest.c
"$RAVA_CANDIDATE" -Djava.library.path=bench/results/jni -cp bench/results/jni \
  JniTest string-bench 1 4096 20000 1
R="$RAVA_CANDIDATE" tests/jni/run.sh
bench/paper_ab.sh "binary:$RAVA_CANDIDATE"
python3 bench/tick_sprint.py -n 1 -s 1000t -r 3 -b "$PAPER_BASE" \
  -c bench/scenarios/mobs.txt "$RAVA_BASE" "$RAVA_CANDIDATE" "$JAVA_HOME/bin/java"
python3 bench/tick_sprint.py -n 1 -s 1000t -r 3 -b "$PAPER_BASE" \
  -c bench/scenarios/mobs.txt "$JAVA_HOME/bin/java" "$RAVA_CANDIDATE" "$RAVA_BASE"
```

For FastReturnBench, repeat three cold processes per VM in rotated order. For JNI, use five
processes per case and VM; kinds 0/1/2/3 select length/UTF length/chars/UTF chars, coder 0/1
selects ASCII/UTF-16. Substitute RAVA_BASE and the JDK `java` executable for each comparison.
Keep raw samples, checksums, host details and build manifests; do not turn a local microbench
ratio into an application-wide claim.
