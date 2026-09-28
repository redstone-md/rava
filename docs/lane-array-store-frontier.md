# The ring lane — investigation record, and the current state

> ## ⚠ EVERYTHING BELOW THE NEXT SECTION IS A HISTORICAL RECORD, AND PARTS OF IT ARE WRONG.
>
> It is kept because the wrong turns are the useful part — each one is a measurement that had to be
> taken to get to the next. But it asserts, among other things, "**One core, for the entire
> batch**", "there is no parallelism at all", "the lock is still the wall", and that the
> array-store bypass "stays default-OFF only because on its own it is a loss". **None of those is
> true of the shipped engine.** Read this header for the current state; read the rest for how it
> was arrived at.
>
> An audit also caught something worse than staleness: four commits titled `docs(lane): …`
> (`01f0413`, `8fc275f`, `79d1392`, `72157ed`) were **EMPTY COMMITS**. Their findings existed only
> in commit messages and never reached this file, so the file went on contradicting the code for
> four commits while looking maintained. The content is folded in below. *A commit message is not
> documentation.*

## CURRENT STATE (2026-07-28)

The ring lane is **ON BY DEFAULT** in the `--features next-gen-gc` build, correct, and a win.

```
                       wall                          Done
parallel (default)   min=78.4  [78.4/80.3/80.2]    min=21.93  [21.93/23.348/23.015]
serial (kill switch) min=86.4  [86.4/86.5/87.0]    min=29.462 [29.571/29.462/29.509]
                        -9.3%                          -25.6%
```

Interleaved, min-of-3, every parallel run beating every serial run on both metrics with no
overlap. Correctness is gated by `bench/RingLane.java`, which dispatches the ring computation
through `CompletableFuture.supplyAsync` — the shape the lane admits — and folds its hash in INDEX
order, so only a wrong VALUE can fail it, never a different interleaving. It must produce
`RingScan`'s hash both armed and disarmed.

### What actually stood in the way, in the order it was found

1. **The array-store barrier** — the region aborted at its first off-GIL array write. Bypassing it
   soundly (`RAVA_GILFREE_ARRAY_OWNED`) produced the first completing region: `127/127 keep=true`.
2. **A silent wrong-answer bug**, pre-existing and invisible: `freeze_resolve` published sites
   whose callee takes a `long`/`double`. Such a parameter arrives as ONE operand cell but occupies
   TWO callee local slots, and the lock-free serve arm passes the buffer through un-marshalled —
   so the ring's noise callee `ImprovedNoise.sampleAndLerp(IIIDDDD)D` read its four doubles from
   the wrong slots and returned plausible, wrong numbers. Fixed by refusing at PUBLICATION.
   It survived indefinitely because **neither existing gate armed the lane**: `ChunkFill` and
   `RingScan` replicate their loops inline and serially. That is why `RingLane` now exists.
3. **The worker count.** `worker_cap()` was a hardcoded 6, well past the optimum. One boot per
   point, on a 16-logical-core box pinned to 12:

   | workers | serial | 1 | 2 | 3 | 4 | 5 | 6 |
   |---|---:|---:|---:|---:|---:|---:|---:|
   | Done | 31.258 | 49.393 | 29.954 | 23.451 | **23.414** | 27.872 | 31.127 |
   | CPU  | 91.4 | 105.4 | 113.6 | 120.5 | 138.2 | 170.5 | 202.8 |

   Six workers burn **2.2× the CPU of one for identical work** (`cbs_sum=384M` in both) and hand
   it all back in contention. The default is now `available_parallelism() / 4`, clamped 2..8.

### Things this file previously got wrong, and the correction

- *"One core, for the entire batch" / "no parallelism at all"* — measured with the worker count at
  6, which is past the point where contention eats the gain. At 3–4 workers the region runs
  genuinely parallel; the lane's own timestamps show four workers with overlapping spans.
- *"The lock is still the wall"* — it was, until the wide-param fix; afterwards `hold_ms` is ~400 ms
  across the region and the wall is elsewhere.
- *"batch 57 s → 128 s after the fix"* — measured with `RAVA_FORK_STATS` and `RAVA_LANE_OUTER`
  armed, which are contended per-dispatch atomics. Clean: 41.8 s. **Disarm the counters before
  timing anything** — this confound was hit three separate times.
- *"−20.97% on Done"* (recorded before the fix) — measured on the divergent build. Skipping
  argument marshalling is fast. Retracted; the honest figure is the −25.6% above, on a bit-exact
  build.
- *"worker_direct is the cost"* — the opposite: 9.8× win on the ring workload.
- A 2048-entry direct-mapped L1 in front of `worker_direct`'s map was **refuted** (neutral,
  8557/8838/8855 vs 8724) and reverted rather than shipped.

### Known limits of the current evidence

- `RAVA_NO_GILFREE_RINGS` is a **bundle** switch, not one mechanism: it also flips the main nursery
  reservation (32 MiB → 256 MiB), `worker_locked_exec`, and the `*astore` COW-armed codegen arm.
  The A+B and C phases are identical to the second across all six runs, so the confound looks
  confined to the Done window — but "the lane as the only variable" overstates it, and the extra
  224 MiB of reserved young space is a real cost of the default that the headline does not price.
- `RingLane`'s lambda is a static method reference (MethodHandle kind 6). The boot's ring admits
  `ChunkGeneratorStructureState::lambda$generateRingPositions$5`, an INSTANCE lambda (kind 5/7,
  receiver in slot 0). `RingLaneInstance.java` covers that shape; the boot's own 127 ring positions
  are still not compared between arms.
- `RingLane`'s `LANE_MS` is **not** a parallel-speedup yardstick: serial 403857 ms vs armed 2855 ms
  for the same 8 positions with comparable callback counts. The gap is `warm_compile` compiling
  bodies a single execution never makes hot. It is a correctness gate only.

---

# HISTORICAL RECORD: the ring lane's frontier, as it was understood at each step

## What was believed

`docs/vm-shared-exec-plan.md` sized a seventeen-leaf `&Vm` refactor on the reading that the lane
is capped by one global lock held across a callee's whole execution, and that dropping it makes
the region abort. The no-lock experiment was run, the region did abort (`completed=0/127`), and
the arc was written up as "the lock is what supplies the exclusive `&mut`".

That reading was built on an abort attribution that could not have been right.

## Why the attribution was wrong, and the fix to the instrument

`request_abort` names its raiser through `#[track_caller]`, but the print is BUDGETED —
`n < 6 || n % 64 == 0`. A boot runs several distinct lane phases; the first six raises all landed
in the datapack phase, and the 128-task ring batch — the one under investigation — printed
nothing at all. The four `jit.rs:3121` lines that were read as "the ring's blocker" belong to a
batch that ran ten seconds earlier.

`gilfree_cf::note_abort_site` now tallies every raise by file:line with no budget, and the tally
is DRAINED PER BATCH at the batch ledger. An abort attributed to the ring batch is now an abort
that happened in the ring batch.

## What the instrument says

Ring batch (n=128, no lock), by raiser:

| raiser | what it is | count |
|---|---|---:|
| `jit.rs:3995` | `rava_ring_store_guard` — the off-GIL array-store barrier | **7** |
| `execution_state.rs:112` | a guest exception got set on a worker (downstream of the above) | 2 |

Not one `vm_ref` denial. The lane dies at its first array write, exactly as
`gilfree_cf::array_owned_stores`'s own doc predicted, and the `NullPointerException: getfield` in
the task ledger is the body continuing against an array it believed it had filled.

## The bypass exists, and its recorded objection is stale

`RAVA_GILFREE_ARRAY_OWNED` admits the store when the array was allocated inside the region's own
pre-reserved slots — sound, because those slots are unreachable from the shared graph. It is
default-OFF on a measurement that says the lane then "converts a fast, safe abort into an
UNBOUNDED GRIND", root-caused to the compiled inline-alloc retry loop spinning on
`rava_arena_safepoint` because **"the rings lane never opens a spill pool"**.

The lane opens one today — `gilfree_cf` line 1243, `parallel::spill_setup(pool_base, pool_len,
nursery_slice())` — and `rava_arena_safepoint` carries the bounded exit (`1` = "no progress, take
your exit") that the grind needed. Both landed after that verdict was recorded. This is the
recorded rule firing again: **a lever's value is not a constant of the lever.**

Re-measured with the bypass on (no lock, one boot):

```
batch n=128  completed=0/127  cbs_sum=8062     ← barrier ON  (before)
batch n=128  completed=0/127  cbs_sum=175917   ← barrier OFF (after)
```

**22× more compiled work per batch** before the region dies, and the array-store raiser is gone
from the tally entirely. What is left is small and named:

| raiser | what it is | count |
|---|---|---:|
| `execution_state.rs:112` | guest exception set on a worker | 4 |
| `jit.rs:3121` | plain `vm_ref()` on a worker with no locked envelope | 1 |
| `lib.rs:18969` | the CLINIT BARRIER — a worker reached an uninitialised class | 1 |

The single `vm_ref` denial is an artefact of running WITHOUT the lock and disappears in the sound
configuration. The clinit barrier is a WARM-UP question (one class the pilot did not touch), not a
refactor question.

## The sound configuration: the region COMPLETES

Lock on, barrier bypassed, one boot:

```
batch n=2   claimed=1/1     completed=1/1     keep=true
batch n=128 claimed=127/127 completed=127/127 keep=true
            cbs_min=2027436 cbs_sum=410719346
            pool=133026KB pool_used=48768KB pool_claims=254  nursery_full=0
```

**127 of 127 ring tasks ran off-GIL and their results were kept** — the first time this lane has
ever produced a kept batch. 410 million compiled callbacks of real computation, and the spill pool
was genuinely exercised (254 claims, 48 MB), which is what the stale grind verdict was waiting on.

## And it prices the lock exactly

Six workers, equal work (~3.9 M callbacks each), all started within 46 ms of one another:

```
run#3  t0=14391  t1=15036     run#8   t0=16045  t1=17887
run#4  t0=14407  t1=15711     run#9   t0=15714  t1=18498
run#5  t0=14437  t1=16042     run#10  t0=14427  t1=19073
run#6  t0=15039  t1=16671     run#11  t0=16674  t1=19642
run#7  t0=14416  t1=17284     run#12  t0=14399  t1=20217
```

Six threads that genuinely ran in parallel would all finish near `t0 + 600 ms`. Instead the
completions come out in a **~600 ms staircase** and each task's elapsed time grows by one step —
`run#3` takes 645 ms, `run#12` takes 5818 ms for the same 3.9 M callbacks. That is not contention;
that is strict serialisation. 127 tasks × ~600 ms of exclusive execution = the 62 s the batch
actually took (t=14.3 s → 76.5 s).

So the earlier "1 ms of contention across 15 outer acquisitions" was measured on a region that
died at its first array write, and it does not survive a region that runs. With the barrier out of
the way the dispatch lock is the wall, and now it has a number.

| configuration | ring batch | wall | Done |
|---|---|---:|---:|
| barrier ON (region aborts → serial recompute) | `completed=0/127` | 113.1 s | 49.3 s |
| barrier bypassed, lock ON | `completed=127/127` | 127.4 s | 64.5 s |

Parallel-but-serialised is 15 s WORSE than serial recompute, which is the 35-45% loss already on
record — now explained rather than merely observed.

## The prize, measured rather than argued

`RAVA_LANE_NOLOCK` on its own could never answer this: every downstream admission gate is phrased
as "do I own the resolve lock", so dropping the guard at the dispatch site made the first `vm_ref`
deny and the run measured an abort. With the flag also claiming that ownership (documented at
`resolve_lock_held_by_me` — unsound, measurement only), the same six workers behave completely
differently:

```
                t0       t1     callbacks
lock ON    run#3  14391   15036   3917749      <- a ~600 ms staircase, one at a time
           run#4  14407   15711   3913261
           run#5  14437   16042   2033680

lock OFF   run#3  13930   13943     39563      <- all six inside a 15 ms window
           run#4  13930   13943     46343
           run#5  13930   13943     46481
           run#6  13929   13943     50598
           run#7  13929   13944     46756
           run#8  13930   13945     40276
```

| | per-thread rate | threads running | aggregate |
|---|---:|---:|---:|
| lock ON | 6.07 M cb/s | 1 | 6.07 M cb/s |
| lock OFF | ~3.5 M cb/s | 6 | **~21 M cb/s** |

Per-thread throughput DROPS (shared cache lines and the freshly-locked shared maps are now
genuinely contended), and the aggregate still comes out **~3.5×**. On a 62 s serialised batch that
is 62 s → ~18 s.

The unlocked run does not survive: it aborts 5× at `class_loader.rs:245` and 5× at the clinit
barrier, then the region dies ~14 ms in.

Warming is not the explanation. Raising the serial pilot from 1 to **8** — 8 full ring tasks run
under the GIL before the region opens — changes those counts not at all (5 and 5 again, same two
raisers). A class that eight serial executions of the same workload never needed is not cold; the
loader is being asked for a name that only exists because six aliased `&mut Vm`s tore a shared
structure. The aborts are corruption, which is precisely what the flag's own header promises.

Naming what the loader is asked for settles it. The two classes are `oshi/util/ParseUtil` and
`net/minecraft/CrashReportCategory$Entry` — **the worker is building a crash report**. The loader
miss is downstream of a guest exception that already happened, not the cause of anything. And the
failure point moves run to run: three no-lock boots died at `class_loader` + clinit + pending-set,
then at clinit + loader, then at pending-set alone, with `cbs_sum` wandering 223 k → 621 k. A
deterministic missing prerequisite does not move; corruption does.

So the ~3.5× is a ceiling read off the milliseconds before the corruption bit, and it is the only
kind of number an unsound run can produce. It does not shorten the road: **the shared state the
dispatch path touches has to be concurrency-safe before the lock can go**, and "just drop the
lock" is not a shortcut past the refactor.

## The lock, measured on the region that runs

`RAVA_LANE_PROBE` + `RAVA_FREEZE_TALLY` over the full 80 s completing batch:

```
locks=522  wait_ms=318057  hold_ms=65187
freeze: table_hit=87.9M direct_hit=206M wsnative_hit=6.5M miss=2.37M
        locked_calls=3855164  dmiss_locks=2370268  stored=6352
aborts: shared_field=0 array_store=0 array_alloc=0 array_region_full=0  requested=false
FREEZE-TALLY: 2 frozen-site touches by gil-free workers — both Heap::write_static_cell
```

- **`hold_ms=65187` against an 80 s batch**: the lock is held for ~81% of the region's wall.
- **`wait_ms=318057` across six workers**: ~53 s of blocked time each, out of ~80 s alive.
- 3.86 M locked calls out of ~302 M dispatches — 0.78%, as the census said — and they still own
  four fifths of the region.
- **Across 430 million callbacks the workers wrote shared VM state exactly TWICE.** Every one of
  the ~20 instrumented cache-fill sites — `jit_cache`, `jit_direct_cache`, `iface_closure_cache`,
  `dispatch_closure_cache`, `offload_cache`, the layout and site caches — reports zero.

That last line is the whole argument for the refactor in one number. The lock is not protecting
work; it is paying for a signature. `opcodes::run` wants `&mut Vm`, and acquiring exclusivity is
the only way to produce one.

The locked callees are unchanged from the earlier census, and now measured on a region that
finished rather than one that died:

```
Predicate#test 412624   SinglePointContext#<init> 412623   Set#contains 412623
Spline$Point#<init> 412623   Climate#target 412623   Function#apply 412623
ThreadLocal#get 412623   ThreadLocal#set 412623
AtomicLong#compareAndSet 245226   AtomicLong#get 245226
Climate$RTree$Node#search 61764   BlockPos#<init> 824
```

## The wall is ONE symbol, and it is a frame-size cap

`RAVA_LANE_OUTER` ranks outer acquisitions by total held time. On the completing region the
answer is not a distribution across the twelve locked callees — it is one line:

```
BiomeSource.findBiomeHorizontal(IIIILjava/util/function/Predicate;L…RandomSource;L…Climate$Sampler;)L…Pair;
    n=112  hold_ms=55028  max_ms=798
```

112 acquisitions holding **55 s of a 77 s batch**, ~491 ms each. Every other anchor in the dump is
0–1 ms. A ring task essentially IS one call of this method, so the whole task body runs inside one
locked span — which is exactly the ~600 ms staircase, seen from the other side.

Two hypotheses, both testable, and the first one is wrong:

**Not compilation.** A method called about once per task can never reach a call-count compile
threshold, so `RAVA_LANE_WARM_COMPILE` was built to compile, on main and before the region opens,
precisely the callees the serial pilot reaches on the general path (35 of 47 for the ring batch,
in 1 ms). The anchor did not move: `n=119 hold_ms=64121`. It was already compiled.

**It is the call-buffer cap.** `dispatch_resolve_cacheable_cls` will only serve a NON-LEAF callee
through the machine-code DEEP arm when `max_locals + max_stack <= dispatch_callbuf_slots()`, which
is 32. A method that does not fit is `NeverCacheable` — permanently, for every site and every
receiver — so every call falls to `jit_call`, and off-GIL that means taking the lock and running
the subtree under it.

Widening it (`RAVA_CALLBUF=64`) on the same completing configuration:

| | ring batch | callbacks | `findBiomeHorizontal` max_ms |
|---|---:|---:|---:|
| `RAVA_CALLBUF=32` | 76.5 s | 410 M | 798–1203 ms |
| `RAVA_CALLBUF=64` | **57.9 s** | 288 M | **428 ms** |

Batch −25%, callback traffic −30% (more callees served directly), and the anchor's worst span cut
by more than half. Note what did NOT change: `findBiomeHorizontal` still anchors. `callbuf_slots`
is clamped to 64, so that method does not fit even at the maximum — the gain came entirely from
OTHER callees crossing the threshold.

### The lane is still a loss, and one run said otherwise

That `RAVA_CALLBUF=64` boot read `Done 43.99 s`, against a 49.3 s serial figure from a `CALLBUF=32`
run, and it was written up here as the lane finally beating serial. **It was not.** Interleaved,
same flags in both arms, lane the only variable:

```
done  off min=40.965 [40.965/54.447/44.657]   on min=45.269 [52.343/64.162/45.269]   +10.51%  overlap
wall  off min=116.9  [116.9/127.3/119.5]      on min=113.8  [122.9/142.2/113.8]       -2.65%  overlap
```

The lane costs **+10.5% on Done**. The 43.99 s was a good draw on a bimodal box compared against a
baseline measured under a different flag set — precisely the two-run comparison this project's
min-of-N interleaved discipline exists to forbid, and I made it anyway. The single-run number is
retracted; the A/B stands.

What the A/B does show is worth its own measurement: the serial arm's `Done` min is **40.97 s** at
`CALLBUF=64`, against 49.3 s seen earlier at 32. That is not a controlled comparison either, so it
is a lead and nothing more — but it is a lead on the DEFAULT path, which matters more than the
lane does.

The cap is not a bug: the buffer is the CALLER's spill area, emitted into every PIC site, so
widening it costs stack everywhere. Serving over-sized frames would mean a scratch buffer the deep
arm reaches through instead of the caller's stack — a codegen change, not a constant.

## The anchor CAN be removed, and removing it exposes the next shape

The warm-compile refutation above was wrong, and for an instructive reason: `force_compile` looked
the method up on the CONCRETE receiver class only. `findBiomeHorizontal` is declared on the
abstract `BiomeSource` and invoked on a concrete subclass, so the lookup returned nothing and the
lever was recorded as refuted having never run on its one real target. (`javap` also settles the
frame-size theory: the 7-arg overload is a three-instruction forwarder. It fits any buffer. It was
simply never compiled, because it is called about once per task.)

Resolving through the hierarchy removes the anchor completely:

```
before:  findBiomeHorizontal  n=112      hold_ms=55028  max_ms=798
after:   findBiomeHorizontal  absent
         top anchor  jit.rs:1775  n=1662942  hold_ms=255  max_ms=0
```

Total held time across the region: **~55 s → ~264 ms**. The dispatch lock is no longer held across
subtrees at all — the thing this whole arc set out to establish.

And it is still not a win: the batch goes **57.9 s → 134.5 s**. One 491 ms locked span became
**1.66 million short ones**. Compiling the method moved its body out of a single interpreted span
into compiled code that calls back per operation, and every callback whose (site, class) is
`NeverCacheable` takes the global lock to get through `rava_dispatch_miss`. At this ratio, trading
span length for acquisition count loses.

That is a genuinely different problem from the one this document opened with, and it is the real
remaining one: the residue is 1.66 M `NeverCacheable` misses, and the census says what they are —
`$Lambda` receivers (`Predicate#test`, `Function#apply`) and `java/` receivers (`AtomicLong`).
`dispatch_resolve_cacheable_cls` rejects both categories by an unconditional prefix test. They need
lock-free SERVICE — a λ-PIC for the first, native arms for the second, the same shape as the
`ThreadLocal` fix — not a narrower envelope.

## Why the residue cannot be direct-called, in its own words

With `picprobe::dump()` wired into the periodic lane probe (its own dump fires from the CLI's clean
exit, which a boot measured to `Done` and then killed never reaches), the engine answers directly:

```
fell to Rust host                     250408100 (58.2%)
  PIC miss -> rava_dispatch_miss      246054522 (57.2%)
    lambda-receiver misses     15401690 (3.5%)  directable 7030321
    λ-PIC fills: deep=0 leaf=0  refused: shape=0 rooting=0 frame=0
  plain invoke -> rava_invoke           4353578 (1.0%)
    static 2195771  virtual 311005  special 1844015  iface 2787
compiled call SITES = 9520: PIC-emitted=6257 (65%), invoke-emitted=3263 (34%)
  invoke sites by kind: static=2080 special=1174  (static+special = structurally non-PIC-able)
synthetic-frame fall-through reasons = 392140:
  callee not compiled yet (tier lag)    14132 (3%)
  native/java-served (no fptr ever)    378008 (96%)
```

Three separate populations, three separate reasons — none of them "not warm enough":

1. **static + invokespecial: 4.04 M plain invokes.** `Climate#target`, `SinglePointContext#<init>`,
   `Spline$Point#<init>`. These sites are *structurally* non-PIC-able (no receiver to guard on), so
   they can only be served by the per-site DIRECT cache — and `jit_direct_call`'s miss arm opens
   with `if gilfree() { return None }`. **A worker can never build a direct entry**, so the entry
   has to have been made by main, for that exact site, before the region opened.
2. **capturing lambdas: 7.03 M directable misses.** `Predicate#test` / `Function#apply`. The λ-PIC
   mechanism exists (`RAVA_LAMBDA_PIC`) and is inert by default. Switched on it fills **19 entries
   and refuses 170,550 on ENTRY SHAPE** — it serves NON-CAPTURING singletons, and the ring's
   predicates capture. Batch 57.9 s → 60.4 s, locked callees unchanged. Refuted for this phase.
3. **`java/`-served: 378,008 (96%) of direct-resolve refusals.** `AtomicLong#get/compareAndSet`.
   No compiled fptr will ever exist; these need a lock-free native arm, the `ThreadLocal` shape.

Two things were ruled out along the way, both by measurement rather than argument:

- **The thread-local call cache.** `jit_direct_resolve` takes a static/special site's resolution
  from `tlcache::call_get`, which is per-thread and empty on every worker — a real gap, closed by
  `crate::shared_call_cache` (publish once, read anywhere; sound because the loader's class cache
  is insert-only and a static/special resolution is a function of the symbolic ref). It is
  **neutral on the lane** (batch 57.9 s, unchanged), because supplying the resolution does not help
  a worker that is refused the direct entry outright by reason 1.
- **More serial warm-up.** `RAVA_PILOT_N=4` — four whole ring tasks executed on main before the
  region opens — leaves the locked-callee census unchanged and the batch slightly worse (60.9 s).
  The third independent refutation of warming this phase.

## A worker can now build direct entries, and a whole population leaves the lock

Population 1's reason — `if gilfree() { return None }` — was a consequence of the FILL mutating the
shared `jit_direct_cache`, not of the RESOLVE needing anything. Separating the two removes it. The
worker resolves and caches in its OWN thread-local table (`crate::worker_direct`), which is
invisible to every other thread, so there is nothing to publish, invalidate or race.

Everything the fill needed turned out to be available or one signature away:

| | was | now | cost of the flip |
|---|---|---|---|
| `resolve_method` | `&mut self` | `&self` | **zero call-site changes** — it only ever touched the shared-readable loader and a thread-local cache |
| `jit_direct_resolve` | `&mut self` | `&self` | two sites |
| `Vm::jit_lambda_ptr` | field on `Vm` | `jit_lambda_scratch` (thread-local) | — |

That last one is not a borrow obstacle, it is a **live data race**: it is per-call scratch handed
from `jit_direct_resolve` down to `jit_direct_call`'s guard comparison, so a sibling writing
between the two silently redirects a lambda guard. It could not have been correct under real
threading, and nothing was measuring it.

Measured on the completing ring region:

```
before:  SinglePointContext#<init>=408736   Spline$Point#<init>=408736   Climate#target=408736
after:   absent
locked_calls  2480730 -> 1372621   (-45%)
```

**The entire static/invokespecial population is gone from the lock.** The batch is not faster
(63.1 s against 57.9 s, inside this box's 57.8–63.1 spread) — the third time today that removing
lock acquisitions has failed to remove time.

Tree-over-tree on the DEFAULT boot, baseline binary vs current, three runs each: `Done −1.13%`,
`wall +1.88%`, both overlapping — neutral, no regression.

What remains is a genuinely different shape from where this document started:

```
Predicate#test 383179   Function#apply 383178      <- capturing lambdas
AtomicLong#get 242404   AtomicLong#compareAndSet 242404
Climate$RTree$Node#search 120299
```

`AtomicLong#compareAndSet` is worth naming for a second reason: in Rava it is a plain
read-compare-write. Under the dispatch lock that serialises into correctness by accident; it is not
atomic, and cannot be, until it is a real CAS on the field cell — the same class of latent bug as
the `ThreadLocal` shared cell and the lambda scratch field, both found by this same hunt.

## The lane runs on ONE core — and the earlier "the lock is no longer the wall" was wrong

Everything above optimises lock ACQUISITIONS. Three separate changes removed 28%, then 45%, of
them, and none moved the batch. The obvious next question — does the region use more than one core
at all — had never been asked. Sampling the process's CPU-time delta per wall second, with six
workers and a region that completes 127/127:

```
 11s cores=0.96   25s cores=1.01   40s cores=1.00   55s cores=0.96   71s cores=1.00
```

**One core, for the entire batch.** And the scaling test says the same thing from the other side —
identical work, `RAVA_FORK_STATS` off so the shared counters cannot confound it:

```
threads=1  t=57927ms  cbs_sum=295482892
threads=6  t=56861ms  cbs_sum=299218173
```

Six workers are **1.9% faster than one**. There is no parallelism at all.

### The correction

An earlier section of this file read `hold_ms=6353` off the probe and concluded the lock had
stopped being the wall. That number is **per worker**. The lane's own per-worker line, six live
workers on the current tree:

```
w3 wait_ms=33862 hold_ms=7825    w6 wait_ms=34785 hold_ms=6353
w4 wait_ms=35110 hold_ms=6325    w7 wait_ms=35044 hold_ms=6632
w5 wait_ms=34054 hold_ms=7378    w8 wait_ms=34223 hold_ms=7192
```

Summed, **41.7 s of held lock in a 57 s batch** — and only one thread can hold it — so ~73% of the
region is single-threaded by construction. Each worker spends ~34 s of its ~57 s life *waiting*.
The lock is still the wall; the previous paragraph was an arithmetic error on a per-thread counter
and is retracted.

That also explains why removing acquisitions kept buying nothing: it is ~80 acquisitions per worker
holding ~80 ms each, so what matters is the LENGTH of a span, not how many there are.

### Where the long span now lives

`rava_dispatch_miss` already narrows correctly — it holds across resolve+publish and drops the
guard before descending. The remaining subtree-wide span is `jit_call`'s own `_worker_exec_guard`,
which is taken for the general path and lives until `jit_call` returns, i.e. across
`opcodes::invoke*` — resolve AND the whole callee execution. Its own comment records it as "1 ms
across 127", which was measured while the dispatch-miss envelope was still wide and is no longer
true now that the other span is gone.

And that is exactly, and only, because `opcodes::invoke*` takes `&mut Vm`. Which puts
`docs/vm-shared-exec-plan.md` back at the top of the queue with a measured prize for the first
time: **41.7 s of a 57 s batch**, and the surface narrowed from seventeen leaves to the invoke
family.

## The no-lock aborts are not corruption — they are Minecraft's own threading detector

Earlier in this file the unlocked region's aborts were written up as corruption from aliased
`&mut Vm`, on the evidence that the failure point moved run to run and eight serial pilots changed
nothing. Naming the classes settles it differently. With the clinit barrier printing its class:

```
[CLINIT-BARRIER] worker hit UNINIT class net/minecraft/util/ThreadingDetector (has_clinit=true)
[LANE-LOADER-MISS] worker asked for "oshi/util/ParseUtil"
[CLINIT-BARRIER] worker hit UNINIT class net/minecraft/CrashReportCategory (has_clinit=false)
[LANE-LOADER-MISS] worker asked for "net/minecraft/CrashReportCategory$Entry"
```

`ThreadingDetector` is the class **Minecraft uses to detect concurrent access to a structure it
requires to be touched by one thread at a time**. The crash-report classes that follow are it
reporting. And the class is UNINITIALISED at that moment, which is the load-bearing detail: main
had never touched it in this boot, so nothing on the serial path reaches that code. It is reached
only when the workers actually run at the same time.

### …and that reading was wrong too. It is a COLD CLASS.

Printing the guest frames at the barrier — the diagnostic that should have existed before any of
this was written up — gives:

```
[CLINIT-BARRIER] worker hit UNINIT class net/minecraft/util/ThreadingDetector (has_clinit=true)
[CLINIT-BARRIER]   guest frames: BiomeSource.findBiomeHorizontal(IIIII…RandomSource;Z…Sampler;)…
```

**`ThreadingDetector` is UNINITIALISED at the moment of the abort.** You cannot call `checkAndLock`
on an instance that was never constructed, and constructing one initialises the class — so the very
first touch here is a `new ThreadingDetector(...)`, not a detection. MC is not reporting anything;
this is the clinit barrier doing exactly its job on a class a worker happened to reach first.

The two supporting details agree. Only `PalettedContainer` and `LegacyRandomSource` reference the
class in the whole jar, and `LegacyRandomSource` constructs one per instance — so the touch comes
from an RNG being created inside `findBiomeHorizontal`. And MC's own bytecode shows the tasks were
DESIGNED to be independent:

```
171: invokeinterface RandomSource.fork()            <- each task gets its OWN RNG
189: invokedynamic   get:(…,RandomSource)           <- the fork is captured
202: invokestatic    CompletableFuture.supplyAsync  <- then dispatched
```

`fork()` hands every task a fresh `LegacyRandomSource`. There is no shared structure and no
guest-level sharing conflict. (It also identifies the census's `AtomicLong#get/compareAndSet`:
that is `LegacyRandomSource`'s seed, one per forked RNG.)

So the "MC's own threading detector is firing" paragraph above is **retracted**. Twice now a class
NAME has been over-read in this file; the frames are what settle it, and they were one small
diagnostic away the whole time.

### What the abort actually is, and the generic fix

A worker reached a class main had never initialised. That is not specific to this class or this
jar — it is structural: main warms the classes MAIN touches, and 127 parallel tasks touch a
slightly different set. Warming harder does not close it (`RAVA_PILOT_N` at 4 and at 8, both
measured, both unchanged), because the pilot is one task and the cold class belongs to a path a
later task takes.

The fix that fits the shape: **the barrier already knows which class it refused.** Record it, let
the coordinator `ensure_init` it on MAIN after the abort, and re-open the region. An aborted
attempt costs ~15 ms against a ~57 s region, so a couple of warm-and-retry rounds are free, and
nothing about it is jar-specific.

## What this changes

Two things, in this order.

1. **The array-store barrier is no longer the frontier.** It is bypassed soundly and the bypass is
   what makes a completing region possible at all. It stays default-OFF only because on its own it
   is a loss — it converts an abort into a serialised run.
2. **The `&Vm` refactor is re-validated, and priced.** It was sized against a misattributed
   frontier, and it turns out to be aimed at the right one anyway: 62 s of strictly serialised ring
   execution is what the lock costs, against ~10 s if the same work spread over six P-cores. That
   is the prize, and it is the first time the arc has had one.

The two remaining named raisers (`vm_ref` without a locked envelope in `rava_new`, and the clinit
barrier — one class the pilot never touched) are small and separate from the lock.
