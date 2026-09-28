# 113 million allocations, and the atomic on every one of them

## The population

A product boot, counted exactly:

```
allocs = 113,497,262      frees = 110,267,249
```

Object births explain almost none of it. There are **5,867,627 handle births** and 4,050,849
arena allocations in the same boot; at two mallocs per handle object (the `Box<Object>` and the
spilled field blob — 98% of objects have more than four fields) that is ~12M. **The other ~100
million have never been attributed to anything.**

This matters because `mi_malloc` was 15% of the samples in the heaviest boot phase — the largest
single mechanism any slicing found this session, against a profile whose top *function* is 4.3%.

## The accounting was costing more than it measured

Every allocation and every free in the process passes through `mem_meter`, which existed to give
the GC scheduler real process bytes instead of a 5x-undercounting estimate. Its implementation
was one `LIVE.fetch_add` per event, plus a load of `HARD` — and `LIVE`, `HARD` and `ABORTING`
are adjacent statics, so each RMW invalidated the line the next load reads, from every thread.

**223.8 million atomic read-modify-writes on one global cache line per boot.** Applying this
project's reckoning rule before writing any code: at 6 ns each that is 1.34 s of a ~47 s boot =
**2.9%**; at 10 ns, 4.8%. Above the +/-2.5% floor either way, which is more than can be said for
most of what has been tried.

Nothing reads `live()` more than a few thousand times a boot. Per-event precision was never
needed, so each thread now accumulates its own delta and pushes it to the shared counter only
when the delta leaves +/-128 KB. Allocation and freeing interleave, so the local value hovers
near zero and a flush is rare.

The safety net survives intact: an allocation larger than the threshold crosses it by itself and
flushes immediately, which is exactly the runaway-native-recursion case (`toString` over a cyclic
graph, once 31 GB) the hard ceiling exists to catch. `live()` becomes stale by at most 128 KB per
live thread, against readers that compare to multi-MB thresholds.

`RAVA_NO_ALLOC_BATCH=1` restores the per-event atomic, so the change can be A/B'd in ONE binary.

## Measured

Interleaved same-binary A/B, product configuration, warm-up discarded, 4 rounds:

```
CPU-s   SET (per-event atomic) min=51.2  median=52.65
        UNSET (batched)        min=47.0  median=50.45
        delta                  min +8.94%   median +4.36%   (SET = the old code = slower)
```

Both statistics agree in sign and both clear the +/-2.7% null band this box measures. What makes
it convincing rather than merely favourable is that **the median landed inside the range
predicted before the code was written** (2.9-4.8% from 223.8M RMWs at 6-10 ns): the arithmetic
and the stopwatch agree, which is not something a drift artefact does.

⚠The first version of the kill switch HUNG EVERY BOOT and is worth remembering: it read the flag
through `OnceLock::get_or_init(|| env::var_os(..))`, `var_os` allocates, the allocation re-enters
the global allocator, and `get_or_init` blocks the thread waiting for an initialisation only that
same thread could finish. Both A/B arms timed out at 400 s with cpu=0. The 4 gates and 21 probes
had passed — on the build one commit earlier. **Nothing reachable from the global allocator may
allocate, and `OnceLock` converts that mistake into a silent deadlock rather than a stack
overflow.** The flags are now plain atomics, filled once from `main`.

## Next: which sites

A percentage without a caller is not actionable, so `RAVA_ALLOC_SITES=1` samples one allocation
in 4096 and records every return address on its stack into a fixed table of atomics —
no allocation, no lock, no symbolisation on the hot path (a re-entrancy guard is mandatory:
capturing a backtrace inside the allocator otherwise recurses to death). Addresses are resolved
once at dump time through the existing `pc_profile::symbolize`.

## The allocator's own page policy

With the accounting tax gone, the next per-event question is what mimalloc itself pays on 113.5M
allocations and 110.3M frees. `main` sets `MIMALLOC_PURGE_DELAY=0` — *more* aggressive than
mimalloc's own default of 10 ms — so a freed page is returned to the OS immediately instead of
being reused. The comment explains why: under worldgen the default retained ~7 GB of already-freed
pages as RSS.

That trade was never priced on the boot. Interleaved same-binary A/B, `MIMALLOC_PURGE_DELAY=500`:

```
CPU-s   SET (500 ms) min=47.8  median=50.90
        UNSET (0)    min=50.6  median=53.20
        delta        min -5.53%   median -4.32%
```

Peak working set, one boot each:

```
delay=0    ~1071-1165 MB
delay=10    1189 MB      (mimalloc's own default)
delay=100   1208 MB
delay=500   1347 MB
```

⛔**Both of those boot readings are noise, and the -4-5% is withdrawn.** `MIMALLOC_PURGE_DELAY=10`
then read *+10.58%* on the same harness — opposite sign on the same axis — so a null test was run
against a flag that does not exist:

```
RAVA_NULL_TEST_5   min +5.06%   median +10.30%
```

**The boot harness cannot currently resolve anything smaller than ~10%** (the box has
`QuantumEngine`, `warp` and `Discord` on it; processor queue 3). Every boot A/B taken in that
window is worthless, including both purge readings.
## An instrument that works: `bench/flag_ab_chunk.ps1`

A 50-second boot is the wrong unit when the box drifts 10%. ChunkFill runs in ~36 CPU-seconds, so
eight rounds fit in the time one boot A/B takes, it is the **worldgen** workload the purge-delay
comment was written about, and it prints the bit-exact gate hash for every run. It reports peak
RSS alongside CPU, so a memory/speed trade is settled in one pass.

Its own null test — a flag that does not exist, 8 rounds:

```
RAVA_NULL_TEST_6   median -1.18%
```

**A floor of ~1.2% instead of ~10%.** On that instrument:

```
MIMALLOC_PURGE_DELAY=500     median  -0.01%   RSS 1015 vs 1144 MB   ⇒ REFUTED, and no RSS cost
RAVA_NO_ALLOC_BATCH=1 (old)  median  +3.06%   (min +3.7%)           ⇒ CONFIRMED, 2.5x the floor
```

All 48 runs across the three passes hashed `8790f04ace726ecf`.

So the allocator's page policy does nothing in either direction — the 7 GB retention the comment
warns about does not reproduce here even on worldgen — and the accounting batching is real,
reproduced on a second workload with an independent instrument at a tenth the noise.
