# Generic runtime performance work

Base: `bed0e219`.

This work speeds up common VM operations that both servers and clients use. Every change keeps
the Java CLI, cold class-file startup and workload-independent policies. No application name
or output selects VM behavior.

## Changes under validation

- Outgoing register ABI hints extend to eligible virtual and interface calls. Profiling
  calls, cold calls and values live across a call stay outside the new hints.
- An existing pre-prologue fast exit may now go through a shared-return phi, once its
  selected predecessor is validated. The operand, register and receiver checks stay.
- JNI lengths, copies and checked regions read compact string contents directly. Borrowed
  contents must not cross a Java allocation, call or safepoint.
- The Unix resolver fallback and file-preferences natives that the semantic suite found
  missing are restored. System calls stay in the platform layer and filesystem waits stay
  outside the running Java state; errno is preserved, and lock descriptors are released on
  allocation failure.

The proposed task-wide dirty-card drain was rejected. A small scanner comparison on hot
metadata regressed, while whole-VM allocation results stayed near noise. The existing scanner
stays, along with the new dirty-card probe and allocation benchmark for later work.

## Validation gates

Before any compiler or collector commit, build the candidate and upstream with the same Rust
release profile and JDK 21, then run `tests/check.sh all`, the Rust tests and full fresh-world
Paper starts. The four documented helpful-NPE-message failures are still expected.

`bench/paper_ab.sh binary:/absolute/path` tests an uncommitted candidate, so a clean worktree
build is not mislabeled as the candidate. Keep the binary hashes and a build/source manifest.
Run alternating upstream/candidate/HotSpot rounds with the fixed Paper version, seed and port
25599. A missing `Done` is a failure whatever the harness exit status says.

Attribute changes with focused generic benchmarks and the generated code. Report observed
timings with their variance. Microbenchmarks or server startup alone do not show overall
Minecraft tick or client-frame gains, so do not claim those from them. Validating the client
game needs a real modpack and a host with graphics. Larger register-allocation and
shrink-wrapping work stays separate.
