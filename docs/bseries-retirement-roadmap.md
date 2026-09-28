# B-series Pinned-Mirror Arc — Retirement Roadmap

> **STATUS 2026-07-24 (FINAL): CLOSED as a boot-speed lever — PINNED_PTRS is a full-boot REGRESSION.**
> The arc was un-parked and driven to a real ChunkFill win (FILL −3.02s / wall −3.14s, bit-exact 8790,
> flag-gated MIRROR+READ+PTRS), fixing genuine coexistence bugs along the way (dual-store coherence
> ef3b5c8, getfield coexistence b45ac69). BUT the full MC-boot A/B ladder (bench/boot_pin_ab.ps1, 6
> interleaved boots) is decisive: **OFF median Done 116.6s vs PTRS median 119.1s = +2.3s SLOWER**, every
> pin run slower than every off run. Cause: the real boot loads a CACHED world (worldgen `Time elapsed:
> 11 ms`), so the density/FILL phase PTRS accelerates barely runs; the boot is SETUP-dominated (class-load
> + registry/datapack) where the mirror dual-store is pure overhead. ChunkFill's 21s of synthetic worldgen
> was a BROKEN PROXY — it over-weighted the one phase PTRS helps. **Lesson: the pinned-stack gate is the
> full boot to Done, never ChunkFill FILL; FILL-phase wins do not transfer to the cached-world boot.** The
> flag stays default-OFF (a specialized worldgen optimization, a cached-boot regression) — NOT a shippable
> boot win. Correctness fixes kept; the dual-store-coexistence approach cannot win the setup-dominated boot
> (only a zero-setup-overhead single-store retirement could). Everything below is the (now historical) plan.
>
> **STATUS 2026-07-24 (earlier): PARKED** (user decision, after measuring the prize). The retirement
> mechanism (bare-load = drop tag discrimination on 0b00 fields) is proven to WORK, but the
> prize FILL is unmeasurable without near-total mirror coverage. Density-scoped bare-load
> crashes at a **26.87% 0b00 hit rate**; the 17-field / 8-class density leak list is all 0b01
> handles in three buckets, and bucket 3 (fastutil `FloatList`/`DoubleList`, `java/util/List`,
> enum `Type` singletons) does NOT fit the layout-known mirror model — reaching ~100% 0b00 is
> the same multi-week generic-coverage problem, not a bounded fix. Payoff (~10%) stays
> unvalidated and in the historically-marginal repr-lever class. **Kept:** increment #1
> (lock-free forward table, b8c86c7) + this map. **Pivoted to:** the startup axis (RAVA's real
> unwalled advantage). Re-enter here only with a mandate for multi-week generic mirror coverage
> AND a plan for the collection/enum/singleton tail.


**Goal:** retire the handle/box value representation so compiled field reads become
direct pinned-pointer loads with NO 2-bit tag discrimination — the only lever on the
73% JIT_EXEC MC-boot wall (`docs/runtime-gap-register.md` GAP root-cause; the
tag-discrimination-per-deref value model is the measured cause of the 6.8× density gap).

**User decision (2026-07-24):** complete this arc (over startup-pivot / accept-ceiling),
accepting multi-week effort and negative interim wall measurements, with bit-exact
`8790f04ace726ecf` mandatory at every step.

## Current state (commits f7ff777..b0c6db3, all flag-gated default-OFF, all bit-exact)

| Flag | Brick | Effect |
|------|-------|--------|
| `RAVA_PINNED_MIRROR` | B3 | dual-store handle-object layout fields into a pinned (non-moving) arena mirror |
| `RAVA_PINNED_READ` | B4 | serve field reads from the mirror (cold Rust funnel + native bridges only) |
| `RAVA_PINNED_VERIFY` | B4-val | execution-verify mirror == FieldBlob on every read |
| `RAVA_PINNED_PTRS` | B5/B6 | a mirrored handle stores/enters compiled code as a raw pinned address (0b00), with 5 coexistence bridges making Obj(mirror) ≡ Ref(h) |

**Measured full stack (MIRROR+READ+PTRS):** hash 8790 bit-exact ✓ but **+2.0s wall**
(SETUP +1.5, FILL +0.4, peakWS +37MB) = the **coexistence-tax valley**: maintains BOTH
the handle FieldBlob AND the mirror, retires neither.

## Audit findings (2026-07-24)

**Why the valley:** the arc's plumbing is a global lock on the hottest path.
`pinned_mirror_get` (nextgen.rs:1064) locks a global `Mutex<Vec<u64>>` on EVERY field
read (heap.rs:3875) and write (heap.rs:3751), even for un-mirrored objects → the +1.5s SETUP.

**Read coverage (who still reads FieldBlob, not the mirror):**
- (a) **Compiled JIT getfield** (`native_getfield*`, jit.rs:13424/15544) reads FieldBlob
  inline for tagged-handle receivers — **NOT flipped by B4**. This is ~75-79% of boot.
  Only the `arena_blk` arm (PINNED_PTRS pointer receiver) consults the mirror.
- (b) **Native `$`-fields** (`$map`,`$elems`,`$port`,`$str`,…) have no layout slot →
  `pinned_read_field` returns None (heap.rs:3880) → always FieldBlob. Structural blocker.
- (c) VM-internal (toString/equals/identity via dispatched guest methods) re-enter (a).
- (d) GC/heap machinery reads FieldBlob intentionally (authoritative store).

**Write coverage:** dual-store is comprehensive on all default-emitted write paths under B4
(put_field / put_field_hint / sync_obj_desc / compiled putfield force the callback). The
`arena_blk` PINNED_PTRS arm intentionally leaves FieldBlob stale (the divergence VERIFY hunts).

**Eligibility:** ALL layout-known handle classes are mirrored (no density subset gate,
heap.rs:2169-2188). The worldgen density graph IS mirrored. Only layout-unknown objects
(`$Lambda`, uncomputed layouts, synthetic-only holders) get no mirror.

## Structural property (critical)

Tag-op elimination is **all-or-nothing**: a ref-field cell is tag-discriminated on deref
because it *could* hold a handle (0b01) / array (0b10) / box (0b11). A pinned pointer (0b00)
is still tag-checked at compile time because the compiler cannot prove the field never holds
a handle/box. The check dies only when **every** handle AND box representation is globally
retired. => intermediate wall stays negative until the final flip; the arc's progress metric
is STRUCTURAL COMPLETENESS (read-paths flipped, classes mirrored incl `$`-fields, box arm
retired), gated on bit-exactness — not incremental wall.

## Increment sequence

1. **[IN PROGRESS] Lock-free forward mirror table.** Replace `Mutex<Vec<u64>>` with a
   grow-only chunked `AtomicU64` array (readers Relaxed-load, writer under GIL publishes
   Release). Removes the #1 tax (+1.5s SETUP). Bit-exact (only the concurrency primitive
   changes). PREREQUISITE: converts "measurably slower, can't tell if flips help" into
   "near-neutral plumbing" so subsequent flips can be honestly measured.
2. **Synthetic `$`-field mirror slots.** Give `$`-fields layout slots (or a parallel mirror
   region) so category (b) reads can flip and FieldBlob for them becomes droppable.
3. **Flip compiled tagged-handle getfield reads** (category a) — or fully commit hot
   receivers to PINNED_PTRS pointers so the arena arm serves them. The 75-79% path. Hash-risky.
4. **Retire dual-store:** once no read path reads FieldBlob, make the mirror the sole store;
   drop the FieldBlob write on mirrored classes.
5. **Retire the box arm (0b11):** give boxed primitives / Str in ref-typed fields a
   non-tagged representation so ref cells are provably 0b00-or-0b01-only, then 0b00-only.
6. **Drop tag discrimination in compiled getfield** — the payoff (~10% wall). Only reachable
   after 1-5 make all reachable refs provably pinned pointers.

**Rule:** never big-bang blind. The 2026-07-22 ceiling pilot crashed at 1.3s — handle/box
refs in arena fields are load-bearing from boot start. Each increment: bit-exact 8790 gate +
GC-stress, built and measured before the next. NEVER stage `.continuum/*` or `daemon.lock`.
