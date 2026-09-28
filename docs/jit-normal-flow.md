# JIT normal-path reachability

The block emitter previously refused bytes after a terminal instruction unless
they were a leader with an already published operand-stack shape. A trailing
exception handler therefore blocked normal-path emission even when its cold
execution was intended to remain in the interpreter.

NormalFlow computes syntactic reachability from PC 0, following both conditional
arms, goto, all switch cases and the default target. Returns and athrow have no
normal successor. It never follows exceptional edges or guesses an empty stack
for a handler. All branch targets are checked by the existing leader builder;
that builder now rejects truncated fixed-length instructions before reading
branch operands. Live fall-through past the method end is also refused.

The emitter constructs this map lazily at an otherwise-refused instruction.
An unreachable instruction is skipped at its original bytecode offset. A live
block without a known stack shape still refuses compilation. Normal methods
whose block shapes are already known do not run the extra graph walk. No
receiver observations, hotness counts, cache outcomes or timing enter the map.

This change does not enable methods with exception tables: the compile-driver
refusal remains. It prepares their normal path for a later cold handler exit,
and also permits ordinary bodies containing dead instruction regions. Other
analyses still inspect the original bytecode; unsupported instructions or
unresolvable references in dead regions can still cause a safe refusal.

## Validation

All 157 JIT unit tests passed with next-gen-gc. Five new graph tests cover
terminal instructions, goto gaps, conditional paths, backward edges, switches
and malformed input. Five emitter tests include actual compiled executions of
a handler-shaped dead trailer, a goto gap and a dead loop between live branches.
They also assert refusal of a live block whose stack shape is not yet known and
continued refusal of methods with exception tables.

Release build with next-gen-gc passed. All 48 semantic probes matched java -Xint
with the existing declared divergences. ChunkFill, RingScan, RingLane,
RingLaneInstance and NbtSave passed their five product hashes without timeout.

Frozen control SHA-256:
62C48966B7A0BC2E1DC2B93FDF9CA94C1A079F6F6269411EA75538D83BCD0954.
Frozen candidate SHA-256:
659A37D667531B6B0CE7C4F7C6AFC0E349FBAE8D9AB15DE49991D8E6CEF194C6.
The control is the validated 8e0bb26c release. The candidate also incorporates
the subsequent exception-transition/type-analysis foundations and disabled
phase-trace instrumentation; this is a comparison against the retained release,
not an isolated attribution of every timing difference to the block emitter.

Null-first interleaved fresh-world launches, first-entry JIT, cap 16384:

| Run | Wall to Done (s) | Process CPU (s) |
| --- | ---: | ---: |
| null0, control | 27.408 | 44.796875 |
| null1, control | 27.307 | 45.968750 |
| candidate0 | 28.538 | 47.796875 |
| control0 | 27.559 | 46.828125 |
| control1 | 28.279 | 47.515625 |
| candidate1 | 27.928 | 47.234375 |

Null spread was 0.101 s. Candidate-minus-control paired deltas were +0.979 s
and -0.351 s, with control drift of 0.720 s between the paired controls. There
is no consistent speedup. All runs reached Done without instrumentation, used
verified frozen hashes and matching environments, and had nonoverlapping
launch-to-Done windows. Performance counters for the quiet check were unavailable.
No builds or tests overlapped the boot series.

The implementation is retained as the normal-emission prerequisite for cold
exception handling, not as a demonstrated wall optimization. First-entry mode
still does not prove full no-adapt execution. The full goal remains wall <5 s,
no-adapt, no snapshot, and is not achieved.

Evidence under TEMP: rava-jit-normal-flow-dd34042a7da049f5ab77d97b10b1e605.
Includes both binaries and hashes, source patch against 72fa8428, unit/build
logs, semantic results, product gates, copied harnesses and all six launch logs.
Temporary artifacts are not guaranteed durable.
