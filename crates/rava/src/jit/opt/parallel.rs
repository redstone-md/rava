//! Diagnostic-only summary of memory effects inside optimized SSA loops.
//!
//! This pass never changes code generation, Java semantics or loop scheduling.
//! It exposes why automatic DOALL parallelization needs inter-iteration alias
//! analysis. A loop containing no calls is not necessarily independent: a phi
//! can carry a reduction, and array accesses can alias across iterations.
//!
//! Run with RAVA_PAR_REPORT=all (or a method-name substring). Run the same
//! workload without the variable to measure production performance.

use super::dom::Dom;
use super::ir::{Graph, Inst, Term, B};
use crate::method::Method;

#[derive(Default)]
struct Effects {
    blocks: usize,
    insts: usize,
    header_phis: usize,
    reads: usize,
    writes: usize,
    calls: usize,
    opaque: usize,
    ordered: usize,
}

/// Report loops *after* ordinary SSA optimizations. This is not a proof that
/// any loop can be parallelized; even a read-only loop may have scalar
/// loop-carried dependencies or observe concurrently modified Java state.
pub(super) fn report(g: &Graph, m: &Method) {
    let Some(filter) = env_once!("RAVA_PAR_REPORT") else {
        return;
    };
    let filter = filter.to_string_lossy();
    if !filter.is_empty() && filter != "all" && !m.to_string().contains(filter.as_ref()) {
        return;
    }

    let dom = Dom::new(g);
    for lp in dom.loops(g) {
        let mut e = Effects::default();
        for (b, &inside) in lp.body.iter().enumerate() {
            if !inside || matches!(g.blocks[b].term, Term::None) {
                continue;
            }
            e.blocks += 1;
            for &v in &g.blocks[b].insts {
                if g.resolve(v) != v {
                    continue;
                }
                e.insts += 1;
                if g.ordered.contains(&v) {
                    e.ordered += 1;
                }
                if reads_mutable(g.inst(v)) {
                    e.reads += 1;
                }
                match g.inst(v) {
                    Inst::Phi(_) if b as B == lp.header => e.header_phis += 1,
                    Inst::StoreField(..) | Inst::StoreStatic(..) | Inst::StoreIndexed(..) => {
                        e.writes += 1
                    }
                    Inst::Call(..) => e.calls += 1,
                    Inst::Monitor(..)
                    | Inst::Fence
                    | Inst::InitCheck(..)
                    | Inst::New(..)
                    | Inst::NewArray(..)
                    | Inst::NewMulti(..) => e.opaque += 1,
                    Inst::Intrinsic(kind, ..) if !kind.is_pure() => e.opaque += 1,
                    _ => {}
                }
            }
        }
        let verdict = if e.ordered != 0 || e.opaque != 0 || e.calls != 0 {
            "opaque-effects"
        } else if e.writes != 0 {
            "needs-memory-dependence-proof"
        } else {
            "needs-scalar-dependence-proof"
        };
        eprintln!(
            "[rava-par] {m} loop_header={} bci={} blocks={} insts={} header_phis={} reads={} writes={} calls={} opaque={} ordered={} verdict={verdict}",
            lp.header,
            g.blocks[lp.header as usize].bci,
            e.blocks, e.insts, e.header_phis, e.reads, e.writes, e.calls, e.opaque, e.ordered,
        );
    }
}

fn reads_mutable(inst: &Inst) -> bool {
    match inst {
        Inst::LoadField(..)
        | Inst::LoadStatic(..)
        | Inst::LoadIndexed(..)
        | Inst::ArrayLength(..) => true,
        Inst::Intrinsic(kind, ..) => kind.reads_mutable(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::reads_mutable;
    use crate::jit::opt::ir::Inst;
    use crate::jit::{intrinsics::Intrinsic, leaf::Leaf};
    use crate::types::BasicType;

    #[test]
    fn mutable_intrinsic_reads_are_reported() {
        let unsafe_read =
            Inst::Intrinsic(Intrinsic::UnsafeGet(BasicType::Int, false), vec![], None);
        let leaf_read = Inst::Intrinsic(Intrinsic::Leaf(Leaf::CountPositives), vec![], None);
        let immutable_query = Inst::Intrinsic(Intrinsic::IsInstance, vec![], None);

        assert!(reads_mutable(&unsafe_read));
        assert!(reads_mutable(&leaf_read));
        assert!(!reads_mutable(&immutable_query));
    }
}
