# Prepared AOT fallback coverage

The target remains full boot below five seconds, no adaptation and no snapshots.

## Evidence, 2026-09-05

F7F9E147... (object-leaf release) with its full prepared classpath reached Done
in 81.931 seconds, CPU 79.9375, when RAVA_JIT_THRESHOLD=2000000000 and
RAVA_BACKGROUND_JIT=0. Other standing flags were unchanged. This is a bounded
experiment suppressing normal hot promotion, not proof a counter can never
reach the threshold or that all adaptive policies are disabled.
Evidence: `%TEMP%/rava-product-boot-829ec6c45e8346b6891cbe27495e9f41`.
The ordinary prepared repeat with hot fallback was 38.921 seconds.

A RAVA_JIT_STATS=1 / RAVA_JIT_STATS_MS=3000 diagnostic run timed out at 300.008
seconds before Done (CPU 168.203125). Do not use it as a boot benchmark or a
complete call census. It names methods reaching jit_run after AOT declined.
Evidence: `%TEMP%/rava-product-boot-26e6d1d3f4ad436782f428127f175801`;
`fallback-top.json` joins each method's last observed counters to methods.tsv.
Direct machine calls bypass these run counters; counts are not CPU attribution.

The leading observed compiled-run rows include:

| Method | Observed runs | Prepared manifest |
| --- | ---: | --- |
| BlockBehaviour$BlockStateBase$Cache.lambda$new$0 | 35,394 | unsupported |
| HashCommon.mix(int) | 30,390 | unsupported |
| Block.lambda$getShapeForEachState$11 | 21,507 | unsupported |
| VoxelShape.lambda$findIndex$4 | 17,226 | unsupported |
| Joiner.toString(Object) | 15,560 | unsupported |

These names are diagnostic evidence only. Eligibility must remain structural.
The actual class bytecode was inspected with javap and saved in
`%TEMP%/rava-object-region-server-9141a6ecae7c41b499967b9d03e37598`:
`HashCommon.javap`, `BooleanOp.javap`, `BlockStateCache.javap`.

## Int shifts implemented

HashCommon.mix(int) contains iushr, which the full AOT emitter did not implement
(long shifts existed). Added ishl/ishr/iushr with 32-bit arithmetic, distance
masked by 31 and sign-extended result cells. Reference/wide operands are refused.
41 AOT unit tests pass, including 408 serialized-machine-code shift cases and
invalid operand shapes. Release SHA256:
`A7B34EFC903B01F0ED877AFFB03405E32BC47E8040189B65F2E9EF341B4DDAEF`.

IntShiftSem matches Java -Xint, including actual fastutil HashCommon mix results.
Loop-shaped shift helpers confirm actual prepared entries for all three shifts;
the initial straight-line helpers used existing runtime shortcuts, so matching
output alone was not sufficient. HashCommon.mix(int) is now compiled in the
prepared manifest, but no actual AOT entry was observed for it in this short
probe. Evidence: `%TEMP%/rava-int-shift-48796f73c2e64129a8adb6b0516eb58d`.
All 33 ordinary-mode semantic probes match Java -Xint with the existing declared
divergences. Full preparation completed with 30,449 classes, 270,351 compiled
methods and 78,660 refusals in
`%TEMP%/rava-int-shift-server-9c4df71e85f44668906a01c2cd944598`; its executable,
classpath, binary hash, sem.log and validation script are preserved separately.
All 33 prepared semantic probes and all six product hash gates passed, including
cold and warm ReloadBench. Prepared boot reached Done in 49.249 seconds, CPU
41.75 (`%TEMP%/rava-product-boot-fe6983d50224468aa3783b3b56b8c277`). The bounded
high-threshold/no-background run reached Done in 81.996 seconds, CPU 79.890625
(`%TEMP%/rava-product-boot-aaf572828637483a91c111d727dc8154`). Quiet-box counters
were unavailable. The latter is essentially unchanged from the 81.931-second
object-leaf baseline: extra opcode coverage alone did not solve hot fallback.

## Next: operand-stack values across CFG edges

The full emitter currently creates an empty operand stack at each block and
rejects nonempty outgoing stacks for branches and fallthrough. This rejects
ordinary short-circuit booleans and conditional object expressions. The inspected
BlockStateCache lambda ends with two paths pushing iconst_1 / iconst_0 and joining
at ireturn; BooleanOp's nontrivial truth functions have the same structure.

Implement typed block parameters and propagate their reference/wide cell shapes.
Use a worklist seeded by the entry block so every emitted block already has its
incoming stack shape. Merge edges must agree; backedges must validate the same
shape. A separate function-entry block should own ABI parameters, allowing loops
to branch to bytecode PC 0 without inventing ABI arguments. Preserve reference
root spilling at subsequent callbacks. Validate boolean truth tables, object
joins with moving GC, backedges and incompatible merge rejection, then remeasure
the full prepared boot with and without normal hot fallback. Do not use a list
of observed method names to select these transformations.
