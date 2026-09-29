//! Opt-in counters (`RAVA_CORE_JIT_STATS=1`): which opcodes compiled code defers to the
//! interpreter, and how many invocations still run interpreted. Printed at exit.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

pub struct Stats {
    deferred: [AtomicU64; 256],
    pub interpreted_calls: AtomicU64,
    pub compiled_methods: AtomicU64,
    pub tier2_methods: AtomicU64,
}

pub fn get() -> Option<&'static Stats> {
    static S: OnceLock<Option<&'static Stats>> = OnceLock::new();
    *S.get_or_init(|| {
        std::env::var_os("RAVA_CORE_JIT_STATS").map(|_| {
            &*Box::leak(Box::new(Stats {
                deferred: std::array::from_fn(|_| AtomicU64::new(0)),
                interpreted_calls: AtomicU64::new(0),
                compiled_methods: AtomicU64::new(0),
                tier2_methods: AtomicU64::new(0),
            }))
        })
    })
}

impl Stats {
    pub fn deferred(&self, op: u8) {
        self.deferred[op as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub fn report(&self) {
        let mut rows: Vec<(u64, usize)> =
            self.deferred.iter().enumerate().map(|(op, c)| (c.load(Ordering::Relaxed), op)).filter(|r| r.0 > 0).collect();
        rows.sort_unstable_by(|a, b| b.cmp(a));
        eprintln!(
            "[jit-stats] compiled methods {} ({} at tier 2), interpreted invocations {}",
            self.compiled_methods.load(Ordering::Relaxed),
            self.tier2_methods.load(Ordering::Relaxed),
            self.interpreted_calls.load(Ordering::Relaxed)
        );
        for (n, op) in rows.iter().take(16) {
            eprintln!("[jit-stats] deferred opcode {op:#04x}: {n}");
        }
    }
}
