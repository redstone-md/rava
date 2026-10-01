//! Parallel moves between allocated locations (phi inputs along an edge): every destination
//! receives the value its source held before any move, cycles broken through a scratch register.

use super::regalloc::Loc;
use crate::jit::asm::{mem, Asm, M, R, X};

/// Scratch registers of the move resolver (not allocatable).
const CYCLE: R = R::R11;
const TEMP: R = R::Rax;

pub fn spill(slot: i32) -> M {
    mem(R::Rsp, 8 * slot)
}

fn frame(slot: i32) -> M {
    mem(R::R13, 8 * slot)
}

/// `dst := src` for one location pair (`dst` is a register or a spill slot).
pub fn mov(a: &mut Asm, dst: Loc, src: Loc) {
    match (dst, src) {
        (d, s) if d == s => {}
        (Loc::Reg(d), Loc::Reg(s)) => a.mov_rr(true, d, s),
        (Loc::Reg(d), Loc::Stack(s)) => a.load(true, d, spill(s)),
        (Loc::Reg(d), Loc::Frame(s)) => a.load(true, d, frame(s)),
        (Loc::Xmm(d), Loc::Frame(s)) => a.fload(true, X(d), frame(s)),
        (Loc::Stack(d), Loc::Frame(s)) => {
            a.load(true, TEMP, frame(s));
            a.store(true, spill(d), TEMP);
        }
        (Loc::Reg(d), Loc::Const(c)) => a.mov_ri(d, c),
        (Loc::Stack(d), Loc::Reg(s)) => a.store(true, spill(d), s),
        (Loc::Stack(d), Loc::Stack(s)) => {
            a.load(true, TEMP, spill(s));
            a.store(true, spill(d), TEMP);
        }
        (Loc::Stack(d), Loc::Const(c)) if i32::try_from(c).is_ok() => a.store_imm(spill(d), c as i32),
        (Loc::Stack(d), Loc::Const(c)) => {
            a.mov_ri(TEMP, c);
            a.store(true, spill(d), TEMP);
        }
        (Loc::Xmm(d), Loc::Xmm(s)) => a.mov_xx(X(d), X(s)),
        (Loc::Xmm(d), Loc::Stack(s)) => a.fload(true, X(d), spill(s)),
        (Loc::Xmm(d), Loc::Reg(s)) => a.mov_xr(true, X(d), s),
        (Loc::Xmm(d), Loc::Const(c)) => {
            a.mov_ri(TEMP, c);
            a.mov_xr(true, X(d), TEMP);
        }
        (Loc::Stack(d), Loc::Xmm(s)) => a.fstore(true, spill(d), X(s)),
        (Loc::Reg(d), Loc::Xmm(s)) => a.mov_rx(true, d, X(s)),
        (d, s) => unreachable!("move {s:?} to {d:?}"),
    }
}

/// Performs all `(dst, src)` moves as if simultaneously.
pub fn parallel(a: &mut Asm, moves: &[(Loc, Loc)]) {
    let mut pending: Vec<(Loc, Loc)> = moves.iter().copied().filter(|(d, s)| d != s && *d != Loc::None).collect();
    while !pending.is_empty() {
        // A move whose destination no other pending move still reads can go now.
        let ready = (0..pending.len()).find(|&i| !pending.iter().any(|&(_, s)| s == pending[i].0));
        match ready {
            Some(i) => {
                let (d, s) = pending.swap_remove(i);
                mov(a, d, s);
            }
            None => {
                // Only cycles remain: park one source in the scratch register.
                let (_, s) = pending[0];
                mov(a, Loc::Reg(CYCLE), s);
                for m in pending.iter_mut().filter(|m| m.1 == s) {
                    m.1 = Loc::Reg(CYCLE);
                }
            }
        }
    }
}
