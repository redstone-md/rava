//! Arithmetic whose operands became constants after the graph was built (inlined arguments,
//! phis of one value, fields forwarded, conditions folded), folded late as C2's ideal-graph
//! transforms keep folding: two constants make a constant, and an identity operand (`x + 0`,
//! `x << 0`, `x * 1`, `x & -1`, ...) leaves the other operand. Building folds only what is
//! constant at the bytecode.

use super::ir::*;

pub fn simplify(g: &mut Graph) {
    if g.alias.len() < g.values.len() {
        let n = g.alias.len();
        g.alias.extend(n as V..g.values.len() as V);
    }
    let mut changed = true;
    while changed {
        changed = false;
        let mut gone = Vec::new();
        for b in 0..g.blocks.len() {
            for &v in &g.blocks[b].insts {
                if g.resolve(v) != v {
                    continue;
                }
                let ty = g.ty(v);
                match *g.inst(v) {
                    Inst::Bin(op, a, c) if matches!(ty, Ty::I | Ty::J) => {
                        let (x, y) = (g.constant(g.resolve(a)), g.constant(g.resolve(c)));
                        // Division by zero keeps its instruction (the zero check before it throws).
                        if let (Some(x), Some(y), false) = (x, y, matches!(op, BinOp::Div | BinOp::Rem) && y == Some(0)) {
                            if let Some(k) = super::canon::bin(op, ty, x, y) {
                                g.values[v as usize].inst = Inst::Const(k);
                                changed = true;
                            }
                            continue;
                        }
                        if let Some(same) = identity(op, ty, a, c, x, y) {
                            g.alias[v as usize] = g.resolve(same);
                            gone.push(v);
                            changed = true;
                        }
                    }
                    Inst::Convert(op, a) => {
                        if let Some(k) = g.constant(g.resolve(a)).and_then(|x| super::canon::convert(op, x)) {
                            g.values[v as usize].inst = Inst::Const(k);
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        if !gone.is_empty() {
            for blk in &mut g.blocks {
                blk.insts.retain(|v| !gone.contains(v));
            }
        }
    }
    fold_unsafe_addresses(g);
}

/// Unsafe accesses at `base + ((long) i << k) + c` (an array element, as `ConcurrentHashMap`'s
/// `tabAt` reads one) take `i`, `1 << k` and `c` as their last three operands, for one scaled
/// address (`[base + i * 2^k + c]`) instead of a shift and an add.
fn fold_unsafe_addresses(g: &mut Graph) {
    use crate::jit::intrinsics::Intrinsic;
    for b in 0..g.blocks.len() {
        let mut i = 0;
        while i < g.blocks[b].insts.len() {
            let v = g.blocks[b].insts[i];
            i += 1;
            if g.resolve(v) != v {
                continue;
            }
            let Inst::Intrinsic(k, args, _) = g.inst(v) else { continue };
            let arity = match k {
                Intrinsic::UnsafeGet(..) => 3,
                Intrinsic::UnsafePut(..) => 4,
                Intrinsic::CompareAndSet(_) => 5,
                _ => continue,
            };
            if args.len() != arity {
                continue;
            }
            let Some((x, scale, disp)) = scaled(g, g.resolve(args[2])) else { continue };
            let (s, d) = (g.add(b as B, Inst::Const(scale), Ty::J), g.add(b as B, Inst::Const(disp), Ty::J));
            // The constants go before the access.
            let blk = &mut g.blocks[b].insts;
            blk.truncate(blk.len() - 2);
            blk.splice(i - 1..i - 1, [s, d]);
            i += 2;
            if let Inst::Intrinsic(_, args, _) = &mut g.values[v as usize].inst {
                args[2] = x;
                args.extend([s, d]);
            }
        }
    }
}

/// `(x, 1 << k, c)` for an offset `(x << k) + c` with `k` up to 3 and `c` an `i32`.
fn scaled(g: &Graph, off: V) -> Option<(V, i64, i64)> {
    let Inst::Bin(BinOp::Add, a, b) = *g.inst(off) else { return None };
    let (a, b) = (g.resolve(a), g.resolve(b));
    let (shifted, c) = match (g.constant(a), g.constant(b)) {
        (None, Some(c)) => (a, c),
        (Some(c), None) => (b, c),
        _ => return None,
    };
    i32::try_from(c).ok()?;
    let Inst::Bin(BinOp::Shl, x, k) = *g.inst(shifted) else { return None };
    let k = g.constant(g.resolve(k)).filter(|k| (0..=3).contains(k))?;
    (g.ty(shifted) == Ty::J).then(|| (g.resolve(x), 1 << k, c))
}

/// The operand `a op b` equals, when the other is an identity constant of `op`.
fn identity(op: BinOp, ty: Ty, a: V, b: V, x: Option<i64>, y: Option<i64>) -> Option<V> {
    let bits = if ty == Ty::I { 31 } else { 63 };
    let ones = |c: i64| if ty == Ty::I { c as i32 == -1 } else { c == -1 };
    match (op, x, y) {
        (BinOp::Add | BinOp::Sub | BinOp::Or | BinOp::Xor, _, Some(0)) => Some(a),
        (BinOp::Add | BinOp::Or | BinOp::Xor, Some(0), _) => Some(b),
        (BinOp::Shl | BinOp::Shr | BinOp::Ushr, _, Some(c)) if c & bits == 0 => Some(a),
        (BinOp::Mul, _, Some(1)) => Some(a),
        (BinOp::Mul, Some(1), _) => Some(b),
        (BinOp::And, _, Some(c)) if ones(c) => Some(a),
        (BinOp::And, Some(c), _) if ones(c) => Some(b),
        _ => None,
    }
}
