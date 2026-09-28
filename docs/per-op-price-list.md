# What each operation costs, against HotSpot

2026-08-28. `bench/NativeCost.java`, every case its own method called repeatedly so it compiles,
no `bridgestat` accounting (which inflates a high-count native far more than a low-count one and
therefore biases its ranking, not just its numbers).

⚠**AND ONE PROCESS PER CASE, WHICH IS THE SECOND CORRECTION THIS TABLE NEEDED.** Run together,
every case after the first measures against a heap the earlier ones filled: the control loop
alone reads **20 ns** and the same loop at the end of a combined run reads **49-64**. Everything
in the first version of this table was therefore inflated by roughly 2x. The numbers below are
one process per case (`NativeCost <n> <reps> <case>`).

```
                   rava (isolated)   java     over rava's control
control loop            20 ns          0
List.size               32 ns          0            12
Map.get                189 ns          4           169
putstatic ref          239 ns          0           219
new Object             344 ns         ~0           324   (birth ~105 once the putstatic is out)
List.iterator          366 ns          0           346
new ArrayList        1,056 ns         ~0         1,036
```

The combined-run table is kept below because the RATIOS between its rows still hold and it is
what the rest of this document was written against — but a per-op price quoted from it is about
twice the truth.



```
                     rava      java      over rava's control loop
control loop           49 ns      0
getstatic ref           1 ns      0        (the value cell works — 1 ns)
List.size              61 ns      0            12
Objects.equals         63 ns      0            14
List.get               67 ns      0            18
add + remove          209 ns      5           160
putstatic int         293 ns      0           244
Map.get               299 ns      4           250
Map.containsKey       311 ns      3           262
putstatic ref         542 ns      0           493
List.iterator         615 ns      0           566
new Object            618 ns     ~0           569
new AtomicLong      1,716 ns     12         1,667
new ArrayList       3,349 ns     ~0         3,300
```

## What the table says that nothing else did

**A native served from machine code costs ~15 ns over the loop.** `List.size`, `List.get`,
`Objects.equals` are all at the floor. The bridge is not expensive when `fu_fastpath` serves it —
which retires the "every native call costs a microsecond" reading that `bridgestat`'s absolute
numbers suggest.

**`getstatic` of a reference is 1 ns.** The value cell is doing its job perfectly.

**Everything expensive is a BIRTH.** `new Object` 618, `new AtomicLong` 1,716, `new ArrayList`
3,349 — against a JVM that does all three in single-digit nanoseconds. Splitting `new ArrayList`:
the allocation is ~2.4 µs and the `add` that follows is ~110 ns, so it is the birth, not the
operation.

⚠**And `new Object` takes ZERO crossings** (`RAVA_DIAG=seam`, case 11) — it is emitted inline in
compiled code and still costs 569 ns over the loop. Splitting it against a control that does the
same `putstatic` without allocating: the store is ~383 ns of that and the birth ~186 ns. So the
inline birth is not the whole story either.

## What was counted before being optimised, and therefore not optimised

`putstatic` is 244-493 ns against HotSpot's 0, and the compiled path is visibly wasteful: it
builds a synthetic `Frame` with a `vec![value]` allocation, touches an `Arc` refcount, hashes the
owner's class name through `get_or_load`, and bumps `static_gen` — which invalidates the
getstatic value cell globally and is folded into every rendered-key memo tag.

**The boot executes a few THOUSAND of them.** Per-slice deltas from `RAVA_DIAG=statics` on a
product boot: 493, 677, 64, 0. At 480 ns that is about a millisecond — **0.003% of the boot**.

The code's own comment predicted exactly this ("a static is written ONCE, in `<clinit>`") and
asked for the population to be counted before anything was touched. It had never been counted.
Now it has, and the answer is: do not touch it.

⭐This is the third time in one session that converting a per-op price into a boot share stopped
a change that would have measured nothing — the others were string map keys (166,606 probes a
boot ≈ 25 ms) and the super-chain cache (12.0 M lookups ≈ 2.6%, under the floor).

## And the births do not matter on the boot either — counted, not assumed

`new ArrayList` at 2.4 µs is a real per-operation price. It is also, on a product boot, a rare
event: **no `<init>` appears anywhere in the top 40 of the boot's native self-time.** So the
"everything expensive is a birth" reading above is true per-op and irrelevant per-boot, exactly
like `putstatic`.

That is the **fourth** time in one session that counting the population stopped a change which
would have measured nothing:

| candidate | per-op price | boot population | verdict |
|---|---|---|---|
| string map keys | ~150 ns | 166,606 a boot ≈ 25 ms | 0.07% — not built |
| super-chain cache | ~80 ns | 12.0 M lookups ≈ 1 s | 2.6%, under the floor — not built |
| `putstatic` | 244-493 ns | a few thousand ≈ 1 ms | 0.003% — not built |
| collection birth | 2,400 ns | not in the top 40 | not built |

## What `bridgestat` can and cannot say

The same session found the ranking that pointed here has two limits, both discovered by
disagreeing with `NativeCost`:

* it charges its own per-call lock and hash update to the callee, so a native called 780,000
  times carries far more of that than one called 300 — the bias is in the RANKING;
* "self" is not a native's own cost when the native runs guest code: a boot puts
  `CompletableFuture#thenCompose` at 2.74 s over **2 calls** and `List#forEach` at 2.44 s over
  **8**, which are not expensive natives but natives that drive the whole datapack load inside
  themselves.

⇒ Use `bridgestat` to find WHICH natives the traffic reaches, then price them one at a time in
`NativeCost`. Both limits are now written at its own source so the next reader does not repeat
the misreading.

## The accounting, and what it does not close

Every population this session counted, priced at its isolated cost, against a 36 s boot:

| population | count a boot | isolated price | share |
|---|---|---|---|
| compiled→host crossings | 8.5 M | ~170 ns | 1.4 s |
| map key derivation | 1.5 M | 1,624 ns avg | 2.4 s |
| interpreted opcodes | ~197 M | ~5.5 ns | 1.1 s |
| **object births** | **8.6 M** (4.39 M arena + 4.22 M handle) | ~105 ns | **0.5 s** |
| `putstatic` | a few thousand | 219 ns | ~1 ms |
| collection births | not in the top 40 | 1,036 ns | — |

**Named total ≈ 5.5 s of 36.** Thirty seconds are not in any of them.

⭐That is the same gap a previous session recorded as *"every counted population × its per-event
price = a few seconds against forty"* — and it is now known NOT to be allocation, crossings, key
derivation, interpretation, statics or births, each ruled out by a count rather than by argument.

What remains is the shape the host profile has shown from the beginning: **no function above
3.4%, and the top forty summing to under half**. The missing time is a long tail of hundreds of
small things the engine does per guest operation. That is not a bug with an address; it is the
architecture, and it is why the recorded verdict is a rewrite rather than an optimisation.

## ⚠ The accounting above compares against the wrong denominator

`[PC-PROFILE] -- host side (16,497 samples, 99.0% of wall)` is **one thread's** table — a boot
prints several, one per sampled thread. At the sampler's 1 kHz, 16,497 samples is about **16.5
seconds**, not the 36 s wall. Every module percentage in
`docs/the-profile-is-flat-only-by-function.md` is therefore a share of ~16.5 s of one thread, and
multiplying it by the wall overstates it by roughly 2x.

Re-stated against the right denominator, the named populations close far better than "5.5 of 36":

| population | count | price | time |
|---|---|---|---|
| compiled→host crossings | 8.5 M | ~300 ns | 2.6 s |
| map key derivation | 1.5 M | 1,624 ns | 2.4 s |
| capturing-lambda indy | 4.3 M | ~310 ns | 1.3 s |
| births: objects | 8.9 M (4.55 M arena + 4.33 M handle) | ~105 ns | 0.9 s |
| **births: arrays** | **8.74 M** — never counted until now | ~150 ns | 1.3 s |
| interpreted opcodes | ~197 M | ~5.5 ns | 1.1 s |

**≈ 9.6 s against a sampled-thread total of ~16.5 s.** The engine's own profile agrees:
`vmstate` 25.8% + `heap` 12.7% + `opcodes` 11.6% + `mimalloc` 8.2% = 58% of that thread, and
those four are exactly the crossings, the births, the interpretation and the allocator.

⭐So the arithmetic DOES close, and the earlier "thirty seconds are in none of them" was a
denominator error of my own — the same class of mistake as the two price-list corrections above.
What does not close is the ratio to a JVM: every one of those six populations is 30-300x its
HotSpot equivalent, and none of them is a defect. That is what "the architecture, not a bug with
an address" actually means, now with the arithmetic behind it instead of a gap.

## Every line of the map now has a measured reason it cannot shrink

| line | time | why it stays |
|---|---|---|
| crossings 8.5 M | 2.6 s | 97% reach natives with no compiled body — nothing to devirtualise into |
| key derivation 1.5 M | 2.4 s | 1.8 s is two DFU records; the component render fixes them and changes the KEY VALUE, which moves DataFixerUpper's own cache hit rate (5 hypotheses tested) |
| indy lambdas 4.3 M | 1.3 s | 99.8% capture, so a fresh object is required — Java allocates too, at ~5 ns |
| arrays 8.95 M | 1.3 s | distinct creations, not growth (`array_push` is `Vec::push`, geometric) |
| objects 9.0 M | 0.9 s | inline in compiled code, zero crossings, ~105 ns each |
| interpreted opcodes ~197 M | 1.1 s | 66% of invocations are a long tail no tiering policy reaches |

And the seventh population count of the session, taken rather than assumed: the array element
interner hashes an eighteen-character type name on each of those 8.95 M allocations —
**0.22 s, 0.7%, under the floor.** Recorded at the site; not built.

⇒ Nothing on this map is a defect. Each line is 30-300× its HotSpot counterpart for the same
reason: the engine does a correct, general thing per event where a JVM does a specialised one.
That is the whole gap, it is now arithmetic rather than assertion, and it is why the standing
verdict is a rewrite of the object and call representation rather than another optimisation.
