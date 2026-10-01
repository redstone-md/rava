# rava — the parity execution core

Authorised by the user on 2026-09-29 after `roadmap.md` established that the current engine's
4x gap to HotSpot is architectural (JDK served as Rust natives behind a 40-800 ns seam, a handle
heap, compiled code that leaves machine code on every call). `rava` is a new crate that grows
next to `rava-interp`; the old engine stays the product until the new core boots the benchmark.

## Rules (unchanged)

- **No workload-specific tuning.** No route, threshold or fast path keyed to one application's
  classes, names, hashes or outputs. Generic JVM adaptivity (counters, tiers, OSR, type profiles,
  inline caches, speculation + deoptimisation) is allowed. JDK intrinsics keyed to `java/*` are
  allowed, exactly as HotSpot intrinsifies by name.
- **No snapshots.** Every launch starts from class files. No CDS-like heap/class archive, no
  checkpoint/restore, no prepared AOT cache.

## Architecture

| layer | decision | why |
|---|---|---|
| class library | the real `java.base` (and friends) bytecode, read from the JDK's `lib/modules` jimage | HotSpot's compiled code is self-contained because `java.util` is ordinary bytecode it compiles and inlines. The old engine's JDK-as-Rust design is the seam that cannot be closed. |
| values | untyped 64-bit slots; a reference is a raw pointer | no handle table, no tag decoding on the hot path |
| objects | 16-byte header (`klass*`, mark word), fields at fixed offsets packed by size; arrays add a 32-bit length at 16 and elements at 24 | field access is one load at a constant offset |
| memory | non-moving heap of size-class pages; threads bump through runs of free cells (inline in compiled code); stop-the-world mark-sweep with **conservative** stack scanning and precise heap scanning; sweeping swaps per-page allocation/mark bitmaps and never touches dead cells | raw pointers stay valid across GC, so Rust natives, the interpreter and compiled code need no handles or stack maps (the JavaScriptCore approach) |
| metadata | `Klass`/`Method` are immortal (boot classes never unload) and addressed by `&'static` | compiled code can embed metadata addresses |
| natives | Rust implementations registered by `(class, name, descriptor)`, the HotSpot native set | these are VM natives, not library replacements |
| execution | interpreter (tier 0) → template compiler emitting x86-64 directly with register caching, inlining and method-handle folding (tier 1) → the same compiler recompiling hot methods with receiver profiles and larger inlining budgets (tier 2, `docs/core/opt-tier.md`) | a boot is compile-latency bound (row 182: C1 alone is 8.3x over the interpreter); Cranelift is too slow as the first tier |

## Tracer bullets

1. jimage reader + class parsing from `java.base`.
2. Class linking: field layout, vtables, static storage; object allocation.
3. Interpreter over the direct layout; run static methods of JDK classes (`HashMap`, `ArrayList`).
4. `System.initPhase1` with the VM native set; `System.out.println` from a user class.
5. Baseline compiler; measure against `java -Xint` / C1 on `bench/BootLike.java`.
6. `invokedynamic` / `java.lang.invoke`, threads, GC — then the Minecraft boot.

Progress is recorded in `docs/core/progress.md`.
