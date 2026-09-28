//! Deriving a map key from a guest value, with the salt and the memo around it.
//!
//! Re-exported from the parent module, so no call site outside it changed.


pub(crate) mod key_render;
pub(crate) use key_render::*;
use crate::map::*;

/// The arena value-object key memo, extracted from `crate::map` and re-encoded — the 1.52%
/// `LocalKey::with<RefCell<(u64, HashMap<u64, Box<str>>)>>` row of the phase-A profile. See the
/// module header for the was/is table and the byte sizes. `RAVA_NO_FAST_VALKEY=1` routes every
/// entry point back to the original, which is still there and untouched.
mod valkey;

/// Read one of the fixed `$`-fields by a CACHED id, behind `&Vm`.
///
/// `key_str` probed FIVE of them by NAME per reference (`$str`, `$name`, `$path`,
/// `$map`, `$elems`), and a by-name `get_field` hashes the field name under the
/// interner's RwLock on every probe. That is the cost `key_render.rs` took off the
/// key path and `immutable::kind_of` was just measured paying — 10.4 s of wall for
/// one such probe — and this one runs five times per key, on every map lookup and
/// (since collections derive their element key with this function) on every
/// collection scan.
///
/// The failure is NEVER memoised: `$name` and `$path` are interned late, and a
/// `None` frozen before the first such object would misclassify every object
/// created afterwards.
fn synth(vm: &Vm, h: u32, cell: &'static std::sync::OnceLock<u32>, name: &'static str) -> Value {
    let id = match cell.get() {
        Some(&id) => Some(id),
        None => vm.heap.field_id_opt(name).inspect(|&id| {
            let _ = cell.set(id);
        }),
    };
    match id {
        Some(id) => vm.heap.get_field_hint(h, id, 0).0,
        None => Value::Null,
    }
}

static F_STR: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
static F_NAME: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
static F_PATH: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
static F_MAP: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
static F_ELEMS: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

/// Derive a lookup key string from a value (stable for String/number/Class/enum).
pub(crate) fn key_str(vm: &Vm, v: &Value) -> String {
    let k = key_str_counted(vm, v);
    crate::contract_stat::ekey_built(k.len());
    k
}

#[inline]
fn key_str_counted(vm: &Vm, v: &Value) -> String {
    key_str_at(vm, v, 0)
}

/// Maximum nesting `key_str` descends before it stops looking at CONTENT and
/// keys by identity instead.
///
/// The recursion is over `$map` entries and `$elems`, which the GUEST controls:
/// `List<Object> l = new ArrayList<>(); l.add(l);` is legal Java, and so is any
/// cycle through two maps. Java's own `AbstractCollection.toString` blows its
/// stack on that and says so in the javadoc, but blowing the RUST stack takes the
/// whole VM down with no catchable error, and `key_str` is now on the path of
/// every `contains`/`indexOf`/`equals` over a collection — not just of a map
/// lookup. Beyond the bound the value keys by identity, which is a key: two
/// DIFFERENT deep structures get different keys, and the same object gets the
/// same one.
///
/// The same bound and the same reason as `map::hashing::HASH_DEPTH`.
const KEY_DEPTH: u32 = 8;

fn key_str_at(vm: &Vm, v: &Value, depth: u32) -> String {
    if depth >= KEY_DEPTH {
        return match v {
            Value::Ref(h) => format!("@{h}"),
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) => format!("@a{}", crate::nextgen::arena_identity_hash(*p)),
            other => other.display(),
        };
    }
    match v {
        // `tagged` = the same bytes, one exactly-sized allocation. A leaf string key is the most
        // frequent thing this function renders (registries, resource locations, JSON) and it is
        // also what every collection scan derives per ELEMENT.
        Value::Str(s) => key_tagged("s:", s.as_str()),
        Value::Int(n) => format!("i:{n}"),
        Value::Long(n) => format!("j:{n}"),
        Value::Null => "null".to_string(),
        Value::Ref(h) => {
            // A heap java.lang.String object ($str) must key IDENTICALLY to a
            // Value::Str — else a map populated with literal "constant" misses
            // when looked up with a codec-decoded (heap) String "constant"
            // (e.g. EasingType.SIMPLE_REGISTRY → "Unknown element id: constant").
            if let Value::Str(s) = synth(vm, *h, &F_STR, "$str") {
                return key_tagged("s:", s.as_str());
            }
            for (cell, f) in [(&F_NAME, "$name"), (&F_PATH, "$path")] {
                if let Value::Str(s) = synth(vm, *h, cell, f) {
                    return key_tagged("o:", s.as_str());
                }
            }
            // A Map used as a key (e.g. a property→value combo keying another
            // Map): key structurally on its entries so equal-by-content maps
            // collide. Sorted so iteration order doesn't matter.
            if let Value::Int(id) = synth(vm, *h, &F_MAP, "$map") {
                let mut parts: Vec<String> = vm.heap.map_entries(id as u32)
                    .iter().map(|(k, val)| format!("{}={}", key_str_at(vm, k, depth + 1), key_str_at(vm, val, depth + 1))).collect();
                parts.sort();
                return map_key_braced(&parts);
            }
            // A synthetic collection used as a key: structural on elements.
            if let Value::Array(a) = synth(vm, *h, &F_ELEMS, "$elems") {
                let parts: Vec<String> = vm.heap.array_values(a).unwrap_or_default()
                    .iter().map(|e| key_str_at(vm, e, depth + 1)).collect();
                return format!("c:[{}]", parts.join(","));
            }
            format!("@{h}") // identity fallback
        }
        // Arena object: no synthetic fields and toString needs &mut, so key by the
        // GC-STABLE identity hash (header-stamped) — never the raw address, which
        // moves on a minor GC. Value-typed arena keys take the toString path in
        // `key_str_mut` (called for the top-level lookup); this immutable variant
        // only feeds nested structural keys, where identity is the safe default.
        // B5 MAP-KEY BRIDGE: a PINNED MIRROR pointer keys EXACTLY as its handle —
        // the full Ref(h) logic ($str/$name/$path/$map/$elems/@h), so a map
        // populated under one reference form hits under the other. Inert until
        // pinned pointers flow (pinned_handle_of is None otherwise).
        // ⭐**ONE `Value::Obj` ARM, BECAUSE THE PINNED LOOKUP IS ONE FACT.** These were three
        // arms and the first one asked `pinned_handle_of(p.addr())` in its GUARD and then asked
        // it AGAIN in its body to unwrap the handle it had just proved was there — the textbook
        // "a value computed before the test that decides whether it is needed", except here the
        // test IS the value. `pinned_handle_of` is a pinned-range test plus a reverse-map probe.
        // Arm ORDER is unchanged — pinned mirror, then arena string, then identity — because a
        // merged arm executes the same three questions in the same sequence with the same early
        // returns; only the second reading of the first answer disappears.
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            // B5 MAP-KEY BRIDGE, as above.
            if let Some(h) = crate::nextgen::pinned_handle_of(p.addr()) {
                return key_str_at(vm, &Value::Ref(h), depth);
            }
            // ArenaString keys identically to a Value::Str (a literal + an equal concat
            // result collide).
            if crate::nextgen::is_arena_string(*p) {
                return key_tagged("s:", &String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p)));
            }
            format!("@a{}", crate::nextgen::arena_identity_hash(*p))
        }
        other => other.display(),
    }
}

/// GAP-P04: derive the equals-verified salted key for value-equality object `k`
/// in map `mid`. Mirrors real `HashMap` semantics: bucket by guest `hashCode()`,
/// membership by guest `equals()` against the LIVE stored entry key (query
/// receiver, stored argument — `getNode` order). Probes salts `0..next_salt`
/// through the map's own index (holes from removals are skipped; the counter is
/// monotonic so a hole can never alias); no equal hit mints the next salt.
/// Read-only lookups may mint counter values for absent keys — a monotonic
/// waste, never an alias. Returns:
/// - `Ok(Some(key))` — the salted key (also for fresh mints);
/// - `Ok(None)` — ineligible (no resolvable `hashCode`/`equals` bytecode, or a
///   non-Int result) — DETERMINISTIC per class, so the caller's toString path
///   stays consistent for every key of that class;
/// - `Err` — a guest `hashCode`/`equals` throw, PROPAGATED exactly as a real
///   `HashMap.get/put` would.
#[allow(dead_code)]
pub(crate) fn salted_value_key(vm: &mut Vm, mid: u32, k: &Value, class: &str) -> Result<Option<String>, VmError> {
    let Some(hcd) = vm.resolve_method(class, "hashCode", "()I")? else {
        return Ok(None);
    };
    let Some(eqd) = vm.resolve_method(class, "equals", "(Ljava/lang/Object;)Z")? else {
        return Ok(None);
    };
    // CONVERGE MEMO: a key that equals-verified against a live entry of THIS map
    // derives the same salted key until the next collection (immutable value
    // equality; an equal key refilling a removal hole cannot alias — only UNEQUAL
    // reuse could, and unequal keys never converge). Minted-but-uninserted keys
    // are NEVER memoized (a later equal insert may land on a different salt).
    #[cfg(feature = "next-gen-gc")]
    let memo_epoch = crate::nextgen::gc_epoch();
#[cfg(not(feature = "next-gen-gc"))]
    let memo_epoch = 0u64;
    let ident = salt_ident(k);
    if let Some(id) = ident {
        if let Some(hit) = saltkey_memo_get(id, mid, memo_epoch) {
            return Ok(Some(hit));
        }
    }
    // Root `k` across the guest calls (hashCode/equals may allocate → move arenas).
    let base = crate::vm::thread_context::root_len(&vm.gc_protect);
    crate::vm::thread_context::push_root(&mut vm.gc_protect, k.clone());
    let out = (|| {
        // MEMOIZE the value-object's hashCode per object: a key's hashCode is stable by
        // the Map contract (a mutating key is lost in a real HashMap too), and the
        // equals verification below still runs on every index hit, so a memo hit can
        // never alias unequal keys. Heap Refs cache in an int `$hashc` field; arena
        // objects use the GC-epoch-tagged address memo (same discipline as the
        // structural-key memo: cache only if no collection ran during the guest call).
        let memo_hc = match crate::vm::thread_context::root_get(&vm.gc_protect, base) {
            Some(Value::Ref(h)) => match vm.heap.get_field(h, "$hashc") {
                Value::Int(h) => Some(h),
                _ => None,
            },
            #[cfg(feature = "next-gen-gc")]
            Some(Value::Obj(p)) => hashc_memo_get(p, crate::nextgen::gc_epoch()),
            _ => None,
        };
        let hc = match memo_hc {
            Some(h) => h,
            None => {
                #[cfg(feature = "next-gen-gc")]
                let epoch = crate::nextgen::gc_epoch();
                let kk = crate::vm::thread_context::root_index(&vm.gc_protect, base);
                let h = match vm.invoke(hcd, "hashCode", "()I", vec![kk])? {
                    Some(Value::Int(h)) => h,
                    _ => return Ok(None),
                };
                match crate::vm::thread_context::root_get(&vm.gc_protect, base) {
                    Some(Value::Ref(rh)) => vm.heap.put_field(rh, "$hashc", Value::Int(h)),
                    #[cfg(feature = "next-gen-gc")]
                    Some(Value::Obj(p)) => {
                        if crate::nextgen::gc_epoch() == epoch {
                            hashc_memo_put(p, h, epoch);
                        }
                    }
                    _ => {}
                }
                h
            }
        };
        let next = vm.heap.map_salt_next(mid, class, hc);
        for salt in 0..next {
            let key = format!("o:gS{class}:{hc}#{salt}");
            let probe = crate::heap::MapKey::Str(key.as_str().into());
            let Some(slot) = vm.heap.map_index_lookup(mid, &probe) else {
                continue; // removal hole — its salt is never reused
            };
            let Some(cand) = vm.heap.map_entry_key(mid, slot) else {
                continue;
            };
            let kk = crate::vm::thread_context::root_index(&vm.gc_protect, base);
            let r = vm.invoke(std::sync::Arc::clone(&eqd), "equals", "(Ljava/lang/Object;)Z", vec![kk, cand])?;
            if matches!(r, Some(Value::Int(1))) {
                // Equals-CONFIRMED against a live entry → memoizable (see above);
                // only if no collection ran during the guest calls (arena addresses
                // must still be this epoch's).
                if salt_epoch_now() == memo_epoch {
                    if let Some(rooted) =
                        crate::vm::thread_context::root_get(&vm.gc_protect, base)
                    {
                        if let Some(id) = salt_ident(&rooted) {
                            saltkey_memo_put(id, mid, &key, memo_epoch);
                        }
                    }
                }
                return Ok(Some(key));
            }
        }
        // Mint WITHOUT storing: the counter advances only when a salted key is
        // actually INSERTED (heap::map_put parses the prefix). Two uninserted
        // mints may share a salt — harmless, nothing exists under it to alias,
        // and the inserting put re-derives against the then-live index.
        let salt = vm.heap.map_salt_next(mid, class, hc);
        Ok(Some(format!("o:gS{class}:{hc}#{salt}")))
    })();
    crate::vm::thread_context::truncate_roots(&mut vm.gc_protect, base);
    out
}

/// Structural key for use as a Map key. Unlike `key_str` (which falls back to
/// object identity), this drains a Map-implementing key — including a real
/// (jar) map such as fastutil's `Reference2ObjectArrayMap` — into a sorted,
/// content-based string so equal-by-content keys collide. Used by Minecraft's
/// block-state machinery, which keys a Map by a `Property→value` combo Map.
/// DIAGNOSTIC (RAVA_KEYSTAT): tally where map-key derivation spends calls, to
/// decide whether the block-bootstrap map-iteration hotspot is rava's keying
/// (fixable by hashCode/equals) or Minecraft's own bytecode. Prints every 2M.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn keystat(tag: &'static str) {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_KEYSTAT").is_some()) {
        return;
    }
    static TOTAL: AtomicU64 = AtomicU64::new(0);
    static ARRAYMAP: AtomicU64 = AtomicU64::new(0);
    static VALOBJ_TOSTR: AtomicU64 = AtomicU64::new(0);
    // The memo is checked AFTER the valobj_tostr tally, so that counter measures ATTEMPTS,
    // not real work — split out the memo HIT and the toString that actually ran, or the
    // number reads 100x worse than reality (the same trap the JIT-OFF weight profile set).
    static MEMO_HIT: AtomicU64 = AtomicU64::new(0);
    static TOSTR_REAL: AtomicU64 = AtomicU64::new(0);
    let c = match tag {
        "arraymap" => &ARRAYMAP,
        "valobj_tostr" => &VALOBJ_TOSTR,
        "memo_hit" => &MEMO_HIT,
        "tostr_real" => &TOSTR_REAL,
        _ => &TOTAL,
    };
    let n = c.fetch_add(1, Ordering::Relaxed) + 1;
    if std::ptr::eq(c, &TOTAL) && n % 500_000 == 0 {
        let att = VALOBJ_TOSTR.load(Ordering::Relaxed);
        let hit = MEMO_HIT.load(Ordering::Relaxed);
        let real = TOSTR_REAL.load(Ordering::Relaxed);
        eprintln!(
            "[KEYSTAT] structural_key total={} arraymap_drain={} valobj_attempts={} memo_hit={} REAL_toString={} ({}% of attempts)",
            n,
            ARRAYMAP.load(Ordering::Relaxed),
            att,
            hit,
            real,
            if att > 0 { real * 100 / att } else { 0 },
        );
    }
}

/// COUNTERFACTUAL PROBE (`RAVA_KEYSHADOW`, default OFF, inert): what would the key-render memo be
/// worth if it were not refused by class, and would it ever be WRONG?
///
/// ⭐**Why this and not a build.** `RAVA_KEYRENDER_STAT` reads **hit=2 miss=4 rejected=999,994**:
/// the memo is refused for essentially every render, so **3.8 M guest `toString`s run a boot at
/// 1,089 ns each — 6.4% of wall** (ledger row 308). The refusal is `class_all_final_word`, and it
/// is CORRECT in principle: a class with a non-final instance field can mutate, and then a cached
/// render is a stale key, which is a silently wrong map lookup rather than a crash.
///
/// Whether it is correct IN PRACTICE is a different question, and it is the one that decides
/// whether the 4.1 s is reachable. So: shadow-memo every render regardless of the class rule,
/// return the REAL render always, and on a shadow hit compare what the memo would have answered
/// against what was actually produced.
///
/// * `same` — the memo would have been right, and that render was pure waste.
/// * `differs` — the object mutated between two keyings, and the class rule earned its keep.
///
/// `differs = 0` over a boot does not PROVE soundness; it bounds the risk and sizes the prize,
/// which is what is needed before designing write-invalidation. `differs > 0` kills the idea
/// outright and costs one boot instead of a week.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod keyshadow {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub(crate) static SAME: AtomicU64 = AtomicU64::new(0);
    pub(crate) static DIFFERS: AtomicU64 = AtomicU64::new(0);
    pub(crate) static MISS: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_KEYSHADOW").is_some())
    }

    /// ✅**AUDITED, LEFT ALONE.** This IS the expensive shape — non-const initialiser, `RefCell`,
    /// an owned `String` per entry — but it is touched only inside `if keyshadow::enabled()`, a
    /// `RAVA_KEYSHADOW` counterfactual probe that is OFF by default and never runs in a product
    /// boot. A thread_local costs nothing until it is first accessed, so its touch frequency is
    /// zero and rewriting it would buy nothing while making the probe harder to read.
    thread_local! {
        pub(crate) static SHADOW: std::cell::RefCell<(u64, crate::FastMap<u32, String>)> =
            std::cell::RefCell::new((0, crate::FastMap::default()));
    }

    pub(crate) fn note(hit: Option<&str>, actual: &str) {
        let c = match hit {
            None => &MISS,
            Some(s) if s == actual => &SAME,
            Some(_) => &DIFFERS,
        };
        let n = c.fetch_add(1, Relaxed) + 1;
        let (a, b, m) = (SAME.load(Relaxed), DIFFERS.load(Relaxed), MISS.load(Relaxed));
        let tot = (a + b + m).max(1);
        if n % 250_000 == 0 {
            eprintln!(
                "[KEYSHADOW] same={a} differs={b} miss={m} | would-hit {:.1}% of all renders, wrong {} times",
                (a + b) as f64 * 100.0 / tot as f64,
                b
            );
        }
    }
}

/// ⭐**THE MAP-KEY FAST PATH.** **DEFAULT ON**, kill switch `RAVA_NO_FAST_MAPKEY=1`, and it also
/// yields to `flags::emit_more_disabled()` so the whole rewrite block prices as one A/B.
///
/// What it turns off, all of it byte-identical to what stays:
///  * `NamedProbe` — the `$str`/`$name`/`$path` triple is read ONCE per derivation instead of
///    twice (`structural_key_scoped_inner::has_named`, then again in `key_str_mut_in`). A `$`
///    field read is `Heap::get_field_by_id`, which LINEARLY SCANS the object's id array (its own
///    comment: "a JVM reads a field at a constant offset; this walks the object's id array"), so
///    three duplicated probes on a 32-field `BlockState` is ~96 wasted comparisons per key.
///  * `value_equality_by_word` — the handle twin of `value_equality_by_cid`. The arena arm was
///    given a class-id memo ("no name allocation, no name hash"); the handle arm still built a
///    ~50-character class-name `String` and hashed all of it to fetch a `bool`.
///  * allocation-free `s:`/`o:` leaves in the combo fold and exact-capacity key building.
pub(crate) fn fast_mapkey() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        crate::flags::fast_natives()
            && std::env::var_os("RAVA_NO_FAST_MAPKEY").is_none()
            && !crate::flags::emit_more_disabled()
    })
}

/// ⭐**THE THREAD-LOCAL SHAPE SWEEP FOR THE MAP-KEY MEMOS.** **DEFAULT ON**, kill switch
/// `RAVA_NO_FAST_MAPKEYTLS=1`, and it also yields to `flags::emit_more_disabled()` so the whole
/// block prices as one A/B.
///
/// A Rust `thread_local!` lowers to a bare TLS-slot load only when it is BOTH const-initialised
/// AND destructor-free. Anything else is an out-of-line `LocalKey::with` carrying a
/// lazy-initialisation state check; a `RefCell` on top adds a borrow-flag load, a compare, a +1
/// store on entry and a release store when the guard drops. Every memo in this file was the
/// second shape. What this switch turns off, all of it byte-identical to what stays:
///
///  * the five per-class VERDICT tables (`value_equality_by_word`, `identity_verdict_cid`,
///    `identity_verdict_word`, `render_is_identity`, `arena_class_all_final`) move from
///    `RefCell<FastMap<K, bool>>` with a non-const initialiser to a `const {}`-initialised
///    `UnsafeCell<FastMap<K, bool>>` — see [`verdict_memo`]. **Same map, same key type, same
///    population, so the SIZE is unchanged.**
///  * `ARENA_KEYRENDER_MEMO` moves onto `valkey`'s span table: no `Box<str>` malloc per put, no
///    borrow flag, and a table that SHRINKS from 25 to 17 bytes per slot. See `valkey::render_get`.
///  * `arena_funnel_get` probes the memo BEFORE the admission check instead of after, so a HIT
///    opens one `LocalKey::with` rather than two. Same answers, same census counts — a
///    non-admissible class is never in the memo, so it still reaches the check and still tallies.
pub(crate) fn fast_mapkeytls() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_MAPKEYTLS").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Declare, inside the calling function, a per-thread class-verdict table in the CHEAP
/// thread-local shape plus the legacy `RefCell` table as the kill-switch arm, and the two
/// accessors `verdict_get` / `verdict_put` that pick between them.
///
/// SHAPE BEFORE (what all five of these were, verbatim):
/// `thread_local! { static SEEN: RefCell<FastMap<K, bool>> = RefCell::new(FastMap::default()); }`
/// — a NON-const initialiser, so every touch is an out-of-line `LocalKey::with` carrying a
/// lazy-initialisation state check and the initialiser closure, and the `RefCell` adds four more
/// memory operations to read one `bool` that is already sitting in a hash table.
///
/// SHAPE AFTER: `const {}` initialiser (the lazy-init branch and the closure disappear) over an
/// `UnsafeCell` (the borrow flag disappears). `HashMap` still owns a destructor, so what remains
/// is the destructor-registration state byte — and that one may NOT go: these tables are the
/// authority for a whole class population, and dropping the map at thread exit is what stops a
/// worker thread's table leaking for the life of the process.
///
/// ⚠**SIZE: UNCHANGED.** Same `FastMap<K, bool>`, same key, same population — one entry per
/// distinct class id / class word ever used as a map key, a few hundred in a product boot at
/// ~16 B/entry, i.e. low single-digit KB. Nothing here adds a slot, a mirror or a second table on
/// the hot path; the tree has measured what growing one of these costs (+6.8% for 2.5 KB → 14 KB).
///
/// SAFETY: the `&mut` never escapes the accessor, and no caller holds one across a computation.
/// Every miss RELEASES the table, computes — which genuinely does re-enter this module, since
/// `render_is_identity`'s miss renders an object and a render folds that object's map entries back
/// through `identity_verdict_*` — and only then calls `verdict_put`. That is the identical
/// discipline the `RefCell` version already had to keep, or it would have panicked on the
/// re-entrant borrow instead of merely being slow.
macro_rules! verdict_memo {
    ($k:ty) => {
        std::thread_local! {
            /// const-init, no borrow flag. Serves whenever `fast_mapkeytls()`.
            static FAST: std::cell::UnsafeCell<crate::FastMap<$k, bool>> = const {
                std::cell::UnsafeCell::new(std::collections::HashMap::with_hasher(
                    std::hash::BuildHasherDefault::new(),
                ))
            };
            /// The kill-switch arm: the original shape, untouched, never written while the other
            /// serves. A mix could only ever MISS, and a miss is a recompute of the same verdict.
            static LEGACY: std::cell::RefCell<crate::FastMap<$k, bool>> =
                std::cell::RefCell::new(crate::FastMap::default());
        }
        #[inline]
        fn verdict_get(k: $k) -> Option<bool> {
            if self::fast_mapkeytls() {
                // SAFETY: see the macro's doc block — the reference never escapes this closure and
                // nothing re-entrant runs while it is live.
                FAST.with(|c| unsafe { (*c.get()).get(&k).copied() })
            } else {
                LEGACY.with(|c| c.borrow().get(&k).copied())
            }
        }
        #[inline]
        fn verdict_put(k: $k, v: bool) {
            if self::fast_mapkeytls() {
                // SAFETY: as above; `insert` may grow the table, which cannot re-enter here.
                FAST.with(|c| unsafe {
                    (*c.get()).insert(k, v);
                });
            } else {
                LEGACY.with(|c| {
                    c.borrow_mut().insert(k, v);
                });
            }
        }
    };
}

/// The `$str`/`$name`/`$path` answer, carried from the funnel head down to the renderer.
///
/// ⚠**IT IS A CARRIED ANSWER, NOT A REORDERING.** `key_str_mut_in` consults this at exactly the
/// point it used to run the three probes itself — after the record renderer and after the pinned
/// mirror bridge — so the ORDER in which the arms can answer is unchanged, and `$str` still wins
/// over `$name` which still wins over `$path`. Only the second reading of the same three heap
/// fields disappears.
pub(crate) enum NamedProbe {
    /// Nobody has looked yet: `key_str_mut_in` probes, as it always did.
    Unprobed,
    /// Probed and ABSENT — skip the three linear field scans.
    Absent,
    /// Probed and present; these are the exact bytes the probe would have built.
    Key(String),
}

/// `has_value_equality` for a HANDLE object, memoised on the heap's class word.
///
/// ⭐**The arena arm already had this and the handle arm did not.** `value_equality_by_cid`'s own
/// comment prices the shape it removed: a fresh heap `String` for a ~50-character class name and
/// then a memo probe that hashes those fifty characters, "paid by every key and used by one in a
/// hundred and seventy". `key_str_mut_in`'s `Value::Ref` arm still ran exactly that —
/// `class_of(h).to_string()` feeding `has_value_equality` — on every handle key that reached the
/// non-structural branch, and `RAVA_KEYRENDER_STAT` counts 749,996 of those in a boot (the
/// handle render memo rejects almost all of them, so almost none are served from a cache).
///
/// Same key as `identity_verdict_word`, so the two verdicts cannot drift on what "this class"
/// means. The class name is materialised at most once per class word, on the miss.
///
/// TABLE SIZE: `FastMap<i64, bool>`, one entry per distinct class word ever used as a map key —
/// a few hundred in a product boot, 16 B/entry ⇒ **~4-8 KB**. It is a process-lifetime verdict
/// memo behind a thread-local, not a per-lookup L1 cache: the thing that had to shrink was the
/// per-probe name hash, not the footprint.
pub(crate) fn value_equality_by_word(vm: &mut Vm, word: i64, h: u32) -> bool {
    verdict_memo!(i64);
    if let Some(v) = verdict_get(word) {
        return v;
    }
    // The table is released before this runs: `equals_kind` resolves `equals` up the super chain
    // and re-enters this module. (Under the `RefCell` shape that release was mandatory or the
    // nested borrow panicked; it is kept exactly as it was.)
    let name = vm.heap.class_of(h).unwrap_or("java/lang/Object").to_string();
    let verdict = has_value_equality(vm, &name);
    verdict_put(word, verdict);
    verdict
}

/// The class name `key_str_mut_in`'s handle arm used to build for EVERY key. It is needed only by
/// the salted derivation, the `RAVA_MAPKEY_HASHCODE` scaffold and the render census — all three
/// off by default — so it is built inside those arms instead of ahead of them.
pub(crate) fn key_class_name(vm: &Vm, h: u32) -> String {
    vm.heap.class_of(h).unwrap_or("java/lang/Object").to_string()
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn key_str_memoed(
    vm: &mut Vm,
    v: &Value,
    salt_mid: Option<u32>,
    named: NamedProbe,
) -> Result<String, VmError> {
    // ⭐**ARENA OBJECTS GET THE MEMO TOO** (`RAVA_ARENA_KEYMEMO=1`).
    //
    // The line below used to send every non-`Ref` straight to the renderer, and `RAVA_KEYRENDER_STAT`
    // showed what that cost: **hit=0, miss=4, rejected=749,996** — the memo was dead, because the
    // dominant key class is an ARENA object. `RAVA_STRUCTKEY_SCAN` names it:
    // `BlockState`, **32 fields**, 147,027 map-key uses; `RAVA_KEYTIME` prices the branch it falls
    // into at 912,225 derivations x 5,004 ns = **4.565 s of a 47 s boot**.
    //
    // Unlike keying such an object by IDENTITY (tried, `RAVA_IDENTITY_KEY_BY_CLASS`, and it kills
    // the boot because the engine derives map keys through four entry points that then disagree),
    // a memo returns **the same string the uncached path would**. It cannot disagree with anything;
    // it can only compute the answer once. The admission rule is the existing one — every instance
    // field final — and the invalidation tag is the existing one.
    if arena_keymemo_enabled() && salt_mid.is_none() && !keyrender_memo_disabled() {
        if let Value::Obj(p) = v {
            if let Some(s) = arena_key_memoed(vm, *p, v)? {
                return Ok(s);
            }
        }
    }
    // A salted render is scoped to one backing map, so it must never be shared.
    let Value::Ref(h) = v else { return key_str_mut_named(vm, v, salt_mid, named) };
    if salt_mid.is_some() || keyrender_memo_disabled() {
        return key_str_mut_named(vm, v, salt_mid, named);
    }
    let h = *h;
    let tag = crate::nextgen::gc_epoch() ^ (vm.runtime_shared().static_gen() as u64).rotate_left(32);
    let hit = KEYRENDER_MEMO.with(|c| {
        let m = c.borrow();
        if m.0 == tag { m.1.get(&h).map(|s| s.to_string()) } else { None }
    });
    if let Some(s) = hit {
        keyrender_stat(0);
        return Ok(s);
    }
    if !class_all_final_word(vm, h) {
        keyrender_stat(2);
        crate::map::keyroot::keyclass_census(vm, h);
        crate::map::keyroot::tostr_fields_census(vm, h);
        if keyshadow::enabled() {
            // Probe FIRST, render SECOND, compare, and return the real render either way. The
            // shadow table is never read by anything that decides behaviour.
            let prior = keyshadow::SHADOW.with(|c| {
                let m = c.borrow();
                if m.0 == tag { m.1.get(&h).cloned() } else { None }
            });
            let actual = key_str_mut_named(vm, v, salt_mid, named)?;
            keyshadow::note(prior.as_deref(), &actual);
            keyshadow::SHADOW.with(|c| {
                let mut m = c.borrow_mut();
                if m.0 != tag {
                    m.0 = tag;
                    m.1.clear();
                }
                if m.1.len() >= KEYRENDER_MAX {
                    m.1.clear();
                }
                m.1.insert(h, actual.clone());
            });
            return Ok(actual);
        }
        return key_str_mut_named(vm, v, salt_mid, named);
    }
    let rendered = key_str_mut_named(vm, v, salt_mid, named)?;
    keyrender_stat(1);
    KEYRENDER_MEMO.with(|c| {
        let mut m = c.borrow_mut();
        if m.0 != tag {
            m.0 = tag;
            m.1.clear();
        }
        // BOUNDED, like `stringify_memo`'s own cache. The key population is small in DISTINCT
        // objects (six `Direction`s), but `BlockState` has ~20k and nothing stops a workload from
        // keying far more, so an unbounded per-thread map is a slow leak rather than a cache.
        // Clearing wholesale on overflow keeps the common case allocation-free; an eviction policy
        // would cost more than the render it protects.
        if m.1.len() >= KEYRENDER_MAX {
            m.1.clear();
        }
        m.1.insert(h, rendered.as_str().into());
    });
    Ok(rendered)
}

#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn key_str_memoed(
    vm: &mut Vm,
    v: &Value,
    salt_mid: Option<u32>,
    named: NamedProbe,
) -> Result<String, VmError> {
    key_str_mut_named(vm, v, salt_mid, named)
}

/// ⭐**THE SAME FOLD, WITHOUT MATERIALISING THE MAP.** `combo_hash_key` reached the fold through
/// `drain_arena_array_map`, which allocates THREE `Vec`s per call — a clone of the whole key
/// array, a clone of the whole value array, and a vector of cloned pairs — purely so a hash can
/// be computed over them. `combo_hash_key` calls it **570,894 times a boot** (490,222 folds plus
/// 80,672 refusals), so that is ~1.7 M vector allocations and every `Value` in every property map
/// cloned, to produce a `u64`.
///
/// This reads the two arrays element by element and folds in place. Byte-for-byte the same hash:
/// the per-entry FNV, the `=` separator, the `splitmix64` per entry, the order-independent
/// `wrapping_add` and the final length mix are the fold below, unchanged.
///
/// It is the same defect, and the same fix, as `bitset_words` materialising a whole `Vec<u64>` to
/// read two words (1,780 -> 1,085 ns).
#[cfg(feature = "next-gen-gc")]
pub(crate) fn combo_hash_arena_array_map(vm: &mut Vm, p: crate::arena::ArenaPtr) -> Option<u64> {
    let cid = crate::nextgen::arena_class_id(p);
    let (kid, vid, sid) = crate::map::arraymap_ids(vm, cid)?;
    let (Value::Array(ka), Value::Array(va)) = (
        crate::opcodes::arena_getfield(p, kid),
        crate::opcodes::arena_getfield(p, vid),
    ) else {
        return None;
    };
    let kn = vm.heap.array_len(ka).unwrap_or(0);
    let vn = vm.heap.array_len(va).unwrap_or(0);
    let size = match crate::opcodes::arena_getfield(p, sid) {
        Value::Int(s) => Some(s),
        _ => None,
    };
    let n = arraymap_entry_count(size, kn, vn);
    let mut acc: u64 = 0;
    let mut est_bytes: usize = 0;
    for i in 0..n {
        let k = vm.heap.array_get(ka, i).unwrap_or(Value::Null);
        let val = vm.heap.array_get(va, i).unwrap_or(Value::Null);
        // Preserve the packed map's key-then-value observation order while keeping both
        // values forwardable across either nested fold.
        let base = crate::vm::thread_context::root_len(&vm.gc_protect);
        let _scope = crate::stream::GpScope(&mut vm.gc_protect as *mut _, base);
        crate::vm::thread_context::push_root(&mut vm.gc_protect, k);
        crate::vm::thread_context::push_root(&mut vm.gc_protect, val);
        let mut ph: u64 = 0xcbf2_9ce4_8422_2325;
        let rooted_key = crate::vm::thread_context::root_index(&vm.gc_protect, base);
        let kl = combo_entry_bytes(vm, &rooted_key, &mut ph);
        ph = combo_fnv(b"=", ph);
        let rooted_val = crate::vm::thread_context::root_index(&vm.gc_protect, base + 1);
        let vl = combo_entry_bytes(vm, &rooted_val, &mut ph);
        est_bytes += kl + vl + 2;
        acc = acc.wrapping_add(splitmix64(ph));
    }
    keystat_bytes(est_bytes);
    Some(acc ^ splitmix64(n as u64 + 1))
}

/// Preserve the packed ArrayMap bounds rule in one pure helper: a valid size is clamped to both
/// backing arrays, while a missing/non-integer size uses the common readable prefix.
#[cfg(feature = "next-gen-gc")]
#[inline]
pub(crate) fn arraymap_entry_count(size: Option<i32>, key_len: usize, value_len: usize) -> usize {
    match size {
        Some(size) => (size.max(0) as usize).min(key_len).min(value_len),
        None => key_len.min(value_len),
    }
}

/// A stack buffer wide enough for `-9223372036854775808` plus a one-byte prefix.
#[inline]
fn itoa_buf() -> [u8; 24] {
    [0u8; 24]
}

/// Write `@{h}` (handle) or `@a{hash}` (arena) into `buf` — the bytes `key_str_at`'s identity
/// case produces, without the allocation.
#[inline]
fn fmt_at(buf: &mut [u8; 24], arena: bool, n: i64) -> &[u8] {
    let head: &[u8] = if arena { b"@a" } else { b"@" };
    let mut i = 24usize;
    let mut m = if n < 0 { n } else { -n };
    loop {
        i -= 1;
        buf[i] = b'0' + ((-(m % 10)) as u8);
        m /= 10;
        if m == 0 {
            break;
        }
    }
    if n < 0 {
        i -= 1;
        buf[i] = b'-';
    }
    let len = 24 - i;
    buf.copy_within(i..24, head.len());
    buf[..head.len()].copy_from_slice(head);
    &buf[..head.len() + len]
}

/// Write `<prefix><n>` into `buf` and return the written slice — the bytes `format!("i:{n}")`
/// and `format!("j:{n}")` would produce, without the allocation. The `:` is part of the prefix.
#[inline]
fn fmt_prefixed(buf: &mut [u8; 24], tag: u8, n: i64) -> &[u8] {
    buf[0] = tag;
    buf[1] = b':';
    let mut i = 24usize;
    let neg = n < 0;
    // `-n` overflows for i64::MIN, so accumulate on the negative side.
    let mut m = if neg { n } else { -n };
    loop {
        i -= 1;
        buf[i] = b'0' + ((-(m % 10)) as u8);
        m /= 10;
        if m == 0 {
            break;
        }
    }
    if neg {
        i -= 1;
        buf[i] = b'-';
    }
    // Move the digits up against the prefix.
    let len = 24 - i;
    buf.copy_within(i..24, 2);
    &buf[..2 + len]
}

#[inline]
fn combo_fnv(bytes: &[u8], mut h: u64) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// A primitive entry's key bytes, folded into `ph` from a STACK buffer — the same bytes
/// `structural_key` would return, without the `String` it would allocate to return them.
///
/// ⭐**SHARED BY BOTH FOLDS, WHICH IS THE POINT.** These arms lived inside `combo_entry_bytes`
/// while `combo_hash_of` — the fold for rava's own arena maps — kept calling `structural_key`
/// and allocating a fresh `String` for `i:5`. Two functions that MUST produce identical bytes
/// (a map populated through one representation is probed through the other; a differing byte is
/// a differing `MapKey::Hash`, i.e. a silent miss) had already drifted apart once. One writer,
/// one encoding, and the allocation disappears from the second fold as a side effect.
///
/// `None` for anything that is not a leaf primitive — the caller keeps its own fallbacks.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn combo_prim_bytes(v: &Value, ph: &mut u64) -> Option<usize> {
    match v {
        Value::Int(n) => {
            let mut buf = itoa_buf();
            let bytes = fmt_prefixed(&mut buf, b'i', *n as i64);
            *ph = combo_fnv(bytes, *ph);
            Some(bytes.len())
        }
        Value::Long(n) => {
            let mut buf = itoa_buf();
            let bytes = fmt_prefixed(&mut buf, b'j', *n);
            *ph = combo_fnv(bytes, *ph);
            Some(bytes.len())
        }
        Value::Null => {
            *ph = combo_fnv(b"null", *ph);
            Some(4)
        }
        _ => None,
    }
}

/// One entry's key bytes, folded into `ph`. The memo first, then the alloc-free primitive
/// encodings, then the identity rules, then the full structural render as the fallback — the
/// same bytes whichever arm answers.
#[cfg(feature = "next-gen-gc")]
fn combo_entry_bytes(vm: &mut Vm, v: &Value, ph: &mut u64) -> usize {
    if let Value::Obj(p) = v {
        if !keymemo_disabled() {
            // ⭐**THE CLOSURE WAS THE COST, NOT THE LOOKUP.** This used to be
            // `valkey_memo_with(*p, epoch, |s| ..fold..)`: a caller-supplied `FnOnce` running while
            // the memo's `RefCell` was borrowed, which is exactly the re-entrancy that borrow flag
            // existed to catch. `valkey::fold` is the same operation made CONCRETE — so the flag,
            // the lazy-TLS-init branch and the `Box<str>` indirection all went with it, and the
            // FNV loop that was written out twice (here and in `combo_hash_of`) now has one writer.
            // Bit-exact: same stored bytes, same seed, same `combo_fnv`.
            // Read the epoch at the query itself. Callers may have just completed a nested
            // render that collected and forwarded `p`, so an epoch carried by the caller could
            // address the old memo generation with the new pointer (or the reverse).
            if let Some(len) = valkey::fold(*p, crate::nextgen::gc_epoch(), ph) {
                crate::diag::combo_entry_hit().inc();
                return len;
            }
        }
    }
    // ⭐**HALF OF EVERY BLOCK-STATE PROPERTY MAP MISSED THE MEMO, AND THE MISS BUILT A STRING.**
    // `RAVA_DIAG=mapkey` on a product boot: **hit 2,219,494, miss 2,219,738 — exactly 50.0%.**
    // The memo only serves arena `Value::Obj`, and a property map's entries are (Property, value):
    // the KEYS are arena objects and hit, the VALUES are primitives and miss. Every one of those
    // 2.2 M misses called `structural_key`, which returns a fresh `String` — for `i:5`.
    //
    // These arms write the SAME BYTES `structural_key` would, into a stack buffer, and FNV them.
    // Bit-exact by construction: the encodings below are copied from the leaf match in
    // `structural_key` and must not drift from it — a different byte here is a different combo
    // hash, which is a different map key.
    if let Some(len) = combo_prim_bytes(v, ph) {
        crate::diag::combo_entry_prim().inc();
        return len;
    }
    // ⭐**AN ENTRY WHOSE CLASS COMPARES BY REFERENCE IS KEYED BY REFERENCE.**
    //
    // `RAVA_DIAG=mapkey` named the remaining misses: **1,168,118 arena objects the memo refused
    // and 879,136 handle objects**, each paying a full structural render inside a combo fold.
    // The memo refuses an arena object whose class has any non-final instance field, which is a
    // conservative rule about whether a RENDER can go stale — but for a class whose `equals` IS
    // reference identity, the render is the wrong question entirely: a JVM's `HashMap` keys such
    // an object by identity, so two distinct instances are distinct keys and the same instance is
    // the same key no matter what its fields say.
    //
    // `identity_verdict_cid` / `identity_verdict_word` are the existing, memoised predicates for
    // exactly that, and they decide from the class's `equals` BODY (plus `ACC_ENUM`) — the same
    // evidence `map_key_inner` already uses to emit `MapKey::Id`. The bytes written here are the
    // ones `key_str_at` writes for its own identity case, so the encoding is not new either.
    match v {
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) if idkey_by_class_enabled() => {
            let cid = crate::nextgen::arena_class_id(*p);
            if identity_verdict_cid(vm, cid) {
                let mut buf = itoa_buf();
                let bytes = fmt_at(&mut buf, true, crate::nextgen::arena_identity_hash(*p) as i64);
                *ph = combo_fnv(bytes, *ph);
                crate::diag::combo_entry_ident().inc();
                return bytes.len();
            }
        }
        Value::Ref(h) if idkey_by_class_enabled() => {
            if let Some(word) = vm.heap.object_class_word(*h) {
                // Either the class MAY be keyed by identity, or its full render already IS the
                // identity token — the second is weaker, checkable, and covers `java/lang/Boolean`.
                if identity_verdict_word(vm, word, *h) || render_is_identity(vm, word, *h) {
                    let mut buf = itoa_buf();
                    let bytes = fmt_at(&mut buf, false, *h as i64);
                    *ph = combo_fnv(bytes, *ph);
                    crate::diag::combo_entry_ident().inc();
                    return bytes.len();
                }
            }
        }
        _ => {}
    }
    // WHICH kinds still miss. 2,047,254 of 2,219,738 misses are not primitives, and an arm cannot
    // be written for a population that has not been named.
    crate::diag::combo_entry_miss().inc();
    // ⭐**AND WHICH CLASSES THEY ARE.** "879,136 handle objects" is a population, not a target —
    // the same gap that made the arena `size()` work start on the wrong class until the refusal
    // census named it. `RAVA_BACKEDGE_CENSUS` prints these as `CK:` rows.
    if crate::backedge::enabled() {
        if matches!(v, Value::Ref(_) | Value::Obj(_)) {
            let name = vm.value_class_name(v);
            crate::backedge::note_entry(
                name.as_ptr() as usize ^ name.len(),
                || format!("CK:{name}"),
            );
        }
    }
    match v {
        Value::Str(_) => crate::diag::combo_miss_str().inc(),
        Value::Ref(_) => crate::diag::combo_miss_ref().inc(),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(_) => crate::diag::combo_miss_obj().inc(),
        _ => crate::diag::combo_miss_other().inc(),
    }
    // ⭐**A STRING LEAF STILL ALLOCATED A STRING TO BE HASHED AND DROPPED.** `combo_miss_str`
    // counts them: the fold reached `structural_key`, which for a leaf string returns
    // `format!("s:{s}")` — a fresh heap copy of the WHOLE string content plus two characters,
    // built solely so `combo_fnv` could walk its bytes. The bytes are already in hand.
    //
    // Byte-identical by construction: `"s:"` then the content, which is exactly what
    // `key_str_at`'s `Value::Str` arm writes and what `key_str_mut_in` writes for an ArenaString
    // (through `from_utf8_lossy`, kept here so an invalid sequence still renders U+FFFD the same
    // way; `Cow::Borrowed` means no allocation whenever the bytes are valid UTF-8, i.e. always in
    // practice). A different byte here is a different `MapKey::Hash`, i.e. a silent miss — the
    // same warning `combo_prim_bytes` carries.
    if fast_mapkey() {
        match v {
            Value::Str(s) => {
                let s = s.as_str();
                *ph = combo_fnv(b"s:", *ph);
                *ph = combo_fnv(s.as_bytes(), *ph);
                return 2 + s.len();
            }
            #[cfg(feature = "next-gen-gc")]
            Value::Obj(p) if crate::nextgen::is_arena_string(*p) => {
                let content = String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p));
                *ph = combo_fnv(b"s:", *ph);
                *ph = combo_fnv(content.as_bytes(), *ph);
                return 2 + content.len();
            }
            _ => {}
        }
    }
    let s = structural_key(vm, v);
    *ph = combo_fnv(s.as_bytes(), *ph);
    s.len()
}

pub(crate) fn combo_hash_of(vm: &mut Vm, entries: &[(Value, Value)]) -> u64 {
    // Was a nested, byte-identical copy of `combo_fnv`. Two hand-copied FNV loops that MUST agree
    // (a differing byte is a differing `MapKey::Hash`, i.e. a silent miss) are a drift hazard for
    // no benefit — the two had already drifted once, per the note on `combo_prim_bytes`.
    let mut acc: u64 = 0;
    let mut est_bytes: usize = 0;
    // Hash a value's key bytes WITHOUT materialising them when the memo already holds them. The
    // fallback is the original `structural_key`, so the bytes fed to FNV are identical either way.
    let mut key_bytes_len = |vm: &mut Vm, v: &Value, ph: &mut u64| -> usize {
        if let Value::Obj(p) = v {
            if !keymemo_disabled() {
                // Same concrete fold as `combo_entry_bytes` — see the note there. Both folds MUST
                // write identical bytes (a map populated through one representation is probed
                // through the other), which is why they now share one implementation instead of
                // two hand-copied FNV closures.
                if let Some(len) = valkey::fold(*p, crate::nextgen::gc_epoch(), ph) {
                    return len;
                }
            }
        }
        // The memo only ever serves arena `Value::Obj`, so every PRIMITIVE entry — half of a
        // property map, since its entries are (Property, value) — fell straight through to
        // `structural_key` and allocated a `String` to hold `i:5`. `combo_prim_bytes` is the
        // encoding `combo_entry_bytes` already uses, byte-for-byte what `structural_key`
        // returns, written into a stack buffer.
        if let Some(len) = combo_prim_bytes(v, ph) {
            return len;
        }
        let s = structural_key(vm, v);
        *ph = combo_fnv(s.as_bytes(), *ph);
        s.len()
    };
    // This function receives the Rust snapshot of an arena map. Root that snapshot only here,
    // rather than taxing every key render, and always re-read the forwarded slot before each
    // nested render.
    let base = crate::vm::thread_context::root_len(&vm.gc_protect);
    let _scope = crate::stream::GpScope(&mut vm.gc_protect as *mut _, base);
    crate::vm::thread_context::reserve_roots(
        &mut vm.gc_protect,
        entries.len().saturating_mul(2),
    );
    for (k, val) in entries {
        crate::vm::thread_context::push_root(&mut vm.gc_protect, k.clone());
        crate::vm::thread_context::push_root(&mut vm.gc_protect, val.clone());
    }
    for i in 0..entries.len() {
        let mut ph = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
        let k = crate::vm::thread_context::root_index(&vm.gc_protect, base + i * 2);
        let kl = key_bytes_len(vm, &k, &mut ph);
        ph = combo_fnv(b"=", ph);
        let val = crate::vm::thread_context::root_index(&vm.gc_protect, base + i * 2 + 1);
        let vl = key_bytes_len(vm, &val, &mut ph);
        est_bytes += kl + vl + 2;
        acc = acc.wrapping_add(splitmix64(ph));
    }
    keystat_bytes(est_bytes);
    acc ^ splitmix64(entries.len() as u64 + 1)
}

#[cfg(all(test, feature = "next-gen-gc"))]
mod composite_key_gc_tests {
    use super::arraymap_entry_count;

    #[test]
    fn arraymap_count_clamps_declared_size_to_both_arrays() {
        assert_eq!(arraymap_entry_count(Some(3), 5, 4), 3);
        assert_eq!(arraymap_entry_count(Some(9), 5, 4), 4);
    }

    #[test]
    fn arraymap_count_preserves_negative_and_missing_size_rules() {
        assert_eq!(arraymap_entry_count(Some(-7), 5, 4), 0);
        assert_eq!(arraymap_entry_count(None, 5, 4), 4);
    }
}

/// SPLIT PROBE (`RAVA_KEYSPLIT`, default OFF, inert): WHERE inside a key derivation the ~2,200 ns
/// go. `RAVA_KEYTIME` timed the whole of `map_key` and five guesses at the reason have now been
/// refuted by measurement — identity formatting, key text volume, the map class, the class-name
/// allocation, and the `equals_kind` name hash (the last two were mine, and together they moved
/// 3.457 s to 3.310 s over 1.5 M derivations). This times the PARTS instead of guessing a sixth.
///
/// Three buckets, chosen because they are the only phases every derivation passes through:
/// `idkeys` = the `$idkeys` by-NAME field probe that decides identity-vs-value keying;
/// `struct` = `structural_key*`, the render itself; `other` = everything else in the funnel.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod keysplit {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub(crate) static NS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
    pub(crate) static N: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_KEYSPLIT").is_some())
    }

    /// Charge `ns` to a bucket: 0 = idkeys, 1 = struct, 2 = other.
    #[inline]
    pub(crate) fn add(bucket: usize, ns: u64) {
        NS[bucket].fetch_add(ns, Relaxed);
    }

    pub(crate) fn tick() {
        let n = N.fetch_add(1, Relaxed) + 1;
        if n % 250_000 == 0 {
            let a = NS[0].load(Relaxed);
            let b = NS[1].load(Relaxed);
            let c = NS[2].load(Relaxed);
            let tot = (a + b + c).max(1);
            eprintln!(
                "[KEYSPLIT] {n} derivations | idkeys {:.3}s ({:.1}%, {} ns) | struct {:.3}s ({:.1}%, {} ns) | other {:.3}s ({:.1}%, {} ns)",
                a as f64 / 1e9, a as f64 * 100.0 / tot as f64, a / n,
                b as f64 / 1e9, b as f64 * 100.0 / tot as f64, b / n,
                c as f64 / 1e9, c as f64 * 100.0 / tot as f64, c / n,
            );
        }
    }
}

// ⛔**"A KEY WHOSE CLASS NEVER OVERRODE `equals`/`hashCode` IS AN IDENTITY KEY" — REFUTED, TWICE,
// BY THE GATES.** The JDK contract argument is sound: `HashMap.get(k)` buckets by `k.hashCode()`
// and compares `k.equals(stored)`, so for a class that inherits both from `java/lang/Object` a
// probe can only ever find the entry stored under the same reference. It is still the wrong rule
// FOR THIS ENGINE, and both attempts died the same way — `IllegalStateException: Errors during
// registry creation`, i.e. registry lookups that used to hit began to miss.
//
// The first attempt read the rule off bytecode alone. That is not enough here: a class can get
// its value semantics from a NATIVE rather than from a method in its class file, and many of the
// JDK and library classes the cascade serves carry no `equals` at all. Adding
// `owner_bits::may_serve` — the engine's own proof that no dispatcher names an owner — did not
// fix it either.
//
// What is left is the deeper reason, and it is the one the plan already names: THE ENGINE HAS TWO
// REPRESENTATIONS OF EVERY OBJECT. The same guest object can be put as a `Value::Obj` and probed
// as a `Value::Ref` (or the reverse), and a STRUCTURAL key matched across that seam because it
// compares contents. An identity key cannot, and the miss is silent. The `$idkeys` branch below
// survives precisely because someone wrote its bridge deliberately (`pinned_handle_of`) for that
// exact hazard.
//
// So the alloc-free key stays where identity was ALREADY the rule, and the general form waits on
// the representation work. Do not re-derive it from the JDK contract — the contract is not the
// thing that is wrong.

pub(crate) fn idkey_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_IDKEY").is_none())
}

/// The alloc-free identity key for `k`, mirroring the strings it replaces exactly.
/// `identity_key` for the builder half in `keyroot`, which lives in another module.
pub(crate) fn identity_key_pub(vm: &Vm, k: &Value) -> Option<crate::heap::MapKey> {
    identity_key(vm, k)
}

fn identity_key(vm: &Vm, k: &Value) -> Option<crate::heap::MapKey> {
    use crate::heap::{MapKey, MAPKEY_ARENA_TAG};
    match k {
        Value::Ref(h) => Some(MapKey::Id(*h as u64)),
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            // B5 IDENTITY-MAP-KEY BRIDGE, unchanged: a pinned mirror IS its handle's object and
            // must key identically, or a map filled with handles and probed with pinned pointers
            // misses.
            if let Some(h) = crate::nextgen::pinned_handle_of(p.addr()) {
                return Some(MapKey::Id(h as u64));
            }
            Some(MapKey::Id(
                MAPKEY_ARENA_TAG | crate::nextgen::arena_identity_hash(*p) as u64,
            ))
        }
        _ => None,
    }
    .filter(|_| {
        let _ = vm;
        true
    })
}

/// Key by IDENTITY when the KEY's class PROVES equality is identity. **DEFAULT ON**, kill switch
/// `RAVA_NO_IDENTITY_KEY`.
///
/// Priced by `bench/StateKeyCost.java`, which reproduces `StateHolder`'s exact shape:
/// ```text
///                        java    rava OFF   rava ON
/// identity-equals key     4 ns     495 ns    105 ns   <- 4.7x
/// value-equals key        4 ns     440 ns    415 ns   <- control, untouched
/// ```
/// The control class declares a real field-comparing `equals` and its timing does not move,
/// which is what says the rule DISCRIMINATES rather than admitting everything.
pub(crate) fn idkey_by_class_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_IDENTITY_KEY").is_none())
}

/// Does `class` — and every class up its super chain — inherit `Object.equals`/`Object.hashCode`
/// unchanged?
///
/// ⭐**Why this is the lever.** `RAVA_KEYTIME` splits a boot's 1,500,000 map-key derivations:
/// 912,225 take the STRUCTURAL-STRING path at **5,004 ns each = 4.565 s**, versus 50 ns for an
/// int and 408 ns for an identity key. `RAVA_STRUCTKEY_SCAN` names the dominant key class:
/// `net/minecraft/world/level/block/state/BlockState`, with **32 fields** — every one of which
/// the structural render walks and formats, on every `get` and every `put`.
///
/// And it never needed to. `HashMap` keys by `hashCode`+`equals`; if BOTH are inherited from
/// `Object`, those ARE reference identity, so the canonical-string render computes an expensive
/// answer to a question whose cheap answer is exact. Minecraft's block states are interned
/// singletons for exactly this reason.
///
/// The rule is decided from BYTECODE — does any class in the chain DECLARE
/// `equals(Ljava/lang/Object;)Z` or `hashCode()I` — which is permitted evidence. It is not a
/// name list and not a heuristic: a record's generated `equals` is declared in its classfile and
/// so is caught, and a class this engine serves natively is excluded by the has-bytecode guard
/// below (its `equals` lives in Rust, where no classfile can testify about it).
///
/// ⚠**Both methods, not just `equals`.** A class that overrides only `hashCode` is legal; keying
/// it by identity would still agree with its `equals`, but two identity-distinct keys could then
/// land in different buckets from the ones its `hashCode` intends, and any code that mixes our
/// key with a guest-computed hash would disagree. Requiring both absent keeps the two halves of
/// the contract together.
/// `equality_is_identity` for an ARENA object, memoised on the class id — no name, no hashing of
/// one. The name is resolved at most once per class, on the first miss.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn identity_verdict_cid(vm: &mut Vm, cid: u32) -> bool {
    verdict_memo!(u32);
    if let Some(v) = verdict_get(cid) {
        return v;
    }
    let verdict = match crate::nextgen::class_name(cid) {
        Some(n) => {
            let n = n.to_string();
            !n.is_empty() && !n.starts_with('[') && equality_is_identity(vm, &n)
        }
        None => false,
    };
    verdict_put(cid, verdict);
    verdict
}

/// The handle twin, memoised on the heap's class word.
/// **THE RENDERER IS ITS OWN EVIDENCE: does this class's full structural render collapse to the
/// identity token `@h`?**
///
/// ⭐The combo fold's largest remaining miss family was `java/lang/Boolean` — 879,136 handle
/// objects a datapack run, each running the whole `structural_key` machinery to produce the four
/// characters `@37`. `identity_verdict_word` refuses it (Boolean declares `equals`), and it is
/// right to: the question it answers is "may this be KEYED by identity", which is a semantic
/// claim. This asks something weaker and checkable — "does the renderer ALREADY answer `@h` for
/// this class" — and settles it by rendering ONCE per class word and comparing.
///
/// So the bytes written on a hit are, by construction, the bytes the fallback would have
/// written; nothing about keying semantics changes, only the allocation and the walk disappear.
/// A class whose render is anything else caches `false` and keeps the full path forever.
fn render_is_identity(vm: &mut Vm, word: i64, h: u32) -> bool {
    // `RAVA_NO_RENDER_IDENT=1` restores the full render at every one of these entries.
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_NO_RENDER_IDENT").is_none()) {
        return false;
    }
    verdict_memo!(i64);
    if let Some(v) = verdict_get(word) {
        return v;
    }
    // The table is released across this render ON PURPOSE: `structural_key` folds the object's own
    // map entries, which re-enters `identity_verdict_*` and this function for other class words.
    let rendered = structural_key(vm, &Value::Ref(h));
    let mut buf = itoa_buf();
    let want = fmt_at(&mut buf, false, h as i64);
    let verdict = rendered.as_bytes() == want;
    verdict_put(word, verdict);
    verdict
}

pub(crate) fn identity_verdict_word(vm: &mut Vm, word: i64, h: u32) -> bool {
    verdict_memo!(i64);
    if let Some(v) = verdict_get(word) {
        return v;
    }
    let verdict = match vm.heap.class_of(h).map(str::to_string) {
        Some(n) => !n.is_empty() && !n.starts_with('[') && equality_is_identity(vm, &n),
        None => false,
    };
    verdict_put(word, verdict);
    verdict
}

pub(crate) fn equality_is_identity(vm: &mut Vm, class: &str) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    // Memoised per class name: this is a per-put/get hot path, and the answer is a pure function
    // of the loaded hierarchy.
    //
    // ⭐**THE HASHER WAS SIPHASH.** A bare `std::collections::HashMap` carries `RandomState`,
    // which is a keyed cryptographic PRF — DoS resistance bought for a table whose keys are this
    // engine's own class names, never an attacker's. It charges four rounds of ARX per 8 bytes,
    // so a 40-60 character `net/minecraft/...` name is ~30 rounds on EVERY probe, and the
    // datapack-reload profile carries SipHash at 6.1% beside the field interner. `FastMap` folds
    // the same name in 5-8 rotate-xor-multiply rounds (`fast_strhash`, word-at-a-time).
    //
    // `Box<str>` rather than `String` for the key: the same bytes without the spare capacity
    // word, and `get(class)` still takes a `&str` through `Borrow`. Bounded by the number of
    // distinct classes ever used as a map key — a few hundred, ~24 B/entry, low tens of KB. This
    // is a process-lifetime memo behind an `RwLock`, NOT an L1 cache, so its footprint is not
    // the thing that has to fit; the per-probe hash was.
    static MEMO: std::sync::OnceLock<std::sync::RwLock<crate::FastMap<Box<str>, bool>>> =
        std::sync::OnceLock::new();
    let memo = MEMO.get_or_init(|| std::sync::RwLock::new(crate::FastMap::default()));
    if let Some(&v) = memo.read().unwrap_or_else(|e| e.into_inner()).get(class) {
        IDKEY_HIT.fetch_add(1, Relaxed);
        return v;
    }
    let mut cur = class.to_string();
    let mut verdict = true;
    let mut depth = 0;
    loop {
        if cur == "java/lang/Object" {
            // Nothing on the chain overrode it ⇒ `Object.equals`, which IS reference identity.
            break;
        }
        depth += 1;
        if depth > 64 {
            verdict = false;
            break;
        }
        // No classfile ⇒ this engine serves the class natively and its equality lives in Rust.
        // Nothing here can testify about it, so refuse.
        let Ok(c) = vm.loader.get_or_load(&cur) else {
            verdict = false;
            break;
        };
        // ⭐**AN ENUM IS ADMISSIBLE BY ITS ACCESS FLAG.** `java.lang.Enum.equals` is declared
        // `final` and its body is `return this == other;` — the JLS forbids an enum from
        // overriding it, so for ANY class carrying `ACC_ENUM` equality IS identity. The flag is
        // bytecode evidence, not a name, and this engine already checks it elsewhere for the
        // same kind of guarantee (`native_contract`'s enum reads).
        //
        // Without this the rule refused every enum, because `java/lang/Enum` is native-served
        // and has no classfile to testify — and enums are exactly what the composite keys are
        // full of: 153,700 `Direction` key-uses in a boot, each re-rendering an 11-field object.
        // An enum constant WITH A BODY is an anonymous subclass that does not carry the flag,
        // but its superclass does and the walk reaches it.
        if c.file.access_flags & 0x4000 != 0 {
            break;
        }
        // ⭐**THE EFFECTIVE `equals` IS THE FIRST ONE UP THE CHAIN — and its BODY decides.**
        //
        // The first version refused a class merely for DECLARING `equals`, and that refused the
        // one that matters: `StateHolder` declares `public final boolean equals(Object)`, so
        // `BlockState` — 147,027 map-key uses, the single largest — was rejected. Its body is
        // `return this == object;`. Minecraft writes it that way ON PURPOSE, because block states
        // are interned; the declaration is an assertion that equality IS identity, not a denial.
        //
        // `hashCode` is deliberately NOT consulted. Our key has to agree with `equals` and only
        // with `equals`: if `equals` is identity then two distinct objects are unequal however
        // their hashes compare, so an identity key can neither merge nor split a pair wrongly.
        if let Some(m) = c.file.method("equals", "(Ljava/lang/Object;)Z") {
            verdict = m
                .code
                .as_ref()
                .is_some_and(|code| body_is_reference_equality(&code.bytecode, &c.file.pool));
            break;
        }
        match c.file.super_class.clone() {
            Some(sup) => cur = sup,
            None => break,
        }
    }
    memo.write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(class.into(), verdict);
    if verdict {
        IDKEY_ADMIT.fetch_add(1, Relaxed);
        // The admitted set is small (one entry per class) and it is the only thing that says
        // WHICH class a wrong verdict came from — but a product boot must not print 293 lines,
        // so it is behind the same census flag the rest of this diagnosis uses.
        if std::env::var_os("RAVA_IDKEY_CENSUS").is_some() {
            eprintln!("[IDKEY-ADMIT] {class}");
        }
    } else {
        IDKEY_REFUSE.fetch_add(1, Relaxed);
    }
    verdict
}

/// Is this method body exactly `return this == other;`?
///
/// javac emits one of two shapes for it, differing only in which way the branch goes:
///   `aload_0 aload_1 if_acmpne +7 iconst_1 ireturn iconst_0 ireturn`
///   `aload_0 aload_1 if_acmpeq +7 iconst_0 ireturn iconst_1 ireturn`
/// Matching the exact byte sequence is the whole check — anything else, including a body that
/// merely starts with an identity short-circuit and then compares fields, is refused.
fn body_is_reference_equality(bc: &[u8], pool: &rava_classfile::ConstantPool) -> bool {
    // `return this == other;`
    const NE: [u8; 9] = [0x2a, 0x2b, 0xa6, 0x00, 0x07, 0x04, 0xac, 0x03, 0xac];
    const EQ: [u8; 9] = [0x2a, 0x2b, 0xa5, 0x00, 0x07, 0x03, 0xac, 0x04, 0xac];
    if bc == NE || bc == EQ {
        return true;
    }
    // ⭐`return super.equals(other);` where the super IS `java/lang/Object` — which is how
    // Minecraft's `StateHolder` writes it, and `StateHolder` is `BlockState`'s effective
    // `equals`, i.e. the single most-used map key in the boot. `aload_0 aload_1 invokespecial
    // <ref> ireturn`, with the ref checked to be `java/lang/Object.equals` and nothing else.
    if bc.len() == 6 && bc[0] == 0x2a && bc[1] == 0x2b && bc[2] == 0xb7 && bc[5] == 0xac {
        let idx = u16::from_be_bytes([bc[3], bc[4]]);
        if let Some(m) = pool.member_ref(idx) {
            return m.owner == "java/lang/Object"
                && m.name == "equals"
                && m.descriptor == "(Ljava/lang/Object;)Z";
        }
    }
    false
}

pub(crate) static IDKEY_HIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static IDKEY_ADMIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static IDKEY_REFUSE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// WHICH BRANCH a derivation took, for `map_key`'s timer to attribute its elapsed to. A total
/// says the derivations cost 9.8 s of a 47 s boot; only the split says whether that is the
/// structural render (fixable) or the guest `hashCode` it calls (not).
pub(crate) mod keybranch {
    pub const INT: u8 = 0;
    pub const ID: u8 = 1;
    pub const ID_STR: u8 = 2;
    pub const HASH: u8 = 3;
    pub const STRUCT: u8 = 4;
    pub const NAMES: [&str; 5] = ["int", "identity", "identity-str", "combo-hash", "structural-str"];
    /// ✅**AUDITED, ALREADY THE CHEAP SHAPE — DO NOT "FIX" IT.** `const {}`-initialised and its
    /// payload is a `Cell<u8>`, which is destructor-free, so this is the one combination that
    /// lowers to a BARE TLS-slot load: no `LocalKey::with` out-of-line call, no lazy-init state
    /// check, no destructor-registration byte, no borrow flag.
    ///
    /// `set` runs on EVERY map-key derivation (~1.5 M a boot) for a diagnostic whose only reader
    /// is the `RAVA_KEYTIME` timer — the shape that is worth gating elsewhere. Not here: the whole
    /// operation is one store to a slot the thread already has in a register, so a process-constant
    /// flag read to skip it would cost strictly more than the store it skips.
    std::thread_local! {
        pub static LAST: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    }
    #[inline]
    pub fn set(b: u8) {
        LAST.with(|c| c.set(b));
    }
    #[inline]
    pub fn get() -> u8 {
        LAST.with(|c| c.get())
    }
}

pub(crate) fn map_key_inner(vm: &mut Vm, map_obj: u32, k: &Value) -> Result<crate::heap::MapKey, VmError> {
    use crate::heap::MapKey;
    // M5.1 STRUCTKEY SCAN: record OBJECT keys (the structural-key churn — a fresh composite key built
    // per put/get). Primitives/strings are cheap non-churn keys; skip them.
    if crate::structkey_scan::enabled() && matches!(k, Value::Obj(_) | Value::Ref(_)) {
        let name = vm.value_class_name(k);
        crate::structkey_scan::note_mapkey(&name);
    }
    // FAST PATH: an `int` key (the autobox `HashMap<Integer,_>` case — the
    // Collections worst path) keys by its raw value with NO string allocation.
    // rava boxes `Integer` as `Value::Int` (value identity), so this is correct
    // even for an IdentityHashMap, and a top-level int was already uniquely encoded
    // `"i:n"` so `Int(n)` collides with no string key.
    if let Value::Int(n) = k {
        keybranch::set(keybranch::INT);
        return Ok(MapKey::Int(*n as i64));
    }
    // ⭐**A NARROW BOX KEYS AS THE NUMBER IT HOLDS — the key it derived before it became an
    // object.** `crate::boxid` turns `Character`/`Short`/`Byte` into real heap objects, and
    // without this arm they would fall through to the IDENTITY key below: the interned range
    // (`Character` 0..=127) would still round-trip, and everything above it would put under one
    // handle and get under another, so `m.put(c, v); m.get(c)` answered null for exactly the
    // chars outside the cache.
    //
    // ⚠**PLACED IMMEDIATELY AFTER THE INT FAST PATH, WHICH IS WHERE THESE KEYS USED TO LAND** —
    // ahead of `string_first`, `identity_first` and the map's own identity-vs-value resolution,
    // all of which the old `Value::Int(codepoint)` also came in ahead of. That ordering is the
    // whole warrant: the derived key is byte-identical to the one this map already held for the
    // same character, so no existing table's keying changes, including a fastutil `Reference2*`
    // map's (which value-keyed these before for the same reason).
    //
    // It also PRESERVES, rather than fixes, one pre-existing aliasing: `MapKey::Int(35)` is the
    // key of both `Integer.valueOf(35)` and `Character.valueOf('#')`, so one `HashMap` holding
    // both collapses them. That was true before this work and stays true; separating them means
    // a tagged `MapKey`, which is a change to the map key encoding rather than to this arm.
    if let Some((_, n)) = crate::natives::boxed_narrow(vm, Some(k)) {
        keybranch::set(keybranch::INT);
        return Ok(MapKey::Int(n as i64));
    }
    // ⭐**AHEAD OF THE MAP-CLASS IDENTITY BLOCK, not after it.** That block reads a synthetic
    // field, resolves the map's class and consults `$idkeys` before any branch is chosen, so a
    // rule placed below it pays the whole prefix: the identity branch measured 1,015 ns for what
    // is a class-id lookup and a header read. This rule needs nothing from the MAP — it is a
    // property of the KEY's class — so it belongs here, next to the int fast path.
    //
    // Ordering is also semantically free: where the map-class block would answer "identity", so
    // does this, and for a value map this answer is exact rather than an approximation.
    if let Some(key) = crate::map::keyroot::string_first(k) {
        keybranch::set(keybranch::ID_STR);
        return Ok(key);
    }
    if let Some(key) = crate::map::keyroot::identity_first(vm, k) {
        keybranch::set(keybranch::ID);
        return Ok(key);
    }
    // Resolve identity-vs-value keying once and cache it on `$idkeys` (this is a
    // per-put/get hot path). Identity iff the map IS-A java.util.IdentityHashMap
    // — including a SUBCLASS: snakeyaml's representer keeps `representedObjects`
    // in an anonymous IdentityHashMap subclass to anchor shared references, so a
    // plain exact-name check keyed it by value and aliased every equal-but-
    // distinct empty section to one anchor (`&id1 []` / `*id1` …), which made the
    // re-parsed config a self-referential node → SequenceNode.toString overflow.
    // fastutil Reference2* maps are identity-keyed too.
    #[cfg(feature = "next-gen-gc")]
    let split_t = keysplit::enabled().then(std::time::Instant::now);
    let identity = match vm.heap.get_field_syn(map_obj, crate::heap::Syn::IdKeys) {
        Value::Int(v) => v == 1,
        _ => {
            let id = match vm.heap.class_of(map_obj) {
                Some(c) if c == "java/util/IdentityHashMap" || c.contains("/Reference2") => true,
                Some(c) => {
                    let c = c.to_string();
                    crate::opcodes::name_is_subtype(vm, &c, "java/util/IdentityHashMap")
                }
                None => false,
            };
            vm.heap.put_field_syn(map_obj, crate::heap::Syn::IdKeys, Value::Int(id as i32));
            id
        }
    };
    #[cfg(feature = "next-gen-gc")]
    let split_t = split_t.map(|t| {
        keysplit::add(0, t.elapsed().as_nanos() as u64);
        std::time::Instant::now()
    });
    if identity {
        // ALLOC-FREE since the `MapKey::Id` variant exists; the encoding is the same
        // distinction the `@{h}` / `@a{hash}` text drew, without the `String`.
        if idkey_enabled() {
            if let Some(key) = identity_key(vm, k) {
                keybranch::set(keybranch::ID);
                return Ok(key);
            }
        }
        if let Value::Ref(h) = k {
            keybranch::set(keybranch::ID_STR);
            return Ok(MapKey::str(format!("@{h}")));
        }
        // Arena key in an identity map: GC-stable identity hash (never toString,
        // never the moving address).
        #[cfg(feature = "next-gen-gc")]
        if let Value::Obj(p) = k {
            // B5 IDENTITY-MAP-KEY BRIDGE: a pinned-mirror pointer is the SAME
            // object as its handle Ref(h). The IdentityHashMap path above keys a
            // handle as `@{h}`; a pinned mirror MUST key identically or a map
            // populated with handles and later probed with pinned pointers (the
            // registry byValue IdentityHashMap, probed by TagLoader/HolderSet
            // during datapack tag resolution) misses. This mirrors the
            // identity-hash bridge in natives::identity_hash (Obj(pinned)→h as i32)
            // and ref_eq. Inert until pinned ptrs flow (pinned_handle_of is None
            // when the pinned region is unmapped → default-off byte-identical).
            if let Some(h) = crate::nextgen::pinned_handle_of(p.addr()) {
                keybranch::set(keybranch::ID_STR);
                return Ok(MapKey::str(format!("@{h}")));
            }
            keybranch::set(keybranch::ID_STR);
            return Ok(MapKey::str(format!("@a{}", crate::nextgen::arena_identity_hash(*p))));
        }
    }
    // Combo-map fast path: fold a block-state property map to an alloc-free u64.
    // KEY-CLASS IDENTITY: equality is identity when the key's class chain declares neither
    // `equals` nor `hashCode` (see `equality_is_identity`). Placed AHEAD of the combo hash and
    // the structural render because it is exact and both of those are approximations of it.

    #[cfg(feature = "next-gen-gc")]
    if combo_hash_enabled() {
        if let Some(h) = combo_hash_key(vm, k) {
            keybranch::set(keybranch::HASH);
            return Ok(MapKey::Hash(h));
        }
    }
    // GAP-P04: this derivation is FOR a specific map — under RAVA_MAPKEY_SALT its
    // backing id is the salt scope (value objects key by equals-verified salted
    // probing; guest throws propagate). Unsalted (or no backing map yet) keeps the
    // exact historic derivation.
    // ⛔**THE RECORD KEY PATH: BUILT, PROVEN TO RUN, AND STILL WORTH NOTHING.**
    //
    // The value-object path below derives a key by running the guest's `toString()`. For a
    // `java.lang.Record` that is the generated `ObjectMethods.bootstrap` indy, recursing through
    // every component, and `RAVA_KEYTIME`'s slow-key histogram prices it exactly:
    //   288 renders of `datafixers.types.templates.Sum`         3,147,677 ns each = 0.907 s
    //   832 renders of `DSL$Instances$TaggedChoiceCacheKey`       720,892 ns each = 0.600 s
    // **1.5 s of a ~40 s boot in 1,120 renders.** A JVM's HashMap never does this: it calls
    // `hashCode()` and settles ties with `equals()` — which is what the GAP-P04 salted path
    // (`RAVA_MAPKEY_SALT`) below says it implements.
    //
    // Three attempts, three measured refusals:
    //  1. A handle twin of the arena funnel memo. Overlapping/worse — those 1,120 renders are
    //     1,120 DISTINCT objects, and a per-object memo cannot serve a first render.
    //  2. `RAVA_MAPKEY_SALT` for every value object: `Done` 0.654/0.684/0.738 against
    //     0.465/0.539/0.620, all three worse. Equals-verified probing is a guest call per probe,
    //     and for a cheap key (`Identifier`, `ResourceKey`) the memoised render already wins.
    //     It also breaks two bit-exact gates (`ringlane`, `ringlanei`) — the salt is assigned by
    //     insertion order, which the lane does not reproduce.
    //  3. Narrowing it to records — admitted by BYTECODE (`extends java/lang/Record`), never by
    //     name, the same kind of evidence the identity rule uses for `ACC_ENUM`. It RUNS:
    //     `[RECORDKEY] seen=23,209 taken=23,209`, gates and all 21 probes green. And `Sum` still
    //     renders 288 times at 3.5 ms. ⭐**`structural_key_in` still builds the `toString` tree
    //     for anything it cannot salt**, so routing more objects into it changes nothing — the
    //     doc's "instead of building the full toString tree" covers only the salted subset.
    //
    // ⇒ What is left is not a patch beside the render but a change to what a `MapKey` IS for a
    // value object: a `(class, guest hashCode)` pair verified by `equals`, the way a real
    // HashMap works, with a salt scheme the parallel lane can reproduce. 1.5 s, named population.
    if mapkey_salt_enabled() {
        if let Value::Int(mid) = vm.heap.get_field_syn(map_obj, crate::heap::Syn::Map) {
            keybranch::set(keybranch::STRUCT);
            return Ok(MapKey::str(structural_key_in(vm, mid as u32, k)?));
        }
    }
    #[cfg(feature = "next-gen-gc")]
    {
        keybranch::set(keybranch::STRUCT);
        let key = MapKey::str(structural_key(vm, k));
        if let Some(t) = split_t {
            keysplit::add(1, t.elapsed().as_nanos() as u64);
            keysplit::tick();
        }
        return Ok(key);
    }
    #[cfg(not(feature = "next-gen-gc"))]
    Ok(MapKey::str(structural_key(vm, k)))
}

/// Is the key-render census (`RAVA_KEYRENDER_STAT`) on?
///
/// `gcsafe::keyrender_stat` keeps its own `OnceLock` private inside the function body, so a
/// caller that wants to SKIP work whose only consumer is a counter cannot ask it. Same variable,
/// same answer; this only lets the question be asked before the work rather than after.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn keyrender_stat_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_KEYRENDER_STAT").is_some())
}

/// The funnel memo's read half. `None` = render and then call `arena_funnel_put`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_funnel_get(vm: &mut Vm, p: crate::arena::ArenaPtr) -> Option<String> {
    let id = crate::nextgen::arena_identity_hash(p) as u32;
    if id == 0 {
        keyrender_stat(2);
        return None;
    }
    let tag = crate::nextgen::gc_epoch() ^ (vm.runtime_shared().static_gen() as u64).rotate_left(32);
    if fast_mapkeytls() {
        // ⭐**THE MEMO FIRST, THE ADMISSION CHECK ONLY ON A MISS.** `arena_class_all_final` is
        // itself a thread-local verdict table, so the old order opened TWO `LocalKey::with`s on
        // the HIT — the case the memo exists for. Answers and census counts are unchanged: only
        // an admissible class is ever PUT, so a non-admissible one still misses, still reaches the
        // check below and still tallies `keyrender_stat(2)`.
        if let Some(s) = arena_render_get(id, tag) {
            keyrender_stat(0);
            return Some(s);
        }
        // ⭐**A VERDICT DERIVED FOR A CENSUS THAT IS OFF, AND DERIVED AGAIN WHERE IT DECIDES.**
        // On a memo MISS this branch's ONLY effect is `keyrender_stat(2)` — both paths `return
        // None` — and `keyrender_stat` is a no-op unless `RAVA_KEYRENDER_STAT` is set. So
        // `arena_class_all_final` (an `arena_class_id` read plus a thread-local verdict-table
        // probe) ran on every miss to answer a question nobody was asking. The SAME verdict for
        // the SAME object is then derived a second time, one render later, by
        // `arena_funnel_put`, which is where it actually decides whether to store.
        //
        // ⚠One fact, two derivations, and the first is conditional on a flag — so the fix is to
        // ask the flag first. `keyrender_stat`'s own gate is private to `gcsafe`, so this reads
        // the identical environment variable into its own `OnceLock`; when the census IS on the
        // call happens exactly as before and the counts are unchanged. Population:
        // `structural_key_scoped` reports 1,968,071 arena renders a boot at a 65.6% hit rate, so
        // this is ~677,000 wasted probes. Kill switch `RAVA_NO_FAST_MAPDERIVE`.
        if (!fast_mapderive() || keyrender_stat_on())
            && !arena_keymemo_all()
            && !arena_class_all_final(vm, p)
        {
            keyrender_stat(2);
        }
        return None;
    }
    if !arena_keymemo_all() && !arena_class_all_final(vm, p) {
        keyrender_stat(2);
        return None;
    }
    let hit = arena_render_get(id, tag);
    if hit.is_some() {
        keyrender_stat(0);
    }
    hit
}

/// The funnel memo's write half.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_funnel_put(vm: &mut Vm, p: crate::arena::ArenaPtr, rendered: &str) {
    let id = crate::nextgen::arena_identity_hash(p) as u32;
    if id == 0 || (!arena_keymemo_all() && !arena_class_all_final(vm, p)) {
        return;
    }
    keyrender_stat(1);
    let tag = crate::nextgen::gc_epoch() ^ (vm.runtime_shared().static_gen() as u64).rotate_left(32);
    arena_render_put(id, tag, rendered);
}

/// ⛔**WHY THERE IS NO HANDLE TWIN OF THE FUNNEL MEMO — BUILT, MEASURED, REMOVED.**
///
/// `structural_key_scoped` memoises a rendered key per object for arena `Value::Obj` only; a
/// handle object gets one ONLY when it is a drainable map (`$skey` in `key_render`). So a
/// handle-side VALUE object — a record, a cache key — is re-rendered from scratch. That looked
/// like the explanation for the slow-key histogram `RAVA_KEYTIME` prints on a product boot:
///
/// ```text
/// 288 renders  0.907 s  3,147,677 ns each  com/mojang/datafixers/types/templates/Sum
/// 832 renders  0.600 s    720,892 ns each  com/mojang/datafixers/DSL$Instances$TaggedChoiceCacheKey
/// 180 renders  0.026 s    146,156 ns each  net/minecraft/tags/TagKey
/// ```
///
/// **1.5 s of a ~40 s boot in 1,120 renders**, at up to 3.1 MILLISECONDS each — 4% of the boot in
/// a thousand events, the most concentrated thing left in this engine. Both DFU classes are
/// `java.lang.Record`s, so their `equals` is structural and the identity rule cannot admit them.
///
/// A handle twin of `arena_funnel_get/put` was written with the arena memo's exact soundness rule
/// (every instance field `final` up the whole hierarchy, tagged by `gc_epoch ^ static_gen` so
/// handle reuse across a collection cannot serve a stale render). Interleaved boot A/B, P-cores:
///
/// ```text
/// memo ON   wall 39.2 42.7 40.7   Done 0.461 0.567 0.477
/// memo OFF  wall 40.8 39.1 38.7   Done 0.471 0.460 0.445
/// ```
///
/// Overlapping and if anything WORSE. ⭐**And the reason is in the histogram itself: those 1,120
/// renders are 1,120 DISTINCT objects, each rendered once.** A per-object memo cannot serve a
/// first render, and the shared-sub-graph re-rendering it was built for either does not happen or
/// is refused by the all-final rule inside DFU. The 3 ms is the render, not repetition of it.
///
/// ⇒ The fix this line actually needs is not a memo but **not producing a canonical STRING for a
/// record at all** — deriving the key from the components the way `combo_hash_of` already does
/// for arena maps (1,750 ns against 17,813 for the structural branch). That is a change to what
/// a `MapKey` IS, not an addition beside it.
/// Walk the hierarchy and answer whether every instance field is `final` all the way up — the
/// "immutable enough to memoise a rendered key for" test. Refuses on a non-final field, an
/// unloadable link, or a chain deeper than 64.
fn class_chain_all_final(vm: &mut Vm, start: &str) -> bool {
    let mut cur = start.to_string();
    let mut depth = 0;
    loop {
        if cur == "java/lang/Object" {
            return true;
        }
        depth += 1;
        if depth > 64 {
            return false;
        }
        let Ok(lc) = vm.loader.get_or_load(&cur) else { return false };
        let all_final = lc
            .file
            .fields
            .iter()
            .filter(|f| f.access_flags & 0x0008 == 0)
            .all(|f| f.access_flags & 0x0010 != 0);
        if !all_final {
            return false;
        }
        match lc.file.super_class.clone() {
            Some(sup) => cur = sup,
            None => return true,
        }
    }
}

pub(crate) fn arena_keymemo_all() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ARENA_KEYMEMO_ALL").is_some())
}

/// Extend the per-object key-string memo to ARENA objects. **DEFAULT ON**, kill switch
/// `RAVA_NO_ARENA_KEYMEMO`.
///
/// ⭐**What it is actually for, which took a second look to see.** Measured when it was first
/// built it was NEUTRAL — because at that point its population was the cheap repeats, and the
/// identity rule had not yet removed them. With those gone, `RAVA_KEYTIME`'s slow-render census
/// names what is left, and it is not block states at all:
///
/// ```text
///    289 renders, 2.367 s, 8,189,152 ns avg  datafixers/types/templates/Sum
///    832 renders, 1.421 s, 1,708,181 ns avg  datafixers/DSL$Instances$TaggedChoiceCacheKey
/// 14,137 renders, 0.351 s,    24,852 ns avg  ResourceKey$InternKey
///  7,925 renders, 0.197 s,    24,834 ns avg  Identifier
/// ```
///
/// **Eight milliseconds to derive one map key.** DataFixerUpper's type templates are recursive,
/// so rendering one walks the whole type graph — and DFU uses them as memo-cache keys, so the
/// same graph is walked again and again. With the memo those four rows leave the census
/// entirely and the worst remaining render is 0.045 s.
///
/// Admission is the STRICT rule — every instance field `final`, up the whole super chain — so
/// this rests on a JVM guarantee rather than on the HashMap contract: the verifier rejects
/// `putfield` on a final field outside the declaring class's `<init>`, so once constructed the
/// render is fixed. DFU's templates pass because they are immutable by construction.
pub(crate) fn arena_keymemo_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_ARENA_KEYMEMO").is_none())
}

std::thread_local! {
    /// (invalidation tag, identity -> rendered key). Same shape and same bound as the handle
    /// memo next to it.
    ///
    /// ⚠**THIS IS NOW THE KILL-SWITCH ARM** (`RAVA_NO_FAST_MAPKEYTLS=1`). It is left exactly as it
    /// was; with the switch ON, `arena_render_get`/`arena_render_put` serve from `valkey`'s span
    /// table instead and this one is never touched. Its shape is the expensive one, spelled out:
    /// a NON-const initialiser (out-of-line `LocalKey::with` + lazy-init check on every touch), a
    /// `RefCell` (borrow flag load, compare, +1 store, guard release store) and a `Box<str>` value
    /// (one allocator block per put, one extra dependent cache miss per hit).
    static ARENA_KEYRENDER_MEMO: std::cell::RefCell<(u64, crate::FastMap<u32, Box<str>>)> =
        std::cell::RefCell::new((0, crate::FastMap::default()));
}

/// The arena render memo's READ half, in one place — it was written out identically in
/// `arena_funnel_get` and `arena_key_memoed`.
///
/// SIZE (the fast arm, `valkey::render_get`): `FastMap<u64, Span>`, 8 + 8 = 16 B/bucket + 1
/// hashbrown control byte = **17 B/slot**, against the legacy `FastMap<u32, Box<str>>`'s 4 + 16
/// padded to 24 B + 1 = **25 B/slot** plus one heap block per entry. Same `KEYRENDER_MAX` = 65,536
/// entry bound, same `gc_epoch ^ static_gen` tag, same population. It SHRINKS; nothing is added.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn arena_render_get(id: u32, tag: u64) -> Option<String> {
    if fast_mapkeytls() {
        return valkey::render_get(id, tag);
    }
    ARENA_KEYRENDER_MEMO.with(|c| {
        let m = c.borrow();
        if m.0 == tag { m.1.get(&id).map(|s| s.to_string()) } else { None }
    })
}

/// The arena render memo's WRITE half. The tag flip and the entry bound are the legacy arm's own,
/// moved verbatim into `valkey::Memo::insert` for the fast arm (which clears the byte buffer with
/// the table, so no span can outlive its bytes).
#[cfg(feature = "next-gen-gc")]
#[inline]
fn arena_render_put(id: u32, tag: u64, rendered: &str) {
    if fast_mapkeytls() {
        valkey::render_put(id, rendered, tag, KEYRENDER_MAX);
        return;
    }
    ARENA_KEYRENDER_MEMO.with(|c| {
        let mut m = c.borrow_mut();
        if m.0 != tag {
            m.0 = tag;
            m.1.clear();
        }
        if m.1.len() >= KEYRENDER_MAX {
            m.1.clear();
        }
        m.1.insert(id, rendered.into());
    });
}

/// `Some(key)` when this arena object is memoisable and its key is known or freshly rendered;
/// `None` when the class is not admissible and the caller must take the ordinary path.
#[cfg(feature = "next-gen-gc")]
fn arena_key_memoed(vm: &mut Vm, p: crate::arena::ArenaPtr, v: &Value) -> Result<Option<String>, VmError> {
    // The identity stamp is a MONOTONIC counter carried in the object header, so it is unique for
    // the process lifetime and survives evacuation — exactly what a memo key must be.
    let id = crate::nextgen::arena_identity_hash(p) as u32;
    if id == 0 {
        return Ok(None);
    }
    let tag = crate::nextgen::gc_epoch() ^ (vm.runtime_shared().static_gen() as u64).rotate_left(32);
    if let Some(s) = arena_render_get(id, tag) {
        keyrender_stat(0);
        return Ok(Some(s));
    }
    // CEILING PROBE (`RAVA_ARENA_KEYMEMO_ALL=1`, measurement only): admit every class, to price
    // what a SOUND admission rule could be worth before spending effort deriving one. The
    // all-final rule refuses 98.5% of renders — `BlockState` is mutated after construction
    // (`populateNeighbours`, `initCache`) — but the question a memo actually needs answered is
    // narrower: are the fields THE RENDER READS immutable?
    if !arena_keymemo_all() && !arena_class_all_final(vm, p) {
        keyrender_stat(2);
        return Ok(None);
    }
    let rendered = key_str_mut_named(vm, v, None, NamedProbe::Unprobed)?;
    keyrender_stat(1);
    arena_render_put(id, tag, &rendered);
    Ok(Some(rendered))
}

/// The arena twin of `class_all_final_word`: every INSTANCE field of the object's class — and of
/// every class up its super chain — declared `final`.
///
/// ⚠The handle version checks only the concrete class's own fields. Walking the supers here is
/// deliberate and STRICTER: a mutable field inherited from a superclass makes the instance
/// mutable just as surely, and a memo that outlives a mutation is a wrong answer, not a slow one.
#[cfg(feature = "next-gen-gc")]
fn arena_class_all_final(vm: &mut Vm, p: crate::arena::ArenaPtr) -> bool {
    verdict_memo!(u32);
    let cid = crate::nextgen::arena_class_id(p);
    if let Some(v) = verdict_get(cid) {
        return v;
    }
    // Same walk as the handle twin — one implementation, so the two memos cannot drift apart on
    // what "immutable enough to memoise" means.
    let verdict = match crate::nextgen::class_name(cid) {
        Some(name) => {
            let name = name.to_string();
            class_chain_all_final(vm, &name)
        }
        None => false,
    };
    verdict_put(cid, verdict);
    verdict
}
