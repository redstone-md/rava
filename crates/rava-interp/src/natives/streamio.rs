//! Stream plumbing: the wrapped source, single reads, availability and resources.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::natives::*;

/// `Class.getResourceAsStream`: resolve the path (absolute `/x` or relative to
/// the class's package), read it, and wrap the bytes in an InputStream object.
pub(crate) fn get_resource_as_stream(vm: &mut Vm, class_obj: Option<&Value>, res: Option<&Value>) -> Value {
    // The name may arrive as a Value::Str (interpreter) OR a Value::Obj ArenaString
    // (compiled makeConcatWithConstants) — str_of handles both. Matching only Str here
    // returned Null for every compiled/AOT caller (e.g. the bundler's readResource), the
    // same Str-vs-ArenaString class as th=128.
    let name = match str_of(vm, res) {
        Some(s) => s,
        None => return Value::Null,
    };
    let path = if let Some(stripped) = name.strip_prefix('/') {
        stripped.to_string()
    } else {
        let cls = class_field_name(vm, class_obj);
        match cls.rfind('/') {
            Some(i) => format!("{}/{}", &cls[..i], name),
            None => name.clone(),
        }
    };
    match vm.loader.read_resource(&path) {
        Some(bytes) => make_stream(vm, &bytes),
        None => Value::Null,
    }
}

/// Wrap raw bytes in a fresh `InputStream` object (`$buf` byte[] + `$pos`).
pub(crate) fn make_stream(vm: &mut Vm, bytes: &[u8]) -> Value {
    let buf = vm.heap.alloc_bytes(bytes);
    let stream = vm.heap.alloc("java/io/InputStream");
    vm.heap.put_field(stream, "$buf", Value::Array(buf));
    vm.heap.put_field_syn(stream, crate::heap::Syn::Pos, Value::Int(0));
    Value::Ref(stream)
}

pub(crate) fn stream_read_one(vm: &mut Vm, recv: Option<&Value>) -> Result<i32, VmError> {
    if let Some(Value::Ref(h)) = recv {
        if let Value::Array(a) = vm.heap.get_field(*h, "$pushback") {
            if !fast_decode_on() || vm.heap.array_len(a).unwrap_or(0) != 0 {
                let mut pb = vm.heap.array_values(a).unwrap_or_default();
                if let Some(b) = pb.pop() {
                    let arr = vm.heap.alloc_values("B", pb);
                    vm.heap.put_field(*h, "$pushback", Value::Array(arr));
                    return Ok((b.as_int().unwrap_or(0) as u8) as i32);
                }
            }
        }
    }
    let Some((sh, buf, pos)) = stream_state(vm, recv) else {
        let Some(stream) = underlying_stream(vm, recv) else {
            return Ok(-1);
        };
        let dst = vm.heap.alloc_bytes(&[0]);
        let Some(n) = guest_stream_read_into(vm, stream, dst, 0, 1)? else {
            return Ok(-1);
        };
        if n <= 0 {
            return Ok(-1);
        }
        return Ok(vm.heap.array_get(dst, 0).and_then(|v| v.as_int()).unwrap_or(0) as u8 as i32);
    };
    match vm.heap.array_get(buf, pos) {
        Some(v) => {
            vm.heap.put_field_syn(sh, crate::heap::Syn::Pos, Value::Int(pos as i32 + 1));
            Ok((v.as_int().unwrap_or(0) as u8) as i32)
        }
        None => Ok(-1),
    }
}

pub(crate) fn stream_available(vm: &Vm, recv: Option<&Value>) -> i32 {
    let pushed = match recv {
        Some(Value::Ref(h)) => match vm.heap.get_field(*h, "$pushback") {
            Value::Array(a) => vm.heap.array_len(a).unwrap_or(0),
            _ => 0,
        },
        _ => 0,
    };
    let underlying = match stream_state(vm, recv) {
        Some((_, buf, pos)) => vm.heap.array_len(buf).unwrap_or(0).saturating_sub(pos),
        None => 0,
    };
    (pushed + underlying) as i32
}

/// Follow a reader's `$stream` link to the underlying InputStream handle (or
/// treat the receiver itself as the stream).
pub(crate) fn underlying_stream(vm: &Vm, recv: Option<&Value>) -> Option<u32> {
    let mut h = match recv {
        Some(Value::Ref(h)) => *h,
        _ => return None,
    };
    // Follow the synthetic $stream chain until a modeled $buf holder or terminal source.
    for _ in 0..16 {
        if matches!(vm.heap.get_field(h, "$buf"), Value::Array(_)) {
            return Some(h);
        }
        match vm.heap.get_field(h, "$stream") {
            Value::Ref(s) => h = s,
            _ => return Some(h),
        }
    }
    Some(h)
}

/// ⭐**`RAVA_NO_CHARBUF=1` (or `RAVA_NO_EMIT_MORE=1`) restores the historic two-step refill
/// EXACTLY** — `array_read_bytes` into a fresh `Vec<u8>`, then `array_write_chars` out of it.
///
/// Resolved ONCE through a `OnceLock`: this is per-refill, not per-char, so the load is amortised
/// over a whole KB of text and there is no reason for the relaxed-atomic form the element paths use.
fn charbuf_fuse() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_CHARBUF").is_none() && !crate::flags::emit_more_disabled()
    })
}

// [chars] census — `RAVA_DIAG=chars`. Every one of these is a `diag_counter`, so with no category
// armed the handle is a compile-time constant and the `fetch_add` folds away entirely; nothing here
// costs a disarmed run an instruction. Events are per REFILL (one `Reader.read(char[],off,len)`),
// never per character.
crate::diag_counter!(pub(crate) charbuf_fills, "chars", "fills", "Reader.read(char[]) refills of a guest char[]");
crate::diag_counter!(pub(crate) charbuf_want_chars, "chars", "want_chars", "chars the guest asked those refills for");
crate::diag_counter!(pub(crate) charbuf_fused_fills, "chars", "fused_fills", "refills served byte[]->char[] in place, with no transient Vec");
crate::diag_counter!(pub(crate) charbuf_fused_bytes, "chars", "fused_bytes", "bytes widened in place — equally, transient-copy bytes NOT allocated");
crate::diag_counter!(pub(crate) charbuf_copy_fills, "chars", "copy_fills", "refills that still went through the two-step copy (shape the fused arm declined)");
crate::diag_counter!(pub(crate) charbuf_copy_bytes, "chars", "copy_bytes", "bytes copied into a transient Vec before being widened");
crate::diag_counter!(pub(crate) charbuf_loop_fills, "chars", "loop_fills", "refills that fell all the way to the one-byte-at-a-time loop");
crate::diag_counter!(pub(crate) charbuf_loop_bytes, "chars", "loop_bytes", "bytes moved one at a time by that loop");

/// Read up to `len` chars from a reader's underlying byte stream into `dst`
/// at `off`. Returns the count read, or -1 at end of input.
pub(crate) fn read_chars(
    vm: &mut Vm,
    recv: Option<&Value>,
    dst: Option<&Value>,
    off: Option<&Value>,
    len: Option<&Value>,
) -> Result<i32, VmError> {
    let Some(stream) = underlying_stream(vm, recv) else { return Ok(-1) };
    let dst = match dst { Some(Value::Array(d)) => *d, _ => return Ok(-1) };
    let off = off.and_then(Value::as_int).unwrap_or(0) as usize;
    let want = len.and_then(Value::as_int).map(|v| v as usize)
        .unwrap_or_else(|| vm.heap.array_len(dst).unwrap_or(0));
    let sv = Some(Value::Ref(stream));
    // THE POPULATION THIS AREA SERVES: one event per `char[]` refill, not per char, so an armed
    // run costs ~1 relaxed add per KB of text and a disarmed one costs nothing at all (a
    // `diag_counter` handle folds to a constant when `RAVA_DIAG` names no category — see the
    // macro's own note). `want` is what the guest asked for; the fill counters below say what the
    // heap actually moved and by which arm.
    charbuf_fills().inc();
    charbuf_want_chars().add(want as u64);
    // BULK, default on (`RAVA_NO_BULK_CHARS` opts out). The loop below moves ONE byte per
    // `$pushback` name-hash + stream-state walk + boxed `array_get` + `$pos` write; the datapack
    // phase drains 5 MB through it. `DataInputStream` already had this treatment
    // (`RAVA_NO_BULK_READ`); this is the same loop, missed.
    static BULK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *BULK.get_or_init(|| std::env::var_os("RAVA_NO_BULK_CHARS").is_none()) {
        // A pushback buffer must still be served LIFO, one byte at a time, so the bulk path is
        // taken only when there is none — the same guard `data_read_bytes` uses.
        let pushed = match vm.heap.get_field(stream, "$pushback") {
            Value::Array(a) => vm.heap.array_len(a).unwrap_or(0) > 0,
            _ => false,
        };
        if !pushed {
            if let Some((sh, buf, pos)) = crate::natives::stream_state(vm, recv) {
                // FUSED REFILL (`RAVA_NO_CHARBUF=1` restores the two-step below verbatim).
                //
                // ⭐The bulk path was bulk in two steps: `array_read_bytes` COPIES the run out of
                // the heap into a fresh `Vec<u8>`, and `array_write_chars` then widens that copy
                // into the guest `char[]`. The copy never escapes this call — one malloc and one
                // memcpy of the whole run, per refill, thrown away on the next line. gson's
                // `JsonReader` refills a `char[1024]`, so a 784-byte datapack file paid it once
                // and a large one once per KB. `array_widen_bytes_to_chars` reaches both arrays
                // through one `split_at_mut` and widens in place; it declines (and only declines)
                // any shape it cannot prove, and the two-step below then runs exactly as before.
                let fused = if charbuf_fuse() {
                    vm.heap.array_widen_bytes_to_chars(buf, pos, dst, off, want)
                } else {
                    None
                };
                let n = match fused {
                    Some(n) => {
                        charbuf_fused_fills().inc();
                        charbuf_fused_bytes().add(n as u64);
                        Some(n)
                    }
                    None => vm.heap.array_read_bytes(buf, pos, want).map(|bytes| {
                        charbuf_copy_fills().inc();
                        charbuf_copy_bytes().add(bytes.len() as u64);
                        vm.heap.array_write_chars(dst, off, &bytes)
                    }),
                };
                if let Some(n) = n {
                    if n > 0 {
                        vm.heap.put_field_syn(sh, crate::heap::Syn::Pos, Value::Int((pos + n) as i32));
                    }
                    return Ok(if n == 0 && want > 0 { -1 } else { n as i32 });
                }
            }
        }
    }
    let mut n = 0usize;
    while n < want {
        let b = stream_read_one(vm, sv.as_ref())?;
        if b < 0 { break; }
        vm.heap.array_set(dst, off + n, Value::Int(b));
        n += 1;
    }
    // The one-byte-at-a-time arm: a pushback buffer is pending, or the `$buf` is not a byte array.
    // Counted so "the bulk path serves everything" is a number rather than an assumption.
    charbuf_loop_fills().inc();
    charbuf_loop_bytes().add(n as u64);
    Ok(if n == 0 && want > 0 { -1 } else { n as i32 })
}

/// Render a writer write/print argument by its declared type.
pub(crate) fn writer_text(vm: &mut Vm, descriptor: &str, arg: Option<&Value>) -> String {
    let t = descriptor.as_bytes().get(1).copied();
    match (t, arg) {
        (Some(b'Z'), Some(v)) => if v.as_int().unwrap_or(0) != 0 { "true" } else { "false" }.into(),
        (Some(b'C'), Some(v)) => char::from_u32(v.as_int().unwrap_or(0) as u32).map(String::from).unwrap_or_default(),
        (Some(b'['), Some(Value::Array(h))) => {
            // char[] / byte[]. `chars_array_to_string` reads the backing `Vec<i32>`
            // DIRECTLY; the old form boxed the entire array into a `Vec<Value>` first, so
            // a writer flushing an 8192-char buffer allocated 8192 `Value`s to build one
            // string. Same answer for both backings — the helper's fallback arm IS the old
            // `array_values` + `filter_map(char::from_u32)`, so a byte >= 0x80 (which
            // sign-extends out of the code-point range) is still dropped exactly as before.
            vm.heap.chars_array_to_string(*h)
        }
        (_, Some(v)) => vm.value_to_string(v).unwrap_or_else(|_| v.display()),
        (_, None) => String::new(),
    }
}

/// Stringify a StringBuilder argument per its declared type (Z→bool, C→char,
/// another buffer/Path/etc. via its text, else display).
pub(crate) fn sb_arg(vm: &Vm, descriptor: &str, arg: Option<&Value>) -> String {
    let t = descriptor.as_bytes().get(1).copied();
    match (t, arg) {
        (Some(b'Z'), Some(v)) => if v.as_int().unwrap_or(0) != 0 { "true" } else { "false" }.into(),
        (Some(b'C'), Some(v)) => char::from_u32(v.as_int().unwrap_or(0) as u32).map(String::from).unwrap_or_default(),
        (_, Some(Value::Ref(h))) => {
            for f in ["$sb", "$str", "$path", "$name"] {
                match vm.heap.get_field(*h, f) {
                    Value::Str(s) => return s.to_string(),
                    Value::Array(a) if f == "$sb" => return vm.heap.chars_array_to_string(a),
                    _ => {}
                }
            }
            "null".into()
        }
        (_, Some(v)) => v.display(),
        (_, None) => String::new(),
    }
}
