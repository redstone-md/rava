//! Reading a jar as a filesystem.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nio::*;

/// The entry's non-empty '/'-separated components, BORROWED and lazy.
///
/// ⭐**THE WHOLE COMPONENT LIST WAS BUILT FOR EVERY JAR `Path` CALL, AND NINE OF THE FOURTEEN
/// ARMS NEVER LOOKED AT IT.** `entry.split('/').filter(..).collect()` at the head of
/// `jar_path_method` is a `Vec` allocation plus a full scan of the entry path, and it stood ahead
/// of `toString`, `getParent`, `resolve`, `resolveSibling`, `relativize`, `startsWith`,
/// `endsWith`, `equals`, `hashCode`, `toUri` and `normalize` — the arms MC's pack scan actually
/// runs, once or more per resource, on paths like `data/minecraft/tags/worldgen/biome/x.json`
/// (eight components). The five arms that do need components now build or walk them themselves.
fn jar_comps<'a>(entry: &'a str) -> impl Iterator<Item = &'a str> + Clone + 'a {
    entry.split('/').filter(|s| !s.is_empty())
}

/// Path methods for a jar-internal path (segments split on '/').
pub(crate) fn jar_path_method(vm: &mut Vm, entry: &str, name: &str, args: &[Value]) -> Option<Option<Value>> {
    let ret = match name {
        "toString" => Value::Str(entry.into()),
        "toRealPath" | "toAbsolutePath" | "normalize" => make_jar_path(vm, entry),
        "getParent" => match entry.rfind('/') {
            Some(i) if i > 0 => make_jar_path(vm, &entry[..i]),
            _ => Value::Null,
        },
        // The LAST component, which never needed the list in front of it.
        "getFileName" => {
            let last = entry.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
            make_jar_path(vm, last)
        }
        "getNameCount" => Value::Int(jar_comps(entry).count() as i32),
        "getName" => {
            let i = args.get(1).and_then(Value::as_int).unwrap_or(0) as usize;
            let c = jar_comps(entry).nth(i).unwrap_or("");
            make_jar_path(vm, c)
        }
        // A RANGE genuinely needs the list (it is indexed from both ends), so this arm keeps it.
        "subpath" => {
            let segs: Vec<&str> = jar_comps(entry).collect();
            let a = args.get(1).and_then(Value::as_int).unwrap_or(0).max(0) as usize;
            let b = (args.get(2).and_then(Value::as_int).unwrap_or(0).max(0) as usize).min(segs.len());
            make_jar_path(vm, &segs.get(a..b).map(|s| s.join("/")).unwrap_or_default())
        }
        "resolve" | "resolveSibling" => {
            let other = jar_entry(vm, args.get(1)).or_else(|| path_str(vm, args.get(1))).unwrap_or_default();
            let base = if name == "resolveSibling" { entry.rsplit_once('/').map(|(a, _)| a).unwrap_or("") } else { entry };
            make_jar_path(vm, &format!("{}/{}", base.trim_end_matches('/'), other.trim_start_matches('/')))
        }
        "relativize" => {
            let other = jar_entry(vm, args.get(1)).unwrap_or_default();
            let rel = other.strip_prefix(entry).map(|s| s.trim_start_matches('/')).unwrap_or(&other);
            make_jar_path(vm, rel)
        }
        "startsWith" => {
            let other = jar_entry(vm, args.get(1)).or_else(|| path_str(vm, args.get(1))).unwrap_or_default();
            Value::Int(entry.starts_with(other.trim_end_matches('/')) as i32)
        }
        "endsWith" => {
            let other = jar_entry(vm, args.get(1)).or_else(|| path_str(vm, args.get(1))).unwrap_or_default();
            Value::Int(entry.ends_with(&other) as i32)
        }
        "equals" => Value::Int((jar_entry(vm, args.get(1)).as_deref() == Some(entry)) as i32),
        "hashCode" => Value::Int(entry.bytes().fold(0i32, |a, b| a.wrapping_mul(31).wrapping_add(b as i32))),
        "toUri" | "toUrl" | "toURL" => {
            let u = make_uri(vm, if name == "toUri" { "java/net/URI" } else { "java/net/URL" }, entry);
            if let Value::Ref(h) = &u { vm.heap.put_field(*h, "$jarres", Value::Str(entry.into())); }
            u
        }
        "getFileSystem" => Value::Ref(vm.heap.alloc("java/nio/file/FileSystem")),
        // A Path is Iterable<Path> over its name elements — Joiner.join(path) and
        // for-each rely on this (PathPackResources builds a resource id from
        // Joiner.join(root.relativize(file))). Without it the iteration is empty
        // and every resource collapses to an empty path.
        "iterator" | "spliterator" => {
            // Genuinely produces one object per component; the `Vec<&str>` is collected first
            // because `make_jar_path` takes `&mut Vm` while an iterator over `entry` is alive
            // only borrows the entry, not the VM — but the borrow checker cannot see that
            // through the closure, so the components are pinned once, exactly as before.
            let segs: Vec<&str> = jar_comps(entry).collect();
            let parts: Vec<Value> = segs.iter().map(|s| make_jar_path(vm, s)).collect();
            crate::stream::iterator_of(vm, parts)
        }
        _ => return None,
    };
    Some(Some(ret))
}

/// Files methods for a jar-internal path — reads/enumerates through the loader.
pub(crate) fn jar_files_method(vm: &mut Vm, entry: &str, name: &str, args: &[Value]) -> Option<Option<Value>> {
    let ret = match name {
        "exists" => Value::Int((vm.loader.has_resource(entry) || jar_is_dir(vm, entry)) as i32),
        "notExists" => Value::Int(!(vm.loader.has_resource(entry) || jar_is_dir(vm, entry)) as i32),
        "isDirectory" => Value::Int(jar_is_dir(vm, entry) as i32),
        "isRegularFile" => Value::Int(vm.loader.has_resource(entry) as i32),
        "isReadable" => Value::Int(1),
        "size" => Value::Long(vm.loader.resource_size(entry).unwrap_or(0) as i64),
        "readAllBytes" => {
            let b = vm.loader.read_resource(entry).unwrap_or_default();
            Value::Array(vm.heap.alloc_bytes(&b))
        }
        "newInputStream" => {
            let b = vm.loader.read_resource(entry).unwrap_or_default();
            crate::natives::make_stream(vm, &b)
        }
        "newBufferedReader" => {
            let b = vm.loader.read_resource(entry).unwrap_or_default();
            let stream = crate::natives::make_stream(vm, &b);
            let r = vm.heap.alloc("java/io/BufferedReader");
            vm.heap.put_field(r, "$stream", stream);
            Value::Ref(r)
        }
        "readAttributes" => {
            let dir = jar_is_dir(vm, entry);
            let sz = vm.loader.resource_size(entry).unwrap_or(0);
            let h = vm.heap.alloc("java/nio/file/attribute/BasicFileAttributes");
            vm.heap.put_field_syn(h, crate::heap::Syn::Size, Value::Long(sz as i64));
            vm.heap.put_field(h, "$dir", Value::Int(dir as i32));
            vm.heap.put_field(h, "$millis", Value::Long(0));
            Value::Ref(h)
        }
        "list" | "newDirectoryStream" => {
            let kids: Vec<Value> = jar_children(vm, entry).iter().map(|e| make_jar_path(vm, e)).collect();
            crate::stream::make_stream(vm, kids)
        }
        "walk" => {
            // Files.walk(start): every descendant (start included), deduplicated
            // (jar dir entries and the start would otherwise repeat).
            let prefix = format!("{}/", entry.trim_end_matches('/'));
            let mut seen: std::collections::BTreeSet<String> =
                vm.loader.list_resources(&prefix).into_iter().map(|e| e.trim_end_matches('/').to_string()).collect();
            seen.insert(entry.trim_end_matches('/').to_string());
            let paths: Vec<Value> = seen.iter().map(|e| make_jar_path(vm, e)).collect();
            crate::stream::make_stream(vm, paths)
        }
        "find" => {
            // Files.find(start, maxDepth, BiPredicate, ...): descendants matching
            // the predicate. Honouring it is essential — MC's pack listing filters
            // to regular .json files, and ignoring it returned the directory
            // entries (relative path ""), which made listResources abort with no
            // resources (every datapack registry came up empty). args[2] is the
            // BiPredicate; absent → keep regular files only.
            let pred = args.get(2).filter(|v| v.is_fn_value()).cloned();
            let prefix = format!("{}/", entry.trim_end_matches('/'));
            let mut seen: std::collections::BTreeSet<String> =
                vm.loader.list_resources(&prefix).into_iter().map(|e| e.trim_end_matches('/').to_string()).collect();
            seen.insert(entry.trim_end_matches('/').to_string());
            // Paths accumulate in a Rust local across apply() calls; gate GC so it
            // can't free them mid-iteration.
            crate::frames::native_depth_inc();
            let mut out = Vec::new();
            for e in seen {
                let is_dir = jar_is_dir(vm, &e);
                let sz = if is_dir { 0 } else { vm.loader.resource_size(&e).unwrap_or(0) };
                let path = make_jar_path(vm, &e);
                let keep = match &pred {
                    Some(p) => {
                        let attrs = vm.heap.alloc("java/nio/file/attribute/BasicFileAttributes");
                        vm.heap.put_field_syn(attrs, crate::heap::Syn::Size, Value::Long(sz as i64));
                        vm.heap.put_field(attrs, "$dir", Value::Int(is_dir as i32));
                        vm.heap.put_field(attrs, "$millis", Value::Long(0));
                        vm.apply(p, vec![path.clone(), Value::Ref(attrs)]).ok().flatten()
                            .and_then(|v| v.as_int()).map(|i| i != 0).unwrap_or(false)
                    }
                    None => !is_dir,
                };
                if keep { out.push(path); }
            }
            crate::frames::native_depth_dec();
            crate::stream::make_stream(vm, out)
        }
        "walkFileTree" => {
            let visitor = args.iter().rev().find_map(|a| match a { Value::Ref(h) => Some(*h), _ => None });
            if let Some(visitor) = visitor {
                let _ = jar_walk(vm, entry, visitor);
            }
            args.first().cloned().unwrap_or(Value::Null)
        }
        _ => return None,
    };
    Some(Some(ret))
}

/// walkFileTree over a jar directory: visit each entry depth-first, invoking the
/// FileVisitor's callbacks (same protocol as the disk walk).
pub(crate) fn jar_walk(vm: &mut Vm, entry: &str, visitor: u32) -> Result<bool, crate::VmError> {
    let mk_attrs = |vm: &mut Vm, dir: bool, sz: u64| -> Value {
        let h = vm.heap.alloc("java/nio/file/attribute/BasicFileAttributes");
        vm.heap.put_field_syn(h, crate::heap::Syn::Size, Value::Long(sz as i64));
        vm.heap.put_field(h, "$dir", Value::Int(dir as i32));
        vm.heap.put_field(h, "$millis", Value::Long(0));
        Value::Ref(h)
    };
    if jar_is_dir(vm, entry) {
        let p = make_jar_path(vm, entry);
        let attrs = mk_attrs(vm, true, 0);
        let r = call_visitor(vm, visitor, "preVisitDirectory", p, attrs)?;
        if r == 1 { return Ok(false); }
        if r != 2 {
            for child in jar_children(vm, entry) {
                if !jar_walk(vm, &child, visitor)? { return Ok(false); }
            }
        }
        let p = make_jar_path(vm, entry);
        let r = call_visitor(vm, visitor, "postVisitDirectory", p, Value::Null)?;
        if r == 1 { return Ok(false); }
    } else {
        let sz = vm.loader.resource_size(entry).unwrap_or(0);
        let p = make_jar_path(vm, entry);
        let attrs = mk_attrs(vm, false, sz);
        let r = call_visitor(vm, visitor, "visitFile", p, attrs)?;
        if r == 1 { return Ok(false); }
    }
    Ok(true)
}
