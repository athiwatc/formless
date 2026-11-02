//! Paired held-out replay statistics, not a claim of population calibration.
//!
//! The exact McNemar test conditions on discordant pairs and uses the two-sided
//! binomial tail under p=1/2. Concordant outcomes affect scores/intervals/sample
//! size, not its p-value. Independence, frozen holdout selection and leakage
//! prevention are caller responsibilities; statistical significance alone is
//! insufficient for promotion (schema/capability/resource gates still apply).
use serde::Serialize;

/// Every count and total must be exactly representable as a binary64 integer.
pub const MAX_OBSERVATIONS: u64 = 1 << 53;
/// Bounds exact-tail work to at most one million recurrence iterations.
pub const MAX_DISCORDANT_PAIRS: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PairedCounts {
    pub both_pass: i64,
    /// Candidate passes and baseline fails (n10).
    pub candidate_only: i64,
    /// Baseline passes and candidate fails (n01).
    pub baseline_only: i64,
    pub both_fail: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct GateCriteria {
    pub minimum_n: u64,
    pub alpha: f64,
    /// Fractional margin (e.g. 0.05 means five percentage points).
    pub minimum_improvement: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateReason {
    InsufficientHoldout,
    NoDiscordance,
    NoPositiveImprovement,
    BelowMargin,
    NotSignificant,
    SignificantImprovement,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PairedReport {
    pub n: u64,
    pub baseline_passes: u64,
    pub candidate_passes: u64,
    pub baseline_rate: f64,
    pub candidate_rate: f64,
    /// Direct discordant count difference / n, not subtraction of rounded rates.
    pub improvement: f64,
    /// Fixed 95% Wilson score intervals, independent of configured test alpha.
    pub baseline_ci: (f64, f64),
    pub candidate_ci: (f64, f64),
    pub discordant: u64,
    pub baseline_only: u64,
    pub candidate_only: u64,
    pub p_value: f64,
    /// Retains meaningful evidence when p_value underflows binary64 to zero.
    pub log_p_value: f64,
    pub p_value_underflow: bool,
    pub criteria: GateCriteria,
    pub eligible_auto: bool,
    pub reason: GateReason,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatisticsError {
    InvalidCounts,
    CountOverflow,
    TooManyObservations,
    TooManyDiscordantPairs,
    InvalidCriteria,
    NumericalFailure,
}
impl std::fmt::Display for StatisticsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidCounts => "Paired counts must be nonnegative with a nonempty sample",
            Self::CountOverflow => "Paired count sum overflowed",
            Self::TooManyObservations => "Replay sample exceeds the exact integer representation bound",
            Self::TooManyDiscordantPairs => "Discordant replay pairs exceed the bounded exact-test work limit",
            Self::InvalidCriteria => "Replay criteria need minimum_n >= 1, finite 0 < alpha < 1, and a finite margin in [0, 1]",
            Self::NumericalFailure => "Replay statistical calculation was not finite and valid",
        })
    }
}
impl std::error::Error for StatisticsError {}

/// Canonical 95% Wilson interval. Invalid data never turns into a confidence
/// claim. Legacy callers may explicitly render (0,1) for an unknown interval.
pub fn wilson_interval(passes: i64, n: i64) -> Result<(f64, f64), StatisticsError> {
    if n <= 0 || passes < 0 || passes > n {
        return Err(StatisticsError::InvalidCounts);
    }
    if n as u64 > MAX_OBSERVATIONS {
        return Err(StatisticsError::TooManyObservations);
    }
    let (n, p, z) = (n as f64, passes as f64 / n as f64, 1.96f64);
    let denominator = 1.0 + z * z / n;
    let centre = (p + z * z / (2.0 * n)) / denominator;
    let half = (z / denominator) * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt();
    let interval = ((centre - half).max(0.0), (centre + half).min(1.0));
    if !interval.0.is_finite() || !interval.1.is_finite() || interval.0 > interval.1 {
        return Err(StatisticsError::NumericalFailure);
    }
    Ok(interval)
}

struct Probability {
    value: f64,
    log: f64,
    underflow: bool,
}
/// Kahan summation keeps long positive recurrence sums stable without factorial
/// overflow, large coefficient allocation, a normal approximation or a dependency.
fn add(sum: &mut f64, correction: &mut f64, value: f64) {
    let adjusted = value - *correction;
    let next = *sum + adjusted;
    *correction = (next - *sum) - adjusted;
    *sum = next;
}
fn paired_probability(
    baseline_only: u64,
    candidate_only: u64,
) -> Result<Probability, StatisticsError> {
    let d = baseline_only
        .checked_add(candidate_only)
        .ok_or(StatisticsError::CountOverflow)?;
    if d > MAX_DISCORDANT_PAIRS {
        return Err(StatisticsError::TooManyDiscordantPairs);
    }
    let k = baseline_only.min(candidate_only);
    if d == 0 || baseline_only.abs_diff(candidate_only) <= 1 {
        return Ok(Probability {
            value: 1.0,
            log: 0.0,
            underflow: false,
        });
    }
    if d <= 60 {
        // Exact integer coefficients/tail, followed by one binary division.
        // Coefficient multiplication is bounded far below u128::MAX at d<=60.
        let mut coefficient = 1u128;
        let mut tail = 1u128;
        for index in 1..=k {
            coefficient = coefficient * (d - index + 1) as u128 / index as u128;
            tail += coefficient;
        }
        let p = ((2 * tail) as f64 / (1u128 << d) as f64).min(1.0);
        return Ok(Probability {
            value: p,
            log: p.ln(),
            underflow: false,
        });
    }
    // log(P[X=k]) = log(C(d,k)) - d*log(2). Compute the binomial
    // coefficient in log-space; log1p avoids cancellation near the centre.
    let (mut log_coefficient, mut correction) = (0.0, 0.0);
    for index in 1..=k {
        let log_ratio = (((d - 2 * index + 1) as f64) / index as f64).ln_1p();
        add(&mut log_coefficient, &mut correction, log_ratio);
    }
    // Sum P[X=j]/P[X=k] downwards. Terms never exceed one, avoiding both
    // overflow and the 2^-d underflow of a forward tail recurrence.
    let (mut relative_sum, mut relative_correction, mut term) = (1.0, 0.0, 1.0);
    for index in (1..=k).rev() {
        term *= index as f64 / (d - index + 1) as f64;
        add(&mut relative_sum, &mut relative_correction, term);
        if term == 0.0 {
            break;
        }
    }
    let log = (std::f64::consts::LN_2 + log_coefficient - d as f64 * std::f64::consts::LN_2
        + relative_sum.ln())
    .min(0.0);
    let value = log.exp();
    if !log.is_finite() || !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(StatisticsError::NumericalFailure);
    }
    Ok(Probability {
        value,
        log,
        underflow: value == 0.0,
    })
}

/// Evaluate one frozen set of identical-input baseline/candidate replay pairs.
/// Counts from independent/unpaired samples must not be supplied here.
/// Errors are evidence insufficiency, never permission for auto-promotion.
pub fn evaluate(
    counts: PairedCounts,
    criteria: GateCriteria,
) -> Result<PairedReport, StatisticsError> {
    if criteria.minimum_n == 0
        || criteria.minimum_n > MAX_OBSERVATIONS
        || !criteria.alpha.is_finite()
        || criteria.alpha <= 0.0
        || criteria.alpha >= 1.0
        || !criteria.minimum_improvement.is_finite()
        || !(0.0..=1.0).contains(&criteria.minimum_improvement)
    {
        return Err(StatisticsError::InvalidCriteria);
    }
    let values = [
        counts.both_pass,
        counts.candidate_only,
        counts.baseline_only,
        counts.both_fail,
    ];
    let mut converted = [0u64; 4];
    for (target, value) in converted.iter_mut().zip(values) {
        *target = u64::try_from(value).map_err(|_| StatisticsError::InvalidCounts)?;
    }
    let [both_pass, candidate_only, baseline_only, both_fail] = converted;
    let n = [both_pass, candidate_only, baseline_only, both_fail]
        .into_iter()
        .try_fold(0u64, |total, count| total.checked_add(count))
        .ok_or(StatisticsError::CountOverflow)?;
    if n == 0 {
        return Err(StatisticsError::InvalidCounts);
    }
    if n > MAX_OBSERVATIONS {
        return Err(StatisticsError::TooManyObservations);
    }
    let discordant = candidate_only
        .checked_add(baseline_only)
        .ok_or(StatisticsError::CountOverflow)?;
    let probability = paired_probability(baseline_only, candidate_only)?;
    let baseline_passes = both_pass + baseline_only;
    let candidate_passes = both_pass + candidate_only;
    let improvement = (candidate_only as f64 - baseline_only as f64) / n as f64;
    let reason = if n < criteria.minimum_n {
        GateReason::InsufficientHoldout
    } else if discordant == 0 {
        GateReason::NoDiscordance
    } else if candidate_only <= baseline_only {
        GateReason::NoPositiveImprovement
    } else if improvement < criteria.minimum_improvement {
        GateReason::BelowMargin
    } else if probability.value > criteria.alpha {
        GateReason::NotSignificant
    } else {
        GateReason::SignificantImprovement
    };
    Ok(PairedReport {
        n,
        baseline_passes,
        candidate_passes,
        baseline_rate: baseline_passes as f64 / n as f64,
        candidate_rate: candidate_passes as f64 / n as f64,
        improvement,
        baseline_ci: wilson_interval(baseline_passes as i64, n as i64)?,
        candidate_ci: wilson_interval(candidate_passes as i64, n as i64)?,
        discordant,
        baseline_only,
        candidate_only,
        p_value: probability.value,
        log_p_value: probability.log,
        p_value_underflow: probability.underflow,
        criteria,
        eligible_auto: reason == GateReason::SignificantImprovement,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn criteria() -> GateCriteria {
        GateCriteria {
            minimum_n: 1,
            alpha: 0.05,
            minimum_improvement: 0.0,
        }
    }
    fn counts(wins: i64, losses: i64) -> PairedCounts {
        PairedCounts {
            candidate_only: wins,
            baseline_only: losses,
            ..Default::default()
        }
    }
    fn near(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{actual} != {expected} within {tolerance}"
        );
    }
    #[test]
    fn five_small_wins_are_not_significant_but_six_are() {
        let five = evaluate(counts(5, 0), criteria()).unwrap();
        assert_eq!(five.p_value, 0.0625);
        assert!(!five.eligible_auto);
        assert_eq!(five.reason, GateReason::NotSignificant);
        let six = evaluate(counts(6, 0), criteria()).unwrap();
        assert_eq!(six.p_value, 0.03125);
        assert!(six.eligible_auto);
        assert_eq!((six.baseline_passes, six.candidate_passes), (0, 6));
        assert_eq!((six.baseline_only, six.candidate_only), (0, 6));
        assert_eq!(six.discordant, 6);
        assert_eq!(six.improvement, 1.0);
    }
    #[test]
    fn exact_two_sided_discordant_table_and_swap_symmetry() {
        for (wins, losses, expected) in [
            (1, 0, 1.0),
            (2, 0, 0.5),
            (3, 0, 0.25),
            (4, 0, 0.125),
            (6, 1, 0.125),
            (8, 2, 0.109375),
            (10, 0, 0.001953125),
            (5, 5, 1.0),
        ] {
            let positive = evaluate(counts(wins, losses), criteria()).unwrap();
            let negative = evaluate(counts(losses, wins), criteria()).unwrap();
            assert_eq!(positive.p_value, expected);
            assert_eq!(negative.p_value, expected);
            assert!(!negative.eligible_auto);
        }
    }
    #[test]
    fn concordance_changes_scores_not_p_value_and_no_discordance_is_not_evidence() {
        let mut pair = counts(6, 0);
        pair.both_pass = 80;
        pair.both_fail = 14;
        let report = evaluate(pair, criteria()).unwrap();
        assert_eq!(report.n, 100);
        assert_eq!(report.baseline_rate, 0.8);
        assert_eq!(report.candidate_rate, 0.86);
        assert_eq!(report.improvement, 0.06);
        assert_eq!(report.p_value, 0.03125);
        let report = evaluate(
            PairedCounts {
                both_pass: 100,
                ..Default::default()
            },
            criteria(),
        )
        .unwrap();
        assert_eq!(report.p_value, 1.0);
        assert_eq!(report.log_p_value, 0.0);
        assert!(!report.eligible_auto);
        assert_eq!(report.reason, GateReason::NoDiscordance);
    }
    #[test]
    fn minimum_sample_positive_gain_margin_and_alpha_are_all_required() {
        let mut gate = criteria();
        gate.minimum_n = 20;
        let report = evaluate(counts(6, 0), gate).unwrap();
        assert_eq!(report.reason, GateReason::InsufficientHoldout);
        let mut pair = counts(6, 0);
        pair.both_pass = 14;
        assert!(evaluate(pair, gate).unwrap().eligible_auto);
        pair.candidate_only = 0;
        pair.baseline_only = 6;
        assert_eq!(
            evaluate(pair, gate).unwrap().reason,
            GateReason::NoPositiveImprovement
        );
        pair = counts(6, 0);
        pair.both_pass = 94;
        gate.minimum_improvement = 0.07;
        assert_eq!(
            evaluate(pair, gate).unwrap().reason,
            GateReason::BelowMargin
        );
        gate.minimum_improvement = 0.06;
        assert!(evaluate(pair, gate).unwrap().eligible_auto);
        gate.alpha = 0.01;
        assert_eq!(
            evaluate(pair, gate).unwrap().reason,
            GateReason::NotSignificant
        );
        gate = criteria();
        gate.alpha = 0.0625;
        assert!(evaluate(counts(5, 0), gate).unwrap().eligible_auto);
    }
    #[test]
    fn checked_wilson_matches_known_intervals_and_boundaries() {
        let zero = wilson_interval(0, 10).unwrap();
        near(zero.0, 0.0, 1e-15);
        near(zero.1, 0.2775401687666166, 1e-14);
        let half = wilson_interval(50, 100).unwrap();
        near(half.0, 0.40382982859014716, 1e-14);
        near(half.1, 0.5961701714098528, 1e-14);
        let all = wilson_interval(10, 10).unwrap();
        near(all.0, 1.0 - zero.1, 1e-14);
        near(all.1, 1.0, 1e-15);
        for (passes, n) in [(-1, 10), (11, 10), (0, 0), (0, -1)] {
            assert!(wilson_interval(passes, n).is_err());
        }
    }
    #[test]
    fn invalid_counts_overflow_and_invalid_criteria_never_produce_a_report() {
        for bad in [
            PairedCounts::default(),
            counts(-1, 0),
            PairedCounts {
                both_pass: -1,
                ..counts(6, 0)
            },
            PairedCounts {
                both_fail: -1,
                ..counts(6, 0)
            },
            counts(0, -1),
        ] {
            assert_eq!(
                evaluate(bad, criteria()).unwrap_err(),
                StatisticsError::InvalidCounts
            );
        }
        let overflow = PairedCounts {
            both_pass: i64::MAX,
            candidate_only: i64::MAX,
            baseline_only: i64::MAX,
            both_fail: i64::MAX,
        };
        assert_eq!(
            evaluate(overflow, criteria()).unwrap_err(),
            StatisticsError::CountOverflow
        );
        for alpha in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 0.0, 1.0] {
            assert_eq!(
                evaluate(
                    counts(6, 0),
                    GateCriteria {
                        alpha,
                        ..criteria()
                    }
                )
                .unwrap_err(),
                StatisticsError::InvalidCriteria
            );
        }
        for minimum_improvement in [f64::NAN, f64::INFINITY, -0.1, 1.01] {
            assert!(evaluate(
                counts(6, 0),
                GateCriteria {
                    minimum_improvement,
                    ..criteria()
                }
            )
            .is_err());
        }
        assert!(evaluate(
            counts(6, 0),
            GateCriteria {
                minimum_n: 0,
                ..criteria()
            }
        )
        .is_err());
    }
    #[test]
    fn large_samples_are_finite_symmetric_and_preserve_count_derived_gain() {
        let positive = evaluate(counts(500_100, 499_900), criteria()).unwrap();
        let negative = evaluate(counts(499_900, 500_100), criteria()).unwrap();
        assert!(positive.p_value > 0.8 && positive.p_value < 0.9);
        assert_eq!(positive.p_value, negative.p_value);
        let balanced = evaluate(counts(500_000, 500_000), criteria()).unwrap();
        assert_eq!(balanced.p_value, 1.0);
        let central_odd = evaluate(
            counts(500_000, 499_999),
            GateCriteria {
                alpha: 0.999999999999,
                ..criteria()
            },
        )
        .unwrap();
        assert_eq!(central_odd.p_value, 1.0);
        assert!(!central_odd.eligible_auto);
        near(positive.p_value, 0.8422627570516166, 2e-8);
        let total = MAX_OBSERVATIONS as i64;
        let report = evaluate(
            PairedCounts {
                both_pass: total - 6,
                ..counts(6, 0)
            },
            criteria(),
        )
        .unwrap();
        assert!(report.improvement > 0.0);
        assert_eq!(report.improvement, 6.0 / MAX_OBSERVATIONS as f64);
        for interval in [report.baseline_ci, report.candidate_ci] {
            assert!(interval.0.is_finite() && interval.1.is_finite());
            assert!((0.0..=1.0).contains(&interval.0));
            assert!((0.0..=1.0).contains(&interval.1));
            assert!(interval.0 <= interval.1);
        }
    }
    #[test]
    fn tail_underflow_is_honest_and_work_limits_fail_closed() {
        let report = evaluate(counts(10_000, 0), criteria()).unwrap();
        assert_eq!(report.p_value, 0.0);
        assert!(report.p_value_underflow);
        assert!(report.log_p_value.is_finite() && report.log_p_value < -6000.0);
        assert!(report.eligible_auto);
        assert_eq!(
            evaluate(counts(MAX_DISCORDANT_PAIRS as i64 + 1, 0), criteria()).unwrap_err(),
            StatisticsError::TooManyDiscordantPairs
        );
        assert_eq!(
            evaluate(
                PairedCounts {
                    both_pass: MAX_OBSERVATIONS as i64,
                    ..counts(1, 0)
                },
                criteria()
            )
            .unwrap_err(),
            StatisticsError::TooManyObservations
        );
        assert!(wilson_interval(1, MAX_OBSERVATIONS as i64 + 1).is_err());
    }
    #[test]
    fn recurrence_across_integer_path_boundary_is_monotonic_and_json_is_finite() {
        let sixty = evaluate(counts(60, 0), criteria()).unwrap();
        let sixty_one = evaluate(counts(61, 0), criteria()).unwrap();
        near(sixty.p_value, 2f64.powi(-59), 1e-30);
        near(sixty_one.p_value, 2f64.powi(-60), 1e-30);
        assert!(sixty_one.p_value < sixty.p_value);
        let report = evaluate(
            PairedCounts {
                both_pass: 35,
                both_fail: 15,
                ..counts(31, 19)
            },
            criteria(),
        )
        .unwrap();
        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json["n"], 100);
        assert!(json["p_value"].is_number());
        assert!(json["candidate_ci"].is_array());
        assert!(json["reason"].is_string());
    }
}
