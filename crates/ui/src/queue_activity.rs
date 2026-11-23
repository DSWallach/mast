//! What the chats attached to queued items are doing, read from the UI's own
//! chat and session state, and the words the queue views use for a stalled
//! item. The queue derivation lives in `mast_proto::work_queue`, and so does
//! the reading of a chat into activity (`ChatActivity::from_records`), shared
//! with the engine's dispatcher; this only supplies its inputs.

use mast_proto::{Chat, ChatActivity, QueueEntry, QueuePauseNote, QueuePausedBy, QueueStall, Session};

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

/// Who paused the queue and why, beside the pause: "Paused by the Manager:
/// waiting on the release freeze". The reason is absent for a user's pause.
pub(crate) fn pause_note_text(note: &QueuePauseNote) -> String {
    let by = match note.by {
        QueuePausedBy::User => "you",
        QueuePausedBy::Admin => "an admin",
        QueuePausedBy::Manager => "the Manager",
    };
    // The reason is free text from a Manager: fold newlines and runs of spaces
    // so it reads as one line wherever it is shown.
    let reason = note.reason.as_deref().map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "));
    match reason.filter(|reason| !reason.is_empty()) {
        Some(reason) => format!("Paused by {by}: {reason}"),
        None => format!("Paused by {by}"),
    }
}

/// "waiting for <title>", with a landing or cycle note when that is the reason.
pub fn waiting_text(entry: &QueueEntry) -> String {
    use mast_proto::QueueWaitReason;
    let Some(first) = entry.waiting_on.first() else {
        return "waiting".into();
    };
    if first.reason == QueueWaitReason::Missing {
        return "waiting for a prerequisite that is not in this project".into();
    }
    let more = entry.waiting_on.len().saturating_sub(1);
    let note = match first.reason {
        QueueWaitReason::NotDone => "to finish",
        QueueWaitReason::NotLanded => "to land",
        QueueWaitReason::Archived => "(archived, so it will not land)",
        QueueWaitReason::Missing => "",
        QueueWaitReason::Cycle => "(waits back on this item)",
    };
    let extra = if more > 0 { format!(" and {more} more") } else { String::new() };
    format!("waiting for \"{}\" {note}{extra}", first.title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pause_reads_as_who_paused_and_why() {
        let note = |by, reason: Option<&str>| QueuePauseNote { by, reason: reason.map(str::to_owned), chat_id: None };
        assert_eq!(
            pause_note_text(&note(QueuePausedBy::Manager, Some("waiting on the release freeze"))),
            "Paused by the Manager: waiting on the release freeze"
        );
        assert_eq!(pause_note_text(&note(QueuePausedBy::Admin, Some("  audit "))), "Paused by an admin: audit");
        assert_eq!(pause_note_text(&note(QueuePausedBy::User, None)), "Paused by you");
        assert_eq!(pause_note_text(&note(QueuePausedBy::Manager, Some("  "))), "Paused by the Manager");
        assert_eq!(
            pause_note_text(&note(QueuePausedBy::Manager, Some("a\n\nb \t c"))),
            "Paused by the Manager: a b c"
        );
    }

    #[test]
    fn a_stall_reads_as_its_reason() {
        assert_eq!(stall_text(QueueStall::Errored), "errored");
        assert_eq!(stall_text(QueueStall::OutOfCredits), "out of credits");
        assert_eq!(stall_text(QueueStall::Idle { minutes: None }), "idle");
        assert_eq!(stall_text(QueueStall::Idle { minutes: Some(45) }), "idle 45m");
        assert_eq!(stall_text(QueueStall::Idle { minutes: Some(125) }), "idle 2h 05m");
    }

    #[test]
    fn a_waiting_entry_names_what_it_waits_for_and_why() {
        use mast_proto::{QueueEntryState, QueueWait, QueueWaitReason};
        let wait = |title: &str, reason| QueueWait { work_item_id: "i".into(), title: title.into(), reason, facts_pending: false };
        let entry = |waiting_on| QueueEntry {
            work_item_id: "w".into(),
            title: "T".into(),
            state: QueueEntryState::Waiting,
            facts_pending: false,
            stall: None,
            waiting_on,
        };
        assert_eq!(waiting_text(&entry(vec![])), "waiting");
        assert_eq!(
            waiting_text(&entry(vec![wait("Implementation", QueueWaitReason::NotLanded)])),
            "waiting for \"Implementation\" to land"
        );
        assert_eq!(
            waiting_text(&entry(vec![
                wait("A", QueueWaitReason::NotDone),
                wait("B", QueueWaitReason::NotLanded),
            ])),
            "waiting for \"A\" to finish and 1 more"
        );
        assert_eq!(
            waiting_text(&entry(vec![wait("x", QueueWaitReason::Missing)])),
            "waiting for a prerequisite that is not in this project"
        );
        assert_eq!(
            waiting_text(&entry(vec![wait("A", QueueWaitReason::Cycle)])),
            "waiting for \"A\" (waits back on this item)"
        );
    }
}
