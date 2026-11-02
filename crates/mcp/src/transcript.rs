//! Flatten session-doc entries into agent-readable messages.
//!
//! The doc keeps every assistant message as a list of parts (text, thinking,
//! tool chips, input requests, errors) and splits long turns into
//! continuation entries. An agent reading another chat wants prose plus a
//! one-line record of what each tool did — never the raw part map.

use mast_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use mast_proto::ToolCall;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    /// Include model thinking (`reasoning` parts). Off by default: it is
    /// long and rarely what a coordinating agent needs.
    pub include_reasoning: bool,
    /// Include the one-line tool ledger.
    pub include_tools: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            include_reasoning: false,
            include_tools: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RenderedMessage {
    pub id: String,
    pub role: MessageRole,
    /// Epoch millis, plus the same instant as RFC 3339 for readability.
    pub created_at: i64,
    pub created_at_iso: String,
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<MessageStatus>,
    /// Prose parts joined with blank lines.
    pub text: String,
    /// Bytes cut from the start of `text` to fit a size cap (the tail is kept).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_omitted_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// One line per tool call, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// An unanswered question the agent is blocked on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_input: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Render entries in order, folding continuation entries into the message
/// they continue.
pub fn render_entries(
    entries: &[SessionMessageEntry],
    options: RenderOptions,
) -> Vec<RenderedMessage> {
    let mut rendered: Vec<RenderedMessage> = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut message = render_one(entry, options);
        if let Some(parent_id) = &entry.continuation_of
            && let Some(parent) = rendered.iter_mut().find(|m| &m.id == parent_id)
        {
            fold_into(parent, message);
            continue;
        }
        // A continuation whose parent is not in the window stands alone;
        // keep its own id so a later `continuation_of` can still find it.
        message.id = entry.id.clone();
        rendered.push(message);
    }
    rendered
}

fn fold_into(parent: &mut RenderedMessage, child: RenderedMessage) {
    if !child.text.is_empty() {
        if !parent.text.is_empty() {
            parent.text.push_str("\n\n");
        }
        parent.text.push_str(&child.text);
    }
    match (&mut parent.reasoning, child.reasoning) {
        (Some(existing), Some(more)) => {
            existing.push_str("\n\n");
            existing.push_str(&more);
        }
        (slot @ None, Some(more)) => *slot = Some(more),
        _ => {}
    }
    parent.tools.extend(child.tools);
    parent.errors.extend(child.errors);
    if child.pending_input.is_some() {
        parent.pending_input = child.pending_input;
    }
    // The continuation carries the live status of the turn.
    if child.status.is_some() {
        parent.status = child.status;
    }
}

fn render_one(entry: &SessionMessageEntry, options: RenderOptions) -> RenderedMessage {
    let mut text_parts: Vec<&str> = Vec::new();
    let mut reasoning_parts: Vec<&str> = Vec::new();
    let mut tools = Vec::new();
    let mut pending_input = None;
    let mut errors = Vec::new();
    for part in &entry.parts {
        match part {
            MessagePart::Text { text, .. } => {
                if !text.trim().is_empty() {
                    text_parts.push(text.as_str());
                }
            }
            MessagePart::Image { name, .. } => tools.push(format!("image: {name}")),
            MessagePart::Reasoning { text, .. } => {
                if options.include_reasoning && !text.trim().is_empty() {
                    reasoning_parts.push(text.as_str());
                }
            }
            MessagePart::Tool {
                call,
                is_error,
                resolved,
                output,
                ..
            } => {
                if options.include_tools {
                    tools.push(tool_line(call, *is_error, *resolved, output.as_deref()));
                }
            }
            MessagePart::Input {
                request_id,
                questions,
                resolved,
                ..
            } => {
                if !*resolved {
                    pending_input = Some(json!({
                        "requestId": request_id,
                        "questions": questions,
                    }));
                }
            }
            MessagePart::Error { message, .. } => errors.push(message.clone()),
        }
    }
    RenderedMessage {
        id: entry.id.clone(),
        role: entry.role,
        created_at: entry.created_at,
        created_at_iso: iso(entry.created_at),
        device_id: entry.device_id.clone(),
        status: entry.status,
        text: text_parts.join("\n\n"),
        text_omitted_bytes: None,
        reasoning: (!reasoning_parts.is_empty()).then(|| reasoning_parts.join("\n\n")),
        tools,
        pending_input,
        errors,
    }
}

/// `kind: subject → first output line`, capped so a transcript window stays
/// small even when a tool printed a build log.
fn tool_line(call: &ToolCall, is_error: bool, resolved: bool, output: Option<&str>) -> String {
    let mut line = match call {
        ToolCall::Exec { command } => format!("exec: {}", clip(command, 160)),
        ToolCall::ReadFile { path } => format!("read: {path}"),
        ToolCall::WriteFile { path, .. } => format!("write: {path}"),
        ToolCall::EditFile { path, .. } => format!("edit: {path}"),
        ToolCall::ApplyPatch { path } => match path {
            Some(path) => format!("patch: {path}"),
            None => "patch".into(),
        },
        ToolCall::Search { pattern, path } => match path {
            Some(path) => format!("search: {pattern} in {path}"),
            None => format!("search: {pattern}"),
        },
        ToolCall::Glob { pattern } => format!("glob: {pattern}"),
        ToolCall::WebFetch { url, .. } => format!("fetch: {url}"),
        ToolCall::WebSearch { query } => format!("web search: {query}"),
        ToolCall::Todo { items } => {
            let done = items.iter().filter(|i| i.done).count();
            format!("todo: {done}/{} done", items.len())
        }
        ToolCall::Mcp { server, tool, .. } => format!("mcp: {server}/{tool}"),
        ToolCall::Unknown { name, .. } => name.clone(),
    };
    if let Some(output) = output.map(str::trim).filter(|o| !o.is_empty()) {
        let first = output.lines().next().unwrap_or_default();
        line.push_str(" → ");
        line.push_str(&clip(first, 200));
    }
    if is_error {
        line.push_str(" (error)");
    } else if !resolved {
        line.push_str(" (running)");
    }
    line
}

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

fn iso(millis: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, role: MessageRole, parts: Vec<MessagePart>) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role,
            parts,
            created_at: 1_700_000_000_000,
            device_id: "dev-a".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        }
    }

    #[test]
    fn folds_text_tools_and_pending_input() {
        let entries = vec![
            entry(
                "u1",
                MessageRole::User,
                vec![MessagePart::Text {
                    id: "t".into(),
                    text: "hello".into(),
                }],
            ),
            entry(
                "a1",
                MessageRole::Assistant,
                vec![
                    MessagePart::Reasoning {
                        id: "r".into(),
                        text: "thinking".into(),
                    },
                    MessagePart::Tool {
                        id: "x".into(),
                        call: ToolCall::Exec {
                            command: "cargo test".into(),
                        },
                        is_error: false,
                        resolved: true,
                        output: Some("ok\nmore".into()),
                        diff: None,
                        output_ref: None,
                        output_bytes: None,
                        diff_ref: None,
                        diff_stats: None,
                        subagent_ref: None,
                        subagent_status: None,
                        subagent_tail: None,
                    },
                    MessagePart::Text {
                        id: "t2".into(),
                        text: "done".into(),
                    },
                    MessagePart::Input {
                        id: "i".into(),
                        request_id: "req-1".into(),
                        questions: vec![],
                        resolved: false,
                    },
                ],
            ),
        ];
        let rendered = render_entries(&entries, RenderOptions::default());
        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[0].text, "hello");
        assert_eq!(rendered[1].text, "done");
        assert_eq!(rendered[1].tools, vec!["exec: cargo test → ok"]);
        assert!(rendered[1].reasoning.is_none(), "reasoning is opt-in");
        assert_eq!(
            rendered[1]
                .pending_input
                .as_ref()
                .and_then(|v| v["requestId"].as_str()),
            Some("req-1")
        );
        assert_eq!(rendered[1].created_at_iso, "2023-11-14T22:13:20Z");
    }

    #[test]
    fn continuations_fold_into_their_parent() {
        let mut tail = entry(
            "a1-cont",
            MessageRole::Assistant,
            vec![MessagePart::Text {
                id: "t".into(),
                text: "second half".into(),
            }],
        );
        tail.continuation_of = Some("a1".into());
        tail.status = Some(MessageStatus::Streaming);
        let entries = vec![
            entry(
                "a1",
                MessageRole::Assistant,
                vec![MessagePart::Text {
                    id: "t".into(),
                    text: "first half".into(),
                }],
            ),
            tail,
        ];
        let rendered = render_entries(&entries, RenderOptions::default());
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].text, "first half\n\nsecond half");
        assert_eq!(rendered[0].status, Some(MessageStatus::Streaming));
    }
}

/// Keep the last `max_bytes` of `text` on a char boundary; returns the bytes cut.
pub fn tail_bytes(text: &mut String, max_bytes: usize) -> usize {
    if text.len() <= max_bytes {
        return 0;
    }
    let mut cut = text.len() - max_bytes;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text.drain(..cut);
    cut
}

/// Cap each message's text to `per_message` bytes (keeping the tail), then drop
/// the oldest messages until the combined text fits `total`; the newest message
/// is always kept (shrunk if it alone is over). Returns `(kept, dropped_older)`.
pub fn cap_messages(
    mut messages: Vec<RenderedMessage>,
    per_message: usize,
    total: usize,
) -> (Vec<RenderedMessage>, usize) {
    for m in &mut messages {
        let cut = tail_bytes(&mut m.text, per_message);
        if cut > 0 {
            m.text_omitted_bytes = Some(m.text_omitted_bytes.unwrap_or(0) + cut);
        }
    }
    let size = |m: &RenderedMessage| {
        m.text.len() + m.tools.iter().map(String::len).sum::<usize>()
            + m.reasoning.as_ref().map_or(0, String::len)
    };
    let mut used = 0usize;
    let mut keep_from = messages.len();
    for (i, m) in messages.iter().enumerate().rev() {
        let n = size(m);
        if keep_from < messages.len() && used + n > total {
            break;
        }
        used += n;
        keep_from = i;
    }
    let dropped = keep_from;
    messages.drain(..keep_from);
    if let Some(last) = messages.last_mut()
        && size(last) > total
    {
        let cut = tail_bytes(&mut last.text, total.saturating_sub(last.tools.iter().map(String::len).sum::<usize>()));
        if cut > 0 {
            last.text_omitted_bytes = Some(last.text_omitted_bytes.unwrap_or(0) + cut);
        }
    }
    (messages, dropped)
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    fn msg(id: &str, text: &str) -> RenderedMessage {
        RenderedMessage {
            id: id.into(),
            role: MessageRole::Assistant,
            created_at: 0,
            created_at_iso: String::new(),
            device_id: String::new(),
            status: None,
            text: text.into(),
            text_omitted_bytes: None,
            reasoning: None,
            tools: vec![],
            pending_input: None,
            errors: vec![],
        }
    }

    #[test]
    fn cap_messages_drops_oldest_first_and_keeps_the_newest() {
        let msgs = vec![msg("a", &"a".repeat(60)), msg("b", &"b".repeat(60)), msg("c", &"c".repeat(60))];
        let (kept, dropped) = cap_messages(msgs, 50, 80);
        assert_eq!(dropped, 2);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "c");
        assert_eq!(kept[0].text.len(), 50);
        assert_eq!(kept[0].text_omitted_bytes, Some(10));
    }

    #[test]
    fn tail_bytes_respects_char_boundaries() {
        let mut s = "ééé".to_string(); // 6 bytes
        assert_eq!(tail_bytes(&mut s, 3), 4);
        assert_eq!(s, "é");
    }
}
