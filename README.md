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
<img src="https://img.shields.io/badge/platforms-Linux%20%C2%B7%20macOS%20%C2%B7%20Windows-22D3EE?style=flat-square&labelColor=0B0614">
<img src="https://img.shields.io/badge/license-MIT-F59E0B?style=flat-square&labelColor=0B0614">
<br>
<a href="https://github.com/redstone-md/rava/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/redstone-md/rava/ci.yml?branch=main&label=CI&style=flat-square&labelColor=0B0614"></a>
<a href="https://github.com/redstone-md/rava/releases"><img src="https://img.shields.io/github/v/release/redstone-md/rava?include_prereleases&label=release&color=A855F7&style=flat-square&labelColor=0B0614"></a>

</div>

<br>

The benchmark that decides whether this works is a Paper 1.21 Minecraft server, started cold
and timed to `Done`. rava gets there in about 1.2× the time HotSpot needs, with no snapshot, no
archive and nothing tuned for the server. It also runs the Minecraft client, vanilla and modded,
from the usual launchers.

> [!WARNING]
> rava is alpha software. It runs real servers and games, but expect crashes and differences
> from HotSpot, and keep backups of anything you open with it. Reports are welcome in
> [Issues](https://github.com/redstone-md/rava/issues).

<br>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=INSTALL&note=one%20archive%2C%20no%20JDK&theme=light">
  <img src="https://gfx.redstone.md/strip?label=INSTALL&note=one%20archive%2C%20no%20JDK&theme=dark" width="100%">
</picture>

Download the archive for your system from [Releases](https://github.com/redstone-md/rava/releases)
and unpack it anywhere. It is a complete Java runtime, the VM with the Java class library it
runs: no JDK or JRE is needed on the machine.

| System | Archive | Notes |
|---|---|---|
| Linux x86-64 | `rava-<version>-linux-x86_64.tar.gz` | glibc 2.35 or newer |
| macOS x86-64 | `rava-<version>-macos-x86_64.tar.gz` | Apple Silicon runs it through Rosetta 2 |
| Windows x86-64 | `rava-<version>-windows-x86_64.zip` | Windows 10 or newer |

```sh
# Linux, macOS
tar xzf rava-0.0.1-alpha-linux-x86_64.tar.gz
export PATH="$PWD/rava-0.0.1-alpha-linux-x86_64/bin:$PATH"
rava -version
```

```powershell
# Windows (PowerShell)
Expand-Archive rava-0.0.1-alpha-windows-x86_64.zip .
.\rava-0.0.1-alpha-windows-x86_64\bin\rava.exe -version
```

Each archive has a `.sha256` beside it. Releases are built and checked on all three systems by
[the release workflow](.github/workflows/release.yml).

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
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=MINECRAFT&note=servers%20and%20the%20client&theme=light">
  <img src="https://gfx.redstone.md/strip?label=MINECRAFT&note=servers%20and%20the%20client&theme=dark" width="100%">
</picture>

| | |
|---|---|
| **Servers** | `rava -jar paper.jar --nogui` in place of `java`: Paper, Fabric, NeoForge (All The Mods 10 starts and ticks) |
| **Client** | Minecraft 1.20.5 to 1.21.x, vanilla, Fabric and NeoForge, from Prism Launcher or any launcher that lets you pick the Java executable |
| **Graphics** | OpenGL through the driver as usual; `-Drava.opengl=vulkan` renders through Mesa's Zink on Linux where a Vulkan driver is installed |

In Prism Launcher: *Settings → Java → Java path*, choose `bin/rava` (`bin\rava.exe` on Windows)
from the unpacked archive. The bundled class library is Java 21's, so versions that require
Java 25 (Minecraft 26.x) do not run yet. More in [docs/minecraft.md](docs/minecraft.md).

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
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=TROUBLESHOOTING&note=thread%20dumps%20and%20reports&theme=light">
  <img src="https://gfx.redstone.md/strip?label=TROUBLESHOOTING&note=thread%20dumps%20and%20reports&theme=dark" width="100%">
</picture>

When a program hangs, rava prints a thread dump as HotSpot does: every Java thread, its state
and its stack.

- **Linux, macOS:** `kill -3 <pid>`, or `Ctrl+\` in the terminal.
- **Windows:** `Ctrl+Break` in the console.
- **Launchers without a console:** set the environment variable `RAVA_DUMP_EVERY=30` and a
  dump goes to the program's output every 30 seconds.

A useful report names the rava version (`rava -version`), the system, the command line or
launcher, and attaches the full output; for a hang, a thread dump. See
[docs/troubleshooting.md](docs/troubleshooting.md).

<br>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=BUILD&note=cargo%2C%20layout%2C%20tests&theme=light">
  <img src="https://gfx.redstone.md/strip?label=BUILD&note=cargo%2C%20layout%2C%20tests&theme=dark" width="100%">
</picture>

Rust stable and a JDK 21 (for the reference `java`, `javac` and the class library a
development build borrows):

```sh
cargo build --release          # target/release/rava
tests/check.sh                 # what CI runs: probes, JNI and patches against java
dist/make.sh                   # the release archive: rava with its class library
bench/paper_ab.sh HEAD java    # server start, A/B against HotSpot
```

The same commands work on macOS and on Windows (from Git Bash, with MinGW's `gcc` for the JNI
test). CI runs them on all three systems for every change.

```
crates/rava/       the VM: loader, interpreter, JIT, GC, natives, launcher, platform layer (src/os)
tests/semantics/   probes whose output must match java's
tests/patches/     a program and a patch exercising every kind of edit
patches/           official patches (rava-patches/ jars, docs/patches.md)
bench/             microbenchmarks, world-generation benchmarks, the server A/B harness
docs/              architecture, the optimizing tier, platforms, patches, the progress log
dist/              the distribution build (dist/make.sh)
```

<br>

### Documentation

| | |
|---|---|
| [Architecture](docs/README.md) | the object model, collector, compilers, natives and launcher, and why |
| [Optimizing tier](docs/opt-tier.md) | the SSA compiler: inlining, speculation, deoptimization |
| [Java releases](docs/jdk-versions.md) | which class libraries run and how releases differ |
| [Platforms](docs/platforms-roadmap.md) | the Linux, macOS and Windows ports |
| [Minecraft](docs/minecraft.md) | servers, the client and launchers |
| [Troubleshooting](docs/troubleshooting.md) | thread dumps, diagnostics, reporting problems |
| [Patches](docs/patches.md) | program-specific changes, installed by the owner |
| [Progress log](docs/progress.md) | what was measured and changed, step by step |

Contributions are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md).

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
