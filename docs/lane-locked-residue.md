# The ring's locked residue, enumerated — 2026-07-27

Measured at the resolve-lock acquisition itself (`RAVA_LANE_PROBE` now populates the tally), not
inferred from the per-site coverage census. Two estimates from that census were wrong by an order
of magnitude, which is why this file exists.

## Revalidation on `4081950` — 2026-08-02

The worker dispatch now calls the existing `Vm::jit_direct_call_worker(&self)` before taking the
resolve lock. This removes the aliased `&mut Vm` escape from that pre-lock arm; it does not claim
to make a previously unservable native or guest call direct.

One fresh, ordinary `release-fat` process from `world_pristine` (no `RAVA_RESTORE`, checkpoint, or
stackful environment) reached `Done (19.949s)` at **68.638 s wall**. This is not the target and
not a demonstrated speedup against the prior fat-LTO samples.

A separate diagnostic boot with `RAVA_LANE_PROBE=1000` and `RAVA_LANE_OUTER=1` reached
`Done (85.055s)`. Its time is invalid for performance comparison because the reporter writes once
per second. It did, however, re-confirm the residue at the actual new revision: at 88.5 s the
region had **86,657,843** resolve-lock acquisitions, with `worker_direct_hit=0`. The leading
locked callees were `Predicate#test` and `Function#apply` (about 412k each), then
`AtomicLong#get` and `AtomicLong#compareAndSet` (about 205k each).

This rules out treating the direct-cache borrow correction as a throughput result. A valid
AtomicLong increment must instead provide a real atomic cell for get/CAS and account for handle
reuse, GC retirement, and heap snapshot/restore. A raw handle-keyed side table without those
lifecycle rules would be unsound.

## What the ring actually locks

| callee | acquisitions | shape |
|---|---:|---|
| `java/util/function/Predicate#test` | 320,911 | guest lambda dispatch |
| `DensityFunction$SinglePointContext#<init>` | 320,911 | record construction |
| `java/util/function/Function#apply` | 320,910 | guest lambda dispatch |
| `java/lang/ThreadLocal#get` | 320,910 | native-served |
| `java/lang/ThreadLocal#set` | 320,910 | native-served, **writes** |
| `java/util/Set#contains` | 320,910 | native-served, pure read |
| `Climate#target` | 320,910 | static factory → construction |
| `DensityFunctions$Spline$Point#<init>` | 320,910 | record construction |
| `AtomicLong#get` | 195,315 | native-served, pure read |
| `AtomicLong#compareAndSet` | 195,315 | native-served, **writes** |
| `Climate$RTree$Node#search` | 49,665 | guest method |
| `BlockPos#<init>` | 657 | construction |

Eight callees at ~320,910 — one per sample — are ~2.57M of the ~2.88M. **The residue is eight
sites, not four**, so serving any one of them removes about an eighth.

## Why they can never enter the direct cache

A native-served method can never become direct-callable, so it can never be cached, so it is
`unknown` forever and falls to the lock on every call. That is not coldness and warming cannot fix
it: `RAVA_PILOT_N` was added to test exactly that, and **three serial pilot tasks instead of one
added ONE entry to the freeze table** (`stored` 6344 → 6345). Warming is saturated.

This is why `ws_arraylist_get` exists as a hand-written arm rather than as a cache entry. **The
WS-native arm is the only mechanism available for this residue**, and the work is therefore
bounded and enumerated rather than open-ended.

## Which of the eight can actually be served, and which cannot

**Served.** `Set#contains` → `Vm::ws_list_contains` (`RAVA_WS_SET_CONTAINS`, now default-ON). `&self`
by construction, runs the same linear scan with the same key function over the same array as the
locked path, so the answer is bit-identical.

> The first version of that arm served ~3% of what it was written for. The census names the SITE,
> whose declared owner is the `Set` interface, but the concrete receiver in the ring is
> `java/util/ArrayList`. Gate on what arrives, not on what the site declares.

**Checked and NOT servable after all.**

* `AtomicLong#get` (195,315) reads `$val` and is a pure read in isolation — but in the ring it is
  paired one-for-one with `compareAndSet` (195,315), i.e. it is a CAS LOOP. Serving the read
  unlocked while the CAS stays locked introduces a real race, and Rava's CAS is a plain
  read-compare-write rather than an atomic, so it cannot arbitrate one.
* The three construction sites (`SinglePointContext#<init>`, `Spline$Point#<init>`,
  `Climate#target`, 962,731 together) looked like the biggest available bloc, and they are not
  available: a record `<init>` is GUEST BYTECODE. Running it needs the interpreter, the interpreter
  needs `&mut Vm`, and the exclusive `&mut Vm` is exactly what the lock provides. The allocation is
  fine on a worker; the constructor body is not.

**Not servable as the engine stands.**

* `ThreadLocal#get`/`set` (641,820). **Rava's `ThreadLocal` is not thread-local**: the value lives
  in `$tlval`/`$tlset` on the ThreadLocal object itself, one copy for all threads. That is correct
  today only because the lock serialises a whole task, so one worker's 3249 get/set pairs never
  interleave with another's. Serving them unlocked without real per-thread storage would let
  workers overwrite each other. Real thread-locality needs GC-traceable per-thread storage — the
  worker's own arena segment — not a Rust side table, which the collector would not trace.
* `AtomicLong#compareAndSet` (195,315). Implemented as a plain read-compare-write on a heap field,
  not an atomic. Unlocked, two workers can both observe the old value and both succeed.
* `Predicate#test`, `Function#apply` (641,821). Guest lambda dispatch — that *is* the general path.
  These need the lambda target served by the direct/PIC path, not a native arm.

## Consequence for the goal as stated

Serving `ThreadLocal.get/set` + `Set.contains` + `Predicate.test` addresses **44%** of the
acquisitions, and two of those three are in the not-servable column above. Even done perfectly the
lock still covers the majority of the region, so the ring does not go multi-core on that set alone.

**After checking all eight, exactly ONE is servable by a native arm: `Set#contains`, 320,910 of
2,884,000 — 11%.** Every other site either runs guest code (constructions, `Predicate#test`,
`Function#apply`, `RTree$Node#search`) or writes shared state (`ThreadLocal#set`,
`AtomicLong#compareAndSet`), and `ThreadLocal#get` cannot be split from its `set`.

That is the ceiling of the WS-native-arm approach, and it is not close to removing the lock. The
common cause is the same one this arc found earlier: **the lock is not merely preventing races, it
is what supplies the exclusive `&mut Vm` that `opcodes::run` requires** (374 `&mut Vm` sites in the
tree). Anything that executes guest code on a worker needs that exclusivity, so it cannot be
handed a lock-free arm — only a lock-free VM.

## ThreadLocal pre-lock arm - REJECTED for wall (2026-08-02)

The existing `tl_worker` storage is per-worker and sound for a lane task. A tracer-bullet change
called `Vm::ws_threadlocal` from `rava_dispatch_miss` before the resolve lock, preserving the locked
fallback for a cold `get`, a supplier, or any shape mismatch. The diagnostic census confirmed the
mechanism: ordinary worker `ThreadLocal#get` calls fell from the prior hundreds of thousands to 127
cold gets in the sampled region.

It did not improve the metric that matters. Reversed fresh-process `release-fat` pairs on the same
`server.jar` were:

| pair | baseline wall / Done | pre-lock wall / Done | baseline minus pre-lock wall | baseline minus pre-lock Done |
|---|---:|---:|---:|---:|
| 1 | 73.8 / 25.210 s | 79.5 / 20.523 s | -5.7 s | +4.687 s |
| 2 | 68.7 / 20.515 s | 69.9 / 20.285 s | -1.2 s | +0.230 s |

Both wall pairs favor the baseline. The runtime arm was reverted in `e7a606c`; the census result is
kept as attribution, not as a speed claim. This is another case where removing a measured lock path
does not reduce external wall because the remaining guest and worker critical path dominates.

## ETW CPU attribution: serial boot versus the ring workers (2026-08-02)

This is diagnostic evidence only, captured from a fresh `release-diag` execution of the ordinary
`server.jar` path. It is not an acceptance benchmark: the run reached `Done (27.066s)` and consumed
82.4 s external diagnostic elapsed time. No restore, checkpoint, snapshot, or stackful controls were
present.

The server log partitions that run into 31 s from process start to `Environment:`, 22 s from
`Environment:` to the server-start/recipe marker, then roughly 27 s through world preparation and
`Done`. ETW context-switch accounting for `rava.exe` PID 26580 gives 143.569 CPU seconds:

| thread | CPU seconds | interpretation |
|---|---:|---|
| 14876 | 56.215 | coordinator / serial boot |
| 27732 | 21.968 | ring worker |
| 27232 | 21.906 | ring worker |
| 29700 | 21.775 | ring worker |
| 29908 | 21.614 | ring worker |
| all remaining RAVA threads | 0.091 | negligible |

Therefore the final ring work is genuinely concurrent: four workers each consume about 22 CPU
seconds over its approximately 22-second wall interval. It is not main waiting while one worker
computes. The radical wall problem is instead the preceding sequential coordinator path, which alone
has 56.2 CPU seconds and spans the two early log intervals.

The merged ETW stack report resolves the RAVA PDB. Across the process, `rava_fu_invoke` is on 74.08%
of inclusive samples; its call chain contains `resolve_lock_acquire` on 47.53% and a contended Rust
mutex on 46.97%. On the coordinator thread specifically, the leading inclusive paths are `Vm::jit_call`
(34.73%), `Vm::jit_call_body` (34.63%), `rava_dispatch_miss` (29.82%), `invokevirtual` (29.39%),
`Vm::invoke` (18.85%), and the native owner-dispatch cascade (about 14.5%). Cranelift compile is only
about 5.1% on that thread. The dynamic guest machine-code leaf remains an ETW `unknown` frame, as
expected for JIT memory, but the host callers identify the serial invoke/dispatch path unambiguously.

This both explains why the pre-lock `ThreadLocal` tracer bullet could not move wall and rules out
another bounded cache as the path to `wall < 20s`: the required work is generic, lock-free execution
of guest invoke/dispatch on a VM design that does not require the coordinator's exclusive `&mut Vm`.
That is a runtime architecture project, not a workload-specific admission rule. Before implementing it,
the stale `&Vm` leaf plan must be replaced with a safe ownership, GC, native, and reflection boundary
design.

Reproduction (administrator shell for recording): record with WPR CPU, then merge before decoding:
`xperf -merge input.etl merged.etl`; set `_NT_SYMBOL_PATH` to the matching `target\\release-diag`
directory plus the Microsoft symbol server; then run
`xperf -i merged.etl -symbols -a stack -pid <pid> -butterfly 1`. The unmerged trace cannot safely
resolve private PDBs. Temporary ETL, report, and symbol-cache files are intentionally not retained.
