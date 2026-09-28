# The phantom `this`

A deoptimisation rebuilds an interpreter frame out of a compiled one. To do that it needs the
type of every local at the resume pc, and this tree does not parse `StackMapTable`, so the types
are recovered by walking the bytecode (`jit::analysis::localtypes`).

That walk needs one bit it was never given: **is the method static?** In an instance method local
0 is `this` and the parameters start at slot 1; in a static method they start at slot 0. The
driver did not carry the flag, so both call sites passed `false`.

At the feasibility counter that was harmless, and the comment said so — an instance-shaped guess
only makes the count of provable sites a LOWER bound, which is the safe direction. The production
map, added later, **copied the same `false`**. There it was not a lower bound. It was a wrong
answer.

## What it did

`VegetationFeatures.segmentedBlockPatchBuilder` is
`(Block, int, int, IntegerProperty, EnumProperty)`, private static. Its locals are:

```
[Ref, Int, Int, Ref, Ref, Ref, Int, Ref, Ref]
```

The tags the emitter baked into the deopt site were:

```
[Ref, Ref, Int, Int, Ref, Ref, Int, Ref, Ref]
```

The same list with a phantom `this` pushed in front. Every parameter one slot late. A deopt at
that site handed the interpreter a frame in which an `int` was decoded as a reference and a
reference as an `int`.

**One** deopt at that one site turned all four bit-exact gates red.

## Why it hid

An all-reference frame survives the shift unchanged — references shift onto references. Nearly
every frame in this workload is all-reference:

- 6,651,357 forced deopts through `guarded_inline` passed the gates and 21 differential probes.
- Of the six sites that deopted in a `RingScan` run, five were all-reference. The sixth was this
  one, and it fired **once**.
- Refusing any site with a primitive in its frame (`RAVA_DEOPT_REFONLY`) went green immediately.

So the bug needed a static method, with a primitive parameter, reached through a guard failure,
in a method the JIT compiled. A 6.65 M-event test did not contain one; a single call did.

## How it was found

By elimination, and only after three wrong hypotheses had each been killed by a measurement:

1. **Frame layout** — `build_body` creates locals as `(0..max_locals)`, so `vars.len()` is exactly
   `code.max_locals`. Logged both: they matched.
2. **Reference returns** — real, fixed (`box_ref` vs `pack_local`), gates stayed red.
3. **The arena bump cursor** — plausible, and wrong: `tlab_try_alloc` already syncs the cursor
   before every Rust-path allocation, so the interpreter never sees a stale young top.

What actually found it was refusing to keep guessing: a per-arm bitmask (`RAVA_ITER_TRAP`) named
two failing arms out of four, a per-class filter (`RAVA_TRAP_ONLY`) narrowed one arm to a single
class, and a log of the site's shape put the tags next to `javap` output. The tags were the truth
shifted by one, and the shift named its own cause.

## The fix

`Code` is stored inline in its `MemberInfo`, so the owning method — and its `ACC_STATIC` bit — is
found by **pointer identity** against the class's method list. Exact, and it needs no new
parameter threaded through eleven call sites. When the class is absent (unit tests pass
`class_ptr = 0`) or the code does not belong to it, there is no map and therefore no deopt site:
a frame is never described on a guess.

## What to take from it

- **A guess documented as safe in one place is not safe in the next place it is copied to.** The
  comment explaining why `false` was fine was still there, three lines above a use where it was
  not fine.
- **A big population is not a strong test if the population is uniform.** 6.65 M events all had
  the shape that hides this. One event with the other shape found it.
- **Bisect the emission, not the theory.** Three hypotheses cost a day; a bitmask and a name
  filter cost an hour and pointed at the line.

## What the fixed machinery is worth, measured

Correct is not the same as profitable. With the defect fixed, three numbers decided what ships.

**The boot cannot judge any of it.** A null test against `RAVA_NULL_TEST_NOT_A_FLAG` — a flag no
code reads — booted four times and reported **−8.20% CPU, DISJOINT**. Same box, same hour, same
binary, both arms identical. That reproduces the earlier finding exactly: the boot's floor is
~8% and `disjoint` is not protection.

So the measurement moved to the gate workload (`bench/gate_flag_cpu_ab.ps1`): same binary, one
env flag toggled, arms alternating ABBA, first pair discarded, min AND median. Its own null test
reads **+0.60% min / −0.27% median, not disjoint** — a floor 13× tighter than the boot's, on a
workload that runs in 30 seconds instead of 95.

On that instrument:

| change | min | median | verdict |
|---|---|---|---|
| null flag (control) | +0.60% | −0.27% | the floor |
| `RAVA_DEOPT=1` (guarded_inline traps) | +0.22% | −1.12% | neutral, arms overlap |
| `RAVA_ITER_TRAP=3` (hasNext + next as traps) | **+7.66%** | **+8.22%** | **regression, disjoint** |

## Why trapping those arms loses, when their guards almost never miss

The guards are excellent: `hasNext` passes 3,912,747 of 3,916,171 and `next` 3,159,908 of
3,162,660 — a **0.09%** miss rate, which is exactly the population a trap is designed for. And
the trap still costs 8%.

The cost is not the transition, it is the **invalidation**. A deopt drops its method back to
`Cold`, so a method in a hot iterator loop goes back to being INTERPRETED until it re-warms — and
then deopts again. 671 extra deopts bought roughly 2.3 seconds, about 3.4 ms each, which is not
the price of a frame rebuild; it is the price of un-compiling something hot.

And the invalidation never converges, for two reasons this measurement made visible:

- **59 sites, 67,032 deopts, 67,032 invalidations, and `sites` never grows.** The method is not
  being recompiled at all. Compiled callers reach it through `jit_direct_cache`, whose
  `DirectEntry.fptr` is the callee's baked native entry point — it never consults `jit_cache`, so
  dropping the cache entry to `Cold` changes nothing for them.
- **The arms are emitted by NAME**, not from a receiver profile, so even a recompile that did
  happen would emit the same arm again. `sitemono::poison` cannot reach them.

`getDouble` is worse still and for a plainer reason: 8,359,524 calls reach its guard and only
2,763,255 pass — a **67% miss rate**. Under a diamond each miss is one native call; under a trap
each is a frame rebuild plus an invalidation. `size` behaves the same way and pushed a gate that
normally finishes in seconds past its 180-second timeout.

## Conclusion

The arms stay diamonds. `RAVA_ITER_TRAP` stays as the per-arm bisection tool it became.

Deoptimisation itself is now **correct** — that is the deliverable here — and it measures
**neutral**, which is the honest result for a mechanism whose one real user cannot yet remove a
failed speculation. Making it pay needs invalidation that actually invalidates: every entry to a
compiled method would have to go through something repointable, instead of a pointer baked into
each caller. That is a change to how compiled code calls compiled code, not a tuning knob.

---

# Postscript: what the string and IO fixes were worth

The session's later work was five mechanism-level defects, all found by splitting a phase into
layers rather than by profiling the whole boot. They have no common flag, so they cannot be
measured with the same-binary flag A/B this tree prefers. The next-best instrument is an
INTERLEAVED TWO-BINARY comparison on a deterministic, hash-arbitrated probe: same workload, arms
alternating so the box's hour-scale drift cancels, first pair discarded, min AND median reported.

`bench/DataPackJson`, `PARSE_MS`, HEAD against the commit before the first fix:

```
pair 0 (discarded)  base 4915   head 3840
pair 1              base 6501   head 4728
pair 2              base 5034   head 3922
pair 3              base 4574   head 4168
pair 4              base 4812   head 3740

base min 4574 med 5034     head min 3740 med 4168
delta    min -18.23%           median -17.20%
```

**Head is faster in all five pairs**, by 1075 / 1773 / 1112 / 406 / 1072 ms, and
`DataPack_HASH=14e8ee5a` is identical in all ten runs.

⚠The arms are not strictly disjoint — one head run (4728) is slower than one base run (4574) —
and a two-binary A/B cannot exclude code-layout effects the way a flag A/B can. The pairwise
agreement is what carries this, not the aggregate.

The five defects behind it, each a case of doing per element what should be done per buffer:

| defect | what it did |
|---|---|
| `String.charAt` | allocated and copied the WHOLE string, per character — guest loops were O(n²) |
| `Reader.read(char[])` | one `stream_read_one` per byte: four hashed or boxed ops to move one byte |
| `StringBuilder.append(char[],off,len)` | boxed the whole 8192-char buffer to keep 780 of it |
| `FieldBlob::push` | grew the blob by exactly one slot — N allocations for N fields |
| `GC seen_arena` | SipHash over raw pointers in the mark loop |

Two of the five had a SIBLING already fixed for the same reason months earlier (`value_str_len`
for `charAt`, `data_read_bytes` for `read(char[])`). **When a per-call copy is fixed, the other
accessors of the same object should be fixed the same day.**


---

# Session total, measured end to end

Interleaved comparison of HEAD against the commit the session started from
(`70d056b5`), `bench/DataPackJson`, six pairs, arms alternating so the box's
hour-scale drift cancels:

```
pair 0  base parse 4102  cpu 29.6  |  head parse 3653  cpu 22.5
pair 1  base parse 4113  cpu 29.5  |  head parse 3250  cpu 22.1
pair 2  base parse 4094  cpu 30.0  |  head parse 3434  cpu 22.3
pair 3  base parse 4096  cpu 29.9  |  head parse 3330  cpu 21.8
pair 4  base parse 4073  cpu 29.8  |  head parse 3471  cpu 22.2
pair 5  base parse 4151  cpu 29.1  |  head parse 3340  cpu 22.0

PARSE_MS   base min 4073 med 4096   head min 3250 med 3340   -20.21% / -18.46%
PROCESS    base min 29.1 med 29.8   head min 21.8 med 22.2   -25.09% / -25.50%
```

**Head is faster in all six pairs on both metrics, and the process-CPU arms are
FULLY DISJOINT** — the slowest head run (22.5 s) beats the fastest base run
(29.1 s) by 6.6 s. `DataPack_HASH = 14e8ee5a` in all twelve runs.

⚠A two-binary comparison cannot exclude code-layout effects the way a flag A/B
can. At this margin that caveat does not change the conclusion, but it is the
reason the individual changes above were each measured with a same-binary flag
A/B instead.

## What produced it

Nine mechanism-level defects, every one found by an instrument rather than by
reading code, and every one either measured or reverted:

| defect | what it did |
|---|---|
| `String.charAt` | allocated and copied the WHOLE string, per character — guest loops were O(n²) |
| `Reader.read(char[])` | one `stream_read_one` per byte: four hashed or boxed ops to move one byte |
| `StringBuilder.append(char[],off,len)` | boxed an 8192-char buffer to keep 780 of it |
| `StringBuilder.length`/`charAt` | rebuilt the whole buffer to read one element |
| `FieldBlob::push` | grew the blob one slot at a time — N allocations for N fields |
| field blob sizing | asked for exact bytes, wasting the allocator's rounding |
| GC `seen_arena` | SipHash over raw pointers in the mark loop |
| `RAVA_RECORD_LAYOUT` | read the ENVIRONMENT on every map-key render — 4% of all allocations |
| the `getDouble`/`size` arms | read the iterator's `$elems` on a list, and waited for a class word they did not need |

Two of those had a sibling already fixed for the same reason months earlier, and
three were the same "not ready ⇒ not emitted" bootstrap in one file.

Reverted for measuring nothing: a constructor-resolution memo, and a mirror
inline shape whose counter read zero.


## ⛔The boot-level number, and the 39x I nearly reported

Measuring the product boot against `70d056b5` — the commit this conversation opened on — gave a
spectacular result, reproduced five times across two configurations:

```
base  wall 55.9 55.7 54.7 55.4 55.0   cpu ~102   Done 16.06 16.37 15.88 16.01 15.72
head  wall 34.9 34.1 35.6 31.9 32.0   cpu ~35    Done  0.41  0.39  0.45  0.40  0.41
```

Wall -40%, CPU -66%, `Done` **39x**. Every round agreed, far outside the boot's ±8% floor.

**It is an artifact of choosing that baseline, and bisection is what caught it.**

```
70d056b5 (session-open commit)   wall 55.4   Done 16.007
9d7b0438 (first session commit)  wall 33.3   Done  0.434
1201153a (before the string work) wall 32.9  Done  0.408
HEAD                             wall 32.3   Done  0.422
```

The entire jump sits between `70d056b5` and `9d7b0438` — which is not a change I made during the
session, it is the **uncommitted working tree that already existed when the session opened** and
which `9d7b0438` merely committed. `70d056b5` as a pure commit is a state that was never actually
run; the session's own opening measurement was `Done ≈ 0.41`, which matches `9d7b0438`, not
`70d056b5`.

⇒ **Against the correct baseline the session's boot-level effect is wall 33.3 → 32.3 and CPU
35.2 → 34.3, about 2-3%** — inside the boot's ±8% floor and therefore NOT resolvable there. That
is entirely consistent with the -25.5% disjoint on `bench/DataPackJson`: that probe is dominated
by the phases this session repaired, and a full boot is not.

⭐**The rule: a baseline must be a state that actually ran.** A commit is not automatically that,
and "the tree had uncommitted changes when I started" is exactly the case where it is not.
