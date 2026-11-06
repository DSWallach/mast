//! Context carried by questions about a diff or a walkthrough step.

use super::{Changes, DiffLine, FileDiff, LineKind, SourceSide};
use gpui::{AppContext as _, Context};
use mast_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use mast_proto::ReviewStep;

/// The message identity is retained for transcript correlation, including
/// when the host parks it in the busy chat's queue.
pub(super) struct SentQuestion {
    pub message_id: String,
    pub queued: bool,
}

/// Resolve a matching review from sidecar contents, rather than assuming that
/// an artifact on the same item necessarily describes the displayed target.
pub(super) fn review_chat_for_target<T: 'static>(
    state: gpui::Entity<crate::state::AppState>,
    space_id: &str,
    target_key: &str,
    head_sha: Option<&str>,
    cx: &mut Context<T>,
) -> gpui::Task<Result<Option<String>, String>> {
    let state = state.read(cx);
    let Some(snapshot) = state.artifacts().snapshot(space_id) else {
        return gpui::Task::ready(Err("Review artifacts are still loading".into()));
    };
    let local_device = state.local_device_id.as_deref();
    let actions = state.artifact_actions();
    let mut artifacts: Vec<_> = snapshot
        .artifacts
        .iter()
        .filter(|artifact| {
            artifact.kind == mast_proto::ArtifactKind::Review && artifact.chat_id.is_some()
        })
        .collect();
    artifacts.sort_by_key(|artifact| std::cmp::Reverse(artifact.updated_at));
    let calls: Vec<_> = artifacts
        .into_iter()
        .map(|artifact| {
            let device = (local_device != Some(artifact.device_id.as_str()))
                .then(|| artifact.device_id.clone());
            let call = actions.content(mast_proto::ArtifactContentParams {
                artifact_id: artifact.id.clone(),
                revision: None,
                companion: None,
                target_device_id: device,
            });
            (artifact.chat_id.clone().unwrap(), call)
        })
        .collect();
    let target_key = target_key.to_owned();
    let head_sha = head_sha.map(str::to_owned);
    cx.spawn(async move |_, _| {
        for (chat_id, call) in calls {
            let reply = call.await.map_err(|error| error.text())?;
            if reply.encoding != mast_proto::ArtifactContentEncoding::Utf8 {
                return Err("Review content is not UTF-8".into());
            }
            let document: serde_json::Value = serde_json::from_str(&reply.content)
                .map_err(|error| format!("Invalid review: {error}"))?;
            let target =
                if let Some(target) = document.get("target").filter(|value| !value.is_null()) {
                    target.clone()
                } else {
                    let mut pr = document.get("pr").cloned().ok_or("Review has no target")?;
                    pr.as_object_mut()
                        .ok_or("Invalid legacy review target")?
                        .insert("kind".into(), "pr".into());
                    pr
                };
            let target: mast_proto::ReviewTarget = serde_json::from_value(target)
                .map_err(|error| format!("Invalid review target: {error}"))?;
            let target_head = match &target {
                mast_proto::ReviewTarget::PullRequest { head_sha, .. }
                | mast_proto::ReviewTarget::Branch { head_sha, .. } => head_sha,
            };
            if target.state_key() == target_key
                && head_sha
                    .as_ref()
                    .is_none_or(|head| head.eq_ignore_ascii_case(target_head))
            {
                return Ok(Some(chat_id));
            }
        }
        Ok(None)
    })
}

pub(super) fn send_question<T: 'static>(
    state: gpui::Entity<crate::state::AppState>,
    chat_id: String,
    message: String,
    cx: &mut Context<T>,
) -> gpui::Task<Result<SentQuestion, String>> {
    let (busy, supported, actions) = {
        let state = state.read(cx);
        let busy = matches!(
            state.indicator_for(&chat_id, chrono::Utc::now()),
            crate::state::Indicator::Working | crate::state::Indicator::AwaitingInput
        );
        let capability = mast_proto::capabilities::MESSAGE_QUEUE_V1;
        let supported = state.engine().is_none_or(|engine| {
            engine.engine_info().supports(capability)
                && state.chat_host_supports(&chat_id, capability)
        });
        (busy, supported, state.work_outline_actions())
    };
    if busy && !supported {
        return gpui::Task::ready(Err(
            "Update the chat's engine to queue messages during a response.".into(),
        ));
    }
    let executor = cx.background_executor().clone();
    cx.background_spawn(async move {
        if busy {
            let message_id = actions
                .queue_message(&chat_id, &message)
                .await
                .map_err(|error| error.message)?;
            return Ok(SentQuestion {
                message_id,
                queued: true,
            });
        }
        let params = mast_proto::ContinueChatForWorkItemParams {
            chat_id,
            prompt: mast_proto::WorkItemPrompt {
                text: message,
                attachments: Vec::new(),
            },
            operation_id: uuid::Uuid::new_v4().to_string(),
        };
        let mut reply = actions
            .continue_chat_for_work_item(params.clone())
            .await
            .map_err(|error| error.message)?;
        for _ in 0..6 {
            if reply.state != mast_proto::WorkItemLaunchState::Queued {
                break;
            }
            executor.timer(std::time::Duration::from_millis(500)).await;
            reply = actions
                .continue_chat_for_work_item(params.clone())
                .await
                .map_err(|error| error.message)?;
        }
        if reply.state == mast_proto::WorkItemLaunchState::Rejected {
            return Err(reply
                .detail
                .unwrap_or_else(|| "Message was rejected".into()));
        }
        Ok(SentQuestion {
            message_id: reply.message_id,
            queued: reply.state == mast_proto::WorkItemLaunchState::Queued,
        })
    })
}

/// Find the response belonging to a sent message, never to an earlier turn.
/// The caller supplies the chat's turn state because individual assistant
/// messages can complete while the agent is still working.
pub(super) fn completed_reply(
    entries: &[SessionMessageEntry],
    message_id: &str,
    busy: bool,
) -> Option<String> {
    if busy {
        return None;
    }
    let sent = entries
        .iter()
        .position(|entry| entry.id == message_id && entry.role == MessageRole::User)?;
    let mut text = None;
    for entry in &entries[sent + 1..] {
        if entry.role == MessageRole::User {
            break;
        }
        if entry.role == MessageRole::Assistant {
            if entry.status == Some(MessageStatus::Streaming) {
                return None;
            }
            let parts = entry
                .parts
                .iter()
                .filter_map(|part| match part {
                    MessagePart::Text { text, .. } if !text.trim().is_empty() => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            if !parts.is_empty() {
                text = Some(parts.join("\n"));
            }
        }
    }
    text
}

/// A selection belongs to one file and one side, even in a split diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DiffSelection {
    pub path: String,
    pub side: SourceSide,
    pub anchor: u32,
    pub extent: u32,
}

impl DiffSelection {
    fn contains(&self, path: &str, side: SourceSide, line: u32) -> bool {
        self.path == path
            && self.side == side
            && (self.anchor.min(self.extent)..=self.anchor.max(self.extent)).contains(&line)
    }

    pub fn lines<'a>(&self, file: &'a FileDiff) -> Vec<&'a DiffLine> {
        if file.path != self.path {
            return Vec::new();
        }
        let first = self.anchor.min(self.extent);
        let last = self.anchor.max(self.extent);
        file.hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| {
                let number = match self.side {
                    SourceSide::Old => line.old_no,
                    SourceSide::New => line.new_no,
                };
                number.is_some_and(|number| (first..=last).contains(&number))
            })
            .collect()
    }
}

impl Changes {
    pub(super) fn select_explanation_line(
        &mut self,
        path: &str,
        side: SourceSide,
        line: u32,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(selection) = self
            .explanation_selection
            .as_mut()
            .filter(|selection| extend && selection.path == path && selection.side == side)
        {
            selection.extent = line;
        } else {
            self.explanation_selection = Some(DiffSelection {
                path: path.to_owned(),
                side,
                anchor: line,
                extent: line,
            });
        }
        cx.notify();
    }

    pub(super) fn explanation_line_selected(
        &self,
        path: &str,
        side: SourceSide,
        line: u32,
    ) -> bool {
        self.explanation_selection
            .as_ref()
            .is_some_and(|selection| selection.contains(path, side, line))
    }
}

/// Format the selected lines as a normal chat message, preserving patch markers.
pub(super) fn selection_question(
    path: &str,
    side: SourceSide,
    first: u32,
    last: u32,
    lines: &[&DiffLine],
    target: &str,
) -> String {
    let side = match side {
        SourceSide::Old => "old",
        SourceSide::New => "new",
    };
    let (first, last) = (first.min(last), first.max(last));
    let mut question = format!(
        "Explain briefly (≤5 lines) what this change does and why it might matter for review.\n\nTarget: {target}\nFile: {path}\nSide: {side}\nLines: {first}–{last}\n\nSelected diff:\n"
    );
    for line in lines {
        let marker = match line.kind {
            LineKind::Context => ' ',
            LineKind::Add => '+',
            LineKind::Del => '-',
            LineKind::Meta => '\\',
        };
        question.push(marker);
        question.push_str(&line.text);
        question.push('\n');
    }
    question
}

/// Include stable step identity and anchors with the reader's question.
pub(super) fn step_question(step: &ReviewStep, question: &str) -> String {
    let mut message = format!(
        "Question about review step {}: {}\nFile: {}\nHunks:",
        step.id, step.title, step.file
    );
    for hunk in &step.hunks {
        message.push_str(&format!("\n- {}–{}", hunk.new_start, hunk.new_end));
        if let Some(hash) = &hunk.hunk_hash {
            message.push_str(&format!(" ({hash})"));
        }
    }
    message.push_str("\n\n");
    message.push_str(question);
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{ReviewHunk, ReviewRisk};

    #[test]
    fn reply_is_correlated_to_the_sent_message_and_waits_for_the_turn() {
        let entry = |id: &str, role, text: &str| SessionMessageEntry {
            id: id.into(),
            role,
            parts: vec![MessagePart::Text {
                id: "text".into(),
                text: text.into(),
            }],
            created_at: 0,
            device_id: "local".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        };
        let entries = vec![
            entry("previous", MessageRole::Assistant, "old"),
            entry("sent", MessageRole::User, "question"),
            entry("answer", MessageRole::Assistant, "answer"),
            entry("next", MessageRole::User, "next question"),
            entry("later", MessageRole::Assistant, "wrong"),
        ];
        assert_eq!(
            completed_reply(&entries, "sent", false).as_deref(),
            Some("answer")
        );
        assert_eq!(completed_reply(&entries, "sent", true), None);
        assert_eq!(completed_reply(&entries, "missing", false), None);
    }

    #[test]
    fn selection_respects_file_side_and_reverse_ranges() {
        let files = super::super::parse_patch(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n shared\n",
        );
        let mut selection = DiffSelection {
            path: "a.rs".into(),
            side: SourceSide::Old,
            anchor: 2,
            extent: 1,
        };
        let lines = selection.lines(&files[0]);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["old", "shared"]
        );
        selection.side = SourceSide::New;
        assert_eq!(selection.lines(&files[0])[0].text, "new");
        selection.path = "other.rs".into();
        assert!(selection.lines(&files[0]).is_empty());
    }

    #[test]
    fn selection_message_preserves_side_range_and_patch() {
        let lines = vec![
            DiffLine {
                kind: LineKind::Del,
                old_no: Some(4),
                new_no: None,
                text: "before".into(),
            },
            DiffLine {
                kind: LineKind::Add,
                old_no: None,
                new_no: Some(4),
                text: "after".into(),
            },
        ];
        let message = selection_question(
            "src/lib.rs",
            SourceSide::Old,
            8,
            4,
            &lines.iter().collect::<Vec<_>>(),
            "abc..def",
        );
        assert!(message.contains("Target: abc..def\nFile: src/lib.rs\nSide: old\nLines: 4–8"));
        assert!(message.ends_with("-before\n+after\n"));
    }

    #[test]
    fn step_message_includes_identity_anchors_and_question() {
        let step = ReviewStep {
            id: "step-1".into(),
            title: "Check state".into(),
            file: "src/lib.rs".into(),
            hunks: vec![ReviewHunk {
                new_start: 4,
                new_end: 8,
                hunk_hash: Some("hash".into()),
            }],
            what: String::new(),
            why: String::new(),
            risk: ReviewRisk::Low,
            risk_note: String::new(),
            tags: vec![],
            group: None,
        };
        let message = step_question(&step, "Why now?");
        assert!(message.contains("step-1: Check state\nFile: src/lib.rs\nHunks:\n- 4–8 (hash)"));
        assert!(message.ends_with("Why now?"));
    }
}
