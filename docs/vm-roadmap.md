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
- **Call-site bookkeeping**: removing the BCI store and the pending-exception test after a
  call (as an experiment) saves 3 % on `MegaCallBench`.

## Done
- **Inlining by shared target** (`opt/inline.rs`): at a megamorphic site whose recorded
  receivers select one implementation, that implementation is inlined behind a vtable test.
  `MixinBench` 19.8 → 11.5 ns (`java` 7.0); `mobs` about −2.5 %.
- **Lazy frame for fallback calls** (`opt/codegen.rs`): a method whose only Java calls are the
  ones made when no inlined target's guard admits the receiver publishes its frame at those
  calls, not on entry. `mobs` 81.2 → 78.0 ms per tick over four rounds (three won).

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
