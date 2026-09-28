//! `RAVA_SLOT_CENSUS` — WHAT ACTUALLY GOES ON THE OPERAND STACK, BY VARIANT.
//!
//! ⭐**WHY, and why before any code.** Row 387 prices two lanes out of the 32-byte operand slot:
//! the full tagged word at **2.09×**, and a mechanical intermediate at **1.29×** — the same enum
//! with a THIN string pointer (`Arc<String>` rather than the fat `Arc<str>`), which drops the
//! payload to 8 bytes and the slot to 16.
//!
//! The 1.29× is not free: it buys the smaller slot by adding **one indirection to every string
//! read**. Whether that is a win therefore depends entirely on a number nobody in this tree has
//! ever measured — **what fraction of operand traffic is `Str`?** A boot is string-dense; if most
//! slot touches are strings, the intermediate step pays for its own win and the only real lane is
//! the full rewrite. If strings are a percent, the mechanical change is straightforwardly good.
//!
//! ⚠**This counts PUSHES IN THE HOT LOOP ONLY**, which is where these programs spend essentially
//! all their opcodes (`ArithLoop` runs 99,764 opcodes per hot-loop entry) but is NOT all of a boot.
//! It undercounts the cold path deliberately: a counter on every cold push would perturb the very
//! loop the answer is about, and this tree has already lost measurements that way — the shape
//! census's own atomic tripled `ArithLoop`'s reported time (row 385).
//!
//! Inert unless the variable is set: one `OnceLock` bool on the push path and nothing else.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_SLOT_CENSUS").is_some())
}

const N: usize = 10;
static COUNT: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
const NAMES: [&str; N] = [
    "Int", "Long", "Float", "Double", "Str", "Ref", "Array", "Obj", "Null", "other",
];

/// Tally one operand push by variant.
#[inline]
pub fn note(v: &crate::value::Value) {
    use crate::value::Value::*;
    let i = match v {
        Int(_) => 0,
        Long(_) => 1,
        Float(_) => 2,
        Double(_) => 3,
        Str(_) => 4,
        Ref(_) => 5,
        Array(_) => 6,
        #[cfg(feature = "next-gen-gc")]
        Obj(_) => 7,
        Null => 8,
        _ => 9,
    };
    COUNT[i].fetch_add(1, Relaxed);
}

pub fn report() -> String {
    if !enabled() {
        return String::new();
    }
    let v: Vec<u64> = COUNT.iter().map(|c| c.load(Relaxed)).collect();
    let tot: u64 = v.iter().sum();
    if tot == 0 {
        return String::new();
    }
    let mut out = format!("[SLOT-CENSUS] {tot} hot-loop operand pushes by variant:");
    let mut idx: Vec<usize> = (0..N).collect();
    idx.sort_by_key(|&i| std::cmp::Reverse(v[i]));
    for i in idx {
        if v[i] == 0 {
            continue;
        }
        out.push_str(&format!(
            "\n[SLOT-CENSUS] {:>8}  {:>14}  {:5.2}%",
            NAMES[i],
            v[i],
            v[i] as f64 * 100.0 / tot as f64
        ));
    }
    // The one number the whole question turns on, spelled out rather than left to be derived.
    out.push_str(&format!(
        "\n[SLOT-CENSUS] => Str is {:.2}% of operand pushes; the 16-byte slot adds ONE indirection \
         to exactly that share (row 387)",
        v[4] as f64 * 100.0 / tot as f64
    ));
    out
}
