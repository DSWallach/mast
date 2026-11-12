//! Read a side chat (and its main chat) as plain conversation text: the
//! payload "Absorb into main chat" sends, and the transcript a side chat's
//! first prompt is seeded with when its harness can't fork the main session.

use mast_proto::side_chat::{SideExchange, Speaker};

use crate::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};

/// Longest question or answer carried into an absorb payload.
pub const ABSORB_TEXT_MAX_BYTES: usize = 16_000;
/// Budget of a seeded main-chat transcript.
pub const SEED_MAX_BYTES: usize = 24_000;
/// Longest single message in a seeded transcript.
pub const SEED_MESSAGE_MAX_BYTES: usize = 4_000;

/// The exchanges not absorbed yet, and the marker to store once they land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsorbBatch {
    pub exchanges: Vec<SideExchange>,
    /// Id of the last message the batch covers.
    pub through: String,
}

/// One message with its continuation entries folded in.
struct Joined<'a> {
    id: &'a str,
    role: MessageRole,
    parts: Vec<&'a MessagePart>,
    status: Option<MessageStatus>,
}

fn join(entries: &[SessionMessageEntry]) -> Vec<Joined<'_>> {
    let mut joined: Vec<Joined<'_>> = Vec::with_capacity(entries.len());
    for entry in entries {
        if let Some(root) = &entry.continuation_of
            && let Some(parent) = joined.iter_mut().find(|m| m.id == root)
        {
            parent.parts.extend(entry.parts.iter());
            if entry.status.is_some() {
                parent.status = entry.status;
            }
            continue;
        }
        joined.push(Joined {
            id: &entry.id,
            role: entry.role,
            parts: entry.parts.iter().collect(),
            status: entry.status,
        });
    }
    joined
}

fn texts(parts: &[&MessagePart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } if !text.trim().is_empty() => Some(text.trim()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The turn's closing prose: text after its last tool call, else its last
/// text. Tool calls, output and reasoning never count.
fn final_answer(parts: &[&MessagePart]) -> String {
    let after_tools = parts
        .iter()
        .rposition(|part| matches!(part, MessagePart::Tool { .. }))
        .map_or(0, |at| at + 1);
    let tail = texts(&parts[after_tools..]);
    if !tail.is_empty() {
        return tail;
    }
    parts
        .iter()
        .rev()
        .find_map(|part| match part {
            MessagePart::Text { text, .. } if !text.trim().is_empty() => {
                Some(text.trim().to_owned())
            }
            _ => None,
        })
        .unwrap_or_default()
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

/// Completed exchanges after `absorbed_through`, or `None` when nothing new
/// has an answer. A turn still streaming (no later question, no final
/// status) ends the batch, so it is absorbed next time instead.
pub fn absorb_batch(
    entries: &[SessionMessageEntry],
    absorbed_through: Option<&str>,
) -> Option<AbsorbBatch> {
    let messages = join(entries);
    let start = absorbed_through
        .and_then(|marker| messages.iter().position(|m| m.id == marker))
        .map_or(0, |at| at + 1);
    let messages = &messages[start..];
    let mut exchanges = Vec::new();
    let mut through = None;
    let mut at = messages.iter().position(|m| m.role == MessageRole::User)?;
    while at < messages.len() {
        let question = &messages[at];
        let replies_end = messages[at + 1..]
            .iter()
            .position(|m| m.role == MessageRole::User)
            .map_or(messages.len(), |n| at + 1 + n);
        let replies = &messages[at + 1..replies_end];
        let answered = replies_end < messages.len()
            || replies.last().is_some_and(|m| {
                matches!(
                    m.status,
                    Some(MessageStatus::Complete | MessageStatus::Aborted)
                )
            });
        if !answered {
            break;
        }
        let parts: Vec<&MessagePart> = replies
            .iter()
            .flat_map(|m| m.parts.iter().copied())
            .collect();
        let answer = final_answer(&parts);
        // Only an answered exchange moves the marker: a turn with no answer
        // yet (an error later retried under the same question, or text that
        // arrives in a continuation) stays absorbable.
        if !answer.is_empty() {
            exchanges.push(SideExchange {
                question: clip(&texts(&question.parts), ABSORB_TEXT_MAX_BYTES),
                answer: clip(&answer, ABSORB_TEXT_MAX_BYTES),
            });
            through = Some(replies.last().unwrap_or(question).id.to_owned());
        }
        at = replies_end;
    }
    (!exchanges.is_empty()).then(|| AbsorbBatch {
        exchanges,
        through: through.expect("an exchange sets the marker"),
    })
}

/// The tail of a chat's user and assistant prose, oldest first, within
/// [`SEED_MAX_BYTES`].
pub fn transcript_tail(entries: &[SessionMessageEntry]) -> Vec<(Speaker, String)> {
    let mut lines = Vec::new();
    let mut used = 0;
    for message in join(entries).iter().rev() {
        let speaker = match message.role {
            MessageRole::User => Speaker::User,
            MessageRole::Assistant => Speaker::Assistant,
            _ => continue,
        };
        let text = texts(&message.parts);
        if text.is_empty() {
            continue;
        }
        let text = clip(&text, SEED_MESSAGE_MAX_BYTES);
        if used + text.len() > SEED_MAX_BYTES {
            break;
        }
        used += text.len();
        lines.push((speaker, text));
    }
    lines.reverse();
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::ToolCall;

    fn text(id: &str, body: &str) -> MessagePart {
        MessagePart::Text {
            id: id.into(),
            text: body.into(),
        }
    }

    fn tool(id: &str) -> MessagePart {
        MessagePart::Tool {
            id: id.into(),
            call: ToolCall::Exec {
                command: "ls".into(),
            },
            is_error: false,
            resolved: true,
            output: Some("a.rs".into()),
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            subagent_ref: None,
            subagent_status: None,
            subagent_tail: None,
        }
    }

    fn entry(
        id: &str,
        role: MessageRole,
        parts: Vec<MessagePart>,
        status: Option<MessageStatus>,
    ) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role,
            parts,
            created_at: 0,
            device_id: "d".into(),
            status,
            continuation_of: None,
        }
    }

    fn user(id: &str, body: &str) -> SessionMessageEntry {
        entry(id, MessageRole::User, vec![text("t", body)], None)
    }

    fn reply(id: &str, parts: Vec<MessagePart>, status: MessageStatus) -> SessionMessageEntry {
        entry(id, MessageRole::Assistant, parts, Some(status))
    }

    #[test]
    fn keeps_questions_and_final_answers_only() {
        let entries = vec![
            user("u1", "What does build.rs do?"),
            reply(
                "a1",
                vec![
                    text("t0", "Let me look."),
                    MessagePart::Reasoning {
                        id: "r".into(),
                        text: "thinking".into(),
                    },
                    tool("x"),
                    text("t1", "It generates bindings."),
                ],
                MessageStatus::Complete,
            ),
        ];
        let batch = absorb_batch(&entries, None).unwrap();
        assert_eq!(
            batch.exchanges,
            [SideExchange {
                question: "What does build.rs do?".into(),
                answer: "It generates bindings.".into()
            }]
        );
        assert_eq!(batch.through, "a1");
    }

    #[test]
    fn the_marker_skips_absorbed_exchanges_and_a_second_absorb_finds_nothing() {
        let mut entries = vec![
            user("u1", "First?"),
            reply("a1", vec![text("t", "One.")], MessageStatus::Complete),
        ];
        let first = absorb_batch(&entries, None).unwrap();
        assert_eq!(first.through, "a1");
        assert_eq!(absorb_batch(&entries, Some(&first.through)), None);
        entries.push(user("u2", "Second?"));
        entries.push(reply(
            "a2",
            vec![text("t", "Two.")],
            MessageStatus::Complete,
        ));
        let second = absorb_batch(&entries, Some(&first.through)).unwrap();
        assert_eq!(second.exchanges.len(), 1);
        assert_eq!(second.exchanges[0].question, "Second?");
        assert_eq!(second.through, "a2");
    }

    #[test]
    fn a_streaming_turn_waits_for_the_next_absorb() {
        let entries = vec![
            user("u1", "Done?"),
            reply("a1", vec![text("t", "Yes.")], MessageStatus::Complete),
            user("u2", "Still going?"),
            reply("a2", vec![text("t", "Work")], MessageStatus::Streaming),
        ];
        let batch = absorb_batch(&entries, None).unwrap();
        assert_eq!(batch.exchanges.len(), 1);
        assert_eq!(batch.through, "a1");
        let only_streaming = &entries[2..];
        assert_eq!(absorb_batch(only_streaming, None), None);
    }

    #[test]
    fn continuations_fold_and_answerless_turns_advance_silently() {
        let mut continued = reply("a1c", vec![text("t", "Part two.")], MessageStatus::Complete);
        continued.continuation_of = Some("a1".into());
        let entries = vec![
            user("u0", "Never answered"),
            user("u1", "Long one?"),
            reply(
                "a1",
                vec![tool("x"), text("t", "Part one.")],
                MessageStatus::Streaming,
            ),
            continued,
        ];
        let batch = absorb_batch(&entries, None).unwrap();
        assert_eq!(batch.exchanges.len(), 1);
        assert_eq!(batch.exchanges[0].answer, "Part one.\n\nPart two.");
        assert_eq!(batch.through, "a1");
    }

    #[test]
    fn an_answerless_turn_never_moves_the_marker_past_itself() {
        let failed = entry(
            "a2",
            MessageRole::Assistant,
            vec![MessagePart::Error { id: "e".into(), message: "overloaded".into() }],
            Some(MessageStatus::Complete),
        );
        let answered = || vec![user("u1", "First?"), reply("a1", vec![text("t", "One.")], MessageStatus::Complete)];

        // A failed turn, later retried under the same question.
        let mut retried = answered();
        retried.extend([user("u2", "Second?"), failed]);
        let first = absorb_batch(&retried, None).unwrap();
        assert_eq!(first.through, "a1");
        retried.push(reply("a2-retry", vec![text("t", "Two.")], MessageStatus::Complete));
        let later = absorb_batch(&retried, Some(&first.through)).unwrap();
        assert_eq!(later.exchanges[0].answer, "Two.");
        assert_eq!(later.through, "a2-retry");

        // A tool-only turn whose text arrives in a continuation.
        let mut continued = answered();
        continued.extend([user("u3", "Third?"), reply("a3", vec![tool("x")], MessageStatus::Complete)]);
        assert_eq!(absorb_batch(&continued, None).unwrap().through, "a1");
        let mut tail = reply("a3c", vec![text("t", "Three.")], MessageStatus::Complete);
        tail.continuation_of = Some("a3".into());
        continued.push(tail);
        let later = absorb_batch(&continued, Some("a1")).unwrap();
        assert_eq!(later.exchanges[0].answer, "Three.");
        assert_eq!(later.through, "a3");
    }

    #[test]
    fn long_answers_are_clipped_on_a_char_boundary() {
        let long = "é".repeat(ABSORB_TEXT_MAX_BYTES);
        let entries = vec![
            user("u", "q"),
            reply("a", vec![text("t", &long)], MessageStatus::Complete),
        ];
        let answer = &absorb_batch(&entries, None).unwrap().exchanges[0].answer;
        assert!(answer.len() <= ABSORB_TEXT_MAX_BYTES + '…'.len_utf8());
        assert!(answer.ends_with('…'));
    }

    #[test]
    fn transcript_tail_keeps_the_newest_prose_within_budget() {
        let big = "x".repeat(SEED_MESSAGE_MAX_BYTES * 2);
        let mut entries = Vec::new();
        for n in 0..10 {
            entries.push(user(&format!("u{n}"), &format!("question {n}")));
            entries.push(reply(
                &format!("a{n}"),
                vec![tool("x"), text("t", &big)],
                MessageStatus::Complete,
            ));
        }
        let tail = transcript_tail(&entries);
        assert_eq!(tail.last().unwrap().0, Speaker::Assistant);
        assert!(tail.iter().map(|(_, t)| t.len()).sum::<usize>() <= SEED_MAX_BYTES);
        assert!(
            tail.iter()
                .all(|(_, t)| t.len() <= SEED_MESSAGE_MAX_BYTES + '…'.len_utf8())
        );
        assert!(tail.len() < entries.len());
        assert_eq!(tail[tail.len() - 2], (Speaker::User, "question 9".into()));
    }
}
