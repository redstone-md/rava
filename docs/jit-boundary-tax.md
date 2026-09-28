# The boundary tax: machcode root-cause of RAVA's worldgen gap (2026-07-23)

Machcode-level root cause of *why* MC boot loses ~17× while RAVA wins its own
benchmarks. Supersedes the "distributed C2-maturity" framing with a precise,
reproducible mechanism.

## The two-microbenchmark proof
Both bit-identical results (Double.doubleToLongBits) vs JVM; RAVA_JIT=1, release.

| microbench (corpus/compute) | what it does | RAVA vs JVM |
|---|---|---|
| `DLoop` | tight loop of pure double arithmetic, NO calls | **1.29×** |
| `VCall` | per-element **virtual** double-returning call tree (mimics a density function) | **11.5×** (3553 vs 310 ms) |

Same arithmetic, same doubles. The ONLY difference is call-boundary density.
DLoop proves RAVA's double math is fine (Cranelift keeps f64 in xmm registers, raw
bits, no tag — see jit.rs:2490 `JT::Double => f64::from_bits`, statically typed slots).
VCall proves the gap is entirely the **per-call boundary tax**.

## Why the boundary is expensive — machcode of `VCall$Mul.compute(D)D`
The method is literally `return a.compute(x) * b.compute(x)` — 2 virtual calls + 1
multiply. Its CLIF: **77 load.i64 + ~150 integer/bit ops (band/ushr/icmp/select/sete)
+ 4 call_indirect** — ~300 IR ops for what C2 inlines to a handful of xmm instructions.
(DensityFunctions$Ap2.compute, the real worldgen hot method, is the same shape at
scale: 2481 machine instrs, a 2352-byte stack frame, 293 i64 loads, ~350 tag ops.)

The cost decomposes into three PER-CALL taxes, none present in a tight loop:

1. **Memory-locals calling convention.** Compiled methods have the uniform signature
   `fn(*mut i64 locals, i64 array_base) -> i64 bits` (jit.rs:9437). Arguments are passed
   through a memory locals array, not registers — so every call stores args to memory
   and the callee loads them back. This is the bulk of the 77 loads. C2 passes doubles
   in xmm registers.
2. **Reference untagging.** Each field/receiver load (`a`, `b`) is a tagged Value; band/
   ushr extract the object pointer before use.
3. **PIC dispatch.** class-id load + icmp against the cached class + select + call_indirect
   (+ callback on miss).

All three are paid *per call*. Worldgen is a deep recursive virtual-dispatch tree
(every DensityFunction node = a virtual `compute()` returning a double), so it pays the
tax millions of times. RAVA's winning benchmarks are call-light (tight loops, ≤4-way
dispatch), so they never pay it. That is the whole paradox.

## Why the earlier fixes were all neutral
The three neutral native-layer fixes (nested cache, iterator intrinsic, fused streams)
all live ABOVE this boundary — they change what the natives do, not the per-call ABI of
the compiled worldgen tree. The bound is the compiled-to-compiled call convention, which
none of them touch.

## The lever (untried, machcode-targeted, LARGE)
**A register-based calling convention for compiled methods, specialized by primitive
descriptor.** A `(D)D` method compiled as `fn(ctx, f64) -> f64` (args + return in xmm,
no locals-array round-trip, no box) would collapse tax #1 — the dominant one — for the
double-dense worldgen tree, on the PIC-hit fast path. Distinct from the CLOSED
representation-unification arc (that was about ref/handle indirection and measured ~0
because "loads dominate" — this explains WHY they dominate: the memory-locals ABI).

This is foundational (touches prologue, every call site, and the PIC/indirect-call
SigRef agreement) — a multi-session arc, not a bounded fix. But it is the FIRST lever
root-caused to the actual dominant machcode cost, with a fast reproducer (`VCall`,
seconds) to iterate against instead of a 2-minute MC boot.

## Reproduce
```
cd corpus/compute
java VCall 20000000                                             # JVM ~310ms
RAVA_JIT=1 rava.exe -cp . VCall 20000000                        # RAVA ~3550ms (11.5×)
RAVA_JIT=1 RAVA_JIT_DUMP=VCall rava.exe -cp . VCall 2000000     # dumps CLIF+machcode to clif.txt
```
