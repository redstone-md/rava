//! Arena-resident lambdas: their struct view and the forwarding a stale pointer needs.
//!
//! Re-exported from the parent module, so no call site outside it changed.

use crate::opcodes::*;

/// Default-on arity-1 CAPTURING-lambda dedup. RAVA_M3_LAMBDA_DEDUP=0/false/off disables it;
/// dedup is per site, keyed by the exact captured value ([[rava-lambda-capture-verdict]]:
/// 45.6% duplicates ≈ 61.7MB). GENERIC — NOT gated on MC
/// worldgen/noise package names (that would violate the no-adapt rule); it applies to every arity-1
/// capturing site, which is sound per JLS §15.27.4 (lambda identity is unspecified). ChunkFill's
/// bit-exact hash is the arbiter: any code that (non-conformantly) relies on lambda `==` identity
/// would diverge under dedup and be caught.
pub(crate) fn lambda_dedup_enabled() -> bool {
    pub(crate) static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| {
        std::env::var("RAVA_M3_LAMBDA_DEDUP")
            .map(|value| {
                !matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "" | "0" | "false" | "no" | "off"
                )
            })
            .unwrap_or(true)
    })
}

/// Exact dedup key for a single captured Value: `(discriminant, i64 payload)`, or `None` for kinds
/// that can't be keyed exactly by an i64 (Str/Float/Double/sentinels) → those sites don't dedup. The
/// discriminant prevents `Ref(5)`/`Int(5)`/`Array(5)` from aliasing. Handle/int/long/arena-ptr
/// captures — the worldgen-dominant case — are exact.
pub(crate) fn lambda_capture_key(v: &Value) -> Option<(u8, i64)> {
    match v {
        // A captured Ref/Array handle roots its object (GC marks lambda.captured), so while the
        // cached lambda is live (pruned-at-GC otherwise) the handle can't be swept+reused → stable key.
        Value::Ref(h) => Some((1, *h as i64)),
        Value::Array(h) => Some((2, *h as i64)),
        Value::Int(i) => Some((3, *i as i64)),
        Value::Long(l) => Some((4, *l)),
        // ⛔ NOT Value::Obj: arena addresses are RECYCLED by the moving collector — a new arena object
        // placed at a freed address would collide with a stale cache key (same site + recycled addr) →
        // a FALSE dedup handing back the wrong closure. (Measured: the full boot's loot-table load hit
        // exactly this — a LootItemCondition site dispatched to a CubicSpline closure.) Skip arena
        // captures; the handle/int/long cases below are stable.
        Value::Null => Some((6, 0)),
        _ => None, // Str / Float / Double / arena Obj / sentinels — not a stable exact key
    }
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn arena_lambda_to_struct_inner(p: crate::arena::ArenaPtr) -> Option<crate::heap::Lambda> {
    let kind = crate::nextgen::arena_lambda_kind(p);
    let site = crate::nextgen::lambda_site_get(crate::nextgen::arena_lambda_site_idx(p))?;
    let n = crate::nextgen::arena_lambda_cap_n(p);
    let mut captured = Vec::with_capacity(n);
    for i in 0..n {
        let (c, k) = crate::nextgen::arena_lambda_cell(p, crate::nextgen::AL_HDR_CELLS + i);
        captured.push(decode_arena_cell(c, k));
    }
    Some(crate::heap::Lambda { kind, site, captured })
}

#[cfg(feature = "next-gen-gc")]
pub(crate) fn forward_stale_arena(p: crate::arena::ArenaPtr) -> Option<(crate::arena::ArenaPtr, u32)> {
    let fwd = crate::nextgen::forward_if_moved(p.addr() as i64);
    if fwd != p.addr() as i64 {
        let np = unsafe { crate::arena::ArenaPtr::from_addr(fwd as u64) };
        Some((np, crate::nextgen::arena_class_id(np)))
    } else {
        if trace_stalefield() {
            eprintln!(
                "[STALE-PTR] {:#x} young={} tenured={} fwd_unchanged (irrecoverable)",
                p.addr(),
                crate::nextgen::tlab_in_nursery(p),
                !crate::nextgen::tlab_in_nursery(p) && crate::nextgen::arena_contains(p.as_ptr()),
            );
        }
        None
    }
}
