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
- Ordered early exits in short counted loops (`opt/unroll_early_exit.rs`). The JIT now clones up
  to eight ordered checks and their deoptimization states when the loop shape and side effects
  are proven simple. `UnrollEarlyExitSem` matches HotSpot with CFG-edge checks enabled, including
  a side-exit store, a late null fault, and eight eligible loops in one method. Before each
  expansion, the pass reserves fresh block-order gaps while preserving ties between existing
  live blocks. Older seven-process and 21-pair comparisons used HotSpot 25.0.3 without
  `-Xshare:off`; that JDK loaded `classes.jsa`, so those timings are excluded from no-snapshot
  evidence. Their Rava pass-off control also selected `MixinBench.round`, so it is not a valid
  no-adapt control. Two later 21-pair runs used HotSpot 21.0.7; a CDS log check confirmed that
  runtime loaded no shared archive. Across those 42 process medians, default Rava measured
  3.87 ns/call (3.775–4.28) and HotSpot 3.9425 (3.51–4.165), with `600000` checksums. Rava beat
  Java in 31/42 pairs, but their ranges overlap widely, so this is near parity on one synthetic
  callback path, not evidence of a general Java advantage. The old pass-off control was still
  method-specific, so it does not attribute this result under no-adapt. Current controls are
  global, and current Java benchmark runs explicitly use `-Xshare:off`. The Rava binary
  SHA-256 was `43D10E17FCEC9990BAC2D7B0BCC22D7C109B685B7B70378F64B0DF05D0432644`; the benchmark
  source SHA-256 was `E07CE81A23F4D412626186E5DAB38F6B01199C3C84094C894A130BC1E12CFF66`, and the JDK
  21-compiled class SHA-256 was `BD94D62D6243409F299F699402D74D762576B7028FC34F6A766970FA12BC172F`.
  Separate historical `early90` and recursion figures used JDK 25 without `-Xshare:off`; that
  runtime loaded CDS, so those timings are excluded from no-snapshot evidence. The current
  candidate has not been measured on Paper.
- Four-step affine recurrence folding (`opt/affine.rs`) wins on the arithmetic kernel it
  targets. Nine rotated fresh-process triplets on JDK 21 measured `CountedEntryBench.counted`
  (64 `value * 33 + i` updates) at 10.092 ns/call on default Rava, 30.935 with the same binary
  and the global `RAVA_NO_AFFINE_RECURRENCE=1` control, and 29.731 on HotSpot 21.0.7 with
  `-Xshare:off`. Across process medians the ranges were 8.659–11.597, 28.277–31.428, and
  27.484–30.267 respectively. All 27 checksums matched at `-80388026518`; default Rava beat
  affine-off and HotSpot in all nine pairs. Rava used its default optimization mode and a fixed
  hash seed; it read the JDK 21.0.12.1 runtime image. This establishes a win on this arithmetic
  loop, not on Minecraft ticks. Rava binary SHA-256:
  `9B2EE67183136A29E03132955557DE50D714CBB5DDDB27345F651A150330D9BD`; benchmark source SHA-256:
  `C028ED4A998C419DBB62648898BD835EFD6CEE863A85244DF4126409145B415B`; JDK 21 class SHA-256:
  `C0211F761DF55E29F8575273855899782833543D64A127E4D7CFDFC553014205`.
- Rechecked the same affine recurrence kernel on JDK 25.0.3 with the current local release
  candidate (`798A342DC6E44914FE0E86C513256F918F98A6CB7C42A1202143DA33C6BD2625`). Nine rotated
  fresh-process triplets measured 9.606 ns/call on default Rava (8.682–10.870), 30.994 with
  `RAVA_NO_AFFINE_RECURRENCE=1` (28.415–31.429), and 29.888 on HotSpot with `-Xshare:off`
  (27.317–30.381). Rava won all nine paired comparisons against both controls; all 27 checksums
  matched at `-80388026518`. The benchmark source hash is
  `C028ED4A998C419DBB62648898BD835EFD6CEE863A85244DF4126409145B415B`, and the JDK 25.0.3 class
  hash is `77920C52A45C1ECA45F1F4917011E1152B5868FF7F15398ADA5492CE37C0E77A`. This confirms the
  targeted synthetic-loop win on the current binary; it does not establish a Minecraft tick win.
- The same uncommitted local release candidate also beat HotSpot 21.0.7 (`-Xshare:off`) in three broader
  generic kernels, each over nine rotated fresh-process pairs with matching checksums or output guards and
  default Rava tiering: `RecordHashBench` measured 7.55 vs 10.45 ns/op (process ranges 7.40–8.00
  vs 9.50–10.75; checksum `33546240000`); `SystemTickBench`, with 40 systems and 80 listener
  calls per tick, measured 1,256.75 vs 1,703.10 ns/tick, with ranges 1,186.55–1,306.45 vs
  1,625.60–1,747.50 and checksum `760000`; and `NoiseBench` measured 12 vs 17 ms per sweep
  (11–12 vs 16–17; output guard
  `525558`).
  `NoiseBench` reports the best of ten timed sweeps per process and has millisecond resolution,
  so treat it as a coarse signal. These are synthetic kernels, not a Paper server result. The
  Rava binary SHA-256 is the one above. These early `SystemTickBench` figures used an inactive
  profiler and predate the active-profiler control below. JDK 21 source/class SHA-256 pairs:
  `RecordHashBench`
  `64004A4101B5C6ECA37335FDF03116253A8ADD432B662D6EAAFC4355806CD3B5` /
  `1BBD46554787C61425A7325409CB09B06DBDF6786533815020595DFC59FE8F85`;
  `SystemTickBench` `C2B50027F7DFD8CF6F8E0C2FAE4795AD7DB5385FDEBF9EA9D0A4ACD39AE6BAAB` /
  `194EAADD439AB09B43A93EBE8B90E5E7BE0C29B2C531F9475EAC7B2B075F77EF`; `NoiseBench`
  `CD97843FBE03E2E6FF9AC75C51D76D8442C91D90748B12DFC656B685F8CCB6CB` /
  `F9FA1DF311EA159FE6C2D0CA58FED408095A1646C80E80569306C7B8372D5C7B`.
- A fresh nine-process rotation on the current local release binary (`DA6E8FDBFDCC147C735DEF9DD2E2141FBE607FD20EFBE53BC1356E4F163D9F4A`) measured
  `SystemTickBench` over 15 rounds of 20,000 ticks per process, using the median of each process's last ten rounds. Default Rava measured
  1,205.5 ns/tick (process medians 1,159.0–1,254.6), the same binary with global
  `RAVA_NO_SHRINKWRAP=1` measured 1,236.8 (1,168.8–1,268.2), and HotSpot 21.0.7 with
  `-Xshare:off` measured 1,482.3 (1,378.5–1,757.5). All 27 checksums were `2850000`; default Rava
  beat HotSpot in all nine process pairs and beat the shrink-wrap-off control in eight of nine.
  The control ranges overlap, so the small shrink-wrap difference is directional only. Rava used
  class library 21.0.12.1+1 and fixed hash seed `1234567`; source SHA-256 is
  `C2B50027F7DFD8CF6F8E0C2FAE4795AD7DB5385FDEBF9EA9D0A4ACD39AE6BAAB`, and the JDK 21.0.7 class
  SHA-256 is `194EAADD439AB09B43A93EBE8B90E5E7BE0C29B2C531F9475EAC7B2B075F77EF`. This remains a
  synthetic listener workload, not a Paper server measurement.
- A refreshed `SystemTickBench` run compares three conditions in nine interleaved fresh-process
  pair groups on the current release binary (`CF01190DFB57502E7E503481CB45972F24492A7F45B75760A7DF21FC31834D2A`)
  and HotSpot 21.0.11 (`-Xshare:off`): default Rava with a no-op profiler measured 654.45 ns/tick
  (process medians 642.45–691.4) versus 1,459.1 for HotSpot (1,415.6–1,551.3), a 2.23× ratio
  and 9/9 Rava wins. With `RAVA_NO_DEAD_CONCAT=1`, Rava measured 1,253.15
  (1,193.6–1,338.95) versus 1,514.95 (1,447.25–1,611.45), a 1.209× ratio and 9/9 wins.
  With a recording profiler that consumes each pushed string using `hashCode()`, Rava measured
  2,067.2 (2,034.6–2,252.05) versus 2,079.1 (2,013.15–2,275.3), a 1.006× ratio and 6/9 wins;
  process ranges overlap. The mode order rotates across pair groups, and Rava/HotSpot order
  alternates within each mode. All 54 tick checksums matched at `2850000`; all recording-profiler
  checksums matched at `6878236459927801088`, and no-op checksums were zero. The pass-off control
  and paired Java shifts support dead string elimination as the main source of the no-op
  advantage on this kernel; the active-profiler case remains near parity. Source SHA-256 is
  `ED07615AC432E7D8C4BF618004DE20A1CB65A1CF0E091AFBF0FBBEAF2D68E771`, and the JDK 21 class
  SHA-256 is `A9C977B364CE121312C393C0B4115B58A3628D9076FEE4F0C00609752959ECB8`. This remains a
  synthetic dispatch benchmark, not a full Paper server result.
- On the same binary and JDK, nine alternating pairs of `RecordHashBench` measured the median
  of the last four of six timed rounds per fresh process: 7.6 ns/op on Rava (process medians
  7.3–7.85) and 9.4 on HotSpot (8.75–9.8), with Rava ahead in all nine pairs. HotSpot again
  used `-Xshare:off`; all 18 checksums were `33546240000`. Source SHA-256 is
  `64004A4101B5C6ECA37335FDF03116253A8ADD432B662D6EAAFC4355806CD3B5`, and the JDK 21 class
  SHA-256 is `BB51A029F7AACE5005FEC243300541767F172DDBE419F3B71AEBA5B509C6F680`. This is a
  separate generic kernel, not evidence of end-to-end Minecraft performance.

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
   optimized-code samples fell in the first bytes of methods). The isolated floating-fast-exit
   variant measured 1.323 vs 1.305 ns/node on `early90` and 0.710 vs 0.686 on recursion; Java
   measured 1.260 and 1.138 respectively. Checksums matched in seven rotated fresh processes per
   VM. The variant did not beat clean `HEAD` or Java on these cases and is dropped; existing
   integer fast exits remain. Next is wider shrink-wrapping, which saves registers only on paths
   that use them.
3. **Register allocation quality.** Values are spilled around calls and reloaded at block
   starts (`mov %rax,0x50(%rsp); mov 0x50(%rsp),%rax` across a label), and loop-carried values
   are kept in stack slots across calls. An isolated CFG-aware call-cost variant measured 5.45
   vs 5.80 ns/call on `MixinBench`, 1.287 vs 1.305 ns/node on `early90`, and 0.646 vs 0.686 on
   recursion against clean `HEAD`; it remained slower than Java on `MixinBench` and is not in the
   current candidate. Combined with unrolling, it moved medians 3.88 → 3.84 on `MixinBench`,
   1.282 → 1.219 on `early90`, and 0.647 → 0.665 on recursion; only 4/7, 6/7, and 0/7 paired
   processes improved, respectively, with overlapping ranges. It is not a consistent gain.
   Compile-time cost and a focused liveness allocator test remain open before revisiting it.
4. **Automatic loop parallelism.** `RAVA_PAR_REPORT` counts SSA effects after optimization; it
   does not prove array-base/index disjointness, scalar recurrences, or preservation of
   exceptions and deoptimization order. A worker scheduler must also coordinate Java memory
   visibility with GC and safepoints. The Minestom harness accepts a run count and radius,
   prints and checks the chunk count and sampled result hash, and rotates launch order for
   three-way comparisons. A 15-group attempt on Minestom 1.21.8, radius 24 (2,304 chunks),
   JDK 25.0.3 completed five groups before the sixth rava process crashed with a native memory
   fault in `VCRUNTIME140.dll`; the runner returned failure and did not count the partial sixth
   group in its comparison. In those five complete groups, default HotSpot had a 532 ms
   chunk-phase median (508–551), HotSpot with
   `-XX:+UnlockExperimentalVMOptions -XX:-VMContinuations` had 645 ms (608–674), and rava had
   1,673 ms (974–2,886). All successful runs had the same sampled checksum
   `c997420016191634`; rava was slower in all five groups against both HotSpot modes. The paired
   median gaps were 1,141 ms against default HotSpot and 1,065 ms against flagged HotSpot; the
   paired median slowdown from the fallback flags was 116 ms for HotSpot. These five groups are
   exploratory performance data, conditional on successful completion, and do not establish
   reliability or steady-state tick throughput. The checksum covers 55 sampled block states in
   one local column per chunk, not the full world. Minestom starts each generation task with
   `Thread.startVirtualThread`; rava currently reports continuation support as false and runs
   each such thread on a native worker. The result is consistent with scheduling overhead but
   does not attribute the full gap or prove a loop-splitting benefit. Investigate the native
   crash, and keep loop scheduling serial until dependence proofs and runtime support exist.

   A later 15-group rerun used the local candidate above, whose source sets a 64 MiB native stack
   reserve per thread. All 45 launches completed and returned the same sampled checksum; no
   crash occurred. Chunk-phase medians were 545 ms for HotSpot (518–579), 654 ms for the
   `-XX:-VMContinuations` fallback (613–738), and 1,535 ms for rava (935–58,317). Rava lost all
   15 paired groups against both modes; the 58.3 s result remains in the range. A separate
   `RAVA_RECORD` run counted 2,339 threads, including 2,304 unnamed worker threads; 71.8% of
   sampled CPU was in syscalls and 17.9% in VM code. In source, each `Thread.start0` creates a
   native thread, while virtual-thread continuation support is disabled. This confirms the
   execution model and thread count in the run, but the single profile does not apportion the
   full slowdown. The rerun did not reproduce the earlier native fault; it does not establish
   that the stack-size experiment fixed that fault. The checksum still covers only sampled blocks,
   and this cold chunk-generation result does not measure steady-state ticks.

Measure each with `bench/java/MegaCallBench.java`, `bench/java/MixinBench.java` and the
`mobs` scenario (`bench/tick_sprint.py -r 3 -s 1200 -c bench/scenarios/mobs.txt`), against
`java`.
