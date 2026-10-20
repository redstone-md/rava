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
                    // A shift reads its count's low 5 or 6 bits, which a narrowing conversion
                    // (l2i, i2b, i2c, i2s) keeps: the count is the conversion's input.
                    Inst::Bin(op @ (BinOp::Shl | BinOp::Shr | BinOp::Ushr), a, c)
                        if matches!(ty, Ty::I | Ty::J) && matches!(*g.inst(g.resolve(c)), Inst::Convert(0x88 | 0x91 | 0x92 | 0x93, _)) =>
                    {
                        let Inst::Convert(_, y) = *g.inst(g.resolve(c)) else { unreachable!() };
                        g.values[v as usize].inst = Inst::Bin(op, a, g.resolve(y));
                        changed = true;
                    }
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
                            continue;
                        }
                        // `(x + c1) + c2` is `x + (c1 + c2)` in wrapping arithmetic, and `x` when
                        // that is 0: the index `(i - 1) + 1` of a loop stepping down is `i`.
                        if let Some((inner, k)) = offset(g, v) {
                            if let Some((base, k0)) = offset(g, inner) {
                                let sum = wrap(ty, k0.wrapping_add(k));
                                if sum == 0 {
                                    g.alias[v as usize] = g.resolve(base);
                                    gone.push(v);
                                } else {
                                    g.values.push(Value { inst: Inst::Const(sum), ty });
                                    let c = g.values.len() as V - 1;
                                    g.alias.push(c);
                                    g.values[v as usize].inst = Inst::Bin(BinOp::Add, base, c);
                                }
                                changed = true;
                            }
                        }
                    }
                    Inst::Convert(op, a) => {
                        if let Some(k) = g.constant(g.resolve(a)).and_then(|x| super::canon::convert(op, x)) {
                            g.values[v as usize].inst = Inst::Const(k);
                            changed = true;
                        }
                    }
                    // A byte, char or short store keeps the value's low bits, which narrowing
                    // conversions to as many bits or more do not change: the conversion goes.
                    Inst::StoreField(_, _, bt, x) | Inst::StoreIndexed(_, _, bt, x) if narrowed(g, bt, x).is_some() => {
                        let y = narrowed(g, bt, x).unwrap();
                        match &mut g.values[v as usize].inst {
                            Inst::StoreField(_, _, _, x) | Inst::StoreIndexed(_, _, _, x) => *x = y,
                            _ => unreachable!(),
                        }
                        changed = true;
                    }
                    Inst::Intrinsic(crate::jit::intrinsics::Intrinsic::UnsafePut(bt, false), ref args, _) if args.len() > 3 && narrowed(g, bt, args[3]).is_some() => {
                        let y = narrowed(g, bt, args[3]).unwrap();
                        if let Inst::Intrinsic(_, args, _) = &mut g.values[v as usize].inst {
                            args[3] = y;
                        }
                        changed = true;
                    }
                    // A choice of two constants compared with a constant is the inner choice's
                    // condition choosing between this choice's values (a boolean method's
                    // result tested again by its caller).
                    Inst::Select(cmp, Ty::I | Ty::J, x, k, t, f) if inner_choice(g, x, k).is_some() => {
                        let (c2, ty2, a2, b2, xt, xf, k) = inner_choice(g, x, k).unwrap();
                        let cty = g.ty(g.resolve(x));
                        let pick = |c: i64| if super::canon::branch(cmp, cty, c, k) { t } else { f };
                        g.values[v as usize].inst = Inst::Select(c2, ty2, a2, b2, pick(xt), pick(xf));
                        changed = true;
                    }
                    // `x * 1.0` and `x / 1.0` are `x` exactly (a NaN stays NaN, -0 stays -0), as
                    // C2 folds them: a vector's literal 1 component through a matrix transform.
                    Inst::Bin(op @ (BinOp::Mul | BinOp::Div), a, c) if matches!(ty, Ty::F | Ty::D) => {
                        let one = |x: V| g.constant(g.resolve(x)) == Some(if ty == Ty::F { 1.0f32.to_bits() as i64 } else { 1.0f64.to_bits() as i64 });
                        let same = if one(c) { Some(a) } else if op == BinOp::Mul && one(a) { Some(c) } else { None };
                        if let Some(same) = same {
                            g.alias[v as usize] = g.resolve(same);
                            gone.push(v);
                            changed = true;
                        }
                    }
                    // The bits of a constant float (`Math.abs(x) == 1` on an inlined argument).
                    Inst::Intrinsic(crate::jit::intrinsics::Intrinsic::Bits(_), ref args, _) if g.constant(g.resolve(args[0])).is_some() => {
                        let x = g.constant(g.resolve(args[0])).unwrap();
                        g.values[v as usize].inst = Inst::Const(match ty {
                            Ty::I => x as u32 as i32 as i64,
                            Ty::F => x as u32 as i64,
                            _ => x,
                        });
                        changed = true;
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
    fold_constant_branches(g);
}

/// `v` as `x + k` when it adds a constant to `x` or subtracts one from it (aliases resolved).
fn offset(g: &Graph, v: V) -> Option<(V, i64)> {
    let v = g.resolve(v);
    let ty = g.ty(v);
    if !matches!(ty, Ty::I | Ty::J) {
        return None;
    }
    match *g.inst(v) {
        Inst::Bin(BinOp::Add, a, c) => match (g.constant(g.resolve(a)), g.constant(g.resolve(c))) {
            (None, Some(k)) => Some((g.resolve(a), k)),
            (Some(k), None) => Some((g.resolve(c), k)),
            _ => None,
        },
        Inst::Bin(BinOp::Sub, a, c) => g.constant(g.resolve(c)).filter(|_| g.constant(g.resolve(a)).is_none()).map(|k| (g.resolve(a), k.wrapping_neg())),
        _ => None,
    }
}

/// `k` wrapped to the width of `ty` (an int's constants are kept sign-extended).
fn wrap(ty: Ty, k: i64) -> i64 {
    if ty == Ty::I { k as i32 as i64 } else { k }
}

/// Branches between two constants (operands folded here or after the builder's tests) go one
/// way.
fn fold_constant_branches(g: &mut Graph) {
    let mut folded = false;
    for b in 0..g.blocks.len() {
        // A switch on a constant (a pattern switch's restart index, once inlined) goes to its
        // case.
        if let Term::Switch { key, cases, default } = &g.blocks[b].term {
            let Some(c) = g.constant(g.resolve(*key)) else { continue };
            let keep = cases.iter().find(|&&(k, _)| k as i64 == c as i32 as i64).map_or(*default, |&(_, t)| t);
            let mut gone = g.blocks[b].term.successors();
            gone.sort_unstable();
            gone.dedup();
            gone.retain(|&t| t != keep);
            for t in gone {
                super::conds::fold_branch(g, b as B, keep, t);
                folded = true;
            }
            continue;
        }
        let Term::If { cmp, ty: ty @ (Ty::I | Ty::J | Ty::A), a, b: y, then, other, .. } = g.blocks[b].term else { continue };
        let (Some(x), Some(z)) = (g.constant(g.resolve(a)), g.constant(g.resolve(y))) else { continue };
        if then == other {
            continue;
        }
        let (keep, gone) = if super::canon::branch(cmp, ty, x, z) { (then, other) } else { (other, then) };
        super::conds::fold_branch(g, b as B, keep, gone);
        folded = true;
    }
    if folded {
        super::conds::remove_unreachable(g);
    }
}

/// `simplify`, after range check elimination (which matches `ArrayLength` instructions): then also
/// the length of an array allocated here is its size operand (non-negative once allocated), a
/// constant array's length is a constant, and a bounds check of a constant index against a
/// known length goes.
pub fn simplify_late(g: &mut Graph) {
    let mut gone = Vec::new();
    for b in 0..g.blocks.len() {
        for &v in &g.blocks[b].insts {
            if g.resolve(v) != v {
                continue;
            }
            match *g.inst(v) {
                Inst::ArrayLength(a) => match length_of(g, a) {
                    Some(Len::Value(n)) => {
                        g.alias[v as usize] = g.resolve(n);
                        gone.push(v);
                    }
                    Some(Len::Const(n)) => {
                        g.values[v as usize].inst = Inst::Const(n);
                    }
                    None => {}
                },
                Inst::BoundsCheck(a, i, _) => {
                    let len = match length_of(g, a) {
                        Some(Len::Const(n)) => Some(n),
                        Some(Len::Value(n)) => g.constant(g.resolve(n)),
                        None => None,
                    };
                    if let (Some(n), Some(i)) = (len, g.constant(g.resolve(i))) {
                        if (0..n).contains(&i) {
                            gone.push(v);
                            }
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
    // Unrolled copies read constant counters (`0 + 1`, `a + 0`): fold them too.
    simplify(g);
}

enum Len {
    Value(V),
    Const(i64),
}

/// The length of array `a` when known: an allocation's size operand, or a constant array's.
fn length_of(g: &Graph, a: V) -> Option<Len> {
    let mut r = g.resolve(a);
    if let Inst::CheckCast(x, ..) = *g.inst(r) {
        r = g.resolve(x);
    }
    match *g.inst(r) {
        Inst::NewArray(_, n, _) => Some(Len::Value(n)),
        Inst::Const(o) if o != 0 && g.ty(r) == Ty::A && !matches!(crate::jit::fold::class_of(o as usize).kind, crate::klass::KlassKind::Instance) => {
            Some(Len::Const(crate::jit::fold::array_length(o as usize) as i64))
        }
        _ => None,
    }
}

/// Unsafe accesses at `base + ((long) i << k) + c` (an array element, as `ConcurrentHashMap`'s
/// `tabAt` reads one) take `i`, `1 << k` and `c` as their last three operands, for one scaled
/// address (`[base + i * 2^k + c]`) instead of a shift and an add. A primitive access with a
/// null base (off-heap memory) at `a + (i << k)` takes `a` as its base.
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
            let (base, off) = (args[1], g.resolve(args[2]));
            // A null base (off-heap memory) with an offset `a + (x << k)`: `a` is the base.
            let primitive = !matches!(k, Intrinsic::UnsafeGet(crate::types::BasicType::Reference, _) | Intrinsic::UnsafePut(crate::types::BasicType::Reference, _) | Intrinsic::CompareAndSet(_));
            let (x, scale, disp, new_base) = match scaled(g, off) {
                Some((x, scale, disp)) => (x, scale, disp, None),
                None => match based(g, off).filter(|_| primitive && g.constant_value(base) == Some(0)) {
                    Some((a, x, scale)) => (x, scale, 0, Some(a)),
                    None => continue,
                },
            };
            let (s, d) = (g.add(b as B, Inst::Const(scale), Ty::J), g.add(b as B, Inst::Const(disp), Ty::J));
            // The constants go before the access.
            let blk = &mut g.blocks[b].insts;
            blk.truncate(blk.len() - 2);
            blk.splice(i - 1..i - 1, [s, d]);
            i += 2;
            if let Inst::Intrinsic(_, args, _) = &mut g.values[v as usize].inst {
                if let Some(a) = new_base {
                    args[1] = a;
                }
                args[2] = x;
                args.extend([s, d]);
            }
        }
    }
}

/// `(x, 1 << k, c)` for an offset `(x << k) + c` with `k` up to 3 and `c` an `i32` (`x + c`
/// with a scale of 1).
fn scaled(g: &Graph, off: V) -> Option<(V, i64, i64)> {
    let Inst::Bin(BinOp::Add, a, b) = *g.inst(off) else { return None };
    let (a, b) = (g.resolve(a), g.resolve(b));
    let (shifted, c) = match (g.constant(a), g.constant(b)) {
        (None, Some(c)) => (a, c),
        (Some(c), None) => (b, c),
        _ => return None,
    };
    i32::try_from(c).ok()?;
    if g.ty(shifted) != Ty::J {
        return None;
    }
    let (x, s) = shift_of(g, shifted).unwrap_or((shifted, 1));
    Some((x, s, c))
}

/// `(a, x, 1 << k)` for an offset `a + (x << k)` with `k` up to 3 (`a + x` with a scale of 1),
/// neither a constant: an absolute address with a base register.
fn based(g: &Graph, off: V) -> Option<(V, V, i64)> {
    let Inst::Bin(BinOp::Add, a, b) = *g.inst(off) else { return None };
    let (a, b) = (g.resolve(a), g.resolve(b));
    if g.ty(off) != Ty::J || g.constant(a).is_some() || g.constant(b).is_some() {
        return None;
    }
    match (shift_of(g, a), shift_of(g, b)) {
        (_, Some((x, s))) => Some((a, x, s)),
        (Some((x, s)), None) => Some((b, x, s)),
        (None, None) => Some((a, b, 1)),
    }
}

/// `(x, 1 << k)` for a long `x << k` with `k` up to 3.
fn shift_of(g: &Graph, v: V) -> Option<(V, i64)> {
    let Inst::Bin(BinOp::Shl, x, k) = *g.inst(v) else { return None };
    let k = g.constant(g.resolve(k)).filter(|k| (0..=3).contains(k))?;
    (g.ty(v) == Ty::J).then(|| (g.resolve(x), 1 << k))
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

/// The input of a narrowing conversion `x` whose low bits a store of `bt` keeps unchanged.
fn narrowed(g: &Graph, bt: crate::types::BasicType, x: V) -> Option<V> {
    use crate::types::BasicType;
    let Inst::Convert(op, y) = *g.inst(g.resolve(x)) else { return None };
    // i2b keeps 8 bits, i2c and i2s 16.
    let ok = match bt {
        BasicType::Byte => matches!(op, 0x91 | 0x92 | 0x93),
        BasicType::Char | BasicType::Short => matches!(op, 0x92 | 0x93),
        _ => false,
    };
    ok.then(|| g.resolve(y))
}

/// `x` as a choice between two constants and `k` a constant: (the choice's condition, its
/// operand type and operands, its two constants, `k`).
fn inner_choice(g: &Graph, x: V, k: V) -> Option<(Cmp, Ty, V, V, i64, i64, i64)> {
    let k = g.constant(g.resolve(k))?;
    let Inst::Select(c2, ty2, a2, b2, t, f) = *g.inst(g.resolve(x)) else { return None };
    Some((c2, ty2, a2, b2, g.constant(g.resolve(t))?, g.constant(g.resolve(f))?, k))
}
