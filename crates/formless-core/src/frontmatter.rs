// This parser supports a bounded scalar subset, not arbitrary YAML. Only
// whitespace-separated hashes outside an actual quoted scalar start comments.
// Block content is consumed separately and must never pass through this helper.
pub(crate) fn scalar_without_comment(value: &str) -> &str {
    if let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let mut chars = value.char_indices().skip(1).peekable();
        while let Some((index, c)) = chars.next() {
            if quote == '"' && c == '\\' {
                chars.next();
            } else if c == quote {
                if quote == '\'' && chars.peek().is_some_and(|(_, next)| *next == '\'') {
                    chars.next();
                    continue;
                }
                let end = index + c.len_utf8();
                let after = &value[end..];
                return if after.starts_with([' ', '\t'])
                    && after.trim_start_matches([' ', '\t']).starts_with('#')
                {
                    &value[..end]
                } else {
                    value
                };
            }
        }
    } else {
        let mut separated = true;
        for (index, c) in value.char_indices() {
            if c == '#' && separated {
                return value[..index].trim_end();
            }
            separated = matches!(c, ' ' | '\t');
        }
    }
    value
}
