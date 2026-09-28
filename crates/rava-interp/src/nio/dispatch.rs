//! Which NIO family serves a call.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::nio::*;

/// Returns `Ok(Some(ret))` when handled (ret = optional return value), `Ok(None)` when
/// this family does not serve the call, and `Err` for the cases the JDK THROWS on —
/// `Files.copy` onto an existing target, `Files.delete` of a missing one. Without the
/// error channel those two answered success, which is the opposite of the contract.
pub(crate) fn dispatch(
    vm: &mut Vm,
    owner: &str,
    name: &str,
    args: &[Value],
) -> Result<Option<Option<Value>>, crate::vmframe::VmError> {
    Ok(match owner {
        "java/nio/file/Paths" | "java/nio/file/Path" if name == "get" || name == "of" => {
            Some(Some(paths_get(vm, args)))
        }
        "java/nio/file/Path" => path_method(vm, name, args),
        "java/nio/file/Files" => files_method(vm, name, args)?,
        "java/nio/file/attribute/BasicFileAttributes" | "java/nio/file/attribute/FileTime"
        | "java/nio/file/attribute/PosixFileAttributes"
        | "java/nio/file/attribute/DosFileAttributes" => fileattr_method(vm, name, args),
        "java/nio/channels/FileChannel" | "java/nio/channels/AsynchronousFileChannel"
        | "java/nio/channels/SeekableByteChannel" => filechannel_method(vm, name, args),
        "java/nio/channels/FileLock" => filelock_method(vm, name, args),
        // SimpleFileVisitor (JDK base class) default callbacks: every variant
        // returns CONTINUE except visitFileFailed, which rethrows. Subclasses
        // call super.preVisitDirectory(...) and ARETURN the result, so a void
        // return here underflows their stack (DirectoryValidator does this).
        "java/nio/file/SimpleFileVisitor" | "java/nio/file/FileVisitor"
            if matches!(name, "preVisitDirectory" | "visitFile" | "postVisitDirectory" | "visitFileFailed") =>
        {
            let h = vm.heap.alloc("java/nio/file/FileVisitResult");
            vm.heap.put_field(h, "$enumName", Value::Str("CONTINUE".into()));
            vm.heap.put_field(h, "$enumOrd", Value::Int(0));
            Some(Some(Value::Ref(h)))
        }
        "java/nio/file/FileSystems" => match name {
            // newFileSystem(uri|path[, ...]) / getDefault → a FileSystem carrying
            // the source's jar entry so getPath builds jar-internal paths.
            "newFileSystem" | "getDefault" => {
                let h = vm.heap.alloc("java/nio/file/FileSystem");
                if let Some(e) = jar_entry(vm, args.first()) { vm.heap.put_field(h, "$jarres", Value::Str(e.into())); }
                Some(Some(Value::Ref(h)))
            }
            _ => Some(Some(Value::Null)),
        },
        "java/nio/file/FileSystem" => match name {
            "getPath" => {
                let mut parts: Vec<String> = Vec::new();
                if let Some(Value::Str(s)) = args.get(1) { parts.push(s.to_string()); }
                if let Some(Value::Array(a)) = args.get(2) {
                    for v in vm.heap.array_values(*a).unwrap_or_default() {
                        if let Value::Str(s) = v { if !s.is_empty() { parts.push(s.to_string()); } }
                    }
                }
                let joined = parts.join("/");
                // A jar-tagged FileSystem yields jar-internal paths.
                if jar_entry(vm, args.first()).is_some() || joined.starts_with("data/") || joined.starts_with("/data") {
                    Some(Some(make_jar_path(vm, joined.trim_start_matches('/'))))
                } else {
                    Some(Some(make_path(vm, &joined)))
                }
            }
            "getSeparator" => Some(Some(Value::Str("/".into()))),
            "provider" => Some(Some(Value::Ref(vm.heap.alloc("java/nio/file/spi/FileSystemProvider")))),
            "isOpen" | "isReadOnly" => Some(Some(Value::Int((name == "isOpen") as i32))),
            "close" => Some(None),
            "supportedFileAttributeViews" => {
                let arr = vm.heap.alloc_values("Ljava/lang/Object;", Vec::new());
                let h = vm.heap.alloc("java/util/HashSet");
                vm.heap.put_field_syn(h, crate::heap::Syn::Elems, Value::Array(arr));
                Some(Some(Value::Ref(h)))
            }
            _ => Some(Some(Value::Null)),
        },
        "java/io/OutputStream" | "java/io/FileOutputStream" | "java/io/BufferedOutputStream"
        | "java/io/FilterOutputStream" | "java/io/ByteArrayOutputStream"
        | "java/security/DigestOutputStream" => out_method(vm, name, args),
        "java/io/DataOutputStream" | "java/util/zip/GZIPOutputStream"
        | "java/util/zip/DeflaterOutputStream" => bin_out_method(vm, owner, name, args),
        // netty's ByteBufOutputStream delegates writeUTF to an inner
        // DataOutputStream whose bytes never reached the wrapped ByteBuf — NBT
        // tag names vanished. Intercept writeUTF here: bin_out_method emits the
        // 2-byte-length + UTF bytes and os_write routes them into the `buffer`.
        // Other methods (writeByte/write/...) run as bytecode straight to the buffer.
        "io/netty/buffer/ByteBufOutputStream" if name == "writeUTF" => bin_out_method(vm, owner, name, args),
        "java/io/InputStream" if name == "transferTo" => Some(Some(transfer_to(vm, args))),
        "java/io/RandomAccessFile" => raf_method(vm, name, args),
        "java/io/File" => file_method(vm, name, args),
        "java/nio/file/spi/FileSystemProvider" if name == "installedProviders" => {
            // rava DOES serve jar/file filesystems natively (newFileSystem above), so
            // advertise both schemes — an empty list made guest probes like MC
            // Util.<clinit>'s `installedProviders().filter(getScheme()=="jar")
            // .orElseThrow` fail with "No jar file system provider found".
            let providers: Vec<Value> = ["file", "jar"]
                .iter()
                .map(|s| {
                    let p = vm.heap.alloc("java/nio/file/spi/FileSystemProvider");
                    vm.heap.put_field(p, "$scheme", Value::Str((*s).into()));
                    Value::Ref(p)
                })
                .collect();
            let arr = vm.heap.alloc_values("Ljava/lang/Object;", providers);
            let list = vm.heap.alloc("java/util/ArrayList");
            vm.heap.put_field_syn(list, crate::heap::Syn::Elems, Value::Array(arr));
            Some(Some(Value::Ref(list)))
        }
        "java/nio/file/spi/FileSystemProvider" if name == "getScheme" => {
            let s = match args.first() {
                Some(Value::Ref(h)) => match vm.heap.get_field(*h, "$scheme") {
                    Value::Str(s) => s.to_string(),
                    _ => "file".to_string(),
                },
                _ => "file".to_string(),
            };
            Some(Some(Value::Str(s.into())))
        }
        "java/nio/file/FileStore" => match name {
            "getTotalSpace" | "getUsableSpace" | "getUnallocatedSpace" => Some(Some(Value::Long(500 * 1024 * 1024 * 1024))),
            "getBlockSize" => Some(Some(Value::Long(4096))),
            "name" | "type" => Some(Some(Value::Str("rava".into()))),
            "isReadOnly" => Some(Some(Value::Int(0))),
            _ => Some(Some(Value::Null)),
        },
        _ => None,
    })
}
