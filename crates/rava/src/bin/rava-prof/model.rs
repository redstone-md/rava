//! A recording loaded for analysis: its strings, threads, modules and stacks, and its samples,
//! each weighing CPU seconds. A thread's CPU between two of its `ThreadCpu` records is shared
//! by its samples in that span; a sample past the last record weighs its own cycles; without
//! any (an imported recording) a sample weighs the recording's sampling period. In a `wall`
//! recording every sample weighs its interval instead: thread time, blocked or running.

use rava::profile::format::{Frame, Reader, Record, State, Tier};
use std::collections::HashMap;
use std::io::BufReader;

pub struct Sample {
    pub time: f64,
    pub thread: u32,
    pub state: State,
    pub stack: u32,
    pub weight: f64,
}

pub struct Module {
    pub path: String,
}

#[derive(Default)]
pub struct Profile {
    pub meta: HashMap<String, String>,
    pub strings: HashMap<u32, String>,
    pub threads: HashMap<u32, String>,
    pub modules: HashMap<u32, Module>,
    pub stacks: HashMap<u32, Vec<Frame>>,
    pub samples: Vec<Sample>,
    /// Bytes of code installed per tier.
    pub code: HashMap<Tier, (u64, u64)>,
    /// Each thread's CPU seconds over the whole recording.
    pub cpu: HashMap<u32, f64>,
    /// Seconds from the first to the last record with a time.
    pub duration: f64,
}

/// Which samples a command looks at.
#[derive(Default, Clone)]
pub struct Filter {
    pub thread: Option<String>,
    pub state: Option<State>,
    pub from: Option<f64>,
    pub to: Option<f64>,
}

impl Profile {
    pub fn load(path: &str) -> Result<Profile, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
        let mut r = Reader::new(BufReader::with_capacity(1 << 20, file)).map_err(|e| format!("{path}: {e}"))?;
        let mut p = Profile::default();
        // Per thread: its CPU records (time, cycles) and its samples' indices.
        let mut cpu: HashMap<u32, Vec<(u64, u64)>> = HashMap::new();
        let mut raw_cycles: Vec<u64> = Vec::new();
        let mut last = 0u64;
        while let Some(rec) = r.next().map_err(|e| format!("{path}: {e}"))? {
            match rec {
                Record::Meta { key, value } => {
                    p.meta.insert(key, value);
                }
                Record::Str { id, text } => {
                    p.strings.insert(id, text);
                }
                Record::Thread { id, name } => {
                    let n = p.strings.get(&name).cloned().unwrap_or_default();
                    p.threads.insert(id, n);
                }
                Record::Module { id, path, .. } => {
                    let path = p.strings.get(&path).cloned().unwrap_or_default();
                    p.modules.insert(id, Module { path });
                }
                Record::Code { size, tier, .. } => {
                    let e = p.code.entry(tier).or_default();
                    e.0 += 1;
                    e.1 += size;
                }
                Record::Stack { id, frames } => {
                    p.stacks.insert(id, frames);
                }
                Record::Sample { time, thread, state, cycles, stack } => {
                    last = last.max(time);
                    p.samples.push(Sample { time: time as f64 / 1e6, thread, state, stack, weight: 0.0 });
                    raw_cycles.push(cycles);
                }
                Record::ThreadCpu { time, thread, cycles } => {
                    last = last.max(time);
                    cpu.entry(thread).or_default().push((time, cycles));
                }
                Record::Unknown(_) => {}
            }
        }
        p.duration = last as f64 / 1e6;
        p.weigh(&cpu, &raw_cycles);
        Ok(p)
    }

    fn weigh(&mut self, cpu: &HashMap<u32, Vec<(u64, u64)>>, raw_cycles: &[u64]) {
        let cps: f64 = self.meta.get("cycles_per_second").and_then(|v| v.parse().ok()).unwrap_or(3e9);
        let period: f64 = self.meta.get("sample_period_us").or(self.meta.get("interval_us")).and_then(|v| v.parse().ok()).unwrap_or(1000.0) / 1e6;
        // Samples per thread and CPU span.
        let span_of = |thread: u32, t: f64| -> Option<usize> {
            let recs = cpu.get(&thread)?;
            let i = recs.partition_point(|&(time, _)| (time as f64 / 1e6) <= t);
            (i > 0 && i < recs.len()).then_some(i)
        };
        let mut counts: HashMap<(u32, usize), u32> = HashMap::new();
        for s in &self.samples {
            if let Some(i) = span_of(s.thread, s.time) {
                *counts.entry((s.thread, i)).or_default() += 1;
            }
        }
        // A wall-clock recording's samples weigh their interval: seconds of the thread's
        // time, blocked or not.
        let wall = self.meta.get("mode").is_some_and(|m| m == "wall");
        for (s, &c) in self.samples.iter_mut().zip(raw_cycles) {
            s.weight = match span_of(s.thread, s.time) {
                _ if wall => period,
                Some(i) => {
                    let recs = &cpu[&s.thread];
                    let used = recs[i].1.saturating_sub(recs[i - 1].1) as f64 / cps;
                    used / counts[&(s.thread, i)] as f64
                }
                None if cpu.is_empty() => period,
                None => c as f64 / cps,
            };
        }
        for (&t, recs) in cpu {
            if let (Some(a), Some(b)) = (recs.first(), recs.last()) {
                self.cpu.insert(t, b.1.saturating_sub(a.1) as f64 / cps);
            }
        }
        if cpu.is_empty() {
            for s in &self.samples {
                *self.cpu.entry(s.thread).or_default() += s.weight;
            }
        }
    }

    pub fn thread_name(&self, id: u32) -> &str {
        self.threads.get(&id).map_or("?", |s| s.as_str())
    }

    pub fn keeps(&self, f: &Filter, s: &Sample) -> bool {
        f.thread.as_ref().map_or(true, |t| self.thread_name(s.thread).starts_with(t.as_str()))
            && f.state.map_or(true, |st| s.state == st)
            && f.from.map_or(true, |t| s.time >= t)
            && f.to.map_or(true, |t| s.time < t)
    }

    /// A frame's label: a Java method, or a module's file name and offset (named through the
    /// module's symbols when `symbols` resolves it).
    pub fn label(&self, f: &Frame, symbols: &mut crate::symbols::Symbols) -> String {
        match *f {
            Frame::Java { method, .. } => self.strings.get(&method).cloned().unwrap_or_else(|| format!("method#{method}")),
            Frame::Native { module, offset } => match self.modules.get(&module) {
                Some(m) => {
                    let file = m.path.rsplit(['\\', '/']).next().unwrap_or(&m.path);
                    match symbols.name(&m.path, offset) {
                        Some(name) => format!("{file}!{name}"),
                        None => format!("{file}+{offset:#x}"),
                    }
                }
                None => format!("module#{module}+{offset:#x}"),
            },
            Frame::Address(a) => format!("{a:#x}"),
        }
    }
}
