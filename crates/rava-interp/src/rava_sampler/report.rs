//! Printing what the sampler collected.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::rava_sampler::*;

/// Per-method tick tallies. Touched ONLY by the sampler thread and the dump — never by the
/// `CURRENT_METHOD` stores on the guest path — so the lock can never contend with execution.
pub(crate) struct MethodTally {
    /// Ticks per method id, run to date; index 0 = "no compiled frame entered".
    cum: Vec<u64>,
    /// Snapshot of `cum` at the previous dump, so a block delta is a subtraction.
    prev: Vec<u64>,
}
impl MethodTally {
    pub(crate) const fn new() -> Self {
        Self { cum: Vec::new(), prev: Vec::new() }
    }
    pub(crate) fn hit(&mut self, mid: u32) {
        let i = mid as usize;
        if self.cum.len() <= i {
            self.cum.resize(i + 1, 0);
        }
        self.cum[i] += 1;
    }
    /// Top-`n` rows as (mid, cumulative, delta-since-previous-dump).
    /// `Rank::Block` ranks by the DELTA, which is what makes one dump block a standalone
    /// profile of the guest phase that ran during it — a method that owns a late phase would
    /// never surface in a cumulative ranking dominated by an earlier one.
    pub(crate) fn top(&self, n: usize, rank: Rank) -> Vec<(u32, u64, u64)> {
        let mut rows: Vec<(u32, u64, u64)> = self
            .cum
            .iter()
            .enumerate()
            .skip(1) // 0 = "no compiled method entered"
            .map(|(i, &c)| (i as u32, c, c - self.prev.get(i).copied().unwrap_or(0)))
            .filter(|&(_, c, d)| if matches!(rank, Rank::Block) { d > 0 } else { c > 0 })
            .collect();
        match rank {
            Rank::Cumulative => rows.sort_by(|a, b| b.1.cmp(&a.1)),
            Rank::Block => rows.sort_by(|a, b| b.2.cmp(&a.2)),
        }
        rows.truncate(n);
        rows
    }
    pub(crate) fn mark(&mut self) {
        self.prev.clone_from(&self.cum);
    }
    pub(crate) fn total(&self) -> u64 {
        self.cum.iter().skip(1).sum()
    }
    /// Ticks attributed during this block only (all methods, not just the printed top-N).
    pub(crate) fn block_total(&self) -> u64 {
        self.cum
            .iter()
            .enumerate()
            .skip(1)
            .map(|(i, &c)| c - self.prev.get(i).copied().unwrap_or(0))
            .sum()
    }
    /// JIT_EXEC ticks with no compiled frame published — the cnp tier-0 / AOT lanes and the
    /// window where the JIT_EXEC guard is up but `jit_run` has not reached `call_raw`.
    pub(crate) fn unattributed(&self) -> u64 {
        self.cum.first().copied().unwrap_or(0)
    }
}
pub(crate) fn dump_now(tag: &str) {
    // Wall-clock stamp (Unix epoch ms) on every dump block: the guest's own stdout log lines
    // carry local [HH:MM:SS] stamps, and `t=` alone is relative to sampler ARM time, which
    // trails process start by an unknown class-loading delay — so epoch time is what lets a
    // dump block be aligned EXACTLY against the guest's phase markers.
    let epoch_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let stamp = format!("{tag} t={:.1}s epoch_ms={epoch_ms}", elapsed_s());
    let tot = TOTAL.load(Relaxed).max(1);
    eprintln!(
        "[WALL-PROFILE {}] {} samples @10ms = ~{:.1}s sampled wall-time; distribution (self-time by activity):",
        stamp,
        TOTAL.load(Relaxed),
        TOTAL.load(Relaxed) as f64 * 0.01
    );
    let mut rows: Vec<(usize, u64)> = (0..NCAT).map(|i| (i, SAMPLES[i].load(Relaxed))).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    for (i, n) in rows {
        let pct = n as f64 * 100.0 / tot as f64;
        let bar = "#".repeat((pct / 2.0).round() as usize);
        eprintln!("[WALL-PROFILE]   {:>6.1}%  {:>8}  {:<34} {}", pct, n, NAMES[i], bar);
    }
    dump_methods(tag);
    crate::pc_profile::dump(&stamp);
    crate::offload_probe::dump();
}

/// Top-20 compiled methods by ENTRY self-time — the decomposition of the JIT_EXEC row.
/// `cum` is process-to-date; `delta` is this block only (since the previous dump), which is
/// what isolates a single guest phase. Name resolution walks a lock and allocates, but this
/// runs at most once per dump period, never on the hot path.
pub(crate) fn dump_methods(tag: &str) {
    const TOPN: usize = 20;
    let (block, run, mtot, btot, unattr) = {
        let Ok(mut t) = METHODS.lock() else { return };
        let r = (
            t.top(TOPN, Rank::Block),
            t.top(TOPN, Rank::Cumulative),
            t.total(),
            t.block_total(),
            t.unattributed(),
        );
        t.mark();
        r
    };
    if run.is_empty() && unattr == 0 {
        return;
    }
    let t = elapsed_s();
    // BLOCK first: on its own it is the profile of whatever guest phase ran since the last
    // dump, which is how SETUP is told apart from FILL (or a boot's pre-ring work from the
    // ring) without the tool knowing anything about the guest.
    print_method_table(
        &format!("{tag} BLOCK t={t:.1}s"),
        &format!("methods by self-time IN THIS BLOCK ({btot} ticks this block)"),
        &block,
        btot,
        true,
    );
    print_method_table(
        &format!("{tag} TOTAL t={t:.1}s"),
        &format!("methods by self-time RUN-TO-DATE ({mtot} JIT_EXEC ticks attributed)"),
        &run,
        SAMPLES[JIT_EXEC as usize].load(Relaxed),
        false,
    );
    if unattr > 0 {
        eprintln!(
            "[WALL-METHOD]   unattributed (cnp/AOT lane or JIT_EXEC guard up before call_raw): \
             {} ticks = {:.1}% of JIT_EXEC",
            unattr,
            unattr as f64 * 100.0 / SAMPLES[JIT_EXEC as usize].load(Relaxed).max(1) as f64
        );
    }
    // THE BRIDGE VIEW. NATIVE_BRIDGES is the second-largest category of the SERIAL window (~21%
    // of it, against JIT_EXEC's ~60%) and nothing decomposed it: the method table is scoped to
    // JIT_EXEC ticks by construction, so every bridge tick fell outside every row printed above.
    let (brun, bblock, bbtot) = match BRIDGES.lock() {
        Ok(mut t) => {
            // 90, not 20: at 20 rows this table covered 57.7% of bridge ticks and the other
            // 42% were invisible. The PC host table had the same defect (ledger row 136) and
            // three "the profile is flat" conclusions rested on it.
            let a = t.top(90, Rank::Cumulative);
            let b = t.top(90, Rank::Block);
            let bt = t.block_total();
            t.mark();
            (a, b, bt)
        }
        Err(_) => (Vec::new(), Vec::new(), 0),
    };
    print_id_table(
        "WALL-BRIDGE",
        "bridge",
        &format!("{tag} BLOCK t={t:.1}s"),
        &format!("bridges by time IN THIS BLOCK ({bbtot} ticks this block)"),
        &bblock,
        bbtot,
        true,
        &bridge_name,
    );
    // THE SITE VIEW of the PIC-miss host path. A category total cannot distinguish "a few
    // megamorphic sites carry it" from "it is spread over every call in the program", and those
    // two answers call for completely different work — the first is a per-site mechanism, the
    // second is not fixable by dispatch at all. `RAVA_DISPATCH_TABLE` removed 12.6% of all misses
    // for ZERO, which is exactly what "the removed ones were the cheap ones" would look like.
    if let Ok(m) = SITES.lock() {
        if let Some(map) = m.as_ref() {
            let cbt = SAMPLES[JIT_CALLBACK as usize].load(Relaxed).max(1);
            let mut rows: Vec<((usize, u32), u64)> =
                map.iter().map(|(k, v)| (*k, *v)).collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1));
            let shown: u64 = rows.iter().take(90).map(|r| r.1).sum();
            eprintln!(
                "[WALL-SITE {tag} t={t:.1}s] top-{} of {} distinct sites; shown hold {:.1}% of JIT_CALLBACK:",
                rows.len().min(90),
                rows.len(),
                shown as f64 * 100.0 / cbt as f64
            );
            for ((cls, cp), n) in rows.iter().take(90) {
                // SAFETY: the loader never evicts, so a class pointer handed to compiled code
                // outlives the process. Dump-time only.
                let name = if *cls != 0 {
                    let lc: &crate::class_loader::LoadedClass = unsafe { &*(*cls as *const _) };
                    match lc.file.pool.member_ref(*cp as u16) {
                        Some(r) => format!(
                            "{} @cp{} -> {}.{}{}",
                            lc.file.this_class, cp, r.owner, r.name, r.descriptor
                        ),
                        None => format!("{} @cp{}", lc.file.this_class, cp),
                    }
                } else {
                    "<no site>".to_string()
                };
                eprintln!(
                    "[WALL-SITE]   {:>5.1}%  {:>8}  {}",
                    *n as f64 * 100.0 / cbt as f64,
                    n,
                    name
                );
            }
        }
    }
    // WHO CALLS THE BRIDGES. See BRIDGE_SITES for the attribution limit this table inherits.
    if let Ok(m) = BRIDGE_SITES.lock() {
        if let Some(map) = m.as_ref() {
            let nbt = SAMPLES[NATIVE_BRIDGES as usize].load(Relaxed).max(1);
            let mut rows: Vec<((usize, u32), u64)> = map.iter().map(|(k, v)| (*k, *v)).collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1));
            let shown: u64 = rows.iter().take(90).map(|r| r.1).sum();
            eprintln!(
                "[BRIDGE-CALLER {tag} t={t:.1}s] top-{} of {} sites; top-20 hold {:.1}% of NATIVE_BRIDGES:",
                rows.len().min(90),
                rows.len(),
                shown as f64 * 100.0 / nbt as f64
            );
            for ((cls, cp), n) in rows.iter().take(90) {
                // SAFETY: the loader never evicts, so a class pointer handed to compiled code
                // outlives the process. Dump-time only.
                let name = if *cls != 0 {
                    let lc: &crate::class_loader::LoadedClass = unsafe { &*(*cls as *const _) };
                    match lc.file.pool.member_ref(*cp as u16) {
                        Some(r) => format!(
                            "{} @cp{} -> {}.{}{}",
                            lc.file.this_class, cp, r.owner, r.name, r.descriptor
                        ),
                        None => format!("{} @cp{}", lc.file.this_class, cp),
                    }
                } else {
                    "<no site>".to_string()
                };
                eprintln!(
                    "[BRIDGE-CALLER]   {:>5.1}%  {:>8}  {}",
                    *n as f64 * 100.0 / nbt as f64,
                    n,
                    name
                );
            }
        }
    }
    print_id_table(
        "WALL-BRIDGE",
        "bridge",
        &format!("{tag} TOTAL t={t:.1}s"),
        &format!(
            "bridges by time RUN-TO-DATE ({} NATIVE_BRIDGES ticks)",
            SAMPLES[NATIVE_BRIDGES as usize].load(Relaxed)
        ),
        &brun,
        SAMPLES[NATIVE_BRIDGES as usize].load(Relaxed),
        false,
        &bridge_name,
    );
}

/// The ranked-table printer, shared by the method and bridge views: same columns, same ranking,
/// only the tag, the column label and the id->name resolver differ.
#[allow(clippy::too_many_arguments)]
pub(crate) fn print_id_table(
    tag: &str,
    col: &str,
    header: &str,
    what: &str,
    rows: &[(u32, u64, u64)],
    share_of: u64,
    by_block: bool,
    name_of: &dyn Fn(u32) -> Option<String>,
) {
    if rows.is_empty() {
        return;
    }
    let den = share_of.max(1);
    eprintln!("[{} {}] top-{} {}:", tag, header, rows.len(), what);
    eprintln!(
        "[{}]   {:>6}  {:>8}  {:>8}  {}",
        tag, "share", "block", "total", col
    );
    for &(mid, cum, delta) in rows {
        let name = name_of(mid).unwrap_or_else(|| "<unknown id>".to_string());
        let lead = if by_block { delta } else { cum };
        eprintln!(
            "[{}]   {:>5.1}%  {:>8}  {:>8}  {}",
            tag,
            lead as f64 * 100.0 / den as f64,
            delta,
            cum,
            name
        );
    }
}
