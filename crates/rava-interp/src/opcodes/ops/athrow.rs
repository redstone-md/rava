//! athrow: find a handler in this frame or propagate.

use super::{guard, throw_exc};
use crate::opcodes::*;

/// `None` = keep interpreting, `Some(exit)` = leave `run`.
#[inline(always)]
/// ⚠**`#[inline(never)]` DELIBERATELY** — row 397: inlined into `opcodes::run`, this body's
/// locals are reserved by `run`'s prologue, which every interpreted call pays. `run` measured
/// a 2136-byte frame; the frame itself costs 160 ns against HotSpot's 7.2.
#[inline(never)]
pub(crate) fn athrow(
    vm: &crate::vm::thread_context::SharedVm,
    frame: &mut Frame,
    code: &Code,
    bc: &[u8],
    pc: usize,
    op: u8,
) -> Result<Option<RunExit>, VmError> {
    let _ = (bc, pc, op);
    match op {
        0xbf => { // athrow
            let mut v = frame.pop()?;
            // JVMS 6.5.athrow: "if objectref is null, athrow throws a
            // NullPointerException instead". Throwing the null itself escaped
            // every `catch` (find_handler cannot type-test a null) and killed the
            // process with "uncaught Null" — so `throw t;` on a null t, and the
            // `throw rethrowOrNull();` shape, were unrecoverable here.
            if matches!(v, Value::Null) {
                v = vm.make_exception_shared("java/lang/NullPointerException", &crate::npe::throw_null());
            }
            // Lightweight athrow trace (RAVA_TRACE_ATHROW): class/message +
            // throw site only — no call_stack bookkeeping, so the timing
            // distortion is minimal (RAVA_TRACE_THROW shifts allocation
            // patterns enough to mask timing-sensitive bugs).
            if vm.trace.athrow {
                if let Value::Ref(h) = &v {
                    let cls = vm.heap.class_of(*h).unwrap_or("?").to_string();
                    let msg = match vm.heap.get_field(*h, "$message") { Value::Str(s) => s.to_string(), _ => String::new() };
                    eprintln!("[ATHROW] {cls}: {msg} | at {} pc={}", frame.class.file.this_class, frame.pc);
                }
            }
            if crate::env_flag!("RAVA_TRACE_THROW") {
                if let Value::Ref(h) = &v {
                    let cls = vm.heap.class_of(*h).unwrap_or("?").to_string();
                    // Suppress the CrashReport.preload() warm-up storm and guava's
                    // routine BucketOverflow fallback, which otherwise flood the trace.
                    let noisy = cls.contains("BucketOverflow")
                        || crate::vm::thread_context::with_call_stack(
                            &vm.call_stack,
                            |call_stack| {
                                call_stack.iter().any(|(c, _)| c.contains("CrashReport"))
                            },
                        );
                    if !noisy {
                        let msg = match vm.heap.get_field(*h, "$message") { Value::Str(s) => s.to_string(), _ => String::new() };
                        eprintln!("[THROW] {cls}: {msg}\n{}", vm.backtrace());
                    }
                }
            }
            // CODEC-THROW INTERCEPT (RAVA_TRACE_CODEC): Wall #11 datapack hunt.
            // A registry element decode that fails throws (per-element, caught +
            // aggregated by Mojang as "Failed to parse … from pack vanilla", so
            // it never reaches the fatal hook). Dump the FIRST handful of throws
            // raised while the call stack is inside the serialization/registry
            // decode path — message + full `$cause` chain (the real codec reason)
            // + named backtrace — to name the failing codec/field and reveal any
            // Obj-vs-Ref boundary. Capped so it can't flood.
            if vm.trace.codec {
                let bt = vm.backtrace();
                let in_decode = bt.contains("serialization")
                    || bt.contains("RegistryDataLoader")
                    || bt.contains("Codec")
                    || bt.contains("Decoder")
                    || bt.contains("JsonOps")
                    || bt.contains("RegistryOps")
                    || bt.contains("DynamicOps");
                let msg = match &v {
                    Value::Ref(h) => match vm.heap.get_field(*h, "$message") { Value::Str(s) => s.to_string(), _ => String::new() },
                    _ => String::new(),
                };
                if in_decode || msg.contains("Failed to parse") {
                    use std::sync::atomic::{AtomicUsize, Ordering};
                    static N: AtomicUsize = AtomicUsize::new(0);
                    // Print the FIRST full chain+backtrace for each distinct reason
                    // family (registry-does-not-exist / cant-access / not-a-json-
                    // array / failed-to-parse) so every codec path is named once.
                    let family = if msg.contains("Registry does not exist") { 0u8 }
                        else if msg.contains("Can't access registry") { 1 }
                        else if msg.contains("Not a json") { 2 }
                        else if msg.contains("Failed to parse") { 3 }
                        else { 4 };
                    static SEEN: [AtomicUsize; 5] = [AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)];
                    let first_of_family = SEEN[family as usize].fetch_add(1, Ordering::Relaxed) == 0;
                    let n = N.fetch_add(1, Ordering::Relaxed);
                    // First occurrence of each family → full chain + backtrace; the
                    // rest → one-line head so the FULL spread is visible, no flood.
                    if first_of_family || n < 6 {
                        eprintln!(
                            "[CODEC-THROW #{n}] thrown at {} pc={}\n  exception + causes:\n{}  backtrace:\n{}",
                            frame.class.file.this_class, frame.pc,
                            vm.throwable_chain(&v), bt,
                        );
                    } else if n < 400 {
                        let head = match &v {
                            Value::Ref(h) => match vm.heap.get_field(*h, "$message") {
                                Value::Str(s) => s.split("missed input").next().unwrap_or("").trim().to_string(),
                                _ => String::new(),
                            },
                            _ => String::new(),
                        };
                        eprintln!("[CODEC-THROW #{n}] {head}");
                    }
                }
            }
            // FATAL-THROW INTERCEPT (RAVA_TRACE_FATAL): low-volume hook for the
            // boot killer. A `*Error` (LinkageError / UnsatisfiedLinkError /
            // ExceptionInInitializerError / NoClassDefFoundError / AssertionError…)
            // or any throw whose call stack is inside the dedicated-server init /
            // Bootstrap path is almost always the exception MC's top-level catch
            // logs as "Failed to start" — but the NOP logger swallows it. Dump the
            // class+message+full Java call stack here, at the throw site, before it
            // unwinds. Filtered so it does NOT flood like RAVA_TRACE_THROW.
            if std::env::var("RAVA_TRACE_FATAL").is_ok() {
                if let Value::Ref(h) = &v {
                    let cls = vm.heap.class_of(*h).unwrap_or("?").to_string();
                    let fatal_ctx = cls.ends_with("Error")
                        || crate::vm::thread_context::with_call_stack(
                            &vm.call_stack,
                            |call_stack| {
                                call_stack.iter().any(|(c, _)| {
                                    c.ends_with("DedicatedServerProperties")
                                        || c.contains("DedicatedServer")
                                        || c.contains("Bootstrap")
                                        || c.contains("/Main")
                                })
                            },
                        );
                    if fatal_ctx {
                        let msg = match vm.heap.get_field(*h, "$message") {
                            Value::Str(s) => s.to_string(),
                            _ => String::new(),
                        };
                        eprintln!("[FATAL-THROW] {cls}: {msg}\n{}", vm.frame_trace());
                    }
                }
            }
            // RAVA_TRACE_MSG=<substr> — dump class+message+backtrace for any throw
            // whose message contains <substr>. Targets a known killer (e.g. a specific
            // `arraylength` NPE) without the RAVA_TRACE_THROW flood or the FATAL stack
            // gate. First match per substr only (one-shot), so it can't loop-spam.
            if let Ok(want) = std::env::var("RAVA_TRACE_MSG") {
                if let Value::Ref(h) = &v {
                    let msg = match vm.heap.get_field(*h, "$message") {
                        Value::Str(s) => s.to_string(),
                        _ => String::new(),
                    };
                    if !want.is_empty() && msg.contains(&want) {
                        let cls = vm.heap.class_of(*h).unwrap_or("?").to_string();
                        eprintln!("[TRACE-MSG] {cls}: {msg}\n{}", vm.frame_trace());
                    }
                }
            }
            // Throws during network handling — either inside the inbound pump,
            // or thrown directly from a netty/packet/login/play class on the
            // main-tick outbound path (LoginListener.c() sending LoginSuccess/
            // JoinGame). These are the ones netty swallows in exceptionCaught;
            // tracing just them surfaces the login/play blocker without the full
            // throw-trace flooding/slowing the boot.
            if std::env::var("RAVA_TRACE_NET").is_ok() {
                let net_ctx = vm.in_pump || {
                    let c = frame.class.file.this_class.as_str();
                    c.contains("netty") || c.contains("Packet") || c.contains("Login")
                        || c.contains("NetworkManager") || c.contains("PlayerConnection")
                        || c.contains("PlayerList") || c.contains("LoginListener")
                };
                if net_ctx {
                    if let Value::Ref(h) = &v {
                        let cls = vm.heap.class_of(*h).unwrap_or("?").to_string();
                        let msg = match vm.heap.get_field(*h, "$message") { Value::Str(s) => s.to_string(), _ => String::new() };
                        eprintln!("[NET-THROW] {cls}: {msg}\n{}", vm.backtrace());
                    }
                }
            }
            match find_handler(vm, frame, code, pc, &v)? {
                Some(h) => { frame.clear_stack(); frame.push(v); frame.pc = h; }
                None => return Err(VmError::Throw(v)),
            }
        }
        _ => return Err(VmError::UnknownOpcode(op)),
    }
    Ok(None)
}
