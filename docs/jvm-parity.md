# JVM parity — the compute-only engineering plan

Mission (user, 2026-06-12): match the JVM's speed at minimum; ideally deliver
better performance at the same resource consumption. **Snapshots are excluded**
— parity must come from real execution speed. Generic engine only; server.jar is
the benchmark, never the target of adaptation.

## Where the ~600x boot gap actually lives (all measured)

| Component | Measured | Evidence |
|---|---|---|
| Interpreted dispatch/heap ops vs C2 native | 140–400x per op | bench: Dispatch 144x, Collections 324x, Allocs 363x |
| Compute in interp vs C2 | ~150x | ArithLoop 146x |
| Compute JIT'd (Cranelift) vs C2 | ~2–3x | ArithLoop jitted 0.064–0.11s vs java 0.05–0.06s |
| Cores used | 1 vs all | executors run inline; worldgen serial |
| JVM warmup | negligible for them | java reaches Done in 5.6s including all warmup |

Boot ≈ 30B interpreted ops. Phase profile: pre-worldgen ~70% = voxel-shapes
cache build (compute-shaped but call/virtual-heavy); worldgen ~77% of total =
noise math (compute) + chunk assembly (heap).

What we already fixed this session: JIT compile slots now reach hot code
(threshold 512 + arena memory provider + cap 4096, flat RSS, verify-clean), RAM
is JVM-class (~1GB). What measurement says remains: even fully-covered JIT code
is held back by **callbacks** (invokes, getstatic, ref-heavy paths) and the
**object model** (id-scan fields, side tables), and everything runs on one core.

## The parity stack — four levers, in dependency order

### 1. Object model: per-class layout (the enabler)
Today every object stores `field_ids`/`field_cells`/`field_kinds` and native
getfield scans ids. Real JVMs compute layout per CLASS at load: field → fixed
slot. With fixed offsets:
- JIT getfield/putfield = one machine load/store (no scan, no callback).
- Object = header + cells (less RAM, faster alloc, GC-friendlier).
- Synthetic fields ($map/$elems) and reflection go to a rare overflow map.
This unblocks every other lever; without it the JIT's ceiling on heap code is
the callback cost, not codegen quality.

### 2. JIT: kill the remaining callbacks (dispatch above all)
Measured order of cost (memory + this session):
1. **invoke** — ~440ns/call via rava_invoke (synthetic frame + resolve +
   marshal) vs ~5ns inlined in JVM. Levers: JIT-to-JIT direct calls (cache the
   callee's compiled entry at the call site, marshal registers, GC-root the
   frame), receiver-class-guarded inline caches for virtual/interface, then
   inlining of small callees (already proven for static leaves: ~2x JVM).
   STATUS (2026-06-18): direct-call cache made POLYMORPHIC (commit d891c50) —
   `jit_direct_cache` now holds ≤4 receiver-class-guarded entries per site, so a
   polymorphic virtual/interface site stays on the direct path for every class
   instead of guard-missing to the general path on all but one. Dispatch bench
   73x→~10x vs JVM (RAVA 4866ms→~668ms). Inlining of small leaf callees (incl.
   getfield readers) already lands; the residual on field-heavy inlined callees
   (DispatchMono ~23x) is loop-invariant field-load LICM the JVM does and
   Cranelift doesn't — folds into lever #1 (object model) + a future LICM pass.
2. **getstatic/putstatic** — resolve once at compile, read a pinned statics slot.
3. **ref-heavy paths** that still bail (boxed values, Str) — shrink with (1)'s
   string-as-heap-object change.
Exit criterion: a fully-jitted hot method does ZERO VM callbacks on its fast
path. Then Cranelift's ~2x-of-C2 floor is the real per-op gap.

### 3. Multi-core (the multiplier)
MC parallelizes chunk gen across an executor pool; the JVM uses every core. The
engine must run those tasks on real OS threads:
- Phase 1 (done, free): Rc→Arc.
- Phase 2: Send+Sync engine — RefCell→locks on cold paths only (class registry,
  statics map); hot paths must be lock-free reads of frozen data.
- Phase 3: per-thread nurseries + shared old gen (objects publish on promote),
  stop-the-world GC at safepoints (op-counter check already exists).
- Phase 4: executor natives (ExecutorService/ForkJoinPool/Thread) run tasks on
  the worker pool. Worldgen then scales ~×cores.

### 4. Tiering polish (cheap, after 1–3)
- Recompile hottest methods at higher effort; lower threshold once compile is
  cheap; background compile thread (needs 3) so compilation never stalls
  execution (the measured ~15s compile stall in a 240s window disappears).

## Honest arithmetic to parity

Per-op: object model + callback-free JIT ⇒ hot code at ~2x C2. Coverage: with
tiering, >95% of boot ops hot ⇒ effective ~2–3x JVM single-core. Cores: ×8–16
on the parallel 77% ⇒ **wall-clock within ~1–2x of the JVM on boot, at parity or
better on steady-state TPS** (tick loops are smaller, fully-jitted, and our
fixed costs — no classfile verification, no tiered re-profiling — start paying
back). Better-than-JVM at equal consumption is credible on RAM (already there)
and on TPS; on cold boot, parity-ish is the realistic best.

This is a multi-month build. Order of work: 1 → 2 → 3 → 4, validating each step
with the bench harness + verify boots + the full boot A/B.

## Measurement discipline (hard-won this session)
- Interp-op counters are NOT comparable across JIT modes (native work is
  invisible). Use wall-clock to a fixed marker, field_box growth, or GC count.
- The byte meter cannot see JIT mmap; check process RSS for JIT memory.
- One machine measurement at a time; thermal noise is ±2x on absolutes —
  compare ratios from interleaved runs.

## Session findings 2026-06-30 (measured, execution-ready)

**Milestone: JIT=1 now boots to `Done`.** The multi-session boot wall was NOT GC —
it was `UnsatisfiedLinkError PushbackInputStream.unread([BII)V` from
DimensionDataStorage (NbtIo peeks the gzip magic→unread→GZIP). Fixed (natives.rs):
unread([BII)/([B)+close; `stream_state` follows the `$stream` wrapper chain and
advances `$pos` on the buf-owner; Pushback/Filter `<init>` link `$stream`;
`stream_read_all` drains `$pushback` so GZIP sees the unread magic. Fast repro =
PbRead.java (bit-exact saved-data `.dat` decompress vs JVM, seconds not a 7-min boot).
Measured: JVM Done 0.21s (cached flat world) vs rava JIT=1 ~440s ≈ 100x total wall
(≈2100x vs the 0.21s server-init slice). The gap is fundamental per-op cost — verified
NOT log spam (202 guest "No data fixer" lines), NOT GC (4 cycles in 300s), NOT a
pathology. So boot parity needs levers 1+2; only TPS can exceed the JVM.

**The multi-core primitive is BUILT (and the boot benchmark won't use it).** The
GIL-free work-stealing reduction pool (parallel_pool.rs + the data-parallel JIT,
RAVA_JIT_PARALLEL=1) is proven 4.2–4.8x FASTER than C2 on associative folds, incl.
canonical array reductions `sum += a[i]` (bench/ParReduce, bench/ArrReduce, bit-exact).
This is the Phase-3/4 worker substrate, already working. THE BLOCKER for applying it to
MC: workers are alloc-free (pure compute only) — MC's hot work allocates (VoxelShape
merge, chunk assembly). Unlocking multi-core MC = **Phase 3 per-thread nurseries**
(workers bump-allocate in a thread-local arena, publish on promote) — that is the
precise, highest-leverage next increment, and the ONLY path that beats the JVM.
NB: the local benchmark world is `level-type=flat` → no noise worldgen → boot cost is
registry + voxel-shape bootstrap (object-heavy serial), so even noise-parallelism
(alloc-free, per-column independent) wouldn't help THIS boot; it helps real worldgen.

**Fresh per-op scorecard (RAVA_JIT=1, degraded machine — ratios noisy ±2x):**
FieldsRef 9.9x, Collections 7.8x, DispatchMono 7.4x, Fields 6.8x, Allocs 5.8x,
Dispatch 2.8x, ArithLoop 1.8x, StringOps 1.7x, Statics ~0 (folded). Field access
dominates → confirms lever #1 (object model: branchless arena getfield, no is_arena
dual-branch that blocks Cranelift egraph LICM) is the top single-thread win. The
arena getfield's runtime `is_arena` branch is what stops loop-invariant field-load
hoisting; a branchless select-the-address form would unblock it (deferred: needs both
addresses computed fault-free).

**Order to actually beat the JVM on the MC server (unchanged, now de-risked):**
1→ object model (branchless arena getfield) — narrows field/dispatch to ~2x floor.
2→ JIT-to-JIT direct calls everywhere (invoke ~440ns is the #1 boot cost).
3→ **per-thread nurseries** so the existing reduction pool can run allocating MC work →
   the only lever that crosses 1.0x. Target metric = steady-state TPS, not cold boot.

## Bottleneck data 2026-06-30 (RAVA_JIT_STATS, decisive)

Collections bench callback tally: **invoke=1,192,633** (new=14, getfield/getstatic/etc
≈0). The dominant per-op cost is the COUNT of `invoke` dispatch callbacks — every
collection op (HashMap.put/get, ArrayList.add/get, Map.Entry.getValue, Integer.intValue)
is one FFI callback into the runtime. The frame-skipping intrinsics (amap_*, fu_invoke,
the new ArrayList path) make each callback CHEAPER (~18x measured on lists) but each is
still a callback (cb(0)), so the count stays ~1.2M.

⇒ The remaining order-of-magnitude is ONLY recoverable by ELIMINATING callbacks =
IR-inline. The template exists: `emit_amap_int_probe` runs `HashMap.get(int)` /
containsKey as a pure-register open-addressing loop, ZERO callbacks, when the map is an
arena `ArenaMap` and the key is an unboxed int (the Integer.valueOf+get fusion). Extending
zero-callback IR-inline to (a) HashMap.PUT(int-key) [fast path + callback only on resize],
(b) arena-backed ArrayList get/add [needs the list migrated off heap `$elems` to a flat
arena array, like arena-maps did for HashMap], and (c) the entrySet/iterator drain, is the
path that cuts the 1.2M callbacks toward zero. This is the concrete, data-justified
multi-session lever for collection-heavy MC per-op cost. Single-thread STILL can't cross
1.0x (Cranelift floor) — only the parallel pool does that — but it closes most of the gap.

## CORRECTED finding 2026-06-30 (field access is FINE; my earlier note was WRONG)

SELF-CORRECTION of the "Field-LICM finding" below: it was wrong on two counts.
(1) Locals are NOT in memory — `load_local`/`store_local` already use Cranelift `Variable`
(use_var/def_var) → locals are SSA. (2) The branchless getfield gave "zero speedup / 100-170x"
ONLY because the test loop (`s += this.a*this.b + this.c - this.a`) is loop-INVARIANT, so C2
FOLDS it to a closed form (~0) while rava runs the 200M-iter loop — a loop-folding gap, not a
field gap. RE-MEASURED on a NON-foldable field-heavy loop (`s += (this.a*i + this.b)*(i&this.c)
- this.a*(i>>1)`): rava is only **1.6-1.7x slower than JVM** (126ms vs 74ms), bit-exact. ⇒
PER-OP FIELD ACCESS IS NEAR THE CRANELIFT FLOOR ALREADY (~1.6-2x), not a bottleneck. Branchless
getfield solved a non-problem (reverted). The real single-thread gaps are: (a) LOOP-FOLDING
coverage — C2 folds invariant loops to ~0 that rava's build_folded_body misses (microbench-only;
real/MC loops do per-iter work, not foldable), (b) the ~1.6-2x Cranelift floor on real work. Neither
crosses 1.0x. Do NOT chase field-access optimizations — measured fine.

## Field-LICM finding 2026-06-30 (branchless getfield is NOT the blocker)

Implemented + measured a BRANCHLESS `this.field` getfield (compile-time proof: instance
method of a guest arena class ⇒ `this` is non-null arena ⇒ unconditional readonly load,
no is_arena/null branch — the branch was thought to block Cranelift's egraph LICM).
Result: CORRECT (bit-exact, 130+73 green) but ZERO speedup — even on `this.field` read in
a tight loop, ON==OFF, and rava stays ~100-170× JVM (JVM hoists the whole invariant
expression to ~0). REVERTED (dead complexity + codegen risk for no gain).

ROOT CAUSE (the real blocker): `this` and ALL locals live in the locals-MEMORY array
(`locals_ptr`), reloaded via a memory load on every `aload`. Cranelift can't prove that
reload loop-invariant (the array could be written), so the field ADDRESS `this+off` is
never loop-invariant — and a branchless readonly load still isn't hoisted. ⇒ Field/expr
LICM is blocked by the LOCALS-IN-MEMORY model, not by the getfield branch.

LEVER: promote locals to SSA values (Cranelift `Variable`s / block params) so `this` and
loop-invariant locals become registers → THEN field loads off them hoist (and the
branchless getfield finally pays off). This is the object-model/SSA work; the parallel-
reduce body emitter already uses SSA `Variable`s for its scratch locals as a template.
Don't re-attempt branchless getfield before locals-SSA — measured useless without it.

## Session findings 2026-06-30b (dispatch tax + structural-key + shipped wins)

**Boot op-rate is ~1.2M op/s (~800ns/op); rava's OWN interp floor is ~60M op/s (arith)
/ ~13M (dispatch).** So boot is **50x slower per op than rava's own arithmetic interp** —
the gap is NOT the per-bytecode core (healthy) but the **per-INVOKE fixed tax** (boot is
invoke-saturated; CB stats: invoke≈total ops). `opcodes::invokevirtual` ran a ~250-line
special-case gauntlet (gson/slf4j/guava-cache/fastutil/lambda/StdOut) + an `actual.to_string()`
alloc + string-keyed IC probe on EVERY virtual call.

SHIPPED (validated, bit-exact, 130+73 green, default byte-identical):
- **fa8ccbc** class-id fast inline cache: for arena `Value::Obj` receivers every gauntlet
  branch is Ref-gated or fixed per site → cache (site,arena_class_id)→decl, hit skips the
  whole gauntlet + the String alloc. **+24%** interp dispatch (Dispatch JIT=0 12.3→15.3M op/s).
- **19dcd09** direct-drain arena array-map structural keys: block-state combo maps
  (fastutil Reference2ObjectArrayMap, arena Obj) used AS keys hit `structural_key` →
  `value_to_string` → the map's `toString()` bytecode (entrySet iteration = 42% of
  block-bootstrap + Property.toString via guava MoreObjects = 32%). Now drains packed
  `key/value/size` arena fields directly. **3.3x** on bench/MapKeyStress (200k keyed
  lookups, 25026→7580ms). Boot wall-clock unmeasurable on this host (phase noise + 1 core).

**ROOT CAUSE of block-state cost = rava keys maps by a structural STRING (drain+stringify
every key/value per lookup) instead of hashCode/equals.** This is the single biggest
remaining block-bootstrap lever.

CORRECT fix (do NOT use field-walk — proven to corrupt): use the object's REAL
`hashCode()`/`equals()` bytecode. The 32% is NOT inherent to hashCode/equals — it's that
rava uses `toString()` as the hash/eq proxy (Property.toString calls guava; Property.hashCode
is cheap). Plan: bucket by `key.hashCode()` (cache the i32 in a header slot like identity-hash),
verify collisions with `query.equals(candidate)` bytecode. Exactly correct by construction
(it IS Java semantics), no equals-replication, kills the 32% (Property.hashCode has no
toString/guava), and fixes RegularImmutableMap (its real equals is over logical entries).
arena_map already has the shape (hash + stored Value + bucket_matches) — swap hash_of(string)
→ hashCode(), bucket_matches string-compare → equals(). Residual risks: cross-rep
(Value::Str vs heap-String must share String.hashCode → native polynomial), identity objects
→ identity path, reentrancy (equals() re-enters interp → native_depth_inc).
⚠️ ATTEMPTED field-wise (walk arena cells) — REVERTED. Verify shield (RAVA_MAPKEY_VERIFY)
proved it corrupts: RegularImmutableMap cell-walk keys by backing-array HANDLES not entries
→ equal maps get different keys. Field-walk replicates equals incorrectly. Don't repeat it.

**Systemic Ref→Obj fastutil gap:** the fastutil native fast-paths (`is_int_set` in
invokevirtual; `fu_fastpath`/FuKind) fire for `Value::Ref` / JIT-compiled only. During arena
boot fastutil objects are `Value::Obj` running INTERPRETED → DoubleArrayList (~5-7% of
block-bootstrap, VoxelShape coords) and other fastutil get NO native path in the interpreter.
Adding an arena-Obj fastutil interpreter dispatch (generic library, no-adapt-clean) is a
safe Strike-method win — but touches VoxelShape data, so needs boot bit-exact validation.

## Negative result 2026-06-30c (DoubleArrayList native-back — DON'T repeat as-is)

After the toString memo, re-profile showed gson JsonReader ~22% + fastutil DoubleArrayList
~17% (VoxelShape coords). Tried a Strike-method native read fast-path for DoubleArrayList
getDouble/size in invokevirtual. Findings + dead-ends (saved so nobody repeats):
- DoubleArrayList is `is_collection` (contains "List") → allocated as a HANDLE (Value::Ref),
  NOT arena Obj. (Combo maps = arena Obj; double-lists = Ref. Know which before writing.)
- Its backing is the real `double[] a` + `int size`, populated by its own bytecode — NOT
  `$elems` (the native-collection backing isn't used here). Reading them needs
  `resolve_field_id(class,"a") + heap.get_field_hint(h,id,0)` (bare `get_field(h,"a")`
  returns Null — the field is stored under its interned id, not bare name). Same lesson as
  Strike-1 drain_map.
- BUT once correct (a=Array(256), size=256), MEASURED **no speedup** (bench/DoubleListStress,
  ON 55s ≈ OFF 50s). Root: getDouble is an O(1) method; the rava_invoke frame it saves
  (~440ns) is ≈ the per-call resolution cost I added (3× class_of string lookups + is_*
  set checks + 2× resolve_field_id). Net zero. **Strike-in-the-gauntlet only pays when it
  replaces O(n) work (array-map drain killed a bytecode entrySet iteration); for an O(1)
  accessor it doesn't.** REVERTED.
- To actually win on O(1) collection accessors: extend the dispatch IC to cache a NATIVE
  HANDLER per call-site (site → {double-list getDouble, field id}) so the hit is O(1) with
  zero re-resolution — the IC-native-handler infra (deferred, ~the same work as making the
  obj IC value an enum Plain(decl)|Native(kind)). Also covers the Ref-dispatch-IC gap (my
  class-id IC is Obj-only; Ref receivers like all fastutil collections still pay the full
  gauntlet every call). That gap (Ref fastutil pays ~650ns/getDouble = full gauntlet) is the
  real DoubleArrayList cost, not the frame.

## Negative result 2026-06-30d (Ref dispatch IC — net-negative for boot)

Extended the class-id dispatch IC (the Obj one, +24%, commit fa8ccbc) to `Value::Ref`
receivers (key = interned-class-name ptr via new `Heap::class_key`; safe — verified
is_lambda is class `$Lambda` and `$fnkind` is class `java/util/function/Function`, both
class-stable, so a fall-through caches safely with NO per-instance re-check, same as Obj).
Correctness PERFECT (boot to Environment[PROD] byte-IDENTICAL). But MEASURED a SLOWDOWN:
same-binary A/B OFF 212s → ON 293s to Environment (ON even had warm OS cache → real
regression). REVERTED.

Root (the decisive lesson): boot's hot Ref calls are COLLECTIONS served by native
early-returns in the gauntlet (guava cache, fastutil int-set, the native-collection
fallback) that return BEFORE the plain-dispatch populate → they never cache → every call
pays the added probe (class_key + thread-local hashmap) for nothing. The Obj IC wins
because Obj receivers are mostly plain-dispatch user/block-state classes that cache+hit;
Ref receivers are mostly collections that NEVER cache. A plain-dispatch IC is the wrong
tool for the Ref/collection path. (Same wall as the DoubleArrayList negative result above:
the per-call cost on a Ref collection accessor is the GAUNTLET + native dispatch, and you
can't shave it by caching plain dispatch — those calls don't take the plain path.)

⇒ The right lever for Ref/collection dispatch = **IC-NATIVE-HANDLER**: cache, per call-site,
the NATIVE dispatch decision itself (site → {fastutil int-set add | guava cache get |
double getDouble + field-id | …}) so a hit serves the native op directly, skipping the
gauntlet AND the rava_invoke frame. This caches exactly the early-return paths the plain IC
misses. Bigger refactor (gauntlet native-checks → a cacheable handler enum; IC value
becomes `Plain(decl) | Native(handler)`), correctness-critical (touches every collection
op), but it's the data-justified path to the collection per-op cost. Validate via boot
frontier (bit-exact) + bench/MapKeyStress-style deterministic A/B.

## RESEARCH-INFORMED PATH: copy-and-patch is the right subsystem, not LLVM 2026-07-01

Literature review (the boot's dominant cost is COLD-code interpretation — many methods
called < the compile threshold, interpreted at ~2-3M op/s; Cranelift is too expensive to
compile them all, PROVEN: RAVA_JIT_THRESHOLD=128 didn't reach Done in 380s). Known
solutions and fit for rava:
- **Copy-and-patch compilation** (Xu & Kjolstad, PLDI'21, arXiv:2011.13127) — THE best fit.
  Pre-built machine-code STENCILS per bytecode, patched with operands/jump targets at
  runtime. Compiles ~5 orders of magnitude faster than LLVM, ~as fast as an interpreter to
  build, produces code ~2x the interpreter's speed. Exactly rava's need: compile the WHOLE
  cold-code boot cheaply → run compiled from the first call, no interpret-then-warm. It is
  the technique behind CPython 3.13+'s JIT (proven, shipping). Recommended over LLVM tier-2
  for the STARTUP problem: LLVM buys peak throughput (irrelevant to a one-shot boot),
  copy-and-patch buys cheap universal compilation (exactly the boot win).
- Threaded/direct dispatch + superinstructions (Ertl, Cacao JVM) — a 2-3x interpreter
  speedup, but the boot op-mix is invoke/field/native-heavy (not fusable simple ops), so
  limited here; Rust's match already lowers to a jump table.
- AOT / template interpreter — persist compiled code across boots (Cranelift relocations
  make serialization hard); the true parity endgame but the largest build.
⇒ REVISED recommendation: build a **copy-and-patch tier** (stencils generated at build
time, then a runtime copy+patch loop) as tier-1 for ALL methods, keeping Cranelift as an
optional tier-2 for the few genuine hotspots. Attacks the measured bottleneck (cold-code
interpretation) head-on, far less work than a full LLVM integration. Still a multi-session
subsystem, but the RIGHT one. Sources: arXiv:2011.13127; complang.tuwien.ac.at/andi;
unibw.de/ucsrl taco14.

## CORRECTION + JIT-coverage grind status 2026-07-01

Goal session drove boot 412s → ~189s (~2.2x) via callback/alloc fixes + direct-call
extensions (see commits) + JIT-coverage work. Two coverage commits:
- `athrow` (eeed20f): emit_op had NO athrow arm → every method with a throw (bounds-check
  IndexOutOfBoundsException) hit `_=>None`. Now implemented (rava_throw stashes the
  exception as jit_pending). CORRECT, no regression. Enables athrow-containing methods to
  compile — but see below, the biggest one still bails elsewhere.
- callback_heavy cold-block (e567ba3): counts invokes on athrow-terminated (cold exception)
  blocks as native, not callback. CORRECT heuristic, no regression (~189s). ⚠️ **CLAIM
  CORRECTION: I stated `getDouble` now compiles + runs 1.86M times JIT'd — that was WRONG
  (misread interleaved multi-dump JIT-STATS output). `DoubleArrayList.getDouble(I)D` STILL
  fails: run=0, compile_fail=1, cached_fail=10.8M. It bails for a reason OTHER than
  callback_heavy — unidentified (likely an emit_op bailout in the exception-build path, or
  find_leaders shape). The callback_heavy change is still valid but its measured benefit is
  UNVERIFIED.**

TOP hot methods STILL failing to compile (RAVA_JIT_STATS, by interpreted-call count):
DoubleArrayList.getDouble(I)D 10.8M, VoxelShape.get(Axis,I)D 3.5M, AbstractDoubleList/
Collection.<init>()V 2.9M each, ArrayVoxelShape.getCoords 2.8M, CubePointRange.<init>,
Reference2ObjectArrayMap$EntrySet$1.next(). These share the VoxelShape/fastutil-double
subsystem. NEXT: instrument the compile bailout (which of exceptions/callback_heavy/
find_leaders/emit_op returns None) per hot method to find + fix each — this is the
concrete JIT-coverage grind toward the Cranelift ceiling (~40s). LESSON (again): verify a
compile-success claim by reading the FINAL single JIT-STATS dump for the EXACT method, not
interleaved output.

## DEFINITIVE: boot parity is architecture-bound (Cranelift can't reach it) — 2026-07-01

Measured end state after the callback/alloc/direct-call work: rava boots server.jar to
`Done` in **~194s** (down from 412s, ~2.1x), STABLE, bit-exact; peak RSS **~2.7 GB**.
JVM (Adoptium 22) same boot: **~9.7s wall**, ~1 GB. So rava is **~20x slower + ~2.7x more
RAM** — worse on every performance axis. Honest arithmetic for why parity is NOT reachable
by tuning:
- Boot = **1240M bytecode ops** (the program's op count — can't shrink materially).
- To match JVM's 9.7s = **128M op/s** required.
- rava op-rate ceilings (measured): interp arith 60M, interp dispatch 13M, JIT'd arith
  ~30-50M (Cranelift), actual boot ~2-3M (invoke-callback + cold-interp bound).
- EVEN a perfect rava (100% JIT coverage, zero callbacks) runs mixed boot code at the
  Cranelift ceiling ~30-50M op/s → ~25-40s = **still ~3-4x JVM**. Cranelift physically
  cannot emit 128M-op/s-quality code; C2 can.
- GC is NOT the bottleneck (1/57 heartbeats slow); it's raw compiled-code quality +
  coverage. Boot profile is MC worldgen (DistanceManager 20%, ChunkMap, LightEngine) +
  op-heavy gson — no clean generic-library Strike remains.

⇒ **Minimum parity requires an LLVM tier-2 (near-C2 codegen for the hottest methods) or
AOT — both major multi-session subsystems.** Single-thread Cranelift tuning caps at ~3-4x
JVM on cold boot; that ceiling is now largely reached (2.1x done). The remaining
enabling work, in order: (a) split JIT compile into main-thread PREP (intern/reserve/
register — the VM-mutating parts) + pure CODEGEN, so codegen can move to a background
thread AND be retargeted to LLVM; (b) LLVM tier-2 for the top-N hottest methods; (c) AOT
(persist compiled code across boots — the real parity endgame). RAM lever (separate,
atomic): default soft cap is 8 GB (crates/rava-cli DEFAULT_MEMORY_MB=8192) → lazy GC vs a
huge cap grows committed arena; a JVM-comparable cap + tighter old-gen would cut the 2.7 GB.

## GROUNDED END-TO-END BENCHMARK 2026-07-01 (the real gap is ~42x, not 1000x)

`rava -jar server.jar nogui` (flat, generate-structures=false) REACHES `Done` and is
STABLE. Measured wall-clock to the `Done (…)! For help` line (log timestamps / poll):
- **JVM (Adoptium 22): ~9.7s** to Done (14 log lines — MC's own boot is fast on a cached
  flat world; the "Done (0.2s)" the logs print is MC's final-phase timer, NOT wall).
- **rava (JIT=1): ~412s** to Done = **~42x slower**. (The memory's "~2000x / 7-8min" mixed
  up MC's phase timer with wall — the real gap is 42x and closeable.)

Where rava's 412s goes (RAVA_HEARTBEAT, full boot): **1240M bytecode ops** at avg ~2-3M
op/s. Phases: 0→Environment[PROD] ≈ 295s (72% — registry/block-state/DFU bootstrap);
Environment→Done ≈ 117s (recipes 1470 + advancements 1584 + loot/tags JSON + worldgen +
level). The op-rate (3M) is 4-6x BELOW rava's own dispatch interp floor (~13M op/s) — the
deficit is GC pauses + native time + the per-invoke tax, NOT the raw interpreter.

GC: 14 collections, HIGH churn (est ~2GB real, box field≈169k array≈15k → autoboxing).
Reducing allocation shrinks both GC pause (CPU) and RSS (RAM) — a two-axis lever.

⇒ To reach parity: close 42x. Since op count (1240M) ≈ the program's bytecode and can't
shrink much, the levers are (in wall-time order): (1) raise op-rate 3M→13M+ by cutting the
per-invoke tax (JIT-to-JIT direct calls — docs lever #2) + GC pause; (2) reduce allocation/
GC churn (autoboxing); (3) AOT to erase interp+warmup for hot classes. Multi-core is a
non-factor for this serial boot. This benchmark (server.jar → Done, wall-clock, same
machine) is THE metric for the goal — microbenches are proxies.

## CORRECTION: gson native-parse boot impact is NEGLIGIBLE (op-weight ≠ wall-time)

The gson commit (89a68ca) claimed "boot 309s→248s ~1.25x". That was NOISE — retested and
DISPROVEN. Follow-up measurements: a second A/B was OFF 295s → ON 300s (no change); and
the JsonParser intercept fires <200×/150s while the real driver (JsonElementTypeAdapter.read
via GsonHelper) fires ~ONCE/130s. The op-weighted RAVA_PROFILE shows JsonReader ~18-22%,
but that is a COUNT of ops, not TIME: the tokenizer does many CHEAP char-read ops, so its
share of WALL-TIME is far smaller. Native-parsing it (even fully) does not move the boot.
The gson_native feature is CORRECT (bit-exact, unit-tested — bench/GsonParse, GsonRead) and
a valid per-parse optimisation, but it is NOT a boot win. Kept default-OFF; do not expect
boot speedup from it.

MEASUREMENT LESSON (hard-won, add to the discipline list): the op-weighted profiler ranks
by OP COUNT, which OVERSTATES op-heavy/time-light hotspots (tokenizers, tight char loops).
Before committing to native-back a profiled hotspot, confirm it with a same-machine A/B on
wall-clock — an op-% is a hypothesis, not a wall-time cost. (This is why the array-map drain
and toString-memo wins are trusted — deterministic bench / same-binary back-to-back — and
gson is not.)

## Negative result: IC-native-handler (A) is net-negative — dispatch-probe approach is DEAD

Tried the IC-native-handler (cache per (site, class) the native dispatch decision, e.g.
fastutil int-set → tag 1; probe at the top of invokevirtual, serve the handler directly
on a hit, skipping the gauntlet). MEASURED a 4x REGRESSION on bench/IntSetStress (52ms →
210ms, JIT=0), correct (SUM matched) but far slower. Reverted.

Root: same wall as the Ref dispatch IC negative — a top-of-dispatch PROBE on the Ref path
(class_name_ptr + a thread-local hashmap get) runs on EVERY Ref virtual call, and that
overhead dominates the savings on the cached subset. This now confirms TWICE that **a
per-call dispatch probe on the Ref/collection path is net-negative**, whether it caches
plain dispatch (Ref-IC) or native handlers (A). The "IC-native-handler" idea — recommended
above as THE fix for collection cost — is WRONG: the probe is the problem.

⇒ Collection per-op cost can NOT be cut by adding a dispatch-layer cache. The viable
directions instead: (a) the JIT-level routing that already exists (fu_routable/fu_fastpath)
— compile more collection methods so they route natively WITHOUT an interpreter probe;
(b) per-bytecode-instruction specialisation woven INTO the opcode decode (no hashmap
probe), i.e. rewrite the bytecode/instruction stream to a specialised op the first time
(inline-cache-in-the-instruction, like a real template interpreter), not a side-table
lookup. Both are larger than a probe but avoid the per-call overhead that killed A.

## Executable plan: gson native-parse (C, ~22% of post-memo boot) — for a focused session

MC builds a JsonElement TREE via `JsonParser.parseReader`/`parseString` → `Streams.parse(JsonReader)`,
then DFU codecs (JsonOps) decode the tree. The JsonReader bytecode tokenizer is ~22% of
the post-toString-memo boot profile. Kill it by parsing natively and building the tree.

Steps:
1. New module `gson_native.rs`: a tolerant recursive-descent JSON parser (no `serde_json`
   dep — it's NOT in the lock; ~100 lines) → a Rust `enum Json { Null, Bool, Num(String),
   Str, Arr(Vec), Obj(Vec<(String,Json)>) }`. Keep number as the raw string (gson does).
2. A builder `build(vm, &Json) -> Value` that allocates the gson heap objects:
   - `com/google/gson/JsonObject` + its `members` field = a LinkedTreeMap populated with
     entries (LinkedTreeMap is is_native_map → use the native $map layer).
   - `com/google/gson/JsonArray` + `elements` = an ArrayList ($elems).
   - `com/google/gson/JsonPrimitive` + `value` = Str | Boolean | a
     `com/google/gson/internal/LazilyParsedNumber` whose `value` String = the number text
     (LazilyParsedNumber already has native intValue/etc support — natives.rs:452).
   - `JsonNull.INSTANCE` singleton.
3. Intercept in `invokestatic` (top, gated `RAVA_GSON_NATIVE`, default OFF): owner
   `com/google/gson/JsonParser`, name `parseString`/`parseReader`/`parse` → extract the JSON
   text (String arg, or the Reader's `$buf`) → parse → build → push. Maybe also
   `com/google/gson/internal/Streams.parse`.
4. KEY RISK + how to validate: the native-built JsonElement must be bit-exact-equivalent to
   the bytecode-built one for ALL downstream (`get`/`getAsString`/`getAsInt`/`getAsBoolean`/
   `has`/`entrySet`/`isJsonObject`/`size`/deepCopy/equals). The big unknown is whether to
   alloc JsonObject as arena Obj (matching bytecode `new`) vs Ref, and whether the `members`/
   `elements`/`value` fields must be set via arena cells (field-id) vs synthetic. Validate
   on boot-frontier (Environment[PROD], strip-timestamps diff vs OFF) — divergence = a
   downstream method read a field the builder didn't set; debug per-field. Default-OFF flag
   means the build stays byte-identical until the native tree proves bit-exact, then flip.
Effort: multi-hour subsystem, slow (6-min) boot validation per iteration. Not a Strike.

## Future JIT backend direction (user, 2026-06-30)

Decided directions for the codegen backend, in order. Cranelift is NOT today's
bottleneck (boot is interp/keying/GC-bound; Cranelift only governs the ~1.6-2.8x-of-C2
HOT-JIT path, and most of THAT headroom is reclaimable for free by fixing IR lowering —
SSA locals, the docs' lever #1). So the backend is a ~2x lever, not an order of magnitude.
Still, planned:

1. **Add an LLVM tier-2** (future, agreed good). Keep Cranelift as the fast tier-1
   (low compile latency, no warmup stall); compile only the top-N hottest methods with
   LLVM for ~1.5-2x-better peak codegen (near-C2). Tiered, exactly how HotSpot (C1/C2),
   GraalVM (Falcon), and Azul do it. LLVM's 10-100x slower compile is fine when gated to
   the few methods that dominate steady-state TPS. Prereq: background compile thread (so
   the slow LLVM compile never stalls execution) + an invocation/back-edge hotness counter
   to pick the tier-2 set.
2. **AOT, foreseeable future** (closed-world, GraalVM-Native-Image-style for server.jar).
   Compile hot classes to native ahead of time. Best peak, no warmup, but a different
   engine model (closed-world; loses arbitrary dynamic class loading) — a strategic
   multi-month effort, not a backend swap. (User tag: "c3 хз" — exact approach TBD.)

Order of payoff stays: SSA-locals IR fix (free Cranelift headroom) → multi-core
(per-thread nurseries; the only >1.0x lever) → LLVM tier-2 → AOT.

⚠️ MEASUREMENT BLOCKER (all of the above): this host does NOT reach Done and op/s swings
±2x (boot is single-core + phase-noisy: class-load/GC/JIT bursts). "host saturated" was a
WRONG inference — host CPU ~13%, idle cores; rava is single-core by GIL design and boot is
serial library bytecode + GC (parallelism can't help boot). Deterministic microbenches
(bench/MapKeyStress, Dispatch JIT=0) are the only reliable A/B here. The risky high-value
levers (hashCode/equals map model, arena fastutil dispatch) need a HEALTHY host that boots
to Done for bit-exact validation before shipping.

## Phase 1 (native-overhead attack) 2026-07-01b — profiler reframes it: sinks are INTERPRETED

Time-weighted profiler (RAVA_PROF) ENGINE split: boot ≈ bytecode 10% / native-Rust 71% /
GC 19%. KEY realization: the sampler fires at the INTERPRETER safepoint, so every sink it
shows is INTERPRETED code — compiled methods are invisible to it. So the top sinks
(VoxelShape ~20%, fastutil doubles ~20%, Reference2ObjectArrayMap + EntrySet iteration
~25-30%) are hot methods running INTERPRETED, i.e. NOT compiled. The lever is JIT coverage
of exactly those, not "faster bytecode".

Two shipped, both bit-exact, tests 147 green:
1. **Route fastutil DOUBLE lists** (commit: DoubleImmutableList/DoubleArrayList
   getDouble/size/isEmpty/iterator via fu_routable/fu_fastpath + new fu_double_view, the f64
   mirror of fu_int_view). The int-list mechanism (~12x) never covered doubles → the
   VoxelShape coord backing had no native path. MEASURED bench/DoubleList same-binary A/B
   getDouble hot loop 8.80s→3.85s (~2.3x), SUM bit-exact. NOT the interpreter-Strike
   (net-zero, doc above) nor a dispatch-probe (net-negative x2) — the JIT-routing path.
2. **callback_heavy counts fu_routable invokes as native** (commit). 96/102 boot compile
   bails were callback_heavy, incl. VoxelShape.get(Axis,I)D (12 bytes = getCoords().getDouble())
   and EntrySet$1.next (5 bytes). A fu_routable invoke lowers to rava_fu_invoke (native
   array serve), NOT a weight-3 method callback — so counting it as native is ACCURATE, not a
   relaxation. VoxelShape.get now scores native>callback and compiles (CBAIL-verified it stops
   bailing). Distinct from the reverted net-negative Ref-IC/IC-native-handler: those added a
   per-CALL runtime probe; this is compile-time-only, ZERO runtime overhead.

⚠️ PENDING: full-boot wall-clock A/B not done — machine was loaded (~0.4M op/s, 5x below the
2-3M floor) so a clean measurement was impossible. Both changes are correctness-safe +
overhead-free + microbench-positive; the boot-wall delta (the goal metric per the discipline
above) must still be confirmed on a quiet host, and reverted if net-negative. Remaining
interpreted sinks after these: object-map entrySet iteration (EntrySet$1.next — its invoke
isn't fu_routable, so the callback_heavy fix doesn't reach it) and the double <init>/abstract
ctors. Architecture ceiling (Cranelift ~3-4x JVM) from the DEFINITIVE section still stands —
this is coverage grind toward it, not past it.

## Empty-ctor-chain inline REVERTED: it exposes a latent root-gap (the advancement ULE hunt) 2026-07-02

The transitively-empty-()V-ctor inline (74a74e4 — the 31.5% ctor-sink kill) made boot FAIL
~100% of fast runs: `Failed to start` right after `Loaded 1470 recipes`. Bisect: base 2/2
DONE, +doubles+gate (c48ac24) 2/2 DONE, +ctor (74a74e4) 2/2 FAILED ⇒ trigger pinned.
REVERTED (345e5b3) — boot stable again. The change is semantically sound (an empty chain
writes/observes nothing); it EXPOSES a pre-existing timing-sensitive root gap by removing
millions of ctor callbacks (= GC safepoints with spill/reload) from the alloc-hot path, so
collections land at new code positions. Forensic trail for the follow-up hunt (all tools in
tree, zero-cost until failure):
- Failure: interpreter invokevirtual ULE `Advancement.validate` from ServerAdvancementManager
  .validate pc14; receiver Obj(0x200007420) hdr=0 (nursery slot already reset) → class_id 0 →
  decodes as the FIRST registered class ("bundler/Main"). Deterministic address across runs
  AND binaries. New [ULE-VIRT] dump prints this automatically (opcodes.rs, fatal path only).
- HEISENBUG discipline: ANY mutator-path slowdown masks it — RAVA_TRACE_MAINCATCH (per-catch
  env::var, now cached — fix ea61981), RAVA_RECEIVER_GUARD, parallel host load all → DONE.
  Only per-collection / fatal-path instrumentation preserves the repro.
- Ruled out (all silent while the failure reproduced): survivor trap RAVA_GC_TRAP (cells_all
  + raw arena scan), RAVA_TRACE_REFMASK (ref-mask gaps), find_arena_holders at the ULE (heap
  maps/arrays/FK_OBJ/lambda-captured — NOTHING holds the address), fu-doubles routing.
- RAVA_WATCH_ADDR=0x200007420 (pre-existing knob, alloc+copy hooks): the final tenant
  (class nf=7, almost certainly the Advancement record) WAS evacuated (copy_obj src=root)
  — so the object was reached + copied via SOME root; a SECOND holder location missed the
  writeback and later fed the interpreter the pre-move address. It is invisible to: frame
  gather (frames::all + trampoline + green), statics, gc_protect, native_args_gather, heap
  shield (arena_root_locations), arena_map temps, JIT cells_all. vm.pending is NOT gathered
  but is trampoline-only (not the default boot path) — still worth fixing when touched.
- Candidate windows for the missed holder (unproven, next session): (a) interpreter invoke*
  `locals: Vec<Value>` between pop_args and Frame creation IF anything in the dispatch
  gauntlet (resolve/ensure_init/native early-returns) can trigger an arena collection;
  (b) map.rs `forEach`/`entrySet` snapshot Vec<(Value,Value)> across per-entry vm.apply
  (bytecode lambdas allocate) — lib.rs drain paths already gc_protect theirs, map.rs forEach
  does NOT; (c) any native building a Rust-local Vec<Value> then calling apply without
  gc_protect/native_depth. Grep pattern: `vm.apply` inside loops over materialized Values.
- Fixed en route (real, kept): jit_direct_call barriered only cells[0]; the PIC-miss resolve
  (class load) can allocate+collect before the arg copy while the caller's args_slot is
  unrooted → REF ARGS crossed stale. Now every non-wide cell is barriered (wide exempt via
  wide_mask — long/double bits can alias young addrs; int/float sign-extended cells cannot).
⇒ The ctor-noop win (~31.5% of interpreter self-time) is BLOCKED on this hunt, not lost:
re-land 74a74e4 (`git revert 345e5b3`) once the root gap is found + fixed. The deterministic
repro: RAVA_JIT=1 boot on a quiet host → fails ~100% within ~3min at the advancement phase.

## ★ ADVANCEMENT-ULE ROOT GAP CRACKED + ctor-noop RELANDED 2026-07-02b

The hunt above CLOSED in one session. Root cause: **map.rs native `Map.forEach` snapshotted
all entries into a bare Rust Vec<(Value,Value)> and ran the BiConsumer per entry** — each
apply runs guest bytecode that allocates; a moving arena collection mid-loop forwards every
ROOTED holder but the Rust-local snapshot is invisible to every root scan, so the next
iteration passed a MOVED young value whose slot was already reset (class-id 0 → ULE).
Proof chain that pinned it: [ULE-STACK] frame-chain dump showed Map<Identifier,Advancement>
.forEach in flight (apply pc14) under the faulting jit_call synthetic frame; RAVA_WATCH_ADDR
proved the Advancement WAS evacuated via other roots at the fatal epoch (⇒ a NON-rooted
second holder); the collection-trigger backtrace ran through map/stream dispatch → vm.apply.

FIX (bb9fe97): the same heap-Object[] rooting transformEntries already used — snapshot into
a shield-scanned array, read each pair back FORWARDED per iteration. Also fixed the same-shape
computeIfAbsent/compute/merge (kv crossing the mapping fn then written back stale).
RELANDED (d23dbd9) the empty-ctor inline: with both, boot 3/3 to Done (was ~100% fail),
147 green, benches bit-exact. The 31.5% ctor sink win is BACK.

LESSONS (add to the discipline list):
1. **Any native holding Values in a Rust local across vm.apply/invoke is a latent stale-ptr
   bug** — the moving collector cannot see Rust stack locals. Root them in a heap Object[]
   (shield-scanned) or vm.gc_protect, and RE-READ after each apply. Audit grep: `vm.apply`
   inside loops / after snapshot Vecs in natives (reflect.rs:1272 forEach still unaudited).
2. Heisenbug discipline: per-catch env::var in a tracer was enough mutator slowdown to mask
   the bug the tracer exists to catch. Diagnostics must be fatal-path or per-collection only.
3. The [ULE-STACK] synthetic-frame signature (pc=0, empty locals) instantly distinguishes
   compiled-caller faults from interpreted-frame faults — keep it.

## Overnight session 2026-07-02: 180s → 166s + THREE bug classes fixed

Boot (clean host, log-timestamp walls, flat world): **JVM 8.3s / rava 166s = 20.0x** (was
21.7x at session start; 412s two days ago). All changes bit-exact (147 tests + DoubleList/
FastUtil benches + 3-run boot stability per change).

Landed (see commits): map/stream/reflect apply-crossing root fixes (+ the advancement-ULE
crack, re-landing the 31.5% ctor-noop win); JDK empty-root-ctor terminators (5.8M interp
ctor calls eliminated); native-served compile gate; **jit_indy primitive-result fix** (a
latent miscompile of every compiled record hashCode/equals — box-tag bits read as the
value); **tiny-method exemption default-ON** (≤16-byte bodies always compile; EntrySet$1
.next/getFaceSupportIndex/bridge sinks now native). Diagnosis tooling added: Class#method
deny bisect (hash-set backed), RAVA_DBG_TINY, arena-Obj [CATCH] decoding, [ULE-STACK].

3c/snapshot: two reentry sites trampolined (thread-start, MethodHandle tail call) — real
boundaries now flow; frontier = clinit/registry-freeze ordering divergence (see
snapshot-roadmap.md). Parked with full notes.

Open items: (a) th=128 NPE Supplier.get (flaky, non-default config); (b) the featureless
build broken since b406c90; (c) next sinks per JIT-STATS: Block.lambda$getShapeForEachState
$13 205k, DataResult.addLifecycle 234k, gson nextQuotedValue, ChunkMap spin loops; GC ~20%.

## Morning addendum 2026-07-02: 154s → 149-150s (negative direct-call cache)

New dmiss counters quantified the invoke residue: 376M invoke callbacks = 179M direct
hits + 176.6M (98%) 'resolve refused' misses RE-RESOLVED every call (positive-only cache).
Negative cache keyed by (site, receiver-class), re-probe every 64th call: 154 → 149-150s.
Threshold experiments at this state: 256 = same 149s; 128 still fails (the pre-existing
NPE Supplier.get — NOT the fixed indy bug; parked, non-default). 3c clinit window CLOSED
(sync init in both drivers; RAVA_PROBE_CLINIT = 0 reads) but the unbound-creative-tab
divergence PERSISTS — throw pinned to BuiltInRegistries.lambda$static$6 under
lambda$createContents$49 (the tab registry bootstrap reading its own just-registered
holders); next tool = a registration-order differential tracer (recursive vs stackful).

Standing: **149s vs JVM 8.3s = 18.0x** (412s → 149s = 2.8x in 3 days).

## AOT plan (user-approved 2026-07-02) + th=128 timing-bug status

**User ruling:** snapshots FORBIDDEN (state freeze). AOT is ALLOWED — it caches compiled
CODE, executes everything honestly, and is modloader-safe (cache keyed by post-transform
bytecode hash → a mixin'd method just misses and recompiles; the HotSpot-AppCDS/ART model).

**Mechanism (proven in cnp.rs tests c2_quality_aot_proof / c2_aot_reloc_roundtrip):**
Context::compile → code_buffer + Abs8 relocs → serialize → next process: alloc_exec +
patch symbols → run. To generalize to the real JIT:
1. Move method compilation from JITModule to codegen::Context (own linker): callbacks
   become UserExternalName indices resolved from a fixed symbol table (the rava_* fns).
2. THE key emission change: today the emitter BAKES runtime constants inline
   (class_ptr iconst, static cell indices, interned-string addresses, field ids) —
   invalid across processes. Introduce a per-method CONSTANT TABLE data slot; the code
   loads constants from it; the AOT loader fills it per process (class name → new
   class_ptr, field owner#name → new id, etc.). Symbolic entries mirror the reloc list.
3. Cache key: (method bytecode hash, descriptor, emission-affecting flags). Persist to
   RAVA_AOT_DIR; load-or-compile at jit_run.
Ceiling honesty: AOT removes interpret-then-warm and lets the WHOLE cold boot run
compiled (~30-50M op/s per the DEFINITIVE math) ⇒ est. 25-40s boot vs the current 149s —
a big real-speed step, still above JVM 8.3s; closing the rest = codegen quality (LLVM
tier) + multi-core.

**th=128 latent bug (blocks WIDER-than-512 coverage, not the AOT mechanism):** NPE
Supplier.get in DSL.taggedChoiceLazy (a schema-templates map VALUE is null). Bisect over
the 210 th=128-only compiled methods: BOTH halves fail ⇒ NOT a single miscompile — a
TIMING-class bug (compilation density shifts GC into yet another unrooted window), same
family as the advancement-ULE. Hunt tools ready; park until it blocks coverage expansion.

## Negative result 2026-07-02d: interp-direct call lane (reverted, un-committed)

Attempt at lever (а): cache (site, exact-receiver-class) → resolved INTERPRETED decl at
jit_direct_resolve's not-compiled bail (all gauntlet exclusions already passed there),
then call Vm::invoke directly from jit_call — skipping the synthetic frame + the whole
invokevirtual special-case chain for the ~170M interp-callee callbacks.

MEASURED: massive regression — the boot stalled in block-shapes with INFLATED interp op
counts (380M ops @105s where baseline has ~40-60M; jit=315 vs ~1600 methods compiled at
the same point). Mechanism NOT pinned (time-boxed): candidates — the lane starves the
interpreter's per-site call_cache/inline_cache warming that the synthetic-frame path
feeds (invoke() re-does a LINEAR class.file.method(name,desc) lookup per call where the
gauntlet had site caches), or a subtle re-dispatch loop. The doc's standing lesson holds
AGAIN: the Ref/collection dispatch path resists shortcut caches (3rd negative result of
this shape). NEXT TIME: profile the lane with per-call counters BEFORE full-boot A/B,
and consider feeding tlcache from the lane instead of bypassing it.

Status of the three levers (user goal): (а) gauntlet — negative result above; the
remaining honest reduction = compile more callees (blocked on the th=128 timing bug) or
AOT-everything; (б) AOT — plan + proven mechanism (see above), implementation next;
(в) multi-core — untouched this session (parallel pool exists, needs per-thread
nurseries for MC work — the documented Phase-3 increment).

## Lever (а) measurement 2026-07-02e: threshold-128 callback elimination (post-crack)

With the ArenaString map-key + dispatch fixes (0b136e9), aggressive compilation works.
Back-to-back same-host boots (loaded machine — walls comparable to each other only):
  th=128: invoke callbacks 349M, wall 347s   (Done, stable)
  th=512: invoke callbacks 414M, wall 349s   (Done, stable)
⇒ **−65M gauntlet callbacks (−16%) at th=128, wall-NEUTRAL** — the callback savings are
offset by the extra ~900 method compilations. THE LEVER CONNECTION: (а)'s next real win
IS (б) — with an AOT cache the compile cost moves off the boot path, and lower
thresholds (128, 64, …) become pure callback wins. Default threshold stays 512 until
the Cranelift AOT tier lands; then re-measure and flip.

## Lever (б) AOT cache VALIDATED end-to-end 2026-07-02f

bench/CnpBench (a pure-int static the cnp tier compiles) through RAVA_AOT_DIR:
  run1: 1 entry persisted, output 1509106146
  run2: links the persisted CompiledObj (no recompile), output 1509106146 — identical.
The cache pipeline (key → compile → persist → load → link → run) is proven on a real
workload, not just the unit test. What remains for (б) to move the BOOT is generalizing
the persisted-code path from the cnp tier (int-only statics) to the Cranelift tier: that
needs codegen::Context emission with Abs8 relocs + a per-method constant table (the
runtime-baked class_ptr / field-ids / static-cell indices / interned-string addresses
become table slots the loader fills per process). Mechanism proven (cnp.rs
c2_aot_reloc_roundtrip); the emitter refactor is the remaining build.

## Lever (б) increment survey 2026-07-02g — no small safe boot-relevant step (code-verified)

Investigated both AOT-coverage-growth paths in code before committing to either:
- **cnp long-extension**: cnp is a 32-bit int emitter (eax + movsxd; stack/locals are
  i64 slots). Adding long needs REX.W variants AND the correct JVM category-2 slot model
  (a long spans 2 local + 2 stack slots) threaded through the emitter, cnp_run
  marshalling, and all_int_static_sig — ~100+ lines of slot-arithmetic x86. AND it is
  ~zero boot value: the methods cnp actually fires on at boot are int-shaped (RGBA pack,
  isPowerOfTwo, bit-round). Real work, negligible boot payoff. Don't.
- **Cranelift AOT (the real boot lever)**: requires emitting via codegen::Context (not
  cranelift_jit::JITModule) so code_buffer + Abs8 relocs are extractable, PLUS a
  per-method constant table replacing every inline-baked runtime value (class_ptr
  iconst, field ids, static-cell indices, interned-string addresses). Multi-hour emitter
  refactor touching the whole compile path. Mechanism proven (cnp.rs
  c2_aot_reloc_roundtrip + the validated RAVA_AOT_DIR pipeline); the build is the work.
⇒ CONCLUSION: no small/safe/boot-relevant (б) increment exists — the Cranelift-Context
emitter refactor is the single remaining piece, and it needs a fresh full-context session
(starting it at exhausted context risks a half-done, broken JIT on a currently-green
branch: default boot Done, 148 tests, multicore beating JVM). Start there next session.

## Lever #4 live-server TPS — first measurement 2026-07-02h (previously deferred)

Booted server.jar to Done (JIT=1, flat world), then observed 70s of IDLE ticking:
  **0 "Can't keep up" warnings, no crash, stable** — the live tick loop keeps pace.
So on the running server (the surface where our fixed costs are lowest — small
fully-JIT'd tick loops, no classfile verification, no tiered re-profiling) rava holds
TPS at idle: functional parity at the "doesn't fall behind" level (the keep-up threshold
is >2s/tick; zero fired over 70s). This is the live-server data point lever #4 was
missing — the fork substrate already beats the JVM on compute (ArrReduce 9.6x, ParReduce
2.9x); the server itself keeps up.
NEXT for a hard TPS ratio: a per-tick timer native (DedicatedServer.tick) to get ms/tick
vs the JVM's ~sub-ms idle tick — the "how much margin" number beyond "keeps up".

## Lever C/#3 root cause 2026-07-03: CubePointRange forced to handle by native ancestor

C1 pinned RSS to 1.64M handle-objects (CubePointRange 605k/35MB + $Lambda 438k/25MB the
top two). Root cause of CubePointRange (the #1 RAM class): it `extends it.unimi.dsi.
fastutil.doubles.AbstractDoubleList`, so `has_native_backed_ancestor` returns true and
new_object_value/arena_new_site force it to a Box<Object> handle (native_super gate).
BUT CubePointRange uses NO native backing — it has one field `int parts` and computes
`getDouble(i)` arithmetically (i-th of `parts` evenly-spaced points), OVERRIDING the
abstract collection accessors with its own bytecode. The conservative native_super gate
(correct + necessary for ImmutableMapEntry, whose key/value ARE synthetic $fields) also
catches these compute-backed subclasses that don't need it.

BOUNDED FIX (generic, no-adapt, correctness-risky → needs careful boot bit-exact gating):
allow arena allocation for a native-collection subclass that DECLARES its own accessor
bytecode (getDouble/getInt/size/get) — i.e. fully overrides the native backing, so it
never reads a non-existent $elems. CubePointRange (605k) + BitSetDiscreteVoxelShape etc.
would go arena → flat cells, no Box+3-Vec overhead → the ~1.1GB handle-overhead RAM drop
toward the <1GB target, AND the field-access speedup (lever #3), one change. $Lambda
(438k) is a separate arena-migration (closures carry captured values) — bigger.
STATUS: root cause measured + pinned; the fix is the object-model migration the goal's
lever #3 estimates as multi-week (correctness-critical: a wrong native_super relaxation
silently no-ops a real native accessor). Not attempted at exhausted context — the gate
touches every arena allocation; a regression there corrupts boot silently.

## Lever C arena-override wall-time 2026-07-03: RAM −29%, speed NEUTRAL

Back-to-back same-host (load 1%): default boot 277s, RAVA_ARENA_OVERRIDE=1 boot 285s —
within noise. So arena-migrating CubePointRange is a RAM lever (−785MB/−29% RSS), NOT a
speed lever — GC was not the boot wall (confirms the compile-time finding: boot cost is
interpreted execution, not GC or compile). Both configs reach Done → the arena-override
relaxation is bit-exact on the full wall path too, strengthening the case to promote it
to default after more runs. (Host was thermal/slow this run — 277s vs the 150s quiet
baseline — but the two configs measured back-to-back, so the RELATIVE neutrality holds.)

## Lever C arena-override DEFAULT-ON decision 2026-07-03: STAY OPT-IN

Considered promoting overrides_collection_backing to default (permanent RAM win). Rejected
after edge-case analysis: the predicate (declares getDouble/getInt/get + size) does NOT
prove the subclass never touches the inherited native $elems backing via OTHER inherited
methods (iterator/stream/toDoubleArray/removeDouble) or a mutable set()/add(). Boot-to-Done
bit-exact only exercises the VoxelShape-BUILD path; default-on would arena-allocate these
in ALL paths (worldgen, runtime, ticks), where an inherited native method on an arena
receiver no-ops/underflows or (mutable case) silently loses writes. So it stays opt-in
(RAVA_ARENA_OVERRIDE) until validated by a boot-past-Done + idle-tick run that exercises
VoxelShape at runtime, OR the predicate is narrowed to immutable + fully-overriding classes.
The −29% RSS win is real and measured; promotion is gated on that runtime validation.

## Lever C RAM progress 2026-07-03: -36% RSS (2710→1735MB), two additive wins

Measured (real server.jar boot, machine thermal ~285s but RSS is load-insensitive):
  original default            : peak RSS 2710MB
  boxed Object.lambda (DEFAULT): 1958MB  (-28%, -752MB) — permanent, no flag, pure layout
  + arena-override CubePointRange (opt-in RAVA_ARENA_OVERRIDE): 1735MB (-36% total, -975MB)
Both bit-exact (boot Done). Toward the <1GB JVM target: -36% done, ~735MB more to go.
Remaining RAM sources (from RAVA_HEAP_DUMP): $Lambda ~1M objects (the closures themselves —
each a real Box<Lambda> now, so arena-migrating closures is the next big cut), Optional
400k, the 3 parallel SmallVec<[i64;4]> per Object (32-byte cell padding on small objects),
and parsed classfile structures (thousands of MC/library classes held in memory). The
boxed-lambda win is the cleanest (default, zero-risk); the rest are object-model surgery
(lever #3, correctness-sensitive).

## Lever #4 TPS — HONEST framing 2026-07-03 (corrects the f6eb2c4 commit-msg overclaim)

RAVA_TICK_PROF counts MinecraftServer.tickServer + wall. MEASURED (real booted server,
idle flat world): 40.3 ticks/s sustained (~25ms/tick period).

A RATE comparison vs JVM is INVALID: Minecraft's runServer paces to 20 TPS with a
sleep-to-target, so a correctly-pacing server shows ~20 ticks/s REGARDLESS of per-tick
speed. rava's 40/s means rava's loop runs UNCAPPED (pacing doesn't hold the 20 cap) — so
40/s is rava's RAW throughput (~25ms/tick), NOT "2x JVM".
- PROVEN: rava's per-tick work (~25ms) fits the 50ms budget with 2x headroom → rava is
  NOT TPS-bottlenecked; it holds a live server comfortably on idle.
- NOT proven: that rava's per-tick WORK beats JVM's. JVM caps at 20/s by design; its raw
  throughput is unmeasured. A fair "faster than JVM" needs per-tick-WORK timing (tickServer
  entry→exit) under LOAD (entities/players), not idle rate.
So lever #4 = "rava runs the live server fine, TPS held with headroom" — a real result,
but the earlier "at/above JVM parity / faster on real load" phrasing was NOT rigorous and
is retracted. The rigorous compute win is separate and stands: fork substrate 9.6x/2.9x on
reductions, bit-exact. NEXT for a real TPS-vs-JVM number: time tick WORK (entry→exit) on
both runtimes under a fixed entity load.

## A2 AOT run-path — status 2026-07-03: pure subset WORKS, call/write bug pinned

The AOT translator output now EXECUTES on the hot path (RAVA_AOT_CLIF, aot_run mirrors
jit_run's marshalling/rooting/decode + run_const shim). VALIDATED: a full server.jar boots
to Done BIT-EXACT with the pure read/numeric/control-flow subset running as AOT machine
code from call #64. So the marshalling (locals→cells box_ref/pack_local, long 2-slot),
GC-rooting (cells_push), run_const ABI, and result decode are all CORRECT.

CALL/WRITE BUG (default-denied; RAVA_AOT_UNSAFE lifts): a method containing invoke/new/
ldc/getstatic/indy/anewarray/putfield/athrow/aastore miscompiles. Pinned:
- Symptom: java/lang/Object.equals with a NULL receiver in com/sun/jna/Structure.
  lambda$getFieldList$0(Class,Class) pc=23 — a field-list comparator.
- [NPE-FRAME]: local[0]==local[1]==Ref(1) (the SAME Class object in BOTH lambda params)
  → an ARGS-marshalling bug (an arg duplicated / a wrong value), not a null field.
- RAVA_AOT_TRACE_NULLRET did NOT fire → the null is NOT a top-level AOT method return;
  it's the args PASSED to an invoke (the comparator gets the wrong two args) or a mid-
  method getfield. Same class as the th=128 ArenaString hunt.
- Bisect (RAVA_AOT_DENY_OPS): the failure survives denying each single invoke kind →
  likely a systematic issue in the shared invoke emit (args buffer / receiver ordering /
  wide-arg cell count vs the callee's 2-slot long expansion), not one kind.
- Ruled out: caller_ptr (Arc::as_ptr matches jit_run), invokestatic phantom receiver
  (fixed), GC-spill of live stack refs (added, didn't fix). 
NEXT (focused session): trace the exact args buffer a failing invoke builds vs what the
interpreter marshals for the same call — compare cell-by-cell. Likely the wide-arg (long/
double) cell count in the buffer vs the callee's expand_locals 2-slot expectation, OR a
dup/dup_x1 width bug feeding duplicated refs.

## A2 invoke bug — deeper findings 2026-07-03 (NON-DETERMINISTIC → rooting/timing)

More instrumentation ruled out the obvious:
- RAVA_AOT_TRACE_INVOKE (rava_invoke logs a non-static call with buf[0]==0): did NOT fire
  → the invoke RECEIVER is never null; args marshal with a valid receiver.
- RAVA_AOT_TRACE_NULLRET: did NOT fire → no top-level AOT method returns null.
- Structural compare to the JIT's emit_invoke (jit.rs): my args marshalling matches —
  pop total=count_args+(recv if non-static), stack_store to a slot at i*8, spill live
  refs, stack_addr, rava_invoke(cls, cp, kind, buf, total). to_cell for int = sextend,
  which my always-i64 stack already does. jit_call decodes raw 1-cell-per-arg (long=1
  cell) — matches my buffer convention.
- The failure point VARIES between runs (early SystemReport vs late jna Structure) ⇒
  NON-DETERMINISTIC ⇒ a GC-TIMING / ROOTING bug in an invoke-containing method, not a
  static marshalling error. A live ref goes unrooted across a GC the callee triggers.
- Ruled out for rooting: operand-stack refs ARE spilled to cells[max_locals+k] (rooted
  span) before every GC-capable call and reloaded after; locals live in the rooted cells
  buffer (forwarded in place). Yet a ref still goes stale.
NEXT (focused, DETERMINISTIC harness — stop using flaky boots): compile a tiny Java class
(callee + caller with invokestatic + a ref local held across the call), load via
ClassFile::parse + LoadedClass::new, run caller via aot_run under forced GC (RAVA_GC_STRESS)
vs the interpreter, diff. That isolates the exact unrooted-ref path without a 5-min boot.
Suspect: the spill/reload indexes by operand-stack position, but a ref materialised into a
Cranelift value from a getfield BEFORE the call and consumed AFTER may not be on the
tracked `stack` vec at spill time (e.g. dup/dup_x1 copies, or a value the emitter holds
in an intermediate). Audit every site that holds a ref Value across a GC-capable call.

---

## SESSION 4 (2026-07-04): exec-from-call-1 BOOTS DONE + A5 disk-cache order-sensitivity

**★ MILESTONE: exec-from-call-1 (RAVA_AOT_FROM_CALL1, threshold 1 — EVERY method runs AOT
machine code from its first call) boots server.jar to Done RELIABLY (3/3 fresh runs, 0
panics).** The A2 boot-WIN correctness path works. Cleared via this session's fixes.

Fixes committed (all keep default byte-identical + threshold=512 boots Done):
- A1 COMPLETE: translator covers int/long/float/double (arith/conv/cmp/array-loads) + wide
  (long/double) fields (field_of→FieldKind) + string-ldc INTERNING (LdcConst::Str→StrLit→
  intern_arena_string, matches the JIT; NOT alloc_arena_string per call which broke identity).
- LDC crack: rava_ldc is REFERENCE-only; numeric ldc must be inline; a numeric ldc marked
  is_ref spilled a fake handle → cell_to_root roots garbage → GC corruption.
- 9 native Str-vs-ArenaString fixes (getResourceAsStream, getProperty/getenv, Charset.forName,
  mapLibraryName, Round/Enum.valueOf, parseInt/parseLong, UUID.fromString, requireNonNull) —
  compiled callers pass Value::Obj ArenaString; Str-only natives returned wrong/Null (th=128
  class, latent for ANY compiled caller).
- EXCEPTION-TABLE bail: translator has no catch support → try-catch methods bail like the JIT
  (cleared the JNA jnidispatch UnsatisfiedLinkError-catch-skip fatal).
- Arena layout completeness: class_field_layout only caches a COMPLETE super chain; partial
  (a super transiently unloadable) → new_object_value heap-fallback instead of a short-celled
  arena object (killed 'arena putfield non-layout field id=18 vs layout [29,30,31]').
- field_declaring_id (strict): bake a field id only if a declaring class is found; no
  timing-dependent bare-name fallback.
- aot_key includes the rava binary mtime (rebuilt translator → stale code never reused).

**A5 WALL MEASUREMENT — BLOCKED (deep order-sensitivity):** exec-from-call-1 FRESH boots Done
but is slow (compiles every method). The wall-WIN needs the disk cache (RAVA_AOT_DIR) so
boot#2 loads pre-compiled. BUT every disk-cache boot fails (arena-putfield panic → fixed →
now an early SLF4J/oshi-area failure), at BOTH threshold 1 and 8, while fresh (any threshold)
works. ROOT: the CompiledObj to_bytes/from_bytes round-trip is CORRECT (verified) — the disk
cache is not corrupting anything; it SHIFTS TIMING (disk-load is faster than compile, so a
method becomes AOT-ready on an earlier call → runs AOT earlier → exposes latent ORDER-
SENSITIVITY: field-id resolution vs class-load ordering; an AOT method baking/running before
a class is fully loaded diverges from the interp's runtime resolution). The default's slow
warmup (threshold 512, compile-not-load) masks this. So the A5 wall-win is a deep multi-
session order-sensitivity fix, NOT a serialization bug. Measured: default AOT@512 boots Done
(0.733s MC timer, ~568s wall under a shared machine), peak RSS 2650MB.

NEXT for A5: make the AOT run-path order-INSENSITIVE — either (a) resolve field ids lazily at
first execution (like interp) instead of baking at compile/link, or (b) gate a method's AOT
availability on its referenced classes being fully loaded, or (c) a moderate-threshold cache
that only persists methods whose class graph is settled. Then boot#2 wall vs default.
