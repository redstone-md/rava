//! Generic `java.util.Map` family (HashMap/LinkedHashMap/ConcurrentHashMap/
//! TreeMap/Hashtable/Properties/…) + Map.Entry. Backed by the heap's map table,
//! addressed by a `$map` id on the owning object. Keyed only on real JDK names.
//!
//! Keys are matched by a derived string (string/number value, or a stable
//! object identity / name), which covers the common String/Class/enum keys.







use crate::value::Value;
use crate::{Vm, VmError};

type Handled = Result<Option<Option<Value>>, VmError>;

/// fastutil's long-keyed object maps (MC's loaded-chunk map = Long2ObjectOpenHashMap,
/// the #1 play hotspot). Backed by the native `$map` (key_str encodes Value::Long →
/// "j:N"). ONLY the long maps: Int2Object* is subclassed by Paper
/// (co.aikar.util.LoadingIntMap) and needs the same declaring-class routing — added
/// once this is proven on the long map. Routing is DECLARING-CLASS-aware (see
/// `routes_to_native_map`), so a subclass instance whose inherited put/get resolves
/// into this class is ALSO served natively — no fastutil bytecode ever runs over the
/// (uninitialised) internal arrays.
/// True for a class whose Map behaviour is served by the native `$map` table
/// (so an instance MUST be a heap handle, never an arena object — the native
/// reads/writes a synthetic `$map` id on the handle). Used by the next-gen-gc
/// allocator to keep native-backed maps handle-backed during the migration.
/// ⭐**THE ALLOCATOR ASKS THIS PER BIRTH, PER ANCESTOR.** `is_native_backed_alloc` calls it for
/// every level of the super chain of every class `new` births, and the scan behind it is 22
/// full-length class-name comparisons — paid in full by the `net/minecraft/*` classes that are
/// almost all of the traffic and share not one literal with the list. `map_owner_maybe` is a
/// compile-time 2048-bit sketch (256 bytes, `.rodata`) over exactly these names: a miss PROVES the
/// name is absent, and a hit runs the identical scan that stood here alone. Kill switch
/// `RAVA_NO_FAST_MAPDISPATCH`.
pub(crate) fn is_native_map(c: &str) -> bool {
    if fast_mapdispatch_enabled() && !map_owner_maybe(c) {
        return false;
    }
    MAP_CLASSES.contains(&c) || is_fastutil_objmap(c)
}

/// A `Map.Entry` type whose key/value rava models as synthetic heap-only
/// `$key`/`$val` fields (see `entry_method`). An instance MUST be a heap handle:
/// the real `key`/`value` fields live in the JDK super (no bytecode here), so an
/// arena layout has no cell for them and `entry_method` only accepts `Value::Ref`.
/// A guava `ImmutableMapEntry` (or any subclass) extends one of these, so the
/// allocator must route it — and its subclasses — to a handle too, else
/// `getKey`/`getValue` resolve to a no-op and the caller's `areturn` underflows.
pub(crate) fn is_entry_type(c: &str) -> bool {
    // ONE authoritative table: `ENTRY_CLASSES` in `map/classes.rs` — the same table
    // `map::dispatch` routes on and the owner_bits facts word reads. The duplicated
    // `matches!` that stood here could drift from it; this delegation cannot.
    ENTRY_CLASSES.contains(&c)
}

/// GAP-P04 (`RAVA_MAPKEY_SALT`, default off): key value-equality objects by
/// `(class, guest hashCode, salt)` with equals-verified salted probing of the
/// map's OWN index instead of building the full `toString` tree per key.
#[allow(dead_code)]
fn combo_inplace_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_NO_COMBO_INPLACE").is_some())
}

#[allow(dead_code)]
fn mapkey_salt_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_MAPKEY_SALT").is_some())
}

/// GAP-P04: the memo identity of a candidate key. ONLY arena objects memoize:
/// their addresses are unique within one GC epoch and the memo is epoch-tagged.
/// Heap `Ref` handles are RECYCLED by mark-sweep without bumping the arena epoch,
/// so a handle-keyed memo could alias a swept-and-reused slot — Refs re-derive
/// (their `$hashc` field memo still spares the guest hashCode call).
fn salt_ident(k: &Value) -> Option<u64> {
    match k {
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => Some(p.addr() as u64),
        _ => None,
    }
}

#[cfg(feature = "next-gen-gc")]
fn salt_epoch_now() -> u64 {
    crate::nextgen::gc_epoch()
}
#[cfg(not(feature = "next-gen-gc"))]
fn salt_epoch_now() -> u64 {
    0
}

/// GAP-P04: per-GC-epoch memo of CONVERGED salted keys ((object ident, map) → key).
thread_local! {
    pub(crate) static SALTKEY_MEMO: std::cell::RefCell<(u64, crate::FastMap<(u64, u32), Box<str>>)> =
        std::cell::RefCell::new((0, crate::FastMap::default()));
}
fn saltkey_memo_get(ident: u64, mid: u32, epoch: u64) -> Option<String> {
    SALTKEY_MEMO.with(|c| {
        let g = c.borrow();
        if g.0 != epoch {
            return None;
        }
        g.1.get(&(ident, mid)).map(|s| s.to_string())
    })
}
fn saltkey_memo_put(ident: u64, mid: u32, key: &str, epoch: u64) {
    SALTKEY_MEMO.with(|c| {
        let mut g = c.borrow_mut();
        if g.0 != epoch || g.1.len() >= SALT_MEMO_CAP {
            g.0 = epoch;
            g.1.clear();
        }
        g.1.insert((ident, mid), key.into());
    })
}

pub(crate) fn key_str_mut(vm: &mut Vm, v: &Value) -> String {
    key_str_mut_in(vm, v, None)
        .unwrap_or_else(|_| unreachable!("unsalted key derivation cannot throw"))
}

/// GAP-P04: per-GC-epoch memo of arena value-object hashCodes (addr → hc), the
/// int twin of `VALKEY_MEMO`: between collections the bump allocator never
/// frees/relocates, so a live address maps to exactly one object; the caller
/// only inserts when the epoch is unchanged across the (allocating) guest call.
#[cfg(feature = "next-gen-gc")]
thread_local! {
    pub(crate) static HASHC_MEMO: std::cell::RefCell<(u64, crate::FastMap<u64, i32>)> =
        std::cell::RefCell::new((0, crate::FastMap::default()));
}
#[cfg(feature = "next-gen-gc")]
fn hashc_memo_get(p: crate::arena::ArenaPtr, epoch: u64) -> Option<i32> {
    HASHC_MEMO.with(|c| {
        let g = c.borrow();
        if g.0 != epoch {
            return None;
        }
        g.1.get(&(p.addr() as u64)).copied()
    })
}
/// Bound for the per-epoch key memos: fresh worldgen churn could otherwise grow
/// them into hundreds of MB within one epoch. Overflow clears (miss = recompute).
const SALT_MEMO_CAP: usize = 1 << 20;

#[cfg(feature = "next-gen-gc")]
fn hashc_memo_put(p: crate::arena::ArenaPtr, hc: i32, epoch: u64) {
    HASHC_MEMO.with(|c| {
        let mut g = c.borrow_mut();
        if g.0 != epoch || g.1.len() >= SALT_MEMO_CAP {
            g.0 = epoch;
            g.1.clear();
        }
        g.1.insert(p.addr() as u64, hc);
    })
}

/// Test-only direct entry to the salted probe engine (the env gate is
/// process-wide, so focused tests exercise the mechanics without it).
#[cfg(test)]
pub(crate) fn salted_probe_for_test(vm: &mut Vm, mid: u32, k: &Value, class: &str) -> Result<Option<String>, VmError> {
    salted_value_key(vm, mid, k, class)
}

/// RAVA_MAPKEY_HASHCODE (DIAGNOSTIC SCAFFOLD): guest-hashCode map keys — measured −28.4s boot
/// but ⚠ BREAKS the bit-exact gate (real hash collisions on structured worldgen params alias
/// unequal keys). See the arm in `key_str_mut`; sound variant needs equals-verified salt
/// chains. (A process-lifetime toString memo was tried first and REFUTED: the pathological
/// derivations are FRESH objects — worldgen builds new density trees per probe — so any
/// memo misses by construction; only making the key itself cheap helps.)
#[cfg(feature = "next-gen-gc")]
fn mapkey_hashcode_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_MAPKEY_HASHCODE").is_some())
}

/// Per-GC-epoch memo of arena value-object structural keys (addr → key). Tagged with
/// the GC epoch (`tlab_collect` bumps `nextgen::gc_epoch`): entries from a prior epoch
/// miss and recompute. Safe because between collections the bump allocator never frees
/// or relocates, so a live address maps to exactly one object — and the caller only
/// inserts when the epoch is unchanged across the (allocating) toString.
/// RAVA_NO_KEYMEMO — bypass the value-object key memo (A/B + safety escape hatch).
#[cfg(feature = "next-gen-gc")]
fn keymemo_disabled() -> bool {
    pub(crate) static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_NO_KEYMEMO").is_some())
}
#[cfg(feature = "next-gen-gc")]
thread_local! {
    pub(crate) static VALKEY_MEMO: std::cell::RefCell<(u64, crate::FastMap<u64, Box<str>>)> =
        std::cell::RefCell::new((0, crate::FastMap::default()));
}
#[cfg(feature = "next-gen-gc")]
fn valkey_memo_get(p: crate::arena::ArenaPtr, epoch: u64) -> Option<String> {
    VALKEY_MEMO.with(|c| {
        let m = c.borrow();
        if m.0 == epoch {
            m.1.get(&(p.addr())).map(|s| s.to_string())
        } else {
            None
        }
    })
}

/// Borrow a memoised key's BYTES without allocating.
///
/// `valkey_memo_get` hands back a `String`, so a HIT still costs a heap allocation - which is
/// pure waste for the fold, whose only use of the key is to feed its bytes to FNV. Measured: the
/// fold's key side is 247 ns per entry at a 99.99% hit rate, and 2M of those hits happen on a
/// boot. Serving the bytes under the borrow is byte-identical by construction: same stored key,
/// same bytes, same hash - there is no behaviour to change.
#[cfg(feature = "next-gen-gc")]
fn valkey_memo_with<R>(
    p: crate::arena::ArenaPtr,
    epoch: u64,
    f: impl FnOnce(&str) -> R,
) -> Option<R> {
    VALKEY_MEMO.with(|c| {
        let m = c.borrow();
        if m.0 != epoch {
            return None;
        }
        m.1.get(&(p.addr())).map(|s| f(s))
    })
}
#[cfg(feature = "next-gen-gc")]
fn valkey_memo_put(p: crate::arena::ArenaPtr, key: &str, epoch: u64) {
    VALKEY_MEMO.with(|c| {
        let mut m = c.borrow_mut();
        if m.0 != epoch {
            m.0 = epoch;
            m.1.clear();
        }
        m.1.insert(p.addr(), key.into());
    });
}

/// Plan A: serve `Reference2ObjectArrayMap`'s entrySet-family natively (materialize
/// its packed key/value arrays into a native collection) instead of running the
/// INTERPRETED `$EntrySet$1` iterator — the #1 boot dispatch hotspot (bootdisp 14.1:
/// ~9.3M interpreted iterator calls, 0 compiled). Faithful for the read-only
/// block-state combo maps (array order [0..size] = fastutil iteration order).
/// 14.2: default-ON (verified bit-exact — RingScan 109205b3, ChunkFill 8790, dropping
/// interpreted method-entries 14.8M→4.25M); `RAVA_NO_R2OMAP=1` restores the guest
/// iterator (kill-switch / A-B).
pub(crate) fn r2omap_native_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_R2OMAP").is_none())
}

/// ⭐**KILL SWITCH FOR THE `Reference2ObjectArrayMap.<init>(I)V` SERVE: `RAVA_NO_R2OMAP_CTOR=1`.**
///
/// The ctor crossing (the largest single BRIDGE-CALLER row of the block-state prefix: R2OArrayMap
/// @cp61 → `<init>(I)V`, 42.8% of block-1 NATIVE_BRIDGES on the PGO profile) is answered in
/// `fu_fastpath`'s `FuKind::RefArrayMap` arm by performing exactly the writes the resolved body
/// performs — two `Object[capacity]` arrays into the fresh arena receiver's `key`/`value` cells.
/// Off, the arm declines and the call takes the byte-identical route it takes today.
///
/// Deliberately a SIBLING of `RAVA_NO_R2OMAP` rather than the same switch: that one withdraws a
/// SHIPPED path (the entrySet-family native materializer), and a switch that withdraws two
/// different bets at once cannot price either. `RAVA_NO_R2OMAP=1` nevertheless withdraws THIS arm
/// with the whole family (the arm consults `r2omap_native_enabled()` first), and
/// `flags::emit_more_disabled()` bisects both in one run.
pub(crate) fn r2omap_ctor_enabled() -> bool {
    pub(crate) static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var_os("RAVA_NO_R2OMAP_CTOR").is_none()
            && r2omap_native_enabled()
            && !crate::flags::emit_more_disabled()
    });
    *ON
}

// ══════════════════ ONE GATE READ PER MAP OPERATION ═══════════════════════════════════════════
//
// ⭐**`map::dispatch` READ FOUR PROCESS CONSTANTS TO SERVE ONE `Map#get`, AND SEVEN TO SERVE ONE
// `entrySet` ON A FASTUTIL ArrayMap.** Each is a `OnceLock<bool>` acquire load — or, for
// `arena_map::enabled`, a relaxed atomic — and neither is a `mov`: an atomic load is an
// OPTIMISATION BARRIER. LLVM may not hoist it out of the caller's loop, may not fold two reads of
// the same flag into one, and must materialise whatever it was holding in registers across each
// one. This tree has already paid for that lesson three times (five such loads in front of a
// six-instruction allocation bump; three in front of a compiled-to-host choke taken 453M times;
// four to serve one cached `u32`), and it already has the cure written down twice —
// `natives::entry::entry_gates` and the `opcodes` gates word both fold several process-constant
// questions into ONE word, read once at the natural entry and passed down.
//
// **Per operation, on the population `owner_bits::S_MAP` admits** — all of `java/util/*`,
// `com/google/common/collect/*` and `it/unimi/*`, i.e. every call that reaches this dispatcher,
// of which `java/util/Map#get` alone is 37.4% of NATIVE_BRIDGES:
//
//   * `get`                                    4 atomic loads → **1**
//   * `entrySet`/`reference2ObjectEntrySet`
//     on a `Reference2ObjectArrayMap`          7 atomic loads → **1**
//   * every other admitted call (the decline)  2 atomic loads → **1**
//
// **Semantics are identical by construction.** Each bit is the SAME predicate the site read,
// computed by the SAME function; only the number of times the answer is fetched changes. None of
// these is a gate whose POSITION decides which path is reachable — every one is still consulted
// exactly where and in the order it was, and none of them has a side effect (they are env-var
// reads with no logging), so hoisting their INITIALISATION to the first dispatch of the process
// is unobservable.
//
// Kill switch `RAVA_NO_MS_GATEWORD=1` (and the tree-wide `RAVA_NO_EMIT_MORE`) clears `MG_ON`;
// every accessor below then calls the original function at the original site, restoring the N
// loads exactly, so the change prices as a same-binary A/B.

/// The gate word is live. Clear ⇒ every accessor falls back to its own atomic load.
pub(crate) const MG_ON: u8 = 1 << 0;
/// `map::classes::fast_mapdispatch_enabled()`.
pub(crate) const MG_FAST: u8 = 1 << 1;
/// `arena_map::enabled()` (always clear without `next-gen-gc`).
#[allow(dead_code)]
pub(crate) const MG_ARENAMAP: u8 = 1 << 2;
/// `r2omap_native_enabled()`.
pub(crate) const MG_R2OMAP: u8 = 1 << 3;
/// `!entryview_cache_disabled_local()` — note the POLARITY FLIP: the bit is the
/// affirmative "the entry-view cache may answer", the function is the opt-out.
pub(crate) const MG_ENTRYVIEW: u8 = 1 << 4;
/// `dispatch::mapget_census::enabled()` (always clear without `next-gen-gc`).
#[allow(dead_code)]
pub(crate) const MG_CENSUS: u8 = 1 << 5;

/// THE ONE ATOMIC LOAD. Read once at the top of `map::dispatch`; every question below it is a
/// register test on the returned byte.
#[inline]
pub(crate) fn map_gates() -> u8 {
    static W: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let mut w = 0u8;
        if std::env::var_os("RAVA_NO_MS_GATEWORD").is_none() && !crate::flags::emit_more_disabled()
        {
            w |= MG_ON;
        }
        if fast_mapdispatch_enabled() {
            w |= MG_FAST;
        }
        if r2omap_native_enabled() {
            w |= MG_R2OMAP;
        }
        if !entryview_cache_disabled_local() {
            w |= MG_ENTRYVIEW;
        }
        #[cfg(feature = "next-gen-gc")]
        {
            if crate::arena_map::enabled() {
                w |= MG_ARENAMAP;
            }
            if dispatch::mapget_census::enabled() {
                w |= MG_CENSUS;
            }
        }
        w
    })
}

#[inline]
pub(crate) fn mg_fast(g: u8) -> bool {
    if g & MG_ON != 0 { g & MG_FAST != 0 } else { fast_mapdispatch_enabled() }
}

#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn mg_arena_map(g: u8) -> bool {
    if g & MG_ON != 0 { g & MG_ARENAMAP != 0 } else { crate::arena_map::enabled() }
}

/// Only read inside `next-gen-gc` arms today; the allow keeps the default build warning-free.
#[allow(dead_code)]
#[inline]
pub(crate) fn mg_r2omap(g: u8) -> bool {
    if g & MG_ON != 0 { g & MG_R2OMAP != 0 } else { r2omap_native_enabled() }
}

/// AFFIRMATIVE form: "the entry-view cache may answer". The historic call site tested the
/// opt-out `entryview_cache_disabled()`, so every use of this reads `!` of that.
#[allow(dead_code)]
#[inline]
pub(crate) fn mg_entryview(g: u8) -> bool {
    if g & MG_ON != 0 { g & MG_ENTRYVIEW != 0 } else { !entryview_cache_disabled_local() }
}

#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn mg_census(g: u8) -> bool {
    if g & MG_ON != 0 { g & MG_CENSUS != 0 } else { dispatch::mapget_census::enabled() }
}

/// IMMUTABLE-VALUE RENDER MEMO (`RAVA_NO_KEYRENDER_MEMO` opts out).
///
/// The guest `toString()` under `key_str_mut_in` is 3.238s over 3,400,000 renders (952 ns each) on
/// a boot, and the key census says the population is tiny and repetitive: 219,142 renders of
/// `Direction`, of which SIX distinct objects exist, and 219,520 of `BlockState`.
///
/// A memo cannot change the answer - same function, fewer calls - so the entire question is when
/// a stored render can go stale. The admission rule is a JVM guarantee, not a heuristic:
///
/// * a class whose instance fields are ALL `final` cannot be mutated after construction, because
///   the verifier rejects `putfield` on a final field outside the declaring class's `<init>`;
/// * so once the object is constructed, its render is fixed for its lifetime.
///
/// ⚠ Two holes, both closed deliberately rather than by omission:
/// * rava does NOT verify bytecode, so a hand-crafted class could write a final field outside
///   `<init>` and go stale. That is what the kill switch is for.
/// * `reflect.rs` can set a field regardless of `final`. Reflective writes bump `static_gen`,
///   which is folded into the memo tag below, so such a write invalidates every entry.
///
/// The GC epoch is in the tag too: heap handles are stable while an object lives, but a collection
/// can hand the same index to a different object.
/// Upper bound on live entries, mirroring `stringify_memo::MAX_ENTRIES`.
#[cfg(feature = "next-gen-gc")]
const KEYRENDER_MAX: usize = 64 * 1024;

/// Combo-map u64 key for the block-state property maps (arena `Value::Obj`:
/// `Reference2ObjectArrayMap` via `drain_arena_array_map`, or a rava arena map).
/// `None` for anything else, which keeps the exact canonical-string path. Only
/// arena representations are folded — block-state combos are always arena Obj
/// (map.rs comment above `structural_key`), so no heap-vs-arena representation of
/// the same logical key can diverge.
#[cfg(feature = "next-gen-gc")]
fn combo_hash_key(vm: &mut Vm, v: &Value) -> Option<u64> {
    if let Value::Obj(p) = v {
        if crate::arena_map::enabled() && crate::arena_map::is_arena_map(*p) {
            let es = crate::arena_map::entries(*p);
            return Some(combo_hash_of(vm, &es));
        }
        // In place: see `combo_hash_arena_array_map`. The materialising form allocated three
        // Vecs and cloned every `Value` in the map, 570,894 times a boot, to produce a u64.
        // `RAVA_NO_COMBO_INPLACE` restores it so the change prices as a same-binary A/B.
        if !combo_inplace_disabled() {
            if let Some(h) = crate::map::keys::combo_hash_arena_array_map(vm, *p) {
                return Some(h);
            }
        }
    }
    None
}

/// RAVA_KEYSTAT: bytes of canonical combo-string NO LONGER built (per fold, the
/// pair bytes that the old `format!`+join+Box<str> allocated). Tallied so the boot
/// A/B can report the churn eliminated.
#[cfg(feature = "next-gen-gc")]
fn keystat_bytes(n: usize) {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;
    pub(crate) static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_KEYSTAT").is_some()) {
        return;
    }
    pub(crate) static BYTES: AtomicU64 = AtomicU64::new(0);
    pub(crate) static CALLS: AtomicU64 = AtomicU64::new(0);
    BYTES.fetch_add(n as u64, Ordering::Relaxed);
    let c = CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    if c % 2_000_000 == 0 {
        eprintln!("[KEYSTAT] combo_hash folds={} canonical_bytes_not_built={}",
            c, BYTES.load(Ordering::Relaxed));
    }
}

pub(crate) fn structural_key(vm: &mut Vm, v: &Value) -> String {
    structural_key_scoped(vm, v, None)
        .unwrap_or_else(|_| unreachable!("unsalted structural_key cannot throw"))
}

/// GAP-P04: `structural_key` with a salt scope — the TOP-LEVEL leaf value-object
/// derivation salts against map `mid` (guest throws propagate); composite
/// (drainable-map) branches and their nested parts keep the exact unsalted form.
pub(crate) fn structural_key_in(vm: &mut Vm, mid: u32, v: &Value) -> Result<String, VmError> {
    structural_key_scoped(vm, v, Some(mid))
}

/// `m:{a=b,c=d}` assembled in ONE allocation with exact capacity.
///
/// `parts.join(",")` materialises the entire key text, and `format!("m:{{{}}}", ..)` then allocates
/// AGAIN and copies all of it a second time. Map keys in this tree have been measured in the
/// hundreds of kilobytes (BlendDensity: 651,441 B, from the key-intern work), so that second copy
/// is a real cost on a path with NO early return - every stored map key passes through here.
/// Byte-for-byte the same output: this removes a copy, it does not touch key identity.
fn map_key_braced(parts: &[String]) -> String {
    let inner: usize =
        parts.iter().map(|p| p.len()).sum::<usize>() + parts.len().saturating_sub(1);
    let mut out = String::with_capacity(inner + 4);
    out.push_str("m:{");
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(p);
    }
    out.push('}');
    out
}

/// THE ENTRY-BIRTH AREA GATE (`RAVA_NO_ENTRY_BIRTH=1` restores the historic code verbatim, and
/// so does `flags::emit_more_disabled()`, so the whole area prices as ONE A/B).
///
/// Read ONCE through a `OnceLock`: the sites it guards are on the entry-view path, which the
/// window enters ~490 k times, and a per-call `var_os` there would cost more than everything the
/// gate saves.
fn entry_birth_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_ENTRY_BIRTH").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// The `$mapview` marker, from a SHARED `Arc` instead of a fresh one per call.
///
/// ⭐**A CONSTANT THAT WAS ALLOCATED PER CALL.** `Value::Str("entry".into())` is
/// `Arc::new(String::from("entry"))` — **two mallocs** (`value.rs:111`) — and the three markers
/// this engine stamps (`entry`, `key`, `value`) are compile-time constants that never change and
/// are never mutated. Four sites paid for it: `dispatch`'s `entrySet` stamp, its
/// `keySet`/`values` stamp, and (through the same helper) anything else that marks a view.
///
/// ⚠**Why sharing one `Arc` across every view is safe.** `RStr` is an immutable `Arc<String>`
/// with no interior mutability and no `get_mut` caller, so no holder can observe another's write.
/// The one place string IDENTITY is read is `opcodes::gates`' fast path
/// `RStr::ptr_eq(x, y) || x == y` — a fast YES that already falls through to a byte compare, so
/// making two equal markers share a pointer can only turn a byte compare into a pointer compare,
/// never change an answer. And `$mapview` is a SYNTHETIC field: it is written and read only by
/// rava's own natives (`map::hashing::view_tag`, `view.rs`), never handed to guest code, so no
/// guest `==` can see it at all.
///
/// The cold arm below is the historic expression verbatim.
pub(crate) fn view_marker(kind: &'static str) -> Value {
    if !entry_birth_enabled() {
        return Value::Str(kind.into());
    }
    // A guard may only REJECT: an unexpected marker text falls back to the historic build rather
    // than inventing a fourth shared cell.
    let cell: &'static std::sync::OnceLock<crate::value::RStr> = match kind {
        "entry" => {
            static E: std::sync::OnceLock<crate::value::RStr> = std::sync::OnceLock::new();
            &E
        }
        "key" => {
            static K: std::sync::OnceLock<crate::value::RStr> = std::sync::OnceLock::new();
            &K
        }
        "value" => {
            static V: std::sync::OnceLock<crate::value::RStr> = std::sync::OnceLock::new();
            &V
        }
        _ => return Value::Str(kind.into()),
    };
    entry_census::bump(&entry_census::MARKER_SHARED, 1);
    Value::Str(cell.get_or_init(|| kind.into()).clone())
}

/// THE ENTRY-BIRTH CENSUS (`RAVA_ENTRY_CENSUS=1`, off by default) — **AND THE ANSWER IT GAVE.**
///
/// ⛔⭐**THIS LINE IS CLOSED BY COUNT, and the count is the whole finding.** The standing figure
/// for `Map$Entry` — *48.11% of the boot's 6,234,501 handle births, ~3.0 M objects* — is a share
/// of HANDLE BIRTHS over a whole product boot, and it had never been converted into a share of
/// the reload window that is the target. Converted, on `bench/ReloadBench`:
///
/// ```text
/// [ENTRY] minted 152220 (handle 152220, arena 0) of 150387 asked
///         | shape memo 460533 hit / 31290 miss (93.6% hit)
/// [ENTRY]   miss reasons: first-ever receiver 31284 | GC epoch moved 6
///           | shape changed 0 | cells changed 0 | view dead 0 | probe declined 0
/// ```
///
/// * **152,220 entries, not 3.0 M.** At `bench/NativeCost.java`'s 324 ns per handle birth that is
///   **49 ms of an 8,067 ms reload = 0.61%** — below the harness floor. The 48% is real; it is a
///   share of a population that is itself small next to the window's arena traffic.
/// * **The cache is already perfect.** 460,533 of 491,823 entry-view calls (93.6%) are served
///   with **no object at all**. Of the 31,290 that are not, **31,284 are receivers the memo has
///   never seen** — a map whose `entrySet()` is called exactly ONCE in its life. `shape changed`,
///   `cells changed` and `view dead` are all **ZERO**: not one miss is a view the memo held and
///   had to throw away, so there is no invalidation to tighten and no capacity to raise.
/// * **`probe declined` is ZERO too**, so there is no receiver population the shape probe is
///   failing to admit — widening it would serve nobody.
/// * The remaining 150,387 entries go straight into guest bytecode: `ObjectSet#iterator` fires
///   exactly 29,708 times against the 29,708 misses, one per view. **They escape by
///   construction**, so no reusable / non-escaping entry can be substituted for them.
///
/// ⇒ What is left to remove is not the object, it is the CONSTANT-per-call waste around it, which
/// is what `view_marker` and `entry_view`'s buffer reuse do above.
///
/// Every counter is behind `enabled()`, a process-constant `OnceLock`, so a production run does
/// not execute one atomic RMW per entry (house rule: a census may not be an unconditional RMW on
/// a hot path). Reported from the periodic profiler dump, never from a teardown a server boot
/// cannot reach.
pub(crate) mod entry_census {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    /// Entries actually MINTED as objects (`make_entry`), split by representation.
    pub(crate) static MINT_HANDLE: AtomicU64 = AtomicU64::new(0);
    pub(crate) static MINT_ARENA: AtomicU64 = AtomicU64::new(0);
    /// `entry_view` calls and the entries they produced — the batch producer behind ~all mints.
    pub(crate) static VIEW_CALLS: AtomicU64 = AtomicU64::new(0);
    pub(crate) static VIEW_ENTRIES: AtomicU64 = AtomicU64::new(0);
    /// The `dispatch` shape memo, which is the only thing standing between the
    /// ~2.9 M `reference2ObjectEntrySet` calls a boot and a full materialisation.
    pub(crate) static EV_HIT: AtomicU64 = AtomicU64::new(0);
    pub(crate) static EV_MISS: AtomicU64 = AtomicU64::new(0);
    /// WHY a miss missed. ⭐This is the split that decides whether the remaining misses are
    /// closable at all: `MISS_FIRST` is a receiver the memo has never seen, i.e. a map whose
    /// `entrySet` is called ONCE in its life — there is nothing a cache could have held — while
    /// `MISS_SHAPE`/`MISS_CELLS`/`MISS_DEAD` are receivers it DID hold and had to discard.
    pub(crate) static MISS_FIRST: AtomicU64 = AtomicU64::new(0);
    /// The whole table was dropped because the GC epoch moved on — a different failure from a
    /// receiver nobody ever cached, and the two were indistinguishable until they were split.
    pub(crate) static MISS_EPOCH: AtomicU64 = AtomicU64::new(0);
    pub(crate) static MISS_SHAPE: AtomicU64 = AtomicU64::new(0);
    pub(crate) static MISS_CELLS: AtomicU64 = AtomicU64::new(0);
    pub(crate) static MISS_DEAD: AtomicU64 = AtomicU64::new(0);
    /// Calls whose receiver the shape probe DECLINED — a non-ArrayMap or off-layout receiver, for
    /// which no memo entry can exist and the historic full rebuild is the only route.
    pub(crate) static NO_PROBE: AtomicU64 = AtomicU64::new(0);
    /// `Arc<String>` view-marker constants NOT built, because the marker is now a shared static.
    pub(crate) static MARKER_SHARED: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_ENTRY_CENSUS").is_some())
    }

    #[inline]
    pub(crate) fn bump(c: &AtomicU64, n: u64) {
        if enabled() {
            c.fetch_add(n, Relaxed);
        }
    }

    pub fn report() -> String {
        if !enabled() {
            return String::new();
        }
        let (mh, ma) = (MINT_HANDLE.load(Relaxed), MINT_ARENA.load(Relaxed));
        let (vc, ve) = (VIEW_CALLS.load(Relaxed), VIEW_ENTRIES.load(Relaxed));
        let (hit, miss) = (EV_HIT.load(Relaxed), EV_MISS.load(Relaxed));
        if mh + ma + vc + hit + miss == 0 {
            return String::new();
        }
        format!(
            "[ENTRY] minted {} (handle {mh}, arena {ma}) over {vc} view builds ({ve} entries) | shape memo {hit} hit / {miss} miss ({:.1}% served with NO object) | view-marker Arc<String> not built {}
[ENTRY]   miss reasons: first-ever receiver {} | GC epoch moved {} | shape changed {} | cells changed {} | view dead {} | probe declined {}",
            mh + ma,
            if hit + miss > 0 { hit as f64 * 100.0 / (hit + miss) as f64 } else { 0.0 },
            MARKER_SHARED.load(Relaxed),
            MISS_FIRST.load(Relaxed),
            MISS_EPOCH.load(Relaxed),
            MISS_SHAPE.load(Relaxed),
            MISS_CELLS.load(Relaxed),
            MISS_DEAD.load(Relaxed),
            NO_PROBE.load(Relaxed),
        )
    }
}

/// Build a `Map$Entry`, arena-born when the route admits it.
///
/// ⭐**One producer for 48% of the boot's handle births.** Six sites allocated the entry and
/// wrote `$key`/`$val` by hand; each was a heap handle by construction, and an entry is the
/// shortest-lived object the engine makes — allocated, read twice, dropped. Mirrors
/// `stream::make_optional` exactly: try the arena, fall back to the handle when any of
/// `try_arena_alloc_by_name`'s vetoes fires, and write through `synth_put`, which serves both
/// forms. Nothing about the entry's contract changes, which is why the readers did not have to.
///
/// ⛔**AND 48% OF THE BIRTHS IS 0.61% OF THE WINDOW — see `entry_census` above for the count.**
/// The reload window mints **152,220** of these, all of them for maps whose `entrySet()` is
/// called exactly once, and the view memo already serves the other 93.6% of the traffic without
/// allocating anything. Do not build a reusable / non-escaping entry here: the census says there
/// is no population for it. `MINT_ARENA` stays at zero because `try_arena_alloc_by_name` is behind
/// `RAVA_ARENA_NATIVE_NEW` + `RAVA_ARENA_SYNTH`, both opt-in and both measured NEUTRAL
/// (`nextgen/gates.rs`); arming them is that flag's A/B, not this function's.
pub(crate) fn make_entry(vm: &mut Vm, k: Value, v: Value) -> Value {
    #[cfg(feature = "next-gen-gc")]
    if let Some(p) = vm.try_arena_alloc_by_name("java/util/Map$Entry") {
        entry_census::bump(&entry_census::MINT_ARENA, 1);
        let recv = Value::Obj(p);
        vm.synth_put(&recv, "$key", k);
        vm.synth_put(&recv, "$val", v);
        return recv;
    }
    entry_census::bump(&entry_census::MINT_HANDLE, 1);
    let h = vm.heap.alloc("java/util/Map$Entry");
    vm.heap.put_field_syn(h, crate::heap::Syn::Key, k);
    vm.heap.put_field_syn(h, crate::heap::Syn::Val, v);
    Value::Ref(h)
}

/// Drain ANY map value's entries — an arena map (RAVA_ARENA_MAPS java/util/HashMap)
/// or a `$map`-backed handle (fastutil/jar/native map). The immutable-copy and
/// putAll paths take a `Map` argument that, under the flag, may now be an arena
/// map, so they must read its flat table instead of a non-existent `$map`.
pub(crate) fn drain_any(vm: &mut Vm, src: &Value) -> Vec<(Value, Value)> {
    match src {
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::arena_map::enabled() && crate::arena_map::is_arena_map(*p) => {
            crate::arena_map::entries(*p)
        }
        Value::Ref(h) => vm.drain_map(*h).ok().flatten().unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Get (or lazily create) the map id stored on the receiver object.
pub(crate) fn map_id(vm: &mut Vm, obj: u32) -> u32 {
    match vm.heap.get_field_syn(obj, crate::heap::Syn::Map) {
        Value::Int(id) => id as u32,
        other => {
            if std::env::var("RAVA_TRACE_MAPKEY").is_ok() {
                eprintln!(
                    "[MAPID-MISS] obj={obj} cls={:?} $map={other:?} fields={:?}",
                    vm.heap.class_of(obj),
                    vm.heap.debug_field_ids(obj)
                );
            }
            let id = vm.heap.alloc_map();
            vm.heap.put_field_syn(obj, crate::heap::Syn::Map, Value::Int(id as i32));
            id
        }
    }
}

#[cfg(feature = "next-gen-gc")]
fn map_key_gc_protect_enabled() -> bool {
    pub(crate) static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("RAVA_MAP_KEY_GC_PROTECT")
            .map(|value| value != "0")
            .unwrap_or(true)
    })
}

/// Wrap values in an ArrayList-like collection (so .stream()/.forEach() work).
pub(crate) fn collection(vm: &mut Vm, elems: Vec<Value>) -> Value {
    let arr = vm.heap.alloc_values("Ljava/lang/Object;", elems);
    // ⭐**GC ROOT — ledger row 341, and this is the CLASS fix.** `arr` is a bare handle in a Rust
    // local, and the very next line ALLOCATES. A Rust local is invisible to the root walk, so a
    // collection here frees the array we are about to install and `$elems` gets a dangling handle.
    // Every `collection()` caller inherited that; rooting it here fixes them all at once, which is
    // the thing this tree keeps recording as "fixed at ONE SITE, not as a class".
    //
    // The value is read back OUT of the rooted slot rather than reused from `arr`: the collector
    // forwards the slot IN PLACE, so the local copy can be stale by the time the alloc returns.
    let mut root = [Value::Array(arr)];
    let base = crate::frames::native_args_push(root.as_ptr(), 1);
    let h = vm.heap.alloc("java/util/ArrayList");
    let elems_cell = root[0].clone();
    crate::frames::native_args_truncate(base);
    vm.heap.put_field_syn(h, crate::heap::Syn::Elems, elems_cell);
    Value::Ref(h)
}

/// Build the `java/util/Map$Entry` view objects for `entries`, GC-ROOTED throughout.
///
/// ⭐**Ledger rows 340/341.** Four sites built these in a plain Rust `Vec<Value>`, holding every
/// already-created entry AND the keys and values feeding them across `heap.alloc` / `put_field`,
/// both of which can collect — so the collector freed entries the loop had already made. It cost a
/// **0xC0000005** on any interpreted run that collected, and it was invisible in production only
/// because production always has the JIT on.
///
/// The buffer is FLAT and PRE-SIZED because `native_args_push` records a `(ptr, len)` ONCE: a
/// growing `Vec` leaves later elements unrooted and a reallocating one leaves the registration
/// dangling. Layout `[k0, v0, k1, v1, …]` then `n` slots for the refs, every slot a valid `Value`.
/// THE ENTRIES OF A SOURCE MAP IN EITHER REPRESENTATION.
///
/// A guest map is `$map`-backed (a handle) or an arena map (`RAVA_ARENA_MAPS`), and every
/// place that copies FROM a map has to read both. `map_method`'s copy-constructor and its
/// `putAll` matched `Some(Value::Ref(src))` only, so an arena source fell through the
/// `if let` and produced an EMPTY copy in silence — `new TreeMap<>(arenaMap)` came back
/// `{}` and the `equals` that followed said false (`bench/CollSem.java`, two of the four
/// divergent lines the flag showed). Same shape as `arena_map::source_entries`, which had
/// the pair right on its side of the fence and no way to be reached from this one.
pub(crate) fn any_map_entries(vm: &mut Vm, src: &Value) -> Vec<(Value, Value)> {
    match src {
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::arena_map::is_arena_map(*p) => crate::arena_map::entries(*p),
        Value::Ref(h) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Map) {
            Value::Int(id) => vm.heap.map_entries(id as u32),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

pub(crate) fn entry_view(vm: &mut Vm, entries: Vec<(Value, Value)>) -> Vec<Value> {
    let n = entries.len();
    entry_census::bump(&entry_census::VIEW_CALLS, 1);
    entry_census::bump(&entry_census::VIEW_ENTRIES, n as u64);
    let mut roots: Vec<Value> = Vec::with_capacity(n * 3);
    for (k, v) in entries {
        roots.push(k);
        roots.push(v);
    }
    roots.resize(n * 3, Value::Null);
    let base = crate::frames::native_args_push(roots.as_ptr(), roots.len());
    for i in 0..n {
        // Read the key/value back out of the ROOTED slots — a collection during the allocation
        // forwards them in place, and a copy taken before it would be stale.
        let (k, val) = (roots[i * 2].clone(), roots[i * 2 + 1].clone());
        roots[n * 2 + i] = make_entry(vm, k, val);
    }
    if !entry_birth_enabled() {
        let out = roots[n * 2..].to_vec();
        crate::frames::native_args_truncate(base);
        return out;
    }
    // ⭐**THE TAIL OF THE ROOT BUFFER *IS* THE ANSWER.** The historic line copied the `n` finished
    // entries into a SECOND `Vec` — one malloc, `n` clones, and then drop glue over all `3n` slots
    // of the first — purely to hand `collection` a vector starting at index 0. Draining the
    // `[k0, v0, …]` prefix shifts the same buffer down instead: one allocation for the whole view,
    // and the `2n` key/value clones are dropped by the drain exactly as they were by the old
    // `Vec`'s destructor.
    //
    // ⚠ORDER IS LOAD-BEARING: `native_args_truncate` FIRST. The registration is a `(ptr, len)`
    // pair over `roots`, and `drain` moves elements inside it — unregistering before any move is
    // what keeps the root walk from reading a slot the shift has already vacated. Nothing between
    // the truncate and the return allocates, so no collection can observe the gap, which is the
    // same window the historic `to_vec()` left open.
    crate::frames::native_args_truncate(base);
    drop(roots.drain(..n * 2));
    roots
}

pub(crate) mod dispatch;
pub(crate) use dispatch::*;
pub(crate) mod keys;
pub(crate) use keys::*;
mod methods;
pub(crate) use methods::*;
mod classes;
pub(crate) use classes::*;
mod gcsafe;
pub(crate) use gcsafe::*;
pub(crate) mod keyroot;
pub(crate) use keyroot::*;

mod hashing;
pub(crate) use hashing::*;

/// The index-renumbering the heap's map operations do around `MapObj::entries`. Lives next
/// to the map family rather than in `heap::maps` because it is pure table arithmetic with no
/// heap of its own — see its module docs for the budgeted-scan argument and the kill switch.
pub(crate) mod table;

/// Which `Map` operations `fu_fastpath` may serve straight from machine code, and therefore which
/// call sites `fu_routable` admits. ONE list, consulted by both — a site admitted here that the
/// fast path then declines pays an extra refused crossing, and the two lists drifting apart is
/// exactly how that happens.
///
/// Reference arguments only: the fast path reads one cell per argument, so a `long`/`double`
/// parameter would silently shift the whole argument list. Nothing on this list has one.
pub(crate) fn fu_map_op(name: &str, desc: &str) -> bool {
    matches!(
        (name, desc),
        ("get", "(Ljava/lang/Object;)Ljava/lang/Object;")
            | ("getOrDefault", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;")
            | ("put", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;")
            | ("remove", "(Ljava/lang/Object;)Ljava/lang/Object;")
            | ("containsKey", "(Ljava/lang/Object;)Z")
            | ("containsValue", "(Ljava/lang/Object;)Z")
            | ("size", "()I")
            | ("isEmpty", "()Z")
            | ("entrySet", "()Ljava/util/Set;")
            | ("keySet", "()Ljava/util/Set;")
            | ("values", "()Ljava/util/Collection;")
    )
}

/// The `RAVA_NO_ENTRYVIEW_CACHE` predicate, read here rather than through `dispatch`.
///
/// `dispatch::entryview_cache_disabled` is the same expression, but reaching it from this module
/// is a visibility question that changes with the crate's feature set, and a gate-word builder
/// must compile in every configuration. The env test is the contract; keeping one copy of the
/// expression beside the bit it fills is what makes the word and the kill-switch arm agree.
fn entryview_cache_disabled_local() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_NO_ENTRYVIEW_CACHE").is_some())
}
