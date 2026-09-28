# The fast path was admitted by concrete class; the guest declares the interface

2026-08-27.

## The shape of the bug

`fu_routable` (crates/rava-interp/src/jit/direct.rs) decides which call sites the code
generator may route to `fu_fastpath` — the service level the seam census prices at
**50-110 ns**, against **700-1700 ns** for the general path
(`jit_call_body` → synthetic frame → `invokevirtual` → `super_chain` → natives cascade).

Its admissions named concrete classes: `java/util/ArrayList`,
`it/unimi/dsi/fastutil/ints/*`, `it/unimi/dsi/fastutil/doubles/*`.

**A call site's owner is its STATIC type.** Minecraft declares `Map<..>`, `List<..>`,
`Collection<..>`, `Set<..>` at essentially every site, so the concrete admissions never saw
the traffic they were written for. `java/util/Map` was not admitted in any form at all —
`FuKind` had no map variant, so a map read had no machine-code arm to be admitted to.

`java/util/Map.get` is **6.92% of the boot's 64,088,346 compiled-to-host crossings** — the
second-largest callee in the whole census.

## Why widening the admission cannot change behaviour

`fu_fastpath` classifies the **receiver**, not the site. The map block asks whether the
receiver carries a native `$map` table id (`Vm::fu_map_id`, resolved by interned field id —
a field read, not a name hash) and hands the call to `map_method`, the same function the
general path ends at. A receiver that is a guest `Map` with a real Java body has no `$map`,
answers `None`, and takes the route it always took.

So the admission list controls **which sites get to ask**, never **which implementation
answers**. That is the whole soundness argument, and it is the same one the `Map$Entry` and
`ArrayList` admissions already rest on.

## The refuted reading (kept, because the census that killed it is reusable)

The first hypothesis for the same 98-vs-1,700 ns gap was that an interface-declared site
pays a **declining cascade pass over each super-chain link** before one answers:
`try_invoke_chain` runs the whole owner cascade per link, and `ClassLoader::super_chain` is
4.0% of the probe's host profile.

A memo of "which link served" was written and then measured by count. `RAVA_CHAIN_CENSUS`
on `bench/SeamCost3.java`:

```
[CHAIN] calls=12000016 links=24000032 avg_len=2.00 memo_hits=0 none=0
[CHAIN]   served at link 0: 12000016 (100.00%)
```

**Link 0 serves 100.00%.** There is nothing to skip. The memo was deleted; the census is
kept (gated, `RAVA_CHAIN_CENSUS`, reported from the CLI's end-of-run dump).

What the count *does* pin is the other reading of that 4.0%: it is the super-chain cache
**lookup** — a hash of a class name on every crossing — which is a separate lever and
shrinks as intrinsics take traffic off the general path.

⚠The census was invisible for one round because it was wired only into the `pc_profile`
dump, a path this configuration does not reach — the same failure mode recorded for five
other censuses that printed only from `lane_probe`. It now reports from `run.rs`, next to
`bridgestat::dump`.

## The bug the widening introduced, and the rule it teaches

The map block was first written as an arm of the `match (name, desc)` that holds the box
unwrappers. `size()I` and `isEmpty()Z` are **shared with the list shapes** classified below
that match, and a list carries no `$map` — so the arm answered `None` for every
`List.size()`, pushing a population the list arm already served back onto the general path.

⭐**A fast path that returns `None` has not declined politely — it has ended the search.**
The block is now a statement before the classification, so a receiver it cannot serve falls
through to the code that can.

## Measurements

Alternating arms, single binary, `RAVA_NO_MAP_INTRINSIC`.

```
bench/SeamCost3.java   Map.get(String)
  ON  430  440  485  490
  OFF 1305 1320 1435 1490 1555          => 3.2x, ranges disjoint

MC boot, 8 rounds off,on,on,off,off,on,on,off, P-cores
  intrinsic ON   wall 38.9 38.8 38.3 37.9   min 37.9  median 38.55
  intrinsic OFF  wall 41.4 41.8 40.4 40.6   min 40.4  median 41.00
  => min -6.19%, median -5.98%; EVERY ON run faster than EVERY OFF run
```

Counted expectation from the census was ~4.8% (6.92% of the seam, cut 3.2x). This is the
first mechanism in a long while where the counted estimate and the measured wall agree.

`Done` does not move: it times the final short phase, not the phase this touches.

4 bit-exact gates OK, 21 differential probes match `java -Xint`.

## The same widening applied to iterators HANGS the boot

`ObjectSet.iterator` is 4.94% of the census and the iterator gate
(`RAVA_ITER_INTRINSIC`, opt-in) admitted only `java/util/Iterator` — which reaches the loop
body of an enhanced-`for` but never the `iterator()` that CREATES it, nor fastutil's object
iterators. That is exactly the asymmetry the map fix repaired, so the same widening was
applied and the gate flipped to default-on.

```
RAVA_NO_ITER_INTRINSIC=1 (widened intrinsic OFF)   wall  38.3 s   Done 0.448   reached
widened intrinsic ON                               wall 700   s   Done  ---    NEVER REACHED
```

Twice, on a fresh binary, with the boot harness's own calibration refusing to measure.

⭐⭐**The 4 bit-exact gates and all 21 differential probes passed on that build.** A fast
path that hands back a plausible-but-wrong iterator does not corrupt an answer a gate
hashes — it fails to TERMINATE, and no correctness gate in this tree looks for that shape.
Every check the project owns was green while the product did not boot.

The narrow gate is itself sound but does not earn a default: 38.3 / 37.3 / 37.8 on against
38.3 / 38.3 / 38.3 off is −1.3% median, under the boot floor. Reverted to opt-in, with the
measurement recorded in the gate itself.

## Widening the map arm past `get`

`getOrDefault`, `put`, `remove`, `containsKey`, `containsValue`, `size`, `isEmpty`,
`entrySet`, `keySet`, `values` now share one predicate (`crate::map::fu_map_op`) consulted
by both `fu_routable` and `fu_fastpath`, so the two lists cannot drift apart — a site
admitted by one and declined by the other pays an extra refused crossing.

It measured **neutral against `get` alone**: min 38.3 / median 38.8 versus the `get`-only
build's 37.9 / 38.55, arms again fully separated from the OFF arm (40.8 / 41.3). The win is
`get`; the rest is coverage of named census entries (`Map.put` 1.40%, `Map.entrySet` 2.09%)
that did not show up on the wall.

## What the census says after the fix, and what came next

`RAVA_JIT_CB_CENSUS` could not be read on a product boot at all until this arc: it printed
only from the `pc_profile` dump. `run.rs` was not enough either — a Minecraft server ends
inside `System.exit`, not by returning from `main`. It now also prints from the
`System.exit`/`Runtime.halt` arm, and `RAVA_CENSUS_MS=<n>` prints it from a timer, which
depends on no exit path at all.

First reading, 70 s product boot, 9,751,478 callbacks, 468 distinct callees:

```
12.84% Function.apply                  5.64% Reference2ObjectMap.reference2ObjectEntrySet
 4.60% Object.equals                    4.25% String.valueOf
 4.21% ObjectSet.iterator               3.84% Objects.equals
 2.61% LoadingCache.getUnchecked        2.38% Objects.requireNonNull
 2.19% Comparable.equals
 Optional: get 3.26 + isPresent 3.05 + map 1.09 + orElse 1.02 + ifPresent 0.68 + isEmpty 0.45
```

⭐**`java/util/Map.get` (was #2, 6.92%) and `java/lang/Class.cast` (was #1, 7.67%) are gone
from the top 40** — the mechanism is confirmed by the disappearance of its target, not only
by the wall.

**Shipped from that list:** the `Optional` readers (`get`, `orElseThrow`, `isPresent`,
`isEmpty`, `orElse`) and `Objects.requireNonNull`. The writers `of`/`ofNullable`/`empty`
already had arms; the readers did not.

⛔**Not shipped: the fastutil view route.** `reference2ObjectEntrySet` is the largest
remaining non-lambda callee, and those maps carry packed key/value arrays rather than a
`$map`, so `map::dispatch`'s Plan-A route was reachable only down the general path. Routing
them by receiver class measured an ON arm of 38.8-39.8 s against the 37.8 s the same arm
read without it, on two consecutive interleaved A/Bs, while the OFF arm did not move — a
regression, not a null. ⭐Its first version also **allocated the class name in order to
decide whether it wanted the class name**, on every miss; hoisting that behind the predicate
recovered most of the loss but not all. Reverted, and 5.64% stands as an open target for a
route that does not need the receiver's class name.

Final state of the bundle (`RAVA_NO_MAP_INTRINSIC`), 6 rounds, P-cores:

```
ON   wall 37.7 39.4 38.3   min 37.7  median 38.30
OFF  wall 41.4 40.8 40.8   min 40.8  median 40.80   => min -7.60%, median -6.13%
```
