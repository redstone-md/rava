//! Forking the chunk-fill loop -- the shape the boot actually spends its time in.
//!
//! OVER THE 500-LINE RULE (594), and this one is unfinished rather than decided. Phase 2 (the
//! worker split) is a clean 477-line span, but it reads eight bindings from phase 1 and
//! produces the results vector, so lifting it needs a plan struct rather than a parameter
//! list. Attempted and reverted twice here; it is the next piece of work on this file, not a
//! measured exception like the two interpreter paths.

use crate::*;

impl Vm {
    pub(crate) fn try_fork_chunk_fill_loop(&mut self, frame: &mut Frame, code: &Code, goto_pc: usize) -> Result<bool, VmError> {
        // 10.2c 3-PHASE FORK: detect the 3-nest coordinate map, then reproduce the remaining iteration
        // space as (1) Inspector — extract span/yMin/height + size `results`, (2) Executor — fan the
        // flat index range 0..total across GIL-free RavaPool-style workers (serial fallback / when
        // RAVA_FORK_WORKERS=1), each reconstructing (x,y,z) and running the REAL body slice
        // `[body_start, compute_end)` on its OWN frame ⇒ identical bytecode/allocs/densities,
        // (3) Reduce-Replay — fold `results` STRICTLY SERIALLY on main with the EXTRACTED constant, so
        // the hash order is deterministic regardless of the map fan-out. Then jump the frame past the
        // loop. Gate off ⇒ never called (dispatch-site gated on chunk_fill_ready).
        let bc = &code.bytecode;
        let Some(m) = crate::jit::match_coord_map_loop(bc, goto_pc) else {
            return Ok(false);
        };
        // Confirm the payload is the density-fill idiom via the pool (pattern-generic — matches the MC
        // noise API signature, not a specific jar): CTX built + passed to a `compute(...)D` interface call.
        let pool = &frame.class.file.pool;
        let ctx_name = pool.class_name(m.ctx_cp).map(|s| s.to_string());
        let compute = pool.member_ref(m.compute_cp).map(|r| (r.owner.to_string(), r.name.to_string(), r.descriptor.to_string()));
        let is_density_fill = matches!(&compute, Some((_, n, d)) if n == "compute" && d.ends_with(")D"));
        if !is_density_fill {
            return Ok(false);
        }
        // Extract the reduction constant C from the pool (`h = h*C + doubleToRawLongBits(d)`), generic —
        // no hard-coded multiplier. Must be a Long.
        let mul_c = match pool.get(m.reduce_mul_cp) {
            Some(Constant::Long(v)) => *v,
            _ => return Ok(false),
        };

        // ── PHASE 1: INSPECTOR ──────────────────────────────────────────────────────────────────
        // Read the loop parameters out of the driver frame's locals. The X back-edge fires AFTER the
        // just-finished x-slab (counted-loop semantics): `cur_x = locals[x_iv]` is the NEXT x, and the
        // accumulator already holds every contribution for x in [0, cur_x). We reproduce x in
        // [cur_x, span_x) only.
        let read_int = |f: &Frame, l: usize| -> Option<i64> {
            match f.locals().get(l) { Some(Value::Int(v)) => Some(*v as i64), _ => None }
        };
        let read_bound = |f: &Frame, b: &crate::jit::RangeBound| -> Option<i64> {
            match b {
                crate::jit::RangeBound::Const(c) => Some(*c as i64),
                crate::jit::RangeBound::Local(l) => read_int(f, *l),
            }
        };
        let (Some(cur_x), Some(span_x), Some(span_z), Some(y_end), Some(y_min)) = (
            read_int(frame, m.x_iv),
            read_bound(frame, &m.x_bound),
            read_bound(frame, &m.z_bound),
            read_bound(frame, &m.y_bound),
            read_int(frame, m.ymin_local),
        ) else {
            return Ok(false);
        };
        let acc0 = match frame.locals().get(m.acc_local) {
            Some(Value::Long(v)) => *v,
            _ => return Ok(false),
        };
        let height = y_end - y_min;
        if height <= 0 || span_z <= 0 || cur_x >= span_x {
            return Ok(false); // nothing left to fork — let the normal loop exit run
        }
        let slab = span_z * height;            // iterations per remaining x
        let total = (span_x - cur_x) * slab;   // remaining iteration count
        if total <= 0 {
            return Ok(false);
        }
        let total = total as usize;

        Self::debug_assert_not_gilfree("numeric_probe_seen (chunk-fill split)");
        if self.numeric_probe_seen.insert((bc.as_ptr() as usize, goto_pc)) {
            eprintln!(
                "[CHUNK-FILL-SPLIT] {} serial 3-phase: x[{cur_x}..{span_x}) z[0..{span_z}) y[{y_min}..{y_end}) slab={slab} total={total} acc=local{} C={mul_c} ctx={} body={}..{} compute_end={}",
                frame.class.file.this_class, m.acc_local,
                ctx_name.as_deref().unwrap_or("?"), m.body_start_pc, m.compute_end_pc, m.compute_end_pc,
            );
        }

        // ── PHASE 2: EXECUTOR — GIL-free PARALLEL work-steal over 0..total (serial fallback) ──────
        // Reconstruct (x,y,z) from the flat index and run the REAL body slice [body_start, compute_end)
        // on a PER-WORKER frame, capturing the one density double into results[index]. Indices are
        // disjoint by construction ⇒ the result writes never alias. This is the SAME worker harness the
        // retired column-fork prototype proved out (a per-index INDEPENDENT guest call that allocates):
        //   • shared *mut Vm — the main thread is PARKED inside `scope` (join barrier) for the whole
        //     region, so no true &mut aliasing; workers only READ shared graph state (compute proven
        //     read-only, 10.2 step-0) + write their own results slot.
        //   • per-worker young-nursery VIEW absorbs each SinglePointContext + the compute-tree's
        //     transient allocs with no cross-worker heap contention (bump-only, no GC in-region;
        //     overflow ⇒ region abort ⇒ serial recompute below).
        //   • main-thread class-layout PRE-WARM so an interpreted worker `new` only READS the shared
        //     layout map (never first-inserts concurrently).
        // Phase 3 (reduce) stays strictly serial on main after the barrier ⇒ hash order is
        // deterministic regardless of how the map fanned out. Bit-exactness is the HASH arbiter.
        // P3-S2: compiled-body worker plan (None → interp body, unchanged semantics).
        #[cfg(feature = "next-gen-gc")]
        let s2 = self.s2_prepare(&*frame, bc, &m, [cur_x as i32, y_min as i32, 0]);
        let mut results = vec![0.0f64; total];
        // RAVA_FORK_WORKERS overrides the worker count (scaling sweep); default = cores-1.
        let workers = std::env::var("RAVA_FORK_WORKERS").ok()
            .and_then(|s| s.trim().parse::<usize>().ok()).filter(|&w| w >= 1)
            .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).saturating_sub(1).max(1))
            .min(total);
        let mut filled = false;
        // RAVA_S2_FORCE1: allow the parallel/S2 path to engage at workers==1 (a single fork
        // thread) — isolates whether the S2 divergence is a CONCURRENCY bug (N>=2) or a
        // single-threaded S2-harness LOGIC bug (would reproduce at 1 fork thread, deterministic).
        let engage_workers = if std::env::var_os("RAVA_S2_FORCE1").is_some() { 1 } else { 2 };
        // Do-no-harm floor inherited from the retired column-fork prototype (its measured
        // verdict: forking a tiny loop pays MORE in spawn + region + pre-warm than it saves).
        const CHUNK_FILL_MIN_TOTAL: usize = 512;
        /// QUARANTINE (GAP-S09, docs/runtime-gap-register.md): the `s2 == None`
        /// INTERPRETER-ON-WORKER arm of this fork (workers drive `opcodes::run`
        /// through the loud `WorkerVmRef::vm_mut_unsound_interp` `&mut Vm` escape)
        /// is OUTSIDE the closed GAP-S09 soundness claim. It is constructible only
        /// through this explicit gate (`RAVA_M3_CHUNK_FILL_INTERP=1`) on top of
        /// `RAVA_M3_CHUNK_FILL_PARALLEL`; without it, an S2-ineligible body takes
        /// the bit-exact serial fallback below. Residual typed-interpreter work is
        /// GAP-A02. The compiled S2 arm is NOT affected: its per-element call is
        /// the shared-borrow `jit_direct_call_worker`, and its remaining `&mut`
        /// uses are the sanctioned locked-exec `jit_call` fallback + fault take.
        fn chunk_fill_interp_arm_quarantine() -> bool {
            static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            *ON.get_or_init(|| std::env::var("RAVA_M3_CHUNK_FILL_INTERP").ok().as_deref() == Some("1"))
        }
        let arm_allowed = s2.is_some() || chunk_fill_interp_arm_quarantine();
        if crate::chunk_fill_parallel() && arm_allowed && workers >= engage_workers && total >= CHUNK_FILL_MIN_TOTAL {
            // Pre-warm per-class layout on main (once) — interpreted worker `new` then only reads it.
            if !self.column_prewarmed {
                for name in self.loader.loaded_class_names() { let _ = self.class_field_layout(&name); }
                self.column_prewarmed = true;
            }
            // Carve the free young nursery into `workers` disjoint per-worker views.
            // 17.2 deterministic engagement. A forced minor-GC HERE to free `pchunk` is UNSAFE: this
            // point has a frame op-borrowed (`frame: &mut Frame`), and arena_safepoint_gc re-borrows
            // every frame in frames::all() (the safepoint invariant at the NurseryFull site is
            // explicitly "no frame is op-borrowed here") — a forced GC would double-borrow + risk a
            // wrong root set → corruption. So instead LOWER the engagement floor to a small per-worker
            // budget: the fork now engages whenever free young ≥ FLOOR·workers (was 256 KiB/worker,
            // which fell to serial on a nursery-tight boot). A worker that exhausts its (smaller)
            // per-worker view still hits the armored NurseryFull abort → serial 10.2b (bit-exact).
            // Pair with a larger RAVA_NURSERY_KB for fat per-worker views + zero OOM aborts.
            const FORK_NURSERY_FLOOR: usize = 64 * 1024;
            let (ybase, ytop, ycap) = crate::nextgen::young_extent();
            let pchunk = ycap.saturating_sub(ytop) / workers;
            if std::env::var_os("RAVA_DBG_PAR").is_some() {
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3 {
                    eprintln!("[FORK-GATE] ycap={} ytop={} free={} workers={} pchunk={} floor={} engage={}",
                        ycap, ytop, ycap.saturating_sub(ytop), workers, pchunk, FORK_NURSERY_FLOOR, pchunk >= FORK_NURSERY_FLOOR);
                }
            }
            if pchunk >= FORK_NURSERY_FLOOR {
                crate::nextgen::young_advance_top(pchunk * workers);
                let pbase = ybase + ytop;
                // P3-S1 SPILL POOL: a DEDICATED low region (young is ~full at fork time — carving
                // it yielded nothing and the exhausted-pool abort killed the region at ~el.870).
                // An overflowing worker claims a fresh view here instead of aborting (dissolves
                // the machcode garbage-continuation class for overflow). Sound for this harness:
                // no per-element worker allocation escapes the element (results are f64s; the
                // COW shadow is cleared per element), so spill addresses never outlive the region.
                {
                    let (sb, sl) = crate::nextgen::spill_region_base();
                    crate::parallel::spill_setup(sb, sl, 1 << 20);
                }
                self.heap.parallel_array_reserve(total.saturating_mul(64).max(256));
                // 11.7: reserve a disjoint per-worker HEAP-Ref (object handle) segment so each worker's
                // transient `Value::Ref` allocations land in its own slice of `objects` — no shared-Vec
                // push/realloc race (the 11.6 ULE). 50k slots/worker = generous headroom over the
                // ~1-few objects/eval the compute tree builds; exhaustion aborts → serial fallback.
                const REF_SLOTS_PER_WORKER: usize = 50_000;
                let obj_arena_base = self.heap.reserve_obj_range(workers * REF_SLOTS_PER_WORKER);
                // 11.13: ARM COW mutation isolation for the region. With RAVA_FORK_COW set at process
                // start (so boot-compiled methods carry the shadow branch), setting jit_ctx.cow_armed
                // makes every worker's guest putfield route to its thread-local shadow instead of the
                // shared graph node — the impure Cache2D/FlatCache memo writes stop racing/corrupting
                // the shared graph. Reads fall shadow→shared (both getfield paths consult the shadow).
                // Main is parked in the scope join, so arming globally only affects the workers.
                self.heap.set_cow_armed(true);
                crate::parallel::clear_abort();
                let echunk = total.div_ceil(workers);
                let vmref = crate::parallel::WorkerVmRef::new(self);
                let results_ptr = results.as_mut_ptr() as usize;
                struct SendArc(std::sync::Arc<crate::class_loader::LoadedClass>);
                unsafe impl Send for SendArc {}
                unsafe impl Sync for SendArc {}
                let class = SendArc(std::sync::Arc::clone(&frame.class));
                let template = frame.locals().to_vec();
                let (bs, ce) = (m.body_start_pc, m.compute_end_pc);
                let ccp = m.compute_cp;
                let (xiv, yiv, ziv) = (m.x_iv, m.y_iv, m.z_iv);
                let class = &class;
                let template = &template;
                let code_ref = code;
                let t0 = std::time::Instant::now();
                // [WORKER-ALIVE] heartbeat state: per-worker last index + live-worker count.
                let fork_trace = crate::parallel::fork_trace_enabled();
                let progress: Vec<std::sync::atomic::AtomicUsize> = if fork_trace {
                    (0..workers).map(|_| std::sync::atomic::AtomicUsize::new(usize::MAX)).collect()
                } else {
                    Vec::new()
                };
                let progress = &progress;
                let remaining = std::sync::atomic::AtomicUsize::new(workers);
                let remaining = &remaining;
                std::thread::scope(|scope| {
                    // Watcher: every 5s print each worker's last index + the resolve-lock owner —
                    // names the stuck worker's frozen position (mid-op = blocking native or
                    // poll-free compiled kernel). Exits when all workers have finished.
                    if fork_trace {
                        scope.spawn(move || {
                            let mut secs = 0u32;
                            while remaining.load(std::sync::atomic::Ordering::Acquire) > 0 && secs < 900 {
                                std::thread::sleep(std::time::Duration::from_secs(5));
                                secs += 5;
                                let idx: Vec<i64> = progress
                                    .iter()
                                    .map(|a| {
                                        let v = a.load(std::sync::atomic::Ordering::Relaxed);
                                        if v == usize::MAX { -1 } else { v as i64 }
                                    })
                                    .collect();
                                eprintln!(
                                    "[WORKER-ALIVE] t={secs}s idx={idx:?} live={} lock_owner={} abort={}",
                                    remaining.load(std::sync::atomic::Ordering::Relaxed),
                                    crate::jit::lock_owner_dbg(),
                                    crate::parallel::abort_requested(),
                                );
                            }
                        });
                    }
                    // RAVA_S2_SEQ: keep `workers` partitions but join each worker before
                    // spawning the next — separates a LIVE data race (seq ⇒ correct) from a
                    // per-worker SETUP bug (seq ⇒ still wrong, like the concurrent run).
                    let s2_seq = std::env::var_os("RAVA_S2_SEQ").is_some();
                    for w in 0..workers {
                        let wlo = w * echunk;
                        let whi = ((w + 1) * echunk).min(total);
                        if wlo >= whi {
                            if fork_trace {
                                remaining.fetch_sub(1, std::sync::atomic::Ordering::Release);
                            }
                            continue;
                        }
                        let h = scope.spawn(move || {
                            vmref.install_current();
                            crate::gil::set_worker();
                            crate::frames::register();
                            crate::parallel::set_gilfree(true);
                            crate::parallel::enter_region();
                            crate::nextgen::worker_tlab_view(pbase + w * pchunk, pchunk);
                            // 11.7: install THIS worker's disjoint object-handle arena segment.
                            crate::parallel::arena_setup(
                                obj_arena_base + (w * REF_SLOTS_PER_WORKER) as u32,
                                REF_SLOTS_PER_WORKER as u32,
                            );
                            let vm = unsafe { vmref.vm_mut_unsound_interp() };
                            // Snapshot the already-reserved descriptor bases and
                            // install this worker's exact COW Bloom. Compiled
                            // callees propagate the existing JitCtx ABI argument.
                            let worker_jit_ctx = vm.heap.install_worker_jit_ctx();
                            // P3-S2 COMPILED-BODY worker: the whole element = Rust ctx-alloc +
                            // 3 arena_putfields + ONE frozen-direct-cache compiled compute call.
                            // ZERO interpreter — kills the measured Arc-churn saturation wall.
                            if let Some((cid, nf, fids, recv_cell, srcs)) = s2 {
                                // RAVA_HOLDER_PROBE=<hex addr>: after this worker's FIRST element,
                                // name every holder of that arena address — the COW-divergence
                                // hunt's "who keeps the Spline$Point HEAD alive across elements".
                                let holder_probe: Option<u64> = std::env::var("RAVA_HOLDER_PROBE")
                                    .ok()
                                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
                                for index in wlo..whi {
                                    if index == wlo + 1 {
                                        if let Some(addr) = holder_probe {
                                            let hs = vm.heap.find_arena_holders(addr);
                                            eprintln!(
                                                "[HOLDER-PROBE] w={w} addr={addr:#x} holders({}): {:?}",
                                                hs.len(),
                                                hs
                                            );
                                        }
                                    }
                                    if fork_trace {
                                        progress[w].store(index, std::sync::atomic::Ordering::Relaxed);
                                    }
                                    if crate::parallel::abort_requested() {
                                        break;
                                    }
                                    let i = index as i64;
                                    let x = cur_x + i / slab;
                                    let within = i % slab;
                                    let z = within / height;
                                    let y = y_min + within % height;
                                    let coords = [x as i32, y as i32, z as i32];
                                    // ctx alloc with spill refill (same pool the collect-guard uses).
                                    let p = loop {
                                        match crate::nextgen::tlab_try_alloc(cid, nf) {
                                            Some(p) => break Some(p),
                                            None => {
                                                if let Some(base) =
                                                    crate::parallel::spill_claim(1 << 20)
                                                {
                                                    crate::nextgen::worker_tlab_view(base, 1 << 20);
                                                    continue;
                                                }
                                                break None;
                                            }
                                        }
                                    };
                                    let Some(p) = p else {
                                        crate::parallel::request_abort_overflow();
                                        break;
                                    };
                                    for k in 0..3usize {
                                        crate::opcodes::arena_putfield(
                                            p,
                                            fids[k],
                                            &Value::Int(coords[srcs[k] as usize]),
                                        );
                                    }
                                    let rawc = [recv_cell, p.addr() as i64];
                                    let cptr = std::sync::Arc::as_ptr(&class.0);
                                    // NOTE: routing this through jit_call (the interp dispatch path)
                                    // instead of jit_direct_call was MEASURED identical (3d7814) —
                                    // the S2 divergence is NOT the dispatch shortcut.
                                    let s2_probe = std::env::var_os("RAVA_S2_PROBE").is_some()
                                        && index == wlo;
                                    if s2_probe {
                                        let gx = crate::opcodes::arena_getfield(p, fids[0]);
                                        let gy = crate::opcodes::arena_getfield(p, fids[1]);
                                        let gz = crate::opcodes::arena_getfield(p, fids[2]);
                                        eprintln!("[S2-PROBE] w={w} idx={index} coords={coords:?} srcs={srcs:?} recv_cell={recv_cell:#x} p={:#x} ctx.f={gx:?},{gy:?},{gz:?}", p.addr());
                                    }
                                    // GAP-S09 stage 4: the direct call goes through the SHARED-
                                    // borrow worker arm (WorkerVmRef → Vm::jit_direct_call_worker,
                                    // `&self`) — behaviour-identical to the old `&mut` call under
                                    // gilfree (hit → invoke; miss/mega → None, no resolve/insert),
                                    // but the type system now proves this hot per-element call
                                    // mutates no shared Vm state. The locked `jit_call` fallback
                                    // below keeps the loud `&mut` escape (stage-5 quarantine).
                                    let bits = match vmref.jit_direct_call_worker(cptr, ccp, 3, &rawc) {
                                        Some(Ok(b)) => { if s2_probe { eprintln!("[S2-PROBE] w={w} arm=DIRECT bits={b:#x} f64={}", f64::from_bits(b as u64)); } b }
                                        Some(Err(_)) => { crate::parallel::request_abort(); break; }
                                        None => match vm.jit_call(cptr, ccp, 3, &rawc) {
                                            Ok(b) => { if s2_probe { eprintln!("[S2-PROBE] w={w} arm=JITCALL bits={b:#x} f64={}", f64::from_bits(b as u64)); } b }
                                            Err(_) => { crate::parallel::request_abort(); break; }
                                        },
                                    };
                                    // Compiled callbacks report guest faults through this worker's
                                    // TLS slot because machine code cannot unwind a VmError. Never
                                    // publish the placeholder bits: abort the detached transaction
                                    // and let the coordinator repeat the element serially.
                                    if vm.jit_pending_take().is_some()
                                        || crate::parallel::abort_requested()
                                    {
                                        crate::parallel::request_abort();
                                        break;
                                    }
                                    unsafe {
                                        *(results_ptr as *mut f64).add(index) =
                                            f64::from_bits(bits as u64);
                                    }
                                    crate::parallel::shadow_clear();
                                }
                                crate::opcodes::run_stop_clear();
                                drop(worker_jit_ctx);
                                crate::nextgen::worker_tlab_clear();
                                crate::parallel::arena_clear();
                                crate::parallel::leave_region();
                                crate::parallel::set_gilfree(false);
                                crate::frames::unregister();
                                crate::jit::set_current_vm(std::ptr::null_mut());
                                if fork_trace {
                                    remaining.fetch_sub(1, std::sync::atomic::Ordering::Release);
                                    eprintln!("[WORKER-EXIT] w={w} range={wlo}..{whi} (S2)");
                                }
                                return;
                            }
                            let mut wframe = {
                                // ONE buffer: the template IS the locals, and the operand region starts after
                                // them (stage 1 of the contiguous stack).
                                let tpl = template.clone();
                                let n = tpl.len();
                                // The worker runs THIS method's body, so the frame's code is the
                                // one the fork was detected in.
                                let mut wf = Frame::from_parts(code_ref, tpl, n, std::sync::Arc::clone(&class.0));
                                wf.pc = bs;
                                wf
                            };
                            for index in wlo..whi {
                                if fork_trace {
                                    progress[w].store(index, std::sync::atomic::Ordering::Relaxed);
                                }
                                if crate::parallel::abort_requested() {
                                    break;
                                }
                                let i = index as i64;
                                let x = cur_x + i / slab;
                                let within = i % slab;
                                let z = within / height;
                                let y = y_min + within % height;
                                wframe.pc = bs;
                                wframe.clear_stack();
                                wframe.locals_mut()[xiv] = Value::Int(x as i32);
                                wframe.locals_mut()[yiv] = Value::Int(y as i32);
                                wframe.locals_mut()[ziv] = Value::Int(z as i32);
                                crate::opcodes::run_stop_set(ce);
                                match crate::opcodes::run(vm, code_ref, &mut wframe) {
                                    Ok(crate::opcodes::RunExit::Bounded) => {}
                                    // 11.8 abort diagnostics (Points 2 & 3): name the exact cause.
                                    Ok(crate::opcodes::RunExit::NurseryFull) => {
                                        eprintln!("[RAVA-ABORT-DIAG] Worker Nursery full! RunExit::NurseryFull triggered.");
                                        crate::parallel::request_abort();
                                        break;
                                    }
                                    Ok(crate::opcodes::RunExit::ClinitPending(_)) => {
                                        eprintln!("[RAVA-ABORT-DIAG] Worker hit ClinitPending (class <clinit> off-main — workers can't drive it).");
                                        crate::parallel::request_abort();
                                        break;
                                    }
                                    Ok(_) => {
                                        eprintln!("[RAVA-ABORT-DIAG] Worker unexpected RunExit (Call/Yield/Normal) at the compute body boundary.");
                                        crate::parallel::request_abort();
                                        break;
                                    }
                                    Err(vm_err) => {
                                        eprintln!("[RAVA-ABORT-DIAG] Guest exception on worker: {:?}", vm_err);
                                        // 11.9: resolve the thrown Throwable's Java class + guest backtrace.
                                        // Read-only `&self` accessors on the cold abort path (fires once,
                                        // then the region tears down); no lock needed.
                                        if let VmError::Throw(exc) = &vm_err {
                                            eprintln!("[RAVA-ABORT-DIAG] EXCEPTION CLASS: {}", vm.value_class_name(exc));
                                            eprintln!("[RAVA-ABORT-DIAG] GUEST BACKTRACE: \n{}", vm.backtrace());
                                        }
                                        crate::parallel::request_abort();
                                        break;
                                    }
                                }
                                match wframe.pop_opt() {
                                    // SAFETY: disjoint index per worker ⇒ no aliasing write.
                                    Some(Value::Double(d)) => unsafe { *(results_ptr as *mut f64).add(index) = d; },
                                    other => {
                                        // 11.8 abort diagnostic (Point 4): body slice left no double.
                                        eprintln!("[RAVA-ABORT-DIAG] Worker body slice produced no Double on stack: {:?}", other);
                                        crate::parallel::request_abort();
                                        break;
                                    }
                                }
                                // 11.13: drop this eval's shadow so each coord is an independent pure
                                // computation (the memo written for one (x,z) never leaks to the next).
                                crate::parallel::shadow_clear();
                            }
                            crate::opcodes::run_stop_clear();
                            drop(worker_jit_ctx);
                            crate::nextgen::worker_tlab_clear();
                            crate::parallel::arena_clear(); // 11.7: drop this worker's object arena
                            crate::parallel::leave_region();
                            crate::parallel::set_gilfree(false);
                            crate::frames::unregister();
                            crate::jit::set_current_vm(std::ptr::null_mut());
                            // P3-S1 convergence probe (RAVA_FORK_TRACE): every worker prints its
                            // exit. The locked-exec region hangs >330s — a missing [WORKER-EXIT]
                            // line names the STUCK worker (fault-retry loop in a compiled kernel,
                            // lock convoy, or gil::acquire wait against parked main), which the
                            // scope join then waits on forever.
                            if fork_trace {
                                remaining.fetch_sub(1, std::sync::atomic::Ordering::Release);
                                eprintln!("[WORKER-EXIT] w={w} range={wlo}..{whi}");
                            }
                        });
                        if s2_seq {
                            let _ = h.join();
                        }
                    }
                });
                self.heap.set_cow_armed(false); // 11.13: disarm COW (region done; serial fallback + main run normal)
                crate::parallel::spill_clear(); // P3-S1: spill pool is region-scoped
                self.heap.parallel_array_commit();
                crate::cache::clear_fork_cache();
                let elapsed = t0.elapsed().as_millis();
                if crate::parallel::abort_requested() {
                    crate::parallel::clear_abort(); // region tore down (nursery overflow) → recompute serially
                    // Worker pending slots are thread-local and were detached on worker exit.
                    // The coordinator entered pending-free; clear its own slot defensively and
                    // let a real guest exception re-raise during the serial retry.
                    self.jit_pending_clear();
                    self.heap.clear_jit_fault();
                } else {
                    filled = true;
                    use std::sync::atomic::Ordering::Relaxed;
                    eprintln!(
                        "[CHUNK-FILL-PAR] workers={workers} total={total} fill_ms={elapsed} (GIL-free parallel) freeze: direct_hit={} hit={} miss={} stored={} locked_calls={} dmiss_locks={}",
                        crate::jit::freeze_resolve::WORKER_DIRECT_HIT.load(Relaxed),
                        crate::jit::freeze_resolve::WORKER_HIT.load(Relaxed),
                        crate::jit::freeze_resolve::WORKER_MISS.load(Relaxed),
                        crate::jit::freeze_resolve::STORED.load(Relaxed),
                        crate::jit::freeze_resolve::WORKER_LOCKED_CALLS.load(Relaxed),
                        crate::gil::worker_miss_count(),
                    );
                    eprintln!(
                        "[CHUNK-FILL-PAR] cow2: putstatic={}",
                        crate::parallel::CNT_PUTSTATIC.load(Relaxed)
                    );
                    eprintln!(
                        "[CHUNK-FILL-PAR] locked-top: {} | wsnative_hit={}",
                        crate::jit::freeze_resolve::locked_tally_top(8),
                        crate::jit::freeze_resolve::WORKER_WSNATIVE_HIT.load(Relaxed)
                    );
                    eprintln!(
                        "[CHUNK-FILL-PAR] cow: shadow_put={} shared_put={} array_skip={} idhash={} shadow_get_hit={} undo={} owned_true={} owned_false={}",
                        crate::parallel::CNT_SHADOW_PUT.load(Relaxed),
                        crate::parallel::CNT_SHARED_PUT.load(Relaxed),
                        crate::parallel::CNT_ARRAY_SKIP.load(Relaxed),
                        crate::parallel::CNT_IDHASH.load(Relaxed),
                        crate::parallel::CNT_SHADOW_GET_HIT.load(Relaxed),
                        crate::parallel::CNT_UNDO.load(Relaxed),
                        crate::parallel::CNT_OWNED_TRUE.load(Relaxed),
                        crate::parallel::CNT_OWNED_FALSE.load(Relaxed),
                    );
                    if crate::parallel::cow_profile_enabled() {
                        let (get_calls, get_ns, put_calls, put_ns) =
                            crate::jit::cow_callback_profile();
                        eprintln!(
                            "[CHUNK-FILL-PAR] cow-profile: get_calls={get_calls} get_ms={:.3} put_calls={put_calls} put_ms={:.3} shadow_miss={} clears={} clear_entries={} max_entries={}",
                            get_ns as f64 / 1_000_000.0,
                            put_ns as f64 / 1_000_000.0,
                            crate::parallel::CNT_SHADOW_GET_MISS.load(Relaxed),
                            crate::parallel::CNT_SHADOW_CLEAR.load(Relaxed),
                            crate::parallel::CNT_SHADOW_CLEAR_ENTRIES.load(Relaxed),
                            crate::parallel::CNT_SHADOW_MAX_ENTRIES.load(Relaxed),
                        );
                    }
                }
            }
        }
        if !filled {
            // SERIAL fallback (10.2b path): run the body slice on the DRIVER frame per index. Also the
            // measured baseline when RAVA_FORK_WORKERS=1.
            let t0 = std::time::Instant::now();
            for index in 0..total {
                let i = index as i64;
                let x = cur_x + i / slab;
                let within = i % slab;
                let z = within / height;
                let y = y_min + within % height;
                frame.locals_mut()[m.x_iv] = Value::Int(x as i32);
                frame.locals_mut()[m.y_iv] = Value::Int(y as i32);
                frame.locals_mut()[m.z_iv] = Value::Int(z as i32);
                frame.pc = m.body_start_pc;
                frame.clear_stack();
                crate::opcodes::run_stop_set(m.compute_end_pc);
                match crate::opcodes::run(self, code, frame) {
                    Ok(crate::opcodes::RunExit::Bounded) => {}
                    Ok(_) | Err(_) => {
                        crate::opcodes::run_stop_clear();
                        return Ok(false); // unexpected body exit → fall back to the original loop
                    }
                }
                match frame.pop_opt() {
                    Some(Value::Double(d)) => results[index] = d,
                    _ => {
                        crate::opcodes::run_stop_clear();
                        return Ok(false);
                    }
                }
            }
            crate::opcodes::run_stop_clear();
            eprintln!("[CHUNK-FILL-SER] workers=1 total={total} fill_ms={} (serial baseline)", t0.elapsed().as_millis());
        }

        // RAVA_DUMP_RESULTS=<path>: dump the per-element density results (raw f64 bits, LE) —
        // diffing the 1-worker (8790) file vs the 4-worker (3d7814) file names the FIRST
        // divergent element index, localizing the COW divergence to a coordinate + worker.
        if let Ok(path) = std::env::var("RAVA_DUMP_RESULTS") {
            let mut bytes = Vec::with_capacity(results.len() * 8);
            for &d in &results {
                bytes.extend_from_slice(&d.to_bits().to_le_bytes());
            }
            let _ = std::fs::write(&path, &bytes);
            eprintln!("[DUMP-RESULTS] wrote {} elements to {path}", results.len());
        }
        // ── PHASE 3: REDUCE-REPLAY ──────────────────────────────────────────────────────────────
        // Fold in the SAME nested order the serial loop used (index ascending == x,z,y ascending),
        // starting from the accumulator's live value (x in [0, cur_x) already folded). Java `long`
        // arithmetic ⇒ wrapping; `Double.doubleToRawLongBits` == `f64::to_bits`.
        let mut h = acc0;
        for &d in &results {
            h = h.wrapping_mul(mul_c).wrapping_add(d.to_bits() as i64);
        }
        frame.locals_mut()[m.acc_local] = Value::Long(h);

        // Jump the driver past the whole X-loop: the remaining iterations are now done.
        frame.clear_stack();
        frame.pc = m.loop_exit_pc;
        Ok(true)
    }
}
