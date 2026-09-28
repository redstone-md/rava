//! `RAVA_SHAPE_CENSUS` — the SHAPE of every interpreted callee body, counted before any
//! machinery is built for it.
//!
//! Ledger row 344: the interpreter gate spends **39.3% in call machinery** and every part of it
//! is already the product of an optimisation pass, so the remaining move is FEWER FRAMES. The
//! cheapest frame to remove is the one a trivial accessor never needed — but "MC is full of
//! getters" is a belief, and this tree has repeatedly priced mechanisms whose population turned
//! out to be zero (the λ-PIC's `ADMITTED = 0` is the most recent). So: count first.
//!
//! Shapes, in the order they are tested:
//!   * GETTER  `aload_0; getfield idx; <T>return`                          — 5 bytes
//!   * CONST   `<const>; <T>return` / `return`                              — 1-2 bytes
//!   * SETTER  `aload_0; <T>load_1; putfield idx; return`                   — 6 bytes
//!   * SMALL   any body under 16 bytes that is none of the above
//!   * OTHER   everything else
//!
//! Inert unless the variable is set: one `OnceLock` bool on the call path and nothing else.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_SHAPE_CENSUS").is_some())
}

const N: usize = 6;
static COUNT: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
const NAMES: [&str; N] = ["GETTER", "CONST", "SETTER", "SMALL(<16B)", "OTHER", "  of which STRAIGHT"];

/// Return-opcode test: `ireturn`..`areturn` (0xac-0xb0) or `return` (0xb1).
#[inline]
fn is_return(op: u8) -> bool {
    (0xac..=0xb1).contains(&op)
}


pub fn note(bc: &[u8]) {
    let idx = match bc {
        // aload_0 (0x2a), getfield (0xb4) hi lo, <return>
        [0x2a, 0xb4, _, _, r] if is_return(*r) => 0,
        // aload_0, <load_1>, putfield hi lo, return  — the four `?load_1` opcodes are
        // iload_1 0x1b, lload_1 0x1f, fload_1 0x23, dload_1 0x27, aload_1 0x2b.
        [0x2a, l, 0xb5, _, _, 0xb1]
            if matches!(*l, 0x1b | 0x1f | 0x23 | 0x27 | 0x2b) =>
        {
            2
        }
        [r] if is_return(*r) => 1,
        [_, r] if is_return(*r) => 1,
        b if b.len() < 16 => 3,
        _ => 4,
    };
    COUNT[idx].fetch_add(1, Relaxed);
    // A SECOND, OVERLAPPING bucket: how many of these bodies a straight-line executor would be
    // allowed to run. Counted across every shape, not just `SMALL`, because a body can be short
    // and still contain an opcode whose semantics a second implementation could get wrong — the
    // whole point of asking before building. Its percentage is therefore of the TOTAL and does not
    // sum with the five above.
    if crate::flatcall::is_straight_candidate(bc) {
        COUNT[5].fetch_add(1, Relaxed);
    }
}

pub fn report() -> String {
    if !enabled() {
        return String::new();
    }
    let v: Vec<u64> = COUNT.iter().map(|c| c.load(Relaxed)).collect();
    let tot: u64 = v[..5].iter().sum();
    if tot == 0 {
        return String::new();
    }
    let mut out = format!("[SHAPE-CENSUS] {tot} interpreted calls by callee body shape:");
    for i in 0..N {
        out.push_str(&format!(
            "\n[SHAPE-CENSUS] {:>12}  {:>12}  {:5.1}%",
            NAMES[i],
            v[i],
            v[i] as f64 * 100.0 / tot as f64
        ));
    }
    out
}
