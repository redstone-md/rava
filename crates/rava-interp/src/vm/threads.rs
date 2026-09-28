//! Threads: OS, green, and the task queue between them.

use crate::*;

impl Vm {
    pub(crate) fn enqueue_guest_task(
        &mut self,
        task: Value,
        future: Option<Value>,
        preset: Option<Value>,
    ) -> bool {
        if !self.task_runtime.is_started() {
            // The gilfree lane no longer routes through this pool (it needs a parked
            // coordinator, which a resident worker cannot give it — see
            // `gilfree_cf::drain`), so the pool keeps its unconditional width.
            let cap = 2;
            let count = std::thread::available_parallelism()
                .map(|n| n.get().min(cap).max(1))
                .unwrap_or(1);
            let mut thread_objects = Vec::with_capacity(count);
            for index in 0..count {
                let thread = self.heap.alloc("java/lang/Thread");
                self.heap.put_field_syn(thread, crate::heap::Syn::Name,
                    Value::Str(format!("rava-executor-{index}").into()),
                );
                self.heap.put_field(thread, "$daemon", Value::Int(1));
                self.heap.put_field(thread, "$alive", Value::Int(1));
                thread_objects.push(thread);
            }
            let vm_token = crate::parallel::GilVmToken::new(self as *mut Vm);
            if !self.task_runtime.start(vm_token, thread_objects) {
                return false;
            }
        }
        let detached = crate::detached_task::prepare(self, &task);
        let detached_plan = detached.is_some();
        let queued = self.task_runtime.enqueue(task, future, preset, detached);
        // ⭐**COUNT THE OFFLOAD, ALWAYS.** With compilers off this path turns a 1,296 s boot into
        // a 0.6 s one; with them ON the same boot spends 71 s inside the `Done` window, which
        // means the offload did not happen — and there was no counter anywhere to say whether it
        // was never asked, asked and refused, or accepted and then not run. Two relaxed adds on a
        // path that runs a few thousand times a boot.
        if queued {
            OFFLOAD_QUEUED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        } else {
            OFFLOAD_REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if queued {
            // GAP-A01/A05 sizing probe (inert unless RAVA_TASKGRAPH_PROBE is set).
            crate::taskgraph_probe::count_enqueued(detached_plan);
        }
        queued
    }

    pub(crate) fn wait_for_tracked_future(&mut self, future: u32) {
        while !matches!(self.heap.get_field(future, "$done"), Value::Int(1))
            && self.task_runtime.tracks_future(future)
        {
            let epoch = self.task_runtime.completion_epoch();
            let runtime = &self.task_runtime as *const crate::task_runtime::TaskRuntime;
            gil::release();
            // SAFETY: TaskRuntime is an immovable Vm field and shutdown joins every
            // worker before the Vm can be dropped.
            unsafe { (*runtime).wait_for_completion_after(epoch) };
            gil::acquire();
        }
    }

    /// Register a cooperative green thread: build the entry frame for `run()V` on
    /// `receiver` (the Runnable, or the Thread object itself when it overrides
    /// run()) and park it. It runs one slice at a time via `run_green_threads_slice`
    /// whenever the recursive main thread yields. No inline execution → an infinite
    /// daemon loop (e.g. MC's FileIOThread chunk-saver) no longer hangs the VM.
    /// Spawn a daemon on a REAL OS thread (the moonshot foundation). It runs only
    /// while it holds the GIL — taken when the main thread releases it at a
    /// blocking point (Thread.sleep) — so the heap stays single-writer. Its frames
    /// stack LIFO on the shared `frame_ptrs` above the (blocked) main thread's, so
    /// GC scans them too with no GC change. The Vm is shared via a raw pointer;
    /// SAFETY rests entirely on the GIL serialising all Vm/heap access.
    pub(crate) fn spawn_os_thread(&mut self, receiver: u32, thread_obj: u32) {
        let p = crate::parallel::GilVmToken::new(self as *mut Vm);
        let _ = std::thread::Builder::new()
            // Match the main engine thread's 1 GiB stack: the recursive interpreter
            // is deep, and a smaller daemon stack overflows (segfault, no backtrace).
            .stack_size(1 << 30)
            .spawn(move || {
                let p = p;
                gil::thread_started();
                gil::acquire();
                if gil::shutting_down() {
                    gil::release();
                    gil::thread_finished();
                    return;
                }
                gil::CURRENT_THREAD.with(|c| c.set(thread_obj));
                gil::set_worker();
                frames::register();
                crate::jit::set_current_vm(p.as_mut_ptr());
                // SAFETY: only the GIL holder dereferences the Vm; the main thread
                // is blocked awaiting the GIL the whole time we hold it, so this is
                // the sole live &mut Vm for the duration. Thread.sleep inside run()
                // drops + re-acquires the GIL to hand control back to main.
                let vm = unsafe { p.vm_mut() };
                let recv = Value::Ref(receiver);
                // Run the daemon body under catch_unwind: a panic here would otherwise
                // skip the GIL release below and DEADLOCK the whole server (the main
                // thread blocks forever on `gil::acquire`). On panic we still release
                // the GIL and clean up, so one dead daemon can't freeze the process —
                // critical for host safety. The GIL is held while we run, so touching
                // the Vm inside is the sole-mutator invariant; AssertUnwindSafe is
                // sound because on unwind we drop our access and never reuse it.
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = vm.apply(&recv, vec![]);
                }));
                if ran.is_err() {
                    eprintln!("[OS-THREAD] daemon panicked; releasing GIL and exiting cleanly");
                }
                frames::unregister();
                gil::release();
                gil::thread_finished();
            });
    }

    pub(crate) fn spawn_green_thread(&mut self, receiver: Value, thread_obj: u32) {
        let cls_name = match receiver {
            Value::Ref(h) => self.heap.class_of(h).map(|s| s.to_string()),
            _ => None,
        };
        let Some(cls_name) = cls_name else { return };
        if std::env::var("RAVA_TRACE_GT").is_ok() {
            eprintln!("[GT-SPAWN] {cls_name}  (live green threads now {})", self.green_threads.len() + 1);
        }
        // Resolve run()V through the hierarchy; skip threads with no runnable body.
        let decl = match self.resolve_method(&cls_name, "run", "()V") {
            Ok(Some(d)) => d,
            _ => return,
        };
        if decl.file.method("run", "()V").and_then(|m| m.code.as_ref()).is_none() {
            return;
        }
        let entry = match self.build_frame(&decl, "run", "()V", vec![receiver]) {
            Ok(f) => f,
            Err(_) => return,
        };
        self.green_threads.push(GreenThread {
            frames: vec![(entry, Arc::from("run"), Arc::from("()V"))],
            thread_obj,
            done: false,
        });
    }

    /// Advance every ready green thread by one slice — run it on the explicit stack
    /// until it yields (its own `Thread.sleep`) or completes. Called from the main
    /// (recursive) thread's `Thread.sleep`, so daemons make progress exactly when
    /// the main thread would otherwise be idle-waiting on them. Finished threads are
    /// reaped. Single OS thread, so this is plain cooperative scheduling.
    pub(crate) fn run_green_threads_slice(&mut self) {
        if self.green_threads.is_empty() || self.in_green_slice {
            return;
        }
        self.in_green_slice = true;
        let n = self.green_threads.len();
        for i in 0..n {
            if self.green_threads[i].done {
                continue;
            }
            // GC sees active frames through the driver's ExecScope and the parked
            // rest directly via the `green_threads` field.
            let mut frames = std::mem::take(&mut self.green_threads[i].frames);
            let thread_obj = self.green_threads[i].thread_obj;
            let prev_thread = self.main_thread;
            let prev_tramp = self.trampoline;
            self.main_thread = Some(thread_obj);
            self.trampoline = true;
            self.pending = None;
            self.yield_request = None;
            let done = self.drive_green(&mut frames);
            self.trampoline = prev_tramp;
            self.main_thread = prev_thread;
            self.pending = None;
            self.yield_request = None;
            self.green_threads[i].frames = frames;
            if done {
                self.green_threads[i].done = true;
            }
        }
        self.green_threads.retain(|g| !g.done);
        self.in_green_slice = false;
    }

    /// Restore heap + statics from a `snapshot` buffer (heap first, so statics'
    /// heap handles resolve into the restored object space). False on a malformed
    /// buffer.
    pub fn restore_state(&mut self, bytes: &[u8]) -> bool {
        let Ok((heap_bytes, statics)) = bincode::deserialize::<(
            Vec<u8>,
            Vec<(String, FastMap<String, Value>)>,
        )>(bytes) else {
            return false;
        };
        let Some(heap) = Heap::restore(&heap_bytes) else {
            return false;
        };
        self.heap = heap;
        self.loader.restore_statics(statics);
        true
    }

    /// The process-wide singleton `Thread.currentThread()` object.
    pub(crate) fn current_thread(&mut self) -> u32 {
        // A real daemon OS thread has its own Thread handle; the main OS thread
        // and green threads leave it 0 and use the shared `main_thread`.
        let tl = gil::CURRENT_THREAD.with(|c| c.get());
        if tl != 0 {
            return tl;
        }
        match self.main_thread {
            Some(h) => h,
            None => {
                let h = self.heap.alloc("java/lang/Thread");
                self.heap
                    .put_field_syn(h, crate::heap::Syn::Name, Value::Str("Server thread".into()));
                self.main_thread = Some(h);
                h
            }
        }
    }
}

/// Guest async bodies handed to the task runtime, and those the runtime refused.
/// Read by the periodic PC dump (`[PC-OFFLOAD]`).
pub(crate) static OFFLOAD_QUEUED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static OFFLOAD_REFUSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// `supplyAsync`/`runAsync` calls that never even reached the queue because the executor could
/// not be proven resident — the third outcome, and the one a queued/refused pair alone hides.
pub(crate) static OFFLOAD_UNPROVEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `supplyAsync` calls that reached the native at all (vs. never arriving).
pub(crate) static OFFLOAD_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
