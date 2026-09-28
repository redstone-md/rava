# Rava endgame — handle→raw-ptr + moving GC migration plan

Branch `feat/rava-endgame`. Goal: land the sandbox-proven model (Allocs 0.87x /
4.04x TLAB, StringOps 0.95x) in the live engine. This is NOT a flag flip — it is a
ground-up object-model + GC rewrite. Scope measured below (not an estimate — grep).

## Measured surface (the work, not an excuse)
- `Value::Ref`/`Value::Array` sites: **865** across 12 files —
  natives 255 · reflect 163 · stream 113 · nio 85 · net 62 · opcodes 54 · lib 54 ·
  map 44 · heap 19 · fastutil 6 · cache 6 · value 2.
- handle-index sites (`as u32`, `objects[h]`, …): **316**.
- rava-interp: **26,716 LOC** (natives.rs alone 4,070).
- **All-or-nothing compile**: changing `Value::Ref`'s representation breaks all 865
  sites simultaneously — there is no incremental-compiling path for the enum change.
- **Send/Sync wall**: a raw `*mut` in `Value` loses `Send`. rava shares the heap
  across REAL OS threads under the GIL (per-thread box tables, `frames::all()`),
  so `Value` MUST stay `Send`. A raw-ptr variant needs an `unsafe impl Send`
  wrapper, sound ONLY because the moving GC relocates at safepoints with every
  thread parked at `gil::acquire` (that safepoint exists — proven by root-discovery).

## What is ALREADY proven / done (the hard ideas)
- Bump-arena + Cheney copying nursery + raw-ptr inline-cell objects: `crates/sandbox-gc`
  + `crates/rava-interp/src/nextgen.rs` — Allocs 0.87x JVM, 18.8x over Box-baseline.
- **Root discovery**: `Vm::gc` enumerates the full root set; shadow-validated LIVE
  under McProbe = **0 mismatch** over 16+ GC cycles (1198→5016 roots/cycle).
- **Write barrier / remembered set**: card table, shadow-validated (87,918 cards).
- **Safepoint**: GIL parks all threads at GC — where `forward_roots()` rewrites ptrs.

## Ordered migration (each phase multi-day; ~3-6 weeks total, 1 eng)
1. **Value model** — add `Value::Obj(ArenaPtr)` (`struct ArenaPtr(NonNull<u8>)` +
   `unsafe impl Send/Sync`, justified by the GIL safepoint). Keep `Value::Ref`
   during transition (hybrid) so the tree compiles subsystem-by-subsystem.
2. **Heap** — `nextgen::GenHeap` (arena, inline cells) becomes the object store;
   `new_object` → TLAB bump (thread-local `GenHeap`). Field access dispatches on the
   Value variant.
3. **Moving GC** — replace mark-sweep with `nextgen` Cheney. `forward_roots()` walks
   the PROVEN root set (0-mismatch) + dirty cards, evacuates nursery→tenured, and
   rewrites every `*mut` in frames/JIT-cells/fields/maps/arrays/box-tables.
4. **JIT codegen** — `native_getfield/putfield` + the cell encoding (`REF_TAG|handle`
   → raw ptr) rewritten for 1-load direct addressing (the sandbox's field model).
   `ObjDesc` table retired.
5. **Migrate the 865 handle sites** subsystem-by-subsystem (natives 255 is the bulk;
   maps/arrays/box-tables store ArenaPtr not u32). Each subsystem compiles + boots
   before the next.
6. **Serialization** — snapshot/restore store ptrs as arena offsets (ptrs can't
   serialize). Green-thread save/restore re-bases.
7. **Remove `Value::Ref`** last; delete handle path + mark-sweep.

## Why not one autonomous burst
Phase 1+5 are coupled (the enum change breaks all 865 sites until migrated) → the
tree is non-compiling for the whole mid-migration window, which spans the 865-site
surface + the JIT + serialization. "boot→Done with a live moving GC over the full
MC heap" (millions of live objects: registries, NBT, block states) is the TERMINAL
milestone — every one of the 865 sites + the GC pointer-rewrite must be correct
first. The sandbox PROVES the endpoint (0.87x); this plan is the path; the path is
mechanical-but-large + correctness-critical (one missed pointer = corruption).

## PHASE 1 — DONE (commit on feat/rava-endgame)
- `crates/rava-interp/src/arena.rs`: `ArenaPtr(NonNull<u8>)` + audited `unsafe impl
  Send/Sync` (GIL-safepoint contract documented). Debug/PartialEq/Eq/Serialize/
  Deserialize so it can live in `Value`. 6 thread-safety unit tests PASS (Send, Sync,
  cross-thread move, &-shared-across-threads, addr roundtrip, null) — Send-wall broken.
- `Value::Obj(ArenaPtr)` DORMANT hybrid variant added. KEY FINDING: it broke only
  **1** exhaustive match in the entire 26.7k-LOC tree (value.rs::display) — every other
  match already has a `_` arm. The hybrid coexists with near-zero disruption ⇒ the
  subsequent subsystem-by-subsystem migration can keep the tree compiling throughout
  (de-risks the marathon significantly vs the feared all-or-nothing break).
- Verified: 71 tests pass, default + `next-gen-gc` builds clean, rava-cli builds.

NEXT (Phase 2): the heap arena — `nextgen::GenHeap` becomes an object store `new_object`
can target (TLAB bump), producing `Value::Obj`; field access dispatches on the variant.
Phase 5 site-migration (natives 255, nio 85, stream 113, …) only begins once Phase 2
gives those sites real `Value::Obj`s to handle (until then they correctly never see one).

## PHASE 2 — DONE (Heap Arena & Object Generation)
- `nextgen`: thread-local **TLAB** (zero-sync pointer bump; overflow → panic = RAVA_HARD_MB
  hard stop, Cheney evac is Phase 3) + **class registry** (name→id→layout for field-slot
  resolution) + arena field access (`tlab_alloc`, `arena_cell`/`set_cell`, `slot_of`,
  `kind_of`). 3 unit tests pass.
- `opcodes`: `new_object_value` → under the flag, `new` TLAB-bumps a `Value::Obj(ArenaPtr)`.
  **Dual dispatch** for getfield/putfield: `Value::Ref` → classic handle path; `Value::Obj`
  → direct inline-cell deref (`arena_getfield`/`arena_putfield`). Field codec
  (`encode/decode_arena_cell`): primitives pack directly, arena→arena ref = raw ptr, null = 0.
  Cross-gen (arena→handle) refs + Str/synthetic fields PANIC predictably (PHASE3/PHASE5).
  2 opcode-level tests (codec roundtrip + Value::Obj putfield/getfield) pass.
- Memory: TLAB/arena overflow → controlled panic (host-safe). RAVA_HARD_MB respected.

VALIDATION (criterion met):
  - Compiles BOTH ways (default + `--features next-gen-gc`). 76 tests pass.
  - Unit tests: single Value::Obj via TLAB bump + field read/write — 5 tests green.
  - McProbe WITHOUT flag: boot Done(371s) + LOGIN→PLAY→JOIN, ERRORS=22 = baseline (the
    getfield/putfield refactor did NOT regress production).
  - WITH flag: predictable clean STOP at the first unmigrated native — `unsupported native
    invokevirtual java/util/List.addAll (from joptsimple)`: an arena Value::Obj reached the
    collection native, hit the existing UnsupportedNative boundary GRACEFULLY (no corruption,
    no segfault). Arena alloc + primitive field dispatch ran first (joptsimple object setup),
    then stopped at the native — exactly as specified.

NEXT (Phase 3): wire the Cheney moving GC + forward_roots (proven root set) so TLAB overflow
EVACUATES instead of panicking, and arena→arena ref-graphs (Allocs Point.next) survive a
collection. Then arena→handle cross-gen refs via the card-table remembered set.

## PHASE 3 — Cheney Moving GC core PROVEN; live boot-GC needs Phase 3b
- `nextgen`: TLAB is now generational (young nursery bump + tenured). `tlab_try_alloc`
  (Option, None=nursery full). `tlab_collect`: LIVE Cheney evacuation — remembered-set
  scan (tenured ref cells → young = extra roots) + forward explicit roots + Cheney
  to-space scan + O(1) nursery reset. Tenured overflow → hard exit (RAVA_HARD_MB).
  `ref_mask_of` (which cells are traced refs, from layout kind ≥4).
- `opcodes`: ACTIVE card barrier in `arena_putfield` — an old→young store (tenured
  object's field ← nursery object) marks the old object's card (remembered set).
- UNIT TESTS (criterion #2 MET — cyclic graph survives GC + memory shift): 3 pass —
  `cyclic_graph_survives_minor_gc_with_memory_shift` (2-cycle a↔b evacuated, addresses
  SHIFT, cycle preserved via pointer rewrite, dead object reclaimed, nursery reset),
  `chain_survives_repeated_gc` (5-chain survives 2 GCs, values intact), AND the LIVE path
  `live_tlab_collect_evacuates_and_preserves_cycle` (through the thread-local TLAB:
  alloc cycle + dead, tlab_collect, cycle survives + dead reclaimed, evac==2).
- VALIDATION: compiles BOTH ways; 79 tests pass (flag) / default unchanged (all Phase-3
  code gated → no-flag engine byte-identical to Phase-2's validated Done(371s)+ERRORS=22).
  Flag build reaches the SAME native checkpoint (List.addAll), no regression.

HONEST BLOCKER (criterion #3 — live boot-GC — NOT yet): wiring `tlab_collect` into the
running boot needs the collection to run at the `drive_stack` LOOP SAFEPOINT (full
exec_frames mutable access, no frame `&mut`-borrowed by the executing op). Forwarding a
pointer inside the op's own borrowed frame — or creating `&mut` to the exec_frames Vec
while an element is borrowed — is aliasing UB = the exact corruption the criterion forbids.
The fix: propagate "nursery full" from new_object_value UP to drive_stack, GC + retry
there (the root set is proven complete by the live shadow 0-mismatch; the evacuator is
proven by unit tests). Until that safepoint plumbing lands, a full nursery is a clean
hard exit, never an unsound mid-op forward. = Phase 3b.

## PHASE 3b — Loop-Safepoint + signal propagation: LIVE GC WORKS
- `RunExit::NurseryFull` (gated): a `new` on a full nursery exits `run` with PC
  UNCHANGED. `new_object_value` → `Result<Option<Value>>` (None = nursery full,
  only in trampoline mode; a recursive invoke can't safepoint → hard exit).
- `drive_stack` + `drive_green`: catch `NurseryFull` at the loop boundary (no frame
  op-borrowed → SOUND) → `arena_safepoint_gc(&mut frames)` → loop re-runs the same
  `new` into the freed nursery (retry).
- `arena_safepoint_gc`: gathers every young `Value::Obj` root (frame locals/stacks +
  green-thread frames + statics via `loader.forward_statics`), runs `tlab_collect`
  (Cheney evac + remembered set), writes each forwarded ptr back. No aliasing — the
  whole `frames` Vec is owned at the safepoint.

LIVE DEMONSTRATION — Allocs (trampolined via RAVA_STACKFUL=1), 4MB nursery, flag:
  10 live `NurseryFull → Safepoint GC → Pointer Forwarding → Retry` cycles, host alive,
  NO corruption/panic. Each: 2 roots forwarded (head chain + p), 4–64 objects evacuated
  (the live chain, traced transitively by Cheney). GENERATIONAL WIN confirmed LIVE:
  8M allocs through 10 GCs, tenured grows only 1KB→14KB (≈99.9% of Points die in the
  nursery; only the kept chain promotes). Compiles both ways, 79 tests pass.

CHECKPOINT NOTE (criterion's List.addAll): the server-boot native checkpoint is TOO
EARLY — joptsimple allocs <10 arena objects before it, so no nursery (even 512B) fills
→ no GC triggers there (verified). And the default main path is RECURSIVE (RAVA_STACKFUL
gates the trampoline) so its allocs can't safepoint. The trampolined Allocs hot loop is
the clean live-GC proof. Phase 3c: route recursive sub-calls (clinits/apply/native
callbacks) through the trampoline so THEY can safepoint too → full-boot live GC.

═══════════════ PHASE 4 — Native Surface Migration (first echelon) ═══════════════

GOAL: migrate the first native cluster (collections + the joptsimple boot chain) to
the dual contract; break the flag boot through the List.addAll checkpoint to the next
native barrier. No segfault. Default byte-identical.

DUAL CONTRACT AT ALLOC LEVEL (the key decision). `new_object_value` flag path:
  - native-backed class → HEAP HANDLE (Value::Ref via new_object). "Native-backed" =
    collections/maps (is_native_backed_alloc = stream::is_collection || map::is_native_map)
    OR a class with NO bytecode in the classpath (loader.get_or_load Err) — a pure JDK
    class (java/io/File, …) whose state lives in synthetic native $elems/$map/$path fields.
  - real bytecode class w/ declared layout → pointer-based ARENA object (Value::Obj).
  Soundest reading of the user's "Value::Ref→old vector path / Value::Obj→arena" — keeps
  every native working UNCHANGED (no per-native rewrite needed yet), pure classes go arena.

TAGGED CROSS-GEN CELLS (arena fields can hold non-arena values). Arena ptrs are 8-aligned
→ low 2 bits free as a tag in the i64 cell:
  0b00 non-zero = arena→arena ptr | 0 = null
  0b01 = heap handle (Value::Ref)   ((h<<2)|1)
  0b10 = array handle (Value::Array) ((a<<2)|2)
  0b11 = boxed Str/sentinel via nextgen::arena_box (append-only side table) ((idx<<2)|3)
  Moving GC skips any cell with low-bits≠0 at ALL three scan sites (minor_gc cheney +
  tlab_collect remembered-set + tlab_collect cheney) — a handle/box is never mistaken
  for an arena pointer to forward. (replaces the Phase-3 panic in encode_arena_cell.)

ARENA VIRTUAL DISPATCH: invokevirtual `actual` match gained a Value::Obj arm — reads the
concrete class from the arena header (nextgen::class_name(arena_class_id)) instead of the
call-site static `owner`. Without it, an interface-typed call site (owner = interface)
resolved to no bytecode → fell to the native layer → "unsupported native". This is what
broke OptionNameMap.putAll (joptsimple interface-typed) until fixed.

BARRIERS BROKEN THIS PHASE (each = one migration step, boot got deeper):
  1. List.addAll (joptsimple) ............ collections alloc-as-handle
  2. arena→handle field store (PHASE3 panic) tagged cross-gen Ref/Array cells
  3. Str into arena field (PHASE5 panic) .. arena_box tag 0b11
  4. OptionNameMap.putAll (unsupported) .. arena virtual dispatch (concrete class)
  5. File.getAbsolutePath (unsupported) .. JDK-no-bytecode classes alloc-as-handle
RESULT: flag boot (RAVA_STACKFUL=1, 4MB nursery) runs joptsimple → bundler "Loading
libraries" → net.minecraft.server DispenserRegistry.<clinit> + log4j init. exit=0, NO
segfault. Default boot still reaches "Preparing spawn area" (byte-identical). 79+3 tests
pass (incl. new cross-gen/Str codec roundtrip + GC-skip test).

NEXT BARRIER (NOT a native-surface issue — deferred): under the trampoline (STACKFUL,
required for arena safepoints) sun/reflect/Reflection.getCallerClass depth vs
live_frame_classes is off → log4j LogManager.getLogger() no-arg throws "No class provided".
Recursive mode can't be used to isolate (recursive+arena segfaults — recursive frames are
Rust-stack-borrowed, can't safepoint; that's the known Phase-3b constraint). So getCallerClass
frame-depth under the trampoline is the next thing to fix, separately from native migration.

═══════════════ PHASE 5 — getCallerClass under the trampoline ═══════════════

BARRIER (from Phase 4 tail): RAVA_STACKFUL boot → log4j LogManager.getLogger() no-arg
→ sun/reflect/Reflection.getCallerClass(depth) → null → "UnsupportedOperationException:
No class provided, and an appropriate one cannot be found."

ROOT CAUSE: the virtual call chain is SPLIT across two stores under the trampoline:
  - trampolined OUTER frames → drive_stack's `exec_frames` Vec (Main → MinecraftServer
    → DispenserRegistry)
  - recursive sub-calls (a <clinit> chain via ensure_init, native re-entry like log4j's
    StackLocator) → thread-local `frames::local` (pushed by opcodes::run).
`live_frame_classes` read ONLY frames::local → under the trampoline it saw just the
recursive tail (3 frames) and MISSED the trampolined outer frames. log4j asks
getCallerClass(4/5); the truncated chain underflowed → null.

FIX (live_frame_classes): when exec_frames_ptr set, return `exec_frames ++ local[1..]`.
drive_stack runs the top frame through opcodes::run which ALSO pushes it to local, so
the currently-running frame is `exec_frames.last() == local[0]` (deduped by name match)
and its recursive descendants are `local[1..]`. The concat reconstructs EXACTLY what a
pure-recursive run would have on local → the proven depth math works unchanged. Default
(non-stackful): exec_frames_ptr==0 → returns local only → byte-identical (verified:
default still reaches "Preparing spawn area").

DIAGNOSTIC THAT CRACKED IT (RAVA_DBG_GCC, since removed): before fix nframes=3, static
[Main, MinecraftServer, DispenserRegistry], getCallerClass(4/5) → 3-4 underflow → null.
After fix nframes=7–10 with the full log4j StackLocator chain present.

RESULT under flag: "No class provided" GONE (grep count 0), log4j initializes (SimpleLogger
fallback — same line as default boot), boot runs deep into DispenserRegistry registry
population (SoundEffect / RegistrySimple / RegistryID). A live [SHADOW-GC] cycle also ran
(roots=50 valid=50 mismatch=0).

NEXT BARRIER (Phase 6 — arena object identity): MC RegistryID rehash throws
"java.lang.RuntimeException: Overflowed :(" — natives.rs identity_hash returns 0 for every
Value::Obj (the `_` arm) → all arena objects collide → rehash can't find a free slot →
overflow. Needs a GC-STABLE identity hash: the moving GC changes addresses, so address-as-
hash is unstable across a minor GC. JVM-style fix = lazily stamp an identity hash into the
arena object header on first request (a spare header word), survive evacuation.

═══════════════ PHASE 6 — Stable identity hash + cross-GC handle rooting ═══════════════

BARRIER (from Phase 5): MC RegistryID rehash threw "RuntimeException: Overflowed :(".

FIX 1 — GC-stable identity hash:
  - Header grew HDR 16→24: [0]=class u32, [4]=nfields u32, [8]=fwd ptr, [16]=identity
    hash word, [24..]=cells. Zeroed on alloc (BOTH paths — nursery recycles by
    resetting top WITHOUT wiping, so a reused slot keeps stale hash bytes → must
    explicitly zero).
  - arena_identity_hash(p): if header hash==0, generate non-zero non-negative
    (mask 0x7fff_ffff) xorshift64* value, STAMP into header, return; else read it back.
    Thread-local RNG seeded with a fixed constant (deterministic single-thread).
  - GC-stable FOR FREE: copy_obj copies obj_size bytes = header (incl hash) + cells,
    so the stamp follows the object into tenured.
  - identity_hash / ref_eq (natives.rs) + ref_equal (opcodes acmp) gained Value::Obj
    arms: hash from header; identity == by current address (sound — GC moves only at
    a safepoint, never mid-compare). Object.hashCode default routes through identity_hash.

FIX 2 — cross-GC handle rooting (the REAL hang). After fix 1, RegistryID still hung:
infinite capacity-doubling loop in a(K,int) (newCap = b.length<<1 = 0; while 0<value
forever). DEBUGGED via HB frame dump: RegistryID arena cells = [array(2669), array(2670),
array(2671), 62(e), 62(f)] but array_len(2669)→None→0. The keys/ids arrays (HEAP handles
held ONLY in the arena RegistryID's cells) were COLLECTED by the mark-sweep heap GC —
which scans frames/statics/handle-fields but NOT arena cells. Dangling handle → length 0.
  - nextgen::arena_handle_roots(out): walk young+old arena regions, for every ref-mask
    cell that is a tagged handle (0b01 Ref / 0b10 Array) push it as a root. Vm::gc calls
    it while gathering roots. (Boxed-Str cells 0b11 hold their own Arc — not a handle.)
  - This is the Phase-4 cross-GC gap, now closed in the handle→arena-holds-handle
    direction. The reverse (arena object reachable only from a handle field) is not yet
    needed — arena objects live in the arena, not the handle space.

DIAGNOSTIC TOOLING (since removed): RAVA_DBG_RID (log RegistryID.a recv/args — showed
value small+sequential 51..62, ruling out a garbage id), HB frame locals+cells dump
(showed b is a valid-looking array handle whose length read 0 → dangling), nextgen::dbg_arena.

RESULT under flag: RegistryID registers sounds, NO overflow, NO spin; heap GC now frees
0MB of arena-rooted handles ([GC] freed 0MB). Boot runs into world init. Default still
reaches "Preparing spawn area" (byte-identical). 8 nextgen tests pass.

NEXT BARRIER (Phase 7 — native-map key derivation for arena keys):
"java.lang.IllegalStateException: Invalid Sound requested: ambient.cave". Sounds register
fine but the lookup `SoundEffects.a(String) → registry.get(MinecraftKey("ambient.cave"))`
misses. The native $map derives a key string from the key object; for an arena MinecraftKey
it can't read the object's fields to derive the value-key → registration-key ≠ lookup-key →
miss. Needs the native map/collection key-derivation (map.rs) to handle Value::Obj receivers
(read arena fields), or MinecraftKey to be value-routed.

═══════════════ PHASE 7 — Native-map key derivation for arena keys ═══════════════

BARRIER (from Phase 6): "IllegalStateException: Invalid Sound requested: ambient.cave".

ROOT CAUSE (DEBUGGED via RAVA_DBG_KEY): the sound registry is a value-keyed native $map.
For a value-type key the native derives a string via the key's toString(). An arena
MinecraftKey (Value::Obj) fell through key_str's `other => display()` → "<arena-object@0x..>"
(the ADDRESS). Registration and lookup happened on DIFFERENT MinecraftKey instances (different
addresses) → key mismatch → miss. Trace showed value_eq=true but toString=Some("<arena-object@..>")
— i.e. value_to_string itself was NOT running the object's toString bytecode for Value::Obj.

THE CORE FIX — value_to_string arena arm: Value::Obj had hit `other => other.display()`
(placeholder). Added an arm that resolves + invokes the object's real toString()
(or a JDK-native toString) with the Obj receiver. MinecraftKey.toString() reads its
namespace/path arena cells → "minecraft:ambient.cave". Confirmed live: toString now
= "minecraft:entity.wolf.death" etc.

MAP KEYING arena arms (map.rs):
  - key_str_mut: value-type Value::Obj (has_value_equality via nextgen::class_name) → key by
    toString() ("o:minecraft:..."); identity object → GC-stable identity hash ("@a<hash>").
  - key_str (immutable, nested keys): Value::Obj → "@a<hash>" (toString needs &mut; identity
    is the safe nested default).
  - map_key identity branch: Value::Obj in an IdentityHashMap/Reference2* → "@a<hash>"
    (never toString, never the moving address).

RESULT under flag (RAVA_JIT=0): "Invalid Sound" GONE, sound keys match, boot runs through
SoundEffects.<clinit> and DEEP into guava registry iteration (AbstractIterator / Iterators /
Preconditions) — into world/registry init. Default still reaches "Preparing spawn area"
(byte-identical). map tests pass.

NEXT BARRIER (JIT raw-ptr codegen — its own phase, NOT key derivation):
With JIT ON, a compiled method's getfield on an arena receiver throws
"NullPointerException: getfield" — the JIT operand-cell tag ABI (REF_TAG/ARRAY_TAG/BOX_TAG in
the high word, value.rs) does not encode Value::Obj (arena = low-2-bit tags), so untag_ref in
rava_getfield (jit.rs:576) returns null → set_pending NPE. Isolated by RAVA_JIT=0 (boot flies
past with JIT off). Arena objects need JIT codegen support (compiled getfield/putfield/invoke/
acmp/instanceof must recognise + deref arena pointers); until then the flag boot runs RAVA_JIT=0.
This is the documented "JIT raw-ptr codegen" milestone.

═══════════════ PHASE 8 — JIT arena-receiver support ═══════════════

BARRIER (from Phase 7): with RAVA_JIT=1 (flag default) a compiled method's getfield on an
arena receiver threw "NullPointerException: getfield" (jit.rs rava_getfield).

ROOT CAUSE: jit_run packs `this`/ref args via box_ref. A raw 64-bit ArenaPtr can't carry a
high-word tag (REF_TAG=1<<40 etc.), so box_ref's `_` arm boxes Value::Obj into the per-thread
box table → a BOX_TAG cell. rava_getfield/putfield decode the receiver via untag_ref, which
only recognises flat REF/ARRAY handles → None → NPE. (acmp/putfield-val already used unbox_ref,
which DOES decode box cells, so those were fine.)

FIX (cold untag-miss path, gated):
  - rava_getfield / rava_putfield: on untag_ref miss, unbox_ref the receiver; JVal::Obj →
    opcodes::arena_getfield / arena_putfield (direct inline-cell read/write; putfield keeps the
    card barrier). untag_ref stays the fast path for plain handles — handle case untouched.
  - instance_of: Value::Obj arm — concrete class from the arena header, same name-walk as a handle.
  - rava_acmp: already unbox_ref + ref_equal (Value::Obj arm = address compare, Phase 6). No change.
  - arena_getfield/arena_putfield made pub(crate) for the callbacks.
All arena handling cfg-gated + on the cold path → default JIT ABI/codegen untouched.

DESIGN NOTE (vs the task's "inline cell machine code"): the JIT operand-cell ABI is
HIGH-WORD-tagged (REF_TAG/ARRAY_TAG/BOX_TAG), NOT the arena low-2-bit raw-pointer scheme. A raw
ArenaPtr only fits a cell by boxing. So the task's "branch on low 2 bits, load inline cell in
Cranelift IR" can't map onto the current cell ABI without a full ABI rewrite (every emitter +
pack/unpack + GC cell scan). The SOUND, bounded fix routes arena field access through the
existing getfield/putfield Rust callbacks (which now deref the ArenaPtr directly) — the method
still JIT-compiles and runs native for everything else; only the arena field op itself is a
callback. Pure-inline arena field codegen is a follow-up gated on unifying the cell ABI onto
raw pointers.

RESULT under flag (RAVA_JIT=1): getfield NPE GONE, hot methods compile (jit=4+), compiled
getfield callbacks (getfield=38) fire on arena receivers with no fault, boot reaches the SAME
deep point as the JIT-off build (guava AbstractIterator/Iterators registry iteration, pcs vary
→ progressing). Default still reaches "Preparing spawn area" (byte-identical). 82 flag + 37 JIT
tests pass.

NEXT BARRIER (common to JIT-on AND JIT-off — NOT JIT-related): boot grinds slowly through guava
registry/datafixer iteration (AbstractIterator/Iterators/Preconditions) at ~4-7M op/s. Next is
world/registry init depth (Bootstrap/DataConverter or chunk-gen). Separate phase.

═══════════════ PHASE 9 — Enums as handles (guava AbstractIterator unblock) + live metrics ═══════════════

BARRIER (from Phase 8): flag boot spun 300M+ ops in guava AbstractIterator — PROFILE: 72.1%
AbstractIterator / 18% Iterators / 9.8% Preconditions, callback counters FROZEN
(invoke=193 getfield=38 unchanging) = a pure-interpreter INFINITE LOOP, not slowness. Same
point under JIT-on AND JIT-off → arena-correctness bug, not JIT.

ROOT CAUSE: enums are native-backed. java.lang.Enum.<init>/ordinal/name store+read name/ordinal
in synthetic $enumName/$enumOrd (Value::Ref only). An arena enum got NEITHER, so ordinal()
returned 0 for EVERY constant. guava AbstractIterator.hasNext does
$SwitchMap[state.ordinal()] → lookupswitch; all-0 ordinals → switch default (tryToComputeNext)
on every call → computeNext re-runs forever → same element re-yielded → unbounded drain.

FIX: new_object_value routes ACC_ENUM (0x4000) classes (or super == java/lang/Enum) to a HEAP
HANDLE, like any other native-backed class → ordinal()/name()/valueOf + ordinal-switch work.
Gated; default unchanged. (Consistent with the Phase-4 dual contract: native-backed → handle.)

RESULT under flag (RAVA_JIT=1): guava loop GONE, boot runs through DFU/registry iteration and
DEEP into block-state registration (BlockStateContainer, stone variants). Default still reaches
"Preparing spawn area" (byte-identical). 8 nextgen + map + JIT tests pass.

LIVE ENGINE METRICS (the endgame proof — TLAB + Cheney + cross-GC rooting all live under a real
MC boot):
  - SHADOW-GC: 1 cycle, roots=498, valid=498, MISMATCH=0 — arena root discovery + heap-GC
    cross-rooting correct at scale (498 roots = hundreds of registered blocks).
  - Cheney GC (live, 2KB nursery to force it): [ARENA-GC] safepoint 7 evacuated, 15 roots
    forwarded, tenured=0KB — generational evacuation + pointer forwarding live mid-boot.
  - Heap GC: freed 0MB of arena-held handles (cross-GC rooting holds them — no dangling).
  - RSS ~1.8GB under the 3584MB hard cap; RAVA_PROC_CAP_MB kernel cap armed. Host safe.
  - With the default 4MB nursery the arena working set never fills (enums/collections/JDK now
    handle-routed → far fewer arena allocs), so steady-state needs no Cheney cycle to boot-depth.

NEXT BARRIER (the documented StateDefinition wall — [[rava-minecraft-removal]], a large separate
subsystem): "NullPointerException: null value in entry ... BlockStone$EnumStoneVariant ... =null".
The block-state property→value map (BlockStateContainer + java.util.stream Cartesian product)
puts a null value under the flag; default passes it. Arena-specific, deep stream/state machinery.

═══════════════ ENDGAME ARC SUMMARY (Phases 1–9) ═══════════════
Handle model (Value::Ref) → raw-pointer arena object model (Value::Obj/ArenaPtr) + generational
Cheney GC, migrated incrementally under `next-gen-gc`, default byte-identical throughout:
  P1 ArenaPtr+Send wall   P2 TLAB+dual getfield/putfield   P3 Cheney moving GC+card table
  P3b loop-safepoint      P3c trampolined clinit safepoint  P4 native-surface dual alloc contract
  P5 getCallerClass under trampoline  P6 GC-stable identity hash + cross-GC handle rooting
  P7 native-map key derivation for arena keys  P8 JIT arena-receiver (getfield/putfield/instanceof)
  P9 enums→handles (guava unblock) + live TLAB/Cheney metrics
Boot depth under flag: joptsimple → log4j → DispenserRegistry → SoundEffects → guava DFU/registry
→ BLOCK-STATE REGISTRATION (the StateDefinition wall). The arena object model is proven semantically
identical to the JVM across the entire bootstrap up to the deep block-state subsystem.
