//! Opt-in counters (`RAVA_CORE_JIT_STATS=1`): which opcodes compiled code defers to the
//! interpreter, and how many invocations still run interpreted. Printed at exit.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

pub struct Stats {
    deferred: [AtomicU64; 256],
    /// Deferrals per (method, bci), and interpreted (or native) invocations per method.
    sites: std::sync::Mutex<std::collections::HashMap<(usize, u32), u64>>,
    interpreted: std::sync::Mutex<std::collections::HashMap<usize, u64>>,
    pub interpreted_calls: AtomicU64,
    pub compiled_methods: AtomicU64,
    pub tier2_methods: AtomicU64,
    pub opt_methods: AtomicU64,
    /// Time spent compiling, in nanoseconds: baseline and optimized.
    pub baseline_nanos: AtomicU64,
    pub opt_nanos: AtomicU64,
    pub deopts: AtomicU64,
}

pub fn get() -> Option<&'static Stats> {
    static S: OnceLock<Option<&'static Stats>> = OnceLock::new();
    *S.get_or_init(|| {
        std::env::var_os("RAVA_CORE_JIT_STATS").map(|_| {
            &*Box::leak(Box::new(Stats {
                deferred: std::array::from_fn(|_| AtomicU64::new(0)),
                sites: Default::default(),
                interpreted: Default::default(),
                interpreted_calls: AtomicU64::new(0),
                compiled_methods: AtomicU64::new(0),
                tier2_methods: AtomicU64::new(0),
                opt_methods: AtomicU64::new(0),
                baseline_nanos: AtomicU64::new(0),
                opt_nanos: AtomicU64::new(0),
                deopts: AtomicU64::new(0),
            }))
        })
    })
}

impl Stats {
    pub fn interpreted(&self, m: &crate::method::Method) {
        self.interpreted_calls.fetch_add(1, Ordering::Relaxed);
        *self.interpreted.lock().unwrap().entry(m as *const _ as usize).or_default() += 1;
    }

    pub fn deferred(&self, m: &crate::method::Method, bci: usize) {
        let op = m.code.map_or(0, |c| c.code[bci]);
        self.deferred[op as usize].fetch_add(1, Ordering::Relaxed);
        *self.sites.lock().unwrap().entry((m as *const _ as usize, bci as u32)).or_default() += 1;
    }

    pub fn report(&self) {
        let mut rows: Vec<(u64, usize)> =
            self.deferred.iter().enumerate().map(|(op, c)| (c.load(Ordering::Relaxed), op)).filter(|r| r.0 > 0).collect();
        rows.sort_unstable_by(|a, b| b.cmp(a));
        eprintln!(
            "[jit-stats] compiled methods {} ({} at tier 2, {} optimized, {} deopts), interpreted invocations {}",
            self.compiled_methods.load(Ordering::Relaxed),
            self.tier2_methods.load(Ordering::Relaxed),
            self.opt_methods.load(Ordering::Relaxed),
            self.deopts.load(Ordering::Relaxed),
            self.interpreted_calls.load(Ordering::Relaxed)
        );
        eprintln!(
            "[jit-stats] compile time: baseline {} ms, optimized {} ms",
            self.baseline_nanos.load(Ordering::Relaxed) / 1_000_000,
            self.opt_nanos.load(Ordering::Relaxed) / 1_000_000
        );
        for (n, op) in rows.iter().take(16) {
            eprintln!("[jit-stats] deferred opcode {op:#04x}: {n}");
        }
        let mut calls: Vec<(usize, u64)> = self.interpreted.lock().unwrap().iter().map(|(k, v)| (*k, *v)).collect();
        calls.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        for (m, n) in calls.iter().take(8) {
            // SAFETY: methods are immortal.
            let m = unsafe { &*(*m as *const crate::method::Method) };
            eprintln!("[jit-stats] interpreted {m}: {n}");
        }
        let mut sites: Vec<((usize, u32), u64)> = self.sites.lock().unwrap().iter().map(|(k, v)| (*k, *v)).collect();
        sites.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        for ((m, bci), n) in sites.iter().take(16) {
            // SAFETY: methods are immortal.
            let m = unsafe { &*(*m as *const crate::method::Method) };
            let op = m.code.map_or(0, |c| c.code[*bci as usize]);
            eprintln!("[jit-stats] deferred {op:#04x} at {m}@{bci}: {n}");
        }
    }
}
