//! URLs, resources, service providers and the runtime object.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::reflect::*;

/// Concurrency locks/conditions — no-ops in the single-threaded model.
pub(crate) fn locks(vm: &mut Vm, name: &str, _args: &[Value]) -> Handled {
    match name {
        "readLock" | "writeLock" => Ok(Some(Some(Value::Ref(vm.heap.alloc("java/util/concurrent/locks/Lock"))))),
        "newCondition" => Ok(Some(Some(Value::Ref(vm.heap.alloc("java/util/concurrent/locks/Condition"))))),
        "tryLock" | "tryAcquire" => Ok(Some(Some(Value::Int(1)))),
        "isLocked" | "isHeldByCurrentThread" | "getReadLockCount" | "getHoldCount"
        | "availablePermits" | "getQueueLength" => Ok(Some(Some(Value::Int(0)))),
        "lock" | "unlock" | "lockInterruptibly" | "acquire" | "release"
        | "await" | "signal" | "signalAll" | "<init>"
        // LockSupport (single-threaded: nothing to block on / wake).
        | "park" | "unpark" | "parkNanos" | "parkUntil" | "setCurrentBlocker" => Ok(Some(None)),
        "getBlocker" => Ok(Some(Some(Value::Null))),
        _ => Ok(None),
    }
}

// --- helpers ----------------------------------------------------------------

/// java.net.URLConnection / HttpURLConnection — a non-networking stub. Setup
/// calls are no-ops; any attempt to actually read fails with IOException, the
/// same outcome a real server sees when offline. Callers that fetch at boot
/// (services public-key validation) catch it and proceed without remote data.
pub(crate) fn urlconnection_method(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let io = |vm: &mut Vm| VmError::Throw(vm.make_exception(
        "java/io/IOException", "rava: network access is unavailable"));
    let ret = match name {
        // Configuration — chainable no-ops.
        "setRequestProperty" | "addRequestProperty" | "setConnectTimeout"
        | "setReadTimeout" | "setDoInput" | "setDoOutput" | "setUseCaches"
        | "setRequestMethod" | "setInstanceFollowRedirects" | "setAllowUserInteraction"
        | "setIfModifiedSince" | "connect" | "disconnect" | "setFixedLengthStreamingMode"
        | "setChunkedStreamingMode" => return Ok(Some(None)),
        "getRequestProperty" | "getHeaderField" | "getContentType"
        | "getContentEncoding" | "getErrorStream" => Value::Null,
        "getContentLength" => Value::Int(-1),
        "getContentLengthLong" => Value::Long(-1),
        "getURL" => args.first().cloned().unwrap_or(Value::Null),
        "usingProxy" => Value::Int(0),
        // Reads/connects fail like an unreachable host.
        "getInputStream" | "getResponseCode" | "getOutputStream"
        | "getContent" | "getHeaderFields" => return Err(io(vm)),
        _ => Value::Null,
    };
    Ok(Some(Some(ret)))
}

/// A `java/net/URL` for a classpath resource, tagged with `$jarres` so a later
/// toURI()/Paths.get() resolves it to a jar-internal Path (the data-pack root).
pub(crate) fn make_resource_url(vm: &mut Vm, path: &str) -> Value {
    let h = vm.heap.alloc("java/net/URL");
    vm.heap.put_field(h, "$res", Value::Str(path.into()));
    vm.heap.put_field(h, "$jarres", Value::Str(path.into()));
    // Report a `file:` scheme: the vanilla-pack builder only accepts file/jar
    // URLs, then does Paths.get(uri) — which we route to a jar-internal Path via
    // $jarres. The literal file path is never opened directly.
    vm.heap.put_field_syn(h, crate::heap::Syn::Str, Value::Str(format!("file:/{path}").into()));
    Value::Ref(h)
}

/// Turn a `file:` URL into a filesystem path (best-effort, Windows-aware).
pub(crate) fn url_to_path(url: &str) -> String {
    let s = url.strip_prefix("file:").unwrap_or(url);
    let s = s.trim_start_matches('/');
    // "D:/temp/x" stays; a Unix "/etc" needs its leading slash back.
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        s.to_string()
    } else {
        format!("/{s}")
    }
}

// --- java.util.function combinators -----------------------------------------

/// Read `META-INF/services/<iface>` impl class names (binary form).
pub(crate) fn service_impls(vm: &mut Vm, iface_binary: &str) -> Vec<String> {
    let path = format!("META-INF/services/{}", iface_binary.replace('/', "."));
    let bytes = vm.loader.read_resource(&path).unwrap_or_default();
    let impls: Vec<String> = String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|l| {
            let l = l.split('#').next().unwrap_or("").trim();
            if l.is_empty() { None } else { Some(l.replace('.', "/")) }
        })
        .collect();
    static TSVC: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if crate::jit::env_gate("RAVA_TRACE_SVC", &TSVC) {
        eprintln!("[SVC] {path} -> {impls:?} ({} bytes)", bytes.len());
    }
    impls
}

pub(crate) fn provider(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let cls = field_str(vm, args.first(), "$cls").unwrap_or_default();
    match name {
        "get" => Ok(Some(Some(new_instance(vm, &cls, "", &[])?))),
        "type" => Ok(Some(Some(vm.make_class(&cls)))),
        _ => Ok(None),
    }
}

// --- atomic field updaters (operate on a target object's named field) -------

pub(crate) fn runtime_method(vm: &mut Vm, name: &str) -> Handled {
    match name {
        "getRuntime" => Ok(Some(Some(Value::Ref(vm.heap.alloc("java/lang/Runtime"))))),
        "availableProcessors" => {
            let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            Ok(Some(Some(Value::Int(n as i32))))
        }
        "maxMemory" | "totalMemory" => Ok(Some(Some(Value::Long(2 * 1024 * 1024 * 1024)))), // 2 GiB
        "freeMemory" => Ok(Some(Some(Value::Long(1024 * 1024 * 1024)))),
        "gc" | "addShutdownHook" | "removeShutdownHook" | "runFinalization" => Ok(Some(None)),
        "version" => Ok(Some(Some(Value::Null))),
        _ => Ok(None),
    }
}

// --- java.lang.StackWalker --------------------------------------------------
