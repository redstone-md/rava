# GIL-free boot rearchitecture — program charter

Status: OPENED 2026-07-22 (user decision, after every bounded lever across
representation + both multicore mechanisms was re-investigated fresh and confirmed
walled — see `representation-unification-spec.md`, `runtime-gap-register.md`
GAP-A01/A05, and memory `rava-repr-arc-closed-pivot`). This is a **multi-month,
research-grade program**, NOT a bounded arc. It is the ONLY remaining door with
real headroom, and it is **Amdahl-capped at a 3–5× realistic floor** (roadmap
completion-bar reality). This charter fixes the goal, the dependency chain, what
is already built vs open, the irreducible walls, and the least-walled entry — so
the program is committed with full clarity, not begun by editing concurrency code
blindly.

## Goal and ceiling
Claw back a slice of the ~103.8 s / 48.5% of boot spent in `CompletableFuture`
async data-loading (registries / datapacks / recipes), which today runs
**inline-synchronous** (`reflect.rs:2004` `vm.apply` on the caller) or on ≤2
GIL-bound resident workers (`task_runtime.rs:58` each re-acquires the GIL). Real
boot measured at **0.98 cores** (`lib.rs:283`). Ceiling if fully solved: 3–5×
that fraction, i.e. a boot-wall cut bounded by Amdahl over the ~48.5% parallel
share and the serial APPLY tail.

## The dependency chain (why this is one program, not one change)
Running guest boot bytecode on real cores requires ALL of:
1. **Green pool → real OS threads.** MC's data-load pool runs as green threads on
   ONE OS thread behind the GIL (`gil-free-vm-layout.md:16-20`). Until these map
   to OS cores, a perfect GIL-free layout buys ZERO. GATE.
2. **GIL release for those threads.** The single global `Mutex<()>` (`lib.rs:267/350`)
   guards the one shared `&mut Vm`. A worker may skip it only in a proof-carrying
   gil-free region (`lib.rs:357`).
3. **Per-worker Vm/heap boundary (GAP-A02).** A worker reads only FROZEN shared
   state and allocates in an isolated slot range + nursery view. **Largely BUILT**
   for the density fork: `WorkerContext`, `WorkerVmRef`, `worker_tlab_view`,
   `arena_setup`, `stop_the_world`, the four-category field contract
   (`gil-free-vm-layout.md`). Reusable for boot.
4. **Dispatch/effect/GC-safety proofs for the guest bytecode (GAP-A01).** A worker
   may run a guest method only if its dispatch closure, effects, code lifetime and
   roots are proven. **THE WALL** — see below.
5. **Concurrent shared mutation for APPLY (GAP-A04).** Registry/classloader/statics
   writes need ownership + barriers + root publication + GC-phase coordination +
   JMM semantics. **UNPROVED, un-built.**
6. **Transactional allocation publication (GAP-A03).** A worker's private
   allocations must publish to the shared heap atomically at a barrier. Narrow
   reservations exist; general case OPEN.

## The map-then-reduce shape (the strategic decomposition)
MC data-loading is `SimpleReloadInstance` = **PREPARE (parse, parallel-shaped) →
APPLY (register, serial)**. PREPARE parses JSON into private objects (CPU-bound,
read-mostly); APPLY writes the registry (shared mutation). The strategy: run
PREPARE on the per-worker boundary (step 3, built) with FROZEN shared reads +
isolated allocation, keep APPLY serial (sidesteps GAP-A04). This targets the CPU
share WITHOUT solving concurrent registry mutation.

**But PREPARE is not free of the wall:** codec/JSON parsing runs guest bytecode
with heavy polymorphic dispatch (the DFU codec framework, collections) and
lambda/indy + validation athrow. So PREPARE hits BOTH:
- **GAP-A01 dispatch closure (step 4).** The canonical sound fork saw 712
  candidates, admitted ZERO — every rejection bottoms out at a collection-framework
  equality/`size` closure: `Comparable.equals` alone = **840 loaded implementors,
  737 failing**, each recursing into further unloadable-interface leaves. Closure
  enumeration (design (b)) is "not bounded work" and REFUTED. Only **design (a) —
  flow-sensitive receiver typing** that proves the ACTUAL narrow concrete receiver
  at a polymorphic site (vs the whole interface closure) — can close these, and it
  "does not exist today."
- **`method_fork_safe` cap.** Even a proven subtree yields **serial-divert
  admission only** while indy + athrow are forbidden on the worker path, and boot
  parse code is lambda/validation-heavy. Lifting that is its own sub-arc.

## Least-walled entry — the first brick
**Brick 1: flow-sensitive receiver typing (GAP-A01 design (a)).** Extend the
devirtualization/ownership analysis from "only `this` and fresh `new`" to prove
the concrete receiver type at a polymorphic call site from flow-sensitive typing
of fields/args/returns. This is the documented unlock for admission, it is a pure
ANALYSIS change (no concurrency, no runtime rearchitecture), it is independently
testable (does sound-fork admission of real boot/initCache subtrees rise, hash
bit-exact via the existing `RAVA_OFFLOAD_DRILL` / `RAVA_FORK_SHAPES` diagnostics
with admission OFF?), and it de-risks the whole program: if flow-sensitive typing
STILL cannot break the collection-framework closure on real boot code, the program
is proven walled at step 4 before any concurrency work is written. If it DOES
expand admission, bricks 2+ (OS-thread mapping for the admitted PREPARE subtrees,
serial APPLY, then GAP-A03 publication) follow.

Sequencing rationale: attack the PROOF wall (analysis, cheap, reversible,
diagnostic-only) BEFORE the concurrency rearchitecture (expensive, risky), because
the proof wall gates everything downstream and is the documented irreducible
surface. A negative Brick-1 result closes the program cheaply; a positive one
justifies the multi-month concurrency build.

## Brick 1 — BUILT, RUN, NEGATIVE (2026-07-22): the fork wall is interface/unloadable-shaped
Implemented flow-sensitive final-field receiver typing: a new `offload::F::Typed(cp)`
lattice value produced at an object-typed `getfield`, consumed at the virtual/
interface devirt (`offload.rs`), emitting a `CalleeQuery::typed_field` that the VM
side (`is_offloadable_rec` recurse) upgrades to EXACT iff the field's declared type
is a `final` class — checked NON-FORCING via `loader.cached` (no class load → no
init-order perturbation). Canonical stayed **bit-exact `8790f04ace726ecf`** across
runs; the analysis is sound (a final class has no proper subtype, so its declared
type IS the exact dynamic type).

**Measured with `RAVA_FORK_SHAPES`+`RAVA_OFFLOAD_DRILL`:** the typing WORKS — the
drill shows new `[exact]` resolution chains through final guava classes
(`SingletonImmutableList.get` → `Preconditions.checkElementIndex` → `Strings.
lenientFormat`, all exact). **But canonical fork admission stayed at ZERO
(`[FORK-EACH] … offloadable=false, forkable 0/6`).** The rejection chains bottom
out at receivers final-field typing structurally cannot reach:
- `it/unimi/dsi/fastutil/doubles/DoubleList.get` → "declared owner is an INTERFACE"
  → Gate-1 closure **854 loaded targets, 853 FAILING**.
- `java/lang/StringBuilder.append` → "declared owner UNLOADABLE [no pure native
  contract]" → **861 FAILING**.

So the canonical fork wall is **interface-typed + unloadable-native shaped**, not
final-field shaped. Design (a) final-field typing (the least-walled entry) does not
reach it; design (b) interface-closure enumeration is REFUTED as unbounded
(runtime-gap-register.md GAP-A01); the unloadable leaves (`StringBuilder.append`,
fastutil `DoubleList`) would each need a worker-pure native contract — a separate,
open-ended surface. Brick 1 reverted (sound but no admission gain → invariant).

**Program verdict — the de-risking checkpoint did its job and returned NEGATIVE.**
The GIL-free program is gated on breaking the dispatch-admission wall BEFORE any
concurrency build; the least-walled bounded entry, built and measured on live
canonical boot code, confirms the wall is the GAP-A01 collection-framework /
unloadable-native closure — "not bounded work." **RAVA is at its architectural
ceiling on every axis re-investigated fresh this cycle (representation, density-fork
multicore, supplyAsync multicore, and now the GIL-free entry brick), all converging
on the same walls documented in the ceiling map.** Reopening requires either the
unbounded interface-closure proof, a broad worker-pure-native-contract program for
the collection framework, or accepting the ceiling.

## Invariant (unchanged)
Every brick: canonical bit-exact `8790f04ace726ecf`, `cargo test --release
--features next-gen-gc` green, GC stress green, on fresh processes. Analysis
bricks land diagnostic-only first (admission OFF → bit-exact trivially), then flip
under measurement. Soundness by proof (dispatch/CFG/effects/ownership/GC), never
by hash/workload/timing. Any brick that does not measurably help, or breaks
hash/tests/GC, is reverted and recorded.

## Iteration: the `vm_ref` frontier is CLEARED; the wall is now the array-store barrier

The previous entry closed with two named blockers: `jit.rs:2777` (the plain
`vm_ref()` gil-free denial, which killed every ring task in 0-1 ms) and warmth.
Both are addressed here. The lane still completes 0/127 ring tasks, but for a
different, newly-named reason — and one of the things found on the way was a
silent-divergence bug that mattered more than the throughput.

### 1. The denial was `rava_direct_miss`, not `rava_invoke`

Callback ordinal 0 is `invoke`, but `rava_invoke` already carried the GAP-S09
stage-4 sanction. The plain `vm_ref()` belonged to **`rava_direct_miss`** — the
23.9 monomorphic static/special direct-call miss handler. `directmono` is
default-ON, and `emit_direct_mono` routes a site to this callback whenever its
baked single-fptr cache is cold (`0`) **or permanently negative (`-1`)** — the
latter covering every `java/`-owned / native-served / uncompiled callee, on
*every* call. So this was not a rare cold path; it was the first thing a ring task
touched.

Decomposition of what it needed `&mut Vm` for:
- **(a) genuine shared publication** — `direct_mono_resolve` (→ `resolve_method`:
  class load, resolution-cache publication, `<clinit>`) and the `*cache_ptr = f`
  store into a leaked baked global every thread reads. A worker must do neither.
- **(b) per-thread state that only looks shared** — `jit_pending_is_set/set`
  (execution_state TLS) and `heap.set_jit_fault()` (the worker-private `JitCtx`),
  exactly as the `rava_throw` sanction already documents.
- **(c) `&mut` only because the signature demands it** — `jit_call`, whose FIRST
  action for a lane worker is already the read-only `jit_direct_call_worker`
  (lib.rs), with the G1.1 guard behind it.

So `invoke` does **not** fundamentally require `&mut Vm` here. Fix: take the
`vm_ref_worker_sanctioned()` accessor (same sanction, same reasoning as
`rava_invoke`) and skip the resolve + cache-fill under `parallel::gilfree()` —
the identical frozen-cache discipline `jit_direct_call` already applies. Serial
behaviour is byte-identical.

**Measured effect:** workers went from 1-3 dispatch misses in an entire boot to
executing real compiled bodies (9,620+ callbacks before the next wall), on six
threads concurrently. The first genuine off-GIL completions in this lane's history
appeared: the `ServerFunctionLibrary::lambda$reload$2` batch now runs off-GIL and
**publishes** (`claimed=1/1 completed=1/1 keep=true`).

### 2. Pilot warm — implemented (`RAVA_GILFREE_NOPILOT` opts out)

`run_batch` PHASE 0 runs the batch's first task serially under the GIL, completes
its future immediately (holding the result across PHASE 1 would expose it to that
phase's collections), and excludes it from the region and from PHASE 4's vote.
It doubles as the lane's **yardstick**, printing the serial cost of exactly one
task: **757-814 ms, 5,622,518 callbacks, 2,738 array allocations**.

### 3. The silent-divergence bug — a lane that "succeeded" by computing nothing

With the region finally executing, telemetry read `completed=100/127`. It was a
lie. The per-task work witness (`cbs=`) showed those 100 tasks firing **3
callbacks each in 1 ms**, against 5.6M/804 ms for the identical body serially.

Cause: `rava_dispatch_miss` (jit.rs, "Abort visibility") short-circuits on the
**process-wide** abort flag and returns a discarded 0. The instant one task
aborts, every sibling still running unwinds through garbage continuations and
returns early — while its own **per-task** `LANE_ABORT` latch never fires, so it
votes `aborted=false`. Keep-all-or-discard-all happened to save us (the aborting
task's own slot votes discard), but the lane must not depend on that coincidence
to avoid publishing garbage. `run()` now includes `parallel::abort_requested()`
in its verdict and reports `poisoned=`. The count corrected to the honest `0/127`.

### 4. Parallel array region opened; the wall is now `rava_ring_store_guard`

The lane opened a nursery view and an object-handle range but **no array region**,
so `rava_newarray` aborted on a task's first array allocation. Fixed by reserving
the region per batch (`RAVA_GILFREE_ARR_PER_TASK`, default 2048/task) and
committing after the join, exactly as the S2 fork does.

That exposed the real wall. `rava_ring_store_guard` — the lane's array-store
barrier — **unconditionally aborts every off-GIL array store and returns 1 =
"skip the store"**, so the body kept running against an array it believed it had
filled and died a few callbacks later with `NullPointerException: getfield`.
Deterministic at `cbs=9620` on the first ring task; the abort then poisons all 126
siblings. Abort site confirmed via a new cheap gate `RAVA_LANE_ABORT_SITE`
(`RAVA_FORK_TRACE` is too heavy — a boot under it died before reaching the ring
phase).

The barrier's own comment names its blocker: "array cells carry no per-object
ownership record". The open array region supplies one — slots
`[parr_base, parr_cap)` are reserved fresh for the region, so an array allocated
there was allocated by a worker *during* the region and is unreachable from the
shared graph. `Heap::array_in_parallel_region` implements the test and
`RAVA_GILFREE_ARRAY_OWNED` admits such stores.

**SUPERSEDED — it is DEFAULT ON.** The verdict below was correct when written and is kept for
the reasoning; what changed is that the wall it describes was a wrong-answer bug in the freeze
table (wide-param sites served un-marshalled) plus a hardcoded worker count of 6, both fixed. The
lane now ships on, bit-exact, at wall -9.3% / Done -25.6%. Historical text follows. The test is
sound and it does unblock the body — the first ring task stops dying and goes on
executing the real computation. But the body then hits the dispatch/lock wall
behind it: a boot with it armed burned **664 s of CPU across six workers without
the first ring task returning**, against 757 ms serially, and never completed the
region in 700 s. It converts a fast, safe abort into an unbounded grind.

### Where the lane stands

| | |
|---|---|
| ring batch | `n=128 pilot=1 claimed=101/127 completed=0/127 keep=false` |
| unclaimed 26 | `nursery_full=26` — pure budget: 127 x 256 KiB `NURSERY_SLICE` exceeds main's young headroom. Not an admission refusal. |
| next frontier | `rava_ring_store_guard` (jit.rs) → behind it, the dispatch/lock wall above |

Boot A/B (interleaved, P-cores, `RAVA_M3_FREEZE_RESOLVE=1
RAVA_M3_WORKER_LOCKED_EXEC=1`): OFF 119.05 / 125.54 s, ON 120.47 / 115.95 s —
**neutral**, as it must be while the ring batch is discarded and re-run serially.
`Done` reached and the world loads in all four runs.

Gates: ChunkFill `8790f04ace726ecf` with the lane OFF and ON; 365 interp tests
pass. Everything here is behind `RAVA_GILFREE_RINGS` (now DEFAULT ON; `RAVA_NO_GILFREE_RINGS` opts out).

**The `~3x` prize is NOT refuted and NOT collected.** The Amdahl case rests on the
freeze census's 96.6% lock-free coverage; the 664 s grind is the first end-to-end
evidence bearing on it, and it points the other way. Deciding between "the pilot
warms too little" and "the census over-counted coverage" is the next measurement,
and it now has a live lane to measure in.

## The 664 s grind is NEITHER: it is an unbounded alloc-retry spin (`RAVA_LANE_PROBE`)

Both named candidates are REFUTED by worker-side measurement. `report()` fires at
heap teardown, which a region that never finishes never reaches, so the lane's own
classification was unreadable during the grind; `lane_probe.rs` adds a watchdog that
dumps per-worker counters on a period. Two boots, lane + `RAVA_GILFREE_ARRAY_OWNED`,
`RAVA_M3_FREEZE_RESOLVE` + `RAVA_M3_WORKER_LOCKED_EXEC` + `RAVA_FORK_STATS`.

### The census was RIGHT, and the pilot warms ENOUGH

Measured ON THE WORKER, whole ring region (six workers), run 1 / run 2:

| arm | run 1 | run 2 |
|---|---|---|
| freeze table hit (lock-free) | 428,595 | 426,097 |
| frozen direct cache hit (lock-free) | 986,420 | 967,515 |
| WS-native hit (lock-free) | 36,144 | 36,081 |
| **lock-free total** | **1,451,159 (99.30%)** | **1,429,693 (99.28%)** |
| fell through to the global lock | 10,192 | 10,362 |
| **OUTER lock acquisitions, process-wide** | **15** | **15** |
| **total lock WAIT / HOLD** | **1 ms / 3 ms** | **1 ms / 2 ms** |

The census predicted 96.6% lock-free with a ~64,690/task locked residual. The worker
beats it: **99.3%**, and the whole region's real contention is **1 ms**. The lock is
not the ceiling — it is four decimal places from irrelevant. More pilots would change
nothing: every cache the census credited is warm, and the arms serve.

### What actually happens: `rava_arena_safepoint` spins forever

One worker (w3 run 1, w4 run 2 — same site, `ImprovedNoise.sampleAndLerp`) executes
about one task's worth of real work AT SERIAL SPEED, then stops making progress and
spins. Every non-`other` callback kind FREEZES permanently; `other` grows without
bound at **~7.8 M/s**, reaching **1,440,014,450** callbacks — **256x the entire
serial task's 5,622,518** — and still climbing linearly when the run was stopped.
Run 2 split the `other` bucket and named it: **safepoint = 176,246,054 of
181,625,474 callbacks (97.0%)**, with `abort_raisers total=176,267,508
overflow=176,267,507` — one `request_abort_overflow` per spin iteration.

The chain, all five links in-tree:

1. `gilfree_cf.rs` `NURSERY_SLICE = 256 KiB` is the task's whole private young view;
   a ring task allocates more than that (the serial pilot alone does 2,738 array
   allocations), so the view exhausts MID-TASK.
2. The compiled inline-alloc **retry loop** (`jit.rs` `emit_inline_arena_new`) branches
   to `overflow`, calls `rava_arena_safepoint`, and **jumps unconditionally back to
   `retry`** — it polls no abort flag and has no bail-out edge.
3. `rava_arena_safepoint` (`jit.rs`) is GAP-S09-sanctioned to RUN on a gil-free
   worker, and calls `Vm::arena_collect_jit`.
4. That function's gil-free arm does not collect (it must not); it tries to refill the
   view from the region's **spill pool** via `parallel::spill_claim(1 MiB)`.
5. `parallel::spill_setup` is called from exactly ONE place — the S2 chunk-fill fork
   harness (`lib.rs`). **The rings lane never opens a spill pool**, so `SPILL_BASE == 0`,
   `spill_claim` returns `None` immediately, and the arm raises
   `request_abort_overflow()` and RETURNS WITH THE VIEW STILL FULL.

So the retry loop re-reads `[next, end]`, still full, and re-enters the safepoint —
forever. `arena_collect_jit`'s own comment licenses this ("residual machcode grind is
bounded by the worker's finite range"): TRUE for the chunk-fill for-index kernel that
polls the abort at each element, FALSE for a ring lambda, which has no such boundary.

### The "six workers burned 664 s of CPU" reading was wrong

Per-thread CPU sampled live (run 1, 215 s wall, 330.5 s process CPU): **one thread
Running at 223.3 s**, main at 107.3 s (the boot itself), and **every other thread at
0.0 s**. Run 2: 54.9 s Running, 104.4 s main, rest 0.0 s. It was never six workers
grinding — it is ONE worker in a closed loop while its five siblings sit at zero.

### Consequence for the lane

The ~3x prize remains neither collected nor refuted, but the blocker is now named and
it is NOT dispatch: the lane needs an allocation budget a ring task actually fits in
(open a spill pool for the region as the S2 fork does, and/or size the per-task slice
to the measured footprint), and the compiled overflow arm needs a bounded exit so an
exhausted worker deopts instead of spinning. (SUPERSEDED: both landed, and
`RAVA_GILFREE_ARRAY_OWNED` is now DEFAULT ON — `RAVA_NO_GILFREE_ARRAY_OWNED` opts out.)

---

## The allocation budget is FIXED — and the lane is now REFUTED on a lock, not a spin

Everything the section above prescribed was built and measured. The lane now runs the
whole ring phase off-GIL and is still slower than serial. The blocker moved.

### What shipped

1. **A spill pool for the rings region.** `parallel::spill_setup` had one caller (the
   S2 chunk-fill fork), so the rings overflow arm found `SPILL_BASE == 0`. It is now
   opened at region open and cleared at teardown, S2's discipline — with one
   deliberate difference. S2 points its pool at the dedicated 64 MB low region
   (`nextgen::spill_region_base`) because its results are f64s that never leave the
   region; a RING task's result ESCAPES into a CompletableFuture, and a published
   object sitting in process-lifetime memory that the next region's cursor reset hands
   out again is a use-after-free. So the rings pool is carved from **main's own
   young**: spill objects are ordinary young objects the collector owns, the discard
   path's `young_set_top` retreat covers them, and the untouched tail is given back on
   keep. `spill_setup` now publishes its own refill granularity (S2 keeps 1 MB chunks;
   rings refills by one slice).

2. **Budgets sized to the measured footprint.** Per-task slice 256 KiB -> a 192 KiB
   *starting view* plus the shared pool (a shared reserve beats padding every slice to
   the worst case: only the tasks that overshoot draw on it). The nursery default is
   raised to 256 MiB under the rings flag, because 127 simultaneously-live task
   footprints of ~480 KB do not fit 32 MiB however the slice is sized. Two further
   reservations were short and each one discarded a whole batch: `FIELD_BOX_SLOTS`
   256 -> 4096 (exhausted on 31 of 127 tasks) and `arr_slots_per_task` 2048 -> 4096
   (the pilot measures 2,738 `newarray`s per task, so the region ran dry at task ~80
   and every task past it took `NullPointerException: array` out of a compiled body).

3. **A bounded exit on the compiled overflow arm.** `rava_arena_safepoint` now RETURNS
   a status — 0 "retry", 1 "I could not free you anything" — and
   `emit_inline_arena_new` branches on it into a `bail` block that takes a scratch
   object from `nextgen::deopt_scratch` (per-thread, low-mapped, wrapping), lays the
   same header down, and lets the body return normally into the already-raised region
   abort. The back edge is no longer unconditional. On main the status is a constant 0,
   so the fast path keeps its shape.

4. **`RAVA_GILFREE_RINGS` now implies `RAVA_M3_WORKER_LOCKED_EXEC`.** Without it every
   lane worker takes the legacy G1.1 guard within THREE callbacks, so the lane admits
   all 127 tasks and completes none — which reads exactly like a deeper failure and
   cost a full measurement run to notice.

5. **PHASE 4 votes by WORK, not by the latch.** A task must fire at least an eighth of
   the pilot's serial callback count or it is counted as an abort.

### The measurement

`claimed=127/127 completed=127/127 keep=true`, `cbs_min=5,013,262` against the pilot's
5,622,518 serial — real work, not a latch. `pool_claims=254`, `pool_used=48.8 MB`: the
pool is what makes it possible. `nursery_full=0`. World loads clean; the ChunkFill gate
is `8790f04ace726ecf` with the lane OFF and ON.

Interleaved full boot, P-cores, `Done`:

| arm | run | Done |
|-----|-----|------|
| OFF | 1   | 119.984 s |
| ON  | 2   | 155.596 s |
| ON  | 3   | 172.637 s |
| OFF | 4   | 119.751 s |

**+37%. REFUTED.** The region itself takes 172-190 s to do what 127 serial tasks do in
~112 s, on six workers.

### The next wall, measured: the ONE global resolve lock, held per SUBTREE

`RAVA_LANE_PROBE` per worker over a 190 s region: **wait_ms ~= 130,000 (68% of the
region blocked)**, **hold_ms ~= 26,000 each — 158 s summed, 83% of the region
serialized**. Only **395 OUTER acquisitions** across all six workers, covering
**3,784,710 locked general `jit_call` entries**: ~400 ms and ~9,500 calls per
acquisition.

That is the shape of the problem. `resolve_lock_acquire` (the `_worker_exec_guard` in
`Vm::jit_call`) is RE-ENTRANT, so the first native-served or uncompiled callee near the
TOP of the ring body's call tree takes the global lock and holds it while the entire
subtree executes. The lane's only two options at that point are the G1.1 abort or the
global lock, and the lock makes six workers run one-wide plus overhead. The earlier
"99.3% lock-free, 15 acquisitions, 1 ms contention" reading was taken on a region where
one task was spinning in the alloc loop and never entered the deep body; once tasks
actually execute, the composition inverts.

The prize needs the locked-exec envelope SCOPED TO THE CALLEE rather than the subtree,
or those callees made worker-safe outright — the probe's recurring sites are
`java/util/Set.contains`, `AtomicLong.get`, `BiomeSource.findBiomeHorizontal` and
`ImprovedNoise.sampleAndLerp`. Allocation is no longer the blocker.

### The envelope WAS narrowed — and the anchor just moves one frame down

The section above names the wrong site. `RAVA_LANE_OUTER` (new, default OFF) bills every
OUTER acquisition to the `resolve_lock_acquire` call site that opened it and to the
callee under it, timed from the acquire. On the reproduction run
(`RAVA_GILFREE_RINGS=1 RAVA_GILFREE_ARRAY_OWNED=1`; 127/127 complete, `cbs_min`
5,406,378 against the 5,622,518-callback serial pilot):

    jit.rs:1650   (rava_dispatch_miss)           n=126  hold_ms=172,832  max_ms=2,203
    lib.rs:12717  (jit_call _worker_exec_guard)  n=127  hold_ms=1        max_ms=0

The `_worker_exec_guard` the previous section blamed holds the lock for **1 ms**. The
172.8 s is `rava_dispatch_miss`, which took the guard and then ran `Vm::dispatch_miss`
whole — and that function is RESOLVE + PUBLISH followed by a tail `jit_call` that
EXECUTES the callee. One acquisition per ring task, held for the task.

So the seam is real and it was taken: `dispatch_miss` is split into
`dispatch_miss_resolve` (loads the class, runs `<clinit>`, compiles, writes the site's
N-way IC and mirrors the verdict into the freeze table, plus the λ-PIC / heap-Ref fills)
and the EXECUTE tail, and the worker releases between them. Nothing resolved is carried
across the release — the execute half re-derives from `(caller, cp, kind, buf)` and
re-probes the tables — so there is no resolve-then-stale-use window; published verdicts
are monotonic with immortal fptrs, the same property that already makes the lock-free
freeze-table hit path sound. Serial behaviour is unchanged (both halves still run back
to back); `RAVA_WIDE_ENVELOPE=1` restores the old wide guard for A/B on one binary.

It works exactly where it was aimed, and it does not pay:

    jit.rs:1650   n=126  hold_ms=172,832  ->  n=126  hold_ms=1
    lib.rs:12743  n=124  hold_ms=1        ->  n=124  hold_ms=164,462  max_ms=2,114
    lane totals   locks=392 wait=900,116 hold=183,230
               -> locks=514 wait=812,401 hold=164,469

The hold MOVED. The new anchor is `jit_call`'s own envelope around
**`BiomeSource.findBiomeHorizontal`** — ~1 acquisition per task, up to 2.1 s each.

That is structural, not another missing seam. A ring task's body is essentially ONE
top-level call: `findBiomeHorizontal`, "a deep virtual tree the devirt analysis cannot
prove" (the same property that makes the lane admit optimistically). Being megamorphic
it misses the read-only `jit_direct_call_worker` cache, so it lands on the general path,
whose envelope necessarily spans its whole subtree. And that envelope CANNOT be split
the way `dispatch_miss` was: `opcodes::invoke*` interleaves resolution with execution
through the special-case gauntlet and ends in `vm.invoke(decl, ..)` — the INTERPRETER
mutating shared `Vm` (`gc_protect`, the `tlcache` inline caches, the heap, cold-callee
compilation). Releasing there is precisely the G1.1 hazard, and it is what the
`vm_ref()` envelope gate exists to deny: every nested `&mut Vm` callback is admitted
ONLY while `resolve_lock_held_by_me()`.

The boot agrees. Interleaved 2v2 on P-cores, `off,on,on,off`, one binary:

    OFF 148.656 / 162.379 s      ON 177.876 / 188.361 s      = +17.7%

Caveat, stated because it changes how much weight the absolutes carry: another agent's
MC boot overlapped runs 1-3, so every number here is inflated (the reference OFF arm on
a quiet machine is ~120 s). The DELTA is still a clear loss, and the lock ledger — which
is measured WITHIN a run and so is contention-insensitive — is the load-bearing result:
total hold 183,230 -> 164,469 ms and wait 900,116 -> 812,401 ms, i.e. the 172.8 s that
left `dispatch_miss` reappeared almost entirely at `jit_call`. The predicted prize (ring
88 s -> 15-25 s, Done -> 40-55 s) did NOT materialise.

VERDICT: scoping the envelope to the callee is SOUND and is now in tree, but it is not
sufficient on its own — with one megamorphic call per task spanning that task, the lock
re-anchors one frame lower. The remaining lever is the other branch: make the recurring
sites worker-safe/lock-free (`findBiomeHorizontal` first, then `Set.contains`,
`AtomicLong.get`, `ImprovedNoise.sampleAndLerp`) so the top-of-body call never reaches
the general path at all.

**STATUS 2026-07-28 — SUPERSEDED: the lane is DEFAULT ON** (`RAVA_NO_GILFREE_RINGS` opts out),
bit-exact against four gates, wall -9.3% / Done -25.6% ON CLEAN. The blocker was not the list
above: it was a silent wrong-answer bug (freeze table publishing wide-param sites) and a hardcoded
worker count of 6, two past the optimum. See docs/lane-array-store-frontier.md.
