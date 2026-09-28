# External execution substrate research

**Date:** 2026-09-27
**Purpose:** evaluate whether any externally maintained execution substrate (AOT cache, code cache, profile reuse, heap snapshot, or replacement runtime) offers a source-backed, falsifiable path to a **< 5 s fresh-process boot** under the unchanged constraints: exact JVM semantics, arbitrary class loading/reflection preserved, no runtime adaptation, no snapshots, no name-based workload selection. No runtime code was changed for this note.

## The measured problem this must solve

| Fact | Measurement | Source |
|---|---|---|
| Current Rava PGO `release-fat` boot | **21.44–21.82 s wall / 37.63–37.68 CPU-s** across three documented runs | [ledger rows 669](boot-speed-ledger.md#L2629), [675](boot-speed-ledger.md#L2635), [686](boot-speed-ledger.md#L2646) |
| Requirement under ideal 6-core scaling | **> 7.6 CPU-s must be removed** | [ledger row 680](boot-speed-ledger.md#L2640) |
| Adaptive-tiering JDK 21 on the same workload | 5.34 s wall / 18.92 CPU-s (meets wall, **violates no-adapt**) | [ledger row 721](boot-speed-ledger.md#L2681), [OpenJDK comparison](openjdk-performance-comparison.md#what-was-compared) |
| Fixed HotSpot modes | `-Xint` 56.9 s; fixed first-use C2 57.7 s (miss wall target) | [ledger row 725](boot-speed-ledger.md#L2685) |
| Rava's own prepared code-cache analog | Prepared full-corpus AOT **slower**: 42.2–43.0 s / 42.2–43.0 CPU-s vs 27.2–28.3 s / 29.3–30.6 CPU-s default | [prepared-aot §Full classpath experiment](prepared-aot.md#full-classpath-experiment), [prepared-aot-semantics §Follow-up](prepared-aot-semantics.md#full-classpath-follow-up) |

The critical asymmetry: Rava's **class read/parse/link pipeline is ~240 ms**
([ledger row 618](boot-speed-ledger.md#L2572), restated in [row 676](boot-speed-ledger.md#L2636))
and **JIT compilation is ~1.02 CPU-s (3.0%)**, already off the wall critical
path ([row 703](boot-speed-ledger.md#L2663)). Substrates whose benefit is
"class loading/linking/compilation shifted ahead of time" address **≤ ~1.2
CPU-s** of a **> 7.6 CPU-s** requirement. Cache coverage must not be equated
with savings.

## Mechanism taxonomy (what each option actually is)

| Option | Class | Prior-run dependency? | Adapts at runtime? |
|---|---|---|---|
| Leyden AOT cache (JEP 483) | class **loading+linking metadata cache** | yes (training run) | yes (full JIT retained) |
| Leyden AOT method profiling (JEP 515) | **profile reuse** into the cache | yes | yes (profiles don't replace on-line profiling) |
| Leyden AOT code (JEP 544) | **AOT code cache** (C1/C2 output, GC-specific barriers) | yes | yes (deopt/reopt retained; no AOT-only mode) |
| OpenJ9 SCC + AOT | **cross-run code + profile cache** compiled *at runtime* | yes | yes (JIT re-optimizes cached AOT) |
| GraalVM Native Image | **closed-world static AOT runtime replacement** (incl. build-time-initialized heap) | no (but build-time config replaces runs) | no |
| CRaC | **heap/process snapshot** | yes (checkpoint of a warmed JVM) | n/a — excluded by constraint |
| HotSpot CDS / built-in archive | class-data sharing (metadata only) | yes | yes |

None of the options that preserve exact semantics is a heap snapshot; the snapshot class is explicitly excluded, and is listed only for completeness.

## 1. OpenJDK Leyden AOT cache, profiling, and AOT code

- **What it is:** inside HotSpot, a cache of classes in *loaded and linked* form plus (JDK 25+) method profiles and (JDK 28, [JEP 544](https://openjdk.org/jeps/544), Proposed to Target) C1/C2-compiled native code. It is **not** a heap snapshot: static initializers still run in production ([JEP 483, Motivation](https://openjdk.org/jeps/483)); JEP 544 explicitly keeps interpretation and JIT, uses slow/fast method versions to preserve class-initialization semantics, and has **no AOT-only mode**.
- **Dynamic loading/reflection:** fully compatible. The cache shifts the *timing* of load/link; reflection is unaffected ([JEP 483, Compatibility](https://openjdk.org/jeps/483#Compatibility)). Classes from **user-defined loaders are not cached** ([JEP 483, Non-Goals](https://openjdk.org/jeps/483#non-goals)) and fall back to normal loading.
- **Prior runs:** required by construction — a [training run](https://openjdk.org/jeps/483#How-to-train-your-JVM) must load/link (483), exercise profiles (515), and drive the compiled methods (544). "An AOT cache only helps insofar as the training run does similar things as production runs."
- **Runtime adaptation:** retained — [JEP 515](https://openjdk.org/jeps/515): cached profiles "do not prevent additional profiling during production runs"; [JEP 544](https://openjdk.org/jeps/544): workload changes trigger deopt/reoptimize. This mechanism does **not** deliver whole-program no-adapt execution.
- **Integration with Rava:** none directly — it is a HotSpot-internal mechanism. Rava's existing prepared AOT is the same mechanism class (code-only, no heap, content-keyed) and is **measured slower end-to-end** ([prepared-aot.md](prepared-aot.md#full-classpath-experiment)), with two semantic blockers historically fixed and "remains an investigation tool rather than a supported replacement execution mode" ([prepared-aot.md](prepared-aot.md#semantic-blockers-found-by-broader-execution)). The additionally reachable slice — class load/link (240 ms) plus JIT compile (~1.02 CPU-s) — is far below > 7.6 CPU-s.

**Assessment:** capability source-verified; performance benefit source-verified *for HotSpot's startup shape* (PetClinic 4.486 → 2.604 s; AOT code adds 65–80% startup reduction on two-core microservice-class apps, [JEP 544, Performance](https://openjdk.org/jeps/544#performance)). Those figures measure HotSpot's class-loading/JIT-ramp cost, which Rava's measured cost distribution does not share. No unmeasured performance may be imported.

## 2. Eclipse OpenJ9 AOT + shared classes cache

- **What it is:** a persistent shared-classes cache holding classes, metadata, **AOT-compiled code produced at runtime** by the JIT, and JIT profile/hints ([AOT compiler](https://eclipse.dev/openj9/docs/aot/), [class data sharing](https://eclipse.dev/openj9/docs/shrc/)). Cross-run code/profile cache, not a heap snapshot; the `snapshotCache` utility snapshots the *cache file*, not a JVM heap.
- **Dynamic loading/reflection:** fully preserved — OpenJ9 is a complete JVM; `URLClassLoader`-derived loaders share for free, custom loaders via the Helper API or `ShareOrphans` ([shrc §Support for custom class loaders](https://eclipse.dev/openj9/docs/shrc/#support-for-custom-class-loaders)).
- **Prior runs:** inherently yes — AOT code exists only after a prior run compiled it into the cache; JIT hints likewise ([shrc §AOT code and JIT data](https://eclipse.dev/openj9/docs/shrc/#aot-code-and-jit-data)).
- **Runtime adaptation:** yes — cached AOT methods "might also be optimized further by the JIT" ([AOT compiler](https://eclipse.dev/openj9/docs/aot/)).
- **Integration with Rava:** none — this is a separate VM whose cache format, AOT code, and object model are OpenJ9-internal. Adopting the mechanism would mean adopting OpenJ9 as the runtime, replacing Rava.

**Assessment:** documentation claims are qualitative ("improves start up performance"); no published figure is comparable to this workload or target. Mechanism class (runtime-generated code + profiles from prior runs) is exactly the profile/prior-run dependency the constraints exclude.

## 3. GraalVM Native Image

- **What it is:** a closed-world **static AOT runtime replacement**: the image builder analyzes reachability at build time and bakes code plus build-time-initialized static state into an image heap ([Native Image basics](https://www.graalvm.org/latest/reference-manual/native-image/basics/)). It is not a code cache; it is a different runtime.
- **Dynamic loading/reflection:** not generally preserved. Arbitrary runtime class loading from bytecode is outside the closed-world model; predefined classes are a limited, best-effort compatibility mechanism. Runtime-discovered reflection requires explicit [reachability metadata](https://www.graalvm.org/latest/reference-manual/native-image/metadata/) ([Dynamic Features of Java](https://www.graalvm.org/latest/reference-manual/native-image/dynamic-features/)). Build-time initialization is an expert feature with correctness hazards for runtime-dependent static state.
- **Prior runs:** no training runs, but the equivalent cost is build-time configuration that must statically capture everything dynamic — this is a stronger constraint than a training run, not a weaker one.
- **Runtime adaptation:** none; peak performance is fixed at build time.
- **Integration with Rava:** impossible as an integration — it would replace the substrate under evaluation. It cannot host Rava's guest JIT, and its closed-world model directly violates the arbitrary class-loading/reflection constraint.

**Assessment:** excluded by the acceptance constraints, independent of any performance number.

## 4. CRaC (Coordinated Restore at Checkpoint)

A **process/heap snapshot** of a warmed JVM restored later ([Project CRaC](https://wiki.openjdk.org/display/crac), [OpenJDK CRaC](https://openjdk.org/crac/)). Excluded twice over: it is exactly the snapshot mechanism the constraints forbid, and it changes observable semantics (checkpoint-time file descriptors, FD state, and timing become part of restored state). Listed for completeness only; no performance claim needed to reject it.

## 5. Others considered

- **HotSpot CDS/AppCDS/built-in archive:** the class-metadata ancestor of JEP 483; already part of the 5.34 s JDK baseline measured in [row 721](boot-speed-ledger.md#L2681) and the [comparison](openjdk-performance-comparison.md#what-was-compared). Nothing beyond rows 668/703 remains: Rava's metadata slice is 240 ms.
- **Graal JIT / libgraal:** a JIT compiler implementation, not an AOT substrate; does not remove the per-operation cost that dominates Rava's boot and adds adaptation, not removes it.
- **Azul Prime/ReadyNow:** profile/POD reuse from prior runs ([ReadyNow](https://www.azul.com/documentation/zing/reference-guides/readynow/)) — same prior-run-dependency class as JEP 515/OpenJ9, proprietary, and a replacement JVM. Nothing mechanically new relative to the entries above.

## Cross-check against Rava's measured cost distribution

| Candidate mechanism | Rava slice it could remove | Measured size | Verdict |
|---|---|---|---|
| Leyden 483-style class cache | class read/parse/link | ~0.24 s ([row 618](boot-speed-ledger.md#L2572), restated in [row 676](boot-speed-ledger.md#L2636)) | a small measured slice, far below the wall gap |
| Leyden 544-style AOT code cache | JIT compilation | ~1.02 CPU-s ([row 703](boot-speed-ledger.md#L2663)) | ~13% of required CPU-s; **and the full-corpus analog already measured slower** ([prepared-aot.md](prepared-aot.md#full-classpath-experiment)) |
| Leyden 515 / OpenJ9 / ReadyNow profile reuse | initial compilation sequencing | ~0 (compilation already off the wall path, [rows 531/671/672 via 676](boot-speed-ledger.md#L2636)); profile-history scheduling was **removed** under no-adapt ([no-profile-recompile.md](no-profile-recompile.md#receiver-history-no-longer-schedules-recompilation)) | excluded by constraint and by measurement |
| NI closed-world / CRaC / build-time init | the entire per-operation cost | — | excluded by constraints (dynamic loading/reflection; snapshots) |
| Adaptive-tiering JDK (reference point) | everything adaptive | 5.34 s / 18.92 CPU-s ([row 721](boot-speed-ledger.md#L2681)) | meets wall **only** by violating no-adapt; fixed HotSpot modes are ~57 s ([row 725](boot-speed-ledger.md#L2685)) |

The honest reading of the reference measurements: the workload boots in 5.34 s only when the VM is allowed to observe and adapt to it. Every substrate above either (a) keeps full adaptation (Leyden, OpenJ9) and so cannot promise no-adapt behavior, or (b) removes adaptation by construction (Native Image) and so cannot preserve arbitrary dynamic loading/reflection, or (c) is a snapshot (CRaC), excluded outright.

## Conclusion: no-go

**No candidate has a source-backed, falsifiable vertical tracer to < 5 s under the unchanged constraints.** The only replacement-runtime mechanisms that could plausibly cover the required > 7.6 CPU-s (closed-world static AOT, heap snapshots) are excluded by the acceptance; permitted cache mechanisms address a small class-loading slice (~0.24 s) or compiler slice (~1.02 CPU-s), and the code-cache class has already been built in Rava and measured **slower** end-to-end ([prepared-aot.md](prepared-aot.md#full-classpath-experiment), [prepared-aot-semantics.md](prepared-aot-semantics.md#full-classpath-follow-up)). Equating Leyden's HotSpot-relative 42–80% startup gains with a Rava win is unsupported: Rava does not have a comparable amount of class-loading/JIT-ramp work.

### What would reopen this

A reopen requires **measured evidence on this repository's canonical boot**, not a capability claim:

1. A maintained substrate or mechanism that preserves arbitrary class loading/reflection and passes the existing 30-probe semantic suite and five product gates (ChunkFill `8790f04ace726ecf`, RingScan family `37539c6c375d0c3`, NbtSave `ae44a34b42766bd4`, ReloadBench `c956377e19071d7a`) as **minimum evidence only**, plus focused tests for any changed semantics; the current gates do not prove complete Java compatibility. See [prepared-aot-semantics.md](prepared-aot-semantics.md#full-classpath-follow-up); **and**
2. Interleaved fresh-world boot pairs against the current PGO baseline (about 21.4–21.8 s wall / 37.6 CPU-s) must both reach **< 5 s** and demonstrate **> 7.6 CPU-s removed** with no wall regression relative to baseline ([row 669](boot-speed-ledger.md#L2629), [675](boot-speed-ledger.md#L2635), [680](boot-speed-ledger.md#L2640), [686](boot-speed-ledger.md#L2646)); a faster-than-historical 27–28 s default control is not sufficient.

Absent (1)+(2), external substrates remain either constraint-violating (adaptation, closed world, snapshots) or bounded far below the required CPU reduction, and the execution-substrate direction stays closed.

## Sources

Primary (all accessed 2026-09-27): [JEP 483](https://openjdk.org/jeps/483) · [JEP 514](https://openjdk.org/jeps/514) · [JEP 515](https://openjdk.org/jeps/515) · [JEP 544](https://openjdk.org/jeps/544) · [OpenJ9 AOT compiler](https://eclipse.dev/openj9/docs/aot/) · [OpenJ9 class data sharing](https://eclipse.dev/openj9/docs/shrc/) · [GraalVM Native Image basics](https://www.graalvm.org/latest/reference-manual/native-image/basics/) · [GraalVM dynamic features](https://www.graalvm.org/latest/reference-manual/native-image/dynamic-features/) · [GraalVM reachability metadata](https://www.graalvm.org/latest/reference-manual/native-image/metadata/) · [Project CRaC](https://wiki.openjdk.org/display/crac) · [openjdk.org/crac](https://openjdk.org/crac/) · [Azul ReadyNow](https://www.azul.com/documentation/zing/reference-guides/readynow/)

Repository evidence: [boot-speed-ledger.md](boot-speed-ledger.md) rows 618, 668–680, 686, 691–726 · [openjdk-performance-comparison.md](openjdk-performance-comparison.md) · [prepared-aot.md](prepared-aot.md) · [prepared-aot-semantics.md](prepared-aot-semantics.md) · [no-profile-recompile.md](no-profile-recompile.md) · [Rava Class.forName/URLClassLoader implementation](../crates/rava-interp/src/reflect/classes.rs)
