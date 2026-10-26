//! Keep heap-loaded references compressed when every use can consume their stored form.

use super::super::ir::{Block, Cmp, Graph, Inst, Term, Ty, B, V};
use super::super::regalloc::{Allocation, Loc};
use super::{cond, Gen};

/// A branch comparing one dynamic reference with one folded reference constant.
fn branch_ref(g: &Graph, a: V, b: V) -> Option<(V, u32)> {
    let (a, b) = (g.resolve(a), g.resolve(b));
    if g.ty(a) != Ty::A || g.ty(b) != Ty::A {
        return None;
    }
    match (g.constant_value(a), g.constant_value(b)) {
        (Some(k), None) => Some((b, encoded_literal(k)?)),
        (None, Some(k)) => Some((a, encoded_literal(k)?)),
        _ => None,
    }
}

/// Encode a reference value after `branch_ref` established that its IR type is `A`.
fn encoded_literal(raw: i64) -> Option<u32> {
    if !crate::heap::COMPRESSED {
        return None;
    }
    let raw = u64::try_from(raw).ok()?;
    let mask = (1u64 << crate::heap::NARROW_SHIFT) - 1;
    if raw & mask != 0 {
        return None;
    }
    u32::try_from(raw >> crate::heap::NARROW_SHIFT).ok()
}

fn branch_candidate(g: &Graph, block: &Block) -> Option<V> {
    let Term::If {
        cmp: Cmp::Eq | Cmp::Ne,
        ty: Ty::A,
        a,
        b,
        ..
    } = &block.term
    else {
        return None;
    };
    branch_ref(g, *a, *b).map(|(v, _)| v)
}

/// Emit a 32-bit reference comparison when `refs` proved the dynamic operand stays compressed.
pub(super) fn compare(
    gen: &mut Gen<'_>,
    cmp: Cmp,
    ty: Ty,
    a: V,
    b: V,
) -> Option<super::super::super::asm::Cond> {
    if ty != Ty::A || !matches!(cmp, Cmp::Eq | Cmp::Ne) {
        return None;
    }
    let (v, constant) = branch_ref(gen.g, a, b)?;
    if constant == 0 {
        return None;
    }
    if !gen.narrow.contains(&gen.g.resolve(v)) {
        return None;
    }
    let r = gen.reg(v, crate::jit::asm::R::Rax);
    // `alu_ri(false, ...)` compares the low 32 bits, preserving the encoded pointer bits even
    // when the immediate's top bit is set.
    gen.a
        .alu_ri(false, crate::jit::asm::Alu::Cmp, r, constant as i32);
    Some(cond(cmp))
}

/// Values safely kept compressed in their register while their same-block users consume them.
pub(super) fn refs(g: &Graph, al: &Allocation) -> std::collections::HashSet<V> {
    use crate::types::BasicType::Reference;
    let mut out = std::collections::HashSet::new();
    if !crate::heap::COMPRESSED {
        return out;
    }
    // Uses of every value outside instructions (terminators, phi inputs): never narrow.
    let mut escapes = vec![false; g.values.len()];
    for b in g.blocks.iter().filter(|b| !matches!(b.term, Term::None)) {
        for u in b.term.inputs() {
            if branch_candidate(g, b) != Some(g.resolve(u)) {
                escapes[g.resolve(u) as usize] = true;
            }
        }
        for &v in &b.insts {
            if let Inst::Phi(ins) = g.inst(v) {
                ins.iter()
                    .for_each(|&u| escapes[g.resolve(u) as usize] = true);
            }
        }
    }
    let mut users: Vec<Vec<(B, usize)>> = vec![Vec::new(); g.values.len()];
    for (bi, b) in g
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| !matches!(b.term, Term::None))
    {
        for (i, &v) in b.insts.iter().enumerate() {
            if g.resolve(v) != v || matches!(g.inst(v), Inst::Phi(_)) {
                continue;
            }
            for u in g.inst(v).inputs() {
                users[g.resolve(u) as usize].push((bi as B, i));
            }
        }
        // The branch reads after the last instruction.
        if let Some(v) = branch_candidate(g, b) {
            users[v as usize].push((bi as B, b.insts.len()));
        }
    }
    for (bi, b) in g
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| !matches!(b.term, Term::None))
    {
        for (i, &v) in b.insts.iter().enumerate() {
            if !matches!(
                g.inst(v),
                Inst::LoadField(_, _, Reference) | Inst::LoadIndexed(_, _, Reference)
            ) || g.resolve(v) != v
                || escapes[v as usize]
            {
                continue;
            }
            if !matches!(al.loc[v as usize], Loc::Reg(_))
                || al.home[v as usize].is_some()
                || al.state_regs[v as usize]
                || users[v as usize].is_empty()
            {
                continue;
            }
            let Some(last) = users[v as usize]
                .iter()
                .map(|&(ub, ui)| (ub == bi as B).then_some(ui))
                .try_fold(0, |m, u| u.map(|u| m.max(u)))
            else {
                continue;
            };
            let base_use = |w: V| match g.inst(w) {
                Inst::LoadField(o, ..) | Inst::ArrayLength(o) | Inst::NullCheck(o, _) => {
                    g.resolve(*o) == v
                }
                Inst::StoreField(o, _, Reference, x) => g.resolve(*x) == v && g.resolve(*o) != v,
                _ => false,
            };
            let quiet = |w: V| {
                let inst = g.inst(w);
                inst.state().is_none()
                    && !matches!(
                        inst,
                        Inst::Call(..) | Inst::Intrinsic(..) | Inst::Monitor(..) | Inst::Phi(_)
                    )
            };
            let window = &b.insts[i + 1..last.min(b.insts.len() - 1) + 1];
            let uses_ok = users[v as usize]
                .iter()
                .all(|&(_, ui)| ui == b.insts.len() || base_use(b.insts[ui]));
            let quiet_ok = window.iter().all(|&w| {
                quiet(w) || matches!(g.inst(w), Inst::NullCheck(o, _) if g.resolve(*o) == v)
            });
            if uses_ok && quiet_ok {
                out.insert(v);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::encoded_literal;

    #[test]
    fn reference_constants_must_be_aligned_and_fit() {
        assert_eq!(encoded_literal(0), Some(0));
        assert_eq!(encoded_literal(0x1008), Some(0x201));
        assert_eq!(encoded_literal((1i64 << 31) * 8), Some(1 << 31));
        assert_eq!(encoded_literal(u32::MAX as i64 * 8), Some(u32::MAX));
        assert_eq!(encoded_literal(0x1009), None);
        assert_eq!(encoded_literal(-8), None);
        assert_eq!(encoded_literal((u32::MAX as i64 + 1) * 8), None);
    }
}
