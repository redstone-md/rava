//! Reading a whole stream or one line from it.
//!
//! The whole-stream path uses synthetic buffers when available and guest bytecode otherwise.

use crate::natives::*;

pub(crate) fn stream_read_all(vm: &mut Vm, recv: Option<&Value>) -> Result<Value, VmError> {
    // Pushback bytes come first; NbtIo may peek and unread the gzip magic before this call.
    let fast = fast_natsurf_on();
    let mut out = Vec::new();
    if let Some(Value::Ref(h)) = recv {
        if let Value::Array(a) = vm.heap.get_field(*h, "$pushback") {
            if !fast || vm.heap.array_len(a).unwrap_or(0) != 0 {
                let mut pb = vm.heap.array_values(a).unwrap_or_default();
                while let Some(b) = pb.pop() {
                    out.push(b.as_int().unwrap_or(0) as u8);
                }
                let arr = vm.heap.alloc_values("B", Vec::new());
                vm.heap.put_field(*h, "$pushback", Value::Array(arr));
            }
        }
    }
    if let Some((sh, buf, pos)) = stream_state(vm, recv) {
        let len = vm.heap.array_len(buf).unwrap_or(0);
        let tail = if fast { vm.heap.array_read_bytes(buf, pos, len.saturating_sub(pos)) } else { None };
        match tail {
            Some(t) if out.is_empty() => out = t,
            Some(t) => out.extend_from_slice(&t),
            None => {
                let all = vm.heap.array_bytes(buf).unwrap_or_default();
                if pos < all.len() {
                    out.extend_from_slice(&all[pos..]);
                }
            }
        }
        vm.heap.put_field_syn(sh, crate::heap::Syn::Pos, Value::Int(len as i32));
    } else if let Some(stream) = underlying_stream(vm, recv) {
        let found = drain_guest_stream(vm, stream, &mut out)?;
        if !found && out.is_empty() {
            return Ok(Value::Null);
        }
    } else if out.is_empty() {
        return Ok(Value::Null);
    }
    let arr = vm.heap.alloc_bytes(&out);
    Ok(Value::Array(arr))
}

/// Drain an unmodeled stream through its resolved guest bytecode method.
fn drain_guest_stream(vm: &mut Vm, stream: u32, out: &mut Vec<u8>) -> Result<bool, VmError> {
    let chunk_len = 8192;
    let chunk = vm.heap.alloc_bytes(&vec![0; chunk_len]);
    let mut found = false;
    loop {
        let Some(n) = guest_stream_read_into(vm, stream, chunk, 0, chunk_len)? else {
            return Ok(found);
        };
        found = true;
        if n <= 0 {
            break;
        }
        if let Some(bytes) = vm.heap.array_read_bytes(chunk, 0, n as usize) {
            out.extend_from_slice(&bytes);
        }
    }
    Ok(found)
}

/// Read one line (up to `\n`, trailing `\r` stripped) from the underlying
/// stream; returns null at end of input.
pub(crate) fn read_line(vm: &mut Vm, recv: Option<&Value>) -> Result<Value, VmError> {
    let Some(stream) = underlying_stream(vm, recv) else { return Ok(Value::Null) };
    // BULK, default on (`RAVA_NO_FAST_NATSURF` opts out). The loop below moves ONE byte per
    // `$pushback` probe by name + `underlying_stream` chain walk (two field reads per hop) +
    // `$buf` and `$pos` reads + a `Value` round trip through `array_get` + a `$pos` write —
    // call it six-to-eight heap operations to advance one character.
    // WAS: O(bytes in the line) HEAP OPERATIONS per readLine. IS: O(1) heap operations plus
    // a byte scan for `\n` over the line's own bytes. `DataInputStream` (`RAVA_NO_BULK_READ`,
    // 3.1x on the save gate) and `Reader.read(char[])` (`RAVA_NO_BULK_CHARS`) already had
    // this treatment; readLine is the same loop, missed.
    if fast_natsurf_on() {
        if let Some(v) = read_line_bulk(vm, stream) {
            return Ok(v);
        }
    }
    let stream_val = Some(Value::Ref(stream));
    let mut line = String::new();
    let mut any = false;
    loop {
        let b = stream_read_one(vm, stream_val.as_ref())?;
        if b < 0 {
            break;
        }
        any = true;
        if b == b'\n' as i32 {
            break;
        }
        line.push(b as u8 as char);
    }
    if !any {
        return Ok(Value::Null);
    }
    if line.ends_with('\r') {
        line.pop();
    }
    Ok(Value::Str(line.into()))
}

/// The bulk form of `read_line`: find the `\n` inside the backing byte array instead of
/// crossing the stream plumbing once per byte.
///
/// REFUSES (returns `None`, having mutated NOTHING) unless it can prove the whole
/// precondition of the per-byte loop it replaces:
///   * `$pushback` is empty — a pushed-back byte is served BEFORE the buffer and this path
///     cannot see it (NbtIo peeks + unreads the gzip magic through exactly that field);
///   * the stream layer really holds a `$buf` that really is a `byte[]` (`array_read_bytes`
///     declines anything else).
/// On refusal the caller runs the original loop, so the fast path is invisible.
///
/// The tail is walked in 256-byte windows rather than sliced whole: reading the entire
/// remainder per line would make reading a file line-by-line QUADRATIC in the file size
/// (`bytes remaining` copied once per line). 256 bytes is one transient `Vec` that covers a
/// typical line in a single window and never grows with the file.
fn read_line_bulk(vm: &mut Vm, stream: u32) -> Option<Value> {
    const WINDOW: usize = 256;
    if let Value::Array(a) = vm.heap.get_field(stream, "$pushback") {
        if vm.heap.array_len(a).unwrap_or(0) != 0 {
            return None;
        }
    }
    let Value::Array(buf) = vm.heap.get_field(stream, "$buf") else { return None };
    let len = vm.heap.array_len(buf)?;
    let start = vm.heap.get_field_syn(stream, crate::heap::Syn::Pos).as_int().unwrap_or(0).max(0) as usize;
    // Probe the representation BEFORE committing: a non-byte array must reach the slow loop
    // with the stream untouched.
    let first = vm.heap.array_read_bytes(buf, start, WINDOW.min(len.saturating_sub(start)))?;
    if start >= len {
        return Some(Value::Null); // end of input: the loop's `any == false`
    }
    let mut bytes: Vec<u8> = Vec::new();
    let mut pos = start;
    let mut window = first;
    loop {
        if window.is_empty() {
            break;
        }
        match window.iter().position(|&b| b == b'\n') {
            Some(i) => {
                bytes.extend_from_slice(&window[..i]);
                pos += i + 1; // the terminator is consumed but not returned
                break;
            }
            None => {
                pos += window.len();
                bytes.extend_from_slice(&window);
            }
        }
        if pos >= len {
            break;
        }
        window = vm.heap.array_read_bytes(buf, pos, WINDOW.min(len - pos))?;
    }
    if pos == start {
        return Some(Value::Null); // nothing consumed at all
    }
    vm.heap.put_field_syn(stream, crate::heap::Syn::Pos, Value::Int(pos as i32));
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    // The per-byte loop pushed `b as u8 as char`, i.e. latin-1, and this must render the
    // same string it did — NOT a UTF-8 decode.
    Some(Value::Str(bytes.iter().map(|&b| b as char).collect::<String>().into()))
}
