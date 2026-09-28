# Performance Roadmap

Integrate handler execution through an explicit native-exit contract. First
preserve non-Throw errors at the existing deopt boundary and reject unsupported
nonterminal exits explicitly. Keep Throwable roots and first-failure ordering.
Then wire protected instruction exits; handler-bearing admission remains closed
until emitted locals, pending state and worker exclusions are complete.

Rejected removal of repeated static/special resolution for handler-bearing Code:
paired wall deltas +0.163/+0.893 seconds. Runtime restored; further work must
address execution coverage rather than pursue this refusal-cache experiment.
See docs/handler-direct-refusal.md. Full no-adapt and wall below five seconds remain unmet.

Current-source fixed-profile smoke, 2026-09-28: source `b5d7b83` built as
`release-fat` with `target-cpu=native` and the existing
`target/pgo-current-head.profdata` reached guest `Done` in 25.0 s in an
isolated fresh-world directory under the canonical JIT/Tier-0/hard-memory
environment, with no snapshot or runtime-adaptation override. The build emitted
many profile function-hash mismatch warnings, so this is not exact-source PGO
evidence and is not comparable to row 740's 21.43 s product. It is a current
source smoke only; no strict no-adapt certification or <5 s result follows.

Exact-source real-boot PGO cycle, 2026-09-28: profile-generation source
`b5d7b83` reached Minecraft `Done` on a fresh world before the 120 s counter
dump (`rc=0`). The merged profile contains 19,658 functions, 377,154 blocks,
and 113,183,229,053 counts (SHA-256
`1b217a36acc68318fe9084c53bc97377d483bcb681362a7a35266f4ad4e5c3cc`). Its
`release-fat` profile-use product (SHA-256
`a35dfa1a2634a0a2a3445c22439f23118841fb61703a4b8e47b8f79c6cd7e0da`) reached
`Done` in 24.1 s on a separate fresh world with canonical fixed Tier-0 settings,
no snapshot, and no extra adaptation overrides. This is one launch, not an A/B;
it misses <5 s by 19.1 s and does not certify the remaining name-routed
policies as strict no-adapt.

### 2026-09-28: current-product loaded-family profile

A symbol-retaining `release-diag` build using the exact-source profile reached
`Done` in 32.4 s under `perf`; this is a diagnostic run, not the 24.1 s
production `release-fat` result. Its cycle profile had 5,785 samples and zero
lost samples. Self-sample shares include 0.25% in
`ClassLoader::cached_native_family`, 1.21% in `dispatch_owner_cascade`, and
2.57% in the largest individual Cranelift symbol. The profile was captured
until process stop at 41.5 s, about 9 s after `Done`, and report attribution
was intentionally self-only because source/call-chain symbolization was broken.
These data do not price aggregate inclusive work, but expose no safe multi-second
loaded-family route; compiler samples remain distributed across backend
functions rather than identifying a bounded replacement seam. No optimization
is justified from this profile. Goal remains unmet; strict no-adapt is still
uncertified.

Exact current-artifact acceptance check, 2026-09-28: the verified profile-use
product (SHA-256 `a35dfa1a2634a0a2a3445c22439f23118841fb61703a4b8e47b8f79c6cd7e0da`)
ran once after a discarded warm-up and then four alternating launches from fresh
`world_pristine` copies under the isolated `env -i` JIT/Tier-0/hard-memory
environment. All four reached `Done`; launch-to-Done was 21.7–22.1 s (pooled
median 21.9 s). Both harness arms used the same binary and no flags, so this is
a repeatability check, not an A/B. It directly confirms the current artifact
misses the five-second gate by 16.9 s; name/count-based runtime routes still
mean this does not certify strict no-adapt. Full results: boot-ledger row 743.

Current-source phase replay, 2026-09-28: the exact profile-use artifact
(`a35dfa1a2634a0a2a3445c22439f23118841fb61703a4b8e47b8f79c6cd7e0da`) ran
`BootPhaseC` against an isolated copy of `world_pristine`. It attributed 13.035 s
to `Bootstrap.bootStrap`, 7.485 s to `WorldLoader.load`, and only 3 ms each to
the Yggdrasil service constructor and `Services.create`. The replay printed
through `STEM=ok` but exited 139 afterward, so it is diagnostic attribution,
not a clean boot result. A separate `BootstrapBench` run exited 0 at 13.227 s
with 29,671 states and the established hash
`b9e7debe631b42b6`, confirming the bootstrap computation's semantic result.
Bootstrap is the largest measured step, but it alone exceeds the target by
8.2 s; this locates the work and does not yet identify a safe removable path.

Focused bootstrap perf attribution used the symbol-retaining exact-source
diagnostic build, not the shipping timing artifact: 5,000 `cycles:u` samples,
0 lost, and the same 29,671-state reference hash. Its instrumented
`BootstrapBench` time was 18.686 s, so do not compare it to the 13.2 s product
measurement. The largest native samples were diffuse: Cranelift regalloc
2.82%, egraph 2.45%, liveness 2.29%, `Heap::retire_array_slot` 1.90%, and
`Vm::jit_call_body` 1.77%. No single no-adapt-safe runtime seam can explain the
8.2 s target deficit; a smaller optimization is not justified by this profile.

## Objective

Reduce Minecraft boot and warm-reload wall time through structural, workload-agnostic changes while preserving exact Java semantics.

User deadline: 2026-09-05 21:00 Europe/Budapest. Acceptance remains full fresh
process startup to Minecraft `Done` below five seconds, no-adapt and no snapshot
speed path. ReloadBench process duration and its warm interval are separate
metrics and must never be reported as the startup wall. Latest exploratory
startup evidence and limitations: `docs/ir-resource-budget.md`.


### Receiver-aware native entry contract

Build a narrow adapter at an existing call-site seam before widening native
eligibility. The packed-cell ABI must distinguish arena objects, heap handles,
and unsupported/tagged values at runtime; forward movable references before
decoding, and root all live operands before any native call that may allocate.
An adapter may decline only before guest-visible mutation, in which case the
existing general dispatch remains authoritative. Once it serves or throws, it
must return through the normal result/pending-fault contract. Keep native-backed
entry refusals until each call path (direct, PIC, inline, AOT) has this complete
fallback proof; class identity alone does not prove representation or state
ownership. Reuse the existing typed PIC descriptor/refusal machinery where its
ABI matches, and extend only after the direct/PIC tracer bullet passes semantic
and performance checks.

This is an ABI architecture experiment, not a speedup claim. In particular,
inline and AOT paths remain strict until their entry/fallback semantics are
verified; no native-name refusal is removed merely to broaden compilation.

Current seam audit: the dispatch PIC already carries packed receiver cells and
has distinct arena-class-id and heap-class-word probes; its native descriptors
can refuse into the existing miss arm. Do not rebuild that ABI. The unresolved
case is positive compiled-body entries in `DirectEntry`/PIC caches when native
ownership varies across representations. A safe adapter needs to preserve the
normal symbolic-call resolution and initialization order, not merely add a class
tag or reuse the general rooted PIC thunk (which always serves via `jit_call`).

The focused direct/PIC audit found no valid compiled-fallback witness. Native
plans are admitted only for forced-native, native-map, or body-absent calls;
their `Unserved` outcome follows ordinary native missing/default handling, not
bytecode. The early iterator shortcut has no plan tied to a compiled body. Keep
all gates and do not turn `Unserved` into a direct-entry fallback without a new
explicit dispatch contract and cross-representation semantic proof.

## Current tracer-bullet cycle

### 2026-09-27: refresh the current-source PGO price census

The current-source profile-use product reached guest `Done` in 22.5 s on one
fresh, uninstrumented launch. A second diagnostic launch ranked `fu_fast`
(9.53 s estimated; 15.72M events) and `compile_self` (6.13 s estimated; 8,079
events, only 32 samples) highest. JSON estimates are p50 × event count, not
removable CPU time; `RAVA_DIAG=price` also instruments each fast crossing, so
neither number is a savings ceiling. Existing native-path and prepared-AOT
evidence still rules out a defensible new tracer; keep worker/native admission
gates. Full artifact identity, census limits, and conclusion: ledger row 723.


### 2026-09-27: remove default lambda hot-count admission

The default-on flat-lambda compile request used a per-`Code` TLS counter and
waited for 512 applications before calling `force_compile`, despite Tier-0's
fixed first-entry policy. Replace that history threshold with a one-shot
first-use memo keyed by stable `Code` identity; worker threads still cannot
compile or consume the pending coordinator request. Direct comparator/key
lambda sites now forward once rather than up to 4,096 counted calls.

The focused policy test passes, and `bench/CallShapes.java` produces the same
`acc=469333328` under HotSpot and Rava. Two isolated no-snapshot launches of
the unprofiled release build reached `Done` in 36.8/36.7 s; these are smoke
measurements, not comparable to the 22.5 s current-PGO baseline. The <5 s
acceptance remains unmet.

### 2026-09-27: major rewrite boundary and tracer gate

The user authorized a major Rava rewrite, with the original acceptance unchanged:
exact Java semantics, no runtime-adaptive routing, no snapshot speed path, and a
fresh-process launch to Minecraft `Done` below five seconds.

The ownership audit supports a two-part design, not one oversized executor:
`ExecKernel` may own per-OS-thread activation state (frames, pending calls,
green-thread state, temporary roots, and exception state); `KernelShared` must
retain the heap, loader, GC-visible global roots, JIT publication/caches, GIL and
region coordination. Existing `ThreadContext`, `WorkerContext`, and per-thread
cache modules are extraction precedents, not proof that a wrapper saves time.
`SharedVm` remains an alias for `Vm`, not a safety boundary. Keep the
`repr(C)` JitCtx/JitThreadCtx layouts unchanged; moving-GC root addresses stable;
class initialization and loader epoch publication ordered; and worker barriers
fail-closed. No execution-site token or first-use pilot is allowed.

**No vertical slice is selected yet.** The current-source audit found no
multi-second removable-work tracer in the execution kernel (ledger row 726).
The current product needs more than 7.6 CPU-s less work even under ideal
six-physical-core scaling (row 680); ownership extraction alone has no measured
ceiling and is not a performance result. Do not implement a facade or move
thread-local fields merely to claim rewrite progress. Build a slice only after
its generic mechanism has a source-backed multi-second ceiling and a measurable
end-to-end path to `Done`; preserve the root/exception/safepoint proof and verify
semantic parity plus GC stress before performance claims.

The fixed first-use compile request for flat-served lambda bodies is now
explicitly opt-in (`RAVA_FLAT_TIER=1`) after a same-binary, four-pair PGO
ABBA found the request added **5.92 s wall / 8.02 CPU-s**; all eight fresh
no-snapshot runs reached `Done` (ledger row 728). The request remains
history-free when enabled, but the faster default still reaches `Done` in
about 21.4 s, so this is a measured regression fix, not a path to the <5 s
objective. No-snapshot startup below five seconds remains unproven.
The tradeoff is real beyond startup: `CallShapes` measured flat lambda calls at
**327–354 ns** by default versus **74–79 ns** with the fixed-tier opt-in, with
the same accumulator (about 4.5× slower in this lambda-heavy microbenchmark;
ledger row 730). Default-off favors launch wall; retain `RAVA_FLAT_TIER=1` for
workloads prioritizing lambda throughput.

The rewrite-scale search found no qualifying substrate path. Fixed-code caches,
static first-use compilation, CNP, and host-runtime reuse can remove at most
2.7 CPU-s of compilation/linking work; prepared AOT was slower than the product,
and fixed-policy JVM runs took about 57 s (ledger rows 690, 703, 721, 725).
Generational/handle-table work is bounded at ≤2 CPU-s, interpreter dispatch at
about 0.1 CPU-s, and native crossing at 2–4 CPU-s (rows 299, 680, 411–414,
723–726). Even generous non-overlapping estimates total only 4–6 CPU-s, below
the >7.6 CPU-s reduction required; they are ceilings, not additive promises.
The remaining Cranelift-quality content-cache falsifier is bounded at 2–2.7
CPU-s and is not selected as the rewrite tracer. No new option has a defensible
end-to-end route to the target: do not start a runtime rewrite from these
estimates. See `docs/vm-shared-exec-plan.md:1049-1092`.

Supporting inventory: `docs/vm-shared-exec-plan.md`,
`crates/rava-interp/src/vm/thread_context.rs`,
`crates/rava-interp/src/parallel/context.rs`,
`crates/rava-interp/src/vm/gc.rs`, and
`crates/rava-interp/src/execution_state.rs`.

The user-selected external-runtime survey also found no compatible no-adapt,
no-snapshot path; primary-source compatibility and re-open criteria are in
`docs/external-execution-substrate-research.md`.

### 2026-09-28: deep-rewrite authorization and design gate

The user authorized a deep VM redesign while preserving the exact acceptance:
fresh-process `Done` under five seconds, no adaptation, no snapshot. Three
independent candidate audits found no qualifying tracer: single-store value
unification already ran bit-exact but regressed full boot by 1.4–2.2 s;
native/crossing redesign is bounded at 2–4 CPU-s against a >7.6 CPU-s
requirement; and a static class-initialization DAG exposes only sub-second
independent work while the CPU-heavy bootstrap mutates shared state. Keep the
goal open, but do not begin a rewrite without a source-backed workload ceiling
and a falsifiable semantic tracer. See runtime-gap-register.md, ledger rows
680, 726, 738, 742, and the class-init audit.

### 2026-09-28: exact-source PGO host profile and fixed-codegen A/B

The earlier 8.55% guest-JIT profile was invalid for product attribution: it
used stripped `target/release/rava` (SHA-256
`dc168db0ac59bc0c4e61626c6bf7d1c8bdda0883d762d852146bbfa61119cfb7`), not
the row-740 PGO product (`fbc41e8562297a59bb7f117282ef7304a7977fe04f892f12f4e7e20d8d8ebda7`).
Runtime source has not changed since row 740. A diagnostic symbolized PGO
profile-use binary was rebuilt from that exact source and trained on a fresh
server boot. Its SHA-256 is
`ccb1e0d393d7b8db552c3f29dd12a5aefd1f3d2d640277595eff73f60895e047`;
it is a debug-symbol-retaining diagnostic build, not the row-740 production
artifact.

The exact-source host profile reached `Done` in 21.532 s under profiler
overhead: 18,561 `cycles:u` samples at 499 Hz, zero lost; `rava` accounted for
85.81% of sampled cycles and guest-JIT native code for 9.10%. Cranelift,
regalloc, and frontend compiler symbols accounted for about 36.96% of sampled
cycles among reportable symbols. This is a real compiler workload signal, not
a demonstrated removable share or a wall-time savings ceiling.

Eight clean fresh-world launches on that same diagnostic binary compared
`RAVA_JIT_OPT_NONE` off/on in OFF-ON-ON-OFF order repeated twice. Median wall
was 21.647 s off versus 21.860 s on; means were 21.871 s and 21.927 s. Paired
on-minus-off differences were +0.404, -0.010, -0.329, and +0.159 s (mean
+0.056 s). There is no measured startup win; process CPU was not collected in
this A/B. Reject `RAVA_JIT_OPT_NONE` as a fixed startup policy.

An additional eight-run OFF-ON-ON-OFF A/B compared the default `speed` level
with `speed_and_size`, again on fresh copies of `world_pristine` and the same
diagnostic binary. `speed_and_size` median/mean wall was 21.786/21.775 s versus
21.888/22.010 s for `speed`; median/mean per-thread process CPU at `Done` was
37.855/37.743 s versus 37.935/38.103 s. Paired on-minus-off wall deltas were
-0.617, -0.186, +0.240, and -0.376 s (mean -0.235 s); CPU deltas were -1.16,
-0.60, +0.33, and -0.01 s (mean -0.36 s). The effect is small and inconsistent
relative to run variance, and nowhere near the required multi-second reduction.
Keep the shipped `speed` default; do not treat `speed_and_size` as a startup
optimization.

The structurally admitted Linux CNP tier was screened with eight fresh-world
boots (after a discarded warm-up), OFF-ON-ON-OFF repeated twice. This used
`target/release-fat/rava` SHA-256
`6bb3ed4da62e256e0d521d961cdecd7c03f4bb9b4cdf844f306a45a74ca0c61b`, a
non-PGO diagnostic binary, not the row-740 production PGO artifact. All runs
reached `Done`. CNP-on median/mean wall was 34.968/34.888 s versus
31.186/31.167 s off; per-thread process CPU was 54.715/54.993 s versus
51.580/51.350 s. Every paired on-minus-off wall delta was positive (+4.776,
+2.781, +3.866, +3.463 s); mean CPU delta was +3.643 s. Reject enabling CNP
as a startup optimization. Because this is not the production PGO binary, it
is negative screening evidence rather than an exact-product bound; a future
implementation change would need a fresh product A/B before reconsideration.

Existing backend boundaries and risks remain: the full JIT must preserve its
`JitFn` ABI, roughly 100 host callbacks, frame-cell GC-root registration,
latched exceptions, and module lifetime. The compiler-heavy profile alone does
not justify replacing Cranelift: no safe replacement has shown the ≥7.6 CPU-s
reduction needed to reach the <5 s goal. Keep the goal open and require a
source-backed, semantics-preserving tracer plus controlled full-boot evidence
before prototyping a new full-tier backend.

### 2026-09-05: identify full AOT inputs before offline preparation

The old full-AOT disk key omitted the constant pool: unchanged `ldc #n` bytecode
could reuse code for a different literal in another version of the same class.
Bind the cache key to pool contents, local-slot count and string-constant policy
as well as owner/bytecode. Verify two same-name class versions against one cache.
This is a correctness prerequisite for offline artifacts, not a startup win or
a complete artifact-compatibility contract.

Completed: old executable reproduces 101500000 instead of 200000000 after a
same-class literal change. The fixed executable passes version order 1/2/2/1
against one cache, with unchanged files on repeat; all 29 AOT unit tests pass.
See `docs/aot-pool-cache-identity.md`. Next separate pool-only compilation from
runtime linking and strengthen compiler/target artifact identity before offline
delivery; current cached execution is not yet a no-adapt offline pipeline.

### 2026-09-05: first-entry compiler work decomposition

First-entry boot remains about 45 s with either zero or ordinary inline budget;
synchronous first-entry compilation takes about 51 s. These switches do not
establish a faster admission policy. Extend the existing opt-in compiler phase
trace with per-invocation IDs and durations, including early returns. Measure
compiler interval coverage without confusing summed concurrent durations with
CPU time, or code generation with all preparation work before the driver.

Completed: trace-on/off first-entry CompSem matches 48 normalized reference lines;
three IR-budget refusals have end records. An instrumented full boot records
4,186 completed scopes and a 20.115 s interval union. Instrumented startup is
69.788 s; it is not a normal wall result or an AOT prediction. The overlap parser
check passes. See `docs/first-entry-compile-work.md` for data and limitations.

### 2026-09-05: static register ABI full-startup pricing

The current binary passes all five ABI semantic/GC modes. Two opposite-order
full-boot pairs favor ABI unset: on 31.681 / 32.915 s, unset 30.735 / 31.689 s.
A separate diagnostic full boot accepts 149 register entries under ordinary
threshold/inlining settings, so the route is not dormant. Defaults remain off;
no speedup is established. See `docs/static-register-abi-pricing.md`.

### 2026-09-05: preserve structural native plans across unhandled calls

Remove the permanent dead marker selected by a prior native `Ok(None)` result.
Keep the structural plan, per-call representation guards, ordinary fallback,
and invalidation when available bytecode changes. Validate semantic probes,
product hashes and fresh process startup before committing. This removes one
history-based admission decision; it does not establish complete no-adapt status
or a startup speedup.

Completed: release build, 29 semantic probes, five product hashes and cold/warm
reload certification pass. Opposite-order fresh-boot pairs: candidate 30.621 /
30.431 s; previous 32.668 / 31.138 s. Small-series evidence favors the candidate,
without proving a precise causal gain. See `docs/native-plan-history-removal.md`.

### 2026-09-05: receiver history becomes diagnostic-only

With inline-selection consumers removed, gate ordinary receiver/lambda history
writers behind `RAVA_RECEIVER_HISTORY_DIAGNOSTICS`. Preserve dispatch caches and
deopt handling. Verify default semantics/product hashes and diagnostic-mode
semantics before measuring fresh full startup against the previous binary.

Result: correctness passes, but both opposite-order boot pairs favour collection.
The experiment was removed; see `docs/receiver-history-diagnostics.md`. Next use
a fresh full-startup PC profile to identify remaining work before another change.

### 2026-09-05: structural inline target selection

Remove receiver-class, heap-reference and lambda-site history from initial
inline planning. Preserve final-method resolution, guarded hierarchy answers,
static lambda provenance and all representation/GC admission guards. Check the
semantic suites and product outputs before pricing full startup. Profile writers
remain diagnostic/legacy overhead for a separate removal after consumer audit.

### 2026-09-05: stop oversized IR during emission

Use constant-time allocated-block/value counts at opcode boundaries, including
nested inline emission. Preserve whole-function refusal and final guards. Test
uninserted-block accounting, the former CompSem expansion, and full startup to
Done before attributing any speed improvement.

### 2026-09-04: deterministic lane completion

- Replace the pilot callback-count floor with completed task outcomes and the
  region abort flag captured after all workers join, before it is cleared.
- Remove class initialization driven by worker refusal history. Retry guest
  tasks only after retiring their nursery/handle reservations; lazy class
  initialization then follows ordinary guest execution.
- Encapsulate joined-region retirement/publication in a module below 500 lines.
- Verify heterogeneous callback counts, missing results, late sibling aborts,
  worker class-init refusal, semantic/hash gates and fresh-process reloads.
- Keep wall <5s unproven until measured; neither snapshots nor timing/callback
  history may select production execution.

Completion status: implemented and checked (four focused tests, eight worker
tests, 27 differential probes, five product hash gates, two fresh reloads).
The heterogeneous-lane reproducer removes seven unnecessary serial replays when
callback diagnostics are enabled. No product wall improvement is established.
ReloadBench now certifies each actual warm resource set after its timed interval;
Java and Rava pass with two warm reloads. Re-establish the null floor using this
benchmark before the next performance price. Details:
`docs/deterministic-region-completion.md`.

### Latest evidence and next profiling boundary

- The certified four-pair null passed all eight cold/warm hashes but retained
  10.5% warm A-arm spread. Small gains remain unresolved.
- Primitive-array eager admission passed its structural and semantic checks,
  but the full hashed ReloadBench produced zero early compilation decisions.
  The runtime prototype was removed. See `docs/structural-array-eager.md`.
- Next, attribute wall time and inspect actual compiler refusal reasons for the
  measured execution population before another implementation. Names and counts
  may locate evidence but must never select production eligibility.
- Use explicit ReloadBench phase markers to exclude hash computation from reload
  profiles. Primary-thread PC residency does not prove all-thread CPU cost.
- Phase profiling and a certified directory/JAR ABBA are complete. Packaging
  gives no demonstrated gain, confirming the older directory-LAST null. This I/O
  front is closed; see `docs/reload-profile-boundaries.md`. Read the corresponding
  sections of `docs/the-boot-price-map.md` before selecting the next runtime change.

### Remove profile-driven recompilation

- Remove the receiver-history recompile baseline, budget, queue, producers and
  main-thread drain. No replacement eligibility may depend on prior profile movement.
- Preserve ordinary compilation and safety deoptimization after failed guards.
- Validate against the existing `RAVA_NO_RECOMPILE` control, then compile and run
  semantic/hash gates. This is a no-adapt contract change, not an assumed speedup.
- Remaining history-based initial compilation and inline admission need separate
  treatment; this removal does not certify the whole VM as no-adapt.
- Completed: scheduler removed; release build, six structural tests, 27 semantic
  probes, five ABI modes, five product hashes and certified cold/warm reload pass.
  Warm reload was 8750 ms in the correctness run, not a demonstrated speed gain.
  Evidence and remaining scope: `docs/no-profile-recompile.md`.

### Earlier candidates (historical, revalidate before use)

1. Re-establish the warm-reload null floor with an unread environment flag; PID 8616 is suspended and not proven to explain the failed null, and must not be terminated by this cycle.
2. Re-run the null after the paired-summary/discard-evidence fix, then re-price `RAVA_BG_JIT_EARLY_RESERVE`, `RAVA_BACKGROUND_JIT_QUEUE=32`, `RAVA_TLS_CLUSTER`, and `RAVA_STRUCTURAL_LOOP_EAGER` in separate interleaved batches.
3. Keep the early-reserve tracer opt-in until it produces a clean wall verdict; its SET and UNSET paths already pass the complete semantic gates.
4. If the structural-loop tracer wins and survives soak, replace the default `main`/`<clinit>` policy; then broaden the immutable admission plan beyond loops so invocation history no longer selects production compilation.
5. Price any later candidate on warm reload, then repeat the structural cycle until the measured product boot is below five seconds.

### Remove counter-selected callee precompilation

- Measure the existing `RAVA_NO_TIER2` control on the post-recompile-removal binary.
- Remove the callee-first prepass whose admission uses `Cold(n)` and threshold lag.
  Keep ordinary compilation, compile-only nesting and worker-drain recursion safety.
- A future structural call-graph plan must resolve without loading new classes;
  `cached(owner)` followed by general `resolve_method` does not guarantee that.
- Build and certify semantics and product hashes before committing the deletion.
- Completed: release build, 27 semantic probes, five product hashes and cold/warm
  ReloadBench certification pass. The control arms overlap; no speedup is proven.
  Candidate warm reload was 7465 ms. See `docs/no-counter-callee-prepass.md`.

## Non-negotiable measurement rules

Apply a structural ceiling of 16,384 IR blocks and 262,144 SSA values before
frontend finalization and before final compilation. Refusal preserves interpreter
execution. Validate ordinary mode and re-run the first-entry memory reproducer
with the same cap; the first-entry switch remains an experiment for this check.

The inline-local pool passed its lease/GC probes but did not prevent the memory
failure. It was archived and removed. Compiler-phase diagnostics show frontend
finalization succeeds before allocation fails inside `finish_compile`, with
about 1.51 million SSA values and 296,233 blocks. Next design a structural IR-work
budget. See `docs/compile-memory-phases.md`.

Before pooling inline-local SSA variables, pin the Cranelift stack-map contract:
same-block redefinition can replace the saved definition used at finalization.
Verify explicit SSA-value rooting before implementing any scoped reuse. Preserve
nesting and control-flow invariants; a memory improvement cannot discard GC roots.
Three contract tests now verify separate variables, naive reuse and explicit
value rooting against Cranelift 0.132.1. See `docs/inline-local-reuse-contract.md`.

The first-entry scheduling experiment hit the 4096 MiB cap in CompSem and was
removed. Its patch and binaries are archived. It exposed an inline expansion
budget defect; repeated-call accounting passes four unit tests, 27 semantic
probes, five product hashes and cold/warm reload certification.
Even with that fix, first-entry compilation needs a structural compiler-resource
bound before it can replace counters. See `docs/inline-expansion-budget.md`.

- Benchmarks use port 25599; port 25565 belongs to the user's server.
- Never terminate `java` broadly. Stop only a stray `rava.exe` started by this cycle, by PID.
- Checkpoint/restore is not a boot-speed path.
- Interleave A/B arms and discard runs without `Done`.
- Treat overlapping arms as unresolved; report signs rather than invented precision.
- Keep only a change that passes semantic gates and moves the appropriate wall metric.
Offline preparation: extract the immutable class-file compiler from VM linking, then verify serialized numeric code without guest class initialization. Preserve runtime eligibility guards; this is a prerequisite, not a full-boot speed result.
Separate immutable prepared-method refusal from failed runtime binding. Never memoize a failed binding as permanent interpreter eligibility; verify recovery when a previously missing field owner becomes loadable. Keep immutable unsupported-method caching to avoid repeated compilation attempts.
Extend existing pure integer inlining across bounded same-class static call chains. Charge repeated expansion against one instruction budget and reject recursive paths. Validate native execution without invoke callbacks and budget refusal before measuring application impact.
Re-established ordinary-JIT reference on the same frozen release: 26.529 s versus prepared/JIT-off 145.761 s. Prioritize explaining this 5.49x execution gap and preserving the fast path's optimizations without adaptive eligibility; do not treat small gains within the slow prepared mode as recovery of the original baseline. See docs/jit-prepared-reference.md.
Repair offline/runtime constant-policy disagreement before pricing broader coverage. Record and validate the prepared constant mode, preserve default-off behavior, and require actual prepared entries plus moving-GC/Java equivalence for string/class literals.
Expanded coverage exposed a missing iterator source-witness check in the compiled shortcut. Share the normal iterator's check before consuming an element; verify cursor preservation and the prior compiled-mode CME failures.
Preserve String semantics across flat and arena representations in collection hashing, case-insensitive comparison and cast diagnostics. Reuse arena cached hashes and the existing string-byte view; validate representation equivalence and all five expanded-coverage failures before timing.
Add explicit classpath preparation and read-only first-call object loading. Bind artifacts to executable content, preserve runtime guards, and verify fresh-process results before full application experiments. Unsupported methods still require the existing runtime; whole-program no-adapt and wall below five seconds remain unproven.
Repair prepared AOT semantic failures by sharing array covariance checks, preserving null-invoke source context, and implementing checked casts. Verify the prior failures and broader exception behavior before another performance experiment.

### 2026-09-28: user selected the full compiler/VM rewrite

The user chose the high-risk full rewrite rather than relaxing the <5 s,
no-adapt, or no-snapshot acceptance criteria. This authorizes a staged redesign;
it is not evidence that the target is reachable.

Three independent source audits compared a minimal `Exec::dispatch` /
`ensure_code` seam, a fixed-enum compiler/output module, and a per-call-site
sealed target cell. The call-site design mostly consolidates existing PIC,
`DirectEntry`, and call-cache state; its estimated 2.5–4 CPU-s ceiling is below
the >7.6 CPU-s reduction required. A compiler/output seam has no measured
multi-second ceiling either. Do not present any of these estimates as savings.

Recommended architecture: a tier-neutral `ExecKernel` owns per-OS-thread
activation state, while `KernelShared` owns the shared heap, loader, GC-visible
registries, and code publication. Keep the hot interface closed and fixed-policy:
an execution activation produces explicit return/guest-throw/VM-failure/refusal
outcomes, and compiled products use a closed representation rather than a
per-call trait-object registry. Preserve `JitFn`, `repr(C)` JIT context layouts,
root addresses, safepoints, first-exception ordering, class initialization, and
worker fail-closed behavior until each replacement has parity evidence.

The first implementation gate is a real compiled-method path through the new
module with identical interpreter/JIT results, exception behavior, and moving-GC
root forwarding. Only after that vertical slice passes should the shared-state
split widen to workers. The product boot and CPU A/B remains the performance
gate; no current source audit supplies a >7.6 CPU-s savings proof, so the goal
stays open and the rewrite carries no speedup claim.

### ABI-preserving compiled-entry tracer (2026-09-28)

- Moved the Cranelift entry protocol into `vm/exec_kernel.rs`; all three JIT
  dispatch sites now call `ExecKernel::enter_jit`. `JitFn`, JIT context
  representation, cell/root addresses, safepoints, and result/error ordering
  remain unchanged.
- This is only a module seam: `ExecKernel` still borrows `&mut Vm` and uses
  `Deref`; it does not yet own an activation or split shared runtime state.
- `cargo check -p rava-interp --features next-gen-gc` and all 118 JIT unit
  tests passed. Two moving-GC root-forwarding tests passed. The third
  `arena_root_tests` assertion still fails at `2040 < 2040`, including alone;
  this is the pre-existing major-compaction accounting failure recorded above.
- One fresh, snapshot-free Linux product boot used a copy of `world_pristine`,
  the fixed JIT configuration, and the release artifact
  `3171b4c2cd8a92270e63c473aa6cc06f9e43c53b4b76611af0ca502151561734`.
  Launch-to-`Done` was 31.035 s and process CPU at `Done` was 50.33 s; the
  guest's own timer printed 0.355 s. This is one observation, not an A/B or
  median, and it misses the five-second target by a wide margin. Runtime
  adaptation is still present and has not been certified absent.
- The tracer has no claimed speedup. Proceed only by widening the ownership
  boundary around a measured multi-second hotspot, with separate semantic,
  GC-root, no-adaptation, and fresh-boot gates.
- A current-source PGO diagnostic build (`release-diag`, binary SHA-256
  `7d0ad0d511dbe5abd526a45c014c10e8763a5c5752697bf6505eb27b3f99a66d`)
  was profiled on another copied `world_pristine` with `perf record -F 49
  -e instructions:u`, no callgraph, and no snapshots. It reached guest
  `Done (0.373s)` at 29.833 s diagnostic wall / 49.5 all-thread CPU-s;
  sampling/debug symbols make those timing figures non-production evidence.
  Perf captured 1,380 samples with zero lost, approximately 180.68B retired
  instructions. Leading symbols were `fu_fastpath` 5.82%, `jit_call_body`
  3.79%, allocator page-zero 3.02%, `rava_fu_invoke` 2.44%,
  `jit_direct_probe` 2.30%, and `ExecKernel::enter_jit` 1.40%. These are
  sampled instruction shares, not removable CPU ceilings. The entry wrapper
  is not a multi-second optimization target; the measured work is spread
  across native, callback, dispatch, allocation, and compiled execution.

### Activation-owned cell-root registration slice (2026-09-28)

`CellRootGuard` now owns the compiled activation's root-stack push/pop lifecycle
inside `ExecKernel`. It preserves explicit-context, null-context TLS fallback,
and self-registering behavior; the explicit drop stays at the original point
after the JIT call and sentinel inspection, before return decoding. The guard
does not borrow the cell buffer because compiled code mutates it through the
existing ABI; the caller's activation scope remains responsible for keeping
that buffer alive. Its destructor also pairs registration during Rust unwinding.

The focused lifecycle test passes for all three registration modes; `cargo
check -p rava-interp --features next-gen-gc` and all 118 JIT tests pass.
`exec_kernel.rs` remains under 500 lines. This is ownership structure and cleanup
reliability only, not a speedup claim or complete activation ownership proof.
Interpreter/JIT exception and moving-GC parity, no-adaptation verification, and
snapshot-free sub-five-second boot remain open.

### Compiled exception transfer and moving-root parity (2026-09-28)

The current-source release CLI (`0e6019d64df2095fced193243a7b8ce9f3119412db1c4e37a87eca602ecb21bd`)
was checked against HotSpot and interpreter-only Rava:

- `JitThrowNullSem` matches all three modes: 4,096 caught null throws,
  4,096 preserved exception identities, and the same null-throw message.
  `RAVA_TRACE_COMPILE=1` confirms both `raise` methods and `main` compiled.
- Added `corpus/conformance/JitMovingRoot.java`: a live arena object exists only
  in a compiled local while allocation forces movement. HotSpot, JIT-off, and
  JIT-on all return `RESULT=-1474935719`. With a 64 KiB nursery, the diagnostic
  run records 73 minor collections and the compile trace names `exercise`;
  the same JIT checksum also passes with GC statistics disabled.

This closes a focused compiled throw-transfer and moving-local-root gate, not
the whole exception/GC state space. The known `arena_root_tests` major-compaction
accounting failure remains. Runtime adaptation is not yet disabled/certified,
and the production Done-under-five-seconds gate remains open.

### Current-source snapshot-free product boot (2026-09-28)

The rebuilt release CLI
(`0e6019d64df2095fced193243a7b8ce9f3119412db1c4e37a87eca602ecb21bd`) ran from
a clean environment in an isolated workdir with a fresh copy of
`world_pristine`, port 25599, and no restore/checkpoint variables or prepared
state. Fixed launch settings were `RAVA_JIT=1`, `RAVA_JIT_TIER0=1`,
`RAVA_PROC_CAP_MB=4096`, and `RAVA_HARD_MB=3584`. Hub readiness observed guest
`Done (0.393s)` at 30.626 s launch-to-Done. This is a single current-source
snapshot-free observation and misses the five-second wall gate by >6×; CPU was
not captured because the server was intentionally stopped at `Done`.

This is **not a strict no-adapt certification**: the clean environment omitted
name-specific opt-in flags, but source still contains name/history-dependent
runtime decisions, so the run cannot be labelled compliant. Existing no-adapt
inventory is stale and explicitly open for re-count. No speedup is attributed
to the root-guard change; neither no-adapt compliance nor the full acceptance
has been achieved.

### Strict no-adapt route audit (2026-09-28)

The Tier-0 product path bypasses the invocation-count threshold with fixed
first-entry compilation, and dispatch compilation no longer uses per-callee
miss history. This does **not** certify the runtime as no-adapt: current live
compile, inline, direct/PIC, and AOT admission still includes class/namespace
name gates; the snapshot-free boot above therefore used clean environment
settings but cannot be labelled strict no-adapt.

A proposed replacement of the collection substring classifier with general
type-closure membership is rejected. Allocation routing is a representation
contract: the collection native arm reads synthetic `$elems` state, while
ordinary user classes can implement collection interfaces without that state.
Type closures can also be partial when JDK ancestors are unloadable or a
GIL-free worker cannot load; using them as a positive allocation fact would
not preserve the current contract. The existing `wide_tier_admits` use is a
separate, fail-closed JIT refusal filter and must not be reused as an allocation
predicate. Keep these boundaries orthogonal until native-state ownership can
be represented by a complete class-load contract.

The current-source recount confirms that the fixed Tier-0 product bypasses the
per-method warmup counter, dispatch compile requests are first-resolve, and
dispatch-table promotion is count-free. This removes several historical
count-gated policies, but it does not establish strict no-adapt: live
name-keyed native, allocation, compile-refusal and inline-refusal decisions
remain. No strict no-adapt boot was run.

The current production-path count is by decision surface (not by every
individual name literal):

- Six prefix alternatives (`java/`, `javax/`, `jdk/`, `sun/`,
  `com/google/common/`, `it/unimi/`) feed `native_served_namespace` and the
  live `jit_native_served_deep` policy. Deep refusal reaches ten logical
  admission decisions: compile entry, four direct/PIC resolve cases, dispatch
  cacheability, two inline candidates, register-ABI refusal, and AOT linking.
  Dispatch cacheability has both class-id and name fallback implementations.
- Allocation is a separate representation route. The default-on birth guard
  still uses namespace membership; inside admitted namespaces,
  `is_native_backed_alloc` contains seven collection-name substrings plus
  exact map, entry, and I/O class families. This is not just a JIT refusal:
  the allocation decision protects synthetic native state.
- Native dispatch still has the `force_native_collection` family router and
  exact method/class accelerators (including spline and fastutil routes).
  Static/special `SiteRef`s and loaded classes cache `NativeFamilyFact`; generic
  chain dispatch consumes the cached class fact and falls back to
  `family_word(owner)` only for an owner not yet loaded. The classifier remains
  name-derived, so this is a cached routing fact, not strict no-name proof.
  JIT NativeProgram selection separately consumes the exact `LoadedClass`
  binding in both native-plan and heap-leaf routes.
- On the fixed Tier-0 product path, main-entry count thresholds are bypassed
  by first-entry compilation and dispatch-tier compilation is first-resolve;
  AOT hot counts and deopt recompile budgets are outside the plain boot path.

The loaded-family fact test passes; the release build succeeds. `ConcSem` JIT
output and `Collections` interpreter/JIT outputs match the previously verified
oracle (`SUM=23109031176`). No full-boot timing was run for this route change.

This inventory counts policy surfaces and the ten deep-refusal consumers; it
does not claim an exhaustive count of all name literals or environment flags.
No evidence supports removing these semantic native-state/dispatch contracts
by toggling flags, and no strict no-adapt build/run configuration is currently
certified.

The remaining native-serving refusal cannot be a class-level boolean: native
coverage varies by exact method and receiver representation. The safe seam is a
resolved-method route fact that proves whether skipping the native cascade is
valid; `Unknown` must retain the ordinary cascade. `NativeProgramClass` only
selects an optional provider and does not prove that other native arms cannot
serve the class. No such complete route fact exists yet.

### Structural native-state fact seam (design, 2026-09-28)

Three read-only interface alternatives were compared. A single
`NativeStateFact::{Carries, Free, Unknown}` is the best base: it asks only
whether instances carry native synthetic payload, not whether a name looks like
a collection. A flexible payload ledger is useful only if it remains behind
that small fact interface; a `BirthRoute` result is not the fact itself and
would conflate policy, class initialization, layout completeness, and TLAB
allocation.

Recommended seam: a class-load-owned native-state fact attached to stable
class metadata, with direct facts sourced from an explicit native storage
contract and inherited facts derived only from a complete hierarchy. The fact
must distinguish:

- **Known absent:** complete class/provider evidence proves no synthetic native
  payload.
- **Known backed:** exact native storage contract identifies payload slots
  (`Syn::Elems`, `Syn::Map`, entry fields, or another declared layout) and
  which representations can carry them.
- **Unknown:** missing provider metadata, unloadable/partial ancestry, or a
  worker unable to complete the class walk.

Native binding may resolve a static provider declaration while linking a
class/method; runtime allocation and JIT policy consumers must use the resolved
fact, not repeat package-prefix or substring eligibility checks. A loaded
bytecode override does not by itself erase native backing: it changes method
dispatch, not the synthetic storage contract. Likewise, a native-state fact
alone must never authorize direct binding or inlining. Compile, direct/PIC,
inline, register-ABI, and AOT policies retain separate receiver/dispatch
proofs until each has a sound structural consumer.

The module's interface should stay narrow: one query for complete class facts
and a stable-identity lookup for already resolved call paths. It owns provider
fact binding, synthetic-layout association, hierarchy completeness, and
permanent-fact publication. Unknown is never memoized as a permanent fact and
maps to the current safe representation/interpretation fallback. It may not
mint an arena class id or publish an incomplete layout.

First implementation slice should be proof-only: classify controlled
`ClassLoader` fixtures and compare the fact against the current representation
contract for (1) a native-backed collection, (2) an ordinary `FooList` bytecode
class, (3) a bytecode override that still shares native storage, (4) a
value-computer-exempt subclass, (5) an unloadable/partial superclass, and (6)
a GIL-free refused load. Keep class initialization and `register_class` behind
the existing allocation adapters. Do not change JIT admission or remove legacy
gates in this slice; the later policy migration needs its own focused
parity/GC-root tests and strict no-adapt boot.

The first implementation now attaches `NativeStateFact::{Unknown,
SyntheticSlots}` to `LoadedClass`. It derives direct slots only from exact
`owner_bits` provider facts; those provider tables remain name-keyed, the
catalog is incomplete, and no `KnownAbsent` fact is claimed. Interpreter birth
and the cached JIT site-route prefer a positive fact while preserving the
legacy conservative gate for unknown or cold classes.

The bridge passed the focused provider tests and the `Collections` parity
scenario (`SUM=23109031176` in interpreter, JIT, and HotSpot); `RAVA_JIT_STATS`
confirmed `Collections.hot` compiled and ran 14 times. This does not certify
strict no-adapt: uncovered native allocation, JIT, and dispatch routes remain.
The <5 s measurement stays blocked until policy consumers stop routing by
names.

### JIT native-program class binding (2026-09-28)

`LoadedClass` records an exact `NativeProgramClass` provider binding at
class-load time. The JIT native-plan builder consumes only that fact; if the
class is absent or unbound, the optional native-program specialization declines
and the ordinary native cascade remains authoritative. It no longer classifies
an unloaded class by name. Per-method descriptor checks and opt-out gates
remain unchanged. The JIT heap-handle NativeProgram leaf also requires that
loaded binding at fill time; other leaf tuples and the interpreter cascade
still have name-based routes.

The exact-name, missing-binding, arena parity, and heap provider-binding tests
pass. The release build succeeds. `Collections` returns
`SUM=23109031176` under HotSpot, interpreter, JIT, and JIT with
`RAVA_NO_NATIVE_PROGRAM=1`; the hot JIT method compiled and ran 14 times.
`ConcSem` output matches HotSpot in both interpreter
and JIT modes, covering ByteBuffer, exceptions, atomics, and concurrent
collections. Other heap leaf tuples, the interpreter cascade, and the
namespace/ancestor JIT refusal remain name-based; these checks do not certify
strict no-adapt or measure full boot.

The arena PIC leaf binder now reads a frozen `LeafClass` from the arena class
mirror instead of converting each bound receiver class ID back to its name.
Registration and snapshot restore derive the same enum from the canonical class
name; unknown class IDs still fail closed. The heap-handle binder remains
name-based because handle classes do not use the arena class registry and the
ThreadLocal leaf is handle-only. A focused parity test compares old name routing
with the frozen contract across ArrayList, AtomicLong, Optional, wrappers,
HashMap, and a synthetic class.

### Precomputed native-family facts at constant-pool sites (2026-09-28)

`SiteRef` carries the complete family word resolved once from its immutable
symbolic owner. Static and special native fallbacks thread that fact into the
family dispatcher; exact and `Has`/`Ends` families all bypass the per-thread
owner-name family memo. Non-site entry points and super-chain walks retain the
existing memo route. `RAVA_NO_ID_RESOLVE=1` continues to use the table scan.

The family-fact classifier test and release build pass. `Collections` matches
under HotSpot, Rava interpreter, interpreter with `RAVA_NO_ID_RESOLVE=1`, and
Rava JIT. `ConcSem` also matches HotSpot across its ByteBuffer `Has`-pattern and
BufferUnderflowException `Ends`-pattern paths, both with the fact route and the
scan opt-out. These checks do not certify strict no-adapt or measure full boot.

### Allocation name-route structuralization: no-go (2026-09-28)

Audited the proposed `NativeStateFact` extension against every allocation,
layout, JIT, statics, and native-dispatch consumer. Do not replace the
`is_native_backed_alloc` name route with one additional class-load fact:

- `NativeStateFact` attaches only to loaded classfiles, while synthetic and
  unloadable JDK owners have no `LoadedClass` to carry a fact.
- `stream::is_collection` is an intentional substring superset with explicit
  exclusions, not an exact provider catalog. The residual names are
  representation-policy decisions; treating `Unknown` as absence can route
  receivers away from synthetic `$elems`/`$map`/entry storage.
- Ancestor policy spans unloadable superclass names; layout safety also walks
  interfaces, and value-computer admission can exempt selected subclasses.
  Replacing the shared predicate therefore needs a complete provider and
  hierarchy contract, not a local bit.
- The allocation call sites already prefer positive exact storage facts before
  falling back to the cached predicate. `owner_bits`, the map-owner sketch,
  and the ancestor memo already amortize the name work; no measured launch-wall
  saving justifies a new fact or broader migration.

No runtime change is authorized by this audit. The required evidence before
revisiting is a full-boot fallback-population/served census, followed by
focused representation tests for exact providers, synthetic-only owners,
name collisions, inherited state, and interface-backed entries. Current
fresh-boot acceptance remains 21.9 s and strict no-adapt remains uncertified.
