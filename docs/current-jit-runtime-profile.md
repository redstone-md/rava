# Current first-entry runtime profile

## Direct-mono resolver outcome census (2026-09-10)

An isolated `world_pristine` boot reached `Done` with no runtime adaptation or
snapshot path. The diagnostic-only build was `release-fat`, `next-gen-gc`,
`target-cpu=native`, using the fixed `pgo-fresh-explicit-tight.profdata`
profile. The profile predates this source and emitted function-hash mismatch
warnings; this run is evidence about counts, not launch performance. Executable
SHA256: `a66b8767f7183a344af62eeacf0e2af91904933a9bdde6bfa48624ce735a6022`.
Instrumentation: `RAVA_DIRECTMONO_CENSUS=1 RAVA_DIAG_EVERY_MS=250`, with no
`RAVA_DIAG` selector.

At the last periodic report before `Done`:

| Population | Calls | Aggregate wall ns (preemption-inclusive upper bound) |
| --- | ---: | ---: |
| Entry cache `-1`, main | 4,358,621 | 8,358,567,720 |
| Entry cache `0/+`, main | 1,746,217 | 16,483,843,803 |
| Entry cache `0/+`, worker | 4 | 271,327 |
| Main `direct_mono_resolve` returned `None` | 1,742,977 | 102,595,813 |
| Main resolver returned compiled fptr | 2,732 | 2,945,633 |
| Main resolver returned permanent negative | 508 | 325,475 |
| Main resolver total | 1,746,217 | 105,866,921 |

`None` is a repeatable dynamic outcome, not a distinct-site count. Resolver
time is only about 106 ms aggregate wall, so caching or accelerating resolver
outcomes cannot plausibly close the multi-second CPU gap. The two callback
columns include `jit_call` and nested guest work, can overlap, and are not CPU
measurements or fully removable cost. `RAVA_FALLBACK_TALLY` uses a shared map
lock and is diagnostic-only; its output is for ranking sites, not latency.

A second isolated run used `RAVA_DIRECTMONO_CENSUS=1`,
`RAVA_FALLBACK_TALLY=1`, `RAVA_JIT_STATS=1`,
`RAVA_JIT_STATS_MS=10000`, and `RAVA_DIAG_EVERY_MS=250`. It reached `Done`,
but the extra map writes and JIT instrumentation invalidate its wall time. The
last direct-mono census reported 4,360,485 main-thread `-1` arrivals
(6,103,763 direct-mono callbacks total). Top direct-mono rows from the tally:
Executable SHA256: `d4000fffe1bc1c0e29c28871106711aa69f6bb3d3b747e17fc65efc42619b4cb`.

| Calls | Site |
| ---: | --- |
| 1,359,930 | `com/google/common/math/MathPreconditions.checkNonNegative(String, I)` |
| 460,210 | `Reference2ObjectArrayMap.<init>(I)` |
| 460,185 | `Reference2ObjectArrayMap.<init>(Reference2ObjectMap)` |
| 453,652 | `IntMath.gcd(I, I)` |
| 226,316 | `IntMath.gcd(I, I)` (another call site) |
| 150,820 | `Reference2ObjectArrayMap.findKey(Object)` |
| 97,611 | `HashCommon.mix(I)` |
| 83,539 | `ObjectArrays.checkElementNotNull(Object, I)` |

These rows' best-effort `why=compiled(direct-miss)` label only says the loader's
JIT cache knows a compiled body; it does not identify why direct-mono refused
the site. This is not a pure native-only population. Keep the cacheability
exclusions intact until each hot site's negative reason and required dispatch
semantics are proven. A descriptor fast path is still deferred.

An isolated repeat with `RAVA_JIT_STATS=1` (which also enables the shared
resolver-reason counter), `RAVA_DIRECTMONO_CENSUS=1`,
`RAVA_FALLBACK_TALLY=1`, and `RAVA_NEVER_EVERY_MS=1000` reached `Done`.
It is diagnostic-only; the extra counters/maps make wall time invalid. The
final direct-mono census reported 6,105,624 callbacks, of which 4,359,124
arrived at main-thread `-1` entries. The shared resolver reported 1,401
permanent negatives overall: `java/*` 549, `is_cache_class` 1,
`is_int_set` 7, `force_native` 18, and `native_served_deep` 826. These are
fill-time resolver outcomes across all callers, not dynamic direct-mono call
counts.

The source trace identifies the hot Guava and fastutil rows above as the raw
prefix arm of resolver clause 5, not `jit_native_served_deep_keyed`:
`native_served_prefix` includes `com/google/common/` and `it/unimi/`, while
`class_loader::note_bytecode` registers overrides only for `java/` or an
explicit `RAVA_GUEST_CLASSES` allowlist. With that allowlist absent, these
classes have no override bit, so the namespace guard permanently rejects the
direct binding even when a compiled body exists. `NEVER-NAMES` runtime rows
also show clause-5 records for `IntMath#gcd`, `MathPreconditions`,
`ObjectArrays`, `Reference2ObjectArrayMap`, and `HashCommon`; the source
branches distinguish their prefix reason from the clause's deep sub-arm.

Do not remove the namespace guard wholesale or add per-owner exceptions.
Native-backed classes can depend on synthetic fields, and previous relaxation
experiments exposed wrong-method/native-state failures.

### Paired hot-site route census (2026-09-10)

A third isolated `world_pristine` boot used baseline `RAVA_JIT=1`,
`RAVA_JIT_TIER0=1`, `RAVA_HARD_MB=3584`; diagnostics added
`RAVA_DIRECTMONO_CENSUS=1`, `RAVA_FALLBACK_TALLY=1`, `RAVA_JIT_STATS=1`,
`RAVA_JIT_STATS_MS=10000`, `RAVA_DIAG_EVERY_MS=250`, and
`RAVA_NEVER_EVERY_MS=1000`. It reached `Done`;
the diagnostic maps and counters invalidate its wall time. It used the
disposable world copy and did not mutate the repo's `world`. Diagnostic
executable SHA256: `0bcccf1529f46bb519da7b217642736b0d519f1909b8f4399b51e89d7b692b95`.
The last route-tally header was 4,781 sites and 11,187,815 total tally events.

For each caller/cp/op key, `negative` counts `-1` cache arrivals and `cold`
counts zero-word callbacks. `fallback_entry` is the matching `Kind::Invoke`
count, recorded only when the inner compiled direct-call/AOT early-return checks
miss; its note precedes later structural/native fastpaths. `pre_fallback` is
`negative + cold - fallback_entry`; it prices route shares, not CPU savings.

| Caller site | Target | Direct-miss arrivals | Fallback entries | Before fallback |
| --- | --- | ---: | ---: | ---: |
| `IntMath.cp90` | `MathPreconditions.checkNonNegative(String, I)` | 1,359,932 | 0 | 1,359,932 (100%) |
| `Reference2ObjectArrayMap.cp61` | `<init>(I)` | 460,217 | 0 | 460,217 (100%) |
| `StateHolder.cp243` | `Reference2ObjectArrayMap.<init>(Reference2ObjectMap)` | 460,186 | 64 | 460,122 (99.99%) |
| `Shapes.cp103` | `IntMath.gcd(II)` | 453,628 | 0 | 453,628 (100%) |
| `DiscreteCubeMerger.cp24` | `IntMath.gcd(II)` | 226,296 | 0 | 226,296 (100%) |
| `Reference2ObjectArrayMap.cp1` | `findKey(Object)` | 150,755 | 0 | 150,755 (100%) |
| `Reference2IntOpenHashMap.cp166` | `HashCommon.mix(I)` | 97,998 | 0 | 97,998 (100%) |
| `ObjectArrays.cp73` | `checkElementNotNull(Object, I)` | 83,458 | 64 | 83,394 (99.92%) |
| `Joiner$MapJoiner.cp80` | `Joiner.access$100(Joiner)` | 41,944 | 41,944 | 0 (0%) |

The high-volume Guava/fastutil sites overwhelmingly do not reach the fallback
tally, consistent with the nested direct-call cache (the per-site counts do not
separate it from an early AOT return). `Joiner.access$100` validates the
opposite case: every counted arrival reaches the fallback tally. Thus the
negative direct-mono cache is not making these hot sites enter the interpreter
fallback. Bypassing the nested route would still execute each guest target.
These counts price route incidence, not dispatch cost, and therefore do not
substantiate a multi-second saving. Keep the namespace guard and native dispatch
intact; do not admit a direct-binding optimization from this route census alone.

## Execution refresh after exception infrastructure (2026-09-06)

Source `dad6f9e2`, built with `cargo rustc --profile release-diag -p rava-cli
--features next-gen-gc --target-dir D:/code/rava/target -- -C link-arg=/OPT:NOICF`.
PC sampling works without `wall-profile`; enabling that feature would add
per-call observers and is unnecessary. Ordinary release was not replaced.

Evidence: `%TEMP%/rava-execution-profile-319b5ff77f554e23b085eef0182c1e82`.
Fresh-world run: `rava-product-boot-5c967ac1dbc54ed5b8090e655d9ac360`.
Executable SHA256: `FC656CB3D0B2ACE7647A45E3E1811A4807E9C9929F3FABFFB7C9DB084B805161`.
Sibling PDB SHA256: `C38565C49AF2016CD5E6B97D70A250F808FF50C5F427E8C0B470E2FE36585DDA`.

Full Done reached at instrumented wall 29.610 s, CPU 52.5 s. This is not a
production performance result. Quiet counters were unavailable. Analysis clips
samples to recorded launch/Done epochs: 20,894 primary-thread samples, first
sample 1,183 ms after launch, 53 unresolved host samples, 2,719 registered guest
JIT samples. Post-Done samples are excluded.

Leading grouped leaves: LocalKey::with 776 (different closures, not one TLS
operation), mi_malloc_aligned 598, jit_call_body 573, memchr 483,
jit_direct_probe 453, NtCreateFile 430, jit_run 408, fu_fastpath 402,
dispatch_owner_cascade 331. The three named JIT boundary/decision functions
account for 1,434 samples (6.9%); this is sampled location share, not a promise
that eliminating their entire cost is possible. The broad distribution agrees
with the earlier profile below; there is no newly dominant execution leaf.

Prioritize removing repeated execution-boundary work over more dormant exception
metadata helpers. Handler-entry counts do not establish handler execution time.
Inspect the generic compiled-call fallback and its direct probe together; retain
rooting, receiver barriers and exception behavior. A runtime candidate still
needs uninstrumented interleaved comparison before any speed claim. This refresh
does not establish a speedup or satisfy the no-adapt / below-five-second goal.

Source HEAD 5462eb76, runtime unchanged since f800384f. The prior background
lambda experiment was removed. The full wall <5 s, no-adapt, no-snapshot goal
remains unmet. Historical opt-level measurements in boot-speed-ledger row 62
showed compile savings offset by slower execution; do not repeat that tuning
without a new causal measurement.

Final diagnostic build: cargo rustc, release-diag, next-gen-gc, /OPT:NOICF.
The extra wall-profile feature was omitted from this final build. A preliminary
build with that feature was not used for either capture. Normal release was not
rebuilt or replaced. Standard first-entry harness: cap 16384, default early queue
reservation, fresh world fixture, affinity 0x0FFF, port 25599.

Executable SHA256:
`3D437A35DF5DE0DB6FA634FC52F1778191A3E726A8225978B1E44E1254E0BBD6`.
PDB SHA256:
`A65088BA806996A2357B981C16199FE065C5699F022B142A5D9E37256C050F4E`.
Artifacts: `%TEMP%/rava-current-profile-748ee7bf5ce14c209a1281be24f9f40a`.

Primary capture: isolated run `rava-product-boot-4056d6bddcd94b46b869d7bb2b5b8d52`.
Instrumented Done wall 28.740 s, CPU 50.921875 s. Of 20962 samples between launch
and Done, 2689 (12.8%) fall in registered guest JIT ranges; 58 host samples remain
unresolved. First sample is 367 ms after launch. Post-Done samples are excluded.
These are primary-thread locations, not all-thread CPU or an upper bound on
removable interpreter overhead. Quiet-counter sampling was unavailable.

| Grouped leaf | Samples |
| --- | ---: |
| LocalKey::with, all instantiations | 773 |
| mi_malloc_aligned | 620 |
| Vm::jit_call_body | 528 |
| NtCreateFile | 435 |
| memchr | 434 |
| Vm::fu_fastpath | 420 |
| Vm::jit_direct_probe | 416 |
| Vm::jit_run | 415 |
| dispatch_owner_cascade | 347 |
| Vm::jit_direct_call | 323 |

Raw TLS instantiations separate valkey fold (205), checkcast_reject memo lookup
(165), hr_synth plan lookup (99), argument layout (43), and flat_note_hot (33).
Do not treat this as a single TLS-access bottleneck; closure work is included.

A second instrumented capture adds PC_STACKS and filters allocation leaves.
Run `rava-product-boot-97fa3a6c190047fdab516be1dbc15f90`: Done 30.157 s, CPU
66.34375 s. In-window mi_malloc_aligned leaf population is 298. Stack-window
scanning finds candidate return addresses, not a true unwound call stack.
Candidate chains include String growth/write_str (49), fu_fastpath (23),
jit_indy (15), stream vector growth (11), map_method (8), and native argument
string canonicalization (7). These counts do not price complete operations.

Source corroboration: fu_fastpath constructs temporary Vec<Value> arguments in
its native Map and Optional arms (and DoubleArrayList.wrap). Map additionally
canonicalizes arena strings before calling map_method; the NativeArgsGuard and
any reentrant guest calls must retain their lifetime/root contracts. This is a
concrete next investigation target. Avoid replacing a heap allocation with a
large hot-function stack frame without measuring the tradeoff; prior outlining
work showed that frame size itself matters. No runtime change is justified by
caller candidates alone, and this profile establishes no new speed gain.

## Rejected: forward nested direct entries into direct-mono sites (2026-09-10)

A tracer attempted to promote an already-resolved static/special `DirectEntry`
into the outer `emit_direct_mono` cache after the cold callback. The gate used
only structural entry facts and preserved the worker publication guard. A
source-level safety review found no ABI, keying, or class-init mismatch against
the existing direct-mono resolver, but that review does not explain the runtime
failure below.

The candidate was built as `release-fat` with `next-gen-gc`,
`target-cpu=native`, and the fixed `pgo-fresh-explicit-tight.profdata`
(`9cc4674248b09e367ae6e3d48d14a18ee31b04f17f19adfcaba02e3591dba9a6`).
Disposable `world_pristine` A/B runs used strict no-adapt settings, no snapshot
path, and an isolated port. First ABBA: all runs reached `Done`; baseline
median external wall was 26.38 s versus 25.83 s for the candidate. This
difference is far below the documented 16.7% boot spread and is not evidence of
a speedup. The normalized level data matched except for volatile `LastPlayed`.

In the second ABBA, baseline runs A3/A4 and candidate B3 reached `Done`, but
candidate B4 did not: it logged an unexpected
`invokevirtual java/math/RoundingMode.ordinal()I` ULE in `IntMath` and exited
with SIGSEGV. The independent source review found no direct route from that
site to the promotion gate, so causation is unproven; nevertheless this
candidate did not satisfy the reliability bar. The focused `fallback_tally`
unit test and product build passed, but no broad guest-parity/GC suite or
repeated no-crash boot series established the candidate as safe.

The source change was reverted. Do not ship or cite this tracer as a
performance improvement; the launch-to-`Done` <5 s goal remains unmet.
