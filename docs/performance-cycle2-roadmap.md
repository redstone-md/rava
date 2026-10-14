# Performance cycle 2: bounded tracers

Source base: `bdc07921` plus documentation commit `68d8657b`.
Changes preserve Java semantics, cold process startup and workload-independent VM policy.
No application-specific VM routes, snapshots or compiled-code reloads are introduced.

## Portable diagnostic files

Existing JIT dump and perf-map flags write fixed `/tmp` paths, which are unavailable on
some hosts and do not provide a portable Windows destination.

1. Centralize diagnostic filename placement using the platform temporary directory,
   honoring `TMPDIR` through `std::env::temp_dir()`, with an optional `RAVA_DIAG_DIR` override.
2. Route optimized code, baseline code and immediate/deferred perf maps through that helper;
   preserve existing filenames and flag behavior. Resolve the directory only on enabled
   diagnostic paths, with no additional default execution cost.
3. Document destination precedence and the perf consumer's usual `/tmp/perf-<pid>.map`
   assumption. The override does not enable diagnostics or create snapshot state.
4. After the active profiling run, verify actual dump files and map contents in a supplied
   directory, platform CI behavior, and default Linux perf-map placement. Run required
   semantic and Paper gates before compiler commits.

Diagnostic directory precedence is a nonempty `RAVA_DIAG_DIR`, then
`std::env::temp_dir()` (Unix `TMPDIR`, otherwise the platform default). It is cached on
first diagnostic use. The chosen directory must already exist and be writable; failures
are reported. This variable does not enable any output flag.

```sh
mkdir -p bench/results/diagnostics
RAVA_DIAG_DIR="$PWD/bench/results/diagnostics" RAVA_JIT_DUMP_RAW=SomeClass.someMethod \
  ./target/release/rava SomeClass
```

`RAVA_JIT_DUMP`, `RAVA_JIT_DUMP_RAW` and `RAVA_PERF_MAP` use the same directory. With no
override and no Unix `TMPDIR`, Linux retains `/tmp/perf-<pid>.map`; `perf` normally looks
there. Relocating that map can prevent automatic symbol discovery: keep the standard
location for ordinary `perf` use or configure the consumer to read the explicit map.
These are output-only artifacts; RAVA never reloads them.

## Numeric intrinsic value numbering

Extend only GVN's explicit numeric allowlist: Round(mode), Sqrt, Abs(type),
MinMax(type, max), Bits(type), and BitCount(opcode, wide), all with no deoptimization state.
Keys retain the full intrinsic payload, result type and exact ordered operands through
existing operand resolution. Exclude Unsafe, leaf calls, identity hash, class queries,
libm and all other calls; introduce no new policy flags or input-specific rules.

The initial IR trace contains six Round(1) operations for duplicate inputs; the target is
three and fewer emitted rounding instructions. Verify NumericGvnSem across NaN, signed
zero, minimum values, raw bits and volatile side effects, then existing generic numeric
benchmarks plus root's semantic and Paper gates before any performance claim.

## Promotion-range ownership on exhaustion

Before a worker requests another old-generation run, drain its completed promotion range
once. An allocation failure must not append that same completed range again. Restrict the
change to `evacuate.rs`, preserving copy, mark, allocator and locking behavior.

Use a regression that executes the production copy path in a local Space with forced
allocator exhaustion. Verify the range's ownership and resulting bookkeeping, rather than
testing a mirror helper. No collector timing policy or scanner changes are part of this fix.

## Per-tick measurement harness

Keep VM code independent of the benchmark application. A separate Paper 1.21.1 plugin
uses the verified `ServerTickEndEvent.getTickDuration` API for raw duration measurements.

1. A command arms capture on the next tick with explicit warm-up and measured counts.
   The listener fills preallocated duration and tick-number arrays; CSV output and
   p50/p95/p99/max/deadline summaries run after capture on the main thread.
2. A direct-process runner isolates the base and regenerates all three dimensions. It
   preserves a fixed seed, complete configuration snapshots and port 25599; trusted argv,
   binary hashes, logs, manifests and child-specific stop/timeout handling are explicit.
3. Alternate VM order and record optional CPU affinity. First capture the existing
   350-mob scenario with zero players clearly labeled; forceloading alone does not prove
   full AI activation. Add a real protocol client later for the player-present scenario.
4. Verify capture counts, raw rows, summaries and clean shutdown before comparative runs.
   Do not change the existing `tick_sprint.py` behavior or infer tick quantiles from its
   sprint averages. Keep each new harness file below 500 lines.

Implementation and validation remain separate: no builds or timing runs while the current
profiling slot is active, and no performance claim before the exact candidate is checked.

## macOS unit executable layout

The forced-exhaustion regression constructs real low-address class metadata. macOS unit
executables must receive the launcher's existing `-pagezero_size,0x1000` linker setting,
so replace the Mac-only bins instruction with generic `rustc-link-arg`. Keep Windows
settings and the GC fixture unchanged; do not skip the test. Cargo forwarding is checked
with a small independent probe, followed by the actual macOS CI gate. The crate currently
builds an ordinary library and binary; revisit this executable-only flag if adding a
macOS `cdylib` target.
