//! Write-barrier elision for initializing stores (HotSpot's ReduceInitialCardMarks): a
//! reference store into an object allocated earlier in the same block, with nothing between
//! that could reach a safepoint, stores into a young object. Such a store cannot create an
//! old-to-young reference, so it needs no card mark.
//!
//! This matters more here than in a copying collector: the heap does not move objects, so new
//! objects fill free cells between old ones, and every card an initializing store marks makes
//! the next young collection rescan the old objects sharing it.

use super::ir::*;

/// Per value: true for a reference store that needs no card mark.
pub fn unbarriered(g: &Graph) -> Vec<bool> {
    let mut out = vec![false; g.values.len()];
    let mut fresh: Vec<V> = Vec::new();
    for b in &g.blocks {
        fresh.clear();
        for &v in &b.insts {
            match &g.values[v as usize].inst {
                Inst::StoreField(o, ..) | Inst::StoreIndexed(o, ..) => out[v as usize] = fresh.contains(&g.resolve(*o)),
                Inst::New(..) | Inst::NewArray(..) | Inst::NewMulti(..) => {
                    // The allocation may collect, promoting what was allocated before it.
                    fresh.clear();
                    fresh.push(v);
                }
                i if may_stop(i) => fresh.clear(),
                _ => {}
            }
        }
    }
    out
}

/// Whether `i` may reach a safepoint (and so a collection).
fn may_stop(i: &Inst) -> bool {
    match i {
        Inst::Call(..) | Inst::InitCheck(..) | Inst::Monitor(..) => true,
        Inst::Intrinsic(k, _, s) => s.is_some() || !k.is_pure(),
        _ => false,
    }
}
