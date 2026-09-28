# Reload profiling boundaries

`ReloadBench --trace-phases` emits epoch-millisecond markers from the original
stdout stream, including after logging replaces System.out. The flag is diagnostic
only; the normal result format and cold/warm hashes remain unchanged.

Use a fresh `release-diag` executable with `next-gen-gc,wall-profile`, the standing
product environment, `RAVA_PROFILE_PC=1` and `RAVA_PC_TRACE=1`. Do not enable the
all-thread capture and describe this trace as an all-thread profile: its stream
contains the primary target only. After a successful process exit, run:

```
python bench/reload_pc_report.py <stdout> <stderr> --json <report.json>
```

The report rejects missing/inconsistent hashes, malformed phase sequences,
backward clocks and windows without samples. It uses the existing symbol
normalizer. Tests verify digest isolation and incomplete-evidence rejection.

`warm.digest` includes post-timer tag publication and bookkeeping followed by
the digest; it is not pure serialization self-time. Samples outside marked
windows are excluded. The sampler need not cover process startup before its
target is armed. These reports cannot establish a full boot wall or a speedup.

## Verified observations

Evidence: `%TEMP%/rava-jit-refusals-20260905`.
Profile executable SHA-256:
`104953518D159621F6B75E1A140FFF01418077F0BC78CE54A9D44457C6BA0AB7`.

- Java, primary-PC and stack-PC runs exit 0 with cold/warm hash
  `c956377e19071d7a`, counts 1470/1584/625, and complete phase markers.
- In the first trace, warm reload has 12232 primary-thread samples over 15694 ms.
  NtCreateFile accounts for 698 (5.71%), mi_malloc_aligned 450 (3.68%), and
  Vm::jit_run 370 (3.02%). These are sampled residency, not all-thread CPU costs.
- Post-warm validation takes 8948 ms and cold validation 8007 ms in that diagnostic
  run. Folding their samples into the reload population would misdirect optimization.
- A separate stack trace has 411 NtCreateFile samples in warm reload. Candidate
  stack-frame scans contain ClassLoader::read_resource in 206; none has a verified
  call chain. This suggests a loader origin but is not conclusive attribution.
- The loader reads every preceding directory source before a matching jar, and
  both reload and product-boot harnesses add an `out` directory to the classpath.
  Directory resources can change while the VM runs;
  blindly caching misses or ignoring their precedence would change semantics.

## Packaging control: no demonstrated gain

The follow-up used identical compiled benchmark classes in a directory and an
ordinary JAR, with no VM changes or profiling flags, baseline executable
`529FB86F00B7F77098451C0CEFE25DA84F8FFFE6BC2070281C8961A9C44C3204`,
standing 4096/4096/3584 MiB limits, the copied product config and affinity 0x0FFF.
All four ABBA processes exited 0 with cold and warm hashes correct.

| Source | Cold ms | Warm ms | Process wall ms |
| --- | --- | --- | --- |
| Directory A1 | 8252 | 6871 | 47206 |
| JAR B1 | 8723 | 7213 | 48201 |
| JAR B2 | 9180 | 7571 | 49908 |
| Directory A2 | 8310 | 7787 | 48216 |

Evidence: `%TEMP%/rava-reload-packaging-20260905`, including raw logs, config,
environment, binary hash, classes, JAR and `runs.jsonl`. Warm ranges overlap;
the small test shows no benefit, so no harness packaging or loader change was
made. The older directory-LAST control in `the-boot-price-map.md` (section
"AND THE REOPEN DEFECT IS RETRACTED") already reached the same conclusion.
Do not reopen this target from sampled NtCreateFile share alone.

## Compiler refusal evidence

The separate `RAVA_DBG_CBAIL` run also exits 0 with both hashes correct. It directly
reports `exceptions` for LinkedTreeMap.findByObject and Identifier.read. The
bytecode for the former has a ClassCastException handler. Simple native-backed
getters/iterators meet a different refusal before the compiler: their Java fields
do not represent the native-owned state. Removing that refusal is not a safe fix.
Global cached-failure counts include validation work and cannot price these methods
inside the reload window. Opcode co-occurrence in compile_fail_census is likewise
not proof of a causal refusal; use the actual driver stage and bytecode.
