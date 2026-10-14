# Performance validation: cycle 2 (2026-10-14)

This cycle is ongoing. RAVA has not demonstrated superiority over HotSpot across workloads.
The previous zero-player Paper sprint comparison remained slower than HotSpot; this cycle
adds narrower compiler checks and raw per-tick measurement rather than extrapolating startup.

## Frozen source and binaries

The source base is `bdc079212c8d6fddf4b94e2515d11b2e4aa4aa96`, including the merged first-cycle
changes, plus documentation-only head `68d8657b4573a4e270fbf7295ff249c674ad49a3`.
The second frozen candidate patch is Git blob `6262a88aac0f0d0283d35405ce58bfed56447100`,
SHA256 `3081b88cee0dc3db683164713789dccca8d3475f532ff407bd8926adf7df8a2d`.

| Local executable | SHA256 |
| --- | --- |
| Unchanged upstream | `7ede277b17a89e8a05e1a64f029ea7c9165fc795bf9afc462f04b251d2b9952b` |
| Candidate 1 and rebuilt candidate 2 | `245eee2b1fda430b055774395d8338731ad7e50887701f5e3b10097d8f8a91cf` |
| Temurin 21 launcher | `2a207f5e7d075afa01d97f8048389a64432a44c4a5af0f5e77d6e286ec5f401d` |

Candidate 1 used patch SHA256
`f5306efe327cd223f117af2a324daf0bde4d51b9ba228810297f144caa297951`.
Candidate 2 adds the macOS test-executable linker instruction and plan; its rebuilt Linux
executable is byte-for-byte identical. Local release builds use Rust 1.99.0 and Temurin
21.0.12.1+1. Diagnostics and raw benchmark outputs remain outside Git.

## Numeric intrinsic commoning

The explicit stateless GVN allowlist retains intrinsic template parameters, result types,
ordered operands and arities. Memory-reading operations and helpers remain excluded.
The existing NoiseBench kernel has six duplicate Round(1) operations before this change
and three afterward; the three emitted rounding instructions were checked in machine code.
NumericGvnSem agrees with HotSpot in default, opt-all and few-register modes, including
signed zero, NaNs, raw payload conversion, integer extremes and intervening volatile stores.

The throughput check uses six balanced fresh processes per VM, each with 20 warmup and
10 measured sweeps of the unchanged NoiseBench.sweep. An external wrapper preserves every
nanosecond duration and complete double-bit checksum. The original benchmark was also run
once per VM. All 540 sweep checksums and complete accumulated checksums agree.

Each whole child was pinned to CPU 2. This also restricts compiler and GC threads; these
numbers are a single-core kernel diagnostic, not a full-machine or Paper result. Host CPU
topology and /proc performance counters were unavailable. The aggregation first takes each
process's median measured sweep, then the median of those six process medians.

| VM | Sweep time (ms, lower is better) |
| --- | ---: |
| Unchanged upstream | 16.421 |
| Candidate | 16.723 |
| HotSpot | 14.157 |

The candidate median is 1.84% longer than upstream and 18.13% longer than HotSpot.
It wins three of six paired blocks. The paired geometric elapsed ratio is 1.0043, with an
exploratory block-bootstrap 95% interval of 0.9585 to 1.0525. The instruction reduction is
established; a throughput improvement or small regression is unresolved at this noise level.
No wall-time gain is claimed for numeric commoning.


## Broader JDK kernels

All 72 fresh processes exit zero and match their benchmark checksums. Six balanced orders
per workload use normal VM flags and whole-process CPU 2 affinity. Aggregation uses medians
of process medians. The declared warmup policy excludes the first five printed SystemTick
rounds and first four Mixin rounds; RecordHash and Dispatch already suppress their warm
round timings. Every measured round remains in the raw data.

| Kernel / unit | Upstream | Candidate | HotSpot | Candidate / HotSpot |
| --- | ---: | ---: | ---: | ---: |
| SystemTick / ns per tick | 1887.025 | 1888.550 | 2182.100 | 0.865 |
| RecordHash / ns per lookup | 17.050 | 16.350 | 6.700 | 2.440 |
| Mixin / ns per call | 7.000 | 6.857 | 5.410 | 1.268 |
| Dispatch virtual / ns per call | 13.685 | 13.350 | 16.280 | 0.820 |
| Dispatch interface / ns per call | 15.060 | 14.325 | 18.960 | 0.756 |
| Dispatch virtual fields / ns per call | 13.925 | 13.585 | 15.980 | 0.850 |
| Dispatch interface fields / ns per call | 15.125 | 14.900 | 19.130 | 0.779 |

One candidate Dispatch process runs around 2.5–2.8 ns for the virtual cases, while the
others are around 13 ns. One upstream Mixin process changes from about 11 to 7 ns late.
Both regimes remain in the calculations. Tier/inline state is not attested in timed runs,
so these differences are not attributed to numeric GVN. No broad throughput improvement
is established. RecordHash is the largest measured deficit in this small kernel set.

## Collector bookkeeping and platform validation

The promotion fix drains a completed run before requesting its replacement, including the
allocator-exhaustion path. A fixture executes production forwarding into eight destination
slots with twelve live sources and four real allocation failures. It checks copied payloads,
headers, retained marks, references and exactly one completed promotion range.
The unchanged fixture fails after reverting only the fix: five duplicate ranges instead of
one. This confirms the regression test detects the original bookkeeping error; it does not
establish a Paper throughput gain or prove a use-after-free.

Local release unit tests pass (25/25). The first remote run passed both Linux hosts and
Windows but the macOS unit executable failed to reserve low-address class metadata.
The production launcher already uses a small PAGEZERO region; the macOS build instruction
now applies that existing setting to the unit executable too. A separate Cargo forwarding
probe verifies target selection; the actual repeated macOS unit step passes.
The complete semantic gate passes on Ubuntu 22.04, Ubuntu 24.04, macOS Intel and Windows
2022, followed by the required full Paper start check:
https://github.com/redstone-md/rava/actions/runs/38524694036.
The artifact ZIP digest is
`de7b59199915091154298667b09185d83a04444cbca2b5abba3e3b2870fc6662`;
its pinned patch provenance and all nine fresh-start logs were independently checked.
Every log reaches Done and includes normal server shutdown.

| CI fresh-start wall time / HWM (median of 3 processes) | Wall (s) | HWM (MiB) |
| --- | ---: | ---: |
| Unchanged upstream | 24.768 | 1127 |
| Candidate | 24.513 | 1176 |
| HotSpot | 28.406 | 1127 |

Candidate startup is 1.03% shorter than upstream and 13.70% shorter than HotSpot in this
run. Three repetitions do not establish a small stable improvement; most of the startup
advantage over HotSpot already exists in upstream. This gate does not measure tick speed.

The local full semantic run has two additional failures already present in unchanged
upstream: NetworkInterfaceSem and MulticastSem. Direct OS queries confirm getifaddrs and
NETLINK_ROUTE fail with EPERM while SIOCGIFCONF and metadata ioctls work. HotSpot enumerates
the real loopback interface; both RAVA binaries currently return no interfaces. Sysfs MTU
and hardware-address files are also absent. This is tracked as a separate generic Linux
compatibility fix, not a candidate regression or a reason to suppress probes.

## Tick observer and profiling limits

The external Paper 1.21.1 observer writes raw durations, tick numbers, sprint attestations
and per-phase p50/p95/p99. A 100-warmup/300-measured smoke succeeded on upstream and HotSpot:
300 consecutive measured ticks each, all sprint attestations true, CSV quantiles matching
JSON, zero players, 399 entities afterward, Done and graceful exit 0. The earlier failed
smoke incorrectly interpreted the sprint API's return value; its log is preserved.
These short windows validate capture, not stable tail latency or full mob AI.

Paper's end-tick API measures duration before the listener. Observer work and later
end-of-tick work are outside that reported duration. Quantiles from this API are therefore
not complete tick-loop wall-time measurements. Every comparative process uses the same
observer and configuration, and all three dimensions are regenerated. Zero-player sprint
and future player-present paced runs must be reported separately.

A separate instrumented upstream diagnostic found 23 young collections during three sprint
windows: 343.6 ms pause, including 324.6 ms marking, around 2.83% of the instrumented tick
window. No full collection occurred within those windows. MEM_LOG perturbed startup by
around 30%; these samples are not performance comparisons and cannot explain the previous
HotSpot gap by themselves. Zero RSS/committed readings are unavailable counters, not zero
memory use. Summed compiler elapsed time across threads is not CPU time.

## Remaining work

Measure repeated dispatch/record/system kernels and long tick windows; use the measured
remaining gaps to select bounded, semantics-preserving changes. A separate compiler tracer
will prove masked-array indices safe only under a dominating positive-length fact for the
same array. A genuine protocol client is being prepared for a player-present server scenario.
Client-present server MSPT does not measure rendered client FPS.
