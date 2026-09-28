//! 23.5 STATIC/SPECIAL DIRECT-CALL POTENTIAL (RAVA_JIT_DIRECT_PROBE, inert). Sizes the addressable set
//! of Lever B (monomorphic machine-code direct call for invokestatic/invokespecial, bypassing
//! rava_invoke) BEFORE building the codegen — the same leaf-only reality that made DVS a no-op applies:
//! a no-rooting direct call is safe only for a GC-LEAF callee (its ref args can't go stale). Per site
//! (memoized) it asks dispatch_resolve_cacheable_cls whether the warmed target is leaf+compiled, then
//! counts calls DIRECTABLE vs FLOOR. The ratio is the realistic ceiling for the rava_invoke drop.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_DIRECT_PROBE").is_some())
}

pub static DIRECTABLE: AtomicU64 = AtomicU64::new(0); // leaf+compiled — no-rooting machcode direct call
pub static ROOTABLE: AtomicU64 = AtomicU64::new(0); // compiled but NON-leaf — needs rooting (jit_direct_call ceiling)
pub static FLOOR: AtomicU64 = AtomicU64::new(0); // uncompiled / native / not-warmed — stays on rava_invoke
static MEMO: Mutex<Option<HashMap<(usize, u16), u8>>> = Mutex::new(None);

/// WHY a site landed in FLOOR. The bucket name "uncompiled/native/not-warmed" lumps
/// three unrelated causes together, and they need opposite fixes: a site the
/// interpreter never resolved (so `tlcache::call_get` is empty) is a WARMING problem,
/// a callee that is genuinely uncompiled is a COMPILE-ORDER problem, and a callee that
/// is compiled but refused by `dispatch_resolve_cacheable_cls` is an ADMISSION problem.
/// Counted once per site (inside the memo miss), so this is a site census, not a call
/// census. 0 = no member_ref, 1 = tlcache miss, 2 = callee not compiled, 3 = compiled
/// but not leaf-cacheable (⇒ classified ROOTABLE, kept here for the totals to close).
pub static FLOOR_WHY: [AtomicU64; 4] =
    [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
/// Distinct sites that reached the classifier at all (memo misses).
pub static SITES: AtomicU64 = AtomicU64::new(0);

pub fn why(reason: usize) {
    SITES.fetch_add(1, Relaxed);
    FLOOR_WHY[reason.min(3)].fetch_add(1, Relaxed);
}

pub fn memo_get(site: (usize, u16)) -> Option<u8> {
    MEMO.lock().ok()?.as_ref()?.get(&site).copied()
}
pub fn memo_put(site: (usize, u16), v: u8) {
    MEMO.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(site, v);
}
pub fn count(klass: u8) {
    match klass {
        2 => DIRECTABLE.fetch_add(1, Relaxed),
        1 => ROOTABLE.fetch_add(1, Relaxed),
        _ => FLOOR.fetch_add(1, Relaxed),
    };
}

pub fn dump() {
    if !enabled() {
        return;
    }
    let (d, r, f) = (DIRECTABLE.load(Relaxed), ROOTABLE.load(Relaxed), FLOOR.load(Relaxed));
    let t = d + r + f;
    let pct = |x: u64| if t > 0 { format!("{}.{}%", x * 1000 / t / 10, x * 1000 / t % 10) } else { "0%".into() };
    eprintln!("[DIRECT-POT] static/special plain-invoke calls = {t}");
    eprintln!("[DIRECT-POT]   LEAF+compiled (no-rooting direct call — Lever B as specified) = {d} ({})", pct(d));
    eprintln!("[DIRECT-POT]   compiled NON-leaf (needs ROOTING — jit_direct_call already serves) = {r} ({})", pct(r));
    eprintln!("[DIRECT-POT]   floor (uncompiled/native/not-warmed — stays on rava_invoke) = {f} ({})", pct(f));
    eprintln!("[DIRECT-POT]   ⇒ no-rooting Lever B ceiling ≈ {}; rooted ceiling ≈ {}", pct(d), pct(d + r));
    eprintln!(
        "[DIRECT-WHY] distinct sites={} | no-memberref={} tlcache-MISS={} callee-NOT-COMPILED={} compiled-not-leaf={}",
        SITES.load(Relaxed),
        FLOOR_WHY[0].load(Relaxed),
        FLOOR_WHY[1].load(Relaxed),
        FLOOR_WHY[2].load(Relaxed),
        FLOOR_WHY[3].load(Relaxed),
    );
}
