# The methods that never compile, and why

*2026-08-26*

Found by COUNTING, which is the only instrument that works today — three null tests on this box
disagreed in sign at ~8%, so no A/B of a few percent can be run here (`boot_ab_flag.ps1` carries
the record).

## The defect, named by its own counter

`jit_run` declines to compile on a worker thread: its `JITModule` dies with the thread, and it
cannot touch VM state off-GIL to resolve compile plans. `JIT_GATE[6]` has counted the consequence
for a while without anyone reading it:

```
worker visits                                            617,728
worker visits to a method ALREADY past the threshold     482,902   (78%)
```

A method proven hot, compiled never, unless the main thread happens to call it again.

And a second rule compounds it. `jit_native_served_deep` refuses any guest class descending from
a native-backed one — which is most of the collection world:

```
gson/LinkedTreeMap extends java/util/AbstractMap        ~830 k interpreted entries
fastutil DoubleArrayList under AbstractCollection         301 k
guava AbstractIndexedListIterator                          69 k
                          36 methods, 1,688,387 entries, ZERO compiled
```

All of it inside the datapack phase, which the phase map prices at **44% of the boot and 10.5x
behind java**.

## Two fixes, both judged by the gates

**A worker can leave a note** (`RAVA_WORKER_HOT`). Three owned strings — no pointers, no heap
access — legal off-GIL. The owning thread drains them where it already drains finished background
compiles, 16 per safepoint.

**A `java/*` ancestor is not a reason to refuse a class's own bytecode** (`RAVA_JIT_GUEST_SUB`).
The stated risk is that such a class "reads real fields the native layer never populates" — but
those 830 k entries ARE that bytecode running correctly, because the natives do not serve those
methods at all. Compiling the same bytecode the interpreter already executes cannot change what
it reads.

⚠Prior art, corrected: the same narrowing exists as `RAVA_JIT_NS_BYTECODE` and is recorded as
"worth zero" — but it only judges `it/unimi/` classes, so it never reached gson, which is 830 k
of the 1.69 M.

## What they move

| | baseline | worker-hot | + guest-sub | + ns-bytecode |
|---|---|---|---|---|
| interpreted entries | 3,196,280 | 3,233,141 | 2,486,500 | **2,226,050** |
| stuck methods | 36 | 32 | 25 | **21** |
| their entries | 1,688,387 | 1,694,581 | 851,758 | **559,432** |

**−30% interpreted method entries, −67% of the stuck population.** 4 gates bit-exact and 21/21
differential probes against `java -Xint` for every combination.

Note the shape: compiled methods went 1216 → 1501 → 1244 → 1488 while interpreted entries fell
monotonically. **Which** methods compile matters more than how many.

## What is left stuck

```
150,917 x2  fastutil DoubleArrayList.equals (both overloads)
104,803     gson LinkedTreeMap.findByObject
 68,957     guava AbstractIndexedListIterator.hasNext
 48,218     guava Lists.newArrayList
 40,505     net/minecraft/resources/Identifier.read
```

## Priced, on a settled box — and it does not pay

The null test came back inside the noise band (−2.68% min / +1.21% median, signs disagreeing,
not disjoint), so the A/B could finally be run. All three flags together, warm-up discarded,
alternating order, 4 rounds:

```
ON  min 47.9  median 48.55
OFF min 47.3  median 48.70
delta  +1.27% (min)  −0.31% (median)
```

**Neutral.** Signs disagree and both magnitudes sit inside the null band.

And the reason is exact, measured from the other end: with the flags on, interpreted opcodes go

```
197,100,000  ->  59,700,000     (-70%)
```

137 million interpreted opcodes removed. At rava's ~5.5 ns/opcode that is **0.75 s of a 48 s
boot — 1.5%**, which is precisely what "inside the noise band" means.

So the mechanism works completely and is worth nothing, because **interpretation was never the
cost**. That was inferred earlier from the opcode count alone; it is now measured from both ends:
remove 70% of it and the boot does not move.

## Status

All three flags stay default-OFF: correct, effective at what they do, and neutral on the wall.
The defect they fix is real and worth knowing about — a method proven hot that never compiles is
a bug whatever its cost — but shipping a neutral change is not this project's discipline.
