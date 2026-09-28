# Prepared runtime cost attribution

The five-second startup goal remains unmet. Before replacing the compiler
backend, distinguish machine-code execution from repeated runtime entry work.

The opt-in diagnostic of the BC75B0B0 release, with JIT disabled and resolved
target memoization enabled, ran BootstrapBench for 30 seconds. It observed
14,421,492 prepared frames, 1,548 cold links and 129,281,800 nanoseconds in cold
linking. Only 181 of 5,332,934 interpreted entries had exception handlers.
These counts concern this partial bootstrap execution, not the whole server.

Evidence: `%TEMP%/rava-aot-resolved-server-4d64a84607af478d8ff430532c3cac99/`
`runtime-costs-6c113454518643a494921d57626bb8d8` (env.json, out, err).
The deadline diagnostic exited normally by design; it did not finish bootstrap
or establish a product result hash.

Packing estimates were 1.442 seconds using the existing trimmed sample metric.
The native-stream meter's trimmed estimate was 0.483 seconds, but its mean was
1,977 nanoseconds per event across 6,984,750 events. Neither discarding this
tail as scheduler noise nor attributing it all to native operations is justified:
the scope can execute guest code, and prepared execution had no nested meter.
These sampled elapsed-time estimates are not a CPU decomposition.

Add three opt-in meters using the existing nested timing implementation:

- `aot.resolve`: target selection inside direct calls, ending before execution.
- `aot.run`: the shared runner, excluding any metered nested scopes.
- `aot.execute`: the native function invocation, excluding metered callbacks.

Keep argument packing separately measured. Nested calls must close their
measurement scopes normally so their enclosing scopes subtract them. The
existing wall sampler remains independent. No admission, code generation,
representation or guest behavior changes. The meter names describe scope;
unmetered child work is still charged to its enclosing scope.

Validate the AOT tests and a prepared dispatch/GC probe, then repeat the same
diagnostic with matching executable and objects. Do not run builds during
the reverse-order BC75 boot comparison. Use a fresh release measurement for
any subsequent performance claim; diagnostic durations are not speedups.

The changes pass `cargo test -p rava-interp --features next-gen-gc aot`:
62 passed, one ignored, zero failed. Log: `%TEMP%/rava-aot-envelope-tests.log`.
The new diagnostic runtime and prepared dispatch probe are not yet validated.
The previous frozen release was blocked by Defender during the reverse-order
boot attempt; see prepared-resolved-targets.md. Do not substitute another copy
or rebuild merely to bypass that detection.

## Updated release measurement

Release B3E74E005794032D665CCE34D097024BC6854849BB5FF888AA38E8D8BAC152CB
includes the new meters and the arithmetic/call/runtime-callback exception exits.
Its matching prepared set and all 38 Java differential probes pass. Evidence:
`%TEMP%/rava-callback-exits-server-f753a5ce6c924346b499d0e0a71d5805`.
The sequential diagnostic completed with exit 0 by its 30-second deadline;
`diagnostic.env.json`, `diagnostic.out` and `diagnostic.err` record its setup.
JIT was disabled, resolved-target memo enabled, with no snapshots.

| Meter | Events | Trimmed estimated seconds | Reported wall share |
| --- | ---: | ---: | ---: |
| aot.execute | 13,098,579 | 10.225 | 34.08% |
| aot.resolve | 26,722,914 | 2.637 | 8.79% |
| aot.run | 20,545,434 | 2.132 | 7.11% |
| aot.pack | 13,098,593 | 1.323 | 4.41% |
| price.interp_self | 4,968,074 | 1.809 | 6.03% |

Cold links consumed 117,303,800 measured nanoseconds across 1,486 attempts.
The native-stream mean is now 140.1 ns/event, versus 1,977 in the prior run;
that comparison is consistent with attributing nested prepared execution to
its own meter. It is not a native-stream speedup: instrumentation and generated
exception checks changed, and fixed-time runs covered different amounts of work.

The execute scope includes unmetered callback work, not just native guest
instructions. Trimmed estimates are not additive CPU accounting; the run-meter
mean (990.8 ns/event) differs greatly from its trimmed value (103.8 ns/event).
Do not turn these values into an exact achievable speedup. They support pricing
the full execution/call boundary, and still argue against optimizing cold object
loading as the main route to five seconds. An uninstrumented boot is pending.
