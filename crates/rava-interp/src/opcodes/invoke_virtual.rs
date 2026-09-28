//! invokevirtual: resolve the receiver's method and enter it, with every fast path in front.
//!
//! Re-exported from the parent module, so no call site outside it changed.
//!
//! LEFT WHOLE ON PURPOSE. This file is 781 lines against a 500-line project rule, and it
//! stays that way because splitting it was measured and it costs. Cutting the resolved-
//! dispatch tail into its own function: 329.3 -> 343.0 ns per interpreted virtual call
//! (n=9, no overlap, +4.2%). Cutting the guest fast paths as well: +12.7%. Forcing
//! #[inline(always)] on both made it worse still (353.5). Reverting restored parity
//! (328.0 vs 326.9, overlapping).
//!
//! This is the hottest path in an interpreted boot; a 4% tax on it to satisfy a file-length
//! rule is the wrong trade. The same measurement is why the interpreter loop kept its cheap
//! opcodes inline (ledger row 204).

use crate::opcodes::*;

/// ⭐**FIVE PROCESS CONSTANTS, ONE ACQUIRE LOAD.**
///
/// Every gate this opcode consults is fixed for the life of the process, and each carried its
/// own `OnceLock<bool>`: the round-7 memo switch (twice), `devirt::enabled` (unconditionally,
/// before anything else happens), `receiver_guard_enabled` (up to three times on one call), and
/// `vecgrow::enabled` inside every `vecgrow::note`. That is three to five ACQUIRE loads on the
/// engine's hottest opcode, and the cost is not the `mov` — an acquire is a reordering barrier
/// LLVM may neither hoist out of the dispatch nor fold with its own twin. It is the same shape
/// as the relaxed atomic this tree pulled off the interpreter's push path for a measured −15%,
/// and as `tlcache::cflags`, which did exactly this for the inline caches.
///
/// COMPLEXITY: WAS one acquire load per gate per CALL. IS one acquire load per call, read into
/// a register at the top of the opcode and consumed as bit tests. What grows: nothing. This is a
/// `u8`, not a table.
#[allow(dead_code)]
mod g {
    /// The `RAVA_ROUND7` block: the site-gauntlet memo table and `ADD_PLAN`. OPT-IN — round 7
    /// measured slower as a block and stayed behind its own env, so everything keyed on this
    /// bit is OFF in a product boot. That is exactly why `gauntlet_mask` still shows up in the
    /// profile, and why the classification below had to get cheaper rather than memoised.
    pub(super) const ROUND7: u8 = 1 << 0;
    /// This round, DEFAULT ON — `RAVA_NO_FAST_IVIRT3=1` (or the global `RAVA_NO_EMIT_MORE`)
    /// restores the previous per-call code byte for byte.
    pub(super) const IVIRT3: u8 = 1 << 1;
    /// `RAVA_DEVIRT_PROBE` — the monomorphism census.
    pub(super) const DEVIRT: u8 = 1 << 2;
    /// `RAVA_RECEIVER_GUARD` — the stale-arena-receiver forensic trap.
    pub(super) const RECV_GUARD: u8 = 1 << 3;
    /// `RAVA_VECGROW_STAT` — the Vec-growth census.
    pub(super) const VECGROW: u8 = 1 << 4;
    /// This round, DEFAULT ON — `RAVA_NO_FAST_IVIRT4=1` (or the global `RAVA_NO_EMIT_MORE`)
    /// puts the receiver-class derivation back to one probe per QUESTION asked, byte for byte.
    pub(super) const IVIRT4: u8 = 1 << 5;
    /// `RAVA_TRACE_CORRUPT` — the garbage-class-`Arc` forensic in the `actual` walk. Read as a
    /// bit so the single-probe arm below can HAND THE WHOLE CALL BACK to the original code when
    /// the forensic is armed, instead of reproducing it against a captured length.
    pub(super) const TRACE_CORRUPT: u8 = 1 << 6;
}

/// Read every gate ONCE per process. `get_or_init` still costs one acquire load; the point is
/// that the whole opcode now pays exactly one instead of one per question asked.
#[inline(always)]
fn iv_gates() -> u8 {
    static G: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *G.get_or_init(|| {
        let mut f = 0u8;
        // Same three-term predicate the round-7 switch always had, unchanged.
        if crate::flags::round7()
            && !crate::flags::emit_more_disabled()
            && std::env::var_os("RAVA_NO_FAST_INVOKEVIRT").is_none()
        {
            f |= g::ROUND7;
        }
        if !crate::flags::emit_more_disabled()
            && std::env::var_os("RAVA_NO_FAST_IVIRT3").is_none()
        {
            f |= g::IVIRT3;
        }
        if crate::devirt::enabled() {
            f |= g::DEVIRT;
        }
        #[cfg(feature = "next-gen-gc")]
        if crate::receiver_guard_enabled() {
            f |= g::RECV_GUARD;
        }
        // Reading it here is what arms the census watchdog, exactly as the first `note` used to.
        if crate::vecgrow::enabled() {
            f |= g::VECGROW;
        }
        if !crate::flags::emit_more_disabled()
            && std::env::var_os("RAVA_NO_FAST_IVIRT4").is_none()
        {
            f |= g::IVIRT4;
        }
        // The forensic's own env, folded in here rather than read on the dispatch path. It was
        // reached only behind `c.len() > 8192`, which no real class name satisfies — but that
        // test is exactly the one the single-probe arm below cannot make, so the ARMED case has
        // to be recognisable from the gate word alone.
        if std::env::var_os("RAVA_TRACE_CORRUPT").is_some() {
            f |= g::TRACE_CORRUPT;
        }
        f
    })
}

/// ⭐**THE RECEIVER'S CLASS IS ONE FACT, AND THE TAIL ASKED THE HEAP FOR IT FOUR TIMES.**
///
/// Every question below the two inline caches that concerns a `Value::Ref` receiver is a
/// question about the object's CLASS, and each one used its own probe of the same slot —
/// `objects.get(h)`, the `ObjectSlot` liveness test, then the `Arc<str>`:
///
///   1. `Heap::class_of` for the guava-cache / fastutil-int-set predicates;
///   2. `Heap::class_of` AGAIN, ~90 lines later, to build `actual`;
///   3. `Heap::class_name_ptr` for the megamorphic-Ref PIC key;
///   4. `Heap::object_class_word` for the word the emitted guard reads.
///
/// (3) and (4) additionally ran on EVERY call that reaches the tail, including the ones that
/// never populate the PIC — a natively-served call (`resolved == None`, the native cascade) and
/// a call whose method is INHERITED (`method_index` → `None`) both compute the key and throw it
/// away, forever, because neither ever seeds the cache that would have skipped the tail.
///
/// COMPLEXITY: WAS four heap-slot probes per heap-`Ref` virtual call that misses the inline
/// caches. IS one, plus `object_class_word` only on the calls that actually seed the PIC.
/// What grows: nothing — this is three words in a register-sized struct, not a table.
///
/// ⚠**WHY A CAPTURED POINTER STAYS TRUE ACROSS THE TAIL.** The two things that can re-enter
/// guest code between probe (1) and its consumers — `cache::dispatch` and `fastutil::dispatch`,
/// each only when it returns `None` — may collect and may grow the heap, so the `&str` BORROW
/// cannot be held; a `usize` can. The class-name interner does not evict, so a class name's
/// `Arc<str>` data address is fixed for the life of the process; an object's class never
/// changes (`Object::class` is written only at allocation, beside `Object::lambda`); and the
/// receiver is rooted in `gc_protect` by `push_call_roots`, so its slot cannot be swept and
/// reused underneath us. That is the same standing assumption `tlcache::REF_INLINE` and the
/// `dispatch_class` memo already run on, and no weaker.
#[derive(Clone, Copy)]
struct RefClass {
    /// `Arc<str>::as_ptr` of the receiver's interned class name — the `dispatch_class` key and
    /// the `ref_inline` key, which are the SAME word by construction (see `Heap::class_name_ptr`).
    /// `0` means "not derived, or derived to nothing": a dead slot, an empty class name, or the
    /// kill switch. Every consumer treats `0` as "ask the heap the old way".
    ptr: usize,
    /// The class name is exactly `"$Lambda"`. A REJECT-ONLY prefilter for `Heap::is_lambda`,
    /// which stays the authority: `Heap::alloc_lambda` is the sole writer of `Object::lambda`
    /// and it interns `"$Lambda"` as that object's class in the same expression, so a class name
    /// that is not `"$Lambda"` cannot belong to a lambda. The converse is NOT relied on.
    lambda_name: bool,
}

impl RefClass {
    /// Nothing derived — every consumer falls back to the probe it always did.
    const NONE: Self = Self { ptr: 0, lambda_name: false };

    /// May this receiver be a lambda? `false` is a PROOF of "no" and skips `Heap::is_lambda`;
    /// `true` only means "ask the heap", which stays the authority. With the kill switch set the
    /// answer is always `true`, so the probe runs exactly as it always did.
    #[inline(always)]
    fn may_be_lambda(&self, gates: u8) -> bool {
        gates & g::IVIRT4 == 0 || self.lambda_name
    }
}

/// ⭐**SEVEN STRING COMPARES PER CALL FOR TWO ANSWERS THAT ARE CONSTANTS OF THE RECEIVER'S
/// CLASS, AND BOTH ANSWER `false`.**
///
/// `cache::is_cache_class` is two `&str` equalities; `fastutil::is_int_set` is
/// `INT_SETS.contains(&cls)`, a LINEAR SCAN of five more. Nothing in either is a function of the
/// call, and on a real program both are `false` for essentially every receiver — the interesting
/// classes are two synthetic ones and five fastutil set types.
///
/// COMPLEXITY: WAS seven `&str` comparisons (length test plus `bcmp`) per heap-`Ref` virtual call
/// that misses the inline caches. IS one byte load and two compares, with the full predicate run
/// only inside the matching arm. The membership answers themselves are unchanged.
///
/// ⚠**THE PREFILTER MAY ONLY REJECT, AND ITS PRECONDITION IS ASSERTED, NOT COMMENTED.** Both
/// cache classes begin with `r` (`rava/GuavaCache`, `rava/GuavaLoadingCache`) and every fastutil
/// int-set with `i` (`it/unimi/dsi/fastutil/ints/...`); a class name starting with neither can be
/// in neither set. `is_int_set`'s array is private to `fastutil`, so the guarantee is checked the
/// way `gauntlet_mask` checks its own digest — against the original predicates, on every call, in
/// a debug build.
#[inline(always)]
fn recv_special_class(c: &str, gates: u8) -> (bool, bool) {
    if gates & g::IVIRT4 == 0 {
        return (crate::cache::is_cache_class(c), crate::fastutil::is_int_set(c));
    }
    debug_assert!(
        crate::cache::CACHE_CLASS.as_bytes()[0] == b'r'
            && crate::cache::LOADING_CACHE_CLASS.as_bytes()[0] == b'r',
        "the guava-cache prefilter admits only class names starting with `r`"
    );
    let r = match c.as_bytes().first() {
        Some(&b'r') => (crate::cache::is_cache_class(c), false),
        Some(&b'i') => (false, crate::fastutil::is_int_set(c)),
        _ => (false, false),
    };
    debug_assert_eq!(
        r,
        (crate::cache::is_cache_class(c), crate::fastutil::is_int_set(c)),
        "receiver-class prefilter rejected a class one of the predicates accepts: {c}"
    );
    r
}

/// ⭐**THE GAUNTLET IS A PROPERTY OF THE CALL SITE, NOT OF THE CALL.**
///
/// Every special case between the inline caches and the resolved dispatch below opens with a
/// test on `owner`, `name` or `descriptor` — the three `Arc<str>`s of the site's constant-pool
/// member ref, which are FIXED for the life of that site. There are eleven of them (the gson
/// factory descriptor, `notify`/`notifyAll`, the Waitable hand-off, the two slf4j owners, the
/// guava `CacheBuilder.build`, the seven-name Object-method test, the ten-name functional
/// combinator test, `forEach`, `add(Object)Z`, `getShape`), and an interpreted virtual call
/// that misses the inline caches re-ran all of them, in order, against string literals.
///
/// COMPLEXITY: WAS O(number of special cases) string comparisons per CALL. IS one table probe
/// per call and one classification per SITE. What grows is executed virtual call sites, not
/// the ~24 000 000 virtual calls a boot makes.
// `FOR_EACH` is consumed only under `next-gen-gc`; the classification is shared either way.
#[allow(dead_code)]
mod gm {
    /// Slot occupied. A computed mask always carries it, so `0` can mean "empty".
    pub(super) const VALID: u64 = 1;
    pub(super) const GSON_CREATE: u64 = 1 << 1;
    pub(super) const NOTIFY: u64 = 1 << 2;
    pub(super) const WAITABLE_GET: u64 = 1 << 3;
    pub(super) const SLF4J: u64 = 1 << 4;
    pub(super) const CACHE_BUILD: u64 = 1 << 5;
    pub(super) const OBJECT_METHOD: u64 = 1 << 6;
    /// `build_fi_default` can return `Some` for this name — see the note on `FI_DEFAULT_NAMES`.
    pub(super) const FI_DEFAULT: u64 = 1 << 7;
    pub(super) const FOR_EACH: u64 = 1 << 8;
    pub(super) const ADD_OBJ: u64 = 1 << 9;
    pub(super) const GET_SHAPE: u64 = 1 << 10;
}

/// ⚠**MIRROR OF THE `match name` IN `gates::build_fi_default` — KEEP THE TWO IN STEP.**
/// The bit only decides whether the gauntlet ASKS; `build_fi_default` remains the sole
/// authority on what a combinator is and on building it. Listing a name here that the
/// function does not handle costs one wasted call; OMITTING one the function handles would
/// mistake a combinator for the SAM and apply the closure eagerly, which is why the list is
/// written out literally rather than approximated by length or prefix.
const FI_DEFAULT_NAMES: [&str; 10] = [
    "andThen", "compose", "negate", "and", "or", "reversed",
    "thenComparing", "thenComparingInt", "thenComparingLong", "thenComparingDouble",
];

/// ⭐**THE CLASSIFICATION IS NOT MEMOISED IN A PRODUCT BOOT, SO IT HAD TO GET CHEAP.**
///
/// `site_gauntlet`'s table is behind `g::ROUND7`, which is opt-in — so in the shipped
/// configuration `gauntlet_mask` runs on EVERY interpreted virtual call that misses the two
/// inline caches, which is every natively-served call, every lambda dispatch and every
/// `Str`/`Int`/`StdOut` receiver. It is the third-row opcode's own 0.44% of callback samples.
///
/// What it spent that on was string comparison. `match name` over nineteen literals compiles to
/// a length switch plus a `bcmp` chain, and then the LAST arm — reached by every name that is
/// none of them, i.e. by almost every call — ran `FI_DEFAULT_NAMES.contains(&n)`, a LINEAR SCAN
/// of ten more `&str` compares. Nothing in that is a function of the call; all of it is a
/// function of three bytes.
///
/// COMPLEXITY: WAS O(literals) `bcmp`s per classification — up to twenty-nine string compares,
/// of which the ten-element scan was unconditional. IS one integer switch on
/// `(len, first byte, last byte)` plus, only inside a matching arm, ONE confirming `==`. The
/// name space of a real program hits the `_` arm and leaves after three loads.
///
/// ⚠**EXACTNESS.** The key is a lossy digest, so an arm may only NARROW: every arm re-compares
/// the full name and applies precisely the bit its old arm applied. `add`/`and` are the only
/// pair that collide and share one arm that tests both. Arm ORDER stopped mattering because the
/// old match already gave each name at most one arm — a guard that failed (`create` with the
/// wrong descriptor, `get`/`build` with the wrong owner, `add` with the wrong descriptor) fell
/// through to arms none of which could match that name, `FI_DEFAULT_NAMES` included.
///
/// `#[cold]`: row 398's rule — a body this size must not have its locals reserved by the
/// prologue of the hottest function in the engine.
#[cold]
#[inline(never)]
fn gauntlet_mask(owner: &str, name: &str, desc: &str, gates: u8) -> u64 {
    if gates & g::IVIRT3 == 0 {
        return gauntlet_mask_scan(owner, name, desc);
    }
    const GSON_DESC: &str =
        "(Lcom/google/gson/Gson;Lcom/google/gson/reflect/TypeToken;)Lcom/google/gson/TypeAdapter;";
    let mut m = gm::VALID;
    match name_key(name) {
        K_CREATE if name == "create" => {
            if desc == GSON_DESC {
                m |= gm::GSON_CREATE;
            }
        }
        // Both are Object methods AND the monitor hand-off, so they set two bits.
        K_NOTIFY if name == "notify" => m |= gm::NOTIFY | gm::OBJECT_METHOD,
        K_NOTIFY_ALL if name == "notifyAll" => m |= gm::NOTIFY | gm::OBJECT_METHOD,
        K_GET if name == "get" => {
            if owner == "org/bukkit/craftbukkit/v1_12_R1/util/Waitable" {
                m |= gm::WAITABLE_GET;
            }
        }
        K_BUILD if name == "build" => {
            if owner == "com/google/common/cache/CacheBuilder" {
                m |= gm::CACHE_BUILD;
            }
        }
        K_TO_STRING if name == "toString" => m |= gm::OBJECT_METHOD,
        K_HASH_CODE if name == "hashCode" => m |= gm::OBJECT_METHOD,
        K_GET_CLASS if name == "getClass" => m |= gm::OBJECT_METHOD,
        K_EQUALS if name == "equals" => m |= gm::OBJECT_METHOD,
        K_WAIT if name == "wait" => m |= gm::OBJECT_METHOD,
        K_FOR_EACH if name == "forEach" => m |= gm::FOR_EACH,
        // The one digest collision in the whole set: `add` and `and` are both (3, 'a', 'd').
        // One arm, both names spelled out — `add` with any other descriptor sets no bit, which
        // is what falling past the old guard did.
        K_ADD_AND => {
            if name == "add" {
                if desc == "(Ljava/lang/Object;)Z" {
                    m |= gm::ADD_OBJ;
                }
            } else if name == "and" {
                m |= gm::FI_DEFAULT;
            }
        }
        K_GET_SHAPE if name == "getShape" => m |= gm::GET_SHAPE,
        // The ten combinator names, one arm each — see `FI_DEFAULT_NAMES` for why the list is
        // literal rather than approximated, and for the mirror it must stay in step with.
        K_AND_THEN if name == "andThen" => m |= gm::FI_DEFAULT,
        K_COMPOSE if name == "compose" => m |= gm::FI_DEFAULT,
        K_NEGATE if name == "negate" => m |= gm::FI_DEFAULT,
        K_OR if name == "or" => m |= gm::FI_DEFAULT,
        K_REVERSED if name == "reversed" => m |= gm::FI_DEFAULT,
        K_THEN_CMP if name == "thenComparing" => m |= gm::FI_DEFAULT,
        K_THEN_CMP_I if name == "thenComparingInt" => m |= gm::FI_DEFAULT,
        K_THEN_CMP_L if name == "thenComparingLong" => m |= gm::FI_DEFAULT,
        K_THEN_CMP_D if name == "thenComparingDouble" => m |= gm::FI_DEFAULT,
        _ => {}
    }
    // Owner-keyed, so it is tested independently of the name arm above. Both literals begin
    // with `o`; a class name that does not cannot be either, and almost none do (`net/`,
    // `java/`, `com/`, `it/`), so the byte test retires the pair without touching `bcmp`.
    debug_assert!(
        SLF4J_OWNERS.iter().all(|o| o.as_bytes()[0] == b'o'),
        "the slf4j owner prefilter admits only names starting with `o`"
    );
    if owner.as_bytes().first() == Some(&b'o')
        && matches!(owner, "org/slf4j/Logger" | "org/apache/logging/log4j/Logger")
    {
        m |= gm::SLF4J;
    }
    debug_assert_eq!(m, gauntlet_mask_scan(owner, name, desc), "gauntlet digest diverged");
    m
}

/// The two owners the `SLF4J` bit is keyed on, present so the prefilter above can assert its own
/// precondition instead of asserting a comment.
const SLF4J_OWNERS: [&str; 2] = ["org/slf4j/Logger", "org/apache/logging/log4j/Logger"];

/// `(length, first byte, last byte)` packed into one integer — the whole input to the switch
/// above. Lengths beyond `u16` cannot alias a name in the set, because none of them is longer
/// than nineteen bytes and the length is compared exactly; an empty name yields `0`, which no
/// key can equal (every key carries a non-zero length in its high half).
#[inline(always)]
fn name_key(s: &str) -> u32 {
    let b = s.as_bytes();
    match (b.first(), b.last()) {
        (Some(&f), Some(&l)) => ((b.len() as u32) << 16) | ((f as u32) << 8) | l as u32,
        _ => 0,
    }
}

/// `name_key` at compile time, so the constants below are derived from the literals themselves
/// and cannot drift out of step with the arms that confirm them.
const fn nk(s: &str) -> u32 {
    let b = s.as_bytes();
    ((b.len() as u32) << 16) | ((b[0] as u32) << 8) | b[b.len() - 1] as u32
}

const K_CREATE: u32 = nk("create");
const K_NOTIFY: u32 = nk("notify");
const K_NOTIFY_ALL: u32 = nk("notifyAll");
const K_GET: u32 = nk("get");
const K_BUILD: u32 = nk("build");
const K_TO_STRING: u32 = nk("toString");
const K_HASH_CODE: u32 = nk("hashCode");
const K_GET_CLASS: u32 = nk("getClass");
const K_EQUALS: u32 = nk("equals");
const K_WAIT: u32 = nk("wait");
const K_FOR_EACH: u32 = nk("forEach");
/// `add` and `and` share this digest; the arm tests both names.
const K_ADD_AND: u32 = nk("add");
const K_GET_SHAPE: u32 = nk("getShape");
const K_AND_THEN: u32 = nk("andThen");
const K_COMPOSE: u32 = nk("compose");
const K_NEGATE: u32 = nk("negate");
const K_OR: u32 = nk("or");
const K_REVERSED: u32 = nk("reversed");
const K_THEN_CMP: u32 = nk("thenComparing");
const K_THEN_CMP_I: u32 = nk("thenComparingInt");
const K_THEN_CMP_L: u32 = nk("thenComparingLong");
const K_THEN_CMP_D: u32 = nk("thenComparingDouble");

/// The classification as it was written: nineteen string literals in a `match`, then a linear
/// scan of ten more. Kept whole as the kill switch's arm and as the `debug_assert` oracle the
/// digest above is checked against, so the two can never silently disagree in a debug build.
fn gauntlet_mask_scan(owner: &str, name: &str, desc: &str) -> u64 {
    let mut m = gm::VALID;
    match name {
        "create"
            if desc
                == "(Lcom/google/gson/Gson;Lcom/google/gson/reflect/TypeToken;)Lcom/google/gson/TypeAdapter;" =>
        {
            m |= gm::GSON_CREATE
        }
        // Both are Object methods AND the monitor hand-off, so they set two bits.
        "notify" | "notifyAll" => m |= gm::NOTIFY | gm::OBJECT_METHOD,
        "get" if owner == "org/bukkit/craftbukkit/v1_12_R1/util/Waitable" => m |= gm::WAITABLE_GET,
        "build" if owner == "com/google/common/cache/CacheBuilder" => m |= gm::CACHE_BUILD,
        "toString" | "hashCode" | "getClass" | "equals" | "wait" => m |= gm::OBJECT_METHOD,
        "forEach" => m |= gm::FOR_EACH,
        "add" if desc == "(Ljava/lang/Object;)Z" => m |= gm::ADD_OBJ,
        "getShape" => m |= gm::GET_SHAPE,
        n if FI_DEFAULT_NAMES.contains(&n) => m |= gm::FI_DEFAULT,
        _ => {}
    }
    // Owner-keyed, so it is tested independently of the name arm above.
    if matches!(owner, "org/slf4j/Logger" | "org/apache/logging/log4j/Logger") {
        m |= gm::SLF4J;
    }
    m
}

/// 512 entries × 8 BYTES = **4 096 bytes**, one page, direct-mapped, no boxing and no
/// per-entry pointer chase. Deliberately not larger: a 2.5 KB → 14 KB growth of the field
/// interner's L1 table measured **+6.8% WORSE** than the allocation it removed (row 447), so
/// the table is sized to stay resident rather than to hold every site a boot ever executes.
///
/// A slot holds `(site_ref pointer << 16) | mask`. The pointer is a leaked `Box<SiteRef>`
/// owned by the caller's class, and user-space addresses are ≤ 48 bits, so the shift is
/// lossless and the readback compare is EXACT — a colliding site is rejected, never served.
/// The failure mode is therefore neutral, not wrong and not slower than the old code: a miss
/// costs exactly the classification the old path did per call, plus one store.
///
/// INVALIDATION: keyed on a pointer derived from `(caller class, cp index)`, the same key
/// shape and the same standing assumption as `tlcache::REF_INLINE`/`OBJ_INLINE` and
/// `IOF_MEMO` — the caller class is never unloaded, so its site boxes are never freed and
/// the address is never recycled. The answer itself depends on nothing else: the constant
/// pool is immutable.
const GM_SLOTS: usize = 512;
thread_local! {
    static GM_TABLE: [std::cell::Cell<u64>; GM_SLOTS] =
        const { [const { std::cell::Cell::new(0u64) }; GM_SLOTS] };
}

/// The site's gauntlet mask — one multiply, one shift, one load and one compare on a hit.
///
/// The slot is a MULTIPLICATIVE hash, not the low bits of the address: `SiteRef` boxes come
/// out of one size class, so consecutive sites are a fixed stride apart and `ptr >> k` would
/// light up every eighth slot and leave the rest of the table cold.
///
/// `gates` is the caller's already-loaded gate word rather than a `OnceLock` read of its own,
/// so the whole classification path — probe, miss, classify — adds no acquire load to a call
/// that has already paid for one.
#[inline(always)]
fn site_gauntlet(site_ref: &crate::class_loader::SiteRef, gates: u8) -> u64 {
    if gates & g::ROUND7 == 0 {
        return gauntlet_mask(&site_ref.owner, &site_ref.name, &site_ref.descriptor, gates);
    }
    let key = site_ref as *const crate::class_loader::SiteRef as u64;
    let slot = ((key.wrapping_mul(0x9E37_79B9_7F4A_7C15)) >> 55) as usize & (GM_SLOTS - 1);
    let e = GM_TABLE.with(|t| t[slot].get());
    if e & gm::VALID != 0 && (e >> 16) == key {
        // Mask off the key half. Every consumer tests a bit below 16 so returning the raw
        // entry would work today; it would stop working the moment a bit is added, and a
        // silently-set gauntlet bit is a wrong-dispatch bug, not a slow one.
        return e & 0xFFFF;
    }
    let m = gauntlet_mask(&site_ref.owner, &site_ref.name, &site_ref.descriptor, gates);
    GM_TABLE.with(|t| t[slot].set((key << 16) | m));
    m
}

/// `AbstractList.add(E)` emulation: does this receiver's class declare `add(int,E)` AS
/// BYTECODE? See the call site for what the answer is for.
///
/// COMPLEXITY: WAS one `resolve_method` per `add(Object)Z` that reached the native layer —
/// once per ELEMENT APPENDED, for an answer fixed by the receiver's class. `resolve_method`
/// memoises, but its key is three STRINGS, so every appended element paid a hash of the class
/// name, the method name and the descriptor plus the confirming compares. IS one hash of
/// three integers, and a resolve only on the first element for a given (site, class). What
/// grows is receiver classes seen at this site, not elements appended.
///
/// Keyed on (site, class identity) rather than class identity alone because the dispatch
/// class is NOT a function of the receiver's class alone: an arena receiver whose class id
/// misses the registry falls back to the SITE's declared owner (see `actual` below). Stable
/// for the same reason `tlcache::inline_put` is: the receiver's class is loaded (we hold an
/// instance) and so is its superclass chain, so no later class load can change the answer.
type AddPlan = Option<Arc<crate::class_loader::LoadedClass>>;
thread_local! {
    static ADD_PLAN: std::cell::RefCell<crate::FastMap<(usize, u16, u64), AddPlan>> =
        std::cell::RefCell::new(crate::FastMap::default());
}

/// The null-receiver trace's context test, moved off the throw path. `#[cold]`: it runs only
/// when `RAVA_TRACE_NET` is set, which is never in production.
#[cold]
#[inline(never)]
fn net_trace_context(vm: &Vm, frame: &Frame) -> bool {
    vm.in_pump || {
        let c = frame.class.file.this_class.as_str();
        c.contains("netty") || c.contains("Packet") || c.contains("Login")
            || c.contains("NetworkManager") || c.contains("PlayerConnection")
            || c.contains("PlayerList") || c.contains("LoginListener")
    }
}

/// The transitional `&SharedVm -> &mut Vm` escape for `invokevirtual`'s genuinely-`&mut`
/// callees — the SAME consolidated cast `invokestatic` hoisted into `invoke.rs` (one
/// definition per opcode file, so the uses below read as plain calls). Everything else on
/// this path is already `&self`-safe (`pop_args_n`, `push_call_roots`, `resolve_method`,
/// `super_chain`, `force_native_collection`, the whole `Heap` surface, and every inline
/// cache — `tlcache`, `ADD_PLAN`, `GM_TABLE`, `dispatch_class`/`FAST_CLASS`, `devirt`,
/// `lambda_site_note` — all thread-local or atomic, so the cache WRITES need no guard) or
/// has an `&self` twin (`make_exception_shared`, `ule_forensics` flipped to `&Vm`); only
/// the dispatch leaves (`invoke_idx`/`invoke`, `pending`) and the native-layer bridges
/// still need exclusivity until the interpreter loop is `&self`. Bit-identical to the
/// `&mut Vm` call it replaces; only the receiver's provenance differs, and every derivation
/// is consumed at its own call site — none is held across another.
#[inline(always)]
#[allow(clippy::mut_from_ref, invalid_reference_casting)]
fn vm_mut(vm: &crate::vm::thread_context::SharedVm) -> &mut Vm {
    unsafe {
        &mut *(vm as *const crate::vm::thread_context::SharedVm as *mut crate::vm::thread_context::SharedVm)
    }
}

/// `invokevirtual`/`invokeinterface` — `PrintStream.println`/`print` natives,
/// else virtual dispatch on the receiver's actual class.
///
/// ⭐**`&SharedVm`** — the invokestatic tracer bullet's twin: the body runs on the plain
/// shared reference (`&mut Vm` callers coerce for free); only the `vm_mut` leaves above
/// keep exclusivity. NPE allocation goes through `make_exception_shared`.
pub(crate) fn invokevirtual(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    index: u16,
) -> Result<(), VmError> {
    // One atomic increment for the whole resolved site, and the argument count comes with it —
    // `pop_args` used to walk the descriptor STRING on every call to recover a number that is
    // fixed for this call site.
    let site_ref = ref_site_ref(unsafe { frame.class_detached() }, index)?;
    // Every process-constant gate this body consults, in ONE acquire load — see `iv_gates`.
    // Read here rather than at each question so the dispatch below is bit tests on a register.
    let gates = iv_gates();
    let (owner, name, desc): (&str, &str, &str) =
        (&site_ref.owner, &site_ref.name, &site_ref.descriptor);
    let args = pop_args_n(vm, frame, site_ref.argc as usize)?;
    #[allow(unused_mut)]
    let mut receiver = frame.pop()?;
    // DEVIRT MONOMORPHISM PROBE (RAVA_DEVIRT_PROBE, read-only): record this virtual site's
    // receiver concrete class (0xb6 + 0xb9 both land here). Default off → no effect.
    if gates & g::DEVIRT != 0 {
        if let Some(k) = crate::devirt::recv_key(vm, &receiver) {
            crate::devirt::record((Arc::as_ptr(&frame.class) as usize, index), k);
        }
    }
    // GC-SAFETY: root the popped receiver + args across the dispatch (receiver-guard-
    // confirmed use-after-free: StateHolder's Reference2ObjectArrayMap swept mid-
    // populateNeighbours, slot reused as MapColor). See push_call_roots.
    // `gc_base` is the gc_protect slot the receiver clone lands in — its FORWARDED value is
    // re-read into locals[0] after resolve_method (below), since push_call_roots keeps the
    // object alive but does NOT update this stale `receiver`/`locals[0]` copy.
    #[cfg(feature = "next-gen-gc")]
    let gc_base = crate::vm::thread_context::root_len(&vm.gc_protect);
    #[cfg(feature = "next-gen-gc")]
    let _gc_root = push_call_roots(vm, Some(&receiver), &args);
    // FAST PATH (next-gen-gc): an arena receiver whose (site, class) already fell
    // through the special-case gauntlet to plain bytecode dispatch. Skip the entire
    // gauntlet, the receiver-class String allocation, and resolve_method. Disabled
    // when the opt-in receiver guard is on (the guard must observe every call).
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = &receiver {
        if gates & g::RECV_GUARD == 0 {
            let cid = crate::nextgen::arena_class_id(*p);
            let site = (std::sync::Arc::as_ptr(&frame.class) as usize, index);
            if let Some((decl, midx)) = crate::tlcache::obj_inline_get(site, cid) {
                let mut locals = args;
                // The census reads `capacity`/`len` BEFORE the insert, so those two loads and
                // the call were unconditional even though `note` opens by returning when the
                // census is off — the acquire load inside it is what stopped LLVM folding them
                // away. Behind the bit, an unarmed boot executes none of the three.
                let (cb, ln) = if gates & g::VECGROW != 0 {
                    (locals.capacity(), locals.len())
                } else {
                    (0, 0)
                };
                locals.insert(0, receiver);
                if gates & g::VECGROW != 0 {
                    crate::vecgrow::note(
                        crate::vecgrow::Site::InsertRecvFast,
                        cb,
                        locals.capacity(),
                        ln,
                    );
                }
                if vm.trampoline {
                        vm_mut(vm).pending = Some(crate::vmstate::PendingCall {
                        decl,
                        name: site_ref.name.clone(),
                        desc: site_ref.descriptor.clone(),
                        args: locals,
                        midx: Some(midx),
                    });
                    return Ok(());
                }
                if let Some(ret) = vm_mut(vm).invoke_idx(&decl, Some(midx), &name, &desc, locals)? {
                    frame.push(ret);
                }
                return Ok(());
            }
        }
    }
    // MEGAMORPHIC-REF FAST PATH (next-gen-gc): the Ref twin of the arena fast path
    // above. A heap receiver whose (site, class) already fell through the whole
    // invokevirtual gauntlet to plain bytecode dispatch — skip the gauntlet, the
    // class-name String alloc (`actual`), and `resolve_method`. Keyed on the interned
    // class-name pointer (cheap, no alloc), so a MEGAMORPHIC Ref site (many receiver
    // classes) is served per-class from the cache instead of re-resolving each call
    // (the ~736ns/call, 37× megamorphic gap). Sound: a fall-through is a function of
    // the site's fixed owner/name/desc + the receiver class (stable per (site,class)).
    #[cfg(feature = "next-gen-gc")]
    if let Value::Ref(h) = &receiver {
        if gates & g::RECV_GUARD == 0 {
            let cptr = vm.heap.class_name_ptr(*h);
            if cptr != 0 {
                let site = (std::sync::Arc::as_ptr(&frame.class) as usize, index);
                if let Some((decl, midx)) = crate::tlcache::ref_inline_get(site, cptr) {
                    let mut locals = args;
                    // As the arena arm above: the census pair is read only when armed.
                    let (cb, ln) = if gates & g::VECGROW != 0 {
                        (locals.capacity(), locals.len())
                    } else {
                        (0, 0)
                    };
                    locals.insert(0, receiver);
                    if gates & g::VECGROW != 0 {
                        crate::vecgrow::note(
                            crate::vecgrow::Site::InsertRecvSlow,
                            cb,
                            locals.capacity(),
                            ln,
                        );
                    }
                    if vm.trampoline {
                        vm_mut(vm).pending = Some(crate::vmstate::PendingCall {
                        decl,
                        name: site_ref.name.clone(),
                        desc: site_ref.descriptor.clone(),
                        args: locals,
                        midx: Some(midx),
                    });
                        return Ok(());
                    }
                    if let Some(ret) = vm_mut(vm).invoke_idx(&decl, Some(midx), &name, &desc, locals)? {
                        frame.push(ret);
                    }
                    return Ok(());
                }
            }
        }
    }
    // Classify this call site ONCE (see `site_gauntlet`). Deliberately AFTER the two inline
    // caches: a served call returns above without ever asking, so the memo is paid for only by
    // the calls that would otherwise have run the whole string gauntlet below.
    let gmask = site_gauntlet(site_ref, gates);
    // RECEIVER GUARD (opt-in RAVA_RECEIVER_GUARD): the interpreter twin of jit_call's
    // forensic — catches a stale arena `this` (its slot reused by an unrelated type)
    // on the JIT=0 dispatch path, where no jit_call runs. Logs the traitor before the
    // unresolved method throws its UnsatisfiedLinkError.
    #[cfg(feature = "next-gen-gc")]
    if gates & g::RECV_GUARD != 0 && matches!(receiver, Value::Ref(_) | Value::Obj(_)) {
        let caller_ptr = std::sync::Arc::as_ptr(&frame.class);
        vm_mut(vm).receiver_guard_check(caller_ptr, index, &receiver);
    }
    // gson `TypeAdapterFactory.create(Gson, TypeToken)` reached on a primitive
    // receiver: a stray primitive slipped into `Gson.factories`. Return null so
    // `getAdapter`'s loop skips this factory and a real one resolves the type,
    // instead of dispatching the call to `Integer.create` (unsupported).
    // Guard on the genuinely-primitive variants only — a real factory is a heap
    // handle (`Ref`) on the legacy heap OR an arena pointer (`Obj`) under
    // next-gen-gc, and both MUST dispatch normally. The old guard excluded only
    // `Ref`/`Array`, so under the arena every real factory (an `Obj`) wrongly hit
    // this skip and returned null — collapsing `Gson.factories` to nothing and
    // making `getAdapter` throw "GSON cannot handle <type>" for every class.
    if gmask & gm::GSON_CREATE != 0 && is_stray_primitive_factory(&receiver) {
        frame.push(Value::Null);
        return Ok(());
    }
    // notify/notifyAll: a no-op in the single-threaded model, a real generation bump once a
    // worker thread exists to be woken (`monitor.rs`).
    #[cfg(feature = "next-gen-gc")]
    let arena_array_notify =
        matches!(&receiver, Value::Obj(p) if crate::opcodes::arena_array_descriptor(*p).is_some());
    #[cfg(not(feature = "next-gen-gc"))]
    let arena_array_notify = false;
    if gmask & gm::NOTIFY != 0
        && (matches!(receiver, Value::Ref(_) | Value::Array(_)) || arena_array_notify)
    {
        if let Some(k) = crate::monitor::key_of(&receiver) {
            crate::monitor::notify(k);
        }
        return Ok(());
    }
    // CraftBukkit/Paper main-thread hand-off. A Waitable is posted to the server's
    // processQueue and the poster then blocks in `get()` (`while(status!=FINISHED)
    // wait()`) until the main thread drains the queue and runs it. Single-threaded,
    // the poster IS the main thread, so run the task inline at `get()`: invoke the
    // waitable's run() (which calls evaluate() and stores value/exception), then
    // return its value (or throw the captured exception). The call site's static
    // owner is Waitable even though the receiver is a concrete subclass (MCUtil$1).
    if gmask & gm::WAITABLE_GET != 0 {
        if let Value::Ref(h) = receiver {
            if !matches!(vm.heap.get_field(h, "$ran"), Value::Int(1)) {
                vm.heap.put_field(h, "$ran", Value::Int(1));
                let cls = vm.heap.class_of(h).unwrap_or("").to_string();
                if let Some(decl) = vm.resolve_method(&cls, "run", "()V")? {
                    vm_mut(vm).invoke(decl, "run", "()V", vec![Value::Ref(h)])?;
                }
            }
            // get() throws ExecutionException wrapping any captured throwable (`t`).
            if let Value::Ref(_) = vm.heap.get_field(h, "t") {
                return Err(VmError::Throw(vm.heap.get_field(h, "t")));
            }
            frame.push(vm.heap.get_field(h, "value"));
            return Ok(());
        }
    }

    // slf4j/log4j logging fast-path. The dedicated server logs via an slf4j
    // Logger backed by log4j; our VM doesn't surface log4j's ConsoleAppender,
    // so its messages (e.g. the EULA notice) would be invisible. Intercept the
    // log calls before resolving into log4j bytecode and emit the formatted
    // line straight to the process stdout. Generic for any slf4j logger.
    if gmask & gm::SLF4J != 0 {
        match name {
            "info" | "warn" | "error" | "fatal" | "debug" | "trace" => {
                let mut largs = Vec::with_capacity(args.len() + 1);
                largs.push(receiver);
                largs.extend(args);
                crate::natives::slf4j_log(vm_mut(vm), &name, &largs);
                return Ok(()); // void
            }
            n if n.starts_with("is") && n.ends_with("Enabled") => {
                // Console threshold is INFO: debug/trace disabled, rest enabled.
                let on = !n.contains("Debug") && !n.contains("Trace");
                frame.push(Value::Int(on as i32));
                return Ok(());
            }
            _ => {}
        }
    }

    // Generic Guava cache fast-path. `CacheBuilder.build[(loader)]` yields a
    // synthetic memoising cache; reads/writes on it are served by the VM-side
    // map + the real CacheLoader bytecode (see cache.rs). Sidesteps Guava's
    // size-eviction/weak-entry machinery, whose internal asserts assume a JVM.
    if gmask & gm::CACHE_BUILD != 0 {
        let c = if args.is_empty() {
            crate::cache::build_cache(vm_mut(vm))
        } else {
            crate::cache::build_loading_cache(vm_mut(vm), args[0].clone())
        };
        frame.push(c);
        return Ok(());
    }
    // The receiver's class, derived ONCE here and consumed by the whole tail below — the
    // guava/fastutil predicates, `is_lambda`, `actual` and the megamorphic-Ref PIC key. See
    // `RefClass` for why a captured address outlives the re-entrant calls in between and a
    // borrow does not. `NONE` when the receiver is not a heap `Ref` or the kill switch is set.
    let mut rc = RefClass::NONE;
    if let Value::Ref(h) = &receiver {
        // Read the receiver class by reference for the fast-path checks — the vast
        // majority of objects are neither a guava cache nor a fastutil int-set, so
        // both miss cheaply without the per-call String alloc this used to do.
        // COMPLEXITY: WAS TWO handle→object probes per call, one per predicate, for two
        // answers that are constants of the same class — and the second probe was paid by
        // every call, since a guava-cache receiver is the rare case. IS one probe, both
        // predicates decided from the single borrow. An object's class never changes, so
        // hoisting the read across the guava arm (which may re-enter and mutate the heap,
        // but cannot reclassify this handle) is exact.
        let (recv_is_cache, recv_is_int_set) = match vm.heap.class_of(*h) {
            Some(c) => {
                // THIS is the probe the rest of the tail now shares. `c.as_ptr()` is
                // `Heap::class_name_ptr`'s answer BY CONSTRUCTION — that function is
                // `o.class.as_ptr()` for exactly this case, a live object with a non-empty
                // class name, and `0` for the two it does not cover, which is the value
                // `RefClass::NONE` already carries.
                if gates & g::IVIRT4 != 0 && !c.is_empty() {
                    rc = RefClass { ptr: c.as_ptr() as usize, lambda_name: c == "$Lambda" };
                }
                recv_special_class(c, gates)
            }
            None => (false, false),
        };
        if recv_is_cache {
            let mut cargs = Vec::with_capacity(args.len() + 1);
            cargs.push(receiver.clone());
            cargs.extend(args.iter().cloned());
            // The cache may invoke the CacheLoader (re-enters); block GC while it
            // holds the key/map in Rust locals. Balance the decrement across `?`.
            crate::frames::native_depth_inc();
            let cr = crate::cache::dispatch(vm_mut(vm), &name, &cargs);
            crate::frames::native_depth_dec();
            if let Some(ret) = cr? {
                if let Some(v) = ret {
                    frame.push(v);
                }
                return Ok(());
            }
        }
        // fastutil int-set fast-path (may drain a Collection → re-enters; gate GC).
        if recv_is_int_set {
            let mut cargs = Vec::with_capacity(args.len() + 1);
            cargs.push(receiver.clone());
            cargs.extend(args.iter().cloned());
            crate::frames::native_depth_inc();
            let r = crate::fastutil::dispatch(vm_mut(vm), &name, &cargs);
            crate::frames::native_depth_dec();
            if let Some(ret) = r? {
                if let Some(v) = ret { frame.push(v); }
                return Ok(());
            }
        }
    }

    // `flush`/`close` on System.out / System.err: nothing is buffered on this path — the write
    // already went to the handle — so this is a no-op. It was MISSING, and a missing native is
    // not a slow path but an `UnsatisfiedLinkError`: `bench/BootPhaseC`, the probe written to
    // attribute the unexplained 10 s between `Environment:` and the yggdrasil failure, died on
    // its first `System.out.flush()` and had never produced a single step timing.
    if matches!(receiver, Value::StdOut | Value::StdErr) && (name == "flush" || name == "close") {
        return Ok(());
    }
    if matches!(receiver, Value::StdOut | Value::StdErr) && (name == "println" || name == "print") {
        // Z/C print by declared type; everything else via String.valueOf semantics.
        let arg_type = desc.as_bytes().get(1).copied();
        let text = match arg_type {
            Some(b'Z') | Some(b'C') => format_print_arg(&desc, args.first()),
            _ => match args.first() { Some(v) => vm_mut(vm).value_to_string(v)?, None => String::new() },
        };
        if matches!(receiver, Value::StdErr) {
            eprintln!("{text}");
        } else {
            vm_mut(vm).print(&text, name == "println");
        }
        return Ok(());
    }
    // printf / format: format string + Object[] varargs; returns the stream.
    // BOTH overloads exist: (String,Object...) and (Locale,String,Object...). Reading
    // the format from args[0] unconditionally made the Locale form format the EMPTY
    // string — `System.out.printf(Locale.ROOT, "%-16s%n", x)` printed NOTHING at all,
    // silently. Select the operand pair from the DESCRIPTOR, which distinguishes the
    // two unambiguously (a Locale receiver value is otherwise just another Ref).
    if matches!(receiver, Value::StdOut | Value::StdErr) && (name == "printf" || name == "format") {
        let fi = if desc.starts_with("(Ljava/util/Locale;") { 1 } else { 0 };
        let fmt = crate::natives::str_of(vm, args.get(fi)).unwrap_or_default();
        let fargs: Vec<Value> = crate::natives::varargs(vm, args.get(fi + 1))
            .iter().map(|v| crate::natives::stringify(vm_mut(vm), v)).collect();
        let text = match crate::natives::format_java(&fmt, &fargs) {
            Ok(t) => t,
            Err((class, msg)) => return Err(VmError::Throw(vm.make_exception_shared(class, &msg))),
        };
        if matches!(receiver, Value::StdErr) { eprint!("{text}"); } else { vm_mut(vm).print(&text, false); }
        frame.push(receiver); // PrintStream return for chaining
        return Ok(());
    }

    // Arena arrays are Value::Obj, so give them the same Object-surface methods as legacy array
    // handles.  The CID is retained across clone and the raw cell copy preserves both primitive
    // payloads and encoded reference cells without inventing a synthetic class name.
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = &receiver {
        if let Some(descriptor) = crate::opcodes::arena_array_descriptor(*p) {
            let array = *p;
            match name {
                "clone" => {
                    let cid = crate::nextgen::arena_class_id(array);
                    let nf = crate::nextgen::arena_nfields(array);
                    let clone = match crate::nextgen::tlab_try_alloc(cid, nf) {
                        Some(np) => np,
                        None => {
                            vm_mut(vm).arena_collect_jit();
                            crate::nextgen::tlab_try_alloc(cid, nf)
                                .expect("arena array clone alloc after collect")
                        }
                    };
                    let source = match crate::vm::thread_context::root_get(&vm.gc_protect, gc_base)
                    {
                        Some(Value::Obj(q)) => q,
                        _ => array,
                    };
                    for i in 0..nf as usize {
                        crate::nextgen::arena_set_cell(
                            clone,
                            i,
                            crate::nextgen::arena_cell(source, i),
                        );
                    }
                    frame.push(Value::Obj(clone));
                }
                "getClass" => frame.push(vm_mut(vm).make_class(&descriptor)),
                "hashCode" => frame.push(Value::Int(crate::nextgen::arena_identity_hash(array))),
                "getName" | "getTypeName" | "getSimpleName" | "getCanonicalName" => {
                    frame.push(Value::Str(descriptor.replace('/', ".").into()));
                }
                _ => return Err(VmError::UnsupportedNative(format!("array.{name}{desc}"))),
            }
            return Ok(());
        }
    }

    // Array methods (arrays are objects in the JVM): clone/getClass.
    if let Value::Array(h) = receiver {
        match name {
            "clone" => {
                // Preserve the ELEMENT TYPE: byte[].clone() must stay a packed
                // byte array. The old blanket "Ljava/lang/Object;" turned every
                // cloned primitive array into a ref-cell array, which the
                // compiled (packed-layout) array-load path misread — guava
                // HashCode.asBytes() → RandomSupport.seedFromHashOf → EVERY
                // named worldgen noise seeded off garbage once the caller
                // JIT-compiled (bench/CloneBytes.java is the 2s repro).
                let elem = vm.heap.array_element(h).unwrap_or_else(|| std::sync::Arc::from("Ljava/lang/Object;")).to_string();
                let vals = vm.heap.array_values(h).unwrap_or_default();
                let nh = vm.heap.alloc_values(&elem, vals);
                frame.push(Value::Array(nh));
            }
            "getClass" => {
                // An array's class is "[<element-descriptor>", not its declaring
                // owner (java/lang/Object). snakeyaml keys RepresentArray on
                // `Object[].class`, so a wrong class here made it match everything.
                let elem = vm.heap.array_element(h).unwrap_or_else(|| std::sync::Arc::from("Ljava/lang/Object;")).to_string();
                let cn = crate::natives::array_class_name(&elem);
                let c = vm_mut(vm).make_class(&cn);
                frame.push(c);
            }
            "hashCode" => frame.push(Value::Int(h as i32)),
            // Class-surface methods reached on an array (e.g. via a Class value
            // that IS the array object in our model): JVM binary name "[<elem>".
            "getName" | "getTypeName" | "getSimpleName" | "getCanonicalName" => {
                let elem = vm.heap.array_element(h).unwrap_or_else(|| std::sync::Arc::from("Ljava/lang/Object;")).to_string();
                frame.push(Value::Str(format!("[{}", elem).replace('/', ".").into()));
            }
            _ => return Err(VmError::UnsupportedNative(format!("array.{name}{desc}"))),
        }
        return Ok(());
    }

    // Functional-interface SAM call → dispatch to the captured implementation.
    // A *default* method (andThen/compose/negate/reversed/...) builds a new
    // combinator instead of being mistaken for the SAM. Object methods
    // (toString/hashCode/equals/getClass) are NEVER the SAM — invoking the
    // lambda for them is a bug (e.g. DFU's Codec.lazyInitialized calls
    // supplier.toString(), which must NOT run the supplier), so let them fall
    // through to the Object natives on the lambda object itself.
    let is_object_method = gmask & gm::OBJECT_METHOD != 0;
    if let Value::Ref(h) = receiver {
        // ⭐**ORDER: THE BIT FIRST, THE HEAP PROBE LAST.** Both operands are pure, so the swap is
        // exact. `is_object_method` is a bit already in a register; `Heap::is_lambda` is a slot
        // probe. Written the other way round, every `toString`/`hashCode`/`equals`/`getClass`/
        // `wait` call on a heap receiver — which can never take this arm — paid the probe to
        // learn something the bit had already decided.
        //
        // `may_be_lambda` then retires the probe for the ordinary receivers too: a lambda's class
        // is the interned `"$Lambda"` and nothing else (`Heap::alloc_lambda` is the only writer of
        // `Object::lambda`, and it sets both in one expression; the slot-reuse path in
        // `objects_alloc` copies `class` and `lambda` together), so a class name that is not
        // `"$Lambda"` PROVES the receiver is not a lambda. The heap remains the authority for the
        // positive answer — this may only reject.
        debug_assert!(
            rc.may_be_lambda(gates) || !vm.heap.is_lambda(h),
            "the $Lambda class-name prefilter rejected a real lambda receiver"
        );
        if !is_object_method && rc.may_be_lambda(gates) && vm.heap.is_lambda(h) {
            // ⭐**DO NOT ASK WHETHER THIS IS A COMBINATOR — THE SITE ALREADY ANSWERED.**
            // `build_fi_default` opens by CLONING `args[0]` (an `Arc` refcount pair for a
            // ref or a string, plus the drop glue on the way out) and then matches the
            // method name against ten literals, only to return `None` for every SAM call —
            // i.e. for almost every lambda dispatch in the boot. The name is a site
            // constant, so the clone and the ten compares are now skipped outright.
            if gmask & gm::FI_DEFAULT != 0 {
                if let Some(combinator) = build_fi_default(vm_mut(vm), &Value::Ref(h), &name, &args) {
                    frame.push(combinator);
                    return Ok(());
                }
            }
            // A CONCRETE (default) method on the declared functional interface is
            // not the SAM — run its real bytecode with the lambda as receiver
            // (e.g. fastutil Int2ObjectFunction.containsKey() returns true)
            // rather than mistaking the call for the SAM and running the closure.
            // The call-site owner may only DECLARE the method abstractly (e.g.
            // AttributeModifier.argumentCodec); the default lives on the lambda's
            // actual functional interface ($fi, the indy return type), so try both.
            // TWO `String` ALLOCATIONS PER LAMBDA DISPATCH, FOR TWO NAMES NOBODY KEEPS.
            // `resolve_method` borrows its first argument as `&str`; the site's owner already
            // IS one, and `$fi` arrives as an `RStr` that derefs to one. WAS: a heap
            // allocation + copy of each, per SAM call, discarded at the end of the loop. IS:
            // zero allocations — the `RStr` is the refcount the field read already produced.
            let fi = match vm.heap.get_field(h, "$fi") { Value::Str(s) => Some(s), _ => None };
            for cls in [Some(owner), fi.as_deref()].into_iter().flatten() {
                if let Some(decl) = vm.resolve_method(cls, &name, &desc)? {
                    if decl.file.method(&name, &desc).map(|m| m.code.is_some()).unwrap_or(false) {
                        let mut locals = Vec::with_capacity(args.len() + 1);
                        locals.push(Value::Ref(h));
                        locals.extend(args);
                        if vm.trampoline {
                            vm_mut(vm).pending = Some(crate::vmstate::PendingCall { decl: decl, name: site_ref.name.clone(), desc: site_ref.descriptor.clone(), args: locals, midx: None });
                            return Ok(());
                        }
                        if let Some(ret) = vm_mut(vm).invoke(decl, &name, &desc, locals)? {
                            frame.push(ret);
                        }
                        return Ok(());
                    }
                }
            }
            // The site's return type against the implementation's: see `Vm::sam_box_return`.
            let impl_desc = vm.lambda_impl_desc(&Value::Ref(h));
            if let Some(ret) = vm_mut(vm).invoke_lambda(h, args)? {
                let ret = match impl_desc {
                    Some(d) => vm_mut(vm).sam_box_return(ret, &desc, &d),
                    None => ret,
                };
                frame.push(ret);
            }
            return Ok(());
        }
        // Synthetic functional objects (identity/compose/collecting consumer).
        if !is_object_method && matches!(vm.heap.get_field_syn(h, crate::heap::Syn::FnKind), Value::Str(_)) {
            // A combinator (andThen/compose/negate/...) on a synthetic functional
            // must build a NEW deferred combinator — not be mistaken for the SAM
            // and applied eagerly. (DFU's RecordCodecBuilder composes getters with
            // andThen during codec build; applying eagerly runs the record ctor
            // with null fields.) Whether the name CAN be one is the site's answer.
            if gmask & gm::FI_DEFAULT != 0 {
                if let Some(combinator) = build_fi_default(vm_mut(vm), &Value::Ref(h), &name, &args) {
                    frame.push(combinator);
                    return Ok(());
                }
            }
            if let Some(ret) = vm_mut(vm).apply(&Value::Ref(h), args)? {
                frame.push(ret);
            }
            return Ok(());
        }
    }

    // RAVA_ARENA_LAMBDA: a CAPTURING lambda born in the arena arrives here as a
    // Value::Obj (its class "[arena-lambda:…]" is never "$Lambda", so the λ-PIC's
    // REF_TAG guard already dropped it to this general path). Route it EXACTLY like
    // the handle arm above (build_fi_default combinator → $fi/owner concrete default
    // method → SAM apply), using the representation-agnostic value-level accessors.
    // A special-case check in the gauntlet above (resolve_method / value_to_string / a
    // class-name String alloc) may have run a collection that moved the receiver; re-read the
    // forwarded pointer from its gc_protect slot so the arena-lambda dispatch below AND `actual`
    // (the dispatch class) + the callee `this` derive from the LIVE object, not a stale pre-move
    // copy whose slot may be reused (→ class None → the worldgen minValue UnsatisfiedLinkError,
    // or an arena-lambda decoded off a recycled header → "apply on '' (no SAM)").
    #[cfg(feature = "next-gen-gc")]
    if matches!(receiver, Value::Obj(_) | Value::Ref(_) | Value::Array(_)) {
        if let Some(fresh) = crate::vm::thread_context::root_get(&vm.gc_protect, gc_base) {
            receiver = fresh;
        }
    }
    // RAVA_ARENA_LAMBDA: a CAPTURING lambda born in the arena arrives here as a
    // Value::Obj (its class "[arena-lambda:…]" is never "$Lambda", so the λ-PIC's
    // REF_TAG guard already dropped it to this general path). Route it EXACTLY like
    // the handle arm above (build_fi_default combinator → $fi/owner concrete default
    // method → SAM apply), using the representation-agnostic value-level accessors.
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = receiver {
        if crate::nextgen::is_arena_lambda(p) && !is_object_method {
            // Same two removals as the handle arm above: the combinator question is the
            // site's, and `owner` is already the `&str` `resolve_method` wants.
            if gmask & gm::FI_DEFAULT != 0 {
                if let Some(combinator) = build_fi_default(vm_mut(vm), &receiver, &name, &args) {
                    frame.push(combinator);
                    return Ok(());
                }
            }
            let fi = vm.value_lambda_fi(&receiver);
            for cls in [Some(owner), fi.as_deref()].into_iter().flatten() {
                if let Some(decl) = vm.resolve_method(cls, &name, &desc)? {
                    if decl.file.method(&name, &desc).map(|m| m.code.is_some()).unwrap_or(false) {
                        let mut locals = Vec::with_capacity(args.len() + 1);
                        locals.push(receiver.clone());
                        locals.extend(args);
                        if vm.trampoline {
                            vm_mut(vm).pending = Some(crate::vmstate::PendingCall { decl: decl, name: site_ref.name.clone(), desc: site_ref.descriptor.clone(), args: locals, midx: None });
                            return Ok(());
                        }
                        if let Some(ret) = vm_mut(vm).invoke(decl, &name, &desc, locals)? {
                            frame.push(ret);
                        }
                        return Ok(());
                    }
                }
            }
            // ⭐**BREAK (1) OF THE LAMBDA SAM INLINE CHAIN: THIS IS THE PROFILE WRITE THAT DID
            // NOT EXIST.**
            //
            // `emit_guarded_inline` only ever sees a callee that `collect_inline_targets` built
            // from a receiver profile, and the only profile writer in the engine —
            // `tlcache::obj_inline_put`, several hundred lines below — is unreachable from here:
            // this arm resolves the SAM and RETURNS. So every lambda call site in the program was
            // invisible to the inliner, and the guarded inliner has never been offered one.
            //
            // ⚠**PLACED HERE AND NOWHERE EARLIER, ON PURPOSE.** The two branches above this line
            // — an `$fi`/owner CONCRETE default method, and a `build_fi_default` combinator — run
            // real bytecode that is NOT the closure's implementation. A profile written before
            // them would claim a SAM apply for a receiver that never performs one, and the
            // compiled guard would then splice the implementation body over a default method. The
            // write belongs on the edge that actually applies the closure, which is this one.
            //
            // What is recorded is the (class id, SITE index) PAIR plus the MethodHandle kind and
            // the capture count, because the class alone is a capture SIGNATURE shared by every
            // closure of that shape and names no implementation. See `jit::lambda_site_note` for
            // the cost, and `jit::lambda_site_mono` for what the admission may conclude from it —
            // in short, nothing that is not re-proved by a guard at run time.
            crate::jit::lambda_site_note(
                std::sync::Arc::as_ptr(&frame.class) as usize,
                index,
                crate::nextgen::arena_class_id(p),
                crate::nextgen::arena_lambda_site_idx(p),
                crate::nextgen::arena_lambda_kind(p),
                crate::nextgen::arena_lambda_cap_n(p) as u32,
            );
            let impl_desc = vm.lambda_impl_desc(&receiver);
            if let Some(ret) = vm_mut(vm).invoke_lambda_value(&receiver, args)? {
                let ret = match impl_desc {
                    Some(d) => vm_mut(vm).sam_box_return(ret, &desc, &d),
                    None => ret,
                };
                frame.push(ret);
            }
            return Ok(());
        }
    }

    let actual = match &receiver {
        // ⭐**THE CLASS WAS ALREADY DERIVED — SPEND THE PROBE ONCE, NOT TWICE.** `rc.ptr` is
        // `class_of(h).as_ptr()` from the guava/fastutil predicate block ~90 lines above, and
        // `dispatch_class`'s key is defined as exactly that pointer. On a memo HIT — which is
        // every call after the first for a given class — the name is never needed at all, so
        // this arm reaches the heap ZERO times; on a MISS (once per class, ever) it re-probes
        // for the name it must build the `Arc` from. WAS: one `Heap::class_of` per heap-`Ref`
        // virtual call reaching `actual`, i.e. per natively-served call and per first resolve.
        // Skipped wholesale when `RAVA_TRACE_CORRUPT` is armed, so that forensic keeps its own
        // `c.len()` test on the original path below rather than a reconstruction of it.
        // `IVIRT3` is in the guard as well because this arm SUBSUMES round 3's — same key, one
        // fewer probe — so its own kill switch has to keep reaching round 3's code.
        Value::Ref(h)
            if rc.ptr != 0
                && gates & g::IVIRT3 != 0
                && gates & g::TRACE_CORRUPT == 0 =>
        {
            // The forwarded re-read above cannot have changed WHICH object this is: a heap
            // handle is a slot index, not an address, so the arena's forwarding pass leaves it
            // alone and the rooted clone comes back as the same `Value::Ref(h)`.
            debug_assert_eq!(
                rc.ptr,
                vm.heap.class_name_ptr(*h),
                "hoisted receiver class pointer diverged from the heap's"
            );
            dispatch_class(rc.ptr as u64 | IOF_REF_TAG, || {
                Arc::from(vm.heap.class_of(*h).unwrap_or(owner))
            })
        }
        Value::Ref(h) => {
            // FORENSIC (RAVA_TRACE_CORRUPT): the GC-cycle-1 SIGSEGV is a corrupted heap
            // Object whose `class: Arc<str>` has a garbage length → to_string reads past
            // the buffer. Reading the &str LENGTH (a fat-ptr field, not a deref of its
            // data) is safe even when corrupt; an insane length pinpoints the corrupt
            // handle + the guest call context (which method dispatched onto it) BEFORE
            // the crash. Falls back to `owner` so the boot continues past the first hit
            // and reveals the PATTERN. Only active under the env — default unchanged.
            let cls = vm.heap.class_of(*h);
            let c = cls.unwrap_or(&owner);
            if c.len() > 8192 && std::env::var_os("RAVA_TRACE_CORRUPT").is_some() {
                let (len, hh) = (c.len(), *h);
                let (addr, in_arena, cnt) = vm.heap.box_probe(hh);
                eprintln!("[CORRUPT-CLASS] handle={hh} dispatch={name}{desc} caller={owner} class-len={len} box_addr={addr:#x} in_arena_range={in_arena} boxes_in_arena_range={cnt}\n{}", vm.backtrace());
                site_ref.owner.clone()
            } else if let (true, Some(cn)) = (gates & g::IVIRT3 != 0 && !c.is_empty(), cls) {
                // ⭐**THE RECEIVER'S CLASS, DERIVED ONCE.**
                //
                // `class_of` and `class_name_ptr` are the SAME probe — `objects.get(h)`, the
                // liveness test, then `o.class` — and this one expression ran both, back to
                // back, on the same handle: the borrow for the name, then a second full slot
                // walk purely to recover the address of the `Arc<str>` that borrow is already
                // pointing INTO. `class_name_ptr` is defined as `o.class.as_ptr()`, so `cn`'s
                // own data pointer IS the key, by construction and not by coincidence.
                //
                // COMPLEXITY: WAS two object-slot probes per Ref dispatch that reaches
                // `actual` — every natively-served virtual call, i.e. the population behind the
                // `dispatch_class<invokevirtual::closure_env>` profile row. IS one.
                //
                // ⚠**THE KEY IS BYTE-IDENTICAL, NOT MERELY EQUIVALENT.** `class_name_ptr` is
                // `o.class.as_ptr()` for exactly the case this arm guards — a LIVE object with
                // a NON-EMPTY class name — and `0` for the other two (no live object, empty
                // name). Those two keep the original expression below, second probe and all, so
                // no cache slot changes hands and the shared `0 | IOF_REF_TAG` entry behaves
                // exactly as it always has. The entire hot population is in the guarded arm.
                //
                // Key stability is the standing assumption of every sibling cache and no weaker
                // here: the class-name interner does not evict, so a live object's class `Arc`
                // outlives the process and its address is never recycled.
                dispatch_class(cn.as_ptr() as usize as u64 | IOF_REF_TAG, || Arc::from(cn))
            } else {
                // COLD, AND UNCHANGED: no live object behind the handle, an empty class name,
                // or the kill switch. `class_name_ptr` answers `0` for the first two, which is
                // the shared slot the memo has always given them.
                //
                // The name is a pure function of the class identity the sibling
                // dispatch caches already key on, so share ONE `Arc<str>` per class
                // instead of allocating a fresh copy per call (see `dispatch_class`).
                dispatch_class(vm.heap.class_name_ptr(*h) as u64 | IOF_REF_TAG, || Arc::from(c))
            }
        }
        // An ArenaString receiver dispatches EXACTLY like a Value::Str — its registry
        // class name is the "[arena-string]" placeholder, which no native owner key
        // matches (a compiled ldc interns PERM ArenaStrings, so they now reach
        // generic dispatch sites: gson's LinkedTreeMap comparator calling
        // `key.compareTo(other)` ULE'd here).
        // `wrapper_class`, NOT `Arc::from`: this arm ran on every virtual dispatch with an arena
        // string receiver and allocated a FRESH `Arc<str>` copy each time, while every sibling arm
        // - including `Value::Str` below, which returns the identical name - goes through the memo.
        // Verified caller attribution put `opcodes::dispatch_class<invokevirtual::closure_env$3>`
        // at 27.3% of `mi_malloc_aligned` samples, and allocation (gc 5.5% + malloc 4.5%) is the
        // largest leaf block of the serial window. Same value, one shared Arc, keyed on the
        // literal's address.
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if crate::nextgen::is_arena_string(*p) => wrapper_class("java/lang/String"),
        // Arena object (next-gen-gc): the concrete runtime class lives in the
        // arena header, not the call-site static type. Use it for virtual
        // dispatch — else an interface-typed call site (owner = the interface)
        // would resolve to no bytecode and fall through to the native layer.
        // The registry read below takes a GLOBAL MUTEX and clones an `Arc`; without
        // the memo that happens on every arena-receiver dispatch that misses the
        // per-(site,class) caches — i.e. on every native-served call, which is what
        // the fallback tally's top rows are.
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            let cid = crate::nextgen::arena_class_id(*p);
            match dispatch_class_opt(cid as u64, || crate::nextgen::class_name(cid)) {
                Some(a) => a,
                None => site_ref.owner.clone(),
            }
        }
        // Values reached through an interface (Comparable/CharSequence/Number)
        // route to their concrete wrapper so the natives resolve.
        Value::Str(_) => wrapper_class("java/lang/String"),
        Value::Int(_) => wrapper_class("java/lang/Integer"),
        Value::Long(_) => wrapper_class("java/lang/Long"),
        Value::Float(_) => wrapper_class("java/lang/Float"),
        Value::Double(_) => wrapper_class("java/lang/Double"),
        Value::Null => {
            // WAS, PER THROWN NPE: seven substring searches over the caller's class name to
            // build `net_ctx`, then up to two `std::env::var` calls — each of which takes the
            // process environment lock and ALLOCATES a `String` — to decide whether to print a
            // trace that is off in production. All three inputs are process constants except
            // the context walk, which now runs only behind the flag that consumes it. The
            // `&&` reorder is free: `net_trace_context` is pure.
            if crate::env_flag!("RAVA_TRACE_THROW")
                || (crate::env_flag!("RAVA_TRACE_NET") && net_trace_context(vm, frame))
            {
                eprintln!("[NET-NPE] {owner}.{name}{desc} | caller={} pc={}\n{}",
                    frame.class.file.this_class, frame.pc, vm.backtrace());
                // Frame forensics (fatal path): the faulting frame's locals — for a
                // null that CAME FROM somewhere (a map value, an entry field), the
                // source object is usually still in a local; dump refs + their key
                // synthetic fields so the corrupt holder is identifiable post-mortem.
                for (i, v) in frame.locals().iter().enumerate() {
                    match v {
                        Value::Ref(h) => {
                            let cls = vm.heap.class_of(*h).unwrap_or("?").to_string();
                            let key = vm.heap.get_field_syn(*h, crate::heap::Syn::Key);
                            let val = vm.heap.get_field_syn(*h, crate::heap::Syn::Val);
                            eprintln!("[NPE-FRAME] local[{i}] Ref({h}) {cls} $key={key:?} $val={val:?}");
                        }
                        #[cfg(feature = "next-gen-gc")]
                        Value::Obj(p) => {
                            let hdr = unsafe { (p.addr() as *const u64).read_unaligned() };
                            eprintln!(
                                "[NPE-FRAME] local[{i}] Obj({:#x}) {:?} hdr={hdr:#018x}",
                                p.addr(),
                                crate::nextgen::class_name(unsafe { crate::nextgen::obj_class(p.as_ptr()) })
                            );
                        }
                        Value::Null => {}
                        other => eprintln!("[NPE-FRAME] local[{i}] {other:?}"),
                    }
                }
            }
            // Targeted diagnostic (RAVA_TRACE_ITEM): dump the call chain for a
            // null-receiver collection/ItemStack accessor so we see which field
            // held null. Covers the player-tick isEmpty NPE and world-tick
            // first()/iterator() NPEs.
            else if vm.trace.item
                && matches!(name, "isEmpty" | "first" | "last" | "iterator" | "get" | "size")
            {
                eprintln!("[COLL-NPE] {owner}.{name}{desc} | caller={} pc={}\n{}",
                    frame.class.file.this_class, frame.pc, vm.backtrace());
            }
            // The JVM's helpful message, whose exact shape is measured in bench/NpeMsg.java.
            // The receiver sits BELOW the arguments, so the walk is told how many operands
            // are above it.
            let src = crate::npe::source_at(
                if frame.code.is_null() { &[] } else { unsafe { &(*frame.code).bytecode } },
                frame.pc,
                crate::count_args(desc),
                &frame.class.file.pool,
            );
            let npe = vm.make_exception_shared(
                "java/lang/NullPointerException",
                &crate::npe::invoke(&owner, name, desc, src),
            );
            return Err(VmError::Throw(npe));
        }
        _ => site_ref.owner.clone(),
    };

    // Capture the arena class id (if any) BEFORE the receiver is moved into the
    // callee locals, so a plain dispatch below can seed the class-id fast cache.
    #[cfg(feature = "next-gen-gc")]
    let obj_cid = match &receiver {
        Value::Obj(p) => Some(crate::nextgen::arena_class_id(*p)),
        _ => None,
    };
    // The Ref receiver's HANDLE, kept across the move into `locals` so the megamorphic-Ref PIC
    // key can be derived at the one place that uses it (mirrors `obj_cid`).
    //
    // ⭐**THE KEY WAS BUILT ON EVERY CALL AND USED ON ALMOST NONE.** `ref_inline_put` runs only
    // in the `Some(decl)` arm and only when `method_index` yields an index; the two populations
    // that fail that test — a natively-served call (`resolved == None`) and a call whose method
    // is INHERITED — never seed the cache, so they never take the fast path either, and they paid
    // both probes on EVERY call for the life of the process. Capturing the handle costs a copy of
    // a `u32`; the probes move to the site that consumes them.
    //
    // COMPLEXITY: WAS two heap-slot probes per heap-`Ref` virtual call reaching the resolved
    // tail. IS one (`object_class_word`), on the calls that actually populate the PIC, plus zero
    // for the key itself — `rc.ptr` already holds it.
    #[cfg(feature = "next-gen-gc")]
    let ref_recv: Option<u32> = match &receiver {
        Value::Ref(h) if gates & g::RECV_GUARD == 0 && gates & g::IVIRT4 != 0 => Some(*h),
        _ => None,
    };
    // Ref receiver's interned class identity, for the megamorphic-Ref cache populate
    // below (mirrors obj_cid). Only when a Ref falls through to plain dispatch.
    // KILL-SWITCH ARM ONLY (`RAVA_NO_FAST_IVIRT4=1`): the eager two-probe form, unchanged.
    #[cfg(feature = "next-gen-gc")]
    // ⚠**TWO WORDS, AND THEY ARE NOT THE SAME ONE.** `class_name_ptr` is this cache's own key —
    // `Arc<str>::as_ptr`, the STRING DATA pointer, `ArcInner + 16` past the two refcounts.
    // `object_class_word` is the `Arc`'s FIRST word, `ArcInner + 0`, which is what the emitted
    // guard loads because it is one dereference off the object. Feeding the profile the first
    // while the guard compares the second is why a `guard_ref_word` devirtualisation could never
    // hit — see `tlcache::ref_inline_put`. Both are captured here, from the same probe.
    let ref_cptr: Option<(usize, u64)> = match &receiver {
        Value::Ref(h) if gates & g::RECV_GUARD == 0 && gates & g::IVIRT4 == 0 => {
            let p = vm.heap.class_name_ptr(*h);
            let w = vm.heap.object_class_word(*h).unwrap_or(0) as u64;
            (p != 0 && w != 0).then_some((p, w))
        }
        _ => None,
    };
    // Reuse the pooled argument buffer as the callee locals by prepending the
    // receiver, instead of allocating a fresh Vec per virtual call (this Vec is
    // recycled back to the pool by `invoke`).
    let mut locals = args;
    // As the two fast paths: the census pair is read only when armed.
    let (cb, ln) = if gates & g::VECGROW != 0 {
        (locals.capacity(), locals.len())
    } else {
        (0, 0)
    };
    locals.insert(0, receiver);
    if gates & g::VECGROW != 0 {
        crate::vecgrow::note(
            crate::vecgrow::Site::InsertRecvResolved,
            cb,
            locals.capacity(),
            ln,
        );
    }
    // Own the buffer through the dispatch below so that the NATIVE arm — which
    // only borrows it and used to let it fall out of scope — puts it back in the
    // pool. The bytecode arm hands ownership on with `into_inner`.
    let mut locals = PooledVals::new(locals);

    // Monomorphic inline cache: skip resolve_method when this call site sees the
    // same receiver class as last time. The guava-HashBiMap force-native decision
    // (which mis-executes as bytecode here — an equal-but-distinct String key
    // misses despite correct hashCode/equals) is baked into the cached value, so
    // a hit needs neither a fresh resolve nor the per-call check.
    let site = (std::sync::Arc::as_ptr(&frame.class) as usize, index);
    let resolved = match crate::tlcache::inline_get(site, &actual) {
        Some(m) => m,
        None => {
            let r = vm.resolve_method(&actual, &name, &desc)?;
            // Route native if the receiver's class forces it OR the resolved method
            // is INHERITED from a fastutil object map (subclass instance — its
            // bytecode would run over the never-built internal arrays).
            let native = force_native_collection(vm, &actual, &name, &desc)
                || r.as_ref().is_some_and(|d| route_native_map(d));
            let r = if native { None } else { r };
            crate::tlcache::inline_put(site, &actual, r.clone());
            r
        }
    };
    // resolve_method above can class-load → run a <clinit> → move the arena. push_call_roots
    // kept the receiver ALIVE (its clone in gc_protect[gc_base] is forwarded) but locals[0] is
    // the stale pre-move copy — re-read the forwarded pointer so the callee's `this` is live.
    // THE worldgen strand: a fresh DensityFunction popped for HolderHolder.minValue()'s
    // invokeinterface went stale in the first-time resolve's <clinit>, so the callee derefed a
    // reset+reused slot → 'class None' UnsatisfiedLinkError. (Clone-not-forwarded, the interp
    // twin of the record/JIT fixes; primitive args need no rooting so only the ref receiver.)
    #[cfg(feature = "next-gen-gc")]
    if matches!(locals.first(), Some(Value::Obj(_) | Value::Ref(_) | Value::Array(_))) {
        if let Some(fresh) = crate::vm::thread_context::root_get(&vm.gc_protect, gc_base) {
            locals[0] = fresh;
        }
    }
    // M3 F3.2: divert a forkable `Collection.forEach(consumer)` (offloadable element method) to
    // the fork path, skipping the normal (guava/JDK) bytecode. The consumer at locals[1] is a
    // ref rooted right after the receiver (gc_base+1); re-read its forwarded copy. If the
    // consumer is NOT offloadable, `try_fork_each_invoke` returns None → the normal bytecode
    // runs UNTOUCHED (exact semantics preserved for datapacks/registries). Gated → default off.
    #[cfg(feature = "next-gen-gc")]
    if gmask & gm::FOR_EACH != 0 && locals.len() == 2 && fork_route_foreach() {
        if matches!(locals.get(1), Some(Value::Ref(_) | Value::Obj(_) | Value::Array(_))) {
            if let Some(fresh) = crate::vm::thread_context::root_get(&vm.gc_protect, gc_base + 1) {
                locals[1] = fresh;
            }
        }
        if vm_mut(vm).try_fork_each_invoke(&locals[0], &locals[1])?.is_some() {
            return Ok(());
        }
    }
    match resolved {
        Some(decl) => {
            // Seed the class-id fast cache: this Obj receiver reached plain bytecode
            // dispatch, so future calls at this (site, class) skip the gauntlet.
            // The index is computed HERE, on the cold resolve, so a hit never looks the method
            // up by name again. `None` means the class INHERITS it — then the cache is not
            // seeded and the by-name path, which walks the hierarchy, keeps serving.
            #[cfg(feature = "next-gen-gc")]
            let hit_midx = crate::tlcache::method_index(&decl, &name, &desc);
            #[cfg(feature = "next-gen-gc")]
            if let (Some(cid), Some(mi)) = (obj_cid, hit_midx) {
                if gates & g::RECV_GUARD == 0 {
                    crate::tlcache::obj_inline_put(site, cid, (decl.clone(), mi));
                }
            }
            // Megamorphic-Ref cache populate: this Ref receiver reached plain bytecode
            // dispatch (no gauntlet early-return, not native-routed), so future calls
            // at this (site, class) take the fast path above instead of re-resolving.
            #[cfg(feature = "next-gen-gc")]
            if let (Some((cptr, cword)), Some(mi)) = (ref_cptr, hit_midx) {
                crate::tlcache::ref_inline_put(site, cptr, cword, (decl.clone(), mi));
            }
            // The deferred form of the block above (default). `rc.ptr` IS `class_name_ptr`'s
            // answer for this handle — same expression, `o.class.as_ptr()`, taken from the one
            // probe the tail already spent — so only the `Arc`'s first word still needs the
            // heap. `resolve_method` may have class-loaded and collected in between; neither
            // moves the answer: the receiver is rooted, its class is immutable, and the
            // class-name interner does not evict.
            #[cfg(feature = "next-gen-gc")]
            if let (Some(h), Some(mi)) = (ref_recv, hit_midx) {
                let cptr = if rc.ptr != 0 { rc.ptr } else { vm.heap.class_name_ptr(h) };
                let cword = vm.heap.object_class_word(h).unwrap_or(0) as u64;
                debug_assert!(
                    rc.ptr == 0 || rc.ptr == vm.heap.class_name_ptr(h),
                    "hoisted receiver class pointer diverged from the heap's"
                );
                if cptr != 0 && cword != 0 {
                    crate::tlcache::ref_inline_put(site, cptr, cword, (decl.clone(), mi));
                }
            }
            if vm.trampoline {
                vm_mut(vm).pending = Some(crate::vmstate::PendingCall { decl: decl, name: site_ref.name.clone(), desc: site_ref.descriptor.clone(), args: locals.into_inner(), midx: None });
                return Ok(());
            }
            let ret = vm_mut(vm).invoke(decl, &name, &desc, locals.into_inner())?;
            // `env::var` allocated a `String` on every null-returning `getShape` — a process
            // constant, read once. The name test is the site's.
            if gmask & gm::GET_SHAPE != 0
                && matches!(ret, None | Some(Value::Null))
                && crate::env_flag!("RAVA_TRACE_SHAPE")
            {
                eprintln!("[NULLSHAPE] {actual}.{name}{desc} -> null\n{}", vm.backtrace());
            }
            if let Some(ret) = ret {
                frame.push(ret);
            }
            Ok(())
        }
        // Fall back to the JDK native layer. Virtual dispatch resolves on the
        // RECEIVER's actual class first (most specific), walking up its
        // superclass chain; only then the declared owner (covers interface
        // natives not in the class chain). Trying the declared owner first was
        // wrong for overridden Object methods: `((Object)anInteger).toString()`
        // has owner java/lang/Object, whose toString native is the
        // "Class@hash" placeholder — it must lose to Integer.toString.
        None => {
            if gates & g::VECGROW != 0 {
                crate::vecgrow::event(2);
            }
            // AbstractList.add(E): we have no bytecode for java.util.AbstractList,
            // so a custom List subclass's INHERITED add(E) would route to the
            // native collection layer ($elems) — diverging from the subclass's own
            // bytecode, which keeps elements in an inner backing list (e.g.
            // NonNullList delegates to `this.a`). That split made an appended item
            // invisible to size()/get() (→ ItemStack.isEmpty NPE on a player's
            // inventory). Emulate the real AbstractList.add(E) = add(size(), e),
            // dispatching to the subclass's OWN add(int,E)/size() bytecode so every
            // op shares one backing. Only when add(int,E) is actually declared as
            // bytecode (a delegating List subclass); plain native collections
            // (ArrayList, HashTreeSet which declares add(E) itself) never get here.
            if gmask & gm::ADD_OBJ != 0 {
                // Per-(site, receiver class) memo of the resolve+has-code question — see
                // `ADD_PLAN`. `None` from `instanceof_class_key` means the receiver has no
                // stable class identity (a lambda handle); REFUSE to memo and let the
                // per-call resolve below serve it, rather than key on something that moves.
                // Still `g::ROUND7`, not this round's bit: `ADD_PLAN` is a `FastMap` memo, and
                // this tree's two losing rounds were both memos added in front of a path that
                // was already cheap. It stays opt-in with the block it shipped in.
                let plan_key = if gates & g::ROUND7 != 0 {
                    instanceof_class_key(&vm.heap, &locals[0]).map(|k| (site.0, site.1, k))
                } else {
                    None
                };
                let cached = plan_key.and_then(|k| ADD_PLAN.with(|t| t.borrow().get(&k).cloned()));
                let plan: AddPlan = match cached {
                    Some(hit) => hit,
                    None => {
                        let r = match vm.resolve_method(&actual, "add", "(ILjava/lang/Object;)V")? {
                            Some(d)
                                if d.file
                                    .method("add", "(ILjava/lang/Object;)V")
                                    .and_then(|m| m.code.as_ref())
                                    .is_some() =>
                            {
                                Some(d)
                            }
                            _ => None,
                        };
                        if let Some(k) = plan_key {
                            ADD_PLAN.with(|t| {
                                t.borrow_mut().insert(k, r.clone());
                            });
                        }
                        r
                    }
                };
                if let Some(adecl) = plan {
                    let recv = locals[0].clone();
                    let elem = locals.get(1).cloned().unwrap_or(Value::Null);
                    let size = match vm.resolve_method(&actual, "size", "()I")? {
                        Some(sdecl) => vm_mut(vm)
                            .invoke(sdecl, "size", "()I", vec![recv.clone()])?
                            .and_then(|v| v.as_int())
                            .unwrap_or(0),
                        None => 0,
                    };
                    vm_mut(vm).invoke(adecl, "add", "(ILjava/lang/Object;)V",
                        vec![recv, Value::Int(size), elem])?;
                    frame.push(Value::Int(1));
                    return Ok(());
                }
            }
            let chain = vm.super_chain(&actual);
            // ONE bridge prologue for the whole owner walk (see try_invoke_chain): the
            // per-owner re-entry re-ran the arena-string canonicalization — which COPIES
            // every string argument — and billed the profiler once per owner TRIED, so a
            // 3-deep chain reported one logical call three times at three owners.
            #[cfg(feature = "next-gen-gc")]
            let handled = {
                let extra = if chain.iter().any(|c| &**c == owner) { None } else { Some(owner) };
                crate::natives::try_invoke_chain(vm_mut(vm), &chain, extra, &owner, &name, &desc, &locals)?
            };
            #[cfg(not(feature = "next-gen-gc"))]
            let handled = {
                let mut handled = None;
                for cn in chain.iter() {
                    handled = crate::natives::try_invoke(vm_mut(vm), cn, &name, &desc, &locals)?;
                    if handled.is_some() { break; }
                }
                if handled.is_none() && !chain.iter().any(|c| &**c == owner) {
                    handled = crate::natives::try_invoke(vm_mut(vm), &owner, &name, &desc, &locals)?;
                }
                handled
            };
            match handled {
                Some(ret) => { if let Some(v) = ret { frame.push(v); } Ok(()) }
                None => {
                    let sig = format!("invokevirtual {actual}.{name}{desc}");
                    if collect_missing(&sig) {
                        if let Some(d) = default_for_return(&desc) { frame.push(d); }
                        Ok(())
                    } else {
                        // ⭐**OUTLINED, `#[cold] #[inline(never)]` — row 398's rule applied to the
                        // block I added in row 437.** This forensic dump runs at most once in a
                        // process, right before a fatal `UnsatisfiedLinkError`, and it is large:
                        // holder walks, a two-level cell dump with `format!` in loops, three
                        // frame/stack scans. Inlined, its locals are reserved by
                        // `invokevirtual`'s PROLOGUE, which EVERY virtual call executes — the
                        // function measured **1816 bytes of stack frame**, the largest
                        // unaddressed one in the engine and on the hottest path there is.
                        ule_forensics(vm, frame, &locals, &sig, &owner, &actual, &name, &desc);
                        let msg = format!("{sig} (from {})", frame.class.file.this_class);
                        Err(unlinked_native(vm, msg))
                    }
                }
            }
        }
    }
}


/// The fatal-path dump for an unresolvable virtual call — see the call site for why it is not
/// inline. Cold by construction: every path through it ends in a `UnsatisfiedLinkError`.
#[cfg(feature = "next-gen-gc")]
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn ule_forensics(
    vm: &Vm,
    frame: &Frame,
    locals: &[Value],
    sig: &str,
    owner: &str,
    _actual: &str,
    _name: &str,
    _desc: &str,
) {
                        // Fatal-path forensics (zero hot-path cost): a ULE whose `actual`
                        // class makes no sense for the call is the stale-receiver
                        // signature — dump the raw receiver so the corruption source
                        // (moved arena Obj vs reused handle) is identifiable post-mortem.
                        let recv_dbg = match locals.first() {
                            Some(Value::Obj(p)) => {
                                let addr = p.addr();
                                let hdr = unsafe { (addr as *const u64).read_unaligned() };
                                #[cfg(feature = "next-gen-gc")]
                                for s in vm.heap.find_arena_holders(addr as u64) {
                                    eprintln!("[ULE-HOLDER] {s}");
                                }
                                // Classify like the PHASE5 probe: young/tenured + was it
                                // forwarded (writeback miss) or never in the forward log
                                // (freed / never rooted)? Names the GC mechanism.
                                #[cfg(feature = "next-gen-gc")]
                                {
                                    let young = crate::nextgen::tlab_in_nursery(*p);
                                    let fwd = crate::nextgen::lookup_fwd(addr);
                                    let (ylo, yhi) = crate::nextgen::tlab_young_window();
                                    let in_window = addr >= ylo && addr < yhi;
                                    eprintln!("[ULE-CLASSIFY] addr={addr:#x} young={young} fwd={fwd:?} window=[{ylo:#x},{yhi:#x}) in_window={in_window}");
                                }
                                format!("Obj({addr:#x}) hdr={hdr:#018x}")
                            }
                            Some(v) => format!("{v:?}"),
                            None => "<none>".into(),
                        };
                        eprintln!("[ULE-JITMETHOD] {:?}", crate::jit::jit_method_cur());
                        // WHICH THREAD, and did guest locking ever actually contend? A ULE whose
                        // receiver is an object of the wrong class is either a stale pointer or a
                        // race, and these two lines separate those without a second run.
                        eprintln!(
                            "[ULE-THREAD] worker={} monitors: {}",
                            crate::gil::is_worker(),
                            crate::monitor::stat_line()
                        );
                        eprintln!(
                            "[ULE-VIRT] {sig} owner={owner} recv={recv_dbg} in {} pc~{}\n{}",
                            frame.class.file.this_class,
                            frame.pc,
                            vm.backtrace()
                        );
                        // The faulting frame's raw slots: if a LOCAL still holds the
                        // stale young address the frame-forwarding pass missed THIS
                        // frame; if the locals are forwarded (old-gen) while the popped
                        // receiver is young-stale, the operand-stack copy diverged.
                        for (i, v) in frame.locals().iter().enumerate() {
                            if let Value::Obj(p) = v {
                                eprintln!("[ULE-FRAME] local[{i}] = Obj({:#x})", p.addr());
                                // ⭐**THE OBJECT'S CELLS, DECODED.** A ULE whose receiver came out
                                // of a field read is unexplainable from the receiver alone: what
                                // matters is what the HOLDER holds. Print each cell of the
                                // faulting frame's object locals with the class of anything that
                                // looks like an arena pointer, so the corrupt link is visible
                                // instead of inferred. (`RAVA_TRACE_THROW` only.)
                                #[cfg(feature = "next-gen-gc")]
                                unsafe {
                                    let raw = p.as_ptr();
                                    let nf = crate::nextgen::obj_nfields(raw);
                                    for c in 0..nf.min(12) as usize {
                                        let w = crate::nextgen::cell(raw, c);
                                        let kind = if w != 0 && w & 7 == 0 {
                                            let cp = w as usize as *mut u8;
                                            crate::nextgen::class_name(crate::nextgen::obj_class(cp))
                                                .map(|n| n.to_string())
                                                .unwrap_or_else(|| "?".into())
                                        } else {
                                            String::new()
                                        };
                                        eprintln!("[ULE-CELL]   local[{i}].cell[{c}] = {w:#x} {kind}");
                                        // ONE MORE LEVEL. The corrupt link is rarely in the
                                        // object you have — it is in what that object points
                                        // at (here: a memoizing supplier's cached `value`).
                                        if !kind.is_empty() && kind != "?" && kind != "[arena-string]" {
                                            let cp = w as usize as *mut u8;
                                            let cnf = crate::nextgen::obj_nfields(cp);
                                            for d in 0..cnf.min(8) as usize {
                                                let dw = crate::nextgen::cell(cp, d);
                                                let dk = if dw != 0 && dw & 7 == 0 {
                                                    crate::nextgen::class_name(
                                                        crate::nextgen::obj_class(dw as usize as *mut u8),
                                                    )
                                                    .map(|n| n.to_string())
                                                    .unwrap_or_else(|| "?".into())
                                                } else {
                                                    String::new()
                                                };
                                                eprintln!("[ULE-CELL2]     {kind}.cell[{d}] = {dw:#x} {dk}");
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        for (i, v) in frame.stack().iter().enumerate() {
                            if let Value::Obj(p) = v {
                                eprintln!("[ULE-FRAME] stack[{i}] = Obj({:#x})", p.addr());
                            }
                        }
                        // The REGISTERED frame chain (interpreted frames carry a real pc;
                        // a jit_call synthetic dispatch frame is pc=0 with empty locals —
                        // so this shows exactly which compiled hop fed the stale value).
                        for &fp in crate::frames::all().iter().rev().take(12) {
                            let f = unsafe { &*(fp as *const Frame) };
                            eprintln!(
                                "[ULE-STACK] {} pc={} locals={} stack={}",
                                f.class.file.this_class,
                                f.pc,
                                f.locals().len(),
                                f.stack_len()
                            );
                        }
}

#[cfg(not(feature = "next-gen-gc"))]
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn ule_forensics(
    _vm: &Vm,
    _frame: &Frame,
    _locals: &[Value],
    _sig: &str,
    _owner: &str,
    _actual: &str,
    _name: &str,
    _desc: &str,
) {
}
