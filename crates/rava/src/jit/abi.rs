//! The register calling convention between optimized methods (HotSpot's compiled calling
//! convention, in small): the first integer and reference arguments in `ARG_GPRS`, the first
//! float and double ones in `ARG_XMMS`, the rest where the interpreter keeps them, in the
//! callee's frame slots. The frame base is in rsi, the method in rdx and the thread in r12, as
//! for every compiled entry.
//!
//! Optimized code has two entries: `CompiledMethod::entry` takes every argument in the frame
//! (the interpreter's, baseline code's and virtual calls' protocol) and loads the register ones
//! before falling into `CompiledMethod::reg_entry`. A direct call from optimized code passes
//! registers and goes through the method's register trampoline (`code_cache::reg_entry`): to
//! the optimized code's register entry, or to an adapter that stores the registers into the
//! frame and enters whatever code the method has.

use super::asm::R;
use crate::method::Method;
use crate::types::BasicType;

/// Registers of integer and reference arguments, in order (all caller-saved, none a scratch
/// register of code generation or the frame base, method or thread register).
pub const ARG_GPRS: [R; 4] = [R::Rdi, R::R8, R::R9, R::R10];
/// SSE registers of float and double arguments, in order (xmm0 and xmm1 are scratch).
pub const ARG_XMMS: [u8; 4] = [2, 3, 4, 5];

/// Where an argument travels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgReg {
    Gpr(R),
    Xmm(u8),
}

/// The register arguments of `m`: (frame slot, register, wide), the receiver first. Arguments
/// past the registers of their kind are not listed: they travel in their frame slots.
pub fn arg_regs(m: &Method) -> Vec<(u16, ArgReg, bool)> {
    let (mut gprs, mut xmms) = (ARG_GPRS.iter(), ARG_XMMS.iter());
    let mut out = Vec::new();
    let mut slot = 0u16;
    if !m.is_static() {
        if let Some(&r) = gprs.next() {
            out.push((0, ArgReg::Gpr(r), false));
        }
        slot = 1;
    }
    for &ty in m.sig.params.iter() {
        let wide = matches!(ty, BasicType::Long | BasicType::Double);
        let reg = match ty {
            BasicType::Float | BasicType::Double => xmms.next().map(|&x| ArgReg::Xmm(x)),
            _ => gprs.next().map(|&r| ArgReg::Gpr(r)),
        };
        if let Some(reg) = reg {
            out.push((slot, reg, wide));
        }
        slot += if wide { 2 } else { 1 };
    }
    out
}
