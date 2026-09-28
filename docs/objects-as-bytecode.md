# `java.util.Objects` as guest bytecode — measured, and it costs 15%

## Why this was the most promising remaining idea

The JIT-callback census over a boot:

```
9,585,402 sampled callbacks | 350 distinct callees | interface=49%
  7.80%  java/lang/Class.cast
  7.20%  java/util/Map.get
  5.04%  java/lang/Boolean.booleanValue
  4.97%  it/unimi/.../ObjectSet.iterator
  4.90%  it/unimi/.../Reference2ObjectMap.reference2ObjectEntrySet
  4.16%  java/util/function/Function.apply
  4.04%  it/unimi/.../DoubleArrayList.wrap
  3.84%  java/lang/String.valueOf
  3.69%  java/util/Objects.equals
```

**The first strongly concentrated distribution found anywhere in this engine** — top-20 hold ~70%
of 64,088,346 compiled-to-host crossings, against a host profile whose top *function* is 4.4%.

`Objects` looked like the ideal first target: the native layer's own comment prices
`Objects.equals` at "17.3% of all native bridge self-time", and unlike the collection classes
NOTHING in `Objects` touches `$elems`/`$map` or the handle heap — which is exactly what made the
`java.util` collections experiment cost +8.46%. It also promised to be **more correct**: the
native cannot dispatch to the guest's own `equals` (re-entering the interpreter from a native
reached through `rava_dispatch_miss` corrupted the operand-cell contract), so it decides what it
can from content keys instead.

## What was built and verified

`jdk/java/util/Objects.java`, complete, compiled with `javac --patch-module java.base=jdk`.
Thirteen-case differential probe (record equality, null pairs, `hash`, `toString(o,default)`,
`requireNonNull` message wording, `checkIndex` wording, `deepEquals` over primitive and object
arrays): **every line byte-identical to `java`**, and all four bit-exact gates green with the
class ahead of the classpath.

⚠One defect found before it ever ran on a classpath: the first `deepEquals` handled only
`Object[]` and fell through to `a.equals(b)` for a primitive array, so
`deepEquals(new int[]{1,2}, new int[]{1,2})` answered `false` where HotSpot answers `true`. Array
`equals` is identity. Diffing against `java` caught it.

## The measurement

`bench/flag_ab_chunk.ps1 -CpPrefix jdk-objects`, 8 rounds, worldgen, floor ±1.2%:

```
CPU-s   OFF (native)  min 32.69  median 33.56
        ON  (guest)   min 37.22  median 38.585
        delta_median  +14.97%
```

All 16 runs hashed `8790f04ace726ecf`. Correct, and decisively slower.

## Why — and what it closes

The native `Objects.equals` is fast **because it cheats in the right direction**: it answers from
a derived content key without re-entering the interpreter. The honest bytecode version does what
the JDK does — `(a == b) || (a != null && a.equals(b))` — and that `invokevirtual equals` is a
SECOND crossing where the native had one. A guest virtual call in this engine costs more than the
native shortcut it replaces.

⇒ **The "serve `java.*` from guest bytecode" line is closed, and not only for collections.**
`java.util` failed at +8.46% and that was attributed to handle-heap births; `Objects` has no
handle-heap coupling at all and fails harder, at +14.97%. The common cause is neither the heap
nor the class — it is that **guest execution is more expensive than the native it would replace**,
so moving work across the seam toward the guest loses regardless of what the work is.

The seam is still 11.3% of the boot and its callee distribution is still concentrated. But the
cure cannot be "make the callee guest code". It has to be "make the crossing cheap" — a typed
direct path for native callees, the one thing the direct-call cache (16.25M hits/boot) does not
serve today, against 29,433,838 `dmiss_res` crossings that fall to the general path and rebuild a
synthetic frame to re-enter the interpreter's `invokevirtual`.

The class stays in the tree, opt-in and off every default classpath, because it is the correct
implementation and the measurement is worth being able to repeat.
