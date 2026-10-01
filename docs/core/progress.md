# rava-core progress

## 2026-09-29 — the real JDK boots

`rava-core -cp DIR Hello` runs HotSpot's own startup sequence over the unmodified `java.base`
from the JDK image: initial thread group and thread, `System.initPhase1` (system properties,
`System.out`/`err` on `FileOutputStream`), `initPhase2` (the module system, `ModuleBootstrap`),
`initPhase3` (system class loader), then `LauncherHelper.checkAndLoadMain` loads the main class
through the JDK's `AppClassLoader` and `main(String[])` runs.

Hello World, wall clock, same container, three runs each:

| launcher | ms |
|---|---:|
| `java` (CDS on) | 44-49 |
| `java -Xshare:off` | 70-86 |
| `java -Xshare:off -Xint` | 60-74 |
| `rava-core` (interpreter only, no snapshot) | **38-59** |

The core loads 458 classes and allocates 632 KiB to reach `main`.

Interpreter speed (tier 0 only; the compiler is the next tracer):
`fib(32)` 405 ms vs HotSpot `-Xint` 143 ms; an array loop 7.7 s vs 2.2 s.

### What exists

- jimage reader (mmap), class-file parser with interned symbols
- direct object layout, bump heap (no collector yet), class loading for the boot loader and
  Java loaders (`loadClass` delegation, initiating-loader records), JVMS 5.4/5.5 linking and
  initialization, vtables and interface dispatch
- interpreter with overlaid argument frames, exceptions with real stack traces
- monitors (`synchronized`, `wait`/`notify`), Java threads as OS threads
- VM natives: Object, Class (incl. reflection members, annotations, constant pool), System,
  Thread, Unsafe, Reflection, module registry, class loaders, file system and file streams,
  system properties, signals, references

### Configuration choices (generic, not workload-specific)

- Every JDK native library is reported as built in; their natives are Rust.
- `Finalizer.isFinalizationEnabled()` is false; reference processing waits for the collector.

## 2026-09-29 — invokedynamic on the JDK's own linkage

`invokedynamic`, signature-polymorphic calls and method-handle constants link through the JDK's
`MethodHandleNatives.linkCallSite` / `linkMethod` / `linkMethodHandleConstant` up-calls, as in
HotSpot; the VM implements `MemberName` resolution, `invokeBasic` / `linkTo*`, hidden classes
(`defineClass0`) and dynamic constants. String concatenation (`StringConcatFactory`), lambdas
(`LambdaMetafactory` spinning hidden classes), method references, records
(`ObjectMethods.bootstrap`) and streams produce HotSpot's output.

`bench/BootLike.java` runs to the correct hash (`H=189720856431`) interpreter-only in 35.4 s
(HotSpot `-Xint` 14.2 s, adaptive HotSpot 0.5 s): the interpreter is correct; speed comes next.

## 2026-09-29 — baseline compiler (tier 1)

A template compiler emits x86-64 directly (own encoder, `jit/asm.rs`, checked against GNU
`as`). Compiled code keeps the interpreter's frame layout — locals, a 4-slot linked header
(method, bci, caller, saved sp) and the operand stack in value-stack memory — so:

- any instruction can be deferred to the interpreter on the same frame (`jit_interp_one`);
  every slow path (null, bounds, class init, unresolved constant, failed cast) re-executes the
  instruction there and gets exact semantics and exception messages;
- exceptions dispatch through the shared handler search; stack traces walk one frame chain;
- on-stack replacement is a jump into the compiled body at a loop header.

Compiled natively: loads/stores, int/long/float/double arithmetic and conversions, branches,
switches, fields, statics (with init checks), arrays, `new`/`newarray`/`anewarray`, type checks
with per-site caches, `invoke*` with vtable dispatch and monomorphic inline caches, per-class
lock-free itables, linked `invokedynamic`, `invokeBasic` and `linkTo*`.
Policy: compile after 100 invocations or 2000 loop back-edges (generic counters).

| workload | interpreter | tier 1 | HotSpot `-Xint` | HotSpot adaptive |
|---|---:|---:|---:|---:|
| `fib(32)` | 388 ms | 49 ms | 143 ms | — |
| array loop (OSR) | 7.8 s | 0.56 s | 2.2 s | — |
| `BootLike` | 35.4 s | 4.2 s | 14.2 s | 0.5 s |

Every test also passes with `RAVA_CORE_JIT_THRESHOLD=1` (the whole JDK boot compiled).

## 2026-09-29 — a real application: Maven

`mvn --version` (Apache Maven 3.9.11 launched through plexus-classworlds: 50+ jars on custom
class loaders, Guice/Sisu, SLF4J, jansi, reflection through method handles) runs on rava-core
with output identical to HotSpot's. New natives: `sun.nio.fs.UnixNativeDispatcher`,
`sun.nio.ch` file channels, `RandomAccessFile`, `java.util.zip` (inflate/deflate, CRC-32,
Adler-32), module resources through the mapped image (`NativeImageBuffer`), `Perf` counters,
process environment. Third-party JNI libraries (here jansi's) are not loaded; jansi falls back
exactly as it does when its library is absent.

Wall clock, three alternating runs, same container:

| | run 1 | run 2 | run 3 |
|---|---:|---:|---:|
| `java` (adaptive + CDS) | 214 ms | 188 ms | 186 ms |
| `rava-core` (no snapshot) | 204 ms | 152 ms | 191 ms |

## 2026-09-29 — garbage collector

Non-moving mark-sweep (`gc/`): 256 KiB pages of 44 segregated size classes up to 32 KiB,
individual mappings above; per-thread allocation caches (free list + bump range per class).
A cell is live iff its class word is non-zero, so no allocation bitmap is maintained.

- Roots: every thread's native stack (conservatively, after spilling callee-saved registers)
  and value stack, thread fields and native handles, all class statics (conservatively),
  mirrors, loaders, resolved constants, interned strings, linked call-site appendices,
  primitive mirrors and module records. The heap is traced precisely from class layouts.
- Stop-the-world at safepoints: interpreter and compiled code poll at method entry and on
  taken backward branches; monitor entry, `wait`, `sleep`, `park` (now real, with per-thread
  permits), class-initialization waits and blocking reads run in the native state.
- Natives never keep references in Rust collections: arrays are allocated first and filled in
  place (found by stress-testing Maven at a 1 MiB trigger: 291 collections, correct output).

## 2026-09-29 — a Minecraft-shaped workload: `bench/mclike/McLike.java`

Built only from the Minecraft server's open-source dependencies (log4j2, gson, guava, fastutil,
netty, jopt-simple, from Maven Central): option parsing, logging, a 1100-block registry with
guava immutable state maps and fastutil ids, 4000 JSON "recipes" decoded through codec-like
combinators, octave Perlin terrain sampling, and binding a netty NIO server. It exercises
`StackWalker` (log4j caller lookup), sockets, epoll, time zones, `java.lang.management` and
module-resource class definition (`defineClass2`), all added here.

Same hash as HotSpot (`aff5afe088deab33`). Three alternating runs:

| | wall | inside `main` |
|---|---:|---:|
| `java` (adaptive + CDS) | 1.73-1.80 s | 463-529 ms |
| `rava-core` | **1.35-1.41 s** | 894-1034 ms |

Startup, class loading and logging initialization are cheaper on rava-core; the steady-state
work inside `main` is ~2x slower than HotSpot's optimizing tiers.

### Next

1. Code quality: register-cached operand stack, cheaper calls (the profile is now flat over
   compiled Java code, led by method-handle chains).
2. Weak/soft/phantom reference processing; parallel marking; JIT allocation fast path.

## 2026-09-29 — class-loading throughput

Thin locks (CAS on the mark word; inflation only under contention or `wait`), a 32-way sharded
FxHash symbol table with an ASCII fast path, swept pages recycled with `memset` instead of
`MADV_DONTNEED` (page faults had dominated the profile), and a 64 MiB default GC trigger.

| | `java` | `rava-core` |
|---|---:|---:|
| LoadAll (15,978 classes from the McLike class path) | 2.64-2.82 s | **2.36-2.53 s** |
| McLike wall | 1.73-1.80 s | **~1.0 s** |
| McLike inside `main` | 463-529 ms | 700-750 ms |

## 2026-09-29 — the JIT grows up: register caching, inlining, method-handle folding

The baseline compiler now keeps hot locals in callee-saved registers and caches the operand stack
in scratch registers (the memory frame stays the interpreter-visible state), inlines small
statically bound and vtable-guarded callees in real (or, for hidden plumbing, unlinked) frames
without native calls, folds constants (static finals, trusted finals, `@Stable`, method-handle
forms and member names) so `invokedynamic` chains collapse into direct calls, and intrinsifies
`Math` rounding, `Unsafe` access, `Class.isInstance` and `Object.getClass`. Design and the C1
comparison: `docs/core/opt-tier.md`.

| | `java` | `rava-core` |
|---|---:|---:|
| McLike wall | 1.81-1.85 s | **0.78-0.93 s** |
| McLike inside `main` | 469-535 ms | 478-540 ms |
| BootLike (records as map keys) | 425-466 ms | 2.16-2.56 s |
| DfTree (megamorphic function trees) | 123 ms | 296 ms |
| RSA-1024 key generation (cold, noisy) | 97-171 ms | 112-634 ms |

### Next

1. Profile-guided inlining of virtual/interface calls (inline-cache state at recompilation),
   polymorphic inline caches.
2. Cheaper calls (register arguments, lighter prologues) and less stack traffic around calls.

## 2026-09-29 — the optimizing tier, and a GC trigger fix

Hot methods are now recompiled by an SSA optimizing tier (`docs/core/opt-tier.md`): graph
building with inlining (direct, vtable- and profile-guarded, method-handle chains), constant
folding, null-check elimination, linear-scan register allocation, and deoptimization to the
interpreter for guards, exceptions and unresolved constants. Two runtime fixes mattered as much:
compiled class-init checks let the initializing thread through (JVMS 5.5), and the GC trigger
counts bumped bytes instead of whole handed-out runs (Minestom went from 13 collections to 1).

| | `java` | `rava-core` |
|---|---:|---:|
| Minestom total (init + 256 chunks + start) | 1.79-1.98 s | **1.98-2.38 s** |
| Minestom chunk generation | 0.69-0.79 s | 1.11-1.31 s |
| BootLike | 563 ms (in `main`) | 1.15-1.39 s wall |
| DfTree | 99-136 ms | 237-273 ms wall |
| RSA-1024 key generation | 119-168 ms | 85-101 ms |

## 2026-09-29 — optimizing tier, continued: SSE registers, background compilation

Since the previous entry: SSE registers for float/double values, register coalescing, trivial
phi elimination, compact deoptimization metadata, background compiler threads, bimorphic
inlining from two-class inline-cache profiles with `InlineSmallCode`, synchronized methods and
monitors in optimized code, intrinsics for `compareAndSet*`, `currentThread` and constant
`allocateInstance`, and a direct native entry. Fixed on the way: exception handlers of baseline
frames after optimized code was installed, inlined frames missing from traces after a deopt,
the initializing thread failing compiled class-init checks, and `wait`/`notify` losing wakeups.

| | `java` | `rava-core` |
|---|---:|---:|
| Minestom total (init + 256 chunks + start) | 1.79-1.98 s | **1.46-1.80 s** |
| Minestom chunk generation | 0.69-0.79 s | 0.83-1.13 s |
| Minestom init | 0.97-1.18 s | **0.63-0.86 s** |
| DfTree (in `main`) | 93-102 ms | 99-149 ms |
| ParGen, 1 thread / 4 threads | 405-439 / 239-265 ms | 950-1215 / 344-375 ms |
| BootLike (in `main`) | 392-607 ms | 793-865 ms |

## 2026-09-30 — the Paper server boots to "Done"

Paper 1.21.1 (Minecraft server, fresh world) now starts on `rava-core -jar paper.jar --nogui`.
What it took, all general VM behavior: waiting for non-daemon threads and running shutdown
hooks after `main` (DestroyJavaVM); child processes (`forkAndExec`), file keys, mount entries,
xattrs and buffered copies; management beans; `UnixSystem`; headless AWT `initIDs`;
`getClassContext` and `getParameters0`; Flight Recorder event registration (classes register as
they initialize, as HotSpot's load-time transformation arranges; recording is unavailable).

Performance work driven by profiling the server:

- GC: the budget between collections is three times the live data within the maximum heap
  (`-Xmx`, else a quarter of RAM); marking runs on every core (770 ms to 145 ms on a 450 MiB
  live heap). Total GC time at start-up went from 16 s to 1.4 s.
- VM stubs for `@IntrinsicCandidate` JDK methods (`NativeRegistry::add_stub`): SHA-1/2/5
  compression, which dominated the jar hashing before bootstrap.
- Optimizing tier: class hierarchy analysis instead of vtable guards, frameless leaves, a
  bailout instead of a lost compiler thread when a compilation fails, and `goto` to the next
  instruction.

Start-up to "Done" on a 4-core cloud VM (timings vary by 30 % between runs of either VM):

| | `java` (JDK 21) | `rava-core` |
|---|---:|---:|
| Paper 1.21.1, fresh world, to "Done" | 24-44 s | 46-57 s (was 95 s at first boot) |
| Perlin noise micro (double math) | ~700 ms | ~850 ms |
| Megamorphic function tree micro | ~270 ms | ~390 ms |

### Next

1. Cheaper compiled calls: register arguments and lighter frame headers (call-heavy code is
   still ~1.5x C2).
2. Deoptimizing instead of calling on profiled guard misses (needs interpreter profiling).
3. Fewer fresh pages touched while the heap grows (page faults are expensive on this VM).


## 2026-09-30 — closing the start-up gap: profiles, speculation limits, register pressure

Measurement first: the server's `level-seed` was empty, so every run generated a different
world (villages near spawn or not) and runs differed by up to 30 %. Benchmarks now fix the seed
(`level-seed=20260930`); with it, same host: `java` 30.9 s, `rava-core` 55 s -> 51 s.

- Deoptimization: per-bytecode trap history, compile-time resolution of loaded classes' constants,
  retiring code at the deopt threshold, `multianewarray`; throw-only paths and never-taken
  branches (first-tier branch profiles) become uncommon traps; a method that deoptimizes twice is
  recompiled without profile speculation (the hottest world-generation method had been falling
  back to baseline code after four recompilations).
- Profiles: one receiver profile per call site across all compiled copies (inlined copies used
  to replace it with a cold one), and receiver profiles for virtual calls; the optimizing tier
  inlines by them.
- Register allocation: values only deoptimization states read stay out of registers (a home
  slot written at the definition); ints stay 32-bit without `movsxd` after every operation.
- Code: branch peepholes and short branches (4 % smaller).
- GC: a young collection no longer rescans every class's mirror, loader and resolved constants
  or the whole intern table (write-once roots, old after one collection); it scans statics,
  the classes defined since the last collection, and the roots noted since then (`gc/recent.rs`).

Where the world-generation thread's time goes now (`java` 12 s CPU, `rava-core` 30 s): compiled
code is ~2x C2 (spills, call overhead: prologue pushes and frame linking, larger code), GC
marking and allocation ~12 %, first touches of heap pages ~5 %.

### Next

1. Cheaper compiled calls (register arguments, no interpreter-frame linking on the fast path).
2. More allocatable registers (rcx/rdx/r11 are all scratch today) or interval splitting.
3. Escape analysis for short-lived iterators, lambdas and positions.

## 2026-09-30 — closing the start-up gap, continued: where the time really goes

Whole-process profiles (all threads, `perf` with perf maps on both sides) replaced per-thread
comparisons, which were misleading: Paper hands world-generation tasks to whichever worker is
free, so `java`'s and `rava-core`'s `Worker-Main-1` did different work. Samples over all
non-compiler threads, same run: Java methods `java` 24.8k / `rava-core` 41.8k, runtime (libjvm /
rava-core) 12.4k / 16.7k, kernel 4.1k / 11.7k, libc 0.8k / 4.0k. Compiled code (~1.5x C2, as
the micro benchmarks show) is the largest part; GC marking is on par with G1's threads.

- Compiler threads: branch relaxation shifted positions by summing over all earlier branches,
  and live sets were iterated bit by bit; both were the top compiler symbols (7k of 15k samples).
  Fixed: the server start went 48.0 -> 45.6 s.
- GC: young collections skip write-once class roots (`gc/recent.rs`), initializing stores into
  young objects need no card mark (`opt/barrier.rs`), in-page allocation refills are cheaper
  (13M refills per start, 42 % of at most two cells).
- Opt code: bounds checks folded or merged (`build_arr.rs`), loop-aware spill choices and
  live-range splitting at loops (`split.rs`); the RTree loop's accumulator and inputs now stay in
  registers.
- Measured and dropped: keeping freed large blocks committed and glibc arena tuning (kernel page
  clearing is 2.5k samples, but neither change moved system time); a fixed 128 MiB young
  generation (fewer page faults, more GC time, same wall time).

Same host now: `java` ~31 s, `rava-core` ~46-47 s (noise on this host is +-1.5 s per run).

Then the call protocol and code size (A/B of 3 runs each on the same host):

- Lazy frame anchoring: optimized code publishes `Thread::top`/`sp` only when it calls out
  (44.7 -> 43.8 s; fib -13 %).
- One `InlinedSite` descriptor per call instead of writing every inlined frame's header: a
  third of all optimized code was those stores (21.3 -> 14.3 MB; 44.2 -> 43.6 s).
- Frequency-based inlining (`opt/freq.rs`): the tier inlined up to 325 bytes at every call site,
  rare paths included (`Heightmap.update` compiled to 152 KB). As C2, only sites run at least
  0.25 times per entry get hot limits (14.3 -> 9.9 MB; 43.0 -> 41.2 s); medium compiled callees
  stay calls at other sites (-> 40.4 s).
- Compile time: liveness from per-block use/def sets, cached state reads, environment flags
  read once.

`java` ~31.5 s, `rava-core` ~40.4 s. `java` uses more CPU (C2 threads, G1) but finishes
sooner: world generation parallelizes better there (chunk workers 13 s CPU in `java`, 24 s here
before these changes).

After that (A/B as above, 3 runs each, not recorded run by run): inlining budget 8000 ->
16000 bytes (41.9 -> 40.6 s); deoptimization states keep only live locals (`opt/locals.rs`,
39.4 -> 38.6 s); a primary supers display for class subtype checks (within noise).

## 2026-10-01 — recorded A/B protocol, volatile fix, where the goal stands

**Goal (unchanged): wall < 5 s from exec to "Done", no workload adaptation, no snapshot or
cache.** Everything above measures progress against the previous build and against `java`;
none of it is near the goal yet.

From now on Paper numbers come from `bench/paper_ab.sh`: it builds each git revision in its own
worktree, alternates the builds round by round on one host (fixed Paper 1.21.1-133 jar and
`level-seed=20260930`, a fresh world every run) and keeps every run: `runs.tsv` (commit, binary
sha256, wall to "Done", the server's own Done time, user/sys CPU, milestone offsets), `env.txt`
(host, CPU, kernel, toolchains, jar and server.properties hashes) and every server log.
`bench/paper_ab_summary.py` summarizes a results directory.

First recorded session, `bench/results/20260930-235938-vm` (4 vCPU Xeon @ 2.10 GHz, 5 rounds):

| build | wall mean (median, sd) | CPU user / sys |
|---|---|---|
| 98ab2a2 (before the volatile fix) | 36.50 s (36.77, 1.02) | 42.2 / 7.4 s |
| fb1f31d (volatile fix) | 35.92 s (35.63, 0.86) | 42.5 / 6.3 s |
| `java` 21.0.12 | 25.65 s (25.31, 0.95) | 56.8 / 3.1 s |

So the gap is 1.40x, not the 1.26x quoted from unalternated runs (35.7 s against a `java` run of
28.3 s). `rava-core` spends less CPU than `java` and still finishes 10 s later. Per-thread CPU sampling
(every 2 s, one run each) shows why the core counts differ: `java`'s extra cores are C2 compiler
threads (17.6 s of CPU), while the server's own threads are slower under `rava-core` - Paper
Common Worker (one thread in both) 16.6 s against 8.3 s, Worker-Main-1 8.0 / 5.1 s, ServerMain
7.8 / 5.0 s, Server thread 6.8 / 4.8 s, and the serial `main` thread (Paperclip, before the
server starts) 2.8 / 0.5 s. So the gap is mutator speed, not idle cores.

What the goal means on this host: `java` itself spends 57 s of CPU on this start; even spread
perfectly over 4 cores that is 14 s. Under 5 s needs total work several times below HotSpot's
for the same world, which per-core JIT and GC speed alone cannot give; it is the yardstick here,
and every result is reported against it.

**Volatile fields** (a reviewer's question): volatile semantics were missing in every tier. No
volatile store was followed by a StoreLoad barrier, the optimizing tier reused loaded values
across volatile and acquire reads (VarHandle `getVolatile`/`getAcquire` included), and static
fields did not record `ACC_VOLATILE`. `bench/VolatileSem` (Dekker) found the forbidden outcome
272-1739 times in 200000 rounds; it now finds none in every mode, at no measurable cost above.
(The fix's commit message lost the word `volatile` in "Resolved::StaticField carries" to shell
quoting.)

**SHA-256 on the SHA extensions** (8645118): Paperclip hashes every library jar on `main`; the
portable compression was that thread's top native frame. `sha256rnds2`/`msg1`/`msg2` when the CPU
has them (as HotSpot's stub), and `implCompressMultiBlock0` folds all blocks in one call.
`bench/results/20261001-004141-vm` (4 rounds): 544f332 35.86 s mean (median 35.10, sd 1.87),
8645118 33.41 s (median 33.68, sd 0.81); the "Environment" milestone comes 1-2 s earlier.

**Division by constants** (6953a9f): `idiv` for constant divisors is gone from optimized code
(±1, biased shifts for powers of two, magic multiplication otherwise; `bench/OptDivSem`).
`BiomeManager.getBiome` ran 24 64-bit `idiv`s per call (`Math.floorMod(seed >> 24, 1024)`).
`bench/results/20261001-012511-vm` (4 rounds): 8645118 34.55 s mean (median 34.01, sd 1.58),
6953a9f 34.17 s (median 34.13, sd 1.89): within this host's noise.

Where the time goes now (perf, one run): the critical path is the one Paper Common Worker
thread (world generation), 16.6 s of CPU against `java`'s 8.3 s - the same as the wall gap.
Of its samples 74.5% are optimized code, 15% the runtime (GC marking 1.3 s, allocation refills
and run clearing 1.1 s), 8% the kernel (first touch of heap pages). In its optimized code about
a third of the samples sit on spill loads and stores, register moves, callee-saved pushes and
pops and `movabs`: nine allocatable registers and one interval per value (no splitting at
uses) keep loop values such as `this` on the stack (`Aquifer.computeSubstance` reloads it before
each of seven field reads).

**Heap prefaulting** (7a0d38d): a background thread keeps 64 MiB past each fresh frontier
populated (`MADV_POPULATE_WRITE`); the world-generation worker's page clearing fell from 1179 to
206 samples and the server thread's from 1108 to 219, moved to `rava-prefault`. Wall time did not
follow: `bench/results/20261001-013641-vm` (5 rounds) 6953a9f 33.17 s mean (median 32.82, sd
0.86), 7a0d38d 33.32 s (median 33.65, sd 0.91). By phase (perf with real-time stamps) the start
is: before "Preparing level" about as fast as `java` (8-9 s); then 12 s (`java` 8) where
Worker-Main-1 computes biome noise for structure placement (`DensityFunctions$ShiftedNoise.compute`
with the inlined Perlin noise, `Climate$RTree$SubTree.search`) while the Common Worker waits on it
part of the time; then 8 s (`java` 5) with the Common Worker busy throughout. Both are optimized
code speed.

**Caches across single-predecessor blocks** (c7c1214): value numbers, field values and bounds
checks now carry into a block whose only predecessor is the one left (inlined bodies, their
returns, guard paths); `ImprovedNoise.sampleAndLerp` loads and null-checks `this.p` once instead
of eight times. `bench/results/20261001-015303-vm` (5 rounds): 7a0d38d 32.01 s mean (median
31.45, sd 1.15), c7c1214 31.19 s (median 30.97, sd 0.82), faster in 4 of 5 rounds.

**Polls** (24857e1, f323beb): compiled code polls a per-thread word (`cmpb $0, 64(%r12)`)
instead of loading the global flag's address, and short counted loops (constant start, step and
bound, at most 4096 trips) poll no more on their back edges. `bench/results/20261001-021015-vm`:
c7c1214 33.73 s, f323beb 33.48 s: within noise. (The host drifts: c7c1214 measured 31.19 s an
hour earlier, which is why only alternated pairs are compared.)

**Allocation**: the GC log now prints the bytes allocated before each collection; a Paper start
allocates 10.3 GiB under `rava-core` against about 5.8 GiB of eden under `java` (55 G1 pauses,
0.8 s in all, against 53 collections and 1.5 s here). `RAVA_CORE_ALLOC_PROF` showed optimized
code calling the runtime for every array (millions of `byte[]`, `int[]`, `Object[]`,
`double[]`, `long[]`): arrays are now bumped inline (720647b; constant lengths up to 32 KiB,
variable ones up to 256 bytes). `bench/results/20261001-022958-vm` (5 rounds): f323beb 33.51 s
mean (median 33.22, sd 1.27), 720647b 32.62 s (median 32.21, sd 0.74), faster in 4 of 5.

**Holes under 256 bytes skipped** when reusing pages (3827755): about 12 million bump runs per
start had been one to fifteen cells. `bench/results/20261001-024352-vm`: within noise (720647b
33.67 s, 3827755 33.80 s). A larger young generation (`RAVA_CORE_GC_MB=384`, 27 collections
instead of 53) saves about 0.2 s of pauses and no measurable wall time.

**Where the session stands**, `bench/results/20261001-025730-vm` (5 alternated rounds):

| build | wall mean (median, sd) | CPU user / sys |
|---|---|---|
| 544f332 (start of this session) | 35.31 s (35.73, 1.07) | 42.8 / 5.4 s |
| c9f9839 (HEAD) | 32.31 s (32.37, 0.77) | 41.5 / 4.9 s |
| `java` 21.0.12 | 25.92 s (26.06, 0.61) | 57.9 / 2.9 s |

8.5% faster, 1.25x `java` (was 1.36x on this run). Against `java` by phase (one profiled run
each, CPU of the critical thread): structure-placement biome noise on Worker-Main-1 6.5 s vs
5.1 s, then the overworld spawn area on the Common Worker 8.6 s vs 4.8 s (of which ~0.9 s is GC
marking and ~0.5 s allocation slow paths; `java`'s collector runs on other cores), then the
nether 3.0 vs 1.7 s. C2 code for the same methods (dumped from a live `java` with `jcmd
Compiler.perfmap` and `/proc/<pid>/mem`) keeps loop-invariant loads and checks out of loops,
has no polls in counted loops, unrolls, and uses compressed references.

**Steady-state kernels** (`bench/mc`, the server jar's own classes): `ImprovedNoise.noise`,
`NormalNoise.getValue` and `DensityFunctions.ShiftedNoise` over `ShiftA`/`ShiftB` run as fast
as under C2; `CubicSpline$Multipoint.apply` (nested splines, a binary search through a capturing
lambda) takes 44-48 ns against 22 ns. In the start itself, the structure-placement biome search
on Worker-Main-1 takes 6.6 s against 4.1 s (perf map dumped from the live `java` mid-search): noise
4.0 vs 3.1 s, `Climate$RTree$SubTree.search` 1.6 vs 0.75 s, splines 0.5 vs 0.07 s.

Allocating rcx and rdx (3d136c2; eleven allocatable registers instead of nine) changed nothing
measurable (`bench/results/20261001-031543-vm`: 30.61 vs 30.68 s); spills are L1 hits. Then
(2071b61, 4bd7ec9, 91c3731): initializing card marks elided along single-predecessor chains,
final fields of compiled allocations read back their stored value (lambdas keep captured values
in registers), and allocation/monitor/subtype slow paths moved out of line (the spline's main
code 9.7 -> 5.5 KB, inlinable again). `bench/results/20261001-034216-vm`: 3d136c2 30.68 s,
91c3731 30.36 s: within noise.

**Bump runs from 2 KiB holes** (79cbd1a): three alternated runs suggested 30.4 -> 29.3 s; the
recorded A/B (`bench/results/20261001-040831-vm`, 5 rounds) gives 91c3731 29.29 s, 79cbd1a
29.03 s, within noise, at +0.6 GiB peak RSS (1.9 -> 2.5 GiB). Kept for now; revisit with
background clearing of reused pages.

Where the world-generation worker's time goes now (whole start, perf): 17.1 s of CPU against
`java`'s 11.0 s. About 7% allocation slow paths (refill, clearing runs), 6% GC marking during
pauses, 1% interface type checks through the runtime; in its optimized code about 13% is the
call protocol (callee-saved pushes and pops, frame-slot traffic, thread-word updates). Megamorphic
calls cost 4-6 ns against C2's 2.5-3 ns (`CallBench`): arguments go through frame slots, the
caller publishes its frame (BCI, `Thread::top`) and checks for a pending exception after each
call, the callee writes METHOD/BCI/CALLER and polls on entry. The rest is the computation
itself, where per-kernel code quality is at or near C2's.

**If-conversion and loop-invariant motion** (40b4d8d, d418f07, fix c5641b9): small branches
become `cmp`/`cmov` (`select.rs`) and loop invariants move to preheaders (`licm.rs`). On the
climate R-tree kernel (`bench/mc/RTreeBench`) 6.2 -> 3.6 us per lookup (`java` 2.9 us). The
first LICM commit hoisted values out of nested loops ahead of their inputs and crashed Paper's
start (an `ArrayIndexOutOfBoundsException` in `Aquifer.computeSubstance`); the A/B that caught it
hung on the failed server and was discarded (`paper_ab.sh` now gives up after `RUN_LIMIT`
seconds and records FAILED). After the fix, `bench/results/20261001-051339-vm`: 79cbd1a 30.40 s,
c5641b9 30.66 s: within noise. The R-tree is not where the start waits.

**Session checkpoint**, `bench/results/20261001-052020-vm` (4 alternated rounds, the host is
faster now than in the morning): 544f332 (session start) 32.19 s, d4c5d1e 29.65 s (-7.9%),
`java` 24.17 s; 1.23x `java` (1.33x at the session start on this run).

**New host** (from 2026-10-01 12:00; 4 vCPU Xeon @ 2.8 GHz, about twice as slow, and a VM whose
host backs guest memory lazily, so a first touch costs about 48 us per 4 KiB page; binaries
rebuilt, `target-cpu=native`). Per-method perf comparison with `java` on the world-generation
worker (`java -XX:+UnlockDiagnosticVMOptions -XX:+DumpPerfMapAtExit` under perf): 29.9k samples
against 18.9k; JIT code 24.4k vs 17.3k, runtime and libc 4.8k vs 1.1k.

- **Synchronized methods at the baseline tier** (b9d246f): they went from the interpreter
  straight to the optimizing tier with no profile, so `PalettedContainer.getAndSet(int, T)`
  (every block write) called `Palette.idFor`, `BitStorage.getAndSet` and `Palette.valueFor`
  through the itable. `bench/results/20261001-123929-vm`: 66.73 -> 64.52 s (noisy, sd 3 s).
- **Less fresh memory** (65a76f7, 392912c): freed large-object blocks are cleared on reuse
  instead of `MADV_DONTNEED` (60k faults), and `MIN_RUN` is back to 256 bytes (110k faults,
  peak RSS 2.5 -> 2.2 GiB). `bench/results/20261001-133222-vm`, 6 rounds: b9d246f 59.09 s
  (sd 3.25), 746a81d 55.81 s (sd 0.91), sys 9.4 -> 4.5 s; `java` 46.24 s: 1.21x.
- Huge pages for the heap (`MADV_HUGEPAGE`) cut faults 4x but not system time, and were slower:
  not kept.

**Steady-state world generation** (`bench/mc/WorldGenBench`: the real overworld generator, chunks
through biomes, noise and surface; checksum of the generated blocks identical to `java`'s). It
separates code quality from warm-up: at the start of the day 111.3 ms per chunk against `java`'s
62.2 (1.79x), with 14% of the time in baseline code of methods that never tiered up.

- Tier-up fixed (dc2c8ed): counters trigger from the threshold on (the interpreter counts past
  it while a method waits for compilation), and baseline code recompiled after unresolved
  deferrals no longer replaces newer optimized code. 111.3 -> 104.3 ms.
- An allocation's exact class holds everywhere (075899a) and loops keep the entry value of
  locals they never write instead of a phi (15974cb): inlined `forEach` lambdas devirtualize.
  Noise 65.0 -> 60.7 ms; total 94.2 -> 90.0 (alternated, same host state).
- Tried and dropped: clearing reused pages on a background thread. `memset` left the
  generating thread (4% -> 0.1%) but the time per chunk did not move: clearing in place also
  brought the lines into the allocating core's cache, and without it the first stores miss.
- Allocation volume: about 22 MiB per chunk (`java`'s G1 log suggests about 16): escape analysis
  with scalar replacement (C2's) would cut allocation, clearing and marking together; the
  objects that do not escape (lambdas, iterators) appear in deoptimization states, so it needs
  rematerialization on deoptimization.

### Next

1. Mutator speed of the world-generation threads (Paper Common Worker 2x), and the remaining
   `main` time (page faults on fresh memory); system time is 6 s against `java`'s 3 s.
2. Code density: ~15 bytes of optimized code per IR value (64-bit constants as `movabs`,
   inline-cache probes), the remaining call overhead (CALLER link, pending-exception check).
3. Interface subtype checks still go through `subtype_cached`.
4. A few cold JDK methods fail to compile ("a single-predecessor block entered twice": an
   edge the predecessor count misses on exception-heavy code).
