# Parallel worldgen — incremental roadmap (the real multicore)

Goal: use ALL cores by running **chunk worldgen** bytecode on worker OS threads in
parallel. MC's main tick is serial by design (multicore can't speed it much), but
worldgen (ChunkProvider*/GenLayer*/NoiseGeneratorOctaves) dominates boot and
on-the-move chunk loading — that's the parallelizable prize.

Current state: ONE daemon runs on a real OS thread, but under a **GIL** (only the
lock holder runs bytecode → effectively single-core). This roadmap relaxes the GIL
for provably-race-free worldgen.

## Hard mandate (every step)
- After EACH increment: boot to Done + McProbe + `RAVA_HARD_MB` never trips + RSS ≤
  the single-thread baseline (~1.2 GB no-player). A leak/RSS regression blocks the
  next step. Commit per increment.
- Memory-safety is the #1 priority (prior 31 GB host-freeze trauma). Correct-but-slow
  beats fast-but-racy. RAVA_JIT_VERIFY where it applies; stress tests for races.

## Why it can be race-free (the safety argument)
A worker generates a chunk that NO other thread can see yet (detached until merge):
- **Writes** go only to that chunk's own new objects → disjoint per worker → no write races.
- **Reads** of shared registries (BlockState/biome/block — immutable after boot) →
  concurrent read of immutable data is safe, no lock.
- **Allocation** must be thread-safe (the one shared mutation) → thread-local arena.
- **GC** must stop ALL threads at a safepoint before scanning roots.
- **Merge** the finished chunk into the main world under the GIL (single-threaded).

## Increments (each: own commit + the memory gate above)
1. **[FOUNDATION] Stop-the-world safepoint primitive.** A `parallel` module: threads
   running GIL-free poll a `GC_PENDING` flag at the op-loop checkpoint and park at a
   barrier; the GC requester sets it, waits until all parked, runs `gc()` (roots via
   `frames::all` already span all threads), clears, resumes. Built DORMANT (no
   parallel regions yet → can't fire) + unit-tested in isolation (N threads bump a
   shared counter, GC-gated, assert no lost updates + correct stop/resume). RISK: low
   (dormant; isolated test). This is the reusable coordination the rest needs.
2. **[ALLOC] Thread-local allocation arena.** A worker allocates objects into its own
   id-range / object buffer, lock-free; partition the handle space (per-thread id
   ranges) so thread-local handles don't collide with main-heap handles. Reads of
   main-heap objects (immutable registries) by handle stay valid. RISK: medium (heap
   handle-space refactor). Verify: a worker allocates + reads its own + shared
   immutable objects concurrently with main; stress for corruption; RSS bounded.
3. **[CONCURRENCY] Verify registry immutability + concurrent-read safety.** Audit that
   post-boot the registries the gen reads (Block.REGISTRY, BlockState lookups, biome)
   are never mutated during gen. If any are written during gen → must lock or snapshot.
   RISK: medium (a missed mutation = race). Verify: trace writes to registry classes
   during a parallel-gen window; must be zero.
4. **[OFFLOAD] Run ONE chunk's gen on a worker, GIL-free, merge under GIL.** Pick the
   chunk-generate entry, run it on a worker with a thread-local arena + the safepoint,
   then merge the produced chunk into the world on the main thread under the GIL.
   Start with ONE worker (parallelism=2) to bound blast radius. RISK: HIGH (first real
   GIL-free bytecode). Verify: generated chunk byte-identical to the serial path
   (compare a known seed's chunk serial vs parallel); boot + McProbe; no corruption.
5. **[SCALE] N workers, work-stealing chunk queue.** Fan out the pending-chunk queue
   across min(cores-2, N) workers. Measure wall-clock boot/gen speedup. RISK: high
   (contention, GC under load). Verify: boot time vs serial (now meaningful — boot is
   gen-bound), RSS bounded, McProbe.

## DISCOVERED PREREQUISITE (audit 2026-06-18): per-thread interpreter state
Running gen BYTECODE on a worker concurrently needs more than the alloc arena — the
interpreter mutates SHARED Vm caches on nearly every op. For two threads to run the
interpreter at once without racing, these must become per-thread (thread_local) OR a
worker must run a cache-bypassing interpreter. Shared mutable Vm state touched per-op
(must be isolated before concurrent execution — increment 3.5, do BEFORE #4):
- HOT, written per-op: ✅ DONE → `tlcache` module (thread_local): `field_cache`
  (2i), `inline_cache` (2d), `call_cache` (2h), `resolve_cache` (2g),
  `field_key_cache` (2e), `getstatic_cache` (2f, `static_gen` stays shared as the
  generation guard — under the GIL its bump is seen by all threads), `vec_pool`
  (→ `frames::VEC_POOL`). REMAINING (counters/JIT, deferred to #2b wiring): the
  per-op counters `op_countdown`/`op_total`/`native_depth` (per-thread by nature
  but coupled to the run-loop + GC gating — isolate when the worker op-loop is
  built), `jit_pending` (48 sites, coupled to the heap jit-fault flag),
  `jit_direct_cache` (read-mostly; workers don't compile).
- ALREADY per-thread (moonshot): frame ptrs, recursion depth, currentThread,
  `jit_cell_roots`, `jit_box_table` (frames module).
- SHARED read-mostly (OK if workers don't WRITE — audit 2026-06-18 of the Vm struct):
  `jit_cache`/`jit_direct_cache`/`jit_stats` (workers never compile → read-only for them;
  main compiling concurrently needs a snapshot or a lock), `class_objects` (the
  `HashMap<String,u32>` of materialized `java.lang.Class` instances — populated post-boot,
  but a worker doing `ldc class` / `getClass` on a not-yet-materialized class would WRITE
  it → a #4 worker must either avoid that path or it must be guarded), `bool_cache`
  (Boolean.TRUE/FALSE handles, lazily written once — a concurrent first-write is idempotent
  in value but still a data race on the Option → guard or pre-warm before a region),
  loader/registries (immutable post-boot → concurrent read safe — verified by increment 3).
This is a sizeable HOT-PATH refactor (~12 fields, touches every dispatch/field access)
with perf tradeoffs (per-thread caches start cold). It is the real gate to #4 and
should be done carefully + measured (single-thread perf must not regress). The 2b
alloc arena (done, proven) is necessary but NOT sufficient on its own.

## Out of scope (serial by nature — don't parallelize)
- The main world/entity/block tick (MC runs it on one thread even on real Paper).
- Anything that mutates SHARED world state (only gen-into-detached-chunk is safe).

## Status
- Increment 1 (safepoint primitive): DONE + tested + pushed (commit a10c097). Dormant.
- Increment 2a (stable object addresses, Box<Object>): DONE + pushed (commit 4abad27).
  Behavior-preserving (48 tests, boot, RSS 1.0GB). NOTE: if alloc overhead ever
  measures as significant, swap Box<Object> → a SEGMENTED store (inline objects in
  stable chunks: `Vec<Box<[Object; CHUNK]>>`, no per-object malloc) — same stable
  addresses, cheaper alloc. Box was chosen as the minimal behavior-preserving step.
- Increment 2b CORE (object + descriptor concurrent materialization): DONE + stress-
  tested + pushed. `reserve_obj_range`/`objects_slot_base` (objects) + `obj_descs_slot_base`
  /`desc_of` (the native field-descriptor Vec) pre-reserve under the GIL so neither Vec
  reallocs during a region; workers fill DISJOINT slots via the single audited unsafe
  primitive `Heap::fill_reserved(obj_base, desc_base, idx, obj)` (heap.rs). Proven race-
  free: `concurrent_disjoint_object_writes_are_race_free` (4 threads × 2000 objects WITH
  fields + concurrent desc sync, 25 iters) — the obj_descs entries read back the same
  fields the JIT/native path would.
- Increment 3 (immutable registry concurrent-read safety): DONE + stress-tested + pushed.
  `concurrent_immutable_reads_during_disjoint_writes_are_consistent` — workers read shared
  immutable "registry" objects through the raw base WHILE writing their own disjoint slots;
  a checksum over every read would diverge on any torn read (asserts == single-threaded
  recompute), and the registry is verified UNMUTATED after the region. This is the read-of-
  immutable + disjoint-write pattern the live gen relies on (reads Block/BlockState/biome
  while building a detached chunk).
- Per-thread interpreter caches (the gate to running gen BYTECODE concurrently — see the
  PREREQUISITE section): DONE + boot+McProbe-verified + pushed (tlcache module, commits
  1fddb66→211458c). Boot reached Done (502s); McProbe joined, played 45s stable, sane spawn.
- Increment 4 (LIVE OFFLOAD, THE REMAINING DANGER — do WITH the user / fresh focus): run
  ONE chunk's gen bytecode on a worker using `fill_reserved` + a thread-local arena, wire
  the safepoint (#1) into the op-loop GC check so GC stops all workers first, move the run-
  loop counters (op_countdown/op_total/native_depth — native_depth gates stop-the-world GC),
  then merge under the GIL. Verify the produced chunk is BYTE-IDENTICAL to the serial path
  (known seed, serial vs parallel) before any live use. All the isolated pieces above are
  proven; this is the first REAL GIL-free bytecode execution and the memory-safety crux.
