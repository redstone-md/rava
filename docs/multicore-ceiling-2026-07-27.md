# The multicore ceiling, localized — 2026-07-27

Measured on current main with `RAVA_GILFREE_RINGS=1`, `RAVA_LANE_PROBE=4000`, `RAVA_LANE_OUTER=1`.

## 1. The lane crashed before it could be judged — fixed

A lane boot panicked at 74.2s, before `Done`:

    internal error: entered unreachable code:
    recursive invoke: new_object_value hard-exits on nursery full, never signals

`RunExit::NurseryFull` had acquired a SECOND meaning — the op checkpoint reuses it as the gil-free
worker's armored abort exit — while `Vm::apply`'s recursive-invoke arm still asserted the first.
A recursive invoke reaches that checkpoint like any other code, so the arm was reachable and fatal.
Fixed: a pending region abort now unwinds as `VmError::RegionAbort`, which is deliberately not a
`Throw` (only `Throw` reaches `find_handler`, so no guest `catch` can swallow an abort).

After the fix the lane **completes**: `Done (95.050s)`, wall 155.5s, no panic.

## 1a. ⚠ CORRECTION — section 2 below was measured with the freeze machinery OFF

I ran the first lane boots with `RAVA_GILFREE_RINGS` alone. The lane's lock-free arms live behind
`RAVA_M3_FREEZE_RESOLVE` (+ `RAVA_GILFREE_ARRAY_OWNED`, `RAVA_M3_WORKER_LOCKED_EXEC`), and the hit
COUNTERS live behind `RAVA_FORK_STATS`. So `table_hit=0 direct_hit=0` meant "the counters are off",
not "there are no hits" — two configuration errors in one investigation, and the second one made
the first look confirmed.

With the full reference flag set:

| | |
|---|---|
| freeze-table hits | 84,753,868 |
| frozen direct-cache hits | 198,620,219 |
| WS-native hits | 6,300,453 |
| **lock-free total** | **289,674,540 — 99.21%** |
| fell through to the lock | 2,292,292 (0.79%) |
| `Done` | **61.977s** (was 95.050s at `RAVA_GILFREE_RINGS` alone) |

**Both statements are true at once, and that is the real finding.** 99.2% of CALLS are lock-free,
and yet `hold_ms=57582` over a 70s window means the lock is held ~82% of the TIME — reconciling
this file's first measurement with the "~82% of a ring region" already on record. The 0.79% that
miss are precisely the calls that span whole subtrees.

So the ceiling is NOT cache coldness (the cache is 99.2% warm) and NOT the number of locking calls.
**It is the guard's LIFETIME**: `lib.rs:13730` holds the lock across the callee's execution, so
0.79% of calls serialize ~82% of the region. The fix is to release before executing, not to
eliminate the acquisition.

Lane vs serial on `Done`: **61.977s vs 43-48s** — still ~35% slower, but a third of the gap this
file originally reported.

## 2. The first (mis-configured) measurement, kept for the record

| | Done | wall |
|---|---|---|
| serial (default) | 43-48s | ~100s |
| gil-free lane | **95.0s** | 155.5s |

The probe at t=24s: `locks=122 wait_ms=48262 hold_ms=10600`, `dmiss_locks=37,663,820`. Workers wait
**four times longer than anyone holds** — eight threads queueing on one mutex.

The outer attribution names the anchor, and it is not distributed at all:

| anchor | n | hold_ms | max_ms |
|---|---|---|---|
| **`BiomeSource.findBiomeHorizontal`** | 25 | **10555** | 588 |
| `SimpleJsonResourceReloadListener.scanDirectory` | 2 | 41 | 40 |
| everything else combined | ~100 | <2 | — |

**10555 of 10600 ms of lock hold sits under one callee.**

### The mechanism

`lib.rs:13730` takes `resolve_lock_acquire()` and the guard lives to the end of the function — past
`invokestatic`/`invokespecial`/`invokevirtual`. So the lock is **not protecting a resolve; it spans
the callee's entire execution**. That is `RAVA_M3_WORKER_LOCKED_EXEC` working as designed: rather
than abort the region when a worker reaches a callee the lock-free fast paths cannot serve, run it
under the lock.

`findBiomeHorizontal` IS the ring task's top-level guest method. So every worker that starts a task
takes the global lock and holds it for the whole task, and the eight workers execute one at a time —
with parallel-lane overhead on top. That is the entire +106%.

The lock-free set today is exactly two entries (`lib.rs:13694-13727`): `ArrayList.get` via
`ws_arraylist_get`, and `CubicSpline.findIntervalStart`. Everything else serializes its subtree.

## 3. What this means for the arc

The refutation on record (+37%, now +106%) is **not a refutation of parallelism**. It is a
measurement of a fallback that serializes by construction. The question the arc actually turns on
is narrower and now precisely stated:

> Can the general dispatch path run on a worker WITHOUT the global lock — or equivalently, can the
> lock-free fast-path set be grown to cover the ring's hot callees?

The probe names those callees: `Set.contains`, `Iterator.hasNext`, `Map.forEach`,
`ImprovedNoise.sampleAndLerp`, and the anchor `findBiomeHorizontal` itself. The first four are
library shapes of the kind already served lock-free. The anchor is not — it is a large guest body,
so it cannot be a "pure-read native" and needs the general path to be thread-safe.

**The prize, stated honestly.** Hold is 10.6s of 24s of wall, i.e. the lock is held ~44% of the
time and the rest of each worker's life is spent waiting for it. If the general path could run
lock-free, the ring's ~44s at eight workers approaches the Amdahl figure already recorded:
`2.2 + 44/8 ≈ 7.7s` on the Done window. That is the only lever on the map with that shape.

**The cost, stated equally honestly.** Making the general dispatch path thread-safe is the
`gil-free Vm` arc (`docs/gil-free-vm-layout.md`, Vm fields split into four contracts). Prior
attempts at pieces of it are recorded as refuted for concrete reasons — per-thread inline caches
were 4.6× slower from cold TLS plus locking, and the freeze-resolve table is worker-only. This is
not a fix; it is a project.

## 4. Recommended next step, before any of that

One cheap experiment settles whether the anchor even needs the general path: find out **why**
`findBiomeHorizontal` reaches `lib.rs:13730` instead of a direct compiled call. If it is a
megamorphic or interface dispatch that could be made direct on the worker, the anchor collapses and
the remaining hold is the ~45 ms tail — which would change the arc from "make the VM thread-safe"
to "admit one more shape to the direct path". `RAVA_FORK_TRACE` already prints
`[FORK-GUARD-CALLEE]` with the receiver's concrete class and whether the target is compiled.


# 5. The mutation surface is EMPTY — and why that still is not a green light

`RAVA_FREEZE_TALLY` (new, `crates/rava-interp/src/freeze_tally.rs`) turns the existing GAP-S09
freeze guard into a release-mode counter, so the question the arc turns on could finally be asked:
**which frozen shared-VM sites does a worker actually reach?**

Whole ring region, full lane flag set:

    [FREEZE-TALLY] 2 frozen-site touches by gil-free workers:
    [FREEZE-TALLY]              2  Heap::write_static_cell  - REAL-STATE (cannot simply skip)

**Two.** Against 292 MILLION calls. And **zero** cache-fill touches, so "execute frozen" would not
even lose a memo. The twenty enumerated frozen sites are, in practice, one site touched twice.

## But the lock is not only a race preventer

It is what gives the worker an exclusive `&mut Vm`, which Rust requires: `opcodes::run` is
`fn run(vm: &mut Vm, ...)` and the tree carries **374 `&mut Vm`** sites and **143 `&mut self`**
methods on `Vm`. Removing the lock does not merely risk a race — the general path cannot be
*called* without that exclusivity. So "drop the lock now that we know nothing mutates" is not
available; converting the execution path to `&Vm` plus interior mutability for the two real
mutations is the gil-free Vm arc, and its size is those 374 sites.

## The cheaper path the same measurement opens

The worker does not need the general path AT ALL if the direct path covers everything. It already
covers 99.21%. The 0.79% that falls through is **concentrated in four library call sites**, from
the per-site freeze-coverage census (per ring task):

| site | per task | served |
|---|---|---|
| `java/lang/ThreadLocal.get()` | 3249 | unknown → LOCK |
| `java/lang/ThreadLocal.set()` | 3249 | unknown → LOCK |
| `java/util/Set.contains()` | 3249 | unknown → LOCK |
| `java/util/function/Predicate.test()` | 3249 | unknown → LOCK |
| `BiomeSource.findBiomeHorizontal` | 1 | unknown → LOCK (the anchor) |
| `PerlinNoise.getValue(DDDDDZ)D` | — | **direct = 116,964** |

4 × 3249 × 128 tasks ≈ **1.66M of the 2.29M fall-throughs**. `unknown` means "nothing known for
(site, class)", not `neg` — none is refused as uncacheable.

So the next build is not a refactor: **add lock-free worker arms for those sites, exactly as
`ws_arraylist_get` already does for `ArrayList.get`.** `Set.contains` is the same read-only shape;
`ThreadLocal.get`/`set` are per-thread by definition, which is the strongest possible case for
serving a worker without a lock. If the fall-throughs collapse, the lock is never taken, the
general path never runs on a worker, and the 374-site question never has to be answered.

`Predicate.test` is the one that is not a native — it is a guest lambda dispatch, so it belongs to
the direct-cache warming question rather than to a native arm.

## Method note, recorded because it cost real time here

Two configuration errors in one investigation, and the second made the first look confirmed: the
lane's lock-free arms live behind `RAVA_M3_FREEZE_RESOLVE` and the hit counters behind
`RAVA_FORK_STATS`. Running with neither produced `table_hit=0`, which reads as "no hits" and is
actually "no counter". A published diagnosis rested on it for an hour. **Before drawing a
conclusion from a zero, check that the counter was armed.**
