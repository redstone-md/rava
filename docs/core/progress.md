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
