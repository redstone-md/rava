//! Scoped facts collected while probing worker-lane admission.

use crate::jit::emit::store_census;

#[derive(Default)]
pub(super) struct LaneProofFacts {
    pub(super) dispatch_open: bool,
    pub(super) write_set_open: bool,
    /// ADDITIVE, diagnostic only (`RAVA_LANE_STORE_CENSUS`): every bytecode `Code`
    /// identity the recursive proof visited, root included — joined against the emit-side
    /// store census at report time. Inert when the census gate is off.
    pub(super) observed_codes: Vec<usize>,
}

pub(crate) struct LaneProofCensusReport {
    pub(crate) dispatch_closed: bool,
    pub(crate) class_load_closed: bool,
    pub(crate) write_set_closed: bool,
    pub(crate) dry_loaded: usize,
    pub(crate) dry_names: Vec<String>,
    pub(crate) reason: Option<String>,
    /// ADDITIVE, diagnostic only (`RAVA_LANE_STORE_CENSUS`): the emit-side store-form
    /// census joined against the Code identities this proof visited. `None` when the
    /// census gate is off — the `[LANE-PROOF]` line then renders exactly as before.
    /// Missing emitted-method records are labeled UNKNOWN, never empty/safe. Evidence of
    /// store FORM only; not a concurrency proof and not authority to remove
    /// `worker_locked_exec`.
    pub(crate) store_join: Option<String>,
}

thread_local! {
    static LANE_PROOF_FACTS: std::cell::RefCell<Option<LaneProofFacts>> =
        const { std::cell::RefCell::new(None) };
}

struct LaneProofFactsRestore(Option<LaneProofFacts>);

impl Drop for LaneProofFactsRestore {
    fn drop(&mut self) {
        LANE_PROOF_FACTS.with(|facts| {
            facts.replace(self.0.take());
        });
    }
}

pub(super) fn note_dispatch_open() {
    LANE_PROOF_FACTS.with(|facts| {
        if let Some(facts) = facts.borrow_mut().as_mut() {
            facts.dispatch_open = true;
        }
    });
}

pub(super) fn note_write_set_open() {
    LANE_PROOF_FACTS.with(|facts| {
        if let Some(facts) = facts.borrow_mut().as_mut() {
            facts.write_set_open = true;
        }
    });
}

/// ADDITIVE, diagnostic only: record a bytecode `Code` identity the proof visited. Inert
/// unless the store census gate is on (then it costs one `Vec::push` per visited body).
pub(super) fn note_observed_code(code_key: usize) {
    if code_key == 0 || !store_census::enabled() {
        return;
    }
    LANE_PROOF_FACTS.with(|facts| {
        if let Some(facts) = facts.borrow_mut().as_mut() {
            facts.observed_codes.push(code_key);
        }
    });
}

pub(super) fn with_lane_proof_facts<R>(f: impl FnOnce() -> R) -> (R, LaneProofFacts) {
    let previous = LANE_PROOF_FACTS.with(|facts| facts.replace(Some(LaneProofFacts::default())));
    let _restore = LaneProofFactsRestore(previous);
    let result = f();
    let facts = LANE_PROOF_FACTS
        .with(|facts| facts.replace(None))
        .unwrap_or_default();
    (result, facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jit::emit::store_census::testsupport::SERIAL;
    use crate::jit::emit::store_census::{ArmState, StoreForm, testsupport};

    #[test]
    fn observed_codes_join_labels_missing_records_unknown() {
        let _s = SERIAL.lock().unwrap();
        testsupport::force_on();
        const K: usize = 0x7f5e_0042;
        let ((), facts) = with_lane_proof_facts(|| {
            note_observed_code(K);
        });
        assert_eq!(facts.observed_codes, vec![K]);
        // The emit census never saw this body: the join must label it UNKNOWN, not
        // compiled/no-store and not safe/empty.
        let joined = crate::jit::emit::store_census::join(&facts.observed_codes).unwrap();
        assert!(joined.contains(&format!("0x{K:x}=UNKNOWN")), "{joined}");
        testsupport::force_off();
    }

    #[test]
    fn observed_codes_distinguish_compiled_no_store_from_missing() {
        let _s = SERIAL.lock().unwrap();
        testsupport::force_on();
        const K_NO_STORE: usize = 0x7f5e_0043;
        const K_MISSING: usize = 0x7f5e_0044;
        const K_STORED: usize = 0x7f5e_0045;
        store_census::record_compiled(K_NO_STORE);
        store_census::record(
            K_STORED,
            20,
            0xb5,
            StoreForm::CowGuardedCallbackInline,
            ArmState::PerSite,
        );
        let ((), facts) = with_lane_proof_facts(|| {
            note_observed_code(K_NO_STORE);
            note_observed_code(K_MISSING);
            note_observed_code(K_STORED);
        });
        let joined = crate::jit::emit::store_census::join(&facts.observed_codes).unwrap();
        assert!(
            joined.contains(&format!("0x{K_NO_STORE:x}=compiled/no-store")),
            "{joined}"
        );
        assert!(joined.contains(&format!("0x{K_MISSING:x}=UNKNOWN")), "{joined}");
        assert!(
            joined.contains(&format!("0x{K_STORED:x}=20:b5[CowGuardedCallbackInline/PerSite]")),
            "{joined}"
        );
        // Cleanup: the census-table keys this test seeded, so another census run
        // starts empty.
        store_census::remove_keys_for_tests(&[K_NO_STORE, K_STORED]);
        testsupport::force_off();
    }

    #[test]
    fn observed_codes_inert_when_census_gate_off() {
        let _s = SERIAL.lock().unwrap();
        testsupport::force_off();
        let ((), facts) = with_lane_proof_facts(|| {
            note_observed_code(0x1234_5678);
        });
        assert!(facts.observed_codes.is_empty());
    }

    #[test]
    fn lane_proof_facts_restore_after_unwind() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_lane_proof_facts(|| panic!("expected test unwind"));
        }));

        assert!(result.is_err());
        assert!(LANE_PROOF_FACTS.with(|facts| facts.borrow().is_none()));
    }
}
