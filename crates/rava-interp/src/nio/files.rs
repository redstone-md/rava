//! Files, Path and the attribute views.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nio::*;

/// `Files.newInputStream` cost split (`RAVA_NIO_STAT`): path build, read, calls, misses.
pub(crate) static NIO_PATH_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static NIO_READ_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static NIO_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static NIO_MISSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn path_method(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    if let Some(entry) = jar_entry(vm, args.first()) {
        return jar_path_method(vm, &entry, name, args);
    }
    let p = path_str(vm, args.first())?;
    // ⭐**A `PathBuf` ALLOCATED FOR EVERY `Path` METHOD, USED BY FOUR OF THEM.** `PathBuf::from`
    // is a heap allocation plus a full copy of the path text, and it stood here unconditionally
    // — ahead of `toString`, `equals`, `hashCode`, `startsWith`, `getNameCount`, `iterator`,
    // `isAbsolute` and `compareTo`, none of which look at it. It is now built inside the four
    // arms that need it, and two of those turned out to need only a borrowed `&Path`
    // (`parent`/`file_name` never allocate), so the allocation survives in exactly two arms.
    let ret = match name {
        "toString" | "toRealPath" => Value::Str(p.clone().into()),
        // ⛔`normalize` IS NOT `canonicalize`. The JDK's is LEXICAL — it folds `.` and `..`
        // without touching the filesystem, so it works on a path that does not exist and
        // never resolves a symlink. Routing it here made it a NO-OP for every path not on
        // disk (the common case: building a path before creating it), which is why
        // `a/./b/../c` came back unchanged. The lexical one is with the path arithmetic.
        "toRealPathResolved" => {
            let pb = PathBuf::from(&p);
            let abs = fs::canonicalize(&pb).unwrap_or(pb);
            make_path(vm, &abs.to_string_lossy())
        }
        // `parent`/`file_name` are slices of the borrowed path — no `PathBuf` needed at all.
        "getParent" => match FsPath::new(&p).parent() {
            Some(par) if !par.as_os_str().is_empty() => make_path(vm, &par.to_string_lossy()),
            _ => Value::Null,
        },
        "getFileName" => match FsPath::new(&p).file_name() {
            Some(f) => make_path(vm, &f.to_string_lossy()),
            None => Value::Null,
        },
        "resolve" | "resolveSibling" => {
            let other = path_str(vm, args.get(1)).unwrap_or_default();
            // Was: `pb.parent().map(to_path_buf).unwrap_or_else(|| pb.clone())` — a THIRD copy of
            // the path on top of the eager `pb`, to name a base that `join` immediately copies
            // again. `parent()` on a borrowed path is a slice, and `unwrap_or(base)` reproduces
            // the old fallback exactly (`parent()` is `None` only for a root or empty path; a
            // one-component path yields `Some("")`, which is what the old `map` arm kept too).
            let base = FsPath::new(&p);
            let base = if name == "resolveSibling" { base.parent().unwrap_or(base) } else { base };
            make_path(vm, &base.join(other).to_string_lossy())
        }
        "toFile" => {
            let h = vm.heap.alloc("java/io/File");
            vm.heap.put_field_syn(h, crate::heap::Syn::Path, Value::Str(p.clone().into()));
            Value::Ref(h)
        }
        "toUri" => make_uri(vm, "java/net/URI", &p),
        "toUrl" | "toURL" => make_uri(vm, "java/net/URL", &p),
        // The EMPTY path has one (empty) component in Java, not zero — `Paths.get("")`
        // reports `getNameCount() == 1`, and code looping over components relies on it.
        "getNameCount" => {
            let n = path_comps(&p).count();
            Value::Int(if n == 0 { 1 } else { n as i32 })
        }
        // A `Path` is `Iterable<Path>` over its components; without this every
        // `for (Path c : path)` ran zero times.
        "iterator" => {
            let comps: Vec<Value> = path_parts(&p).into_iter().map(|c| make_path(vm, &c)).collect();
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", comps);
            let it = vm.heap.alloc("java/util/Iterator");
            vm.heap.put_field_syn(it, crate::heap::Syn::Elems, Value::Array(arr));
            vm.heap.put_field_syn(it, crate::heap::Syn::Pos, Value::Int(0));
            Value::Ref(it)
        }
        // `getName(i)` — the i-th component, and the loop body of every hand-rolled path
        // walk. Absent entirely. Out of range answers null rather than the
        // `IllegalArgumentException` the JDK raises: this dispatcher has no throw
        // channel, and inventing one for an index no correct caller passes is not worth
        // the surface.
        "getName" => {
            let i = args.get(1).and_then(Value::as_int).unwrap_or(-1);
            // ONE component was answered by materialising ALL of them, each its own `String`.
            match usize::try_from(i).ok().and_then(|u| path_comps(&p).nth(u)) {
                Some(c) => make_path(vm, c),
                None => Value::Null,
            }
        }
        "subpath" => {
            let (a, b) = (
                args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize,
                args.get(2).and_then(Value::as_int).unwrap_or(0).max(0) as usize,
            );
            let parts = path_parts(&p);
            let slice = parts.get(a..b.min(parts.len())).unwrap_or(&[]).join("/");
            make_path(vm, &slice)
        }
        // `normalize` folds `.` away and `..` into its predecessor, LEXICALLY — no
        // filesystem is consulted, so a leading `..` survives, as the JDK leaves it.
        "normalize" => {
            let mut out: Vec<String> = Vec::new();
            for c in path_parts(&p) {
                match c.as_str() {
                    "." => {}
                    ".." if matches!(out.last().map(String::as_str), Some(x) if x != "..") => {
                        out.pop();
                    }
                    other => out.push(other.to_string()),
                }
            }
            make_path(vm, &out.join("/"))
        }
        // `relativize` — the path leading from this one to `other`: walk off the common
        // prefix, then one `..` per component left here.
        "relativize" => {
            let other = path_str(vm, args.get(1)).unwrap_or_default();
            let (a, b) = (path_parts(&p), path_parts(&other));
            let common = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
            let mut out: Vec<String> = vec!["..".to_string(); a.len() - common];
            out.extend_from_slice(&b[common..]);
            make_path(vm, &out.join("/"))
        }
        "isAbsolute" => Value::Int(FsPath::new(&p).is_absolute() as i32),
        "toAbsolutePath" => {
            let abs = std::env::current_dir()
                .map(|d| d.join(&p))
                .unwrap_or_else(|_| FsPath::new(&p).to_path_buf());
            make_path(vm, &abs.to_string_lossy())
        }
        "compareTo" => {
            let other = path_str(vm, args.get(1)).unwrap_or_default();
            Value::Int(match p.as_str().cmp(other.as_str()) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })
        }
        // BY COMPONENT, not by string prefix. `Paths.get("world/region").startsWith("wor")`
        // is FALSE in Java and was TRUE here, because the test was `str::starts_with` — so
        // any guard shaped as "is this path inside that directory" accepted a sibling
        // whose name merely shared a prefix.
        // ⭐**A YES/NO QUESTION BUILT TWO WHOLE COMPONENT LISTS TO ANSWER IT.** `path_parts`
        // allocates a `Vec` AND a fresh `String` per component, and this arm built one for each
        // side — for `world/datapacks/x/data/minecraft/tags/…` that is a dozen allocations to
        // decide a boolean the pack scan asks per file. The components are slices of the two
        // path strings; comparing the iterators directly is the same test with no heap at all.
        "startsWith" | "endsWith" => {
            let other = path_str(vm, args.get(1)).unwrap_or_default();
            let (na, nb) = (path_comps(&p).count(), path_comps(&other).count());
            let ok = if nb > na {
                false
            } else if name == "startsWith" {
                // `zip` stops at the shorter side, which is `other` — exactly `a[..b.len()] == b`.
                path_comps(&p).zip(path_comps(&other)).all(|(x, y)| x == y)
            } else {
                path_comps(&p).skip(na - nb).eq(path_comps(&other))
            };
            Value::Int(ok as i32)
        }
        // BY COMPONENT: `Paths.get("a/b")` and `Paths.get("a", "b")` are the same path,
        // and comparing the stored strings said otherwise as soon as the two were built
        // with different separators.
        "equals" => {
            let other = path_str(vm, args.get(1)).unwrap_or_default();
            // Same shape as `startsWith` above: two `Vec<String>`s built and dropped to decide
            // one bit. `Iterator::eq` is the identical comparison (length, then element-wise).
            Value::Int(path_comps(&p).eq(path_comps(&other)) as i32)
        }
        "hashCode" => Value::Int(p.bytes().fold(0i32, |a, b| a.wrapping_mul(31).wrapping_add(b as i32))),
        _ => return None,
    };
    Some(Some(ret))
}

/// A path's NAME COMPONENTS, the unit every `Path` method above is defined in terms
/// of. Splits on both separators because a path can arrive in either form, and drops
/// the empties a leading or doubled separator leaves behind.
fn path_parts(p: &str) -> Vec<String> {
    path_comps(p).map(str::to_string).collect()
}

/// The same components, BORROWED — the shape every predicate over a path actually needs.
///
/// `path_parts` exists because `normalize`/`relativize`/`subpath`/`iterator` genuinely build a
/// new list. The predicates (`equals`, `startsWith`, `endsWith`, `getNameCount`, `getName`) do
/// not: they consume the components once, in order, and throw them away — so the `Vec` and the
/// per-component `String` were pure overhead, one allocation per path segment per call, on the
/// arms the pack scan hits per file. `path_parts` is now defined in terms of this, so the two
/// can never disagree about what a component is.
fn path_comps<'a>(p: &'a str) -> impl Iterator<Item = &'a str> + Clone + 'a {
    p.split(['/', '\\']).filter(|c| !c.is_empty())
}

/// THE THROWING WRAPPER — the same shape `uuid_dispatch` and `bytebuffer_dispatch` grew.
/// `Files` raises for two ordinary mistakes and this dispatcher could only answer or
/// decline, so copying over an existing file SUCCEEDED (silently replacing it, which is
/// the opposite of what the JDK does without `REPLACE_EXISTING`) and deleting a missing
/// one reported success.
pub(crate) fn files_method(
    vm: &mut Vm,
    name: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, crate::vmframe::VmError> {
    if name == "copy" || name == "move" {
        // A target that already exists is an error unless REPLACE_EXISTING is among the
        // options, which arrive as the trailing varargs.
        let replace = args.iter().skip(3).any(|v| match v {
            Value::Ref(h) => matches!(vm.heap.get_field_syn(*h, crate::heap::Syn::Name), Value::Str(s) if s.as_ref() == "REPLACE_EXISTING"),
            _ => false,
        });
        if !replace {
            if let Some(t) = path_str(vm, args.get(1)) {
                if FsPath::new(&t).exists() {
                    let e = vm.make_exception("java/nio/file/FileAlreadyExistsException", &t);
                    return Err(crate::vmframe::VmError::Throw(e));
                }
            }
        }
    }
    if name == "delete" {
        if let Some(t) = path_str(vm, args.first()) {
            if !FsPath::new(&t).exists() {
                let e = vm.make_exception("java/nio/file/NoSuchFileException", &t);
                return Err(crate::vmframe::VmError::Throw(e));
            }
        }
    }
    Ok(files_method_inner(vm, name, args))
}

fn files_method_inner(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    // ⭐**TWO RAW `std::env::var` READS PER `Files.*` CALL, AND THEY WERE THE LEFT OPERAND.**
    // On Windows MSVC `std::env::var` is `RtlQueryEnvironmentVariable`: a UTF-16 encode of the
    // key into a fresh heap buffer, a linear walk of the WHOLE process environment block under
    // a lock, and — on the miss this always is — an `Err` built and thrown away. Short-circuit
    // could never save it, because it stood FIRST in both conjunctions, so the cheap four-way
    // name test ran only after the expensive one. Every `exists`, `size`, `readAllBytes` and
    // `newInputStream` the datapack reload asks — thousands per pack — paid both.
    //
    // Now the name test leads (a register compare that rejects every non-walk call outright)
    // and the flag itself is the tree's memoised `env_flag!`, which reads the environment once
    // per process. Same trace output, same condition.
    if matches!(name, "find" | "walk" | "list" | "newDirectoryStream") && crate::env_flag!("RAVA_TRACE_FIND") {
        match jar_entry(vm, args.first()) {
            Some(e) => eprintln!("[FIND] jar {name} entry={e}"),
            None => eprintln!("[FIND] disk {name} path={:?}", path_str(vm, args.first())),
        }
    }
    if let Some(entry) = jar_entry(vm, args.first()) {
        return jar_files_method(vm, &entry, name, args);
    }
    let exists = |vm: &Vm, b: bool| {
        Value::Int(path_str(vm, args.first()).map(|p| {
            let m = FsPath::new(&p);
            match name {
                "exists" => m.exists(),
                "notExists" => !m.exists(),
                "isDirectory" => m.is_dir(),
                _ => m.is_file(),
            }
        }).unwrap_or(b) as i32)
    };
    match name {
        "exists" | "isDirectory" | "isRegularFile" => Some(Some(exists(vm, false))),
        "notExists" => Some(Some(exists(vm, true))),
        // `createTempDirectory(prefix, attrs...)` / `createTempFile` — absent, and they
        // are how anything that needs scratch space starts. The name carries the prefix
        // and a counter, which is enough to be unique within a run and keeps the result
        // reproducible enough to reason about.
        "createTempDirectory" | "createTempFile" => {
            use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let prefix = match args.first() {
                Some(Value::Str(s)) => s.to_string(),
                _ => "rava".to_string(),
            };
            let suffix = match args.get(1) {
                Some(Value::Str(s)) if name == "createTempFile" => s.to_string(),
                _ => String::new(),
            };
            let n = SEQ.fetch_add(1, Relaxed);
            let pid = std::process::id();
            let dir = std::env::temp_dir().join(format!("{prefix}{pid}-{n}{suffix}"));
            let made = if name == "createTempDirectory" {
                fs::create_dir_all(&dir).is_ok()
            } else {
                dir.parent().map(|d| fs::create_dir_all(d).is_ok()).unwrap_or(true)
                    && fs::write(&dir, b"").is_ok()
            };
            if !made {
                return Some(Some(Value::Null));
            }
            Some(Some(make_path(vm, &dir.to_string_lossy())))
        }
        "createDirectories" | "createDirectory" => {
            if let Some(p) = path_str(vm, args.first()) {
                let _ = fs::create_dir_all(&p);
            }
            Some(Some(args.first().cloned().unwrap_or(Value::Null)))
        }
        "createFile" => {
            if let Some(p) = path_str(vm, args.first()) {
                let _ = fs::File::create(&p);
            }
            Some(Some(args.first().cloned().unwrap_or(Value::Null)))
        }
        // Files.copy(InputStream, Path, ...) returns a byte count; Files.copy(Path,
        // Path, ...) returns the destination Path. Dispatch both overloads below.
        "copy" => Some(Some(files_copy(vm, args))),
        // Files.move(src, dst, options...) — used by LevelStorageAccess to swap
        // the freshly written temp file onto level.dat atomically.
        "move" => {
            if let (Some(src), Some(dst)) = (path_str(vm, args.first()), path_str(vm, args.get(1))) {
                if fs::rename(&src, &dst).is_err() {
                    let _ = fs::copy(&src, &dst).and_then(|_| fs::remove_file(&src));
                }
            }
            Some(Some(args.get(1).cloned().unwrap_or(Value::Null)))
        }
        // Files.createTempFile(Path dir, String prefix, String suffix, attrs...) —
        // used by LevelStorageAccess.saveDataTag for an atomic level.dat write.
        "createTempFile" => {
            let dir = path_str(vm, args.first())
                .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
            let prefix = match args.get(1) { Some(Value::Str(s)) => s.to_string(), _ => "tmp".into() };
            let suffix = match args.get(2) { Some(Value::Str(s)) => s.to_string(), _ => ".tmp".into() };
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos()).unwrap_or(0);
            let path = format!("{}/{}{}{}", dir.trim_end_matches(['/', '\\']), prefix, nanos, suffix);
            let _ = fs::File::create(&path);
            Some(Some(make_path(vm, &path)))
        }
        "size" => {
            let n = path_str(vm, args.first()).and_then(|p| fs::metadata(&p).ok()).map(|m| m.len()).unwrap_or(0);
            Some(Some(Value::Long(n as i64)))
        }
        "readAllBytes" => {
            let bytes = path_str(vm, args.first()).and_then(|p| fs::read(&p).ok()).unwrap_or_default();
            Some(Some(Value::Array(vm.heap.alloc_bytes(&bytes))))
        }
        "newInputStream" => {
            // ⭐**92.7 us per call over 6,308 calls (0.58 s of the boot), with ZERO guest opcodes
            // and ZERO nested natives** — so every one of those microseconds is inside this arm.
            // Split it: how many reads FIND a file, how long the path build takes, how long the
            // read takes. A failed open on Windows is not cheap, and MC probes a lot of paths.
            // The four clock reads below are `QueryPerformanceCounter` calls and they ran
            // UNCONDITIONALLY, to feed counters that only `RAVA_NIO_STAT` ever prints. That is
            // instrumentation on the served path — the shape this round sweeps for — though the
            // honest price is well under a millisecond over the boot's 6,308 calls, so this is
            // hygiene, not a win. With the flag ON the timings are byte-for-byte what they were.
            let dbg = crate::env_flag!("RAVA_NIO_STAT");
            let t0 = dbg.then(std::time::Instant::now);
            let p = path_str(vm, args.first());
            let tp = t0.map(|t| t.elapsed().as_nanos() as u64).unwrap_or(0);
            let t1 = dbg.then(std::time::Instant::now);
            let read = p.as_ref().and_then(|p| fs::read(p).ok());
            let tr = t1.map(|t| t.elapsed().as_nanos() as u64).unwrap_or(0);
            if dbg {
                NIO_PATH_NS.fetch_add(tp, std::sync::atomic::Ordering::Relaxed);
                NIO_READ_NS.fetch_add(tr, std::sync::atomic::Ordering::Relaxed);
                NIO_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if read.is_none() {
                    NIO_MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            let bytes = read.unwrap_or_default();
            Some(Some(crate::natives::make_stream(vm, &bytes)))
        }
        "newBufferedReader" => {
            let bytes = path_str(vm, args.first()).and_then(|p| fs::read(&p).ok()).unwrap_or_default();
            let stream = crate::natives::make_stream(vm, &bytes);
            let r = vm.heap.alloc("java/io/BufferedReader");
            vm.heap.put_field(r, "$stream", stream);
            Some(Some(Value::Ref(r)))
        }
        "newOutputStream" => {
            if let Some(p) = path_str(vm, args.first()) {
                let _ = fs::File::create(&p); // truncate/create
                let h = vm.heap.alloc("java/io/OutputStream");
                vm.heap.put_field_syn(h, crate::heap::Syn::Path, Value::Str(p.into()));
                return Some(Some(Value::Ref(h)));
            }
            Some(Some(Value::Null))
        }
        // A char Writer that buffers into $sb and flushes to the file on
        // flush/close (truncating). Used to write server.properties/eula.txt.
        "newBufferedWriter" => {
            if let Some(p) = path_str(vm, args.first()) {
                let h = vm.heap.alloc("java/io/BufferedWriter");
                vm.heap.put_field_syn(h, crate::heap::Syn::Path, Value::Str(p.into()));
                return Some(Some(Value::Ref(h)));
            }
            Some(Some(Value::Null))
        }
        "write" => {
            if let (Some(p), Some(Value::Array(b))) = (path_str(vm, args.first()), args.get(1)) {
                let bytes = vm.heap.array_bytes(*b).unwrap_or_default();
                let _ = fs::write(&p, &bytes);
            }
            Some(Some(args.first().cloned().unwrap_or(Value::Null)))
        }
        // The STRING half of the file API — `writeString`, `readString` and
        // `readAllLines` — none of which existed, so the shortest way to touch a text
        // file was an UnsatisfiedLinkError. The charset argument is accepted and ignored:
        // this engine is UTF-8 throughout (`file.encoding` reports it), and a guest that
        // asks for UTF-8 explicitly — which is every caller in this jar — gets what it
        // asked for.
        "writeString" => {
            if let (Some(pth), Some(txt)) = (path_str(vm, args.first()), crate::natives::str_of(vm, args.get(1))) {
                let _ = fs::write(&pth, txt.as_bytes());
            }
            Some(Some(args.first().cloned().unwrap_or(Value::Null)))
        }
        "readString" => {
            let Some(pth) = path_str(vm, args.first()) else { return Some(Some(Value::Null)) };
            match fs::read_to_string(&pth) {
                Ok(t) => Some(Some(Value::Str(t.into()))),
                Err(_) => Some(Some(Value::Null)),
            }
        }
        "readAllLines" => {
            let Some(pth) = path_str(vm, args.first()) else { return Some(Some(Value::Null)) };
            let text = fs::read_to_string(&pth).unwrap_or_default();
            // `readAllLines` drops the trailing newline's empty line, as `BufferedReader`
            // does — a file ending in a newline has as many lines as it has newlines.
            let mut lines: Vec<&str> = text.split('\n').collect();
            if matches!(lines.last(), Some(&"")) {
                lines.pop();
            }
            let vals: Vec<Value> = lines
                .into_iter()
                .map(|l| Value::Str(l.trim_end_matches('\r').into()))
                .collect();
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", vals);
            let h = vm.heap.alloc("java/util/ArrayList");
            vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
            Some(Some(Value::Ref(h)))
        }
        // readAttributes(path, BasicFileAttributes.class, ...) → a snapshot
        // object over the real fs metadata. (Missing path already threw
        // NoSuchFileException upstream in natives.rs.)
        "readAttributes" => {
            let m = path_str(vm, args.first()).and_then(|p| fs::metadata(&p).ok());
            let h = vm.heap.alloc("java/nio/file/attribute/BasicFileAttributes");
            if let Some(m) = m {
                vm.heap.put_field_syn(h, crate::heap::Syn::Size, Value::Long(m.len() as i64));
                vm.heap.put_field(h, "$dir", Value::Int(m.is_dir() as i32));
                vm.heap.put_field(h, "$millis", Value::Long(mtime_millis(&m)));
            }
            Some(Some(Value::Ref(h)))
        }
        "getLastModifiedTime" => {
            let ms = path_str(vm, args.first()).and_then(|p| fs::metadata(&p).ok())
                .map(|m| mtime_millis(&m)).unwrap_or(0);
            let h = vm.heap.alloc("java/nio/file/attribute/FileTime");
            vm.heap.put_field(h, "$millis", Value::Long(ms));
            Some(Some(Value::Ref(h)))
        }
        "isSameFile" => {
            let a = path_str(vm, args.first());
            let b = path_str(vm, args.get(1));
            Some(Some(Value::Int((a.is_some() && a == b) as i32)))
        }
        "walkFileTree" => {
            // walkFileTree(start, [opts, maxDepth,] visitor). The visitor is the
            // last Ref arg; the start path is the first.
            let start = path_str(vm, args.first());
            let visitor = args.iter().rev().find_map(|a| match a { Value::Ref(h) => Some(*h), _ => None });
            if let (Some(start), Some(visitor)) = (start, visitor) {
                let _ = walk_file_tree(vm, &start, visitor);
            }
            Some(Some(args.first().cloned().unwrap_or(Value::Null)))
        }
        "list" | "newDirectoryStream" => {
            // Direct children as a Stream<Path> / DirectoryStream.
            let mut kids = Vec::new();
            if let Some(p) = path_str(vm, args.first()) {
                if let Ok(rd) = fs::read_dir(&p) {
                    for e in rd.flatten() { kids.push(make_path(vm, &e.path().to_string_lossy())); }
                }
            }
            Some(Some(crate::stream::make_stream(vm, kids)))
        }
        "walk" | "find" => {
            // Recursive Stream<Path> including the start.
            let mut out = Vec::new();
            if let Some(p) = path_str(vm, args.first()) {
                collect_tree(&FsPath::new(&p), &mut out);
            }
            let paths: Vec<Value> = out.iter().map(|s| make_path(vm, s)).collect();
            Some(Some(crate::stream::make_stream(vm, paths)))
        }
        "getFileStore" => Some(Some(Value::Ref(vm.heap.alloc("java/nio/file/FileStore")))),
        "deleteIfExists" => {
            let ok = path_str(vm, args.first()).map(|p| fs::remove_file(&p).is_ok()).unwrap_or(false);
            Some(Some(Value::Int(ok as i32)))
        }
        "delete" => {
            if let Some(p) = path_str(vm, args.first()) {
                let _ = fs::remove_file(&p);
            }
            Some(None)
        }
        _ => None,
    }
}

/// Files.walkFileTree over a real directory: invoke the FileVisitor's
/// preVisitDirectory/visitFile/postVisitDirectory callbacks (real bytecode,
/// e.g. a SimpleFileVisitor subclass) for each entry. Returns true to keep
/// walking, false on TERMINATE. FileVisitResult ordinals: CONTINUE=0,
/// TERMINATE=1, SKIP_SUBTREE=2, SKIP_SIBLINGS=3.
pub(crate) fn walk_file_tree(vm: &mut Vm, path: &str, visitor: u32) -> Result<bool, crate::VmError> {
    let fp = FsPath::new(path);
    let md = match fs::metadata(fp) { Ok(m) => m, Err(_) => return Ok(true) };
    let mk_attrs = |vm: &mut Vm, m: &fs::Metadata| -> Value {
        let h = vm.heap.alloc("java/nio/file/attribute/BasicFileAttributes");
        vm.heap.put_field_syn(h, crate::heap::Syn::Size, Value::Long(m.len() as i64));
        vm.heap.put_field(h, "$dir", Value::Int(m.is_dir() as i32));
        vm.heap.put_field(h, "$millis", Value::Long(mtime_millis(m)));
        Value::Ref(h)
    };
    if md.is_dir() {
        let pathv = make_path(vm, path);
        let attrs = mk_attrs(vm, &md);
        let r = call_visitor(vm, visitor, "preVisitDirectory", pathv, attrs)?;
        if r == 1 { return Ok(false); } // TERMINATE
        if r != 2 {
            // CONTINUE → descend (SKIP_SUBTREE=2 would skip children).
            if let Ok(rd) = fs::read_dir(fp) {
                let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
                entries.sort();
                for child in entries {
                    if !walk_file_tree(vm, &child.to_string_lossy(), visitor)? { return Ok(false); }
                }
            }
        }
        let pathv = make_path(vm, path);
        let r = call_visitor(vm, visitor, "postVisitDirectory", pathv, Value::Null)?;
        if r == 1 { return Ok(false); }
    } else {
        let pathv = make_path(vm, path);
        let attrs = mk_attrs(vm, &md);
        let r = call_visitor(vm, visitor, "visitFile", pathv, attrs)?;
        if r == 1 { return Ok(false); }
    }
    Ok(true)
}

/// BasicFileAttributes / FileTime accessors over the `$size`/`$dir`/`$millis`
/// snapshot captured by Files.readAttributes/getLastModifiedTime.
pub(crate) fn fileattr_method(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    let recv = match args.first() { Some(Value::Ref(h)) => *h, _ => return Some(Some(Value::Null)) };
    let long = |vm: &Vm, f: &str| match vm.heap.get_field(recv, f) { Value::Long(v) => v, Value::Int(v) => v as i64, _ => 0 };
    let ret = match name {
        "size" => Value::Long(long(vm, "$size")),
        "isDirectory" => Value::Int((long(vm, "$dir") != 0) as i32),
        "isRegularFile" => Value::Int((long(vm, "$dir") == 0) as i32),
        "isSymbolicLink" | "isOther" => Value::Int(0),
        // FileTime
        "toMillis" => Value::Long(long(vm, "$millis")),
        "toInstant" => {
            let h = vm.heap.alloc("java/time/Instant");
            vm.heap.put_field(h, "$millis", Value::Long(long(vm, "$millis")));
            Value::Ref(h)
        }
        "compareTo" => {
            let other = match args.get(1) { Some(Value::Ref(o)) => match vm.heap.get_field(*o, "$millis") { Value::Long(v) => v, _ => 0 }, _ => 0 };
            let a = long(vm, "$millis");
            Value::Int((a > other) as i32 - (a < other) as i32)
        }
        // Attributes that return a FileTime — reuse the same $millis snapshot.
        "lastModifiedTime" | "creationTime" | "lastAccessTime" => {
            let h = vm.heap.alloc("java/nio/file/attribute/FileTime");
            vm.heap.put_field(h, "$millis", Value::Long(long(vm, "$millis")));
            Value::Ref(h)
        }
        "fileKey" => Value::Null,
        "toString" => Value::Str(long(vm, "$millis").to_string().into()),
        _ => return None,
    };
    Some(Some(ret))
}

pub(crate) fn file_method(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Option<Value>> {
    // Static: createTempFile(prefix, suffix[, dir]).
    if name == "createTempFile" {
        let prefix = match args.first() { Some(Value::Str(s)) => s.to_string(), _ => "tmp".into() };
        let suffix = match args.get(1) { Some(Value::Str(s)) => s.to_string(), _ => ".tmp".into() };
        let dir = path_str(vm, args.get(2)).unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = format!("{}/{}{}{}", dir.trim_end_matches(['/', '\\']), prefix, nanos, suffix);
        let _ = fs::File::create(&path);
        let h = vm.heap.alloc("java/io/File");
        vm.heap.put_field_syn(h, crate::heap::Syn::Path, Value::Str(path.into()));
        return Some(Some(Value::Ref(h)));
    }
    // new File(String) | new File(parent, child) | new File(URI)
    if name == "<init>" {
        if let Some(Value::Ref(r)) = args.first() {
            let path = match (args.get(1), args.get(2)) {
                (Some(parent), Some(_)) => {
                    let pp = path_str(vm, Some(parent)).unwrap_or_default();
                    let child = path_str(vm, args.get(2)).unwrap_or_default();
                    if pp.is_empty() { child } else { format!("{}/{}", pp.trim_end_matches(['/', '\\']), child) }
                }
                (Some(one), None) => path_str(vm, Some(one)).unwrap_or_default(),
                _ => String::new(),
            };
            vm.heap.put_field_syn(*r, crate::heap::Syn::Path, Value::Str(path.into()));
        }
        return Some(None);
    }
    let p = path_str(vm, args.first())?;
    // ⭐**AN OWNED `PathBuf` WHERE NOTHING EVER OWNED IT.** Every `java.io.File` method entered
    // through here allocated a `PathBuf` and copied the path text into it, and not one arm below
    // keeps it: `exists`/`is_dir`/`parent`/`metadata`/`read_dir`/`rename`/`canonicalize` all take
    // `AsRef<Path>`, which `&Path` satisfies, and `parent()`/`file_name()` return slices. A
    // borrowed `Path` is a fat pointer at the same address as `p`'s bytes — no allocation, no
    // copy — and every call site below is unchanged because `&&Path: AsRef<Path>`. The
    // population is every `File.exists`/`isDirectory`/`getName`/`length` MC's world storage and
    // the classpath scan ask, which is per candidate path, not per file found.
    let pb = FsPath::new(&p);
    let ret = match name {
        "toString" | "getPath" | "getAbsolutePath" => Value::Str(p.clone().into()),
        "toURI" => make_uri(vm, "java/net/URI", &p),
        "toURL" => make_uri(vm, "java/net/URL", &p),
        "toPath" => make_path(vm, &p),
        "exists" => Value::Int(pb.exists() as i32),
        "isDirectory" => Value::Int(pb.is_dir() as i32),
        "isFile" => Value::Int(pb.is_file() as i32),
        "mkdirs" | "mkdir" => Value::Int(fs::create_dir_all(&pb).is_ok() as i32),
        "getName" => Value::Str(pb.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default().into()),
        "length" => Value::Long(fs::metadata(&pb).map(|m| m.len()).unwrap_or(0) as i64),
        "delete" | "deleteOnExit" => Value::Int(fs::remove_file(&pb).is_ok() as i32),
        "canRead" | "canWrite" | "canExecute" | "setReadable" | "setWritable" | "setExecutable" | "setLastModified" => Value::Int(1),
        "isAbsolute" => Value::Int(pb.is_absolute() as i32),
        "createNewFile" => Value::Int(fs::OpenOptions::new().write(true).create_new(true).open(&pb).is_ok() as i32),
        "getParent" => match pb.parent() { Some(p) if !p.as_os_str().is_empty() => Value::Str(p.to_string_lossy().into_owned().into()), _ => Value::Null },
        "getParentFile" => match pb.parent() {
            Some(p) if !p.as_os_str().is_empty() => {
                let h = vm.heap.alloc("java/io/File");
                vm.heap.put_field_syn(h, crate::heap::Syn::Path, Value::Str(p.to_string_lossy().into_owned().into()));
                Value::Ref(h)
            }
            _ => Value::Null,
        },
        "getCanonicalFile" | "getAbsoluteFile" => args.first().cloned().unwrap_or(Value::Null),
        "getCanonicalPath" => Value::Str(fs::canonicalize(&pb).map(|c| c.to_string_lossy().into_owned()).unwrap_or(p.clone()).into()),
        "lastModified" => Value::Long(fs::metadata(&pb).ok().and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0)),
        "list" | "listFiles" => {
            let entries: Vec<Value> = fs::read_dir(&pb).map(|rd| rd.filter_map(|e| e.ok()).map(|e| {
                let ep = e.path().to_string_lossy().into_owned();
                if name == "list" {
                    Value::Str(e.file_name().to_string_lossy().into_owned().into())
                } else {
                    let h = vm.heap.alloc("java/io/File");
                    vm.heap.put_field_syn(h, crate::heap::Syn::Path, Value::Str(ep.into()));
                    Value::Ref(h)
                }
            }).collect()).unwrap_or_default();
            Value::Array(vm.heap.alloc_values("Ljava/lang/Object;", entries))
        }
        "renameTo" => {
            let dst = path_str(vm, args.get(1)).unwrap_or_default();
            Value::Int(fs::rename(&pb, &dst).is_ok() as i32)
        }
        "hashCode" => Value::Int(p.bytes().fold(0i32, |a, b| a.wrapping_mul(31).wrapping_add(b as i32))),
        "equals" => Value::Int((path_str(vm, args.get(1)).as_deref() == Some(p.as_str())) as i32),
        "compareTo" => Value::Int(match p.as_str().cmp(path_str(vm, args.get(1)).as_deref().unwrap_or("")) {
            std::cmp::Ordering::Less => -1, std::cmp::Ordering::Equal => 0, std::cmp::Ordering::Greater => 1 }),
        _ => return None,
    };
    Some(Some(ret))
}
