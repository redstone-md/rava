//! Serving a fastutil/collection call off its backing store with no guest frame.

use crate::*;

/// ⭐**KILL SWITCH FOR THE WIDENED COLLECTION FAST PATH: `RAVA_NO_FAST_COLL=1`.**
///
/// Covers both halves of this round's change together — the class-word classification memo and
/// the two receiver families the memo made affordable to add — because they are one bet: the
/// families only pay if classifying them is free, and the memo only pays if it serves more than
/// it used to. It also consults `flags::emit_more_disabled()`, so `RAVA_NO_EMIT_MORE=1` bisects
/// this block along with the rest of the round in ONE run instead of one build per suspect.
///
/// With it off, `fu_kind_of` derives the verdict from the class NAME on every call exactly as it
/// did before, and the two new families classify as refusals — so the engine takes byte-identical
/// routes to the ones it took before this file changed.
#[inline]
pub(crate) fn fast_coll_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_COLL").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**KILL SWITCH FOR THE `Reference2ObjectArrayMap` `size`/`isEmpty` ARM: `RAVA_NO_REFMAP_SIZE=1`.**
///
/// Off, those two members fall through to the arm's ordinary operation match, which serves only
/// `get`/`containsKey`/`put` and refuses everything else — i.e. the byte-identical route they take
/// today, array reads and all. It also consults `flags::emit_more_disabled()` so
/// `RAVA_NO_EMIT_MORE=1` bisects this arm along with the rest of the round in ONE run.
///
/// Separate from `RAVA_NO_REFMAP` on purpose: that switch withdraws a SHIPPED path
/// (`get`/`containsKey`/`put`), and a switch that withdraws two different bets at once cannot
/// price either.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn refmap_size_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_REFMAP_SIZE").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**KILL SWITCH FOR THE STRUCTURAL RING-SIZE FAST PATH: `RAVA_NO_RING_SIZE=1`** (or the block
/// switch `RAVA_NO_EMIT_MORE=1`). Resolved once.
///
/// Serves a zero-argument `size()`/`isEmpty()` whose resolved target's own bytecode is the exact
/// 25-byte ring-buffer arithmetic (`end - start`, `+ length` when negative) — the shape every
/// fastutil `*ArrayFIFOQueue` compiles to, the lighting BFS queue's 3.2M calls a boot — with no
/// synthetic frame and no `opcodes::invoke*` gauntlet. Admission is STRUCTURAL: the resolved
/// target's body must match the byte pattern at every offset and its three `getfield`s must be
/// `I` fields of the declaring class itself; nothing names a guest class, field or package.
/// Off, every call takes the general path byte-for-byte as before.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn ring_size_enabled() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var_os("RAVA_NO_RING_SIZE").is_none() && !crate::flags::emit_more_disabled()
    });
    *ON
}

/// ⭐**KILL SWITCH FOR THIS ROUND'S REFUSAL-COST WORK: `RAVA_NO_FAST_FU2=1`.**
///
/// One switch for one thesis, and the thesis is a single sentence: **a call this file refuses
/// should cost the class-word compare and nothing else.** Everything the round changes is a step
/// on the road to that verdict — the name-signature prefilter in front of the statics block, the
/// `$map` shape probe that now runs only when the class is not already recognised, the `Optional`
/// class-name test replaced by a memo code, and the three refusal counters that were
/// unconditional atomics. Splitting them into four switches would price four halves of one route;
/// they bisect together, or with `RAVA_NO_EMIT_MORE=1`, which this also consults.
///
/// Off, every one of them takes the byte-identical route it took before this file changed: the
/// statics match runs on every arrival, the `$map` probe runs on every map-op name, `Optional` is
/// recognised by a class-name compare, the counters bump unconditionally, and the five class
/// names this round adds to `size_field_class` classify as refusals.
#[inline]
pub(crate) fn fast_fu2_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_FU2").is_none() && !crate::flags::emit_more_disabled()
    })
}

#[cfg(feature = "next-gen-gc")]
crate::diag_counter!(
    amap_fu_op,
    "price",
    "amap_fu_op",
    "arena-map HashMap get/put/getOrDefault/... answered in fu_fastpath's $map block by arena_map::dispatch"
);

/// ⭐**KILL SWITCH FOR THE `$map` BLOCK'S ARENA TWIN: `RAVA_NO_FU_AMAP=1`** (or the block switch
/// `RAVA_NO_EMIT_MORE=1`; the block's `RAVA_NO_MAP_INTRINSIC` gate sits above it as always).
/// Resolved once.
///
/// The handle twin above serves a `$map`-backed heap receiver; an arena `HashMap` is a
/// `Value::Obj` the handle arm cannot see, so without this arm every one of its map ops paid
/// the general path to reach `arena_map::dispatch` anyway (the fu-miss census: 186 K
/// `get`/`put`/`getOrDefault` arrivals a boot, `NeverCacheable`). The twin routes them one
/// crossing earlier — receiver test `is_arena_map` (exact arena class id, no name consulted),
/// answer from the same `dispatch` — so the route is shortened and the result cannot change.
///
/// ⭐**DEFAULT ON, per the house pattern** (ring/getter/SAM precedent): the arm is a
/// route-shortening to the very function the general path reaches, and an A/B prices it with
/// `RAVA_NO_FU_AMAP=1`. Off, every one of these calls takes the byte-identical general route
/// it took before this file changed. It also consults `flags::emit_more_disabled()`, and the
/// family switch `RAVA_NO_FAST_COLL` — the collections fast-path round this arm belongs to —
/// withdraws it together with the rest of the family, exactly as `arena_entry_class` does.
#[inline]
#[cfg(feature = "next-gen-gc")]
fn amap_fu_enabled() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var_os("RAVA_NO_FU_AMAP").is_none()
            && !crate::flags::emit_more_disabled()
            && fast_coll_enabled()
    });
    *ON
}

/// ⭐**KILL SWITCH FOR THE VALUE-COMPUTER CALL ROUND: `RAVA_NO_CUBECALL=1`** (or the block switch
/// `RAVA_NO_EMIT_MORE=1`). Resolved once.
///
/// ON (the default), the three places that identified `CubePointRange` BY NAME stop doing so:
///
/// * `fu_kind_slow` no longer classifies the name as `fk::CUBE_RANGE` in either representation.
///   An ARENA receiver whose class the birth side structurally admitted as a pure value-computer
///   (`Vm::value_computer_verdict`, `vm/native_backing.rs`) classifies as `fk::VALUE_COMPUTER`,
///   which is COUNTED and DECLINED — the compiled accessor runs through `jit_direct_call`, and
///   that crossing is what writes the receiver profile the machine-code arm needs;
/// * `jit_run::prewarm_inline_field_ids` no longer resolves `CubePointRange.parts` by name, so
///   `CUBE_PARTS_FID` and `CUBE_CLASS_WORD` stay zero and the hand-written handle cube shape in
///   `jit/emit/iterator_inline.rs` is not emitted at all;
/// * the `getDouble`/`size` arms' ARENA edge probes immutable exact-bytecode linkage by the full
///   `(selector, receiver class-id)` key. The first callback publishes it and already-compiled
///   callers observe it immediately.
///
/// OFF, every one of those takes the byte-identical route it took before: the name compares, the
/// `parts` resolve, the `i / parts` handle shape and the `CubeRange` arm below.
#[inline]
pub(crate) fn cubecall_enabled() -> bool {
    #[cfg(feature = "next-gen-gc")]
    {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            std::env::var_os("RAVA_NO_CUBECALL").is_none()
                && !crate::flags::emit_more_disabled()
        })
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        false
    }
}

/// `[CUBECALL]` — arena value-computer receivers that reached `fu_fastpath` and were DECLINED to
/// the compiled accessor (the callback half of the population the inline arm serves). A plain
/// cell, `load` + `store`, never a locked RMW: this is the `valuecomputer::bump` discipline.
#[cfg(feature = "next-gen-gc")]
pub(crate) static VC_CALLBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn vc_callback() -> u64 {
    #[cfg(feature = "next-gen-gc")]
    {
        VC_CALLBACK.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(feature = "next-gen-gc"))]
    {
        0
    }
}

/// ⭐**THE REFUSAL COUNTERS WERE THREE UNCONDITIONAL ATOMIC RMWs ON THE LARGEST POPULATION IN
/// THE ENGINE.**
///
/// `FU_WHY[0..3]` has exactly one consumer — `lane_probe`'s periodic dump, which prints nothing
/// unless `RAVA_LANE_PROBE` is set — yet **7.9 M refusals a boot** each executed a `lock xadd` on
/// one of three shared statics. That is not a counter's `mov`: a relaxed atomic is a hoisting and
/// reordering barrier, which is why this tree already measured **−15% from removing ONE relaxed
/// atomic** off the interpreter's push path, and why the neighbouring `jit_fu_invoke` collapsed
/// four disarmed diagnostic gates into a single cached `fu_call_diag()` for this very population.
///
/// This is that gate: one `OnceLock` read of a question fixed before `main` ran. Armed, the
/// counters bump exactly as they always did, so an operator running `RAVA_LANE_PROBE` or
/// `RAVA_DIAG` sees every event; disarmed — the default — a refusal writes to no shared line at
/// all. It also reads true when the round's kill switch is off, so `RAVA_NO_FAST_FU2=1` restores
/// the unconditional bumps.
#[inline]
fn fu_why_armed() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !fast_fu2_enabled() || crate::lane_probe::enabled() || crate::diag::any_armed()
    })
}

/// The classification verdict as a STORABLE byte.
///
/// ⚠A `FuKind` cannot be the memo's value type: it lives in `lib.rs` (outside this round's file
/// set), it has no stable discriminant, and it has no room for the two families added here. These
/// are the bytes the table holds; the tail of `fu_fastpath` converts the ones that correspond to
/// a `FuKind` back, so every pre-existing arm is reached by the same `if let` it always was.
///
/// The three REFUSAL codes are the point of the whole exercise: a refusal is a verdict like any
/// other, so it is memoised like any other, and the second call from a refused class costs one
/// multiply, one load and one compare.
#[allow(dead_code)]
mod fk {
    pub const REFUSE_REF: u8 = 0;
    pub const REFUSE_OBJ: u8 = 1;
    pub const REFUSE_OTHER: u8 = 2;
    pub const ITER: u8 = 3;
    pub const IMM_LIST: u8 = 4;
    pub const ARR_LIST: u8 = 5;
    pub const D_IMM_LIST: u8 = 6;
    pub const D_ARR_LIST: u8 = 7;
    pub const LIST: u8 = 8;
    pub const ENTRY: u8 = 9;
    pub const CUBE_RANGE: u8 = 10;
    pub const REF_ARRAY_MAP: u8 = 11;
    /// An ARENA object whose `getKey`/`getValue` are one cell load each off REAL declared
    /// `key`/`value` fields. `f0`/`f1` carry the two resolved field ids, so the arm itself
    /// resolves nothing.
    pub const ARENA_ENTRY: u8 = 12;
    /// A fastutil collection whose logical length IS its `int size` field; `f0` carries the id.
    pub const SIZE_FIELD: u8 = 13;
    /// ⚠**NOT STORABLE.** The shape matched but a field id had no slot in this receiver's layout
    /// *yet*. `nextgen::slot_of` caches only POSITIVE answers for exactly this reason — a layout
    /// is only ever appended to, so a `None` may become a `Some` — and memoising this verdict
    /// would refuse the class for the life of the thread. Counted as an arena refusal, re-asked
    /// next call.
    pub const REFUSE_UNCACHED: u8 = 14;
    /// ⭐**`java/util/Optional`, RECOGNISED BY THE CLASS WORD INSTEAD OF BY ITS NAME.**
    ///
    /// The `Optional` reader block below used to answer "is this an Optional?" with a
    /// `heap.class_of` (or, on the arena, `class_name` — a thread-local mirror lookup plus an
    /// `Arc` clone and drop) followed by an 18-byte string compare, **on every arrival whose
    /// member is one of `get`/`isEmpty`/`isPresent`/`orElse`/`orElseThrow`**. `isEmpty()Z` is one
    /// of the three hottest names in the boot and lands on lists, sets and maps far more often
    /// than on an `Optional`, so that test was overwhelmingly a compare that said no.
    ///
    /// It is the same question the classification already asks, on the same receiver, one line
    /// later — so it is asked once per class per thread and carried in the memo like every other
    /// verdict. Neither representation is served here; the block still reads the fields.
    pub const OPTIONAL: u8 = 15;
    /// ⭐**AN ARENA RECEIVER WHOSE CLASS THE BIRTH SIDE STRUCTURALLY ADMITTED AS A PURE
    /// VALUE-COMPUTER** (`Vm::value_computer_verdict == ADMIT`: own `size()I` + indexed accessor
    /// bytecode, primitive-only fields, abstract fieldless chain to a native-backed base). Not
    /// served here — its accessors are its OWN bytecode, and the only honest answer from the host
    /// is to run that bytecode, which the general path already does through `jit_direct_call`
    /// (compiled body, and the crossing writes the receiver profile the inline arm reads). The
    /// verdict exists to COUNT that population and to memoise the decision as one compare.
    /// Replaces the `CubePointRange` NAME arm under `cubecall_enabled()`.
    pub const VALUE_COMPUTER: u8 = 16;
    /// ⭐**A RECEIVER WHOSE RESOLVED `size()` BODY IS THE EXACT RING-BUFFER SHAPE** —
    /// `end - start`, plus `length` when that is negative — with all three fields `I` and
    /// declared on the size-owner class itself. The ring arm keeps its own memo (`ring_memo`,
    /// keyed by the (receiver class, resolved target) PAIR rather than the class alone —
    /// the same class carries other `()I` selectors this arm must refuse), so it needs no
    /// `fk` code; this documents that the verdict exists in the same design space.
    pub const RING_SIZE: u8 = 17;
}

/// Is this verdict a refusal — i.e. did classification recognise NOTHING about the receiver?
///
/// The `$map` shape probe below is gated on it, so it is worth being exact about what it claims.
/// A `false` means the class word matched one of the exact class names enumerated in
/// `fu_kind_slow` / `arena_entry_class` / `size_field_class`: a fastutil int or double list, a
/// `java/util/ArrayList`, one of the three JDK entry classes, a native `$elems`/`$pos` iterator,
/// a `CubePointRange`, a `Reference2ObjectArrayMap`, a guava/gson map entry, a fastutil set whose
/// length is its `size` field, or `java/util/Optional`. **Not one of those carries a `$map`** —
/// the engine writes that synthetic field only into the objects its own map layer births, and
/// `Reference2ObjectArrayMap` is on this list precisely because it carries packed key/value
/// arrays *instead* of one (the `$map` block's own comment says so).
///
/// `REFUSE_UNCACHED` counts as a refusal: it is an arena shape whose field id had no slot in the
/// layout yet, and the probe it gates requires a `Value::Ref` anyway.
#[inline]
fn is_refusal(code: u8) -> bool {
    matches!(code, fk::REFUSE_REF | fk::REFUSE_OBJ | fk::REFUSE_OTHER | fk::REFUSE_UNCACHED)
}

/// ⭐**THE NAMES THE STATICS/UNWRAPPER BLOCK CAN SERVE, AS ONE 24-BIT INTEGER.**
///
/// `fu_fastpath` opens with a `match (name, desc)` of fifteen member names — the box unwrappers,
/// `Class.cast`, `Objects.equals`/`requireNonNull`, `System.identityHashCode`,
/// `String.valueOf`, `DoubleArrayList.wrap` and the three `Optional` writers. It runs on **all
/// 38.0 M arrivals**, and the traffic it is in front of is `size`, `isEmpty`, `get`, `getKey`,
/// `getValue`, `hasNext`, `next`, `getDouble`, `put`, `containsKey`, `iterator` — none of which
/// it can serve. Those pay the whole chain of `&str` compares to learn nothing, which is the same
/// waste the class-word memo removed one layer down.
///
/// The signature is `len | first byte << 8 | last byte << 16`. It is a FILTER, not an answer: a
/// hit runs the original match unchanged, so a collision costs exactly today's behaviour and can
/// never serve the wrong member. What it buys is that every hot member name above has a
/// signature outside the set — `iterator` (8,i,r) is the only near miss, against `intValue`
/// (8,i,e), and the last byte separates them — so the common arrival skips fifteen string
/// compares for one integer switch.
///
/// ⚠**THE DRIFT HAZARD IS REAL AND IS GUARDED.** A name added to the match below without a
/// signature here would go silently dead. `debug_assert` at the call site re-derives the answer
/// from the literal names, so a debug build fails loudly at the first arrival instead.
#[inline]
const fn name_sig(name: &str) -> u32 {
    let b = name.as_bytes();
    let n = b.len();
    if n == 0 {
        return 0;
    }
    (n as u32) | ((b[0] as u32) << 8) | ((b[n - 1] as u32) << 16)
}

// One `const` per admitted name so the filter below is a `match` on integers (rustc builds a
// switch) rather than a linear scan of an array.
const SIG_BOOLEAN_VALUE: u32 = name_sig("booleanValue");
const SIG_INT_VALUE: u32 = name_sig("intValue");
const SIG_SHORT_VALUE: u32 = name_sig("shortValue");
const SIG_BYTE_VALUE: u32 = name_sig("byteValue");
const SIG_CHAR_VALUE: u32 = name_sig("charValue");
const SIG_LONG_VALUE: u32 = name_sig("longValue");
const SIG_VALUE_OF: u32 = name_sig("valueOf");
const SIG_WRAP: u32 = name_sig("wrap");
const SIG_OF: u32 = name_sig("of");
const SIG_OF_NULLABLE: u32 = name_sig("ofNullable");
const SIG_EMPTY: u32 = name_sig("empty");
const SIG_REQUIRE_NON_NULL: u32 = name_sig("requireNonNull");
const SIG_IDENTITY_HASH_CODE: u32 = name_sig("identityHashCode");
const SIG_EQUALS: u32 = name_sig("equals");
const SIG_CAST: u32 = name_sig("cast");

/// Could `name` be one of the statics/unwrapper block's fifteen members? See `name_sig`.
#[inline]
fn maybe_static_member(name: &str) -> bool {
    matches!(
        name_sig(name),
        SIG_BOOLEAN_VALUE
            | SIG_INT_VALUE
            | SIG_SHORT_VALUE
            | SIG_BYTE_VALUE
            | SIG_CHAR_VALUE
            | SIG_LONG_VALUE
            | SIG_VALUE_OF
            | SIG_WRAP
            | SIG_OF
            | SIG_OF_NULLABLE
            | SIG_EMPTY
            | SIG_REQUIRE_NON_NULL
            | SIG_IDENTITY_HASH_CODE
            | SIG_EQUALS
            | SIG_CAST
    )
}

/// The same question answered from the literal names — the `debug_assert` witness for
/// `maybe_static_member`, and the ONE place a new member name has to be mirrored.
#[cfg(debug_assertions)]
fn is_static_member_name(name: &str) -> bool {
    matches!(
        name,
        "booleanValue"
            | "intValue"
            | "shortValue"
            | "byteValue"
            | "charValue"
            | "longValue"
            | "valueOf"
            | "wrap"
            | "of"
            | "ofNullable"
            | "empty"
            | "requireNonNull"
            | "identityHashCode"
            | "equals"
            | "cast"
    )
}

/// ⭐**CLASSIFY BY THE CLASS WORD, NOT BY THE CLASS NAME.**
///
/// `fu_fastpath` runs **38.0 M times a boot** and the first thing it did after unboxing the
/// receiver was a chain of up to ten `&str` compares against 18-to-49-character class names. On
/// the ARENA side it was worse: `class_name(cid)` runs first, and that is a thread-local mirror
/// lookup plus an `Arc` clone and drop — two atomics — for a name it only ever compares and
/// throws away. **7.9 M of those calls end in a REFUSAL**, having paid the whole chain to learn
/// nothing, and the file's own header already says a refusal is not free.
///
/// The key here is the same integer the EMITTED guards compare: `object_class_word` — the first
/// word of the object's interned `Arc<str>` class name, precisely what `emit_heap_ref_class_key`
/// reads out of the header — for a handle, and the arena class id for an arena object. Both are
/// stable for the life of a loaded class, so a verdict derived once is valid forever, the
/// negative verdict included. That is what makes a refusal one compare.
///
/// Direct-mapped, thread-local, no lock and no hashing of a string — the same shape as
/// `nextgen::slot_memo`, for the reason that module records: the cost in this layer is locks and
/// hashes. A collision re-derives the verdict, which is what every call did before, so a thrashing
/// table degrades to today's behaviour rather than to a wrong answer.
///
/// ⚠**TABLE SIZE: 128 slots x (8 B key + 1 B kind + 4 B f0 + 4 B f1 + 4 B f2) = 2,688 bytes**, of
/// which the 1,152 B of keys+kinds is all a refusal touches. Deliberately small: this tree measured a
/// **+6.8% regression** from a per-thread cache growing 2.5 KB -> 14 KB, and a classifier that
/// stops fitting in L1 costs more than the string compares it removes. If this ever needs more
/// rows, widen the hash before widening the table.
mod kind_memo {
    const SLOTS: usize = 128;
    /// No `Arc` pointer and no `(class_id << 1) | 1` can be `u64::MAX`, so it is a safe "empty".
    const EMPTY: u64 = u64::MAX;

    struct Table {
        key: [u64; SLOTS],
        kind: [u8; SLOTS],
        f0: [u32; SLOTS],
        f1: [u32; SLOTS],
        /// Third payload column — the ring arm's `length` field id (see `fk::RING_SIZE`); every
        /// other code stores 0 here.
        f2: [u32; SLOTS],
    }

    thread_local! {
        static T: std::cell::RefCell<Table> = const {
            std::cell::RefCell::new(Table {
                key: [EMPTY; SLOTS],
                kind: [0; SLOTS],
                f0: [0; SLOTS],
                f1: [0; SLOTS],
                f2: [0; SLOTS],
            })
        };
    }

    /// Fibonacci mix, high bits: an `Arc` pointer's low bits are allocator alignment and an
    /// arena class id's high bits are all zero, so neither end is usable raw.
    #[inline]
    fn slot(key: u64) -> usize {
        (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 57) as usize & (SLOTS - 1)
    }

    #[inline]
    pub(super) fn get(key: u64) -> Option<(u8, u32, u32, u32)> {
        let i = slot(key);
        T.with(|t| {
            let t = t.borrow();
            (t.key[i] == key).then(|| (t.kind[i], t.f0[i], t.f1[i], t.f2[i]))
        })
    }

    #[inline]
    pub(super) fn put(key: u64, kind: u8, f0: u32, f1: u32, f2: u32) {
        let i = slot(key);
        T.with(|t| {
            let mut t = t.borrow_mut();
            t.key[i] = key;
            t.kind[i] = kind;
            t.f0[i] = f0;
            t.f1[i] = f1;
            t.f2[i] = f2;
        })
    }
}

/// ⭐**THE RING ARM'S OWN MEMO — KEYED BY (RECEIVER CLASS, RESOLVED TARGET) PAIR.**
///
/// `kind_memo` answers "what is this receiver class?" and one row per class is exactly right
/// for it. The ring arm needs a stronger identity: the same receiver class can carry MANY
/// zero-argument `()I`/`()Z` selectors, and the arm may serve `size` while it must refuse
/// `capacity` (whose body is a plain field read). The pair that decides is (the receiver's
/// class, THE CLASS THE SITE'S SELECTOR RESOLVES TO) — the same resolved-dispatch identity the
/// general path computes — so that is the key here. Both halves are stable for the life of a
/// loaded class and of the process (the loader is append-only, `resolve_method` is a memoised
/// pure function), so a verdict derived once is valid forever, refusal included.
///
/// Same shape as `kind_memo` (direct-mapped, thread-local, no lock): a collision re-derives,
/// which is the general path, never a wrong answer. The `kind` byte is the MATCHED BODY
/// SHAPE'S own answer kind — the codes below — memoised so a hit serves the same kind the
/// first derivation did, refusal included.
#[cfg(feature = "next-gen-gc")]
mod ring_memo {
    const SLOTS: usize = 64;
    const EMPTY: u64 = u64::MAX;

    /// The resolved target's body matched the 25-byte ring shape: it `ireturn`s the raw
    /// computed length, so the answer is that int — whatever the site's descriptor says
    /// (`Z` is an `int` at the bytecode level, so a JVM running that body returns the
    /// same raw value, and so does this arm).
    pub(super) const RING_INT: u8 = 0;
    /// Verified structural refusal for exactly this (class, target) pair — the body is not
    /// a shape this arm emulates. The bytecode is immutable, so the verdict is permanent.
    pub(super) const REFUSAL: u8 = 1;
    /// The resolved target's body matched the 15-byte canonical delegation: it answers
    /// `size() == 0` as `iconst_0`/`iconst_1`, so the answer is the boolean 0/1 — again
    /// whatever the site's descriptor says.
    pub(super) const CANON_BOOL: u8 = 2;
    /// The resolved target's body matched the 13-byte DIRECT field-test `isEmpty`:
    /// `getfield size; ifne; 1/0; ireturn`. Answer is boolean 0/1 on the single field;
    /// `f0` holds the one derived field id, `f1`/`f2` unused.
    pub(super) const FIELD_BOOL: u8 = 3;

    struct Table {
        key: [u64; SLOTS],
        kind: [u8; SLOTS],
        f0: [u32; SLOTS],
        f1: [u32; SLOTS],
        f2: [u32; SLOTS],
    }

    thread_local! {
        static T: std::cell::RefCell<Table> = const {
            std::cell::RefCell::new(Table {
                key: [EMPTY; SLOTS],
                kind: [0; SLOTS],
                f0: [0; SLOTS],
                f1: [0; SLOTS],
                f2: [0; SLOTS],
            })
        };
    }

    #[inline]
    fn slot(key: u64) -> usize {
        (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) as usize & (SLOTS - 1)
    }

    #[inline]
    pub(super) fn get(key: u64) -> Option<(u8, u32, u32, u32)> {
        let i = slot(key);
        T.with(|t| {
            let t = t.borrow();
            (t.key[i] == key).then(|| (t.kind[i], t.f0[i], t.f1[i], t.f2[i]))
        })
    }

    #[inline]
    pub(super) fn put(key: u64, kind: u8, f0: u32, f1: u32, f2: u32) {
        let i = slot(key);
        T.with(|t| {
            let mut t = t.borrow_mut();
            t.key[i] = key;
            t.kind[i] = kind;
            t.f0[i] = f0;
            t.f1[i] = f1;
            t.f2[i] = f2;
        })
    }
}

/// (SITE, receiver-class) rows for the unspilled LEAF `isEmpty()Z` serve — see
/// `fu_isempty_leaf_serve` below for why the site half exists at all. Thread-local for the
/// same reason `ring_memo` is: the derive (`ring_size_fastpath`, under a spilled caller on
/// any thread) and the serve (`nt_leaf` off compiled code, main only — its prologue refuses
/// workers) must see the same rows, and a row written by a worker is never read (its serves
/// refuse in the prologue), so the tables may simply be per-thread.
///
/// A row is ONLY ever written at a SUCCESSFUL `ring_size_fastpath` serve — never for a
/// refusal — and stores the exact tuple that serve answered from, so the leaf serve replays
/// the identical read. The (site, class) key is two 64-bit halves because the pair memo
/// (`ring_memo`) is keyed by (class, RESOLVED TARGET) — the leaf serve cannot resolve (a
/// resolve can run `<clinit>` and collect), so it needs the SITE + RECEIVER-CLASS key it
/// can compute from pure reads alone.
#[cfg(feature = "next-gen-gc")]
mod site_memo {
    const SLOTS: usize = 512;
    const EMPTY: u64 = u64::MAX;

    /// The site half of the key: the caller class's `Arc` pointer OR'd with the constant-pool
    /// index in the high 16 bits. Both halves are stable identities of the SITE (a leaked
    /// class file and an immutable pool index), and a user-space pointer is `< 2^47` so the
    /// high bits are free — `cp` is `u16`, fitting exactly. No two (class, cp) sites collide.
    #[inline]
    pub(super) fn site_key(caller_ptr: *const crate::class_loader::LoadedClass, cp: u16) -> u64 {
        (caller_ptr as u64) | ((cp as u64) << 48)
    }

    struct Table {
        site: [u64; SLOTS],
        recv: [u64; SLOTS],
        kind: [u8; SLOTS],
        f0: [u32; SLOTS],
        f1: [u32; SLOTS],
        f2: [u32; SLOTS],
    }

    thread_local! {
        static T: std::cell::RefCell<Table> = const {
            std::cell::RefCell::new(Table {
                site: [EMPTY; SLOTS],
                recv: [EMPTY; SLOTS],
                kind: [0; SLOTS],
                f0: [0; SLOTS],
                f1: [0; SLOTS],
                f2: [0; SLOTS],
            })
        };
    }

    #[inline]
    fn slot(site: u64, recv: u64) -> usize {
        ((site ^ recv.rotate_left(31)).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 55) as usize
            & (SLOTS - 1)
    }

    /// BOTH key halves must match: a row derived for one site must not serve another, and a
    /// row derived for one receiver class must not serve another class at the same site.
    #[inline]
    pub(super) fn get(site: u64, recv: u64) -> Option<(u8, u32, u32, u32)> {
        let i = slot(site, recv);
        T.with(|t| {
            let t = t.borrow();
            (t.site[i] == site && t.recv[i] == recv)
                .then(|| (t.kind[i], t.f0[i], t.f1[i], t.f2[i]))
        })
    }

    #[inline]
    pub(super) fn put(site: u64, recv: u64, kind: u8, f0: u32, f1: u32, f2: u32) {
        let i = slot(site, recv);
        T.with(|t| {
            let mut t = t.borrow_mut();
            t.site[i] = site;
            t.recv[i] = recv;
            t.kind[i] = kind;
            t.f0[i] = f0;
            t.f1[i] = f1;
            t.f2[i] = f2;
        })
    }
}


/// The guava immutable ENTRY classes the `arena_entry_class` arm below serves — hoisted to a
/// `pub(crate)` const so [`crate::map::GUAVA_IMMUTABLE_PROVIDERS`] (the authoritative union behind
/// `FACT_GUAVA_IMMUTABLE`) lists its provider rows from the one place they are written, and the
/// drift test in `map/classes.rs` can prove the union never misses one. The flag semantics are
/// unchanged: this table answers only the guava arm's membership; the gson arm and its kill
/// switch stay inline, exactly as before.
pub(crate) const GUAVA_IMMUTABLE_ENTRY_CLASSES: &[&str] = &[
    "com/google/common/collect/ImmutableEntry",
    "com/google/common/collect/ImmutableMapEntry",
    "com/google/common/collect/ImmutableMapEntry$NonTerminalImmutableMapEntry",
    "com/google/common/collect/ImmutableMapEntry$NonTerminalImmutableBiMapEntry",
];

/// ⭐**AN ARENA MAP ENTRY WHOSE `getKey`/`getValue` ARE ONE CELL LOAD EACH.**
///
/// The engine already served this SHAPE twice — `FuKind::Entry` for the three JDK entry classes
/// (which read the engine's own synthetic `$key`/`$val`) and a gson-only arm for
/// `LinkedTreeMap$Node` (which reads real declared fields). Guava's immutable entries are the
/// same second shape and had NEITHER: they are what `ImmutableMap.entrySet()` iteration hands
/// back, they arrive at the `java/util/Map$Entry`-typed sites `fu_routable` already admits for
/// `getKey`/`getValue`, and every one of them was refused and paid the general path to reach an
/// accessor that is `*(obj + slot)`.
///
/// `key`/`value` are declared on `ImmutableEntry` and inherited by the rest, which is why the id
/// is taken with `resolve_field_id` (it walks to the DECLARING class and interns `Owner#name` —
/// the same id the layout was built with) and then verified against this receiver's layout with
/// `slot_of`. An off-layout `arena_getfield` is a PANIC, not a wrong answer, so that check is not
/// optional; it happens once per class here instead of once per call in the arm.
///
/// ⚠**The JDK entry names are deliberately absent from this list.** They are already classified
/// as `FuKind::Entry`, whose arm reads `$key`/`$val` — the fields the engine's own entry builders
/// write — and a class that is served synthetically in one representation must not be read
/// structurally in the other.
fn arena_entry_class(cls: &str) -> bool {
    // gson's node had its own arm before this round and keeps its own kill switch; it must stay
    // classified even with `RAVA_NO_FAST_COLL=1`, or that flag would withdraw a shipped path.
    if cls == "com/google/gson/internal/LinkedTreeMap$Node" {
        return true;
    }
    fast_coll_enabled() && GUAVA_IMMUTABLE_ENTRY_CLASSES.contains(&cls)
}

/// ⭐**THE FASTUTIL COLLECTIONS WHOSE LOGICAL LENGTH *IS* THEIR `int size` FIELD.**
///
/// `[STUCK]` — the boot-dispatch census of methods that are hot in the interpreter and never
/// compiled — names five one-line accessors carrying **12.87 M of the boot's 13.46 M stuck
/// interpreted entries**, three of them byte-identical thirteen-byte bodies
/// (`aload_0 getfield size:I ifne .. iconst_1 goto .. iconst_0 ireturn`):
/// `LongOpenHashSet.isEmpty` 3.83 M, `ReferenceOpenHashSet.isEmpty` 1.04 M,
/// `ObjectArrayList.isEmpty` 1.04 M.
///
/// ⛔**AND THE FIRST CURE FOR THEM MEASURED +2.03% SLOWER.** A `Flat::FieldZeroTest` shape served
/// exactly these frame-free, five of five gates bit-exact, and the boot got WORSE — because the
/// replacement read the field through the HANDLE heap: resolve the field, walk `Heap::objects`,
/// clone the receiver `Value`, which costs about what the `Frame` it removed cost. ⭐So the arm
/// this list feeds serves ONLY a `Value::Obj` receiver, where the read is a direct arena cell
/// load, and refuses the handle form — memoised, so that refusal is one integer compare.
///
/// SEMANTICS, not name inference. Every class here has, in fastutil's own source,
/// `public int size() { return size; }`, with `isEmpty()` either inherited as `size() == 0` or
/// written as `size == 0`; the field is the element count and not a capacity, and it counts the
/// null element of the hash sets. A class is on this list because its `size()` BODY was read —
/// ⚠having a field called `size` is not evidence of anything, and inferring the one from the
/// other is how a fast path returns a wrong answer. Matching is EXACT, so a guest subclass that
/// overrides either accessor gets a different class id, classifies as a refusal, and runs its own
/// body.
///
/// ⚠**BOUNDED BY WHAT CAN ACTUALLY ARRIVE.** `jit::direct::fu_routable` admits `size`/`isEmpty`
/// for `java/util/{List,Collection,AbstractList}`-typed sites and for every class under
/// `it/unimi/dsi/fastutil/{ints,doubles}/`; that admission is how a fastutil set reaches this
/// function at all. The other two `[STUCK]` rows — `LongArrayFIFOQueue.size` and
/// `PriorityQueue.isEmpty`, 6.96 M entries between them — are deliberately ABSENT: nothing routes
/// a `LongPriorityQueue`-typed site here, and an arm that cannot be reached is not a fast path,
/// it is dead code with a comment claiming a number.
///
/// ⛔**AND THE `ints/` SETS ARE EXCLUDED, THOUGH THEY ARE THE ONES MOST EASILY ROUTED HERE.**
/// `fastutil::is_int_set` re-models `IntOpenHashSet`/`IntLinkedOpenHashSet`/`IntArraySet`/the two
/// tree sets as a sorted-unique `$elems` array, because their real bytecode builds EMPTY sets at
/// our JDK boundary (`Need some octaves!`). For those objects the declared `size` field is not the
/// element count — it is whatever the bytecode last left there, and reading it would be a WRONG
/// answer rather than a slow one. The same goes for `map::is_fastutil_objmap`. Both predicates are
/// consulted here rather than trusted to stay out of the list below, so the two cannot drift apart
/// when someone extends either one.
fn size_field_class(cls: &str) -> bool {
    if crate::fastutil::is_int_set(cls) || crate::map::is_fastutil_objmap(cls) {
        return false;
    }
    // `RAVA_NO_FAST_COLL=1` withdraws the WHOLE family, the widening below included: a switch
    // that withdraws a path must withdraw all of it, or the arm it leaves behind cannot be
    // compared against its own history.
    if !fast_coll_enabled() {
        return false;
    }
    matches!(
            cls,
            "it/unimi/dsi/fastutil/objects/ObjectArrayList"
                | "it/unimi/dsi/fastutil/objects/ObjectArraySet"
                | "it/unimi/dsi/fastutil/objects/ObjectOpenHashSet"
                | "it/unimi/dsi/fastutil/objects/ObjectLinkedOpenHashSet"
                | "it/unimi/dsi/fastutil/objects/ReferenceArrayList"
                | "it/unimi/dsi/fastutil/objects/ReferenceArraySet"
                | "it/unimi/dsi/fastutil/objects/ReferenceOpenHashSet"
                | "it/unimi/dsi/fastutil/objects/ReferenceLinkedOpenHashSet"
                | "it/unimi/dsi/fastutil/longs/LongArrayList"
                | "it/unimi/dsi/fastutil/longs/LongArraySet"
                | "it/unimi/dsi/fastutil/longs/LongOpenHashSet"
                | "it/unimi/dsi/fastutil/longs/LongLinkedOpenHashSet"
                | "it/unimi/dsi/fastutil/doubles/DoubleArraySet"
                | "it/unimi/dsi/fastutil/doubles/DoubleOpenHashSet"
        )
        // ⭐**FIVE MORE, AND EVERY ONE OF THEM HAD ITS `size()` AND `isEmpty()` BYTECODE READ**
        // out of `libraries/it/unimi/dsi/fastutil/8.5.18/fastutil-8.5.18.jar` before it was added
        // — `aload_0; getfield size:I; ireturn` and the thirteen-byte `size == 0`, identical to
        // the fourteen above. They are the custom-hash and linked variants of families already on
        // the list, i.e. the same fastutil template with a different hashing strategy, and the
        // strategy is not consulted by either accessor.
        //
        // ⛔**AND THE SAME READING REJECTED TWO CANDIDATES**, which is the evidence that the rule
        // is being applied rather than recited: `ObjectRBTreeSet.size()` returns a field called
        // **`count`**, not `size`, and `ObjectImmutableList.size()` is `a.arraylength` with no
        // count field at all. Both would have resolved a field this class does not declare —
        // `slot_of` would answer `None`, the verdict would be `REFUSE_UNCACHED`, and the class
        // would be re-asked on every single call forever. Neither is here.
        //
        // Behind THIS round's switch AS WELL, so the widening can be priced apart from the
        // family it widens without either switch leaving half a path standing.
        || (fast_fu2_enabled()
            && matches!(
                cls,
                "it/unimi/dsi/fastutil/objects/ObjectOpenCustomHashSet"
                    | "it/unimi/dsi/fastutil/objects/ObjectLinkedOpenCustomHashSet"
                    | "it/unimi/dsi/fastutil/longs/LongOpenCustomHashSet"
                    | "it/unimi/dsi/fastutil/longs/LongLinkedOpenCustomHashSet"
                    | "it/unimi/dsi/fastutil/doubles/DoubleLinkedOpenHashSet"
            ))
}

impl Vm {
    /// The recognised-class fast path behind `jit_fu_invoke`. Returns `Ok(Some(cell))`
    /// when it served the call off the backing array, `Ok(None)` to fall back. The
    /// receiver class is read once into a cheap discriminant (no String allocation,
    /// and the `&str` borrow is dropped before the mutating field reads).
    #[cfg(feature = "next-gen-gc")]
    /// THE READ-ONLY FAST PATH, over `&Vm` — the one a GIL-FREE WORKER may take without
    /// the global resolve lock.
    ///
    /// `RAVA_LANE_OUTER` attributed **82,382,752 of a boot's 86,302,646 resolve-lock
    /// acquisitions** to a single line: the `WorkerGate::Locked` arm in
    /// `jit/runtime/compile.rs`. A per-kind tally said 100% of them are `fu_invoke`, and a
    /// per-MEMBER tally said 100% of THOSE are **`getDouble`** — a pure read. The ring's
    /// workers were serialising on one mutex to read an element out of an array, which is
    /// why the ring's own PC profile bills `Mutex::lock_contended` 8.49% +
    /// `resolve_lock_acquire` 6.35% + `RtlWaitOnAddress` 4.21% of live samples.
    ///
    /// The lock is not there for `$elems` publication — it is there because `vm_ref()`
    /// hands out `&mut Vm`, which two workers may not hold at once. GAP-S09 stage 2 already
    /// names the way out for exactly this case: a callback that migrates to `vm_shared()`
    /// is **proven by its signature** to only read. This is that migration.
    ///
    /// SOUNDNESS, and why it is narrower than the locked path:
    /// * `DoubleImmutableList` ONLY. Its backing array is never mutated, so a reader cannot
    ///   race a writer. `DoubleArrayList` can be mutated by another worker under the lock
    ///   and keeps the locked path — a shared read there would be a genuine data race.
    /// * Three members, all reads: `getDouble`, `size`, `isEmpty`. No iterator (advancing
    ///   `$pos` is a write), no allocation, no boxing that touches the heap.
    /// * A COLD field-id memo returns `None` rather than resolving (resolution needs
    ///   `&mut Vm`), and every `None` falls through to the unchanged locked path — so this
    ///   can only ever serve calls the locked path would have served identically.
    ///
    /// `RAVA_NO_FU_SHARED=1` disables it, so the saving is priced rather than assumed.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn fu_read_shared(&self, name: &str, desc: &str, cells: &[i64]) -> Option<i64> {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ON.get_or_init(|| std::env::var_os("RAVA_NO_FU_SHARED").is_none()) {
            return None;
        }
        // Cheapest discriminator first: three member names, before touching the receiver.
        match (name, desc) {
            ("getDouble", "(I)D")
            | ("size", "()I")
            | ("isEmpty", "()Z")
            | ("get", "(I)Ljava/lang/Object;") => {}
            _ => return None,
        }
        let recv = jit::unbox_ref(self, cells.first().copied().unwrap_or(0), "");


        // WHY IT BAILS, when it bails. The first version of this path served ZERO of the
        // 80,000,000 calls and the counters could not say which precondition failed —
        // receiver class, cold memo, or the shared Vm itself. Guessing costs a build each.
        let cls: Option<&str> = match &recv {
            Value::Ref(h) => self.heap.class_of(*h),
            _ => None,
        };
        // BOTH halves of the double family. The first version admitted only the immutable
        // one, reasoning that its backing can never be mutated — and served ZERO calls,
        // because a census of the receivers came back **100% `DoubleArrayList`**. The
        // conservative choice had excluded the entire population.
        //
        // What makes the mutable one safe is not the class, it is the guard: the caller
        // holds the resolve lock in SHARED mode for the whole read, and every mutator takes
        // it exclusively. Readers no longer serialise against each other; they are still
        // excluded from writers, which is the property the correctness rests on.
        let is_dbl = match &recv {
            Value::Ref(_) => matches!(
                cls,
                Some("it/unimi/dsi/fastutil/doubles/DoubleImmutableList")
                    | Some("it/unimi/dsi/fastutil/doubles/DoubleArrayList")
            ),
            Value::Obj(p) => matches!(
                crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)).as_deref(),
                Some("it/unimi/dsi/fastutil/doubles/DoubleImmutableList")
                    | Some("it/unimi/dsi/fastutil/doubles/DoubleArrayList")
            ),
            _ => false,
        };
        let is_dal = match &recv {
            Value::Ref(_) => cls == Some("it/unimi/dsi/fastutil/doubles/DoubleArrayList"),
            Value::Obj(p) => {
                crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)).as_deref()
                    == Some("it/unimi/dsi/fastutil/doubles/DoubleArrayList")
            }
            _ => false,
        };
        // ⭐**java.util.ArrayList JOINS THE SHARED READ, AND IT IS THE POPULATION THAT MATTERS.**
        // The lane's dominant abort is `InternalError: direct miss`, and its cause is that
        // `force_compile` cannot pre-warm a callee with no bytecode: 18 of 30 warm targets fail at
        // CLASS LOAD, and `[WARM-NOCLASS]` names them `java/util/ArrayList#get`,
        // `java/util/HashMap#values`, `java/util/ArrayList#iterator` … — the JDK classes this
        // engine serves with natives. A worker reaching one has nothing to call and tears down the
        // region.
        //
        // `size`/`isEmpty`/`get` on an `$elems`-backed ArrayList are PURE READS of the same shape
        // the double lists already take here, and the soundness argument is the one written above
        // verbatim: the reader holds the resolve lock in SHARED mode for the whole read and every
        // writer takes it EXCLUSIVELY, so readers are excluded from writers. Nothing else about
        // the class matters — that is the lesson the double-list arm already recorded when
        // admitting only the immutable half excluded the entire population.
        //
        // `get` is admitted only for a REFERENCE element: `jit::box_ref` would have to ALLOCATE a
        // box for a primitive, and allocation is a mutation this shared path must not make. A
        // primitive element simply falls through to the locked path, exactly as an out-of-bounds
        // index already does.
        let is_alist = match &recv {
            Value::Ref(_) => cls == Some("java/util/ArrayList"),
            _ => false,
        };
        if is_alist {
            let rh = match &recv { Value::Ref(h) => *h, _ => return None };
            let Value::Array(arr) = self.heap.get_field_syn(rh, crate::heap::Syn::Elems) else {
                return None;
            };
            // Spare capacity is not elements — see `Heap::elems_values`.
            let len = self.heap.elems_len(rh, arr);
            return match name {
                "size" => Some(jit::pack_local(&Value::Int(len as i32))),
                "isEmpty" => Some(jit::pack_local(&Value::Int((len == 0) as i32))),
                "get" => {
                    let i = *cells.get(1).unwrap_or(&0) as i32;
                    if i < 0 || i as usize >= len {
                        return None; // let the locked path throw the real exception
                    }
                    match self.heap.array_get(arr, i as usize) {
                        // Reference elements only — see above.
                        Some(v @ (Value::Ref(_) | Value::Obj(_) | Value::Null)) => {
                            Some(jit::pack_local(&v))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
        }
        if !is_dbl {
            if crate::lane_probe::enabled() {
                crate::gilfree_cf::note_locked_member(match &recv {
                    Value::Ref(_) => cls.unwrap_or("ref:?"),
                    Value::Obj(_) => "arena",
                    _ => "not-a-ref",
                });
            }
            return None;
        }
        let Some((h, len)) = self.fu_double_view_shared(&recv, is_dal) else {
            if crate::lane_probe::enabled() {
                crate::gilfree_cf::note_locked_member("COLD-MEMO");
            }
            return None;
        };
        match name {
            "size" => Some(jit::pack_local(&Value::Int(len as i32))),
            "isEmpty" => Some(jit::pack_local(&Value::Int((len == 0) as i32))),
            _ => {
                let i = *cells.get(1).unwrap_or(&0) as i32;
                if i < 0 || i as usize >= len {
                    return None; // out of bounds -> the locked path lets the real method throw
                }
                let v = self.heap.array_get(h, i as usize).map(|x| x.as_f64()).unwrap_or(0.0);
                Some(jit::pack_local(&Value::Double(v)))
            }
        }
    }

    /// The `parts` field of a `CubePointRange`, from either representation. `None` when the field
    /// is absent (then the general path runs and nothing changes). The id is resolved once — the
    /// same memo shape `DoubleArrayList.wrap` needed, and for the same reason.
    #[cfg(feature = "next-gen-gc")]
    fn cube_parts(&mut self, recv: &Value) -> Option<i32> {
        const OWNER: &str = "net/minecraft/world/phys/shapes/CubePointRange";
        static PID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let pid = match PID.get() {
            Some(&v) => v,
            None => {
                // â **THE OWNER-QUALIFIED ID, NOT THE BARE ONE.** The inline arm first
                // published `field_id_opt("parts")` and never emitted a single site, because
                // `resolve_field_id(OWNER, "parts")` answers a DIFFERENT id â the same
                // two-id-spaces trap the driver's own comment warns about for `$elems`. The arm
                // must be handed the id this function actually reads with.
                let v = self.resolve_field_id(OWNER, "parts");
                let _ = PID.set(v);
                crate::jit::CUBE_PARTS_FID
                    .store(v as i64 + 1, std::sync::atomic::Ordering::Relaxed);
                v
            }
        };
        let v = match recv {
            Value::Ref(h) => self.heap.get_field_by_id(*h, pid),
            Value::Obj(p) => crate::opcodes::arena_getfield(*p, pid),
            _ => return None,
        };
        match v.as_int() {
            Some(n) if n > 0 => Some(n),
            _ => None,
        }
    }

    /// Does `Class.cast(v)` on this mirror PASS? Memoised on (mirror identity, value class id)
    /// under the GC-epoch tag; only a pass is cached, so a refusal always re-runs the full
    /// three-valued test and the caller raises the identical exception. Shared by the machine-code
    /// arm and the natives-cascade arm so the two can never diverge.
    pub(crate) fn cast_passes_memoised(&mut self, mirror: &Value, v: &Value) -> bool {
        let cls_key: u64 = match v {
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => 1 << 40 | crate::nextgen::arena_class_id(*p) as u64,
            Value::Ref(h) => 2 << 40 | self.heap.object_class_word(*h).unwrap_or(0) as u64,
            Value::Str(_) => 3 << 40,
            Value::Int(_) => 4 << 40,
            Value::Long(_) => 5 << 40,
            Value::Float(_) => 6 << 40,
            Value::Double(_) => 7 << 40,
            _ => 0,
        };
        let mkey = match mirror {
            Value::Ref(h) => *h as u64,
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => 1 << 32 | crate::nextgen::arena_identity_hash(*p) as u64,
            _ => 0,
        };
        if mkey == 0 || cls_key == 0 {
            return false;
        }
        let key = (mkey << 24) ^ cls_key;
        std::thread_local! {
            static PASS: std::cell::RefCell<(u64, crate::FastMap<u64, ()>)> =
                std::cell::RefCell::new((0, crate::FastMap::default()));
        }
        let tag = crate::nextgen::gc_epoch();
        if PASS.with(|c| { let m = c.borrow(); m.0 == tag && m.1.contains_key(&key) }) {
            return true;
        }
        let sup = crate::reflect::field_str(self, Some(mirror), "$name").unwrap_or_default();
        if sup.is_empty() || crate::opcodes::instance_of_tri(self, v, &sup) == crate::opcodes::Tri::No {
            return false;
        }
        PASS.with(|c| {
            let mut m = c.borrow_mut();
            if m.0 != tag { m.0 = tag; m.1.clear(); }
            if m.1.len() >= 64 * 1024 { m.1.clear(); }
            m.1.insert(key, ());
        });
        true
    }

    /// The memoised receiver classification — one multiply, one load, one compare on a hit.
    /// Returns `(code, f0, f1)`; see `fk` for the codes and `kind_memo` for why the key is the
    /// class WORD rather than the class name.
    ///
    /// Deriving is `fu_kind_slow`, and it runs once per class per thread. Two things force it to
    /// run every time instead, both of them process constants read out of a `OnceLock`:
    /// `RAVA_NO_FAST_COLL=1`, and an ARMED refusal census — `RAVA_BACKEDGE_CENSUS` counts one row
    /// per refused ARRIVAL, and a memo would turn 7.9 M arrivals into one row per class, i.e.
    /// silently answer a different question than the one the operator asked.
    fn fu_kind_of(&mut self, recv: &Value, name: &str) -> (u8, u32, u32, u32) {
        if !fast_coll_enabled() || crate::backedge::enabled() {
            return self.fu_kind_slow(recv, name);
        }
        let key = match recv {
            Value::Ref(h) => match self.heap.object_class_word(*h) {
                // `u64::MAX` is the table's empty marker; an `Arc` pointer cannot be one, and a
                // handle with no live object is not classifiable in either path.
                Some(w) if w as u64 != u64::MAX => w as u64,
                _ => return (fk::REFUSE_REF, 0, 0, 0),
            },
            // An arena class id is small and dense, so `<< 1 | 1` puts it in the odd keys — clear
            // of every 8-aligned `Arc` pointer. One table, two representations, and a verdict
            // derived for one representation can never be handed to the other's receiver, which
            // is the class of bug the `$present` shape test in this file already records.
            Value::Obj(p) => ((crate::nextgen::arena_class_id(*p) as u64) << 1) | 1,
            _ => return (fk::REFUSE_OTHER, 0, 0, 0),
        };
        if let Some(hit) = kind_memo::get(key) {
            return hit;
        }
        let v = self.fu_kind_slow(recv, name);
        if v.0 != fk::REFUSE_UNCACHED {
            kind_memo::put(key, v.0, v.1, v.2, v.3);
        }
        v
    }

    /// Derive the verdict from the receiver's class NAME. This is the chain `fu_fastpath` used to
    /// run on all 38.0 M calls; it now runs once per class per thread, which is what makes it
    /// affordable to ALSO resolve the field ids the two new families need right here instead of
    /// in their arms.
    ///
    /// ⚠**No publishing happens here any more.** The `DoubleArrayList` and `java/util/ArrayList`
    /// guard-word publishers used to sit in this match and were guarded only by "is the word
    /// still unpublished", so they retried on every arrival until one succeeded. Behind a memo
    /// they would get exactly ONE attempt, and a first arrival that cannot publish (no class
    /// word, a field id not yet resolvable) would leave the emitted guards comparing against zero
    /// for the life of the process — the identical failure mode `dg_fast=0, dg_slow=16,858,977`
    /// already recorded once. They live in their own arms now, where the retry is preserved.
    fn fu_kind_slow(&mut self, recv: &Value, name: &str) -> (u8, u32, u32, u32) {
        match recv {
            Value::Ref(h) => {
                let code = match self.heap.class_of(*h) {
                    Some("java/util/Iterator") => fk::ITER,
                    Some("it/unimi/dsi/fastutil/ints/IntImmutableList") => fk::IMM_LIST,
                    Some("it/unimi/dsi/fastutil/ints/IntArrayList") => fk::ARR_LIST,
                    Some("it/unimi/dsi/fastutil/doubles/DoubleImmutableList") => fk::D_IMM_LIST,
                    Some("it/unimi/dsi/fastutil/doubles/DoubleArrayList") => fk::D_ARR_LIST,
                    Some("java/util/ArrayList") => fk::LIST,
                    // A MAP ENTRY IS TWO FIELD READS, AND IT WAS COSTING A DISPATCH MISS EACH.
                    // `Map$Entry#getKey` (2,597,306) and `#getValue` (2,484,716) are #3 and #4 of
                    // the never-cacheable census — 5.08 M host excursions a boot for `*(obj+k)`.
                    // The cascade already has an `entry_fastpath` for exactly these two, but it
                    // sits AFTER the dispatch miss, so the miss is paid before the shortcut runs.
                    Some("java/util/Map$Entry")
                    | Some("java/util/AbstractMap$SimpleEntry")
                    | Some("java/util/AbstractMap$SimpleImmutableEntry") => fk::ENTRY,
                    // COLD ARM (`RAVA_NO_CUBECALL=1`): the class identified BY NAME. With the round
                    // on, a handle receiver of this class has no arm here — the birth side puts
                    // every admitted value-computer on the ARENA, so a handle one exists only when
                    // the admission itself is switched off, and then it takes the general path
                    // like any other refused handle.
                    Some("net/minecraft/world/phys/shapes/CubePointRange") if !cubecall_enabled() => {
                        fk::CUBE_RANGE
                    }
                    // `java/util/Optional` is not SERVED by an arm down there — the reader block
                    // above owns it — but classifying it here is what lets that block ask an
                    // integer instead of re-reading and re-comparing the class name on every
                    // arrival. See `fk::OPTIONAL`.
                    Some("java/util/Optional") if fast_fu2_enabled() => fk::OPTIONAL,
                    _ => fk::REFUSE_REF,
                };
                // WHICH classes the compiled-code fast path is turning away. 7.9 M refusals a
                // boot, all of them a plain `Value::Ref` whose class is not one of the ones it
                // admits — so the list, not the representation, is the limit. Keyed by the
                // INTERNED class-name address, so each class gets its own row instead of every
                // refusal collapsing onto one.
                if code == fk::REFUSE_REF && crate::backedge::enabled() {
                    if let Some(c) = self.heap.class_of(*h) {
                        crate::backedge::note_entry(c.as_ptr() as usize, || format!("FU:{c}"));
                    }
                }
                (code, 0, 0, 0)
            }
            Value::Obj(p) => {
                let cid = crate::nextgen::arena_class_id(*p);
                let Some(cls) = crate::nextgen::class_name(cid) else {
                    return (fk::REFUSE_OBJ, 0, 0, 0);
                };
                match &*cls {
                    "it/unimi/dsi/fastutil/ints/IntImmutableList" => (fk::IMM_LIST, 0, 0, 0),
                    "it/unimi/dsi/fastutil/ints/IntArrayList" => (fk::ARR_LIST, 0, 0, 0),
                    "it/unimi/dsi/fastutil/doubles/DoubleImmutableList" => (fk::D_IMM_LIST, 0, 0, 0),
                    "it/unimi/dsi/fastutil/doubles/DoubleArrayList" => (fk::D_ARR_LIST, 0, 0, 0),
                    // COLD ARM (`RAVA_NO_CUBECALL=1`): the NAME. With the round on, the same
                    // receiver reaches the `other` arm below and is classified by the STRUCTURAL
                    // verdict instead — see `fk::VALUE_COMPUTER`.
                    "net/minecraft/world/phys/shapes/CubePointRange" if !cubecall_enabled() => {
                        (fk::CUBE_RANGE, 0, 0, 0)
                    }
                    // The arena half of the `Optional` verdict — and the half that saves more,
                    // because the test it replaces is `class_name(cid)`: a thread-local mirror
                    // lookup plus an `Arc` clone and drop, two atomics, for a name that was only
                    // ever compared and thrown away.
                    "java/util/Optional" if fast_fu2_enabled() => (fk::OPTIONAL, 0, 0, 0),
                    "it/unimi/dsi/fastutil/objects/Reference2ObjectArrayMap" => {
                        (fk::REF_ARRAY_MAP, 0, 0, 0)
                    }
                    // The two families below resolve their field ids ONCE, here, and hand them to
                    // the arm through the memo. An off-layout `arena_getfield` is a PANIC rather
                    // than a wrong answer, so the layout check is mandatory — and a missing slot
                    // is `REFUSE_UNCACHED`, never a stored refusal (see `fk`).
                    c if arena_entry_class(c) => {
                        let kf = self.resolve_field_id(c, "key");
                        let vf = self.resolve_field_id(c, "value");
                        if crate::nextgen::slot_of(cid, kf).is_none()
                            || crate::nextgen::slot_of(cid, vf).is_none()
                        {
                            return (fk::REFUSE_UNCACHED, 0, 0, 0);
                        }
                        (fk::ARENA_ENTRY, kf, vf, 0)
                    }
                    c if size_field_class(c) => {
                        let sf = self.resolve_field_id(c, "size");
                        if crate::nextgen::slot_of(cid, sf).is_none() {
                            return (fk::REFUSE_UNCACHED, 0, 0, 0);

                        }
                        (fk::SIZE_FIELD, sf, 0, 0)
                    }
                    // ⭐**NAME THE ARENA REFUSALS TOO.** The `[FU-RECV]` census counts ARRIVALS,
                    // so a class at the top of it may be perfectly well served — reading it as a
                    // list of targets is how the arena `size()` work started on a class that was
                    // already refused and a class that was not.
                    other => {
                        // ⭐**THE STRUCTURAL VERDICT, WHERE THE NAME USED TO BE.** Asked once per
                        // class per thread (this function is memoised by class id), of the SAME
                        // predicate the birth side answered when it put the object on the arena
                        // — `value_computer_verdict` is itself memoised per class name — so the
                        // classifier and the allocator cannot disagree about a class. Asked
                        // AFTER every exact-name list above: none of those names can also be an
                        // admitted value-computer (each declares a reference field or is
                        // native-backed itself), so nothing classified before this line changes.
                        //
                        // A TRANSIENT loader refusal (a GIL-free worker) is `REFUSE_UNCACHED`,
                        // never memoised, re-asked next call — the `fk` note explains why a
                        // "not yet" must not be stored as a "never". Anything the verdict refuses
                        // falls through to the historic refusal below, census and all.
                        #[cfg(feature = "next-gen-gc")]
                        if cubecall_enabled() && super::native_backing::valuecomputer_enabled() {
                            use super::native_backing::valuecomputer as vc;
                            match self.value_computer_verdict(other) {
                                vc::ADMIT => return (fk::VALUE_COMPUTER, 0, 0, 0),
                                vc::LOADER_REFUSED => return (fk::REFUSE_UNCACHED, 0, 0, 0),
                                _ => {}
                            }
                        }
                        if crate::backedge::enabled() {
                            crate::backedge::note_entry(other.as_ptr() as usize, || {
                                format!("FUO:{other}#{name}")
                            });
                        }
                        (fk::REFUSE_OBJ, 0, 0, 0)
                    }
                }
            }
            _ => (fk::REFUSE_OTHER, 0, 0, 0),
        }
    }

    pub(crate) fn fu_fastpath(&mut self, name: &str, desc: &str, cells: &[i64]) -> Result<Option<i64>, VmError> {
        // â­**PUBLISH THE GUARD WORD HERE, IN `fu_fastpath` ITSELF.** The `getDouble` arm's
        // class word must be the one the OBJECTS carry, and interning the name at VM construction
        // produces a different `Arc<str>` - the guard then missed 16,858,961 times out of
        // 16,858,961, with `dg_cls` proving the compare was reached every time.
        //
        // â **And the previous fix was pasted into the wrong function.** `let recv =
        // jit::unbox_ref(..)` appears twice in this file, and the first occurrence is
        // `fu_read_shared`, which serves `DoubleImmutableList` only - so the publisher ran for a
        // class that never reaches this arm and printed nothing at all. An anchor that matches
        // twice is not an anchor.
        // ⚠**THE OLD PLACEMENT, KEPT ONLY AS A MEASUREMENT ARM** (`RAVA_FU_TOPPUB=1`). It is the
        // block that used to run unconditionally here; leaving it switchable is what makes the
        // move above an A/B rather than an assertion.
        #[cfg(feature = "next-gen-gc")]
        {
            static TOP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *TOP.get_or_init(|| std::env::var_os("RAVA_FU_TOPPUB").is_some())
                && crate::jit::DALIST_CLASS_WORD.load(std::sync::atomic::Ordering::Relaxed) == 0
            {
                if let Some(Value::Ref(h)) = cells
                    .first()
                    .map(|c| jit::unbox_ref(self, *c, ""))
                    .as_ref()
                {
                    if self.heap.class_of(*h)
                        == Some("it/unimi/dsi/fastutil/doubles/DoubleArrayList")
                    {
                        if let Some(w) = self.heap.object_class_word(*h) {
                            crate::jit::DALIST_CLASS_WORD
                                .store(w, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            }
        }
        // The cheapest service level a crossing can get: a native answered from machine code,
        // no synthetic frame, no cascade. Metered so the map has its price too -- and because a
        // REFUSAL here is not free either, and this is what a refusal costs.
        let _fu = crate::diag::fu_fast().start();
        let _reg = crate::alloc_sites::RegionGuard::enter(crate::alloc_sites::Region::FuFast);
        // ⚠**A METER'S OWN BOOKKEEPING IS CHARGED TO ITS PARENT, so do not nest one here.** A
        // second guard around the unbox alone priced it at 5.3 ns -- and raised THIS meter's self
        // time from 63.6 to 102.9, because the child's atomics and thread-locals run inside the
        // parent's span while the child's own measurement excludes them. On a 38 M-event path
        // that is ~44 ns of pure instrument. The outermost meter of a nest is the trustworthy
        // one; every child added inflates it.
        //
        // Measured before it was removed: **unboxing the receiver is 5.3 ns of the 63.6.**
        let recv = jit::unbox_ref(self, cells.first().copied().unwrap_or(0), "");
        // This round's kill switch, read ONCE. Three sites below consult it, and a `OnceLock`
        // read is an acquire load LLVM may not hoist across the calls between them — so it is
        // hoisted here by hand, exactly as `jit_fu_invoke` hoists `fu_call_diag()` for the same
        // population and the same reason.
        let fu2 = fast_fu2_enabled();
        // ⭐**AND ON WHICH RECEIVER CLASSES.** Counting this population by METHOD NAME chose a
        // target that turned out to be wrong: the `List.size()` inline arm pinned the guard to
        // `java/util/ArrayList` and missed, because one name arrives on a dozen unrelated
        // classes. A population is not a target until its RECEIVERS are counted. Sampled 1 in
        // 256, so the map lock is touched ~150k times on a boot rather than 38 M.
        if crate::diag::any_armed() {
            use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
            static N: AtomicU64 = AtomicU64::new(0);
            if N.fetch_add(1, Relaxed) % 256 == 0 {
                // Tag the REPRESENTATION too: a guard written for one of the two heaps cannot
                // hit a receiver that lives in the other, and that is invisible in a class name.
                let cls = match &recv {
                    Value::Ref(h) => self.heap.class_of(*h).map(|c| format!("R:{c}")),
                    #[cfg(feature = "next-gen-gc")]
                    Value::Obj(p) => crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                        .map(|c| format!("O:{c}")),
                    _ => None,
                };
                if let Some(cls) = cls {
                    fu_recv_note(&cls, name);
                }
            }
        }
        // WHICH OPERATIONS ARE THE 37.3 MILLION? A 712-line function reached 38 M times a boot is
        // not restructured on a guess about its distribution.
        if crate::diag::any_armed() {
            match name {
                "get" => crate::diag::fu_op_get().inc(),
                "isEmpty" => crate::diag::fu_op_isempty().inc(),
                "size" => crate::diag::fu_op_size().inc(),
                "add" => crate::diag::fu_op_add().inc(),
                "equals" => crate::diag::fu_op_equals().inc(),
                "intValue" | "longValue" | "booleanValue" | "doubleValue" => {
                    crate::diag::fu_op_boxval().inc()
                }
                "hasNext" => crate::diag::fu_op_hasnext().inc(),
                "next" => crate::diag::fu_op_next().inc(),
                "iterator" => crate::diag::fu_op_iterator().inc(),
                "getKey" | "getValue" => crate::diag::fu_op_entry().inc(),
                "cast" | "identityHashCode" => crate::diag::fu_op_misc().inc(),
                "isPresent" | "orElse" | "orElseThrow" => crate::diag::fu_op_optional().inc(),
                "set" | "remove" => crate::diag::fu_op_mutate().inc(),
                _ => crate::diag::fu_op_other().inc(),
            }
        }
        // ⭐**FIFTEEN STRING COMPARES, ON ALL 38.0 M ARRIVALS, FOR A BLOCK MOST OF THEM CANNOT
        // ENTER.** The match below serves the box unwrappers and the statics; the traffic in
        // front of it is `size`, `isEmpty`, `get`, `getKey`, `hasNext`, `getDouble`, `put`,
        // `iterator`. `maybe_static_member` answers "could this name be one of the fifteen?" from
        // `len | first byte | last byte` — one integer switch — and every one of those hot names
        // is outside the set. A hit runs the match unchanged, so the filter can only ever cost
        // today's behaviour; it cannot serve a different member. See `name_sig`.
        //
        // The `debug_assert` is the drift guard: a name added to the match without a signature
        // would go silently dead, and this makes a debug build fail at its first arrival.
        let try_static = !fu2 || maybe_static_member(name);
        #[cfg(debug_assertions)]
        debug_assert!(
            try_static || !is_static_member_name(name),
            "fu_fastpath: `{name}` is served by the statics match but has no `name_sig` entry"
        );
        if try_static {
            // BOX UNWRAPPERS: the receiver is the boxed value itself, in either representation, and
            // is answered before the class classification below — which only knows `Ref`/`Obj` and
            // would refuse a `Value::Int` receiver outright.
            match (name, desc) {
                ("booleanValue", "()Z") => {
                    return Ok(Some(jit::pack_local(&Value::Int(
                        crate::natives::bool_of(self, Some(&recv)),
                    ))));
                }
                ("intValue", "()I") | ("shortValue", "()S") | ("byteValue", "()B")
                | ("charValue", "()C") => {
                    if let Some(v) = recv.as_int() {
                        return Ok(Some(jit::pack_local(&Value::Int(v))));
                    }
                    return Ok(None);
                }
                ("longValue", "()J") => {
                    if let Some(v) = recv.as_long() {
                        return Ok(Some(jit::pack_local(&Value::Long(v))));
                    }
                    return Ok(None);
                }
                // `String.valueOf` of a primitive — STATIC, and byte-identical to the native arm's
                // rendering (Java's `Integer.toString`/`Long.toString`/`"true"`/`"false"`/one char).
                ("valueOf", "(I)Ljava/lang/String;") => {
                    let n = *cells.first().unwrap_or(&0) as i32;
                    let v = Value::Str(n.to_string().into());
                    return Ok(Some(jit::box_ref(self, &v)));
                }
                ("valueOf", "(J)Ljava/lang/String;") => {
                    let n = *cells.first().unwrap_or(&0);
                    let v = Value::Str(n.to_string().into());
                    return Ok(Some(jit::box_ref(self, &v)));
                }
                ("valueOf", "(Z)Ljava/lang/String;") => {
                    let b = *cells.first().unwrap_or(&0) != 0;
                    let v = Value::Str(if b { "true" } else { "false" }.into());
                    return Ok(Some(jit::box_ref(self, &v)));
                }
                ("valueOf", "(C)Ljava/lang/String;") => {
                    // ⚠A Java `char` is a UTF-16 CODE UNIT, so a lone surrogate (0xD800-0xDFFF) is a
                    // legal value here and `char::from_u32` rejects it. Falling back to `' '` would
                    // be a WRONG answer, not a slow one — hand those to the general path instead.
                    match char::from_u32(*cells.first().unwrap_or(&0) as u32) {
                        Some(c) => {
                            let v = Value::Str(c.to_string().into());
                            return Ok(Some(jit::box_ref(self, &v)));
                        }
                        None => return Ok(None),
                    }
                }
                // `DoubleArrayList.wrap([D)` / `wrap([DI)` — STATIC. `dal_wrap_native` is the arm
                // the general path reaches; this only shortens the route to it. `None` (a shape it
                // declines, or a length fastutil would throw on) falls through unchanged.
                #[cfg(feature = "next-gen-gc")]
                ("wrap", _)
                    if desc.starts_with("([D")
                        && desc.ends_with(")Lit/unimi/dsi/fastutil/doubles/DoubleArrayList;") =>
                {
                    let a0 = jit::unbox_ref(self, cells.first().copied().unwrap_or(0), "");
                    let mut args: Vec<Value> = Vec::with_capacity(2);
                    args.push(a0);
                    if desc.starts_with("([DI") {
                        args.push(Value::Int(cells.get(1).copied().unwrap_or(0) as i32));
                    }
                    return Ok(self.dal_wrap_native(desc, &args).map(|v| jit::box_ref(self, &v)));
                }
                // `Optional.of` / `ofNullable` / `empty` — STATIC, and byte-identical to the cascade
                // arm: `of` REJECTS null (the arm throws; returning an empty Optional here would be
                // the WRONG answer, which is exactly the defect that arm's comment records).
                ("of", _) | ("ofNullable", _) | ("empty", _)
                    if desc.ends_with(")Ljava/util/Optional;") =>
                {
                    let v = if name == "empty" {
                        None
                    } else {
                        let a0 = jit::unbox_ref(self, cells.first().copied().unwrap_or(0), "");
                        if matches!(a0, Value::Null) {
                            if name == "of" {
                                return Ok(None); // let the cascade arm raise the NPE
                            }
                            None
                        } else {
                            Some(a0)
                        }
                    };
                    let opt = crate::stream::make_optional_value(self, v);
                    return Ok(Some(jit::box_ref(self, &opt)));
                }
                // `Objects.requireNonNull` — STATIC, 2.38% of the boot's crossings, and for a
                // non-null argument it is the identity function. A NULL argument must throw, and the
                // message forms take a second argument (a `String` or, for `requireNonNull(T,
                // Supplier)`, a lambda to invoke) — so null returns `None` and the cascade arm that
                // owns the throw does exactly what it did before.
                ("requireNonNull", _)
                    if desc.starts_with("(Ljava/lang/Object;")
                        && desc.ends_with(")Ljava/lang/Object;") =>
                {
                    let a0 = jit::unbox_ref(self, cells.first().copied().unwrap_or(0), "");
                    if matches!(a0, Value::Null) {
                        return Ok(None);
                    }
                    return Ok(Some(jit::box_ref(self, &a0)));
                }
                // `System.identityHashCode` — STATIC, one argument, and 810 ns on the general path
                // (`bench/SeamCost2.java`) for what the natives layer answers with `identity_hash`.
                // 1.21% of the boot's compiled-to-host crossings.
                ("identityHashCode", "(Ljava/lang/Object;)I") => {
                    let v = jit::unbox_ref(self, cells.first().copied().unwrap_or(0), "");
                    return Ok(Some(jit::pack_local(&Value::Int(
                        crate::natives::identity_hash(Some(&v)),
                    ))));
                }
                // `Objects.equals` — STATIC, so `cells[0]`/`cells[1]` are the two arguments. Serves
                // exactly the cases the native arm's `quick` table serves without deriving a content
                // key, including the proven-identity rule, plus the ONE content shape that is
                // answerable without one — a pair of fastutil double lists, via the same
                // `fu_double_list_equals` primitive the ladder arm and the `misc.rs` arm call
                // (`RAVA_NO_DLIST_EQ=1` refuses it back to None). Anything else returns None and
                // the native arm answers. 3.35% of the boot's compiled-to-host crossings.
                ("equals", "(Ljava/lang/Object;Ljava/lang/Object;)Z") => {
                    let a = jit::unbox_ref(self, cells.first().copied().unwrap_or(0), "");
                    let b = jit::unbox_ref(self, cells.get(1).copied().unwrap_or(0), "");
                    let both_null = matches!(a, Value::Null) && matches!(b, Value::Null);
                    if both_null {
                        return Ok(Some(jit::pack_local(&Value::Int(1))));
                    }
                    if matches!(a, Value::Null) || matches!(b, Value::Null) {
                        return Ok(Some(jit::pack_local(&Value::Int(0))));
                    }
                    let quick = match (&a, &b) {
                        (Value::Int(x), Value::Int(y)) => Some(x == y),
                        (Value::Long(x), Value::Long(y)) => Some(x == y),
                        (Value::Float(x), Value::Float(y)) => Some(x.to_bits() == y.to_bits()),
                        (Value::Double(x), Value::Double(y)) => Some(x.to_bits() == y.to_bits()),
                        (Value::Str(x), Value::Str(y)) => Some(x == y),
                        (Value::Ref(_) | Value::Obj(_), Value::Ref(_) | Value::Obj(_)) => {
                            if crate::natives::ref_eq(Some(&a), Some(&b)) {
                                Some(true)
                            } else if crate::natives::objects_identity_equals(self, &a) {
                                Some(false)
                            } else {
                                self.fu_double_list_equals(&a, &b)
                            }
                        }
                        _ => None,
                    };
                    return Ok(match quick {
                        Some(v) => Some(jit::pack_local(&Value::Int(v as i32))),
                        None => None,
                    });
                }
                // `Class.cast` — the same memoised verdict the cascade arm uses, reached without a
                // synthetic frame. A refusal (`No`) returns None so the cascade raises the identical
                // `ClassCastException`; only a PASS is answered here.
                ("cast", "(Ljava/lang/Object;)Ljava/lang/Object;") => {
                    let arg_cell = cells.get(1).copied().unwrap_or(0);
                    let v = jit::unbox_ref(self, arg_cell, "");
                    if matches!(v, Value::Null) {
                        return Ok(Some(jit::box_ref(self, &v)));
                    }
                    if self.cast_passes_memoised(&recv, &v) {
                        return Ok(Some(jit::box_ref(self, &v)));
                    }
                    return Ok(None);
                }
                _ => {}
            }
        }
        // ⭐**THE CLASSIFICATION, AND IT RUNS BEFORE THE TWO SHAPE PROBES NOW.**
        //
        // `fu_kind_of` keys the verdict on the receiver's class WORD (handle) or class id
        // (arena) — the same integer the emitted guards compare — so the chain of ten `&str`
        // compares this used to be runs once per class per thread. It memoises the REFUSALS too,
        // which is the half that matters: 7.9 M of the 38.0 M calls a boot end in one.
        //
        // ⭐**It was BELOW the `$map` and `Optional` blocks, and that ordering was backwards.**
        // Both of those blocks open by asking a question about the receiver's SHAPE or CLASS —
        // `fu_map_id(h)`, which resolves `$map` and walks the object's fields, and a `class_of`
        // plus an 18-byte compare against `java/util/Optional` — and both are reached by
        // `size`/`isEmpty`/`get`/`remove`, i.e. by the hottest names in the boot arriving on
        // lists, sets, iterators and entries that are neither a map nor an `Optional`. The
        // verdict below answers both questions from one integer compare, so it is taken first
        // and the probes are gated on it.
        let (code, cf0, cf1, cf2) = self.fu_kind_of(&recv, name);

        // ⭐**`java/util/Map.get` IS #2 OF THE WHOLE SEAM — 6.92% of 64,088,346 crossings a
        // boot — AND IT HAD NO MACHINE-CODE ARM AT ALL.** `FuKind` recognised six list-ish
        // shapes and a map was not one of them, so every map read went the general path:
        // `jit_call_body` → synthetic frame → `invokevirtual` → `super_chain` → the natives
        // cascade → here. `bench/SeamCost3.java` prices that route at **1,635-1,870 ns** for
        // a `Map<String,String>` read; the same read against a concrete-class site is 98.
        //
        // ⛔The other reading of that gap was tested first and REFUTED: the theory was that
        // an interface-declared site pays a declining cascade pass over each super-chain
        // link. `RAVA_CHAIN_CENSUS` says **link 0 serves 100.00% of 12,000,016 calls** — the
        // walk is not the cost, the DISTANCE to the arm is (the same diagnosis the box
        // unwrappers and `Class.cast` above were fixed by).
        //
        // Admission is by RECEIVER SHAPE, not by class name: `fu_map_id` asks whether the
        // object carries the `$map` table id that every arm in `map_method` already requires,
        // by interned field id. Anything else returns `None` and takes the ordinary route
        // unchanged, so a guest `Map` implementation with a real Java body is untouched. The
        // answer itself comes from `map_method` — the very function the cascade reaches —
        // so this shortens the route and cannot change the result.        #[cfg(feature = "next-gen-gc")]
        //
        // ⭐**AND IT NOW ASKS THE SHAPE QUESTION ONLY OF A CLASS NOTHING ELSE RECOGNISED.**
        // `fu_map_op` admits `size()I`, `isEmpty()Z` and `remove(Ljava/lang/Object;)L…;`, so
        // every `List.size()`, every fastutil `isEmpty()`, every `CubePointRange.size()` and
        // every `Optional.isEmpty()` with a handle receiver reached `fu_map_id(h)` — a `$map`
        // field-id resolve plus a walk of the object's fields, returning a `Value` — purely to be
        // told the object is not a map. `is_refusal(code)` is the same answer for one integer
        // compare: it is false exactly when the class word matched one of the exact class names
        // this file enumerates, and the note on `is_refusal` lists them and says why not one of
        // them carries a `$map`. A genuine map classifies as `REFUSE_REF` and the probe runs
        // exactly as before.
        if (!fu2 || is_refusal(code))
            && crate::jit::map_intrinsic_enabled()
            && crate::map::fu_map_op(name, desc)
        {
            // ⚠`size`/`isEmpty` are shared with the LIST shapes classified below, so a receiver
            // without a `$map` must FALL THROUGH to that classification — not be refused. This
            // block is a statement, not a `match` arm, for exactly that reason: the first version
            // was an arm and answered `None` for every `List.size()`, sending a population the
            // list arm already served back to the general path.
            if let Value::Ref(h) = &recv {
                let h = *h;
                // ⛔**THE PLAN-A FASTUTIL VIEW ROUTE WAS BUILT HERE AND DID NOT PAY.**
                // `Reference2ObjectMap.reference2ObjectEntrySet` is 5.64% of the seam — the
                // second-largest callee left — and those maps carry packed key/value arrays
                // instead of a `$map`, so this block skipped them and `map::dispatch` was only
                // reachable down the general path. Routing them by receiver class measured, on
                // two consecutive interleaved A/Bs, an ON arm of 38.8-39.8 s against the 37.8 s
                // the same arm read without it — a ~1 s REGRESSION, while the OFF arm stayed put.
                // ⭐The first version also allocated the class name to decide whether it wanted
                // the class name, on every MISS; hoisting that behind the predicate recovered
                // most of it but not all, so the route is not merely mispriced, it costs.
                // Reverted. The 5.64% stands as an open target for a route that does not need
                // the receiver's class name at all.
                if self.fu_map_id(h).is_some() {

                    // Every admitted descriptor takes REFERENCE arguments only, so one cell is
                    // one argument and the slot layout needs no widening rule.
                    let nargs = crate::argcall::count_args(desc);
                    let mut args: Vec<Value> = Vec::with_capacity(nargs + 1);
                    args.push(Value::Ref(h));
                    for i in 0..nargs {
                        args.push(jit::unbox_ref(self, cells.get(i + 1).copied().unwrap_or(0), ""));
                    }
                    let args = &args[..];
                    // Same prologue the bridge pays, for the same two reasons: a native reads
                    // `Value::Str`, and the arg buffer must be a GC root while `map_method`
                    // derives a key (which allocates).
                    // The gates word is the caller's, per this function's contract: reading it here keeps the
                    // one acquire load per native call rather than one per canonicalised argument.
                    let canon = crate::natives::entry::canon_arena_strings(
                        crate::natives::entry::entry_gates(),
                        args,
                    );
                    let a: &[Value] = canon.as_deref().unwrap_or(args);
                    let _g = crate::natives::entry::NativeArgsGuard::new(a);
                    return match crate::map::map_method(self, name, a)? {
                        // A reference return is boxed, a primitive one packed — the descriptor
                        // says which, and getting it backwards hands machine code a pointer as
                        // an int.
                        Some(Some(v)) => Ok(Some(if desc.ends_with(';') || desc.ends_with(']') {
                            jit::box_ref(self, &v)
                        } else {
                            jit::pack_local(&v)
                        })),
                        Some(None) => Ok(Some(jit::box_ref(self, &Value::Null))),
                        None => Ok(None),
                    };
                }
            }
            // ⭐**THE ARENA TWIN OF THIS BLOCK: A `HashMap` BORN ON THE ARENA IS A `Value::Obj`,
            // AND THE HANDLE ARM ABOVE COULD NEVER SERVE IT.**
            //
            // The fu-miss receiver census split the `$map` block's unserved half and found the
            // arena `HashMap` (the arena-map allocation route births it as a flat-table
            // `Value::Obj`) paying the FULL general path on every map op — synthetic frame,
            // `opcodes::invoke*` gauntlet, natives cascade — only to be answered by
            // `arena_map::dispatch` anyway: **186 K `get`/`put`/`getOrDefault` arrivals a boot,
            // a `NeverCacheable` row**, so the general path cannot cache its way out of the cost
            // either. This twin asks the same question ONE CROSSING EARLIER: the receiver test
            // is `is_arena_map` — the exact arena class id, validated as an arena address
            // before the header is read; no guest or host NAME is consulted anywhere — and the
            // answer comes from the same `dispatch` the cascade reaches, so the route is
            // shortened and the result cannot change.
            //
            // ⚠A STATEMENT, not a `match` arm, for the block's own fall-through reason: an
            // arena object that is NOT one of our maps — a list, a set, an entry — must fall
            // through to the LIST classification below. `dispatch` answers `None` for a name
            // it does not serve and the twin returns that verbatim, so an unhandled name takes
            // the ordinary route unchanged too.
            //
            // ⚠The twin pays the identical prologue the handle arm above pays — `count_args`,
            // cell unboxing, `canon_arena_strings`, `NativeArgsGuard` — for the same two
            // reasons: a native reads `Value::Str` (arena strings arrive as `Obj`), and the
            // arg buffer must be a GC root while `dispatch` derives a key or grows a table
            // (which allocates, and can collect). `dispatch` re-protects every argument in
            // its own temp scope; the guard is the same belt-and-braces the bridge wears, and
            // the drop order — canon first, guard second — is the bridge's own. Kill switch:
            // `RAVA_NO_FU_AMAP=1` (or the block/family switches) — see `amap_fu_enabled`.
            #[cfg(feature = "next-gen-gc")]
            if let Value::Obj(p) = &recv {
                if amap_fu_enabled() && crate::arena_map::is_arena_map(*p) {
                    // Every admitted descriptor takes REFERENCE arguments only — the handle
                    // twin's own note — so one cell is one argument.
                    let nargs = crate::argcall::count_args(desc);
                    let mut args: Vec<Value> = Vec::with_capacity(nargs + 1);
                    args.push(Value::Obj(*p));
                    for i in 0..nargs {
                        args.push(jit::unbox_ref(self, cells.get(i + 1).copied().unwrap_or(0), ""));
                    }
                    let args = &args[..];
                    let canon = crate::natives::entry::canon_arena_strings(
                        crate::natives::entry::entry_gates(),
                        args,
                    );
                    let a: &[Value] = canon.as_deref().unwrap_or(args);
                    let _g = crate::natives::entry::NativeArgsGuard::new(a);
                    return match crate::arena_map::dispatch(self, name, a)? {
                        Some(r) => {
                            amap_fu_op().inc();
                            // A reference return is boxed, a primitive one packed — the
                            // descriptor says which, exactly as the handle twin packs its
                            // `map_method` answer; getting it backwards hands machine code
                            // a pointer as an int.
                            Ok(Some(match r {
                                Some(v) => {
                                    if desc.ends_with(';') || desc.ends_with(']') {
                                        jit::box_ref(self, &v)
                                    } else {
                                        jit::pack_local(&v)
                                    }
                                }
                                None => jit::box_ref(self, &Value::Null),
                            }))
                        }
                        None => Ok(None),
                    };
                }
            }
        }


        // ⭐**THE `Optional` READERS ARE ~9.5% OF THE SEAM AND EACH IS ONE FIELD READ.**
        // `RAVA_JIT_CB_CENSUS` on a 70 s product boot: `get` 3.26%, `isPresent` 3.05%,
        // `map` 1.09%, `orElse` 1.02%, `ifPresent` 0.68%, `isEmpty` 0.45%. The WRITERS
        // (`of`/`ofNullable`/`empty`) already have machine-code arms above; the readers had
        // none, so every `isPresent()` paid the general path to reach `optional_op`.
        //
        // Only the arms that read are taken: `map`, `flatMap`, `filter` and `ifPresent` invoke a
        // guest lambda and belong on the framed path.
        //
        // ⚠A STATEMENT, not a `match` arm — `isEmpty()Z` is shared with the map block above and
        // the list shapes below, so a receiver that is not an `Optional` must FALL THROUGH. That
        // is the same defect the map block was first written with.
        //
        // ⭐**AND THE "IS THIS AN OPTIONAL?" TEST IS NOW THE CLASS WORD, NOT THE CLASS NAME.**
        // Four of the five admitted members share their name with a collection accessor —
        // `get`, `isEmpty`, plus `isPresent`/`orElse`/`orElseThrow` arriving on `Optional`-typed
        // sites — and `isEmpty()Z` is one of the three hottest names in the boot. Every one of
        // them used to reach a `heap.class_of` (or, on the arena, `class_name(cid)`: a
        // thread-local mirror lookup plus an `Arc` clone and drop, two atomics) and an 18-byte
        // string compare, to be told the receiver is a list or a set. `fk::OPTIONAL` is the same
        // verdict, derived from the same exact class test, once per class per thread — so the
        // gate is one integer compare and it is asked FIRST, before the member match.
        #[cfg(feature = "next-gen-gc")]
        if (!fu2 || code == fk::OPTIONAL)
            && crate::jit::map_intrinsic_enabled()
            && matches!(
                (name, desc),
                ("get", "()Ljava/lang/Object;")
                    | ("orElseThrow", "()Ljava/lang/Object;")
                    | ("isPresent", "()Z")
                    | ("isEmpty", "()Z")
                    | ("orElse", "(Ljava/lang/Object;)Ljava/lang/Object;")
            )
        {
            // ⛔**THE FIRST VERSION TESTED FOR A `$present` FIELD AND BROKE ALL FOUR GATES.** A
            // missing field reads back as `Value::Null` from the handle heap — but from the
            // ARENA it reads as a raw cell, `Value::Int(0)`, so every arena object answered "yes,
            // I am an Optional" and `optional_op` then reported `isPresent() == false` for
            // objects that are not Optionals at all. ⭐A shape test is only as sound as the
            // "absent" answer of the read it is built on, and the two heaps do not agree on that.
            // The class is exact in both representations, and it is the same question the
            // classification below asks.
            {
                // `fk::OPTIONAL` is derived from EXACTLY the match below (both representations,
                // exact class), so with the round on the outer gate has already proved it and
                // this collapses to `true`; with the round off the original test runs verbatim.
                let is_opt = fu2
                    || match &recv {
                        Value::Ref(h) => {
                            self.heap.class_of(*h).is_some_and(|c| c == "java/util/Optional")
                        }
                        #[cfg(feature = "next-gen-gc")]
                        Value::Obj(p) => {
                            crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                                .as_deref()
                                == Some("java/util/Optional")
                        }
                        _ => false,
                    };
                if is_opt {
                    let nargs = crate::argcall::count_args(desc);
                    let mut args: Vec<Value> = Vec::with_capacity(nargs + 1);
                    args.push(recv.clone());
                    for i in 0..nargs {
                        args.push(jit::unbox_ref(self, cells.get(i + 1).copied().unwrap_or(0), ""));
                    }
                    let _g = crate::natives::entry::NativeArgsGuard::new(&args);
                    // `get`/`orElseThrow` on an EMPTY optional must raise
                    // `NoSuchElementException`; `optional_op` answers `Ok(None)` for that so the
                    // cascade arm can throw, and returning `None` here hands it back unchanged.
                    return match crate::stream::optional_op(self, name, &args)? {
                        Some(Some(v)) => Ok(Some(if desc.ends_with(';') || desc.ends_with(']') {
                            jit::box_ref(self, &v)
                        } else {
                            jit::pack_local(&v)
                        })),
                        Some(None) => Ok(Some(jit::box_ref(self, &Value::Null))),
                        None => Ok(None),
                    };
                }
            }
        }

        // ⭐**AND THIS IS WHERE THE 7.9 M REFUSALS A BOOT LEAVE — FOR ONE INTEGER COMPARE AND,
        // NOW, NO WRITE AT ALL.** The three counters are read by `lane_probe` and by nobody else;
        // `fu_why_armed()` is one cached `OnceLock` read of whether anyone is looking. See its
        // note for why an unconditional relaxed atomic is not free on a path this size.
        match code {
            fk::REFUSE_REF => {
                if fu_why_armed() {
                    FU_WHY[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                return Ok(None);
            }
            // `REFUSE_UNCACHED` is an arena shape whose field id had no slot in the layout YET.
            // It is an arena refusal for counting purposes and is re-asked next call.
            fk::REFUSE_OBJ => {
                // The chained `size()` shape's writer: publish this arena class's `size()I`
                // structural facts (plain-getter / literal body, or a permanent refusal) so
                // the site's emitted probe can answer it from one cell load after the first
                // callback. Structural linkage only — see `link_mg_size_structural`.
                #[cfg(feature = "next-gen-gc")]
                if name == "size" && desc == "()I" {
                    self.link_mg_size_structural(&recv);
                }
                if fu_why_armed() {
                    FU_WHY[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                return Ok(None);
            }
            fk::REFUSE_UNCACHED => {
                if fu_why_armed() {
                    FU_WHY[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                return Ok(None);
            }
            fk::REFUSE_OTHER => {
                if fu_why_armed() {
                    FU_WHY[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                return Ok(None);
            }
            _ => {}
        }
        // ⭐**DECOMPOSITION BY SEPARATE RUN (`RAVA_FU_CLASSIFY_ONLY=1`), not by nesting.**
        //
        // `fu_fast`'s p50 is 564 ns and its category is 38% of the boot, but that is SELF time
        // over the whole attempt — classification, guard, and the operation's real work (array
        // reads, map lookups, boxing) are all inside it, and nothing says which dominates. A
        // nested meter cannot answer it: this file records that a second guard around the unbox
        // alone priced that unbox at 5.3 ns and raised the parent's self time from 63.6 to 102.9,
        // because a child's atomics and thread-locals run inside the parent's span.
        //
        // So: return HERE, right after the verdict is decided and before any arm runs. The meter
        // then times classification ONLY, and the difference against a normal run is everything
        // else. The boot is wrong under this flag (every call falls to the general path) but the
        // METER is not — it measures its own span either way.
        {
            static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *ON.get_or_init(|| std::env::var_os("RAVA_FU_CLASSIFY_ONLY").is_some()) {
                let _ = (code, cf0, cf1, cf2);

                return Ok(None);
            }
        }
        // ⭐**`size`/`isEmpty` ON A FASTUTIL SET OR LIST — THE TOP OF THE `[STUCK]` CENSUS.**
        //
        // See `size_field_class` for which classes are admitted and why each one is a reading of
        // its `size()` body rather than a guess from a field name. The two facts that shape this
        // arm:
        //
        // * ⛔**the handle form is REFUSED.** Serving these frame-free through the handle heap
        //   measured **+2.03% slower** on the boot with all gates bit-exact, because the
        //   replacement read costs about what the frame it removes costs. Only `Value::Obj` is
        //   served, where the read is a direct arena cell load; the handle form classifies as a
        //   refusal and now pays one compare to find that out.
        // * the field id and its layout slot were resolved ONCE, at classification, and arrive in
        //   `cf0`. This arm resolves nothing, allocates nothing and hashes nothing.
        //
        // `size()I` and `isEmpty()Z` have distinct descriptors, so the descriptor alone picks the
        // answer; anything else falls through to the general path unchanged.
        if code == fk::SIZE_FIELD {
            if !matches!((name, desc), ("size", "()I") | ("isEmpty", "()Z")) {
                return Ok(None);
            }
            let Value::Obj(p) = &recv else { return Ok(None) };
            // A non-`Int` cell is not a shape this arm understands — refuse rather than coerce.
            let Value::Int(n) = crate::opcodes::arena_getfield(*p, cf0) else {
                return Ok(None);
            };
            let r = if desc == "()I" { n } else { (n == 0) as i32 };
            return Ok(Some(jit::pack_local(&Value::Int(r))));
        }
        // ═══ AN ARENA VALUE-COMPUTER (kill switch `RAVA_NO_CUBECALL=1`, see `cubecall_enabled`) ═══
        //
        // COUNTED, then DECLINED. The receiver's accessors are its own bytecode — the structural
        // verdict is exactly "own `size()I` and own `(I)<value>` body" — and this file has no
        // honest way to answer them except by running that bytecode, which the general path
        // already does better: `jit_fu_invoke`'s refusal edge is `jit_direct_call`, i.e. the
        // COMPILED accessor behind a per-site class-id cache, and that crossing writes the
        // receiver profile (`note_recv_profile` → `sitemono`) from which the caller's second
        // compile builds the guarded arena callee the inline arm splices. The hand-written
        // `i / parts` this replaced was byte-identical to ONE class's bytecode and keyed on its
        // name; serving it here for the whole admitted population would be a wrong answer for any
        // other member.
        //
        // The verdict was derived and memoised in `fu_kind_slow`, so this is one integer compare
        // and one plain-cell increment on the population it names, and nothing on any other.
        #[cfg(feature = "next-gen-gc")]
        if code == fk::VALUE_COMPUTER {
            if let Value::Obj(p) = &recv {
                self.link_structural_accessor(crate::nextgen::arena_class_id(*p), name, desc);
            }
            VC_CALLBACK.store(
                VC_CALLBACK.load(std::sync::atomic::Ordering::Relaxed) + 1,
                std::sync::atomic::Ordering::Relaxed,
            );
            return Ok(None);
        }
        let kind = match code {
            fk::ITER => FuKind::Iter,
            fk::IMM_LIST => FuKind::ImmList,
            fk::ARR_LIST => FuKind::ArrList,
            fk::D_IMM_LIST => FuKind::DImmList,
            fk::D_ARR_LIST => FuKind::DArrList,
            fk::LIST => FuKind::List,
            fk::ENTRY => FuKind::Entry,
            fk::CUBE_RANGE => FuKind::CubeRange,
            fk::REF_ARRAY_MAP => FuKind::RefArrayMap,
            // `TreeNode` is the arena-entry family: gson's node, which named the variant, plus
            // guava's immutable entries, which share its shape exactly (see `arena_entry_class`).
            fk::ARENA_ENTRY => FuKind::TreeNode,
            // Every refusal code returned above; anything else is a code with no arm, and a fast
            // path that cannot prove its precondition refuses.
            _ => return Ok(None),
        };
        // ITERATOR: a native `$elems`/`$pos` iterator (produced by `iterator_of`,
        // including our own `iterator()` fast path). Read/advance a SINGLE element
        // directly — `iter_op` otherwise materialises the whole backing Vec per call.
        if let FuKind::Iter = kind {
            let Value::Ref(h) = recv else { return Ok(None) };
            // Publish the inline arm's guard word and field ids FROM THIS RECEIVER. Everything
            // here is known-good: the class was just classified as an iterator and both fields
            // are about to be read by the code below.
            #[cfg(feature = "next-gen-gc")]
            if crate::jit::ITER_CLASS_WORD.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                if let (Some(w), Some((e, p))) =
                    (self.heap.object_class_word(h), self.fu_iter_field_ids())
                {
                    crate::jit::ITER_ELEMS_FID
                        .store(e as i64 + 1, std::sync::atomic::Ordering::Relaxed);
                    crate::jit::ITER_POS_FID
                        .store(p as i64 + 1, std::sync::atomic::Ordering::Relaxed);
                    crate::jit::ITER_SOURCE_FID.store(
                        self.heap.field_id("$source") as i64 + 1,
                        std::sync::atomic::Ordering::Relaxed);
                    crate::jit::ITER_CLASS_WORD
                        .store(w, std::sync::atomic::Ordering::Relaxed);
                }
            }
            // Resolve $pos/$elems by id once (cached) → no per-call name hash. $elems is
            // written first by `iterator_of` (slot 0), $pos second (slot 1).
            let (elems_id, pos_id) = match self.fu_iter_field_ids() {
                Some(ids) => ids,
                None => return Ok(None),
            };
            let pos = self.heap.get_field_hint(h, pos_id, 1).0.as_int().unwrap_or(0);
            let Value::Array(arr) = self.heap.get_field_hint(h, elems_id, 0).0 else {
                return Ok(None);
            };
            let len = self.heap.array_len(arr).unwrap_or(0) as i32;
            return Ok(match name {
                "hasNext" if desc == "()Z" => Some(jit::pack_local(&Value::Int((pos < len) as i32))),
                "nextInt" | "nextLong" | "next" | "nextDouble" | "nextFloat" => {
                    if crate::stream::collections::iterator_source_changed(self, h) {
                        return Ok(None); // preserve the cursor for the throwing path
                    }
                    if pos < 0 || pos >= len {
                        return Ok(None); // exhausted/odd → let the real iterator handle it
                    }
                    let v = self.heap.array_get(arr, pos as usize).unwrap_or(Value::Null);
                    self.heap.put_field_hint(h, pos_id, Value::Int(pos + 1), 1);
                    Some(match name {
                        "nextInt" => jit::pack_local(&Value::Int(v.as_int().unwrap_or(0))),
                        "nextLong" => jit::pack_local(&Value::Long(v.as_long().unwrap_or(0))),
                        "nextDouble" => jit::pack_local(&Value::Double(v.as_f64())),
                        "nextFloat" => jit::pack_local(&Value::Float(v.as_f32())),
                        _ => jit::box_ref(self, &v),
                    })
                }
                _ => return Ok(None),
            });
        }
        // MAP ENTRY: `$key`/`$val` straight off the object, by CONSTANT field id (`heap::Syn`)
        // so the read costs no name hash. Byte-identical to the cascade's `entry_fastpath`
        // arm — same two fields, same order — which is what makes this a placement change and
        // not a semantics one. An entry without the field (never a real entry) answers
        // `Value::Null`, and a `Null` here is indistinguishable from a genuine null value, so
        // it is NOT served: fall through to the general path, which knows the difference.
        if let FuKind::Entry = kind {
            let Value::Ref(h) = recv else { return Ok(None) };
            // Publish the inline arm's guard word from THIS receiver, once. The field ids are
            // resolved in `jit_run` before compiling (they are synthetic and interned by whoever
            // built the entry), so only the word needs a live object.
            #[cfg(feature = "next-gen-gc")]
            if crate::jit::ENTRY_CLASS_WORD.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                if let Some(w) = self.heap.object_class_word(h) {
                    crate::jit::ENTRY_CLASS_WORD
                        .store(w, std::sync::atomic::Ordering::Relaxed);
                }
            }
            let f = match (name, desc) {
                ("getKey", "()Ljava/lang/Object;") => crate::heap::Syn::Key,
                ("getValue", "()Ljava/lang/Object;") => crate::heap::Syn::Val,
                _ => return Ok(None),
            };
            let v = self.heap.get_field_syn(h, f);
            if matches!(v, Value::Null) {
                return Ok(None);
            }
            return Ok(Some(jit::box_ref(self, &v)));
        }
        // java.util.ArrayList: serve size/isEmpty/get/add/set off the native `$elems`
        // backing (boxed `Value` elements) — the same in-place ops the interpreter's
        // collection layer runs (stream.rs), but with no rava_invoke synthetic frame.
        // A list with no `$elems` yet (constructed, never `add`ed), an out-of-bounds
        // index, or anything else returns `Ok(None)` → identical general dispatch.
        if let FuKind::List = kind {
            let Value::Ref(h) = recv else { return Ok(None) };
            // Publish the class word the inline `get` guard compares against — ONLY when that
            // guard is emitted at all.
            //
            // ⛔**MEASURED, AND THE FIRST VERSION COST 13x.** Publishing unconditionally from the
            // classifier took `bench/OpCost2.java`'s index loop from 3000 to 43000 ns with the
            // four gates still bit-exact: correctness is not what a hot-path helper breaks. The
            // flag check is a resolved `OnceLock` read and keeps the default path byte-identical.
            //
            // ⚠It sits in the ARM, not in the classifier, for the reason the double-list arm
            // below spells out: the classifier is memoised now, and this publisher must be free
            // to retry on every arrival until one succeeds.
            #[cfg(feature = "next-gen-gc")]
            if crate::jit::alist_get_inline_enabled() {
                self.publish_arraylist_word();
            }
            let Value::Array(arr) = self.heap.get_field_syn(h, crate::heap::Syn::Elems) else {
                return Ok(None);
            };
            // Spare capacity is not elements — see `Heap::elems_values`.
            let len = self.heap.elems_len(h, arr);
            return Ok(match (name, desc) {
                ("size", "()I") => Some(jit::pack_local(&Value::Int(len as i32))),
                ("isEmpty", "()Z") => Some(jit::pack_local(&Value::Int((len == 0) as i32))),
                ("get", "(I)Ljava/lang/Object;") => {
                    let i = *cells.get(1).unwrap_or(&0) as i32;
                    if i < 0 || i as usize >= len {
                        return Ok(None); // OOB → let the real method throw IOOBE
                    }
                    let v = self.heap.array_get(arr, i as usize).unwrap_or(Value::Null);
                    Some(jit::box_ref(self, &v))
                }
                ("add", "(Ljava/lang/Object;)Z") => {
                    return self.jit_list_mutation(h, name, desc, cells);
                }
                ("remove", "(I)Ljava/lang/Object;") => {
                    return self.jit_list_mutation(h, name, desc, cells);
                }
                ("set", "(ILjava/lang/Object;)Ljava/lang/Object;") => {
                    return self.jit_list_mutation(h, name, desc, cells);
                }
                // RAVA_ITER_INTRINSIC: a native `$elems`/`$pos` iterator sharing the
                // list's backing (zero copy) — `next`/`hasNext` on it are served by the
                // FuKind::Iter arm above. Correct for the non-modifying for-each (the
                // dominant case); a structural modify during iteration is snapshot-ish
                // vs Java's CME. Gated until MC-boot A/B confirms parity.
                ("iterator", _) if desc.starts_with("()L") && jit::iter_intrinsic_enabled() => {
                    let it = self.heap.alloc("java/util/Iterator");
                    self.heap.put_field_syn(it, crate::heap::Syn::Elems, Value::Array(arr));
                    self.heap.put_field_syn(it, crate::heap::Syn::Pos, Value::Int(0));
                    // The SAME back-link and fail-fast witness `coll_op`'s iterator arm
                    // stamps. Without them this intrinsic — which serves `iterator()` for
                    // every `java/util/ArrayList`, i.e. every enhanced-for loop in the guest
                    // — produced an iterator that could neither remove from its source nor
                    // notice a structural modification, and the check in `iter_op` was dead
                    // code for the case it exists to catch.
                    self.heap.put_field_syn(it, crate::heap::Syn::Source, Value::Ref(h));
                    self.heap.put_field_syn(it, crate::heap::Syn::ExpSize, Value::Int(len as i32));
                    Some(jit::box_ref(self, &Value::Ref(it)))
                }
                _ => return Ok(None),
            });
        }
        // DOUBLE LIST: a fastutil double list (`DoubleImmutableList`/`DoubleArrayList`) —
        // the VoxelShape coordinate backing (a boot hot sink). Serve off the backing
        // `double[]` (`a`) exactly as the int lists do (getDouble/size/isEmpty/iterator);
        // an over-allocated `DoubleArrayList`'s logical length is its `size` field.
        // CUBE POINT RANGE: `parts+1` evenly spaced doubles, computed rather than stored.
        // Byte-identical to the bytecode, which is four instructions:
        //   getDouble(i): iload_1 i2d aload_0 getfield parts i2d ddiv dreturn
        //   size():       aload_0 getfield parts iconst_1 iadd ireturn
        // An out-of-range index returns None so the real method raises the same
        // IndexOutOfBoundsException `AbstractDoubleList` would.
        if let FuKind::CubeRange = kind {
            static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *OFF.get_or_init(|| std::env::var_os("RAVA_NO_CUBERANGE").is_some()) {
                return Ok(None);
            }
            let parts = match self.cube_parts(&recv) {
                Some(p) => p,
                None => return Ok(None),
            };
            // Publish the inline arm's guard word and field id from THIS receiver, which has just
            // been classified as a CubePointRange and whose `parts` was just read.
            #[cfg(feature = "next-gen-gc")]
            if crate::jit::CUBE_CLASS_WORD.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                if let Value::Ref(h) = &recv {
                    if let Some(w) = self.heap.object_class_word(*h) {
                        crate::jit::CUBE_CLASS_WORD
                            .store(w, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            let r = match (name, desc) {
                ("size", "()I") => jit::pack_local(&Value::Int(parts.saturating_add(1))),
                ("isEmpty", "()Z") => jit::pack_local(&Value::Int(0)),
                ("getDouble", "(I)D") => {
                    let i = *cells.get(1).unwrap_or(&0) as i32;
                    if i < 0 || i > parts {
                        return Ok(None); // out of bounds → let the real method throw
                    }
                    jit::pack_local(&Value::Double(f64::from(i) / f64::from(parts)))
                }
                _ => return Ok(None),
            };
            return Ok(Some(r));
        }
        // **AN ARENA MAP ENTRY: `getKey`/`getValue` ARE ONE ARENA CELL LOAD EACH.**
        //
        // gson's `LinkedTreeMap$Node` — 323,484 refused crossings a datapack run — implements
        // `java.util.Map$Entry`, so the engine already served this SHAPE; but that arm is bound to
        // the three JDK entry classes and reads `$key`/`$val`, the engine's OWN synthetic fields,
        // while a gson node has real declared ones. ⭐**And so do guava's immutable entries**, the
        // objects `ImmutableMap.entrySet()` iteration hands back, which had no arm at all and were
        // refused into the general path for `*(obj + slot)`. `arena_entry_class` names the family;
        // one shape, one arm.
        //
        // ⭐**THE FIELD IDS ARRIVE ALREADY RESOLVED (`cf0`/`cf1`), AND THE SLOT CHECK IS ALREADY
        // DONE.** This arm used to call `class_name(cid)` — a thread-local mirror lookup plus an
        // `Arc` clone and drop — then `resolve_field_id`, which hashes a 45-character class name,
        // then `slot_of`, on EVERY call, to re-derive a constant. All of it is a property of the
        // class, so it is now derived once per class in `fu_kind_slow` and carried by the memo. An
        // off-layout `arena_getfield` is a PANIC rather than a wrong answer, so the check is not
        // optional — it is only moved, and the memo is keyed by the very class id it verified.
        #[cfg(feature = "next-gen-gc")]
        if let FuKind::TreeNode = kind {
            // `RAVA_NO_TREENODE=1` restores the general path for the whole family. (The name
            // predates the widening; it is the same arm, and a kill switch that changes name
            // between rounds cannot be compared against its own history.)
            static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if !*ON.get_or_init(|| std::env::var_os("RAVA_NO_TREENODE").is_none()) {
                return Ok(None);
            }
            let Value::Obj(p) = &recv else { return Ok(None) };
            let fid = match (name, desc) {
                ("getKey", "()Ljava/lang/Object;") => cf0,
                ("getValue", "()Ljava/lang/Object;") => cf1,
                _ => return Ok(None),
            };
            let v = crate::opcodes::arena_getfield(*p, fid);
            crate::diag::treenode_op().inc();
            return Ok(Some(jit::box_ref(self, &v)));
        }
        // **A REFERENCE-KEYED ARRAY MAP, SERVED OFF ITS THREE PACKED ARENA FIELDS.**
        //
        // fastutil's `Reference2ObjectArrayMap` is a linear map — `key[]`, `value[]`, `size` —
        // and its key comparison is `==`. `ref_equal` is the engine's own `if_acmpeq`, so the
        // scan below asks exactly what the guest body asks, in the same order, and stops where it
        // stops. The arena refusal census names this as the largest refused family: `#put`
        // 709,683 a datapack run and `#get` 112,384, every one paying the attempt AND the general
        // dispatch before a native answers it anyway.
        //
        // ⚠**GROWTH IS REFUSED, deliberately.** An append that fills the arrays has to reallocate
        // both backings, and doing that here would duplicate allocation policy that lives in one
        // place. `None` sends the call down the identical path it takes today.
        #[cfg(feature = "next-gen-gc")]
        if let FuKind::RefArrayMap = kind {
            // `RAVA_NO_REFMAP=1` sends every one of these back down the general path.
            static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if !*ON.get_or_init(|| std::env::var_os("RAVA_NO_REFMAP").is_none()) {
                return Ok(None);
            }
            let Value::Obj(p) = recv else { return Ok(None) };
            let cid = crate::nextgen::arena_class_id(p);
            let Some((kid, vid, sid)) = crate::map::arraymap_ids(self, cid) else {
                return Ok(None);
            };
            // ⚠**THE LAYOUT CHECK IS ALREADY DONE — IT IS THE MEMO'S OWN ADMISSION TEST.**
            // An off-layout `arena_getfield` is a PANIC rather than a wrong answer, so the check
            // is mandatory; it is not, however, mandatory HERE. `map::arraymap_ids` returns
            // `Some((kid, vid, sid))` only when `slot_of(cid, ·)` answered `Some` for all three
            // (see `map/classes.rs`), and it is keyed by the very `cid` this receiver carries, so
            // a `Some` from it IS the proof. Repeating it cost three `FastMap` probes per SERVED
            // call — a thread-local borrow and a hash each — to re-derive a constant of the class
            // that the line above had just returned. Slot tables are append-only (`nextgen`'s own
            // reason for caching only POSITIVE `slot_of` answers), so a slot that existed when the
            // memo was filled cannot later be absent, and the assertion below re-derives it in
            // debug builds so the invariant is checked where checking is free.
            debug_assert!(
                crate::nextgen::slot_of(cid, kid).is_some()
                    && crate::nextgen::slot_of(cid, vid).is_some()
                    && crate::nextgen::slot_of(cid, sid).is_some(),
                "arraymap_ids admitted a class whose layout lacks key/value/size"
            );
            // ⭐**`size`/`isEmpty` ARE THE `size` CELL AND NOTHING ELSE — AND THEY WERE BEING
            // REFUSED AFTER READING BOTH BACKING ARRAYS.**
            //
            // `fu_fastpath`'s own receiver census — the `[FU-RECV]` table, i.e. the population
            // that ARRIVES here — is quoted verbatim in `jit/trivial_getter.rs`:
            //
            //     O:it/unimi/dsi/fastutil/objects/Reference2ObjectArrayMap#size  461,824
            //
            // second only to `IndirectMerger#size`. `java/util/Map#size()I` and `#isEmpty()Z` are
            // both on `map::fu_map_op`, so `fu_routable` admits those sites; the receiver is an
            // arena `Value::Obj`, so the `$map` block above falls through by design; classification
            // then lands on `REF_ARRAY_MAP` and the operation match below serves only
            // `get`/`containsKey`/`put` — so every one of them read `key[]`, `value[]`, `size` and
            // both array lengths, refused, and still owed `jit_direct_call` + `jit_call`.
            //
            // fastutil's bodies are `public int size() { return size; }` and
            // `public boolean isEmpty() { return size == 0; }` — the field verbatim, no clamp, no
            // consultation of the backings — so this arm is answered from `sid` alone and must NOT
            // borrow the `n > cap` shape guard below, which exists to bound the `get`/`put` SCANS
            // and would answer a different number than the guest does.
            //
            // ⚠It sits ahead of the array reads for that reason and no other: the two backings are
            // not evidence about `size()` and reading them to answer it is the waste being removed.
            // Only `Value::Obj` is served — the identical restriction the `SIZE_FIELD` family
            // records, where serving the same shape through the HANDLE heap measured **+2.03%
            // slower** because the replacement read cost about what the frame it removed cost.
            // Here the read is one direct arena cell load.
            //
            // The guard proves: this class id classified EXACTLY as `Reference2ObjectArrayMap`
            // (memoised on the arena class id, not on a name compare); `arraymap_ids` proved `size`
            // resolves on this class and has a slot in this layout; and the cell decodes as
            // `Value::Int`. A negative count is not a shape this arm understands — it is refused
            // rather than coerced. Every failure branches to `Ok(None)`, the cold arm that is the
            // route this call takes today.
            if refmap_size_enabled() && matches!((name, desc), ("size", "()I") | ("isEmpty", "()Z"))
            {
                let Value::Int(s) = crate::opcodes::arena_getfield(p, sid) else {
                    return Ok(None);
                };
                if s < 0 {
                    return Ok(None);
                }
                let r = if desc == "()I" { s } else { (s == 0) as i32 };
                crate::diag::refmap_op().inc();
                return Ok(Some(jit::pack_local(&Value::Int(r))));
            }
            let (Value::Array(ka), Value::Array(va)) = (
                crate::opcodes::arena_getfield(p, kid),
                crate::opcodes::arena_getfield(p, vid),
            ) else {
                return Ok(None);
            };
            let n = match crate::opcodes::arena_getfield(p, sid) {
                Value::Int(s) => s.max(0) as usize,
                _ => return Ok(None),
            };
            let cap = self
                .heap
                .array_len(ka)
                .unwrap_or(0)
                .min(self.heap.array_len(va).unwrap_or(0));
            if n > cap {
                return Ok(None);
            }
            let find = |vm: &mut Vm, k: &Value| -> Option<usize> {
                (0..n).find(|&i| {
                    vm.heap
                        .array_get(ka, i)
                        .map(|e| crate::opcodes::ref_equal(&e, k))
                        .unwrap_or(false)
                })
            };
            let r = match (name, desc) {
                ("get", "(Ljava/lang/Object;)Ljava/lang/Object;") => {
                    let k = jit::unbox_ref(self, *cells.get(1).unwrap_or(&0), "");
                    let v = match find(self, &k) {
                        Some(i) => self.heap.array_get(va, i).unwrap_or(Value::Null),
                        None => Value::Null,
                    };
                    jit::box_ref(self, &v)
                }
                ("containsKey", "(Ljava/lang/Object;)Z") => {
                    let k = jit::unbox_ref(self, *cells.get(1).unwrap_or(&0), "");
                    jit::pack_local(&Value::Int(find(self, &k).is_some() as i32))
                }
                ("put", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;") => {
                    let k = jit::unbox_ref(self, *cells.get(1).unwrap_or(&0), "");
                    let v = jit::unbox_ref(self, *cells.get(2).unwrap_or(&0), "");
                    match find(self, &k) {
                        Some(i) => {
                            let old = self.heap.array_get(va, i).unwrap_or(Value::Null);
                            self.heap.array_set(va, i, v);
                            jit::box_ref(self, &old)
                        }
                        None if n < cap => {
                            self.heap.array_set(ka, n, k);
                            self.heap.array_set(va, n, v);
                            crate::opcodes::arena_putfield(p, sid, &Value::Int(n as i32 + 1));
                            jit::box_ref(self, &Value::Null)
                        }
                        None => return Ok(None),
                    }
                }
                _ => return Ok(None),
            };
            crate::diag::refmap_op().inc();
            return Ok(Some(r));
        }
        if let FuKind::DImmList | FuKind::DArrList = kind {
            let is_dal = matches!(kind, FuKind::DArrList);
            // ⭐**PUBLISH THE `getDouble` ARM'S GUARD WORD FROM THIS RECEIVER, NOT FROM
            // `intern_class`.** The inline arm published it at VM CONSTRUCTION, by interning the
            // name before the class was loaded — and the object the loader later builds carries a
            // different `Arc<str>`, so the guard compared two unequal words and missed **every**
            // time: `dg_fast=0, dg_slow=16,858,977` across 31 emitted sites. A site count cannot
            // show that; only a per-arm counter can. Taking the word off a live receiver cannot be
            // wrong by construction — it is the value `emit_heap_ref_class_key` reads out of the
            // header.
            //
            // ⚠**It lives HERE, in the arm, and not in the classifier, because the classifier is
            // now MEMOISED.** The publisher is guarded by "is the word still unpublished", i.e. it
            // retries on every arrival until one succeeds; behind the memo it would get exactly
            // one attempt per class per thread, and a first arrival that cannot publish would
            // leave the emitted guards comparing against zero forever. The arm runs on every
            // arrival, so the retry survives — and the condition it tests for (a live
            // `DoubleArrayList` reaching `fu_fastpath`) is exactly this arm.
            //
            // ⚠A HANDLE receiver only, exactly as before the move: the word the inline guard
            // compares is a handle-heap `Arc` pointer, and an arena receiver has no such word to
            // publish. Widening it here would publish field ids against a word that stays zero.
            #[cfg(feature = "next-gen-gc")]
            if is_dal {
                use std::sync::atomic::Ordering::Relaxed;
                if let Value::Ref(rh) = &recv {
                    let rh = *rh;
                    if crate::jit::DALIST_CLASS_WORD.load(Relaxed) == 0 {
                        if let Some(w) = self.heap.object_class_word(rh) {
                            crate::jit::DALIST_CLASS_WORD.store(w, Relaxed);
                        }
                        // The field ids travel with the word: a method is compiled once, so ids
                        // arriving later would never reach its inline arm.
                        let owner = "it/unimi/dsi/fastutil/doubles/DoubleArrayList";
                        if crate::jit::DALIST_A_FID.load(Relaxed) == 0 {
                            let aid = self.resolve_field_id(owner, "a");
                            if aid != 0 {
                                crate::jit::DALIST_A_FID.store(aid as i64 + 1, Relaxed);
                            }
                        }
                        if crate::jit::DALIST_SIZE_FID.load(Relaxed) == 0 {
                            let sid = self.resolve_field_id(owner, "size");
                            if sid != 0 {
                                crate::jit::DALIST_SIZE_FID.store(sid as i64 + 1, Relaxed);
                            }
                        }
                    }
                }
            }
            let (h, len) = match self.fu_double_view(&recv, is_dal) {
                Some(v) => v,
                None => return Ok(None),
            };
            let r = match (name, desc) {
                ("size", "()I") => jit::pack_local(&Value::Int(len as i32)),
                ("isEmpty", "()Z") => jit::pack_local(&Value::Int((len == 0) as i32)),
                ("getDouble", "(I)D") => {
                    let i = *cells.get(1).unwrap_or(&0) as i32;
                    if i < 0 || i as usize >= len {
                        return Ok(None); // out of bounds → let the real method throw IOOBE
                    }
                    let v = self.heap.array_get(h, i as usize).map(|x| x.as_f64()).unwrap_or(0.0);
                    jit::pack_local(&Value::Double(v))
                }
                // ⭐**`equals` IS A LOOP, AND IT WAS THE LARGEST INTERPRETED BODY LEFT.**
                // `RAVA_INTERP_WHO` on a datapack run: `DoubleArrayList.equals` at **301,834
                // interpreted entries** across its two overloads — the top of the census after
                // the guest-subclass compile fix, and unreachable by that fix because this
                // class's OWN name is in a natively-served namespace.
                //
                // It is served here instead, which is where the rest of this class already
                // lives. fastutil's own body is `size` compare then `a1[i] != a2[i]` over the
                // logical prefix, and Rust's `!=` on `f64` is the same IEEE comparison Java's
                // is — NaN differs from itself, +0.0 equals -0.0 — so the answer is identical
                // element by element.
                //
                // ⚠**Refuses unless BOTH sides are double lists.** `equals(Object)` arrives with
                // whatever the guest passed; anything without a double view (a set, a boxed
                // list, null) returns `None` and the real method answers exactly as before.
                ("equals", _) if desc.ends_with(")Z") && cells.len() >= 2 => {
                    let other = jit::unbox_ref(self, cells[1], "");
                    match self.fu_double_list_equals(&recv, &other) {
                        Some(eq) => jit::pack_local(&Value::Int(eq as i32)),
                        None => return Ok(None),
                    }
                }
                ("iterator", _) | ("listIterator", _) if desc.starts_with("()L") => {
                    // Native `$elems`/`$pos` iterator (nextDouble reads it directly). An
                    // immutable list shares its backing array (zero copy — never mutated);
                    // an over-allocated array list copies its logical [0,size) prefix.
                    let arr = if !is_dal {
                        h
                    } else {
                        let mut elems: Vec<Value> = Vec::with_capacity(len);
                        for i in 0..len {
                            elems.push(self.heap.array_get(h, i).unwrap_or(Value::Double(0.0)));
                        }
                        self.heap.alloc_values("double", elems)
                    };
                    let it = self.heap.alloc("java/util/Iterator");
                    self.heap.put_field_syn(it, crate::heap::Syn::Elems, Value::Array(arr));
                    self.heap.put_field_syn(it, crate::heap::Syn::Pos, Value::Int(0));
                    jit::box_ref(self, &Value::Ref(it))
                }
                _ => return Ok(None),
            };
            return Ok(Some(r));
        }
        // LIST: a fastutil int list (`IntImmutableList`/`IntArrayList`). Serve off the
        // backing `int[]` (`a`); `IntArrayList`'s logical length is its `size` field.
        let is_arraylist = matches!(kind, FuKind::ArrList);
        let (h, len) = match self.fu_int_view(&recv, is_arraylist) {
            Some(v) => v,
            None => return Ok(None),
        };
        let r = match (name, desc) {
            ("size", "()I") => jit::pack_local(&Value::Int(len as i32)),
            ("isEmpty", "()Z") => jit::pack_local(&Value::Int((len == 0) as i32)),
            ("getInt", "(I)I") => {
                let i = *cells.get(1).unwrap_or(&0) as i32;
                if i < 0 || i as usize >= len {
                    return Ok(None); // out of bounds → let the real method throw IOOBE
                }
                let v = self.heap.array_get(h, i as usize).and_then(|x| x.as_int()).unwrap_or(0);
                jit::pack_local(&Value::Int(v))
            }
            ("iterator", _) | ("listIterator", _) if desc.starts_with("()L") => {
                // Native iterator with NO fastutil iterator object or per-element
                // bytecode. An immutable list shares its backing array directly (zero
                // copy — the array never changes); an over-allocated array list copies
                // its logical [0,size) prefix so the tail isn't iterated.
                let arr = if !is_arraylist {
                    h
                } else {
                    let mut elems: Vec<Value> = Vec::with_capacity(len);
                    for i in 0..len {
                        elems.push(self.heap.array_get(h, i).unwrap_or(Value::Int(0)));
                    }
                    self.heap.alloc_values("int", elems)
                };
                let it = self.heap.alloc("java/util/Iterator");
                self.heap.put_field_syn(it, crate::heap::Syn::Elems, Value::Array(arr));
                self.heap.put_field_syn(it, crate::heap::Syn::Pos, Value::Int(0));
                jit::box_ref(self, &Value::Ref(it))
            }
            _ => return Ok(None),
        };
        Ok(Some(r))
    }

    /// Stage 3 / 14.4: native `DoubleArrayList.wrap([D)` / `wrap([DI)` — the fastutil "wrap" ctors are
    /// NO-COPY (this.a = a; this.size = len). Build the list object directly (write `a`=the SAME double[]
    /// + `size` via write_value_field, the dual of fu_double_view's reads) instead of running the
    /// interpreted chain wrap→wrap→<init>([DZ)→AbstractDoubleList.<init>→AbstractDoubleCollection.<init>.
    /// Returns None (→ guest bytecode) if the shape doesn't match or `len` exceeds the array (fastutil
    /// throws there — let the real method throw). Bit-exact: same backing array, same logical length.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn dal_wrap_native(&mut self, descriptor: &str, args: &[Value]) -> Option<Value> {
        let arr = match args.first() {
            Some(Value::Array(a)) => *a,
            _ => return None,
        };
        let phys = self.heap.array_len(arr)?;
        let size = match descriptor {
            "([D)Lit/unimi/dsi/fastutil/doubles/DoubleArrayList;" => phys,
            "([DI)Lit/unimi/dsi/fastutil/doubles/DoubleArrayList;" => {
                let n = args.get(1).and_then(Value::as_int)?;
                if n < 0 || n as usize > phys {
                    return None; // fastutil throws (len out of range) — let the bytecode do it
                }
                n as usize
            }
            _ => return None,
        };
        let owner = "it/unimi/dsi/fastutil/doubles/DoubleArrayList";
        let h = self.heap.alloc(owner);
        let recv = Value::Ref(h);
        // MEMOISED. `resolve_field_id` is a by-NAME resolution, and this fast path runs 2.3M
        // times a boot (`DoubleArrayList.wrap` is 3.64% of the 64,088,346 compiled-to-host
        // crossings) for two field ids that are constants of this one class. The same shape the
        // `arraymap_ids` memo exists for, one file over.
        static AID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        static SID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let aid = match AID.get() {
            Some(&v) => v,
            None => {
                let v = self.resolve_field_id(owner, "a");
                let _ = AID.set(v);
                v
            }
        };
        let sid = match SID.get() {
            Some(&v) => v,
            None => {
                let v = self.resolve_field_id(owner, "size");
                let _ = SID.set(v);
                v
            }
        };
        self.write_value_field(&recv, aid, Value::Array(arr));
        self.write_value_field(&recv, sid, Value::Int(size as i32));
        Some(recv)
    }

    /// **THE `Reference2ObjectArrayMap.<init>(I)V` SERVE** — the `jit_call_body` choke arm
    /// (kill switch `RAVA_NO_R2OMAP_CTOR=1`, withdrawn with the `RAVA_NO_R2OMAP` family; see
    /// the arm's comment there for the profile row and the body this replaces).
    ///
    /// `Some(0)` = served (the packed `void` result; a `V` descriptor's cell is unused by the
    /// caller). `None` = decline, the byte-identical synthetic-frame route. The caller has
    /// already proved `kind == 2`, `raw.len() == 2` and the kill switch; this re-derives
    /// nothing about the site and asks the receiver four questions, each with a `None` decline:
    ///
    /// * is the receiver an ARENA object (`Value::Obj`)?
    /// * does its class classify as `fk::REF_ARRAY_MAP` — the SAME memoised verdict the
    ///   shipped refmap arms key on (`fu_kind_of`, keyed by the arena class id, derived from
    ///   the exact class name once per class per thread)? A subclass that overrides the ctor
    ///   classifies as a refusal and runs its own body.
    /// * is the site's selector exactly `<init>`/`(I)V` (the caller's own pool, the same
    ///   `member_ref` the marshal below reads)?
    /// * do the `(key, value, size)` field ids resolve on this class with all three layout
    ///   slots present (`map::arraymap_ids` — the class's own resolved fields, never a literal;
    ///   an off-layout `arena_putfield` is a PANIC, so the proof is mandatory)?
    ///
    /// The body then performs exactly the writes the resolved body performs: two
    /// `Object[capacity]` arrays into the fresh receiver's `key`/`value` cells and `size=0`
    /// (the born-zero cell, written for parity with the `DoubleArrayList.wrap` precedent).
    /// A negative capacity declines — fastutil's `anewarray` would throw
    /// `NegativeArraySizeException` and the general path raises the identical one.
    ///
    /// ⚠GC: `heap.alloc_array` can collect, and the receiver is a raw `ArenaPtr` a collection
    /// would move — so the receiver rides a `NativeArgsGuard` whose slice the collector
    /// forwards in place, each fresh array is rooted in `gc_protect` (a Rust-local handle is
    /// invisible to the root walk — the `stream/dispatch.rs` forEach hazard note), and the
    /// receiver is RE-READ from the guarded cell after the second allocation, never from `p`.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn r2omap_ctor_fastpath(
        &mut self,
        caller_ptr: *const crate::class_loader::LoadedClass,
        cp: u16,
        raw: &[i64],
    ) -> Option<i64> {
        let caller: &crate::class_loader::LoadedClass = unsafe { &*caller_ptr };
        let m = caller.file.pool.member_ref(cp)?;
        if m.name != "<init>" || m.descriptor != "(I)V" {
            return None;
        }
        let cap = *raw.get(1)? as i32;
        if cap < 0 {
            return None; // fastutil throws NASE on anewarray — let the general path raise it
        }
        let recv = jit::unbox_ref(self, *raw.first()?, "");
        let Value::Obj(p) = recv else {
            return None;
        };
        // THE MEMOISED CLASS-WORD VERDICT the shipped refmap arms key on — no name compare.
        let (code, ..) = self.fu_kind_of(&recv, "<init>");
        if code != fk::REF_ARRAY_MAP {
            return None;
        }
        let cid = crate::nextgen::arena_class_id(p);
        let (kid, vid, sid) = crate::map::arraymap_ids(self, cid)?;
        // The layout proof IS `arraymap_ids`' own admission test (three `slot_of`s), the same
        // argument the refmap arm's debug_assert records; an off-layout `arena_putfield` is a
        // PANIC, so a `None` here declines rather than risks one.
        debug_assert!(
            crate::nextgen::slot_of(cid, kid).is_some()
                && crate::nextgen::slot_of(cid, vid).is_some()
                && crate::nextgen::slot_of(cid, sid).is_some(),
            "arraymap_ids admitted a class whose layout lacks key/value/size"
        );
        let mut guarded = [Value::Obj(p), Value::Null];
        let _g = crate::natives::entry::NativeArgsGuard::new(&guarded);
        let ka = self.heap.alloc_array("Ljava/lang/Object;", cap as usize);
        let _gk = crate::stream::gc_protect_array(self, ka);
        let va = self.heap.alloc_array("Ljava/lang/Object;", cap as usize);
        let _gv = crate::stream::gc_protect_array(self, va);
        // The receiver may have moved under the second allocation: re-read it from the
        // GUARDED cell, which the collector forwards in place (Wall #19). The guards all pop
        // themselves on drop (reverse push order), including on the decline paths above.
        let Value::Obj(p) = guarded[0] else {
            return None;
        };
        self.write_value_field(&Value::Obj(p), kid, Value::Array(ka));
        self.write_value_field(&Value::Obj(p), vid, Value::Array(va));
        self.write_value_field(&Value::Obj(p), sid, Value::Int(0));
        crate::diag::r2omap_ctor_op().inc();
        Some(0) // `void` return: the descriptor ends in `V`, so the packed result is unused.
    }

    /// Stage 3: native `iterator()` for a fastutil double list. `AbstractDoubleList.iterator()`
    /// otherwise builds a real `DoubleArrayList$1` and drives it bytecode-by-bytecode
    /// (`$1.<init>`/`hasNext`/`nextDouble`/`listIterator(I)`/`ensureIndex` = the boot iterator
    /// swamp, ~470k interpreted entries, JIT never tiers it). Return the SAME native
    /// `$elems`/`$pos` iterator the JIT fast path builds (served forward by `iter_op` with no
    /// synthetic frame). `iterator()` is the forward-only `Iterator` contract — read-only, so
    /// this is bit-exact (identical doubles, identical order); the mutation-capable
    /// `listIterator()` is deliberately NOT intercepted. Immutable list shares its backing
    /// array (zero copy, never mutated); an over-allocated array list copies its logical
    /// [0,size) prefix so the tail isn't iterated. None (→ bytecode) if `recv` isn't a double list.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn dal_iterator_native(&mut self, recv: &Value) -> Option<Value> {
        // Classify the receiver while its class name is borrowed; the borrow ends with the
        // match (returning a bool), so the mutating reads below are free of it.
        let is_dal = match recv {
            Value::Ref(h) => match self.heap.class_of(*h) {
                Some("it/unimi/dsi/fastutil/doubles/DoubleArrayList") => true,
                Some("it/unimi/dsi/fastutil/doubles/DoubleImmutableList") => false,
                _ => return None,
            },
            Value::Obj(p) => {
                match crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)).as_deref() {
                    Some("it/unimi/dsi/fastutil/doubles/DoubleArrayList") => true,
                    Some("it/unimi/dsi/fastutil/doubles/DoubleImmutableList") => false,
                    _ => return None,
                }
            }
            _ => return None,
        };
        let (h, len) = self.fu_double_view(recv, is_dal)?;
        let arr = if !is_dal {
            h
        } else {
            let mut elems: Vec<Value> = Vec::with_capacity(len);
            for i in 0..len {
                elems.push(self.heap.array_get(h, i).unwrap_or(Value::Double(0.0)));
            }
            self.heap.alloc_values("double", elems)
        };
        let it = self.heap.alloc("java/util/Iterator");
        self.heap.put_field_syn(it, crate::heap::Syn::Elems, Value::Array(arr));
        self.heap.put_field_syn(it, crate::heap::Syn::Pos, Value::Int(0));
        Some(Value::Ref(it))
    }
}

/// Sampled (receiver class, method) census for the `fu_fastpath` population - the instrument the
/// `List.size()` misfire proved was missing. Printed by `fu_recv_report`.
static FU_RECV: std::sync::Mutex<Option<std::collections::HashMap<(String, String), u64>>> =
    std::sync::Mutex::new(None);

pub(crate) fn fu_recv_note(cls: &str, name: &str) {
    let mut g = FU_RECV.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(std::collections::HashMap::new);
    *m.entry((cls.to_string(), name.to_string())).or_insert(0) += 1;
}

/// Top `n` (class, method) pairs by sampled count, each multiplied back by the sampling rate.
pub(crate) fn fu_recv_report(n: usize) -> String {
    let g = FU_RECV.lock().unwrap_or_else(|e| e.into_inner());
    let Some(m) = g.as_ref() else { return String::new() };
    let mut v: Vec<(&(String, String), &u64)> = m.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1));
    v.into_iter()
        .take(n)
        .map(|((c, nm), k)| format!("{c}#{nm}={}", k * 256))
        .collect::<Vec<_>>()
        .join("  ")
}

impl Vm {
    /// **`equals` FOR TWO fastutil DOUBLE LISTS, COMPARED OFF THEIR BACKING ARRAYS.**
    ///
    /// `RAVA_INTERP_WHO` on a datapack run put `DoubleArrayList.equals` at **301,834 interpreted
    /// entries** across its two overloads — the top of the census once the guest-subclass compile
    /// fix landed, and out of that fix's reach because this class's own name is in a
    /// natively-served namespace.
    ///
    /// ⚠**The engine already routed these to the GUEST body ON PURPOSE, and the reason is
    /// recorded in `natives/misc.rs`: offering them to the collection native first measured
    /// +15.7%** — because `coll_op`'s `equals` MATERIALISES both collections into `Vec<Value>`
    /// and derives a content key per element, which is more work than interpreting fastutil's own
    /// loop. That verdict is about THAT implementation, not about the operation: this one reads
    /// the two backing arrays directly and short-circuits on the first differing element, which
    /// is the shape the same note calls right.
    ///
    /// Semantics are fastutil's: lengths first, then `a[i] != b[i]` element by element. Rust's
    /// `!=` on `f64` is the same IEEE comparison Java's is — NaN differs from itself, `+0.0`
    /// equals `-0.0` — so the answer is identical.
    ///
    /// ⭐**`CubePointRange` IS ADMITTED AS A VIRTUAL DOUBLE LIST** (the fu-miss census's
    /// runner-up pair: the `Objects.equals` rows over a CPR operand, 217,876 census events a
    /// reload). Its `size()` is `parts+1` and its `getDouble(i)` is `(double)i / (double)parts`
    /// (javap, 1.21.11) — a `DoubleList` whose elements are COMPUTED, so the view needs no
    /// backing array and no allocation: `parts` is read once (`cube_parts`, the same memoised
    /// owner-qualified id the `CubeRange` arm uses), the length is `parts+1`, and element `i`
    /// is the same `i/parts` quotient the arm's `getDouble` serves. Exactness carries to the
    /// edge: a `parts < 1` receiver declines (`cube_parts` requires `n > 0`, matching the
    /// class's own ctor, which throws below 1), so the arm never computes a `0/0` NaN — and if
    /// it ever did, `NaN != NaN` answers `false`, the same verdict the guest body's `dcmpl`
    /// produces for a NaN operand (dcmpl yields 1, `ifeq` falls through to `iconst_0`).
    ///
    /// ⚠**BOTH operands must be double-list views, exactly as before** — a plain
    /// `java/util/List` other declines (`None`), because the guest body compares that path by
    /// BOXED `Object.equals` (`Double.equals`), whose NaN/+0.0/-0.0 verdicts differ from
    /// `dcmpl`. The widening only adds CPR to the admitted set; every other shape keeps the
    /// answer it had.
    ///
    /// `None` means "not two double lists", and every caller then does exactly what it did
    /// before. `RAVA_NO_DLIST_EQ=1` disables it.
    pub(crate) fn fu_double_list_equals(&mut self, a: &Value, b: &Value) -> Option<bool> {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ON.get_or_init(|| std::env::var_os("RAVA_NO_DLIST_EQ").is_none()) {
            return None;
        }
        // One operand's element source: a backing `double[]` (with its logical length), or
        // CubePointRange's VIRTUAL view (element `i` computed as `i/parts`, no allocation).
        enum Side {
            Arr { h: u32, len: usize },
            #[cfg(feature = "next-gen-gc")]
            Cube { parts: i32 },
        }
        let side_of = |vm: &mut Vm, v: &Value| -> Option<Side> {
            let name: Option<String> = match v {
                Value::Ref(h) => vm.heap.class_of(*h).map(str::to_string),
                #[cfg(feature = "next-gen-gc")]
                Value::Obj(p) => {
                    crate::nextgen::class_name(crate::nextgen::arena_class_id(*p)).map(|s| s.to_string())
                }
                _ => None,
            };
            match name.as_deref() {
                // `true` = an array list, whose logical length is its `size` field; `false` = an
                // immutable list, whose backing array IS its contents.
                Some("it/unimi/dsi/fastutil/doubles/DoubleArrayList") => {
                    let (h, len) = vm.fu_double_view(v, true)?;
                    Some(Side::Arr { h, len })
                }
                Some("it/unimi/dsi/fastutil/doubles/DoubleImmutableList") => {
                    let (h, len) = vm.fu_double_view(v, false)?;
                    Some(Side::Arr { h, len })
                }
                // The virtual view — `parts` off the same memoised owner-qualified id the
                // `CubeRange` arm reads (`cube_parts` declines a non-positive cell, which the
                // class's own ctor makes unreachable).
                #[cfg(feature = "next-gen-gc")]
                Some("net/minecraft/world/phys/shapes/CubePointRange") => {
                    Some(Side::Cube { parts: vm.cube_parts(v)? })
                }
                _ => None,
            }
        };
        let sa = side_of(self, a)?;
        let sb = side_of(self, b)?;
        let len_of = |s: &Side| match s {
            Side::Arr { len, .. } => *len,
            #[cfg(feature = "next-gen-gc")]
            Side::Cube { parts } => parts.saturating_add(1) as usize,
        };
        let (la, lb) = (len_of(&sa), len_of(&sb));
        if la != lb {
            crate::diag::dlist_eq().inc();
            return Some(false);
        }
        for i in 0..la {
            // The element of a virtual view is the SAME `i / parts` quotient the `CubeRange`
            // arm's `getDouble` computes; an array view reads its backing cell.
            let elem = |vm: &Vm, s: &Side, i: usize| -> f64 {
                match s {
                    Side::Arr { h, .. } => vm
                        .heap
                        .array_get(*h, i)
                        .map(|v| v.as_f64())
                        .unwrap_or(0.0),
                    #[cfg(feature = "next-gen-gc")]
                    Side::Cube { parts } => f64::from(i as i32) / f64::from(*parts),
                }
            };
            let x = elem(self, &sa, i);
            let y = elem(self, &sb, i);
            if x != y {
                crate::diag::dlist_eq().inc();
                return Some(false);
            }
        }
        crate::diag::dlist_eq().inc();
        Some(true)
    }
}

// ═══════════ THE STRUCTURAL RING-SIZE FAST PATH (kill switch `RAVA_NO_RING_SIZE=1`) ═══════════
//
// ⭐**THE LIGHTING BFS QUEUE'S `size()`/`isEmpty()` — 3.2 M CALLS A BOOT — WAS PAYING THE FULL
// COMPILED→HOST CROSSING TO RUN 25 BYTES OF ARITHMETIC.** The concrete call site re-enters
// `jit_call_body` after the PIC/native gauntlets refuse it (the queue is a plain guest class:
// no `$elems`, no compiled direct entry), and the synthetic frame the crossing then builds is
// the whole cost. The body it would run is a ring-buffer length computation:
//
//     n = end - start; if (n < 0) n += length; return n
//
// — three `int` field reads and two arithmetic ops. This arm answers it at the choke, with no
// frame and no gauntlet, for BOTH representations.
//
// ⛔**THE ADMISSION IS STRUCTURAL, AND THAT IS THE WHOLE SOUNDNESS ARGUMENT.** Nothing here
// names a guest class, field, package or method; no `fu_routable` owner admission exists for
// it; no selector literal is consulted for admission. Every call through the hook resolves the
// site's own `(name, descriptor)` selector against the RECEIVER'S ACTUAL CLASS via
// `Vm::resolve_method` — the same resolution the general path performs — and then requires the
// resolved target's own bytecode to be, at every offset, one of two exact shapes:
//
//   * the 25-byte ring body: `aload_0; getfield a; aload_0; getfield b; isub; istore_1;
//     iload_1; iflt L; iload_1; goto R; L: aload_0; getfield c; iload_1; iadd; R: ireturn`,
//     with `L = 18`, `R = 24`, no exception table, and all three `Fieldref`s of descriptor `I`
//     owned by the size-owner class itself;
//   * the 15-byte canonical isEmpty body: `aload_0; invokeinterface #m, 1; ifne T;
//     iconst_1; goto R; T: iconst_0; R: ireturn` — whose invoked selector must resolve,
//     from the receiver's own class, to the SAME target the ring verification then runs on.
//     An `isEmpty` that is overridden anywhere in the chain resolves to a different body,
//     fails the byte match, and declines.
//
// Any other zero-arg body — a plain field read, a literal answer, a loop — fails the
// exact-byte compare and this arm never engages. The verdict is memoised per thread in
// `ring_memo`, keyed by the (RECEIVER CLASS, RESOLVED TARGET) PAIR — both stable
// process-lifetime identities, so the pair pins exactly the resolution the byte
// verification ran on. The pair, not the class alone, is load-bearing: the same class
// carries other `()I` selectors this arm must refuse (`capacity` is a plain field read), and
// a class-only key would hand one selector's verdict to another. The memoised row carries
// all three field ids, derived from the VERIFIED body's own `Fieldref`s
// (`resolve_field_id(fr.owner, fr.name)`), never from a name literal.
//
// ⛔**THE PREVIOUS RING ARM WAS REVERTED FOR EXACTLY THE THREE DEFECTS THIS ONE REMOVES**:
// it routed by hardcoded owner name, it memoised three field ids in process-global
// `OnceLock`s (a single-class global), and it matched field NAMES. Here the memo is
// per-(class, target)-pair per thread, the ids come from bytecode refs, and the gate
// consults the actual resolved dispatch, not a name.
//
// **Semantics are the bytecode's, verbatim**: `isub` wraps (Rust `wrapping_sub` is the same
// two's-complement arithmetic), `iflt` selects the `+ length` arm, and the answer's KIND is
// the MATCHED BODY's, never the site descriptor's — the ring shape returns the raw int even
// if an unusual `()Z` descriptor references it, and the canonical shape answers 0/1 even
// under `()I`. The kind is memoised in the same row as the field ids, so a memo hit answers
// identically to the first derivation. A null receiver, a non-int field cell, or an arena
// receiver whose layout lacks one of the three slots declines to the unchanged general path
// (the layout case UNMEMOISED, re-asked next call — a layout is only ever appended to). The
// fastpath never throws: every failure is a `None` and the general path produces the
// identical exception the bytecode would.
/// The pair key for `ring_memo`: the receiver-class key (`fu_kind_of`'s encoding) mixed with
/// the resolved target's `Arc` pointer. Both halves are stable process-lifetime identities.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn ring_memo_key(recv_key: u64, target_ptr: u64) -> u64 {
    (recv_key ^ target_ptr.rotate_left(17)).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1
}

#[cfg(feature = "next-gen-gc")]
impl Vm {
    /// The body of the ring shape, byte-for-byte. Returns the three `getfield` pool indices
    /// (minuend at bc[1], subtrahend at bc[5], wrap-addend at bc[19]) on an exact match.
    ///
    /// Exact bytes (verified against the jar):
    /// `2a b4 hi lo 2a b4 hi lo 64 3c 1b 9b 00 07 1b a7 00 09 2a b4 hi lo 1b 60 ac`
    /// — `iflt 18` is `9b 00 07` (18 = 11 + 7), `goto 24` is `a7 00 09` (24 = 15 + 9).
    fn ring_size_body_fields(bc: &[u8]) -> Option<(u16, u16, u16)> {
        if bc.len() != 25
            || bc[0] != 0x2a
            || bc[1] != 0xb4
            || bc[4] != 0x2a
            || bc[5] != 0xb4
            || bc[8] != 0x64
            || bc[9] != 0x3c
            || bc[10] != 0x1b
            || bc[11] != 0x9b
            || bc[12] != 0x00
            || bc[13] != 0x07
            || bc[14] != 0x1b
            || bc[15] != 0xa7
            || bc[16] != 0x00
            || bc[17] != 0x09
            || bc[18] != 0x2a
            || bc[19] != 0xb4
            || bc[22] != 0x1b
            || bc[23] != 0x60
            || bc[24] != 0xac
        {
            return None;
        }
        let f = |i: usize| ((bc[i] as u16) << 8) | bc[i + 1] as u16;
        Some((f(2), f(6), f(20)))
    }

    /// The canonical `size()==0` delegation body, byte-for-byte. Returns the invoked
    /// interface-method pool index on an exact match.
    ///
    /// Exact bytes (verified against the jar):
    /// `2a b9 hi lo 01 00 9a 00 07 04 a7 00 04 03 ac`
    /// — `invokeinterface #m, 1` with the reserved byte `00`; `ifne 13` is `9a 00 07`
    /// (13 = 6 + 7); `goto 14` is `a7 00 04` (14 = 10 + 4).
    fn canonical_empty_body_invokee(bc: &[u8]) -> Option<u16> {
        if bc.len() != 15
            || bc[0] != 0x2a
            || bc[1] != 0xb9
            || bc[4] != 0x01
            || bc[5] != 0x00
            || bc[6] != 0x9a
            || bc[7] != 0x00
            || bc[8] != 0x07
            || bc[9] != 0x04
            || bc[10] != 0xa7
            || bc[11] != 0x00
            || bc[12] != 0x04
            || bc[13] != 0x03
            || bc[14] != 0xac
        {
            return None;
        }
        Some(((bc[2] as u16) << 8) | bc[3] as u16)
    }

    /// The direct field-test `isEmpty` body, byte-for-byte. Returns the `getfield`
    /// pool index on an exact match.
    ///
    /// Exact bytes (verified against fastutil 8.5.18 `LongOpenHashSet`,
    /// `ReferenceOpenHashSet`, `ObjectArrayList`):
    /// `2a b4 hi lo 9a 00 07 04 a7 00 04 03 ac`
    /// — `ifne 11` is `9a 00 07` (11 = 4 + 7); `goto 12` is `a7 00 04` (12 = 8 + 4).
    fn field_empty_body_field(bc: &[u8]) -> Option<u16> {
        if bc.len() != 13
            || bc[0] != 0x2a
            || bc[1] != 0xb4
            || bc[4] != 0x9a
            || bc[5] != 0x00
            || bc[6] != 0x07
            || bc[7] != 0x04
            || bc[8] != 0xa7
            || bc[9] != 0x00
            || bc[10] != 0x04
            || bc[11] != 0x03
            || bc[12] != 0xac
        {
            return None;
        }
        Some(((bc[2] as u16) << 8) | bc[3] as u16)
    }

    /// Decode the receiver cell and its receiver-class key — the shared front half of the
    /// ring derive below and the leaf `isEmpty` serve, so the two arms can never disagree
    /// about which class a receiver belongs to. The key is `ring_memo`'s encoding: the
    /// heap object's class WORD for a `Ref` (with the swept-tombstone `u64::MAX` bail the
    /// derive always made), `(class_id << 1) | 1` for an arena `Obj`. A null or a non-ref
    /// declines. PURE READS ONLY — the leaf serve calls this unspilled.
    fn ring_recv_decode(&self, cell: i64, owner: &str) -> Option<(Value, u64)> {
        let recv = jit::unbox_ref(self, cell, owner);
        let key = match &recv {
            Value::Ref(h) => {
                let w = self.heap.object_class_word(*h)?;
                if w as u64 == u64::MAX {
                    return None;
                }
                w as u64
            }
            Value::Obj(p) => {
                let cid = crate::nextgen::arena_class_id(*p);
                ((cid as u64) << 1) | 1
            }
            _ => return None,
        };
        Some((recv, key))
    }

    /// Serve the ring `size()`/`isEmpty()` structurally, or return `None` for the general
    /// path. Called from the `jit_call_body` choke for ANY zero-argument instance call — the
    /// selector gate is the resolution itself, not a literal.
    pub(crate) fn ring_size_fastpath(
        &mut self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        raw: &[i64],
    ) -> Option<i64> {
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        // The site's ACTUAL selector, from the caller's own pool. `raw.len() == 1` is the
        // caller's precondition (a zero-arg instance call), so `raw[0]` is the receiver.
        let m = caller.file.pool.member_ref(cp)?;
        // Zero-arg and boolean-or-int return, from the descriptor alone: the two shapes this
        // arm can answer both are `()`-shaped. Anything wider is declined before any resolve.
        if !m.descriptor.starts_with("()") || !m.descriptor.ends_with("I") && !m.descriptor.ends_with("Z") {
            return None;
        }
        // Resolve the receiver. A null takes the general path (its NPE is the bytecode's).
        // The decode front half is shared with the LEAF `isEmpty` serve (`ring_recv_decode`),
        // so the two arms read the same class key for the same receiver.
        let (recv, key) = self.ring_recv_decode(*raw.first()?, m.owner)?;
        let cls = match &recv {
            Value::Ref(h) => self.heap.class_of(*h)?.to_string(),
            Value::Obj(p) => {
                crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))?.to_string()
            }
            _ => return None,
        };
        let arena = matches!(recv, Value::Obj(_));
        // THE SITE KEY: (caller class, cp, receiver class). Written ONLY at successful
        // serves below, so the unspilled LEAF twin can answer the same site+receiver-class
        // pair from a memo row without ever resolving. Refusals put nothing: a refusal's
        // verdict may be transient (lazy backing) and must not stick to the site row.
        let site = site_memo::site_key(caller_ptr, cp);
        // Resolve the site's selector against the receiver's ACTUAL class — the identical
        // dispatch the general path would perform. `Err` (a GIL-free worker's region abort)
        // and `Ok(None)` (no body anywhere in the chain) both decline to the general path.
        let Ok(Some(target)) = self.resolve_method(&cls, m.name, m.descriptor) else {
            return None;
        };
        // THE PAIR KEY: (receiver class, RESOLVED TARGET). The target half is the declaring
        // class's identity word — the `Arc<LoadedClass>` pointer, the same stable identity the
        // whole engine keys caches on. It pins exactly the pair the ring verification ran on,
        // so `size` can be served while `capacity` (a different target) re-derives and is
        // refused by the byte compare.
        let pair = ring_memo_key(key, Arc::as_ptr(&target) as u64);
        if let Some((kind, f0, f1, f2)) = ring_memo::get(pair) {
            if kind == ring_memo::FIELD_BOOL {
                // Native-remodelled receivers (int sets, object maps) answer from the
                // native layer, not the declared cell — the probe is the backing's
                // presence, a routing fact, never a name. UNMEMOISED decline: backing
                // mints lazily, so today's absence is not tomorrow's (the getter
                // arm's gate, same placement and same reason).
                if self.ring_recv_backed(&recv) {
                    return None;
                }
                // The single-cell read of a verified direct field-test body. `f0` is the
                // one derived field id. A non-`Int` cell declines to the general path.
                let n = self.ring_cell_read(&recv, f0)?;
                site_memo::put(site, key, kind, f0, f1, f2);
                return Some(jit::pack_local(&Value::Int((n == 0) as i32)));
            }
            if matches!(kind, ring_memo::RING_INT | ring_memo::CANON_BOOL) {
                // Verified at derive. The boolean-vs-int answer KIND comes from the MEMO,
                // not from the site's descriptor: it is a property of the matched body
                // shape (the ring body `ireturn`s the raw length, the canonical body
                // answers 0/1), and storing it keeps a hit identical to the first
                // derivation.
                let r = self.ring_size_serve(&recv, f0, f1, f2)?;
                site_memo::put(site, key, kind, f0, f1, f2);
                return Some(jit::pack_local(&Value::Int(
                    if kind == ring_memo::CANON_BOOL { (r == 0) as i32 } else { r },
                )));
            }
            // A memoised structural refusal for this (class, target) pair is permanent —
            // the bytecode is immutable.
            return None;
        }
        let Some(mi) = target.file.method(m.name, m.descriptor) else { return None };
        let Some(code) = mi.code.as_ref() else { return None };
        // No exception handler may sit on the body we emulate.
        if !code.exceptions.is_empty() {
            return None;
        }
        let bc = &code.bytecode[..];
        // THIRD admissible shape, checked first: the direct 13-byte field-test `isEmpty`
        // (`LongOpenHashSet`/`ReferenceOpenHashSet`/`ObjectArrayList`). Derive the single
        // `I` field id from the body's own Fieldref and answer from one cell read. The
        // memo row carries FIELD_BOOL with `f0` = the field id.
        if let Some(f) = Self::field_empty_body_field(bc) {
            // Same gate as the hit path: a native-remodelled receiver answers from the
            // native layer, not the declared cell. UNMEMOISED decline — backing mints
            // lazily, so a refusal here must not stick to the (class, target) row.
            if self.ring_recv_backed(&recv) {
                return None;
            }
            let fr = target.file.pool.member_ref(f)?;
            if fr.descriptor != "I" || fr.owner != target.file.this_class {
                ring_memo::put(pair, ring_memo::REFUSAL, 0, 0, 0);
                return None;
            }
            let fid = self.resolve_field_id(fr.owner, fr.name);
            if arena {
                let cid = (key >> 1) as u32;
                if crate::nextgen::slot_of(cid, fid).is_none() {
                    return None;
                }
            }
            ring_memo::put(pair, ring_memo::FIELD_BOOL, fid, 0, 0);
            let n = self.ring_cell_read(&recv, fid)?;
            site_memo::put(site, key, ring_memo::FIELD_BOOL, fid, 0, 0);
            return Some(jit::pack_local(&Value::Int((n == 0) as i32)));
        }
        // Two other admissible shapes. The ring body serves the site directly; the canonical
        // isEmpty body delegates — its invoked selector must resolve, from the SAME receiver
        // class, to a ring body (which is then what serves the site, as `n == 0`).
        //
        // `ring_owner` is the class whose POOL the ring body's `getfield` operands index —
        // the ring body's own declaring class — and the one the field ids are verified
        // against. `kind` is the ANSWER KIND OF THE SHAPE THAT MATCHED, never a function of
        // the site's descriptor: the ring body `ireturn`s the raw length even if an unusual
        // `()Z` descriptor references it, and the canonical body answers 0/1 even under
        // `()I`. It rides in the memo row, so a hit answers identically.
        let (ring_owner, end_f, start_f, len_f, kind) =
            if let Some((a, b, c)) = Self::ring_size_body_fields(bc) {
                (Arc::clone(&target), a, b, c, ring_memo::RING_INT)
            } else if let Some(v) = Self::canonical_empty_body_invokee(bc) {
                // The invoked selector, read from the CANONICAL BODY'S OWN POOL — not a
                // literal. Re-resolved against the RECEIVER's class: the exact dispatch the
                // body itself would perform.
                let sel = target.file.pool.member_ref(v)?;
                let Ok(Some(size_owner)) = self.resolve_method(&cls, sel.name, sel.descriptor)
                else {
                    return None;
                };
                let smi = size_owner.file.method(sel.name, sel.descriptor)?;
                let scode = smi.code.as_ref()?;
                if !scode.exceptions.is_empty() {
                    return None;
                }
                let (a, b, c) = Self::ring_size_body_fields(&scode.bytecode[..])?;
                (size_owner, a, b, c, ring_memo::CANON_BOOL)
            } else {
                // Not a shape this arm emulates. Memoise the structural refusal for exactly
                // this (class, target) pair — the bytecode is immutable, so the verdict is
                // permanent for it.
                ring_memo::put(pair, ring_memo::REFUSAL, 0, 0, 0);
                return None;
            };
        // Derive the three ids from the RING body's own Fieldrefs — of descriptor `I` and
        // owned by the declaring class itself.
        let (end_id, start_id, len_id) =
            self.ring_derive_ids(&ring_owner, end_f, start_f, len_f)?;
        // Arena layout check: an off-layout `arena_getfield` is a PANIC, so a missing slot
        // declines UNMEMOISED and is re-asked next call — a layout is only ever appended
        // to (`nextgen::slot_of` caches only positive answers for the same reason).
        if arena {
            let cid = (key >> 1) as u32;
            if crate::nextgen::slot_of(cid, end_id).is_none()
                || crate::nextgen::slot_of(cid, start_id).is_none()
                || crate::nextgen::slot_of(cid, len_id).is_none()
            {
                return None;
            }
        }
        ring_memo::put(pair, kind, start_id, end_id, len_id);
        let r = self.ring_size_serve(&recv, start_id, end_id, len_id)?;
        site_memo::put(site, key, kind, start_id, end_id, len_id);
        Some(jit::pack_local(&Value::Int(
            if kind == ring_memo::CANON_BOOL { (r == 0) as i32 } else { r },
        )))
    }

    /// Resolve the ring body's three `getfield` Fieldrefs to field ids, verifying each is an
    /// `I` field owned by the declaring class itself. The ids come from the refs' own owner
    /// and name — no literal is consulted.
    fn ring_derive_ids(
        &self,
        owner: &Arc<LoadedClass>,
        end_f: u16,
        start_f: u16,
        len_f: u16,
    ) -> Option<(u32, u32, u32)> {
        let pool = &owner.file.pool;
        let mut ids = [0u32; 3];
        for (i, idx) in [end_f, start_f, len_f].into_iter().enumerate() {
            let fr = pool.member_ref(idx)?;
            if fr.descriptor != "I" || fr.owner != owner.file.this_class {
                return None;
            }
            ids[i] = self.resolve_field_id(fr.owner, fr.name);
        }
        Some((ids[0], ids[1], ids[2]))
    }

    /// Read the three int cells and compute the ring length, per the verified body:
    /// `n = end - start` (wrapping, as `isub` is), `if n < 0 { n += length }`. Any non-`Int`
    /// cell declines rather than coercing — the general path re-reads the fields and its
    /// behaviour (including any exception) is unchanged. The caller selects the answer's
    /// KIND from the matched body shape; this computes the ring length either way.
    fn ring_size_serve(
        &self,
        recv: &Value,
        start_id: u32,
        end_id: u32,
        len_id: u32,
    ) -> Option<i32> {
        let start = self.ring_cell_read(recv, start_id)?;
        let end = self.ring_cell_read(recv, end_id)?;
        let n = end.wrapping_sub(start);
        Some(if n < 0 { n.wrapping_add(self.ring_cell_read(recv, len_id)?) } else { n })
    }

    /// Read ONE `I` cell from either receiver representation, declining (never coercing) on
    /// anything but a `Value::Int`. Shared by the ring serve and the direct field-test
    /// serve; the arena layout check happened at derive on both paths, and the
    /// non-panicking variant still declines if the slot is somehow absent.
    fn ring_cell_read(&self, recv: &Value, id: u32) -> Option<i32> {
        let v = match recv {
            Value::Ref(h) => self.heap.get_field_by_id(*h, id),
            Value::Obj(p) => crate::opcodes::arena_getfield_opt(*p, id)?,
            _ => return None,
        };
        match v {
            Value::Int(n) => Some(n),
            _ => None,
        }
    }

    /// Is this receiver served by the native collection layer? The probe is the PRESENCE
    /// of an engine-minted backing (`$map` / `$elems` / the arena-map class word), which
    /// is exactly the routing fact the natives read on every call — no guest class, field
    /// or method name is consulted. A backed receiver's declared `size` cell is whatever
    /// the bytecode last left there (the natives never write it), so reading it would be
    /// a WRONG answer, not a slow one. Mirrors `getter_fastpath`'s gate, including the
    /// lazy-mint discipline: the caller declines UNMEMOISED because absence today is not
    /// absence tomorrow.
    fn ring_recv_backed(&self, recv: &Value) -> bool {
        match recv {
            Value::Ref(h) => {
                matches!(self.heap.get_field_syn(*h, crate::heap::Syn::Map), Value::Int(_))
                    || matches!(
                        self.heap.get_field_syn(*h, crate::heap::Syn::Elems),
                        Value::Array(_)
                    )
                    || matches!(
                        self.heap.field_id_opt("$inverseof"),
                        Some(id) if matches!(self.heap.get_field_by_id(*h, id), Value::Ref(_))
                    )
            }
            Value::Obj(p) => {
                if crate::arena_map::is_arena_map(*p) {
                    return true;
                }
                let cid = crate::nextgen::arena_class_id(*p);
                [crate::heap::Syn::Map, crate::heap::Syn::Elems]
                    .into_iter()
                    .filter_map(|s| self.heap.syn_field_id(s))
                    .any(|id| crate::nextgen::slot_of(cid, id).is_some())
            }
            _ => true,
        }
    }

    /// ⭐**THE UNSPILLED LEAF `isEmpty()Z` SERVE — `pic_native::OP_FU_ISEMPTY`'s body.**
    ///
    /// Serves the census population `heappic_fill`'s own comment prices at **8.39 M of the
    /// 9.06 M heap-Ref misses per boot** — `isEmpty` on `LongOpenHashSet` (3.42 M),
    /// `LongArrayFIFOQueue` (3.11 M), `ReferenceOpenHashSet` (933 k), `ObjectArrayList`
    /// (931 k) — from inside the LEAF native arm, i.e. with the caller's references NOT
    /// spilled: no `spill_for_gc`, no `reload_locals_after_gc`, no `reload_ref_slot`, no
    /// `cells_push`. Every call that would otherwise cross into `rava_dispatch_miss` →
    /// negative probe → `jit_call` → `try_invoke_chain` is answered by one memo probe and
    /// at most three cell reads.
    ///
    /// **THE ONE SOUNDNESS RULE: HIT-ONLY, OFF A ROW `ring_size_fastpath` DERIVED.** This
    /// arm must NEVER resolve: `resolve_method` can load a class, run its `<clinit>` and
    /// COLLECT — moving the very unspilled references the caller holds — and the caller
    /// reloads nothing because no collection is possible on this arm. So the arm answers
    /// ONLY from a `site_memo` row, which is written exclusively at a successful
    /// `ring_size_fastpath` serve of the SAME (caller class, cp, receiver class) key — a
    /// serve that ran under a spilled caller and resolved everything once. A memo miss is a
    /// plain refusal: the site's `miss` block spills and takes today's path, and that path
    /// (`jit_call` → the ring gate) re-derives and writes the row. Because the descriptor
    /// is bound on the SAME first miss that runs the ring gate's serve, the second call
    /// onward serves here — zero steady-state refusals for this population.
    ///
    /// **GC-SAFETY OF THE UNSPILLED CALL** (the argument the emit side relies on): every
    /// read below is pure — the class word, memo rows, `ring_cell_read`'s field cells,
    /// `ring_size_serve`'s arithmetic, `ring_recv_backed`'s presence probes. None births a
    /// guest object, so no nursery bump, no allocation request, no collection: the caller's
    /// unspilled references cannot move under the call. A refusal happens BEFORE any
    /// mutation, so the site's `miss` re-spill of the same references is idempotent.
    ///
    /// **THE ANSWER IS THE BYTECODE'S, VERBATIM.** The row stores the (kind, f0, f1, f2)
    /// the derive answered from: FIELD_BOOL answers `(cell == 0)` off the single verified
    /// field; CANON_BOOL answers `(ringlen == 0)` off the three verified cells; both
    /// replay the identical computation the derive performed, on the identical ids. A
    /// RING_INT row is deliberately NOT served here: the ring body `ireturn`s the RAW
    /// length, and a raw length answering a `()Z` site is the matched body's semantics —
    /// not something this arm re-derives from the row's kind half; leave it to the rooted
    /// path. `ring_recv_backed` mirrors the derive's gate: a receiver that gained a native
    /// backing since the derive REFUSES (unmemoised), exactly as the derive's own hit path
    /// declines, so a lazily-minted backing re-routes to the general path.
    pub(crate) fn fu_isempty_leaf_serve(
        &self,
        caller_ptr: *const LoadedClass,
        cp: u16,
        raw: &[i64],
    ) -> Option<i64> {
        if self.trampoline {
            return None;
        }
        let caller: &LoadedClass = unsafe { &*caller_ptr };
        // House re-verify pattern (`ws_optional`): the classifier keyed on (name, desc), so
        // re-read the site's own selector and gate on it — a wrong binding can only refuse.
        let m = caller.file.pool.member_ref(cp)?;
        if m.name != "isEmpty" || m.descriptor != "()Z" {
            return None;
        }
        if raw.len() != 1 {
            return None;
        }
        // Decode the receiver and its class key — the SAME shared front half
        // (`ring_recv_decode`) the derive uses, so a row written by the derive is found
        // by the serve for the identical receiver.
        let (recv, key) = self.ring_recv_decode(raw[0], m.owner)?;
        let (kind, f0, f1, f2) = site_memo::get(site_memo::site_key(caller_ptr, cp), key)?;
        match kind {
            ring_memo::FIELD_BOOL => {
                if self.ring_recv_backed(&recv) {
                    return None;
                }
                let n = self.ring_cell_read(&recv, f0)?;
                Some(jit::pack_local(&Value::Int((n == 0) as i32)))
            }
            ring_memo::CANON_BOOL => {
                if self.ring_recv_backed(&recv) {
                    return None;
                }
                let r = self.ring_size_serve(&recv, f0, f1, f2)?;
                Some(jit::pack_local(&Value::Int((r == 0) as i32)))
            }
            _ => None,
        }
    }
}
