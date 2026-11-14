//! Ranking for module/Skill summaries only. Tool tokenization/stemming remains
//! independent; this is not script-aware segmentation for spaceless languages.
use std::collections::BTreeSet;

fn tokens(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

pub(crate) struct Query {
    terms: Vec<String>,
}

impl Query {
    pub(crate) fn new(text: &str) -> Self {
        Self {
            terms: tokens(text)
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }

    fn score(&self, name: &str, description: &str) -> Option<(usize, usize, usize, usize)> {
        if self.terms.is_empty() {
            return None;
        }
        let name_tokens = tokens(name);
        let description_tokens = tokens(description);
        let name = name.to_lowercase();
        let description = description.to_lowercase();
        let (mut coverage, mut exact, mut named, mut prefix) = (0, 0, 0, 0);
        for term in &self.terms {
            let exact_name = name_tokens.contains(term);
            let exact_description = description_tokens.contains(term);
            // Short terms can match exact tokens, but cannot open up arbitrary
            // substring/prefix matches merely because UTF-8 uses several bytes.
            let fallback = term.chars().count() >= 3;
            let name_match = exact_name || fallback && name.contains(term);
            let description_match = exact_description || fallback && description.contains(term);
            if name_match || description_match {
                coverage += 1;
                exact += usize::from(exact_name || exact_description);
                named += usize::from(name_match);
                prefix += usize::from(
                    fallback
                        && name_tokens
                            .iter()
                            .chain(&description_tokens)
                            .any(|token| token.starts_with(term)),
                );
            }
        }
        (coverage > 0).then_some((coverage, exact, named, prefix))
    }

    pub(crate) fn rank<T, F>(
        &self,
        items: impl IntoIterator<Item = T>,
        limit: usize,
        fields: F,
    ) -> Vec<T>
    where
        F: for<'a> Fn(&'a T) -> (&'a str, &'a str),
    {
        let mut hits: Vec<_> = items
            .into_iter()
            .filter_map(|item| {
                let (name, description) = fields(&item);
                self.score(name, description).map(|score| (score, item))
            })
            .collect();
        // Stable: equal scores keep the storage list's existing input order.
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.0));
        hits.into_iter().take(limit).map(|(_, item)| item).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranked<'a>(query: &str, rows: &[(&'a str, &'a str)], limit: usize) -> Vec<&'a str> {
        Query::new(query)
            .rank(rows.iter().copied(), limit, |row| (row.0, row.1))
            .into_iter()
            .map(|row| row.0)
            .collect()
    }

    #[test]
    fn distinct_term_coverage_precedes_exact_name_prefix_and_substring_ties() {
        let rows = [
            ("alpha", "database"),
            ("z-complete", "database backup"),
            ("backup", "database"),
            ("database-backup", ""),
            ("prefix", "database backups"),
            ("substring", "predatabase postbackup"),
        ];
        let expected = [
            "database-backup",
            "backup",
            "z-complete",
            "prefix",
            "substring",
            "alpha",
        ];
        assert_eq!(ranked("database backup", &rows, 10), expected);
        assert_eq!(
            ranked("DATABASE backup backup database", &rows, 10),
            expected
        );
        assert_eq!(ranked("database backup", &rows, 1), ["database-backup"]);
        assert!(ranked("database", &rows, 0).is_empty());
    }

    #[test]
    fn short_terms_require_exact_tokens_and_unicode_gates_count_scalars() {
        let rows = [
            ("db-helper", ""),
            ("adbc", ""),
            ("s3", "storage"),
            ("猫", ""),
            ("猫咪", ""),
            ("éé", ""),
            ("xééx", ""),
            ("ไทยงาน", ""),
        ];
        assert_eq!(ranked("DB", &rows, 10), ["db-helper"]);
        assert_eq!(ranked("s3", &rows, 10), ["s3"]);
        assert_eq!(ranked("猫", &rows, 10), ["猫"]);
        assert_eq!(ranked("éé", &rows, 10), ["éé"]);
        assert_eq!(ranked("ไทย", &rows, 10), ["ไทยงาน"]);
        assert!(ranked(" --- ... ", &rows, 10).is_empty());
    }

    #[test]
    fn equal_scores_keep_input_order_and_original_text() {
        let rows = [
            ("Z_ไทย", "雪 backup"),
            ("a_é", "雪 backup"),
            ("M", "雪 backup"),
        ];
        assert_eq!(ranked("backup", &rows, 2), ["Z_ไทย", "a_é"]);
        let reversed = rows.into_iter().rev().collect::<Vec<_>>();
        assert_eq!(ranked("backup", &reversed, 3), ["M", "a_é", "Z_ไทย"]);
    }
}
