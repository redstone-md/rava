# The boot's phase map, against HotSpot — and where the parallelism gap really is

## The map, both engines, same world and classpath

```
                          rava            java
start -> `Environment:`   22 s            ~6 s       ~3.7x
`Environment:` -> recipes 13 s (+7 net)    1 s        ~13x
recipes -> Done            1 s           ~0.2 s
                          ----            ----
wall                      44 s            7.5 s
cpu                       46 s           24.2 s
cores                     1.05            3.2
```

`bench/phases.ps1` and `bench/phases_java.ps1` produce these from MC's own log timestamps.

## What the two phases are

**start -> `Environment:` (22 s, half the boot).** The guest profile is unambiguous: `VoxelShape`,
`BitSetDiscreteVoxelShape.isFull`, `Shapes.joinIsNotEmpty`, `IndirectMerger`,
`BlockBehaviour$BlockStateBase$Cache.<init>` — building the collision-shape cache for every one of
~20k block states.

**`Environment:` -> recipes (13 s, 13x worse than java).** Datapacks, registries, codecs. This is
where the 8-millisecond DataFixerUpper key renders lived (see
`map-key-derivation-is-13-percent.md`); removing them took 4.3 s out of this phase.

## The parallelism gap is not what it looks like

rava runs the boot at **1.05 cores**, java at **3.2**. That looks like a 3x parallelism deficit,
but java's extra cores are its JIT compiler threads, not guest work: `java -Xint` boots the same
server at 1.22 cores. Compilation is ~4.4% of our CPU, so matching java's thread count would buy
at most that.

The honest statement is therefore about work, not threads: **46 CPU-s against java's ~8 mutator
CPU-seconds — roughly 5.8x the work per boot** — and that work is flat (host profile top function
4.4%).

## Why the biggest phase does not fork

The engine has a whole per-element fork machinery (`RAVA_FORK_SHAPES`, `RAVA_FORK_PARALLEL`,
`try_fork_each_invoke`, `method_offloadable_forked` with its element-ownership rule written
specifically for "`BlockStateBase.initCache()`-style methods"). Enabled on a boot it changes
nothing — **1.05 cores either way** — and its own census never even mentions `initCache`.

`javap` says why, and it is semantic rather than an oversight. `initCache` is not reached through
`List.forEach`; it is called from a plain loop in `Blocks.<clinit>`:

```
40081: invokevirtual net/minecraft/core/IdMapper.add:(Ljava/lang/Object;)V
40085: invokevirtual net/minecraft/world/level/block/state/BlockState.initCache:()V
```

The loop body does two things per element: `initCache`, which is per-element private and forkable,
and `IdMapper.add`, which appends to a SHARED registry and is not. A fork of this loop would have
to split the body, which changes the program rather than scheduling it.

## And `memchr` is not regex

`memchr` sits at 3-6% of every time slice of the boot, and the `memchr` crate arrives through
`regex`, so regex was the obvious suspect. Caller attribution (`bench/boot_pc.ps1 -Stacks memchr`,
which needed a new switch — the harness cleared `RAVA_PROFILE_PC_STACKS` by design, and that is
why the question returned silence for so long) says otherwise:

```
15.8%  owner_bits::bits                 <- native dispatch plan, keyed by class NAME
 8.8%  ClassFile::method                <- method by name+descriptor
 7.5%  ClassLoader::super_chain
 7.5%  heap::intern::field_id_existing
 6.5%  tlcache::inline_get     6.0% resolve_get     5.8% Heap::field_id
 5.8%  argcall::arg_layout     3.5% Heap::intern    3.3% get_or_load
```

Every one of them is a **string-keyed lookup on a class, method or field name**. That is the third
of the three standing causes, and it is as flat as the rest: the largest single caller is 15.8% of
3.5% = **0.55% of the boot**. `ClassFile::method` is already a hash map plus a two-element linear
scan over overloads, so there is no local fix — the cure is to stop keying by strings, which is a
representation change, not an optimisation.


## The seam, priced callee by callee — and what that bought

`bench/SeamCost.java` prices the JIT-callback census's top rows against HotSpot. The numbers are
why the seam is 11% of the boot:

```
                       java     rava (before)
  Class.cast            0 ns     1050 ns
  Boolean.booleanValue  0 ns     1155 ns
  BitSet.clear          0 ns       45 ns     (already emitted inline by the JIT)
  BitSet.nextClearBit   0 ns     1525 ns
  R2OMap entry-iter    15 ns     7980 ns
```

A one-field unbox at 1,155 ns — against this engine's own `Map.get` at 98 ns — is not the
dispatch gauntlet: with `RAVA_DISPATCH_GATE` on, the probe barely moves. It is the **distance to
the arm**: `cascade_table` for the boxes, `reflect::dispatch` for `cast`. The engine already
shortcuts exactly this for `Map$Entry.getKey/getValue`, `Optional.of` and native
`Iterator.hasNext/next`; three more were added.

```
  Class.cast          1050 -> 700 ns
  Boolean.boolValue   1155 -> 480 ns
  R2OMap entry-iter   7980 -> 7300 ns
```

⚠**The first `cast` arm never executed.** `dispatch_owner_cascade` opens with `try_last_arm`, a
per-thread memo of the arm that served the previous native call, and a loop calling one native
repeatedly hits it every time — so `cast` kept being served from `reflect::dispatch` and the probe
read the same 735 ns with the fast path ON and OFF. **A shortcut placed after a shortcut to the
old arm is not a shortcut.**

⭐And the iterator arm was declining fastutil **on a name**: its condition was
`matches!(owner, "java/util/Iterator" | "java/util/ListIterator")`, while `ObjectSet.iterator`
plus `reference2ObjectEntrySet` are 9.46% of all crossings. It now admits by the RECEIVER's shape
(does it carry our synthetic `$elems` + `$pos`), which is a property of the object and needs no
name list.

Paired boot measurement, 6 rounds per arm, one session:

```
fast paths OFF (SET = old)  SET +3.90% min / +8.87% median
null test                   SET +3.77% min / +2.73% median
difference (new faster by)      +0.13 pt   / +6.14 pt
```

against a counted expectation of ~8.6% from the census populations.

⭐**The residue is now named rather than flat**: iterating a small map costs 7,300 ns against
HotSpot's 15, and the cost is NOT `hasNext`/`next` (widening that arm bought 8%) — it is the two
setup crossings, `entrySet` and `iterator`, which build views, plus `getKey` per element.


### How much of that reached the wall — measured, not assumed

```
                       start    now    java
  Class.cast           1255      700     0 ns
  Boolean.booleanValue 1365      270     0 ns    4.4x on the operation
  BitSet.nextClearBit  2165      325   0-4 ns    5.5x
  R2OMap entry-iter    8180     5140    15 ns    1.6x
```

On the wall, far less:

```
worldgen, 8 rounds, floor +/-1.2%   old code +1.36%   (16/16 runs bit-exact)
boot, paired against a null test    UNRESOLVABLE — the null's own median moved from
                                    +2.73% to +6.90% between consecutive runs
```

⚠**So the seam fast paths are worth about 1.4% on worldgen and cannot be resolved on the boot**,
against operation-level speedups of 3-5x. Two paired boot runs on this stack disagreed in SIGN,
and the reason is visible in the null tests themselves rather than in the change.

That gap between the operation and the wall is the recurring lesson of this work and it is not a
measurement failure: a 4x on an operation that is 4.78% of crossings, each ~1 us, is ~2 s of
counted work — and counted work has repeatedly failed to appear on a 40-second wall whose profile
has no peak above 4.4%. The instrument that can see 1.2% says 1.4%. That is the honest number.


## Where the session ended

```
              rava (min of 3)      java
  wall             41.1 s          7.2 s     5.7x
  cpu              43.1 s         23.5 s     1.8x
  Done            0.395 s        0.210 s     1.9x
  cores             1.05           3.3
```

The seam fast paths, measured on the instrument that can see 1.2%: **worldgen +2.49% for the old
code**, up from +1.36% before the last batch (`Objects.equals`, `DoubleArrayList.wrap`,
`BitSet`, the fastutil entry view).

Operation-level, this session's arc:

```
                        start     now     java
  Map.get                 588      98      3 ns    (identity key rule)
  Map.containsKey         514      95      3 ns
  Objects.equals         1620     340      2 ns
  DoubleArrayList.wrap   3410     600      1 ns
  BitSet.nextClearBit    2165     325    0-4 ns
  Boolean.booleanValue   1365     270      0 ns
  Class.cast             1255     700      0 ns
  R2OMap entry-iter      8180    5140     15 ns
```

## The arithmetic that now closes

64,088,346 compiled-to-host crossings at the costs above IS the wall: at ~700 ns average that is
44.9 seconds, and the boot is 41-47. **The seam is not a component of the boot; it is the boot.**
That is why the flat host profile never located anything — the cost is not in a function, it is in
a population, and the population is crossings.

It also explains why operation speedups of 3-5x buy 2.5%: the top-20 callees are ~70% of the
population, and this session moved perhaps a third of them. The remaining rows are named and
priced (`Function.apply` 1,050 ns at 4.49%, `StringBuilder.append` 5,160 at 1.83%, `List.add`
3,570, `Set.iterator` 5,760, `String.valueOf` 380 at 3.77%, `identityHashCode` 810 at 1.21%),
and each is the same shape of work as the ones already done.


## Calibrate against java, not against the clock

The box moved a lot across this session — java itself booted in 7.0 s at one point and 8.1 s at
another, a 16% swing — so an absolute rava number means little. The RATIO does:

```
                rava      java     ratio
  session start  ~47 s     7.2 s    6.5x
  mid-session     37.3     7.0      5.3x
  end             42.1     8.1      5.2x
```

Done time over the same span: 0.43 / 0.202 = 2.1x at the start, **0.422 / 0.250 = 1.69x** at the
end.

## The last mechanism found, and the one still open

`fu_routable` was consulted only by the `invokevirtual` and `invokedynamic` emitters — so every
STATIC admitted to the machine-code fast path was dead code, and an arm for `Objects.equals`
measured exactly nothing until the `invokestatic` emitter asked the same question AND was placed
above `emit_direct_mono`, which is default-on and claims the site first. That is the second time
in this file's history that a shortcut placed after an existing claimant measured as "no effect"
rather than as "not reached"; the first was `try_last_arm` and `Class.cast`.

  Objects.equals  1,620 -> **50 ns**  (HotSpot: 2)

⚠Priced at **+0.20% on worldgen** (floor 1.2%), which is what a 3.35%-of-crossings callee looks
like on a workload that barely calls it. The operation number is same-binary and reproducible;
the wall number is honestly small.

Still open, named and priced: `Function.apply` at 840 ns and 5.37% of crossings (a lambda
invocation — the direct-call cache already admits lambdas, so the cost is elsewhere),
`ObjectSet.iterator` + `entrySet` at 8.4% (the two view constructions, not `hasNext`/`next`),
`String.valueOf` 380 ns at 3.26%, `LoadingCache.getUnchecked` 2.34%.


## `Function.apply` explained, and why it is not the same kind of work

The largest remaining callee (5.37% of crossings, 750 ns against HotSpot's 1) was probed to a
complete explanation rather than optimised by guess:

- **Not the argument build.** `RAVA_LAMBDA_STAT` reports 4,100,000 applies in one probe run with
  `captured_values=0` and **`argvec_allocs=0`** — already allocation-free.
- **Not the method resolution.** `invoke_lambda_parts` calls `resolve_method` on every apply, which
  looked like the field-id pattern that paid 5.7x on `DoubleArrayList.wrap`. A memo keyed on
  (site address, owner pointer) under the loader epoch measured **neutral** and was reverted.
- **It is the guest call itself.** What is left is `self.invoke(decl, ...)` — a real framed call
  into the lambda body. This tree already prices a framed call at 331 ns against `java -Xint`'s
  17.4, so crossing (~300) + framed call (~330) + return marshalling ≈ 750 is the whole number.

⇒ `Function.apply` is not another route to shorten. It is the **frame**, which is the contiguous-
value-stack line (`docs/contiguous-stack-plan.md`) — priced there at ~5.4% for stages 2+3 and
explicitly unstartable piece by piece.

Two more facts from the same probe, both worth not re-deriving:

```
[PIC-COV]     native/java-served (no fptr ever)   90,239 (89% of refusals)
[HEAPPIC-LIVE] leaf heap-Ref PIC: cache fills=0  never-cacheable=5,775,000
```

The heap-Ref PIC fills **nothing** — not because it is broken but because 89% of what reaches it
resolves to a Rust native, which has no compiled entry to cache. Every inline-cache line in this
engine is bounded by the same fact.
