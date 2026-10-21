//! The reports on one recording: `summary`, `threads`, `top`, `tree` and `folded`.

use crate::model::{Filter, Profile};
use crate::symbols::Symbols;
use rava::profile::format::{Frame, State};
use std::collections::{HashMap, HashSet};

/// How frames become rows.
#[derive(Default, Clone)]
pub struct View {
    pub filter: Filter,
    /// Self time goes to the innermost frame not inlined into another (the compiled method
    /// holding the code), as both VMs can tell it; otherwise to the innermost frame.
    pub physical: bool,
    /// Java frames carry their tier (`method [opt]`).
    pub tiers: bool,
    /// Lambda and hidden classes' addresses dropped (`A$$Lambda/0x...` as `A$$Lambda`): each
    /// run names them anew.
    pub merge_lambdas: bool,
    pub top: usize,
}

/// Self and inclusive seconds per label, and the total.
pub struct Totals {
    pub own: HashMap<String, f64>,
    pub inclusive: HashMap<String, f64>,
    pub total: f64,
}

pub fn label(p: &Profile, f: &Frame, v: &View, sym: &mut Symbols) -> String {
    let mut l = p.label(f, sym);
    if v.merge_lambdas && matches!(f, Frame::Java { .. }) {
        l = merge_hidden(&l);
    }
    match f {
        Frame::Java { tier, .. } if v.tiers => format!("{l} [{}]", tier.name()),
        _ => l,
    }
}

pub fn totals(p: &Profile, v: &View, sym: &mut Symbols) -> Totals {
    let mut t = Totals { own: HashMap::new(), inclusive: HashMap::new(), total: 0.0 };
    // Labels per stack, computed once.
    let mut labels: HashMap<u32, (String, Vec<String>)> = HashMap::new();
    for s in p.samples.iter().filter(|s| p.keeps(&v.filter, s)) {
        let (own, all) = labels.entry(s.stack).or_insert_with(|| {
            let frames = p.stacks.get(&s.stack).map_or(&[][..], |f| f.as_slice());
            let leaf = if v.physical { frames.iter().position(|f| !matches!(f, Frame::Java { inlined: true, .. })).unwrap_or(0) } else { 0 };
            let own = frames.get(leaf).map_or_else(|| "<no stack>".to_string(), |f| label(p, f, v, sym));
            let mut seen = HashSet::new();
            let all = frames.iter().map(|f| label(p, f, v, sym)).filter(|l| seen.insert(l.clone())).collect();
            (own, all)
        });
        *t.own.entry(own.clone()).or_default() += s.weight;
        for l in all.iter() {
            *t.inclusive.entry(l.clone()).or_default() += s.weight;
        }
        t.total += s.weight;
    }
    t
}

fn pct(x: f64, total: f64) -> f64 {
    if total > 0.0 { 100.0 * x / total } else { 0.0 }
}

pub fn summary(p: &Profile, v: &View) {
    println!("{} ({})", p.meta.get("vm").map_or("?", |s| s), p.meta.get("command").map_or("", |s| s));
    println!("duration {:.1} s, {} samples, {} stacks, {} threads", p.duration, p.samples.len(), p.stacks.len(), p.threads.len());
    let mut by_state: HashMap<State, f64> = HashMap::new();
    let mut total = 0.0;
    for s in p.samples.iter().filter(|s| p.keeps(&v.filter, s)) {
        *by_state.entry(s.state).or_default() += s.weight;
        total += s.weight;
    }
    println!("\nsampled CPU {total:.2} s by state:");
    let mut states: Vec<_> = by_state.into_iter().collect();
    states.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (st, w) in states {
        println!("  {:8} {:8.2} s {:5.1}%", st.name(), w, pct(w, total));
    }
    let mut code: Vec<_> = p.code.iter().collect();
    code.sort_by_key(|(t, _)| **t);
    if !code.is_empty() {
        println!("\ncode installed:");
        for (tier, (n, bytes)) in code {
            println!("  {:8} {:7} methods {:9.1} KiB", tier.name(), n, *bytes as f64 / 1024.0);
        }
    }
    println!();
    threads_total(p, v);
}

/// CPU seconds per thread name (threads of one name, a pool's workers, summed).
fn threads_total(p: &Profile, v: &View) {
    let mut by: HashMap<String, (f64, usize)> = HashMap::new();
    for (&id, &cpu) in &p.cpu {
        let name = p.thread_name(id).to_string();
        if v.filter.thread.as_ref().map_or(true, |t| name.starts_with(t.as_str())) {
            let e = by.entry(name).or_default();
            e.0 += cpu;
            e.1 += 1;
        }
    }
    let mut rows: Vec<_> = by.into_iter().collect();
    rows.sort_by(|a, b| b.1 .0.total_cmp(&a.1 .0));
    println!("{:>9}  {:>3}  thread", "CPU s", "n");
    for (name, (cpu, n)) in rows.into_iter().take(v.top.max(1)) {
        println!("{cpu:9.2}  {n:3}  {name}");
    }
}

/// CPU per thread name per window of `window` seconds.
pub fn threads(p: &Profile, v: &View, window: f64) {
    let mut names: Vec<String> = Vec::new();
    let mut cells: HashMap<(usize, String), f64> = HashMap::new();
    let mut last = 0;
    for s in p.samples.iter().filter(|s| p.keeps(&v.filter, s)) {
        let w = (s.time / window) as usize;
        last = last.max(w);
        let name = p.thread_name(s.thread).to_string();
        if !names.contains(&name) {
            names.push(name.clone());
        }
        *cells.entry((w, name)).or_default() += s.weight;
    }
    let mut total: Vec<(String, f64)> = names.iter().map(|n| (n.clone(), (0..=last).map(|w| cells.get(&(w, n.clone())).copied().unwrap_or(0.0)).sum())).collect();
    total.sort_by(|a, b| b.1.total_cmp(&a.1));
    total.truncate(v.top.clamp(1, 12));
    print!("{:>7}", "from s");
    for (n, _) in &total {
        print!(" {:>15}", &n[..n.len().min(15)]);
    }
    println!();
    for w in 0..=last {
        print!("{:7.0}", w as f64 * window);
        for (n, _) in &total {
            print!(" {:15.2}", cells.get(&(w, n.clone())).copied().unwrap_or(0.0) / window);
        }
        println!();
    }
    println!("(cores busy per window)");
}

pub fn top(p: &Profile, v: &View, inclusive: bool, sym: &mut Symbols) {
    let t = totals(p, v, sym);
    let (map, other) = if inclusive { (&t.inclusive, &t.own) } else { (&t.own, &t.inclusive) };
    let mut rows: Vec<_> = map.iter().collect();
    rows.sort_by(|a, b| b.1.total_cmp(a.1));
    println!("total {:.2} CPU s", t.total);
    println!("{:>9} {:>6} {:>9}  {}", if inclusive { "incl s" } else { "self s" }, "%", if inclusive { "self s" } else { "incl s" }, "frame");
    for (k, &s) in rows.into_iter().take(v.top) {
        println!("{s:9.2} {:6.2} {:9.2}  {k}", pct(s, t.total), other.get(k).copied().unwrap_or(0.0));
    }
}

/// The callees under `root` (its outermost occurrence in each stack), as a tree of inclusive
/// seconds down to `depth` levels, branches under `min` percent of the root left out.
pub fn tree(p: &Profile, v: &View, root: &str, depth: usize, min: f64, sym: &mut Symbols) {
    #[derive(Default)]
    struct Node {
        time: f64,
        kids: HashMap<String, Node>,
    }
    let mut top = Node::default();
    for s in p.samples.iter().filter(|s| p.keeps(&v.filter, s)) {
        let frames = p.stacks.get(&s.stack).map_or(&[][..], |f| f.as_slice());
        let labels: Vec<String> = frames.iter().map(|f| label(p, f, v, sym)).collect();
        let Some(r) = labels.iter().rposition(|l| l.contains(root)) else { continue };
        top.time += s.weight;
        let mut node = &mut top;
        for l in labels[..r].iter().rev().take(depth) {
            node = node.kids.entry(l.clone()).or_default();
            node.time += s.weight;
        }
    }
    println!("{:.2} CPU s under {root}", top.time);
    fn print(n: &Node, total: f64, min: f64, indent: usize) {
        let mut kids: Vec<_> = n.kids.iter().collect();
        kids.sort_by(|a, b| b.1.time.total_cmp(&a.1.time));
        let shown: f64 = kids.iter().map(|k| k.1.time).sum();
        for (name, k) in kids {
            if pct(k.time, total) < min {
                continue;
            }
            println!("{:8.2} {:6.2}%  {}{}", k.time, pct(k.time, total), "  ".repeat(indent), name);
            print(k, total, min, indent + 1);
        }
        let own = n.time - shown;
        if indent > 0 && pct(own, total) >= min {
            println!("{:8.2} {:6.2}%  {}<self>", own, pct(own, total), "  ".repeat(indent));
        }
    }
    print(&top, top.time, min, 0);
}

/// Folded stacks (`thread;outermost;...;innermost milliseconds`), for flame graph tools.
pub fn folded(p: &Profile, v: &View, sym: &mut Symbols) {
    let mut out: HashMap<String, f64> = HashMap::new();
    for s in p.samples.iter().filter(|s| p.keeps(&v.filter, s)) {
        let frames = p.stacks.get(&s.stack).map_or(&[][..], |f| f.as_slice());
        let mut line = p.thread_name(s.thread).replace(';', ":");
        for f in frames.iter().rev() {
            line.push(';');
            line += &label(p, f, v, sym).replace(';', ":");
        }
        *out.entry(line).or_default() += s.weight;
    }
    let mut rows: Vec<_> = out.into_iter().collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (line, w) in rows {
        let ms = (w * 1000.0).round() as u64;
        if ms > 0 {
            println!("{line} {ms}");
        }
    }
}

/// `x/Y$$Lambda/0x000000080103c000.run()V` as `x/Y$$Lambda.run()V`, and a lambda form's
/// `LambdaForm$MH/0x...` likewise.
fn merge_hidden(l: &str) -> String {
    let Some(at) = l.find("/0x") else { return l.to_string() };
    let end = l[at + 3..].find(|c: char| !c.is_ascii_hexdigit()).map_or(l.len(), |i| at + 3 + i);
    format!("{}{}", &l[..at], merge_hidden(&l[end..]))
}

#[cfg(test)]
mod tests {
    #[test]
    fn merges_hidden_class_addresses() {
        assert_eq!(super::merge_hidden("a/B$$Lambda/0x00000000087fd400.apply(Ljava/lang/Object;)V"), "a/B$$Lambda.apply(Ljava/lang/Object;)V");
        assert_eq!(super::merge_hidden("java/lang/invoke/LambdaForm$MH/0x000000000803c000.invoke()I"), "java/lang/invoke/LambdaForm$MH.invoke()I");
        assert_eq!(super::merge_hidden("java/lang/String.length()I"), "java/lang/String.length()I");
    }
}