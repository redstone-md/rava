# Direct prepared static calls

The no-JIT profile attributes little time to prepared machine instructions and
substantial time to the callback, synthetic-frame and invocation machinery.
Add a static same-class prepared route before the general compiled-call fallback.
Resolve from the caller's immutable constant pool and declared method flags;
reject non-static, synchronized, native and initializer targets. Preserve the
shared AOT class-initialization, representation and worker guards. No observed
receiver, hot count or method-name admission list is used.

Raw arguments remain rooted across cold linking. The shared frame packer accepts
either interpreter locals or raw call cells, inserts category-2 padding for raw
long/double arguments, and preserves reference forwarding. Execution, local
roots, exception extraction and result decoding remain in the existing runner.
Decode reference results before truncating callee boxes, then rebox for the
caller. Unsupported methods retain the existing fallback.

55 AOT-related tests pass, including raw/value wide-slot equivalence. The release
AotDirectSem probe matches Java for mixed primitive arguments, reference/array
identity, boxed results, mutation, allocation and NPE. Trace confirms direct
entries for all five checked callees. A second process with a 64-KB nursery
matches the same output and reports actual ARENA-GC events while retaining an
object across nested allocating calls. Evidence:
`%TEMP%/rava-aot-direct-31b980adb8094a329e8700f108919b9a`.
The final source moves direct execution after the prologue meter closes,
avoiding diagnostic inclusion of callee time. Its rebuilt release passes the
same tests and both Java runs; the stress trace reports seven collections.
Final release SHA256:
`D1402D988FB11CE9C24264E8567FB5B1825EA2B4AF6C75C01B5519D194178C65`.
Final probe: `%TEMP%/rava-aot-direct-ee07ce6f5a9e4045aa969d46296fdd54`.
Existing
general AOT limitations on immediate pending-exception checks are not fixed by
this routing change. Full boot and no-adapt certification remain outstanding.

The extension admits public static targets in initialized classes, and public
virtual/interface targets declared by the exact receiver class. Require positive
type-closure evidence for the symbolic owner. Keep inherited, synchronized and
unresolved targets on the existing path. Root raw arguments before resolving any
type metadata. Validate alternating overrides, interface dispatch, static
initialization order, receiver/null handling and moving GC before accepting it.

The first dispatch probe matched Java but did not enter direct interface targets:
the symbolic interface was not loaded. Resolve its metadata using get_or_load
(which does not initialize classes), then require a declared public non-static
symbolic method before selecting the receiver implementation. The revised
release probe passes both ordinary and 64-KB nursery executions, with confirmed
direct Base/Derived interface entries, static entry and retained receiver entry.
Synchronized calls are explicitly checked not to enter the direct route.
Evidence: `%TEMP%/rava-aot-dispatch-4c9a29eff49e4fd7911b402a52242266`.
55 AOT unit tests passed before this metadata-resolution correction; the rebuilt
release exercises the correction in the dispatch probe. Full validation and
isolated boot timing for the extension remain outstanding.

The preceding static-only executable passed all 35 semantic probes with JIT=0.
The original no-JIT baseline passed CompSem on its 300-second retry and five
product gates, but ReloadBench exceeded its 600-second limit without a result.
Its automatic boot stage therefore did not run. Preserve that timeout as a
failure, not as a passing baseline or an isolated wall-time measurement.
