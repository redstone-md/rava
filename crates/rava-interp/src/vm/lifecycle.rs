//! Constructing a Vm and running a program: frames, checkpoints, entry points.

use crate::*;

impl Vm {
    #[inline]
    pub(crate) fn jit_pending_is_set(&self) -> bool {
        crate::execution_state::pending_is_set()
    }

    #[inline]
    /// Convert a `VmError` from a guest call inside a JIT intrinsic into the
    /// pending-exception slot (GAP-P04: salted key derivation runs guest
    /// hashCode/equals, whose throws must surface exactly like any guest throw).
    pub(crate) fn pend_vm_err(&mut self, e: VmError) {
        let v = match e {
            VmError::Throw(v) => v,
            other => self.make_exception("java/lang/RuntimeException", &other.to_string()),
        };
        self.jit_pending_set(v);
    }

    pub(crate) fn jit_pending_set(&mut self, value: Value) {
        crate::execution_state::pending_set_first(value);
    }

    #[inline]
    pub(crate) fn jit_pending_take(&mut self) -> Option<Value> {
        crate::execution_state::pending_take()
    }

    #[inline]
    pub(crate) fn jit_pending_clear(&mut self) {
        crate::execution_state::pending_clear();
    }

    pub fn new(loader: ClassLoader) -> Self {
        // Preserve the old per-VM pending-exception lifetime now that the slot is
        // thread-local: a reused host thread must not leak one Vm's fault into the next.
        crate::execution_state::pending_clear();
        // Primitive-array representation ids are process constants. Publish all eight in
        // atype order before `BackgroundJit::from_env` can start compiler threads and before
        // any foreground compile can observe an incomplete registry.
        #[cfg(feature = "next-gen-gc")]
        crate::nextgen::init_primitive_array_classes();
        // Vec-growth census snapshot thread (inert unless RAVA_VECGROW_STAT is set).
        crate::vecgrow::start_sampler();
        let trace = TraceFlags {
            throw: std::env::var("RAVA_TRACE_THROW").is_ok(),
            fatal: std::env::var("RAVA_TRACE_FATAL").is_ok(),
            athrow: std::env::var("RAVA_TRACE_ATHROW").is_ok(),
            lambda: std::env::var("RAVA_TRACE_LAMBDA").is_ok(),
            frontier: std::env::var("RAVA_TRACE_FRONTIER").is_ok(),
            underflow: std::env::var("RAVA_TRACE_UNDERFLOW").is_ok(),
            jit_stats: std::env::var("RAVA_JIT_STATS").is_ok(),
            heartbeat: std::env::var("RAVA_HEARTBEAT").is_ok(),
            item: std::env::var("RAVA_TRACE_ITEM").is_ok(),
            gt: std::env::var("RAVA_TRACE_GT").is_ok(),
            big: std::env::var("RAVA_TRACE_BIG").is_ok(),
            ops: std::env::var("RAVA_TRACE_OPS").ok(),
            codec: std::env::var("RAVA_TRACE_CODEC").is_ok(),
        };
        // RAVA_XMX_MB caps the heap by estimated bytes (Heap::approx_bytes).
        let mem_limit = std::env::var("RAVA_XMX_MB")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .map(|mb| mb.saturating_mul(1024 * 1024))
            .unwrap_or(0);
        let mut vm = Self {
            loader,
            heap: Heap::default(),
            output: String::new(),
            call_stack: Vec::new(),
            trace,
            mem_limit,
            max_depth: std::env::var("RAVA_MAX_DEPTH")
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .filter(|&d| d > 0)
                .unwrap_or(20_000),
            // First GC after ~256MB of growth (re-armed to ~2x live after). Was
            // half the cap (e.g. 10GB) — far too late, RSS ballooned to 20GB+.
            gc_next: 256 * 1024 * 1024,
            gc_next_real: 256 * 1024 * 1024,
            class_objects: std::collections::HashMap::new(),
            set_index: crate::FastMap::default(),
            // Value-check, not existence: RAVA_JIT=0/false/off disables (the old
            // is_ok() meant RAVA_JIT=0 still enabled it). Unset → off for embedders
            // and tests; the `rava` CLI publishes RAVA_JIT=true by default.
            jit_enabled: crate::flags::jit_enabled_env(),
            #[cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))]
            cnp_enabled: std::env::var("RAVA_CNP")
                .ok()
                .map(|v| !matches!(v.trim(), "0" | "false" | "no" | "off" | ""))
                .unwrap_or(false),
            #[cfg(all(any(windows, target_os = "linux"), target_arch = "x86_64"))]
            cnp_cache: FastMap::default(),
            #[cfg(all(windows, target_arch = "x86_64"))]
            aot_clif_cache: FastMap::default(),
            #[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
            aot_full_cache: FastMap::default(),
            #[cfg(all(feature = "next-gen-gc", target_os = "linux", target_arch = "x86_64"))]
            aot_leaf_cache: std::collections::HashMap::new(),
            #[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
            aot_direct_targets: Default::default(),
            #[cfg(all(feature = "next-gen-gc", any(windows, target_os = "linux"), target_arch = "x86_64"))]
            aot_cells: Default::default(),
            #[cfg(feature = "next-gen-gc")]
            aot_hot: FastMap::default(),
            // GC defaults ON even when unset (a runtime needs it); RAVA_GC=0 off.
            gc_enabled: std::env::var("RAVA_GC")
                .ok()
                .map(|v| !matches!(v.trim(), "0" | "false" | "no" | "off"))
                .unwrap_or(true),
            jit_verify: std::env::var("RAVA_JIT_VERIFY").is_ok(),
            jit_cache: FastMap::default(),
            background_jit: background_jit::BackgroundJit::from_env(),
            #[cfg(feature = "next-gen-gc")]
            reg_abi_cache: FastMap::default(),
            offload_cache: FastMap::default(),
            offload_epoch: 0,
            dispatch_closure_cache: FastMap::default(),
            unloadable_owner_closed_cache: FastMap::default(),
            iface_closure_cache: FastMap::default(),
            shared_memos: crate::vm::SharedMemos::default(),
            jit_stats: std::collections::BTreeMap::new(),
            jit_compiled: 0,
            jit_cap: std::env::var("RAVA_JIT_CAP")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(16384), // shared module ⇒ ~2.5 KB/method (measured); 16384 ≈ 40 MB
            jit_threshold: std::env::var("RAVA_JIT_THRESHOLD")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                // BootSlice82: threshold 1 was shipped on the strength of a head4 A/B and
                // REVERTED here after it failed to replicate on a profile trained with it.
                // On the head5 product the same-binary A/B flips sign — `RAVA_JIT_THRESHOLD=16`
                // boots 20.1/20.2/19.4/19.6 against the default-1 arm's 20.4/20.2/20.0/20.4 —
                // and head5 (profile at 1) is no faster than head4 (profile at 16). The head4
                // delta was a profile/schedule mismatch artefact, not a wall lever.
                // Explicit RAVA_JIT_THRESHOLD still wins.
                .unwrap_or(if jit_tier0() { 16 } else { 512 }),
            jit_direct_cache: FastMap::default(),
            jit_ctx_slot: Box::leak(Box::new(std::sync::atomic::AtomicUsize::new(
                frames::ctx_addr() as usize,
            ))),
            jit_direct_neg: FastMap::default(),
            #[cfg(feature = "next-gen-gc")]
            bitset_name_ptr: 0,
            #[cfg(feature = "next-gen-gc")]
            bitset_arena_cid: u32::MAX,
            indy_singletons: FastMap::default(),
            indy_singleton_handles: FastSet::default(),
            lambda_dedup: FastMap::default(),
            #[cfg(feature = "next-gen-gc")]
            arena_lambda_dedup: FastMap::default(),
            #[cfg(feature = "next-gen-gc")]
            arena_indy_singletons: FastMap::default(),
            op_total: 0,
            bool_cache: [None, None],
            box_cache: Box::default(),
            main_thread: None,
            entry_class: None,
            gc_protect: Vec::new(),
            task_runtime: crate::task_runtime::TaskRuntime::default(),
            lane: crate::gilfree_cf::LaneBatch::default(),
            #[cfg(feature = "next-gen-gc")]
            numeric_probe_seen: std::collections::HashSet::new(),
            #[cfg(feature = "next-gen-gc")]
            column_prewarmed: false,
            sockets: std::collections::HashMap::new(),
            net_selectors: Vec::new(),
            pending_streams: std::collections::HashMap::new(),
            in_pump: false,
            next_pump_op: 0,
            trampoline: false,
            pending: None,
            yield_request: None,
            green_threads: Vec::new(),
            inline_thread_active: false,
            os_thread_spawned: false,
            in_green_slice: false,
            gc_calls: 0,
            checkpointed: false,
        };
        // Allocate the JIT field-hint table up front so field_hint_base is valid
        // for any compiled code (incl. unit tests that compile() without jit_run).
        vm.heap.ensure_field_hints();
        // Same for the getstatic value-cell table + its generation mirror.
        vm.heap.ensure_static_cells();
        // Primitive array element ids for the JIT native-load type gate (before
        // any compile — foreground or background — can read them).
        vm.heap.ensure_prim_element_ids();
        vm.heap.set_static_gen_mirror(vm.runtime_shared().static_gen());
        // The inline `ArrayList.get` arm needs `$elems` BEFORE the first method compiles, and
        // the only other publisher (`fu_fastpath`) runs from compiled code, i.e. strictly
        // after. Interning two names at construction is what the natives do anyway.
        // ⚠**FOURTH AND LAST GATE OF THE SAME CHAIN.** The `$elems` inline family hung entirely
        // off one opt-in flag: the pre-scan looked only for `get`, the id resolve required
        // `alist_get_inline_enabled`, and so did this publication — so a NEW arm for `size`,
        // written three files away and default-ON, refused every one of its 4.4 M sites in
        // silence. Four separate conditions, each reasonable on its own, none of them near the
        // arm they disabled.
        if crate::jit::alist_get_inline_enabled()
            || crate::jit::alist_size_inline_enabled()
            || crate::jit::dalist_get_inline_enabled()
        {
            vm.publish_arraylist_word();
        }
        vm
    }


    /// Checkpoint the mutable VM state — heap + every loaded class's statics — to
    /// a byte buffer (generic, jar-agnostic; the CRaC-style base for skipping
    /// deterministic init on a later run). Does NOT yet capture execution state
    /// (call stack) or native resources (sockets) — later increments.
    pub fn snapshot(&self) -> Vec<u8> {
        bincode::serialize(&(self.heap.snapshot(), self.loader.snapshot_statics()))
            .unwrap_or_default()
    }

    /// Build a method activation the way `invoke`'s prologue does (expand
    /// long/double + the receiver into the local slots), without running it.
    pub(crate) fn build_frame(
        &mut self,
        class: &Arc<LoadedClass>,
        name: &str,
        desc: &str,
        args: Vec<Value>,
    ) -> Result<Frame, VmError> {
        self.build_frame_idx(class, None, name, desc, args)
    }

    /// `build_frame`, with the callee's index in `class.file.methods` when the staging site knew
    /// it. An interpreted call site caches that index on its `SiteRef`; without it every
    /// trampolined call re-finds the method by name — a hash plus a descriptor compare across the
    /// overload set, per call, for something fixed at the site.
    pub(crate) fn build_frame_idx(
        &mut self,
        class: &Arc<LoadedClass>,
        midx: Option<u32>,
        name: &str,
        desc: &str,
        args: Vec<Value>,
    ) -> Result<Frame, VmError> {
        let method = match midx {
            Some(i) => class.file.methods.get(i as usize),
            None => class.file.method(name, desc),
        }
        .ok_or_else(|| {
            VmError::MethodNotFound(format!("{}.{name}{desc}", class.file.this_class))
        })?;
        let is_static = method.access_flags & 0x0008 != 0;
        let code = method
            .code
            .as_ref()
            .ok_or_else(|| VmError::NoCode(format!("{name}{desc}")))?;
        // POOLED, and the widening skipped when the descriptor does not need it — the same two
        // things `invoke_idx` does. This builds a frame per trampolined call, and it was taking
        // two fresh allocations and an unconditional copy of every argument.
        let mut args = args;
        let locals = match crate::argcall::arg_layout(desc) {
            Some(l) if l.wide == 0 && l.unbox == 0 => args,
            _ => {
                let mut out = self.take_vec();
                expand_locals_into(&mut out, is_static, desc, &mut args, &self.heap);
                self.give_vec(args);
                out
            }
        };
        Ok(Frame::new(code, locals, Arc::clone(class), Vec::new()))
    }

    /// Run a method on an EXPLICIT `Vec<Frame>` instead of the Rust call stack:
    /// invokes suspend `run` (via `trampoline`/`pending`) and the driver pushes a
    /// callee frame; returns pop a frame and land the result on the caller. The
    /// whole machine state is the frame vector — serializable, hence the basis for
    /// checkpoint/restore of a *running* program (docs/snapshot-roadmap.md, 3c).
    /// Increment scope: normal call/return on the static/special/virtual paths.
    /// Exceptions and the lambda/invokedynamic paths still recurse (correct
    /// results, just not yet checkpointable mid-call) — later increments.
    pub fn execute_stackful(
        &mut self,
        class: Arc<LoadedClass>,
        name: &str,
        desc: &str,
        args: Vec<Value>,
    ) -> Result<Option<Value>, VmError> {
        let prev = self.trampoline;
        self.trampoline = true;
        self.pending = None;
        let r = self.run_stackful(class, name, desc, args);
        self.trampoline = prev;
        self.pending = None;
        r
    }

    pub(crate) fn run_stackful(
        &mut self,
        class: Arc<LoadedClass>,
        name: &str,
        desc: &str,
        args: Vec<Value>,
    ) -> Result<Option<Value>, VmError> {
        let entry = self.build_frame(&class, name, desc, args)?;
        self.drive_stack(vec![(entry, Arc::from(name), Arc::from(desc))])
    }

    /// Freeze the live frame stack + heap + statics to RAVA_CHECKPOINT_FILE — a
    /// running program captured to bytes. Restored by `restore_running`.
    pub(crate) fn write_checkpoint(&self, frames: &[(Frame, Arc<str>, Arc<str>)]) -> std::io::Result<()> {
        let snaps: Vec<FrameSnapshot> = frames
            .iter()
            .map(|(f, n, d)| FrameSnapshot {
                class_name: f.class.file.this_class.clone(),
                name: n.to_string(),
                desc: d.to_string(),
                pc: f.pc,
                locals: f.locals().to_vec(),
                stack: f.stack().to_vec(),
            })
            .collect();
        #[cfg(feature = "next-gen-gc")]
        let arena = crate::nextgen::snapshot_arena();
        #[cfg(not(feature = "next-gen-gc"))]
        let arena: Option<u8> = None;
        let buf = bincode::serialize(&(
            self.heap.snapshot(),
            self.loader.snapshot_statics(),
            snaps,
            // Runtime classpath additions (the bundler's extracted server jar): a
            // restore must re-add them BEFORE rebuilding frames, whose classes live
            // there, not on the base classpath.
            self.loader.added_paths.clone(),
            // The ARENA (regions + class registry + perm pool + boxes) — frames and
            // heap cells hold RAW arena pointers, valid only if the arena is rebuilt
            // byte-identically at the same fixed bases.
            arena,
            // VM-side singleton caches whose IDENTITY is semantically load-bearing:
            // Boolean.valueOf must keep returning the SAME two objects the restored
            // property sets contain (a fresh pair identity-misses ImmutableSet.contains
            // → 'Value false does not belong to property'), and currentThread() must
            // keep returning the object MinecraftServer captured as mainThread.
            (self.bool_cache, self.main_thread, self.inline_thread_active),
        ))
        .unwrap_or_default();
        let path = std::env::var("RAVA_CHECKPOINT_FILE").unwrap_or_else(|_| "rava.ckpt".into());
        std::fs::write(path, buf)
    }

    /// Restore a `write_checkpoint` file and RESUME the running program on the
    /// explicit stack: rebuild heap + statics + the frame stack, then drive it to
    /// completion. The basis for a near-instant restart (docs/snapshot-roadmap.md).
    pub fn restore_running(&mut self, path: &str) -> Result<Option<Value>, VmError> {
        let _t = std::time::Instant::now();
        let bytes = std::fs::read(path)
            .map_err(|e| VmError::UnsupportedNative(format!("checkpoint read: {e}")))?;
        #[cfg(feature = "next-gen-gc")]
        type ArenaCk = Option<crate::nextgen::ArenaSnapshot>;
        #[cfg(not(feature = "next-gen-gc"))]
        type ArenaCk = Option<u8>;
        type Ck = (
            Vec<u8>,
            Vec<(String, FastMap<String, Value>)>,
            Vec<FrameSnapshot>,
            Vec<String>,
            ArenaCk,
            ([Option<u32>; 2], Option<u32>, bool),
        );
        let (heap_bytes, statics, snaps, added_paths, arena, vm_singletons): Ck =
            bincode::deserialize(&bytes)
                .map_err(|e| VmError::UnsupportedNative(format!("checkpoint decode: {e}")))?;
        let (bool_cache, main_thread, inline_active) = vm_singletons;
        self.bool_cache = bool_cache;
        // The narrow-box intern tables are NOT checkpointed, so they must not survive one: their
        // handles index the heap this restore is about to REPLACE. Cleared rather than carried —
        // a cold `Vm` starts here too, and the only thing lost is `valueOf` identity between a
        // restored box and one minted after the restore. (`bool_cache` above is carried because
        // ImmutableSet property membership is identity-checked against it; nothing in this engine
        // identity-checks a Character against a restored snapshot.)
        *self.box_cache = crate::boxid::BoxCache::default();
        crate::optlite::reset(); // ADDED: same reason as the line above — the shared empty Optional's handle indexes the heap this restore replaces
        self.main_thread = main_thread;
        self.inline_thread_active = inline_active;
        // ARENA FIRST (before any allocation can claim the fixed bases): rebuild the
        // regions/registry/perm-pool byte-identically so every raw arena pointer in
        // the heap/statics/frames below is already valid.
        #[cfg(feature = "next-gen-gc")]
        if let Some(a) = &arena {
            crate::nextgen::restore_arena(a)
                .map_err(|e| VmError::UnsupportedNative(format!("arena restore: {e}")))?;
        }
        #[cfg(not(feature = "next-gen-gc"))]
        let _ = &arena;
        // Re-add the runtime classpath — the frames below reference classes
        // (e.g. the bundler-extracted server jar) not on the base classpath.
        for p in &added_paths {
            self.loader.add_classpath(p);
        }
        if let Some(h) = Heap::restore(&heap_bytes) {
            self.heap = h;
        }
        self.loader.restore_statics(statics);
        let mut frames: Vec<(Frame, Arc<str>, Arc<str>)> = Vec::with_capacity(snaps.len());
        for s in snaps {
            let class = self.loader.get_or_load(&s.class_name)?;
            // Restoring a checkpoint is once per frame and cold, so the body is found by name
            // here rather than being carried in the snapshot.
            let code = class
                .file
                .method(&s.name, &s.desc)
                .and_then(|m| m.code.as_ref())
                .map_or(std::ptr::null(), |c| c as *const Code);
            frames.push((
                {
                    // One buffer now: locals first, then the operands the snapshot saved.
                    let nlocals = s.locals.len();
                    let mut buf = s.locals;
                    buf.extend(s.stack);
                    let mut f = Frame::from_parts(code, buf, nlocals, class);
                    f.pc = s.pc;
                    f
                },
                Arc::from(s.name),
                Arc::from(s.desc),
            ));
        }
        // Re-open the OS listener sockets the checkpoint couldn't serialize, so a
        // restored server actually holds its port again (post-Done serving / TPS).
        for (h, port, host) in self.heap.bound_listeners() {
            if let Ok(l) = std::net::TcpListener::bind((host.as_str(), port as u16)) {
                let _ = l.set_nonblocking(true);
                self.sockets.insert(h, crate::net::NetSock::Listener(l));
            }
        }
        if std::env::var("RAVA_RESTORE_PROF").is_ok() {
            eprintln!("[RESTORE-PROF] restore_running ready in {:?} (heap+statics+frames); resuming", _t.elapsed());
        }
        let prev = self.trampoline;
        self.trampoline = true;
        let r = self.drive_stack(frames);
        self.trampoline = prev;
        r
    }

    /// Load `class_name`, run its `<clinit>`, then `main(String[])`.
    pub fn run_main(&mut self, class_name: &str) -> Result<(), VmError> {
        self.run_main_with_args::<&str>(class_name, &[])
    }

    /// Run `class_name.main(String[])`, passing `args` as the argument array.
    pub fn run_main_with_args<S: AsRef<str>>(
        &mut self,
        class_name: &str,
        args: &[S],
    ) -> Result<(), VmError> {
        // The main thread holds the GIL for the whole run, releasing it only at
        // blocking points (Thread.sleep) so daemon OS threads can run.
        gil::acquire();
        frames::register();
        // Restore + resume a checkpointed running program instead of booting.
        // No <clinit>: restore_running reloads classes and injects the saved
        // statics directly, then resumes the frame stack.
        if let Ok(ckpt) = std::env::var("RAVA_RESTORE") {
            self.restore_running(&ckpt)?;
            return Ok(());
        }
        let class = self.loader.get_or_load(class_name)?;
        self.entry_class = Some(class.file.this_class.clone());
        self.ensure_init(&class)?;
        let argv_vals: Vec<Value> = args.iter().map(|s| Value::Str(s.as_ref().into())).collect();
        let argv = self.heap.alloc_values("Ljava/lang/String;", argv_vals);
        let main_args = vec![Value::Array(argv)];
        // RAVA_STACKFUL drives main() through the explicit-stack interpreter (the
        // checkpoint/restore execution model) instead of the recursive one. Still
        // maturing (exceptions unwind; lambda/invokedynamic paths recurse) — a
        // validation harness, not yet the default.
        if std::env::var("RAVA_STACKFUL").is_ok() {
            self.execute_stackful(class, "main", "([Ljava/lang/String;)V", main_args)?;
        } else {
            self.invoke(class, "main", "([Ljava/lang/String;)V", main_args)?;
        }
        Ok(())
    }
}
