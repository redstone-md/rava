//! WHY DOES THE COMPILER REFUSE 89% OF WHAT IT IS OFFERED? (`RAVA_COMPILE_FAIL_CENSUS=1`,
//! default OFF, counts only.)
//!
//! A product boot's own log says `attempts=3482 compile-failed=3105 COMPILED=354`, and the host
//! profile says **89.9% of samples are in the Rust host, ~10% in compiled guest code** — against
//! a JVM that runs essentially all of this program compiled. A method whose compile fails is
//! cached `Failed` and runs interpreted for the rest of the process, so the refusal set IS the
//! host share.
//!
//! The tree had no instrument for it: `compile_failed` was a bare counter with no reason
//! attached, and the emitter's refusals are `return None` at dozens of sites. Rather than trust a
//! reading of the compiler, this compares POPULATIONS — the distinct opcodes present in methods
//! that failed against those in methods that compiled. An opcode that appears in nearly every
//! failure and almost no success is the refusal, whatever line of the emitter spells it.
//!
//! No string is built at note time; the histogram is 256 counters per outcome.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

static OK: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];
static FAIL: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];
static N_OK: AtomicU64 = AtomicU64::new(0);
static N_FAIL: AtomicU64 = AtomicU64::new(0);
/// Bytecode length buckets, to separate "too big" from "unsupported shape".
static LEN_OK: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
static LEN_FAIL: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

#[inline]
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_COMPILE_FAIL_CENSUS").is_some())
}

fn len_bucket(n: usize) -> usize {
    match n {
        0..=15 => 0,
        16..=63 => 1,
        64..=255 => 2,
        256..=511 => 3,
        512..=1023 => 4,
        1024..=2047 => 5,
        2048..=4095 => 6,
        _ => 7,
    }
}

/// Mark every DISTINCT opcode present in `bc`. Distinct, not total: the question is which
/// opcodes a method must contain to be refused, and a loop that repeats one opcode 400 times
/// would otherwise drown the shape out.
pub(crate) fn note(ok: bool, bc: &[u8]) {
    if !on() {
        return;
    }
    let (tab, n, lens) = if ok {
        (&OK, &N_OK, &LEN_OK)
    } else {
        (&FAIL, &N_FAIL, &LEN_FAIL)
    };
    n.fetch_add(1, Relaxed);
    lens[len_bucket(bc.len())].fetch_add(1, Relaxed);
    let mut seen = [false; 256];
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        if !seen[op as usize] {
            seen[op as usize] = true;
            tab[op as usize].fetch_add(1, Relaxed);
        }
        match crate::jit::bytecode::insn_len_at(bc, pc) {
            Some(l) if l > 0 => pc += l,
            // Undecodable from here: stop rather than walk garbage. The opcodes already
            // marked are still true of this method.
            _ => break,
        }
    }
}

pub fn report() {
    if !on() {
        return;
    }
    let (no, nf) = (N_OK.load(Relaxed), N_FAIL.load(Relaxed));
    if no + nf == 0 {
        return;
    }
    eprintln!("[CFAIL] compiled={no} failed={nf} ({:.1}% refused)", nf as f64 * 100.0 / (no + nf) as f64);
    let names = |b: &[AtomicU64; 8]| -> String {
        b.iter().map(|a| a.load(Relaxed).to_string()).collect::<Vec<_>>().join("/")
    };
    eprintln!("[CFAIL] bytecode len <16/64/256/512/1k/2k/4k/more  ok={}", names(&LEN_OK));
    eprintln!("[CFAIL] bytecode len <16/64/256/512/1k/2k/4k/more fail={}", names(&LEN_FAIL));
    // Rank by how much MORE often an opcode appears in a failure than in a success.
    let mut rows: Vec<(u8, f64, f64, f64)> = Vec::new();
    for op in 0..256usize {
        let f = FAIL[op].load(Relaxed) as f64;
        let o = OK[op].load(Relaxed) as f64;
        if f == 0.0 {
            continue;
        }
        let fp = if nf > 0 { f * 100.0 / nf as f64 } else { 0.0 };
        let op_ = if no > 0 { o * 100.0 / no as f64 } else { 0.0 };
        rows.push((op as u8, fp, op_, fp - op_));
    }
    rows.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    eprintln!("[CFAIL] opcode  in-failures%  in-compiled%  excess  (top 24 by excess)");
    for (op, fp, op_, d) in rows.into_iter().take(24) {
        eprintln!("[CFAIL]   0x{op:02x}  {fp:11.1}  {op_:12.1}  {d:+6.1}");
    }
}
