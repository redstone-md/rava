//! Streams, writers and the data-input reads.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// The decode path's bulk-copy gate. DEFAULT ON; `RAVA_NO_FAST_DECODE=1` (or the
/// tree-wide `RAVA_NO_EMIT_MORE=1`) restores the byte-at-a-time / whole-array-clone
/// forms of every stream and buffer copy in `io.rs`, `streamio.rs` and `buffers.rs`,
/// so the whole family bisects in ONE run instead of one flag per call site.
///
/// State: a single `OnceLock<bool>` (16 bytes, read-only after the first call) —
/// nothing that competes for L1 with the buffers it is guarding.
pub(crate) fn fast_decode_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_DECODE").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Read up to `len` bytes into `dst` at `off`; returns the count or -1 at EOF.
pub(crate) fn stream_read_into(
    vm: &mut Vm,
    recv: Option<&Value>,
    dst: Option<&Value>,
    off: usize,
    len: Option<usize>,
) -> Result<i32, VmError> {
    let dst = match dst {
        Some(Value::Array(d)) => *d,
        _ => return Ok(-1),
    };
    let want = len.unwrap_or_else(|| vm.heap.array_len(dst).unwrap_or(0));
    if want == 0 {
        return Ok(0);
    }

    let mut written = 0usize;
    if let Some(Value::Ref(h)) = recv {
        if let Value::Array(a) = vm.heap.get_field(*h, "$pushback") {
            if !fast_decode_on() || vm.heap.array_len(a).unwrap_or(0) != 0 {
                let mut pb = vm.heap.array_values(a).unwrap_or_default();
                while written < want {
                    match pb.pop() {
                        Some(b) => {
                            vm.heap.array_set(dst, off + written, Value::Int((b.as_int().unwrap_or(0) as u8) as i8 as i32));
                            written += 1;
                        }
                        None => break,
                    }
                }
                let arr = vm.heap.alloc_values("B", pb);
                vm.heap.put_field(*h, "$pushback", Value::Array(arr));
            }
        }
    }
    if written >= want {
        return Ok(written as i32);
    }

    let Some((sh, buf, pos)) = stream_state(vm, recv) else {
        if written > 0 {
            return Ok(written as i32);
        }
        let Some(stream) = underlying_stream(vm, recv) else {
            return Ok(-1);
        };
        return Ok(guest_stream_read_into(vm, stream, dst, off, want)?.unwrap_or(-1));
    };
    if fast_decode_on() {
        if let Some(chunk) = vm.heap.array_read_bytes(buf, pos, want - written) {
            if chunk.is_empty() {
                return Ok(if written > 0 { written as i32 } else { -1 });
            }
            if !vm.heap.array_write_bytes(dst, off + written, &chunk) {
                for (i, &b) in chunk.iter().enumerate() {
                    vm.heap.array_set(dst, off + written + i, Value::Int(b as i8 as i32));
                }
            }
            vm.heap.put_field_syn(sh, crate::heap::Syn::Pos, Value::Int((pos + chunk.len()) as i32));
            return Ok((written + chunk.len()) as i32);
        }
    }
    let src = vm.heap.array_bytes(buf).unwrap_or_default();
    if pos >= src.len() {
        return Ok(if written > 0 { written as i32 } else { -1 });
    }
    let n = (want - written).min(src.len() - pos);
    for i in 0..n {
        vm.heap.array_set(dst, off + written + i, Value::Int(src[pos + i] as i8 as i32));
    }
    vm.heap.put_field_syn(sh, crate::heap::Syn::Pos, Value::Int((pos + n) as i32));
    Ok((written + n) as i32)
}

/// Invoke a concrete guest `read(byte[], off, len)` body when no synthetic stream model exists.
pub(crate) fn guest_stream_read_into(
    vm: &mut Vm,
    stream: u32,
    dst: u32,
    off: usize,
    len: usize,
) -> Result<Option<i32>, VmError> {
    let owner = vm.heap.class_of(stream).unwrap_or("").to_owned();
    let Some(method) = vm.resolve_method(&owner, "read", "([BII)I")? else {
        return Ok(None);
    };
    let value = vm.invoke(
        method,
        "read",
        "([BII)I",
        vec![
            Value::Ref(stream),
            Value::Array(dst),
            Value::Int(off as i32),
            Value::Int(len as i32),
        ],
    )?;
    Ok(value.and_then(|v| v.as_int()))
}

pub(crate) fn data_read(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Option<Value>, VmError> {
    let recv = args.first();
    let v = match name {
        "readInt" => { let b = data_read_bytes(vm, recv, 4)?; Value::Int(be_i64(&b) as i32) }
        "readLong" => { let b = data_read_bytes(vm, recv, 8)?; Value::Long(be_i64(&b)) }
        "readShort" => { let b = data_read_bytes(vm, recv, 2)?; Value::Int(be_i64(&b) as i16 as i32) }
        "readUnsignedShort" | "readChar" => { let b = data_read_bytes(vm, recv, 2)?; Value::Int(be_i64(&b) as i32) }
        "readByte" => { let b = data_read_bytes(vm, recv, 1)?; Value::Int(b.first().map(|x| *x as i8 as i32).unwrap_or(-1)) }
        "readUnsignedByte" | "read" => { let b = data_read_bytes(vm, recv, 1)?; Value::Int(b.first().map(|x| *x as i32).unwrap_or(-1)) }
        "readBoolean" => { let b = data_read_bytes(vm, recv, 1)?; Value::Int((b.first().copied().unwrap_or(0) != 0) as i32) }
        "readFloat" => { let b = data_read_bytes(vm, recv, 4)?; Value::Float(f32::from_bits(be_i64(&b) as u32)) }
        "readDouble" => { let b = data_read_bytes(vm, recv, 8)?; Value::Double(f64::from_bits(be_i64(&b) as u64)) }
        "readUTF" => {
            let len_b = data_read_bytes(vm, recv, 2)?;
            let len = be_i64(&len_b) as usize;
            let body = data_read_bytes(vm, recv, len)?;
            // `from_utf8_lossy(..).into_owned()` COPIES the whole body even when it is
            // already valid UTF-8 — which every NBT/registry name is — because the
            // `Cow::Borrowed` it returns has to be cloned to be owned. `from_utf8` takes
            // the `Vec` we already own. Identical answer: on valid input the lossy form
            // produces exactly these bytes, and the malformed case still goes through it.
            Value::Str(match String::from_utf8(body) {
                Ok(s) => s,
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
            }.into())
        }
        "skipBytes" => {
            let n = args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let got = data_read_bytes(vm, recv, n)?;
            Value::Int(got.len() as i32)
        }
        "available" => {
            let s = underlying_stream(vm, recv);
            Value::Int(match s { Some(h) => stream_available(vm, Some(&Value::Ref(h))), None => 0 })
        }
        "readFully" => {
            if let Some(Value::Array(dst)) = args.get(1) {
                let dst = *dst;
                let off = args.get(2).and_then(Value::as_int).unwrap_or(0) as usize;
                let len = args.get(3).and_then(Value::as_int).map(|v| v as usize)
                    .unwrap_or_else(|| vm.heap.array_len(dst).unwrap_or(0));
                let bytes = data_read_bytes(vm, recv, len)?;
                // ⭐**THE ONE `readFully` THE BULK-COPY ROUND MISSED.** `stream_read_into` above
                // was taught to `array_write_bytes`; this arm still moved the payload ONE BOXED
                // `Value` AT A TIME — per byte: an `array_set` call, the destination handle
                // re-resolved, a bounds check, and an `i32` boxed and unboxed. `readFully` is how
                // `DataInputStream` is asked for a WHOLE record (NbtIo reads a chunk payload,
                // `readUTF`'s body, a region sector), so the population is bytes-of-file, not
                // calls: kilobytes per call.
                //
                // Bit-exact with the loop it replaces: `array_write_bytes` stores the raw `u8`,
                // which is exactly what `Value::Int(b as i8 as i32)` truncates back to on store.
                // It range-checks BEFORE copying and refuses without writing anything, so a
                // `char[]`/`int[]` destination (a Reader's buffer) or a short one falls through
                // to the identical element-wise path.
                if !crate::nio::fast_nioshape()
                    || !vm.heap.array_write_bytes(dst, off, &bytes)
                {
                    for (i, b) in bytes.iter().enumerate() {
                        vm.heap.array_set(dst, off + i, Value::Int(*b as i8 as i32));
                    }
                }
            }
            return Ok(None); // void
        }
        "close" | "mark" | "reset" => return Ok(None), // void
        _ => return Ok(None),
    };
    Ok(Some(v))
}

/// java.io Writer family (StringWriter/PrintWriter/...): accumulate into `$sb`.
/// `Err` is the guest-throw channel `printf`/`format` need: a conversion with no
/// argument is `MissingFormatArgumentException` on the JVM, and a writer that
/// swallowed it would print a plausible wrong line instead.
pub(crate) fn writer_dispatch(vm: &mut Vm, _owner: &str, name: &str, descriptor: &str, args: &[Value]) -> Result<Option<Option<Value>>, VmError> {
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Ok(None) };
    Ok(match name {
        "<init>" => {
            match args.get(1) {
                // FileWriter(String path) → write target is that file.
                Some(Value::Str(p)) => vm.heap.put_field_syn(recv, crate::heap::Syn::Path, Value::Str(p.clone())),
                Some(Value::Ref(h)) => {
                    // FileWriter(File): take its $path. Else wrap the writer/stream
                    // (its own $path, if any, is reached via writer_target's $out).
                    match vm.heap.get_field_syn(*h, crate::heap::Syn::Path) {
                        Value::Str(p) => vm.heap.put_field_syn(recv, crate::heap::Syn::Path, Value::Str(p)),
                        _ => vm.heap.put_field(recv, "$out", Value::Ref(*h)),
                    }
                }
                _ => {}
            }
            Some(None)
        }
        "write" | "append" | "print" => {
            let target = writer_target(vm, recv);
            // Writer.write(int) / Appendable.append(int) write a single CHAR (low 16 bits),
            // not the decimal text — print(int) writes decimal. gson's JsonWriter emits
            // structural punctuation ({ } : , " [ ]) via write(int); decimal here corrupts it.
            let text = if name != "print" && descriptor.as_bytes().get(1) == Some(&b'I') {
                if let Some(v) = args.get(1) {
                    char::from_u32(v.as_int().unwrap_or(0) as u32 & 0xFFFF).map(String::from).unwrap_or_default()
                } else { String::new() }
            } else {
                writer_text(vm, descriptor, args.get(1))
            };
            // 3-arg forms write(src, off, len) / append(cs, start, end) emit only a
            // slice. Ignoring off/len wrote the whole source on every call, so the
            // YAML emitter (which writes a scalar one space-separated chunk at a
            // time) repeated each multi-word value (2N-1)x.
            //
            // ⭐**A WINDOW OF THE TEXT COST A `Vec<char>` OF THE WHOLE TEXT, PLUS A SECOND
            // `String` FOR THE WINDOW.** Java's writer idiom is a big reusable `char[]` written
            // in small pieces — gson's `JsonWriter`, the YAML emitter, every `BufferedWriter`
            // flush — so an 8 KiB buffer written 40 characters at a time exploded 8,192 chars
            // into a 32 KiB `Vec<char>` and collected 40 of them back out, per call:
            // **O(text) allocation and copying for an O(window) answer**, and the waste is the
            // ratio of the two. Char INDICES still decide the window (Java counts UTF-16 units
            // and this engine's strings are the same text), so the replacement is one pass of
            // `char_indices` that records the two byte offsets and then borrows the slice —
            // zero allocation, same bytes appended.
            //
            // The `ba <= bk` guard reproduces the old `a <= end` refusal exactly: `end` was
            // `min(k, count)`, so `a > end` meant either `a > k` (an `append` whose END is
            // before its START) or `a > count` (a start past the text) — and both land here as
            // `ba > bk` or as `ba == bk == text.len()`, which yields the same empty write.
            let windowed: String; // only the legacy path fills it; kept alive for the borrow
            let text: &str = match (args.get(2).and_then(Value::as_int), args.get(3).and_then(Value::as_int)) {
                (Some(a), Some(b)) if args.len() >= 4 => {
                    let a = a.max(0) as usize;
                    // write(char[]/String, off, LEN) vs append(CharSequence, start, END):
                    // CharSequence's third arg is an end index, the others a length.
                    let cs = descriptor.starts_with("(Ljava/lang/CharSequence;");
                    if crate::nio::fast_nioshape() {
                        let k = if cs { b.max(0) as usize } else { a.saturating_add(b.max(0) as usize) };
                        let (mut ba, mut bk) = (None, None);
                        for (i, (byte, _)) in text.char_indices().enumerate() {
                            if i == a { ba = Some(byte); }
                            if i == k { bk = Some(byte); break; }
                        }
                        // A char index at or past the end maps to the end of the text, which is
                        // what `min(.., chars.len())` did.
                        let ba = ba.unwrap_or(text.len());
                        let bk = bk.unwrap_or(text.len());
                        if ba <= bk { &text[ba..bk] } else { "" }
                    } else {
                        let chars: Vec<char> = text.chars().collect();
                        let end = if cs {
                            (b.max(0) as usize).min(chars.len())
                        } else {
                            (a + b.max(0) as usize).min(chars.len())
                        };
                        windowed = if a <= end { chars[a..end].iter().collect() } else { String::new() };
                        windowed.as_str()
                    }
                }
                _ => text.as_str(),
            };
            writer_append(vm, target, text);
            if name == "append" { Some(Some(Value::Ref(recv))) } else { Some(None) }
        }
        // `flush`/`close` on a print stream: a no-op here, because `writer_append` writes through
        // to the real handle immediately — there is no buffer to drain.
        //
        // ⭐It was MISSING, and a missing native is not a slow path, it is an
        // `UnsatisfiedLinkError`: `bench/BootPhaseC` — the probe that exists specifically to
        // attribute the unexplained gap between `Environment:` and the yggdrasil failure, which
        // is 10 s of a 33 s boot — died on its first `System.out.flush()` and had never produced
        // a single step timing.
        "flush" | "close" => Some(None),
        "println" => {
            let target = writer_target(vm, recv);
            let text = match args.get(1) { Some(_) => writer_text(vm, descriptor, args.get(1)), None => String::new() };
            // `format!("{text}\n")` allocated a second copy of the whole line to add one byte to
            // it, and `sb_append` is an amortised in-place append that can take the two pieces
            // directly. Same bytes, same order, one allocation and one memcpy of the line less
            // per `println` — and a log line is the population.
            writer_append(vm, target, &text);
            writer_append(vm, target, "\n");
            Some(None)
        }
        "printf" | "format" => {
            // (String,Object...) vs (Locale,String,Object...) — pick the operand pair
            // from the DESCRIPTOR, not from the first arg's runtime type. See the
            // matching fix on the System.out path in opcodes.rs.
            let fi = if descriptor.starts_with("(Ljava/util/Locale;") { 2 } else { 1 };
            let fmt = str_of(vm, args.get(fi)).unwrap_or_default();
            let fargs: Vec<Value> = varargs(vm, args.get(fi + 1)).iter().map(|v| stringify(vm, v)).collect();
            let text = match format_java(&fmt, &fargs) {
                Ok(t) => t,
                Err((class, msg)) => return Err(VmError::Throw(vm.make_exception(class, &msg))),
            };
            let target = writer_target(vm, recv);
            writer_append(vm, target, &text);
            Some(Some(Value::Ref(recv)))
        }
        "newLine" => { let t = writer_target(vm, recv); writer_append(vm, t, "\n"); Some(None) }
        "toString" => {
            let t = writer_target(vm, recv);
            Some(Some(Value::Str(vm.heap.sb_read(t).into())))
        }
        "getBuffer" => {
            let t = writer_target(vm, recv);
            let sb = vm.heap.alloc("java/lang/StringBuilder");
            let cur = vm.heap.sb_read(t);
            vm.heap.sb_set(sb, &cur);
            Some(Some(Value::Ref(sb)))
        }
        "flush" | "close" => {
            // A file-backed writer (Files.newBufferedWriter / FileWriter) persists
            // its buffered $sb to disk here (truncating create).
            let t = writer_target(vm, recv);
            if let Value::Str(path) = vm.heap.get_field_syn(t, crate::heap::Syn::Path) {
                let content = vm.heap.sb_read(t);
                let _ = std::fs::write(path.as_ref(), content.as_bytes());
            }
            Some(None)
        }
        "checkError" => Some(Some(Value::Int(0))),
        _ => None,
    })
}
