//! Native JSON parse for the gson fast-path (RAVA_GSON_NATIVE).
//!
//! Minecraft builds a `JsonElement` tree via `JsonParser.parseReader`/`parseString`
//! → `Streams.parse(JsonReader)`; the JsonReader bytecode tokenizer is ~22% of the
//! post-memo boot profile. This module tokenizes + parses the JSON text in Rust
//! into a lightweight `Json` tree, which the caller then materialises into real gson
//! `JsonElement` objects by invoking gson's OWN bytecode constructors (`new JsonObject`,
//! `add`, `new JsonPrimitive`, …) — so the object graph is exactly gson's, only the
//! tokenizer is replaced. Standard JSON (the shape of every MC data file); on any
//! deviation the parser returns `None` and the caller falls back to the bytecode path.

/// A parsed JSON value. Numbers keep their raw text (gson wraps them in a
/// `LazilyParsedNumber` over the same text; `getAsInt`/`getAsDouble` reparse).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

/// Parse a complete JSON document. `None` on any malformed/unsupported input (the
/// caller then runs the real gson bytecode, so a parser gap is never a correctness
/// risk — only a missed optimisation).
pub(crate) fn parse(s: &str) -> Option<Json> {
    let mut p = Parser { b: s.as_bytes(), i: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i == p.b.len() {
        Some(v)
    } else {
        None // trailing junk → bail to bytecode
    }
}

impl<'a> Parser<'a> {
    #[inline]
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn value(&mut self) -> Option<Json> {
        match self.peek()? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string().map(Json::Str),
            b't' => self.lit(b"true").then_some(Json::Bool(true)),
            b'f' => self.lit(b"false").then_some(Json::Bool(false)),
            b'n' => self.lit(b"null").then_some(Json::Null),
            c if c == b'-' || c.is_ascii_digit() => self.number(),
            _ => None,
        }
    }

    fn lit(&mut self, kw: &[u8]) -> bool {
        if self.b[self.i..].starts_with(kw) {
            self.i += kw.len();
            true
        } else {
            false
        }
    }

    fn object(&mut self) -> Option<Json> {
        self.i += 1; // '{'
        let mut out = Vec::new();
        self.ws();
        if self.peek()? == b'}' {
            self.i += 1;
            return Some(Json::Obj(out));
        }
        loop {
            self.ws();
            if self.peek()? != b'"' {
                return None;
            }
            let key = self.string()?;
            self.ws();
            if self.peek()? != b':' {
                return None;
            }
            self.i += 1;
            self.ws();
            let val = self.value()?;
            out.push((key, val));
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Some(Json::Obj(out));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self) -> Option<Json> {
        self.i += 1; // '['
        let mut out = Vec::new();
        self.ws();
        if self.peek()? == b']' {
            self.i += 1;
            return Some(Json::Arr(out));
        }
        loop {
            self.ws();
            out.push(self.value()?);
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Some(Json::Arr(out));
                }
                _ => return None,
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        self.i += 1; // opening '"'
        let mut s = String::new();
        loop {
            let c = self.peek()?;
            self.i += 1;
            match c {
                b'"' => return Some(s),
                b'\\' => {
                    let e = self.peek()?;
                    self.i += 1;
                    match e {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'b' => s.push('\u{0008}'),
                        b'f' => s.push('\u{000C}'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'u' => {
                            let cp = self.hex4()?;
                            // Surrogate pair handling for astral code points.
                            if (0xD800..=0xDBFF).contains(&cp) {
                                if self.peek()? != b'\\' {
                                    return None;
                                }
                                self.i += 1;
                                if self.peek()? != b'u' {
                                    return None;
                                }
                                self.i += 1;
                                let lo = self.hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&lo) {
                                    return None;
                                }
                                let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                s.push(char::from_u32(c)?);
                            } else {
                                s.push(char::from_u32(cp)?);
                            }
                        }
                        _ => return None,
                    }
                }
                // A raw control char in a string is invalid strict JSON → bail.
                0x00..=0x1F => return None,
                // UTF-8 continuation/lead bytes: copy the byte through; the buffer is
                // valid UTF-8 (it came from a &str), so multi-byte sequences reassemble.
                _ => {
                    // Re-decode the full UTF-8 char starting at the byte we consumed.
                    let start = self.i - 1;
                    let len = utf8_len(c);
                    if len == 1 {
                        s.push(c as char);
                    } else {
                        let end = start + len;
                        let chunk = self.b.get(start..end)?;
                        s.push_str(std::str::from_utf8(chunk).ok()?);
                        self.i = end;
                    }
                }
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = self.b.get(self.i..self.i + 4)?;
        self.i += 4;
        u32::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        let txt = std::str::from_utf8(&self.b[start..self.i]).ok()?;
        // Reject a bare "-" or empty (malformed) — gson would error; bail to bytecode.
        if txt.is_empty() || txt == "-" {
            return None;
        }
        Some(Json::Num(txt.to_string()))
    }
}

#[inline]
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// Materialise a parsed `Json` tree into real gson `JsonElement` objects by invoking
/// gson's OWN bytecode constructors — so the object graph is exactly what the gson
/// tokenizer would build, only the tokenizing is replaced. `Ok(None)` ⇒ a node could
/// not be built (caller falls back to the bytecode parse — never a correctness risk).
/// The caller must block GC for the whole build (the partial tree lives in Rust
/// locals, not on a GC-rooted operand stack); heap Refs are non-moving so the handles
/// stay valid.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn build(vm: &mut crate::Vm, j: &Json) -> Result<Option<crate::value::Value>, crate::VmError> {
    use crate::reflect::new_instance;
    use crate::value::Value;
    const PRIM: &str = "com/google/gson/JsonPrimitive";
    let v = match j {
        Json::Null => match crate::natives::get_static(vm, "com/google/gson/JsonNull", "INSTANCE") {
            Some(v) => v,
            None => new_instance(vm, "com/google/gson/JsonNull", "", &[])?,
        },
        Json::Bool(b) => {
            // A real boxed java.lang.Boolean — gson's JsonPrimitive keys behaviour on
            // `value instanceof Boolean`, which a bare Value::Int would fail.
            let boxed = vm.boolean_obj(*b);
            new_instance(vm, PRIM, "Ljava/lang/Boolean;", &[boxed])?
        }
        Json::Str(s) => {
            let _t = crate::diag::gson_build_new().start();
            // 62,557 string values over 7,952 distinct — see `token_string`.
            new_instance(vm, PRIM, "Ljava/lang/String;", &[token_string(s.as_str())])?
        }
        Json::Num(n) => {
            // gson stores a parsed number as a LazilyParsedNumber over the raw text.
            //
            // ⛔**AND THIS ONE IS DELIBERATELY *NOT* INTERNED — `CompSem` caught it.**
            // `natives/cascade_owners.rs` serves every `LazilyParsedNumber` accessor
            // (`intValue`/`longValue`/`doubleValue`/`toString`/`hashCode`) by reading the
            // `value` field and matching `Value::Str(s) => s.as_str().trim(), _ => ""` —
            // so an ARENA string in that field reads as the EMPTY text and every accessor
            // answers 0. The symptom was exact and silent: `{"flight_duration":1}` decoded
            // as 0, so a firework result no longer equalled its item prototype and
            // `recipe.encode` grew a `"components":{"minecraft:fireworks":{}}` the JVM does
            // not print. That reader lives in another area's file, so this side declines
            // instead — and a number text is not the population this area is aimed at
            // anyway (the 62x key and 7.9x value repeats are; a number is re-PARSED, never
            // re-read as a string, so an arena body would buy it nothing).
            let num = new_instance(vm, "com/google/gson/internal/LazilyParsedNumber",
                "Ljava/lang/String;", &[Value::Str(n.as_str().into())])?;
            new_instance(vm, PRIM, "Ljava/lang/Number;", &[num])?
        }
        // Two meters, nested self-time: `new` is the by-name class+constructor resolve plus the
        // guest constructor body; `add` is the guest `add()` call, which for an object lands in
        // `LinkedTreeMap.put` -- a method the interpreted census names directly (put / find /
        // rebalance / rotateLeft, thousands of entries each, never compiled). The recursive
        // `build` call sits OUTSIDE both, so a child's cost is not charged to its parent's arm.
        Json::Arr(items) => {
            let arr = {
                let _t = crate::diag::gson_build_new().start();
                new_instance(vm, "com/google/gson/JsonArray", "", &[])?
            };
            for item in items {
                let Some(el) = build(vm, item)? else { return Ok(None) };
                let _t = crate::diag::gson_build_add().start();
                invoke_void(vm, &arr, "add", "(Lcom/google/gson/JsonElement;)V", &[el])?;
            }
            arr
        }
        Json::Obj(entries) => {
            let obj = {
                let _t = crate::diag::gson_build_new().start();
                new_instance(vm, "com/google/gson/JsonObject", "", &[])?
            };
            for (k, val) in entries {
                let Some(el) = build(vm, val)? else { return Ok(None) };
                let _t = crate::diag::gson_build_add().start();
                // 107,274 members over 1,731 distinct keys — the 62x population.
                invoke_void(vm, &obj, "add",
                    "(Ljava/lang/String;Lcom/google/gson/JsonElement;)V",
                    &[token_string(k.as_str()), el])?;
            }
            obj
        }
    };
    Ok(Some(v))
}

#[cfg(feature = "next-gen-gc")]
fn invoke_void(vm: &mut crate::Vm, recv: &crate::value::Value, name: &str, desc: &str,
    args: &[crate::value::Value]) -> Result<(), crate::VmError> {
    let cls = vm.value_class_name(recv);
    if let Some(decl) = vm.resolve_method(&cls, name, desc)? {
        let mut locals = Vec::with_capacity(args.len() + 1);
        locals.push(recv.clone());
        locals.extend_from_slice(args);
        vm.invoke(decl, name, desc, locals)?;
    }
    Ok(())
}

// ═══════════════════ TOKEN INTERNING — RAVA_NO_TOKEN_INTERN ═══════════════════
//
// ⭐**A JSON KEY TEXT IS MATERIALISED 62 TIMES PER RELOAD, AND EVERY ONE OF THEM WAS TWO
// MALLOCS AND A HANDLE.** The read-only census of the reload window counts **107,274
// object members over 1,731 DISTINCT keys** (62x each) and **62,557 string values over
// 7,952 distinct** (7.9x). `build` turned every occurrence into `Value::Str(s.into())` —
// `Arc::new(String::from(s))`, i.e. the `ArcInner` box AND the `String` buffer, freed
// again microseconds later — and handed the guest a HANDLE string.
//
// ⭐**AND THE HANDLE IS THE BIGGER HALF.** `jit/emit/strings.rs` gates its inline
// `charAt`/`length` arms on `emit_is_arena(recv)`, so a handle string sends compiled code
// out to a host callback for EVERY character. `ResourceLocation`'s `isValidNamespace` /
// `isValidPath` are two such calls PER CHARACTER, over the 24,442 id-shaped values (4,939
// distinct) this window parses. Making the token arena-resident is what lets those arms
// fire at all; interning is what makes it free and permanent.
//
// **THE MECHANISM IS NOT NEW — IT IS `nextgen::intern_arena_string`**, the
// process-lifetime, content-deduped, LOW (<2^40) bump pool the JIT already mints `ldc`
// String constants into. Its layout is byte-identical to `tlab_alloc_string`, so every
// `is_arena_string` reader (length / hashCode / startsWith / `str_of` / the map key
// derivation / `natives::entry`'s canonicaliser) treats the result identically — and, the
// property that decides this design, **the pool is NOT any thread's nursery, so the Cheney
// collector returns the pointer as-is and never traces it.** The table below may therefore
// hold the raw address across any number of collections, which the house rule otherwise
// forbids: a nursery string (`tlab_alloc_string`) would need a GC root or to be
// re-derivable, and the permanent pool needs neither. That is the whole reason it, and not
// the nursery, is the store.
//
// ⭐**IDENTITY IS NOT OBSERVABLE HERE, AND THAT IS A PROPERTY OF THIS ENGINE RATHER THAN A
// HOPE.** `opcodes::gates::ref_equal` — this engine's `if_acmp*`, shared verbatim by the
// JIT's `rava_acmp` — already answers `Str`/`Str` by CONTENT (`ptr_eq || ==`) and
// arena-string/arena-string by CONTENT (`arena_str_bytes(x) == arena_str_bytes(y)`),
// deliberately, because the value model has no per-instance String identity. Dedup is
// therefore invisible to `==`: two occurrences of `"minecraft:stone"` compared equal BEFORE
// this change and compare equal after, in either representation (there is a mixed
// `Obj`/`Str` arm too). `natives::values::identity_hash` returns **0 for every
// `Value::Str`**, so an interned arena string — which gets a real lazily-stamped header
// hash — can only be MORE distinguishable, never less. The `arena_cells.rs` string arm
// records the same reasoning for the same pool.
//
// ⛔**AND ONE OF THE THREE TOKEN KINDS IS NOT ADMITTED AT ALL.** Keys and string values are;
// a NUMBER's raw text is not, because `natives/cascade_owners.rs` serves every
// `LazilyParsedNumber` accessor by matching that field as `Value::Str` and reading `""` for
// anything else. `CompSem` caught it as a wrong recipe encoding, not as a crash. The reasoning
// and the symptom are at the `Json::Num` arm of `build`; this is the pointer to it.
//
// **EVERY GUARD ONLY REJECTS.** Kill switch off ⇒ historic. Token too long ⇒ historic.
// Budget spent ⇒ historic. Pool exhausted (`intern_arena_string` answers 0) ⇒ historic.
// An address that fails `is_arena_obj_addr` ⇒ historic. Each refusal lands on the
// byte-for-byte original expression, so a gap is a missed optimisation, never a different
// answer — the same contract the tokenizer above already keeps with `None`.

/// Longest token that may be interned PERMANENTLY. Keys and identifier-shaped values sit
/// far below this (the average whole FILE is 784 B); a longer token is both unlikely to
/// repeat and expensive to keep for the process lifetime, so it takes the historic path.
#[cfg(feature = "next-gen-gc")]
const MAX_TOKEN_BYTES: usize = 192;
/// Ceiling on what this path may claim of the 16 MiB permanent pool it SHARES with the
/// JIT's `ldc` constants. The measured population needs ~9,700 entries and well under
/// 512 KiB; the cap exists so a pathological input cannot starve the compiler's literals.
#[cfg(feature = "next-gen-gc")]
const MAX_TOKEN_POOL_BYTES: usize = 4 * 1024 * 1024;
#[cfg(feature = "next-gen-gc")]
const MAX_TOKEN_ENTRIES: usize = 1 << 17;

/// ⭐**KILL SWITCH.** `RAVA_NO_TOKEN_INTERN=1` — or the block-wide `RAVA_NO_EMIT_MORE` —
/// restores the historic `Value::Str(s.into())` births EXACTLY. Resolved once.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn token_intern_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_TOKEN_INTERN").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// The per-thread memo in FRONT of `intern_arena_string`.
///
/// It mints nothing — the permanent pool does — it only remembers what the pool already
/// answered, for three reasons: the pool's own map lives behind a PROCESS-WIDE `Mutex`
/// shared with the compiler's `ldc` interning, its keys are hashed with SipHash, and
/// neither "how many DISTINCT tokens are there" nor "how many bytes did the repeats save"
/// is answerable from the pool at all (it cannot tell its callers apart). One `Box<str>`
/// is allocated per DISTINCT token — ~9,700 for a whole reload — never per occurrence.
#[cfg(feature = "next-gen-gc")]
struct TokenTable {
    map: std::collections::HashMap<Box<str>, u64>,
    /// Content bytes this path has handed to the permanent pool (the budget).
    pool_bytes: usize,
    // Batched counters, folded into the globals below on every MISS and every 8,192
    // occurrences — so the served path performs no atomic RMW at all (house rule).
    seen: u64,
    hits: u64,
    saved: u64,
    fallback: u64,
}

#[cfg(feature = "next-gen-gc")]
thread_local! {
    static TOKENS: std::cell::RefCell<TokenTable> = std::cell::RefCell::new(TokenTable {
        map: std::collections::HashMap::new(),
        pool_bytes: 0,
        seen: 0,
        hits: 0,
        saved: 0,
        fallback: 0,
    });
}

#[cfg(feature = "next-gen-gc")]
mod tokstats {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub(super) static SEEN: AtomicU64 = AtomicU64::new(0);
    pub(super) static HITS: AtomicU64 = AtomicU64::new(0);
    pub(super) static DISTINCT: AtomicU64 = AtomicU64::new(0);
    pub(super) static SAVED: AtomicU64 = AtomicU64::new(0);
    pub(super) static FALLBACK: AtomicU64 = AtomicU64::new(0);
    pub(super) fn add(seen: u64, hits: u64, saved: u64, fallback: u64) {
        SEEN.fetch_add(seen, Relaxed);
        HITS.fetch_add(hits, Relaxed);
        SAVED.fetch_add(saved, Relaxed);
        FALLBACK.fetch_add(fallback, Relaxed);
    }
    pub(super) fn note_distinct(n: u64) {
        DISTINCT.fetch_max(n, Relaxed);
    }
}

/// `[TOKENS]` — the population this area serves. Printed from the periodic profiler dump
/// beside `compile_bail::report`, for the reason that census records: a server boot exits
/// through `System.exit`, so a teardown dump never runs. Empty when nothing was interned.
pub fn intern_report() -> String {
    #[cfg(not(feature = "next-gen-gc"))]
    {
        String::new()
    }
    #[cfg(feature = "next-gen-gc")]
    {
        use std::sync::atomic::Ordering::Relaxed;
        // Fold this thread's un-flushed batch in first, so a dump taken from the guest
        // thread is exact rather than up to 8,191 occurrences stale.
        flush_local_stats();
        let seen = tokstats::SEEN.load(Relaxed);
        if seen == 0 {
            return String::new();
        }
        let hits = tokstats::HITS.load(Relaxed);
        let distinct = tokstats::DISTINCT.load(Relaxed);
        let saved = tokstats::SAVED.load(Relaxed);
        let fallback = tokstats::FALLBACK.load(Relaxed);
        format!(
            "[TOKENS] {seen} JSON tokens materialised, {hits} served from the intern table \
             ({:.1}%), {distinct} distinct entries ({:.1}x reuse), {fallback} declined to \
             Value::Str | {} mallocs and {saved} bytes of Arc<String> never allocated",
            hits as f64 * 100.0 / seen as f64,
            if distinct == 0 { 0.0 } else { seen as f64 / distinct as f64 },
            hits * 2,
        )
    }
}

#[cfg(feature = "next-gen-gc")]
fn flush_local_stats() {
    TOKENS.with(|t| {
        if let Ok(mut t) = t.try_borrow_mut() {
            let (seen, hits, saved, fb, n) = (t.seen, t.hits, t.saved, t.fallback, t.map.len());
            t.seen = 0;
            t.hits = 0;
            t.saved = 0;
            t.fallback = 0;
            drop(t);
            tokstats::add(seen, hits, saved, fb);
            tokstats::note_distinct(n as u64);
        }
    });
}

/// The token as a guest `java.lang.String` — an ARENA string out of the permanent interned
/// pool when every guard admits it, and otherwise the historic `Value::Str` verbatim.
#[cfg(feature = "next-gen-gc")]
fn token_string(s: &str) -> crate::value::Value {
    use crate::value::Value;
    // GUARD 1 — the kill switch, and the historic expression byte for byte.
    if !token_intern_enabled() {
        return Value::Str(s.into());
    }
    // GUARD 2 — a token too long to keep for the process lifetime. Rejected BEFORE the
    // table is touched, so a large text blob costs one length compare and nothing else.
    if s.len() > MAX_TOKEN_BYTES {
        TOKENS.with(|t| {
            if let Ok(mut t) = t.try_borrow_mut() {
                t.seen += 1;
                t.fallback += 1;
            }
        });
        return Value::Str(s.into());
    }
    let addr = TOKENS.with(|t| {
        // Nothing below runs guest code, so a re-entrant borrow cannot arise — but if one
        // ever did, DECLINE rather than panic.
        let Ok(mut t) = t.try_borrow_mut() else { return 0u64 };
        t.seen += 1;
        if let Some(&a) = t.map.get(s) {
            t.hits += 1;
            // What the historic arm would have allocated for THIS occurrence: the `String`
            // buffer plus the `ArcInner<String>` box holding it (strong + weak + payload).
            t.saved += (s.len() + std::mem::size_of::<String>() + 16) as u64;
            if t.seen & 0x1FFF == 0 {
                let (seen, hits, saved, fb, n) =
                    (t.seen, t.hits, t.saved, t.fallback, t.map.len());
                t.seen = 0;
                t.hits = 0;
                t.saved = 0;
                t.fallback = 0;
                drop(t);
                tokstats::add(seen, hits, saved, fb);
                tokstats::note_distinct(n as u64);
            }
            return a;
        }
        // GUARD 3 — the budget on the 16 MiB pool this path SHARES with the JIT's `ldc`
        // literals. Spent ⇒ decline, and the compiler's constants keep their room.
        if t.pool_bytes + s.len() > MAX_TOKEN_POOL_BYTES || t.map.len() >= MAX_TOKEN_ENTRIES {
            t.fallback += 1;
            return 0;
        }
        // GUARD 4 — the pool refuses (0) once exhausted.
        let a = crate::nextgen::intern_arena_string(s) as u64;
        // GUARD 5 — and the address must be a dereferenceable arena object. One that is
        // not would hand every downstream `is_arena_string` a wild deref, so it is never
        // wrapped in a `Value::Obj`.
        if a == 0 || !crate::nextgen::is_arena_obj_addr(a) {
            t.fallback += 1;
            return 0;
        }
        t.pool_bytes += s.len();
        t.map.insert(s.into(), a);
        let (seen, hits, saved, fb, n) = (t.seen, t.hits, t.saved, t.fallback, t.map.len());
        t.seen = 0;
        t.hits = 0;
        t.saved = 0;
        t.fallback = 0;
        drop(t);
        // A miss happens once per DISTINCT token (~9,700 a reload), so flushing here costs
        // four relaxed adds per NEW entry and none at all per occurrence.
        tokstats::add(seen, hits, saved, fb);
        tokstats::note_distinct(n as u64);
        a
    });
    match addr {
        0 => Value::Str(s.into()),
        // SAFETY: `a` came from `intern_arena_string` — the permanent pool's bump
        // allocator — and passed `is_arena_obj_addr`, so it is a live, 8-aligned arena
        // object header inside the committed low region; the pool is never collected and
        // never recycles an address, so it stays one for the process lifetime.
        a => Value::Obj(unsafe { crate::arena::ArenaPtr::from_addr(a) }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives() {
        assert_eq!(parse("true"), Some(Json::Bool(true)));
        assert_eq!(parse(" false "), Some(Json::Bool(false)));
        assert_eq!(parse("null"), Some(Json::Null));
        assert_eq!(parse("42"), Some(Json::Num("42".into())));
        assert_eq!(parse("-3.14e2"), Some(Json::Num("-3.14e2".into())));
        assert_eq!(parse("\"hi\""), Some(Json::Str("hi".into())));
    }

    #[test]
    fn escapes_and_unicode() {
        assert_eq!(parse(r#""a\nb""#), Some(Json::Str("a\nb".into())));
        assert_eq!(parse(r#""A""#), Some(Json::Str("A".into())));
        assert_eq!(parse(r#""😀""#), Some(Json::Str("😀".into())));
        assert_eq!(parse(r#""café""#), Some(Json::Str("café".into())));
    }

    #[test]
    fn object_and_array() {
        assert_eq!(
            parse(r#"{"a":1,"b":[true,null,"x"]}"#),
            Some(Json::Obj(vec![
                ("a".into(), Json::Num("1".into())),
                ("b".into(), Json::Arr(vec![Json::Bool(true), Json::Null, Json::Str("x".into())])),
            ]))
        );
        assert_eq!(parse("{}"), Some(Json::Obj(vec![])));
        assert_eq!(parse("[]"), Some(Json::Arr(vec![])));
        assert_eq!(parse(r#"{ "k" : { "n" : 2 } }"#).is_some(), true);
    }

    /// The two properties the interning rests on, both asserted rather than assumed:
    /// a token comes back as a real ARENA STRING (the precondition of the JIT's inline
    /// `charAt`/`length` arms), and the dedup it performs is INVISIBLE to `if_acmp*`,
    /// because `ref_equal` already answers strings by content in every representation.
    #[cfg(feature = "next-gen-gc")]
    #[test]
    fn interned_tokens_are_arena_strings_and_identity_is_unchanged() {
        use crate::value::Value;
        let a = token_string("minecraft:stone");
        let b = token_string("minecraft:stone");
        let c = token_string("minecraft:dirt");
        let Value::Obj(pa) = a else { panic!("token did not become an arena string: {a:?}") };
        let Value::Obj(pb) = b else { panic!("second occurrence declined: {b:?}") };
        assert!(crate::nextgen::is_arena_string(pa) && crate::nextgen::is_arena_string(pb));
        // Content-deduped: the repeat is the SAME permanent object, so it cost a probe.
        assert_eq!(pa.addr(), pb.addr());
        assert_eq!(crate::nextgen::arena_str_bytes(pa), b"minecraft:stone");
        // `==` agrees across representations and with the historic form, both ways.
        let handle = Value::Str("minecraft:stone".into());
        assert!(crate::opcodes::ref_equal(&a, &b));
        assert!(crate::opcodes::ref_equal(&a, &handle));
        assert!(crate::opcodes::ref_equal(&handle, &a));
        assert!(!crate::opcodes::ref_equal(&a, &c));
        // GUARD 2: a token past the permanent-pool length limit takes the historic path.
        let long = "x".repeat(MAX_TOKEN_BYTES + 1);
        assert!(matches!(token_string(&long), Value::Str(_)));
    }

    #[test]
    fn malformed_bails_to_none() {
        assert_eq!(parse("{"), None);
        assert_eq!(parse("[1,]"), None);
        assert_eq!(parse("tru"), None);
        assert_eq!(parse("1 2"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("-"), None);
        assert_eq!(parse("{\"a\":}"), None);
    }
}
