//! Branch clean-ups for optimized code (`Asm::enable_peephole`): dropping a jump to the next
//! instruction, inverting a conditional jump over an unconditional one, and re-encoding branches
//! within reach with 8-bit displacements.

use super::{Asm, Label};

impl Asm {
    /// Removes the last `size` bytes (a branch), moving labels bound at the end with them.
    fn drop_branch(&mut self, size: u32) {
        let end = self.pos();
        self.buf.truncate((end - size) as usize);
        if self.bound_at == end {
            for &b in &self.bound {
                self.labels[b as usize] = Some(end - size);
            }
            self.bound_at = end - size;
        }
    }

    /// Re-encodes branches whose target is within reach as 2-byte short branches (to a fixed
    /// point: shortening one only brings others closer), moving labels and other fixups along.
    pub(super) fn relax(&mut self) {
        // (start, fixup index, short) of each branch still in the code, in code order.
        let mut br: Vec<(u32, usize, bool)> = std::mem::take(&mut self.branches).into_iter().map(|(at, fx)| (at, fx, false)).collect();
        br.sort_by_key(|b| b.0);
        let saving = |buf: &[u8], at: u32| if buf[at as usize] == 0xE9 { 3 } else { 4 };
        // New position of old position `p`: minus the savings of short branches before it.
        let shift = |br: &[(u32, usize, bool)], buf: &[u8], p: u32| -> u32 {
            p - br.iter().take_while(|b| b.0 < p).filter(|b| b.2).map(|b| saving(buf, b.0)).sum::<u32>()
        };
        loop {
            let mut changed = false;
            for i in 0..br.len() {
                if br[i].2 {
                    continue;
                }
                let (at, fx, _) = br[i];
                let Some(target) = self.labels[self.fixups[fx].1 .0 as usize] else { continue };
                let from = shift(&br, &self.buf, at) as i64 + 2;
                let to = shift(&br, &self.buf, target) as i64;
                if (-128..128).contains(&(to - from)) {
                    br[i].2 = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        if !br.iter().any(|b| b.2) {
            return;
        }
        // Rebuild: short branches get their final displacement now, other references move.
        let old = std::mem::take(&mut self.buf);
        let new_pos = |p: u32| shift(&br, &old, p);
        let mut out = Vec::with_capacity(old.len());
        let mut short_fixups = vec![false; self.fixups.len()];
        let mut from = 0usize;
        for &(at, fx, short) in &br {
            if !short {
                continue;
            }
            out.extend_from_slice(&old[from..at as usize]);
            let target = new_pos(self.labels[self.fixups[fx].1 .0 as usize].unwrap()) as i64;
            let rel = (target - (out.len() as i64 + 2)) as i8;
            if old[at as usize] == 0xE9 {
                out.extend_from_slice(&[0xEB, rel as u8]);
                from = at as usize + 5;
            } else {
                out.extend_from_slice(&[0x70 + (old[at as usize + 1] - 0x80), rel as u8]);
                from = at as usize + 6;
            }
            short_fixups[fx] = true;
        }
        out.extend_from_slice(&old[from..]);
        let fixups = std::mem::take(&mut self.fixups);
        self.fixups = fixups.into_iter().enumerate().filter(|(i, _)| !short_fixups[*i]).map(|(_, (at, l))| (new_pos(at), l)).collect();
        for p in self.labels.iter_mut().flatten() {
            *p = new_pos(*p);
        }
        self.buf = out;
    }

    pub(super) fn branch_peephole(&mut self, l: Label) {
        let end = self.pos();
        // A jump to the next instruction.
        if let Some(&(at, fx)) = self.branches.last() {
            if at + 5 == end && self.buf[at as usize] == 0xE9 && fx + 1 == self.fixups.len() && self.fixups[fx].1 == l {
                self.fixups.pop();
                self.branches.pop();
                self.drop_branch(5);
            }
        }
        // A conditional jump over an unconditional one to here, no label bound between them.
        let end = self.pos();
        let n = self.branches.len();
        if n >= 2 {
            let ((jcc, fc), (jmp, fj)) = (self.branches[n - 2], self.branches[n - 1]);
            if jmp + 5 == end && jcc + 6 == jmp && self.buf[jmp as usize] == 0xE9 && self.buf[jcc as usize] == 0x0F
                && fj + 1 == self.fixups.len() && fc + 2 == self.fixups.len() && self.fixups[fc].1 == l && self.bound_at < jmp
            {
                let far = self.fixups[fj].1;
                self.buf[jcc as usize + 1] ^= 1;
                self.fixups[fc].1 = far;
                self.fixups.pop();
                self.branches.pop();
                self.drop_branch(5);
            }
        }
    }

}
