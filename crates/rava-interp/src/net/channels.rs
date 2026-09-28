//! Socket and server-socket channels.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::net::*;

pub(crate) fn server_socket_channel(vm: &mut Vm, name: &str, descriptor: &str, args: &[Value]) -> Handled {
    let h = match recv(args) { Some(h) => h, None => return Ok(None) };
    match name {
        "open" => Ok(Some(Some(Value::Ref(new_obj(vm, "java/nio/channels/ServerSocketChannel"))))),
        "configureBlocking" => {
            vm.heap.put_field(h, "$blocking", args.get(1).cloned().unwrap_or(Value::Int(1)));
            Ok(Some(Some(Value::Ref(h))))
        }
        "isBlocking" => Ok(Some(Some(vm.heap.get_field(h, "$blocking")))),
        "bind" => {
            let (host, port) = addr_of(vm, args.get(1));
            let trace = std::env::var("RAVA_TRACE_NET").is_ok();
            match TcpListener::bind((host.as_str(), port as u16)) {
                Ok(l) => {
                    let _ = l.set_nonblocking(true);
                    let local = l.local_addr().ok();
                    if trace { eprintln!("[NET] ServerSocketChannel.bind OK {host}:{port} chan={h}"); }
                    vm.sockets.insert(h, NetSock::Listener(l));
                    if let Some(SocketAddr::V4(a)) = local { vm.heap.put_field(h, "$port", Value::Int(a.port() as i32)); }
                    else { vm.heap.put_field(h, "$port", Value::Int(port)); }
                    vm.heap.put_field(h, "$host", Value::Str(host.into()));
                    vm.heap.put_field(h, "$bound", Value::Int(1));
                    Ok(Some(Some(Value::Ref(h))))
                }
                Err(e) => {
                    if trace { eprintln!("[NET] ServerSocketChannel.bind FAIL {host}:{port}: {e}"); }
                    let msg = format!("{host}:{port} - {e}");
                    Err(VmError::Throw(vm.make_exception("java/net/BindException", &msg)))
                }
            }
        }
        "register" => {
            // register(Selector sel, int ops[, Object att]) -> SelectionKey
            let key = vm.heap.alloc("java/nio/channels/SelectionKey");
            vm.heap.put_field(key, "$channel", Value::Ref(h));
            vm.heap.put_field(key, "$selector", args.get(1).cloned().unwrap_or(Value::Null));
            vm.heap.put_field(key, "$interestOps", args.get(2).cloned().unwrap_or(Value::Int(0)));
            vm.heap.put_field(key, "$readyOps", Value::Int(0));
            if let Some(att) = args.get(3) { vm.heap.put_field(key, "$att", att.clone()); }
            // track on the selector's key set
            if let Some(Value::Ref(sel)) = args.get(1) {
                if let Value::Array(arr) = vm.heap.get_field(*sel, "$keys") {
                    let mut ks = vm.heap.array_values(arr).unwrap_or_default();
                    ks.push(Value::Ref(key));
                    let na = vm.heap.alloc_values("Ljava/lang/Object;", ks);
                    vm.heap.put_field(*sel, "$keys", Value::Array(na));
                }
            }
            vm.heap.put_field_syn(h, crate::heap::Syn::Key, Value::Ref(key));
            Ok(Some(Some(Value::Ref(key))))
        }
        "accept" => {
            // Prefer a connection the selector poll already stashed; otherwise try
            // the OS directly (non-blocking). Hand back a SocketChannel or null.
            let stream = vm.pending_streams.get_mut(&h).and_then(|q| q.pop_front()).or_else(|| {
                match vm.sockets.get(&h) {
                    Some(NetSock::Listener(l)) => l.accept().ok().map(|(s, _)| { let _ = s.set_nonblocking(true); s }),
                    _ => None,
                }
            });
            match stream {
                Some(stream) => {
                    let sc = vm.heap.alloc("java/nio/channels/SocketChannel");
                    vm.sockets.insert(sc, NetSock::Stream(stream));
                    Ok(Some(Some(Value::Ref(sc))))
                }
                None => Ok(Some(Some(Value::Null))),
            }
        }
        "validOps" => Ok(Some(Some(Value::Int(OP_ACCEPT)))),
        // Open from creation until explicitly closed (a fresh, unbound channel is
        // open — netty registers it before binding and asserts ensureOpen()).
        "isOpen" => Ok(Some(Some(Value::Int(!matches!(vm.heap.get_field(h, "$open"), Value::Int(0)) as i32)))),
        "isRegistered" => Ok(Some(Some(Value::Int(matches!(vm.heap.get_field_syn(h, crate::heap::Syn::Key), Value::Ref(_)) as i32)))),
        "keyFor" => Ok(Some(Some(vm.heap.get_field_syn(h, crate::heap::Syn::Key)))),
        "provider" => Ok(Some(Some(Value::Ref(new_obj(vm, "java/nio/channels/spi/SelectorProvider"))))),
        "socket" => {
            let s = vm.heap.alloc("java/net/ServerSocket");
            vm.heap.put_field(s, "$channel", Value::Ref(h));
            Ok(Some(Some(Value::Ref(s))))
        }
        "getLocalAddress" | "localAddress" => {
            let isa = vm.heap.alloc("java/net/InetSocketAddress");
            vm.heap.put_field(isa, "$host", vm.heap.get_field(h, "$host"));
            vm.heap.put_field(isa, "$port", vm.heap.get_field(h, "$port"));
            Ok(Some(Some(Value::Ref(isa))))
        }
        "close" => { vm.sockets.remove(&h); vm.heap.put_field(h, "$bound", Value::Int(0)); vm.heap.put_field(h, "$open", Value::Int(0)); Ok(Some(None)) }
        "setOption" => Ok(Some(Some(Value::Ref(h)))),
        _ if descriptor.is_empty() => Ok(None),
        _ => Ok(None),
    }
}

/// java.net.ServerSocket / Socket adapter wrapping a channel ($channel) — the
/// thin object netty's NioServerSocketChannel.javaChannel().socket() returns.
pub(crate) fn server_socket_adapter(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let h = match recv(args) { Some(h) => h, None => return Ok(None) };
    let chan = match vm.heap.get_field(h, "$channel") { Value::Ref(c) => Some(c), _ => None };
    let bound = chan.map(|c| matches!(vm.heap.get_field(c, "$bound"), Value::Int(1))).unwrap_or(false);
    let open = chan.map(|c| !matches!(vm.heap.get_field(c, "$open"), Value::Int(0))).unwrap_or(true);
    match name {
        "isBound" => Ok(Some(Some(Value::Int(bound as i32)))),
        "isClosed" => Ok(Some(Some(Value::Int(!open as i32)))),
        "isConnected" => Ok(Some(Some(Value::Int(bound as i32)))),
        "getLocalPort" => Ok(Some(Some(match chan { Some(c) => vm.heap.get_field(c, "$port"), None => Value::Int(-1) }))),
        "getInetAddress" | "getLocalSocketAddress" => {
            let isa = vm.heap.alloc("java/net/InetSocketAddress");
            if let Some(c) = chan {
                vm.heap.put_field(isa, "$host", vm.heap.get_field(c, "$host"));
                vm.heap.put_field(isa, "$port", vm.heap.get_field(c, "$port"));
            }
            Ok(Some(Some(Value::Ref(isa))))
        }
        // ServerSocket.bind(addr[, backlog]) — netty's NioServerSocketChannel.
        // doBind uses this on pre-Java-7. Bind the underlying channel for real so
        // the listener exists regardless of which path netty picks.
        "bind" => {
            let trace = std::env::var("RAVA_TRACE_NET").is_ok();
            if let Some(c) = chan {
                if matches!(vm.heap.get_field(c, "$bound"), Value::Int(1)) {
                    if trace { eprintln!("[NET] ServerSocket.bind: channel already bound"); }
                    return Ok(Some(None));
                }
                let (host, port) = addr_of(vm, args.get(1));
                match TcpListener::bind((host.as_str(), port as u16)) {
                    Ok(l) => {
                        let _ = l.set_nonblocking(true);
                        let local = l.local_addr().ok();
                        if trace { eprintln!("[NET] ServerSocket.bind OK {host}:{port} chan={c}"); }
                        vm.sockets.insert(c, NetSock::Listener(l));
                        if let Some(SocketAddr::V4(a)) = local { vm.heap.put_field(c, "$port", Value::Int(a.port() as i32)); }
                        else { vm.heap.put_field(c, "$port", Value::Int(port)); }
                        vm.heap.put_field(c, "$host", Value::Str(host.into()));
                        vm.heap.put_field(c, "$bound", Value::Int(1));
                    }
                    Err(e) => {
                        if trace { eprintln!("[NET] ServerSocket.bind FAIL {host}:{port}: {e}"); }
                        return Err(VmError::Throw(vm.make_exception("java/net/BindException", &format!("{host}:{port} - {e}"))));
                    }
                }
            } else if trace { eprintln!("[NET] ServerSocket.bind: no $channel"); }
            Ok(Some(None))
        }
        // java.net.Socket option setters netty's NioSocketChannelConfig drives —
        // all no-ops here (the real OS socket uses its defaults).
        "setReuseAddress" | "setReceiveBufferSize" | "setSoTimeout" | "setPerformancePreferences"
        | "setOption" | "setKeepAlive" | "setTcpNoDelay" | "setSoLinger" | "setSendBufferSize"
        | "setTrafficClass" | "setOOBInline" | "setBroadcast" => Ok(Some(None)),
        "getReuseAddress" | "getKeepAlive" | "getTcpNoDelay" | "getOOBInline" => Ok(Some(Some(Value::Int(1)))),
        "getReceiveBufferSize" | "getSendBufferSize" => Ok(Some(Some(Value::Int(65536)))),
        "getSoLinger" | "getTrafficClass" | "getSoTimeout" | "getPort" => Ok(Some(Some(Value::Int(0)))),
        "isInputShutdown" | "isOutputShutdown" => Ok(Some(Some(Value::Int(0)))),
        "shutdownInput" | "shutdownOutput" => Ok(Some(None)),
        "getRemoteSocketAddress" => {
            let isa = vm.heap.alloc("java/net/InetSocketAddress");
            vm.heap.put_field(isa, "$host", Value::Str("127.0.0.1".into()));
            vm.heap.put_field(isa, "$port", Value::Int(0));
            Ok(Some(Some(Value::Ref(isa))))
        }
        "close" => { if let Some(c) = chan { vm.sockets.remove(&c); vm.heap.put_field(c, "$open", Value::Int(0)); } Ok(Some(None)) }
        _ => Ok(None),
    }
}

pub(crate) fn socket_channel(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let h = match recv(args) { Some(h) => h, None => return Ok(None) };
    match name {
        "configureBlocking" => { vm.heap.put_field(h, "$blocking", args.get(1).cloned().unwrap_or(Value::Int(1))); Ok(Some(Some(Value::Ref(h)))) }
        "isOpen" => Ok(Some(Some(Value::Int(!matches!(vm.heap.get_field(h, "$open"), Value::Int(0)) as i32)))),
        "isConnected" => Ok(Some(Some(Value::Int(1)))),
        "finishConnect" => Ok(Some(Some(Value::Int(1)))),
        // read(ByteBuffer dst): pull bytes from the socket into the buffer. Returns
        // the count, 0 if the non-blocking socket has nothing now, or -1 at EOF —
        // the contract netty's NioSocketChannel.doReadBytes expects.
        "read" => {
            let bb = match args.get(1) { Some(Value::Ref(b)) => *b, _ => return Ok(Some(Some(Value::Int(0)))) };
            let room = buffer_room(vm, bb);
            if room == 0 { return Ok(Some(Some(Value::Int(0)))); }
            let mut tmp = vec![0u8; room];
            let n = match vm.sockets.get_mut(&h) {
                Some(NetSock::Stream(s)) => match s.read(&mut tmp) {
                    Ok(0) => -1,
                    Ok(n) => n as i32,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0,
                    Err(_) => -1,
                },
                _ => -1,
            };
            if n > 0 { crate::nio::buffer_fill(vm, bb, &tmp[..n as usize]); }
            Ok(Some(Some(Value::Int(n))))
        }
        // write(ByteBuffer src): drain the buffer's remaining bytes to the socket.
        "write" => {
            let bb = match args.get(1) { Some(Value::Ref(b)) => *b, _ => return Ok(Some(Some(Value::Int(0)))) };
            let data = crate::nio::buffer_remaining(vm, bb);
            if std::env::var("RAVA_TRACE_NET").is_ok() {
                let n = data.len().min(40);
                let hex: String = data[..n].iter().map(|b| format!("{b:02x}")).collect();
                eprintln!("[NET] SocketChannel.write {} bytes, head={hex}", data.len());
            }
            if data.is_empty() { return Ok(Some(Some(Value::Int(0)))); }
            // The socket is non-blocking, so a single write can drain only part of
            // a large buffer (a 25 KB chunk packet) — the kernel send buffer fills
            // and returns a short count or WouldBlock. With the netty event loop
            // neutralised, nothing retries the remainder, so the packet reaches the
            // client TRUNCATED and it stalls waiting for the missing bytes ("Timed
            // out"). Drain the whole buffer here: loop on short writes, briefly
            // yielding on WouldBlock until the kernel buffer frees. A disconnected
            // peer surfaces as a write error → stop. Bounded so a wedged peer can't
            // hang the tick loop forever.
            let n = match vm.sockets.get_mut(&h) {
                Some(NetSock::Stream(s)) => {
                    let mut written = 0usize;
                    let mut spins = 0u32;
                    let res = loop {
                        if written >= data.len() { break written as i32; }
                        match s.write(&data[written..]) {
                            Ok(0) => break if written > 0 { written as i32 } else { -1 },
                            Ok(k) => { written += k; spins = 0; }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                spins += 1;
                                if spins > 200_000 { break written as i32; }
                                std::thread::sleep(std::time::Duration::from_micros(20));
                            }
                            Err(_) => break if written > 0 { written as i32 } else { -1 },
                        }
                    };
                    res
                }
                _ => -1,
            };
            if n > 0 {
                let pos = vm.heap.get_field_syn(bb, crate::heap::Syn::Pos).as_int().unwrap_or(0);
                vm.heap.put_field_syn(bb, crate::heap::Syn::Pos, Value::Int(pos + n));
            }
            Ok(Some(Some(Value::Int(n.max(0)))))
        }
        // netty's NioSocketChannel reads socket options + addresses via socket().
        "socket" => {
            let s = vm.heap.alloc("java/net/Socket");
            vm.heap.put_field(s, "$channel", Value::Ref(h));
            Ok(Some(Some(Value::Ref(s))))
        }
        "getRemoteAddress" | "getLocalAddress" => {
            let isa = vm.heap.alloc("java/net/InetSocketAddress");
            vm.heap.put_field(isa, "$host", Value::Str("127.0.0.1".into()));
            vm.heap.put_field(isa, "$port", Value::Int(0));
            Ok(Some(Some(Value::Ref(isa))))
        }
        "shutdownInput" | "shutdownOutput" | "setOption" => Ok(Some(Some(Value::Ref(h)))),
        "close" => { vm.sockets.remove(&h); vm.heap.put_field(h, "$open", Value::Int(0)); Ok(Some(None)) }
        "provider" => Ok(Some(Some(Value::Ref(new_obj(vm, "java/nio/channels/spi/SelectorProvider"))))),
        _ => Ok(None),
    }
}
