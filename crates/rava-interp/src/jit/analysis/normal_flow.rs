//! Reachability from method entry without exceptional edges.
//!
//! Exception handlers are executed by the cold interpreter transition. Their
//! presence must not invent normal stack predecessors in generated code.

use crate::jit::{find_leaders, insn_len_at, s16, switch_targets};

pub(crate) struct NormalFlow {
    reachable: Vec<bool>,
}

impl NormalFlow {
    pub(crate) fn compute(bc: &[u8]) -> Option<Self> {
        // Validate every branch target, including branches in dead code, using
        // the same instruction-boundary contract as the body compiler.
        find_leaders(bc)?;
        let mut reachable = vec![false; bc.len()];
        let mut pending = vec![0usize];
        reachable[0] = true;
        while let Some(pc) = pending.pop() {
            let next = pc.checked_add(insn_len_at(bc, pc)?)?;
            let mut visit = |target: usize| -> Option<()> {
                let seen = reachable.get_mut(target)?;
                if !*seen {
                    *seen = true;
                    pending.push(target);
                }
                Some(())
            };
            match bc[pc] {
                0x99..=0xa7 | 0xc6 | 0xc7 => {
                    let target = (pc as i64).checked_add(s16(bc, pc + 1) as i64)?;
                    visit(usize::try_from(target).ok()?)?;
                    if bc[pc] != 0xa7 { visit(next)?; }
                }
                0xaa | 0xab => {
                    let (default, cases) = switch_targets(bc, pc)?;
                    visit(default)?;
                    for (_, target) in cases { visit(target)?; }
                }
                0xac..=0xb1 | 0xbf => {}
                _ => visit(next)?,
            }
        }
        Some(Self { reachable })
    }

    pub(crate) fn contains(&self, pc: usize) -> bool {
        self.reachable.get(pc).copied().unwrap_or(false)
    }
}

#[cfg(test)]
mod tests;
