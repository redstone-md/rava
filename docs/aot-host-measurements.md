# AOT host measurements

Goal: distinguish object loading/linking from per-entry argument preparation
before choosing a new backend or changing the frame representation.

Select `RAVA_DIAG=aot` with `RAVA_DIAG_EVERY_MS=1000` and
`RAVA_DIAG_FORMAT=json`. `RAVA_DIAG_OUT` appends snapshots to a file.
Use a fresh output path for each owned process. These are instrumented runs,
not production wall-time evidence. Snapshot counts exclude work since the last
dump and can include work after Done unless the harness stops at that boundary.

- `cold_calls`: attempts entering the cold compile/link path, including refusals.
- `cold_ns`: inclusive elapsed time in that path, including nested work.
- `key_ns`: prepared-directory validation and cache-key construction.
- `read_ns`, `decode_ns`, `bind_ns`: object read, decode, and relocation/constant
  resolution for disk objects. Missing reads are included in read time.
- `pack`: existing sampled meter around allocation and argument packing only;
  it ends before guest execution. Sampling is not an exact CPU measurement.
- `cells`: histogram of local-plus-spill cells for entered native frames.

Do not sum child elapsed counters into their inclusive parent. Do not treat
method counts as execution coverage or median times as exact total cost.
Instrumentation never changes method eligibility or feeds optimization policy.

Validation: 33 AOT unit tests pass after adding the instruments. Full server
measurement requires rebuilding preparation artifacts with the exact new
executable; previous complete markers deliberately reject another binary.

## Full boot evidence, 2026-09-05

Executable SHA256: `C5F06B5A9BC3ED3863D6EC531BDD6D1DCF4DA4915F0F1F11CB52354EDDED694D`.
Prepared 30,443 classes: 269,382 compiled methods, 79,609 refusals.
Artifacts: `%TEMP%/rava-aot-host-fe39ae1efcf54b7a87e7d54d0a651279`.
Both instrumented processes used fresh world fixture copies and reached Done.
The localized performance counters were unavailable; quiet-box verification
was not established. Neither run is an uninstrumented performance claim.

| Measurement | First process | Repeat process |
| --- | ---: | ---: |
| Process to Done, seconds | 81.956 | 42.209 |
| Process CPU seconds at Done | 44.109 | 41.953 |
| Cold path inclusive elapsed, seconds | 39.125 | 0.878 |
| File read elapsed, seconds | 38.783 | 0.670 |
| Key elapsed, seconds | 0.156 | 0.121 |
| Decode elapsed, seconds | 0.019 | 0.007 |
| Bind elapsed, seconds | 0.143 | 0.068 |
| Entered native frames | 11,562,961 | 11,570,124 |
| Packing sampled mean, nanoseconds | 134.7 | 136.8 |

Counters are the last timer snapshots before harness termination, not an atomic
snapshot at the exact Done log event. Packing mean times event count estimates
approximately 1.56/1.58 seconds, with sampling and preemption uncertainty; it
does not include the later buffer deallocation. The histogram places only
1,022 frames in the 32..63-cell bucket; all others are smaller. It cannot
separate exactly 32 cells from 33..63.

Evidence directories are `%TEMP%/rava-product-boot-` followed by
`93901ad3ce2743c39a77b21fe9ea131f` and
`d8ec13f349a842d8a3c6a878064e610d`. Each contains `result.json`, `aot.jsonl`,
environment, binary/fixture hashes, and guest logs.

The large first-process delay lies inside object file reads; the cause of those
reads being slow is not established. On repeat, cold work falls below a second
but full execution remains over 42 seconds. Argument-buffer allocation is a
bounded candidate, not a plausible explanation for the entire remaining gap.
Next investigate generated code and transitions between native methods and
runtime callbacks before spending another full preparation cycle on key hashing
or a buffer-only rewrite. The sub-five-second/no-adapt goal remains unproven.

## Isolated call bridge, 2026-09-05

`bench/aot_call_cost.ps1 -Exe <executable>` compiles `AotCallCost.java`, prepares
its objects, confirms actual AOT entry for `calls`, `mix`, and `inline`, then
compares fresh processes against Java for two seeds and both execution orders.
The timed runs disable tracing and use a two-billion JIT threshold. Both loops
perform the same wrapping integer recurrence for two million iterations. Each
step is bijective in the previous integer value, preserving input dependence.
The single-call loop invokes a tiny same-class static method each iteration;
the other spells out its arithmetic. Main is unsupported and interpreted.

On executable C5F06B5A...694D, four runs measured 798..844 ms for the call loop
and 3..4 ms for the inline-body loop. Results match Java and differ between seeds
(`1973430073`, `1948435507`). The coarse guest timer limits precision of the
short measurement. Evidence:
`%TEMP%/rava-aot-call-cost-18021498b78848b7a71f7b1ec5dbcee0`.

This isolates a severe call-path penalty in a deliberately tiny workload. It
does not establish the frequency/cost of equivalent eligible calls in full boot,
nor prove that source translation to Rust would remove them automatically.
The emitter routes invokes through `rava_invoke` with packed arguments and
root spills; `rava_invoke` enters `Vm::jit_call`. A useful next tracer is static
inlining of structurally proven same-class numeric leaf methods, preserving
Java integer semantics and rejecting recursion, exceptions, synchronization,
and effects. Eligibility must derive from classfile and bytecode, never these
benchmark names or measured outcomes. Broader linking/class-initialization and
GC proofs are required before extending this to arbitrary method graphs.

## Static numeric leaf implementation

The ClassCompiler now resolves same-class MethodRef targets with static,
non-synchronized, non-native, non-abstract flags and a descriptor containing
only int arguments and an int return. Bodies are limited to 96 bytes and 16
arguments. The structural stack validator permits argument loads, integer
constants, add/subtract/multiply/bitwise operations, negation, and one terminal
return. It rejects branches, calls, exception tables, object operations and
division. The emitter substitutes I32 arithmetic with an I64 sign-extended
result, preserving Java wrapping semantics. This path does not allocate guest
objects, call callbacks, or introduce a class initialization boundary.

Artifact keys now incorporate same-class method bodies, descriptors, flags,
local/stack limits and handler presence, in addition to the previous caller,
pool and executable identity. Callee edits cannot reuse a caller compiled with
its previous body. This is content compatibility, not history-based admission.

Validation on release SHA256
`DCE754F1FCEEA44289EB8F2B492EA584EC07D0129A638ACC457222C21666F42E`:

- 36 AOT unit tests pass, including native execution with no invoke callback,
  changed-callee key invalidation and structural refusal cases.
- `aot_call_cost.ps1 -ExpectInline`: both loops measured 3 ms in all four runs,
  with both Java results preserved and no separate AOT entry into the leaf.
  Evidence: `%TEMP%/rava-aot-call-cost-ada9c752a2da48009572022d0502c547`.
- All 30 semantic probes match Java -Xint using prepared probe-class objects;
  library methods without prepared objects still fall back. Evidence:
  `%TEMP%/rava-inline-sem-ee4de9577003446a8ab786b726a722f0`.
- Prepared-entry/fresh-initialization checks pass in
  `%TEMP%/rava-prepared-db844f35994e40d5b8bc0563aa923b84`.
- Constant-pool version 1/2/2/1 cache checks pass in
  `%TEMP%/rava-aot-pool-cache-58fce946d9024d9eacbe1bb66d793a88`.

This closes the isolated numeric call penalty. Full classpath preparation,
server/reload hash gates and process-to-Done measurements for this executable
remain outstanding. The AOT backend remains opt-in; neither the overall speed
goal nor whole-program no-adapt execution is certified.

## Full server validation of numeric inlining

The DCE754F1...F42E executable prepared the full classpath into
`%TEMP%/rava-inline-server-1e7ffc0b0fd84248baff18e6384f8dcf/objects`:
30,443 classes, 269,382 compiled methods, 79,609 refused methods. The unchanged
method count does not measure how many calls were inlined.

All 30 semantic probes pass with these full-classpath objects. ChunkFill,
RingScan, RingLane, RingLaneInstance, NbtSave, and both ReloadBench hashes pass
in `%TEMP%/rava-inline-validation-ed193d4fcb1f440d86ac06bd13cba378`.
ReloadBench process elapsed was 65,773 ms; this is not full server boot time.

Uninstrumented, fresh fixture-copy process-to-Done measurements:

| Mode | Wall seconds | CPU seconds | Evidence suffix |
| --- | ---: | ---: | --- |
| Prepared first | 49.514 | 41.906 | f3960d93922b46c696c727502580889d |
| Prepared repeat | 42.646 | 42.922 | 99c5b6914f074a58912b688cca783f41 |
| Default control | 27.103 | 29.109 | 52a50a05213c4d3992c4e39d3b57e76e |

Evidence directories use `%TEMP%/rava-product-boot-<suffix>`. All reached Done.
Localized performance counters were unavailable, so quiet-box verification
was not established. This small series does not establish a precise percentage
change against older runs. It does show no material full-boot improvement from
the isolated numeric-leaf win, and prepared execution still loses to default.

Do not broaden this optimization merely by adding more numeric opcodes without
evidence of relevant execution coverage. Next identify the actual remaining AOT
runtime transition population and code costs; simple leaf-call microbenchmarks
are no longer sufficient evidence to prioritize the full-boot path. Keep the
backend opt-in. The <5s/no-adapt/no-snapshot goal remains unmet.

## Symbolized prepared execution profile

Built `release-diag` with `--features next-gen-gc` and `/OPT:NOICF`, without the
`wall-profile` feature. Prepared matching objects in
`%TEMP%/rava-inline-profile-322bb016e5674347b46026d0bebd11d3` and copied the PDB
beside each owned executable. PC stacks sampled every 3000 microseconds,
unfiltered; dumps every 3000 milliseconds. Only timestamps between process
launch and the harness Done observation were analyzed. The harness waited for
the final dump after Done, so filtering out the extra samples is mandatory.

First run: `%TEMP%/rava-product-boot-7102f08b2c59422ebaceb7dc3cf1605b`,
73.225 instrumented wall seconds, 70.000 CPU seconds. Of 15,647 samples,
6,885 name NtCreateFile. As in the earlier exact elapsed counters, first-use
object reads dominate the profile; their underlying delay is not diagnosed.

Repeat: `%TEMP%/rava-product-boot-01165b11277c4ffea01d55b3e0e853b6`,
43.862 instrumented wall seconds, 58.391 CPU seconds. Of 9,609 samples:

| Leaf | Samples |
| --- | ---: |
| LocalKey::with (multiple instantiations) | 351 |
| Vm::jit_call_body | 349 |
| Vm::jit_direct_probe | 245 |
| mi_malloc_aligned | 235 |
| memchr | 225 |
| Vm::invoke_framed | 214 |
| Vm::jit_direct_call | 204 |
| NtCreateFile | 189 |
| unresolved host address | 169 |
| invokevirtual | 163 |
| dispatch_owner_cascade | 158 |
| RtlQueryEnvironmentVariable | 145 |
| seam_plan::derive | 100 |

The unresolved group is 1.76%; it may include unregistered generated AOT code.
Do not use the generic symbol shortener on unresolved address strings: it can
turn them into an empty string and conceal their count. `ranked.json` in the
repeat directory preserves this group. No captured frame names rava_sampler.
The first run's temporary ranked.json used the lossy shortener; its unknown
count of zero is invalid and must not be cited as symbol coverage evidence.

TLS instantiations include valkey memo (90), native-backed-class lookup (76),
NativePlan lookup (58), and argument-layout lookup (26). These are leaf samples,
not inclusive costs. `jit_direct_probe` is the real guarded dispatch lookup,
not merely a diagnostic despite its name. It already caches resolved targets
and forwards the receiver before representation/class checks. NativePlan and
seam-plan caches also already exist; adding another cache is not justified.

The profile supports investigating the general/native invocation boundary,
not assuming that more numeric inlining covers the workload. Environment reads
are visible, and AOT checks RAVA_AOT_TRACE_NULLRET per completed native method;
however no caller chain was verified for the environment-function samples.
Do not attribute all those samples to that flag or forecast a speedup from them.
Conservative stack candidates are not an unwind, only the primary target is
sampled, PC mode still arms region guards, and quiet-box verification remains
unavailable. These measurements do not replace the 27.103/42.646-second
uninstrumented default/prepared control pair.

## Diagnostic environment gate

AOT result decoding now reads RAVA_AOT_TRACE_NULLRET through the existing
`jit::env_gate` and a dedicated OnceLock rather than querying the OS environment
after every executed method. The shared gate retains its existing memoization
kill switch. Configuration is applied before VM startup; no runtime admission
rule or Java result depends on this diagnostic flag.

Release build, prepared-entry/fresh-initialization checks and the numeric inline
probe pass. A separate two-call null-return probe confirms actual AOT entry,
identical `null` outputs, zero trace records with the flag absent and two with
it enabled. Evidence: `%TEMP%/rava-null-trace-09579f137d2f4e33ac938724ae86013e`
(`loop-False` / `loop-True`). The initial trivial-return fixture bypassed AOT
and was rejected as execution evidence; the loop-shaped fixture exercises it.
Other evidence: `%TEMP%/rava-prepared-f643458086e9465e957a1693da0e0380` and
`%TEMP%/rava-aot-call-cost-0c5cc9b8c46b4e2e95f1ac3b50a85c85`.
Full-server performance for this executable has not yet been measured.

Source inspection also explains a possible cause of recurring seam-plan derive:
the 4096-row table permanently claims a row for its first site, and colliding
sites cannot publish a plan. This is not yet a measured collision population;
do not assume all derive samples are collisions (wide descriptors are another
cause), or broaden the table based on this observation alone.

## Full boot after caching the diagnostic flag

Release SHA256 `A1BF61D10B3FCEFDB75F76632F73B981A335D6CD38122CFF9C2D4148F850DF1D`,
prepared artifacts in `%TEMP%/rava-gate-server-07117e7cba904fa492c9a19798200e61`.
Preparation: 30,443 classes, 269,382 compiled methods, 79,609 refused methods.
Uninstrumented fresh-world-copy runs all reached Done:

| Run | Wall seconds | CPU seconds | Evidence suffix |
| --- | ---: | ---: | --- |
| New first process | 76.050 | 41.219 | 0b20223efc874f7295feba77bd176eb5 |
| New repeat | 38.837 | 38.875 | 778251a6c3904546ac63ea12fb0b3aef |
| Old DCE754F1 control | 42.064 | 42.094 | d91494865bd24744a38f6133aaea409f |
| New control | 38.967 | 39.062 | ab390580290749acb869ec88439e608c |

Each suffix is under `%TEMP%/rava-product-boot-`. The old executable used its
own preserved prepared objects. The adjacent old/new comparison improves by
3.097 seconds; the two new repeats agree closely. This is a small series with
quiet-box counters unavailable, not a universal percentage guarantee. First-use
delay remains substantial. The new prepared backend still loses to the earlier
27.103-second ordinary-mode control and remains opt-in.

## Open no-adapt violation found in the invocation path

`vm/jit_call_body.rs` stores P_GETTER_REFUSED in the seam-plan word after the
first resolved receiver body is not recognized as a trivial getter. The source
explicitly allows this first receiver to suppress recognition for a different
class later reaching the site. Unlike P_NO_GETTER (a descriptor-derived proof),
this is history-dependent optimization admission. It must be removed or replaced
with a proof scoped to the actual resolved receiver/method. It is not acceptable
to certify whole-program no-adapt while that site-wide refusal remains. Next
remove this latch, retain descriptor-based rejection, and verify the resulting
behavior and wall cost. Speed-only changes do not close this requirement.

### Site-wide negative getter latch removed

Removed P_GETTER_REFUSED and mark_getter_refused from the runtime. Getter
eligibility in the seam plan now uses only P_NO_GETTER, derived from the
immutable descriptor. Recognition failure for one resolved receiver is no
longer published as a refusal for every future receiver at the site. Positive
getter entries retain their existing receiver/representation guards; their
capacity and admission behavior require separate review before any claim of
whole-runtime no-adapt compliance.

Release SHA256 `58656AC94E04C1E443D082B9EAE473CDC834C997F21173E4B2AB4C7B3EBC41B0`.
The release builds and all 31 semantic probes match Java -Xint, including new
GetterOrderSem (computed/trivial getters alternate through one call site while
fields change between phases). That probe is semantic coverage, not a trace
proving optimization admission. The removed state/write/read are established by
source inspection. A trial unit-test filter matched zero tests; it provides
compilation evidence only, not a passing getter unit suite.

Evidence: `%TEMP%/rava-getter-no-history-5a968fce6dc445ecad11c7e5b1f6f708`.
Uninstrumented ordinary-mode boot reached Done in 27.514 seconds, 29.328 CPU
seconds, peak RSS 1,159,393,280 bytes; directory
`%TEMP%/rava-product-boot-6c966829b353407bb4c6616b7d78117c`.
Quiet-box verification was unavailable. One observation does not establish an
exact regression percentage against the previous 27.103-second control.
Full prepared-classpath and product hash gates for this executable remain
outstanding. Neither sub-five-second performance nor whole-runtime no-adapt is
achieved by closing this one violation.

### Positive first-receiver cache retired

Further source inspection confirmed that the positive trivial-getter table
permanently keeps the first successfully recognized receiver class per site.
Class guards preserve correctness, but do not make that optimization selection
independent of receiver history. Its shared `enabled()` gate is now constant
false: neither runtime recording nor JIT emission can activate the table.
Environment flags cannot re-enable it. The historical implementation remains
marked retired until replaced/removed; ordinary guarded method dispatch and
compilation continue serving getters.

Before editing, the existing RAVA_NO_TRIVIAL_GETTER switch on the 58656AC9...
binary reached Done in 27.610 seconds, 29.656 CPU seconds; evidence
`%TEMP%/rava-product-boot-3e2bfa1f7860424da3f85ce7ae95d318`.
This follows the 27.514-second enabled observation, and is a small series rather
than a precise price of the optimization.

Release SHA256 `ED79742D38DBB0A30D9D610E5E98B30C4D7DC8A1D0FAA24A7434AA4D597CAFEC`
builds and all 31 semantic probes match Java -Xint with ordinary configuration.
Evidence: `%TEMP%/rava-retired-getter-935fb17521144f22bbd5e305cacf6553`.
An uninstrumented boot reached Done in 28.011 seconds, CPU 30.328 seconds, peak
RSS 1,135,104,000 bytes; directory
`%TEMP%/rava-product-boot-e9b665c8056649eca5d5a9c564cf4eee`.
Quiet-box counters were unavailable. These measurements show no startup
speedup; the change removes another history-selected optimization path.
Full prepared-classpath validation remains outstanding. General JIT admission
and fallback policy still prevent whole-program no-adapt certification.
