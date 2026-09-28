//! Interpreter diagnostics: the heartbeat, the profile dump, and the throw census.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

/// Ops between heartbeat/GC/cap checks. The op-loop decrements a Vm-side
/// countdown by 1 per instruction and only does periodic work when it reaches 0,
/// so the common path pays a decrement+compare instead of an atomic RMW and two
/// non-power-of-two modulos. 100k is small enough to bound GC latency and large
/// enough that the check itself is negligible.
pub(crate) const CHECK_INTERVAL: u32 = 100_000;

thread_local! {
    /// Statistical profiler (RAVA_PROFILE): top-of-stack class sampled once per
    /// CHECK_INTERVAL ops. Op-weighted, so it shows where bytecode execution
    /// spends its time. (Time inside native Rust natives isn't sampled — that
    /// shows up instead as a DROP in the heartbeat op-rate.)
    pub(crate) static PROF: std::cell::RefCell<std::collections::HashMap<Box<str>, u64>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    pub(crate) static PROF_ON: std::cell::Cell<i8> = const { std::cell::Cell::new(-1) };
    pub(crate) static LAST_HB: std::cell::Cell<Option<(std::time::Instant, u64)>> = const { std::cell::Cell::new(None) };
}

/// Heartbeat op-rate (ops/sec since the last call) — a low rate means the engine
/// is spending wall-time inside native Rust (a heavy map drain / codec native /
/// string-key build), not executing bytecode.
pub(crate) fn hb_oprate(now_ops: u64) -> f64 {
    LAST_HB.with(|c| {
        let now = std::time::Instant::now();
        let rate = match c.get() {
            Some((t, ops)) => {
                let dt = now.duration_since(t).as_secs_f64();
                if dt > 0.0 { (now_ops - ops) as f64 / dt } else { 0.0 }
            }
            None => 0.0,
        };
        c.set(Some((now, now_ops)));
        rate
    })
}

/// Dump the profiler histogram (top classes by sampled op-weight).
pub fn dump_profile() {
    if !prof_enabled() {
        return;
    }
    PROF.with(|p| {
        let map = p.borrow();
        let total: u64 = map.values().sum();
        if total == 0 {
            return;
        }
        let mut v: Vec<_> = map.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        eprintln!("[PROFILE] top classes by op-weight ({total} samples):");
        for (cls, n) in v.into_iter().take(25) {
            eprintln!("  {:5.1}%  {cls}", 100.0 * (*n as f64) / total as f64);
        }
    });
}

/// Collect-mode throw swallowing (RAVA_COLLECT_THROWS — SEPARATE from
/// RAVA_COLLECT). DANGEROUS: swallowing an exception at the throwing method
/// prevents a caller's `catch` from running, which BREAKS exception-based
/// control flow (e.g. log4j probes for a class via ClassNotFoundException, and
/// swallowing it makes log4j think the class exists). Use only to peek past a
/// known throw-bug, never for a clean harvest.
pub(crate) fn collect_throw(sig: &str, exc: &str) -> bool {
    thread_local! {
        static ON: std::cell::Cell<i8> = const { std::cell::Cell::new(-1) };
        static SEEN: std::cell::RefCell<std::collections::HashSet<String>> =
            std::cell::RefCell::new(std::collections::HashSet::new());
    }
    let on = ON.with(|c| {
        let v = c.get();
        if v >= 0 { return v == 1; }
        let on = std::env::var_os("RAVA_COLLECT_THROWS").is_some();
        c.set(on as i8);
        on
    });
    if on {
        SEEN.with(|s| {
            if s.borrow_mut().insert(sig.to_string()) {
                eprintln!("[THROW-SKIP] {sig} | {exc}");
            }
        });
    }
    on
}

/// RAVA_TRACE_BIG diagnostic: a double LOCAL receiving an abnormally large value
/// (the pre-clamp source of the player-tick X,Z blow-up — putfield only sees the
/// post-clamp 3e7). Skips classes that legitimately hold large doubles (tick-time
/// nanos, memory bytes, the world border) so only the physics path surfaces.
/// Takes the two frame FIELDS it reads rather than the frame: the hot opcode loop destructures
/// `Frame` into disjoint borrows so its operand stack can live in registers, and after that
/// there is no `&Frame` to hand anybody.
pub(crate) fn trace_big_local(vm: &Vm, class: &crate::class_loader::LoadedClass, pc: usize, v: &Value) {
    if let Value::Double(d) = v {
        // > 3e7 skips the world-border bound (±2.9999984e7, computed in collision
        // every tick) and only catches a TRUE coordinate blow-up (the pre-clamp
        // runaway). A mob hitting the bug while wandering surfaces here with no
        // player needed.
        if d.abs() > 3.0e7 {
            let c = class.file.this_class.as_str();
            let noisy = c.contains("Slack") || c.contains("RollingAverage") || c.contains("timings")
                || c.contains("TimingHistory") || c.contains("MinecraftServer") || c.contains("FullServerTick")
                || c.contains("WorldData") || c.contains("WorldBorder") || c.contains("NBTTagDouble")
                || c.contains("PacketPlayOutWorldBorder");
            if !noisy {
                eprintln!("[BIG-LOC] {c} pc={pc} dstore={d}\n{}", vm.backtrace());
            }
        }
    }
}
