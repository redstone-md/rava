//! 23.7 SELF_REG COVERAGE (RAVA_JIT_SELFREG_PROBE, inert). Sizes Option-A (self-registering callee,
//! Deep-IC v3) for the ROOTED-machcode lever: of the NON-LEAF compiled callees in the two target pools
//! (SS = static/special; HR = heap-Ref virtual/interface), how many are self_reg-ELIGIBLE — i.e.
//! non-leaf + compiled + frame_len (max_locals+max_stack) ≤ the deep callbuf. Those get the self-
//! registering prologue under ic_deep, so the unified rooted hit serves them with no caller-side
//! cells_push. If self_reg covers the bulk, Option B (inline caller cells_push) is unnecessary.
//! Run with RAVA_IC_DEEP=1 so dispatch_callbuf_slots()=32 is the eligibility threshold.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_SELFREG_PROBE").is_some())
}

// Per pool, 4 buckets: 0=floor(uncompiled/native), 1=leaf, 2=self_reg-eligible non-leaf, 3=too-big non-leaf.
pub static SS: [AtomicU64; 4] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
pub static HR: [AtomicU64; 4] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static MEMO: Mutex<Option<HashMap<(usize, u16), u8>>> = Mutex::new(None);

pub fn memo_get(site: (usize, u16)) -> Option<u8> {
    MEMO.lock().ok()?.as_ref()?.get(&site).copied()
}
pub fn memo_put(site: (usize, u16), v: u8) {
    MEMO.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(site, v);
}
/// pool: 0 = SS (static/special), 1 = HR (heap-Ref).
pub fn count(pool: u8, klass: u8) {
    let arr = if pool == 0 { &SS } else { &HR };
    arr[(klass & 3) as usize].fetch_add(1, Relaxed);
}

fn line(tag: &str, a: &[AtomicU64; 4]) {
    let b: [u64; 4] = std::array::from_fn(|i| a[i].load(Relaxed));
    // non-leaf compiled pool = self_reg-eligible (2) + too-big (3).
    let nonleaf = b[2] + b[3];
    let pct = if nonleaf > 0 { b[2] * 1000 / nonleaf } else { 0 };
    eprintln!(
        "[SELFREG] {tag}: calls={}  floor={} leaf={} nonleaf-selfreg={} nonleaf-toobig={}  ⇒ self_reg covers {}.{}% of the NON-LEAF compiled pool (Option A)",
        b[0] + b[1] + b[2] + b[3], b[0], b[1], b[2], b[3], pct / 10, pct % 10
    );
}
pub fn dump() {
    if !enabled() {
        return;
    }
    line("static/special", &SS);
    line("heap-Ref virt/iface", &HR);
}
