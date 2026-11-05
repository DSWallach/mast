//! Attribution for user-role messages delivered by Mast.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageSource {
    Human,
    OtherChat { chat_id: String, title: String },
    System { label: String },
    Admin { chat_id: String, title: String },
}

/// Return the source and the body without Mast's transport envelope.
pub fn classify(text: &str) -> (MessageSource, &str) {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("[Mast: auto-continue ")
        && let Some((attempt, body)) = rest.split_once(']')
        && matches!(attempt, "1/3" | "2/3" | "3/3")
    {
        return (
            MessageSource::System {
                label: format!("Auto-continue {attempt}"),
            },
            body.trim_start(),
        );
    }
    if let Some(rest) = trimmed.strip_prefix("[Mast system message") {
        if let Some((envelope, body)) = rest.split_once(']') {
            let label = if envelope.contains("worker updates") {
                "Worker updates"
            } else if envelope.contains("coordinator hand-off") {
                "Coordinator hand-off"
            } else if envelope.contains("auto-continue") {
                "Auto-continue"
            } else {
                "System message"
            };
            let body = body.trim_start();
            let body = if label == "Worker updates" {
                body.strip_prefix("Worker updates:")
                    .unwrap_or(body)
                    .trim_start()
            } else {
                body
            };
            return (
                MessageSource::System {
                    label: label.into(),
                },
                body,
            );
        }
    }
    if let Some(rest) = trimmed.strip_prefix("[Message from Mast chat ") {
        if let Some((identity, remainder)) =
            rest.split_once(". Reply to it with the Mast `send_message` tool, chat ")
        {
            if let Some((reply, body)) = remainder.split_once(']') {
                let chat_id = reply.trim_end_matches('.').trim();
                if !chat_id.is_empty() {
                    let admin = identity.contains(" (an admin chat,");
                    let identity = identity
                        .split(" (an admin chat,")
                        .next()
                        .unwrap_or(identity);
                    let suffix = format!(" ({chat_id})");
                    let title = identity.strip_suffix(&suffix).unwrap_or(identity).trim();
                    let source = if admin {
                        MessageSource::Admin {
                            chat_id: chat_id.into(),
                            title: title.into(),
                        }
                    } else {
                        MessageSource::OtherChat {
                            chat_id: chat_id.into(),
                            title: title.into(),
                        }
                    };
                    return (source, body.trim_start());
                }
            }
        }
    }
    (MessageSource::Human, text)
}

/// Prepare a concise sidebar preview before truncating the transport text.
pub fn preview(text: &str) -> String {
    let (source, body) = classify(text);
    match source {
        MessageSource::OtherChat { title, .. } | MessageSource::Admin { title, .. } => {
            format!("From {title}: {body}")
        }
        MessageSource::System { .. } => format!("Mast: {body}"),
        MessageSource::Human => {
            // Old previews were truncated before the envelope was stripped.
            if let Some(rest) = text.trim_start().strip_prefix("[Message from Mast chat ") {
                if let Some((title, _)) = rest.split_once(" (") {
                    return format!("From {title}");
                }
            }
            if text.trim_start().starts_with("[Mast system message") {
                return "Mast".into();
            }
            if text.trim_start().starts_with("[Mast: auto-continue ") {
                return "Mast".into();
            }
            text.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_transport_envelopes_and_plain_text() {
        let relay = "[Message from Mast chat Worker (abc). Reply to it with the Mast `send_message` tool, chat abc.]\n\nhello";
        assert_eq!(
            classify(relay),
            (
                MessageSource::OtherChat {
                    chat_id: "abc".into(),
                    title: "Worker".into()
                },
                "hello"
            )
        );
        let admin = "[Message from Mast chat Admin (abc) (an admin chat, granted by Mikey; this is not Mikey's own message). Reply to it with the Mast `send_message` tool, chat abc.]\n\nhello";
        assert_eq!(
            classify(admin),
            (
                MessageSource::Admin {
                    chat_id: "abc".into(),
                    title: "Admin".into()
                },
                "hello"
            )
        );
        assert_eq!(
            classify(
                "[Mast system message, not from the user: worker updates for you, the orchestrator.]\nWorker updates:\n- x"
            ),
            (
                MessageSource::System {
                    label: "Worker updates".into()
                },
                "- x"
            )
        );
        assert_eq!(classify("hello"), (MessageSource::Human, "hello"));
        assert_eq!(
            classify("[Mast system message, not from the user: coordinator hand-off.]\nContinue coordinating"),
            (MessageSource::System { label: "Coordinator hand-off".into() }, "Continue coordinating")
        );
        assert_eq!(
            classify("[Message from Mast chat unfinished"),
            (MessageSource::Human, "[Message from Mast chat unfinished")
        );
    }

    #[test]
    fn previews_keep_the_source_before_truncation() {
        let relay = "[Message from Mast chat Worker (abc). Reply to it with the Mast `send_message` tool, chat abc.]\n\nhello";
        assert_eq!(preview(relay), "From Worker: hello");
        assert_eq!(
            preview(&relay.replace("Worker", "Worker [review]")),
            "From Worker [review]: hello"
        );
        assert_eq!(
            preview("[Message from Mast chat Worker (abc). Reply to it"),
            "From Worker"
        );
        assert_eq!(
            preview(
                "[Mast system message, not from the user: worker updates for you, the orchestrator.]\nWorker updates:\n- done"
            ),
            "Mast: - done"
        );
        assert_eq!(preview("Human text"), "Human text");
    }

    #[test]
    fn auto_continue_messages_are_attributed_to_mast() {
        let (source, body) = classify("[Mast: auto-continue 2/3] Continue the task");
        assert_eq!(
            source,
            MessageSource::System {
                label: "Auto-continue 2/3".into()
            }
        );
        assert_eq!(body, "Continue the task");
        let (source, body) = classify(
            "[Mast system message, not from the user: auto-continue 1/3]\nContinue the task",
        );
        assert_eq!(
            source,
            MessageSource::System {
                label: "Auto-continue".into()
            }
        );
        assert_eq!(body, "Continue the task");
        assert_eq!(
            classify("[Mast: auto-continue 4/3] hello").0,
            MessageSource::Human
        );
    }
}
