//! Lock coarsening (C2's `EliminateLocks` of adjacent regions): a monitor exit followed in its
//! block by an enter of the same object, with only instructions between that cannot
//! deoptimize, call, block or allocate, merges the two regions into one: both go. The accesses
//! between join the critical section, which the memory model allows (they could have been
//! ordered inside it). A partially unrolled loop over a synchronized body (`unroll.rs`) then
//! takes the lock once per copy group instead of once per iteration.
//!
//! No deoptimization state lies between the two, so every state of the merged region resumes
//! the interpreter with the object locked once, as it was before.

use super::ir::*;

pub fn coarsen(g: &mut Graph) {
    let mut gone: Vec<V> = Vec::new();
    for blk in &g.blocks {
        // The last exit seen, while every instruction since may join the region.
        let mut open: Option<(V, V)> = None;
        for &v in blk.insts.iter().filter(|&&v| g.resolve(v) == v) {
            match g.inst(v) {
                &Inst::Monitor(false, o, _) => open = Some((v, g.resolve(o))),
                &Inst::Monitor(true, o, _) => {
                    if let Some((exit, _)) = open.take().filter(|&(_, lock)| lock == g.resolve(o)) {
                        gone.extend([exit, v]);
                    }
                }
                inst if joinable(inst) && !g.ordered.contains(&v) => {}
                _ => open = None,
            }
        }
    }
    if gone.is_empty() {
        return;
    }
    for blk in &mut g.blocks {
        blk.insts.retain(|v| !gone.contains(v));
    }
}

/// Whether `inst` may move into a critical section: no state (it cannot deoptimize or throw),
/// no call, allocation, fence or intrinsic (which may call out or block). Volatile accesses
/// stay out as well (checked by the caller).
fn joinable(inst: &Inst) -> bool {
    inst.state().is_none() && !matches!(inst, Inst::Call(..) | Inst::Monitor(..) | Inst::Fence | Inst::New(..) | Inst::NewArray(..) | Inst::NewMulti(..) | Inst::Intrinsic(..) | Inst::Phi(_))
}

