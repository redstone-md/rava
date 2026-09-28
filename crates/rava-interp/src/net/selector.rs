//! Selectors and the poll behind them.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::net::*;

pub(crate) fn inet_socket_address(vm: &mut Vm, name: &str, descriptor: &str, args: &[Value]) -> Handled {
    // Static factory: createUnresolved(host, port) — no receiver.
    if name == "createUnresolved" {
        let host = match args.first() { Some(Value::Str(s)) => s.to_string(), _ => "0.0.0.0".into() };
        let port = args.get(1).and_then(Value::as_int).unwrap_or(0);
        let h = vm.heap.alloc("java/net/InetSocketAddress");
        vm.heap.put_field(h, "$host", Value::Str(host.into()));
        vm.heap.put_field(h, "$port", Value::Int(port));
        vm.heap.put_field(h, "$unresolved", Value::Int(1));
        return Ok(Some(Some(Value::Ref(h))));
    }
    let h = match recv(args) { Some(h) => h, None => return Ok(None) };
    match name {
        "<init>" => {
            // (int) -> wildcard host; (String|InetAddress|null, int) -> host+port.
            // A null InetAddress means the wildcard address, so the port still comes
            // from arg2 — the old `_ => (.., 0)` catch-all dropped it, so MC's
            // `new InetSocketAddress((InetAddress)null, 25565)` bound port 0.
            let (host, port) = if descriptor.starts_with("(I") {
                ("0.0.0.0".to_string(), args.get(1).and_then(Value::as_int).unwrap_or(0))
            } else {
                let host = match args.get(1) {
                    Some(Value::Str(s)) => s.to_string(),
                    Some(Value::Ref(a)) => match vm.heap.get_field(*a, "$host") {
                        Value::Str(s) => s.to_string(), _ => "0.0.0.0".into(),
                    },
                    _ => "0.0.0.0".to_string(),
                };
                (host, args.get(2).and_then(Value::as_int).unwrap_or(0))
            };
            vm.heap.put_field(h, "$host", Value::Str(host.into()));
            vm.heap.put_field(h, "$port", Value::Int(port));
            Ok(Some(None))
        }
        "getPort" => Ok(Some(Some(vm.heap.get_field(h, "$port")))),
        "getHostString" | "getHostName" => Ok(Some(Some(match vm.heap.get_field(h, "$host") {
            Value::Str(s) => Value::Str(s), _ => Value::Str("0.0.0.0".into()),
        }))),
        // Return a real InetAddress (not null) so callers that chain
        // getAddress().getHostAddress() — MC's HandshakeListener — don't NPE.
        "getAddress" => {
            let ia = vm.heap.alloc("java/net/InetAddress");
            let host = match vm.heap.get_field(h, "$host") { Value::Str(s) => s.to_string(), _ => "127.0.0.1".into() };
            vm.heap.put_field(ia, "$addr", Value::Str(host.clone().into()));
            vm.heap.put_field_syn(ia, crate::heap::Syn::Name, Value::Str(host.into()));
            Ok(Some(Some(Value::Ref(ia))))
        }
        "isUnresolved" => Ok(Some(Some(Value::Int(matches!(vm.heap.get_field(h, "$unresolved"), Value::Int(1)) as i32)))),
        "toString" => {
            let host = match vm.heap.get_field(h, "$host") { Value::Str(s) => s.to_string(), _ => "0.0.0.0".into() };
            let port = vm.heap.get_field(h, "$port").as_int().unwrap_or(0);
            Ok(Some(Some(Value::Str(format!("{host}:{port}").into()))))
        }
        _ => Ok(None),
    }
}

/// Poll one selector: mark each registered key's `$readyOps`, stash the ready
/// keys on `$selected`, and return them as `(key_handle, ready_ops)`. Shared by
/// `Selector.select()` and the per-tick `pump_network`.
pub(crate) fn poll_selector(vm: &mut Vm, sel: u32) -> Vec<(u32, i32)> {
    let key_vals = match vm.heap.get_field(sel, "$keys") {
        Value::Array(a) => vm.heap.array_values(a).unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut selected_vals = Vec::new();
    let mut ready_keys = Vec::new();
    for kv in &key_vals {
        let key = match kv { Value::Ref(k) => *k, _ => continue };
        let chan = match vm.heap.get_field(key, "$channel") { Value::Ref(c) => c, _ => continue };
        let interest = vm.heap.get_field(key, "$interestOps").as_int().unwrap_or(0);
        let mut ready = 0;
        let is_listener = matches!(vm.sockets.get(&chan), Some(NetSock::Listener(_)));
        if is_listener {
            if interest & OP_ACCEPT != 0 {
                // Drain pending OS connections into the stash. Only report ACCEPT
                // ready (and so drive netty's allocating accept) when one actually
                // arrived — idle polls cost one non-blocking accept() syscall and
                // allocate nothing.
                loop {
                    let s = match vm.sockets.get(&chan) {
                        Some(NetSock::Listener(l)) => match l.accept() {
                            Ok((s, _)) => { let _ = s.set_nonblocking(true); Some(s) }
                            Err(_) => None,
                        },
                        _ => None,
                    };
                    match s {
                        Some(s) => vm.pending_streams.entry(chan).or_default().push_back(s),
                        None => break,
                    }
                }
                if vm.pending_streams.get(&chan).is_some_and(|q| !q.is_empty()) {
                    ready |= OP_ACCEPT;
                }
            }
        } else if let Some(NetSock::Stream(s)) = vm.sockets.get(&chan) {
            if interest & OP_READ != 0 {
                let mut probe = [0u8; 1];
                match s.peek(&mut probe) {
                    Ok(_) => ready |= OP_READ,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => ready |= OP_READ,
                }
            }
            if interest & OP_WRITE != 0 { ready |= OP_WRITE; }
        }
        vm.heap.put_field(key, "$readyOps", Value::Int(ready));
        if ready != 0 { selected_vals.push(kv.clone()); ready_keys.push((key, ready)); }
    }
    // Only allocate the selected-set array when something is ready; an idle poll
    // leaves it null (selectedKeys() materialises an empty set on demand).
    if selected_vals.is_empty() {
        vm.heap.put_field(sel, "$selected", Value::Null);
    } else {
        let arr = vm.heap.alloc_values("Ljava/lang/Object;", selected_vals);
        vm.heap.put_field(sel, "$selected", Value::Array(arr));
    }
    ready_keys
}

pub(crate) fn selector(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let h = match recv(args) { Some(h) => h, None => return Ok(None) };
    match name {
        "open" => Ok(Some(Some(Value::Ref(new_selector(vm))))),
        // Poll every registered channel and mark readiness. A listener is reported
        // acceptable whenever ACCEPT is in its interest set (accept() returns null
        // when nothing is pending); a stream is readable iff the OS has buffered
        // bytes (non-consuming peek) and is treated as always writable. The keys
        // that became ready are stashed on `$selected` for selectedKeys().
        "select" | "selectNow" => {
            let n = poll_selector(vm, h).len() as i32;
            Ok(Some(Some(Value::Int(n))))
        }
        "wakeup" => Ok(Some(Some(Value::Ref(h)))),
        "selectedKeys" => {
            let set = vm.heap.alloc("java/util/LinkedHashSet");
            let sel = match vm.heap.get_field(h, "$selected") {
                Value::Array(a) => Value::Array(a),
                _ => Value::Array(vm.heap.alloc_values("Ljava/lang/Object;", Vec::new())),
            };
            vm.heap.put_field_syn(set, crate::heap::Syn::Elems, sel);
            Ok(Some(Some(Value::Ref(set))))
        }
        "keys" => {
            let keys = vm.heap.get_field(h, "$keys");
            let set = vm.heap.alloc("java/util/LinkedHashSet");
            vm.heap.put_field_syn(set, crate::heap::Syn::Elems, keys);
            Ok(Some(Some(Value::Ref(set))))
        }
        "isOpen" => Ok(Some(Some(Value::Int(1)))),
        "provider" => Ok(Some(Some(Value::Ref(new_obj(vm, "java/nio/channels/spi/SelectorProvider"))))),
        "close" => Ok(Some(None)),
        _ => Ok(None),
    }
}
