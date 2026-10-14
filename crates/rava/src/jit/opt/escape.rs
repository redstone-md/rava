//! Scalar replacement of non-escaping allocations (C2's): an object whose identity is never
//! observed (it is only the base of its own field loads and stores, and appears in
//! deoptimization states) does not need to exist. Each of its fields becomes an SSA variable:
//! the allocation defines every field as zero, a store defines its field anew, a load reads the
//! current definition, and blocks where definitions merge get a phi. The allocation, stores and
//! loads disappear; a field updated in a loop (an iterator's cursor) lives in a register.
//!
//! A state that mentions the object gets a `Virtual` snapshot of the field values at that
//! point; if the compiled code deoptimizes there, the interpreter rebuilds the object from them
//! (`deopt.rs`), once per state however many slots hold it.
//!
//! Not replaced: arrays (they can raise `NegativeArraySizeException`, a guest-visible effect),
//! an object stored into another object's field (no nesting), or one locked, passed, returned,
//! merged by a phi or compared with one. An identity test against any other value folds:
//! nothing else can hold an object that never escaped.
//!
//! On by default (`RAVA_SCALAR_REPLACE=0` turns it off).

use super::ir::*;
use crate::klass::Klass;
use crate::types::BasicType;
use std::collections::HashMap;
use std::rc::Rc;

pub fn eliminate(g: &mut Graph) {
    if env_once!("RAVA_SCALAR_REPLACE").is_some_and(|v| v == "0") || !g.values.iter().any(|v| matches!(v.inst, Inst::New(..))) {
        return;
    }
    let candidates = find_candidates(g);
    if candidates.is_empty() {
        return;
    }
    if g.alias.len() < g.values.len() {
        let n = g.alias.len();
        g.alias.extend(n as V..g.values.len() as V);
    }
    fold_identity_tests(g, &candidates);
    Replace::new(g, candidates).run();
}

/// For a reference test of an allocation `n` against `x`: `(n, x)`, when `x` is not a phi
/// (that could merge `n` with something else).
fn identity_test(g: &Graph, a: V, b: V) -> Option<(V, V)> {
    let (a, b) = (g.resolve(a), g.resolve(b));
    let is_new = |v: V| matches!(g.values[v as usize].inst, Inst::New(..));
    let is_phi = |v: V| matches!(g.values[v as usize].inst, Inst::Phi(_));
    match (is_new(a), is_new(b)) {
        (true, _) if !is_phi(b) => Some((a, b)),
        (_, true) if !is_phi(a) => Some((b, a)),
        _ => None,
    }
}

/// Tests of a replaced allocation's identity become constant: equal to itself only, as no
/// other value can hold an object that never escaped.
fn fold_identity_tests(g: &mut Graph, candidates: &HashMap<V, Candidate>) {
    for b in 0..g.blocks.len() {
        let Term::If { ty: Ty::A, a, b: other, .. } = g.blocks[b].term else { continue };
        let Some((n, x)) = identity_test(g, a, other) else { continue };
        if !candidates.contains_key(&n) {
            continue;
        }
        let same = x == n;
        let (zero, one) = (g.add(b as B, Inst::Const(0), Ty::I), g.add(b as B, Inst::Const(1), Ty::I));
        if let Term::If { ty, a, b: rhs, .. } = &mut g.blocks[b].term {
            (*ty, *a, *rhs) = (Ty::I, zero, if same { zero } else { one });
        }
    }
}

/// The phis something reads: a non-phi instruction, a terminator, a state, or a phi read.
fn used_phis(g: &Graph) -> Vec<bool> {
    let mut used = vec![false; g.values.len()];
    let mut work = Vec::new();
    let read = |x: V, used: &mut Vec<bool>, work: &mut Vec<V>| {
        let x = g.resolve(x);
        if !used[x as usize] {
            used[x as usize] = true;
            if matches!(g.values[x as usize].inst, Inst::Phi(_)) {
                work.push(x);
            }
        }
    };
    let mut states = Vec::new();
    for blk in g.blocks.iter().filter(|b| !matches!(b.term, Term::None)) {
        for &v in &blk.insts {
            if g.resolve(v) != v {
                continue;
            }
            let inst = &g.values[v as usize].inst;
            if !matches!(inst, Inst::Phi(_)) {
                inst.inputs().into_iter().for_each(|x| read(x, &mut used, &mut work));
            }
            if let Some(s) = inst.state() {
                states.push(s.clone());
            }
        }
        blk.term.inputs().into_iter().for_each(|x| read(x, &mut used, &mut work));
        if let Term::Deopt(s) = &blk.term {
            states.push(s.clone());
        }
    }
    fn slots(s: &State, out: &mut Vec<V>) {
        out.extend(s.locals.iter().chain(s.stack.iter()).flatten());
        if let Some(c) = &s.caller {
            slots(c, out);
        }
    }
    let mut xs = Vec::new();
    for s in &states {
        slots(s, &mut xs);
    }
    xs.into_iter().for_each(|x| read(x, &mut used, &mut work));
    while let Some(p) = work.pop() {
        if let Inst::Phi(inputs) = &g.values[p as usize].inst {
            inputs.clone().into_iter().for_each(|x| read(x, &mut used, &mut work));
        }
    }
    used
}

/// A replaceable allocation: its class and the type of each field it stores.
struct Candidate {
    klass: &'static Klass,
    fields: Vec<(u32, BasicType)>,
}

/// The allocations used only as the base of their own field accesses (and in states).
fn find_candidates(g: &Graph) -> HashMap<V, Candidate> {
    let used = used_phis(g);
    let mut escapes = vec![false; g.values.len()];
    let mut fields: HashMap<V, Vec<(u32, BasicType)>> = HashMap::new();
    for blk in &g.blocks {
        if matches!(blk.term, Term::None) {
            continue;
        }
        for &v in &blk.insts {
            // An instruction aliased away (a trivial phi, a forwarded load) is dead: its operands
            // are not real uses.
            if g.resolve(v) != v {
                continue;
            }
            match g.values[v as usize].inst {
                Inst::StoreField(base, off, bt, val) => {
                    let base = g.resolve(base);
                    let f = fields.entry(base).or_default();
                    match f.iter().find(|&&(o, _)| o == off) {
                        Some(&(_, t)) if t != bt => escapes[base as usize] = true,
                        Some(_) => {}
                        None => f.push((off, bt)),
                    }
                    escapes[g.resolve(val) as usize] = true;
                }
                Inst::LoadField(..) => {}
                // A phi nothing reads (a dead local merged at a loop header) uses nothing.
                Inst::Phi(_) if !used[v as usize] => {}
                _ => g.values[v as usize].inst.inputs().into_iter().for_each(|x| escapes[g.resolve(x) as usize] = true),
            }
        }
        match blk.term {
            // An allocation compared by identity with anything but a phi (which could carry it)
            // keeps it from escaping: nothing else can hold it unless it escapes some other way,
            // and then it stays (`fold_identity_tests` folds only for the replaced ones).
            Term::If { ty: Ty::A, a, b, .. } if identity_test(g, a, b).is_some() => {}
            _ => blk.term.inputs().into_iter().for_each(|x| escapes[g.resolve(x) as usize] = true),
        }
    }
    let mut out = HashMap::new();
    for (v, x) in g.values.iter().enumerate() {
        let Inst::New(klass, _) = x.inst else { continue };
        if g.resolve(v as V) != v as V || escapes[v] {
            continue;
        }
        let mut f = fields.remove(&(v as V)).unwrap_or_default();
        f.sort_by_key(|&(o, _)| o);
        out.insert(v as V, Candidate { klass, fields: f });
    }
    out
}

/// A state and the field values of each candidate it mentions.
type SnapKey = (*const State, Vec<(V, Vec<V>)>);

/// The current value of each field of each candidate, along one block.
type Current = HashMap<(V, u32), V>;

struct Replace<'g> {
    g: &'g mut Graph,
    cands: HashMap<V, Candidate>,
    /// Zero of each type, made on demand (never placed: constants need no block).
    zeros: HashMap<Ty, V>,
    /// The phis this pass made, by block: (phi, candidate, offset).
    phis: Vec<(B, V, V, u32)>,
    /// Snapshots made per original state, so instructions sharing a state share its rewrite.
    rewritten: HashMap<SnapKey, Rc<State>>,
}

impl<'g> Replace<'g> {
    fn new(g: &'g mut Graph, cands: HashMap<V, Candidate>) -> Self {
        Replace { g, cands, zeros: HashMap::new(), phis: Vec::new(), rewritten: HashMap::new() }
    }

    fn fresh(&mut self, inst: Inst, ty: Ty) -> V {
        self.g.values.push(Value { inst, ty });
        let v = self.g.values.len() as V - 1;
        self.g.alias.push(v);
        v
    }

    fn zero(&mut self, ty: Ty) -> V {
        if let Some(&z) = self.zeros.get(&ty) {
            return z;
        }
        let z = self.fresh(Inst::Const(0), ty);
        self.zeros.insert(ty, z);
        z
    }

    fn run(mut self) {
        let dom = super::dom::Dom::new(self.g);
        let index = dom.index(self.g.blocks.len());
        // The block of each candidate's allocation: its fields are defined in the blocks that
        // block dominates.
        let mut home: HashMap<V, usize> = HashMap::new();
        for (b, blk) in self.g.blocks.iter().enumerate() {
            for &v in &blk.insts {
                if self.cands.contains_key(&v) && index[b] != usize::MAX {
                    home.insert(v, index[b]);
                }
            }
        }
        self.cands.retain(|v, _| home.contains_key(v));
        if self.cands.is_empty() {
            return;
        }
        let keys: Vec<(V, u32, BasicType)> = {
            let mut k: Vec<_> = self.cands.iter().flat_map(|(&v, c)| c.fields.iter().map(move |&(o, t)| (v, o, t))).collect();
            k.sort_by_key(|&(v, o, _)| (v, o));
            k
        };
        let mut end: Vec<Option<Current>> = vec![None; self.g.blocks.len()];
        for (i, &b) in dom.rpo.clone().iter().enumerate() {
            // Values at the block's entry: through its single predecessor, or merged by a phi
            // (inputs filled once every predecessor is done).
            let preds = self.g.blocks[b as usize].preds.clone();
            let mut cur: Current = HashMap::new();
            for &(v, off, bt) in &keys {
                let h = home[&v];
                if i == h || !dom.dominates(h, i) {
                    continue;
                }
                let reachable: Vec<B> = preds.iter().copied().filter(|&p| index[p as usize] != usize::MAX).collect();
                let x = if reachable.len() == 1 && preds.len() == 1 {
                    end[reachable[0] as usize].as_ref().and_then(|e| e.get(&(v, off)).copied())
                } else {
                    None
                };
                let x = match x {
                    Some(x) => x,
                    None => {
                        let phi = self.fresh(Inst::Phi(Vec::new()), Ty::of(bt));
                        self.phis.push((b, phi, v, off));
                        phi
                    }
                };
                cur.insert((v, off), x);
            }
            self.walk(b, &mut cur);
            end[b as usize] = Some(cur);
        }
        // Phi inputs, in predecessor order; an unreachable predecessor contributes a zero.
        for k in 0..self.phis.len() {
            let (b, phi, v, off) = self.phis[k];
            let ty = self.g.ty(phi);
            let preds = self.g.blocks[b as usize].preds.clone();
            let inputs: Vec<V> = preds.iter().map(|&p| end[p as usize].as_ref().and_then(|e| e.get(&(v, off)).copied()).unwrap_or_else(|| self.zero(ty))).collect();
            self.g.values[phi as usize].inst = Inst::Phi(inputs);
        }
        self.place_live_phis();
    }

    /// Rewrites block `b`: drops the candidates' allocations, stores and loads (loads become
    /// aliases of the current values), and gives states that mention a candidate snapshots.
    fn walk(&mut self, b: B, cur: &mut Current) {
        // The state a loop's entry deopts to (`loop_entry`), as of the header's start.
        if let Some(s) = self.g.loop_entry.get(&b).cloned() {
            if let Some(r) = self.snapshot(&s, cur) {
                self.g.loop_entry.insert(b, r);
            }
        }
        let insts = std::mem::take(&mut self.g.blocks[b as usize].insts);
        let mut kept = Vec::with_capacity(insts.len());
        for v in insts {
            if self.g.resolve(v) != v {
                kept.push(v);
                continue;
            }
            match self.g.values[v as usize].inst.clone() {
                // An unread phi (`used_phis`) merging a replaced object: gone with it.
                Inst::Phi(inputs) if inputs.iter().any(|&x| self.cands.contains_key(&self.g.resolve(x))) => continue,
                Inst::New(..) if self.cands.contains_key(&v) => {
                    let fields = self.cands[&v].fields.clone();
                    for (off, bt) in fields {
                        let z = self.zero(Ty::of(bt));
                        cur.insert((v, off), z);
                    }
                    continue;
                }
                Inst::StoreField(base, off, bt, val) if self.cands.contains_key(&self.g.resolve(base)) => {
                    let base = self.g.resolve(base);
                    let val = self.g.resolve(val);
                    let one = (bt == BasicType::Boolean).then(|| self.fresh(Inst::Const(1), Ty::I));
                    let val = match narrowing(bt, val, one) {
                        Some(inst) => {
                            let n = self.fresh(inst, Ty::I);
                            kept.push(n);
                            n
                        }
                        None => val,
                    };
                    cur.insert((base, off), val);
                    continue;
                }
                Inst::LoadField(base, off, bt) if self.cands.contains_key(&self.g.resolve(base)) => {
                    let base = self.g.resolve(base);
                    let x = if off == 0 && bt == BasicType::Int {
                        let k = self.cands[&base].klass;
                        self.fresh(Inst::Const(k as *const Klass as i64), Ty::J)
                    } else {
                        // A field never stored reads zero.
                        match cur.get(&(base, off)) {
                            Some(&x) => x,
                            None => self.zero(Ty::of(bt)),
                        }
                    };
                    self.g.alias[v as usize] = x;
                    continue;
                }
                _ => {}
            }
            if let Some(s) = self.g.values[v as usize].inst.state().cloned() {
                if let Some(r) = self.snapshot(&s, cur) {
                    self.g.values[v as usize].inst.set_state(r);
                }
            }
            kept.push(v);
        }
        self.g.blocks[b as usize].insts = kept;
        if let Term::Deopt(s) = self.g.blocks[b as usize].term.clone() {
            if let Some(r) = self.snapshot(&s, cur) {
                self.g.blocks[b as usize].term = Term::Deopt(r);
            }
        }
    }

    /// `s` with each candidate it mentions (callers included) replaced by one snapshot of the
    /// current field values, or None if it mentions none. Instructions sharing a state with the
    /// same field values share the rewrite.
    fn snapshot(&mut self, s: &Rc<State>, cur: &Current) -> Option<Rc<State>> {
        let mut found = Vec::new();
        mentioned(self.g, s, &self.cands, &mut found);
        if found.is_empty() {
            return None;
        }
        found.sort_unstable();
        found.dedup();
        let mut snaps: Vec<(V, Vec<(u32, BasicType, V)>)> = Vec::new();
        for v in found {
            let spec = self.cands[&v].fields.clone();
            let fields = spec.into_iter().map(|(off, bt)| (off, bt, cur.get(&(v, off)).copied().unwrap_or_else(|| self.zero(Ty::of(bt))))).collect();
            snaps.push((v, fields));
        }
        let key = (Rc::as_ptr(s), snaps.iter().map(|(v, f)| (*v, f.iter().map(|x| x.2).collect::<Vec<V>>())).collect::<Vec<_>>());
        if let Some(r) = self.rewritten.get(&key) {
            return Some(r.clone());
        }
        let mut map: HashMap<V, V> = HashMap::new();
        for (v, fields) in snaps {
            let w = self.fresh(Inst::Const(0), Ty::A);
            self.g.virtuals.insert(w, Virtual { klass: self.cands[&v].klass, fields });
            map.insert(v, w);
        }
        let r = remap(self.g, s, &map);
        self.rewritten.insert(key, r.clone());
        Some(r)
    }

    /// Keeps the phis something reads (instructions, terminators, states through snapshots,
    /// other kept phis), placing them first in their blocks; the rest are dropped.
    fn place_live_phis(&mut self) {
        let mine: HashMap<V, usize> = self.phis.iter().enumerate().map(|(i, &(_, p, _, _))| (p, i)).collect();
        let mut live = vec![false; self.phis.len()];
        let mut work = Vec::new();
        let mark = |x: V, g: &Graph, live: &mut Vec<bool>, work: &mut Vec<V>| {
            if let Some(&i) = mine.get(&g.resolve(x)) {
                if !live[i] {
                    live[i] = true;
                    work.push(g.resolve(x));
                }
            }
        };
        for blk in &self.g.blocks {
            for &v in &blk.insts {
                if self.g.resolve(v) != v {
                    continue;
                }
                for x in self.g.values[v as usize].inst.inputs() {
                    mark(x, self.g, &mut live, &mut work);
                }
                if let Some(s) = self.g.values[v as usize].inst.state() {
                    let mut reads = Vec::new();
                    super::liveness::state_values(self.g, s, &mut reads);
                    reads.into_iter().for_each(|x| mark(x, self.g, &mut live, &mut work));
                }
            }
            for x in blk.term.inputs() {
                mark(x, self.g, &mut live, &mut work);
            }
            if let Term::Deopt(s) = &blk.term {
                let mut reads = Vec::new();
                super::liveness::state_values(self.g, s, &mut reads);
                reads.into_iter().for_each(|x| mark(x, self.g, &mut live, &mut work));
            }
        }
        while let Some(p) = work.pop() {
            for x in self.g.values[p as usize].inst.inputs() {
                mark(x, self.g, &mut live, &mut work);
            }
        }
        let mut by_block: HashMap<B, Vec<V>> = HashMap::new();
        for (i, &(b, p, _, _)) in self.phis.iter().enumerate() {
            if live[i] {
                by_block.entry(b).or_default().push(p);
            }
        }
        for (b, ps) in by_block {
            let insts = &mut self.g.blocks[b as usize].insts;
            insts.splice(0..0, ps);
        }
        // Trivial phis (a loop that never stores the field) become aliases.
        super::phis::fold_trivial(self.g);
    }
}

/// What a store of `val` into a field of this type leaves there (`putfield` narrows), or None
/// when the value is stored unchanged. `one` is the constant 1 for a boolean's mask.
fn narrowing(bt: BasicType, val: V, one: Option<V>) -> Option<Inst> {
    match bt {
        BasicType::Byte => Some(Inst::Convert(0x91, val)),
        BasicType::Char => Some(Inst::Convert(0x92, val)),
        BasicType::Short => Some(Inst::Convert(0x93, val)),
        BasicType::Boolean => Some(Inst::Bin(BinOp::And, val, one.expect("the mask"))),
        _ => None,
    }
}

/// The candidates `s` (callers included) holds in a slot.
fn mentioned(g: &Graph, s: &State, cands: &HashMap<V, Candidate>, out: &mut Vec<V>) {
    for &x in s.locals.iter().chain(s.stack.iter()).flatten() {
        let x = g.resolve(x);
        if cands.contains_key(&x) {
            out.push(x);
        }
    }
    if let Some(c) = &s.caller {
        mentioned(g, c, cands, out);
    }
}

/// `s` with slots holding a key of `map` (resolved) holding its value instead.
fn remap(g: &Graph, s: &Rc<State>, map: &HashMap<V, V>) -> Rc<State> {
    let caller = s.caller.as_ref().map(|c| remap(g, c, map));
    let m = |xs: &[Option<V>]| xs.iter().map(|&x| x.map(|x| map.get(&g.resolve(x)).copied().unwrap_or(x))).collect();
    Rc::new(State { method: s.method, bci: s.bci, locals: m(&s.locals), stack: m(&s.stack), caller, frame: s.frame })
}
