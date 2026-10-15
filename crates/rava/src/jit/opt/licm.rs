//! Loop-invariant code motion (C2's loop invariant hoisting): from each loop with a plain
//! preheader (its single outside predecessor, ending in a jump to the header), instructions
//! whose inputs are all defined outside the loop move to the end of the preheader, innermost
//! loops first so values can climb several levels:
//! - constants, pure arithmetic (no division), conversions, compares, conditional moves and pure
//!   intrinsics, which may run once before the loop whether or not the loop would have;
//! - array lengths and field loads of objects known non-null at the preheader (a dominating null
//!   check, an allocation, or the receiver), which cannot fault there; a field load only when the
//!   loop makes no call, takes no monitor, has no fence or volatile read, and stores to no field
//!   at that offset.
//!
//! A field load waits for the loop's tests of its object's class: invariant branches that
//! deoptimize on one side (`guards::branches`) become preheader guards first, and a class that
//! stays tested keeps the loads of its object below the test.
//!
//! A null check inside the loop of a value defined outside it is predicated first (C2 peels
//! such loops for the same effect): it becomes one `Inst::Guard` in the preheader that
//! deoptimizes to the loop's entry state, so the value is known non-null there and its field
//! loads can leave the loop. A check on a path some iterations skip is speculated the same way.
//! A failing guard counts as a trap at the header, so the recompilation keeps the check in the
//! loop. Hoisting repeats until nothing moves: a hoisted load's null check is invariant next.
//!
//! The climate tree's distance loop reloaded `node.parameterSpace` every iteration.

use super::dom::Dom;
use super::ir::*;
use std::collections::HashSet;

pub fn hoist(g: &mut Graph) {
    preheaders(g);
    let dom = Dom::new(g);
    let n = g.blocks.len();
    let index = dom.index(n);
    let loops = dom.loops(g);
    // Values known non-null at the end of each block (by rpo position), through dominators.
    let nonnull = nonnull_by_block(g, &dom);
    // The block each instruction is in now: a value hoisted into an inner loop's preheader is
    // still inside the enclosing loop, invariant there only if hoisted again.
    let mut block_of = vec![usize::MAX; g.values.len()];
    for (b, blk) in g.blocks.iter().enumerate() {
        for &v in &blk.insts {
            block_of[v as usize] = b;
        }
    }
    for l in loops.iter().rev() {
        let h = &g.blocks[l.header as usize];
        let outside: Vec<B> = h.preds.iter().copied().filter(|&p| !l.body[p as usize]).collect();
        let [pre] = outside[..] else { continue };
        if !matches!(g.blocks[pre as usize].term, Term::Goto(t) if t == l.header) || index[pre as usize] == usize::MAX {
            continue;
        }
        // The loop's memory effects.
        let mut barrier = false;
        // Fields stored in the loop: offset and declaring class (when known).
        let mut stored: Vec<(u32, Option<&'static crate::klass::Klass>)> = Vec::new();
        // Statics stored in the loop, and whether it initializes a class (which may store any).
        let (mut stored_statics, mut inits): (HashSet<usize>, bool) = (HashSet::new(), false);
        for b in (0..n).filter(|&b| l.body[b]) {
            for &v in &g.blocks[b].insts {
                match g.inst(v) {
                    Inst::Call(..) | Inst::Monitor(..) | Inst::Fence => barrier = true,
                    // A vertex written to off-heap memory changes no field.
                    Inst::Intrinsic(..) if g.is_raw_put(v) => {}
                    Inst::Intrinsic(k, ..) if !k.is_pure() => barrier = true,
                    Inst::LoadField(..) | Inst::LoadStatic(..) if g.ordered.contains(&v) => barrier = true,
                    Inst::StoreStatic(addr, ..) => {
                        stored_statics.insert(*addr);
                    }
                    Inst::InitCheck(..) => inits = true,
                    Inst::StoreField(_, off, ..) => {
                        stored.push((*off, g.field_holders.get(&v).copied()));
                    }
                    _ => {}
                }
            }
        }
        let mut known = nonnull[index[pre as usize]].clone();
        let order: Vec<B> = dom.rpo.iter().copied().filter(|&b| l.body[b as usize]).collect();
        // A hoisted load makes its null check invariant, and the check's predicate lets the
        // loads through the loaded object follow (`this.a.b.c`).
        loop {
            predicate_null_checks(g, l, pre, &block_of, &mut known);
            super::guards::branches(g, l, pre, &block_of, &known);
            // What those added (guards, class words reloaded for them) is in the preheader.
            block_of.resize(g.values.len(), usize::MAX);
            g.blocks[pre as usize].insts.iter().for_each(|&v| block_of[v as usize] = pre as usize);
            let known = &known;
            let typed = type_tests(g, l);
            let mut moved = Vec::new();
            for &b in &order {
                let insts = g.blocks[b as usize].insts.clone();
                let mut keep = Vec::with_capacity(insts.len());
                for v in insts {
                    let invariant = |x: V| block_of.get(g.resolve(x) as usize).map_or(true, |&b| b == usize::MAX || !l.body[b]);
                    let inst = g.inst(v);
                    let movable = g.resolve(v) == v
                        && inst.inputs().into_iter().all(invariant)
                        && match inst {
                            Inst::Bin(op, ..) => !matches!(op, BinOp::Div | BinOp::Rem) || matches!(g.ty(v), Ty::F | Ty::D),
                            // A constant moves too, so the arithmetic on it can follow (inlining
                            // makes a `Const` per use, inside the loop).
                            Inst::Const(_) | Inst::Neg(_) | Inst::Convert(..) | Inst::Compare(..) | Inst::Select(..) => true,
                            Inst::Intrinsic(k, _, None) => k.is_pure() && !k.reads_mutable(),
                            // Class metadata (vtable entries, the supers display) read
                            // through a class word: never written, so no store or call in the
                            // loop can change it, and a class word is never null.
                            Inst::LoadField(o, ..) if class_word(g, *o) => true,
                            Inst::ArrayLength(o) => known_nonnull(g, known, *o) && !typed.contains(&g.resolve(*o)),
                            Inst::LoadStatic(addr, _) => !barrier && !inits && !stored_statics.contains(addr) && !g.ordered.contains(&v),
                            Inst::LoadField(o, off, _) => {
                                let o = g.resolve(*o);
                                !barrier && (g.final_loads.contains(&v) || !stored.iter().any(|&(o, h)| o == *off && Graph::fields_may_alias(g.field_holders.get(&v).copied(), h))) && !g.ordered.contains(&v) && known_nonnull(g, known, o) && (*off == 0 || !typed.contains(&o))
                            }
                            _ => false,
                        };
                    if movable {
                        block_of[v as usize] = pre as usize;
                        moved.push(v);
                    } else {
                        keep.push(v);
                    }
                }
                g.blocks[b as usize].insts = keep;
            }
            if moved.is_empty() {
                break;
            }
            g.blocks[pre as usize].insts.extend(moved);
        }
    }
}

/// Whether `x` is in `known`, or a cast of a value that is (a cast passes its object through).
fn known_nonnull(g: &Graph, known: &HashSet<V>, x: V) -> bool {
    let mut x = g.resolve(x);
    loop {
        if known.contains(&x) {
            return true;
        }
        match *g.inst(x) {
            Inst::CheckCast(y, ..) => x = g.resolve(y),
            _ => return false,
        }
    }
}

/// Whether `x` is an object's class word (`LoadField(object, 0)` read as a pointer, as the
/// builder's `klass_of` makes it).
fn class_word(g: &Graph, x: V) -> bool {
    let x = g.resolve(x);
    matches!(g.inst(x), Inst::LoadField(_, 0, crate::types::BasicType::Int)) && g.ty(x) == Ty::J
}

/// Gives each loop entered by one forward edge from a branch (`if (hi > a.length) ...; for
/// (...)`) a block of its own on that edge, ending in a jump to the header: the preheader this
/// pass, `rce.rs` and `guards.rs` place what they hoist in.
fn preheaders(g: &mut Graph) {
    let headers: Vec<B> = (0..g.blocks.len() as B).filter(|&h| g.blocks[h as usize].loop_header && !matches!(g.blocks[h as usize].term, Term::None)).collect();
    let mut renumbered = false;
    for h in headers {
        let order = g.blocks[h as usize].order;
        let entries: Vec<usize> = g.blocks[h as usize].preds.iter().enumerate().filter(|&(_, &p)| g.blocks[p as usize].order < order).map(|(i, _)| i).collect();
        let [i] = entries[..] else { continue };
        let p = g.blocks[h as usize].preds[i];
        if matches!(g.blocks[p as usize].term, Term::Goto(_)) {
            continue;
        }
        // Make room in the build order between the branch and the header.
        if !std::mem::replace(&mut renumbered, true) {
            for blk in g.blocks.iter_mut().filter(|b| b.order != u32::MAX) {
                blk.order *= 2;
            }
        }
        let pre = g.new_block(-1);
        let (order, freq) = (g.blocks[h as usize].order - 1, g.blocks[p as usize].freq.min(g.blocks[h as usize].freq));
        let blk = &mut g.blocks[pre as usize];
        (blk.term, blk.preds, blk.order, blk.freq) = (Term::Goto(h), vec![p], order, freq);
        g.blocks[h as usize].preds[i] = pre;
        match &mut g.blocks[p as usize].term {
            Term::If { then, other, .. } => [then, other].into_iter().filter(|t| **t == h).for_each(|t| *t = pre),
            Term::Switch { cases, default, .. } => cases.iter_mut().map(|c| &mut c.1).chain(std::iter::once(default)).filter(|t| **t == h).for_each(|t| *t = pre),
            _ => {}
        }
    }
}

/// The values whose class loop `l` still tests (a class-word compare, `instanceof`, a cast): a
/// load from one may rely on the test (an inlined method of the tested class reading its own
/// field), so it stays below the test until the test is a guard in the preheader.
fn type_tests(g: &Graph, l: &super::dom::Loop) -> HashSet<V> {
    let mut typed = HashSet::new();
    let class_of = |x: V| match *g.inst(g.resolve(x)) {
        Inst::LoadField(o, 0, _) => Some(g.resolve(o)),
        _ => None,
    };
    for blk in g.blocks.iter().enumerate().filter(|&(b, _)| l.body[b]).map(|(_, blk)| blk) {
        for &v in &blk.insts {
            match *g.inst(v) {
                Inst::InstanceOf(x, _) | Inst::CheckCast(x, ..) => {
                    typed.insert(g.resolve(x));
                }
                Inst::Guard(_, _, a, b, _) => typed.extend(class_of(a).into_iter().chain(class_of(b))),
                Inst::Intrinsic(crate::jit::intrinsics::Intrinsic::IsInstance, ref args, _) => typed.extend(args.get(1).map(|&x| g.resolve(x))),
                _ => {}
            }
        }
        if let Term::If { a, b, .. } = blk.term {
            typed.extend(class_of(a).into_iter().chain(class_of(b)));
        }
    }
    typed
}

/// For each block (by rpo position), the values non-null at its end: checked by a null check in
/// it or a dominator, tested non-null on the way, allocated there, the receiver, or a phi of
/// non-null values.
pub(super) fn nonnull_by_block(g: &Graph, dom: &Dom) -> Vec<HashSet<V>> {
    // What `conds.rs` proved (and so dropped the checks of): tested branches, merged
    // non-null values.
    let (proved, phis) = super::conds::nonnull_facts(g);
    let mut out: Vec<HashSet<V>> = vec![HashSet::new(); dom.rpo.len()];
    for (i, &b) in dom.rpo.iter().enumerate() {
        let mut set = if i == 0 || dom.idom[i] == usize::MAX { HashSet::new() } else { out[dom.idom[i]].clone() };
        if i == 0 {
            set.extend(g.receiver);
            set.extend(phis.iter().copied());
        }
        set.extend(proved[b as usize].iter().copied());
        for &v in &g.blocks[b as usize].insts {
            match g.inst(v) {
                Inst::NullCheck(x, _) => {
                    set.insert(g.resolve(*x));
                }
                // A null-check predicate (`predicate_null_checks`).
                Inst::Guard(Cmp::Ne, Ty::A, x, null, _) if g.constant(g.resolve(*null)) == Some(0) => {
                    set.insert(g.resolve(*x));
                }
                Inst::New(..) | Inst::NewArray(..) | Inst::NewMulti(..) => {
                    set.insert(v);
                }
                _ => {}
            }
        }
        out[i] = set;
    }
    out
}

/// Moves the null checks of loop-invariant values in `l` to its preheader `pre` as guards
/// (adding the values to `known`). A check on a path the loop may never take is speculated too:
/// a null there deoptimizes on entry, and the recompilation keeps the checks in the loop.
fn predicate_null_checks(g: &mut Graph, l: &super::dom::Loop, pre: B, block_of: &[usize], known: &mut HashSet<V>) {
    let h = l.header as usize;
    let Some(entry_state) = g.loop_entry.get(&l.header).cloned() else { return };
    let invariant = |x: V| block_of.get(x as usize).map_or(true, |&b| b == usize::MAX || !l.body[b]);
    let mut checks = Vec::new();
    for b in (0..g.blocks.len()).filter(|&b| l.body[b]) {
        for &v in &g.blocks[b].insts {
            if let Inst::NullCheck(x, _) = g.inst(v) {
                let x = g.resolve(*x);
                if invariant(x) && !known.contains(&x) {
                    checks.push((b, v, x));
                }
            }
        }
    }
    if checks.is_empty() {
        return;
    }
    let entry = g.blocks[h].preds.iter().position(|&p| p == pre).expect("the preheader enters the header");
    let Some(ps) = super::rce::entry_state_at(g, &entry_state, h, entry) else { return };
    let null = g.add(0, Inst::Const(0), Ty::A);
    for (b, v, x) in checks {
        if known.insert(x) {
            g.add(pre, Inst::Guard(Cmp::Ne, Ty::A, x, null, ps.clone()), Ty::I);
        }
        g.blocks[b].insts.retain(|&i| i != v);
    }
}
