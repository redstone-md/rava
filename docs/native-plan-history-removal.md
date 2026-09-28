# Native plan admission without prior refusal history

`hr_synth` cached a permanent dead marker after a native chain returned
`Ok(None)`. Later calls at the same site and receiver class could not rebuild
the plan. This selected a future execution route using an earlier result rather
than bytecode, descriptors or current resolved structure.

The plan table now contains only structural plans. An unhandled call falls back
to ordinary dispatch without changing that table or clearing its native-plan bit.
The `unserved` counter is diagnostic only. Invalidation when available bytecode
changes, receiver/arity checks, GC roots, argument conversion and exception
handling remain in place. The historical threshold and name-based decisions
elsewhere in RAVA are not removed by this change.

The cost risk is repeated attempts for calls no native can handle. The former
dead marker avoided those attempts. No speedup is implied by deleting it; an
eventual static native contract can reject unsupported shapes without consulting
execution history.

## Validation

Release build succeeded. Candidate SHA-256:
`18985DD93BF325683874C8084A59E0FEE378A1B95799DD30DB65B562E3F4BD1A`.
Evidence directory: `%TEMP%/rava-native-plan-20260905`.

All 29 semantic probes match Java `-Xint` under standing flags, with each probe's
existing declared divergences. `NativeDeclineSem` adds 512 repeated unresolved
native calls and verifies every call throws `UnsatisfiedLinkError`. It is an
exception-regression check, not by itself proof that a particular native plan
was installed or that every no-adapt requirement is satisfied.

Five product hashes pass: ChunkFill `8790f04ace726ecf`, RingScan/RingLane/
RingLaneInstance `37539c6c375d0c3`, and NbtSave `ae44a34b42766bd4`.
Cold and warm ReloadBench outputs both certify 1470 recipes, 1584 advancements,
625 tags and hash `c956377e19071d7a`. Its 44,402 ms process duration is a
correctness run, not full Minecraft startup timing.

The first fresh-process boot pair, candidate then previous release:

| Binary | Wall to Done | CPU | Evidence directory suffix |
| --- | ---: | ---: | --- |
| Candidate | 30,621 ms | 32.34375 s | `78003b6886c648d1ae78dc5b7ba0b27d` |
| Previous | 32,668 ms | 34.796875 s | `ecae0276acd64f5da634322a9cf7b559` |
| Previous, repeat | 31,138 ms | 33.00000 s | `d6954f179cbc4ad2a480e87d96925636` |
| Candidate, repeat | 30,431 ms | 32.78125 s | `01d554067b6c4647af343dce9af09672` |

Directories are `%TEMP%/rava-product-boot-<suffix>`. The previous binary is
SHA-256 `BB58CE690AEB510DAD5DF0B0495B74A360023E98DD5E918BBB383DF42FA23880`.
Both use standing flags, fresh processes and independent pristine-world copies,
port 25599, affinity 0x0FFF, with no profiling or snapshots. Host quiet counters
remain unavailable. Both opposite-order pairs favor the candidate, but this
small series does not establish a precise causal speed improvement. The runtime
change is retained for eliminating the past-result admission rule with passing
correctness checks and no observed startup regression. It does not prove the
unhandled branch's frequency, full no-adapt compliance, or the below-five-second
goal. The most recent verified full startup is 30,431 ms.
