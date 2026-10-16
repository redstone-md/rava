# Runtime performance validation, 2026-10-14

This report measures generic JNI string operations and compiled calls against baseline
`bed0e219`. It does not show any improvement in client FPS, server MSPT or whole-application
throughput. Every process starts cold. The changes add no application-specific policies, no
class or heap snapshots and no persistent compiled-code caches.

## Artifact identity

The final local Linux release measurements use these exact binaries. Both were built with the
same Rust release profile and Temurin JDK/class library 21.0.12.1+1:

| Artifact | SHA-256 |
|---|---|
| Baseline, `bed0e219` | `3a02afa064b01befe2e0b434f9f16bbe7b14bc485578e648e90cd019935cc3db` |
| Final candidate | `6d0c79c3c922aa4d542f37ecd49ed75eec644d36a9eb6a3977c6209530b66fac` |

The candidate includes compact-string JNI access, shared-return fast exits, register ABI
hints for eligible virtual/interface calls, and platform-native compatibility fixes.
[Pre-commit CI run 38506857225](https://github.com/redstone-md/rava/actions/runs/38506857225)
passed the semantic, JNI and patch checks on Linux, Windows, macOS Intel and macOS ARM64
with the pinned patch. The four documented helpful-NPE-message probes still fail, as
expected. The CI-built Paper executables have their own artifact identities, listed below.

## JNI string getters

Ten cases, five cold processes per case and VM, 150 process samples in total. Each process
warms the native loop for 2,000 iterations and then measures 20,000 calls. VM order
alternates between samples, and every checksum is checked. The JNI sample metadata does not
record CPU affinity. The values are median nanoseconds per call. For the character-buffer
getters they include the matching release call.

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

GetStringLength no longer decodes the backing array just to get its length. The other getters
skip intermediate UTF-16 vectors they did not need. HotSpot is still faster for length queries
in these cases. The UTF-length and character-buffer results apply to these exact inputs and
APIs only. Functional JNI tests also cover modified UTF-8 and checked regions, but the region
APIs are not timed separately here. Borrowed string contents must not cross a Java
allocation, a call or a safepoint.

## Shared-return fast exits

FastReturnBench makes recursive calls whose fast and slow results join at one return phi.
It runs three cold processes per VM, rotating VM order between processes. Each process does
three warm-up rounds and three measured rounds. The reported value is the median of the
three per-process best rounds. These runs are pinned to available CPU 8.

| Nanoseconds per call | Baseline | Candidate | HotSpot |
|---|---:|---:|---:|
| Arithmetic result through shared return | 1.83 | 1.39 | 1.06 |
| Constant result through shared return | 1.75 | 1.33 | 1.02 |

All nine processes return checksum `141557760`. In both cases the candidate takes about 24%
less time than baseline and is still about 31% and 30% slower than HotSpot, respectively.
The theoretical recursive-call count includes calls that inlining removes.

The generated code shows the mechanism. The arithmetic fast path returns at offset `0x1e`,
before the first prologue push at `0x1f`. The constant path returns at `0x1a`, before its
push at `0x1b`. Baseline starts the prologue at `0x10`. Slow branches still enter the
prologue. `/tmp` was unavailable, so the dumps came from throwaway binaries whose only change
was an equal-length replacement of the dump path. Timing used the unmodified release binaries
listed above. FastExitSem matches HotSpot in default, optimizing-all and few-register modes;
the last two ran with graph verification enabled.

## ABI hints and rejected collector experiment

An isolated comparison of generated code shows that eligible virtual/interface argument hints
remove two register moves (six bytes) from a generic call site that passes a field as an
argument. Profiled and cold sites, and values live across calls, are handled as before.
Dispatch timings vary widely, and tiering and inlining decisions differ between processes.
Neither a stable runtime gain nor a HotSpot dispatch advantage has been shown.

The proposed task-wide dirty-card drain was rejected. It made a focused scanner comparison
over hot metadata slower, and whole-VM allocation results stayed near noise. The existing
scanner stays. DirtyCardsSem and YoungCardsBench are kept for later work.

## Verified CI Paper startup

[Paper gate run 38509430246](https://github.com/redstone-md/rava/actions/runs/38509430246)
completed successfully. The downloaded [artifact 11869516741](https://github.com/redstone-md/rava/actions/runs/38509430246/artifacts/11869516741)
was checked against its source manifest and binary hashes. It applies patch SHA-256
`5f8f21d022298fc8bdde112601e12a7e94140862f088311ba48c8b15c15fef25` to `bed0e219`.
These x86-64-v2 CI executables are not the local microbenchmark binaries:

| CI executable | SHA-256 |
|---|---|
| Baseline | `59ed7c3c5ede5029d5d3822a82c11a63b5770d886565b99d3db9e63543b273c0` |
| Candidate | `21ae929e940efffda4d5d15c1221079721d13b1b13afb0a184095ade963e2e28` |

Each VM ran three fresh-world starts, with the order reversed in the second round. All nine
compressed logs contain a real `Done` line. The host was a four-vCPU AMD EPYC 7763 CI runner,
running Paper 1.21.1 build 133 with seed `20260930`, view/simulation distance 10 and port 25599.

| Median | Baseline | Candidate | HotSpot |
|---|---:|---:|---:|
| Harness elapsed to readiness, seconds | 23.30947 | 23.02268 | 28.28892 |
| Paper logged Done duration, seconds | 22.817 | 22.573 | 27.774 |
| Peak resident memory, MB | 1,167 | 1,170 | 1,096 |

The candidate's median elapsed time is 1.23% below baseline. Three samples are not enough to
call that an end-to-end gain. It is 18.62% below HotSpot, but most of that gap is already
there in baseline RAVA and does not come from this patch. This verified artifact comparison
replaces the earlier local `03aa1a57` Paper figures. Startup time says nothing about tick
performance. Client validation still needs a real modpack and a graphics-capable host. No client
FPS or server MSPT improvement is claimed.

## Reproducing the generic checks

Set RAVA_BASE and RAVA_CANDIDATE to separately built release executables, and JAVA_HOME to
JDK 21. Check the executable hashes before running. Use the same host and rotate process
order. Skip CPU pinning where it is unavailable, but record that choice instead of changing it
silently.

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
```

For FastReturnBench, run three cold processes per VM in rotated order. For JNI, run five
processes per case and VM. Kinds 0/1/2/3 select length/UTF length/chars/UTF chars, and coder
0/1 selects ASCII/UTF-16. Substitute RAVA_BASE and the JDK `java` executable for each
comparison. Keep the raw samples, checksums, host details and build manifests. A local
microbenchmark ratio is not an application-wide result.
