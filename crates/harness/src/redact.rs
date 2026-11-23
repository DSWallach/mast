/// Remove recognizable credentials at the boundary where diagnostics become UI text.
pub fn redact_secrets(text: &str) -> String {
    let text = redact_url_credentials(text);
    let text = text.as_str();
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
        // Environment-style names (`AWS_SECRET_ACCESS_KEY=`) and the
        // spellings of a key header or flag that carry a value after a space.
        "_key=",
        "apikey=",
        "api-key=",
        "api-key:",
        "api_key:",
        "auth-token:",
        "access-token:",
        "authorization: token ",
        "--password ",
        "--passwd ",
        "--token ",
        "--secret ",
        "--api-key ",
        "--apikey ",
        "--access-token ",
        "--auth-token ",
        "--client-secret ",
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

/// Replace the password of `scheme://user:password@host` with a placeholder.
/// A user name alone (`ssh://git@host`) is not a credential.
fn redact_url_credentials(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let authority_start = at + 3;
        result.push_str(&rest[..authority_start]);
        rest = &rest[authority_start..];
        let authority_end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\'' | '`'))
            .unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        match authority.rfind('@').and_then(|at| authority[..at].find(':').map(|colon| (colon, at))) {
            Some((colon, at)) => {
                result.push_str(&authority[..=colon]);
                result.push_str("[REDACTED]");
                result.push_str(&authority[at..]);
            }
            None => result.push_str(authority),
        }
        rest = &rest[authority_end..];
    }
    result.push_str(rest);
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

    #[test]
    fn redacts_command_line_credentials() {
        for (input, hidden) in [
            ("curl --password hunter2 https://example.com", "hunter2"),
            ("tool --token abc123 --verbose", "abc123"),
            ("tool --api-key abc123", "abc123"),
            ("AWS_SECRET_ACCESS_KEY=wJalrFake/bPxRfi aws s3 ls", "wJalrFake"),
            ("STRIPE_API_KEY=live123 run", "live123"),
            ("git clone https://user:hunter2@example.com/repo.git", "hunter2"),
            ("curl -H 'X-Api-Key: abc123' https://example.com", "abc123"),
            ("curl -H 'x-auth-token: abc123' https://example.com", "abc123"),
            ("curl -H 'Authorization: token abc123' https://example.com", "abc123"),
        ] {
            let result = redact_secrets(input);
            assert!(result.contains("[REDACTED]"), "{input}: {result}");
            assert!(!result.contains(hidden), "{input}: {result}");
        }
    }

    #[test]
    fn leaves_ordinary_commands_alone() {
        for input in [
            "cargo nextest run -p mast-engine --test engine",
            "git clone ssh://git@example.com/repo.git",
            "git clone https://example.com/repo.git",
            "tool --password-file /tmp/pw --token-file /tmp/t",
            "rg -n 'api_key' src",
            "scripts/run.sh --sort-order asc",
        ] {
            assert_eq!(redact_secrets(input), input);
        }
    }
}
