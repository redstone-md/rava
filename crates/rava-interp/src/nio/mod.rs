//! Generic `java.nio.file` (Path/Files), `java.io.File`, and stream IO. Real
//! disk operations — exactly what a JVM does — keyed only on real JDK class and
//! method names (never on a specific jar). Descriptor-tolerant because the JDK
//! exposes many varargs-option overloads (LinkOption[]/OpenOption[]/...).





use std::fs;
use std::path::{Path as FsPath, PathBuf};

use crate::value::Value;
use crate::Vm;

/// ✅**DEFAULT-ON** — opt OUT with `RAVA_NO_FAST_NIO=1`.
///
/// The file/resource API round: every change under this switch is a COMPLEXITY change, not a
/// constant-factor one. Each answers a question about ONE level, ONE window or ONE element
/// without materialising the whole tree, the whole array or the whole descendant list first.
/// The switch is on the BLOCK because no single one of them is worth a boot A/B on its own —
/// the same reason `emit_more_disabled` is a block switch — and this consults that flag too, so
/// one `RAVA_NO_EMIT_MORE` boot prices the whole rewrite line.
///
/// No table, no state: a single `OnceLock<bool>`, 1 byte plus its init word.
#[inline]
pub(crate) fn fast_nio() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_NIO").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// ✅**DEFAULT-ON** — opt OUT with `RAVA_NO_FAST_NIOSHAPE=1` (or the tree-wide
/// `RAVA_NO_EMIT_MORE=1`, which this consults, so one bisect boot prices the whole line).
///
/// The nio SHAPE round. Every change under this switch removes a cost that was in the SHAPE of
/// the code rather than in the work it had to do — a whole list materialised to answer a
/// one-element question, a whole file read to fill a 4 KiB window, a whole string exploded into
/// a `Vec<char>` to take a slice of it, a byte moved through a boxed `Value` at a time. None of
/// them is a memo, a mirror or a side table; each is a smaller amount of the same work.
///
/// Separate from `fast_nio` on purpose: that block is already shipped and already measured, and
/// mixing the two would make neither bisectable.
///
/// No table, no state: one `OnceLock<bool>`, read once per file OPERATION (never per byte and
/// never per opcode — every loop below hoists it out).
#[inline]
pub(crate) fn fast_nioshape() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_NO_FAST_NIOSHAPE").is_none() && !crate::flags::emit_more_disabled()
    })
}

/// Build a `java/nio/file/Path` object holding a path string.
fn make_path(vm: &mut Vm, s: &str) -> Value {
    let h = vm.heap.alloc("java/nio/file/Path");
    vm.heap.put_field_syn(h, crate::heap::Syn::Path, Value::Str(s.into()));
    Value::Ref(h)
}

/// Extract a filesystem path string from a Path/File object or a String.
pub(crate) fn path_str(vm: &Vm, v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Str(s)) => Some(s.to_string()),
        // ArenaString (a compiled inline concat's product — e.g. RegionFileStorage's
        // "r." + x + "." + z + ".mca"): read the inline UTF-8. Before this arm, an
        // arena filename fell to None and Path.resolve's unwrap_or_default() built
        // the BARE FOLDER path — every rx=0 region file open after the concat
        // compiled read the directory instead of the .mca (chunks silently absent).
        #[cfg(feature = "next-gen-gc")]
        Some(Value::Obj(p)) if crate::nextgen::is_arena_string(*p) => {
            Some(String::from_utf8_lossy(crate::nextgen::arena_str_bytes(*p)).into_owned())
        }
        Some(Value::Ref(h)) => match vm.heap.get_field_syn(*h, crate::heap::Syn::Path) {
            Value::Str(s) => Some(s.to_string()),
            // A heap java.lang.String object (not a Path): its text lives in $str.
            _ => match vm.heap.get_field_syn(*h, crate::heap::Syn::Str) {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            },
        },
        _ => None,
    }
}

fn paths_get(vm: &mut Vm, args: &[Value]) -> Value {
    // Paths.get(URI)/Path.of(URI) on a jar-tagged URI yields a jar-internal Path.
    if let Some(entry) = jar_entry(vm, args.first()) {
        return make_jar_path(vm, &entry);
    }
    let mut buf = PathBuf::from(match path_str(vm, args.first()) {
        Some(s) => s,
        None => return Value::Null,
    });
    // Trailing String[] varargs.
    if let Some(Value::Array(h)) = args.get(1) {
        let len = vm.heap.array_len(*h).unwrap_or(0);
        for i in 0..len {
            if let Some(Value::Str(s)) = vm.heap.array_get(*h, i) {
                if !s.is_empty() {
                    buf.push(s.as_ref());
                }
            }
        }
    }
    make_path(vm, &buf.to_string_lossy())
}

/// A jar-internal Path carries `$jarres` (a slash-separated entry path inside
/// the loaded jars, e.g. "data/minecraft/recipe/x.json") instead of `$path`.
/// This is how the vanilla data pack is read — MC resolves a `.mcassetsroot`
/// resource URL to a Path, walks its parent, and reads each file.
pub(crate) fn make_jar_path(vm: &mut Vm, entry: &str) -> Value {
    let h = vm.heap.alloc("java/nio/file/Path");
    vm.heap.put_field(h, "$jarres", Value::Str(entry.into()));
    Value::Ref(h)
}

pub(crate) fn jar_entry(vm: &Vm, v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Ref(h)) => match vm.heap.get_field(*h, "$jarres") {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Is `entry` a directory inside the jar tree (has children, or no own bytes)?
fn jar_is_dir(vm: &mut Vm, entry: &str) -> bool {
    vm.loader.resource_is_dir(entry)
}

/// Direct children entry-paths of a jar directory `entry` (one level deep).
///
/// ⭐**ONE LEVEL COST THE WHOLE SUBTREE, AT EVERY LEVEL OF THE DESCENT.** This built the ENTIRE
/// descendant list through `list_resources` — every entry name under the prefix, each one a
/// fresh `String` — kept the first path segment of each and threw the rest away. `jar_walk`
/// calls it once per directory node on the way down, and `Files.list` is called per directory
/// per namespace per registry by the pack scan, so a subtree of depth d was enumerated d times:
/// **O(descendants × depth) in entries under the start**, per walk.
///
/// `list_resource_children` binary-searches the merged sorted index and then SKIPS PAST each
/// child's whole subtree with a partition point ⇒ **O(children × log N)** — the cost is the
/// answer's size, not the tree's. Same set, same order (both key a `BTreeSet` on `{prefix}{seg}`).
fn jar_children(vm: &mut Vm, entry: &str) -> Vec<String> {
    let prefix = format!("{}/", entry.trim_end_matches('/'));
    if fast_nio() {
        return vm.loader.list_resource_children(&prefix);
    }
    let plen = prefix.len();
    let mut seen = std::collections::BTreeSet::new();
    for e in vm.loader.list_resources(&prefix) {
        if let Some(rest) = e.get(plen..) {
            let first = rest.trim_end_matches('/').split('/').next().unwrap_or("");
            if !first.is_empty() { seen.insert(format!("{prefix}{first}")); }
        }
    }
    seen.into_iter().collect()
}

/// Collect every path under `dir` (inclusive) into `out`, depth-first.
///
/// ⭐**THE DIRECTORY ENTRY ALREADY KNOWS WHETHER IT IS A DIRECTORY.** The old form recursed into
/// `collect_tree(e.path())`, whose first act was `dir.is_dir()` — a fresh `GetFileAttributes`
/// syscall per entry, on top of the `read_dir` that had just handed us the entry's own type. That
/// is **one extra stat per file in the tree** (a world save's `region/` is thousands). `file_type`
/// comes out of the directory record on both Windows and Linux and costs nothing; only a SYMLINK
/// needs the follow, and that keeps the old semantics exactly (`Path::is_dir` follows links).
/// Push order is unchanged: self, then each child's subtree in `read_dir` order.
fn collect_tree(dir: &FsPath, out: &mut Vec<String>) {
    out.push(dir.to_string_lossy().into_owned());
    if !fast_nio() {
        if dir.is_dir() {
            if let Ok(rd) = fs::read_dir(dir) {
                for e in rd.flatten() { collect_tree(&e.path(), out); }
            }
        }
        return;
    }
    collect_children(dir, out);
}

/// `collect_tree`'s recursion, entered only for a path already known to be a directory.
///
/// ⭐**THE PATH WAS BUILT ONCE AND COPIED TWICE.** `e.path()` already allocates a `PathBuf`
/// holding the full child path — parent bytes plus separator plus name — and
/// `to_string_lossy().into_owned()` then allocated a SECOND buffer and memcpy'd the whole thing
/// into it, only for the `PathBuf` to be dropped one line later for every FILE (the dominant
/// population; only directories are used again, for the recursion). `OsString::into_string`
/// hands the `PathBuf`'s own buffer over as the `String` with no copy at all: on Windows an
/// `OsString` is WTF-8, so valid UTF-8 — which every path this walk produces is — moves without
/// touching a byte, and the invalid case falls back to exactly the old lossy conversion of the
/// same `OsString`, so the produced text is identical either way.
///
/// Push ORDER is unchanged (self, then that child's subtree, in `read_dir` order): the directory
/// arm still pushes before it recurses.
fn collect_children(dir: &FsPath, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    // Hoisted out of the entry loop: one gate read per DIRECTORY, not per entry.
    let fast = fast_nioshape();
    for e in rd.flatten() {
        let p = e.path();
        let is_dir = match e.file_type() {
            // A symlink's own type says nothing about its target; only this case pays the stat.
            Ok(t) if t.is_symlink() => p.is_dir(),
            Ok(t) => t.is_dir(),
            Err(_) => p.is_dir(),
        };
        if is_dir {
            // A directory is needed again by the recursion, so it keeps both.
            out.push(p.to_string_lossy().into_owned());
            collect_children(&p, out);
        } else if fast {
            out.push(match p.into_os_string().into_string() {
                Ok(s) => s,
                Err(os) => os.to_string_lossy().into_owned(),
            });
        } else {
            out.push(p.to_string_lossy().into_owned());
        }
    }
}

/// Invoke one FileVisitor callback and decode its FileVisitResult ordinal.
fn call_visitor(vm: &mut Vm, visitor: u32, method: &str, path: Value, second: Value) -> Result<i32, crate::VmError> {
    // postVisitDirectory/visitFileFailed take an IOException second arg; the
    // other two take BasicFileAttributes.
    let desc = if matches!(method, "postVisitDirectory" | "visitFileFailed") {
        "(Ljava/lang/Object;Ljava/io/IOException;)Ljava/nio/file/FileVisitResult;"
    } else {
        "(Ljava/lang/Object;Ljava/nio/file/attribute/BasicFileAttributes;)Ljava/nio/file/FileVisitResult;"
    };
    // ⭐**A `String` COPIED PER VISITED FILE FOR A NAME NOBODY KEEPS.** `class_of` and
    // `resolve_method` both take `&Vm`, so the class name never needed to be owned at all — the
    // borrow ends with the statement and `decl` is an owned `Arc`. That is one heap allocation
    // and one memcpy of the visitor's class name removed per entry of every `walkFileTree`
    // (grows with entries in the tree). `resolve_method` itself is already memoised in `tlcache`.
    let decl = if fast_nio() {
        let cls = vm.heap.class_of(visitor).unwrap_or("");
        vm.resolve_method(cls, method, desc)?
    } else {
        let cls = vm.heap.class_of(visitor).unwrap_or("").to_string();
        vm.resolve_method(&cls, method, desc)?
    };
    let res = match decl {
        Some(decl) => vm.invoke(decl, method, desc, vec![Value::Ref(visitor), path, second])?,
        None => return Ok(0), // no override → CONTINUE
    };
    // Decode the FileVisitResult enum ordinal ($enumOrd), default CONTINUE.
    match res {
        Some(Value::Ref(h)) => Ok(match vm.heap.get_field(h, "$enumOrd") { Value::Int(o) => o, _ => 0 }),
        _ => Ok(0),
    }
}

fn filelock_method(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Some(Some(Value::Null)) };
    let ret = match name {
        "isValid" => Value::Int(match vm.heap.get_field(recv, "$valid") { Value::Int(v) => v, _ => 0 }),
        "isShared" => Value::Int(0),
        "release" | "close" => { vm.heap.put_field(recv, "$valid", Value::Int(0)); return Some(None); }
        "channel" | "acquiredBy" => vm.heap.get_field(recv, "$channel"),
        "position" | "size" => Value::Long(0),
        _ => return None,
    };
    Some(Some(ret))
}

/// Positioned read into `buf` from `path` at `pos`; returns bytes read (short at
/// EOF). Reopens the file each call (RAF ops are infrequent: chunk header sectors).
fn raf_read_at(path: &str, pos: u64, buf: &mut [u8]) -> usize {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = fs::File::open(path) else { return 0 };
    if f.seek(SeekFrom::Start(pos)).is_err() { return 0; }
    let mut total = 0;
    while total < buf.len() {
        match f.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => break,
        }
    }
    total
}

/// Positioned write of `bytes` to `path` at `pos`, extending the file as needed.
fn raf_write_at(path: &str, pos: u64, bytes: &[u8]) {
    use std::io::{Seek, SeekFrom, Write};
    if let Ok(mut f) = fs::OpenOptions::new().create(true).write(true).open(path) {
        let _ = f.seek(SeekFrom::Start(pos));
        let _ = f.write_all(bytes);
    }
}

fn vm_get_long(vm: &Vm, h: u32, f: &str) -> i64 {
    match vm.heap.get_field(h, f) { Value::Long(v) => v, Value::Int(v) => v as i64, _ => 0 }
}

/// Bytes remaining in a ByteBuffer (`$buf[$pos..$limit]`).
///
/// ⭐**A WINDOW OF THE ARRAY COST A COPY OF THE ARRAY.** `array_bytes` clones the WHOLE backing
/// `byte[]` and the slice was taken afterwards, so a 4-byte `readInt` off a region-file channel
/// copied the entire 4 KiB-plus buffer first: **O(capacity) per call for an O(limit-pos) answer**.
/// `array_read_bytes` slices in the heap and copies only the window. It declines a non-byte
/// array — then, and only then, the old whole-array widening path runs.
pub(crate) fn buffer_remaining(vm: &Vm, bb: u32) -> Vec<u8> {
    let buf = match vm.heap.get_field(bb, "$buf") { Value::Array(a) => a, _ => return Vec::new() };
    let pos = match vm.heap.get_field_syn(bb, crate::heap::Syn::Pos) { Value::Int(v) => v.max(0) as usize, _ => 0 };
    let lim = match vm.heap.get_field(bb, "$limit") { Value::Int(v) => v.max(0) as usize, _ => 0 };
    if fast_nio() {
        if let Some(b) = vm.heap.array_read_bytes(buf, pos, lim.saturating_sub(pos)) {
            return b;
        }
    }
    let all = vm.heap.array_bytes(buf).unwrap_or_default();
    all.get(pos..lim.min(all.len())).map(|s| s.to_vec()).unwrap_or_default()
}

/// Advance a ByteBuffer's position to its limit (all remaining consumed).
fn buffer_advance_all(vm: &mut Vm, bb: u32) {
    let lim = match vm.heap.get_field(bb, "$limit") { Value::Int(v) => v, _ => 0 };
    vm.heap.put_field_syn(bb, crate::heap::Syn::Pos, Value::Int(lim));
}

/// How many bytes `buffer_fill` would accept into `bb` — `min($limit, capacity) − $pos`.
///
/// Exposed because a channel `read` that does not know this has to guess, and the only safe
/// guess is "everything" (see `channels.rs`: `read_to_end` after a seek pulled the WHOLE REST OF
/// THE FILE in to fill a 4 KiB window). Answering it is three field reads; it allocates nothing
/// and it is the same arithmetic `buffer_fill` performs below, so the two cannot drift.
pub(crate) fn buffer_room(vm: &Vm, bb: u32) -> usize {
    let buf = match vm.heap.get_field(bb, "$buf") { Value::Array(a) => a, _ => return 0 };
    let pos = match vm.heap.get_field_syn(bb, crate::heap::Syn::Pos) { Value::Int(v) => v.max(0) as usize, _ => 0 };
    let lim = match vm.heap.get_field(bb, "$limit") { Value::Int(v) => v.max(0) as usize, _ => 0 };
    let cap = vm.heap.array_len(buf).unwrap_or(0);
    lim.min(cap).saturating_sub(pos)
}

/// Copy `data` into a ByteBuffer at its position, advancing it. Returns count.
pub(crate) fn buffer_fill(vm: &mut Vm, bb: u32, data: &[u8]) -> usize {
    let buf = match vm.heap.get_field(bb, "$buf") { Value::Array(a) => a, _ => return 0 };
    let pos = match vm.heap.get_field_syn(bb, crate::heap::Syn::Pos) { Value::Int(v) => v.max(0) as usize, _ => 0 };
    let lim = match vm.heap.get_field(bb, "$limit") { Value::Int(v) => v.max(0) as usize, _ => 0 };
    let cap = vm.heap.array_len(buf).unwrap_or(0);
    let room = lim.min(cap).saturating_sub(pos);
    let n = data.len().min(room);
    // ⭐**A BULK COPY WAS BEING PAID FOR ONE `Value` ROUND TRIP PER BYTE.** `array_set` boxes each
    // byte into a `Value::Int`, re-resolves the array handle and re-checks bounds — n times, where
    // n is a whole channel read (kilobytes per `read()`). `array_write_bytes` resolves once and
    // `copy_from_slice`s. It refuses a non-byte array or a range that does not fit; `pos + n` is
    // bounded by `cap` above, so a refusal here can only mean a widened array — fall through.
    if !fast_nio() || !vm.heap.array_write_bytes(buf, pos, &data[..n]) {
        for i in 0..n { vm.heap.array_set(buf, pos + i, Value::Int(data[i] as i32)); }
    }
    vm.heap.put_field_syn(bb, crate::heap::Syn::Pos, Value::Int((pos + n) as i32));
    n
}

/// fs mtime as epoch millis (0 if unavailable — std SystemTime is not blocked
/// for fs metadata, only the argless Date::now is).
fn mtime_millis(m: &fs::Metadata) -> i64 {
    m.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `Files.copy`: (InputStream -> Path) or (Path -> Path).
fn files_copy(vm: &mut Vm, args: &[Value]) -> Value {
    match args.first() {
        // InputStream source: drain remaining bytes to the target file.
        Some(Value::Ref(h)) if matches!(vm.heap.get_field(*h, "$buf"), Value::Array(_)) => {
            let bytes = drain_stream(vm, *h);
            let n = bytes.len();
            if let Some(target) = path_str(vm, args.get(1)) {
                let _ = fs::write(&target, &bytes);
            }
            Value::Long(n as i64)
        }
        // Path source.
        _ => {
            if let (Some(src), Some(dst)) = (path_str(vm, args.first()), path_str(vm, args.get(1))) {
                let _ = fs::copy(&src, &dst);
            }
            args.get(1).cloned().unwrap_or(Value::Null)
        }
    }
}

/// Drain all remaining bytes from a byte-buffer InputStream, advancing `$pos`.
///
/// ⭐**TWO FULL COPIES OF THE STREAM TO HAND BACK ITS TAIL.** `array_bytes` cloned the whole
/// array and `all[pos..].to_vec()` cloned the tail out of that clone: **2 × O(capacity)** for an
/// O(capacity − pos) answer, on every `Files.copy(InputStream, …)` and `transferTo` — i.e. once
/// per file the reload streams. One slicing read replaces both.
fn drain_stream(vm: &mut Vm, stream: u32) -> Vec<u8> {
    let buf = match vm.heap.get_field(stream, "$buf") {
        Value::Array(b) => b,
        _ => return Vec::new(),
    };
    let pos = vm.heap.get_field_syn(stream, crate::heap::Syn::Pos).as_int().unwrap_or(0) as usize;
    if fast_nio() {
        let len = vm.heap.array_len(buf).unwrap_or(0);
        if let Some(tail) = vm.heap.array_read_bytes(buf, pos, len.saturating_sub(pos)) {
            vm.heap.put_field_syn(stream, crate::heap::Syn::Pos, Value::Int(len as i32));
            return tail;
        }
    }
    let all = vm.heap.array_bytes(buf).unwrap_or_default();
    vm.heap.put_field_syn(stream, crate::heap::Syn::Pos, Value::Int(all.len() as i32));
    if pos < all.len() {
        all[pos..].to_vec()
    } else {
        Vec::new()
    }
}

fn transfer_to(vm: &mut Vm, args: &[Value]) -> Value {
    let src = match args.first() {
        Some(Value::Ref(h)) => *h,
        _ => return Value::Long(0),
    };
    let bytes = drain_stream(vm, src);
    let n = bytes.len();
    os_write(vm, args.get(1), &bytes);
    Value::Long(n as i64)
}

/// Extract the bytes from a write([B,off,len) / write([B) / write(int) call.
///
/// ⭐**`write(buf, 0, 40)` COPIED THE WHOLE 8 KiB BUFFER AND THEN WALKED IT BYTE BY BYTE.** The
/// old form cloned the entire array, then `skip(off).take(len)` iterated it one element at a time
/// to build the result — **O(capacity) per write for an O(len) payload**, and Java's stream idiom
/// is a big reusable buffer written in small pieces, so the waste is the ratio of the two. Now
/// one slice copy of exactly `len`.
fn write_bytes_arg(vm: &Vm, args: &[Value]) -> Vec<u8> {
    match args.get(1) {
        Some(Value::Array(b)) => {
            let off = args.get(2).and_then(Value::as_int).unwrap_or(0) as usize;
            if fast_nio() {
                let cap = vm.heap.array_len(*b).unwrap_or(0);
                let len = args.get(3).and_then(Value::as_int).map(|v| v as usize)
                    .unwrap_or_else(|| cap.saturating_sub(off));
                // A negative off/len reaches here sign-extended into an enormous `usize` (the old
                // `skip`/`take` absorbed that silently). Refuse rather than let `off + len` wrap
                // inside the slice read, and let the unchanged path below answer it exactly as
                // it always did.
                if off <= cap && len <= cap - off {
                    if let Some(bytes) = vm.heap.array_read_bytes(*b, off, len) {
                        return bytes;
                    }
                }
            }
            let all = vm.heap.array_bytes(*b).unwrap_or_default();
            let len = args.get(3).and_then(Value::as_int).map(|v| v as usize)
                .unwrap_or_else(|| all.len().saturating_sub(off));
            all.into_iter().skip(off).take(len).collect()
        }
        Some(Value::Int(b)) => vec![*b as u8],
        _ => Vec::new(),
    }
}

/// Close a stream chain: a zip stream compresses its buffer to the wrapped
/// stream first, then close propagates down so a file sink is written.
fn close_stream(vm: &mut Vm, h: u32) {
    // A `String` allocated to compare against two constants, once per link of every closed stream
    // chain. `class_of` borrows `vm.heap`; the borrow dies with the `matches!`, so nothing needs
    // to own the name. (Kept behind the block switch only so one boot prices the whole round.)
    let zipping = if fast_nio() {
        matches!(vm.heap.class_of(h).unwrap_or(""),
            "java/util/zip/GZIPOutputStream" | "java/util/zip/DeflaterOutputStream")
    } else {
        let cls = vm.heap.class_of(h).unwrap_or("").to_string();
        matches!(cls.as_str(), "java/util/zip/GZIPOutputStream" | "java/util/zip/DeflaterOutputStream")
    };
    if zipping {
        // `out_buffer`, because `$buf` is a capacity and `$blen` is the live length;
        // reading the array raw would compress the zero padding as well.
        let raw = out_buffer(vm, h);
        let compressed = gzip_bytes(&raw);
        let out = vm.heap.get_field(h, "$out");
        os_write(vm, Some(&out), &compressed);
    }
    if let Value::Ref(oh) = vm.heap.get_field(h, "$out") {
        close_stream(vm, oh);
    }
}

fn gzip_bytes(raw: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = enc.write_all(raw);
    enc.finish().unwrap_or_default()
}

/// gunzip (GZIPInputStream) — or raw zlib/deflate (InflaterInputStream).
pub(crate) fn gunzip_bytes(raw: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    if flate2::read::GzDecoder::new(raw).read_to_end(&mut out).is_ok() && !out.is_empty() {
        return out;
    }
    out.clear();
    let _ = flate2::read::ZlibDecoder::new(raw).read_to_end(&mut out);
    out
}

/// Build a `java/net/URI` or `java/net/URL` carrying both the raw fs path
/// (`$path`, used by the classpath loader) and a `file:` URL string (`$str`).
pub(crate) fn make_uri(vm: &mut Vm, class: &str, fs_path: &str) -> Value {
    // Two allocations and two passes (a `replace` copy, then a `format!` copy of that) for one
    // string, on a path taken once per `getResource` — which the classpath scan runs thousands of
    // times. One exact-size buffer, one pass. Byte-for-byte the same result.
    let url = if fast_nio() {
        let mut u = String::with_capacity(fs_path.len() + 8);
        u.push_str("file:///");
        for c in fs_path.chars() { u.push(if c == '\\' { '/' } else { c }); }
        u
    } else {
        format!("file:///{}", fs_path.replace('\\', "/"))
    };
    let h = vm.heap.alloc(class);
    vm.heap.put_field_syn(h, crate::heap::Syn::Path, Value::Str(fs_path.into()));
    vm.heap.put_field_syn(h, crate::heap::Syn::Str, Value::Str(url.into()));
    Value::Ref(h)
}

fn append_file(path: &str, bytes: &[u8]) {
    use std::io::Write;
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(bytes);
    }
}
mod dispatch;
pub(crate) use dispatch::*;
mod jar;
pub(crate) use jar::*;
pub(crate) mod files;
pub(crate) use files::*;
mod channels;
pub(crate) use channels::*;
