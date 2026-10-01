# Architecture

| layer | decision | why |
|---|---|---|
| class library | the real `java.base` (and friends) bytecode, read from the JDK's `lib/modules` jimage | HotSpot's compiled code is self-contained because `java.util` is ordinary bytecode it compiles and inlines. A VM that serves the library from native code pays a seam on every call that no JIT can remove. |
| values | untyped 64-bit slots; a reference is a raw pointer | no handle table, no tag decoding on the hot path |
| objects | 16-byte header (`klass*`, mark word), fields at fixed offsets packed by size; arrays add a 32-bit length at 16 and elements at 24 | field access is one load at a constant offset |
| memory | non-moving heap of size-class pages; threads bump through runs of free cells (inline in compiled code); generational stop-the-world mark-sweep with parallel marking, **conservative** stack scanning and precise heap scanning; sweeping swaps per-page allocation/mark bitmaps and never touches dead cells | raw pointers stay valid across GC, so Rust natives, the interpreter and compiled code need no handles or stack maps (the JavaScriptCore approach) |
| metadata | `Klass`/`Method` are immortal (boot classes never unload) and addressed by `&'static` | compiled code can embed metadata addresses |
| natives | Rust implementations registered by `(class, name, descriptor)`, the HotSpot native set | these are VM natives, not library replacements |
| execution | interpreter → baseline compiler emitting x86-64 directly with register caching, inline caches and method-handle folding → SSA optimizing tier with receiver-profile inlining and deoptimization ([opt-tier.md](opt-tier.md)) | a boot is compile-latency bound, so the first compiler must be cheap; the hot minority gets the expensive one |
| launcher | the `java` command line, with version, help and main-class resolution delegated to the JDK's `LauncherHelper` | rava replaces `java` without changing how anything is started |

The rules (no workload-specific tuning, no snapshots) are in the top-level
[README](../README.md#rules). What was measured and changed, session by session, is in
[progress.md](progress.md).
