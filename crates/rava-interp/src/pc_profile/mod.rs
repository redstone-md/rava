//! PC-sampling decomposition of compiled code (`RAVA_PROFILE_PC`, default OFF).
//!
//! WHY THIS EXISTS. `rava_sampler`'s per-method table is ENTRY self-time: it reads the method
//! id published by the `jit_method_push/pop` pair that brackets `call_raw`, so a subtree that
//! is fully served by machine-code PIC / direct-mono hits never re-enters Rust and bills
//! entirely to the frame that entered it. On a call-dense workload that collapses the whole
//! profile onto one row (ChunkFill FILL is 99.9% `ChunkFill.main`; ShapeOr is 89% `ShapeOr.round`)
//! and the JIT_EXEC row stops being decomposable.
//!
//! The obvious fix — publish the current method from inside the PIC hit arm — would emit a
//! store into every compiled call site, which this codebase has measured to be the wrong
//! trade (a body-duplicating / body-growing change does not transfer to the boot). This
//! module takes the other route: **sample the guest thread's instruction pointer** from the
//! existing background sampler and map it, at dump time, to the compiled function whose code
//! range contains it. Nothing is emitted into any compiled body, the codegen is byte-identical
//! whether the flag is on or off, and the resulting histogram is a TRUE self-time
//! decomposition of machine-code execution — PIC-resolved callees included.
//!
//! HOW IT SAMPLES. The guest thread publishes a duplicated handle to itself the first time it
//! enters compiled code (`arm_current_thread`). Each sampler tick then does
//! `SuspendThread` → `GetThreadContext(CONTEXT_CONTROL)` → `ResumeThread` and keeps the raw
//! RIP. Resolution is deferred to the dump so the sampler NEVER takes a lock the suspended
//! thread might hold — the guest thread locks `RANGES` at compile publication, and taking it
//! while that thread is frozen would deadlock the process.
//!
//! The sampler/dump implementation is Windows-only. On Linux, `RAVA_PERF_MAP=1` can
//! instead publish compiled methods to `/tmp/perf-<pid>.map` for external `perf` tools.




#[cfg(target_os = "linux")]
mod perf_map;

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

/// One compiled function's machine-code extent, recorded at `jit_cache` publication.
struct CodeRange {
    start: usize,
    end: usize,
    mid: u32,
}

/// Append-only registry of compiled code extents. Written by the guest thread (once per
/// compiled method, off the hot path), read only by `dump`.
static RANGES: Mutex<Vec<CodeRange>> = Mutex::new(Vec::new());

/// Raw RIP samples as `(epoch_ms, rip)`, owned exclusively by the sampler thread. Resolved at
/// dump time.
///
/// WHY THE TIMESTAMP. A boot is a sequence of phases whose boundaries are only knowable AFTER
/// the run (they are guest log lines, and one of them — the first ring task — is not a log line
/// at all). A dump block is a fixed time slice, so a table printed per block can only ever be
/// re-bucketed into phases by choosing the dump period in advance and accepting its granularity.
/// Stamping each SAMPLE instead makes the dump period irrelevant to the analysis: any window
/// discovered later is an exact filter over the samples.
static SAMPLES: Mutex<Vec<(u64, usize)>> = Mutex::new(Vec::new());

/// Duplicated handle to the thread that runs compiled guest code (0 = not yet armed).
static TARGET: AtomicUsize = AtomicUsize::new(0);

/// ⭐**START THE SAMPLER IN A PRODUCT BUILD.** This profiler was unreachable outside a
/// `wall-profile` build: its sampler thread was spawned by `rava_sampler::enter`, which is a
/// no-op STUB whenever that feature is off, and `dump` was never called from the CLI at all. So
/// `RAVA_PROFILE_PC=1` on a shipped binary printed nothing — and the module's own comment already
/// records the interpreter-only case as *"the one run it is now most needed for"*.
///
/// Idempotent and inert unless the flag is set. One thread, one `SuspendThread` /
/// `GetThreadContext` / `ResumeThread` per tick; the guest thread is stopped for microseconds.
/// Symbolize an address for the crash handler: `(function+offset, file:line)`. Uses the same
/// PDB lookup the profiler already loads, which is why the crash reporter costs no new machinery.
#[cfg(windows)]
pub fn fault_site(addr: usize) -> (String, String) {
    (win::symbolize(addr), win::source_line(addr))
}
#[cfg(not(windows))]
pub fn fault_site(_addr: usize) -> (String, String) {
    (String::new(), String::new())
}

pub fn start_sampler_if_on() {
    // Arm the shared region guards for TIME attribution whenever the PC profile runs.
    if enabled() {
        crate::alloc_sites::arm_region_time();
    }
    if !enabled() {
        return;
    }
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, Relaxed) {
        return;
    }
    // ⛔⭐**SAY WHICH BINARY THIS IS, BECAUSE THE PRODUCT ONE MISATTRIBUTES.** `[profile.release]`
    // is `strip = "symbols"` and the MSVC linker folds identical functions (`/OPT:ICF`), so a
    // sample inside one function is reported against whichever byte-identical twin the PDB
    // resolves. Row 428 spent two optimisations on `native_served_namespace` at "11.6%" — on
    // `release-diag` with `/OPT:NOICF` the same run reads `run_hot` 11.1% and that function does
    // not appear at all. A profiler that can be read wrong should say so every time it starts.
    eprintln!(
        "[PC-PROFILE] sampling {} — symbol attribution is only trustworthy on a build with          `strip = \"none\"` AND `/OPT:NOICF` (see `release-diag`; row 428)",
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown exe>".into())
    );
    // 1 kHz: fine enough to rank host functions over a multi-second run, coarse enough that the
    // suspend/resume pair is a rounding error against what it measures.
    let period = std::env::var("RAVA_PROFILE_PC_US")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(1000);
    std::thread::Builder::new()
        .name("rava-pc-sampler".into())
        .spawn(move || loop {
            crate::pc_profile::sample::sample();
            std::thread::sleep(std::time::Duration::from_micros(period));
        })
        .ok();
    // ⚠**PERIODIC DUMP, BECAUSE A MINECRAFT SERVER NEVER EXITS.** The CLI dumps this table at a
    // clean exit, which on the only workload this project measures means never — the same defect
    // row 393 recorded for six other censuses. `RAVA_PROFILE_PC_MS` (default 30 s) prints a block
    // every interval, so each table covers the guest phase it overlaps instead of nothing at all.
    let every_ms = std::env::var("RAVA_PROFILE_PC_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(30_000);
    std::thread::Builder::new()
        .name("rava-pc-dump".into())
        .spawn(move || {
            let mut n = 0u32;
            let mut prev_ops = 0u64;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(every_ms));
                n += 1;
                // ⭐**ns/OPCODE, printed next to the profile it explains.** A flat host profile
                // cannot tell "we run the same work slower" from "we run more work"; the opcode
                // count can. `OPS_ALL` is exact to within CHECK_INTERVAL per live frame, and one
                // relaxed load per block costs nothing.
                let ops = crate::opcodes::ops_all();
                let dops = ops.saturating_sub(prev_ops);
                prev_ops = ops;
                eprintln!(
                    "[PC-OPS] block{n}: {dops} opcodes in {every_ms} ms = {:.2} ns/op ({ops} total)",
                    if dops == 0 { 0.0 } else { every_ms as f64 * 1e6 / dops as f64 }
                );
                // The native-dispatch shortcut's hit rate belongs next to the profile row it
                // explains (`dispatch_owner_cascade`): a low rate means the one-entry memo is
                // thrashing and a per-SITE memo is worth building; a high one means the 3.1% is
                // the tail, not the lookup. It was only ever printed by `lane_probe`, which a
                // normal boot never arms.
                {
                    use std::sync::atomic::Ordering::Relaxed;
                    let (h, m) = (
                        crate::natives::ARM_HIT.load(Relaxed),
                        crate::natives::ARM_MISS.load(Relaxed),
                    );
                    if h + m > 0 {
                        eprintln!(
                            "[PC-ARM] last-arm hit={h} miss={m} ({:.1}% hit)",
                            h as f64 * 100.0 / (h + m) as f64
                        );
                    }
                    // JIT_CALLBACK is the second-largest category of the product boot's block A
                    // (22.5%), and its POPULATION was only ever printed at process exit — which a
                    // boot benchmark never reaches, because the harness stops at `Done`. Print it
                    // with every block, so the category can be attributed to a kind on a live run
                    // instead of being a number with no name under it.
                    {
                        let mut rows: Vec<(u64, &str)> = crate::jit::CB_NAMES
                            .iter()
                            .enumerate()
                            .map(|(i, n)| (crate::gilfree_cf::jit_cb_kind(i), *n))
                            .filter(|(c, _)| *c > 0)
                            .collect();
                        let total: u64 = rows.iter().map(|(c, _)| *c).sum();
                        if total > 0 {
                            rows.sort_unstable_by(|a, b| b.0.cmp(&a.0));
                            let text: Vec<String> = rows
                                .iter()
                                .take(10)
                                .map(|(c, n)| {
                                    format!("{n}={c} ({:.1}%)", *c as f64 * 100.0 / total as f64)
                                })
                                .collect();
                            eprintln!("[PC-CB] total={total} | {}", text.join(" "));
                        }
                    }
                    // The dispatch-table census, for the same reason: `report()` printed only
                    // from the lane probe's reporter, which runs inside a deferral-lane region —
                    // so on a workload with no lane (every microbenchmark, including the sharpest
                    // probe this tree has) the `miss_shape` split was unreadable.
                    // THE HEAP-REF PIC'S OWN TALLY, for the same reason and with the same defect:
                    // `heappic::dump` had NO CALLER anywhere in the tree, so the one number that
                    // says whether the default-on leaf PIC is filling or refusing had never been
                    // printed on any workload. `[PC-DT]`'s `miss_shape` puts 22,594,566 of ~50 M
                    // dispatch misses on a HEAP-REF receiver — this line is what splits them into
                    // "cached and hitting" versus "never-cacheable", which is the difference
                    // between a capacity problem and an admission one.
                    crate::heappic::dump();
                    // WHY the lane sees so little of the reload fan-out. Four admits a boot
                    // against an EMPTY rejection census means the bodies never reached the
                    // admission test; this names the arm above it that turned them away.
                    {
                        let dw = crate::gilfree_cf::defer_why_report();
                        if !dw.is_empty() {
                            eprintln!("[PC-DEFER] {dw}");
                        }
                    }
                    // ⭐**AND WHICH `plan_meta` GATE REFUSED THE BODIES THAT DID REACH IT.**
                    // `plan_census::dump` was wired ONLY into `pc_profile::dump::dump`, which
                    // early-returns on `stamped.is_empty()` — and this module samples the
                    // instruction pointer through a Windows-only path, so on Linux there are
                    // never any samples and that call site is unreachable. The refusal census
                    // was therefore silent on every boot measured on this box, which is exactly
                    // the "EMPTY rejection census" the comment above calls out. It belongs
                    // beside `[PC-DEFER]`: same periodic block, same reason, proven reachable.
                    crate::gilfree_cf::plan::plan_census::dump();
                    // AND WHICH CLAUSE DID THE REFUSING, ranked. `[HEAPPIC-LIVE]` says 19.86 M
                    // fills were refused; this splits that by clause and names the top
                    // (class, method) pairs behind each, which is the difference between "extend
                    // the inline-emitted native population" and "the receiver representation is
                    // the blocker". Both reports already existed and printed only from the lane
                    // probe's reporter, which needs an open deferral region — so on the workload
                    // they were written for they were readable only inside a ~300 ms window.
                    {
                        let why = crate::icmiss::never_why_report();
                        if !why.is_empty() {
                            eprintln!("[PC-NEVER] {why}");
                            eprintln!("[PC-NEVER] {}", crate::icmiss::never_names_report(14));
                        }
                    }
                    {
                        let dt = crate::jit::dispatch_table::report();
                        if !dt.is_empty() {
                            eprintln!("[PC-DT] {dt}");
                            // The REFUSAL split, which is the actionable half: the table's own
                            // comment records "lambda machinery refused 6,629,289 of 6,629,289",
                            // and which clause did the refusing decides whether the fix is a
                            // site-keyed PIC or a capture-unpacking thunk.
                            let lr = crate::jit::dispatch_table::learn_refusal_line();
                            if !lr.is_empty() {
                                eprintln!("[PC-DTR] {lr}");
                            }
                            // FEASIBILITY OF A MACHINE-CODE CAPTURE COPY. `note_cap_cell`
                            // classifies each capture cell exactly as `decode_arena_cell`
                            // discriminates it, and one of the six kinds — an `arena_box` index
                            // needing `box_ref` — cannot be emitted. Its count is therefore the
                            // whole feasibility question for the capture-unpacking thunk, and it
                            // was only readable at process exit, i.e. never on a boot.
                            {
                                use std::sync::atomic::Ordering::Relaxed;
                                let cc: [u64; 6] =
                                    std::array::from_fn(|i| crate::picprobe::CAP_CELL[i].load(Relaxed));
                                if cc.iter().sum::<u64>() > 0 {
                                    eprintln!(
                                        "[PC-CAP] prim={} null={} ref={} array={} arenaptr={} BOXIDX={} (BOXIDX is the only un-emittable kind)",
                                        cc[0], cc[1], cc[2], cc[3], cc[4], cc[5]
                                    );
                                }
                                let seen = crate::jit::emit::cap_thunk::SEEN.load(Relaxed)
                                    + crate::jit::emit::cap_thunk::REFUSE
                                        .iter()
                                        .map(|a| a.load(Relaxed))
                                        .sum::<u64>();
                                if seen > 0 {
                                    eprintln!(
                                        "[PC-THUNK] qualified={seen} site_ok={} installed={} | refused voidwide={} arg0={} retref={} noshift={}",
                                        crate::jit::emit::cap_thunk::SITE_OK.load(Relaxed),
                                        crate::jit::emit::cap_thunk::INSTALLED.load(Relaxed),
                                        crate::jit::emit::cap_thunk::REFUSE[0].load(Relaxed),
                                        crate::jit::emit::cap_thunk::REFUSE[1].load(Relaxed),
                                        crate::jit::emit::cap_thunk::REFUSE[2].load(Relaxed),
                                        crate::jit::emit::cap_thunk::REFUSE[3].load(Relaxed),
                                    );
                                }
                            }
                        }
                    }
                }
                { let r = crate::jit::lambda_shape::report(); if !r.is_empty() { eprintln!("[PC-INDY] {r}"); } } // HR-INDY census (added line)
                {
                    use std::sync::atomic::Ordering::Relaxed;
                    let (q, r, u) = (
                        crate::vm::threads::OFFLOAD_QUEUED.load(Relaxed),
                        crate::vm::threads::OFFLOAD_REFUSED.load(Relaxed),
                        crate::vm::threads::OFFLOAD_UNPROVEN.load(Relaxed),
                    );
                    if q + r + u + crate::vm::threads::OFFLOAD_SEEN.load(Relaxed) > 0 {
                        let seen = crate::vm::threads::OFFLOAD_SEEN.load(Relaxed);
                        eprintln!("[PC-OFFLOAD] supplyAsync seen={seen} → queued={q} refused={r} executor-unproven={u}");
                    }
                }
                {
                    let c = crate::gilfree_cf::warmup::batch_census();
                    if !c.starts_with("drains=0 ") {
                        eprintln!("[PC-LANE] {c}");
                    }
                }
                #[cfg(feature = "wall-profile")]
                crate::offload_probe::dump();
                // WHICH CLASSES the compiled-code fast path turns away. `fu_fastpath`'s own
                // comment says "7.9 M refusals a boot ... the list, not the representation, is
                // the limit" — this is that list, and it only printed from the lane probe, which
                // a boot measured to `Done` and then killed never reaches.
                // WHY a call site is permanently refused a direct entry. `dmiss_res` is 77% of
                // all crossings in a probe run, and the verdict is PERMANENT — an
                // over-approximating clause is paid by every later call of every site it touches.
                // Same reason as the FU census below: it only printed from the lane probe.
                if crate::icmiss::on() {
                    crate::icmiss::dump();
                }
                // ⚠**THE FIFTH CENSUS THAT ONLY PRINTED FROM `lane_probe`.** A boot measured to
                // `Done` and then killed never reaches that path, so every one of these tables
                // was unreadable for exactly the run that matters. icmiss, backedge, dresolve,
                // vecgrow and now picprobe are all wired here instead.
                if crate::picprobe::enabled() {
                    crate::picprobe::dump();
                }
                // Per-SITE receiver profile (`RAVA_SITEMONO_TABLE=1`, default OFF): the
                // aggregate `profcov` gauges cannot say whether one NAMED site is monomorphic.
                crate::sitemono::dump_table();
                // WHICH numbered clause refuses a direct entry. Only printed from the lane
                // probe, like the three censuses above.
                {
                    let r = crate::dresolve::report();
                    if !r.is_empty() {
                        eprintln!("{r}");
                    }
                }
                // DOES `locals.insert(0, receiver)` REALLOCATE? It runs on every virtual call,
                // and `pop_args_n` hands back a buffer sized for the ARGS — so the receiver
                // insert both shifts and may grow. Same reason as the two censuses above: this
                // only printed from a path a boot never reaches.
                crate::vecgrow::report();
                // DOES the super-chain walk re-run the cascade on declining links?
                crate::natives::entry::chain_census::report();
                if crate::backedge::enabled() {
                    let fu = crate::backedge::report_prefix("FU:", 20);
                    if !fu.is_empty() {
                        eprintln!("[FU-REFUSED] {fu}");
                    }
                    // The arena half of the same question — which (class, method) pairs the
                    // compiled-code fast path turns away when the receiver lives in the arena.
                    let fuo = crate::backedge::report_prefix("FUO:", 20);
                    if !fuo.is_empty() {
                        eprintln!("[FU-REFUSED-ARENA] {fuo}");
                    }
                }
                {
                    use std::sync::atomic::Ordering::Relaxed;
                    eprintln!(
                        "[PC-MALLOC] allocs={} frees={}{}",
                        crate::mem_meter::ALLOC_N.load(Relaxed),
                        crate::mem_meter::FREE_N.load(Relaxed),
                        if crate::alloc_sites::on() { crate::alloc_sites::report(30) } else { String::new() },
                    );
                }
                {
                    use std::sync::atomic::Ordering::Relaxed;
                    let h = crate::heap::objects_alloc::HANDLE_BIRTHS.load(Relaxed);
                    if h > 0 {
                        eprintln!(
                            "[PC-HANDLE] objects born as HEAP HANDLES: {h}{}",
                            if crate::heap::objects_alloc::handle_census() {
                                crate::heap::objects_alloc::handle_census_report(15)
                            } else {
                                String::new()
                            }
                        );
                    }
                }
                // ⚠`bootdisp` prints only at a clean exit and a Minecraft server never exits —
                // the defect row 393 recorded for six censuses, met again. It answers the one
                // question the product profile raises (only 8% of samples are inside compiled
                // code: is the JIT compiling too little, or is the compiled code just cheap?),
                // so it is printed where it can be read.
                {
                    let c = crate::frames::pool_census_report();
                    if !c.is_empty() {
                        eprintln!("{c}");
                    }
                }
                crate::bootdisp::dump();
                {
                    // WHY a compiled site never caches its callee — the clause tally plus the
                    // class names behind it. Same reason as everything else here: it printed
                    // only at exit.
                    let r = crate::icmiss::never_why_report();
                    if !r.is_empty() {
                        eprintln!("{r}");
                        eprintln!("{}", crate::icmiss::never_names_report(12));
                    }
                }
                // The dispatch census, on the periodic dump rather than at a graceful exit that
                // the boot harness never reaches: the ratio that matters is compiled entries
                // against the host excursions they make, because a crossing costs more than
                // either side of it (`docs/jit-tiering-is-a-narrow-optimum.md`).
                if std::env::var_os("RAVA_PROFILE_BOOT_DISPATCH").is_some() {
                    crate::bootdisp::dump_totals();
                    // The methods that are hot in the INTERPRETER and never compile. Counting,
                    // so it is unaffected by the box drift that currently puts the timing floor
                    // at ~8%, and directly actionable: each one is a method the tiering never
                    // reached.
                    crate::bootdisp::dump_stuck(12);
                    // WHO is on the far side of a crossing? The three verdicts choose the lever:
                    // `NEVER` means the callee is native-served (a typed native ABI is the fix),
                    // `NOTYET` means it simply is not compiled yet (compile in dependency order),
                    // `COMPILED` means the caches are failing on a callee that WAS compiled.
                    use std::sync::atomic::Ordering::Relaxed as R;
                    eprintln!(
                        "[RESOLVE-VERDICTS] compiled={} never-cacheable={} not-yet={}",
                        crate::icmiss::R_COMPILED.load(R),
                        crate::icmiss::R_NEVER.load(R),
                        crate::icmiss::R_NOTYET.load(R),
                    );
                }
                // How much of the program is even ELIGIBLE to run compiled? The profile says only
                // 9.1% of samples land inside compiled code; this says whether that is because
                // few methods compile (a cap or threshold) or because compiled methods leave.
                eprintln!(
                    "[JIT-COVERAGE] compiled={} cap={}",
                    crate::vm::jit_run::COMPILED_N
                        .load(std::sync::atomic::Ordering::Relaxed),
                    crate::vm::jit_run::CAP_N.load(std::sync::atomic::Ordering::Relaxed),
                );
                // ⭐**THE POPULATION NOBODY HAD COUNTED ON THIS BINARY.** Every call-level census
                // came back in the tens of millions against a forty-second boot; interpreted
                // OPCODES are the one population that can be in the billions, and the figure on
                // file (10 M, i.e. negligible) was taken on a different configuration.
                if std::env::var_os("RAVA_OBJ_COUNT").is_some() {
                    eprintln!(
                        "[INTERP-OPS] {} (all threads)",
                        crate::opcodes::checkpoint::ops_all()
                    );
                }
                #[cfg(feature = "next-gen-gc")]
                if std::env::var_os("RAVA_OBJ_COUNT").is_some() {
                    eprintln!(
                        "[ARENA-ALLOCS] {}",
                        crate::nextgen::ARENA_ALLOCS
                            .load(std::sync::atomic::Ordering::Relaxed)
                    );
                }
                {
                    let fw = crate::vmstate::Vm::force_why_report();
                    if !fw.is_empty() {
                        eprintln!("[FORCE-WHY] {fw}");
                    }
                }
                {
                    use std::sync::atomic::Ordering::Relaxed as R;
                    let g: Vec<usize> = crate::counters::JIT_GATE.iter().map(|c| c.load(R)).collect();
                    eprintln!(
                        "[JIT-GATE] worker-visits={} worker-visits-ALREADY-HOT={} (a worker never                          compiles: its JITModule dies with the thread)",
                        g.get(5).copied().unwrap_or(0),
                        g.get(6).copied().unwrap_or(0),
                    );
                }
                if std::env::var_os("RAVA_LOOKUP_COUNT").is_some() {
                    use std::sync::atomic::Ordering::Relaxed as R;
                    eprintln!(
                        "[LOOKUP-COUNT] super_chain={} get_or_load={}",
                        crate::class_loader::SUPER_CHAIN_CALLS.load(R),
                        crate::class_loader::GET_OR_LOAD_CALLS.load(R),
                    );
                }
                if std::env::var_os("RAVA_NSN_COUNT").is_some() {
                    eprintln!(
                        "[NSN-COUNT] native_served_prefix calls on this thread: {}",
                        crate::opcodes::nsn_calls()
                    );
                }
                if std::env::var_os("RAVA_RES_STAT").is_some() {
                    use std::sync::atomic::Ordering::Relaxed as R;
                    eprintln!(
                        "[RES-STAT] read_resource lookups={} distinct={}",
                        crate::class_loader::RES_LOOKUPS.load(R),
                        crate::class_loader::RES_DISTINCT.load(R),
                    );
                }
                if std::env::var_os("RAVA_NIO_STAT").is_some() {
                    use std::sync::atomic::Ordering::Relaxed as R;
                    let n = crate::nio::files::NIO_CALLS.load(R).max(1);
                    eprintln!(
                        "[NIO-STAT] newInputStream calls={} misses={} path={:.0}ns/call read={:.0}ns/call",
                        crate::nio::files::NIO_CALLS.load(R),
                        crate::nio::files::NIO_MISSES.load(R),
                        crate::nio::files::NIO_PATH_NS.load(R) as f64 / n as f64,
                        crate::nio::files::NIO_READ_NS.load(R) as f64 / n as f64,
                    );
                }
                if std::env::var_os("RAVA_NATIVE_HIST").is_some() {
                    eprintln!("{}", crate::natives::entry::hist_report());
                    eprintln!("{}", crate::natives::entry::slow_report(20));
                }
                if std::env::var_os("RAVA_NATIVE_COUNT").is_some() {
                    eprintln!(
                        "[NATIVE-CALLS] {}",
                        crate::natives::entry::NATIVE_CALLS
                            .load(std::sync::atomic::Ordering::Relaxed)
                    );
                }
                if std::env::var_os("RAVA_PUTSTATIC_COUNT").is_some() {
                    eprintln!(
                        "[PUTSTATIC] {} static writes so far",
                        crate::opcodes::statics::PUTSTATIC_N
                            .load(std::sync::atomic::Ordering::Relaxed)
                    );
                }
                if std::env::var_os("RAVA_TASK_TIME").is_some() {
                    use std::sync::atomic::Ordering::Relaxed;
                    eprintln!(
                        "[TASK-TIME] {} guest executor tasks, {:.2}s inside them",
                        crate::task_runtime::TASK_COUNT.load(Relaxed),
                        crate::task_runtime::TASK_NS.load(Relaxed) as f64 / 1e9
                    );
                }
                if crate::jit_cb_census::enabled() {
                    eprintln!("{}", crate::jit_cb_census::report(40));
                }
                crate::pc_profile::dump::dump(&format!("block{n}"));
            }
        })
        .ok();
}

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    // The caller-attribution mode (`pc_stack`) is a strictly richer capture on the same tick,
    // so it arms this one rather than making a caller remember to set both.
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_PROFILE_PC").is_some() || crate::pc_stack::enabled()
    })
}

/// `RAVA_PC_TRACE`: emit ONE line per sample (`[PC-TRACE] <epoch_ms> ...`) instead of the
/// per-block tables, so the ranking is done afterwards over any window the run turns out to
/// have. The tables answer "what is hot in this 10 s slice"; the trace answers "what is hot
/// between two guest log lines", which is the only question a phase map can use.
pub(crate) fn trace() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| enabled() && std::env::var_os("RAVA_PC_TRACE").is_some())
}

/// Wall clock as Unix epoch milliseconds — the one stamp that can be lined up against the
/// guest's own `[HH:MM:SS]` log lines without knowing when the sampler armed.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Record a compiled function's code extent. Called once per method at publication, where
/// both the finalized pointer and the interned method id are in hand.
pub fn register(start: usize, len: usize, mid: u32) {
    if start == 0 || len == 0 {
        return;
    }
    let profile_enabled = enabled();
    #[cfg(target_os = "linux")]
    let perf_map_enabled = perf_map::enabled();
    #[cfg(not(target_os = "linux"))]
    let perf_map_enabled = false;
    if !profile_enabled && !perf_map_enabled {
        return;
    }
    #[cfg(target_os = "linux")]
    if perf_map_enabled {
        perf_map::register(start, len, mid);
    }
    if profile_enabled {
        if let Ok(mut v) = RANGES.lock() {
            v.push(CodeRange { start, end: start + len, mid });
        }
    }
}

pub(crate) fn perf_map_enabled() -> bool {
    #[cfg(target_os = "linux")]
    {
        perf_map::enabled()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// MULTI-THREAD CAPTURE (`RAVA_PROFILE_PC_ALL`, default OFF).
///
/// `TARGET` follows ONE thread — first to enter compiled code. `[PC-WHEN]` showed what that
/// costs: the armed thread is `ZwWaitForAlertByThreadId` 100% for the last ~19 s of a boot, so
/// the single-target profiler **has never sampled the ring window at all** (a third of the
/// wall), and every host percentage it reports is diluted by that parked tail.
///
/// This capture is ADDITIVE and separate: `TARGET`/`SAMPLES` keep their exact meaning, so
/// `pc_stack` and the existing tables are untouched and remain comparable with every earlier
/// measurement. Only the new `[PC-ALL]` section reads these.
const MAX_TARGETS: usize = 16;
static TARGETS: [AtomicUsize; MAX_TARGETS] = [const { AtomicUsize::new(0) }; MAX_TARGETS];
/// Guest OS thread ids already registered, so a thread re-entering compiled code does not
/// occupy a second slot (and so a slot index is stable enough to name a thread in the dump).
static TARGET_TIDS: [AtomicU32; MAX_TARGETS] = [const { AtomicU32::new(0) }; MAX_TARGETS];
/// `(epoch_ms, rip, slot)` across every armed thread. Owned by the sampler thread.
static ALL_SAMPLES: Mutex<Vec<(u64, usize, u8)>> = Mutex::new(Vec::new());

pub fn all_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PROFILE_PC_ALL").is_some())
}

/// Refresh the target set by ENUMERATING the process's threads, rather than waiting for each to
/// announce itself.
///
/// The self-arming design (`arm_current_thread` from the `JIT_EXEC` category guard) covered
/// exactly ONE thread in practice: a first attempt at multi-target capture came back
/// "2 armed threads", both the same one. Ring workers reach compiled code by a route that never
/// raises that guard, so no amount of slot bookkeeping was going to find them. Enumeration has
/// no such dependency — a thread is sampled because it EXISTS, which is the property actually
/// wanted.
///
/// Called from the sampler thread only, every `REFRESH_TICKS` ticks so threads created after the
/// first tick (the ring pool is built late) are picked up without paying a snapshot per sample.
#[cfg(windows)]
fn refresh_targets() {
    let tids = win::process_thread_ids();
    let me = win::current_thread_id(); // the sampler itself is not a subject
    'next: for tid in tids {
        if tid == me {
            continue;
        }
        for i in 0..MAX_TARGETS {
            if TARGET_TIDS[i].load(Relaxed) == tid {
                continue 'next;
            }
        }
        for i in 0..MAX_TARGETS {
            if TARGET_TIDS[i]
                .compare_exchange(0, tid, Relaxed, Relaxed)
                .is_ok()
            {
                // Publish the handle AFTER the tid claim: the sample loop reads `TARGETS`
                // alone, so a zero handle simply means "slot not ready yet".
                TARGETS[i].store(win::open_thread(tid), Relaxed);
                continue 'next;
            }
        }
        return; // full — an under-covered profile is readable; a reshuffled one is not
    }
}

/// Name the function a sampled address is IN: the guest method when it lands in compiled
/// code (prefixed `<jit> `, so a trace line says on its face whether the tick was inside
/// machine code), the Rust/system function otherwise.
pub(crate) fn leaf_name(pc: usize, ranges: &[(usize, usize, u32)]) -> String {
    let i = ranges.partition_point(|r| r.0 <= pc);
    match i.checked_sub(1).map(|j| ranges[j]) {
        Some((_, end, mid)) if pc < end => format!(
            "<jit> {}",
            crate::jit::compiled_method_name(mid).unwrap_or_else(|| format!("mid {mid}"))
        ),
        _ => {
            let s = symbolize(pc);
            s.rsplit_once('+').map_or_else(|| s.clone(), |(f, _)| f.to_string())
        }
    }
}

/// Resolve a host address to `symbol+0xNN`. Dump-time only; shared with `pc_stack`, which
/// needs the same PDB lookup for the frames it recovers.
///
/// MEMOISED PROCESS-WIDE. A dbghelp lookup is tens of microseconds and the same few thousand
/// host addresses recur in every dump; without the memo the dump cost scales with the number
/// of dumps, which is exactly what a short dump period (the thing that makes a phase map
/// possible) maximises. Code pages are never unmapped here, so a resolution is permanent.
pub(crate) fn symbolize(pc: usize) -> String {
    static MEMO: Mutex<Option<std::collections::HashMap<usize, String>>> = Mutex::new(None);
    if let Ok(g) = MEMO.lock() {
        if let Some(m) = g.as_ref() {
            if let Some(s) = m.get(&pc) {
                return s.clone();
            }
        }
    }
    let s = symbolize_uncached(pc);
    if let Ok(mut g) = MEMO.lock() {
        g.get_or_insert_with(std::collections::HashMap::new).insert(pc, s.clone());
    }
    s
}

fn symbolize_uncached(pc: usize) -> String {
    #[cfg(windows)]
    {
        win::symbolize(pc)
    }
    #[cfg(not(windows))]
    {
        format!("<host {pc:#x}>")
    }
}

/// Entry address of the function containing `pc` (0 when unknown). This is what lets a
/// recovered return address be VERIFIED rather than guessed: the target of the `call` it
/// returns from must be the entry of the function the inner frame is executing.
pub(crate) fn symbol_base(pc: usize) -> usize {
    #[cfg(windows)]
    {
        // Memoised for the same reason as `symbolize`, and more urgently: the verified-frame
        // walk calls this once per candidate return address per sample.
        static MEMO: Mutex<Option<std::collections::HashMap<usize, usize>>> = Mutex::new(None);
        if let Ok(g) = MEMO.lock() {
            if let Some(m) = g.as_ref() {
                if let Some(&b) = m.get(&pc) {
                    return b;
                }
            }
        }
        let b = win::symbol_base(pc);
        if let Ok(mut g) = MEMO.lock() {
            g.get_or_insert_with(std::collections::HashMap::new).insert(pc, b);
        }
        b
    }
    #[cfg(not(windows))]
    {
        let _ = pc;
        0
    }
}

/// True when `addr` points into committed, executable memory — the test that separates a
/// return address on the stack from an ordinary data word.
pub(crate) fn is_executable(addr: usize) -> bool {
    #[cfg(windows)]
    {
        win::is_executable(addr)
    }
    #[cfg(not(windows))]
    {
        let _ = addr;
        false
    }
}
// The symbol-resolution backend is `dbghelp` + `kernel32`: a Windows module in fact as well as in
// name. Every function above that consumes it already carries a `#[cfg(not(windows))]` arm, so the
// module itself is all that needed gating — without it a Linux link fails on `-ldbghelp`.
#[cfg(windows)]
mod win;
#[cfg(windows)]
pub(crate) use win::*;
pub mod dump;
pub(crate) use dump::*;
pub mod sample;
pub(crate) use sample::*;
