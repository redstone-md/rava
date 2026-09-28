//! java.nio networking + netty native-transport glue.
//!
//! Backed by real OS sockets (`std::net`): a `ServerSocketChannel.bind` opens a
//! genuine `TcpListener`, so the dedicated server truly holds the port. The
//! interpreter is single-threaded, so netty's event loop is run *inline*:
//! `inEventLoop()` reports true (see `force_native_collection`), which makes
//! netty execute channel registration/bind directly on the calling thread
//! instead of handing them to a background selector thread that would never
//! return. The blocking accept loop is therefore not pumped here yet — that
//! arrives with real threads — but the listener is bound, so boot reaches Done.



use crate::value::Value;
use crate::{Vm, VmError};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, SocketAddr};

type Handled = Result<Option<Option<Value>>, VmError>;

/// OS socket object backing a java.nio channel, owned by `Vm::sockets`.
pub enum NetSock {
    Listener(TcpListener),
    Stream(TcpStream),
}

const OP_READ: i32 = 1 << 0;
const OP_WRITE: i32 = 1 << 2;
const OP_ACCEPT: i32 = 1 << 4;

/// ⭐**THE NETTY EVENT-EXECUTOR SEMANTIC PROVIDER TABLE — the structural replacement
/// for the `io/netty/` `inEventLoop` prefix arm of `force_native_collection`.**
///
/// A full-classpath census (40 jars, 29,760 effective classes, 71 `inEventLoop`
/// method refs across 12 ask owners) proved the prefix population is EXACTLY the
/// `EventExecutorGroup` closure: 22 instance classes, every one carrying the root in
/// its type closure, and zero non-members declaring or exposing `inEventLoop`. The
/// single old-prefix ask-owner divergence is the exact row below — `SslHandler`'s
/// private static `inEventLoop(Ljava/util/concurrent/Executor;)Z` (one Methodref,
/// one call site in `runDelegatedTasks`), licensed as a genuine semantic provider:
/// it asks the same thread-identity question the inline event-loop model answers
/// true, and its exact identity is encodable as a tuple. This table is the semantic
/// provider's home — never a workload allowlist.
pub(crate) const EVENT_EXECUTOR_GROUP_ROOT: &str = "io/netty/util/concurrent/EventExecutorGroup";
/// The licensed exact provider outside the closure — owner half of the tuple. Owner
/// uniqueness for `inEventLoop` on this classpath was established by the census, so
/// the owner compare alone settles the gate side (which sees no descriptor).
pub(crate) const SSL_HANDLER_OWNER: &str = "io/netty/handler/ssl/SslHandler";
/// The descriptor half of the exact tuple: `(Executor)Z`, private static. The
/// dispatch side sees descriptors, so it pins the full tuple.
pub(crate) const SSL_HANDLER_INEVENTLOOP_DESC: &str = "(Ljava/util/concurrent/Executor;)Z";
/// The exact `ThreadPerTaskExecutor` execute row — owner half of the tuple. The
/// static census proved every `execute` ask owner, so the owner compare is the
/// identity.
pub(crate) const THREAD_PER_TASK_EXECUTOR: &str = "io/netty/util/concurrent/ThreadPerTaskExecutor";
/// The descriptor half of the exact execute tuple: TPTX's `execute(Runnable;)V`,
/// the thread-start swallow. The dispatch side pins the full tuple.
pub(crate) const THREAD_PER_TASK_EXECUTE_DESC: &str = "(Ljava/lang/Runnable;)V";
/// The two `execute` descriptors the census saw on family owners; every other
/// shape on a family member is a different overload and not the provider's.
pub(crate) const EXECUTE_DESCS: &[&str] = &["(Ljava/lang/Runnable;)V", "(Ljava/lang/Runnable;Z)V"];

/// GATE predicate — `force_native_collection`'s structural `inEventLoop` verdict
/// (name + loader closure + the exact Ssl owner; the gate sees no descriptor).
///
/// GIL-FREE WORKERS cannot load, so their `type_closure` can be a fragment — and a
/// fragment can only ever MISS the root, never contain it wrongly. The conservative
/// arm therefore answers false for every ask the exact owner compare above did not
/// settle — the only verdict that cannot be wrong without the loader; the structural
/// answer only ever narrows the owning thread's decision.
pub(crate) fn gate_in_event_loop(vm: &Vm, owner: &str, name: &str) -> bool {
    if name != "inEventLoop" {
        return false;
    }
    // Pure name compare — decidable on every thread, closure not needed.
    if owner == SSL_HANDLER_OWNER {
        return true;
    }
    if crate::parallel::gilfree() {
        return false;
    }
    vm.loader.is_netty_executor_family(owner)
}

/// DISPATCH predicate — the same family question where the descriptor IS in scope.
/// `net::dispatch` runs on the interpreter thread, never a GIL-free worker, so the
/// closure read here is always complete.
pub(crate) fn dispatch_in_event_loop(vm: &Vm, owner: &str, name: &str, descriptor: &str) -> bool {
    if name != "inEventLoop" {
        return false;
    }
    if owner == SSL_HANDLER_OWNER {
        return descriptor == SSL_HANDLER_INEVENTLOOP_DESC;
    }
    vm.loader.is_netty_executor_family(owner)
}

/// EXECUTE-ACTION predicate — the licensed exact TPTX tuple plus the family
/// closure question, pure over its inputs. The DISPATCH side reads the closure
/// (interpreter thread only, so it is complete); the GATE must settle gil-freeness
/// itself and consults this for the nonexact remainder — see `force_native_collection`.
/// Every decline is a real decline: `net::dispatch` falls through untouched, and the
/// arms below return the resolver's answer.
pub(crate) fn dispatch_execute_action(vm: &Vm, owner: &str, name: &str, descriptor: &str) -> ExecuteAction {
    if name != "execute" {
        return ExecuteAction::Decline;
    }
    // The exact TPTX tuple — the only swallow. Owner identity is the whole test:
    // the census proved owner uniqueness for `execute` on this classpath.
    if owner == THREAD_PER_TASK_EXECUTOR {
        return if descriptor == THREAD_PER_TASK_EXECUTE_DESC {
            ExecuteAction::Swallow
        } else {
            ExecuteAction::Decline
        };
    }
    // Every other row is the structural family question plus one of the two
    // census descriptors — the task applies inline.
    if vm.loader.is_netty_executor_family(owner) && EXECUTE_DESCS.contains(&descriptor) {
        return ExecuteAction::InlineApply;
    }
    ExecuteAction::Decline
}

/// What to do with an `execute` ask — the one definition, consumed by both seams.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ExecuteAction {
    /// Not this provider's ask; the caller declines untouched.
    Decline,
    /// The exact TPTX tuple: the event-loop thread start is swallowed (the inline
    /// model runs every task on the caller — the loop thread must never spawn).
    Swallow,
    /// A family member's execute(Runnable[,Z]): the task is applied INLINE (our
    /// loop never runs, so a queued task — the deferred channel bind — would hang).
    InlineApply,
}
pub(crate) fn dispatch(vm: &mut Vm, owner: &str, name: &str, descriptor: &str, args: &[Value]) -> Handled {
    // (FileIOThread's async chunk-save daemon now runs for real as a cooperative
    // green thread — see Vm::spawn_green_thread — so its run()/b() loops execute
    // instead of being neutralised. No MC-class-specific interception here.)
    // Netty: report the current thread as the event-loop thread so registration
    // and bind run inline (no background selector thread to deadlock on). The
    // `inEventLoop` arm is hoisted ABOVE the `io/netty/` prefix guard: its
    // population is structural (the `EventExecutorGroup` closure plus the exact
    // SslHandler tuple — the provider table above), so a non-prefix implementor
    // the gate routed is served here rather than dropped into the resolver.
    if dispatch_in_event_loop(vm, owner, name, descriptor) {
        return Ok(Some(Some(Value::Int(1))));
    }
    // `execute` — the second structural provider (the exact TPTX tuple + the
    // family closure), hoisted above the `io/netty/` prefix guard for the same
    // reason: a non-prefix `EventExecutorGroup` implementor the gate routed is
    // served here, not dropped into the resolver.
    match dispatch_execute_action(vm, owner, name, descriptor) {
        ExecuteAction::Swallow => return Ok(Some(None)),
        ExecuteAction::InlineApply => {
            // The task may be a heap `Value::Ref` lambda OR — under the arena GC —
            // a `Value::Obj` (e.g. netty's doBind0 / register0 Runnable). The old
            // `Ref`-only guard silently DROPPED an arena task, so the bind future
            // never completed; `await()` then saw it un-done and `checkDeadLock()`
            // threw `BlockingOperationException` (inEventLoop()=true). `apply`
            // dispatches the SAM for both representations.
            if let Some(r) = args.get(1).cloned() {
                if matches!(r, Value::Ref(_) | Value::Obj(_)) { vm.apply(&r, vec![])?; }
            }
            return Ok(Some(None));
        }
        ExecuteAction::Decline => {}
    }
    if owner.starts_with("io/netty/") {
        match name {
            "run" if owner.contains("EventLoop") || owner.contains("EventExecutor") || owner.ends_with("IoHandler") => {
                // Neutralise the infinite event-loop / IoHandler `run()V` poll
                // loop. But a NioEventLoop$N is a PrivilegedAction whose
                // `run()Ljava/lang/Void;` RETURNS a value (its synthetic
                // `run()Object` bridge does areturn) — force_native routes it here
                // by class name, so run its real (short, side-effect-only)
                // bytecode and return the value instead of leaving the bridge's
                // stack empty (operand-stack underflow at the areturn).
                if descriptor == "()V" {
                    return Ok(Some(None));
                }
                if let Some(Value::Ref(h)) = args.first() {
                    let cls = vm.heap.class_of(*h).unwrap_or(owner).to_string();
                    if let Some(decl) = vm.resolve_method(&cls, "run", descriptor)? {
                        let r = vm.invoke(decl, "run", descriptor, vec![Value::Ref(*h)])?;
                        return Ok(Some(Some(r.unwrap_or(Value::Null))));
                    }
                }
                return Ok(Some(Some(Value::Null)));
            }
            _ => return Ok(None),
        }
    }
    match owner {
        "java/nio/ByteOrder" if name == "nativeOrder" => {
            // x86/x64 — little-endian.
            let h = vm.heap.alloc("java/nio/ByteOrder");
            vm.heap.put_field_syn(h, crate::heap::Syn::Name, Value::Str("LITTLE_ENDIAN".into()));
            Ok(Some(Some(Value::Ref(h))))
        }
        "java/net/NetworkInterface" => match name {
            // Empty enumeration -> netty's MacAddressUtil falls back to a random MAC.
            "getNetworkInterfaces" => {
                let it = vm.heap.alloc("java/util/Iterator");
                let empty = vm.heap.alloc_values("Ljava/lang/Object;", Vec::new());
                vm.heap.put_field_syn(it, crate::heap::Syn::Elems, Value::Array(empty));
                vm.heap.put_field_syn(it, crate::heap::Syn::Pos, Value::Int(0));
                Ok(Some(Some(Value::Ref(it))))
            }
            "getByInetAddress" | "getByName" | "getByIndex" => Ok(Some(Some(Value::Null))),
            "getHardwareAddress" => Ok(Some(Some(Value::Null))),
            _ => Ok(None),
        },
        "java/net/ServerSocket" | "java/net/Socket" => server_socket_adapter(vm, name, args),
        "java/net/InetSocketAddress" => inet_socket_address(vm, name, descriptor, args),
        "java/nio/channels/spi/SelectorProvider" => selector_provider(vm, name, args),
        "java/nio/channels/Selector"
        | "java/nio/channels/spi/AbstractSelector" => selector(vm, name, args),
        "java/nio/channels/ServerSocketChannel"
        | "java/nio/channels/spi/AbstractSelectableChannel"
        | "java/nio/channels/spi/AbstractInterruptibleChannel"
        | "java/nio/channels/SelectableChannel" => server_socket_channel(vm, name, descriptor, args),
        "java/nio/channels/SocketChannel" => socket_channel(vm, name, args),
        "java/nio/channels/SelectionKey"
        | "java/nio/channels/spi/AbstractSelectionKey" => selection_key(vm, name, args),
        _ => Ok(None),
    }
}

fn recv(args: &[Value]) -> Option<u32> {
    match args.first() { Some(Value::Ref(h)) => Some(*h), _ => None }
}

fn new_obj(vm: &mut Vm, class: &str) -> u32 { vm.heap.alloc(class) }

fn selector_provider(vm: &mut Vm, name: &str, _args: &[Value]) -> Handled {
    match name {
        "provider" => Ok(Some(Some(Value::Ref(new_obj(vm, "java/nio/channels/spi/SelectorProvider"))))),
        "openSelector" => Ok(Some(Some(Value::Ref(new_selector(vm))))),
        "openServerSocketChannel" => Ok(Some(Some(Value::Ref(new_obj(vm, "java/nio/channels/ServerSocketChannel"))))),
        "openSocketChannel" => Ok(Some(Some(Value::Ref(new_obj(vm, "java/nio/channels/SocketChannel"))))),
        "inheritedChannel" => Ok(Some(Some(Value::Null))),
        _ => Ok(None),
    }
}

fn new_selector(vm: &mut Vm) -> u32 {
    let s = vm.heap.alloc("java/nio/channels/Selector");
    // registered keys live in $elems (a plain Object[] set we grow on register)
    let empty = vm.heap.alloc_values("Ljava/lang/Object;", Vec::new());
    vm.heap.put_field(s, "$keys", Value::Array(empty));
    // Track for per-tick pumping (the netty poll loop is neutralised).
    if !vm.net_selectors.contains(&s) { vm.net_selectors.push(s); }
    s
}

/// Drive networking one iteration: netty's event-loop poll is neutralised, so on
/// each main-thread tick we poll every selector and, for a ready channel, invoke
/// its netty `Channel.unsafe().read()` (accept on a server channel, byte read on
/// a socket). The SelectionKey's attachment is the netty channel (netty registers
/// `javaChannel.register(sel, 0, niochannel)`). `execute()` runs inline here, so
/// the accepted child registers + the inbound pipeline (decoders → MC packet
/// handlers) all run synchronously on this thread.
pub(crate) fn pump_network(vm: &mut Vm) {
    // A yield is the guest giving up the calling thread — the moment deferred bodies are allowed
    // to run. During loading the main thread never gets here, which is exactly why a deferred body
    // lands after `Done`. Errors are swallowed: a deferred supplier completes its future
    // exceptionally rather than propagating into the network pump.
    let _ = crate::reflect::drain_parked(vm);
    if vm.net_selectors.is_empty() { return; }
    let selectors = vm.net_selectors.clone();
    for sel in selectors {
        let ready = poll_selector(vm, sel);
        for (key, ops) in ready {
            if ops & (OP_ACCEPT | OP_READ) == 0 { continue; }
            // Attachment = the netty channel; call channel.unsafe().read().
            let chan = match vm.heap.get_field(key, "$att") { Value::Ref(c) => c, _ => continue };
            if let Err(e) = drive_channel_read(vm, chan) {
                if std::env::var("RAVA_TRACE_NET").is_ok() {
                    eprintln!("[NET] unsafe().read() failed: {e}");
                }
            }
        }
    }
}

/// Invoke `nettyChannel.unsafe().read()` to drive an accept or a byte read.
fn drive_channel_read(vm: &mut Vm, chan: u32) -> Result<(), VmError> {
    let cls = match vm.heap.class_of(chan) { Some(c) => c.to_string(), None => return Ok(()) };
    // unsafe() -> Channel$Unsafe
    let unsafe_obj = match vm.resolve_method(&cls, "unsafe", "()Lio/netty/channel/Channel$Unsafe;")? {
        Some(decl) => vm.invoke(decl, "unsafe", "()Lio/netty/channel/Channel$Unsafe;", vec![Value::Ref(chan)])?,
        None => return Ok(()),
    };
    let unsafe_h = match unsafe_obj { Some(Value::Ref(h)) => h, _ => return Ok(()) };
    let ucls = match vm.heap.class_of(unsafe_h) { Some(c) => c.to_string(), None => return Ok(()) };
    if let Some(decl) = vm.resolve_method(&ucls, "read", "()V")? {
        vm.invoke(decl, "read", "()V", vec![Value::Ref(unsafe_h)])?;
    }
    Ok(())
}

fn empty_set(vm: &mut Vm) -> u32 {
    let set = vm.heap.alloc("java/util/LinkedHashSet");
    let empty = vm.heap.alloc_values("Ljava/lang/Object;", Vec::new());
    vm.heap.put_field_syn(set, crate::heap::Syn::Elems, Value::Array(empty));
    set
}

/// Free space (bytes) in a ByteBuffer: `$limit - $pos` clamped to capacity.
fn buffer_room(vm: &Vm, bb: u32) -> usize {
    let cap = match vm.heap.get_field(bb, "$buf") { Value::Array(a) => vm.heap.array_len(a).unwrap_or(0), _ => 0 };
    let pos = vm.heap.get_field_syn(bb, crate::heap::Syn::Pos).as_int().unwrap_or(0).max(0) as usize;
    let lim = vm.heap.get_field(bb, "$limit").as_int().unwrap_or(0).max(0) as usize;
    lim.min(cap).saturating_sub(pos)
}

fn selection_key(vm: &mut Vm, name: &str, args: &[Value]) -> Handled {
    let h = match recv(args) { Some(h) => h, None => return Ok(None) };
    match name {
        "interestOps" if args.len() > 1 => { vm.heap.put_field(h, "$interestOps", args[1].clone()); Ok(Some(Some(Value::Ref(h)))) }
        "interestOps" => Ok(Some(Some(vm.heap.get_field(h, "$interestOps")))),
        "readyOps" => Ok(Some(Some(vm.heap.get_field(h, "$readyOps")))),
        "channel" => Ok(Some(Some(vm.heap.get_field(h, "$channel")))),
        "selector" => Ok(Some(Some(vm.heap.get_field(h, "$selector")))),
        "attach" => { let old = vm.heap.get_field(h, "$att"); vm.heap.put_field(h, "$att", args.get(1).cloned().unwrap_or(Value::Null)); Ok(Some(Some(old))) }
        "attachment" => Ok(Some(Some(vm.heap.get_field(h, "$att")))),
        "isValid" => Ok(Some(Some(Value::Int(1)))),
        "isAcceptable" => Ok(Some(Some(Value::Int((vm.heap.get_field(h, "$readyOps").as_int().unwrap_or(0) & OP_ACCEPT != 0) as i32)))),
        "isReadable" => Ok(Some(Some(Value::Int((vm.heap.get_field(h, "$readyOps").as_int().unwrap_or(0) & OP_READ != 0) as i32)))),
        "isWritable" => Ok(Some(Some(Value::Int((vm.heap.get_field(h, "$readyOps").as_int().unwrap_or(0) & OP_WRITE != 0) as i32)))),
        "isConnectable" => Ok(Some(Some(Value::Int(0)))),
        "cancel" => Ok(Some(None)),
        _ => Ok(None),
    }
}

/// Extract (host, port) from a java.net.InetSocketAddress heap object.
fn addr_of(vm: &Vm, v: Option<&Value>) -> (String, i32) {
    if let Some(Value::Ref(a)) = v {
        let host = match vm.heap.get_field(*a, "$host") { Value::Str(s) => s.to_string(), _ => "0.0.0.0".into() };
        let port = vm.heap.get_field(*a, "$port").as_int().unwrap_or(0);
        let host = if host.is_empty() || host == "0.0.0.0" { "0.0.0.0".into() } else { host };
        return (host, port);
    }
    ("0.0.0.0".into(), 0)
}
mod channels;
pub(crate) use channels::*;
mod selector;
pub(crate) use selector::*;

/// The exact-provider tuple and the closure predicate, pinned so a table edit cannot
/// drift past the seams that consume it (the census's 22 closure rows are covered by
/// the synthetic-subclass shape every one of them has: `EventExecutorGroup` in its
/// type closure).
#[cfg(test)]
mod eventloop_provider_tests {
    use super::*;

    fn vm() -> Vm {
        Vm::new(crate::class_loader::ClassLoader::new())
    }

    #[test]
    fn ssl_handler_tuple_is_exact() {
        let vm = vm();
        // Gate side: owner uniqueness — no descriptor reaches it.
        assert!(gate_in_event_loop(&vm, SSL_HANDLER_OWNER, "inEventLoop"));
        assert!(!gate_in_event_loop(&vm, SSL_HANDLER_OWNER, "inEventLoopX"));
        // Dispatch side: the descriptor pins the tuple.
        assert!(dispatch_in_event_loop(&vm, SSL_HANDLER_OWNER, "inEventLoop", SSL_HANDLER_INEVENTLOOP_DESC));
        assert!(!dispatch_in_event_loop(&vm, SSL_HANDLER_OWNER, "inEventLoop", "()Z"));
        // A non-member with the same name is neither predicate's business.
        assert!(!gate_in_event_loop(&vm, "com/example/NotNetty", "inEventLoop"));
        assert!(!dispatch_in_event_loop(&vm, "com/example/NotNetty", "inEventLoop", "()Z"));
    }

    #[test]
    fn closure_rows_route_at_both_seams() {
        let vm = vm();
        // A class whose super IS the root (an unloadable terminal name) — the same
        // shape as every census closure row, provable from a synthetic classfile.
        let file = rava_classfile::ClassFile::synthesize(
            rava_classfile::ConstantPool::from_entries(vec![rava_classfile::Constant::Unusable]),
            0x0021,
            "Syn/EventLoopRow".to_string(),
            Some(EVENT_EXECUTOR_GROUP_ROOT.to_string()),
            vec![],
            Vec::<rava_classfile::MemberInfo>::new(),
        );
        vm.loader.define_synthetic(file);
        assert!(gate_in_event_loop(&vm, "Syn/EventLoopRow", "inEventLoop"));
        // Both instance descriptors the census saw are served identically.
        assert!(dispatch_in_event_loop(&vm, "Syn/EventLoopRow", "inEventLoop", "()Z"));
        assert!(dispatch_in_event_loop(&vm, "Syn/EventLoopRow", "inEventLoop", "(Ljava/lang/Thread;)Z"));
    }
    #[test]
    fn nonprefix_implementor_is_served_and_unrelated_falls_through() {
        let mut vm = vm();
        let file = rava_classfile::ClassFile::synthesize(
            rava_classfile::ConstantPool::from_entries(vec![rava_classfile::Constant::Unusable]),
            0x0021,
            "Syn/NonprefixLoop".to_string(),
            Some(EVENT_EXECUTOR_GROUP_ROOT.to_string()),
            vec![],
            Vec::<rava_classfile::MemberInfo>::new(),
        );
        vm.loader.define_synthetic(file);
        // The hoisted arm: a non-io/netty implementor the gate routed is SERVED
        // Int(1) here, not dropped into the resolver.
        let Ok(r) = dispatch(&mut vm, "Syn/NonprefixLoop", "inEventLoop", "()Z", &[]) else {
            panic!("dispatch errored");
        };
        assert!(matches!(r, Some(Some(Value::Int(1)))));
        // The exact Ssl tuple is served; a wrong descriptor is not the tuple and
        // falls through to the resolver.
        let Ok(r) = dispatch(&mut vm, SSL_HANDLER_OWNER, "inEventLoop", SSL_HANDLER_INEVENTLOOP_DESC, &[]) else {
            panic!("dispatch errored");
        };
        assert!(matches!(r, Some(Some(Value::Int(1)))));
        let Ok(r) = dispatch(&mut vm, SSL_HANDLER_OWNER, "inEventLoop", "()Z", &[]) else {
            panic!("dispatch errored");
        };
        assert!(matches!(r, None));
        // An unrelated class asking the same method name is nobody's business.
        let Ok(r) = dispatch(&mut vm, "com/example/NotNetty", "inEventLoop", "()Z", &[]) else {
            panic!("dispatch errored");
        };
        assert!(matches!(r, None));
    }

    /// A synthetic `EventExecutorGroup` implementor living OUTSIDE `io/netty/` —
    /// the same shape as every census closure row, so both structural providers
    /// serve it without any package test.
    fn nonprefix_family(vm: &mut Vm) {
        let file = rava_classfile::ClassFile::synthesize(
            rava_classfile::ConstantPool::from_entries(vec![rava_classfile::Constant::Unusable]),
            0x0021,
            "Syn/ExecFamily".to_string(),
            Some(EVENT_EXECUTOR_GROUP_ROOT.to_string()),
            vec![],
            Vec::<rava_classfile::MemberInfo>::new(),
        );
        vm.loader.define_synthetic(file);
    }

    #[test]
    fn execute_action_is_exact_and_descriptor_pinned() {
        let mut vm = vm();
        // The exact TPTX tuple — the only swallow, and only for its one descriptor.
        assert_eq!(
            dispatch_execute_action(&vm, THREAD_PER_TASK_EXECUTOR, "execute", THREAD_PER_TASK_EXECUTE_DESC),
            ExecuteAction::Swallow
        );
        assert_eq!(
            dispatch_execute_action(&vm, THREAD_PER_TASK_EXECUTOR, "execute", "(Ljava/lang/Runnable;J)V"),
            ExecuteAction::Decline,
            "the exact owner only swallows the census descriptor"
        );
        assert_eq!(
            dispatch_execute_action(&vm, THREAD_PER_TASK_EXECUTOR, "executeX", THREAD_PER_TASK_EXECUTE_DESC),
            ExecuteAction::Decline
        );
        // Nothing else is the exact owner, whatever its name or package.
        assert_eq!(
            dispatch_execute_action(&vm, "io/netty/util/concurrent/DefaultEventExecutor", "execute", THREAD_PER_TASK_EXECUTE_DESC),
            ExecuteAction::Decline,
            "a non-family owner is not the exact row, even under io/netty/"
        );
    }

    #[test]
    fn execute_family_admits_both_census_descriptors_only() {
        let mut vm = vm();
        nonprefix_family(&mut vm);
        for d in EXECUTE_DESCS {
            assert_eq!(
                dispatch_execute_action(&vm, "Syn/ExecFamily", "execute", d),
                ExecuteAction::InlineApply,
                "family member's census descriptor {d} applies inline"
            );
        }
        // A different shape on the same member is not this provider's ask.
        assert_eq!(
            dispatch_execute_action(&vm, "Syn/ExecFamily", "execute", "(Ljava/lang/Runnable;J)V"),
            ExecuteAction::Decline,
            "a wrong descriptor on a family member declines"
        );
        // An unrelated owner with a census descriptor is still nobody's business.
        assert_eq!(
            dispatch_execute_action(&vm, "com/example/NotNetty", "execute", THREAD_PER_TASK_EXECUTE_DESC),
            ExecuteAction::Decline
        );
        // An unloaded name declines the same way — an inconclusive closure is
        // never a family member.
        assert_eq!(
            dispatch_execute_action(&vm, "Syn/NeverDefined", "execute", THREAD_PER_TASK_EXECUTE_DESC),
            ExecuteAction::Decline
        );
    }

    #[test]
    fn nonprefix_execute_reaches_dispatch_and_wrongs_decline() {
        let mut vm = vm();
        nonprefix_family(&mut vm);
        // The hoisted provider: a non-io/netty family member's execute reaches
        // the inline-apply arm. No task argument, so the arm is a pure Swallow
        // of the ask — the predicate action is checked directly above.
        let Ok(r) = dispatch(&mut vm, "Syn/ExecFamily", "execute", THREAD_PER_TASK_EXECUTE_DESC, &[]) else {
            panic!("dispatch errored");
        };
        assert!(matches!(r, Some(None)), "family execute with no task arg returns void");
        // The exact TPTX swallow.
        let Ok(r) = dispatch(&mut vm, THREAD_PER_TASK_EXECUTOR, "execute", THREAD_PER_TASK_EXECUTE_DESC, &[]) else {
            panic!("dispatch errored");
        };
        assert!(matches!(r, Some(None)), "exact TPTX tuple swallows");
        // Wrong descriptors and unrelated owners decline to the resolver.
        let Ok(r) = dispatch(&mut vm, THREAD_PER_TASK_EXECUTOR, "execute", "(Ljava/lang/Runnable;J)V", &[]) else {
            panic!("dispatch errored");
        };
        assert!(matches!(r, None), "wrong descriptor on the exact owner declines");
        let Ok(r) = dispatch(&mut vm, "com/example/NotNetty", "execute", THREAD_PER_TASK_EXECUTE_DESC, &[]) else {
            panic!("dispatch errored");
        };
        assert!(matches!(r, None), "unrelated owner declines");
    }

    #[test]
    fn execute_gate_routes_exact_and_family_gilfree_conservatively() {
        let mut vm = vm();
        nonprefix_family(&mut vm);
        // GIL-FREE POLICY: the worker's closure can only MISS the root, so a
        // nonexact ask must answer false there — the only sound verdict without
        // the loader. The exact TPTX row is a pure name+descriptor compare, so it
        // stays decidable on every thread.
        crate::parallel::set_gilfree(true);
        assert!(
            crate::opcodes::force_native_collection(&vm, THREAD_PER_TASK_EXECUTOR, "execute", THREAD_PER_TASK_EXECUTE_DESC),
            "the exact tuple is decidable without the loader"
        );
        assert!(
            !crate::opcodes::force_native_collection(&vm, "Syn/ExecFamily", "execute", THREAD_PER_TASK_EXECUTE_DESC),
            "a gil-free worker conservatively declines a family ask"
        );
        crate::parallel::set_gilfree(false);
        // Owning thread: the structural answer holds for the family member, and
        // the gate declines what the provider declines.
        assert!(
            crate::opcodes::force_native_collection(&vm, "Syn/ExecFamily", "execute", THREAD_PER_TASK_EXECUTE_DESC),
            "the owning thread routes the family member's execute"
        );
        assert!(
            !crate::opcodes::force_native_collection(&vm, THREAD_PER_TASK_EXECUTOR, "execute", "(Ljava/lang/Runnable;J)V"),
            "the exact owner's wrong descriptor stays bytecode"
        );
        assert!(
            !crate::opcodes::force_native_collection(&vm, "com/example/NotNetty", "execute", THREAD_PER_TASK_EXECUTE_DESC),
            "an unrelated owner's execute stays bytecode"
        );
    }
}
