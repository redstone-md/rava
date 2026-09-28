//! Invoke-site census: which EMITTER each compiled invoke site takes.
//! `RAVA_INVOKE_CENSUS=1` prints it alongside the existing callee table.
//! Emit-time (once per site), off the hot path.

use std::sync::atomic::{AtomicU64, Ordering};

pub static INLINE_HIT: AtomicU64 = AtomicU64::new(0);
pub static DIRECT: AtomicU64 = AtomicU64::new(0);
pub static PIC: AtomicU64 = AtomicU64::new(0);
pub static CB: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn bump_inline() {
    INLINE_HIT.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn bump_direct() {
    DIRECT.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn bump_pic() {
    PIC.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn bump_cb() {
    CB.fetch_add(1, Ordering::Relaxed);
}

pub fn report() -> String {
    if std::env::var_os("RAVA_INVOKE_CENSUS").is_none() {
        return String::new();
    }
    let (ih, d, p, cb) = (
        INLINE_HIT.load(Ordering::Relaxed),
        DIRECT.load(Ordering::Relaxed),
        PIC.load(Ordering::Relaxed),
        CB.load(Ordering::Relaxed),
    );
    let tot = ih + d + p + cb;
    if tot == 0 {
        return String::new();
    }
    let pct = |v: u64| format!("{:.1}%", 100.0 * v as f64 / tot as f64);
    format!(
        "[INVOKE-SITES] {tot} invoke sites | inline_hit {ih} {} | direct {d} {} | pic {p} {} | cb {cb} {}\n",
        pct(ih),
        pct(d),
        pct(p),
        pct(cb)
    )
}
