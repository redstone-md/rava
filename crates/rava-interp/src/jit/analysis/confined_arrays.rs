//! Structural confinement proof shared by primitive and reference arrays.
//!
//! The proof is an under-approximation: any malformed bytecode, ambiguous merge, exception
//! handler, unsupported stack shape, or operation that can carry a live candidate through a
//! host/GC boundary rejects the candidate.  It depends only on bytecode and method shape.

use rava_classfile::Code;

use crate::jit::{insn_len_at, s16, switch_targets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfinedArrayKind {
    Primitive(u8),
    Reference,
}

impl ConfinedArrayKind {
    #[inline]
    fn producer(self) -> u8 {
        match self {
            Self::Primitive(_) => 0xbc,
            Self::Reference => 0xbd,
        }
    }

    #[inline]
    fn consumers(self) -> (u8, u8) {
        match self {
            Self::Primitive(4 | 8) => (0x33, 0x54),
            Self::Primitive(5) => (0x34, 0x55),
            Self::Primitive(6) => (0x30, 0x51),
            Self::Primitive(7) => (0x31, 0x52),
            Self::Primitive(9) => (0x35, 0x56),
            Self::Primitive(10) => (0x2e, 0x4f),
            Self::Primitive(11) => (0x2f, 0x50),
            Self::Reference => (0x32, 0x53),
            Self::Primitive(_) => unreachable!(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    stack: Vec<bool>,
    locals: Vec<bool>,
}

impl State {
    fn live(&self) -> bool {
        self.stack.iter().chain(&self.locals).any(|&v| v)
    }

    fn pop(&mut self) -> Option<bool> {
        self.stack.pop()
    }

    fn pop_clean(&mut self, n: usize) -> Option<()> {
        for _ in 0..n {
            if self.pop()? {
                return None;
            }
        }
        Some(())
    }
}

pub(crate) struct ConfinedFlow {
    starts: Vec<bool>,
    successors: Vec<Vec<usize>>,
    pcs: Vec<usize>,
}

impl ConfinedFlow {
    pub(crate) fn pcs(&self) -> &[usize] {
        &self.pcs
    }

    pub(crate) fn decode(bc: &[u8]) -> Option<Self> {
        if bc.is_empty() {
            return None;
        }
        let mut starts = vec![false; bc.len()];
        let mut lengths = vec![0; bc.len()];
        let mut pcs = Vec::new();
        let mut pc = 0;
        while pc < bc.len() {
            let len = insn_len_at(bc, pc)?;
            if len == 0 || pc.checked_add(len)? > bc.len() {
                return None;
            }
            starts[pc] = true;
            lengths[pc] = len;
            pcs.push(pc);
            pc += len;
        }

        let mut successors = vec![Vec::new(); bc.len()];
        for &pc in &pcs {
            let len = lengths[pc];
            let fall = pc + len;
            let branch16 = || -> Option<usize> {
                let target = pc as i64 + s16(bc, pc + 1) as i64;
                usize::try_from(target).ok()
            };
            match bc[pc] {
                0x99..=0xa6 | 0xc6 | 0xc7 => {
                    successors[pc].push(branch16()?);
                    if fall < bc.len() {
                        successors[pc].push(fall);
                    } else {
                        return None;
                    }
                }
                0xa7 => successors[pc].push(branch16()?),
                0xaa | 0xab => {
                    let (default, cases) = switch_targets(bc, pc)?;
                    successors[pc].push(default);
                    successors[pc].extend(cases.into_iter().map(|(_, target)| target));
                }
                0xac..=0xb1 | 0xbf => {}
                _ if fall < bc.len() => successors[pc].push(fall),
                _ => return None,
            }
            successors[pc].sort_unstable();
            successors[pc].dedup();
            if successors[pc]
                .iter()
                .any(|&target| target >= bc.len() || !starts[target])
            {
                return None;
            }
        }
        Some(Self {
            starts,
            successors,
            pcs,
        })
    }
}

/// Prove that the candidate at `site_pc` never crosses an unsupported operation or method exit.
pub(crate) fn proves_confined(
    code: &Code,
    flow: &ConfinedFlow,
    site_pc: usize,
    kind: ConfinedArrayKind,
) -> bool {
    let bc = &code.bytecode;
    let mut incoming: Vec<Option<State>> = vec![None; bc.len()];
    incoming[0] = Some(State {
        stack: Vec::new(),
        locals: vec![false; code.max_locals as usize],
    });
    let mut work = vec![0usize];
    let mut reached_site = false;
    while let Some(pc) = work.pop() {
        if !flow.starts[pc] {
            return false;
        }
        let Some(mut state) = incoming[pc].clone() else {
            return false;
        };
        if pc == site_pc {
            reached_site = true;
        }
        if transfer(bc, pc, site_pc, kind, &mut state).is_none() {
            return false;
        }
        if state.stack.len() > code.max_stack as usize
            || (flow.successors[pc].is_empty() && !state.stack.is_empty())
        {
            return false;
        }
        for &next in &flow.successors[pc] {
            match &incoming[next] {
                None => {
                    incoming[next] = Some(state.clone());
                    work.push(next);
                }
                Some(old) if old == &state => {}
                Some(_) => return false,
            }
        }
    }
    reached_site
}

fn local_index(
    bc: &[u8],
    pc: usize,
    indexed: u8,
    short: std::ops::RangeInclusive<u8>,
) -> Option<usize> {
    let op = bc[pc];
    if op == indexed {
        Some(*bc.get(pc + 1)? as usize)
    } else if short.contains(&op) {
        Some((op - *short.start()) as usize)
    } else {
        None
    }
}

fn live_op_is_safe(op: u8, kind: ConfinedArrayKind, stack: &[bool]) -> bool {
    let (load, store) = kind.consumers();
    match op {
        0x2e..=0x35 => {
            stack.len() >= 2 && stack[stack.len() - 2] && !stack[stack.len() - 1] && op == load
        }
        0x4f..=0x56 => {
            stack.len() >= 3
                && stack[stack.len() - 3]
                && !stack[stack.len() - 2]
                && !stack[stack.len() - 1]
                && op == store
        }
        0xbe => stack.last() == Some(&true),
        // These may call, allocate, resolve, throw through a callback, or deoptimise.
        0x12..=0x14 | 0x6c..=0x73 | 0xb2..=0xbd | 0xbf | 0xc0 | 0xc1 => false,
        _ => true,
    }
}

fn transfer(
    bc: &[u8],
    pc: usize,
    site_pc: usize,
    kind: ConfinedArrayKind,
    state: &mut State,
) -> Option<()> {
    let op = bc[pc];
    if pc == site_pc && op != kind.producer() {
        return None;
    }
    let live = state.live();
    if live && !live_op_is_safe(op, kind, &state.stack) {
        return None;
    }
    match op {
        0x00 => {}
        0x01..=0x11 => state.stack.push(false),
        0x12..=0x14 => {
            if live {
                return None;
            }
            state.stack.push(false);
        }
        0x15..=0x18 | 0x1a..=0x29 => state.stack.push(false),
        0x19 | 0x2a..=0x2d => {
            let index = local_index(bc, pc, 0x19, 0x2a..=0x2d)?;
            state.stack.push(*state.locals.get(index)?);
        }
        0x2e..=0x35 => {
            let index = state.pop()?;
            let array = state.pop()?;
            if index || (array && op != kind.consumers().0) {
                return None;
            }
            state.stack.push(false);
        }
        0x36..=0x39 | 0x3b..=0x4a => state.pop_clean(1)?,
        0x3a | 0x4b..=0x4e => {
            let index = local_index(bc, pc, 0x3a, 0x4b..=0x4e)?;
            let value = state.pop()?;
            *state.locals.get_mut(index)? = value;
        }
        0x4f..=0x56 => {
            let value = state.pop()?;
            let index = state.pop()?;
            let array = state.pop()?;
            if value || index || (array && op != kind.consumers().1) {
                return None;
            }
        }
        0x57 => {
            state.pop()?;
        }
        0x59 => state.stack.push(*state.stack.last()?),
        0x58 | 0x5a..=0x5f => return None,
        0x60..=0x73 | 0x78..=0x83 | 0x94..=0x98 => {
            state.pop_clean(2)?;
            state.stack.push(false);
        }
        0x74..=0x77 | 0x85..=0x93 => {
            state.pop_clean(1)?;
            state.stack.push(false);
        }
        0x84 | 0xa7 => {}
        0x99..=0x9e => state.pop_clean(1)?,
        0x9f..=0xa4 => state.pop_clean(2)?,
        0xa5 | 0xa6 => state.pop_clean(2)?,
        0xaa | 0xab => state.pop_clean(1)?,
        0xac..=0xaf => state.pop_clean(1)?,
        0xb0 => {
            if state.pop()? {
                return None;
            }
        }
        0xb1 => {}
        0xb2..=0xbb | 0xbd => {
            if live {
                return None;
            }
            match op {
                0xb2 | 0xbb => state.stack.push(false),
                0xb3 => state.pop_clean(1)?,
                0xb4 => {
                    state.pop_clean(1)?;
                    state.stack.push(false);
                }
                0xb5 => state.pop_clean(2)?,
                0xbd if kind == ConfinedArrayKind::Reference => {
                    state.pop_clean(1)?;
                    if pc == site_pc {
                        state.stack.push(true);
                    } else {
                        state.stack.push(false);
                    }
                }
                _ => return None,
            }
        }
        0xbc => {
            state.pop_clean(1)?;
            if pc == site_pc {
                if !matches!(kind, ConfinedArrayKind::Primitive(_)) {
                    return None;
                }
                state.stack.push(true);
            } else {
                if live {
                    return None;
                }
                state.stack.push(false);
            }
        }
        0xbe => {
            state.pop()?;
            state.stack.push(false);
        }
        0xbf => {
            if state.pop()? {
                return None;
            }
        }
        0xc0 => {
            if state.pop()? {
                return None;
            }
            state.stack.push(false);
        }
        0xc1 => {
            if live || state.pop()? {
                return None;
            }
            state.stack.push(false);
        }
        0xc6 | 0xc7 => state.pop_clean(1)?,
        _ => return None,
    }
    Some(())
}
