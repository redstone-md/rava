<div align="center">

<img src="assets/brand/hero.png" alt="rava" width="100%">

<br><br>

**A Java Virtual Machine written from scratch in Rust.**

Self-contained: ships with the Java class library it runs, compiles hot code to x86-64 in two
tiers, and takes the `java` command line unchanged.

<br>

<img src="https://img.shields.io/badge/rust-stable-A855F7?style=flat-square&labelColor=0B0614">
<img src="https://img.shields.io/badge/JIT-baseline%20%2B%20SSA-A855F7?style=flat-square&labelColor=0B0614">
<img src="https://img.shields.io/badge/GC-generational%20mark--sweep-22D3EE?style=flat-square&labelColor=0B0614">
<img src="https://img.shields.io/badge/license-MIT-F59E0B?style=flat-square&labelColor=0B0614">

</div>

<br>

The benchmark that decides whether this works is a Paper 1.21 Minecraft server, started cold
and timed to `Done`. rava gets there in about 1.2× the time HotSpot needs, with no snapshot, no
archive and nothing tuned for the server.

<br>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=USAGE&note=the%20java%20command%20line&theme=light">
  <img src="https://gfx.redstone.md/strip?label=USAGE&note=the%20java%20command%20line&theme=dark" width="100%">
</picture>

Wherever you would type `java`, type `rava`:

```sh
rava -jar paper.jar --nogui
rava -Xmx4G -XX:+UseG1GC --add-opens java.base/java.lang=ALL-UNNAMED -cp 'lib/*' app.Main
rava -p mods -m app/app.Main
rava Hello.java
rava @options.txt Main
```

Class path from `-cp`, `CLASSPATH` or `.`, with `dir/*` wildcards; `-jar`, `-m` and single
source files; `-D`, `-ea`/`-da`, `--add-opens` and the other module options; `-X` and `-XX`
options; `@argfiles`; `JAVA_TOOL_OPTIONS`, `JDK_JAVA_OPTIONS` and `_JAVA_OPTIONS`. Help, version,
`-XshowSettings` and module listings come from the JDK's own launcher classes, so the output,
error messages and exit codes are java's. JVM TI agents (`-javaagent`, `-agentlib`) are not
supported.

**Nothing else to install.** `dist/make.sh` builds a distribution laid out as a Java runtime:
`bin/rava` and, beside it, the class library (`lib/modules`, the OpenJDK platform classes as
bytecode), the data files library code reads (`lib/tzdb.dat`, `lib/security`, `conf/`) and
their licences (`legal/`). rava implements every native method itself; no JDK, JRE or HotSpot
is needed on the machine. A development build (`target/release/rava`, no `lib/` beside it)
borrows a JDK 21 or 25 class library from `JAVA_HOME` or the `java` on `PATH`.

<br>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=ARCHITECTURE&note=class%20files%20to%20machine%20code&theme=light">
  <img src="https://gfx.redstone.md/strip?label=ARCHITECTURE&note=class%20files%20to%20machine%20code&theme=dark" width="100%">
</picture>

<img src="assets/brand/pipeline.png" alt="class and jar files, interpreter, JIT, machine code, collector" width="100%">

**The real class library.** `java.base` and the rest come from the JDK's `lib/modules` image as
ordinary bytecode, the same bytes HotSpot runs. The VM supplies only what HotSpot supplies: the
native methods, `Unsafe`, method handles, threads. `HashMap` is compiled and inlined like any
other code.

<img src="assets/brand/jit.png" alt="JIT" width="31%" align="right">

**Three tiers.** An interpreter; a baseline compiler that emits x86-64 straight from bytecode,
with inline caches and method-handle folding; an optimizing tier on SSA with linear-scan
register allocation, receiver-profile inlining, CHA, loop-invariant code motion and
deoptimization back to the interpreter.

**Raw pointers everywhere.** A reference is an address and a field is a load at a fixed
offset (references inside objects are compressed to 32 bits, as HotSpot's). The heap never
moves what a stack, a native or the VM holds: size-class pages, bump allocation inline in
compiled code, a generational mark-sweep with parallel marking and conservative stack
scanning that moves survivors reached only through the heap into packed pages. Natives, the
interpreter and compiled code share objects without handles or stack maps.

<br clear="all">

<br>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=NUMBERS&note=against%20HotSpot%20on%20the%20same%20host&theme=light">
  <img src="https://gfx.redstone.md/strip?label=NUMBERS&note=against%20HotSpot%20on%20the%20same%20host&theme=dark" width="100%">
</picture>

<img src="assets/brand/server.png" alt="Minecraft server" width="100%">

4-vCPU Xeon @ 2.1 GHz, JDK 21, alternating rounds:

| | rava | HotSpot |
|---|---:|---:|
| Paper 1.21.1 cold start to `Done` | 34.4 s | 28.5 s |
| Hello world | 92 ms | 54 ms (72 ms with `-Xshare:off`) |

HotSpot's hello world reads a class-data archive. rava never does: every launch starts from
class files. Most of the server gap is world generation, where HotSpot is still ahead on
allocation and on the many small calls of its density functions. The record of what was
measured and changed is in [docs/progress.md](docs/progress.md).

<br>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=BUILD&note=cargo%2C%20layout%2C%20tests&theme=light">
  <img src="https://gfx.redstone.md/strip?label=BUILD&note=cargo%2C%20layout%2C%20tests&theme=dark" width="100%">
</picture>

```sh
cargo build --release          # target/release/rava
tests/semantics/run.sh         # every probe's output against java's
tests/patches/run.sh           # patches end to end
bench/paper_ab.sh HEAD java    # server start, A/B against HotSpot
```

```
crates/rava/       the VM: loader, interpreter, JIT, GC, natives, launcher
tests/semantics/   probes whose output must match java's
tests/patches/     a program and a patch exercising every kind of edit
patches/           official patches (rava-patches/ jars, docs/patches.md)
bench/             microbenchmarks, world-generation benchmarks, the server A/B harness
docs/              architecture, the optimizing tier, patches, the progress log
```

<br>

### Rules

- **No workload-specific tuning.** No path, threshold or policy keyed to one application's
  classes, names or outputs. The adaptive machinery every production JVM has (counters, tiers,
  OSR, type profiles, inline caches, speculation with deoptimization) is fair game, as are
  intrinsics for `java.*` methods.
- **No snapshots.** Every launch starts from class files: no heap or class archive, no
  checkpoint/restore, no AOT cache. A cold start is the only start that counts.
- **Program-specific changes are patches.** Code that knows one program lives in a patch the
  owner installs in `rava-patches/` ([docs/patches.md](docs/patches.md)), never in the VM;
  results with patches are reported apart from VM comparisons.

<br>

<div align="center">

<img src="assets/brand/logo.png" alt="rava" width="300">

MIT — see [LICENSE](LICENSE)

</div>
