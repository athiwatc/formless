//! Streaming substring search: allocate for the query, never for each message.
fn folded(c: char) -> char {
    // Both forms of lowercase sigma refer to the same uppercase letter.
    if c == '\u{03c2}' {
        '\u{03c3}'
    } else {
        c
    }
}
pub(super) struct Matcher {
    needle: Vec<char>,
    prefix: Vec<usize>,
}
impl Matcher {
    pub(super) fn new(query: &str) -> Self {
        let needle: Vec<_> = query
            .chars()
            .flat_map(char::to_lowercase)
            .map(folded)
            .collect();
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
        for c in text.chars().flat_map(char::to_lowercase).map(folded) {
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
