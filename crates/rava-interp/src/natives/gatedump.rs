//! Reading the dispatch gate counters back out.

use crate::natives::*;

use crate::natives::*;
/// Print the gate tally + census. Called from the CLI exit chain next to the other probes.
pub fn dispatch_gate_dump() {
    if !dispatch_gate_verify() {
        return;
    }
    use std::sync::atomic::Ordering::Relaxed;
    let (t, s, h, n, st) = (
        GATE_TOTAL.load(Relaxed),
        GATE_SKIPPED.load(Relaxed),
        GATE_SERVED.load(Relaxed),
        GATE_NAME_BLOCKED.load(Relaxed),
        GATE_STAGES_SKIPPED.load(Relaxed),
    );
    eprintln!(
        "[DISPATCH-GATE] {t} owner-cascade entries; {st} of {} sub-dispatcher entries removed \
         ({:.1}%); {s} whole cascades decidable from the owner alone ({:.2}%); \
         {n} held by the owner-agnostic NAME list; {h} skipped-but-served (must be 0)",
        t * STAGE_COUNT,
        st as f64 * 100.0 / (t * STAGE_COUNT).max(1) as f64,
        s as f64 * 100.0 / t.max(1) as f64,
    );
    if let Ok(c) = gate_census().lock() {
        let mut rows: Vec<_> = c.values().cloned().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!("[DISPATCH-GATE] top 30 (owner#name, calls) of {} distinct:", rows.len());
        for (label, calls) in rows.into_iter().take(30) {
            eprintln!("[DISPATCH-GATE]   {calls:>12}  {label}");
        }
    }
}
pub(crate) fn valueof_probe_record(cls: String, ns: u64, out_len: usize) {
    use std::sync::{Mutex, OnceLock};
    type T = std::collections::HashMap<String, (u64, u64, u64)>; // cls → (calls, ns, out_bytes)
    pub(crate) static TALLY: OnceLock<Mutex<T>> = OnceLock::new();
    let m = TALLY.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Ok(mut t) = m.lock() {
        let e = t.entry(cls).or_insert((0, 0, 0));
        e.0 += 1;
        e.1 += ns;
        e.2 += out_len as u64;
        // periodic dump every ~60s of accumulated valueOf time OR every 500k calls
        static NEXTC: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(200_000);
        let total_calls: u64 = t.values().map(|v| v.0).sum();
        if total_calls >= NEXTC.load(std::sync::atomic::Ordering::Relaxed) {
            NEXTC.store(total_calls + 200_000, std::sync::atomic::Ordering::Relaxed);
            let mut rows: Vec<_> = t.iter().map(|(k, v)| (k.clone(), *v)).collect();
            rows.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
            let tot_ns: u64 = rows.iter().map(|r| r.1 .1).sum();
            eprintln!("[VALUEOF-PROBE] {} classes, {} calls, {:.1}s total; top 15 by time:", rows.len(), total_calls, tot_ns as f64 / 1e9);
            for (cls, (c, ns, ob)) in rows.into_iter().take(15) {
                eprintln!(
                    "[VALUEOF-PROBE]   {:>7.2}s {:>9} calls  avg={:>6.1}us avglen={:>5}  {}",
                    ns as f64 / 1e9, c, ns as f64 / 1e3 / c.max(1) as f64, ob / c.max(1), cls
                );
            }
        }
    }
}
