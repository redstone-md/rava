//! The interpreter loops.

use crate::*;

/// Pop the top explicit-stack frame and RETURN ITS TWO BUFFERS TO THE POOL.
///
/// `Frame` has no `Drop`, so a bare `frames.pop()` frees its `locals` and `stack` `Vec`s — while
/// every trampolined call takes three buffers OUT of the per-thread pool (`pop_args_n`, and twice
/// in `build_frame`). That is two allocations and two frees per call which the recursive machine
/// does not pay: `invoke_idx` does exactly this `mem::take` + `give_vec` pair on the way out.
///
/// No `clear()`, deliberately: the pool clears on the way OUT (`frames::pool_take`), and the
/// recursive machine hands back uncleared buffers too. Making these two paths differ there would
/// be a change to when the previous callee's values are dropped.
fn recycle_frame(vm: &mut Vm, frame: Frame) {
    let mut frame = frame;
    // ONE buffer to recycle instead of two (stage 1 of the contiguous stack).
    let locals = std::mem::take(&mut frame.buf);
    let stack: Vec<crate::Value> = Vec::new();
    vm.give_vec(locals);
    vm.give_vec(stack);
}

fn run_stackful_frame(
    vm: &mut Vm,
    frame: &mut Frame,
    name: &str,
    descriptor: &str,
) -> Result<opcodes::RunExit, VmError> {
    if frame.code.is_null() {
        return Err(VmError::NoCode(format!("{name}{descriptor}")));
    }
    // SAFETY: the Code lives in the class held by this activation.
    let code: &Code = unsafe { &*frame.code };

    // Offer only fresh, handler-free GC leaves to machine code. The structural
    // predicate excludes guest calls and explicit allocation/class-load ops;
    // any pending Java exception returns as VmError for driver-side unwinding.
    #[cfg(feature = "next-gen-gc")]
    if vm.jit_enabled
        && frame.pc == 0
        && frame.stack_len() == 0
        && code.exceptions.is_empty()
        && crate::jit::is_gc_leaf(&code.bytecode)
    {
        if let Some(method) = frame.class.file.method(name, descriptor) {
            let is_static = method.access_flags & 0x0008 != 0;
            if let Some(result) = vm.jit_run(
                code,
                name,
                descriptor,
                is_static,
                &frame.class,
                frame.locals(),
            ) {
                return result.map(opcodes::RunExit::Normal);
            }
        }
    }

    opcodes::run(vm, code, frame)
}

impl Vm {
    pub(crate) fn drive_stack(
        &mut self,
        mut frames: Vec<(Frame, Arc<str>, Arc<str>)>,
    ) -> Result<Option<Value>, VmError> {
        let checkpoint_at: Option<u64> = std::env::var("RAVA_CHECKPOINT_OPS")
            .ok()
            .and_then(|s| s.trim().parse().ok());
        // Deterministic checkpoint trigger: freeze when this method is entered
        // (CRaC-style explicit checkpoint point), independent of op count.
        let checkpoint_method: Option<String> = std::env::var("RAVA_CHECKPOINT_METHOD").ok();
        // HOISTED. This was read inside the loop — an environment lookup at EVERY frame boundary,
        // i.e. on every call and every return, for a probe nobody arms.
        let ckpt_debug = std::env::var_os("RAVA_CKPT_DEBUG").is_some();
        // The global registry exposes this stack to GC; the thread-local scope
        // also gives diagnostics the correct nested stack for this Vm.
        let owner = self as *const Self as usize;
        let _exec_scope = crate::frames::ExecScope::new(owner, std::ptr::addr_of!(frames) as usize);
        loop {
            // Frame-boundary checkpoint: the whole stack is at clean opcode
            // boundaries here, so it serializes + resumes deterministically.
            // RAVA_CKPT_DEBUG: log each boundary's op-count + top frame (sampled) so a
            // never-firing trigger is diagnosable (e.g. the whole phase running inside
            // one native-reentry dive with no main-trampoline boundary).
            if ckpt_debug {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n < 10 || n % 1_000 == 0 || self.op_total > 150_000_000 {
                    if let Some((_, fname, _)) = frames.last() {
                        eprintln!(
                            "[CKPT-B] boundary#{n} ops={}M frames={} top={fname}",
                            self.op_total / 1_000_000,
                            frames.len()
                        );
                    }
                }
            }
            if !self.checkpointed {
                let by_method = checkpoint_method
                    .as_deref()
                    .map(|m| frames.last().is_some_and(|(_, n, _)| n.as_ref() == m))
                    .unwrap_or(false);
                let by_ops = checkpoint_at.is_some_and(|thr| self.op_total >= thr);
                if by_method || by_ops {
                        // Collect first so the checkpoint captures the LIVE set, not
                        // accumulated garbage — restore time scales with object count.
                        self.gc();
                        let _ = self.write_checkpoint(&frames);
                        self.checkpointed = true;
                        eprintln!(
                            "[CHECKPOINT] wrote running state at {}M ops ({} frames)",
                            self.op_total / 1_000_000,
                            frames.len()
                        );
                        return Ok(None);
                    }
            }
            // Run the top frame (the Code is re-fetched each step so no borrow is
            // held across the mutable `run`).
            let exit = {
                let (frame, fname, fdesc) = frames.last_mut().unwrap();
                // THE FRAME KNOWS ITS BODY. This used to clone the class `Arc` and then look the
                // method up BY NAME — a hash and a descriptor compare across the overload set —
                // every time control came back to a frame, which is once per call AND once per
                // return. `Frame::new` was handed the `Code` when the frame was built.
                run_stackful_frame(self, frame, fname, fdesc)
            };
            match exit {
                Ok(opcodes::RunExit::Normal(v)) => {
                    // A deferred (trampolined) <clinit> frame just finished → its
                    // statics are now populated. Mark clinit_done so a getstatic/
                    // invokestatic no longer reads the class in the
                    // initialized-but-clinit-pending window (3c parity).
                    {
                        let (done_frame, fname, _) = frames.last().unwrap();
                        if fname.as_ref() == "<clinit>" {
                            done_frame.class.clinit_done.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    if let Some((f, _, _)) = frames.pop() {
                        recycle_frame(self, f);
                    }
                    match frames.last_mut() {
                        Some((caller, _, _)) => {
                            if let Some(val) = v {
                                caller.push(val);
                            }
                        }
                        None => return Ok(v),
                    }
                }
                #[cfg(feature = "next-gen-gc")]
                Ok(opcodes::RunExit::Bounded) => unreachable!("RunExit::Bounded only from the counted-loop fork worker path"),
                Ok(opcodes::RunExit::Call) => {
                    let crate::vmstate::PendingCall { decl, name: cname, desc: cdesc, args: cargs, midx } =
                        self.pending.take().expect("RunExit::Call without a pending call");
                    // Mirror of the RAVA_TRACE_CALLS hook in Vm::invoke — staged
                    // trampoline calls bypass invoke, so a machine-differential trace
                    // must log them here too (tagged [CALL-T] to tell the paths apart).
                    trace_call_hook(&decl.file.this_class, &cname, &cdesc, &cargs, true);
                    let callee = self.build_frame_idx(&decl, midx, &cname, &cdesc, cargs)?;
                    frames.push((callee, cname, cdesc));
                }
                Ok(opcodes::RunExit::Yield) => {
                    // Increment 1 (no scheduler yet): a single coroutine, so a
                    // Thread.sleep yield is just the network-pump point — same
                    // effect the old Thread.sleep native had. The green-thread
                    // scheduler replaces this with park + round-robin.
                    self.yield_request = None;
                    crate::net::pump_network(self);
                }
                #[cfg(feature = "next-gen-gc")]
                Ok(opcodes::RunExit::NurseryFull) => {
                    // P3-S1: a GIL-free worker must NEVER collect (its GenHeap is a
                    // zero-cap view — a major here is an instant "tenured exhausted"
                    // process abort). Raise the overflow abort and stop this drive; the
                    // region tears down to the bit-exact serial retry (garbage results
                    // discarded, same discipline as the G1.1 catch_unwind).
                    if crate::parallel::gilfree() {
                        crate::parallel::request_abort_overflow();
                        return Ok(None);
                    }
                    // SAFEPOINT: no frame is op-borrowed here. Evacuate the nursery
                    // (Cheney) forwarding every root, then loop — the same `new`
                    // re-executes (PC was not advanced) into the freed nursery.
                    self.arena_safepoint_gc(&mut frames);
                }
                #[cfg(feature = "next-gen-gc")]
                Ok(opcodes::RunExit::ClinitPending(class)) => {
                    // 3c PARITY: run the <clinit> SYNCHRONOUSLY (recursive), exactly
                    // like the default machine. Flattening it onto the explicit stack
                    // (ensure_init_tramp) left an initialized-latched / clinit-pending
                    // WINDOW: while the queued clinit frame waited under deeper frames,
                    // any native-reentry path could getstatic the class and read
                    // half-built statics — a registry registered its entries AFTER the
                    // freeze → "unbound value …creative_mode_tab…" ~120s into a
                    // STACKFUL boot. Synchronous init closes the window by
                    // construction; a checkpoint simply never fires mid-clinit (the
                    // trigger only runs at drive_stack frame boundaries anyway).
                    self.ensure_init(&class)?;
                }
                Err(VmError::Throw(exc)) => {
                    // The throwing frame has no matching handler (`run` checked it),
                    // so unwind the explicit stack: pop it, then search each caller
                    // for a handler covering its in-flight invoke site.
                    if let Some((f, _, _)) = frames.pop() {
                        recycle_frame(self, f);
                    }
                    let mut handled = false;
                    loop {
                        let Some((caller, cn, cd)) = frames.last_mut() else { break };
                        let cclass = Arc::clone(&caller.class);
                        // The caller pc is just past the invoke; probe inside it.
                        let probe = caller.pc.saturating_sub(1);
                        let found = match cclass.file.method(cn, cd).and_then(|m| m.code.as_ref())
                        {
                            Some(ccode) => opcodes::find_handler(self, caller, ccode, probe, &exc)?,
                            None => None,
                        };
                        match found {
                            Some(h) => {
                                caller.clear_stack();
                                caller.push(exc.clone());
                                caller.pc = h;
                                handled = true;
                                break;
                            }
                            None => {
                                if let Some((f, _, _)) = frames.pop() {
                        recycle_frame(self, f);
                    }
                            }
                        }
                    }
                    if !handled {
                        return Err(VmError::Throw(exc));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Drive one green thread's explicit frame stack until it yields or finishes.
    /// Returns true when the thread completed (its stack drained, or it died on an
    /// uncaught throw / error — logged, not propagated, so one daemon dying can't
    /// abort the server). Mirrors `drive_stack` minus the checkpoint machinery.
    pub(crate) fn drive_green(&mut self, frames: &mut Vec<(Frame, Arc<str>, Arc<str>)>) -> bool {
        let owner = self as *const Self as usize;
        let _exec_scope =
            crate::frames::ExecScope::new(owner, std::ptr::addr_of!(*frames) as usize);
        loop {
            if frames.is_empty() {
                return true;
            }
            let exit = {
                let (frame, fname, fdesc) = frames.last_mut().unwrap();
                // ⭐**THE FRAME KNOWS ITS BODY — AND THIS DRIVER STILL WENT LOOKING FOR IT.**
                //
                // `drive_stack` was fixed to read the `Code` pointer `Frame::new` was handed;
                // its green-thread twin, which is the same loop, kept the old shape: an
                // `Arc<LoadedClass>` clone (a pair of atomics) plus `ClassFile::method` —
                // hashing the frame's name AND its descriptor and memcmp-ing both against the
                // overload set — every time control comes back to a frame, which is once per
                // call and once per return, to re-derive a pointer fixed when the frame was
                // built.
                //     was: O(1) hash of (name, descriptor) bytes + 2 atomics per frame RESUME,
                //          repeated for every call and return a green thread makes;
                //     is:  one pointer load.
                // `RAVA_NO_FAST_APPLY=1` restores the lookup.
                if super::apply::fast_apply() {
                    if frame.code.is_null() {
                        return true; // same refusal as a method/body that would not resolve
                    }
                    // SAFETY: as in `drive_stack` — the `Code` lives in the class this frame
                    // holds an `Arc` to, classes are never unloaded, and the frame outlives
                    // this borrow.
                    let code: &Code = unsafe { &*frame.code };
                    opcodes::run(self, code, frame)
                } else {
                    let fclass = Arc::clone(&frame.class);
                    let method = match fclass.file.method(&**fname, &**fdesc) {
                        Some(m) => m,
                        None => return true,
                    };
                    let code = match method.code.as_ref() {
                        Some(c) => c,
                        None => return true,
                    };
                    opcodes::run(self, code, frame)
                }
            };
            match exit {
                Ok(opcodes::RunExit::Normal(v)) => {
                    if let Some((f, _, _)) = frames.pop() {
                        recycle_frame(self, f);
                    }
                    match frames.last_mut() {
                        Some((caller, _, _)) => {
                            if let Some(val) = v {
                                caller.push(val);
                            }
                        }
                        None => return true,
                    }
                }
                Ok(opcodes::RunExit::Call) => {
                    let crate::vmstate::PendingCall { decl, name: cname, desc: cdesc, args: cargs, midx } =
                        match self.pending.take() {
                            Some(c) => c,
                            None => return true,
                        };
                    match self.build_frame_idx(&decl, midx, &cname, &cdesc, cargs) {
                        Ok(callee) => frames.push((callee, cname, cdesc)),
                        Err(_) => return true,
                    }
                }
                Ok(opcodes::RunExit::Yield) => {
                    // The thread hit its own Thread.sleep — slice over, stay parked.
                    self.yield_request = None;
                    return false;
                }
                #[cfg(feature = "next-gen-gc")]
                Ok(opcodes::RunExit::Bounded) => unreachable!("RunExit::Bounded only from the counted-loop fork worker path"),
                #[cfg(feature = "next-gen-gc")]
                Ok(opcodes::RunExit::NurseryFull) => {
                    // P3-S1: same worker-must-not-collect rule as the drive_stack site.
                    if crate::parallel::gilfree() {
                        crate::parallel::request_abort_overflow();
                        return true;
                    }
                    self.arena_safepoint_gc(frames);
                }
                #[cfg(feature = "next-gen-gc")]
                Ok(opcodes::RunExit::ClinitPending(class)) => {
                    // Synchronous init — same 3c-parity reasoning as drive_stack (the
                    // trampolined-clinit window let interleaved reads see half-built
                    // statics; the recursive machine never exposes it).
                    if self.ensure_init(&class).is_err() {
                        return true;
                    }
                }
                Err(VmError::Throw(exc)) => {
                    if let Some((f, _, _)) = frames.pop() {
                        recycle_frame(self, f);
                    }
                    let mut handled = false;
                    loop {
                        let Some((caller, cn, cd)) = frames.last_mut() else { break };
                        let cclass = Arc::clone(&caller.class);
                        let probe = caller.pc.saturating_sub(1);
                        let found = match cclass.file.method(cn, cd).and_then(|m| m.code.as_ref()) {
                            Some(ccode) => match opcodes::find_handler(self, caller, ccode, probe, &exc) {
                                Ok(h) => h,
                                Err(_) => None,
                            },
                            None => None,
                        };
                        match found {
                            Some(h) => {
                                caller.clear_stack();
                                caller.push(exc.clone());
                                caller.pc = h;
                                handled = true;
                                break;
                            }
                            None => {
                                if let Some((f, _, _)) = frames.pop() {
                        recycle_frame(self, f);
                    }
                            }
                        }
                    }
                    if !handled {
                        if std::env::var("RAVA_TRACE_NET").is_ok() {
                            eprintln!("[GREEN] thread died on uncaught throw");
                        }
                        return true;
                    }
                }
                Err(_) => return true,
            }
        }
    }

    /// Check out a reusable Value buffer (cleared) from this thread's pool.
    ///
    /// `&self`, NOT `&mut self`: the pool is THIS THREAD's cell (`frames::pool_take`
    /// resolves it itself), so `Vm` state is only read, never written. The
    /// `&mut self` spelling was the last thing keeping `opcodes::statics::pop_args*`
    /// on `&mut Vm`, which kept `invokestatic` (and the whole invoke* family) on it.
    pub(crate) fn take_vec(&self) -> Vec<Value> {
        frames::pool_take()
    }

    /// Return a buffer to this thread's pool (bounded so it can't grow without limit).
    /// `&self` — same thread-local pool as `take_vec`, same reason.
    pub(crate) fn give_vec(&self, v: Vec<Value>) {
        frames::pool_give(v)
    }

    /// Apply a functional value (lambda/method-ref closure) to `args`. Used by
    /// the stream/collection natives to call user predicates and mappers.
    /// Resolve a ZERO-ARG functional (a Thread's Runnable) to a stageable pending
    /// call WITHOUT invoking it — the trampoline path for the inline-thread
    /// `start()` native, so a spawned main (MC's "ServerMain"/"Server thread" —
    /// the entire boot) runs on the EXPLICIT frame stack (checkpointable) instead
    /// of recursing on the Rust stack. Returns `None` for shapes that need the
    /// recursive helpers (ctor refs, functional receivers, $fnkind combinators,
    /// native-served methods) — the caller then falls back to the recursive apply.
    pub(crate) fn stage_runnable(
        &mut self,
        func: &Value,
    ) -> Option<crate::vmstate::PendingCall> {
        // The lambda snapshot is representation-agnostic (a 0b01 handle OR a
        // RAVA_ARENA_LAMBDA 0b00 arena object); only the two NON-lambda arms below
        // still need a handle, and they keep their own `Value::Ref` guard. Staging
        // matters here precisely because this is the boot's own thread: a missed
        // arena-lambda would silently drop MC's main onto the recursive Rust stack.
        if let Some(lam) = self.value_lambda_of(func) {
            if lam.kind == 8 {
                return None; // ctor ref — allocates + returns, not a plain call
            }
            let call_args = lam.captured.clone();
            let (kind, lowner, lname, ldesc) =
                (lam.kind, lam.owner().to_string(), lam.name().to_string(), lam.descriptor().to_string());
            let owner = if matches!(kind, 5 | 9) {
                match call_args.first() {
                    Some(Value::Ref(rh)) => {
                        // A functional receiver needs invoke_lambda's delegation.
                        if self.heap.lambda_of(*rh).is_some()
                            || matches!(self.heap.get_field_syn(*rh, crate::heap::Syn::FnKind), Value::Str(_))
                        {
                            return None;
                        }
                        self.heap.class_of(*rh).unwrap_or(&lowner).to_string()
                    }
                    _ => return None, // arena/odd receiver — recursive path handles it
                }
            } else {
                lowner
            };
            if crate::opcodes::force_native_collection(self, &owner, &lname, &ldesc) {
                return None;
            }
            let decl = self.resolve_method(&owner, &lname, &ldesc).ok()??;
            decl.file.method(&lname, &ldesc)?.code.as_ref()?; // must be bytecode
            Some(crate::vmstate::PendingCall {
                decl,
                name: Arc::from(lname.as_str()),
                desc: Arc::from(ldesc.as_str()),
                args: call_args,
                // A lambda body is reached by name here, not through a cached call site.
                midx: None,
            })
        } else if let Value::Ref(h) = func {
            if matches!(self.heap.get_field_syn(*h, crate::heap::Syn::FnKind), Value::Str(_)) {
                return None; // synthetic combinator — apply() interprets it natively
            }
            // A plain Runnable/Thread subclass: its own bytecode run()V.
            let cls = self.heap.class_of(*h)?.to_string();
            let decl = self.resolve_method(&cls, "run", "()V").ok()??;
            decl.file.method("run", "()V")?.code.as_ref()?;
            Some(crate::vmstate::PendingCall {
                decl,
                name: Arc::from("run"),
                desc: Arc::from("()V"),
                args: vec![func.clone()],
                midx: None,
            })
        } else {
            None // not a lambda and not a handle — the recursive apply handles it
        }
    }
}
