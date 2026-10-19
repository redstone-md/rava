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

## Server tick: All The Mods 10 (2026-10-19)

On an i5-14400F host (32 GB, Windows 11, JDK 21, both VMs pinned to the P-cores), the NeoForge
All The Mods 10 server with the same mob scenario runs tick sprints faster on rava than on
HotSpot 21. Interleaved rounds, one fresh world per round, at 9d8a76cb:

| Format | rava | HotSpot |
|---|---:|---:|
| 3 rounds, 3 sprints of 2000 ticks each | **26.2 ms/tick** | 29.0 ms/tick |
| 4 rounds, 2 sprints of 4000 ticks each | **27.4 ms/tick** | 28.9 ms/tick |

rava won every round in both formats and used about 25% less CPU (223-249 s against 295-382 s
per round). The same morning the gap had been 12-18% the other way. Free physical memory was
about 5 GB during these runs, and round-to-round spread was about 1.5 ms, so compare
interleaved rounds only.

What closed it, found by per-instruction sampling (`RAVA_SAMPLE_DETAIL=/`) and an inclusive
call-tree comparison with JFR (`RAVA_SAMPLE_STACK=1`):

- Interface type checks probe a per-class hash set instead of scanning the itable (bbf8e202):
  tick 31.1 -> 27.6 ms.
- A class guard's exact type is kept for a phi receiver (9d8a76cb). A default method's
  polluted profile had left an interface call per block in the collision loop: about -5%.
- Smaller ones, near noise on the tick: inline Latin-1 `String.equals` (19de0327) and saves
  around calls from block liveness (0c56d87d).

Still behind HotSpot per subtree: stream pipelines (megamorphic `Sink` calls still scan the
itable after an inline-cache miss), mixin callbacks HotSpot inlines and rava calls (each
allocating its `CallbackInfo`), and cache misses on entity data.

## Validation gates

Before any compiler or collector commit, build the candidate and upstream with the same Rust
release profile and JDK 21, then run `tests/check.sh all`, the Rust tests and full fresh-world
Paper starts. The four documented helpful-NPE-message failures are still expected.

`bench/paper_ab.sh binary:/absolute/path` tests an uncommitted candidate, so a clean worktree
build is not mislabeled as the candidate. Keep the binary hashes and a build/source manifest.
Run alternating upstream/candidate/HotSpot rounds with the fixed Paper version, seed and port
25599. A missing `Done` is a failure whatever the harness exit status says. The script needs a
Linux host (it reads `/proc`); under Git Bash on Windows even HotSpot fails to start through
it, so there the gate is the same fresh-world start to `Done`, run directly.

Attribute changes with focused generic benchmarks and the generated code. Report observed
timings with their variance. Microbenchmarks or server startup alone do not show overall
Minecraft tick or client-frame gains, so do not claim those from them. Validating the client
game needs a real modpack and a host with graphics. Larger register-allocation and
shrink-wrapping work stays separate.
