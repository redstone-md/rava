//! String building, searching and splitting shared by the string families.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// `Character.isWhitespace`, which is NOT Rust's `char::is_whitespace`. The two
/// disagree in both directions and the difference is guest-visible through
/// `strip`/`isBlank`/`String.split("\\s")`:
///   * Java counts the four information separators U+001C..U+001F as whitespace;
///     Unicode's `White_Space` property does not.
///   * Java EXCLUDES the non-breaking spaces U+00A0 / U+2007 / U+202F (they are
///     `Zs`, but the spec carves them out); Unicode counts A0 and 202F.
///   * U+0085 NEL is `White_Space` in Unicode but a `Cc` with no separator
///     category, so Java answers false.
pub(crate) fn java_is_whitespace(c: char) -> bool {
    match c {
        '\u{9}'..='\u{d}' | '\u{1c}'..='\u{1f}' => true,
        '\u{a0}' | '\u{2007}' | '\u{202f}' | '\u{85}' => false,
        _ => c.is_whitespace(),
    }
}

/// `Character.toUpperCase(char)` — the SINGLE-char mapping. Rust's
/// `char::to_uppercase` is the full mapping and can yield several chars ('ß' →
/// "SS"); Java's char overload leaves such a char unchanged because the result
/// would not fit in one code unit. Only the case-insensitive comparisons use
/// this — `String.toUpperCase` correctly uses the full mapping.
pub(crate) fn java_to_upper(c: char) -> char {
    let mut it = c.to_uppercase();
    match (it.next(), it.next()) { (Some(u), None) => u, _ => c }
}

/// `Character.toLowerCase(char)` — the single-char mapping; see `java_to_upper`.
pub(crate) fn java_to_lower(c: char) -> char {
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) { (Some(l), None) => l, _ => c }
}

/// Java's case-insensitive char equality, as `String.regionMatches(true, ..)`
/// spells it: compare the uppercase forms, and if those differ fall back to the
/// lowercase of the uppercase forms (the second test is what makes Georgian and
/// the Turkish dotless 'ı' compare equal to 'I').
pub(crate) fn java_eq_ignore_case(a: char, b: char) -> bool {
    if a == b { return true; }
    let (ua, ub) = (java_to_upper(a), java_to_upper(b));
    ua == ub || java_to_lower(ua) == java_to_lower(ub)
}

// ── JDK bounds-failure messages ────────────────────────────────────────────────
// Guest code reads `getMessage()`, so the wording is part of the contract, not
// decoration. All three are the `jdk.internal.util.Preconditions` formats that
// String/AbstractStringBuilder/List route their checks through:
//   checkIndex          → "Index %d out of bounds for length %d"
//   checkFromToIndex    → "Range [%d, %d) out of bounds for length %d"
//   checkFromIndexSize  → "Range [%d, %d + %d) out of bounds for length %d"
// The third keeps `off + count` UNEVALUATED — that is literally what the JDK
// prints ("Range [1, 1 + 5) out of bounds for length 3"), because the sum may
// have overflowed.

/// `StringIndexOutOfBoundsException` for an (offset, count) pair — the form
/// `String.valueOf(char[],int,int)`, `String.copyValueOf` and the
/// `new String(char[]/byte[], off, count)` constructors use.
pub(crate) fn str_offcount_oob(vm: &mut Vm, off: i32, count: i32, length: usize) -> VmError {
    let ex = vm.make_exception(
        "java/lang/StringIndexOutOfBoundsException",
        &format!("Range [{off}, {off} + {count}) out of bounds for length {length}"),
    );
    VmError::Throw(ex)
}

/// The JDK's own `checkFromIndexSize` predicate: true when the (off, count)
/// window is entirely inside `length`, with the overflow-safe comparison the
/// JDK uses (`(length | off | count) < 0 || count > length - off`).
pub(crate) fn off_count_ok(off: i32, count: i32, length: usize) -> bool {
    let len = length as i64;
    let (off, count) = (off as i64, count as i64);
    off >= 0 && count >= 0 && count <= len - off
}

/// The first error a `replaceAll`/`replaceFirst` REPLACEMENT string carries, as
/// `(exception class, message)` — or None when it is well formed.
///
/// Mirrors `Matcher.appendExpandedReplacement`, which rejects three separate
/// shapes and not just one: a `$N` naming no capturing group is an
/// IndexOutOfBoundsException, while a trailing `\`, a trailing `$`, and a `$`
/// followed by anything that is neither a digit nor `{` are each an
/// IllegalArgumentException. The first cut detected only the first of those and
/// let the other three through as literal text — a silently different result.
///
/// A `$` followed by digits takes the LARGEST prefix that is still a legal group.
/// Because the greedy extension only accepts a number `<= group_count`, the
/// reference can only end up out of range when it is the FIRST digit alone, so
/// testing that one digit is exactly equivalent to running the JDK's loop.
pub(crate) fn replacement_error(replacement: &str, group_count: usize) -> Option<(&'static str, String)> {
    let b = replacement.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            // A trailing backslash has nothing to escape.
            b'\\' => {
                if i + 1 >= b.len() {
                    return Some((
                        "java/lang/IllegalArgumentException",
                        "character to be escaped is missing".to_string(),
                    ));
                }
                i += 2;
            }
            b'$' => {
                match b.get(i + 1) {
                    // A `$` at the very end names no group at all.
                    None => {
                        return Some((
                            "java/lang/IllegalArgumentException",
                            "Illegal group reference: group index is missing".to_string(),
                        ))
                    }
                    Some(c) if c.is_ascii_digit() => {
                        let d = (c - b'0') as usize;
                        if d > group_count {
                            return Some((
                                "java/lang/IndexOutOfBoundsException",
                                format!("No group {d}"),
                            ));
                        }
                        i += 2;
                    }
                    // `${name}` is a named reference and a different JDK path; it is
                    // left alone rather than judged here.
                    Some(b'{') => i += 2,
                    // Anything else after `$` is not a reference the JDK accepts.
                    Some(_) => {
                        return Some((
                            "java/lang/IllegalArgumentException",
                            "Illegal group reference".to_string(),
                        ))
                    }
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Build the text for a `new String(...)` from its constructor descriptor:
/// char[]/byte[] (with optional offset+length) or a copied String/CharSequence.
///
/// The (off, count) constructors are BOUNDS-CHECKED like the JDK's: they used to
/// `skip(off).take(len)`, which silently produced a short (or empty) string where
/// the JVM raises StringIndexOutOfBoundsException, and a null array produced the
/// four-character text "null" where the JVM raises NullPointerException.
pub(crate) fn build_string(vm: &mut Vm, descriptor: &str, args: &[Value]) -> Result<String, VmError> {
    let src = args.get(1);
    let off = args.get(2).and_then(Value::as_int).unwrap_or(0);
    // A null array reaches `Arrays.copyOfRange`/`value.length` in the JDK before
    // any bounds test, so the NPE (with the helpful-message wording naming the
    // parameter) precedes the range check.
    if (descriptor.starts_with("([C") || descriptor.starts_with("([B") || descriptor.starts_with("([I"))
        && matches!(src, Some(Value::Null) | None)
    {
        // The helpful NPE names the PARAMETER, and `String`'s three array
        // constructors do not share one: `value` for char[], `bytes` for byte[],
        // `codePoints` for int[]. One name for all three put the wrong identifier
        // in a message written to be read.
        let param = match descriptor.as_bytes().get(2) {
            Some(b'B') => "bytes",
            Some(b'I') => "codePoints",
            _ => "value",
        };
        let ex = vm.make_exception(
            "java/lang/NullPointerException",
            &format!("Cannot read the array length because \"{param}\" is null"),
        );
        return Err(VmError::Throw(ex));
    }
    if descriptor.starts_with("([C") {
        // char[] of int code points
        if let Some(Value::Array(h)) = src {
            let all = vm.heap.array_values(*h).unwrap_or_default();
            let (off, len) = if descriptor.starts_with("([CII") {
                (off, args.get(3).and_then(Value::as_int).unwrap_or(0))
            } else { (0, all.len() as i32) };
            if !off_count_ok(off, len, all.len()) {
                return Err(str_offcount_oob(vm, off, len, all.len()));
            }
            return Ok(all.into_iter().skip(off as usize).take(len as usize)
                .filter_map(|v| char::from_u32(v.as_int().unwrap_or(0) as u32)).collect());
        }
    } else if descriptor.starts_with("([B") {
        if let Some(Value::Array(h)) = src {
            let all = vm.heap.array_bytes(*h).unwrap_or_default();
            // ([BII...) has offset+len; bare ([B...) is whole.
            let (start, len) = if descriptor.starts_with("([BII") {
                (off, args.get(3).and_then(Value::as_int).unwrap_or(0))
            } else { (0, all.len() as i32) };
            if !off_count_ok(start, len, all.len()) {
                return Err(str_offcount_oob(vm, start, len, all.len()));
            }
            let slice: Vec<u8> = all.into_iter().skip(start as usize).take(len as usize).collect();
            return Ok(String::from_utf8_lossy(&slice).into_owned());
        }
    } else if descriptor.starts_with("([I") {
        // int[] code points (offset, count)
        if let Some(Value::Array(h)) = src {
            let all = vm.heap.array_values(*h).unwrap_or_default();
            let len = args.get(3).and_then(Value::as_int).unwrap_or(all.len() as i32);
            if !off_count_ok(off, len, all.len()) {
                return Err(str_offcount_oob(vm, off, len, all.len()));
            }
            return Ok(all.into_iter().skip(off as usize).take(len as usize)
                .filter_map(|v| char::from_u32(v.as_int().unwrap_or(0) as u32)).collect());
        }
    }
    // (String) / (CharSequence) / (StringBuilder) copy, or empty.
    Ok(str_of(vm, src).unwrap_or_default())
}

/// indexOf/lastIndexOf for either a substring or a single char code, honouring
/// an optional `fromIndex` (the two-arg overloads). Works in char-index space
/// to match Java semantics. Ignoring `fromIndex` previously broke any code that
/// scans a string segment-by-segment (e.g. FileUtil.decomposePath, which kept
/// re-finding the first '/' and produced empty path segments).
pub(crate) fn index_of(s: &str, arg: Option<&Value>, from: Option<&Value>, vm: &Vm, last: bool) -> i32 {
    let needle: Vec<char> = match arg {
        // indexOf(int codePoint): a value outside [0, 0x10FFFF] (or a lone
        // surrogate) names no character, so Java returns -1. Letting
        // `from_u32` fail into an EMPTY needle instead made `indexOf(-1)`
        // answer 0 — the "found at the start" reply, the opposite of absent.
        Some(Value::Int(c)) => match char::from_u32(*c as u32) {
            Some(ch) => vec![ch],
            None => return -1,
        },
        other => str_of(vm, other).unwrap_or_default().chars().collect(),
    };
    let hay: Vec<char> = s.chars().collect();
    let nlen = needle.len();
    let from = from.and_then(Value::as_int);
    let matches_at = |i: usize| hay[i..].starts_with(&needle);
    if last {
        // Largest index <= fromIndex (default end) where the needle starts.
        // A NEGATIVE fromIndex names no searchable region, so Java answers -1
        // outright. Clamping it to 0 instead let an EMPTY needle report a hit
        // at 0 — `lastIndexOf("", -5)` said 0 where the JVM says -1.
        if matches!(from, Some(f) if f < 0) { return -1; }
        let max_start = hay.len().saturating_sub(nlen);
        let limit = from.map(|f| (f.max(0) as usize).min(max_start)).unwrap_or(max_start);
        for i in (0..=limit).rev() {
            if matches_at(i) { return i as i32; }
        }
        -1
    } else {
        let start = from.map(|f| f.max(0) as usize).unwrap_or(0);
        if nlen == 0 { return (start.min(hay.len())) as i32; }
        for i in start..=hay.len().saturating_sub(nlen) {
            if matches_at(i) { return i as i32; }
        }
        -1
    }
}

/// Java `String.split`: the delimiter is a regex. Trailing empties are dropped
/// when limit == 0. Falls back to literal splitting if the regex won't compile.
pub(crate) fn split_regex(s: &str, pat: &str, limit: i32) -> Vec<String> {
    // Literal delimiters (`:`, `/`, …) — the overwhelmingly common case in
    // resource-location parsing — skip the regex engine entirely.
    let literal_split = |p: &str| -> Vec<String> {
        if limit > 0 {
            s.splitn(limit as usize, p).map(String::from).collect()
        } else {
            s.split(p).map(String::from).collect()
        }
    };
    // The literal fast path is only equivalent to Java for a NON-EMPTY literal:
    // an empty (or zero-width-matching) pattern needs Java's leading-match rule,
    // which `str::split` does not have — it emits a leading "" that Java drops.
    let mut parts: Vec<String> = if regex_is_literal(pat) {
        literal_split(pat)
    } else {
        match cached_regex(pat) {
            Some(re) => return split_pattern(&re, s, limit),
            None => literal_split(pat),
        }
    };
    // Java returns `{ input }` whenever the delimiter never matched, BEFORE the
    // trailing-empty trim can see it. Without this guard `"".split(",")` — one
    // segment, itself empty — was trimmed down to a ZERO-length array where the
    // JVM hands back a one-element array holding "".
    if parts.len() == 1 {
        return parts;
    }
    if limit == 0 {
        while matches!(parts.last(), Some(p) if p.is_empty()) {
            parts.pop();
        }
    }
    parts
}

/// `java.util.regex.Pattern.split`, ported verbatim. Rust's `Regex::split` is not
/// a substitute: on a pattern that can match the empty string it yields a leading
/// empty field ("abc".split("") → ["", "a", "b", "c", ""]), whereas Java skips a
/// zero-width match sitting at index 0 ONLY — giving ["a", "b", "c"] at limit 0
/// and ["a", "b", "c", ""] at a negative limit.
fn split_pattern(re: &regex::Regex, s: &str, limit: i32) -> Vec<String> {
    let match_limited = limit > 0;
    let cap = if match_limited { limit as usize } else { usize::MAX };
    let mut out: Vec<String> = Vec::new();
    let mut index = 0usize; // BYTE offset just past the previous delimiter
    for m in re.find_iter(s) {
        if !match_limited || out.len() < cap - 1 {
            // The one zero-width match Java refuses to split on.
            if index == 0 && m.start() == 0 && m.end() == 0 {
                continue;
            }
            out.push(s[index..m.start()].to_string());
            index = m.end();
        } else if out.len() == cap - 1 {
            out.push(s[index..].to_string());
            index = m.end();
        }
    }
    if index == 0 {
        return vec![s.to_string()]; // no delimiter matched
    }
    if !match_limited || out.len() < cap {
        out.push(s[index..].to_string());
    }
    if limit == 0 {
        while matches!(out.last(), Some(p) if p.is_empty()) {
            out.pop();
        }
    }
    out
}

/// Format a number for DecimalFormat: `frac` fraction digits, optional `,`
/// grouping of the integer part. US/ROOT conventions ('.' decimal, ',' group).
pub(crate) fn format_decimal(value: f64, min_frac: usize, max_frac: usize, grouping: bool) -> String {
    let neg = value.is_sign_negative() && value != 0.0;
    let v = value.abs();
    // Round to max_frac, then trim trailing zeros down to min_frac.
    let mut s = format!("{:.*}", max_frac, v);
    if max_frac > min_frac {
        if let Some(dot) = s.find('.') {
            let keep_min = dot + 1 + min_frac;
            let mut end = s.len();
            while end > keep_min && s.as_bytes()[end - 1] == b'0' { end -= 1; }
            if end > 0 && s.as_bytes()[end - 1] == b'.' { end -= 1; }
            s.truncate(end);
        }
    }
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s.clone(), None),
    };
    let int_grouped: String = if grouping && int_part.len() > 3 {
        let mut g = String::new();
        for (i, ch) in int_part.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 { g.push(','); }
            g.push(ch);
        }
        g.chars().rev().collect()
    } else {
        int_part
    };
    let body = match frac_part {
        Some(f) => format!("{int_grouped}.{f}"),
        None => int_grouped,
    };
    if neg { format!("-{body}") } else { body }
}

/// Substitute slf4j `{}` placeholders with the params, in order.
pub(crate) fn slf4j_format(vm: &mut Vm, template: &str, params: &[Value]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut pi = 0;
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'}' {
            if let Some(p) = params.get(pi) {
                out.push_str(&vm.value_to_string(p).unwrap_or_else(|_| p.display()));
                pi += 1;
            } else {
                out.push_str("{}");
            }
            i += 2;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Emit an slf4j/log4j log line to the real process stdout, mimicking
/// Minecraft's console layout `[HH:mm:ss] [ServerMain/LEVEL]: msg`. Only
/// INFO/WARN/ERROR reach the console (matching the default log4j level), so the
/// server's own messages (e.g. the EULA notice) are visible — our VM doesn't
/// surface log4j's ConsoleAppender otherwise. Generic for any slf4j logger.
pub(crate) fn slf4j_log(vm: &mut Vm, level: &str, args: &[Value]) {
    if matches!(level, "debug" | "trace") {
        return; // below the console threshold
    }
    // args[0] is the logger; an optional Marker (Ref, not Str) precedes the
    // String template; the rest are format params.
    let mut rest = args.iter().skip(1);
    let template = loop {
        match rest.next() {
            Some(Value::Str(s)) => break s.to_string(),
            Some(_) => continue, // skip a leading Marker
            None => return,
        }
    };
    let mut params: Vec<Value> = rest.cloned().collect();
    // error(String, Object...) packs 3+ logical args into a single Object[].
    // slf4j treats a lone trailing Object[] as the argument array — spread it so
    // each {} placeholder maps to one element (else the whole array fills the
    // first {} and renders as <array@N>).
    if let [Value::Array(h)] = params.as_slice() {
        if let Some(vs) = vm.heap.array_values(*h) { params = vs; }
    }
    let msg = slf4j_format(vm, &template, &params);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let line = format!("[{h:02}:{m:02}:{s:02}] [ServerMain/{}]: {msg}", level.to_uppercase());
    vm.print(&line, true);
}
