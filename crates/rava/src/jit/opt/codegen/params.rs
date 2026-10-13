//! The register entry of optimized code (`jit::abi`): the frame-slot entry that loads the
//! register arguments, and the prologue's placing of those arguments.

use super::Gen;
use crate::jit::abi::{self, ArgReg};
use crate::jit::asm::{mem, Label, R, X};
use crate::jit::opt::ir::{Inst, V};
use crate::jit::opt::regalloc::{Allocation, Loc};
use crate::method::Method;

impl Gen<'_> {
    /// The method's two entries (`jit::abi`) when it can take register arguments (`allowed`):
    /// here, the frame-slot entry, which loads the register arguments from the frame; then the
    /// register entry, whose label is returned. Each parameter that arrives in a register must
    /// be among the leading instructions of the entry block (its interval starts at the entry,
    /// where the prologue places it); a frameless body using rsi as its frame base must not
    /// have one allocated to rsi.
    pub(super) fn register_entry(&mut self, m: &'static Method, al: &Allocation, leaf: bool, allowed: bool) -> Option<Label> {
        if !allowed {
            return None;
        }
        let g = self.g;
        let args = abi::arg_regs(m);
        let by_slot = |s: u16| args.iter().find(|a| a.0 == s).map(|a| a.1);
        let lead = g.blocks[0].insts.iter().take_while(|&&v| matches!(g.inst(v), Inst::Param(_))).count();
        let elsewhere = g.blocks.iter().enumerate().any(|(i, b)| {
            b.insts.iter().enumerate().any(|(j, &v)| matches!(g.inst(v), Inst::Param(s) if by_slot(*s).is_some() && !(i == 0 && j < lead)))
        });
        let params: Vec<(V, ArgReg, u16)> = g.blocks[0].insts[..lead]
            .iter()
            .filter_map(|&v| match g.inst(v) {
                Inst::Param(s) => by_slot(*s).map(|r| (v, r, *s)),
                _ => None,
            })
            .collect();
        if elsewhere || leaf && params.iter().any(|&(v, _, _)| al.loc[v as usize] == Loc::Reg(R::Rsi)) {
            return None;
        }
        // The frame-slot entry: rsi is the frame (r13 is set by the prologue).
        for &(v, reg, slot) in &params {
            if self.needs_param(v) {
                match reg {
                    ArgReg::Gpr(r) => self.a.load(true, r, mem(R::Rsi, 8 * slot as i32)),
                    ArgReg::Xmm(x) => self.a.fload(true, X(x), mem(R::Rsi, 8 * slot as i32)),
                }
            }
        }
        self.reg_params = params;
        // Direct calls enter here: aligned, as a function start (only fixed-length loads precede
        // it, so branch relaxation does not move it).
        self.a.align(16);
        let entry = self.a.new_label();
        self.a.bind(entry);
        Some(entry)
    }

    /// Whether parameter `v` is read at all: from a location, or from its frame slot (a home
    /// that deoptimization reads, or a spilled parameter).
    fn needs_param(&self, v: V) -> bool {
        !matches!(self.loc[v as usize], Loc::None) || matches!(self.home[v as usize], Some(Loc::Frame(_)))
    }

    /// Moves the register arguments to the parameters' locations, and into their frame slots
    /// where those are read (deoptimization's homes, spilled parameters).
    pub(super) fn place_params(&mut self) {
        let params = self.reg_params.clone();
        for &(v, reg, slot) in &params {
            if matches!(self.home[v as usize], Some(Loc::Frame(_))) || matches!(self.loc[v as usize], Loc::Frame(_)) {
                match reg {
                    ArgReg::Gpr(r) => self.a.store(true, self.slot(slot as i32), r),
                    ArgReg::Xmm(x) => self.a.fstore(true, self.slot(slot as i32), X(x)),
                }
            }
        }
        let moves: Vec<(Loc, Loc)> = params
            .iter()
            .filter(|&&(v, _, _)| matches!(self.loc[v as usize], Loc::Reg(_) | Loc::Xmm(_) | Loc::Stack(_)))
            .map(|&(v, reg, _)| (self.loc[v as usize], match reg { ArgReg::Gpr(r) => Loc::Reg(r), ArgReg::Xmm(x) => Loc::Xmm(x) }))
            .collect();
        crate::jit::opt::moves::parallel(&mut self.a, &moves);
    }

    /// Whether `v` is a parameter the prologue placed from a register argument.
    pub(in crate::jit::opt) fn placed(&self, v: V) -> bool {
        self.reg_params.iter().any(|p| p.0 == v)
    }
}
