# Bounded pure integer call chains

The existing same-class integer inliner accepted only bodies without calls.
A small helper calling another equally pure helper consequently went through
the runtime invocation machinery. Extend the existing representation with
nested static calls, preserving class-file-only eligibility.

Every node must satisfy the existing static, non-synchronized, non-native,
non-initializer contract and the integer-only descriptor/opcode subset. No
heap operations, division, exceptions or external-class calls are admitted.
Active method identities reject recursion. A shared 128-instruction budget
charges each repeated expansion, and a depth ceiling of eight bounds traversal.
The existing 96-byte per-body bound still applies. This is not profile-guided
inlining, and compilation does not execute class initializers.

Nested arguments and results remain I32 SSA values; only the outer boundary
converts the I64 cell ABI. Addition/subtraction retain Java wrapping behavior.
The inline artifact namespace advances to v5; full keys include this identity
and already bind the immutable class inputs.

Tests execute generated native code with callback symbols resolved to zero,
check overflow and argument order, and reject exponential repeated expansion.
All 71 AOT-selected library tests pass (one ignored), including the three new
native-execution/budget tests. Log: `%TEMP%/rava-numeric-chains-tests.log`.
Release application validation and timing are pending. The frozen release used
for the ordinary-JIT versus prepared comparison predates this change. No
full-boot speedup is claimed.
