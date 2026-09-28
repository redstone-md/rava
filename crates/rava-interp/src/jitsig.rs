//! 23.1 JIT CALL-SIGNATURE PROBE (RAVA_JIT_SIG_PROBE, read-only, gated). Classifies EVERY compiled→
//! runtime call (jit_call is the single choke every one of the ~174M nested boot calls flows through)
//! by its static callee descriptor, to size the addressable set for a primitive register-ABI (23.1):
//! a Fast-Register call can only apply to a callee whose args + return carry NO guest reference. The
//! buckets are frequency-weighted (one tick per executed call) so the ratio directly answers "what
//! fraction of the hot calls could a primitive register ABI even touch?". Off ⇒ zero effect.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_JIT_SIG_PROBE").is_some())
}

// Frequency-weighted call buckets (one increment per executed jit_call).
static STATIC_PRIM: AtomicU64 = AtomicU64::new(0); // static, no ref in args or return → fully register-eligible
static INSTANCE_PRIM: AtomicU64 = AtomicU64::new(0); // instance (this=ref), but all args + return primitive
static HAS_REF_ARG: AtomicU64 = AtomicU64::new(0); // ≥1 reference/array parameter (NOT register-eligible)
static REF_RET_ONLY: AtomicU64 = AtomicU64::new(0); // primitive args but a reference/array RETURN
static TOTAL: AtomicU64 = AtomicU64::new(0);

/// Classify one call by its static descriptor + static-ness. `kind==0` is invokestatic.
pub fn record(desc: &str, kind: u8) {
    TOTAL.fetch_add(1, Relaxed);
    let inner = match (desc.find('('), desc.find(')')) {
        (Some(a), Some(b)) if b > a => &desc[a + 1..b],
        _ => return,
    };
    let ref_arg = inner.contains('L') || inner.contains('[');
    let rstart = desc.rfind(')').map(|p| p + 1).unwrap_or(desc.len());
    let ref_ret = matches!(desc.as_bytes().get(rstart), Some(b'L') | Some(b'['));
    if ref_arg {
        HAS_REF_ARG.fetch_add(1, Relaxed);
    } else if ref_ret {
        REF_RET_ONLY.fetch_add(1, Relaxed);
    } else if kind == 0 {
        STATIC_PRIM.fetch_add(1, Relaxed);
    } else {
        INSTANCE_PRIM.fetch_add(1, Relaxed);
    }
}

pub fn dump() {
    if !enabled() {
        return;
    }
    let (t, sp, ip, ra, rr) = (
        TOTAL.load(Relaxed),
        STATIC_PRIM.load(Relaxed),
        INSTANCE_PRIM.load(Relaxed),
        HAS_REF_ARG.load(Relaxed),
        REF_RET_ONLY.load(Relaxed),
    );
    let pct = |x: u64| if t > 0 { x * 1000 / t } else { 0 };
    let p = |x: u64| format!("{}.{}%", pct(x) / 10, pct(x) % 10);
    eprintln!(
        "[JIT-SIG] {t} compiled→runtime calls (frequency-weighted):\n\
         [JIT-SIG]   static-prim (fully register-eligible, zero refs) = {sp} ({})\n\
         [JIT-SIG]   instance-prim (this=ref, args+ret primitive)      = {ip} ({})\n\
         [JIT-SIG]   ref-return-only (prim args, ref return)           = {rr} ({})\n\
         [JIT-SIG]   has-ref-arg (object-passing, NOT eligible)        = {ra} ({})\n\
         [JIT-SIG]   ⇒ register-ABI addressable (static-prim + instance-prim) = {} ({})",
        p(sp), p(ip), p(rr), p(ra), sp + ip, p(sp + ip)
    );
}
