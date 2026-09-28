//! Generic mark-sweep rearm policy.
//!
//! The default uses the measured 1.5x post-live trigger. Setting
//! RAVA_GC_REARM_TIGHT=0 restores the historical 2x policy for bisection.

use std::sync::OnceLock;

const MIB: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GcRearmPolicy {
    growth_numerator: usize,
    growth_denominator: usize,
    minimum_slack: usize,
}

impl GcRearmPolicy {
    const STANDARD: Self = Self {
        growth_numerator: 1,
        growth_denominator: 1,
        minimum_slack: 256 * MIB,
    };

    const TIGHT: Self = Self {
        growth_numerator: 1,
        growth_denominator: 2,
        minimum_slack: 128 * MIB,
    };

    fn active() -> &'static Self {
        static POLICY: OnceLock<GcRearmPolicy> = OnceLock::new();
        POLICY.get_or_init(|| {
            let configured = std::env::var("RAVA_GC_REARM_TIGHT").ok();
            if tight_from_value(configured.as_deref()) {
                Self::TIGHT
            } else {
                Self::STANDARD
            }
        })
    }

    fn target(self, post_live: usize) -> usize {
        let proportional =
            post_live.saturating_mul(self.growth_numerator) / self.growth_denominator;
        post_live.saturating_add(proportional.max(self.minimum_slack))
    }
}

pub(crate) fn rearm_target(post_live: usize) -> usize {
    GcRearmPolicy::active().target(post_live)
}

pub(crate) fn tight_enabled() -> bool {
    *GcRearmPolicy::active() == GcRearmPolicy::TIGHT
}

pub(crate) fn trace_enabled() -> bool {
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("RAVA_GC_REARM_TRACE").is_some())
}

fn tight_from_value(value: Option<&str>) -> bool {
    !matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("0" | "false" | "no" | "off")
    )
}

#[cfg(test)]
mod tests {
    use super::{tight_from_value, GcRearmPolicy, MIB};

    #[test]
    fn standard_policy_preserves_two_x_rearm_above_floor() {
        assert_eq!(GcRearmPolicy::STANDARD.target(512 * MIB), 1024 * MIB);
    }

    #[test]
    fn tight_policy_uses_one_and_a_half_x_rearm_above_floor() {
        assert_eq!(GcRearmPolicy::TIGHT.target(512 * MIB), 768 * MIB);
    }

    #[test]
    fn both_policies_keep_their_small_heap_floor() {
        assert_eq!(GcRearmPolicy::STANDARD.target(64 * MIB), 320 * MIB);
        assert_eq!(GcRearmPolicy::TIGHT.target(64 * MIB), 192 * MIB);
    }

    #[test]
    fn tight_policy_is_default_on_with_an_explicit_false_kill_switch() {
        assert!(tight_from_value(None));
        assert!(tight_from_value(Some("1")));
        assert!(!tight_from_value(Some("false")));
        assert!(!tight_from_value(Some("OFF")));
    }
}
