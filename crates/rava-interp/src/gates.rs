//! Crate-level gates, deny tables and the trace hooks over them.
//!
//! Re-exported from the parent module, so no call site outside it changed.





use crate::value::Value;
use crate::*;
/// DEFAULT-ON since 2026-07-26 (`RAVA_NO_FAST_STRHASH` disables) — fold byte-slice keys
/// 8 bytes per round instead of 1. Read once; the branch is a predictable load off a hot
/// cache line.
///
/// Flipped on the evidence the RIP profiler produced: the boot spends only 3-16% of its
/// time INSIDE compiled code, so host-side bookkeeping like this fold is where the wall
/// actually is. Paired with the field-key scratch memo, blocks A+B measured min −5.6% /
/// median −9.1% over 10 interleaved runs with EVERY ON run below EVERY OFF run — this
/// project's standard for shipping a default.
///
/// This changes hash VALUES, hence `FastMap` iteration order. Nothing may depend on that
/// order, but "may not" is not "does not", which is why the opt-out stays and the claim is
/// arbitrated by the bit-exact ChunkFill hash rather than by inspection.
pub(crate) fn fast_strhash() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_FAST_STRHASH").is_none())
}

/// 19.2 M4 native CubicSpline interval-search gate (RAVA_M4_SPLINE_NATIVE=1, default OFF). When on,
/// `CubicSpline$Multipoint.findIntervalStart([FF)I` is served natively (a bit-exact binary lower-bound
/// over the `float[]`) AND denied compilation — so the compiled `apply` routes its call through the
/// `Vm::invoke` choke where the intercept lives, killing the ~1.45M guest predicate-lambda calls.
#[cfg(feature = "next-gen-gc")]
/// `RAVA_RECORD_TOSTRING_PURE` (default OFF) — admit the record accessor synthesised by
/// `java/lang/runtime/ObjectMethods.bootstrap` as a PURE render, so the per-call stringify
/// memo can cache it. See `Vm::record_synth_tostring`.
/// Implied by key interning: hash-consing a value graph of records is exactly the
/// case the proof unlocks, so the two are never usefully separated. Since key interning went
/// default-ON, this follows it — disabling that (`RAVA_NO_KEY_INTERN`) also withdraws the
/// purity admission, which is what makes the opt-out a real fallback rather than a half one.
pub(crate) fn record_tostring_pure_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_RECORD_TOSTRING_PURE").is_some()
            || crate::key_intern::enabled()
    })
}

/// Stage-3 `DoubleArrayList.wrap` native (`RAVA_NO_DAL_WRAP` opts out). Its call site is already
/// short-circuited by a name and owner-class test, so this one was never hot — memoised only so
/// the two sibling gates cannot drift apart.
pub(crate) fn dal_wrap_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_NO_DAL_WRAP").is_none())
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn worker_locked_exec_enabled() -> bool {
    #[cfg(test)]
    match LOCKED_EXEC_TEST_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => return false,
        2 => return true,
        _ => {}
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        // GILFREE-RINGS IMPLIES IT. Without the locked-exec envelope a lane worker that
        // reaches the general `jit_call` path — a JDK native-served or not-yet-compiled
        // callee, which the ring body hits within THREE callbacks — takes the legacy G1.1
        // guard and aborts the whole region. The lane is then armed, admits all 127 tasks,
        // and completes none, which reads exactly like a deeper failure; it cost a full
        // measurement run to notice the missing companion flag. A lane that cannot work
        // without a second env var must turn it on itself.
        crate::gilfree_cf::rings()
            || std::env::var("RAVA_M3_WORKER_LOCKED_EXEC").ok().as_deref() == Some("1")
    })
}

/// TPS probe: on each MinecraftServer.tickServer, print the tick count + the rolling
/// tick rate (ticks/s) every 20 ticks. 20 ticks/s = the MC target = JVM parity; lower
/// means rava can't hold the 50ms/tick budget on the live server. Gated by RAVA_TICK_PROF.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn tps_probe_tick() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("RAVA_TICK_PROF").is_some()) {
        return;
    }
    static COUNT: AtomicU64 = AtomicU64::new(0);
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n % 20 == 0 {
        let secs = start.elapsed().as_secs_f64();
        eprintln!("[TPS] {n} ticks in {secs:.1}s = {:.2} ticks/s (JVM target 20)", n as f64 / secs);
    }
}

/// Generic call tracer (RAVA_TRACE_CALLS=substr[,substr…]): print every call whose
/// `Class#method` contains a listed substring, with a shallow arg preview. Hooked at
/// BOTH machines' call points — `Vm::invoke` ([CALL]) and the drive_stack staged-frame
/// push ([CALL-T]) — so recursive-vs-stackful ordering diffs see every call. Zero cost
/// when unset (one OnceLock deref + is_empty).
/// ONE bool decides whether this hook does anything, and it is checked before the string compare.
///
/// The hook runs on every interpreted call. It used to compare `name == "tickServer"` first —
/// a length test plus a memcmp on a path where the whole call is ~250 ns — and only then read the
/// filter. Both probes are opt-in, so their arming is a single OnceLock read; the comparison
/// belongs behind it, not in front of it.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn call_hook_armed() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("RAVA_TICK_PROF").is_some() || std::env::var_os("RAVA_TRACE_CALLS").is_some()
    })
}

#[inline]
pub(crate) fn trace_call_hook(class_name: &str, name: &str, descriptor: &str, args: &[Value], staged: bool) {
    #[cfg(feature = "next-gen-gc")]
    if !call_hook_armed() {
        return;
    }
    // TPS PROBE (RAVA_TICK_PROF): count MinecraftServer tick calls + wall between them
    // → live-server tick rate (goal #4: is rava at TPS parity/faster than JVM's 20/s?).
    // Not staged (count each tick once).
    #[cfg(feature = "next-gen-gc")]
    if !staged && name == "tickServer" {
        tps_probe_tick();
    }
    static FILTER: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let f = FILTER.get_or_init(|| {
        std::env::var("RAVA_TRACE_CALLS")
            .ok()
            .map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
            .unwrap_or_default()
    });
    if f.is_empty() {
        return;
    }
    let key = format!("{class_name}#{name}");
    if f.iter().any(|s| key.contains(s.as_str())) {
        let preview: Vec<String> = args
            .iter()
            .take(3)
            .map(|v| match v {
                Value::Str(s) => format!("\"{}\"", &s[..s.len().min(48)]),
                other => format!("{other:?}").chars().take(48).collect(),
            })
            .collect();
        let tag = if staged { "[CALL-T]" } else { "[CALL]" };
        eprintln!("{tag} {key}{descriptor} args=[{}]", preview.join(", "));
    }
}

/// (class-substring list, exact `Class#method` set). The exact set is a hash lookup —
/// a bisect can carry hundreds of entries without a per-invoke substring scan.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn deny_tables() -> &'static (Vec<String>, std::collections::HashSet<String>) {
    static DENY: std::sync::OnceLock<(Vec<String>, std::collections::HashSet<String>)> =
        std::sync::OnceLock::new();
    DENY.get_or_init(|| {
        let mut subs = Vec::new();
        let mut exact = std::collections::HashSet::new();
        if let Ok(s) = std::env::var("RAVA_JIT_DENY") {
            for p in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                if p.contains('#') {
                    exact.insert(p.to_string());
                } else {
                    subs.push(p.to_string());
                }
            }
        }
        (subs, exact)
    })
}
