# Pure-call memo — rule, soundness, and the measured admission wall

`RAVA_PURE_MEMO` / `jit.pure_memo`, default OFF. Census arm: `RAVA_PURE_MEMO_PROBE`,
which runs the identical admission and interception but serves nothing, so the numbers
below can be read without the memo changing a result.

## The lever

The boot's `Done` window is ~103.5 s, of which the stronghold-ring phase is ~92.6 s
(89%) and is pure compiled-code throughput. A prior census showed the density-function
router is a DAG of 2,153 objects with 238 nodes shared between parents, walked once per
INCOMING EDGE — so of the 56 `DensityFunction.compute` calls in one
`Climate$Sampler.sample`, only 15 produce distinct results and 41 (73%) are exact
repeats. A Java-level shared memo over that sampler measured 3.0× on the sample loop and
1.96× on `findBiomeHorizontal`, i.e. a ceiling of about −45 s on the window.

A memo REMOVES work rather than duplicating a body, so it is on the right side of the
tree's transfer rule. It still adds an entry check, so the admitted set must stay small.

## The rule

Everything is derived from bytecode — effects, descriptor, size, leafness. No class,
method or package name is consulted anywhere (no-adapt).

A concrete dispatch `(class, name, desc)` is:

* **`Memo`** — the existing transitive escape analysis (`Vm::is_offloadable_devirt`)
  accepts the whole call subtree, the return is PRIMITIVE, and receiver + argument local
  slots fit one key (12 cells).
* **`Opener`** — accepted by the same proof but its result is not replayable
  (reference/void return, or too wide a key). It may still open the window its callees
  memoize inside.
* **`No`** — otherwise; also anything GC-leaf under 32 bytecodes, which cannot repay a
  fall into the host.

Both non-`No` verdicts make `dispatch_resolve_cacheable_cls` refuse to let the
machine-code PIC cache that (site, class), which routes the call through `jit_call`,
where the window lives.

## Soundness

### 1. A pointer key is not stable

The arena is a moving (Cheney) collector, and the handle table has free lists, so a raw
address or handle can be REUSED by a different object — a false hit returning a silently
wrong result that no bit-exact gate would catch. Every entry is stamped with
`(gc_epoch, handle_epoch)` and the entire cache is dropped the moment either moves; the
check is two relaxed loads on the lookup path.

Two gaps had to be closed for that stamp to mean anything:

* `nextgen::GC_EPOCH` is documented as exactly this signal but was bumped only by
  `tlab_collect`. `major_collect` swaps the whole tenured region — every tenured address
  changes — without bumping. Fixed; this also repairs `map.rs`'s existing VALKEY /
  hashCode memos, which read the same epoch.
* The handle heap had NO such counter. `heap::HANDLE_EPOCH` is new and is bumped in
  `retire_object_slot` and at the array/map sweep sites. A per-collection bump would not
  have sufficed: `release_obj_range` returns a whole handle range to the free list
  OUTSIDE any collection.

### 2. Static purity is not immutability — the FROZEN WINDOW

Proving a callee performs no store proves only that THIS call does not write. It says
nothing about who writes the receiver's reachable graph between two calls. Rather than
require deep immutability (which a `final` array field defeats — its ELEMENTS stay
mutable, and vanilla's `PerlinNoise` holds exactly such an `ImprovedNoise[]`), the memo
is confined to a window in which nothing relevant CAN be written:

* A **window** is the dynamic extent of an outermost admitted call. The escape analysis
  that admitted it rejects `putstatic`, monitors, unmodelled opcodes, non-`Pure` natives,
  and any `putfield`/array-store whose target is not an object allocated INSIDE the call
  — transitively. So for the whole window this thread writes nothing that already
  existed when the window opened.
* A window-BORN object can still be written by its allocator, so a KEY must not name one
  unless it is independently immutable. A key reference is admitted only when it is
  either (a) out of the nursery — tenuring happens only at a collection, which ends the
  window via the epoch stamp, so such an object necessarily pre-dates the window — or
  (b) a VALUE RECORD: every non-static field `final` AND primitive, so its reachable
  graph is itself and JVMS §6.5 confines `putfield` on a `final` instance field to that
  class's own `<init>`.
* Another thread writing is excluded by requiring no gil-free region to be open
  (`parallel::any_parallel()` / `region_count()`). Green threads are cooperative and a
  proven-pure subtree contains no switch point.

Together: everything a memoized call reads is bitwise invariant for the window, so
replaying the first result is exact.

**Identity, never equality.** Keys are the raw operand cells — a reference cell is the
address/handle, a primitive cell its bit pattern. Distinct-but-equal objects MISS
(conservative); `NaN` payloads and `-0.0` compare by bits.

**Only primitive returns are replayed.** Replaying a reference would alias two separate
allocations, which `==` and identity hash can observe.

**Recursion** is the point: a nested admitted call joins the open window and only the
outermost frame closes it. A frame stores on the way OUT, so a cycle can never observe a
partial entry and an exceptional exit stores nothing.

## Measured admission — the wall

`RAVA_PURE_MEMO_PROBE` on `bench/RingScan` (the standalone stronghold-ring workload):
**5,282 dispatched methods examined — 407 Memo, 645 Opener, 4,230 refused.**

One bug fell out and is fixed in its own commit: the escape analysis's array-load arm
omitted `baload` (0x33) although its comment claimed it, so every method reading a
`byte[]` was refused as an UNMODELLED opcode. That single omission refused the whole
vanilla noise leaf. With it modelled, `ImprovedNoise.noise(DDDDD)D` is admitted.

What is still refused, and why — every ring-hot method funnels into the same two chains:

| refused | chain to the real cause |
|---|---|
| `Ap2.compute`, `Mapped.compute`, `MulOrAdd.compute`, `ShiftedNoise.compute`, `Spline$Coordinate.apply`, `Climate$Sampler.sample`, `MultiNoiseBiomeSource.getNoiseBiome` | `DensityFunction.compute` **closure 13/20 impure** |
| `ShiftA.compute`, `ShiftB.compute`, `NoiseHolder.getValue`, `NormalNoise.getValue`, `PerlinNoise.getValue` | `DoubleList.getDouble` **closure 6/8 impure** → `DoubleArrayList.getDouble` → `StringBuilder.append` **closure 1007/1007 impure** |

### Blocker A — `DensityFunction.compute` is genuinely 13/20 impure

`DensityFunction` is an interface; the proof must range over every loaded implementor,
and 13 of the 20 write (`NoiseChunk`'s `Cache2D`/`FlatCache`/`CacheOnce`/… каждый
`this.lastPos = …`). Those wrappers are inert during the ring, but STATIC admission
cannot know that. This is not an analysis imprecision — it needs runtime
devirtualisation (see "What a follow-up needs").

### Blocker B — a cold exception path, through a megamorphic JDK closure

`DoubleArrayList.getDouble(int)` guards with
`throw new IndexOutOfBoundsException("Index (" + index + ") …")`. fastutil targets
Java 8, so that is a real `StringBuilder` chain, not the `makeConcatWithConstants` indy
the analysis already models. Two compounding imprecisions:

1. `StringBuilder.append` returns `this`, but the model pushes `F::Shared` for every
   return value, so the SECOND `append` in a fluent chain dispatches on a receiver whose
   freshness was lost — a shared-receiver query instead of the exact one that could have
   consulted the receiver-mutating native contract.
2. `java/lang/StringBuilder` is natively served (no bytecode), so `name_may_subtype` is
   inconclusive and the interface/closure fallback enumerates **1,007** candidate
   classes, all of which fail.

Both are fixable and generic (propagate receiver freshness through a `returns this`
native contract; tighten the bytecodeless-chain subtype test). Neither was attempted
here because Blocker A is decisive.

## Why fixing only Blocker B would still buy nothing

The prize is CROSS-ROOT sharing: `ShiftA` is called 12× per `sample` with the same
context but only ~2× within any single root. So the window must span
`Climate$Sampler.sample`, and `sample` is refused by Blocker A. With B fixed and A
standing, `ShiftA.compute` becomes `Memo` but every call is its own window, and a window
of one call has no repeats to serve.

## Measured verdict

`bench/wt_ringscan.ps1`, P-cores, High-Performance, 4 ring positions — the ring workload
itself, not a proxy:

| arm | SETUP (ms) | ring (ms) | HASH |
|---|---|---|---|
| OFF | 67,040 | 4,064 | `3ed21b4965` |
| ON  | 113,943 | 4,948 | `3ed21b4965` |

**Bit-exact** (identical hash), and ChunkFill under the flag reproduces the canonical
gate `8790f04ace726ecf`. And **refuted**: ring +21.7%, setup +70%.

The instrumentation says exactly why:

```
[PURE-MEMO] windows=14305125 lookups=97484 hits=0 (0.0%) stores=97484
```

14.3 M windows opened, 97,484 results recorded, **zero ever reused**. Windows count only
depth-0 scopes, so essentially nothing nested: the admitted set is leaf-ish pure methods
called from unadmitted code, each its own window, cleared on return. The flag pays 14.3 M
falls out of the machine-code PIC and buys nothing. Default OFF.

## Real boot — interleaved A/B

`bench/wt_boot_flag_ab.ps1 -Flag RAVA_PURE_MEMO -Seq off,on,off,on -Cores ecore`. Every
run listed; min-of-N, because this box's variance is placement, not distribution.

Round 1 ran while two other agents were booting and is shown only to be discarded — run
1 was contended and run 4 lost port 25599 to a foreign boot (`BindException`), which is
why the harness now RETRIES a bind failure instead of recording it as a result.

| round | run | cfg | Done (s) | wall (s) | note |
|---|---|---|---|---|---|
| 1 | 1 | off | 375.732 | 787.6 | contended |
| 1 | 2 | on  | 201.358 | 454.3 | |
| 1 | 3 | on  | 202.710 | 433.9 | |
| 1 | 4 | off | — | 175.8 | port lost, discarded |
| 2 | 1 | off | 206.847 | 374.6 | |
| 2 | 2 | on  | 200.053 | 437.7 | |
| 2 | 3 | off | 210.400 | 376.6 | |
| 2 | 4 | on  | 203.309 | 459.9 | |

Round 2, min-of-N:

| metric | OFF | ON | delta |
|---|---|---|---|
| `Done` window | 206.8 | 200.1 | **−3.3%** |
| process wall | 374.6 | 437.7 | **+16.8%** |
| pre-`Done` (wall − Done) | 167.8 | 237.6 | **+42%** |

The `Done` difference has no mechanism behind it — the memo serves 0 results — and sits
inside this box's demonstrated spread, so it is not claimed as a win. The wall
regression is large, reproduced on both ON runs, and mechanically explained below.

## Mechanism, read with the RIP sampler

The category sampler bills everything under `jit_run`'s guard to `JIT_EXEC`, including
host callbacks, so it cannot answer where this flag's time goes. `RAVA_PROFILE_PC`
samples the guest RIP and resolves it against compiled ranges or a host symbol.
RingScan, 4 positions, both arms:

| arm | SETUP (ms) | ring (ms) | host-side share of wall |
|---|---|---|---|
| OFF | 62,731 | 4,129 | 83.3% (314 samples) |
| ON  | 87,477 | 4,626 | **94.1% (2,623 samples)** |

The ON arm's top host symbols are not the memo — they are the ADMISSION PROOF:

```
 7.0%  195  memchr
 3.4%   95  <&str as Pattern>::is_contained_in
 3.2%   88  mi_malloc_aligned
 2.6%   73  ClassLoader::get_or_load
 2.5%   70  ClassLoader::super_chain
 2.3%   64  sip::Hasher::write
 2.2%   62  Vm::jit_call
```

`super_chain` + `get_or_load` + substring matching + the `String` traffic feeding them is
the dispatch-closure enumeration: for each `(site, class)` examined, the proof walks the
whole loaded class set testing subtyping by name. `jit_call` at 2.2% is the forced host
falls themselves. So the flag's cost is host work — the same axis the boot is bound by —
and it buys nothing, which is the worst possible shape for a lever here.

Two rules fall out, both about method:

1. **Size a memo by CALLS AVOIDED, and measure that first.** `hits` is that number
   directly, and it was 0 before any wall measurement was needed. A per-call cost model
   is irrelevant when the numerator is zero.
2. **Interception is not free in a host-bound runtime — it is the expensive direction.**
   Refusing a machine-code PIC fill converts a call into a host round-trip, the very cost
   a memo exists to remove. So the intercepted set must be almost exactly the set that
   HITS. Broad interception with a narrow hit rate is strictly negative, and the
   admission wall forced exactly that shape.

## What a follow-up needs

Admitting `sample` means DEFERRING the unprovable `DensityFunction.compute` callee to a
runtime check: accept it statically, intercept every dispatch of that signature, and
TAINT the window (clear the cache, suppress stores in all open frames) the moment one
resolves to an unadmitted target — before it runs. That is sound only if interception of
the deferred signature is TOTAL, and it is not, because a site whose (site, class) pair
was PIC-cached BEFORE the signature joined the deferred set is served entirely in
machine code and is never seen. Closing that needs a PIC-invalidation epoch — a way to
retract positive entries from the per-site 4-way caches and the append-only
freeze-resolve table. That, not the memo, is the real prerequisite.

And it needs a second thing the measurement above makes non-negotiable: the admission
proof must not run on the dispatch path. Today it enumerates the loaded class set by
name for every `(site, class)` it examines, and the RIP profile shows that enumeration —
not the memo — is where the ON arm's time went. A deferral increment that widens the
examined set without first making the proof cheap (or hoisting it off the dispatch path
entirely) will lose by a larger margin than this one did.
