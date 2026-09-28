//! LANED STORE-FORM CENSUS (`RAVA_LANE_STORE_CENSUS`, default OFF, diagnostic only).
//!
//! # Why this exists
//!
//! The lane proof census (`[LANE-PROOF]`) reports whether a method's WRITE SET was *proved*
//! closed by the structural walker. That verdict and the store forms the JIT actually EMITS
//! at each write site are different artifacts: an emitted site is callback-only, a
//! COW-guarded callback+inline diamond, an inline store under the ring-worker barrier, or a
//! bare inline form — and a COW `Disarmed` body (the method-entry UNSWITCH already settled
//! the arm flag for the whole invocation) emits no diamond at all without being unsafe. The
//! census correlates the two sides: emission records each `putfield`/`*astore` form per
//! (Code, PC, opcode), the dry-run proof collects the Code identities it visits, and
//! [`join`] renders the correspondence in the existing `[LANE-PROOF]` line.
//!
//! # What this is NOT
//!
//! Strictly diagnostic. Gated behind `RAVA_LANE_STORE_CENSUS`, off by default; every hook is
//! an early return and the table is never allocated when the flag is unset. It never affects
//! routing, admission, codegen, or runtime behavior, and it is NOT evidence of shared-
//! execution safety and NOT authority to remove `worker_locked_exec` — a store-form census
//! says nothing about whether the *interpreter* or the ring reaches a site.

use crate::jit::CowArm;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{LazyLock, Mutex};

/// Which store form the emitter generated at a site.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum StoreForm {
    /// The write goes through the `rava_putfield` host callback (native-backed class).
    CallbackOnly,
    /// COW armed: a per-site diamond routes the write to the callback when armed, inline
    /// otherwise. Selected under the existing method-entry arm branch.
    CowGuardedCallbackInline,
    /// Inline store emitted under the GILFREE-RINGS array-store worker barrier.
    InlineRingGuard,
    /// Inline store with no per-site guard emitted here. NOT by itself unsafe: with
    /// [`ArmState::Disarmed`] the COW method-entry UNSWITCH settled the arm flag for the
    /// whole invocation (no diamond needed); with the other arms it means the ring gate
    /// was off at compile time. The arm state, not the form alone, carries that meaning.
    InlineUnguarded,
}

/// Which method-entry arm state this emission was compiled under.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum ArmState {
    /// The unswitch hoisted the arm test into the entry block; this half is the ARMED body.
    Hoisted,
    /// No entry hoist: the site tests `[ctx+12]` itself.
    PerSite,
    /// The COW method-entry UNSWITCH proved this invocation disarmed: no COW code at all.
    /// Recorded separately so a disarmed body is never conflated with an unguarded arm.
    Disarmed,
}

fn arm_of(cow: CowArm) -> ArmState {
    match cow {
        CowArm::Hoisted(_) => ArmState::Hoisted,
        CowArm::PerSite => ArmState::PerSite,
        CowArm::Disarmed => ArmState::Disarmed,
    }
}

#[derive(Default)]
struct MethodRecord {
    /// The body reached emission — present even when the body has no store sites, so the
    /// join can distinguish compiled/no-store from a Code identity the emitter never saw.
    compiled: bool,
    /// (bytecode PC, opcode) → deduplicated (form, arm) pairs. Repeated identical
    /// emissions collapse; distinct arm/form evidence (e.g. the COW unswitch's disarmed
    /// AND armed halves of one site) is retained.
    sites: BTreeMap<(usize, u8), BTreeSet<(StoreForm, ArmState)>>,
}

#[cfg(test)]
static TEST_ON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub(crate) fn enabled() -> bool {
    #[cfg(test)]
    if TEST_ON.load(std::sync::atomic::Ordering::Relaxed) != 0 {
        return true;
    }
    static ON: LazyLock<bool> =
        LazyLock::new(|| std::env::var_os("RAVA_LANE_STORE_CENSUS").is_some());
    *ON
}

fn table() -> &'static Mutex<BTreeMap<usize, MethodRecord>> {
    static T: LazyLock<Mutex<BTreeMap<usize, MethodRecord>>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));
    &T
}

/// Record one emitted store form for `(code_key, pc, op)`. Diagnostics only; a zero
/// `code_key` (never a real pinned body) is refused.
pub(crate) fn record(code_key: usize, pc: usize, op: u8, form: StoreForm, arm: ArmState) {
    if code_key == 0 || !enabled() {
        return;
    }
    let mut g = table().lock().unwrap();
    let m = g.entry(code_key).or_default();
    m.compiled = true;
    m.sites.entry((pc, op)).or_default().insert((form, arm));
}

/// Mark `code_key` as emitted even when the body has no store sites.
pub(crate) fn record_compiled(code_key: usize) {
    if code_key == 0 || !enabled() {
        return;
    }
    table()
        .lock()
        .unwrap()
        .entry(code_key)
        .or_default()
        .compiled = true;
}

/// Emission-side hook for a `putfield` (0xb5): mirrors the arm selection in
/// `ops::fields` exactly (same expression, no intervening state), evaluated only when the
/// census gate is on.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn record_putfield(env: &crate::jit::Env, pc: usize, cow: CowArm) {
    if !enabled() {
        return;
    }
    let form = if env.callback_fields {
        StoreForm::CallbackOnly
    } else if crate::parallel::cow_gate() && !matches!(cow, CowArm::Disarmed) {
        StoreForm::CowGuardedCallbackInline
    } else {
        StoreForm::InlineUnguarded
    };
    record(env.code_key, pc, 0xb5, form, arm_of(cow));
}

/// Emission-side hook for a `*astore`: `ring_guarded` is `emit_ring_store_guard`'s
/// `Some`/`None` — `None` covers both the rings gate being off (config) and a COW
/// `Disarmed` body (no diamond needed), which the arm state separates.
#[cfg(feature = "next-gen-gc")]
pub(crate) fn record_array_store(
    env: &crate::jit::Env,
    pc: usize,
    op: u8,
    ring_guarded: bool,
    cow: CowArm,
) {
    if !enabled() {
        return;
    }
    let form = if ring_guarded {
        StoreForm::InlineRingGuard
    } else {
        StoreForm::InlineUnguarded
    };
    record(env.code_key, pc, op, form, arm_of(cow));
}

/// Join the proof's visited Code identities against the emit census. `None` when the
/// census gate is off (the `[LANE-PROOF]` line then renders exactly as before) or when no
/// identities were observed. Missing records are labeled UNKNOWN — never empty/safe.
pub(crate) fn join(codes: &[usize]) -> Option<String> {
    if !enabled() || codes.is_empty() {
        return None;
    }
    let mut sorted = codes.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let g = table().lock().unwrap();
    Some(
        sorted
            .iter()
            .map(|&k| describe(&g, k))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn describe(g: &BTreeMap<usize, MethodRecord>, code_key: usize) -> String {
    match g.get(&code_key) {
        None => format!("0x{code_key:x}=UNKNOWN"),
        Some(m) if m.sites.is_empty() => format!("0x{code_key:x}=compiled/no-store"),
        Some(m) => {
            let sites: Vec<String> = m
                .sites
                .iter()
                .map(|((pc, op), ev)| {
                    let forms: Vec<String> =
                        ev.iter().map(|(f, a)| format!("{f:?}/{a:?}")).collect();
                    format!("{pc}:{op:02x}[{}]", forms.join(","))
                })
                .collect();
            format!("0x{code_key:x}={}", sites.join(","))
        }
    }
}

#[cfg(test)]
pub(crate) mod testsupport {
    use super::TEST_ON;
    use std::sync::{LazyLock, Mutex};

    /// ONE serial lock shared by every census fact test in the crate: the table is
    /// global and the gate is a global flag, so tests in two modules can interleave
    /// (and flip the gate on each other) unless they share one lock.
    pub(crate) static SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    /// Arm the census for a test. Tests must clean up their own keys; the flag itself
    /// merely widens emission-side recording, which unit tests never trigger.
    pub(crate) fn force_on() {
        TEST_ON.store(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn force_off() {
        TEST_ON.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Test-only cleanup: remove keys a test seeded. Top-level so `lane_proof`'s tests
/// share the same table discipline; `MethodRecord` stays private.
#[cfg(test)]
pub(crate) fn remove_keys_for_tests(keys: &[usize]) {
    let mut g = table().lock().unwrap();
    for k in keys {
        g.remove(k);
    }
}

#[cfg(test)]
mod tests {
    use super::testsupport::SERIAL;
    use super::{
        join, record, record_compiled, remove_keys_for_tests, table, testsupport, ArmState,
        StoreForm,
    };

    const K1: usize = 0x7f5e_0001;
    const K2: usize = 0x7f5e_0002;

    /// Census tests must run serialized (see `testsupport::SERIAL`) — one global
    /// table, one global gate flag.
    struct Serial {
        _guard: std::sync::MutexGuard<'static, ()>,
    }
    impl Serial {
        fn take() -> Self {
            Serial {
                _guard: SERIAL.lock().unwrap(),
            }
        }
    }
    #[test]
    fn repeated_identical_forms_dedup_per_site() {
        let _s = Serial::take();
        testsupport::force_on();
        for _ in 0..3 {
            record(K1, 12, 0xb5, StoreForm::InlineUnguarded, ArmState::Disarmed);
        }
        let g = table().lock().unwrap();
        let ev = &g[&K1].sites[&(12, 0xb5)];
        assert_eq!(ev.len(), 1);
        assert!(ev.contains(&(StoreForm::InlineUnguarded, ArmState::Disarmed)));
        drop(g);
        remove_keys_for_tests(&[K1]);
    }

    #[test]
    fn distinct_arm_and_form_evidence_is_retained() {
        let _s = Serial::take();
        testsupport::force_on();
        // The COW unswitch emits the same site twice: disarmed (no diamond) and armed
        // (guarded diamond). Both must survive the dedup.
        record(K1, 44, 0x53, StoreForm::InlineUnguarded, ArmState::Disarmed);
        record(K1, 44, 0x53, StoreForm::InlineRingGuard, ArmState::Hoisted);
        let g = table().lock().unwrap();
        let ev = &g[&K1].sites[&(44, 0x53)];
        assert_eq!(ev.len(), 2);
        assert!(ev.contains(&(StoreForm::InlineUnguarded, ArmState::Disarmed)));
        assert!(ev.contains(&(StoreForm::InlineRingGuard, ArmState::Hoisted)));
        drop(g);
        remove_keys_for_tests(&[K1]);
    }

    #[test]
    fn distinct_code_and_bci_stay_separate() {
        let _s = Serial::take();
        testsupport::force_on();
        record(K1, 10, 0xb5, StoreForm::CallbackOnly, ArmState::PerSite);
        record(K1, 30, 0x51, StoreForm::InlineRingGuard, ArmState::Hoisted);
        record(K2, 10, 0xb5, StoreForm::InlineUnguarded, ArmState::PerSite);
        let g = table().lock().unwrap();
        assert_eq!(g[&K1].sites.len(), 2);
        assert_eq!(g[&K2].sites.len(), 1);
        assert!(g[&K2].sites.contains_key(&(10, 0xb5)));
        assert!(!g[&K2].sites.contains_key(&(30, 0x51)));
        drop(g);
        remove_keys_for_tests(&[K1, K2]);
    }

    #[test]
    fn join_labels_missing_records_unknown_and_no_store_compiled() {
        let _s = Serial::take();
        testsupport::force_on();
        record_compiled(K1); // emitted, but no store sites
                             // K2 left entirely unobserved by the emitter.
        let out = join(&[K1, K2]).expect("census armed, identities present");
        assert!(
            out.contains(&format!("0x{K1:x}=compiled/no-store")),
            "{out}"
        );
        assert!(out.contains(&format!("0x{K2:x}=UNKNOWN")), "{out}");
        // A site with forms is rendered with its form/arm evidence.
        record(K2, 8, 0x55, StoreForm::InlineRingGuard, ArmState::Hoisted);
        let out = join(&[K2]).unwrap();
        assert!(
            out.contains(&format!("0x{K2:x}=8:55[InlineRingGuard/Hoisted]")),
            "{out}"
        );
        remove_keys_for_tests(&[K1, K2]);
    }

    #[test]
    fn join_is_none_when_gate_off_or_no_identities() {
        let _s = Serial::take();
        testsupport::force_off();
        assert!(join(&[K1]).is_none());
        testsupport::force_on();
        assert!(join(&[]).is_none());
        testsupport::force_off();
    }
}
