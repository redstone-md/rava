//! `RAVA_HOT_CENSUS` — opcodes executed per entry into `run_hot`.
//!
//! `opcodes::run` is one 57 KB function and `run_hot` is the subset that needs nothing but the
//! frame, extracted so an interpreter loop has registers (see `hot.rs`'s header). The split has a
//! cost nobody has priced: `run_hot` returns `Cold` for every opcode it does not own — **every
//! field access and every call among them** — and each re-entry re-destructures the frame and
//! reloads the operand-stack cursor.
//!
//! Ledger row 344 puts `run_hot` at 20.5% of interpreter time and `run` at 12.5%. If the average
//! run is two or three opcodes, the split pays its setup more often than it amortises it, and
//! moving the common cold opcodes IN is a real lever. If the average is twenty, the split is
//! working and the lever does not exist. **One ratio decides it.**
//!
//! Inert unless the variable is set.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_HOT_CENSUS").is_some())
}

static ENTRIES: AtomicU64 = AtomicU64::new(0);
static OPS: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn note_entry() {
    ENTRIES.fetch_add(1, Relaxed);
}

#[inline]
pub fn note_op() {
    OPS.fetch_add(1, Relaxed);
}

pub fn report() -> String {
    if !enabled() {
        return String::new();
    }
    let (e, o) = (ENTRIES.load(Relaxed), OPS.load(Relaxed));
    if e == 0 {
        return String::new();
    }
    format!(
        "[HOT-CENSUS] run_hot entries={e} opcodes={o} = {:.2} opcodes per entry",
        o as f64 / e as f64
    )
}

/// WHICH opcode ends a hot run — the number that decides whether the 4.17-per-entry ratio is a
/// LEVER or a fact of life.
///
/// A run that ends on `getfield` could be continued if the opcode were callable without
/// `&mut Frame`. A run that ends on `invokevirtual` could NOT: the call unwinds into `run`
/// regardless, because the callee needs its own frame. **So the ratio only becomes a lever to the
/// extent that non-call opcodes end the runs**, and that split has never been taken.
static COLD_OP: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];

#[inline]
pub fn note_cold(op: u8) {
    COLD_OP[op as usize].fetch_add(1, Relaxed);
}

/// JVM opcode names for the ones this census is likely to see; anything else prints as hex.
fn op_name(op: u8) -> &'static str {
    match op {
        0xb2 => "getstatic",
        0xb3 => "putstatic",
        0xb4 => "getfield",
        0xb5 => "putfield",
        0xb6 => "invokevirtual",
        0xb7 => "invokespecial",
        0xb8 => "invokestatic",
        0xb9 => "invokeinterface",
        0xba => "invokedynamic",
        0xbb => "new",
        0xac..=0xb1 => "?return",
        0x12 | 0x13 | 0x14 => "ldc",
        0x2e..=0x35 => "?aload[]",
        0x4f..=0x56 => "?astore[]",
        0xbe => "arraylength",
        0xc0 => "checkcast",
        0xc1 => "instanceof",
        0x99..=0xa6 => "if_*",
        0xa7 => "goto",
        _ => "",
    }
}

pub fn cold_report() -> String {
    if !enabled() {
        return String::new();
    }
    let mut rows: Vec<(u64, u8)> = (0..256)
        .map(|i| (COLD_OP[i].load(Relaxed), i as u8))
        .filter(|(n, _)| *n > 0)
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    rows.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let tot: u64 = rows.iter().map(|(n, _)| *n).sum();
    // A call MUST unwind to `run` whatever the loop does; anything else is addressable.
    let is_call = |op: u8| (0xb6..=0xba).contains(&op);
    let calls: u64 = rows.iter().filter(|(_, o)| is_call(*o)).map(|(n, _)| *n).sum();
    let mut out = format!(
        "[HOT-COLD] {tot} hot-run exits | CALLS {calls} ({:.1}%) are unavoidable; \
         the other {:.1}% are addressable:",
        calls as f64 * 100.0 / tot as f64,
        (tot - calls) as f64 * 100.0 / tot as f64
    );
    for (n, op) in rows.iter().take(12) {
        out.push_str(&format!(
            "\n[HOT-COLD] {:>12}  {:5.1}%  0x{op:02x} {}",
            n,
            *n as f64 * 100.0 / tot as f64,
            op_name(*op)
        ));
    }
    out
}

/// PERIODIC DUMP. A `server.jar` boot never exits, so a report wired only to teardown says nothing
/// about it — the trap this ledger has recorded three times (rows 104, 114, 138) and hit again
/// with `pure_memo` and `bridgestat`. `RAVA_HOT_CENSUS_MS` (default 20 s) prints the running totals
/// so the opcode RATE inside a named window can be read without waiting for `Done`.
///
/// ⭐**Why the rate matters (row 355).** The boot's gap is one window — `Preparing level` → `Done`
/// is **~626×** `java -Xint` — while rava's interpreter is only **1.97×** on identical work
/// (row 354). Those reconcile only if that window executes far MORE opcodes, not slower ones.
/// **This is the counter that decides which.**
pub fn start_watchdog() {
    static W: OnceLock<()> = OnceLock::new();
    if !enabled() {
        return;
    }
    W.get_or_init(|| {
        let ms = std::env::var("RAVA_HOT_CENSUS_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(20_000)
            .max(1_000);
        let _ = std::thread::Builder::new()
            .name("rava-hotcensus".to_string())
            .spawn(move || {
                let t0 = std::time::Instant::now();
                let mut last = (0u64, 0u64);
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    let (e, o) = (ENTRIES.load(Relaxed), OPS.load(Relaxed));
                    let dt = ms as f64 / 1000.0;
                    {
                        let cr = class_report();
                        if !cr.is_empty() {
                            eprintln!("{cr}");
                        }
                    }
                    eprintln!(
                        "[HOT-RATE] t+{:.0}s entries={e} opcodes={o} | last {:.1}s: {:.1}M opcodes/s, {:.1}M entries/s",
                        t0.elapsed().as_secs_f64(),
                        dt,
                        (o - last.1) as f64 / dt / 1e6,
                        (e - last.0) as f64 / dt / 1e6
                    );
                    last = (e, o);
                }
            });
    });
}

/// OPCODES BY GUEST CLASS (`RAVA_HOT_CENSUS_CLASS`) — the instrument the interpreted path did not
/// have.
///
/// ⭐**Why it is needed (rows 356-358).** The boot's bad window executes **~58 billion opcodes** at
/// full interpreter speed, and **call counts cannot find the consumer**: censuses of
/// `DensityFunction`, `levelgen` and `mojang` all top out in the hundreds of thousands, so the
/// bytecode is concentrated in relatively few calls with large bodies. Guest self-time would name
/// it, but `rava_sampler`'s per-method attribution hooks `jit_method_push/pop` inside `jit_run` —
/// **which does not exist under `RAVA_JIT=0`**. The same shape as the missing interpreter gate
/// (row 339): the interpreted path has no attribution at all.
///
/// ⚠**SAMPLED 1-in-256 by entry count**, because a map touch on every one of 1.29 billion entries
/// would perturb the thing it measures — the mistake row 329's abort census made (it took the
/// boot 56 → 78 s and changed the race it was counting).
#[cfg(feature = "next-gen-gc")]
pub fn class_census() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_HOT_CENSUS_CLASS").is_some())
}

#[cfg(feature = "next-gen-gc")]
static CLASS_SEQ: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "next-gen-gc")]
fn class_tally() -> &'static std::sync::Mutex<std::collections::HashMap<String, u64>> {
    static M: OnceLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> = OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// One sampled entry: `ops` opcodes were executed by `class`. Scaled by the sampling rate at
/// report time, so the printed figure reads as opcodes.
#[cfg(feature = "next-gen-gc")]
pub fn note_class_ops(class: &str, ops: u64) {
    if CLASS_SEQ.fetch_add(1, Relaxed) % 256 != 0 {
        return;
    }
    if let Ok(mut m) = class_tally().lock() {
        *m.entry(class.to_string()).or_insert(0) += ops;
    }
}

#[cfg(feature = "next-gen-gc")]
pub fn class_report() -> String {
    if !class_census() {
        return String::new();
    }
    let Ok(m) = class_tally().lock() else { return String::new() };
    if m.is_empty() {
        return String::new();
    }
    let mut rows: Vec<(&String, &u64)> = m.iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    let tot: u64 = rows.iter().map(|(_, n)| **n).sum();
    let mut out = format!(
        "[HOT-CLASS] sampled 1-in-256; {:.1}B opcodes attributed (scaled); top classes:",
        tot as f64 * 256.0 / 1e9
    );
    for (c, n) in rows.iter().take(15) {
        out.push_str(&format!(
            "\n[HOT-CLASS] {:>8.2}B  {:5.1}%  {}",
            **n as f64 * 256.0 / 1e9,
            **n as f64 * 100.0 / tot as f64,
            c
        ));
    }
    out
}
