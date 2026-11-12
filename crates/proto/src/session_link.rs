//! Workspace-relative session references for agent replies. These resolve only
//! inside the current Mast workspace, never through an external URL handler.

/// A Markdown destination; the caller supplies the human-readable link label.
pub fn session_link(chat_id: &str) -> String {
    let mut url = String::from("mast://chat/");
    for byte in chat_id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            url.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url
}

pub fn parse_session_link(url: &str) -> Option<String> {
    let encoded = url.strip_prefix("mast://chat/")?;
    let mut bytes = Vec::new();
    let mut rest = encoded.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            bytes.push(byte);
            rest = tail;
        } else {
            return None;
        }
    }
    let id = String::from_utf8(bytes).ok()?;
    (!id.is_empty() && !id.chars().any(char::is_control)).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_references_round_trip_and_reject_invalid_destinations() {
        for id in ["chat-123", "id/with space?#", "会話"] {
            assert_eq!(parse_session_link(&session_link(id)).as_deref(), Some(id));
        }
        for url in [
            "mast://chat/",
            "mast://chat/%",
            "mast://chat/%GG",
            "mast://chat/%FF",
            "mast://chat/%00",
            "mast://chat/id?workspace=x",
            "https://chat/id",
            "mast://chat/id/other",
        ] {
            assert_eq!(parse_session_link(url), None, "{url}");
        }
    }
}
