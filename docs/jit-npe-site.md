# Bytecode identity for JIT null invocation faults

The String length/hash null branch already roots references and exits the
compiled body, but its synthetic invocation lacks the original bytecode PC.
Consequently helpful NPE source clauses differ once a method compiles.

Pass the pinned class, Code identity and PC to a small cold callback, reusing
npe::source_at and npe::invoke. These identities come from the exact emission
environment (including a splice's own class and Code). Preserve rooting, pending
exception precedence and deep epilogue; gate exception construction on workers.
Successful String operations gain no new runtime test. Append the callback to
the AOT symbol table to preserve existing ordinals, without changing AOT policy.

First integration covers the existing String.length/hashCode null branch only.
Other invocation, field, array and monitor faults remain follow-up integrations.
The full wall <5 s, no-adapt, no-snapshot objective remains open.

Validation plan: Java/control differential probe for local/field receiver source,
int/void method returns and post-fault effects; full semantic suite, JIT execution
statistics, product hashes and a fresh-world boot. New files stay below 500 lines.

## Validation result

Final release build with next-gen-gc succeeded. A preliminary build was followed
by an SSA correction before tests: GC reloads update a cloned cold-arm operand
stack, never the nonnull arm's live value descriptions. Only the rebuilt final
binary was tested.

Frozen control lost both source clauses in JitNpeSiteSem. Candidate matches Java:
8192 caught exceptions, zero post-fault effects, local0 in the length message
and JitNpeSiteSem.field in the hash message. All 48 semantic probes matched
java -Xint with their existing declared divergences; none were added.

Separate JIT statistics with nursery 64 KiB: localLength ran compiled 4003 times,
fieldHash 4004; each compiled once without failure. Main remained interpreted.
The diagnostic reported zero arena allocations, so this run is not evidence of
minor GC or moving-root coverage. Spliced and off-thread null-fault execution
are also not established by this probe; their identity/gate handling was checked
in source only.

All five product gates passed expected hashes, exited 0 and did not time out.
Fresh-world uninstrumented boot reached Done in 27.679 s, CPU 46.640625 s;
quiet performance counters were unavailable. This single observation is slower
than the prior single 26.957 s observation, and establishes no speed gain or
paired regression estimate. No performance improvement is claimed for this fix.
Boot directory under TEMP: rava-product-boot-4fcb6fa98c3e422c87b72359db0dc2b3.

Evidence directory under TEMP:
`rava-jit-npe-site-20dc25e0d04949c1b107b55a6a5ae1bf`.
Includes frozen binaries, build logs, control and candidate semantic outputs,
JIT/arena diagnostic output, environment, product results and boot output.
Temporary evidence is not guaranteed durable.

Candidate SHA-256:
`62C48966B7A0BC2E1DC2B93FDF9CA94C1A079F6F6269411EA75538D83BCD0954`.
Control SHA-256:
`27DB068A205EF50F1C4242CC40B53195B8720235961B1B26B2A3D0C402A9E22C`.
