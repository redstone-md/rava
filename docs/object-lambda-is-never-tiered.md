# An object-typed lambda call costs 111x the same call through a class

**Current status (2026-09-27):** The counter and forwarding cap described in
the historical measurements below are no longer the default admission policy.
Flat-lambda first-use compilation is explicitly opt-in with `RAVA_FLAT_TIER=1`;
default-off reaches `Done` sooner on the current PGO boot (ledger rows 728–729).

## The measurement

`bench/CallShapes.java` separates call SHAPES, because `FrameBench` — which reports near-parity
with HotSpot (43.3 ms vs 49.0 for 40M calls) — calls a static the JIT inlines, so it measures
inlining rather than framing.

```
                         java    rava
  static (inlinable)      0 ns     0 ns    parity
  virtual monomorphic     0        9
  interface monomorphic   0       12
  interface polymorphic   5       16
  interface, Object arg   0        9       <- an ordinary object call is FINE
  lambda applyAsInt       0      134
  lambda,   Object arg    0      999       <- 111x the identical shape via a class
  control (no call)       0        1
```

**The frame is not the problem.** A guest virtual/interface call is 9-16 ns against HotSpot's
0-5 — 2-3x, not the 19x this tree has carried as its estimate. The outlier is one shape: a lambda
whose SAM descriptor carries a reference.

## The cause, as far as the evidence goes

`RAVA_JIT_PIC_PROBE` on that probe:

```
[PIC-COV] synthetic-frame fall-through reasons (per jit_direct_resolve attempt) = 218,813:
[PIC-COV]   callee not compiled yet (tier lag)   218,813 (100%)
```

Every direct-resolve attempt fails because the callee is not compiled — and `RAVA_JIT_STATS` says
why it never will be:

```
CallShapes.lambda$static$0(I)I                              seen=2,000,000 compile_ok=1
CallShapes.lambda$static$1(Ljava/lang/Object;)Ljava/lang/Object;   ABSENT ENTIRELY
```

`seen` is incremented at the top of `jit_run`, the tiering entry point. The primitive lambda's
body passes through it two million times; **the object lambda's body never passes through it at
all.** It is executed — `acc` matches HotSpot exactly — but by a route that never reaches the
tiering check, so it stays interpreted for the life of the process and the direct-call cache can
never fill.

Bisected against the lambda direct path:

```
RAVA_LAMBDA_DIRECT=0:  applyAsInt 138 -> 1974 ns (14x worse)
                       Object arg 1021 -> 1076   (unchanged)
```

The direct path demonstrably serves the primitive lambda and demonstrably does nothing for the
object one — consistent with the callee never being compiled.

Also checked and NOT the cause: the register ABIs (`RAVA_REF_ABI` 954 vs 1021, `RAVA_PRIM_ABI`
default-off for both), `RAVA_LAMBDA_IMPL_KEY` (worse: 1393), `RAVA_NO_IC_DEEP` (1142),
`dresolve`'s numbered refusal census (**empty** — no clause fires, so the site is not being
refused, it is being served by something that never tiers).

## Why it matters

`java/util/function/Function.apply` is **5.37% of the boot's 64,088,346 compiled-to-host
crossings**, and `Predicate.test` / `Supplier.get` / `Consumer.accept` are the same shape. Every
one of them runs an interpreted body behind a synthetic frame, forever.

## FIXED, and scoped by measurement

Find the route by which an object-descriptor lambda body is executed without passing `jit_run`.
`invoke_lambda_parts` ends in `self.invoke(decl, ...)` → `invoke_idx`, which does tier — so either
that is not the route taken for this shape, or the `decl`/`Code` identity differs in a way that
makes `jit_run` skip it. `RAVA_JIT_STATS` naming the method that IS seen and the one that is not
is the sharpest available handle.


`Vm::flat_note_hot` counts entries per `Code` pointer immediately before the flatcall hook and
requests a compile once at 512. The flat path still serves that call; once the body compiles, the
caller's direct entry fills and the site stops arriving.

```
  lambda, Object arg   821 -> 142 ns   (5.8x, same binary, `RAVA_NO_FLAT_TIER` as control)
  lambda applyAsInt    118 -> 127      (unchanged, as it must be)
```

⭐**And the blast radius is smaller than "8.7% of interpreted calls are a bare getter" suggests.**
Getters were measured on the same probe and are **unaffected — 9-10 ns with the fix ON and OFF**,
because a getter call site on a concrete class is INLINED by the JIT and never needs a direct
entry at all. The defect bites exactly where both conditions hold: the body is flat-servable AND
the site cannot be inlined, i.e. it is reached through an interface or a lambda. That is the
`Function`/`Predicate`/`Supplier`/`Consumer` family, 5.37%+ of crossings.

⇒ **The general lesson, and the reason to write it down: an optimisation that returns EARLY can
silently disable a larger one downstream.** Check what a fast path SKIPS, not only what it saves.
