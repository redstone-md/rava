//! Linking and running an AOT-compiled module, with its on-disk cache.

use crate::*;

impl Vm {
    /// A2 — compile a method through the AOT translator and LINK it to a per-process
    /// (CompiledFn, consts) pair. `field_of`/`method_of`/`indy_of` resolve from the
    /// class constant pool (immutable); `resolve_const` fills the runtime constant table
    /// in this process (FieldId→interned id, ClassPtr→this class's LoadedClass ptr,
    /// Imm→value). Ok(None) is immutable refusal; Err(()) is a failed binding/read
    /// that must not become a permanent execution-policy decision.
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn aot_compile_link(
        &mut self,
        code: &Code,
        class: &Arc<LoadedClass>,
    ) -> Result<Option<(cnp::CompiledFn, Vec<i64>)>, ()> {
        use crate::aot_clif::diagnostics as measurement;
        measurement::cold_calls().inc();
        let _cold = measurement::Elapsed::start(measurement::cold_ns());
        // ELIGIBILITY PROBE (RAVA_LLVM_PROBE, read-only): count hot methods that adding
        // getfield/putfield to the LLVM subset would unlock — data before the risky change.
        #[cfg(all(windows, target_arch = "x86_64"))]
        if std::env::var_os("RAVA_LLVM_PROBE").is_some() {
            crate::llvm_aot::llvm_probe(&code.bytecode);
        }
        // BAIL on methods with an exception table: the translator has no catch-block
        // support, so a thrown exception would skip its handler and propagate out instead
        // of being caught (e.g. JNA's load() catches UnsatisfiedLinkError and falls back —
        // skipping that catch turns a recoverable miss into a fatal boot error). The JIT
        // bails the same way (has_exceptions). Same guard in the probe + aot_run.
        if !code.exceptions.is_empty() {
            crate::vm::aot_route_diag::aot_link_unsupported().inc();
            return Ok(None);
        }
        // The AOT run-path is CORRECT for invoke/new/getstatic/putfield/indy/anewarray/
        // instanceof/athrow/aastore + the read/numeric/control-flow core — a full
        // server.jar boots Done bit-exact at threshold=512. Numeric ldc is inline (proven);
        // string/class ldc bails in ldc_of unless RAVA_AOT_UNSAFE. RAVA_AOT_DENY_OPS=hex,…
        // overrides with a custom deny set (bisecting).
        {
            let deny: Vec<u8> = if let Ok(list) = std::env::var("RAVA_AOT_DENY_OPS") {
                list.split(',').filter_map(|s| u8::from_str_radix(s.trim(), 16).ok()).collect()
            } else if std::env::var_os("RAVA_AOT_UNSAFE").is_some() {
                Vec::new()
            } else {
                // Default-safe: no opcode-level deny — the ldc string/class subcase is
                // gated inside ldc_of (numeric ldc is inline + proven correct; string/
                // class ldc bails there). Everything else (invoke/new/getstatic/putfield/
                // indy/…) runs as AOT machine code, boots Done bit-exact.
                Vec::new()
            };
            if !deny.is_empty() {
                let mut pc = 0;
                while pc < code.bytecode.len() {
                    if deny.contains(&code.bytecode[pc]) {
                        crate::vm::aot_route_diag::aot_link_unsupported().inc();
                        return Ok(None);
                    }
                    match crate::jit::insn_len_at(&code.bytecode, pc) {
                        Some(l) => pc += l,
                        None => {
                            crate::vm::aot_route_diag::aot_link_unsupported().inc();
                            return Ok(None);
                        }
                    }
                }
            }
        }
        let class = Arc::clone(class);
        // DISK CACHE (A5, RAVA_AOT_DIR): a hit loads the persisted CompiledObj (skips the
        // translator entirely) so a SECOND boot links pre-compiled machine code — the
        // basis for exec-from-call-1 without paying the compile on every boot. The key
        // includes the constant pool: unchanged instruction bytes can refer to new literals.
        let key_time = measurement::Elapsed::start(measurement::key_ns());
        let prepared = crate::aot_clif::prepared::directory();
        let disk = prepared.or_else(|| cnp::aot_dir().map(|d| d.as_path()))
            .and_then(|d| Self::aot_disk_key(&class, code).map(|key| d.join(key)));
        drop(key_time);
        if let Some(path) = &disk {
            let read_time = measurement::Elapsed::start(measurement::read_ns());
            let bytes = if prepared.is_some() {
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    crate::vm::aot_route_diag::aot_link_read_fail().inc();
                    return Err(());
                };
                crate::aot_clif::prepared::read(name)
            } else { std::fs::read(path) };
            drop(read_time);
            if prepared.is_some() {
                if let Err(error) = &bytes {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        crate::vm::aot_route_diag::aot_link_read_fail().inc();
                        return Err(());
                    }
                }
            }
            if let Ok(bytes) = bytes {
                let decode_time = measurement::Elapsed::start(measurement::decode_ns());
                let obj = cnp::CompiledObj::from_bytes(&bytes);
                drop(decode_time);
                if let Some(obj) = obj {
                    let _bind = measurement::Elapsed::start(measurement::bind_ns());
                    let class_ptr = Arc::as_ptr(&class) as i64;
                    let mut rc = |d: &cnp::ConstDesc| -> Option<i64> {
                        match d {
                            cnp::ConstDesc::FieldId(o, n) => Some(self.field_declaring_id(o, n)? as i64),
                            cnp::ConstDesc::ClassPtr(_) => Some(class_ptr),
                            cnp::ConstDesc::Imm(v) => Some(*v),
                            cnp::ConstDesc::StrLit(s) => {
                                let p = crate::nextgen::intern_arena_string(s);
                                if p != 0 { Some(p) } else { None }
                            }
                            cnp::ConstDesc::StaticSlot(..) => None,
                            cnp::ConstDesc::AotLeaf(o, n, d) => self.resolve_direct_leaf(o, n, d),
                        }
                    };
                    let linked = obj.link_full(&|i| crate::aot_clif::resolve_symbol(i), &mut rc);
                    if linked.is_none() {
                        crate::vm::aot_route_diag::aot_link_bind_fail().inc();
                    }
                    return linked.map(Some).ok_or(());
                }
                if prepared.is_some() {
                    crate::vm::aot_route_diag::aot_link_decode_fail().inc();
                    return Err(());
                }
        }
        }
        // An explicit offline build is read-only. A missing/refused object must not
        // silently cause runtime compilation or populate a history-selected cache.
        if prepared.is_some() {
            crate::vm::aot_route_diag::aot_link_no_object().inc();
            return Ok(None);
        }
        // LLVM-AOT tier (RAVA_LLVM_AOT): for a pure int/long-numeric method, emit LLVM IR and
        // compile it with clang -O2 for near-C2 codegen. The subset is reloc-free (no
        // callbacks), so from_code().link() binds directly; the .text matches the AOT ABI
        // fn(locals, consts)->i64 that aot_run calls. Any miss (non-numeric shape, clang
        // failure) falls through to the Cranelift translator below.
        #[cfg(all(windows, target_arch = "x86_64"))]
        if llvm_aot_enabled() {
            // Whole-tree module: build_llvm_module BFS-discovers the LLVM-eligible static-call tree
            // (root + callees as one module with internal alwaysinline @fN → clang inlines +
            // cross-vectorises), fills the array-ABI consts, and returns (module IR, cvec). A
            // no-invoke method is just the single root function. Keyed on the IR in the blob cache.
            if let Some((ir, cvec)) = self.build_llvm_module(code, &class) {
                if std::env::var_os("RAVA_LLVM_DUMP").is_some() {
                    eprintln!("=== LLVM IR {} ===\n{ir}", class.file.this_class);
                }
                if let Some(text) = crate::llvm_aot::clang_compile_ir_cached(&ir) {
                    if let Some(f) = cnp::CompiledObj::from_code(text).link() {
                        if std::env::var_os("RAVA_AOT_TRACE_RUN").is_some() {
                            eprintln!("[LLVM-RUN] {}", class.file.this_class);
                        }
                        return Ok(Some((f, cvec)));
                    }
                }
            }
        }
        let Some(obj) = crate::aot_clif::ClassCompiler::new(&class.file)
            .with_string_constants(std::env::var_os("RAVA_AOT_UNSAFE").is_some())
            .compile_code(code) else {
                crate::vm::aot_route_diag::aot_link_unsupported().inc();
                return Ok(None);
            };
        // Link: resolve the symbolic const table in this process.
        let class_ptr = Arc::as_ptr(&class) as i64;
        // Persist the freshly-compiled object for a later disk-cache hit — ATOMICALLY.
        // A plain fs::write truncates then fills the file, so a CONCURRENT reader on
        // another thread (SLF4J bootstraps its logger factory on a background thread; the
        // same method can be compiled by one thread and cache-read by another) can see a
        // half-written blob → from_bytes deserializes CORRUPT machine code → a getfield
        // with a garbage field id reads null (the disk-cache-only 'getMarkerFactory
        // returned null' NPE that fresh compiles never hit). Write to a per-thread temp,
        // then rename — rename is atomic on NTFS/POSIX, so a reader sees either the old
        // (absent → recompiles) or the complete new file, never a torn one.
        if let Some(path) = &disk {
            let tid: String = format!("{:?}", std::thread::current().id())
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect();
            let tmp = path.with_extension(format!("tmp{tid}"));
            if std::fs::write(&tmp, obj.to_bytes()).is_ok() && std::fs::rename(&tmp, path).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        let mut resolve_const = |d: &cnp::ConstDesc| -> Option<i64> {
            match d {
                cnp::ConstDesc::FieldId(o, n) => Some(self.field_declaring_id(o, n)? as i64),
                cnp::ConstDesc::ClassPtr(_) => Some(class_ptr),
                cnp::ConstDesc::Imm(v) => Some(*v),
                // A String ldc: the stable interned PERM-pool address (like the JIT's
                // baked intern_arena_string), resolved per-process at link.
                cnp::ConstDesc::StrLit(s) => {
                    let p = crate::nextgen::intern_arena_string(s);
                    if p != 0 { Some(p) } else { None }
                }
                cnp::ConstDesc::StaticSlot(..) => None, // not emitted by this translator
                cnp::ConstDesc::AotLeaf(o, n, d) => self.resolve_direct_leaf(o, n, d),
            }
        };
        let linked = obj.link_full(&|i| crate::aot_clif::resolve_symbol(i), &mut resolve_const);
        if linked.is_none() {
            crate::vm::aot_route_diag::aot_link_bind_fail().inc();
        }
        linked.map(Some).ok_or(())
    }

    /// A2 — the AOT run-path lane. Runs a covered method as AOT machine code from call
    /// #1 (RAVA_AOT_CLIF), mirroring `jit_run`'s locals→cells marshalling + cells-rooting
    /// + result decode, but calling the `run_const(locals, consts)` ABI. `None` → the
    /// method is ineligible (uncovered) and falls through to jit_run/interpret.
    #[cfg(feature = "next-gen-gc")]
    /// Does the on-disk AOT cache (RAVA_AOT_DIR) already hold a compiled object for this
    /// method's bytecode? A cheap existence check (one `stat`), used to gate exec-from-call-1
    /// to validated boot#1-hot methods only. False when no cache dir is set.
    /// Disk-cache filename binds owner, bytecode, pool contents, local-slot count and
    /// string-constant policy. Owner plus bytecode alone still aliases different versions
    /// of the SAME class when a constant changes without changing its pool index.
    pub(crate) fn aot_disk_key(class: &LoadedClass, code: &Code) -> Option<String> {
        crate::aot_clif::cache_key::class_method_key(
            &class.file, code,
            std::env::var_os("RAVA_AOT_UNSAFE").is_some(),
        )
    }
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn aot_disk_has(&self, class: &LoadedClass, code: &Code) -> bool {
        cnp::aot_dir()
            .and_then(|d| Self::aot_disk_key(class, code).map(|key| d.join(key).exists()))
            .unwrap_or(false)
    }

    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn aot_run(
        &mut self,
        code: &Code,
        name: &str,
        descriptor: &str,
        is_static: bool,
        class: &Arc<LoadedClass>,
        locals: &[Value],
    ) -> Option<Result<Option<Value>, VmError>> {
        self.aot_run_input(code, name, descriptor, is_static, class,
            super::aot_arguments::Arguments::Values(locals))
    }

    #[cfg(feature = "next-gen-gc")]
    pub(super) fn aot_run_input(&mut self, code: &Code, name: &str, descriptor: &str,
        is_static: bool, class: &Arc<LoadedClass>, arguments: super::aot_arguments::Arguments<'_>,
    ) -> Option<Result<Option<Value>, VmError>> {
        let _run_time = crate::aot_clif::diagnostics::run().start();
        // The full AOT ABI owns a serial `Vm` + frame-root envelope.  A GIL-free
        // worker may reuse a cached function, but its callback paths are admitted
        // separately and must not inherit that serial envelope.  Fall through to
        // the worker-safe JIT/interpreter route until an AOT worker ABI is proven.
        // This is execution-state based (not class/workload based), so it preserves
        // the same semantics for every guest program.
        if crate::parallel::gilfree() {
            crate::vm::aot_route_diag::aot_guard_gilfree().inc();
            return None;
        }
        // ORDER-SAFETY (task #7): only run a method AOT once its class's <clinit> has
        // completed. `initialized` latches when init STARTS (to break recursive class
        // loading); it is therefore not sufficient here — a method reached during its
        // own <clinit> can still observe incomplete statics and construction state. The
        // interpreter sequences that window correctly, so defer until `clinit_done`.
        // This is an execution/lifecycle guard, not a workload admission rule; after the
        // one-time init it adds only one relaxed atomic load to every guest program.
        if !class.clinit_done.load(std::sync::atomic::Ordering::Relaxed) {
            crate::vm::aot_route_diag::aot_guard_clinit().inc();
            return None;
        }
        // Native-backed objects can store their state outside bytecode-visible fields.
        // Offline compilation does not establish representation compatibility: retain
        // the existing runtime guard before executing a prepared method. A future
        // widening must share the JIT type-closure proof rather than duplicate it.
        if (crate::opcodes::native_served_prefix(&class.file.this_class)
            && !crate::class_loader::has_bytecode(&class.file.this_class))
            || self.jit_native_served_deep(&class.file.this_class)
            || crate::owner_bits::facts(&class.file.this_class) & crate::owner_bits::FACT_ENGINE_SYNTHETIC != 0
        {
            JIT_GATE[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            crate::vm::aot_route_diag::aot_guard_native_synth().inc();
            return None;
        }
        let key = code as *const Code as usize;
        match self.aot_full_cache.get(&key) {
            Some(Some(_)) => crate::vm::aot_route_diag::aot_cache_ready().inc(),
            Some(None) => {
                crate::vm::aot_route_diag::aot_cache_declined().inc();
                return None;
            }
            None => {
                crate::vm::aot_route_diag::aot_cache_miss().inc();
                // Explicit preparation bypasses hotness. Preserve the legacy adaptive
                // cache policy only for runs which did not request prepared objects.
                let ready = if crate::aot_clif::prepared::enabled() {
                    true
                } else {
                    let call1 = std::env::var_os("RAVA_AOT_FROM_CALL1").is_some();
                    let cur = {
                        let n = self.aot_hot.entry(key).or_insert(0);
                        *n += 1;
                        *n
                    };
                    if call1 {
                        cur >= self.jit_threshold || (cur == 1 && self.aot_disk_has(class, code))
                    } else {
                        cur >= self.jit_threshold
                    }
                };
                if !ready {
                    crate::vm::aot_route_diag::aot_admission_wait().inc();
                    return None;
                }
                crate::vm::aot_route_diag::aot_link_attempt().inc();
                // A failed link is not evidence that this bytecode is unsupported.
                // Metadata may become available later, independently of past attempts.
                let linked = match self.aot_compile_link(code, class) {
                    Ok(linked) => linked,
                    Err(()) => {
                        crate::vm::aot_route_diag::aot_link_error().inc();
                        return None;
                    }
                };
                if linked.is_some() && std::env::var_os("RAVA_AOT_TRACE_RUN").is_some() {
                    eprintln!("[AOT-RUN] {}#{name}{descriptor}", class.file.this_class);
                }
                if crate::pc_profile::enabled() || crate::pc_profile::perf_map_enabled() {
                    if let Some((function, _)) = &linked {
                        let mid = jit::register_compiled_method(&class.file.this_class, name, descriptor);
                        crate::pc_profile::register(function.code_ptr() as usize, function.code_len(), mid);
                    }
                }
                Self::debug_assert_not_gilfree("aot_full_cache");
                let linked = linked.map(|(function, constants)| (function, constants,
                    super::aot_arguments::Layout::new(descriptor, is_static)));
                let declined = linked.is_none();
                self.aot_full_cache.insert(key, linked);
                if declined {
                    return None;
                }
            }
        }
        // Capture the raw code ptr + consts ptr, then drop the cache borrow so the
        // callbacks can re-enter the VM (jit_run does the same with cells).
        let (code_ptr, consts_ptr, rs) = {
            let Some((f, consts, layout)) = self.aot_full_cache.get(&key).and_then(Option::as_ref) else {
                return None;
            };
            (f.code_ptr(), consts.as_ptr(), layout.return_start())
        };
        crate::vm::aot_route_diag::aot_pack_entered().inc();
        let pack_time = crate::aot_clif::diagnostics::pack().start();
        crate::aot_clif::diagnostics::cells()
            .record(code.max_locals as u64 + code.max_stack as u64);
        let box_base = frames::box_len();
        static CELL_POOL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let reuse = jit::env_gate("RAVA_AOT_CELL_POOL", &CELL_POOL);
        let total = code.max_locals as usize + code.max_stack as usize;
        let scratch = if reuse { self.aot_cells.take() } else { Vec::with_capacity(total) };
        let Some(layout) = self.aot_full_cache.get(&key).and_then(Option::as_ref).map(|entry| &entry.2) else {
            crate::vm::aot_route_diag::aot_pack_layout_missing().inc();
            return None;
        };
        // Packing cannot execute guest code or mutate the linked-method cache.
        // End this borrow before a callback can link another method and rehash it.
        let mut cells = arguments.pack_reusing(self, layout, total, scratch);
        drop(pack_time);
        crate::vm::aot_route_diag::aot_pack_completed().inc();
        // Run: register cells as GC roots, stash the VM for callbacks, call run_const.
        self.heap.clear_jit_fault();
        let prev = jit::set_current_vm(self as *mut Vm);
        frames::cells_push((cells.as_ptr(), cells.len()));
        let f: extern "C" fn(*mut i64, *const i64) -> i64 = unsafe { std::mem::transmute(code_ptr) };
        // 20.3 wall-profile: the Cranelift machine code runs here. Nested calls from compiled code
        // that re-enter the interpreter push their own category, so this is compiled SELF-time.
        let bits = {
            let _wp = crate::rava_sampler::enter(crate::rava_sampler::JIT_EXEC);
            let _method = crate::aot_clif::runtime_context::MethodContext::enter(code);
            let _execute_time = crate::aot_clif::diagnostics::execute().start();
            crate::vm::aot_route_diag::aot_execute_entered().inc();
            let bits = f(cells.as_mut_ptr(), consts_ptr);
            crate::vm::aot_route_diag::aot_executed();
            bits
        };
        frames::cells_pop();

        // Decode the result per the return descriptor (BEFORE truncating the box table).
        // NULLRET trace (RAVA_AOT_TRACE_NULLRET): a ref-returning method that yields 0
        // (null) is the invoke/marshalling-bug signature — log which method + verify.
        static TRACE_NULLRET: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if jit::env_gate("RAVA_AOT_TRACE_NULLRET", &TRACE_NULLRET)
            && bits == 0
            && matches!(descriptor.as_bytes().get(rs), Some(b'L') | Some(b'['))
        {
            eprintln!("[AOT-NULLRET] {}#{name}{descriptor} returned null", class.file.this_class);
        }
        let out = match descriptor.as_bytes().get(rs) {
            Some(b'V') => Ok(None),
            Some(b'L') | Some(b'[') => Ok(Some(jit::unbox_ref(self, bits, &descriptor[rs..]))),
            Some(b'J') => Ok(Some(Value::Long(bits))),
            Some(b'D') => Ok(Some(Value::Double(f64::from_bits(bits as u64)))),
            Some(b'F') => Ok(Some(Value::Float(f32::from_bits(bits as u32)))),
            _ => Ok(Some(Value::Int(bits as i32))),
        };
        frames::box_truncate(box_base);
        jit::set_current_vm(prev);
        // No roots or guest references retain a borrow of this inactive buffer.
        // Returning it uses only the host allocator, never guest allocation/GC.
        if reuse { self.aot_cells.give(cells); }
        if let Some(error) = crate::execution_state::pending_error_take() {
            return Some(Err(error));
        }
        Some(out)
    }
}
