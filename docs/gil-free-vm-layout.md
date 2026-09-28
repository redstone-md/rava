# GIL-free Vm layout — field contract audit

Infrastructure prep for parallel worldgen workers. This increment is **inert**: it
classifies every `Vm` field into a concurrency contract and adds the (dormant)
`WorkerContext` type + a `debug_assert` guard, WITHOUT flipping any parallelism on.
Default execution is unchanged (`ChunkFill_HASH=8790f04ace726ecf` bit-exact).

> This audit predates the strict P0 review in
> [`runtime-gap-register.md`](runtime-gap-register.md). In particular, a matching
> hash is verification evidence, never a soundness proof. Current iterator/COW
> workers still need exact dispatch, code-lifetime and freeze proofs before this
> layout can be considered a safe execution boundary.

## ⚠ Necessary but NOT sufficient

Measured ([[rava-heap-census-verdict]]): a boot takes the GIL **once, contended
zero times, with zero real OS threads** — MC's chunk pool runs as green threads on
ONE OS thread behind the GIL. So a perfect GIL-free `Vm` layout buys **zero**
speedup until the guest chunk pool's `Thread`s map onto real OS cores. This audit
prepares the layout; the guest-pool-on-OS-threads step is a separate, larger arc.

## The four contract categories

A worker running a compiled worldgen subtree GIL-free may touch a field only per its
category. Enforced (in debug) by `parallel::assert_not_gilfree()` on the mutation
side of every FROZEN field.

### 1. FROZEN — read-only inside a region (worker reads without the GIL)
The driver brings these to a terminal state BEFORE opening a region (force-compile
the whole callee tree, run all reachable `<clinit>`, prewarm the caches); a write
while `gilfree()` is a bug.

| field | why frozen |
|---|---|
| `loader` (post-clinit) | class metadata + statics; all reachable classes initialised before the region |
| `trace`, `mem_limit`, `jit_enabled`, `cnp_enabled`, `gc_enabled`, `jit_verify`, `jit_cap`, `jit_threshold`, `max_depth`, `entry_class` | config, set once at startup |
| `jit_cache` | force-compile the whole tree first ([[rava-g1-initcache-fork]]: sound path = all-compiled-tree); hotness counters then frozen |
| `cnp_cache`, `aot_clif_cache`, `aot_full_cache`, `aot_hot`, `jit_direct_cache`, `jit_direct_neg`, `offload_cache`, `numeric_probe_seen` | compile / resolution memos — prewarm before the region (`column_fork_no` retired with the column fork, GAP-S09 stage 5) |
| `class_objects`, `indy_singletons`, `bool_cache`, `main_thread`, `jit_lambda_ptr`, `bitset_name_ptr` | interned singletons / learned-once pointers (also GC roots); warm before |
| `column_prewarmed` | set true by the driver before the region |
| networking (`sockets`, `net_selectors`, `in_pump`, `next_pump_op`, `pending_streams`) | main-thread only; a worldgen worker never touches them |
| scheduler (`trampoline`, `pending`, `yield_request`, `checkpointed`, `green_threads`, `inline_thread_active`, `os_thread_spawned`, `in_green_slice`) | main-thread only |

Explicit driver stacks are not a `Vm` field: `frames::ExecScope` registers them
for collector root scanning and tracks the current stack per thread and `Vm` for
diagnostics.

### 2. THREAD-LOCAL — per-worker private execution state
Each worker needs its OWN, or a shared `Vec`'s push/pop corrupts across the
interleave (the exact hazard that already evicted the JIT cell roots + box table +
frame stack + vec-pool to per-thread storage).

| field | destination |
|---|---|
| `output` | per-worker `String`, merged after the region |
| `call_stack`, `gc_protect`, `op_total` | `ThreadContext` TLS, routed by safe context-or-`Vm` helpers |
| `jit_pending`, JIT cells, frame stack, vec-pool | `frames::` TLS (first-fault-wins and active roots are per-thread) |
| `set_index` | per-worker or frozen (worldgen rarely mutates a stream set) |

`ThreadContext.gc_protect` roots are registered for their lifetime and included
in the collector's root walks/writeback. This does not let a collector run
concurrently with mutators; the registered-root visitor requires owners stopped.

### 3. SHARED / SYNCHRONIZED — shared tables require their existing lock or atomic
| state | note |
|---|---|
| `shared_memos` (`new_site`, `anewarray`, class/partial layout, alloc-route, pure-value override, tagkeep) | Extracted synchronized memo tables. `RuntimeShared` exposes only named lock-backed operations; existing insert gates and cache semantics remain unchanged. This narrow view does not authorize worker execution. |
| `shared_memos.static_gen` | getstatic-cache generation → `AtomicU64` |
| `gc_next`, `gc_next_real` | GC scheduling ratchet → `AtomicUsize` |
| `jit_stats` | diagnostic counters → per-thread + merge, or atomics |
| `gc_calls` | leak-diagnostic counter → `AtomicU64` |

### 4. SPECIAL — `heap`
Not a single category. The object/array tables are shared-immutable for REGISTRY
reads, but allocation needs isolation:
- Allocation: each worker bump-allocates from a pre-reserved disjoint slot range
  (`parallel::ARENA` / `arena_setup`, ALREADY built) + a per-worker nursery view
  (`nextgen::worker_tlab_view`, ALREADY built). No shared `objects.push`.
- Mutation isolation: the M3 COW shadow (`RAVA_FORK_COW`) redirects worker field
  writes to a TLS shadow. Hash equality is not a proof of rollback, effect or
  dispatch safety; the executable obligations are GAP-S01 through GAP-S09.
- The mark-sweep + arena collectors require exclusive VM execution. The low-level
  `parallel::stop_the_world` protocol is dormant outside tests and does not yet
  prove full-runtime mutator polling, root publication, or JIT reload.

## The `WorkerContext` type
`&mut Vm` cannot be shared across threads. Instead a worker holds a `WorkerContext`
= `{ vm: *const Vm (FROZEN reads only), arena slot range, nursery view }`. It exposes
only `&Vm` (shared, for frozen reads) — never `&mut Vm` — so the type system stops a
worker from mutating a frozen field. Execution-local call stack, temporary roots,
and operation accounting live in `ThreadContext`; frames/JIT cells remain in
`frames::` TLS. LANDED (GAP-S09, 2026-07-21): fork payloads carry the
typed `parallel::WorkerVmRef`, kind-A workers run under the RAII
`parallel::WorkerContext`, and the discipline-only `vmptr: *mut Vm` sharing this
paragraph described is gone (the column-fork prototype itself was retired; the
remaining interpreter-arm experiments are quarantined behind default-off env gates
outside the S09 claim — see `runtime-gap-register.md`).

## What this increment ships (inert)
1. This audit.
2. `parallel::WorkerContext` (dormant, `#[allow(dead_code)]`).
3. `parallel::assert_not_gilfree()` — a `debug_assert` helper; applied at the key
   FROZEN mutation points (`class_objects`, `jit_cache`, static writes) as the
   contract's executable spec. `gilfree()` is false on the single-threaded boot, so
   the assert never fires → bit-exact.

## NOT in this increment (the risky flips, later arcs)
- Landing the guest chunk pool on real OS threads (the actual speedup gate).
- Moving the THREAD-LOCAL fields into `frames::` (behaviour-neutral but broad).
- ~~Flipping column-fork onto the `WorkerContext` by default~~ — moot: the
  column-fork prototype was RETIRED at GAP-S09 stage 5 (superseded by the S2
  compiled-body chunk-fill fork).
