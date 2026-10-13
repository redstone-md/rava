# Documentation

| | |
|---|---|
| [Minecraft](minecraft.md) | servers, the client and launchers |
| [Troubleshooting](troubleshooting.md) | thread dumps, diagnostics, reporting problems |
| [Optimizing tier](opt-tier.md) | the SSA compiler: inlining, speculation, deoptimization |
| [Java releases](jdk-versions.md) | which class libraries run and how releases differ |
| [Platforms](platforms-roadmap.md) | the Linux, macOS and Windows ports |
| [Patches](patches.md) | program-specific changes, installed by the owner |
| [VM roadmap](vm-roadmap.md), [patch roadmap](patch-roadmap.md), [GUI roadmap](gui-roadmap.md) | what is planned |
| [Progress log](progress.md) | what was measured and changed, step by step |
| [Release notes](releases/) | the highlights of each release |

## Architecture

| layer | decision | why |
|---|---|---|
| class library | the real `java.base` (and friends) bytecode, read from the JDK's `lib/modules` jimage | HotSpot's compiled code is self-contained because `java.util` is ordinary bytecode it compiles and inlines. A VM that serves the library from native code pays a seam on every call that no JIT can remove. |
| values | untyped 64-bit slots; a reference is a raw pointer | no handle table, no tag decoding on the hot path |
| objects | 16-byte header: a 32-bit class pointer (classes live below 2 GiB), 4 bytes for an array's length or the first small fields, the mark word at 8; fields at fixed offsets packed by size, references inside the heap compressed to 32 bits (address >> 3, heap below 32 GiB); array elements from 16 | field access is one load at a constant offset (a reference load adds a shift); objects about as dense as HotSpot's |
| memory | heap of size-class pages; threads bump through runs of free cells (inline in compiled code); generational stop-the-world mark-sweep with parallel marking, **conservative** stack scanning and precise heap and static scanning; objects reached only through the heap or a static move out of allocation pages into packed old pages, what other roots hold stays in place (mostly-copying, as Bartlett's); sweeping swaps per-page allocation/mark bitmaps and never touches dead cells | a pointer held by a stack, native or the VM never sees a move, so Rust natives, the interpreter and compiled code need no handles or stack maps (the JavaScriptCore approach), while survivors are still compacted |
| metadata | `Klass`/`Method` are immortal (boot classes never unload) and addressed by `&'static` | compiled code can embed metadata addresses |
| natives | Rust implementations registered by `(class, name, descriptor)`, the HotSpot native set and the class library's own natives for each system | these are VM natives, not library replacements; no JDK is needed beside rava |
| JNI | other libraries load with `dlopen` (`LoadLibraryW` on Windows); their natives are called with the system's ABI (System V, or Win64 on Windows) in the native thread state, `jobject` being the object's address kept in place by a local or global reference root ([gui-roadmap.md](gui-roadmap.md)) | applications' own native code (LWJGL, netty's epoll) runs as on any JVM |
| platforms | one VM for Linux, macOS and Windows on x86-64: memory, faults, threads and libraries behind `src/os/`, each system's class-library natives in their own modules ([platforms-roadmap.md](platforms-roadmap.md)) | the compilers, collector and object model are shared; only the edges differ |
| execution | interpreter → baseline compiler emitting x86-64 directly with register caching, inline caches and method-handle folding → SSA optimizing tier with receiver-profile inlining and deoptimization ([opt-tier.md](opt-tier.md)) | a boot is compile-latency bound, so the first compiler must be cheap; the hot minority gets the expensive one |
| launcher | the `java` command line, with version, help and main-class resolution delegated to the JDK's `LauncherHelper` | rava replaces `java` without changing how anything is started |
| patches | jars in `rava-patches/` next to the program redirect calls and open members of the classes it defines ([patches.md](patches.md)) | program-specific changes live in code the owner installs, never in the VM |

The rules (no workload-specific tuning, no snapshots) are in the top-level
[README](../README.md#rules). What was measured and changed, session by session, is in
[progress.md](progress.md); what is planned for the VM in [vm-roadmap.md](vm-roadmap.md), for the patches in [patch-roadmap.md](patch-roadmap.md), for desktop applications in [gui-roadmap.md](gui-roadmap.md).
