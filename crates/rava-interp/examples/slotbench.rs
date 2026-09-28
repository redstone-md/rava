//! WHAT DOES THE 32-BYTE OPERAND SLOT COST? Three loops, one opcode stream, no VM.
//!
//! WHY. Ledger row 385 measured the interpreter's ceiling: on `ArithLoop`, where 99,764 opcodes
//! run per hot-loop entry and there are no calls, no exits and no allocation, rava does
//! **~7.0 ns/opcode against `java -Xint`'s ~1.0 ns**. Every bookkeeping suspect around it is closed
//! (rows 378-384), so what remains is the loop: a Rust `match` over a **32-byte `Value` in memory**
//! versus hand-written stubs with the operand-stack top **in registers**.
//!
//! Closing that means a value-model rewrite — `docs/value-model-rewrite-plan.md`, explicitly
//! "multi-session/multi-year". This tree's rule before machinery of that size is to COUNT FIRST
//! (the λ-PIC's `ADMITTED = 0`, the SETTER row's 2,089), so this prices the change before anyone
//! commits to it:
//!
//!   `enum32`  operand stack of the REAL 32-byte `Value`      — what we do today
//!   `word8`   operand stack of raw `i64` slots               — what a tagged-word model would do
//!   `word8_tos` same, with the top of stack kept in a LOCAL  — plus top-of-stack caching
//!   `enum16`  the SAME enum with a THIN string pointer          — the cheap intermediate step
//!
//! `enum16` exists because the rewrite plan prices only the endpoint. `Value` is 32 bytes because
//! `Str(Arc<str>)` is a FAT pointer (16 bytes) and the payload then needs 24; making it
//! `Arc<String>` — a THIN pointer — drops the payload to 8 and the whole enum to 16. That costs one
//! extra indirection per string read and touches the ~500 construction sites the plan names, but it
//! is MECHANICAL, unlike a tagged word. If 16 captures most of the 8-byte win, that is a bounded
//! project; if it captures little, only the full rewrite is worth anything and nobody should spend
//! a session finding that out by doing it.
//!
//! All three run the SAME opcode stream through the SAME `match`, so the difference is the slot
//! representation and nothing else.
//!
//! ⚠ **This is a MICROBENCH and it is bounded as one.** It cannot say what the boot would gain —
//! this tree has a row for exactly that mistake (`rava-arena-maps`: "~7-8× faster" in a microbench,
//! **+48% on a real boot**). What it CAN say is whether the representation is worth an arc at all:
//! if `word8` is level with `enum32`, the rewrite is dead on arrival and nobody needs to spend a
//! session discovering that.
//!
//! ⛔**THE FIRST VERSION MEASURED NOTHING AND SAID SO LOUDLY**: it reported **0.01 ns/opcode**,
//! which is a third of a cycle — impossible. The opcode stream and the initial locals were
//! compile-time constants, so LLVM folded all three loops to their answers. Every arm now reads its
//! stream through `black_box` and feeds the previous iteration's result back into the locals, so
//! the work is data-dependent and cannot be hoisted. ⭐**A microbench result that is physically
//! impossible is the easy case; the dangerous one is a plausible number from a folded loop.**
//!
//!   cargo run --release -p rava-interp --features next-gen-gc --example slotbench

use std::hint::black_box;
use std::time::Instant;

/// A stand-in for the real `Value`: same size and alignment, same discriminated shape, so the
/// generated load/store sequence matches what the interpreter actually emits.
#[derive(Clone, Copy)]
#[repr(align(32))]
#[allow(dead_code)]
enum Slot32 {
    Int(i32),
    Long(i64),
    Double(f64),
    /// The fat-pointer variant is what forces 24 bytes of payload before alignment rounds it to 32.
    Str(*const u8, usize),
    Null,
}

/// The same discriminated enum with a THIN string pointer: payload 8 bytes, so 16 total.
#[derive(Clone, Copy)]
#[repr(align(16))]
#[allow(dead_code)]
enum Slot16 {
    Int(i32),
    Long(i64),
    Double(f64),
    /// A thin pointer — this is the whole difference from `Slot32`.
    Str(*const String),
    Null,
}

/// The opcode stream. Deliberately the shape `ArithLoop` produces: load a local, load a local,
/// combine, store back — the sequence that makes an interpreter's operand traffic visible.
const OPS: [u8; 8] = [
    0x1a, // iload_0
    0x1b, // iload_1
    0x60, // iadd
    0x3b, // istore_0
    0x1a, // iload_0
    0x10, // bipush
    0x7e, // iand
    0x3b, // istore_0
];

fn run_enum32(iters: usize, seed: i32) -> i64 {
    let mut stack: Vec<Slot32> = Vec::with_capacity(16);
    let mut locals: [Slot32; 4] =
        [Slot32::Int(black_box(seed)), Slot32::Int(black_box(3)), Slot32::Null, Slot32::Null];
    for _ in 0..iters {
        // Opaque stream: the optimiser may not assume which opcodes run.
        for &op in black_box(&OPS[..]).iter() {
            match op {
                0x1a | 0x1b => {
                    let i = (op - 0x1a) as usize;
                    stack.push(locals[i]);
                }
                0x10 => stack.push(Slot32::Int(0x7f)),
                0x60 | 0x7e => {
                    let b = match stack.pop() {
                        Some(Slot32::Int(v)) => v,
                        _ => 0,
                    };
                    let a = match stack.pop() {
                        Some(Slot32::Int(v)) => v,
                        _ => 0,
                    };
                    stack.push(Slot32::Int(if op == 0x60 { a.wrapping_add(b) } else { a & b }));
                }
                0x3b => {
                    let v = stack.pop().unwrap_or(Slot32::Null);
                    locals[0] = v;
                }
                _ => {}
            }
        }
    }
    match locals[0] {
        Slot32::Int(v) => v as i64,
        _ => 0,
    }
}

fn run_enum16(iters: usize, seed: i32) -> i64 {
    let mut stack: Vec<Slot16> = Vec::with_capacity(16);
    let mut locals: [Slot16; 4] =
        [Slot16::Int(black_box(seed)), Slot16::Int(black_box(3)), Slot16::Null, Slot16::Null];
    for _ in 0..iters {
        for &op in black_box(&OPS[..]).iter() {
            match op {
                0x1a | 0x1b => {
                    let i = (op - 0x1a) as usize;
                    stack.push(locals[i]);
                }
                0x10 => stack.push(Slot16::Int(0x7f)),
                0x60 | 0x7e => {
                    let b = match stack.pop() {
                        Some(Slot16::Int(v)) => v,
                        _ => 0,
                    };
                    let a = match stack.pop() {
                        Some(Slot16::Int(v)) => v,
                        _ => 0,
                    };
                    stack.push(Slot16::Int(if op == 0x60 { a.wrapping_add(b) } else { a & b }));
                }
                0x3b => {
                    let v = stack.pop().unwrap_or(Slot16::Null);
                    locals[0] = v;
                }
                _ => {}
            }
        }
    }
    match locals[0] {
        Slot16::Int(v) => v as i64,
        _ => 0,
    }
}

fn run_word8(iters: usize, seed: i32) -> i64 {
    // A tagged word: the payload in the low bits, exactly what the rewrite plan's `Value = i64`
    // describes. This bench only needs ints, so the tag is carried but never branched on — which
    // is the point: a word-sized slot is MOVED, not decoded.
    let mut stack: Vec<i64> = Vec::with_capacity(16);
    let mut locals: [i64; 4] = [black_box(seed) as i64, black_box(3), 0, 0];
    for _ in 0..iters {
        for &op in black_box(&OPS[..]).iter() {
            match op {
                0x1a | 0x1b => {
                    let i = (op - 0x1a) as usize;
                    stack.push(locals[i]);
                }
                0x10 => stack.push(0x7f),
                0x60 | 0x7e => {
                    let b = stack.pop().unwrap_or(0);
                    let a = stack.pop().unwrap_or(0);
                    stack.push(if op == 0x60 { a.wrapping_add(b) } else { a & b });
                }
                0x3b => {
                    locals[0] = stack.pop().unwrap_or(0);
                }
                _ => {}
            }
        }
    }
    locals[0]
}

fn run_word8_tos(iters: usize, seed: i32) -> i64 {
    // Top-of-stack caching: the topmost operand lives in `tos`, a plain local the optimiser can
    // keep in a register, and only the slots BELOW it touch memory. This is what HotSpot's
    // template interpreter does and what a 32-byte slot makes impossible.
    let mut stack: Vec<i64> = Vec::with_capacity(16);
    let mut locals: [i64; 4] = [black_box(seed) as i64, black_box(3), 0, 0];
    let mut tos: i64 = 0;
    let mut has_tos = false;
    for _ in 0..iters {
        for &op in black_box(&OPS[..]).iter() {
            match op {
                0x1a | 0x1b => {
                    let i = (op - 0x1a) as usize;
                    if has_tos {
                        stack.push(tos);
                    }
                    tos = locals[i];
                    has_tos = true;
                }
                0x10 => {
                    if has_tos {
                        stack.push(tos);
                    }
                    tos = 0x7f;
                    has_tos = true;
                }
                0x60 | 0x7e => {
                    let b = tos;
                    let a = stack.pop().unwrap_or(0);
                    tos = if op == 0x60 { a.wrapping_add(b) } else { a & b };
                }
                0x3b => {
                    locals[0] = tos;
                    tos = stack.pop().unwrap_or(0);
                    has_tos = false;
                }
                _ => {}
            }
        }
    }
    locals[0]
}

fn main() {
    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20_000_000);
    let ops = iters * OPS.len();
    println!("{iters} iterations x {} ops = {ops} opcodes per arm", OPS.len());
    println!("{:<12} {:>10} {:>14} {:>12}", "arm", "ms", "ns/opcode", "vs enum32");

    // THREE passes, keep the minimum. Two was not enough: at two passes this bench reported
    // `word8_tos` as a 7% WIN, and at three repeats it is a consistent LOSS in every run (row 387).
    // The arms differ by ~10% and this box drifts by ~40% between runs, so a single pass ranks
    // noise. Same min-of-N discipline as every A/B in this tree, for the same reason.
    let mut best: Vec<(&str, f64)> = Vec::new();
    for _ in 0..3 {
        for (name, f) in [
            ("enum32", run_enum32 as fn(usize, i32) -> i64),
            ("enum16", run_enum16),
            ("word8", run_word8),
            ("word8_tos", run_word8_tos),
        ] {
            let t0 = Instant::now();
            let r = f(iters, black_box(1));
            let ms = t0.elapsed().as_secs_f64() * 1e3;
            black_box(r);
            match best.iter_mut().find(|(k, _)| *k == name) {
                Some((_, v)) if *v <= ms => {}
                Some((_, v)) => *v = ms,
                None => best.push((name, ms)),
            }
        }
    }
    let base = best.iter().find(|(k, _)| *k == "enum32").map(|(_, v)| *v).unwrap_or(1.0);
    for (name, ms) in &best {
        println!(
            "{name:<12} {ms:>10.1} {:>14.2} {:>11.2}x",
            ms * 1e6 / ops as f64,
            base / ms
        );
    }
    println!();
    println!();
    println!("expected shape (row 387, min-of-3): enum32 1.00x, enum16 ~1.3x, word8 ~2.1x,");
    println!("word8_tos BELOW word8 - top-of-stack caching is a REGRESSION here, not a win.");
    println!("row 385: the real interpreter is ~7.0 ns/opcode against java -Xint's ~1.0");
    println!("a microbench cannot predict the boot - see rava-arena-maps (~7-8x here, +48% there)");
}
