# HANDOFF — RAVA server continuation — 2026-09-10

Server session on `ns3180355` (Linux x86_64, 12 cores, 31 GB RAM).
Continues `docs/HANDOFF-2026-09-10.md` (Windows session). Read that file first.

## Git state

- Branch: `codex/deterministic-region-completion` (was detached at `44b3c11b`, now checked out and tracking origin).
- HEAD after this session's fix: `64dbf4cb` (`port(build): restore Linux next-gen-gc build after AOT drift`), pushed to origin. Ahead of `86be9307` (handoff doc commit) by 1.
- Working tree clean expected after commit of this file (next commit). Protected file `docs/HANDOFF-2026-09-04-2248.md` untouched (not present in this checkout; never touched).
- Remote: `https://github.com/redstone-md/rava.git`. No force-push used.

## Server toolchain (installed this session)

- OpenJDK 21 headless (`21.0.12.1`), `build-essential`, rustup stable `1.98.1`.
- `cargo build --release -p rava-cli --features next-gen-gc` now succeeds on Linux.
- Binary: `target/release/rava` (15.8 MB, built 2026-09-10). No `server.jar`, no `bench/mc-cp.txt`, no `world_pristine` on this box — full Minecraft boot NOT measurable here.

## Fix shipped this session

Later commits after `eceaa6f0` reintroduced ungated `aot_clif` references, breaking the Linux build with 6x E0433 (`crate::aot_clif` configured out on non-Windows):

- `crates/rava-interp/src/flags.rs`: `aot_clif_enabled()` now returns `false` by construction off windows-x86_64 (same shape as the existing `llvm_aot_enabled()` gate).
- `crates/rava-interp/src/jit/runtime/compile.rs`: the six `aot_clif::` symtab entries resolve to `0` off windows-x86_64; indices stay stable, and no AOT writer/reader is compiled there so the placeholders are never consumed.
- `git diff --check` clean. Commit message and comments in English.

## Measurements (all bit-exact vs JVM)

Base env where noted: `RAVA_JIT=1 RAVA_JIT_TIER0=1 RAVA_HARD_MB=3584 RAVA_GSON_NATIVE=1 RAVA_M3_BITSTORAGE=1` (`RAVA_PROC_CAP_MB` warns "NOT protected by an OS cap" on Linux — Job Object is Windows-only — and was dropped from timed runs).

- `Hello` (1M-iteration sum): JVM `SUM=499999500000`, rava `SUM=499999500000`. Pass.
- `SamTest` (lambda `x -> x*2+1`, 200K applies): JVM `SUM=40000000000`, rava `SUM=40000000000`. Pass — arena-lambda apply path works on Linux.
- `ComputeBench` (corpus): JVM `CHECKSUM=5411900725941656235 DONE`, rava identical, JIT and INT both ~0.20 s wall (4 interleaved pairs, no separation — workload too small to resolve JIT vs interp).
- `CallShapes` (static + virtual + lambda + `Integer.toString`, 200K iters, `SUM=100000988890` on all arms): same-binary JIT-vs-INT ABBA, 6 pairs, alternating order:

| pair | JIT wall (s) | INT wall (s) |
| --- | --- | --- |
| 1 | 0.525 | 0.575 |
| 2 | 0.495 | 0.608 |
| 3 | 0.518 | 0.617 |
| 4 | 0.532 | 0.619 |
| 5 | 0.510 | 0.586 |
| 6 | 0.516 | 0.619 |

Every JIT below every INT (DISJOINT, JIT faster ~10–15%). Narrow causal signal only — NOT a boot WALL claim.

- `RAVA_JIT_STATS=1` on ComputeBench: `methods=1 ... run=0 compile_ok=0` — the single `main` never compiled (expected: one-shot main below threshold / numeric-only gate). `RAVA_CALLBACK_STATS=1` and `RAVA_DIAG=jit` printed no census lines on this workload (gates inert at this size).

## What was inspected, not changed

Read per the 09-10 handoff recommendation: `vm/jit_call_body.rs` (seam gates, direct-call fast path, iterator shortcut, plan lookup), `vm/jit_direct_resolve.rs`, `vm/jit_direct_call.rs`, `vm/jit_direct_invoke.rs`, `vm/apply.rs` (`apply`/`apply_sam`, SAM memo, pooled `sam_locals`), `vm/lambda.rs` (ApplyPlan), `flatcall.rs`, plus `docs/first-entry-flat-lambda.md` and `docs/first-entry-lambda-background.md`. No production JIT change made — correctly, since no new causal measurement justifies one yet.

## Recommended next move (unchanged, server-adapted)

Do not repeat: global threshold lowering, first-entry sync/background compilation, name-keyed routing, snapshots, broad sweeps.

1. Get a measurable workload onto this box first: copy the deobfuscated MC jar + `bench/mc-cp.txt` + `world_pristine`, or promote a corpus/bench workload that resolves JIT-vs-INT above noise (CallShapes qualifies as a smoke, not as a boot proxy).
2. Then target the generic compiled-call fallback (`jit_call_body.rs` general path past the direct-call miss, `jit_direct_resolve.rs` tier-lag refusals, `apply.rs` SAM path) with a small structural tracer bullet: bytecode/CFG/descriptor/resolved-dispatch eligibility only, barriers/GC-roots/exceptions/native-routing preserved, kill switch, one narrow ABBA, full-process wall verdict.
3. Keep the Linux AOT gates in mind: `aot_clif`/`llvm_aot`/`cnp` paths are windows-x86_64-only; server measurements exercise the Cranelift JIT + interpreter only.

## Server measurement notes

- Use `target/release/rava` (name is `rava`, not `rava.exe`); Linux harnesses `bench/boot_ab_flags.sh` and `bench/reload_ab.sh` require `bench/mc-cp.txt`, absent here.
- `/usr/bin/time -f` works for wall/cpu; the earlier `CallShapes` loop failure was a shell quoting issue in the probe, not the binary — the rewritten ABBA loop works.
- Full-process `wall_s` remains the only quotable metric; `Done` subspans alone prove nothing.
