//! WHAT IS A BYTECODE DISPATCH STYLE WORTH? — the experiment that decides a template interpreter.
//!
//! ⭐**WHY THIS AND NOT A PROTOTYPE.** A template interpreter is a rewrite. The case for it is
//! that a `match op { .. }` has ONE indirect branch site for the whole language while a template
//! interpreter re-dispatches at the end of every handler, giving each opcode its own site and its
//! own predictor entry. `DispatchVariety` showed rava's per-opcode cost rising 2.7x -> 8.7x with
//! opcode variety while HotSpot's falls, and `RAVA_PC_INSN` puts ~10% of `run_hot` on the two
//! instructions where the jump table lands. Both point at the dispatcher; neither says what
//! fixing it is WORTH.
//!
//! This prices it in isolation. The same synthetic opcode stream is run through dispatchers that
//! differ ONLY in how many distinct dispatch SITES they have — 1 (a plain loop, what rava does),
//! 2 and 4 (the match replicated, each copy ending by jumping into the next). Every handler does
//! identical trivial work, so the difference between them is the branch predictor and nothing
//! else. If 1 -> 4 sites buys little, threading buys little, and a rewrite is not the lever.
//!
//! ⚠Rust has no computed goto; replication is the honest approximation of it available here, and
//! it is a LOWER bound on threading (4 sites, not 200). Numbers are min-of-N on a loaded box.
use std::hint::black_box;
use std::time::Instant;

const N_OPS: usize = 24;

/// A pseudo-random but FIXED opcode stream, so every dispatcher sees identical work and identical
/// predictability. Seeded deterministically: `Math::random` is unavailable here by design.
fn stream(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut x: u32 = 0x1234_5678;
    for _ in 0..len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        v.push((x % N_OPS as u32) as u8);
    }
    v
}

/// Every handler is the same shape: touch two "operand" slots and advance. What matters is the
/// dispatch around it, so the body is deliberately uniform and cheap.
macro_rules! handlers {
    ($op:expr, $acc:expr, $st:expr, $sp:expr, $next:tt) => {
        match $op {
            0 => { $st[$sp & 7] = $acc; $sp = $sp.wrapping_add(1); $acc = $acc.wrapping_add(1); }
            1 => { $acc = $acc.wrapping_add($st[$sp & 7]); }
            2 => { $acc = $acc.wrapping_mul(3); }
            3 => { $acc ^= $st[($sp ^ 3) & 7]; }
            4 => { $sp = $sp.wrapping_sub(1); $acc = $acc.wrapping_sub(1); }
            5 => { $st[$sp & 7] = $acc ^ 0x5555; }
            6 => { $acc = $acc.rotate_left(3); }
            7 => { $acc = $acc.wrapping_add($st[($sp + 1) & 7]); }
            8 => { $acc |= 1; }
            9 => { $acc &= !2; }
            10 => { $acc = $acc.wrapping_shl(1); }
            11 => { $acc = $acc.wrapping_shr(1); }
            12 => { $st[($sp + 2) & 7] = $acc; }
            13 => { $acc = $acc.wrapping_add(7); }
            14 => { $acc ^= 0x0f0f; }
            15 => { $acc = $acc.wrapping_mul(5); }
            16 => { $sp = $sp.wrapping_add(2); }
            17 => { $acc = $acc.wrapping_sub($st[$sp & 7]); }
            18 => { $acc = $acc.rotate_right(5); }
            19 => { $st[($sp + 3) & 7] = $acc.wrapping_add(1); }
            20 => { $acc |= 0x100; }
            21 => { $acc = $acc.wrapping_add($st[($sp + 4) & 7]); }
            22 => { $acc ^= $sp as u64; }
            _ => { $acc = $acc.wrapping_add(2); }
        }
        $next
    };
}

/// ONE dispatch site — what rava's `run_hot` is.
fn sites1(bc: &[u8]) -> u64 {
    let (mut acc, mut sp, mut st) = (1u64, 0usize, [0u64; 8]);
    let mut i = 0usize;
    while i < bc.len() {
        let op = bc[i];
        i += 1;
        handlers!(op, acc, st, sp, {});
    }
    acc ^ sp as u64 ^ st[0]
}

/// TWO dispatch sites: each copy ends by dispatching into the other.
fn sites2(bc: &[u8]) -> u64 {
    let (mut acc, mut sp, mut st) = (1u64, 0usize, [0u64; 8]);
    let mut i = 0usize;
    'outer: loop {
        if i >= bc.len() { break }
        let op = bc[i];
        i += 1;
        handlers!(op, acc, st, sp, {});
        if i >= bc.len() { break 'outer }
        let op = bc[i];
        i += 1;
        handlers!(op, acc, st, sp, {});
    }
    acc ^ sp as u64 ^ st[0]
}

/// FOUR dispatch sites.
fn sites4(bc: &[u8]) -> u64 {
    let (mut acc, mut sp, mut st) = (1u64, 0usize, [0u64; 8]);
    let mut i = 0usize;
    loop {
        if i >= bc.len() { break }
        let op = bc[i]; i += 1; handlers!(op, acc, st, sp, {});
        if i >= bc.len() { break }
        let op = bc[i]; i += 1; handlers!(op, acc, st, sp, {});
        if i >= bc.len() { break }
        let op = bc[i]; i += 1; handlers!(op, acc, st, sp, {});
        if i >= bc.len() { break }
        let op = bc[i]; i += 1; handlers!(op, acc, st, sp, {});
    }
    acc ^ sp as u64 ^ st[0]
}

/// ⚠**The control must be a real STREAM, not a constant.** The first version pinned `op` to a
/// literal and LLVM folded the whole dispatch away — it reported **0.000 ns/op**, which is not a
/// fast interpreter, it is no interpreter. This one reads a genuinely predictable stream (a short
/// repeating cycle) through the SAME dispatcher and the SAME handlers, so the only difference
/// against `sites1` is how well the branch predictor does.
fn cyclic_stream(len: usize, period: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8) % period).collect()
}


/// ⭐**THREADED: ONE DISPATCH SITE PER OPCODE** — what a template interpreter gives you.
///
/// Generated, not hand-written: each opcode's arm ends with its OWN copy of the dispatch, so the
/// predictor sees `site(op_i) -> op_{i+1}` instead of one shared site seeing everything. That is
/// the whole mechanism a template interpreter buys, expressed in the only way stable Rust allows
/// (there is no computed goto, and no guaranteed tail call).
///
/// ⚠One level of replication, so the sites are per-FIRST-opcode of each pair. That is a LOWER
/// bound on true threading, and it is the level at which the per-opcode successor distribution
/// (measured at 22.9% top-1) actually applies.
#[allow(clippy::needless_range_loop)]
fn threaded(bc: &[u8]) -> u64 {
    let (mut acc, mut sp, mut st) = (1u64, 0usize, [0u64; 8]);
    let mut i = 0usize;
    let n = bc.len();
    loop {
        if i >= n { break }
        let op = bc[i]; i += 1;
        match op {
            0 => {
                st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            1 => {
                acc = acc.wrapping_add(st[sp & 7]);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            2 => {
                acc = acc.wrapping_mul(3);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            3 => {
                acc ^= st[(sp ^ 3) & 7];
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            4 => {
                sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            5 => {
                st[sp & 7] = acc ^ 0x5555;
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            6 => {
                acc = acc.rotate_left(3);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            7 => {
                acc = acc.wrapping_add(st[(sp + 1) & 7]);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            8 => {
                acc |= 1;
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            9 => {
                acc &= !2;
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            10 => {
                acc = acc.wrapping_shl(1);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            11 => {
                acc = acc.wrapping_shr(1);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            12 => {
                st[(sp + 2) & 7] = acc;
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            13 => {
                acc = acc.wrapping_add(7);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            14 => {
                acc ^= 0x0f0f;
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            15 => {
                acc = acc.wrapping_mul(5);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            16 => {
                sp = sp.wrapping_add(2);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            17 => {
                acc = acc.wrapping_sub(st[sp & 7]);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            18 => {
                acc = acc.rotate_right(5);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            19 => {
                st[(sp + 3) & 7] = acc.wrapping_add(1);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            20 => {
                acc |= 0x100;
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            21 => {
                acc = acc.wrapping_add(st[(sp + 4) & 7]);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            22 => {
                acc ^= sp as u64;
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            23 => {
                acc = acc.wrapping_add(2);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
            _ => {
                acc = acc.wrapping_add(11);
                if i >= n { break }
                let op2 = bc[i]; i += 1;
                match op2 {
                    0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = acc.wrapping_add(1); }
                    1 => { acc = acc.wrapping_add(st[sp & 7]); }
                    2 => { acc = acc.wrapping_mul(3); }
                    3 => { acc ^= st[(sp ^ 3) & 7]; }
                    4 => { sp = sp.wrapping_sub(1); acc = acc.wrapping_sub(1); }
                    5 => { st[sp & 7] = acc ^ 0x5555; }
                    6 => { acc = acc.rotate_left(3); }
                    7 => { acc = acc.wrapping_add(st[(sp + 1) & 7]); }
                    8 => { acc |= 1; }
                    9 => { acc &= !2; }
                    10 => { acc = acc.wrapping_shl(1); }
                    11 => { acc = acc.wrapping_shr(1); }
                    12 => { st[(sp + 2) & 7] = acc; }
                    13 => { acc = acc.wrapping_add(7); }
                    14 => { acc ^= 0x0f0f; }
                    15 => { acc = acc.wrapping_mul(5); }
                    16 => { sp = sp.wrapping_add(2); }
                    17 => { acc = acc.wrapping_sub(st[sp & 7]); }
                    18 => { acc = acc.rotate_right(5); }
                    19 => { st[(sp + 3) & 7] = acc.wrapping_add(1); }
                    20 => { acc |= 0x100; }
                    21 => { acc = acc.wrapping_add(st[(sp + 4) & 7]); }
                    22 => { acc ^= sp as u64; }
                    23 => { acc = acc.wrapping_add(2); }
                    _ => { acc = acc.wrapping_add(11); }
                }
            }
        }
    }
    acc ^ sp as u64 ^ st[0]
}


/// ⭐**WHAT DOES THE OPERAND REPRESENTATION COST?** — the question row 413 left standing.
///
/// Replaying the real stream showed dispatch at **2.27 ns/op** against rava's **~5.5**, so ~3.2
/// ns/op is HANDLER work. The handlers above use bare `u64` slots, which is free. rava's slots
/// are `Value`: a 16-byte tagged enum with an `Arc` variant, so **every local load is a `clone`
/// that branches on the tag, and every local store DROPS the old value** — another tag branch and
/// a possible refcount decrement. HotSpot's interpreter slot is an untyped word with neither.
///
/// This runs the SAME dispatcher and the SAME logical operations over slots of that shape, so the
/// difference against `sites1` is the representation and nothing else. It is the cheap way to
/// price a change to `Value` before making one — the engine has ~240 files that touch it.
#[derive(Clone)]
enum V {
    Int(i64),
    Long(i64),
    Double(f64),
    Str(std::sync::Arc<String>),
    Null,
}

impl V {
    #[inline]
    fn as_i(&self) -> u64 {
        match self {
            V::Int(i) | V::Long(i) => *i as u64,
            V::Double(d) => *d as u64,
            V::Str(_) => 1,
            V::Null => 0,
        }
    }
}

fn sites_value(bc: &[u8]) -> u64 {
    let mut acc = V::Int(1);
    let mut sp = 0usize;
    // One `Str` slot so the tag branch in `clone`/drop is REAL and not trivially predicted away.
    let mut st: [V; 8] = [
        V::Int(0), V::Int(0), V::Str(std::sync::Arc::new("x".to_string())), V::Int(0),
        V::Int(0), V::Int(0), V::Int(0), V::Int(0),
    ];
    let mut i = 0usize;
    while i < bc.len() {
        let op = bc[i];
        i += 1;
        match op {
            0 => { st[sp & 7] = acc.clone(); sp = sp.wrapping_add(1); acc = V::Int(acc.as_i().wrapping_add(1) as i64); }
            1 => { acc = V::Int(acc.as_i().wrapping_add(st[sp & 7].as_i()) as i64); }
            2 => { acc = V::Int(acc.as_i().wrapping_mul(3) as i64); }
            3 => { acc = V::Int((acc.as_i() ^ st[(sp ^ 3) & 7].as_i()) as i64); }
            4 => { sp = sp.wrapping_sub(1); acc = V::Int(acc.as_i().wrapping_sub(1) as i64); }
            5 => { st[sp & 7] = V::Int((acc.as_i() ^ 0x5555) as i64); }
            6 => { acc = V::Int(acc.as_i().rotate_left(3) as i64); }
            7 => { acc = V::Int(acc.as_i().wrapping_add(st[(sp + 1) & 7].as_i()) as i64); }
            8 => { acc = V::Int((acc.as_i() | 1) as i64); }
            9 => { acc = V::Int((acc.as_i() & !2) as i64); }
            10 => { acc = V::Int(acc.as_i().wrapping_shl(1) as i64); }
            11 => { acc = V::Int(acc.as_i().wrapping_shr(1) as i64); }
            12 => { st[(sp + 2) & 7] = acc.clone(); }
            13 => { acc = V::Int(acc.as_i().wrapping_add(7) as i64); }
            14 => { acc = V::Int((acc.as_i() ^ 0x0f0f) as i64); }
            15 => { acc = V::Int(acc.as_i().wrapping_mul(5) as i64); }
            16 => { sp = sp.wrapping_add(2); }
            17 => { acc = V::Int(acc.as_i().wrapping_sub(st[sp & 7].as_i()) as i64); }
            18 => { acc = V::Int(acc.as_i().rotate_right(5) as i64); }
            19 => { st[(sp + 3) & 7] = V::Int(acc.as_i().wrapping_add(1) as i64); }
            20 => { acc = V::Int((acc.as_i() | 0x100) as i64); }
            21 => { acc = V::Int(acc.as_i().wrapping_add(st[(sp + 4) & 7].as_i()) as i64); }
            22 => { acc = V::Int((acc.as_i() ^ sp as u64) as i64); }
            _ => { acc = V::Int(acc.as_i().wrapping_add(2) as i64); }
        }
    }
    acc.as_i() ^ sp as u64 ^ st[0].as_i()
}


/// ⭐**IS IT THE SIXTEEN BYTES, OR THE DROP GLUE?** — they need different fixes.
///
/// `Value`-shaped slots cost +1.47 ns/op over bare `u64`. Two candidates: the slot is 16 bytes
/// instead of 8 (so every push, pop and local load moves twice the memory), or the slot OWNS an
/// `Arc`, so `clone` branches on the tag and every overwrite runs drop glue. This variant is the
/// same 16-byte tagged enum with the `Arc` replaced by an interner index: identical SIZE, but
/// `Copy`, no clone branch, no drop.
///
/// If it recovers the whole 1.47 ns the fix is "get the owning variant out of `Value`" — large
/// but bounded. If it recovers little, only a narrower slot (8-byte tagged word / NaN-boxing)
/// would help, which is a different and much deeper change. Worth one benchmark either way.
#[derive(Clone, Copy)]
enum VC {
    Int(i64),
    Long(i64),
    Double(f64),
    Str(u32),
    Null,
}

impl VC {
    #[inline]
    fn as_i(&self) -> u64 {
        match self {
            VC::Int(i) | VC::Long(i) => *i as u64,
            VC::Double(d) => *d as u64,
            VC::Str(s) => *s as u64,
            VC::Null => 0,
        }
    }
}

fn sites_copyval(bc: &[u8]) -> u64 {
    let mut acc = VC::Int(1);
    let mut sp = 0usize;
    let mut st: [VC; 8] = [
        VC::Int(0), VC::Int(0), VC::Str(7), VC::Int(0),
        VC::Int(0), VC::Int(0), VC::Int(0), VC::Int(0),
    ];
    let mut i = 0usize;
    while i < bc.len() {
        let op = bc[i];
        i += 1;
        match op {
            0 => { st[sp & 7] = acc; sp = sp.wrapping_add(1); acc = VC::Int(acc.as_i().wrapping_add(1) as i64); }
            1 => { acc = VC::Int(acc.as_i().wrapping_add(st[sp & 7].as_i()) as i64); }
            2 => { acc = VC::Int(acc.as_i().wrapping_mul(3) as i64); }
            3 => { acc = VC::Int((acc.as_i() ^ st[(sp ^ 3) & 7].as_i()) as i64); }
            4 => { sp = sp.wrapping_sub(1); acc = VC::Int(acc.as_i().wrapping_sub(1) as i64); }
            5 => { st[sp & 7] = VC::Int((acc.as_i() ^ 0x5555) as i64); }
            6 => { acc = VC::Int(acc.as_i().rotate_left(3) as i64); }
            7 => { acc = VC::Int(acc.as_i().wrapping_add(st[(sp + 1) & 7].as_i()) as i64); }
            8 => { acc = VC::Int((acc.as_i() | 1) as i64); }
            9 => { acc = VC::Int((acc.as_i() & !2) as i64); }
            10 => { acc = VC::Int(acc.as_i().wrapping_shl(1) as i64); }
            11 => { acc = VC::Int(acc.as_i().wrapping_shr(1) as i64); }
            12 => { st[(sp + 2) & 7] = acc; }
            13 => { acc = VC::Int(acc.as_i().wrapping_add(7) as i64); }
            14 => { acc = VC::Int((acc.as_i() ^ 0x0f0f) as i64); }
            15 => { acc = VC::Int(acc.as_i().wrapping_mul(5) as i64); }
            16 => { sp = sp.wrapping_add(2); }
            17 => { acc = VC::Int(acc.as_i().wrapping_sub(st[sp & 7].as_i()) as i64); }
            18 => { acc = VC::Int(acc.as_i().rotate_right(5) as i64); }
            19 => { st[(sp + 3) & 7] = VC::Int(acc.as_i().wrapping_add(1) as i64); }
            20 => { acc = VC::Int((acc.as_i() | 0x100) as i64); }
            21 => { acc = VC::Int(acc.as_i().wrapping_add(st[(sp + 4) & 7].as_i()) as i64); }
            22 => { acc = VC::Int((acc.as_i() ^ sp as u64) as i64); }
            _ => { acc = VC::Int(acc.as_i().wrapping_add(2) as i64); }
        }
    }
    acc.as_i() ^ sp as u64 ^ st[0].as_i()
}

fn bench(name: &str, bc: &[u8], reps: usize, f: fn(&[u8]) -> u64) {
    let mut best = f64::MAX;
    let mut all = Vec::new();
    for _ in 0..reps {
        let t = Instant::now();
        black_box(f(black_box(bc)));
        let ns = t.elapsed().as_nanos() as f64 / bc.len() as f64;
        all.push(ns);
        if ns < best { best = ns }
    }
    let sp: Vec<String> = all.iter().map(|v| format!("{v:.3}")).collect();
    println!("{name:<26} {best:>7.3} ns/op   spread {}", sp.join("/"));
}

fn main() {
    // With a path argument, replay a REAL executed-opcode stream (`RAVA_OPDUMP`), remapped to a
    // dense set of its top opcodes so the generated dispatchers can index it. Without one, fall
    // back to the synthetic random stream — which is the worst case, not the representative one.
    let arg = std::env::args().nth(1);
    let bc = match arg {
        Some(path) => {
            let raw = std::fs::read(&path).expect("opcode dump");
            let mut freq = [0u64; 256];
            for &b in &raw { freq[b as usize] += 1 }
            let mut order: Vec<usize> = (0..256).collect();
            order.sort_by_key(|&i| std::cmp::Reverse(freq[i]));
            let mut map = [N_OPS as u8; 256];
            for (rank, &opc) in order.iter().take(N_OPS).enumerate() {
                map[opc] = rank as u8;
            }
            let covered: u64 = order.iter().take(N_OPS).map(|&i| freq[i]).sum();
            let total: u64 = freq.iter().sum();
            println!(
                "real stream: {} opcodes, top {N_OPS} distinct cover {:.1}%",
                raw.len(),
                covered as f64 * 100.0 / total as f64
            );
            raw.into_iter().map(|b| map[b as usize]).collect::<Vec<u8>>()
        }
        None => stream(40_000_000),
    };
    let reps = 5;
    println!("dispatch styles over an identical {} -opcode stream, {N_OPS} distinct opcodes", bc.len());
    bench("1 site (rava's shape)", &bc, reps, sites1);
    bench("2 sites", &bc, reps, sites2);
    bench("4 sites", &bc, reps, sites4);
    bench("THREADED (per-opcode)", &bc, reps, threaded);
    bench("1 site, Value-shaped slots", &bc, reps, sites_value);
    bench("1 site, 16B Copy slots", &bc, reps, sites_copyval);
    // Same dispatcher, same handlers, streams of decreasing predictability. The gap between the
    // 4-cycle stream and the random one is the ENTIRE prize any dispatcher redesign competes for.
    let cyc4 = cyclic_stream(bc.len(), 4);
    let cyc8 = cyclic_stream(bc.len(), 8);
    bench("1 site, 4-cycle stream", &cyc4, reps, sites1);
    bench("1 site, 8-cycle stream", &cyc8, reps, sites1);
}
