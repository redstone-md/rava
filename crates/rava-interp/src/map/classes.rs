//! Which classes are maps, which are fastutil, and the ids behind them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::map::*;

pub(crate) const MAP_CLASSES: &[&str] = &[
    "java/util/HashMap",
    "java/util/LinkedHashMap",
    "java/util/concurrent/ConcurrentHashMap",
    "java/util/TreeMap",
    "java/util/Hashtable",
    "java/util/IdentityHashMap",
    "java/util/WeakHashMap",
    "java/util/EnumMap",
    "java/util/Properties",
    "java/util/Map",
    "java/util/AbstractMap",
    // guava maps served natively (their bytecode mis-executes here).
    "com/google/common/collect/HashBiMap",
    "com/google/common/collect/ImmutableBiMap",
    "com/google/common/collect/BiMap",
    // MapMaker().makeMap() result — a weak/concurrent interner used by
    // ResourceKey.create. Its segment/entry bytecode needs sun.misc.Unsafe and
    // mis-executes here, so serve it from the native $map.
    "com/google/common/collect/MapMakerInternalMap",
];

/// ⭐**THE EXACT-IDENTITY PROVIDERS BEHIND `FACT_FORCE_NATIVE_MAP` — THE CLASSES
/// `force_native_collection` ROUTES WHOLESALE TO THE NATIVE MAP LAYER.**
///
/// The force-native gate historically admitted the HashBiMap by the SHAPE
/// `com/google/common/collect/HashBiMap` as a PREFIX — a forbidden approximation now that
/// the fact exists. The provider truth is EXACT: the native map layer completely supplies
/// `HashBiMap` (`create`/`create(int)`/`create(Map)` fabricate an exact-name handle with a
/// native `$map` — see `map::dispatch`'s create arm — `inverse()` fabricates an
/// exact-parent-name view marked `$inverseof`, and `map_method` serves the whole method
/// surface), so the class is licensed as an exact identity the way RegisterNatives is —
/// never a workload-derived allowlist. Both fabrication sites allocate the EXACT parent
/// name, and the nested prefix classes (`HashBiMap$BiEntry`/`$Itr`/`$Inverse`/
/// `$InverseKeySet`) are born only inside HashBiMap bytecode that cannot execute while
/// every method of the exact class force-routes — so no nested name can reach the gate as
/// owner, actual or lambda owner. That is the static membership proof; no served history
/// defines it.
///
/// The drift tests at the foot of this file prove every member is a [`MAP_CLASSES`] row
/// and is listed once — so a name added here and not there fails a test, not a boot.
pub(crate) const FORCE_NATIVE_MAP_CLASSES: &[&str] = &["com/google/common/collect/HashBiMap"];

/// The classes `map::dispatch` routes to `entry_method` — i.e. the objects that
/// carry our `$key`/`$val` entry shape. Used to decide whether the OTHER operand
/// of `Map.Entry.equals` is an entry we can compare by value.
pub(crate) const ENTRY_CLASSES: &[&str] = &[
    "java/util/Map$Entry",
    "java/util/AbstractMap$SimpleEntry",
    "java/util/AbstractMap$SimpleImmutableEntry",
];

pub(crate) fn is_entry_class(c: &str) -> bool {
    ENTRY_CLASSES.contains(&c)
}

/// Primitive-keyed fastutil object maps, served natively in every build.
///
/// A LIST rather than a `matches!` arm so that [`MAP_OWNER_SKETCH`] and the drift test below
/// can both read the membership from the one place it is written. Same names, same order.
pub(crate) const FASTUTIL_OBJMAPS: &[&str] = &[
    "it/unimi/dsi/fastutil/longs/Long2ObjectOpenHashMap",
    "it/unimi/dsi/fastutil/longs/Long2ObjectLinkedOpenHashMap",
    // Int2Object* IS subclassed by Paper (co.aikar.util.LoadingIntMap extends
    // Int2ObjectOpenHashMap); safe now that routing is declaring-class-aware
    // (the subclass's inherited put/get resolves INTO the base → native).
    "it/unimi/dsi/fastutil/ints/Int2ObjectOpenHashMap",
    "it/unimi/dsi/fastutil/ints/Int2ObjectLinkedOpenHashMap",
];

/// Object-keyed fastutil maps. Wall #12: DFU's BaseMapCodec.decode (every
/// Codec.unboundedMap: world_preset dimensions, advancement criteria, …)
/// accumulates into an `Object2ObjectArrayMap`, then `ImmutableMap.copyOf`
/// materialises it — as GUEST bytecode it had no `$map`, so the copyOf drain read
/// it EMPTY. Wall #14: `AdvancementTree.nodes` is an `Object2ObjectOpenHashMap`,
/// and its guest `get(parentId)` mis-matched heap-`Identifier` keys (instance
/// hash/equals), so 18 advancements couldn't find their parent ("Couldn't load
/// advancements"). Force all three native (value-keyed `$map`) — the
/// `object2ObjectEntrySet→entrySet` alias (see `fastutil_alias`) covers the
/// fastutil-only accessor DFU's TaggedChoice drives, so the Open/Linked variants
/// no longer ULE the type bootstrap. Gated → default build byte-identical.
pub(crate) const FASTUTIL_OBJ2OBJ: &[&str] = &[
    "it/unimi/dsi/fastutil/objects/Object2ObjectArrayMap",
    "it/unimi/dsi/fastutil/objects/Object2ObjectOpenHashMap",
    "it/unimi/dsi/fastutil/objects/Object2ObjectLinkedOpenHashMap",
];

pub(crate) fn is_fastutil_objmap(c: &str) -> bool {
    FASTUTIL_OBJMAPS.contains(&c)
        || (cfg!(feature = "next-gen-gc") && FASTUTIL_OBJ2OBJ.contains(&c))
}

/// ⭐**THE EXACT-IDENTITY PROVIDERS BEHIND `FACT_GUAVA_IMMUTABLE` — ONE AUTHORITATIVE UNION.**
///
/// `jit_native_served` historically admitted guava immutable collections by the SHAPE
/// `com/google/common/collect/*` + contains("Immutable") — a forbidden approximation now that
/// the fact exists (and `wide_tier_admits`, which shared the literal, answers the same classes
/// by type closure instead — see its own comment). This is the union of the four provider
/// tables that name such classes exactly, deduplicated (a class listed by two tables is ONE row):
/// `GUAVA_IMMUTABLE_COPYOF_OWNERS` (map/dispatch's copyOf arm),
/// `GUAVA_IMMUTABLE_ENTRY_CLASSES` (fu_fastpath's arena-entry arm), the Immutable rows of
/// `MAP_CLASSES` (ImmutableBiMap), and the Immutable rows of `crate::stream::OWNER_LITERALS`
/// (ImmutableMap, ImmutableBiMap). Every member satisfies the historic shape; the drift test at
/// the foot of this file re-derives all four containments so a name added to a provider table
/// and not here fails a test rather than a boot. The bit is the exact union membership and the
/// check's ONLY guava arm — the classes only the historic shape admitted (no table member, no
/// earlier arm) compile.
pub(crate) const GUAVA_IMMUTABLE_PROVIDERS: &[&str] = &[
    // GUAVA_IMMUTABLE_COPYOF_OWNERS (map/dispatch copyOf arm).
    "com/google/common/collect/ImmutableMap",
    "com/google/common/collect/ImmutableSortedMap",
    "com/google/common/collect/ImmutableBiMap",
    // GUAVA_IMMUTABLE_ENTRY_CLASSES (fu_fastpath arena-entry arm).
    "com/google/common/collect/ImmutableEntry",
    "com/google/common/collect/ImmutableMapEntry",
    "com/google/common/collect/ImmutableMapEntry$NonTerminalImmutableMapEntry",
    "com/google/common/collect/ImmutableMapEntry$NonTerminalImmutableBiMapEntry",
    // MAP_CLASSES' Immutable row (ImmutableBiMap — deduplicated above).
    // stream::OWNER_LITERALS' Immutable rows (ImmutableMap, ImmutableBiMap — deduplicated above).
];

// ─── WHICH OWNERS THE MAP LAYER CAN SERVE, ANSWERED BEFORE THE CALL COSTS ANYTHING ─────────
//
// ⭐**A QUESTION ABOUT A NAME WAS BEING ASKED PER CALL — AFTER THE CALL HAD ALREADY BEEN PAID
// FOR.**
//
// `map::dispatch` opens by CLONING every argument into `vm.gc_protect` (a map op can re-enter the
// interpreter, so its arguments must be roots), and only THEN walks ~12 `owner == "…"` literals
// before reaching its closing `MAP_CLASSES.contains(owner) || is_fastutil_objmap(owner)` — 22 more
// comparisons. The stage bit that admits calls to it (`owner_bits::S_MAP`) is a deliberate
// superset: all of `java/util/*`, `com/google/common/collect/*` and `it/unimi/*`. So
// `java/util/Objects.equals`, `java/util/Random.nextInt` and `java/util/UUID.fromString` — none of
// which any arm here can serve — each pay one `Value` clone and one `Vec` push per argument plus
// up to 34 name comparisons, to be told no.
//
// `is_native_map` has the same shape on a worse path: the ALLOCATOR asks it for every ancestor of
// every class it births (`birth_native_backed_ancestor` → `is_native_backed_alloc`), i.e. up to 22
// comparisons per super-chain level per `new`, and nearly every birth is a `net/minecraft/*` class
// that shares not one literal with this list.
//
// **What grows: the number of class names the map layer serves** — 31 today, one literal per
// natively-served class — scanned per CALL and per BIRTH for an answer that is a constant of the
// name.
//
// The cure is NOT a memo; there is no state worth keeping. [`MAP_OWNER_SKETCH`] is a 2048-bit
// membership filter over `(length, last four bytes)`, folded from the literal table AT COMPILE
// TIME: **256 bytes in `.rodata`, no per-call state, no thread-local probe, nothing to
// invalidate.**
// A miss is a PROOF of absence in about three instructions; a hit falls through to exactly the
// comparisons that stood there before, so the filtered arm cannot answer differently from the
// unfiltered one. These names are long package paths sharing long prefixes and differing at the
// tail, which is why the tail is what the fingerprint reads.

/// Every class name any owner-keyed arm of `map::dispatch` can serve, plus everything
/// `is_native_map` answers `true` for. A deliberate SUPERSET of both: over-listing costs only the
/// exact comparison that follows, under-listing would drop a native. The drift test at the foot of
/// this file re-derives the containment from `MAP_CLASSES` / `ENTRY_CLASSES` / the two fastutil
/// lists, so a name added there and not here fails the build's tests rather than a boot.
pub(crate) const MAP_OWNERS: &[&str] = &[
    // --- MAP_CLASSES ---
    "java/util/HashMap",
    "java/util/LinkedHashMap",
    "java/util/concurrent/ConcurrentHashMap",
    "java/util/TreeMap",
    "java/util/Hashtable",
    "java/util/IdentityHashMap",
    "java/util/WeakHashMap",
    "java/util/EnumMap",
    "java/util/Properties",
    "java/util/Map",
    "java/util/AbstractMap",
    "com/google/common/collect/HashBiMap",
    "com/google/common/collect/ImmutableBiMap",
    "com/google/common/collect/BiMap",
    "com/google/common/collect/MapMakerInternalMap",
    // --- ENTRY_CLASSES ---
    "java/util/Map$Entry",
    "java/util/AbstractMap$SimpleEntry",
    "java/util/AbstractMap$SimpleImmutableEntry",
    // --- FASTUTIL_OBJMAPS + FASTUTIL_OBJ2OBJ (listed unconditionally: the sketch may
    //     over-admit, and `is_fastutil_objmap` still applies the cfg) ---
    "it/unimi/dsi/fastutil/longs/Long2ObjectOpenHashMap",
    "it/unimi/dsi/fastutil/longs/Long2ObjectLinkedOpenHashMap",
    "it/unimi/dsi/fastutil/ints/Int2ObjectOpenHashMap",
    "it/unimi/dsi/fastutil/ints/Int2ObjectLinkedOpenHashMap",
    "it/unimi/dsi/fastutil/objects/Object2ObjectArrayMap",
    "it/unimi/dsi/fastutil/objects/Object2ObjectOpenHashMap",
    "it/unimi/dsi/fastutil/objects/Object2ObjectLinkedOpenHashMap",
    // --- owners named inline by one of `dispatch`'s own arms ---
    "it/unimi/dsi/fastutil/objects/Reference2ObjectArrayMap",
    "com/google/common/collect/MapMakerInternalMap$InternalEntry",
    "com/google/common/collect/MapMaker",
    "com/google/common/collect/ImmutableMap",
    "com/google/common/collect/ImmutableSortedMap",
    "com/google/common/collect/Maps",
];

/// 2048 buckets — see [`MAP_OWNER_SKETCH`] for why this number and not a smaller one.
const SKETCH_WORDS: usize = 32;
const SKETCH_BUCKETS: usize = SKETCH_WORDS * 64;

/// `(length, last four bytes)` mixed into a bucket index.
///
/// The TAIL is what carries the information: every name here is a package path under `java/util/`,
/// `com/google/common/collect/` or `it/unimi/dsi/fastutil/…/`, so the first twenty-odd bytes are
/// shared by design and the leading bytes discriminate nothing. Four tail bytes rather than two
/// because three of the eleven `java/util` map names are the same LENGTH and end in the same
/// three bytes (`HashMap` / `TreeMap` / `EnumMap`, all 17 characters ending `Map`) — with two
/// bytes those three shared one bucket and the filter leaked. `MIN_OWNER_LEN` is 13, so all four
/// reads are in range for any name that gets this far.
const fn owner_fp(s: &[u8]) -> usize {
    let n = s.len();
    let h = (n as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (s[n - 1] as u64).wrapping_mul(0xFF51_AFD7_ED55_8CCD)
        ^ (s[n - 2] as u64).wrapping_mul(0xC4CE_B9FE_1A85_EC53)
        ^ (s[n - 3] as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
        ^ (s[n - 4] as u64).wrapping_mul(0x2545_F491_4F6C_DD1D);
    ((h >> 45) as usize) & (SKETCH_BUCKETS - 1)
}

/// 2048 bits = **256 bytes in `.rodata`**, built by the compiler from [`MAP_OWNERS`]. Not a memo:
/// nothing is ever written to it at run time, there is nothing to invalidate, and it is the same
/// four cache lines for every thread.
///
/// Sized against the false-positive rate, which is what an over-small filter costs: 31 names in
/// 2048 buckets occupy 30 distinct ones, and over a sample of 50 non-map `java/util` /
/// `com/google/common/collect` / fastutil owners exactly one gets through to the exact scan
/// behind it. At 256 bits it was nine of fifty, and they included `java/util/Objects` and
/// `java/util/ArrayList` — precisely the declining traffic the filter exists to turn away.
const MAP_OWNER_SKETCH: [u64; SKETCH_WORDS] = {
    let mut m = [0u64; SKETCH_WORDS];
    let mut i = 0;
    while i < MAP_OWNERS.len() {
        let h = owner_fp(MAP_OWNERS[i].as_bytes());
        m[h >> 6] |= 1u64 << (h & 63);
        i += 1;
    }
    m
};

/// Shortest listed name (13 — `java/util/Map`). One length test in front of `owner_fp` is what
/// makes its four tail reads unconditionally in range.
const MIN_OWNER_LEN: usize = {
    let mut best = usize::MAX;
    let mut i = 0;
    while i < MAP_OWNERS.len() {
        let n = MAP_OWNERS[i].len();
        if n < best {
            best = n;
        }
        i += 1;
    }
    best
};

/// `false` PROVES `owner` is on no list this module keeps. `true` proves nothing — the caller
/// still runs its exact comparison.
#[inline]
pub(crate) fn map_owner_maybe(owner: &str) -> bool {
    let b = owner.as_bytes();
    if b.len() < MIN_OWNER_LEN {
        return false;
    }
    let h = owner_fp(b);
    (MAP_OWNER_SKETCH[h >> 6] >> (h & 63)) & 1 != 0
}

/// DEFAULT-ON. `RAVA_NO_FAST_MAPDISPATCH=1` (or the tree-wide `RAVA_NO_EMIT_MORE=1`) restores the
/// historic order everywhere this round touched: root the arguments and walk the whole owner
/// ladder before finding out there was nothing to root them for, materialise a fastutil ArrayMap
/// before reading the method name that decides whether the copy is wanted, and scan every literal
/// on every `is_native_map`. Both arms read the same literal tables, so the switch is a
/// performance switch and never a semantic one.
///
/// `RAVA_MAPGET_CENSUS` turns it off too, and that is not a courtesy: the census counts the
/// receivers of every `get` that REACHES `map::dispatch`, and declining a call earlier would
/// silently shrink the population it is there to describe. Folding it in here keeps the
/// admission test one flag read rather than two on the path it exists to shorten.
pub(crate) fn fast_mapdispatch_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !crate::flags::emit_more_disabled()
            && std::env::var_os("RAVA_NO_FAST_MAPDISPATCH").is_none()
            && std::env::var_os("RAVA_MAPGET_CENSUS").is_none()
    })
}

/// Is `owner` one of the names an owner-keyed arm of `map::dispatch` matches?
#[inline]
pub(crate) fn map_owner_known(owner: &str) -> bool {
    map_owner_maybe(owner) && MAP_OWNERS.contains(&owner)
}

/// ⭐**CAN THIS CALL BE SERVED AT ALL — asked WITHOUT touching the arguments' contents.**
///
/// A strict SUPERSET of the disjunction of every arm in `map::dispatch`; it may only ever
/// over-admit, in which case the call proceeds exactly as it did before this function existed.
/// The three ways in:
///
/// * an ARENA receiver on one of the two receiver-shaped routes that never look at the owner —
///   `arena_map::dispatch` (only when `RAVA_ARENA_MAPS` is on, which is where its own `enabled()`
///   is read) and the fastutil packed-array `get`. Both gates are read here in the same order the
///   arms read them, so this admits exactly what they can serve and nothing more; in particular a
///   `Objects.equals(arenaObj, x)` — a hot boot call whose owner no arm here matches — is NOT
///   admitted by the mere presence of an arena argument.
/// * one of the four METHOD names whose arms key on a prefix or a substring of the owner
///   (`create` on any `com/google/common/collect/*`, fastutil's `synchronize`/`unmodifiable`
///   wrappers, `fastIterable`) — each of those arms already tests the name first.
/// * an owner on [`MAP_OWNERS`].
#[inline]
pub(crate) fn map_call_admissible(owner: &str, name: &str, args: &[Value]) -> bool {
    arena_receiver_route(name, args)
        || matches!(name, "create" | "synchronize" | "unmodifiable" | "fastIterable")
        || map_owner_known(owner)
}

/// The two receiver-shaped routes, with THEIR OWN gates read in THEIR OWN order — so this
/// answers `true` for exactly the calls those two arms can serve.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn arena_receiver_route(name: &str, args: &[Value]) -> bool {
    match args.first() {
        Some(Value::Obj(p)) => {
            (name == "get" && r2omap_native_enabled())
                || (crate::arena_map::enabled() && crate::arena_map::is_arena_map(*p))
        }
        _ => false,
    }
}

/// Without the feature no `Value::Obj` is ever constructed, so neither arm exists.
#[cfg(not(feature = "next-gen-gc"))]
#[inline]
fn arena_receiver_route(_name: &str, _args: &[Value]) -> bool {
    false
}

/// Map a fastutil-specific method name to its JDK-Map equivalent so the single
/// `map_method` serves both (long2ObjectEntrySet→entrySet, getLongKey→getKey). The
/// primitive-key overloads (get/put/remove/containsKey with a `long` arg) already
/// work — the key is args[1] and key_str encodes it.
pub(crate) fn fastutil_alias(name: &str) -> &str {
    match name {
        "long2ObjectEntrySet" | "int2ObjectEntrySet" | "object2ObjectEntrySet" => "entrySet",
        "getLongKey" | "getIntKey" => "getKey",
        _ => name,
    }
}

/// Map lookup key honouring Java equals/hashCode for value objects. `key_str`
/// falls back to identity for an object with no synthetic $name/$str/$path
/// field, so two equal-but-distinct value objects (e.g. a ResourceLocation with
/// the same namespace+path) miss — which broke EVERY registry/tag lookup keyed
/// by Identifier ("missing references"). Here, such an object is keyed by its
/// real toString() (correct for Identifier/ResourceKey/records, whose toString
/// is injective over equality), memoised on `$keystr` since value-object keys
/// are immutable.
/// Whether `class` keys by VALUE (a substantive `equals` override) rather than
/// identity. `resolve_method` walks to `java/lang/Object.equals` (which has
/// bytecode), so a non-Object declaring class is necessary but not sufficient:
/// snakeyaml's `Node` makes `equals`/`hashCode` `final` yet routes both straight
/// to `Object` (identity semantics) — keying it by `toString()` merged every
/// equal-content node (so two `true` scalars shared one anchor → self-referential
/// config YAML) and recursed on cyclic nodes. Treat such a trivial
/// `return super.equals(o)` delegate as identity too; real value types
/// (Identifier, ResourceKey, records) keep their `toString()` value key.
pub(crate) fn has_value_equality(vm: &mut Vm, class: &str) -> bool {
    matches!(equals_kind(vm, class), EqualsKind::Value)
}

/// What a class's own `equals` BYTECODE says about its equality semantics.
///
/// `has_value_equality` collapses the last two into `false`, which is right for a
/// map key (both mean "do not key by toString"). A COLLECTION element needs them
/// apart: `Unknown` is a natively-served JDK class whose `equals` this VM never
/// loaded — `java.io.File`, `java.nio.charset.Charset` — and those are keyed by the
/// synthetic `$path`/`$name` the natives stamp on them, which IS their value
/// identity. `Identity` is a class whose bytecode was read and says `this == that`,
/// and it must never be merged no matter what text it carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EqualsKind {
    /// A substantive `equals` override: compares by content.
    Value,
    /// Resolved to `Object.equals`, or a trivial `return super.equals(o)` delegate.
    Identity,
    /// No `equals` bytecode is resolvable for this class — nothing was proven.
    Unknown,
}

/// MEMOISED, because the answer is a property of the CLASS FILE and nothing else:
/// resolving `equals` walks the super chain and then reads a method body, and
/// collections now ask this per ELEMENT per scan (`stream::ekey_mut`), where the
/// old key was one `display()`. Classes are never redefined in this engine, so a
/// name is a permanent key; the table is per-thread to stay off any lock on the
/// element path.
thread_local! {
    static EQKIND_MEMO: std::cell::RefCell<crate::FastMap<String, EqualsKind>> =
        std::cell::RefCell::new(crate::FastMap::default());
}

/// The memoised answer for `class`, or `None` if it has never been derived.
///
/// ⭐**EXISTS SO THE CALLER NEED NOT BUILD A `String` TO ASK.** `equals_kind` takes `&mut Vm`, so
/// its callers reached for `Vm::value_class_name`, which ALLOCATES a class-name `String` on every
/// call and then hashes all 44 characters of it. On `bench/ShapeOr.java` that is the whole of the
/// `Objects.equals` fast path's cost: skipping the block entirely
/// (`RAVA_NO_EQUALS_KIND_FASTPATH=1`) measured **-19.7%**, and a size bound on the KEY
/// DERIVATION — the thing the block was assumed to be paying for — measured **zero**. The
/// derivation was never the cost; asking which derivation to run was.
pub(crate) fn equals_kind_peek(class: &str) -> Option<EqualsKind> {
    EQKIND_MEMO.with(|m| m.borrow().get(class).copied())
}

pub(crate) fn equals_kind(vm: &mut Vm, class: &str) -> EqualsKind {
    use EQKIND_MEMO as MEMO;
    // The lookup is its OWN statement: holding the `RefCell` borrow across the
    // derivation below would re-enter it through the recursive `equals` resolve.
    let hit = MEMO.with(|m| m.borrow().get(class).copied());
    if let Some(k) = hit {
        return k;
    }
    let kind = equals_kind_uncached(vm, class);
    MEMO.with(|m| m.borrow_mut().insert(class.to_string(), kind));
    kind
}

/// ⭐**THE JDK'S OWN `equals` CONTRACT, FOR THE CLASSES WHOSE BYTECODE THIS ENGINE NEVER
/// LOADS.**
///
/// [`equals_kind_uncached`] decides from the class's `equals` BODY, which is the right evidence
/// and is unavailable for a JDK class the native layer serves: `resolve_method` finds nothing,
/// the answer is `Unknown`, and `Unknown` keys the element BY IDENTITY. For `java.util.Optional`
/// that is a wrong answer to a question the JDK answers explicitly — `Optional.equals` is
/// specified as "both empty, or both present with equal values" — so two distinct
/// `Optional.of(5)` were distinct collection elements: `values().contains(Optional.of(7))` was
/// FALSE against a map holding exactly that, and `values().remove(Optional.of(5))` removed
/// nothing and reported false (bench/CompSem.java `values.remove`, bench/ValRem probe — on a
/// plain `HashMap` as much as on a fastutil one, so this is the element contract and not the map).
///
/// Only the four `Optional` shapes are listed. This is a JDK CONTRACT table, not a name route
/// for guest code: every entry is a `java/util/*` class whose `equals` semantics the
/// specification fixes and whose body we cannot read, and the rule that forbids routing by name
/// is about GUEST classes. A guest class always has bytecode here, so it never needs an entry —
/// which is also why the list cannot grow to cover one by accident.
///
/// `RAVA_NO_JDK_VALUE_EQ=1` drops the table, restoring the historic `Unknown`/identity answer.
fn jdk_value_equality(class: &str) -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_NO_JDK_VALUE_EQ").is_none()) {
        return false;
    }
    matches!(
        class,
        "java/util/Optional"
            | "java/util/OptionalInt"
            | "java/util/OptionalLong"
            | "java/util/OptionalDouble"
    )
}

fn equals_kind_uncached(vm: &mut Vm, class: &str) -> EqualsKind {
    // Asked BEFORE the resolve, because the resolve is what fails for these: they have no
    // `equals` bytecode anywhere on their chain, so the arm below could never be reached.
    if jdk_value_equality(class) {
        return EqualsKind::Value;
    }
    let resolved = match vm.resolve_method(class, "equals", "(Ljava/lang/Object;)Z") {
        Ok(Some(c)) => c,
        _ => return EqualsKind::Unknown,
    };
    if &*resolved.file.this_class == "java/lang/Object" {
        return EqualsKind::Identity;
    }
    // Identity delegate body: aload_0 aload_1 invokespecial <Object.equals> ireturn.
    if let Some(m) = resolved.file.method("equals", "(Ljava/lang/Object;)Z") {
        if let Some(code) = &m.code {
            let bc = &code.bytecode;
            if bc.len() == 6 && bc[0] == 0x2a && bc[1] == 0x2b && bc[2] == 0xb7 && bc[5] == 0xac {
                let idx = u16::from_be_bytes([bc[3], bc[4]]);
                if resolved.file.pool.member_ref(idx).is_some_and(|mr| mr.owner == "java/lang/Object") {
                    return EqualsKind::Identity;
                }
            }
        }
    }
    EqualsKind::Value
}

/// Read an array-backed fastutil map (an `*ArrayMap` as an arena `Value::Obj`) —
/// packed `key`/`value`/`size` fields — into (key, value) pairs in insertion (=
/// iteration) order. None when `p` is not an array-backed map of that layout.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn drain_arena_array_map(vm: &mut Vm, p: crate::arena::ArenaPtr) -> Option<Vec<(Value, Value)>> {
    // ⭐**USE THE MEMO THAT ALREADY EXISTS.** This ran the admission inline — a fresh heap
    // `String` for a ~50-character class name, a substring scan of it, and three by-NAME field
    // resolutions — on EVERY call, and `combo_hash_key` calls it 570,894 times in a boot
    // (490,222 folds plus 80,672 refusals). `arraymap_ids` is the identical test memoised on the
    // class id; the comment thirty lines above it already says this pattern "sat unmemoised one
    // screen earlier in the same function", and this is that screen.
    let cid = crate::nextgen::arena_class_id(p);
    let (kid, vid, sid) = arraymap_ids(vm, cid)?;
    if let (Value::Array(ka), Value::Array(va)) =
        (crate::opcodes::arena_getfield(p, kid), crate::opcodes::arena_getfield(p, vid))
    {
        let keys = vm.heap.array_values(ka).unwrap_or_default();
        let vals = vm.heap.array_values(va).unwrap_or_default();
        let n = match crate::opcodes::arena_getfield(p, sid) {
            Value::Int(s) => (s.max(0) as usize).min(keys.len()).min(vals.len()),
            _ => keys.len().min(vals.len()),
        };
        return Some((0..n).map(|i| (keys[i].clone(), vals[i].clone())).collect());
    }
    None
}

/// `(key, value, size)` field ids for an arena ArrayMap class id, or None when the class is not an
/// ArrayMap or its layout lacks one of them. Memoised per class id - see the call site for why.
#[cfg(feature = "next-gen-gc")]
/// `has_value_equality` for an ARENA class id, memoised on the id instead of on the name.
///
/// ⭐**Why this exists, measured.** `structural_key_scoped`'s value-object arm did this on EVERY
/// arena key, before anything was known about the key:
///
/// ```ignore
/// let actual = class_name(cid).map(|a| a.to_string()).unwrap_or_else(|| "java/lang/Object".into());
/// if has_value_equality(vm, &actual) { ... }
/// ```
///
/// — a fresh heap `String` for a ~50-character class name, then a lookup in `equals_kind`'s memo,
/// which is keyed by `String` and therefore hashes those fifty characters. `equals_kind` was
/// already memoised; what was not memoised was **getting to it**.
///
/// `RAVA_KEYTIME` puts key derivation at **1,500,000 calls and 2,304 ns each** on today's engine,
/// and `RAVA_KEYSTAT` says the work it is charged for almost never happens: of 3,500,000
/// `structural_key` calls, **20,766 attempt a value-object toString and 14,554 run one** — 0.6%.
/// So the fifty-character allocation and hash were paid by every key and used by one in a hundred
/// and seventy.
///
/// ⭐**And the fix is a copy of the one thirty lines below.** `arraymap_ids` memoises the identical
/// shape — name lookup, substring scan, field resolutions, all pure functions of `cid` — and its
/// own comment records that this pattern "already paid min −7.4% once in this tree". The same
/// pattern sat unmemoised one screen earlier in the same function.
///
/// Semantics are preserved exactly, including the fallback: an unnamed `cid` asks the predicate
/// about `java/lang/Object`, as the inline code did. Class ids are stable and classes are never
/// redefined, so the answer cannot go stale. `RAVA_NO_VEQ_CID_MEMO=1` restores the per-call form
/// so the change prices as a same-binary flag A/B.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn value_equality_by_cid(vm: &mut Vm, cid: u32) -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let memo_on = *ON.get_or_init(|| std::env::var_os("RAVA_NO_VEQ_CID_MEMO").is_none());
    if !memo_on {
        let actual = crate::nextgen::class_name(cid)
            .map(|a| a.to_string())
            .unwrap_or_else(|| "java/lang/Object".to_string());
        return has_value_equality(vm, &actual);
    }
    thread_local! {
        static MEMO: std::cell::RefCell<crate::FastMap<u32, bool>> =
            std::cell::RefCell::new(crate::FastMap::default());
    }
    if let Some(hit) = MEMO.with(|m| m.borrow().get(&cid).copied()) {
        return hit;
    }
    // The borrow is released before this runs: `equals_kind` resolves `equals` up the super chain
    // and can re-enter this module.
    let name = crate::nextgen::class_name(cid)
        .map(|a| a.to_string())
        .unwrap_or_else(|| "java/lang/Object".to_string());
    let ans = has_value_equality(vm, &name);
    MEMO.with(|m| {
        m.borrow_mut().insert(cid, ans);
    });
    ans
}

pub(crate) fn arraymap_ids(vm: &mut Vm, cid: u32) -> Option<(u32, u32, u32)> {
    thread_local! {
        static MEMO: std::cell::RefCell<crate::FastMap<u32, Option<(u32, u32, u32)>>> =
            std::cell::RefCell::new(crate::FastMap::default());
    }
    if let Some(hit) = MEMO.with(|m| m.borrow().get(&cid).copied()) {
        return hit;
    }
    let ans = (|| {
        let cls = crate::nextgen::class_name(cid)?;
        if !cls.contains("ArrayMap") {
            return None;
        }
        let kid = vm.resolve_field_id(&cls, "key");
        let vid = vm.resolve_field_id(&cls, "value");
        let sid = vm.resolve_field_id(&cls, "size");
        // Same layout guard as before the memo: an off-layout `arena_getfield` PANICS.
        if crate::nextgen::slot_of(cid, kid).is_some()
            && crate::nextgen::slot_of(cid, vid).is_some()
            && crate::nextgen::slot_of(cid, sid).is_some()
        {
            Some((kid, vid, sid))
        } else {
            None
        }
    })();
    MEMO.with(|m| {
        m.borrow_mut().insert(cid, ans);
    });
    ans
}

pub(crate) fn entry_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    // ⭐**AN ARENA ENTRY ANSWERS THE SAME THREE METHODS.** `Map$Entry` is arena-born now
    // (`synthetic_layout`), so the receiver arrives as `Value::Obj` and the handle-only match
    // below would have declined it — sending the call down the cascade to an owner with no
    // bytecode. `synth_get`/`synth_put` serve both representations, which is exactly why the
    // migration is three arms and not a rewrite of the map layer.
    #[cfg(feature = "next-gen-gc")]
    if let Some(recv @ Value::Obj(_)) = args.first() {
        let recv = recv.clone();
        match name {
            "<init>" => {
                if let Some(k) = args.get(1).cloned() { vm.synth_put(&recv, "$key", k); }
                if let Some(v) = args.get(2).cloned() { vm.synth_put(&recv, "$val", v); }
                return Ok(Some(None));
            }
            "getKey" => return Ok(Some(Some(vm.synth_get(&recv, "$key")))),
            "getValue" => return Ok(Some(Some(vm.synth_get(&recv, "$val")))),
            _ => {}
        }
    }
    let obj = match args.first() {
        Some(Value::Ref(h)) => *h,
        _ => return Ok(None),
    };
    let ret = match name {
        "<init>" => {
            if let Some(k) = args.get(1).cloned() { vm.heap.put_field_syn(obj, crate::heap::Syn::Key, k); }
            if let Some(v) = args.get(2).cloned() { vm.heap.put_field_syn(obj, crate::heap::Syn::Val, v); }
            return Ok(Some(None));
        }
        "getKey" => vm.heap.get_field_syn(obj, crate::heap::Syn::Key),
        "getValue" => vm.heap.get_field_syn(obj, crate::heap::Syn::Val),
        "setValue" => {
            // An entry of an unmodifiable map refuses setValue, and it refuses it
            // BEFORE writing anything — ours wrote the entry's own `$val` first and
            // then wrote through, so a refusal after the fact would still have
            // mutated the snapshot.
            let kind = crate::immutable::kind_of(vm, obj);
            if kind != crate::immutable::NONE
                && crate::immutable::refuses(kind, crate::immutable::Op::Write, true)
            {
                return Err(crate::immutable::uoe(vm));
            }
            let old = vm.heap.get_field_syn(obj, crate::heap::Syn::Val);
            vm.heap.put_field_syn(obj, crate::heap::Syn::Val, args.get(1).cloned().unwrap_or(Value::Null));
            // WRITE-THROUGH. `Map.Entry.setValue` on an entry obtained from
            // entrySet() updates the BACKING map (`$entryowner`, stamped only by
            // that view). Without it `for (e : m.entrySet()) e.setValue(...)` —
            // the canonical in-place map update — mutated a snapshot and the map
            // kept its old values.
            if let Value::Ref(owner) = vm.heap.get_field(obj, "$entryowner") {
                let mid = map_id(vm, owner);
                let k0 = vm.heap.get_field_syn(obj, crate::heap::Syn::Key);
                // May run guest `toString`/`equals` (salted derivation) and so
                // allocate: re-read key AND value from the entry's heap fields
                // afterwards, which the collector forwards in place.
                let ks = value_map_key_in(vm, mid, &k0)?;
                let k = vm.heap.get_field_syn(obj, crate::heap::Syn::Key);
                let v = vm.heap.get_field_syn(obj, crate::heap::Syn::Val);
                vm.heap.map_put(mid, ks, k, v);
                // The map changed → its memoised structural key is stale.
                vm.heap.put_field_syn(owner, crate::heap::Syn::SKey, Value::Null);
            }
            old
        }
        // Map.Entry.equals/hashCode are defined by the KEY and VALUE, not by
        // entry identity — two entries of equal maps must match, which is what
        // makes `m1.entrySet().equals(m2.entrySet())` (and Map.equals through it)
        // work at all.
        "hashCode" => {
            let k = vm.heap.get_field_syn(obj, crate::heap::Syn::Key);
            let v = vm.heap.get_field_syn(obj, crate::heap::Syn::Val);
            Value::Int(entry_hash(vm, &k, &v))
        }
        "equals" => {
            let eq = match args.get(1) {
                Some(Value::Ref(o)) if *o == obj => true,
                Some(Value::Ref(o)) if vm.heap.class_of(*o).is_some_and(is_entry_class) => {
                    let (ak, av) = (vm.heap.get_field_syn(obj, crate::heap::Syn::Key), vm.heap.get_field_syn(obj, crate::heap::Syn::Val));
                    let (bk, bv) = (vm.heap.get_field_syn(*o, crate::heap::Syn::Key), vm.heap.get_field_syn(*o, crate::heap::Syn::Val));
                    structural_key(vm, &ak) == structural_key(vm, &bk)
                        && structural_key(vm, &av) == structural_key(vm, &bv)
                }
                _ => false,
            };
            Value::Int(eq as i32)
        }
        "toString" => {
            let k = vm.heap.get_field_syn(obj, crate::heap::Syn::Key);
            let v = vm.heap.get_field_syn(obj, crate::heap::Syn::Val);
            Value::Str(format!("{}={}", k.display(), v.display()).into())
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(ret)))
}

/// The one thing a compile-time membership filter can get wrong is DRIFT: a name added to
/// `MAP_CLASSES` (or either fastutil list) and not to `MAP_OWNERS` would make `map_owner_maybe`
/// answer `false` for a class the map layer really does serve, and the call would silently fall
/// through to bytecode that reads a `$map` nobody filled. Re-derive the containment here so that
/// mistake fails a test rather than a boot.
#[cfg(test)]
mod owner_sketch_tests {
    use super::*;

    #[test]
    fn map_owners_covers_every_served_name() {
        for c in MAP_CLASSES
            .iter()
            .chain(ENTRY_CLASSES.iter())
            .chain(FASTUTIL_OBJMAPS.iter())
            .chain(FASTUTIL_OBJ2OBJ.iter())
        {
            assert!(MAP_OWNERS.contains(c), "{c} is served but missing from MAP_OWNERS");
        }
    }

    #[test]
    fn sketch_admits_everything_it_was_built_from() {
        for c in MAP_OWNERS {
            assert!(map_owner_maybe(c), "{c} is in MAP_OWNERS but its own sketch rejects it");
            assert!(map_owner_known(c), "{c} is in MAP_OWNERS but map_owner_known rejects it");
        }
    }

    /// `is_native_map`'s filtered arm must agree with the raw scan it stands in front of, both
    /// for names it serves and for the `net/minecraft/*` traffic the filter exists to reject.
    #[test]
    fn native_map_filter_agrees_with_the_scan() {
        let cases = [
            "java/util/HashMap",
            "com/google/common/collect/MapMakerInternalMap",
            "it/unimi/dsi/fastutil/longs/Long2ObjectOpenHashMap",
            "it/unimi/dsi/fastutil/objects/Object2ObjectArrayMap",
            "java/util/Objects",
            "java/util/ArrayList",
            "java/util/Random",
            "java/util/Optional",
            "net/minecraft/world/level/block/state/BlockState",
            "com/google/common/collect/ImmutableList",
            "",
            "a",
        ];
        for c in cases {
            let raw = MAP_CLASSES.contains(&c) || is_fastutil_objmap(c);
            assert_eq!(raw, crate::map::is_native_map(c), "is_native_map disagrees for {c}");
        }
    }
}

/// [`GUAVA_IMMUTABLE_PROVIDERS`] is a hand-listed union — Rust cannot const-concat slices, so
/// the provider rows necessarily re-appear as literals here. The union is drift-guarded the same
/// way `MAP_OWNERS` is: every provider row that matches the historic admission shape must be
/// listed, every listed member must satisfy the shape, and no member may be listed twice. A name
/// added to any provider table and not here fails this test, not a boot census.
#[cfg(test)]
mod guava_immutable_tests {
    use super::*;

    /// The historic admission shape `jit_native_served`'s literal arm used — package prefix
    /// plus "Immutable" anywhere in the simple name. The TEST may query the shape; the FACT
    /// may not.
    fn shape(c: &str) -> bool {
        c.starts_with("com/google/common/collect/") && c.contains("Immutable")
    }

    /// The four sources the union folds, as ITERABLES over their provider rows: the copyOf
    /// owners, the arena-entry classes, `MAP_CLASSES`, and `stream::OWNER_LITERALS`.
    fn sources() -> [Vec<&'static str>; 4] {
        [
            crate::map::GUAVA_IMMUTABLE_COPYOF_OWNERS.to_vec(),
            crate::vm::fu_fastpath::GUAVA_IMMUTABLE_ENTRY_CLASSES.to_vec(),
            MAP_CLASSES.to_vec(),
            crate::stream::OWNER_LITERALS.to_vec(),
        ]
    }

    /// Every provider row that matches the historic shape must be a union member — a row
    /// added to a provider table and not to the union would leave the FACT admitting less
    /// than the tables it stands for.
    #[test]
    fn every_shaped_provider_row_is_unioned() {
        for src in sources() {
            for c in src {
                if shape(c) {
                    assert!(
                        GUAVA_IMMUTABLE_PROVIDERS.contains(&c),
                        "{c} matches the shape but is missing from GUAVA_IMMUTABLE_PROVIDERS"
                    );
                }
            }
        }
    }

    /// Every union member satisfies the historic shape AND belongs to at least one provider
    /// table — the bit must stay a subset of the retained literal, and no member may be
    /// invented outside a real provider.
    #[test]
    fn every_union_member_is_shaped_and_sourced() {
        for c in GUAVA_IMMUTABLE_PROVIDERS {
            assert!(shape(c), "{c} is unioned but does not match the historic shape");
            assert!(
                sources().iter().any(|src| src.contains(c)),
                "{c} is unioned but appears in no provider table"
            );
        }
    }

    /// The union is DEDUPLICATED: a class listed by two provider tables (ImmutableMap and
    /// ImmutableBiMap are in both the copyOf owners and the stream literals) is ONE row.
    #[test]
    fn the_union_has_no_duplicate_rows() {
        for (i, c) in GUAVA_IMMUTABLE_PROVIDERS.iter().enumerate() {
            assert!(
                !GUAVA_IMMUTABLE_PROVIDERS.iter().skip(i + 1).any(|o| o == c),
                "{c} is listed twice in GUAVA_IMMUTABLE_PROVIDERS"
            );
        }
    }
}

/// [`FORCE_NATIVE_MAP_CLASSES`] is a hand-listed provider table beside [`MAP_CLASSES`], so
/// the same drift discipline applies: every member must be a `MAP_CLASSES` row (the native
/// map layer must actually serve it), the production gate must route exactly the members
/// (its guava arm is the fact alone), and no member may be listed twice. A name added
/// here and not to `MAP_CLASSES` fails this test, not a boot.
#[cfg(test)]
mod force_native_map_tests {
    use super::*;

    /// Every member is a `MAP_CLASSES` row — the native map layer must completely supply the
    /// class before the force-native gate may route every call on it away from bytecode.
    #[test]
    fn every_member_is_a_map_classes_row() {
        for c in FORCE_NATIVE_MAP_CLASSES {
            assert!(
                MAP_CLASSES.contains(c),
                "{c} is force-native but no MAP_CLASSES row — the map layer cannot serve it"
            );
        }
    }

    /// The production gate routes exactly the members: `force_native_collection`'s guava
    /// arm is the exact fact, so a provider row the gate misses would leave a
    /// completely-supplied class running bytecode that mis-executes here, and a nested
    /// name the gate admitted would be a silent wrong-execution — the nested classes are
    /// born only inside HashBiMap bytecode that cannot execute while the exact class
    /// force-routes, so none is a provider.
    #[test]
    fn the_gate_routes_exactly_the_members() {
        let vm = crate::Vm::new(crate::class_loader::ClassLoader::new());
        for c in FORCE_NATIVE_MAP_CLASSES {
            assert!(
                crate::opcodes::force_native_collection(&vm, c, "get", "(Ljava/lang/Object;)Ljava/lang/Object;"),
                "{c} is force-native but the gate does not route it"
            );
        }
        for c in [
            "com/google/common/collect/HashBiMap$BiEntry",
            "com/google/common/collect/HashBiMap$Itr",
            "com/google/common/collect/HashBiMap$Inverse",
            "com/google/common/collect/HashBiMap$InverseKeySet",
        ] {
            assert!(
                !crate::opcodes::force_native_collection(&vm, c, "get", "(Ljava/lang/Object;)Ljava/lang/Object;"),
                "{c} is not a provider but the gate routes it"
            );
        }
    }

    /// The table is DEDUPLICATED — a class listed twice is a copy error, not a second
    /// provider fact.
    #[test]
    fn the_table_has_no_duplicate_rows() {
        for (i, c) in FORCE_NATIVE_MAP_CLASSES.iter().enumerate() {
            assert!(
                !FORCE_NATIVE_MAP_CLASSES.iter().skip(i + 1).any(|o| o == c),
                "{c} is listed twice in FORCE_NATIVE_MAP_CLASSES"
            );
        }
    }
}
