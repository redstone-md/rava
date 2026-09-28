//! The cascade tail, second half.

use crate::natives::*;

/// `Ok(None)` = no arm matched; the cascade continues.
pub(crate) fn owner_table2(
    vm: &mut Vm,
    plan: u32,
    verify: bool,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, VmError> {
    let stage = |bit: u32| plan & bit != 0 || verify;
    let _ = (stage, plan, verify);
    let handled: Option<Value> = match (owner, name, descriptor) {
        // java.lang.Class
        // java.lang.reflect.Array.newInstance(componentType, length).
        ("java/lang/reflect/Array", "newInstance", "(Ljava/lang/Class;I)Ljava/lang/Object;") => {
            let comp = class_field_name(vm, args.first());
            let len_i = args.get(1).and_then(Value::as_int).unwrap_or(0);
            // Same contract as the `newarray` opcode: a negative length is a
            // NegativeArraySizeException naming the length. `.max(0)` handed the
            // caller a silently EMPTY array instead.
            if len_i < 0 {
                return Err(VmError::Throw(vm.make_exception(
                    "java/lang/NegativeArraySizeException",
                    &len_i.to_string(),
                )));
            }
            let len = len_i as usize;
            let (elem, default) = match comp.as_str() {
                "int" | "short" | "byte" | "char" | "boolean" => ("I", Value::Int(0)),
                "long" => ("J", Value::Long(0)),
                "float" => ("F", Value::Float(0.0)),
                "double" => ("D", Value::Double(0.0)),
                other => (other, Value::Null),
            };
            // `Array.newInstance(int[].class, n)` builds an `int[][]`, so the
            // component name reaching here can itself be an array descriptor —
            // the same rule `opcodes::with_ref_desc` states for `anewarray`.
            let elem_desc = if elem.len() == 1 || elem.starts_with('[') {
                elem.to_string()
            } else {
                format!("L{elem};")
            };
            let arr = vm.heap.alloc_values(&elem_desc, vec![default; len]);
            Some(Value::Array(arr))
        }
        // Multi-dimensional: newInstance(componentType, int[] dimensions).
        ("java/lang/reflect/Array", "newInstance", "(Ljava/lang/Class;[I)Ljava/lang/Object;") => {
            let raw: Vec<i32> = match args.get(1) {
                Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default()
                    .iter().map(|v| v.as_int().unwrap_or(0)).collect(),
                _ => Vec::new(),
            };
            // Every dimension is validated before anything is allocated, exactly
            // as `multianewarray` does.
            if let Some(neg) = raw.iter().copied().find(|&d| d < 0) {
                return Err(VmError::Throw(vm.make_exception(
                    "java/lang/NegativeArraySizeException",
                    &neg.to_string(),
                )));
            }
            let dims: Vec<usize> = raw.iter().map(|&d| d as usize).collect();
            fn build(vm: &mut Vm, dims: &[usize]) -> Value {
                match dims.split_first() {
                    None => Value::Null,
                    Some((&n, rest)) if rest.is_empty() => {
                        Value::Array(vm.heap.alloc_values("Ljava/lang/Object;", vec![Value::Null; n]))
                    }
                    Some((&n, rest)) => {
                        let inner: Vec<Value> = (0..n).map(|_| build(vm, rest)).collect();
                        Value::Array(vm.heap.alloc_values("Ljava/lang/Object;", inner))
                    }
                }
            }
            Some(build(vm, &dims))
        }
        ("java/lang/reflect/Array", "getLength", _) => {
            Some(match args.first() { Some(Value::Array(h)) => Value::Int(vm.heap.array_len(*h).unwrap_or(0) as i32), _ => Value::Int(0) })
        }
        ("java/lang/reflect/Array", "get", _) => {
            let i = args.get(1).and_then(Value::as_int).unwrap_or(0) as usize;
            Some(match args.first() { Some(Value::Array(h)) => vm.heap.array_get(*h, i).unwrap_or(Value::Null), _ => Value::Null })
        }
        ("java/lang/reflect/Array", "set", _) => {
            let i = args.get(1).and_then(Value::as_int).unwrap_or(0) as usize;
            if let Some(Value::Array(h)) = args.first() { vm.heap.array_set(*h, i, args.get(2).cloned().unwrap_or(Value::Null)); }
            Some(Value::Null)
        }
        // Class.getModule() — an opaque unnamed-module stub; callers that probe
        // it (getName -> null, isNamed -> false) just need a non-null Module.
        ("java/lang/Class", "getModule", _) => {
            Some(Value::Ref(vm.heap.alloc("java/lang/Module")))
        }
        ("java/lang/Module", "getName", _) => Some(Value::Null),
        ("java/lang/Module", "isNamed", _) => Some(Value::Int(0)),
        ("java/lang/Module", "getDescriptor", _) => Some(Value::Null),
        // Module/ModuleLayer introspection (JvmProfiler probes for the jdk.jfr
        // module). No module system here: hand back an empty layer so JFR is
        // simply reported absent instead of NPEing on a null layer.
        ("java/lang/Module", "getLayer", _) | ("java/lang/ModuleLayer", "boot", _) => {
            Some(Value::Ref(vm.heap.alloc("java/lang/ModuleLayer")))
        }
        ("java/lang/ModuleLayer", "findModule", _) => {
            let opt = vm.heap.alloc("java/util/Optional");
            vm.heap.put_field(opt, "$present", Value::Int(0));
            Some(Value::Ref(opt))
        }
        ("java/lang/ModuleLayer", "modules", _) => {
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", Vec::new());
            let h = vm.heap.alloc("java/util/HashSet");
            vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
            Some(Value::Ref(h))
        }
        ("java/lang/Class", "getName", "()Ljava/lang/String;")
        | ("java/lang/Class", "getSimpleName", "()Ljava/lang/String;") => {
            let raw = class_field_name(vm, args.first());
            let dotted = raw.replace('/', ".");
            let out = if name == "getSimpleName" {
                dotted.rsplit('.').next().unwrap_or(&dotted).to_string()
            } else {
                dotted
            };
            Some(Value::Str(out.into()))
        }

        // java.lang.Class resource access — read an entry from the dir/jar and
        // hand back an InputStream (or null when absent).
        ("java/lang/Class", "getResourceAsStream", "(Ljava/lang/String;)Ljava/io/InputStream;") => {
            Some(get_resource_as_stream(vm, args.first(), args.get(1)))
        }

        // java.io.InputStream — backed by a byte[] buffer + position cursor.
        ("java/io/ByteArrayInputStream", "<init>", _) if descriptor.starts_with("([B") => {
            if let (Some(Value::Ref(r)), Some(Value::Array(buf))) = (args.first(), args.get(1)) {
                vm.heap.put_field(*r, "$buf", Value::Array(*buf));
                let off = args.get(2).and_then(Value::as_int).unwrap_or(0).max(0);
                vm.heap.put_field_syn(*r, crate::heap::Syn::Pos, Value::Int(off));
            }
            None
        }
        // `new FileInputStream(File|String)`: read the file into the receiver's
        // `$buf` so read()/Properties.load() see its bytes. Without this, reading
        // eula.txt yielded nothing and the server refused to start.
        ("java/io/FileInputStream", "<init>", _) => {
            let path = match args.get(1) {
                Some(Value::Str(s)) => Some(s.to_string()),
                Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Path) {
                    Value::Str(s) => Some(s.to_string()),
                    _ => match vm.heap.get_field_syn(*h, crate::heap::Syn::Name) {
                        Value::Str(s) => Some(s.to_string()),
                        _ => None,
                    },
                },
                _ => None,
            };
            if let (Some(p), Some(Value::Ref(recv))) = (path, args.first()) {
                match std::fs::read(&p) {
                    Ok(bytes) => {
                        let arr = vm.heap.alloc_bytes(&bytes);
                        vm.heap.put_field(*recv, "$buf", Value::Array(arr));
                        vm.heap.put_field_syn(*recv, crate::heap::Syn::Pos, Value::Int(0));
                        vm.heap.put_field_syn(*recv, crate::heap::Syn::Path, Value::Str(p.into()));
                    }
                    // Match the JVM: a missing file throws FileNotFoundException.
                    Err(_) => {
                        let e = vm.make_exception("java/io/FileNotFoundException", &p);
                        return Err(VmError::Throw(e));
                    }
                }
            }
            None
        }
        ("java/io/InputStream" | "java/io/FileInputStream", "read", "()I") => Some(Value::Int(stream_read_one(vm, args.first())?)),
        ("java/io/InputStream" | "java/io/FileInputStream", "available", "()I") => Some(Value::Int(stream_available(vm, args.first()))),
        ("java/io/InputStream" | "java/io/FileInputStream" | "java/io/FilterInputStream"
            | "java/io/PushbackInputStream" | "java/io/BufferedInputStream", "close", "()V") => None,
        ("java/io/InputStream" | "java/io/FileInputStream", "readAllBytes", "()[B") => Some(stream_read_all(vm, args.first())?),
        ("java/io/InputStream" | "java/io/FilterInputStream" | "java/io/PushbackInputStream"
            | "java/io/BufferedInputStream", "read", "([B)I") => {
            Some(Value::Int(stream_read_into(vm, args.first(), args.get(1), 0, None)?))
        }
        ("java/io/InputStream" | "java/io/FilterInputStream" | "java/io/PushbackInputStream"
            | "java/io/BufferedInputStream", "read", "([BII)I") => {
            let off = args.get(2).and_then(Value::as_int).unwrap_or(0) as usize;
            let len = args.get(3).and_then(Value::as_int).unwrap_or(0) as usize;
            Some(Value::Int(stream_read_into(vm, args.first(), args.get(1), off, Some(len))?))
        }
        ("java/io/PushbackInputStream" | "java/io/BufferedInputStream", "read", "()I") =>
            Some(Value::Int(stream_read_one(vm, args.first())?)),
        ("java/io/PushbackInputStream", "unread", "(I)V") => {
            // Push one byte back onto the stream: prepend it to a $pushback list
            // consulted by stream_read_one. (Charset/format detectors peek+unread.)
            if let (Some(Value::Ref(h)), Some(b)) = (args.first(), args.get(1).and_then(Value::as_int)) {
                let mut pb = match vm.heap.get_field(*h, "$pushback") { Value::Array(a) => vm.heap.array_values(a).unwrap_or_default(), _ => Vec::new() };
                pb.push(Value::Int(b & 0xFF));
                let arr = vm.heap.alloc_values("B", pb);
                vm.heap.put_field(*h, "$pushback", Value::Array(arr));
            }
            return Ok(Some(None));
        }
        ("java/io/PushbackInputStream", "unread", "([B)V" | "([BII)V") => {
            // Push back a slice `b[off..off+len]` (full array for the `([B)V` form).
            // After this the next reads return b[off], b[off+1], … — so we push them
            // onto the LIFO `$pushback` in REVERSE, leaving b[off] on top (popped first).
            // NbtIo.readCompressed peeks the gzip magic then unreads it this way; the
            // bulk `read([B…)` path drains `$pushback` first (see stream_read_into).
            if let (Some(Value::Ref(h)), Some(Value::Array(src))) = (args.first(), args.get(1)) {
                let bytes = vm.heap.array_values(*src).unwrap_or_default();
                let (off, len) = if descriptor == "([B)V" {
                    (0usize, bytes.len())
                } else {
                    let o = args.get(2).and_then(Value::as_int).unwrap_or(0) as usize;
                    let l = args.get(3).and_then(Value::as_int).unwrap_or(0) as usize;
                    (o, l)
                };
                let end = off.saturating_add(len).min(bytes.len());
                let mut pb = match vm.heap.get_field(*h, "$pushback") { Value::Array(a) => vm.heap.array_values(a).unwrap_or_default(), _ => Vec::new() };
                for i in (off..end).rev() {
                    let b = bytes[i].as_int().unwrap_or(0) & 0xFF;
                    pb.push(Value::Int(b));
                }
                let arr = vm.heap.alloc_values("B", pb);
                vm.heap.put_field(*h, "$pushback", Value::Array(arr));
            }
            return Ok(Some(None));
        }

        // GZIP/Inflater input: eagerly decompress the wrapped stream's bytes into
        // our own $buf so subsequent reads (and a wrapping DataInputStream) serve
        // the inflated NBT. Pairs with the GZIPOutputStream write path.
        // Reader wrappers — store a link ($stream) to the underlying byte
        // InputStream so readLine can pull from its buffer.
        ("java/io/InputStreamReader", "<init>", _) if descriptor.starts_with("(Ljava/io/InputStream;") => {
            if let Some(Value::Ref(r)) = args.first() {
                if let Some(src) = args.get(1).cloned() {
                    vm.heap.put_field(*r, "$stream", src);
                }
            }
            None
        }
        // StringReader/CharArrayReader hold their own bytes ($buf+$pos) and act as
        // the byte source directly (no wrapped InputStream). underlying_stream falls
        // back to the reader itself when there is no $stream link, so the shared
        // read machinery (read_chars/read/readLine) works unchanged. This is the
        // path JsonParser.parseString uses — without it gson saw EOF immediately.
        ("java/io/StringReader" | "java/io/CharArrayReader", "<init>", _) => {
            if let Some(Value::Ref(r)) = args.first() {
                let bytes: Vec<u8> = match args.get(1) {
                    Some(Value::Str(s)) => s.as_bytes().to_vec(),
                    Some(Value::Array(h)) => vm.heap.array_values(*h).unwrap_or_default()
                        .into_iter().filter_map(|v| char::from_u32(v.as_int().unwrap_or(0) as u32))
                        .collect::<String>().into_bytes(),
                    _ => Vec::new(),
                };
                let buf = vm.heap.alloc_bytes(&bytes);
                vm.heap.put_field(*r, "$buf", Value::Array(buf));
                vm.heap.put_field_syn(*r, crate::heap::Syn::Pos, Value::Int(0));
            }
            None
        }
        ("java/io/BufferedReader", "<init>", _) if descriptor.starts_with("(Ljava/io/Reader;") => {
            if let (Some(Value::Ref(r)), Some(Value::Ref(inner))) = (args.first(), args.get(1)) {
                // Prefer the inner reader's own underlying stream; if it has none
                // (e.g. a StringReader that IS the byte source), wrap the inner.
                let src = match vm.heap.get_field(*inner, "$stream") {
                    Value::Ref(s) => Value::Ref(s),
                    _ => Value::Ref(*inner),
                };
                vm.heap.put_field(*r, "$stream", src);
            }
            None
        }
        ("java/io/BufferedReader", "readLine", "()Ljava/lang/String;")
        | ("java/io/InputStreamReader", "readLine", "()Ljava/lang/String;") => {
            Some(read_line(vm, args.first())?)
        }
        ("java/io/InputStreamReader" | "java/io/BufferedReader" | "java/io/Reader" | "java/io/StringReader" | "java/io/CharArrayReader", "read", _)
            if descriptor.starts_with("([C") =>
        {
            Some(Value::Int(read_chars(vm, args.first(), args.get(1), args.get(2), args.get(3))?))
        }
        // read(CharBuffer): pull a chunk of bytes from the underlying stream,
        // UTF-8 decode, store into the CharBuffer's $sb; -1 at EOF. Backs
        // CharStreams.toString's `while (reader.read(buf) != -1)` loop.
        ("java/io/InputStreamReader" | "java/io/BufferedReader" | "java/io/Reader" | "java/io/StringReader" | "java/io/CharArrayReader", "read", _)
            if descriptor.starts_with("(Ljava/nio/CharBuffer;") =>
        {
            let cb = match args.get(1) { Some(Value::Ref(h)) => *h, _ => return Ok(Some(Some(Value::Int(-1)))) };
            let mut bytes = Vec::new();
            let us = underlying_stream(vm, args.first());
            if let Some(s) = us {
                for _ in 0..8192 {
                    let b = stream_read_one(vm, Some(&Value::Ref(s)))?;
                    if b < 0 { break; }
                    bytes.push(b as u8);
                }
            }
            let _ = us;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let n = text.chars().count();
            vm.heap.put_field_syn(cb, crate::heap::Syn::Sb, Value::Str(text.into()));
            Some(Value::Int(if n == 0 { -1 } else { n as i32 }))
        }
        ("java/io/InputStreamReader" | "java/io/BufferedReader" | "java/io/Reader" | "java/io/StringReader" | "java/io/CharArrayReader", "read", "()I") => {
            let stream = underlying_stream(vm, args.first());
            Some(Value::Int(match stream { Some(s) => stream_read_one(vm, Some(&Value::Ref(s)))?, None => -1 }))
        }
        ("java/io/InputStreamReader" | "java/io/BufferedReader" | "java/io/Reader" | "java/io/StringReader" | "java/io/CharArrayReader", "close", "()V")
        | ("java/io/BufferedReader" | "java/io/Reader" | "java/io/StringReader" | "java/io/CharArrayReader", "mark" | "reset", _) => None, // void no-ops
        ("java/io/InputStreamReader" | "java/io/BufferedReader" | "java/io/Reader" | "java/io/StringReader" | "java/io/CharArrayReader", "ready", "()Z") => {
            let s = underlying_stream(vm, args.first());
            Some(Value::Int(match s { Some(h) => (stream_available(vm, Some(&Value::Ref(h))) > 0) as i32, None => 0 }))
        }

        // java.io.DataInputStream / DataInputStream wrappers — big-endian reads.
        (c, "<init>", _)
            if descriptor.starts_with("(Ljava/io/InputStream;")
                && matches!(
                    c,
                    "java/io/DataInputStream"
                        | "java/io/DataInput"
                        | "java/io/ObjectInputStream"
                        | "java/io/BufferedInputStream"
                        | "java/io/PushbackInputStream"
                        | "java/io/FilterInputStream"
                ) =>
        {
            // Transparent byte wrappers: link $stream to the wrapped source so the
            // recursive underlying_stream chain reaches the real $buf holder. (Pushback
            // adds its own $pushback layer on top, served first by the read helpers.)
            if let (Some(Value::Ref(r)), Some(src)) = (args.first(), args.get(1).cloned()) {
                vm.heap.put_field(*r, "$stream", src);
            }
            None
        }
        // Inflater/GZIP streams: <init> eagerly inflates into the receiver's own $buf, so reads
        // serve straight from it. Without these arms a direct read on the zip stream (not via a
        // DataInputStream wrapper) is a ULE.
        ("java/util/zip/GZIPInputStream" | "java/util/zip/InflaterInputStream", "read", "()I") => {
            Some(Value::Int(stream_read_one(vm, args.first())?))
        }
        ("java/util/zip/GZIPInputStream" | "java/util/zip/InflaterInputStream", "read", d)
            if d.starts_with("([B") =>
        {
            let off = args.get(2).and_then(Value::as_int).unwrap_or(0) as usize;
            let len = args.get(3).and_then(Value::as_int).map(|l| l as usize);
            Some(Value::Int(stream_read_into(vm, args.first(), args.get(1), off, len)?))
        }
        ("java/util/zip/GZIPInputStream" | "java/util/zip/InflaterInputStream", "available", "()I") => {
            Some(Value::Int(stream_available(vm, args.first())))
        }
        ("java/io/DataInputStream" | "java/io/DataInput", n, _)
            if n.starts_with("read") || n == "skipBytes" || n == "close" || n == "available" || n == "mark" || n == "reset" =>
        {
            return Ok(Some(data_read(vm, n, args)?));
        }

        // String.getChars(srcBegin, srcEnd, char[] dst, dstBegin) — void.
        ("java/lang/String", "getChars", _) => {
            if let (Some(s), Some(Value::Array(dst))) = (str_of(vm, args.first()), args.get(3)) {
                let dst = *dst;
                let sb = args.get(1).and_then(Value::as_int).unwrap_or(0) as usize;
                let se = args.get(2).and_then(Value::as_int).unwrap_or(0) as usize;
                let db = args.get(4).and_then(Value::as_int).unwrap_or(0) as usize;
                let chars: Vec<char> = s.chars().collect();
                for (i, idx) in (sb..se).enumerate() {
                    if let Some(c) = chars.get(idx) {
                        vm.heap.array_set(dst, db + i, Value::Int(*c as i32));
                    }
                }
            }
            None
        }

        // java.lang.String constructors: byte[]/char[] (+offset/len), or copy.
        ("java/lang/String", "<init>", _) => {
            if let Some(Value::Ref(r)) = args.first() {
                let text = build_string(vm, descriptor, args)?;
                vm.heap.put_field_syn(*r, crate::heap::Syn::Str, Value::Str(text.into()));
            }
            None
        }

        // THE ONLY OTHER EXIT, and the whole warrant for `table2_may_serve` skipping this call:
        // every arm above is keyed on an owner literal or the exact buffered-wrapper provider;
        // an owner in neither set can only arrive here.
        _ => return Ok(None),
    };
    Ok(Some(handled))
}

// ── OWNER FILTER FOR THE SECOND TAIL TABLE ─────────────────────────────────
// See the block comment above `owner_sig` in `cascade_table.rs` for the mechanism and
// measurement. The owner filter mirrors the exact owner literals used by native arms above.

/// Every owner literal in `owner_table2`.
/// Checked against this file's own source by `no_class_literal_in_this_file_is_unregistered`.
pub(crate) const OWNERS2: &[&str] = &[
    "java/lang/reflect/Array",
    "java/lang/Class",
    "java/lang/Module",
    "java/lang/ModuleLayer",
    "java/lang/String",
    "java/io/ByteArrayInputStream",
    "java/io/FileInputStream",
    "java/io/InputStream",
    "java/io/FilterInputStream",
    "java/io/PushbackInputStream",
    "java/io/BufferedInputStream",
    "java/io/InputStreamReader",
    "java/io/StringReader",
    "java/io/CharArrayReader",
    "java/io/BufferedReader",
    "java/io/Reader",
    "java/io/DataInputStream",
    "java/io/DataInput",
    "java/io/ObjectInputStream",
    "java/util/zip/GZIPInputStream",
    "java/util/zip/InflaterInputStream",
];

/// See `FILTER_SECTION_MARKER` in `cascade_table.rs`: the drift test reads only the part of this
/// file above the first occurrence of this text.
#[cfg(test)]
const FILTER2_SECTION_MARKER: &str = "OWNER FILTER FOR THE SECOND TAIL TABLE";

/// Class-shaped literals in this file that are NOT owner keys — see `NOT_OWNERS1`.
#[cfg(test)]
const NOT_OWNERS2: &[&str] = &[
    "java/io/FileNotFoundException",
    "java/lang/NegativeArraySizeException",
    "java/util/HashSet",
    "java/util/Optional",
];

static MAP2: [u64; SIG_WORDS] = sig_map(OWNERS2);


/// Can any arm of [`owner_table2`] be keyed on this owner?
///
/// `false` is a PROOF OF ABSENCE, on the same terms as [`table1_may_serve`]: not in the literal
/// set, and the table's only other exit is `_ => return Ok(None)`.
#[inline]
pub(crate) fn table2_may_serve(sig: u32, _owner: &str) -> bool {
    sig_maybe(&MAP2, sig)
}

#[cfg(test)]
mod filter2_tests {
    use super::*;

    #[test]
    fn every_registered_owner_is_admitted() {
        for o in OWNERS2 {
            assert!(table2_may_serve(owner_sig(o), o), "{o} rejected by its own filter");
        }
    }

    /// An unrelated custom wrapper has no native table arms and must run its loaded bytecode.
    #[test]
    fn custom_buffered_stream_is_not_native_routed() {
        let class = "net/minecraft/util/FastBufferedInputStream";
        assert!(!OWNERS2.contains(&class));
        assert!(!table2_may_serve(owner_sig(class), class));
    }

    /// See `no_class_literal_in_this_file_is_unregistered` in `cascade_table.rs` — same invariant,
    /// same failure mode, this file's source.
    #[test]
    fn no_class_literal_in_this_file_is_unregistered() {
        let src = include_str!("cascade_table2.rs");
        let body = src.split(FILTER2_SECTION_MARKER).next().unwrap_or(src);
        for lit in crate::natives::class_literals(body) {
            let known = OWNERS2.contains(&lit.as_str()) || NOT_OWNERS2.contains(&lit.as_str());
            assert!(
                known,
                "class literal {lit:?} appears in cascade_table2.rs but is in neither OWNERS2 nor \
                 NOT_OWNERS2. If it is the owner of a new match arm it MUST go in OWNERS2, or \
                 `table2_may_serve` will skip that arm for every call."
            );
        }
    }

    #[test]
    fn representative_jar_classes_are_rejected() {
        let rejected = [
            "net/minecraft/world/level/levelgen/NoiseChunk",
            "net/minecraft/core/Holder$Reference",
            "it/unimi/dsi/fastutil/objects/Object2ObjectOpenHashMap",
            "com/mojang/serialization/codecs/RecordCodecBuilder",
        ]
        .iter()
        .filter(|o| !table2_may_serve(owner_sig(o), o))
        .count();
        assert!(rejected >= 3, "the fingerprint is not discriminating: only {rejected}/4 rejected");
    }

    #[test]
    fn the_map_is_one_cache_line() {
        assert_eq!(std::mem::size_of_val(&MAP2), 64);
    }
}
