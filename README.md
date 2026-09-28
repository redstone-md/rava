<div align="center">

<img src="assets/brand/hero.png" alt="rava" width="100%">

<br><br>

**A Java Virtual Machine written from scratch in Rust.**

Loads real `.class` files and `.jar`s. Interprets bytecode, JIT-compiles hot methods with
Cranelift, and runs under a moving generational collector.

<br>

<img src="https://img.shields.io/badge/rust-stable-A855F7?style=flat-square&labelColor=0B0614">
<img src="https://img.shields.io/badge/JIT-Cranelift-A855F7?style=flat-square&labelColor=0B0614">
<img src="https://img.shields.io/badge/GC-moving%20generational-22D3EE?style=flat-square&labelColor=0B0614">
<img src="https://img.shields.io/badge/license-MIT-F59E0B?style=flat-square&labelColor=0B0614">

</div>

<br>

The benchmark that decides whether this works is a Minecraft 1.21 server. rava boots it to
`Done`, and a player can join and stay in the world.

It is a research engine, not a drop-in JDK. It exists to answer one question — how far can a
small, hackable runtime get against a JIT that has had twenty-five years and thousands of
engineers? On most operations, not far enough. On a few, it already wins. Both halves are
below, with numbers.

<br>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=ARCHITECTURE&note=class%20files%20to%20machine%20code&theme=light">
  <img src="https://gfx.redstone.md/strip?label=ARCHITECTURE&note=class%20files%20to%20machine%20code&theme=dark" width="100%">
</picture>

<img src="assets/brand/pipeline.png" alt="class and jar files, interpreter, JIT, machine code, collector" width="100%">

A class file is parsed, verified and interpreted. A method that runs often enough is handed to
Cranelift and replaced by native code. Objects are allocated by a pointer bump emitted inline in
that native code, and collected by a Cheney copy.

Every layer is checked against the JDK. The arithmetic, the string hashing, the integer
overflow, the object moves — all of it has to match bit for bit, or the test fails. That
constraint is what makes the rest of the numbers meaningful.

<br>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=JIT&note=Cranelift%20and%20the%20loop%20forker&theme=light">
  <img src="https://gfx.redstone.md/strip?label=JIT&note=Cranelift%20and%20the%20loop%20forker&theme=dark" width="100%">
</picture>

<img src="assets/brand/jit.png" alt="JIT" width="31%" align="right">

**Loops that parallelize themselves.** When the compiler can prove a counted loop's iterations
are independent, it forks the loop across a work-stealing pool and reduces the results. No
annotation, no `parallelStream()` — plain `for (int i = ...)` bytecode runs on every core. The
JVM does not do this, and it is the one place rava beats C2 by a multiple rather than trailing
it.

**Escape analysis that removes the allocation.** When an object provably never leaves the
method, rava does not allocate a cheaper version of it. It does not allocate. The fields live in
registers. Inside a parallel loop body that means the workers never touch the heap and never
stall behind a collection.

**Loop folding.** A loop that accumulates an invariant N times becomes one multiply. There is no
loop left to run.

<br clear="all">

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=MEMORY&note=bump%20allocation%2C%20copying%20collection&theme=light">
  <img src="https://gfx.redstone.md/strip?label=MEMORY&note=bump%20allocation%2C%20copying%20collection&theme=dark" width="100%">
</picture>

<img src="assets/brand/gc.png" alt="nursery and tenured space" width="31%" align="right">

Allocation on the hot path is a pointer bump in machine code — no callback into the runtime, no
free list. The nursery is collected by copying the survivors out; when tenured space fills it
gets a copying major collection of its own.

The collector moves objects, which means every reference the compiled code holds has to survive
a move. That is the hardest invariant in the engine and the source of most of its interesting
bugs.

<br clear="all">

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=PARALLEL&note=work%20stealing%20without%20a%20global%20lock&theme=light">
  <img src="https://gfx.redstone.md/strip?label=PARALLEL&note=work%20stealing%20without%20a%20global%20lock&theme=dark" width="100%">
</picture>

<img src="assets/brand/parallel.png" alt="one stream fanning into parallel lanes" width="100%">

| Data-parallel loop | serial JIT | parallel JIT | vs JDK |
|---|---:|---:|---:|
| Integer reduction, 200M trip | 192 ms | **31 ms** | ~5.5× faster |
| Reduction reading a captured parameter | 234 ms | **32 ms** | ~7× faster |
| Reduction allocating a confined object | 90 ms | **3 ms** | ~2× faster, zero allocations |

The last row is the two optimizations stacking: every core instead of one, and the
per-iteration allocation erased. The JDK runs that loop on a single core and allocates the
object every time.

<img src="assets/brand/workstealing.png" alt="work-stealing pool" width="46%" align="left">

The pool the forked iterations run on has no global interpreter lock. Workers take their own
arena segment, steal from each other when they run dry, and publish results through a barrier.

This is also where the engine's current ceiling lives: the forker only fires on counted,
side-effect-free reduction loops. A Minecraft tick is none of those, so the server itself still
runs on one core.

<br clear="all">

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=BENCHMARKS&note=measured%20against%20the%20JDK%2C%20lower%20is%20better&theme=light">
  <img src="https://gfx.redstone.md/strip?label=BENCHMARKS&note=measured%20against%20the%20JDK%2C%20lower%20is%20better&theme=dark" width="100%">
</picture>

JIT-hot-path microbenchmarks, each run against `java` on the same machine. The ratio is rava
time divided by JDK time, so **lower is better** and 1.0 is parity. These move 2-3× run to run
with machine load and JVM warmup — read them as a range.

| Benchmark | rava / JDK | What it measures |
|---|---:|---|
| `Statics` | **faster** | counted invariant loop, folded to a closed form |
| `ArithLoop` | 1.2-1.4× | tight integer and long arithmetic |
| `StringOps` | 1.4-1.7× | concatenation, `hashCode`, `startsWith` |
| `Allocs` | 2.3-2.8× | object allocation and construction |
| `Dispatch` | 2.1-3.8× | polymorphic virtual calls |
| `Fields` | 3.9-5.7× | primitive field access |
| `DispatchMono` | 8-10× | monomorphic virtual calls |
| `FieldsRef` | 10-11× | reference field access |
| `Collections` | 10-16× | `HashMap` put and get |

Parity on arithmetic and strings. A small constant behind on allocation and dispatch. Several
times slower on collections and reference-heavy code. The mature JVM wins most of this table and
it would be dishonest to present it otherwise.

The interesting claim is not that rava is faster than the JVM. It is that a few thousand lines
of Rust can reach it on tuned paths and beat it on the multi-core paths it ignores.

Methodology and reproduction: **[BENCHMARKS.md](BENCHMARKS.md)**.

<br>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=SERVER&note=Minecraft%201.21%20to%20Done&theme=light">
  <img src="https://gfx.redstone.md/strip?label=SERVER&note=Minecraft%201.21%20to%20Done&theme=dark" width="100%">
</picture>

<img src="assets/brand/server.png" alt="Minecraft server" width="100%">

The server boots to `Done` in about **100 seconds**, against the JVM's **12**. Roughly eight
times behind, and the reason is not the one we assumed for a long time: profiling the guest
instruction pointer shows only **3-16% of that wall is spent inside compiled code**. The boot is
host-bound. The cost is per-call bookkeeping in the runtime, not the quality of the code
Cranelift emits — which is why a JIT intrinsic measured at 9.6× on its own microbenchmark moved
the boot by exactly zero.

Two other things are honestly slow:

- **Collections.** Every `HashMap.put` leaves machine code for a Rust callback. Inlining the
  bucket probe would close most of it and has not been done.
- **One core.** See the parallel section — the forker does not fire on the shapes a server tick
  is made of.

Where rava wins outright is **cold start**: short programs run end to end in 39-78 ms against
the JVM's 77-156 ms, because there is no JVM to start.

<br>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://gfx.redstone.md/strip?label=BUILD&note=cargo%2C%20flags%2C%20layout&theme=light">
  <img src="https://gfx.redstone.md/strip?label=BUILD&note=cargo%2C%20flags%2C%20layout&theme=dark" width="100%">
</picture>

```sh
# default build — mark-sweep GC, conservative
cargo build --release -p rava-cli

# with the moving GC, the JIT speed work, and the loop forker
cargo build --release -p rava-cli --features next-gen-gc

# run a class
./target/release/rava -cp path/to/classes Main

# production build — fat LTO + one codegen unit; slower to build, faster at runtime
cargo build --profile release-fat -p rava-cli --features next-gen-gc
./target/release-fat/rava -cp path/to/classes Main

# the JIT is off by default: it costs boot time and pays back on long loops
RAVA_JIT=1 ./target/release/rava -cp path/to/classes Main

# opt into the auto-parallelizing JIT
RAVA_JIT=1 RAVA_JIT_PARALLEL=1 ./target/release/rava -cp path/to/classes Main
```

Benchmarks run with `bash bench/run.sh`, which needs a JDK on `PATH` for the comparison.

```
crates/
  rava-cli/        the rava binary
  rava-interp/     interpreter, JIT, GC, standard-library natives
  rava-classfile/  .class parsing
  rava-jar/        .jar loading
bench/             microbenchmarks and the harness
docs/              design notes, profiles, decision log
```

The arena GC, the JIT speed work and the loop forker all sit behind the `next-gen-gc` Cargo
feature. Build without it and the engine is byte-for-byte the older mark-sweep runtime. The
default build is the conservative one.

<br>

### Status

rava implements enough of the JVM spec and the class library to boot a real modded-scale
server: `invokedynamic`, lambdas, records, generic erasure, most of `java.util`, NIO,
reflection, threads. It does not implement everything, the error messages are not the JDK's, and
the boot is slow. Point it at your own jar and something will be missing — that is expected.
File it or read the code.

One standing rule: the engine is never adapted to a specific jar. Every native implements
generic JDK behaviour. The Minecraft server is the health check, not the target.

<br>

<div align="center">

<img src="assets/brand/logo.png" alt="rava" width="300">

MIT — see [LICENSE](LICENSE)

</div>
