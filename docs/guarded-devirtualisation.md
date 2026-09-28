# The call a JVM does not make — built, measured, and priced

2026-08-29.

## Why this and not another lever

Dispatch, the seam, the frame, the caches, the operand representation, the lane — every lever of
the last two months made a CALL cheaper, and every one landed on the measurement floor. They
share a blind spot: **HotSpot does not make the call.** It reads a site's receiver profile,
devirtualises the monomorphic ones, splices the body in behind a class guard, and `ArrayList.size()`
becomes a field load. A cheap call cannot beat an absent call.

Our inliner bound callees by TYPE only — static, special, and virtual to a final-effective method
— and refused `invokeinterface` outright. On `bench/StreamApplyCost.java` it logged **zero**
collapses.

## What was built

`jit::emit::guarded_inline` emits, at a site whose profile names one concrete arena class:

```text
          is_arena(recv) ? guard : slow
guard:    class_id(recv) == K ? inline : slow
inline:   <callee body spliced, no call>   -> merge(result, live refs)
slow:     <the site's original emission>   -> merge(result, live refs)
```

The profile comes from `sitemono`, a lock-free per-site table fed from the interpreter's
inline-cache MISS — once per (site, class) pair, not per call. The `devirt` probe answers the same
question but takes a global mutex on every interpreted `invokevirtual`, which is why a boot with
it armed cannot be measured at all.

**The guard, not the profile, is what makes it correct.** A stale profile, a class first seen
after compilation, or a table collision all take the slow arm — the code the site ran before.

## What it does, counted

```text
admitted     585      monomorphic sites the profile offered
emitted       84      guarded inlines actually in machine code
guard_hits 1,782,416  inline arms EXECUTED on a 33-second boot
```

**1.78 million calls not made.** The guards hit, ~21,000 times per site.

## And what that is worth, priced honestly

1.78 M × the 30-70 ns a call of this shape costs = **0.05-0.12 s of a 33 s boot, 0.15-0.4%.**

That is under this box's measurement floor, and it explains two A/Bs that looked contradictory:

```text
first six boots   off 39.4 39.5 40.0   on 38.4 38.9 39.0   -2.54%, disjoint
same hour, again  off 40.2 39.8 41.5   on 39.7 39.9 40.1   -0.25%, overlapping
```

Neither was measuring the mechanism — both were measuring the box. The **count** is what is real,
and the count says the mechanism is sound and currently too small to see.

## What that makes the next step, precisely

Not "tune it". **Raise the site count and reach the population it was built for.**

* 84 emitted of 585 admitted — the gap is callers that never compile in the window and bodies the
  inliner still refuses.
* The collection API — **38.4 M `fu_fastpath` calls a boot, 8.12% of the wall** — is not reachable
  at all: those receivers are `java/util` classes served by Rust natives, and a native cannot be
  spliced into compiled code.
* Guest `java.util` bytecode CAN be spliced. Measured today with the inliner working:
  native 5,293 ms, guest 5,752, **guest+devirt 5,550** — inlining recovers about half of what the
  migration costs, and the migration is still behind. It is not yet a win; it is now a mechanism
  with a direction.

⚠**And the interface half is gated OFF** (`RAVA_DEVIRT_IFACE`). It raises admitted sites 297 →
866, which is where the whole collection API arrives, and took the boot A/B to +0.51% with the
arms overlapping and the probe matrix to its worst row. More coverage is not automatically more
speed. The tree's own `devirt::record_steady` exists because someone already found that an
interpreter warm-up profile "biases toward monomorphism" — driving the guard from the
steady-state PIC instead is the next thing to try before widening again.

## One more instrument lesson

The hit counter was first written as `static GUARD_HITS: [u64; 1]`, and the generated store
faulted immediately: `[CRASH] 0xc0000005 write of 0x7ff6871f00c0`. An immutable Rust static lands
in read-only memory and does not become writable because machine code holds its address. It is an
`AtomicU64` now.

## And the two ways to reach the collections, both measured today

**1. A hand-written inline arm per operation.** `List.size()` was the obvious one — 4.44 M calls a
boot, the largest single collection operation. It was built and it fires (175 sites, 4 gates and
21 probes green), and the counters rejected it: with the arm on, `size` calls REACHING the native
rose **4.41 M → 6.97 M (+58%)** against 8% growth in total traffic. The guard misses, because
`size()` in Minecraft arrives overwhelmingly on fastutil and guava receivers, not on this
engine's `java/util/ArrayList`.

⭐**The census that chose that target counted calls by METHOD NAME.** It could not see that one
name arrives on a dozen unrelated receiver classes. A population is not a target until its
RECEIVERS are counted — which is precisely what a devirtualisation profile does, and why the
general mechanism is the right shape and the hand-written arm is a special case that guessed the
class wrong.

**2. Let those receivers run their own bytecode.** `RAVA_JIT_NS_BYTECODE=1` judges an `it/unimi/`
class by its own bytecode; with it on, the collection population does not move at all
(`fu_fast` 39.2 M either way). The flag governs whether such a class may be COMPILED, not who
SERVES it — the serving decision sits in `native_served_deep` / `force_native_collection`, and
flipping that wholesale is the change this tree already recorded taking a boot from 40 s to a
400-second timeout.

⇒ So the 8.12% is not reachable by widening a guard or by flipping a compile flag. It needs the
receivers themselves to stop being native-served, one family at a time, with the inliner in place
to make the bytecode worth running. That is a migration, and it now has a mechanism waiting for
it rather than the other way round.

## ⭐The receivers, finally counted — and the target is not what the name census suggested

The `List.size()` misfire proved the instrument that was missing: this population had only ever
been counted by METHOD NAME. Sampled 1 in 256 by (receiver class, method), a product boot says:

```text
it/unimi/dsi/fastutil/doubles/DoubleArrayList#getDouble   10,528,512
net/minecraft/world/phys/shapes/CubePointRange#getDouble   6,434,304
java/util/Iterator#hasNext                                 4,032,000
```

**`getDouble` on `DoubleArrayList` is 10.5 M calls a boot** — the largest single (class, method)
pair in the engine, more than twice the whole `size` population that the name census pointed at,
and it is a primitive read out of a backing `double[]`. With `CubePointRange#getDouble` it is
**17 M of the 38 M, 44% of the population**, and both are ONE concrete class each — not the dozen
unrelated receivers that made the `size` guard miss.

This is what the name census could not show and what the receiver census does in one run:

| chosen by | target | outcome |
|---|---|---|
| method name | `size` 4.44 M | guard pinned to `ArrayList`, missed, +58% traffic |
| receiver class | `DoubleArrayList#getDouble` 10.5 M | one class, one primitive read, guard cannot miss the way `size` did |

At the measured ~65-70 ns a `fu_fastpath` call costs, 10.5 M is **0.7 s of a 33 s boot, ~2.1%** —
and 17 M is ~3.4%. That is the first collection target whose receivers are known before the code
is written, which is the whole lesson of the day applied to itself.
