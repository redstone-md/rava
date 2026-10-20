//! Estimated executions of each instruction per entry of its method, from first-tier branch
//! profiles, so inlining can tell hot call sites from cold ones as C2 does with its call-site
//! counts: a rarely taken path that inlines everything it calls bloats the code around the hot
//! one.
//!
//! Wu and Larus's frequency propagation over the bytecode's basic blocks: branch probabilities
//! come from profiles (an even split without one); each loop, innermost first, gets its cyclic
//! probability (how often its back edges bring control back per entry, at most 95 %: about 20
//! iterations), which scales its header's frequency as the enclosing code is propagated.

use crate::jit::analysis::{insn_len, successors};
use crate::method::Method;

/// Branch executions below which the profile is not trusted.
const MIN_PROFILE: u32 = 16;
const MAX_CYCLIC: f32 = 0.95;
/// Bound on the cyclic probability exits are scaled by (a loop measured to never exit).
const MAX_EXIT_CYCLIC: f32 = 0.9999;
/// The least probability a profiled branch direction gets (`out_edges`): with it a loop that
/// never exited by its profile still hands its entry flow on (`MAX_EXIT_CYCLIC`).
const MIN_PROB: f32 = 1e-4;

struct Blocks {
    starts: Vec<usize>,
    /// Outgoing (block, probability) edges.
    succ: Vec<Vec<(usize, f32)>>,
    preds: Vec<Vec<usize>>,
}

/// Frequency of each instruction (0 where unreachable), relative to the method's entry.
/// `taken`: the probability a conditional branch jumps, where the caller knows it better than`n/// the method's own profile (a lambda form's branch on one handle's `profileBoolean` counts).
pub fn frequencies(m: &'static Method, code: &[u8], live: impl Fn(usize) -> bool, taken: &dyn Fn(usize) -> Option<f32>) -> Vec<f32> {
    let bl = blocks(m, code, &live, taken);
    let n = bl.starts.len();
    if n == 0 {
        return vec![0.0; code.len()];
    }
    // Depth-first search from the entry: reverse postorder and back edges (to a block still on
    // the stack), whose targets head loops.
    let (mut rpo, mut state, mut stack) = (Vec::with_capacity(n), vec![0u8; n], vec![(0usize, 0usize)]);
    let mut back = vec![Vec::new(); n];
    state[0] = 1;
    while let Some(&mut (b, ref mut i)) = stack.last_mut() {
        if let Some(&(s, _)) = bl.succ[b].get(*i) {
            *i += 1;
            match state[s] {
                0 => {
                    state[s] = 1;
                    stack.push((s, 0));
                }
                1 => back[s].push(b),
                _ => {}
            }
        } else {
            state[b] = 2;
            rpo.push(b);
            stack.pop();
        }
    }
    rpo.reverse();
    // Natural loop bodies, innermost (smallest) first.
    let mut loops: Vec<(usize, Vec<bool>)> = (0..n)
        .filter(|&h| !back[h].is_empty())
        .map(|h| {
            let mut body = vec![false; n];
            body[h] = true;
            let mut work = back[h].clone();
            while let Some(b) = work.pop() {
                if !std::mem::replace(&mut body[b], true) {
                    work.extend(bl.preds[b].iter().copied());
                }
            }
            (h, body)
        })
        .collect();
    loops.sort_by_key(|(_, body)| body.iter().filter(|&&x| x).count());
    let is_back = |from: usize, to: usize| back[to].contains(&from);
    let mut cyclic = vec![0f32; n];
    let mut freq = vec![0f32; n];
    // Per loop header: how much more often each exit is taken than its probability times the
    // capped header frequency says, so a loop still hands its whole entry flow on when it
    // runs longer than the cap (else everything after a long loop looked cold).
    let mut exit_scale = vec![1f32; n];
    let exits = |from: usize, to: usize, exit_scale: &[f32]| -> f32 {
        loops.iter().filter(|(_, body)| body[from] && !body[to]).map(|&(h, _)| exit_scale[h]).product()
    };
    let propagate = |head: usize, body: &dyn Fn(usize) -> bool, cyclic: &mut Vec<f32>, freq: &mut Vec<f32>, exit_scale: &[f32]| {
        let mut to_head = 0f32;
        for &b in rpo.iter().filter(|&&b| body(b)) {
            // The head of the loop being measured is entered once (its cyclic probability is
            // not known yet); the method's entry may head a loop measured already.
            freq[b] = if b == head {
                1.0 / (1.0 - cyclic[b])
            } else {
                let inflow: f32 = bl.preds[b].iter().filter(|&&p| body(p) && !is_back(p, b)).map(|&p| freq[p] * prob(&bl, p, b) * exits(p, b, exit_scale)).sum();
                inflow / (1.0 - cyclic[b])
            };
            to_head += bl.succ[b].iter().filter(|&&(s, _)| s == head && is_back(b, s)).map(|&(_, p)| freq[b] * p).sum::<f32>();
        }
        to_head
    };
    for (h, body) in &loops {
        let c = propagate(*h, &|b| body[b], &mut cyclic, &mut freq, &exit_scale);
        cyclic[*h] = c.min(MAX_CYCLIC);
        exit_scale[*h] = (1.0 - cyclic[*h]) / (1.0 - c.min(MAX_EXIT_CYCLIC));
    }
    propagate(0, &|_| true, &mut cyclic, &mut freq, &exit_scale);
    let mut out = vec![0f32; code.len()];
    for (i, &start) in bl.starts.iter().enumerate() {
        let end = bl.starts.get(i + 1).copied().unwrap_or(code.len());
        out[start..end].fill(freq[i]);
    }
    out
}

/// Probability of the edge from block `from` to block `to` (summed over parallel edges).
fn prob(bl: &Blocks, from: usize, to: usize) -> f32 {
    bl.succ[from].iter().filter(|&&(s, _)| s == to).map(|&(_, p)| p).sum()
}

fn blocks(m: &'static Method, code: &[u8], live: &impl Fn(usize) -> bool, taken: &dyn Fn(usize) -> Option<f32>) -> Blocks {
    let n = code.len();
    let mut leader = vec![false; n + 1];
    leader[0] = true;
    let mut pc = 0;
    while pc < n {
        let len = insn_len(code, pc);
        if is_branch(code[pc]) {
            successors(code, pc, len).into_iter().filter(|&s| s < n).for_each(|s| leader[s] = true);
            leader[(pc + len).min(n)] = true;
        }
        pc += len;
    }
    let starts: Vec<usize> = (0..n).filter(|&p| leader[p] && live(p)).collect();
    let index = |pc: usize| starts.binary_search(&pc).ok();
    let mut succ = Vec::with_capacity(starts.len());
    for &start in &starts {
        let mut pc = start;
        loop {
            let len = insn_len(code, pc);
            let next = pc + len;
            if is_branch(code[pc]) || next >= n || leader[next] {
                succ.push(out_edges(m, code, pc, len, taken).into_iter().filter_map(|(t, p)| index(t).map(|b| (b, p))).collect::<Vec<_>>());
                break;
            }
            pc = next;
        }
    }
    let mut preds = vec![Vec::new(); starts.len()];
    for (b, out) in succ.iter().enumerate() {
        for &(s, _) in out {
            if !preds[s].contains(&b) {
                preds[s].push(b);
            }
        }
    }
    Blocks { starts, succ, preds }
}

fn is_branch(op: u8) -> bool {
    matches!(op, 0x99..=0xa7 | 0xaa | 0xab | 0xac..=0xb1 | 0xbf | 0xc6..=0xc8)
}

/// The instruction at `pc` ending a block hands control to these targets with these
/// probabilities.
fn out_edges(m: &'static Method, code: &[u8], pc: usize, len: usize, known: &dyn Fn(usize) -> Option<f32>) -> Vec<(usize, f32)> {
    let succ = successors(code, pc, len);
    match code[pc] {
        0x99..=0xa6 | 0xc6 | 0xc7 => {
            // Never quite 0 or 1: a direction the profile never saw may still be taken where
            // the profile does not count, the exit of a loop that ran in on-stack-replaced code
            // above all (the baseline code that profiles it never left the loop), and a loop
            // with no exit at all would leave everything after it at frequency 0, its calls
            // refused as cold.
            let taken = match (known(pc), crate::jit::profile::branch_counts(m, pc)) {
                (Some(p), _) => p.clamp(MIN_PROB, 1.0 - MIN_PROB),
                (None, Some((t, f))) if t + f >= MIN_PROFILE => (t as f32 / (t + f) as f32).clamp(MIN_PROB, 1.0 - MIN_PROB),
                _ => 0.5,
            };
            vec![(succ[1], taken), (succ[0], 1.0 - taken)]
        }
        0xaa | 0xab => {
            let share = 1.0 / succ.len() as f32;
            succ.into_iter().map(|s| (s, share)).collect()
        }
        _ => succ.into_iter().map(|s| (s, 1.0)).collect(),
    }
}
