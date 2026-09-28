# JIT execution of methods with exception handlers

## Evidence and scope

Source HEAD 900c245b. Normal release remains the validated 8e0bb26c executable,
SHA-256 62C48966B7A0BC2E1DC2B93FDF9CA94C1A079F6F6269411EA75538D83BCD0954.
The runtime compile driver unconditionally refuses nonempty Code.exceptions.
Inlining also refuses handler-bearing bodies because its CFG does not model
exception edges. Removing either refusal without implementing those edges is
incorrect. The historical first-receiver trivial-getter table remains disabled;
it is not an acceptable shortcut around this coverage gap.

Fresh-world first-entry census enabled only exec.interp_has_handlers and
exec.interp_no_handlers, with one-second JSON dumps to a dedicated file.
Run under TEMP: rava-product-boot-8d33458793f44b44a2e923be1d2a2389.
Instrumented Done wall was 29.289 s, CPU 49.8125 s; no speed claim. Quiet counters
were unavailable. Last complete snapshot:

| Interpreted entry population | Count |
| --- | ---: |
| Method has exception handlers | 277741 |
| Method has no exception handlers | 831481 |
| Total | 1109222 |

Handler-bearing methods account for approximately 25.04% of these entries.
File last-write epoch is 1788661076733 ms, 877 ms before the harness observed
Done at 1788661077610 ms. This is a near-end cumulative snapshot, not the complete
launch-to-Done population. Counters are entry-weighted, not time-weighted; a small
number of long methods can dominate cost. Absence of handlers does not imply
that the compiler supports every other feature in a method.

Evidence under TEMP: rava-jit-handler-census-be8fd624d47e46d386098e142f7ad930.
Includes frozen executable/hash, copied harness, environment, result, raw counter
snapshots and file timestamp. Temporary artifacts are not guaranteed durable.
No runtime/configuration changes were made for this census.

## Implementation direction

Support compiled normal execution with a cold exception exit that resumes the
authoritative interpreter handler machinery. This is an architectural plan,
not an implemented or measured speed path. Eligibility must depend on bytecode,
verified frame state and configured resource bounds, never invocation counts,
first receiver, observed failure history or cached VM execution state.

Before enabling a method, establish these contracts:

1. Derive protected instruction ranges and normal/exceptional CFG edges from
   the original Code. Preserve ordered catch selection and inclusive start /
   exclusive end PCs. Do not treat clearing Code.exceptions as implementation.
2. Carry the exact throwing instruction PC and current typed locals to the cold
   exit. The handler operand stack contains only the exception. Preserve updated
   wide slots, reference roots and pending-exception identity through allocation.
3. Check pending faults after every admitted potentially throwing operation in
   protected regions. No later bytecode side effect may execute after a fault.
   Existing direct early returns (athrow, checkcast, String null branches) must
   reach the same exception exit rather than bypassing the local handler.
4. A failed callee is caught at the caller's invocation PC. A shared global
   last-fault PC is insufficient for nested compiled calls. Splices need their
   own exception context; until that exists, refuse protected inlining rather
   than returning out of the whole caller past its handler.
5. Handler code may initially execute in the interpreter, but handler locals,
   rethrow, finally, nested ranges and continuation must all match normal VM
   execution. An interpreter-only method is not a successful JIT coverage test.
6. Worker region aborts remain control events, never guest exceptions consumed
   by catch(Throwable). Preserve compiler resource bounds and root protocols.

Begin with the exception-exit/frame-state contract and verifier/CFG tests, then
connect fallible emission to it. Keep new modules below 500 lines and reuse the
existing exception matcher, frame reconstruction and fault latch where their
contracts fit; do not create a second exception semantics implementation.

## Validation and acceptance

PendingExceptionSites stages boxed owned sites at stable addresses. Abandoning
the batch releases its pins. publish consumes the batch and retains the sites
for the native code's actual lifetime. Verified against the locked Cranelift
0.132.1 source: finalized function pointers remain valid until explicit unsafe
JITModule::free_memory, even after JITModule drop. No RAVA interpreter source
calls that operation. Consequently committed sites are retained for process
lifetime, matching existing native code, rather than freed on compiler-thread
exit or cache replacement. Future executable reclamation must reclaim both.

This staging API must be committed after successful finalization and before
exposing a callable entry; raw addresses from abandoned batches are invalid.
The production compiler does not call it yet. Two tests verify aborted-batch
cleanup and a generated native thunk containing a site address. The latter
invokes a test-only int-return bridge after vector growth and JITModule drop,
and checks handler results and cleared faults for five integer inputs. It does
not compile a handler-bearing Java method or implement the general exit ABI.
The complete `jit::` unit suite passed all 172 tests with next-gen-gc. The selected
ArenaMemoryProvider's Drop likewise explicitly retains finalized segments; this
was checked in the installed dependency source as well as through native execution.
No release rebuild or new wall measurement accompanies this unconnected stage.

OwnedExceptionSites now computes the local-type map once from an owned class's
actual method descriptor and static flag. It creates OwnedExceptionSite records
containing a class Arc, method index, fault PC and copied local-type row. No
borrowed Code is extended to a fictitious static lifetime. Borrowed site views
now reference the type table directly; only owned records copy a row.

An owned record creates an activation with its own class pin and refuses an
activation from another class or method before transferring pending state.
Records are Send + Sync without adding unsafe implementations. They must still
be kept at stable addresses for the entire lifetime of reachable machine code,
including superseded raw entry pointers. Storing them only in an evictable JitFn
or cache entry is insufficient. That machine-code ownership/publication is not
connected yet; no native callback is enabled by this increment.

All 170 JIT unit tests passed with next-gen-gc, including five ownership tests:
class/Code retention and eventual release, static/instance/wide argument slots,
wrong class/method refusal, invalid member/PC refusal, and owned sites surviving
their shared compile analysis. The last validated release remains SHA-256
659A37D667531B6B0CE7C4F7C6AFC0E349FBAE8D9AB15DE49991D8E6CEF194C6;
it was not rebuilt for this unconnected metadata increment.

For native return emission, use the existing normal-return ABI protocol:
memory-locals entries return i64 cells; register entries convert via
abi_ret_from_cell after emit_deep_epilogue. Do not copy the deopt return snippet
as a universal return encoder. Pending errors and unfinished exits remain
separate from successfully returned cells.

ExceptionSite::resume_pending now joins reconstruction, pending transfer, fault
latch clearing and rooted handler execution. It refuses detached worker context,
trampoline mode, absent pending exceptions and invalid reconstruction before
mutating the frame or fault state. Its Rust result preserves RunExit and VmError;
the caller keeps ownership of the frame, so this layer does not discard a
continuation or encode an unfinished exit as zero. Native ABI/emission remains
unconnected. The ordinary recursive invoke already clears trampoline before JIT
entry, but an eventual native adapter must still represent unexpected exits.

Validation: all 165 JIT unit tests passed with next-gen-gc. The eight state tests
now include pending/latch preservation on refusal, successful fault clearing,
same-object rethrow and detached-context refusal. This is a direct context-guard
test, not a completed worker-region replay or emitted native exception exit.
The native adapter must republish both the pending guest exception and native
fault latch when a returned Throw escapes the handler; the Rust transition
returns that error to its caller and deliberately does not invent an ABI value.

`jit::exception_state` now provides a compile-time ExceptionFrameMap and borrowed
ExceptionSite descriptors. Each site is tied to its live Code and exact protected
instruction PC. Unknown, unreachable, unprotected and operand-byte states are
refused. Reconstruction checks frame identity and exact cell count before
modifying the frame, decodes primitive bit patterns and tagged references through
the existing ABI, and restores Unset slots as Null instead of reading arbitrary
unused words as pointers. It leaves the pending exception and fault latch to
the eventual exit adapter. The input cells must remain rooted through restoration;
the restored frame must be registered before any subsequent guest collection.

This descriptor/restore API is not yet called by generated code. Its Code borrow
does not itself pin class-loader storage for a published machine-code callback.
That ownership, fault-latch transition, continuation handling and worker exclusion
remain obligations of the upcoming emission/ABI integration. No additional boot
speed claim or release promotion follows from this foundation.

Validation: all 162 JIT unit tests passed with next-gen-gc, including five new
state tests. They cover wide and floating-point bit preservation, tagged object
and array cells, boxed values surviving box-table truncation, unused slots,
method/cell-count refusal, ambiguous/unprotected PCs and raw-cell restoration
followed by pending-exception transfer and interpreter handler execution. This
does not yet prove emitted spill maps, moving-GC safety or a native callback.

The local-type analysis now validates exception-table boundaries and propagates
pre-instruction locals into every covering handler, including backward and
nested handler edges. It conservatively includes nonthrowing instructions and
shadowed catches; this may refuse otherwise valid reconstructions, but never
selects a type from observed exceptions. Incompatible incoming types merge to
Unknown. The existing eight-sweep convergence refusal remains; table scanning
is additionally bounded to 1048576 instruction/handler checks per sweep.
Truncated instructions are refused before constructing boundary maps.

This local map does not verify operand stacks, uninitialized-object rules or
catch-class assignability. It is insufficient by itself to admit handler-bearing
methods; generated-code integration remains pending.

Validation of this increment: all 147 `jit::` unit tests passed with next-gen-gc,
including ten local-type tests. The wider run also exposed an old array-store
test still expecting an index-only bounds message; its exact expected message
was aligned with the already shipped index-and-length correction. No runtime
array behavior was changed. The new tests cover exceptional-only reachability,
pre-store states, incompatible joins, backward/nested/shadowed handlers, invalid
boundaries, truncated instructions, ordinary branch joins and the work bound.

The next emitter integration must also handle normal-CFG reachability:
`emit/misc.rs::emit_blocks` currently refuses unreachable bytes after a terminal
instruction and leaders without a known stack shape. Exception-only handlers
must not be fed into it with an invented empty stack. Integer division currently
sets a pending exception and rejoins normal emission; protected operations need
an immediate exception exit before later local writes can change the restored
state. These are remaining implementation requirements, not enabled behavior.

The first implementation increment adds `opcodes::exception_entry::resume_exception`.
It accepts an already reconstructed Frame and a VmError, roots the exception in
the frame, uses the existing ordered matcher at the supplied fault PC, and runs
the selected handler through the interpreter. Non-Throw errors pass through;
all interpreter RunExit variants remain distinguishable. It does not touch JIT
cache state, consume the pending latch, or replay the throwing instruction.

The helper is not called by generated code yet. Admission still rejects all
handler-bearing methods. Typed cell decoding, verified exceptional frame states,
fault-latch ownership, protected emission and worker eligibility remain required
before enabling the feature. Tests of reconstructed Value locals are not evidence
of correct machine-code spill maps or a moving collection. No release binary or
boot performance result is changed by this increment.

Validation: `cargo test -p rava-interp --lib --features next-gen-gc opcodes::`
passed all 23 tests, including eight new transition tests. They cover ordered
typed catches, type mismatch, inclusive start/exclusive end, stale operand-stack
discard, updated primitive/wide/reference locals, same-object propagation,
rethrow into an enclosing handler, void return, interpreter errors, RegionAbort
and balanced frame-root registration. Moving GC and compiled execution remain
unvalidated for this helper.

Differential probes must prove actual compiled normal-path executions and cover
typed and catch-all handlers, ordering/overlap, mutated primitive/wide/reference
locals, callee throws, array/field/division faults, explicit null throw, rethrow,
finally, and effects immediately after a fault. Include moving-GC and nested
compiled-call evidence when those paths become admitted. Unsupported frame
states must refuse compilation before execution.

Run the complete semantic suite and product hashes before promotion. Price the
expanded JIT path with frozen hashes, null-first interleaved fresh-world boots.
Green correctness tests or larger compiled counts are not performance evidence.
The final target remains wall <5 s, no-adapt, no snapshot; this plan narrows none
of those requirements, and completion is currently unproven.
