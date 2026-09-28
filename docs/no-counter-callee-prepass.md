# Remove invocation-selected callee precompilation

The old `tier2_precompile_callees` selected callees that had already executed and
were within the recursion depth of their invocation threshold. Its `Cold(n)`
admission was execution-history dependent even though it only advanced compile
order. The prepass, active stack, counters, call site and opt-out are removed.
Ordinary JIT compilation and `force_compile` remain. Compile-only RAII and the
worker drain's own recursion guard remain required and are preserved.

This does not establish whole-runtime no-adapt compliance. Initial invocation
thresholds, worker-hot admission and receiver-profile inline planning remain.
It also does not establish a speedup or the wall-under-five-seconds goal.

## Structural replacement boundary

Do not replace the old `Cold(n)` predicate with unconditional `force_compile`.
The prepass checked that the symbolic owner was cached, then called the general
`Vm::resolve_method`. That resolver can load superclass and interface definitions
on a cache miss. Therefore the owner-cache check does not prove a compile-time
walk is free of loading. Prior entry also supplied an implicit initialization
assumption which a structural plan cannot inherit.

A replacement must use resolved dispatch and bytecode/effect evidence, traverse
without loading new classes, account explicitly for static initialization, and
handle call-graph cycles before publishing code. Names may resolve symbols but
must not select an optimization by workload identity. Existing decoder, loader,
compiler and dispatch proofs should be reused rather than duplicated.

## Verification

Control complete: `%TEMP%/rava-tier2-control-20260905`, two-pair ABBA using the
existing `RAVA_NO_TIER2` on the post-profile-recompile-removal binary, SHA-256
`9C88D73C97368279EC04577093F60FDE754055B1E7BD01654AF4DB519705C6BE`.
The deliberate stale-source override covers only the newer deopt comments at
test launch. Later source edits cannot alter this preserved executable.

All four runs exited 0 and certified cold/warm counts and hash
`c956377e19071d7a`. Warm A was 7532/7607 ms; disabled B was 7181/7603 ms.
The arms overlap. B median warm was -2.34%, CPU +4.62%, cycles +3.73%; cold
paired differences had opposite signs. This short, variable series proves
neither a speedup nor a clear performance cost. It does not measure full boot.

Release build passes. Candidate SHA-256:
`AF79E7B6563E890621BF9BDE67575105CACE4881202EF34F1831336F849F8FAF`.
Candidate evidence directory: `%TEMP%/rava-no-counter-callee-20260905`.
All 27 semantic probes match Java `-Xint`, accounting for their existing declared
divergences. Five product processes exited 0 with the expected hashes: ChunkFill
`8790f04ace726ecf`, RingScan/RingLane/RingLaneInstance `37539c6c375d0c3`, and
NbtSave `ae44a34b42766bd4`.

The fresh candidate ReloadBench exited 0 and certified both cold and warm as
`c956377e19071d7a`, with 1470 recipes, 1584 advancements and 625 tags. Cold was
8200 ms, warm 7465 ms and process wall 47152 ms. This single candidate correctness
run is not a paired performance comparison or a full-server boot result.
Product processes used affinity `0x0FFF`, a copy of the main configuration and the
seven standing variables recorded in `product.env`, after clearing inherited
`RAVA*` variables. No world, snapshot or restore input was used.
