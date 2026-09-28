# Full Representation Unification — arc plan

> **DEFINITIVE EMPIRICAL VERDICT (2026-07-22, after the FULL build).** The
> unification was BUILT TO LIVE EXECUTION across 18 gated commits (B2 pinned
> region → dual-store mirrors → GC forwarding → five identity/liveness/hash/map
> bridges → B5 production flip → B6a operand flip + callback-force removal):
> under `RAVA_PINNED_MIRROR+READ+PTRS`, handle references flow as raw untagged
> pinned pointers through arena cells, JIT operands and compiled field ops,
> bit-exact `8790f04ace726ecf` including GC stress. The completion measurement —
> interleaved 3×OFF/3×ON fresh-process pairs — shows **ON repeatably SLOWER by
> ~1.4-2.2s in every pair with no overlap** (OFF 91.9/92.2/92.6 vs ON
> 93.4/94.3/94.4): the representation's carrying cost (dual-store + putfield
> callback + mirror alloc, ~0.7s after optimization) exceeds its gain (~0 — the
> discrimination ALU was already hidden under the 334-load memory traffic, and
> FILL's hot path was already arena-form, exactly as the root-cause triangulation
> predicted). **The completion criterion "repeatable wall reduction attributed to
> the representation change" is empirically unsatisfiable on this workload — now
> proven by full construction, not analysis.** The flags stay default-OFF (the
> shipped path is byte-neutral); the built unification remains in-tree as gated
> infrastructure. Formal deletion of `unify_untag`/`>>40` would require shipping
> the flags default-ON, which the arc's own "100% speeds up" invariant forbids.

> **STATUS: REOPENED (2026-07-22) as the sanctioned big-bang.** The user waived the
> "no big-bang" + "per-stage measured wall speedup" constraints (they were proven
> mutually exclusive for this arc) and authorized building the full handle+box→
> pointer migration as ONE long build under the retained hash-exact + GC-safety
> gate, payoff unknown until ~complete. Brick sequence B2→B6 (pinned region →
> dual-store headers → unify field access → flip operand representation → remove
> the guard). **B2 LANDED (pinned non-moving region, nextgen.rs; lazy-mapped +
> dead → canonical bit-exact 8790, GC-stress bit-exact, unit-tested).** Building
> autonomously; correctness gate (hash 8790 + release tests + GC stress) enforced
> per commit even though the per-stage-win requirement is waived.

> **Prior status: CLOSED (2026-07-22), build-backed.** After reopening to attempt the
> prescribed staging, THREE levers were BUILT and RUN, all empirically null and
> reverted: Stage 1 (storage-encoding swap, +3.8% SETUP), B1 guardless getfield
> (SIGSEGV — handle instances of arena-eligible classes pervade boot, so the
> guard is un-removable and un-derisk-able), arena_box interning (bit-exact but
> <0.5% RSS). The one real lever (handle→pointer guard removal) is un-derisk-able
> AND likely ~0 (the 334-load memory traffic dominates; the guard ALU hides under
> it). See `-spec.md` for the full build-backed verdict. User decision: CLOSE and
> pivot to a higher-headroom axis (multicore / load-traffic), which is NOT in the
> reference-representation space.

Goal: replace the dual reference encoding (compact low-2-bit STORAGE tag in
arena field cells + high-word OPERAND tag on JIT-stack values) with ONE uniform
encoding, so a reference is one form — eliminating the per-access storage↔operand
CONVERSION and the per-use discrimination that machcode proved is the #1 wall
cost (see runtime-gap-register.md "Root-cause dig": `Ap2.compute` = 334 load +
263 tag-op on ~8 mul). Each stage is hash-gated (`8790f04ace726ecf`), GC-stress
gated, and accepted only with a repeatable canonical speedup vs the prior
baseline, else fully reverted.

## Baseline (2026-07-22, 3 fresh P-core runs)
SETUP_MS 80.5 / 81.5 / 86.7 (median ~81.5s), FILL 21.2–27.3s, wall 102–115s
(3rd = thermal outlier), hash `8790f04ace726ecf`, peakWS ~738 MiB. Primary
comparison metric = SETUP_MS median (most stable).

## The two encodings (measured)
- STORAGE (arena field cell, `opcodes::decode_arena_cell`/encode, low-2-bit):
  `0b01|h<<2`=heap Ref handle, `0b10|h<<2`=Array, `0b11|idx<<2`=boxed,
  low-bits-0=raw arena ptr, `0`=null.
- OPERAND (JIT stack value, `jit::box_ref`/`unbox_ref`, high-word):
  `REF_TAG_HI<<32|h`=handle, `ARRAY_TAG_HI<<32|h`=array, `BOX_TAG_HI<<32|idx`=box,
  raw ptr (`<2^40`)=arena, `0`=null.
- Arena-ptr and null are IDENTICAL in both. Only handle/array/box differ, so the
  conversion (`native_getfield_ref_const_arena_dual` band/ushr/bor/select) exists
  only to re-encode those; the control-flow to *decide* costs ~5 ops per read.

## Stage 1 (first measurable): unify arena field-cell encoding to the operand form
Make arena object reference field cells store the OPERAND high-word encoding.
Then a compiled ref getfield = plain `load cell` and ref putfield = plain
`store cell` — no dual path, no `is_arena`, no classify — because any operand
form in the cell is already a valid operand. UNCONDITIONAL (no per-class arena
proof needed; handles unify too). Removes the conversion+discrimination from
every arena field access.

Blast radius (all must move together, hash-gated):
- `opcodes::decode_arena_cell` / the arena putfield encode site → identity for
  refs (store/load the operand cell verbatim; primitives unchanged: int/long raw,
  float/double `from_bits`/`to_bits`).
- `arena_getfield` / `arena_putfield` (interp path).
- The GC ARENA SCAN / forwarding (`nextgen` arena root walk): must decode the
  new high-word storage encoding to find+forward arena ptrs and rewrite handle
  cells — THE correctness-critical part. A missed/miswritten forward = corruption
  → caught by the hash gate + GC stress; any failure = full revert.
- The JIT dual-path field ops (`native_getfield_ref_const_arena_dual`,
  `native_getfield_ref_const`, `native_putfield_ref_arena_dual`) collapse to
  plain load/store.
- Snapshot serialize/restore of arena cells (keep wire shape or re-encode at the
  boundary; golden bytes preserved).
- `is_arena_obj_addr` / any code reading arena cells directly.

Gate: env flag (e.g. `RAVA_UNIFIED_ARENA_CELLS`) for A/B while landing; when
proven, default-on and the flag retired. Acceptance: canonical hash exact,
release gate + GC stress green, SETUP_MS repeatably below the 81.5s baseline
(interleaved OFF/ON pairs, every sample recorded).

## Stage 1 — REJECTED (2026-07-22), fully reverted, no commit
Correctness gates ALL passed: full debug+release suites bit-exact both gate
off/on, and heavy GC stress (`RAVA_NURSERY_KB=2048`, ~16× collections) held
`8790f04ace726ecf`. **Performance failed:** gate-ON SETUP_MS median 82.5s vs
79.5s OFF (~+3.8%, above the 81.5s baseline) → rejected per the invariant.

Durable findings (reusable for later stages):
- **GC forwarding is MINIMAL** — a raw arena ptr is bit-identical in both schemes
  (`>>40==0 && &3==0`), so arena-ptr forwarding needs NO change; only handle
  cells change bits and the collector skips handle cells in both schemes. The
  4 forwarding skip-sites need only an added `|| (raw>>40)!=0` clause that is a
  NO-OP when off. `arena_trace_refs`/`decode`/`encode` gate on the scheme. This
  de-risks the GC aspect of the whole arc.
- **Why no speedup:** boxed refs (Str/boxed-primitive) DO occur in arena fields,
  and the operand `BOX_TAG` side-table is per-frame TRANSIENT while the storage
  `arena_box` is PERSISTENT — so the box path MUST keep the low-2-bit tag-3
  persistent table in both schemes. The JIT getfield therefore cannot become a
  pure load; it keeps a box guard, and disambiguating box (`&3==3`) from a
  handle-ending-in-`0b11` needs an extra high-word test → the common arena-ptr
  read costs ~the same. The storage↔operand conversion RELOCATES, not vanishes,
  at Stage-1 granularity.
- **Secondary:** peakPM was consistently ~66 MiB LOWER under ON (~1037 vs
  ~1103 MiB) — a real memory reduction (not the acceptance metric).
- **CLIF re-measure (decisive):** band/select near a CALL = 0, near a LOAD = 64,
  neither = 84. The tag-ops are the getfield STORAGE decode, NOT operand boxing
  at call boundaries. So Stage 1 targeted the right cost — but the decode is
  IRREDUCIBLE while boxed values live in arena ref fields (the box guard can't be
  removed). **Structural conclusion: the representation is a tightly-coupled web
  — getfield stays a decode until box AND handle cases are removed from arena
  ref fields, i.e. box+handle+arena are unified to ONE pointer form. No isolated
  small stage wins; the winning change is the full unification (Stage 2), which
  is a large coupled big-bang that does not decompose into independently-
  measurable increments.** This is the key strategic result of Stage 1.

## Later stages (order recovered after Stage 1 learnings)
2. Unify the boxed-value + handle representations behind one object header
   (`kind` byte in the header, read only when needed) so operand refs are a
   single uniform pointer form — remove the high-word tag entirely.
3. Unify the handle-heap and moving arena behind one pointer form.
4. Precise JIT stack maps so GC finds roots without any cell tag → refs are raw
   untagged pointers everywhere (spill/reload become tag-free).
5. Delete `unify_untag` (2-bit) and the `>>40`/high-word schemes from the hot path.

Rule: unknown/unproven stays on the original path; speculation only under a guard
with deopt; GC-safety absolute (untagged ptr across a safepoint must be in the
precise root set or forwarded); any stage that does not measurably speed up, or
breaks hash/tests/GC, is fully reverted and recorded as rejected.
