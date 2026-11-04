/// Remove recognizable credentials at the boundary where diagnostics become UI text.
pub fn redact_secrets(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let markers = [
        "bearer ",
        "basic ",
        "sk-",
        "ghp_",
        "gho_",
        "github_pat_",
        "lin_api_",
        "akia",
        "asia",
        "xox",
        "api_key=",
        "password=",
        "token=",
        "secret=",
        "\"password\":",
        "\"token\":",
        "\"secret\":",
        "\"api_key\":",
    ];
    let mut result = String::new();
    let mut offset = 0;
    while let Some((start, marker)) = markers
        .iter()
        .filter_map(|marker| {
            lower[offset..]
                .find(marker)
                .map(|at| (offset + at, *marker))
        })
        .min_by_key(|(at, _)| *at)
    {
        let credential = if marker.ends_with(' ') || marker.ends_with('=') || marker.ends_with(':')
        {
            start + marker.len()
        } else {
            start
        };
        let rest = &text[credential..];
        let whitespace = rest.len() - rest.trim_start_matches(char::is_whitespace).len();
        let quote_at = credential + whitespace;
        let quote = text[quote_at..]
            .chars()
            .next()
            .filter(|c| matches!(c, '\"' | '\''));
        let credential = quote_at + quote.map_or(0, char::len_utf8);
        let end = if let Some(quote) = quote {
            let mut escaped = false;
            text[credential..]
                .char_indices()
                .find_map(|(index, c)| {
                    if escaped {
                        escaped = false;
                        return None;
                    }
                    if c == '\\' {
                        escaped = true;
                        return None;
                    }
                    (c == quote).then_some(credential + index)
                })
                .unwrap_or(text.len())
        } else {
            text[credential..]
                .find(|c: char| {
                    c.is_whitespace() || matches!(c, '\"' | '\'' | ',' | ';' | '&' | '<' | '>')
                })
                .map_or(text.len(), |at| credential + at)
        };
        result.push_str(&text[offset..credential]);
        result.push_str("[REDACTED]");
        // Empty credentials still advance past the marker.
        offset = end.max(start + marker.len());
    }
    result.push_str(&text[offset..]);
    result
}

#[cfg(test)]
mod tests {
    use super::redact_secrets;

    #[test]
    fn redacts_credential_patterns() {
        for value in [
            "ghp_fake",
            "gho_fake",
            "github_pat_fake",
            "sk-fake",
            "sk-ant-fake",
            "lin_api_fake",
            "AKIAFAKE123456789012",
            "ASIAFAKE123456789012",
            "xoxb-fake",
            "Bearer fake",
            "password=fake",
            "token=fake",
            "secret=fake",
        ] {
            let result = redact_secrets(value);
            assert!(result.contains("[REDACTED]"), "{value}: {result}");
            assert!(!result.to_lowercase().contains("fake"));
        }
        assert_eq!(
            redact_secrets(r#"password="fake secret""#),
            r#"password="[REDACTED]""#
        );
        assert_eq!(
            redact_secrets(r#"{"token":"fake secret"}"#),
            r#"{"token":"[REDACTED]"}"#
        );
    }
}
