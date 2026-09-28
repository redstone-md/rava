# HotSpot startup forensics — how the reference JVM reaches `Done` in 5.4–5.7 s

Written 2026-09-26 by the HotSpotForensicsWriter agent. Diagnostic-only document; **no source,
config, benchmark, or runtime state was changed.** Every claim is labelled [MEASURED],
[INFERRED], or [UNKNOWN]. Measured-vs-inferred table in §8; attribution gaps and the next
measurement plan in §9–§10.

## 0. Goal statement (verbatim, preserved)

- User objective (docs/HANDOFF-2026-09-10.md:9): *"drive a real, clean, full-process Minecraft
  boot below **5 s WALL**, with JIT as the focus, no snapshots/checkpoints, and no
  workload-adaptive routing."*
- roadmap.md:115 / docs/deadline-status-2026-09-05.md: *"Keep wall <5s unproven until measured;
  neither snapshots nor timing/callback history may select production execution"* — full
  process-to-`Done` below five seconds, no-adapt and no snapshot speed path.
- **No goal completion is claimed by this document.** Current Rava PGO product measured
  21.2–21.8 s launch→`Done` on this box (ledger rows 668, 675, 686: 21.2–21.82 s, 37.64 CPU-s),
  i.e. still >4× the target and ~4× the JVM reference measured here.

## 1. Environment, versions, and provenance of every number

| Item | Value | Source |
|---|---|---|
| Host OS | Debian, kernel 6.12.105+deb13-amd64, x86-64 | host uname / workstation context |
| CPU | AMD Ryzen 5 3600X, 6 cores / 12 threads, 4.4 GHz max, L2 3 MB, THP=always, cpufreq governor=powersave | host lscpu, /sys checks [MEASURED 2026-09-26] |
| RAM | 31 GB (≈14.7 GB available at measurement time) | host /proc/meminfo [MEASURED] |
| JDK (reference) | OpenJDK **21.0.12.1** build `21.0.12.1+1-1-deb13u1-Debian`, HotSpot, mixed mode, **sharing** | `java -version` [MEASURED 2026-09-26] |
| Rava binary | current-source PGO `release-fat`, canonical `env -i RAVA_JIT=1 RAVA_JIT_TIER0=1 RAVA_HARD_MB=3584` baseline, no snapshots/prepared-AOT | ledger rows 668–713 [MEASURED] |
| Server jar | `versions/1.21.11_unobfuscated/server-1.21.11_unobfuscated.jar` (23 MB) + 39 dependency jars, 66 MB classpath total, 29,760 effective classes (`bench/mc-cp.txt`, 40 entries) | classpath census (price-map row, bc908a7e) [MEASURED] |
| World | copy of `world_pristine` (2.3 MB) restored per run, benchmark port 25599 | bench harnesses [MEASURED] |

Source citations use the matching upstream update release **jdk21u tag `jdk-21.0.6-ga`**
(commit `7069f193f1f8c61869fc68a36c17f3a9a7b7b2a0`), the newest jdk21u GA tag retrievable via
the GitHub API (21.0.7–21.0.12 update tags were not exposed as git tags by the API at
citation time; the Debian build string is 21.0.12). Tiering/interpreter/CDS code between
21.0.6 and 21.0.12 in these areas is believed unchanged, but per-line drift is **[UNKNOWN]
until diffed**; the exact-command check `curl -s <url>` for each cited file returns 200.

Repo evidence lives in (paths under the repo root):
`docs/boot-speed-ledger.md` (rows cited by number), `docs/the-boot-price-map.md` (sections
cited by line), `bench/jvm_modes.ps1` + `bench/jvmmode_*.log`, `bench/jvm_classload.ps1` +
`bench/jvm_cl_classes.txt`, `bench/jvm_comp.txt`, `bench/jvm_cpu.ps1`, `bench/jvm_boot.log`,
`bench/boot_ab_flags.sh`, `bench/BootPhaseC.java`, `bench/mc-cp.txt`.

## 2. The reference measurement, end to end

### 2.1 Parent-run baseline on THIS host (primary evidence)

Fresh, uninstrumented, isolated `world_pristine` copies, exact command
`java -Xmx3584m -cp <bench/mc-cp.txt absolute> net.minecraft.server.Main --nogui`
(main agent, 2026-09-26; each run its own process, no snapshots):

| run | wall to `Done` | note |
|---|---|---|
| 1 | **5.44 s** | plain baseline |
| 2 | **5.53 s** | clean repeat |
| 3 | **5.62 s** | clean repeat |

Single additional samples superseded by the controlled experiment in §5.1.1 (4 interleaved
ABBA blocks, 8 boots per arm, fresh `world_pristine` copy + eula per run, all reached `Done`):
`-XX:TieredStopAtLevel=1` (C1 only) 5.49 s; `-Xint` 56.51 s.

Clean-run milestone decomposition (wall from process start; /proc process CPU at marker):

| milestone | wall | process CPU | implied avg cores |
|---|---|---|---|
| `Environment:` | 3.896 s | 12.09 s | 3.10 |
| `Loaded 1470 recipes` | 4.955 s | 17.11 s | 3.45 |
| `Preparing level` | 5.246 s | 17.95 s | 3.42 |
| `Done (…)` | 5.534 s | 19.71 s | 3.51 |

Reading: the JVM does the whole boot in **19.7 CPU-seconds**, of which **12.1 CPU-s are
already burned by the `Environment:` line** (bootstrap + classpath/registry/<clinit> work,
mostly before guest-visible progress). Rava's same-split measurement on this box (row 675):
launch→`Environment:` 13.62 s wall / 22.91 CPU-s; `Environment:`→recipes +12.32 CPU-s;
`Preparing level`→`Done` 0.29 s wall / 1.11 CPU-s; total 21.82 s wall / **37.64 CPU-s**.

⇒ **The honest gap on this host is ~1.9× total CPU work (37.64 vs 19.71 CPU-s) times ~2.0×
effective core deficit (3.51 vs ≈1.7 avg cores, rows 623/675) ≈ 3.9× wall** — consistent with
the observed 21.3–21.8 s vs 5.4–5.7 s. [MEASURED on both engines, different runs/days]

### 2.2 Instrumented diagnostic runs (single samples, instrumentation-inflated — do not
treat as baselines)

- **JFR** (profile run, reached `Done` in 5.78 s): at ~5.51 s, **9,631 compilations**, ~12.1 s
  aggregate compiler CPU across **4 compiler threads**, **79 OSR compilations**. JFR standard
  profile recorded only 19 >threshold compilation events (mostly C2) — that count is a
  threshold artifact, not the C2 population.
- **`-Xlog:jit+compilation=debug`** (profile run, Done 5.542 s), records ≤ JVM uptime 5.5 s:
  **9,284 compilation records**: level1 660, level2 448, level3 5,680, level4 1,911
  (+585 native registrations counted inside 9,284; user-level Java comp events = 8,699).
  Named guests compiled by ~5.5 s: `ImprovedNoise.noise` (L4), `Climate$RTree$SubTree.search`
  (L4), `MultiNoiseBiomeSource.getNoiseBiome` (L3). Cuts are approximate; log continues past
  `Done` due SIGTERM shutdown.
- **`-Xlog:class+load`** (profile run), cumulative load events by uptime 1–5 s:
  4,159 / 7,360 / 10,301 / 15,165 / **19,534**; CDS-sourced ≤5 s: **1,298**; jar/directory
  sources ≤5 s: **5,946**. At 5.5 s: 21,087 events, CDS 1,300. Xlog counts class *definitions*
  (hidden classes included) and the tail includes post-`Done` activity — approximate.
- **GC** (JFR, profile run): 20 collections by recording end — 16 G1 young + 4 G1 old
  (metadata-threshold driven); **sum of pauses 115.57 ms**, longest pause 10.3 ms; event-sum
  272 ms includes concurrent old-collection time, not all stop-the-world. Some post-`Done`
  activity included; GC wall contribution on a clean 5.5 s boot is **[UNKNOWN] pending a
  clean-run JFR re-take** (ceiling: ~115 ms of pauses plus concurrent overhead, <3% of wall).

## 3. Process entry and VM bootstrap (what the first ~50–100 ms buys)

[MEASURED phase costs from `-Xlog:startuptime` on this host, `java -version`-scale process:]

```
StubRoutines initial  0.14 ms → Genesis 11.6 ms → interpreter gen 0.83 ms
→ VMThread start 0.24 ms → java.lang init 3.6 ms → module system 2.4 ms
→ Create VM total 22.8 ms
```

Mechanics, with matching jdk21u/jdk-21.0.6-ga sources:

1. **Native launcher** (`src/java.base/share/native/libjli/java.c`): `LoadMainClass`
   (java.c:508) resolves `net.minecraft.server.Main`, then `JNI_CreateJavaVM` builds the VM
   and the launcher calls `main(String[])` through JNI — no extra exec, no shell.
2. **VM init** (`src/hotspot/share/runtime/threads.cpp`): three explicit phases —
   `initPhase1` (System properties/signal handlers), then *compiler initialization before
   module init* so early Java code is compiled (threads.cpp:700-714,
   `CompileBroker::compilation_init_phase1/2`), then `initPhase2` (module system, 2.4 ms
   measured), `initPhase3`, live phase. The three-phase split exists precisely so that
   module-system code runs with the JIT already available (threads.cpp comment, ~line 295).
3. **CDS on by default**: `classes.jsa` is mapped without any flag — [MEASURED]
   `-Xlog:cds -version` on this host prints
   `trying to map /usr/lib/jvm/java-21-openjdk-amd64/lib/server/classes.jsa … Opened archive
   …` at 0.004 s, and version string says `sharing`. Source behaviour: `-Xshare` parsing with
   default/auto/on/off semantics (`arguments.cpp:2604-2619`), default archive path
   `lib/server/classes.jsa` (`arguments.cpp:3372-3387`), `init_shared_archive_paths`
   (`arguments.cpp:3430`). (Exact declaration header for the internal `UseSharedSpaces`
   static in 21.0.6-ga was **[UNKNOWN]** — I verified the runtime behaviour and the option
   parsing instead; `PrintFlagsFinal` does not list it, consistent with it being an internal
   flag rather than a `product` JVMFlag in this release.)

**Scale check vs the 5.5 s boot**: launcher + VM init + archive mapping is tens of
milliseconds (~1% of the boot). The first ~3.9 s of guest-visible work starts almost
immediately after. [MEASURED + arithmetic]

## 4. Class loading, verification, linking, initialization

- **Volume and split** [MEASURED, parent diagnostic ≤5 s]: ~19.5 k class-load events by 5 s;
  ~1.3 k served from the CDS archive; ~5.9 k read from jars/directories; remainder
  generated (lambdas/hidden classes). Repo's own census (Windows Temurin run,
  `bench/jvm_cl_classes.txt`): 22,211 load lines, 1,451 shared, 7,210 file/jar, 579
  generated — includes post-`Done` loads; used as corroboration only.
- **Pipeline** [SOURCE]: `InstanceKlass::link_class_impl` verifies → rewrites → links
  (`instanceKlass.cpp:796+`), `verify_code` → `Verifier::verify` (`instanceKlass.cpp:774`),
  initialization via `initialize_impl` running `<clinit>` under JVMS §5.5 semantics
  (`instanceKlass.cpp:1049`). CDS-shared classes are already parsed and verified at dump
  time (`cds/metaspaceShared.cpp`, `cds/archiveBuilder.cpp`).
- **Rava comparison** [MEASURED, repo]: rava's whole class pipeline (read+parse+link for
  ~5,000 classes) is **240 ms** (142+40+57; row 618), and a Forge-safe archivable slice is
  **62 ms / 0.3% of wall** (row 619). These costs alone cannot explain the ~16 s gap;
  however, JVM class-loading time was not isolated here, and load-event counts are not
  elapsed time—so the JVM-side class-loading contribution is not ruled out.
- **Class initialization is a dynamic first-touch cascade in both engines** [MEASURED, repo
  row 682]: `getstatic`/invoke trigger `<clinit>`; the JVM runs it through
  `initialize_impl`, rava through `Vm::ensure_init` (JVMS §5.5 in both). Rava's worker
  fail-closed clinit contract is the reason its parallelism stays closed there (row 682).

## 5. Interpretation, tiered compilation, and the compiler's share

### 5.1 Fixed JVM mechanics [SOURCE + MEASURED flags]

- **Interpreter with quickening** [SOURCE]: the template interpreter *rewrites* hot bytecodes
  after first execution — e.g. `getfield` patches to `_fast_igetfield` with the field offset
  baked in (`templateTable_x86.cpp:2934`, verified at tag). This is a *fixed* mechanism, not
  workload adaptation.
- **Adaptive tiering is default and IS workload-adaptive** [SOURCE]: `TieredCompilation=true`
  {pd product default} on this host; levels `CompLevel_full_profile` (C1+MDO profiling) and
  `CompLevel_full_optimization` (C2) (`compilerDefinitions.hpp:38-44`); tier thresholds
  [MEASURED `PrintFlagsFinal`]: Tier3 invocation/min-invocation/compile/backedge =
  200/100/2000/60000; Tier4 = 5000/600/15000/40000,
  `OnStackReplacePercentage=140`, `InterpreterProfilePercentage=33`,
  `ProfileInterpreter=true`. Queue-pressure scales thresholds at runtime
  (`compilationPolicy.cpp:threshold_scale`, :305). → **The reference command uses adaptive
  tiering; a no-adapt rava may not mirror it** (see §7).
- **Compiler threads** [MEASURED + SOURCE]: `CICompilerCount=4 {ergonomic}`,
  `CICompilerCountPerCPU=true {default}`, `UseDynamicNumberOfCompilerThreads=true` (this
  host); HotSpot's source formula uses integer `log2i`, not real-valued logarithms
  (`compilationPolicy.cpp:430-437`). With 12 active logical CPUs (`-Xlog:os+cpu=info`),
  `log_cpu=3`, `loglog_cpu=1`, so `max(3×1×3/2, 2)=4` exactly matches the measured count.
  Parent JFR independently shows 4 compiler threads. Dynamic growth under queue pressure
  exists (`compileBroker.cpp:possibly_add_compiler_threads`, :1069).
- **OSR** [MEASURED]: 79 OSR compilations by ~5.5 s (JFR) — on-stack replacement exists and
  fires, but is a small slice (79 of ~9.6 k).
- **Deoptimization** [MEASURED]: only 19 >threshold compilation-related events in the JFR
  standard profile (mostly C2) — no evidence of a deopt storm in the boot; full deopt count
  **[UNKNOWN]** (needs `-XX:+LogCompilation` or JFR compiler stats on a clean run).
- **C2's contribution** — see §5.1.1: modest, repeatable ~0.1-0.4 s, measured by parent ABBA.
  The decisive step is compiled execution itself: **the interpreter-only boot is ~10× slower
  (56.5 s this host; 47.89 s row 182).**

### 5.1.1 C2's actual contribution — now measured, not just inferred [MEASURED, parent]

Parent ABBA experiment on this host (4 interleaved 4-run blocks, ABBA/BAAB/ABBA/BAAB, same
clean isolated fixture/world copy per launch, all 16 boots reached `Done`):

| policy | 8 runs (s) | median |
|---|---|---|
| default (C1+C2) | 5.302, 5.248, 5.363, 5.392, 5.497, 5.403, 5.163, 5.281 | **5.333** |
| `-XX:TieredStopAtLevel=1` (C1 only) | 5.665, 5.642, 5.523, 5.577, 5.513, 5.584, 5.485, 5.573 | **5.575** |

Ranges are almost disjoint (default max 5.497 vs C1-only min 5.485 — 12 ms of overlap);
**all four block medians favour default by 0.380 / 0.173 / 0.099 / 0.307 s (+4.54% on
medians)**. This refines the older repo row 182 (Windows box, where C1-only read slightly
*faster*, 5.74 vs 6.03 s on n=3) and supersedes the single-sample impression here:

⇒ **C2 buys a modest but repeatable ~0.1–0.4 s on this workload — not the dominant factor.
The 50.9 s interpreter→compiled gain (56.5 s → 5.5 s) belongs to having C1-class compiled
execution at all.** Both policies are adaptive; C2's small win does not change the no-adapt
analysis in §7.

### 5.2 Where the JVM's 5.5 s goes [MEASURED parent milestone split + repo phase maps]

| phase (this host, JVM) | wall | CPU |
|---|---|---|
| start → `Environment:` | 3.90 s | 12.09 s |
| `Environment:` → `Loaded 1470 recipes` | 1.06 s | 5.02 s |
| → `Preparing level` → `Done` | 0.58 s | 2.60 s |

- ~70% of the CPU (12.1 s of 19.7 s) is spent *before the first guest log line* — bootstrap,
  classpath, registries, block-state `<clinit>` work. Rava's same window: 13.62 s wall /
  22.91 CPU-s (row 675). **The JVM's head start is work-rate, not a different phase list.**
- ~3.5 cores average [MEASURED] are mostly its own 4 compiler threads plus G1 concurrent
  work: repo rows 1501-1521 already proved java's extra cores are its compilers (halving
  `CICompilerCount` dropped CPU 23.0→15.9 and the wall got *faster*), and row 436 measured
  `java -Xint` at 1.15-1.22 cores. The critical path is single-threaded guest semantics in
  both engines. [MEASURED, repo]

### 5.3 What C1-compiled execution buys per operation [SOURCE, fixed mechanics]

- Direct field access with baked offsets after rewrite (§5.1), raw untagged 8-byte stack
  slots, SP-relative frames, safepoint polls as one polling-page load — versus rava's host
  seams: an interpreted call 146 vs ~22 ns (`java -Xint`, row 208/213), guest→native bridge
  28-718 ns vs HotSpot intrinsic calls (rows 173/197), explicit null checks on ≥18 emit
  sites vs implicit SIGSEGV traps (row 621). [repo MEASURED; the JVM-side mechanics are
  SOURCE-verified]

## 6. Runtime/class library intrinsics, object layout, GC

- **Intrinsics / native class library** [SOURCE + repo MEASURED]: HotSpot ships
  compiled/java-replaced implementations for `java.util` (and JMH-grade intrinsics like
  `System.arraycopy`), so a compiled caller calls real optimized code. Rava's equivalents
  cross a Rust callback seam (28-718 ns, row 173) or a synthetic field read; the "96% of
  compiled-call refusals are native-served" census (row 196) and "natives own the state"
  structural finding (row 181) describe why rava cannot simply delete the bridge.
- **Object layout / compressed oops** [MEASURED]: this host's archive records
  `UseCompressedOops = 1, UseCompressedClassPointers = 1`; `UseTLAB=true`; G1
  `G1HeapRegionSize=4 MB {ergo}`, InitialHeap 500 MB / MaxHeap 8 GB {ergo}. HotSpot objects
  are header+narrow-oop fields, one `mov` per field access. Rava's tag-discrimination and
  descriptor indirections are the measured counterpart (rows 620-621: 2.13× retired
  instructions, explicit null checks). [INFERRED mapping between the two is well-supported by
  rows 620-621, not per-opcode measured on THIS host]
- **GC** [MEASURED, JFR profile run]: G1 by default (ergonomic on this class of machine);
  20 collections by recording end, **115.6 ms summed pauses** — the direct GC wall cost on a
  clean 5.5 s boot is at most a few percent; the exact clean-run number is **[UNKNOWN]**
  (see §10). Rava's GC share on the boot is measured ~0.9% CPU (row 693, price-map
  :3193-3208). GC is not the gap for either engine.

## 7. CDS, AOT, and the no-adapt/no-snapshot compliance question

| Mechanism | Does the reference command use it? | Is it no-adapt/no-snapshot compatible for rava? |
|---|---|---|
| **Base CDS** (`classes.jsa`, JEP 310) | **YES — by default** (mapped at 0.004 s, 1.3 k classes from archive) | Yes in principle (archive keyed by on-disk bytes); but rava-equivalent priced at **0.3% / 62 ms** for the archivable slice (row 619) because rava's own class pipeline is already 240 ms (row 618). **Not a gap-driver.** |
| **Dynamic CDS / AppCDS** (JEP 350) | **NO** in the reference command — no `-XX:SharedArchiveFile`, no `-XX:ArchiveClassesAtExit`, `AutoCreateSharedArchive=false {default}` | Dynamic archives *record* prior-run state — that is exactly the snapshot path rava forbids. |
| **AOT cache (JEP 483, JDK 24)** | **Not applicable** — JDK 21 has no AOT cache; `jaotc` was removed in JDK 17 | Not applicable. |
| **Profile-guided optimization of the JIT itself** | **YES, implicitly** — the C1/C2 pipeline is *runtime* profile-guided (MDO, invocation/backedge counters) | **NO — this is adaptive.** Rava's ledger row 574/619 reasoning and HANDOFF rules forbid timing/history-selected production execution. HotSpot's tiering is by definition workload-history-driven. **The reference's advantage partly rests on an admissibility class rava excludes** — that is the honest framing, not a defect in rava. |
| **Rava engine-level PGO** (LLVM `-C profile-use`) | n/a | Explicitly allowed and shipped (rows 17/668) — static, compile-time profile, no guest adaptation. |
| **Rava CompressedOops-like width reduction** | n/a | Measured **+15.8% loss** on head7 (row 574) — closed. |

⇒ **Assessment**: of the JVM's 5.5 s, ~0.1 s is CDS; the C1/C2 compiler machinery — whose
*existence* is the dominant factor (56.5 s → 5.5 s when it is enabled) — is adaptive by
design. No proposed rava acceleration in the ledger that targets the remaining ~16 s uses
adaptive mechanisms; conversely, none of the closed candidates (CDS 0.3%, CompressedOops,
prepared-AOT row 690, CF_SUBMIT rows 623/704/705, thresholds rows 702/710) reaches the
target. [repo MEASURED]

## 8. Measured vs inferred, at a glance

| # | Claim | Status | Evidence |
|---|---|---|---|
| 1 | Plain JVM boots to `Done` in 5.44-5.62 s on this host | **MEASURED** (n=3, parent) | parent run logs |
| 2 | C1-only ≈5.49 s; `-Xint` ≈56.5 s (single samples; the C1-only figure superseded by the controlled ABBA in row 11/§5.1.1) | MEASURED, n=1 each | parent; corroborated by repo row 182 (5.74/47.89 s on Windows box) |
| 3 | JVM boot = 19.7 CPU-s, ≈3.5 avg cores; 12.1 CPU-s before `Environment:` | MEASURED | parent /proc milestones |
| 4 | Rava boot = 21.8 s wall / 37.64 CPU-s on this host; same phase split | MEASURED | row 675 |
| 5 | ~9.3-9.6 k compilations by 5.5 s; L3-heavy (5.7 k); 79 OSR; 4 compiler threads | MEASURED, single instrumented runs | parent jit+compilation/JFR logs |
| 6 | ~19.5 k class-load events ≤5 s; 1.3 k from CDS | MEASURED, diagnostic run | parent xlog; repo jvm_cl_classes.txt |
| 7 | GC ≈115 ms summed pauses by ~5.8 s | MEASURED, profile run | parent JFR |
| 8 | VM init ≈23 ms; CDS map ≈12 ms; class pipeline cost small | MEASURED (small-scale) | host -Xlog:startuptime; repo rows 617-618 |
| 9 | Interpreter getfield rewrites to `_fast_igetfield` with baked offset | SOURCE (jdk21u jdk-21.0.6-ga) | templateTable_x86.cpp:2934 |
| 10 | Tier thresholds/queue-scaling/ergo as described in §5.1 | SOURCE + MEASURED flags | compilationPolicy.cpp:230-330; PrintFlagsFinal |
| 11 | C2 buys ~0.1-0.4 s on this host (4.54% median); compiled-execution step is worth ~50 s | **MEASURED** (parent ABBA, 16 boots, near-disjoint, all 4 block medians favour default) — supersedes the older n=1 impression and qualifies row 182 | §5.1.1 |
| 12 | JVM's per-op advantage = compiled execution + cheap class library + raw slots | INFERRED (mechanism-level, multi-row) | rows 173/196/621 + source |
| 13 | Gap to 5 s requires >7.6 CPU-s reduction at ideal 6-core use | MEASURED arithmetic | row 680 |
| 14 | JVM pre-`Environment:` split: **11.84 CPU-s total; main 3.61, compiler threads 7.48, other 0.75** in a fresh clean run. The exact Java-method composition remains **UNKNOWN**: only 35 JFR execution samples precede the marker, with broad but sparse stacks. | **MEASURED thread split; method-level UNKNOWN** | New per-thread `/proc` marker capture + 118-event JFR; see comparison doc and ledger row 721 |
| 15 | HotSpot's integer-log compiler-thread formula yields 4 threads for 12 active CPUs | **SOURCE + MEASURED** | §5.1; `-Xlog:os+cpu=info`; `compilationPolicy.cpp:430-437` |
| 16 | Exact clean-run GC wall share; exact deopt count | UNKNOWN | §10 |

**Explicitly NOT claimed**: that "HotSpot intrinsically takes 5.5 s" — 5.5 s is the
*complete workload* measurement of this jar/world/box, not a property of HotSpot alone. Also
not claimed: full understanding of the gap ("no single phase is the answer" — rows 200/200's
successors; the flat-profile findings rows 322/615/685 all refuse a root-cause narrative).

## 9. Comparison of boot-phase evidence, JVM vs Rava (same jar/world/box)

| phase | JVM (this host) | Rava (row 675, this host) | ratio |
|---|---|---|---|
| launch→`Environment:` | 3.90 s / 12.09 CPU-s | 13.62 s / 22.91 CPU-s | 3.5× wall / 1.9× CPU |
| `Environment:`→recipes | 1.06 s / 5.02 CPU-s | 7.46 s / 12.32 CPU-s | 7.0× wall / 2.5× CPU |
| recipes→`Done` | 0.58 s / 2.60 CPU-s | 0.29 s / 1.11 CPU-s | rava faster |
| total | 5.53 s / 19.71 CPU-s | 21.8 s / 37.64 CPU-s | 3.9× wall / 1.9× CPU |

Older but structurally consistent cross-references (Windows box): rows 1302-1304 phase
table; row 2321 (20.0-20.7 vs 5.5-5.7, 3.6×, 4/4 ABBA); row 686 (21.44 s recheck). The
consistent picture across boxes: **the gap is uniformly distributed work-rate
(instruction-count bloat 2.13× + parallelism 0.54×, row 620/675), concentrated in the
pre-`Environment:` and registry/recipe windows, with `Done` itself (spawn area) already at
parity.**

## 10. Open attribution gaps and the reproducible command matrix

### 10.1 Open gaps

1. **JVM-side method composition of the pre-`Environment:` window** — its per-thread CPU split is now measured (fresh run: 3.61 s main, 7.48 s compiler threads, 0.75 s other), but the JFR has only 35 samples in that interval and cannot identify precise method costs. A higher-sample, low-overhead method profile remains useful only if individual Java methods are needed. (See ledger row 721.)
2. **Clean-run GC wall share and deopt counts** — all GC/JFR numbers so far come from
   instrumented runs. (Experiment P3/P6.)
3. ~~`CICompilerCount=4` versus the ergo formula~~ **RESOLVED** — integer `log2i` yields 4
   threads from 12 active CPUs (§5.1); no cgroup/distro anomaly needed.
4. **Per-opcode retired-instruction comparison on THIS host with matching runs** — row 620's
   `perf stat` predates the current binary/JDK pair. (Experiment P8.)
5. ~~Whether C1-only ≈ default holds statistically here~~ **RESOLVED by parent ABBA** —
   default is repeatably faster by ~0.1-0.4 s (§5.1.1); the open residual is only the
   profiling-vs-execution split (P6).

### 10.2 Command matrix (non-destructive; isolated `world_pristine` copies; benchmark port
25599; kill only own PIDs; never kill `java` broadly — user's own server may share the box)

Baseline / controls:
```sh
# baseline (n>=3, interleaved), each in its own world copy:
CP=$(cat bench/mc-cp.txt | tr -d '\r\n')
java -Xmx3584m -cp "$CP" net.minecraft.server.Main --nogui          # expect ~5.5 s
java -Xmx3584m -XX:TieredStopAtLevel=1 -cp "$CP" net.minecraft.server.Main --nogui
java -Xmx3584m -Xint -cp "$CP" net.minecraft.server.Main --nogui    # expect ~56 s
```

Phase/milestone timing (same as parent):
```sh
# poll for milestones with 0.2 s latency; read /proc/<pid>/stat utime+stime at markers
```

Logging matrix (each on a DISPOSABLE copy; every instrumented run is a profile, not a
baseline):
```sh
java -Xlog:startuptime -version
java -Xlog:cds -version                                  # archive mapped? which classes
java -Xlog:class+load=info:file=cls.log:time,uptime -cp "$CP" net.minecraft.server.Main --nogui
java -Xlog:jit+compilation=debug:file=jit.log:uptime -cp "$CP" …
java -Xlog:gc+start=info,gc+heap=info:file=gc.log:uptime -cp "$CP" …
java -XX:+PrintFlagsFinal -version                        # tier/GC/ergo defaults
```

JFR:
```sh
java -XX:StartFlightRecording=filename=rec.jfr,duration=60s \
     -XX:+UnlockDiagnosticVMOptions -XX:+DebugNonSafepoints \
     -cp "$CP" net.minecraft.server.Main --nogui
# read: jdk.ExecutionSample (critical path), jdk.Compilation, jdk.GarbageCollection pauses
```

Cross-engine hardware counters:
```sh
perf stat -e instructions:u,cycles:u,task-clock -p <pid>   # row-620 style, re-run on current pair
perf record -F 499 -e cycles:u -g --call-graph dwarf,8192 -p <pid>   # JVM-side callgraph
```

Rava side (existing harnesses, unchanged):
```sh
bench/boot_ab_flags.sh - 4 400                            # whole-binary ABBA
RAVA_BOOT_EXE_OFF/ON=… bench/boot_ab_flags.sh "<flag>" 4 400
```

Avoid: `-Xshare:dump` on this JDK (destructive to the system archive), killing processes by
name, `--patch-module`, or anything writing into `libraries/`/`versions/`.

### 10.3 Prioritized next measurements

1. **P1 (optional, method-level only): JVM pre-`Environment:` profile** — increase execution-sample count on a clean run, compare to the 35-sample profile, and verify the method ordering against an uninstrumented boot. The per-thread gap is already resolved; this experiment can refine methods, not the engine-level cause.
2. ~~C1-only n≥3 vs default n≥3 ABBA~~ **DONE by parent** (§5.1.1): resolved — C2 worth
   ~0.1-0.4 s. Remaining question folded into P6: how much of C1's advantage is profiling
   vs compiled execution.
3. **P3: clean-run GC pause sum + deopt count** (JFR without logging overhead, stop at
   `Done`).
4. **P4: `perf stat` instructions:u pair on the current binary + JDK 21.0.12.1** to refresh
   row 620's 2.13× with matched runs.
5. ~~P5: `-Xlog:os+cpu` + cgroup inspection~~ **RESOLVED** — integer-log arithmetic gives
   the measured compiler count (§5.1).
6. **P6: `-XX:-ProfileInterpreter` + `-XX:TieredStopAtLevel=1` combined** — bounds how much
   of C1's advantage is *profiling* (adaptive) vs *compiled execution* (mechanism), directly
   informing what a no-adapt rava may copy.

## 11. What this document deliberately does not conclude

- It does not attribute the 5.5 s to any single JVM mechanism. The measured decomposition is:
  ~70% of CPU before the first guest log line, ~3.5 avg cores of which the extra ~1.8 are
  compiler threads (repo row 436/1501), C2 worth a modest 0.1-0.4 s (§5.1.1), GC ≤~3%, CDS
  ~2%.
- It does not propose that rava adopt adaptive tiering; §7 records that the reference's
  compiler pipeline is adaptive by design and that no measured admissible candidate closes
  the remaining ~16 s (rows 676-705, 680).
- The single largest remaining unknown is the JVM-side composition of the pre-`Environment:`
  window; P1 in §10.3 is the exact experiment for it.

## Appendix A — citation index (all verified reachable 2026-09-26)

Upstream source, tag `jdk-21.0.6-ga` (commit `7069f193f1f8c61869fc68a36c17f3a9a7b7b2a0`,
https://github.com/openjdk/jdk21u/tree/jdk-21.0.6-ga):

- `src/hotspot/cpu/x86/templateTable_x86.cpp` — `_fast_igetfield` rewrite (line 2934)
  https://github.com/openjdk/jdk21u/blob/jdk-21.0.6-ga/src/hotspot/cpu/x86/templateTable_x86.cpp#L2934
- `src/hotspot/share/compiler/compilationPolicy.cpp` — per-CPU ergo (L430-437),
  split c1/c2 (L478-498), `threshold_scale` (L305)
  https://github.com/openjdk/jdk21u/blob/jdk-21.0.6-ga/src/hotspot/share/compiler/compilationPolicy.cpp
- `src/hotspot/share/compiler/compilerDefinitions.hpp` — `CompLevel` enum (L38-44)
  https://github.com/openjdk/jdk21u/blob/jdk-21.0.6-ga/src/hotspot/share/compiler/compilerDefinitions.hpp
- `src/hotspot/share/compiler/compilerDefinitions.cpp` — policy flags, legacy scaling
  (L255-300), CICompilerCount/PerCPU conflict guard (L508)
  https://github.com/openjdk/jdk21u/blob/jdk-21.0.6-ga/src/hotspot/share/compiler/compilerDefinitions.cpp
- `src/hotspot/share/compiler/compileBroker.cpp` — `init_compiler_threads` (L929),
  `possibly_add_compiler_threads` (L1069)
  https://github.com/openjdk/jdk21u/blob/jdk-21.0.6-ga/src/hotspot/share/compiler/compileBroker.cpp
- `src/hotspot/share/runtime/threads.cpp` — 3-phase init comment (L276-310), compiler init
  before module init (L700-714)
  https://github.com/openjdk/jdk21u/blob/jdk-21.0.6-ga/src/hotspot/share/runtime/threads.cpp
- `src/hotspot/share/oops/instanceKlass.cpp` — `link_class_impl` (L796), `verify_code`
  (L774), `initialize_impl` (L1049)
  https://github.com/openjdk/jdk21u/blob/jdk-21.0.6-ga/src/hotspot/share/oops/instanceKlass.cpp
- `src/hotspot/share/runtime/arguments.cpp` — `-Xshare` parsing (L2604-2619), default
  archive path (L3372-3387), `init_shared_archive_paths` (L3430), CDS disable conditions
  (L3068-3072)
  https://github.com/openjdk/jdk21u/blob/jdk-21.0.6-ga/src/hotspot/share/runtime/arguments.cpp
- `src/hotspot/share/cds/metaspaceShared.cpp`, `cds/archiveBuilder.cpp`,
  `cds/heapShared.cpp` — dump/mapping internals
- `src/java.base/share/native/libjli/java.c` — launcher `LoadMainClass` (L508)
- `src/hotspot/share/gc/g1/g1Arguments.cpp`, `gc/g1/g1CollectedHeap.cpp` (TLAB note L120-128)
- `src/hotspot/share/c1/c1_Compiler.cpp`, `src/hotspot/share/opto/c2compiler.cpp` — C1/C2
  entry points
- `src/hotspot/share/classfile/verifier.cpp`, `classfile/classFileParser.cpp`,
  `classfile/systemDictionary.cpp`

JEPs / official docs (all HTTP 200 verified): JEP 310 (base CDS)
https://openjdk.org/jeps/310 · JEP 350 (dynamic CDS) https://openjdk.org/jeps/350 ·
JEP 328 (JFR) https://openjdk.org/jeps/328 · JEP 483 (AOT cache, JDK 24 — inapplicable to 21)
https://openjdk.org/jeps/483 · java(1) manual
https://docs.oracle.com/en/java/javase/21/docs/specs/man/java.html

Repo ledger rows (docs/boot-speed-ledger.md unless noted): 182 (Xint/C1/default splits),
223/230 (per-block caveat), 2321 (row 464: 20.0-20.7 vs 5.5-5.7), 2517 (row 581 Linux perf),
2566 (row 686: 21.44 s recheck), 2573-2574 (rows 619-620 CDS/PMU), 2577 (row 623 CF_SUBMIT),
2599-2600 (rows 675-676 phase split / no-go), 2602-2612 (rows 680-705 closures),
~2636 (row 713 wide-tier). docs/the-boot-price-map.md: L1302-1304 (phase table), L1501-1521
(compiler-config table), L998-1010 (cores = compilers). bench/: jvm_modes.ps1 + jvmmode_*.log,
jvm_classload.ps1 + jvm_cl_classes.txt + jvm_cl.log, jvm_comp.txt (Windows instrumented run),
jvm_cpu.ps1, jvm_boot.log, jvm_cc2.log, boot_ab_flags.sh, BootPhaseC.java, mc-cp.txt.
