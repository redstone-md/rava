# Making the interpreter run on `&Vm` — the sizing, and the plan

## Current source state (2026-09-22; product source base `903ec60c`)

The source has advanced beyond the initial signature-only sizing below. Since
row 625, commits `24dcd02e` through `e48831da` moved heap tables behind locks,
made heap allocation/field writes callable through shared references, converted
the invoke/opcode execution path through `&SharedVm`, and removed the heap's
`as_mut_transitional` wrappers. `ThreadContext` and `HeapView` also exist.

This is **not yet shared execution**: `SharedVm` is still a type alias for `Vm`.
`WorkerContext` now installs a worker-local `ThreadContext` through TLS for the
trace-gated diagnostic call stack only; coordinator execution retains the
`Vm.call_stack` fallback. This state contains `String`s, not GC `Value`s, and is
not a checkpoint or collector root. `jit_run` and `jit_call_body` still require
`&mut self`, and worker callbacks retain `worker_locked_exec`, which holds the
resolve lock across callee execution. The remaining `repr(C)` `JitCtx` layout
is consumed by generated machine code; do not change its field representation
or treat `&SharedVm` signatures as proof that concurrent mutation is safe.

## Next tracer bullet

The current bounded bullet moves only the trace-gated diagnostic call stack
into worker-owned `ThreadContext`; coordinator callers continue using the
`Vm.call_stack`. It does not change the worker lock lifetime or make compiled
invocation lock-free. Focused tests cover exact trace formatting, worker TLS
isolation and moving-GC preservation. Continue from the **actual serialized
execution seam** through `worker_locked_exec`, `jit_call_body`, `jit_run`,
callback dispatch and GC roots.
Unknown callbacks, shared writes and allocation/GC paths stay serialized until
their ownership contracts are proved. Exact product hashes and successful
worker completion without fallback remain acceptance gates; only a
snapshot-free same-binary launch comparison may claim wall improvement.

### Live result (2026-09-24)

The no-name-shortcut diagnostic boot reached `Done` in 30.293 s, but its
first worker batch completed 0/2 at 48 ms (`direct miss abort_in_cb=invoke`,
shared-field aborts, then pending-exception aborts). A one-run retry with
reserved-handle ownership enabled also completed 0/2 and then aborted on a
worker class-load miss for `LootPool`; it exited before `Done`.

Source tracing found distinct cold-metadata refusal paths: worker direct-mono
cache fills are intentionally skipped, while `ClassLoader::get_or_load` refuses
any cache miss on a gilfree worker. The next candidate is not a wider worker
lock; first audit whether serial pilot warmup can prepopulate the admitted
bodies' structural dispatch/class-load closure completely. No such complete
prewarm proof or performance result exists yet. Neither boot proves the string
spill path or improves startup wall time.

Follow-up evidence (2026-09-25): the bounded dry-run callee census in ledger row 670 did **not** stage the first `LootPool` worker miss. The relevant registry lambda reported `dry_loaded=0,dry_names=[]`; the miss occurred later under `RecordCodecBuilder → Function.apply`. The current structural callee closure is therefore incomplete, and it does not justify a warm-only preflight publisher. Broader class-file/linkage discovery remains unproven; keep worker class-load and ownership guards unchanged.

Pilot-count diagnostic: `RAVA_PILOT_N=2` alone made the n=3 batch fully serial
through fast-park. With `RAVA_NO_FAST_PARK=1`, the first remaining worker still
hit the direct-miss abort (0/1); a later unrelated n=2 batch completed 1/1,
while subsequent batches aborted. This is not stable worker success or
performance evidence, and changing pilot counts is not a production fix.

No speed credit is assigned to the signature/heap groundwork. Current
no-explicit-shortcut snapshot-free launch median is 29.3 seconds (ledger row
628), and the remaining native-safety name gates still block strict no-adapt
acceptance.

## Replacement direction (2026-08-03)

The warning below is the active decision: do not execute the original
mechanical `&Vm` signature flip. The fresh serial PC-stack run and the merged
ETW attribution show that the wall floor is before the ring phase: roughly
53-60 s of coordinator work is spent in generic guest JIT, callback dispatch,
native bridges and moving-GC activity. Removing the worker lock alone cannot
reach the wall target.

The next arc is an ownership redesign with tracer bullets, not a larger set of
native arms:

1. **Inventory state by ownership.** Classify every field touched by
   `Vm::invoke`, `Vm::jit_call_body`, `opcodes::run`, native dispatch and GC as
   immutable loader/JIT metadata, worker-local execution state, or shared
   mutable heap/GC state. The inventory must include reflection, native
   re-entry, exception roots and safepoints. No code path changes in this step.
2. **Introduce an execution context.** A worker receives a private context
   containing frames, argument/root buffers, nursery allocation state and
   pending exception state. It may borrow frozen loader/JIT metadata through
   read-only handles; it may not borrow the mutable `Vm` or a Rust side table
   that the collector cannot trace.
3. **Make publication transactional.** A worker either completes a proven
   read-only guest segment or publishes its private allocations and write set
   at a stop-the-world boundary. Native, reflection, class loading, unresolved
   dispatch, allocation overflow and GC requests remain on the serial fallback
   until each has an executable proof. Unknown cases stay serial.
4. **Build one generic invoke tracer bullet.** Split the compiled callback at
   the ownership boundary and test one primitive/read-only guest call through
   the context. The bullet must have focused parity tests, moving-GC stress,
   forced abort/retry tests and a complete admission census. Do not broaden it
   to the full `opcodes::run` surface until these proofs pass.
5. **Only then measure parallel wall.** Use reversed fresh-process A/B on the
   actual jar, with no restore/checkpoint/stackful controls, and report wall,
   `Done`, RSS and hash separately. A faster ring or diagnostic run is not
   acceptance evidence while the coordinator prefix remains above 20 s.

This ordering preserves the no-adapt rule: admission may use only structural
proofs, ownership state, exact ABI/epoch and explicit freeze/lifetime facts;
never jar identity, class/method names, timing, hashes, outputs or history.

> **2026-07-27 (later) — the invoke family is NOT a bounded surface, and this plan should not be
> executed as written.** Flipping the four `opcodes::invoke*` to `&Vm` and asking the compiler
> gives **25 errors**, and they are not spread across invoke's own logic — they are almost all one
> dependency: the NATIVE LAYER. `natives::try_invoke`, `natives::try_invoke_chain`,
> `natives::stringify`, `fastutil::dispatch`, `cache::dispatch`, `gson_native::build`,
> `build_fi_default`, `unlinked_native`, plus `push_call_roots` and `pop_args`. "Flip the invoke
> family" therefore means "flip `natives.rs` and `reflect.rs`" — thousands of lines that genuinely
> allocate and mutate. The seventeen-leaf sizing below badly understates this one group.
>
> The cheaper direction is to take calls AWAY from `jit_call` rather than make it lock-free: a call
> served by `jit_direct_call` never reaches the locked gauntlet at all. The census names exactly
> which populations are excluded from it — capturing lambdas (`Predicate#test`/`Function#apply`,
> ~766 k locked calls per region), `java/`-served natives (`AtomicLong`, ~485 k), and
> `Climate$RTree$Node#search` (~120 k). Three independent pieces of work, none of which requires
> touching the native layer's signatures.
>
> **2026-07-27 — read `docs/lane-array-store-frontier.md` first.** This plan was sized against an
> abort attribution that turned out to be wrong: the ring batch never aborted at `vm_ref`, it
> aborted seven times at the array-store barrier. With that barrier soundly bypassed the region
> completes for the first time (`127/127`, `keep=true`), and only THEN does the lock become the
> measured wall — a ~600 ms one-at-a-time staircase, ~3.5× aggregate throughput left on the table.
> So the conclusion below survives, but its ORDER does not: the first prerequisite is not the
> seventeen leaves, it is making the **resolve caches concurrency-safe**, because an unlocked
> region dies on six threads racing first-touch class loading long before any leaf signature
> matters.

## Why this and not more arms

The gil-free lane's ceiling is one global lock held across a callee's whole *execution*, and the
audit in `docs/lane-locked-residue.md` closed the cheaper route: of the eight callees that take it,
**exactly one** can be served by a hand-written lock-free arm. Every other one either runs guest
code or writes shared state. The common cause is not races — it is that `opcodes::run` is
`fn run(vm: &mut Vm, ..)`, and **the lock is what supplies that exclusive `&mut`**.

So the arc is: let the execution path run on `&Vm`.

## The sizing, measured rather than estimated

The tree carries 374 `&mut Vm` sites and 143 `&mut self` methods on `Vm`, which reads as a
multi-day refactor. It is not: most of those are in natives, compile paths and tooling, not on the
interpreter's execution path.

Flipping the one signature and letting the compiler enumerate the fallout gives the real number:

```
pub(crate) fn run(vm: &Vm, code: &Code, frame: &mut Frame) -> Result<RunExit, VmError>
    => 32 errors, all E0308, at 17 distinct callees
```

| callee | sites | why it wants `&mut` |
|---|---:|---|
| `find_handler` | 3 | exception dispatch may load a class |
| `getstatic` / `putstatic` / `getfield` / `putfield` | 4 | static-cell fills, field-id memo |
| `invokevirtual` / `invokespecial` / `invokestatic` / `invokedynamic` | 5 | resolve + cache fill |
| `new_object_value` / `anewarray_ref` / `alloc_multi_array` | 3 | allocation |
| `instance_of_at` | 1 | type-test memo |
| `pump_network` | 1 | main-thread only — a worker never reaches it |

**Seventeen callees is the whole surface**, and that is what makes this tractable.

## What the measurement already says about them

`RAVA_FREEZE_TALLY` counted, across a whole ring region and 292 million calls, **two** touches of
frozen shared state — both `Heap::write_static_cell`, zero cache fills. So on the worker path these
seventeen mutate essentially nothing; they carry `&mut` because of what they *might* do on the main
thread, not what they *do* in the ring.

That is the difference between this refactor and a redesign: the semantics do not change, only who
is allowed to say so.

## Order of work

1. **Split the fields the seventeen actually touch** into interior-mutable cells — allocation
   cursor, the resolve/type-test memos, the static-cell table. Everything else on `Vm` stays as it
   is.
2. **Flip the seventeen to `&Vm`** one family at a time, in this order: the memo readers
   (`instance_of_at`, field/static access) first because they are pure on the warm path; then
   allocation, which needs the worker's own arena segment and already has one; then invoke, which
   is the largest and depends on the first two.
3. **`find_handler` and `pump_network` last.** The first can load a class, which is genuinely
   main-thread work; the second is main-thread by contract and can be gated out of the worker path
   outright.
4. Only then remove the lock from `worker_locked_exec`, and re-run the lane A/B.

Each step is independently gated by ChunkFill `8790f04ace726ecf` and RingScan `37539c6c375d0c3`,
so a regression names itself at the step that caused it rather than at the end.

## What success looks like

The lane currently reads `Done 62-64 s` against serial's `43-48 s` — parallelism is a 35-45% LOSS,
because eight workers execute one at a time plus lane overhead. With the lock gone the arithmetic
already on record applies: `2.2 + 44/6 ≈ 9.5 s` on the Done window at six P-cores, i.e. wall from
~97 s toward ~63 s.

## Ownership audit update (2026-09-24)

The worker path is not blocked by a missing thread-local context alone. `LaneWorker::prepare`
installs a private `JitCtx`, TLAB/arena/field-box slices and abort state; `run_batch` passes that
context to compiled calls (`gilfree_cf/mod.rs`, `gilfree_cf/batch.rs`). The serializing seam remains
`worker_locked_exec` in `vm/jit_call_body.rs`: its resolve lock supplies exclusive `&mut Vm` across
callee execution.

The first barriers are cold direct-mono cache fills and class-loader misses
(`jit/callbacks/dispatch.rs`, `vm/dispatch.rs`, `class_loader/loader.rs`). Later barriers include
shared field/array writes, unresolved indy, class literals, allocation overflow and pending
exceptions. These fail closed and trigger whole-batch serial retry; removing the lock without a
proof for each shared state path would be unsound.

Smallest viable next seam: a serial-pilot-published, immutable admission ticket proving dispatch
closure, class-load closure and the task's write set before a region opens. Workers may use the
ticket only for covered calls; everything else retains the existing abort/retry path. Per-worker
`JitCtx`, TLAB views, reserved slices, TLS exception state and frames remain private; loader caches,
static cells, side tables, JIT publication and GC remain synchronized or coordinator-owned.

This is necessary groundwork, not the route to `<5 s` by itself. The worker batch currently
completes `0/2` on the measured boot, while the remaining serialized execution includes far more
than the small parallelizable lane prefix. No wall-time credit is available without a new,
transactionally correct shared-execution tracer and a fresh snapshot-free measurement.

## Deferred-task census (2026-09-24)

One isolated current-source launch ran with `RAVA_PLAN_CENSUS=1` and the periodic
reporter enabled, but without `RAVA_LANE_PURE_ADMIT`. The route was therefore
unchanged by the purity gate. It reached `Done (0.441s)`; the reporter observed
three deferred tasks and two `plan_meta` calls, both admitted. These counters
describe planning volume, not startup wall or speedup.

This is not yet the requested structural census: existing counters do not
classify those deferred bodies by static-dispatch closure, class-load closure,
or write set. Enabling `RAVA_LANE_PURE_ADMIT` would execute the proof as an
admission gate and change routing, so it is not a valid substitute for a
diagnostic-only census. No structural proof verdicts or performance claims are
inferred from this run.

**Prototype status (2026-09-24):** an opt-in `RAVA_LANE_PROOF_CENSUS=1`
diagnostic now runs the same devirtualised proof after ordinary task planning,
without controlling admission. Its loader scope stages cold classfiles outside
the live loader/hierarchy/path-index caches; resolution and VM proof memos are
also bypassed while the scope is active. With the flag unset, the census does
not run.

The focused loader contract test passes with the lane's required feature:
`cargo test -p rava-interp --features next-gen-gc dry_run_load_tests -- --test-threads=1`
(7 passed); the panic-unwind scope test also passes (1 passed). Coverage includes staged visibility,
cleanup, worker refusal, nested/per-loader isolation, no shared-cache/epoch changes, no path-index
mutation, and TLS fact restoration. The default-feature test invocation does not compile this
checkout because it excludes `next-gen-gc` while current source paths reference feature-gated APIs;
that is a separate cfg-discipline issue.

Two ON/OFF pairs (four launches: ON, OFF, OFF, ON), using the same
`release-fast` binary and disposable copies of `world_pristine`, all reached `Done`.
External launch-to-Done medians were 33.29s with the census and 34.29s without it;
the individual runs ranged 32.86–33.71s ON and 33.99–34.59s OFF. The internal
server `Done (x)` timer is not launch wall time. The census emitted ~95 KB of
diagnostics versus ~26 KB without it, so these four noisy runs do not establish
a performance gain; importantly, this diagnostic does not approach the <5s
acceptance target.

The corrected runtime census recorded 79 proof rows: 4 closed dispatch and class
loading but left writes/effects open; 2 had open dispatch with no cold loads;
73 had open dispatch, cold loads, and open writes/effects. No row closed all
three dimensions. The frequently planned chunk-ring lambda reaches virtual
dispatch and cold class-load blockers (`dry_loaded=4`); the function-library
reload lambda opened dispatch and staged 17 cold classes. Root-body rejects,
as well as recursive rejects, now conservatively mark writes/effects open.
This supports the existing decision: do not issue an execution-lock ticket.
The deferred lanes still need closed write effects and callbacks, plus safe
handling of cold metadata and nested dispatch.

## Ticket-gated execution decision (2026-09-24)

The worker-path review rejects skipping `worker_locked_exec` on the strength of
the census alone. The proof covers bytecode-level dispatch, loads, and writes;
it does not yet prove every emitted inline store's worker-access path, indirect
PIC call, callback that can reconstruct `&mut Vm`, or nested dispatch miss.
The general callback path relies on the resolve lock for exclusive VM access.
Unlocking it before those paths are closed would be an unsound semantic change,
not a safe prototype.

Keep the structural census diagnostic-only. The next runtime gate requires a
buildable JIT, a worker batch that completes work on cold metadata (the measured
batch is still `0/2`), and proof that every uncovered callback and generated
store continues to abort before publication. Until then, the existing lock-free
direct/frozen/native arms remain the only execution paths to extend.

### Emit-time store-form correlation census (2026-09-26)

With both `RAVA_LANE_PROOF_CENSUS=1` and `RAVA_LANE_STORE_CENSUS=1`, the
`[LANE-PROOF]` row now joins the dry-run proof's visited `Code` identities to
the JIT emitter's `putfield` and `*astore` forms by code identity and bytecode
PC. It distinguishes callback-only, COW-guarded callback/inline, ring-guarded,
and unguarded inline forms; the COW arm state is recorded separately.
`compiled/no-store` is distinct from a missing emit record (`UNKNOWN`).

This is diagnostic-only and default-off. It does not change admission, routing,
or barriers, and it is not a shared-execution or `worker_locked_exec` safety
proof: open dispatch/load/effect edges, indirect PIC and callback re-entry,
and incomplete nested proof coverage remain stop gates. No worker lock may be
removed on the strength of this store census alone.

A disposable release smoke on 2026-09-26 ran three fresh `world_pristine`
copies: census flags off, store census only, and both proof/store censuses.
All three reached `Done`; this did not reproduce the reported hardware-info
SIGSEGV. In the combined run, product lane rows contained only `compiled/no-store`
or `UNKNOWN` store joins—no concrete emitted store form was observed in the
visited proof trees. This confirms the join works, but supplies no product
segment to admit and is not a launch-time A/B.

### Shared execution is not a standalone path to the launch target (2026-09-26)

The latest no-PGO release ABBA measured 29.30 s median for the branch
(ledger row 698). A prior PGO release boot measured 21.44 s (row 686); even
that lower reference needs 16.44 s removed to meet the target.
The PGO phase trace measured 13.62 s to `Environment`, another 7.46 s through
recipes, and only 0.29 s from level preparation to `Done` (row 675). The
worker-reachable reload window is bounded, while the larger prefix remains
serial.

Current task-runtime workers hold the GIL for each complete guest task
(`task_runtime.rs`); the gilfree batch parks the coordinator and only runs a
region when its precomputed keep/claim gates pass (`gilfree_cf/batch.rs`).
Prior pricing bounds the best shared-execution opportunity in the reload window
to at most 1.55 s saved, and generic metadata preflight to 0.3–1.0 s (ledger
rows 677, 681). Neither closes the required 16.44 s wall reduction. The 2026-09-26
store-form join also produced only `compiled/no-store` or `UNKNOWN` for product
proof trees; dispatch/load/effect closure and worker completion remain absent.

Decision: keep this census diagnostic-only and preserve `worker_locked_exec`.
Do not implement a lock-removal tracer on this evidence. Reopen the shared-
execution route only with a generic, measured source of parallel work on the
serial prefix and a closed worker proof; a reload-only speedup cannot satisfy
the launch objective.

## Cross-arc contracts: value model, shared execution, and C2-class codegen

**Decision (2026-09-24): stage all three arcs, but do not couple their safety gates.**
The arcs solve different costs and share only explicit contracts:

1. **Single-store value model owns reference meaning.** Until the global migration gate is
   satisfied, tagged `Value`/handle paths remain valid and are the fallback. Any arena
   reference must be forwarded or rooted across moving-GC safepoints; no dual-store mirror
   and no inference from class/method names, boot history, or observed values.
2. **Shared execution owns mutation and publication.** Workers may mutate only their private
   execution context until a proved publication boundary. Immutable loader/JIT metadata may
   be borrowed only with a pinned lifetime/epoch. An unresolved call, cold metadata, native
   callback, shared write, exception, allocation overflow, or GC request keeps the existing
   serialized abort/retry behavior. The census is diagnostic, not an admission ticket.
3. **C2-class codegen owns emitted control flow, not VM semantics.** Generated code must
   obey the active `Value` representation, rooting, barriers, exception, and callback ABIs.
   It may use only structural facts validated by the compiler/runtime; it must not bake in
   workload names, output, timing, or history. A faster microbenchmark does not waive VM
   parity or safepoint requirements.

### First end-to-end tracer: primitive-array single-store slice

Use the existing opt-in `RAVA_ARENA_PRIM_ARRAYS=1` method-confined primitive-array
tracer as the first slice, rather than starting a second byte-array implementation.
The current source has a CFG/confined-flow admission plan, all primitive `atype` CIDs,
an inline arena allocator, representation-aware JIT consumers, and the historical handle
callback as fallback. Its current opt-in, callbacks, and tests are not yet a proof of
end-to-end safety: first close parity and moving-GC behavior across overflow/retry, exact
array-kind operations, and unsupported/escaping CFG fallback. Do not admit arrays that
escape the analyzer's confinement proof or broaden into reference arrays as part of this
slice.

This candidate was selected as a correctness/migration tracer, not a promised wall-time win:
primitive arrays are only 8.5% of the measured array blocker bucket, and the value-model
plan says bare-load payoff arrives only after the global flip. Its product coverage is the
stop/go gate; the full-run recheck below rejects it, so do not spend further safety effort
on it solely for the startup objective.

Do not advance from a candidate that fails its coverage gate. For a replacement
value-model slice, the dependency order remains: prove the single-store/reference-root
contract, then build a generic shared-execution invoke tracer without bypassing the lock
on unproved paths, then select a C2-class codegen change against the stable representation
and measured compiled hot path. Each tracer has its own parity, GC/worker-safety, and
focused performance gates; no intermediate result is credited toward the launch target
unless a fresh snapshot-free launch measurement shows it.

### Primitive-array tracer product-coverage gate (2026-09-24)

The full-run recheck rejects this as the first **product** tracer for now. On the rebuilt
release binary (`85614d16126e140f229c3efb2e53bb6cc9944252e79e7c3aa4e7d3dcb5e631f5`), an
isolated copy of `world_pristine`, with `RAVA_ARENA_PRIM_ARRAYS=1`,
`RAVA_ARENA_PRIM_ARRAY_STATS=1`, and periodic `RAVA_JIT_STATS_MS=10000`, reached
`Done (0.469s)`. All three periodic `[ARENA-PROBE]` rows through `Done` omitted `o4298`,
the opt-in primitive-array allocation counter, while reporting millions of ordinary
`newarray` callbacks. Thus the 12-second prefix result was not just a windowing artifact:
the current analyzer admits no primitive-array allocations on this production run.

The existing code stays default-off. Static analysis and GC-shape tests pass, but no
moving-GC/output parity campaign is justified for a path with zero product coverage. Reopen
that proof only after a structurally admitted product population is demonstrated; otherwise
choose a different value-model tracer. This is a candidate rejection, not evidence that the
array implementation is semantically correct or that the value-model arc is complete.


### Reference-array coverage probe

An earlier OFF/ON full-run comparison had callback-count drift similar to its
apparent signal, so it was inconclusive. The diagnostic gate
`RAVA_ARENA_REF_ARRAY_STATS=1` now inserts the existing read-only
`rava_arraylength` callback only after a successful inline reference-array
allocation; the callback census labels it `o4298`. With the gate unset, the
feature-on path emits no diagnostic callback.

The captured release run (`3cd77e2995b2a58d2e59d6df23276502ded333a3510eb5cbeaaba1788d5f6136`)
used an isolated `world_pristine` copy, clean JIT environment, and both
`RAVA_ARENA_REF_ARRAYS=1` and `RAVA_ARENA_REF_ARRAY_STATS=1`. It reached
`Done (0.433s)`; all three periodic `[ARENA-PROBE]` rows omitted `o4298` while
reporting millions of ordinary `newarray` callbacks. No successful inline
reference-array allocation was observed during this product boot. This rejects
it as the current product tracer; it does not establish moving-GC/output parity
or provide timing evidence.

### Runtime field-receiver census (2026-09-24)

The opt-in `RAVA_FIELD_REPR_CENSUS=1` wrapper classifies receivers at the existing
`rava_getfield` / `rava_putfield` callback boundary, then delegates unchanged to the
original callback. With the flag unset, module construction registers the original
callbacks directly. This census covers those callback crossings only; it does not count
field accesses emitted inline in generated code.

The rebuilt release binary (`8b5034c6132646ce429103bb60347fccf5f3901ee4746e6924655cc1ec044b7b`)
ran from an isolated `world_pristine` copy and clean JIT environment.
It reached `Done (0.424s)` and emitted three periodic probes. At the last probe it had
counted 10,689,238 getfield callbacks (10,138,631 arena receivers, 550,607 handles) and
2,381,707 putfield callbacks (2,233,712 arena receivers, 147,995 handles). No null,
pinned, array, box, array-box, or other receiver was observed. The arena fractions are
94.8% of reads and 93.8% of writes.

This establishes a product-covered arena callback population, but not a single-store
opportunity by itself: the callback already routes arena objects through arena cells,
while handle writes retain their authoritative FieldBlob/mirror contract. Do not bypass
those paths based on these counts.
The follow-up slot-cache census below found no runtime executions of the emitted probe.
Do not pursue its hit-rate or performance optimization further without first identifying
a product workload that executes these probe sites.

### Arena slot-probe product-coverage gate (2026-09-24)

`RAVA_FIELD_PROBE_CENSUS=1` counts emitted sites during JIT compilation and uses a gated
atomic counter in generated code for dynamic hits/misses. With the flag unset, generated
code contains no counter operation. The rebuilt release binary
(`3219cc2ceed7b30007aad6032026d1597d0385008d3731dea3970cf2638127f2`) ran against an
isolated `world_pristine` copy with clean JIT settings and both field census gates enabled.

Three periodic probes reported **2,051 cumulative emitted sites, 0 hits, 0 misses**.
The boot reached `Done (0.412s)`; the server then remained alive until the 36-second
timeout (status 124), so this run is coverage evidence, not launch-to-Done timing. The
separate field callback census still counted over 10.6M getfield callbacks, showing that
this emitted slot-probe population is not the source of the measured product field traffic.
This slot-cache tracer is therefore a product-coverage reject.

Do not confuse those callback counts with slot-probe attempts: callbacks are the fallback
boundary and do not identify which generated field sites would have executed the probe.


### Shared-execution and C2 stop gates (2026-09-24)

The worker proof census covers 79 rows and closes **0/79** across dispatch, class-load, and
write/effect facts. The measured first worker batch remains **0/2**; cold class-load and
direct-dispatch misses abort to the serial path. `worker_locked_exec` is the current
exclusive-`&mut Vm` ownership proof, not incidental overhead. While enabled, seven shared
outer-callback gates account for 82.38M of 86.30M resolve-lock acquisitions (95.5%).
The existing WS-NATIVE pure-read fastpath (`List#get`, `findIntervalStart`) is the only
small end-to-end lock-free population already established; it cannot be widened by method
or workload identity. There is no generic lock-safe shared-execution tracer at present.
Next safe work is diagnostic-only: extend proof coverage to emitted inline stores, indirect
PIC/callback re-entry, and nested misses before issuing any immutable admission ticket.

The existing production census already measures the deferred product writes: after
admitting the reload tasks, seven datapack/bootstrap tasks reach the lane in batches of
one to three, and all abort/re-run serially; four early batches report
`aborts: shared_field=2` even with worker-owned handle tracking enabled. Those tasks
mutate pre-existing registries by design, so per-worker object ownership cannot certify
them as private. See `boot-speed-ledger.md` rows 247–249. This closes the write-semantics
stop test: broad worker offload is not the remaining safe route for those tasks.

The current C2-oriented direct-local repeated-getfield tracer is bit-exact but neutral in
the reference-field hot loop; row 657 also shows at least 23.4% of sampled wall outside
`JIT_EXEC`. Codegen remains a necessary arc, not a sufficient launch plan. Do not promote
another emitter change without a measured hot-path reduction and a paired plan for the
non-codegen work.

### Combined stop/go contract

No arc is accepted from type-checking or microbenchmarks alone. Preserve exact guest output
and exception behavior; run the canonical parity gates, moving-GC stress for any changed
root representation, and cold-metadata/abort-retry tests for shared execution. Profile the
same production configuration before and after, then use fresh isolated worlds and a
snapshot-free comparison for launch-to-Done. Roll back a tracer that regresses its
pre-registered metric or violates parity.
The fresh gates-off product run on binary
`3219cc2ceed7b30007aad6032026d1597d0385008d3731dea3970cf2638127f2` used an isolated
`world_pristine` copy, no snapshots, and no field-census flags. It reached `Done` in
**28.526 s launch-to-Done**; the process was then terminated with SIGTERM (status 143)
because the server remains alive after `Done`. This is a single baseline measurement,
not an A/B win, and remains far above `<5 s`.
The remaining C2-vs-codec comparison is an investigation, not a launch-sized candidate:
the warm-reload profile assigns 6.03% to compiled guest bytecode, while measured call
plumbing is ~30% and the largest call-plan removal bought ~2.4% (see
`the-boot-price-map.md`, rows/sections 3095–3110 and 3166–3184). Even eliminating that
entire compiled-code slice cannot close the 23.5-second gap in the current launch sample.
The `<5 s` target therefore has no admitted single-arc tracer; a multi-arc combination
would need fresh end-to-end evidence before being claimed.

### Broader runtime rewrite gate (2026-09-25)

The follow-up host-work map found no safe, measured local tracer that can support
the `<5 s` objective. The allocation families are already served by structural
arena/cache work or have neutral prior removals; the remaining handle-heap
representation has a priced ceiling of about 0.82% of boot. The field-probe
population is zero at runtime.

The compiled fastutil seam is not an unserved call population: 7.88M measured
`fu_fastpath` refusals are 99.94% `CubePointRange`, and every miss already
continues through the direct-call cache. Skipping those attempts through a
compile-time class proof would require a new reference-class analysis; the
equivalent static bypass was boot-neutral (59.35 s to 59.24 s wall; `Done`
slightly slower). Its ceiling is about 0.5%, not a launch-scale lever.

A new native call-plan registry is also a duplicate design: interpreted
`CallPlan`, compiled `NativePlan`, and resolved `NativeProgram` layers already
exist. The remaining generic-dispatch residue is estimated at about 2% of wall,
with sibling dispatch tracers measured neutral or worse. The shared-execution
route remains unsafe for product tasks, while the largest call-plan removal
measured only about 2.4% on warm reload.

Decision: do not implement another narrow cache, receiver memo, or native-plan
variant under the label of a broad rewrite. The <5-second acceptance task stays
open. A valid next prototype must first establish a materially different
runtime work model and a multi-arc ceiling from current-source product evidence;
the existing candidate set does not justify claiming that the target is
reachable.

### Activation-stack call-plan slice (2026-09-25)

The runtime already has `execute_stackful`/`drive_stack`; this is not a new
trampoline implementation. The audit found the explicit driver handles normal
returns, staged calls, yields, GC safepoints, class initialization and exception
unwind. Compiled entries deliberately clear the trampoline before running so
compiled callbacks remain synchronous. Do not route compiled/JIT calls through
`RunExit::Call` without a resumable compiled-call ABI.

The driver still builds the staged callee frame directly, so stackful guest calls
continue to bypass `invoke_framed`'s flatcall/JIT/CNP/AOT admission and remain
interpreter-only; this is pre-existing trampoline behavior, not changed by the
CallPlan replay. Plan eligibility also has to exclude every opcode-specific
intercept, so new static/special-call hooks must update the corresponding
`gson_claim` / `fastutil_int_init` exclusion.

The first reversible slice reuses the existing `CallPlan` on the explicit stack.
After each site's native interception decisions, static and special bytecode
calls may publish the resolved declaration/index even while stackful; subsequent
replays stage that cached target as `PendingCall` rather than invoking
`invoke_idx` recursively. The class-initialization latch and the Gson/fastutil
exclusions remain intact, and pending arguments keep the existing GC-root scope.
No checkpoint, snapshot, or runtime adaptation code is involved.

Focused parity coverage starts with stackful calls, checks plan publication,
then compares recursive and cached-stackful results for static and special
calls.

The release product A/B toggled only `RAVA_STACKFUL` on a copied pristine world;
the harness cleared inherited `RAVA_*` variables and no snapshot/checkpoint
inputs were used. Across four paired samples all eight launches reached `Done`.
Pooled medians: recursive 29.4 s wall / 49.1 CPU-s, stackful 29.2 s / 47.6
CPU-s. Wall delta is −0.68%, inside the harness's ±8% wall floor; CPU delta is
−3.05%, just beyond its ±2.5% floor. This is evidence that the stackful product
path completes, not evidence that the CallPlan slice alone caused the CPU
difference. The best measured launch remains 29.2 s, so `<5 s` is still open.

### Compiled callback resume ABI audit (2026-09-25)

Generated invoke sites call a synchronous C ABI (`rava_invoke`/specialized
callbacks) that must return the callee's packed value before machine code
continues. The callback has only caller class, constant-pool index, kind, and
packed arguments; the caller's remaining operand stack and live locals exist as
Cranelift SSA values, not as an activation the explicit-stack driver can resume.
The current `jit_call_body` makes a synthetic frame, dispatches synchronously,
and packs the return value.

There is reusable mid-method deoptimization: `emit_deopt_exit` serializes typed
locals and the operand stack at an invoke PC, then `rava_deopt` rebuilds a
`Frame` and interprets from that PC. It is not a compiled continuation: the
compiled caller returns, and the rest of that method stays interpreted. Today
the emitter also refuses spliced bodies, methods without complete local types,
and void-returning methods. Emitting this at every compiled call would therefore
be incomplete and would force interpreter execution for each affected method.

A true compiled-to-stackful resume needs explicit caller-frame state plus a
continuation contract across all generated call forms (generic callbacks,
specialized callbacks, direct calls and PIC hits), including return values,
exceptions and GC roots. Existing synchronous callback ABI cannot be toggled to
stage guest calls: callbacks returning 0 early already caused lost side effects
in registry initialization. Do not change that behavior until the continuation
is represented and parity-tested. This audit establishes a viable deopt
experiment, not a credible route to `<5 s`; no continuation prototype or
performance claim is made.

### Resumable compiled-activation design (2026-09-25)

The minimum coherent contract is an event-returning compiled activation, not
an early-returning callback:

1. At a guest call, generated code spills the exact caller locals and residual
   operand stack into an activation-owned, GC-visible frame buffer and returns a
   `Call` event containing the call-site continuation id and resolved call
   operands. Native/host calls remain synchronous and return a value normally.
2. `drive_stack` pushes the guest callee activation. On return, it writes the
   typed result into the caller's saved stack state and invokes the same
   compiled method at its continuation id; execution resumes after the call
   rather than replaying effects before it.
3. A thrown guest exception unwinds the activation stack with existing handler
   semantics. Compiled frames currently have no exception handlers; until
   handler-state serialization exists, any compiled method with handlers must
   remain ineligible. All saved references must participate in the collector's
   root/forwarding protocol across allocations and safepoints.
4. The event ABI must cover every machine-code guest-call path—generic and
   specialized callbacks, direct calls, PIC hits—and distinguish guest calls
   from synchronous native callbacks. A missed path would preserve recursion
   or return a placeholder and silently corrupt semantics.

Precondition before this ABI can even be exercised: `execute_stackful` currently
builds an interpreter `Frame` and `drive_stack` always calls `opcodes::run`;
the stackful root never enters `invoke_framed`/`jit_run`. The deopt callback
likewise calls `opcodes::run` synchronously and treats `RunExit::Call` as an
unsupported nonterminal exit. Therefore first prove that a stackful activation
can safely enter compiled code and return to the driver; wiring staged callbacks
before that is unreachable on the target path.

The first prototype, if justified by a hot-path census, should support one
ordinary generic guest call in a non-spliced compiled method with a complete
local-type map and no handlers. It must round-trip primitive/reference/void
returns, nested guest calls, thrown exceptions, and moving-GC roots before any
additional call emitter is admitted. Existing deoptimization is useful for
building a caller frame at the call site, but not as the continuation
implementation: it finishes the method in the interpreter and does not resume
compiled execution.

This is a multi-boundary JIT ABI change, not a small trampoline optimization.
The current launch evidence gives no reason to spend that complexity budget yet:
stackful execution completes but moves wall by less than the noise floor, and
the generic dispatch remainder is only a small fraction of wall. Measure
coverage and a credible best-case speed ceiling for the proposed guest-call
continuation before enabling even this one-call tracer. The `<5 s` acceptance
target remains open.

### Compiled entry on the activation stack (2026-09-25)

The stackful driver now attempts the existing JIT entry for a deliberately
narrow class of activations: PC zero, empty operand stack, no exception table,
and bytecode accepted by the existing `is_gc_leaf` structural predicate.
These constraints exclude guest invoke callbacks, allocation, class loading,
and other suspension/GC-triggering operations; a cache miss or ineligible
method remains interpreted. A focused synthetic test disables background
compilation, verifies the cache is compiled before the third invocation, and
checks that invocation's result.

Four snapshot-free ABBA rounds on one release binary, a disposable copy of the
pristine world, and fixed JIT/heap flags measured stackful mode ON at median
29.0 s wall / 47.70 CPU-s, versus OFF at 29.4 s / 49.50 CPU-s. Individual ON
wall times were 28.8–29.6 s and OFF 29.4–31.0 s; server `Done` readings overlap
(ON 0.351–0.375 s, OFF 0.359–0.384 s). The apparent median differences are
small and noisy. Since the A/B toggled the whole stackful path rather than
isolating compiled-leaf eligibility, it does not establish a leaf-specific
gain. It also does not establish a meaningful launch improvement or approach
the `<5 s` target.

This is only a safe compiled-entry prerequisite. It does not implement the
event-returning ABI or resumable guest calls described above; compiled guest
callbacks remain synchronous, and the continuation prototype still requires
separate justification and parity tests.

### Continuation investment gate (2026-09-25)

The new entry test clears the mechanical prerequisite, not the payoff gate.
The snapshot-free release ABBA still needs to remove about 24 seconds of
launch wall to meet `<5 s`; the latest ON/OFF medians differ by only 0.4 s and
the `Done` spans overlap. The measured OFF median is 49.50 CPU-s. Even ideal
distribution of all that work across the workstation's six physical cores
would take 8.25 s, before accounting for serial work. Reaching five seconds
therefore requires removing at least 39% of current CPU work as well as
achieving impossible-to-assume perfect scaling.

An earlier caller-attribution capture (2026-09-06, release-diag,
12,227 primary-thread samples) found 1,789 samples in guest JIT ranges; its
largest named caller leaves were `jit_call_body` (297) and
`jit_direct_probe` (264). This is diagnostic coverage, not an exclusive time
breakdown: the capture is older than the current release A/B, omits compiler
thread samples, and uses conservative candidate-caller scanning. It provides
no evidence that a compiled guest-call continuation could remove the required
CPU fraction. Existing whole-boot profiles also show broad native/runtime work,
not one dominant guest-call leaf.

The Linux address-aware continuation census is implemented as an opt-in
`RAVA_PERF_MAP=1` writer. At code publication it records finalized guest
method address ranges and names in `/tmp/perf-<pid>.map`, the format consumed
by Linux `perf`. It adds no instructions to generated guest code. The map is
created exclusively with mode `0600`, and each record is flushed so it remains
usable after the server exits. This supplements, rather than changes, the
Windows in-process PC sampler.

A stripped release snapshot-free stackful launch reached `Done` in 29.27 s;
`perf` recorded 4,365 cycle samples with no loss. The map contained 8,489
compiled methods; 368 samples matched guest JIT ranges, accounting for 8.41%
of sampled process cycles. No individual guest method exceeded 0.45%. This
establishes
that generated-code coverage is real, but guest-code samples are distributed
across many methods rather than concentrated in one continuation-sized win.

A separate current-source release-diag run reached `Done` in 29.47 s, with
4,419 recorded samples and no loss. The map contained 8,475 methods; weighted
address matching attributed 7.36% of sampled process cycles to compiled guest
methods. Its largest named guest rows were `IndirectMerger.<init>` (0.63%) and
`Shapes.joinIsNotEmpty` (0.47%). The largest native call-boundary self shares
were `jit_call_body` (2.07%), `pic_native::nt_rooted` (1.11%), `jit_enter`
(0.79%), `jit_direct_invoke` (0.72%), and `jit_direct_probe` (0.52%).
Cranelift work remains visible, including register allocation (`VCode::init`,
4.49%; `VCode::run`, 3.42%) and egraph elaboration (1.96%). These are sampled
cycle shares, not exclusive wall times or direct savings estimates.

The map closes the earlier symbol-coverage gap: `perf report` resolves guest
method names alongside native symbols. Still, the guest samples are diffuse,
the individual run and sampling profile are diagnostic, and the symbolized
release-diag timing is not product acceptance evidence. The 29.27 s release
run remains far from `<5 s`; neither capture demonstrates a credible
multi-second ceiling for the continuation ABI. Do not prototype that ABI on
this evidence. Keep the roots/exception proof and a material payoff gate in
place; `<5 s` acceptance remains open.

### Moving-GC root writeback audit (2026-09-25)

The `RAVA_PROBE_STALE` diagnostic now re-enumerates the full post-collection
root population in `vm/gc_root_validation.rs`: interpreter, trampoline and green
thread frames; pending exceptions; task/lane roots; loader statics; native and
map temporaries; the complete arena-heap cell set; JIT buffers; declared
stack-map slots; and enabled pinned-root sets. A stale address forwarded by the
current collection is reported as either `WRITEBACK-MISS-ROOT` or
`WRITEBACK-MISS-CELL`. This runs after writeback, changes no collector inputs,
and is absent unless the diagnostic flag is set.

`cargo check -p rava-interp --features next-gen-gc` and the focused moving-GC
map-root test passed with `RAVA_PROBE_STALE=1`. This extends the safety audit;
it is not a performance result and does not establish the <5 s launch target.

The broader `arena_root_tests` set passes 2/3 tests; its major-GC reclaim-size
assertion fails at `2040 < 2040` both with and without `RAVA_PROBE_STALE`, so
that failure predates and is independent of this diagnostic.

## Full execution redesign (2026-09-26)

The bounded-tracer route is closed; the authorized direction is a correctness-first
shared-execution redesign on a separate branch, while preserving no-adapt and
no-snapshot. This is a new architecture program, not a promise that the existing
lane or its prior 0/79 proof census can be made parallel by removing one lock.

### Required module interfaces

- **`ExecutionContext` is worker-owned.** It owns frames, temporary GC roots,
  pending guest exception, call depth, op counters, thread identity, and the
  active JIT root/cell buffers. A worker never obtains `&mut Vm`, nor a mutable
  reference reconstructed from `WorkerVmRef`.
- **`RuntimeShared` is shareable by construction.** It exposes immutable,
  lifetime-pinned class/JIT metadata and explicit synchronized operations for
  loader state, class initialization, Java-visible shared heap writes, and task
  publication. No plain mutable `Vm` field is reachable through this interface.
- **The collector owns the safepoint protocol.** Every registered execution
  context publishes roots before parking. Collection begins only after all
  mutators acknowledge the same stop epoch; forwarding/writeback is checked by
  the existing `RAVA_PROBE_STALE` verifier. Worker roots may not be omitted or
  inferred from one lane's current `frames::all()` registry.
- **Publication is explicit and transactional.** Private allocations and task
  results remain unpublished until success is validated. Abort discards only
  that task's owned allocations and write log, then retries the task serially;
  shared writes may not be silently retained or rolled back without a proof
  covering every alias and generated-store path.

### Staged implementation order

1. Inventory every `Vm` field by owner and every callback/JIT mutation path.
   Keep the existing serial path byte-identical during this inventory.
2. Extract a worker-local execution context behind a small interface; keep
   coordinator behavior unchanged and add tests for nested worker/TLS lifetime.
3. Register worker frames, raw JIT cells, pending exceptions, and native roots
   with the stop-the-world root walk; prove forwarding/writeback under forced
   minor and major collections.
4. Replace the unsafe `WorkerVmRef`/`&mut Vm` envelope incrementally with a
   shareable runtime handle and explicit mutation APIs. A type-level `Sync`
   assertion is necessary but not sufficient; callback re-entry and generated
   inline stores must be included in the audit.
5. Implement concurrent class loading/initialization and Java shared-state
   synchronization according to JLS §12.4.2 and Chapter 17. Preserve recursive
   same-thread initialization, cross-thread wait/publication, erroneous-class
   behavior, monitors, and happens-before.
6. Admit only one complete worker segment whose reads, writes, exceptions,
   allocation ownership, and nested callbacks have executable proofs. All
   uncovered cases remain on the serial fallback.
7. Run focused semantic and moving-GC stress, then compare fresh isolated
   snapshot-free launches against `main`; measure launch-to-Done, CPU, and
   fallback/retry work separately. Do not claim `<5 s` from a microbench or
   worker-throughput result.

Runtime routing may depend only on structural proofs, ownership, synchronization,
and available workers. It must not depend on workload/class names, observations,
timing, prior executions, result hashes, or persisted boot state.

### Existing context seam from source review

Do not introduce a second `ExecutionContext` type. `vm/thread_context.rs` already
defines `ThreadContext` with worker-local roots, call stack, op accounting,
trampoline/pending-call state, green-thread state, and a TLAB view. Its worker
guard currently installs an empty context and only diagnostic call-stack access
uses it; most same-named fields remain on `Vm`. `SharedVm` is also only a type
alias for `Vm`, not a proof of shareability. These are migration gaps, not two
independent sources of truth to preserve.

The first extraction should promote that existing `ThreadContext` boundary
incrementally and keep the serial `Vm` path unchanged until each access is
migrated. `gc_protect`, pending/yield state, call stack, and per-execution op
accounting are execution-owned candidates. Loader/heap ownership, output,
network pumping, task queues, and class/static state remain coordinator/shared
state until they have explicit synchronization and root/publication contracts.
The former `exec_frames_ptr` field has been removed rather than copied into
`ThreadContext`. `frames::ExecScope` registers each active driver stack globally
for GC and separately in thread-local state keyed by `Vm` identity for
diagnostics. This preserves nested-driver visibility without making the
collector's root registry depend on a single last-writer pointer.

The first migration boundary is narrower than the eventual worker context:
retain the already-active call-stack routing, then route `gc_protect` through
the same current-context-or-`Vm` fallback and update every collector/root
validator before any worker may rely on it. Add `op_total` and GC thresholds as
write-both/read-`Vm` mirrors first; do not change checkpoint or network-pump
decisions in that pass. Defer `trampoline`, `pending`, `yield_request`, and
green-thread state: their driver swap/restore and loop-head suspend semantics
are behavior, not merely storage.

### Stop-the-world and region-publication invariants

- A coordinator requests a monotonically increasing stop epoch. Every running
  mutator polls at interpreter checkpoints, compiled backedges, allocation
  refill, and callback/resume boundaries. It publishes its thread-owned `Value`
  roots, JIT cell buffers/stack-map slots, active frame vectors, pending
  exception, allocation frontier, and unpublished write log before acknowledging
  that epoch. Poll coverage must be tested for each execution tier; the current
  operation-countdown in `frames.rs` does not imply that generated JIT loops
  poll.
- Root registrations own stable memory for the full published interval.
  Acknowledgement freezes that mutator's roots until collection completes;
  the coordinator scans every registered context, writes every forwarded
  pointer back, and only then releases workers to reload/use those roots. A
  context cannot unregister until it is quiescent and its roots are no longer
  reachable.
- No worker may park while holding a runtime lock required by root gathering,
  forwarding, or resume. Lock acquisition and class-init waits must themselves
  be safepoint-aware without exposing partially initialized state or violating
  Java monitor/happens-before behavior.
- Region abort is decided only after all workers have joined or acknowledged
  quiescence. Before retreating an allocation frontier, invalidate every handle,
  JIT cell, future/result, and raw pointer into the discarded region. Any
  unlogged shared write, untracked spill allocation, or unregistered root makes
  the segment ineligible; serial retry is not a substitute for undoing effects
  already made visible.
- Successful publication is one release-ordered commit after validation of the
  task result, allocation ownership, and shared-write set. Dependent tasks must
  acquire that publication before observing results. Failure leaves no task
  result or shared mutation visible and retries in original serial order.

The existing `parallel::stop_the_world`/`safepoint` tests establish only that
region threads park; they do not establish full-JVM mutator polling, worker-root
writeback, JIT register reload, or per-task transactional publication. Extend
those proofs before admitting a segment that can collect or mutate shared guest
state.

### Implemented groundwork (2026-09-26)

`WorkerThreadContextGuard` now boxes each active context and registers its
temporary-root vector for the entire guard lifetime; nested contexts remain
registered until their own guards exit. Focused tests cover nested registration
and reading a parked owner's root vector. The GIL-free interpreter checkpoint
also increments that worker context's `op_total`; coordinator counters and
checkpoint behavior are unchanged.

The collector consumes registered `ThreadContext.gc_protect` vectors in the
mark-sweep root seed, both moving arena root-writeback walks, and the stale-root
validator. The registry lock protects addresses against guard teardown; the
collector-side helper still has an unsafe contract requiring every owner to be
stopped. A focused root test proves writeback only while its owner is explicitly
parked, and the existing arena-string moving-GC regression still passes.

The full `Vm.gc_protect` producer/consumer surface now goes through safe,
clone-returning and stack-operation helpers: operations route to the active
`ThreadContext` or retain the coordinator's original `Vm` fallback. No
reference/iterator into TLS escapes.
The collector's Vm fallback walk stays explicit and worker roots remain a
separate registered source. The centralized
`push_call_roots` bridge still contains a transitional `&Vm`-to-`&mut Vm` cast;
this does not establish Rust aliasing safety for shared Vm execution.

This does not establish stop epochs or park active mutators, and it does not
register worker frames, pending exceptions, or JIT register roots. No worker-safe
guest segment is admitted. `RAVA_JIT_POLL=1` remains stop-only: it does not
spill/publish register roots, scan JIT frames, reload forwarded references, or
call a collector. Moving collection across active workers remains forbidden
until full mutator quiescence, root writeback, and JIT reload contracts are
implemented and tested.

### Current shared-view limit (2026-09-28)

`Vm` now aggregates only its already-synchronized memo tables in `SharedMemos`.
`RuntimeShared` is a `Send + Sync`, borrow-only view over that aggregate; it
exposes named lock-backed cache operations and a read-only `static_gen` load.
The generation bump remains a `Vm` method that enforces the non-worker fence
and refreshes the heap's native mirror. The view cannot reach the loader, heap,
JIT cache, GC scheduler, or execution state.

This still does not make a generic worker call safe. The interpreter loop
continues to consult `pending`, `yield_request`, and `trampoline` on `Vm`; the
interpreter and compiled-call paths still have transitional `&Vm`-to-`&mut Vm`
escapes. The worker audit found no admissible generic guest-code segment:
control-state migration, cast removal, cold-load closure, and a GC-free
execution proof are prerequisites. No worker admission or parallelism change
is part of this increment.

## First worker-segment admission audit (2026-09-26)

The repository already has one complete, bounded worker guest segment:
`gilfree_cf::run_batch` executes admitted compiled closure bodies with
worker-private `JitCtx` state, private allocation slices, COW barriers,
join-before-publication, and abort-to-serial retry. The measured rings lane
kept 127/127 tasks and sustained 4.0–4.67 cores; its interleaved product
comparison reported `Done` 34.319s off versus 27.122s on. The separate earlier
no-name-shortcut probe that completed 0/2 is not this admitted rings batch.

This is a bounded lane segment, not general shared-`Vm` execution. The separate
79-row deferred-task census closed no row across dispatch, class-load, and
write-set facts; it does not invalidate the rings result or prove those other
tasks safe. The associative-reduction fork is also safe for its narrow pure
arithmetic template, but bypasses `Vm` and is not a generic segment. Do not
drop `worker_locked_exec` for any uncovered callee based on either result.

`RAVA_JIT_POLL` only lets compiled loops park for stop-the-world coordination.
It does not publish JIT register roots or make collection safe. Workers still
must abort or serialize on uncovered callbacks, class-load misses, unsafe
shared writes, boxed results, exceptions, and allocation/refill exhaustion.
Generalizing beyond the existing admitted lane requires closing those effects
and publication paths structurally, not relying on identity or task history.

## Structural purity-admission smoke A/B (2026-09-26)

The default-off `RAVA_LANE_PURE_ADMIT` gate was tested in one paired product
launch from an isolated copy of `world_pristine`, using the same release binary
and the Linux A/B harness. OFF→ON launch-to-`Done` was 30.4s→28.6s and process
CPU was 49.9s→48.5s; the server's internal `Done` timers were 0.343s→0.375s.
One pair is directional only, not a promotion-quality result and nowhere near
the five-second launch target.

The OFF arm opened its first two-task worker region, then both tasks aborted
before completion on direct-call misses (`completed=0/2`). The ON proof refused
both plans before opening the region (`region=false`, `claimed=0/2`), preserving
serial fallback and avoiding that doomed worker attempt. This shows the gate
can refuse this unsafe batch; it does not prove any additional worker execution.

After timestamp and `Done`-timer normalization, both logs had identical stable
server markers (recipe/advancement counts, version, world preparation, and
completion). Both also reported zero persistent chunks, and this run contained
no 120-task ring batch. It supplies neither a content hash nor exact guest-state
parity for the measured ring workload. Keep structural purity admission
opt-in: promotion needs repeated A/Bs on the target workload, exact output
parity, and evidence that useful work is actually kept.

## Worker write-set and class-init boundary audit (2026-09-26)

The current bounded lane has a batch-level keep/discard vote, not a general
transactional write set. `JoinedRegion::finish` keeps results only when the
whole region is un-aborted and every task has a result; otherwise it retreats
the nursery, releases reserved object handles, then retries tasks serially in
order. Worker COW shadows are cleared by `WorkerJitCtxGuard::drop`; they are
not merged into shared guest objects. A task whose externally visible result
depends on a shadowed shared write is therefore not eligible to be kept.

The interpreter `putfield_value` path requests region abort before writing a
receiver outside the worker-owned ranges. Compiled stores enter the COW
callback through the worker-private armed `JitCtx`, but that barrier alone is
not proof that every generated store, array mutation, static write, callback,
or returned reference has a reversible effect. The existing deferred-task
census closes 0/79 rows across dispatch, class loads, and writes; admission
must continue to reject such bodies.

For class initialization, a GIL-free access to an uninitialized class requests
abort without setting `initialized` or running `<clinit>`. The region is retired
before serial `Vm::apply` performs the initialization in guest order. The
focused `refused_class_initializes_only_on_serial_execution_after_retirement`
test verifies this transition. Admission may rely on `clinit_done` only for a
class already fully initialized; it may not warm `<clinit>` to make a worker
plan eligible.

Thus the only currently justified keep contract is: worker writes stay inside
that worker's reserved/private objects, the result is publishable after join,
all shared observable effects are absent, and every cold-class/callback/GC
case aborts before publication. General shared execution still needs a
complete effect inventory plus either a proof of zero shared writes or a
root-aware transactional log with ordered commit and rollback. Neither exists
for the product deferred-task population; do not remove `worker_locked_exec`
or promote purity admission on this audit.

## Authorized shared-field transaction spike (2026-09-26)

The user authorized a shared-execution rewrite after the no-adapt/no-snapshot
worker-pool and pricing audits found no safe path under the existing contract.
This authorization permits investigation and a bounded tracer; it does not
change the <5s target or authorize relaxing unrelated guards.

The first candidate seam is a per-task field transaction, not general shared
execution:

- `FieldTxn` owns an ordered write log keyed by
  `(arena-object identity, field id)`. Only non-owned, non-pinned arena
  receivers may enter it. Handle-backed and pinned-mirror receivers remain
  fail-closed until their identities and canonical storage routes are proven.
- To avoid a costly and incomplete dynamic read barrier, transactional
  admission is write-only with respect to shared state: reject every shared
  instance-field read, mutable-static read, shared-array read, native/unresolved
  call, and constructor splice in the transitive bytecode closure. Reads of
  fresh task-local objects and arrays remain eligible. Unexpected runtime
  writes must fail closed before mutation.
- Worker tasks may therefore compute their result without observing another
  task's shared writes. Apply each task's writes in slot order, then publish
  its result and complete its future before advancing to the next slot. This
  preserves synchronous future-listener observations of the committed prefix.
- On any abort or failed precondition, drop every journal before nursery
  retreat and use the existing serial retry. The serial pilot remains an
  earlier committed operation and completes its listeners before the worker
  region starts.
- Before callbacks, root every pending task result, journal receiver, and
  reference-valued write in the coordinator's scanned root set. Preflight each
  task's destinations and values after prior listeners may have collected;
  re-read forwarded roots before applying that task's writes through the
  canonical non-allocating arena cell store. Commit itself must not throw,
  allocate guest objects, or execute guest code.
  The nursery reservation remains live through the ordered commit/completion
  sequence; on abort, drop every journal before nursery retreat.

The transitive analysis enforces the write-only subset, routes compiled and
interpreted shared `putfield` stores through the journal, excludes constructor
splice bodies, and drains per-task TLS state. Focused tests prove direct
commit-prefix visibility, moving-GC forwarding of journal roots, preflight
rollback without shared mutation, class-init gating, and cold/class-sensitive
rejections.

The worldless `ReloadBench` probe exercised the configured gate but did not
admit reload work: proof census showed the three-task registry batch at
`claimed=0/2`, `region=false`, with `write_set_closed=false`; the other reload
closures also left dispatch or write effects open. A clean opt-in run exited
successfully and preserved the exact deterministic hash
`c956377e19071d7a` (1,470 recipes, 1,584 advancements, 625 tags), but all work
completed through the serial path. One attempted hermetic A/B dropped its ON
arm on nonzero exit; there is no valid speed comparison and no launch-to-Done
measurement. Keep `RAVA_LANE_FIELD_TXN` default-off.

The registry batch's first reported transitive callee is
`LootDataType.registryKey()`, whose bytecode reads `this.registryKey` with
`getfield`. FieldTxn intentionally rejects every read from a shared receiver,
including final fields: the current proof establishes a write-only journal,
not a general shared-read consistency model. The other reload closures also
fail dispatch or cold-class-load checks, so admitting that getter alone would
not establish an executable transaction.

Still open: a real `LaneWorker` transaction through queued future
completion/listener observation, plus an abort after journalled worker writes
with exact serial-retry output. Until those gates pass, no worker completion or
product speedup is claimed.

## Authorized generic execution-kernel rewrite (2026-09-27)

After the bounded runtime candidates failed to approach the launch target, the
user authorized a major runtime rewrite while retaining exact no-adapt and
no-snapshot semantics. Three generic seams were compared:

- Unifying the guest heap/object representation is a performance no-go: the
  broad pinned/handle migration already ran and regressed boot by 1.4–2.2 s.
- Unifying the guest value/native-call ABI has useful leverage, but its
  evidence-backed ceiling is about 2–2.5 CPU-s, below the >7.6 CPU-s reduction
  required even under ideal six-core scaling.
- A tier-neutral execution kernel is the strongest architecture seam because
  interpreter, compiled, and native call routes converge on entry, rooting,
  exception, and safepoint policy. Its estimated savings overlap and remain
  unverified; they are not an acceptance claim.

The selected architectural seam remains a tier-neutral execution kernel, but
its first proposed vertical tracer—the out-of-line PIC target probe—is a
measured no-go. `probe_ways` and its fallback execute on every dispatch:
about 479.6 million times per boot. A direct-mapped fallback probe was reached
on only 1.57 million of 289.9 million PIC misses (0.54%), despite a 96.2% hit
rate when reached. Outlining the existing branchless target probe would add a
call, ABI setup, and result branch without removing work from the hot path.
No stub was implemented. The reusable 25.4%-of-CLIF-IR share is an emission-
time opportunity across roughly 9,500 sites, not evidence for a runtime call.

The replacement tracer is not yet selected. Any next candidate must identify
product-hot work that can be removed (not merely relocated), provide a
specific correctness/GC/exception contract, and be measured against the
>7.6 CPU-s ideal-scaling gap before implementation. No guest name, workload
history, timing, or output may steer routing. `FieldTxn` and its default-off
policy are unchanged. The <5 s acceptance target remains open.

The allocation/GC family was also audited as a possible replacement tracer
and rejected before implementation. The compiled arena-allocation path already
uses an inline TLAB bump with zero per-birth callback; the remaining handle
births are bounded by a measured ~0.82% of boot, and collections cost ~0.9%
of wall. The largest conceivable generic birth-path savings are ~0.7 CPU-s
(~2.0 CPU-s only by removing all measured slack), far short of the >7.6 CPU-s
ideal-scaling gap. Rust-side temporary allocations are numerous but diffuse;
multiple targeted removals already measured neutral. Do not rebuild TLAB,
allocator-policy, or generic SROA mechanisms without new product-coverage
evidence.

## Thread-local GC pressure marks (2026-09-28)

`gc_next` and `gc_next_real` now route through value-only
`thread_context::gc_thresholds` / `set_gc_thresholds` helpers. With no active
worker context, `Vm` remains authoritative and the existing rearm formulas are
unchanged. A `WorkerThreadContextGuard` starts with `usize::MAX` marks, so
threshold-driven `maybe_gc` and opt-in stream GC cannot collect or ratchet the
parked coordinator's marks from that context.

This isolates only the pressure marks; it does not make arbitrary worker GC
safe, prove that all direct `Vm::gc` calls are unreachable on workers, or admit
a generic execution segment. The callback's separate GIL-free TLAB-refill
behavior and the cold-load/write/ownership gates remain in force. Focused
coverage pins coordinator fallback, worker-local updates, and restoration.
