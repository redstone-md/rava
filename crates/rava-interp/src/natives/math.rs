//! Arithmetic families: Math, boxed integers, chars, big numbers and Random.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// `java.math.BigDecimal` / `java.math.BigInteger`, backed by the canonical decimal
/// text in a synthetic `$str` field. Construction stores the text; every accessor
/// (scale/precision/intValue/doubleValue/toString/…) derives from it. Returns
/// `Ok(None)` for an unhandled method so the caller surfaces it (we learn the next
/// one a codec needs, rather than silently returning a wrong value).
pub(crate) fn bignum_dispatch(vm: &mut Vm, owner: &str, name: &str, args: &[Value]) -> Result<Option<Option<Value>>, VmError> {
    // Constructors: store the canonical text. BigDecimal(String|double|float|int|
    // long|BigInteger), BigInteger(String|int|long). gson uses the String form.
    if name == "<init>" {
        if let Some(Value::Ref(recv)) = args.first() {
            let text = bignum_text(vm, args.get(1));
            vm.heap.put_field_syn(*recv, crate::heap::Syn::Str, Value::Str(text.into()));
        }
        return Ok(Some(None)); // <init> is void
    }
    // Static factories: BigDecimal/BigInteger.valueOf(long|double[, scale]).
    if name == "valueOf" {
        let text = match (args.first(), args.get(1)) {
            (Some(Value::Long(u)), Some(Value::Int(sc))) if owner.ends_with("BigDecimal") => scaled_text(*u, *sc),
            _ => bignum_text(vm, args.first()),
        };
        let h = vm.heap.alloc(owner);
        vm.heap.put_field_syn(h, crate::heap::Syn::Str, Value::Str(text.into()));
        return Ok(Some(Some(Value::Ref(h))));
    }
    // Instance methods read the stored text.
    //
    // ⭐**THE TEXT IS ALREADY AN `Arc<String>` ON THE FIELD; COPYING IT WAS THE COST.**
    // `get_field_syn` hands back a `Value::Str`, which OWNS the very `Arc<String>` the object
    // holds — and this read then did `.to_string()`, a malloc and a copy of the number's digits,
    // on EVERY instance call. gson parses every JSON number in every datapack file into one of
    // these (`NumberLimits.parseBigDecimal`) and MC's codecs then read `scale`/`intValue`/
    // `doubleValue`/`toString` off it, so this sits inside the datapack reload — 44% of the
    // boot's wall and the phase that runs 5.3x slower than `java -Xint`, with `mi_malloc` at 15%
    // of it. Keeping the `RStr` and borrowing `&str` out of it is one atomic increment instead.
    //
    // Bit-identical: `t` is the same `&str` it always was. Kept unconditional rather than behind
    // the kill switch, because carrying both shapes of the same read is the two-implementations
    // merge hazard, not a redundancy.
    let s: Option<crate::value::RStr> = match args.first() {
        Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Str) {
            Value::Str(s) => Some(s),
            _ => None,
        },
        _ => None,
    };
    let t = s.as_deref().unwrap_or("").trim();
    let asf64 = || t.parse::<f64>().unwrap_or(0.0);
    let asi64 = || t.parse::<i64>().or_else(|_| t.parse::<f64>().map(|f| f as i64)).unwrap_or(0);
    // Same reasoning as `s` above: the operand's text is handed back as the `Arc<String>` the
    // object already holds, so the caller trims a BORROW out of it instead of allocating a copy.
    let other_text = |vm: &mut Vm| -> Option<crate::value::RStr> {
        match args.get(1) {
            Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Str) {
                Value::Str(s) => Some(s),
                _ => None,
            },
            _ => None,
        }
    };
    let r = match name {
        "scale" => Value::Int(decimal_scale(t)),
        "precision" => Value::Int(decimal_precision(t)),
        "signum" => Value::Int(if asf64() > 0.0 { 1 } else if asf64() < 0.0 { -1 } else { 0 }),
        "intValue" | "intValueExact" => Value::Int(asi64() as i32),
        "longValue" | "longValueExact" => Value::Long(asi64()),
        "shortValue" | "shortValueExact" => Value::Int(asi64() as i16 as i32),
        "byteValue" | "byteValueExact" => Value::Int(asi64() as i8 as i32),
        "doubleValue" => Value::Double(asf64()),
        "floatValue" => Value::Float(asf64() as f32),
        // ⭐**THE ANSWER IS THE FIELD.** `t.into()` built a `String` AND an `Arc` to hand back
        // text this object already holds in exactly that form. When the stored text needed no
        // trimming — which is every value `bignum_text` writes, since it writes canonical text —
        // the field's own `RStr` IS the return value, at one atomic instead of two mallocs.
        // Equal lengths after `trim` prove the two are the same bytes.
        // `RAVA_NO_FAST_MISC=1` restores the copy.
        "toString" | "toPlainString" | "toEngineeringString" => match &s {
            Some(r) if fast_misc() && r.len() == t.len() => Value::Str(r.clone()),
            _ => Value::Str(t.into()),
        },
        "hashCode" => Value::Int(t.bytes().fold(0i32, |a, b| a.wrapping_mul(31).wrapping_add(b as i32))),
        "equals" => {
            let o = other_text(vm);
            Value::Int((t == o.as_deref().unwrap_or("").trim()) as i32)
        }
        "compareTo" => {
            let o = other_text(vm);
            let b = o.as_deref().unwrap_or("").trim().parse::<f64>().unwrap_or(0.0);
            let a = asf64();
            Value::Int(if a < b { -1 } else if a > b { 1 } else { 0 })
        }
        // Return a fresh BigDecimal/BigInteger with adjusted text.
        "abs" | "negate" | "stripTrailingZeros" => {
            let nt = match name {
                "abs" => t.trim_start_matches(['+', '-']).to_string(),
                "negate" => match t.strip_prefix('-') { Some(r) => r.to_string(), None => format!("-{t}") },
                _ /* stripTrailingZeros */ => t.to_string(),
            };
            let h = vm.heap.alloc(owner);
            vm.heap.put_field_syn(h, crate::heap::Syn::Str, Value::Str(nt.into()));
            Value::Ref(h)
        }
        "toBigInteger" | "toBigIntegerExact" => {
            let intpart = t.split(['.', 'e', 'E']).next().unwrap_or("0");
            let h = vm.heap.alloc("java/math/BigInteger");
            vm.heap.put_field_syn(h, crate::heap::Syn::Str, Value::Str(intpart.into()));
            Value::Ref(h)
        }
        _ => return Ok(None),
    };
    Ok(Some(Some(r)))
}

/// THE THROWING WRAPPER — the same shape the uuid, base64, regex, bytebuffer and Files
/// dispatchers grew. `nextInt` REJECTS a non-positive bound and an inverted range; this
/// clamped with `bound.max(1)` instead, so `nextInt(0)` and `nextInt(-5)` quietly returned
/// 0 where the JDK raises. A bound computed as zero by a guest bug then looked like a
/// legitimate draw.
/// `java.util.SplittableRandom` — SplitMix64, exactly as the JDK specifies it. Absent
/// entirely, so a guest using it got an unnamed failure. Unlike `java.util.Random` the
/// seed is NOT scrambled at construction; the gamma is the golden constant.
pub(crate) fn splittable_dispatch(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    const GOLDEN: u64 = 0x9E3779B97F4A7C15;
    let recv = match args.first() {
        Some(Value::Ref(h)) => *h,
        _ => return Some(Some(Value::Null)),
    };
    // The seed advances by GAMMA on every draw; `mix64`/`mix32` are the finalisers.
    let mut next_seed = |vm: &mut Vm| -> u64 {
        let cur = vm.heap.get_field(recv, "$sseed").as_long().unwrap_or(0) as u64;
        let s = cur.wrapping_add(GOLDEN);
        vm.heap.put_field(recv, "$sseed", Value::Long(s as i64));
        s
    };
    let mix64 = |mut z: u64| -> u64 {
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    };
    let mix32 = |mut z: u64| -> i32 {
        z = (z ^ (z >> 33)).wrapping_mul(0x62A9D9ED799705F5);
        (((z ^ (z >> 28)).wrapping_mul(0xCB24D0A5C88C35B3)) >> 32) as i32
    };
    match name {
        "<init>" => {
            let seed = args.get(1).and_then(Value::as_long).unwrap_or(0);
            vm.heap.put_field(recv, "$sseed", Value::Long(seed));
            Some(None)
        }
        "nextLong" => {
            let s = next_seed(vm);
            Some(Some(Value::Long(mix64(s) as i64)))
        }
        "nextInt" => {
            let s = next_seed(vm);
            let v = mix32(s);
            match (args.get(1).and_then(Value::as_int), args.get(2).and_then(Value::as_int)) {
                (Some(o), Some(b)) if b > o => Some(Some(Value::Int(o + (v.rem_euclid(b - o))))),
                (Some(b), None) if b > 0 => Some(Some(Value::Int(v.rem_euclid(b)))),
                _ => Some(Some(Value::Int(v))),
            }
        }
        "nextDouble" => {
            let s = next_seed(vm);
            let bits = mix64(s) >> 11;
            Some(Some(Value::Double(bits as f64 * (1.0f64 / (1u64 << 53) as f64))))
        }
        "nextBoolean" => {
            let s = next_seed(vm);
            Some(Some(Value::Int(((mix32(s) < 0) as i32).abs())))
        }
        "split" => {
            let s = next_seed(vm);
            let h = vm.heap.alloc("java/util/SplittableRandom");
            vm.heap.put_field(h, "$sseed", Value::Long(mix64(s) as i64));
            Some(Some(Value::Ref(h)))
        }
        _ => None,
    }
}

pub(crate) fn random_dispatch(
    vm: &mut Vm,
    name: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, crate::vmframe::VmError> {
    if name == "nextInt" {
        let bad = match (args.get(1).and_then(Value::as_int), args.get(2).and_then(Value::as_int)) {
            (Some(origin), Some(bound)) => origin >= bound,
            (Some(bound), None) => bound <= 0,
            _ => false,
        };
        if bad {
            let e = vm.make_exception("java/lang/IllegalArgumentException", "bound must be positive");
            return Err(crate::vmframe::VmError::Throw(e));
        }
    }
    Ok(random_dispatch_inner(vm, name, args))
}

fn random_dispatch_inner(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    const MASK: u64 = (1u64 << 48) - 1;
    const MUL: u64 = 0x5DEECE66D;
    if name == "current" { // static ThreadLocalRandom.current()
        return Some(Some(Value::Ref(vm.heap.alloc("java/util/concurrent/ThreadLocalRandom"))));
    }
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return None };
    fn rd(vm: &Vm, r: u32) -> u64 { vm.heap.get_field(r, "$seed").as_long().unwrap_or(0) as u64 & MASK }
    fn wr(vm: &mut Vm, r: u32, s: u64) { vm.heap.put_field(r, "$seed", Value::Long((s & MASK) as i64)); }
    fn next(vm: &mut Vm, r: u32, bits: u32) -> i32 {
        let s = rd(vm, r).wrapping_mul(MUL).wrapping_add(0xB) & MASK;
        wr(vm, r, s);
        (s >> (48 - bits)) as u32 as i32
    }
    // Seed a fresh Random (no $seed yet) from a process-wide counter.
    if matches!(vm.heap.get_field(recv, "$seed"), Value::Null) {
        static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0x2545F491_4F6CDD1D);
        let s = CTR.fetch_add(0x9E3779B9_7F4A7C15, std::sync::atomic::Ordering::Relaxed);
        wr(vm, recv, (s ^ MUL) & MASK);
    }
    let bounded = |vm: &mut Vm, bound: i32| -> i32 {
        let bound = bound.max(1);
        if bound & (bound - 1) == 0 {
            ((bound as i64).wrapping_mul(next(vm, recv, 31) as i64) >> 31) as i32
        } else {
            loop {
                let bits = next(vm, recv, 31);
                let val = bits % bound;
                if bits - val + (bound - 1) >= 0 { return val; }
            }
        }
    };
    match name {
        "<init>" => {
            if let Some(seed) = args.get(1).and_then(Value::as_long) {
                wr(vm, recv, (seed as u64 ^ MUL) & MASK);
            }
            Some(None)
        }
        "setSeed" => {
            let s = args.get(1).and_then(Value::as_long).unwrap_or(0) as u64;
            wr(vm, recv, (s ^ MUL) & MASK);
            // The JDK's `setSeed` clears the cached second Gaussian; leaving it would
            // hand the next caller a value drawn from the OLD seed.
            vm.heap.put_field(recv, "$haveG", Value::Int(0));
            Some(None)
        }
        "next" => Some(Some(Value::Int(next(vm, recv, args.get(1).and_then(Value::as_int).unwrap_or(32) as u32)))),
        "nextInt" => match (args.get(1).and_then(Value::as_int), args.get(2).and_then(Value::as_int)) {
            (Some(origin), Some(bound)) => { let span = (bound - origin).max(1); Some(Some(Value::Int(origin + bounded(vm, span)))) }
            (Some(bound), None) => Some(Some(Value::Int(bounded(vm, bound)))),
            _ => Some(Some(Value::Int(next(vm, recv, 32)))),
        },
        "nextLong" => {
            let hi = next(vm, recv, 32) as i64;
            let lo = next(vm, recv, 32) as i64;
            Some(Some(Value::Long((hi << 32).wrapping_add(lo))))
        }
        "nextBoolean" => Some(Some(Value::Int((next(vm, recv, 1) & 1 != 0) as i32))),
        // `ints(n)` / `ints(n, lo, hi)` / `doubles(n)` / `longs(n)` — the stream forms,
        // absent entirely. They draw from the SAME generator in the same order, so a
        // guest that mixes `nextInt()` with `ints()` still sees one sequence.
        "ints" | "longs" | "doubles" => {
            let n = args.get(1).and_then(Value::as_long).unwrap_or(0).max(0) as usize;
            let (lo, hi) = (
                args.get(2).and_then(Value::as_int),
                args.get(3).and_then(Value::as_int),
            );
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let v = match name {
                    "longs" => {
                        let hi32 = next(vm, recv, 32) as i64;
                        let lo32 = next(vm, recv, 32) as i64;
                        Value::Long((hi32 << 32).wrapping_add(lo32))
                    }
                    "doubles" => {
                        let h = next(vm, recv, 26) as i64;
                        let l = next(vm, recv, 27) as i64;
                        Value::Double(((h << 27).wrapping_add(l)) as f64 * (1.0f64 / (1u64 << 53) as f64))
                    }
                    _ => match (lo, hi) {
                        (Some(o), Some(b)) => Value::Int(o + bounded(vm, (b - o).max(1))),
                        _ => Value::Int(next(vm, recv, 32)),
                    },
                };
                out.push(v);
            }
            Some(Some(crate::stream::make_stream(vm, out)))
        }
        "nextDouble" => {
            let hi = next(vm, recv, 26) as i64;
            let lo = next(vm, recv, 27) as i64;
            Some(Some(Value::Double(((hi << 27).wrapping_add(lo)) as f64 * (1.0f64 / (1u64 << 53) as f64))))
        }
        "nextFloat" => Some(Some(Value::Float(next(vm, recv, 24) as f32 / (1u32 << 24) as f32))),
        // `nextGaussian` — the JDK's polar method, EXACTLY, because the sequence is
        // specified and worldgen consumes it. This returned a constant 0.0, which is not
        // a Gaussian and not even a draw: it consumed no randomness, so every subsequent
        // value in the stream was shifted too.
        //
        // The algorithm keeps a SPARE: one pass produces two normals, the second is
        // cached on the receiver and returned by the next call. Caching it is not an
        // optimisation — the JDK does it, so omitting it changes the sequence.
        "nextGaussian" => {
            if matches!(vm.heap.get_field(recv, "$haveG"), Value::Int(1)) {
                vm.heap.put_field(recv, "$haveG", Value::Int(0));
                let g = vm.heap.get_field(recv, "$nextG").as_f64();
                return Some(Some(Value::Double(g)));
            }
            let mut draw = |vm: &mut Vm| -> f64 {
                let hi = next(vm, recv, 26) as i64;
                let lo = next(vm, recv, 27) as i64;
                ((hi << 27).wrapping_add(lo)) as f64 * (1.0f64 / (1u64 << 53) as f64)
            };
            let (v1, v2, s2) = loop {
                let v1 = 2.0 * draw(vm) - 1.0;
                let v2 = 2.0 * draw(vm) - 1.0;
                let s2 = v1 * v1 + v2 * v2;
                if s2 < 1.0 && s2 != 0.0 {
                    break (v1, v2, s2);
                }
            };
            let multiplier = (-2.0 * s2.ln() / s2).sqrt();
            vm.heap.put_field(recv, "$nextG", Value::Double(v2 * multiplier));
            vm.heap.put_field(recv, "$haveG", Value::Int(1));
            Some(Some(Value::Double(v1 * multiplier)))
        }
        "nextBytes" => {
            if let Some(Value::Array(a)) = args.get(1) {
                let a = *a;
                let n = vm.heap.array_len(a).unwrap_or(0);
                let mut i = 0;
                while i < n {
                    let mut rnd = next(vm, recv, 32);
                    let mut k = (n - i).min(4);
                    while k > 0 {
                        vm.heap.array_set(a, i, Value::Int(rnd as i8 as i32));
                        rnd >>= 8; i += 1; k -= 1;
                    }
                }
            }
            Some(None)
        }
        "ints" | "longs" | "doubles" => None, // streams — fall through if unused
        _ => None,
    }
}

/// A Java throwable a pure native wants to raise: `(class, message)`. These
/// helpers have no `&mut Vm`, so the caller in the cascade materialises it.
pub(crate) type NativeThrow = (&'static str, String);

/// Integer/Long static formatting, parsing, and bit operations.
pub(crate) fn int_static(is_long: bool, name: &str, args: &[Value]) -> Option<Result<Value, NativeThrow>> {
    let a = args.first();
    let av = a.map(Value::as_i64).unwrap_or(0);
    let bv = args.get(1).map(Value::as_i64).unwrap_or(0);
    let unsigned: u64 = if is_long { av as u64 } else { av as u32 as u64 };
    let unsigned_b: u64 = if is_long { bv as u64 } else { bv as u32 as u64 };
    let v = match name {
        "toHexString" => Value::Str(format!("{unsigned:x}").into()),
        "toBinaryString" => Value::Str(format!("{unsigned:b}").into()),
        "toOctalString" => Value::Str(format!("{unsigned:o}").into()),
        "toString" => {
            let radix = args.get(1).and_then(Value::as_int).unwrap_or(10);
            if args.len() >= 2 && radix != 10 { Value::Str(to_radix(av, radix as u32).into()) }
            else { Value::Str(av.to_string().into()) }
        }
        // The value is REINTERPRETED as unsigned: Integer.toUnsignedString(-1) is
        // "4294967295". This arm used to share the signed `toString` code above and
        // so printed "-1" — and because owner_families runs before owner_table, it
        // shadowed the (correct) unsigned arm in the cascade table.
        "toUnsignedString" => {
            let radix = args.get(1).and_then(Value::as_int).unwrap_or(10);
            if args.len() >= 2 && radix != 10 { Value::Str(to_radix_unsigned(unsigned, radix as u32).into()) }
            else { Value::Str(unsigned.to_string().into()) }
        }
        // Only the (String) and (String, int) overloads: the 4-argument
        // parseInt(CharSequence, begin, end, radix) puts beginIndex where this
        // reads the radix, so it is refused rather than answered wrongly.
        "parseInt" | "parseLong" | "parseUnsignedInt" | "parseUnsignedLong" if args.len() <= 2 => {
            let s = match a { Some(Value::Str(s)) => s.to_string(), _ => return None };
            let radix = args.get(1).and_then(Value::as_int).unwrap_or(10);
            return Some(parse_java_int(&s, radix, is_long, name.starts_with("parseUnsigned")));
        }
        "bitCount" => Value::Int(unsigned.count_ones() as i32),
        "numberOfLeadingZeros" => Value::Int(if is_long { (av as u64).leading_zeros() } else { (av as u32).leading_zeros() } as i32),
        "numberOfTrailingZeros" => Value::Int(if av == 0 { if is_long { 64 } else { 32 } } else if is_long { (av as u64).trailing_zeros() as i32 } else { (av as u32).trailing_zeros() as i32 }),
        "highestOneBit" => { let h = if unsigned == 0 { 0 } else { 1u64 << (63 - unsigned.leading_zeros()) }; if is_long { Value::Long(h as i64) } else { Value::Int(h as i32) } }
        "lowestOneBit" => { let l = unsigned & unsigned.wrapping_neg(); if is_long { Value::Long(l as i64) } else { Value::Int(l as i32) } }
        "compare" => Value::Int(av.cmp(&bv) as i32),
        // Both operands are UNSIGNED. Comparing the sign-extended i64s made
        // Integer.compareUnsigned(-1, 1) answer -1 where the JDK answers 1 —
        // enough to invert an unsigned sort.
        "compareUnsigned" => Value::Int(unsigned.cmp(&unsigned_b) as i32),
        "max" => if is_long { Value::Long(av.max(bv)) } else { Value::Int(av.max(bv) as i32) },
        "min" => if is_long { Value::Long(av.min(bv)) } else { Value::Int(av.min(bv) as i32) },
        "sum" => if is_long { Value::Long(av.wrapping_add(bv)) } else { Value::Int((av.wrapping_add(bv)) as i32) },
        "signum" => Value::Int(av.signum() as i32),
        "hashCode" => Value::Int(if is_long { (av ^ (av >> 32)) as i32 } else { av as i32 }),
        "reverseBytes" => if is_long { Value::Long((av as u64).swap_bytes() as i64) } else { Value::Int((av as u32).swap_bytes() as i32) },
        "reverse" => if is_long { Value::Long((av as u64).reverse_bits() as i64) } else { Value::Int((av as u32).reverse_bits() as i32) },
        "rotateLeft" => if is_long { Value::Long((av as u64).rotate_left(bv as u32) as i64) } else { Value::Int((av as u32).rotate_left(bv as u32) as i32) },
        "rotateRight" => if is_long { Value::Long((av as u64).rotate_right(bv as u32) as i64) } else { Value::Int((av as u32).rotate_right(bv as u32) as i32) },
        // A zero divisor is an ArithmeticException here exactly as it is for idiv;
        // the old `.max(1)` turned it into a silent "divide by one".
        "divideUnsigned" | "remainderUnsigned" => {
            if unsigned_b == 0 {
                return Some(Err(("java/lang/ArithmeticException", "/ by zero".into())));
            }
            let r = if name == "divideUnsigned" { unsigned / unsigned_b } else { unsigned % unsigned_b };
            if is_long { Value::Long(r as i64) } else { Value::Int(r as i32) }
        }
        "hashCode32" => Value::Int(av as i32),
        _ => return None,
    };
    Some(Ok(v))
}

/// `Integer.parseInt` / `Long.parseLong` and their unsigned twins, to the letter.
/// The old body trimmed the text, parsed into an i64 and silently narrowed, so
/// `parseInt(" 1 ")` succeeded where the JDK throws, `parseInt("2147483648")`
/// returned MIN_VALUE instead of throwing, and every malformed string became 0
/// (an unparseable config value read as zero rather than failing loudly).
pub(crate) fn parse_java_int(s: &str, radix: i32, is_long: bool, unsigned: bool) -> Result<Value, NativeThrow> {
    let nfe = |m: String| ("java/lang/NumberFormatException", m);
    if radix < 2 {
        return Err(nfe(format!("radix {radix} less than Character.MIN_RADIX")));
    }
    if radix > 36 {
        return Err(nfe(format!("radix {radix} greater than Character.MAX_RADIX")));
    }
    let bad = || {
        nfe(if radix == 10 { format!("For input string: \"{s}\"") }
            else { format!("For input string: \"{s}\" under radix {radix}") })
    };
    // i128/u128 hold every 64-bit result, so the parse only fails on genuinely
    // malformed text (or absurd length) and the range check below is the one that
    // decides "out of range for this type" — which is a NumberFormatException too.
    if unsigned {
        if s.starts_with('-') {
            return Err(nfe(format!("Illegal leading minus sign on unsigned string {s}.")));
        }
        let n = u128::from_str_radix(s, radix as u32).map_err(|_| bad())?;
        let lim = if is_long { u64::MAX as u128 } else { u32::MAX as u128 };
        if n > lim {
            let ty = if is_long { "long" } else { "int" };
            return Err(nfe(format!("String value {s} exceeds range of unsigned {ty}.")));
        }
        Ok(if is_long { Value::Long(n as u64 as i64) } else { Value::Int(n as u32 as i32) })
    } else {
        let n = i128::from_str_radix(s, radix as u32).map_err(|_| bad())?;
        let (lo, hi) = if is_long { (i64::MIN as i128, i64::MAX as i128) }
                       else { (i32::MIN as i128, i32::MAX as i128) };
        if n < lo || n > hi { return Err(bad()); }
        Ok(if is_long { Value::Long(n as i64) } else { Value::Int(n as i32) })
    }
}

/// `Double.parseDouble` / `Float.parseFloat`. The old bodies were
/// `s.trim().parse().ok().unwrap_or(0.0)`, which silently turned every malformed
/// string into 0.0 — a corrupt config value read as zero rather than failing —
/// and rejected two forms the JDK's grammar accepts: the trailing type suffix
/// ("1.5d") and the hexadecimal significand ("0x1p3").
///
/// The decimal case is handed to Rust at the TARGET width so `Float.parseFloat`
/// rounds once rather than twice. NOTE: the hex path scales a rounded 53-bit
/// significand by a power of two, so a hex literal that lands in the subnormal
/// range can differ from the JDK's exact conversion by an ulp.
pub(crate) fn parse_java_fp(s: &str, is_float: bool) -> Result<Value, NativeThrow> {
    let nfe = |m: String| ("java/lang/NumberFormatException", m);
    let bad = || nfe(format!("For input string: \"{s}\""));
    let t = s.trim_matches(|c: char| c <= ' ');
    if t.is_empty() {
        return Err(nfe("empty String".into()));
    }
    // A single trailing f/F/d/D is part of the literal, not of the number.
    let body = match t.strip_suffix(['f', 'F', 'd', 'D']) {
        Some(rest) if !rest.is_empty() && !rest.ends_with(['x', 'X']) => rest,
        _ => t,
    };
    let (neg, mag) = match body.strip_prefix(['+', '-']) {
        Some(rest) => (body.starts_with('-'), rest),
        None => (false, body),
    };
    // Rust's parser accepts "inf"/"infinity"/"nan" in any case; Java's grammar
    // spells exactly two words, so anything else alphabetic is a format error.
    if mag.starts_with(|c: char| c.is_ascii_alphabetic()) {
        let v = match mag { "NaN" => f64::NAN, "Infinity" => f64::INFINITY, _ => return Err(bad()) };
        let v = if neg { -v } else { v };
        return Ok(if is_float { Value::Float(v as f32) } else { Value::Double(v) });
    }
    if let Some(hex) = mag.strip_prefix("0x").or_else(|| mag.strip_prefix("0X")) {
        let v = parse_hex_significand(hex).ok_or_else(bad)?;
        let v = if neg { -v } else { v };
        return Ok(if is_float { Value::Float(v as f32) } else { Value::Double(v) });
    }
    if is_float {
        body.parse::<f32>().map(Value::Float).map_err(|_| bad())
    } else {
        body.parse::<f64>().map(Value::Double).map_err(|_| bad())
    }
}

/// The part of a hex floating-point literal after "0x": `hexdigits[.hexdigits]`
/// followed by a MANDATORY binary exponent `p[+-]digits` (JLS 3.10.2).
fn parse_hex_significand(hex: &str) -> Option<f64> {
    let (digits, exp) = hex.split_once(['p', 'P'])?;
    let exp: i32 = exp.parse().ok()?;
    let (int_part, frac_part) = digits.split_once('.').unwrap_or((digits, ""));
    if int_part.is_empty() && frac_part.is_empty() { return None; }
    let mut mant: u128 = 0;
    let mut dropped = false;
    for c in int_part.chars().chain(frac_part.chars()) {
        let d = c.to_digit(16)? as u128;
        // Past 124 bits any further digit only matters for the sticky bit.
        if mant >> 124 != 0 { dropped |= d != 0; } else { mant = (mant << 4) | d; }
    }
    let mut e = exp - 4 * frac_part.len() as i32;
    if mant == 0 { return Some(0.0); }
    // Round the significand to 53 bits (nearest, ties to even) before it meets f64.
    let bits = 128 - mant.leading_zeros() as i32;
    if bits > 53 {
        let shift = bits - 53;
        let rem = mant & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        mant >>= shift;
        if rem > half || (rem == half && (dropped || mant & 1 == 1)) { mant += 1; }
        e += shift;
    }
    // Scale in steps so the power of two itself never overflows or underflows.
    let mut v = mant as f64;
    while e > 1023 { v *= 2f64.powi(1023); e -= 1023; }
    while e < -1022 { v *= 2f64.powi(-1022); e += 1022; }
    Some(v * 2f64.powi(e))
}

/// `Integer.decode` / `Long.decode`: an optional sign, then "0x"/"0X"/"#" for
/// hexadecimal, a leading "0" for octal, and decimal otherwise.
pub(crate) fn decode_java_int(s: &str, is_long: bool) -> Result<Value, NativeThrow> {
    let (sign, rest) = match s.strip_prefix(['+', '-']) {
        Some(rest) => (if s.starts_with('-') { "-" } else { "" }, rest),
        None => ("", s),
    };
    let (radix, digits) = if let Some(d) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        (16, d)
    } else if let Some(d) = rest.strip_prefix('#') {
        (16, d)
    } else if rest.len() > 1 && rest.starts_with('0') {
        (8, &rest[1..])
    } else {
        (10, rest)
    };
    parse_java_int(&format!("{sign}{digits}"), radix, is_long, false)
}

/// java.lang.Character static helpers (char arg passed as int).
///
/// ⛔**AND IT SERVES THE TWO INSTANCE METHODS TOO — `toString` AND `hashCode` — WHICH IS WHY IT
/// TAKES A `Vm`.** `owner_families` runs BEFORE `owner_table`, so this function shadows the
/// cascade table's own `Character` arms; with `args.first().and_then(Value::as_int)` reading
/// `None` for the boxed receiver `crate::boxid` now hands it, `Character.valueOf('#').toString()`
/// rendered `char::from_u32(0)` — a NUL — and `hashCode()` answered 0 for every character alike.
/// (`int_static` beside it has the same shadowing property and its comment records the same
/// lesson from `toUnsignedString`; it is safe here only because `Short`/`Byte` are in no owner
/// family and never reach it.)
///
/// `int_of_or0` is `as_int` for every primitive input — which is the whole population these
/// static signatures admit — and differs only for the box.
pub(crate) fn char_dispatch(vm: &Vm, name: &str, args: &[Value]) -> Option<Value> {
    let ci = crate::natives::int_of_or0(vm, args.first());
    // Surrogate / code-point math operates on the raw int — surrogate code
    // units (0xD800..0xDFFF) aren't valid Rust chars, so handle them before the
    // char::from_u32 conversion below.
    let ci2 = crate::natives::int_of_or0(vm, args.get(1));
    match name {
        "isHighSurrogate" => return Some(Value::Int((0xD800..=0xDBFF).contains(&ci) as i32)),
        "isLowSurrogate" => return Some(Value::Int((0xDC00..=0xDFFF).contains(&ci) as i32)),
        "isSurrogate" => return Some(Value::Int((0xD800..=0xDFFF).contains(&ci) as i32)),
        "isSurrogatePair" => return Some(Value::Int(((0xD800..=0xDBFF).contains(&ci) && (0xDC00..=0xDFFF).contains(&ci2)) as i32)),
        "toCodePoint" => return Some(Value::Int(0x10000 + ((ci - 0xD800) << 10) + (ci2 - 0xDC00))),
        "highSurrogate" => return Some(Value::Int(0xD800 + ((ci - 0x10000) >> 10))),
        "lowSurrogate" => return Some(Value::Int(0xDC00 + ((ci - 0x10000) & 0x3FF))),
        "isBmpCodePoint" => return Some(Value::Int((ci <= 0xFFFF) as i32)),
        "isSupplementaryCodePoint" => return Some(Value::Int((0x10000..=0x10FFFF).contains(&ci) as i32)),
        "isValidCodePoint" => return Some(Value::Int((0..=0x10FFFF).contains(&ci) as i32)),
        _ => {}
    }
    let c = char::from_u32(ci as u32)?;
    let v = match name {
        // `Character.isWhitespace` is NOT Rust's `char::is_whitespace`: Java counts
        // the information separators U+001C..U+001F and EXCLUDES the non-breaking
        // spaces U+00A0/U+2007/U+202F. `java_is_whitespace` already encodes that for
        // `strip`/`isBlank`/`split("\\s")`; the Character native was the one caller
        // still asking Unicode, so a tokenizer written on it split differently from
        // one written on `String.strip`. `isSpaceChar` is a DIFFERENT predicate (the
        // Zs/Zl/Zp categories), which is why U+00A0 is a space char and not
        // whitespace — the two must not share an arm.
        "isWhitespace" => Value::Int(crate::natives::java_is_whitespace(c) as i32),
        "isSpaceChar" => Value::Int(matches!(c, ' ' | '\u{a0}' | '\u{1680}'
            | '\u{2000}'..='\u{200a}' | '\u{2007}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
            | '\u{2028}' | '\u{2029}') as i32),
        "isDigit" => Value::Int(c.is_ascii_digit() as i32),
        "isLetter" => Value::Int(c.is_alphabetic() as i32),
        "isLetterOrDigit" => Value::Int(c.is_alphanumeric() as i32),
        "isAlphabetic" => Value::Int(c.is_alphabetic() as i32),
        "isUpperCase" => Value::Int(c.is_uppercase() as i32),
        "isLowerCase" => Value::Int(c.is_lowercase() as i32),
        "toLowerCase" => Value::Int(c.to_ascii_lowercase() as i32),
        "toUpperCase" => Value::Int(c.to_ascii_uppercase() as i32),
        "isJavaIdentifierStart" => Value::Int((c.is_alphabetic() || c == '_' || c == '$') as i32),
        "isJavaIdentifierPart" => Value::Int((c.is_alphanumeric() || c == '_' || c == '$') as i32),
        "digit" => {
            let radix = args.get(1).and_then(Value::as_int).unwrap_or(10) as u32;
            Value::Int(c.to_digit(radix).map(|d| d as i32).unwrap_or(-1))
        }
        "getNumericValue" => Value::Int(c.to_digit(10).map(|d| d as i32).unwrap_or(-1)),
        "forDigit" => {
            let radix = args.get(1).and_then(Value::as_int).unwrap_or(10) as u32;
            Value::Int(std::char::from_digit(ci as u32, radix).map(|d| d as i32).unwrap_or(0))
        }
        "toString" => Value::Str(c.to_string().into()),
        "hashCode" => Value::Int(ci),
        "charCount" => Value::Int(if ci > 0xFFFF { 2 } else { 1 }),
        "isISOControl" => Value::Int(c.is_control() as i32),
        _ => return None,
    };
    Some(v)
}

/// descriptor); transcendental ops return double.
pub(crate) fn math_dispatch(name: &str, desc: &str, args: &[Value]) -> Option<Result<Value, NativeThrow>> {
    let rt = desc.bytes().last().unwrap_or(b'D');
    // Every Math parameter is a primitive, so the descriptor's parameter letters
    // are one byte each. The RETURN type is not enough to type the operands:
    // `Math.floorMod(long, int)` returns an int, and `Math.ulp(float)` takes a
    // float while `Math.getExponent` returns one in every overload.
    //
    // ⭐**AND BECAUSE EACH IS ONE BYTE, THEY SIT AT FIXED OFFSETS — the scan was never needed.**
    // `desc.find(')')` ran a memchr over the descriptor on every `Math` call, to slice a string
    // whose first two bytes are the only part anything reads. `desc[1]`/`desc[2]` answer the same
    // question, with `)` (a zero- or one-parameter descriptor) standing in for "absent" exactly
    // as the old empty/short slice did. `RAVA_NO_FAST_MISC=1` restores the scan.
    let (p0, p1) = if fast_misc() {
        let b = desc.as_bytes();
        let p0 = match b.get(1).copied() {
            Some(c) if c != b')' => c,
            _ => b'D',
        };
        let p1 = match b.get(2).copied() {
            Some(c) if c != b')' => c,
            _ => p0,
        };
        (p0, p1)
    } else {
        let params = &desc[1..desc.find(')').unwrap_or(1)];
        let p0 = params.as_bytes().first().copied().unwrap_or(b'D');
        let p1 = params.as_bytes().get(1).copied().unwrap_or(p0);
        (p0, p1)
    };
    let typed = |x: f64| -> Value {
        match rt {
            b'I' => Value::Int(x as i32),
            b'J' => Value::Long(x as i64),
            b'F' => Value::Float(x as f32),
            _ => Value::Double(x),
        }
    };
    let a = args.first();
    let b = args.get(1);
    let af = a.map(Value::as_f64).unwrap_or(0.0);
    let bf = b.map(Value::as_f64).unwrap_or(0.0);
    let ai = a.map(Value::as_i64).unwrap_or(0);
    let bi = b.map(Value::as_i64).unwrap_or(0);
    let v = match name {
        // Integer max/min/abs must NOT roundtrip through typed()'s f64 (a long
        // above 2^53 loses low bits) — return the exact integer Value directly.
        "max" if rt == b'J' => Value::Long(ai.max(bi)),
        "min" if rt == b'J' => Value::Long(ai.min(bi)),
        "abs" if rt == b'J' => Value::Long(ai.wrapping_abs()),
        "max" if rt == b'I' => Value::Int((ai as i32).max(bi as i32)),
        "min" if rt == b'I' => Value::Int((ai as i32).min(bi as i32)),
        "abs" if rt == b'I' => Value::Int((ai as i32).wrapping_abs()),
        "max" => typed(java_fmax(af, bf)),
        "min" => typed(java_fmin(af, bf)),
        "abs" => typed(af.abs()),
        "floor" => Value::Double(af.floor()),
        "ceil" => Value::Double(af.ceil()),
        "rint" => Value::Double(af.round_ties_even()),
        // Java rounds HALF-UP (toward +inf): round(-2.5) = -2, where Rust's
        // round() is half-away-from-zero (-3). floor + (frac >= 0.5) matches the
        // JDK exactly, including round(0.49999999999999994) == 0; the saturating
        // `as` cast supplies the JDK's NaN→0 and ±inf clamp.
        "round" => {
            if rt == b'J' {
                let f = af.floor();
                Value::Long((f + ((af - f) >= 0.5) as i64 as f64) as i64)
            } else {
                let x = af as f32;
                let f = x.floor();
                Value::Int((f + ((x - f) >= 0.5) as i32 as f32) as i32)
            }
        }
        "sqrt" => Value::Double(af.sqrt()),
        "cbrt" => Value::Double(af.cbrt()),
        // Math.pow is NOT IEEE-754 pow. The JDK returns NaN when the exponent is
        // NaN, and when |base| == 1 with an infinite exponent; IEEE 754 (which is
        // what Rust's powf implements) answers 1.0 to both. The zero-exponent rule
        // still wins over the NaN one — pow(NaN, 0.0) is 1.0 — which powf already
        // gives, so only these two cases are overridden.
        "pow" => Value::Double(
            if bf.is_nan() || (af.abs() == 1.0 && bf.is_infinite()) { f64::NAN } else { af.powf(bf) },
        ),
        "exp" => Value::Double(af.exp()),
        "log" => Value::Double(af.ln()),
        "log10" => Value::Double(af.log10()),
        "sin" => Value::Double(af.sin()),
        "cos" => Value::Double(af.cos()),
        "tan" => Value::Double(af.tan()),
        "asin" => Value::Double(af.asin()),
        "acos" => Value::Double(af.acos()),
        "atan" => Value::Double(af.atan()),
        "atan2" => Value::Double(af.atan2(bf)),
        "hypot" => Value::Double(af.hypot(bf)),
        // Math.signum returns the ARGUMENT for +/-0.0 (and for NaN); Rust's
        // f64::signum answers +/-1.0 there, so signum(-0.0) came out -1.0 and
        // signum(0.0f) came out 1.0.
        "signum" => {
            let sign = |x: f64| if x == 0.0 || x.is_nan() { x } else { x.signum() };
            if rt == b'F' { Value::Float(sign(af as f32 as f64) as f32) } else { Value::Double(sign(af)) }
        }
        "copySign" => if rt == b'F' { Value::Float((af as f32).copysign(bf as f32)) } else { Value::Double(af.copysign(bf)) },
        // nextUp/nextDown/ulp have float overloads: stepping in DOUBLE space and
        // narrowing afterwards gave nextUp(1.0f) == 1.0 (the double successor is
        // not representable as a float), so the step must happen at the argument's
        // own width.
        "nextUp" => if p0 == b'F' { Value::Float((af as f32).next_up()) } else { Value::Double(af.next_up()) },
        "nextDown" => if p0 == b'F' { Value::Float((af as f32).next_down()) } else { Value::Double(af.next_down()) },
        "nextAfter" => {
            if af.is_nan() || bf.is_nan() {
                if p0 == b'F' { Value::Float(f32::NAN) } else { Value::Double(f64::NAN) }
            } else if p0 == b'F' {
                let x = af as f32;
                Value::Float(if bf > af { x.next_up() } else if bf < af { x.next_down() } else { bf as f32 })
            } else {
                Value::Double(if bf > af { af.next_up() } else if bf < af { af.next_down() } else { bf })
            }
        }
        // Math.ulp is defined at the edges too: NaN -> NaN, infinity -> infinity,
        // and at MAX_VALUE the forward difference overflows to infinity, so it
        // falls back to the backward one (the two spacings are equal there).
        "ulp" => {
            let ulp64 = |x: f64| -> f64 {
                let x = x.abs();
                if x.is_nan() || x.is_infinite() { return x; }
                let fwd = x.next_up() - x;
                if fwd.is_infinite() { x - x.next_down() } else { fwd }
            };
            if p0 == b'F' {
                let x = (af as f32).abs();
                Value::Float(if x.is_nan() || x.is_infinite() { x } else {
                    let fwd = x.next_up() - x;
                    if fwd.is_infinite() { x - x.next_down() } else { fwd }
                })
            } else { Value::Double(ulp64(af)) }
        }
        "scalb" => if rt == b'F' { Value::Float((af as f32) * 2f32.powi(bi as i32)) } else { Value::Double(af * 2f64.powi(bi as i32)) },
        // The IEEE remainder rounds the quotient to the NEAREST EVEN integer.
        // Rust's round() is half-away-from-zero, so IEEEremainder(0.5, 1.0) came
        // out -0.5 where the JDK gives 0.5, and an infinite divisor produced NaN
        // instead of leaving the dividend alone. NOTE: this is still the naive
        // formula — for |x/y| beyond 2^53 the quotient itself is inexact and the
        // result can differ from fdlibm's exact reduction. Fixing that needs a
        // multi-step reduction, not a rounding-mode change.
        "IEEEremainder" => Value::Double(
            if af.is_nan() || bf.is_nan() || af.is_infinite() || bf == 0.0 { f64::NAN }
            else if bf.is_infinite() { af }
            else { af - (af / bf).round_ties_even() * bf },
        ),
        "expm1" => Value::Double(af.exp_m1()),
        "log1p" => Value::Double(af.ln_1p()),
        "sinh" => Value::Double(af.sinh()),
        "cosh" => Value::Double(af.cosh()),
        "tanh" => Value::Double(af.tanh()),
        "fma" => Value::Double(af.mul_add(bf, args.get(2).map(Value::as_f64).unwrap_or(0.0))),
        // The RAW IEEE-754 unbiased exponent, which is defined for every input:
        // zero and subnormals report MIN_EXPONENT-1, NaN and infinity report
        // MAX_EXPONENT+1. Deriving it from log2() answered 0 for 0.0 and NaN,
        // i32::MAX for infinity, and -1074 for a subnormal.
        "getExponent" => Value::Int(if p0 == b'F' {
            (((af as f32).to_bits() >> 23) & 0xFF) as i32 - 127
        } else {
            ((af.to_bits() >> 52) & 0x7FF) as i32 - 1023
        }),
        // The JDK multiplies by a PRE-ROUNDED constant (DEGREES_TO_RADIANS =
        // 0.017453292519943295), which is what Rust's to_radians does; dividing by
        // 180 first is a different rounding and gets toRadians(181.0) wrong by an
        // ulp. Measured, not assumed — the probe caught it.
        "toRadians" => Value::Double(af.to_radians()),
        "toDegrees" => Value::Double(af.to_degrees()),
        // Integer division helpers: EXACT, in the operand's own width, wrapping
        // like the JVM. The f64 route lost the low bits of any long past 2^53
        // (floorDiv(1234567890123456789L, 7) was off by 16) and saturated
        // floorDiv(MIN_VALUE, -1) to MAX_VALUE where the JVM wraps to MIN_VALUE.
        // A zero divisor is an ArithmeticException, not 0 — same as idiv.
        "floorDiv" | "floorMod" | "ceilDiv" | "ceilMod" => {
            let x = if p0 == b'J' { ai } else { ai as i32 as i64 };
            let y = if p1 == b'J' { bi } else { bi as i32 as i64 };
            if y == 0 {
                return Some(Err(("java/lang/ArithmeticException", "/ by zero".into())));
            }
            // wrapping_div/rem so MIN_VALUE / -1 wraps instead of panicking; the
            // sign test below then leaves that case alone, as the JDK does.
            let q0 = x.wrapping_div(y);
            let r0 = x.wrapping_rem(y);
            let exact = q0.wrapping_mul(y) == x;
            let r = match name {
                "floorDiv" => if (x ^ y) < 0 && !exact { q0 - 1 } else { q0 },
                "floorMod" => if (r0 ^ y) < 0 && r0 != 0 { r0 + y } else { r0 },
                "ceilDiv" => if (x ^ y) >= 0 && !exact { q0 + 1 } else { q0 },
                // ceilMod is defined as x - ceilDiv(x, y) * y.
                _ => {
                    let c = if (x ^ y) >= 0 && !exact { q0 + 1 } else { q0 };
                    x.wrapping_sub(c.wrapping_mul(y))
                }
            };
            if rt == b'J' { Value::Long(r) } else { Value::Int(r as i32) }
        }
        // The *Exact family exists precisely to REPORT overflow; the old bodies
        // wrapped silently, and routed through f64 on the way out so
        // addExact(1234567890123456789L, 1) came back with its low bits cleared.
        "addExact" | "subtractExact" | "multiplyExact" | "negateExact"
        | "incrementExact" | "decrementExact" | "absExact" | "toIntExact" => {
            let overflow = || Some(Err((
                "java/lang/ArithmeticException",
                if rt == b'J' { "long overflow" } else { "integer overflow" }.to_string(),
            )));
            if rt == b'J' {
                let r = match name {
                    "addExact" => ai.checked_add(bi),
                    "subtractExact" => ai.checked_sub(bi),
                    "multiplyExact" => ai.checked_mul(if p1 == b'J' { bi } else { bi as i32 as i64 }),
                    "negateExact" => ai.checked_neg(),
                    "incrementExact" => ai.checked_add(1),
                    "decrementExact" => ai.checked_sub(1),
                    _ /* absExact */ => ai.checked_abs(),
                };
                match r { Some(v) => Value::Long(v), None => return overflow() }
            } else {
                // toIntExact narrows a long; the rest are int-by-int.
                let (a, b) = (ai as i32, bi as i32);
                let r = match name {
                    "addExact" => a.checked_add(b),
                    "subtractExact" => a.checked_sub(b),
                    "multiplyExact" => a.checked_mul(b),
                    "negateExact" => a.checked_neg(),
                    "incrementExact" => a.checked_add(1),
                    "decrementExact" => a.checked_sub(1),
                    "absExact" => a.checked_abs(),
                    _ /* toIntExact */ => i32::try_from(ai).ok(),
                };
                match r { Some(v) => Value::Int(v), None => return overflow() }
            }
        }
        // multiplyHigh/multiplyFull: the parts of a 64x64 product the JVM cannot
        // express in one long.
        "multiplyHigh" => Value::Long(((ai as i128 * bi as i128) >> 64) as i64),
        "unsignedMultiplyHigh" => Value::Long(((ai as u64 as u128 * bi as u64 as u128) >> 64) as u64 as i64),
        "multiplyFull" => Value::Long(ai as i32 as i64 * bi as i32 as i64),
        // Integer clamp must stay in integer space for the same reason the *Exact
        // family must. NOTE: the JDK also throws IllegalArgumentException when
        // min > max (or either bound is NaN); that validation is not modelled.
        "clamp" if rt == b'I' || rt == b'J' => {
            let hi = args.get(2).map(Value::as_i64).unwrap_or(0);
            let r = ai.max(bi).min(hi);
            if rt == b'J' { Value::Long(r) } else { Value::Int(r as i32) }
        }
        "clamp" => {
            let hi = args.get(2).map(Value::as_f64).unwrap_or(0.0);
            typed(java_fmin(hi, java_fmax(af, bf)))
        }
        _ => return None,
    };
    Some(Ok(v))
}
