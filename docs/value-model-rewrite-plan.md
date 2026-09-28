# Value-Model Rewrite — Single-Store, Tag-Discrimination Retirement

> ⛔⛔ **PREMISE RETRACTED — READ LEDGER ROW 133 BEFORE SCOPING THIS.** The line below prices this
> arc against "the **81% JIT_EXEC** MC-boot wall". **That number is a category-sampler artifact**
> (ledger rows 103 and 129): the sampler's guard stays set while the HOST works on behalf of
> compiled code, so callbacks and bridges are billed as compiled time. The instrument that reads
> the instruction pointer puts the current boot at **7.2% inside compiled code**.
>
> The DIAGNOSIS still holds and is visible in the machine code — one `BitSetDiscreteVoxelShape`
> constructor compiles to 2,080 instructions, **1,235 of them `movq` (59%)**, in a 2,432-byte
> frame. The ARITHMETIC does not: removing all tag discrimination is bounded by 7.2% of wall,
> and the flip's measured 2.43× microbench prize applied to that share is **≈4% of wall**.
>
> ⚠ **THE 4% BOUND APPLIES TO THIS DOCUMENT'S NARROW READING ONLY (ledger row 134).** Retiring
> the COMPILED arms' tag tests is capped by the 7.2%. Retiring the HANDLE HEAP is not: the host
> functions that dominate the boot's flat tail — `ref_parts`, `owner_bits`, `field_id_existing`,
> `box_ref`/`unbox_ref`, `super_chain`, the whole `jit_call` bridge — exist *because* of the
> side-table heap, the string-keyed field ids and an enum `Value` that must be decoded at every
> boundary. Those live in the **92.6%**. This plan describes both changes and prices neither
> separately.
>
> ⛔ **THAT EXPERIMENT IS NOT RUNNABLE (ledger row 135).** The flag is sound only on a program
> whose graph is 100% arena-born — and such a program has no handle-representation host traffic
> to remove, so the quantity is zero by construction. It breaks the boot AND `ChunkFill`.
>
> ⇒ **Bottom-up instead, from measured rows:** the serial prefix's representation lookups —
> `ref_parts` 2.8%, `owner_bits` 2.2%, `field_id_existing` 2.1%, `super_chain` 1.1%,
> `get_or_load` 1.0% ⇒ **≈9.2%** — plus the 7.2% compiled share. The `jit_call` bridge (13.5%)
> is dispatch, not representation, and is not counted.
>
> ✅ **REVISED UP (ledger row 136): ≈25% of wall, `44.3 → ~33`.** The host table was printing
> only its first 30 rows; at 120 it covers 78.2% and the recovered 46 points are **frames**
> (`expand_locals_into`, `pool_take`/`pool_give`, `drop_in_place<Frame>` = 3.2%), **field access**
> (`field_id`, `get_field(_hint)`, `put_field`, `intern`, `FieldBlob::reserve` = 3.9%),
> **allocator** (2.8%) and **arena/GC** (1.2%) — all of them consequences of the side-table heap
> and the boxed `Value`. **This is the largest single identified lever in the tree.**

**Chosen 2026-07-24** (user directive: "possibly changing architecture") as the path to the
81% JIT_EXEC MC-boot wall. Multi-session/multi-year. Gate every step on the REAL full boot to
Done (bench/boot_pin_ab.ps1 / phaseA_mc.ps1) AND canonical 8790 — NOT ChunkFill alone (ChunkFill
FILL is a broken proxy: it over-weights worldgen, which the real cached-world boot spends ~11ms in;
see [[rava-realboot-profile-2026-07-24]]).

## The wall
Value = i64, low-2-bit tag: 0b00 raw arena/pinned pointer, 0b01 handle (h<<2)|1, 0b10 array,
0b11 boxed (arena_box side-table idx: boxed prims / String). Compiled code tag-DISCRIMINATES every
ref deref (band/cmp/select) — ~200 field loads/method in hot code. That is the JIT_EXEC 81%.

## Why the two prior attempts failed (do not repeat)
- **Big-bang "assume 0b00" pilot**: CRASHED at 1.3s — handle/box refs are load-bearing from boot start.
- **Pinned-mirror DUAL-STORE** (2026-07-24, commits ef3b5c8..b45ac69): bit-exact, worldgen −14%, but
  REGRESSED the real boot +2.3s — dual-store MAINTENANCE (write FieldBlob AND mirror for the boot's
  large object population) outweighs the read benefit; the real boot barely does worldgen. LESSON:
  **coexistence/dual-store is a net loss. The win needs a SINGLE store, objects BORN in it (zero
  coexistence overhead), and all refs provably 0b00 so the check is statically removable.**

## Target (agy-arbitrated design)
ONE store: extend the arena to hold ALL objects; retire the handle heap (FieldBlob / Value::Ref) and
the box side-table. Every non-null ref satisfies `ref==0 || ref&7==0` (raw 0b00 pointer). A migrated
class is BORN 0b00 in the arena — readers/writers hit the arena cell directly, NO dual-store. Unmigrated
classes keep 0b01 handles, decoded via `decode_arena_cell` (opcodes.rs) as the coexistence SAFETY NET
during migration. GC: the moving arena already relocates arena refs; the hazard is native Rust code
holding Values across safepoints (must root/forward) + pinned vs moving tradeoff (pinned = no relocate
but leaks; moving = GC-relocate cost). Box arm: boxed prims / String become fixed-layout arena 0b00
objects (+ small-int/String interning), per agy.

## MEASURED 2026-07-25 — the interim GC cost is NOT the arena-lambda regression
The section below predicted that a born-arena population regresses the boot because the MOVING
collector COPIES it at every collection, and prescribed born-PINNED lifetime routing as the
mitigation. **For lambdas that prediction is REFUTED by direct measurement.** `RAVA_GC_STATS`
(`nextgen::gc_stats_*`, collection counts + tenured sweep volume + GC wall time + per-class tenured
histogram) on canonical ChunkFill, arena-lambda OFF vs ON:

| | OFF | ON | delta |
|---|---|---|---|
| SETUP_MS | 61 707 | 69 524 | **+7 817 (+12.7%)** |
| GC wall time | 1 129.7 ms | 1 406.5 ms | **+276.8 ms (3.5% of the regression)** |
| minor / major GCs | 25 / 0 | 34 / 0 | +9 / 0 |
| tenured live | 60.2 MB | 61.7 MB | **+1.5 MB** |
| arena-lambdas born | 0 | 2 592 357 | — |
| …of which reach tenured | — | 41 588 | **1.6%** |

**98.4% of arena-lambdas die in the nursery and are NEVER copied** — precisely the case a Cheney
collector handles for free. The plan's premise (lambdas are 30% of the LIVE handle population ⇒
they are long-lived ⇒ they are copied repeatedly) confuses the live POPULATION with the allocation
STREAM: the live set is long-lived, but it is 1.6% of the births. Two further facts kill the
mitigation outright: there are no MAJOR collections during a boot (`arena_should_major` fires at 75%
of a 1 792 MB tenured cap; the boot's tenured is ~60 MB), so nothing is ever re-copied; and routing
2.59 M births into the never-freeing 256 MB `PINNED_CAP` would exhaust it (~166 MB on ChunkFill
alone) and hit `pinned_alloc`'s assert. **Born-pinned lifetime routing addresses at most 277 ms of a
7.8 s problem and is not viable for this population.** It may still be right for genuinely immortal
singletons (enums — STEP 3, where it was measured to help); it is wrong as a general policy.

**Where the regression actually goes** (same A/B, `RAVA_PROFILE_WALL`, samples ×10 ms):
JIT_EXEC +3.8 s, NATIVE_BRIDGES +2.7 s, VM_DISPATCH +2.2 s (**more than doubled**, 2.0→4.2 s),
INTERP_EXEC −0.6 s. There is NO single dominant term: the cost is DIFFUSE across the CONSUMER
surface — exactly the unfinished B3 work item, not the collector. What was cheaply recoverable
has been recovered (see `ArenaLambdaShape`): the per-instantiation shape derivation and the
`$fi` box leak were worth −2.1 s of ChunkFill SETUP and **−198 MiB peak working set** (arena-lambda
ON is now 736 MiB vs 934 MiB OFF, i.e. the representation is a clear MEMORY win). The residual
~+14.8% SETUP is the un-migrated consumer layer.

**Read for the arc:** the single-store direction is NOT refuted — `bench/ShapeOr` is **−23.6%** with
the flag ON, bit-exact, so serving captures from flat inline cells pays exactly as designed. What is
refuted is the claim that the interim cost is a COLLECTOR cost with a routing fix. Finishing lambdas
means finishing B3 (the ~30 consumer sites across 12 files), and each one needs the wall attribution
above rather than a GC hypothesis. Gate progress on VM_DISPATCH/NATIVE_BRIDGES shrinking.

## CRITICAL — interim GC cost & lifetime routing (born-pinned) [learned from STEP 3 enums]
The single-store rewrite is structurally BETTER than the failed dual-store mirror (no double-write, no
coexistence maintenance), but it has ONE interim cost: born-arena objects live in the MOVING (Cheney)
collector, which COPIES live objects every GC. For SHORT-LIVED objects this is cheap (they die young, never
copied). For LONG-LIVED objects (enum singletons, registry entries, interned constants) it is pure repeated
copying cost with no benefit until the phase-6 flip — STEP 3 enums showed +130MB peakWS / +1.8s (within noise
but real, and it would ACCUMULATE as more long-lived classes migrate = the same "interim regression, far
payoff" trap that killed the pinned mirror, in slower motion).
**MITIGATION (mandatory for viability): route by LIFETIME.** Long-lived / stable-address objects → born-PINNED
(the non-moving pinned region, 0b00, never copied — nextgen.rs pinned_object_alloc, lock-free, header already
arena-compatible). Short-lived → the moving arena. This keeps interim cost ~0 (pinned objects aren't copied;
moving-arena short-lived objects die before copying) while every ref still trends to 0b00 for the phase-6 flip.
PREREQUISITE INFRA: a **pinned-object ROOT WALK** — the GC must scan born-pinned objects' ref cells as roots +
trace INTO them (the mirror directory PMM_DIR alone is not enough; born-pinned needs a general pinned-object
enumeration). This same infra is lambda-B4's requirement (non-capturing singletons need a stable address), so
it is HIGH-LEVERAGE: unlocks the enum born-pinned optimization AND lambda progress.
✅**BUILT AND WIRED — commit 69cd607 (2026-07-24), `nextgen/pinned.rs::pinned_region_root_walk`, called from
`vm/gc_collect.rs` under `pinned_enum_enabled`.** This paragraph read "Build it next" until 2026-08-20 and cost
a session's start re-implementing it; the DONE record was 140 lines further down (B4). ⇒ **What is actually
open is the step AFTER it**: re-route enums (STEP 3) to born-pinned and confirm the +1.8s is recovered toward
neutral. Object-level enumeration is `pinned_walk` / `pinned_walk_from`, which the root walk is written on.

## THE PRIZE IS MEASURED (2026-07-24) — 2.43× on ref-deref-dense code
`RAVA_JIT_BARE_REF_LOAD` (commit 72bc253, default OFF, **measurement only — UNSOUND on a mixed
graph**) emits step 6's Bare Dereference Mode TODAY: the compiled arena arms of the reference
getfield/putfield emitters (`emit_arena_getfield_ref_flat`, the arena arms of
`native_getfield_ref_const_arena_dual` / `native_putfield_ref_arena_dual`) drop the low-2-bit
classify and become one `load.i64` / `store.i64`. Run it on a program whose graph is provably
100% arena-born (plain user classes) and the endgame is measured directly, no migration needed.
`bench/bare_ref_pilot.ps1`, median of N, P-cores + High-Performance, SUM identical in all configs:

| bench | tagged (today) | bare (endgame) | JVM | prize | vs JVM |
|---|---|---|---|---|---|
| FieldsRef (ref-field hot loop) | 231ms | **95ms** | 24ms | **2.43×** | 9.65× → **3.97×** |
| Dispatch (aaload + prim field) | 157ms | 163ms | 51ms | 0.96× | 3.08× → 3.20× |
| NestedMath (pure numeric, CONTROL) | 14ms | 14ms | 7.8ms | 1.00× | 1.79× → 1.79× |

The CONTROL at 1.00× proves the flag touches only reference derefs. Dispatch at 0.96× is
consistent, not a counter-example: its hot path is `aaload` + a primitive field, and reference
ARRAYS are still 0b10 handles (plan step 2) — so nothing there is bare-able yet. **Conclusion:
the prize scales with ref-field-deref density and is 2.43× on that axis — an order of magnitude
above the old "~10%" guess in [[docs/bseries-retirement-roadmap.md]]. The arc is worth finishing;
the remaining question is COVERAGE (reaching 100% 0b00), not payoff.** Corollary: step 2 (arrays)
is not optional — Dispatch shows array-indexed code gets NOTHING until ref arrays are 0b00 too.

### CALIBRATION — the 2.43× is the CACHE-RESIDENT ceiling (same day)
`bench/BigGraph.java` (new): a FULL binary tree, 262,143 nodes ≈ 8 MB, traversed in its
ENTIRETY so the working set IS the graph. `FieldsRef` cannot measure this — its 17-node
graph lives in L1, so it isolates the tag ALU and nothing else. Medians of 3, SUM identical,
vs JVM 57.0 ms:

| config | HOTNS | vs JVM |
|---|---|---|
| tag + BFS (today) | 364 ms | 6.4× |
| bare + BFS (post-flip) | 332 ms | **5.8×** |
| bare + DFS (post-flip + copy-order) | 331 ms | 5.8× |

**Removing tag discrimination is worth ~9% here, not 2.43×** — on a cache-bound pointer
graph the loads dominate and the tag ALU hides under the misses. The real boot mixes both
regimes, so the flip's payoff is a RANGE whose lower end is single-digit percent. Equally
important: the residual **5.8× vs JVM on memory-bound work is NOT the value model**, so that
share of the gap needs its own root cause (object size/layout, allocation rate, GC cost per
node) and will NOT be fixed by finishing this arc.

### COPY-ORDER LOCALITY — measured, built, REFUTED as a lever
`nextgen::edge_locality_census`: of 1.27M live arena→arena edges, **82.3% span ≥4096 cache
lines**, only 1.6% within two — parents and children are essentially never co-located. That
motivated `RAVA_GC_DFS_COPY` (approximately-depth-first evacuation, Moon; `GenHeap::dfs_drain`,
default OFF, byte-identical when off, all gate hashes 8790f04ace726ecf). It does not pay:
ChunkFill 6-run interleave = SETUP tie (60550 vs 60455), FILL **+3.4%** (19143 vs 19789); and
on BigGraph WITH the bare-load flip it is 331 vs 332 ms, i.e. nothing. Kept as ready
infrastructure; re-test on the real workload after the flip, not before.

## COVERAGE IS MEASURED TOO (2026-07-24) — 61% bare-ready, blockers split 46/36/18
`RAVA_CELL_CENSUS` (`nextgen::cell_census`, printed from the arena-collection hook in lib.rs)
walks every live arena object at each collection and histograms its REFERENCE cells by
low-2-bit tag, per owning class. Real MC boot (default flag stack, arena-lambda OFF), stable
across the run's later snapshots:

```
ref cells 4,571,750:  0b00 2,806,830 (61.4% BARE-READY)
                      handle 861,229 (46% of blockers) | array 472,885 (36%*) | box 430,806 (18%*)
   *share of the 1,764,920 cells that block the flip
top holders: IndirectMerger (array), BlockState (handle+array), Reference2ObjectArrayMap (array),
             DFU Tag / Pair / Named / TagType (box+handle), ArrayVoxelShape / SubShape / SliceShape
             (handle), BlockStateBase$Cache (array)
```

**CONTENT of each bucket, measured (the buckets are MIXTURES, and this reorders the work):**
- **Box = 97.9% STRINGS** (Str 197,677 | Int 4,076 | Float 31 | Double 10 | sentinel 5). The
  mechanism for strings already exists (`RAVA_ARENA_STR_REFCELL`), is bit-exact over 3 ChunkFill
  runs and time-neutral over an E-core boot pair (193.287 off vs 193.658 on). So this whole
  bucket reduces to "close the two fallbacks + flip default-ON"; the elaborate arena-boxed-
  primitive form (with its `Integer.valueOf` identity-interning hazard) would buy **2%** and can
  be skipped.
- **Array is WORSE than the share suggests.** By element type: 356,121 cells with NO descriptor
  (internal growable storage — `$elems`/`$sb`/`$buf`, which are not Java arrays at all and guest
  code never `arraylength`s), 217,068 `Object[]`, 45,165 `VoxelShape[]`, 10,184
  `Climate$Parameter[]`, 6,198 `RTree$Node[]`. All primitive arrays together (boolean 34,603 +
  double 17,673 + char 4,326 + float 2,023 + byte 897) are **8.5% of the bucket** ⇒ the planned
  "primitive arrays first, they are tractable" increment would clear ~3% of blockers, i.e. nearly
  nothing. The value sits in REFERENCE arrays + internal storage — precisely the hard parts
  (an `ALL_REFS` shape flag for the Cheney scan, and reclassifying growable storage out of the
  array bucket entirely).

Read: **the graph is already ~61% single-store**, and the residue splits along this plan's steps —
box arm 18% (step 1, ALREADY BUILT as `RAVA_ARENA_STR_REFCELL`), arrays 36% (step 2), handles 46%
(steps 3-5) — but ORDER should follow cost and information, not cell share. This is a far better starting position
than the pinned-mirror arc's 26.87% 0b00 hit rate, and it makes the remaining work rankable by
measured cell counts instead of by census-of-allocations guesswork.

## Compiler proof = GLOBAL PHASE GATE (not per-site)
Per-site "field F of class C is 0b00-only" FAILS (polymorphism, java/lang/Object slots, reflection
pollute fields with 0b01/0b11). Instead: during migration the JIT keeps emitting the tag check (the
safety net). ONLY after 100% handle retirement (allocation count of legacy heap == 0, legacy types
deleted) does the pipeline flip GLOBALLY to Bare Dereference Mode (`ptr = load.i64 base,off` — no
band/cmp/select). => the ~200 tag ops/method collapse to 0. **The wall-win payoff is at the FINAL flip
only; steps 1-5 are correctness/migration progress with NO wall win (bank bit-exactness + GC-stress).**
Possible SMALL interim wins: a migrated class's reads lose the handle→addr indirection (arena obj = direct
addr) even with the tag check still present — but watch for moving-GC-relocation cost regressing the boot
(the pinned-mirror RSS/GC-pacing lesson). Measure each step on the REAL boot.

## Risk-ranked increment roadmap
1. **Box Arm Retirement [LOW]** — boxed prims/String in ref fields → fixed-layout arena 0b00 objects
   (+ interning). Gate: 8790 + GC-stress clean + full boot Done no-regress. FIRST INCREMENT.
2. **Array Unification [MED → actually a multi-increment SUB-ARC]** — primitive & ref arrays born in arena
   as 0b00; zero Value::Array. AUDITED 2026-07-24: NO arena-array form exists (unlike arena-strings which
   were already first-class Value::Obj). Value::Array(u32) is a HANDLE into heap.arrays: Vec<ArrayObj>
   (heap.rs:448/977) + parallel flat ArrayDesc (heap.rs:515); consumed at **310 match sites across 14 files**
   (natives 98, stream 46, reflect 34, lib 28, opcodes 26, nio 25, heap 19, …) all keyed off the handle
   (aaload→array_get(h), aastore→array_set(h), arraylength→array_len(h), the data-parallel fork
   array_base_ptr, SnapshotArrays ser/de). The encode-in-ref-cell flip is NOT separable: a born-0b00 array
   decodes to Value::Obj, and the next `matches!(v, Value::Array(h))` falls to the non-array error arm — so a
   non-dual-store correct slice needs the WHOLE consumer layer first. Sub-arc sequencing:
   - **2a. Arena-array INFRA (bounded, dormant, bit-exact):** an `[arena-array]` class (like string_class_id),
     header = element-kind + length + inline flat buffer; `tlab_alloc_array_prim(kind,len)->Option<ArenaPtr>`
     (never-collect like tlab_alloc_string); `is_arena_array` + arena_arr_len/elem_kind/base_ptr + JIT offset
     consts. PRIMITIVE arrays = ref-mask-EMPTY (GC copies bytes, tractable first); REFERENCE arrays need a real
     ref-mask (GC traces element cells — HARD, defer).
   - **2b+. Consumer migration ONE KIND AT A TIME:** add `Value::Obj(p) if is_arena_array(p)` arms to the array
     opcodes + instance_of/val_class_name (getClass/instanceof agreement — the gson trap step 1 hit) + the ~98
     natives + stream/reflect/nio (arraycopy/clone/Arrays.*) + the JIT native-array path + array_base_ptr fork
     + snapshot. Start byte[]/int[] (ref-mask-empty, highest boot volume: BitStorage, voxel grids). Gate each
     kind on 8790 + full boot. THEN flip encode_arena_cell Array arm (+ unify_pack) born-0b00 behind
     RAVA_ARENA_ARRAY_REFCELL, 0b10 arm as the coexistence net.
   Hazard: element alignment / bounds; ref-array GC tracing.
3. **Synthetic Class Migration [MED → LAMBDAS = a multi-increment SUB-ARC]** — StringBuilder / Holder /
   internal closures born in arena. LAMBDAS audited 2026-07-24 (30% of handle heap, biggest prize): NOT a
   bounded slice — 4 structural blockers: **B1 (the true gate) heterogeneous per-instance typed captures vs
   the static per-class ref_mask** (nextgen.rs:197/2209 — the moving GC traces cells by a fixed per-class
   mask; lambda captures mix Int/Long/Double/Ref/Obj/Str/Null per-instance; can't be ref-mask-0 like
   arena-strings (they hold live refs to forward) nor all-ref (a prim Long whose bits look like a young arena
   addr → CHENEY-MISS corruption nextgen.rs:2199). Needs a NEW per-object inline capture-kind bitmap the
   Cheney trace consults — genuinely new GC infra); **B2** synthetic $fi/$cmp/kind/site have no arena cell
   (arena_getfield panics) → need a bespoke lambda arena format + content-interned u32→Arc<LambdaSite> table
   (the SOUND non-recyclable-address identity — dedup must NOT key on p.addr(), opcodes.rs:352 recycle hazard);
   **B3** ~30 consumers across 12 files match Value::Ref(h)/handle:u32 (lambda_of/invoke/comparator/λ-PIC —
   opcodes/stream/natives/reflect/gilfree_cf/detached_task/comparator_plan/…); **B4** the λ-PIC machine-code
   dispatch (lib.rs:10071, the hottest lambda path, IndexConsumer.merge millions×) truncates raw[0] to a u32
   handle + checks REF_TAG + assumes stable-address singletons (indy_singleton_handles) → needs born-pinned
   objects + a pinned-object root walk (not built). No bounded subset: capturing-only hits B1; non-capturing-
   only hits B4/B2. Sub-arc order: (i) per-instance capture-kind GC representation [B1, the gate, independently
   GC-stress-testable] → (ii) lambda arena payload + interned site table [B2] → (iii) born-pinned + pinned root
   walk [B4 — DONE commit 69cd607: pinned_region_root_walk + born-pinned birth] → (iv) consumer
   Value::Obj-if-arena-lambda arms one group at a time [B3]. NEXT = B1 (capture-kind GC representation).
   Hazard: stale native ptrs across interpreter call boundaries; identity soundness on a moving object.
   **STATUS 2026-07-24: B3 is NOT done and ChunkFill is NOT a sufficient gate for it.** With
   RAVA_ARENA_LAMBDA=1 ChunkFill is bit-exact 8790 (SETUP 90420 / FILL 22091, baseline range) but the
   REAL boot dies ~2min in, during the resource/recipe reload: `[EXECUTOR] uncaught guest task error:
   Throw(Ref(0))` + a Rust stack overflow, preceded by an exception object carrying the WRONG class
   (`java/util/concurrent/ConcurrentHashMap: rava: network access is unavailable` where the same throw
   prints `java/io/IOException` with the flag off) ⇒ a Value decoding to an unrelated object. Excluding
   gil-free WORKERS from arena-lambda birth (commit 72bc253) removed the worker-segment aborts but NOT
   the crash, so the parallel lane is not the cause. Fixed on the way (commit d010559): every stream
   consumer that bound the fn ONCE before a per-element loop went stale after the first guest GC —
   `with_rooted_fn`/`rooted_fn` now re-read the FORWARDED fn from gc_protect per element. RAVA_GC_VALIDATE
   reported 300 dangling edges but all `reached=false` and including plain arena classes (ArrayVoxelShape,
   gson nodes) ⇒ dead-object noise, not a proven root gap. NEXT for this sub-arc: finish the B3 consumer
   surface (~30 sites/12 files still matching `Value::Ref` for a function: comparator_plan — which also
   silently loses the sort key-cache for an arena-lambda keyfn — detached_task, gilfree_cf, stream:1435,
   opcodes:3498) before re-gating on the full boot.
4. **Core Java Lib [MED-HIGH]** — HashMap / ArrayList / Throwable born in arena. Hazard: complex graph roots
   in mark-compact.
5. **Legacy Heap Deletion [HIGH]** — remove FieldBlob, handle tables, Value::Ref/Array. Hazard: residual
   unmigrated fallback.
6. **JIT Bare-Load Flip [HIGH] = THE PAYOFF** — emit bare load_u64 for all ref field loads after 100%
   retirement. Hazard: wild deref if ANY non-0b00 cell persists. Gate: 8790 + full boot + JIT_EXEC wall drop.

## Migration surface (audited 2026-07-24)
BIRTH RULE (opcodes.rs:2394-2495 new_object_value / lib.rs:17250 arena_new_site): arena-born 0b00 iff
NOT native-backed (is_native_backed_alloc: collection/map/entry/io) AND NOT enum AND NOT native_super AND
real loadable bytecode class AND initialized AND complete layout cached; else Value::Ref handle (0b01).
Heap::alloc/alloc_interned ALWAYS handle; 0b00 minted only by tlab_try_alloc/tlab_alloc_string/arena-map.
HANDLE CENSUS (RAVA_CENSUS, handle heap only): 2.84M live / 1270MB. **#1 lambdas 30.4% (~0.9-1.1M)**,
obj.struct 23.9%, then native collections/entries, enums. Arena-born already: MC-own worldgen classes
(Climate$Parameter, Spline$Point, DataResult$Success, …) + arena-strings 30.6% of alloc volume.
GC READY: arena rooting/forwarding COMPLETE + reusable (arena_collect_jit, protect/protected shadow-root
arena_map.rs:59, forward_stale_arena read-barrier opcodes.rs:2630) — NO new GC infra for a class-flip.
BLOCKERS per bucket: synthetic $-fields (enum $enumOrd/$enumName, collection $elems/$map, io $buf) have NO
arena cell → arena_getfield panics (opcodes.rs:2655); migrating needs promoting them to real layout cells
first. Lambda: Box<Lambda>{captured:Vec<Value>} payload + impl-method identity + dedup address-recycle hazard
(RAVA_M3_LAMBDA_DEDUP, [[rava-lambda-capture-verdict]]). Pinned region reusable for BORN-PINNED stable-address
objects (single-store, not mirror) — needs a birth-site choice + a pinned-object root walk.
QUICK-WIN RANK: (1) RAVA_ARENA_STR_REFCELL already built (step 1, 8e8d18e) — cheapest, LOW. (2) LAMBDAS
biggest prize 30% but MED (closure payload→inline cells + identity). (3) enums (promote 2 synthetic cells).
(4) RAVA_ARENA_MAPS built, low prize 2.9%.

## Method
Sub-agents code each increment on a clean context; I (+ agy) arbitrate. Every increment: born-in-store
(never dual-store), bit-exact 8790, GC-stress, and a FULL boot to Done with no regression, before the next.
Reuse where sound: the pinned-region alloc (nextgen.rs, now lock-free) + arena object layout/HDR + the
coexistence bridges (ref_equal/liveness/identity-hash/map-keys/getfield/putfield) already built for the
mirror — but SINGLE-store (drop the FieldBlob dual-write).

---

## Arc status, MEASURED 2026-07-28 (`bench/cell_census.ps1`)

The metric for this arc is the BLOCKING CELL COUNT, not wall time — steps 1-5 have no wall win by
construction. `RAVA_CELL_CENSUS` reports it, so each increment can be judged by its own metric
instead of by argument. Current tree vs this document's 2026-07-24 baseline:

| bucket | 2026-07-24 | now |
|---|---|---|
| box | 430,806 | **1,320** |
| handle | 861,229 | 516,499 |
| array | 472,885 | 205,508 |
| bare-ready | 61.4% | **65.03%** |
| total cells | 4,571,750 | 2,068,365 (723,327 block) |

### Step 1 (box-arm retirement) — VERIFIED
`RAVA_ARENA_STR_REFCELL` is default-ON (kill switch `RAVA_NO_ARENA_STR_REFCELL`) and the box
bucket is **99.7% gone**. It measured neutral on the clock, exactly as this plan predicts for
steps 1-5, and moved the metric that actually gates the final flip.

### Step 3 (enums) — ⛔ INERT AS BUILT, do not treat as a quick win
The QUICK-WIN RANK below lists enums at #3, and `RAVA_ARENA_ENUM` is already built and documented.
It does nothing on a real boot:

```
off : 2,068,365 cells, 65.03% bare-ready, 723,327 BLOCK
on  : 2,074,267 cells, 65.09% bare-ready, 724,131 BLOCK      <- noise; totals differ by 6k too
```

The target histogram is byte-identical armed — NoteBlockInstrument 30,837, PushReaction 30,837,
Direction$Axis$1/2/3 unchanged. Nothing migrates; it is not a partial effect. Gates stay bit-exact
ARMED, which is what an inert flag looks like.

Narrowed, not proven: BOTH birth sites consult the flag (`new_object_value` and its JIT twin
`arena_new_site`, the latter explicitly added "because every enum minted through a compiled `new`
stayed a handle"). The JIT site's condition is `is_enum || native_super || !initialized`, and enum
CONSTANTS are allocated inside their own class's `<clinit>`, when the class is by definition not
yet initialised — so the third term forces a handle for every constant however the flag is set.
**Put a counter at the birth site before changing anything.**

★ Two items this session were found already built and flagged off. One was real (step 1, verified
above); one is not. **"It exists behind a flag" is not evidence that it works — the census is.**

### Residue, by migration target
```
205,527  $Lambda                     <- dominant, and the hardest: 4 structural blockers
 50,855  BitSetDiscreteVoxelShape       plus a known real-boot crash (see step 3 above)
 36,179  Object2ObjectArrayMap
 32,530  DoubleArrayList
 30,837  NoteBlockInstrument   \
 30,837  PushReaction           |  enums ~101,290 = ~14% of the residue,
 39,616  Direction$Axis$1/2/3  /   but see the INERT finding above
 24,229  Climate$Parameter
```

Also measured, not yet actionable: **edge locality is poor** — 92.7% of the 852,769 arena→arena
edges span >= 4 KB, i.e. the arena graph has essentially no spatial locality.

---

# ⛔ THE PAYOFF IS PRICED — 5.6%, NOT 4x (2026-07-28)

Everything above assumes the final bare-dereference flip is worth enough to justify a
multi-session/multi-year arc. **That assumption is now measured, and it does not hold.**

## The flip, measured

`RAVA_JIT_BARE_REF_LOAD` is the emitter side of the final flip. It could not be priced on the real
workload (unsound while any 0b01/0b11 cell exists — a boot with it on dies before the density
functions compile) nor on `RingScan` (numeric hot body, never touches the arena ref-field emitters,
compiles byte-identically). `bench/BareRef.java` was built to satisfy both constraints at once: all
objects are its own plain classes, arena-born 0b00, so the flip is **sound** there — both arms print
`BAREREF_HASH=71199712585cb2a0` — and its hot loop is nothing but reference-field reads and writes.

```
BareRef.(LBareRef$Node;I)J     <- the most favourable workload the flip can have
  BARE off : blocks=388  tag_ops=196  total_ops=2198
  BARE on  : blocks=368  tag_ops=158  total_ops=2075
  tag ops removed 38/196 (19.4%) · TOTAL ops removed 123/2198 (5.6%) · blocks 20/388 (5%)
```

**On the best possible case, in the end state this arc spends years reaching, the flip removes 5.6%
of emitted operations.** (`bench/bareref_mix.ps1`.)

## Why so little — and what the 6.6x actually is

The ring's hottest compiled method, `DensityFunctions$ShiftedNoise.compute(FunctionContext)D`, is
852 blocks / ~8900 instructions for FOUR LINES of Java. Classifying those blocks
(`bench/block_anatomy.ps1`, `bench/guard_census.ps1`):

```
blocks=852   median block = 4 instrs   2555 movq
  contain a CALL            206  (24.2%)
  load/store, no call       112  (13.1%)
  BRANCH-ONLY               534  (62.7%)   of which:
        unconditional / merge   435  (81.5%)   <- block4: jump block5(v8)
        null / zero test         40  (7.5%)
        other conditional        29  (5.4%)
        REF_TAG (>>40)           16  (3%)
        value compare (PIC)      14  (2.6%)
```

The body is **not full of guard tests. It is full of the structural residue of guards.** A guard
emits test-block + two arms + **merge**, and four fifths of the branch-only population are merges
containing no computation at all. Each merge carries a block-param list and dozens of SSA aliases
(~11 aliases per merged value across 9 params in one sampled block) — the phi web Cranelift's
register allocator must then satisfy, which is the plausible source of the 2555 `movq`.

⇒ Removing a tag **test** deletes two compares and leaves the **diamond** — merge, phis, register
pressure — standing. That is exactly why the flip measures 5.6% and not 4x.

## What this means for the plan above

* The steps are still **correct** as a migration, and step 1 (box-arm retirement) is shipped and
  verified by its own metric (box bucket 430,806 → 1,320; `bench/cell_census.ps1`).
* But the plan's **premise** — that this arc is the way to close the boot's 6.6x CPU gap against
  HotSpot — is not supported. Do not start step 2 or 3 expecting a wall win.
* The measured cause points at a **codegen-architecture** lever instead: emit fewer diamonds —
  straight-line guards via conditional moves, or trap-on-fail, instead of branch-and-merge — so the
  merges and their phi webs never exist. That is independent of this arc and is where the evidence
  says the 6.6x lives.

  ⛔ **CORRECTION (2026-07-28) — "straight-line guards" does NOT reach the dominant source.**
  The merges are dispatch merges: `emit_dispatch_pic` creates exactly **12.0 blocks per call**
  (`RAVA_JIT_BLOCK_CENSUS`, `dfg.num_blocks()` snapshots) over 2948 calls, and the hot method's
  103 call sites × ~65% PIC-emitting × 12 ≈ **804 of its 852 blocks**. Enumerating the emitter's
  block-creation sites against a default build (heap_mode 2, ic_deep on, IC_PARALLEL and
  RAVA_DISPATCH_TABLE off) accounts for the 12 exactly:

  ```
  heap-Ref arm (6, measured by toggling RAVA_NO_JIT_HEAPPIC: 12/call -> 6/call)
        refpic  refhit  refdeep  refcheck  ref_split  + 1 from emit_heap_ref_class_key
  core (6)
        pic  hit  miss  merge  leaf_hit  deep_hit
  ```

  **Every one of these diamonds has a CALL in at least one arm** — hit calls the cached callee,
  miss calls the host, `leaf_hit`/`deep_hit` are a plain vs a rooted call, `refhit`/`refdeep` the
  same split for heap-Ref receivers. A conditional move cannot fold a call, and trap-on-fail cannot
  either: both arms are live, normal control flow, not a failure case. So the cmov/trap idea
  applies only to the pure-guard minority the census already sized at **16 of 435 merges** (the
  arena ref-cell classify) — and folding exactly those was built and measured: 852 → 836 blocks for
  **+390 instructions** (`RAVA_JIT_FLAT_TAGSELECT`, kept OFF).

  What remains structurally possible, in ascending order of disruption: (a) drop the
  `leaf_hit`/`deep_hit` split and always take the rooted arm — 2 of 12 blocks, at the cost of
  rooting every GC-leaf call; (b) tail-duplicate the continuation instead of merging — trades merges
  for code size, which a compile-once-and-move-on boot has repeatedly refused; (c) move the
  polymorphism out of the caller entirely — one unconditional indirect call to a per-site stub that
  the runtime patches, so the caller has no arms and no merge at all. Only (c) is sized like the
  goal, and it is a dispatch-architecture rewrite, not a flag.

  ⇒ **Fewer dispatch SITES** is the other half, and it is closed by prior measurement, not by
  argument: the C2-style inlining sweep regresses monotonically (86.9 → 93.0s) and devirt
  monomorphism is recorded closed.

Wall arithmetic for context (`bench/wall_budget.ps1`): wall 79.3s = A+B 34 + C 20 + Done 23.7, with
**68% of wall single-threaded**, so wall<20 needs a ~3.9x single-thread improvement. No lever
measured this session exceeds ~6%.

---

## 2026-07-29 — the measurement phase is CLOSED, and it closed in this plan's favour

Everything above was written when the profile still had unexplored regions. It no longer does. The
serial window is now decomposed end to end, by three INDEPENDENT instruments, and every region came
back flat:

| category | share of the serial window | how it was closed |
|---|---|---|
| JIT_EXEC | ~60% | per-method entry table; top row `initCache` ~23% of JIT_EXEC, then a flat tail |
| NATIVE_BRIDGES | ~21% | NEW sampled bridge table: **two thirds is the main thread PARKED in `CompletableFuture#allOf`**, the rest flat, largest real bridge `Map#get` ~2.2% of wall |
| INTERP_EXEC | ~13% | — |
| VM_DISPATCH | ~3.4% | — |
| CLASS_LOADING | ~0.6% | — |

And the individually-sized levers, corrected for the JIT_EXEC denominator:

| lever | ceiling | verdict |
|---|---|---|
| Cranelift compile offload | ~4.5s (realistic 2-3s) | now type-UNBLOCKED (`assert_send::<InlineCallee>()` compiles) |
| Mojang codec decode | ~3.0s | mechanism named; the lambda-PIC lever is a re-run of a measured null |
| the three per-state loops | ~11s | two hard blockers |
| NATIVE_BRIDGES | ~0 | closed by measurement — mostly waiting |

Sum of every ceiling ≈ 19s against a 66.5s wall. **Perfect execution of every remaining local lever
lands at ~48s, against a target of 20s.**

### What the flat tail means — and what it does NOT mean

A flat tail under three instruments is not "we have not looked hard enough". It is the SIGNATURE of
a uniform per-operation cost rather than a hot spot: a hot spot would surface in at least one lens,
a multiplier surfaces in none of them because it is in all of them equally.

⛔ **It is NOT evidence for finishing this arc, and an earlier version of this section claimed it
was. That was wrong, and it contradicted a measurement fifty lines above it in this same file.**
The reasoning went "uniform multiplier ⇒ the value representation ⇒ Phase 2", which is exactly the
inference `The flip, measured` already refuted with numbers: on `bench/BareRef.java` — every object
arena-born 0b00, the most favourable workload the flip can possibly have, both arms bit-exact — the
flip removes **5.6% of emitted operations**. Removing a tag TEST deletes two compares and leaves
the DIAMOND standing: merge, phi web, register pressure. And the measured source of the blocks is
not tagging at all but DISPATCH: `emit_dispatch_pic` emits **12.0 blocks per call**, which accounts
for ~804 of the hot method's 852 blocks.

So the correct synthesis of this cycle is:

* every LOCAL lever is shipped, refuted, or sized below the target (ceilings sum to ~19s of 66.5s),
* the tail is flat under three independent instruments, and
* the measured systemic cost is the **per-call dispatch diamond**, not the value tag.

⇒ The next real work is the dispatch-architecture item already named in `What this means for the
plan above` as option (c): move the polymorphism out of the caller entirely — one unconditional
indirect call to a per-site stub the runtime patches, so the caller has no arms and no merge at all.
It is the only remaining candidate sized like the goal, and unlike this arc it has not been priced
and refuted; it has simply never been built.

★ The lesson is the same one this cycle keeps paying for in a new costume: **check the measurement
before writing the synthesis.** A tidy causal story ("uniform cost ⇒ representation") is not
evidence, and this file already contained the number that falsified it.

### Carry these forward — they are cheap and they were paid for

* **Read what a profiler row is a share OF.** Method-table rows are shares of JIT_EXEC (43.5% of the
  run, ~60% of the serial window), not of wall. Converting one straight to seconds inflates it ~2.3x
  and produced a false "the sampler is 2x wrong" claim this cycle.
* **"Self-time" in the method table is ENTRY self-time.** It includes everything reached through the
  PIC. `MapDecoder.lambda$compressedDecode$1` carries 16.7% of a block and its whole body is
  `aload_0; aload_1; aload_2; invokeinterface; areturn` — five bytecodes. Reading it as own-work
  suggested a forwarder-inlining lever that would have removed a frame and none of the time.
* **A structural claim about a type is worth only its compiler check.** Three rounds of "the last
  `!Sync` blocker is X" all missed `ClassFile::method_lookup`, in another crate, because they read
  `LoadedClass`'s OWN fields. `assert_send::<InlineCallee>()` found it in one build.
* **The read is rarely the cost.** A dense lock-free `OnceLock` table beat a hashed `RefCell` on
  every read and lost on the boot: it traded a hash for an allocation spanning every class's whole
  constant pool. Same shape as every refuted "cache in front of a short path", wearing a lock-free
  hat.

## Option (c) is not sized like the goal either — and that was checkable without building it

The section above ends by naming option (c) — per-site patched stub, no arms and no merge in the
caller — as "the only remaining candidate sized like the goal". That is an ARGUMENT, and the tree
already contains a measurement that bears on it.

`bench/dispatch_arm_cost.ps1` exists to price the arms, and its own header records the natural
experiment: `RAVA_NO_JIT_HEAPPIC` takes the emitter from **12.0 blocks per call to 6.0** — it
removes HALF the dispatch diamonds — and the recorded boot effect is **−2.6%** (with FILL +12.7%).

Halving the blocks per call bought 2.6%. Option (c) removes the rest. Nothing here suggests the
~3.9x single-thread improvement `wall < 20s` requires.

⚠ Read the caveat honestly: that toggle does not ONLY delete blocks, it also degrades dispatch
(more host falls), so −2.6% is a NET figure and the pure block-removal term is larger than 2.6%.
It is a prior, not a verdict on (c). But it is the only measurement pointing at the quantity (c)
would change, and it points small.

### Where that leaves the arithmetic, stated once, with sources

| candidate | measured size | source |
|---|---|---|
| value-model flip (this arc's end state) | 5.6% of emitted ops, best possible case | `bench/bareref_mix.ps1`, `BareRef` bit-exact both arms |
| halving dispatch blocks per call | boot −2.6% | `RAVA_NO_JIT_HEAPPIC`, recorded in `dispatch_arm_cost.ps1` |
| Cranelift compile offload | ~4.5s ceiling (~2-3s real) of 66.5s | `RAVA_COMPILE_TIME`, agrees with the sampler |
| Mojang codec decode | ~3.0s | sampled method table, JIT_EXEC denominator applied |
| NATIVE_BRIDGES | ~0 | sampled bridge table: two-thirds is `allOf` parking |
| the three per-state loops | ~11s ceiling | two hard blockers, never built |
| C2-style inlining sweep | REGRESSES 86.9 → 93.0s | prior sweep |
| direct-mapped dispatch table | 0 (Done +1.7%, overlapping) | built and measured |

**No identified lever, and no combination of them, is sized like `wall < 20s`.** The largest
single measured item on the board is worth ~4.5s against a 66.5s wall, and the two candidates that
were argued to be structural have each now been priced by proxy at single-digit percentages.

This is not "we have not found it yet" — it is eight independent sizings, six of them from built
code. Anything that reaches the target has to be something none of the current architecture's
measurements point at, and identifying it is a research question, not a backlog item.
