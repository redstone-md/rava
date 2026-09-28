# The 5× boot gap is not the code generator

2026-08-28.

## Why this had to be asked

The boot's dominant phase is the 22.9 s before Minecraft prints `Environment:` — 69% of a 33.4 s
boot, against java's 5.9 s. Its guest profile is almost entirely `VoxelShape` and `BlockState`
methods, and those run **compiled**. That made codegen quality the obvious suspect, and it had
never been measured against C2 directly.

## The measurement

```
ArithLoop   rava 121.0 ms   java  64.4 ms   1.88x
ArrReduce   rava  43.0 ms   java  37.1 ms   1.16x
```

**1.16-1.88×, not 5×.** On pure compute this engine's Cranelift output is within a factor of two
of C2. That is good, and it closes the line: the boot gap lives somewhere else.

## What is left, priced

Against a 33.4 s boot, with today's counters:

| cost | population × price | share |
|---|---|---|
| interpreted invocations | 12.4 M × 331 ns | 4.1 s (12%) |
| seam crossings (compiled → host) | 12.6 M × 172 ns | 2.2 s (6.5%) |
| map **key derivation** | 1.5 M × 1.9 µs | 2.8 s (8.5%) |
| everything else | | **~24 s** |

Java does the whole program in 6.8 s. **Zeroing all three named costs still leaves 24 s against
6.8.** The mass is in "everything else".

## And "everything else" is the natives' own work

This engine implements `java.util` in Rust. The dispatch INTO those natives is now cheap — that
was this session's collection-intrinsic work, which took `Map.get` from 1,305-1,555 ns to
420-490 on `bench/SeamCost3.java`. But a JVM's `HashMap.get` is ~15 ns.

**The dispatch is no longer the cost. The native is.**

The largest identified component of that: **this engine derives a map key by rendering a
canonical STRING**, where a JVM calls `hashCode()` and settles ties with `equals()`. Key
derivation alone is 8.5% of the boot before the map operation itself begins, and
`RAVA_KEYTIME`'s branch split shows where it goes:

```
int            27,876   ×     38 ns
identity      585,287   ×     60 ns
identity-str  293,229   ×    114 ns
combo-hash    490,222   ×  2,719 ns
structural    103,386   × 14,413 ns
```

## What this changes about the plan

Three targets are now told apart by measurement rather than by intuition:

1. **Codegen — closed.** 1.16-1.88× off C2 is not where five times lives.
2. **The interpreter's per-invocation frame** — 331 ns against `java -Xint`'s 17.4, on 66% of
   invocations. Worth ~12% and no more, because the population is bounded
   (`docs/coverage-is-not-the-threshold.md`).
3. **What a collection operation costs** — the remaining mass, and the map-key representation is
   its largest named component.

⇒ The next question is not "which path is slow" but "what does a `Map.get` DO for 420 ns", and
the answer starts with a string that should have been a hash.
