use crate::table::Tone;
use formless_core::success_rate;

/// SUCCESS cell for the Tools list: label and tone for a success rate.
/// Tone uses the unrounded `r < 0.8` predicate; below that threshold a
/// label that would round to `80%` is shown as `<80%` instead.
pub(crate) fn tool_success_cell(rate: Option<f64>) -> (String, Tone) {
    match rate {
        Some(r) if r < 0.8 => {
            let label = success_rate::label(r);
            let label = if label == "80%" { "<80%".into() } else { label };
            (label, Tone::Bad)
        }
        Some(r) => (success_rate::label(r), Tone::Good),
        None => ("-".into(), Tone::Dim),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imperfect_high_rates_keep_good_tone_without_claiming_perfection() {
        for (rate, expected) in [
            (0.9949, "99%"),
            (0.995, ">99%"),
            (0.9999, ">99%"),
            (1.0, "100%"),
        ] {
            assert_eq!(tool_success_cell(Some(rate)), (expected.into(), Tone::Good));
        }
    }

    #[test]
    fn below_threshold_rounding_to_80_shows_bounded_label() {
        assert_eq!(tool_success_cell(Some(0.799)), ("<80%".into(), Tone::Bad));
        let just_below = f64::from_bits(0.8f64.to_bits() - 1);
        assert!(just_below < 0.8);
        assert_eq!(
            tool_success_cell(Some(just_below)),
            ("<80%".into(), Tone::Bad)
        );
    }

    #[test]
    fn exact_threshold_and_none_unchanged() {
        assert_eq!(tool_success_cell(Some(0.8)), ("80%".into(), Tone::Good));
        assert_eq!(tool_success_cell(None), ("-".into(), Tone::Dim));
    }

    #[test]
    fn ordinary_rates_format_as_before() {
        assert_eq!(tool_success_cell(Some(0.0)), ("0%".into(), Tone::Bad));
        assert_eq!(tool_success_cell(Some(0.5)), ("50%".into(), Tone::Bad));
        assert_eq!(tool_success_cell(Some(0.79)), ("79%".into(), Tone::Bad));
        assert_eq!(tool_success_cell(Some(0.95)), ("95%".into(), Tone::Good));
        assert_eq!(tool_success_cell(Some(1.0)), ("100%".into(), Tone::Good));
    }
}
