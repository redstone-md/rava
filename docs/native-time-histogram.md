# Timing natives inside the boot, and catching a 17-second illusion

*2026-08-26*

The open question was whether the boot's ~727 ns per counted event is a uniform inflation or a
thin tail of outliers. An average cannot answer that; a distribution can. `try_invoke` now
histograms its own duration under `RAVA_NATIVE_HIST`.

## The distribution (inclusive timing — see the correction below)

```
      128 ns  32.5%      1024 ns  4.1%      16384 ns  0.6%
      256 ns  44.7%      2048 ns  4.4%      32768 ns  0.5%
      512 ns   7.1%      4096 ns  1.8%      65536 ns  0.2%
                         8192 ns  1.3%     >=131072  0.1%
```

The median native call in the boot is 256–512 ns against the warm microbenchmark's 142 ns, so the
whole distribution has moved — the mean is not an artefact of a thin tail.

## The 17-second illusion, and how it was caught

Attributing the slow buckets by callee named a single method carrying **17.03 s over 117,635
calls at 145 us each** — `java/lang/String.valueOf` on DataFixerUpper type templates, ~19 s once
the sibling entries were added. Forty-two percent of the boot in one method.

Then the argument's rendered STRING turned out to be **under 64 bytes**, at 300 us a call. A
sub-64-byte string cannot take 300 us, so either the render path was pathological or the
measurement was wrong.

⛔**The measurement was wrong, and this is the correction.** `String.valueOf` re-enters the guest
through the object's `toString`, which calls natives again — DFU's type templates are recursive,
so an inclusive timer at every level bills every child to every ancestor. Depth-gating the timer
so only the OUTERMOST native invocation is measured makes the totals a real partition of wall
time, and the same entry collapses:

```
inclusive   String.valueOf(Sum)   8.29 s over 26,504 calls
exclusive   String.valueOf(Sum)   1.14 s over    288 calls
```

**Native calls total 5.37 s of the 40 s boot**, mean 1254 ns per outermost call. That is
consistent with every other seam estimate, and there is no villain in it.

## What survives

* Natives: **5.37 s** (13% of the boot), no single entry above 1.14 s.
* `String.valueOf` on a DFU `Sum` still costs **3.96 ms per call** for a sub-64-byte result — 288
  calls, 1.14 s, 2.8% of the boot. Concentrated, and the only named item left of that size.
* `Files.newInputStream` **0.58 s** over 6,093 calls and `Files.find` **0.27 s** over 183 — the
  file I/O the phase map had already spotted, now with call counts.

Adding the accounted parts: natives 5.37 s + compiled code 3.6 s (9.1% of samples) + interpreter
~1.1 s (197 M opcodes) ≈ **10 s of 40**. The remaining thirty are the host outside `try_invoke` —
GC, allocation, class loading, the JIT bridge, the string caches — exactly the categories the
profile has been reporting all along, none above 11.4%.

**So the call-level search is closed from this direction too.** The distribution says the cost is
real and spread, not an artefact of averaging.
