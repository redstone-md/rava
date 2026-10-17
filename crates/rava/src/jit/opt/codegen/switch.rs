//! A `switch`'s dispatch on its key in `rax`, as C2 lowers one: a jump table when its cases
//! are dense, a binary search over them when they are many but sparse, and a compare per case
//! when they are few. A compare per case ran every case before the default, 30 for an enum
//! switch of 30 constants.

use super::Gen;
use crate::jit::asm::{mem_idx, rip, Alu, Cond, Label, R};

/// Cases from which a dense switch dispatches through a table.
pub(super) const JUMP_TABLE_CASES: usize = 5;
/// Cases up to which a sparse switch compares each in turn.
pub(super) const LINEAR_CASES: usize = 6;

/// Whether sorted `cases` cover at least a quarter of their key range (and it is not huge):
/// a table of the range then costs at most four entries per case.
pub(super) fn dense(cases: &[(i32, Label)]) -> bool {
    let (lo, hi) = (cases[0].0 as i64, cases[cases.len() - 1].0 as i64);
    let span = hi - lo + 1;
    span <= 4 * cases.len() as i64 && span <= 4096
}

impl Gen<'_> {
    /// Jumps to the label of the case `rax` holds, through a table of the key range of the
    /// sorted `cases`; keys outside it, or without a case, go to `otherwise`.
    pub(super) fn jump_table(&mut self, cases: &[(i32, Label)], otherwise: Label) {
        let (lo, hi) = (cases[0].0, cases[cases.len() - 1].0);
        // A 32-bit operation clears the high half, which the indexed load reads (an int may sit
        // in its register sign-extended).
        if lo != 0 {
            self.a.alu_ri(false, Alu::Sub, R::Rax, lo);
        } else {
            self.a.mov_rr(false, R::Rax, R::Rax);
        }
        // Unsigned: a key below the range wrapped above it.
        self.a.alu_ri(false, Alu::Cmp, R::Rax, (hi as i64 - lo as i64) as i32);
        self.a.jcc(Cond::A, otherwise);
        let table = self.a.new_label();
        self.a.lea(R::R11, rip(table));
        self.a.load_sx(4, R::Rax, mem_idx(R::R11, R::Rax, 4, 0));
        self.a.alu_rr(true, Alu::Add, R::Rax, R::R11);
        self.a.jmp_r(R::Rax);
        self.a.bind(table);
        let mut next = cases.iter().peekable();
        for k in lo as i64..=hi as i64 {
            let target = match next.peek() {
                Some(&&(c, l)) if c as i64 == k => {
                    next.next();
                    l
                }
                _ => otherwise,
            };
            self.a.table_entry(table, target);
        }
    }

    /// Jumps to the label of the case `rax` holds by a binary search over the sorted `cases`;
    /// falls through when none matches.
    pub(super) fn switch_search(&mut self, cases: &[(i32, Label)], otherwise: Label) {
        if cases.len() <= LINEAR_CASES {
            for &(k, l) in cases {
                self.a.alu_ri(false, Alu::Cmp, R::Rax, k);
                self.a.jcc(Cond::E, l);
            }
            return;
        }
        let mid = cases.len() / 2;
        let (k, l) = cases[mid];
        let upper = self.a.new_label();
        self.a.alu_ri(false, Alu::Cmp, R::Rax, k);
        self.a.jcc(Cond::E, l);
        self.a.jcc(Cond::G, upper);
        self.switch_search(&cases[..mid], otherwise);
        self.a.jmp(otherwise);
        self.a.bind(upper);
        self.switch_search(&cases[mid + 1..], otherwise);
    }
}
