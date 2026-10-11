# VM roadmap: closing the gap to HotSpot under load

Under All The Mods 10 loads (`bench/scenarios/`), rava's optimized code is about 1.5× slower
than HotSpot's in steady state: `mobs`, third sprint in a row, 75 ms per tick against 49.
Warm-up is not the cause. The gap is spread evenly: a method-by-method comparison
(`RAVA_PERF_MAP=inline` against `java` with JFR) finds nearly every call chain 1.3–2× slower,
with no single bad method. What was ruled out, what was fixed, and what is left, in order:

## Ruled out
- **Heap locality**: size-class pages do not hurt pointer chasing; on a micro-benchmark of
  entities with small parts, rava takes 37 ns per entity, `java` 100.
- **Mixin allocations**: escape analysis removes the `CallbackInfoReturnable` of an inlined
  handler.
- **Trampolines on direct calls**: calling a known method's entry directly, the call sites
  repointed when the entry changes (as HotSpot), left `mobs` unchanged (114.3 against 112.2
  ms per tick); the method-entry samples are the callee's code arriving, not the jump.
- **Call-site bookkeeping**: removing the BCI store and the pending-exception test after a
  call (as an experiment) saves 3 % on `MegaCallBench`.

## Done
- **Inlining by shared target** (`opt/inline.rs`): at a megamorphic site whose recorded
  receivers select one implementation, that implementation is inlined behind a vtable test.
  `MixinBench` 19.8 → 11.5 ns (`java` 7.0); `mobs` about −2.5 %.
- **Lazy frame for fallback calls** (`opt/codegen.rs`): a method whose only Java calls are the
  ones made when no inlined target's guard admits the receiver publishes its frame at those
  calls, not on entry. `mobs` 81.2 → 78.0 ms per tick over four rounds (three won).
- **Loop-invariant guards in inlined Mixin handlers** (`opt/escape.rs`, `opt/rce.rs`): the
  state a loop's entry deoptimizes to mentioned the handler's scalar-replaced
  `CallbackInfoReturnable`, so no guard or predicate of the loop could be hoisted. Escape
  analysis now rewrites loop-entry states too, and the hoisting passes accept replaced objects
  in them. With a deopt (not a call) when a receiver fails a shared-target guard
  (`opt/inline.rs`) and class metadata loads hoisted (`opt/licm.rs`), the slot loop of
  `MixinBench` keeps only its load, compare and increment: 14.5 → 12.4 ns (`java` 9.2 on the
  same host). `mobs` unchanged (108.9 against 109.3 ms per tick, three rounds on a slower host).
- **Non-null facts for LICM** (`opt/conds.rs`, `opt/licm.rs`): a null check that a dominating
  test or a phi of non-null values proves was dropped by `conds.rs`, and LICM, which looked
  only at null checks, then kept the loads through that value in the loop. Since 89c8e980 a
  stream pipeline's sink chain was reloaded per element again (bisected): `Kern` stream 15.5 →
  7.8-9.6 ms (`java` 7.6). LICM now takes `conds.rs`'s facts.
- **Block layout** (`opt/codegen.rs`): when a chain of fall-through successors ended, the next
  block emitted was the next in build order, often a rare one, so cold code sat between hot
  chains. Chains now start at the most frequent unplaced block. `mobs` warm sprints 113.2 →
  108.7 ms per tick (two rounds of three won).

## Where the time goes (`mobs`, all optimized code dumped, samples by instruction)
Of the server thread, 93 % is in optimized code. By the instruction a sample stalls on: loads
from memory 60 % (of which an object's first word, its class, 38 %; small-offset fields 37 %;
array elements 9 %; class metadata 9 %), branches 15 %, the first instructions of a method
(pushes of callee-saved registers: the cost of arriving through a call) 8 %, spill slots 2 %,
frame slots 2 %, the thread word 2 %. The code waits on memory far more than it executes
protocol: what is left is the number of objects touched per tick and the work between, not
call or register overhead alone.

The same classification for `java` (C2 code copied out of the process after the sprint), the
same sprint, absolute samples at 500 Hz: loads about equal (rava ~38800, `java` ~41000), so
neither the heap layout nor the number of objects touched is the difference. rava's excess
is at method entry (~5400 samples, 4200 of them on the first instruction: the frontend
arriving through a call, which `java` pays ~800 for), branches (+2800) and frame, thread-word
and spill traffic (+~500). rava's sampled code also spans 40 % more cache lines (7900 against
5700). Three rounds of three sprints: `java` 87-100 ms per tick warm, rava 114-118.

Per-process timing on this host varies by up to 2× for `java` and rava alike (4 shared vCPUs):
compare with the minimum or median of many runs, never one.

## To do
1. **Register calling convention.** Arguments go through the value stack in memory and the
   callee reads them back; every entry checks the value-stack limit, links the frame and pushes
   callee-saved registers. 10–14 % of optimized-code samples fall in the first bytes of
   methods; megamorphic calls cost 6–8 ns against HotSpot's 3.5. Passing arguments in
   registers and slimming the prologue needs the register allocator's cooperation:
   a multi-day project, potentially 10–20 %.
2. **Register allocation quality.** Values spilled around calls and reloaded at block starts
   (`mov %rax,0x50(%rsp); mov 0x50(%rsp),%rax` across a label), loop-carried values kept in
   stack slots across calls.

Measure each with `bench/java/MegaCallBench.java`, `bench/java/MixinBench.java` and the
`mobs` scenario (`bench/tick_sprint.py -r 3 -s 1200 -c bench/scenarios/mobs.txt`), against
`java`.
