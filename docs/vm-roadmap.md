# VM roadmap: closing the gap to HotSpot under load

Under All The Mods 10 loads (`bench/scenarios/`), rava's optimized code is about 1.5× slower
than HotSpot's in steady state. On `mobs`, third sprint in a row, a tick takes 75 ms against 49.
Warm-up is not the cause. The gap is spread evenly: a method-by-method comparison
(`RAVA_PERF_MAP=inline` against `java` with JFR) finds nearly every call chain 1.3–2× slower,
and no single method stands out. Below, in order: what was ruled out, what was fixed, what is
left.

## Ruled out
- Heap locality. Size-class pages do not hurt pointer chasing: on a micro-benchmark of
  entities with small parts, rava takes 37 ns per entity and `java` 100.
- Mixin allocations. Escape analysis removes the `CallbackInfoReturnable` of an inlined
  handler.
- Trampolines on direct calls. Calling a known method's entry directly, with the call sites
  repointed when the entry changes (as HotSpot does), left `mobs` unchanged (114.3 against
  112.2 ms per tick). The method-entry samples are the callee's code arriving, not the jump.
- Inlining budget (`TIER2_BUDGET`, one round of two sprints each on `mobs`). 8000 cost 11 %
  (115 against 103 ms per tick); 32000 changed nothing (111 against 111). The tick already
  gets the inlining it needs, and more does not help.
- Interface casts. They cost 13 % of a two-lambda loop (measured by removing `checkcast` by
  hand), but C2 makes the same check through the secondary-supers cache, and without the
  casts the loop still runs 1.6× slower than `java`'s.
- Call-site bookkeeping. Removing the BCI store and the pending-exception test after a call
  (as an experiment) saves 3 % on `MegaCallBench`.

## Done
- Inlining by shared target (`opt/inline.rs`). At a megamorphic site whose recorded receivers
  all select one implementation, that implementation is inlined behind a vtable test.
  `MixinBench` 19.8 → 11.5 ns (`java` 7.0); `mobs` about −2.5 %.
- Lazy frame for fallback calls (`opt/codegen.rs`). Some methods make Java calls only when no
  inlined target's guard admits the receiver. Such a method now publishes its frame at those
  calls instead of on entry. `mobs` 81.2 → 78.0 ms per tick over four rounds (three won).
- Loop-invariant guards in inlined Mixin handlers (`opt/escape.rs`, `opt/rce.rs`). The state
  a loop's entry deoptimizes to mentioned the handler's scalar-replaced
  `CallbackInfoReturnable`, so none of the loop's guards or predicates could be hoisted. Escape
  analysis now rewrites loop-entry states too, and the hoisting passes accept replaced objects
  in them. Together with a deopt (not a call) when a receiver fails a shared-target guard
  (`opt/inline.rs`) and hoisted class metadata loads (`opt/licm.rs`), the slot loop of
  `MixinBench` is down to its load, compare and increment: 14.5 → 12.4 ns (`java` 9.2 on the
  same host). `mobs` unchanged (108.9 against 109.3 ms per tick, three rounds on a slower host).
- Non-null facts for LICM (`opt/conds.rs`, `opt/licm.rs`). `conds.rs` dropped a null check
  when a dominating test or a phi of non-null values proved it. LICM looked only at null
  checks, so it then kept the loads through that value inside the loop. Since 89c8e980 this
  made a stream pipeline reload its sink chain per element again (found by bisection): `Kern`
  stream 15.5 → 7.8-9.6 ms (`java` 7.6). LICM now uses `conds.rs`'s facts.
- Block layout (`opt/codegen.rs`). When a chain of fall-through successors ended, the next
  block emitted was the next one in build order, often a rare one, so cold code sat between
  hot chains. Chains now start at the most frequent unplaced block. `mobs` warm sprints
  113.2 → 108.7 ms per tick (two rounds of three won).
- Direct calls to the only implementation (`opt/inline.rs`). A virtual call of a method that
  nothing overrides and that was not inlined (too large, recursive) went through the vtable.
  It now calls the target directly, behind its class-hierarchy dependency. `mobs` warm sprints
  103.6 → 101.1 ms per tick (two rounds won, one even).

## Where the time goes (`mobs`, all optimized code dumped, samples by instruction)
93 % of the server thread's time is in optimized code. Classified by the instruction a sample
stalls on: loads from memory 60 % (of which an object's first word, its class, 38 %;
small-offset fields 37 %; array elements 9 %; class metadata 9 %), branches 15 %, the first
instructions of a method (pushes of callee-saved registers, i.e. the cost of arriving through
a call) 8 %, spill slots 2 %, frame slots 2 %, the thread word 2 %. The code spends far more
time waiting on memory than executing call protocol. What remains is the number of objects
touched per tick and the work done between them, not only call or register overhead.

The same classification for `java` (C2 code copied out of the process after the sprint), same
sprint, absolute samples at 500 Hz: loads are about equal (rava ~38800, `java` ~41000), so the
difference is neither the heap layout nor the number of objects touched. rava's excess is at
method entry (~5400 samples, 4200 of them on the first instruction: the frontend arriving
through a call, which costs `java` ~800), in branches (+2800), and in frame, thread-word and
spill traffic (+~500). rava's sampled code also spans 40 % more cache lines (7900 against
5700). Three rounds of three sprints: `java` 87-100 ms per tick warm, rava 114-118.

On this host, per-process timing varies by up to 2× for both `java` and rava (4 shared vCPUs).
Compare the minimum or median of many runs, never a single run.

## To do
1. **Register calling convention.** Direct, vtable and interface calls between optimized
   methods pass their first arguments in registers (`docs/opt-tier.md`), and the allocator aims
   arguments at those registers. `bench/java/DirectCallBench.java` gains about 12 % on
   double-heavy calls, and `MegaCallBench` 9-10 % at best on megamorphic virtual and interface
   calls (both noisy). Paper's start and the `mobs` sprints are unchanged within noise. Still
   open: values live across calls go to the four callee-saved registers or to memory (there is
   no splitting around calls), and parameters the allocator spills are stored by the prologue
   and reloaded.
2. **Prologue cost.** Early returns now run before the prologue (fast exits): `DirectCallBench`
   fib 2.4 → 1.85 ns and the four-int recursion 4.0 → 2.3 ns per call (HotSpot 1.4 and 1.9),
   `mobs` sprints about 3 % faster (noisy). Every other entry still checks the value-stack
   limit, links the frame and pushes callee-saved registers before its first block (10–14 % of
   optimized-code samples fell in the first bytes of methods). Next is wider shrink-wrapping,
   which saves registers only on the paths that use them.
3. **Register allocation quality.** Values are spilled around calls and reloaded at block
   starts (`mov %rax,0x50(%rsp); mov 0x50(%rsp),%rax` across a label), and loop-carried values
   are kept in stack slots across calls.

Measure each with `bench/java/MegaCallBench.java`, `bench/java/MixinBench.java` and the
`mobs` scenario (`bench/tick_sprint.py -r 3 -s 1200 -c bench/scenarios/mobs.txt`), against
`java`.
