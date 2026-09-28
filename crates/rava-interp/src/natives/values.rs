//! Reading guest values as host types: strings, lengths, identity and reference equality.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// ENUM MIGRATION (RAVA_ARENA_ENUM): read a synthetic enum field ($enumName /
/// $enumOrd) from EITHER a handle enum (Value::Ref — the historic form) or an
/// arena-born enum (Value::Obj, flag ON — the two cells appended to the layout by
/// `class_field_layout`). Returns None when the receiver is not an enum-shaped
/// object (no handle, or an arena object whose layout has no such slot) so the
/// caller falls through to its default exactly as it did for a non-enum receiver.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn enum_field(vm: &mut Vm, v: Option<&Value>, field: &str) -> Option<Value> {
    match v {
        Some(Value::Ref(h)) => Some(vm.heap.get_field(*h, field)),
        Some(Value::Obj(p)) if crate::nextgen::arena_enum_enabled() => {
            // A READ goes through the TLS L1 (`field_id_opt`), not the plain interner
            // (`intern_field` = an `RwLock` read-guard plus an FxHash of the name, per call).
            // A name nothing has ever been given is not interned, so it has no slot — which is
            // the `None` the `slot_of` miss below already returns. See `Vm::synth_get`.
            let Some(id) = vm.heap.field_id_opt(field) else {
                return None;
            };
            let cid = crate::nextgen::arena_class_id(*p);
            if crate::nextgen::slot_of(cid, id).is_some() {
                Some(crate::opcodes::arena_getfield(*p, id))
            } else {
                None
            }
        }
        _ => None,
    }
}
#[cfg(not(feature = "next-gen-gc"))]
pub(crate) fn enum_field(vm: &mut Vm, v: Option<&Value>, field: &str) -> Option<Value> {
    match v {
        Some(Value::Ref(h)) => Some(vm.heap.get_field(*h, field)),
        _ => None,
    }
}

/// The canonicalized-args buffer for one native call. `pooled` (RAVA_CANON_SCRATCH)
/// returns the Vec to `frames::pool_give` CLEARED on drop — the drop runs after the
/// `NativeArgsGuard` (declared later in `try_invoke*`, so dropped first) has
/// unrooted the slots, and clearing means the pool never holds a `Value` across a
/// GC point. Flag OFF keeps the historic fresh-Vec/drop behavior byte-for-byte.
#[cfg(feature = "next-gen-gc")]
pub(crate) struct CanonArgs {
    pub(crate) buf: Vec<Value>,
    pub(crate) pooled: bool,
}
#[cfg(feature = "next-gen-gc")]
impl std::ops::Deref for CanonArgs {
    type Target = [Value];
    #[inline]
    fn deref(&self) -> &[Value] {
        &self.buf
    }
}
#[cfg(feature = "next-gen-gc")]
impl Drop for CanonArgs {
    fn drop(&mut self) {
        if self.pooled {
            let mut v = std::mem::take(&mut self.buf);
            v.clear();
            crate::frames::pool_give(v);
        }
    }
}

/// Build a JVM array class name ("[I", "[Ljava/lang/Object;", "[[...") from an
/// element type, which may be a primitive name, a field descriptor, or a bare
/// internal class name.
pub(crate) fn array_class_name(element: &str) -> String {
    let desc = match element {
        "int" => "I".to_string(),
        "long" => "J".to_string(),
        "boolean" => "Z".to_string(),
        "byte" => "B".to_string(),
        "char" => "C".to_string(),
        "short" => "S".to_string(),
        "float" => "F".to_string(),
        "double" => "D".to_string(),
        e if e.starts_with('L') || e.starts_with('[') => e.to_string(),
        e => format!("L{e};"),
    };
    format!("[{desc}")
}

/// Resolve a value to its string text: a `Value::Str`, or a heap object's
/// `$str` field (e.g. one built by `new String(byte[])`), else its display.
/// `String.length()` over a `Value` (Str / ArenaString / heap `$str`) — char count.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn value_str_len(vm: &Vm, v: &Value) -> i32 {
    if let Value::Obj(p) = v {
        if crate::nextgen::is_arena_string(*p) {
            return crate::nextgen::arena_str_char_len(*p);
        }
    }
    // Count WITHOUT materialising a copy: the old `str_of(..)` path deep-copied
    // the whole text (O(n) malloc + memcpy) just to count it — a per-call host
    // cost the residue-window RIP profile billed to this symbol + the allocator.
    // ASCII (the overwhelming case) is chars == bytes; results are identical.
    fn char_len(s: &str) -> i32 {
        if s.is_ascii() { s.len() as i32 } else { s.chars().count() as i32 }
    }
    match v {
        Value::Str(s) => char_len(s),
        Value::Ref(h) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Str) {
            Value::Str(s) => char_len(&s),
            _ => 0, // matches str_of's None → 0
        },
        _ => str_of(vm, Some(v)).map(|s| char_len(&s)).unwrap_or(0),
    }
}
/// `String.charAt(i)` WITHOUT materialising a copy — the twin of `value_str_len`, and it was
/// missing for eleven months while `length` had it.
///
/// ⭐**What it replaces, measured.** The old path called `value_to_string`, which returns an
/// OWNED `String`: every single character read **allocated and memcpy'd the whole text**, then
/// counted its chars (`chars().count()`) for the bounds check, then walked to the index
/// (`chars().nth(i)`). Three O(n) passes and a malloc, per character. A guest loop over a string
/// — `for (i = 0; i < s.length(); i++) h ^= s.charAt(i)` — is therefore **O(n²) with an
/// allocation per iteration**, and that is not a corner case: it is how every tokenizer, hasher
/// and parser in the datapack phase reads text.
///
/// The `DataPackPhases` split priced it: the FOLD layer (exactly that loop, over 2.88 MB) took
/// **1,172 ms against `java`'s 14 — 83.7×**, the worst ratio of any layer measured, on a phase
/// worth 26% of the boot.
///
/// **The fast path is the PREFIX, not the whole string.** If bytes `0..=i` are all ASCII then
/// there are at least `i + 1` characters and character `i` IS byte `i`, so the index is in
/// bounds and the answer is one load. Checking only the prefix is both cheaper than
/// `is_ascii()` over the whole text and correct for a string whose non-ASCII lives past `i`.
/// Anything else falls back to the exact `chars()` walk, so surrogate mapping and the
/// exception's reported length are unchanged.
///
/// `Err(n)` = out of range, carrying the character length the caller's message must report.
pub(crate) fn value_str_charat(vm: &Vm, v: &Value, i: i32) -> Result<i32, i32> {
    fn at(s: &str, i: i32) -> Result<i32, i32> {
        fn char_len(s: &str) -> i32 {
            if s.is_ascii() { s.len() as i32 } else { s.chars().count() as i32 }
        }
        if i < 0 {
            return Err(char_len(s));
        }
        let b = s.as_bytes();
        let idx = i as usize;
        if idx < b.len() && b[..=idx].is_ascii() {
            // ASCII prefix ⇒ char index == byte index, and `unmap_surrogate` is the identity
            // below 0x80, so this is the same answer the walk below would give.
            return Ok(b[idx] as i32);
        }
        let n = s.chars().count();
        if idx >= n {
            return Err(n as i32);
        }
        Ok(s.chars()
            .nth(idx)
            .map(|c| rava_classfile::unmap_surrogate(c as u32) as i32)
            .unwrap_or(0))
    }
    if let Value::Obj(p) = v {
        if crate::nextgen::is_arena_string(*p) {
            let bytes = crate::nextgen::arena_str_bytes(*p);
            if let Ok(st) = std::str::from_utf8(bytes) {
                return at(st, i);
            }
        }
    }
    match v {
        Value::Str(s) => at(s, i),
        Value::Ref(h) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Str) {
            Value::Str(s) => at(&s, i),
            _ => Err(0),
        },
        _ => match str_of(vm, Some(v)) {
            Some(s) => at(&s, i),
            None => Err(0),
        },
    }
}

/// `BigDecimal.valueOf(long unscaledVal, int scale)` → unscaledVal × 10^(-scale).
pub(crate) fn scaled_text(unscaled: i64, scale: i32) -> String {
    if scale <= 0 {
        format!("{unscaled}{}", "0".repeat((-scale).min(40) as usize))
    } else {
        let sign = if unscaled < 0 { "-" } else { "" };
        let digits = unscaled.unsigned_abs().to_string();
        let scale = scale as usize;
        if digits.len() > scale {
            let (int, frac) = digits.split_at(digits.len() - scale);
            format!("{sign}{int}.{frac}")
        } else {
            format!("{sign}0.{}{digits}", "0".repeat(scale - digits.len()))
        }
    }
}

pub(crate) fn str_of(vm: &Vm, v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Str(s)) => Some(s.to_string()),
        // ArenaString: a `Value::Obj` of the string class — read its inline UTF-8
        // bytes directly (this is what protects the ~430 `Value::Str` call sites).
        #[cfg(feature = "next-gen-gc")]
        Some(Value::Obj(p)) if crate::nextgen::is_arena_string(*p) => {
            Some(String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p)).into_owned())
        }
        Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Str) {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        },
        Some(other) => Some(other.display()),
        None => None,
    }
}

/// `str_of` WITHOUT its rendering fallback — Some only when the value genuinely IS a string.
///
/// `str_of`'s last arm renders any value (`Some(other) => other.display()`), which is right
/// for concatenation and wrong for equality: it made `"1".equals(Integer.valueOf(1))` answer
/// TRUE, and a `String`-keyed map would then be reachable with a number
/// (bench/EqualsSem.java, `eq.otherType`). The three arms that remain are exactly the three
/// representations a real String has here: an inline `Value::Str`, an arena string object,
/// and a heap handle carrying `$str`.
pub(crate) fn as_string_strict(vm: &Vm, v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Str(s)) => Some(s.to_string()),
        #[cfg(feature = "next-gen-gc")]
        Some(Value::Obj(p)) if crate::nextgen::is_arena_string(*p) => {
            Some(String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p)).into_owned())
        }
        Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Str) {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn valueof_arg_class(vm: &Vm, v: Option<&Value>) -> String {
    match v {
        #[cfg(feature = "next-gen-gc")]
        Some(Value::Obj(p)) => {
            if crate::nextgen::is_arena_string(*p) {
                "ArenaString".to_string()
            } else {
                crate::nextgen::class_name(crate::nextgen::arena_class_id(*p))
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "arena?".to_string())
            }
        }
        Some(Value::Ref(h)) => vm.heap.class_of(*h).map(|s| s.to_string()).unwrap_or_else(|| "ref?".to_string()),
        Some(Value::Str(_)) => "Str".to_string(),
        Some(Value::Int(_)) => "int".to_string(),
        Some(Value::Long(_)) => "long".to_string(),
        Some(Value::Double(_)) => "double".to_string(),
        Some(Value::Null) | None => "null".to_string(),
        Some(other) => format!("{other:?}").chars().take(12).collect(),
    }
}
/// Resolve a value to a printable form: string-carrying objects (Path/File/
/// String/Class wrappers via `$str`/`$path`/`$name`) become their text;
/// primitives pass through unchanged (so `%d`/`%f` still see numbers).
pub(crate) fn stringify(vm: &mut Vm, v: &Value) -> Value {
    match v {
        // An ARENA object is an object like any other and must reach its guest
        // `toString()` too. Only `Value::Ref` did, so `String.format("%s", x)` /
        // `printf` on an arena-born object printed the debug placeholder
        // `<arena-object@0x…>` — `Value::display()`, which exists for traces, not
        // for the guest. Found on an enum (`%s` of a constant printed the address
        // where every other path printed its name); it is not enum-specific — every
        // arena receiver, including a record, formatted that way. Primitives still
        // pass through unchanged so `%d`/`%f` keep seeing numbers.
        //
        // ⛔**A NUMERIC CONVERSION CANNOT READ A PRE-RENDERED STRING.** This function exists so
        // `%s` reaches a guest `toString`, and it flattens EVERY reference to a `Value::Str`
        // before `format_java` sees it — which is right for `%s` and wrong for `%d`/`%x`/`%o`/
        // `%f`, all of which read their argument with `as_int`/`as_long`. Boxed `Short` and
        // `Byte` are real objects now (`crate::boxid`), so unbox them to the primitive instead:
        // their `%s` render IS the number (`Value::Int(9).display() == "9"`), so nothing is lost
        // and every numeric conversion keeps working.
        //
        // `Character` is deliberately NOT unboxed here — `%s` of it must print the CHARACTER,
        // not the code point — so it takes the render below and `%c` reads it back out of the
        // one-character string (see the `b'c'` arm in `format.rs`).
        Value::Ref(h)
            if matches!(
                vm.heap.class_of(*h).and_then(crate::boxid::Kind::of_class),
                Some(crate::boxid::Kind::Short) | Some(crate::boxid::Kind::Byte)
            ) =>
        {
            Value::Int(vm.heap.get_field(*h, "$value").as_int().unwrap_or(0))
        }
        Value::Ref(_) => match vm.value_to_string(v) {
            Ok(s) => Value::Str(s.into()),
            Err(_) => Value::Str(v.display().into()),
        },
        #[cfg(feature = "next-gen-gc")]
        Value::Obj(_) => match vm.value_to_string(v) {
            Ok(s) => Value::Str(s.into()),
            Err(_) => Value::Str(v.display().into()),
        },
        other => other.clone(),
    }
}

/// Compute a cryptographic digest of `data` for a JCA algorithm name.
pub(crate) fn md_hash(algo: &str, data: &[u8]) -> Vec<u8> {
    use digest::Digest;
    let norm = algo.to_uppercase().replace(['-', '_'], "");
    match norm.as_str() {
        "SHA1" | "SHA" => sha1::Sha1::digest(data).to_vec(),
        "SHA224" | "SHA2224" => sha2::Sha224::digest(data).to_vec(),
        "SHA384" | "SHA2384" => sha2::Sha384::digest(data).to_vec(),
        "SHA512" | "SHA2512" => sha2::Sha512::digest(data).to_vec(),
        "MD5" => md5::Md5::digest(data).to_vec(),
        // SHA-256 and anything unrecognised default to SHA-256 (the common case).
        _ => sha2::Sha256::digest(data).to_vec(),
    }
}

/// Format an integer in an arbitrary radix (2..=36), Java-style.
pub(crate) fn to_radix(mut n: i64, radix: u32) -> String {
    if !(2..=36).contains(&radix) { return n.to_string(); }
    if n == 0 { return "0".to_string(); }
    let neg = n < 0;
    let mut digits = Vec::new();
    let mut un = (n as i128).unsigned_abs();
    let _ = &mut n;
    let r = radix as u128;
    while un > 0 {
        let d = (un % r) as u32;
        digits.push(std::char::from_digit(d, radix).unwrap());
        un /= r;
    }
    if neg { digits.push('-'); }
    digits.iter().rev().collect()
}

/// The same, for a value read as UNSIGNED (`Integer.toUnsignedString(i, radix)`).
/// Separate from `to_radix` because a u64 above `i64::MAX` has no i64 spelling —
/// routing it through the signed formatter is what produced "-1" for
/// `Long.toUnsignedString(-1L, 16)`. An out-of-range radix falls back to 10, as
/// the JDK's `Integer.toUnsignedString` does.
pub(crate) fn to_radix_unsigned(mut n: u64, radix: u32) -> String {
    if !(2..=36).contains(&radix) { return n.to_string(); }
    if n == 0 { return "0".to_string(); }
    let mut digits = Vec::new();
    let r = radix as u64;
    while n > 0 {
        digits.push(std::char::from_digit((n % r) as u32, radix).unwrap());
        n /= r;
    }
    digits.iter().rev().collect()
}

/// (year, month 1-12, day 1-31) from a count of days since the Unix epoch.
/// Howard Hinnant's civil-from-days algorithm.
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub(crate) fn identity_hash(v: Option<&Value>) -> i32 {
    match v {
        Some(Value::Ref(h)) => *h as i32,
        Some(Value::Int(i)) => *i,
        // Arena object: a GC-stable hash lazily stamped into its header. Without
        // it every Value::Obj hashed to 0 and MC's RegistryID rehash overflowed.
        // B5 IDENTITY-HASH BRIDGE: a PINNED MIRROR pointer is the same object as
        // its handle — it must hash exactly as Ref(h) does (h as i32), or a map
        // keyed under one form misses the other. Inert until pinned ptrs flow.
        #[cfg(feature = "next-gen-gc")]
        Some(Value::Obj(p)) => match crate::nextgen::pinned_handle_of(p.addr()) {
            Some(h) => h as i32,
            None => crate::nextgen::arena_identity_hash(*p),
        },
        _ => 0,
    }
}

pub(crate) fn ref_eq(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (Some(Value::Ref(x)), Some(Value::Ref(y))) => x == y,
        (Some(Value::Null), Some(Value::Null)) => true,
        (Some(Value::Str(x)), Some(Value::Str(y))) => x == y,
        // Arena objects: identity is the current address. Both operands are live
        // at the comparison and the GC moves them only at a safepoint (never
        // mid-compare), so address equality is a sound identity test.
        #[cfg(feature = "next-gen-gc")]
        (Some(Value::Obj(x)), Some(Value::Obj(y))) => x.addr() == y.addr(),
        // B5 IDENTITY BRIDGE (see opcodes::ref_equal): Obj(pinned-mirror-of-h) and
        // Ref(h) are the SAME object during coexistence. Inert until ptrs flow.
        #[cfg(feature = "next-gen-gc")]
        (Some(Value::Obj(p)), Some(Value::Ref(h))) | (Some(Value::Ref(h)), Some(Value::Obj(p))) => {
            crate::nextgen::pinned_handle_of(p.addr()) == Some(*h)
        }
        _ => false,
    }
}
