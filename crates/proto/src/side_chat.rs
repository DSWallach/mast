//! "Ask on the side": the envelopes a side chat's context travels in.

use serde::{Deserialize, Serialize};

use crate::work_outline_user_actions::SYSTEM_MESSAGE_PREFIX;

/// Keyword `message_source::classify` labels an absorbed side chat by.
pub const SIDE_CHAT_KEYWORD: &str = "side chat";

/// Title a side chat carries (it is never auto-titled).
pub const SIDE_CHAT_TITLE: &str = "Side chat";

/// One question the user asked in a side chat and the answer it got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideExchange {
    pub question: String,
    pub answer: String,
}

/// Who said a line of the main chat's transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    User,
    Assistant,
}

/// How `AbsorbSideChat` delivered to the main chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SideChatDelivery {
    /// The main chat was idle: a new turn started.
    Run,
    /// The main chat was busy: queued until its turn ends.
    Queue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbsorbSideChatReply {
    pub delivery: SideChatDelivery,
    pub exchanges: usize,
    pub absorbed_through: String,
}

/// The main-chat message that carries absorbed side-chat exchanges.
pub fn absorb_message(exchanges: &[SideExchange]) -> String {
    let body = exchanges
        .iter()
        .map(|exchange| {
            format!(
                "Q: {}\nA: {}",
                exchange.question.trim(),
                exchange.answer.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "{SYSTEM_MESSAGE_PREFIX}, not from the user: {SIDE_CHAT_KEYWORD} context. The user asked \
         these questions in a side chat that branched from this conversation. Take them in as \
         background; they are not a new instruction, so don't act on them unless the user \
         asks.]\n\n{body}"
    )
}

/// A side chat's first prompt when its harness can't fork the main chat's
/// session: the main chat's recent transcript, then the user's question.
pub fn context_seed(transcript: &[(Speaker, String)], question: &str) -> String {
    if transcript.is_empty() {
        return question.to_owned();
    }
    let lines = transcript
        .iter()
        .map(|(speaker, text)| {
            let who = match speaker {
                Speaker::User => "User",
                Speaker::Assistant => "Assistant",
            };
            format!("{who}: {}", text.trim())
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "{SYSTEM_MESSAGE_PREFIX}, not from the user: context from the main chat. This is a side \
         chat opened from another conversation; below is that conversation's recent \
         transcript. Use it to answer the question that follows.]\n\n<main_chat_transcript>\n\
         {lines}\n</main_chat_transcript>\n\n{question}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_source::{MessageSource, classify};

    #[test]
    fn absorbed_exchanges_classify_as_a_side_chat_system_message() {
        let text = absorb_message(&[
            SideExchange {
                question: "What is X?".into(),
                answer: "X is Y.".into(),
            },
            SideExchange {
                question: "And Z?".into(),
                answer: "Z too.".into(),
            },
        ]);
        let (source, body) = classify(&text);
        assert_eq!(
            source,
            MessageSource::System {
                label: "Side chat".into()
            }
        );
        assert_eq!(body, "Q: What is X?\nA: X is Y.\n\nQ: And Z?\nA: Z too.");
        assert!(text.contains("not a new instruction"));
        assert_eq!(
            crate::message_source::preview(&text),
            format!("Mast: {body}")
        );
    }

    #[test]
    fn seed_wraps_the_transcript_before_the_question() {
        let seed = context_seed(
            &[
                (Speaker::User, "Fix the bug".into()),
                (Speaker::Assistant, "Done.".into()),
            ],
            "Why that fix?",
        );
        assert!(
            seed.starts_with(
                "[Mast system message, not from the user: context from the main chat."
            )
        );
        assert!(seed.contains(
            "<main_chat_transcript>\nUser: Fix the bug\n\nAssistant: Done.\n</main_chat_transcript>"
        ));
        assert!(seed.ends_with("\n\nWhy that fix?"));
        assert_eq!(context_seed(&[], "Plain"), "Plain");
    }
}
