//! **WHICH METHODS THE NATIVES ACTUALLY SERVE — recorded, not predicted.**
//!
//! The compile gate for a natively-served namespace has been wrong in both directions all day.
//! Refusing the whole namespace interprets forever every method the natives never touch; allowing
//! it compiles bodies that then serve receivers the natives were handling, which is the
//! counter-example that retired `RAVA_NS_BYTECODE` (`AbstractIntList.toString()` under
//! `ToStringHelper.toString()`, reachable only when a representation change moved classes between
//! the two heaps).
//!
//! Both failures come from the same missing fact: **whether the natives serve a given method is
//! decidable only per (class, name, descriptor), and no predicate in the tree answers it.** The
//! cascade EXECUTES an arm rather than answering a question, and its admission is spread over a
//! dozen files.
//!
//! So it is not predicted here — it is OBSERVED. Every time the native cascade answers a call for
//! a class in a served namespace, that method is marked. The compile gate then refuses exactly the
//! marked ones, which is the same evidence the interpreter acted on, in the same process.
//!
//! ⚠**Not airtight, and the gap is stated rather than hidden:** a method first served AFTER it
//! compiled is not marked in time. That window is small (service happens from the first call,
//! compilation only after the threshold) and the direction of the residual risk is unchanged from
//! today's default, which refuses the namespace outright. What this buys is that the flag becomes
//! testable against the counter-example instead of merely disabled.
//!
//! Direct-mapped and lock-free: a collision marks an extra method, and marking too many only means
//! compiling too few — the safe direction. The whole table is inert unless the relaxation flag is
//! on, so a default run pays one relaxed load per native service.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

const N: usize = 4096;
#[allow(clippy::declare_interior_mutable_const)]
const Z: AtomicU64 = AtomicU64::new(0);
static MARKS: [AtomicU64; N] = [Z; N];
static ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Armed by the relaxation flag: without it nothing consults the table, so nothing needs filling.
pub(crate) fn arm() {
    ON.store(true, Relaxed);
}

#[inline]
pub(crate) fn armed() -> bool {
    ON.load(Relaxed)
}

#[inline]
fn key(class: &str, name: &str, desc: &str) -> u64 {
    // FNV over the three parts with separators, so `a.b` and `ab.` cannot collide by construction
    // of the separator alone.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [class.as_bytes(), b"#", name.as_bytes(), b"#", desc.as_bytes()] {
        for &b in part {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    h | 1 // never 0: 0 means "empty slot"
}

/// The natives answered this call. Called from the cascade's success path.
pub(crate) fn mark(class: &str, name: &str, desc: &str) {
    if !armed() {
        return;
    }
    let k = key(class, name, desc);
    let i = (k >> 13) as usize & (N - 1);
    // Idempotent: the common case is "already marked", one relaxed load.
    if MARKS[i].load(Relaxed) != k {
        MARKS[i].store(k, Relaxed);
    }
}

/// Has the native cascade ever answered this method in this process?
pub(crate) fn claimed(class: &str, name: &str, desc: &str) -> bool {
    if !armed() {
        return false;
    }
    let k = key(class, name, desc);
    let i = (k >> 13) as usize & (N - 1);
    MARKS[i].load(Relaxed) == k
}
