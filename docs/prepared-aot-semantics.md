# Prepared AOT semantic repairs

The first full-classpath experiment exposed an unchecked array store and a null
invocation crash. Broader preparation of the probe classes then exposed incorrect
array error messages, an exhausted-iterator shortcut returning null, null throws,
and truncated long/floating-point stores.

The implementation now emits null guards before dereferencing field, array,
invoke, and throw operations. The fault branch returns from the compiled method
immediately. A scoped, nested thread-local Code borrow supplies the original
bytecode position to the existing helpful-NPE formatter. Normal invoke callbacks
retain their five-argument ABI. Synthetic interpreted frames with no Code also
avoid dereferencing a null pointer while formatting an NPE.

Checked casts use the interpreter's existing cast predicate. Reference-store
callbacks use its existing array covariance predicate. Primitive AOT stores carry
their opcode so long and floating-point cells retain their width and bit pattern.
Array callbacks report the index and array length. The native iterator shortcut
declines exhausted next() calls without advancing the cursor, allowing the normal
throwing path to handle them; a real null element still succeeds.

No workload-name, observed-success, or prior-run admission rule was added.
Prepared objects remain code-only and require a new build after the executable
changes. The changes do not establish whole-program no-adapt execution or wall
below five seconds.

## Verification

Release SHA-256:
`BFEFEAF2BDCAF62A2EFEBF2DB99D778435B839BE9C0BDFA67A9AC46E5DB01CF9`.

- 33 AOT unit tests pass, including null-operation exit paths and relocation of
  their opcode/position/depth information. The mock getfield loop uses a non-null
  receiver; its former zero receiver is no longer a valid successful field read.
- All 30 semantic probes match Java with prepared probe classes, accounting only
  for their predeclared divergences. Evidence:
  `%TEMP%/rava-aot-exceptions-162a714bb97a475590f31b2742f42bbe`.
- AOT entry traces confirm that AotExceptionSem's store/cast/call/next methods
  execute prepared code. It checks exceptions, permitted null values, repeated
  exhaustion, valid operations after exceptions, and absent later side effects.
- All 30 probes also pass with prepared mode absent. Log:
  `%TEMP%/rava-aot-exceptions-default-suite.log`.

The first version of AotExceptionSem used unsupported putstatic instructions and
therefore fell back; that result was not accepted as AOT evidence. The final
fixture stores its side-effect counter in an array and all four relevant methods
are both compiled and observed entering AOT.

These checks do not close the separate unchecked covariance path in the JIT's
inline native_aastore emitter, or prove termination immediately after every
non-null callback fault. Full server classpath and reload validation are separate
from the prepared-probe suite above.

## Full classpath follow-up

Preparation with the latest probe classes and server classpath completed in
191,961 ms: 30,443 classes, 269,382 compiled methods, 79,609 unsupported. Evidence:
`%TEMP%/rava-aot-fixed-server-c8fa2e5a9ffb45e2a75bf63d424589e7`.
All 30 semantic probes also pass with these library objects enabled, recorded in
`%TEMP%/rava-aot-fixed-validation-c942a7448e5b4c41a3d676e8e7d28a87/sem.log`.

The first fresh-world full boot reaches Done in 85,726 ms with 45.016 CPU seconds
and peak RSS 1,177,853,952 bytes. Evidence:
`%TEMP%/rava-product-boot-e228e5d3f6df4048a917c591dbd01c98`.
It again has a long pre-Environment interval; this elapsed time is not a clean
measure of code execution cost. No speed improvement is claimed.

The full prepared-classpath validation passes the five product hashes:
ChunkFill `8790f04ace726ecf`; RingScan/RingLane/RingLaneInstance
`37539c6c375d0c3`; NbtSave `ae44a34b42766bd4`.
ReloadBench certifies both cold and warm results with 1470 recipes, 1584
advancements, 625 tags, and hash `c956377e19071d7a`. Its measured reload windows
are 10,771/7,742 ms; its 59,702 ms process duration is not a server boot time.
Logs and outputs are in the validation directory above. These results verify
the tested data paths, not untested Java semantics or the startup objective.

The repeated prepared boot reaches Done in 42,972 ms (42.984 CPU seconds), and
the following default-mode control in 27,208 ms (29.266 CPU seconds). Evidence
suffixes are `37435d4d06984c7d8b8cde02845a602a` and
`bbea72fb87b84eeb80107adfa411167e` under `%TEMP%/rava-product-boot-`.
The prepared backend remains slower and opt-in. Fixing the observed semantic
defects did not close its execution-cost gap. Next measure that gap before
expanding this backend or claiming that offline compilation meets the goal.
