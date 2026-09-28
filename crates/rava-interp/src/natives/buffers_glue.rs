//! Buffer construction and the small string predicates around the natives.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// THE REMAINING-NATIVE-SURFACE gate. DEFAULT ON; `RAVA_NO_FAST_NATSURF=1` (or the
/// tree-wide `RAVA_NO_EMIT_MORE=1`) restores the materialise-everything forms of every
/// fast path in `streams.rs` and `buffers_glue.rs` — the whole-stream read, the bulk
/// `readLine`, the empty-`$pushback` skip and the ASCII `regionMatches` window — so the
/// family bisects in ONE run instead of one flag per call site. (The `crypto.rs` changes
/// are not gated: they are refactors whose output is byte-identical, plus two bug fixes.)
///
/// State: a single `OnceLock<bool>` — 16 bytes, written once, read-only thereafter.
/// Nothing here competes for L1 with the buffers it guards.
pub(crate) fn fast_natsurf_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_NATSURF").is_none() && !crate::flags::emit_more_disabled()
    })
}

pub(crate) fn gate_census() -> &'static std::sync::Mutex<GateCensus> {
    pub(crate) static T: std::sync::OnceLock<std::sync::Mutex<GateCensus>> = std::sync::OnceLock::new();
    T.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// The non-owner half of the whole-cascade skip: no owner-agnostic name, and no
/// receiver-shaped arm (`map::dispatch` serves an arena-map receiver before it
/// consults the owner, but only under RAVA_ARENA_MAPS). The owner half is
/// `plan & DISPATCH_ANY == 0`.
#[inline]
pub(crate) fn no_owner_free_arm_applies(name: &str, args: &[Value]) -> bool {
    !owner_agnostic_name(name) && !arena_map_receiver(args)
}

/// Read the name stored in a `java/lang/Class` instance.
pub(crate) fn class_field_name(vm: &Vm, v: Option<&Value>) -> String {
    match v {
        Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Name) {
            Value::Str(s) => s.to_string(),
            _ => "java/lang/Object".to_string(),
        },
        _ => "java/lang/Object".to_string(),
    }
}

/// `String.hashCode()` over a `Value` — the cached arena hash, else recompute.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn value_str_hash(vm: &Vm, v: &Value) -> i32 {
    if let Value::Obj(p) = v {
        if crate::nextgen::is_arena_string(*p) {
            return crate::nextgen::arena_str_hash(*p);
        }
    }
    // A CLONE TAKEN FOR A VALUE THAT IS ONLY READ. `str_of` returns an owned `String`, so
    // every `String.hashCode()` from compiled code allocated (and freed) a full copy of the
    // string just to fold 31*h+c over it — the fold never needs ownership. Inline `Value::Str`
    // is the representation the non-arena half of the heap uses, so it is the whole population
    // this arm serves. WAS: one heap allocation + one memcpy per hashCode, sized by the
    // string's bytes. IS: zero allocations; the fold reads the borrowed bytes in place.
    // Identical arithmetic (same `chars()` iteration), so no gate — nothing observable moves.
    if let Value::Str(s) = v {
        return s.chars().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32));
    }
    str_of(vm, Some(v))
        .map(|s| s.chars().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32)))
        .unwrap_or(0)
}

/// regionMatches with optional case-insensitive comparison (char-index based).
pub(crate) fn region_matches_ci(s: &str, toff: i32, other: &str, ooff: i32, len: i32, ignore: bool) -> bool {
    if toff < 0 || ooff < 0 { return false; }
    // A WHOLE COLLECTION MATERIALISED TO LOOK AT `len` OF ITS ELEMENTS, TWICE.
    // The general path below collects BOTH strings into `Vec<char>` — two heap allocations
    // sized by the FULL strings — in order to compare a window of `len` characters, and
    // `String.regionMatches` is what `startsWith`/`endsWith`/`equalsIgnoreCase` reduce to on
    // paths that compare a 3-char prefix of a 200-char resource path.
    // WAS: O(chars per string) allocation + O(chars per string) decode, per call.
    // IS: zero allocations, and a memcmp over exactly `len` bytes.
    // PRECONDITION, and it is why this is safe: both strings ASCII ⇒ byte index == char
    // index and byte value == char value, so every index and every comparison below is the
    // same one the general path computes. Not ASCII ⇒ fall through invisibly.
    if fast_natsurf_on() && s.is_ascii() && other.is_ascii() {
        let (sb, ob) = (s.as_bytes(), other.as_bytes());
        let (t, o, n) = (toff as i64, ooff as i64, len as i64);
        if t > sb.len() as i64 - n || o > ob.len() as i64 - n { return false; }
        if n <= 0 { return true; }
        let (t, o, n) = (t as usize, o as usize, n as usize);
        return if ignore {
            (0..n).all(|i| java_eq_ignore_case(sb[t + i] as char, ob[o + i] as char))
        } else {
            sb[t..t + n] == ob[o..o + n]
        };
    }
    let (sc, oc): (Vec<char>, Vec<char>) = (s.chars().collect(), other.chars().collect());
    // Java's guard is `toffset > length() - len` in 64-bit arithmetic, then a
    // `while (len-- > 0)` loop. A NEGATIVE len therefore passes the guard and
    // compares nothing, so the answer is TRUE — rejecting it up front (the old
    // `len < 0 => false`) inverted the result the JVM gives.
    let (t, o, n) = (toff as i64, ooff as i64, len as i64);
    if t > sc.len() as i64 - n || o > oc.len() as i64 - n { return false; }
    if n <= 0 { return true; }
    // Case folding must be Java's char rule, not ASCII: `"É".equalsIgnoreCase("é")`
    // and the Greek/Cyrillic pairs answered false under `to_ascii_lowercase`.
    (0..n as usize).all(|i| {
        let (a, b) = (sc[t as usize + i], oc[o as usize + i]);
        if ignore { java_eq_ignore_case(a, b) } else { a == b }
    })
}

pub(crate) fn make_buffer(vm: &mut Vm, buf: u32, pos: usize, limit: usize, cap: usize) -> Value {
    let h = vm.heap.alloc("java/nio/ByteBuffer");
    vm.heap.put_field(h, "$buf", Value::Array(buf));
    vm.heap.put_field_syn(h, crate::heap::Syn::Pos, Value::Int(pos as i32));
    vm.heap.put_field(h, "$limit", Value::Int(limit as i32));
    vm.heap.put_field(h, "$cap", Value::Int(cap as i32));
    vm.heap.put_field(h, "$order", Value::Str("BE".into()));
    Value::Ref(h)
}

pub(crate) fn parse_str(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Str(s)) => s.trim().parse::<i64>().ok(),
        // Compiled callers (Integer/Long.parseInt of a concat) pass an ArenaString.
        #[cfg(feature = "next-gen-gc")]
        Some(Value::Obj(p)) if crate::nextgen::is_arena_string(*p) => {
            String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p)).trim().parse::<i64>().ok()
        }
        _ => None,
    }
}
