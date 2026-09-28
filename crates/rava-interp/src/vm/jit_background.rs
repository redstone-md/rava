//! The background compile queue and the JIT counters.

/// Methods a worker found hot but could not compile. `(seen, queue)` — the set keeps a method
/// from being re-queued every visit, the queue is what the owning thread drains.
///
/// ⭐**`seen` IS KEYED BY `Code` POINTER, NOT BY THE THREE NAMES.** The producer side runs on
/// 482,902 worker visits a boot; the consumer side has ~1,000 distinct methods to compile. Keying
/// the set on `(String, String, String)` meant building three owned strings AND cloning them —
/// six allocations — on every one of those visits, purely to ask a question a `usize` answers.
/// The names are still what the drain needs (`force_compile` resolves by name, because a `Code`
/// pointer is not something the loader can be asked about), so they are materialised exactly
/// once per method, at the moment it enters the queue.
#[allow(clippy::type_complexity)]
static WORKER_HOT: std::sync::Mutex<(
    Option<std::collections::HashSet<usize>>,
    Vec<(String, String, String)>,
)> = std::sync::Mutex::new((None, Vec::new()));

/// Queue depth, published outside the mutex.
///
/// ⭐**THE DRAIN IS CALLED FROM `jit_run` ON EVERY MAIN-THREAD PASSAGE — ~21 M a boot — AND IT
/// TOOK A GLOBAL LOCK EACH TIME, contending with the very workers filling it.** That cost is paid
/// whether or not anything is pending, and the queue is empty for almost all of those passages
/// (the whole population is ~1,000 methods). One relaxed load answers "is there anything to do?"
/// and the lock is taken only when the answer is yes. Removing it puts a lock round-trip back on
/// the invoke path of every compiled and interpreted entry in the process.
static WORKER_HOT_DEPTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

use crate::*;

impl Vm {
    /// Run a method on a specific class with the given locals/args (instance
    /// RAVA_PROF sample: charge the elapsed wall-time to the current guest call stack
    /// (bottom→top class names, depth-capped so deep VoxelShape recursion stays cheap —
    /// zero String allocation, the profiler interns the &str once) + a heap snapshot.
    pub(crate) fn prof_tick(&self) {
        let ptrs = frames::all();
        // Sample the TOP frames only (the leaf + its immediate callers — where time is
        // actually spent) and collapse consecutive same-class frames. This BOUNDS the
        // tree: VoxelShape shape-merges recurse thousands deep with alternating classes,
        // so an uncapped full-stack sample builds a unique multi-thousand-deep path every
        // time and the tree explodes to millions of nodes (the ~10x slowdown seen).
        let n = ptrs.len();
        let start = n.saturating_sub(48);
        let mut names: Vec<&str> = Vec::with_capacity(48);
        for &p in &ptrs[start..] {
            // SAFETY: read at a synchronous op-loop safepoint; every ptr is a live Frame.
            let f = unsafe { &*(p as *const Frame) };
            let c = f.class.file.this_class.as_str();
            if names.last().map_or(true, |&l| l != c) {
                names.push(c);
            }
        }
        let mem = (
            (crate::nextgen::arena_old_bytes() / 1024) as u64,
            (crate::mem_meter::live() / (1024 * 1024)) as u64,
            crate::nextgen::gc_epoch(),
        );
        crate::prof::sample(&names, crate::opcodes::CHECK_INTERVAL as u64, mem);
    }

    /// Resolve every field identity the background compiler can ask for. This is
    /// intentionally performed on the VM thread: field/layout interning mutates
    /// VM-owned state, while the compiler thread gets a read-only lookup table.
    pub(crate) fn jit_background_field_ids(
        &mut self,
        code: &Code,
        pool: &rava_classfile::ConstantPool,
    ) -> Option<std::collections::HashMap<(String, String), (u32, Option<u32>)>> {
        let _phase = crate::jit_phase_trace::enter("fields", code as *const Code as usize);
        let fields = jit::resolve_fields(&code.bytecode, pool, &mut |owner, name| {
            let id = self.resolve_field_id(owner, name);
            let slot = self.layout_slot(owner, id);
            (id, slot)
        })?;
        let mut ids = std::collections::HashMap::with_capacity(fields.len() + 1);
        for (cp, (id, _kind, slot)) in fields {
            let field = pool.member_ref(cp)?;
            ids.insert((field.owner.to_string(), field.name.to_string()), (id, slot));
        }
        // `compile_with_static_intrinsics` resolves this synthetic field only
        // when it emits the BitSet fast path. Match that exact trigger so this
        // helper never loads or interns an unrelated class early.
        let mut pc = 0usize;
        while pc < code.bytecode.len() {
            let op = code.bytecode[pc];
            let len = jit::insn_len_at(&code.bytecode, pc)?;
            if op == 0xb6 {
                let cp = ((code.bytecode[pc + 1] as u16) << 8) | code.bytecode[pc + 2] as u16;
                if let Some(method) = pool.member_ref(cp) {
                    if method.owner == "java/util/BitSet"
                        && method.name == "get"
                        && method.descriptor == "(I)Z"
                    {
                        let id = self.resolve_field_id("java/util/BitSet", "$bits");
                        let slot = self.layout_slot("java/util/BitSet", id);
                        ids.insert(("java/util/BitSet".to_string(), "$bits".to_string()), (id, slot));
                        break;
                    }
                }
            }
            pc += len;
        }
        Some(ids)
    }

    /// Compile requests a WORKER could not serve itself.
    ///
    /// ⭐**MEASURED: 482,902 worker visits per boot land on a method that has ALREADY crossed the
    /// compile threshold** — proven hot, and compiled never, because a worker cannot own a
    /// JITModule (it dies with the thread) and cannot touch VM state off-GIL to resolve the
    /// compile plans. The comment beside `JIT_GATE[6]` has named this for a while; this is its
    /// population, and the interpreted-entry census names its victims: `gson/LinkedTreeMap`
    /// (~830 k entries across find/get/put/rebalance), `DoubleArrayList.equals`, guava's
    /// `AbstractIndexedListIterator` — 36 methods, 1,688,387 interpreted entries, none compiled.
    ///
    /// A worker cannot compile, but it can leave a NOTE: three owned strings, no pointers, no
    /// heap access. The owning thread drains them where it already drains finished compiles.
    ///
    /// ⭐**THE HALF-MILLION-TIMES PATH DOES NO ALLOCATION.** `key` is the callee's `Code` pointer
    /// — the identity `jit_cache` itself uses — so the "already reported?" question is a `usize`
    /// hash. The three names are built only on the transition into the queue, ~1,000 times a boot
    /// instead of 482,902. `seen` is never drained, so a method is reported once for the whole
    /// process even after it has compiled: ~1,000 `usize` at 8 B is under 16 KB and it is touched
    /// only by workers, never by the interpreter's own loop.
    pub(crate) fn note_worker_hot(key: usize, class_name: &str, name: &str, desc: &str) {
        const CAP: usize = 8192;
        let mut g = WORKER_HOT.lock().unwrap_or_else(|e| e.into_inner());
        let (seen, queue) = &mut *g;
        if queue.len() >= CAP {
            return;
        }
        let seen = seen.get_or_insert_with(std::collections::HashSet::new);
        if seen.insert(key) {
            queue.push((class_name.to_string(), name.to_string(), desc.to_string()));
            // Published AFTER the push and while the lock is held, so a reader that sees a
            // non-zero depth is guaranteed a non-empty queue by the time it acquires the lock.
            WORKER_HOT_DEPTH.store(queue.len(), std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Publish completed background code on the owning VM thread. The compiler
    /// thread keeps its module mapped for the lifetime of `background_jit`, and
    /// the cache is declared before that owner so its function pointers die first.
    /// Serve the compile requests workers left behind (see `note_worker_hot`). Runs on the
    /// owning VM thread, where compiling is legal. Bounded per drain so a safepoint stays short.
    pub(crate) fn drain_worker_hot(&mut self) {
        // ONE RELAXED LOAD ON THE PASSAGE PATH. Both halves of this test are `OnceLock`/atomic
        // reads; the mutex is reached only when a worker has actually left something, which is
        // ~1,000 times against ~21 M passages. The old shape locked unconditionally.
        if WORKER_HOT_DEPTH.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            return;
        }
        if !(crate::vm::worker_hot_enabled() || crate::vm::jit_run::wide_tier_enabled()) {
            return;
        }
        // ⛔**RE-ENTRANT BY CONSTRUCTION, AND IT HAD NO GUARD.** `force_compile` calls `jit_run`,
        // and `jit_run` calls this on every main-thread passage — so draining 16 notes entered
        // this function 16 more times, each draining 16 more. The queue caps at 8,192 and a
        // `jit_run` frame is a kilobyte-class Rust frame, so a full queue could nest hundreds of
        // levels deep before it emptied. Nothing was lost while the flag was OFF (the queue was
        // always empty); it is load-bearing the moment notes are actually written. The guard is a
        // thread-local bool: the outer drain keeps the work, the inner calls return instantly and
        // the notes they skipped are taken by the next passage.
        thread_local! {
            static DRAINING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        if DRAINING.with(|d| d.replace(true)) {
            return;
        }
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                DRAINING.with(|d| d.set(false));
            }
        }
        let _g = Guard;
        let batch: Vec<(String, String, String)> = {
            let mut g = WORKER_HOT.lock().unwrap_or_else(|e| e.into_inner());
            let take = g.1.len().min(16);
            let batch: Vec<_> = g.1.drain(..take).collect();
            WORKER_HOT_DEPTH.store(g.1.len(), std::sync::atomic::Ordering::Relaxed);
            batch
        };
        // `force_compile` re-applies every gate `jit_run` applies — the per-class native-served
        // refusal included — so a worker's note can only compile what the main thread would have
        // compiled had it made the call itself. It cannot widen eligibility, only its timing.
        let _hr_wait = crate::background_jit::hr_wait::DeferScope::enter(crate::background_jit::hr_wait::WaitCaller::WorkerHot); // [HR-WAIT] nobody waits on a note: a `Compiling` body is published by a later drain, never parked for
        for (cls, name, desc) in batch {
            let _ = self.force_compile(&cls, &name, &desc);
        }
    }


    pub(crate) fn drain_background_jit(&mut self) {
        let mut completed = Vec::new();
        if let Some(background) = self.background_jit.as_ref() {
            while let Some(result) = background.try_take() {
                completed.push(result);
            }
        }
        for result in completed {
            self.publish_background_jit(result);
        }
    }

    /// Preserve the synchronous GIL-free warm-up contract when a needed body is
    /// already in the background queue. There are at most two outstanding jobs,
    /// so this waits only until the requested pending entry can be published.
    pub(crate) fn wait_for_background_jit(&mut self, key: usize) {
        let _phase = crate::jit_phase_trace::enter("wait", key);
        if crate::background_jit::hr_wait::defer() { return; } // [HR-WAIT] the caller opened a DeferScope: it does not need the body now; the entry stays `Compiling` and the next drain publishes it (kill switch RAVA_NO_HR_WAIT=1 → the historic park below)
        while matches!(self.jit_cache.get(&key), Some(jit::JitStatus::Compiling)) {
            let result = self
                .background_jit
                .as_ref()
                .and_then(background_jit::BackgroundJit::take_blocking);
            let Some(result) = result else {
                self.jit_cache.insert(key, jit::JitStatus::Failed);
                return;
            };
            self.publish_background_jit(result);
        }
    }

    pub(crate) fn publish_background_jit(&mut self, result: background_jit::CompileResult) {
        let _phase = crate::jit_phase_trace::enter("publish", result.key);
        if !matches!(self.jit_cache.get(&result.key), Some(jit::JitStatus::Compiling)) {
            return;
        }
            if std::env::var_os("RAVA_COMPILE_TIME").is_some() {
                use std::sync::atomic::{AtomicU64, Ordering};
                static NANOS: AtomicU64 = AtomicU64::new(0);
                static COUNT: AtomicU64 = AtomicU64::new(0);
                let ns = result.elapsed.as_nanos() as u64;
                let total = NANOS.fetch_add(ns, Ordering::Relaxed) + ns;
                let count = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                if count % 500 == 0 {
                    eprintln!(
                        "[COMPILE-TIME] {count} background methods compiled, {:.2}s cumulative Cranelift compile, {:.2}s of it in finalize_definitions",
                        total as f64 / 1e9,
                        crate::jit::runtime::finish::FINALIZE_NS
                            .load(Ordering::Relaxed) as f64
                            / 1e9
                    );
                }
            }
        match result.compiled {
                Some(parts) => {
                    let mut function = jit::JitFn::from_parts(parts);
                    function.mid = jit::register_compiled_method(
                        &result.class_name,
                        &result.name,
                        &result.descriptor,
                    );
                    crate::pc_profile::register(
                        function.raw() as usize,
                        function.code_len as usize,
                        function.mid,
                    );
                    if self.trace.jit_stats {
                        self.jit_stats
                            .entry(format!("{}.{}{}", result.class_name, result.name, result.descriptor))
                            .or_default()
                            .compile_ok += 1;
                    }
                    debug_assert!(
                        matches!(self.jit_cache.get(&result.key), Some(jit::JitStatus::Compiling)),
                        "background JIT may publish only its own pending entry"
                    );
                    Self::debug_assert_not_gilfree("jit_cache (background compile publication)");
                    crate::pilot_census::note_compile();
                    let hint = (
                        function.raw(),
                        function.ret,
                        function.self_reg,
                        function.mid,
                    );
                    self.jit_cache
                        .insert(result.key, jit::JitStatus::Compiled(function));
                    crate::vm::jit_run::publish_jit_entry_hint(
                        result.key,
                        hint.0,
                        hint.1,
                        hint.2,
                        hint.3,
                    );
                    self.jit_compiled += 1;
                    COMPILED_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                None => {
                    if result.reserved_static_slots > 0 {
                        self.heap.free_static_slots_from(
                            result.reserved_static_base,
                            result.reserved_static_slots,
                        );
                    }
                    if self.trace.jit_stats {
                        self.jit_stats
                            .entry(format!("{}.{}{}", result.class_name, result.name, result.descriptor))
                            .or_default()
                            .compile_failed += 1;
                    }
                    self.jit_cache.insert(result.key, jit::JitStatus::Failed);
                }
        }
    }

    /// GILFREE-RINGS item-1: synchronously compile `class#name desc` on the MAIN
    /// thread (under the GIL) WITHOUT running it — priming the JIT cache so a
    /// gilfree-CF task whose shared impl is still cold can be routed off-GIL on
    /// its first enqueue. Returns true iff the body is Compiled afterward. Cheap
    /// after the first call (the 120 ring tasks share ONE impl). No-op on a
    /// worker (only main compiles) or a native-served / denied / Failed body.
    #[cfg(feature = "next-gen-gc")]
    /// WHY a `force_compile` refused, by arm. Every return point below was a bare `false`, and
    /// the lane's own pre-pass reports `warm-compile 9/25` on a product boot — sixteen callees it
    /// could not compile before opening the region, each of which becomes a live
    /// `InternalError: direct miss` when a worker reaches it (three of four aborting runs die
    /// that way). Which arm refuses decides whether the fix is hierarchy resolution, an
    /// abstract/native callee that can never be compiled, or a real codegen failure worth reading.
    pub(crate) fn force_why_report() -> String {
        use std::sync::atomic::Ordering::Relaxed;
        let v: Vec<u64> = FORCE_WHY.iter().map(|a| a.load(Relaxed)).collect();
        let total: u64 = v.iter().sum();
        if total == 0 {
            return String::new();
        }
        format!(
            "attempts={total} | {}",
            FORCE_ARMS
                .iter()
                .zip(v.iter())
                .filter(|(_, n)| **n > 0)
                .map(|(k, n)| format!("{k}={n}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }


    /// Request compilation from one resolved dispatch miss without using a call-count threshold.
    /// The same structural target may already be queued, compiled or permanently refused.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn request_dispatch_compile(
        &mut self,
        class: &Arc<LoadedClass>,
        name: &str,
        descriptor: &str,
    ) -> bool {
        if gil::is_worker() || !self.jit_enabled {
            return false;
        }
        self.drain_background_jit();
        let Some(method) = class.file.method(name, descriptor) else {
            return false;
        };
        let Some(code) = method.code.as_ref() else {
            return false;
        };
        let key = code as *const Code as usize;
        match self.jit_cache.get(&key) {
            Some(jit::JitStatus::Compiled(_) | jit::JitStatus::Compiling) => return true,
            Some(jit::JitStatus::Failed) => return false,
            Some(jit::JitStatus::Cold(_)) | None => {}
        }

        let is_static = method.access_flags & 0x0008 != 0;
        let threshold = self.jit_threshold.max(1);
        self.jit_cache
            .insert(key, jit::JitStatus::Cold(threshold.saturating_sub(1)));

        struct RequestScope {
            was_compile_only: bool,
            previous_async_key: usize,
        }
        impl Drop for RequestScope {
            fn drop(&mut self) {
                COMPILE_ONLY.with(|flag| flag.set(self.was_compile_only));
                COMPILE_ASYNC_ONLY_KEY.with(|key| key.set(self.previous_async_key));
            }
        }
        let _scope = RequestScope {
            was_compile_only: COMPILE_ONLY.with(|flag| flag.replace(true)),
            previous_async_key: COMPILE_ASYNC_ONLY_KEY.with(|async_key| async_key.replace(key)),
        };
        let _ = self.jit_run(code, name, descriptor, is_static, class, &[]);
        matches!(
            self.jit_cache.get(&key),
            Some(jit::JitStatus::Compiled(_) | jit::JitStatus::Compiling)
        )
    }

    pub(crate) fn force_compile(&mut self, class_name: &str, name: &str, desc: &str) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        if gil::is_worker() {
            FORCE_WHY[0].fetch_add(1, Relaxed);
            return false; // a worker's JITModule frees on thread exit → never compile here
        }
        // AND NOT WITH THE JIT OFF. `jit_run` carries no `jit_enabled` test of its own — the gate
        // lives in its caller (vm/invoke.rs) — so this path would compile bodies in a process whose
        // whole premise is that nothing compiles. Reachable today: gilfree_cf::enabled() is
        // `RAVA_GILFREE_CF.is_some() || rings()`, so the env var re-arms the lane even after
        // rings() started requiring the JIT.
        if !self.jit_enabled {
            FORCE_WHY[1].fetch_add(1, Relaxed);
            return false;
        }
        self.drain_background_jit();
        let Ok(mut class) = self.loader.get_or_load(class_name) else {
            // NAME THE 18. `class-load` is the biggest arm of the warm-compile census and each
            // one is a callee the lane cannot pre-compile, hence a live `direct miss` abort the
            // moment a worker reaches it. Budgeted, and only under the lane's own diagnostic
            // gate, so the default path pays a single already-loaded `OnceLock` read.
            #[cfg(feature = "next-gen-gc")]
            if crate::parallel::lane_abort_site_enabled() {
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if N.fetch_add(1, Relaxed) < 12 {
                    eprintln!("[WARM-NOCLASS] {class_name}#{name}{desc}");
                }
            }
            FORCE_WHY[2].fetch_add(1, Relaxed);
            return false;
        };
        // RESOLVE THROUGH THE HIERARCHY. `class_name` is the CONCRETE receiver, which is what the
        // caller can observe, but the bytecode usually lives on an ancestor — and looking only at
        // the exact class silently returned false for precisely those methods. That is not
        // hypothetical: it is why the lane's warm-compile reported 35/47 and left
        // `BiomeSource.findBiomeHorizontal` — declared on the abstract base, invoked on a concrete
        // subclass, and the single symbol holding the region's dispatch lock — uncompiled, so the
        // lever looked refuted when it had never run on its one real target.
        if class.file.method(name, desc).is_none() {
            match self.resolve_method(class_name, name, desc) {
                Ok(Some(decl)) => class = decl,
                _ => {
                    FORCE_WHY[3].fetch_add(1, Relaxed);
                    return false;
                }
            }
        }
        let Some(code) = class.file.method(name, desc).and_then(|m| m.code.as_ref()) else {
            FORCE_WHY[4].fetch_add(1, Relaxed);
            return false;
        };
        let key = code as *const Code as usize;
        match self.jit_cache.get(&key) {
            Some(jit::JitStatus::Compiled(_)) => {
                FORCE_WHY[6].fetch_add(1, Relaxed);
                return true;
            }
            Some(jit::JitStatus::Compiling) => {
                self.wait_for_background_jit(key);
                return matches!(self.jit_cache.get(&key), Some(jit::JitStatus::Compiled(_)));
            }
            Some(jit::JitStatus::Failed) => {
                FORCE_WHY[5].fetch_add(1, Relaxed);
                return false;
            }
            _ => {}
        }
        let is_static = class
            .file
            .method(name, desc)
            .map(|m| m.access_flags & 0x0008 != 0)
            .unwrap_or(false);
        // Prime the counter so jit_run's Cold arm compiles THIS invocation.
        let thr = self.jit_threshold.max(1);
        self.jit_cache.insert(key, jit::JitStatus::Cold(thr));
        // ⛔**`COMPILE_ONLY` WAS SET AND THEN CLEARED — NOT SAVED AND RESTORED — AND THIS
        // FUNCTION ALREADY NESTS IN THE PRODUCT CONFIGURATION.** `jit_run`'s first act on the
        // owning thread is `drain_worker_hot`, whose whole job is to call `force_compile`, and
        // its gate `wide_tier_enabled()` is DEFAULT ON. So: outer `force_compile` sets the flag,
        // its `jit_run` drains one worker note, the inner `force_compile` clears the flag on the
        // way out, and the outer `jit_run` then reaches `jit_enter` with the flag FALSE. That
        // early-return is the only thing standing between a force-compile and EXECUTING the body
        // it just compiled on the empty `locals` this function passes — an arbitrary method run
        // with zero arguments at a point in the program where the guest never called it. That is
        // an observable-side-effect-sequence violation, not a performance bug. Preserve the
        // outer compile-only state across every nested compilation.
        //
        // Restored through `Drop` rather than a trailing `set`, so an unwind out of `jit_run`
        // cannot leave the flag stuck on and silently turn every later compile synchronous.
        struct CompileOnlyScope {
            previous_compile_only: bool,
            previous_async_key: usize,
        }
        impl Drop for CompileOnlyScope {
            fn drop(&mut self) {
                COMPILE_ONLY.with(|c| c.set(self.previous_compile_only));
                COMPILE_ASYNC_ONLY_KEY.with(|key| key.set(self.previous_async_key));
            }
        }
        let _co = CompileOnlyScope {
            previous_compile_only: COMPILE_ONLY.with(|c| c.replace(true)),
            previous_async_key: COMPILE_ASYNC_ONLY_KEY.with(|key| key.replace(0)),
        };
        // jit_run compiles + inserts Compiled, then returns None (COMPILE_ONLY
        // early-return) before running on the empty locals.
        let _ = self.jit_run(code, name, desc, is_static, &class, &[]);
        let ok = matches!(self.jit_cache.get(&key), Some(jit::JitStatus::Compiled(_)));
        FORCE_WHY[if ok { 6 } else { 5 }].fetch_add(1, Relaxed);
        ok
    }

    /// JIT call dispatch (invoked from the rava_invoke callback). Drives the
    /// REAL interpreter dispatch (`opcodes::invoke*`) through a synthetic frame
    /// standing in for the compiled caller, so every receiver special-case
    /// (slf4j/guava/fastutil/SAM/println/...) stays intact — reimplementing the
    /// dispatch here diverged from the interpreter and corrupted the bundler.
    ///
    /// `caller_ptr` is `Arc::as_ptr(&caller_class)` (baked as a JIT constant).
    /// Unpacks `raw` per the method descriptor (receiver first for non-static),
    /// builds the operand stack invoke* expects (receiver deepest, args on top),
    /// dispatches by `kind` (0=static, 1=virtual, 2=special, 3=interface), and
    /// returns the packed result (0 = void). kind 3 routes through invokevirtual,
    /// exactly as the interpreter's 0xb9 handler does.
    /// Identity of a 0b01 HANDLE lambda receiver cell for the direct-call cache:
    /// `(snapshot, per-call re-check fingerprint, λ-IMPL-KEY site key)`.
    ///
    /// Extracted so the resolve path can name the arena-lambda representation
    /// alongside it as a peer rather than as a special case bolted onto the handle
    /// prologue — the two differ only in how identity is obtained.
    pub(crate) fn jit_lambda_handle_ident(&self, cell: i64) -> Option<(heap::Lambda, u64, u64)> {
        if (cell as u64) >> 32 != (crate::value::REF_TAG as u64) >> 32 {
            return None;
        }
        let lam_fp = self.heap.lambda_fp(cell as u32);
        let lam = self.heap.lambda_of(cell as u32)?;
        // λ-IMPL KEY (gated): PIN this call site's `Arc<LambdaSite>` before its
        // address is ever used as a cache key, then key the entry on it. Pinning
        // FIRST is the whole soundness argument — see `nextgen::lambda_site_pin`.
        // 0 (receiver is not a live lambda) can't happen here (lambda_of succeeded)
        // but is refused rather than assumed.
        let site_key = if crate::jit::lambda_impl_key_enabled() {
            crate::nextgen::lambda_site_pin(&lam.site);
            let k = self.heap.lambda_site_key(cell as u32);
            if k == 0 {
                return None;
            }
            k
        } else {
            0
        };
        Some((lam, lam_fp, site_key))
    }

    /// WALL-CLOCK JIT-STATS DUMP (`RAVA_JIT_STATS_MS=<period>`, default off).
    ///
    /// `dump_jit_stats` is reachable from the CLI's clean exit and from the interpreter
    /// heartbeat at every 100M INTERPRETED ops. A server boot measured to Done and then killed
    /// reaches neither: it never exits cleanly, and it is mostly COMPILED, so the interpreted
    /// op counter does not get near 100M. The per-method table was therefore unavailable for
    /// exactly the phase under investigation — the third readout in this tree wired to a path
    /// the boot does not take (picprobe::dump and icmiss::dump were the first two, both fixed
    /// by moving them onto the lane probe's reporter thread).
    ///
    /// This one cannot move to that thread, because the stats live in `Vm` and the reporter has
    /// no `&Vm`. So the trigger is a timestamp compared on a path the boot DOES take, checked
    /// from `jit_run` which already holds `&mut self`. Off ⇒ one relaxed load of a `u64` that is
    /// 0, hoisted out of anything hot; the whole check is behind `trace.jit_stats` anyway.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn jit_stats_tick(&self) {
        use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
        // Plain bool field FIRST. `jit_run` is the hottest entry in the engine, and this check
        // is off for every production run, so it must not cost an atomic — the OnceLock below
        // is only reached once someone has already armed RAVA_JIT_STATS.
        if !self.trace.jit_stats {
            return;
        }
        static PERIOD_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        static NEXT_MS: AtomicU64 = AtomicU64::new(0);
        static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let period = *PERIOD_MS.get_or_init(|| {
            std::env::var("RAVA_JIT_STATS_MS")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(0)
        });
        if period == 0 {
            return;
        }
        let t0 = *T0.get_or_init(std::time::Instant::now);
        let now = t0.elapsed().as_millis() as u64;
        let due = NEXT_MS.load(Relaxed);
        if now < due {
            return;
        }
        // Racy CAS is fine: a lost race means one extra dump, and the reader keeps the LAST
        // row per method anyway.
        if NEXT_MS
            .compare_exchange(due, now + period, Relaxed, Relaxed)
            .is_ok()
        {
            self.dump_jit_stats();
        }
    }

    pub fn dump_jit_stats(&self) {
        // Compiled→interpreter fallback tally (RAVA_FALLBACK_TALLY): carries its
        // own gate, so it reports even when jit_stats is off.
        crate::fallback_tally::dump(self);
        // Choke-residue census (RAVA_CHOKE_RESIDUE): same contract — carries its own
        // gate, silent when unarmed.
        #[cfg(feature = "next-gen-gc")]
        super::choke_residue::dump(self);
        // Fu-miss receiver census (RAVA_FU_MISS_CENSUS): same contract — carries its
        // own gate, silent when unarmed.
        #[cfg(feature = "next-gen-gc")]
        super::fu_miss_census::dump(self);
        if !self.trace.jit_stats || self.jit_stats.is_empty() {
            return;
        }
        let mut rows: Vec<_> = self.jit_stats.iter().collect();
        rows.sort_by(|(ak, a), (bk, b)| {
            let aw = a.compiled_runs
                + a.compile_ok
                + a.compile_failed
                + a.cached_failed
                + a.verify_skipped;
            let bw = b.compiled_runs
                + b.compile_ok
                + b.compile_failed
                + b.cached_failed
                + b.verify_skipped;
            bw.cmp(&aw).then_with(|| ak.cmp(bk))
        });
        eprintln!(
            "[JIT-STATS] methods={} columns=seen,cold,run,compile_ok,compile_fail,cached_fail,verify_skip",
            rows.len()
        );
        // Uncapped: a truncated dump silently hides methods from compile-set diffs
        // (a th=A-vs-th=B miscompile bisect needs the FULL compiled list).
        for (name, s) in rows.into_iter() {
            eprintln!(
                "[JIT-STATS] {name} seen={} cold={} run={} compile_ok={} compile_fail={} cached_fail={} verify_skip={}",
                s.seen, s.cold, s.compiled_runs, s.compile_ok, s.compile_failed, s.cached_failed, s.verify_skipped
            );
        }
        eprintln!("[JIT-CODESIZE] {}", crate::jit::code_size_stats());
        #[cfg(feature = "next-gen-gc")]
        eprintln!(
            "[ARENA-PROBE] ARENA_ALLOCS(tlab_try_alloc)={} | callbacks: {}",
            crate::nextgen::ARENA_ALLOCS.load(std::sync::atomic::Ordering::Relaxed),
            crate::jit::callback_stats()
        );
    }
}



/// Per-arm tally for `Vm::force_compile` (see `force_why_report`).
pub(crate) static FORCE_WHY: [std::sync::atomic::AtomicU64; 7] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 7];
pub(crate) const FORCE_ARMS: [&str; 7] = [
    "worker",
    "jit-off",
    "class-load",
    "method-missing",
    "no-code",
    "compile-failed",
    "COMPILED",
];
