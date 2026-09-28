# Native call regions: evidence and implementation contract

Objective: full process-to-Done below five seconds, without adaptive admission
or snapshots. Neither existing ordinary execution nor prepared AOT meets it.

## Crossing census, 2026-09-05

Saved diagnostic executable SHA256:
`1322F8345AE62CB77F3680483D0BDC09CF28C867717DAE8069540AF933606368`.
It is the earlier numeric-inline diagnostic build, not current HEAD. It uses
its own matching prepared objects from
`%TEMP%/rava-inline-profile-322bb016e5674347b46026d0bebd11d3/objects`.
Run evidence:
`%TEMP%/rava-product-boot-00bbd3533f6e437ebbb7dfa2bcf1b166`.

RAVA_INVOKE_CENSUS=1 and periodic PC profile tables were enabled. Done occurred
at 51.728 seconds (55.453 CPU seconds); this instrumented duration is not a
performance baseline. Quiet-box verification was unavailable. The runner allowed
3.5 seconds after Done to flush diagnostics, then killed only its owned process.
The last cumulative census therefore may include post-Done execution and is not
an exact count bounded by Done. It reports 29,653,141 crossings, 4,765 targets.
It counts the shared rava_invoke bridge, including both AOT and JIT callers;
target strings name constant-pool references, not resolved receiver classes.

Largest reported targets (diagnosis only, never admission rules):

| Target | Calls | Share |
| --- | ---: | ---: |
| Iterator.hasNext | 1,026,585 | 3.46% |
| Object constructor | 787,857 | 2.66% |
| Iterator.next | 719,231 | 2.43% |
| Map.put | 701,835 | 2.37% |
| Map.get | 683,464 | 2.30% |
| String.length | 480,947 | 1.62% |

No single listed target dominates the crossing count. Counts do not establish
CPU cost: allocation, implementation work and dispatch cost differ per call.
This supports investigating general object-call regions, not predicting a
speedup or adding target-name special cases.

An initial run in `rava-product-boot-aae9418f49df41cd8e38fce185b97c3a`
enabled PC_TRACE. dump.rs returns before the census in that mode, so it yielded
no census table and must not be used as crossing evidence.

## Existing mechanisms and next tracer bullet

JIT emit/calls.rs and dispatch_pic.rs already implement direct machine calls.
Prepared AOT emit_ops_rest.rs normally marshals arguments, spills reference
roots, calls rava_invoke, and reloads roots. NumericLeaf is the current bounded
same-class static exception. Generating Rust with the same callback ABI would
preserve these costs.

The next code experiment should remove a complete same-class call boundary
with object arguments/results, rather than extend the scalar opcode list.
Eligibility must derive from immutable bytecode, descriptors, resolved method
flags and effects. Start with statically bound calls; do not assume that a
virtual receiver has the caller's class. Reject unresolved overrides,
synchronized/native methods and unsupported exception control flow.

Required implementation proofs: correct slot/category-2 argument layout;
class-initialization order; receiver null behavior; GC root visibility across
allocation and forwarding; immediate propagation of pending exceptions;
native-backed representation compatibility; cache identity including callee
code; no receiver-history or hot-count eligibility. Preserve existing fallback
for unsupported shapes, but do not certify whole-runtime no-adapt from this.

Validation must execute object mutation, aliasing, allocation/GC and exception
cases against Java, then measure the full isolated boot. A small region probe
alone cannot establish the objective or justify expanding the compiler.

## Object-leaf implementation

ObjectLeaf now recognizes same-class static, non-synchronized, non-native
straight-line methods (at most 96 bytecode bytes and 16 narrow arguments).
Supported operations are argument loads, dup, instance field reads/writes and
typed returns. Every field must be declared in that class, non-static and
non-volatile; writes to final fields are refused. Every receiver must originate
directly from an argument whose descriptor names that class. Dereferencing a
field result, allocating, branching and nested calls are refused. Wide values
are refused, rather than reinterpreted using a narrow argument layout.

All dereferenced argument cells are checked for null before any fast-path write.
A null takes the original invocation, preserving earlier writes and the original
NPE location, then exits the caller. With no branches, every admitted receiver
is necessarily dereferenced, so this slow arm necessarily throws. Non-null arms
use the same non-GC field callbacks as existing AOT field emission and eliminate
the method-entry bridge. No new allocator, object representation or root ABI is
introduced. Same-class AOT entry retains the existing completed-class-init and
native-representation guards. Field declarations/flags now participate in the
artifact key alongside callee code.

39 AOT unit tests pass, including actual machine execution with stub field
callbacks: distinct/aliased receivers, object identity, no invoke callback on
the fast path and no fast-path write before null fallback. Rejection cases
cover method/field flags, indirect receiver origins, allocation and nested calls;
callee and field-flag changes invalidate caller artifacts.
An additional test runs the inlined reference result across a following callback
that forces arena collection: it asserts both a changed address and preserved
object contents. The first version omitted frames::register and gathered zero
roots, which an address-change assertion caught; the corrected fixture registers
and unregisters its thread. The production collector was not changed.

The prepared Java probe passes against Java -Xint, with confirmed AOT caller
entries and no leaf-runner entries in the non-null run. It covers integer
extremes, reference aliasing, null values and writes preceding NPE. Reference
retention across allocation pressure also passes; this does not prove that a
collection actually occurred. System.gc is unavailable in the existing runtime
(UnsatisfiedLinkError), so the probe does not depend on it.
Evidence: `%TEMP%/rava-object-leaf-0cae0aafb27a41afaa79e2598fa74a30`.
All 32 ordinary-mode semantic probes match, with existing declared divergences.
The full prepared classpath contains 30,448 classes, 269,395 compiled methods
and 79,611 refusals. Saved release SHA256:
`F7F9E14752C83D9E5109663B2599E39F5A075113CE1073880AE17C9290FC3B2E`.
Executable and matching objects:
`%TEMP%/rava-object-region-server-9141a6ecae7c41b499967b9d03e37598`.
All 32 prepared-mode semantic probes and the ChunkFill, RingScan, RingLane,
RingLaneInstance, NbtSave and cold/warm ReloadBench hash gates pass. Evidence:
`%TEMP%/rava-object-region-validation-00d5c5e38cdf4e09a5ca51fcee58db7d`.

Uninstrumented isolated boots, with quiet-box verification unavailable:

| Build/run | Wall seconds | CPU seconds | Evidence suffix under rava-product-boot- |
| --- | ---: | ---: | --- |
| Object leaf, first | 47.774 | 40.4375 | 1c7c4dfb68e9486bbc3b32a80b1b8428 |
| Previous prepared AOT control | 41.199 | 40.5625 | 5b1487828d5e48c584d62936fab8c1c3 |
| Object leaf, repeat | 38.921 | 39.296875 | a9da4100de8b4322a5e864a2e7d7a39d |

The control is the saved A1BF61D1... executable and matching objects, predating
the getter-history removals as well as this inline change. This is not an
isolated measurement of the object-leaf change. The repeated result is close to
earlier 38.837/38.967-second AOT measurements, so a durable whole-boot improvement
is not established. It remains slower than ordinary execution around 28 seconds.
The completed tracer bullet proves removal of eligible object method boundaries,
not sub-five-second performance or whole-runtime no-adapt compliance.
