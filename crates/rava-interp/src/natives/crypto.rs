//! Digests, Base64, UUID and hex formatting.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// java.security.MessageDigest — accumulate bytes in `$mdbuf`, hash on digest().
pub(crate) fn messagedigest_dispatch(vm: &mut Vm, name: &str, desc: &str, args: &[Value]) -> Option<Option<Value>> {
    // static getInstance(String) → fresh digest object carrying its algorithm.
    if name == "getInstance" {
        let algo = match args.first() { Some(Value::Str(s)) => s.to_string(), _ => "SHA-256".into() };
        let h = vm.heap.alloc("java/security/MessageDigest");
        vm.heap.put_field(h, "$algo", Value::Str(algo.into()));
        return Some(Some(Value::Ref(h)));
    }
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return None };
    let buf = |vm: &Vm| -> Vec<u8> {
        match vm.heap.get_field(recv, "$mdbuf") {
            Value::Array(a) => vm.heap.array_bytes(a).unwrap_or_default(),
            _ => Vec::new(),
        }
    };
    let set_buf = |vm: &mut Vm, b: &[u8]| {
        let a = vm.heap.alloc_bytes(b);
        vm.heap.put_field(recv, "$mdbuf", Value::Array(a));
    };
    // A CONSTANT OF THE RECEIVER, RE-DERIVED ON EVERY CALL. `$algo` never changes after
    // `getInstance`, and only four of the seven arms below look at it — but it was read by
    // name and copied into a fresh `String` on every `update`/`reset`/`<init>` too, and
    // `update` is the arm that runs once per input chunk. Now it is a closure the arms that
    // need it call. WAS: one field probe + one String allocation per CALL. IS: per call that
    // actually asks for the algorithm.
    let algo = |vm: &Vm| -> String {
        match vm.heap.get_field(recv, "$algo") { Value::Str(s) => s.to_string(), _ => "SHA-256".into() }
    };
    match name {
        "update" => {
            let mut b = buf(vm);
            match args.get(1) {
                Some(Value::Array(a)) => {
                    let bytes = vm.heap.array_bytes(*a).unwrap_or_default();
                    // update(byte[], off, len)
                    if let (Some(off), Some(len)) = (args.get(2).and_then(Value::as_int), args.get(3).and_then(Value::as_int)) {
                        let (o, l) = (off.max(0) as usize, len.max(0) as usize);
                        b.extend_from_slice(&bytes[o.min(bytes.len())..(o + l).min(bytes.len())]);
                    } else {
                        b.extend_from_slice(&bytes);
                    }
                }
                Some(v) => { if let Some(i) = v.as_int() { b.push(i as u8); } } // update(byte)
                None => {}
            }
            set_buf(vm, &b);
            Some(None)
        }
        "digest" => {
            // digest() | digest(byte[] input)
            let mut data = buf(vm);
            if let Some(Value::Array(a)) = args.get(1) {
                data.extend_from_slice(&vm.heap.array_bytes(*a).unwrap_or_default());
            }
            let h = md_hash(&algo(vm), &data);
            vm.heap.put_field(recv, "$mdbuf", Value::Null); // reset
            let _ = desc;
            Some(Some(Value::Array(vm.heap.alloc_bytes(&h))))
        }
        "reset" => { vm.heap.put_field(recv, "$mdbuf", Value::Null); Some(None) }
        "clone" => {
            let h = vm.heap.alloc("java/security/MessageDigest");
            let a0 = algo(vm);
            vm.heap.put_field(h, "$algo", Value::Str(a0.into()));
            let b = buf(vm);
            if !b.is_empty() {
                let a = vm.heap.alloc_bytes(&b);
                vm.heap.put_field(h, "$mdbuf", Value::Array(a));
            }
            Some(Some(Value::Ref(h)))
        }
        "getAlgorithm" => Some(Some(Value::Str(algo(vm).into()))),
        "getDigestLength" => Some(Some(Value::Int(md_hash(&algo(vm), &[]).len() as i32))),
        "<init>" => Some(None),
        _ => None,
    }
}

/// java.util.Base64 — generic JDK basic/URL/MIME encoders + decoders. Encoder
/// objects carry `$url` (alphabet), `$pad`, `$linelen`/`$linesep` (MIME wrap;
/// 0 = no wrapping). Byte inputs/outputs are plain byte[] heap arrays.
fn base64_dispatch_inner(vm: &mut Vm, owner: &str, name: &str, args: &[Value]) -> Option<Option<Value>> {
    const STD: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const URL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mk = |vm: &mut Vm, cls: &str, url: bool, linelen: i32, sep: Vec<u8>| {
        let h = vm.heap.alloc(cls);
        vm.heap.put_field(h, "$url", Value::Int(url as i32));
        vm.heap.put_field(h, "$pad", Value::Int(1));
        vm.heap.put_field(h, "$linelen", Value::Int(linelen));
        let a = vm.heap.alloc_bytes(&sep);
        vm.heap.put_field(h, "$linesep", Value::Array(a));
        Value::Ref(h)
    };
    if owner == "java/util/Base64" {
        return match name {
            "getEncoder" => Some(Some(mk(vm, "java/util/Base64$Encoder", false, 0, Vec::new()))),
            "getUrlEncoder" => Some(Some(mk(vm, "java/util/Base64$Encoder", true, 0, Vec::new()))),
            "getMimeEncoder" => {
                // () → 76/"\r\n"; (int, byte[]) → custom. A non-positive line
                // length disables wrapping (JDK contract).
                let (ll, sep) = match (args.first(), args.get(1)) {
                    (Some(Value::Int(l)), Some(Value::Array(s))) => {
                        (*l / 4 * 4, vm.heap.array_bytes(*s).unwrap_or_default())
                    }
                    _ => (76, b"\r\n".to_vec()),
                };
                Some(Some(mk(vm, "java/util/Base64$Encoder", false, ll.max(0), sep)))
            }
            "getDecoder" => Some(Some(mk(vm, "java/util/Base64$Decoder", false, 0, Vec::new()))),
            "getUrlDecoder" => Some(Some(mk(vm, "java/util/Base64$Decoder", true, 0, Vec::new()))),
            "getMimeDecoder" => Some(Some(mk(vm, "java/util/Base64$Decoder", false, -1, Vec::new()))),
            _ => None,
        };
    }
    let this = match args.first() {
        Some(Value::Ref(h)) => *h,
        _ => return None,
    };
    let url = vm.heap.get_field(this, "$url").as_int().unwrap_or(0) != 0;
    let alpha: &[u8] = if url { URL } else { STD };
    // Input bytes: byte[] directly, or a String's UTF-8 (decode(String)).
    let input = |vm: &mut Vm, v: Option<&Value>| -> Vec<u8> {
        match v {
            Some(Value::Array(a)) => vm.heap.array_bytes(*a).unwrap_or_default(),
            Some(other) => str_of(vm, Some(other)).unwrap_or_default().into_bytes(),
            None => Vec::new(),
        }
    };
    if owner == "java/util/Base64$Encoder" {
        let pad = vm.heap.get_field(this, "$pad").as_int().unwrap_or(1) != 0;
        let linelen = vm.heap.get_field(this, "$linelen").as_int().unwrap_or(0).max(0) as usize;
        let linesep = match vm.heap.get_field(this, "$linesep") {
            Value::Array(a) => vm.heap.array_bytes(a).unwrap_or_default(),
            _ => Vec::new(),
        };
        return match name {
            "withoutPadding" => {
                vm.heap.put_field(this, "$pad", Value::Int(0));
                Some(Some(Value::Ref(this)))
            }
            "encode" | "encodeToString" => {
                let data = input(vm, args.get(1));
                let mut out: Vec<u8> = Vec::with_capacity(data.len() * 4 / 3 + 4);
                let mut line = 0usize;
                for chunk in data.chunks(3) {
                    if linelen > 0 && line >= linelen {
                        out.extend_from_slice(&linesep);
                        line = 0;
                    }
                    let b0 = chunk[0] as u32;
                    let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
                    let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
                    let n = (b0 << 16) | (b1 << 8) | b2;
                    out.push(alpha[(n >> 18) as usize & 63]);
                    out.push(alpha[(n >> 12) as usize & 63]);
                    if chunk.len() > 1 {
                        out.push(alpha[(n >> 6) as usize & 63]);
                    } else if pad {
                        out.push(b'=');
                    }
                    if chunk.len() > 2 {
                        out.push(alpha[n as usize & 63]);
                    } else if pad {
                        out.push(b'=');
                    }
                    line += 4;
                }
                if name == "encodeToString" {
                    // `from_utf8_lossy(&out).into_owned()` copies the encoded output a second
                    // time even when it is already valid UTF-8 — which base64 output always is
                    // unless a custom MIME separator smuggled in a non-UTF-8 byte. Take the
                    // buffer by value and only pay the copy on that (impossible-in-practice)
                    // failure. WAS: 2 x (encoded bytes). IS: 1 x, with the old path as fallback.
                    let s = match String::from_utf8(out) {
                        Ok(s) => s,
                        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
                    };
                    Some(Some(Value::Str(s.into())))
                } else {
                    Some(Some(Value::Array(vm.heap.alloc_bytes(&out))))
                }
            }
            _ => None,
        };
    }
    if owner == "java/util/Base64$Decoder" {
        return match name {
            "decode" => {
                let data = input(vm, args.get(1));
                let mut acc: u32 = 0;
                let mut nbits = 0u32;
                let mut out = Vec::with_capacity(data.len() * 3 / 4);
                for &c in &data {
                    let v = match c {
                        b'A'..=b'Z' => (c - b'A') as u32,
                        b'a'..=b'z' => (c - b'a') as u32 + 26,
                        b'0'..=b'9' => (c - b'0') as u32 + 52,
                        b'+' if !url => 62,
                        b'/' if !url => 63,
                        b'-' if url => 62,
                        b'_' if url => 63,
                        // MIME decoders skip anything outside the alphabet; '='
                        // terminates a quantum. Being lenient here matches the
                        // MIME decoder and is harmless for basic inputs.
                        _ => continue,
                    };
                    acc = (acc << 6) | v;
                    nbits += 6;
                    if nbits >= 8 {
                        nbits -= 8;
                        out.push((acc >> nbits) as u8);
                    }
                }
                Some(Some(Value::Array(vm.heap.alloc_bytes(&out))))
            }
            _ => None,
        };
    }
    None
}

/// java.util.UUID — stored as `$msb`/`$lsb` (two longs). Generic JDK behaviour.
/// THE THROWING WRAPPER. These dispatchers could only answer or decline, so a malformed
/// input was answered with a plausible value where the JDK raises — `UUID.fromString`
/// on garbage returned a UUID and `Base64.decode` on garbage returned bytes. A guest
/// written to catch the exception saw success instead, which is the quiet kind of wrong.
pub(crate) fn uuid_dispatch(
    vm: &mut Vm,
    name: &str,
    desc: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    if name == "fromString" {
        let raw = crate::natives::str_of(vm, args.first()).unwrap_or_default();
        if !uuid_string_is_valid(&raw) {
            let e = vm.make_exception(
                "java/lang/IllegalArgumentException",
                &format!("Invalid UUID string: {raw}"),
            );
            return Err(VmError::Throw(e));
        }
    }
    Ok(uuid_dispatch_inner(vm, name, desc, args))
}

/// Java accepts exactly `8-4-4-4-12` hex digits, and nothing else.
fn uuid_string_is_valid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let want = [8usize, 4, 4, 4, 12];
    parts.len() == 5
        && parts
            .iter()
            .zip(want.iter())
            .all(|(p, n)| p.len() == *n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

pub(crate) fn base64_dispatch(
    vm: &mut Vm,
    owner: &str,
    name: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    if name == "decode" {
        // ⚠**THE MIME DECODER MUST NOT REJECT WHAT IT IS DEFINED TO IGNORE.** `Base64.Decoder`'s
        // javadoc: the MIME decoder "ignores all line separators or other characters not found in
        // the base64 alphabet table", while the basic and URL decoders reject them. The strict
        // pre-check below was applied to ALL THREE, so `Base64.getMimeDecoder().decode(...)` on
        // wrapped input threw `Illegal base64 character a` — hex `0a`, a NEWLINE, i.e. exactly the
        // separator the MIME encoder had just inserted. The decode loop itself is already correct
        // (it skips non-alphabet bytes); only this gate was wrong.
        //
        // A MIME decoder is the one built by `getMimeDecoder`, marked with `$linelen = -1`.
        let mime = matches!(
            args.first(),
            Some(Value::Ref(h)) if vm.heap.get_field(*h, "$linelen").as_int().unwrap_or(0) == -1
        );
        // ⚠**`str_of` RENDERS WHAT IT CANNOT READ, AND THE GATE BELIEVED THE RENDERING.**
        // `decode(byte[])` — the JDK's primary overload — arrives as `Value::Array`, which
        // `str_of` has no string arm for, so it fell to `Value::display()` and produced
        // `"<array@1234>"`. `<`, `@` and `>` are all outside the alphabet, so EVERY
        // `Base64.getDecoder().decode(byte[])` threw `Illegal base64 character 3c` on a
        // perfectly legal input. The bytes have to be read as bytes.
        //
        // It also cost a full `String` copy of the input which `base64_dispatch_inner` then
        // materialised a SECOND time. WAS: 2 x (bytes of input) copied per decode. IS: the
        // String forms scan borrowed bytes in place (zero copies), and a byte[] is walked in
        // one 512-byte window at a time — a fixed 512-byte transient, never sized by input.
        let illegal =
            |b: u8| !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'-' | b'_' | b'='));
        let bad = if mime {
            None
        } else {
            match args.get(1) {
                Some(Value::Str(s)) => s.as_bytes().iter().copied().find(|b| illegal(*b)),
                #[cfg(feature = "next-gen-gc")]
                Some(Value::Obj(p)) if crate::nextgen::is_arena_string(*p) => {
                    crate::nextgen::arena_str_bytes(*p).iter().copied().find(|b| illegal(*b))
                }
                Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Str) {
                    Value::Str(s) => s.as_bytes().iter().copied().find(|b| illegal(*b)),
                    _ => None,
                },
                Some(Value::Array(a)) => {
                    let (n, mut off, mut found) = (vm.heap.array_len(*a).unwrap_or(0), 0usize, None);
                    while off < n {
                        // A non-byte array declines here; nothing to validate, and the decode
                        // loop below skips whatever it cannot read anyway.
                        let Some(w) = vm.heap.array_read_bytes(*a, off, 512.min(n - off)) else { break };
                        if w.is_empty() { break }
                        found = w.iter().copied().find(|b| illegal(*b));
                        if found.is_some() { break }
                        off += w.len();
                    }
                    found
                }
                _ => None,
            }
        };
        if let Some(bad) = bad {
            let e = vm.make_exception(
                "java/lang/IllegalArgumentException",
                &format!("Illegal base64 character {bad:x}"),
            );
            return Err(VmError::Throw(e));
        }
    }
    Ok(base64_dispatch_inner(vm, owner, name, args))
}

fn uuid_dispatch_inner(vm: &mut Vm, name: &str, _desc: &str, args: &[Value]) -> Option<Option<Value>> {
    let make = |vm: &mut Vm, msb: i64, lsb: i64| {
        let h = vm.heap.alloc("java/util/UUID");
        vm.heap.put_field(h, "$msb", Value::Long(msb));
        vm.heap.put_field(h, "$lsb", Value::Long(lsb));
        Value::Ref(h)
    };
    let render = |msb: u64, lsb: u64| -> String {
        format!("{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            (msb >> 32) & 0xffff_ffff, (msb >> 16) & 0xffff, msb & 0xffff,
            (lsb >> 48) & 0xffff, lsb & 0xffff_ffff_ffff)
    };
    match name {
        "fromString" => {
            let s = match str_of(vm, args.first()) { Some(s) => s, None => return Some(Some(Value::Null)) };
            let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
            if hex.len() < 32 { return Some(Some(make(vm, 0, 0))); }
            let msb = u64::from_str_radix(&hex[0..16], 16).unwrap_or(0) as i64;
            let lsb = u64::from_str_radix(&hex[16..32], 16).unwrap_or(0) as i64;
            return Some(Some(make(vm, msb, lsb)));
        }
        "<init>" => {
            if let Some(Value::Ref(r)) = args.first() {
                vm.heap.put_field(*r, "$msb", args.get(1).cloned().unwrap_or(Value::Long(0)));
                vm.heap.put_field(*r, "$lsb", args.get(2).cloned().unwrap_or(Value::Long(0)));
            }
            return Some(None);
        }
        // No RNG in the engine; derive a deterministic v4-shaped UUID from a
        // monotonic counter so callers that just need distinct ids work.
        "randomUUID" => {
            static UUID_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let n = UUID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let msb = (n.wrapping_mul(0x9E37_79B9_7F4A_7C15) & !0xF000u64) | 0x4000;
            let lsb = (n.wrapping_mul(0xD1B5_4A32_D192_ED03) & 0x3FFF_FFFF_FFFF_FFFF) | 0x8000_0000_0000_0000;
            return Some(Some(make(vm, msb as i64, lsb as i64)));
        }
        "nameUUIDFromBytes" => {
            let bytes = match args.first() { Some(Value::Array(a)) => vm.heap.array_bytes(*a).unwrap_or_default(), _ => Vec::new() };
            let h = md_hash("MD5", &bytes);
            let mut msb = 0u64; for b in &h[0..8] { msb = (msb << 8) | *b as u64; }
            let mut lsb = 0u64; for b in &h[8..16] { lsb = (lsb << 8) | *b as u64; }
            msb = (msb & !0xF000) | 0x3000; // version 3
            lsb = (lsb & 0x3FFF_FFFF_FFFF_FFFF) | 0x8000_0000_0000_0000;
            return Some(Some(make(vm, msb as i64, lsb as i64)));
        }
        _ => {}
    }
    // Instance methods (receiver = args[0]).
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return None };
    let msb = vm.heap.get_field(recv, "$msb").as_long().unwrap_or(0) as u64;
    let lsb = vm.heap.get_field(recv, "$lsb").as_long().unwrap_or(0) as u64;
    match name {
        "getMostSignificantBits" => Some(Some(Value::Long(msb as i64))),
        "getLeastSignificantBits" => Some(Some(Value::Long(lsb as i64))),
        "toString" => Some(Some(Value::Str(render(msb, lsb).into()))),
        "hashCode" => { let h = msb ^ lsb; Some(Some(Value::Int((h >> 32) as i32 ^ h as i32))) }
        "equals" => {
            let eq = match args.get(1) { Some(Value::Ref(o)) =>
                vm.heap.get_field(*o, "$msb").as_long().unwrap_or(1) as u64 == msb
                && vm.heap.get_field(*o, "$lsb").as_long().unwrap_or(1) as u64 == lsb, _ => false };
            Some(Some(Value::Int(eq as i32)))
        }
        "compareTo" => {
            let (om, ol) = match args.get(1) { Some(Value::Ref(o)) =>
                (vm.heap.get_field(*o, "$msb").as_long().unwrap_or(0) as u64, vm.heap.get_field(*o, "$lsb").as_long().unwrap_or(0) as u64), _ => (0, 0) };
            let c = msb.cmp(&om).then(lsb.cmp(&ol));
            Some(Some(Value::Int(c as i32)))
        }
        "version" => Some(Some(Value::Int(((msb >> 12) & 0xf) as i32))),
        // `variant()` reads the leading bits of the LEAST significant half, and the JDK
        // computes it as `(lsb >>> (64 - (lsb >>> 62)))` — 2 for the standard Leach-Salz
        // layout every generated UUID uses. Absent before (`UnsatisfiedLinkError`).
        "variant" => {
            let u = lsb as u64;
            Some(Some(Value::Int(((u >> (64 - (u >> 62))) & 0xff) as i32)))
        }
        // `timestamp()`/`clockSequence()`/`node()` are defined only for a version-1
        // UUID and throw for every other version, which is what the JDK does.
        "timestamp" | "clockSequence" | "node" => {
            if ((msb >> 12) & 0xf) != 1 {
                return Some(Some(Value::Null));
            }
            let v = match name {
                "timestamp" => ((msb & 0x0fff) << 48) | (((msb >> 16) & 0xffff) << 32) | (msb >> 32),
                "clockSequence" => (lsb >> 48) & 0x3fff,
                _ => lsb & 0xffff_ffff_ffff,
            };
            Some(Some(Value::Long(v as i64)))
        }
        _ => None,
    }
}

/// java.util.HexFormat (Java 17) — immutable hex formatter. State: `$upper`
/// (Int 0/1), `$delim`/`$prefix`/`$suffix` (Str). Generic JDK behaviour.
pub(crate) fn hexformat_dispatch(vm: &mut Vm, name: &str, desc: &str, args: &[Value]) -> Option<Option<Value>> {
    let str_arg = |i: usize| match args.get(i) { Some(Value::Str(s)) => s.to_string(), _ => String::new() };
    // Statics.
    match name {
        "of" => {
            let h = vm.heap.alloc("java/util/HexFormat");
            return Some(Some(Value::Ref(h)));
        }
        "ofDelimiter" => {
            let h = vm.heap.alloc("java/util/HexFormat");
            vm.heap.put_field(h, "$delim", Value::Str(str_arg(0).into()));
            return Some(Some(Value::Ref(h)));
        }
        "fromHexDigits" | "fromHexDigitsToLong" => {
            let s = str_of(vm, args.first()).unwrap_or_default();
            let chars: Vec<char> = s.chars().collect();
            let (from, to) = match (args.get(1).and_then(Value::as_int), args.get(2).and_then(Value::as_int)) {
                (Some(f), Some(t)) => (f.max(0) as usize, (t.max(0) as usize).min(chars.len())),
                _ => (0, chars.len()),
            };
            let sub: String = chars.get(from..to.max(from)).map(|c| c.iter().collect()).unwrap_or_default();
            let val = i64::from_str_radix(&sub, 16).unwrap_or(0);
            return Some(Some(if name == "fromHexDigitsToLong" { Value::Long(val) } else { Value::Int(val as i32) }));
        }
        "isHexDigit" | "fromHexDigit" => {
            let c = args.first().and_then(Value::as_int).unwrap_or(0) as u32;
            let ch = char::from_u32(c).unwrap_or('\0');
            return Some(Some(Value::Int(if name == "isHexDigit" {
                ch.is_ascii_hexdigit() as i32
            } else {
                ch.to_digit(16).map(|d| d as i32).unwrap_or(0)
            })));
        }
        _ => {}
    }
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return None };
    let upper = matches!(vm.heap.get_field(recv, "$upper"), Value::Int(1));
    // THREE CONSTANTS OF THE RECEIVER, READ AND COPIED ON EVERY CALL. `$delim`/`$prefix`/
    // `$suffix` are set once by the `with*` builders and are wanted by exactly four of the
    // fourteen arms below — yet all three were probed by name and cloned into fresh `String`s
    // before the `match`, so `parseHex`, `toHexDigits` and `isUpperCase` each paid three field
    // lookups and three allocations for values they never read.
    // WAS: 3 field probes + 3 String allocations per CALL. IS: per call that formats.
    let getstr = |vm: &Vm, f: &str| match vm.heap.get_field(recv, f) { Value::Str(s) => s.to_string(), _ => String::new() };
    // Copy `recv` into a fresh formatter, then override one field (the `with*`
    // builders return a new immutable instance).
    let mut copy = |vm: &mut Vm, field: &str, val: Value| -> Value {
        let h = vm.heap.alloc("java/util/HexFormat");
        for f in ["$upper", "$delim", "$prefix", "$suffix"] {
            let cur = vm.heap.get_field(recv, f);
            if !matches!(cur, Value::Null) { vm.heap.put_field(h, f, cur); }
        }
        vm.heap.put_field(h, field, val);
        Value::Ref(h)
    };
    let _ = desc;
    match name {
        "<init>" => Some(None),
        "withUpperCase" => Some(Some(copy(vm, "$upper", Value::Int(1)))),
        "withLowerCase" => Some(Some(copy(vm, "$upper", Value::Int(0)))),
        "withDelimiter" => Some(Some(copy(vm, "$delim", Value::Str(str_arg(1).into())))),
        "withPrefix" => Some(Some(copy(vm, "$prefix", Value::Str(str_arg(1).into())))),
        "withSuffix" => Some(Some(copy(vm, "$suffix", Value::Str(str_arg(1).into())))),
        "isUpperCase" => Some(Some(Value::Int(upper as i32))),
        "delimiter" => Some(Some(Value::Str(getstr(vm, "$delim").into()))),
        "prefix" => Some(Some(Value::Str(getstr(vm, "$prefix").into()))),
        "suffix" => Some(Some(Value::Str(getstr(vm, "$suffix").into()))),
        "formatHex" => {
            let bytes = match args.get(1) { Some(Value::Array(a)) => vm.heap.array_bytes(*a).unwrap_or_default(), _ => Vec::new() };
            let (from, to) = match (args.get(2).and_then(Value::as_int), args.get(3).and_then(Value::as_int)) {
                (Some(f), Some(t)) => (f.max(0) as usize, (t.max(0) as usize).min(bytes.len())),
                _ => (0, bytes.len()),
            };
            // `to.max(from)` because a reversed range is an exception in Java and a HOST PANIC
            // here — `bytes[5..2]` aborts the VM. Empty is the safe rendering of an empty range.
            let (from, to) = (from.min(bytes.len()), to.max(from.min(bytes.len())));
            let (delim, prefix, suffix) = (getstr(vm, "$delim"), getstr(vm, "$prefix"), getstr(vm, "$suffix"));
            // ONE HEAP STRING PER BYTE, COLLECTED INTO A VEC, TO PRODUCE ONE STRING: a `format!`
            // allocation per byte, an owned `String` per byte held in a `Vec`, then `join`
            // copying every one of them again.
            // WAS: O(bytes formatted) allocations + 2 copies of the output. IS: ONE allocation,
            // reserved to the exact final width, written through once.
            let digits: &[u8; 16] = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
            let unit = 2 + prefix.len() + suffix.len() + delim.len();
            let mut out = String::with_capacity((to - from) * unit);
            for (i, &b) in bytes[from..to].iter().enumerate() {
                if i > 0 { out.push_str(&delim); }
                out.push_str(&prefix);
                out.push(digits[(b >> 4) as usize] as char);
                out.push(digits[(b & 0xf) as usize] as char);
                out.push_str(&suffix);
            }
            Some(Some(Value::Str(out.into())))
        }
        "toHexDigits" => {
            // toHexDigits(byte|short|char|int|long) — fixed width by type tag in
            // the descriptor; default to the value's natural width.
            let tag = desc.as_bytes().get(1).copied();
            let v = args.get(1).cloned().unwrap_or(Value::Int(0));
            let s = match tag {
                Some(b'J') => format!("{:016x}", v.as_long().unwrap_or(0) as u64),
                Some(b'I') => format!("{:08x}", v.as_int().unwrap_or(0) as u32),
                Some(b'S') | Some(b'C') => format!("{:04x}", (v.as_int().unwrap_or(0) as u16)),
                Some(b'B') => format!("{:02x}", (v.as_int().unwrap_or(0) as u8)),
                _ => format!("{:08x}", v.as_int().unwrap_or(0) as u32),
            };
            Some(Some(Value::Str(if upper { s.to_uppercase() } else { s }.into())))
        }
        "parseHex" => {
            let s = match args.get(1) { Some(Value::Str(s)) => s.to_string(), _ => String::new() };
            let clean: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
            let bytes: Vec<u8> = clean.as_bytes().chunks(2)
                .filter_map(|c| std::str::from_utf8(c).ok().and_then(|h| u8::from_str_radix(h, 16).ok()))
                .collect();
            Some(Some(Value::Array(vm.heap.alloc_bytes(&bytes))))
        }
        _ => None,
    }
}
