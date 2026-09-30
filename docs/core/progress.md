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

