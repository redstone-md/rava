# TOPIC: M3 Chunk-Fill Multicore Arc — SEALED (Variant B, conservation)

Status: **CLOSED**. Serial 3-phase split (10.2b) is the shipped, bit-exact default; the parallel
worker contour is conserved, gated OFF, awaiting a runtime tectonic shift (per-thread `Vm` or
callback-free compilation).

## Target
`bench/ChunkFill.java` — a coordinate-independent density-cube fill
(`router.finalDensity().compute(SinglePointContext(x,y,z))` over an N×N column stack; no loop-carried
RNG → embarrassingly parallel). Determinism gates: `ChunkFill_HASH = 8790f04ace726ecf`,
`RingScan HASH = 109205b3` (both bit-exact, run-to-run stable).

## Shipped (default)
- **10.2b serial 3-phase split** (`try_fork_chunk_fill_loop`, gate `RAVA_M3_CHUNK_FILL_READY=1`):
  inspector → serial body-slice executor (`run_stop`) → ordered reduce-replay with a pool-extracted
  fold constant. Bit-exact `8790f04ace726ecf`. Workers OFF.
- Graceful abort → serial fallback is always intact: any worker fault tears the region down and the
  driver recomputes on main → the hash is **never** wrong.

## Conserved (gated OFF by default — inert)
Six worker-isolation layers, all real, tested, and env-gated:

| Gate | Layer |
|------|-------|
| `RAVA_M3_IC_PARALLEL` | 11.1–11.3 per-thread relative-addressed inline caches |
| (auto on worker) | 11.4/11.6 Thread-Owner **re-entrant** `VM_RESOLVE_LOCK` |
| (region) | 11.7 heap-handle table segmentation (per-worker `objects`/`arrays` slices) |
| (region) | 11.2 per-worker young-nursery view |
| `RAVA_M3_IC_COLD` | cold per-thread IC (no snapshot seed) |
| `RAVA_FORK_COW` | copy-on-write field-write isolation to a thread-local shadow |
| `RAVA_M3_CHUNK_FILL_PARALLEL` | the RavaPool 15-worker executor itself |

`RavaPool` scales **11.7×** for callback-free compiled bodies (proven, 9.2e).

## Why parallel is paused (the wall)
Workers run the **full `opcodes::run` interpreter concurrently on one shared `&mut Vm`**. We isolated
the specific re-entry callbacks above, but `run` re-enters `&mut Vm` through **many more** unguarded
paths (`getstatic`, `card_mark`, class-load/`<clinit>`, `indy`, string ops, …). Live 15-worker runs
therefore abort **non-deterministically** (observed: NPE `Holder.value`, ULE `input()` receiver-misID,
NPE `DensityFunction.compute`) — each a different unguarded shared-`&mut Vm` re-entry, not one fixable
site. Isolating them one-by-one diverges (whack-a-mole). The clean alternative — a callback-free
compiled `compute` body (the path that hits RavaPool's 11.7×) — is blocked by the devirtualisation
wall: `compute(ctx)` passes objects, so it cannot be LLVM-inlined into a self-contained function.

## Reviving parallel (future)
Needs one of two tectonic shifts, then flip `RAVA_M3_CHUNK_FILL_PARALLEL=1` (+ `RAVA_M3_IC_PARALLEL=1`):
1. **Per-thread `Vm` dispatch/resolution state** — make every `run` re-entry callback worker-safe
   (audit `getstatic` first), or
2. **Callback-free compilation** of the compute graph (requires cracking object-passing devirt).

Full increment-by-increment history lives in the project's engineering notes.
