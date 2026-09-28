//! M5.3 ESCAPE-TARGET SCAN (RAVA_ESCAPE_SCAN, inert). The complete allocation histogram: at each minor
//! GC, linear-walk the young nursery [base,top) BEFORE Cheney and tally every object's class_id. Unlike
//! structkey_scan's tlab_try_alloc hook (Rust-path only), this catches INLINE-JIT bump allocations too
//! (the compiled worldgen's short-lived BlockPos / iterator / boxed objects) — the escape-analysis /
//! scalar-replacement targets agy flagged as the 10x lever. Read-only ⇒ inert, determinism-neutral.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_ESCAPE_SCAN").is_some())
}

const N: usize = 1 << 16;
static COUNT: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static TOTAL: AtomicU64 = AtomicU64::new(0);
static WALKS: AtomicU64 = AtomicU64::new(0);

/// Tally one live-in-young object by class_id (called from the pre-collection young walk).
#[inline]
pub fn note(class_id: u32) {
    if (class_id as usize) < N {
        COUNT[class_id as usize].fetch_add(1, Relaxed);
    }
    TOTAL.fetch_add(1, Relaxed);
}

/// Called once per minor GC after the walk — periodic dump. Minor GCs are RARE (large young → ~0%
/// GC per the boot-wall), and the boot is hard-killed (no clean exit dump), so dump early + often.
pub fn walk_done() {
    let w = WALKS.fetch_add(1, Relaxed) + 1;
    if w == 3 || w == 8 || w % 15 == 0 {
        dump();
    }
}

pub fn dump() {
    if !enabled() {
        return;
    }
    let mut rows: Vec<(u32, u64)> = (0..N as u32)
        .filter_map(|c| {
            let n = COUNT[c as usize].load(Relaxed);
            if n > 0 { Some((c, n)) } else { None }
        })
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    let total = TOTAL.load(Relaxed).max(1);
    eprintln!(
        "[ESCAPE-SCAN] {} objects walked over {} minor GCs | {} distinct classes. Top allocated (escape-analysis targets):",
        total,
        WALKS.load(Relaxed),
        rows.len()
    );
    eprintln!("[ESCAPE-SCAN]  {:>13} {:>6}   fields(i J fp r)   class", "count", "%");
    for (cid, n) in rows.iter().take(25) {
        let (name, layout) = match crate::nextgen::class_layout(*cid) {
            Some(x) => x,
            None => continue,
        };
        let (mut i, mut j, mut fp, mut r) = (0u32, 0u32, 0u32, 0u32);
        for &(_, k) in layout.iter() {
            match k { 0 => i += 1, 1 => j += 1, 2 | 3 => fp += 1, 6 => r += 1, _ => {} }
        }
        eprintln!(
            "[ESCAPE-SCAN] {:>13} {:>5.1}%   {}i {}J {}fp {}r   {}",
            n, *n as f64 * 100.0 / total as f64, i, j, fp, r, name
        );
    }
}
