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

### Next

1. Code quality: register-cached operand stack, cheaper calls (the profile is now flat over
   compiled Java code, led by method-handle chains).
2. Garbage collector (BootLike allocates 258 MiB with no collection).
