//! Forking a parallel apply over a leaf, and the S2 preparation it needs.

use crate::*;

impl Vm {
    /// G1 — GIL-FREE parallel apply of a NO-ALLOC LEAF per-element method over arena elements.
    /// The compiled `m()` runs on scoped worker threads, each element's field writes hitting only
    /// that element's own memory (disjoint) and reads touching shared-immutable state. Sound for
    /// this class: no alloc → no GC/realloc mid-region (so raw arena access is stable); no calls →
    /// no transitive impurity; disjoint int-field writes → no card-barrier/shared mutation (the
    /// [[rava-parallel-jit-win]] raw-shared-access model). Receiver marshalling is driver-side +
    /// Vm-free (arena → raw addr). Returns Ok(true) if it ran the loop in parallel, Ok(false) to
    /// fall back to the serial apply (ineligible: not compiled yet / has alloc or calls / a
    /// non-arena or null element). Gated by the caller (RAVA_FORK_PARALLEL).
    ///
    /// GAP-S01: `classes` is the caller's proved batch — every DISTINCT receiver class, all of
    /// which resolve `mn md` to ONE identical bytecode method. The safety proofs below run for
    /// every class (nested `this`-dispatch devirtualises per receiver class).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn try_parallel_apply_leaf(&mut self, elems: &[Value], classes: &[String], mn: &str, md: &str) -> Result<bool, VmError> {
        let Some(ecls) = classes.first().map(String::as_str) else { return Ok(false) };
        // Resolve + fetch the compiled entry; ineligible until compiled (early warmup reps).
        let decl = match self.resolve_method(ecls, mn, md) { Ok(Some(d)) => d, _ => return Ok(false) };
        let Some(code) = decl.file.method(mn, md).and_then(|m| m.code.as_ref()) else { return Ok(false) };
        // GAP-S01 pin check: every class in the proved batch must still resolve to this exact
        // bytecode method (defensive re-verification at the execution boundary).
        for cls in &classes[1..] {
            let d = match self.resolve_method(cls, mn, md) { Ok(Some(d)) => d, _ => return Ok(false) };
            match d.file.method(mn, md).and_then(|m| m.code.as_ref()) {
                Some(c) if std::ptr::eq(c, code) => {}
                _ => return Ok(false),
            }
        }
        // NO-GC eligibility: the method + its whole bytecode call-tree must be alloc-free (so no
        // GC/realloc can move the shared heap mid-region) and free of shared writes / sync /
        // throw. Non-leaf is allowed — a warm callee runs via `jit_direct_call` (per-thread
        // tlcache + direct compiled call, no shared mutation on the success path).
        // Fork-safety: no-alloc (G1/G2a → workers need no nursery) OR allocating (G3 → workers
        // bump a view of main's young). Both require a card-free, side-effect-free call-tree.
        // GAP-S04: the structural fork-safety proof is mandatory — there is no optimistic
        // bypass. An unproved tree always returns to the unchanged serial apply.
        // GAP-S01: the proof must hold for EVERY receiver class in the batch.
        let needs_partition = {
            let mut all_no_alloc = true;
            for cls in classes {
                let no_alloc = {
                    let mut s = std::collections::HashSet::new();
                    self.method_fork_safe(cls, mn, md, false, 0, &mut s)
                };
                let alloc_ok = no_alloc || {
                    let mut s = std::collections::HashSet::new();
                    self.method_fork_safe(cls, mn, md, true, 0, &mut s)
                };
                if !alloc_ok {
                    if std::env::var_os("RAVA_FORK_TRACE").is_some() {
                        eprintln!("[FORK-PAR-SKIP] {cls}.{mn}{md} not fork-safe");
                    }
                    return Ok(false);
                }
                all_no_alloc &= no_alloc;
            }
            !all_no_alloc
        };
        let key = code as *const Code as usize;
        let fptr = match self.jit_cache.get(&key) {
            Some(jit::JitStatus::Compiled(f)) => f.raw() as usize,
            other => {
                if std::env::var_os("RAVA_FORK_TRACE").is_some() {
                    eprintln!("[FORK-PAR-SKIP] {ecls}.{mn}{md} not compiled (status={})",
                        match other { Some(jit::JitStatus::Failed) => "Failed", Some(_) => "other", None => "none" });
                }
                return Ok(false);
            }
        };
        let buf_len = (code.max_locals.saturating_add(code.max_stack)).max(1) as usize;
        // Driver-side receiver marshalling (Vm-free): arena Obj → raw addr; heap Ref → tagged
        // handle. Any null / other value → ineligible (fall back to serial).
        let mut cell0: Vec<i64> = Vec::with_capacity(elems.len());
        for e in elems {
            match e {
                Value::Obj(ptr) => cell0.push(ptr.addr() as i64),
                Value::Ref(h) => cell0.push((*h as i64) | crate::value::REF_TAG),
                _ => return Ok(false),
            }
        }
        let vmref = crate::parallel::WorkerVmRef::new(self);
        // 9.2e.2 diagnostic: RAVA_FORK_WORKERS overrides the worker count to sweep the scaling curve
        // (a hard lock stays ~1x even at 2 workers; a physical limit scales then plateaus). Default =
        // available_parallelism-1 (unchanged behaviour when unset).
        let explicit_workers = std::env::var("RAVA_FORK_WORKERS")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|&w| w >= 1);
        let base_workers = explicit_workers.unwrap_or_else(|| {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).saturating_sub(1).max(1)
        });
        let workers = base_workers.min(elems.len()).max(1);
        let n = elems.len();
        let echunk = n.div_ceil(workers); // elements per worker

        // G3: an allocating method needs each worker a disjoint sub-range (VIEW) of main's young
        // nursery so its allocations land in main's persistent heap. Partition [top, cap); too
        // little free nursery → fall back to serial. Reserve the whole span on main up front.
        // G1.4: the young `top` BEFORE the partition reservation, so an abort can retreat to it and
        // un-reserve the span (leaving its garbage bytes ABOVE top, never GC-scanned).
        let mut reserve_top: Option<usize> = None;
        let (pbase, pchunk) = if needs_partition {
            let (ybase, ytop, ycap) = crate::nextgen::young_extent();
            let free = ycap.saturating_sub(ytop);
            reserve_top = Some(ytop);
            const MIN_CHUNK: usize = 256 * 1024;
            // G1.3 RESERVATION BOUND: the original `bchunk = free / workers` reserved the WHOLE free
            // young for EVERY fork — fine for a large-n kernel (ChunkFill fills a chunk) but catastrophic
            // for the small-n initCache loops G1.1 unblocked (n=2 in practice): reserving 22 MiB of young
            // for 2 elements, repeatedly, exhausts the nursery → GC thrash → the boot grinds. Size each
            // worker's view to its ACTUAL need — echunk elements × a generous per-element budget — capped
            // at the fair share (free / workers). An under-estimate makes a worker's TLAB view
            // overflow, which aborts the region BEFORE any publication and retries serially for
            // this invocation only (GAP-S05: never recorded, never routed). Over-reservation for
            // tiny loops is gone.
            let echunk = n.div_ceil(workers);
            const PER_ELEM_BUDGET: usize = 4 * 1024 * 1024; // generous per-element capacity bound
            let fair = free / workers;
            let bchunk = echunk
                .saturating_mul(PER_ELEM_BUDGET)
                .max(MIN_CHUNK)
                .min(fair);
            if bchunk < MIN_CHUNK {
                return Ok(false); // not enough nursery to partition → serial
            }
            crate::nextgen::young_advance_top(bchunk * workers);
            crate::parallel::clear_abort();
            (ybase + ytop, bchunk)
        } else {
            (0, 0)
        };

        // G3.5: if the per-element method allocates PRIMITIVE arrays, open a parallel array region
        // — workers' `newarray` claims pre-reserved slots (the heap array Vecs aren't concurrent-
        // safe to push). Only for array-allocating trees (skip the reserve otherwise). Over-
        // reserve (8/elem); exhaustion → abort → serial. GAP-S01: any class's tree counts.
        let array_region = needs_partition && classes.iter().any(|cls| {
            let mut s = std::collections::HashSet::new();
            self.fork_tree_has_newarray(cls, mn, md, 0, &mut s)
        });
        if array_region {
            self.heap.parallel_array_reserve(n.saturating_mul(8).max(64));
        }

        // GAP-S07: the region's DISPATCH FREEZE. (a) Every worker — allocating or not —
        // runs with the gil-free guard armed, so an uncompiled-callee dispatch (the path
        // that would resolve/compile/interpret on a worker) aborts the region before any
        // publication. (b) The loader hierarchy epoch is captured here and re-checked
        // after join: a moved epoch means something loaded a class mid-region, every
        // closure-based admission proof may be stale, and the region result is discarded
        // for an unchanged serial retry. Compiled entries themselves are append-only
        // (`jit_cache` never replaces a `Compiled` entry), so `fptr` cannot dangle.
        let region_epoch = self.loader.epoch();
        crate::parallel::clear_abort();
        struct Sh { cell0: *const i64, n: usize, fptr: usize, buf_len: usize,
                    vm: crate::parallel::WorkerVmRef,
                    echunk: usize, needs_partition: bool, pbase: usize, pchunk: usize, gilfree_on: bool }
        unsafe impl Sync for Sh {}
        let sh = Sh { cell0: cell0.as_ptr(), n, fptr, buf_len, vm: vmref,
                      echunk, needs_partition, pbase, pchunk, gilfree_on: true };
        let sh = &sh;
        if std::env::var_os("RAVA_FORK_TRACE").is_some() {
            eprintln!("[FORK-PAR] elem={ecls} m={mn}{md} n={n} workers={workers} alloc={needs_partition} (GIL-free)");
        }
        // M3 COW: ARM the compiled field-access COW branch for the parallel phase — workers
        // now redirect putfield→shadow / getfield→shadow-first (read [ctx+12]!=0). Disarmed
        // immediately after the scope so the main thread returns to the inline fast path.
        if crate::parallel::cow_gate() {
            self.heap.set_cow_armed(true);
        }
        std::thread::scope(|scope| {
            for w in 0..workers {
                let lo = w * sh.echunk;
                let hi = ((w + 1) * sh.echunk).min(sh.n);
                if lo >= hi { continue; }
                scope.spawn(move || {
                    // GAP-S09 stage 4: the whole worker setup — CURRENT_VM publication
                    // (the compiled field callbacks resolve through it; the worker never
                    // mutates shared Vm state: field ops are raw + disjoint, heap writes
                    // are primitive, enforced by method_fork_safe), worker marking,
                    // frames TLS, the gil-free flag + region (arms the jit_call
                    // worker-guard; allocations bump this worker's VIEW of main's young,
                    // overflow → tlab abort → serial retry), the private TLAB view and
                    // the stage-2 private JitCtx (fault isolation: every coordinator
                    // reserve — young_advance_top / parallel_array_reserve /
                    // set_cow_armed — completed before the spawn, so the snapshot bases
                    // are frozen) — is now ONE RAII pair: build ctx → loop → drop ctx.
                    // `WorkerContext::drop` performs the exact old teardown order
                    // (JitCtx guard, TLAB view, region+flag, frames TLS, CURRENT_VM) on
                    // any exit, including a panic unwinding out of this closure.
                    let ctx = crate::parallel::WorkerContext::enter(
                        sh.vm,
                        sh.gilfree_on,
                        sh.needs_partition.then(|| (sh.pbase + w * sh.pchunk, sh.pchunk)),
                    );
                    // The compiled root must READ the same private ctx the callbacks latch:
                    // pass the ACTIVE (worker) JitCtx — same snapshot values the shared
                    // block held at spawn — instead of the coordinator-computed shared
                    // address, so a fault short-circuits THIS worker's native stores only.
                    let desc = ctx.active_jit_ctx_ptr();
                    let mut cells = vec![0i64; sh.buf_len];
                    for i in lo..hi {
                        if sh.gilfree_on && crate::parallel::abort_requested() {
                            break; // a sibling worker aborted (overflow / G1.1 uncompiled-callee) → stop
                        }
                        for c in cells.iter_mut() { *c = 0; }
                        cells[0] = unsafe { *sh.cell0.add(i) };
                        // G1.1: a garbage-continuation after an abort (the compiled root runs to its
                        // return reading discarded 0s) could panic on a degenerate value — catch it and
                        // convert to an abort so the region unwinds cleanly to a serial retry instead of
                        // propagating the panic through the scope join (which would abort the process).
                        let fptr = sh.fptr;
                        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            unsafe { crate::jit::call_raw(fptr as *const u8, &mut cells, desc); }
                        }));
                        if ran.is_err() {
                            crate::parallel::request_abort();
                            break;
                        }
                    }
                    // GAP-S09 stage 4: teardown is `WorkerContext::drop` — the same
                    // sequence the explicit block here used to perform (private JitCtx
                    // first, so any latched fault dies with the worker; then TLAB view,
                    // region + gil-free flag, frames TLS, CURRENT_VM).
                    drop(ctx);
                });
            }
        });
        // M3 COW: DISARM — the main thread returns to the inline field fast path.
        if crate::parallel::cow_gate() {
            self.heap.set_cow_armed(false);
        }
        if std::env::var_os("RAVA_FORK_TRACE").is_some() {
            // Force-compile thesis arbiter: cumulative worker VM_RESOLVE_LOCK takes. If this is
            // large the megamorphic compute dispatch still evicts the PIC on every call and the
            // workers serialise on the global lock — coarser partition / force-compile can't fix it.
            // P3-S1 gate: freeze_hit should absorb the traffic (lock-takes → ~0 as the table warms).
            use std::sync::atomic::Ordering::Relaxed;
            eprintln!(
                "[FORK-PAR] worker_dispatch_miss(cumulative)={} freeze: worker_hit={} worker_miss={} stored={}",
                crate::gil::worker_miss_count(),
                crate::jit::freeze_resolve::WORKER_HIT.load(Relaxed),
                crate::jit::freeze_resolve::WORKER_MISS.load(Relaxed),
                crate::jit::freeze_resolve::STORED.load(Relaxed),
            );
        }
        // Close the array region (trim unclaimed slots) whether or not we aborted.
        if array_region {
            self.heap.parallel_array_commit();
        }
        // Empty the shared fork guava-cache (its region-allocated values must not dangle into the
        // next region; the real cache $map is untouched — memoisation restarts next fork).
        crate::cache::clear_fork_cache();
        // GAP-S07: a moved hierarchy epoch invalidates every closure-based admission proof
        // — treat it exactly like a worker abort (discard, unchanged serial retry; element
        // writes are deterministic functions of immutable-in-region shared state under the
        // admission proof, so serial re-execution is idempotent).
        let epoch_moved = self.loader.epoch() != region_epoch;
        if crate::parallel::abort_requested() || epoch_moved {
            // GAP-S05: an abort affects ONLY this invocation — it is never recorded and can
            // never route a future admission decision (no blacklist, no backoff). The overflow
            // flag is read purely for the diagnostic trace below.
            let was_overflow = crate::parallel::overflow_requested();
            crate::parallel::clear_abort();
            // The worker-local pending slots were consumed/detached during teardown.
            // Clear only the coordinator's slot defensively before its serial retry.
            self.jit_pending_clear();
            self.heap.clear_jit_fault();
            // G1.4 ROOT-CAUSE FIX: retreat main's young top to un-reserve the aborted partition span.
            // Without this, main's top stays advanced past the aborted region's partial/stale bytes,
            // and the next GC parses that garbage as live objects → heap corruption → the netty enum
            // crash. On abort nothing from the fork is kept (serial retry re-does it), so this is sound.
            if let Some(top) = reserve_top {
                crate::nextgen::young_set_top(top);
            }
            if std::env::var_os("RAVA_FORK_TRACE").is_some() {
                let kind = if epoch_moved {
                    "hierarchy epoch moved (GAP-S07 freeze violated)"
                } else if was_overflow {
                    "nursery OVERFLOW"
                } else {
                    "guard (uncompiled callee)"
                };
                eprintln!("[FORK-PAR-ABORT] {ecls}.{mn}{md} {kind} → serial retry (does not affect future admission)");
            }
            return Ok(false); // abort OR stale freeze → serial retry, no partial result kept
        }
        Ok(true)
    }

    /// M3 T2 step 3b — FORK a counted loop `for (i=…; i<hi; i++) { <body> }` at its backward
    /// `goto` (top-test), running the offloadable BODY across workers for disjoint index
    /// sub-ranges. Sound for the minimal safe body (see `counted_loop_body_simple`): BRANCHLESS,
    /// CALL-FREE, ALLOC-FREE, primitive `arr[iv]` writes + shared reads only — so each worker only
    /// does disjoint array-cell writes + immutable reads (no GC, no shared-metadata mutation, no
    /// per-class RefCell touch). Returns Ok(true) if it forked (caller must not take the goto —
    /// `frame` is left mid last-iteration for the interpreter to finish serially, leaving the
    /// driver locals exactly as a serial loop would).
    #[cfg(feature = "next-gen-gc")]
    /// M3 10.1b(B) SCAFFOLD (INERT) — fork the coordinate chunk-noise-fill map. The target pattern
    /// (10.2 detector): a nested counted loop over block coords `[x][z][y]` whose body evaluates a
    /// value-returning density `compute(x,y,z)` with NO loop-carried RNG / cross-iteration state, then
    /// folds the result into an ordered reduction (the ChunkFill polygon). Since every point is
    /// independent (pure fn of coord + read-only wired router — unlike the RNG-recurrent ring loop),
    /// it partitions cleanly across the 11×-proven RavaPool: inspector-free scatter (coords are the
    /// index) → parallel compute → ordered reduce. Returns Ok(false) while the gate is OFF ⇒ the
    /// original serial bytecode runs, byte-identical. NOT wired into the backedge dispatch yet (10.2).
    #[cfg(feature = "next-gen-gc")]
    /// P3-S2 (RAVA_M3_S2, default OFF): compiled-body worker plan for the chunk-fill fork —
    /// the whole per-element body (new ctx(x,y,z) + root.compute(ctx)) runs in RUST + compiled
    /// code, ZERO interpreter (the measured wall: interp Arc-refcount churn saturates ~250 el/s
    /// regardless of worker count). GENERIC ctor gate: the ctx ctor must be the canonical
    /// 3-int-record shape `aload_0; invokespecial Object.<init>; (aload_0; iload_k; putfield)×3;
    /// return` — anything else returns None and the interp body runs (no-adapt: shape-matched,
    /// not name-matched).
    #[cfg(feature = "next-gen-gc")]
    pub(crate) fn s2_prepare(
        &mut self,
        frame: &Frame,
        bc_body: &[u8],
        m: &crate::jit::CoordMapLoop,
        first: [i32; 3], // the fork's first (x,y,z) — used to WARM the direct site on main
    ) -> Option<(u32, u32, [u32; 3], i64, [u8; 3])> {
        fn s2_enabled() -> bool {
            static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            *ON.get_or_init(|| std::env::var("RAVA_M3_S2").ok().as_deref() == Some("1"))
        }
        if !s2_enabled() {
            return None;
        }
        macro_rules! bail {
            ($why:expr) => {{
                if std::env::var_os("RAVA_FORK_TRACE").is_some() {
                    eprintln!("[S2-BAIL] {}", $why);
                }
                return None;
            }};
        }
        let pool = &frame.class.file.pool;
        let Some(ctx_name) = pool.class_name(m.ctx_cp).map(|s| s.to_string()) else {
            bail!("ctx_cp not a class");
        };
        let Ok(cls) = self.loader.get_or_load(&ctx_name) else { bail!("ctx load failed") };
        let Some(ctor) = cls.file.method("<init>", "(III)V") else { bail!("no (III)V ctor") };
        let Some(code_c) = ctor.code.as_ref() else { bail!("ctor no code") };
        let bc = &code_c.bytecode;
        // Canonical record ctor: 2a b7 ssss | (2a 1b|1c|1d b5 ffff)×3 | b1  = 4+15+1 = 20 bytes.
        if bc.len() != 20 || bc[0] != 0x2a || bc[1] != 0xb7 || bc[19] != 0xb1 {
            bail!(format!("ctor shape len={} b0={:02x} b1={:02x}", bc.len(), bc[0], bc[1]));
        }
        let super_cp = u16::from_be_bytes([bc[2], bc[3]]);
        let Some(sref) = cls.file.pool.member_ref(super_cp) else { bail!("super ref") };
        // java/lang/Object.<init> and java/lang/Record.<init> are both guaranteed-empty JDK
        // ctors (Record is the abstract record superclass) — any other super ctor could have
        // side effects → interp body.
        if !(matches!(sref.owner, "java/lang/Object" | "java/lang/Record") && sref.name == "<init>")
        {
            bail!(format!("super ctor {}#{}", sref.owner, sref.name));
        }
        let sref_name = sref.name.to_string();
        let _ = sref_name;
        let mut fids = [0u32; 3];
        for k in 0..3usize {
            let g = 4 + k * 5;
            let iload = [0x1b, 0x1c, 0x1d][k]; // iload_1/2/3 in arg order
            if bc[g] != 0x2a || bc[g + 1] != iload || bc[g + 2] != 0xb5 {
                bail!(format!("ctor group {k}: {:02x} {:02x} {:02x}", bc[g], bc[g + 1], bc[g + 2]));
            }
            let fcp = u16::from_be_bytes([bc[g + 3], bc[g + 4]]);
            let fname = match cls.file.pool.member_ref(fcp) {
                Some(f) => f.name.to_string(),
                None => bail!("ctor field ref"),
            };
            fids[k] = self.resolve_field_id(&ctx_name, &fname);
        }
        // Arena class id + layout (idempotent; the serial warmup already registered it).
        let layout = self.class_field_layout(&ctx_name);
        let name_arc: std::sync::Arc<str> = std::sync::Arc::from(ctx_name.as_str());
        let cid = crate::nextgen::register_class(&name_arc, &layout);
        let nf = layout.len() as u32;
        // Every ctor field must map to an arena slot.
        for f in fids {
            if crate::nextgen::slot_of(cid, f).is_none() {
                bail!(format!("field id {f} has no arena slot"));
            }
        }
        // Parse the BODY prefix to learn which loop local feeds each ctor parameter (the call
        // is `new ctx; dup; iload a; iload b; iload c; invokespecial <init>` — a,b,c are some
        // permutation of x_iv/y_iv/z_iv). Optional leading `aload recv` (the compute receiver
        // is pushed under the ctx). Any other shape → None (interp body).
        let dbc = bc_body;
        let mut pos = m.body_start_pc;
        // optional aload (0x19 idx | 0x2a..=0x2d)
        match *dbc.get(pos)? {
            0x19 => pos += 2,
            0x2a..=0x2d => pos += 1,
            _ => {}
        }
        if *dbc.get(pos)? != 0xbb {
            bail!(format!("body: expected new, got {:02x} @{pos}", dbc[pos]));
        }
        pos += 3;
        if *dbc.get(pos)? != 0x59 {
            bail!(format!("body: expected dup, got {:02x} @{pos}", dbc[pos]));
        }
        pos += 1;
        let mut srcs = [0u8; 3];
        for s in srcs.iter_mut() {
            let (idx, len) = match *dbc.get(pos)? {
                0x15 => (*dbc.get(pos + 1)? as usize, 2),
                op @ 0x1a..=0x1d => ((op - 0x1a) as usize, 1),
                other => bail!(format!("body: expected iload, got {other:02x} @{pos}")),
            };
            *s = if idx == m.x_iv {
                0
            } else if idx == m.y_iv {
                1
            } else if idx == m.z_iv {
                2
            } else {
                bail!(format!("body: ctor arg local {idx} not a loop coord"));
            };
            pos += len;
        }
        if *dbc.get(pos)? != 0xb7 {
            bail!(format!("body: expected invokespecial, got {:02x} @{pos}", dbc[pos]));
        }
        // The loop-invariant receiver, packed for the callback ABI. GC never runs in-region,
        // so an arena address stays valid for the whole fork.
        let recv_cell = crate::jit::pack_local(frame.locals().get(m.recv_local)?);
        // WARM the (driver, compute) direct-call site on MAIN (resolve+insert allowed outside a
        // region — the frozen read-only rule applies to workers only). Without this the site is
        // EMPTY (the driver is interpreted; no machcode path ever filled it) and every worker
        // element fell to the locked jit_call = full serialization (measured 90s). One real
        // compute of the fork's FIRST element — the same value the fork recomputes; any memo it
        // writes is coordinate-keyed identical state (hash arbiter confirms).
        let dummy = crate::nextgen::tlab_try_alloc(cid, nf)?;
        for k in 0..3usize {
            crate::opcodes::arena_putfield(dummy, fids[k], &Value::Int(first[srcs[k] as usize]));
        }
        let rawc = [recv_cell, dummy.addr() as i64];
        let cptr = std::sync::Arc::as_ptr(&frame.class);
        match self.jit_direct_call(cptr, m.compute_cp, 3, &rawc) {
            Some(Ok(_)) => {}
            _ => bail!("direct-site warm failed (compute not direct-servable)"),
        }
        Some((cid, nf, fids, recv_cell, srcs))
    }
}
