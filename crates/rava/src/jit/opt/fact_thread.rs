//! Jump threading by what each predecessor brings (C2's split-if on a merged object, the
//! pattern `ReduceAllocationMerges` targets): a merge branching on values from before it, on its
//! phis, or on a field of an object its phi merges (`Optional.ofNullable`'s result, tested by
//! `isPresent`: the empty constant's value, the new object's stored one) sends each predecessor
//! whose operands decide the test straight to the outcome. A fresh object no longer merged with
//! anything is then scalar replaced (`escape.rs`).

use super::ir::*;
use super::thread::{drop_pred, own_readers, reads, redirect_ok, redirect_with};

/// Folds each merge block holding only phis read by the next merge's phis (an inlined method's
/// returns, merged again at the caller's merge) into that merge: its predecessors jump there
/// directly, so each brings its own value to the branch after it (`thread`).
fn fold_empty_merges(g: &mut Graph) {
    super::phis::fold_trivial(g);
    let uses = super::split::uses(g);
    for b in 0..g.blocks.len() as B {
        let blk = &g.blocks[b as usize];
        let Term::Goto(m) = blk.term else { continue };
        let mb = &g.blocks[m as usize];
        if m == b || blk.preds.len() < 2 || blk.loop_header || mb.loop_header || g.loop_entry.contains_key(&b) || g.loop_entry.contains_key(&m) {
            continue;
        }
        if blk.preds.iter().any(|p| mb.preds.contains(p) || match g.blocks[*p as usize].term { Term::Switch { .. } => true, Term::If { then, other, .. } => then == other, _ => false }) || mb.preds.iter().filter(|&&p| p == b).count() != 1 {
            continue;
        }
        // Only phis, each read on the edge into `m`'s phis alone (states included).
        // (Constants and unread pure values may stay behind: constants need no block.)
        let phis_only = blk.insts.iter().all(|&v| {
            g.resolve(v) != v || matches!(g.inst(v), Inst::Phi(_) | Inst::Const(_)) || uses[v as usize].is_empty() && matches!(g.inst(v), Inst::LoadField(..) | Inst::Bin(..) | Inst::Convert(..) | Inst::ArrayLength(_) | Inst::Copy(_) | Inst::Neg(_))
        });
        let local = blk.insts.iter().filter(|&&v| matches!(g.inst(v), Inst::Phi(_))).all(|&v| uses[v as usize].iter().all(|u| matches!(u, super::split::Use::Edge(p, _) if *p == b)));
        if !phis_only || !local || states_read(g, &blk.insts) {
            continue;
        }
        let j = mb.preds.iter().position(|&p| p == b).unwrap();
        let preds = blk.preds.clone();
        let own = blk.insts.clone();
        let m_phis: Vec<V> = mb.insts.iter().copied().filter(|&v| matches!(g.inst(v), Inst::Phi(_))).collect();
        for q in m_phis {
            let Inst::Phi(ins) = g.inst(q).clone() else { unreachable!() };
            let x = g.resolve(ins[j]);
            let per: Vec<V> = (0..preds.len()).map(|k| match g.inst(x) {
                Inst::Phi(xs) if own.contains(&x) => xs[k],
                _ => x,
            }).collect();
            if let Inst::Phi(ins) = &mut g.values[q as usize].inst {
                ins.remove(j);
                ins.extend(per);
            }
        }
        let mbm = &mut g.blocks[m as usize];
        mbm.preds.remove(j);
        mbm.preds.extend(preds.iter().copied());
        for &p in &preds {
            if let Term::Goto(t) = &mut g.blocks[p as usize].term {
                *t = m;
            } else if let Term::If { then, other, .. } = &mut g.blocks[p as usize].term {
                if *then == b {
                    *then = m;
                }
                if *other == b {
                    *other = m;
                }
            }
        }
        let blk = &mut g.blocks[b as usize];
        blk.preds.clear();
        blk.insts.clear();
        blk.term = Term::None;
    }
}

/// Whether a deoptimization state anywhere reads one of `vs`.
fn states_read(g: &Graph, vs: &[V]) -> bool {
    let mut xs = Vec::new();
    g.blocks.iter().filter(|b| !matches!(b.term, Term::None)).any(|blk| {
        blk.insts.iter().filter_map(|&v| g.inst(v).state()).chain(match &blk.term {
            Term::Deopt(s) => Some(s),
            _ => None,
        }).any(|s| {
            xs.clear();
            super::liveness::state_values(g, s, &mut xs);
            xs.iter().any(|&x| vs.contains(&g.resolve(x)))
        })
    })
}

/// Moves each allocation of a branching merge, with the stores initializing it there, into the
/// branch target that alone reads it (`orElseThrow`'s supplier, read only where it throws), so
/// the merge is left with what `fact_shape` threads. Its state still names its instruction,
/// which the interpreter runs again from there with the branch.
fn sink_allocations(g: &mut Graph) {
    let mut dom: Option<(super::dom::Dom, Vec<usize>)> = None;
    for m in 0..g.blocks.len() as B {
        let Term::If { then, other, .. } = g.blocks[m as usize].term else { continue };
        if g.blocks[m as usize].preds.len() < 2 || then == other || !g.blocks[m as usize].insts.iter().any(|&v| matches!(g.inst(v), Inst::New(..))) {
            continue;
        }
        let (d, index) = dom.get_or_insert_with(|| {
            let d = super::dom::Dom::new(g);
            let i = d.index(g.blocks.len());
            (d, i)
        });
        let under = |t: B, b: B| index[t as usize] != usize::MAX && index[b as usize] != usize::MAX && d.dominates(index[t as usize], index[b as usize]);
        let uses = super::split::uses(g);
        let news: Vec<V> = g.blocks[m as usize].insts.iter().copied().filter(|&v| g.resolve(v) == v && matches!(g.inst(v), Inst::New(..))).collect();
        for x in news {
            let stores: Vec<V> = g.blocks[m as usize].insts.iter().copied().filter(|&s| matches!(*g.inst(s), Inst::StoreField(o, _, _, val) if g.resolve(o) == x && g.resolve(val) != x)).collect();
            let readers: Vec<B> = uses[x as usize].iter().filter(|u| !matches!(u, super::split::Use::Inst(_, s) if stores.contains(s))).map(|u| u.block()).collect();
            let mut state_blocks = Vec::new();
            let mut xs = Vec::new();
            for (b, blk) in g.blocks.iter().enumerate().filter(|(_, b)| !matches!(b.term, Term::None)) {
                let states = blk.insts.iter().filter(|&&v| v != x).filter_map(|&v| g.inst(v).state()).chain(match &blk.term {
                    Term::Deopt(s) => Some(s),
                    _ => None,
                });
                for s in states {
                    xs.clear();
                    super::liveness::state_values(g, s, &mut xs);
                    if xs.iter().any(|&y| g.resolve(y) == x) {
                        state_blocks.push(b as B);
                    }
                }
            }
            let all: Vec<B> = readers.into_iter().chain(state_blocks).collect();
            let Some(t) = [then, other].into_iter().find(|&t| g.blocks[t as usize].preds == [m] && !all.is_empty() && all.iter().all(|&b| under(t, b))) else { continue };
            let moved: Vec<V> = std::iter::once(x).chain(stores).collect();
            g.blocks[m as usize].insts.retain(|v| !moved.contains(v));
            let at = g.blocks[t as usize].insts.iter().position(|&v| !matches!(g.inst(v), Inst::Phi(_))).unwrap_or(g.blocks[t as usize].insts.len());
            for (k, v) in moved.into_iter().enumerate() {
                g.blocks[t as usize].insts.insert(at + k, v);
            }
        }
    }
}

pub fn thread(g: &mut Graph) {
    fold_empty_merges(g);
    sink_allocations(g);
    let reads = reads(g);
    let mut asks = std::collections::HashMap::new();
    let mut merges = Vec::new();
    for m in 0..g.blocks.len() as B {
        let Some((cmp, ty, a, b)) = fact_shape(g, m) else { continue };
        let mut any = false;
        for i in 0..g.blocks[m as usize].preds.len() {
            let p = g.blocks[m as usize].preds[i];
            if !matches!(g.blocks[p as usize].term, Term::Goto(t) if t == m) {
                continue;
            }
            let (Some(x), Some(y)) = (operand_at(g, m, i, a), operand_at(g, m, i, b)) else { continue };
            asks.insert((p, m), (cmp, ty, x, y));
            any = true;
        }
        if any {
            merges.push(m);
        }
    }
    if merges.is_empty() {
        return;
    }
    let decided = super::conds::edge_decisions(g, &asks);
    for m in merges {
        let Term::If { then, other, .. } = g.blocks[m as usize].term else { continue };
        let todo: Vec<(B, B)> = g.blocks[m as usize].preds.iter().filter_map(|&p| decided.get(&(p, m)).map(|&r| (p, if r { then } else { other }))).collect();
        if !todo.iter().any(|&(p, t)| redirect_ok(g, p, m, t)) {
            continue;
        }
        // The field loads the branch reads that something after it reads too: each redirected
        // predecessor brings what it knows of them (`operand_at`), or it stays.
        let loads: Vec<V> = g.blocks[m as usize].insts.iter().copied().filter(|&v| g.resolve(v) == v && matches!(g.inst(v), Inst::LoadField(..) | Inst::ArrayLength(_)) && reads[v as usize] > local_reads(g, m, v)).collect();
        let mut extras = std::collections::HashMap::new();
        let mut todo = todo;
        todo.retain(|&(p, _)| {
            let i = g.blocks[m as usize].preds.iter().position(|&x| x == p).unwrap();
            let mut extra = std::collections::HashMap::new();
            for &l in &loads {
                match operand_at(g, m, i, l) {
                    Some(x) => {
                        extra.insert(l, x);
                    }
                    None => return false,
                }
            }
            extras.insert(p, extra);
            true
        });
        let phis: Vec<V> = g.blocks[m as usize].insts.iter().copied().filter(|&v| g.resolve(v) == v && matches!(g.inst(v), Inst::Phi(_))).collect();
        if todo.is_empty() || !phis.into_iter().chain(loads.iter().copied()).all(|p| own_readers(g, m, p, [then, other])) {
            continue;
        }
        for (p, t) in todo {
            let Some(i) = g.blocks[m as usize].preds.iter().position(|&x| x == p) else { continue };
            if redirect_ok(g, p, m, t) {
                redirect_with(g, p, i, m, t, &extras[&p]);
            }
        }
        if g.blocks[m as usize].preds.is_empty() {
            let blk = &mut g.blocks[m as usize];
            let succ = std::mem::replace(&mut blk.term, Term::None).successors();
            blk.insts.clear();
            for s in succ {
                drop_pred(g, s, m);
            }
        }
    }
    super::phis::fold_trivial(g);
}

/// How often `m`'s own branch and instructions read `v` (`reads` counts every read).
fn local_reads(g: &Graph, m: B, v: V) -> u32 {
    let blk = &g.blocks[m as usize];
    let term = blk.term.inputs().into_iter().filter(|&x| g.resolve(x) == v).count();
    let insts: usize = blk.insts.iter().filter(|&&x| g.resolve(x) == x && !matches!(g.inst(x), Inst::Phi(_))).map(|&x| g.inst(x).inputs().into_iter().filter(|&y| g.resolve(y) == v).count()).sum();
    (term + insts) as u32
}

/// A merge branching on operands `operand_at` can follow into its predecessors, holding only
/// phis, constants and field loads (those read after the branch too are repaired by the caller):
/// the comparison and operands.
fn fact_shape(g: &Graph, m: B) -> Option<(Cmp, Ty, V, V)> {
    let blk = &g.blocks[m as usize];
    let Term::If { cmp, ty: ty @ (Ty::I | Ty::J | Ty::A), a, b, then, other, .. } = blk.term else { return None };
    if blk.loop_header || g.loop_entry.contains_key(&m) || then == other || blk.preds.len() < 2 {
        return None;
    }
    let own = |v: V| blk.insts.contains(&g.resolve(v));
    let allowed = |v: V| match *g.inst(v) {
        Inst::Phi(_) | Inst::Const(_) => true,
        Inst::LoadField(o, ..) | Inst::ArrayLength(o) => !own(o) || matches!(g.inst(g.resolve(o)), Inst::Phi(_)),
        _ => g.resolve(v) != v,
    };
    blk.insts.iter().all(|&v| allowed(v)).then_some((cmp, ty, a, b))
}

/// `v` (an operand of merge `m`'s branch) as `m`'s `i`th predecessor brings it: itself when
/// defined before `m`, a phi's input, or a field of the object it brings when that is known
/// there (a fresh object's one store or zero, a class word, a constant object's final field).
fn operand_at(g: &mut Graph, m: B, i: usize, v: V) -> Option<V> {
    let v = g.resolve(v);
    if g.constant(v).is_some() || !g.blocks[m as usize].insts.contains(&v) {
        return Some(v);
    }
    match g.inst(v).clone() {
        Inst::Phi(inputs) => Some(g.resolve(inputs[i])),
        Inst::LoadField(o, off, bt) => {
            let x = operand_at(g, m, i, o)?;
            let known = field_of(g, x, off, bt)?;
            Some(known.map_or_else(|c| g.add(m, Inst::Const(c), g.ty(v)), |w| w))
        }
        Inst::ArrayLength(o) => (!g.blocks[m as usize].insts.contains(&g.resolve(o))).then_some(v),
        _ => None,
    }
}

/// Field `off` of `x` where `x` is: the value one store of a fresh object put there (or zero
/// with none), as a value; a constant (Err) for a class word of a fresh or constant object or a
/// constant object's final field.
fn field_of(g: &Graph, x: V, off: u32, bt: crate::types::BasicType) -> Option<Result<V, i64>> {
    use crate::types::BasicType;
    match *g.inst(x) {
        Inst::New(k, _) if off == 0 && bt == BasicType::Int => Some(Err(crate::heap::klass_word(k) as i64)),
        Inst::New(..) => {
            let home = g.blocks.iter().position(|b| b.insts.contains(&x))?;
            // Only its own field accesses (and merges) see the object: nothing else can write it.
            let passed = g.blocks.iter().filter(|b| !matches!(b.term, Term::None)).any(|blk| {
                blk.term.inputs().into_iter().any(|y| g.resolve(y) == x)
                    || blk.insts.iter().any(|&s| {
                        g.resolve(s) == s
                            && match *g.inst(s) {
                                Inst::Phi(_) | Inst::LoadField(..) => false,
                                Inst::StoreField(o, _, _, val) => g.resolve(o) != x && g.resolve(val) == x || g.resolve(val) == x,
                                ref i => i.inputs().into_iter().any(|y| g.resolve(y) == x),
                            }
                    })
            });
            if passed {
                return None;
            }
            let mut stores = g.blocks.iter().enumerate().flat_map(|(b, blk)| blk.insts.iter().map(move |&s| (b, s))).filter(|&(_, s)| g.resolve(s) == s && matches!(*g.inst(s), Inst::StoreField(o, f, _, _) if g.resolve(o) == x && f == off));
            match (stores.next(), stores.next()) {
                (None, _) => Some(Err(0)),
                (Some((b, s)), None) if b == home => match *g.inst(s) {
                    Inst::StoreField(_, _, sbt, val) if sbt == bt => Some(Ok(g.resolve(val))),
                    _ => None,
                },
                _ => None,
            }
        }
        Inst::Const(o) if g.ty(x) == Ty::A && o != 0 => {
            if off == 0 && bt == BasicType::Int {
                return Some(Err(crate::heap::klass_word(crate::jit::fold::class_of(o as usize)) as i64));
            }
            crate::jit::fold::final_field_at(o as usize, off).map(Err)
        }
        _ => None,
    }
}

