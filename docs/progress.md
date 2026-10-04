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

Known issue: with `RAVA_OPT=all` (every method straight to the optimizing tier, a test mode)
`StackOverflowSem` aborts about once in twelve runs: the native stack overflows before the value
stack's limit. The default mode passed 20 of 20.

A/B (`efe49b55` against java, Paper 1.21.1-133, fresh world, 2 rounds, same host and hour):

| processors | rava `Done` | java `Done` | rava CPU | java CPU | rava peak RSS | java peak RSS |
|---|---|---|---|---|---|---|
| 4 | 22.8-23.4 s | 25.3-26.2 s | 37-38 s | 62-63 s | 939-944 MiB | 1140-1197 MiB |
| 12 (`-XX:ActiveProcessorCount=12`) | 12.9-14.1 s | 20.9-21.3 s | 37-40 s | 69-71 s | 1021-1029 MiB | 1244-1260 MiB |
