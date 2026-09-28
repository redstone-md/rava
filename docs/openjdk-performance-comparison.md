# OpenJDK vs Rava: performance comparison

**Date:** 2026-09-27
**Purpose:** understand the measured startup gap, inspect the competitor's implementation, and avoid porting mechanisms that do not address Rava's measured bottlenecks.

## Executive summary

On this host, for the same Minecraft server JAR and restored world, Debian OpenJDK 21.0.12.1 reaches `Done` in **5.44–5.62 s**. Rava's current-source PGO `release-fat` baseline takes **21.2–21.8 s** and consumes **37.64 CPU-s**, compared with OpenJDK's **19.71 CPU-s**. The observed wall gap is approximately **3.9×**: about **1.9× more CPU work** multiplied by about **2× fewer effective cores** during Rava's boot.

This is not a single class-loader, GC, JIT, or startup-initialization defect. Rava's class pipeline is about 240 ms; the reference VM bootstrap and CDS mapping are tens of milliseconds. Current profiles and censuses show costs distributed across execution, native/JDK substitutes, allocation, and VM/host boundaries. The latest source-and-measurement audit has no safe, generic, unpriced change with a credible multi-second return under the no-adaptation/no-snapshot constraints.

**No runtime code was changed from this investigation.** That is deliberate: previously measured generic-allocation savings top out below the required CPU reduction, and the only candidate that could materially raise safe parallelism—shared execution across mutable VM state—still fails its correctness/boot gates. The practical architectural lesson is to reduce the cost of Rava's general per-operation representation and host boundary, not to imitate HotSpot's interpreter or add another allocation/dispatch micro-fast-path without a boot-sized ceiling.

## What was compared

| | Reference | Rava |
|---|---|---|
| Runtime | Debian OpenJDK 21.0.12.1, HotSpot, mixed mode, CDS sharing | Current-source PGO `release-fat`, `RAVA_JIT=1`, `RAVA_JIT_TIER0=1` |
| Workload | Minecraft 1.21.11 server, same classpath and `world_pristine` copy | Same |
| Startup to `Done` | 5.44–5.62 s | 21.2–21.8 s |
| Process CPU | 19.71 CPU-s | 37.64 CPU-s |
| Average effective cores | 3.51 | approximately 1.7 |

These are controlled real-boot measurements collected in the existing forensics and boot ledger, not a source-level estimate. JVM mode comparisons used interleaved fresh-world runs. Rava's latest PGO baseline and its A/Bs use the repository's clean environment and boot harness. See [`hotspot-startup-forensics.md`](hotspot-startup-forensics.md) §§1–2 and [`boot-speed-ledger.md`](boot-speed-ledger.md), especially rows 675, 680, 686–720.

## Layer-by-layer source audit

The nearest upstream source to the measured runtime is OpenJDK 21u tag [`jdk-21.0.12-ga`](https://github.com/openjdk/jdk21u/tree/jdk-21.0.12-ga), commit `9de4f68c88a0a1510373f291d1a95b1f6b0db8c8`. The installed binary is Debian OpenJDK `21.0.12.1+1-deb13u1`; Debian patch drift from that upstream tag was not audited. Source comparisons below are pinned to that JDK 21u tag, not current mainline.

| Layer | OpenJDK 21u path | Rava path | What the comparison says |
|---|---|---|---|
| Launch / lifecycle | `Threads::create_vm`, then `call_initPhase1/2/3` (`src/hotspot/share/runtime/threads.cpp`) | CLI `main` → `run_jar` / `run_classpath` (`crates/rava-cli/src/main.rs`, `run.rs`) → `run_main_with_args` (`crates/rava-interp/src/vm/lifecycle.rs`) | HotSpot VM creation and CDS mapping cost tens of ms. No multi-second fixed-startup advantage to port. |
| Load / link / initialize | `InstanceKlass::link_class_impl` and `initialize_impl` (`src/hotspot/share/oops/instanceKlass.cpp`); CDS archive mapping in `src/hotspot/share/cds/metaspaceShared.cpp` | loader pipeline (`crates/rava-interp/src/class_loader/loader.rs`); first-touch `Vm::ensure_init` (`vm/apply.rs`) | Rava's full ~5,000-class read/parse/link pipeline is ~240 ms. Its ~13.6 s serial prefix is a dynamic, order-sensitive `<clinit>` cascade, not a static task DAG; worker initialization fails closed. |
| Interpreter / dispatch | Generated template interpreter, TOS-specialized dispatch and safepoint polling (`src/hotspot/cpu/x86/interp_masm_x86.cpp`, `templateTable_x86.cpp`) | Rust opcode loop (`crates/rava-interp/src/opcodes/run.rs`) plus JIT (`vm/jit_background.rs`, `jit/emit/`) | HotSpot's interpreter is not a flat C++/Rust-style switch. But Rava's measured interpreter-only opcode share is ~40 ms; replacing its interpreter is the wrong boot-sized target. |
| Compilation policy | Invocation/backedge counters, MethodData, queued C1/C2 tiers and OSR; adaptive policy in `src/hotspot/share/compiler/` | Four background JIT workers by default; eager compilation policy, no OSR / profile-guided tier selection | HotSpot compiled execution is essential (`-Xint` ~56.5 s vs ~5.3 s mixed mode). C2 adds only ~0.1–0.4 s over C1 in the controlled boots. Rava's six-worker trial cuts CPU but adds ~3.5 s wall; importing more adaptive workers is not a win. |
| Object layout / field access | With compressed oops, 16-byte object header and 4-byte `int` fields; resolved offsets and typed field templates | Default 24-byte shrink-header objects, 8-byte cells, tagged values and explicit shape/null checks (`jit/emit/fields_prim.rs`) | HotSpot's compact layout and direct offsets reduce work per access. Rava's layout carries semantic and GC guarantees; prior packing/value-representation experiments did not produce a boot-sized win. |
| Allocation / GC | Both have TLAB bump allocation (`BarrierSetAssembler::tlab_allocate` on x86); G1 uses card barriers and concurrent phases | JIT inline arena cursor bump (`jit/emit/inline.rs`), generational STW collection at GIL safepoints with conservative remset coverage | The fast allocator pattern is already present in Rava. Collection itself is ~0.35 CPU-s; importing G1 barriers/queues cannot close a >7.6 CPU-s gap and does not match Rava's collector model. |
| Java libraries / native boundary | `HashMap`, `ArrayList` and other `java.base` methods are ordinary Java bytecode; C2 can inline them and perform escape analysis / scalar replacement (`src/hotspot/share/opto/macro.cpp`, `c2_globals.hpp`) | Many `java.util` classes are Rust-native implementations over synthetic fields (`natives/program.rs` → `map::map_method` / `stream::dispatch`); selected compiled/JIT fast paths exist | This is the clearest method-level difference: HotSpot can optimize through library code; a Rust native boundary is opaque to Rava's guest JIT. The opportunity is real per call, but boot value depends on the actual call population. |

The C2 allocation claim is source-verifiable: `DoEscapeAnalysis` and `EliminateAllocations` default to true in JDK 21u `c2_globals.hpp`; `PhaseMacroExpand::eliminate_allocate_node` only removes eligible non-escaping allocations and invokes scalar replacement in `macro.cpp`. This explains why temporary iterators/lists can disappear after inlining. It does **not** imply that every allocation in the benchmark is eliminated.

## Function-level experiment: Java library boundary

To test the source-level difference directly, I compiled one `bench/NativeCost.java` class once with `javac` and ran the same classfile on the installed JDK and the unprofiled Rava release binary (SHA-256 `4cab0e0407508d8c60dfc641603ed781cab805777339404bd611af2280286236`). Runs were interleaved in two JDK/Rava/Rava/JDK blocks; each arm warmed the same method then timed 1,000,000 calls. All eight runs returned the same `acc=46000000`.

| Guest operation | JDK median ns/call | Rava median ns/call | Readout |
|---|---:|---:|---|
| `HashMap.get(Object)` | 3 | 215.5 | ~72× in this microbenchmark |
| `HashMap.get(Integer)` | 1 | 224 | The alloc-free integer-key path is no faster here; crossing/dispatch cost dominates key representation. |
| `HashMap.containsKey(Object)` | 2 | 219.5 | Same native-backed map path. |
| `ArrayList.get(int)` | 0.5 | 78 | JDK readings are integer-truncated samples in the 0–3 ns range; do not treat the ratio as precise. |
| `new Object()` assigned to static field | 2 | 337 | Rava object birth is substantially more expensive in this path. |
| `new AtomicLong()` assigned to static field | 6 | 932 | Includes allocation and constructor/field initialization. |

Additional cases (`List.size`, iterator creation, local `new ArrayList` + add, and `Objects.equals`) printed 0–1 ns on JDK and tens to >1,000 ns on Rava. Those are especially susceptible to C2 inlining, loop-invariant folding, dead-store elimination and scalar replacement, and the benchmark prints integer-divided nanoseconds. Treat them as evidence of optimization across the library boundary, **not** as sound per-call prices. This was a focused diagnostic, not a JMH suite or a boot profile; it does not justify multiplying these ratios by an estimated boot count.

The source paths explain the contrast. JDK 21u `HashMap.get` delegates to `getNode` and normal Java hashing/equality; `ArrayList.get` is bounds-check plus array read. HotSpot can inline those bodies into the caller. Rava routes these APIs to native families (`crates/rava-interp/src/natives/program.rs`), where `map_method` reaches `heap.map_get` and `stream::dispatch` serves list operations over synthetic backing state. Rava also has direct JIT/fast-native arms (`jit_collections::jit_map_get`, `fu_fastpath`, and the worker `ws_arraylist_get`); this is not an absence of optimization, but those arms do not give the guest optimizer the same general ability to inline and eliminate work across the boundary.

**Candidate, not a code-change authorization:** native-boundary specialization/inlining is the only portable HotSpot lesson directly supported by this microbenchmark. I did not implement it: current boot evidence has no reliable per-method population proving that these specific calls can yield the required multi-second savings, while broad call-count × microprice estimates have repeatedly overstated boot gains. First collect a current, low-overhead method census; then test one exact receiver/site fast path against the real boot and Java semantic tests.

### Source-version caveat

The requested clone is `/tmp/openjdk-jdk`, shallow-cloned at mainline commit `1587398892ce98284c9c994b14696a96cd43e1ed`; it is not the source for the measured JDK 21 binary. The layer audit therefore uses the separate `/tmp/openjdk-jdk21u` checkout pinned above. Debian-specific source patches remain unverified; do not attribute the measured result to a post-JDK-21 mainline feature.

## Where Rava pays

- **More CPU work:** Rava uses about 1.9× the process CPU for the same boot. The current evidence describes a broad per-operation tax, not one obviously bad hot method.
- **Less effective parallelism:** OpenJDK averages about 3.5 cores and Rava about 1.7. This contributes materially to wall time, but adding compiler threads does not parallelize arbitrary guest VM work; controlled Rava tests have been neutral or slower. The shared-execution experiment still aborts the early mutable registry batch, and bypassing the write barrier fails to boot. It is not an optimization to trade away VM correctness.
- **General-purpose operation and representation costs:** Rava must preserve Java semantics across its guest-value representation, heap/arena/GC, and host-library replacements. HotSpot can specialize these layers together. Rava already has compiled paths, per-site caches, and fast-native routes; copying a quickened-interpreter or dispatch-cache shape without proving it targets a remaining population is not a meaningful improvement.

### Phase and thread attribution

The phase boundary is now precise. Against the matched clean-start markers in §2.1 and
ledger row 675, the accepted Rava PGO product spends **13.62 s / 22.91 CPU-s before
`Environment:`**, then **7.46 s / 12.32 CPU-s from `Environment:` to recipes**, and only
**0.74 s / 2.41 CPU-s from recipes to `Done`**. OpenJDK's corresponding wall intervals are
3.896 s, 1.059 s, and 0.579 s. The wall gap grows by **9.72 s before `Environment:`** and
another **6.40 s before recipes**; only 0.16 s is added after recipes. Level preparation
does not explain the startup gap.

Per-thread `/proc` CPU measurements on a fresh clean JDK run and a freshly profile-trained
Rava candidate localize most of the extra work to Rava's main runtime thread:

| Marker | JDK total CPU | JDK main | JDK C1/C2 | Rava candidate total | Rava main runtime | Rava four JIT workers |
|---|---:|---:|---:|---:|---:|---:|
| `Environment:` | 11.84 s | 3.61 s | 7.48 s | 22.56 s | 13.06 s | 9.47 s |
| `Loaded 1470 recipes` | 16.35 s | 4.21 s | 10.58 s | 35.15 s | 20.91 s | 14.20 s |

Through recipes, the Rava candidate's main runtime thread accounts for **16.70 of the
18.80 CPU-s total gap (~89%)**; its four compiler workers account for a further 3.62 CPU-s
of extra compiler work, partly offset by 1.52 CPU-s less in other threads. The interval
`Environment:`→recipes shows the same pattern: ~7.25 of the 8.08 CPU-s incremental gap is
on Rava's main runtime thread. This is the strongest causal result: the early serial
guest/runtime path is doing much more CPU work per boot than the JVM's main thread; JIT
compilation contributes, but is not the dominant source of the gap. The existing four-vs-one
worker ABBA result (ledger row 671) is consistent: four workers improve wall time, despite
using more total CPU.

**Candidate/product caveat:** the thread split above used fresh profile-use binary SHA-256
`a5e7c1b603be7ca175596fab1d2b6faedfe93675abb70be26733faa1d1cc1ac8`, not the accepted
product SHA-256 `302b1d19…` in row 718. Its `Environment:` and recipe CPU totals closely
match the accepted product's row-675 totals (22.56 vs 22.91; 35.15 vs 35.23 CPU-s), but it
crashed later in level preparation on unresolved `RoundingMode.ordinal()` before `Done`.
Use this run only to attribute the measured prefix; it cannot replace the accepted product
baseline or say anything about its tail.

Method-level attribution is still diffuse. A current-source, no-PGO symbolized diagnostic
profile stopped at recipes with 22,297 user-cycle samples and no lost samples: the largest
flat symbol was Cranelift `VCode::init` at 3.41%, followed by `VCode::run` at 2.71% and
`Vm::jit_call_body` at 2.12%; no single function reached 3.5%. This profile is a diagnostic
build, not a product timing, and its flat samples do not measure inclusive guest cost.
Likewise, the fresh JDK JFR contained only 118 `ExecutionSample` events (35 before
`Environment:`), too few to rank Java methods precisely. The exact answer is therefore:
**where:** before `Environment:` and between it and recipe completion; **which thread:**
primarily Rava's serial main runtime thread; **which individual method:** no single dominant
method has been established.

See ledger rows 721–722 for run hashes, counters, and sampling details.

The latest evidence rejects several tempting but inadequate fixes:

- Generic allocation and GC work is only about 4–5 CPU-s total, with actual GC around 0.35 CPU-s; the measured per-birth surplus has a 2.04 CPU-s ceiling. The minimum CPU reduction for the target is over 7.6 CPU-s even under an ideal six-core wall-time assumption (ledger rows 680 and 693). Allocation work is real but cannot close this gap by itself.
- Class loading, startup infrastructure, class initialization scheduling, and remaining generic algorithmic work have no safe multi-second opportunity in the current audit (rows 689 and 695–697).
- More workers and shared-state bypasses are unsafe or slower; `RAVA_LANE_NOLOCK` remains at zero of two early registry tasks and is slightly slower on current PGO (row 720).
- A new JIT/interpreter fast path should not be justified by callback counts alone. The ledger has repeatedly shown that counts multiplied by microbenchmark prices overstate real boot savings when work overlaps, misses, or is not on the critical path.

## Lessons to apply to Rava

1. **Treat the boot as the acceptance test.** A microbenchmark win is a lead, not a product win. Require an isolated mechanism, correct guest-visible behavior, and an interleaved real-boot measurement before promoting a fast path.
2. **Specialize only where the site is stable and the fallback is exact.** HotSpot's resolved/typed paths illustrate the value of removing repeated general lookup after resolution. Rava already applies this pattern in portions of its compiled dispatch; future applications must be based on a current census and retain a sound generic fallback.
3. **Measure total work and cores separately.** Here, ~1.9× CPU work and ~2× core use multiply. Optimizing only a tiny startup stage or adding workers that introduce contention cannot close the observed wall gap.
4. **Prefer a representation/boundary redesign over another pile of local probes.** Existing investigations show many cheap local optimizations are exhausted, while the large remaining cost is the general work performed per guest operation. A redesign should be staged behind a stable VM boundary and validated for Java semantic parity and end-to-end boot behavior.

## Decision

**No runtime optimization was promoted by this comparison.** The high-level lesson is adopted; no HotSpot-specific mechanism had an evidence-backed, semantics-safe path to the multi-second savings needed on the current Rava architecture. Re-open a candidate only with a current-source workload census, a bound large enough to matter, a precise correctness argument, and a controlled real-boot A/B. The <5 s objective remains unmet.

## Evidence index

- [`hotspot-startup-forensics.md`](hotspot-startup-forensics.md): host, runtime versions, startup timings, mode comparison, and caveats.
- [`boot-speed-ledger.md`](boot-speed-ledger.md): current baseline and closed/rejected optimization arcs; rows 675, 680, 686–720.
- [`the-boot-price-map.md`](the-boot-price-map.md): operation prices and population-based ceilings, including allocation no-go at lines 3193–3208 and 338–460.
- OpenJDK source: upstream JDK 21u tag `jdk-21.0.12-ga`, commit `9de4f68c88a0a1510373f291d1a95b1f6b0db8c8`; exact installed Debian source patches remain unknown. The requested mainline clone is separate and not used to explain JDK 21 behavior.
