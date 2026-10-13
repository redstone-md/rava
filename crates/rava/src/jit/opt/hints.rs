//! Register hints from the register calling convention (`jit::abi`): a parameter that arrives
//! in a register, and an argument that dies at a direct call passing it in a register, prefer
//! that register, so that neither the prologue nor the call moves it.

use super::ir::{Callee, Graph, Inst};
use super::regalloc::Loc;
use crate::jit::abi::{self, ArgReg};
use crate::method::Method;

fn loc(r: ArgReg) -> Loc {
    match r {
        ArgReg::Gpr(r) => Loc::Reg(r),
        ArgReg::Xmm(x) => Loc::Xmm(x),
    }
}

/// The preferred register of each value, given each value's position (`pos`) and the last
/// position its interval covers (`end`).
pub(super) fn abi_hints(g: &Graph, m: &Method, pos: &[u32], end: &[u32]) -> Vec<Option<Loc>> {
    let mut hint = vec![None; g.values.len()];
    let params = abi::arg_regs(m);
    for (v, val) in g.values.iter().enumerate() {
        match &val.inst {
            Inst::Param(slot) => hint[v] = params.iter().find(|p| p.0 == *slot).map(|p| loc(p.1)),
            Inst::Call(Callee::Direct(t), args, _) if g.resolve(v as u32) == v as u32 => {
                for (&a, r) in args.iter().zip(abi::regs_by_arg(t)) {
                    let a = g.resolve(a) as usize;
                    if let Some(r) = r.filter(|_| end[a] == pos[v] && hint[a].is_none()) {
                        hint[a] = Some(loc(r));
                    }
                }
            }
            _ => {}
        }
    }
    hint
}
