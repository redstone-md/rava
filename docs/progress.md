# rava progress

Server A/B runs cited as `bench/results/<run>` were written by `bench/paper_ab.sh` on the
host of that session; they are not kept in git.

## 2026-09-29 — the real JDK boots

`rava -cp DIR Hello` runs HotSpot's own startup sequence over the unmodified `java.base`
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
| `rava` (interpreter only, no snapshot) | **38-59** |

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

Every test also passes with `RAVA_JIT_THRESHOLD=1` (the whole JDK boot compiled).

## 2026-09-29 — a real application: Maven

`mvn --version` (Apache Maven 3.9.11 launched through plexus-classworlds: 50+ jars on custom
class loaders, Guice/Sisu, SLF4J, jansi, reflection through method handles) runs on rava
with output identical to HotSpot's. New natives: `sun.nio.fs.UnixNativeDispatcher`,
`sun.nio.ch` file channels, `RandomAccessFile`, `java.util.zip` (inflate/deflate, CRC-32,
Adler-32), module resources through the mapped image (`NativeImageBuffer`), `Perf` counters,
process environment. Third-party JNI libraries (here jansi's) are not loaded; jansi falls back
exactly as it does when its library is absent.

Wall clock, three alternating runs, same container:

| | run 1 | run 2 | run 3 |
|---|---:|---:|---:|
| `java` (adaptive + CDS) | 214 ms | 188 ms | 186 ms |
| `rava` (no snapshot) | 204 ms | 152 ms | 191 ms |

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
| `rava` | **1.35-1.41 s** | 894-1034 ms |

Startup, class loading and logging initialization are cheaper on rava; the steady-state
work inside `main` is ~2x slower than HotSpot's optimizing tiers.

### Next

1. Code quality: register-cached operand stack, cheaper calls (the profile is now flat over
   compiled Java code, led by method-handle chains).
2. Weak/soft/phantom reference processing; parallel marking; JIT allocation fast path.

## 2026-09-29 — class-loading throughput

Thin locks (CAS on the mark word; inflation only under contention or `wait`), a 32-way sharded
FxHash symbol table with an ASCII fast path, swept pages recycled with `memset` instead of
`MADV_DONTNEED` (page faults had dominated the profile), and a 64 MiB default GC trigger.

| | `java` | `rava` |
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
comparison: [opt-tier.md](opt-tier.md).

| | `java` | `rava` |
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

Hot methods are now recompiled by an SSA optimizing tier ([opt-tier.md](opt-tier.md)): graph
building with inlining (direct, vtable- and profile-guarded, method-handle chains), constant
folding, null-check elimination, linear-scan register allocation, and deoptimization to the
interpreter for guards, exceptions and unresolved constants. Two runtime fixes mattered as much:
compiled class-init checks let the initializing thread through (JVMS 5.5), and the GC trigger
counts bumped bytes instead of whole handed-out runs (Minestom went from 13 collections to 1).

| | `java` | `rava` |
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

| | `java` | `rava` |
|---|---:|---:|
| Minestom total (init + 256 chunks + start) | 1.79-1.98 s | **1.46-1.80 s** |
| Minestom chunk generation | 0.69-0.79 s | 0.83-1.13 s |
| Minestom init | 0.97-1.18 s | **0.63-0.86 s** |
| DfTree (in `main`) | 93-102 ms | 99-149 ms |
| ParGen, 1 thread / 4 threads | 405-439 / 239-265 ms | 950-1215 / 344-375 ms |
| BootLike (in `main`) | 392-607 ms | 793-865 ms |

## 2026-09-30 — the Paper server boots to "Done"

Paper 1.21.1 (Minecraft server, fresh world) now starts on `rava -jar paper.jar --nogui`.
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

| | `java` (JDK 21) | `rava` |
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
(`level-seed=20260930`); with it, same host: `java` 30.9 s, `rava` 55 s -> 51 s.

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

Where the world-generation thread's time goes now (`java` 12 s CPU, `rava` 30 s): compiled
code is ~2x C2 (spills, call overhead: prologue pushes and frame linking, larger code), GC
marking and allocation ~12 %, first touches of heap pages ~5 %.

### Next

1. Cheaper compiled calls (register arguments, no interpreter-frame linking on the fast path).
2. More allocatable registers (rcx/rdx/r11 are all scratch today) or interval splitting.
3. Escape analysis for short-lived iterators, lambdas and positions.

## 2026-09-30 — closing the start-up gap, continued: where the time really goes

Whole-process profiles (all threads, `perf` with perf maps on both sides) replaced per-thread
comparisons, which were misleading: Paper hands world-generation tasks to whichever worker is
free, so `java`'s and `rava`'s `Worker-Main-1` did different work. Samples over all
non-compiler threads, same run: Java methods `java` 24.8k / `rava` 41.8k, runtime (libjvm /
rava) 12.4k / 16.7k, kernel 4.1k / 11.7k, libc 0.8k / 4.0k. Compiled code (~1.5x C2, as
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

Same host now: `java` ~31 s, `rava` ~46-47 s (noise on this host is +-1.5 s per run).

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

`java` ~31.5 s, `rava` ~40.4 s. `java` uses more CPU (C2 threads, G1) but finishes
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
28.3 s). `rava` spends less CPU than `java` and still finishes 10 s later. Per-thread CPU sampling
(every 2 s, one run each) shows why the core counts differ: `java`'s extra cores are C2 compiler
threads (17.6 s of CPU), while the server's own threads are slower under `rava` - Paper
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
allocates 10.3 GiB under `rava` against about 5.8 GiB of eden under `java` (55 G1 pauses,
0.8 s in all, against 53 collections and 1.5 s here). `RAVA_ALLOC_PROF` showed optimized
code calling the runtime for every array (millions of `byte[]`, `int[]`, `Object[]`,
`double[]`, `long[]`): arrays are now bumped inline (720647b; constant lengths up to 32 KiB,
variable ones up to 256 bytes). `bench/results/20261001-022958-vm` (5 rounds): f323beb 33.51 s
mean (median 33.22, sd 1.27), 720647b 32.62 s (median 32.21, sd 0.74), faster in 4 of 5.

**Holes under 256 bytes skipped** when reusing pages (3827755): about 12 million bump runs per
start had been one to fifteen cells. `bench/results/20261001-024352-vm`: within noise (720647b
33.67 s, 3827755 33.80 s). A larger young generation (`RAVA_GC_MB=384`, 27 collections
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
calls cost 4-6 ns against C2's 2.5-3 ns on a megamorphic call microbenchmark: arguments go through frame slots, the
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

- guardWithTest handles inline both branches (b4d1a4b, 0205060): records' equals/hashCode
  compile to one body (`bench/RecordHashBench` 180 -> 80-95 ns, `java` 30-57); biomes 12.4 ->
  10.5 ms per chunk.

Server start, `bench/results/20261001-153755-vm` (6 rounds, noisy host: sd 3-4 s): 746a81d
60.17 s, 87ed062 57.43 s, `java` 45.10 s. Spawn-area generation (the dimensions' "Time elapsed"):
22.0 -> 20.4 s against `java`'s 13.2 (1.54x).

**Later host** (Xeon @ 2.1 GHz). Megamorphic calls: int-typed ones match `java` (2.4-2.7 ns,
`bench/java/MegaCallBench`); double results cost 5.5-6.2 ns until they came back in xmm0 (08d6d6e,
3.8-4.0 ns; `java` 3.0). Escape analysis is not a lever here: `java -XX:-DoEscapeAnalysis` runs
WorldGenBench as fast. Register allocation neither: with 3 allocatable registers instead of 11
the server start is only about 3% slower (`RAVA_OPT_FEW_REGS`, which also shook out the r11
clobbers of e0fecec).

- Major receiver at megamorphic sites (da6f3f8): WorldGenBench 49.9 -> 47.9 ms per chunk.
- Conditional moves straight into the result (4d7f33b): RTreeBench 4200 -> 3830 ns.
- Frameless leaves without r13 (a67efd7); tier-1 counters ask once every 1024 counts past the
  threshold (6cdb618).

Server start, `bench/results/20261001-184955-vm` (6 rounds): 87ed062 36.43 s, 4d7f33b 34.40 s,
`java` 28.51 s (1.21x). Spawn-area generation 12.4 s against `java`'s 8.1 (within noise of
87ed062's 12.1).

Allocation slow path in steady-state generation: about 7k refills per chunk (runs average
2.8 KiB in reused pages), about 0.3 ms per chunk above `java`'s TLAB refills.

### Next

1. Mutator speed of the world-generation threads (Paper Common Worker 2x), and the remaining
   `main` time (page faults on fresh memory); system time is 6 s against `java`'s 3 s.
2. Code density: ~15 bytes of optimized code per IR value (64-bit constants as `movabs`,
   inline-cache probes), the remaining call overhead (CALLER link, pending-exception check).
3. Interface subtype checks still go through `subtype_cached`.
4. A few cold JDK methods fail to compile ("a single-predecessor block entered twice": an
   edge the predecessor count misses on exception-heavy code).

## 2026-10-01 — one engine, the java command line

The old interpreter engine and its crates are gone; the core is the single `rava` crate and
binary. The launcher takes `java`'s command line whole (class path rules and wildcards, `-jar`,
`-m`, single source files, module options, `-ea`/`-da`, `-X`/`-XX`, `@argfiles`, the
`JAVA_TOOL_OPTIONS` family) and delegates help, version, settings and main-class resolution to
the JDK's `LauncherHelper`, so the server starts as `rava -jar paper.jar --nogui` and an
uncaught exception prints exactly as java prints it. Running `javac` for single-file programs
works unchanged.

## 2026-10-02 — scalar replacement, and what a cold boot is bound by

The optimizing tier now replaces non-escaping allocations: an object whose identity is never
observed and whose fields, after load forwarding, are read nowhere is removed from the hot path
and recorded as a virtual (`escape.rs`); on the rare deoptimization the interpreter rebuilds it
from its field values, which stay live wherever the object appears in a state (`deopt.rs`
rematerialization). A plain `New` qualifies when its construction is provably safe to defer (all
stores in one block dominating every state that mentions it, no array — those can throw, no
nested virtual). Allocation during deopt is safe because the collector does not move and the
stack is scanned conservatively: every field value is read into a root before allocating.
`OptScalarSem` exercises the deopt path (verified under GC stress and against `java`); all
world-gen checksums still match bit for bit. The captured lambda in `CubicSpline`'s binary
search is the motivating case: SplineBench 2.0x -> 1.5x of `java` (min-of-5, pinned).

It costs compile time, though, and an A/B put the cold Paper boot about a second slower with it
on (five rounds, the milestone gap growing smoothly from `env` onward — the mark of per-compile
overhead, not worse world-gen code). That is the lesson of the session: **a cold boot is not
codegen-bound.** Lowering or raising the tier-2 threshold (1000..15000) and the compiler-thread
count (1..4) moved the boot by less than the host's own ±3-5 s run-to-run noise; scalar
replacement, a genuine steady-state win, is a boot loss. The boot is bound by interpreting
not-yet-compiled code and runtime per-call bookkeeping, so the levers for it are a cheaper
interpreter or less runtime overhead, not tighter compiled code. Scalar replacement is kept on
by default (a server runs for hours; `RAVA_SCALAR_REPLACE=0` turns it off for a boot A/B).

Also: the inline cache now stores its (class, target) pair inline instead of behind a leaked
pointer, so a monomorphic hit is one compare and one load and a cache update allocates nothing.

## 2026-10-02 — where the cold-boot time actually is: the interpreter

Following the finding that a cold boot is not codegen-bound, measured the interpreter directly
(`RAVA_NO_JIT=1` vs `java -Xint`, MegaCallBench, pinned): rava's interpreter is about **2.8x
slower per call** (≈77 ns vs ≈28 ns). Virtual and interface calls cost the same in it, so the
gap is not method selection (the vtable index load and the itable scan would differ); method
resolution is already cached, the safepoint poll is a relaxed load, and the frame push writes
only its four header slots. The cost is the common call machinery and the switch-dispatch loop
itself — the architectural gap between a Rust `match` interpreter and HotSpot's hand-written
template interpreter, which threads dispatch per opcode and keeps the operand stack in registers.

So the lever for the boot — which runs much un-compiled, short-lived init code — is a faster
interpreter (a template/threaded-dispatch rewrite, or a streamlined interpreted call path), not
tighter compiled code. It is a large, correctness-critical change and wants a quiet host: this
one's run-to-run noise is ±10% even on a pinned microbenchmark, which already drowns any sub-15%
win, so measuring incremental interpreter trims here is unreliable.

## 2026-10-02 (cont.) — the cold-boot gap is world generation, and a monitor-contention trim

Rebuilt the A/B harness honestly first: `paper_ab.sh` ran every round in a fixed label order, so
whichever build was listed first paid the cold-host (page-cache, CPU-frequency) penalty every
round. It now reverses the order on even rounds, averaging the first-position bias out. With that,
an order-balanced boot A/B puts rava at ~37 s and `java` at ~31 s on this host.

The milestones localise the whole gap: up to `spawn` the two are even (rava even reaches `prep`
first — the interpreter work paid off), and every extra second is `spawn -> done`, i.e. nether and
end world generation. CPU accounting over that window: rava ~1.55 cores, `java` ~2.49 — and rava
burns *fewer* CPU-seconds there (≈22 vs ≈29), so its threads are idle, not running slower code.

Ruled out, by measurement, as the world-gen lever:
- **GC frequency.** `RAVA_GC_MB=512` cut young collections 52 -> 20 and stop-the-world 2.0 s -> 1.2 s,
  yet world-gen cores and wall did not move. Not the bottleneck.
- **Tiering thresholds.** Lowering tier-1/tier-2 (40/1500 vs 100/5000) left world-gen within noise
  and made the whole boot slightly worse (more compile competing for cores).
- **Steady-state codegen quality.** Two generic microbenchmarks representative of terrain math —
  `DfTree` (megamorphic double tree) and the new `NoiseBench` (array-heavy gradient noise over a
  permutation table) — have rava at parity-to-faster than `java` once warm (NoiseBench 24 ms vs
  27 ms). The optimizing tier is not the problem.

What is left is single-thread throughput of the mostly-serial world-gen pipeline plus the long tail
of lukewarm methods that never reach the optimizing tier and run interpreted — consistent with the
earlier "boot is interpreter-bound" finding.

One concrete, generic win landed from this: a `RAVA_PROFILE` monitor-contention readout showed
~900 inflated-monitor enters *blocking* straight onto a condition variable per boot. HotSpot spins
adaptively before parking; rava did not. Added a bounded `try_lock` spin before the wait in
`sync::enter_inflated` (a spinner never blocks the owner from releasing). Blocked enters fell
~900 -> ~250 and the world-gen window dropped about a second, order-balanced. Kept on by default.

## 2026-10-02 (cont.) — interpreter dispatch: unchecked bytecode, and the ceiling of dispatch tricks

Toward a faster interpreter (the standing lever for the boot), landed the first, safe step of a
"threaded dispatch" rewrite: read verified bytecode without bounds checks. The dispatch fetch and
every operand read indexed a `&[u8]`, bounds-checking each; a verified method's pc and operand
bytes always lie within `code` (the assumption a template interpreter is built on), so they now go
through an unchecked accessor. Measured interpreter-only (`RAVA_NO_JIT=1`, back-to-back):
dispatch-heavy `NoiseBench` 3134 -> 3019 ms (~3.7 %, consistent); call-heavy `MegaCallBench` within
noise (it is dominated by the call machinery, not the fetch). Kept.

The tracer-bullet lesson for the larger rewrite: **dispatch-mechanism changes are a single-digit
lever here.** `NoiseBench` interpreted is ~3100 ms against ~24 ms compiled — the interpreter is
~130x slower than the optimizing tier, and that ratio is set by the per-opcode work and the call
machinery, not by how the dispatch branch is threaded. On modern branch predictors the classic
switch-vs-threaded-goto gap is small, and Rust has neither computed goto nor guaranteed tail calls,
so a function-pointer/tail-threaded dispatch would trade real correctness risk for a few percent.
The higher-value interpreter work is a streamlined interpreted call path (frame setup, argument
marshalling) and getting lukewarm methods compiled, not re-threading the dispatch loop.

## 2026-10-02 (cont.) — the baseline tier was 3.6x slower than C1 on doubles; SSE stack caching

Compared the tiers directly on the array-noise microbenchmark: rava's optimizing tier matches
`java` (26 vs 27 ms) but its **baseline tier was 3.6x slower than HotSpot's C1** (131 vs 36 ms) —
and worldgen's many lukewarm methods (100..5000 calls) run at the baseline tier, which starves the
mostly-serial chunk pipeline. Root cause in `fp_arith`: the baseline register cache (`vstack.rs`)
held every operand-stack value in a GPR, so each double op did `movq`→xmm, op, `movq`→gpr. Added an
`Xmm` location to the cache so double results stay in SSE registers and chain (NoiseBench baseline
129 → 110 ms). A subtle aliasing bug caught by `OptLiveSem` on the way: the second operand is popped
first, so allocating the first operand's register must exclude it or `a + b` compiles to `a + a`
(the deepest kind of baseline bug — only shows once a value is register-cached). Semantics stay at
76 pass / 4 intentional after the fix.

Baseline float codegen is still ~3x C1 (operands still reload from the frame each op, no CSE, bounds
checks on every array access) — SSE result-chaining is the first and safest of those, landed; the
rest (keeping loaded locals/constants in xmm, array-bounds hoisting) are the follow-ups.

## 2026-10-03 — a working profiler, and what C2 actually buys on world generation

**Profiling works here.** `/usr/bin/perf` refuses this kernel, but the real binary
`/usr/lib/linux-tools-6.8.0-142/perf` runs as root (no hardware counters in this VM, so
`task-clock` sampling). With `RAVA_PERF_MAP=1` and the `release-diag` profile JIT and runtime
frames resolve; for `java`, `jcmd PID Compiler.perfmap` while it runs (or
`-XX:+DumpPerfMapAtExit`), and C2's code bytes can be read from `/proc/PID/mem`. Recording only the
world-generation window (overworld spawn milestone to `Done`) over the same seed:

- Chunk workers (Paper Common Worker): **~14 CPU-s in rava against ~8 in `java`**; the gap is
  there, not in parallelism (both runs keep the workers about one core busy — the chunk pipeline's
  dependency chain bounds it) and not on the main thread.
- Hot `java` code there is all C2; hot rava code is all the optimizing tier (the baseline tier and
  interpreter are not on this path, so the SSE stack caching above does not move the start).
- Worker time outside compiled code: rava ~14% (GC marking during pauses ~6%, allocation slow
  path ~5%, `subtype` ~1%), `java` ~3%.

**Ablating C2 on the same start** (`java` flags, world-generation wall, ~9.2 s baseline): no
escape analysis, no loop optimizations (unrolling, predication, peeling, unswitching, superword,
range-check elimination), no implicit null checks, no `@Stable` folding, no lock elimination,
no split-if — **all within noise**. `-UseTypeProfile -UseTypeSpeculation` +35 %; `-Inline`
+170 % (worse than C1 alone, which takes 21 s); compressed oops off and Serial GC: no change.
So what matters is inlining and what is done across inlined code, not loop or allocation
optimizations, object size or the collector.

**Per-method comparisons mislead**: the compilers inline differently. `BlendedNoise.compute`
looked 3.3x slower, but C2 calls `ImprovedNoise.sampleAndLerp` out of line: with it, `java`
spends 1436 samples on blended noise against rava's 830 — the optimizing tier is *faster* on this
numeric kernel (as on `NoiseBench`). The loss is in call-heavy object code.

Ruled out by measurement on the start, as changes to rava: a larger young generation, lower
tier-2 thresholds, treating every call site as hot or none as cold, a larger inlining budget.

What remains, consistent with the earlier breakdown: the **call protocol** (megamorphic call
6.5 ns against C2's 3.5 ns; per call: arguments stored to frame slots and reloaded by the
callee, BCI and frame anchor stores, four dependent loads to reach the entry, a pending-exception
check after the call), the allocation slow path and stop-the-world marking.

## 2026-10-03 (cont.) — false dependencies of scalar SSE conversions

Instruction-level profiles of the same method in both VMs (rava: `RAVA_JIT_DUMP` and perf IPs;
`java`: `jcmd Compiler.perfmap`, code read from `/proc/PID/mem`) showed `BiomeManager.getBiome`
stalling right after `cvtsi2sd`. That instruction (like `cvtss2sd`/`cvtsd2ss`, `sqrtsd` and
`roundsd`) writes only the low lane of its destination, so it waits for that register's previous
writer, here the previous loop iteration's arithmetic chain. The assembler now clears the
destination with `xorps` first (unless it is also the source), the idiom other compilers use.
An int-to-double loop went from 6.3 to 1.8 ns per iteration (`java` 4.25). On the Paper start it
is within noise (4 rounds: median 33.6 s against 33.7 s; `java` 29.2 s).

Also seen: the optimizing tier's code for `ShiftedNoise.compute` is 30 KB because it inlines the
whole noise chain, which then makes it too large to inline anywhere (C2 keeps it at 1 KB and calls
`NormalNoise.getValue`); time there is noise sampling either way, so it is not a lever by itself.

**InlineSmallCode back to C2's 2500 bytes.** The tier inlined callees with up to 7500 bytes of
optimized code, so hot methods grew to 30-70 KiB (`doFill` 71 KiB against C2's 33, with
`getInterpolatedState` and `setBlockState` inlined that C2 calls). Sweeping the limit on the start
(1200, 2500, 4000, 7500, 15000) put 2500 best; a 5-round A/B: Done median 31.65 s against
32.01 s (`java` 28.5 s). Entry costs are not the gap: the samples on the first instructions of
compiled methods are the same in both VMs (~1.3k of the workers' JIT samples).

Allocation volume is: over the start rava allocates ~12 GB against `java`'s ~6.5 GB (16-byte
headers, 8-byte references, 24-byte array base against 12/4/16 with compressed oops), so it
collects about as often while allocating twice the bytes (pauses 1.06 s against 0.81 s).

## 2026-10-03 (cont.) — where the worldgen gap is, and profile-driven fixes

**Measuring.** Per-method sample comparisons mislead (the compilers inline differently), and
1-2% changes drown in the start's ±1 s noise. Two tools helped:

- *Per-thread samples*: during the start one Paper worker thread does all world generation in
  both VMs (java 16.5k samples, rava 26k at the time), so the gap is single-thread speed of
  that worker: compiled code, allocation slow path, collector.
- *Steady state*: after `Done`, `forceload add 3000 3000 3200 3200` makes the server generate
  196 more chunks; the worker's samples there (java 28k, rava 47k) show the gap is not a
  warm-up effect, and both VMs' hot code can be dumped and compared instruction by instruction
  (`RAVA_JIT_DUMP`; for java `jcmd Compiler.perfmap` and `/proc/PID/mem`).

**What moved the start** (Done median 32.55 -> 30.66 s, java 28.27 s):

- profiling holes: a call site that had recorded no receiver when its method was compiled
  (a path cold until then, such as `Beardifier`'s junction loop) stayed an unguarded
  interface call forever; such sites now profile themselves from optimized code and ask for a
  recompilation once hot;
- speculation: after two recompilations a method was compiled without any speculation, so
  cold-then-hot paths lost exact types through merges and inlined nothing; per-bytecode trap
  limits (already kept) are enough;
- smaller code-quality items: SSE conversions without false dependencies, InlineSmallCode at
  C2's 2500 bytes, scalar replacement of objects whose fields change (iterators), branches on
  inlined boolean results, final-class `checkcast`, deoptimization-only blocks out of line,
  exact classes through boxing merges, `Class` queries on constants, `System.arraycopy`
  intrinsic, reused heap runs zeroed 4 KiB at a time.

**Measured and dropped:** a larger minimum run for reused pages, reusing only pages at most a
quarter full, a hot-site inlining budget, a smaller total budget (8000), and returning
trapping code to the profiling tier until it is hot again (HotSpot's way; at a cold start the
time back in baseline code costs what the fresher profile gains).

**Allocation volume** is about twice java's (16-byte headers, 8-byte references); the slow
path (refills of reused runs, ~140 ns each, 6 million per start) and zeroing are ~6% of the
worker. Shrinking headers would cut volume ~13%; `java -XX:-UseCompressedOops` (bigger
objects) was neutral on the start, so it is not the first lever.

## 2026-10-03 (cont.) — a steady-state benchmark, and what the hot code is made of

**Benchmark.** One process boots to `Done`, then generates five far 8x8-chunk regions in turn
(`forceload add`, wait until idle, `forceload remove all`); the CPU seconds of regions 3-5 sum
to within ±1% across runs, so changes of 1-2% in generated code show (the cold start's spread
is ±1 s). It measured this series at -3.8% CPU against `e7b52a3`, and block layout by
estimated frequency at -1.6%.

**Hot generated code by instruction class** (steady state, worker thread, samples attributed
to the instruction before each sampled IP; rava 36.4k, java 25.5k):

| class | rava | java |
|---|---|---|
| loads/stores to the heap (with ALU ops on memory) | 14.3k | 11.1k |
| branches | 6.1k | 2.6k |
| floating point | 5.8k | 5.9k |
| register moves | 3.1k | 1.0k |
| spill loads/stores | 3.2k | 3.1k |
| push/pop | 1.8k | 0.15k |
| frame (r13) accesses | 1.0k | — |

Spills and floating point match; the excess is branches (bounds checks 1.1k, unconditional
jumps 1.0k, null tests 0.9k, class guards 0.5k), register moves, and pushes/pops, which are
mostly the slow-path calls around allocation refills (6 million per start) and runtime calls.

**Dropped:** coalescing phis with their back-edge inputs when the phi is dead over the input's
interval (by block liveness): loops lose their moves (an int-array sum 3.6 -> 2.9 ms), but the
benchmark did not move, and the register sharing needed care (no eviction of a shared
register, no sharing with a non-first operand or with another phi).

## 2026-10-03 (cont.) — parity on the server's own clock

A/B (`bench/paper_ab.sh -n 4`, server `Done` time, means): `a3b47ec` 29.49 s against java 29.79
s, and `62b4ad9` 29.47 against 29.16 in the next session: within the ±0.4 s that java itself
moves between sessions. By milestone (50 ms polling, two runs each), rava reaches `Preparing
level` ~2.4 s earlier and loses ~3.5 s in world generation (stronghold placement's biome search
on `Worker-Main`, then the spawn chunks on the one `Paper Common Worker`).

**What moved it:**

- collector: card roots are collected before any marking (a young object marked by another
  worker had looked old and been rescanned), pages without old objects only have their cards
  cleaned (card scanning 890 -> 560 ms CPU per start); marked objects are prefetched through an
  8-entry FIFO ahead of the scan (largest full collection 140 -> 105 ms, pauses ~990 -> ~820 ms);
- classes live in the low 2 GiB (`MAP_32BIT`), so type guards compare against an imm32 instead
  of a 10-byte `movabs` (they were most of the `movabs` samples in hot code);
- each class caches its last itable hit (HotSpot's secondary super cache) for interface
  `checkcast` and megamorphic interface calls;
- implicit null checks: a null check right before an access of the same object emits nothing,
  and the SIGSEGV handler (`jit/fault.rs`) resumes at its deoptimization stub (24k of 33k null
  checks in a start; explicit tests had been ~6% of hot samples);
- range check elimination for counted loops (`opt/rce.rs`): `i < a.length` from a constant
  start needs no bounds check, and loop-invariant bounds become one `RangeGuard` in the
  preheader that deoptimizes to the loop-entry state;
- intrinsics: `Math.floorMod/floorDiv` by a constant power of two (the biome lookup's sign
  fix-up branch mispredicted on hashed values), `Integer/Long.bitCount`,
  `numberOfLeadingZeros/TrailingZeros` (popcnt, lzcnt, tzcnt);
- SHA-256 unrolled with BMI2 rotates (154 -> 260 MB/s): Paper's launcher hashes 94 MB of jars
  before `Main` starts, which had cost 0.37 s more than on HotSpot.

**Dropped:** prefetching the next hole of a reused page at refill (no change), allocating from
empty pages before holes (pauses 0.9 -> 1.5 s, 250 MB more touched), full collections at 3x
instead of 2x the old generation (neutral), 128-byte cards (no faster to scan).

## 2026-10-03 (cont.) — ahead of HotSpot on the server start

A/B (`bench/paper_ab.sh -n 5`, means): `1bac97a` reaches `Done` in 28.32 s against java's 28.78 s
(-1.6%), 29.38 against 29.83 s from exec (-1.5%), ahead in four rounds of five.

**What moved it since parity:**

- slow paths (allocation refills, polls, subtype and monitor calls) preserve only the registers
  live at their instruction instead of every register the method allocates (the chunk
  generator's fill loop: 1023 xmm saves and 609 pushes became 143 and 185, its code 45 -> 32
  KB);
- low memory (`crate::low`) for methods, their class-hierarchy flags and inlined-call records:
  CHA guards are one `cmpb $0, [abs32]`, constants are stored as imm32, type checks compare
  classes as immediates;
- loop rotation (header test repeated at the back edge), and loop-invariant deoptimizing tests
  (an iterator's modification count) hoisted to the preheader as guards
  (`opt/guards.rs`);
- InlineSmallCode 3500 bytes instead of C2's 2500: the same methods are larger here (every
  round of an A/B, -1.7%);
- escape analysis: an allocation's result is non-null wherever it flows, and casts and
  instanceof of exactly known classes fold, so fewer fresh objects (lambdas built through
  method handles) count as escaping.

**Fixed on the way:** passes that add values after aliases exist shifted the alias table
(`Graph::add` now keeps it whole), and predicates hoisted to a loop's entry could deoptimize to
a state naming an allocation escape analysis had removed.

**Measured and dropped:** MIN_RUN 512 (+0.4%), 32-byte stores for clearing holes (refill samples
up), INLINE_SMALL_CODE 4500 (same as 3500), young generation of 256 MB (same pauses).

**Where the rest is:** world generation's worker runs ~1.25x HotSpot's samples; allocation and
collection are ~9% of it (holes of ~300 bytes cost a refill each, mostly the cache misses of
clearing them); the remaining escapes are allocations merged by phis (C2 in JDK 21 does not
remove those either) and objects stored into others.

## 2026-10-03 (cont.) — 4% ahead, every round

A/B (`bench/paper_ab.sh -n 5`, means): `836c68b` reaches `Done` in 28.07 s against java's 29.31 s
(-4.2%), 29.11 against 30.37 s from exec (-4.2%), ahead in all five rounds.

**What moved it since 1bac97a:**

- inline-cache cells, profiling countdowns, subtype caches and class statics in low memory,
  addressed absolutely (no movabs of a cell's address on each virtual call or static access);
  casts no longer compute the subtype answer on their inline hits;
- wide constants (an object such as a lookup table, a 64-bit multiplier) used in an innermost
  loop are copied once into its preheader (`opt/consts.rs`) instead of a `movabs` per use, and a
  spilled operand of `imul` or of float arithmetic is read from memory directly;
- `a[i] & 0xff` past its mask constant is a `movzx` load (the noise permutation tables).

**Measured and dropped:** a third compiler thread (same), the first full collection at 400 MB of
old objects instead of 128 (same pauses). Deoptimizations (5.6k per start) are spread over
start-up code, at most 75 at one site; collections take ~290 ms of world generation's 21 s.

Later the same day, `42a3e5e`: `Done` in 28.11 s against java's 29.64 s (-5.2%), 29.11 against
30.72 s from exec (-5.2%), all five rounds. Since `836c68b`: float compares and int-to-float
conversions read spilled operands from memory, and a commutative operation's result may take
its dying second operand's register (the noise lerps' `a + t * (b - a)`, accumulators).

What remains in the hot code is mostly memory: the samples on class and null compares sit
right after the loads of object headers they test (cache misses on objects that are larger
here, 16-byte headers and 8-byte references against HotSpot's 12 and 4).

## 2026-10-04 — ahead on time, CPU and memory

A/B (`bench/paper_ab.sh -n 5`, means; the script now records peak and current RSS at `Done`):
`1fd9e0a` reaches `Done` in 25.92 s against java's 27.89 s (-7.1%), uses 39.5 s of user CPU
against 65.6 s (-40%) and peaks at 1070 MiB resident against 1154 MiB (-7.3%). At `9bda932` the
same start peaked at ~2.1 GiB.

**Object model:**

- compressed references: fields and `Object[]` elements hold the address >> 3 in 32 bits (the
  heap is reserved at 4 GiB, below the 32 GiB they reach); registers, frames, statics and
  natives keep full pointers (`heap::obj::{read_ref, write_ref, load_elem, store_elem}`, the
  compilers' `load_typed_to`/`encode_ref`, Unsafe split on a null base);
- a 32-bit class word (classes are in low memory); the 4 bytes after it hold an array's length
  (elements from 16) or an instance's first small fields; size classes every 8 bytes to 256.

Live data per object went from ~58 to ~40 bytes, and the smaller objects made the start faster.

**Collector:**

- young and full collections move what the trace reaches through the heap or a static out of
  allocation pages into old pages packed by the collector (`gc/evacuate.rs`); what another root
  holds stays in place, as do locked and large objects and compiled code's constants
  (`recent::embed`). Compiler threads read the heap under the collection lock. Allocation pages
  no longer turn into pages of short holes compiled code cannot bump through (they held ~300 MB
  of free cells it could not use);
- statics are precise roots whose slots follow moves;
- a young generation of a third of the live data (at least 128 MB); free large-object memory
  past 32 MB and surplus empty pages go back to the kernel; prefault 16 MB ahead.

**Metadata:** deoptimization descriptors LEB128-encoded and shared (were ~60 MB of vectors),
pool resolution caches one word per entry (were 32 bytes), symbols one pointer to arena text,
pool entries 16 bytes; classes defined from bytes keep only what the runtime reads later.

**Fixed on the way:** the main thread never ran `Thread.exit()` when `main` returned, so NIO's
per-thread temporary direct buffers (Paperclip reads whole jars through them, ~60 MB) were never
freed.

**Measured and dropped:** 64 KB pages (-40 MB, slower), runs of 64 bytes (-100 MB, 2 s slower:
a refill per short hole), releasing surplus pages after every collection (2-4 ms a pause).

## 2026-10-04 (cont.) — baseline code nearly halved

The 42-mod pack compiled 80 MB of baseline code (26k methods, ~3.1 KB each; HotSpot's whole
code cache is ~68 MB there). Per-method and per-site overhead dominated, not the templates:

- **Shared slow paths:** a deferral site (re-execute in the interpreter) was ~60 bytes; it is
  `mov ecx, bci | depth << 16; call T` with one trampoline per method and inlining context
  (`jit/trampoline.rs`).
- **Shared frame code:** the prologue (link the frame, stack check, poll, invocation counter;
  ~150 bytes), epilogue and exception dispatch (~70) are process-wide stubs
  (`jit/frame_stubs.rs`), parameterized by `max_locals | frame_slots << 16` in ecx.
- **Denser encodings:** branches relaxed to 2 bytes where in reach; r13 biased 128 bytes into
  the frame so 32 slots take 8-bit displacements; branch-profile counters in low memory (one
  `add [abs32], 1`); the code cache reserved just below rava's text so runtime calls are
  `call rel32` (resolved at install) instead of `movabs` + `call rax`.
- **Less code:** entry stubs only at handlers and loop headers (every block had one); no null
  check on `this` (local 0 of an instance method that never writes it, inlined bodies included),
  new objects and their `dup`s; field and array templates write the operand stack back only on
  their slow path.

| | Paper b1 | pack b1 | pack code total |
|---|---|---|---|
| before | 45.5 MB | 80.5 MB | 107 MB |
| after | 25.2 MB | 44.7 MB | 68 MB |

Boot time and CPU are unchanged on Paper (41 s CPU either way); the pack's CPU fell 95.6 → ~91 s.
HashMap.putVal: 9338 → 4352 bytes for 300 bytes of bytecode.

A/B on the 42-mod NeoForge pack (`bench/paper_ab.sh -n 3 -b <pack>`, `ed26678` against java,
means): wall to `Done` 45.8 s against 51.1 s (-10%), user+sys CPU 91.1 s against 146.1 s (-38%),
peak RSS 1800 MiB against 1918 MiB (-6%; java ranged 1823-2058). The server's own `Done` clock
is still behind, 25.7 s against 22.9 s: world generation runs hotter code in C2.

## 2026-10-04 (cont.) — threads: hand-offs, monitors, and what bounds world generation

**What a JVM can and cannot do for a server's threads.** It cannot run one thread's code on
several cores: the Java memory model and the order of side effects forbid it, and no safe
transformation of arbitrary code does it. What it can do is make the threads the application
already has cost less: cheap hand-offs between them, monitors that do not serialize on the VM,
no VM work on their critical path.

**Where world generation stands.** On this 4-CPU host, during the 42-mod pack's spawn
preparation java keeps 1.4 CPUs busy on average and its three `Worker-Main` threads 0.75 of
them; rava keeps 2.0 busy. The chunk pipeline is bound by its dependencies and by the speed of
the code on its critical path, not by cores.

**Hand-offs (`parker.rs`).** `LockSupport.park` blocked at once and `unpark` always issued a
futex wake (a system call and, in a VM, an inter-processor interrupt). The permit is now an
atomic, an untimed park spins for it within an adaptive 1-20 µs budget, and unpark wakes only a
blocked owner. Timed parks do not spin: short timeouts are polling loops, and spinning in them
kept Paper's server thread busy for 7 s of CPU. Ping-pong through park/unpark: 36 → 0.4 µs a
round trip (java 38); a CompletableFuture hop between two executors: 19 → 1.2 µs (java 21).

**Monitors (`sync/monitor.rs`).** An inflated monitor took a sharded map lookup, an Arc clone and
a mutex on every enter and exit, and woke the entry condition on every exit. The owner is now an
atomic claimed by CAS, contenders spin adaptively (0.5-20 µs) before blocking, an exit wakes an
entrant only if one is blocked, waiters spin for their notification with the lock free, and each
thread caches monitor lookups. Four threads on one monitor: 1650 → ~350 ns an operation;
wait/notify ping-pong: 37 → 1.9 µs (java 40), with 140x less system time.

**The critical path's code.** The climate R-tree search (`Climate$RTree`, here biolith's
`searchTreeGet`) was 18% of world generation. Two optimizer gaps made it slower than C2 even
with the tree in cache:

- `licm.rs` hoisted field loads only of objects known non-null at the preheader; the node's
  first null check was inside the loop. Loop-invariant null checks that run on every iteration
  are now predicated into one guard before the loop (deoptimizing to the loop's entry state), so
  `node.parameterSpace` and its bounds check leave the loop.
- `select.rs` did not convert nested conditionals (`a > 0 ? a * a : b > 0 ? b * b : 0` merges
  three paths into one block). A diamond into a wider merge now folds into one edge with a
  select per differing value, repeatedly, so both branches become conditional moves.

Tree search microbenchmark: 1347 → 592 ns per search with the tree in cache (java 1071), 3089 →
1258 ns on a 56k-node tree (java 2440).

A/B on the pack after these changes (`eb57764` against java, 3 rounds, means): wall to `Done`
44.9 s against 51.0 s (-12%), user+sys CPU 89.1 s against 143.7 s (-38%), peak RSS 1798 MiB
against 2039 MiB (-12%). The server's `Done` clock: 25.1 s against 23.1 s (was 25.7 against
22.9).

## 2026-10-04 (cont.) — many cores: the collector's helpers

Measurements on real Minecraft code first, to find where a faster machine would leave rava
behind. Steady-state code is not it: the spawn biome search (`Climate.findSpawnPosition` with
vanilla's registries) runs a round in 11-16 ms on both VMs; `BlendedNoise.compute`, monomorphic
or behind four `FunctionContext` classes, is 1.05-1.3 µs on rava against 1.2-1.4 µs on C2; the
overworld's final density and climate sampling are within 10%, and scale from one to four
threads like C2's. During Paper's start-region phase one `Paper Common Worker` thread generates
every chunk on both VMs and is the critical path. (Paper's `PalettedContainer.getAndSet` is
`synchronized`; C2 is faster on a tight loop of such calls only because it unrolls the loop and
coarsens the locks, which world generation's calls, far apart, do not allow.)

What grew with the core count was the collector:

- **Spinning markers** (`gc/mark.rs`). A marker out of work spun until all were idle. When
  markers outnumber free cores, or share one as SMT siblings, the spinners took CPU time from the
  markers that still had work. They now spin for 1024 checks and then yield, as HotSpot's task
  terminator does. Paper with 12 processors on 4 cores: young pause 39.5 → 15.6 ms on average,
  `Done` 13.4 → 11.9 s.
- **Helpers spawned per collection** (`gc/pool.rs`). The marking helpers were new threads every
  collection, so creation and start-up sat inside each pause, once per helper. They are now
  started once and parked, named `rava-gc-N`; they used to inherit the collecting thread's name,
  which put a share of GC time into `Paper Common Worker` in profiles.
- **Release after young collections** (`gc/mod.rs`). Free pages beyond the next period's budget
  went back to the kernel after every collection, and the allocation that followed faulted them
  back in, zeroed: ~640 MiB over a Paper start, each release being a TLB shootdown on every core
  inside the pause. As G1 does, the heap now shrinks only after a full collection: minor faults
  545k → 483k (java 329k), peak RSS +10 MiB.

A/B (`4a9c03b2` against java, Paper 1.21.1-133, fresh world, 2 rounds each):

| processors | rava `Done` | java `Done` | rava CPU | java CPU | rava peak RSS | java peak RSS |
|---|---|---|---|---|---|---|
| 4 (the host) | 22.7-22.9 s | 24.6-25.8 s | 35-36 s | 60-64 s | 957-960 MiB | 1075-1127 MiB |
| 12 (`-XX:ActiveProcessorCount=12`) | 12.6-13.0 s | 19.8-20.0 s | 36-37 s | 66 s | 1024-1036 MiB | 1166-1244 MiB |

The same start runs from the self-contained distribution (`dist/make.sh`) with an empty
environment: no `JAVA_HOME`, no `java` on `PATH` (`Done` 21.9 s).

## 2026-10-04 (cont.) — the stronghold search: opt-tier code against C2's

Before `Preparing start region`, one `Worker-Main` thread computes the stronghold rings
(128 biome searches of 57×57 points each); on both VMs it is the critical path of that phase, and
a faster machine does not shorten it. A harness running the same searches on the real classes
(`MultiNoiseBiomeSource.getNoiseBiome`) put rava ~7% behind C2: the noise was already faster
(NormalNoise and ShiftNoise samples 14.9k against C2's 17.0k in PerlinNoise), the climate
R-tree search 1.4-1.6x slower. C2's code for `Climate$RTree$SubTree.search`, disassembled from
`-XX:CompileCommand=print` bytes, showed the differences; each fix below is generic:

- **Predicates of enclosing loops** (`guards.rs`). An inner loop's null and bounds predicates
  sat in its preheader, so they ran on every iteration of the outer loop, and the type check of
  the invariant `DistanceMetric` receiver stayed in the loop because its class word was loaded
  there. Guards whose inputs the enclosing loop does not change, in blocks it always runs, move
  to its preheader with its entry state; a branch on the class word of an invariant non-null
  object becomes a guard that loads it there. Override checks (CHA flags) stay in the loop.
- **Constant counted-loop predicates** (`codegen_ops.rs`): `for (i = 0; i < 7; i++)` no longer
  tests `0 < 7` and `0 >= 0` at run time.
- **Unreachable exits** (`ir.rs`). A branch turned into a guard left its deoptimizing side
  without predecessors, but in the code order its state kept values live to its position: the
  inner loop's accumulator was stretched across a call, spilled, and summed through memory.
  Such blocks are dropped before allocation.
- **Compressed references used in place** (`codegen.rs`, `asm.rs`). Every loaded reference was
  decoded with a shift before its fields were read. One whose uses all follow it in its block as
  the object of a field load or array length (or its null check), with no call, allocation or
  state in between, now stays compressed and is addressed as `[narrow*8 + offset]`, as C2 does.
  Straight-line block chains are merged first so that a null check and its loads share a block.
- **Dead code** (`dce.rs`): values nothing reads are no longer computed.

Tree search harness: 3481 → 2994 ns per search (C2: 2200-2500). The phase's `Worker-Main` CPU
in Paper: 5.50-5.75 → 5.32-5.47 s.

Also in this round:

- **`Math.sin/cos/tan/exp/log`** (`natives/math.rs`). Their bytecode ran fdlibm's Java port; as
  HotSpot does with its own stubs, the VM supplies their bodies from the C library (within an
  ulp, as `Math` allows; `StrictMath` keeps fdlibm), and the optimizing tier calls the same
  functions directly, so results never depend on the tier. Probe: `MathSem`.
- **Call sites** (`opt/codegen.rs`): the prologue no longer writes the call-site slot that every
  call site overwrites anyway (`anchor` writes it where a frame is published without a call).

**Deep recursion** (`runtime/mod.rs`, `boot.rs`). With `RAVA_OPT=all` a recursion to
StackOverflowError aborted up to a third of the runs, the native stack exhausted. Two causes:
constructing the error overflowed the reserved zone and started another construction, nested
until the native stack ran out (a nested overflow now gets the error without its constructor,
as HotSpot throws a preallocated one); and `StackOverflowError` first initialized at a stack's
end could fail there and stay erroneous, each later throw building a `NoClassDefFoundError`
recursively (the VM's own errors are now initialized at boot, as HotSpot does after
`initPhase1`). Calls into Java from the VM also check a native stack reserve. 60 of 60 runs pass.

**Short counted loops** (`unroll.rs`): a loop counting by 1 between constants at most 8 apart,
of one body block without calls or allocations, is unrolled completely (the tree's distance
loop over 7 parameters). Tree search: 2994 → ~2800 ns.

A/B (`efe49b55` against java, Paper 1.21.1-133, fresh world, 2 rounds, same host and hour):

| processors | rava `Done` | java `Done` | rava CPU | java CPU | rava peak RSS | java peak RSS |
|---|---|---|---|---|---|---|
| 4 | 22.8-23.4 s | 25.3-26.2 s | 37-38 s | 62-63 s | 939-944 MiB | 1140-1197 MiB |
| 12 (`-XX:ActiveProcessorCount=12`) | 12.9-14.1 s | 20.9-21.3 s | 37-40 s | 69-71 s | 1021-1029 MiB | 1244-1260 MiB |

## 2026-10-04 (cont.) — All The Mods 10

The metric is now a heavy modded server: All The Mods 10 8.2 (NeoForge 21.1.251, 464 mods),
its own `user_jvm_args.txt` (`-Xms4G -Xmx8G`, G1 options, `AlwaysPreTouch`) for both VMs, a
fresh world each run (`bench/paper_ab.sh -b <atm10 base>`, the base set up by the pack's
`startserver.sh` installer and one first start).

Getting it to start took compatibility fixes, each generic and matched to HotSpot:

- `Class.getNestMembers0` was missing and `getDeclaredClasses0` returned nothing
  (`NestSem`).
- `Unsafe.staticFieldBase` is the class mirror and `staticFieldOffset` relative to it, as in
  HotSpot: mods read `MethodHandles.Lookup.IMPL_LOOKUP` by passing the `Class` object as the
  base. Unsafe reference accesses tell a static slot from a heap field by address
  (`UnsafeStaticSem`).
- java.lang.invoke's core classes are initialized at boot (HotSpot's
  `initialize_jsr292_core_classes`): reflective field access before the first lambda threw
  `InternalError`.
- Only the JDK's own native libraries count as built in; loading another one (Truffle's
  `libtruffleattach`, netty's epoll transport) throws `UnsatisfiedLinkError` at load time, so
  their users take their fallbacks, instead of failing at the first native call. (There is no
  JNI for third-party libraries yet.)
- `HotSpotDiagnosticMXBean.getVMOption` works: the command line's `-XX:` options and a few
  defaults (`FlagSem`).

With these, rava's log has no error Java's has not (Java's first start also reports config
files it creates).

A/B (`28cdc636` against java, 4 cores, 2 rounds):

| | rava | java |
|---|---|---|
| wall to `Done` | 150.9-158.8 s | 153.4-161.5 s |
| CPU (user+sys) | 253-265 s | 371-377 s |
| peak RSS | 6298-6373 MiB | 7731-7805 MiB |

Milestones from the first log line (one run each): mods constructed at 64 s (java 76 s), the
server starting at 132 s (java 133 s), `Done` at 159 s (java 157 s). The data loading between
construction and the server's start (recipes, KubeJS scripts, tags, unification) takes rava
68 s against java's 57 s: the next target.

## 2026-10-05 — All The Mods 10: OSR into optimized code, the main thread

Profiling the main thread during the data loading found the module-graph resolution at
ModLauncher start (`Resolver.makeGraph`, cubic in the ~450 automatic modules) running
first-tier code to its end: a long loop of a method called once had no way into optimized
code, which has no block entries. Changes, each generic:

- **OSR into optimized code** (`jit/opt/osr.rs`, HotSpot's OSR compilations): a hot
  first-tier back edge asks for a version of the method built from the loop header, its live
  locals read from the frame as parameters (kinds from a verifier-style inference), entered
  through a bridge that calls it on the running frame. Branches first-tier code never ran
  deoptimize in such versions (C2's `Reason_unreached`), so code after the loop gets its own
  version once profiled (`OsrSem`).
- **Inline caches** publish (class, target) as one immutable pair: two threads missing on a
  site at once could leave a class paired with another's target for good, which ran a method
  on a receiver of the wrong class (an `AbstractMethodError` in a KubeJS codec, about one start
  in eight).
- **Retiring optimized code** works for methods without loops: their baseline code was not
  recorded, so a failing speculation in, e.g., `HashSet.add` deoptimized on every call instead
  of recompiling after 8.
- **Frequency estimation** conserves flow out of loops longer than its 20-iteration cap; the
  innermost sites of nested loops looked 20x colder per level and were not inlined.
- **String and array intrinsics** (`jit/leaf.rs`): `StringLatin1.equals`,
  `ArraysSupport.vectorizedMismatch` / `vectorizedHashCode`, `StringCoding.countPositives` as
  word-at-a-time leaf calls (`StrIntrSem`).

Main-thread CPU from ModLauncher to `Done`: 71.1 k samples before, 64.2 k after OSR (Java
56.7 k); the resolver alone went from ~8.7 k to ~5 k samples.

A/B (4 cores, fresh world, rounds 2-3 of 3; the first round of each build is cold):

| | rava `9a331a61` | rava `3899ec26` | java |
|---|---|---|---|
| wall to `Done` | 134-145 s | 149-151 s | 141-151 s |
| CPU (user+sys) | 246-263 s | 246-250 s | 350-360 s |
| peak RSS | 6.2-6.4 GiB | 6.3 GiB | 7.6-8.3 GiB |

Hosts vary by ±4 s between sessions; within one session rava now matches or beats java on
wall time with a third less CPU and 1.4-2 GiB less memory. Left on the main thread: code
quality of the recipe loading (regex, gson, KubeJS filters; C2 inlines more and
scalar-replaces the iterators), allocation refills, and the module graph's memory locality
(a copying young collection lays out each set's nodes together).

## 2026-10-05 (cont.) — Using the idle cores

Per-thread-group CPU over an All The Mods 10 start (`/proc` sampled each second):

| | main | JIT | GC | application pools |
|---|---|---|---|---|
| rava `9a331a61` | 62 s | 29 s | 21 s | 122 s |
| java | 53 s | 115 s | 48 s | 123 s |

The application's own threads cost the same on both; most of the start is one thread at a
time (the main thread, mod construction on `modloading-sync`, the datapack load) with the
other cores idle. HotSpot fills them with compilation (~0.8 cores throughout). rava's two
compiler threads used ~0.2.

- Compiler threads are now one per core (2-8) at nice 10: they take only the cores the
  application leaves idle. The tier-2 threshold dropped from 5000 to 1500 invocations
  (variants: 1500 best; 1000 no better; 600 slower), and OSR requests jump the queue. Main
  thread CPU 65 -> 60 s.
- A JDK `Keep-Alive-Timer` thread spun a whole core in about one start in four: failed socket
  writes (EPIPE) were reported as 0 bytes written (an error marker narrowed to `int`), which
  the JDK retries forever (`BrokenPipeSem`).

GC pauses were checked too: marking already runs on every core, sweeping takes 0.4 s of 8.5 s
of pauses, time to safepoint ~50 ms over a start.

A/B (`ca252dca`, 3 rounds, fresh world):

| | rava `ca252dca` | rava `9a331a61` | java |
|---|---|---|---|
| wall to `Done` | 130.8-135.4 s | 136.8-137.8 s | 134.9-146.5 s |
| CPU (user+sys) | 232-244 s | 230-308 s (the spin) | 334-357 s |
| peak RSS | 6.3-6.5 GiB | 6.3 GiB | 6.7-7.6 GiB |

## 2026-10-05 (cont.) — The server tick: `/tick sprint`

Startup is not the game. The vanilla `/tick sprint 1d` (24000 ticks as fast as the server can,
`bench/tick_sprint.py`) measures the single game thread flat out, on All The Mods 10 after a
fresh start (spawn chunks, every mod's per-tick handlers):

| | ticks per second |
|---|---|
| rava `c6a7fcd2` | 730-757 |
| java | 1602-1704 |

Every sample is on the `Server thread`. Two generic causes found first:

- **`ldc` of dynamic constants** was left to the interpreter on every execution by both
  compiled tiers; NeoForge's generated event-listener classes read their class data so at the
  start of every call, 35 M times in one sprint. Resolved reference-typed dynamic constants now
  compile as constants: 866-913 tps.
- **`Runtime.totalMemory`** (asked by a mod every tick) summed every large object under the
  heap lock: now a running total. 1025-1040 tps.

Java's lead is now 1.6x (rava 970-1040, java 1600-1700). Checked and not the cause: megamorphic
dispatch (an event-bus microbenchmark: 2.9 ns per listener against java's 2.4), object
locality (a pointer walk over mixed-size objects: rava 7.7-8.4 ns, java 9.4-9.6), inline
copies of short arrays (a stub made no difference to the sprint, removed). 82% of the tick runs
optimized code; the gap is its quality against C2 across all mod code.

Also checked and not the cause: the load before each call. Virtual calls with the entry kept in
the vtable slot (one dependent load fewer), and calls to known methods made direct (`call rel32`
to a per-method `jmp rel32` stub patched when the method's code changes, as HotSpot's static
call sites) each left the sprint within noise (direct 824/849 tps against 895/793), so neither
was kept. Class queries on unknown classes and `Reference.refersTo0` now skip the native adapter
(1.8x on a microbenchmark, within noise on the sprint). Next: the call protocol's argument
stores and register saves around calls, allocation refills.

**Huge pages.** The hottest tick method (a mod's level system: a map lookup, a cast, a virtual
call) spent most of its samples on the first touch of each object and its class, and neither
rava nor this `java` command line used huge pages: with 4 KiB pages the TLB covers a few
megabytes of a multi-gigabyte heap. `MADV_HUGEPAGE` on the code cache gave 832/890/855 tps
against 847/849/812, on the heap 911/912/939 against 867/889/868. Paper's start costs 1-6% more
peak RSS (1015/1058 MiB against 1001/993) at the same start time.

## 2026-10-07 — where the tick gap is: C2, not the runtime

**Steady state, not warm-up.** Three sprints in a row in one server (`bench/tick_sprint.py -r`):
rava 918, 1019, 997 tps; `java` 1422, 1755, 1704. Both settle after the first sprint; the gap
(1.7x) is steady-state code.

**Not the runtime, collector or heap.** `java -XX:TieredStopAtLevel=1` (C1 only, the same
G1, heap and class library) runs the sprint at 412-414 tps: the optimizing tier here is 2.4x
better than C1, and everything `java` gains beyond that is C2's. Allocation is the same in both:
rava allocates 7.4 GB over the 24000-tick sprint (306 KB per tick, `RAVA_GC_LOG`), `java` about
6.9 GB (290 KB per tick, `-Xlog:gc`), so escape analysis removes about as much in both; young
pauses cost rava 0.5 s of the sprint against `java`'s 0.2 s.

**Kernels** (best of 12, ms; `java` / rava): HashMap get with record keys 19.3 / 53.0,
ConcurrentHashMap get with String keys 10.6 / 30.6, stream filter-map-sum 4.9 / 12.9, string
concatenation 15.8 / 40.9, `instanceof` pattern chain 13.2 / 31.4, boxing in a HashMap merge
27.4 / 41.2, allocation of small records 47.6 / 64.1, lambdas 10.3 / 13.2, megamorphic interface
calls over a list 30.0 / 22.0, `Arrays.sort` 80.2 / 85.7. A map lookup loop bisected: summing
cached String hashes 0.60 / 0.91 ns, a hand-written chained table 2.14 / 3.06, HashMap 2.56 /
4.01, ConcurrentHashMap 2.87 / 4.89 ns per lookup: 1.4-1.7x at every step. Method-handle
construction (`unreflect`, `bindTo`, `invokeWithArguments`, what one mod does per event) is
760 / 1450 ns.

**Measured and dropped** (each within noise on the sprint or the kernels): direct calls to known
methods through patched stubs; the entry in vtable slots; larger allocation chunks (16 and
64 KiB against 4 KiB); larger inlining budgets (worse); no safepoint polls in loops (3-8 % on
tight loops, nothing on object code); spill weights by block frequency (the spilled accumulator
of a lookup loop moved to a register, same time); values across cold calls kept in caller-saved
registers; a scratch register instead of `push`/`pop` in `Unsafe` reference accesses.

**Kept:** transparent huge pages for the heap (911/912/939 tps against 867/889/868) and the code
cache (+2-3 %), class queries and `Reference.refersTo0` as direct leaf calls, jump threading of
branches on merged boolean constants, type checks dominated by a proving test.

What remains is C2's breadth, spread evenly over every method: loop optimizations (unrolling,
range-check predication, strip-mined polls), fewer instructions per bytecode (sign extensions,
copies, repeated checks, decoded narrow references), a leaner call protocol. No single change
found here moves the sprint by more than a few percent.

## 2026-10-08 — loop-invariant chains: a stream pipeline at `java`'s speed

The stream kernel (`filter`/`mapToLong`/`sum` over 200 `Integer`s) ran at 828 ns against
`java`'s 352: every element reloaded the sink chain (`downstream.downstream...`) and rechecked
each sink's class. Four causes, one per fix:

- **Final fields past stores.** The sum sink stores its state at the offset where other sinks
  keep `downstream`; LICM kept every load at a stored offset. Loads of final instance fields
  outside constructors (`Graph.final_loads`) now pass such stores (764 ns).
- **Loops entered from a branch.** A bounds check before the loop made its only outside
  predecessor end in an `If`, and LICM, RCE and guard hoisting all skipped loops without a
  plain preheader. Such an edge now gets a block of its own (`licm::preheaders`).
- **Chains.** LICM repeats until nothing moves: a hoisted load's null check is invariant on the
  next round and becomes a preheader guard, so `this.a.b.c` leaves the loop whole. Null checks
  on paths some iterations skip are speculated too (a failing guard traps at the header).
- **Type tests first.** Loads of an inlined receiver's fields are valid only below the test of
  its class. Invariant branches that deoptimize on one side become guards inside the LICM
  fixpoint (`guards::branches`), and an object whose class the loop still tests keeps its
  loads in the loop. Without this, `LinkedList` iteration in compiled `ArrayList` code
  dereferenced a field read from the wrong class (ScalarLoopSem crashed one run in two).

Stream: 330 ns (`java` 352). The tick sprint stays within noise (two A/Bs, both orders:
first sprint 1004-1212 against 1106-1196, warm 1138-1279 against 1174-1306 tps).
`LoopHoistSem` covers final chains, a speculated check on a skipped path, and a polymorphic
receiver read in a loop.

## 2026-10-08 (cont.) — the sprint profiled method by method against `java`

`bench`-style sprint profiles of both VMs (`perf record` over one `/tick sprint 1d`, `java` with
`-XX:+DumpPerfMapAtExit`) put the server thread at 21.2 s for rava against 15.1 s. `java` spends
1.5 s of it in megamorphic dispatch stubs ("vtable chunks") and 0.6 s interpreting, so event
dispatch (NeoForge's `EventBus.post` and its generated listeners) costs about the same in both;
rava's runtime takes 2.5 s on that thread (`arraycopy_fast` 0.7 s, allocation refills 0.4 s,
helping the collector mark). The largest single difference was xycraft's per-tick system loop,
which builds `dimension().location().toLanguageKey()` (a string concatenation) for every system
on every tick.

- **arraycopy in place**: a copy into a fresh primitive array (what concatenation and
  `String.getBytes` do) is checked and copied inline, with overlapping unaligned moves up to 16
  bytes; the shared helper's size switch was mispredicted by sites of different lengths.
  `namespace + "." + path`: 32 to 21 ns (`java` 10).
- **declared types**: a field declared with a final class gives its loads that exact class (C2
  types loads by declaration), so `stringOf`'s `toString` on a `String` field binds statically
  however polluted its profile. Only a supertype cast keeps the exact class (null passes any
  cast; binding through an unrelated one inlined a body with the wrong locals and panicked).
- **exact class past a guarded inline**: when every inlined target needed one class guard and a
  miss deoptimizes, the receiver keeps that class in the builder's per-block facts.
- **Unsafe on arrays**: an Unsafe access whose base the method also uses as an array (CHM's
  `tabAt`) skips the heap-range test.

Sprint A/B, 3 rounds, against the build before these: first sprint 1341/1408/1354 against
1303/1271/1260 tps, warm 1479/1496/1513 against 1454/1409/1365 (+5-6 %).

## 2026-10-08 (cont.) — dispatch, guards and boxes

Instruction classes of the warm sprint's compiled code (every optimized method dumped, samples
by instruction): 12 % on a method's first instruction (control reaching a callee), 5.6 % on the
instruction right after a virtual call's target loads (class, vtable pointer, entry: cache
misses on class metadata), 11.6 % on class compares and 8.5 % on reference decodes (mostly the
loads before them missing). `java`'s second sprint is 34 % faster than its first, rava's 7 %:
the gap is steady-state code. Per method (warm, matched by name) rava is 1.9x `java` on the
same methods, a third of the difference in the few at 3x or more (xycraft's system loop, NeoForge
listeners), the rest spread at 1.3-2x.

- **itable hits point at their block** (its interface in the word before it): the method loads
  while the interface compares, four dependent loads instead of five.
- **embedded vtables**: each class has room after it for its vtable, read at a fixed offset from
  the class in every tier (no vtable pointer load).
- **class guards in place**: a class word read only by its block's branch is one `cmp dword
  [obj], class` (the implicit null check moves onto it); compressed references also stay
  compressed when their block's branch tests them for null or a field store takes them.
- **box elimination** (C2's EliminateAutoBox): `Integer.valueOf(x)` and its siblings return a
  box of x; an unboxing of such a box, merged through inlined call results, reads x and the
  allocation goes. A bimorphic `Supplier<Integer>` loop: 19.9 to 11-13 ms (`java` 8).

Sprint A/B, 5 rounds against the build before these four: first sprint 1400 against 1292 tps
(+8.4 %), warm 1491 against 1415 (+5.4 %). `java` on this host: 1780-1827 first, 2382-2487 warm.

## 2026-10-08 (cont.) — against C2's code itself

**Where the steady state goes.** On the warm sprint rava's server thread spends 1.97 s on the
first instruction of compiled methods against `java`'s 0.18 s (plus 1.19 s in its vtable stubs):
control reaching callees stalls (instruction fetch), not the instructions after it. The code
cache is all on huge pages (182 MB resident, all `AnonHugePages`), so it is not the TLB.
Doubling the inlining limits looked like +4 % in two A/Bs and was noise in a third (the start
lost 3 % every time): reverted.

**C2's code, instruction by instruction.** Without a working `hsdis`, `-XX:CompileCommand=print`
still dumps C2's code as hex; a small script (`objdump` over the dump, HotSpot's comments kept)
reads it. `LevelChunk.getBlockState` is about 75 instructions on its hot path in C2's code and 110
in rava's. The difference, piece by piece: a 15-instruction prologue against 5 (frame-stack check,
method and caller links, entry poll, parameters loaded from the frame), stores of values that
deoptimization states read, null tests repeated after a branch already tested the value, three
range checks where C2 keeps one, CHA flag tests (C2 records dependencies instead), and sign
extensions.

- **Dominated conditions** (`opt/conds.rs`, C2's `dominated_by`): branch edges, null checks and
  bounds checks leave facts (operands keyed by value, immutable class word, array length or
  constant; integer ranges against constants); null checks of known non-null values (casts of
  them too), bounds checks inside an explicit range test, and branches with a known outcome go.
- **Array elements read again** in an extended block reuse the first load (int, long, float,
  double and reference elements only: a mask may make a byte load unsigned).

Sprint A/B, 4 rounds on this (slower) host: 706/829 tps against 694/812 (+2 %, within noise).

## 2026-10-08 (cont.) — the gap is spread evenly

**Method by method.** The biggest single difference in the sprint profile,
`LevelSystemBase.tick` (12 % of the server thread), takes the same share of `java`'s server
thread (12.2 % against 12.4 %): every part of it costs rava about 1.5x, on the same loads that
miss in both (the attachment's class word, a slot of the `IdentityHashMap` table, the class
metadata of a megamorphic receiver). `EventBus.post`'s listener loop is already six instructions
around the call; its time is the receiver class's vtable line, as in C2's code. No one method or
mechanism holds the gap: it is the code quality everywhere.

**`bench/java/SystemTickBench.java`** reproduces it in seconds (listener lambdas over 40 system
classes, attachment lookups, interface calls, a discarded concatenation): rava ~5.4 µs per tick
against `java` ~3.5 µs. The tick is compiled on its own: entered through OSR in the middle of the
loop, the list iterator could not be scalar-replaced, which no real tick has.

- **Publishing the frame once.** A method that calls Java stores its header as `Thread::top`
  in the prologue; each call then writes only its BCI (callees restore `top` on every exit).
- **Interface type checks** compare `Klass::super_hit`, the interface the class was last
  found to implement: one load from the class instead of the itable block and its header.
- **Array allocations** have an exact class, so the cast of a fresh `byte[]` (string
  concatenation's `newArray`) is gone.

Still open from these profiles: megamorphic call sites whose receivers all share one
implementation (an interface implemented by one abstract base class) are dispatched, not
inlined; loop counters live across calls are reloaded from their spill slot at every use.

## 2026-10-09 — counting instructions instead of timing

**A noise-free yardstick.** Timing on this host moves ±5-20 % between runs, more than most
code-generation changes. Under `valgrind --tool=callgrind` (a `target-cpu=x86-64-v3` build, as
valgrind does not decode AVX-512), with `RAVA_SYNC_COMPILE=1` and `RAVA_PERF_MAP=exit` (the map
written once at exit: a write per compilation stalled the serialized threads), the main
thread's instructions per tick of `SystemTickBench` are exact to about ±3 % (identity hashes vary
the `IdentityHashMap` probes), and `--dump-instr=yes` gives each JIT instruction's execution
count. `java -Xbatch` runs the same way.

- Per tick, `java` executes 33,500 instructions; rava 37,400 before the changes below, 34,200
  after (−7 % measured over three runs each).
- **Late algebra** (`opt/algebra.rs`): `x + 0`, `x << 0`, `x * 1`, `x & -1` and constant pairs
  that only became constant after inlining and folding.
- **Wide constants** read twice between calls take one `movabs` (`reload.rs`).
- **Proved non-null objects**: casts, instance tests and identity hashes skip their null test,
  marked by a last dominator walk over the final graph (marking earlier let LICM move a cast
  above its null check).
- **Inline arraycopy**: one move per zero-extension, constant positions as displacements.
- Dropping `l2i`'s sign extension crashed Paper: array indexing reads an int index's whole
  register (reverted; the comment there now says so).

**What the instructions do not explain.** With equal instruction counts rava still takes
~4.4 µs per tick against `java`'s ~3.0 µs. `MegaCallBench`'s "2x slower call" turned out to be
baseline code: the baseline compiler inlined the loop into `main`, whose loop never reached OSR,
so 65 % of the time ran at tier 1. The optimized loop's own bytes, run in a C harness at the
process's own data addresses, take 3.65-3.9 ns a call, as HotSpot's do. In that harness two of
64 frame offsets doubled the cost (4K aliasing between the frame stores of a call and the loads
after it), a hazard only memory-passed arguments and per-call BCI stores have. Padding
branches off 32-byte boundaries (HotSpot's JCC-erratum mitigation, worth 10 % to `java` here)
and 32/64-byte code alignment changed nothing measurable: reverted.

## 2026-10-09 (cont.) — the frame protocol, home slots, declared types

Measured with the same instruction and data-reference counts (`SystemTickBench` per tick; a
`ConcurrentHashMap.get` loop with `String` keys per get), `java -Xbatch` alongside:

| change | tick instr | tick data refs | CHM get instr |
|---|---|---|---|
| before | 34,812 | 13,921 | 68 |
| lazy frame header | 34,736 | 13,764 | |
| no entry poll in bounded methods | 34,652 | 13,720 | |
| evicted values without home slots | 34,376 | 13,444 | |
| declared parameter types | 33,838 | 13,225 | 66 |
| scaled Unsafe addresses | 33,839 | 13,225 | 64 |
| `java` | 33,500 | 11,238 | 50 |

- **Lazy frame header.** A method that calls no Java code (a leaf with slow paths) wrote its
  `Method` and caller link on entry and restored `Thread::top` on exit. The first slow path that
  publishes the frame now writes the header; an exit unlinks only a published frame: four
  memory operations fewer per call.
- **No entry poll** in a method without loops or Java calls (its callers' polls bound the time
  to a safepoint).
- **Evicted values**: linear scan left a spilled value's register in its location until slots
  were assigned, so a value states read past its last use also got a home slot (two stores of
  the same value).
- **Declared parameter types**: the compiled method's parameters declared with a final class or
  an array of one have that class (a `String[]` element is a `String`). Inlined bodies' parameters
  are not typed: one is a caller value, and `KeySet.iterator()` inlined behind a class guard
  passes its receiver on as a `KeySet`, which typed the caller's `Collection` everywhere and broke
  Paper's start (an iterator rebuilt on deoptimization from the wrong object).
  `RAVA_OPT_IR_STATES` (states, aliases and virtual objects in the IR dump) traced it.
- **Scaled Unsafe addresses**: `base + ((long) i << k) + c` is one addressing mode.

Not kept: preferring a phi's slot for its input's spill (no change), hoisting `FlagCheck`s out of
loops (a class loaded mid-loop would leave the inlined target running; HotSpot deoptimizes the
frame instead).

Wall time stays noisy on this host; the CHM loop is 1.26 ms against `java`'s 0.87 per 200k gets,
in line with the instruction counts (IPC is similar).

## 2026-10-09 (cont.) — strings, folds, and two compiler bugs All The Mods 10 found

Counted per operation (valgrind, `x86-64-v3` build), `java -Xbatch` alongside:

| kernel | before | after | `java` |
|---|---|---|---|
| `SystemTickBench`, instructions per tick | 33,839 | 31,320 | 33,500 |
| two-string concatenation | 211 | 195 | 198 |
| `HashMap.get` with record keys | 66 | 61 | 44 |
| `ConcurrentHashMap.get` with `String` keys | 64 | 62 | 50 |

- **`vectorizedHashCode`** (`String.hashCode`, `Arrays.hashCode`): AVX2 lanes, 32- and 8-element
  blocks, instead of an iterator fold (11 instructions per byte).
- **Trusted instance finals** as `ciField::trust_final_non_static_fields`: `String`, the boxes,
  reflection, foreign and vector packages, the field updaters. A literal's `coder` and length fold.
- **Late folds**: a fresh array's length is its size, a constant array's a constant, bounds checks
  of constant indexes against them go, branches between constants fold; an inline arraycopy from a
  constant array checks it at compile time; `bastore` into an array of known class skips the
  `boolean[]` test; a constant Unsafe offset is a displacement.
- **Null tests**: a cast whose block then branches on null tests null once; a phi of values
  non-null on each edge is non-null.
- **Shift counts** skip `l2i`/`i2b`/`i2c`/`i2s`; conversions read their source register.

**Wall time is not instruction count.** With fewer instructions than `java`, `SystemTickBench`
still takes 2414 ns per tick against 1939. Without its per-tick string concatenation the two are
within 8 % (1268 / 1171 ns); a concatenation alone is 13.2 against 8.3 ns. Timer-sampled
profiles (`perf -e cpu-clock` works in this VM, hardware counters do not) put the time on
dependent loads: object headers and class display words in the tick, and in allocation the bump
pointer of the size class, whose address depends on the computed size (HotSpot's single TLAB top
loads in parallel). Allocation prefetch (`prefetchw` past the bump pointer) measured as noise.

**Two bugs All The Mods 10 exposed** (Paper and the semantics probes passed):
- the cast/null fusion left the null side before the cast's home-slot store: a deoptimization
  there rebuilt the frame from a stale slot (SIGSEGV in the interpreter's `checkcast` at start).
  Found by bisecting the day's commits on ATM10's start, then by method-hash bisection.
- `prune_unreachable` left unreachable blocks jumping into removed ones; a later pass laid one out
  and code generation hit an unbound label, so `BlockableEventLoop.execute`, a
  `ChunkGenerator.createStructures` lambda and others stayed at the lower tier. `RAVA_OPT_VERIFY`
  now checks edges after every pass; a fatal SIGSEGV report gives pc's module offset.

ATM10 `/tick sprint 1d` (one server each, two sprints): rava 1654 / 1810 tps, `java` 1686 / 2335
(earlier today rava 1506 / 1663). The warm sprint's profile: xycraft's level systems ~16 %, chunk
ticking ~6 %, NeoForge event dispatch ~4 %, and ~6 % in `java.lang.invoke` building method handles
per event (one mod calls `unreflect(m).bindTo(o).invokeWithArguments(a)` for each): 652 against
431 ns, a third of it in the runtime (`Array.newInstance` behind `Arrays.copyOf`, which HotSpot
allocates inline).

### Copies, subtype tests, statics as roots

- **arraycopy inline** in more cases: references into an array allocated in the same code (no card
  mark: nothing between could collect; a copy itself never collects, so a run of copies into one
  new array, as `MethodType.insertParameterTypes` makes, all qualify); primitives into an array
  whose class a field or parameter declares (`[B`, `[J`: exact by type), with a null test on it;
  a source proved non-null, or declared with the destination's class, skips that test; a few
  non-zero bytes of a constant `@Stable` array (a literal separator) are stored as immediates.
- **Subtype tests against a run-time class** (`Class.isInstance`, `aastore` into a non-`Object[]`)
  check the primary display and the last-matched interface inline; the runtime notes a confirmed
  interface for the next test.
- **Young collections visit only static slots whose value changed** since the last collection
  (`gc/statics.rs`): walking every class's static fields and updating each was the largest part
  of a young pause's fixed cost on ATM10 (~25 of ~40 ms of marker CPU). No write barrier.
- **Diagnostics**: `RAVA_GC_LOG` reports page faults per collection; `RAVA_JIT_DUMP` writes a
  `.map` of each IR value's code offset with its bytecode position, to attribute samples inside a
  large compiled method.

Findings that did not turn into gains yet:
- Allocation refills clear 4 KiB chunks of memory last touched a collection ago: ~2500 cycles a
  chunk on ATM10 (~5 % of the server thread). Prefetching the next chunk took ~12 % off. Clearing
  is 3× cheaper when the allocation area fits in L3 (≤64 MiB), but a young collection's fixed
  cost (~15 ms with a 3.6 GiB old generation: cards, statics, threads, sweep) rules out a small
  young generation for now.
- xycraft's level systems spend ~25 % of their time building `level.dimension().location()
  .toLanguageKey()` for an inactive profiler's `push`; the `String` is scalar-replaced already,
  the `byte[]` and its copies are not (as with C2).
- Hot code size is the same as HotSpot's (99 % of samples in ~1.35 MB of code on both).

ATM10 `/tick sprint 1d` on a 2.8 GHz Xeon (33 MiB L3), two servers each: before these commits
1365 / 1472 and 1317 / 1469 tps, after 1357 / 1509 and 1225 / 1433, `java` 1482 / 1968 and
1446 / 1904. The first sprint is within ~8 % of `java`; the warm one ~25 % behind.

## 2026-10-10 — clearing at allocation, uninitialized buffers, checked copies

- **Each object is cleared as it is made** (compiled code and the runtime, from the word after
  the class word to the cell's end), with HotSpot's allocation prefetch (`prefetchw` 192, 256
  and 320 bytes past the new bump pointer), instead of clearing 4 KiB runs ahead of the bump
  pointer: those runs were last touched a collection ago and cost ~2500 cycles each. A run a
  thread leaves unused goes back to its page's bitmap before a collection and when the thread
  detaches (a dead thread's untouched cells marked allocated sent a trace into garbage).
  `ThreadChurnGcSem` covers threads that die while collections run. Sprint A/B: +4-7 % first
  sprint, +2-3 % warm.
- **`Unsafe.allocateUninitializedArray0`** of a constant primitive class (string
  concatenation's buffer) allocates without clearing the elements (C2's
  `_allocateUninitializedArray`): a two-piece concatenation 12.3 → 11.2 ns; sprint +2-6 % /
  +1-4 % (two rounds).
- **Reference copies that need element checks** (`Object[]` into `String[]`:
  `Arrays.copyOf(.., Class)`, `ArrayList.toArray(T[])`, `SpinedBuffer.copyInto`) copy in the
  runtime with a check per element, as HotSpot's checkcast arraycopy, instead of deoptimizing:
  they were the four most frequent deoptimization sites of an ATM10 run, and
  `Arrays.copyOfRange` ended its recompilations at baseline code. `CheckedCopySem`.
- **Bounds-check smearing stays in its block**: a store to element 1 behind a branch widened the
  dominating check of element 0, so a one-element `List12.toArray` deoptimized on every call.

The ATM10 deoptimization log (`RAVA_DEOPT_LOG`) of a start and two sprints: 38k
deoptimizations, almost all during the start; the arraycopy and `toArray` sites were ~4k of
them.

Where the warm sprint's server thread goes now (dumped code, samples by IR operation): type
checks and the loads before them (`CheckCast` 13 %, class constants compared in guards
18 %, field loads 14 %), callee entries 10 %, virtual and interface calls 14 %. The hottest
cast (xycraft's `ISystemData`, megamorphic) waits on the object's class word and then on the
class's `super_hit` line; on this host `java` runs the warm sprint at ~1825 tps, rava ~1600.

### Metadata on huge pages

The low arena (classes with their vtables, methods, hierarchy flags, inline-cache cells, statics)
held ~400 MB on 4 KiB pages on ATM10, and the card table was on 4 KiB pages too, while the heap
and the code cache already used huge pages. With `MADV_HUGEPAGE` on both (and inline-cache pairs
and flattened itables moved into the arena): about +4 % on each, noisy over three to four rounds
each. Aligning `Klass` to 64 bytes (before the arena was on huge pages) cost 5 % and was
reverted.

- **Interface casts** compare the class first-tier code saw pass (`profile::cast_cell`) before
  reading the class's `super_hit`; the compare with the interface itself is gone. Neutral.
- **Not kept:** calling a devirtualized target that was not inlined directly instead of through
  the vtable or inline cache (first sprint −3 % and −11 %).
- **Locality is not the gap:** a 2-million-element walk of small objects of mixed sizes after
  collections takes rava ~35 ns per element against `java`'s ~57 (evacuation packs survivors in
  trace order). Allocation rates in the warm sprint are alike (~400-470 MB/s), and so are the
  young pauses (~45 ms every ~3 s against G1's 15-60 ms every ~5 s).

On this host, two servers each: `java` 1215 / 1796 and 1249 / 1840 tps, rava 1344 / 1508 and
1412 / 1689: rava leads the first sprint by ~11 % and trails the warm one by ~12 %.

### Mixed old pages (tried, not kept)

Segregated size classes keep an object and its fields' objects of other sizes on different
pages. A walk of a million small heterogeneous graphs (holder, array, values, string, arrays)
in random order takes rava ~165 ns per graph against `java`'s ~131 (whose full collection
slides objects in allocation order). An experiment moved objects up to 2 KiB into "mixed" old
pages (8-byte granules, bits at object starts, filled by bump in trace order, reused only when
empty, with card scanning and interior-pointer lookup by bitmap): copying each object's
references right after it (depth first) brought the walk to ~129 ns, copying on discovery (G1's
young order) only to ~168. On ATM10, three-way sprint A/B over three rounds: off 1395 / 1593,
depth first 1457 / 1579, G1 order 1366 / 1530; another three rounds of depth first against off
were +2 % / −6 %. Depth-first copying also serialized marking (subtrees are copied by the
marker that found them, outside the shared stack and the prefetch ring): full collections at
the start took 4-8 s instead of 0.4-0.8 s. Not kept; the warm sprint's hot data does not
behave like the microbenchmark.
