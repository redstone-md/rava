# Status at the 2026-09-05 21:00 Budapest deadline

The requested wall below five seconds without adaptive eligibility or snapshots
is not achieved. The original baseline remains 27-30 seconds.

Current release SHA256:
`6AA554379870637E131AED4C8E1E6AF88A9A611485F4193C35845572300C7B39`.
Its ordinary-JIT full boot is 25.148 seconds; this is a reference, not proof of
the requested no-adapt result. See `prepared-early-call.md` for exact evidence.
The earlier frozen progress-fix release's prepared boot is 138.584 seconds.
Its archive must not be used with the current executable.

Retained: prepared-loop progress checkpoints, passing asynchronous worker/GC
probe, 40 Java semantic comparisons on the progress-fix source, and completed
prepared ReloadBench with matching cold/warm hashes. Removed: experimental
early prepared dispatch, after reverse-order boot measurements failed to show
a repeatable improvement. Runtime source matches commit 20e6df51.

A final ordinary interpreter control used JIT=0 with no prepared directory,
fresh pristine world and no snapshots. It terminated after 32.896 seconds with
process exit 0, but printed `Failed to start the minecraft server` and never
printed Done. This is a startup failure, not a performance result or timeout.
The visible logs do not expose its underlying exception. Evidence:
`%TEMP%/rava-product-boot-4419ccb993a0404bb779e8cf360dae01`.
The owned process is terminal; the harness restored the power scheme.

Outstanding: diagnose the interpreter-only startup failure, establish a
sub-five-second full startup under the actual no-adapt restrictions, and verify
all remaining product workloads. Current tests and reference timings do not
prove these requirements. No completion or speedup is claimed.
