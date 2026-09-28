# The permanent verdict that was re-derived nine million times

2026-08-28.

## What the counter said, and what it did not mean

`RAVA_CALLBACK_STATS` on a product boot prints the reasons a call site can never be
inline-cached:

```text
[ICMISS] total=9057565  is_cache_class=254953(3%)  is_int_set=34(0%)
         force_native=45446(1%)  native_served_deep=8757132(97%)
[ICMISS] classes: LongOpenHashSet#isEmpty=3420033   LongArrayFIFOQueue#isEmpty=3107170
                  ReferenceOpenHashSet#isEmpty=933364  ObjectArrayList#isEmpty=931380
                  GuavaLoadingCache#getUnchecked=254953  LinkedTreeMap#get=155504
```

**8.39 M of the 9.06 M are `isEmpty` on four fastutil collections.** The obvious reading —
and the one I acted on first — is that eight million cheap collection calls cross the seam
into a native, so the cure is to answer them earlier. That reading is wrong twice over.

## The first wrong move, and what it proved instead

An early cascade arm for `("isEmpty","()Z")` on an `$elems`-backed receiver, byte-identical to
the `coll_op` block it shortcuts. It is correct — 4 gates and 21 probes green — and it fired
**9,115 times**, not 8.4 M.

The reason is a trap this tree has recorded twice before under other names: `try_last_arm`
runs FIRST, and in a loop that asks the same question of the same class it hits every time.
**A shortcut placed after the shortcut to the last arm is not a shortcut.** The 8.4 M calls
were already being answered cheaply.

⇒ So the number is not a count of expensive native calls. It is a count of **resolves**.

## What the number actually was

`dispatch_resolve_cacheable_cls` returns `NeverCacheable`, and its callers write `-1` into the
site's inline cache. Every comment in the area says the same thing:

> *"the negative entry this returns is permanent, so the proof never re-runs on the hot path"*

That is true of the STORE and false of the WORK. Machine code treats only `fp > 0` as a hit, so
a `-1` slot sends the next call straight back to the fill routine — which re-allocated `name`,
`desc` and `cls`, re-read the class word, and re-walked `resolve_method` and
`jit_native_served_deep`, only to write the same `-1` again.

The arena path has probed for this all along (`rava_dispatch_miss`, `vm/dispatch.rs`), and its
own comment names the stake: *"skip the per-call resolve — on an uncacheable hot site that was
the dominant dispatch cost."*

**The handle-heap twin (`heappic_fill`) never got that probe.** It has a fast negative bail, but
only for `$`-synthetic and `java/` receiver classes; fastutil is neither, so every fastutil
collection call fell through to the full path. The handle heap is where the collections live.

## The fix and its count

Probe the site's own cache for `(arc_key, -1)` before the three `to_string()`s. Behaviour-
identical for the same reason the `$`/`java/` bail is: `heappic_fill` only FILLS a cache, so
declining to re-derive a verdict already recorded in that very cache cannot change which method
dispatches — the EXECUTE half runs the call either way.

```text
pic_neg_hit   9,473,938   (45-second capped boot; the full boot's population is larger)
```

Kill switch `RAVA_NO_PIC_NEG`. 4 bit-exact gates and 21 differential probes green.

## ⛔And it is worth 0.28% — the population was real and the price was not

Wall was unreadable today: a null test against `RAVA_NONEXISTENT_NULL_TEST` read **-7.99%
between identical arms, 3/3 pairs favouring ON** — the recorded warm-up bias, and a floor twice
yesterday's ±3.1%. So the arms were compared on CPU seconds, six product boots, order alternated
between rounds to cancel that bias:

```text
off  cpu 35.2  35.6  35.4        wall 33.2  33.6  33.7      Done 0.466  0.414  0.414
on   cpu 35.5  35.1  35.6        wall 33.5  33.2  33.5      Done 0.415  0.406  0.405
CPU min-of-N delta: -0.28%       (arm spread 1.4% — the instrument IS tight here)
```

**9,473,938 eliminated resolves cost about 0.1 s of a 35 s CPU boot — roughly 10 ns each.**
Not the 200-400 ns that three `String` allocations and a `resolve_method` walk suggest, because
everything inside that walk is already memoized and the allocations are small.

⇒ That refutes the arena twin's own comment, which calls the same per-call resolve *"the
dominant dispatch cost"* on an uncacheable hot site. It was never priced; it is now, and it is
under the floor.

The change stays: it is strictly less work, behaviour-identical, gated and probed. It is **not
counted as a win**, on the same terms as the synthetic-field-id conversion and the in-place
combo fold — kept for doing less, not for a delta.

## What this closes

`docs/per-op-price-list.md` carries the line *"crossings 8.5 M | 2.6 s | 97% reach natives with
no compiled body"*. The 8.5 M is this same population. Its **resolve half is now measured at
~0.1 s**, so whatever that line costs, it is not the resolving — and the calls themselves are
already served by `try_last_arm` in one comparison. The line is smaller than the map says.

⭐**Eighth time this session that converting a population into a share of the boot killed the
claim — and the first where I built it before converting.** The rule holds in both directions:
9.5 M events is not a big number until it is multiplied by a price that was measured rather
than assumed.

## The method note

Three separate records in this tree assert that a negative dispatch verdict is permanent. All
three describe the stored value. **Not one of them had counted how often the verdict was
computed**, and the counter that would have shown it was reporting under a name — "IC misses" —
that everyone, including me, read as "seam crossings".

⇒ When a census names a POPULATION, check which event it increments before deciding what to
build. The first thing built here was correct, measured, and served 0.1% of the population it
was aimed at.

## ⭐The window was the whole disagreement — and it is now an instrument

The `diag` registry gained a METER (`crate::diag::meter`, `RAVA_DIAG=seam.pic_resolve`): it
wraps a site, times one event in 256, subtracts the calibrated cost of its own `rdtsc` pair and
reports events, p50, mean, estimated total and share of wall. Pointed at this very site — price
already known from the six-boot A/B — it read **242.8 ns/event, 3.33% of wall**, ten times the
A/B. Two corrections followed, and only the second was the real one:

1. **Mean → median.** `rdtsc` counts wall cycles, so a sample landing on a preemption adds
   microseconds; on a box carrying ~29% background load that dominates a mean. With log2 buckets
   and a median the reading fell to 153.8 ns — still 6x the A/B.
2. **The window.** A `RAVA_DIAG_EVERY_MS=3000 RAVA_DIAG_DELTA=1` series:

```text
36s  cumulative      651,157 events
39s                1,842,510
42s                3,639,224
45s                5,587,944
```

The boot finishes near 33 s. **86% of this population is server ticking, not booting.** Inside
the boot it is ~0.15 s of 33 = 0.5%, which is what the A/B measured. Both instruments were
right; a count taken over a capped window and a wall A/B taken to `Done` are not measuring the
same program.

## The phase signature, first seen the same way

With every counter deltaed, three populations over 3-second windows, boot then tick:

| population | boot windows | tick windows |
|---|---|---|
| `crossings` | 683k / 783k / 753k | **1,694k / 3,158k / 2,856k** — 4x |
| `births_handle` | 324k / 317k / 373k | 220k / 150k / 135k — 0.4x |
| `births_arena` | 211k / 262k / 248k | 224k / 202k — flat |

The boot and the tick are different workloads by a factor of four in the engine's largest
population, so **any census taken over a fixed window mixes two regimes.** The recorded 8.5 M
boot crossings survives this (≈250k/s × 33 s ≈ 8.3 M); the 9.06 M ICMISS figure does not.

⚠And the tool had to be corrected against itself twice more: counters ignored
`RAVA_DIAG_DELTA` entirely while gauges honoured it, so the first series printed cumulative
totals and per-window rates side by side under one "DELTA since previous" banner — which read as
an accelerating birth rate that does not exist. Both fixed at the source, with the misreading
recorded beside the line that caused it.
