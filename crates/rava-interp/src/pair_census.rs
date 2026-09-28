//! `RAVA_PAIR_CENSUS` — which ADJACENT OPCODE PAIRS does the interpreter actually execute?
//!
//! ⭐**WHY.** Row 410 closed the dispatch loop's HEAD: of five candidates only one paid, and what
//! remains inside `run_hot` — still 44% of an interpreted run — is the opcode work itself. The
//! standard lever there is a SUPERINSTRUCTION: give a frequently-adjacent pair one arm, so the pair
//! costs one dispatch instead of two. Halving the dispatch count for a common pair is worth real
//! percent on a loop whose per-opcode overhead is now mostly the dispatch itself.
//!
//! ⚠**But which pairs is an empirical question, and this ledger has been wrong three times in a row
//! guessing populations** (rows 403, 406: a first-blocker histogram overstated every candidate it
//! named). So count first, and count what a fused arm would actually SEE.
//!
//! ⚠**CONSECUTIVELY EXECUTED, WHICH IS NOT QUITE THE SAME AS ADJACENT IN BYTECODE.** A backward
//! branch makes the branch opcode and the loop head look consecutive when they are not neighbours
//! in the body. A fused arm re-checks the second opcode at its own `pc` offset, so it simply never
//! fires on those; they inflate a pair's count without invalidating it as a candidate. Stated here
//! rather than silently corrected, because the alternative — a full opcode-length table — is the
//! kind of second implementation this tree keeps paying for.
//!
//! Inert unless the variable is set: one local bool in the dispatch loop, read once per entry.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

/// 256 x 256 pairs. Half a megabyte of statics for a diagnostic that is off by default; the
/// alternative (a map) would put a hash on the per-opcode path, which row 409 just measured the
/// price of.
static PAIRS: [AtomicU64; 65536] = [const { AtomicU64::new(0) }; 65536];
static TOTAL: AtomicU64 = AtomicU64::new(0);

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_PAIR_CENSUS").is_some())
}

/// Count one executed pair. The caller gates on a hoisted bool — see `run_hot`.
#[inline]
pub fn note(prev: u8, cur: u8) {
    PAIRS[(prev as usize) << 8 | cur as usize].fetch_add(1, Relaxed);
    TOTAL.fetch_add(1, Relaxed);
}

pub fn report() -> String {
    if !enabled() {
        return String::new();
    }
    let total = TOTAL.load(Relaxed);
    if total == 0 {
        return String::new();
    }
    let mut rows: Vec<(usize, u64)> = (0..65536)
        .map(|i| (i, PAIRS[i].load(Relaxed)))
        .filter(|&(_, n)| n > 0)
        .collect();
    rows.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    let mut out = format!(
        "[PAIR-CENSUS] {total} executed opcode pairs, {} distinct; top 30\n",
        rows.len()
    );
    let mut cum = 0.0;
    for (k, n) in rows.iter().take(30) {
        let (a, b) = ((k >> 8) as u8, (k & 0xff) as u8);
        let pct = *n as f64 * 100.0 / total as f64;
        cum += pct;
        out.push_str(&format!(
            "  0x{a:02X} {:<14} -> 0x{b:02X} {:<14} {n:>14} {pct:>6.2}%  (cum {cum:>5.2}%)\n",
            name(a),
            name(b)
        ));
    }
    // ⭐**PER-OPCODE SUCCESSOR CONCENTRATION — the number that decides threaded dispatch.**
    //
    // A flat GLOBAL pair distribution (top pair 6.90%) says superinstructions are weak, and row
    // 411 measured that. It says nothing about THREADED DISPATCH, which does not fuse pairs: it
    // gives each opcode its OWN indirect-branch site, so what matters is how concentrated that
    // ONE opcode's successors are. Those are different questions and conflating them would pick
    // the wrong lever.
    //
    // If a hot opcode's top successor covers most of its executions, a per-opcode branch site
    // predicts it and threaded dispatch pays. If its successors are spread, the site mispredicts
    // exactly as the shared one does and threaded dispatch buys nothing.
    let mut by_first: Vec<(u8, u64, Vec<(u8, u64)>)> = Vec::new();
    for a in 0..256usize {
        let mut succ: Vec<(u8, u64)> = (0..256)
            .map(|b| (b as u8, PAIRS[(a << 8) | b].load(Relaxed)))
            .filter(|&(_, n)| n > 0)
            .collect();
        let tot: u64 = succ.iter().map(|(_, n)| *n).sum();
        if tot == 0 {
            continue;
        }
        succ.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        by_first.push((a as u8, tot, succ));
    }
    by_first.sort_by_key(|&(_, t, _)| std::cmp::Reverse(t));
    out.push_str(
        "[PAIR-CENSUS] per-opcode SUCCESSOR concentration (decides threaded dispatch, not fusion)
",
    );
    let mut w_top1 = 0.0f64;
    let mut w_top2 = 0.0f64;
    for (a, tot, succ) in by_first.iter().take(16) {
        let share = *tot as f64 * 100.0 / total as f64;
        let t1 = succ[0].1 as f64 * 100.0 / *tot as f64;
        let t2 = t1 + succ.get(1).map_or(0.0, |x| x.1 as f64 * 100.0 / *tot as f64);
        w_top1 += t1 * share / 100.0;
        w_top2 += t2 * share / 100.0;
        out.push_str(&format!(
            "  0x{a:02X} {:<14} {share:>6.2}% of all  | {} distinct succ | top1 {t1:>5.1}% (0x{:02X} {}) | top2 {t2:>5.1}%
",
            name(*a),
            succ.len(),
            succ[0].0,
            name(succ[0].0)
        ));
    }
    out.push_str(&format!(
        "  ⇒ traffic-weighted over these: top-1 successor {w_top1:.1}%, top-2 {w_top2:.1}%
"
    ));
    out
}

/// Short names for the opcodes this census is likely to rank. An unknown code prints as `?`
/// rather than a guess — a wrong name on a table used to choose what to implement is worse than
/// no name.
fn name(op: u8) -> &'static str {
    match op {
        0x00 => "nop", 0x01 => "aconst_null",
        0x02..=0x08 => "iconst_<i>", 0x09 | 0x0a => "lconst_<l>",
        0x0b..=0x0d => "fconst_<f>", 0x0e | 0x0f => "dconst_<d>",
        0x10 => "bipush", 0x11 => "sipush", 0x12 => "ldc", 0x13 => "ldc_w", 0x14 => "ldc2_w",
        0x15 => "iload", 0x16 => "lload", 0x17 => "fload", 0x18 => "dload", 0x19 => "aload",
        0x1a..=0x1d => "iload_<n>", 0x1e..=0x21 => "lload_<n>", 0x22..=0x25 => "fload_<n>",
        0x26..=0x29 => "dload_<n>", 0x2a..=0x2d => "aload_<n>",
        0x2e => "iaload", 0x2f => "laload", 0x30 => "faload", 0x31 => "daload",
        0x32 => "aaload", 0x33 => "baload", 0x34 => "caload", 0x35 => "saload",
        0x36 => "istore", 0x3a => "astore",
        0x3b..=0x3e => "istore_<n>", 0x3f..=0x42 => "lstore_<n>",
        0x43..=0x46 => "fstore_<n>", 0x47..=0x4a => "dstore_<n>", 0x4b..=0x4e => "astore_<n>",
        0x4f..=0x56 => "<t>astore",
        0x57 => "pop", 0x59 => "dup", 0x5a => "dup_x1", 0x5c => "dup2",
        0x60 => "iadd", 0x61 => "ladd", 0x62 => "fadd", 0x63 => "dadd",
        0x64 => "isub", 0x65 => "lsub", 0x66 => "fsub", 0x67 => "dsub",
        0x68 => "imul", 0x69 => "lmul", 0x6a => "fmul", 0x6b => "dmul",
        0x6c => "idiv", 0x6f => "ddiv", 0x70 => "irem",
        0x74 => "ineg", 0x78 => "ishl", 0x79 => "lshl", 0x7a => "ishr", 0x7c => "iushr",
        0x7e => "iand", 0x80 => "ior", 0x82 => "ixor", 0x84 => "iinc",
        0x85..=0x93 => "<t>2<t>", 0x94 => "lcmp", 0x95 | 0x96 => "fcmp", 0x97 | 0x98 => "dcmp",
        0x99..=0x9e => "if<cond>", 0x9f..=0xa4 => "if_icmp<c>", 0xa5 | 0xa6 => "if_acmp",
        0xa7 => "goto", 0xaa => "tableswitch", 0xab => "lookupswitch",
        0xac..=0xb0 => "<t>return", 0xb1 => "return",
        0xb2 => "getstatic", 0xb3 => "putstatic", 0xb4 => "getfield", 0xb5 => "putfield",
        0xb6 => "invokevirtual", 0xb7 => "invokespecial", 0xb8 => "invokestatic",
        0xb9 => "invokeiface", 0xba => "invokedynamic",
        0xbb => "new", 0xbc => "newarray", 0xbd => "anewarray", 0xbe => "arraylength",
        0xbf => "athrow", 0xc0 => "checkcast", 0xc1 => "instanceof",
        0xc6 | 0xc7 => "ifnull/nn", 0xc8 => "goto_w",
        _ => "?",
    }
}
