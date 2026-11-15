//! Shared success-rate labels for CLI and TUI reports.

/// Round to a whole percent, reserving `100%` for a perfect success rate.
/// An imperfect rate that rounds up to 100 is shown as `>99%` instead.
pub fn label(rate: f64) -> String {
    let rounded = format!("{:.0}%", rate * 100.0);
    if rate < 1.0 && rounded == "100%" {
        ">99%".into()
    } else {
        rounded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rare_failures_remain_distinct_from_perfect_success() {
        for (rate, expected) in [
            (0.9949, "99%"),
            (0.995, ">99%"),
            (0.9999, ">99%"),
            (1999.0 / 2000.0, ">99%"),
            (f64::from_bits(1.0f64.to_bits() - 1), ">99%"),
            (1.0, "100%"),
        ] {
            assert_eq!(label(rate), expected, "rate={rate}");
        }
    }

    #[test]
    fn ordinary_percent_rounding_is_unchanged() {
        for (rate, expected) in [(0.0, "0%"), (0.5, "50%"), (0.904, "90%"), (0.95, "95%")] {
            assert_eq!(label(rate), expected);
        }
    }
}
