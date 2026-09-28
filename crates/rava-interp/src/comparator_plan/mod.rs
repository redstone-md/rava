//! Structural execution plans for synthetic Java comparators.
//!
//! A `thenComparing` tree normally re-reads its `$cmp/$first/$second` heap fields
//! recursively on every comparison. Flattening that immutable synthetic tree
//! once per sort removes the dispatch tax while retaining the authoritative
//! extractor call count, order, short-circuiting, exceptions and allocations.
//! Unknown nodes always fall back to `Vm::compare`.




use crate::{Value, Vm, VmError};
use std::collections::HashSet;

const MAX_CLAUSES: usize = 16;

#[derive(Clone)]
struct KeyClause {
    function: Value,
    lambda: Option<crate::heap::Lambda>,
    reverse: bool,
}

fn apply_clause(
    vm: &mut Vm,
    clause: &KeyClause,
    argument: Value,
) -> Result<Option<Value>, VmError> {
    match &clause.lambda {
        Some(lambda) => vm.invoke_lambda_snapshot(lambda, vec![argument]),
        None => vm.apply(&clause.function, vec![argument]),
    }
}

fn directed(ordering: i32, reverse: bool) -> i32 {
    debug_assert!((-1..=1).contains(&ordering));
    if reverse {
        -ordering
    } else {
        ordering
    }
}

fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("RAVA_FLAT_COMPARATOR")
            .map(|value| value != "0")
            .unwrap_or(true)
    })
}

/// How often does a keyed comparator get refused because its key function is not a heap
/// handle, and how often does a plan get built at all? Two counters, because the first
/// number alone cannot say whether the refusal is the common case or a rounding error.
pub static KEYFN_NOT_REF: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static PLANS_BUILT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn cmp_plan_census_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAVA_CMP_PLAN_CENSUS").is_some())
}

/// ARENA-LAMBDA DISPATCH SPLIT (`RAVA_CMP_PLAN_CENSUS`, same gate — both answer "which path
/// did this receiver take"). `arena_lambda_enabled`'s contract says an arena lambda's class is
/// `[arena-lambda:…]`, never `$Lambda`, that the λ-PIC's REF_TAG guard rejects a non-handle
/// receiver, and that such a call therefore falls to the general `invoke_lambda`/`apply`
/// dispatch. But `lib.rs` DOES admit an `arena_lam` arm into the host-side direct-call entry
/// build. Prose and code disagree, and reading either one harder is what produced seven
/// retractions in the last arena-lambda hunt — so count both ends instead.
pub static AL_DIRECT_ENTRY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static AL_GENERAL_APPLY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn lambda_snapshot_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("RAVA_COMPARATOR_LAMBDA_SNAPSHOT")
            .map(|value| value != "0")
            .unwrap_or(true)
    })
}
mod plan;
pub(crate) use plan::*;
mod sort;
pub(crate) use sort::*;
mod tests;
pub(crate) use tests::*;
