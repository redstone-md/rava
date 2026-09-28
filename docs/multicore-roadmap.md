# Roadmap: multi-core execution

> **Status 2026-07-21: every P0 soundness gap (GAP-S01…S09) is CLOSED** with
> executable proofs — see [`runtime-gap-register.md`](runtime-gap-register.md).
> The fork/offload admission machinery is now proof-only (exact dispatch
> batches, dispatch-closure tree proofs, typed native contracts, hierarchy-epoch
> freeze, a typed worker boundary). What remains is the **P1 architecture arc**
> (GAP-A01…A05): the guest chunk pool still runs green-threaded behind the GIL,
> so provably-independent work does not yet reach real OS cores.
>
> **The completion-bar reality is documented and authoritative** (register,
> "Completion-bar reality"): the JVM wall/RSS targets sit below RAVA's proven
> floor under the current Cranelift backend + object model (Cranelift-vs-C2
> codegen quality is 67.9% of wall; multicore is Amdahl-capped at a 3–5×
> realistic floor). Closing the bar is an **architectural decision** (a
> C2-class backend or a full GIL-free rearchitecture), not bounded A/B tuning.
> A self-contained continuation prompt is in
> [`handoff-notes.md`](handoff-notes.md); the S09 worker-boundary
> design record is [`worker-context-plan.md`](worker-context-plan.md); the
> refuted interface-closure design is
> [`unloadable-iface-closure-plan.md`](unloadable-iface-closure-plan.md).

## Goal

Remove the global GIL from provably independent guest work without weakening JVM
semantics. A coarse `Mutex<Heap>` is not an intermediate target: it preserves
serialization and adds lock traffic on every heap access.

## Non-negotiable no-adapt rule

Parallel eligibility is derived only from program structure and runtime
capabilities:

- bytecode, CFG and descriptor shape;
- effect summaries and resolved call-graph closure;
- ownership, allocation-region and GC-root proofs;
- linked executor/task capabilities.

Class, method or package identity, workload names, body hashes, expected outputs,
checksums and fixture-specific allowlists are forbidden as eligibility inputs.
Missing or invalid proof always selects the original serial path. Diagnostic
filters such as `RAVA_FORK_ONLY` must never participate in production
eligibility.

## Current substrate

- OS execution threads already carry per-thread interpreter frames, JIT cells,
  box tables, native args, recursion/native depth and safepoint countdowns.
- GIL-free regions have a stop-the-world handshake, isolated worker TLAB views,
  scoped `RavaPool` workers and an abort-to-serial protocol.
- The generic JIT associative-reduction path is the production-near tracer:
  structural loop/effect analysis emits a detached `ReduceBody`, workers run it
  without `Vm`, and any failed guard takes the unchanged serial compiled loop.
- Generic guest bytecode and shared heap mutation still require the GIL. Existing
  iterator/COW and workload integration experiments are not evidence that the
  shared heap is generally concurrent.

## P0 soundness status (S01–S09 ALL CLOSED; A-gaps remain)

GAP-S01: the iterator fork resolves every
receiver's exact runtime dispatch target under the GIL (`fork_batch_target`) and
admits only a batch whose distinct classes all select one identical bytecode
method, each class independently passing the offload and whole-tree-compiled
proofs; heterogeneous, unresolved, native, abstract and differing targets stay
serial. GAP-S03: name allowlists for natives are replaced by typed declarative
contracts keyed by the exact receiver class and mechanically checked (bytecode
resolves first and is always analysed; uncontracted natives reject; worker
contexts also reject allocating/callback contracts). GAP-S04: the optimistic
proof bypass is removed — every admission runs the unconditional proofs.
GAP-S05: the abort-history blacklist is removed — an abort affects only its own
invocation. GAP-S06: shared-receiver virtual/interface callees are admitted
only through the exact dispatch closure over the loaded hierarchy
(`virtual_dispatch_closure`), valid per loader epoch, with consumption-boundary
epoch re-checks; interface owners fail closed.

GAP-S02 is closed on the same substrate: the fork call-tree proofs
(`method_fork_safe`, `fork_tree_all_compiled`, both allocation detectors) range
over the dispatch closure per virtual edge, fail closed on unenumerable
closures and over-approximate allocation. GAP-S07 is closed by three layers:
every fork worker runs with the gil-free dispatch guard armed, a gil-free
thread's class-load miss aborts the region (loader/`jit_cache` entries are
append-only, so pinned metadata and code cannot dangle), and the loader
hierarchy epoch is re-checked after join — a moved epoch discards the region
for an unchanged serial retry. GAP-S08 is closed: one flat class namespace by
construction, published name→class bindings are never replaced, and no
admission proof runs `<clinit>`.

GAP-S09 is closed by the five-stage typed `WorkerContext` arc
(`docs/worker-context-plan.md`, now a historical design record): fork workers
hold a typed `WorkerVmRef`/RAII `WorkerContext` instead of `*mut Vm`,
read-only callbacks route through `jit::vm_shared()`, every worker gets a
private fault latch, the worker-illegal shared-mutation callbacks are abort
stubs, plain `vm_ref()` is denied mode-aware on gil-free workers (sanctioned
`&mut` reaches are per-site documented), debug mutation guards enforce the
frozen-field contract, and instrumented acceptance tests pin
zero-resolution/compilation/dispatch-miss/stub-hit/denial regions. The
interpreter-on-worker experiments are QUARANTINED behind explicit default-off
env gates outside the S09 claim, and the `RAVA_FORK_COLUMN` column-fork
prototype is RETIRED (superseded by the S2 compiled-body fork).

With that, every P0 soundness gap (GAP-S01 through GAP-S09) is CLOSED; the
open work is the P1 architecture arc (GAP-A01..A05 — real multicore, the
capability-safe `Vm` boundary hygiene, transactional publication, shared-heap
concurrency, executor routing). The authoritative details and tests are in the
gap register.

## Completed execution-state brick

Pending JIT exceptions no longer live in shared `Vm` state:

- each OS thread owns a first-wins pending-exception slot;
- slot registration follows the same frame-thread lifecycle used by STW;
- both moving arena collectors forward these slots in place;
- mark-sweep clones them into its root set;
- a GIL-free worker fault requests region abort, cannot publish placeholder
  results, and is retried serially;
- a new `Vm` and every registered worker scope begin pending-free.

This removes one shared mutable execution field without adding a heap lock and
establishes the pattern for the remaining execution-only fields.

## Completed resident task-runtime brick

Executor and CompletableFuture work now has a per-VM resident host runtime:

- two capped workers sleep on a condition variable outside the GIL;
- queued and running records own task, future and preset-result values and are
  mutable roots for both moving collectors and mark-sweep;
- in-flight records are capped at 256; saturation rejects immediately to the
  existing inline serial path instead of growing an unbounded rooted queue;
- `get`/`join` release the GIL and wait on a monotonic completion epoch, avoiding
  polling and lost wakeups;
- completion captures guest throws in the future, fires callbacks under the GIL
  and notifies all waiters;
- executor factories attach an explicit resident-task capability; unknown
  executor-like objects fail closed to the legacy inline path;
- VM teardown releases the GIL, rejects queued work and joins every running
  worker before heap destruction.
- a task-scope RAII guard unregisters frames, clears worker TLS, releases the GIL
  and decrements the active-thread count even when guest execution or completion
  hits a host panic.

This is scheduling and ownership infrastructure, not a GIL-free bytecode claim.
Arbitrary guest tasks still acquire the GIL. `bench/AsyncFuture.java` proves
pending futures, exact results and overlap across blocking calls. Its `peak`
counter measures logical task overlap, not simultaneous CPU execution: a GIL
handoff inside a CPU loop can also report two active tasks. Multi-core acceptance
therefore still requires CPU/wall evidence from a structurally detached kernel.

## Validated task-graph tracer

`bench/AsyncReduce.java` closes the first Executor-to-detached-kernel path:

- two ordinary `CompletableFuture.supplyAsync` tasks enter the resident queue;
- each task invokes a previously warmed counted primitive reduction;
- the existing bytecode/CFG reduction proof lowers that loop to
  `RavaPool::parallel_reduce` while the resident worker remains the GIL-holding
  coordinator;
- benchmark identity and checksum are used only as acceptance oracles and never
  participate in runtime eligibility.

On the 2026-07-20 Windows acceptance host, five fresh release runs at one billion
iterations per Future produced the exact JVM result
`-13404515298688` twice. RAVA completed in 373--381 ms with process CPU/wall
13.85--14.42; the JVM reference completed in 1026 ms with CPU/wall 1.90. This is
real multi-core execution inside a standard Future task graph, but it does not
make arbitrary Supplier bytecode GIL-free: only the structurally proven inner
reduction detaches today.

## Completed whole-task tracer

The resident queue can now detach a complete zero-argument SAM wrapper when a
closed structural proof establishes all of the following:

- the wrapper only snapshots primitive receiver fields, invokes one static
  primitive kernel, invokes one unresolved native post-adapter and returns;
- the kernel is already compiled and initialized, has primitive-only arguments
  and result, and its decoded CFG contains no allocation, heap/static access,
  invokes, monitors, throws, exception handlers or division/remainder faults;
- the queue record remains the authoritative mutable GC root while the detached
  invocation owns only scalar cells, a live JIT entry pointer and adapter
  metadata.

Workers acquire the GIL to snapshot fields, release it for the proven JIT call,
then reacquire it for the native reference-producing adapter and Future
publication. Missing proof or failed materialization preserves the original
`vm.apply` path under the GIL. Eligibility contains no class, method, package,
fixture, hash, checksum or observed-result routing.

With nested JIT parallel reduction explicitly disabled, five fresh runs at one
billion iterations per Future produced the exact result above in 1377--1413 ms,
with 2703--2766 ms of process CPU and CPU/wall 1.94--1.98. This independently
proves that the two ordinary Future kernels overlap outside the global GIL. With
the separately proven inner reduction enabled, RAVA completed in 412 ms versus
1057 ms for the JVM reference on the same acceptance pass, with exact parity.

## Completed LambdaMetafactory expansion

`bench/AsyncLambdaReduce.java` extends the whole-task tracer from explicit SAM
classes to genuine capturing closures produced by `LambdaMetafactory`. Admission
still contains no workload identity gate. It accepts only a static method-handle
whose descriptor and bytecode prove all of the following:

- the zero-argument SAM contributes no runtime arguments, so every implementation
  argument is an exact primitive capture;
- local loads consume those captures once in descriptor/JVM-slot order;
- the implementation then performs exactly one static primitive-kernel call, one
  unresolved native reference-adapter call and `areturn`;
- the adapter result descriptor exactly equals the implementation result;
- the common initialized/JIT/effect proof for the primitive kernel closes.

Reference captures, `Null` primitive adaptation, reordered or widened loads,
non-static method handles, executable guest adapters and any incomplete proof all
retain the original GIL-held `vm.apply` path. Lambda owner/name/descriptor are used
only to resolve the linked implementation bytecode whose structure is proved.

With inner JIT parallelism disabled, five fresh two-billion-iteration runs produced
the exact JVM result `-44725839616768` twice, completed each Future pair in
2763--2884 ms and reached process CPU/wall 1.77--1.88. This proves two genuine
LambdaMetafactory Future kernels execute simultaneously outside the global GIL.
With the independently proven inner reduction enabled, three exact runs completed
the Future pair in 692--706 ms and the process in 1005--1017 ms, versus 2028 ms
for the JVM Future pair and 3032 ms for its process on the same acceptance pass.

## Completed captured-reference scalar snapshots

`bench/AsyncCapturedFieldReduce.java` proves that a reference capture does not by
itself require concurrent heap access. A LambdaMetafactory implementation may now
detach when each reference parameter is consumed exactly as
`aload <descriptor slot>; getfield <primitive>`, followed by the already-proven
primitive kernel, native adapter and `areturn` chain.

The queue record and lambda remain the mutable GC roots. Under the GIL the worker
re-reads the live lambda capture, resolves the exact non-static field identity and
copies only its primitive value into the detached JIT frame. No object handle or
arena pointer crosses the GIL boundary. A null/non-object capture, reference array,
wide/reordered load, reference-valued field or missing field proof fails admission
or materialization and retains the original serial `vm.apply` path.

Against the previous release binary with inner parallelism disabled, the
one-billion-iteration Future region fell from 2715 ms to 1383 ms (1.96x), while
CPU/wall changed from 0.87 to 1.34. Five fresh two-billion-iteration runs then
produced the exact JVM result twice with CPU/wall 1.85--1.88. In normal composed
mode, three exact runs completed the Future region in 691--709 ms and the process
in 1005--1010 ms, versus 2047 ms and 3031 ms for the JVM reference. A sampled
one-billion run used 10.8 MiB peak working set / 303.6 MiB peak private memory for
RAVA versus 42.0 / 617.4 MiB for the JVM.

## Completed multi-input capture proof

`bench/AsyncMultiFieldReduce.java` removes the one-capture/one-input restriction
without weakening admission. The LambdaMetafactory wrapper parser now reads the
actual sequence of scalar-producing bytecode expressions before the primitive
kernel call. Each expression must independently be either an exact primitive
capture load from its descriptor-assigned JVM local or an exact reference-capture
load followed by a resolved primitive `getfield`.

This admits repeated primitive captures, repeated reads from one reference
capture and bytecode-explicit input ordering. The target descriptor must still
equal the complete produced scalar-kind sequence. Unknown instructions, wrong
locals, reference fields, malformed pool entries and null/non-object field
receivers still fail closed to the original GIL-held task path. Capture values are
snapshotted under the GIL; only primitive cells enter detached execution.

With inner JIT parallelism disabled, five fresh one-billion-iteration runs
produced the exact result `-13403515298688` twice, completed the Future region in
1363--1453 ms and reached CPU/wall 1.94--1.98. In composed mode, three exact RAVA
runs completed the Future region in 367--368 ms and the process in 390--394 ms;
the JVM reference needed 1007--1015 ms and 1059--1066 ms respectively. At two
billion iterations the sampled RAVA process used 15.0 MiB peak working set /
342.9 MiB peak private memory, versus 41.0 / 617.4 MiB for the JVM, while
finishing in 749 ms versus 2169 ms.

## Completed nested capture-field proof

`bench/AsyncNestedFieldReduce.java` extends scalar snapshots through an arbitrary
non-empty chain of reference-valued instance fields ending in one primitive
field, such as `holder.config.trip`. Every `getfield` is resolved under the GIL;
the declared owner of the first field must exactly match the lambda capture
descriptor, and every following owner must exactly match the preceding reference
descriptor. This deliberately rejects inherited or otherwise ambiguous chains
until a wider subtype proof exists.

Materialization walks the resolved field-id chain under the GIL, checks that each
live receiver class exactly matches its proven owner and copies only the final
primitive into the JIT frame. A null/non-object intermediate, array descriptor,
static/missing field, owner mismatch, malformed bytecode or any non-`getfield`
operation retains the original serial guest path. No heap handle or arena pointer
crosses into detached execution.

Five fresh one-billion-iteration detached-only runs produced the exact result
`-13403515298688` twice, completed the Future region in 1442--1515 ms and reached
CPU/wall 1.96--1.99; a final owner-proof build reproduced 1.98. In composed mode,
three exact RAVA runs completed the Future region in 362--372 ms and the process
in 386--397 ms, versus 1060--1066 ms and 1110--1118 ms for the JVM reference. At
two billion iterations RAVA finished in 769 ms with 15.0 MiB peak working set /
342.8 MiB peak private memory; the JVM needed 2206 ms and 41.0 / 617.3 MiB.

## Completed cold primitive parallel compilation

`parallel_compile_policy` removes the hotness-threshold blind spot for one-shot
primitive work. On a method's first invocation, the VM now performs a cheap
back-edge screen and then admits only one of the existing exact bytecode/dataflow
proofs: a call-free associative primitive reduction or a call-free disjoint
primitive-array map. An admitted method enters the normal JIT immediately, so its
runtime `MIN_PARALLEL_TRIP` guard can fork the first large invocation instead of
interpreting it under the GIL hundreds of times before compilation.

The policy contains no class, method, package, workload, hash or fixture input.
Exception tables, unknown control flow, calls, shared stores, non-unit loop steps
and unproved aliasing reject admission. Constants are resolved and type-checked
under the GIL by the existing compiler. Compile/materialization failure retains
the original interpreter, `RAVA_NO_JIT_PARALLEL` retains the old hotness policy,
and a runtime trip below 10,000 executes the generated serial fallback without
opening a worker region.

On five fresh `ParallelNumeric 2000000` processes, the first and only fill was
bit-exact at `a0cc8d67d5194c21` and completed in 3--5 ms. The explicit parallel
opt-out took 829--909 ms because that cold call remained interpreted; HotSpot took
7.43--7.60 ms. Thus the first invocation now uses multiple cores and its proven
kernel is about 1.5--2.5x faster than the JVM, without warmup. A sampled whole
process used 25.7 MiB peak working set / 287.7 MiB peak private memory versus the
JVM's 55.8 / 617.4 MiB. The whole RAVA process was still slower (630 vs 433 ms)
because the subsequent order-dependent checksum remains interpreted; this
increment closes the cold map itself, not the remaining serial application work.

## Safe sequence

### Completed tracer: thread-owned detached Deep-IC roots

Split Deep-IC root-context lookup by execution mode. The normal GIL-held path
keeps its inline per-VM slot load with no callback. A COW-armed detached worker
instead obtains `JitThreadCtx*` from its own TLS before pushing a compiled frame,
so concurrent workers cannot cross-publish or mutate another thread's root
stack. The tracer does not widen detached-task eligibility or enable chunk-fill
gates.

A focused four-thread machine-code test forces every worker through the cold
root-stack growth path and proves that each Deep-IC prologue grows/restores its
own TLS context. GIL-held code retains the previous inline slot path.

The gated S2 worldgen acceptance is now bit-exact at both one and four workers.
The short height-8 differential produced `42d078f1708bd06d` in both modes; the
full 16,384-cell run produced `8790f04ace726ecf` in both modes, with zero shared
puts and 92,160 COW-shadowed puts. This closes the deterministic `3d7814`
cross-thread-root divergence. It does not make the experimental fork fast:
full-COW fill took 89.7 s at one worker and 130.4 s at four workers, so the
remaining COW callback/coordination tax must be removed by a separate proof.

### Completed tracer: exact-negative compiled COW reads

Profiling the corrected four-worker height-8 path showed that the COW map itself
was not the primary problem. Compiled code made 36,622,144 shadow-aware
`getfield` callbacks for only 61,008 hits; 36,732,016 lookups missed. Those get
callbacks consumed 47.0 seconds of summed worker CPU inside a 14.8-second fork.
A compact linear-map experiment retained exact semantics but made the full fork
slightly slower (132.1 seconds versus the 130.4-second baseline), so it was
discarded without widening policy.

The accepted tracer adds a 256-counter (1 KiB) process-wide counting Bloom table
over the exact `(object identity, field id)` keys currently present in all TLS
shadows. `ShadowFields` owns map and counter lifetime together: the first insert
increments one counter, clear/drop decrements every distinct live key, and an
update does neither. Compiled `getfield` hashes its already-materialized receiver
cell and resolved field id before entering Rust. A zero counter is an exact
negative across every worker and takes the pre-existing inline shared-read arm;
a collision or another worker's matching key only causes the old authoritative
TLS lookup. Thus the filter can create false positives but never bypass a live
shadow write. Panic/TLS destruction and concurrent collision accounting retain
balanced cleanup.

On the same four-worker height-8 acceptance, get callbacks fell to 2,587,602
(-92.9%), their summed CPU time fell to 1.23 seconds, and the fork fell from
14.83 to 2.84 seconds while retaining `42d078f1708bd06d`. The full 15,360-element
fork retained `8790f04ace726ecf`, all 92,160 writes remained isolated, and fork
time fell from 130.36 to 19.49 seconds (6.69x). It is also 4.60x faster than the
previous one-worker COW fork (89.69 seconds). The experimental chunk fork and all
of its admission gates remain default-off; the filter contains no class, method,
package, workload, hash, fixture or observed-result input.

The post-tracer scaling control is deliberately not claimed as a multicore win:
the new full one-worker fork took 18.34 seconds, so four workers at 19.49 seconds
were 6.3% slower. The profile localizes the next wall: one worker made 8.62
million conservative get callbacks, while four workers made 20.78 million
because active keys from other workers create safe cross-worker false positives.
This increment removes the former catastrophic COW lookup tax; the next tracer
must shard the exact-negative proof by execution worker without reviving the
failed compiled-ABI change or any owned-write bypass.

### Completed refinement: low-collision exact-negative COW reads

The same semantics-preserving counting Bloom table is now 4,096 counters
(16 KiB), with one authoritative mask shared by the Rust and generated-code
hash paths. This is still a process-wide summary of live TLS shadow keys: zero
remains an exact negative, while every collision takes the unchanged
authoritative TLS callback. The refinement changes neither detached admission
nor COW ownership and contains no workload, class, method, package, hash,
fixture or observed-result routing.

On the full four-worker S2 acceptance, conservative get callbacks fell from
20.78 million to 5.51 million, their summed callback CPU fell from 10.36 seconds
to 0.989 seconds, and the profiled fork fell from 19.49 to 16.28 seconds. The
one-worker control took 16.70 seconds, and both modes retained
`8790f04ace726ecf` with all 92,160 shadowed writes. A separate unprofiled run
measured 74.656 process-CPU seconds across 18.942 wall seconds (CPU/wall 3.941),
proving that the region genuinely occupied roughly four cores. The remaining
weak wall scaling is therefore per-core throughput, cache/memory pressure or
frequency contention rather than serialization by the global GIL.

### Completed tracer: worker-local JIT context and COW summary

Detached S2 workers now install a scoped `JitCtx` snapshot after the coordinator
has completed every object/array reserve. The existing second compiled ABI
parameter already propagates that context through machine-code calls; Rust
dispatch callbacks obtain the same pointer through a TLS override. Each snapshot
owns a private 4,096-counter COW Bloom table, so another worker's shadow can no
longer create a conservative lookup or bounce its counters. The RAII guard clears
any abort-left shadow before releasing the table and restores the prior context.
The native fault latch is also worker-local while the guard is installed, removing
the remaining concurrent write to the shared `JitCtx`; the ordinary GIL path
retains its original shared context.

Focused tests prove identical `(object, field)` keys remain isolated across live
workers, clear/drop balance each local table, and a worker fault never mutates the
shared latch. On the full four-worker S2 acceptance, get callbacks fell from
5.51 million to 4.73 million and their summed callback CPU fell from 0.989 to
0.566 seconds while preserving `8790f04ace726ecf`, all 92,160 shadowed writes and
zero shared puts. This tracer is an exact state-isolation improvement, not a wall
speedup claim: matched unprofiled P-core runs took 17.91 seconds at one worker and
18.81 seconds at four. P-core affinity did not improve the four-worker result, so
the next wall is the shared compiled density graph's per-core/cache/memory cost,
not GIL, scheduler placement, dispatch locks or COW lookup.

### Completed refinement: opt-in callback telemetry

The remaining 4.73 million compiled-to-Rust callbacks each performed an
unconditional relaxed atomic increment even when no callback report was
requested. Because all detached workers updated the same twelve process-wide
counters, this diagnostic bookkeeping introduced cross-core read-modify-write
traffic outside the previously measured COW callback timer. Callback counters
are now collected only when `RAVA_CALLBACK_STATS`, `RAVA_HEARTBEAT`, or
`RAVA_JIT_STATS` requests their consumer. Test-only per-thread tallies remain
unconditional, so no-callback assertions retain their original semantics.

On matched P-core, diagnostics-disabled full S2 runs, the four-worker parallel
region fell from 18.812 to 15.788 seconds (-16.1%). A fresh one-worker control
took 16.950 seconds, making four workers 6.9% faster (1.074x) inside the region;
the corresponding end-to-end fill times were 18.783 and 17.681 seconds. Both
runs retained `8790f04ace726ecf`, all 92,160 writes stayed in the COW shadow, and
zero shared puts occurred. This refinement changes telemetry cost only: it adds
no admission, effect, ownership, materialization, workload, class, method,
package, fixture, hash, or result-dependent runtime policy.

### Completed refinement: worker-local COW telemetry

The remaining always-on COW report still performed one process-wide relaxed
atomic increment for every shadow put and every shadow-read hit. The full S2
acceptance therefore made 580,224 contended telemetry RMWs after callback
accounting had already been made opt-in. Those three hot report fields now
accumulate in execution-thread-local counters and merge only at the outermost
`leave_region`; a four-worker run with no shared puts needs eight merge RMWs
instead of 580,224 per-event RMWs. `RAVA_FORK_TRACE` retains immediate global
event ordinals, and non-region callers retain the original global accounting.

A focused nested-region test proves that an inner exit neither publishes nor
resets its outer worker's counters and that the outer exit drains them exactly
once. The full four-worker acceptance retained `8790f04ace726ecf`, 92,160
shadow puts, 488,064 shadow-read hits and zero shared puts; its parallel region
took 17.024 seconds. Recent same-machine samples varied enough that this is not
claimed as an isolated wall-time win; the exact result is removal of 580,216
shared RMWs while retaining post-join cumulative telemetry. The batching changes
no bytecode/CFG admission, ownership, COW value, fallback, or workload policy.

Two field-hint isolation tracers were rejected before this refinement. Private
per-worker hint snapshots produced 15.607 then 18.718-second regions, while a
shared-read-only table took 17.407 seconds. Both retained exact output but lost
the cooperative benefit of worker publications and were fully reverted.

### Completed refinement: non-atomic private COW summaries

The 4,096-counter COW Bloom table inside each detached `WorkerJitState` was
already private to one execution thread, but inherited the process-wide table's
`AtomicU32` representation. A full four-worker acceptance therefore performed
92,160 atomic increments and 92,160 atomic decrements even though no other
thread could observe those counters. The private backing is now an address-stable
`UnsafeCell<[u32; 4096]>`; Rust callbacks and generated code use the same plain
worker-local cells, while the ordinary GIL context retains the original shared
atomic table unchanged. Saturating private increments make the impossible
bounded-heap overflow fail safe as a false positive rather than a missed COW
lookup.

Focused tests cover cross-worker isolation for identical keys, note/remove
balance, fresh zeroed storage across repeated guard lifetimes, and prove worker
operations never mutate the shared canonical table. Two interleaved matched
P-core samples measured 16.810/17.032-second one-worker regions and
16.422/15.857-second four-worker regions; the conservative upper medians were
17.032 and 16.422 seconds respectively, so four workers remained 3.6% faster
inside the region. Both four-worker runs retained `8790f04ace726ecf`, 92,160
shadow puts, 488,064 shadow-read hits and zero shared puts. Timing remains noisy,
so this refinement claims removal of 184,320 unnecessary private atomic RMWs,
not an isolated wall-time delta. It changes no admission, effect, ownership,
materialization, fallback, workload, class, method, package, fixture, hash or
result-dependent policy.

### Rejected follow-up COW tracers

Two stronger Bloom proofs were measured and fully reverted. A second counting
probe reduced profiled exact misses from 5,018,825 to 3,988,626 but added hash,
load and branch work to every query; its unprofiled four-worker region took
17.637 seconds. A singleton XOR witness reached essentially the same
3,988,434 misses and took 17.473 seconds. Both retained
`8790f04ace726ecf`, but neither paid for its extra generated instructions.
Further Bloom complexity is therefore not the next wall.

A worker-range-owned direct-write plus reverse undo tracer also retained the
exact hash, replaced all 92,160 shadow writes and 488,064 shadow-read hits with
92,160 undo entries, and produced final batched samples of 16.107 and 15.976
seconds. It was still reverted: range membership proves allocation provenance,
not non-escape, and a mixed interpreter shadow followed by a compiled direct
write can violate program order. Reference-valued undo entries also need an
executable guarantee that no collector can park/move the target before rollback
or must participate in the precise root set. A future direct-write path must
therefore prove exact shadow absence, unique non-escaping ownership, transaction
boundaries and GC visibility; any missing proof keeps the current shadow path.

### Completed refinement: opt-in direct/PIC telemetry

The dispatch audit found another set of always-on diagnostic writes after the
callback and COW counters had been gated or batched. Every successful Rust-side
direct compiled call incremented the process-wide `DIRECT_HITS` cache line;
direct misses did the same for one of three reason counters. Heap-PIC and
monomorphic-direct cache fill tallies were also updated even though their only
consumer is the opt-in PIC report. Megamorphic misses additionally queried the
process environment on every event merely to decide whether to print a trace.

Direct-call totals now update only when callback statistics, heartbeat, JIT
statistics or the megamorphic trace consumes them. Heap-PIC and monomorphic fill
totals update only under the existing PIC probe, and the megamorphic trace gate
is cached once. The cache entries, target pointers, resolve locks, freeze table,
publication atomics and all dispatch/fallback choices are unchanged. This is
therefore telemetry removal only: it adds no bytecode/CFG admission, ownership,
materialization, workload, class, method, package, fixture, hash or result-based
runtime policy.

Two matched diagnostics-disabled P-core samples retained
`8790f04ace726ecf`. One-worker regions took 18.696/18.771 seconds with
949.8-952.4 MiB peak RSS; four-worker regions took 16.240/18.553 seconds with
1029.1-1030.4 MiB peak RSS. The two-sample midpoint was 18.734 versus 17.397
seconds (four workers 7.1% faster), but the four-worker spread is too wide for an
isolated wall-time claim. The exact accepted result is that idle direct/PIC
telemetry no longer generates shared RMW traffic while diagnostic runs retain
their original cumulative counters.

### Completed refinement: opt-in worker progress/allocation telemetry

The next worker-loop audit found two more diagnostic writes in every S2 element.
`tlab_try_alloc` incremented the process-wide `ARENA_ALLOCS` counter for each
fresh coordinate context even though that total is diagnostic only.
Independently, every worker stored its current element index
into an adjacent `AtomicUsize`; those cache-line-sharing slots are read only by
the five-second `RAVA_FORK_TRACE` liveness watcher. The watcher completion
counter was likewise updated even when no watcher existed.

Arena allocation totals now accumulate in execution-thread-local storage inside
a region and merge once at each worker's outermost exit; non-region allocation
and post-join totals retain their original exact process-wide accounting.
Worker progress storage, its vector allocation and completion RMWs now exist
only while fork tracing is requested. On the canonical 15,360-element parallel
slice this replaces 15,360 shared allocation-counter RMWs with four merges and
removes 15,360 false-sharing progress stores plus four cold completion RMWs.
Diagnostic-on runs retain exact per-element progress and post-join allocation
totals. Allocation,
safepoints, abort polling, result publication, TLAB ranges and all serial
fallbacks are unchanged; no identity or observed workload property participates
in the gates.

A diagnostics-disabled final-batching confirmation retained
`8790f04ace726ecf`, all 92,160 shadow writes, all 488,064 shadow-read hits and
zero shared writes. Its region took 18.108 seconds with 974.5 MiB peak RSS,
inside the immediately preceding build's 16.240-18.553-second noise range.
The accepted result is therefore the exact removal of 30,720 hot shared
telemetry writes without a parity or memory regression, not an isolated
wall-time claim.

### Completed refinement: chunked parallel-array slot claims

The remaining primitive-array worker allocator performs one shared
`parr_next.fetch_add(1)` for every array. The current tracer is a fixed-size,
worker-local claim cursor keyed by heap address plus a monotonically changing
region epoch. A worker acquires a small disjoint slot range with one atomic RMW
and serves subsequent allocations locally. Exhaustion keeps the existing abort
and serial retry; neither chunk size nor any runtime decision depends on class,
method, package, workload, fixture, hash or observed timing.

Partial chunks cannot be discarded by truncating only a high-water tail:
workers may finish out of order and returned handles cannot be compacted. The
coordinator must therefore close the region only after the scoped join, count
exact live slots, truncate a completely unused suffix, clear every interior
placeholder descriptor and put each unpublished hole on `free_arr` exactly
once. Focused acceptance covers disjoint concurrent claims, one RMW serving
multiple allocations, epoch/heap isolation, exhaustion, stable live handles,
hole reuse and duplicate-free reclamation. If canonical ChunkFill does not
execute this allocator, the change receives only structural/focused evidence
and no worldgen wall-time claim.

Five focused release tests pass. They prove that three allocations advance the
shared cursor by one eight-slot claim, four concurrent threads receive 20
distinct stable handles, a new epoch rejects stale TLS, switching between two
heaps cannot share a cursor, exhaustion still returns `None`, and partial-range
holes remain duplicate-free through GC before later serial reuse. The canonical
four-worker ChunkFill retained `8790f04ace726ecf` and reported
`reserved=983040 claimed_span=0 arrays=0`: S2 reserves defensively because its
bytecode tree contains `newarray`, but this run allocated no primitive arrays in
the region. Its traced fill took 30.049 seconds (trace enabled). Therefore this
increment makes only the exact generic allocator claim-reduction assertion; it
does not claim a ChunkFill or worldgen speedup.

Independent review caught a stale-TLS edge before acceptance: after commit, an
old local range could otherwise survive until a direct allocator call with no
active region. The final helper rejects inactive regions first, validates the
cached range against the current `[base, cap)` and epoch, and uses a bounded CAS
claim that stops exactly at `cap`; repeated exhaustion neither advances nor can
wrap the shared cursor. The focused regression covers both repeated exhaustion
and a post-commit call returning `None`.

### Completed RAM refinement: exact parallel-array spine reservation

ChunkFill currently opens a 983,040-slot array region before its scoped fork.
Even when no worker allocates an array, repeated `push` growth may geometrically
over-allocate the `arrays`, `arr_alive` and `array_descs` spines; commit truncates
their lengths but deliberately retains capacity. The tracer records those
three capacities before and after reserve, then preflights the already-proven
slot count with `reserve_exact`. This changes allocator capacity only: bytecode
admission, handles, region bounds, worker claims, abort and serial retry remain
identical, with no class/method/workload/result-dependent choice.

The matched trace measured all three capacities growing from 2,097,152 to
4,194,304 slots under geometric push growth even though the required final
length was 2,648,102. `ArrayObj + bool + ArrayDesc` is 65 bytes per slot. After
the exact preflight, every spine stopped at 2,648,102, removing 1,546,202 excess
slots or 100,503,130 bytes (95.8 MiB) of retained capacity. Both runs retained
`8790f04ace726ecf`, zero worker array allocations and identical COW totals. Their
trace-enabled regions took 30.045 and 30.016 seconds respectively; the accepted
result is the exact capacity reduction, not a wall-time claim.

### Rejected tracer: deterministic hashing for authoritative COW shadows

The exact worker-local `ShadowFields` map still uses the standard randomized
SipHash builder for trusted `(object identity, field id)` integer keys. The
runtime already centralizes this case in `FastMap`/`FxHasher`. Reusing that
authoritative map type changes neither Bloom negatives, exact key equality,
shadow lifetime, callback admission nor collision correctness. Acceptance is a
matched `RAVA_COW_PROFILE` comparison of callback count/time plus canonical hash;
timing that does not clear noise is reverted.

Two interleaved profiled FastMap samples reduced summed get-callback time from
SipHash's 1.764/1.387 seconds to 1.430/1.148 seconds while all 4,730,333 calls,
4,242,269 misses, 488,064 hits, 92,160 puts and the canonical hash stayed exact.
The diagnostics-off gate nevertheless regressed from a 26.911-second SipHash
region to 42.048 seconds. FxHasher also weakens crafted-collision resistance for
guest-influenced integer keys. The two-line container substitution was fully
reverted: the profiled local win does not justify the production regression or
the CPU-DoS trade-off.

### Completed RAM refinement: exact lambda-capture GC accounting

The heap-pressure counter charged every lambda allocation for its captured
`Value` buffer, but object sweep subtracted only the base object and field
bytes. Dead capture buffers were therefore freed by Rust while their estimated
bytes remained permanently charged to `Heap::bytes`, causing progressively
earlier full collections. Sweep now subtracts the same capture estimate used by
allocation. `Object.clone()` also charges the deep-cloned capture buffer, so
allocation, cloning and reclamation remain symmetric. The decision depends only
on the actual lambda representation and capture length; GC admission and all
roots are unchanged.

A focused release regression allocates and clones a three-capture lambda, sweeps
both instances, and proves that the reclaimed total exactly returns the heap
budget to zero. Two fresh P-core `ChunkFill 0 0` confirmations measured 86.576
seconds / 836.9 MiB and 86.853 seconds / 838.4 MiB versus the immediately
preceding 86.901-second / 927.8-MiB baseline. The canonical `ChunkFill 1 64`
completed in 105.062 seconds with 981.4 MiB peak RSS and retained
`8790f04ace726ecf` (`SETUP_MS=86163`, `FILL_MS=18413`). The accepted memory
claim is therefore a repeatable roughly 90 MiB setup-peak reduction, not a
wall-time claim.

### Completed RAM refinement: symmetric native-map entry accounting

The same heap-pressure audit found that `map_put` charged every new native map
entry while `map_remove` and `map_clear` discarded entries without subtracting
their estimates. A later sweep could account only for entries still present, so
removed key/value estimates accumulated permanently. `MapKey::entry_bytes` is
now the single authoritative formula used by put, remove, clear and sweep.
Replacement and insertion-order moves remain allocation-neutral.

A focused release regression covers both removal and clear, then sweeps the
empty map and proves that the heap budget returns exactly to zero. A fresh
P-core `ChunkFill 0 0` run completed in 84.842 seconds with 837.5 MiB peak RSS,
inside the accepted lambda-accounting memory range. Canonical `ChunkFill 1 64`
completed in 103.401 seconds / 979.5 MiB and retained
`8790f04ace726ecf` (`SETUP_MS=84705`, `FILL_MS=18214`). This change therefore
claims exact long-lived map-churn accounting with no boot regression, not an
isolated wall-time or RSS improvement.

### Completed RAM refinement: account boxed side-table entries before compaction

`field_box` and `array_box` append a full `Value` whenever a flat field or
reference-array cell cannot encode its payload directly. GC compaction already
subtracted 24 bytes for every discarded side-table entry, but insertion never
charged those bytes. Churn could therefore drive `Heap::bytes` below the live
heap through saturating subtraction and delay later collections. Both encoding
choke points now charge the same `SLOT_BYTES` estimate that compaction removes.
Direct handles, arena pointers and primitives remain unboxed and unchanged.

A focused release regression creates one field-box and one array-box entry,
overwrites both cells, proves that live-root compaction removes exactly the two
entry estimates, then proves that the final container sweep returns the budget
to zero. Two fresh P-core `ChunkFill 0 0` runs measured 87.042 seconds / 820.8
MiB and 86.805 seconds / 822.1 MiB, preserving the preceding setup-time range
while lowering its peak by about 16 MiB. Canonical `ChunkFill 1 64` completed in
107.524 seconds / 914.1 MiB with `8790f04ace726ecf`
(`SETUP_MS=87037`, `FILL_MS=19996`). The canonical peak is 65.4 MiB below the
immediately preceding map-accounting run; its single-run timing is not treated
as a speed claim.

### Rejected RAM tracers: setup caches and post-GC capacity trimming

A generic datapack prepass tokenized 6,351 JSON resources on owned Rust strings
before serial guest materialization. It was routed only by resource extension
and content, not by package, class, method or workload identity. The prepass
itself took 24 ms, but the measured setup path consumed none of its cached
trees (`cache_hits=0`, `inline_misses=1`) and peak RSS increased from 1,030.9 to
1,060.1 MiB. The implementation was fully reverted.

Reference-array trimming after mark-sweep was also tested behind a diagnostic
gate. Matched P-core `ChunkFill 0 0` runs measured 88.586 seconds / 927.5 MiB
with trimming disabled and 87.122 seconds / 932.6 MiB with it enabled. The
allocator did not return enough pages to reduce process peak RSS, so this gate
remains disabled and makes no production claim.

An exact, collision-checked weak cache for multi-capture lambdas reused only
entries with the same invokedynamic site and bit-identical supported captures.
It remained non-root and was pruned after sweeping GC, but its persistent
fingerprint table cost more than the shared capture buffers saved. Matched
P-core runs were 88.291 seconds / 931.0 MiB with the cache disabled and 88.292
seconds / 1,003.0 MiB with it enabled. The cache and its tests were fully
reverted; a persistent canonicalization index must not be recreated from this
design.

Finally, a stop-the-world tracer called `shrink_to_fit` on the object, array and
map free-handle stacks only when capacity exceeded both twice the live length
and a 4,096-handle floor. This preserved handle contents and LIFO reuse, and a
focused release test passed. Nevertheless, matched P-core `ChunkFill 0 0`
measured 86.901 seconds / 927.8 MiB with trimming disabled versus 87.460
seconds / 937.4 MiB enabled. The expected capacity slack did not lower peak
RSS and added allocator churn, so the implementation was fully reverted.

### Completed dispatch refinement: flatten synthetic comparator trees

The coarse wall sampler on a fresh P-core canonical run attributed 66.7% of
sampled wall time to compiled guest code and 21.5% to native bridges. A
per-bridge self-time run then identified collection sorting as the largest
single bridge bucket: 3,291 `ArrayList.sort` calls consumed 11.70 seconds. A
structural shape probe found 2,024 six-clause `thenComparing` trees sorting
213,891 total elements (largest list 7,593).

Eagerly caching every clause key once per element was investigated first and
rejected. Both observed extractor bytecodes contain reference-array reads,
virtual/static calls and integer division; one also contains a backward loop.
They therefore fail the required no-throw/no-callback/no-allocation effect
proof. Precomputing them would change `thenComparing` short-circuiting, callback
count/order and exception timing. The tracer and caching implementation were
fully reverted rather than weakening the strict `no-adapt` fallback.

The accepted path instead flattens only immutable synthetic
`key`/`keyrev`/`then`/`reversed` comparator nodes into an ordered clause plan.
Every comparison still invokes the same key extractors in the same left/right
order, stops at the same first non-equal key, and propagates the same errors;
only recursive `$cmp`/`$first`/`$second` heap dispatch is removed. Unknown,
cyclic, over-depth or non-handle trees use the original serial comparator.
Admission also requires `native_depth > 0`, the existing executable proof that
moving GC cannot run across callbacks. The trigger uses only comparator
structure and the GC invariant, never class, method, package or workload
identity. `RAVA_FLAT_COMPARATOR=0` is the emergency kill switch.

Three interleaved P-core `ChunkFill 0 0` pairs on the same binary measured
OFF setup times of 83.807, 86.242 and 85.683 seconds versus ON times of 83.389,
84.068 and 82.926 seconds. Medians improved from 85.683 to 83.389 seconds
(2.294 seconds, 2.7%). Median peak RSS was effectively flat at 814.6 versus
815.6 MiB. The default-on canonical `ChunkFill 1 64` completed in 104.548
seconds / 839.1 MiB with `SETUP_MS=83663`, `FILL_MS=19943` and exact hash
`8790f04ace726ecf`. Structural tests cover nested/whole-tree reversal and cyclic
fallback; the full release suite remains the acceptance gate.

### Rejected RAM tracer: reuse eager-stream output arrays

The eager `map`, `filter`, `peek` and `flatMap` bridges currently accumulate
their results in a rooted synthetic `Object[]`, copy the values into a Rust
vector, then allocate the `Object[]` owned by the returned Stream. A structural
tracer attached the already-owned array directly. It did not inspect class,
method, package, workload or result identity, and retained the same rooted
callback execution and serial/GIL fallback.

Two matched P-core `ChunkFill 0 0` pairs consistently traded a small memory
reduction for a material setup regression. In OFF-to-ON order, wall time moved
from 89.540 to 93.320 seconds while peak RSS moved from 814.9 to 809.6 MiB. In
ON-to-OFF order, wall time moved from 94.202 to 91.831 seconds while peak RSS
moved from 806.3 to 816.6 MiB. Saving 5.3--10.3 MiB does not justify a repeated
2.4--3.8 second slowdown. The implementation, environment gate and focused
test were fully removed; future Stream work must target the underlying bridge
and allocation path rather than publishing this array directly.

### Rejected dispatch tracer: direct eager Stream mapping

The post-comparator bridge profile still attributed 7.61 seconds to 324
`Stream.collect` calls and about 10.7 seconds to the structurally aggregated
`Stream.map` entries. A gated one-to-one map path therefore read the synthetic
Stream's already-rooted backing array directly, pre-sized its rooted output and
published each mapper result without allocating a temporary Rust vector per
element. Callback count, order, exception propagation and eager semantics were
unchanged; non-synthetic sources used the generic path.

Three interleaved P-core `ChunkFill 0 0` comparisons all retained the expected
setup-only hash. OFF setup times were 109.302, 88.208 and 90.704 seconds; ON
times were 94.364, 93.415 and 90.009 seconds. The cold 109-second OFF outlier
made the first pair look strongly positive, but the three-run medians were
90.704 seconds OFF and 93.415 seconds ON: a 2.711-second (3.0%) regression.
Peak RSS fell only about 3 MiB. The implementation, gate and focused test were
fully removed. Avoiding eager-map copies without eliminating guest callback
and bridge execution is not a viable optimization on this workload.

### Completed dispatch refinement: call-scoped pure `toString` memoization

The argument-class probe for `String.valueOf` exposed repeated construction of
large recursive value strings. Its final setup snapshot observed 171,871
`Sum` values averaging 947 output bytes, 96,868 `Product` values averaging 503
bytes and 832 `RecursiveTypeFamily` values averaging 18,646 bytes. These names
are diagnostics only and never participate in admission.

The accepted path memoizes a receiver only within one outer nested
`value_to_string` call. Admission requires `native_depth > 0`, which prevents a
moving collection from recycling heap handles or arena addresses, plus the
existing transitive bytecode/CFG effect proof for the concrete `toString`
implementation. Unknown, unresolved, impure, shared-mutating or unproved call
trees execute unchanged. Successful results alone are cached; the cache is
bounded to 8,192 entries / 16 MiB and is destroyed when the outer call returns.
The policy contains no application class, method, package, workload, input,
hash or result routing. `RAVA_STRINGIFY_MEMO=0` is the emergency kill switch.

Three interleaved P-core `ChunkFill 0 0` pairs retained the expected setup-only
hash. OFF setup times were 88.150, 92.931 and 120.685 seconds versus ON times of
87.309, 90.667 and 92.350 seconds. Medians improved from 92.931 to 90.667
seconds (2.264 seconds, 2.4%). Median sampled peak RSS moved from 816.4 to 815.2
MiB. The 120-second cold OFF outlier is retained in the evidence rather than
discarded; the accepted claim rests on all three pair directions and medians.
The default-on canonical `ChunkFill 1 64` completed in 108.683 seconds / 839.2
MiB with `SETUP_MS=86706`, `FILL_MS=21001` and exact hash
`8790f04ace726ecf`.

### Completed dispatch refinement: snapshot immutable comparator lambdas

The flattened comparator plan still called `Heap::lambda_of` for every key
extraction. That accessor must return an owned `Lambda`, so each callback cloned
the immutable call-site metadata and capture vector before the authoritative
lambda invocation cloned the captures again into its argument vector. The plan
now snapshots a concrete lambda once per clause and invokes the exact existing
lambda body from that snapshot. Non-lambda functions retain `Vm::apply`.

The snapshot lifetime is one serial sort. Admission inherits the flat-plan
requirement that `native_depth > 0`, so GC cannot sweep handles or move arena
values across callbacks. Callback count/order, capture/SAM argument order,
guest bytecode, allocations, exception timing and fallback behavior are
unchanged. Selection depends only on the already-recognized synthetic comparator
tree and concrete lambda representation; it contains no class, method, package,
workload, input, hash or result routing. `RAVA_COMPARATOR_LAMBDA_SNAPSHOT=0` is
the emergency kill switch.

Three interleaved P-core `ChunkFill 0 0` pairs retained the expected setup-only
result. OFF setup times were 84.559, 86.280 and 86.471 seconds versus ON times
of 86.505, 83.609 and 84.883 seconds. Medians improved from 86.280 to 84.883
seconds (1.397 seconds, 1.6%); two of three individual pairs improved. Median
peak RSS moved from 816.1 to 816.7 MiB, inside measurement noise. The default-on
canonical `ChunkFill 1 64` completed in 107.464 seconds / 838.6 MiB with
`SETUP_MS=85168`, `FILL_MS=21064` and exact hash `8790f04ace726ecf`. The full
required release suite passed with 291 interpreter tests passed, zero failed and
two ignored; all other workspace tests and doc-tests also passed.

### Rejected dispatch tracer: clone-free lambda existence checks

`Vm::apply` currently calls `Heap::lambda_of(handle).is_some()` before routing a
concrete lambda to `invoke_lambda`, cloning its immutable metadata and capture
vector only to test representation. A tracer replaced that query with the
existing allocation-free `Heap::is_lambda` predicate; a kill switch restored the
old accessor. Both predicates inspect only the same heap representation and the
invocation/fallback paths were otherwise identical.

Three interleaved P-core `ChunkFill 0 0` pairs did not produce a repeatable wall
win. OFF setup times were 84.853, 84.632 and 82.534 seconds versus ON times of
85.914, 84.022 and 84.518 seconds. Medians moved only from 84.632 to 84.518
seconds (0.114 seconds, 0.13%), while ON won just one of the three individual
pairs. Median peak RSS moved from 816.8 to 816.2 MiB. The code and environment
gate were fully removed: avoiding this isolated clone is below current process
noise and does not justify another production branch.

### Completed RAM refinement: root native-map keys without guest arrays

The next-gen native-map dispatcher allocated a two-element guest `Object[]` for
every operation with a key. That array existed only to keep the key and optional
value visible while structural-key derivation could execute guest `toString()`
and trigger a moving collection. The new `MapKeyRoot` keeps the same two values
in `Vm::gc_protect`, the collector's existing precise mutable root stack, and
re-reads their forwarded slots after key derivation. Its RAII scope restores the
previous root depth on every return. The old heap-array carrier remains available
through `RAVA_MAP_KEY_GC_PROTECT=0`.

This changes only root storage. Map-key derivation, equality, lookup/update
ordering, callbacks, exceptions and returned values are unchanged. The decision
contains no class, method, package, workload, input, hash or result routing.
Focused release tests prove that both carriers expose identical key/value slots
and that nested `gc_protect` entries are removed without disturbing older roots.

Three interleaved P-core `ChunkFill 0 0` pairs retained the expected setup-only
result. OFF setup times were 82.164, 77.379 and 79.763 seconds versus ON times of
80.302, 78.312 and 81.530 seconds. Wall medians moved from 80.660 to 81.254
seconds, a 0.594-second (0.7%) difference inside the observed process noise; no
speed claim is made from these pairs. Peak RSS, however, fell in every run: the
median moved from 815.9 to 791.2 MiB, a repeatable 24.7-MiB reduction. The
default-on canonical `ChunkFill 1 64` completed in 103.609 seconds / 810.2 MiB
with `SETUP_MS=82277`, `FILL_MS=20441` and exact hash `8790f04ace726ecf`.
The full required release suite passed with 293 interpreter tests passed, zero
failed and two ignored; all other workspace tests and doc-tests also passed.

### Completed RAM refinement: release swept object storage

The handle table previously replaced every swept object with a newly allocated
empty `Box<Object>`. This preserved the numeric handle and pointer-table stride,
but 1,143,383 dead slots in the measured setup heap still owned about 52.3 MiB
of object structs plus allocator metadata. The table now stores a transparent
`ObjectSlot(Option<Box<Object>>)` and represents a swept hole as `None`. The
slot itself remains pointer-sized and handle indices never move; allocation
reconstructs a live box when the existing free list reuses the handle.

The Rust raw worker API now uses `*mut ObjectSlot` directly instead of casting
the table to `*mut Box<Object>`. This removes type-punning while retaining the
machine-code ABI: focused tests prove identical slot size/alignment, a live
slot word equal to the boxed `Object` pointer and a null dead-slot word. All
object-table accessors explicitly unwrap live storage, so diagnostic scans and
GC helpers skip null tombstones instead of relying on a panicking `Deref`.
Retirement is idempotent, invalidates the native descriptor and publishes the
handle to the free list exactly once. Snapshot serialization still emits the
legacy `Vec<Object>` sequence shape; liveness reconstructs nullable holes on
restore, preserving old snapshot compatibility and every handle index.

Admission is the existing GC liveness proof only: every unreachable live
handle is released at the same stop-the-world sweep point. There is no class,
method, package, workload, input, hash or result routing. Unknown reachability
continues to keep the object live. `RAVA_NULL_OBJECT_SLOTS=0` restores the
legacy empty-box representation.

Three interleaved P-core `ChunkFill 0 0` pairs all retained setup-only hash
`1`. OFF setup times were 82.798, 77.372 and 76.506 seconds versus ON times of
78.686, 76.939 and 76.747 seconds. Medians moved from 77.372 to 76.939 seconds,
a 0.433-second (0.6%) difference inside process noise; no speed claim is made.
Peak RSS fell in every pair: OFF values were 790.3, 791.4 and 791.5 MiB versus
776.0, 776.3 and 776.0 MiB ON, moving the median from 791.4 to 776.0 MiB, a
repeatable 15.4-MiB reduction. The default-on canonical `ChunkFill 1 64`
completed in 95.505 seconds / 799.6 MiB with `SETUP_MS=76094`, `FILL_MS=18487`
and exact hash `8790f04ace726ecf`. Focused release tests cover layout, null
lookup, one-time retirement, handle reuse, snapshot/restore and both existing
concurrent raw-slot/STW scenarios. The full required release suite passed with
298 interpreter tests passed, zero failed and two ignored; all other workspace
tests and doc-tests also passed.

The nullable-slot follow-up also closes every machine-code class-word consumer.
BitSet inline dispatch, the shadow heap-Ref PIC and the live heap-Ref PIC now use
one shared emitter which branches a null tombstone to the pre-existing slow/miss
arm before loading `Object.class`. This restores the legacy invalid-handle
fallback instead of risking an address-zero load. A focused release test emits
and executes the real Cranelift guard against an `ObjectSlot(None)` word. The
guard is structural for every heap reference and contains no class, method,
package, workload, input, hash or result routing.

### Completed RAM refinement: release unused heap-table capacity

Mark/sweep keeps numeric handles stable, so the outer object and array tables
cannot compact or truncate dead slots. They can still retain substantial unused
allocator capacity after growth. The collector now releases capacity, without
changing length or contents, from `objects`, `obj_descs`, `arrays`,
`array_descs`, `free_obj`, `free_arr` and `free_map`. Each table must have at
least 1 MiB of structural slack, preventing insignificant reallocations on
alternating allocate/collect cycles. Capacity savings are not added to GC's
`freed` return value because `Heap::bytes` does not authoritatively account
these outer allocations.

Relocation is admitted only outside a pool worker, with global parallel-region
count zero and no active parallel-array reservation. A missing exclusivity proof
therefore leaves every table untouched. After any eligible pass, the current
object, object-descriptor and array-descriptor bases are republished through
`JitCtx`. Focused release tests prove content and handle preservation, exact JIT
base publication, unique free-handle reuse, the 1-MiB anti-thrash floor and both
parallel-state refusals. The decision uses only table length/capacity and runtime
ownership state; it contains no class, method, package, workload, input, hash or
result routing. `RAVA_HEAP_TABLE_TRIM=0` restores retained capacities.

Two independent three-pair P-core `ChunkFill 0 0` series retained hash `1` in
all twelve processes. In the first series, OFF/ON setup medians were 83.542 and
80.664 seconds while peak-RSS medians were 775.0 and 772.3 MiB. The second
series recorded OFF setup times of 87.089, 80.227 and 81.090 seconds versus ON
times of 83.987, 85.127 and 81.318 seconds. Its pairwise ON-minus-OFF time deltas
were -3.102, +4.900 and +0.228 seconds: the sign reversal between pairs and
between series is process noise, so no speed claim is made. RSS fell in every
fully recorded pair: OFF values were 776.1, 775.8 and 775.9 MiB versus ON values
of 771.3, 770.9 and 770.9 MiB, moving the median from 775.9 to 770.9 MiB. The
repeatable RSS reduction, with no repeatable time regression, admitted the gate
default-on.

The fresh default-on canonical `ChunkFill 1 64` completed in 104.847 seconds /
792.9 MiB with `SETUP_MS=83347`, `FILL_MS=20583` and exact hash
`8790f04ace726ecf`. Its slower setup than the preceding canonical is not claimed
as a trim effect because the interleaved A/B timing did not reproduce a single
direction. The full required release suite passed with 302 interpreter tests
passed, zero failed and two ignored; all other workspace tests and doc-tests
also passed.

### Completed RAM refinement: intern array element types by numeric id

The post-trim census left the stable-handle `ArrayObj` spine as the largest
single Rust-side component: 1,544,784 slots at 48 bytes, or 70.9 MiB. Each slot
owned a 16-byte `Arc<str>` even though element names were canonical and shared.
The runtime slot now stores a heap-owned `u32` intern id and is exactly 40 bytes
on the x86-64 target, a structural reduction of about 11.8 MiB at the measured
capacity. Worker regions receive copied pre-interned ids and never mutate the
interner. Swept holes use an out-of-range sentinel; reuse installs a valid id at
the same handle. Missing or malformed ids fail to `None`/diagnostic fallback.

Snapshot boundaries still emit and consume the legacy string field in the same
record and sequence wire shape. Export uses a streaming serde sequence rather
than allocating a DTO vector proportional to the array spine; restore interns
only live slots and reconstructs the dead sentinel. Golden bincode tests cover
both record and sequence bytes. Further focused tests cover the exact layout,
canonical and distinct ids, empty/Unicode/NUL names, GC retirement and handle
reuse, parallel worker writes without interner growth, snapshot handle indices
and malformed ids.

Two independent three-pair P-core `ChunkFill 0 0` series retained hash `1` in
all twelve processes. The first series measured baseline setup times of 71.272,
75.196 and 75.522 seconds versus id-layout times of 73.063, 78.032 and 79.938
seconds; its pairwise id-minus-baseline deltas were +1.791, +2.836 and +4.416
seconds. The reversed independent series measured baseline times of 84.822,
86.577 and 77.149 seconds versus id-layout times of 84.601, 84.733 and 76.398
seconds; every pair changed sign at -0.221, -1.844 and -0.751 seconds. A trim-off
reversed pair likewise measured 83.812 seconds for the id layout and 84.588 for
baseline. The independent sign reversal prevents either a speedup or slowdown
claim.

Peak RSS fell in every default-on pair. First-series baseline values were 773.1,
771.6 and 772.0 MiB versus 747.0, 747.3 and 745.8 MiB; second-series baseline
values were 771.4, 771.1 and 771.6 MiB versus 743.6, 747.4 and 746.4 MiB. Both
series therefore moved the median by exactly 25.0 MiB. Second-series median
private memory also fell from 1,076.1 to 1,050.4 MiB. The process-level saving
exceeds the isolated slot-size delta because allocation size, paging and
fragmentation also changed; only the observed process counters are claimed.
The fresh canonical `ChunkFill 1 64` completed in 90.287 seconds / 769.3 MiB
with `SETUP_MS=71578`, `FILL_MS=17830` and exact hash `8790f04ace726ecf`.

Admission is representation-wide and contains no class, method, package,
workload, input, hash or result routing. The full required release suite passed
with 308 interpreter tests passed, zero failed and two ignored; all other
workspace tests and doc-tests also passed.

### Completed RAM refinement: store array element ids in descriptor padding

The numeric element id initially reduced each `ArrayObj` from 48 to 40 bytes,
but the parallel `ArrayDesc` spine still retained four bytes of mandatory C
layout padding after its `(pointer, u32 length)` fields. Moving `element_id` into
that padding leaves the descriptor's JIT-visible size, stride and data/length
offsets exactly unchanged while reducing `ArrayObj` from 40 to 32 bytes. At the
fresh 1,544,784-slot census capacity this is an exact further structural saving
of 12,358,272 bytes (11.786 MiB); at the earlier 2,648,102-slot ceiling it would
save 20.203 MiB. Snapshot wire records remain string-based and unchanged.

The descriptor owns metadata for live, dead and pre-reserved handles. GC writes
the out-of-range dead sentinel together with the null pointer and zero length;
slot reuse atomically republishes the new id/data/length tuple. Parallel workers
write one disjoint pre-reserved `ArrayObj` and descriptor each, without touching
the interner. Restore interns only live wire names before rebuilding the same
descriptor metadata. Focused release tests cover exact 32/16-byte layouts and
offset 12, normal mutation, GC retirement/reuse, malformed ids, concurrent
reserve/commit, snapshot golden bytes and native JIT array loads/stores.

A rejected intermediate tracer split the descriptor into aligned pointer and
length tables, reducing its cost from 16 to 12 bytes. Two live-counter pairs did
lower RSS by 6.7 and 11.6 MiB and private memory by 5.5 and 5.8 MiB, but five
pairwise setup deltas changed direction (`+3.491`, `-0.821`, `-3.601`, `+4.040`,
`+1.846` seconds). It also added a second base load and index operation to the
hot compiled array path. The split implementation was removed in favour of the
larger padding-fusion saving with the original JIT instruction path.

Three interleaved P-core `ChunkFill 0 0` pairs, including both execution orders,
retained hash `1` in all six processes. Baseline setup times were 81.667, 76.224
and 77.650 seconds versus fused-layout times of 76.038, 75.034 and 73.755
seconds. Every pair improved; the median moved from 77.650 to 75.034 seconds
(`-2.616` seconds, 3.4%). Median process wall likewise moved from 78.128 to
75.502 seconds. Peak RSS fell in every pair: 745.3/745.6/744.7 MiB baseline
versus 732.1/732.2/732.1 MiB fused, moving the median by 13.2 MiB. Median private
memory fell from 1,050.1 to 1,037.9 MiB (12.2 MiB), closely matching the exact
spine delta. The fresh fused-layout canonical `ChunkFill 1 64` completed in
93.735 seconds / 758.0 MiB with `SETUP_MS=73566`, `FILL_MS=19745` and exact hash
`8790f04ace726ecf`; its single-run wall difference from the prior canonical is
not attributed to the layout because the interleaved setup series is the
controlled timing evidence.

Admission is representation-wide and depends only on stable handle layout. It
contains no class, method, package, workload, input, hash or result routing.
The full required release suite passed with 309 interpreter tests passed, zero
failed and two ignored; all other workspace tests and doc-tests also passed.

1. Finish extracting all execution-thread state from `Vm`.
2. Keep every per-thread root visible to GIL-held and region-STW collectors.
3. Expand detached `RavaPool` kernels only where effect/ownership proofs close.
4. Add private nurseries plus transactional publication for allocating tasks.
5. Route structurally recognized Executor/ForkJoin/CompletableFuture task graphs
   from the resident queue through scoped detached tasks.
6. Make shared old-heap operations concurrent only after object/field ownership,
   barriers and GC publication have executable proofs.

## Acceptance gates

- Exact result parity with serial RAVA and a JVM reference.
- Forced abort proves the unchanged serial retry and publishes no partial result.
- Sub-threshold work remains serial.
- At least five interleaved fresh-process runs show CPU/wall above one core and a
  parallel median outside timing noise.
- Release tests, bounded RSS and GC stress pass.

The existing `bench/ParReduce.java` is the smallest generic acceptance workload;
its identity is used only by the benchmark command, never by runtime policy.
