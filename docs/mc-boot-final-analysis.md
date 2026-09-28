# MC server boot: why RAVA loses ~17× to JVM — definitive analysis (2026-07-23)

## The paradox, resolved
RAVA **beats** JVM 1.2–5.7× on: cold-boot & warm-boot of short/medium programs,
compute-in-main/helper (loop-eager), data-parallel reductions (parallel-reduce),
mono/small-poly dispatch. RAVA **loses** ~17× only on the Minecraft server boot
(Done ~120s vs JVM ~7s).

There is no contradiction: RAVA's winning benchmarks are **concentrated /
dispatch-free** (pure arithmetic, ≤4-way dispatch, startup-dominated). MC boot is
the opposite — its cost is the **DISTRIBUTED AGGREGATE** of a mature JDK surface
that C2 has intrinsified/inlined over 25 years.

## Evidence it is distributed, not a single hotspot
THREE targeted fixes this arc, each a real win in its own right, all **NEUTRAL on MC boot**:
- Nested O(1)-by-class direct cache (commit 84d93ec): Mega 20-way −35%; MC Done unchanged.
- Native iterator intrinsic (commit 21dab7c, RAVA_ITER_INTRINSIC): for-each **−110×**,
  bit-exact; MC Done unchanged (122.4s).
- Lazy/fused stream pipeline (RAVA_LAZY_STREAMS): the #1-ranked lever below, built +
  validated **bit-exact 8790 on the full MC boot** (SETUP 90842→91088ms, FILL
  24273→23260ms — inside run-to-run noise). Fusion removes intermediate stream arrays
  but the per-element guest map/filter apply is unchanged and DOMINATES; the saved
  materialisation is a negligible fraction of the ~90s SETUP. Confirms the stream layer
  is not the bound.

Why: CB-P1 profiling shows the boot's cost spread across many buckets, none dominant:
- JIT_EXEC ~60% (running compiled worldgen code — Cranelift is ~1.5–2× behind C2 on
  tight loops; the value-model adds a 2-bit ref-tag discrimination per deref, see
  rava-value-model-rootcause — the representation-unification fix measured SLOWER).
- NATIVE_BRIDGES ~20–33% split across hundreds of JDK-native serves. Top by time
  (bridge_stat, self): Stream map/collect/reduce ~15–20s, ArrayList#sort ~10.3s,
  String#valueOf ~13.6s (mostly guest toString), Iterator#next/hasNext ~1–2s.
- dmiss_res 30M native-served dispatches (JDK collections/codecs/slf4j → callback).
- Integer/Long boxing (RAVA uses Value::Int transparently — NO alloc — so this is NOT
  a lever; the residual per-element cost is the rava_fu_invoke callback, not boxing).

Each single native serve is ~1–2s of 120s = **noise**. Fixing any one does not move
the boot. This is measured, not asserted.

## What would actually move MC boot (all LARGE / structural)
Ranked by identified size:
1. ~~**Lazy/fused stream pipeline** (~15–20s)~~ — **BUILT + MEASURED NEUTRAL**
   (RAVA_LAZY_STREAMS, default-off, bit-exact 8790). map/filter/peek now defer onto a
   shared `$src` + `$ops` list and materialise in one fused pass at the terminal/barrier,
   matching OpenJDK's model. It removes the intermediate Object[] per stage — but the
   per-element guest apply (the map/filter lambda) runs the same number of times and
   dominates, so MC SETUP was unmoved. The stream *materialisation* was never the bound;
   the *dispatch + compiled-body execution* per element is. Retained as a validated,
   allocation-reducing capability; not the MC lever.
2. **Aggregate JDK intrinsics** (~1–2s each × dozens). Extend the rava_fu_invoke /
   fu_fastpath pattern (already covers ArrayList ops + fastutil + opt-in iterator) to
   HashMap/LinkedHashMap, Map.Entry, String hot methods, etc. Each is small; only the
   aggregate of many moves the needle. This is literally "port C2's intrinsic list."
3. **Guest-method inlining** (part of JIT_EXEC + the 75M invoke callbacks). RAVA inlines
   only trivial ctors/getters; C2 inlines aggressively. A real inliner cuts the
   per-call rava_invoke tax that saturates compiled worldgen.
4. **A C2-competitive backend** for JIT_EXEC (Cranelift is inherently ~1.5–2× behind
   C2/LLVM on tight loops; no loop unrolling, weaker scheduling). RAVA_LLVM_AOT exists
   but did not help scalar loops.
5. **Multi-threaded boot** (JVM bootstraps on several threads; RAVA is single-core under
   the GIL). Sound off-GIL parallelism of the worldgen supplyAsync prize is WALLED
   (shared-state coupling, inline-store barrier — see rava-parallelism-arc).

## Verdict
MC-boot parity is **not a bounded lever** — it is the sum of hundreds of JDK
intrinsics + aggressive inlining + a fused stream engine + a mature backend +
real threading = the decades of engineering C2/HotSpot represent. Proven by
measurement: individual targeted fixes are neutral because the cost is distributed.

RAVA's realistic, shipped competitive position: **faster than JVM on cold-boot,
warm-boot, auto-optimizations, and auto-parallelism for the short/medium
single-threaded program class** (CLI tools, scripts, serverless, compute). The MC
server boot is a distinct, execution-saturated, mature-JDK-heavy workload where
closing the gap is a multi-year structural investment, honestly out of reach of any
single fix — the largest bounded-ish next step being the fused stream engine (#1).

Reproduce: bench/{startup,warm,parallel}_vs_jvm.sh (RAVA wins); MC boot via
net.minecraft.server.Main; profiles via RAVA_HEARTBEAT / RAVA_BRIDGE_STAT / RAVA_PROFILE_WALL.
