//! Bounds checks of masked indices: `a[x & c]` with a constant `c >= 0` keeps the index in
//! `[0, c]`, so the check becomes `c < a.length`, the same for every such access of `a` with
//! that mask (a noise function's permutation table read eight times in a row by `p[i & 255]`):
//! the first one stays and the ones it dominates go, with their deoptimization states. It
//! asks more than the access needs, so an array shorter than the mask deoptimizes where the
//! access would not fail; the interpreter then runs the access, and the trap at that site
//! keeps its check exact in the recompilation (`profile::trapped`).

use super::dom::Dom;
use super::ir::*;

pub fn speculate(g: &mut Graph) {
    let reads = super::thread::reads(g);
    let mask_of = |g: &Graph, i: V| -> Option<i64> {
        let Inst::Bin(BinOp::And, x, y) = *g.inst(g.resolve(i)) else { return None };
        let c = g.constant(g.resolve(y)).or_else(|| g.constant(g.resolve(x)))?;
        (0..=i32::MAX as i64).contains(&c).then_some(c)
    };
    let candidate = |g: &Graph, v: V| -> Option<(V, i64)> {
        if g.resolve(v) != v || reads[v as usize] != 0 {
            return None;
        }
        let Inst::BoundsCheck(arr, idx, ref s) = *g.inst(v) else { return None };
        let c = mask_of(g, idx)?;
        (!crate::jit::profile::trapped(s.method, s.bci as usize)).then_some((g.resolve(arr), c))
    };
    let checks: Vec<(V, V, i64)> = g.blocks.iter().flat_map(|b| b.insts.iter().copied()).filter_map(|v| candidate(g, v).map(|(a, c)| (v, a, c))).collect();
    if checks.is_empty() {
        return;
    }
    // The masks as constants in the entry block, which dominates every use.
    let mut consts: Vec<(i64, V)> = Vec::new();
    for &(_, _, c) in &checks {
        if !consts.iter().any(|&(x, _)| x == c) {
            let k = g.values.len() as V;
            g.values.push(Value { inst: Inst::Const(c), ty: Ty::I });
            // After the parameters, which code generation takes as the block's first values.
            let at = g.blocks[0].insts.iter().take_while(|&&x| matches!(g.values[x as usize].inst, Inst::Param(_) | Inst::Phi(_))).count();
            g.blocks[0].insts.insert(at, k);
            consts.push((c, k));
        }
    }
    let dom = Dom::new(g);
    let index = dom.index(g.blocks.len());
    let mut order: Vec<usize> = (0..g.blocks.len()).filter(|&b| index[b] != usize::MAX).collect();
    order.sort_by_key(|&b| index[b]);
    // The checks kept: (array, mask, block, position in it).
    let mut kept: Vec<(V, i64, usize, usize)> = Vec::new();
    let mut gone: Vec<V> = Vec::new();
    for b in order {
        for pos in 0..g.blocks[b].insts.len() {
            let v = g.blocks[b].insts[pos];
            let Some(&(_, arr, c)) = checks.iter().find(|&&(x, ..)| x == v) else { continue };
            let dominated = kept.iter().any(|&(a, k, kb, kp)| a == arr && k >= c && if kb == b { kp < pos } else { dom.dominates(index[kb], index[b]) });
            if dominated {
                gone.push(v);
                continue;
            }
            let k = consts.iter().find(|&&(x, _)| x == c).expect("a mask constant").1;
            let Inst::BoundsCheck(_, _, s) = g.inst(v).clone() else { unreachable!("a bounds check") };
            g.values[v as usize].inst = Inst::BoundsCheck(arr, k, s);
            kept.push((arr, c, b, pos));
        }
    }
    if !gone.is_empty() {
        for blk in &mut g.blocks {
            blk.insts.retain(|v| !gone.contains(v));
        }
    }
}
