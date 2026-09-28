//! File channels, random access files and the output streams.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nio::*;

/// java.nio.channels.FileChannel over a real file. Used by the world-storage
/// lock (session.lock): open → write magic → tryLock. Backed by `$path` and a
/// channel position `$cpos`; reads/writes go through ByteBuffers.
pub(crate) fn filechannel_method(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    use std::io::{Read, Seek, SeekFrom, Write};
    if name == "open" {
        let p = path_str(vm, args.first())?;
        let _ = fs::OpenOptions::new().create(true).write(true).read(true).open(&p);
        let h = vm.heap.alloc("java/nio/channels/FileChannel");
        vm.heap.put_field_syn(h, crate::heap::Syn::Path, Value::Str(p.into()));
        vm.heap.put_field(h, "$cpos", Value::Long(0));
        return Some(Some(Value::Ref(h)));
    }
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Some(Some(Value::Null)) };
    let path = match vm.heap.get_field_syn(recv, crate::heap::Syn::Path) { Value::Str(s) => s.to_string(), _ => return Some(Some(Value::Null)) };
    let cur_pos = vm_get_long(vm, recv, "$cpos").max(0) as u64;
    let ret = match name {
        "write" => {
            // write(ByteBuffer [, long position]) — drain the buffer's remaining bytes.
            let (bytes, src) = match args.get(1) { Some(Value::Ref(b)) => (buffer_remaining(vm, *b), *b), _ => return Some(Some(Value::Int(0))) };
            let at = args.get(2).and_then(Value::as_long).map(|v| v as u64).unwrap_or(cur_pos);
            if let Ok(mut f) = fs::OpenOptions::new().create(true).write(true).open(&path) {
                let _ = f.seek(SeekFrom::Start(at));
                let _ = f.write_all(&bytes);
            }
            let n = bytes.len();
            if args.get(2).is_none() { vm.heap.put_field(recv, "$cpos", Value::Long((at + n as u64) as i64)); }
            buffer_advance_all(vm, src);
            Value::Int(n as i32)
        }
        "read" => {
            let dst = match args.get(1) { Some(Value::Ref(b)) => *b, _ => return Some(Some(Value::Int(-1))) };
            let at = args.get(2).and_then(Value::as_long).map(|v| v as u64).unwrap_or(cur_pos);
            let mut data = Vec::new();
            if let Ok(mut f) = fs::File::open(&path) {
                let _ = f.seek(SeekFrom::Start(at));
                // ⭐**A 4 KiB WINDOW COST THE WHOLE REST OF THE FILE.** `read_to_end` after the
                // seek pulled every byte from `at` to EOF into `data`, and `buffer_fill` then
                // kept the first `room` of them and dropped the rest. A region file is read
                // sector by sector through exactly this call, so a file of size S read in
                // windows of R cost **O(S²/R) of read and memcpy for O(S) of payload** — the
                // classic quadratic drain, and it grows with the world, not with the request.
                //
                // `room` is what `buffer_fill` will accept, asked of the destination buffer
                // itself so the two cannot disagree. The `+ 1` is load-bearing: the EOF test
                // below is `data.is_empty()`, which must keep meaning "there was nothing at
                // `at`" and not "the buffer was already full" — a full buffer at a readable
                // offset returns 0 on a FileChannel, never -1. One extra byte answers that
                // without reading the tail; `buffer_fill` caps at `room` and discards it.
                if fast_nioshape() {
                    let room = buffer_room(vm, dst);
                    let _ = f.take(room as u64 + 1).read_to_end(&mut data);
                } else {
                    let _ = f.read_to_end(&mut data);
                }
            }
            let n = buffer_fill(vm, dst, &data);
            if crate::env_flag!("RAVA_TRACE_CHAN") {
                eprintln!("[CHAN] read {path} at={at} n={n} pos_arg={}", args.get(2).is_some());
            }
            if args.get(2).is_none() { vm.heap.put_field(recv, "$cpos", Value::Long((at + n as u64) as i64)); }
            if n == 0 && data.is_empty() { Value::Int(-1) } else { Value::Int(n as i32) }
        }
        "position" if args.len() == 1 => Value::Long(cur_pos as i64),
        "position" => { vm.heap.put_field(recv, "$cpos", Value::Long(args.get(1).and_then(Value::as_long).unwrap_or(0))); return Some(Some(Value::Ref(recv))); }
        "size" => Value::Long(fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as i64),
        "truncate" => {
            let n = args.get(1).and_then(Value::as_long).unwrap_or(0).max(0) as u64;
            if let Ok(f) = fs::OpenOptions::new().write(true).open(&path) { let _ = f.set_len(n); }
            return Some(Some(Value::Ref(recv)));
        }
        "force" | "flush" => return Some(None),
        "isOpen" => Value::Int(1),
        "tryLock" | "lock" => {
            let l = vm.heap.alloc("java/nio/channels/FileLock");
            vm.heap.put_field(l, "$valid", Value::Int(1));
            vm.heap.put_field(l, "$channel", Value::Ref(recv));
            Value::Ref(l)
        }
        "close" => return Some(None),
        _ => return None,
    };
    Some(Some(ret))
}

/// java.io.RandomAccessFile over a real file — positioned read/write keyed on a
/// `$path` + a file pointer `$pos`. Minecraft's RegionFile (the .mca anvil chunk
/// store) drives chunk save/load through this: seek to a sector, read/write the
/// chunk header and payload. Each op reopens the file (no persistent handle), so
/// the position is tracked in `$pos`, not by the OS file descriptor.
pub(crate) fn raf_method(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Some(Some(Value::Null)) };
    if name == "<init>" {
        // RandomAccessFile(File|String file, String mode)
        let p = path_str(vm, args.get(1))?;
        let mode = match args.get(2) { Some(Value::Str(s)) => s.to_string(), _ => "r".into() };
        // A write mode must create the file if absent (without truncating it).
        if mode.contains('w') { let _ = fs::OpenOptions::new().create(true).append(true).open(&p); }
        vm.heap.put_field_syn(recv, crate::heap::Syn::Path, Value::Str(p.into()));
        vm.heap.put_field_syn(recv, crate::heap::Syn::Pos, Value::Long(0));
        return Some(None);
    }
    let path = match vm.heap.get_field_syn(recv, crate::heap::Syn::Path) { Value::Str(s) => s.to_string(), _ => return Some(Some(Value::Null)) };
    let pos = vm_get_long(vm, recv, "$pos").max(0) as u64;
    let ret = match name {
        "length" => Value::Long(fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as i64),
        "getFilePointer" => Value::Long(pos as i64),
        "getChannel" => {
            let h = vm.heap.alloc("java/nio/channels/FileChannel");
            vm.heap.put_field_syn(h, crate::heap::Syn::Path, Value::Str(path.into()));
            vm.heap.put_field(h, "$cpos", Value::Long(pos as i64));
            Value::Ref(h)
        }
        "seek" => {
            let at = args.get(1).and_then(Value::as_long).unwrap_or(0).max(0);
            vm.heap.put_field_syn(recv, crate::heap::Syn::Pos, Value::Long(at));
            return Some(None);
        }
        "setLength" => {
            let n = args.get(1).and_then(Value::as_long).unwrap_or(0).max(0) as u64;
            if let Ok(f) = fs::OpenOptions::new().create(true).write(true).open(&path) { let _ = f.set_len(n); }
            return Some(None);
        }
        // read(byte[] [,off,len]) — fill the array from $pos, return count (-1 EOF).
        "read" | "readFully" if matches!(args.get(1), Some(Value::Array(_))) => {
            let arr = match args.get(1) { Some(Value::Array(a)) => *a, _ => return Some(Some(Value::Int(-1))) };
            let cap = vm.heap.array_len(arr).unwrap_or(0);
            let off = args.get(2).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let len = args.get(3).and_then(Value::as_int).map(|v| v.max(0) as usize).unwrap_or(cap.saturating_sub(off));
            let mut data = vec![0u8; len];
            let n = raf_read_at(&path, pos, &mut data);
            for i in 0..n { vm.heap.array_set(arr, off + i, Value::Int(data[i] as i32)); }
            vm.heap.put_field_syn(recv, crate::heap::Syn::Pos, Value::Long((pos + n as u64) as i64));
            // readFully returns void; read returns the count.
            if name == "readFully" { return Some(None); }
            if n == 0 { Value::Int(-1) } else { Value::Int(n as i32) }
        }
        // read() (no arg): next unsigned byte (0..255) or -1 at EOF.
        "read" => {
            let mut b = [0u8; 1];
            let n = raf_read_at(&path, pos, &mut b);
            if n == 0 { Value::Int(-1) } else {
                vm.heap.put_field_syn(recv, crate::heap::Syn::Pos, Value::Long((pos + 1) as i64));
                Value::Int(b[0] as i32)
            }
        }
        "readByte" | "readUnsignedByte" | "readBoolean" => {
            let mut b = [0u8; 1];
            raf_read_at(&path, pos, &mut b);
            vm.heap.put_field_syn(recv, crate::heap::Syn::Pos, Value::Long((pos + 1) as i64));
            // readByte is signed; readUnsignedByte/readBoolean are 0..255 / 0|1.
            if name == "readByte" { Value::Int(b[0] as i8 as i32) } else { Value::Int(b[0] as i32) }
        }
        "readInt" | "readUnsignedShort" | "readShort" | "readChar" => {
            let width = if name == "readInt" { 4 } else { 2 };
            let mut b = [0u8; 4];
            raf_read_at(&path, pos, &mut b[..width]);
            vm.heap.put_field_syn(recv, crate::heap::Syn::Pos, Value::Long((pos + width as u64) as i64));
            let v = match name {
                "readInt" => i32::from_be_bytes(b),
                "readShort" => (i16::from_be_bytes([b[0], b[1]])) as i32,
                _ => u16::from_be_bytes([b[0], b[1]]) as i32, // unsigned short / char
            };
            Value::Int(v)
        }
        // write(int): low 8 bits as one byte.
        "write" | "writeByte" if !matches!(args.get(1), Some(Value::Array(_))) => {
            let b = [args.get(1).and_then(Value::as_int).unwrap_or(0) as u8];
            raf_write_at(&path, pos, &b);
            vm.heap.put_field_syn(recv, crate::heap::Syn::Pos, Value::Long((pos + 1) as i64));
            return Some(None);
        }
        // write(byte[] [,off,len]).
        "write" => {
            let arr = match args.get(1) { Some(Value::Array(a)) => *a, _ => return Some(None) };
            let all = vm.heap.array_bytes(arr).unwrap_or_default();
            let off = args.get(2).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let len = args.get(3).and_then(Value::as_int).map(|v| v.max(0) as usize).unwrap_or(all.len().saturating_sub(off));
            let slice = all.get(off..(off + len).min(all.len())).unwrap_or(&[]);
            raf_write_at(&path, pos, slice);
            vm.heap.put_field_syn(recv, crate::heap::Syn::Pos, Value::Long((pos + slice.len() as u64) as i64));
            return Some(None);
        }
        "writeInt" | "writeShort" | "writeChar" => {
            let v = args.get(1).and_then(Value::as_int).unwrap_or(0);
            let bytes: &[u8] = &v.to_be_bytes();
            let slice = if name == "writeInt" { bytes } else { &bytes[2..] }; // low 2 bytes, BE
            raf_write_at(&path, pos, slice);
            vm.heap.put_field_syn(recv, crate::heap::Syn::Pos, Value::Long((pos + slice.len() as u64) as i64));
            return Some(None);
        }
        "close" | "flush" | "sync" => return Some(None),
        _ => return None,
    };
    Some(Some(ret))
}

/// Write `bytes` to an OutputStream object, generically:
/// * `DigestOutputStream` → feed its `MessageDigest` (`$mdbuf`), then forward to
///   the wrapped stream (`$out`);
/// * a file-backed stream (`$path`, from `Files.newOutputStream`/FileOutputStream)
///   → append to disk;
/// * `ByteArrayOutputStream` → append to its `$buf`;
/// * any other sink (the null OutputStream) → discard.
pub(crate) fn os_write(vm: &mut Vm, out: Option<&Value>, bytes: &[u8]) {
    let Some(Value::Ref(h)) = out else { return };
    let h = *h;
    if vm.heap.class_of(h) == Some("java/security/DigestOutputStream") {
        if let Value::Ref(md) = vm.heap.get_field(h, "$digest") {
            let mut acc = match vm.heap.get_field(md, "$mdbuf") {
                Value::Array(a) => vm.heap.array_bytes(a).unwrap_or_default(),
                _ => Vec::new(),
            };
            acc.extend_from_slice(bytes);
            let a = vm.heap.alloc_bytes(&acc);
            vm.heap.put_field(md, "$mdbuf", Value::Array(a));
        }
        let wrapped = vm.heap.get_field(h, "$out");
        if !matches!(wrapped, Value::Null) {
            os_write(vm, Some(&wrapped), bytes);
        }
        return;
    }
    if let Value::Str(path) = vm.heap.get_field_syn(h, crate::heap::Syn::Path) {
        append_file(&path, bytes);
        return;
    }
    // ByteArrayOutputStream and the zip output streams buffer into $buf — the
    // zip ones compress that buffer on close (see bin_out_method).
    let cls = vm.heap.class_of(h).unwrap_or("");
    if matches!(cls, "java/io/ByteArrayOutputStream"
        | "java/util/zip/GZIPOutputStream" | "java/util/zip/DeflaterOutputStream")
    {
        // AMORTISED, and it has to be: this used to read the whole accumulation out,
        // extend it and allocate a fresh array on EVERY write, which is quadratic in
        // the number of writes. `NbtIo.write` emits a few bytes per tag, so serialising
        // one chunk is thousands of writes over a growing buffer — the save-path gate
        // (`bench/NbtSave.java`) did not just run slowly, it exhausted the heap.
        //
        // `$buf` is now a CAPACITY with the live length in `$blen`: append in place
        // while it fits, and double it when it does not. Growing allocates a NEW array
        // rather than extending the old one, because `array_write_bytes` may not move a
        // backing buffer the parallel fork holds pointers into.
        let cap = match vm.heap.get_field(h, "$buf") {
            Value::Array(a) => vm.heap.array_len(a).unwrap_or(0),
            _ => 0,
        };
        let len = vm.heap.get_field(h, "$blen").as_int().unwrap_or(0).max(0) as usize;
        let needed = len + bytes.len();
        if needed > cap {
            let mut grown = vec![0u8; needed.next_power_of_two().max(64)];
            if len > 0 {
                if let Value::Array(a) = vm.heap.get_field(h, "$buf") {
                    let old = vm.heap.array_bytes(a).unwrap_or_default();
                    let n = len.min(old.len());
                    grown[..n].copy_from_slice(&old[..n]);
                }
            }
            let a = vm.heap.alloc_bytes(&grown);
            vm.heap.put_field(h, "$buf", Value::Array(a));
        }
        if let Value::Array(a) = vm.heap.get_field(h, "$buf") {
            vm.heap.array_write_bytes(a, len, bytes);
        }
        vm.heap.put_field(h, "$blen", Value::Int(needed as i32));
        return;
    }
    // netty ByteBufOutputStream (what PacketDataSerializer.a(NBTTagCompound)
    // wraps the packet ByteBuf in): write the bytes into its `buffer` ByteBuf via
    // ByteBuf.writeBytes. Without this, NBT field names — emitted through this
    // stream's writeUTF -> inner DataOutputStream -> here — were silently dropped,
    // so the client got malformed NBT ("Loading NBT data" DecoderException).
    if cls == "io/netty/buffer/ByteBufOutputStream" {
        // Invoke its own write([B) bytecode, which goes straight to the wrapped
        // ByteBuf (this.buffer.writeBytes). Reading the `buffer` field natively
        // returned null, but the bytecode path is the same one writeByte uses.
        let arr = vm.heap.alloc_bytes(bytes);
        // A region abort lands in the Err arm and is dropped ON PURPOSE here: this function
        // returns nothing, and an aborting worker's writes are discarded with the rest of its
        // region. Every other swallow of this error was a defect; this one is the exception, so
        // it says so.
        if let Ok(Some(decl)) = vm.resolve_method("io/netty/buffer/ByteBufOutputStream", "write", "([B)V") {
            let _ = vm.invoke(decl, "write", "([B)V", vec![Value::Ref(h), Value::Array(arr)]);
        }
        return;
    }
    // A filter stream (DataOutputStream / BufferedOutputStream) forwards to the
    // stream it wraps — via our `$out`, or the bytecode `out` field a real
    // FilterOutputStream sets in its constructor.
    let wrapped = match vm.heap.get_field(h, "$out") {
        Value::Ref(o) => Some(Value::Ref(o)),
        _ => match vm.heap.get_field(h, "out") { Value::Ref(o) => Some(Value::Ref(o)), _ => None },
    };
    if let Some(out) = wrapped {
        os_write(vm, Some(&out), bytes);
        return;
    }
    // else: discarding sink (OutputStream.nullOutputStream()).
}

/// THE ONE READER of an accumulating stream's buffer.
///
/// `$buf` is a CAPACITY, not a length: `os_write` grows it geometrically and records
/// how much of it is live in `$blen`, so every reader has to truncate. Routing all of
/// them through here is what keeps that invariant in one place — `size`, the zip
/// compress-on-close and `toByteArray` all read the same bytes by construction.
/// A buffer with no `$blen` (an INPUT stream's, set whole by its constructor) reads
/// back entire, which is what those callers have always seen.
pub(crate) fn out_buffer(vm: &mut Vm, h: u32) -> Vec<u8> {
    let Value::Array(a) = vm.heap.get_field(h, "$buf") else { return Vec::new() };
    let mut bytes = vm.heap.array_bytes(a).unwrap_or_default();
    if let Some(n) = vm.heap.get_field(h, "$blen").as_int() {
        let n = (n.max(0) as usize).min(bytes.len());
        bytes.truncate(n);
    }
    bytes
}

fn buffered_bytes(vm: &mut Vm, recv: Option<&Value>) -> Vec<u8> {
    match recv {
        Some(Value::Ref(h)) => out_buffer(vm, *h),
        _ => Vec::new(),
    }
}

pub(crate) fn out_method(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    match name {
        // static OutputStream.nullOutputStream() — a discarding sink object.
        "nullOutputStream" => return Some(Some(Value::Ref(vm.heap.alloc("java/io/OutputStream")))),
        "<init>" => {
            let Some(Value::Ref(recv)) = args.first().cloned() else { return Some(None) };
            // FileOutputStream(File|String[, append]): bind the target path and,
            // unless opened in append mode, TRUNCATE it now (java truncates on
            // construction). Without this each `new FileOutputStream(f)` appended
            // to the previous contents — e.g. world `session.lock` grew by one
            // 8-byte timestamp per boot, so checkSession read a stale first long
            // and aborted with "accessed from another location".
            let target_path = match args.get(1) {
                Some(Value::Str(s)) => Some(s.to_string()),
                Some(Value::Ref(r)) if vm.heap.class_of(*r) == Some("java/io/File") => {
                    match vm.heap.get_field_syn(*r, crate::heap::Syn::Path) { Value::Str(s) => Some(s.to_string()), _ => None }
                }
                _ => None,
            };
            if let Some(p) = target_path {
                let append = matches!(args.get(2), Some(Value::Int(1)));
                if append {
                    let _ = fs::OpenOptions::new().create(true).append(true).open(&p);
                } else {
                    let _ = fs::File::create(&p); // create + truncate
                }
                vm.heap.put_field_syn(recv, crate::heap::Syn::Path, Value::Str(p.into()));
                return Some(None);
            }
            // Wrapper streams: BufferedOutputStream/FilterOutputStream(out) and
            // DigestOutputStream(out, digest) — remember the inner stream (and the
            // digest) so writes pass through (and feed the digest).
            if let Some(o) = args.get(1).cloned() { vm.heap.put_field(recv, "$out", o); }
            if let Some(md) = args.get(2).cloned() { vm.heap.put_field(recv, "$digest", md); }
            return Some(None);
        }
        _ => {}
    }
    match name {
        "write" => {
            let bytes = match args.get(1) {
                Some(Value::Array(b)) => {
                    let all = vm.heap.array_bytes(*b).unwrap_or_default();
                    let off = args.get(2).and_then(Value::as_int).unwrap_or(0) as usize;
                    let len = args.get(3).and_then(Value::as_int).map(|v| v as usize)
                        .unwrap_or_else(|| all.len().saturating_sub(off));
                    all.into_iter().skip(off).take(len).collect::<Vec<u8>>()
                }
                Some(Value::Int(b)) => vec![*b as u8],
                _ => Vec::new(),
            };
            os_write(vm, args.first(), &bytes);
            Some(None)
        }
        "flush" | "close" => Some(None),
        // `ByteArrayOutputStream`'s readers. `os_write` above accumulates into `$buf`
        // for exactly these classes, but nothing ever read it back, so a guest that
        // serialised into one and then asked for the bytes hit an
        // UnsatisfiedLinkError — found by the save-path gate (`bench/NbtSave.java`),
        // where `NbtIo.write` into a `ByteArrayOutputStream` is the idiom.
        "toByteArray" => {
            let bytes = buffered_bytes(vm, args.first());
            // A COPY, because java's `toByteArray` returns a fresh array: handing
            // back the `$buf` handle itself would alias it, and the next `write`
            // would be visible through an array the guest already owns.
            let a = vm.heap.alloc_bytes(&bytes);
            Some(Some(Value::Array(a)))
        }
        "size" => {
            let n = buffered_bytes(vm, args.first()).len() as i32;
            Some(Some(Value::Int(n)))
        }
        "reset" => {
            // Keeps the capacity and drops the length, exactly as java's does.
            if let Some(Value::Ref(h)) = args.first() {
                vm.heap.put_field(*h, "$blen", Value::Int(0));
            }
            Some(None)
        }
        // `writeTo(OutputStream)` — drain this buffer into another stream through the
        // same sink the `write` arm uses, so a file/zip/netty target behaves the same.
        "writeTo" => {
            let bytes = buffered_bytes(vm, args.first());
            os_write(vm, args.get(1), &bytes);
            Some(None)
        }
        _ => None,
    }
}

/// java.io.DataOutputStream (big-endian binary writes) and the zip output
/// streams (GZIP/Deflater). DataOutputStream forwards each value's bytes to its
/// wrapped stream via os_write; a GZIP/Deflater stream buffers everything and
/// compresses on close. Used by NbtIo.writeCompressed for level.dat.
pub(crate) fn bin_out_method(vm: &mut Vm, owner: &str, name: &str, args: &[Value]) -> Option<Option<Value>> {
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Some(None) };
    let is_zip = owner != "java/io/DataOutputStream";
    match name {
        "<init>" => {
            if let Some(o) = args.get(1).cloned() { vm.heap.put_field(recv, "$out", o); }
            Some(None)
        }
        "flush" => Some(None),
        "close" => { close_stream(vm, recv); Some(None) }
        "size" => {
            // Through `out_buffer`, not `array_len`: `$buf` is a capacity now.
            let n = out_buffer(vm, recv).len();
            Some(Some(Value::Int(n as i32)))
        }
        _ if is_zip => {
            // GZIP/Deflater write([B,off,len) / write(int): buffer it.
            let bytes = write_bytes_arg(vm, args);
            os_write(vm, Some(&Value::Ref(recv)), &bytes);
            Some(None)
        }
        // DataOutputStream big-endian writes.
        _ => {
            let bytes: Vec<u8> = match name {
                "writeByte" | "writeBoolean" => vec![args.get(1).and_then(Value::as_int).unwrap_or(0) as u8],
                "writeShort" | "writeChar" => (args.get(1).and_then(Value::as_int).unwrap_or(0) as u16).to_be_bytes().to_vec(),
                "writeInt" => (args.get(1).and_then(Value::as_int).unwrap_or(0)).to_be_bytes().to_vec(),
                "writeLong" => (args.get(1).and_then(Value::as_long).unwrap_or(0)).to_be_bytes().to_vec(),
                "writeFloat" => {
                    let f = match args.get(1) { Some(Value::Float(x)) => *x, Some(v) => v.as_int().unwrap_or(0) as f32, None => 0.0 };
                    f.to_bits().to_be_bytes().to_vec()
                }
                "writeDouble" => {
                    let d = match args.get(1) { Some(Value::Double(x)) => *x, Some(Value::Float(x)) => *x as f64, Some(v) => v.as_long().unwrap_or(0) as f64, None => 0.0 };
                    d.to_bits().to_be_bytes().to_vec()
                }
                "writeUTF" => {
                    let s = match args.get(1) {
                        Some(v) => vm.value_to_string(v).unwrap_or_default(),
                        None => String::new(),
                    };
                    let b = s.into_bytes();
                    let mut out = (b.len() as u16).to_be_bytes().to_vec();
                    out.extend_from_slice(&b);
                    out
                }
                "writeBytes" => match args.get(1) { Some(Value::Str(s)) => s.bytes().collect(), _ => Vec::new() },
                "writeChars" => match args.get(1) { Some(Value::Str(s)) => s.encode_utf16().flat_map(|c| c.to_be_bytes()).collect(), _ => Vec::new() },
                "write" => write_bytes_arg(vm, args),
                _ => return Some(None),
            };
            os_write(vm, Some(&Value::Ref(recv)), &bytes);
            Some(None)
        }
    }
}
