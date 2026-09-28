# The boot's price map, measured on the boot

Every phase map in this ledger was taken on a boot that no longer exists — the current one is
~32-33 s, the older ones were 75 s, 112 s, 145 s. **Proportions from those maps do not transfer.**
This is the current one, and one of its numbers corrects a target chosen two commits earlier.

## Phases (`bench/BootPhaseC`, which runs only now that `PrintStream.flush` exists)

```
Bootstrap.bootStrap                              17,183 ms   52%
WorldLoader.load (datapacks/registries/recipes)  12,521 ms   38%
Bootstrap.validate                                   62 ms
PackRepository.reload                                21 ms
getSummary                                           13 ms
everything else                                    ~200 ms
```

Two phases are 90% of the boot.

**The ~10 s gap in the boot LOG between `Environment:` and "Failed to request yggdrasil public
key" is not a phase.** This ledger records it as never attributed, and plans have treated it as
engine time. Measured: `new YggdrasilAuthenticationService` **2 ms**, `Services.create` **2 ms**.
It is a logging artifact.

## Price (`boot_counts_live.ps1 -Groups price`, meters on the real boot)

| meter | events | p50 | est | share |
|---|---|---|---|---|
| `fu_fast` | 12,651,930 | 564 ns | 7.68 s | **23.3%** |
| `interp_self` | 4,278,649 | 564 ns | 3.20 s | **9.7%** |
| `native_lastarm` | 7,581,503 | 71 ns | 0.55 s | 1.7% |
| `native_map` | 3,858,948 | 58 ns | 0.36 s | 1.1% |
| `gson_build` + `_add` + `_new` | | | 0.40 s | **1.2%** |
| `gson_tokenize` | 6,297 | | 0.03 s | 0.08% |

## ⚠What this corrects

`gson_build` — guest-object materialisation, measured at **19.5× tokenisation** and named as the
next target — is **1.2% of the real boot**. `bench/DataPackJson` over-weights it badly: the probe
also runs `el.toString()` and an FNV fold over every document, and **the boot does neither**.

⇒ **A probe's layer split gives the ratio inside that probe. It does not give a share of the
boot.** The ratio was correct and the conclusion drawn from it was not.

The real target is the one the inline arms were already on: **`fu_fast` at 23.3%**, still the
largest single meter after the arm work cut its population by 70%. Second is interpretation at
9.7%.

## The gap to the JVM, measured today

Same box, same world, same classpath, same hour:

| | rava | java | ratio |
|---|---|---|---|
| wall | ~32-33 s | **15.4 s** | **2.1×** |
| `Done` | 0.420 s | **0.337 s** | **1.25×** |

Not the 4.7× / 1.9× carried forward from the session-opening summary (6.8 s / 0.219 s). Those
figures are stale, and quoting them overstated the gap by a factor of two.


## Where the 23.3% actually sits — and why this line ends here

`fu_fast` is 23.3% of the boot in AGGREGATE. Its composition on the real boot (FU-RECV, richest
dump, 3.23 M crossings sampled):

```
625,664  O:Reference2ObjectArrayMap#put     <- ARENA receiver
481,792  O:IndirectMerger#size              <- ARENA
437,504  R:LinkedHashMap#get
287,744  R:Class#cast
284,416  R:Map$Entry#getKey
184,576  R:Boolean#booleanValue
180,480  R:Map$Entry#getValue
126,208  R:DoubleArrayList#equals
 97,024  R:EnumMap#get
 89,600  O:DiscreteCubeMerger#size
 87,552  R:ArrayList#add
 84,480  O:IdenticalMerger#size
 75,008  R:CubePointRange#equals
```

**`getDouble` is gone from the top** — the inline arms serve it on the real boot, not just on the
probe. That was 58% of this population when the session started.

**What is left is a flat tail.** The largest family is ~625 k crossings; at the boot's measured
p50 of 564 ns that is **0.35 s = ~1% of the boot**, which is at the measurement floor. The three
merger `size` variants together (IndirectMerger + DiscreteCubeMerger + IdenticalMerger = 655 k)
come to the same ~1%.

So the 23.3% is real but it is spread across a dozen families of ~1% each. The concentration that
made `getDouble` worth an arm — one name at 58% of the population — has been taken, and no
successor to it exists.

⇒ **Getting the rest requires making a CROSSING cheaper (564 ns), not serving more methods.** That
is the native-ABI/dispatch item, not another inline arm.

Two further notes for whoever picks this up:

- The top two families are **arena receivers** (`O:`). Every arm built in this session guards on a
  heap-Ref class WORD; an arena receiver needs the `class_id` compare shape that
  `jit::emit::guarded_inline` already implements. A new arm here is not a copy of the existing
  ones.
- `DoubleArrayList#equals` (126 k) and `CubePointRange#equals` (75 k) are the same two classes the
  session's arms already cover, on a different method — and `equals` has exact primitive
  semantics (`a1[s] != a2[s]`: NaN != NaN, +0.0 == -0.0) that a wrong implementation would break
  silently.


## The boot by CATEGORY — 60.5% accounted, and one category is 38%

Grouping every price meter on the real boot:

```
native serving          38.3%   fu_fast 23.27 + native_stream 3.75 + native_services 3.07
                                + native_stages 3.01 + native_self 2.46 + native_lastarm 1.67
                                + native_map 1.10
interpretation           9.7%   interp_self
compiled->host seam      5.1%   seam_prologue_full 2.37 + direct_probe 1.91 + seam_epilogue 0.83
heap                     3.4%   field_read 2.12 + births_handle 1.26
JIT compiler             3.0%   compile_self  (1,762 compiles at 578 us each)
JSON build               1.0%   gson_build_new + gson_build_add
                        -----
                        60.5%
```

**Serving natives is 38% of the boot — four times interpretation, seven times the seam.** That is
the answer to "where does this engine's time go", and it is not where the ledger has been
looking: not the interpreter (9.7%), not the compiled/host boundary (5.1%), not compilation
(3.0%), not the heap (3.4%).

It also agrees with a much older measurement this ledger already had and did not follow:
`bench/NativeCost.java` priced our `Map.get` at **528 ns against java's 2** and `Objects.equals`
at **687 against 0**, and concluded "the ceiling is our java.util replacements, not the routing".
The boot-level number now says the same thing with a share attached.

⇒ **The engine is fast at running bytecode and slow at BEING the JDK.** Every remaining lever in
the ledger — dispatch, the seam, the frame, the interpreter loop — is smaller than this one, and
they have each been measured and closed. This has not been.


## The refusals are receiver mismatches, not name mismatches — tried and reverted

Of the 12,651,930 crossings, **2,752,707 (22%) are refused** (`fu_missed`), and `fu_op_other` —
the census bucket for names outside the classified list — is 2,441,472. Those two numbers sit so
close together that the obvious reading is "most refusals are methods no arm can serve, and they
pay the receiver unbox, `class_of` and six class-name compares before anything looks at the
method". So: refuse on the NAME first, before touching the receiver.

Built it, with a self-check — if the servable-name list were missing a name, `fu_served` would
fall. It did:

```
list derived by matching the arms (32 names)   fu_served 9,899,223 -> 8,944,977   fu_early_out 1,596,439
```

~954 k calls that HAD been served were refused. The list was missing six names, and one of them
was **`put`** — the single largest crossing family on the boot
(`Reference2ObjectArrayMap#put`, 625,664). ⇒ **a servable-name list must be checked against
`fu_served`, never trusted because it was derived mechanically.**

With `put`, `wrap`, `entrySet`, `getOrDefault`, `containsKey`, `ofNullable` added:

```
fu_served 9,871,307 (restored)      fu_early_out 70,721
```

**70,721, not 2.4 million.** So the premise was wrong: the refusals are NOT names no arm can
serve. They are servable names failing on their RECEIVER — `get` on a map shape the arm does not
handle, `size` on an unexpected class, and so on.

At 70 k x ~500 ns that saves ~35 ms, against a name match on all 12.6 M crossings at ~5 ns =
~63 ms. **A net loss, and reverted.**

⇒ For whoever attacks the 38%: the discriminator that would pay is the RECEIVER, not the method
name, and it has to be cheaper than the `class_of` + six-compare classification that runs today.


## What "make a crossing cheaper" would actually buy — priced before building

The refusal analysis above says the discriminator that matters is the RECEIVER. This file already
agrees with itself on that: the `_ =>` arm of the class match carries `FU_WHY[0]` and the note
"7.9 M refusals a boot, all of them a plain `Value::Ref` whose class is not one of the six it
admits — so the list, not the representation, is the limit."

The obvious cure is a cheaper receiver discriminator: today the classification calls `class_of`
and then compares the result against ~10 class-name string literals in sequence; the engine
already has an integer key for the same thing — the class WORD (the interned name's pointer),
which every inline arm guards on. A word→kind memo would replace those compares with one integer
compare.

Priced against 12,651,930 crossings on a 33 s boot:

```
save 20 ns per crossing -> 0.25 s = 0.77% of the boot   (3.5% of fu_fast)
save 30 ns             -> 0.38 s = 1.15%               (5.3%)
save 50 ns             -> 0.63 s = 1.92%               (8.9%)
```

Most of those literals differ in LENGTH, so a compare usually fails on the length check alone —
20-30 ns for the whole sequence is the realistic end. **So the cure is worth about 1% of the boot,
which is this instrument's floor.** Not built.

⚠And the more important caveat for whoever continues: **`fu_fast`'s 564 ns is self-time, and it is
not known to be dominated by overhead.** The other native meters (`native_stages`,
`native_lastarm`, `native_services`) have their own event counts and are a different path — the
interpreter's cascade — so they are not nested inside it. That means the 564 ns includes the
actual work of each operation: the array reads, the map lookups, the boxing. Attacking "the
crossing" generically assumes the opposite.

⇒ **The next step here is not a change, it is a decomposition of the 564 ns** into
classification / guard / operation. The file itself warns how to do it: "a meter's own bookkeeping
is charged to its parent, so do not nest one here" — a second guard around the unbox alone priced
that unbox at 5.3 ns and raised the parent's self time from 63.6 to 102.9. So the decomposition
has to be done with counters and separate runs, not by nesting meters.


## The decomposition — and a caveat on the 23.3% that must travel with it

`RAVA_FU_CLASSIFY_ONLY=1` returns immediately after `kind` is decided, so the meter times
classification alone. Separate runs, not nested meters:

```
normal          fu_fast  12,651,930 ev   p50 564.1 ns
classify-only   fu_fast  28,459,648 ev   p50 333.4 ns
```

**Classification is ~333 ns; the operation itself is the remaining ~231 ns.** So more than half a
crossing is spent deciding what it is. (The event count differs because with nothing served the
fast path is attempted far more often, so the population is not identical — the split is
indicative, not exact.)

### ⚠But both numbers are inflated by the instrument that produced them

`RAVA_DIAG=price` arms two blocks that run INSIDE the `fu_fast` span on EVERY crossing: the
per-op classification (`match name` plus an atomic increment) and the receiver-census gate. The
meter's own timing is sampled 1-in-256, but those counters are not.

Measured directly — median boot wall, same box, same hour:

```
with RAVA_DIAG=price   37.8 s
without                32.8 s      => the instrumentation costs 15.2% of the boot
```

⇒ **The 564 ns, the 333 ns and the 23.3% share are all UPPER BOUNDS.** They were taken in a
configuration that is 15% slower than the one being described, and part of that 15% lives inside
the span being measured.

This does not overturn the category result — serving natives is still the largest thing in the
boot by a wide margin, and the arms measured on the gate instrument (`RAVA_NO_DALIST_GET_INLINE`,
−2.99% disjoint) were measured WITHOUT `price` armed. But the exact share is not 23.3%, and
anyone planning against that number should re-derive it with a leaner instrument first.

⭐The general form of this, which this ledger has hit before with `RAVA_DIAG=exec` at 2.2x:
**an armed counter inside a hot span becomes part of what the span reports.**


## The class-word memo: built, measured, zero — and why that closes the loop

The decomposition said classification is 333 ns of a 564 ns crossing, so the cure it implied — an
O(1) class-word→kind memo replacing the `class_of` + ten-literal chain — should have been worth
far more than the ~1% first estimated from "20-30 ns of string compares".

Built it: direct-mapped 64-slot thread-local keyed by the interned class-name pointer, refusals
memoised too (that is the 7.9 M population), a miss falling through to the same string match which
then fills the slot. Correct — 4 bit-exact gates and 21 differential probes green, `fu_fast` event
count unchanged at 7,298,503 against 7,299,009, `DataPack_HASH` unchanged.

Same-binary flag A/B, `RAVA_NO_FU_KIND_MEMO`:

```
OFF min 22.00 med 22.19   ON min 21.83 med 22.36
-0.78% min / +0.77% median, NOT disjoint
```

**Zero. The arms straddle it.** Reverted.

⇒ **And that retroactively explains the 333 ns.** It was measured with `RAVA_DIAG=price` armed,
which runs a per-crossing `match name` plus an atomic INSIDE the span — the same instrumentation
priced at 15.2% of the boot two sections above. The string chain really is the 20-30 ns the first
estimate said, and the decomposition's headline number was mostly its own instrument.

**The first estimate was right and the more elaborate measurement was wrong.** Both were mine, and
the one that cost a build and an A/B to disprove is the one that looked more rigorous.

⇒ For this line the remaining conclusion stands unchanged: the 564 ns is not dominated by
dispatch overhead that a cheaper key can remove. It is the operations themselves — our java.util
and fastutil replacements — which is the same ceiling `bench/NativeCost.java` named long ago
(`Map.get` 528 ns against java's 2). That is a rewrite of the natives, not a fast path in front of
them.


## The per-operation ceiling, re-measured today — and it is ALLOCATION, not dispatch

`bench/NativeCost.java` on the current binary, ns per call:

```
                  rava    java
call static          0       0     <- parity
call virtual         3       0     <- parity
Objects.equals      38       0
List.size           42       0
List.get            45       0
Map.containsKey    166       1
Map.get            175       2
new Object         324       1
List.iterator      309       0
new AtomicLong     786       0
new ArrayList     1059       0
List.add(new)     1455       0
```

⚠The JVM's zeros are escape analysis deleting the allocations outright, so the ratios overstate.
The rava column is what matters, and it is an absolute number.

**Two things follow, and they point in opposite directions from where this ledger has been
looking.**

**1. Calling is at parity.** `call static` is 0 ns and `call virtual` is 3 ns — the same as the
JVM. Every lever this ledger has spent sessions on — dispatch, the seam, the frame, inline
caches, devirtualisation — is in front of a cost that no longer exists. The call is not the
category.

**2. Allocation is the ceiling.** `new Object` costs **324 ns**; a `new ArrayList` costs 1,059 and
an `add` of a fresh element 1,455. That is the same number the JSON work found from the other
end — `gson_build_new` at 2,256 ns per tree node, which is a constructor plus an allocation — and
the same one the allocation-leaf profile kept surfacing all session (`put_field_syn`,
`FieldBlob::push`, the blob sizing).

`Map.get` has also improved since the figure this ledger carries: **175 ns, not 528**.

⇒ **The engine is fast at running bytecode and fast at calling. It is slow at MAKING OBJECTS.**
That is where the remaining 2.1× lives, it is consistent with every independent instrument used
today, and it is the one category that has never been attacked directly.


## The handoff: allocation, with the arithmetic already done

`new Object` measures **324 ns** while the `births_arena` meter prices an arena birth at
**28.8 ns** and `births_handle` at 96.2. **So ~295 ns per allocation is outside the birth itself**,
and that is the largest unexplained per-operation number left in the engine.

Against the boot's 6,912,017 births (`births_handle` 4,005,790 + `births_arena` 2,906,227):

```
remove  30 ns/alloc -> 0.21 s = 0.63% of boot
remove  50 ns/alloc -> 0.35 s = 1.05%
remove 100 ns/alloc -> 0.69 s = 2.09%
if ALL ~295 ns were removable -> 2.04 s = 6.2% of boot
```

So this is the first target in a long time whose ceiling is **above** the measurement floor rather
than under it — and the only one where the per-operation number and the boot-level share agree.

Two candidates are visible by reading, and BOTH need measuring before either is built — the
lesson of the class-word memo, which was priced from an inflated number and delivered zero:

1. **The interpreted `new` fast path takes a lock.** `opcodes::alloc::new_object`'s cached-site
   arm does `vm.new_site_cache.read()` — an `RwLock` acquire and release — then a hash lookup on
   `(class ptr, cp index)`, then **two `Arc` clones** (two atomic increments) before it allocates
   anything. A per-allocation lock and three atomics is the shape this session has repeatedly
   found to be worth 1-3%.
2. **Which heap the object lands on.** `births_handle` is 4.0 M of the 6.9 M births and meters
   3.3x an arena birth. The ledger's standing verdict is that removing the handle heap needs
   arena collections first (`RAVA_ARENA_MAPS`, +48%), so this is the structural half.

⚠And note what the benchmark also says: `call static` 0 ns and `call virtual` 3 ns, **at parity
with the JVM**. Whatever is left is not in front of the call.


## Candidate 1 tried: the allocation lock is not the cost either

Replaced the `new` site cache's `RwLock` + hash with a thread-local direct-mapped cache — a
miss falls through to the shared map, so it can only ever answer with the right class. Correct:
4 bit-exact gates green.

`new Object` stayed at **324 ns, unchanged** — because that benchmark's loop COMPILES, and the
compiled `new` does not go through `opcodes::alloc::new_object` at all. **So the 295 ns lives in
the COMPILED allocation path, not the interpreted one.** Worth knowing before anyone else spends
a build on the interpreted side.

Measured anyway on the datapack workload, where interpretation is heavy:

```
n=3 pairs:  +1.82% min / +1.69% median   (disabling looks costly)
n=7 pairs:  -0.07% min / -1.12% median   (sign FLIPPED, pairs split 3-3-1)
```

**Zero.** The n=3 reading was noise, and the extra four pairs are the only reason it was not
shipped as a 1.7% win. Reverted.

⭐This is the third time today the same discipline paid: a result that does not survive more pairs
is not a result. The ledger already records a null test whose sign flipped between consecutive
runs; this is the same phenomenon inside a single A/B.

⇒ **The allocation target is real but it is in the COMPILED path.** The next attempt should start
by finding what compiled `new` emits for a fieldless class — an inline bump cannot cost 324 ns, so
either it is a callback or the bump is not being used.


## ⚠Correction: NativeCost's absolute numbers are window-local

The per-operation table above (`Map.get` 175, `new Object` 324) was taken in one window. Later in
the same session, on the SAME binary, three consecutive runs read:

```
Map.get     332  325  333        (was 175)
new Object  629  660  627        (was 324)
```

Tight within the window — ±5% — and roughly double the earlier level. That is the same box drift
already documented here at 1.8x on `release-diag`.

⇒ **The absolute ns figures are comparable only inside one window.** Two consequences, both mine:

- An attempt to cache the `new` path's two locked lookups together read `new Object` 324 -> 525
  and `new ArrayList` 983 -> 2272, and I called it a regression. **That attribution was unsound**
  — the reverted code then read 613 and 2614 on the same box. The change was reverted anyway, so
  nothing was lost, but the reasoning was wrong and would have been wrong in the other direction
  too.
- The RATIOS inside a single run are unaffected, and they are what the conclusion rests on:
  `call static` 0 ns and `call virtual` 3 ns against `new Object` at 324 *or* 629. **Calls are at
  parity and allocation is the ceiling either way.**

⭐The rule, for the third time today in a different disguise: **compare only within a window, and
prefer a same-binary A/B to any absolute number.** A microbenchmark that is stable across three
consecutive runs is still not stable across an hour.


## The allocation path: three attempts, three zeros, and what that leaves

The interpreted `new` really does take two locks and two hashes per allocation — `new_site_cache`
under an `RwLock` keyed by `(class ptr, cp index)`, then `class_field_layout` under a second
`RwLock` keyed by the class NAME — plus three `Arc` clones. All three attempts to remove that
measured nothing:

```
1. thread-local site cache (first lock only)     n=3  +1.7%   n=7  -1.1%   -> noise, reverted
2. both lookups, carrying the loader epoch       absolute numbers unusable (box drift) -> reverted
3. both lookups, no epoch (COMPLETE layouts only) n=7  -0.84% min / -0.14% med, not disjoint
```

Attempt 3 is the clean one: it removes both locks, both hashes and one `Arc` clone from every
interpreted allocation, it is correct (4 bit-exact gates and 21 differential probes green), and
it is worth **zero**. Reverted, like the others — nothing this tree cannot measure gets to stay.

⇒ **The locks are not the cost.** What is left inside an allocation is the work itself:
`alloc_interned` (mint the handle or bump the arena) and `prepopulate_layout` (size the field blob
and write one entry per declared field). Those are the two things this session already improved
from the other side — `FieldBlob::push` grew one slot at a time, and the blob asked for exact
bytes and wasted the allocator's rounding.

So the allocation ceiling is not a lookup in front of the allocator. It is what the allocator
does, and reducing it means changing what an object IS — which is the handle-heap rewrite this
ledger already names, and the one thing in it that has never been attempted.


## ⭐⭐The rewrite's precondition is no longer blocked

The ledger's standing verdict is that removing the handle heap — the change every other line in
this document ends up pointing at — cannot start until collections have an arena representation
without regress, and that `RAVA_ARENA_MAPS` was **+48% on the boot**, so "the whole risk is there;
if it does not work the rewrite does not begin".

That number is from a much older and roughly four-times-slower engine, and this document has
already shown twice that proportions from those measurements do not transfer. Re-tested today on
the current build:

```
correctness   4 bit-exact gates            OK
              21 differential probes       all match java -Xint
              full product boot            reaches Done

speed         gate A/B (DataPackJson)      +0.85% min / +0.69% median, NOT disjoint
              product boot                 wall 32.5 s, Done 0.425
                                           (default: 32.3-33.4 s, Done 0.417-0.434)
```

**Neutral, and correct, on every instrument available** — not +48%.

⚠What this does NOT prove: the A/B is one workload, and the boot cannot resolve anything under
its ±8% floor, so "neutral on the boot" means "not measurably worse", not "free". The +48% may
also have been real at the time and fixed since by unrelated work; nothing here identifies what
changed.

But the recorded blocker is not supported by any current evidence, and it is the single gate in
front of the one structural change this whole document keeps arriving at:

- calls are at parity with the JVM, so the call is not the category;
- the natives are 38% of the boot and their cost is the operations themselves;
- allocation is the per-operation ceiling and it is not the lookups in front of the allocator;
- and all three of those end at what an object IS.

⇒ **Step 1 of the rewrite should be re-planned as open, not blocked.** Anyone picking it up should
re-run the four checks above first — they are cheap, and one of them is a full boot.


## …but arena maps are the wrong gate, and the census says which classes are

Having found `RAVA_ARENA_MAPS` correct and neutral, the obvious next question is what it buys
toward the precondition, which is stated as "100% off handles, held back by native collections".
Measured:

```
meter                default     arena_maps    delta
births_handle      4,005,790      4,039,057     +0.8%
births_arena       4,080,488      4,227,067     +3.6%
fu_fast           12,651,930     12,691,354     +0.3%
```

**Handle births do not move.** The flag is correct and costs nothing, and it also does nothing for
the population it was supposed to be the gate for.

The handle census says why — the objects born as handles are not maps:

```
36.2%  java/util/Iterator
23.4%  it/unimi/dsi/fastutil/doubles/DoubleArrayList
21.5%  java/util/stream/Stream
13.2%  java/util/Map$Entry
 2.6%  java/util/BitSet
 2.1%  java/util/ArrayList
```

**Four classes are 94% of every handle birth**, and `java/util/Map` is not among them.

⇒ **The rewrite's step 1 is not "arena maps".** It is an arena representation for `Iterator`,
`DoubleArrayList`, `Stream` and `Map$Entry` — and two of those four are the same classes this
session already built inline arms for, so their shapes are known: `Iterator` is `$elems`/`$pos`,
`DoubleArrayList` is `a` + `size`, and `Map$Entry` is two fields. `Stream` is the one with no
known shape.

That is a materially different and much more specific starting point than the ledger's, and it
was reachable only because the flag was re-tested instead of inherited.


## Step 1's largest item is not blocked by a predicate — it just mints a handle

`java/util/Iterator` is 36.2% of all handle births, the single largest producer, and unlike the
other three it is a FABRICATED class: this engine creates it itself, in six lines.

```rust
pub(crate) fn iterator_of(vm: &mut Vm, elems: Vec<Value>) -> Value {
    let arr = vm.heap.alloc_values(OBJ, elems);
    let it = vm.heap.alloc("java/util/Iterator");          // <- the handle minter, directly
    vm.heap.put_field_syn(it, Syn::Elems, Value::Array(arr));
    vm.heap.put_field_syn(it, Syn::Pos, Value::Int(0));
    Value::Ref(it)
}
```

And the birth predicate does not forbid it. `is_native_backed_alloc` rejects a class when
`is_collection` matches, and `is_collection` is a substring test over
`LIST | SET | QUEUE | DEQUE | COLLECTION | VECTOR | STACK` — **`java/util/Iterator` contains none
of those**, nor is it a native map or an entry type. So nothing in the eligibility rules stands in
the way; `iterator_of` was simply written to call `Heap::alloc` rather than the arena route.

⇒ **The work is not a predicate change, it is a representation change**: every native that reads
`$elems`/`$pos` off an iterator must accept a `Value::Obj` as well as a `Value::Ref`. Some already
do — `read_value_field` and `read_value_field_shared` both branch on the two — and the inline arm
this session fixed reads them by field id, which is representation-independent. The unknown is how
many other sites take a bare `u32` handle.

That is the first concrete task of the rewrite, on the largest single population, with the
predicate already clear and the field shape already known.


### And the unknown in it is countable

"How many other sites take a bare `u32` handle" is a grep, not a risk:

```
Syn::Elems / Syn::Pos sites            139
Syn::Pos sites                          69
  of those, via get_field_syn/put_field_syn (bare u32)   68
  but NOT iterators — nio buffers use $pos for buffer position:
    natives/buffers.rs 12, nio/channels.rs 9, nio/mod.rs 6   = 27
```

So the iterator-facing subset is roughly **40 sites**, and the two accessors that already branch
on `Value::Ref` vs `Value::Obj` (`read_value_field` / `read_value_field_shared`) are the shape the
rest have to be converted to. `natives/buffers.rs` and `nio/*` can be left alone entirely — they
are a different object that happens to share a synthetic field name.

That is the whole of step 1's first task: ~40 call sites, a known field shape, a predicate that
already permits it, and 4 bit-exact gates plus 21 differential probes to verify it against.


## ⚠And now price the rewrite itself — it is worth about 1%, not 2.1x

Having re-opened the handle-heap path and sized its first task, the rule this document has
enforced against every other candidate applies to it too. Convert to a share BEFORE building:

```
Iterator handle births      1,450,096  x (96.2 - 28.8 ns)  = 0.098 s = 0.30% of the boot
ALL handle births           4,005,790  x  67.4 ns          = 0.27  s = 0.82%
full Value::Ref removal     (already recorded in this ledger)         = 1.19%
```

**So the entire handle-heap rewrite prices at roughly one percent.** It is real, it is now
unblocked, and its first task is 40 sites — and it is not the answer to a 2.1x gap. Recommending
it as "the" next step, as the previous three sections did, was the same mistake this document
catches everywhere else: naming a mechanism without converting it to a share.

### Where the gap actually is, on the evidence assembled here

```
calls                 at PARITY with the JVM (0 ns static, 3 ns virtual)
serving natives       38% of the boot
  of which the crossing is not the cost   (classification memo: zero)
  and the birth is not the cost           (three allocation attempts: zero)
  => what remains is what the operations THEMSELVES do
interpretation         9.7%
seam                   5.1%
heap                   3.4%
compiler               3.0%
```

`Map.get` costs **175 ns against the JVM's 2**; `Map.containsKey` 166 against 1;
`List.iterator` 309 against 0. Those are not dispatch, not allocation and not the boundary — they
are the implementations. A native layer as fast as the JDK's would take 38% of the boot down to a
few percent, which is the 2.1x.

⇒ **The target is the native implementations themselves, and the first one to price is
`java/util/HashMap.get` at 175 ns.** Everything else in this document is either at parity, already
closed by measurement, or worth about one percent.


## The mechanism behind the 38%: our maps are keyed by a RENDERED STRING

`HashMap.get` was named above as the first native to price. Reading it, the answer is in the
code's own GC-safety comment:

> `map_key` → `value_to_string` → the key's guest `toString()`.

**A native map lookup renders its key to a String**, and for anything but a primitive that runs
the key's guest `toString()` bytecode — which for a record key compiles to an `ObjectMethods`
`makeConcatWithConstants` invokedynamic that ALLOCATES. The surrounding comment exists because
that allocation can move the arena mid-lookup.

That is the 175 ns against the JVM's 2, and it explains the whole category rather than one method:

- `Map.get` 175, `Map.containsKey` 166, `Map.get(Integer)` 179 — all pay a key render
- `memchr` at 2.3% of the boot profile and the `valkey_memo_with`/`combo_hash_of` thread-local at
  2.2% are the caches built to make that rendering survivable
- this session's very first allocation find — `RAVA_RECORD_LAYOUT` read from the environment on
  every key render, 4% of all allocations — was on the same path

⇒ **The JVM hashes and compares; we serialise.** A per-object `$keystr` memo already brings a
repeat down to ~251 ns, which is the shape of a workaround, not a fix.

⚠This ledger records a "map-key render" arc as closed with "there is no 3.6 s prize". That verdict
was about optimising the RENDERING. It is not a verdict on replacing rendered-string keys with
hash+equals, which is a different and much larger change — and which is what a 175 ns → 2 ns
`Map.get` would require.

⇒ **The next real target, and the only one left whose ceiling matches the 2.1x gap: key a native
map by hash+equals instead of by a rendered string.** Everything else in this document is at
parity, closed by measurement, or worth about one percent.


## …and the map recommendation fails the same test. The 38% has no concentrated member.

The section above named "key a native map by hash+equals" as the target whose ceiling matches the
gap. Apply the rule to it, as it was just applied to the rewrite:

```
native-serving meters, share of the real boot
  fu_fast          23.27%   7.68 s     <- SELF time; already contains the operations
  interp_self       9.69%   3.20 s
  native_stream     3.75%   1.24 s
  native_services   3.07%   1.01 s
  native_stages     3.01%   0.99 s
  native_self       2.46%   0.81 s
  native_lastarm    1.67%   0.55 s
  native_map        1.10%   0.36 s     <- everything maps do, all of it
```

**`native_map` is 1.10% of the boot.** Rendered-string keys are a real and ugly mechanism, and
fixing them cannot be worth more than that. The recommendation was wrong for exactly the reason
the previous one was, one section earlier.

### The honest conclusion

Every candidate priced today lands in the same band:

```
handle-heap rewrite (all births)      0.82%      Value::Ref removal    1.19%
native maps by hash+equals            1.10%      class-word memo       ~1% (measured ZERO)
offload memo refinement          0.4-0.8%       jit_indy allocations   0.2-0.4%
```

**There is no concentrated member of the 38%.** It is real in aggregate and it is a dozen families
of one to three percent each — which is what this ledger's older entries already said with
"flatness goes all the way down" and "~50 items of 0.5-2%". Today's work confirms it with a
complete price map instead of a profile.

⇒ Closing a 2.1x gap therefore needs either many separate one-percent wins — this session shipped
several and they moved the boot ~2-3%, inside its floor — or one change that makes ALL natives
faster at once. The only such change is what the natives fundamentally ARE: today they are Rust
implementations reached through a guest-value ABI, and every one of them pays for that ABI
(`Map.get` serialises its key; `fu_fastpath` classifies its receiver; every crossing boxes and
unboxes `Value`).

**That is the finding to carry forward: the gap is the ABI between guest values and native code,
not any single native, and not the heap.**

## The measurement ledger, re-taken on a calm box (2026-08-30)

Every A/B in the preceding sections was taken while the box drifted — the same binary read a
DataPackJson process CPU of 22.3 s, 24.5 s and 26.9 s across one afternoon, and two consecutive
series of the same flag disagreed in SIGN twice. The rule this ledger already carried
("first action on a cold box: a null test, then the A/B") was applied again at the end of the
session, and the answer was that the instrument had recovered:

```
NULL TEST, RAVA_NULL_TEST_NOT_A_FLAG (does not exist)   min 0.96%   med 1.27%
```

⇒ **The floor was ±1.3%, not ±8%**, so everything shipped that day could be re-priced. All
figures are the cost of DISABLING the mechanism, 6 ABBA pairs each, warm-up discarded:

| kill switch | min | median | verdict |
|---|---|---|---|
| `RAVA_NO_JIT_GUEST_SUB` | **+4.23%** | **+2.56%** | **DISJOINT — the only clean win** |
| `RAVA_NO_DLIST_EQ` | +1.89% | +1.82% | above the floor, not disjoint |
| `RAVA_NO_TREENODE` | +0.77% | +0.83% | at the floor |
| `RAVA_NO_TRIVIAL_GETTER` | +0.84% | +0.76% | at the floor |
| `RAVA_NO_REFMAP` | +0.49% | +0.21% | at the floor |
| `RAVA_NO_RENDER_IDENT` | −5.52% | +0.21% | at the floor (the min is one outlier run) |

⭐**This retroactively explains every "the sign did not reproduce" in the session.** The changes
did not flip; the BOX did. A result taken during drift is not a smaller result — it is not a
result, and the only defence is the null test that establishes the day's floor before anything
else is believed.

⭐And the ordering is itself the finding: **the one mechanism that measures is the one that changed
WHAT RUNS (compiling a class the JIT had refused for its ancestor), while the four that changed
HOW A CALL IS SERVED all land at the floor** — even the one that removed 705,840 crossings. The
same shape this document already recorded from the other direction ("coverage is not time"), now
with the converse: **a crossing removed is not time either, unless what it was crossing INTO was
the expensive part.**

Standing numbers on the same calm box, back to back:

```
rava   wall 31.2   cpu 33.5   cores 1.07   Done 0.417   RSS 1287 MB
java   wall  6.5   cpu 20.8   cores 3.20   Done 0.200   RSS  542 MB
```

Work 1.61x, cores 2.99x, product 4.8 = the observed wall ratio 4.8. The arithmetic closes, and
the larger factor is still the one this engine cannot use: java reaches 3.20 cores on a boot that
`java -Xint` runs at 1.22, i.e. the difference is its COMPILER, not the workload.

## ⛔The class-word kind memo, re-tried with the arena half — and it is still zero

The publisher move (`d315b395`) paid 3.7-4.3% for taking ONE forty-character string compare off
`fu_fastpath`'s entry, which makes the obvious next question: the classification match itself
compares the receiver's class name against a dozen more. Memoise the verdict by class identity
and none of them run.

Built twice. First keyed on the heap-Ref class WORD (a 64-bit key, so the table must store and
compare it): **307,072 hits** on a datapack run, and disabling it read +1.84% min / +0.48% median
— the floor. The census says why: the arrivals are dominated by `Value::Obj`, and the Ref table
never saw them. So the arena half was added, keyed on the arena class id — a small DENSE integer,
so the table is exact by construction with no key to verify, one byte load:

    fu_kind_hit    307,072 -> 2,123,323      (7x the coverage)
    disabling it   -1.30% min  -0.67% median  (i.e. the memo is very slightly WORSE)

⭐**Seven times the hit rate and the answer does not move.** Reverted. The mechanism is now clear
and it is the difference between the two shapes: the classification match is `match s { "a" =>,
"b" => … }` over names of DIFFERENT LENGTHS, which lowers to a length switch and one or two
memcmps — a handful of nanoseconds. What the publisher move deleted was not a compare, it was an
`unbox_ref` plus a `class_of` plus a full-length compare, running BEFORE and IN ADDITION TO that
match on every call.

⇒ **"Remove the string compares" was the wrong reading of that 4%.** The win was removing an
entire redundant pass over the receiver, and a memo that shortens the pass that remains buys
nothing. This also confirms the earlier "class-word memo measured ZERO" entry, now with the
reason rather than the observation.

## ⛔And the other "redundant pass" on the same path is not one

If the publisher's +3.7-4.3% came from deleting a whole redundant pass over the receiver, the
next candidate is one line below it: `jit_fu_invoke` calls `pool.member_ref(cp)` on EVERY one of
its 38 M calls a boot, to produce a `(name, descriptor)` pair that cannot change for a given
`(class, cp)`. Memoised per site — sound, because a `LoadedClass` is owned by the loader's map
for the process (only DERIVED caches are ever cleared) so its pool's UTF8 storage never moves:

    RAVA_NO_FU_MEMBER_MEMO=1 (re-derive every call)    min 0.00%   median +1.08%

At the floor, and reverted. `member_ref` looks expensive written out — a `MethodRef`, its
`NameAndType`, two UTF8s and the owner's class name — but every one of those is an INDEX into a
`Vec` and the strings are already slices; there is no parse and no allocation. ⇒ **"A pass that
recomputes a constant" is only worth removing when the pass touches memory the call would not
otherwise touch.** The publisher did (`unbox_ref` + `class_of` + a 40-byte compare on the
object); this one walks a table that is already hot.

## 52 million allocations in one datapack parse — the number is solid, the attribution is not

`[PC-MALLOC]` on `bench/DataPackJson` (6,351 files, ~2.9 MB of JSON): **52,027,810 allocations,
50,120,757 frees** — about 8,000 allocations per file, against 1,869,654 objects born as heap
handles. So the great majority is Rust-side: `Vec`, `String`, `Box` inside the natives and the
seam. At mimalloc's ~20-30 ns that is on the order of a second of a ~7-second parse, which would
make it the largest single mechanism named on this workload.

⚠**And it could not be attributed, which is the part worth recording.** `RAVA_ALLOC_SITES` names
the caller of each sampled allocation, and its top row is
`vm::offload::is_offloadable_rec::closure$1` at 5.1%. That is contradicted by the offload probe's
own counters, added here as `RAVA_DIAG=offload` gauges: on the same workload
`offload_analyzed = 0`, `offload_hits = 0` — **the analysis never runs**, so it cannot be
allocating three million times.

Rebuilding the diagnostic binary with `/OPT:NOICF` (which this file's own banner names as the
precondition) did not change the row. The remaining explanation is inlining: a leaf sample lands
on the symbol the allocation was inlined INTO, and a generic closure passed into
`method_offloadable_forked` is a large inlining target. ⇒ **`ALLOC-LEAF` is a caller census, not a
site census, and on an inlined build the difference matters.** The census needs an allocation-site
marker the optimiser cannot move — an explicit `#[inline(never)]` shim on the allocating helpers,
or per-arena counters — before this million-scale number can be aimed at anything.

The gauges stay: they are zero in the production build (the probe compiles out without
`wall-profile`), and they are what turned a plausible-looking profile row into a contradiction.

## And the frame-buffer pool is a warm-up cost, not a steady one

`RAVA_VECGROW_STAT` on the same parse, which the pool's own comment ("4096, not 1024") invites
raising the cap for:

```
POOL take=19,499,991  miss=2,488,004 (12.76%)  give_full=865,671  give_cap0=1,757,891
```

The miss count is IDENTICAL in every periodic dump after the first — 2,488,004 at t=2 s and at
the end. ⇒ **every pool miss is warm-up**; the steady state never misses, and a larger cap cannot
help what has already happened. What a larger cap would recover is `give_full`, 865,671 discarded
buffers of **52 M allocations = 1.7%**, about 0.02-0.03 s.

Not built. The recompute rule again: the comment argued from a ratio (misses vs discards) without
converting either to a share of the workload.

## …and a size histogram DOES survive the optimiser

`ALLOC-LEAF` names the function an allocation was inlined into, so it could not aim at the parse's
52 M allocations. A byte count has no symbol to fold and no frame to attribute, so
`RAVA_ALLOC_SIZES=1` answers the same question from the other side:

```
     32..63     41.0%          16..31     20.8%          64..127    15.8%
    128..255     8.2%           8..15      6.6%         256..511     2.6%
```

**Sixty-two percent of every allocation on this workload is 16-63 bytes**, and 78% is under 128.
With `Value` at 16 bytes that is the signature of SHORT `Vec<Value>` — two or three elements —
plus small `Box`es and short strings: argument buffers, operand stacks, entry pairs. Nothing in
the distribution is large; there is no buffer to right-size and no single structure to pool.

⇒ It is the same conclusion the rest of this document reaches, now measured in a way inlining
cannot distort: **the allocation mass is the representation itself.** A `Vec<Value>` of two
elements per call is not a site to fix, it is what the engine's calling convention IS.

## Allocation attributed by REGION — and it is interpretation, not the seam

A size histogram says what is allocated; it cannot say where. `RAVA_ALLOC_SIZES=1` now also tags
each allocation with the engine region it happened in — a thread-local set by a guard at three
coarse boundaries. A tag is data, not a frame, so inlining cannot move it.

⚠**The first reading was wrong in an instructive way.** With the seam's guard left open across
the dispatch it read `seam 54.3%` — because a COMPILED callee runs with no region of its own, so
every object the callee allocated was attributed to the crossing that called it. Closing the guard
before the dispatch, so `seam` means the marshalling only:

```
interpreter frame     78,192,610   87.6%     6.76 GB
seam (marshalling)     4,961,395    5.6%     0.62 GB
fu_fastpath            4,569,709    5.1%     0.19 GB
other                  1,561,017    1.7%     3.64 GB
```

**87.6% of all allocations happen under an interpreter frame** — and there are only 877,227
interpreted entries on this workload against 2.6 M compiled ones. That is **~89 allocations per
interpreted frame** (the region nests, so it includes what the interpreted method calls into).

⚠**AND THAT READING WAS STILL WRONG, FOR THE SAME REASON, TWICE MORE.** A region only means what
its boundaries say, and two spans still had none: the natives reached from interpreted code, and
COMPILED bodies. Adding both:

```
compiled guest body   58,791,499   65.6%     4.17 GB
interpreter frame     14,814,306   16.5%     1.48 GB
native cascade         5,271,966    5.9%     0.83 GB
seam (marshalling)     4,697,420    5.2%     0.72 GB
fu_fastpath            4,487,419    5.0%     0.19 GB
other                  1,586,228    1.8%
```

⭐**The allocation mass is the PROGRAM's own objects, executed by compiled code — not the
engine's plumbing.** Seam, native fast path and native cascade together are 16.1%; the dispatch
machinery this ledger has spent the most words on allocates a sixth of the total.

⭐And the method is the lesson: the same census read `seam 54.3%`, then `interpreter 87.6%`, then
`compiled 65.6%` — each time because a region was left open across a span that had no region of
its own, so its allocations were charged to the caller. **A region attribution is only as true as
its narrowest boundary**, and the way to check one is to add the missing boundary and watch which
number moves.

### …and the obvious reading of that 65.6% is wrong too — births are only an eighth

"Compiled code allocates two thirds" invites one hypothesis: the program's `new` and `newarray`.
Counted on the same parse:

```
births_array   6,610,026     (ref 3.43 M, int 1.47 M, fp 0.92 M, byte 0.67 M, long 0.12 M)
births_arena   2,700,537
births_handle  2,512,651
                              total ~11.8 M against 52-89 M allocations = ~13%
```

⇒ **Object and array births are an eighth of the allocations.** The other seven eighths are host
TEMPORARIES created while compiled code is on the stack — its callbacks into the runtime — because
those callbacks have no region of their own and inherit `Compiled`. And the leaf census, for all
its inlining trouble, agrees on the shape: `jit_call_body` 4.7%, `jit_indy` 4.3%, `rava_newarray`
4.0%, `put_field_syn` 4.6%, `pop_args_n` 4.1%, `invokedynamic` 3.3% — **~300 sites, none above 5%**.

⇒ The allocation profile is as flat as the time profile, and for the same reason. There is no
allocation to delete; there is a representation that allocates.

## The namespace-compile blocker, as a defect report rather than a hypothesis

Three instruments later, "compiling the served namespaces crashes" reads like this:

```
minimal repro     AbstractIntList + IntImmutableList compiled together (3 of 3, deterministic)
default config    faults in COMPILED net/minecraft/…/StateDefinition$Builder.add([LProperty;)
                  read of 0x100000000  — an arena header (nfields=1, class_id=0) as a pointer
arena lambdas off faults in the NATIVE helper jit_bitset_isempty, called from compiled
                  DiscreteCubeMerger.forMergedIndexes, read of 0x1000001c079
```

Both consumers are innocent of fastutil: one is Minecraft's own compiled code, the other is one of
this engine's JIT helpers. ⇒ **The failure is not "fastutil's bytecode reads an empty slot"** —
the reading `aot_link.rs` predicts and the two-class bisect suggested. Compiling those classes
changes the REPRESENTATION of values that reach code compiled against the other one, and that code
dereferences what it is handed.

Which is the same one-representation blocker this document keeps arriving at, now with the three
parts a fix needs: what must be compiled, WHO faults, and what shape the value has when it does.
The `<host 0x…>` form of this report — everything before the crash handler learned to read
`jit_method_push`'s stack — had none of them.

## The session's changes, measured on the BOOT (interleaved, product config, harness fixed)

`boot_ab_flag.ps1` was setting `RAVA_OFFLOAD_FIRST=1` in its `-Jit` arm — the same defect
`jvm_cpu.ps1` had (row 452) — so every "product" boot it had ever printed was a number for that
flag. With it removed and the box calm (parse-workload null test: 0.41% median), three rounds per
flag, cost of DISABLING each:

| flag | CPU min | CPU median | wall (SET vs UNSET) | |
|---|---|---|---|---|
| `RAVA_NO_JIT_GUEST_SUB` | **+3.88%** | **+3.85%** | 32.8/32.7/32.2 vs 31.7/31.2/31.2 | **DISJOINT** |
| `RAVA_NO_NS_BYTECODE` | +0.60%* | **+2.67%** | 31.2/32.4/32.2 vs 31.2/31.7/31.3 | |
| `RAVA_FU_TOPPUB` (old placement) | −0.30% | −1.15% | 31.2/32.2/32.3 vs 31.2/32.4/32.8 | |

\* min and median disagree; the harness treats the median as the verdict.

⇒ **The two compile-coverage changes confirm on the boot**, one of them disjoint — together about
6% of boot CPU and ~1.3 s of wall. The parse phase said the same thing at a much finer floor.

⭐**And the publisher move does NOT confirm on the boot, for a reason its own mechanism predicts.**
Its cost was a per-call test that only runs *while the awaited class has not been seen* — a boot
meets a `DoubleArrayList` early, so the window is short; the JSON-only parse workload may never
meet one, so every call paid. **A win can be workload-shaped without either measurement being
wrong**, and the honest statement is the pair, not the larger number.

### The shelved opt-in arms, re-priced on the boot with the fixed harness

Three flags had been left opt-in on evidence the old harness could not give (it was measuring
`RAVA_OFFLOAD_FIRST`). Cost of ENABLING each, 3 interleaved rounds, product config:

```
RAVA_ALIST_SIZE_INLINE   +0.30% min   +2.08% median
RAVA_ALIST_GET_INLINE    +1.18% min   +2.07% median
RAVA_FU_RETIRE           +1.48% min   +0.88% median
```

All three are slower, and they stay off. ⭐This is the same shape as the +30% regression from
widening the trivial-getter arm to every zero-argument site: **an inline arm is a guard paid by
everyone to serve a few, and it only wins when the call it replaces was a crossing into something
expensive.** `List.get` was once named the largest single callee crossing the seam — and inlining
it still costs 2%, because what it crosses into is cheap.

⇒ Three open questions closed for the price of twelve minutes, now that the boot can answer.

### The parallelism front, checked rather than assumed

The gap the goal names most loudly is cores: 1.08 against `java`'s 3.15. Two mechanisms in the
tree add real threads, and both were checked with the working harness:

* `RAVA_BACKGROUND_JIT` — already DEFAULT-ON since 2026-08-03, so compilation is off the critical
  path already and is part of the 1.08.
* `RAVA_FORK_PARALLEL` — per-element iterator-loop methods on GIL-free workers. With it on: the 4
  gates stay green, the DataPackJson hash is unchanged, and **cores read 1.09** — unchanged. With
  `RAVA_FORK_TRACE=1` the run prints **zero** fork lines of any kind: not a refusal, an absence.
  The divert never triggers, because the workload has no eligible iterator loop.

⇒ The one built parallelism mechanism does not engage on this workload, and `java`'s extra cores
are its compiler threads (measured earlier: `java -Xint` runs the same boot at 1.22 cores).
**Neither engine parallelises the guest work; ours simply has less compiler to run in the
background.** Closing the core-count gap is therefore not a matter of turning something on.

## The namespace-compile line, ended by making it correct

`native_claimed` gives the gate the fact it never had: a method the native cascade has ANSWERED is
refused both compilation and execution (the check sits at the top of `jit_run`, whose `None` means
"do not run compiled"), and served-namespace classes are already barred from the direct/PIC path,
so a compiled caller cannot reach one behind the gate's back. The counter-example that retired the
flag now passes **15/15 parse runs and a full boot**, gates and probes green.

And with it correct, it is slower:

```
RAVA_NS_BYTECODE, boot, 3 interleaved rounds   SET +1.45% min  +2.03% median  DISJOINT
```

against **-2.67% median** for the same flag before the gate existed.

⭐**The win was the unsoundness.** What the relaxation actually bought was compiling the methods
the natives were serving — exactly the ones that must not be compiled — and once those are
excluded the remainder (interpreted entries -8% instead of -24%) costs more in guard and compile
time than it returns.

⇒ The line that began with "java.util as bytecode" and ran through four refutations ends here,
not because the idea was wrong but because its measurable part was the part that was incorrect.
`RAVA_NS_BYTECODE` and `RAVA_GUAVA_JIT` stay opt-in; `native_claimed` stays, because it is what
makes the question answerable at all — and it is inert unless one of those flags arms it.

## The first mutually-exclusive TIME attribution — and the seam is 20%, not 5%

Every price this document carried was a category that overlapped its neighbours: "natives 38%",
"seam 5.1%", "interpretation 9.7%" summed to ~60% and left 40% unowned. Today's allocation work
showed why that is not a bookkeeping detail — the same census read 54%, 88% and 66% for the same
quantity as its boundaries got narrower.

So the region tags now feed the PC sampler too. A sampler cannot read another thread's TLS, so the
guard publishes the tag to a global slot (`REGION_NOW`) — sound while guest code runs under the
GIL, which the 1.08-core boot confirms. Result, on `DataPackJson`:

```
compiled guest body    50.6%
seam (marshalling)     20.3%
interpreter frame      16.6%
fu_fastpath             5.8%
native cascade          5.4%
other (unowned)         1.3%
```

**Unowned is 1.3%.** The 40% that no category claimed was not hiding anywhere — it was spread
across boundaries that overlapped.

⭐**And the headline is the seam: 20.3%, against the 5.1% this document had.** The region is closed
BEFORE the dispatch and re-entered after `frames::pop()`, so this is the crossing's own
marshalling — argument unboxing, the synthetic frame, resolution — with the callee excluded by
construction. The fast path (`fu_fastpath`) is 5.8% by comparison: **the cheap crossing is cheap;
it is the GENERAL one that costs**, and 893,000 refusals a run take exactly that path.

⇒ This reorders the work. "Make a native faster" is 5.4%. "Make the fast path faster" is 5.8%.
"Stop taking the general crossing" is 20.3%.

### ⛔And that lasted twenty minutes: the seam is 7.9%, not 20.3%

The rule this document had just written down caught its own next violation. `jit_direct_invoke`
enters compiled code through `call_raw` WITHOUT going through `jit_enter`, so the callee ran with
no region of its own and was charged to the crossing that dispatched it. With a `Compiled` guard
around that call:

```
compiled guest body    63.0%   (was 50.6%)
interpreter frame      16.9%
seam (marshalling)      7.9%   (was 20.3%)
fu_fastpath             5.8%
native cascade          5.2%
other (unowned)         1.2%
```

**Twelve points moved from the seam to the callee it dispatched.** Fourth time in one session that
narrowing a boundary moved a number by a factor, and the third time it overturned a conclusion I
had already written down.

⇒ The honest map: **80% of the time is executing guest code** (63% compiled, 17% interpreted) and
**19% is this engine's machinery** (seam 7.9 + fast path 5.8 + native cascade 5.2). There is no
20% crossing to eliminate. Closing a 5x gap means the guest execution itself — the code the
compiler emits and the representation it operates on — not the plumbing around it.

## The 63% split: 18% is emitted code, 82% is the runtime it calls

Same samples, split by the compiled-range test:

```
[COMPILED-SPLIT] 10,450 samples under a compiled body:
                 1,905 (18.2%) in EMITTED CODE, 8,545 (81.8%) in host callbacks
```

⇒ Of the whole workload, **~11.5% is machine code the JIT emitted** and **~51.5% is the runtime
that machine code calls into**. And the callback list is dominated by CALLS, not by data work:

```
invokedynamic 4.3%   mi_malloc_aligned 4.3%   jit_indy 3.1%   memchr 2.8%
invokevirtual 2.8%   dispatch_owner_cascade 1.8%   jit_call 1.8%   dispatch_miss 1.4%
```

**Compiled code cannot make a call without returning to Rust**, and that — not the codegen — is
where the time is. The ledger's oldest entry ("the call is the category") is right, and this is
the first measurement of it with boundaries that do not overlap.

### ⭐The machine-code dispatch table is allocated and DEAD

The mechanism that would keep those calls in machine code exists and does nothing:

```
[PC-DT] mc_hits=0 mc_probes=0 serves=0 fills=0 grows=0
        promoted_sites=382  table_entries=1,564,672 (~12,224 KB)
```

**12 MB reserved, 382 sites promoted, zero probes and zero fills.** Two facts found while chasing
it:

* `main_dt_learn()` — the learning path on the main thread — is opt-in (`RAVA_MAIN_DT_LEARN`) and
  default OFF, so `learn` is always 0 and `note_serve_direct` never records anything.
* Turning it ON changes nothing: `serves` stays 0. The upstream fact is `mc_probes=0` — the
  emitted probe never executes, so `DISPATCH_SITE` is never set and the learn slot stays 0
  regardless.

The probe is emitted whenever `dispatch_table::enabled()` (default on) and runs on the arm where
the 4-way PIC has already missed. So the open question is exactly one: **does the 4-way PIC always
hit (and the misses in the callback list come from sites that have no PIC at all), or is this emit
path not reached on this workload?** `[PC-DT]`'s own counters answer it and now print at exit
rather than only from the periodic sampler dump.

### …and grouped, the callbacks say the dispatch table is not the largest item

Reading the `[COMPILED-SPLIT]` list as groups rather than rows changes which lead is biggest:

```
lambda / indy machinery   invokedynamic 4.3 + jit_indy 3.1              = 7.4%
virtual dispatch          invokevirtual 2.8 + jit_call 1.8 + miss 1.4   = 6.0%
allocation                mi_malloc_aligned 4.3 + with_tlab 1.4         = 5.7%
strings / interner        memchr 2.8                                    = 2.8%
```

(percentages of the compiled region, which is 63% of the workload)

So the dead dispatch table is worth at most the 6.0% virtual-dispatch group, and **the largest
single group is the lambda/indy machinery at 7.4%** — every `invokedynamic` in compiled code
returns to Rust to build or invoke a closure. That is a different lead with a different fix, and
it was invisible while the categories overlapped.

⇒ Three leads now have sizes instead of adjectives: indy 7.4%, dispatch 6.0%, allocation 5.7% —
all inside the 51.5% that compiled code spends in the runtime. None of them is the 5x on its own,
which is itself the answer to "is there one lever": there is not, and the four largest are now
named and measured.

### The indy lead, sized: 2.53 M capturing lambda mints

`RAVA_DIAG=indy` on the same workload:

```
lambda              2,537,018      concat   296,460
lambda_noncapturing     6,188      record     9,381
```

**2.53 M LambdaMetafactory assemblies, of which 6,188 capture nothing.** So the traffic is
essentially all capturing closures — each one a callback into Rust that pops its captures and
mints an object. Java allocates too, but its call site is a linked MethodHandle and its allocation
is a TLAB bump; ours re-enters the runtime.

⛔The cheap part is not worth building: `pop_args` re-walks the descriptor per call
(`arg_count(desc)`), and 2.53 M walks of a ~20-character string is on the order of 1% — below the
floor by the recompute rule, and the plan is already cached per site.

⇒ Moving the 7.4% means **minting the closure in emitted code** instead of returning to Rust —
the same shape as `emit_inline_concat`, which already does this for the StringConcatFactory arm
(296,460 calls). The arena lambda layout and its site index already exist
(`nextgen::lambdas`, `AL_SITE_SLOT`), so the pieces are there; what is missing is the emitter.
That is a codegen feature with a measured 7.4% behind it, and it is the largest single named item
left in the engine.

## ⛔The 21.1 M refusals are a big COUNT of a cheap EVENT

`RAVA_JIT_PIC_PROBE` on a product boot, with `picprobe::dump()` newly hooked to the periodic
sampler (the FOURTH census in this tree found reporting only at a teardown a server boot never
reaches):

```
[PIC-COV] dynamic calls out of compiled code = 181,289,555
[PIC-COV]   machine-code PIC hit (no Rust)     143,398,439 (79.0%)   <- real PIC coverage
[PIC-COV]   PIC miss -> rava_dispatch_miss      35,105,187 (19.3%)
[PIC-COV]   plain invoke (static/special)        2,785,929 ( 1.5%)
[PIC-COV] compiled call SITES = 8,748: PIC-emitted 5,295 (60%)
[PIC-COV] synthetic-frame fall-through = 368,319 total (92% native-served)
[HEAPPIC-LIVE] cache fills=32  never-cacheable=21,112,339
```

Thirty-two positive fills for a whole boot against **21.1 M refusals** — 60% of every PIC miss.
And the mechanism was named exactly: the `$`/`java/`-without-bytecode bail in `heappic_fill` sat
AHEAD of the negative-cache probe written directly under it, so it re-derived its own permanent
verdict on every call and returned WITHOUT recording anything.

So the order was reversed — class KEY first, 4-way negative probe, then the class NAME, with the
bail writing its `-1` through the common tail (and forbidden from evicting a positive, since 4
slots hold only 32 good entries all boot). 5/5 correctness gates pass. On a calm box — null test
against a nonexistent flag: **median 0.00%, min −0.85%, not disjoint**, the quietest floor this
tree has recorded — the A/B says:

```
RAVA_NO_PIC_ORDER (SET = the OLD order)  SET median 34.8  UNSET median 35.25  -1.28%
```

⇒ **The old order is FASTER. Reverted.** The bail was already the cheap path: two `starts_with`
on a borrowed `&str` cost less than `object_class_word` + a 4-way scan + a store, and a site
dispatching on `java/*` receivers is megamorphic, so four slots fill with negatives and every
later call re-derives with the ADDED cost on top.

⭐**This is the tree's own rule (rows 445-448) collecting again: convert a population into a share
of the boot BEFORE writing the code.** 19.4 M × ~20 ns is 0.39 s of 34 = **1.1%**, and that is the
ceiling for removing the work ENTIRELY — not for replacing it with comparable work. The number
that made this look like the session's biggest lead (21.1 M, 60% of all misses) is a count, and
the ledger has now paid for that lesson at both ends: a count too small to matter, and a count too
large to be true.

⇒ The line is closed. What survives it is the coverage number itself: **79.0% of the 181 M calls
out of compiled code already resolve entirely in machine code**, so the PIC is not the gap — and
`[COMPILED-SPLIT]`, now printing 60 rows with a cumulative column instead of 12, says why nothing
else is either: **547 distinct host callees, top-60 = 63.2% of callback samples, top row 5.06%.**

## ⭐The idle cores are not a contract problem — the lane ADMITS FOUR TASKS

Same calm box, same day. `bench/jvm_cpu.ps1` and the boot A/B harness, measured back to back:

```
java   wall  6.6   cpu 20.1   cores 3.03   Done 0.198   RSS  550 MB
rava   wall 32.8   cpu 35.0   cores 1.07   Done 0.42    RSS ~1.2 GB
```

**Work differs by 1.74x. Cores differ by 2.8x.** Eight axes of WORK have now been measured flat —
host callees (547, top 5.06%), regions (6, largest internally flat), callback families (largest
16% of callbacks), allocation sites (1,450 frames, top 5.4% => ~0.5% of boot), opcodes, opcode
pairs, dispatch, and PIC coverage (already 79.0%). Nothing on the work side is worth more than a
few percent. Three cores sit idle, and at unchanged work that is a **2.8x wall**.

`RAVA_FORK_STATS` says why, and it is not the `drain`-parks-main contract row 450 named:

```
admit# = 4        run# = 5        aborted=true = 4
keep=true = 1     keep=false = 3
abort_in_cb:  invoke x3,  putfield x1
run#2 aborted: NullPointerException: Profiler.ACTIVE_COUNT is null   (abort_in_cb=invoke)
run#4 aborted after 141,689 callbacks and 93 ms of real work — all of it discarded
```

**A whole product boot admits four tasks to the parallel lane, runs five, aborts four, and keeps
one batch out of four.** The join contract cannot be the binding constraint on a population of
four. Admission and abort are.

And the two abort causes are different in kind, which matters because only one of them is a
soundness limit:

  * `abort_in_cb=putfield` after 141,689 callbacks — the escape analysis admitted a body that then
    wrote to non-fresh state. That is the analysis being optimistic, and discarding is correct.
  * `Profiler.ACTIVE_COUNT is null` — **a static that is null only because a worker cannot run a
    `<clinit>` off-GIL.** The guest code is fine; the task would complete if main had initialised
    that class first. This is a fixable precondition, not a limit of the design.

Cache churn under the same run, `RAVA_OFFLOAD_CACHE_PROBE`:

```
[OFFLOAD-CACHE] hits=533,180 analyzed=12,692 (97.7% hit) stored=12,671
                unstable=21 epoch_clears=1,517 evicted=12,670
```

`analyzed` and `evicted` agree to within 22 — **essentially every admission proof this boot
performs exists because a loader-epoch move wiped the memo**, and a datapack boot loads classes
continuously. `is_offloadable_rec -> analyze -> arith_or_reject` is correspondingly the single
richest allocation LEAF in the boot (3.6% of sampled allocations).

⇒ Ranked, with the mechanism named for each:

1. **Pre-initialise the classes a lane task touches** (or let a worker demand `<clinit>` through
   main) — removes an abort family that is not a soundness limit at all.
2. **Split the admission memo** into epoch-stable facts and epoch-dependent closure proofs — the
   probe was written for exactly this question and now answers it: 1,517 clears, 12,670 evictions,
   and the re-proof is the top allocation leaf.
3. Only then the `drain` contract of row 450 — which is real, but cannot bind at n=4.

## ⭐The phase split says which 15x to attack — and it is not the one the ledger has been on

Product binary, boot log timestamps, against `java` on the same box:

| phase | rava | java | ratio | share of rava's wall |
|---|---|---|---|---|
| start → `Environment:` | 18 s | ~5.5 s | **3.3x** | 53% |
| `Environment:` → `Loaded 1470 recipes` (datapack reload) | 15 s | ~1 s | **15x** | 44% |
| → `Done` | 1 s | ~0 s | — | 3% |

⇒ **The reload phase is 15x off and is 44% of our wall against 15% of java's.** Phase A, which the
ledger's earlier map put at 90 s and 2.4x, is now 18 s and 3.3x — it improved by 5x while the
reload improved by 2.4x, so the deviation has MOVED, and the old ranking is stale.

Two corrections to conclusions drawn earlier today, both from measurement:

⛔**`RAVA_OFFLOAD_FIRST` no longer wins.** The ledger records it as "2.13x on the boot" and "the
best product number of the session". Today, same-binary A/B, 3 rounds: **+7.91% median / +3.13%
min, NOT disjoint** (one 44.5 s outlier). So the honest verdict is *no win today*, not *a loss* —
and its 2.13x belonged to the 112.7 s regime either way. (The harness was checked for the row-452
defect: `boot_ab_flag.ps1` line 24 removes this flag, but the `Set-Item` for the flag under test
runs after it, so the SET arm really does carry it.)

⛔**The core gap is not worth 2.8x.** Cores are 1.07 against java's 3.03, but only the reload phase
is parallel in java — so perfect parallelism there caps at ~26% of our wall, not 2.8x. The earlier
framing assumed the whole boot was parallelisable and was wrong.

⛔**And `memchr` in the reload phase is not the cost either.** `bench/boot_pc.ps1 -Stacks memchr`
VERIFIES the chain `next_match_back < SplitInternal::next_back<char> < jit_enter` — that is
`descriptor.rsplit(')')`, a reverse substring search on every entry to a compiled method that
returns a reference, in a function entered 8,545,386 times a boot. Replaced by plain byte loops
(`descriptor::ret_desc` / `params_start`, kill switch `RAVA_NO_FAST_DESC`), 5/5 gates green:
**min 0.00%, median −0.27%. Neutral.**

⭐**The instrument had already said so and the reading was careless**: the report labels that chain
"5.2% of 363 samples covered". Five percent of a 5% row is a quarter of a percent, which is what
the A/B returned. **Read the COVERAGE line before reading the caller name** — a verified attribution
that explains a twentieth of its row names a real caller and a negligible cost.


## The lane EARNS 13%, and three more defaults price at zero

Same day, same harness, 4 rounds each, CPU-s median (negative = SET faster):

| flag (SET) | median | min | disjoint | verdict |
|---|---|---|---|---|
| `RAVA_GILFREE_NOLANE` (lane OFF) | **+13.08%** | +8.52% | **YES** | the deferral lane is load-bearing |
| `RAVA_OFFLOAD_FIRST` (runtime before lane) | +7.91% | +3.13% | no | no win today |
| `RAVA_GSON_NATIVE` (native gson OFF in the UNSET arm) | −0.66% | +0.54% | no | inert |
| `RAVA_NO_FAST_DESC` (old `str::pattern` slicing) | −0.27% | 0.00% | no | the fast path is neutral |

⭐**The lane result is the day's only DISJOINT number**, and it settles a question the earlier
entry above got wrong: `[PC-LANE] drains=4 tasks=7` against `[PC-DEFER] offered=135 | DEFERRED=135`
looked like 128 bodies going missing, but turning the lane off costs 13% — so those bodies are
being executed by it productively and the drain census is simply incomplete, not the system.

And the admission gate is not the constraint either. `RAVA_PLAN_CENSUS` (new, hooked to the
periodic dump beside `picprobe`) instruments all eleven `return None` gates in `plan_meta`:

```
[PLAN-CENSUS] 5 plan_meta calls, 5 ADMITTED (100.00%)
```

**Five calls, 100% admitted, and not one of the eleven gates ever fires.** Every hypothesis of the
form "the lane refuses too much" is closed by that line — including `clinit NOT done`, which the
abort trace had made the obvious suspect.

⇒ So the reload phase's 15x is not the lane refusing work, not the runtime order, not gson, and
not descriptor parsing. What remains inside it is what the time-sliced profile shows and what every
other axis has shown: `mi_malloc` 5%, `memchr` 5%, `Heap::gc` 3-5%, `get_or_load` 4% — flat, with
**RSS 1.2 GB against java's 550 MB** as the one structural difference still unpriced.

## ⭐⭐24 methods are 88.5% of every interpreted entry — and compiling them is a LOSS

`RAVA_PROFILE_BOOT_DISPATCH` on a product boot:

```
[DISPATCH-TOTALS] interpreted-entries=15,212,176  jit-entries=7,712,120
                  host-crossings=30,700,766  crossings-per-jit-entry=3.98
[STUCK] hot in the interpreter, never compiled:
      3,828,505  it/unimi/dsi/fastutil/longs/LongOpenHashSet.isEmpty()Z
      3,478,226  it/unimi/dsi/fastutil/longs/LongArrayFIFOQueue.size()I
      3,478,226  it/unimi/dsi/fastutil/PriorityQueue.isEmpty()Z
      1,044,696  it/unimi/dsi/fastutil/objects/ReferenceOpenHashSet.isEmpty()Z
      1,043,468  it/unimi/dsi/fastutil/objects/ObjectArrayList.isEmpty()Z
        156,040  com/google/gson/internal/LinkedTreeMap.findByObject(...)
[STUCK]   24 methods, 13,463,451 interpreted entries total
```

**Twenty-four methods carry 88.5% of every interpreted entry in the boot, and 12.87 M of them are
five one-line accessors.** `PriorityQueue.isEmpty()` and `LongArrayFIFOQueue.size()` read the
IDENTICAL count — the default method calls the other, so each loop iteration builds TWO
interpreted frames to evaluate one comparison. That is the flattest profile's opposite: a single
population, named, with a mechanism.

The mechanism is the per-CLASS refusal at `vm/jit_run.rs`:

```rust
if self.jit_native_served_deep(&class.file.this_class) || is_cache_class(...) { return None }
```

⭐**The same one predicate answering five questions.** Its own comment already concedes the point —
"the native layer serves these calls so the bytecode normally never runs", against a census that
says the bytecode runs three million times and returns CORRECT results. Being in a natively-served
CLASS is not evidence that the natives answer THIS METHOD.

⛔**And the per-method gate does not pay.** `RAVA_NS_BYTECODE` arms exactly that narrowing (the
`native_claimed` table records the `(class, name, desc)` triples the cascade actually ANSWERED, so
the relaxation is per-method and sound). Same-binary A/B, 4 rounds: **+2.54% median, +3.78% min —
SLOWER**, reproducing the +2.03% recorded earlier in this session.

⇒ **So the answer is neither interpreting these calls nor compiling them.** A compiled entry is
*more* expensive than an interpreted one for a one-instruction body: `jit_enter` builds a cell
buffer, boxes every reference argument, pushes and pops a root frame and truncates the box table,
all to evaluate `return size == 0`.

⛔**And the obvious piece of that — the allocation — is not the cost either.** `vecgrow` reports
**8,545,386 of 8,545,386 entries grew the buffer, 52,262,650 elements memmoved per boot**, so the
buffer was replaced with a per-thread free list (zero allocation after warm-up, kill switch
`RAVA_NO_CELL_POOL`). 5/5 gates green. The A/B: **min +0.79%, median −0.78% — the signs disagree,
so it is noise.** Reverted rather than carried. 8.5 M `malloc`/`free` pairs a boot are worth ≤0.4%:
mimalloc is not the cost, and that retires the `vecgrow` census as a target.

⇒ What is left of the entry cost is the frame: rooting, boxing, and the crossing itself. The only
remaining shape that removes it is **not making the call** — inlining a trivial body at the site,
which is what `jit/trivial_getter.rs` already does for arena getters and what `inline_targets`
refuses here for the same per-class reason. That refusal, not the compile gate, is now the target.

## ⭐The gap decomposes: ~4x work everywhere, plus ONE phase where java uses three cores

The 15x reload ratio is a WALL ratio, and java runs that phase on three cores. Converting both
sides to WORK removes the anomaly:

| phase | rava CPU | java CPU (wall x cores) | work ratio |
|---|---|---|---|
| start → `Environment:` | 18 s | ~5.5 s x 1 | **3.3x** |
| datapack reload | 15 s | ~1 s x 3 ≈ 3 s | **5.0x** |
| → `Done` | 1 s | ~0 | — |

⇒ **The engine is uniformly 3.3-5x per core; there is no phase anomaly.** The 15x is 5x work times
3x cores, and that is the whole of it. Everything the eight flat axes said is consistent with this:
a uniform 4x has no hot spot to find, which is exactly what a flat profile looks like.

So the gap has two terms and both now have sizes:

1. **Work per core, ~4x.** Flat across host callees (547, top 5.06%), regions, callback families,
   allocation sites, opcodes, opcode pairs, dispatch, and PIC coverage (79% already in machine
   code). No lever above ~5% has survived measurement, and today added four more zeros to that
   list (`RAVA_NS_BYTECODE` +2.54%, the cell-buffer pool ±0.8% noise, `RAVA_GSON_NATIVE` −0.66%,
   the descriptor fast path −0.27%).
2. **Cores in the reload phase, 1 against 3.** Worth **up to 29% of the boot** (15 s → 5 s), which
   is the largest single number left. The lane already earns 13% there (the day's only DISJOINT
   result), so the headroom is real and partly reachable.

⇒ And the biggest named population — 24 methods, 13.46 M interpreted entries, 88.5% of them —
prices at **~5% of the boot** (13.46 M x ~130 ns, the measured cost of an entry whose callee is
free). Compiling them is measured at +2.54%, and inlining them is blocked by a documented crash:
`inline_targets` refuses because **inlining SKIPS the dispatch where the native would have won**,
and relaxing that shared predicate previously faulted in unrelated compiled code. A sound version
needs the native question asked at RUNTIME per receiver, not removed — that is the shape of the
work, and it is worth 5%, not 15x. Priced before building, per the rule this file keeps re-paying.

## ⛔Removing 5.9 M interpreted frames measures at ZERO — the average is not the member

The `[STUCK]` list named 24 methods carrying 88.5% of every interpreted entry, and three of the top
five are byte-identical thirteen-byte bodies:

```
0: aload_0   1: getfield size:I   4: ifne 11   7: iconst_1   8: goto 12   11: iconst_0   12: ireturn
```

`flatcall` — the frame-free hook that already serves getters and constants — refused them for one
reason: the shape BRANCHES, which `Flat::Getter` does not allow and `Flat::Straight` (straight-line
only) cannot reach. So a `Flat::FieldZeroTest` arm was added, matching that exact byte sequence
(literal branch offsets, so no other control flow can classify) for both `ifne` and `ifeq`.

It works, and the counting proves it: `LongOpenHashSet.isEmpty`, `ReferenceOpenHashSet.isEmpty` and
`ObjectArrayList.isEmpty` **leave the `[STUCK]` list entirely** — 5.9 M interpreted frames a boot
that no longer get built. 5/5 gates bit-exact.

And the boot A/B, same binary, 4 rounds, CPU-s:

```
RAVA_NO_FLAT_ZEROTEST  SET (shape OFF) median 38.65   UNSET (shape ON) 39.45   -2.03%
```

⇒ **Serving them frame-free is 2.03% slower** (arms overlap, so: no win). Now opt-in
(`RAVA_FLAT_ZEROTEST=1`).

⭐**What this prices is the replacement, not the frame.** `getfield_value` on a `Value::Ref`
receiver — resolve the field, walk the handle heap, clone the receiver `Value` — costs about what
the `Frame` it removes costs. And the estimate that justified building it was **5.9 M x ~300 ns =
5% of the boot**, where the 300 ns came from dividing the interpreter region (7 s) by its entries
(15.2 M). These entries are far below that average: they are the cheapest bodies in the engine,
which is exactly why they are the most numerous.

⇒ **The average cost of a population is not the cost of its cheapest members.** The tree's standing
rule says convert a count into a share of the boot before writing code; this is the refinement that
rule needed — convert it with a price measured on THAT population, not on the region containing it.

## ⭐⭐THE PARALLELISM FRONT DOES NOT EXIST — java's extra cores are its COMPILER

Every core-count comparison in this ledger has been read as "java uses three cores and we use one".
Three java boots on one box, same world, only the compiler configuration changed:

| java configuration | wall | cpu | cores | Done |
|---|---|---|---|---|
| default | 7.8 s | 23.0 s | 2.94 | 0.238 |
| `-XX:CICompilerCount=2` | 7.1 s | 15.9 s | 2.24 | 0.271 |
| `-XX:TieredStopAtLevel=1` (C1 only) | **6.6 s** | **11.6 s** | **1.76** | 0.213 |

**Halving the compiler halves the CPU (23.0 → 11.6, −49%), drops the cores from 2.94 to 1.76, and
the wall gets FASTER, not slower** (7.8 → 6.6). A workload whose wall improves when you take two
cores away is not using them for its critical path.

⇒ **java's boot is serial and its extra cores are compiler threads.** Matching its core count buys
nothing, and the "cores are worth up to 29% of our boot" estimate recorded earlier in this file is
**WRONG and withdrawn** — it assumed java's 3.03 cores were application parallelism.

This retires an entire front. What survives from it:

* `RAVA_GILFREE_NOLANE` is still +13.08% DISJOINT, so our deferral lane genuinely earns 13% — but
  by moving OUR OWN work off the critical path, not by matching a parallelism java has.
* The lane's small batches are not a defect either: the chaining family is already exempt from
  draining, only `thenCompose` forces one, and `[PC-DEFER] offered=135` against `drains=4 tasks=7`
  means MC simply does not offer more concurrent `supplyAsync` bodies between compose points. The
  other 128 are `Util.backgroundExecutor()` work that **the JVM does not run during boot either**.

⇒ **There is exactly ONE front, and it is work per core.** The reference to beat is java's best
configuration: **6.6 s wall / 11.6 CPU-s / Done 0.213**, against our 37 CPU-s — a **3.2x work
ratio** with no parallelism escape available on either side.

## ⭐The compiler is asked 1,951 times and says yes 98.2% of the time

`[CBAIL]` is new: the compile driver's five `bail!` stages were printed one line at a time under a
debug gate and never counted, so "which refusal keeps two thirds of all method entries in the
interpreter?" had no answer. It does now, on a product boot:

```
[CBAIL] 1951 compile attempts, 1915 COMPILED (98.2%), 36 refused
[CBAIL]   1.79%      35  exceptions
[CBAIL]   0.05%       1  emit_body
[BOOT-DISPATCH] 15038 distinct methods | interpreted entries=14,087,689 jit entries=7,589,919
```

⇒ **The compiler does not refuse — it is barely asked.** Fifteen thousand distinct methods have
entries and one thousand nine hundred fifty-one reach the compiler at all; of those it accepts
98.2%. That closes a whole family of hypotheses this ledger has carried for a long time:

* ⛔**"any method with a try/catch is never compiled" is true and worth nothing** — 35 methods a
  boot. The `exceptions` bail has been cited repeatedly as a structural blocker (datapack loading
  is exception-dense); it refuses 1.79% of attempts.
* ⛔**The 4,096 cap is not what binds either**, and neither is `find_leaders`, `resolve_fields` or
  `callback_heavy` — all three read zero.

And the residue is consistent with everything else measured today: 14.1 M interpreted entries over
~13,000 uncompiled methods, of which **88.5% belong to 24 methods** the per-class native refusal
keeps out of `jit_run` before a compile is ever attempted — and compiling those measures **+2.54%**
(`RAVA_NS_BYTECODE`), while serving them frame-free measures **−2.03%** (`RAVA_FLAT_ZEROTEST`).

⇒ Every path into and out of that population is now measured, and all of them cost. The tiering,
the compile gate, the cap, the exception bail and the interpreted residue are closed together.

## ⛔A LIVE CLIFF AT TIER THRESHOLDS 32-64, AND THE DEFAULT SITS ONE STEP BELOW IT

`RAVA_JIT_THRESHOLD` sweep, one product boot each, 200-300 s timeout:

| threshold | wall | CPU-s | cores | Done |
|---|---|---|---|---|
| 16 (tier-0 default) | 29.9 | 33.1 | 1.11 | 0.480 |
| **32** | — | **198.8** | 0.99 | **NEVER** |
| **48** | — | **198.0** | 0.99 | **NEVER** |
| **64** | — | **295.8** | 0.99 | **NEVER** |
| 128 | 30.2 | 33.8 | 1.12 | 0.446 |
| 256 | 30.1 | 33.7 | 1.12 | 0.745 |
| 1024 | 30.2 | 33.3 | 1.10 | 0.387 |

⇒ **The collapse is a BAND, not a direction**, and it is sharp: 16 works, 32 does not, 128 works
again. The ledger already carried "threshold 32 -> 400 s" as a single point; this brackets it, and
the bracket is what makes it usable. Note the cores column: inside the band the boot also drops to
0.99, so the deferral lane is not running either.

This is the same mechanism the ledger records as **partial coverage being worse than none** — 408
compiled methods measured worse than 0 (240 s against 121 s with the JIT off) because at partial
coverage almost every call crosses the seam, and a crossing is dearer than either side of it. A
threshold in 32..64 evidently lands the compiled set exactly in that regime for this workload.

Two consequences worth carrying:
* **The shipped default sits one step below a cliff.** That is not a reason to move it — 128, 256
  and 1024 are all measurably WORSE than 16 (33.8 / 33.7 / 33.3 against 33.1), so compiling cold
  methods aggressively does pay — but any future tiering change must bracket this band rather than
  sample one point.
* **Compiling less does not help.** The hypothesis this sweep was run to test — that speculation
  and compilation do not earn back their cost on a one-shot boot — is TRUE for speculation (the
  corrected profile hash gives 2.3x more emitter answers and costs +1.5%, disjoint) and FALSE for
  compilation itself. Those are different levers and the sweep separates them.

## Round 31 — the derive-once sweep: NEUTRAL, and a hole in my own gate run

Seven areas swept by the rule that won round 30 ("one fact per operation, derived once and
threaded"). Real repeated derivations were found and removed:

* `opcodes/invoke.rs` + `statics.rs` — `pop_args_n` moved N args out, `frame.pop()` lifted the
  receiver separately, then `locals.insert(0, receiver)` **memmoved the whole argument vector one
  slot right to re-open the hole step 1 had just closed**. JVMS 6.5 already lays the operand stack
  out as `objectref, arg1 … argN`; one `copy_nonoverlapping` of `N+1` now does it. Also
  `is_int_set(owner)` twice per `invokespecial` — and its population is *permanent*, not
  first-call: `java/lang/Object.<init>` resolves to `None`, never installs a plan, and javac emits
  it as the implicit super of **every constructor in the program**, so five 40-char string
  compares ran on every `new`, forever, to answer "no".
* `vm/jit_enter.rs` — "is any instrument armed in this process?" asked **4× per entry in 4
  spellings** across ~7.6M compiled entries. The *other* side of this same crossing had already
  folded its eight gates; the entry side never got the treatment.

**Measured, three ABBA passes, ON = kill switches set = block disabled:**

| arm | min | median |
|---|---|---|
| all seven | +1.97% | +0.31% |
| callpath + crossing2 | +6.02% | +3.77% |
| the other five | −6.45% | +0.60% |
| callpath + crossing2, re-run on the shipped default | +6.98% | **−2.06%** |

⛔**The pair's own median flipped sign between two runs of the IDENTICAL comparison.** That is the
box's known floor (the null test against a nonexistent flag once read 7.34% "disjoint"), so the
+3.77% is not a result. Round 31 ships as NEUTRAL: the pair stays default-ON because it is
strictly fewer operations per call (a memmove and three acquire loads deleted, bit-exact), and the
five go OPT-IN (`RAVA_FAST_FIELDDERIVE` / `_CASCADE5` / `_MAPDERIVE` / `_ARENADERIVE` /
`_EMITDERIVE`) because they measured neutral on their own and the arithmetic of the three passes
puts them on the wrong side.

⚠**METHODOLOGY DEFECT FOUND IN MY OWN PIPELINE:** `bench\pgo_gates.ps1` defaults to
`-Exe target\pgo-fat\rava.exe`. Run bare, it gates a PGO binary from **2026-08-20** and reports
PASS while verifying nothing about the build under test. `sem_all.ps1` defaults to
`target\release\rava.exe` and is unaffected. **Every gate run must pass
`-Exe target\release\rava.exe` explicitly.** Re-run correctly here: 4/4 hashes bit-exact
(`8790f04ace726ecf`, `37539c6c375d0c3` ×2, `ae44a34b42766bd4`), 21/21 probes match `java -Xint`.

## The ASM question, answered with a profile — and the first real concentration in many rounds

Asked whether anything is left that only hand-written assembly can fix. A full profile of today's
binary (`bench\boot_pc.ps1`, boot to `Done`) says **no, not at the leaf level**: the host table is
flat, top entry `mi_malloc_aligned` **3.2%**, then `jit_call_body` / `dispatch_owner_cascade` /
`NtCreateFile` at 2.5%. Hand-written assembly pays when one kernel dominates; none does. The
call-site table (`WALL-SITE`) is flat too — top 0.7%.

**But the activity split and the GUEST table are not flat:**

| activity | share |
|---|---|
| JIT_EXEC (Cranelift machine code) | 42.2% |
| NATIVE_BRIDGES (gson/R2OMAP/IO) | 22.8% |
| JIT_CALLBACK (PIC miss → host dispatch) | 18.0% |
| INTERP_EXEC | 11.8% |
| VM_DISPATCH | 3.7% |
| CLASS_LOADING | 1.5% |

and inside JIT_EXEC, **three guest methods hold 41% of all machine-code time**:
`BlockBehaviour$BlockStateBase.initCache` 21.1%, `WallBlock.lambda$makeShapes$0` 10.0%,
`StateHolder.lambda$populateNeighbours$0` 9.9%. Converted by the ledger's own rule: **≈17% of the
whole boot in three methods.** `initCache` runs once per block state (~28k). That is the first
concentration this engine has shown in a long time — every other axis measured flat.

### What the hottest method actually compiles to

`RAVA_JIT_DUMP=BlockStateBase` (the filter matches the CLASS name, not the method) writes
`clif.txt`. `initCache()V`:

* 2,068 real instructions, **1,409 value aliases**, value ids to **10,880**, 426 blocks
* 668 loads, 74 `call_indirect` + 37 `call` = **111 calls**
* load histogram `+0/+8/+16/…/+56`, counts decaying with offset ⇒ **argument marshalling through
  memory, ~6 loads per call** — not field access
* **101 `explicit_slot` totalling 18,976 BYTES of stack frame, every slot referenced exactly once**

⭐**MECHANISM: `callbuf` is minted PER CALL SITE at emit time** (`jit/emit/calls.rs:256`,
`jit/emit/dispatch_pic.rs:139`) and it *is* the callee's locals frame. Cranelift does not colour
explicit slots, so **a compiled method's frame is the SUM of every call site's callee buffer.**
A method with 111 calls carries 18.5 KB of frame — over half of L1d — and re-touches it on each of
~28k invocations.

The buffers are poolable: the emitter pops arguments as SSA values, so `f(g(x))` fully evaluates
`g` before `f` stores anything ⇒ buffer lifetimes are strictly nested, never overlapping; a `deep`
callee registers `(buffer, frame_len)` as a cell-root range and pops it on return. The hazard a
pool makes *worse* is the one this tree has already learned twice: a pooled slot holds the previous
site's bytes, and an uninitialised wide-argument pair slot inside a scanned range decodes as a raw
arena pointer. Round 32 is aimed exactly here.

## ⛔ HALF OF ALL PRODUCT BOOTS WERE FAULTING, AND THE A/B WAS SCORING THEM AS FAST RUNS

Found while checking why an A/B row carried `Done=0`. Six boots of the shipped default: **3 died**
with `0xC0000005`. Six more with this round's block disabled: **3 died** — same rate, so it was not
new. Four on `release-diag` for symbols: 3 died, every one identical:

```
[CRASH] 0xc0000005 at Vm::install_cap_thunk+0x1d2   jit_direct_call.rs:167
[CRASH]   compiled[0] com/mojang/serialization/DataResult$Success.ifSuccess(Consumer)
[CRASH]   read of 0x10000136720 / 0x100000a42f4 / 0x100000c38a7
```

Every faulting address is `0x100_0000_0000 | small` — `REF_TAG | handle`, since `REF_TAG = 1 << 40`.

⭐**CAUSE: `ArenaPtr::new` is `NonNull::new` — a NULL CHECK, NOT A SHAPE TEST.** The capturing-
lambda thunk path did `ArenaPtr::new(recv as *mut u8)` and then read the class word through it, so
a heap-`Ref` receiver — a cell with bit 40 set, an address in no mapping — was dereferenced. The
same file's `jit_direct_probe` shape-tests every one of its four arms, and
`jit::dispatch_table::note_serve` refuses this exact population in as many words ("not an arena
object: no class id to index by"). This path simply never got the test.

**Fix:** `is_arena_obj_addr(recv)` — the predicate every other decoder uses, bounding against the
live arena window — then `from_addr`, whose contract that predicate exactly satisfies. REJECT-only:
a non-arena receiver has no arena class id, so `is_lambda_cid` could never have admitted it.

**After: 0 of 6 boots died.** Gates 4/4 bit-exact, 21/21 probes.

⚠**AND THE MEASUREMENT CONSEQUENCE IS THE BIGGER FINDING.** A faulted boot **exits early**, so it
recorded a SHORT wall and a SMALL CPU, and `boot_ab_flags.ps1` averaged it in as though it were a
fast run. That is how this round's first reading came back **+15.55% min / +7.03% median** for a
change whose honest number is **+0.96% / +0.28%**. Any A/B in this ledger taken before this commit
may contain faulted boots on either side; a result whose arms differed by less than the ~8% floor
was never separable from them. The harness now waits on the whole `Done (…s)` line rather than its
prefix, retries a boot that never printed one, and DROPS the round if an arm cannot complete.

## Round 33 — the two mystery range tests, NAMED and PRICED (and one of them routes nothing)

The CLIF of the hottest method showed every call site emitting a baked base, a subtraction, and two
`icmp_imm ult` span tests before an indirect call. They are now identified, and the identification
is arithmetic rather than opinion: `pic_native::NativeEntry` is 8×`AtomicI64` + 2×`AtomicU32` =
**72 B**, so `leaf_span = 4096 × 72 = 294,912 = 0x48000` and
`pool_span = (4096 + 2 × 20480) × 72 = 3,244,032 = 0x318000` — exactly the two immediates.

* **Test A** `fptr − pool_base <u pool_span` — "is the word the 4-way PIC probe returned a
  `pic_native` host-thunk descriptor, or a compiled guest function pointer?" Emitted at
  `dispatch_pic.rs:443` (arena arm) and identically at `:624` (heap-Ref arm).
* **Test B** `t <u leaf_span` — LEAF thunk (pure read, caller's refs unspilled) vs ROOTED
  (spill → call → reload). `pic_helpers.rs:243`.

`[CALLSEQ]`, a new default-OFF census, prices them on a real boot:

```
3,202 compiled PIC sites emit test A on the arena arm; 3,202 (100%) emit a SECOND identical
      test on the heap-Ref arm
93,363,121 executions reached test A
  -> GUEST fptr   85,354,721 (91.422%)
  -> pic_native    8,008,400 ( 8.578%)   of which LEAF 0 (0.000%)  ROOTED 8,008,400
EMIT-TIME, test B: PROVABLY NEVER LEAF 3,132 of 3,202 (97.8%) | undecidable 70 (2.2%)
COST: 4 IR instructions on 100% of hits to route 8.578%; test B costs 1 more to route 0.000%
```

⭐**Test B routes NOTHING — 93.4 M executions, 0 leaf descriptors — and 97.8% of sites can prove it
dead at emit time** (the half is fixed at allocation, the only cross-half transition demotes and
never promotes, and a descriptor in site S's ways was allocated for S). Test A's 4 instructions run
on all 93.4 M hits to route 8.6% of them.

⛔**AND THE CODEGEN THAT ACTED ON THIS DID NOT PAY — ALL OF IT IS OPT-IN.** The bundle measured
−2.69% min / −2.01% median (disabling it was FASTER). Unbundled by per-area switch over 4 rounds
each: SSA/branch construction +2.52% / +4.27%, call-site emission +1.81% / +3.15%, natives crossing
−1.54% / −3.59%. So the bridges area was made opt-in and the other two kept — and then the deciding
**7-round ABBA (14 interleaved boots) on that surviving pair read −1.84% min / −1.21% median**,
the opposite sign to both of its members measured alone. ⇒ **two 4-round readings of overlapping
sets disagreed in sign with the 7-round reading of their union: that is the floor talking, not a
result.** Default follows the tightest measurement; every area is behind
`RAVA_FAST_CALLSEQ_BLOCKS` / `_EMIT` / `_BRIDGES`.

⭐**The round's product is the census, not the codegen.** Two facts now have numbers instead of
guesses: a dead test on 93 M executions, and 91.4% of PIC hits paying for a 8.6% route.

## ⛔ PARALLELISM RETIRED AS A LEVER — for the second time, now by the reference engine itself

`bench\jvm_cpu.ps1 -JavaOpts '-Dmax.bg.threads=1'` caps Minecraft's OWN background executor at one
thread, which separates guest parallelism from the JVM's compiler and GC threads in the cores column:

| java | wall | cpu | cores |
|---|---|---|---|
| default | 6.3 / 6.3 | 19.5 / 21.6 | 3.07 / 3.40 |
| `-Dmax.bg.threads=1` | **7.2 / 6.1** | 19.1 / 17.7 | 2.65 / 2.91 |

Wall does not move. ⭐**HotSpot boots this server in 6.3 s on ONE guest thread. rava boots it in
28.4 s on one guest thread. The whole gap is 4.5× of per-core work on the same serial path.** The
"cores 2.74×" term in the earlier decomposition was real arithmetic about a number that does not
shorten java's wall, so it said nothing about ours. Any round aimed at making more of our threads
run guest code is aimed at a factor the reference engine does not use.

Where one core spends 4.5× more, by activity: JIT_EXEC 42.2%, NATIVE_BRIDGES 22.8%,
JIT_CALLBACK 18.0%, INTERP_EXEC 11.8%. HotSpot has no seam (its equivalent of JIT_CALLBACK is
zero), runs the real JDK under C2 where we run Rust replacements, and inlines where we call — the
hottest method still makes 111 calls.

## ⛔ BLOCK-STATE IDS WERE WRONG ON 238 OF 1,166 BLOCKS — found by the new instrument on its first run

`bench/BootstrapBench.java` (new) runs Minecraft's own `Bootstrap.bootStrap()` — the registry
`<clinit>` chain, the block-state graph and `initCache`, i.e. the code that holds 41% of machine-code
time on a real boot — times it inside the guest and folds every block state (sorted property values,
default flag, six `initCache` scalars) into one FNV-64. Same bytecode, both engines:

| | BOOTSTRAP_MS | STATES | HASH |
|---|---|---|---|
| java | 3,277 | 29,671 | `b9e7debe631b42b6` |
| rava, before | 15,522 | 29,671 | **`642e711e8034cd1a`** |
| rava, after | 15,324 | 29,671 | `b9e7debe631b42b6` |

`--dump` (per-block hash) and `--dump-block <key>` (per-state) localised it in two diffs: **238 blocks
differed, and in every one the SET of states and every `initCache` scalar were identical — only the
ORDER of `getPossibleStates()` differed.** That order is the block-state protocol id (network palette,
chunk storage), so a client would have seen scrambled bed/propagule/note-block states.

⭐**CAUSE: `map/dispatch.rs` served `ImmutableSortedMap.copyOf` with the same arm as `ImmutableMap.copyOf`
— a `LinkedHashMap` filled in SOURCE order** — and the native map layer decides sorted iteration by the
object's CLASS (`is_sorted_map` in `map_method.rs`). `StateDefinition` builds the cartesian product by
iterating `ImmutableSortedMap.copyOf(properties).values()`. Every earlier probe inserted keys already
alphabetical and could not see it. Fix: the sorted factory allocates a `TreeMap` (same storage, sorted
verdict attached). New permanent probe `bench/SortedSem.java`. Gates 4/4, 22/22 probes.

⭐**AND THE INSTRUMENT'S FLOOR IS ~0.2%, NOT 8%**: `bootstrap_ab.ps1 -Flag RAVA_NULL_TEST -Pairs 4`
reads cpu-ms median 0.22% / cycles 0.17%, arm spread 2.0-3.4%. Every 1-4% claim this month was made
with an instrument that could not see it; from here, levers are priced on this bench first.

## The first sweep on the 1% instrument: 19 blocks re-priced, three real, sixteen inside the floor

`bootstrap_ab.ps1 -Pairs 3` over every kill-switch / opt-in flag of the last four weeks (B = flag set;
three rows per flag = guest ms / cpu-ms / cycles):

| flag | min B vs A | median B vs A | verdict |
|---|---|---|---|
| `RAVA_NO_CAP_THUNK` | +6.3 / +6.2 / +6.1% | +6.6 / +6.0 / +6.0% | **DISJOINT ×3 — the capture thunk is a real 6%** (shipped ON) |
| `RAVA_FAST_MAPDERIVE` | +3.3 / +2.9 / +3.2% | +7.5 / +4.8 / +5.6% | **real harm** (opt-in, stays off) |
| `RAVA_NO_DT_LEARN` | +2.3 / +3.8 / +2.6% | +2.0 / +1.2 / +1.5% | dispatch-table learning pays ~2% (shipped ON; the "0 probes" counter is measuring something else) |
| `RAVA_FAST_FIELDDERIVE` | −2.3 / −1.2 / −2.1% | −2.2 / −2.3 / −1.9% (one DISJOINT) | **re-run at 6 pairs: +0.16 / +0.17 / +0.51%, 0/6 pairs — NEUTRAL** |
| `RAVA_JIT_NS_BYTECODE` | −0.6 / −1.0 / −1.1% | −1.3 / −1.6 / −1.2% (one DISJOINT) | **has no reader — this is the 3-pair FLOOR** |
| `RAVA_ROUND7`, `_ARENADERIVE`, `_CROSSING2`, `_SHARED_CALLBUF` | −0.2…−1.1% | −0.4…−2.1% | inside the floor |
| the other ten | ±1% | ±1% | inside the floor |

⭐**Calibration: at 3 pairs a nonexistent flag can read −1.3% median with one DISJOINT metric; at 6
pairs a "−2% disjoint" block read +0.2%.** So the bench's working floor is ~1.5% at 3 pairs and ~0.5%
at 6, and a single DISJOINT row is not evidence — three agreeing rows are. Nothing flips: the two
real wins were already default-ON, the one real harm already opt-in. What changed is that sixteen
verdicts made with a blind instrument are now known to be verdicts about noise.

## ⛔ java.util AS COMPILED BYTECODE: >10× SLOWER on the bootstrap — the clean comparison, finally run

The ledger's +8.46% verdict was void (the override ran INTERPRETED — `native_served_prefix` matched
`java/`, so the class never reached the compiler). With `has_bytecode ⇒ compilable` now in the tree, the
comparison it meant to run: `jdk-out` (ArrayList + Objects from `jdk/`, `javac --patch-module`) first on
the classpath of `BootstrapBench`.

Native: **15.0 s**, hash `b9e7debe631b42b6`. Override: **did not finish in 150 s**, twice. Not hung —
crawling: `[PC-OPS] 12.4M opcodes in 30 s = 2,419 ns/op`, `[PC-ARM] last-arm hit 4,602 / miss
1,959,008 (0.2%)`, `[CBAIL] 371 compiled, 3 refused` — the override IS compiled, and the boot is still
>10× slower. That is the half-migration hazard: every native that receives a guest `ArrayList` now
drains it, and every guest `ArrayList` method reached from a native crosses the seam per element. The
per-element win (5.0×, 12.2×) was real and the boot loses anyway. **Line closed; the instrument
priced it in one run instead of a session.**

⚠ Also: `timeout(1)` under MSYS does not kill the Windows child. A hung `rava.exe` kept the CPU
through two later measurements (a native run read 44 s). Kill by PID with `taskkill /F /T`, never
`java`.

## The host-row wave: two disjoint wins on the bench, and the void+wide miscompile named

Aimed by the bench population's `[COMPILED-SPLIT]` (74.1% of samples under compiled bodies are host
callbacks), one agent per top host row, each behind `RAVA_NO_HR_<AREA>`. Priced with
`bootstrap_ab.ps1 -Pairs 3` (B = area disabled; rows = guest ms / cpu-ms / cycles):

| area | min B vs A | median B vs A | |
|---|---|---|---|
| `INDY` | +5.27 / +4.31 / +4.61% | +5.00 / +3.29 / +2.89% | **DISJOINT ×3** |
| `SYNTH` | +4.43 / +4.92 / +5.16% | +3.26 / +4.13 / +3.94% | **DISJOINT ×3** |
| `STRINGS` | +0.54 / −0.19 / −0.21% | +1.96 / +2.35 / +2.27% | candidate |
| `CAPVOID` | +0.28 / −2.07 / −0.12% | −1.32 / −1.76 / −1.38% | fix kept; admission priced below |
| `COMPARE` | +0.77 / +1.34 / −0.10% | +1.61 / −0.74 / −0.81% | neutral |
| `ALLOC` | −1.63 / −1.02 / −1.26% | +0.32 / 0.00 / −0.18% | census only, neutral |

⭐**INDY — the dominant refusal was never inside the mint.** The inline lambda mint already converts
array captures correctly (JIT `h|1<<42` → arena `(h<<2)|2`); it was never *attempted* at the hot sites
for two reasons no counter could see: (1) `ops/invoke_dynamic.rs` only calls the mint when the method
has a hoisted TLAB cursor, and `body.rs` hoists one only for methods with an arena `new` — none of
`BitSetDiscreteVoxelShape.lambda$join$1/2`, `Shapes.joinIsNotEmpty`, `VoxelShape.forAllBoxes` contain
a `new`; (2) `lambda_shape::record` refused more than 8 captures and `lambda$join$1/2` have 12 and 10
— while the `KINDS` word had 14 nibbles free. Now: a site-cursor arm (one idempotent `jit_cursor_addr`
leaf call past the guard, the discipline late `new` already uses) and a 14-capture bound. `[PC-INDY]`
counts every refusal by reason.

⭐**SYNTH — 39,344 of 58,117 synthetic-frame fall-throughs were for callees that will never have an
fptr.** Compiled code → `jit_direct_resolve` → refusal → synthetic interpreter frame →
`invokevirtual` handler → native. Now the four refusals whose interpreter twin is "no body →
`try_invoke_chain`" mark a thread-local channel; the negative cache gains a `NEG_NATIVE` bit (zero
new bytes); a hit builds a per-`(site, cp, recv_guard)` `NativePlan` once (owner/name/desc, super
chain, per-arg decode kinds, ref-return) and dispatches the native directly with the seam's own
rooting, no frame.

⭐⭐**CAPVOID — the "void AND wide" refusal was hiding an indexing bug, not a void bug.** The thunk is
installed into the PIC hit arm, which hands the callee a buffer laid out in **JVM slots** (a wide arg
= its cell + a zeroed pair slot); only `rava_dispatch_miss` gets a *packed* copy. The thunk read SAM
argument `j` at `buf[(1+j)*8]` — packed arithmetic on a slot-laid buffer — so `(DDDDDD)V` received
`d0, 0, d1, 0, d2, 0`, and `Shapes.box` threw exactly "The min values need to be smaller or equals to
the max values". A single *trailing* wide argument reads correctly by coincidence (its pad is never
read), which is why "every wide mask among non-void callees" passed the gates and the corner was
labelled VOID. Two more rode along: the bail arm passed the slot-laid buffer to the packed-ABI miss
callback, and the thunk's own wide pair slots were never written inside a range the collector scans.
Fixed via a `SiteLayout` derived from the site's own descriptor with the same `arg_slot_widths` the
PIC emitter used. ChunkFill `8790f04ace726ecf` holds with the corner admitted.

Gates 4/4 bit-exact, 22/22 probes, bench hash `b9e7debe631b42b6`, 14,947 ms.

### Confirmed at six pairs, and what the full boot can and cannot see

| area | min B vs A | median B vs A | pairs favouring disable |
|---|---|---|---|
| `INDY` | +8.78 / +6.59 / +7.70% | +8.88 / +8.25 / +8.75% | **0/6** |
| `SYNTH` | +4.33 / +3.95 / +4.14% | +4.36 / +3.64 / +3.01% | **0/6** |
| `STRINGS` | −0.67 / +1.33 / +0.94% | +2.54 / +3.14 / +3.55% | **0/6** |
| `CAPVOID` | +1.05 / +3.01 / +0.81% | −1.64 / −1.13 / −1.17% | 1/6 — neutral; kept as the miscompile fix |

Three real wins, ~15% of the bootstrap population together. That population is 56% of the boot, so
the expected boot-level effect is ~8% — exactly the full-boot harness's floor. Product ABBA on the
three switches, 4 rounds: min −3.73%, median +0.72%, walls 28.5–34.6 s in BOTH arms, zero faulted
boots. ⇒ **the boot harness cannot see an 8% change today; the bench sees a 3% one at 0/6 pairs.**
From here the boot is the correctness/stability gate and the bench is the price.

### Two binaries, interleaved — the wave is real, and absolutes drift 8% across a day

The shipped build's absolute bench level (15.0–15.4 s) looked unchanged from the morning, which would
have made the A/B a measurement of the kill switch's cost. The decisive test is the previous commit
(`7fb5c19d`, built to `target-prewave/`) interleaved with the wave on the same bench:

```
PRE  16,554   WAVE 15,086
PRE  16,515   (then a foreign load hit the box: 35,489 / 31,387 / 33,861 — discarded)
```

⭐**−9% on the bootstrap, two clean pairs.** The morning's "15.0" was already a wave build; the
pre-wave binary reads 16.5 in the afternoon's box state. Absolute numbers drift 5–8% across a day;
only interleaved pairs mean anything — and the "disabled" arms (16.2–16.6) sit exactly at the pre-wave
level, so the switches do restore history. `target-prewave/` stays as a pinned reference binary.

## After the wave: the bench's own mechanism, by census

Post-wave bench profile: `[COMPILED-SPLIT]` 37.5% of compiled-body samples in emitted code (was 25.9%),
62.5% host. The host rows are flat again (top `Vm::compare` 2.0%) — but the censuses are not:

```
[PC-BIRTH] rava_new callbacks 2,695,047: arena 0 (0.0%) | handle 2,695,044 (100.0%, one Box<Object> each)
[PC-BIRTH]   site at callback: poisoned(permanent) 2,695,023 (100.0%)
[PC-BIRTH]   arrays from compiled code (never arena, one host Vec each): newarray 3,029,533  anewarray 1,150,369
[PC-BIRTH]     handle-born  92.02%  2,480,066  net/minecraft/world/phys/shapes/CubePointRange
[PC-MALLOC] allocs=41,547,884      [PC-HANDLE] objects born as heap handles: 2,119,493
```

⭐**One class is 92% of all handle births on the boot-critical path.** `CubePointRange extends
AbstractDoubleList` (fastutil), one `int parts`, `getDouble(i) = i / parts`. The birth predicate
(`native_gate::birth_native_backed_alloc` via `Vm::birth_native_backed_ancestor`) forces a HANDLE for
any subclass of a native-backed collection; the JIT side (`native_backing.rs`) already exempts exactly
this "pure value-computer" shape from native serving. The two predicates disagree about one class, and
that disagreement is 2.48 million `Box<Object>` + `FieldBlob` births, every field read of which goes
`rava_getfield → class_field_layout → slot_of` through the host — the 4.2% cluster at the top of the
host table. Arrays from compiled code are a second population: 4.18 M host `Vec`s, `tlab_alloc_array`
has zero call sites. Together they are the 41.5 M mallocs.

Also read off the same run: `[PC-INDY]` 2,097,194 mints inline (1.9 M via the new site cursor),
282,962 still refused as `undescribed`; `[HR-SYNTH]` 3,212,449 natives served without a frame;
`[PIC-COV]` fall-throughs 18,961, 99% tier lag (was 67% native-served).

## Wave 2: the value-computer birth, a park on the compile queue, a shape table that lost collisions forever, and `_NLG_Return2` unmasked

**VALUECOMPUTER** (`RAVA_NO_VALUECOMPUTER`). Two disagreements, not one: the birth sites asked
`birth_native_backed_ancestor && !overrides_collection_backing`, and the override was opt-in
(`RAVA_ARENA_OVERRIDE`) — *and inert even when set*, because `class_field_layout` marks a layout PARTIAL
at the first unloadable super (`AbstractDoubleList → AbstractDoubleCollection → java/util/AbstractCollection`,
unloadable: `jdk/` holds only `ArrayList`/`Objects`), and every birth site falls back to a handle on
exactly that absence. Now ONE structural predicate, memoised per class: loadable, concrete, not
enum-shaped; every own instance field primitive; declares `size()I` and an indexed accessor chosen by
DESCRIPTOR `(I)<non-void>`, never by name; every loadable strict ancestor abstract and fieldless, the
chain's first `NotFound` link native-backed; all six `Collection` mutators have bodies in the chain (no
mutator can reach a native that fabricates `$elems`); no `Fieldref` in the chain names a field the
chain does not declare. It admits the class at birth AND completes its layout at the same link, and
`coll_op` gains an arena-receiver arm (`isEmpty`/`toArray` via the class's own bytecode).
Census on the bench: **`rava_new` callbacks 2,695,047 → 213,133; `[VALUECOMP]` 100 verdicts, 3 admitted**
(44 refused for a reference field, 27 abstract, 15 unloadable, 10 no `size()`, 1 enum-shaped).

**WAIT** (`RAVA_NO_HR_WAIT`). The park is `BackgroundJit::take_blocking` = `results.recv()`, reached
only from the `Compiling` arm of `force_compile`: the main thread asked for a body that was ALREADY
queued behind up to 7 jobs in a depth-8 FIFO in front of one compiler thread, and slept for the whole
makespan. The dispatch-tier caller (64th miss from compiled code) documents `NotYet` as its own
fallback and does not need the body now — so it keeps going and picks the fptr up on a later passage.
`[HR-WAIT] parks=0 | deferred=4,216 (dispatch-tier 4,214, flat-hot 2)`.

**UNDESC** (`RAVA_NO_HR_UNDESC`). The recorder does run on the compiled path; sites stayed undescribed
for two structural reasons that could never converge: (1) the shape table was direct-mapped, 4,096 rows,
`compare_exchange(0, v)` first-writer-wins across DIFFERENT sites — the second site into a row was
refused forever while the emitter had baked that row's address; (2) `StringConcatFactory`,
`ObjectMethods` and `SwitchBootstraps` indy sites were given the lambda mint guard, and the recorder
lives in the `LambdaMetafactory` arm only, so they refused `undescribed` on 100% of executions.

⭐⭐**UNWIND — `_NLG_Return2` IS `memcpy`.** The sampler's symbol path is the exe dir only, so
`vcruntime140.dll` resolves by nearest preceding EXPORT: `__NLG_Return2` is one `ret` at RVA 0x1bc30
and the 0x1370 bytes after it are the `rep movsb`/SSE/AVX copy bodies the loader retargets `memcpy`/
`memmove` to; `memset` and `memcmp` land inside the `memmove` export's gap. **Every `_NLG_Return2` row in
this ledger since July was `memcpy`, every `memmove` row was `memset`+`memcmp`, and "SEH unwind glue" was
never on any path** — nothing installs an unwind frame per call (0 `catch_unwind` on the entry or
crossing). The one member of that family on the entry path: `write_bytes(p, 0, total)` with a run-time
length = one `memset` libcall per compiled entry, ~7.6 M a boot — now constant-size inline stores.
`[HR-UNWIND] 2,325,504 entries zeroed inline`.

**Priced on the bench, 3 pairs (B = area disabled):**

| area | min B vs A | median B vs A | |
|---|---|---|---|
| `VALUECOMPUTER` | +1.66 / +4.26 / +2.73% | +2.04 / +2.95 / +2.85% | **DISJOINT ×3 — shipped ON** |
| `UNWIND` (memset → inline stores) | +0.59 / +0.58 / +0.64% | +1.05 / +0.09 / +0.06% | small, consistent sign — shipped ON |
| `UNDESC` | −3.00 / −1.32 / −2.04% | −1.42 / −0.56 / −0.99% | one DISJOINT the wrong way — **opt-in** `RAVA_FAST_HR_UNDESC` |
| `WAIT` | −0.65 / −1.40 / −1.52% | −1.26 / −1.77 / −1.43% | not parking is SLOWER single-threaded — **opt-in** `RAVA_FAST_HR_WAIT` |

Only ~3% for moving 2.48 M objects out of `Box<Object>`: the arena-born `CubePointRange` still has its
`getDouble`/`size` served host-side (`CUBE_CLASS_WORD` is published only from a `Value::Ref`, so the
inline arm never arms for arena receivers) — the allocation was the smaller half of that object's cost;
the call is the larger, and it is the next target. Gates 4/4, 22/22 probes, hash `b9e7debe631b42b6`.

## ⛔ WAVE 2 HUNG THE PRODUCT BOOT — and the hang was a compile loop nobody had a counter for

The boot A/B on wave 2 ran 55 minutes for seven boots. Every default-arm boot stopped after the oshi
report and never printed `Environment:`; `RAVA_NO_VALUECOMPUTER=1` completed (`Done 0.880`). Yet
`BootstrapBench --validate` (both of the server's next two calls) completed under rava, and an
interpreter-only boot completed too (`Done 1.388` at a 400 s cap — my first "hangs without JIT too"
reading was a 90 s cap on a 150 s interpreted boot). So: JIT-only, server-only, value-computer-dependent.

Naming it took three instruments in sequence. `[REGION-TIME]`: seam 82.6%, 20.6 M calls out of
compiled code, 100 k interpreted opcodes in 30 s — the guest was calling out, not computing.
`RAVA_PC_TRACE`: Cranelift's share of samples went **3% in the first 4,000 → 54% in the last 4,000**
(per second: 0% … 0%, then 45 / 64 / 65 / 70 / 67%), while `[FORCE-WHY] attempts=132 COMPILED=132`
said the method compiler was NOT looping. `RAVA_PROFILE_PC_STACKS=ion3run`: **`cap_thunk::build_site_thunk`
under 12.3% of all stacks.**

⭐**MECHANISM.** `install_cap_thunk` keys the 4-way PIC on the receiver's arena CLASS id; every lambda of
one capture signature shares that id; the thunk itself then compares the lambda's SITE index against
the one baked into it. A call site that sees two lambda sites of the same signature therefore
ping-pongs: guard mismatch → miss → **a fresh Cranelift build** → install over the same way → the other
lambda arrives → mismatch → build… Each round is a whole compile. Two waves opened it together: arena
births made more lambdas share a capture signature (a `DoubleList` capture is now an arena pointer
kind, not a handle kind), and the void+wide corner admitted exactly those lambdas' thunks. Neither
alone hung; the bench never sees it because the alternating sites live in the world-loading path.

Fix (`cap_thunk::site_memo`): one thunk per `(site, lambda site)`, reused, never rebuilt; a way that
flips between lambda sites more than twice is refused for good. `Done 1.175`.

⭐**RULE: any install path that can run a compiler must be memoised by the key it installs under, and
must have a flip counter — a cache way that alternates is a loop with a compiler inside it.**

## Wave 3: all three opt-in — the value-computer's CALL is not cheaper as bytecode

Priced on the bench, 3 pairs (B = area disabled):

| area | min B vs A | median B vs A | |
|---|---|---|---|
| `CUBECALL` (structural arm; decline to the compiled accessor) | −2.71 / −2.94 / −1.93% | −4.78 / −4.02 / −3.71% | **two DISJOINT the wrong way → opt-in** `RAVA_FAST_CUBECALL` |
| `ARENA_PRIMARRAY` (Vec-backing pool) | +0.64 / +1.08 / −0.75% | −0.39 / −1.40 / −1.91% | neutral → opt-in `RAVA_FAST_ARENA_PRIMARRAY` |
| `CMPCALLER` (compare caller census + direct sort) | +2.70 / +3.32 / +2.64% | −0.71 / −0.53 / −0.65% | signs disagree → opt-in `RAVA_FAST_CMPCALLER` |

CUBECALL is the instructive one: declining the `fu_fastpath` callback into the class's COMPILED
accessor (a `jit_direct_call` through the per-site cache, with the receiver profile written on the way)
costs more than the historic `i / parts` formula arm, and the inline arm only arms after a recompile
with a site profile in hand — so on a 15-s run most calls pay the decline. The name-keyed
`fk::CUBE_RANGE` / `CPR` / `CUBE_CLASS_WORD` arms therefore stay in the default path; they are a
standing NO-ADAPT violation, priced at ~4% to remove this way. The two censuses (`[PC-ARRAYS]`: 2.31 M
array births per run, 70.6% retired within the run, `ref` 1.63 M of them with 976 k of length 1–4;
`[CMP-CALLER]`) are the round's product.

Product boot on the shipped binary: **wall 27.4 s / cpu 30.7 / cores 1.12 / Done 0.362** against
java 6.9 / 21.5 / 3.13 / 0.223 (same session, back to back). 4/4 stability boots complete.

## ⭐ THE LARGEST NUMBER LEFT: the datapack reload is 135 bodies run one after another by main

Phase stamps, same session, both engines:

```
                      rava        java
start → Environment   ~17 s      ~5 s    (3.4×)
Environment → recipes 13–17 s     2 s    (6.5×)   ← Minecraft's datapack RELOAD
recipes → Done          1 s       0 s
```

The whole-process sampler shows **1.00–1.26 live threads in every 3-second window of the boot**. The
reload is Minecraft's own parallel stage — per listener `supplyAsync(prepare, backgroundExecutor)
.thenCompose(barrier::wait).thenAcceptAsync(apply, gameExecutor)` — and the censuses say what we do
with it:

```
[PC-OFFLOAD] supplyAsync seen=135 → queued=1 refused=0 executor-unproven=0
[PC-DEFER]   offered=135 | DEFERRED=135
[PC-LANE]    drains=4 tasks=7 mean=1.75 max=3
```

**135 prepare bodies are deferred and then executed by the main thread at the join, serially, under the
GIL.** The gilfree lane ran 7 tasks the whole boot (the chunk ring). On the JVM the same 135 bodies run
on ~3 cores in 2 s; ours are ~5× dearer each and serialized, hence 13–17 s = **42% of our wall**.
Even 2× overlap is worth ~−20% of the boot — larger than every lever measured this month combined, and
above any noise floor. The `-Dmax.bg.threads=1` experiment did not refute this: it showed the reload's
parallel part is small in ABSOLUTE terms on the JVM (each body is cheap there), not that it is small
on ours.

Ruled out on the way: more compiler threads (`RAVA_BG_JIT_THREADS=4`: +3.9 / +5.5 / +5.6%, DISJOINT ×3
slower — more threads compile more cold code); main's parks on the compile queue total 165 ms of a
16 s bench (1%).

### ⛔ CORRECTION, by the map wave that was launched to design the fix

Five readers plus a synthesizer re-read the deferral path, the GIL, the heap, the natives and the 135
bodies (`wf_b96d0c37-f62`). The paragraph above overstated the prize on three counts, each settled by
source:

* **Of the 135 deferred bodies, 7 ran** (batches 3/1/2/1, `r32.log.err:1470,2029,2033,2036`); the other
  128 are the stronghold ring — never observed, never run, exactly as on HotSpot.
* **"java runs the reload on ~3 cores" is wrong** — its extra cores are C1/C2; `java -Xint` boots at 1.22
  cores (this ledger already knew that and the paragraph above forgot it).
* **The reload window's parallel ceiling is ~1.55 s**, triangulated three ways (sum−max of the measured
  prepare times ≈ 1.5 s; the bodies report's DAG; IO 0.58 s/boot): a 5 s synchronous prefix (tags +
  worldgen registries, serial on the JVM too), then a DAG-serial chain, then a phase where only
  recipes (1.54 s) can hide under advancements (2.73 s). **≤5.7% of the boot with infinite engineering.**

Candidate verdicts: the gilfree lane cannot host gson prepares (every native/uncompiled callee runs
under `VM_RESOLVE_LOCK` for its whole execution, `jit_call_body.rs:452-535`; the loot batch died at
callback 6-7 on a handle-receiver `putfield`); true concurrent guest threads need the heap report's
7-item list plus off-main class loading/`<clinit>`/JIT plus locks on every handle-collection mutation —
weeks, for ≤5.7%. The one correct-by-construction off-GIL work in a prepare body is pure-Rust: `fs::read`
and the gson tokenizer — a ~40-line tracer worth ≤0.4 s, i.e. ~1.5% of the boot, invisible to the
full-boot harness and outside the bootstrap bench. ⇒ **The window is 42% of the wall, but it is
per-body cost, not serialization: block 4 is JIT_EXEC 33% / NATIVE_BRIDGES 29% / JIT_CALLBACK 23% /
INTERP 10%. It needs its own instrument before it needs a lever.**

## The reload window, decomposed by a read-only wave (5 reports, synthesised by hand)

Two corrections to this ledger first:

⭐**"JIT_CALLBACK 23%" is only the invoke-shaped fifth of the seam.** The category is armed in ONE place
(`vm/jit_call.rs:60`), so every exit that does not route through `jit_call` — getfield/putfield,
getstatic, `new`, arrays, instanceof, indy, ldc, the String reads, the map bridges, card_mark,
safepoint, throw: 60-odd of the 76 bridges — is billed to JIT_EXEC instead (`pc_profile/dump.rs:482`).
RIP sampling of the window says **7.8% of samples are inside compiled code, 92.2% host**;
`[COMPILED-SPLIT]` says 18.2% emitted / 81.8% host callbacks. ⇒ **exits from machine code are ~45-48%
of this window, not 23%**, and only ~8 of the 33 "JIT_EXEC" points are machine code.

⭐**The gson intercept fires on every file, and the note saying it probably never fires is wrong.** All
three JSON drivers (`SimpleJsonResourceReloadListener.scanDirectory`,
`RegistryDataLoader.loadElementFromResource`, `TagLoader.load`) carry exception tables, and
`jit/runtime/driver.rs:393` bails on `exceptions` — **they can never compile**, so the `invokestatic`
intercept at `opcodes/invoke.rs:196` is reachable for all 6,351 files.

### The counted shape of the window

* **6,351 JSON files, 4.98 MB, avg 784 B, zero non-ASCII.** 148,994 nodes; 107,274 object members over
  **1,731 distinct keys — each key text materialised 62× on average**; 62,557 string values over 7,952
  distinct (7.9×), 95% identifier-shaped; median JsonObject has 2 members.
* **The text exists 4× at once** (Rust inflate buffer → guest `byte[] $buf` → transient refill Vec →
  gson's `char[1024]`), plus one real `java.lang.String` per token with **no interning** (`RStr` =
  `Arc::new(s.to_string())` = 2 mallocs, `value.rs:111`). And a guest `char[]` is stored as
  `ArrayData::Int`, **4 bytes per char** ⇒ 4,096 B per file × 6,351 = **26 MB of char buffer**.
* **`Map$Entry` is 48.11% of the boot's 6,234,501 handle births ≈ 3.0 M entry handles** (~0.97 s of pure
  birth at 324 ns). Every `entrySet()` materialises 2 `Vec`s at array CAPACITY + a Vec of pairs + one
  `Map$Entry` object per mapping + 2-3 field writes + **an `Arc<String>("entry")` allocated per call**.
  Measured in-tree: **6,115 ns net on a FOUR-entry map against 0 on the JVM — "the largest single-native
  gap in that table"** (`map_method.rs:850`). ~2.9 M `reference2ObjectEntrySet` calls a boot = 4.52% of
  the 64,088,346 compiled-to-host crossings. The view is cached; `iterator()` re-copies its array anyway.
* **Registries:** 39 `MappedRegistry`, 1,293 registers = **14,223 container ops, 1,293 `Holder$Reference`,
  2,586 `Identifier`-keyed probes**; 24,442 id-shaped values over **4,939 distinct (5:1)**. `Identifier`
  has NO native — `parse` runs `isValidPath`/`isValidNamespace`, which are **two host String calls per
  character**, and `substring` returns a boxed `Value::Str`, so the JIT's inline `charAt`/`length` arms
  (which require `emit_is_arena(recv)`) never fire.
* **Codecs:** `DataResult$Instance` overrides only `ap/ap2/ap3`, so arity ≥ 4 halves recursively —
  `Optional` allocations per record decode: arity 4 → **12**, 8 → **24**, 16 → **48**, purely to answer
  "are all present?" about values it already holds. ~**20 dynamic dispatches leave compiled code per
  decoded field** against ~0-1 on HotSpot (all callees are 4-30 bytecodes, far under FreqInlineSize).
  A lambda apply from machine code costs **134 ns** compiled / **999 ns** via flatcall, where **the same
  call shape through a normal class costs 9 ns** (`vm/invoke.rs:387`). 2,007,617 general-path arena-lambda
  applies vs 11,532 direct; ~13.2 M dispatch-miss host falls with a lambda receiver, 6.2 M "directable".
* ⭐**A one-line defect found on the way:** `nextgen/lambdas.rs:265` `lambda_class_id_for` opens
  `let site = u32::MAX;` and **discards its `_site_idx`** — the lambda PIC cannot tell two sites apart.

### Ranked levers for this window (my synthesis; nothing built yet)

| # | lever | population | why it should pay |
|---|---|---|---|
| 1 | **Intern token strings at the Rust tokenizer** | 194 k materialisations → ~15 k distinct | kills 2 mallocs each AND makes the strings arena-resident, which is the precondition the inline `charAt`/`length` arms need — so it also pays `Identifier.parse`'s two-host-calls-per-character |
| 2 | **Stop minting `Map$Entry` objects** | 3.0 M handle births, 48% of all | the arena machinery exists but is gated OFF (`RAVA_ARENA_NATIVE_NEW`, `RAVA_ARENA_SYNTH`); a consumer that does `getKey`/`getValue` immediately needs no object at all |
| 3 | **Serve `Optional` without allocating** | 12-48 per record decode | it is a two-state wrapper the codecs create and consume in one scope |
| 4 | **`lambda_class_id_for`'s discarded site** | 6.2 M directable misses | one line; the reason a lambda site cannot be keyed |
| 5 | **`char[]` as `u16`, or skip the guest char buffer entirely on the intercepted path** | 26 MB per boot | the intercept already has the bytes in Rust |

## ⭐ BOTH ENGINES NOW AGREE BYTE-FOR-BYTE ON THE WHOLE DATAPACK RELOAD — and the fix is worth 6%

`ReloadBench` under rava: `RELOAD_MS=15315 RECIPES=1470 ADVANCEMENTS=1584 TAGS=625
HASH=c956377e19071d7a`; java prints the same hash. That covers every registry id and tag, all 1,470
recipe JSONs through `Recipe.CODEC`, all 1,584 advancement JSONs, every function and every root
command. The one capability difference is now excluded and reported instead of hashed: `Commands.<init>`
registers `/jfr` only when `JvmProfiler.INSTANCE.isAvailable()`, true on a HotSpot with `jdk.jfr` and
false on any engine without Flight Recorder — the bench prints `JFR=true|false`.

Getting there found five real defects, each with its own gate:

* ⭐**A boxed `Character` had no identity.** `Character|Short|Byte.valueOf` returned the primitive
  unchanged (`cascade_table.rs:267`) where `Boolean` boxes into a real object, so `getClass()` said
  `Integer` and every render of the OBJECT printed the code point: a shaped recipe encoded as
  `"key":{"35":…}` instead of `"key":{"#":…}` — wire-visible. Now a real interned box (`boxid.rs`, the
  JDK's own ranges: Character 0..=127, Short/Byte -128..=127) with class-checked unboxing at the
  choke points (`argcall`'s descriptor mask widened `Z`→`Z|C|S|B`, `invoke.rs:964` return unboxing,
  `map/hashing.rs` + `map/keys` by-value keying). `Character` had **no `hashCode` arm at all** — it fell
  to `Object.hashCode` — and `equals` was untyped, which the arm's own comment had blamed on "the value
  model". ⭐**Disabling all of it (`RAVA_NO_BOXID=1`) measures +6.70/+5.30/+6.20% min, +7.66/+6.56/+6.10%
  median on the bootstrap bench, DISJOINT ×3** ⇒ the correctness fix is also a ~6% win.
* **`addAll` on a fastutil hash set was a no-op that returned `true`.** `AbstractCollection.addAll` has
  no bytecode here, so it fell to `coll_op` and wrote `$elems`, which the jar's own `size`/`iterator`
  never read. `TooltipDisplay.CODEC` builds `hidden_components` through it, empty made the record equal
  to `DEFAULT`, `optionalFieldOf` elided the field — **two advancements silently lost
  `minecraft:tooltip_display`**. Now run through the receiver's own `add` bytecode when
  `recv_declares_add` proves it owns its state (`RAVA_NO_JAR_ADDALL`).
* **`JsonObject.equals` was always true.** `drain_map`'s generic tail used `iterate_object`, which sees
  nothing in gson's `LinkedTreeMap$EntrySet` (nodes linked through `next`), so `{"k":1}.equals({})` held.
  One shared `Vm::drain_entry_set` for both representations.
* **View mutations did not write through.** `keySet().removeIf/remove/retainAll` on a
  `Reference2ObjectMap` returned a verdict and changed nothing — and `DataComponentPatch.forget(pred)`'s
  whole body is exactly that (`RAVA_NO_R2OMAP_VIEW_WRITETHROUGH`). Plus `values().contains(Optional.of(7))`
  was false on any map, because `Optional` has no `equals` bytecode here and `equals_kind` fell to
  identity (`RAVA_NO_JDK_VALUE_EQ`).
* A regression caused and fixed inside the same pass: `scratch::natural_cmp` has no `&Vm`, so with real
  boxes it compared two handle RENDERINGS and `Collections.max` over a `Map<Character,String>` answered
  `'"'`(34) for a set containing `'\'`(92) — guava's `ArrayBasedEscaperMap` then wrote past a
  `new char[35]` and killed MC's bootstrap. One normalisation at `vm/iterate.rs:814`.

### The window's ratio, now that both engines produce the same answer

| | java | rava | ratio |
|---|---|---|---|
| `REGISTRIES_MS` (tags + worldgen/dimension registries) | 1,445 / 1,569 | 9,028 / 9,166 | **6.0×** |
| `RELOAD_MS` (`loadResources` + drain) | 1,369 / 1,553 | 15,315 / 17,252 | **10.4×** |
| `WINDOW_MS` (the whole `WorldLoader.load`) | 2,814 / 3,123 | 24,343 / 26,418 | **8.2×** |

⇒ the reload proper is the worst thing in the engine by ratio, and it now has a bench with a
content hash. Gates 4/4 bit-exact, **23/23 probes** match `java -Xint`.

## ⭐ WARM AND FLAT: the reload costs java 145 ms and us 7,200 — 50× on pure execution

`ReloadBench --reloads N` runs the reload again over the same `ResourceManager` (which is what
`/reload` does) and times each pass, so warm-up is separated from steady state:

```
java   first 594 ms   then  192, 148, 148, 144      → converges at ~145 ms
rava   first 9,162    then 7,211, 7,079, 7,356, 7,488 → flat at ~7,200 ms
```

Three things fall out, and one of them corrects a reading I made an hour earlier:

1. ⛔**There is no degradation.** A single earlier run showed reload #2 (23.3 s) SLOWER than #1
   (18.1 s) and I called it accumulation; the trend says flat, and that run was taken while two
   `rustc` processes were building. A repeat measurement on a loaded box is not a measurement.
2. ⭐**HotSpot gains 4× from warm-up (594 → 145); we gain 1.27× (9,162 → 7,200).** Our JIT adds almost
   nothing on the second pass over the same code where C2 adds everything.
3. ⭐**Steady-state ratio is 50×**, against 15× on the first pass. So the window's cost is NOT
   dominated by one-time work — not class loading, not first-time interning, not compile time. It is
   per-execution cost of the codec/collection/string machinery, which is exactly what the four
   levers now in flight aim at.

`RELOAD2_MS` (the first warm pass) is now the metric `reload_ab.ps1` reports beside RELOAD_MS,
cpu-ms, cycles and WINDOW_MS. ⚠**The instrument's floor on RELOAD_MS is bad** — a null test against a
nonexistent flag reads −3.29% min / −5.52% median with a 5-9% arm spread, because RELOAD_MS carries
the whole first-pass warm-up. That is why the warm pass exists; its own floor is still to be measured
on a quiet box.

## The warm reload is the precise instrument, and token interning is its first win

`RELOAD2_MS` — the second, warm reload over the same `ResourceManager` — reads an **A-arm spread of
0.9%** where `RELOAD_MS` reads 6.2% on the same six runs, and `cpu_ms` reads 0.5%. That is the tightest
metric this project has; the cold first pass carries the whole JIT warm-up and should not be priced on.

**`RAVA_NO_TOKEN_INTERN` (B = interning disabled):**

| metric | min B vs A | median B vs A | |
|---|---|---|---|
| `RELOAD2_MS` | +3.72% | +8.93% | **DISJOINT** |
| `cpu_ms` | +2.70% | +2.62% | **DISJOINT** |
| `RELOAD_MS` (cold) | +2.99% | +0.59% | arms overlap |

⭐**The mechanism:** every JSON token became a fresh `RStr` = `Arc::new(s.to_string())`, two mallocs, no
interning, and the datapack repeats itself enormously — 107,274 object members over 1,731 distinct keys,
62,557 string values over 7,952 distinct. Tokens now come from `nextgen::intern_arena_string`, the
process-lifetime content-deduped pool the JIT already mints `ldc` constants into — so they are also
ARENA-resident, which is the precondition the JIT's inline `charAt`/`length` arms require
(`emit_is_arena`). Counted on the real window:

```
[TOKENS] 270,325 JSON tokens materialised, 260,889 served from the intern table (96.5%),
         9,436 distinct entries (28.6x reuse), 0 declined
       | 521,778 mallocs and 13,578,613 bytes of Arc<String> never allocated
```

Identity is safe because `opcodes::gates::ref_equal` (this engine's `if_acmp*`) already answers
`Str`/`Str` and arena/arena by CONTENT, and `identity_hash` returns 0 for every `Value::Str`, so a
deduped token is strictly more distinguishable than before, never less.

⛔**A latent mine found on the way and deliberately not stepped on:**
`natives/cascade_owners.rs:158` serves every `LazilyParsedNumber` accessor with
`match &sv { Value::Str(s) => s.as_str().trim(), _ => "" }` — an ARENA string falls to `""`, so
`intValue`/`longValue`/`doubleValue`/`toString` all silently answer **0**. It surfaced as
`{"flight_duration":1}` decoding to `0`, which made a firework result stop equalling its prototype and
grew a `"components"` block the JVM does not print. The number site therefore declines to intern, with
the reasoning recorded at the call. Anyone making that field arena-resident must fix that line first.

### Two of the four levers were refuted by their own census — which is the point of asking for one

* **`Map$Entry` births.** The brief said 3.0 M entries, 48% of all handle births. The census says the
  shape memo already serves **93.6% of view builds with no object at all** (460,533 hits / 31,290
  misses) and only **152,220** entries are minted. What was left was real but small: the `$mapview`
  marker was `Value::Str("entry".into())` = `Arc::new(String::from("entry"))`, **two mallocs for a
  compile-time constant on every one of 31,290 calls**, and `entry_view` copied its root buffer into a
  second `Vec` before dropping the first. Both gone.
* **The 26 MB char buffer.** Refuted outright: a `[chars]` census reads **493 refills / 495,474 bytes
  per process**, and BootstrapBench alone (no reload at all) reads 484 / 493,167 — so **the entire
  datapack reload contributes 9 refills and 2,307 bytes**. The control is decisive: with
  `RAVA_GSON_NATIVE` unset (reload 9,049 → 35,219 ms, so the guest tokenizer really did run) the census
  is bit-identical. The Rust tokenizer takes every document, so gson's `JsonReader` and its `char[1024]`
  are never built on the served path. `4,096 B × 6,351 files` described a path the engine does not take.

### The other three levers, and a floor lesson

| switch (B = area disabled) | `RELOAD2_MS` | `cpu_ms` | verdict |
|---|---|---|---|
| `RAVA_NO_TOKEN_INTERN` | +3.72 / +8.93% **DISJOINT** | +2.70 / +2.62% **DISJOINT** | **win, default ON** |
| `RAVA_NO_OPT_LITE` | +1.53 / +3.71% | −0.38 / +1.49% | every sign positive, none disjoint — kept ON (strictly less work, and `Optional.empty()` interning moves TOWARD the JDK, where it is a singleton) |
| `RAVA_NO_CHARBUF` | +5.70 / +2.53% | +2.28 / +0.71% | kept ON as a strictly-fewer-copies rewrite; its own census says expect zero |
| `RAVA_NO_ENTRY_BIRTH` | 3 pairs: −1.60 / +0.54, and **DISJOINT "faster when disabled" on the cold metrics** | — | **at 5 pairs nothing is disjoint and the signs flip positive** (+2.90/−0.82 cold, −0.89/+4.09 warm, +2.66/+1.28 cpu) ⇒ the 3-pair disjoint was noise; kept ON |

⚠**FLOOR LESSON: `RELOAD2_MS`'s 0.9% arm spread is a property of a QUIET box, not of the metric.** The
intern run read 0.9%; the entry runs read 9.3-11.3% and manufactured a DISJOINT verdict that reversed
at five pairs. Read the A-arm spread the harness prints before believing any verdict, and re-run at
five pairs whenever it exceeds ~3%.

## ⭐ THE ARRAY POOL WAS PRICED ON THE WRONG WORKLOAD — it pays 2-4% on the reload

`[PC-ARRAYS]` on a warm reload run counts **8,482,575 array births, 94.2% of them dead inside the
run**: ref 5,166,358 (883,675 of length ZERO, 3,142,205 of length 1-4), int 1,481,967, double 921,539,
boolean 661,639, long 117,489, char 74,030, byte 58,789. Each is a host `Vec`.

The size-class pool for that backing was built yesterday, measured on the BOOTSTRAP bench
(+0.64/+1.08/−0.75% min — the bootstrap barely allocates arrays), and shipped opt-in. Re-priced where
the population actually lives:

| metric | min B vs A | median B vs A |
|---|---|---|
| `RELOAD_MS` | **−4.36%** | **−4.54%** |
| `WINDOW_MS` | −2.53% | −2.39% |
| `RELOAD2_MS` | −2.66% | −3.57% |
| `cpu_ms` | −2.11% | −2.89% |
| `cycles` | −1.29% | −2.73% |

Ten signs out of ten in favour, nothing disjoint (arm spreads 2.6-5.6%). Default ON again.

⭐**RULE: price a lever on the workload whose census carries its population.** A neutral reading on a
bench that does not contain the population is not a verdict about the lever — it is a verdict about
the bench. Two instruments now exist precisely so that this can be checked both ways.

### Where the warm reload stands, and what is next

Quiet box, same session: `BOOTSTRAP_MS=15,129` (against java ~4,500) and the reload warm at
**5,565-6,020 ms against java's 145** — 38-41×. The profile of the warm state (102,780 samples):
compiled guest body 40.9%, seam 22.4%, **interpreter frame 17.7%**, native cascade 12.0%,
fu_fastpath 6.5%.

The next population is named and unattacked: **1,187,483 object births per run, 100% of them heap
handles (`Box<Object>` + `FieldBlob`), 100% at PERMANENTLY POISONED sites**, i.e. every `new` in the
reload leaves machine code for a host callback and can never be inlined. The census gives one reason:
`not-arena(handle)`.

## The native-supertype refusal, priced at last: −2% CPU, 0% wall

`[INTERP-WHO]` on a WARM reload names what is still interpreted when everything hot should be
compiled: **2,991,395 interpreted entries over 12,330 methods**, the top ten carrying 28.5% of them
and every one marked `failed` —

```
368,429  failed  com/google/gson/internal/LinkedTreeMap.findByObject
110,635  failed  net/minecraft/resources/Identifier.read
 74,234  failed  com/google/common/collect/AbstractIndexedListIterator.hasNext
 69,196  failed  it/unimi/dsi/fastutil/objects/ObjectOpenHashSet$SetIterator.hasNext
 45,308  failed  com/google/common/collect/ImmutableMapEntry.getKey
 38,574  failed  com/mojang/serialization/JsonOps.getNumberValue
```

`failed` here is not a compiler error: `jit_run.rs:1130` refuses **before any attempt** when
`wide_tier_admits` finds `Collection`, `Iterable`, `Map`, `Map$Entry`, `Iterator` or `BaseStream` in
the class's type closure, on the grounds that such a class's own bytecode might read state the
natives never populate. The census refutes that for this population — those methods RUN, interpreted,
millions of times, to a bit-exact hash. `RAVA_WIDE_TIER_ALL=1` admits them all:

* refusals before any attempt **433 → 244**, compiled 2,220 → 2,256
* interpreted entries **2,991,395 → 2,471,965 (−17.4%)**
* `ReloadBench` hash unchanged (`c956377e19071d7a`) ⇒ the refusal was insurance, not a requirement
* **priced: cpu −1.81/−2.01%, cycles −1.24/−1.64%, guest ms +0.59/+0.42%, nothing disjoint**

⇒ **the line the ledger has carried since August with "wall NOT measured" is now measured: ~2% of CPU
and none of the wall.** The interpreted entries it removes are not on the critical path. Kept opt-in as
the measured ceiling for the narrow form (refuse per METHOD — does a native serve this exact
owner/name/descriptor — rather than per CLASS), which cannot beat this ceiling.

⭐**And that reframes the window.** Removing 17% of interpreted entries moved nothing, the seam has been
attacked repeatedly, and the birth population converts to 1.5%. What is left is the 40.9% inside
compiled guest bodies: the codec machinery makes **~20 dynamic dispatches per decoded field where
HotSpot makes ~0-1**, because every callee is 4-30 bytecodes — far under `FreqInlineSize` — and C2
inlines the whole chain into straight-line code. The inliner was priced at ≤0.6% once, but that was on
the BOOTSTRAP, whose callees are VoxelShape ops; the reload's callees are codec lambdas. Same lesson as
the array pool: that verdict was about the bench, not about the lever.

## ⭐⭐ THE CODEC SITES ARE MEGAMORPHIC BY CONSTRUCTION — the profile is not missing, it is saturated

A new census, `[INLCEN]` (`vm/inline_census.rs`), is the inliner's own `compile_bail`: 62 rows, every
refusal named for the gate that made it, compile-time only. On a warm reload:

```
[INLCEN] 75,277 sites offered, 5,756 admitted (7.6%)
  23,155 site.devirt(0xb6|0xb9)   18,404 devirt.no-profile   = 79.5% of devirt offers
  14,458 site.sam(0xb9)           14,415 sam.no-profile      = 99.7% of SAM offers, 6 admitted
   3,193 shape.invokeinterface     4,239 shape.new    1,479 body.partial-gc-unsafe
```

**The dominant refusal is "no receiver profile" — 32,819 of 75,277 offers (43.6%). No opcode rule is
within an order of magnitude**, and the ranking only became visible because the census asks the shape
walk in opcode-before-budget order: `jit::inlinable_bail_reason` tests SIZE first and so reported
`size` for a 138-byte body whose real blocker was `0xb9`, which is what made "size" look like a rule.

⭐**Then the learn path was built and measured at ZERO, and that is the finding.** `lambda_site_note`
had exactly one writer — the INTERPRETED arena-SAM apply — so under tier-0 (a method compiles on its
first entry) a hot SAM site is permanently unprofiled. The cure was written
(`note_sam_site_from_compiled`, fed from `note_recv_profile`'s lambda-shape refusal, with an epoch
bump so `recompile::should_supersede` can see it). Measured on the same workload:

```
991,507 host dispatch misses: 14,888 arena-lambda receivers, 434,239 arena-other, 542,380 tagged/null
transitions across the whole run: 1 site claimed, 8 poisoned
```

**14,888 lambda misses producing 9 transitions means the sites were already claimed — by the
interpreter during the cold pass — and are already saturated to "refused".** The profile exists and
says megamorphic. The reason is structural: the key is `(caller class, constant-pool index)`, and
`DataResult$Success @cp27` is ONE `Function.apply` Methodref shared by `map`, `mapOrElse`, `flatMap`,
`ap` and `lambda$ap$0` — and `map` is called with every codec lambda in Minecraft. A per-offset key
would not save it either.

⇒ ⭐⭐**HOW HOTSPOT WINS HERE IS NOW NAMEABLE: INLINING CREATES MONOMORPHISM.** The site inside `map`
is megamorphic; the same site inside a copy of `map` inlined into ONE caller is monomorphic, because
that caller passes one lambda, minted by one `invokedynamic` in its own body. C2 inlines `map` into
its caller (MaxInlineLevel 15) and only then sees a constant receiver. We decide inlining with a
profile keyed on the callee's own site, so we can never reach that state — and the engine's chained
depth-N machinery (`callee.nested`, `hof_sam_pc`, `active_sam`, `hof_fusions`) exists but admits 6 of
14,458 SAM offers. **The next design is context-sensitive inlining: when a callee is spliced, re-decide
its interior call sites against the CALLER's SSA facts (a lambda argument that is a constant of the
caller), rather than against the callee's saturated profile.**

### What shipped from the wave, and its price

A widened interface-body arm (`RAVA_NO_CODEC_INLINE`): a callee that keeps ONE surviving interface
dispatch may now inline — e.g. `DataResult$Success.mapOrElse`, eleven bytes, refused until now because
`is_inlinable_kind` returns false at the first `0xb9` and DFU is interface-typed end to end. Three
containments: guarded entries only (so `analysis/range.rs`'s existing predicate already excludes them
from the `readonly` stamp — no edit to the file that asserts the invariant), depth 0 only, and its own
GC proof (`iface_partial_gc_safe`) because `partial_inline_gc_safe` counts real calls over three
opcodes only and would read a body with a live `0xb9` as fully collapsed — the 20.4 crash class.

Population: 171-321 admitted, **20-24 actually spliced per warm run**. Priced on a quiet box (arm
spreads 0.6-2.8%): RELOAD_MS −0.90/−0.61%, WINDOW_MS −0.69/−0.17%, RELOAD2_MS −0.35/+0.37%, cpu
−0.12/−0.49%, cycles −0.39/−0.19% — **everything overlaps; neutral, as 20 splices must be.** Kept ON
as strictly fewer calls at run time, bit-exact. Gates 4/4, 23/23 probes, both hashes.

## ⛔ CONTEXT-SENSITIVE INLINING: PROVENANCE WORKS, AND IT IS NOT THE BLOCKER

The design from the census — "the caller knows which lambda it passes, so re-decide the callee's
interior site against the CALLER's facts" — was built (`vm/lambda_prov.rs`, opt-in `RAVA_CTXINLINE=1`)
and it produced a refusal with counts. Two findings, both worth more than the code:

⭐**The profile was never needed.** `lambda_facts` reads the MethodHandle kind and the implementation's
owner/name/descriptor straight out of `BootstrapMethods` arg 1 of the `invokedynamic` — the same two
pool reads `opcodes/indy.rs:319` already makes — and `site_idx` follows because `LambdaSite` is
content-interned, while `cid = lambda_class_id(cap_kinds)` is a pure function of the indy descriptor.
Both are exactly what the mint writes. **A lambda's implementation is a static fact of the bytecode,
not a profile.**

⭐⭐**But the caller never applies the closure it mints — it hands it away.** An exact abstract operand
stack over 26,035 caller bodies proved **4,626 closures** and then:

```
1,683  the token is consumed as an ARGUMENT
    0  the token is consumed as a RECEIVER      <- shape A is empty, 0 of 4,626
1,613  that argument's call site is not inlined  (705 iface / 515 special / 323 virtual / 70 static)
    2  provenance reached an inlined call → 1 no-sole-iface-site, 1 body-refused
    0  admitted, 0 spliced
```

So caller-local dataflow can never name the receiver at a SAM site: those receivers arrive as
parameters and captures, one frame further in. The 17,335 `sam.no-profile` refusals are untouchable
this way, and no per-site profile fixes them either — which closes the loop with last night's finding
that the profile is saturated, not missing.

⭐⭐⭐**And the real blocker is now named exactly.** `RAVA_CTX_LOG=1` lists the 1,613 uninlined argument
sites: 414 `IllegalStateException.<init>`, **294 `DataResult.flatMap` (already monomorphic, mono=1)**,
194 `Schema.register`, 185 `Map.computeIfAbsent` (native-served), 35 `DataResult.map`. The 294 are the
codec chain, they are monomorphic, and they are refused for a reason that has nothing to do with
profiles or provenance:

* `DataResult$Success.flatMap` makes **two** `invokeinterface` calls and reads `this.lifecycle`
  between them ⇒ refused by `iface_partial_gc_safe`'s `real_calls != 1` (`inline_targets.rs:994`).
* `DataResult$Success.map` **allocates** (`new Success`) ⇒ refused by `shape.new`, because a spliced
  callee may not contain `new`.

**Those two rules are the wall.** Neither is a budget: the first is the splice's rooting proof (a
spliced callee's locals are fresh Cranelift `Variable`s written from offset 0 of the caller's frame
buffer, so nothing of the callee is rooted across a surviving GC-capable call), the second is
`emit_inline`'s precondition. HotSpot has neither restriction — C2 inlines allocating callees and
scalar-replaces the allocation, and it inlines callees with any number of surviving calls.

⇒ **The ladder to the codec window is now two named rungs, in order: (1) allow a spliced callee to
allocate (the arena late-`new` machinery already exists — `emit_inline_arena_new_late`), and (2) make
the rooting proof general so N surviving calls are allowed, by spilling the callee's live references
into the caller's frame the way an ordinary call already does.** Until those, no amount of profile or
provenance work can inline the chain.

Two latent-correctness fixes fell out and ship default-ON: `emit_inline`'s child env now clears
`local_types` (a spliced body's PCs are not the outer method's and `emit_deopt_exit` indexes it BY PC
— harmless until this round, because no nested map could hold a guarded entry before), and
`emit_guarded_inline` now `Declined`s instead of `Aborted`ing when deopt has no frame map.

Verified: gates 4/4 bit-exact, 23/23 probes, `BootstrapBench b9e7debe631b42b6`,
`ReloadBench c956377e19071d7a`, warm passes `[5983, 5642, 6107]` with the flag off — byte-identical
plan to baseline.

## ⭐⭐⭐ THE ROOTED SPLICE WORKS, MEASURES ZERO, AND THAT IS THE DEEPEST FINDING OF THE INVESTIGATION

The two rungs the last round named — "a spliced callee may not allocate" and "a spliced callee may
keep at most one surviving call" — were correctly diagnosed as ONE problem: a spliced callee's locals
are fresh Cranelift `Variable`s written from offset 0 of the CALLER's frame buffer, so a GC point
inside the splice would root the callee at the cost of the caller. `emit_inline` did not even take the
caller's variables as an argument; that absence was the bug.

Built (`jit/emit/inline.rs:112` `enter_splice_roots` / `:180` `leave_splice_roots`, one shared window
per function in `Env::splice_window`): the splice now performs the root-registration half of the
prologue it elides — zero a window, push `(window, slots)` on the thread's cell-root stack,
`spill_for_gc` the caller; the body emits with `locals_ptr` pointing INTO that window, so every
existing spill/reload roots the callee in a disjoint region. **No emitter below `emit_inline`
changed.** Admission is rejection-only: outermost depth only (a nested rooted splice would have to
root its parent's Cranelift variables, which live in no buffer — `Env::in_splice` refuses the compile
rather than allow it), every nested spliced callee must be a GC leaf, straight-line with one return,
footprint ≤ `dispatch_callbuf_slots()`, and no side exit that skips the epilogue.

A latent hazard was found and fixed on the way: `new_arena` / `ctor_inlines` / `static_intrinsics` /
`array_fields` are keyed by the CALLER's constant-pool indices and were handed to every spliced body.
A rooted splice is the first body that can read them — cp 8 in `DataResult$Success` is not cp 8 in its
caller, so it would have allocated the wrong class.

**It hit the target exactly**: 1,634 admitted, **440 spliced sites, 400 allocations and 444 calls now
running inside spliced bodies**, `shape.new/newarray` refusals 4,642 → 3,716, and the top admitted
body is **`DataResult$Success.flatMap` (two surviving calls) at 314 sites** — the 294-site monomorphic
population the previous round named. 18 verification runs plus two ten-reload soaks: hash
`c956377e19071d7a` every time, no crash.

⛔**And priced on a quiet box (arm spreads 1.6-2.1%) it is NOTHING**, with most signs favouring the
switch being OFF:

| metric | min B vs A | median B vs A |
|---|---|---|
| `RELOAD_MS` | −1.15% | +0.28% |
| `WINDOW_MS` | −0.86% | −0.47% |
| `RELOAD2_MS` | +0.18% | −0.33% |
| `cpu_ms` | −1.12% | −1.35% |
| `cycles` | −0.63% | −0.91% |

⭐⭐⭐**WHY, AND IT IS THE ANSWER THE WHOLE INVESTIGATION WAS LOOKING FOR: WE DID NOT REMOVE THE COST,
WE MOVED IT.** Removing 444 calls also required adding the rooting those calls already performed —
zero a window, push a root range, spill the caller's live references, reload them after. The net is
zero, which measures the thing directly: **the price of a call in this engine is not the call, it is
the rooting protocol around every GC point.** That protocol is paid per call and per allocation,
inlined or not, and it is why "inline more" cannot close a 40× gap. HotSpot pays none of it: C2 emits
a PRECISE STACK MAP at each safepoint and the collector reads the map, so a call costs a call.

⇒ **THE NEXT RUNG IS NAMED AND THE TOOLCHAIN ALREADY SUPPORTS IT.** `cranelift-frontend 0.132` (the
version this tree builds against) exposes `declare_var_needs_stack_map` (`frontend.rs:439`) and
`declare_value_needs_stack_map` (`:554`): declare which values hold GC references and Cranelift emits
user stack maps at safepoints, which the collector consults instead of a shadow frame. Adopting them
would delete the spill/reload and the cell-root push from every compiled call and allocation in the
engine — the tax this measurement just isolated. That is the largest structural item left, it is
above every noise floor, and it is the first thing the next session should cost out.

Shipped OPT-IN (`RAVA_NO_INLINE_GCPOINT` restores the historic emission; `RAVA_NO_INLINE_GCPOINT_CALLS`
bisects stage 2 alone) — the capability is proven correct and is the precondition for the stack-map
work, but it does not pay on its own. Also recorded so no one spends a day on it: under a shrunk
nursery the engine dies in BOTH arms (2 MiB: 3/3 both; 32 MiB: control 1/2, this round 3/3 clean) —
small nurseries are already broken in this tree, independent of this work.

## Precise stack maps, Phase A: the API works, and the two protocols cannot coexist

`jit/runtime/stackmaps.rs` (new, `RAVA_STACKMAPS`, default OFF, four arms: `Off` / `probe` = register
and size census only / `sp` = the whole census apparatus with NOTHING declared / `1` = everything).
Cranelift 0.132's own APIs, read from the crate rather than guessed:
`cranelift-frontend/src/frontend.rs:439 declare_var_needs_stack_map`, `:554
declare_value_needs_stack_map`, `frontend/safepoints.rs SafepointSpiller`;
`cranelift-codegen/src/machinst/buffer.rs:1918 MachBufferFinalized::user_stack_maps() -> &[(CodeOffset,
u32, UserStackMap)]`; `ir/user_stack_maps.rs:176 UserStackMap::entries() -> (Type, offset-from-SP)`.

⭐**Frame pointers are NOT preserved in this tree and turning them on would not have helped**:
`cranelift-jit` never calls `RtlAddFunctionTable`, so `RtlVirtualUnwind` treats compiled frames as
leaves, and the collection point is deep in Rust which is not built with forced frame pointers.
Instead the deep prologue stores `get_stack_pointer` into a parallel array beside the cell-root push
(`jit/emit/prologue.rs:191`, `frames.rs`): Cranelift x64 holds SP constant through a body, every live
compiled frame is blocked in a call, so `*(SP-8)` is its return address and `SP + entry_offset` each
map slot. Five instructions, flag-gated. Frame location measured **exact: 80/80 and 303/303 located,
100% of return addresses preceded by a call encoding.**

```
[SMAP] 2,000 fns, 99,781 CLIF safepoints of which 56,391 carry a map (341,933 slots)
[SMAP] cells at located frames: ranges 296 (arena 236), maps 43 (arena 36)
[SMAP] DISAGREEMENT range-only 234 | map-only 0 | bad map offsets 0
code size: probe 19,859,022 B vs declare 20,050,454 B = +0.96%
```

⛔**FINDING 1 — the maps are nearly empty BECAUSE the shadow frame is still running.**
`reload_locals_after_gc` emits `def_var(var, load(buffer))` for every local immediately after the
call, so the pre-call value of a local has no use after the safepoint, is not live across it, and
Cranelift is CORRECT to record nothing. The maps that do exist are dominated by ref operand-stack
values, which the locals-only reload sites never refresh. ⇒ **Phase A cannot certify map CONTENTS,
and deleting the reload is not the last step of Phase B — it is the precondition for measuring it.**

⛔**FINDING 2 — declaring is not GC-neutral.** At a 64 MB nursery: the `declare` arm crashed 2/4
(access violation in `nextgen::census::arena_trace_refs`), the default arm 3/3 clean, and the `sp`
arm — the entire census, SP store and cross-check, with nothing declared — **4/4 clean**. That
attributes it to `declare_*_needs_stack_map` itself: `SafepointSpiller` rewrites every use of a
declared value into a load from a Cranelift stack slot **our collector neither scans nor forwards**,
which lengthens the window in which an un-reloaded ref operand-stack value holds a stale pointer.
(At 2 MB both arms crash in the same collector — that defect is pre-existing, see the nursery note.)

⇒ **Phase B is reachable but cannot be incremental. It is a single atomic replacement**: delete
`spill_for_gc`/`reload_locals_after_gc`, let the collector root from the maps only, and forward
Cranelift's own stack slots in place. Two blockers are named now: (a) ref operand-stack values at the
~30 locals-only reload sites are ALREADY an unrooted-window hazard and must be covered by the maps
rather than by an emitter argument; (b) the collector needs the map lookup on its own path — the
machinery in this module is exactly that, minus the write.

Verified with the flag OFF (byte-identical emission: no declaration, no SP store, no registration, no
cross-check) and ON: gates 4/4, 23/23 probes both ways, both bench hashes, `ReloadBench --reloads 3`
three times with the flag on.

## ⭐⭐⭐ PHASE B SHIPPED: THE SHADOW FRAME IS GONE, AND IT IS THE FIRST STRUCTURAL WIN OF THE SERIES

The rooting protocol around every GC point — `spill_for_gc`, a `(buffer, len)` push on the thread's
cell-root stack, `reload_locals_after_gc` — is replaced by Cranelift's own precise stack maps.
Default ON; `RAVA_NO_STACKMAPS=1` restores the shadow frame exactly, and `=probe` / `=sp` remain as
the phase-A diagnostic arms.

**Emission, counted over the same 2,000-function boundary:**

| | shadow frame (OFF) | stack maps (ON) |
|---|---|---|
| spill sites / stores | 41,165 / 262,176 | 41,814 / 269,120 **elided** |
| reload sites / loads + `def_var` | 41,165 / 235,911 | 41,814 / 242,375 **elided** |
| rooting memory ops | **498,087 = 249.0 per function** | **511,495, all elided** |
| CLIF safepoints carrying a map | 0 of 100,215 | **78,495 of 101,942 (77%)**, 289,750 slots |
| compiled bytes per function | **10,317** | **6,738 (−34.7%)** |

≈21 spill+reload site PAIRS disappear per compiled function. Phase A's blocker inverted exactly as
predicted: deleting the reload restored the liveness the maps needed, so map coverage went 56% → 77%
static and ~20% → ~50% of located frames dynamically.

**Priced on the warm reload, twice.** First run on a noisy box (arm spreads 7-10%): all ten
statistics favoured it, `RELOAD_MS` −3.04/−6.38%, `cpu` −4.66/−9.06%. Second run on a quiet box (arm
spreads 1.6-4.1%): **DISJOINT on four of five metrics** —

| metric | min B vs A | median B vs A | |
|---|---|---|---|
| `WINDOW_MS` | −2.58% | −2.94% | **DISJOINT** |
| `RELOAD2_MS` (warm pass) | −1.46% | −2.19% | **DISJOINT** |
| `cpu_ms` | −4.86% | −5.59% | **DISJOINT** |
| `cycles` | −5.36% | −5.44% | **DISJOINT** |

**Correctness**, in both arms: 4 bit-exact gates, 23 differential probes, `BootstrapBench
b9e7debe631b42b6`, `ReloadBench c956377e19071d7a` (×3 plus two ten-reload soaks), three product server
boots (`Done 0.363-0.456`), and `RAVA_STACKMAPS_VERIFY=1` — which asserts after every collection that
each arena pointer the maps named lies in the live region, printing return address, function range and
slot offset otherwise — reporting **zero missed forwards and zero out-of-extent slots in every run**.

⛔**AND THE DEFAULT FLIP FAILED FIRST, FOR THE REASON THIS LEDGER KEEPS WRITING DOWN.** Turning the
mode on by default produced a THIRD behaviour that crashed instantly (`write of 0x0` inside compiled
`Identifier.isValidPath`, 5 failures in a row) on a build whose every explicit-flag run was green,
because `frames.rs:877` re-derived "is the protocol on?" from the RAW VARIABLE instead of asking
`stackmaps::mode()`. With the variable unset the emitter stopped emitting spill/reload while the
prologue stopped recording the frame's SP, so the collector could not locate a single compiled frame
and **nothing was rooted at all**. One fact, two derivations, and the default was the input that made
them disagree. `frames::stackmaps_on` now calls `stackmaps::records_sp()`.

Still conservative, and said plainly: the self-registering prologue continues to publish
`(locals_ptr, frame_len)` and the collector still scans it, because that push is where the frame's SP
is recorded and it is the fallback for the capacity-overflow arm (`FRAMES_NO_SP` measured **0** in
every run — the arm never fired). What it holds under the new protocol is exactly the prologue's entry
image, written once and forwarded in place, so a reference that arrived as an ARGUMENT retains a
second static root. It costs nothing, but it means the maps are not literally the only root source
yet, and it masks a hypothetical map miss for the argument population.

### The rooting-derived inline limits, dropped rather than worked around

With the shadow frame gone, `iface_partial_gc_safe`'s `real_calls != 1` and its tail rule — both of
whose comments name the shadow frame verbatim — have nothing left to protect. Under
`stackmaps::declaring()` they are skipped, the splice emits **no root window at all** (the zero loop,
the cell-root push and the caller spill were all writing into a region nobody reads any more), and the
body runs on the caller's `locals_ptr` as every non-rooted splice always has.

```
iface.calls!=1 refusals      18 / 282 / 456   →   0 in every run
splices with NO root window  466 / 408 / 474  =   all of them
calls now inside a spliced body                  832 / 716 / 838
compiled bytes per function  −0.15% … −1.6% (expected ≈0.4% from ~470 windows × ~120 B)
```

The one thing added rather than removed: **the caller's own ref operand-stack values were named by
nothing inside a splice**, because the `spill_for_gc` choke point that declares them is handed the
CALLEE's stack there. `declare_stack_roots` now names them once per splice and emits zero
instructions — `SafepointSpiller` gives a slot only where a value is live across a safepoint.

Priced: neutral (±1.3%, everything overlapping). Kept default-ON as strictly less work, bit-exact.

⭐**Two rules were read and deliberately NOT relaxed, which is the more useful half of the result:**
* **`method_no_alloc` is not a rooting rule.** It refuses `new` because an allocation MOVES objects,
  and that is what licenses the `readonly` stamp on descriptor loads for LICM. Stack maps forward
  roots; they do not stop the heap moving.
* **`partial_inline_gc_safe` IS rooting-derived and relaxable — but not yet.** That band emits
  UNGUARDED entries, for which `leaves_gc_point()` answers false, so widening it would feed bodies
  with surviving GC points to the analysis that stamps arena field loads `readonly`. It needs
  `leaves_gc_point()` extended to a fourth shape first. **Population left on the table:
  `body.partial-gc-unsafe` = 991-1,150 refusals per run** — the next named target.

### The stack-map stretch, measured end to end

Interleaved against the commit before it (`c45809fe`), three runs each, three warm reloads per run,
same box, same hash (`c956377e19071d7a`) throughout:

```
PRE  medians 6002 / 6044 / 6060 ms
NOW  medians 5884 / 5893 / 5893 ms      → −2.4%, every pair in the same direction
```

That is the honest guest-visible figure for replacing the rooting protocol; the CPU and cycle figures
were larger (−4.86/−5.59% and −5.36/−5.44%, disjoint) because much of what was deleted is memory
traffic that overlaps with other work. The compiled-code figure is the structural one: **−34.7% of
machine code per method**, and it was all rooting.

⭐**Three rounds in a row now measure zero on time while doing exactly what they set out to do**, and
together they say something the ledger should not lose: the rooted splice hit `flatMap` at 314 sites
and cost nothing; the stack maps deleted 498,087 rooting operations and bought 2.4%; dropping the
rooting-derived inline limits made every splice windowless and bought nothing. ⇒ **inlining is no
longer the constraint — we inline what we meant to, and it is free.** What remains is not "do we make
the call" but "what runs": the warm reload is compiled body 38.4%, seam 21.6%, interpreter 19.6%,
native cascade 10.5%, `fu_fastpath` 6.5% — and HotSpot does the same work in 145 ms.

### `partial_inline_gc_safe` relaxed — the fourth neutral in a row, and that is now the finding

The predicate was extended first, as the previous round required: `InlineCallee` gains
`partial_gc_points`, so `leaves_gc_point()` (`jit/model/plans.rs:218`) answers TRUE for the fourth
shape — an unguarded, windowless entry whose surviving GC points are covered by the stack maps —
and `range.rs`'s `readonly` stamp therefore keeps excluding exactly what it must. Only then was the
band itself relaxed: `partial_inline_gc_safe` now delegates to `partial_inline_gc_safe_mode`, which
takes the mode as a parameter, gated by `partial_free_enabled()` — its own `OnceLock` for
`RAVA_NO_PARTIAL_FREE`, ANDed with `stackmaps::declaring()` **asked, never cached** (the rule this
ledger wrote after `frames.rs` re-derived that fact and left the engine with no roots at all).

Priced: −0.03/+0.64%, −0.54/+0.21%, −0.21/+0.79%, cpu −0.48/−0.39%, cycles −0.13/−0.47% — everything
overlapping. Verified: gates 4/4 bit-exact, 23/23 probes, both bench hashes, a ten-reload soak, and
`RAVA_STACKMAPS_VERIFY` reporting **zero missed forwards** across every walk.

⭐**Four consecutive rounds of inlining work now measure neutral while each did exactly what it set
out to do.** The rooted splice inlined `flatMap` at 314 sites; the stack maps deleted the rooting that
made it free; the rooting-derived limits came off and every splice went windowless; the partial band
opened. ⇒ **Inlining is not the constraint and should not be attacked again without a new
measurement.** The warm reload is still compiled body 38.4%, seam 21.6%, interpreter 19.6%, native
cascade 10.5% — and HotSpot does the same work in 145 ms, which puts our compiled code itself about an
order of magnitude behind C2 on this workload. The next honest step is not another admission rule: it
is an instruction-level comparison of what we emit for one hot codec method against what C2 emits for
it.

## ⭐⭐⭐ MEASURED AGAINST C2 DIRECTLY: THE REMAINING GAP IS INLINE DEPTH

`java -XX:+UnlockDiagnosticVMOptions -XX:+PrintInlining` on `ReloadBench --reloads 2` prints C2's
entire inline tree for the identical workload. No disassembler needed, and it settles the question
four rounds of our own work had narrowed:

```
C2:  52,386 inline decisions, 27,978 inlined (hot)
     by DEPTH:  0-2: 4,560 | 3-5: 1,727 | 6-10: 2,840 | 11-20: 10,304 | 21-40: 4,113 | 41+: 195
     maximum depth: 69
     refusals: callee too large 7,327 | no static binding 3,434 | too big 1,232 | not inlineable 556
ours: 75,277 sites offered, 5,756 admitted (7.6%), and every arm that matters is DEPTH 0
```

⭐**83% of C2's inlining is at depth ≥ 3 and its largest single bucket is depth 11-20.** We are offered
more sites than C2 makes decisions and admit a fifth as many, all shallow. That is the order of
magnitude in compiled-code quality, stated as a count rather than a guess.

And the restriction that enforces our depth 0 is, once again, a rooting argument: `emit_inline`
refuses a nested rooted splice because it would have to root its PARENT's Cranelift variables, which
live in no buffer, so `Env::in_splice` refuses the compile outright. **Under the stack maps that
objection is void — the maps cover every frame's declared values, parents included** — exactly as the
`real_calls != 1` rule and the `partial_inline_gc_safe` band were void once the maps carried the
roots. `gcpoint_admit`'s own depth-0 rule was already measured as never incremented.

⇒ Depth is the next rung, and unlike the last four it is not a percent: it is the difference between
5,756 shallow splices and 27,978 deep ones. C2's own budget for reference: `MaxInlineLevel 15`,
`MaxRecursiveInlineLevel 1`, `MaxInlineSize 35` and `FreqInlineSize 325` bytecodes.

## ⭐⭐ DEPTH IS NOT CAPPED — IT IS STARVED, AND THE STARVING GATES ARE NAMED

The depth-0 restrictions that were rooting arguments are lifted under the stack maps
(`RAVA_NO_DEEP_INLINE`, with `RAVA_NO_DEEP_IFACE` / `RAVA_NO_DEEP_DEVIRT` to bisect): `Env::in_splice`'s
compile abort (now reached ~610×/run), `gcpoint_admit`'s and `gcpoint_admit_fresh`'s `depth != 0`
(the second is why the first's census row read 0 — it was unreachable behind it),
`gcpoint_nested_gc_leaf`, `inline_target_for_call`'s `allow_iface_body: false`, and devirtualisation's
`depth == 0` (which was never argued at all — `sitemono` is keyed by *(containing class, cp)*, so a
nested callee's own sites are already profiled). Depth became a budget: `RAVA_INLINE_BUDGET` (6000
bytecodes per compiled method's whole tree) beside the existing `RAVA_INLINE_DEPTH`.

Two soundness payments were made at the source: the widened-interface arm now sets
`partial_gc_points = true` unconditionally (it requires an `invokeinterface`, so a real dispatch always
survives — that was previously safe only because its one caller happened to set a guard), and the
cp-indexed side maps `static_intrinsics` and `ctor_inlines` are now per-level, because a depth-8 chain
is eight unrelated constant pools indexing one table and both are reachable (`is_inlinable_kind`
admits `0xb8`/`0xb7`).

**And then the census refuted the plan's own premise:**

```
             0-2     3-5    6-10   11-20   21-40    41+
C2          4,560   1,727  2,840  10,304  4,113    195     (83% at depth >= 3)
rava OFF    4,876       1      0       0      0      0
rava ON     7,513     131      0       0      0      0

exact:        d0      d1     d2    d3   d4+
OFF        3898     936     42     1     0
ON         5996    1367    150   131     0
```

⭐**`shape.depth-cap` is 0 in every run, and `RAVA_INLINE_DEPTH=15` still leaves d4 empty.** Only a
CHAIN admission builds the next level, and the non-leaf count runs **169 → 12 → 3 → 0**: the three
depth-3 admissions are all leaves, so there is nothing at depth 4 to collect. The round roughly
doubles the non-leaf count at every level and still terminates at 3.

⇒ **What starves it are BODY-SHAPE gates, not depth**, and they are counted per run:
`iface.control-flow` **708**, `shape.new/newarray` **649**, `shape.invokeinterface` **573**,
`iface.size` **537**, `body.class-uninitialised` **418**, `shape.void-return` **339**,
`shape.size > tier0-cap` **264**. Our inliner takes straight-line, allocation-free, non-interface,
non-void bodies under 35 bytes; C2 takes anything hot under 325 bytecodes. **That is the next rung,
and the 35-byte tier-0 cap has a recorded measurement behind it (boot 86.9 → 93.0 s) so it must be
re-measured, not assumed.**

Verified: gates 4/4 bit-exact, 23/23 probes both arms, both bench hashes, `--reloads 10` soaks, and
`RAVA_STACKMAPS_VERIFY` over 34 post-collection walks / 904 slots / 710 arena pointers reporting
**zero missed forwards**. Compiled size +2.3% (6,791 → 6,950 B/fn) — no explosion.

⚠**UNPRICED, DELIBERATELY.** The box entered a 2.5× slow state during this round: a `--reloads 10`
soak that read 5,905-6,141 ms in the morning read 13,000-16,000 ms in the evening, **on the committed
HEAD binary as well as on the working tree**, with the machine otherwise idle. Both arms of every
comparison moved together, so nothing here is a regression — and nothing here has a price yet either.
Re-price on a quiet box before treating the depth work as a win or a loss.

## ⭐⭐ THE STRAIGHT-LINE RULE WAS A LEFTOVER, AND THE 708 WAS AN OFFER COUNT

`emit_inline` has emitted full control flow since V4.2 and the ordinary nested band exercises it
today: `find_leaders` in the CALLEE's own pc space (`jit/emit/inline.rs:1041`), one Cranelift block per
leader, operand-stack values crossing every edge as block params (`jit/support/stack.rs:139/161/175`),
each `*return` a `jump` to one `cont` join whose parameter is the return value — **and no parameter at
all for `void`** (`:1050-1054`) — with every leader block sealed after the walk, so back edges are
legal. `straight_line_return` is a fast path whose own comment says it "changes no emitted
instruction"; it exists so a getter does not cost a phi.

The refusal lived in exactly one place: `iface_body_admissible` refused `0x99..=0xab | 0xc6..=0xc9`,
and its stated reason was `iface_partial_gc_safe`'s **"single-linear-path premise"** — the premise
under which that predicate's `real_calls != 1` clause and its linear tail walk reasoned about liveness.
`inline_free_enabled()` retired both of those clauses **inside that same function** two rounds ago.
⭐**The straight-line rule was the last clause of a retired argument, still standing only because it
lived in a different `match` arm from the clauses retired with it.** Not a capability — a leftover.

One genuine emit-side constraint remains and it is a decoder fact, not a proof: the splice walks a
body with `insn_len(op)`, which has no arm for `tableswitch`/`lookupswitch`/`jsr`/`ret`/`goto_w`, and
its `None` fails the WHOLE caller's compile. Those stay refused (~20 a run) and are counted.

```
iface.control-flow   OFF 567 / 521 / 503        ON 0 / 0 / 0
branchy offers                                  ON 1028 / 1370 / 1561
  → past the shape gate                            24 / 22 / 23
  → admitted                                       13 / 10 / 12   (2 with a BACK EDGE every run)
  → spliced                                         8 /  8 /  8
iface.shape          OFF 1621 / 1547 / 1437     ON 1856 / 2144 / 2155
```

⛔**And that decomposition refutes the headline the round was aimed by.** `iface.control-flow`'s ~500
was one refusal per *(site, compile)* OFFER, not per body. Lifting it converts ~2% into admissions;
`iface.shape` grows by ~+517, the same size as the row that vanished. ⇒ **the real blocker of a
branchy interface body is `iface.shape` — a `getstatic` / `putfield` / `new` / `instanceof` /
array-store further along the same body — not the branch.** That is now a counted decomposition in
the census rather than a guess, and it is the next target.

What did splice (8 a run) is nevertheless the right shape: `Mth.binarySearch(IILIntPredicate;)I`,
**53 bytes, a counted loop with a guest `IntPredicate.test` inside it**, now inlined into its caller;
`TagLoader.findTagsForRegistry` (43 B, back edge); and `ExtraCodecs$2.encode/decode` +
`ComponentSerialization$StrictEither.decode` — the DFU codec pipeline this arm exists for.

Verified: gates 4/4 bit-exact, 23/23 probes both arms, both bench hashes ×3, two ten-reload soaks,
`RAVA_STACKMAPS_VERIFY` reporting **zero missed forwards in all 8 runs**, and `[CBAIL]` showing 19-21
compile refusals ON against 19 OFF — **no caller was lost to an undecodable opcode or a join
mismatch**, which was the one hazard. Size +0.16%, inside both arms' own spread.

⚠**Unpriced.** The box is now 3× slower than this morning on the SAME committed binary
(`BOOTSTRAP_MS` 14,055 → 44,947; a ten-reload soak 5,905-6,141 → 15,000-16,000), so no timing from
this stretch means anything. Both the depth round and this one are shipped correct and unpriced;
re-price both on a quiet box.

## ⛔⭐⭐ "THE BOX IS 3× SLOW" WAS WRONG — AND SO WAS MY FIRST REPLACEMENT FOR IT

The previous two entries shipped unpriced on the claim that the machine had entered a 2.5-3× slow
state. That claim is withdrawn, and so is the finding I first replaced it with. Both were the same
mistake, made twice in one hour.

What survives, each ruled out by its own measurement:

* **Not the last two rounds.** Built `6495eea6` (the last commit whose code was measured fast, at
  17:51) and HEAD side by side and ran them interleaved: PRE read 14,791 and 17,589, HEAD 21,570 and
  19,300. **Both arms slow ⇒ not a regression I shipped.**
* **Not the clock.** Sampled `% Processor Performance` 66 times *during* a slow run: 143-151% of a
  2500 MHz base = a steady **3,630 MHz**, on the High-Performance scheme. No throttle.
* **Not paging.** `Pages Input/sec` over a whole slow run totals ~3,948 pages ≈ 15 MB, and the
  process working set peaks at **1,177 MB against a 4,096 MB cap**. Free physical 4.0-4.7 GB.
* **Not the process cap, not the standing env.** With the pin held fixed, the full standing
  environment and the harness's smaller one read 6,368 and 6,459 — one number.

⛔⭐**AND NOT AFFINITY, WHICH IS THE PART I GOT WRONG AND HAD TO RETRACT WITHIN THE HOUR.** Two
sequences appeared to show that our engine pays 2.6× for running unpinned while HotSpot pays nothing:

| tried in this ORDER | reading |
|---|---|
| pinned `0x0FFF`, full env | 6,368 |
| pinned `0x0FFF`, harness env | 6,459 |
| **unpinned** | **16,501** |

and a mask sweep `0xFFFF` / `0x0FFF` / `0x1FFF` / none reading 6,072 / 9,384 / 11,016 / 20,221.
⭐**Both are monotonic in RUN ORDER, and in both the interesting arm was placed last.** Re-run as a
proper ABBA — pinned, unpinned, unpinned, pinned — the difference disappears entirely:

| | `RELOAD2_MS` | `cpu_ms` | cores |
|---|---|---|---|
| pinned `0x0FFF` | 5,846 / 6,054 | 44,000 / 43,953 | 1.07 |
| unpinned | 6,037 / 6,035 | 46,219 / 48,250 | 1.05-1.07 |

**Affinity costs nothing.** The java control that made the false finding look solid was the one
sequence I *had* interleaved (pin, unpin, pin, unpin) — which is exactly why it was flat, and exactly
what I failed to do for our own arms.

⭐**The rule this cost me, stated so it is not re-learned a fourth time: an A/B whose arms run in
sequence measures WHEN they ran. Interleave, or measure nothing.** The ledger has written this down
before under other names — the null test, the disjoint-is-not-accurate row — and I still ran three
un-interleaved sweeps tonight and believed two of them.

**What is actually true about the box: it has EPISODES.** The same binary, unpinned, read 16,501 at
21:40 and 6,035 at 22:20, with no change to clock, memory, or code. During an episode a single warm
reload was seen to take **101,127 ms**. Nothing is priceable during one and everything is priceable
outside one, so the null test is not optional discipline here — it is the only way to know which
state the box is in. Tonight's null test (`RAVA_NONEXISTENT_NULL_TEST`, 3 pairs, pinned harness) came
back with A-arm `RELOAD2_MS` 5,846 / 5,616 / 5,760, nothing disjoint, arm spreads **4.1-8.8%** ⇒ the
harness is sound right now and tonight's floor is ~5%, not the morning's ~1%.


## ⭐⭐⭐ THE LARGEST REFUSAL IS "NO PROFILE", ITS CURE WAS WRITTEN INTO THE PLACE ITS OWN COMMENT REJECTS, AND MOVING IT CHANGES NOTHING

Three inliner rounds in a row measured neutral and `9aecbf42` had already written "that closes the
inlining line", so this round stopped tuning the inliner and re-read the census on HEAD instead. It
does not say what I had been aiming at.

```
[INLCEN] 32,033 sites offered, 4,401 admitted (13.7%)
   depth exact  d0: 3,273 | d1: 1,475 | d2: 98 | d3: 27 | d4+: 0     => 0.6% at depth >= 3
   (cap RAVA_INLINE_DEPTH=8 never reached, tree budget 0 refused)
```

⭐**Depth dies between d1 and d2 — 1,475 to 98, a 93% cliff — and neither the depth cap nor the tree
budget is the thing that kills it.** Against C2's 27,978 inlined with 83% at depth ≥ 3, we are at
0.6%. And the refusals that hold the population are not the ones I named last round:

| refusal | count | of 32,033 offers |
|---|---|---|
| `devirt.no-profile` | **8,116** | 25% |
| `body.unresolved` | 4,910 | 15% |
| `body.<init>/<clinit>` | 4,903 | 15% |
| `sam.no-profile` | **4,198** | 13% |
| `iface.shape` | 1,954 | 6% |

⛔**`iface.shape`, which the last round's own conclusion named as "the next round's target", is
fifth and an order of magnitude off the top.** The two "there is no profile" rows are 38% of every
offer between them, and their proportions are unchanged since the census was built: `sam.no-profile`
refuses **99.0%** of SAM offers (4,198 of 4,239) and `devirt.no-profile` **78.8%** of devirt offers,
against 99.7% and 79.5% when the rows were first read.

### The cure exists, and it was wired into the one place its own design document rules out

`note_sam_site_from_compiled` was built precisely to fill that table — tier-0 compiles a method on
first entry, so the interpreted writer that is the profile's only other source stops running exactly
when a site gets hot. Its comment names where it must live and why:

> **WHERE, AND WHY THERE.** `rava_dispatch_miss` is the compiled site's own dispatch falling to the
> host — it carries `(caller_ptr, cp)`, which IS the site key … `Vm::note_recv_profile` was the
> other candidate and is the WRONG one: it runs only where a direct-call entry is BUILT, and a
> lambda apply from compiled code does not build a direct entry, it falls to the general path.

The call sits in `vm/jit_direct_call.rs:326` — inside `note_recv_profile`, the candidate that
paragraph rejects. A five-counter funnel added to the hook prices the consequence exactly:
**40 hook entries in a warm reload**, 4 of them refused on MethodHandle kind, against 6,003 SAM
sites offered.

### Moving it to the named place fires it 26,000× more often and yields ONE profile

Wired into `rava_dispatch_miss` as specified, with the same main-thread rule as the
`note_epoch_move` beside it:

| | count |
|---|---|
| hook reached from compiled code | **1,061,065** |
| receiver is an arena object that is not a lambda | 472,297 |
| receiver is a handle that is not a lambda | 291,034 |
| receiver is some other tagged shape (array / box / null) | ~280,000 |
| receiver is a lambda of MethodHandle kind ≠ 6/7 | 5,199 |
| **receiver is a HANDLE-backed lambda** | **0** |
| arena lambda, kind 6/7, passing every filter | ~12,500 |
| **new sites profiled** | **1** |

⭐⭐**So the hook was in the wrong place AND the right place does not carry the population.** The
handle-backed-lambda row was added specifically to test the ledger's standing explanation for the
guarded-devirtualisation failure — "every collection receiver is a heap Ref" — and it reads **zero**:
these receivers are not heap Refs either. A compiled SAM apply does not fall to the host at all,
by any of the three paths now counted. `sam.no-profile` is therefore not a missing WRITER, which is
what the cure assumed; it is a site that never leaves machine code, so no host callback can ever see
it.

⇒ The next attempt on this line must write the profile from **inside compiled code** — at the
arena-lambda apply's own fast path — or abandon the profile and bind these sites by class-hierarchy
analysis instead, which needs no profile at all and which this engine has never had (`grep` finds no
CHA, no unique-implementor, no single-concrete-subtype anywhere) while the invalidation machinery a
CHA assumption requires is already built and measured (`727,602 → 1` forced deopts).

**Shipped from this round: the funnel counters only.** The correctly-placed hook is REVERTED — it
costs a million calls on the dispatch-miss path and buys one profiled site. The counters that stay
sit inside a function the shipped configuration reaches 40 times.

### And the aiming measurement behind that verdict: the warm reload's host profile is flat

Before designing class-hierarchy analysis, the rule this ledger keeps writing down — convert to a
share of load BEFORE writing code — says to price what perfect inlining could buy. A `release-diag`
build (`strip = "none"`, the only build whose host symbols are trustworthy) profiled over a warm
`ReloadBench --reloads 2`:

```
[PC-PROFILE block1] 15,169 samples: 2,204 (14.5%) inside compiled code, 12,965 (85.5%) in the Rust host
[PC-PROFILE final ] 14,629 samples: 1,613 (11.0%) inside compiled code, 13,016 (89.0%) in the host
```

⇒ **A perfect inliner is bounded by 11-14.5% of the reload**, which is the same bound row 434 put on
the product boot (7.4-8.2%). The host half is flat, exactly as every previous slicing of it:

| share | host function |
|---|---|
| 2.2% | `Vm::jit_run` |
| 1.9% | `Vm::jit_call_body` |
| 1.7% | `Vm::jit_direct_probe` |
| 1.7% | `Vm::fu_fastpath` |
| 1.6% | `natives::cascade::dispatch_owner_cascade` |
| 1.5% | `opcodes::invoke_virtual::invokevirtual` |
| 1.3% | `Vm::jit_direct_call` |
| 1.2% | `Vm::invoke_lambda_parts` |
| 1.2% | `opcodes::hot::run_hot` |
| 1.1% | `Heap::gc_with_object_slot_policy` |

The call/dispatch family sums to ≈13%, which is the seam this ledger has priced at ~14% twice
before, from two other directions. **Nothing in the reload is concentrated: the top host row is 2.2%
and the top GUEST method is 0.7%** (`IndirectMerger.<init>`, with the whole `world/phys/shapes`
voxel-merge family behind it).

⭐**One thing the same logs settle, cheaply, against a standing suspicion.** Every gil-free ring run
in a reload aborts — `aborted=true … first=java/lang/InternalError: direct miss`, 6 of 6, `keep=false`
— which looked like the lane throwing away parallel work and redoing it serially. It is not worth
chasing: those regions run `t0=50ms t1=52ms`, **two milliseconds**, and the time actually sits in
`pilot#0 SERIAL ms=2,028` and `ms=3,173` inside batches that read `n=1`. A one-task batch has no
parallelism to lose. The reload is serial because the WORK is serial, not because the lane drops it.

## ⭐⭐⭐ THE WARM RELOAD BY CATEGORY: 30% IS CALL PLUMBING, 6% IS COMPILED GUEST WORK

The per-function host profile is flat (top row 2.2%) and has been every time it was read. This is
the same profile aggregated by MODULE, over the `final` block alone — 10,811 ticks attributed of
14,629 samples (74%) — which is the slicing that was never taken on the warm reload:

| share | category |
|---|---|
| **20.09%** | `rava::vmstate` |
| 9.70% | `rava::heap` |
| 7.82% | `rava::opcodes` |
| 6.09% | `rava::natives` |
| 6.05% | `std/core/alloc` |
| **6.03%** | **GUEST bytecode (compiled)** |
| 5.61% | `mi_malloc_aligned` |
| 5.35% | `rava::nextgen` |
| 3.76% | `hashbrown` |
| 3.63% | `rava::tlcache` |
| 3.58% | `memchr` |
| **3.28%** | **`NtCreateFile`** |
| 2.69% | `rava::jit` |
| 2.21% | `_NLG_Return2` |

⭐⭐**`rava::vmstate` is entirely call plumbing** — `jit_run`, `jit_call_body`, `jit_direct_probe`,
`jit_direct_call`, `jit_direct_invoke`, `jit_call`, `jit_fu_invoke`, `fu_fastpath`,
`invoke_lambda_parts`, `invoke_framed`, `invoke_idx`, `hr_synth_finish`. With `opcodes` (7.8%) and
`jit` (2.7%) beside it that is **≈30% of the warm reload spent deciding how to perform a call,
against 6.03% spent inside the compiled guest code that the call exists to reach.** Five to one.
The per-function view could never show this: the plumbing is spread over a dozen functions none of
which exceeds 2.2%, which is exactly why every previous reading concluded "flat, no lever".

Allocation is the other real category: `std/core/alloc` 6.05% + `mi_malloc_aligned` 5.61% + the
`mi_free` / `mi_theap_realloc_aligned` / `RtlSizeHeap` rows below the cut ≈ **15%**.

### And a defect the category map exposed: we re-open the filesystem 275× more than java does

`NtCreateFile` at 3.28% of a WARM reload — one that runs over the same `ResourceManager` — is not
something a warm pass should be doing at all. Measured with `GetProcessIoCounters`, per additional
warm reload (`--reloads 4` minus `--reloads 1`, divided by 3), pinned, both engines on the identical
workload:

| | read ops per reload | **other ops per reload** |
|---|---|---|
| rava | +4,378 | **+13,492** |
| java | +8,763 | **+49** |

⭐**java performs FORTY-NINE filesystem control operations per warm reload; we perform 13,492 —
275×.** It reads more BYTES than we do (49 MB vs 18 MB over the run) and touches the filesystem
almost not at all, which is the signature of held-open archive handles against per-lookup reopening.
At ~3.3% of the reload this is worth ~220 ms of 6,700 — real, non-flat, and the first thing found in
this whole stretch that is a plain defect rather than a distribution.

⇒ Two named targets, both priced before any code: the call-plumbing category at ~30% (the seam,
which the per-function view has been under-reading at 14% because it never summed the family), and
the reopen defect at ~3.3%. Neither is the inliner, whose ceiling is 11-14.5%.

### ⛔ AND THE REOPEN DEFECT IS RETRACTED WITHIN THE HOUR: 65% OF IT IS MY OWN BENCHMARK, AND REMOVING IT COSTS NOTHING

`list_resources`' own comment says it plainly — "with no dir source on the path (the shipped MC
classpath is jars only)". `ReloadBench` runs as `-cp out;<jars>`, so `out` is a Dir source sitting
FIRST, and `read_resource`'s Dir arm probes it with a real `std::fs::read` on every lookup that the
directory does not hold. Moving the directory to the END of the classpath is the discriminator:

| | other ops per warm reload | reload times |
|---|---|---|
| dir FIRST (as measured above) | **13,492** | 6,415 / 5,938 / 6,804 / 7,378 |
| dir LAST | **4,736** | 7,626 / 6,834 / 6,412 / 6,505 |

⭐**8,756 of the 13,492 were my own benchmark's classpath directory** — the product classpath has no
Dir source at all, so two thirds of the "275× worse than java" figure never happens in production.

⛔**And the third that remains is not worth anything either: deleting 8,756 syscalls per reload moved
the reload time NOT AT ALL.** The two arms overlap completely. So `NtCreateFile`'s 3.28% of samples
is not causal — Windows answers a repeated negative lookup out of its own cache, and the sampler
attributes the entry cost without it being on the critical path. **The "~220 ms of 6,700" I wrote an
hour ago is withdrawn.**

⇒ This is the third time tonight that a category read off a profile did not survive its own control
experiment, and the pattern is worth naming: **a profile says where the instruction pointer is, not
what the wall is waiting for.** The share only becomes a price when something removes the work and
the clock moves. Of the three targets the category map named, the two structural ones —
call plumbing ≈30%, allocation ≈15% — remain unpriced but untouched by this refutation; the I/O one
is closed.

### The call-plumbing category, probed by removing work: its largest known lever is worth 2.4%

The category map says 30% of the warm reload is call plumbing against 6.03% inside compiled guest
code. Tonight's rule says a share is not a price until removing the work moves the clock, so the
first thing done with that number was an experiment, not a design. `RAVA_NO_CALL_PLAN` disables the
per-site call plan — by this ledger's own record the single largest call-plumbing change ever made
here, `-27.6% disjoint` on the interpreted call benchmark. Three pairs, pinned harness:

| metric | B (plan removed) vs A |
|---|---|
| `cpu_ms` | **+2.20% / +2.42%** |
| `cycles` | **+2.31% / +3.32%** |
| `RELOAD2_MS` | +4.76% / +8.75% |
| `RELOAD_MS` | +0.20% / +1.28% |
| pairs favouring the flag | **0 of 3** |

Every metric moves the same way and no pair dissents, so the sign is solid even though the
magnitudes sit inside the arm spreads. ⇒ **The largest lever this project has ever built for call
plumbing is worth ~2.4% of the warm reload.**

That does not bound the category — removing an optimisation adds work, it does not measure the
whole. What it does say is that the 30% is not a pool of tunable overhead waiting for a better
dispatcher: the plumbing that a plan can remove has already been removed, and what is left is the
crossing itself. The same conclusion the boot reached from the other end (row 434: the bridge OUT of
compiled code costs more than everything inside it), now reproduced on the reload with a removal
experiment instead of a profile.

### The allocation category, split: collection is 0.9%, the cost is in ALLOCATING

`RAVA_GC_STATS=1` on `--reloads 1` and `--reloads 4`, differenced over the three extra warm reloads,
prices the collector directly instead of inferring it from the profile:

| per warm reload | |
|---|---|
| GC time | **+55 ms** (~0.9% of 6,400) |
| promoted | +8.7 MB, +186,520 objects |
| arena closures minted | +41,034 |
| closure SNAPSHOTS | **+991,984**, +66 ms (~1.0%) |

⇒ **The ~15% the category map attributes to allocation is not the collector.** Minor GCs run twice
in three reloads and cost 55 ms each; everything else in that 15% is the allocate path —
`mi_malloc_aligned` 5.61% and `std/core/alloc` 6.05% — which is Rust-side transient allocation spread
through the runtime, not arena bump allocation (the arena does not call `malloc` per object).

That is the same population the ledger has already attacked three times: "an allocation made to
answer a question" (`warm_note`'s three `String`s, the interner's L1 `Box<str>`, `arena_lambda_fi`'s
two allocations rebuilding a CONSTANT). All three were removed, all three were bit-exact, and **all
four measurements were neutral.** The category is large and diffuse in exactly the way that has not
paid before.

One number in that table is worth keeping for its own sake: **a warm reload takes 991,984 closure
snapshots** — roughly one per five bytecodes of guest work — for 66 ms. It is only 1.0%, so it is not
a lever, but it is the largest single mechanical population the reload has, and any future change to
closure representation should be priced against it rather than guessed at.

## ⭐⭐⭐ CLASS HIERARCHY ANALYSIS: THE ENGINE'S LARGEST INLINE REFUSAL NOW HAS AN ANSWER THAT NEEDS NO PROFILE

`devirt.no-profile` is the biggest single refusal in the census — 8,116 of 32,033 offers, 25% — and
the previous round proved it is **unfixable as a profile**: tier-0 compiles a method on its first
entry, so a hot site is one the interpreted profiler never ran on, and the compiled-code learn hook
measured 1,061,065 entries for ONE new profile because a compiled apply never falls to the host.

CHA answers the same question without a profile: *over the currently loaded classes, how many
distinct concrete types can this site select?* Exactly one ⇒ that is the guess.

⭐**AND IT NEEDS NO INVALIDATION, WHICH IS WHY THE CHANGE IS THIS SMALL.** The value produced is
`callee.guard_class` — a SPECULATION the emitted diamond re-proves on every execution. A class
loaded later that makes the hierarchy answer stale costs a guard miss, i.e. the ordinary dispatch,
exactly as a stale profile does. CHA is a better SOURCE for a guess the engine already checks, not a
new assumption that must be policed. The whole seam is one `Option<u32>` at `inline_targets.rs`'s
`sitemono::mono_class` call.

### First build used the existing enumerator, and the enumerator was the cost

`Vm::virtual_dispatch_closure` already computed this and **no admission path had ever read it**. But
it clones every loaded class NAME per query (`loaded_class_names()` → N `String`s) and walks super
chains with string compares. Priced, 3 pairs:

| metric | B (CHA via the scan) |
|---|---|
| `RELOAD_MS` (cold, carries compilation) | **+6.82 / +8.82%** |
| `RELOAD2_MS` (**warm**) | **−4.83 / −2.78%** |
| `cpu_ms` | +1.19 / +2.89% |

⭐**A clean split: the code got better and the compiler paid more than it bought.**

### The fix was a downward index — the only child-pointing table this loader has

`super_chain_cache` and `type_closure_cache` both answer "what is this a". `ClassLoader::sub_index`
answers "who is this": one pass over the cache reading `super_class` and `interfaces`, stamped with
the hierarchy epoch and rebuilt only when a query arrives at a newer one. `concrete_subtypes(owner,
cap)` then walks the SUBTREE, and a cap of 2 turns a root like `java/lang/Object` into an immediate
refusal instead of a walk of the world.

It also does what the old enumerator structurally could not: **interface owners**. The index records
`implements` edges, so `invokeinterface` sites are eligible — that was 9,006 of the refusals in the
first build.

| | CHA via the scan | CHA via the index |
|---|---|---|
| sole implementor found | 1,785 | **2,978** |
| `ADMIT.devirt` | 1,605 | **2,473** |
| sites admitted overall | 7,967 of 44,597 | 7,483 of 41,014 (**18.2%** vs 13.7% before CHA) |
| deepest inline | d3 | **d4:6 d5:6 d6:6 d7+:4** |
| `RELOAD_MS` min vs A | +6.82% | **−0.60%** |

⭐**The engine inlines past depth three for the first time.** Every previous census read
`d4: 0 | d5: 0 | d6: 0 | d7+: 0`, and the depth cap of 8 had never been approached, let alone
reached.

**Correctness, with the flag on:** 4/4 bit-exact gates (`8790f04ace726ecf`, `37539c6c375d0c3`,
`ae44a34b42766bd4`), 23/23 differential probes matching `java -Xint`, and `ReloadBench` returning
`HASH=c956377e19071d7a` on every run.

⛔**AND THE FIRST INDEX COST THE BOOT 17.8% CPU — because I invalidated a table that never needed
invalidating.** Stamping the index with `epoch()` is nearly free on the warm reload, where the loaded
set is stable and the build happens once. On a BOOT it is pathological: classes load in their
thousands, every publication bumps the epoch, and a query per compiled method rebuilt the whole map.
Interleaved, 3 rounds: wall 37.3/38.4/32.0 against 31.5/35.9/32.2, CPU median **+17.78%**.

⭐**A downward index does not need epoch invalidation at all: a class publication only ADDS edges.**
`note_sub_edges` appends them at the two publication sites, the full pass runs once on first use, and
the regression inverts into a win — 3 rounds CPU min **−7.72%** median **−2.82%**, then 4 rounds min
−0.30% median **−3.55%**.

✅**SHIPPED DEFAULT-ON** (`RAVA_NO_CHA_DEVIRT=1` disables), on the same terms as guarded devirt
itself: every reading favourable, none against, correctness exact. Boot CPU median −2.82% and −3.55%
across two interleaved runs; warm `RELOAD2_MS` −4.83/−2.78% then −2.46/−3.12%. Not disjoint — the box
spent the evening entering and leaving episodes — so the magnitudes are indicative and the SIGN is
what carries.

⚠**The unset path is the one that was verified after the flip**, because today's other default flip
failed for exactly the opposite omission (`frames.rs:877` re-derived one fact two ways and they
disagreed only when the variable was unset). With no CHA variable in the environment: gates 4/4
bit-exact, probes 23/23, `ReloadBench HASH=c956377e19071d7a`, and `RELOAD2_MS` 6,133 / **5,477** —
the fastest warm reload recorded tonight.

⚠**Originally priced only in part and shipped OPT-IN before the index fix.** The warm pass favours
it in both independent rounds (−4.83/−2.78%, then −2.46/−3.12%) and the index removed the cold cost
(min +6.82% → −0.60%), but the box re-entered an episode during the second A/B — A-arm spreads 10.2%
and **28.1%**, both arms drifting upward across the run — so the medians there are unreadable. The
mechanism and the counts are solid; the wall verdict is not yet.

### ⭐⭐ SEVERAL TYPES ARE NOT SEVERAL BODIES — the CHA refusal that was a question about the wrong thing

CHA's largest remaining refusal was **6,071 sites a run** reading "more than one loaded implementor".
But `concrete_subtypes` answers in TYPES and the splice needs one BODY: four subclasses that all
inherit the same method have exactly one. The first build refused every answer above one, which
threw away most of its own reach.

Two halves, both small:

* **The body test.** Resolve the method for each subtype and require ONE declaring class —
  `Arc::as_ptr` equality on the `resolve_method` result, which is thread-local memoised, so the
  test is exact and nearly free. Anything else refuses exactly as before.
* **The widened guard.** The extra ids become a DISJUNCTION of `class_id` compares in the same
  diamond (`guarded_inline.rs`, `InlineCallee::guard_class_more`). It widens what the guard
  ACCEPTS, never what runs behind it — so this is still a guarded speculation and still needs no
  invalidation. A receiver that is none of the ids lands on the ordinary dispatch, unchanged.

| | sole type | one body |
|---|---|---|
| CHA answers found | 2,978 | **8,651** |
| of which span several types | — | **3,463** |
| `ADMIT.devirt` | 2,473 | **8,240** |
| inlines at depth 0 | 4,357 | **10,321** |

⚠**And the first reading of this was WRONG, caught before it was written down.** A single profiled
run read `RELOAD2_MS` 10,189 against a 6,133 baseline and looked like a clear regression from code
bloat. `RAVA_CHA_TYPE_CAP` was added so the trade could be bisected inside ONE binary (`=1` restores
the shipped sole-implementor behaviour exactly), and the interleaved sweep 1/2/4/1/2/4 says there is
no difference at all:

```
cap=1  [8580,7690,7492]  [6610,7310,7103]
cap=2  [8007,6947,7214]  [7340,7100,6979]
cap=4  [7772,6882,7275]  [7320,7322,7029]
```

Three arms, complete overlap. The 10,189 was one run of a census-armed binary on an episodic box —
**the exact trap that cost four retractions earlier the same evening, checked this time before the
conclusion was recorded rather than after.**

⇒ Coverage tripled at no measured cost. Gates 4/4 bit-exact, probes 23/23, `ReloadBench
HASH=c956377e19071d7a`.

## ⛔⭐⭐⭐ CHA ADMITS 8,240 SITES AND REMOVES ZERO OF 48.8 MILLION CROSSINGS — measured by COUNT, not by clock

The box spent the night entering and leaving episodes, so every wall reading of CHA was unreadable.
Counts do not care. `RAVA_CALLBACK_STATS` arms the machine-code → host crossing counters, and
`[CB]` is now emitted from the profile dump as well as the interpreted-op heartbeat — which a
compiled workload never reaches, so **the one population this engine measures EXACTLY rather than by
sampling was invisible on precisely the workloads it describes.**

Warm `ReloadBench`, two runs per arm, interleaved:

| crossings | CHA ON | CHA OFF |
|---|---|---|
| `invoke` | 48,845,846 / 48,822,337 | 48,807,790 / 49,262,944 |
| `dmiss_res` | 16,279,798 / 16,283,163 | 16,358,176 / 16,382,017 |
| `getfield` | 11,093,738 / 11,118,245 | 11,106,348 / 11,093,603 |
| `direct` | 10,696,499 / 10,733,109 | 10,700,065 / 10,696,597 |

⭐**Every column is identical to within run-to-run drift.** CHA admitted 8,240 devirtualised sites,
tripled inline coverage, and took the engine past inline depth three for the first time — and it
removed **none** of the 48.8 million host crossings a warm reload performs.

⇒ **The admitted sites are not the hot ones, and this is the cleanest statement yet of why the whole
inlining line keeps measuring neutral.** Six rounds of it — rooted splice, control flow, depth,
context sensitivity, CHA, CHA widened — every one correct, several structural, and not one of them
touched the population that dominates. Inlining guest bytecode cannot remove a crossing whose callee
has no bytecode: the ledger already counted 59.4% of crossings leading into `java/*` served by Rust
natives, and `rava::natives` is 6.09% of the profile beside `rava::vmstate`'s 20.09% of plumbing to
reach it.

**CHA stays ON.** It is correct (gates 4/4 bit-exact, probes 23/23 on every build), it is strictly
less work at run time, and its cost is bounded — but it must not be quoted as a win, and the line it
belongs to is now closed by a count rather than by a noisy clock.

⇒ What this leaves is the thing the category map already pointed at and this measurement now
corners: **48.8 M crossings whose far side is a Rust native.** Not a dispatcher to tune, not a body
to splice — a boundary that exists because `java.util` and friends are implemented in Rust rather
than compiled from bytecode. Row 1833 measured the naive inversion (`java.util` as compiled
bytecode: >10× SLOWER), so the answer is neither "inline more" nor "compile java.util", and naming
it correctly is the next session's first job.

### ⛔ AND THE FAR SIDE OF THE CROSSINGS IS FLAT TOO — top callee 3.40%, and it is GUEST code

The previous entry ended by naming the survivor as "48.8 M crossings whose far side is a Rust
native". **That characterisation is wrong, and `[INVCEN]` — a new by-name census on the crossing
itself (`RAVA_INVOKE_CENSUS=1`, inert otherwise) — says so directly.**

First it corrected the arithmetic: wiring the census into `rava_invoke` caught only **78,640**
crossings, so `[CB] invoke=48.8 M` is the whole invoke FAMILY, not that one callback. Moved to
`rava_dispatch_miss` — the largest column, `dmiss_res` ≈16.3 M — the distribution reads:

| share | callee |
|---|---|
| 3.40% | `net/minecraft/world/level/block/Block.getShape(…)` |
| 2.91% | `[Lnet/minecraft/core/Direction$Axis;.clone()` |
| 1.74% | `java/lang/Integer.compareTo` |
| 1.62% | `java/util/function/Function.apply` |
| 1.39% | `java/lang/String.getBytes(Charset)` |
| 1.37% | `net/minecraft/util/StringRepresentable.getSerializedName()` |
| 1.24% | `java/lang/Object.hashCode()` |
| 1.21% | `net/minecraft/core/Direction$Axis.choose(III)` |

⭐**The top callee is 3.40% and it is Minecraft's own bytecode, not a native.** So the dominant
crossing is interface and virtual dispatch over MC's own hierarchy that the inline caches miss —
`Block.getShape`, `StringRepresentable.getSerializedName`, `Property.getPossibleValues`,
`DataResult.isSuccess`, the DFU codec interfaces. An intrinsic for `Map.get` would not touch it, and
neither would any single intrinsic: there is no single anything.

⇒ **That is the sixth independent slicing of this workload to come back flat**, and the list is now
long enough to be the finding rather than a disappointment: host functions (top 2.2%), guest methods
(top 0.7%), inline refusals (fixed, and the fix removed no crossings), allocation sites, I/O, and now
crossing callees (top 3.4%). Every axis is a long tail. The engine is not losing 4× in one place; it
is losing a few percent in fifty places, and the only levers that have ever paid here were CONTRACT
changes — the async offload (12.4×), the lane drain scope (2.4×) — never a distribution attacked at
its head.

**Shipped: `[INVCEN]` itself.** Gates 4/4 bit-exact, probes 23/23 with it wired in; the crossing path
pays one relaxed bool read when it is not armed.

## ✅⭐⭐⭐ THE ARENA-MAP DOOR WAS NOT LOCKED — DISJOINT −11% CPU, AND THE +48% IN THIS LEDGER WAS STALE

Row 434's verdict ended: *"Шаг 1 = арена-представление коллекций без регресса (`RAVA_ARENA_MAPS`
сейчас +48% на загрузке) — весь риск там; не выйдет = переписывание не начинается."* That number has
been the reason the whole "remove the handle heap" line was recorded as not-startable. **It is
stale.** Re-measured on today's engine, interleaved, 4 rounds:

```
wall  ON  32.0  32.2  29.6  30.2      max 32.2
      OFF 32.5  36.2  36.8  35.2      min 32.5
cpu   ON  35.1  35.3  32.4  33.6      max 35.3
      OFF 35.5  39.4  40.1  38.1      min 35.5
```

⭐**Every ON below every OFF on BOTH statistics — DISJOINT — CPU min −8.73%, median −11.35%**, and
that on a box whose background is a user `javaw` holding 8 GB and half the CPU, which inflates both
arms equally and makes a disjoint result harder, not easier.

Counted beside it, on the warm reload, where counts do not care about the box:

| | arena maps OFF | ON |
|---|---|---|
| arena-lambda births | 1,387,143 | **302,879** (−78%) |
| GC time | 370.8 ms | 312.3 ms |
| objects promoted | 1,354,088 | 1,222,286 |
| `ReloadBench` HASH | `c956377e19071d7a` | `c956377e19071d7a` |

**Shipped DEFAULT-ON** (`RAVA_NO_ARENA_MAPS=1` restores the byte-identical `$map` path;
`RAVA_ARENA_MAPS=1` stays accepted so every existing script keeps meaning what it meant). Verified on
the UNSET path after the flip — the omission that broke today's stack-map flip — gates 4/4 bit-exact,
probes 23/23, `HASH=c956377e19071d7a`.

⇒ **This is the first structural win of the session and it re-opens the only line that was ever
priced above 14%.** The precondition row 434 set for removing the handle heap is met: arena
collections do not regress, they pay. What follows is the rest of that plan, not another
distribution.

### The stale-flag sweep the arena-map result argued for: two sets tested, both refused

Arena maps had been sitting behind an opt-in flag on a **+48%** verdict that turned out to be −11%
DISJOINT. That is a strong argument that other long-standing opt-in flags carry stale verdicts too,
so the obvious next move is to re-price them rather than to build anything. Interleaved boot A/B,
4 rounds each, same harness and same box:

| flag set | CPU min | CPU median | rounds favouring ON | verdict |
|---|---|---|---|---|
| `RAVA_JIT_GUEST_SUB` + `RAVA_WORKER_HOT` + `RAVA_JIT_NS_BYTECODE` | **+8.14%** | **+12.20%** | 0 of 4 | ⛔ stays off |
| `RAVA_ALIST_GET_INLINE` + `RAVA_ALIST_SIZE_INLINE` + `RAVA_DALIST_GET_INLINE` | +2.21% | +2.80% | 2 of 4 | ⛔ inside the noise, stays off |

The first is the notable one. `RAVA_JIT_GUEST_SUB` was recorded as **+3.85% CPU DISJOINT in its
favour** and was left off only because wall had never been measured; the worker-hot note and the
namespace-bytecode gate were measured with it. Re-priced together on today's engine they are
**12% SLOWER on the median and lose all four rounds** — the opposite sign, decided in one
measurement.

⇒ Stale verdicts cut both ways: one flag was a locked door that had opened, another was a recorded
win that had rotted. Neither could be known without re-running, and re-running is minutes.

⛔**The sweep stopped there, and the reason is recorded so the third set is not written off.**
`RAVA_ARENA_NATIVE_NEW` + `RAVA_ARENA_SYNTH` got two rounds — `OFF wall 39.9 / 158.0`,
`ON wall 41.5 / 231.4` **at CPU 43 and 75** — a process that is waiting, not computing. The box was
then measured directly: **99.9% CPU busy and 2,635 MB free of 32,547**, against a benchmark that
asks for a 4,096 MB cap, with a user `javaw` holding 8,326 MB and 24,539 CPU-seconds. Under memory
pressure that deep the harness measures the swapper. **That set is UNTESTED, not refused.**

## ⭐⭐⭐⭐ INSIDE A COMPILED BODY, 70% OF THE TIME IS NOT IN THE EMITTED CODE

`[COMPILED-SPLIT]` answers the question every flat slicing was circling, and it answers it with one
number that does not move:

```
8,895 samples under a compiled body: 2,709 (30.5%) in EMITTED CODE, 6,186 (69.5%) in host callbacks
```

Eight independent runs, both CHA arms, warm reload: **27.9% / 28.9% / 28.9% / 28.9% / 30.5% / 30.7%
/ 30.8% / 33.2% in emitted code.** The rest — consistently ~70% — is Rust host callbacks reached
from inside a method the JIT compiled.

⭐**Compose it with the other bound and the picture closes.** Samples inside compiled code at all are
11-14.5% of the warm reload; of those, 30% are in machine code. ⇒ **the emitted code this engine
produces executes roughly 4% of the workload.** HotSpot's compiled code runs essentially all of its
own body. That is the 3-4× in one sentence, and it is why every distribution attacked at its head
came back flat: the flatness is a property of the CALLBACK population, and the callback population is
the design.

The callbacks themselves are flat too — 623 distinct host callees, top-60 = 66.8%, and the head is:

| share of callback samples | callee |
|---|---|
| 3.29% | `vm::iterate::compare` |
| 2.77% | `ZwWaitForAlertByThreadId` (thread parking) |
| 2.25% | `vm::jit_natives::jit_fu_invoke` |
| 1.99% | `opcodes::invoke_virtual::invokevirtual` |
| 1.88% | `mi_malloc_aligned` |
| 1.69% | `vm::lambda::invoke_lambda_parts` |
| 1.62% | `tlcache::resolve_get` |
| 1.44% | `drop_in_place<Option<Value>>` |

⇒ **The lever is not "which callback to remove" — it is "why does emitted code call out at all".**
Every previous round asked the first question. `RAVA_ARENA_MAPS` is the only thing that ever moved
the second, which is exactly why it is the only structural win of the session: it changes what a
collection IS, so the emitted code stops needing a host to interpret it.

⚠And the two bounds must be read together, because separately each is misleading: "a perfect inliner
buys ≤14.5%" understates the problem, and "70% of compiled time is callbacks" overstates how much of
the WHOLE workload that is. The product — ~4% of the workload runs as machine code — is the honest
figure and the one to carry.

### The array pool's budget refuses 3.7 M buffers a reload — and raising it buys nothing

Arrays are the population that can never be arena-allocated (`jit/emit/ops/objects.rs`, the `0xbc`
comment: an array is a tagged handle into the `ArrayDesc` spine, so a bump allocation would hand
every consumer a raw pointer where it expects a handle index; `register_uniform_ref_class` panics by
construction and `tlab_alloc_array` has zero call sites). So the pool is the only thing standing
between 7.7 M array births and 7.7 M host allocations — and its census says the byte budget, not the
length rule, is what limits it:

```
pool ON: served 1,944,008 births (25.2%), refused len 412,464 budget 3,659,420
```

`RAVA_POOL_BUDGET_MB` makes that testable in one binary. Interleaved 8/64/8/64:

| budget | pool served | refused on budget | holding | reload |
|---|---|---|---|---|
| 8 MB | 23.9% / 27.4% | 3,728,718 / 3,120,730 | 28 / 25 MiB | 6,920 / 6,328 |
| **64 MB** | **29.8% / 33.7%** | **897,550 / 958,572** | **97 / 102 MiB** | 7,089 / 6,120 / 6,917 / 6,383 |

⭐**Budget refusals fall 75% and roughly 700,000 host allocations a reload disappear — and the clock
does not move.** The arms overlap completely, while resident memory grows **+72 MiB** on an engine
whose RSS is already double java's.

⛔**So it does not ship.** This is the fifth time this session that removing real, counted work
changed no wall time, and it is the same lesson each time: **a host allocation avoided is not a
millisecond saved unless the allocation was on the critical path.** The default stays 8 MB; the knob
stays so the next round can re-test instead of re-derive.

## ⛔⭐⭐ RAVA DOES NOT RUN VANILLA MINECRAFT — the obfuscated jar panics on both platforms

Setting up the clean Linux box surfaced something the whole benchmark suite had been hiding: every
harness here runs `versions/1.21.11_unobfuscated`, a DEOBFUSCATED jar. Pointed at Mojang's own
shipped server jar — downloaded from `piston-data.mojang.com`, extracted by its own bundler — the
engine dies before the server starts:

```
thread '<unnamed>' panicked at opcodes/arena_cells.rs:485:
PHASE5: arena getfield of non-layout field id=600 on class Some("amt$a"); layout ids=[598,599]
```

⭐**And it is NOT a Linux problem.** The identical run on Windows against the identical vanilla jar
gives the identical panic on the identical class, only with different interned ids
(`id=546, layout ids=[544,545]` on the server, `id=600, layout=[598,599]` locally). Two platforms,
one bug.

The panic site names its own two candidate causes — a PARTIAL layout (a super transiently
unloadable, showing a short id list) or a MIS-RESOLVED field ref (a full layout that simply lacks the
id). Both were probed:

| probe | result |
|---|---|
| `RAVA_STALE_SOFT=1` (degrade the read instead of panicking) | server still fails to start — the null propagates, so this is the only blocker AND it is load-bearing |
| `RAVA_NO_ARENA_FIELDLESS=1` (drop the fieldless-super completeness shortcut) | identical panic |
| `RAVA_NO_VALUECOMPUTER=1` (drop the value-computer completeness shortcut) | identical panic |

So neither completeness shortcut is the one marking the chain complete, which points at the second
candidate — and there is a mechanism that fits it exactly: **field ids are interned by NAME ALONE**
(`Heap::field_id(&self, name: &str)`, one process-wide `FastMap<Box<str>, u32>`). On deobfuscated
bytecode a shared field name across classes is harmless, because each class's own layout maps that id
to its own slot. On OBFUSCATED bytecode every class declares `a`, `b`, `c`, so a subclass shadowing
its super's `a` and a sibling's unrelated `a` all collapse onto one id — a layout can hold only one
slot per id.

⚠**Stated as a lead, not a conclusion: the discriminator was not run.** The panic prints ids, not
names, so "the layout is complete and the ref is wrong" versus "the layout is short" is still open,
and the next step is to print the field NAME and the class's super chain at that site.

⇒ This matters beyond the crash: **every performance number this project has ever recorded was
measured on a deobfuscated jar**, which is a different class-name population than the one real
servers run. Nothing about the flat profiles or the arena verdicts is invalidated by that, but the
engine's headline claim — "runs Minecraft" — is currently true only of the deobfuscated build.

## ⛔⛔⭐⭐⭐ THE ENGINE ROUTES EXECUTION BY MINECRAFT CLASS NAMES — the no-adapt rule, violated structurally

The vanilla-Minecraft panic was diagnosed by teaching the failure site to print NAMES instead of ids
(a process-wide field-name mirror, since that panic fires on a background datapack thread where
`vm_shared()` is `None`). The answer is unambiguous:

```
PHASE5: arena getfield of non-layout field id=600 (registry) on class Some("amt$a");
        layout ids=[598,599] names=[amt$a#a, amt$a#b]
```

The object's layout carries OBFUSCATED, class-qualified names. The field being read is **`registry`**
— a DEOBFUSCATED name that does not exist anywhere in vanilla bytecode. So the access did not come
from the guest at all: it came from **our native layer, which identifies Minecraft classes by their
deobfuscated names and then expects fields those names imply.**

### Counted, not asserted

**Nine name-keyed routing predicates**, each deciding EXECUTION — arena vs handle heap, native
implementation vs guest bytecode, IO handling — from a class-name substring:

`is_native_backed_alloc` · `birth_native_backed_alloc` · `birth_native_backed_ancestor` ·
`native_served_prefix` · `route_native_map` · `is_native_map` · `is_entry_type` ·
`is_native_io_class` · `is_collection`

**31 distinct `net/minecraft/*` literals and 14 `com/mojang/*` literals** are compiled into the
engine. Not only in tests — `net/minecraft/core/Holder`, `core/Holder$Reference`, `core/Registry`,
`core/NonNullList`, `nbt/CompoundTag`, `resources/RegistryFileCodec`, `ReportedException` appear in
production paths: `class_loader/loader.rs`, `map/classes.rs`, `natives/cascade.rs`,
`natives/cascade_table.rs`, `natives/cascade_table2.rs`, `native_contract.rs`, `opcodes/invoke.rs`,
`kernelscan.rs`.

⛔**This is the project's own no-adapt rule broken at the level of the design, not at one call site.**
The rule permits bytecode, CFG, descriptors, resolved dispatch and effect summaries as evidence, and
forbids routing on names. These predicates route on names, and they route the two decisions that
this ledger has just spent a session proving are the ONLY ones that move the clock: whether an object
lives in the arena, and whether a call leaves machine code.

⇒ **Two consequences, both concrete.** First, "rava runs Minecraft" is true only of a deobfuscated
build; Mojang's shipped jar crashes identically on Windows and Linux. Second, and worse for the
numbers: **every measurement this project has ever taken was taken on the one input the engine was
shaped around.** The flat profiles, the arena verdicts and the crossing counts are all still
internally valid — they compare rava to rava — but a comparison against HotSpot on a workload the
engine was fitted to is not the general claim it reads as.

## ⭐⭐ A PARTIAL LAYOUT COULD FREEZE A CLASS ID FOREVER — fixed, and it was not the vanilla crash

Seven agents traced the vanilla-Minecraft panic. They found a real bug on the way, with file:line:

`nextgen::register_class` is **FIRST-WINS and discards the layout it is handed** on a name hit, and
the per-thread `ClassMirror` built from that record is cached on the explicit claim that "a class id,
once observed, names the same immutable record forever" — nothing in the tree ever rewrites an
`ArenaClass.layout`. Meanwhile `Vm::arena_new_site_route` called `register_class` **BEFORE** its own
completeness test:

```
let cid = crate::nextgen::register_class(&arc, &layout);   // ← partial layout, id minted
let nf  = layout.len() as u32;
if self.class_layout_cache…contains_key(name) { Arena { cid, nf } } else { Unstable { partial: … } }
```

⇒ One visit to a `new` site while the class's super was TRANSIENTLY unloadable — re-entrancy, or
mid-`<clinit>` on another thread — minted the id against a short layout, and **every later birth of
that class, including the ones that correctly got the complete layout and the right cell count, kept
answering `slot_of` from the frozen short record.** The completeness test now runs FIRST and an
incomplete chain mints nothing; `RAVA_PARTIAL_ARENA_CID=1` restores the old behaviour as the A/B arm.

It also closes a second hole the same trace named: `arena_new_site` flattens
`SiteRoute::Unstable { partial }` into its return value, and the JIT's inline TLAB bump for `new`
(`jit/emit/ops/objects.rs`, opcode `0xbb`) trusts that pair with **no completeness check of its
own** — the comment there asserting that the predicate "has exactly one non-permanent term,
`initialized`" was wrong.

Gates 4/4 bit-exact, probes 23/23.

⛔**And it does NOT fix vanilla Minecraft, which is the useful part of the result.** With
`RAVA_REGCLASS_LOG=1` (new: prints every mint, and every re-registration whose length DIFFERS from
the record) the offending class reads `[REGCLASS] mint amt$a nf=2` — minted **once**, and **zero**
differing re-registrations across 270 mints. So the layout is not a frozen partial: `amt$a` genuinely
has two fields and its chain is complete.

⇒ The panic is therefore the OTHER candidate the site names: a **mis-resolved field ref**. `registry`
printed UNQUALIFIED, and `Vm::resolve_field_id` only falls back to a bare `intern_field(name)` when
its owner-chain walk breaks — so the field's OWNER did not resolve, and the resulting id was then
used to read an `amt$a` receiver that never had that field. The next step is to print the field ref's
OWNER at the fault, which the panic site does not currently carry.

## ✅⭐⭐⭐⭐ RAVA NOW RUNS VANILLA MINECRAFT — and the fix was DELETING an adaptation

```
[ServerMain/INFO]: Loaded 1470 recipes
[ServerMain/INFO]: Loaded 1584 advancements
[ServerMain/INFO]: Starting minecraft server version 1.21.11
[ServerMain/INFO]: Starting Minecraft server on *:25599
[ServerMain/INFO]: Preparing level "world"
```

Mojang's own shipped, obfuscated server jar, extracted by its own bundler, still serving after two
minutes. Before this it panicked in under a second, identically on Windows and Linux.

**The bug, traced end to end.** `java/lang/runtime/ObjectMethods.bootstrap` takes the record's
components as `args[1]`, a `;`-joined STRING. The JVM uses that string for one thing — the text
`toString()` prints — and reaches the VALUES through the getter MethodHandles in `args[2..]`. An
obfuscator therefore has no reason to rewrite it, and Mojang's does not. `javap` on the crashing
class says exactly that:

```
final class amt$a extends java.lang.Record {
  final amo a;
  final amo b;            ← the fields, obfuscated
}
  #32 = String  // registry;identifier    ← the bootstrap name string, NOT obfuscated
```

This engine resolved COMPONENT FIELDS from that string. So it asked for `amt$a.registry`, the owner
walk found no such field and broke at `java/lang/Record` (no bytecode shipped for it), the id fell
back to the UNQUALIFIED `registry`, and the first arena read with an id that belongs to no class's
layout panicked.

⭐**On a deobfuscated jar the string and the fields agree — which is precisely why this survived: the
engine was reading a coincidence of ONE input as a rule.** That is the no-adapt rule broken in its
purest form, and it was invisible because every benchmark in this tree runs that one input.

**The fix removes the assumption rather than special-casing around it.** JLS 8.10.1: a record's
components are declared, in the order written, as private final instance fields of the record class.
So the component list is read from `frame.class.file.fields` (instance fields, declaration order) —
bytecode-derived, obfuscation-independent, the same answer on both jars. The string is kept only for
the rendered name.

**Verified on the workload the engine was shaped around, to show nothing was traded away:** gates 4/4
bit-exact (`8790f04ace726ecf`, `37539c6c375d0c3`, `ae44a34b42766bd4`), probes 23/23, `ReloadBench
HASH=c956377e19071d7a`, and `RELOAD2_MS` **5,194 / 5,109** — the fastest warm reload recorded in this
session.

## ⛔⭐⭐⭐ THE WALL GAP IS WORK, NOT PARALLELISM — and the arithmetic that said otherwise was mine

On a quiet box, three interleaved pairs, the cleanest head-to-head this project has recorded:

| | rava | java | ratio |
|---|---|---|---|
| wall | 25.2 / 25.2 / 25.5 | 6.3 / 6.3 / 6.0 | **4.0×** |
| CPU | 28.5 / 28.3 / 29.2 | 20.7 / 20.4 / 18.5 | 1.45× |
| cores | 1.13 / 1.12 / 1.15 | 3.30 / 3.26 / 3.07 | 2.9× |
| `Done` | 0.346 / 0.347 / 0.353 | 0.218 / 0.201 / 0.217 | 1.65× |
| peak RSS | ~1,060 MB | ~553 MB | 1.9× |

⛔**The obvious reading — "we are only 1.45× on work and 2.9× on cores, so two thirds of the wall gap
is parallelism" — is WRONG, and it is worth writing down because it is arithmetically seductive**
(1.45 × 2.83 = 4.1, exactly the wall ratio). Java's extra cores are its own C1/C2 compiler threads:
`java -Xint` uses 1.15-1.22 cores on this workload, and our compiler already runs on a background
thread whose worker count was re-tested here — four workers instead of one add **0.04 cores** and
move no wall (`background_jit.rs` records the same result from an earlier round).

⇒ Comparing APP work to APP work: java's is ~6.3 CPU-seconds, ours is ~28.5. **We do roughly 4.5× the
work HotSpot does for the same program**, and java hides its compilation behind cores we do not need
to match. That converges with the independent measurement from the other end — **the emitted code
executes ~4% of the workload** — rather than competing with it.

### The instrument did not move today, and that is the honest read

`[COMPILED-SPLIT]` after this session's fixes: **29.8% / 29.0%** in emitted code — inside the same
27.9-33.2% band as before. Today's work was correctness (a frozen partial layout, record components
resolved from a `toString` string), and correctness fixes are not expected to move it. The top host
callee from compiled code is `Vm::compare` at 3.66%, then `ZwWaitForAlertByThreadId` 2.53%,
`rava_indy` 2.27%, `jit_fu_invoke` 2.08%, `invokevirtual` 1.87% — the call family again, still flat.

### Three more stale-verdict flags re-priced, all refused

| flag set | CPU min | CPU median | verdict |
|---|---|---|---|
| `RAVA_ARENA_SYNTH` alone | +7.53% | +0.47% | ⛔ overlapping, stays off |
| `RAVA_SORT_KEY_MEMO` + `RAVA_PURE_MEMO` + `RAVA_VIRT_CLASS_MEMO` | **+32.99%** | **+26.88%** | ⛔ **DISJOINT the wrong way**, all four rounds worse |

Running total for the sweep this session: **one flag out of five sets was a locked door that had
opened (`RAVA_ARENA_MAPS`, −11% DISJOINT, shipped); four sets were refused by measurement**, one of
them a previously RECORDED win that had rotted.

## ⛔⭐⭐⭐ THE EMITTED-CODE FRACTION DOES NOT PREDICT THE WALL — retracting my own instrument

Two entries ago this ledger proposed `[COMPILED-SPLIT]`'s emitted-code share as the instrument every
future change should be judged by, on the reasoning that a change which does not raise it cannot move
the wall. **That was a hypothesis presented as a tool, and it is now refuted by the one change that
demonstrably paid.**

`RAVA_ARENA_MAPS` measured DISJOINT −11.35% CPU median on the boot, every ON round below every OFF
round on both statistics. Interleaved ON/OFF/ON/OFF with the profiler armed:

| arm | emitted code |
|---|---|
| arena maps ON | 28.1% |
| arena maps OFF | 28.2% |
| arena maps ON | 28.2% |
| arena maps OFF | 29.8% |

⭐**Indistinguishable.** The one structural win of the session does not move the fraction at all.

The same test from the other side agrees. `RAVA_ARENA_NATIVE_NEW` — which turns out to be the gate on
`try_arena_alloc_by_name`, already wired at the `Optional` mint site in `stream/helpers.rs:16` —
removes **316,000 handle births a warm reload** (4,599,481 → 4,283,706, −6.9%; `java/util/Optional`
6,230 → 5,982) and the fraction reads **27.6% → 27.7%**. Three hundred thousand objects moved from
the handle heap to the arena, and the instrument does not notice.

⇒ **Retracted.** The 27.9-33.2% band is a real and remarkably stable fact about this engine — the
emitted code genuinely runs about a third of compiled-body time, and about 4% of the workload — but
it is a DESCRIPTION, not a predictor. Judging a change by it would have rejected the only change that
worked.

**This is the sixth time this session the same rule bit**: a share is not a price until removing the
work moves the clock. It bit hardest here because the share was mine, freshly measured, stable across
eight runs, and mechanistically plausible.

### Two more flag sets priced against the wall itself, both refused

| flag set | CPU min | CPU median | verdict |
|---|---|---|---|
| `RAVA_ARENA_SYNTH` alone | +7.53% | +0.47% | ⛔ overlapping — and by COUNT it is nearly inert: `Optional` births 5,364 → 5,239 |
| `RAVA_PRIM_ABI` + `RAVA_REF_ABI` | −3.58% | +5.35% | ⛔ opposite signs, arms overlap, baseline drifted 25.2 → 30.5-33.7 mid-run |

⇒ The standing position is unchanged and now better evidenced: **only the wall judges the wall**, the
profile is flat on every axis tried, and the single lever that ever moved it moved nothing else that
can be measured cheaply.

## ⭐⭐⭐ THE BOOT HARNESS HAS 16.7% INTRINSIC NOISE ON AN IDLE BOX — and the noise is OURS

Challenged to prove the box was noisy rather than assert it, the answer came out sharper than the
claim. The box: **CPU 4-11% busy, 7.5 GB free, nothing running.** The null test — eight boots of the
SAME binary with the SAME configuration, the "flag" being a name no code reads:

```
wall   ON  29.4  26.8  28.8  30.7      OFF  29.1  31.5  27.0  28.3
```

⭐**27.0 to 31.5 within one arm — 16.7% — with nothing else on the machine.** So the variance is not
contention. It is the boot itself: a world copied to disk, a cold page cache, and above all **this
engine's own non-determinism** — the background compiler gets through a different set of methods each
run, compilation order changes what is compiled at all, and the collector fires at different points.
Every boot does physically different work.

⇒ **The boot cannot resolve anything smaller than ~17%, and every flag A/B run against it this
session was doomed before it started.** The warm reload, in-process and past warm-up, reads an
**A-arm spread of 1.1-1.8%** on the same box in the same minutes — a factor of ten better. The ledger
said this when `ReloadBench` was built ("`RELOAD2_MS` is the metric to price on") and this session
drifted off it.

### Re-priced on the precise instrument, and now they have verdicts

| flag | cycles min / median | A-arm spread | verdict |
|---|---|---|---|
| `RAVA_ARENA_NATIVE_NEW` | **+1.15% / +1.41%** | 1.1% | ⛔ **DISJOINT the wrong way** — every B above every A. A real, small loss, decided instead of guessed |
| `RAVA_ANEWARRAY_SCRATCH` | +0.03% / +1.14% | 0.4% | ⛔ overlapping, no win |

The first is the useful one: on the boot harness it read "+7.53% min, +0.47% median, arms overlap",
i.e. nothing. On the reload it is a clean disjoint loss. **Same flag, same box, same hour — one
instrument says "no answer" and the other says "no, and here is the sign".**

## ✅⭐⭐⭐ FOUR COMPILER THREADS, DISJOINT −2.9% — the old "neutral" was the instrument, not the flag

`background_jit.rs` carried a recorded verdict: four workers instead of one "did 0.04 cores of extra
work and moved no wall", measured by alternating BOOTS. The null test in the entry above prices that
harness at **16.7% intrinsic spread on an idle box**, and names the cause — the background compiler
gets through a different set of methods every run. ⭐**A harness whose noise IS compilation scheduling
cannot price compilation scheduling.**

Re-priced on the warm-reload harness (A-arm spread **0.9%**), 4 pairs interleaved:

| metric | B (4 threads) vs A (1) | |
|---|---|---|
| `RELOAD_MS` (cold — carries compilation) | **−2.73% / −2.89%** | **every B below every A — DISJOINT** |
| `WINDOW_MS` | **−1.80% / −1.89%** | **DISJOINT** |
| `cycles` | +2.59% / +2.77% | DISJOINT the other way |
| `RELOAD2_MS` (warm — no compilation) | +4.55% / +0.69% | overlapping |
| `cpu_ms` | +1.54% / +2.20% | overlapping |

⭐**That is exactly the shape parallel compilation must have**: the same work finishes sooner in wall
terms and costs more total cycles, and the steady state — the warm pass, which compiles nothing —
pays nothing. A result that had the wrong shape would be suspect; this one has the right one.

⛔**Eight is worse, and the same instrument says so cleanly**: `RELOAD_MS` −3.63%/−2.36% with the arms
OVERLAPPING, while `RELOAD2_MS` +1.73%/+3.69%, `cpu_ms` +3.44% and `cycles` +3.33% are all DISJOINT
the wrong way. Eight workers start charging the warm pass for eight `JITModule`s. **Four is an
optimum, not a direction** — the same shape the JIT threshold has (row: both sides of the default
collapse worse than no JIT at all).

**Shipped default 4** (`RAVA_BG_JIT_THREADS=<n>` still overrides, 1..8). Gates 4/4 bit-exact, probes
23/23, `ReloadBench HASH=c956377e19071d7a`, and a plain run reads `RELOAD_MS=5,998 WINDOW_MS=9,610`
against the 6,363-6,641 the one-thread arm produced in the same session.

## 🔬 THE FULL QUEUE WAS REPLANNING THE SAME HOT METHODS — opt-in early permit, correctness-gated and unpriced

The `17,652 / 19,476` queue-drop ratio was not a population of unique methods. A `Full` submit put
the method back in `Cold(n2)` with `n2` already at the threshold, so the next entry immediately ran
the complete planning path again: bytecode walks, inline/native/static/arena maps, owned field-id
keys and static-cell reservations were built before `try_send`, then destroyed and rolled back.
The counter was therefore a retry storm, not evidence that 17,652 useful bodies were lost.

The current configuration also made the old queue experiment stale: four compiler workers force
`capacity = max(configured_depth, threads * 4) = 16`, so `RAVA_BACKGROUND_JIT_QUEUE=1`, `4`, `8`
and `16` are identical. A future depth re-price must compare 16 with a meaningful larger value such
as 32.

`RAVA_BG_JIT_EARLY_RESERVE=1` is now an opt-in tracer. It acquires a non-cloneable RAII capacity
permit after the existing side-effectful callee-first precompile but before `jit_plans`. A full
queue therefore returns to the interpreter before constructing work that would be discarded. The
envelope holds the permit until worker `recv`; planning failure, channel `Full`, disconnect and every
other early return refund through `Drop`. The unset path keeps the same bounded FIFO and retryable
transport-failure fallback. `Full` and `Disconnected` are no longer conflated in the queue-full
counter.

Correctness evidence on a separately built release binary, both flag SET and UNSET:

- five gate programs pass with the exact expected hashes (`chunkfill`, `ringscan`, both lane forms,
  `nbtsave`);
- all 23 differential probes match `java -Xint`;
- five deterministic permit tests pass, including concurrent bounded acquisition and refund after
  `recv`, `Full` and disconnect.

⛔**No performance verdict yet.** Identical null arms spread 19.0% on `RELOAD2_MS`, 15.7% on CPU
and 16.0% on process cycles, so queue depth, early permit and TLS were deliberately not priced. A
pre-existing `rava.exe` PID 8616 (`ReloadBench --reloads 2`, created 03:57:49) was initially suspected,
but a later authoritative sample found one suspended waiter, zero CPU-seconds over five seconds and
799 MB RSS. It was not started by this session and was not terminated, but it also does not explain
the process-cycle spread; the failed null remains unexplained rather than being assigned a false
cause. The harness's paired-summary cast also collapsed an `Object[]` to one delta under Windows
PowerShell; that reporting bug is fixed, but it does not rescue this batch.

## ⛔ THE CLI SHADOWS THE TIER-0 THRESHOLD — and loop eager still violates NO-ADAPT

The actual product CLI resolves default `jit_threshold=512` and `jit_cap=4096`, then writes both
values into the environment before constructing the VM. That makes the engine fallbacks
`tier0 ? 16 : 512` and cap 16384 unreachable on the CLI path. Thus the standing
`RAVA_JIT_TIER0=1` does not imply threshold 16; the checked-in `rava.toml` also explicitly pins 512.
This is a configuration-contract defect, but threshold 16 has a history of exposed miscompiles and
cannot be flipped from static reasoning or noisy boot measurements.

More fundamentally, invocation-count eligibility is not in the NO-ADAPT allowlist, and the current
default-on loop-eager arm routes specially on `ENTRY_MAIN_CODE` and `name != "<clinit>"`. The
replacement direction is a class-load-time immutable compile plan derived only from bytecode, CFG,
descriptors, resolved dispatch and effect summaries: structurally admitted loops/callback-free
closures, compiled callee-first by SCC, with no `main` or `<clinit>` exceptions. The numeric
threshold remains diagnostic-only until that structural policy exists.

## 🔬 LOAD-TIME STRUCTURAL LOOP ELIGIBILITY — default-on, NO-ADAPT and correctness-gated

Scalar-loop eager admission is default-on; `RAVA_NO_LOOP_EAGER=1` disables that scalar
strategy for experiments. Before a `LoadedClass` is published, the loader derives immutable
structural flags from the method's bytecode, CFG, descriptor, access flags and effects. Runtime
admission reads those flags only: names, `Code` identity, invocation counts and previous queue
outcomes do not participate.

Under `next-gen-gc`, parallel-loop classification is a separate structural bit checked before
the scalar opt-out. `RAVA_NO_LOOP_EAGER=1` therefore is not a complete disable for every eager
strategy; `RAVA_NO_JIT_PARALLEL=1` disables parallel eager compilation. Keep the strategies
separate in both measurements and correctness claims.

Correctness evidence on a separate release binary, independently for flag SET and UNSET:

- all five gate programs produce the required hashes;
- all 23 probes match `java -Xint`;
- `rava-classfile` tests pass 16/16;
- pure policy tests cover disabled/unarmed, armed candidate and armed noncandidate states.

⛔**Still unpriced.** A second null batch was much tighter on its valid runs (A-arm spread 0.6% for
`WINDOW_MS` and 0.7% for CPU/cycles; 3.8% for `RELOAD2_MS`), but one of eight identical runs exited
with `ReportedException`. A null batch with a semantic failure is not a valid instrument. The paired
summary then failed again; the actual PowerShell bug was case-insensitive aliasing between local
`$pairs` and the typed `[int]$Pairs` parameter, not the element cast fixed earlier. The vector now
has a distinct name, and future discarded runs preserve their full stdout/stderr instead of being
overwritten by the next arm.

## ✅ MOVING-GC EPOCH GUARD FIXES THE INTERMITTENT VANILLA TAG LOSS

The failed null run was a product correctness defect, not benchmark noise. With background JIT and
arena maps enabled, a warm reload intermittently lost one arbitrary base biome tag
(`is_badlands`, `is_hill`, or `is_jungle`), then failed its dependent tags and exited with
`ReportedException`. The same binary passed with background JIT off or arena maps off.

Two plausible root fixes were rejected by stress rather than accepted from static reasoning:

- rooting arena-map operation temporaries still failed on attempt 4;
- keeping popped direct-call arguments in precise stack maps still failed on attempt 5.

The causal hole was the outer arena structural-key memo. `structural_key_scoped` retained a raw
`ArenaPtr` across guest-capable key rendering. A nested `toString` could allocate and run moving GC;
the function then passed the stale pre-move pointer to `arena_funnel_put`, reading identity/class
metadata from reclaimed or reused space and potentially publishing another object's rendered key.
The inner value-key memo already had the correct rule: capture `gc_epoch` before rendering and skip
the cache fill if it changes. The outer funnel now follows the same rule. A skipped memo fill changes
only performance; the exact rendered key is still returned.

Evidence on a release binary containing only this seven-line guard:

- **10/10** sequential `ReloadBench` runs returned `RECIPES=1470 ADVANCEMENTS=1584 TAGS=625` and
  `HASH=c956377e19071d7a`, with no missing/unbound tag, registry error, crash, or `ReportedException`;
- the previous default path failed on attempt 4 and the strongest rejected root fix on attempt 5;
- all bit-exact gates passed (`chunkfill 8790f04ace726ecf`, ring variants
  `37539c6c375d0c3`, `nbtsave ae44a34b42766bd4`);
- all 23 semantic probes matched `java -Xint`.

The decision uses only moving-GC structural state. It is independent of class/method/workload names,
inputs, outputs, hashes, timings, and prior failures, so it satisfies NO-ADAPT. Timing was not inferred
from the stress series; its purpose was correctness and restoring a valid measurement instrument.

## ⛔ BACKGROUND QUEUE 32 IS NOT A WIN ON THE RELOAD INSTRUMENT

The effective default queue is 16 (`max(depth 8, four workers * 4)`), so the next meaningful depth
was 32. Five interleaved warm-reload pairs all returned the exact expected hash. `RELOAD_MS` paired
deltas were `-310, +174, +172, +687, -499 ms`; the median was `+172 ms`, two pairs favoured 32 and
three favoured 16, and the arms overlapped. `WINDOW_MS`, `RELOAD2_MS`, CPU and cycles also overlapped.
There is no positive verdict, so 16 remains the default. The early-reserve price was postponed when
the stale-executable guard correctly refused to compare a binary older than the active source tree.

## ✅ RELOCATION-SAFE ARENA-MAP PROBES FIX THE SECOND TAG-LOSS PATH

The first epoch guard restored the default schedule, but `RAVA_NO_LOOP_EAGER=1` exposed the same
random missing-tag failure in three of five runs. The discarded runs lost different base tags
(`is_hill`, `is_ocean`/`is_deep_ocean`, `is_badlands`), which is the signature of a dropped mapping,
not a deterministic tag rule.

The arena map's core `get`, `contains`, `insert` and `remove` operations read raw `map` and `table`
pointers before deriving a structural key. Key derivation may execute guest `toString`, allocate and
move the arena. Stored-key comparison inside the probe loop may do the same. The operation then
continued probing or writing the evacuated table; an insert could therefore publish a tag mapping
only into dead space. Rehash had the twin defect: after attaching the new table, the old table was
reachable only through a raw Rust pointer while reinserted keys could collect.

The fixed contract roots the receiver, query key and inserted value for the whole operation, returns
the forwarded receiver through `&mut ArenaPtr`, and re-derives `map -> table` on every probe iteration
and immediately after a guest-capable stored-key comparison. Rehash roots the detached old table.
Compound dispatch roots every argument and source snapshot so a second `contains/get/put` never
reuses a pre-GC Rust local. Composite key rendering likewise roots arena-map snapshots, preserves
the original key/value observation order, and reads the current epoch at every address-memo query.

Isolation and verification:

- arena maps ON reproduced `ReportedException` in 1/5 runs; `RAVA_NO_ARENA_MAPS=1` passed 5/5;
- background JIT ON and OFF both passed 5/5 in the separate schedule probe, so it was not necessary;
- the first relocation-safe build passed the formerly failing `RAVA_NO_LOOP_EAGER=1` configuration
  **10/10**, every run `HASH=c956377e19071d7a`, with no discard;
- the final build passed all bit-exact PGO gates and all 23 differential probes.

This is a representation/lifetime fix. No class name, workload identity, input, output, expected hash,
timing or prior failure participates in routing.

## ✅ FAIL-CLOSED STRUCTURAL EAGER COMPILATION REPLACES MAIN/CLINIT ROUTING

Loop eager no longer uses `ENTRY_MAIN_CODE`, method names, `<clinit>` exceptions, invocation count or
prior queue outcomes. Each class-load performs one allocation-free bytecode prefilter. Straight-line,
malformed and compiler-unsupported bodies return immediately. A scalar candidate is capped at 512
bytes and 64 blocks, has a valid descriptor/frame, no exception or synchronized/native/abstract flag,
and every retreating edge is a natural back edge whose header dominates its source. The natural loop
contains only primitive/local/arithmetic/stack/control operations; references, arrays, fields, calls,
allocation, monitors and throws inside the loop fail closed. Calls outside the proven loop are allowed.
The existing data-parallel map/reduction analyzers supply a separate immutable structural bit.

Structurally eager methods compile synchronously. Sending them to the background queue returned to the
interpreter before code generation, so without OSR a one-shot loop received no benefit and queue `Full`
could repeat planning on every entry. The synchronous route executes the compiled body on the current
invocation, skips speculative callee precompile and profile-based recompile bookkeeping, and cannot use
queue state as an eligibility signal. Ordinary non-eager JIT scheduling is unchanged.

The final release passed every PGO hash and all 23 semantic probes. Five interleaved reload pairs against
`RAVA_NO_LOOP_EAGER=1` produced 10/10 exact hashes. Opt-out was slower by median `+2.43% RELOAD_MS` and
`+0.81% WINDOW_MS`, with 4/5 paired reload signs favouring the structural default, but the arms overlap;
the honest performance verdict is **no resolvable loss**, not a claimed speedup. `RELOAD2_MS`, CPU and
cycles also overlap. The architectural verdict is independent: eager eligibility is now derived only
from permitted bytecode, CFG, descriptor, access and effect facts established at class load.

**Fresh opt-out failure (2026-09-25; not a performance result).** On current release
`5c6d9b484121263c48201336334d7490ac162aa5f4bbf68582d086496ee5d36e`, the default
`ReloadBench --reloads 2` completed with `HASH=c956377e19071d7a`. With
`RAVA_NO_LOOP_EAGER=1`, the same two-reload probe failed twice with
`ExecutionException: ArrayIndexOutOfBoundsException: Index 0 out of bounds for length 0`.
A third run with `RAVA_TRACE_COMPILE=1` passed, so this failure is timing-sensitive rather than a
stable red path. The one-reload opt-out completed with the expected hash; two reloads also completed when
`RAVA_NO_ARENA_MAPS=1`. Disabling all collection fast paths or parallel JIT compilation did
not remove the failure. This implicates an interaction with the arena-map/repeated-reload
path but does not isolate its cause; the GIL-free-lane-off diagnostic exceeded its 300s
bound and was terminated. The scalar opt-out is therefore not a valid A/B control on this
binary, and no opt-out wall comparison is claimed. The clear-arm kill switch still failed,
so the new clear arm is not the sole cause. Also, `RAVA_NO_LOOP_EAGER` disables scalar
admission only: `EAGER_PARALLEL` structural classification is separate.

A fail-only `RAVA_TRACE_RELOAD_FAILURE=1` path in `ReloadBench` now dumps the
original cause after `future.get()` throws, leaving pre-failure scheduling
unchanged. Two captures remain inconsistent: one named `LootCommand.register`;
another showed `Commands.<unknown>@pc=0` and `ReloadBench.main@pc=583` (the warm
`ReloadableServerResources.loadResources` call). The worker frame is unresolved,
so neither capture identifies the bad array access or proves an arena-map
operation is responsible. `RAVA_STACK_PC=1` annotates captured methods for
forensics only; it does not repair or price the opt-out.

## ⛔ EARLY BACKGROUND-JIT PERMIT RESERVATION SLOWS THE WARM RELOAD

`RAVA_BG_JIT_EARLY_RESERVE=1` reserves a compile permit before planning instead of after it. Five
interleaved reload pairs completed with the exact expected hash in all ten arms and no discard.
Primary `RELOAD_MS` and `WINDOW_MS` overlap, so neither supports a magnitude claim. The steady-state
`RELOAD2_MS` result is disjoint in the wrong direction: every flagged arm was above every default
arm (`5760/5315/5310/5342/5280 ms` versus `5043/5178/5193/5223/5166 ms`), with a median paired loss
of `+119 ms` (`+2.65%` by the harness summary). CPU and cycle arms overlap.

Early reservation is therefore rejected and remains off. The only non-noisy verdict is a loss; no
queue-pressure or compiler-overlap narrative overrides the measured clock.

## ⛔ PRIMITIVE-ARRAY ESCAPE WIDENING HAS ZERO RELOAD COVERAGE

The exact-CID primitive-array tracer was widened in two structural steps: descriptor-aware calls
that do not consume the candidate, then exception-table CFG edges that preserve pre-instruction
locals and replace the handler stack with one clean `Throwable`. Raw candidates still could not be
passed, returned, stored into fields/statics/reference arrays, or used as a receiver. A deliberately
broader prototype had admitted 366/511 MAIN sites, but the PGO gates crashed in
`ArrayVoxelShape.<init>` when a raw `double[]` crossed into native-backed fastutil code expecting a
handle. That prototype was rejected; no name exception or workload whitelist was added.

The safe non-consuming proof admitted only 5/511 MAIN allocation sites; exception-aware CFG raised
that to 6/511. The sixth was an ore-worldgen method absent from existing boot/reload runtime logs.
The focused `PrimArraySem` differential probe matched `java -Xint`, but an opt-in product census on
the five-site release printed no `o4298` allocation tick in the SET arm: **zero admitted raw
allocations actually executed during ReloadBench**. The timing pair was therefore not a price and
no A/B series followed. All widening and boundary fallback code was removed; the previously
committed default-off confined-array foundation remains available for a future representation
contract that has real dynamic coverage.

## ⛔ BACKGROUND QUEUE 64 IS ALSO NOT A RELOAD WIN

With four compiler workers, queue values 1 through 16 all produce the same effective capacity 16.
Depth 32 was already neutral, so 64 was the last stale boot-measured capacity worth checking. A
fresh four-pair null test was noisy (`RELOAD2_MS` A-arm spread 4.9%, all arms overlapping). Four
interleaved 16-vs-64 pairs then returned every required hash but no disjoint improvement:

- `RELOAD_MS` median candidate delta `+0.81%`, only 1/4 pairs favouring 64;
- `RELOAD2_MS` median `-0.40%`, with overlapping arms and 9.2% A-arm spread;
- CPU median `+0.80%` and cycles `+0.68%`, both overlapping.

The last control arm was an 8.852-second outlier, so these magnitudes are not prices. The valid
verdict is only that capacity 64 supplies no resolvable win; capacity 16 remains the default.

## ⛔ TLS CLUSTERING DOES NOT MOVE THE WARM RELOAD CLOCK

`RAVA_TLS_CLUSTER=1` replaces the remaining `tlcache` and `owner_bits` `LocalKey::with` accessors
with one inline TLS cursor load. Older profiles and a four-flag bundle could not isolate its price;
the current reachable census was still roughly 410 million accesses, so it was re-priced alone.

Four interleaved reload pairs returned all exact hashes. The control arms were tight for
`RELOAD_MS`, `WINDOW_MS`, CPU and cycles (0.7-1.4% spread), but every metric overlapped:

- `RELOAD_MS` median candidate delta `+0.81%`, only 1/4 pairs favouring TLS clustering;
- `WINDOW_MS` median `+1.71%`;
- `RELOAD2_MS` median `-0.87%`, with overlapping arms and 7.3% control spread;
- CPU median `-0.30%`, cycles `+0.05%`.

Frequency was not price: removing the accessor shape does not produce a resolvable workload win.
The flag remains off.

## ⛔ THE REMAINING CHA OVER-CAP POOL IS 34, NOT 6,071

The often-quoted 6,071 `CHA_POLY` refusals predated the one-body widening that added 5,673 answers.
An opt-in structural split (`RAVA_CHA_STATS`) now separates cap overflow, resolution failure and
multiple resolved bodies without changing admission. Its only expensive arm asks the existing
epoch-stable dispatch-closure enumerator whether an over-cap `invokevirtual` still resolves to one
exact bytecode body; the answer is counted but never consumed by production routing.

One full hashed ReloadBench census observed 2,875 CHA queries: 1,014 exceeded the active four-type
cap, 412 resolved to several bodies, and 47 had no bytecode resolution. Only **34** over-cap queries
resolved to one shared body. The diagnostic run retained the exact reload hash, although its
full-hierarchy scans intentionally made it unsuitable for timing.

That 34-query ceiling does not justify implementing a new loader publication seqlock, baked-code
epoch guards and recompile semantics. The current deopt path is not such an invalidation mechanism:
old direct/PIC/freeze pointers remain callable, its trap checks only the primary widened CID, and
recompilation can recreate the same CHA assumption. This front is closed until a dynamic hit census
shows a materially larger structural population.

## ⛔ FIRST-ESCAPE PRIMITIVE-ARRAY MATERIALIZATION IS CORRECT BUT ECONOMICALLY WRONG

A sound alternative to universal raw-array native support was implemented and gated: one exact
primitive `newarray` could remain arena-raw until its first invoke/return/field/static/outer-array
boundary. Immediately before that opcode, emitted code allocated the ordinary tagged array through
the existing callback, copied the bit-exact payload, and replaced every local, operand-stack and
shadow-frame alias with the same handle. No name or workload identity participated. This avoided
the fastutil representation crash of the universal-escape prototype.

The contract passed every PGO hash and all 24 differential probes. It also opened real product
coverage: 80/511 MAIN sites were structurally admitted and an armed ReloadBench census observed
657,297 raw allocations, versus zero under the confined proof.

The profitability census rejected it before a timing series. Every admitted array eventually
materialised, so no legacy allocation disappeared. Of 80 sites, 55 performed zero candidate array
operations before the boundary and six performed only one. Across the whole admitted population
there were 99 matching stores, zero loads and two arraylength operations; only one method placed a
candidate access on a CFG cycle. Thus the general case adds an arena allocation and payload copy to
the historical allocation without removing compensating work. The one diagnostic timing pair was
not treated as a price. The tracer was removed despite its correctness; the next representation
change must eliminate or directly back the legacy array storage rather than merely defer it.

## ⛔ THE STRUCTURAL PRIMITIVE-ARRAY CONFINEMENT HAS ZERO BOOT-PREFIX COVERAGE

`RAVA_ARENA_PRIM_ARRAYS=1` is the current-HEAD confined primitive-array tracer bullet: the producer
is admitted only from bytecode, CFG, descriptor and effect facts, and the dual consumers
(`primitive_array_consumers.rs`) route on the resulting exact class ids with the historical
tagged-array emitters as their complete fallback. No name, package, workload identity or prior
outcome participates, so admission satisfies NO-ADAPT by construction. The flag is default-OFF.

Censused on the accepted product binary without a rebuild — `target/release/rava`, SHA256
`c2e8a417273bc3f71a1515adb8ff3eeccacf72d0838fdf26b0077e63cc39931e`, the old head7 PGO build — with
the world restored from `world_pristine` before each arm, port 25599, snapshots/checkpoints unset,
and `RAVA_CALLBACK_STATS=1` armed in BOTH arms so the counter could not be silently unarmed. On
Linux the `[CB]` table is unreachable (the pc_profile sampler is Windows-only), so the census read
the `dump_jit_stats` → `[ARENA-PROBE]` callback line at the 11 s JIT-stats tick; each arm ran
exactly 12 s and was killed by PID. The ON arm added only `RAVA_ARENA_PRIM_ARRAYS=1
RAVA_ARENA_PRIM_ARRAY_STATS=1`.

| 12 s fixed window | OFF | ON |
|---|---|---|
| `o4298` confined raw births (ordinal 22) | — | **zero** (absent from the table, which skips zero counters; 0 matches in the full log) |
| `newarray` residual tagged births (ordinal 5) | 3,650,817 | 3,686,945 |
| bodies / code bytes compiled | 4,393 / 21,176,862 | 4,439 / 21,230,972 |
| CPU-s burned | ~19.2 | ~19.2 |

The zero is **coverage, not an unarmed instrument**: the ON arm compiled 46 more bodies carrying
54,110 more code bytes — the confinement plan being analysed and the inline producer emitting at
admitted sites — and the flag's gate was live. The `newarray` delta (+36,128, ON not lower) is
**phase noise**: the `invoke` column moved by 2.5 M across the same arms (24,328,695 vs 21,795,386),
so neither the confined nor the residual tagged population shrank.

Conservative ceiling: **0.0 s** (zero confined births × any per-callback cost), far below the ~1 s
admission bar, so no A/B was warranted. No semantic, GC-stress or ABBA gates were run — the frozen
protocol stops at the census for a coverage reject, and this is one, not a correctness verdict.
It is also the **second independent zero-coverage reading**: the five-site release census two
sections above already printed no `o4298` tick in its SET arm, and the current-HEAD structural
confinement has the same empty dynamic population, now measured on the accepted PGO product.

⇒ **REJECT: `RAVA_ARENA_PRIM_ARRAYS` stays default-OFF and unpromoted.** No product source was
changed and none is warranted. The 12 s window bounds the claim to the bootstrap serial prefix —
the ring engages near t+24 s, so no fork/prim-array interaction is observable in-window, and a
ring-phase confined-birth count would need a longer instrumented window before this direction is
retried.

## ✅⭐⭐⭐ THE ADMISSION GATES READ ONE FACTS WORD — −1.5% wall, and the PGO lane is part of the profile's identity

Every native-representation admission check — the JIT compile refusal, its AOT twin, the direct-call
resolver's `invokespecial` arm and `wide_tier_admits` — re-derived the same five name predicates by
scanning provider tables on each ask. The class-facts word memoises them: five bits, computed once
per distinct owner name and held in its OWN thread-local memo — deliberately NOT a third word of the
plan/family entry, whose two-word shape and address-keyed direct slot are the hot path this module
exists to keep (`fact_tests` pins `size_of::<Entry>() == 8`; the plan/family accessors are untouched).
`compute_facts` is the only writer, so the memo arm and the `RAVA_NO_CLASS_FACTS` bypass cannot drift,
and no name-shape, `has_bytecode`, history or prior outcome participates — a bit is a table lookup
and nothing else:

| bit | the ONE provider table it reads |
|---|---|
| `FACT_ENGINE_SYNTHETIC` | `cache::{LOADING_CACHE_CLASS, CACHE_CLASS}` |
| `FACT_NATIVE_REPR` | `fastutil::INT_SETS` |
| `FACT_NATIVE_MAP` | `map::{MAP_CLASSES, FASTUTIL_OBJMAPS}` + the `next-gen-gc`-gated `FASTUTIL_OBJ2OBJ` |
| `FACT_ENTRY_TYPE` | `map::ENTRY_CLASSES` |
| `FACT_IO_CLASS_EXACT` | the hoisted `opcodes::IO_CLASSES_EXACT` |

Three provider tables were consolidated so a bit cannot drift from its predicate: `is_native_io_class`
answers from one hoisted `IO_CLASSES_EXACT`, `is_entry_type` delegates to `map::ENTRY_CLASSES`, and
`INT_SETS` is the one table `is_int_set` reads. The hot ladders and shape arms stay LITERALS at their
callers — deferred, not folded: the `BufferedInputStream` SUFFIX arm, the guava
`com/google/common/collect/…Immutable` prefix arm and the `native_served_prefix` ladder are shape
matches or ladders, not table members, and `fact_tests::the_io_suffix_is_not_a_fact` pins
`FastBufferedInputStream` to zero. `owner_sketch_tests` proves every served name passes
`is_native_map`'s `map_owner_maybe` filter, so `FACT_NATIVE_MAP ≡ is_native_map` exactly.

Focused evidence on the patched tree, `cargo test -p rava-interp --lib --features next-gen-gc`:
**20 owner_bits tests passed, 0 failed, 573 filtered** (10 module tests + 5 `family_tests` + the 5 new
`fact_tests`), and **4 map/entry tests passed, 589 filtered** (3 `owner_sketch_tests` + the arena
`entry_types_are_native_backed_handle_alloc` probe).

### ⚠ The 26.92-second scare was the LANE, not the source — head7 PGO is explicit-target-only

The candidate was built in the effective lane, verbatim: `RUSTFLAGS='-C target-cpu=native -C
profile-use=/root/workspace/rava/target/pgo-boot-head7.profdata' cargo build --profile release-fat -p
rava-cli --features next-gen-gc --target x86_64-unknown-linux-gnu`. rustc printed **21 PGO messages**
off `rava_interp.998d255fb90c1c88-cgu.0`: **20 "function control flow change detected (hash mismatch)
… count discarded" warnings naming the changed functions** (`Vm::jit_run`, `Vm::jit_direct_resolve`,
`is_native_io_class`, `dispatch_owner_cascade`, the `pic_native` leaf emitters and the rest of the
touched set) plus one partially-ignored `BTreeMap`-iterator profile. **The discards are themselves the
evidence the profdata was consumed.**

An earlier implicit `--release` rebuild read **26.92 s wall / 45.2 CPU-s** — the no-PGO class (row
608: 27.3/46.0), not a source result. The profdata was ATTACHED (dep-info lists it) but the unit's
`compile_kind 0` matched **zero** records, and LLVM warns about nothing. Profile keys carry
lane-dependent crate metadata: head7 was trained in the explicit lane
(`rava_interp.998d255fb90c1c88`), so only an explicit `--target x86_64-unknown-linux-gnu` consumer
matches; `--target` presence, not the profile name, is the discriminator, and the requirement is
symmetric — an implicit-lane-trained profile feeds no explicit-lane build either, so any future
retrain must train AND consume in one lane. **Every 26.9–27.1 s class-facts reading, including the
first neutral kill-switch A/B, is an invalid lane artifact, not a measurement of this patch.**

### The gate, then four interleaved pairs on the effective lane

The pre-registered gate is wall-only — ≥25 s reject-and-stop, (22,25) gray-stop, ≤22 s proceed — and
the candidate's gate boot read **19.84 s wall / 34.94 CPU-s**: proceed. Four interleaved pairs against
the accepted baseline `c2e8a417…` (the old head7 PGO product), alternating order, world restored from
`world_pristine` per arm, port 25599, snapshots/checkpoints unset:

| launch→`Done` wall, s | r1 | r2 | r3 | r4 | median |
|---|---|---|---|---|---|
| candidate `dbf9b468…` | 19.65 | 19.64 | 20.05 | 19.85 | **19.75** |
| baseline `c2e8a417…` | 20.05 | 19.85 | 20.05 | 20.06 | 20.05 |

Wall median **−1.50%**, the candidate ≤ baseline in **4/4 pairs** (three wins, one tie), none against;
CPU median **34.97 vs 35.10 s (−0.37%)**. **8/8 arms reached `Done`**, no panic, no crash, no semantic
divergence. Not disjoint — the magnitudes are indicative; the sign and the 4/4 pair direction carry
the verdict.

⇒ **ACCEPT: `owner_bits::facts` is DEFAULT-ON** (`RAVA_NO_CLASS_FACTS=1` restores the per-ask table
scans bit for bit, and `fact_tests::the_memo_and_the_tables_agree_fact_by_fact` pins both arms to the
one word). The candidate is installed as `target/release/rava`, SHA256
`dbf9b468ce8df04646c25ee482d7f7b38f25e307654dfd460c2893698ac7254b` (16,076,744 B).

## ✅⭐⭐⭐ MATCHED-LANE FRESH PGO WINS −4.3% WALL — and row 608's "fresh training is effectively no-PGO" was the LANE, not the profile

Row 608 rejected fresh current-source boot training with "a clean profile reproduces the same
loss, i.e. fresh training is effectively no-PGO … REJECT both fresh profiles, KEEP
`target/pgo-boot-head7.profdata`." **That conclusion is superseded: it was a train/use lane
mismatch, not evidence about fresh current-source PGO.** Row 608's trainer
(`pgo_train_current.sh`) ran `target/release/rava` — an implicit-lane build, no `--target` — so
`target/pgo-current-tight.profdata` carries the implicit CGU key `rava_interp.8dad5567e536ca60`
(read from the profdata itself), while the profile-use consumer builds in the explicit lane, whose
units key `rava_interp.998d255fb90c1c88`. The profdata was attached but matched **zero** records:
the fresh arms were measuring an un-profiled binary, which is exactly why they sat in the no-PGO
class (27.2 s vs 27.3). Row 608's own phase split — both phases lost, no per-function PGO evidence —
is the lane artifact's signature. The historical readings stand as measured; their
*interpretation* does not. Retrained in the matched lane, the fresh profile **wins**, and the
product below is that binary.

**The same-lane invariant** (the correction row 608's procedure needed): a PGO profile's identity
includes its build lane, so training and use MUST be identical in five things — the explicit
`--target` triple (present in both or neither; presence is the discriminator), the profile
(`-C profile-generate=` data dir → merged `.profdata` → `-C profile-use=`), feature propagation
(`cargo` default features and per-unit feature sets, here `[default, next-gen-gc, shrink-header]`
at use; `pgo-dump` is training-only), source, and `-C target-cpu=native`. A profile trained in
one lane feeds zero counts to a build in the other — silently, with no rustc warning — so a
cross-lane "fresh PGO" arm is an un-profiled build, not a measurement.

The corrected experiment, all commands verbatim, all from this checkout at HEAD `930f7132`:

```sh
# 1. instrumented (profile-generate) build — EXPLICIT lane, like the accepted product
RUSTFLAGS="-C target-cpu=native -C profile-generate=/root/workspace/rava/target/pgo-fresh-explicit-data" \
  cargo build --target x86_64-unknown-linux-gnu --profile release-fat -p rava-cli \
  --features next-gen-gc,pgo-dump
# → target/x86_64-unknown-linux-gnu/release-fat/rava, SHA256 d03884e2967ac33cc066a041f339afd249fc6c7fa3b081a6eb47007ead09553f, 27,417,848 B

# 2. training boot — one real Minecraft boot, tight dump, no idle contamination
#    (RAVA_NO_FAST_EXIT=1 per docs/pgo-build.md trap 1: fast exit leaves a 0-byte profraw)
env -u RAVA_RESTORE -u RAVA_CHECKPOINT_FILE -u RAVA_CHECKPOINT_OPS -u RAVA_STACKFUL \
    RAVA_JIT=1 RAVA_JIT_TIER0=1 RAVA_HARD_MB=3584 RAVA_GSON_NATIVE=1 RAVA_M3_BITSTORAGE=1 \
    RAVA_NO_FAST_EXIT=1 RAVA_PGO_DUMP_AFTER_MS=42000 \
    LLVM_PROFILE_FILE=/root/workspace/rava/target/pgo-fresh-explicit-data/boot-%p.profraw \
    target/x86_64-unknown-linux-gnu/release-fat/rava -cp "$(cat bench/mc-cp.txt)" \
    net.minecraft.server.Main --nogui
# launch→Done 38.5 s (instrumented), [PGO-DUMP] wrote counters after 42000 ms (rc=0) → Done+~3.5 s, 0 panics
# → boot-520828.profraw, 9,045,840 B, SHA256 417cf11382a6b62b244b6f627a78f1044ca7bde996596ccd7777f0ae77a6762b

# 3. merge — matching-toolchain llvm-profdata (LLVM 22.1.8-rust-1.98.1-stable)
/root/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-profdata \
  merge -output=target/pgo-fresh-explicit-tight.profdata \
  target/pgo-fresh-explicit-data/boot-520828.profraw
# → SHA256 1b43775836ac43c6c34efd02dc0c9a4834612608fd4e008f2a22825379f7a19a, 7,944,640 B, 19,034 functions, 53.1 G total counts

# 4. profile-use candidate build — same lane as training AND as the accepted product
RUSTFLAGS="-C target-cpu=native -C profile-use=/root/workspace/rava/target/pgo-fresh-explicit-tight.profdata" \
  cargo build --target x86_64-unknown-linux-gnu --profile release-fat -p rava-cli \
  --features next-gen-gc
# → candidate SHA256 f75c17ca40d8d3af039f3f0a8b1a7936d86fd53a3ea99391730f88229ed5d8fb, 16,581,256 B
```

The dump fires at 42 s — Done+~3.5 s after the ~38.5 s instrumented `Done` — and the run is killed
there: no idle server time enters the profile, because a profile trained through minutes of idle
tick loops would carry the no-PGO boot's stale block ordering into the sections the boot actually
pays for. Row 608's tight arm already proved contamination was not the loss's cause; the lane was.

Consumption evidence, this time in-lane: the use build printed **one** PGO message off
`rava_interp.998d255fb90c1c88-cgu.0` — the same partially-ignored `BTreeMap IntoIter::dying_next`
profile the head7 consume also prints — and **zero** "function control flow change detected (hash
mismatch) … count discarded" warnings (head7's consume printed 20 against changed source; this
profile was trained on the same source, so nothing hashes out). The fresh profile's CGU key is
byte-identical to the consumer's: **the match is the lane match**.

The pre-registered gate — wall-only, ≥25 s reject-and-stop, (22,25) gray-stop, ≤22 s proceed — read
**18.63 s wall / 33.5 CPU-s** on the candidate: proceed. Then four interleaved pairs (ABBA, one
discarded warm-up boot per binary, alternating first arm, pristine world per arm, port 25599,
snapshots/checkpoints unset) against the accepted `dbf9b468…` product (same source, same features,
same `--target`, same `target-cpu`; the ONLY difference is `-C profile-use`):

| launch→`Done` wall / CPU-s | r1 | r2 | r3 | r4 | median |
|---|---|---|---|---|---|
| OLD `dbf9b468…` (head7 profile) | 19.8 / 34.9 | 19.9 / 34.8 | 20.1 / 35.4 | 19.8 / 35.0 | **19.85 / 34.95** |
| NEW `f75c17ca…` (fresh profile) | 18.8 / 34.0 | 20.1 / 35.5 | 19.0 / 34.2 | 19.0 / 34.3 | **19.00 / 34.25** |

Wall median **−4.28%**, CPU median **−2.00%**, the candidate faster in **3/4 pairs** (r2 loses by
0.2 s), none of the magnitudes disjoint — indicative, not conclusive; the 3/4 direction and the
gate carry the call. Semantic gates: **8/8 arms reached `Done`**, no panic, no crash, no semantic
divergence, and the use build's features resolve to the accepted product's
`[default, next-gen-gc, shrink-header]` — the training-only `pgo-dump` is stripped, so the shipped
binary does not link `__llvm_profile_write_file`.

⇒ **PROMOTE: `target/pgo-fresh-explicit-tight.profdata` supersedes `target/pgo-boot-head7.profdata`
as the product profile, trained and consumed in one explicit lane.** The candidate is installed as
`target/release/rava`, SHA256 `f75c17ca40d8d3af039f3f0a8b1a7936d86fd53a3ea99391730f88229ed5d8fb`
(16,581,256 B); the superseded product is preserved at `/tmp/rava-classfacts-head7`
(`dbf9b468ce8df04646c25ee482d7f7b38f25e307654dfd460c2893698ac7254b`, 16,076,744 B). Row 608's
preserved conclusion is now: fresh current-source PGO wins when the lane matches — the lane, not
the profile, is part of the profile's identity.

## ✅⭐⭐⭐ THE GUAVA ADMISSION IS AN EXACT PROVIDER FACT — both package+contains name-shape arms deleted, and the closure residue compiles

The class-facts word ended with a caveat it had already earned: the guava
`com/google/common/collect/…` + contains("Immutable") arm was carried into the word's own
adoption section as "a shape match, not a table member" that "stays a literal." But that shape
was the LAST name-shape admission in either JIT gate — it lived in two places, and both decided
who may compile vs who must stay interpreted so a native answers. The shape is a forbidden
approximation now that facts exist: it admitted `ImmutableTable`, every `Regular*`/`Singleton*`
concrete impl and every Builder — classes no provider table names — on a substring alone.

**The problem, located exactly.** `jit_native_served` (opcodes/native_gate.rs) and
`wide_tier_admits` (vm/jit_run.rs) each carried the same literal. The two sites answer different
questions and were cured differently:

- `jit_native_served` keeps guava out of the compiled tier because natives serve its members'
  `values()`/`keySet()`/`iterator()` — a bytecode call would read fields rava never populates
  (the historic `propertiesByName.values().iterator()` NPE). This question is about exact
  semantics, so it reads the new sixth fact: `FACT_GUAVA_IMMUTABLE`, set by
  `crate::map::GUAVA_IMMUTABLE_PROVIDERS` — the union of the four provider tables that name
  such classes exactly, hoisted from the arms that already had the literals:
  `GUAVA_IMMUTABLE_COPYOF_OWNERS` (map/dispatch's `copyOf` arm, 3 rows),
  `GUAVA_IMMUTABLE_ENTRY_CLASSES` (fu_fastpath's arena-entry arm, 4 rows), `MAP_CLASSES`'
  Immutable row (`ImmutableBiMap`) and `stream::OWNER_LITERALS`' Immutable rows (`ImmutableMap`,
  `ImmutableBiMap`) — **7 members, deduplicated**, because `ImmutableMap`/`ImmutableBiMap` sit
  in two tables each. Three drift tests pin the union: every shaped provider row is a member,
  every member is shaped and sourced, no member twice. A name added to a provider table and not
  the union fails a test, not a boot.
- `wide_tier_admits` had no guava question of its own to answer: its `NATIVE_SUPERTYPES` type
  closure (`Collection`/`Iterable`/`Map`/`Map$Entry`/`Iterator`/`BaseStream`) refuses every
  class a native can answer — provider-union members by their JDK supertype (`ImmutableMap` →
  `Map`, `ImmutableEntry` → `Map$Entry`) and collection impls likewise (`ImmutableList` →
  `Collection`). The package+contains arm there refused only the closure-clean residue
  (`ImmutableTable`, `ImmutableRangeMap` families, Builders, SerializedForms) on name alone —
  and a closure-clean class implements no JDK type a native holds a receiver contract for.
  **Deleted; the arm reads no name and no fact.**

**The check: a served census, taken in the deletion's own instrument before anything was
deleted.** A one-boot census (`RAVA_IMMUTABLE_DIFF=1 RAVA_SERVED_CENSUS=1`, 45 s timer,
pristine world, exit 0, `Done (0.278s)`) enumerated every class the two sites saw with the
shape's answer (`shape=true`) against the fact's answer (`bit=false`):

```
jit_native_served: 10 divergent classes, 0/10 ever answered by a native
wide_tier_admits: 33 divergent classes, 5/33 served — intersection NON-EMPTY, literal stays
```

That non-empty intersection — `ImmutableMapEntrySet$RegularEntrySet`, `RegularImmutableList`,
`RegularImmutableSet`, `SingletonImmutableList`, `SingletonImmutableSet`, all served through
wide_tier's closure and refused by it — is why the cutover was SEQUENTIAL, not simultaneous:
wide_tier went first (delete the shape arm there, keep the literal in `jit_native_served`,
re-census: **0/10 served, the remaining literal admits nothing served**), then the fact replaced
the last literal. **The census was proof only: every `RAVA_IMMUTABLE_DIFF`/served-history hook,
the `immutable_diff` module, the census timer in `lib.rs` and the `was_served` diagnostic were
removed with it — no served history routes the product, and the shape now survives only where a
test may query it.**

**Tests and gates.** `owner_bits::fact_tests` 7 passed / 0 failed (now including
`every_guava_union_member_lands_the_bit` — `ImmutableBiMap` carries both `FACT_NATIVE_MAP` and
`FACT_GUAVA_IMMUTABLE` — and `the_guava_shape_is_not_a_fact`, pinning `ImmutableList`,
`ImmutableSet`, `ImmutableMultiset`, `RegularImmutableMap`, `SingletonImmutableList`,
`RegularImmutableSortedMap` to ZERO facts); `map::classes::guava_immutable_tests` 3 passed /
0 failed. Hash gates on the final product: `NbtSave_HASH=ae44a34b42766bd4` (BYTES=10070921)
and `DataPack_HASH=14e8ee5a` (count=6351 fail=0 bytes=2876335), both byte-identical to the
accepted baseline's. Semantic probes vs `java -Xint` (stdout-only, `// DIVERGES:` skipped):
14/15 match — `CompSem` alone shows its 14-line pre-existing JSON **object-key-order**
divergence (Codec-encode field order), identical to the accepted product's output to the set
hash; parsed canonically the 48 semantic lines are identical.

⚠**The ChunkFill/Ring gates could not certify this product — and cannot certify the baseline
either, which is the point.** `bench/ChunkFill 1 64` and the ring family fail on the ACCEPTED
product `f75c17ca` on this box with `uncaught java/lang/ArrayIndexOutOfBoundsException: Index -1
out of bounds for length 10` — ChunkFill dies in `Bootstrap.bootStrap` before any hash, RingScan
reproduced 3× (the `java -Xint` reference prints the documented `RINGS_MS=14104 N=8
HASH=37539c6c375d0c3`), and one 13:10:05 baseline ChunkFill pass could not be reproduced in any
env/class-version/cwd combination tried. **This is a pre-existing gate limitation of the baseline
on this box, recorded as such — NOT a pass, NOT attributed to this change, and NOT evidence of
candidate correctness.** The product's own bar is the boot, the two hash gates above, and the
probe suite.

**The PGO question, answered rather than assumed.** Deleting admission arms changes hot control
flow, so the structural source was retrained in the matched explicit lane
(`profile-generate` → boot → tight 42 s dump → merge → `profile-use`, CGU key
`rava_interp.998d255fb90c1c88` byte-identical to the consumer's, zero consumption warnings):
candidate `bb9fc345…` vs accepted product across two procedure-exact ABBA sets, 8 pairs —
**wall median +1.08%, CPU +1.32%, accepted faster in 6/8 pairs. KEEP the accepted structural
profile** `target/pgo-guava-structural-tight.profdata` is therefore retained but NOT promoted;
the shipped product was rebuilt on it as a compromise with one expected PGO discard
(`Vm::jit_native_served_deep`, up to 8,746,974 counts — the source changed after training;
exactly one, no others).

⇒ **ACCEPT and CUTOVER.** The final smoke boot on the installed product: **19.03 s wall /
34.1 CPU-s, `Done (0.336s)`, 0 panics** — inside the ≤22 s gate. Installed as
`target/release/rava`, SHA256
`8e4892b3d836027ae0f12fce3ef517b0b3321c64246a085c686fd5fb5e0d3ce7` (16,342,088 B); the
superseded accepted product is preserved at `/tmp/rava-classfacts-fresh-explicit`
(`f75c17ca40d8d3af039f3f0a8b1a7936d86fd53a3ea99391730f88229ed5d8fb`, 16,581,256 B). The
shape-only classes — `ImmutableTable`, the `Regular*`/`Singleton*` impls, the Builders — now
compile instead of being pinned interpreted, and the guava question the gates ask is answered
by a table, a bit and a type closure: nothing anywhere admits by name-shape.

## ✅ THE FORCE-NATIVE HASHBIMAP ADMISSION IS AN EXACT PROVIDER FACT — the last guava prefix deleted under a zero-divergence census

"Nothing anywhere admits by name-shape" — the Guava round's closing claim — was true of the two
JIT gates it touched. `force_native_collection` (opcodes/gates.rs) is a third gate the sentence did
not cover: its HashBiMap arm was `class.starts_with("com/google/common/collect/HashBiMap")`, the
last guava PREFIX in that gate. Unlike `wide_tier_admits`, which had no guava question of its own
and lost the arm outright, this gate has a real one — the HashBiMap's own bytecode mis-executes
here (an equal-but-distinct String key misses despite correct hashCode/equals), so every method
on the class must route to the native map layer. A real exact-identity question gets the fact,
not the shape.

**The semantic-provider exception.** `FACT_NATIVE_MAP` deliberately does NOT answer it: that bit
already answers every `MAP_CLASSES` row — including `java/util/HashMap`, which is served but must
never be force-routed. Force-native is route-everything-away-from-bytecode for a class the native
map layer COMPLETELY supplies, a stronger question only exact providers license. So the facts
word gains its seventh bit — `FACT_FORCE_NATIVE_MAP` (1 << 6), reading exactly one table,
`map::FORCE_NATIVE_MAP_CLASSES`, today the singleton `com/google/common/collect/HashBiMap` — and
the gate's guava arm is now `facts(class) & FACT_FORCE_NATIVE_MAP != 0`. The provider proof is
static, not historical: `create`/`create(int)`/`create(Map)` (map::dispatch's create arm) and
`inverse()` (map_method) both allocate the EXACT parent name with a native `$map`/`$inverseof`
view, and the nested classes the prefix additionally admitted — `HashBiMap$BiEntry`/`$Itr`/
`$Inverse`/`$InverseKeySet` — are born only inside HashBiMap bytecode that cannot execute while
every method of the exact class force-routes. No nested name can reach the gate as owner, actual
or lambda owner; the provider cannot produce one. The licensing is the RegisterNatives one — an
exact identity the native layer completely supplies, never a workload-derived allowlist.

**Deletion licensed by a zero-divergence census, taken before anything was deleted.** The prefix
matched four names the fact does not; the license to drop them is measured, not argued:

```
pristine boot        0 divergent asks          Done (0.364s)
existing probes      49/49 explicit completed:0
```

Not one ask — boot or probe — where the shape and the fact disagreed. The census hook was removed
with the arm: no served history routes the product, and the forbidden literal is gone from the
tree.

**Validation.** The Guava round's drift disciplines, re-applied: `map::classes::
force_native_map_tests` 3 passed / 0 failed (every member a `MAP_CLASSES` row; the gate routes
exactly the members, refusing all four nested names; no member listed twice);
`owner_bits::fact_tests` 10 passed / 0 failed, now including
`every_force_native_map_member_lands_the_bit` (HashBiMap carries both `FACT_NATIVE_MAP` and
`FACT_FORCE_NATIVE_MAP`), `the_nested_hashbimap_classes_are_not_providers` (all four nested names
read zero) and `a_jdk_map_is_not_force_native` (`java/util/HashMap`/`TreeMap`/`Map` carry
`FACT_NATIVE_MAP` alone — conflating the bits would force-route a JDK map); the memo-agreement
test re-pinned to `HashBiMap`/`HashBiMap$BiEntry` in both arms; `map::classes::
guava_immutable_tests` 3 passed / 0 failed. **16 scoped tests, 0 failures.**

⇒ **ACCEPT and CUTOVER — as policy, not as a win.** The exact-only artifact is
`target/release/rava`, SHA256
`f3e7e1cff13ab55ad8d22e160f4370b01d01ff3a993f9b44495a23eb423dc7b1` (16,341,256 B), rebuilt on
the retained structural profile with exactly **3 PGO discards, all in changed or knock-on
functions, none unexplained**. Final smoke: **launch→`Done` ~20 s, `Done (0.291s)`, 0 panics**,
inside the ≤22 s gate. **No wall claim is made, and none is earned:** the admission answer is
identical on every ask the boot and the probes could produce, so this preserves behavior rather
than moving it — its product is a deleted forbidden input, not a wall lever. The shape arms that
remain in the gate (netty's `io/netty/` event-loop prefixes, `is_fastutil_objmap`, the
`BufferedInputStream` suffix) answer questions that are genuinely shape questions — a wrapper's
package is a jar's business. The HashBiMap arm never was one: its question was exact identity, and
exact questions get tables and bits, not prefixes.

## ⛔ THE SITE RE-ARM'S 24M PREMISE WAS AN ALL-ORIGIN COUNT — cumulative host falls 1,144,238, STOP at the census

The machine-code site re-arm was proposed against a ~24M-crossing population. That number is the
`[PC-CB]` invoke FAMILY — every `cb(0)` from any origin and route: dispatch misses, direct misses,
`rava_indy`, map callbacks, interpreted-frame continuations (row 578's fixed 12 s census read
invoke 24,790,738 the same way) — not the population a re-arm can touch. A callback-emitted
compiled-call site can only eliminate crossings that are today compiled-origin falls to the Rust
host, and the meters that count exactly those crossings read one to two orders lower.

Censused on the accepted product with the tree's own instruments (`RAVA_CALLBACK_STATS=1
RAVA_JIT_PIC_PROBE=1 RAVA_PROFILE_PC=1 RAVA_PROFILE_PC_MS=12000`; pristine world restored before
the boot, port 25599, snapshots/checkpoints unset), 58 blocks × 12 s cumulative, `Done (0.318s)`:

```
[PIC-COV] dynamic calls out of compiled code = 2081110689
[PIC-COV]   machine-code PIC hit (no Rust)         2079966451 (99.9%)  <- real PIC coverage
[PIC-COV]   fell to Rust host                         1144238 (0.0%)
[PIC-COV]       PIC miss -> rava_dispatch_miss        1065583 (0.0%)
```

**The one-sided bound.** Host falls = **1,144,238** (PIC miss 1,065,583 + plain invoke 78,655),
0.055% of the 2,081,110,689 dynamic calls out of compiled code. The indy fallback escapes both of
those counters, so the full in-scope superset adds the `[PC-INDY]` `rava_indy` callback census —
**994,187** (noncapturing 315 + cap_mintable 454,677 + cap_boxidx 22,611 + cap_other_tag 0 +
not_metafactory 516,584) — for **2,138,425**. Both bounds saturate inside the window (falls flat
at 1,144,238 from block 19, plain invoke flat at 78,655 from block 7), so the 58-block duration
inflates neither and any boot-only population is a subset; row 577's fixed 12 s prefix had already
put the same falls at 652,106 on the head7 product — the second independent reading.

Against the pre-registered ≥10M gate: **1,144,238 is 8.8× under, 2,138,425 is 4.7× under.** The
absent guest/native split cannot change a one-sided bound, so the STOP fires on the census alone.

⇒ **STOP: the site re-arm is rejected at the census, before design.** No implementation was
built, no source was changed, no candidate build/test/boot was made — and no wall claim is made
or earned: the census is counts, not a clock, and the instrumented run's own wall is not
performance evidence. State restored and verified after the census run: world back from
`world_pristine`, tree clean at `a28f9654`, `target/release/rava` unchanged at `f3e7e1cf…`,
port free. Raw evidence: `/tmp/rearm_census/` (`run_boot.sh`, `boot_stderr.log`,
`parse_census.py`).

⭐This is the tree's own rule collecting a third time (rows 445-448; the 21.1 M refusals section):
convert a population into the population the change can actually touch BEFORE pricing it. An
all-origin callback count is not a crossing population, and the crossing-denominated census on
the current product is the number any re-open of this line must start from.

## ⛔ COMBO-PREHASH'S VERIFICATION COST MORE THAN IT SAVED — +97 ns/call wrong-way disjoint, STOP at the meter

The combo-hash line already carries its own price here: the `valkey_memo_with`/`combo_hash_of`
thread-local caches are 2.2% of the boot profile ("The mechanism behind the 38%"), an honest
ceiling of ~0.73 s for everything those caches do. The candidate (`c1cf74ce`, release-fat)
pre-hashed the combos so a lookup could skip re-derivation.

Pre-registered gate: GO required a ≥25% REDUCTION in per-derivation cost. Same-binary pristine,
3 boots per arm, n=490,224 combo-hash derivations per boot, ns per call:

```
ON   1019 / 998 / 997
OFF   925 / 900 / 899
=> +97 ns/call ≈ +10.7% — every ON boot above every OFF boot, DISJOINT
```

⇒ **STOP: combo-prehash is rejected on the time leg alone.** The gate demanded −25% and the
meter read +10.7% on the wrong side of zero with no overlap between the arms — a one-sided
bound, so the `[COMBOPRE]` hit row that would decompose the effect cannot rescue it. That row is
unavailable on this box anyway: the PC sampler that produces it is Windows-only and returns
early on Linux. The same logic stopped the site re-arm one entry above without its guest/native
split.

**And the honest reason it lost: the verification overhead exceeded the work it saved.** The
armed path measured +97 ns/call over the plain path on all 490,224 derivations — 490,224 × 97 ns
≈ +47.6 ms per boot of overhead, against a ~0.73 s ceiling for eliminating the whole population.
Correctness held throughout — ON==OFF on the targeted semantic probes, focused tests 25+52+2
all pass — so the meter, not the semantics, is the verdict. No wall claim is made or earned:
ns/call is not a clock, and no boot wall was compared.

State after the rejection: the candidate was never promoted or committed — all four source
files reverted to HEAD `6d3f9c92`, tree/world/ports/process clean, the product untouched at
`f3e7e1cf…`.

## ✅ THE NETTY inEventLoop VERDICT IS A STRUCTURAL PROVIDER — the event-loop prefix admission deleted under a zero-divergence census (2026-09-17)

The HashBiMap round's closing sentence listed "netty's `io/netty/` event-loop prefixes" among
the shape arms that remain. True of the gate's other netty arms — but `inEventLoop` was never
a shape question: the ask is sound exactly for classes whose type closure contains
`EventExecutorGroup`, and a closure is a structural fact, not a jar's business.

**The proof, a full-classpath census** (pure-python classfile parser, exhaustive; raw JSON
`/tmp/netty_ineventloop_census.json`): **40 jars, 29,769 class entries parsed (29,760
effective); 22 classes expose an instance `inEventLoop`, and the prefix population and the
`EventExecutorGroup` closure population are the SAME 22 names** — zero divergences either way,
every one of them under `io/netty/`. **71 constant-pool `inEventLoop` refs across 12 ask
owners**, every owner a closure member but one: `SslHandler`'s private **static** helper (one
Methodref, one call site), licensed as the exact tuple it is — owner
`io/netty/handler/ssl/SslHandler`, name `inEventLoop`, desc `(Ljava/util/concurrent/Executor;)Z`.

**The design.** The verdict moved out of the prefix and into net/mod.rs's semantic provider
table: name first, then the exact Ssl owner compare, then the loader's ALREADY-memoised
per-loader type closure to the root — no new cache, no new invalidation. The gate sees no
descriptor, so the owner compare alone settles the Ssl row; the dispatch side pins the full
tuple. GIL-free workers cannot load, so a fragment closure can only MISS the root, never
contain it wrongly — every ask the exact owner compare did not settle fails to bytecode (gate
false), never a wrong serve. The cascade's net stage admits the bare `inEventLoop` name
owner-agnostically, and `net::dispatch` stays the sole authority for who is served.

**Verification.** Runtime differential (`RAVA_NETTY_EVENTLOOP_DIFF`): **49/49 probes explicit
zero**, and the full pristine 45 s census boot printed `[ELDIFF] netty inEventLoop old-XOR-new
divergent asks: 0` after `Done` — not one ask where prefix and provider disagreed. Focused:
**63 tests pass, cargo checks pass**. The census/diff hooks were removed with the arm; no
served history routes the product.

⇒ **ACCEPT as compliance cleanup — no wall claim is made or earned.** Prefix and provider
answer identically on every ask the classpath, boot and probes could produce; the product is a
deleted forbidden input, not a lever. Candidate `c2dacd1a…` (16,254,920 B, preserved at
`/tmp/rava-netty-eventloop-final-candidate`), rebuilt on the retained structural profile with
exactly **5 PGO discards**, all expected (`jit_native_served_deep`, `owner_bits::compute_facts`,
`natives::owner_agnostic_name`, `natives::cascade::dispatch_owner_cascade`,
`opcodes::gates::force_native_collection`). Final PGO smoke, single cold boot: launch→`Done`
**18.63 s**, internal `Done` **0.276 s**, 0 panics — recorded as regression evidence only, not a
timing claim. **Scope boundary: the
package-prefix admission was removed for `inEventLoop` ONLY** — the unrelated netty
`execute`/`run` arms in the gate and `net::dispatch` are untouched and remain future work.

## ✅ NETTY `execute` CUT OVER TO THE STRUCTURAL PROVIDER — compliance-only, zero divergence both seams; the `run` prefix stays STOP (2026-09-17)

The inEventLoop round's closing scope left "the unrelated netty `execute`/`run` arms in the gate
and `net::dispatch`" as future work. `execute` is now cut over the same way: the verdict moved
out of the old gate's class-name shapes and into the same structural fact — the per-loader
`EventExecutorGroup` closure. `run` is not, and the census below is the reason.

**The proof, static** (raw JSON `/tmp/netty_exec_run_census.json`): **40 jars, 29,769 class
entries parsed (29,760 effective), 0 errors; 193 `execute` Methodrefs total**. The **9
old-gate-served ref owners are all `EventExecutorGroup` closure members**; the **52 old-only
class-name shapes — none exposes an `execute`, zero effective ask divergence**. Outside the
closure there is exactly one semantic row — **TPTX** — licensed as the exact tuple it is,
**swallow**; the force gate pins the exact descriptors `(Ljava/lang/Runnable;)V` and
`(Ljava/lang/Runnable;Z)V`.

**The design, permanent**: descriptor-aware force gate, exact TPTX tuple, per-loader
`EventExecutorGroup` closure, gilfree unknown false, owner-agnostic cascade route to the
authoritative provider.

**Verification.** Full differential boot, both seams: **gate asks 6 (3 old&new, 3 neither,
old-only/new-only 0), workers 0; dispatch 4 (3 inline->inline, 1 decline->decline); zero
divergence on both seams**, boot to `Done`. **Scoped tests pass, cargo checks pass.** Final PGO
smoke, single cold boot: `Done` **19.45 s**, **0 panics** — regression evidence only, not a
timing claim. Candidate `d7c5279d…` (16,254,792 B), rebuilt with exactly **5 expected PGO
discards**, all at named touched/knock-on seams.

⇒ **ACCEPT as compliance-only — no wall claim or credit.** Old and new answered identically on
every ask the classpath and the boot could produce; the product is a deleted forbidden input,
not a lever.

**Scope boundary: the netty `run` prefix was NOT cut over — it remains future work, unresolved,
STOP.** The run side of the census found old-only reachable/semantic rows, so `run` must remain
unchanged exactly as it is; this entry does not carry it along silently.

## ⛔ THE IMMUTABLE-ITERATOR SHARING SUBSET PRICES AT 0.028 s, 14.2% OF THE 0.2 s GATE — the stale ~1.4% is superseded, STOP at the census (2026-09-17)

The one sound form of sharing the iterator's array with its collection — a receiver whose
`$immutable` is set — is recorded in `docs/what-is-left-and-why.md` §3 as "a subset worth ~1.4% —
recorded, not built". That figure had no census behind it. One full current-product census,
`--features contract-stat`, behind the eligibility hook:

```
hook-A calls             1,640,743
  eligible                 165,197   (10.07%)   over 453,718 eligible elements
  declined: kind         1,475,323
  declined: view               223
  declined: noelems / sparelen / reversed   0
iterator_of births       1,448,969
```

Strict conservative upper bound — every eligible call priced at a full 150 ns arm, every
eligible element at 8 ns:

```
165,197 × 150 ns + 453,718 × 8 ns = 28,409,294 ns = 0.028409 s
```

**14.2% of the 0.2 s gate. The gate does not clear.** The `contract-stat` feature's
instrumentation leaves the retained PGO symbols unmatched, but that is an artifact of the
instrument build: the counts are runtime- and layout-independent, and the census run's own wall
is not performance evidence — **no wall claim is made or earned.**

⇒ **STOP: the immutable-iterator sharing arm is rejected at the census, before design.** No
implementation was built, no source was changed, and the accepted product `d7c5279d…` is
untouched. The ~1.4% recorded for this subset is superseded by the 0.028409 s bound above.

## ⛔ GENERIC HASH+EQUALS FOR MAP KEYS — the two dominant record classes hash the same fields they render, STOP at the classfile (2026-09-17)

Replacing rendered-string map keys with generic guest `hashCode`+`equals` was the one target the
ledger left open on this line. Priced with a counts-only diagnostic on the current accepted
product (`d7c5279d…`; counters only, nothing modified, `Done` healthy), per boot:

```
KEYTIME derivations     1.25 M            1.680 s total
  int                   17,373       26 ns
  identity             543,946      208 ns
  identity-str         157,932      148 ns
  combo                 490,224    1,038 ns
  structural             40,525      1.034 s   (Obj 31,910 / 1.026 s; Ref 8,615 / 0.008 s)

structural, by class:
  Sum                        288      0.603 s   ~2.1 ms per derivation
  TaggedChoiceCacheKey       832      0.281 s   ~338 us
  every other structural class        ≤ 0.150 s  <- under the 0.2 s gate even if free
KEYRENDER tick   hit 59,057   miss 108,100   rejected 82,843
STRUCTKEY        1,603,083 ops   310,180 allocs
```

**Structural keys are 3% of the count and 62% of the bill** — 40,525 of 1.25 M derivations,
1.034 s of the 1.680 s — and that bill is two classes: **`Sum` (288, 0.603 s) and
`TaggedChoiceCacheKey` (832, 0.281 s) = 0.884 s together, 53% of the whole key-derivation bill
from 1,120 derivations.** Every other structural class together is ≤0.150 s — under the 0.2 s
gate even if eliminating it were free. **But that residual bound holds only by excluding the
two dominant records, and for them the classfile is the verdict:**

- **`Sum` is a record.** Its `toString` recursively renders `f`,`g`, and the record `ObjectMethods`
  `hashCode`/`equals` operate on those same `f`,`g` fields — a generic hash+equals key runs the
  identical recursive guest walk, guest `hashCode`/`equals` where the render ran guest `toString`.
  Only the string materialisation goes; the semantic work stays.
- **`TaggedChoiceCacheKey` is a record** (fields `name`, `keyType`, `types`); its `toString`,
  `hashCode` and `equals` all run the same `ObjectMethods` bootstrap over those same fields.

The non-structural remainder needs no new verdict: `int` + `identity` + `identity-str` total
~0.137 s, and `combo` (490,224 at 1,038 ns) is the exact population the combo-prehash round
already closed at the meter. `RECORDRENDER` reads **records=0** in the record-special counter —
that zero is a fact about the counter, not the population: javap and the classfile declarations
prove both dominant classes ARE Java records.

**And the design space was already measured shut.** The sound form — equals-verified, salted —
measured **slower** and **broke parallel-lane reproducibility**; the fast hash-only form was
**unsound, real collisions** observed on this workload. Every sound variant still performs the
recursive walk the render performs, and the one built soundly lost on time.

⭐A residual bound is a verdict about the residue, not about what it leaves out — "everything
else is under the gate" closed the tail here only because the classfile separately closed the two
classes the bound had to exclude.

⇒ **STOP: generic guest hash+equals for map keys — no design, no implementation, no candidate.**
The residue is priced out; the dominant records still require the same recursive semantic work;
the previously measured sound form loses. KEYTIME is the diagnostic's derivation total, not a
wall share — the counts-only run's own wall is not performance evidence. **No wall claim is made
or earned.** The diagnostic modified nothing; the accepted product is untouched.

## ✅ THE CLI THRESHOLD CONTRACT CORRECTED — the hardcoded `.unwrap_or(512)` export that made the engine's `tier0 ? 16 : 512` fallback unreachable (2026-09-17)

**Cause.** The CLI read its threshold with `.unwrap_or(512)` and exported the env var
unconditionally, so the engine's `tier0 ? 16 : 512` fallback could never fire — the CLI's
hardcoded 512 rode in even when the user had set no threshold at all, and the engine's own
default was dead code behind it.

**Fix — configuration correctness, no engine change.** The Config threshold is an `Option`:
when absent it is simply not exported, and the default template ships the threshold commented
out. Explicit precedence is untouched — shell over file over legacy, exactly as before.

**Matrix — six actual CLI cases, all pass:** threshold absent + tier0 → auto (template
commented); absent plain → auto; file 64 → 64; shell 32 over file 64 → 32; legacy 24 → 24;
repo explicit 512 → 512.

**Verification.** Cargo check passes. Final PGO candidate `c8f3d555…` (16,257,032 B); the
repo-config full boot ran threshold 512 to `Done` in **20.66 s**, 0 panics — recorded as
regression evidence only, not a timing claim.

⇒ **ACCEPT as configuration correctness — no wall claim or credit.** T16 vs T512 was
previously measured neutral 4/4, so no engine/default performance result is claimed, and the
explicit repository benchmark behavior remains 512. The product is not yet promoted.

## ✅ NETTY `Thread.start` CUT OVER TO THE EXACT CENSUS TABLE — compliance-only; the runnable-target (S2) prefix stays STOP (2026-09-17)

The S1 `Thread.start` skip admitted every `io/netty/` RECEIVER by class-name prefix — the same
forbidden-input shape the inEventLoop and `execute` rounds deleted. Cut over the same way: the
verdict moved out of the prefix and into a census-derived table.

**The proof, static** (same pure-python classfile parser, exhaustive; raw JSON
`/tmp/netty_thread_subclass_census.json`, git `bc908a7e`): **40 jars, 29,760 effective classes,
0 parse errors; 13 `java/lang/Thread` descendants on the classpath, exactly 3 under `io/netty/`**.
The single-name form — `tcls == FastThreadLocalThread`, attractive because the census shows
`FastThreadLocalThread` has **0 subclasses** and is what `DefaultThreadFactory` allocates for the
event loops this skip exists for — was rejected by the census itself: two more netty Thread
descendants exist, and a one-name row would silently start them. The exact table that replaces the
prefix, licensed by that census:

```
io/netty/handler/codec/http/multipart/DeleteFileOnExitHook$1
io/netty/util/concurrent/FastThreadLocalThread
io/netty/util/internal/ThreadLocalRandom$1
```

**The design, permanent**: the receiver check in `reflect/concurrent.rs` is now
`NETTY_THREAD_SUBCLASSES.contains(&tcls)` — nothing else in the `start` arm changed (the
double-start `IllegalThreadStateException` guard and the first-thread/daemon model are untouched).
The table is pinned by a source test: a name edited, added or dropped must fail the build, not
silently change which threads get skipped.

**Verification.** Focused **3/3 tests pass, cargo checks pass**. Final PGO candidate `2a12720e…`
(16,287,176 B, preserved at `/tmp/rava-netty-thread-s1-final-candidate`), rebuilt with exactly
**6 expected PGO discards**, all at touched/knock-on seams — `jit_native_served_deep`,
`owner_bits::compute_facts`, `natives::owner_agnostic_name`,
`natives::cascade::dispatch_owner_cascade`, `opcodes::gates::force_native_collection`, and
`reflect::dispatch::by_family`. Pristine smoke, single cold boot: `Done` **19.24 s**, internal
`Done` **0.275 s**, 0 panics — recorded as regression evidence only, not a timing claim.

⇒ **ACCEPT as compliance-only — no wall claim or credit.** Prefix and table answer identically on
every Thread receiver the classpath can produce; the product is a deleted forbidden input, not a
lever.

⚠**The table is CLASSPATH-SCOPED, and the contract travels with it.** `tcls` is the object's
allocation tag and this classpath defines no classes dynamically and builds no proxies, so the
set is closed — but the three names are facts about TODAY'S classpath, not about Netty. Any Netty
upgrade or classpath change MUST re-run the census and re-derive the table: a new Netty Thread
subclass would otherwise be silently started, a removed one pointlessly skipped.

**Scope boundary: S2 — the runnable-target `starts_with("io/netty/")` check on the `$runnable`
object — was NOT cut over and remains STOP, unresolved.** A Thread-descendant census cannot
license it: Runnable targets are not Thread subclasses, and their wrapper population is
heterogeneous, so no table this census can produce is a verdict for S2. It keeps its prefix
exactly as it is, and this entry does not carry it along silently. The product is not yet promoted.

## ✅ THE WORKER `findIntervalStart` ARM NOW ADMITS ONLY ITS EXACT OWNER — the lock-free lane's name+descriptor admission corrected to the interpreter/jit_run contract (2026-09-17)

**Defect.** The WS-NATIVE fast path in `jit_call_body.rs` admitted a static call on name and
descriptor alone — any `findIntervalStart` with `([FF)I`, from any owner. Its twins, the
`Vm::invoke` intercept and the `jit_run` compile-deny pin, both require the exact class
`net/minecraft/util/CubicSpline$Multipoint`. A foreign class exposing the same signature would
have been served the native interval-search on the worker lane while running bytecode on the main
lane — an admission the twins never granted, and one the raw-cell decode (arg0 as a guest
`float[]`) is not licensed to make for anyone else.

**Fix — one exact conjunct.** The arm resolves the constant-pool Methodref and now requires
`m.owner == "net/minecraft/util/CubicSpline$Multipoint"` beside name and descriptor, matching
`vm/invoke.rs` and `jit_run.rs`. A foreign same-signature call fails the conjunction and falls to
the locked general path — bytecode, exactly as the twins already serve it. Nothing else in the
arm changed.

**Verification.** Cargo check passes, **0 errors with zero warning delta**; focused **41 tests
pass**; reviewer approved. Final PGO candidate `1ce18ecbb5a7…` (16,031,496 B), rebuilt with exactly
**7 expected PGO discards**, including the touched `jit_call_body` seam. Exact-env pristine smoke,
single cold boot: wall **19.8 s**, internal `Done` **0.335 s**, **0 genuine panics**, bind clean —
recorded as regression evidence only, not a timing claim. The candidate is untouched and not yet
promoted.

⇒ **ACCEPT as compliance/correctness — no wall claim or credit.** All three seams now admit the
same single owner on the same single signature; the product is a tightened admission, not a
lever.

⚠**Test boundary.** No runtime fixture exercises a foreign same-signature `findIntervalStart`;
that refusal is proven by the admission semantics (the exact-owner conjunction cannot admit
another owner) and by review, not by an observed foreign-owner test. The default boot exercises
the real owner end-to-end.

## ⛔ THE COLLECTION SUBSTRING CENSUSED — dynamic STOP: two JDK names carry 99.6% of the positive volume, but the two-halved ROOTS+closure architecture lacks authoritative roots and a closure proof, so the substring stays (2026-09-17)

`is_collection` was the last of the nine name-keyed routing predicates (row 3632) without a
dynamic census. This round instrumented the predicate itself and the `stream::dispatch` collection
arm (the `o if is_collection(o)` arm calling `coll_op`) with TEMPORARY, env-gated counters
(`RAVA_COLL_CENSUS=1` + `RAVA_SERVED_CENSUS=1` + `RAVA_CENSUS_MS=105000`), ran ONE pristine full
boot on the standard env, and reverted the instrumentation from the tree completely. The census is
evidence about the population; it licenses nothing in the product, and per the no-adapt rule
(row 3643) its names are EVIDENCE ONLY — no production decision may key on any of them.

**The run.** Diagnostic build sha1 `74c03e31127e86cf11ae5fa0a2d503dbbaaecf8a` (product release
binary sha1 `896bbcd9770553f41ed2d89dafac25104b9cf419` untouched), repo head `60ed8e88…`, world
restored from `world_pristine`, port 25599, `Done (0.357s)` at 07:15:25, census timer fired at
105 s mid-boot with the server still serving. Artifact: `/tmp/coll_census_evidence/` (README,
evidence.json, three TSV tables, the positive_never list, full boot log).

**The counts.** `is_collection` answered TRUE **29,877,166 times over 178 distinct names**; the
collection arm SERVED **18,532,844 calls over 92 distinct owner#method pairs from 33 distinct
owners**; the all-stage [SERVED] census held **138 distinct classes** at the timer (top-20 printed
only). The 178 positives are, by family and calls: JDK 22 names / 29,854,795 (99.92% of volume);
Fastutil 106 / 10,106; Guava 41 / 5,717; Minecraft 6 / 3,368; Gson 1 / 3,176; Netty 2 / 3.

**The concentration.** Two JDK names carry **99.56% of all positive volume**: `java/util/ArrayList`
(19,424,559) + `java/util/concurrent/ConcurrentLinkedQueue` (10,321,214) = 29.75 M of 29.88 M. The
coll-arm's served volume is even narrower: **`ConcurrentLinkedQueue#peek` (10,309,036) +
`ArrayList#clear` (7,746,184) = 18,055,220, 97.4% of the 18.5 M served calls** — two operations on
two classes. And 145 of the 178 positives (81%) were NEVER served by the collection arm at all.

**The census found cross-family false positives, with routes.** `HolderSetCodec#toString` — a
Minecraft codec helper that renders a list of holder references, served 1,773 times by the coll arm
purely because its class name contains "Set" — is the standing empirical proof that the substring
admits non-collection owners and routes them to collection semantics. `IntImmutableList` and
`AbstractIntList` appear as positives whose only coll-arm service was `#getClass` (33 calls each)
— name-only traffic, no collection operation ever served. The cross-family name traffic in the
all-stage [SERVED] top-20 (AtomicReference, Optional, Boolean, ThreadLocal…) is NOT positive volume:
the coll-arm census counts only the `o if is_collection(o)` arm, while [SERVED] counts every stage
(nio/net/reflect/map/stream), so the two tables cannot be compared as populations.

**The architecture prerequisite.** The static/dynamic review verdict on the two-halved
ROOTS+closure design — derive an authoritative root set, close it under subclassing/implementation
once, admit exactly the closed set — is that it MAY be sound only after (a) authoritative roots
are derived from classfile facts (interfaces implemented, superclasses extended), not from names,
and (b) closure is PROVEN, not assumed. The same rootset+closure license that closed the
`Thread.start` round (row 4830) applies: this classpath defines no classes dynamically and builds
no proxies, so a static closure is in principle checkable — but this run did not do that work.

**The proof is incomplete, on both halves.** The run did NOT enumerate all-stage owners — the
all-stage [SERVED] table is rank-truncated to its top-20 of 138 classes at the timer, with no full
owner table and no census of the map/reflect/nio stages' collection-class traffic — and it did NOT
prove that the closed ROOTS set covers the census: the 145 never-served positives and the 92
coll-arm ops are subsets observed under one boot, one seed, and a 105 s window that excludes the
tail. A full-boot owner enumeration with a closure∩ROOTS coverage proof remains undone. Counts are
lower bounds for a whole run, exact for the prefix measured.

⭐**A census of the predicate is not a verdict on its architecture.** The dynamic counts price the
population (two JDK names = 99.6% of volume, two ops = 97.4% of served) and expose the substring's
cross-family false routes (`HolderSetCodec#toString` 1,773; `IntImmutableList` et al.
`#getClass`-only) — but concentration alone cannot license admission of a rootset the closure proof
was never run for, and rank-truncated tables cannot stand in for all-stage enumeration.

⇒ **STOP: collection structural-provider cutover — no design, no implementation, no candidate.**
The substring `is_collection` remains exactly as it is; the instrumentation was fully reverted; no
source, build, or test was run in this round beyond the diagnostic boot. **No wall claim is made
or earned** — these are COUNTS from one boot under one seed, not timings; the boot cannot resolve
anything under its ±8% floor (row 482), so no timing inference is drawn. Per the no-adapt rule, none
of the 178 names, the 92 ops, or the served top-20 may key any product decision.

## ✅ THE CHAIN GATE NOW ADMISSIONS BY TYPE, NOT BY NAME — CompletableFuture fast path plus a strict CompletionStage subtype, closed against unknowns (2026-09-17)

**Defect.** The old CHAIN gate token-matched the strings `CompletableFuture` and `CompletionStage`
wherever they appeared in the receiver's name. A token match is not a type verdict: a name like
`MyCompletableFutureLikeThing` admitted, while a genuine completion stage reached through a
renamed or wrapper class failed and fell to the slow path. The gate's promise — "completion-style
receivers take the chain lane" — was keyed to spelling, not to the type hierarchy it claims to
police.

**Fix — exact fast path plus a strict subtype conjunct.** The first attempt was a lenient
provider-style form (`name_is_subtype_strict` alone over a loose name derivation); the reviewer
blocked it because its inconclusive results for `FutureTask`/Guava chains would have admitted
receivers the type system cannot vouch for — the same impostor problem one layer up. The accepted
form is narrower: an exact `java/util/concurrent/CompletableFuture` fast path, plus
`name_is_subtype_strict(..., CompletionStage)` for the interface family; unknown and
inconclusive receivers now FAIL CLOSED, and the exception path is unchanged.

**Verification.** Cargo check passes; the closest **32 tests pass 32/32**, and the broader suite
passes **616/617** with **1 pre-existing GC failure, byte-identical to baseline**. Reviewer
approved. Final PGO candidate `75bd488d…` (**16,036,424 B**) rebuilt with exactly **8 expected PGO
discards, including the touched `future_method` seam**. Exact-env pristine smoke, single cold
boot: wall **20.86 s**, internal `Done` **0.343 s**, **0 genuine panics**, **0 gate throws**,
real `CompletableFuture` traffic only — recorded as regression evidence, **not a timing claim**:
the boot cannot resolve anything under its ±8% floor (row 482), so no wall credit is taken.
The candidate is untouched and not yet promoted.

⇒ **ACCEPT as compliance/correctness — no wall claim or credit.** The gate now admits exactly the
type family it names, fails closed on everything it cannot prove, and no longer keys on spelling.

⚠**Test boundary.** No runtime fixture exercises a receiver that is name-similar but
type-unrelated (the impostor direction) or a genuine `CompletionStage` implementation that is
not a `CompletableFuture` (the interface-subtype direction); both refusals and admissions are
proven by the admission semantics — the exact-name fast path cannot admit impostors and the strict
subtype conjunct cannot reject genuine implementations — and by review, not by an observed
foreign-receiver boot. The pristine smoke exercises the real `CompletableFuture` end-to-end only.

## ✅ THE SCALAR ATOMIC'S STORAGE KIND NOW COMES FROM AN EXACT RECEIVER TABLE — the substring receiver that embedded-token names could misfile is gone; routing, bodies and defaults unchanged (2026-09-17)

**Defect.** `atomic_method` asked TWO SUBSTRING QUESTIONS ON THE RECEIVER'S CLASS NAME, PER
ATOMIC OPERATION: `contains("AtomicLong")` chose the Long cell, `contains("Reference")` the Ref
cell, `contains("DoubleAdder")` the Double cell — exactly the shape the census killed on
`Thread.start`. A "Reference" token matches jctools' `AtomicReferenceArrayQueue` (a queue, not a
scalar atomic), and an "AtomicLong" token matches any name that merely embeds it
(`MyAtomicLongReference`). The storage verdict was keyed to spelling, not to the receiver.

**Fix — two exact tables, pure functions of the owner string.** Atomic names now appear ONLY in
`scalar_atomic_storage` / `adder_storage` (`reflect/atomics.rs`), which answer without
allocating:

```
scalar_atomic_storage:   AtomicLong -> Long
                         AtomicReference / AtomicStampedReference / AtomicMarkableReference -> Ref
                         AtomicInteger / AtomicBoolean / unknown & impostors -> Int   (historic default)
adder_storage:           DoubleAdder -> Double
                         LongAdder / unknown -> Long                              (historic default)
```

The uninitialised-cell default (`AtomicBoolean` folds into Int through the same `num()` path) is
unchanged, every method body is unchanged, and upstream routing/service acceptance is unchanged:
the callers (`atomic_method`, `adder_method`) now read the two booleans/discriminant the table
answers instead of running three substring scans per operation. The jctools
`AtomicReferenceArrayQueue` impostor — which carries BOTH historic tokens ("/atomic/Atomic" +
"Reference") yet is a queue — now defaults to Int with every other unknown, instead of being
handed the Ref cell by its "Reference" substring.

**Census license.** bench/mc-cp.txt, **40 jars / 29,760 effective classes / 0 parse errors**
(2026-09-17, git bc908a7e): no classpath jar shadows the `java/util/concurrent/atomic` package;
the needle-matched jctools queues are the only impostors and are fail-closed by the default.
⚠Re-run the census on any classpath/jar change.

**Verification.** Cargo check passes, **0 errors with zero warning delta**; the compact
table-driven pin `storage_tables_exact_and_fail_closed` passes **1/1**, covering every exact row
both tables admit, the `MyAtomicLongReference` and `DoubleAdderLike` embedders, the jctools
impostor, and the `""` dead-receiver (None-class) arm every consumer passes. Reviewer approved.
Final PGO candidate `5913b28c…` (**16,002,888 B**) rebuilt with exactly **9 expected PGO discards,
including the touched `dispatch::tail` seam**. Exact-env pristine smoke, single cold boot: wall
**20.0 s**, internal `Done` **0.310 s**, **0 genuine panics** — recorded as regression evidence
only, not a timing claim. The candidate is untouched and not yet promoted.

⇒ **ACCEPT as compliance/correctness — no wall claim or credit.** The boot cannot resolve
anything under its ±8% floor (row 482), and this round takes no timing inference: the cutover is
a receiver-verdict correction with identical bodies, not a lever. No dispatch-gate diagnostic was
run — the diagnostic build is excluded from the extra-build budget this round, so no
dispatch-census evidence is claimed for or against the change.

⚠**Test boundary.** No runtime fixture exercises a live jctools `AtomicReferenceArrayQueue`
receiver through `atomic_method`; the impostor's Int default is proven by the table's admission
semantics (an exact-match table cannot admit an unlisted owner) and the pin, not by an observed
foreign-receiver boot. The pristine smoke exercises the classpath's genuine atomics end-to-end
only.

## ✅ REMOVE THE CUSTOM BUFFERED-STREAM NATIVE ROUTE; DELEGATE UNMODELED STREAM READS TO GUEST BYTECODE (2026-09-24)

**Correction to the prior exact-provider route.** `javap -p -c` on Mojang's
`FastBufferedInputStream` shows constructors initialize real `in`, `buffer`, `limit` and `position`
fields; `skip(long)` reads and mutates `position`/`limit`. The native substitution bypassed those
constructors and had no `skip` implementation, so exact-owner matching was not a valid structural
fact and could not preserve the class contract.

**Change.** Removed the custom wrapper from native-I/O admission, forced-native ownership and stream
dispatch. When a stream has no synthetic `$buf`, the generic byte adapter now resolves and invokes
its concrete guest `read(byte[], off, len)` body; single-byte/data-input paths use the same boundary.
Pushback bytes are consumed first, and VM errors propagate instead of being reported as EOF. No
Minecraft class name participates in the decision.

**Verification.** `cargo check -p rava-interp --features next-gen-gc`; focused route/stream tests
(2 + 1 + 4 passed); release build; isolated pristine-world smoke through
`bench/boot_ab_flags.sh - 1 400`. Both identical, no-extra-flag arms reached `Done`: OFF 29.0 s
launch wall / 0.362 s internal `Done`; ON 29.8 s / 0.368 s. No `level.dat` fallback. The 2.76%
wall spread is below this harness's resolution; it is correctness evidence only.

⇒ **ACCEPT as a correctness correction, not an optimization.** The `<5 s` target remains open; the
worker ownership inventory still points to a frozen serial-pilot admission ticket as necessary
groundwork, not a sufficient wall-time lever.
