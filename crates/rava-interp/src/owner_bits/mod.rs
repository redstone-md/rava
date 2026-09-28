//! Memoised owner-name classification for the native-dispatch gauntlet
//! DEFAULT-ON since 2026-07-26 (`RAVA_NO_OWNER_BITS` restores the per-call scans).
//! Shipped on the same clean-separation A/B as env_memo (the two were measured as one
//! arm): blocks A+B min -7.4%, every ON below every OFF. The needle table is shared
//! with the flag-off fallback so the arms cannot drift.
//!
//! WHAT THIS REPLACES. Deciding how to serve a call asks a series of SUBSTRING questions about
//! the owner's class name: `stream::is_collection` runs seven (`List`, `Set`, `Queue`, `Deque`,
//! `Collection`, `Vector`, `Stack`) and the tail of `reflect::dispatch` runs six more
//! (`MXBean`, `/atomic/Atomic`, `FieldUpdater`, `Executor`, `Mailbox`, `Lock`). Each is a
//! `str::contains(&str)`, which builds a fresh searcher over the name on every call — and the
//! name is a 40-60 byte package path, so the questions cost far more than the answers.
//!
//! RIP sampling of a real boot names them: `str::contains` is ~6% of blocks A+B, and its
//! VERIFIED callers are `stream::is_collection` (55.6%) and `reflect::dispatch` (30.3%). The
//! allocator path is the worst case — `birth_native_backed_ancestor` applies the collection
//! predicate to EVERY ancestor of the class being born, so one `new` pays seven substring
//! searches per super-chain level.
//!
//! WHY A MEMO AND NOT A FASTER SCAN. The answers are a pure function of the class name and the
//! set of distinct names a boot ever asks about is a few thousand, each asked millions of
//! times. Making the scan cheaper still pays it every time; memoising pays it once per name.
//! One hash + probe replaces up to thirteen searcher constructions.
//!
// This is an operation being cheapened, not a body being specialised or trained from observations.
// It emits nothing into compiled code; the one custom stream provider is an explicit, exact class
// identity. `needle_matches` is shared by memo construction and the flag-off path so both remain
// behaviorally identical.


use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::sync::OnceLock;

/// One bit per substring question the dispatch gauntlet asks. The index into [`NEEDLES`] IS
/// the bit position, which is what keeps the memo and the fallback in step.
pub(crate) const LIST: u32 = 1 << 0;
pub(crate) const SET: u32 = 1 << 1;
pub(crate) const QUEUE: u32 = 1 << 2;
pub(crate) const DEQUE: u32 = 1 << 3;
pub(crate) const COLLECTION: u32 = 1 << 4;
pub(crate) const VECTOR: u32 = 1 << 5;
pub(crate) const STACK: u32 = 1 << 6;
pub(crate) const MXBEAN: u32 = 1 << 7;
pub(crate) const ATOMIC: u32 = 1 << 8;
pub(crate) const FIELD_UPDATER: u32 = 1 << 9;
pub(crate) const EXECUTOR: u32 = 1 << 10;
pub(crate) const MAILBOX: u32 = 1 << 11;
pub(crate) const LOCK: u32 = 1 << 12;
// --- gate-only classifications (see `may_serve`) -----------------------------
// These bits let ONE memo entry say whether the native-dispatch gauntlet has any
// arm that could match this owner. Most are cheap supersets; BUFFERED_INPUT is
// deliberately exact registry membership because suffix shape proves no contract.
pub(crate) const EXCEPTION: u32 = 1 << 13;
pub(crate) const ERROR: u32 = 1 << 14;
pub(crate) const BUFFERED_INPUT: u32 = 1 << 15;
pub(crate) const EVENT_LOOP: u32 = 1 << 16;
pub(crate) const CONSECUTIVE_EXECUTOR: u32 = 1 << 17;
pub(crate) const SCHEDULER: u32 = 1 << 18;
pub(crate) const CONDITION: u32 = 1 << 19;
pub(crate) const SEMAPHORE: u32 = 1 << 20;
pub(crate) const BYTE_BUFFER: u32 = 1 << 21;
pub(crate) const FASTUTIL: u32 = 1 << 22;

/// Bit `i` is set when the owner matches the classification for `NEEDLES[i]`.
const NEEDLES: [&str; 23] = [
    "List",
    "Set",
    "Queue",
    "Deque",
    "Collection",
    "Vector",
    "Stack",
    "MXBean",
    "/atomic/Atomic",
    "FieldUpdater",
    "Executor",
    "Mailbox",
    "Lock",
    "Exception",
    "Error",
    "BufferedInputStream",
    "EventLoop",
    "ConsecutiveExecutor",
    "Scheduler",
    "Condition",
    "Semaphore",
    "ByteBuffer",
    "fastutil",
];

/// The JDK buffered-input owner is exact; custom wrappers retain guest bytecode semantics.
#[inline]
fn needle_matches(owner: &str, index: usize, needle: &str) -> bool {
    if 1 << index == BUFFERED_INPUT {
        owner == "java/io/BufferedInputStream"
    } else {
        owner.contains(needle)
    }
}

// --- provider facts (see `facts`) ----------------------------------------------
// The FACTS word: its own 32-bit space for exact provider memberships, held in its OWN
// thread-local memo — NOT a third word of the plan/family entry, whose measured
// shape this module must not grow for a once-per-decision question. Each bit is
// EXACT membership of ONE provider table or explicit substitution registry, an immutable
// semantic fact about the class name; admission checks read the bit rather than re-derive it.
// Never a substring/prefix/package approximation: [`compute_facts`] is the only writer.
pub(crate) const FACT_ENGINE_SYNTHETIC: u32 = 1 << 0;
pub(crate) const FACT_NATIVE_REPR: u32 = 1 << 1;
pub(crate) const FACT_NATIVE_MAP: u32 = 1 << 2;
pub(crate) const FACT_ENTRY_TYPE: u32 = 1 << 3;
pub(crate) const FACT_IO_CLASS_EXACT: u32 = 1 << 4;
/// Membership of [`crate::map::GUAVA_IMMUTABLE_PROVIDERS`] — the authoritative union of the exact
/// provider tables that name guava immutable collections. `jit_native_served` — the one
/// admission check that answers the guava question — reads this bit as its guava arm:
/// exact table membership, never a package/substring shape. The classes only the historic
/// package+contains literal admitted (no table member, no earlier arm — `ImmutableTable`,
/// the `Regular*`/`Singleton*` concrete impls, Builders) compile: a full boot's served
/// census proved no native ever answered one. (`wide_tier_admits` consults neither: its
/// type-closure check refuses every member and every concrete impl by supertype, so the
/// guava question there has no name and no fact to read.)
pub(crate) const FACT_GUAVA_IMMUTABLE: u32 = 1 << 5;

/// Membership of [`crate::map::FORCE_NATIVE_MAP_CLASSES`] — the exact-identity providers
/// `force_native_collection` routes wholesale to the native map layer; the gate reads
/// this bit as its guava arm, exact table membership and never a prefix approximation.
/// `FACT_NATIVE_MAP` deliberately does NOT cover this question: it already answers
/// every `MAP_CLASSES` row — including `java/util/HashMap` — but the force-native
/// semantics are route-everything-away-from-bytecode for classes the native map layer
/// completely supplies, which a JDK map is not.
pub(crate) const FACT_FORCE_NATIVE_MAP: u32 = 1 << 6;

// --- stage bits (see `natives::dispatch_owner`) ------------------------------
// `dispatch_owner` is a linear cascade: five sub-dispatchers, each with its own
// `match owner`, then ~40 owner tests of its own. A call for `java/lang/Boolean`
// enters and declines all five before reaching its arm, and two of them
// (`stream`, `map`) CLONE EVERY ARGUMENT into `gc_protect` before they look at the
// owner at all. Which stages an owner can possibly enter is decidable from the
// owner, so it is decided once per name here and read as a bit test per call.
//
// Each predicate is a deliberate SUPERSET of its stage's own owner conditions:
// over-admitting only costs the historic scan, under-admitting would drop a
// native. RAVA_DISPATCH_GATE_VERIFY runs the skipped stages anyway and reports any
// that would have served — the drift check for all six claims.
pub(crate) const S_NIO: u32 = 1 << 23;
pub(crate) const S_NET: u32 = 1 << 24;
pub(crate) const S_STREAM: u32 = 1 << 25;
pub(crate) const S_REFLECT: u32 = 1 << 26;
pub(crate) const S_MAP: u32 = 1 << 27;
pub(crate) const S_TIME: u32 = 1 << 28;
/// Every gateable sub-dispatcher stage.
pub(crate) const STAGE_ANY: u32 = S_NIO | S_NET | S_STREAM | S_REFLECT | S_MAP | S_TIME;

/// `nio::dispatch` — every arm is a literal under one of these.
fn nio_owner(o: &str) -> bool {
    o.starts_with("java/nio/")
        || o.starts_with("java/io/")
        || o.starts_with("java/util/zip/")
        || o.starts_with("io/netty/buffer/")
        || o == "java/security/DigestOutputStream"
}

/// `net::dispatch` — the `io/netty/` prefix arm, plus literals under java/net and
/// java/nio (ByteOrder, the channel/selector family).
fn net_owner(o: &str) -> bool {
    o.starts_with("io/netty/") || o.starts_with("java/net/") || o.starts_with("java/nio/")
}

/// `stream::dispatch`'s `match owner` — literals under java/util, java/io and
/// com/google/common, plus its `is_collection(o)` substring arm. Its NAME-gated
/// Iterable defaults are owner-agnostic and are admitted by the caller, not here.
fn stream_owner(o: &str) -> bool {
    o.starts_with("java/util/")
        || o.starts_with("java/io/")
        || o.starts_with("com/google/common/")
        || NEEDLES
            .iter()
            .enumerate()
            .any(|(i, n)| COLLECTION_ANY & (1 << i) != 0 && o.contains(n))
}

/// `reflect::dispatch` — literals under java/lang, java/util, java/net,
/// java/nio/charset, javax, sun, jdk and com/google/common, plus its
/// atomic/updater/executor/lock substring arms and the `EventLoop`/
/// `ConsecutiveExecutor`/`Scheduler`/`Condition`/`Semaphore` suffixes.
/// (`sun/reflect/Reflection` and `jdk/internal/reflect/Reflection` were found
/// missing here by RAVA_DISPATCH_GATE_VERIFY on the ChunkFill gate — which is
/// exactly the drift the verify mode exists to catch.)
fn reflect_owner(o: &str) -> bool {
    o.starts_with("java/lang/")
        || o.starts_with("java/util/")
        || o.starts_with("java/net/")
        || o.starts_with("java/nio/charset/")
        || o.starts_with("javax/")
        || o.starts_with("sun/")
        || o.starts_with("jdk/")
        || o.starts_with("com/google/common/")
        || o.contains("/atomic/Atomic")
        || o.contains("FieldUpdater")
        || o.contains("Executor")
        || o.contains("Mailbox")
        || o.contains("Lock")
        || o.contains("EventLoop")
        || o.contains("Scheduler")
        || o.contains("Condition")
        || o.contains("Semaphore")
}

/// `map::dispatch` — MAP_CLASSES + the entry types (java/util, guava collect), the
/// fastutil object maps and the fastutil static view helpers.
fn map_owner(o: &str) -> bool {
    o.starts_with("java/util/")
        || o.starts_with("com/google/common/collect/")
        || o.starts_with("it/unimi/")
        || o.contains("fastutil")
}

/// `natives::time_dispatch` — java.time / java.text plus the three java.util dates.
fn time_owner(o: &str) -> bool {
    o.starts_with("java/time/")
        || o.starts_with("java/text/")
        || matches!(o, "java/util/Calendar" | "java/util/GregorianCalendar" | "java/util/Date")
}

// --- family bits (see `natives::cascade_owners::owner_families`) -------------
//
// WHAT THIS REPLACES. `owner_families` is the second half of the native cascade and it
// decided every one of its ~23 arms by comparing the owner NAME: about fifty exact
// `owner == "java/..."` compares, one `owner.contains("ByteBuffer")` (a fresh `memchr`
// searcher over a 40-60 byte package path) and two `ends_with`. A boot pays that whole
// ladder per natively-dispatched call, and pays ALL of it for the calls it declines — a PC
// profile of `bench/CtorBench.java` puts `owner_families` at 3.4% and bare `memchr` at 4.2%
// of the constructor path, and `memchr` is 3.35% of the compiled profile overall.
//
// The answers are a pure function of the owner name, so they are computed ONCE per distinct
// name into a second word of the SAME memo entry `plan`/`may_serve` already fill, and each
// arm becomes a bit test. The entry costs no extra memory: the direct-slot tuple was
// `(usize, Box<str>, u32)` = 28 bytes padded to 32, so the second `u32` lands in the padding.
//
// Index into [`FAMILIES`] IS the bit position — the `NEEDLES` discipline, for the same
// reason: `family_word` (memo) and `family_scan` (the kill-switch arm) both read this table, so
// the two arms cannot drift. Unlike the stage bits these are EXACT, not supersets: the arms
// they guard no longer re-test the owner themselves, so an over-admitting bit would run a
// family's body for a class it does not belong to.
pub(crate) const F_CODER_RESULT: u32 = 1 << 0;
pub(crate) const F_UUID: u32 = 1 << 1;
pub(crate) const F_BASE64: u32 = 1 << 2;
pub(crate) const F_TEXT: u32 = 1 << 3;
pub(crate) const F_ROUNDING_MODE: u32 = 1 << 4;
pub(crate) const F_BITSET: u32 = 1 << 5;
pub(crate) const F_LAZY_NUMBER: u32 = 1 << 6;
pub(crate) const F_BIGNUM: u32 = 1 << 7;
pub(crate) const F_MESSAGE_DIGEST: u32 = 1 << 8;
pub(crate) const F_KEY: u32 = 1 << 9;
pub(crate) const F_BYTE_BUFFER: u32 = 1 << 10;
pub(crate) const F_CHAR_BUFFER: u32 = 1 << 11;
pub(crate) const F_TYPED_BUFFER: u32 = 1 << 12;
pub(crate) const F_REGEX: u32 = 1 << 13;
pub(crate) const F_MATH: u32 = 1 << 14;
pub(crate) const F_CHARACTER: u32 = 1 << 15;
pub(crate) const F_INT_LONG: u32 = 1 << 16;
pub(crate) const F_STRING: u32 = 1 << 17;
pub(crate) const F_WRITER: u32 = 1 << 18;
pub(crate) const F_STRING_BUILDER: u32 = 1 << 19;
pub(crate) const F_STACK_TRACE_ELEMENT: u32 = 1 << 20;
pub(crate) const F_THROWABLE: u32 = 1 << 21;
pub(crate) const F_ZIP_INPUT: u32 = 1 << 22;
/// Not a gate but a VALUE: the `Integer`/`Long` arm passes "is this the Long half?" into
/// `int_static`, and that was a string compare per boxing static. Same table, same word,
/// no extra probe.
pub(crate) const F_LONG: u32 = 1 << 23;

/// How a family recognises an owner — the entire vocabulary `owner_families` used.
#[derive(Clone, Copy)]
pub(crate) enum Pat {
    /// `owner == s`
    Eq(&'static str),
    /// `owner.contains(s)` — the `memchr` this table exists to stop paying per call.
    Has(&'static str),
    /// `owner.ends_with(s)`
    Ends(&'static str),
}

/// Bit `i` of the FAMILY word is set when the owner matches any pattern in `FAMILIES[i]`.
/// Transcribed one-for-one from `owner_families`; this is now the only place the literals
/// live, so an arm that grows a new owner grows it here.
const FAMILIES: [&[Pat]; 24] = [
    &[Pat::Eq("java/nio/charset/CoderResult")],
    &[Pat::Eq("java/util/UUID")],
    &[
        Pat::Eq("java/util/Base64"),
        Pat::Eq("java/util/Base64$Encoder"),
        Pat::Eq("java/util/Base64$Decoder"),
    ],
    &[
        Pat::Eq("java/text/DecimalFormatSymbols"),
        Pat::Eq("java/text/DecimalFormat"),
        Pat::Eq("java/text/NumberFormat"),
        Pat::Eq("java/text/Format"),
    ],
    &[Pat::Eq("java/math/RoundingMode")],
    &[Pat::Eq("java/util/BitSet")],
    &[Pat::Eq("com/google/gson/internal/LazilyParsedNumber")],
    &[Pat::Eq("java/math/BigDecimal"), Pat::Eq("java/math/BigInteger")],
    &[Pat::Eq("java/security/MessageDigest")],
    &[
        Pat::Eq("java/security/KeyPairGenerator"),
        Pat::Eq("java/security/KeyPair"),
        Pat::Eq("java/security/PublicKey"),
        Pat::Eq("java/security/PrivateKey"),
        Pat::Eq("java/security/Key"),
        Pat::Eq("java/security/interfaces/RSAPublicKey"),
        Pat::Eq("java/security/interfaces/RSAPrivateKey"),
        Pat::Eq("java/security/KeyFactory"),
        Pat::Eq("javax/crypto/KeyGenerator"),
        Pat::Eq("javax/crypto/SecretKey"),
    ],
    &[Pat::Has("ByteBuffer")],
    &[Pat::Eq("java/nio/CharBuffer")],
    &[
        Pat::Eq("java/nio/IntBuffer"),
        Pat::Eq("java/nio/LongBuffer"),
        Pat::Eq("java/nio/ShortBuffer"),
        Pat::Eq("java/nio/FloatBuffer"),
        Pat::Eq("java/nio/DoubleBuffer"),
        Pat::Eq("java/nio/CharBuffer"),
    ],
    &[Pat::Eq("java/util/regex/Pattern"), Pat::Eq("java/util/regex/Matcher")],
    &[Pat::Eq("java/lang/Math"), Pat::Eq("java/lang/StrictMath")],
    &[Pat::Eq("java/lang/Character")],
    &[Pat::Eq("java/lang/Integer"), Pat::Eq("java/lang/Long")],
    &[Pat::Eq("java/lang/String")],
    &[
        Pat::Eq("java/io/StringWriter"),
        Pat::Eq("java/io/PrintWriter"),
        Pat::Eq("java/io/CharArrayWriter"),
        Pat::Eq("java/io/BufferedWriter"),
        Pat::Eq("java/io/Writer"),
        Pat::Eq("java/io/PrintStream"),
        Pat::Eq("java/io/FileWriter"),
        Pat::Eq("java/io/OutputStreamWriter"),
    ],
    &[Pat::Eq("java/lang/StringBuilder"), Pat::Eq("java/lang/StringBuffer")],
    &[Pat::Eq("java/lang/StackTraceElement")],
    &[Pat::Eq("java/lang/Throwable"), Pat::Ends("Exception"), Pat::Ends("Error")],
    &[
        Pat::Eq("java/util/zip/GZIPInputStream"),
        Pat::Eq("java/util/zip/InflaterInputStream"),
    ],
    &[Pat::Eq("java/lang/Long")],
];

/// Family membership resolved once from an immutable symbolic owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeFamilyFact {
    Unknown,
    Known(u32),
}

impl NativeFamilyFact {
    /// Resolve the authoritative family table once; callers without a fixed site remain Unknown.
    pub(crate) fn for_owner_name(owner: &str) -> Self {
        Self::Known(compute_families(owner))
    }
}

/// Set when the owner lives in a namespace that some dispatcher names by literal.
/// Not a needle (it is a prefix disjunction, not a substring), so it is computed
/// separately and lives above the needle bits.
const NAMESPACE: u32 = 1 << 31;

/// Namespaces in which EVERY class-name literal of the native-dispatch gauntlet
/// lives (`natives::dispatch_owner` and the `nio`/`net`/`stream`/`reflect`/`map`
/// dispatchers it calls). Derived by extracting every class-shaped string literal
/// from those six modules; `namespace_literals_are_covered` re-checks the claim.
const NAMESPACES: [&str; 7] =
    ["java/", "javax/", "jdk/", "sun/", "com/google/", "io/netty/", "it/unimi/"];

/// Every collection-shaped name `stream::is_collection` accepts.
pub(crate) const COLLECTION_ANY: u32 = LIST | SET | QUEUE | DEQUE | COLLECTION | VECTOR | STACK;

/// Everything the native-dispatch gauntlet can key an arm on through a namespace, substring,
/// or suffix. The buffered-input bit is exact registry membership despite sharing the word.
/// An owner with NONE of these bits cannot match ANY native arm.
pub(crate) const DISPATCH_ANY: u32 = NAMESPACE
    | COLLECTION_ANY
    | MXBEAN
    | ATOMIC
    | FIELD_UPDATER
    | EXECUTOR
    | MAILBOX
    | LOCK
    | EXCEPTION
    | ERROR
    | BUFFERED_INPUT
    | EVENT_LOOP
    | CONSECUTIVE_EXECUTOR
    | SCHEDULER
    | CONDITION
    | SEMAPHORE
    | BYTE_BUFFER
    | FASTUTIL;
pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_OWNER_BITS").is_none())
}

/// DEFAULT-ON. `RAVA_NO_ID_RESOLVE=1` restores every by-string lookup this family
/// replaced, in one switch, so a broken boot bisects in one run: the `owner_families`
/// literal ladder (here, via [`family_scan`]) and the pointer-indexed L1s in front of
/// `tlcache`'s `resolve` and `field_key` memos. Both arms read the same tables, so the
/// switch is a performance switch and never a semantic one.
pub(crate) fn id_resolve() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_ID_RESOLVE").is_none())
}

/// DEFAULT-ON. `RAVA_NO_CLASS_FACTS=1` restores the per-ask table scans the facts
/// word memoised: the admission checks re-derive each bit from the same provider
/// tables by name, exactly as they did before the word existed. The memo arm and
pub(crate) fn class_facts() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        !crate::flags::emit_more_disabled() && std::env::var_os("RAVA_NO_CLASS_FACTS").is_none()
    });
    *ON
}

/// Does `owner` match any of `pats`? The ONLY definition of a family's membership —
/// [`compute_families`] and [`family_scan`] both come through here.
#[inline]
fn match_any(owner: &str, pats: &[Pat]) -> bool {
    pats.iter().any(|p| match *p {
        Pat::Eq(s) => owner == s,
        Pat::Has(s) => owner.contains(s),
        Pat::Ends(s) => owner.ends_with(s),
    })
}

/// The provider-facts word for `owner` — one memo probe answers every exact-membership
/// question the admission checks ask (JIT/AOT compile refusal, the direct-call
/// resolver's invokespecial arm). Low-frequency by construction: each consumer runs
/// once per decision, never per call, so the memo is a SEPARATE thread-local map and
/// the plan/family entry below keeps its committed two-word shape untouched.
/// Falls back to the tables when any switch is off.
pub(crate) fn facts(owner: &str) -> u32 {
    if !enabled() || !class_facts() {
        return compute_facts(owner);
    }
    FACTS.with(|m| {
        // The lookup is its own STATEMENT, for the same reason as `entry`'s: a
        // `match m.borrow().get(..)` would hold the shared borrow across the miss
        // arm's `borrow_mut` and panic.
        let hit = m.borrow().get(owner).copied();
        match hit {
            Some(f) => f,
            None => {
                let f = compute_facts(owner);
                m.borrow_mut().insert(owner.into(), f);
                f
            }
        }
    })
}

/// One family's membership computed the long way — the kill-switch arm. Scans EXACTLY the
/// masked family's patterns, i.e. the compares that stood at that arm before, so an A/B
/// measures the memo rather than a widened question (the argument [`contains_any`] records).
pub(crate) fn family_scan(owner: &str, mask: u32) -> bool {
    FAMILIES
        .iter()
        .enumerate()
        .any(|(i, pats)| mask & (1 << i) != 0 && match_any(owner, pats))
}

/// Does `owner` match ANY of the classifications in `mask`?
///
/// With the flag OFF this scans EXACTLY the masked classifications; BUFFERED_INPUT is the one
/// exception to the substring patterns and reads the same exact provider registry as the memo.
/// With it on, one memo entry answers every mask.
pub(crate) fn contains_any(owner: &str, mask: u32) -> bool {
    if !enabled() {
        return NEEDLES
            .iter()
            .enumerate()
            .any(|(i, n)| mask & (1 << i) != 0 && needle_matches(owner, i, n));
    }
    bits(owner) & mask != 0
}

/// The whole per-owner dispatch plan in one word: the stage bits above plus the
/// `DISPATCH_ANY` bits that say whether any arm keys on this owner at all. ONE
/// memo lookup answers the entire cascade's owner questions.
pub(crate) fn plan(owner: &str) -> u32 {
    bits(owner)
}

/// The FAMILY word of the SAME memo entry — the `plan()`/`bits()` accessor's sibling
/// for the second word, gate-free: every caller reaches it only where
/// `enabled() && id_resolve()` is already established (`owner_families` proves it in
/// its `by_id` arm). A caller that has not proved that belongs on [`family_scan`],
/// the kill-switch arm.
pub(crate) fn family_word(owner: &str) -> u32 {
    entry(owner).1
}

/// Could ANY arm of the native-dispatch gauntlet be keyed on this owner?
///
/// `false` is a proof of absence: no dispatcher names the owner by literal or namespace, and no
/// active substring/suffix arm can match it. The buffered-input bit is exact provider membership,
/// so a foreign suffix alone cannot admit the cascade. `true` says nothing — the cascade runs
/// exactly as before.
///
/// Answered from the SAME memo entry as `contains_any`, so an owner that is
/// already classified costs no additional lookup.
pub(crate) fn may_serve(owner: &str) -> bool {
    if !enabled() {
        return may_serve_scan(owner);
    }
    bits(owner) & DISPATCH_ANY != 0
}
/// `may_serve` computed the long way — the disjunction the memo stands in for.
/// The gate's off-arm and the memo's on-arm both come from here, so they cannot
/// drift (the same guarantee `contains_any` gets from `NEEDLES`).
pub(crate) fn may_serve_scan(owner: &str) -> bool {
    NAMESPACES.iter().any(|p| owner.starts_with(p))
        || NEEDLES
            .iter()
            .enumerate()
            .any(|(i, n)| DISPATCH_ANY & (1 << i) != 0 && needle_matches(owner, i, n))
}

/// Per-thread memo table. The only writer is the thread asking, so this can never contend
/// with execution, and a worker gets its own (identical) table rather than a lock on a
/// shared one.
///
/// Reached through a cluster cursor because the memo REPLACED substring scans with a
/// thread-local access, and the access then showed up in its place: RIP sampling puts
/// `owner_bits::contains_any` at 20.0% of blocks A+B's `LocalKey::with` cluster and 24.8%
/// of block C's — the largest consumer in block C, where `LocalKey::with` is the single
/// biggest leaf in the window. The memo is right; what was left to pay for it was the way
/// it was reached.
/// Number of direct-mapped slots in front of the map. A boot classifies a few thousand distinct
/// owners, but any single call site sees one — the window that matters is the working set of the
/// loop being interpreted, and 256 covers it with a 2 KB table.
const DIRECT: usize = 256;

/// One memo entry: the needle/namespace/stage word, then the FAMILY word. Two words
/// because they are read by different consumers (`plan`/`contains_any`/`may_serve` vs
/// `owner_families`) and neither has a spare bit; ONE entry because they are the same
/// pure function of the same name and a second table would double the probes.
type Entry = (u32, u32);

#[derive(Default)]
pub(crate) struct Memo {
    table: RefCell<HashMap<Box<str>, Entry, BuildHasherDefault<crate::FxHasher>>>,
    /// ADDRESS-KEYED front cache, VERIFIED BY CONTENT — the shape `argcall::arg_layout` already
    /// uses for descriptors, and for the same reason it gives there.
    ///
    /// The map behind it is keyed by the owner's CONTENT, so every hit hashed a 44-character
    /// class name and then compared it. A PC profile of the natively-served call path puts
    /// `owner_bits::bits` at 10.0% — the largest row in that window that is not dispatch. Owner
    /// names come from the constant pool and the class-name interner, so the same call site
    /// passes the same ADDRESS every time; an index and a pointer compare replace the hash.
    ///
    /// The content check is not optional: not every caller passes an interned name (some build a
    /// `String` from an arena class id), and a freed temporary's address can be reused. A wrong
    /// hit here would classify a class as native-served that is not, which is a silent
    /// wrong-execution bug rather than a crash.
    ///
    /// SLOT SIZE IS UNCHANGED by the family word: `(usize, Box<str>, u32)` is 28 bytes
    /// padded to 32, so the second `u32` lands in the padding and the table is still
    /// 256 x 32 = 8 KB. That matters — this tree has measured a +6.8% REGRESSION from a
    /// front cache that grew until it stopped fitting in L1.
    direct: RefCell<Vec<Option<(usize, Box<str>, u32, u32)>>>,
}

crate::tls_cluster::thread_cluster! {
    /// This thread's owner-classification memo, through one inlined TLS read.
    fn tl() -> &'static Memo;
    slot MEMO = Memo::default();
    cursor MEMO_PTR;
    census crate::tls_cluster::stat::OWNER_BITS;
}

/// All classification bits for `owner`.
fn bits(owner: &str) -> u32 {
    entry(owner).0
}

/// The whole memo entry for `owner` — the classification word and the family word.
fn entry(owner: &str) -> Entry {
    let m = tl();
    let ptr = owner.as_ptr() as usize;
    let slot = (ptr >> 4) & (DIRECT - 1);
    {
        let d = m.direct.borrow();
        if let Some(Some((p, name, v, f))) = d.get(slot) {
            if *p == ptr && &**name == owner {
                return (*v, *f);
            }
        }
    }
    // The lookup is its own STATEMENT: a `match m.table.borrow().get(..)` would hold the shared
    // borrow for the whole match expression, and the miss arm's `borrow_mut` then panics. That is
    // what the first version of this did, and the benchmark's "no samples" was the panic.
    let hit = m.table.borrow().get(owner).copied();
    let e = match hit {
        Some(e) => e,
        None => {
            let e = (compute(owner), compute_families(owner));
            m.table.borrow_mut().insert(owner.into(), e);
            e
        }
    };
    {
        let mut d = m.direct.borrow_mut();
        if d.is_empty() {
            d.resize(DIRECT, None);
        }
        d[slot] = Some((ptr, owner.into(), e.0, e.1));
    }
    e
}

/// The classification itself — the substring scans, run once per distinct owner name.
fn compute(owner: &str) -> u32 {
    let mut b = 0u32;
    for (i, n) in NEEDLES.iter().enumerate() {
        if needle_matches(owner, i, n) {
            b |= 1 << i;
        }
    }
    if NAMESPACES.iter().any(|p| owner.starts_with(p)) {
        b |= NAMESPACE;
    }
    for (bit, p) in [
        (S_NIO, nio_owner as fn(&str) -> bool),
        (S_NET, net_owner),
        (S_STREAM, stream_owner),
        (S_REFLECT, reflect_owner),
        (S_MAP, map_owner),
        (S_TIME, time_owner),
    ] {
        if p(owner) {
            b |= bit;
        }
    }
    b
}

/// The family classification — the ~50 exact compares, one `contains` and two `ends_with`
/// that `owner_families` used to run PER CALL, run once per distinct owner name.
fn compute_families(owner: &str) -> u32 {
    let mut f = 0u32;
    for (i, pats) in FAMILIES.iter().enumerate() {
        if match_any(owner, pats) {
            f |= 1 << i;
        }
    }
    f
}

/// The per-thread facts memo — a SEPARATE map, deliberately outside the plan/family
/// memo: that entry's two-word shape and address-keyed direct slot are the measured
/// hot path this module exists to keep, and a once-per-decision question does not
/// grow them. Keyed by CONTENT (these callers pass one-off built names, so an
/// address-keyed slot would be wrong), and reached by a plain `thread_local!`
/// rather than a cluster cursor because the census crossings the cluster saves are
/// a per-call cost and this is not one. The only writer is the thread asking, so
/// it never contends with execution.
thread_local! {
    static FACTS: RefCell<HashMap<Box<str>, u32>> = RefCell::new(HashMap::new());
}

/// The provider-facts classification — exact membership of provider tables and explicit
/// substitution registries, run once per distinct owner name. Each bit reads the ONE authoritative
/// table for its question (never a wrapper predicate that might embed a performance filter, never
/// a substring/prefix/package approximation).
fn compute_facts(owner: &str) -> u32 {
    let mut f = 0u32;
    if owner == crate::cache::LOADING_CACHE_CLASS || owner == crate::cache::CACHE_CLASS {
        f |= FACT_ENGINE_SYNTHETIC;
    }
    if crate::fastutil::INT_SETS.contains(&owner) {
        f |= FACT_NATIVE_REPR;
    }
    if crate::map::MAP_CLASSES.contains(&owner)
        || crate::map::FASTUTIL_OBJMAPS.contains(&owner)
        || (cfg!(feature = "next-gen-gc") && crate::map::FASTUTIL_OBJ2OBJ.contains(&owner))
    {
        f |= FACT_NATIVE_MAP;
    }
    if crate::map::ENTRY_CLASSES.contains(&owner) {
        f |= FACT_ENTRY_TYPE;
    }
    // The force-native map providers — exact table membership. The force-native gate reads
    // this bit as its exact predicate; see FACT_FORCE_NATIVE_MAP.
    if crate::map::FORCE_NATIVE_MAP_CLASSES.contains(&owner) {
        f |= FACT_FORCE_NATIVE_MAP;
    }
    if crate::opcodes::IO_CLASSES_EXACT.contains(&owner) {
        f |= FACT_IO_CLASS_EXACT;
    }
    // The guava immutable union — exact table membership. `jit_native_served` reads this
    // bit as its guava arm; see FACT_GUAVA_IMMUTABLE.
    if crate::map::GUAVA_IMMUTABLE_PROVIDERS.contains(&owner) {
        f |= FACT_GUAVA_IMMUTABLE;
    }
    f
}
/// The family word is the one classification the cascade acts on WITHOUT re-testing the
/// owner, so its bits must be exact, not the superset the stage bits are. These are the
/// assertions that settle that without a boot.
#[cfg(test)]
mod family_tests {
    use super::*;

    /// Every owner the transcribed arms name must land in its family, and only in it.
    #[test]
    fn each_family_admits_its_own_literals_and_nobody_elses() {
        for (owner, want) in [
            ("java/nio/charset/CoderResult", F_CODER_RESULT),
            ("java/util/UUID", F_UUID),
            ("java/util/Base64$Encoder", F_BASE64),
            ("java/text/DecimalFormat", F_TEXT),
            ("java/math/RoundingMode", F_ROUNDING_MODE),
            ("java/util/BitSet", F_BITSET),
            ("com/google/gson/internal/LazilyParsedNumber", F_LAZY_NUMBER),
            ("java/math/BigInteger", F_BIGNUM),
            ("java/security/MessageDigest", F_MESSAGE_DIGEST),
            ("javax/crypto/SecretKey", F_KEY),
            ("java/nio/IntBuffer", F_TYPED_BUFFER),
            ("java/util/regex/Matcher", F_REGEX),
            ("java/lang/StrictMath", F_MATH),
            ("java/lang/Character", F_CHARACTER),
            ("java/lang/Long", F_INT_LONG | F_LONG),
            ("java/lang/String", F_STRING),
            ("java/io/PrintStream", F_WRITER),
            ("java/lang/StringBuffer", F_STRING_BUILDER),
            ("java/lang/StackTraceElement", F_STACK_TRACE_ELEMENT),
            ("java/util/zip/GZIPInputStream", F_ZIP_INPUT),
        ] {
            assert_eq!(compute_families(owner), want, "{owner}");
        }
        // The three overlaps that are real and intended: CharBuffer is in both buffer
        // families (it was in both `if`s), a *ByteBuffer is matched by substring, and a
        // Throwable name by suffix.
        assert_eq!(compute_families("java/nio/CharBuffer"), F_CHAR_BUFFER | F_TYPED_BUFFER);
        assert_eq!(compute_families("some/pkg/WrappedByteBuffer"), F_BYTE_BUFFER);
        assert_eq!(compute_families("net/minecraft/ReportedException"), F_THROWABLE);
        assert_eq!(compute_families("java/lang/Throwable"), F_THROWABLE);
    }

    /// A plain jar class must reach NO family — an over-admitting bit would run a
    /// family's body for a class that is not in it, which is silent wrong execution
    /// rather than a lost native.
    #[test]
    fn a_plain_jar_class_is_in_no_family() {
        for owner in [
            "net/minecraft/world/level/levelgen/NoiseChunk",
            "net/minecraft/core/Holder$Reference",
            "java/util/ArrayList",
            "java/lang/Object",
            "",
        ] {
            assert_eq!(compute_families(owner), 0, "{owner}");
        }
    }

    /// Memo arm and kill-switch arm must agree for every family — the whole safety
    /// argument for `RAVA_NO_ID_RESOLVE` being a performance switch.
    #[test]
    fn the_memo_and_the_ladder_agree_family_by_family() {
        for owner in [
            "java/lang/String",
            "java/lang/StringBuilder",
            "java/nio/CharBuffer",
            "some/pkg/WrappedByteBuffer",
            "net/minecraft/ReportedException",
            "net/minecraft/world/item/ItemStack",
            "java/security/KeyFactory",
            "",
        ] {
            let word = compute_families(owner);
            for i in 0..FAMILIES.len() {
                assert_eq!(
                    word & (1 << i) != 0,
                    family_scan(owner, 1 << i),
                    "family {i} disagrees for {owner}"
                );
            }
            if enabled() && id_resolve() {
                assert_eq!(family_word(owner), word, "memo disagrees for {owner}");
            }
        }
    }

    /// The family word is its own 32-bit space; nothing here may reach past it.
    #[test]
    fn every_family_fits_the_word() {
        assert!(FAMILIES.len() <= 32);
    }

    /// The claim the module comment makes about memory, checked rather than asserted in
    /// prose: the family word rides in the direct slot's existing padding, so the table
    /// is still 256 x 32 = 8 KB. A slot that grows is the +6.8% L1 regression this tree
    /// has already measured once.
    #[test]
    fn the_family_word_costs_no_slot_bytes() {
        assert_eq!(std::mem::size_of::<Option<(usize, Box<str>, u32)>>(), 32);
        assert_eq!(std::mem::size_of::<Option<(usize, Box<str>, u32, u32)>>(), 32);
        assert_eq!(DIRECT * 32, 8192);
    }
    #[test]
    fn family_facts_match_the_authoritative_table_for_exact_and_shape_names() {
        for family in FAMILIES {
            for pattern in family {
                let Pat::Eq(owner) = *pattern else { continue };
                assert_eq!(
                    NativeFamilyFact::for_owner_name(owner),
                    NativeFamilyFact::Known(compute_families(owner)),
                    "{owner}",
                );
            }
        }
        for owner in [
            "java/util/ArrayList",
            "pkg/WrappedByteBuffer",
            "pkg/ReportedException",
        ] {
            assert_eq!(
                NativeFamilyFact::for_owner_name(owner),
                NativeFamilyFact::Known(compute_families(owner)),
                "{owner}",
            );
        }
    }
}

/// The facts word is the exact-membership contract the admission checks read, and
/// its memo is a SEPARATE thread-local map; these are the assertions that settle
/// both without a boot, in the family_tests pattern.
#[cfg(test)]
mod fact_tests {
    use super::*;

    /// Every member of every provider table must land its bit — and ONLY its bit,
    /// because an admission check reads the word as the exact name predicate it
    /// stood for, and a stray bit would change a different check's answer. The
    /// loops read the tables themselves, so a member is covered the day it lands.
    #[test]
    fn every_table_member_sets_exactly_its_own_bit() {
        assert_eq!(compute_facts(crate::cache::LOADING_CACHE_CLASS), FACT_ENGINE_SYNTHETIC);
        assert_eq!(compute_facts(crate::cache::CACHE_CLASS), FACT_ENGINE_SYNTHETIC);
        for owner in crate::fastutil::INT_SETS {
            assert_eq!(compute_facts(owner), FACT_NATIVE_REPR, "{owner}");
        }
        // ImmutableBiMap is also a GUAVA_IMMUTABLE_PROVIDERS member, and HashBiMap is also
        // a FORCE_NATIVE_MAP_CLASSES member, so a member's word carries those bits too —
        // see the union and force-native loops below.
        for owner in crate::map::MAP_CLASSES {
            let want = FACT_NATIVE_MAP
                | if crate::map::GUAVA_IMMUTABLE_PROVIDERS.contains(owner) {
                    FACT_GUAVA_IMMUTABLE
                } else {
                    0
                }
                | if crate::map::FORCE_NATIVE_MAP_CLASSES.contains(owner) {
                    FACT_FORCE_NATIVE_MAP
                } else {
                    0
                };
            assert_eq!(compute_facts(owner), want, "{owner}");
        }
        for owner in crate::map::FASTUTIL_OBJMAPS {
            assert_eq!(compute_facts(owner), FACT_NATIVE_MAP, "{owner}");
        }
        for owner in crate::map::ENTRY_CLASSES {
            assert_eq!(compute_facts(owner), FACT_ENTRY_TYPE, "{owner}");
        }
        for owner in crate::opcodes::IO_CLASSES_EXACT {
            assert_eq!(compute_facts(owner), FACT_IO_CLASS_EXACT, "{owner}");
        }
        // The cfg-gated fastutil object maps land their bit exactly when the wrapper
        // predicate would — the fact must mirror `is_fastutil_objmap`'s cfg arm.
        for owner in crate::map::FASTUTIL_OBJ2OBJ {
            let want = if cfg!(feature = "next-gen-gc") { FACT_NATIVE_MAP } else { 0 };
            assert_eq!(compute_facts(owner), want, "{owner}");
        }
    }

    /// The guava union: every member lands the FACT_GUAVA_IMMUTABLE bit, and a member that
    /// is ALSO a native-map provider (ImmutableBiMap) carries both bits — the overlap is the
    /// deduplication the union's own drift test pins.
    #[test]
    fn every_guava_union_member_lands_the_bit() {
        for owner in crate::map::GUAVA_IMMUTABLE_PROVIDERS {
            let want = FACT_GUAVA_IMMUTABLE
                | if crate::map::MAP_CLASSES.contains(owner) {
                    FACT_NATIVE_MAP
                } else {
                    0
                };
            assert_eq!(compute_facts(owner), want, "{owner}");
        }
    }

    /// The force-native providers: every member lands the FACT_FORCE_NATIVE_MAP bit, and a
    /// member that is ALSO a native-map provider (HashBiMap) carries both bits — the
    /// overlap is what `force_native_map_tests`' membership drift test pins.
    #[test]
    fn every_force_native_map_member_lands_the_bit() {
        for owner in crate::map::FORCE_NATIVE_MAP_CLASSES {
            let want = FACT_FORCE_NATIVE_MAP
                | if crate::map::MAP_CLASSES.contains(owner) {
                    FACT_NATIVE_MAP
                } else {
                    0
                };
            assert_eq!(compute_facts(owner), want, "{owner}");
        }
    }

    /// The nested HashBiMap classes are in NO provider table, so they read zero — the
    /// bit is exact table membership and nothing else. They are born only inside
    /// HashBiMap bytecode that cannot execute while the exact class force-routes,
    /// so no nested name can reach the gate as owner, actual or lambda owner.
    #[test]
    fn the_nested_hashbimap_classes_are_not_providers() {
        for owner in [
            "com/google/common/collect/HashBiMap$BiEntry",
            "com/google/common/collect/HashBiMap$Itr",
            "com/google/common/collect/HashBiMap$Inverse",
            "com/google/common/collect/HashBiMap$InverseKeySet",
        ] {
            assert_eq!(compute_facts(owner), 0, "{owner}");
        }
    }

    /// The force-native bit must NOT fire for a JDK map: `FACT_NATIVE_MAP` answers
    /// serve-capability for every MAP_CLASSES row, but route-everything-away-from-bytecode
    /// is a stronger question only the exact providers license. Conflating the two would
    /// force-route `java/util/HashMap` and change the default truth table.
    #[test]
    fn a_jdk_map_is_not_force_native() {
        for owner in ["java/util/HashMap", "java/util/TreeMap", "java/util/Map"] {
            assert!(crate::map::MAP_CLASSES.contains(&owner));
            assert_eq!(compute_facts(owner), FACT_NATIVE_MAP, "{owner}");
        }
    }

    /// The historic admission SHAPE is not the fact: ImmutableList/ImmutableSet/ImmutableMultiset
    /// and the concrete impls (RegularImmutableMap, SingletonImmutableList) match
    /// `com/google/common/collect/*` + contains("Immutable") but are in NO provider table, so
    /// they read zero — the bit is exact table membership and nothing else, which is what
    /// lets `jit_native_served` admit guava by the bit alone: those shape-only names compile,
    /// and a full boot's served census proved no native ever answered one. (`wide_tier_admits`
    /// needs neither: its type-closure check refuses every one of these names by
    /// supertype, so the guava question there is settled without a name or a fact.)
    #[test]
    fn the_guava_shape_is_not_a_fact() {
        for owner in [
            "com/google/common/collect/ImmutableList",
            "com/google/common/collect/ImmutableSet",
            "com/google/common/collect/ImmutableMultiset",
            "com/google/common/collect/RegularImmutableMap",
            "com/google/common/collect/SingletonImmutableList",
            "com/google/common/collect/RegularImmutableSortedMap",
        ] {
            assert!(owner.starts_with("com/google/common/collect/") && owner.contains("Immutable"));
            assert_eq!(compute_facts(owner), 0, "{owner}");
        }
    }

    /// A plain jar class must read ZERO — an over-admitting bit would let an
    /// admission check act on a class no provider table names, which is silent
    /// wrong execution rather than a lost optimization.
    #[test]
    fn a_plain_jar_class_has_no_facts() {
        for owner in [
            "net/minecraft/world/level/levelgen/NoiseChunk",
            "net/minecraft/core/Holder$Reference",
            "java/util/ArrayList",
            "java/lang/Object",
            "",
        ] {
            assert_eq!(compute_facts(owner), 0, "{owner}");
        }
    }

    /// The MC wrapper has concrete bytecode state not modeled by the synthetic stream layer.
    #[test]
    fn custom_buffered_stream_keeps_guest_class_semantics() {
        let class = "net/minecraft/util/FastBufferedInputStream";
        assert_eq!(compute_facts(class), 0);
        assert!(!crate::opcodes::is_native_io_class(class));
        assert!(!contains_any(class, BUFFERED_INPUT));
    }

    /// Memo arm and kill-switch arm must agree — the whole safety argument for
    /// `RAVA_NO_CLASS_FACTS` being a performance switch. `facts` and `compute_facts`
    /// are one function, so this can only fail if the two arms are ever split.
    #[test]
    fn the_memo_and_the_tables_agree_fact_by_fact() {
        for owner in [
            "rava/GuavaLoadingCache",
            "it/unimi/dsi/fastutil/ints/IntOpenHashSet",
            "java/util/HashMap",
            "java/util/Map$Entry",
            "java/io/BufferedInputStream",
            "net/minecraft/util/FastBufferedInputStream",
            "net/minecraft/world/level/levelgen/NoiseChunk",
            // The force-native bit must ride the same memo the other facts do, and a
            // non-provider nested name must agree at zero in both arms.
            "com/google/common/collect/HashBiMap",
            "com/google/common/collect/HashBiMap$BiEntry",
            "com/google/common/collect/RegularImmutableMap",
            "",
        ] {
            let word = compute_facts(owner);
            if enabled() && class_facts() {
                assert_eq!(facts(owner), word, "memo disagrees for {owner}");
            }
        }
    }

    /// The facts word lives in its OWN memo, so the hot entry keeps its committed
    /// shape: two words, the classification word and the family word. (The direct
    /// slot's 8 KB is family_tests' claim, unchanged.)
    #[test]
    fn the_facts_word_costs_no_entry_bytes() {
        assert_eq!(std::mem::size_of::<Entry>(), 8);
    }
}

mod tests;
pub(crate) use tests::*;
