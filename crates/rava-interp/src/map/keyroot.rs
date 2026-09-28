//! The map-key root and the value keys derived through it.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::map::*;

/// ⭐**THE KEY ROOT'S OWN SWITCH.** DEFAULT ON, kill switch `RAVA_NO_FAST_KEYROOT=1`, and it also
/// yields to `flags::emit_more_disabled()` so the whole rewrite block prices as one A/B.
///
/// Everything behind it is byte-identical to what it replaces; what changes is how much WORK the
/// answer costs. In order of the population each serves:
///
///  * **`string_first` built its key with `format!`.** `format!` sizes by doubling, so the result
///    overshoots its length; `MapKey::str` then calls `String::into_boxed_str`, i.e.
///    `shrink_to_fit`, which allocates AGAIN and copies the whole key a second time. **Two
///    allocations and two full copies of the key text before the map hashes anything**, on the
///    single most frequent key shape in this workload (registries, resource locations, JSON —
///    Minecraft keys by string everywhere). `tagged_exact` sizes the buffer to
///    `tag.len() + body.len()`, which makes the shrink a no-op: **one allocation, one copy**.
///    O(2·len) -> O(len) in the key's byte length, per probe.
///  * **`value_map_key` — the BUILDER half — had no string shortcut at all.** Its three sibling
///    funnels (`map_key_inner`, `value_map_key_in`, `cache_key`) all call `string_first`; this one
///    walked the entire `structural_key` prologue — the arena-map test, the ArrayMap admission
///    chain, `KeyScope::enter`, the record renderer, the pinned-mirror bridge, the funnel memo —
///    to reach a two-line arm at the very bottom of the renderer that produces exactly the bytes
///    `string_first` produces. O(prologue) -> O(1) per `Map.of`/`copyOf`/drain string key.
///  * **`cache_key` was missing the `Value::Int` fast path its own doc comment demands.** Its maps
///    ARE ordinary heap maps (`Syn::Map`), reachable from the ordinary map natives, and all three
///    other funnels key a top-level `Value::Int(n)` as `MapKey::Int(n)`. This one rendered
///    `"i:{n}"` through the whole structural path instead — a different key for the same value, so
///    a cache filled through `asMap().put` could not be found through `getIfPresent`, and vice
///    versa. Convergence, not a new rule.
///  * **`identity_token` went through `core::fmt` for two constant characters and a hex integer.**
///    Written directly into an exactly-sized buffer instead. It is called once per NESTED entry of
///    a composite key (153,700 `Direction` key-uses a boot), so it is on the per-element path.
///  * **`MapKeyRoot` rooted values that cannot move and cannot be collected.** See its comment.
///
/// TABLE SIZE: none. Nothing here adds a cache; every item removes work.
pub(crate) fn fast_keyroot() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_KEYROOT").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// `format!("{tag}{body}")` with the allocation sized EXACTLY once — the local twin of
/// `keys::key_render::key_tagged`.
///
/// ⚠**IT IS DELIBERATELY NOT A CALL TO THAT ONE.** `key_tagged` consults `keys::fast_mapkey()`,
/// which is `&& crate::flags::fast_natives()`, and `fast_natives` is OPT-IN (`RAVA_FAST_NATIVES=1`,
/// default OFF because the round-3 block measured ~3% slower as a block). Routing this path
/// through it would leave the change inert on every default boot — the one failure mode this
/// round cannot afford. The gate here is `RAVA_NO_FAST_KEYROOT`, default ON, so the fix ships and
/// still prices as a same-binary A/B.
///
/// Byte-for-byte identical to the `format!` it replaces; what changes is the allocation count.
/// `format!` grows its buffer by doubling and so almost always ends with `capacity > len`;
/// `MapKey::str` then calls `String::into_boxed_str`, whose `shrink_to_fit` allocates a SECOND
/// time and memcpys the whole key again. Sizing to `tag.len() + body.len()` makes that shrink a
/// no-op.
#[inline]
fn tagged_exact(tag: &str, body: &str) -> String {
    let mut out = String::with_capacity(tag.len() + body.len());
    out.push_str(tag);
    out.push_str(body);
    out
}

/// Combo-key u64 fast path (default ON; `RAVA_NO_MAP_COMBO_HASH=1` restores the
/// canonical-string path for A/B or safety). Replaces the `m:{k=v,…}` String +
/// Box<str> that `BlockState.populateNeighbours` built per lookup with an
/// allocation-free `MapKey::Hash(u64)`.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn combo_hash_enabled() -> bool {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_MAP_COMBO_HASH").is_none())
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn keyrender_memo_disabled() -> bool {
    pub(crate) static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_NO_KEYRENDER_MEMO").is_some())
}

#[cfg(feature = "next-gen-gc")]
thread_local! {
    /// (tag, handle -> render). `tag` folds the GC epoch and the reflective-write generation.
    pub(crate) static KEYRENDER_MEMO: std::cell::RefCell<(u64, crate::FastMap<u32, Box<str>>)> =
        std::cell::RefCell::new((u64::MAX, crate::FastMap::default()));
}

/// Does every INSTANCE field of this class carry ACC_FINAL?
///
/// Keyed by the object's CLASS WORD (the interned `Arc<str>` pointer), not by the class NAME.
/// The first cut looked the name up: `class_of(h).map(str::to_string)` allocated a String and then
/// took a global `Mutex` on EVERY call whose class was not admitted - so for any workload with
/// mutable key objects the memo added cost exactly where it saved nothing. A thread-local map
/// keyed on a pointer-sized word has neither.
#[cfg(feature = "next-gen-gc")]
/// CENSUS (`RAVA_KEYCLASS`, default OFF, inert): of the classes the key-render memo REFUSES, how
/// many are refused for a reason that a cell-comparison could see?
///
/// Row 309 measured the counterfactual — the refused memo would hit **86.7% of all renders and
/// would never have been wrong** — and named three sound designs, two of which (verify-on-hit by
/// field cells, key-by-cells) are sound only when a mutation cannot hide behind a REFERENCE field:
/// if a non-final field holds a ref, the referenced object can change while this object's own
/// cells do not, and the render changes with it.
///
/// So the classes split three ways, and only a count says whether the sound subset is the
/// population or a rounding error:
///
/// * `final` — every instance field final; the memo already admits these.
/// * `prim` — has non-final instance fields, but ALL of them are primitive descriptors. A cell
///   comparison sees every mutation that can affect the render, so (b) and (c) are sound here.
/// * `ref` — has a non-final field of reference or array type. Neither design is sound; only
///   write-invalidation would be, and that means touching `putfield` (4.1 M a boot).
/// CENSUS (`RAVA_TOSTR_FIELDS`, default OFF, inert): would a NARROWER, still-sound admission rule
/// recover the memo the class rule refuses?
///
/// ⭐**The class rule's soundness argument is about the RENDER, but the rule is about the OBJECT.**
/// `map/mod.rs` states it: the verifier rejects `putfield` on a final field outside the declaring
/// class's `<init>`, so an all-final object's render is fixed once it is constructed. The render,
/// however, only depends on the fields its own `toString` READS — and `BlockStateBase.initCache()`
/// is the shape that makes this matter: it writes non-final CACHE fields that `toString` never
/// touches, and the whole class is refused for them.
///
/// So the narrower rule is: **admit when every field the class's own `toString` reads is final.**
/// Sound by the same argument, decided by bytecode (a `getfield` scan plus the declaring class's
/// access flags), and a pure function of the class, so it memoises exactly like `arraymap_ids`.
///
/// This counts what it would be worth before anything is built. Buckets:
/// * `all_final` — `toString` exists, reads only `this` fields, every one final ⇒ ADMISSIBLE.
/// * `nonfinal_read` — it reads a non-final field ⇒ genuinely unsafe, the rule earns its keep.
/// * `calls_out` — it invokes something, so the fields it transitively reads are unknown here;
///   conservative, and the honest bucket for "this census cannot decide".
/// * `no_tostring` — no `toString` bytecode resolvable.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn tostr_fields_census(vm: &mut Vm, h: u32) {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_TOSTR_FIELDS").is_some()) {
        return;
    }
    static C: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
    thread_local! {
        static KIND: std::cell::RefCell<crate::FastMap<i64, u8>> =
            std::cell::RefCell::new(crate::FastMap::default());
    }
    let Some(word) = vm.heap.object_class_word(h) else { return };
    let cached = KIND.with(|c| c.borrow().get(&word).copied());
    let kind = match cached {
        Some(k) => k,
        None => {
            let k = classify_tostring(vm, h);
            KIND.with(|c| {
                c.borrow_mut().insert(word, k);
            });
            k
        }
    };
    let n = C[kind as usize].fetch_add(1, Relaxed) + 1;
    let v: Vec<u64> = C.iter().map(|a| a.load(Relaxed)).collect();
    let tot: u64 = v.iter().sum::<u64>().max(1);
    if n % 250_000 == 0 {
        eprintln!(
            "[TOSTRFIELDS] all_final={} nonfinal_read={} calls_out={} no_tostring={} | admissible {:.1}% of refused renders",
            v[0], v[1], v[2], v[3],
            v[0] as f64 * 100.0 / tot as f64
        );
    }
}

/// 0 = every `toString`-read field is final · 1 = reads a non-final field · 2 = calls out (unknown)
/// · 3 = no resolvable `toString`.
#[cfg(feature = "next-gen-gc")]
fn classify_tostring(vm: &mut Vm, h: u32) -> u8 {
    let Some(cls) = vm.heap.class_of(h).map(str::to_string) else { return 3 };
    let Ok(lc) = vm.loader.get_or_load(&cls) else { return 3 };
    let Some(m) = lc.file.methods.iter().find(|m| m.name == "toString" && m.descriptor == "()Ljava/lang/String;") else {
        return 3;
    };
    let Some(code) = m.code.as_ref() else { return 3 };
    let bc = &code.bytecode;
    let mut saw_nonfinal = false;
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        let Some(len) = crate::jit::insn_len_at(bc, pc) else { return 2 };
        match op {
            // getfield: the fields this render actually depends on.
            0xb4 => {
                let idx = ((bc[pc + 1] as u16) << 8) | bc[pc + 2] as u16;
                match lc.file.pool.member_ref(idx) {
                    Some(mr) => {
                        // The DECLARING class decides finality, not the receiver's class.
                        let owner_final = vm
                            .loader
                            .get_or_load(mr.owner)
                            .ok()
                            .and_then(|oc| {
                                oc.file
                                    .fields
                                    .iter()
                                    .find(|f| f.name == mr.name)
                                    .map(|f| f.access_flags & 0x0010 != 0)
                            });
                        match owner_final {
                            Some(true) => {}
                            _ => saw_nonfinal = true,
                        }
                    }
                    None => return 2,
                }
            }
            // Any call makes the transitive field set unknown to this census.
            0xb6..=0xba => return 2,
            _ => {}
        }
        pc += len;
    }
    if saw_nonfinal { 1 } else { 0 }
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn keyclass_census(vm: &mut Vm, h: u32) {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_KEYCLASS").is_some()) {
        return;
    }
    static C: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3]; // final, prim, ref
    thread_local! {
        static KIND: std::cell::RefCell<crate::FastMap<i64, u8>> =
            std::cell::RefCell::new(crate::FastMap::default());
    }
    let Some(word) = vm.heap.object_class_word(h) else { return };
    let cached = KIND.with(|c| c.borrow().get(&word).copied());
    let kind = match cached {
        Some(k) => k,
        None => {
            let k = (|| {
                let Some(cls) = vm.heap.class_of(h).map(str::to_string) else { return 2u8 };
                let Ok(lc) = vm.loader.get_or_load(&cls) else { return 2u8 };
                let mut saw_nonfinal_prim = false;
                for f in lc.file.fields.iter().filter(|f| f.access_flags & 0x0008 == 0) {
                    if f.access_flags & 0x0010 != 0 {
                        continue; // final
                    }
                    // A primitive descriptor is exactly one of the eight single letters; anything
                    // starting `L` or `[` can hide a mutation behind a reference.
                    match f.descriptor.as_bytes().first() {
                        Some(b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z') => {
                            saw_nonfinal_prim = true;
                        }
                        _ => return 2u8,
                    }
                }
                if saw_nonfinal_prim { 1u8 } else { 0u8 }
            })();
            KIND.with(|c| {
                c.borrow_mut().insert(word, k);
            });
            k
        }
    };
    let n = C[kind as usize].fetch_add(1, Relaxed) + 1;
    let (a, b, r) = (C[0].load(Relaxed), C[1].load(Relaxed), C[2].load(Relaxed));
    let tot = (a + b + r).max(1);
    if n % 250_000 == 0 {
        eprintln!(
            "[KEYCLASS] final={a} nonfinal_all_prim={b} has_ref_field={r} | cell-comparable {:.1}% of refused renders",
            (a + b) as f64 * 100.0 / tot as f64
        );
    }
}

pub(crate) fn class_all_final_word(vm: &mut Vm, h: u32) -> bool {
    thread_local! {
        static SEEN: std::cell::RefCell<crate::FastMap<i64, bool>> =
            std::cell::RefCell::new(crate::FastMap::default());
    }
    let Some(word) = vm.heap.object_class_word(h) else { return false };
    if let Some(v) = SEEN.with(|c| c.borrow().get(&word).copied()) {
        return v;
    }
    let Some(cls) = vm.heap.class_of(h).map(str::to_string) else { return false };
    let verdict = match vm.loader.get_or_load(&cls) {
        Ok(lc) => lc
            .file
            .fields
            .iter()
            // static fields cannot make an instance mutable
            .filter(|f| f.access_flags & 0x0008 == 0)
            .all(|f| f.access_flags & 0x0010 != 0),
        Err(_) => false,
    };
    SEEN.with(|c| c.borrow_mut().insert(word, verdict));
    verdict
}

/// The key guava's `LoadingCache` stores under. Its maps are reachable from the ordinary map
/// natives too (`LoadingCache.getUnchecked` is 2.55% of the boot's compiled-to-host crossings),
/// so it must choose keys the same way they do.
pub(crate) fn cache_key(vm: &mut Vm, k: &Value) -> crate::heap::MapKey {
    // ⭐**THE INT ARM WAS THE ONE PLACE THIS FUNNEL DISAGREED WITH ITS OWN DOC COMMENT.** A guava
    // cache's store is an ordinary heap map (`Syn::Map`), so `asMap()`/`Map.get` reach it through
    // `map_key_inner` — and that funnel, like `value_map_key` and `value_map_key_in`, keys a
    // top-level `Value::Int(n)` as `MapKey::Int(n)`. This one had no int arm, so it fell through
    // `string_first` and `identity_first` into the full structural renderer to produce the string
    // `"i:{n}"`: a DIFFERENT key for the same value, which is exactly the split that once printed
    // `Unbound values in registry … world_preset` on the identity rule. An entry put through the
    // map native could not be found through `getIfPresent`, and vice versa.
    //
    // So this is convergence, not a new rule — and it also removes a `String` allocation, its
    // `shrink_to_fit` twin and the whole prologue per int-keyed cache op: O(prologue + digits) ->
    // O(1). The `MapKey` doc records why `Int(n)` cannot alias anything: a top-level int was
    // already uniquely encoded `"i:n"`, and NESTED ints inside composite keys stay strings.
    if fast_keyroot() {
        if let Value::Int(n) = k {
            return crate::heap::MapKey::Int(*n as i64);
        }
    }
    if let Some(key) = string_first(k) {
        return key;
    }
    if let Some(key) = identity_first(vm, k) {
        return key;
    }
    crate::heap::MapKey::str(structural_key(vm, k))
}

/// GAP-P04: `value_map_key` for a key that targets a KNOWN backing map `mid` —
/// salts value objects against it under `RAVA_MAPKEY_SALT` (throws propagate);
/// byte-identical to `value_map_key` otherwise.
pub(crate) fn value_map_key_in(vm: &mut Vm, mid: u32, k: &Value) -> Result<crate::heap::MapKey, VmError> {
    if let Value::Int(n) = k {
        return Ok(crate::heap::MapKey::Int(*n as i64));
    }
    // ⚠This funnel was the one missed on the first attempt. Salted derivation is scoped to one
    // map, but the identity rule is a property of the KEY's class, so it is admissible here too
    // and MUST be, or a salted put disagrees with an unsalted get.
    if let Some(key) = string_first(k) {
        return Ok(key);
    }
    if let Some(key) = identity_first(vm, k) {
        return Ok(key);
    }
    #[cfg(feature = "next-gen-gc")]
    if combo_hash_enabled() {
        if let Some(h) = combo_hash_key(vm, k) {
            return Ok(crate::heap::MapKey::Hash(h));
        }
    }
    if mapkey_salt_enabled() {
        return Ok(crate::heap::MapKey::str(structural_key_in(vm, mid, k)?));
    }
    Ok(crate::heap::MapKey::str(structural_key(vm, k)))
}

/// Lookup key for a key `k` in a specific map object. An identity-based map
/// (java.util.IdentityHashMap, or a fastutil Reference2* map) keys references by
/// object identity regardless of equals/hashCode — MappedRegistry's byValue is
/// an IdentityHashMap holding records (e.g. GameEvent) that are equals-equal but
/// must stay distinct. Every other Map keys by value (structural_key).
/// SPLIT PROBE (`RAVA_KEYTIME`, default OFF, inert): how much of a native map operation is KEY
/// DERIVATION and how much is everything else. `Map#get` is 29.7% of block A+B's native-bridge
/// time at ~4.3 us per call, and three separate guesses at why (identity formatting, key text
/// volume, the map class) were each refuted by measurement. This times the one remaining half
/// directly instead of guessing at it a fourth time.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn map_key(vm: &mut Vm, map_obj: u32, k: &Value) -> Result<crate::heap::MapKey, VmError> {
    pub(crate) static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_KEYTIME").is_some()) {
        return map_key_inner(vm, map_obj, k);
    }
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub(crate) static NS: AtomicU64 = AtomicU64::new(0);
    pub(crate) static N: AtomicU64 = AtomicU64::new(0);
    pub(crate) static NESTED: AtomicU64 = AtomicU64::new(0);
    static SLOW_KEYS: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<String, (u64, u64)>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    pub(crate) static STRUCT_KIND: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
    pub(crate) static STRUCT_KIND_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
    pub(crate) static BR_N: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
    pub(crate) static BR_NS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
    // ⚠**ONLY THE OUTERMOST DERIVATION IS TIMED.** `map_key_inner` re-enters guest code (a key's
    // `hashCode`/`equals`/`toString`), which derives more keys — so an inclusive timer at every
    // level bills every child to every ancestor. Undepth-gated this read **4807 ns average**,
    // which is the same double count that once made `String.valueOf` look like 17 seconds and
    // collapsed to 1.14 under exactly this fix. The count is exact either way; only the time
    // needed the gate.
    std::thread_local! {
        static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    if DEPTH.with(|d| d.get()) != 0 {
        NESTED.fetch_add(1, Relaxed);
        return map_key_inner(vm, map_obj, k);
    }
    DEPTH.with(|d| d.set(1));
    let t0 = std::time::Instant::now();
    let r = map_key_inner(vm, map_obj, k);
    let el = t0.elapsed().as_nanos() as u64;
    DEPTH.with(|d| d.set(0));
    // Attribute the elapsed to the BRANCH the derivation actually took. The total alone cannot
    // say whether the cost is the structural render (which is ours to fix) or the guest
    // `hashCode` a combo-hash calls (which is not).
    {
        use crate::map::keys::keybranch;
        // WHICH VALUE KIND still reaches the structural renderer. The object census
        // (`RAVA_STRUCTKEY_SCAN`) only notes `Obj`/`Ref` and accounts for ~50k of the 489,760
        // that remain, so the rest is some other kind and nothing said which.
        // WHICH structural renders are the expensive ones. The kind split says
        // `Obj = 87,149 x 50,907 ns`, and 51 us is not a five-property map — so the population
        // has to be named before anything is built for it.
        if keybranch::get() == keybranch::STRUCT && el > 10_000 {
            let name = vm.value_class_name(k);
            let mut g = SLOW_KEYS.lock().unwrap_or_else(|e| e.into_inner());
            let e = g.entry(name).or_insert((0u64, 0u64));
            e.0 += 1;
            e.1 += el;
        }
        if keybranch::get() == keybranch::STRUCT {
            let kind = match k {
                Value::Str(_) => 0usize,
                Value::Obj(_) => 1,
                Value::Ref(_) => 2,
                Value::Array(_) => 3,
                Value::Long(_) => 4,
                Value::Double(_) | Value::Float(_) => 5,
                Value::Null => 6,
                _ => 7,
            };
            STRUCT_KIND[kind].fetch_add(1, Relaxed);
            STRUCT_KIND_NS[kind].fetch_add(el, Relaxed);
        }
        let b = keybranch::get() as usize;
        if b < BR_N.len() {
            BR_N[b].fetch_add(1, Relaxed);
            BR_NS[b].fetch_add(el, Relaxed);
        }
    }
    let ns = NS.fetch_add(el, Relaxed) + el;
    let n = N.fetch_add(1, Relaxed) + 1;
    if n % 250_000 == 0 {
        use crate::map::keys::keybranch::NAMES;
        let mut split = String::new();
        for (i, nm) in NAMES.iter().enumerate() {
            let c = BR_N[i].load(Relaxed);
            if c > 0 {
                split.push_str(&format!(
                    " | {nm} n={c} {:.3}s ({} ns avg)",
                    BR_NS[i].load(Relaxed) as f64 / 1e9,
                    BR_NS[i].load(Relaxed) / c
                ));
            }
        }
        {
            const KN: [&str; 8] = ["Str", "Obj", "Ref", "Array", "Long", "FP", "Null", "other"];
            let mut ks = String::new();
            for (i, nm) in KN.iter().enumerate() {
                let c = STRUCT_KIND[i].load(Relaxed);
                if c > 0 {
                    ks.push_str(&format!(
                        " {nm}={c} ({:.3}s, {} ns)",
                        STRUCT_KIND_NS[i].load(Relaxed) as f64 / 1e9,
                        STRUCT_KIND_NS[i].load(Relaxed) / c
                    ));
                }
            }
            eprintln!("[KEYKIND] structural by value kind:{ks}");
            let g = SLOW_KEYS.lock().unwrap_or_else(|e| e.into_inner());
            let mut rows: Vec<(&String, &(u64, u64))> = g.iter().collect();
            rows.sort_by_key(|(_, v)| std::cmp::Reverse(v.1));
            for (name, (n, ns)) in rows.iter().take(8) {
                eprintln!(
                    "[KEYSLOW] {:>7} renders, {:.3}s, {} ns avg  {name}",
                    n,
                    *ns as f64 / 1e9,
                    ns / n.max(&1)
                );
            }
        }
        eprintln!(
            "[RECORDRENDER] records seen={} rendered={}",
            crate::map::keys::key_render::RECORD_RENDER_SEEN.load(Relaxed),
            crate::map::keys::key_render::RECORD_RENDER_DONE.load(Relaxed),
        );
        eprintln!(
            "[IDKEY] class-verdicts: admitted={} refused={} memo-hits={}",
            crate::map::keys::IDKEY_ADMIT.load(Relaxed),
            crate::map::keys::IDKEY_REFUSE.load(Relaxed),
            crate::map::keys::IDKEY_HIT.load(Relaxed),
        );
        eprintln!(
            "[KEYTIME] {n} OUTERMOST derivations, {:.3}s total, {} ns avg | nested (not timed) {}{split}",
            ns as f64 / 1e9,
            ns / n,
            NESTED.load(Relaxed),
        );
    }
    r
}

/// Top-level VALUE-map key (no identity semantics) as a `MapKey` — the same Int
/// fast path as `map_key`, used by the builder paths (`Map.of`/`copyOf`/drain/…)
/// that key by value. An int keys allocation-free; everything else structurally.
/// THE identity rule, in ONE place. Every funnel that CHOOSES a map key calls this first; a
/// funnel that skips it derives a different key for the same object and the map stops finding
/// its own entries (measured: `Unbound values in registry ... world_preset`).
///
/// The funnels are exactly four — `map_key_inner`, `value_map_key`, `value_map_key_in` and
/// guava's `cache.rs`. The other `MapKey` constructors in the tree are not funnels: `arena_map`
/// only READS keys, `cascade_table`/`map_method` build STRING keys, and `jit/emit/collections`
/// emits only the `MapKey::Int` fast path — none of them can see an object key.
/// A STRING key, straight to its final form.
///
/// `RAVA_STRUCTKEY_SCAN` puts `[arena-string]` at 145,652 map-key uses — the largest remaining
/// class once `BlockState` moved to the identity rule — and every one of them walked the whole
/// `structural_key_scoped` prologue (the arena-map test, the ArrayMap admission chain, the key
/// scope) to reach a two-line arm at the top of the renderer. Hoisting it changes nothing about
/// the ANSWER: this is byte-for-byte the string that arm returns, which is what makes the
/// shortcut safe against the other funnels by construction.
pub(crate) fn string_first(k: &Value) -> Option<crate::heap::MapKey> {
    // ⭐**A PLAIN `Value::Str` KEY WAS WALKING THE WHOLE RENDERER.** The arena-string arm below
    // was added first and covered only `Value::Obj`; a heap `Value::Str` fell past the identity
    // rule (not a Ref/Obj), past the combo hash (Obj only) and into `structural_key` — to produce
    // the four characters `"s:" ++ contents`. Measured (`bench/SeamCost3.java`): `Map.get` with a
    // STRING key **1,865 ns** against the same map's 98 ns with an identity key, and Minecraft
    // keys by string everywhere — registries, resource locations, JSON. Byte-identical to
    // `key_str`'s `Value::Str(s) => format!("s:{s}")`.
    if let Value::Str(s) = k {
        // COUNT BEFORE CHANGING: how many map probes go through the string fast path, and
        // therefore pay one `format!` allocation plus a full-length hash each. `RAVA_DIAG=mapkey`.
        crate::diag::mapkey_str().inc();
        let _t = crate::diag::mapkey_str_price().start();
        // ⭐**THE COUNT WAS TAKEN AND THE CHANGE IS THIS LINE.** `format!("s:{s}")` sizes its
        // buffer by DOUBLING, so it almost always overshoots; `MapKey::str` then takes
        // `String::into_boxed_str`, whose `shrink_to_fit` allocates a second time and copies the
        // whole key again. Two allocations and two full copies of the key content, before the map
        // has hashed a single byte. `tagged_exact` sizes to `2 + s.len()` exactly, so the shrink
        // is a no-op: **one allocation, one copy** — O(2·len) -> O(len) in the key's byte length,
        // on every string-keyed probe. Byte-for-byte the same key text either way.
        if fast_keyroot() {
            return Some(crate::heap::MapKey::str(tagged_exact("s:", s.as_str())));
        }
        return Some(crate::heap::MapKey::str(format!("s:{s}")));
    }
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = k {
        if crate::nextgen::is_arena_string(*p) {
            // Same two-allocation defect on the arena-string arm, plus one more: for valid UTF-8
            // — every arena string this engine builds — `from_utf8_lossy` returns
            // `Cow::Borrowed`, so the `format!` was copying out of a borrow it could have written
            // from directly. Lossy is KEPT, not dropped: an invalid sequence must still render the
            // same U+FFFD bytes it always did.
            let body = String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p));
            if fast_keyroot() {
                return Some(crate::heap::MapKey::str(tagged_exact("s:", &body)));
            }
            return Some(crate::heap::MapKey::str(format!("s:{body}")));
        }
    }
    let _ = k;
    None
}

/// The identity rule ONE LEVEL DEEPER: a token to substitute for a nested value's full render
/// inside a composite key.
///
/// ⭐**Why this is sound, and why it is not the same statement as the top-level rule.** A
/// composite key must distinguish exactly what `equals` distinguishes. Map equality compares
/// values with `equals`; if a value's `equals` IS identity, then two maps are equal only when
/// they hold the identical value objects, so substituting that object's identity for its render
/// is exact rather than an approximation. If its `equals` compares fields, the rule refuses and
/// the render happens as before.
///
/// ⭐**Why it matters.** `RAVA_KEYTIME`+`RAVA_STRUCTKEY_SCAN` put the remaining structural cost
/// at `Obj = 326,612 x 16,115 ns = 5.26 s`, dominated by `Reference2ObjectArrayMap` — the
/// block-state property map used AS a key — whose render recurses into every entry. Its entries
/// are `Property`/`Direction`-style interned singletons: 153,700 `Direction` key-uses in a boot,
/// each one previously re-rendering an 11-field object.
pub(crate) fn identity_token(vm: &mut Vm, v: &Value) -> Option<String> {
    let key = identity_first(vm, v)?;
    match key {
        // `format!("@i{u:x}")` for two constant bytes and at most sixteen hex digits: a
        // `core::fmt::Arguments` built on the stack, a dynamic-dispatch `write_fmt` loop, a
        // radix-formatting call, and a `String` that then overshoots and is copied again by
        // `into_boxed_str`. This is the PER-ELEMENT path of a composite key — one call per nested
        // entry, 153,700 `Direction` key-uses in a boot — so it is worth writing by hand.
        // Identical bytes: `{:x}` is minimal-width lowercase hex, and zero prints `"0"`.
        crate::heap::MapKey::Id(u) if fast_keyroot() => {
            let mut d = [0u8; 16];
            let mut i = d.len();
            let mut n = u;
            loop {
                i -= 1;
                d[i] = b"0123456789abcdef"[(n & 0xf) as usize];
                n >>= 4;
                if n == 0 {
                    break;
                }
            }
            // Exact capacity, so `MapKey::str`'s later `shrink_to_fit` is a no-op.
            let mut out = String::with_capacity(2 + (d.len() - i));
            out.push_str("@i");
            for &b in &d[i..] {
                out.push(b as char);
            }
            Some(out)
        }
        crate::heap::MapKey::Id(u) => Some(format!("@i{u:x}")),
        _ => None,
    }
}

pub(crate) fn identity_first(vm: &mut Vm, k: &Value) -> Option<crate::heap::MapKey> {
    // ⭐**THE CHEAPER DISCRIMINATOR FIRST.** Both tests are pure, so the order is invisible to the
    // answer — but the `matches!` is a tag compare in a register while `idkey_by_class_enabled` is
    // a `OnceLock` acquire-load, and the refusals here are dominated by values that are neither
    // `Obj` nor `Ref` (every `Long`, `Double`, `Str` and `Null` key reaching this funnel).
    if !matches!(k, Value::Obj(_) | Value::Ref(_)) {
        return None;
    }
    if !crate::map::keys::idkey_by_class_enabled() {
        return None;
    }
    // ⚠**Do NOT ask for the class NAME here.** `value_class_name` builds a fresh `String` on every
    // derivation and the verdict memo then hashes it — measured at 1,191 ns for what is supposed
    // to be a pointer read. The verdict is a pure function of the object's CLASS, and both
    // representations carry a cheap integer for that: the arena class id, and the heap's class
    // word. Keyed on those, the name is built at most once per class.
    let verdict = match k {
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(p) => {
            let cid = crate::nextgen::arena_class_id(*p);
            crate::map::keys::identity_verdict_cid(vm, cid)
        }
        Value::Ref(h) => {
            let word = vm.heap.object_class_word(*h)?;
            crate::map::keys::identity_verdict_word(vm, word, *h)
        }
        _ => return None,
    };
    if !verdict {
        return None;
    }
    crate::map::keys::identity_key_pub(vm, k)
}

pub(crate) fn value_map_key(vm: &mut Vm, k: &Value) -> crate::heap::MapKey {
    // M5.1 STRUCTKEY SCAN: record object keys (see map_key).
    if crate::structkey_scan::enabled() && matches!(k, Value::Obj(_) | Value::Ref(_)) {
        let name = vm.value_class_name(k);
        crate::structkey_scan::note_mapkey(&name);
    }
    if let Value::Int(n) = k {
        return crate::heap::MapKey::Int(*n as i64);
    }
    // ⭐**THE STRING SHORTCUT WAS MISSING FROM THIS FUNNEL ALONE.** `map_key_inner`,
    // `value_map_key_in` and `cache_key` all consult `string_first` here; the BUILDER half did
    // not, so every string key that `Map.of`/`copyOf`/drain stores walked the whole
    // `structural_key` prologue — `KeyScope::enter`, the arena-map test, the per-class ArrayMap
    // admission chain, the record renderer, the pinned-mirror bridge, the funnel memo — to reach a
    // two-line arm at the BOTTOM of `key_str_at` (`Value::Str(s) => key_tagged("s:", s)`). O(that
    // prologue) per stored string key -> O(1).
    //
    // Safe against the other three by construction, and for the same reason `string_first`'s own
    // doc gives: this returns byte-for-byte the string that arm returns, so put and get still
    // agree — which is the invariant the paragraph below was written in blood for.
    //
    // PLACED AFTER the census block and the int arm, exactly as in `map_key_inner`, so
    // `RAVA_STRUCTKEY_SCAN` still notes the same population it noted before.
    if fast_keyroot() {
        if let Some(key) = string_first(k) {
            return key;
        }
    }
    // ⚠**THE SAME RULE MUST RUN HERE.** This is the BUILDER half — `Map.of`/`copyOf`/drain fill
    // through it while `map_key_inner` probes — so a class admitted to the identity rule on one
    // side and not the other keys differently for put and get. Measured: with the rule only in
    // `map_key_inner`, the boot died at `Unbound values in registry ... world_preset` with every
    // element reporting `Failed to parse`, which is what a registry map that can no longer find
    // its own entries looks like from the outside.
    if let Some(key) = identity_first(vm, k) {
        return key;
    }
    #[cfg(feature = "next-gen-gc")]
    if combo_hash_enabled() {
        if let Some(h) = combo_hash_key(vm, k) {
            return crate::heap::MapKey::Hash(h);
        }
    }
    crate::heap::MapKey::str(structural_key(vm, k))
}

#[cfg(feature = "next-gen-gc")]
pub(crate) enum MapKeyRoot {
    HeapArray(u32),
    GcProtect {
        base: usize,
        _scope: crate::stream::GpScope,
    },
    /// ⭐**A ROOT FOR A VALUE THAT CANNOT MOVE AND CANNOT BE COLLECTED.**
    ///
    /// The shield exists because `map_key` re-enters guest `toString()`, which allocates, which
    /// can trigger a moving arena collection — so the key and the value must be reachable from a
    /// root scan and re-read FORWARDED afterwards. That argument is about a *reference*. Of the
    /// eleven `Value` variants exactly three carry one: `Obj` (an arena pointer, which moves),
    /// `Ref` and `Array` (heap handles, which a collection may reclaim). The other eight —
    /// `Int`/`Long`/`Float`/`Double`/`Null`/`StdOut`/`StdErr` and `Str`, whose `RStr` is an `Arc`
    /// the `Value` itself owns — have nothing for the collector to trace, forward or free.
    ///
    /// So for `map.get("some/resource/path")` — a string key and an absent value argument, which
    /// is the shape of every `get`/`containsKey`/`remove` in a workload that keys by string
    /// everywhere — the old code pushed two `Value`s onto `vm.gc_protect`, built a raw-pointer
    /// scope guard, and read both back through bounds-checked `Vec` indexing plus a clone, to
    /// protect two things that were never in danger. (With `RAVA_MAP_KEY_GC_PROTECT=0` it was
    /// worse: a `Vec<Value>` **and** a heap `Object[]` object allocated and left as garbage, per
    /// map operation.) Holding them inline is the same two clones the caller already made and
    /// nothing else.
    ///
    /// REFUSES rather than guesses: if EITHER slot is `Obj`/`Ref`/`Array` the real root is taken,
    /// so a mixed pair (string key, object value) is shielded exactly as before.
    Inline(Value, Value),
}

/// Can a collection invalidate this `Value`'s meaning — by moving it or by reclaiming it?
///
/// The three referential variants only. Deliberately written as a positive list of what is
/// dangerous, so a future variant defaults to being rooted rather than to being skipped.
#[cfg(feature = "next-gen-gc")]
#[inline]
fn needs_gc_root(v: &Value) -> bool {
    matches!(v, Value::Obj(_) | Value::Ref(_) | Value::Array(_))
}

#[cfg(feature = "next-gen-gc")]
impl MapKeyRoot {
    pub(crate) fn new(vm: &mut Vm, key: Value, value: Value, use_gc_protect: bool) -> Self {
        if fast_keyroot() && !needs_gc_root(&key) && !needs_gc_root(&value) {
            return Self::Inline(key, value);
        }
        if use_gc_protect {
            let base = crate::vm::thread_context::root_len(&vm.gc_protect);
            crate::vm::thread_context::push_root(&mut vm.gc_protect, key);
            crate::vm::thread_context::push_root(&mut vm.gc_protect, value);
            Self::GcProtect {
                base,
                _scope: crate::stream::GpScope(&mut vm.gc_protect as *mut _, base),
            }
        } else {
            Self::HeapArray(vm.heap.alloc_values("Ljava/lang/Object;", vec![key, value]))
        }
    }

    pub(crate) fn get(&self, vm: &Vm, index: usize) -> Value {
        match self {
            Self::HeapArray(array) => vm.heap.array_get(*array, index).unwrap_or(Value::Null),
            Self::GcProtect { base, .. } => crate::vm::thread_context::root_get(
                &vm.gc_protect,
                base + index,
            )
            .unwrap_or(Value::Null),
            // Slots 0 and 1, `Null` past the end — the same shape `HeapArray` answers with.
            Self::Inline(k, v) => match index {
                0 => k.clone(),
                1 => v.clone(),
                _ => Value::Null,
            },
        }
    }
}

#[cfg(all(test, feature = "next-gen-gc"))]
mod map_key_root_tests {
    use super::MapKeyRoot;
    use crate::{ClassLoader, Value, Vm};

    /// REFERENTIAL values, so the real root is taken and the scope guard is exercised — the
    /// non-referential pair now takes the `Inline` arm and would never build one.
    #[test]
    pub(crate) fn gc_protect_root_reads_exact_slots_and_restores_depth() {
        let mut vm = Vm::new(ClassLoader::new());
        crate::vm::thread_context::push_root(&mut vm.gc_protect, Value::Int(3));
        let base = crate::vm::thread_context::root_len(&vm.gc_protect);
        {
            let root = MapKeyRoot::new(&mut vm, Value::Ref(7), Value::Array(11), true);
            assert!(matches!(root.get(&vm, 0), Value::Ref(7)));
            assert!(matches!(root.get(&vm, 1), Value::Array(11)));
            crate::vm::thread_context::push_root(&mut vm.gc_protect, Value::Int(13));
        }
        assert_eq!(
            crate::vm::thread_context::root_len(&vm.gc_protect),
            base
        );
        assert_eq!(
            crate::vm::thread_context::root_index(&vm.gc_protect, 0).as_int(),
            Some(3)
        );
    }

    #[test]
    pub(crate) fn heap_array_fallback_reads_the_same_slots() {
        let mut vm = Vm::new(ClassLoader::new());
        let root = MapKeyRoot::new(&mut vm, Value::Ref(7), Value::Array(11), false);
        assert!(matches!(root.get(&vm, 0), Value::Ref(7)));
        assert!(matches!(root.get(&vm, 1), Value::Array(11)));
    }

    /// The unrooted arm: a value with no reference in it reads back identically and takes NO
    /// `gc_protect` slot and NO heap array. `Str` is included because its `Arc` is owned by the
    /// `Value`, not by the collector.
    #[test]
    pub(crate) fn inline_root_takes_no_slot_and_reads_the_same_values() {
        let mut vm = Vm::new(ClassLoader::new());
        let before = crate::vm::thread_context::root_len(&vm.gc_protect);
        let root = MapKeyRoot::new(
            &mut vm,
            Value::Str(crate::value::RStr::from("minecraft:stone")),
            Value::Long(7),
            true,
        );
        assert_eq!(
            crate::vm::thread_context::root_len(&vm.gc_protect),
            before
        );
        match root.get(&vm, 0) {
            Value::Str(s) => assert_eq!(s.as_str(), "minecraft:stone"),
            other => panic!("slot 0 read back as {other:?}"),
        }
        assert_eq!(root.get(&vm, 1).as_long(), Some(7));
        assert!(matches!(root.get(&vm, 2), Value::Null));
    }
}
