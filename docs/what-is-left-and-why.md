# What is left, sorted by what actually blocks it

After a session of taking the seam callee by callee, the remainder no longer sorts by size. It
sorts by KIND of blocker, and each kind needs a different thing.

## 1. Route — essentially exhausted

Eleven callees moved from the general path (700-1,700 ns) to an early cascade arm (270-700) or to
`fu_fastpath`, which serves from machine code with no synthetic frame (40-110):

```
                        before    after    java
  Boolean.booleanValue   1365       50       0 ns    27x
  Objects.equals         1620       50       2       32x
  System.identityHashCode 810       40       1       20x
  BitSet.nextClearBit    2165      325     0-4
  Map.get                 588       98       3
  String.valueOf(int)     380      150      15
  Class.cast             1255      590       0
  DoubleArrayList.wrap   3410      540       1
  R2OMap entry-iter      8180     4920      15
```

Two traps here cost a false conclusion each, and both are worth remembering:

- **`Class.cast` sat behind `try_last_arm`**, the per-thread memo of the arm that served the
  previous native call. A loop calling one native hits it every time, so the new fast path never
  ran and measured identically ON and OFF.
- **`Objects.equals` sat behind `emit_direct_mono`**, which is default-on and claims the site
  before the emitter reaches the `fu_routable` question. Also measured as exactly zero.

⇒ **When a fast path measures exactly zero, check that it RUNS before concluding it does not pay.**

## 2. Frame — needs the contiguous value stack

`Function.apply` is 5.37% of crossings at 750 ns against HotSpot's 1, and it was probed to a
complete explanation rather than optimised:

- not the argument build (`argvec_allocs=0` over 4,100,000 applies);
- not the method resolution (a memo on (site, owner) measured neutral, reverted);
- it is `self.invoke(decl, ...)` — a real framed call, which this tree prices at 331 ns against
  `java -Xint`'s 17.4.

Everything that ends in guest code lands here. The cure is `docs/contiguous-stack-plan.md`, priced
there at ~5.4% for stages 2+3 **as a whole** — its own note says the line cannot be shipped or
measured piece by piece.

## 3. Semantics — the work is real, not the route

- **`iterator()` drains the collection.** `iterate_object` copies `$elems` into a `Vec` and
  `iterator_of` allocates a second array from it. Routing it earlier measured **neutral** (5,680
  vs 5,600 ns), because the cost is the two copies, not the distance.

  ⛔**And they cannot simply be shared.** `Iterator.remove()` locates its target in the
  ITERATOR's own array (`elems.get(pos - 1)`) and then deletes through `$source`; with a shared
  array the following `next()` would walk shifted content. Sharing is sound only for a receiver
  whose `$immutable` is set, which is a subset worth ~1.4% — recorded, not built.

- **`DoubleArrayList.wrap` must allocate the list object.** After memoising its two field ids
  (3,410 -> 600 ns) and routing it to machine code (600 -> 540), what remains is one handle birth.

## The number that frames all three

64,088,346 compiled-to-host crossings a boot. The general crossing costs ~800 ns (measured:
`System.identityHashCode` before its fast path), `fu_fastpath` 40-110, an inline emission 20-45.
**The cost is a population, not a function** — which is why five sessions of function profiling
found nothing above 4.4%, and why the work is necessarily callee by callee until the frame line
lands.


## 4. Parallelism — blocked by the GIL, verified rather than assumed

The boot runs at **1.05 cores** against java's 3.2. Two facts settle what that is and is not:

- **java's extra cores are its compiler**, not guest work: `java -Xint` boots the same server at
  1.22 cores, and our own compilation is ~4.4% of CPU.
- **Our executor gate is not the blocker.** MC hands `supplyAsync` a two-argument call whose
  executor carries no resident-task capability, so the body falls through to the calling thread —
  and `RAVA_ASYNC_ANY_EXECUTOR=1` exists precisely to override that. Measured: **46.5 / 52.9 s
  against 45.4 s by default, and still 1.02 cores.** The bodies do reach our task runtime; the
  runtime executes guest code under the GIL, so they serialise anyway.

`RAVA_GILSTAT` agrees from the other side: `real_threads=1` on a full boot. One thread ever
registers as a guest-code executor.

⇒ Parallelism is not a gate to open or an executor to prove. It is one global lock over one heap,
and lifting it is the engine change this tree has named from the start — not an optimisation.
