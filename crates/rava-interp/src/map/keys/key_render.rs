//! Rendering a key from a mutable value, and the scoped structural key.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::map::keys::*;

/// `key_str_mut` with an optional GAP-P04 salt scope: `salt_mid = Some(map id)`
/// routes TOP-LEVEL value-equality objects through `salted_value_key` (guest
/// hashCode/equals — throws PROPAGATE). Under an active salt scope the
/// cross-map `$keystr` / arena key-memo caches are bypassed for value objects
/// (their salted keys are per-map). `None` is byte-identical to the historic
/// derivation and never errs.
/// SPLIT PROBE (`RAVA_RENDERSPLIT`, default OFF, inert): is the 1,089 ns average a POPULATION of
/// expensive renders, or a few expensive ones amortised over cheap repeats?
///
/// ⭐**Why the question is not rhetorical.** `RAVA_TOSTR_TIME` reads 3,800,000 renders at 1,089 ns
/// (ledger row 308) and `RAVA_KEYSHADOW` reads only **153,096 distinct handles** behind them. This
/// function already caches its answer on the object — `$keystr` is written at the end and probed
/// at the top — so a repeat should return early and cost almost nothing. If that is true, the 4.1 s
/// is ~150 k FIRST renders at ~25 us each, and the lever is those, not a memo. **Six mechanism
/// guesses in this hunt have already been refuted by measurement; this one is not going to be the
/// seventh by assertion.**
///
/// Buckets, by which exit the call took:
/// `early` = answered by a synthetic string field (`$str`/`$name`/`$path`/`$keystr`);
/// `full` = ran the whole path, which is where `value_to_string` lives.
#[cfg(feature = "next-gen-gc")]
pub(crate) mod rendersplit {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub(crate) static N: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
    pub(crate) static NS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

    pub(crate) fn enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("RAVA_RENDERSPLIT").is_some())
    }

    /// WHICH CLASSES pay the full render. The early bucket is served by `$keystr` at 251 ns and
    /// needs no names; the FULL bucket is 149,096 calls at 9,037 ns and is the only population
    /// left worth a lever, so the question is whether it is one class or four thousand.
    /// How LONG is the thing being rendered? 149,708 of the 149,096-odd full renders are one class,
    /// `DoubleArrayList`, at 8,663 ns each. Formatting a double is tens of nanoseconds, so the
    /// element COUNT decides whether the cost is the formatting or something around it — and that
    /// decides the design. Buckets: <4, <16, <64, <256, >=256.
    /// ⚠**SEPARATE BUCKETS, because the first version of this counter had a FALLBACK and reported
    /// both quantities under one label** (ledger row 316). It read `array_len($elems)` when that
    /// field was an array and `key.len()` otherwise — and every render that reaches this code got
    /// here through the arm that runs only when the `$elems` probe FAILS, so it always measured
    /// CHARACTERS while being read as ELEMENTS for four rows. `kind` is now explicit: 0 = element
    /// count, 1 = key characters.
    pub(crate) fn note_len_kind(kind: usize, n: usize) {
        static K: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
        static KN: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
        K[kind].fetch_add(n as u64, Relaxed);
        let c = KN[kind].fetch_add(1, Relaxed) + 1;
        if c % 50_000 == 0 {
            eprintln!(
                "[RENDERLEN2] elems: n={} sum={} | chars: n={} sum={}",
                KN[0].load(Relaxed), K[0].load(Relaxed),
                KN[1].load(Relaxed), K[1].load(Relaxed)
            );
        }
    }

    /// Time `value_to_string` alone inside the full-render arm. Row 316 left the 9,037 ns
    /// unexplained after the "2.15 M doubles" reading turned out to be characters; this asks the
    /// one remaining question directly instead of dividing one number by another again.
    /// How many `java_dtoa` calls happen INSIDE one `value_to_string` window. Row 317's open
    /// question, answered by counting rather than by dividing one total by another.
    pub(crate) fn note_vts_dtoa(k: u64) {
        static SUM: AtomicU64 = AtomicU64::new(0);
        static N: AtomicU64 = AtomicU64::new(0);
        static ZERO: AtomicU64 = AtomicU64::new(0);
        SUM.fetch_add(k, Relaxed);
        if k == 0 {
            ZERO.fetch_add(1, Relaxed);
        }
        let n = N.fetch_add(1, Relaxed) + 1;
        if n % 50_000 == 0 {
            eprintln!(
                "[VTS-DTOA] {n} renders, {} dtoa calls inside them ({:.2} per render), {} renders made ZERO",
                SUM.load(Relaxed),
                SUM.load(Relaxed) as f64 / n as f64,
                ZERO.load(Relaxed)
            );
        }
    }

    pub(crate) fn note_vts(ns: u64) {
        static NS: AtomicU64 = AtomicU64::new(0);
        static N: AtomicU64 = AtomicU64::new(0);
        NS.fetch_add(ns, Relaxed);
        let n = N.fetch_add(1, Relaxed) + 1;
        if n % 50_000 == 0 {
            let t = NS.load(Relaxed);
            eprintln!(
                "[VTS] {n} value_to_string calls, {:.3}s, {} ns avg",
                t as f64 / 1e9,
                t / n
            );
        }
    }

    pub(crate) fn note_len(n: usize) {
        static B: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
        static TOTAL: AtomicU64 = AtomicU64::new(0);
        static ELEMS: AtomicU64 = AtomicU64::new(0);
        let b = match n { 0..=3 => 0, 4..=15 => 1, 16..=63 => 2, 64..=255 => 3, _ => 4 };
        B[b].fetch_add(1, Relaxed);
        ELEMS.fetch_add(n as u64, Relaxed);
        let t = TOTAL.fetch_add(1, Relaxed) + 1;
        if t % 50_000 == 0 {
            let v: Vec<u64> = B.iter().map(|a| a.load(Relaxed)).collect();
            eprintln!(
                "[RENDERLEN] {t} renders, {} elements ({:.1}/render) | n<4:{} n<16:{} n<64:{} n<256:{} n>=256:{}",
                ELEMS.load(Relaxed),
                ELEMS.load(Relaxed) as f64 / t as f64,
                v[0], v[1], v[2], v[3], v[4]
            );
        }
    }

    pub(crate) fn note_class(cls: &str) {
        static M: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, u64>>> =
            std::sync::OnceLock::new();
        let m = M.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()));
        let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
        *g.entry(cls.to_string()).or_insert(0) += 1;
        let total: u64 = g.values().sum();
        if total % 50_000 == 0 {
            let mut rows: Vec<(&String, &u64)> = g.iter().collect();
            rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
            let head: Vec<String> = rows
                .iter()
                .take(8)
                .map(|(c, n)| format!("{n} {c}"))
                .collect();
            eprintln!(
                "[RENDERCLASS] {total} full renders over {} distinct classes | {}",
                g.len(),
                head.join(" · ")
            );
        }
    }

    /// 0 = early exit, 1 = full render.
    pub(crate) fn note(bucket: usize, ns: u64) {
        NS[bucket].fetch_add(ns, Relaxed);
        let n = N[bucket].fetch_add(1, Relaxed) + 1;
        if bucket == 0 && n % 500_000 == 0 {
            let (ne, nf) = (N[0].load(Relaxed), N[1].load(Relaxed));
            let (se, sf) = (NS[0].load(Relaxed), NS[1].load(Relaxed));
            eprintln!(
                "[RENDERSPLIT] early n={ne} {:.3}s ({} ns avg) | full n={nf} {:.3}s ({} ns avg)",
                se as f64 / 1e9, se / ne.max(1),
                sf as f64 / 1e9, sf / nf.max(1)
            );
        }
    }
}

/// `format!("{tag}{body}")` with the allocation sized EXACTLY once.
///
/// Every key built here is immediately turned into a `MapKey::Str`, i.e. `String::into_boxed_str`,
/// which calls `shrink_to_fit`: a `format!` result whose capacity overshoots its length therefore
/// pays a SECOND allocation and a second copy of the whole key before the map ever hashes it.
/// Sizing the buffer to `tag.len() + body.len()` makes the shrink a no-op. Byte-identical output;
/// `RAVA_NO_FAST_MAPKEY=1` restores the `format!`, so the pair prices as a same-binary A/B.
#[inline]
pub(crate) fn key_tagged(tag: &str, body: &str) -> String {
    if !fast_mapkey() {
        return format!("{tag}{body}");
    }
    let mut out = String::with_capacity(tag.len() + body.len());
    out.push_str(tag);
    out.push_str(body);
    out
}

/// The historic 3-argument entry point, kept for callers outside this module (`map::key_str_mut`),
/// which have no probe to hand it.
pub(crate) fn key_str_mut_in(vm: &mut Vm, v: &Value, salt_mid: Option<u32>) -> Result<String, VmError> {
    key_str_mut_named(vm, v, salt_mid, NamedProbe::Unprobed)
}

pub(crate) fn key_str_mut_named(
    vm: &mut Vm,
    v: &Value,
    salt_mid: Option<u32>,
    named: NamedProbe,
) -> Result<String, VmError> {
    // ⭐**A RECORD IS RENDERED FROM ITS COMPONENTS, NOT BY RUNNING ITS `toString()`.**
    //
    // The guest-`toString()` path below is, for a `java.lang.Record`, the generated
    // `ObjectMethods.bootstrap` indy recursing through every component in GUEST code.
    // `RAVA_KEYTIME`'s slow-key histogram prices it on a product boot:
    //   288 renders of `datafixers.types.templates.Sum`         3,147,677 ns each = 0.907 s
    //   832 renders of `DSL$Instances$TaggedChoiceCacheKey`       720,892 ns each = 0.600 s
    // **1.5 s of a ~40 s boot in 1,120 renders.** A record's `equals` is component-wise, so a
    // component-wise render is exactly as discriminating — and it runs in OUR renderer, where
    // every nested object meets the funnel memo instead of re-entering guest bytecode.
    //
    // Admission is a BYTECODE property (`extends java/lang/Record`), the same kind of evidence
    // the identity rule uses for `ACC_ENUM`; no workload, package or class name is matched.
    //
    // ⚠**THE FIELD ID IS THE QUALIFIED NAME.** Four instrumented attempts died before this
    // worked, the last two on the id alone: `field_id_opt("f")` refused 23,379 of 23,387 records
    // because the bare component name is never interned, and interning it then failed
    // `slot_of` on all 23,379 — because the arena layout stores
    // `com/mojang/datafixers/types/templates/Sum#f`, not `f` (`RAVA_RECORD_LAYOUT` prints both
    // lists side by side). `slot_of` stays as the safety check: `arena_getfield` PANICS on a
    // field outside the layout, so a refusal must fall through to the guest path, never guess.
    //
    // ⚠**AND IT BELONGS HERE, BELOW THE MEMO — THE FIRST PLACEMENT COST 20% OF `Done`.** Put in
    // `structural_key_scoped_inner` it ran AHEAD of `key_str_memoed`'s per-object cache, so all
    // 106,582 record renders became fresh walks where most had been memo hits:
    //   renderer ON (above the memo)  Done 0.557 0.583 0.627
    //   renderer OFF                  Done 0.457 0.466 0.499
    // Every run worse, while the 1.5 s of catastrophic renders it removed showed up as nothing
    // on the wall. `key_str_mut_in` is reached only on a memo MISS, so here the cheap repeats
    // keep the cache and only a first render pays the walk.
    #[cfg(feature = "next-gen-gc")]
    if salt_mid.is_none() && record_render_enabled() {
        if let Some(out) = record_component_key(vm, v)? {
            return Ok(out);
        }
    }
    #[cfg(feature = "next-gen-gc")]
    let rs_t = rendersplit::enabled().then(std::time::Instant::now);
    let salting = salt_mid.is_some() && mapkey_salt_enabled();
    // B5 MAP-KEY BRIDGE (top-level twin of key_str's): a pinned mirror pointer
    // keys exactly as its handle. Inert until pinned pointers flow.
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = v {
        if let Some(h) = crate::nextgen::pinned_handle_of(p.addr()) {
            return key_str_mut_named(vm, &Value::Ref(h), salt_mid, NamedProbe::Unprobed);
        }
    }
    if let Value::Ref(h) = v {
        let h = *h;
        // ⭐**THE SAME THREE FIELDS WERE READ TWICE PER KEY.** `structural_key_scoped_inner`
        // reads `$str`/`$name`/`$path` to decide whether this handle is a drainable map, then
        // throws the values away and this arm reads all three again. A `$` field read is
        // `Heap::get_field_by_id`, whose own comment records what it is: "a JVM reads a field at a
        // constant offset computed once at class-load time; this walks the object's id array until
        // it matches" — a LINEAR SCAN, and `RAVA_STRUCTKEY_SCAN`'s dominant key class carries 32
        // fields. Three duplicate scans is ~96 wasted comparisons on every handle map key.
        //
        // `NamedProbe` carries the first reading down here. It is consulted at exactly the point
        // the probes used to run — after the record renderer, after the pinned-mirror bridge — so
        // no arm can answer earlier or later than it did, and `$str` still outranks `$name` which
        // still outranks `$path`.
        match named {
            NamedProbe::Key(k) => {
                #[cfg(feature = "next-gen-gc")]
                if let Some(t) = rs_t { rendersplit::note(0, t.elapsed().as_nanos() as u64); }
                return Ok(k);
            }
            // Already read, already absent: skip the three scans below.
            NamedProbe::Absent => {}
            NamedProbe::Unprobed => {
                // A heap String keys identically to a Value::Str (see key_str).
                if let Value::Str(s) = vm.heap.get_field_syn(h, crate::heap::Syn::Str) {
                    #[cfg(feature = "next-gen-gc")]
                    if let Some(t) = rs_t { rendersplit::note(0, t.elapsed().as_nanos() as u64); }
                    return Ok(key_tagged("s:", s.as_str()));
                }
                for f in [crate::heap::Syn::Name, crate::heap::Syn::Path] {
                    if let Value::Str(s) = vm.heap.get_field_syn(h, f) {
                        #[cfg(feature = "next-gen-gc")]
                        if let Some(t) = rs_t { rendersplit::note(0, t.elapsed().as_nanos() as u64); }
                        return Ok(key_tagged("o:", s.as_str()));
                    }
                }
            }
        }
        // GAP-P04: the $keystr cache is CROSS-map ("o:{toString}") — a salted map
        // must not consult it for what may be a value object (mode mixing).
        if !salting {
            if let Value::Str(s) = vm.heap.get_field_syn(h, crate::heap::Syn::KeyStr) {
                #[cfg(feature = "next-gen-gc")]
                if let Some(t) = rs_t { rendersplit::note(0, t.elapsed().as_nanos() as u64); }
                return Ok(s.to_string());
            }
        }
        // $map/$elems-backed keys keep the structural path in key_str.
        let structural = matches!(vm.heap.get_field_syn(h, crate::heap::Syn::Map), Value::Int(_))
            || matches!(vm.heap.get_field_syn(h, crate::heap::Syn::Elems), Value::Array(_));
        if !structural {
            // Only an object that OVERRIDES equals has value identity — key those
            // by toString() (Identifier, ResourceKey, records). An identity object
            // (no equals override, e.g. Block) MUST stay handle-keyed: it matches
            // Java's identity equals AND avoids recursion when its toString itself
            // performs a registry lookup keyed by the same object (Block.toString
            // → MappedRegistry.wrapAsHolder → … → Block.toString).
            // ⭐**THE PREDICATE IS ANSWERED FROM THE CLASS WORD, NOT FROM A FRESH NAME STRING.**
            // This built a ~50-character heap `String` for the class name and then handed it to
            // `has_value_equality`, whose memo hashes all fifty characters — per key derivation,
            // for a `bool`. That is the exact defect `value_equality_by_cid` was written to remove
            // on the ARENA side ("no name allocation, no name hash"); the handle side kept paying
            // it. `RAVA_KEYRENDER_STAT` counts 749,996 handle derivations a boot reaching here
            // with the render memo REJECTING them, so almost none of these were amortised.
            //
            // The name is still built — inside the three arms below that actually need it, all
            // three of which are off by default.
            // Read out of the scrutinee first: a `vm.heap.…` temporary in a match head keeps the
            // shared borrow alive for the whole match, and the arms need `&mut Vm`.
            let class_word = vm.heap.object_class_word(h);
            let value_obj = match (fast_mapkey(), class_word) {
                (true, Some(word)) => value_equality_by_word(vm, word, h),
                // No class word (or the fast path is off): the historic form, unchanged.
                _ => {
                    let actual = key_class_name(vm, h);
                    has_value_equality(vm, &actual)
                }
            };
            // GAP-P04: a salted map keys value objects by (class, hashCode, salt) —
            // per-map, so NEVER cached in $keystr. Ineligibility (Ok(None)) is
            // deterministic per class → the toString fallback below stays consistent.
            if value_obj && salting {
                if let Some(mid) = salt_mid {
                    let actual = key_class_name(vm, h);
                    if let Some(key) = salted_value_key(vm, mid, v, &actual)? {
                        return Ok(key);
                    }
                }
            }
            // RAVA_MAPKEY_HASHCODE parity: the Ref arm must key the SAME way as the arena-Obj
            // arm — a class living in both representations otherwise splits its keys ("o:g…"
            // vs "o:…"), the exact s:/o: mismatch bug class. (Do NOT cache $keystr under the
            // gate: hashCode-keys stay derivation-only.)
            #[cfg(feature = "next-gen-gc")]
            if value_obj && mapkey_hashcode_enabled() {
                let actual = key_class_name(vm, h);
                if let Some(decl) = vm.resolve_method(&actual, "hashCode", "()I")? {
                    if let Ok(Some(Value::Int(hc))) =
                        vm.invoke(decl, "hashCode", "()I", vec![v.clone()])
                    {
                        return Ok(format!("o:gR{actual}:{hc}"));
                    }
                }
            }
            let key = if value_obj {
                #[cfg(feature = "next-gen-gc")]
                let vts_t = rs_t.map(|_| (std::time::Instant::now(), crate::value::dtoa_calls()));
                let r = vm.value_to_string(v);
                #[cfg(feature = "next-gen-gc")]
                if let Some((t, d0)) = vts_t {
                    rendersplit::note_vts(t.elapsed().as_nanos() as u64);
                    rendersplit::note_vts_dtoa(crate::value::dtoa_calls().saturating_sub(d0));
                }
                match r {
                    Ok(s) => key_tagged("o:", &s),
                    _ => format!("@{h}"),
                }
            } else {
                format!("@{h}")
            };
            vm.heap.put_field_syn(h, crate::heap::Syn::KeyStr, Value::Str(key.clone().into()));
            #[cfg(feature = "next-gen-gc")]
            if let Some(t) = rs_t {
                rendersplit::note(1, t.elapsed().as_nanos() as u64);
                rendersplit::note_class(&key_class_name(vm, h));
                // The rendered length, read from the backing store the natives keep for this
                // collection. `$elems` is the array every native-backed list carries.
                if let Value::Array(a) = vm.heap.get_field_syn(h, crate::heap::Syn::Elems) {
                    rendersplit::note_len_kind(0, vm.heap.elems_len(h, a));
                } else {
                    rendersplit::note_len_kind(1, key.len());
                }
            }
            return Ok(key);
        }
    }
    // Arena object key (next-gen-gc): a value type (overrides equals — Identifier/
    // MinecraftKey/ResourceKey) keys by its toString() so equal-by-value keys
    // collide exactly like the handle path; a pure identity object keys by its
    // GC-stable identity hash. Reading the arena object's strings happens inside
    // value_to_string (its toString bytecode runs over arena getfields).
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = v {
        // An ArenaString must key IDENTICALLY to a Value::Str ("s:content", exactly
        // like the immutable key_str arm) — NOT through the value-equality toString
        // path, whose "o:content" prefix diverges. A compiled method's ldc interns a
        // PERM-pool ArenaString, so a map populated by COMPILED code keyed "o:…"
        // while the interpreter's Value::Str lookups keyed "s:…": every entry became
        // invisible to the other side (th=128: V1510's rename `map.put(new,
        // map.remove(old))` got null — the old key was stored under "o:").
        if crate::nextgen::is_arena_string(*p) {
            return Ok(key_tagged("s:", &String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p))));
        }
        // IDENTITY TOKEN for a value whose class proves equality is identity — see
        // `keyroot::identity_token`. Placed after the string arm (a string's equality is by
        // VALUE and its class is native-served, so it is refused anyway) and before everything
        // that walks fields.
        if let Some(tok) = crate::map::keyroot::identity_token(vm, v) {
            return Ok(tok);
        }
        let cid = crate::nextgen::arena_class_id(*p);
        // The PREDICATE is answered from the class id — no name allocation, no name hash — and the
        // name itself is materialised only inside the arm that needs it, which `RAVA_KEYSTAT` puts
        // at 20,766 of 3,500,000 derivations. See `value_equality_by_cid`.
        if value_equality_by_cid(vm, cid) {
            let actual = crate::nextgen::class_name(cid)
                .map(|a| a.to_string())
                .unwrap_or_else(|| "java/lang/Object".to_string());
            // GAP-P04: same salted derivation as the Ref arm (representation parity);
            // the cross-map VALKEY memo below is bypassed while salting.
            if salting {
                if let Some(mid) = salt_mid {
                    if let Some(key) = salted_value_key(vm, mid, v, &actual)? {
                        return Ok(key);
                    }
                }
            }
            keystat("valobj_tostr");
            // RAVA_MAPKEY_HASHCODE (DIAGNOSTIC SCAFFOLD — ⚠ FAILS the bit-exact gate, never
            // default): key value objects by their guest `hashCode()` int instead of the
            // toString proxy. MEASURED on the real boot: the pathological structural keys are
            // FRESH worldgen density trees + DFU type templates probed against dedup caches —
            // toString builds 9KB-362KB strings PER KEY (VALUEOF probe), no memo can help
            // (fresh objects). hashCode keying: boot-to-Done 259.5s → 231.1s (−28.4s / 11%) —
            // the PRIZE CEILING of any cheap-key scheme. BUT ChunkFill_HASH diverged
            // (aa01a18a186d0001 ≠ 8790f04ace726ecf): Java's `31*a+b` hashCodes on structured
            // worldgen params (Climate/Noise grids) COLLIDE in practice, aliasing UNEQUAL
            // functions — measured, not theoretical. A sound version MUST equals-verify every
            // index hit (per-map salt chains — see the rava-valueof-mapkey-verdict design memo).
            // Kept as the mechanical scaffold for that work (both-arm parity pattern proven;
            // full-identity keying dies even earlier — datapack load needs value semantics).
            if mapkey_hashcode_enabled() {
                if let Some(decl) = vm.resolve_method(&actual, "hashCode", "()I")? {
                    if let Ok(Some(Value::Int(h))) =
                        vm.invoke(decl, "hashCode", "()I", vec![v.clone()])
                    {
                        // class NAME token (not cid): must match the Ref-arm key exactly.
                        return Ok(format!("o:gR{actual}:{h}"));
                    }
                }
                // resolve/invoke failure → fall through to the exact toString path below.
            }
            // MEMOIZE the value-object's toString key per GC epoch. Property/Identifier
            // singletons are keyed millions of times (KEYSTAT: 38% of structural_key);
            // toString() is exact but expensive (guava MoreObjects + iterates
            // getPossibleValues). The memo is keyed by arena ADDRESS, tagged with the GC
            // epoch: within an epoch addresses are unique among live objects and the bump
            // allocator never reuses one (no free without a collection), so a hit is
            // ALWAYS the same object. A collection bumps the epoch → entries miss and
            // recompute (no stale across a move). Exact semantics, no abort.
            let epoch = crate::nextgen::gc_epoch();
            if !keymemo_disabled() && !salting {
                if let Some(cached) = super::valkey::get(*p, epoch) {
                    keystat("memo_hit");
                    return Ok(cached);
                }
            }
            keystat("tostr_real"); // the memo missed — the guest toString below really runs
            if let Ok(s) = vm.value_to_string(v) {
                let key = key_tagged("o:", &s);
                if keymemo_disabled() || salting {
                    return Ok(key);
                }
                // Only cache if NO collection ran during value_to_string (it allocates,
                // so it can GC): a collection would have moved `p`, making `p.addr()`
                // stale → caching it risks a later alias to a reused address. On a GC
                // mid-toString, skip the put (still correct, just no memo for this one).
                if crate::nextgen::gc_epoch() == epoch {
                    super::valkey::put(*p, &key, epoch);
                }
                return Ok(key);
            }
        }
        return Ok(format!("@a{}", crate::nextgen::arena_identity_hash(*p)));
    }
    Ok(key_str(vm, v))
}

/// TIMER for the guest-`toString()` arm (`RAVA_TOSTR_TIME`, default OFF).
///
/// The non-combo path costs 3,028 ns per derivation and the working assumption is that the guest
/// `toString()` under `key_str_mut_in` is all of it. That assumption has not been measured, and
/// unmeasured mechanism claims have cost three builds in this hunt already.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn tostring_timed(
    vm: &mut Vm,
    v: &Value,
    salt_mid: Option<u32>,
    named: NamedProbe,
) -> Result<String, VmError> {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_TOSTR_TIME").is_some()) {
        return key_str_memoed(vm, v, salt_mid, named);
    }
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static NS: AtomicU64 = AtomicU64::new(0);
    static N: AtomicU64 = AtomicU64::new(0);
    let t0 = std::time::Instant::now();
    let r = key_str_memoed(vm, v, salt_mid, named);
    let ns = NS.fetch_add(t0.elapsed().as_nanos() as u64, Relaxed)
        + t0.elapsed().as_nanos() as u64;
    let n = N.fetch_add(1, Relaxed) + 1;
    if n % 200_000 == 0 {
        eprintln!("[TOSTR] {n} renders, {:.3}s, {} ns avg", ns as f64 / 1e9, ns / n);
    }
    r
}

pub(crate) fn structural_key_scoped(vm: &mut Vm, v: &Value, salt_mid: Option<u32>) -> Result<String, VmError> {
    #[cfg(feature = "next-gen-gc")]
    if salt_mid.is_none() && crate::map::keys::arena_keymemo_enabled() {
        if let Value::Obj(p) = v {
            if let Some(hit) = crate::map::keys::arena_funnel_get(vm, *p) {
                return Ok(hit);
            }
            let epoch = crate::nextgen::gc_epoch();
            let out = structural_key_scoped_inner(vm, v, salt_mid)?;
            // Key derivation may run guest `toString`, allocate, and move `p`.
            // Never read identity/class metadata through the pre-GC pointer; a
            // missed memo fill is harmless and the next lookup recomputes it.
            if crate::nextgen::gc_epoch() == epoch {
                crate::map::keys::arena_funnel_put(vm, *p, &out);
            }
            return Ok(out);
        }
    }
    structural_key_scoped_inner(vm, v, salt_mid)
}

fn structural_key_scoped_inner(vm: &mut Vm, v: &Value, salt_mid: Option<u32>) -> Result<String, VmError> {
    // The ONE funnel every stored map key passes through, so it is where a
    // key-derivation scope belongs: inside it nested `toString` renders collapse to
    // interned tokens (`RAVA_KEY_INTERN`), and outside it every render is verbatim.
    let _keying = crate::key_intern::KeyScope::enter();
    #[cfg(feature = "next-gen-gc")]
    keystat("total");
    // ⭐**THE MEMO BELONGS HERE**, at the funnel this function's own doc comment names — not at
    // `key_str_memoed`, which several of the paths below never pass through. Wired there first,
    // it served 1,968,071 renders (65.6% hit) and **the timed derivations did not move**,
    // because the dominant one does not go that way.
    //
    // Keyed on the arena object's identity stamp: a MONOTONIC header counter, unique for the
    // process lifetime and carried through evacuation. Invalidated by the same tag the handle
    // memo uses. A memo returns the SAME string the renderer would, so unlike keying by identity
    // it cannot disagree with any other derivation path.

    // An arena HashMap used AS a map key (block-state combos, …): drain the flat
    // table into the SAME order-independent `m:{...}` form the `$map` path produces,
    // so two equal maps with different bucket layouts collide. No `$skey` memo (an
    // arena map has no heap field) — recomputed, which is fine for the rare case.
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = v {
        if crate::arena_map::enabled() && crate::arena_map::is_arena_map(*p) {
            // `entries` is a Rust-owned snapshot, so the collector cannot forward arena
            // pointers in it. Move the pairs into the scanned root set before a nested render
            // can collect, then re-read each slot immediately before rendering that side.
            let entries = crate::arena_map::entries(*p);
            let base = crate::vm::thread_context::root_len(&vm.gc_protect);
            let _scope = crate::stream::GpScope(&mut vm.gc_protect as *mut _, base);
            crate::vm::thread_context::reserve_roots(
                &mut vm.gc_protect,
                entries.len().saturating_mul(2),
            );
            for (k, val) in entries {
                crate::vm::thread_context::push_root(&mut vm.gc_protect, k);
                crate::vm::thread_context::push_root(&mut vm.gc_protect, val);
            }
            let entry_count =
                (crate::vm::thread_context::root_len(&vm.gc_protect) - base) / 2;
            let mut parts = Vec::with_capacity(entry_count);
            for i in 0..entry_count {
                let k = crate::vm::thread_context::root_index(&vm.gc_protect, base + i * 2);
                let rendered_key = structural_key(vm, &k);
                let val =
                    crate::vm::thread_context::root_index(&vm.gc_protect, base + i * 2 + 1);
                let rendered_val = structural_key(vm, &val);
                parts.push(format!("{rendered_key}={rendered_val}"));
            }
            parts.sort();
            return Ok(map_key_braced(&parts));
        }
    }
    // A JAR array-backed map used AS a map key — block-state combo maps are
    // fastutil `Reference2ObjectArrayMap<Property,Comparable>`, allocated as arena
    // `Value::Obj`. Drain its packed `key`/`value`/`size` arena fields DIRECTLY into
    // the same order-independent `m:{...}` form, instead of falling through to
    // key_str_mut → value_to_string → the map's `toString()` bytecode (which
    // iterates `entrySet()` — a ~42% block-bootstrap interpreter hotspot — and
    // stringifies each Property key through guava `MoreObjects`). Gated on an
    // `ArrayMap` class name (entries are packed in `[0..size]`; a hash map's arrays
    // are bucketed with null gaps, so it must keep the generic path) and on every
    // field being in the class layout (an off-layout `arena_getfield` panics).
    #[cfg(feature = "next-gen-gc")]
    if let Value::Obj(p) = v {
        let cid = crate::nextgen::arena_class_id(*p);
        // MEMOISED PER CLASS ID. Everything the admission test needs - the class NAME, the
        // "ArrayMap" substring scan, the three field-id resolutions and the three layout checks -
        // is a pure function of `cid`, yet it ran on EVERY key derivation: one fresh `String` from
        // `class_name(..).to_string()`, one substring scan, three by-name resolutions. Verified
        // caller attribution puts this chain at the top of the post-shortcut profile
        // (`field_id_existing` 3.4% reached 93.5% from `Heap::get_field`, `memchr` 3.8-4.2%,
        // `mi_malloc_aligned` 3.6-6.6%), and the identical shape - `is_collection` scanning class
        // names per `new` - already paid min -7.4% once in this tree.
        //
        // `None` caches "not an ArrayMap, or the layout lacks one of the three", so a negative is
        // as cheap as a positive. Field ids and arena layouts are stable for a class id, so the
        // answer cannot go stale.
        let ids = arraymap_ids(vm, cid);
        if let Some((kid, vid, sid)) = ids {
            keystat("arraymap");
            {
                if let (Value::Array(ka), Value::Array(va)) = (
                    crate::opcodes::arena_getfield(*p, kid),
                    crate::opcodes::arena_getfield(*p, vid),
                ) {
                    // ⭐**TWO WHOLE-ARRAY CLONES TO READ `size` ELEMENTS.** `array_values`
                    // materialises a `Vec<Value>` copy of the entire backing array — twice, keys
                    // and values — before a single entry is rendered, and `Value` clones are
                    // refcount traffic, not memcpy. `combo_hash_arena_array_map` fixed exactly
                    // this defect on the HASH half of the same data structure ("allocates THREE
                    // `Vec`s per call ... purely so a hash can be computed over them") and the
                    // structural half kept it. Reading element by element is the same fix, and
                    // the same one `bitset_words` paid 1,780 -> 1,085 ns for.
                    //
                    // Identical bounds: `array_len` fails exactly where `array_values` failed, so
                    // an unreadable array still yields `n == 0` and the empty `m:{}` key.
                    let (kn, vn) = if fast_mapkey() {
                        (vm.heap.array_len(ka).unwrap_or(0), vm.heap.array_len(va).unwrap_or(0))
                    } else {
                        (
                            vm.heap.array_values(ka).unwrap_or_default().len(),
                            vm.heap.array_values(va).unwrap_or_default().len(),
                        )
                    };
                    let size = match crate::opcodes::arena_getfield(*p, sid) {
                        Value::Int(s) => Some(s),
                        _ => None,
                    };
                    let n = arraymap_entry_count(size, kn, vn);
                    let mut parts: Vec<String> = Vec::with_capacity(n);
                    for i in 0..n {
                        let k = vm.heap.array_get(ka, i).unwrap_or(Value::Null);
                        let val = vm.heap.array_get(va, i).unwrap_or(Value::Null);
                        // Preserve the packed map's key-then-value observation order. Both
                        // values must be rooted before either render can collect; the value is
                        // then re-read from its forwarded slot after rendering the key.
                        let base = crate::vm::thread_context::root_len(&vm.gc_protect);
                        let _scope = crate::stream::GpScope(&mut vm.gc_protect as *mut _, base);
                        crate::vm::thread_context::push_root(&mut vm.gc_protect, k);
                        crate::vm::thread_context::push_root(&mut vm.gc_protect, val);
                        let rooted_key = crate::vm::thread_context::root_index(
                            &vm.gc_protect,
                            base,
                        );
                        let rendered_key = structural_key(vm, &rooted_key);
                        let rooted_val = crate::vm::thread_context::root_index(
                            &vm.gc_protect,
                            base + 1,
                        );
                        let rendered_val = structural_key(vm, &rooted_val);
                        parts.push(format!("{rendered_key}={rendered_val}"));
                    }
                    parts.sort();
                    return Ok(map_key_braced(&parts));
                }
            }
        }
    }
    // Read once here, consumed by `key_str_mut_in` at the point it used to read them itself.
    // Stays `Unprobed` for every non-handle value, so the arena arms are byte-for-byte untouched.
    let mut named = NamedProbe::Unprobed;
    if let Value::Ref(h) = v {
        // Plain value-wrappers (have $name/$str/$path) keep the cheap path.
        // BY ID. These three synthetic names were hashed and probed under the field-interner's
        // RwLock on EVERY key derivation - three per call, plus `$skey` below - which is what puts
        // `Heap::field_id_existing` at 3.4% of the serial window with 93.5% of it reached from
        // `Heap::get_field`. The names are fixed, so resolve once. An unresolved id means NO object
        // carries that field yet, so it cannot match `Value::Str` either - treating it as absent is
        // what the by-name probe would have returned.
        // ⭐**THE PROBE IS KEPT, NOT THROWN AWAY.** This block used to answer a `bool` and
        // discard the three values it read; `key_str_mut_in` then read the SAME three fields again
        // to build the key from them. `Heap::get_field_by_id` is a linear scan of the object's id
        // array, so that was three redundant scans per handle map key on top of the four this
        // function still makes.
        //
        // Two things had to hold for the answer to be carryable, and both do:
        //  * ORDER. `key_str_mut_in` ranks `$str` above `$name` above `$path`; the old loop ran
        //    `$name` first, but since it only produced a `bool` the order was invisible. Probing
        //    in the renderer's order and building the renderer's bytes is byte-identical.
        //  * PLACE. The carried answer is consulted where the probes used to run — after the
        //    record renderer and after the pinned-mirror bridge — so nothing answers earlier.
        //    The record renderer's placement is load-bearing: hoisting it above the render memo
        //    once cost 20% of `Done`, and this does not move it.
        //
        // `$skey` below is skipped exactly when it was skipped before (a named object is not a
        // drainable map), so the drain path is untouched.
        let probe = {
            // Cache each id ONLY ONCE RESOLVED. `get_or_init` over an array would freeze a `None`
            // taken before the name was ever interned, and every object created afterwards would
            // then be misclassified - a silent behaviour change the gates need not reproduce.
            // While unresolved this re-asks, which is exactly what the by-name probe did.
            static STR_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            static NAME_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            static PATH_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            let mut found: NamedProbe = NamedProbe::Absent;
            for (cell, nm, tag) in [
                (&STR_ID, "$str", "s:"),
                (&NAME_ID, "$name", "o:"),
                (&PATH_ID, "$path", "o:"),
            ] {
                let id = match cell.get() {
                    Some(&id) => Some(id),
                    None => match vm.heap.field_id_opt(nm) {
                        Some(id) => {
                            let _ = cell.set(id);
                            Some(id)
                        }
                        None => None,
                    },
                };
                if let Some(id) = id {
                    if let Value::Str(sv) = vm.heap.get_field_hint(*h, id, 0).0 {
                        found = NamedProbe::Key(key_tagged(tag, sv.as_str()));
                        break;
                    }
                }
            }
            found
        };
        // The identical `bool` the discarded loop produced: any of the three holding a `Value::Str`.
        // Independent of the kill switch, so the `$skey` drain decision cannot move with the flag.
        let has_named = matches!(probe, NamedProbe::Key(_));
        // Only the CARRYING is switchable: off, `key_str_mut_in` reads the three fields again as
        // it always did, which is exactly the form this replaces.
        if fast_mapkey() {
            named = probe;
        }
        if !has_named {
            // Draining a (jar) map via its iterator() bytecode is O(entries) of
            // interpreter work; block-state neighbour building calls this for
            // every lookup, so memoise the result on a `$skey` field. Map
            // mutators (put/remove/clear) clear it.
            // Same rule: cache only a RESOLVED id, never a `None`.
            static SKEY_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
            let skey_id = match SKEY_ID.get() {
                Some(&id) => Some(id),
                None => match vm.heap.field_id_opt("$skey") {
                    Some(id) => {
                        let _ = SKEY_ID.set(id);
                        Some(id)
                    }
                    None => None,
                },
            };
            let skey_val = match skey_id {
                Some(id) => vm.heap.get_field_hint(*h, id, 0).0,
                None => Value::Null,
            };
            if let Value::Str(s) = skey_val {
                return Ok(s.to_string());
            }
            if let Ok(Some(entries)) = vm.drain_map(*h) {
                let mut parts: Vec<String> = entries.iter()
                    .map(|(k, val)| format!("{}={}", structural_key(vm, k), structural_key(vm, val)))
                    .collect();
                parts.sort();
                let key = map_key_braced(&parts);
                vm.heap.put_field_syn(*h, crate::heap::Syn::SKey, Value::Str(key.clone().into()));
                return Ok(key);
            }
        }
    }
    // ⛔**FOURTH REFUSAL ON THE 1.5 s MAP-KEY LINE, AND THIS ONE IS A FACT ABOUT THE ARENA.**
    //
    // The line below derives a value object's key by executing the guest's `toString()`. For a
    // `java.lang.Record` that is the generated `ObjectMethods.bootstrap` indy, recursing through
    // every component in GUEST code. `RAVA_KEYTIME`'s slow-key histogram, product boot:
    //   288 renders of `datafixers.types.templates.Sum`         3,147,677 ns each = 0.907 s
    //   832 renders of `DSL$Instances$TaggedChoiceCacheKey`       720,892 ns each = 0.600 s
    // **1.5 s of a ~40 s boot in 1,120 renders.** A record's `equals` is component-wise, so a
    // component-wise render in OUR renderer is exactly as discriminating and meets the funnel
    // memo instead of re-entering guest bytecode. It was built, admitted by BYTECODE
    // (`extends java/lang/Record`, never by name), and instrumented — and every step was refused
    // by a different, now-known cause:
    //
    //   `Value::Ref` only               records seen=0        every record key here is ARENA
    //   + arena, `field_id_opt`         seen=23,387 noid=23,379   the component NAME is never
    //                                                             interned (accessors resolve by
    //                                                             their own path)
    //   + `intern_field`                seen=23,387 nolayout=23,379
    //
    // ⭐**The last line is the answer: `slot_of(cid, field_id)` says the record's declared
    // components are NOT IN THE ARENA OBJECT'S LAYOUT.** The components are not reachable by
    // declared name from the arena representation at all, so no renderer built on the class's
    // field list can read them — and `arena_getfield` would PANIC rather than answer, which is
    // why the check is there.
    //
    // ⇒ Next attempt starts by dumping what IS in that layout for one of these classes, not by
    // writing another renderer. Three earlier attempts on this same 1.5 s are recorded at the
    // branch point in `map_key_inner`.
    // ONE-SHOT DIAGNOSTIC (`RAVA_RECORD_LAYOUT=1`, inert otherwise): what IS in the arena layout
    // of a record whose declared components `slot_of` refuses? The fourth refusal above is a fact
    // about the representation, and this is the question it leaves.
    // ⭐**THIS READ THE ENVIRONMENT ON EVERY KEY RENDER.** `std::env::var_os` allocates an
    // `OsString` and, on Windows, queries the process environment block
    // (`RtlQueryEnvironmentVariable`) — per call. The allocation-site profile of
    // `Bootstrap.bootStrap` put `std::env::_var_os` at **4.0% of ALL allocations** and its stack
    // named this exact site: `structural_key_scoped_inner` <- `combo_entry_bytes` <-
    // `combo_hash_arena_array_map`. A one-shot diagnostic was costing a syscall and a malloc on
    // the map-key path. Four other flags in this same file already memoise; this one was missed.
    #[cfg(feature = "next-gen-gc")]
    if {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        crate::jit::env_gate("RAVA_RECORD_LAYOUT", &ON)
    } {
        if let Value::Obj(p) = v {
            let cid = crate::nextgen::arena_class_id(*p);
            if let Some(name) = crate::nextgen::class_name(cid) {
                let is_rec = {
                    let mut cur = name.to_string();
                    let mut d = 0;
                    loop {
                        if cur == "java/lang/Record" { break true }
                        if cur == "java/lang/Object" || d > 64 { break false }
                        d += 1;
                        let Ok(lc) = vm.loader.get_or_load(&cur) else { break false };
                        match lc.file.super_class.clone() { Some(s) => cur = s, None => break false }
                    }
                };
                if is_rec {
                    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
                        std::sync::OnceLock::new();
                    let set = SEEN.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
                    let mut g = set.lock().unwrap_or_else(|e| e.into_inner());
                    if g.len() < 12 && g.insert(name.to_string()) {
                        let layout = crate::nextgen::layout_dump(cid);
                        let ids: Vec<String> = layout
                            .iter()
                            .map(|(id, k)| format!("{}:{k}", vm.heap.field_name(*id).unwrap_or_else(|| format!("#{id}"))))
                            .collect();
                        let declared: Vec<String> = match vm.loader.get_or_load(&name.to_string()) {
                            Ok(lc) => lc.file.fields.iter().filter(|f| f.access_flags & 0x0008 == 0)
                                .map(|f| f.name.to_string()).collect(),
                            Err(_) => Vec::new(),
                        };
                        eprintln!("[RECLAYOUT] {name}
  declared: {declared:?}
  arena:    {ids:?}");
                    }
                }
            }
        }
    }
    // Not a drainable map key: route value objects (Identifier, ResourceKey, …)
    // through their toString() so equal-by-value keys collide instead of falling
    // to identity — or, under a GAP-P04 salt scope, through salted probing.
    tostring_timed(vm, v, salt_mid, named)
}

#[cfg(feature = "next-gen-gc")]
std::thread_local! {
    static RECORD_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Kill switch for the record component render.
#[cfg(feature = "next-gen-gc")]
fn record_render_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // ⛔**FOURTH HYPOTHESIS FOR THE `Done` COST, AND IT IS NOT THE POPULATION EITHER.** The
    // renderer was narrowed by BYTECODE to records with a non-String reference component — the
    // only ones whose `toString` can recurse — which excludes the cheap high-count records
    // (`Identifier`, `ResourceKey$InternKey`, all-String components) and leaves exactly `Sum` and
    // `TaggedChoiceCacheKey`. Eight interleaved rounds:
    //   renderer ON   Done 0.481 0.484 0.484 0.493   wall min 34.2
    //   renderer OFF  Done 0.418 0.422 0.424 0.429   wall min 34.8
    // **Identical to the un-narrowed result**: still ~14% of `Done`, every run, while wall
    // slightly favours it. ⭐So the cost is not the rendering of the cheap records and not the
    // volume — it is the KEY VALUE ITSELF. A component render distinguishes or conflates
    // differently from the `toString` it replaces, and DataFixerUpper's own caches are keyed on
    // exactly these objects: a key that partitions differently changes their hit rate, which is
    // guest-side behaviour no amount of engine-side tuning here will recover.
    //
    // ⛔**RE-TESTED AFTER THE KEY PATH CHANGED, AND THE TRADE IS THE SAME.** The plan is now
    // memoised and keyed by class id, the combo fold no longer materialises its map, and
    // identity-equality entries are keyed by identity — none of it moved this. Interleaved, one
    // round (the box was 3 s slower that round, so compare within it only):
    //   renderer ON    Done 0.503 0.508 0.521
    //   renderer OFF   Done 0.437 0.442 0.500
    // Still ~10-15% worse on `Done`. Second independent confirmation; stays opt-in.
    //
    // ⛔**DEFAULT OFF: IT WORKS AND IT TRADES THE WRONG THING.** The mechanism does what it was
    // built for — `Sum` and `TaggedChoiceCacheKey` leave `RAVA_KEYTIME`'s slow-key histogram
    // entirely (288 x 3.15 ms and 832 x 0.72 ms gone; worst remaining render 38 us) and it serves
    // 106,582 of 107,068 records. But interleaved boot A/B, both placements and both key formats:
    //
    //   renderer ON   Done 0.607 0.614 0.632 0.666 | 0.610 0.624 0.642
    //   renderer OFF  Done 0.456 0.459 0.477 0.556 | 0.452 0.493 0.538
    //   wall          ON min 38.2-38.6   OFF min 38.8-39.8
    //
    // **Every run of `Done` worse, ~25%, for a wall gain of 1.5-3% that does not clear the
    // floor.** `Done` is an explicit goal, so this cannot ship on that trade. Two candidate
    // causes were tested and are NOT it: placement above the per-object memo (moved below it,
    // unchanged) and key LENGTH (the class now goes in as a 64-bit fingerprint, unchanged).
    // ✅**AND THE THIRD CAUSE IS MEASURED: MEMO CHURN, +1,000,000 CONSULTATIONS.**
    // `RAVA_KEYRENDER_STAT` at the same 45 s cut of the same boot:
    //   renderer OFF  hit  36,273  miss 135,054  rejected 3,328,673   (3.50 M total)
    //   renderer ON   hit  91,837  miss 193,190  rejected 4,214,973   (4.50 M total)
    // The component walk recurses through `structural_key`, so every nested component consults
    // the funnel memo: **+28.6% consultations, of which 886,300 are outright REJECTIONS** — the
    // class rule refusing, which is pure overhead. ~1 M extra consultations at 50-100 ns is the
    // same order as the 0.15 s `Done` regression, and the hit count rising 36 K -> 92 K confirms
    // repeats still cache, so this is added traffic and not a broken memo.
    //
    // ⛔**AND THAT WAS TESTED TOO, AND IS NOT THE CAUSE EITHER.** Making a component render skip
    // the funnel memo cut the traffic back by half (4.50 M -> 4.00 M consultations against a
    // 3.50 M baseline) and `Done` did not move: 0.604/0.626/0.638/0.641 with the renderer on
    // against 0.457/0.488/0.529/0.539 with it off, the same ~25%. Reverted as neutral.
    //
    // ⇒ Three candidate causes tested and refuted: placement above the memo, key LENGTH, memo
    // CHURN. What the numbers actually describe is a PHASE asymmetry — the renderer helps the
    // bootstrap phase, where the catastrophic DFU type-tree renders live (wall min 38.3 against
    // 38.7, and its worst run 39.1 against 40.3), and costs the reload phase that `Done` times.
    // A fix has to explain why the reload's records are cheaper to render through the guest.
    *ON.get_or_init(|| std::env::var_os("RAVA_RECORD_RENDER").is_some())
}

#[cfg(feature = "next-gen-gc")]
pub(crate) static RECORD_RENDER_SEEN: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "next-gen-gc")]
pub(crate) static RECORD_RENDER_DONE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// `Some(key)` when `v` is a `java.lang.Record`: `R:<class>(c1,c2,..)` over its declared
/// instance fields in declaration order, each rendered by the ordinary key renderer. `None` for
/// anything it cannot read exactly — a missing layout slot, a cycle, a class it cannot load —
/// so the guest path answers unchanged.
#[cfg(feature = "next-gen-gc")]
fn record_component_key(vm: &mut Vm, v: &Value) -> Result<Option<String>, VmError> {
    // ⭐**KEYED BY CLASS ID, NOT BY THE CLASS NAME.** Reading the name costs a fresh `String` on
    // every call and then hashes 40-60 characters to look the plan up — the same defect the class
    // loader's own `super_chain` comment records ("a cache keyed on a string still hashes the
    // string every call"). The name is now read ONLY on a plan miss, once per class.
    let key: i64 = match v {
        Value::Ref(h) => match vm.heap.object_class_word(*h) {
            Some(w) => w,
            None => return Ok(None),
        },
        Value::Obj(p) => -(crate::nextgen::arena_class_id(*p) as i64) - 1,
        _ => return Ok(None),
    };
    let cid = match v {
        Value::Obj(p) => Some(crate::nextgen::arena_class_id(*p)),
        _ => None,
    };
    // A component could point back at an ancestor. `toString` would loop too, but this renderer
    // must not: bail to the guest path rather than recurse without bound. The same cell also
    // tells the funnel above to stop memoising while a component walk is in progress.
    if RECORD_DEPTH.with(|d| d.get()) > 32 {
        return Ok(None);
    }
    // ⭐**THE COMPONENT IDS AND THE CLASS FINGERPRINT ARE MEMOISED PER CLASS.** The first version
    // built a `Vec<String>` of component names and one `format!("{class}#{n}")` per component
    // per CALL — so ~250,000 fresh 60-character Strings a boot to answer a question whose answer
    // is a constant of the class. That is the allocation-to-answer-a-question shape this tree has
    // paid for three times, and it is the likeliest reason a SHALLOW record rendered slower here
    // than through the guest's `toString`.
    let plan = {
        // ⭐**THE MEMO STILL ALLOCATED ON EVERY HIT.** The plan is a per-class CONSTANT, but it
        // was stored as `Vec<u32>` and served with `.cloned()` — so every one of the ~250,000
        // record renders a boot allocated a fresh vector and memcpy'd the component ids into it,
        // purely to get the value out from under the `RefCell` borrow (the loop below needs
        // `&mut Vm`, so the borrow cannot be held). That is the same
        // allocation-to-answer-a-question shape the comment above this one records paying for.
        //
        // `Rc<[u32]>` is the shape that fits: the borrow is still released, but the "clone" is a
        // non-atomic refcount bump on a thread-local — no allocator call, no copy, and the ids
        // are read straight out of the shared buffer. `Rc`, not `Arc`: this memo is
        // thread_local, so an atomic RMW would be paid for a sharing that cannot happen.
        std::thread_local! {
            static PLANS: std::cell::RefCell<crate::FastMap<i64, Option<(u64, std::rc::Rc<[u32]>)>>> =
                std::cell::RefCell::new(crate::FastMap::default());
        }
        if let Some(p) = PLANS.with(|c| c.borrow().get(&key).cloned()) {
            p
        } else {
            let built = (|| {
                // Miss: NOW read the name, decide `extends java/lang/Record`, resolve the ids.
                let class: String = match v {
                    Value::Ref(h) => vm.heap.class_of(*h)?.to_string(),
                    Value::Obj(p) => {
                        crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))?.to_string()
                    }
                    _ => return None,
                };
                if !is_record_class(vm, &class) {
                    return None;
                }
                let lc = vm.loader.get_or_load(&class).ok()?;
                let comps: Vec<(u32, bool)> = lc
                    .file
                    .fields
                    .iter()
                    .filter(|f| f.access_flags & 0x0008 == 0)
                    .map(|f| {
                        let d: &str = &f.descriptor;
                        // A component that is a reference to something other than a String is
                        // what makes a record's `toString` RECURSIVE — and recursion is the only
                        // reason this path exists.
                        let deep = d.starts_with('L')
                            && d != "Ljava/lang/String;"
                            || d.starts_with('[');
                        (vm.heap.intern_field(&format!("{class}#{}", f.name)), deep)
                    })
                    .collect();
                if comps.is_empty() {
                    return None;
                }
                // ⭐**NARROWED BY BYTECODE, NOT BY TIMING.** The full renderer works — `Sum` and
                // `TaggedChoiceCacheKey` leave the slow-key histogram entirely — and costs ~10%
                // of `Done`, for reasons three separate hypotheses failed to explain. What it
                // was applied to included records whose components are all Strings
                // (`Identifier`, `ResourceKey$InternKey`): those are the CHEAP ones, they are the
                // high-count ones, and their existing memoised `toString` path already serves
                // them. A record can only have an expensive recursive render if one of its
                // COMPONENT DESCRIPTORS is a reference to something other than a String, and that
                // is a property of the class file — no timing, no workload name.
                if record_deep_only() && !comps.iter().any(|(_, deep)| *deep) {
                    return None;
                }
                let ids: std::rc::Rc<[u32]> = comps.into_iter().map(|(id, _)| id).collect();
                let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                for b in class.as_bytes() {
                    h ^= *b as u64;
                    h = h.wrapping_mul(0x0000_0100_0000_01B3);
                }
                Some((h, ids))
            })();
            PLANS.with(|c| c.borrow_mut().insert(key, built.clone()));
            built
        }
    };
    let Some((cls_h, ids)) = plan else { return Ok(None) };
    RECORD_RENDER_SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The layout check is per (class id, field id) and cannot be folded into the per-class plan:
    // `arena_getfield` PANICS outside the layout, so it is re-asked for this receiver's class id.
    if let Some(cid) = cid {
        for id in ids.iter() {
            if crate::nextgen::slot_of(cid, *id).is_none() {
                return Ok(None);
            }
        }
    }
    RECORD_DEPTH.with(|d| d.set(d.get() + 1));
    // ⚠**THE CLASS GOES IN AS A FINGERPRINT, NOT AS ITS NAME.** Writing the name verbatim puts
    // a 40-60 character class at EVERY level of a recursive record, so the key string for a deep
    // type tree came out longer than the `toString` it replaces — and a longer key is paid again
    // by every hash and every compare downstream. Measured that way, `Done` was 0.607/0.614/
    // 0.632/0.666 against 0.456/0.459/0.477/0.556 with the renderer off: every run worse, on a
    // wall that was a wash. A 64-bit FNV of the name discriminates the same set of classes (a
    // collision would have to be between two record classes with identical component renders)
    // and costs 16 characters instead of 60.
    let mut out = String::with_capacity(24 + ids.len() * 8);
    out.push_str("R:");
    // `push_str(&format!(..))` built a whole second `String` to copy 16 characters out of it,
    // once per render. `write!` formats straight into the buffer that is already reserved.
    {
        use std::fmt::Write as _;
        let _ = write!(out, "{cls_h:x}");
    }
    out.push('(');
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let fv = match v {
            Value::Ref(h) => vm.heap.get_field_hint(*h, *id, 0).0,
            Value::Obj(p) => crate::opcodes::arena_getfield(*p, *id),
            _ => Value::Null,
        };
        out.push_str(&crate::map::structural_key(vm, &fv));
    }
    out.push(')');
    RECORD_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    RECORD_RENDER_DONE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(Some(out))
}

/// Does this class extend `java/lang/Record`? Memoised per class name; the walk is the class
/// hierarchy, so nothing is matched against a list of names.
#[cfg(feature = "next-gen-gc")]
fn is_record_class(vm: &mut Vm, name: &str) -> bool {
    std::thread_local! {
        static SEEN: std::cell::RefCell<crate::FastMap<String, bool>> =
            std::cell::RefCell::new(crate::FastMap::default());
    }
    if let Some(v) = SEEN.with(|c| c.borrow().get(name).copied()) {
        return v;
    }
    let mut cur = name.to_string();
    let mut depth = 0;
    let verdict = loop {
        if cur == "java/lang/Record" {
            break true;
        }
        if cur == "java/lang/Object" {
            break false;
        }
        depth += 1;
        if depth > 64 {
            break false;
        }
        let Ok(lc) = vm.loader.get_or_load(&cur) else { break false };
        match lc.file.super_class.clone() {
            Some(sup) => cur = sup,
            None => break false,
        }
    };
    SEEN.with(|c| c.borrow_mut().insert(name.to_string(), verdict));
    verdict
}

/// Restrict the record component render to records with a non-String reference component.
/// `RAVA_RECORD_RENDER_ALL=1` applies it to every record, as the first version did.
#[cfg(feature = "next-gen-gc")]
fn record_deep_only() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_RECORD_RENDER_ALL").is_none())
}
