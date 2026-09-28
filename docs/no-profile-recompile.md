# Receiver history no longer schedules recompilation

The removed mechanism recorded the receiver profiles available at first compile,
watched profile epochs, queued changed callers, and selected methods for another
compile when more profiled sites became available. A process budget of 192 bounded
the work but did not make eligibility structural: past execution selected it.

The baseline/profile table, queue, budget, producers and main-thread drain are
removed. Lambda-profile transitions no longer bump an epoch solely to trigger
that queue. The old RAVA_NO_RECOMPILE and RAVA_RECOMPILE_MAX switches no longer
control anything. No alternative history-based recompile scheduler was added.

Safety deoptimization remains in `jit/callbacks/deopt.rs`: failed guards still
poison the failed speculation and may supersede its entry through the existing
guard-failure path. This deletion does not make a stale assumption valid or remove
fallback execution. Ordinary initial compilation is retained.

Flat-served lambda bodies have no compile request by default. `RAVA_FLAT_TIER=1`
enables a fixed first-use request; `RAVA_NO_FLAT_TIER=1` overrides it. Neither
setting restores receiver-history admission.

## Control and validation

Before deleting the mechanism, a two-pair ABBA on the preserved baseline used
RAVA_NO_RECOMPILE as its existing opt-out. All four processes exited 0 and passed
both cold and warm hashes. Evidence:
`%TEMP%/rava-no-recompile-control-20260905` and its sibling `.log`.

Warm default: 7333/7406 ms; disabled: 7743/7391 ms, overlapping. Disabled CPU and
cycles were above both controls (median +1.60% and +1.44% respectively); the small
series does not show a speedup. Removal is required by the no-adapt constraint;
its performance cost must be recovered using structural decisions, not restored
receiver-history optimization. This is not a full-boot timing result.

Six structural-admission tests and the release build pass after removal. All 27
semantic probes match Java `-Xint` with their existing declared divergences. All
five static-ABI modes pass at a 64 KiB nursery, each recording 169 minor GCs.
All five product gates exited 0 with their expected hashes: ChunkFill
`8790f04ace726ecf`, RingScan/RingLane/RingLaneInstance `37539c6c375d0c3`,
and NbtSave `ae44a34b42766bd4`.

One fresh ReloadBench process exited 0 and certified both cold and warm as
`c956377e19071d7a`, with 1470 recipes, 1584 advancements and 625 tags. Cold was
8698 ms, warm 8750 ms, and whole-process wall 48801 ms. This is a correctness run,
not an interleaved speed comparison or a full-server boot result. All product
processes used affinity `0x0FFF`, the standing seven-variable environment recorded
in `product.env`, and a copy of the main checkout's configuration. No world,
snapshot or restore input was used.

Candidate evidence: `%TEMP%/rava-no-profile-recompile-20260905`, SHA-256
`9C88D73C97368279EC04577093F60FDE754055B1E7BD01654AF4DB519705C6BE`.
ABI evidence: `%TEMP%/rava-static-abi-cf2bbe0ef082429facba111d5e145a90`.
Only explanatory deopt comments were updated after this binary was built;
the executable deopt implementation is unchanged.

## Remaining no-adapt work

The recompile scheduler is gone. A current-source audit narrows the remaining
no-adapt questions:

- The measured boot harness sets `RAVA_JIT_TIER0=1`, enabling first-entry
  compilation and bypassing `jit_run.rs`'s main-thread invocation-threshold
  gate. The former 64-miss per-callee dispatch-tier counter is removed:
  `vm/dispatch.rs` now requests compilation on the first eligible resolved
  dispatch through the bounded background compiler queue. Only that exact
  `Code` key may queue under `COMPILE_ONLY`; all other compile-only callers
  retain their synchronous contract. Four fresh-world ABBA/BAAB launches per
  binary all reached `Done`: baseline SHA-256
  `59640b22483de9bc89f09aecfbe81f46458d8d65cce69ceeaf64173749fa4535`
  measured 29.224 s median; candidate SHA-256
  `4721c8564d28e7030d9c89a3cf187a56c5f2a25f9d6a5dd7801c90b71bcf2aae`
  measured 29.125 s (−0.34%, below run variation). This is not a measured
  launch win; the structural queue replacement is retained to remove the
  load-bearing miss-count decision. The earlier threshold-one synchronous
  sweep was 31.8 s versus default 64 at 29.7 s (+7.1% wall, +4.1% CPU), and
  disabling dispatch/wide tiering was 33.7 s versus 29.3 s (+15.0% wall,
  +8.2% CPU). Neither historical A/B applies to the new asynchronous route.
- Count-based behavior still exists in `jit_run.rs` when first-entry compilation
  is disabled. The dispatch `NotYet` memo now waits only while its resolved code
  key remains `JitStatus::Compiling`; the VM drains completed background results
  on a result-ready signal and retries after publication or queue refusal. This
  removes the per-site 64-miss cadence, but has no measured launch win yet.
  PIC-native leaf entries now demote to their rooted fallback on the first
  refusal; no refusal-count threshold remains. There is no 256-miss dispatch-
  table promotion; the dispatch table admits only a validated positive fill.
  Remaining `NotYet` and threshold-mode policies do not certify full no-adapt.
  Focused dispatch-memo and background-result readiness tests both pass.
  A small fresh-process ABBA check (two runs per memo mode) matched results for
  `VCall 500000` and `PolyCall 500000`; VCall was 222–227 ms and PolyCall
  135–142 ms across arms. This is smoke-level coverage, not a launch-speed win.
  A separate two-pair ReloadBench oracle produced the expected
  `c956377e19071d7a` hash for all four successful arms; intermittent ring/array
  failures also occurred in candidate runs, including one with dispatch-tier
  compilation disabled (ledger row 645), so that existing race remains a
  semantic-test risk.
- The PIC-native leaf's 64-consecutive-refusal counter is removed. A first
  refusal atomically retires the leaf descriptor to a rooted twin, preserving
  the complete per-call fallback without learning from repeated history.
  `jit::callbacks::dispatch::pic_native::tests::first_leaf_refusal_claims_fallback_once`
  passed. The rebuilt release-fat candidate passed one fresh-world no-snapshot
  boot smoke with PIC-native enabled and one with `RAVA_NO_PIC_NATIVE=1`;
  both reached `Done` at 21.5 s wall (37.3 / 35.5 CPU-s). This single pair
  validates boot only; it is not a speed comparison of the retirement change.
- `Cold` level atomics are telemetry-only under the measured Tier-0 setting;
  the `n2 >= threshold` compile condition is short-circuited by first-entry
  compilation on the main thread. Threshold mode remains reachable when
  first-entry is disabled (for example by `RAVA_NO_FIRST_ENTRY_JIT=1`,
  `RAVA_NO_EMIT_MORE=1`, or omitting Tier-0), so this does not certify every
  runtime configuration.
- `inline_targets.rs` no longer uses receiver-history tables for inline
  eligibility: it uses resolved calls, CHA hierarchy facts and static lambda
  provenance. Remaining `lambda_site_mono` reads are transition telemetry;
  `sitemono::mono_class` in `lambda_prov.rs` is census-only, and `mono_ref` has no
  executable caller in the current tree.
- Name-based native-safety refusals remain on production compile/dispatch paths
  (`native_served_namespace`/`jit_native_served_deep`, PIC admission and AOT
  guards). Existing counterexamples make dropping them unsafe; replacing them
  requires structural native-state contracts, which are not yet sized.
  A proposed `Compile`-only relaxation was reverted: its profile-use binary
  segfaulted twice within 0.6 s on fresh worlds, while the same binary with
  `RAVA_NO_SPLIT_NATIVE=1` reached `Done` in 23.0 s. Compiled machine-code calls
  can bypass the interpreter's native cascade; a host-only check at `jit_enter`
  cannot cover direct-call/PIC entry edges. Safe admission needs a proven
  receiver-representation guard at every such edge, not just a method/class
  check. This failed experiment is not a candidate result.
- Rebuilt the reverted source after removing the dead `NativeQ` wrapper and
  guarding register-ABI admission, as profile-use `release-fat`; executable
  SHA-256 `89b4880dda5b2f4a724e3646a057a7faaa1a6110f96744b3bb2d72be08e8a969`.
  The isolated no-snapshot harness used a disposable `world_pristine` copy and
  accepted EULA. Its discarded warm-up, default arm, and `RAVA_PRIM_ABI=1` arm
  all reached `Done`; the measured arms were 21.5/21.1 s wall and 37.8/37.0
  CPU-s. This is one safety smoke, not a performance A/B; the profile is stale
  and LLVM discarded mismatched counts. The focused test
  `selfreg::tests::register_abi_refuses_native_and_synthetic_classes` passed.
  `jit_resolve_reg` now applies the normal native-served and synthetic-owner
  refusal before publishing a direct entry. `RAVA_NO_SPLIT_NATIVE` was only a
  candidate-binary control and has no reader in current source.
- An earlier 29.3-second snapshot-free run explicitly set
  `RAVA_GSON_NATIVE=1` and `RAVA_M3_BITSTORAGE=1`. The latter's name-specific
  parallel route has since been removed; that variable is now ignored.
  Subsequent strict runs must continue omitting `RAVA_GSON_NATIVE`. The fresh
  no-snapshot launches reached `Done` at 28.8, 29.2, 29.4 and 30.0 seconds
  (pooled median 29.3; CPU median 48.85 seconds). This is not full no-adapt
  acceptance because native-safety name gates remain.

- Feasibility note from the no-explicit-shortcut baseline: 48.85 CPU seconds /
  29.3 wall seconds is about 1.67 average cores. Ideal linear scaling of the
  same CPU work across six physical cores would still take about 8.1 seconds;
  this is only a theoretical bound, but it shows that more parallelism alone
  is not a credible path to five seconds.

The latest profile-use binary's isolated fresh-world no-snapshot smoke ran a
default arm at 21.5 s wall / 37.8 CPU-s and an `RAVA_PRIM_ABI=1` arm at 21.1 s /
37.0 CPU-s; both reached `Done`. This one pair is a safety smoke, not a valid
performance A/B. The broader evidence closes the remaining in-tree startup candidates:
the opcode residency census found no instruction family with a five-second
ceiling and its only large invoke prototype regressed (ledger row 580); safe
shared execution is bounded below one second (row 715); current-source host
profiles remain diffuse (rows 721-722). The current Rust VM architecture has no
credible measured path from this baseline to sub-five seconds. Reaching that
target likely requires a different execution/heap substrate, not another
local fast path. This is a current-architecture no-go, not goal completion or
permission to relax no-adapt/no-snapshot acceptance.

The full wall-under-five-seconds, no-adapt, no-snapshot objective remains open.
