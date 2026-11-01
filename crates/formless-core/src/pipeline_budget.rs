//! Pure resolution of an explicit pipeline Run limit against fresh profile
//! limits. No configuration, Store, provider, registry or billing access lives
//! here. Resolve again after waiting for admission, using the same immutable
//! RunLimit and the newly loaded profile limits.
use anyhow::{ensure, Result};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RunLimit {
    /// Existing profile behavior, including the existing Resume waiver.
    Profile,
    /// A per-Run override. Zero is a real prohibition, never "unset".
    Explicit(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EffectiveLimits {
    pub run_usd: Option<f64>,
    pub daily_usd: Option<f64>,
    pub explicit: bool,
}

impl EffectiveLimits {
    /// An explicit zero must be refused before dispatch. It remains capped
    /// even though legacy profile zero means that the limit is disabled.
    pub fn explicit_zero(&self) -> bool {
        self.explicit && self.run_usd == Some(0.0)
    }

    pub fn capped(&self) -> bool {
        self.explicit_zero()
            || self.run_usd.is_some_and(|limit| limit > 0.0)
            || self.daily_usd.is_some_and(|limit| limit > 0.0)
    }
}

pub(crate) fn resolve_limits(
    limit: RunLimit,
    profile_run: Option<f64>,
    profile_daily: Option<f64>,
    waived: bool,
) -> Result<EffectiveLimits> {
    // Match the existing admission interpretation of profile zero/off. This
    // helper does not broaden or replace profile-setting validation.
    let positive = |limit: Option<f64>| limit.filter(|limit| *limit > 0.0);
    match limit {
        RunLimit::Profile if waived => Ok(EffectiveLimits {
            run_usd: None,
            daily_usd: None,
            explicit: false,
        }),
        RunLimit::Profile => Ok(EffectiveLimits {
            run_usd: positive(profile_run),
            daily_usd: positive(profile_daily),
            explicit: false,
        }),
        RunLimit::Explicit(max) => {
            ensure!(
                max.is_finite() && max >= 0.0,
                "--max-usd must be a finite nonnegative USD amount"
            );
            // A Session's Resume waiver cannot erase an explicit pipeline
            // limit or its independent daily profile cap.
            Ok(EffectiveLimits {
                run_usd: Some(max),
                daily_usd: positive(profile_daily),
                explicit: true,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_defaults_and_waivers_keep_existing_semantics() {
        let off = resolve_limits(RunLimit::Profile, None, None, false).unwrap();
        assert_eq!(
            off,
            EffectiveLimits {
                run_usd: None,
                daily_usd: None,
                explicit: false
            }
        );
        assert!(!off.capped());
        let on = resolve_limits(RunLimit::Profile, Some(2.0), Some(5.0), false).unwrap();
        assert_eq!((on.run_usd, on.daily_usd), (Some(2.0), Some(5.0)));
        assert!(on.capped());
        let waived = resolve_limits(RunLimit::Profile, Some(2.0), Some(5.0), true).unwrap();
        assert_eq!(waived, off);
        let profile_zero = resolve_limits(RunLimit::Profile, Some(0.0), Some(0.0), false).unwrap();
        assert_eq!(profile_zero, off);
        assert!(!profile_zero.explicit_zero());
    }

    #[test]
    fn explicit_run_override_can_raise_lower_or_supply_a_cap_but_never_replace_daily() {
        for waived in [false, true] {
            for (global, explicit) in [(Some(1.0), 3.0), (Some(3.0), 1.0), (None, 2.0)] {
                let limits =
                    resolve_limits(RunLimit::Explicit(explicit), global, Some(5.0), waived)
                        .unwrap();
                assert_eq!(limits.run_usd, Some(explicit));
                assert_eq!(limits.daily_usd, Some(5.0));
                assert!(limits.explicit && limits.capped());
                assert!(!limits.explicit_zero());
            }
        }
    }

    #[test]
    fn explicit_zero_stays_a_real_cap_even_with_no_profile_limit_or_a_resume_waiver() {
        for waived in [false, true] {
            for global in [None, Some(0.0), Some(1.0)] {
                let limits = resolve_limits(RunLimit::Explicit(0.0), global, None, waived).unwrap();
                assert_eq!(limits.run_usd, Some(0.0));
                assert!(limits.explicit_zero() && limits.capped());
            }
        }
        let daily = resolve_limits(RunLimit::Explicit(0.0), None, Some(5.0), true).unwrap();
        assert_eq!(daily.daily_usd, Some(5.0));
    }

    #[test]
    fn invalid_explicit_limits_never_fall_back_to_profile_or_waived_behavior() {
        for max in [-0.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for waived in [false, true] {
                assert!(
                    resolve_limits(RunLimit::Explicit(max), Some(1.0), Some(5.0), waived).is_err()
                );
            }
        }
    }

    #[test]
    fn admission_reresolves_fresh_profile_limits_after_waiting_without_changing_run_policy() {
        let policy = RunLimit::Explicit(0.75);
        let before = resolve_limits(policy, Some(0.25), Some(5.0), false).unwrap();
        let after = resolve_limits(policy, Some(100.0), Some(0.50), true).unwrap();
        assert_eq!(before.run_usd, after.run_usd);
        assert_eq!(after.run_usd, Some(0.75));
        assert_eq!(after.daily_usd, Some(0.50));
        assert!(after.explicit && after.capped());
        let removed = resolve_limits(policy, None, None, true).unwrap();
        assert_eq!(removed.run_usd, Some(0.75));
        assert_eq!(removed.daily_usd, None);
        let profile_before = resolve_limits(RunLimit::Profile, None, None, false).unwrap();
        let profile_after = resolve_limits(RunLimit::Profile, Some(1.0), Some(2.0), false).unwrap();
        assert!(!profile_before.capped());
        assert!(profile_after.capped());
    }
}
