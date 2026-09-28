# Structural IR resource budget

The compiler rejects an emitted function with more than 16,384 inserted blocks
or 262,144 SSA values before frontend finalization, and checks again before
codegen because finalization can add values. Rejection uses the existing
interpreter fallback. This bounds admitted IR, not total process memory, and
does not yet stop excessive work while emitting a body.

The policy uses only generated IR structure. It does not consult method names,
invocation history, timing, inputs, or previous failures. Diagnostic names and
counts do not influence eligibility.

## Evidence, 2026-09-05

Candidate SHA-256:
`02944F15CD5D7952A6A8C00CD850AA87344DD9CA42512616B8D06235F6A5D696`.
Evidence: `%TEMP%/rava-ir-budget-20260905`.

- Three boundary tests pass.
- CompSem with first-entry compilation, synchronous compiler, ordinary inline
  budget and a 4 GiB process cap matches all 48 normalized Java reference lines.
  The former 1,331,566-value / 296,233-block body is rejected before finalization.
- All 28 differential probes match Java with first-entry compilation and the
  normal background compiler configuration.
- Default mode passes all 28 differential probes, five product hashes, and the
  certified cold and warm ReloadBench results. Evidence is in
  `%TEMP%/rava-ir-budget-default-validation-20260905`. Its 46,398 ms whole-process
  ReloadBench duration is a correctness run, not a startup speed measurement.

## Full product boot is a separate measurement

The historical 27.4 s figure in `the-boot-price-map.md` measures Minecraft startup
to `Done`. The 44,698 ms result in
`%TEMP%/rava-inline-expansion-20260905/final-result.txt` measures the entire
ReloadBench process including its additional warm reload and certification.
These numbers do not establish a startup regression or its absence.

Fresh isolated boots use the same pristine world fixture, port 25599, P-core
affinity, ordinary release build, standing memory caps, and no snapshot flags.
Only the process started by the harness is stopped. Each row requires a full
`Done (...)s` line; failed boots have no wall-to-Done result.

| Candidate mode | Wall to Done | Process CPU | Evidence directory suffix |
| --- | ---: | ---: | --- |
| Default | 31,613 ms | 33.500 s | `148dbbf015b54e87a69cf67d517a7337` |
| First-entry | 43,482 ms | 51.969 s | `476bbb06fedc4a429d08db884b18a3b8` |

Directories are under `%TEMP%/rava-product-boot-<suffix>` and contain the binary,
configuration, fixture hashes, environment, output and result JSON. The harness
is `%TEMP%/rava-isolated-product-boot.ps1`. Host performance counters were
unavailable. This is one exploratory pair, not a causal performance estimate.
First-entry compilation remains experimental and off by default. The resource
guard resolves the observed OOM but does not establish a speedup. Full boot
under five seconds and complete no-adapt compliance remain unproven.

Next: avoid generating oversized IR in the first place, then measure the
compile-work reduction before considering broader first-entry scheduling.

## Early-emission validation

SHA-256: `1C57A50A4B534C6CBD5DB43226017D86E94346DD7A78C3BE41DFCFE35F9E9A15`.
Source now counts allocated blocks in O(1), including uninserted leaders, and
checks before each `emit_op`, including nested inline calls. A single opcode
can cross the ceiling; the following check refuses the whole function. Final
guards remain in place. This is not a bound on all allocator activity.

Four unit tests pass. CompSem matches its 48 normalized reference lines under
the same synchronous first-entry setup. The formerly 1,331,566-value body now
refuses during emission at 66,715 values / 16,391 inserted blocks. Evidence:
`%TEMP%/rava-ir-early-20260905`.

An exploratory first-entry boot pair (candidate then previous binary) measured:

| Binary | Wall to Done | CPU | Peak working set | Directory suffix |
| --- | ---: | ---: | ---: | --- |
| Early refusal | 45,711 ms | 53.938 s | 1,406,537,728 B | `32b93662afcf4073b5d2d9379c17b224` |
| Previous | 46,295 ms | 54.375 s | 1,639,358,464 B | `6f7c7377b36c4ccb97ed159f657b6322` |

Both reached Done. The small time difference does not establish a speedup;
memory usage fell in this pair. No claim of sub-five-second startup follows.
Default mode passes all 28 semantic probes, five product hashes and certified
cold/warm ReloadBench results. First-entry mode also passes all 28 semantic
probes. Default evidence directory:
`%TEMP%/rava-ir-early-default-validation-20260905`.
