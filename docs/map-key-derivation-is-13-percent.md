# Map-key derivation is 13.7% of the boot, and 9.7% of it renders 32 fields to a string

## The measurement

`RAVA_KEYTIME` times every top-level key derivation and attributes it to the branch it took.
Depth-gated, so a derivation that re-enters guest code is billed once (`nested` = 6 in a whole
boot, i.e. there is no recursion here — the undepth-gated read was NOT a double count, unlike
`String.valueOf`'s once was):

```
1,500,000 OUTERMOST derivations, 6.459 s total  (~13.7% of a 47 s boot)
  int             27,876 x    50 ns = 0.001 s
  identity        69,677 x   408 ns = 0.028 s
  combo-hash     490,222 x 3,801 ns = 1.863 s
  structural-str 912,225 x 5,004 ns = 4.565 s     <- 9.7% of the boot
```

Five microseconds to derive one map key. For comparison, `bench/NativeCost.java` on this tree:

```
                  rava      java
Map.get           447 ns     3 ns      (149x)
Map.get(Integer)  107 ns     2 ns      <- the alloc-free MapKey::Int path
Objects.equals  1,443 ns     0 ns
List.size          65 ns     0 ns
control loop       41 ns     0 ns
```

The bench's own control already isolates it: an `Integer` key costs 107 ns where a bare `Object`
key costs 447, so **the cost is the KEY DERIVATION, not the bridge**.

## What is being rendered

`RAVA_STRUCTKEY_SCAN` names the key classes:

```
map-key= 147,027  nfields=32  net/minecraft/world/level/block/state/BlockState
map-key= 145,652  nfields= 0  [arena-string]
map-key=  12,764  nfields= 2  net/minecraft/resources/ResourceKey
map-key=  10,506  nfields= 2  net/minecraft/resources/Identifier
```

**`BlockState`, with 32 fields, walked and formatted on every `get` and every `put`.**

## Why it never needed to happen

`HashMap` keys by `hashCode` + `equals`. `BlockState` — like `Block`, `Item`, `EntityType`,
`GameRule`, `LevelResource`, `Holder$Reference` and 255 other classes in this boot — declares
**neither**. Both are inherited from `Object`, so both ARE reference identity: Minecraft interns
its block states precisely so that identity comparison is correct. The canonical-string render
computes an expensive answer to a question whose exact answer is a pointer comparison.

The rule is decidable from bytecode (does any class in the chain DECLARE
`equals(Ljava/lang/Object;)Z` or `hashCode()I`) — permitted evidence, not a name list. A record's
generated `equals` is declared in its classfile and is caught; a natively served class has no
classfile to testify and is refused.

`RAVA_IDENTITY_KEY_BY_CLASS=1` implements it (`equality_is_identity`, memoised per class, 261
classes admitted in a boot).

## SHIPPED, default-ON (`RAVA_NO_IDENTITY_KEY` to disable)

Two corrections were needed before the rule was both correct and useful.

**1. The funnels are four, not five, and the code generator is not one of them.** The first
attempt died with `Unbound values in registry ... world_preset` because the rule ran on the PROBE
half (`map_key_inner`) and not the BUILDER half. Grepping the `MapKey` constructors and reading
each one shows what actually chooses an OBJECT key:

```
map/keyroot.rs   map_key_inner, value_map_key, value_map_key_in
cache.rs         guava LoadingCache: get / getIfPresent / put / invalidate
```

and what does not: `arena_map` only READS keys, `cascade_table`/`map_method` build STRING keys,
and `jit/emit/collections.rs` emits only the `MapKey::Int` fast path. All four now call one
function, `keyroot::identity_first`.

**2. The rule must read the `equals` BODY, not its declaration.** Refusing a class for merely
declaring `equals` refused the one that matters. `javap` on the chain:

```
StateHolder.equals:  aload_0; aload_1; invokespecial java/lang/Object.equals; ireturn
```

`BlockState`'s effective `equals` is `return super.equals(o)` into `java/lang/Object` — reference
identity, written that way ON PURPOSE because block states are interned. The declaration is an
assertion that equality IS identity, not a denial of it. The matcher accepts exactly two shapes —
`return this == o` (both branch polarities) and that delegation, with the constant-pool ref
checked to be `java/lang/Object.equals` — and nothing else. `hashCode` is deliberately not
consulted: the key must agree with `equals` and only with `equals`, and if `equals` is identity
then two distinct objects are unequal however their hashes compare.

With both fixed, `BlockState` is admitted and the boot's derivations move:

```
                  before      after
identity         115,428    345,830
structural-str   866,474    636,072
```

**230,402 derivations off the structural path**, and `BlockState` disappears from the
`RAVA_STRUCTKEY_SCAN` histogram entirely.

### The price, on an instrument that can see it

The boot A/B cannot resolve this (its null test reads +10.30%) and worldgen does not exercise it
(-0.11%). `bench/StateKeyCost.java` reproduces `StateHolder`'s exact shape so the low-noise path
can:

```
                       java    rava OFF   rava ON
identity-equals key     4 ns     495 ns    105 ns    <- 4.7x
value-equals key        4 ns     440 ns    415 ns    <- control, untouched
control loop            0 ns       0 ns      0 ns
```

The control class declares a real field-comparing `equals`; its timing does not move. That is
what says the rule DISCRIMINATES rather than admitting everything — a rule that made both rows
faster would be wrong, not better.

4 gates bit-exact, 21/21 probes match `java -Xint`, boot `Done 1.011` with the registry intact.

### What it is worth, end to end

`bench/NativeCost.java`, same binary, kill switch as the control:

```
                  ON (default)   OFF (RAVA_NO_IDENTITY_KEY)   java -XX
Map.get             ~105 ns              588 ns               3 ns     5.6x
Map.containsKey     ~100 ns              514 ns               3 ns     5.1x
Map.get(Integer)     110 ns              107 ns               2 ns     unchanged (already MapKey::Int)
List.size             68 ns               65 ns               0 ns     unchanged
```

The gap to HotSpot on `Map.get` with an object key goes from **196x to 35x**. `Map.get` is 7.20%
of the boot's 64,088,346 compiled-to-host crossings — the second-largest callee in the census.

The rows that should not move do not move, which is the same discrimination check the targeted
probe makes: an `Integer` key already took the alloc-free path, and `List.size` never derives a
key at all.

### One level deeper: the rule inside a COMPOSITE key

After the top-level rule shipped, `RAVA_KEYTIME` split the remaining structural cost by value
kind and found it was almost entirely arena objects:

```
Str = 146,917 (0.14 s, 937 ns)   Obj = 326,612 (5.26 s, 16,115 ns)   Ref = 16,231 (0.02 s)
```

16 microseconds per object, dominated by `Reference2ObjectArrayMap` — the block-state property
map used AS a key — whose render recurses into every entry. The same rule applies one level
down, and is sound by the same argument: a composite key must distinguish exactly what `equals`
distinguishes, so a nested value whose `equals` IS identity may be represented by its identity.

That alone barely moved (16,115 -> 15,185 ns) because the entries are **enums**, and
`java/lang/Enum` is native-served — it has no classfile to testify, so the rule refused every one
of them. But an enum carries `ACC_ENUM` in its access flags, the JLS forbids overriding
`Enum.equals` (declared `final`, body `this == other`), and an access flag is bytecode evidence.
Admitting on the flag:

```
identity       345,830 -> 585,293   at 185 ns
structural-str 489,760 -> 250,297
Obj structural 326,612 ->  87,149
```

**Structural renders across this whole line of work: 912,225 -> 250,297, a 73% reduction.**

### What the boot says, honestly

The boot harness cannot resolve this, and the reason is now documented rather than assumed: a
null test against a flag that does not exist read **+5.06% min / +10.30% median** in the morning
and **-3.04% min / -5.10% median** in the evening. **The artifact changes SIGN.** The only sound
use of that harness is a paired difference taken in the same session:

```
null test        (8 rounds)  SET -3.04% min  / -5.10% median
kill switch      (8 rounds)  SET -2.27% min  / -1.51% median     (SET = the OLD code)
difference                       +0.8 pt     / +3.6 pt           (new code faster)
```

Consistent in sign with the mechanism and with the counted population, and far too small for this
instrument to have found on its own. The operation-level number is the one to trust:
`Map.get` 588 -> 98 ns, same binary, kill switch as control.

### The 8-millisecond key, and the first wall-time effect this line has produced

A slow-render census (any structural derivation over 10 us, tallied by class) finds what no
earlier instrument could:

```
   289 renders, 2.367 s, **8,189,152 ns avg**  datafixers/types/templates/Sum
   832 renders, 1.421 s,  1,708,181 ns avg     datafixers/DSL$Instances$TaggedChoiceCacheKey
14,137 renders, 0.351 s,     24,852 ns avg     ResourceKey$InternKey
 7,925 renders, 0.197 s,     24,834 ns avg     Identifier
```

**Eight milliseconds for one map key.** DataFixerUpper's type templates are recursive, so
rendering one walks the whole type graph — and DFU uses them as memo-cache keys, so the same
graph is walked again and again. 1,121 derivations carry 3.79 s of a ~43 s boot.

The arena key memo measured NEUTRAL when it was built, and that reading was right at the time:
its population was then the cheap repeats, which the identity rule has since removed. What is
left is exactly what a memo is for. With it on, all four rows leave the census and the worst
remaining render is 0.054 s — **4.34 s of slow renders down to 0.09 s**.

Paired boot measurement, both arms 6 rounds, same session, differenced against the null:

```
kill switch (SET = memo OFF = old)   SET -1.71% min / -3.29% median
null test   (SET = nothing)          SET +3.84% min / +6.67% median
difference  (memo ON faster by)          5.55 pt    /  9.96 pt
```

Two independent estimates agree: **4.34 s counted out of ~43 s is ~10%**, and the differenced
boot reads 5.6-10%. This is the first change in this line whose effect is visible on the boot
itself rather than only on the operation.

## The earlier failure, kept for the record

The boot dies:

```
IllegalStateException: Unbound values in registry ResourceKey[minecraft:root / minecraft:worldgen/world_preset]
  >> Failed to parse minecraft:worldgen/world_preset/normal.json from pack vanilla   (x6)
Failed to load datapacks, can't proceed with server load.
```

That is what a registry map which can no longer find its own entries looks like from outside.

The first cause was found and fixed: **the rule ran on the PROBE half (`map_key_inner`) and not
on the BUILDER half (`value_map_key`)**, which `Map.of`/`copyOf`/drain fill through — so an
admitted class was `put` under a structural key and `get` under an identity key. Fixing that did
not fix the boot, which says there is at least one more derivation entry point that must agree:
`structural_key`, `structural_key_in`, `combo_hash_key` and `key_str_memoed` are each reachable
without passing through either funnel, and the native collection internals compare by keys they
derive themselves.

⇒ **The prerequisite is not the rule; it is that every map key in the engine be derived by ONE
function.** Until then a correct rule silently disagrees with itself across paths — which is
exactly the failure above.

Nothing about the default path changed: 4 gates bit-exact, 21/21 probes match `java -Xint`,
`Done 0.969`.

## The memo: correct, complete, and worth nothing — which locates the cost exactly

`key_str_memoed` existed but read **hit=0, miss=4, rejected=749,996**, because it returns early
for anything that is not a `Value::Ref` and the dominant key class is an ARENA object. Extending
it to arena objects (`RAVA_ARENA_KEYMEMO`, keyed on the monotonic identity stamp in the object
header — unique for the process lifetime and carried through evacuation) is the one shape that
carries no correctness risk at all: **a memo returns the same string the renderer would**, so
unlike keying by identity it cannot disagree with any other derivation path.

Wiring it at `key_str_memoed` served 1,968,071 renders at 65.6% hit and moved the timed
derivations by nothing — because the dominant path never goes that way. Moved to
`structural_key_scoped`, the funnel its own doc comment names, it hits 61.6%. Still nothing:

```
worldgen A/B, 8 rounds, floor +/-1.2%:  median -0.96%   (16/16 runs bit-exact)
KEYTIME structural-str: 912,225 x 5,801 ns — unchanged
```

**That is the measurement that locates the cost.** 61.6% of renders are repeats and they are
already nearly free; the bill is the **349,012 MISSES — distinct key objects — at roughly 15 us
each**. Rendering one key runs the guest's own `toString`, and no cache can remove a first render.

⇒ The cure cannot be "render each key once". It has to be "do not render the key at all", which
is the identity rule above — 349k guest `toString` executions replaced by 349k pointer reads.

## Every funnel that must agree

The identity rule is exact but it is a CHOICE of key, so every place that chooses one must make
the same choice. Grepping the `MapKey` constructors finds them:

```
map/keyroot.rs          8   (map_key_inner, value_map_key, value_map_key_in)
arena_map/mod.rs        7
cache.rs                4
natives/cascade_table.rs 1
map/methods/map_method.rs 1
jit/emit/collections.rs  1   <- the JIT EMITS key derivation into machine code
```

The last one is why this is a project rather than a patch: agreement has to hold between the
interpreter, the natives, the arena map and **the code generator**. Fixing only the first two is
what produced `Unbound values in registry ... world_preset`.

## Why this is the biggest number located

Every other axis measured this session came back flat or below the floor: the host profile's top
function is 4.4%, the top allocation site 6.5% of allocations, the top seam callee ~1% of the
boot. This is a single mechanism worth **9.7%**, with a cure that is exact rather than heuristic,
and a blocker that is a refactor rather than a research question.
