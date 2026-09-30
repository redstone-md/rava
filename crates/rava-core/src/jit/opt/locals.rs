//! Liveness of a method's local variables at each bytecode index (HotSpot's MethodLiveness): a
//! deoptimization state keeps only the locals the interpreter may still read from there, so
//! values the rest of the method never uses are not kept alive (in registers or home slots)
//! for deoptimization.
//!
//! Backward dataflow over instructions: live-in = uses + (live-out - defs), live-out the union
//! over successors, exception handlers of every range covering the instruction included (the
//! interpreter continuing after a deoptimization may throw there).

use crate::classfile::CodeInfo;
use crate::jit::analysis::{insn_len, successors};

/// Live-in locals of each instruction start, as bit sets of `words` words each.
pub struct LiveLocals {
    words: usize,
    bits: Vec<u64>,
}

impl LiveLocals {
    pub fn compute(info: &CodeInfo) -> LiveLocals {
        let code = info.code;
        let words = (info.max_locals as usize).div_ceil(64).max(1);
        let n = code.len();
        let mut starts = Vec::new();
        let mut pc = 0;
        while pc < n {
            starts.push(pc);
            pc += insn_len(code, pc);
        }
        let access: Vec<(Vec<usize>, Vec<usize>)> = (0..n).map(|pc| if starts.binary_search(&pc).is_ok() { uses_defs(code, pc) } else { (Vec::new(), Vec::new()) }).collect();
        let mut bits = vec![0u64; n * words];
        let mut out = vec![0u64; words];
        loop {
            let mut changed = false;
            for &pc in starts.iter().rev() {
                out.iter_mut().for_each(|w| *w = 0);
                let len = insn_len(code, pc);
                let succ = successors(code, pc, len);
                let handlers = info.handlers.iter().filter(|h| (h.start as usize..h.end as usize).contains(&pc)).map(|h| h.handler as usize);
                for s in succ.into_iter().chain(handlers).filter(|&s| s < n) {
                    for (o, i) in out.iter_mut().zip(&bits[s * words..(s + 1) * words]) {
                        *o |= i;
                    }
                }
                let (uses, defs) = &access[pc];
                for &d in defs {
                    out[d / 64] &= !(1 << (d % 64));
                }
                for &u in uses {
                    out[u / 64] |= 1 << (u % 64);
                }
                let row = &mut bits[pc * words..(pc + 1) * words];
                if row != &out[..] {
                    row.copy_from_slice(&out);
                    changed = true;
                }
            }
            if !changed {
                return LiveLocals { words, bits };
            }
        }
    }

    /// Whether local `slot` may be read at or after instruction `pc`.
    pub fn live(&self, pc: usize, slot: usize) -> bool {
        self.bits.get(pc * self.words + slot / 64).is_some_and(|w| w >> (slot % 64) & 1 == 1)
    }
}

/// The local slots the instruction at `pc` reads and writes (both halves of a long or double).
fn uses_defs(code: &[u8], pc: usize) -> (Vec<usize>, Vec<usize>) {
    let op = code[pc];
    let (op, index) = if op == 0xc4 {
        (code[pc + 1], u16::from_be_bytes([code[pc + 2], code[pc + 3]]) as usize)
    } else {
        (op, code.get(pc + 1).copied().unwrap_or(0) as usize)
    };
    let two = |i: usize| vec![i, i + 1];
    match op {
        0x15 | 0x17 | 0x19 => (vec![index], vec![]),
        0x16 | 0x18 => (two(index), vec![]),
        0x1a..=0x2d => {
            let (kind, n) = ((op - 0x1a) / 4, ((op - 0x1a) % 4) as usize);
            (if kind == 1 || kind == 3 { two(n) } else { vec![n] }, vec![])
        }
        0x36 | 0x38 | 0x3a => (vec![], vec![index]),
        0x37 | 0x39 => (vec![], two(index)),
        0x3b..=0x4e => {
            let (kind, n) = ((op - 0x3b) / 4, ((op - 0x3b) % 4) as usize);
            (vec![], if kind == 1 || kind == 3 { two(n) } else { vec![n] })
        }
        0x84 | 0xa9 => (vec![index], vec![]),
        _ => (vec![], vec![]),
    }
}
