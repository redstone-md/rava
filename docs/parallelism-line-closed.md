# The parallelism line is closed, and the boot is a serial program

*2026-08-26*

## What was believed

The standing conclusion (rows 440, 450-452) was that the largest untaken lever in the engine
is parallel bytecode execution: rava boots at ~1.0 cores against HotSpot's ~3.25, so even a
perfect single-threaded engine could not reach "seconds", and only the GIL stands between us
and a 3x multiplier.

## What was measured

**1. rava has no parallel window at all.** Sampling CPU-time delta per wall second across a
whole product boot (`/tmp/cores.ps1`) gives a flat line: 41 consecutive samples between 0.94
and 1.36 cores, 6 OS threads throughout. Not an average diluted by serial stretches — there is
no burst anywhere.

**2. Neither offload mechanism runs.** `RAVA_TASK_TIME` (new) counts guest executor tasks:
**one task, 0.01 s, in the entire boot**. `RAVA_LANE_DRAIN_TRACE` + `RAVA_LANE_PROBE` print
nothing at all — the gil-free lane is never touched. `[PC-OFFLOAD] supplyAsync seen=0`. The
boot does not offload because it does not ask to.

**3. And it does not ask because MC's boot is serial.** `java -Xint` on the same world:
**45.6 s wall, 55.8 CPU, 1.22 cores**, and its per-second trace is `1.0` eighty-five times in a
row. With HotSpot's JIT out of the picture the guest program uses one core.

**4. HotSpot's extra cores are HotSpot's compiler.**

| java config | wall | CPU | cores | `Done` |
|---|---|---|---|---|
| product | 6.3 | 22.1 | 3.53 | 0.202 |
| `-XX:CICompilerCount=2` | 6.1 | 17.9 | **2.91** | 0.185 |
| `-Xint` | 45.6 | 55.8 | **1.22** | 1.739 |

Throttling compiler threads removed **4.2 CPU-seconds and changed the wall by nothing.** Those
cores are not doing the guest's work; they are compiling.

## What follows

Making guest bytecode run in parallel cannot buy a 3x multiplier on a program that HotSpot
itself runs on one core. **The line is closed by measurement, not by opinion.**

And the comparison that remains is much sharper than the one it replaces:

| | wall | CPU | cores | `Done` |
|---|---|---|---|---|
| rava (product) | 39 | 41 | 1.08 | **0.405** |
| `java -Xint` | 45.6 | 55.8 | 1.22 | 1.739 |
| `java` (JIT) | 6.3 | 22.1 | 3.53 | 0.202 |

**rava already beats the JVM interpreter on the product boot — in wall, in CPU, and in `Done`
by 4.3x.** The entire remaining gap is one thing: HotSpot's compiled code is ~6x better than
ours on this workload, on a serial program.

And we already know why, from the other end: only **9.4% of RIP samples land inside compiled
code**; 90.6% are in the Rust host. HotSpot runs the guest as compiled guest code; we run it
as Rust natives with a seam in front. That is the same conclusion the seam census reaches from
its own side — 13,534,209 callbacks, 377 distinct callees, largest 10.37% — a seam that flat
cannot be optimised callee by callee, and a boot that spends 90% of its samples in the host is
not a boot the code generator can fix either.

## Method notes

- `bench/cores.log`, per-second `TotalProcessorTime` delta; affinity pinned to 12 cores in both
  engines so the comparison is like-for-like.
- The java arms run their own server on **port 25599** and kill only the PID they started — the
  user's own MC on 25565 is never touched.
