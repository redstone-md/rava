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

### Next

1. Baseline compiler (tier 1).
2. Garbage collector (BootLike allocates 258 MiB with no collection).
