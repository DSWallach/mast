//! What the chats attached to queued items are doing, read from the UI's own
//! chat and session state, and the words the queue views use for a stalled
//! item. The queue derivation lives in `mast_proto::work_queue`, and so does
//! the reading of a chat into activity (`ChatActivity::from_records`), shared
//! with the engine's dispatcher; this only supplies its inputs.

use mast_proto::{Chat, ChatActivity, QueueStall, Session};

/// A lookup over the UI's chats and sessions. A chat the UI does not know is
/// treated as gone (archived).
pub(crate) fn chat_activity_lookup<'a>(
    chats: &'a [Chat],
    sessions: &'a [Session],
) -> impl Fn(&str) -> ChatActivity + 'a {
    move |chat_id| {
        let Some(chat) = chats.iter().find(|chat| chat.id == chat_id) else {
            return ChatActivity { archived: true, ..ChatActivity::default() };
        };
        let session = sessions.iter().find(|session| session.chat_id == chat_id);
        ChatActivity::from_records(
            chat,
            session,
            chrono::Utc::now(),
            crate::quota_guard::is_quota_error_text,
        )
    }
}

/// How a stalled item reads: "errored", "out of credits" or "idle 45m".
pub(crate) fn stall_text(stall: QueueStall) -> String {
    match stall {
        QueueStall::Errored => "errored".into(),
        QueueStall::OutOfCredits => "out of credits".into(),
        QueueStall::Idle { minutes: None } => "idle".into(),
        QueueStall::Idle { minutes: Some(minutes) } if minutes < 60 => format!("idle {minutes}m"),
        QueueStall::Idle { minutes: Some(minutes) } => {
            format!("idle {}h {:02}m", minutes / 60, minutes % 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stall_reads_as_its_reason() {
        assert_eq!(stall_text(QueueStall::Errored), "errored");
        assert_eq!(stall_text(QueueStall::OutOfCredits), "out of credits");
        assert_eq!(stall_text(QueueStall::Idle { minutes: None }), "idle");
        assert_eq!(stall_text(QueueStall::Idle { minutes: Some(45) }), "idle 45m");
        assert_eq!(stall_text(QueueStall::Idle { minutes: Some(125) }), "idle 2h 05m");
    }
}
