# AOT reference identity

Prepared CFG validation failed in CompSem during Minecraft bootstrap: a shape
lookup returned null after a reference-comparison predicate rejected matching
values. AOT compared raw cells, although separate BOX_TAG entries can represent
the same reference. The interpreter and JIT already decode those cells.

Use the existing non-allocating rava_acmp callback for both reference branch
opcodes. This is independent of class names, workload and invocation history.
Advance the full-method cache namespace to avoid reusing raw-comparison code.

The machine-code regression test creates two different ABI boxes from one
shared string reference and compares them through a merged boolean result.
Before the fix it returned false. After the fix all 48 AOT unit tests passed,
including both reference branch opcodes, null operands and CFG/GC tests.
The following optimization skips the callback for equal cells or two non-boxed
cells, matching the existing JIT representation check. A serialized-code test
counts actual callback entries for equal/distinct ordinary cells, equal/distinct
boxes and both mixed operand orders. All 49 AOT tests pass with this fast path.

The callback-corrected release passed CompSem against java -Xint: 48 compared
lines, with the probe's two existing declared divergence patterns excluded.
The previous VoxelShape null failure disappeared. Preparation compiled 292497
methods from 30450 classes and refused 56521. Evidence is under
`%TEMP%/rava-cfg-identity-server-0ec00c2fa9854e69b6de7b1dd8b767e0`.

Full prepared validation and boot timing of the fast-comparison release completed
with fresh artifacts under
`%TEMP%/rava-cfg-identity-fast-server-aacd4d4204d9401c9be2448a550c095d`.
All 34 semantic probes and six product gates passed, including both ReloadBench
hashes. Release SHA256 is
`49F1A270390892EED5C7E02B5471FFD77E7887F95479CCC522E304314A78A818`.

Ordinary prepared boot: 51.720 s wall, 43.703125 s CPU, 1167278080 bytes peak RSS;
evidence `%TEMP%/rava-product-boot-528b3c6b6fab4d53892b0f80b431f35f`.
High-threshold/no-background boot: 81.952 s wall, 79.984375 s CPU, 1076215808 bytes
peak RSS; evidence `%TEMP%/rava-product-boot-2e045c81c8bc41a7a3945178656bcb36`.
Both used fresh world copies and no snapshots; quiet-box counters were unavailable.
The high-threshold result is unchanged from the earlier 81.996 s. Expanded CFG
coverage and corrected identity do not establish a startup performance gain.
High threshold still does not certify execution without JIT adaptation.
