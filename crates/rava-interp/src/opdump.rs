//! `RAVA_OPDUMP=<path>` — capture the first N executed opcodes to a file.
//!
//! ⭐**WHY A REAL STREAM MATTERS.** `examples/dispatchbench` prices dispatch styles, and on a
//! xorshift-RANDOM opcode stream it measures the branch predictor at **19x** (9.63 ns/op against
//! 0.51 for a predictable stream) while replicating the dispatch site buys nothing. But a random
//! stream is the worst case by construction: it has no structure beyond first order, and modern
//! indirect predictors use long path history. Real bytecode is full of higher-order structure —
//! loops, repeated call shapes, the same field access sequence per object — so a per-opcode
//! branch site may predict far better on it than the first-order successor table (22.9% top-1)
//! suggests. That difference is exactly the case for or against a template interpreter, and it
//! cannot be settled on a synthetic stream.
//!
//! Inert unless the variable is set: one bool, read once per dispatch-loop entry.

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

const CAP: usize = 64 * 1024 * 1024;

static BUF: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static N: AtomicUsize = AtomicUsize::new(0);

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_OPDUMP").is_some())
}

/// Append one executed opcode. Stops at `CAP` — a bounded prefix is a representative sample and
/// an unbounded one is a memory leak with a benchmark attached.
#[inline]
pub fn note(op: u8) {
    if N.fetch_add(1, Relaxed) >= CAP {
        return;
    }
    if let Ok(mut b) = BUF.lock() {
        if b.capacity() == 0 {
            b.reserve(CAP);
        }
        b.push(op);
    }
}

/// Write the captured prefix out. Called from the clean-exit dump.
pub fn dump_if_on() {
    if !enabled() {
        return;
    }
    let Ok(b) = BUF.lock() else { return };
    if b.is_empty() {
        return;
    }
    let Ok(path) = std::env::var("RAVA_OPDUMP") else { return };
    match std::fs::write(&path, &b[..]) {
        Ok(()) => eprintln!("[OPDUMP] wrote {} opcodes to {path}", b.len()),
        Err(e) => eprintln!("[OPDUMP] could not write {path}: {e}"),
    }
}
