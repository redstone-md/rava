# The rewrite's premise has never been executed

2026-08-29. Written on being told to take on the seam.

## The standing verdict, and its precondition

The engine's structural verdict has been: the profile is flat to the bottom, so what remains is a
point rewrite — **remove the handle heap**. Its recorded precondition was an arena representation
of collections, and the map half of that migration is **done**: `RAVA_ARENA_MAPS` is correct
(4 gates, 21 probes) and measures at **parity**.

Parity, not a win — and `docs/arena-maps-are-at-parity.md` explains why that was expected:

> Goal: erase the FFI/callback boundary **so a later step can emit the probe loop straight into
> Cranelift IR against arena memory.**

The representation was never supposed to pay by itself. The payoff was the next step.

## That step exists in the tree, and has never run

`jit::emit::collections::emit_amap_int_probe` is exactly it: the open-addressing probe emitted
into machine code, hashing in IR, loading capacity and table out of arena memory, guarded by the
`ArenaMap` class id. It has been in the tree for some time.

Counters on it, product boot, `RAVA_ARENA_MAPS=1`:

```text
am_sites   0     sites the probe was EMITTED at
am_arena   0     times the arena guard was reached
am_fast    0     times the probe ran
```

**Never emitted. Not once, in a whole boot.**

The call site requires a raw unboxed `JT::Int` key on the operand stack at a
`HashMap.get(Object)` site — i.e. the `Integer.valueOf` + `get` fusion must already have left the
key unboxed. On this workload that never happens.

## What that does to the verdict

**The premise of the rewrite is untested, not proven.** The chain was: arena representation →
inline collection operations → the seam disappears. The first link is built and at parity. The
second link has never executed. So the recorded parity says nothing about whether the chain pays,
and the case for building the list half — 81 `$elems` sites, 33 operations — rests on a step
nobody has ever seen run.

And widening the probe does not obviously help: the receiver census taken the same day says the
maps that actually dominate this boot are not `java/util/HashMap` with int keys at all —

```text
Reference2ObjectArrayMap#put   594,176
LinkedHashMap#get              452,096
Map$Entry#getKey/getValue      861,440
```

— so the one inline map path the tree owns is aimed at a population this workload does not have.

## What to do instead of building the list half

1. **Make the payoff step run on something.** Either widen the probe to a key shape that occurs
   (boxed Integer, or the `Reference2ObjectArrayMap` shape), or accept that the int-key probe is
   aimed at the wrong map family.
2. **Then measure it.** If arena representation plus a genuinely-executing inline probe still
   reads parity, the rewrite is refuted and eighty-one sites do not get written.
3. Only if it pays does the list half become justified.

⭐This is the third dead mechanism found in one day by the same instrument — a counter on each
guard step. The other two were `getDouble` (emitted 31 sites, guard never hit) and
`ArrayList.get` (alive but serving one percent of its population). **Bit-exact gates cannot see
any of them.** A mechanism that is correct, committed, and never executed looks exactly like a
mechanism that works, and this tree has at least three.

## And the ceiling on the whole rewrite, measured rather than argued

Taking the seam on means asking what removing it is worth. That was measured directly the same
day, with the callee excluded by construction (the guard is dropped on the line before the
dispatch, and a second one opens after `frames::pop()`):

| part of the seam | population | price | share of boot |
|---|---|---|---|
| the decision — every admission arm | 21.3 M | 7.7 ns | 0.50% |
| the general path — Arc, `member_ref`, arg unpack, synthetic frame | 8.70 M | 92.9 ns | 2.45% |
| the epilogue — return packing, vec give-back | 8.70 M | 35.1 ns | 0.93% |
| **the whole seam** | | | **3.9%** |

**A perfect rewrite of the seam is worth 3.9% of a boot that is 5.2x slower than a JVM.** It
cannot close the gap, and it is bounded by measurement, not by opinion.

A second, independent check the same day points the same way. Four inline arms removed
**10,942,759 collection calls a boot** — hasNext, next, `CubePointRange#getDouble` and `#size` —
answering them in machine code instead of crossing to a native. Six alternated boots:
**+0.81% CPU, arms overlapping.** Ten point nine million removed calls did not move the boot.

⚠Those were the CHEAPEST calls in the population (a field read and a compare), so that experiment
does not price the expensive crossings — but it does show that removing collection call overhead
at scale is not where a 5x gap lives.

## So what the seam work should be, and what it should not

**Should not:** build the list half of the collections migration (81 `$elems` sites, 33
operations, months) on the strength of a payoff step that has never executed and a ceiling of
3.9%.

**Should:** find where the other 71% is. Named populations with measured prices account for about
29% of a boot — natives 14%, interpretation 9%, the seam 3.9%, births 2.1%. The module profile
puts the rest in places no meter has been pointed at yet: `mimalloc` 8.2%, Cranelift itself 7.4%,
`tlcache` 4.3%, class loading and parsing 3.0%.

⭐**Cranelift at 7.4% of host samples is compilation, not guest work at all** — nearly twice the
whole seam. That is the kind of thing worth pricing before rewriting an object representation.

## The first unnamed item, priced

```text
compile_self   1,467 compiles x ~982 us trimmed = 1.441 s = 4.37% of the boot
```

**The compiler costs more than the entire seam**, and unlike the seam it is not guest work at all
— it is the price of having a JIT. The number matches the tree's own older `RAVA_COMPILE_TIME`
reading (~4.4%), so two independent instruments agree.

⚠It is not wall. The experiment was run and the answer is clean:

```text
compile_worker  1,436        compile_main  0
```

**Every compile runs off the main thread.** Compilation is fully background here and the tree's
recorded claim that it overlaps execution is correct.

⛔And the first version of this experiment said the opposite — "all 1,504 compiles run on the
thread the boot waits for" — because it asked `gil::is_worker()`, which names GIL-FREE WORKERS.
The background compiler is neither a worker nor the main thread, so every background compile was
counted as main and a fully-overlapped cost looked like 4.4% of the critical path. The corrected
test compares `std::thread::current().id()` against the id stamped on the VM thread at startup.

⭐Same shape as three other errors this session: a predicate that answers a NEARBY question is
worse than no predicate, because its answer looks valid. `is_worker` vs `is main` cost one wrong
conclusion and one commit.

## Where the accounting stands after a day of pricing

| line | share of boot | how |
|---|---|---|
| natives (7 pieces, none above 3.3%) | 14% | self-time meters |
| interpretation | 9.1% | 2.9 M frames x 1036 ns |
| **the compiler itself** | **4.4%** | 1,467 compiles |
| the seam (its whole ceiling) | 3.9% | prologue + general path + epilogue |
| births (objects, arrays, handles) | 2.1% | 13.8 M births |
| **named total** | **~33%** | |

Two thirds of a boot that is 5.2x slower than a JVM is still unnamed, and the module profile says
where to point the next meters: `heap` 12.7%, `mimalloc` 8.2%, `tlcache` 4.3%, class loading and
parsing 3.0%.

⇒ **That is the case against starting the rewrite now.** Its ceiling is 3.9%, its premise has
never executed, and two thirds of the boot has never been priced. Pricing the rest is days; the
rewrite is months and cannot close a 5x gap on its own.

## The second unnamed item: field access

```text
field_read   25,864,564 events x 30.7 ns = 0.794 s = 2.41% of the boot
scan length  2^1: 11.4 M   2^2: 12.5 M   (objects are small; the walk is short)
```

**Twenty-six million field reads a boot** — a larger population than the entire seam — each
walking the object's id array to find the slot. A JVM computes that offset once at class-load and
reads at a constant displacement, in about a nanosecond.

The histogram says the cost is not the walk's LENGTH (two to four ids), it is that the offset is
re-derived per access. The tree already has the cure in one place: `InlineCallee.fields` carries
`Option<u32>` layout slots, so constant-slot field access exists for inlined JIT bodies and not
for the interpreter or the natives.

⇒ ~2.3% is available here, mechanically, without touching a representation.

## The complete accounting, and what it says

| line | share | population |
|---|---|---|
| natives (7 pieces, none above 3.3%) | 14% | 7.6 M calls |
| interpretation | 9.1% | 2.9 M frames |
| the compiler itself | 4.4% | 1,467 compiles |
| the seam, whole | 3.9% | 21.3 M crossings |
| **field reads** | **2.41%** | **25.9 M** |
| births | 2.1% | 13.8 M |
| **named** | **~36%** | |

**Nothing is above 14%, and that 14% is itself seven pieces of one to three percent.** The engine
is 5.2x a JVM by the sum of roughly twenty such lines, which is why every single lever measured
this session landed on the floor — and why the rewrite, at a 3.9% ceiling, cannot be the answer
either.

⇒ The honest strategic reading: there is no lever. There is a list, and it is long. The work that
moves this engine is the boring one — take the list in order of measured share, and expect each
item to be invisible on its own and visible only in aggregate.
