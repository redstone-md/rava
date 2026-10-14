# Generic runtime performance work

Base: `bed0e219`, branch `codex/perf-runtime-20261014`.

Improve common VM operations used by both servers and clients. Every change keeps the
Java CLI, cold class-file startup and workload-independent policies. No application names
or outputs select VM behavior.

## Changes under validation

- Extend outgoing register ABI hints to eligible virtual and interface calls. Keep
  profiling calls, cold calls and values live across a call outside the new hints.
- Allow an existing pre-prologue fast exit through a shared-return phi by validating its
  selected predecessor, while retaining operand, register and receiver checks.
- Read compact string contents directly for JNI lengths, copies and checked regions.
  Borrowed contents must not cross a Java allocation, call or safepoint.
- Restore Unix resolver fallback and file-preferences natives exposed by the semantic
  suite. Keep system calls in the platform layer and filesystem waits outside the running
  Java state; preserve errno and release lock descriptors on allocation failure.

The proposed task-wide dirty-card drain was rejected: a small, hot-metadata scanner
comparison regressed, while whole-VM allocation results stayed near noise. Retain the
existing scanner, plus the new dirty-card probe and allocation benchmark for future work.

## Validation gates

Before any compiler or collector commit, build the candidate and upstream using the same
Rust release profile and JDK 21, then run `tests/check.sh all`, Rust tests and full fresh-world
Paper starts. The four documented helpful-NPE-message failures remain expected.

`bench/paper_ab.sh binary:/absolute/path` permits testing an uncommitted candidate without
mislabeling a clean worktree build as the candidate. Preserve binary hashes and a build/source
manifest. Run alternating upstream/candidate/HotSpot rounds with the fixed Paper version,
seed and port 25599; a missing `Done` is a failure, regardless of the harness exit status.

Use focused generic benchmarks and generated code to attribute changes. Report observed
timings with their variance; do not infer overall Minecraft tick or client-frame gains from
microbenchmarks or server startup alone. Client game validation requires a real modpack and
graphics-capable host. Larger register-allocation and shrink-wrapping work remains separate.
