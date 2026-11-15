//! Import reporting uses exact stored rates, rather than billing estimates.
use formless_core::config::ModelPrice;

pub(crate) fn change(model: &str, old: Option<&ModelPrice>, new: &ModelPrice) -> Option<String> {
    if old.is_some_and(|old| {
        old.input == new.input
            && old.output == new.output
            && old.cached_input == new.cached_input
            && old.cache_write_input == new.cache_write_input
    }) {
        return None;
    }
    let rate = |value: f64| format!("${value}");
    let optional = |value: Option<f64>, input: f64| {
        value
            .map(rate)
            .unwrap_or_else(|| format!("unset (uses input ${input})"))
    };
    let fields = |price: &ModelPrice| {
        [
            rate(price.input),
            rate(price.output),
            optional(price.cached_input, price.input),
            optional(price.cache_write_input, price.input),
        ]
    };
    let new_fields = fields(new);
    let old_fields = old.map(fields);
    let labels = ["input", "output", "cached input", "cache-write input"];
    let details = labels
        .iter()
        .enumerate()
        .map(|(index, label)| match &old_fields {
            Some(old) => format!("{label} {} -> {}", old[index], new_fields[index]),
            None => format!("{label} {}", new_fields[index]),
        })
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "{model}: {details} per 1M tokens{}",
        if old.is_some() { " (updated)" } else { "" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn price() -> ModelPrice {
        ModelPrice {
            input: 1.0,
            output: 2.0,
            cached_input: Some(0.25),
            cache_write_input: Some(0.5),
            retrieved_at: Some("old".into()),
        }
    }
    #[test]
    fn all_four_rates_participate_but_timestamp_alone_does_not() {
        let old = price();
        let mut new = old.clone();
        new.retrieved_at = Some("new".into());
        assert_eq!(change("id", Some(&old), &new), None);
        for index in 0..4 {
            let mut new = new.clone();
            match index {
                0 => new.input = 3.0,
                1 => new.output = 3.0,
                2 => new.cached_input = Some(3.0),
                _ => new.cache_write_input = Some(3.0),
            }
            assert!(change("id", Some(&old), &new).is_some());
        }
    }
    #[test]
    fn small_differences_keep_distinct_old_and_new_values() {
        let mut old = price();
        old.cache_write_input = Some(0.000000010001);
        let mut new = old.clone();
        new.cache_write_input = Some(0.000000010002);
        let text = change("model/ไทย", Some(&old), &new).unwrap();
        assert!(text.starts_with("model/ไทย:"));
        assert!(
            text.contains("cache-write input $0.000000010001 -> $0.000000010002"),
            "{text}"
        );
    }
    #[test]
    fn missing_rates_are_explicit_fallbacks_in_new_and_updated_records() {
        let old = price();
        let mut new = old.clone();
        new.cache_write_input = None;
        new.cached_input = None;
        let text = change("id", Some(&old), &new).unwrap();
        assert!(text.contains("cached input $0.25 -> unset (uses input $1)"));
        assert!(text.contains("cache-write input $0.5 -> unset (uses input $1)"));
        let text = change("id", None, &new).unwrap();
        assert!(text.contains("input $1; output $2; cached input unset (uses input $1); cache-write input unset (uses input $1)"));
        assert!(!text.contains("updated") && !text.contains("$0;"));
    }
}
