//! Streaming substring search: allocate for the query, never for each message.
use formless_core::fuzzy::fold_chars;

pub(super) struct Matcher {
    needle: Vec<char>,
    prefix: Vec<usize>,
}
impl Matcher {
    pub(super) fn new(query: &str) -> Self {
        let needle: Vec<_> = fold_chars(query).collect();
        let mut prefix = vec![0; needle.len()];
        for i in 1..needle.len() {
            let mut matched = prefix[i - 1];
            while matched > 0 && needle[matched] != needle[i] {
                matched = prefix[matched - 1];
            }
            if needle[matched] == needle[i] {
                matched += 1;
            }
            prefix[i] = matched;
        }
        Self { needle, prefix }
    }
    pub(super) fn contains(&self, text: &str) -> bool {
        if self.needle.is_empty() {
            return true;
        }
        let mut matched = 0;
        for c in fold_chars(text) {
            while matched > 0 && self.needle[matched] != c {
                matched = self.prefix[matched - 1];
            }
            if self.needle[matched] == c {
                matched += 1;
            }
            if matched == self.needle.len() {
                return true;
            }
        }
        false
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_search_handles_unicode_expansions_and_overlapping_prefixes() {
        for (query, text) in [
            ("BUILD", "Fix the build"),
            ("cafe", "open café.rs"),
            ("café", "open cafe\u{301}.rs"),
            ("cafe\u{301}", "open café.rs"),
            ("strasse", "Straße"),
            ("STRAẞE", "strasse"),
            ("i\u{307}", "İstanbul"),
            ("İ", "i\u{307}stanbul"),
            ("ος", "ΟΣ"),
            ("οσ", "ος"),
            ("ไทย", "ข้อความไทย"),
            ("前", "前文"),
            ("ababac", "ababababac"),
        ] {
            assert!(Matcher::new(query).contains(text), "{query:?} in {text:?}");
        }
        assert!(!Matcher::new("build").contains("deploy it"));
        assert!(!Matcher::new("aaaaab").contains(&"a".repeat(1024 * 1024)));
        assert!(Matcher::new("").contains(""));
    }
}
