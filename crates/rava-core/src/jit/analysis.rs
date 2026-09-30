//! Pre-pass for the baseline compiler: instruction lengths, the operand-stack depth before
//! every reachable instruction, and basic-block leaders. Verified class files have one depth per
//! instruction; a conflict (or an unsupported instruction) makes the method uncompilable.

use crate::classfile::{ClassFile, Cp};
use crate::method::Method;
use crate::types::{BasicType, MethodSig};

pub struct Analysis {
    /// Operand-stack depth (slots) before each instruction; `None` if unreachable.
    pub depth: Vec<Option<u16>>,
    /// Instruction starts that begin a basic block.
    pub leader: Vec<bool>,
}

/// Length in bytes of the instruction at `pc`.
pub fn insn_len(code: &[u8], pc: usize) -> usize {
    match code[pc] {
        0x10 | 0x12 | 0x15..=0x19 | 0x36..=0x3a | 0xa9 | 0xbc => 2,
        0x11 | 0x13 | 0x14 | 0x84 | 0x99..=0xa8 | 0xb2..=0xb8 | 0xbb | 0xbd | 0xc0 | 0xc1 | 0xc6 | 0xc7 => 3,
        0xc5 => 4,
        0xb9 | 0xba | 0xc8 | 0xc9 => 5,
        0xc4 => if code[pc + 1] == 0x84 { 6 } else { 4 },
        0xaa => {
            let p = (pc + 4) & !3;
            let (low, high) = (i4(code, p + 4), i4(code, p + 8));
            p + 12 + 4 * (high - low + 1) as usize - pc
        }
        0xab => {
            let p = (pc + 4) & !3;
            p + 8 + 8 * i4(code, p + 4) as usize - pc
        }
        _ => 1,
    }
}

pub fn i4(code: &[u8], at: usize) -> i32 {
    i32::from_be_bytes([code[at], code[at + 1], code[at + 2], code[at + 3]])
}

pub fn i2(code: &[u8], at: usize) -> i16 {
    i16::from_be_bytes([code[at], code[at + 1]])
}

fn field_slots(cf: &ClassFile, index: u16) -> Option<i32> {
    let (_, _, desc) = cf.member_ref(index)?;
    Some(BasicType::from_desc(desc.as_str().as_bytes()[0]).slots() as i32)
}

/// Stack slots (popped, pushed) by the instruction at `pc`.
fn effect(cf: &ClassFile, code: &[u8], pc: usize) -> Option<(i32, i32)> {
    let op = code[pc];
    let u2 = || u16::from_be_bytes([code[pc + 1], code[pc + 2]]);
    Some(match op {
        0x00 => (0, 0),
        0x01..=0x08 | 0x0b..=0x0d | 0x10 | 0x11 => (0, 1),
        0x09 | 0x0a | 0x0e | 0x0f => (0, 2),
        0x12 | 0x13 => (0, match cf.pool.get(if op == 0x12 { code[pc + 1] as usize } else { u2() as usize })? {
            Cp::Dynamic { nat, .. } => cf.name_and_type(*nat).map(|(_, d)| BasicType::from_desc(d.as_str().as_bytes()[0]).slots() as i32)?,
            _ => 1,
        }),
        0x14 => (0, 2),
        0x15 | 0x17 | 0x19 | 0x1a..=0x1d | 0x22..=0x25 | 0x2a..=0x2d => (0, 1),
        0x16 | 0x18 | 0x1e..=0x21 | 0x26..=0x29 => (0, 2),
        0x2e | 0x30 | 0x32..=0x35 => (2, 1),
        0x2f | 0x31 => (2, 2),
        0x36 | 0x38 | 0x3a | 0x3b..=0x3e | 0x43..=0x46 | 0x4b..=0x4e => (1, 0),
        0x37 | 0x39 | 0x3f..=0x42 | 0x47..=0x4a => (2, 0),
        0x4f | 0x51 | 0x53..=0x56 => (3, 0),
        0x50 | 0x52 => (4, 0),
        0x57 => (1, 0),
        0x58 => (2, 0),
        0x59 => (1, 2),
        0x5a => (2, 3),
        0x5b => (3, 4),
        0x5c => (2, 4),
        0x5d => (3, 5),
        0x5e => (4, 6),
        0x5f => (2, 2),
        0x60 | 0x62 | 0x64 | 0x66 | 0x68 | 0x6a | 0x6c | 0x6e | 0x70 | 0x72 | 0x78 | 0x7a | 0x7c | 0x7e | 0x80 | 0x82 => (2, 1),
        0x61 | 0x63 | 0x65 | 0x67 | 0x69 | 0x6b | 0x6d | 0x6f | 0x71 | 0x73 | 0x7f | 0x81 | 0x83 => (4, 2),
        0x79 | 0x7b | 0x7d => (3, 2),
        0x74 | 0x76 => (1, 1),
        0x75 | 0x77 => (2, 2),
        0x84 => (0, 0),
        0x85 | 0x87 => (1, 2),
        0x86 | 0x8b | 0x91..=0x93 => (1, 1),
        0x88 | 0x89 | 0x8e | 0x90 => (2, 1),
        0x8a | 0x8f => (2, 2),
        0x8c | 0x8d => (1, 2),
        0x94 | 0x97 | 0x98 => (4, 1),
        0x95 | 0x96 => (2, 1),
        0x99..=0x9e | 0xc6 | 0xc7 => (1, 0),
        0x9f..=0xa6 => (2, 0),
        0xa7 | 0xc8 => (0, 0),
        0xaa | 0xab => (1, 0),
        0xac | 0xae | 0xb0 => (1, 0),
        0xad | 0xaf => (2, 0),
        0xb1 => (0, 0),
        0xb2 => (0, field_slots(cf, u2())?),
        0xb3 => (field_slots(cf, u2())?, 0),
        0xb4 => (1, field_slots(cf, u2())?),
        0xb5 => (1 + field_slots(cf, u2())?, 0),
        0xb6..=0xb9 => {
            let (_, _, desc) = cf.member_ref(u2())?;
            let sig = MethodSig::parse(desc);
            (sig.arg_slots as i32 + (op != 0xb8) as i32, sig.ret.slots() as i32)
        }
        0xba => {
            let Cp::InvokeDynamic { nat, .. } = cf.pool.get(u2() as usize)? else { return None };
            let sig = MethodSig::parse(cf.name_and_type(*nat)?.1);
            (sig.arg_slots as i32, sig.ret.slots() as i32)
        }
        0xbb => (0, 1),
        0xbc | 0xbd | 0xbe | 0xc0 | 0xc1 => (1, 1),
        0xbf | 0xc2 | 0xc3 => (1, 0),
        0xc4 => match code[pc + 1] {
            0x15 | 0x17 | 0x19 => (0, 1),
            0x16 | 0x18 => (0, 2),
            0x36 | 0x38 | 0x3a => (1, 0),
            0x37 | 0x39 => (2, 0),
            0x84 => (0, 0),
            _ => return None,
        },
        0xc5 => (code[pc + 3] as i32, 1),
        _ => return None,
    })
}

/// Control successors of the instruction at `pc` (excluding exception edges).
pub(super) fn successors(code: &[u8], pc: usize, len: usize) -> Vec<usize> {
    let rel = |off: i32| (pc as i64 + off as i64) as usize;
    match code[pc] {
        0x99..=0xa6 | 0xc6 | 0xc7 => vec![pc + len, rel(i2(code, pc + 1) as i32)],
        0xa7 => vec![rel(i2(code, pc + 1) as i32)],
        0xc8 => vec![rel(i4(code, pc + 1))],
        0xaa => {
            let p = (pc + 4) & !3;
            let (low, high) = (i4(code, p + 4), i4(code, p + 8));
            let mut v = vec![rel(i4(code, p))];
            v.extend((0..=(high - low) as usize).map(|i| rel(i4(code, p + 12 + 4 * i))));
            v
        }
        0xab => {
            let p = (pc + 4) & !3;
            let n = i4(code, p + 4) as usize;
            let mut v = vec![rel(i4(code, p))];
            v.extend((0..n).map(|i| rel(i4(code, p + 12 + 8 * i))));
            v
        }
        0xac..=0xb1 | 0xbf => vec![],
        _ => vec![pc + len],
    }
}

/// For each instruction, whether every path from it ends in `athrow` (none returns): such
/// throwing paths are cold, and the optimizing tier leaves them to the interpreter.
pub fn throw_paths(code: &[u8]) -> Vec<bool> {
    let mut starts = Vec::new();
    let mut pc = 0;
    while pc < code.len() {
        starts.push(pc);
        pc += insn_len(code, pc);
    }
    let mut throws = vec![false; code.len()];
    // The least fixed point: a loop that never provably reaches a throw stays warm.
    let mut changed = true;
    while changed {
        changed = false;
        for &pc in starts.iter().rev() {
            if throws[pc] {
                continue;
            }
            let succ = successors(code, pc, insn_len(code, pc));
            let t = code[pc] == 0xbf || (!succ.is_empty() && succ.iter().all(|&s| s < code.len() && throws[s]));
            if t {
                throws[pc] = true;
                changed = true;
            }
        }
    }
    throws
}

/// Whether any branch or exception handler leads to bci 0.
pub fn targets_start(m: &Method) -> bool {
    let Some(info) = m.code else { return false };
    let code = info.code;
    if info.handlers.iter().any(|h| h.handler == 0) {
        return true;
    }
    let mut pc = 0;
    while pc < code.len() {
        let len = insn_len(code, pc);
        let s = successors(code, pc, len);
        if s.contains(&0) {
            return true;
        }
        pc += len;
    }
    false
}

pub fn analyze(m: &Method) -> Option<Analysis> {
    let code_info = m.code?;
    let code = code_info.code;
    let cf = m.holder.cf.as_ref()?;
    let n = code.len();
    let mut depth: Vec<Option<u16>> = vec![None; n];
    let mut leader = vec![false; n];
    let mut work: Vec<(usize, u16)> = vec![(0, 0)];
    leader[0] = true;
    for h in &code_info.handlers {
        work.push((h.handler as usize, 1));
        leader[h.handler as usize] = true;
    }
    while let Some((pc, d)) = work.pop() {
        if pc >= n {
            return None;
        }
        match depth[pc] {
            Some(existing) if existing == d => continue,
            Some(_) => return None,
            None => depth[pc] = Some(d),
        }
        let len = insn_len(code, pc);
        let (pop, push) = effect(cf, code, pc)?;
        let after = d as i32 - pop + push;
        if after < 0 || after > code_info.max_stack as i32 {
            return None;
        }
        let succ = successors(code, pc, len);
        let branches = !matches!(code[pc], 0xac..=0xb1 | 0xbf) && (succ.len() != 1 || succ[0] != pc + len);
        for &s in &succ {
            if branches {
                *leader.get_mut(s)? = true;
            }
            work.push((s, after as u16));
        }
        if branches && pc + len < n {
            leader[pc + len] = true;
        }
    }
    Some(Analysis { depth, leader })
}
