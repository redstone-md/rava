# The tier-0 JIT threshold has never been in effect (2026-07-28)

## What the engine actually runs

```
$ RAVA_VERBOSE=1 RAVA_JIT_TIER0=1 rava ... net.minecraft.server.Main
rava: memory cap 4096 MiB, 16 core(s), jit=true (cap 4096, threshold 512), gc=true
```

`RAVA_JIT_TIER0=1` is set and the threshold is **512**, not 16.

## Why

`crates/rava-interp/src/lib.rs` resolves the knob with the tier-0 value as a FALLBACK:

```rust
jit_threshold: std::env::var("RAVA_JIT_THRESHOLD")
    .ok().and_then(|v| v.trim().parse().ok())
    // 20.1 Tier-0: compile cold code aggressively (16 vs 512) ...
    .unwrap_or(if jit_tier0() { 16 } else { 512 }),
```

but `crates/rava-cli/src/main.rs` resolves its OWN default first and then publishes it into the
environment before the engine is constructed:

```rust
let jit_threshold = std::env::var("RAVA_JIT_THRESHOLD").ok()...
    .or_else(|| file.get("jit_threshold")...)
    .unwrap_or(512);                                   // main.rs:275
...
fn apply(&self) {
    std::env::set_var("RAVA_JIT_THRESHOLD", self.jit_threshold.to_string());   // main.rs:290
```

So `std::env::var("RAVA_JIT_THRESHOLD")` in the engine ALWAYS finds a value, the `unwrap_or` arm is
unreachable, and `jit_tier0()`'s 16 is dead code on the CLI path. **The same shadowing applies to
`jit_cap`**: the engine documents `16384` ("~2.5 KB/method, ≈ 40 MB"), the CLI publishes `4096`, and
4096 is what runs — as the banner above confirms.

Every benchmark in `bench/` sets `RAVA_JIT_TIER0=1` in the belief that it lowers the threshold to
16. None of them did.

## Do NOT simply delete the shadowing — threshold 16 does not boot

Setting the intended value explicitly fails the MC server boot **7 times out of 7**, deterministically,
at ~51s, in datapack loading:

```
java.lang.IllegalStateException: Unbound values in registry
    ResourceKey[minecraft:root / minecraft:worldgen/placed_feature]:
    [minecraft:lake_lava_underground, minecraft:patch_firefly_bush_near_water, ...]
java.lang.IllegalStateException: Failed to parse <arena-object@0x...> from pack vanilla
Failed to load datapacks, can't proceed with server load
```

Isolation, all on the same HEAD binary, port verified free, world restored per run:

| configuration | effective threshold | result |
|---|---|---|
| no `RAVA_JIT_THRESHOLD` (+`RAVA_JIT_TIER0=1`) | 512 | boots 3/3 — Done 25.306 / 25.552 / 25.963 |
| `RAVA_JIT_THRESHOLD=256` | 256 | boots 3/3 — Done 25.282 / 25.432 / 25.581 |
| `RAVA_JIT_THRESHOLD=64` | 64 | boots 3/3 — Done **23.533** / 23.596 / 50.15 |
| `RAVA_JIT_THRESHOLD=16` | 16 | **FAILS 7/7** |

Not the affinity mask (fails with and without `0x0FFF`) and not the `RAVA_COMPILE_TIME` probe
(fails with and without it). The single discriminator is the threshold value.

⇒ **There is a latent JIT correctness bug that only compiling MORE code exposes.** It has been
hidden for as long as the shadowing has, because the aggressive setting never actually took effect.

## What this is worth: NOTHING - the knob is null (A/B run 2026-07-28)

The shape pass suggested 64 was worth 7-9% of Done. **It does not reproduce.** A clean 64-vs-512
A/B, 4 reps, permuted order, all 8 boots reaching Done:

| | Done, 4 runs | min | mean | wall min | cpu mean |
|---|---|---|---|---|---|
| thr=64 | 23.95 / 23.938 / 23.801 / 24.43 | 23.801 | 24.03 | 84.5 | 144.1 |
| thr=512 | 24.869 / 23.12 / 25.178 / 23.727 | **23.12** | 24.22 | **82.9** | 143.8 |

Completely overlapping: 512 holds the better MINIMUM on both wall and Done, 64 the better mean on
Done by 0.8%. Nothing separates. The 7-9% was noise plus a 50.15s outlier in a three-rep arm.

=> **The compile threshold is a NULL KNOB between 64 and 512, so the shadowing is not costing
throughput** - it is costing honesty, since every bench script advertises a setting that does not
apply.

* The informative part is the null itself: thr=64 compiles **3000** methods in 6.4s, thr=512
compiles **2000** in 5.7s, and the extra thousand compiled bodies plus 0.7s of Cranelift buy
NOTHING measurable. Compile coverage past ~2000 methods is worthless on this boot, which also
re-prices the background-compile-thread idea downward: the plan claims ~5.7% of wall for moving
4.3s of compilation off the critical path, and here 0.7s moved on or off it changed nothing.

## Method note


This surfaced only after the boot harness was made to REQUIRE the `Done` line. Two earlier sweeps
had reported eighteen tidy wall/cpu rows that were all times-to-failure, and the threshold-16 arm
looked like an ordinary — if slow — measurement. A harness that only prints numbers will print
numbers for a crash. `Assert-FreshExe` cannot substitute: it compares mtimes and passes a binary
built recently from an old commit, which is separately what had been hardlinked into
`target\release` while this was being chased.
