# Benchmarks

These are microbenchmarks of the JIT hot path, not application benchmarks. Each one
isolates a single operation (arithmetic, a virtual call, a HashMap put) in a tight
loop, runs it on rava and on a real JDK, and checks that both produce the same
result. The point is to find where the gap to the JVM lives, one operation at a time.

## How to read them

The ratio is `rava_time / jdk_time`. Lower is better. 1.0 is parity. The harness
prints `R/J` for each benchmark and an `ok`/`MISMATCH` column — a benchmark only
counts if rava's output matches the JDK's exactly.

A warning that's easy to forget: these numbers swing by 2-3x between runs. The JVM
needs warmup and the wall clock is sensitive to whatever else the machine is doing.
Run the harness three times and you'll see Collections land anywhere from 10x to
16x. So the tables below are ranges from several runs, not single measurements, and
the honest takeaway is "parity here, a constant behind there," not a precise score.

## Single-core hot path

Run with `RAVA_JIT=1`, recursive (no trampoline), `--features next-gen-gc`.

| Benchmark    | rava / JDK | Notes |
|--------------|-----------:|-------|
| Statics      | faster than JDK | the loop folds to `base + step * trip`; there is no loop left |
| ArithLoop    | ~1.2-1.4x  | native Cranelift integer/long codegen |
| StringOps    | ~1.3-1.7x  | strings are real arena objects; concat is an inline itoa + bump |
| Fields       | ~2.0x      | REGISTER PROMOTION: the unswitched body keeps every hoisted field in a register — zero memory ops in the loop |
| Allocs       | ~2.3-3.8x  | inline TLAB bump, constructor inlined, no per-object callback |
| Dispatch     | ~2.1-3.8x  | a 4-way machine-code inline cache for arena receivers |
| Collections  | ~3.3-16x   | HashMap ops cross into a Rust callback on every call |
| DispatchMono | ~3.5x      | final-class virtual inline + promoted receiver fields |
| FieldsRef    | ~10-12x    | reference fields still pay a tagging/boxing cost (chained receivers defeat promotion) |

What the table says, read plainly: the operations that got deep JIT work (arithmetic,
strings, allocation) are at or near parity. The ones that still leave machine code to
call back into Rust (collections, reference fields) are several times slower. That
callback boundary is the single biggest remaining cost, and closing it is mostly
engineering, not research.

## Data-parallel loops

This is the part that's actually novel. When the JIT proves a counted loop's
iterations are independent and reduce into one accumulator, it compiles the body into
a separate function and forks it across a work-stealing pool, GIL-free. The JDK runs
the same loop on one core.

Run with `RAVA_JIT=1 RAVA_JIT_PARALLEL=1`, 16 cores.

| Workload | serial JIT | parallel JIT | speedup | vs JDK |
|----------|-----------:|-------------:|--------:|-------:|
| `sum += f(i)`, 200M iterations | 192 ms | 31 ms | 6.2x | ~5.5x faster |
| reduction reading an outer param | 234 ms | 32 ms | 7.3x | ~7x faster |
| reduction with a confined `new` per iteration | 90 ms | 3 ms | 30x | ~2x faster, zero heap allocations |

Two things are happening on the last row. The loop runs on 16 cores instead of one,
and the per-iteration object never gets allocated because escape analysis scalar-
replaces it. With 20 million iterations that's 800 MB of allocation that simply
doesn't happen, which is also why the parallel workers never stall on garbage
collection.

The catch, stated up front: the analyzer only recognizes counted, side-effect-free
reduction loops over `int`/`long` with associative accumulation. Real application
loops usually aren't that. So this is a real win on numeric kernels and a
non-factor on, say, a Minecraft server tick. It's a sharp tool, not a general one.

## Reproducing

```sh
# build the comparison engine
cargo build --release -p rava-cli --features next-gen-gc

# run the suite (needs javac + java on PATH)
bash bench/run.sh
```

The harness compiles `bench/*.java` with `javac`, runs each under both `java` and
`rava` with a low JIT threshold so the warmup loops trigger compilation, and prints
the table. The data-parallel numbers above come from small single-file reduction
programs run with and without `RAVA_JIT_PARALLEL=1`. The same loop shapes are
covered as correctness tests in `crates/rava-interp/src/jit.rs` (search for
`jit_parallel_fork` and `jit_sroa`), which check the parallel result bit-for-bit
against the serial fold.

If your numbers look very different, check that you built with `--features
next-gen-gc` and that nothing else is hammering the CPU. A loaded machine will make
both columns slower but the ratio noisier.
