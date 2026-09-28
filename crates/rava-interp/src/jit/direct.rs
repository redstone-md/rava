//! The direct-call cache and the small predicates that decide whether a site may take a shortcut.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use super::*;

/// GAP-S09 stage 4: outcome of the READ-ONLY per-site direct-call cache probe
/// (`Vm::jit_direct_probe`), shared by the serial (`&mut`, resolves on Miss)
/// and worker (`&self`, never resolves) arms of the direct compiled→compiled
/// call. `Undecodable`/`Mega` are terminal for both arms (general path).
pub(crate) enum DirectProbe {
    /// Cached entry whose guard matches this receiver — invoke directly.
    /// `recv_cell` is the receiver cell the probe ALREADY read-barriered to
    /// decode the guard. Nothing between a hit and the invoke can collect, so
    /// the invoke half reuses it instead of re-running `forward_if_moved` on the
    /// same cell (the RIP tables name that barrier at 2.2% of the ring, 66% of
    /// it under `jit_direct_invoke`). `None` = barrier it yourself: the MISS arm
    /// resolves first, and a resolve loads classes and can move the receiver.
    Hit { entry: DirectEntry, recv_cell: Option<i64> },
    /// Receiver shape undecodable / malformed site → general path (no resolve).
    Undecodable,
    /// Site at JIT_PIC_CAP (megamorphic) → general path (no resolve).
    Mega,
    /// No matching entry; `recv_guard` is the key a serial resolve caches under.
    Miss { recv_guard: usize },
}

/// One direct compiled→compiled call-site cache entry (`Vm::jit_direct_cache`).
#[derive(Clone, Copy)]
pub(crate) struct DirectEntry {
    /// Callee native entry point.
    pub fptr: usize,
    /// Callee return kind (None = void).
    pub ret: Option<JT>,
    /// Callee cell-buffer length (max_locals + max_stack).
    pub buf_len: u16,
    /// Wide (long/double) param positions by RAW arg index — each gets a pad slot.
    pub wide_mask: u64,
    /// Ref-typed return (needs box re-homing).
    pub ret_ref: bool,
    /// Receiver guard: interned class-name ptr (heap Ref), arena class-id+1,
    /// 0 (static/special), or — for `shift` lambda entries — the lambda's
    /// identity cell bits (all lambdas share the class name "$Lambda").
    pub guard: usize,
    /// Lambda→virtual targets only: the class guard the target was resolved
    /// against, re-checked on raw[1] every call. 0 = no extra check.
    pub arg0_guard: usize,
    /// Drop raw[0] (the lambda receiver): raw[1..] are the target's locals
    /// (an unbound instance ref's arg0 IS the receiver; a static ref maps 1:1).
    pub shift: bool,
    /// Lambda entries: content fingerprint (`Heap::lambda_fp`) re-checked per
    /// call — a swept+reused handle would identity-match the guard otherwise.
    pub lam_fp: u64,
    /// IMPL-IDENTITY entries (RAVA_LAMBDA_IMPL_KEY, 0 when unused): the pinned
    /// `Arc<LambdaSite>` key this entry was resolved for. Replaces `lam_fp` as
    /// BOTH the site-cache key and the per-call re-check — a true identity
    /// instead of a 64-bit hash, and one load instead of hashing three strings.
    pub site_key: u64,
    /// Deep-IC v3: the callee body self-registers its frame (prologue push /
    /// epilogue pop) — the wrapper skips its redundant registration.
    pub self_reg: bool,
    /// The callee body is GC-LEAF (`is_gc_leaf`) AND its locals fit the dispatch
    /// call buffer. A leaf body contains no safepoint at all, so nothing can
    /// collect while it runs: neither its own frame nor the caller's live refs
    /// need rooting. That is what lets a MACHINE-CODE hit arm — which has no
    /// Rust wrapper to `cells_push` for it — serve the entry with a plain
    /// `call_indirect` (the contract `heappic_fill`'s class-word leaf entries
    /// have shipped on since 23.4b). `!self_reg` alone does NOT imply this: a
    /// non-leaf body compiled without the v3 prologue is also `!self_reg`.
    pub leaf: bool,
    /// Capturing-lambda direct-call (gated): number of captured values to
    /// PREPEND as the target's leading locals (0 = not a capturing lambda). The
    /// captured values are re-fetched from the live lambda each call (they're
    /// heap refs that move under GC — a cached cell would go stale).
    pub cap_n: u16,
    /// One bit per captured slot (LSB = captured[0]): set = REFERENCE type
    /// (`box_ref`), clear = primitive (`pack_local`). Decided by the impl
    /// descriptor's declared type, not the Value variant.
    pub cap_ref_mask: u64,
}

/// Inline `BitSet.get(I)Z`: read the packed `$bits` word straight from the heap
/// (no `rava_bitset_get` FFI) on the exact-class fast path. DEFAULT ON; opt-out
/// `RAVA_NO_BITSET_INLINE` restores the FFI-only routing (an A/B + safety hatch).
#[cfg(feature = "next-gen-gc")]
/// Inline `ArrayList.get(I)` (`RAVA_ALIST_GET_INLINE`, default OFF until A/B'd; opt-out once on
/// is `RAVA_NO_ALIST_GET_INLINE`). Reads the element out of `$elems` in emitted IR instead of
/// leaving machine code for `fu_fastpath` — which serves the same call today at 187 ns per
/// element (`bench/OpCost2.java`) purely because reaching it is a host call.
#[cfg(feature = "next-gen-gc")]
/// ⛔**`List.size()` INLINE — BUILT, FIRED, AND MEASURED WRONG-SHAPED. OPT-IN
/// (`RAVA_ALIST_SIZE_INLINE=1`).**
///
/// The population justified it: `size` is 4,439,370 of the 38.4 M `fu_fastpath` calls a boot at
/// ~65 ns each, the largest single collection operation, and a JVM answers it with a field load.
/// The arm reads the backing array's length behind the same guard chain `get(I)` uses, and it
/// does emit — **175 sites on a product boot**, 4 gates and 21 probes green.
///
/// Then the counters said what the population count could not: with the arm on, `size` calls
/// REACHING the native rose **4.41 M → 6.97 M (+58%)** while total `fu_fastpath` traffic rose
/// only 8%. That disproportion is the guard MISSING: `size()` in Minecraft is overwhelmingly
/// called on fastutil and guava collections, not on this engine's `java/util/ArrayList`, so the
/// class-word pin is the wrong pin for this population — and a miss is worse than before,
/// because the slow arm funnels through `fu_invoke` where some sites previously took the PIC.
///
/// ⭐The census that chose this target counted calls by METHOD NAME. It could not see that the
/// same name arrives on a dozen unrelated receiver classes. **A population is not a target until
/// its RECEIVERS are counted too** — which is exactly what the guarded-devirtualisation profile
/// does, and why that mechanism is the general answer where this one is a special case.
/// ⛔**`DoubleArrayList.getDouble(I)D` inline — RIGHT TARGET, WRONG FIELD. OPT-IN
/// (`RAVA_DALIST_GET_INLINE=1`).**
///
/// The target is the best-chosen one in this tree: the RECEIVER census puts
/// `DoubleArrayList#getDouble` at **10,528,512 calls a boot**, the largest (class, method) pair
/// in the engine, on ONE concrete class, each a primitive read behind a ~65-70 ns host call. The
/// arm emits — **31 sites** on a product boot, 4 gates and 21 probes green.
///
/// And the guard never hits: the census still reports 10.2 M `getDouble` reaching the native with
/// the arm on. The reason is one field name. `fu_fastpath` serves a fastutil double list "off the
/// backing `double[]` (**`a`**)" — fastutil's own field — while this arm reads `$elems`, which is
/// the synthetic name the engine's OWN collections use. Same guard chain, same class word,
/// different field, so every receiver takes the slow arm.
///
/// ⚠**And the fix is not just the field id.** The same source says "an over-allocated
/// `DoubleArrayList`'s logical length is its `size` field", so the bounds check must be against
/// that field and not against the array's length — otherwise an index between the logical size
/// and the capacity returns a stale slot instead of throwing. **No bit-exact gate would catch
/// that**: it needs a workload that reads out of logical range, and the gates never do.
///
/// ⛔**AND THE FIELD WAS NOT THE PROBLEM EITHER — THE GUARD SIMPLY NEVER MATCHES.** `arr_of` in
/// `fastutil.rs` reads `Syn::Elems`, so `$elems` was right all along and the `a` in the comment
/// above is fastutil's own source field, not this engine's storage. Per-arm counters emitted into
/// the machine code settle it:
///
/// ```text
/// dalist_get_inline  31          sites emitted
/// dg_fast             0          inline arm executed
/// dg_slow    16,859,206          guard missed, callback taken
/// ```
///
/// **Zero hits in 16.9 M executions**, and those 16.9 M are the whole `getDouble` population the
/// receiver census names — so the sites are the right sites and the guard is what fails. The
/// receivers are all heap-Ref (the census tags representation now), so it is the class WORD:
/// published at VM construction by interning a name before the class is loaded, while the object
/// the loader later builds carries a different `Arc<str>`. Publishing it from a live receiver
/// instead did not fix it, which means the publication point sits below where these calls are
/// already served.
///
/// ⭐**A site count cannot show a guard that never fires.** "31 sites emitted" read like progress
/// for two measurements; one counter per arm ended it in one run. Any future speculative arm gets
/// those counters from the start.
///
/// ⛔**AND PUBLISHING FROM A LIVE RECEIVER DID NOT FIX IT EITHER.** Third attempt: the store now
/// sits at the ENTRY of `fu_fastpath`, a line every one of these calls passes, taking the word
/// from `heap.object_class_word(h)` on a receiver whose class name is confirmed to be
/// `DoubleArrayList`. Result unchanged: **`dg_fast=0`, `dg_slow=16,859,429`.**
///
/// ⇒ So the published value and the value the machine code compares are **different
/// quantities**: `object_class_word(handle)` is not what `emit_heap_ref_class_key` reads out of
/// `[box_ptr + Object::class_offset]`. Publishing a "correct" word from Rust cannot work until
/// those two are reconciled, and that is where this resumes — read both for one receiver and
/// print them side by side, rather than assuming the pair that the ArrayList arm assumes.
///
/// ⚠**Which also puts `alist_get_inline` under suspicion**: it is opt-in, it publishes its word
/// the same way, and it has no per-arm counter — so nothing in this tree has ever shown that ITS
/// guard hits either. That is one run with the counters this arm now has.
///
/// ⛔**FIVE FIXES, STILL `dg_fast = 0`.** The publisher was moved to VM construction, then to a
/// live receiver, then to what looked like `fu_fastpath`'s entry — that last one landed in
/// `fu_read_shared`, because `let recv = jit::unbox_ref(..)` occurs TWICE in that file and the
/// first match serves `DoubleImmutableList` only. Moved into `fu_fastpath` proper, the reading is
/// unchanged: `dg_cls = dg_slow = 16,858,993`, `dg_fast = 0`.
///
/// So guard 1 passes on every call and the class compare fails on every call, with a word that is
/// now taken off a live `DoubleArrayList` receiver in the same function that serves it. What has
/// NOT been done, and is the one thing left: **print the baked word and the receiver's word side
/// by side from Rust and compare them.** Every fix so far assumed which of the two was wrong;
/// none of them looked.
///
/// ⭐The instruments this produced are the durable part: per-arm counters (`dg_fast` / `dg_cls` /
/// `dg_slow`) turn "31 sites emitted" into "31 sites emitted and none of them fire", and a
/// receiver census that tags representation. Both are what any future speculative arm starts with.
///
/// ✅**ROOT CAUSE FOUND — AND IT IS NOT THIS ARM ALONE.** A counter on every guard step:
///
/// ```text
/// dg_cls   16,859,301   reached the class compare
/// dg_elem  10,352,630   PASSED it  <- exactly the DoubleArrayList population
/// dg_bnd            0   `$elems` was an array
/// dg_fast           0
/// ```
///
/// The class guard is perfect: 10.35 M receivers pass it and the 6.5 M `CubePointRange` calls are
/// correctly rejected. The published word and the receiver's word were printed side by side and
/// are identical (`0x8021c3f000`). **Guard 3 is what fails, on every one of the 10.35 M.**
///
/// Why: `Heap::decode_field` returns `FK_ARRAY` as `Value::Array(cell as u32)` — the cell holds a
/// BARE HANDLE and the "this is an array" fact lives in a separate KIND byte. The emitted guard
/// tests `elems >> 32 == ARRAY_TAG_HI`, a tag a field-stored array never carries, so it can never
/// be true.
///
/// ⚠**`emit_arraylist_get_inline` tests the same thing the same way** — and it is opt-in, and it
/// has never had a per-arm counter, so nothing in this tree has ever shown that ITS guard hits
/// either. That is now one run away, and it would explain why this whole family stayed opt-in.
///
/// ⛔⛔**AND THE FINAL ANSWER IS THAT THIS APPROACH CANNOT WORK HERE AT ALL.** Printing the
/// receiver's own fields from Rust — the check that should have been first — ends it:
///
/// ```text
/// [DG-CMP] name=getDouble  elems=Null  a=Null  elems_fid=0  a_fid=1079
/// ```
///
/// A live `DoubleArrayList` receiver has **neither `$elems` nor `a`**. Its doubles are not in the
/// object's field table at all — the native keeps them elsewhere — so no machine-code field read
/// can ever find the backing, whatever tag or kind it tests. (And `field_id("$elems")` answers
/// **0** at publication time because the name is not interned yet, so the id baked into the
/// emitted code addressed field number zero.)
///
/// ⇒ Inlining a native's SEMANTICS by reading its backing field is only possible for collections
/// whose backing IS a guest field. For the fastutil families that dominate this population it is
/// not, and the only route to them stays the one already recorded: stop serving those receivers
/// from natives, so their state becomes ordinary object fields and their bytecode becomes
/// inlinable.
///
/// ⭐What the six cycles bought, and it is not nothing: the class guard is PROVEN correct on real
/// traffic (10.35 M `DoubleArrayList` receivers accepted, 6.5 M `CubePointRange` correctly
/// rejected, published word bit-identical to the receiver's), and a per-arm counter chain now
/// exists that turns "the arm is emitted" into "the arm is emitted, reaches step 3 ten million
/// times, and dies there". The same chain should be pointed at `alist_get_inline`, which has
/// never had one.
pub(crate) fn dalist_get_inline_unreachable_note() {}
/// ⭐**DEFAULT ON.** `DoubleArrayList.getDouble` is the single largest native crossing in this
/// engine — 10,364,672 of the 24,516,007 `fu_fastpath` crossings on a datapack run, and with
/// `CubePointRange.getDouble` the two are **58%** of all of them, at p50 1,128 ns each.
///
/// The arm existed for months and had executed ZERO times: it read `$elems` (the native
/// ITERATOR's synthetic field, which a list does not have) and its emission gate could never be
/// satisfied in time. Reading `a` with a `min(size, phys)` bound and resolving the field ids
/// before compiling rather than from the arm's own calls, it serves **6,168,411 of 9,887,620**.
///
/// Same-binary flag A/B, `bench/DataPackJson`, alternating arms, first pair discarded, against a
/// null floor of 0.6% on this instrument:
///
///   OFF min 22.89 med 23.22   ON min 22.14 med 22.56
///   **-3.28% min / -2.83% median, DISJOINT**, ON faster in all four pairs.
///
/// Confirmed on a SECOND workload, so the win is not one probe's shape. `bench/ChunkFill 1 64`,
/// same instrument, flag ON = arm disabled:
///
///   OFF min 25.17 med 25.58   ON min 25.38 med 26.16
///   disabling costs +0.81% min / **+2.26% median** — same sign and magnitude as above.
///
/// ⚠Weaker evidence than the datapack run: these arms OVERLAP, so this one confirms the
/// direction rather than establishing the size.
///
/// `RAVA_NO_DALIST_GET_INLINE=1` disables it; `RAVA_DALIST_GET_INLINE` is kept as an explicit
/// force-on so existing scripts that set it keep meaning what they meant.
/// `RAVA_NO_ENTRY_INLINE=1` disables the `Map$Entry.getKey`/`getValue` inline arm.
pub(crate) fn entry_inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ENTRY_INLINE").is_none())
}

pub(crate) fn dalist_get_inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_DALIST_GET_INLINE").is_none()
            || std::env::var_os("RAVA_DALIST_GET_INLINE").is_some()
    })
}

pub(crate) fn alist_size_inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ALIST_SIZE_INLINE").is_some())
}

pub(crate) fn alist_get_inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ALIST_GET_INLINE").is_some())
}

pub(crate) fn bitset_get_inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_BITSET_INLINE").is_none())
}

// ── `fu_routable`'s own gate word and owner pre-reject ────────────────────────────────────────
//
// ⭐**EIGHT PROCESS CONSTANTS PER CALL, ONE READ (`FUG_*` below).**
//
// Shape before: one `fu_routable(owner, name, desc)` call read `map_intrinsic_enabled` ×3,
// `iter_intrinsic_enabled` ×3, `entry_intrinsic_enabled` ×1 and `list_iter_admitted` ×1 — eight
// `OnceLock` reads, i.e. eight ACQUIRE loads and eight branches. The JIT gate word
// (`jit::gates::hot_gates`) already collapsed the first seven onto ONE static, but N reads of one
// static are still N barriers: an acquire load may not be forwarded across the `bcmp` calls the
// owner cascade emits between two gate sites, so the compiler cannot fold them even though every
// answer is fixed before the VM exists.
//
// Shape after: ONE load at the natural entry, bit-tested at each of the eight sites.
//
// ⚠The FOLD has no kill switch of its own and does not need one: each bit is the SAME accessor,
// evaluated once per process instead of once per site, and `jit::gates` documents (and proves)
// that none of those bits' positions is load-bearing — they are pure `env::var_os` memos, none
// arms a thread and none decides whether a later side effect fires. A verdict therefore cannot
// change. What CAN change a verdict is the pre-reject below, and that is what the kill switch
// covers.
#[cfg(feature = "next-gen-gc")]
const FUG_MAP: u8 = 1 << 0;
#[cfg(feature = "next-gen-gc")]
const FUG_ITER: u8 = 1 << 1;
#[cfg(feature = "next-gen-gc")]
const FUG_ENTRY: u8 = 1 << 2;
#[cfg(feature = "next-gen-gc")]
const FUG_LIST_ITER: u8 = 1 << 3;
/// The owner pre-reject is armed. `RAVA_NO_FAST_DIRECTGATES=1` (and the blanket
/// `RAVA_NO_EMIT_MORE`) clears it, restoring the full cascade walk for every owner.
#[cfg(feature = "next-gen-gc")]
const FUG_FAST: u8 = 1 << 4;

#[cfg(feature = "next-gen-gc")]
#[inline]
fn fu_gates() -> u8 {
    static W: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let mut w = 0u8;
        if crate::jit::map_intrinsic_enabled() {
            w |= FUG_MAP;
        }
        if crate::jit::iter_intrinsic_enabled() {
            w |= FUG_ITER;
        }
        if crate::jit::entry_intrinsic_enabled() {
            w |= FUG_ENTRY;
        }
        if list_iter_admitted() {
            w |= FUG_LIST_ITER;
        }
        if std::env::var_os("RAVA_NO_FAST_DIRECTGATES").is_none()
            && !crate::flags::emit_more_disabled()
        {
            w |= FUG_FAST;
        }
        w
    })
}

/// ⭐**THE CASCADE BELOW CAN ADMIT ONLY THREE OWNER PREFIXES — SO TEST THOSE FIRST.**
///
/// Shape before: every `fu_routable` call walked the entire cascade — a ~20-literal `matches!`
/// over `(owner, name, desc)`, then six further owner groups and two `starts_with`es, on the
/// order of forty `bcmp`s, interleaved with the eight gate reads above. Shape after: one byte
/// load, and at most one `starts_with`, for an owner no arm can accept.
///
/// Population: every invoke site the compiler looks at, and it is asked more than once per site
/// — `emit::ops::invoke_{virtual,static,dynamic}`, `runtime::ic::wide_site_pic_ok`,
/// `runtime::gcroots::promo_virt_preempted`, `analysis::trivial`, `vm::jit_plans`. On a
/// Minecraft boot the overwhelming majority of those sites declare `net/minecraft/…`,
/// `com/mojang/…`, `org/…` or `net/fabricmc/…` owners, none of which any arm below can admit;
/// each is now rejected on its FIRST CHARACTER.
///
/// ⚠EXHAUSTIVE BY CONSTRUCTION, and this enumeration is the proof obligation for any arm added
/// below. The admitting arms name: `java/lang/{Boolean,Integer,Long,Short,Byte,Character,Class,
/// System,String}`; `java/util/{Objects,Optional,Map,HashMap,LinkedHashMap,IdentityHashMap,
/// AbstractMap,SortedMap,TreeMap,Iterator,Map$Entry,AbstractMap$SimpleEntry,
/// AbstractMap$SimpleImmutableEntry,List,Collection,AbstractList,ArrayList}`;
/// `com/google/common/collect/ImmutableMap`; and every owner under
/// `it/unimi/dsi/fastutil/doubles/` or `it/unimi/dsi/fastutil/ints/`. Each is covered by exactly
/// one arm here. **Add an owner below without adding it here and it silently stops routing** —
/// which is why the kill switch exists and why the three prefixes are stated, not inferred.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn owner_may_route(owner: &str) -> bool {
    let b = owner.as_bytes();
    match b.first() {
        Some(b'j') => b.starts_with(b"java/"),
        Some(b'i') => b.starts_with(b"it/unimi/dsi/fastutil/"),
        Some(b'c') => owner == "com/google/common/collect/ImmutableMap",
        _ => false,
    }
}

/// Compile-time predicate (next-gen-gc): is this call-site member one the fastutil
/// scalar fast path (`jit_fu_invoke`) can serve? Gated by the fastutil ints package
/// + the method name/descriptor; the receiver's concrete class is checked at runtime.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn fu_routable(owner: &str, name: &str, desc: &str) -> bool {
    // ONE acquire load for the eight process constants this cascade used to read one at a time,
    // taken at the natural entry and bit-tested below. See the block comment above.
    let g = fu_gates();
    // Owner pre-reject — the whole rest of this function is unreachable for an owner outside the
    // three admissible prefixes. Kill switch: `RAVA_NO_FAST_DIRECTGATES=1`.
    if g & FUG_FAST != 0 && !owner_may_route(owner) {
        return false;
    }
    // RAVA_ITER_INTRINSIC (gated): serve generic java/util/Iterator advance off the
    // native $elems/$pos backing (fu_fastpath FuKind::Iter). A NON-native iterator
    // (real ArrayList$Itr / HashMap iterator) has a different class → fu_fastpath
    // returns None → general path, so this is safe by construction. Highest-count
    // uncovered native dispatch in MC boot (~5M Iterator#next/hasNext).
    // ⭐**THE BOX UNWRAPPERS AND `Class.cast`, ONE LEVEL DOWN.** Both already have early arms in
    // the natives cascade, and those took `Boolean.booleanValue` from 1,155 to 270 ns and
    // `Class.cast` from 1,050 to 700. But an arm reached through `jit_call_body` still builds a
    // synthetic frame; `IntPredicate.test`, which `fu_fastpath` serves from machine code, costs
    // **110 ns**. The census puts `Class.cast` at 6.75% and `booleanValue` at 4.23% of all
    // compiled-to-host crossings, so the difference between 700 and 110 is seconds.
    // `fu_fastpath` re-checks the receiver and returns None for anything unexpected, so
    // admitting the SITE cannot change what runs.
    if matches!(
        (owner, name, desc),
        ("java/lang/Boolean", "booleanValue", "()Z")
            | ("java/lang/Integer", "intValue", "()I")
            | ("java/lang/Long", "longValue", "()J")
            | ("java/lang/Short", "shortValue", "()S")
            | ("java/lang/Byte", "byteValue", "()B")
            | ("java/lang/Character", "charValue", "()C")
            | ("java/lang/Class", "cast", "(Ljava/lang/Object;)Ljava/lang/Object;")
            // STATIC: `cells[0]` is the first ARGUMENT, not a receiver. The arm reads the cells
            // directly and serves only the cases it can decide without deriving anything —
            // nulls, same object, and classes that PROVE identity equality — returning None for
            // the rest so the ordinary native arm answers exactly as before.
            | ("java/util/Objects", "equals", "(Ljava/lang/Object;Ljava/lang/Object;)Z")
            | ("java/lang/System", "identityHashCode", "(Ljava/lang/Object;)I")
            // More statics, now that the `invokestatic` emitter asks: `DoubleArrayList.wrap` is
            // 3.55% of the boot's crossings and `Optional.of`/`ofNullable`/`empty` 2.08%, both
            // still paying the general path to reach an arm that already exists.
            | ("it/unimi/dsi/fastutil/doubles/DoubleArrayList", "wrap", _)
            | ("java/util/Optional", "of", _)
            | ("java/util/Optional", "ofNullable", _)
            | ("java/util/Optional", "empty", _)
            // `String.valueOf` of a PRIMITIVE only. 3.26% of the boot's crossings, and the
            // formatting is pure. ⚠The `(Ljava/lang/Object;)` overload is deliberately absent:
            // it re-enters the guest's `toString`, which is not leaf work and has no GC root in
            // the raw call buffer.
            | ("java/lang/String", "valueOf", "(I)Ljava/lang/String;")
            | ("java/lang/String", "valueOf", "(J)Ljava/lang/String;")
            | ("java/lang/String", "valueOf", "(Z)Ljava/lang/String;")
            | ("java/lang/String", "valueOf", "(C)Ljava/lang/String;")
    ) {
        return true;
    }
    // `Optional` READERS — ~9.5% of the seam between them, one field read each, and the
    // WRITERS next door (`of`/`ofNullable`/`empty`) were already admitted. `fu_fastpath` tests
    // the receiver for `$present` and falls through otherwise.
    if g & FUG_MAP != 0
        && owner == "java/util/Optional"
        && matches!(
            (name, desc),
            ("get", "()Ljava/lang/Object;")
                | ("orElseThrow", "()Ljava/lang/Object;")
                | ("isPresent", "()Z")
                | ("isEmpty", "()Z")
                | ("orElse", "(Ljava/lang/Object;)Ljava/lang/Object;")
        )
    {
        return true;
    }
    // MAP READS. `java/util/Map.get` alone is 6.92% of the boot's compiled-to-host crossings —
    // the second-largest callee in the whole census — and Minecraft declares the INTERFACE at
    // essentially every site, so the concrete-class admissions below never see it. `fu_fastpath`
    // re-checks that the receiver carries a native `$map` and returns `None` otherwise, so
    // admitting the site cannot change which implementation answers.
    if g & FUG_MAP != 0
        && crate::map::fu_map_op(name, desc)
        && (matches!(
            owner,
            "java/util/Map"
                | "java/util/HashMap"
                | "java/util/LinkedHashMap"
                | "java/util/IdentityHashMap"
                | "java/util/AbstractMap"
                | "java/util/SortedMap"
                | "java/util/TreeMap"
                | "com/google/common/collect/ImmutableMap"
        ))
    {
        return true;
    }
    if g & FUG_ITER != 0 && owner == "java/util/Iterator" {
        return matches!((name, desc), ("hasNext", "()Z") | ("next", "()Ljava/lang/Object;"));
    }
    // java.util.Map$Entry — `getKey`/`getValue`, #3 and #4 of the never-cacheable census at
    // 5,082,022 dispatch misses a boot. The receiver's concrete class is re-checked in
    // `fu_fastpath`, which falls back to general dispatch for anything else (including an
    // entry whose field is absent), so admitting the SITE here cannot change what runs.
    // Gated: `RAVA_NO_ENTRY_INTRINSIC` turns it back off without a rebuild.
    if g & FUG_ENTRY != 0
        && matches!(
            owner,
            "java/util/Map$Entry"
                | "java/util/AbstractMap$SimpleEntry"
                | "java/util/AbstractMap$SimpleImmutableEntry"
        )
    {
        return matches!(
            (name, desc),
            ("getKey", "()Ljava/lang/Object;") | ("getValue", "()Ljava/lang/Object;")
        );
    }
    // java.util.ArrayList: serve the hot ops off the native `$elems` backing, skipping
    // the rava_invoke synthetic frame (the receiver's concrete class + $elems presence
    // are re-checked in fu_fastpath, which falls back to the general dispatch on a miss
    // — a subclass, a fresh never-`add`ed list, or an out-of-bounds index).
    // ⭐**THE SAME INTERFACE/CONCRETE ASYMMETRY THE MAP ARM WAS BUILT FOR.** The list admission
    // below names `java/util/ArrayList`, but a call site's owner is its STATIC type and Minecraft
    // declares `List<..>`/`Collection<..>` nearly everywhere — so the concrete admission never
    // saw the traffic it was written for. `fu_fastpath` classifies by the RECEIVER's class and
    // refuses anything that is not a native `$elems`-backed list, so widening the site admission
    // cannot change which implementation answers; it only stops the general path being the only
    // way to find out.
    if g & FUG_MAP != 0
        && matches!(owner, "java/util/List" | "java/util/Collection" | "java/util/AbstractList")
    {
        return matches!(
            (name, desc),
            ("size", "()I")
                | ("isEmpty", "()Z")
                | ("get", "(I)Ljava/lang/Object;")
                | ("add", "(Ljava/lang/Object;)Z")
                | ("set", "(ILjava/lang/Object;)Ljava/lang/Object;")
                | ("remove", "(I)Ljava/lang/Object;")
        ) || (g & (FUG_ITER | FUG_LIST_ITER) == (FUG_ITER | FUG_LIST_ITER)
            && (name, desc) == ("iterator", "()Ljava/util/Iterator;"));
        // ⛔**AND `java/util/List.iterator()` IS NOT ADMITTED — THE PROBE AND THE BOOT DISAGREED.**
        // `bridgestat` on `bench/StreamApplyCost.java` says a `List`-typed site never reaches the
        // concrete `ArrayList` admission above, leaving `List#iterator` at 36% of that probe and
        // 1,462 ns a call even with the intrinsic on. Admitting the one extra owner took the
        // probe from 2,337-2,533 to **1,738-1,841 ns per element, another -25%**, and all four
        // gates plus 21 probes stayed green and the boot reached `Done`.
        //
        // The boot then said no. Interleaved, same flag, consecutive rounds against an OFF arm
        // that read 38.2 both times:
        //   narrow only          median wall 36.80
        //   narrow + List owner  median wall 37.50
        // ⭐**A probe reproduces the SHAPE, not the receiver DISTRIBUTION.** Every receiver in the
        // probe is a native `$elems` `ArrayList`, so the widened admission always hits; on the
        // boot a `List`-typed site sees guest lists, sets and views that `fu_fastpath` must
        // refuse, and an over-approximating admission is paid by every later call of every site
        // it touches. Reverted.
    }
    if owner == "java/util/ArrayList" {
        return matches!(
            (name, desc),
            ("size", "()I")
                | ("isEmpty", "()Z")
                | ("get", "(I)Ljava/lang/Object;")
                | ("add", "(Ljava/lang/Object;)Z")
                | ("set", "(ILjava/lang/Object;)Ljava/lang/Object;")
                | ("remove", "(I)Ljava/lang/Object;")
        ) || (g & FUG_ITER != 0 && (name, desc) == ("iterator", "()Ljava/util/Iterator;"));
    }
    // fastutil double lists (`DoubleImmutableList`/`DoubleArrayList`) — the VoxelShape
    // coordinate backing. Served off the native `double[] a` by fu_fastpath (concrete
    // class + backing re-checked at runtime, OOB falls back to the real method).
    if owner.starts_with("it/unimi/dsi/fastutil/doubles/") {
        return match name {
            "getDouble" => desc == "(I)D",
            "size" => desc == "()I",
            "isEmpty" => desc == "()Z",
            "iterator" | "listIterator" => desc.starts_with("()L"),
            _ => false,
        };
    }
    if !owner.starts_with("it/unimi/dsi/fastutil/ints/") {
        return false;
    }
    match name {
        "getInt" => desc == "(I)I",
        "size" => desc == "()I",
        "isEmpty" => desc == "()Z",
        "iterator" | "listIterator" => desc.starts_with("()L"),
        // Iterator advance on a fastutil `IntIterator`/`IntListIterator` static type —
        // its concrete object is our native `$elems` iterator (verified at runtime).
        "hasNext" => desc == "()Z",
        "nextInt" => desc == "()I",
        "nextLong" => desc == "()J",
        "next" => desc == "()Ljava/lang/Object;",
        _ => false,
    }
}

/// GAP-S09 stage 3 — is this `ldc` constant a Class literal whose Class object is
/// NOT yet interned? Serving it would publish into the FROZEN `class_objects` map
/// (`make_class` insert — the one shared-mutating arm of `jit_ldc`), so a gil-free
/// caller must abort BEFORE. Everything else stays served: primitive/String
/// constants and a WARM Class literal mutate nothing shared (an arena string goes
/// to THIS worker's TLAB view; boxing uses the thread-local box table). Read-only
/// peek: the caller's constant pool + one lookup in the region-frozen map.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn ldc_cold_class_literal(vm: &crate::Vm, caller_ptr: i64, cp: i64) -> bool {
    let Some(caller) = (unsafe { (caller_ptr as *const crate::class_loader::LoadedClass).as_ref() })
    else {
        return true; // undecodable caller — never guess, abort
    };
    match caller.file.pool.get(cp as u16) {
        Some(Constant::Class { name_index }) => match caller.file.pool.utf8(*name_index) {
            Some(name) => !vm.class_objects.contains_key(name),
            None => true,
        },
        Some(_) => false,
        None => true,
    }
}

/// Is `recv` a raw arena map pointer? (low address, an `ArenaMap` object).
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn amap_recv(recv: i64) -> Option<crate::arena::ArenaPtr> {
    let raw = recv as u64;
    if raw != 0 && raw >> 40 == 0 {
        let p = unsafe { crate::arena::ArenaPtr::from_addr(raw) };
        if crate::arena_map::is_arena_map(p) {
            return Some(p);
        }
    }
    None
}

/// Whether an inline reference store needs the diagnostic card-mark callback.
///
/// The production minor collector still scans the complete tenured arena
/// (`nextgen::minor_collect`); its shadow card table is deliberately a dry-run
/// verifier. Consequently an unconditional JIT→Rust transition after every
/// reference store is pure overhead unless one of the write diagnostics is armed.
/// Environment switches are immutable for a VM run and each probe uses a
/// `OnceLock`, so deciding this while compiling a method preserves their semantics.
///
/// ⭐**FOUR PROCESS CONSTANTS READ PER EMITTED REFERENCE STORE, TO ANSWER ONE QUESTION.**
/// Shape before: a four-term `||` over four separate `OnceLock`s — `m3shape::enabled`
/// (`RAVA_M3_SHAPE_PROBE`), `m3biome::enabled` (`RAVA_M3_BIOME_SHAPE_PROBE`),
/// `nextgen::shadow_enabled` (`RAVA_SHADOW_GC`) and `stale_store_trace`
/// (`RAVA_TRACE_STALESTORE`) — i.e. up to four ACQUIRE loads, four branches and four distinct
/// cache lines, evaluated afresh at every `emit`-time reference store
/// (`emit::fields_ref`, `emit::fields_arena`). An acquire load is an optimisation barrier, so
/// none of the four could be forwarded across the others.
/// Shape after: ONE `OnceLock<bool>`; the disjunction is computed once per process.
/// Population: every emitted `putfield`/array-store of a reference in every compiled method —
/// thousands of sites a boot, and all four terms are diagnostics that are off in every run that
/// is not being debugged.
/// ⚠Sound for exactly the reason the doc above already relies on ("environment switches are
/// immutable for a VM run and each probe uses a `OnceLock`"): all four terms are env memos, so
/// their disjunction is a process constant too. Nothing here arms a thread or has a side effect,
/// so latching it at the first ask cannot move one.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn card_mark_callback_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        crate::m3shape::enabled()
            || crate::m3biome::enabled()
            || crate::nextgen::shadow_enabled()
            || stale_store_trace()
    })
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn analyze_escape(bc: &[u8], pool: &ConstantPool) -> Vec<(usize, EscapeReason)> {
    let mut sites: Vec<usize> = Vec::new();
    let mut has_cf = false;
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        if op == 0xbb {
            sites.push(pc);
        }
        // Any conditional/uncond branch, jsr, switch, athrow → not straight-line.
        if matches!(op, 0x99..=0xa8 | 0xaa | 0xab | 0xbf | 0xc6 | 0xc7) {
            has_cf = true;
        }
        let Some(len) = insn_len_at(bc, pc) else {
            // Undecodable opcode → conservatively a sink for every site.
            return sites.into_iter().map(|p| (p, EscapeReason::Sink)).collect();
        };
        pc += len;
    }
    if has_cf {
        // Straight-line walk can't track merges → conservatively escaped, but flagged
        // as CF (the CF-aware detector's target) rather than a genuine sink.
        return sites.into_iter().map(|p| (p, EscapeReason::ControlFlow)).collect();
    }
    sites
        .iter()
        .map(|&new_pc| {
            let reason = if object_escapes_straightline(bc, pool, new_pc) {
                EscapeReason::Sink
            } else {
                EscapeReason::Confined
            };
            (new_pc, reason)
        })
        .collect()
}

/// `RAVA_NO_LOOP_BCE` — the only flag in this file that re-read the environment on every ask.
/// `licm` consults it once per loop it analyses, so it was O(env entries) per loop compiled; every
/// sibling predicate here is a `OnceLock`, and an env var is immutable for a VM run. was O(env
/// entries) per analysed loop, is O(1) per process.
pub(crate) fn loop_bce_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_LOOP_BCE").is_none())
}

/// 15.2 RavaPool candidate scanner (RAVA_POOL_SCAN): a pure diagnostic that names every hot method
/// which FULLY satisfies the data-parallel MAP analyzer (counted `out[i]=f(a[i])` loop, callback-free
/// = zero invoke except Math.max/min, zero new). These are the reachable next targets for the pool —
/// the same bar the now-removed SimpleBitStorage.unpack experiment cleared. No codegen change, no fork ⇒ math is inert.
/// 16.1 leaf-inliner observability (RAVA_JIT_INLINER): the JIT already inlines side-effect-free
/// leaf calls (invokestatic/invokespecial/final-virtual, ≤96B, no wide params, no invokes — see
/// is_inlinable + collect_inline_targets) via emit_inline with local-slot remapping. This gate just
/// LOGS each distinct (callee→caller) collapse so the mechanism is observable + its real boot
/// coverage measurable. Pure diagnostic; the inliner runs regardless.
#[cfg(feature = "next-gen-gc")]
/// ⚠**INTERFACE SITES ARE A SEPARATE QUESTION FROM VIRTUAL ONES** (`RAVA_DEVIRT_IFACE=1`,
/// default OFF). Guarded devirtualisation shipped on `invokevirtual` at a measured -2.54% CPU
/// with fully disjoint arms. Extending the same emission to `invokeinterface` — 866 admitted
/// sites instead of 297 — took the same A/B to **+0.51% with the arms overlapping**, i.e. the
/// win disappeared. More coverage is not automatically more speed, and the two halves are gated
/// apart until that is understood rather than assumed.
pub(crate) fn devirt_iface_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_DEVIRT_IFACE").is_some())
}

/// **DEOPTIMISATION** (`RAVA_DEOPT=1`; `RAVA_DEOPT_FORCE=1` makes every guard fail, which is how
/// the transition is verified — the bit-exact gates must still pass when every call leaves
/// through the interpreter).
///
/// Off by default until the emitter half is proven. See `docs/deoptimisation-plan.md`.
pub(crate) fn deopt_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_DEOPT").is_some())
}

/// `RAVA_ITER_TRAP=<mask>`: which of the four collection inline arms take the TRAP shape — the
/// guard's failing edge leaves the function through the interpreter instead of calling the native
/// oracle and merging. A BITMASK, not a flag, because the arms are independent and one bad arm
/// turns every gate red without saying which: bit0 `hasNext`, bit1 `next`, bit2 `getDouble`,
/// bit3 `size`. The arms keep the diamond by default.
pub(crate) fn iter_trap_mask() -> u32 {
    static M: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *M.get_or_init(|| {
        std::env::var("RAVA_ITER_TRAP")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(0)
    })
}

/// Recompile a method whose trap fired (`RAVA_NO_DEOPT_INVALIDATE=1` to keep the old code).
pub(crate) fn deopt_invalidate() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_DEOPT_INVALIDATE").is_none())
}

pub(crate) fn deopt_force() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_DEOPT_FORCE").is_some())
}

pub(crate) fn jit_inliner_log_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_INLINER").is_some())
}
#[cfg(feature = "next-gen-gc")]
pub(crate) fn jit_inline_log(caller: &str, callee: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let key = format!("{callee}=>{caller}");
    let mut g = SEEN.get_or_init(|| Mutex::new(HashSet::new())).lock().unwrap_or_else(|e| e.into_inner());
    if g.insert(key) {
        eprintln!("[RAVA-JIT-INLINE] Successfully inlined leaf method: {} into {}", callee, caller);
    }
}

/// TRACER (next-gen-gc): dual-path constant-slot getfield for a PRIMITIVE int
/// field — the first inline arena field access, the marathon's tracer bullet.
/// Branches on the receiver representation at runtime:
///   - ArenaPtr (raw, 8-aligned, no high-word tag) → INLINE native `load.i64` at
///     `[obj + HDR + slot*8]`, ZERO callbacks (the whole point: arena field reads
///     stop boxing into the cold `rava_getfield` path).
///   - heap handle / null → the existing high-word handle path, unchanged (it
///     raises the NPE on a null receiver itself).
///
/// Discriminator: `obj != 0 && (obj u>> 40) == 0` — deliberately NOT `obj & 3`.
/// During ABI coexistence handles keep the high-word scheme (default for every
/// other op), so a handle's low 2 bits are `h & 3` and can be `0b00`; `obj & 3`
/// would misread such a handle as an ArenaPtr and inline-load a garbage address.
/// Every high-word ref cell instead carries a tag in bits 40..43 (`hi >= 256`),
/// and a real arena pointer is `< 2^40` (asserted in `nextgen::Region::new`), so
/// testing the top word is the sound discriminator until Task 1 flips the whole
/// handle ABI to low-2-bit (after which `obj & 3` becomes correct).
/// Opt-out (`RAVA_NO_ARENA_INLINE`): force arena int-field reads back onto the
/// cold `rava_getfield` callback (pre-tracer behaviour) instead of the inline dual
/// path. Lets the inline win be A/B-measured in one binary; also a safety hatch.
/// DEFAULT: inline ON.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_inline_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var("RAVA_NO_ARENA_INLINE").is_ok())
}

/// Inline `String.charAt(I)C`: read the character straight out of an ASCII arena
/// string's inline bytes instead of paying the native-bridge trampoline. DEFAULT
/// ON; opt-out `RAVA_NO_STR_CHARAT` restores the callback-only routing (A/B +
/// safety hatch).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn str_charat_intrinsic_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_STR_CHARAT").is_none())
}

/// `java/util/List.iterator()` admission — the one that lost 0.7 s on the boot before sites
/// could retire. Flagged so the two can be told apart in ONE round of an interleaved A/B.
fn list_iter_admitted() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_LIST_ITER").is_some())
}
