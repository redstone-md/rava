# java.util as guest bytecode — the mechanism works, and the blocker is exact

*2026-08-26*

## The finding that started it

`bench/GuestColl.java` runs the same array-backed list twice in one process — once as our Rust
native `java.util.ArrayList`, once as ordinary guest bytecode compiled by our own JIT — with the
checksums asserted equal:

| | native (our Rust) | guest bytecode (our JIT) | |
|---|---|---|---|
| list | **227** ns/element | **45** | **5.0x** |
| map | **148** ns/lookup | **65** | 2.3x |

On HotSpot the two list arms are **identical at 1 ns**, so there is no algorithmic advantage to
normalise away. The 5x is purely what our engine does to the same work depending on which side
of the native boundary it runs on.

⚠The map arm was WRONG on its first run and the correction is instructive: keyed on `Object` it
called `hashCode`/`equals` per probe — both natives — so it measured the seam a second time and
read **16x SLOWER**. It also read 3.75x slower on HotSpot, which is what gave it away. Keyed on
`int` it makes no native call at all and reads 2.3x faster.

## Why this is worth more than the seam

Removing seam crossings has been measured and it does not pay: seven million fewer host
excursions moved a 48 s boot by less than the floor, and inlining `List.get` — the single
largest seam callee at 10.37% of 13.5M callbacks — measured **+1.68%** and **+3.90%**.

The cost is not the crossing. It is what the native does once it is there, and the handle census
says exactly what that is: **6.23M handle births per boot, 96% of them collection wrappers**
(`Map$Entry` 48.1%, `Iterator` 15.7%, `ArrayList` 11.5%, `Optional` 8.3%, `Stream` 6.4%,
`DoubleArrayList` 6.2%). Those handles are what drive the host categories that dominate a boot:

```
JIT+seam 11.4% · heap/arena/intern 8.3% · interpreter 8.1% · allocator 5.9%
Value repr 5.5% · mem/str 4.5% · GC 4.5% · natives dispatch 3.4% · hash/maps 2.3%
```

A list written as guest bytecode allocates a plain `Object[]` in the arena and **creates no
handle at all**, so it does not feed heap/intern, the allocator, the GC, or `Value` marshalling.
That is a target of roughly **24% of the boot**, against the seam's 6.6%.

## Two engine facts, probed before writing anything

1. **The class loader already prefers classpath bytecode over the native of the same name.** A
   deliberately wrong `java/util/ArrayList` whose `size()` returns `12345` was observed returning
   12345. No loader change is required.
2. **A classpath bytecode class may implement a NATIVE interface and be used through it.**
   `java/util/RavaIt implements Iterator` linked and ran (`sum=3`).

`javac --patch-module java.base=jdk` compiles into `java.util` without complaint.

## What was built

`jdk/java/util/ArrayList.java` — a complete `ArrayList` implementing `List`, `RandomAccess`,
`Cloneable`, `Serializable`, with its own `ListIterator`. Verified against HotSpot on a
self-contained exercise (add/get/set/remove/indexOf/copy-ctor/equals/hashCode/for-each/iterator
removal/sort/subList/toArray/toString):

**all nine output lines byte-identical to `java`.**

## The blocker, exactly

Booting MC with `jdk-out` ahead of the classpath fails in 0.6 s:

```
rava: uncaught java/lang/IllegalArgumentException: need at least one option
```

and the probe names the cause in one line:

```
java.util.Arrays.asList("a","b","c").size() == 0
```

**Our natives fabricate instances of `java/util/ArrayList` themselves** — `vm.heap.alloc("java/util/ArrayList")`
— and populate them through `$elems`. With the class resolving to bytecode, `size()` is now our
guest method reading field `n`, which the native never wrote. Two representations of one class
name, and the guest one wins the method lookup while the native one wins the construction.

`java/util/ArrayList` is named at **46 sites in 27 files, of which 15 are constructions**:
`map/methods/mod.rs`, `map/mod.rs`, `nio/dispatch.rs`, `nio/files.rs`, `reflect/concurrent.rs`
(x2), `reflect/dispatch.rs`, `reflect/functional.rs` (x2), `stream/mod.rs` (x2), and others.

So `java.util` in this engine is not a leaf — it is a web. Migrating a collection pulls every
native that PRODUCES one: `Arrays.asList`, `List.of`, `Collections.*`, the stream collectors,
`Files.list`, the map views.

## What that means for staging

The migration is viable and its payoff is measured, but it cannot be done one class at a time in
isolation. It has to move a coherent block — the collection classes together with the natives
that produce them — and each producer that becomes bytecode stops fabricating handles, which is
where the win is.

Order that follows from the census rather than from taste:

1. `ArrayList` + its producers (`Arrays.asList`, `List.of`, stream `toList`/`collect`,
   `Files.list`). 11.5% of handle births plus most of `Iterator`'s 15.7%.
2. `HashMap` + `Map$Entry` + the entry-set views — **48.1% of handle births**, the largest single
   population, and the one the arena-map work already circles.
3. `Optional` (8.3%) and `Stream` (6.4%), which are almost pure wrappers.

Each step is measurable on its own with the existing boot A/B, and every step that lands removes
handles rather than moving them.

## What the override actually costs and buys, measured

Four `java/`-keyed refusals had to learn about the override. Each was one line, and each was
worth a multiple on the same benchmark (a list index loop, concrete-typed):

| after fixing | ns/element |
|---|---|
| nothing (bytecode never ran — `$elems` handed the methods back to the natives) | 203 |
| `native_served_namespace` declines an overridden class | 3,934 |
| arena layout interns `$`-names bare (`PHASE5: arena putfield of non-layout field id=0`) | — |
| `dispatch_resolve_cacheable_cls` stops marking it NeverCacheable | 1,931 |
| `pic_fill` + `jit_direct_resolve` stop bailing on it | **372** |

And the bisect that says the name no longer costs anything: the SAME class compiled into
`rava.util` instead of `java.util` reads **146 vs 146 ns**.

Then the number that matters, because MC reaches lists through the interface — `java/util/List.get`
is 10.37% of all 13.5M seam callbacks and every one of them is an `invokeinterface`:

```
loop over a List-typed variable, same process, same run
  native  java.util.ArrayList   1707 ns/element
  guest   rava.util.ArrayList    140 ns/element     = 12.2x
  (java)                           7  /  1
```

With the override in place, the `java.util.ArrayList` arm becomes the 146 ns one. **Overriding
the native with bytecode is worth 12x on the shape the real workload uses.**

## Where it stands, and the one thing left

MC now boots much further and dies on missing `Collection` DEFAULT methods rather than on
anything structural — `java.util.List` declares `getFirst`/`toArray(IntFunction)` as defaults, so
javac never asks for them, but our `java/util/List` is a NATIVE interface and hands out no
defaults. Each one is a method to add; `getFirst`/`getLast`/`addFirst`/`addLast`/`removeFirst`/
`removeLast`/`reversed`/`toArray(IntFunction)` are in now.

The blocker for a real measurement is different and known: **`add` is O(n)**. The natives keep no
size field — an `$elems` array's LENGTH is the size, and they get amortised growth for free
because that array is a Rust `Vec`. A Java array cannot grow, so a guest `add` must reallocate,
and a boot that builds registries element by element went to 166 s.

The fix is an explicit size in the shared convention: an optional `$size` field, with the native
readers respecting it when present. `iterate_object` is documented as "the one place every drain
of an `$elems`-backed collection passes through", which makes it the first stop, and the same
file counts ~69 direct readers of the field behind it.

## The convention, after three defects that only a boot could find

The shared shape is now `$elems` (may hold spare capacity) + `$size` (logical length) + `$init`
(has a constructor of the overriding class run?), read by natives through `Heap::elems_len`.

Three things had to be true, and each was learned by breaking the boot:

1. **`$init`, not a zero test.** A DECLARED `$size` exists in the layout of every instance of
   that class — including the ones natives fabricate and never write, where it reads a default 0.
   Trusting it there reported every native-built list as EMPTY and the boot died with
   `ConcurrentModificationException`.
2. **Natives that REPLACE `$elems` must maintain the length.** They mutate collections by storing
   a new array through the name-keyed setter and never touch `$size`; a guest class that had
   adopted the old array reported the old length (`IndexOutOfBoundsException: Index 0 out of
   bounds for length 0` in the datapack load). `Heap::put_field` now keeps `$size`/`$init` in step.
3. **And that maintenance must be scoped to the OVERRIDDEN CLASS.** Every `$elems` collection
   shares the field — sets, iterators, stream results — and several are grown IN PLACE by natives
   (`array_push`, `coll_op`'s O(1) arms) without touching `$size`. Maintaining it for those would
   leave it stale below the array's real length and they would silently lose elements.

`add` is amortised and entirely in bytecode. Two earlier forms are on the record: reallocating
per append is O(n) (boot 40 s -> 166 s), and calling a native that pushes onto the backing Rust
`Vec` in place is flat but costs **593 ns/add** — the seam this migration exists to remove, paid
once per element.

## What the natives assume, and what the override breaks

The remaining work has one shape, and `Collections.unmodifiableList` shows it exactly:

```rust
let Value::Ref(sh) = src else { return src.clone() };          // "ours ⇒ handle-born"
let synthetic = matches!(vm.resolve_method(&cls, "size", "()I"), Ok(None));  // "ours ⇒ no bytecode"
if !synthetic { return src.clone(); }
```

Both guards encode *"a class of ours has no bytecode"*, and an overridden class violates both: it
is arena-born and it does have a `size()I`. So `unmodifiableList(guestList)` hands back the source
unchanged and the list stays mutable.

`immutable::mark` had the same assumption and is fixed — it now stamps `Value::Obj` as well, so
`List.of` / `Arrays.asList` / `List.copyOf` all refuse correctly. The guest class reads the
natives' own `$immutable` field and implements all six contract kinds, including the conditional
`removeIf` the singletons and fixed-size kinds need.

Measured against `java` on `ImmProbe`, **4 of 5 lines now agree** (`List.of.add` UOE,
`Arrays.asList.add` UOE, `List.copyOf.add` UOE, `Arrays.asList.set` allowed, `equals`/`hashCode`
agree); `unmodifiableList` is the one still wrong, for the reason above.

⛔Ignoring the contract is not a slower `ArrayList`, it is a WRONG one: before this,
`List.of("a","b").add("c")` silently succeeded, so `equals` answered false and MC refused its own
data pack — *"Found new data pack vanilla, but can't load it due to missing features
minecraft:vanilla"*.

## Where the boot stands

With the override on, MC reaches the datapack load and stops there. Default (override off) is
**wall 40.6 / cpu 43.5 / Done 0.391**, 4 gates bit-exact, 20/20 probes.

The next step is not a mystery: revisit the natives whose fast paths test *"is this one of ours"*
by asking whether the class has bytecode. `unmodifiable_view` is the first; the census of
`$elems` readers (21 sites in 13 files) bounds how many more there can be.

## MC boots on it — and the trade-off that remains is exact

With the override on, MC completes: **`Done (0.396s)`, wall 38.6 / cpu 41.2**, against a default
boot of 40.0-40.6 / 42.7-43.5 / `Done 0.39-0.42`. So the migration is **correct enough to run the
product and performance-neutral at boot** — not yet a win.

Getting there took four more fixes, each the same assumption wearing a different hat:

| what broke | the assumption |
|---|---|
| `new LinkedList<>(guestList)` came back `[]` | `if let Some(Value::Ref(_)) = args.get(1)` — a copy source must be a handle |
| `elems_of` returned nothing for an arena source | every arm tested `Value::Ref` |
| `subList` was a copy, so writes did not reach the parent | — (guest-side; now a real `Sub` view) |
| `for (x : list) list.add(y)` never terminated | no `modCount`; it grew until a **1 GiB allocation failed** |

The last one is worth naming separately: a missing `ConcurrentModificationException` is not a
cosmetic divergence, it is a hang.

### The trade-off, measured from both sides

An overridden class can be born in the arena or as a handle, and **neither choice is currently
correct**:

| birth | MC boot | `Collections.reverse`/`swap`/`rotate`/`fill`/`addAll`, `unmodifiableList`, `LinkedList.equals` |
|---|---|---|
| **arena** (current) | **boots**, `Done 0.396` | broken — 12 divergent lines in `ArraysSem` |
| handle | **dies at 0.6 s** | correct |

Arena birth is also where the prize is: the handle census says 96% of a boot's 6.23M handle
births are collection wrappers, and an arena-born collection creates none. Handle birth forfeits
exactly that and buys only the method-execution speed, which the seam census prices at ~1.5% of
the boot.

So the remaining work is not a choice between the two — it is to make the collection natives
**representation-agnostic**, so arena birth costs nothing. The probes name the population
precisely: `ArraysSem` (12 lines), `MapViewSem` (24), `IterSem` (truncated at
`Arrays.spliterator(Object[],int,int)`, unimplemented), `GuestListSem` (6).

## The migration has its own gate now

`bench/sem_all.ps1` takes `-Prefix`, so the whole differential suite runs against an overridden
classpath:

```
bench\sem_all.ps1                                  # default: 4 gates + 21/21 probes green
bench\sem_all.ps1 -Prefix D:\code
ava\jdk-out     # override: 4 of 21 divergent
```

⚠And it is needed, because **neither the gates nor the probes catch what the boot catches**: the
override registry claiming guava took the boot from 40 s to a 400 s timeout across three builds
while all of them stayed green.

`bench/GuestListSem.java` is new and covers what the other 20 do not — views, identity, fail-fast,
what the factories hand back — with the engine's pre-existing factory-class divergences declared.

## And then it was priced, and it does not pay

The line was carried all the way to a working implementation before being measured against the
product, which is the only way this question could be answered honestly. The answer:

| | |
|---|---|
| list loop through the `List` interface | **1707 -> 140 ns/element (12.2x)** |
| handle births per boot | **5,343,863 -> 4,777,611 (-10.6%)** |
| **product boot, interleaved A/B, 3 rounds** | **+8.46% CPU, not disjoint** |

```
r1: DEFAULT cpu=40.2  OVERRIDE cpu=46.7
r2: DEFAULT cpu=45.0  OVERRIDE cpu=43.7
r3: DEFAULT cpu=43.7  OVERRIDE cpu=43.6
```

So a 12x win on the operation, and half a million fewer handle births, move a 40-second boot by
nothing. **The estimate that this line was worth ~24% of the boot is refuted.** It rested on
adding up the host categories that handles feed — heap/intern, allocator, GC, `Value` — and
assuming a proportional share falls out when the handles do. It does not.

That makes this the fourth independent mechanism this session to be built or measured to
completion and priced at the floor:

* parallel bytecode execution — refuted, MC's boot is a serial program (`java -Xint` = 1.22 cores)
* `Value` becoming `Copy` — deletes the profile's hottest function outright, **-2.10%**
* inlining the largest seam callee (`List.get`, 10.37% of 13.5M callbacks) — **+1.68% / +3.90%**
* `java.util` as guest bytecode — 12x on the operation, **+8.46% on the boot**

The common finding is not that each idea was bad. It is that **this boot has no concentration to
attack**: by function the top host row is 4.3%, by category the top is 11.4%, by seam callee the
top is 10.37% of a 14% seam, and removing any one of them entirely leaves the rest untouched.

## What is left of the line

The implementation stays in the tree, opt-in, because it is correct enough to run the product and
because it is the only working example of a `java.util` class executing as guest bytecode:

* MC boots on it (`Done 0.396-0.42`).
* 4 of 21 probes still diverge under the override, all with one cause — natives written against
  `Value::Ref`. `ArraysSem` is down from 12 divergent lines to 2 after `Collections.*` learned to
  write through a `Value`; `MapViewSem`'s 24 are map views, which the natives build as instances
  of the overridden class and then cannot keep live.

If it is ever resumed, it should be for a workload where collection operations actually dominate
— which MC's boot, measured, is not.

## Status

`jdk/java/util/ArrayList.java` is in the tree; `jdk-out/` is gitignored and on **no default
classpath**. Every engine hook is inert unless a `java/` classfile has been loaded. Default path
verified throughout: 4 gates bit-exact, **21/21 probes**, boot `wall 37.2-42.3 / cpu 40.2-45.0 /
Done 0.373-0.402`.

## 2026-08-31 — RETESTED WITH THE REPRESENTATION FIX, AND IT IS WORSE, NOT BETTER

The verdict above rested on a representation cost: guest bytecode beat our own native by 5-12x on
the operation, and the boot still paid **+8.46%**, because every collection became a handle birth.
`RAVA_ARENA_MAPS` is the cure for exactly that cost, and **it stopped being expensive**: it was
+48% on the boot when this document was written, and today it measures **+2.80% median / +1.54%
min, not disjoint**, with 5/5 gates bit-exact and **21/21 differential probes matching
`java -Xint`**. So the one combination the arc never tried became testable: guest `java.util` ON
TOP OF the arena representation.

`javac --patch-module java.base=jdk -d jdk-out jdk/java/util/{ArrayList,Objects}.java`, the classes
copied into `out/` (already first on the classpath), three product boots back to back on one box:

| configuration | CPU | Done | peak RSS |
|---|---|---|---|
| guest `ArrayList` + `RAVA_ARENA_MAPS` | **44.1 s** | 0.566 | 1012 MB |
| `RAVA_ARENA_MAPS` alone | 37.1 s | 0.490 | 1179 MB |
| default | 37.2 s | 0.446 | 1301 MB |

⇒ **+18.9% CPU — worse than the +8.46% this document recorded, not better.** The representation was
not what the guest path was paying for, so fixing the representation did not help it. The line is
closed a second time, now with its most favourable precondition satisfied.

⭐Two facts worth keeping out of the wreckage:

* **`RAVA_ARENA_MAPS` is no longer a regression.** +48% → within noise, and it takes 122 MB off
  peak RSS (1301 → 1179). The precondition the ledger names for removing the handle heap is met;
  what that unblocks is priced separately at ~1.19% (`RAVA_DIAG=price`), which is its own answer.
* **Guest `ArrayList` takes another 167 MB off RSS** (1179 → 1012) while costing 19% CPU. Footprint
  and speed move in opposite directions here, so RSS is not a proxy for the work.
