# The engine's price map, measured where the work happens

2026-08-28.

## What was wrong with the old map

`docs/per-op-price-list.md` carries six lines that between them are supposed to account for a
boot. Every price in it came from `bench/NativeCost.java` — one operation, isolated, in a loop.
That document already corrects itself twice for exactly this reason (a combined run inflated
every case ~2x; `bridgestat` biases a ranking by its own per-call accounting), and the pattern
holds a third time: an isolated price and the same site's real cost are different numbers.

The `diag` registry now has a METER (`crate::diag::meter`) that prices a site in place. Below is
the map re-taken with it, on a 33-second product boot, `RAVA_DIAG=price`.

## The measurement

```text
births_array_small   6,934,331 ev  27,088 smp   p50   20.8   trim   22.3   mean   29.9 ns   0.155 s = 0.47%
births_array_big           917 ev       4 smp   p50  166.7   trim   73.2   mean   97.2 ns   0.000 s = 0.00%
births_arena         3,557,950 ev  13,899 smp   p50   41.7   trim   39.1   mean   55.6 ns   0.139 s = 0.42%
births_handle        3,351,536 ev  13,092 smp   p50  115.4   trim  116.8   mean  130.9 ns   0.392 s = 1.19%
mapkey_str             155,413 ev     608 smp   p50  115.4   trim  125.8   mean  138.7 ns   0.020 s = 0.06%
```

| line | the map's estimate | measured in situ |
|---|---|---|
| array births | 8.74 M x ~150 ns = **1.3 s** | 6.93 M x 22.3 ns = **0.155 s** |
| object births, arena | (of a 0.9 s pair) | 3.56 M x 39.1 ns = **0.139 s** |
| object births, handle | (of the same pair) | 3.35 M x 116.8 ns = **0.392 s** |
| string map keys | ~150 ns each | 155 k x 125.8 ns = **0.020 s** |

**Every birth in the engine is 0.69 s of a 33 s boot — 2.1%, against the map's 2.2 s.** The
isolated prices overstated the birth lines by 3.2x.

## The one asymmetry that survives, with a number on it

**A handle-heap birth costs 3.0x an arena birth: 116.8 ns against 39.1.** That is the first
direct in-situ measurement of the difference between this engine's two object representations,
and it supports the standing verdict that the handle heap is the more expensive one.

⛔It also prices that verdict's birth component: **retiring `Value::Ref` entirely saves 1.19% of
the boot at the allocation site.** Whatever else the handle heap costs — and it costs more
elsewhere, in every field access, GC root scan and native crossing that must branch on which
representation it has — the births are not where the case for removing it lives.

## Three things the meter got wrong before it got this right

Each is recorded at its own source, because each is a way to mis-measure that will recur.

1. **The mean.** First reading of a known site: 242.8 ns against a six-boot CPU A/B's -0.28%.
   `rdtsc` counts wall cycles, so a sample landing on a preemption adds microseconds. Fixed with
   log2 buckets and a median.
2. **The window.** Still 6x off, and the time series said why: 651,157 events at 36 s and
   5,587,944 at 45 s, on a boot that finishes near 33. **86% of that population was server
   ticking.** A capped-window census and a wall A/B taken to `Done` are not measuring the same
   program — and the phase series showed the tick runs 4x denser in crossings than the boot does.
3. ⭐**Its own nested instrumentation.** Two meters wrapping nearly the same region disagreed
   **13x** — 187.5 ns for `put_array` as a whole against 14.4 ns for the same function minus the
   diagnostics inside it. The outer meter was timing the inner meter's atomics and an armed
   histogram's contended cells. Removing them, the honest answer is 22.3 ns.

   That third one had already produced a wrong conclusion — *"array births are 3.94% of the boot,
   the largest line on the re-priced map"* — which survived exactly one run before its own
   instrument refuted it. **A meter measures everything inside it, including other meters.**

## ⭐The seam, priced without its callee — and it is the biggest line in the engine

The map's largest entry, *crossings 8.5 M x ~300 ns = 2.6 s*, could not be metered directly: a
guard around `jit_call_body` would time the guest method it invokes. So the crossing was metered
in its halves, with the callee excluded by construction — the prologue guard is dropped
explicitly on the line before the dispatch, and a second opens after `frames::pop()`.

```text
seam_prologue        21,332,401 ev   p50 83.3   trim  95.3   mean 217.1 ns   2.033 s = 6.16%
seam_prologue_full    8,702,063 ev   p50 83.3   trim  88.2   mean 101.5 ns   0.768 s = 2.33%
seam_epilogue         8,702,051 ev   p50 35.3   trim  33.0   mean  44.7 ns   0.287 s = 0.87%
```

**The population is 21.3 M, not 8.5 M.** The recorded figure counts crossings that reach the
general dispatch — which is exactly what the epilogue counts, 8.70 M, and the agreement between
two independently sampled meters is the check. The other **12.6 M return through an early service
arm, and every one of them still pays whatever runs before the arm can decide.**

Subtracting the general path's own work from the total splits the seam three ways:

⛔**AND THE FIRST VERSION OF THAT SPLIT WAS WRONG, BY THE TRAP THIS DOCUMENT IS ABOUT.** It read
"the decision costs 21.3 M x 59 ns = 3.8% of the boot", and `jit_direct_call` — the fast arm
those 12.6 M early returns go through — does not merely decide: **it RUNS the callee when it
hits.** A guard alive across it charges the whole guest method to the crossing. Dropped on the
line before the direct-call attempt instead, the same meter reads:

```text
seam_prologue        21,328,683 ev   p50  4.4   trim  7.7   mean   8.8 ns   0.164 s = 0.50%
seam_prologue_full    8,698,728 ev   p50 96.2   trim 92.9   mean 101.3 ns   0.808 s = 2.45%
seam_epilogue         8,698,715 ev   p50 35.3   trim 35.1   mean  38.5 ns   0.306 s = 0.93%
```

| part | population | price | share of boot |
|---|---|---|---|
| the decision — every gate before a service level is chosen | 21.3 M | **7.7 ns** | **0.164 s = 0.50%** |
| the general path: Arc round-trip, `member_ref`, arg unpack, synthetic frame | 8.70 M | 92.9 ns | **0.808 s = 2.45%** |
| the epilogue: return packing, vec give-back | 8.70 M | 35.1 ns | **0.306 s = 0.93%** |

**The seam's own overhead is 1.28 s = 3.9% of the boot, not 7.0%**, and its largest half is
building the synthetic frame on the general path. The gate cascade that chooses a service level —
the thing five sessions of work have added to — costs 7.7 ns and 0.50%.

⭐Every wrong number in this document had the same shape: **the meter was measuring something
that runs guest code inside it.** Three times now — `bridgestat`'s self column, `put_array`'s
nested instrument, and this. The rule is not "beware subtrees", it is: **before believing a
meter, name what can execute inside its guard.**

⚠`seam_prologue_full` is nested inside `seam_prologue`, which is the trap that ruined the array
reading. Here it cost almost nothing: the outer meter read 98.5 ns before the inner one existed
and 95.3 after. That difference is the nesting overhead, measured rather than assumed — and it is
why both numbers are quoted.

## The rule this leaves

The tree's standing rule is *convert a population into a share of the boot before writing code*.
Three qualifiers now attach to it, all of them learned by being wrong:

* price it **in situ**, because an isolated bench and a real site differ by 3x either way;
* price it **in the phase you care about**, because boot and tick differ by 4x;
* price it with **nothing else armed inside the region**, because an instrument inside a
  measurement is part of the measurement.

## ⚠And a fourth: the baseline moved under all of it

After the map was taken, a plain product boot read **wall 42.9 / cpu 44.1** against the 33.2-33.7
/ 35.1-35.6 the same script produced hours earlier — a 29% gap, far outside the recorded floor,
and the meters sit on paths taken ~14 M times a boot. So they were the obvious suspect.

Two binaries were built, identical except that one has no meters on the allocation paths, and run
interleaved. Every run of BOTH landed in **36.2-38.3 s wall / 38.2-40.0 s CPU**. The morning's
33.2 does not reproduce on either binary.

⇒ The box drifted, exactly as `docs/null-test-2026-08-28.md` records (that null test read -7.99%
between identical arms the same day). The meters are indistinguishable from their absence.

⚠The A/B's own labels were partly corrupt — four of six binary swaps failed because the exe was
still locked by the previous run, and the script reported the arm it *intended*, not the one that
ran. What survives is the label-independent fact: **both binaries produce the same band, and
neither reproduces the earlier number.** A harness that names an arm it did not actually install
is worth writing down, because it fails silently and reads as a result.

## Where the other 94% is: self-time meters, and the natives decomposed

The map above accounts for ~6% of a boot. The rest was never in any population because the two
things that hold it — the Rust natives and the interpreter — could not be metered while a meter
timed everything inside its guard.

**So the meters were made to nest.** Each guard subtracts the time its metered children consumed
and reports SELF time; a forced timing (a child inside a sampled parent that is not its own
sample) is used for subtraction only and never recorded as the child's sample, so no meter's
selection is biased by its parent's. Meter the callee as well as the caller and the caller's
number becomes honest automatically — the difference between a stopwatch and a profiler.

With `natives::dispatch_owner_cascade` and `opcodes::run` metered, a 33-second boot:

```text
interp_self     2,899,336 ev   trim 1036.4 ns   3.005 s =  9.11%
native (total, decomposed below)                         ~14.2%
seam (prologue + general path + epilogue)                  3.37%
births (arrays + arena + handle)                           2.08%
```

And the native cascade, decomposed — every row self time, so they sum rather than overlap:

| region | events | self | share |
|---|---|---|---|
| the inline arm tail (`owner` compared against string literals, one after another) | 6.12 M | 152.6 ns | **2.83%** |
| the service stages: nio, net, reflect + the stage machinery | 5.18 M | 173.2 ns | **2.72%** |
| the stream / collections stage | 5.13 M | 212.0 ns | 3.29% |
| the cascade head | 7.64 M | 99.1 ns | 2.30% |
| the last-arm shortcut | 6.12 M | 96.6 ns | 1.79% |
| the map stage | 3.23 M | 108.8 ns | 1.07% |
| finding the cached arm — **hashing the owner and method strings** | 6.12 M | 10.0 ns | **0.19%** |

**≈29% of the boot is now attributed to named populations with measured prices**, against a host
profile whose top function is 3.4%.

Two candidates die here. The string-keyed arm cache — one of the three mechanisms the standing
verdict names as a root cause — is **0.19%**. And the shortcut that five sessions have been
adding arms to is 1.79%, while the gauntlet it skips is 5.55%.

⛔And the flatness goes all the way down: the largest piece of the largest structure is 3.29%,
and nothing in the decomposition passes today's ±8% measurement floor on its own. The natives
cost 14% collectively because there are seven of them at two to three percent, which is the same
shape every other axis in this engine has shown.

⚠**A third scoping error was caught in this stretch, of a new kind.** The shortcut's meter was
bound without a scope, so the guard lived to the end of the enclosing function and reported the
whole remaining cascade under the shortcut's name — 403 ns instead of 96.6. Not the callee trap
(code it called) but its sibling: code that merely FOLLOWED it. Both now have the same cure
written at the sites: **name what runs inside the guard, and give the guard exactly that scope.**

## The largest population in the engine had never been counted

`vmstate` is the biggest module in the host profile at 25.8%, and the decomposition above
reached only 3.4% of it — because the fast paths holding the rest RUN THE CALLEE and were
excluded by hand. Metering the two halves that do not (the direct-call *probe*, and
`fu_fastpath`, which is a leaf):

```text
fu_fast        38,413,533 ev   p50 70.5   trim 69.7   mean 108.9 ns   2.679 s = 8.12%
direct_probe   21,350,585 ev   p50 24.0   trim 26.3   mean  31.5 ns   0.561 s = 1.70%
fu_unbox       (removed)                  trim  5.3 ns                          0.62%
```

**38.4 million calls a boot — four and a half times the crossing count, and nobody had ever
counted them.** `fu_fastpath` is the cheapest service level in the engine, a native answered
from machine code with no synthetic frame and no cascade, and it is **8.12% of the wall**, the
largest single line measured anywhere in this document.

It is not a tax. **37.1 M served against 1.29 M declined — a 96.7% hit rate**, so the admission
policy that five sessions of work went into is right. The 8.12% is work, not waste.

And what the work is, is flat like everything else:

```text
size     4,439,370      set/remove   1,582,448      add        471,367
hasNext  4,019,045      getKey/Val     896,875      equals     371,041
next     3,230,380      cast/idHash    870,839      optional   297,280
get      2,862,379      box unwrap     493,861      isEmpty     57,414
                                                    (unnamed) 18,942,931
```

**No operation is above 12% of the population.** The top one, `size`, is 4.4 M calls — about
0.9% of the boot. Half the population still has no name: those calls are served by the map and
collection blocks, which match on a PREDICATE (`fu_map_op`) rather than on string literals, so
they never appear in a literal census. Naming them is the next measurement.

⚠**And the instrument found its own limit here.** A second guard nested inside this one, around
the unbox alone, priced it at 5.3 ns — and raised the OUTER meter's self time from 63.6 to
102.9. A meter's own bookkeeping (a contended `fetch_add`, two thread-locals) runs inside its
parent's span while its own measurement excludes it, so on a 38 M-event path a nested meter is
~44 ns of pure instrument. **The outermost meter of a nest is the trustworthy one, and every
child added inflates it.** Recorded at the site; the nested meter was removed.
