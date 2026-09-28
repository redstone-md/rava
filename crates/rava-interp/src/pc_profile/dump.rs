//! Reporting a profile: per thread, host timeline, and the flat table.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::pc_profile::*;

/// Resolve every sample against the code registry and print the ranked decomposition.
/// `header` tags the block so it can be aligned against the guest's own phase timings.
pub fn dump(header: &str) {
    if !enabled() {
        return;
    }
    // DRAINED, so each dump block covers exactly the interval since the previous one. A boot
    // is a sequence of very different phases and a cumulative table is dominated by whichever
    // one ran longest; the block view is what lets a caller line a dump up against the guest's
    // own phase markers and read that phase's composition on its own.
    let stamped: Vec<(u64, usize)> = match SAMPLES.lock() {
        Ok(mut v) => std::mem::take(&mut v),
        Err(_) => return,
    };
    if stamped.is_empty() {
        // The primary target may never have armed (interpreter-only run) while the enumerating
        // capture has plenty — dumping nothing here would hide the whole process.
        if all_enabled() {
            if let Ok(mut ranges) = RANGES.lock() {
                let r: Vec<(usize, usize, u32)> =
                    ranges.iter().map(|x| (x.start, x.end, x.mid)).collect();
                drop(ranges);
                dump_all_threads(&r);
            }
        }
        return;
    }
    let mut ranges: Vec<(usize, usize, u32)> = match RANGES.lock() {
        Ok(v) => v.iter().map(|r| (r.start, r.end, r.mid)).collect(),
        Err(_) => return,
    };
    ranges.sort_unstable_by_key(|r| r.0);

    // In trace mode the stack module owns the per-sample line (it has the frames as well as
    // the leaf); this module emits it only when stacks are off, so a sample is never printed
    // twice and a trace line always carries everything captured on that tick. Either way the
    // per-block TABLES are skipped: ranking a fixed time slice is the thing trace mode exists
    // to replace, and printing both would multiply an already large log by five.
    if trace() {
        if crate::pc_stack::enabled() {
            crate::pc_stack::dump(header, &ranges);
        } else {
            // Diagnostic selection only: never consulted by execution or compilation.
            // Resolve each selected address once per dump; retain the existing leaf field
            // so timestamp/leaf consumers can still filter the exact launch-to-Done window.
            let detail = std::env::var("RAVA_PC_TRACE_DETAIL").ok().filter(|s| !s.is_empty());
            let mut sites = std::collections::HashMap::new();
            for &(t, pc) in &stamped {
                let leaf = leaf_name(pc, &ranges);
                if detail.as_ref().is_some_and(|filter| leaf.contains(filter)) {
                    let (symbol, line) = sites.entry(pc).or_insert_with(|| fault_site(pc));
                    eprintln!("[PC-TRACE] {t} {leaf} | rip={pc:#x} symbol={symbol} source={line}");
                } else {
                    eprintln!("[PC-TRACE] {t} {leaf} |");
                }
            }
        }
        return;
    }

    let samples: Vec<usize> = stamped.iter().map(|&(_, pc)| pc).collect();
    let mut per_mid: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    let mut outside: u64 = 0;
    for &pc in &samples {
        // Largest start <= pc, then a containment test — ranges are disjoint by construction
        // (Cranelift hands out non-overlapping extents in the shared JIT module).
        let i = ranges.partition_point(|r| r.0 <= pc);
        match i.checked_sub(1).map(|j| ranges[j]) {
            Some((_, end, mid)) if pc < end => *per_mid.entry(mid).or_insert(0) += 1,
            _ => outside += 1,
        }
    }
    let total = samples.len() as u64;
    let inside = total - outside;
    eprintln!(
        "[PC-PROFILE {header} BLOCK] {total} RIP samples since the previous dump: \
         {inside} ({:.1}%) inside compiled code, \
         {outside} ({:.1}%) in the Rust host / JDK natives / kernel",
        inside as f64 * 100.0 / total as f64,
        outside as f64 * 100.0 / total as f64,
    );
    // The region breakdown, printed beside the inside/outside split it refines: "in the Rust
    // host" is 88% and says nothing about WHICH host, which is the gap this closes.
    {
        let r = crate::alloc_sites::region_time_report();
        if !r.is_empty() {
            eprint!("{r}");
        }
        // The dispatch-table census, at EXIT rather than only from the periodic sampler dump —
        // a workload shorter than the dump interval otherwise never prints it, which is the same
        // defect the sampler's own comments record for two other censuses.
        {
            let dt = crate::jit::dispatch_table::report();
            if !dt.is_empty() {
                eprintln!("[PC-DT] {dt}");
            }
            let lr = crate::jit::dispatch_table::learn_refusal_line();
            if !lr.is_empty() {
                eprintln!("[PC-DTR] {lr}");
            }
        }
        // ⭐**WHAT FRACTION OF A COMPILED CALL STAYS IN MACHINE CODE?** `picprobe::dump` answers
        // that, and it was wired ONLY to `run.rs`'s exit path — which a server boot never reaches,
        // because the harness kills the process at `Done (`. That is the FOURTH census in this tree
        // found reporting only at teardown, so it is hooked here beside the others.
        crate::picprobe::dump();
        {
            // WHICH REFUSAL keeps two thirds of all method entries interpreted.
            { let hr = crate::vm::hr_compare_report(); if !hr.is_empty() { eprintln!("{hr}"); } } // [HR-CMP]: what Vm::compare derived once per comparator and ordered without allocating
            // `[BOXID]` — narrow-box mints/unboxes, empty unless `RAVA_JIT_PIC_PROBE` armed
            // the counters. Printed here for the reason `compile_bail::report` is: a server
            // boot exits through `System.exit`, so a teardown dump never runs.
            let bx = crate::boxid::report();
            if !bx.is_empty() {
                eprintln!("{bx}");
            }
            // ADDED [INLCEN]: which rule refuses an inline, counted at compile time.
            { let ic = crate::vm::inline_census::report(); if !ic.is_empty() { eprintln!("{ic}"); } }
            // `[CB]` — the machine-code → host CROSSING counters. They lived only behind the
            // interpreted-op heartbeat, which a compiled workload never reaches (a warm reload
            // executes too few interpreted bytecodes to fire even one period), so the one
            // population that is measured EXACTLY rather than sampled was invisible on exactly
            // the workloads it describes. Counts do not care that the box is noisy.
            { let cb = crate::jit::callback_stats(); if !cb.is_empty() { eprintln!("[CB] {cb}"); } }
            // `[INVCEN]` — WHO the crossings go to, by name. Empty unless RAVA_INVOKE_CENSUS.
            { let iv = crate::vm::invoke_census::report(); if !iv.is_empty() { eprint!("{iv}"); } }
            let cb = crate::jit::compile_bail::report();
            if !cb.is_empty() {
                eprintln!("{cb}");
            }
            // `[SMAP]` — Phase-A stack-map cross-check, empty unless `RAVA_STACKMAPS=1`.
            #[cfg(feature = "next-gen-gc")]
            {
                let sm = crate::jit::runtime::stackmaps::report();
                if !sm.is_empty() {
                    eprintln!("{sm}");
                }
            }
            { let ec = crate::map::entry_census::report(); if !ec.is_empty() { eprintln!("{ec}"); } } // [ENTRY]: Map$Entry births, the entries served without one, and the shape memo's hit rate
            { let ol = crate::optlite::report(); if !ol.is_empty() { eprintln!("{ol}"); } } // ADDED [OPTLITE]: Optional births by state, the empties served from the shared instance, and the per-name shape of the cascade's arrivals
            { let fs = crate::jit::emit::ops::fields::site_census::report(); if !fs.is_empty() { eprintln!("{fs}"); } } // [FIELD-SITES]: getfield/putfield emit arms by slot resolved/unresolved
            { let tk = crate::gson_native::intern_report(); if !tk.is_empty() { eprintln!("{tk}"); } } // [TOKENS]: JSON key/value tokens materialised, the repeats served from the arena intern table, and the Arc<String> births avoided
            { let hs = crate::vm::jit_direct_call::hr_synth::report(); if !hs.is_empty() { eprintln!("{hs}"); } } // [HR-SYNTH]: native-served callees dispatched from the resolve, no synthetic frame
            { let hu = crate::vm::hr_unwind_report(); if !hu.is_empty() { eprintln!("{hu}"); } } // [HR-UNWIND]: compiled entries whose cell zero became inline stores; names what the `_NLG_Return2` row really is
            { let hw = crate::background_jit::hr_wait::report(); if !hw.is_empty() { eprintln!("{hw}"); } } // [HR-WAIT]: main-thread parks on the background compile queue, by caller kind, and the waits deferred instead
            crate::jit::emit::cap_thunk::report_shapes(); // [PC-CAPSHAPE]: capturing-lambda thunk installs/refusals by (void, wide) shape
            eprint!("{}", crate::jit::birth_census_report()); // [PC-BIRTH]: which birth route feeds mi_malloc under compiled bodies
            eprint!("{}", crate::heap::Heap::array_pool_report()); // [PC-ARRAYS]: array births/retirements by element kind and length, and the retired buffers the recycle pool served back
            { let vc = crate::Vm::valuecomputer_report(); if !vc.is_empty() { eprintln!("{vc}"); } } // [VALUECOMP]: pure value-computer subclasses admitted to the arena, by verdict, and the arena receivers coll_op served
            { let cc = crate::jit::emit::iterator_inline::cubecall_report(); if !cc.is_empty() { eprintln!("{cc}"); } } // [CUBECALL]: arena value-computer accessors served inline from the receiver's own bytecode vs declined to the compiled accessor
            // Same place, same reason (a boot never reaches a teardown): WHAT the two baked range
            // tests in front of every compiled call discriminate, and how often each arm wins.
            let cs = crate::jit::callseq::report();
            if !cs.is_empty() {
                eprintln!("{cs}");
            }
        }
        {
            // Same place, same reason: which cell SHAPES `String.startsWith` from compiled code
            // reaches the host with, and how many the HR-STRINGS arm served in place.
            let hs = crate::jit::callbacks::hr_strings_report();
            if !hs.is_empty() {
                eprintln!("{hs}");
            }
        }
        {
            // ⭐**HOW MANY BYTES OF STACK FRAME DOES A COMPILED BODY RESERVE?** `initCache` — one
            // method, 21.1% of all machine-code time — declares 101 explicit slots totalling
            // 18,976 bytes, each referenced exactly once, because Cranelift does not color
            // explicit slots and a body's frame is the SUM of its call sites' buffers. A CLIF
            // dump says that about ONE method; this says it about the whole compiled population,
            // which is the form in which "the frames got smaller" is a measurement. Same place
            // as the censuses above, for the same reason: a boot never reaches a teardown.
            let fr = crate::jit::emit::frame_bytes::report();
            if !fr.is_empty() {
                eprintln!("{fr}");
            }
        }
        // Same reason, same place: `plan_meta`'s refusal census.
        crate::gilfree_cf::plan::plan_census::dump();
        // ⭐**IS THE 63% THE MACHINE CODE, OR THE RUNTIME IT CALLS?** Same samples, split by the
        // compiled-range test the header line already uses. A high "machine code" share means the
        // codegen is the target; a high "host callbacks" share means the runtime is — and the two
        // are different projects, so the split has to come before either is chosen.
        if let Ok(v) = crate::alloc_sites::COMPILED_RIPS.lock() {
            if !v.is_empty() {
                let mut inside = 0u64;
                let mut per_host: std::collections::HashMap<String, u64> =
                    std::collections::HashMap::new();
                for &pc in v.iter() {
                    let i = ranges.partition_point(|r| r.0 <= pc);
                    let hit = i
                        .checked_sub(1)
                        .and_then(|k| ranges.get(k))
                        .is_some_and(|r| pc < r.1);
                    if hit {
                        inside += 1;
                    } else {
                        let sym = super::symbolize(pc);
                        let name = sym.split('+').next().unwrap_or(&sym).to_string();
                        *per_host.entry(name).or_insert(0) += 1;
                    }
                }
                let tot = v.len() as f64;
                eprintln!(
                    "[COMPILED-SPLIT] {} samples under a compiled body: {inside} ({:.1}%) in EMITTED CODE, {} ({:.1}%) in host callbacks",
                    v.len(),
                    inside as f64 * 100.0 / tot,
                    v.len() as u64 - inside,
                    (v.len() as u64 - inside) as f64 * 100.0 / tot
                );
                let mut rows: Vec<(String, u64)> = per_host.into_iter().collect();
                rows.sort_by(|a, b| b.1.cmp(&a.1));
                // ⭐**PRINT THE TAIL, NOT JUST THE HEAD.** The first capture stopped at 12 rows,
                // which named 22% of the callback time and left 60% of it anonymous — and an
                // anonymous 60% is indistinguishable from "flat" without being it. The cumulative
                // column is the whole point: if 40 rows reach 80%, this is a WORK LIST; if they
                // reach 30%, the tail really is thousands of tiny callees and codegen coverage
                // has to be raised wholesale rather than callee by callee.
                let ncb = (v.len() as u64 - inside).max(1) as f64;
                let mut cum = 0u64;
                for (name, n) in rows.iter().take(60) {
                    cum += *n;
                    eprintln!(
                        "[COMPILED-SPLIT] {:>6.2}%  cum {:>6.2}%  {n:>7}  {name}",
                        *n as f64 * 100.0 / tot,
                        cum as f64 * 100.0 / ncb,
                        );
                }
                eprintln!(
                    "[COMPILED-SPLIT] tail: {} distinct host callees, top-60 = {:.1}% of callback samples",
                    rows.len(),
                    cum as f64 * 100.0 / ncb
                );
            }
        }
        // WHICH PART of the crossing. The region tick prices the seam; these are the same
        // samples, symbolised, so the answer comes from one capture rather than a second mode.
        if let Ok(v) = crate::alloc_sites::SEAM_RIPS.lock() {
            if !v.is_empty() {
                let mut per: std::collections::HashMap<String, u64> =
                    std::collections::HashMap::new();
                for &rip in v.iter() {
                    // Aggregate per FUNCTION, not per offset: the raw symbol carries `+0x…` and a
                    // hot loop then spreads itself across a dozen rows that each look small.
                    let sym = super::symbolize(rip);
                    let name = sym.split('+').next().unwrap_or(&sym).to_string();
                    *per.entry(name).or_insert(0) += 1;
                }
                let mut rows: Vec<(String, u64)> = per.into_iter().collect();
                rows.sort_by(|a, b| b.1.cmp(&a.1));
                let tot = v.len() as f64;
                eprintln!("[SEAM-WHERE] {} samples inside the crossing's own marshalling", v.len());
                for (name, n) in rows.into_iter().take(15) {
                    eprintln!(
                        "[SEAM-WHERE] {:>6.1}%  {n:>7}  {name}",
                        n as f64 * 100.0 / tot
                    );
                }
            }
        }
    }
    let mut rows: Vec<(u32, u64)> = per_mid.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("[PC-PROFILE]   {:>6}  {:>8}  method (TRUE self-time, PIC-resolved callees included)", "share", "ticks");
    for (mid, n) in rows.iter().take(30) {
        let name =
            crate::jit::compiled_method_name(*mid).unwrap_or_else(|| format!("<mid {mid}>"));
        eprintln!(
            "[PC-PROFILE]   {:>5.1}%  {:>8}  {}",
            *n as f64 * 100.0 / total as f64,
            n,
            name
        );
    }
    dump_host(&samples, &ranges, total);
    #[cfg(windows)]
    dump_insn(&samples, &ranges);
    dump_host_timeline(&stamped, &ranges);
    dump_all_threads(&ranges);
    crate::pc_stack::dump(header, &ranges);
}

/// The whole PROCESS, not one thread (`RAVA_PROFILE_PC_ALL`). Two tables, and the second is the
/// one that pays: per-thread occupancy (how much of each armed thread's time is real work rather
/// than a park), and the aggregate host ranking with parked samples EXCLUDED — the first host
/// profile of this boot that covers the ring window instead of watching a blocked coordinator.
///
/// Parked samples are dropped rather than ranked because they are not a cost of any function:
/// a thread waiting for work bills its wait to whatever kernel routine implements the wait, and
/// including that row makes every real row smaller by the same meaningless factor.
/// Report `symbol+offset` buckets instead of whole symbols (`RAVA_PC_OFFSETS`, default OFF).
fn offsets() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PC_OFFSETS").is_some())
}

pub(crate) fn dump_all_threads(ranges: &[(usize, usize, u32)]) {
    #[cfg(windows)]
    {
        if !all_enabled() {
            return;
        }
        let all: Vec<(u64, usize, u8)> = match ALL_SAMPLES.lock() {
            Ok(mut v) => std::mem::take(&mut v),
            Err(_) => return,
        };
        if all.is_empty() {
            return;
        }
        // A park is named by its kernel routine; the set is small and stable on Windows.
        let parked = |s: &str| {
            s.starts_with("ZwWait")
                || s.starts_with("NtWait")
                || s.starts_with("ZwDelay")
                || s.starts_with("NtDelay")
        };
        let mut per_thread: std::collections::HashMap<u8, (u64, u64)> = Default::default();
        let mut per_fn: std::collections::HashMap<String, u64> = Default::default();
        let (mut n_live, mut n_jit) = (0u64, 0u64);
        for &(_, pc, slot) in &all {
            let e = per_thread.entry(slot).or_insert((0, 0));
            e.0 += 1;
            let i = ranges.partition_point(|r| r.0 <= pc);
            let in_jit = matches!(i.checked_sub(1).map(|j| ranges[j]), Some((_, end, _)) if pc < end);
            if in_jit {
                e.1 += 1;
                n_live += 1;
                n_jit += 1;
                continue;
            }
            let s = win::symbolize(pc);
            let f = s.rsplit_once('+').map_or(s.as_str(), |(f, _)| f).to_string();
            if parked(&f) {
                continue;
            }
            e.1 += 1;
            n_live += 1;
            // RAVA_PC_OFFSETS: keep the OFFSET WITHIN the function, bucketed to 16 bytes. A
            // symbol-level table cannot say anything about a function that is one big loop —
            // `opcodes::run::run` is 83% of a pure interpreter run and every guess about what
            // inside it costs is a guess. Bucketed offsets line up against `dumpbin /disasm`.
            let key = if offsets() {
                match s.rsplit_once('+') {
                    Some((name, off)) => {
                        let o = usize::from_str_radix(off.trim_start_matches("0x"), 16).unwrap_or(0);
                        format!("{name}+0x{:x}", o & !0xF)
                    }
                    None => f.clone(),
                }
            } else {
                f
            };
            *per_fn.entry(key).or_insert(0) += 1;
        }
        eprintln!(
            "[PC-ALL]   -- WHOLE PROCESS: {} samples over {} armed threads; {n_live} live ({:.1}%), {n_jit} in compiled code ({:.1}% of live) --",
            all.len(),
            per_thread.len(),
            n_live as f64 * 100.0 / all.len() as f64,
            n_jit as f64 * 100.0 / n_live.max(1) as f64
        );
        let mut th: Vec<(u8, (u64, u64))> = per_thread.into_iter().collect();
        th.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
        for (slot, (n, live)) in th {
            let name = format!("slot{slot}");
            eprintln!(
                "[PC-ALL]   {name:<8} samples={n:<6} live={live:<6} busy={:>5.1}%",
                live as f64 * 100.0 / n.max(1) as f64
            );
        }
        // CONCURRENCY OVER TIME — the measurement the whole parallelism arc has been inferring
        // from `cpu/wall` ratios. `cpu/wall` is an average over the run and cannot say WHICH
        // phase is serial; this can, because every tick samples every thread, so live samples
        // per tick IS the instantaneous thread count. A phase at 1.0 has no parallelism to win
        // back and a phase at 4.0 is already spending the cores it has.
        {
            let (t0, t1) = (all[0].0, all[all.len() - 1].0);
            let span = t1.saturating_sub(t0);
            if span > 0 {
                const NB: u64 = 10;
                eprintln!("[PC-ALL]   -- LIVE THREADS over time (concurrency, 10 slices) --");
                for b in 0..NB {
                    let (lo, hi) = (t0 + span * b / NB, t0 + span * (b + 1) / NB);
                    let (mut live, mut ticks) = (0u64, std::collections::HashSet::new());
                    for &(t, pc, _) in &all {
                        if t < lo || (t >= hi && b + 1 < NB) {
                            continue;
                        }
                        ticks.insert(t);
                        let i = ranges.partition_point(|r| r.0 <= pc);
                        let in_jit =
                            matches!(i.checked_sub(1).map(|j| ranges[j]), Some((_, e, _)) if pc < e);
                        if in_jit || !parked(
                            win::symbolize(pc)
                                .rsplit_once('+')
                                .map_or(win::symbolize(pc).as_str(), |(f, _)| f),
                        ) {
                            live += 1;
                        }
                    }
                    let n = ticks.len().max(1) as f64;
                    eprintln!(
                        "[PC-ALL]   t+{:>6}..{:<6} ms  ticks={:<5} live_samples={live:<6} concurrency={:.2}",
                        lo - t0,
                        hi - t0,
                        ticks.len(),
                        live as f64 / n
                    );
                }
            }
        }
        let mut fns: Vec<(String, u64)> = per_fn.into_iter().collect();
        fns.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!("[PC-ALL]   -- host functions, PARKED SAMPLES EXCLUDED (% of live) --");
        for (sym, n) in fns.iter().take(120) {
            eprintln!(
                "[PC-ALL]   {:>5.2}%  {:>8}  {sym}",
                *n as f64 * 100.0 / n_live.max(1) as f64,
                n
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = ranges;
    }
}

/// WHEN, not just what. The by-function table ranks the whole block at once, and a boot is a
/// sequence of phases that do completely different work — so a row can be 20% of the block and
/// still be 0% of the phase that binds the wall. That distinction is the whole question for a
/// PARK row (`ZwWaitForAlertByThreadId`): the sampler follows ONE thread (`arm_current_thread`,
/// first caller wins — the coordinator), so parked samples are the coordinator BLOCKED. Blocked
/// while workers compute is the ring joining and costs nothing extra; blocked during the serial
/// prefix is dead wall. A cumulative percentage cannot tell those apart and this split can.
///
/// Ten equal-time buckets over the block, top three host functions each. Deliberately NOT
/// aligned to guest phase markers: the markers are what a reader wants to correlate against,
/// and a bucket boundary chosen to match them would beg the question.
pub(crate) fn dump_host_timeline(stamped: &[(u64, usize)], ranges: &[(usize, usize, u32)]) {
    #[cfg(windows)]
    {
        let (Some(&(t0, _)), Some(&(t1, _))) = (stamped.first(), stamped.last()) else {
            return;
        };
        let span = t1.saturating_sub(t0);
        if span == 0 {
            return;
        }
        const NB: u64 = 10;
        eprintln!(
            "[PC-WHEN]   -- host by TIME BUCKET ({span} ms in {NB} slices, top 3 host fns each) --"
        );
        for b in 0..NB {
            let (lo, hi) = (t0 + span * b / NB, t0 + span * (b + 1) / NB);
            let mut per_fn: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
            let (mut n_host, mut n_all) = (0u64, 0u64);
            for &(t, pc) in stamped {
                // Half-open, except the last bucket which must also take the final sample.
                if t < lo || (t >= hi && b + 1 < NB) {
                    continue;
                }
                n_all += 1;
                let i = ranges.partition_point(|r| r.0 <= pc);
                if let Some((_, end, _)) = i.checked_sub(1).map(|j| ranges[j]) {
                    if pc < end {
                        continue; // inside compiled code — the other table owns it
                    }
                }
                n_host += 1;
                let s = win::symbolize(pc);
                let f = s.rsplit_once('+').map_or(s.as_str(), |(f, _)| f).to_string();
                *per_fn.entry(f).or_insert(0) += 1;
            }
            if n_all == 0 {
                continue;
            }
            let mut fns: Vec<(String, u64)> = per_fn.into_iter().collect();
            fns.sort_by(|a, b| b.1.cmp(&a.1));
            let top = fns
                .iter()
                .take(3)
                .map(|(f, n)| format!("{f} {:.0}%", *n as f64 * 100.0 / n_all as f64))
                .collect::<Vec<_>>()
                .join("  ·  ");
            eprintln!(
                "[PC-WHEN]   t+{:>6}..{:<6} ms  n={n_all:<5} host={:>4.0}%  {top}",
                lo - t0,
                hi - t0,
                n_host as f64 * 100.0 / n_all as f64
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (stamped, ranges);
    }
}

/// Decompose the samples that landed OUTSIDE compiled code — the Rust host, the JDK-native
/// bridges and the kernel. This is the half of the picture `rava_sampler`'s categories get
/// wrong on a call-dense workload: the JIT_EXEC guard is raised around `jit_run`, so every
/// callback from compiled code back into Rust that does not itself raise a category is billed
/// to JIT_EXEC even though no machine code is running. Symbolised against the shipped PDB.
/// ⭐**INSTRUCTION-LEVEL ATTRIBUTION INSIDE THE HOTTEST FUNCTION** (`RAVA_PC_INSN`).
///
/// The per-function table says `run_hot` is 44% of an interpreted run and stops there — but
/// `run_hot` IS the interpreter, so "the interpreter is hot" is not a finding. `symbolize`
/// already returns `name+offset` and the caller throws the offset away; keeping it turns the same
/// samples into a per-INSTRUCTION histogram, which is what decides whether the remaining ~15
/// cycles an opcode are the dispatch branch, the operand traffic, or something not yet suspected.
///
/// Offsets are into the loaded image, so they line up with
/// `llvm-objdump --start-address=<fn base>` on the `release-diag` build, whose codegen is
/// identical to `release` by construction (it inherits the profile and only keeps symbols).
#[cfg(windows)]
pub(crate) fn dump_insn(samples: &[usize], ranges: &[(usize, usize, u32)]) {
    if std::env::var_os("RAVA_PC_INSN").is_none() {
        return;
    }
    // Which function owns the most host samples?
    let mut per_fn: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for &pc in samples {
        let i = ranges.partition_point(|r| r.0 <= pc);
        if let Some((_, end, _)) = i.checked_sub(1).map(|j| ranges[j]) {
            if pc < end {
                continue;
            }
        }
        let s = win::symbolize(pc);
        let f = s.rsplit_once('+').map_or(s.as_str(), |(f, _)| f).to_string();
        *per_fn.entry(f).or_insert(0) += 1;
    }
    let Some((hot_fn, hot_n)) = per_fn.into_iter().max_by_key(|(_, n)| *n) else {
        return;
    };
    // Histogram the OFFSETS within it, and recover its base address so the numbers can be fed
    // straight to a disassembler.
    let mut per_off: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    let mut line_of: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    let mut base: Option<usize> = None;
    for &pc in samples {
        let s = win::symbolize(pc);
        let Some((f, off)) = s.rsplit_once('+') else { continue };
        if f != hot_fn {
            continue;
        }
        let off = u64::from_str_radix(off.trim_start_matches("0x"), 16)
            .or_else(|_| off.parse::<u64>())
            .unwrap_or(u64::MAX);
        if off == u64::MAX {
            continue;
        }
        base = Some(base.map_or(pc - off as usize, |b| b.min(pc - off as usize)));
        line_of.entry(off).or_insert_with(|| win::source_line(pc));
        *per_off.entry(off).or_insert(0) += 1;
    }
    let mut rows: Vec<(u64, u64)> = per_off.into_iter().collect();
    rows.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    eprintln!(
        "[PC-INSN] hottest host fn: {hot_fn} ({hot_n} samples), base {:#x}; top offsets",
        base.unwrap_or(0)
    );
    for (off, n) in rows.iter().take(24) {
        let pct = *n as f64 * 100.0 / hot_n as f64;
        eprintln!(
            "[PC-INSN]   +0x{off:<6x} {n:>8} {pct:>6.2}%  {}",
            line_of.get(off).map(String::as_str).unwrap_or("")
        );
    }
}

pub(crate) fn dump_host(samples: &[usize], ranges: &[(usize, usize, u32)], total: u64) {
    #[cfg(windows)]
    {
        let mut per_sym: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut n_out = 0u64;
        for &pc in samples {
            let i = ranges.partition_point(|r| r.0 <= pc);
            if let Some((_, end, _)) = i.checked_sub(1).map(|j| ranges[j]) {
                if pc < end {
                    continue;
                }
            }
            n_out += 1;
            *per_sym.entry(win::symbolize(pc)).or_insert(0) += 1;
        }
        if n_out == 0 {
            return;
        }
        // Roll the per-instruction rows up to whole FUNCTIONS. A hot Rust routine is spread
        // over dozens of `+0x..` offsets, so the by-address view understates it by an order of
        // magnitude and buries it below one-off addresses — the function view is the one that
        // ranks causes.
        let mut per_fn: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
        for (sym, n) in &per_sym {
            let f = sym.rsplit_once('+').map_or(sym.as_str(), |(f, _)| f);
            *per_fn.entry(f).or_insert(0) += n;
        }
        let mut fns: Vec<(&str, u64)> = per_fn.into_iter().collect();
        fns.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!(
            "[PC-PROFILE]   -- host side ({n_out} samples, {:.1}% of wall), by FUNCTION --",
            n_out as f64 * 100.0 / total as f64
        );
        // 120, not 30: at 30 rows this table covered ~half the samples and the REST was
        // invisible — which is exactly the span that decides whether the boot's cost is a few
        // named functions or a genuinely flat tail. A truncated table and a flat profile look
        // identical from the outside, and only one of them is a reason to stop looking.
        for (sym, n) in fns.iter().take(120) {
            eprintln!(
                "[PC-PROFILE]   {:>5.1}%  {:>8}  {}",
                *n as f64 * 100.0 / total as f64,
                n,
                sym
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (samples, ranges, total);
    }
}
