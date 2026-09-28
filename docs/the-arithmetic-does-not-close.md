# The per-call numbers are all small, and they do not add up to forty seconds

*2026-08-26*

`bench/CrossCost.java` finally prices a boundary crossing directly, against a guest call in the
same loop shape. Every seam estimate in this ledger had been inferred from profile shares; this
measures it.

| | rava | java | ratio |
|---|---|---|---|
| empty loop | 0.6 ns | 0.5 | 1.2x |
| guest static call | **1.8 ns** | 0.3 | 6x |
| guest virtual call | **9.0 ns** | 0.5 | 18x |
| native `List.size` | **73.6 ns** | 0.0 | — |
| native `List.get` | **109.4 ns** | 2.2 | 50x |
| native `Map.get` | **142.4 ns** | 3.5 | 41x |

⚠The first version of this bench routed each case through a fresh `IntUnaryOperator` lambda, so
every timed loop ran at a COLD call site and the empty loop measured **1053 ns/op** — slower than
the native calls it was the control for. Plain static methods, one loop each, warmed first.

**A crossing is 40-80x a guest call.** That is the seam, measured rather than inferred.

## And then the arithmetic

Every population counted this session is in the tens of millions:

```
compiled -> host crossings   32,049,770     x ~100 ns  =  3.2 s
compiled method entries       5,675,530
interpreted method entries    3,464,578
seam callbacks               13,534,209
native invocations            6,353,898     x ~100 ns  =  0.6 s
handle births                 5,343,863
static field writes               9,944     x  226 ns  =  0.002 s
```

The boot is **forty seconds** — forty thousand million nanoseconds. Multiply any of these
populations by its measured per-event cost and the total is a few seconds, not forty.

So the dominant cost is **not at call granularity at all**. It sits below it, in the operations
that run per object and per field: allocation, interning, hashing, string comparison, GC, the
`Value` encode/decode on every load and store. That is exactly the shape the host profile has
been showing all along — heap/intern 8.3%, allocator 5.9%, `Value` 5.5%, mem/str 4.5%, GC 4.5% —
and it is why every call-level lever priced at the floor.

⛔This is the one thing this session did NOT close. `try_invoke_chain` counts 6.35 M native
invocations, but it is the chain WALKER — most natives are reached through faster paths
(`fu_fastpath`, the JIT intrinsics, the map and stream dispatchers) that never touch it, so that
number is a floor and not a census. The interpreter's `[HB]` heartbeat counts only interpreted
opcodes, and interpretation is not where this boot lives.

**The open question, stated precisely:** what is the population of the operations that actually
fill the forty seconds, and what does each cost? Every lever priced this session was priced
against a call-level population. If the cost is one level down, the levers were aimed one level
too high — which would explain, in a single sentence, why six independent mechanisms all landed
at the instrument floor.

## Two more populations, same order of magnitude

```
arena object allocations   4,164,212   (RAVA_JIT_STATS arms the existing counter)
native invocations         4,560,137   (try_invoke_chain — a FLOOR, see below)
```

Everything is between four and thirty-two million. Summing every population counted this session
gives roughly **55 M events against 40,000 M nanoseconds — 727 ns per event**, when the most
expensive event actually measured (`native Map.get`, boundary included) is **142 ns**.

## Which of the two possibilities holds — settled

**Branch (2) is refuted by data already in hand.** The profiler reports **9.1% of samples inside
compiled code**, i.e. 3.6 s of a 40 s boot. The instruction stream inside compiled code is not the
missing thirty-five seconds.

**And the interpreted-opcode figure on file was wrong for this configuration.** Counted on today's
binary: **197,100,000 interpreted opcodes**, not the 10 M recorded earlier — twenty times more.
At rava's ~5.5 ns/op that is still only **1.1 s**, so the correction does not change the
conclusion, but the old number should not be quoted again.

## So the arithmetic closes, and not where I was looking

The host profile's categories already sum to the whole boot by construction — JIT+seam 11.4%,
heap/intern 8.3%, interpreter 8.1%, allocator 5.9%, `Value` 5.5%, mem/str 4.5%, GC 4.5%, natives
3.4%, hash/maps 2.3%, class loading 2.3%, kernel 2.0%, frames 1.2%, unclassified 15.3%, compiled
9.1%. There is no missing time. What is missing is a *consistent per-event price*:

```
55 M counted events  /  40 s  =  ~727 ns per event
warm microbenchmark, the most expensive event measured  =  142 ns
```

**The same operations cost roughly five times more in the boot than in a warm microbenchmark.**
That is the signature of a workload whose cost is memory, not compute — cold caches, polymorphic
sites, a working set that does not fit. And it explains the flatness better than any structural
theory: in a memory-bound workload the stalls are everywhere, so every profile slicing comes back
flat by nature.

The supporting datum is on file: **RSS 774 MB against HotSpot's 351 MB** for the same work.

⚠The counter-datum is also on file, and it is why this is a thesis and not a finding: nursery
size was already A/B'd — 4 MiB was *worse* than 32 (166 s vs 154), and 128 MiB scored the same as
32. If locality alone were the lever, a smaller nursery should have helped. It did not.

**The one test available without hardware counters, and it does not confirm the thesis.**
`bench/CrossCost.java` grew a ballast of 2 M live objects, so the same measured loops run against
a working set that cannot sit in cache. Alternating empty / large blocks four times — because
order and live set are confounded otherwise, and reading the first block as a live-set effect is
exactly the mistake this bench exists to avoid:

```
live set   empty      2M        empty      2M
List.get   109.0 ns   97.8 ns   124.2 ns   443.4 ns
Map.get    198.8 ns  147.4 ns   355.6 ns   338.4 ns
```

No pattern follows the live set. What the run does show is a **four-fold swing on the same
operation inside one process at identical warm state** — which is itself a datum: our per-operation
cost is not stable to within a factor of four, and the "727 ns in the boot against 142 ns warm"
gap may be that same instability rather than a property of cache behaviour.

**So the diagnosis stands unresolved, and honestly so.** Three candidate explanations for the
flat profile remain, and the instruments available here cannot separate them:

1. the work is genuinely spread across the engine, and the 6x is the sum of ~50 items;
2. the workload is memory-bound, so stalls are everywhere and removing events cannot help;
3. per-operation cost is unstable (GC and allocation interference), and averages over a boot are
   not comparable to a warm microbenchmark at all.

**What would settle it:** cache-miss/IPC counters or per-operation timing histograms. Those counters were unavailable in the original Windows runs; a Debian follow-up with process-scoped PMU counters is recorded below.

## Linux PMU follow-up (2026-09-29)

The current Debian host does expose `cycles`, `instructions`, `cache-references`, and
`cache-misses` through `perf stat`. Two fresh-world launches of the exact current-source
profile-use product (`a35dfa1a2634a0a2a3445c22439f23118841fb61703a4b8e47b8f79c6cd7e0da`,
runtime source `b5d7b83`) used only the canonical `RAVA_JIT=1`,
`RAVA_JIT_TIER0=1`, `RAVA_HARD_MB=3584` environment. Both reached ordinary `Done`.
`perf stat -p <pid>` was attached immediately after process launch and counted through
termination just after `Done`:

| Run | Launch → Done | Cycles | Instructions | IPC | Cache refs | Cache misses | Miss rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Rava 1 | 22.806 s | 165.91 B | 259.87 B | 1.57 | 17.90 B | 3.78 B | 21.10% |
| Rava 2 | 22.307 s | 163.89 B | 259.59 B | 1.58 | 17.73 B | 3.70 B | 20.87% |

One same-classpath fresh-world OpenJDK 25.0.4.1 launch reached `Done` in 5.629 s. This adaptive JVM is not an acceptance configuration. Its PMU totals are not a reliable cross-runtime comparator and varied materially with the stopping protocol: a SIGTERM after `Done` produced a 7.007 s interval and 81.32 B instructions, while a second fresh launch terminated by SIGKILL as soon as `Done` was observed produced a 6.006 s interval and 30.88 B instructions. Neither Java counter total is used to claim an instruction or cache-miss ratio.

The two Rava PMU runs are internally stable (instructions differ by 0.11%, cache misses by 2.0%), but they do not identify which operations generated that traffic. These whole-process counters therefore neither confirm nor refute the 727 ns vs 142 ns per-event explanation. They expose no bounded, safe work-removal candidate; the per-operation discrepancy remains open, as do the <5 s and strict-no-adapt gates.

### Process-count versus sample-count disagreement

A fresh exact-current-product rerun tested whether the old `perf record` event totals could be
treated as whole-process instruction totals. In one snapshot-free boot, `perf stat -p <pid>`
counted **259.63 B** `instructions:u`. Concurrently, `perf record -c 100000000 -e instructions:u
-p <pid>` captured 1,397 fixed-period samples with **zero lost samples**, which `perf report`
estimated at **139.70 B**. An explicit `--inherit` run reproduced the same ~0.54 ratio. This is
not frequency-mode extrapolation and the count difference is much larger than the attach/stop
window.

The cause of this same-PID disagreement is not established. Treat `perf stat` as the direct
counter result and `perf record`/`perf report` as a sampling profile only; do not compare the
profile's approximate event total with direct counters, nor use it to compute an instruction
ratio. The older 136–181 B sampled totals in the roadmap therefore do not contradict or calibrate
the ~260 B direct count. This still provides no operation-level ownership or bounded optimization
ceiling.

### Current-product hot-symbol check: no-go for this objective

A symbolized report from the exact current PGO product found
`Reference2ObjectArrayMap.<init>(Reference2ObjectMap)` at 4.82% and
`StateHolder.lambda$populateNeighbours$0` at 4.39% of sampled instructions. The existing route
census records about 460k copy-constructor arrivals; the guest path is block-state neighbour-map
construction (`Stream.map`/`Map.get`/`put`). These are product-reachable hot methods, not a newly
measured removable CPU ceiling. A class/descriptor-specific native copy shortcut would add
another adaptation route, while the strict no-adapt surface is still uncertified. No operation
counter or direct per-method timing demonstrates the ≥7.6 CPU-s reduction required by the current
wall target. Do not build from sample share alone.

### Symbolized exact-source diagnostic profile

The matching debug-enabled diagnostic artifact (`50f1899e1020feace65cee2da1c4903f35795362ebca58c8d2db93cbf6e6bcab`,
runtime source `b5d7b83`) reached `Done` in 32.005 s on a disposable world copy. It is not a
product timing. A low-overhead `perf record -F 49 -e instructions:u` capture through `Done`
retained 1,455 samples with zero loss. The largest named Rava-host symbols were `Heap::get_field_by_id`
(2.68%), `Vm::jit_call_body` (2.67%), `Vm::fu_fastpath` (2.49%), `Vm::jit_direct_probe` (2.10%),
`run_hot` (1.98%), and `invokevirtual` (1.82%); allocation, key-fold, and native dispatch followed
at 1.78%, 1.62%, and 1.59%. No single host method dominates this source-matched diagnostic
profile. These shares cannot be converted into process CPU savings, and they do not identify a
multi-second no-adapt-safe cut.

## Calibration: the instrument floor is ~8% today, not ±2.5%

Ran `boot_ab_flag.ps1` against a flag that does not exist, so both arms are the same binary in
the same environment:

```
round 1: UNSET wall 51.3  cpu 53.1     SET wall 48.5
round 2: UNSET wall 52.0  cpu 54.1     SET wall 46.9  cpu 49.2
round 3: UNSET wall 52.4  cpu 54.5     SET wall 47.9  cpu 50.2
CPU-s: SET min 49.2  UNSET min 53.1  delta -7.34%  DISJOINT=True
```

**7.34%, reported disjoint, between identical arms** — and the arm that runs second in each round
is faster every time. That is warm-up and thermal drift, not a configuration difference.

⚠So the floor is **~8% CPU right now**, not the ±2.5% the ledger assumes. Every result inside
that band measured today is noise rather than a magnitude: the `java.util` override's +8.46%, the
`RAVA_JIT_CAP` lift's +5.49%, `RAVA_DISPATCH_TIER_N=256`'s +2.69%. None of their VERDICTS change
— all were "no win", and a no-win inside the noise band is still a no-win — but they must not be
quoted as numbers.

Results outside the band are unaffected: the served-bit regression (374.8 s against 54.2), the
tiering cliffs (336 s and 400 s against 40), the JIT off/on gap (121 s against 40), and
everything established by COUNTING rather than timing.

Baseline on this tree, same session: **wall 46.9–52.4 s, cpu 49.2–54.5** — slower than this
morning's 38–45 s, which is the same drift.
