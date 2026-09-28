# RAVA runtime gap register

Verified against `main` at `ed20625` on 2026-07-21. This register records known
correctness, no-adapt, concurrency and performance gaps. A missing entry is not
evidence that an area is sound. Close a gap only with executable proof and the
acceptance evidence listed here.

## Status and severity

- `OPEN`: present in the current source.
- `IN PROGRESS`: an implementation exists only in an uncommitted branch/diff.
- `CLOSED`: fixed, tested and committed; include the fixing commit.
- `P0`: may execute the wrong program or violates the non-negotiable no-adapt rule.
- `P1`: blocks safe multi-core execution or the stated JVM performance target.
- `P2`: measured performance or memory opportunity without a correctness hazard.
- `P3`: maintenance, observability or documentation debt.

## Reference and completion bar

| Runtime | Wall | Peak RSS | Result hash |
|---|---:|---:|---|
| JVM reference | 13.764 s | 481.7 MiB | `8790f04ace726ecf` |
| RAVA canonical at `ed20625` | 93.735 s | 758.0 MiB | `8790f04ace726ecf` |

The project goal is not complete until a fresh canonical RAVA run is faster than
the JVM reference and uses less peak RSS, while correctness, stress and release
gates remain green. A single lucky run is insufficient: use interleaved fresh
processes and report every sample.

### Root-cause dig: the per-instruction gap is the unified-cell value model (2026-07-22)

A machcode-level investigation nailed WHY compiled guest code is ~6.8× slower
per core (correcting two wrong intermediate hypotheses along the way). Method:
`RAVA_PROF` (per-class compiled self-time) + `RAVA_JIT_DUMP` (CLIF/machcode of
the #1 hotspot) on a hash-exact canonical run.

- **The profile is PEAKED, not flat.** Compiled self-time: `DensityFunctions$Ap2.compute`
  19.7%, `BlockStateBase.initCache` 17.2%, `StateHolder.lambda$populateNeighbours$0`
  11.3% (top-3 = 48%). Worldgen density/shape nodes dominate.
- **The #1 hotspot's machcode** (`Ap2.compute`, 2697 CLIF lines): 334 loads,
  263 tag-ops (band 140 / select 69 / ushr 54), ~272 compares, 69 calls, and
  only ~8 muls + 2 f64const of actual FP work. The function signature is
  `(i64,i64)->i64` for a method that returns `double`.
- **NOT the causes first suspected:** field access is DIRECT (`load [obj+off]`
  at 8-byte strides — the arena flat-object model works; the "4-load handle
  indirection" claim was WRONG); there is ZERO stack-slot spill traffic (the
  "GC-spill convention" claim was WRONG); primitives (`D/F/I/J`) are stored RAW
  (`from_bits`, one bitcast, no tag).
- **The real cause: reference discrimination in the unified-cell value model.**
  Every value is an `i64` whose low 2 bits tag its representation
  (`0b00` arena-ptr / `0b01` heap handle / `0b10` array handle / `0b11` boxed —
  `jit::jit_abi::unify_untag`). 58% of loaded values feed a tag-op within 3
  instructions: every REFERENCE use must `cell & 3` → compare → select → shift
  to discriminate arena-ptr vs handle vs boxed. C2 has ONE uniform pointer
  representation and pays none of this. This is inherent to the dual-heap
  (handle-heap + moving arena) design. Plus 16 distinct baked table/mask
  addresses re-loaded 197× (descriptor/static/field-id tables), and a giant
  tier0 partial-inline (2697 lines) that multiplies the tagged scaffolding.
- **Magnitude matches:** ~7-10× more memory+tag ops per method (334 loads +
  263 tag-ops vs C2's ~30-50 direct loads) ≈ the 6.8× wall gap. The
  representation alone explains the gap; dispatch (69 calls = 2.5%), Cranelift
  codegen quality and multicore are secondary or gated by it.
- **This reconciles the LLVM-AOT result** (4×C2 on tag-free kernels, negligible
  boot-win): a heavier backend can't remove tag-discrimination that lives in
  the value REPRESENTATION, not the codegen.

**Consequence:** there is NO cheap localized patch for the #1 cost. Closing it
is the value/heap representation arc — unify the reference representation (arena
or handle-only, direct pointers, no tag discrimination) — the single largest,
highest-risk change in the codebase, and the prerequisite that gates both a
better backend and the multicore payoff. A bounded, reversible pilot (a
speculative arena-ref fast-path in the JIT that skips handle/boxed
discrimination under a guard, analogous to the heap-Ref dispatch PIC) is the
way to measure the arc's value before committing to it.

**Arc ran and CLOSED (2026-07-22, commit dd61495 + this update).** The pilot
above was built and run (`RAVA_ARENA_REF_PURELOAD`, and Stage 1 = a storage
cell encoding swap). Findings:
- **Stage 1** (storage cell → operand encoding): sound and bit-exact including
  GC stress, but SETUP_MS +3.8% (82.5 vs 79.5 s) → fully reverted. The decode
  RELOCATES, it does not vanish (a box guard survives: boxed refs live in arena
  fields, and the operand box table is per-frame transient vs the persistent
  storage `arena_box`).
- **CLIF re-measure** of `Ap2.compute`: band/select near a CALL = 0, near a LOAD
  = 64, neither = 84 → the tag-ops are the getfield STORAGE decode, not operand
  boxing at call boundaries.
- **Ceiling pilot** (skip band/classify, pure `load` on the arena arm): the
  process CRASHES at ~1.3 s on the FIRST class-provider lookup, before worldgen
  (`Provider 'java.lang.Object'`). Handle/box refs are LOAD-BEARING in arena
  fields from boot start, so the decode cannot be stubbed to isolate its cost —
  the arc admits NO bounded pre-measurement.
- **Ceiling triangulation:** tag-ops are ~26% of `Ap2.compute` CLIF ops, but
  they are ALU ops (`band`/`ushr`/`select`) + predicted branches that hide under
  the 334-load memory latency and 69-call overhead on an out-of-order core.
  Realistic ceiling of the full unification = single-digit-to-~10% wall at
  extreme heap-corruption risk (weeks, gated only by hash + GC stress, no
  pre-validation). Consistent with the completion-bar-reality section below and
  the prior ceiling map.

**Decision (user, 2026-07-22): CLOSE the arc.** Poor risk/reward; the payoff is
unknowable without building the multi-week big-bang, and every measurement
(Stage 1 null, pilot crash, ALU/OoO hiding) points to a low ceiling. Full
verdict in `docs/representation-unification-spec.md` / `-plan.md`.

**FINAL (2026-07-22, after the FULL build): the unification was constructed to
LIVE EXECUTION and the completion criterion measured unsatisfiable.** Across 19
gated commits (pinned region → dual-store mirrors → precise GC forwarding → five
identity/liveness/hash/map coexistence bridges → production flip → operand flip →
callback-force removal), handle references flow as raw untagged pinned pointers
through arena cells, JIT operands and compiled field ops — bit-exact
`8790f04ace726ecf` incl. GC stress, flags default-OFF (shipped path byte-neutral).
The interleaved 3×OFF/3×ON completion measurement: **ON repeatably SLOWER by
~1.4-2.2s in every pair, no overlap** (OFF 91.9/92.2/92.6 vs ON 93.4/94.3/94.4).
The representation's carrying cost exceeds its gain, which is ~0 because the
removed discrimination ALU was already hidden under the 334-load memory traffic
and the FILL hot path was already arena-form — the Root-cause dig's triangulation
CONFIRMED at the deepest level possible (full construction, not analysis). A wall
REDUCTION attributable to the representation is therefore mathematically
unavailable (the removed cost was zero), and shipping the flags default-ON is
forbidden by the arc's own no-regression invariant. See
`representation-unification-plan.md` for the full verdict.

**Prior investigation history (2026-07-22, pre-build):** Three levers were BUILT
and RUN, all reverted: Stage 1 (storage swap, +3.8% SETUP); B1 guardless getfield
(SIGSEGV — arena-eligibility is a `new`-site property, NOT a per-instance
guarantee: native/reflective/interning code allocates HANDLE instances of the
same final classes, so the arena-vs-handle guard is load-bearing and cannot be
removed at any site until every allocation path is migrated → no sound early
checkpoint exists); arena_box content-interning (bit-exact, but <0.5% RSS — the
leak is genuinely negligible). The representation space has no incrementally-
deliverable measurable win; its one real lever (handle→pointer) is un-derisk-able
and likely ~0 (loads dominate). Next: pivot to multicore / load-traffic axes.

### Completion-bar reality: the wall target is below the proven floor

Two independent 2026-07-17 analyses (an outside expert and the project's own
cpuprobe arithmetic) established that the wall target is **unreachable by
bounded structural work**, and the 2026-07-21 session re-confirmed every
mechanism behind that conclusion:

- **The gap is codegen quality, not a missing optimization.** A fresh canonical
  sampler attributes 67.9% of wall to compiled guest code (JIT_EXEC) and 20.7%
  to native bridges. RAVA compiles with Cranelift (a fast-compile,
  WASM-oriented backend: no escape analysis, SIMD/vectorization, GVN or
  advanced devirtualization); HotSpot C2 does all of those. No bytecode/CFG/
  ownership A/B closes a per-core codegen-throughput gap — it needs a
  C2-class backend or object-model rearchitecture (a multi-month arc; the
  existing LLVM-AOT path's boot win is negligible).
- **Multicore cannot bridge it either, and the fork arc is a canonical dead
  end.** RAVA's boot runs at 0.98 cores behind the GIL (MC's chunk pool
  collapses onto green threads). Even INFINITE cores hit an Amdahl limit
  (~107 s on the real boot; serial bridges/bootstrap/compile remain); the
  realistic multicore+representation floor is 3–5× JVM, still far above 1×. On
  the canonical workload specifically: the task-graph probe found ZERO
  executor substrate (GAP-A05 irrelevant here), and the only iterator-fork
  candidate (`initCache`) is irreducible (GAP-A01 — the `Comparable.equals`
  closure is the whole collection framework) AND capped at serial-divert
  (`method_fork_safe` forbids indy/athrow on workers), so even full proof
  coverage yields no canonical parallelism.
- **Every bounded lever is measured and exhausted** (see the P-entries): array
  interner (sub-noise, parked), salt map keys (boot-RAM only, wall-neutral on
  canonical), λ-PIC (implemented, A/B-refuted), collect/toMap (generic apply
  cost), dispatch host-falls (irreducible native floor).

Consequently the stated target (13.764 s wall) sits at or below RAVA's proven
achievable floor under the current backend and object model. Reaching it
requires a backend/codegen replacement or a full object-model + GIL-free
multicore rearchitecture — outside the bounded-A/B mandate — and even the
optimistic floor may not reach it. The RSS target (481.7 MiB vs ~758 MiB) is
likewise gated by the object model (many prior representation refinements
already landed). This section records the arithmetic so the completion bar is
not re-litigated from scratch; closing it is an architectural decision, not a
tuning task.

## P0 soundness and no-adapt gaps

### GAP-S01 — heterogeneous iterator dispatch used the first element

**Status:** CLOSED (2026-07-21, `fix(fork): prove exact dispatch per receiver`).

**Evidence (historical):** `Vm::try_fork_iterator_loop` derived `ecls` from
`elems[0]`; `Vm::try_parallel_apply_leaf` resolved one `Code`/compiled entry
from that class and applied it to every cell; the report-only
`Vm::fork_each_probe` made the same first-element assumption.

**Closure:** `Vm::fork_batch_target` resolves EVERY element's runtime class
under the GIL through the same `resolve_method` walk serial dispatch uses. The
batch admits only when each distinct class selects one identical bytecode
method (`Code` pointer identity); native, abstract, unresolved, null/untyped
and differing targets reject to the unchanged serial path. Every distinct
receiver class then independently passes the element-ownership offload proof
and the whole-tree-compiled proof (a shared root body does not imply shared
nested `this`-dispatch), and `try_parallel_apply_leaf` re-verifies the pin and
runs its structural fork-safety proof per class before taking a compiled entry.
`fork_each_probe` now requires one exact runtime class across all elements.

**Verified:** focused release tests cover heterogeneous subclasses serial,
homogeneous exact override admitted, override-mixed-with-inherited serial, two
interface implementors serial, inherited identical implementations
deduplicating without losing receiver coverage, abstract/native/unresolved/null
rejection and rename invariance (`fork_batch_tests.rs`); full release gate
green.

### GAP-S02 — call-tree proof followed constant-pool owners, not runtime targets

**Status:** CLOSED (2026-07-21, `fix(offload): native contracts + exact dispatch closure`).

**Evidence (historical):** `Vm::fork_tree_all_compiled`, `Vm::method_fork_safe`,
`Vm::fork_tree_has_new` and `Vm::fork_tree_has_newarray` recursed through the
member owner from the constant pool, so an abstract declaration could hide an
uncompiled, allocating, native or side-effecting override.

**Closure:** every virtual/interface edge in these proofs now enumerates the
exact dispatch closure over the loaded hierarchy (`virtual_dispatch_closure`,
normal runtime resolution, one representative per distinct `Code`): the safety
and all-compiled proofs require EVERY closure target to pass and fail on
unenumerable closures (interface owners, unloadable classes); the allocation
detectors OVER-approximate (an unenumerable closure counts as allocating).
Static/special edges recurse their exact owner. Native targets participate only
through worker-pure contracts (GAP-S03). Closure validity is epoch-scoped and
the fork re-checks the epoch at admission (GAP-S06).

**Verified:** a focused test proves an unsafe loaded override anywhere in the
closure rejects the tree while an exact `invokespecial` edge is unaffected;
closure enumeration/dedup/abstract/interface behaviour is covered separately;
full release gate green.

**Residual (tracked):** default methods and bridge/covariant-return edges rely
on `resolve_method`'s standard walk (interface owners currently fail closed);
duplicate names in separate loaders are GAP-S08; pinning targets for a worker's
lifetime is GAP-S07.

### GAP-S03 — arbitrary native methods were admitted by names

**Status:** CLOSED (2026-07-21, `fix(offload): native contracts + exact dispatch closure`).

**Evidence (historical):** `offload::native_pure`/`native_receiver_mut` were
class/method allowlists (including third-party guava/fastutil and blanket
enum-signature entries), consulted BEFORE bytecode resolution;
`Vm::fork_tree_all_compiled` treated any `ACC_NATIVE` method as worker-safe.

**Closure:** `native_contract.rs` holds typed declarative contracts
(`Pure`/`ReceiverMut` effect, `allocates`, `may_callback`) describing rava's own
Rust implementations, keyed by the EXACT receiver class and mechanically
checked (enum reads require `ACC_ENUM` on the exact class). The analysis now
resolves guest BYTECODE first — a resolvable body is always analysed, never
trusted by name — so a guest class under a JDK name gains nothing. Uncontracted
natives reject everywhere; worker contexts (`method_fork_safe`,
`fork_tree_all_compiled`) additionally reject allocating or callback-bearing
contracts. The guava cache and fastutil allowlist entries were removed (fail
closed); receiver-mutating contracts remain admissible only on receivers proved
private, keyed by the exact class.

**Verified:** focused tests cover contract-less rejection, ACC_ENUM mechanical
proof, JDK-name bytecode analysis, worker-context allocation/callback
rejection and the pure/receiver-mut split; full release gate green.

**Residual (tracked):** contracts live in one module rather than physically on
each `natives.rs` match arm; every entry names its serving implementation
family, and behavioural drift between a contract and its Rust implementation is
not yet machine-checked.

### GAP-S04 — optimistic fork gate bypassed proofs

**Status:** CLOSED (2026-07-21, `fix(fork): remove optimistic proof bypass`).

**Evidence (historical):** an `opcodes` env gate and its branches in
`Vm::try_fork_iterator_loop`/`Vm::try_parallel_apply_leaf` skipped offloadability
and all-compiled checks. Comments cited result hashes as the correctness arbiter.

**Closure:** the gate function, its CLI alias and every optimistic branch were
deleted. The element-ownership offload proof, the whole-tree-compiled proof and
the structural fork-safety proof are now unconditional; an unproved method or
tree always takes the unchanged serial path. The trust-and-guard worker mode,
the fat-worker nursery scaling that existed only for the optimistic mode and the
hash-as-arbiter comments were removed with it. Hashes remain verification
outputs only.

**Verified:** code search finds no optimistic fork gate; `cargo test --release
--features next-gen-gc` green after removal.

### GAP-S05 — abort history changed future admission

**Status:** CLOSED (2026-07-21, `fix(fork): remove abort-history admission`).

**Evidence (historical):** `Vm::fork_leaf_blacklisted`, `fork_abort_counts` and
a blacklist threshold permanently routed a leaf to serial after three observed
nursery overflows.

**Closure:** the per-leaf abort-count map, the blacklist predicate, the
threshold constant and the counting on the abort path were deleted. No abort
state survives an invocation: an aborted region retreats the reserved nursery
span, publishes nothing and retries serially for that invocation only.
Capacity is bounded before the fork from the current region extent
(per-worker view sized from the current free nursery, minimum-chunk floor
rejects to serial); an undersized view overflows, aborts before any
publication and never feeds back into admission. Remaining abort counters
(`ABORT_RAISED` etc.) are process-wide diagnostics that no control flow reads.

**Verified:** code search finds no fork abort-count storage;
`cargo test --release --features next-gen-gc` green after removal.

### GAP-S06 — shared virtual/interface calls could pass without exact devirtualization

**Status:** CLOSED (2026-07-21, `fix(offload): native contracts + exact dispatch closure`).

**Evidence (historical):** the invoke branch in `offload.rs` accepted
`callee(m.owner, ...)` — the DECLARED owner — before requiring an exact dynamic
receiver, so a shared receiver could run an unanalysed override.

**Closure:** the invoke branch now distinguishes exact targets
(`invokestatic`/`invokespecial` owners, fresh `new C` receivers, the fork's
concrete `this`) from shared-receiver virtual/interface calls. The latter are
admitted only through `Vm::virtual_dispatch_closure`: every DISTINCT resolution
over the currently loaded concrete subtypes of a CLASS owner must pass the same
proof; interface owners reject outright (lambda implementors are not enumerable
from the loader). Closures are valid per loader hierarchy epoch
(`ClassLoader::epoch`): the memoised offload/closure caches clear on any class
load, closure computation runs to an epoch fixed point, verdicts derived across
an epoch move are not memoised, and both consumers re-check the epoch at the
consumption boundary (fork admission stays serial for that invocation; the
call-scoped `toString` memo refuses to cache). The relaxed ceiling probe keeps
owner-based acceptance as a diagnostic upper bound only.

**Verified:** focused tests cover declared-owner rejection vs closure
admission, closure enumeration/deduplication/abstract exclusion, interface and
unloadable owners rejecting, and late-loaded overrides invalidating the
memoised closure; full release gate green.

**Residual (tracked):** pinning the closure for a WORKER's lifetime (beyond the
admission-to-consumption window) is GAP-S07's freeze obligation.

### GAP-S07 — no dispatch freeze or compiled-entry lifetime proof

**Status:** CLOSED (2026-07-21, `fix(fork): freeze dispatch for worker lifetime`).

**Evidence (historical):** `Vm::try_parallel_apply_leaf` copied a raw compiled
pointer and `*mut Vm` into a worker payload with no hierarchy epoch, JIT pin or
guard preventing class definition, `<clinit>`, cache publication or
compiled-entry invalidation until join; no-alloc trees even ran workers without
the gil-free dispatch guard.

**Closure:** three enforced layers. (1) PREVENTION — every fork worker now runs
gil-free with the uncompiled-callee dispatch guard armed (resolution,
compilation, `<clinit>` and interpreter fallback abort the region before any
publication), and `ClassLoader::get_or_load` fails closed on any gil-free
thread's cache miss, requesting the region abort (this also covers the S2
chunk-fill and pool workers, which all set the gil-free flag). (2) PINNING —
`jit_cache` `Compiled` entries and loader cache entries are append-only for the
process lifetime (debug-asserted at the only compile-publication site), so the
pinned `fptr` and `Arc<LoadedClass>` metadata cannot dangle; no GC runs inside
a region (workers abort on nursery exhaustion; the coordinator is parked).
(3) VALIDATION — the loader hierarchy epoch is captured at region open and
re-checked after join: a moved epoch discards the region exactly like a worker
abort and retries serially (idempotent under the admission proof's
deterministic disjoint element writes). Worker throws/panics are caught and
converted to the abort; regions do not nest on this path.

**Verified:** focused tests prove a gil-free thread's class-load miss fails
closed and requests the abort while cache hits stay readable; full release gate
green.

**Residual (tracked):** replacing `*mut Vm` with a narrow typed `WorkerContext`
is GAP-S09/GAP-A02.

### GAP-S08 — class-loader identity and initialization ordering are not explicit

**Status:** CLOSED (2026-07-21, `fix(loader): pin one identity per class name`).

**Evidence (historical):** fork helpers pass textual class names through
`resolve_method` with no explicit loader identity or initialization epoch.

**Closure:** the runtime has exactly ONE flat class namespace by construction:
every guest `ClassLoader`/`URLClassLoader` is served by adding sources to the
single `Vm::loader` (`reflect.rs` routes `loadClass` through it; `defineClass`
is not served), sources resolve first-wins like a JVM flat classpath, and a
published name→class binding is never replaced — `get_or_load` inserts only on
a miss and `define_synthetic` now returns the existing identity instead of
overwriting. Textual-name aliasing across loaders is therefore impossible, so
a textual name plus the resolved `Code` pointer plus the hierarchy epoch
(GAP-S06/S07) IS a complete loader-qualified method identity. Initialization
ordering: no admission proof runs `<clinit>` — `resolve_method`,
`fork_batch_target`, the offload/closure analyses and the tree proofs never
call `ensure_init`, and the whole-tree-compiled requirement means every
admitted body already executed (hence initialized) in normal program order.

**Verified:** focused tests prove a second definition under a resolved name
cannot replace or alias the original (identity and method bodies stable) and
that running every admission proof over a `<clinit>`-bearing class leaves its
initialization state untouched; full release gate green.

### GAP-S09 — worker fallback can touch shared mutable VM state

**Status:** CLOSED (2026-07-21, staged per `docs/worker-context-plan.md`:
instrumentation `21f4ac5` + debug guards `0328d01` + plan `87c80d2`, then
stage 1 `a55bb3f` `refactor(fork): type worker VM handles`, stage 2 `b026887`
`fix(fork): shared-borrow callbacks, worker fault isolation`, stage 3
`65eae35` `fix(fork): abort stubs for worker-illegal callbacks`, stage 4
`f843567` `fix(fork): mode-aware vm_ref + WorkerContext RAII`, and stage 5 —
this commit — quarantining the interp-on-worker experiments and retiring the
column fork).

**Evidence (historical):** fork workers carried a raw `*mut Vm`; compiled-code
callbacks resolved an unrestricted `&mut Vm` through `jit::vm_ref()`;
`rava_putstatic` on a gil-free worker was counted, not aborted (statics are not
COW-shadowed — live hole 1); kind-A apply-leaf workers never installed a
private `JitCtx`, so a worker fault latched the SHARED fault word (live hole
2); and three interpreter-on-worker experiment arms ran `opcodes::run` against
the coordinator's `Vm` under discipline-only comments.

**Closure:** worker `Vm` access is now typed and mode-checked end to end.
(1) OWNERSHIP — `parallel::WorkerVmRef` replaces every fork payload pointer
(`GilVmToken` types the GIL-held daemon handles), and the RAII
`parallel::WorkerContext` performs the kind-A worker enter/teardown (TLS,
gil-free flag, region, TLAB view, private `JitCtx`) on any exit including
panic. (2) READS — the re-pointable callback group migrated to `jit::vm_shared()`
(read-only, signature-proven) plus TLS. (3) FAULTS — every worker installs
`WorkerJitCtxGuard`, so compiled-code faults latch the worker-private word
(hole 2). (4) MUTATION — the worker-illegal callbacks (putstatic, publishing
indy, cold ldc, `fu_invoke`, map/amap and enum/bitset receiver mutators,
out-of-region anewarray) are abort stubs on gil-free threads: abort requested
BEFORE any publication (hole 1), honoring the locked-exec envelope exactly like
the uncompiled-callee guard. `indy` joined `ldc` as a SPLIT gate rather than a
blanket denial (GILFREE-RINGS): only sites that would publish are denied —
cold resolution (bootstrap walk + impl-identity interning), the non-capturing
per-site singleton, and the arity-1 dedup map. A capturing lambda whose impl
identity is already interned mints its closure entirely inside the worker's own
pre-reserved slices (`opcodes::worker_indy_refusal` names which check refused). (5) DENIAL — plain `vm_ref()` is mode-aware: on a
gil-free worker outside the locked-exec envelope it tallies, requests the
region abort and returns `None` (debug builds panic naming the callback); the
few sanctioned `&mut` reaches route through `vm_ref_worker_sanctioned()`, each
with a per-site soundness note (grep-enumerable). (6) QUARANTINE — the
interpreter-on-worker experiments are constructible only through the loud
`WorkerVmRef::vm_mut_unsound_interp` escape behind explicit default-off env
gates with QUARANTINE doc blocks placing them OUTSIDE this claim
(`RAVA_FORK_NUMERIC` counted loop; `RAVA_M3_CHUNK_FILL_INTERP` for the S2
`s2 == None` interp arm); the `RAVA_FORK_COLUMN` column-fork prototype was
RETIRED outright (function, opcode hook, gates and CLI aliases deleted —
superseded by the S2 compiled-body fork). The claim covers the default
configuration and the typed compiled-worker surfaces; the quarantined arms
remain hash-arbitrated experiments.

**Verified:** instrumented acceptance tests pin a real compiled reduction fork
with live counters — zero class loads, zero compilations, zero worker dispatch
misses, zero abort-stub hits and zero `vm_ref` denials across the region
(`s09_workers_do_zero_resolution_or_compilation`,
`s09_census_gilfree_counts_no_frozen_mutators`); negative tests prove worker
putstatic/map-put abort without publication, the kind-A fault lands in the
worker latch, and a gil-free plain `vm_ref()` is denied while `vm_shared()`
keeps working (`s09_worker_illegal_*`, `s09_kind_a_worker_fault_lands_in_worker_latch`,
`s4_gilfree_plain_vm_ref_denied_and_aborts`); debug mutation guards
(`debug_assert_not_gilfree`) cover the frozen-field contract of
`docs/gil-free-vm-layout.md`; full debug and release suites green.

**Residual (tracked):** merging the S2 chunk-fill worker's hand-rolled
choreography onto `WorkerContext`, moving the remaining execution-local `Vm`
fields to TLS and shrinking the sanctioned `vm_ref_worker_sanctioned` surface
— typed-interpreter hygiene, re-filed under GAP-A02.

## P1 architecture and performance blockers

### GAP-A01 — real workload remains effectively single-core

**Status:** OPEN.

**Canonical sound-fork sizing (2026-07-21, hash-exact run with
`RAVA_FORK_SHAPES`+`RAVA_FORK_PARALLEL`+trace):** the now-sound iterator fork
saw 712 admission candidates and admitted ZERO. Every rejection is the
`initCache()V` per-element loop failing the element-ownership offload proof at
`callee Block.getFluidState` (a virtual edge whose dispatch closure over the
loaded Block hierarchy does not prove out — the exact population the removed
optimistic bypass used to wave through unsoundly). The path to a sound
canonical fork is therefore PROOF-COVERAGE work: drill the full rejection
chain (not just the first callee) and extend the native-contract/closure
machinery until the real blockers are either proved or shown irreducible.

**Drill frontier (2026-07-21, after iterations 1–4 — concat-indy/fresh-athrow/
Class.cast, throwable-ctor contract, unloadable-owner fallback, JDK_SUPERS
Boolean/Character rows):** the remaining `initCache` leaves are virtual calls
whose DECLARED OWNER is an unloadable JDK **interface** —
`java/lang/Comparable.equals` (StateHolder.setValueInternal; receivers are
property values: guest enums / boxed Integer / Boolean) and `java/util/List.get`
(LiquidBlock.getFluidState). These are IRREDUCIBLE under the current machinery,
for a structural reason the existing levers cannot reach: the unloadable-owner
contract fallback keys the contract on the OWNER, which is only sound when the
owner names the exact native family (`java/lang/Boolean` → the boxed natives).
An interface owner names NO implementation — a blanket Pure contract on
`Comparable`/`List` would claim purity for EVERY implementor, including guest
bytecode (enums with user `equals` overrides, guava/fastutil lists) — unsound,
refused. Closing these soundly needs new machinery, one of: (a) receiver
CONCRETE-type evidence at the call site (flow-sensitive typing of fields/args —
the analysis today only devirtualises `this` and fresh `new`); or (b) an
enumerable closure for unloadable-interface owners = loaded guest implementors
(epoch-guarded, already buildable) ∪ lambda-`$fi` refutation (a lambda can BE a
`Comparable`) ∪ a registry of exact JDK classes rava natives can mint instances
of (does not exist today — would let the closure bound the unloadable-receiver
population). Until then `initCache` stays serial. Note the cap even after
admission: `method_fork_safe` hard-forbids invokedynamic and athrow on the
PARALLEL (worker) path, and the proven `initCache` subtree contains the
StateHolder error-path concat-indy + athrow — so full proof coverage of this
chain yields SERIAL-DIVERT admission only, not a parallel fork.

**Design (b) MEASURED AND REFUTED (2026-07-21, `RAVA_OFFLOAD_DRILL` Gate-1
diagnostic, hash-exact drill5 run).** The interface-closure enumerator
(`interface_dispatch_closure` + `name_may_subtype`, wired diagnostic-only —
no admission change) sized the `Comparable.equals` closure at **840 loaded
concrete implementors, 737 failing**, and each failing `equals` recurses into
FURTHER unloadable-interface leaves (`DoubleCollection.equals`,
`java/util/List.size`, `java/util/Map.size`, `Multimap.size`,
`Reference2ObjectMap.size`, … — each itself 800+ targets). `Comparable` is
implemented by essentially the entire loaded library surface (every enum, every
boxed type, the whole fastutil/guava collection framework); proving all 840 (and
transitively the collection framework's `equals`/`size`/`hashCode`) is not
bounded work and many targets have genuine blockers. Design (b) is therefore
NOT viable for this leaf. The honest structural conclusion: the canonical
`initCache` fork is guarded by the collection-framework equality closure, an
irreducible surface for closure enumeration. Only design (a) — flow-sensitive
receiver typing that proves the ACTUAL narrow receiver at
`setValueInternal`'s `Comparable.equals` (a single property value: an enum or
`Integer`), not the whole interface closure — could close it, and even that is
capped at serial-divert. The Gate-1 diagnostic is retained as the drill
instrument; the closure was NOT wired into admission.

The earlier `docs/gil-free-vm-layout.md` census recorded one GIL acquisition and
no real guest OS worker threads for that workload. A fresh administrator ETW
capture of the ordinary `server.jar` boot (2026-08-02, PID 26580, diagnostic only)
refines this: the final ring phase does run on four OS workers, each consuming
21.6-22.0 CPU seconds concurrently, while the coordinator consumes 56.215 CPU
seconds. The log reaches `Environment:` after about 31 s and the server-start /
recipe marker after another 22 s. Thus roughly 53 s of wall is serial before the
worker phase even begins. Removing the worker lock alone cannot satisfy
`wall < 20s`; the P1 architecture must also move or radically reduce this
generic coordinator invoke/dispatch path.

The same ETW stack report puts `rava_fu_invoke` on 74.08% of inclusive samples,
with `resolve_lock_acquire` and a contended mutex at 47.53% and 46.97% across
the process. On the coordinator, the leading paths are `Vm::jit_call` /
`jit_call_body` (about 35% each), `rava_dispatch_miss` (29.8%),
`invokevirtual` (29.4%), and native owner dispatch (about 14.5%). This is
diagnostic attribution, not a production timing claim; the full record and
reproduction command are in `docs/lane-locked-residue.md`.

Close with detached task plans, exact roots, bounded queues and at least five
fresh-process runs showing CPU above one core and a wall-time median outside
noise. Do not map arbitrary guest threads to OS threads without scheduler,
exception, join and GC-root proofs.

### GAP-A02 — `Vm` is not yet a capability-safe worker boundary

**Status:** OPEN.

Execution-only state is only partly moved to TLS; frozen fields rely on runtime
discipline plus the GAP-S09 debug guards. Finish the field contract in
`docs/gil-free-vm-layout.md`, keep every TLS root visible to both collectors
and expose only the capabilities a detached kernel needs. Re-filed here from
the GAP-S09 closure: merge the S2 chunk-fill worker's hand-rolled choreography
onto `parallel::WorkerContext`, shrink the sanctioned
`vm_ref_worker_sanctioned` surface toward zero, and either type the
QUARANTINED interpreter-on-worker arms (`RAVA_FORK_NUMERIC`,
`RAVA_M3_CHUNK_FILL_INTERP` — today they run through the loud
`vm_mut_unsound_interp` escape, outside the S09 claim) or retire them like the
column fork.

### GAP-A03 — allocating parallel tasks lack general transactional publication

**Status:** OPEN.

Private nursery/array reservations exist for narrow fork paths, but arbitrary
allocating task graphs lack a general commit protocol. Partial objects, side
tables, remembered-set entries, exceptions and statics must remain invisible on
abort. Close with private regions, exact capacity/ownership proof and one atomic
publication phase under the collector protocol.

### GAP-A04 — shared old-heap concurrency is unproved

**Status:** OPEN.

COW and disjoint-element experiments do not make the heap generally concurrent.
Shared reference stores require ownership, barriers, root publication, GC phase
coordination and Java memory-model semantics. Unknown shared mutation remains
behind the GIL.

### GAP-A05 — executor/ForkJoin/CompletableFuture routing is incomplete

**Status:** OPEN — but MEASURED IRRELEVANT to the canonical completion bar
(2026-07-21).

A bounded resident host runtime exists, but structurally recognized guest task
graphs are not generally lowered to detached, proof-carrying work. Completion,
throw, cancellation, join, callbacks and saturation must preserve the existing
serial semantics.

**Canonical sizing (`RAVA_TASKGRAPH_PROBE`, hash-exact run):** every routing
counter is ZERO — resident=0, detached=0, inline=0 across all nine seams
(executor execute/submit/invokeAll/schedule, CF supplyAsync/runAsync/callback,
FutureTask.run). The canonical ChunkFill setup is a plain serial call graph
with NO task-graph structure to route; the recorded 48.5% supplyAsync share is
a REAL-BOOT property only. Executor routing therefore cannot move the
canonical wall at all; on canonical the only parallelism substrate is generic
loop/fork admission over the serial guest code (the now-sound iterator/S2
machinery).

## P2 measured optimization opportunities

### GAP-P01 — array element descriptors are repeatedly resolved and hashed

**Status:** MEASURED AND PARKED (2026-07-21) — opportunity below the noise floor.

The tracer (`RAVA_ARRAY_INTERN_STATS`, inert counters in
`heap::array_intern_stats`, reported on `Heap` drop) measured one full
canonical `ChunkFill 1 64` (hash exact, SETUP_MS=81104, FILL_MS=19861):
3,369,006 primitive + 4,002,327 reference interner lookups (257 misses) and
1,202,961 per-allocation `L…;` descriptor strings built by the two `anewarray`
sites. At realistic per-op costs this bounds the whole opportunity at roughly
0.3–0.7 s of an 81 s setup — under 1%, inside the multi-second fresh-process
spread, so a structural ID-based allocation API cannot produce a repeatable
median outside noise and is NOT implemented (the mandatory A/B gate would
reject it). Revisit only if a future change multiplies array allocation
volume. The tracer stays for re-measurement.

### GAP-P02 — compiled guest code and native bridges dominate remaining wall

**Status:** OPEN (attribution refreshed 2026-07-21).

Fresh canonical sampler (hash exact, wall 99.4 s): JIT_EXEC 67.9%,
NATIVE_BRIDGES 20.7%, INTERP_EXEC 6.5%, VM_DISPATCH 4.7%, CLASS_LOADING 0.2%.
Per-bridge self-time on the same build (RAVA_BRIDGE_STAT, stat-inflated run,
37.8 s bridge self-time total): `Stream#collect` 7.42 s / 324 calls (~23 ms
per call), `Stream#map` 6.52 s / 131 k calls, the `String#valueOf` family
≈7 s / ~1.3 M calls, `LinkedHashMap#get` 2.25 s / 460 k calls (~5 µs each),
`Map$Entry#getKey/getValue` 2.4 s / 4.2 M calls. JIT admission is healthy
(11,057 methods, zero compile failures, no cap saturation). The dominant
COHERENT cause behind collect/valueOf/map-get is GAP-P04 below.

Do not resurrect the rejected direct Stream-map/output-array designs.

**Collect attribution (2026-07-21, `RAVA_COLLECT_PROBE`, hash-exact run):**
`toMap` is the whole bucket — 1,988 calls, 9.08 s inclusive, 61,943 elements
(~146 µs/element, max 189 ms/call); `custom`/`joining`/`list` are negligible.
Per element the path runs two guest applies (keyfn/valfn) plus a full
name-dispatched map `put` with key derivation. The salted-key gate did not
move the wall, so the time is dominated by the guest-apply/dispatch overhead
itself (~8 µs per Rust callback, the recorded S1 wall) — the reduction lever
is generic apply/dispatch cost, not collect-specific mechanics.

### GAP-P04 — value-object map keys are derived through full `toString` trees

**Status:** OPEN; next recommended structural target (the largest measured
single lever left: an unsound scaffold measured −28.4 s on the 259.5 s-era
boot, ~11%).

**Evidence:** `map::key_str_mut`/`structural_key` key every value-equality
object (a class with a substantive `equals` override) by its full guest
`toString()`. The pathological keys are FRESH worldgen density trees and DFU
type templates probed against dedup caches — 9 KB–362 KB of string built per
key, unmemoizable by construction (fresh objects each probe). This is what the
fresh bridge profile shows as `Stream#collect`, `String#valueOf` and
`LinkedHashMap#get` time. The `RAVA_MAPKEY_HASHCODE` scaffold (guest
`hashCode()` as the key) measured the −28.4 s prize but BREAKS bit-exactness:
Java `31*a+b` hashes collide on structured worldgen params, aliasing unequal
keys (`aa01…` ≠ `8790…`) — recorded, not theoretical.

**Sound design (equals-verified salted index probing), to implement behind
`RAVA_MAPKEY_SALT` (default off):**
1. Key a value-equality object as the string `o:gS{class}:{hc}#{salt}` with
   `hc` = guest `hashCode()`. Salt derivation needs NO representative storage:
   probe the map's OWN index for salts `0..next_salt` of `(class, hc)`; on an
   index hit, verify with guest `equals(k, entries[i].key)` against the LIVE
   entry's stored key object — equal → that salted key; unequal → next salt;
   absent salts are skipped (removed entries leave holes, never reused); no
   equal hit → mint `next_salt` and bump it. The only new state is a per-map
   monotonic `(class, hc) → next_salt` counter map — removal-safe (counters
   never decrease, so a hole can never alias), zero GC impact (entry keys are
   already map roots), and deterministically rebuildable from the salted key
   strings wherever the index is rebuilt (snapshot restore; wire unchanged).
   This mirrors Java HashMap semantics (hashCode bucket + equals membership)
   exactly — semantically MORE faithful than the toString proxy — and `equals`
   short-circuits where `toString` builds the whole tree.
2. KEYING-MODE CONSISTENCY per map: every derivation for a given map must go
   through the same mode. `map_key` (map.rs, has the owner handle) salts;
   `value_map_key` builder/drain/copy paths must receive the target map (or the
   builders' maps must opt into toString mode uniformly) — a map probed through
   both modes would silently miss its own entries.
3. Fallbacks stay exact and deterministic: `hashCode`/`equals` RESOLUTION
   failure (no bytecode target — deterministic per class) → the current
   toString path for that class's keys; a guest THROW from `hashCode`/`equals`
   PROPAGATES (exactly what a real `HashMap.get/put` would do — never swallow
   into a different keying); non-map contexts → toString path.
4. ⚠ Cross-object STRING comparisons of structural keys exist and are
   incompatible with per-scope salts: `map.rs:1345`, `map.rs:1672` (value
   equality via `structural_key(x) == structural_key(y)`) and the guava-cache
   keys in `cache.rs` — these sites must either keep the toString derivation or
   route through guest `equals` directly. Ref-arm `$keystr` and the arena
   VALKEY memo are cross-map caches and MUST be bypassed under salt keying.
5. Both representation arms (heap `Ref` and arena `Obj`) must key identically
   (the proven both-arm parity pattern from the hashCode scaffold).

**Acceptance:** focused tests (equal keys converge to one salted key; unequal
same-hash keys split salts; remove/re-insert stability; GC forwards chain
representatives; Ref/Obj parity; fallback exactness), then ≥3 interleaved
fresh-process canonical pairs with EXACT hash `8790f04ace726ecf` and a
repeatable median win outside noise before default-on.

**Measured verdict (2026-07-21, implemented behind `RAVA_MAPKEY_SALT`; gate
stays OFF):** five salted canonical runs were ALL BIT-EXACT
(`8790f04ace726ecf` — the soundness design holds where the raw hashCode
scaffold diverged). Refinement steps and their effects: bare salting peaked at
1197 MiB WS; adding the per-object hashCode memo (`$hashc` field / arena epoch
memo — sound, equals still verifies every index hit) and the converge-only
salted-key memo (arena-only identity: heap handles are recycled without an
epoch bump and MUST NOT key an epoch memo) brought it to ~900 MiB WS /
1303 MiB PM, stable across three runs; moving counter updates from mint-time to
INSERT-time (`note_salted_key` in `heap::map_put`; mints store nothing, two
uninserted mints may share a salt harmlessly) plus 1M-entry memo caps changed
neither number. Walls: 94.2–102.9 s across salted runs vs the 97.7–101.9 s
baseline band — neutral within fresh-process noise, as expected: ChunkFill's
key-derivation share is only ~7–10 s (the −28.4 s prize was measured on the
real-boot workload, where the DFU key churn dominates). The residual
+145 MiB WS / +241 MiB PM is NOT the salt counters, NOT the memos (capped),
and NOT extra JIT code (11,091 vs 11,057 methods); remaining suspects are
`$hashc` FieldBlob growth on transient heap keys and allocator high-water from
guest hashCode/equals invocation churn (records resolve through indy linkage
per call).

**RSS attribution (census pair, both hash-exact):** the +145 MiB is a
GC-PACING shift, not a leak. At the OFF peak the census shows 1,730 live maps
/ 33 k entries / malloc_live 398 MB / old arena 24 MB; at the ON peak 5,265
live maps / 198 k entries / malloc_live 514 MB / old arena 64 MB — MORE LIVE
UNCOLLECTED STATE. Cause: `Heap::bytes` charges map entries by key length;
salted keys (~40 B) accrue pressure far slower than the multi-KB toString keys
they replace, so budget-driven collections fire later and the high-water rises.
A default-on salt therefore needs a pacing compensation (e.g. charging a
derivation-independent entry estimate) — an accounting change to A/B on its
own.

**Real-boot A/B (2026-07-21, `bench/boot_ab.ps1`, three interleaved cached-world
pairs, all six boots reached Done):** peak WS 1235.2/1233.1/1235.2 MiB OFF vs
771.0/769.4/769.0 MiB ON — a REPEATABLE **−466 MiB (−37.7%)** with under 2 MiB
spread (peak private −509 MiB): the multi-KB DFU toString keys really were
resident. Walls 230.4/238.7/244.3 s OFF vs 247.1/245.0/242.7 s ON — pairwise
deltas +16.7/+6.3/−1.6 s (median +2.6%, mixed sign): the historical −28.4 s
raw-hashCode prize does NOT materialize with equals-verified probing on the
current (already ~135 s faster) boot. VERDICT: the gate stays default-OFF
(the canonical completion-bar workload loses ~145 MiB RSS to GC pacing under
it), but `maps.salt_keys` is now a documented opt-in BOOT-RAM lever
(config alias added). Default-on reconsideration needs the GC-pacing
compensation plus a canonical-neutral wall series.

**Rejected compensation (2026-07-21):** charging salted entries a 512 B
`entry_bytes` floor (hash-exact run) left the canonical peak at 899 MiB —
unchanged. The delayed-collection driver is therefore NOT the heap map-entry
budget; the census delta points at the ARENA side (old arena 64 vs 24 MB at
peak), i.e. arena collection pacing under derivation churn. The floor was
reverted (ineffective and dishonest accounting). A future compensation must
target arena GC cadence, not `Heap::bytes`.

**Why the salted `hashCode()` was not cheaper than the `toString()` it replaced
(2026-07-26, root cause):** for a RECORD key — which is what the worldgen density
graph and the DFU templates are — `hashCode()` links to
`ObjectMethods.bootstrap`, and rava serves that call site in
`opcodes::record_object_method`, where a REFERENCE component is folded as
`value_to_string(component)` then `chars().fold(31*a+b)`. The synthesized
`equals` likewise compares reference components by
`value_to_string(a) != value_to_string(b)`. So the salted path builds the very
`toString` tree the salt exists to avoid, twice per equals verification. That,
not the probing, is why the −28.4 s raw-hashCode prize did not materialize.
A record's `hashCode`/`equals` should recurse into the component's own
`hashCode()`/`equals()` (an int fold, and an early exit on the first differing
component) — JDK-authoritative, generic, and a prerequisite before
`maps.salt_keys` is re-measured for wall.

**Per-class attribution of the derivation (2026-07-26, `RAVA_RESIDUE_PROBE` on
`bench/RsProbe.java`):** the derivation flattens a DAG, so a shared node
contributes its whole subtree text once per parent and every level copies its
children's text again. One `RandomState.create` builds **~21 MB** of key text
over ~238 k guest `toString` renders. Average rendered key length by owner:
`DensityFunctions$Ap2` 98,022 B (85 roots), `Pair` 18,344 B (846),
`types/templates/Sum` 16,765 B (289), `Marker` 1,470 B (3,435),
`HolderHolder` 1,873 B (1,795), `Spline` 78,518 B, `RangeChoice` 66,174 B,
`FindTopSurface` 367,953 B, `BlendDensity` 651,441 B. The existing per-call
stringify memo could not collapse ANY of it: the offload analyzer hard-rejects
every bootstrap except `StringConcatFactory`, so a record's synthesized
`toString` was never proven pure and was never cached.

### GAP-P06 — hash-consed value-object map keys (`RAVA_NO_KEY_INTERN` disables)

**Status:** SHIPPED, default ON (real-boot A/B closed 2026-07-31). The mechanism
is generic: admission depends on render purity, nesting and text only, never on
the jar, class name, workload, hash or timing.

**Change:** (1) admit the `ObjectMethods.bootstrap` record accessor as a pure
render — rava serves that call site itself, reading the record's own final
components and building ONE fresh String, exactly the pure-alloc shape already
modelled for `StringConcatFactory`; the transitive component condition is checked
dynamically by an impurity mark over the ACTUAL values rather than statically
over declared types. (2) Inside a key derivation, every NESTED render of ≥48 B
collapses to a fixed-width token naming its interned text, and a per-GC-epoch
arena-address memo skips the render entirely for a node already derived this
epoch.

**Injectivity:** inductive and exact — equal tokens imply equal token-form texts
imply equal full texts, so the scheme never MERGES two keys the string form kept
apart. It can only SPLIT two objects whose full texts coincided through
differently shaped renders. Tokens start with `\u{1}`, which no real `toString`
emits, and are fixed width so none is a prefix of another. Substitution is
admitted only inside a chain of renders that are ALL proven pure, and never at
render depth 0 (a depth-0 render under a key scope is not necessarily the key's
own — the derivation also runs guest code).

**Measured on the HOST-BOUND baseline (`bench/RsProbe.java`, rebased onto
`e7ae4a1`, two interleaved pairs, P-core affinity, `RsProbe_HASH` equal to the
JVM's `abf4a3ebc07412f` in all four runs):**

| item | OFF | ON | JVM |
|---|---:|---:|---:|
| `RandomState.create` #1 | 2800 / 2596 ms | 772 / 751 ms | 37 ms |
| `RandomState.create` #2 | 2349 / 2336 ms | 500 / 498 ms | 7 ms |
| `RandomState.create` #3 | 2424 / 2407 ms | **445 / 448 ms** | 5 ms |
| `Bootstrap.bootStrap` | 58783 ms | 51152 ms | 3559 ms |
| `VanillaRegistries.createLookup` | 6312 ms | 5790 ms | 251 ms |

Steady state is **−81%** (5.4x); the ON arm is stable to ±3 ms across pairs while
the OFF arm is not, because the work removed is exactly the part that scaled with
the graph. This lands on the host side of the reframe: the derivation is string
building, hashing and allocation in Rust, not compiled-code quality. Key text
`DensityFunctions$Ap2` 98,022 B → 130 B, `Pair` 18,344 B → 14 B,
`BlendDensity` 651,441 B → 46 B. Gates with the flag on, rebased build:
`ChunkFill_HASH=8790f04ace726ecf`, `RingScan HASH=37539c6c375d0c3`.

**Real-boot verdict (2026-07-31): ACCEPTED, default stays ON.** The actual
kill-switch is `RAVA_NO_KEY_INTERN=1`; a set `RAVA_KEY_INTERN` variable does
nothing and must never be used as an A/B dimension. The run used an isolated
server root (separate `world_pristine`/`world`/logs), P-core affinity and a
calibration boot of 78.9 s against the 79.3 s clean reference. Six fresh,
interleaved boots produced:

| configuration | `Done` seconds | wall seconds |
|---|---:|---:|
| default, interning enabled | 22.824 / 21.924 / 22.005 | 75.2 / 71.6 / 71.5 |
| `RAVA_NO_KEY_INTERN=1`, disabled | 42.514 / 41.353 / 42.376 | 95.7 / 93.1 / 94.8 |

Every enabled sample beats every disabled sample. Pairwise `disabled - enabled`
`Done` deltas are +19.690, +19.429 and +20.371 s (median **+19.690 s**);
min-of-N is 21.924 vs 41.353 s (**−46.98% enabled**). Every process reached
server `Done`. With the mechanism disabled, the GIL-free ring's small registry
batch safely aborted to its serial recovery path on a guest NPE; with it enabled,
the full ring batch completed. That is an end-to-end effect of the default
configuration, not a claim that the key transform alone owns all 19.7 seconds.

Independent exact gates on the same release binary pass in both configurations:
`ChunkFill_HASH=8790f04ace726ecf` (enabled Setup/Fill 33183/7973 ms; disabled
40789/7815 ms) and `RingScan(8) HASH=37539c6c375d0c3` (enabled/disabled). The
single-run ChunkFill timings are diagnostic only; the server interleaving above
is the performance verdict.

### GAP-P07 — `RAVA_SORT_KEYCACHE` independently confirmed on `createLookup`

**Status:** the mechanism landed separately on `main`
(`comparator_plan::extract_keys` / `compare_keys`, gate `RAVA_SORT_KEYCACHE`).
This entry records an INDEPENDENT measurement of it, taken before the rebase,
because it names which workload the flag actually buys.

**Which sort it is.** `javap` on `Climate$RTree`: `build()` sorts single-key
(already key-cached), but the recursion's inner `sort(list,i,j,b)` builds
`comparator(i,b).thenComparing(comparator(j,b))` — a composite tree, which fell
through to the general merge sort and re-applied EVERY clause's extractor to BOTH
sides of EVERY comparison.

**Measured (`bench/RsProbe.java`, two interleaved pairs, P-core affinity, every
run's `RsProbe_HASH` equal to the JVM's `abf4a3ebc07412f`):**
`VanillaRegistries.createLookup()` **16537 ms → 8086 ms** and
**16596 ms → 8389 ms**, a repeatable **−51%**; `RandomState.create` is unchanged
in the same runs (2555/2414/2251 → 2560/2331/2273 ms), so the attribution is
clean. Gates: `ChunkFill_HASH=8790f04ace726ecf`,
`RingScan HASH=37539c6c375d0c3`.

**Real boot (`bench/boot_flag_ab.ps1`, private server dir, P-core affinity):** the
useful signal is the PRE-`Done` interval — process start to the
`Starting minecraft server version` line, i.e. blocks A+B+C, which is where the
registry sorts live and which the `Done` timer excludes. Measured from the log
timestamps of one interleaved pair: **111 s OFF → 93 s ON (−18 s)**. The `Done`
window itself is ring-dominated and unaffected (130.879 s OFF / 131.550 s ON on
adjacent runs). The pair's OFF partner in the first slot was invalidated by a
port collision, and two other agents were benchmarking throughout, so treat the
−18 s as one clean sample, not a series.

**Caveat:** extraction is unconditional, so a later clause's extractor runs where
the authoritative path would have short-circuited on an earlier one — the same
purity assumption the shipped single-key cache already makes about key
extractors.

### GAP-P08 — disabled wall profiler still executed hot-path gates

**Status:** CLOSED (2026-07-31, release sampler split).

The wall/PC profiler is diagnostic-only, but its former inactive implementation
remained in every production build and every category scope evaluated an
environment-backed gate. The production build now selects an API-compatible,
inline no-op sampler; the diagnostic implementation is explicitly selected by
the `wall-profile` feature. This changes neither bytecode routing nor any
admission proof: it does not inspect class or method identity, workload,
inputs, output hashes, timing, or execution history.

Both supported configurations compile: release `next-gen-gc`, and release
`next-gen-gc,wall-profile`. The full release gate passed with 419 unit tests.
An exact-HEAD two-binary P-core A/B produced `Done` deltas of -0.062 s and
-0.440 s for the stripped build; a third reverse-order pair was contaminated
by a slower baseline wall (80.8 s versus 76.8 s). Treat the first two as a
small directional signal only, not a credited server-speed result. A fresh
production boot reached `Done (23.880s)` at 78.4 s wall, so the required
`Done < 10 s` and `wall < 20 s` targets remain open.

### GAP-P09 — offload-proof cache epoch churn has no local-cache win

**Status:** diagnostic complete; local-reject split rejected (2026-07-31).

`is_offloadable_rec` memoizes an admission proof, then conservatively erases
every verdict when the loader epoch changes: a newly loaded override can alter
a virtual-dispatch closure. No persistence is permitted until the cost of that
conservative invalidation is measured. With the diagnostic-only
`wall-profile` build, `RAVA_OFFLOAD_CACHE_PROBE=1` reports aggregate hits,
fresh analyses, stores, epoch-crossing unstable analyses, cache-clear count,
and entries evicted with each clear alongside wall-profile dumps. It records
no class/method identity, workload name, input, output, hash, timing, or
execution history, and changes no proof or execution route.

**Observed (fresh restored world, P-cores, diagnostic binary):** by the 62.3 s
wall-profile dump: 488,153 ordinary hits, 12,413 full analyses, 12,391 stores,
22 epoch-crossing analyses, 1,494 clears, and 12,367 evictions. Thus nearly
every stored epoch-scoped verdict was evicted, even though the cache had a 97.5%
hit rate between loads. The same diagnostic boot reached `Done (23.976s)` at
78.1 s wall; diagnostic timing is not acceptance evidence.

**Measured and rejected generalisation:** a candidate persisted only
`Handler`, `Malformed`, `Opcode`, and `SharedWrite` rejects, keyed by immutable
`Code` address plus existing analysis-mode bits; `Callee` rejects remained
epoch-scoped. Its focused safety tests passed, but a fresh-world P-core
two-binary A/B against exact `f18e3c2` was inconclusive and did not separate the
arms: baseline `Done` 22.235/22.516 s versus candidate 22.255/23.307 s; paired
deltas were +1.072 and -0.261 s. The candidate added a VM-lifetime set without
a measured speed win, so it was removed. The remaining tracer is
diagnostic-only and does not change proof or execution behavior.

No further persistence follows from these counts alone. Any future proposal must
first show a clean generic A/B win, retain solely facts independent of hierarchy
state, and keep all dispatch-closure admission epoch-scoped with targeted
invalidation tests; `server.jar`-specific routing or cache policy is forbidden.

### GAP-P05 — compiled dispatch falls to the Rust host on every third call

**Status:** OPEN; the largest measured canonical wall after GAP-P02's guest
code itself.

**Evidence (2026-07-21, `RAVA_JIT_PIC_PROBE`, hash-exact canonical):**
200,054,519 dynamic calls left compiled code; 126.9 M (63.4%) hit the
machine-code PIC, 70.0 M (34.9%) fell to `rava_dispatch_miss` and 3.1 M to
plain `rava_invoke`. 2,010 compiled call sites: 1,223 PIC-emitted, 787
invoke-emitted (static+special are structurally non-PIC-able). Synthetic-frame
fall-through reasons are 95% native/java-served (no compiled fptr can ever
exist — the recorded irreducible native floor), 4% tier lag. The leaf heap-Ref
PIC filled 4.9 M (site, class) pairs while 34.6 M receivers were
never-cacheable.

**Levers:** per recorded verdicts the miss tax is largely the irreducible
`jit_call`/native floor; the one open structural lever was the MAIN-THREAD
λ-PIC.

**λ-PIC MEASURED AND REFUTED as a wall lever (2026-07-21, commit `33daa1b`):**
13.2 M of the host falls carry lambda receivers (6.2 M directable). The
machine-code λ-PIC arm was implemented behind `RAVA_LAMBDA_PIC` (identity key
`(recv<<1)|1` remapped from the `$Lambda` class word by one select; SHIFT bit
calls the SAM impl on `bufaddr+8`; fills restricted to immortal per-indy-site
singletons so identity can never be swept-and-reused; gate-off codegen audited
byte-identical). Three interleaved hash-exact canonical pairs: SETUP/FILL
ranges fully overlap (walls −2.6/+2.4/+1.3 s) — statistically identical. The
apparent −67 MB peak-private under the gate is exactly one 64 MiB
commit-highwater pacing quantum (the default build measured the same 1128 MiB
mode on the parent commit's binary) and is not creditable. CONCLUSION: the
historic ~5–10% main-thread λ-PIC estimate does not hold on the canonical
workload — the per-miss host handler is cheap relative to the guest work it
dispatches, consistent with the recorded "irreducible jit_call tax" verdicts.
The gate stays default-OFF; with this, every recorded canonical wall lever is
measured and exhausted (guest JIT_EXEC quality and the native floor remain).

### GAP-P03 — RAVA remains above the JVM memory target

**Status:** OPEN.

Descriptor fusion reduced canonical peak to 758.0 MiB, still 276.3 MiB above the
JVM reference. Use heap census plus exact structural byte accounting before
changing representation. Preserve stable handles, snapshot wire and JIT ABI.

## Rejected designs that must not be recreated unchanged

| Design | Evidence | Verdict |
|---|---|---|
| Direct eager Stream mapping | about 3% slower | removed |
| Reuse eager Stream output arrays | saved 5–10 MiB, added 2.4–3.8 s | removed |
| Persistent lambda weak cache | added about 72 MiB | removed |
| Datapack JSON pre-cache | zero useful hits, added about 29 MiB | removed |
| Reference/free-list `shrink_to_fit` tracers | no RSS win or more RSS | removed |
| Clone-free lambda existence check | 0.13% median, inconsistent pairs | removed |
| Split array descriptors | some RSS win, mixed timing and hotter JIT path | removed |
| FxHasher authoritative COW shadow | diagnostics improved, production 26.9 → 42.0 s and weaker collision resistance | removed |

## Mandatory closure workflow

1. Reproduce the gap or add an executable negative test.
2. Implement a structural, fail-closed tracer; no names, workload, inputs,
   hashes, results or history in admission.
3. Run focused release tests.
4. Run `cargo test --release --features next-gen-gc`.
5. Run interleaved fresh-process A/B when performance is claimed.
6. Update this register and `docs/multicore-roadmap.md`.
7. Commit one logical change with a Conventional Commit message.

Never stage `.continuum/graph.json` or `.continuum/daemon.lock`.
