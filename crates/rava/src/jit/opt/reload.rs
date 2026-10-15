//! Reloads of spilled values, one per stretch of code between calls (live-range splitting at
//! calls, as C2's register allocator splits around them): linear scan spills a value for its
//! whole interval, so a loop's cursor that lives across the calls of an inlined body is read
//! from its slot at every use. After a first allocation, each spilled value read at least twice
//! in a call-free region of the extended basic block tree (blocks entered only from the one
//! before) gets a `Copy` at the start of the region; the region's reads take the copy, which
//! crosses no call and so gets a scratch-free caller-saved register when one is left, and the
//! graph is allocated again. Deoptimization states keep reading the original.

use super::ir::*;
use super::regalloc::Loc;
use std::collections::HashMap;

/// What a region reads repeatedly: a spilled value, or a constant too wide for an immediate
/// (materialized with a 10-byte `movabs` at each read otherwise), by type and value since
/// inlining makes a `Const` per use.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Val(V),
    Wide(Ty, i64),
}

/// A read: by an instruction, or by a block's terminator.
#[derive(Clone, Copy)]
enum Read {
    Inst(V),
    Term(B),
}

/// A call-free region: where it starts (block, instruction index), its blocks, and the reads
/// of each spilled value in it.
struct Region {
    start: (B, usize),
    blocks: Vec<B>,
    reads: HashMap<Key, Vec<(V, Read)>>,
}

/// Adds the copies; returns them (the graph then needs allocating again).
pub fn split(g: &mut Graph, loc: &[Loc]) -> Vec<V> {
    let mut added = loops(g, loc);
    let spilled = |g: &Graph, x: V| matches!(loc.get(x as usize), Some(Loc::Stack(_) | Loc::Frame(_))) && !matches!(g.inst(x), Inst::Const(_));
    // The key of a read of `x` by `by` (None: not worth a copy). A wide constant is only taken
    // from instructions that read any register alike (not intrinsics or guards, whose code
    // generation may want the constant itself).
    let key = |g: &Graph, x: V, by: Option<V>| -> Option<Key> {
        if spilled(g, x) {
            return Some(Key::Val(x));
        }
        let c = g.constant(x).filter(|&c| i32::try_from(c).is_err() && matches!(g.ty(x), Ty::I | Ty::J | Ty::A))?;
        let plain = by.is_none_or(|v| matches!(g.inst(v), Inst::LoadField(..) | Inst::StoreField(..) | Inst::LoadIndexed(..) | Inst::StoreIndexed(..) | Inst::ArrayLength(_) | Inst::Bin(..) | Inst::Compare(..) | Inst::Call(..)));
        plain.then_some(Key::Wide(g.ty(x), c))
    };
    let live = |g: &Graph, b: B| !matches!(g.blocks[b as usize].term, Term::None);
    // The extended basic block tree: a block entered only from `p` continues `p`'s regions.
    let n = g.blocks.len();
    let mut children = vec![Vec::new(); n];
    for b in 0..n as B {
        if !live(g, b) {
            continue;
        }
        for s in g.blocks[b as usize].term.successors() {
            if g.blocks[s as usize].preds[..] == [b] && !children[b as usize].contains(&s) {
                children[b as usize].push(s);
            }
        }
    }
    let roots: Vec<B> = (0..n as B).filter(|&b| live(g, b) && (b == 0 || g.blocks[b as usize].preds.len() != 1 || g.blocks[b as usize].preds[0] == b)).collect();
    let mut regions: Vec<Region> = Vec::new();
    for root in roots {
        // (block, region it continues).
        regions.push(Region { start: (root, 0), blocks: Vec::new(), reads: HashMap::new() });
        let mut work = vec![(root, regions.len() - 1)];
        while let Some((b, mut r)) = work.pop() {
            regions[r].blocks.push(b);
            let insts = g.blocks[b as usize].insts.clone();
            for (i, &v) in insts.iter().enumerate() {
                if g.resolve(v) != v {
                    continue;
                }
                let inst = g.inst(v);
                if !matches!(inst, Inst::Phi(_)) {
                    for x in inst.inputs() {
                        let x = g.resolve(x);
                        if let Some(k) = key(g, x, Some(v)) {
                            regions[r].reads.entry(k).or_default().push((x, Read::Inst(v)));
                        }
                    }
                }
                if matches!(inst, Inst::Call(..)) {
                    regions.push(Region { start: (b, i + 1), blocks: vec![b], reads: HashMap::new() });
                    r = regions.len() - 1;
                }
            }
            for x in g.blocks[b as usize].term.inputs() {
                let x = g.resolve(x);
                if let Some(k) = key(g, x, None) {
                    regions[r].reads.entry(k).or_default().push((x, Read::Term(b)));
                }
            }
            work.extend(children[b as usize].iter().map(|&c| (c, r)));
        }
    }
    // Where each value is defined: block and index.
    let mut def: HashMap<V, (B, usize)> = HashMap::new();
    for b in 0..n as B {
        for (i, &v) in g.blocks[b as usize].insts.iter().enumerate() {
            def.insert(v, (b, i));
        }
    }
    // Later starts first: copies inserted into a block do not move the indices before them.
    regions.sort_by_key(|r| std::cmp::Reverse(r.start));
    for region in regions {
        let (root, from) = region.start;
        // The copies go after the root's phis and parameters (both defined on entry).
        let at = from.max(g.blocks[root as usize].insts.iter().take_while(|&&v| matches!(g.inst(v), Inst::Phi(_) | Inst::Param(_))).count());
        let mut reads: Vec<(Key, Vec<(V, Read)>)> = region.reads.into_iter().filter(|(_, rs)| rs.len() >= 2).collect();
        reads.sort_by_key(|(_, rs)| rs[0].0);
        let mut copies = Vec::new();
        for (k, rs) in reads {
            let x = rs[0].0;
            // Only a value defined before the region is there to copy (its definition then
            // dominates the region's root, as it does each read); a constant is anywhere.
            let before = matches!(k, Key::Wide(..)) || def.get(&x).is_some_and(|&(b, i)| !region.blocks.contains(&b) || (b == root && i < at));
            if !before {
                continue;
            }
            let c = g.add(root, Inst::Copy(x), g.ty(x));
            g.blocks[root as usize].insts.pop();
            copies.push(c);
            // The reads name the value or one standing for it (aliases are not rewritten), or
            // a constant of the same value.
            let names: Vec<V> = match k {
                Key::Val(x) => (0..g.values.len() as V).filter(|&u| g.resolve(u) == x).collect(),
                Key::Wide(..) => {
                    let xs: Vec<V> = rs.iter().map(|&(u, _)| u).collect();
                    (0..g.values.len() as V).filter(|&u| xs.contains(&g.resolve(u))).collect()
                }
            };
            for (_, r) in rs {
                let to = |u: V| if names.contains(&u) { c } else { u };
                match r {
                    Read::Inst(v) => {
                        let mut inst = std::mem::replace(&mut g.values[v as usize].inst, Inst::Fence);
                        inst.map_inputs(to);
                        g.values[v as usize].inst = inst;
                    }
                    Read::Term(b) => {
                        let mut term = std::mem::replace(&mut g.blocks[b as usize].term, Term::None);
                        term.map_inputs(to);
                        g.blocks[b as usize].term = term;
                    }
                }
            }
        }
        if !copies.is_empty() {
            added.extend(&copies);
            let insts = &mut g.blocks[root as usize].insts;
            let tail = insts.split_off(at);
            insts.extend(copies);
            insts.extend(tail);
        }
    }
    added
}

/// A loop's own copies: a value spilled for its whole interval because a rarely taken path of a
/// loop (a call, many temporaries) left no register there, defined before the loop and read in
/// it more often than the loop is entered, gets a `Copy` at the end of the loop's preheader
/// that the loop reads instead. The copy lives across the whole loop, so it gets a register
/// only when one is free there throughout (a callee-saved one, or saved around a rare call);
/// otherwise `undo_spilled` takes it back. Innermost loops first: an enclosing loop's copy then
/// feeds the inner one's.
fn loops(g: &mut Graph, loc: &[Loc]) -> Vec<V> {
    let spilled = |g: &Graph, x: V| matches!(loc.get(x as usize), Some(Loc::Stack(_) | Loc::Frame(_))) && !matches!(g.inst(x), Inst::Const(_) | Inst::Phi(_));
    let dom = super::dom::Dom::new(g);
    let mut def: HashMap<V, B> = HashMap::new();
    for (b, blk) in g.blocks.iter().enumerate() {
        for &v in &blk.insts {
            def.insert(v, b as B);
        }
    }
    let mut added = Vec::new();
    for l in dom.loops(g).iter().rev() {
        let h = l.header as usize;
        let outside: Vec<B> = g.blocks[h].preds.iter().copied().filter(|&p| !l.body[p as usize]).collect();
        let [pre] = outside[..] else { continue };
        if !matches!(g.blocks[pre as usize].term, Term::Goto(t) if t as usize == h) {
            continue;
        }
        let entry = g.blocks[pre as usize].freq.max(1e-3);
        // Reads in the loop of each value defined outside it, with their blocks' frequency.
        let mut reads: HashMap<V, (f32, Vec<Read>)> = HashMap::new();
        for b in (0..g.blocks.len()).filter(|&b| l.body[b]) {
            let f = g.blocks[b].freq;
            for &v in &g.blocks[b].insts {
                if g.resolve(v) != v || matches!(g.inst(v), Inst::Phi(_)) {
                    continue;
                }
                for x in g.inst(v).inputs() {
                    let x = g.resolve(x);
                    if spilled(g, x) && def.get(&x).is_some_and(|&d| !l.body[d as usize]) {
                        let e = reads.entry(x).or_insert((0.0, Vec::new()));
                        e.0 += f;
                        e.1.push(Read::Inst(v));
                    }
                }
            }
            for x in g.blocks[b].term.inputs() {
                let x = g.resolve(x);
                if spilled(g, x) && def.get(&x).is_some_and(|&d| !l.body[d as usize]) {
                    let e = reads.entry(x).or_insert((0.0, Vec::new()));
                    e.0 += f;
                    e.1.push(Read::Term(b as B));
                }
            }
        }
        let mut hot: Vec<(V, Vec<Read>)> = reads.into_iter().filter(|(_, (f, _))| *f >= 2.0 * entry).map(|(x, (_, rs))| (x, rs)).collect();
        hot.sort_by_key(|(x, _)| *x);
        for (x, rs) in hot {
            let c = g.add(pre, Inst::Copy(x), g.ty(x));
            added.push(c);
            for r in rs {
                match r {
                    Read::Inst(v) => {
                        let mut inst = std::mem::replace(&mut g.values[v as usize].inst, Inst::Fence);
                        let aliases: Vec<V> = inst.inputs().into_iter().filter(|&u| g.resolve(u) == x).collect();
                        inst.map_inputs(|u| if aliases.contains(&u) { c } else { u });
                        g.values[v as usize].inst = inst;
                    }
                    Read::Term(b) => {
                        let mut term = std::mem::replace(&mut g.blocks[b as usize].term, Term::None);
                        let aliases: Vec<V> = term.inputs().into_iter().filter(|&u| g.resolve(u) == x).collect();
                        term.map_inputs(|u| if aliases.contains(&u) { c } else { u });
                        g.blocks[b as usize].term = term;
                    }
                }
            }
        }
    }
    added
}

/// Takes back the copies `split` added that got no register after all (a load and a store
/// more, for nothing): their reads go back to the original. Returns whether any did.
pub fn undo_spilled(g: &mut Graph, copies: &[V], loc: &[Loc]) -> bool {
    let back: HashMap<V, V> = copies.iter().filter(|&&c| !matches!(loc[c as usize], Loc::Reg(_) | Loc::Xmm(_))).map(|&c| match *g.inst(c) {
        Inst::Copy(x) => (c, x),
        _ => unreachable!("a reload copy"),
    }).collect();
    if back.is_empty() {
        return false;
    }
    // A copy of a copy taken back too reads the first one's source.
    let to = |mut u: V| {
        while let Some(&x) = back.get(&u) {
            u = x;
        }
        u
    };
    for b in 0..g.blocks.len() {
        g.blocks[b].insts.retain(|v| !back.contains_key(v));
        let mut term = std::mem::replace(&mut g.blocks[b].term, Term::None);
        term.map_inputs(to);
        g.blocks[b].term = term;
        for i in 0..g.blocks[b].insts.len() {
            let v = g.blocks[b].insts[i] as usize;
            let mut inst = std::mem::replace(&mut g.values[v].inst, Inst::Fence);
            if !matches!(inst, Inst::Phi(_)) {
                inst.map_inputs(to);
            }
            g.values[v].inst = inst;
        }
    }
    true
}
