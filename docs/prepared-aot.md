# Explicit code preparation experiment

On Windows x86-64 with `next-gen-gc`, run:

```powershell
rava --prepare-aot <new-output-directory> '<directory-or-jar;another-entry>'
$env:RAVA_AOT_PREPARED_DIR = '<new-output-directory>'
$env:RAVA_JIT = '1'
rava -cp '<same-classpath>' MainClass
```

Preparation parses classes without creating a VM or executing guest code. It
walks explicit directory/JAR inputs in classpath order, keeps the first class
definition, and writes relocatable method objects plus `methods.tsv`. It refuses
an existing output directory. A compiler-identity `complete` marker is written
last; interrupted builds cannot be loaded. No initialized heap is serialized.
Nested JAR discovery, wildcard expansion, symlink traversal, and multi-release
overlays are not implemented by this command. Supply the actual extracted
classpath used by the runner. Objects currently target the preparing machine's
native CPU; cross-machine portability is not supported.

Full-AOT keys bind executable SHA-256, serialization version, owner, constant
pool, bytecode, local count, and constant policy. Copying the executable or
changing its timestamp does not alter identity. Changing executable content
invalidates the completed build. The old timestamp-keyed full-AOT namespace is
not reused. The directory is trusted executable input, not an untrusted package
format or a signed artifact.

Prepared mode loads eligible objects from the first invocation after the existing
class-initialization and representation guards permit it. It never increments
the AOT hot counter or writes/compiles missing AOT objects. Unsupported methods
still use the existing runtime, including its configured JIT policy. Therefore
this experiment alone does not establish whole-program no-adapt behavior or
startup below five seconds. Coverage counts are method counts, not CPU coverage.

`bench/prepared_aot_sem.ps1` checks a single-call loop method against Java with
a JIT threshold of two billion, fresh initializer output on each execution,
actual AOT entry tracing, executable-copy identity, and unchanged artifacts.

Validation on 2026-09-05: all 32 AOT unit tests and the release build pass.
The directory/JAR first-call probe, fresh initializer, unchanged files, missing
marker rejection, and incompatible compiler rejection pass in
`%TEMP%/rava-prepared-f2816c7cb9454b96a278f485e761ee3f`.
The existing constant-pool version 1/2/2/1 probe also passes in
`%TEMP%/rava-aot-pool-cache-00cb045852a04ea0b09df7d864cb2c22`.
Release SHA-256:
`8644B866EEC7DFD0399A7036423938A4B9E36B7E85658CF9CF4EFB585DAE660E`.

## Full classpath experiment

The explicit server classpath prepared 30,407 classes: 269,184 methods compiled,
79,521 unsupported. Preparation took 201,936 ms, 182.547 CPU seconds, peak RSS
82,370,560 bytes. Evidence:
`%TEMP%/rava-prepared-server-eb57e93a0dfe4973ac32fc599b7ea289`.

Fresh-world, process-launch-to-Done runs using the same executable:

| Mode | Wall ms | CPU seconds | Evidence suffix |
|---|---:|---:|---|
| Prepared | 87899 | 43.750 | de491388f4f94197a6243c688690ef39 |
| Default | 27693 | 29.969 | 4f4e865c0adb41aa8d5473a25af57206 |
| Prepared repeat | 42188 | 42.234 | dc00154cacff4044b2f92ae4a147a9a6 |
| Default repeat | 28296 | 30.578 | ec522e41dcb64d2fb9395c5809030e37 |

Evidence directories are `%TEMP%/rava-product-boot-<suffix>`. The first prepared
run logged a public-key request failure and had substantial non-CPU elapsed
time; its 87.9 seconds must not be attributed wholly to generated-code cost.
The repeat still loses materially in both wall and CPU. All reached Done;
recipe/advancement counts are not a substitute for all semantic probes or
complete reload hashes. Localized performance counters were unavailable, so
quiet-machine verification was not established. These are experimental runs,
not proof of a precise causal regression percentage.

This backend is not a startup speed improvement and stays opt-in. Removing
runtime compilation alone did not pay for this backend's execution and linking
costs. The next optimization must address those costs or improve compiled-code
quality; merely preparing more of the same code is not supported by this result.

## Semantic blockers found by broader execution

The 29-probe suite with the prepared server objects passes 27 probes and fails
two: `ClassSem` reports `arrayStore NO THROW` instead of ArrayStoreException;
`ExcSem` exits with access violation 0xc0000005 before its first result. Both
pass on the same binary with prepared mode absent, and both prepared failures
repeat. The crash report names `opcodes::invoke_virtual::invokevirtual` reading
address 0x8. These defects blocked the initial implementation. Subsequent repair
and verification are recorded in `prepared-aot-semantics.md`. Prepared mode
remains an investigation tool rather than a supported replacement execution mode.

Evidence lives under the preparation directory's `semantic-controls` and
`semantic-failures` subdirectories; the full suite log is `semantic-suite.log`.
Only artifacts matching current inputs can load, but this is not a proof of
translator semantic correctness. Source leads to inspect next: the AOT runner
clears heap JIT faults yet only consumes `jit_pending` after execution;
`rava_aastore` uses `heap.array_set`. The null invocation needs a separate
receiver-check analysis. Additionally, AOT `checkcast` is currently a no-op.
Do not hide these defects with workload-name or previously-successful-method
admission rules.
