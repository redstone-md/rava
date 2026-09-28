# Compiled coverage is not limited by the tiering threshold

2026-08-28. Answered in two runs with the new `exec.compiled` / `exec.interpreted` counters
(`RAVA_DIAG=exec`), a number this engine did not have before.

## The question

The host profile has said for several sessions that ~90% of samples are in the Rust host and
~10% inside compiled guest code, against a JVM that runs this program essentially all-compiled.
That is a *sample* share. Nobody had counted how many guest **invocations** each side serves,
and the two are not the same thing.

## The measurement

Matched conditions — same deny, same 40 s deadline, `RAVA_DIAG=exec,jit`, only the threshold
differing:

```
threshold 16 (default)  compiled 6,404,660  interpreted 12,383,717  = 34.1%   3,944 methods
threshold  1            compiled 5,467,694  interpreted  7,844,912  = 41.1%   4,104 methods
```

⚠**A CORRECTION TO MY FIRST READING OF THIS.** The first pair of runs was taken under
`RAVA_DIAG=all`, which arms the seam census — an expensive instrument — so the boot got less far
in the same window and reported 1,839 compiled methods against this run's 3,944. **Arming
everything perturbs the thing you are measuring**; the matched pair above is the one to quote,
and the earlier "2.2x as many methods" was an artefact of that.

## The conclusion

**Threshold 1 compiles essentially the same SET of methods — 3,944 against 4,104, four percent
more — and moves coverage by seven points anyway.** So the difference is not WHICH methods
compile. It is WHEN: at the default threshold about 4.5 M invocations happen before their
method is compiled, and at threshold 1 they happen after.

And the ceiling is still 41%. Even compiling every method on its first invocation, **59% of
guest invocations build an interpreter frame** — because with ~4,100 methods compiled out of the
~16,000 the boot executes, the remainder is a LONG TAIL of methods invoked a handful of times
each, where compiling costs more than it saves. That is the same shape a JVM sees; the
difference is what an interpreted invocation costs on each engine, measured at **331 ns here
against 17.4 ns on `java -Xint`**.

So this line does not end at "tier harder". It ends where `docs/contiguous-stack-plan.md`
already points: the price of building an interpreter frame, paid 12.4 M times in a 40 s window.

⇒ Two separate levers, and they are now told apart by measurement:
1. **compile SOONER** (worth ~7 points of coverage and ~6% of `Done`) — blocked by the single
   miscompile below, not by the policy;
2. **make an interpreted invocation cheaper** — the 59% that will never be compiled, and the
   larger of the two.

## Two things the sweep found on the way

### Threshold 4 does not boot

200 s, `Done` never reached, twice — while 1 and 16 both work. Non-monotonic in the threshold,
so this is a defect bound to a specific value, not a cost curve. Unexplained; reproducible with
`RAVA_JIT_THRESHOLD=4`.

### Threshold 1 exposes exactly ONE miscompile, and it is pinned

```
RAVA_JIT_THRESHOLD=1                                                  -> dies before Done
RAVA_JIT_THRESHOLD=1 RAVA_JIT_DENY=com/mojang/serialization           -> boots, Done 0.376
RAVA_JIT_THRESHOLD=1 RAVA_JIT_DENY=com/mojang/serialization/MapDecoder#lambda$compressedDecode$1
                                                                      -> boots, wall 37.0, Done 0.400
```

against the default's `Done` 0.424-0.439. One method. Its whole body is five bytecodes:

```
0: aload_0
1: aload_1
2: aload_2
3: invokeinterface decode:(DynamicOps;MapLike;)DataResult
8: areturn
```

Nothing arithmetic can be wrong here. The crash dump says the **receiver** is garbage:

```
[ULE-VIRT] invokevirtual Codec.decode(..) recv=Obj(0x204086f48) hdr=0x2f736e6f69746172
[ULE-JITMETHOD] com/mojang/serialization/MapDecoder.lambda$compressedDecode$1
```

`0x2f736e6f69746172` is ASCII — `"rations/"` — so what the engine read as an object header is
**string bytes**. The reference points into a `String`.

**Narrowed to the Deep-IC path, and then to a RACE.**

```
RAVA_JIT_THRESHOLD=1 RAVA_NO_IC_DEEP=1        -> boots clean, Done 0.428, NO deny needed
RAVA_JIT_THRESHOLD=1 RAVA_NO_CAP_LAMBDA=1     -> still fails
RAVA_JIT_THRESHOLD=1 RAVA_NO_CALLBUF_WIDE=1   -> still fails
RAVA_JIT_THRESHOLD=1 RAVA_NO_WIDE_PIC=1       -> still fails
RAVA_JIT_THRESHOLD=1 RAVA_TRACE_COMPILE=1     -> BOOTS CLEAN, Done 0.557
```

⭐**The last line is the diagnosis.** Adding a print to the compile path — which changes nothing
but timing — makes the fault disappear. So this is not a deterministic codegen error; it is a
race, and the Deep-IC arm is where it lands.

The mechanism the code makes available: **the deep bit is the LOW BIT of the cached fptr**
(`band_imm(fptr, 1)` selects between the leaf arm and the deep arm in
`jit/emit/dispatch_pic.rs`). If a fill publishes a pointer whose deep bit disagrees with the
body actually at that address, the caller runs the wrong protocol — a leaf body entered with the
deep contract, or a deep body entered without it — and the frame buffer is then read with the
wrong layout. That is exactly a garbage receiver, and at threshold 1 there are far more fills
racing against compilation, which is why the window only opens there.

The prologue's own arithmetic was checked and is not at fault for this method: it zeroes
`[args_slots, frame_len)`, and for a body of `aload_0 aload_1 aload_2 invokeinterface areturn`
that is slots 3..6 of a 6-slot frame with 3 argument slots.

Also ruled out on the way, and both are their own findings:
* the arena-inline `new` — `RAVA_NO_ARENA_INLINE=1` kills the boot in 1.2 s with
  `0xc0000005 read of 0xffffffffffffffff`, so it is not a usable probe **and is a separate bug**;
* `RAVA_DIAG=all` perturbs a boot enough to change how many methods compile in a fixed window.

⇒ Next step is not another boot: a 40-second Minecraft start is the wrong reproducer for a race
this narrow. What is needed is a small program that fills an IC with deep and leaf entries under
concurrent compilation — which is a tooling task, and belongs in the roadmap.

⚠**The "−11% wall" that first appeared at threshold 1 is not real** — those runs crashed early,
and a process that dies at 32 s has not booted in 32 s. Once it completes, threshold 1 is
wall-neutral and about 6% better on `Done`.

## Why this document exists

Both answers came from counters that did not exist a day earlier, and both took one boot each.
That is the argument for `docs/rava-tooling-roadmap.md`: the constraint on this engine's
optimisation has been the instruments, not the ideas.
