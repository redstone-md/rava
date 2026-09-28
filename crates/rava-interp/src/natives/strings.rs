//! String, StringBuilder, CharBuffer and text formatting served natively.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// `StringIndexOutOfBoundsException` in the JVM's exact wording. The message is
/// guest-visible (it reaches logs and `getMessage()`), so the two spellings Java
/// uses are reproduced verbatim rather than paraphrased.
fn str_index_oob(vm: &mut Vm, msg: String) -> VmError {
    VmError::Throw(vm.make_exception("java/lang/StringIndexOutOfBoundsException", &msg))
}

/// `RAVA_NO_SB_FAST=1` restores the old `StringBuilder.length`/`charAt`, which rebuilt the whole
/// buffer into a `String` to answer a question about one element. Kept as a kill switch because
/// "strictly less work" is a claim about the code, not a measurement, and this tree has shipped
/// a cache that was slower than the allocation it removed.
fn sb_fast_off() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("RAVA_NO_SB_FAST").is_some())
}

/// Generic `java.lang.String` instance/static methods (all return a value).
///
/// Returns `Err(VmError::Throw)` for the range checks the JVM performs: those
/// arms used to CLAMP, which turned a program's bug into silently wrong text
/// instead of the exception the guest's own error handling is written against.
pub(crate) fn str_dispatch(vm: &mut Vm, name: &str, descriptor: &str, args: &[Value]) -> Result<Option<Value>, VmError> {
    // Static methods first.
    if name == "valueOf" || name == "copyValueOf" {
        // RAVA_VALUEOF_PROBE (inert): the boot bridge-census measured String#valueOf at ~28s /
        // 2.1M calls / 45µs each — pathological. Tally the ARGUMENT's concrete class + time +
        // result length so the expensive toString tree is NAMED before any fix is designed.
        let vp = if valueof_probe_enabled() {
            Some((valueof_arg_class(vm, args.first()), std::time::Instant::now()))
        } else {
            None
        };
        // Stringify by the declared parameter type: valueOf(char[][,off,count]),
        // valueOf(boolean), valueOf(char) need special handling; otherwise the
        // Object overload uses the value's toString. (valueOf(char[]) previously
        // stringified the ARRAY itself → "<array@..>", corrupting gson strings.)
        let t = descriptor.as_bytes().get(1).copied();
        let s = match (t, args.first()) {
            (Some(b'Z'), Some(v)) => if v.as_int().unwrap_or(0) != 0 { "true".to_string() } else { "false".to_string() },
            (Some(b'C'), Some(v)) => char::from_u32(v.as_int().unwrap_or(0) as u32).map(String::from).unwrap_or_default(),
            (Some(b'['), Some(Value::Array(h))) => {
                let all = vm.heap.array_values(*h).unwrap_or_default();
                let (off, len) = if descriptor.starts_with("([CII") {
                    (args.get(1).and_then(Value::as_int).unwrap_or(0),
                     args.get(2).and_then(Value::as_int).unwrap_or(all.len() as i32))
                } else { (0, all.len() as i32) };
                // valueOf/copyValueOf(char[],off,count) delegate to `new String(char[],
                // off,count)`, which range-checks. Clamping here (the old `.max(0)` +
                // skip/take) returned a SHORT string where the JVM throws.
                if !off_count_ok(off, len, all.len()) {
                    return Err(str_offcount_oob(vm, off, len, all.len()));
                }
                all.into_iter().skip(off as usize).take(len as usize)
                    .filter_map(|v| char::from_u32(v.as_int().unwrap_or(0) as u32)).collect()
            }
            // A null char[] is dereferenced for its length by the JDK before any
            // bounds test — `String.valueOf((char[])null, 0, 1)` is an NPE there,
            // not the literal text "null" this arm used to fall through to.
            // Both `String.valueOf(char[])` and `String.valueOf(char[],off,count)`
            // dereference the array for its length before anything else, so a null
            // is an NPE in either — the single-argument form used to fall through to
            // the generic stringify arm and answer the text "null".
            (Some(b'['), Some(Value::Null) | None) => {
                let ex = vm.make_exception(
                    "java/lang/NullPointerException",
                    "Cannot read the array length because \"value\" is null",
                );
                return Err(VmError::Throw(ex));
            }
            (_, Some(v)) => vm.value_to_string(v).unwrap_or_else(|_| v.display()),
            (_, None) => "null".to_string(),
        };
        if let Some((cls, t0)) = vp {
            valueof_probe_record(cls, t0.elapsed().as_nanos() as u64, s.len());
        }
        return Ok(Some(Value::Str(s.into())));
    }
    if name == "join" {
        let delim = str_of(vm, args.first()).unwrap_or_default();
        let items: Vec<Value> = match args.get(1) {
            Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default(),
            Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Elems) {
                // Spare capacity is not elements — see `Heap::elems_values`.
                Value::Array(a) => vm.heap.elems_values(*h, a),
                _ => args[1..].to_vec(),
            },
            _ => args[1..].to_vec(),
        };
        let mut parts = Vec::with_capacity(items.len());
        for it in &items { parts.push(vm.value_to_string(it).unwrap_or_else(|_| it.display())); }
        return Ok(Some(Value::Str(parts.join(&delim).into())));
    }
    if name == "format" {
        // format(String, Object[]) or format(Locale, String, Object[]). Keyed on the
        // DESCRIPTOR: the value test `args[0] is Value::Str` silently mis-selects when
        // the format arrives as an arena string (Value::Obj) — then fmt would be read
        // from the varargs array and the whole result would be empty.
        let (fi, ai) = if descriptor.starts_with("(Ljava/util/Locale;") { (1, 2) } else { (0, 1) };
        let fmt = str_of(vm, args.get(fi)).unwrap_or_default();
        let raw = varargs(vm, args.get(ai));
        let mut fargs = Vec::with_capacity(raw.len());
        for v in &raw { fargs.push(stringify(vm, v)); }
        return match format_java(&fmt, &fargs) {
            Ok(t) => Ok(Some(Value::Str(t.into()))),
            Err((class, msg)) => Err(VmError::Throw(vm.make_exception(class, &msg))),
        };
    }
    pub(crate) static TSTR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if crate::jit::env_gate("RAVA_TRACE_STR", &TSTR) {
        eprintln!("[STR] {name} recv={:?} ok={}", args.first().map(|v| v.display()), str_of(vm, args.first()).is_some());
    }
    // ⭐**THE COPY-FREE READS, BEFORE THE TEXT IS MATERIALISED.** `str_of` returns an owned
    // `String`: it allocates and memcpy's the whole receiver. For `length` and `charAt` — the two
    // reads a guest loop makes per iteration — that turns an O(1) operation into O(n) with a
    // malloc, and the loop into O(n²). `value_str_len` was fixed for exactly this reason; its
    // twin `value_str_charat` did not exist, so `charAt` kept paying it.
    if let Some(recv) = args.first() {
        match name {
            "length" => return Ok(Some(Value::Int(crate::natives::value_str_len(vm, recv)))),
            "charAt" => {
                let i = args.get(1).and_then(Value::as_int).unwrap_or(0);
                return match crate::natives::value_str_charat(vm, recv, i) {
                    Ok(c) => Ok(Some(Value::Int(c))),
                    Err(n) => Err(str_index_oob(vm, format!("Index {i} out of bounds for length {n}"))),
                };
            }
            _ => {}
        }
    }
    let Some(s) = str_of(vm, args.first()) else { return Ok(None) };
    let arg1 = args.get(1);
    let ret = match name {
        "length" => Value::Int(s.chars().count() as i32),
        "isEmpty" => Value::Int(s.is_empty() as i32),
        // isBlank/strip test Character.isWhitespace, NOT Unicode White_Space:
        // `"\u{a0}".isBlank()` is false on the JVM and `"\u{1f}".isBlank()` is
        // true. `str::trim` answers the opposite on both. See java_is_whitespace.
        "isBlank" => Value::Int(s.chars().all(java_is_whitespace) as i32),
        "hashCode" => Value::Int(s.chars().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))),
        "charAt" | "codePointAt" => {
            let i = arg1.and_then(Value::as_int).unwrap_or(0);
            let n = s.chars().count();
            // Java bounds-checks before reading. Answering 0 for an out-of-range
            // index handed the guest a NUL character where the JVM unwinds.
            if i < 0 || i as usize >= n {
                return Err(str_index_oob(vm, format!("Index {i} out of bounds for length {n}")));
            }
            Value::Int(s.chars().nth(i as usize).map(|c| rava_classfile::unmap_surrogate(c as u32) as i32).unwrap_or(0))
        }
        "codePointBefore" => {
            // Java checks `index - 1`, and reports THAT in the message.
            let i = arg1.and_then(Value::as_int).unwrap_or(0) - 1;
            let n = s.chars().count();
            if i < 0 || i as usize >= n {
                return Err(str_index_oob(vm, format!("Index {i} out of bounds for length {n}")));
            }
            Value::Int(s.chars().nth(i as usize).map(|c| rava_classfile::unmap_surrogate(c as u32) as i32).unwrap_or(0))
        }
        "codePointCount" => {
            let n = s.chars().count() as i32;
            let b = arg1.and_then(Value::as_int).unwrap_or(0);
            let e = args.get(2).and_then(Value::as_int).unwrap_or(n);
            if b < 0 || e > n || b > e {
                return Err(str_index_oob(vm, format!("Range [{b}, {e}) out of bounds for length {n}")));
            }
            // rava indexes by SCALAR, so a char index is already a code-point index.
            Value::Int(e - b)
        }
        // trim() strips every char <= U+0020; strip() strips Character.isWhitespace.
        // They are NOT the same set, and `str::trim` (Unicode White_Space) is
        // neither: it ate the no-break space trim/strip both keep and kept the
        // U+001C..U+001F separators both remove.
        "trim" => Value::Str(s.trim_matches(|c| c <= '\u{20}').into()),
        "strip" => Value::Str(s.trim_matches(|c: char| java_is_whitespace(c)).into()),
        "stripLeading" => Value::Str(s.trim_start_matches(|c: char| java_is_whitespace(c)).into()),
        "stripTrailing" => Value::Str(s.trim_end_matches(|c: char| java_is_whitespace(c)).into()),
        "toLowerCase" => Value::Str(s.to_lowercase().into()),
        "toUpperCase" => Value::Str(s.to_uppercase().into()),
        "concat" => Value::Str(format!("{s}{}", str_of(vm, arg1).unwrap_or_default()).into()),
        "intern" | "toString" => Value::Str(s.into()),
        "repeat" => {
            let c = arg1.and_then(Value::as_int).unwrap_or(0);
            if c < 0 {
                let ex = vm.make_exception("java/lang/IllegalArgumentException", &format!("count is negative: {c}"));
                return Err(VmError::Throw(ex));
            }
            Value::Str(s.repeat(c as usize).into())
        }
        // `equals` is TYPED: `"1".equals(Integer.valueOf(1))` is false on every JVM. Using
        // `str_of` here compared RENDERINGS, so it answered true — and a String-keyed map
        // then had a second, numeric door into it.
        "equals" => Value::Int(
            (crate::natives::as_string_strict(vm, arg1).as_deref() == Some(s.as_str())) as i32,
        ),
        "equalsIgnoreCase" => {
            // Java folds case per CHAR (upper, then lower-of-upper), so the
            // accented, Greek and Cyrillic pairs match. `eq_ignore_ascii_case`
            // folded only A-Z and answered false for every one of them.
            Value::Int(str_of(vm, arg1).map(|o| {
                o.chars().count() == s.chars().count()
                    && s.chars().zip(o.chars()).all(|(a, b)| java_eq_ignore_case(a, b))
            }).unwrap_or(false) as i32)
        }
        // compareTo/compareToIgnoreCase return the DIFFERENCE at the first
        // unequal char (else the length difference), not a -1/0/1 sign. Guest
        // code reads the magnitude — "A".compareTo("a") is -32, not -1.
        "compareTo" | "compareToIgnoreCase" => {
            let fold = name.len() > "compareTo".len();
            let o = str_of(vm, arg1).unwrap_or_default();
            let mut diff = 0i32;
            for (a, b) in s.chars().zip(o.chars()) {
                let (a, b) = if fold {
                    let (ua, ub) = (java_to_upper(a), java_to_upper(b));
                    if ua == ub { continue; }
                    (java_to_lower(ua), java_to_lower(ub))
                } else {
                    (a, b)
                };
                if a != b {
                    diff = a as i32 - b as i32;
                    break;
                }
            }
            if diff == 0 { diff = s.chars().count() as i32 - o.chars().count() as i32; }
            Value::Int(diff)
        }
        "startsWith" => {
            let prefix = str_of(vm, arg1).unwrap_or_default();
            // startsWith(prefix, toffset): match at a char offset.
            match args.get(2).and_then(Value::as_int) {
                Some(off) => Value::Int(region_matches(&s, off, &prefix, 0, prefix.chars().count() as i32, false) as i32),
                None => Value::Int(s.starts_with(&prefix) as i32),
            }
        }
        "endsWith" => Value::Int(s.ends_with(&str_of(vm, arg1).unwrap_or_default()) as i32),
        "contains" => Value::Int(s.contains(&str_of(vm, arg1).unwrap_or_default()) as i32),
        "regionMatches" => {
            // (Z?)ILjava/lang/String;II — optional leading ignoreCase boolean.
            let ic_form = descriptor.starts_with("(Z");
            let b = ic_form as usize;
            let ignore = ic_form && args.get(1).and_then(Value::as_int).unwrap_or(0) != 0;
            let toff = args.get(1 + b).and_then(Value::as_int).unwrap_or(0);
            let other = str_of(vm, args.get(2 + b)).unwrap_or_default();
            let ooff = args.get(3 + b).and_then(Value::as_int).unwrap_or(0);
            let len = args.get(4 + b).and_then(Value::as_int).unwrap_or(0);
            Value::Int(region_matches_ci(&s, toff, &other, ooff, len, ignore) as i32)
        }
        "indexOf" => Value::Int(index_of(&s, arg1, args.get(2), vm, false)),
        "lastIndexOf" => Value::Int(index_of(&s, arg1, args.get(2), vm, true)),
        "substring" | "subSequence" => {
            let chars: Vec<char> = s.chars().collect();
            let n = chars.len() as i32;
            let b = arg1.and_then(Value::as_int).unwrap_or(0);
            let e = args.get(2).and_then(Value::as_int).unwrap_or(n);
            // `checkBoundsBeginEnd`. Clamping instead returned a WRONG SUBSTRING
            // for every out-of-range call — substring(-1) handed back the whole
            // string and substring(3,2) an empty one, both where the JVM throws.
            if b < 0 || b > e || e > n {
                return Err(str_index_oob(vm, format!("Range [{b}, {e}) out of bounds for length {n}")));
            }
            Value::Str(chars[b as usize..e as usize].iter().collect::<String>().into())
        }
        "replace" => {
            // (CC) uses char codes; (CharSequence,CharSequence) uses strings.
            let (from, to) = if descriptor.starts_with("(CC)") {
                let f = arg1.and_then(Value::as_int).and_then(|c| char::from_u32(c as u32)).unwrap_or('\0');
                let t = args.get(2).and_then(Value::as_int).and_then(|c| char::from_u32(c as u32)).unwrap_or('\0');
                (f.to_string(), t.to_string())
            } else {
                (str_of(vm, arg1).unwrap_or_default(), str_of(vm, args.get(2)).unwrap_or_default())
            };
            Value::Str(s.replace(&from, &to).into())
        }
        "matches" => {
            let pat = str_of(vm, arg1).unwrap_or_default();
            let m = cached_regex(&format!("^(?:{pat})$")).map(|re| re.is_match(&s)).unwrap_or(s == pat);
            Value::Int(m as i32)
        }
        "replaceAll" | "replaceFirst" => {
            let pat = str_of(vm, arg1).unwrap_or_default();
            let rep = str_of(vm, args.get(2)).unwrap_or_default();
            let out = match cached_regex(&pat) {
                Some(re) => {
                    // A `$N` in the replacement that names no group is an
                    // IndexOutOfBoundsException in `Matcher.group(int)`; the regex
                    // crate substitutes the empty string instead, so the wrong text
                    // reached the guest silently. Only raised when the pattern
                    // actually matches — Java expands the replacement per match, so
                    // an unmatched pattern never reaches `group()` either.
                    if re.is_match(&s) {
                        if let Some((class, msg)) = replacement_error(&rep, re.captures_len() - 1) {
                            return Err(VmError::Throw(vm.make_exception(class, &msg)));
                        }
                    }
                    let rep = java_replacement(&rep);
                    if name == "replaceAll" {
                        re.replace_all(&s, rep.as_str()).into_owned()
                    } else {
                        re.replace(&s, rep.as_str()).into_owned()
                    }
                }
                // No regex ⇒ the pattern is treated literally, and so is the
                // replacement (Java's escapes have no meaning without a Matcher).
                None => s.replacen(&pat, &rep, if name == "replaceFirst" { 1 } else { usize::MAX }),
            };
            Value::Str(out.into())
        }
        "split" => {
            let pat = str_of(vm, arg1).unwrap_or_default();
            let limit = args.get(2).and_then(Value::as_int).unwrap_or(0);
            let parts: Vec<Value> = split_regex(&s, &pat, limit)
                .into_iter()
                .map(|p| Value::Str(p.into()))
                .collect();
            let h = vm.heap.alloc_values("Ljava/lang/String;", parts);
            Value::Array(h)
        }
        // chars()/codePoints() are the SAME sequence here: rava indexes strings by
        // scalar, so there are no surrogate halves to expose. On a supplementary
        // char the JVM's chars() yields the two UTF-16 units and this yields one —
        // the same representation gap `length()` has, not a separate defect.
        "chars" | "codePoints" => {
            let cps: Vec<Value> = s.chars().map(|c| Value::Int(c as i32)).collect();
            let arr = vm.heap.alloc_values("int", cps);
            let h = vm.heap.alloc("java/util/stream/IntStream");
            vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
            Value::Ref(h)
        }
        "lines" => {
            // Splits on \n, \r\n and \r; a terminator at the very end does NOT
            // yield a trailing empty line.
            let mut out: Vec<Value> = Vec::new();
            let mut cur = String::new();
            let mut it = s.chars().peekable();
            while let Some(c) = it.next() {
                match c {
                    '\r' => {
                        if it.peek() == Some(&'\n') { it.next(); }
                        out.push(Value::Str(std::mem::take(&mut cur).into()));
                    }
                    '\n' => out.push(Value::Str(std::mem::take(&mut cur).into())),
                    _ => cur.push(c),
                }
            }
            if !cur.is_empty() { out.push(Value::Str(cur.into())); }
            // `natives::make_stream` is the byte-array one; this is the value one.
            crate::stream::of_values(vm, out)
        }
        // `"fmt".formatted(args)` is exactly `String.format(this, args)`.
        "formatted" => {
            let raw = varargs(vm, arg1);
            let mut fargs = Vec::with_capacity(raw.len());
            for v in &raw { fargs.push(stringify(vm, v)); }
            match format_java(&s, &fargs) {
                Ok(t) => Value::Str(t.into()),
                Err((class, msg)) => return Err(VmError::Throw(vm.make_exception(class, &msg))),
            }
        }
        "indent" => {
            // Java: shift each line by n (n<0 removes up to |n| leading spaces),
            // normalize the terminators, and ALWAYS end with a newline.
            let n = arg1.and_then(Value::as_int).unwrap_or(0);
            let mut out = String::new();
            for line in s.split_inclusive(['\n']).map(|l| l.trim_end_matches(['\n', '\r'])) {
                if n >= 0 {
                    out.push_str(&" ".repeat(n as usize));
                    out.push_str(line);
                } else {
                    let drop = line.chars().take_while(|c| java_is_whitespace(*c)).count().min((-n) as usize);
                    out.push_str(&line.chars().skip(drop).collect::<String>());
                }
                out.push('\n');
            }
            Value::Str(out.into())
        }
        "getBytes" => {
            let h = vm.heap.alloc_bytes(s.as_bytes());
            Value::Array(h)
        }
        "toCharArray" => {
            let chars: Vec<Value> = s
                .chars()
                .map(|c| Value::Int(rava_classfile::unmap_surrogate(c as u32) as i32))
                .collect();
            Value::Array(vm.heap.alloc_values("char", chars))
        }
        _ => return Ok(None),
    };
    Ok(Some(ret))
}

/// Translate a `Matcher.appendReplacement` replacement string into the regex
/// crate's expansion syntax. The two agree on `$n` and `${name}` and on nothing
/// else, so passing Java's spelling through unchanged mis-rendered every escape:
///   * Java's `\` escapes the next char (`\$` is a literal `$`, `\\` a literal
///     backslash); the regex crate has no escape and printed the backslash.
///   * A bare `$` the regex crate cannot resolve is literal in Java only when
///     escaped, so an unescaped one is emitted as the crate's `$$`.
///   * `$1x` is group 1 then "x" in Java, but names the group `1x` in the crate,
///     so a digit run is re-spelled in braces.
fn java_replacement(rep: &str) -> String {
    let mut out = String::with_capacity(rep.len());
    let mut it = rep.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some('$') => out.push_str("$$"),
                Some(n) => out.push(n),
                None => {}
            },
            '$' if it.peek() == Some(&'{') => out.push('$'), // ${name}: same spelling
            '$' => {
                let mut digits = String::new();
                while let Some(d) = it.peek() {
                    if d.is_ascii_digit() { digits.push(*d); it.next(); } else { break }
                }
                if digits.is_empty() { out.push_str("$$") } else { out.push_str(&format!("${{{digits}}}")) }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A `$sb`-string-backed standalone CharBuffer (CharBuffer.allocate/wrap) — the
/// char counterpart to the byte-array typed buffers. Returns None for methods it
/// doesn't own (the caller then tries `typed_buffer_dispatch`). Enough for the
/// `CharStreams.toString` read loop: read(buf) fills `$sb`, append(buf) reads it.
/// A `$sb`-string-backed standalone CharBuffer (CharBuffer.allocate/wrap) — the
/// char counterpart to the byte-array typed buffers. Returns None for methods it
/// doesn't own (the caller then tries `typed_buffer_dispatch`). Enough for the
/// `CharStreams.toString` read loop: read(buf) fills `$sb`, append(buf) reads it.
pub(crate) fn charbuffer_dispatch(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    match name {
        "allocate" => {
            let h = vm.heap.alloc("java/nio/CharBuffer");
            vm.heap.put_field_syn(h, crate::heap::Syn::Sb, Value::Str("".into()));
            Some(Some(Value::Ref(h)))
        }
        "wrap" => {
            let s = match args.first() {
                Some(Value::Str(s)) => s.to_string(),
                Some(Value::Array(a)) => vm.heap.array_values(*a).unwrap_or_default().into_iter()
                    .filter_map(|v| char::from_u32(v.as_int().unwrap_or(0) as u32)).collect(),
                _ => String::new(),
            };
            let h = vm.heap.alloc("java/nio/CharBuffer");
            vm.heap.put_field_syn(h, crate::heap::Syn::Sb, Value::Str(s.into()));
            Some(Some(Value::Ref(h)))
        }
        _ => {
            let h = match args.first() { Some(Value::Ref(h)) => *h, _ => return None };
            // Only handle the $sb-backed kind here; an asCharBuffer view ($buf)
            // falls through to typed_buffer_dispatch. $sb may be a char-array
            // (post-append) or a legacy Str; absence ⇒ not our buffer.
            let sb = match vm.heap.get_field_syn(h, crate::heap::Syn::Sb) {
                Value::Str(s) => s.to_string(),
                Value::Array(a) => vm.heap.chars_array_to_string(a),
                _ => return None,
            };
            match name {
                "toString" | "subSequence" => Some(Some(Value::Str(sb.into()))),
                "length" | "remaining" => Some(Some(Value::Int(sb.chars().count() as i32))),
                "capacity" => Some(Some(Value::Int(sb.chars().count().max(1) as i32))),
                "hasRemaining" => Some(Some(Value::Int((!sb.is_empty()) as i32))),
                "charAt" => {
                    let i = args.get(1).and_then(Value::as_int).unwrap_or(0) as usize;
                    Some(Some(Value::Int(sb.chars().nth(i).map(|c| c as i32).unwrap_or(0))))
                }
                "clear" | "compact" => { vm.heap.put_field_syn(h, crate::heap::Syn::Sb, Value::Str("".into())); Some(Some(Value::Ref(h))) }
                "flip" | "rewind" | "mark" | "reset" | "position" | "limit" => Some(Some(Value::Ref(h))),
                _ => None,
            }
        }
    }
}

/// java.lang.StringBuilder / StringBuffer — a mutable `$sb` string buffer.
/// java.lang.StringBuilder / StringBuffer — a mutable `$sb` string buffer.
/// Returns `Err(VmError::Throw)` for `charAt`'s range check — see `str_dispatch`.
pub(crate) fn sb_dispatch(vm: &mut Vm, name: &str, descriptor: &str, args: &[Value]) -> Result<Option<Option<Value>>, VmError> {
    let recv = match args.first() {
        Some(Value::Ref(h)) => *h,
        _ => return Ok(None),
    };
    Ok(match name {
        "<init>" => {
            // <init>() | <init>(int capacity) | <init>(String/CharSequence)
            let init = match args.get(1) {
                Some(Value::Str(s)) => s.to_string(),
                Some(Value::Ref(_)) => sb_arg(vm, descriptor, args.get(1)),
                _ => String::new(),
            };
            vm.heap.sb_set(recv, &init);
            Some(None)
        }
        "append" => {
            // Stringify by the declared parameter type. The Object/CharSequence
            // overload must call the argument's real toString() — peeking at a
            // few known fields (the old sb_arg) returned "null" for any other
            // heap object (e.g. a ResourceLocation), which is why
            // append(identifier) produced "null".
            let t = descriptor.as_bytes().get(1).copied();
            // 3-arg overloads carry index bounds: append(char[] s, int off, int len)
            // slices off..off+len; append(CharSequence s, int start, int end) slices
            // start..end. gson's JsonReader.nextQuotedValue relies on the char[]
            // form to stitch a string across a buffer refill — ignoring the bounds
            // appended the whole 1024-char buffer and corrupted the token.
            let bound_a = args.get(2).and_then(Value::as_int).map(|v| v.max(0) as usize);
            let bound_b = args.get(3).and_then(Value::as_int).map(|v| v.max(0) as usize);
            let piece = match (t, args.get(1)) {
                (Some(b'Z'), Some(v)) => if v.as_int().unwrap_or(0) != 0 { "true".to_string() } else { "false".to_string() },
                (Some(b'C'), Some(v)) => char::from_u32(v.as_int().unwrap_or(0) as u32).map(String::from).unwrap_or_default(),
                (Some(b'['), Some(Value::Array(h))) => match (bound_a, bound_b) {
                    // append(char[], offset, len) — slice the RAW array, do not box all of it.
                    // See `chars_slice_to_string`: this used to build a boxed `Vec<Value>` of the
                    // whole buffer (8192 entries for a 780-byte file) and slice afterwards.
                    (Some(off), Some(len)) => vm.heap.chars_slice_to_string(*h, off, len),
                    _ => vm.heap.chars_array_to_string(*h),
                },
                (_, Some(v)) => {
                    let s = vm.value_to_string(v).unwrap_or_else(|_| v.display());
                    match (bound_a, bound_b) {
                        // append(CharSequence, start, end) — end is exclusive index
                        (Some(start), Some(end)) => s.chars().skip(start).take(end.saturating_sub(start)).collect(),
                        _ => s,
                    }
                }
                (_, None) => String::new(),
            };
            vm.heap.sb_append(recv, &piece); // amortized O(1) in-place append
            Some(Some(Value::Ref(recv))) // chainable
        }
        "appendCodePoint" => {
            let cp = args.get(1).and_then(Value::as_int).unwrap_or(0) as u32;
            if let Some(c) = char::from_u32(cp) {
                let mut buf = [0u8; 4];
                vm.heap.sb_append(recv, c.encode_utf8(&mut buf));
            }
            Some(Some(Value::Ref(recv)))
        }
        "insert" => {
            // insert(int offset, X) — stringify X by the 2nd param type, splice at
            // the char offset (snakeyaml's scanner inserts into its buffer).
            let chars: Vec<char> = vm.heap.sb_read(recv).chars().collect();
            // `checkOffset(offset, count)`: 0 <= offset <= length, else
            // StringIndexOutOfBoundsException — in the RANGE wording, which is what
            // `AbstractStringBuilder.insert` produces. Clamping instead appended at
            // the end, so a bad offset silently wrote to the wrong place.
            let raw_off = args.get(1).and_then(Value::as_int).unwrap_or(0);
            if raw_off < 0 || raw_off as usize > chars.len() {
                let n = chars.len();
                return Err(str_index_oob(vm, format!("Range [{raw_off}, {n}) out of bounds for length {n}")));
            }
            let off = raw_off as usize;
            let t = descriptor.as_bytes().get(2).copied();
            let piece = match (t, args.get(2)) {
                (Some(b'Z'), Some(v)) => if v.as_int().unwrap_or(0) != 0 { "true".into() } else { "false".into() },
                (Some(b'C'), Some(v)) => char::from_u32(v.as_int().unwrap_or(0) as u32).map(String::from).unwrap_or_default(),
                (_, Some(v)) => vm.value_to_string(v).unwrap_or_else(|_| v.display()),
                (_, None) => String::new(),
            };
            let mut out: String = chars[..off].iter().collect();
            out.push_str(&piece);
            out.extend(&chars[off..]);
            vm.heap.sb_set(recv, &out);
            Some(Some(Value::Ref(recv)))
        }
        "toString" => Some(Some(Value::Str(vm.heap.sb_read(recv).into()))),
        // ⭐**READ THE BUFFER, DO NOT REBUILD IT.** These three went through `sb_read`, which
        // decodes the whole backing array into a fresh `String` — O(n) and a malloc to answer a
        // question about one element or the length. `sb_len` / `sb_char_at` read the array.
        "length" | "capacity" if !sb_fast_off() => Some(Some(Value::Int(vm.heap.sb_len(recv) as i32))),
        "length" | "capacity" => Some(Some(Value::Int(vm.heap.sb_read(recv).chars().count() as i32))),
        "ensureCapacity" | "trimToSize" => Some(None),
        "isEmpty" => Some(Some(Value::Int((vm.heap.sb_len(recv) == 0) as i32))),
        "charAt" => {
            let i = args.get(1).and_then(Value::as_int).unwrap_or(0);
            // Same range check String.charAt has: an out-of-range index must
            // unwind, not hand back a NUL character the guest will then process.
            let c = if i < 0 {
                None
            } else if sb_fast_off() {
                let t = vm.heap.sb_read(recv);
                t.chars().nth(i as usize).map(|c| c as i32)
            } else {
                vm.heap.sb_char_at(recv, i as usize)
            };
            match c {
                Some(c) => Some(Some(Value::Int(c))),
                None => {
                    let n = vm.heap.sb_len(recv);
                    return Err(str_index_oob(vm, format!("Index {i} out of bounds for length {n}")));
                }
            }
        }
        "setLength" => {
            let n = args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let mut s: String = vm.heap.sb_read(recv).chars().take(n).collect();
            while s.chars().count() < n { s.push('\0'); }
            vm.heap.sb_set(recv, &s);
            Some(None)
        }
        "reverse" => {
            let r: String = vm.heap.sb_read(recv).chars().rev().collect();
            vm.heap.sb_set(recv, &r);
            Some(Some(Value::Ref(recv)))
        }
        // deleteCharAt/setCharAt both `checkIndex(index, count)` on the JVM. The old
        // code filtered/mapped on a `usize` index, so an out-of-range (or negative,
        // which wrapped huge) index was a silent no-op returning the buffer intact.
        "deleteCharAt" => {
            let raw = args.get(1).and_then(Value::as_int).unwrap_or(0);
            let cur = vm.heap.sb_read(recv);
            let n = cur.chars().count();
            if raw < 0 || raw as usize >= n {
                return Err(str_index_oob(vm, format!("Index {raw} out of bounds for length {n}")));
            }
            let i = raw as usize;
            let s: String = cur.chars().enumerate().filter(|(j, _)| *j != i).map(|(_, c)| c).collect();
            vm.heap.sb_set(recv, &s);
            Some(Some(Value::Ref(recv)))
        }
        "setCharAt" => {
            let raw = args.get(1).and_then(Value::as_int).unwrap_or(0);
            let cur = vm.heap.sb_read(recv);
            let n = cur.chars().count();
            if raw < 0 || raw as usize >= n {
                return Err(str_index_oob(vm, format!("Index {raw} out of bounds for length {n}")));
            }
            let i = raw as usize;
            let c = args.get(2).and_then(Value::as_int).and_then(|v| char::from_u32(v as u32)).unwrap_or('\0');
            let s: String = cur.chars().enumerate().map(|(j, ch)| if j == i { c } else { ch }).collect();
            vm.heap.sb_set(recv, &s);
            Some(None)
        }
        // checkRangeSIOOBE(start, end, count): 0 <= start <= end <= length, else
        // StringIndexOutOfBoundsException. Was clamped to a shorter substring.
        "substring" | "subSequence" => {
            let chars: Vec<char> = vm.heap.sb_read(recv).chars().collect();
            let n = chars.len();
            let b = args.get(1).and_then(Value::as_int).unwrap_or(0);
            let e = args.get(2).and_then(Value::as_int).unwrap_or(n as i32);
            if b < 0 || e > n as i32 || b > e {
                return Err(str_index_oob(vm, format!("Range [{b}, {e}) out of bounds for length {n}")));
            }
            Some(Some(Value::Str(chars[b as usize..e as usize].iter().collect::<String>().into())))
        }
        // indexOf scans forward, lastIndexOf backward. `lastIndexOf` had NO arm, so
        // it left the native layer as an UnsatisfiedLinkError — a hard failure on a
        // method every text scanner reaches for.
        // indexOf scans forward, lastIndexOf backward. `lastIndexOf` had NO arm, so
        // it left the native layer as an UnsatisfiedLinkError.
        //
        // CHAR SPACE, not byte space. `str::find`/`rfind` answer BYTE offsets and
        // `s.get(from..)` slices by BYTES, while every other index in this file
        // (length, charAt, substring, delete, insert) is a CHAR index — so on any
        // buffer holding a non-ASCII character the two disagreed and the answer was
        // a position the guest could not use. The whole family now works on the
        // decoded chars, which is also what makes the `fromIndex` rules below
        // comparable with the JDK's.
        "indexOf" | "lastIndexOf" => {
            let needle: Vec<char> = sb_arg(vm, "(Ljava/lang/String;", args.get(1)).chars().collect();
            let hay: Vec<char> = vm.heap.sb_read(recv).chars().collect();
            let nlen = needle.len();
            let last = name == "lastIndexOf";
            let from = args.get(2).and_then(Value::as_int);
            let pos = if nlen > hay.len() {
                None
            } else if last {
                // A NEGATIVE fromIndex names no searchable region, so the JDK answers
                // -1 outright rather than clamping to 0 — clamping made an EMPTY
                // needle report a hit at 0. Same rule `text.rs::index_of` states.
                if matches!(from, Some(f) if f < 0) {
                    None
                } else {
                    let max_start = hay.len() - nlen;
                    let limit = from.map(|f| (f as usize).min(max_start)).unwrap_or(max_start);
                    (0..=limit).rev().find(|&i| hay[i..].starts_with(&needle))
                }
            } else {
                let start = from.unwrap_or(0).max(0) as usize;
                (start..=hay.len() - nlen).find(|&i| hay[i..].starts_with(&needle))
            };
            Some(Some(Value::Int(pos.map(|i| i as i32).unwrap_or(-1))))
        }
        // delete(start, end) / replace(start, end, str): `checkRangeSIOOBE` on the
        // start, and `end` CLAMPS to the length (the JDK's own `if (end > count) end
        // = count`) — so `delete(1, 99)` is legal and `delete(2, 1)` is not. Both
        // arms were missing entirely: every call was an UnsatisfiedLinkError.
        "delete" | "replace" => {
            let chars: Vec<char> = vm.heap.sb_read(recv).chars().collect();
            let n = chars.len();
            let b = args.get(1).and_then(Value::as_int).unwrap_or(0);
            let e_raw = args.get(2).and_then(Value::as_int).unwrap_or(n as i32);
            // The JDK CLAMPS `end` to `count` FIRST and only then range-checks
            // (`AbstractStringBuilder.delete`: `if (end > count) end = count;` then
            // `checkRangeSIOOBE(start, end, count)`), so the message reports the
            // CLAMPED end — `delete(2, 99)` on "abc" reads `Range [2, 3)`, never
            // `[2, 99)`.
            let e = (e_raw.max(0) as usize).min(n);
            if b < 0 || b as usize > n || b as usize > e {
                return Err(str_index_oob(vm, format!("Range [{b}, {e}) out of bounds for length {n}")));
            }
            let piece = if name == "replace" {
                // `AbstractStringBuilder.replace` calls `str.length()` before it
                // writes anything, so a null `str` is a NullPointerException — not
                // the four-character text "null" that `sb_arg` renders.
                if matches!(args.get(3), Some(Value::Null) | None) {
                    return Err(VmError::Throw(
                        vm.make_exception("java/lang/NullPointerException", ""),
                    ));
                }
                sb_arg(vm, "(Ljava/lang/String;", args.get(3))
            } else {
                String::new()
            };
            let mut out: String = chars[..b as usize].iter().collect();
            out.push_str(&piece);
            out.extend(&chars[e..]);
            vm.heap.sb_set(recv, &out);
            Some(Some(Value::Ref(recv)))
        }
        _ => None,
    })
}

/// `%.Nf` with Java's rounding, which is HALF_UP over the value's SHORTEST decimal
/// representation — not Rust's `{:.*}`, which rounds the exact binary value half-to-
/// even. The two disagree on the everyday cases: `%.1f` of 0.25 is "0.3" in Java and
/// "0.2" in Rust; 9.95 is "10.0" vs "9.9". Rust's `Display` for f64 emits the same
/// shortest round-trip decimal Java's `Double.toString` does and never uses exponent
/// notation, so rounding that STRING half-up reproduces Java.
/// java.text DecimalFormat / DecimalFormatSymbols / NumberFormat (US/ROOT only,
/// generic JDK behaviour). DecimalFormat stores `$frac`/`$minfrac`/`$group` parsed
/// from its pattern; format(...) renders a number.
pub(crate) fn text_dispatch(vm: &mut Vm, owner: &str, name: &str, desc: &str, args: &[Value]) -> Option<Option<Value>> {
    if owner == "java/text/DecimalFormatSymbols" {
        return match name {
            "getInstance" | "<init>" => {
                if name == "<init>" { return Some(None); }
                Some(Some(Value::Ref(vm.heap.alloc("java/text/DecimalFormatSymbols"))))
            }
            "getDecimalSeparator" => Some(Some(Value::Int('.' as i32))),
            "getGroupingSeparator" => Some(Some(Value::Int(',' as i32))),
            "getMinusSign" => Some(Some(Value::Int('-' as i32))),
            "getZeroDigit" => Some(Some(Value::Int('0' as i32))),
            "getPercent" => Some(Some(Value::Int('%' as i32))),
            "getPerMill" => Some(Some(Value::Int('\u{2030}' as i32))),
            "getInfinity" => Some(Some(Value::Str("\u{221e}".into()))),
            "getNaN" => Some(Some(Value::Str("\u{fffd}".into()))),
            n if n.starts_with("set") => Some(None),
            _ => Some(Some(Value::Null)),
        };
    }
    // DecimalFormat / NumberFormat instance + factory.
    let parse_pattern = |p: &str| -> (usize, usize, bool) {
        let grouping = p.contains(',');
        let frac = p.rsplit_once('.').map(|(_, f)| f.chars().filter(|&c| c == '0' || c == '#').count()).unwrap_or(0);
        let minfrac = p.rsplit_once('.').map(|(_, f)| f.chars().filter(|&c| c == '0').count()).unwrap_or(0);
        (minfrac, frac, grouping)
    };
    match name {
        // Factories return a formatter (default: up to 3 fraction digits, grouped).
        "getInstance" | "getNumberInstance" | "getIntegerInstance" | "getPercentInstance" => {
            let h = vm.heap.alloc("java/text/DecimalFormat");
            let frac = if name == "getIntegerInstance" { 0 } else { 3 };
            vm.heap.put_field(h, "$minfrac", Value::Int(0));
            vm.heap.put_field(h, "$frac", Value::Int(frac));
            vm.heap.put_field(h, "$group", Value::Int(1));
            Some(Some(Value::Ref(h)))
        }
        "<init>" => {
            if let Some(Value::Ref(r)) = args.first() {
                let (minf, maxf, grp) = match args.get(1) {
                    Some(Value::Str(p)) => parse_pattern(p),
                    _ => (0, 3, false),
                };
                vm.heap.put_field(*r, "$minfrac", Value::Int(minf as i32));
                vm.heap.put_field(*r, "$frac", Value::Int(maxf as i32));
                vm.heap.put_field(*r, "$group", Value::Int(grp as i32));
            }
            Some(None)
        }
        "applyPattern" | "applyLocalizedPattern" => {
            if let (Some(Value::Ref(r)), Some(Value::Str(p))) = (args.first(), args.get(1)) {
                let (minf, maxf, grp) = parse_pattern(p);
                vm.heap.put_field(*r, "$minfrac", Value::Int(minf as i32));
                vm.heap.put_field(*r, "$frac", Value::Int(maxf as i32));
                vm.heap.put_field(*r, "$group", Value::Int(grp as i32));
            }
            Some(None)
        }
        "setMaximumFractionDigits" => { if let Some(Value::Ref(r)) = args.first() { vm.heap.put_field(*r, "$frac", args.get(1).cloned().unwrap_or(Value::Int(0))); } Some(None) }
        "setMinimumFractionDigits" => { if let Some(Value::Ref(r)) = args.first() { vm.heap.put_field(*r, "$minfrac", args.get(1).cloned().unwrap_or(Value::Int(0))); } Some(None) }
        "setGroupingUsed" => { if let Some(Value::Ref(r)) = args.first() { vm.heap.put_field(*r, "$group", args.get(1).cloned().unwrap_or(Value::Int(0))); } Some(None) }
        "setRoundingMode" | "setMaximumIntegerDigits" | "setMinimumIntegerDigits"
        | "setParseIntegerOnly" | "setDecimalFormatSymbols" | "setPositivePrefix" | "setNegativePrefix" => Some(None),
        "format" => {
            let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Some(Some(Value::Str("".into()))) };
            let frac = vm.heap.get_field(recv, "$frac").as_int().unwrap_or(3).max(0) as usize;
            let minfrac = vm.heap.get_field(recv, "$minfrac").as_int().unwrap_or(0).max(0) as usize;
            let group = matches!(vm.heap.get_field(recv, "$group"), Value::Int(1));
            let val = args.get(1).map(Value::as_f64).unwrap_or(0.0);
            let _ = desc;
            Some(Some(Value::Str(format_decimal(val, minfrac.min(frac), frac, group).into())))
        }
        "toPattern" => Some(Some(Value::Str("#,##0.###".into()))),
        _ => Some(Some(Value::Null)),
    }
}
