# Representation Unification — design spec + ceiling pilot

Companion to `representation-unification-plan.md`. Stage 1 (storage-encoding
swap) was sound but did not speed up; the CLIF re-measure proved the tag cost is
the getfield STORAGE decode (`band 0b11`/`icmp is_raw`/classify), irreducible
while boxed/handle values live in arena ref fields. The winning change is the
full box+handle+arena unification (one pointer form + `kind` in the header).
Before committing weeks to that big-bang, this spec (a) fixes the target design
and (b) defines a bounded, reversible PILOT that MEASURES the arc's ceiling.

## Current ref-getfield shape (measured, `native_getfield_ref_const_arena_dual`)
```
is_arena = (obj != 0) && (obj>>40 == 0)      // emit_is_arena: routes receiver
  arena arm:
    cell = load [obj + HDR + slot*8]
    t    = cell & 0b11
    is_raw = (t == 0)                          // <-- the 64 near-LOAD band/select
      raw:      return cell                     // common object-graph edge
      classify: is_box=(t==3)? cb : retag       // cold: handle/array/box refs
  handle arm: native_getfield_ref_const(...)   // high-word heap path
```
The raw-arena fast path already returns `cell` verbatim — but only AFTER the
`band 0b11` + `icmp` guard and the classify block exist in the CFG. The full
unification removes that guard entirely: if every ref cell is a raw pointer
(arena or handle-as-pointer), the arena arm is a pure `load` with no decode.

## Target design (the big-bang, for when the pilot justifies it)
- **One pointer form.** A reference operand and a stored ref cell are BOTH a raw
  machine pointer to an object with a single header. No low-2-bit tag, no
  high-word tag. `null` = 0 (unchanged).
- **Kind in the header.** The object header carries a `kind` discriminator
  (arena-moving vs handle-stable-pinned vs boxed-value-carrier). It is read ONLY
  by code that must branch on it (GC forwarding, identity, box unwrap) — NEVER on
  an ordinary dereference or field access.
- **Handle heap becomes pinned pointers.** Handle-heap objects keep stable
  addresses (they already don't move); a handle ref becomes the object's raw
  address with `kind=handle` in its header, so it is dereferenced like any
  pointer. The `Vec<Object>` handle table stays as the owner/allocator but is no
  longer indexed on the hot path.
- **Boxed values become real objects.** A boxed Str/primitive in an arena field
  becomes a real (arena or handle) object carrying the value + `kind=boxed`,
  reachable by a raw pointer — removing the `arena_box` persistent side-table
  from the field-read path. This is the piece that removes the box guard that
  defeated Stage 1.
- **GC roots by kind, not by cell tag.** The collector reads each object's
  header `kind` to decide forward/skip; precise JIT stack maps (later stage)
  supply live pointer roots so no operand tag is needed to find refs.

## Blast radius (unchanged from plan; recorded for the big-bang)
`opcodes::decode_arena_cell`/encode, `arena_getfield`/`arena_putfield`,
`nextgen` arena root walk + forwarding (correctness-critical), the JIT dual-path
field ops, snapshot serialize/restore, `is_arena_obj_addr`, all direct arena-cell
readers, `unify_untag` (jit.rs:108) + the `>>40` scheme.

## CEILING PILOT (bounded, reversible, MEASUREMENT-ONLY)
Purpose: quantify the maximum wall win available from removing the storage decode
BEFORE writing the big-bang. Not shippable; a measurement instrument.

- **Flag** `RAVA_ARENA_REF_PURELOAD` (default OFF). When set, the arena arm of
  `native_getfield_ref_const_arena_dual` emits ONLY `cell = load [...]; return
  cell` — skipping `band 0b11`, `is_raw` brif, classify/retag/cb. The
  `emit_is_arena` receiver routing is KEPT (handle receivers still take the
  high-word path, so no wild deref / crash).
- **Soundness as a measurement.** On the raw-arena edge, `cell` is already a
  valid operand (bits identical in storage & operand). If the canonical run's
  arena ref fields are 100% raw-arena in this workload, the pure load produces
  the SAME values → hash stays `8790f04ace726ecf`, proving the skipped ops were
  redundant FOR THIS WORKLOAD, which is exactly the arc's ceiling. If the hash
  DIVERGES, the workload stores tagged (handle/array/box) refs in arena fields →
  the decode is essential → the arc's ceiling is near zero and the big-bang is
  not worth the risk.
- **Acceptance of the SIGNAL (not of a stage):**
  - hash `8790` + measurable SETUP_MS drop ⇒ the arc has a real ceiling worth the
    big-bang; proceed to design detail + staged implementation.
  - hash `8790` + NO drop ⇒ the storage decode is already free (branch-predicted,
    off critical path) ⇒ the arc CANNOT win from decode removal ⇒ close/redirect.
  - hash diverges ⇒ tagged refs live in arena fields ⇒ decode essential ⇒ the
    box/handle migration is mandatory before any win ⇒ record and reassess scope.
- Pilot is reverted after measurement regardless of outcome (measurement
  instrument, not a shipped change); the NUMBER is the deliverable.

## PILOT RESULT (2026-07-22) — decode is LOAD-BEARING from boot start
Ran interleaved OFF/ON canonical. OFF: SETUP_MS 81380 / 80872, hash
`8790f04ace726ecf` (baseline intact). ON (`RAVA_ARENA_REF_PURELOAD`): **instant
crash at ~1.3s**, before worldgen — the log dies at provider resolution with
`Provider 'java.lang.Object'` (a ref field read the raw tagged cell and resolved
to the wrong object). This is the "hash diverges" branch, and sharper than
expected: the process corrupts within 1.3s, at the FIRST class-provider lookup —
so handle/box refs live in arena fields and are read **frequently and load-
bearing from the very start of boot**, not a rare tail.

**Verdict — the arc admits NO cheap ceiling measurement.** Because the classify
(handle/box) refs are load-bearing from boot, they cannot be stubbed to isolate
the decode's cost; any attempt corrupts immediately. The only way to measure the
win is to BUILD the full box+handle→pointer migration — there is no bounded probe
that yields the number first. Combined with:
- Stage 1 (encoding swap): sound, **no wall win** — the decode relocated, did not
  vanish.
- Machcode static share: tag-ops are ~26% of Ap2.compute CLIF ops, but they are
  ALU ops (`band`/`ushr`/`select`) + predicted branches that largely hide under
  the 334-load memory latency and 69-call overhead on an out-of-order core.
- Prior [[rava-ceiling-map-2026-07-19]]: Rava already at ceiling; the only open
  lever was a ~5-10% risky main-thread λ-PIC.

…the realistic ceiling of the full unification is **single-digit-to-~10% wall at
extreme risk** (weeks of work, heap-corruption surface, gated only by the hash +
GC stress, with NO way to pre-validate the payoff). This is a poor risk/reward
and is consistent with every prior measurement. **Recommendation: do NOT commit
the big-bang blindly.** If pursued, it must be a full build accepting the payoff
is unknown until done. The decision returns to the user.

## Sub-stage viability — MECHANISM-PROVEN (2026-07-22, two deep source maps)

After the pilot, two exhaustive read-only source maps (JIT↔GC root cooperation;
the full box taxonomy) pinned down exactly where the reference tag is read and
what each candidate sub-stage would and would not remove. Result: every stage
short of the full handle→pointer migration is a COLD-ARM or ORTHOGONAL change
that cannot pass the "each stage measurably speeds up" invariant.

### The hot cost, located exactly
The compiled ref-getfield hot path is `native_getfield_ref_const_arena_dual`
(jit.rs:14234). For an ARENA receiver it does: `load cell` → `band 0b11` →
`icmp is_raw` → (raw `0b00`: return cell) / (cold: reclassify handle/array/box).
The `band 0b11 + icmp` guard exists because an arena object's reference FIELD can
hold either a raw arena child (`0b00`) OR a heap handle / array / box
(`0b01/0b10/0b11`). That guard is the per-read tag cost. It is removed ONLY when
a reference field can hold nothing but a uniform raw pointer — i.e. non-arena
(handle) objects and boxes ALSO become raw headered pointers.

### Stage "precise JIT stack maps" (goal's recommended #1) — REJECTED, orthogonal
- JIT stack-root discovery is ALREADY tag-independent: a CONSERVATIVE 8-aligned +
  `arena_contains` address scan of the spill buffer (lib.rs:6835-6848), explicitly
  self-documented as "harmless false roots… never corrupt" (lib.rs:6724). The
  reference tag is NOT read for stack roots. So precise stack maps do not unblock
  untagging (the stack side needs no tag today).
- Perf-negligible: the conservative all-locals spill is N stores per safepoint;
  even tens of millions of stores ≈ tens of ms out of an 80 s boot (<0.1%). No
  measurable speedup is possible → the invariant auto-rejects it.

### Stage "box → headered object" — REJECTED, cold-arm + soundness blockers
- The `0b11` box arm is the ONLY getfield arm that leaves compiled code and is
  COLD BY DESIGN; the raw-arena and handle/array arms re-tag inline. Removing the
  box case does NOT touch the hot `band 0b11 + icmp` guard (arena-vs-handle stays
  ambiguous) → no hot-path win.
- Boxes hold `Str(Arc<str>)`, `StdOut`/`StdErr` sentinels, and IDENTITY-boxed
  primitives (`Value::Int/Long/Double` with no wrapper class, no header, no
  address). Promoting them to real objects requires synthesizing autobox +
  `Integer.valueOf` −128..127 cache identity (would change `==`), materializing
  PrintStream sentinels, and allocating a String object per string — the box
  side-table exists PRECISELY to avoid that allocation, so promotion is a likely
  perf REGRESSION plus a semantic hazard. Blast radius: 4 encode + ~30 decode
  sites + GC invariant changes (`raw&3!=0 ⇒ skip`).

### The single real lever, and why it cannot be staged with per-stage wins
The hot guard drops only when arena + handle + box references are ONE uniform raw
headered-pointer form (the scaffolded `Value::Obj` replacing `Value::Ref`,
value.rs:36-41, the author's stated "rava-endgame"). But a PARTIAL migration
leaves both forms coexisting, so the guard must stay to tell them apart → the
hot-path win appears only at ~100% migration. Therefore this lever CANNOT be
delivered as independently-measurable increments: **the arc's two constraints —
"stage with a per-stage measured speedup" AND "no big-bang" — are mutually
exclusive for this arc.** Proven, not preferred. The only paths that remain are:
(a) relax the no-big-bang / per-stage-win constraint and build the handle→pointer
endgame as one long guarded big-bang (hash + GC-stress gated, payoff unknown
until ~complete), or (b) close the arc as not incrementally achievable.

## B1 checkpoint BUILT and RUN — the derisking checkpoint is itself unsound (2026-07-22)

The user relaxed the staging constraint to build the endgame, and the architect
plan's brick B1 (an early empirical checkpoint) was implemented to measure the
guard's value BEFORE the multi-week migration. B1: a `RAVA_ARENA_GUARDLESS` flag
that, for a getfield whose owner is the FINAL, arena-REGISTERED class being
compiled AND whose receiver is `this` (aload_0), drops `emit_is_arena` to a bare
null check (arena arm only). Predicate + emit built across nextgen.rs
(`arena_registered_id` non-forcing query), jit.rs (flag/TLS site set + guardless
param on both dual getfields), lib.rs (per-method `this.field` site scan).

**Result: the flag-ON build CRASHES at ~47-49 s (mid-SETUP, before FILL) with a
silent SIGSEGV** — a guardless arena arm dereferenced a HANDLE (high-address)
receiver. The `[GUARDLESS]` diagnostic confirmed the predicate fires on real
final+arena-eligible classes (`Identifier`, `SubShape`, `IntegerProperty`,
datafixers `Type` nodes, fastutil AVL entries). Flag-OFF stays exact
(`8790f04ace726ecf`, SETUP 83.8 s), so the crash is definitively the guardless
emit, not a shared-code regression. Even restricting to `this`-rooted reads did
NOT fix it.

**Decisive finding — the plan's B1 soundness argument is EMPIRICALLY FALSE.** The
argument was "a final, arena-eligible class has no subclass, so its instances are
always arena." But arena-eligibility is a property of the bytecode `new` SITE, NOT
a guarantee that every instance is arena: **native/reflective/interning code
allocates HANDLE instances of the very same final, arena-eligible classes**
(e.g. an interned `Identifier`, a datafixers `Type` built by native DFU), and
their methods run during boot with `this` = a handle. So the arena-vs-handle
guard is genuinely load-bearing even for `this.field` on a final class.

**Consequences for the endgame:**
- There is NO sound early checkpoint. The guard cannot be removed at ANY site
  until EVERY allocation path (bytecode `new`, native `Heap::alloc`, reflection,
  interning) of that class produces the unified pointer form — i.e. not until
  bricks B3–B5 are complete for all such classes. The payoff cannot be measured
  before doing the multi-week migration; the derisking brick the plan relied on
  does not exist.
- This tightens the earlier conclusion: the endgame is all-or-nothing, multi-week,
  extreme-risk, AND un-pre-validatable, on top of the standing hypothesis that
  the guard's ALU cost is hidden under the 334-load latency (so the payoff may be
  ~0). B1 was fully reverted (git checkout; measurement-only + unsound).

## arena_box interning (a standalone B3–B5 lever) BUILT — negligible (2026-07-22)

With guard-removal un-derisk-able, the next attempt was a DIFFERENT migration
lever with standalone value: the `arena_box` side table (form D, nextgen.rs:855)
is append-only and "never compacted (acceptable boot-scale leak)". A probe showed
it grows to **200 000+ entries (~4.7 MiB of `Value` slots and still climbing)** on
the canonical run — dominated by duplicate `Value::Str` (a resource-location
namespace like "minecraft" is boxed once per `Identifier`, thousands of dup
entries).

Built a SOUND fix: intern `arena_box_push` by `Value::Str` content + the
`StdOut`/`StdErr` sentinels, bounding the table to the UNIQUE value count.
Soundness is exact: RAVA string `==` is already content-based (`ref_equal`:
`Arc::ptr_eq(x,y) || x==y`), so two equal-content Strs are behaviourally
indistinguishable and can share one box index — hash stayed **8790f04ace726ecf**
across 2 runs, confirming bit-exactness.

**But the RSS win is within noise:** peakWS 737.9 / 740.7 MiB vs ~737-740 baseline
(unchanged); peakPM ~1097 vs ~1103 MiB (~6 MiB, ~0.5%). The ~5 MiB of `Value`
slots is negligible against 737 MiB WS, and the `Arc<str>` text was already shared
by string interning, so deduping the slots frees little. SETUP even ticked up
slightly (the per-box hashmap lookup). No measurable win → **reverted** per the
invariant.

## Arc conclusion — every built lever is empirically null
Three levers from this arc were BUILT and RUN, not just analysed: Stage 1
(storage-encoding swap, +3.8% SETUP → reverted), B1 (guardless getfield,
SIGSEGV — unsound → reverted), arena_box interning (bit-exact but <0.5% RSS →
reverted). Together with the mechanism proofs (stack maps orthogonal + negligible;
box→object cold-arm + regression), the representation-unification space contains
**no incrementally-deliverable, measurable win**, and its one real lever
(handle→pointer guard removal) is un-derisk-able and likely ~0 (loads dominate).
Recommendation stands: close the arc; the levers with real headroom are elsewhere
(multicore / load-traffic reduction), not in reference representation.
