# Full startup PC profile, 2026-09-05

Runtime source matches the structural-inline-selection implementation. The
receiver-history collection experiment was removed. The normal release binary
was rebuilt (SHA-256
`BB58CE690AEB510DAD5DF0B0495B74A360023E98DD5E918BBB383DF42FA23880`).

The restored normal binary completed an uninstrumented fresh-process boot in
33,213 ms (CPU 35.171875 s, peak RSS 1,150,558,208 bytes). Evidence:
`%TEMP%/rava-product-boot-e6706952f22a48b3908c562bf8473d10/result.json`.
This is process launch to the full Done line, not ReloadBench duration or the
guest's final world-start timer. The below-five-second goal remains unmet.

## Instrument validation

The profiles below built with `wall-profile` are historical attribution probes.
Prefer the no-wall-feature follow-up at the end of this document: PC sampling
implicitly enables extra per-call observers when that feature is compiled in.

The first isolated profile omitted the sibling PDB: 22,103 of 28,250 samples
were unresolved host addresses. It is rejected for function attribution.
Evidence: `%TEMP%/rava-product-boot-d5a058a8ab8d4b77b75d0863a0d72924`.

The corrected executable was linked with:

`cargo rustc --profile release-diag -p rava-cli --features next-gen-gc,wall-profile --target-dir D:/code/rava/target -- -C link-arg=/OPT:NOICF`

Both executable and PDB were copied into the isolated boot directory. Only the
primary sampler was enabled (`RAVA_PROFILE_PC=1`, `RAVA_PC_TRACE=1`,
`RAVA_PROFILE_PC_MS=3000`); all-thread capture and stack capture were unset.
Standing configuration, pristine world fixture and affinity were preserved.

Corrected evidence directory:
`%TEMP%/rava-product-boot-75405492dfa6447896ab91c4536a7ebb`.

- Executable SHA-256: `B5DEB146457937579DE4C6D80EFE5D23BBA4E4C31A4EC4183874B9E7AB85133E`.
- PDB SHA-256: `E846F92CECFDEA349CF39BB157F4105B2395333360A0D7E7E23245167D062913`.
- Complete Done line, 1,470 recipes and 1,584 advancements observed.
- 28,715 samples before Done; 67 unresolved host addresses (0.23%).
- First sample is 392 ms after process-launch timing began.
- Samples after Done are excluded using recorded epoch timestamps.

The instrumented 35,091 ms run is **not** a normal startup performance result.
Host quiet counters were unavailable. These are sampled primary-thread locations,
not exact CPU percentages or a profile of all compiler threads.

## Observations

| Leaf | Samples | Share |
| --- | ---: | ---: |
| `mi_malloc_aligned` | 849 | 2.96% |
| `NtCreateFile` | 776 | 2.70% |
| `Vm::jit_run` | 690 | 2.40% |
| `memchr` | 651 | 2.27% |
| `dispatch_owner_cascade` | 567 | 1.97% |
| `invokevirtual` | 541 | 1.88% |
| `Vm::jit_call_body` | 537 | 1.87% |

3,610 samples (12.57%) fall inside registered guest JIT ranges. The rest is not
all removable overhead: it includes useful native work, allocation, I/O, runtime
services and compilation. A change of output language alone would not erase it.

An approximate split at the second-resolution Environment log line yields
15,375 samples before it and 13,340 after it. Registered guest JIT ranges account
for 2,415 and 1,195 respectively. The largest post-Environment leaf is
`NtCreateFile` (630 samples); allocations and call handling appear in both phases.
Raw and grouped evidence is in `boot.err`, `leaf-counts.json` and
`phase-counts.json`. Boundary precision is limited by the guest log timestamps.

The profile does not identify one dominant function whose removal could deliver
the goal. Follow-up should trace repeated runtime work across call boundaries
and distinguish useful native work from avoidable conversion/dispatch. It must
not treat a whole category's share as the price of a proposed optimization.

## Follow-up stack capture

Evidence: `%TEMP%/rava-product-boot-4ece5e12ff2d45c289ae70b1ab8e1da4`.
The same diagnostic binary and PDB completed Done with stack capture enabled,
sampling every 3000 microseconds. There are 9,548 trace samples at or before
the recorded Done timestamp. `stack-samples.json` preserves the parsed records.
Instrumented wall was 36,186 ms and CPU 61.421875 s; neither is an ordinary
startup timing or evidence of a runtime regression.

The sampler scans copied stack words rather than unwinding frames. Even its
direct-call-target-matched column can include stale return addresses from an
earlier call to the same function. Treat these as candidate callers, not a
proven dynamic call tree or additive inclusive timings.

- Of 180 `dispatch_owner_cascade` leaf samples, 89 have a target-matched chain
  through `dispatch_owner`, `try_invoke_chain`, `hr_synth_finish`,
  `jit_direct_call`, `jit_call_body`, and `jit_call`. Another 36 match the
  interpreter `invokevirtual` route. This supports inspecting native-call
  resolution and conversion together, rather than only JIT admission.
- There are 278 `mi_malloc_aligned` samples. The largest repeated matched chain
  (42 samples) ends at string `write_str` through vector growth. It does not
  identify which application operation requested the formatting.
- There are 163 `memchr` samples with no matched direct caller. Conservative
  frames contain `owner_bits::entry` in 52 and `dispatch_owner_cascade` in 49.
  Those occurrences alone do not establish that either called `memchr`.

Source inspection rules out simply adding memoization to `owner_bits::entry`:
it already has a content-keyed map and a 256-slot pointer front cache whose
content check protects against reused temporary-string addresses. Classification
is computed once per distinct name. Removing that check without an interned-name
lifetime contract would be incorrect. Prior cache-size regressions are documented
in the source. Do not reopen this as an unmeasured substring-cache optimization.

The next actionable boundary is the native-call route through `hr_synth_finish`:
determine which per-method resolution or conversion work is already cached and
which repeats, preserving native-backed representations and exception behavior.

## PC-only follow-up: exclude wall-profile hooks

Source audit found that `rava_sampler::enabled()` includes PC profiling. Thus
the earlier `wall-profile` build activated bridge registration and its mutex on
every native entry, even without `RAVA_PROFILE_WALL`. Ordinary release uses the
no-op `rava_sampler_stub`. Those extra observers are not product execution cost.
PC sampling independently starts and arms from `opcodes::run_instrumented`, so
the wall feature is unnecessary for this experiment.

Built and ran successfully with:

`cargo rustc --profile release-diag -p rava-cli --features next-gen-gc --target-dir D:/code/rava/target -- -C link-arg=/OPT:NOICF`

Evidence: `%TEMP%/rava-product-boot-1466869e1d5a442892b0a238a33e239a`.
The isolated stack harness retained the 3000-microsecond period, copied the PDB,
and collected through the full Done line. Recipe/advancement counts were
1470/1584. Build log: `%TEMP%/rava-pc-only-build.log`.

- Executable SHA-256: `86C7A96DAF5E7D8000D0B8A029D8901F8C75072383A92683551D6C687A1A1390`.
- PDB SHA-256: `B71ED55B3025957E9CACFC0105C6F05266B7486D5BCEB005D096C880580A6FDF`.
- 6,620 samples before Done, starting 403 ms after the launch epoch.
- Zero `rava_sampler` leaf symbols or conservative frames in these samples.
- Instrumented wall 33,938 ms, CPU 49.265625 s. This is not a production speedup.
- 128 cascade leaf samples; 59 have a target-matched chain through
  `try_invoke_chain`, `hr_synth_finish`, `jit_direct_call`, and `jit_call_body`.
- 175 allocator leaf samples; 28 match the vector-growth/string-write chain.

The native boundary remains visible after removing the extra observer. Its plan
already caches the superclass chain and argument kinds. Per-call work includes
decoding raw cells, rooting references, canonicalizing arena strings where needed,
and dispatching through the common native entry. Adding another superclass cache
does not remove those operations. A direct native ABI needs equivalent rooting,
representation and exception contracts; skipping them is not an optimization.

Limitations remain: conservative stack candidates are not a true unwind, only
the primary target is captured, host quiet counters are unavailable, and PC
profiling still arms region-time guards. Sample counts from different instruments
are not directly comparable. `bench/boot_pc.ps1` now gives the no-wall-feature
build recipe and uses `RAVA_PROFILE_PC_MS` for its PC dump interval; these boots
used the separate isolated harness, not that script's in-place world reset.
