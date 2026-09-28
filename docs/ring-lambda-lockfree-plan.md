# Serving capturing lambdas lock-free on a ring worker

> ✅ **PREMISE RE-DERIVED AGAINST THE ARMED INSTRUMENT (2026-08-07, ledger row 159) — and the
> population is BIGGER than this document claimed, not smaller.**
>
> The earlier warning here was right that the table below was built without `RAVA_FORK_STATS`.
> Re-measured with the probe armed, `RAVA_LANE_PROBE=1 RAVA_FORK_STATS=1`, one boot:
>
> ```
> Predicate#test            412,624  ┐ 80.0% of ALL remaining locked calls
> Function#apply            412,623  ┘
> AtomicLong#compareAndSet  205,051    19.9%
> BlockPos#<init>               841  ┐
> Objects#requireNonNull        128  ├ everything else together < 0.11%
> ThreadLocal#get               127  ┘
> locked_calls = 1,031,437     direct_hit = 212,249,475   table_hit = 42,158,504
> ```
>
> **Lambdas are 80% of the remainder, not 38%** — the share ROSE because `AtomicLong#get` was
> shipped away (row 127). And the diagnosis below is confirmed to the unit: `neg_hit = 412,628`
> against `Predicate#test = 412,624`. **Every lambda call stores `-1` (NeverCacheable) in the
> freeze table, reads it back, and falls through to the lock** — exactly the `$`-exclusion clause
> named in "The one line that causes it".
>
> ⚠ **BUT THE DESIGN BELOW IS INCOMPLETE, and this is the correction that matters.** It presents
> three coordinated edits, of which the third is "stop returning `NeverCacheable` for `$`".
> Lifting that exclusion does NOT produce a correct fill: `dispatch_resolve_cacheable_cls` resolves
> by `(class, name, descriptor)`, and a lambda receiver's class has no `test`/`apply` body to find
> — the implementation is reached through the lambda's SITE (`jit_direct_probe` /
> `lambda_impl_key`), which is a different mechanism, not a different key. **So the work is
> "route lambda receivers to site-based resolution inside the freeze path", not "widen an
> admission clause".** Size it accordingly before starting.
>
> Everything below about the KEY COLLISION, the failure mode and the gate discipline is accurate
> and is why the design is kept.

**Status: specified, not started.** Written 2026-08-07 from ledger rows 111-128 so the next
attempt executes a design instead of re-deriving one. Every number below is measured on the
shipped artifact (`bench/build_prod.ps1 -BootTrain`), not estimated.

## Why this and not something else

The shipped ring does **not** abort — it completes `127/127` with `keep=true`. Its problem is
WAIT:

```
locks=84,283,234   wait_ms=40,480   hold_ms=5,945      (RAVA_LANE_PROBE, row 111)
```

40.5 s of aggregate wait against 5.9 s held: the workers are **parked, not busy**. The
population is named (row 112):

| locked callee | count/boot | share |
|---|---:|---|
| `java/util/function/Predicate#test` | 400,230 | 19% |
| `java/util/function/Function#apply` | 400,229 | 19% |
| `AtomicLong#compareAndSet` | 200,302 | 9% |
| ~~`AtomicLong#get`~~ | ~~200,302~~ | **shipped, row 127** |

So lambdas are **38%** and they are the largest remaining block.
**⬆ SUPERSEDED by the armed re-measurement in the header: the true share is 80%.**

## The one line that causes it

`Vm::dispatch_resolve_cacheable_cls`, first clause:

```rust
if cls.starts_with("java/") || cls.starts_with('$') || … { return NeverCacheable; }
```

Every lambda receiver's class is `$Lambda`, so **every lambda call is excluded from caching
unconditionally**. A worker that cannot cache must take the resolve lock. That is the 800k
lock takes.

`RAVA_LAMBDA_PIC` exists and was **verified firing** (locked lambdas 400,230 → 377,690), but its
own header scopes it to NON-CAPTURING ("directable") lambdas — 5.6% of the population. The
ring's lambdas carry captured state.

## The design

**The obstacle is the KEY, not the mechanism.** The freeze table already serves compiled callees
lock-free on workers (`freeze_resolve::get` → `f(buf, ctx)`), keyed `(caller|cp<<48, ck)` where
`ck` is the receiver's class word. For lambdas that key **collides**: every lambda has the same
class word, so one entry would answer for unrelated implementations — a silently wrong dispatch,
not a crash.

`heap.lambda_site_key(h)` already exists and yields a per-SITE key; `jit_direct_probe` uses it
under `lambda_impl_key_enabled`, and `lambda_site_pin` makes the address immortal. A lambda's
implementation is a function of its site; its captures are its operands.

Three coordinated edits, and they must land together:

1. **Lookup** (`jit.rs`, the worker freeze arm): when the receiver's class word is the lambda
   scratch pointer, use `lambda_site_key(h)` as `ck` instead of the class word.
2. **Store** (`lib.rs`, `dispatch_miss_resolve`'s `freeze_resolve::put`): the same substitution,
   or the table stores under a key the lookup can never produce — the exact defect row 111 found
   (`put` used `caller|cp<<48` while `get` used `cache_ptr`: `stored=6113`, `table_hit=0`).
3. **Admission** (`dispatch_resolve_cacheable_cls`): stop returning `NeverCacheable` for `$`
   **only** when the site-key path above is active, so the class-keyed arms are never reached
   with a lambda receiver.

Leave `AtomicLong#compareAndSet` alone: it mutates, and the pre-lock probe template is for pure
reads (row 127).

## How it fails, and how to see it

The failure mode is **a plausible wrong answer**, not a crash: the wrong lambda body runs and
returns a number of the right type. Nothing in the default gates covers it — rows 105 and 122
both turned on this distinction.

* `RingLane` / `RingLaneInstance` are the gates that ARM this path. They must stay
  `37539c6c375d0c3`. `ChunkFill` (`8790f04ace726ecf`) does not exercise it.
* `RAVA_FREEZE_VERIFY` re-derives, with pure reads, the body the locked path would have entered
  and compares it against what the table is about to call. Run with it on before trusting a
  wall number.
* `bench/RingLane.java` exists because no other gate arms the lane; it is the one that moved
  from `3782fea2971a062` to the JVM's hash when wide-param sites were refused at publication.

## Acceptance test — decide it on these, not on wall

Wall will not move much and that is **predicted**, so do not use it as the criterion:

1. `RAVA_LANE_PROBE`: `Predicate#test` and `Function#apply` must **leave** the locked-callee
   list, the way `AtomicLong#get` did in row 127.
2. `wait_ms` must fall from ~40,480 toward ~25,000 (the 38% share).
3. `completed=127/127 keep=true` must hold — the batch must not start aborting.
4. Gates bit-exact **with the mechanism armed**, not just with it compiled in.

## The ceiling, stated up front

Row 112's arithmetic bounds the entire 40.5 s of wait at **`Done ≈ 11-12 s`**. This work is 38%
of that, so on its own it is worth roughly **`Done 14.6 → 13.5`**. It does **not** reach
`Done < 10`, and it does nothing for `wall`, whose binding constraint is the ~29 s serial prefix.

Do it because it is the largest measured, bounded, correct piece of work available — not because
it closes the goal. `wall < 20` still requires the value-model rewrite, whose own first cost is
now known (row 120: the ring's global abort, not object birth).


## THE NEXT MEASUREMENT, SPECIFIED (2026-08-07, after ledger row 161)

Row 161 refuted this plan's central framing for the direct path. `jit_direct_probe` **already**
keys a lambda receiver by `lambda_site_key` when `RAVA_LAMBDA_IMPL_KEY=1`, and `jit_direct_resolve`
computes the SAME key (`jit_lambda_handle_ident`) — so probe and resolve agree, and turning the
flag on still leaves `locked_calls` at 1,024,679 against a baseline of 1,031,437. **The obstacle
is not the key.** Worse, the flag drives worker direct callbacks `d_cb` from 387,088/102,834 to
**0/0**, so it is a regression that reads as neutral on wall.

So the question is *which* refusal fires, and there is a counter for exactly that —
`jit::DIRECT_MISS[0..2]`, reported by `jit::callback_stats()` as
`dmiss_recv` (receiver shape undecodable) / `dmiss_mega` (megamorphic site) / `dmiss_res`
(resolve refused).

**Recipe — every variable in ONE command, per the row-129 rule (a fresh shell per tool call means
env does NOT persist):**

```powershell
$env:RAVA_JIT='1'; $env:RAVA_CALLBACK_STATS='1'; $env:RAVA_LANE_PROBE='1'; $env:RAVA_FORK_STATS='1'
# ... boot server.jar, then read `dmiss_recv= dmiss_mega= dmiss_res=` beside `locked_calls=`
```

Run it twice: once with `RAVA_LAMBDA_IMPL_KEY` unset, once with it `=1`. **The pair is what names
the cause**, because the flag is known to change `d_cb` to zero and a single run cannot say which
of the three refusals moved.

⚠One hypothesis worth holding loosely, NOT a finding: the arena-lambda group is default-ON now, so
many ring lambda receivers are ARENA lambdas, which take a different branch in both probe
(`arena_lambda_of_cell` → `arena_lambda_guard(si)`) and resolve (`site_key` deliberately `0`,
"because the guard already IS the site"). If the ring's `Predicate#test` receivers are arena
lambdas, `RAVA_LAMBDA_IMPL_KEY` could not have affected them at all, and the whole lambda question
belongs on the arena side. **`dmiss_*` decides it; do not build on this paragraph.**
