//! Context carried by questions about a diff or a walkthrough step.

use super::{Changes, DiffLine, FileDiff, LineKind, SourceSide};
use gpui::{AppContext as _, Context, IntoElement, div, prelude::*, px};
use mast_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use mast_proto::ReviewStep;

gpui::actions!(diff_explanation, [ExplainDiff]);

pub(super) fn bind_keys(cx: &mut gpui::App, keymap: &crate::settings::KeymapConfig) {
    let id = crate::settings::ShortcutId::ExplainDiff;
    let combo = keymap.get(id);
    if combo.is_empty() {
        return;
    }
    let combo = crate::settings::platform_combo(combo);
    let combo = if gpui::Keystroke::parse(&combo).is_ok() {
        combo
    } else {
        crate::settings::platform_combo(id.default_combo())
    };
    let predicate = Some(std::rc::Rc::new(
        gpui::KeyBindingContextPredicate::parse(
            "ChangesDiff && !Input && !Composer && !MessageComposer && !PaletteSearch",
        )
        .unwrap(),
    ));
    if let Ok(binding) = gpui::KeyBinding::load(
        &combo,
        Box::new(ExplainDiff),
        predicate,
        false,
        None,
        &gpui::DummyKeyboardMapper,
    ) {
        cx.bind_keys([binding]);
    }
}

pub(super) enum ExplanationPanel {
    Waiting {
        queued: bool,
        chat_id: Option<String>,
    },
    Answer {
        chat_id: String,
        text: String,
    },
    Error(String),
}

/// The message identity is retained for transcript correlation, including
/// when the host parks it in the busy chat's queue.
pub(super) struct SentQuestion {
    pub message_id: String,
    pub queued: bool,
}

/// Watch the destination transcript independently of whichever chat the
/// reader currently has open. Dropping the task stops waiting only.
pub(super) fn watch_question<T: 'static>(
    state: gpui::Entity<crate::state::AppState>,
    chat_id: String,
    message_id: String,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    cx: &mut Context<T>,
) -> gpui::Task<Result<String, String>> {
    let Some(engine) = state.read(cx).engine().cloned() else {
        return gpui::Task::ready(Err("Engine not connected".into()));
    };
    cx.spawn(async move |_, cx| {
        let mut receiver = engine
            .client()
            .subscribe(
                mast_rpc::methods::WATCH_DOC_MESSAGES,
                serde_json::json!({"chatId": chat_id}),
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut entries = Vec::new();
        loop {
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("Cancelled".into());
            }
            let receive = receiver.recv();
            let timer = cx
                .background_executor()
                .timer(std::time::Duration::from_millis(250));
            futures::pin_mut!(receive, timer);
            if let futures::future::Either::Left((value, _)) =
                futures::future::select(receive, timer).await
            {
                let value = value.ok_or("Transcript watch ended")?;
                let update: mast_doc::TranscriptUpdate =
                    serde_json::from_value(value).map_err(|error| error.to_string())?;
                mast_doc::apply_transcript_frame(&mut entries, update.frame)
                    .map_err(|error| error.to_string())?;
            }
            let busy = state.update(cx, |state, _| {
                matches!(
                    state.indicator_for(&chat_id, chrono::Utc::now()),
                    crate::state::Indicator::Working | crate::state::Indicator::AwaitingInput
                )
            });
            if let Some(reply) = completed_reply(&entries, &message_id, busy) {
                return Ok(reply);
            }
        }
    })
}

/// Resolve a matching review from sidecar contents, rather than assuming that
/// an artifact on the same item necessarily describes the displayed target.
pub(super) fn review_chat_for_target<T: 'static>(
    state: gpui::Entity<crate::state::AppState>,
    space_id: &str,
    target_key: &str,
    head_sha: Option<&str>,
    branch_hint: Option<(String, String, String)>,
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
            artifact.kind == mast_proto::ArtifactKind::Review
                && artifact.chat_id.is_some()
                && artifact.archived_at.is_none()
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
            (
                artifact.chat_id.clone().unwrap(),
                artifact.device_id.clone(),
                call,
            )
        })
        .collect();
    let target_key = target_key.to_owned();
    let head_sha = head_sha.map(str::to_owned);
    cx.spawn(async move |_, _| {
        for (chat_id, device_id, call) in calls {
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
            if review_target_matches(
                &target,
                &target_key,
                head_sha.as_deref(),
                branch_hint.as_ref(),
                &device_id,
            ) {
                return Ok(Some(chat_id));
            }
        }
        Ok(None)
    })
}

fn review_target_matches(
    target: &mast_proto::ReviewTarget,
    key: &str,
    head: Option<&str>,
    branch_hint: Option<&(String, String, String)>,
    device_id: &str,
) -> bool {
    let target_head = match target {
        mast_proto::ReviewTarget::PullRequest { head_sha, .. }
        | mast_proto::ReviewTarget::Branch { head_sha, .. } => head_sha,
    };
    let branch_matches = key.starts_with("ref:") && branch_hint.is_some_and(|(device, path, name)| {
        device == device_id && matches!(target, mast_proto::ReviewTarget::Branch { checkout_path: Some(checkout), branch: Some(branch), .. } if checkout == path && branch == name)
    });
    (target.state_key() == key || branch_matches)
        && head.is_none_or(|head| head.eq_ignore_ascii_case(target_head))
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
    if !supported {
        return gpui::Task::ready(Err(
            "Update the chat's engine to queue messages during a response.".into(),
        ));
    }
    cx.background_spawn(async move {
        // A held queue row drains immediately when the chat is idle and also
        // works for chats that have no Work Outline assignment.
        let message_id = actions
            .queue_message(&chat_id, &message)
            .await
            .map_err(|error| error.message)?;
        Ok(SentQuestion {
            message_id,
            queued: busy,
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
    pub(super) fn open_explanation_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        if let Some(shell) = self
            .explanation_shell
            .as_ref()
            .and_then(|shell| shell.upgrade())
        {
            shell.update(cx, |shell, cx| shell.open_chat(chat_id, cx));
        } else {
            self.state
                .update(cx, |state, cx| state.select_chat(Some(chat_id), cx));
        }
    }

    pub(super) fn explain_focused_hunk(&mut self, cx: &mut Context<Self>) {
        if self.explanation_selection.is_some() {
            cx.notify();
            return;
        }
        let top = self
            .list
            .logical_scroll_top()
            .item_ix
            .min(self.rows.len().saturating_sub(1));
        let file = self.rows.get(top).and_then(|row| row.file());
        let Some((file_index, hunk_index)) =
            self.rows
                .iter()
                .take(top + 1)
                .rev()
                .find_map(|row| match row {
                    super::DiffRow::HunkHeader {
                        file: candidate,
                        hunk,
                    } if Some(*candidate as usize) == file => {
                        Some((*candidate as usize, *hunk as usize))
                    }
                    _ => None,
                })
        else {
            return;
        };
        let Some(file) = self
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.files.get(file_index))
        else {
            return;
        };
        let Some(hunk) = file.hunks.get(hunk_index) else {
            return;
        };
        let side = if hunk.lines.iter().any(|line| line.new_no.is_some()) {
            SourceSide::New
        } else {
            SourceSide::Old
        };
        let numbers: Vec<_> = hunk
            .lines
            .iter()
            .filter_map(|line| match side {
                SourceSide::Old => line.old_no,
                SourceSide::New => line.new_no,
            })
            .collect();
        let (Some(first), Some(last)) = (numbers.first(), numbers.last()) else {
            return;
        };
        self.explanation_selection = Some(DiffSelection {
            path: file.path.clone(),
            side,
            anchor: *last,
            extent: *first,
        });
        self.explanation_hunk = Some((file_index, hunk_index));
        if let Some(row) = self.rows.iter().position(|row| match row {
            super::DiffRow::Line {
                file, hunk, line, ..
            } if *file as usize == file_index && *hunk as usize == hunk_index => {
                let line = &self.parsed.as_ref().unwrap().files[file_index].hunks[hunk_index].lines
                    [*line as usize];
                (match side {
                    SourceSide::Old => line.old_no,
                    SourceSide::New => line.new_no,
                }) == Some(*first)
            }
            super::DiffRow::SplitLine {
                file,
                hunk,
                left,
                right,
            } if *file as usize == file_index && *hunk as usize == hunk_index => {
                let index = match side {
                    SourceSide::Old => *left,
                    SourceSide::New => *right,
                };
                index.is_some_and(|index| {
                    let line = &self.parsed.as_ref().unwrap().files[file_index].hunks[hunk_index]
                        .lines[index as usize];
                    (match side {
                        SourceSide::Old => line.old_no,
                        SourceSide::New => line.new_no,
                    }) == Some(*first)
                })
            }
            _ => false,
        }) {
            self.list.scroll_to_reveal_item(row);
        }
        cx.notify();
    }

    pub(super) fn render_explanation(
        &self,
        theme: &crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let trigger = div()
            .id("diff-explanation")
            .absolute()
            .right(px(8.0))
            .top(px(0.0))
            .px_2()
            .when(true, |el| crate::components::explanation_trigger(el, theme))
            .text_color(theme.text)
            .text_size(crate::typography::ui_rems(12.0))
            .child("Explain")
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation());
        match &self.explanation_panel {
            None => trigger
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| this.explain_selection(cx)))
                .into_any_element(),
            Some(status) => {
                let (label, chat_id) = match status {
                    ExplanationPanel::Waiting { queued, chat_id } => (
                        if *queued {
                            "Queued".to_owned()
                        } else {
                            "Waiting for explanation…".to_owned()
                        },
                        chat_id.clone(),
                    ),
                    ExplanationPanel::Answer { chat_id, text } => {
                        (text.clone(), Some(chat_id.clone()))
                    }
                    ExplanationPanel::Error(error) => (error.clone(), None),
                };
                let mut panel = div()
                    .id("explanation-answer-panel")
                    .debug_selector(|| "explanation-answer-panel".into())
                    .w(px(320.0))
                    .p_2()
                    .text_size(crate::typography::ui_rems(12.0))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .text_color(theme.text)
                    .child(label);
                if matches!(status, ExplanationPanel::Waiting { .. }) {
                    panel = panel.child(crate::components::changes_loading(
                        theme, "explanation-spinner", cx.entity_id(), cx,
                    ));
                }
                if let Some(chat_id) = chat_id {
                    panel = panel.child(
                        div()
                            .id("explanation-open-chat")
                            .cursor_pointer()
                            .child("Open in chat")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_explanation_chat(chat_id.clone(), cx);
                            })),
                    );
                }
                panel = panel.child(
                    div()
                        .id("explanation-close")
                        .cursor_pointer()
                        .child(if matches!(status, ExplanationPanel::Waiting { .. }) {
                            "Cancel"
                        } else {
                            "Close"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_explanation();
                            cx.notify();
                        })),
                );
                trigger
                    .child(crate::popover::anchored_menu_below_end(
                        "explanation-panel",
                        panel.into_any_element(),
                        None,
                    ))
                    .into_any_element()
            }
        }
    }

    pub(super) fn cancel_explanation(&mut self) {
        if let Some(cancelled) = self.explanation_cancelled.take() {
            cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(task) = self.explanation_task.take() {
            task.detach();
        }
        self.explanation_panel = None;
    }

    pub(super) fn explain_selection(&mut self, cx: &mut Context<Self>) {
        self.cancel_explanation();
        if self.review_link_task.is_some() || self.review_branch_lookup_task.is_some() {
            self.explanation_panel = Some(ExplanationPanel::Error(
                "Review target is still loading; try again shortly.".into(),
            ));
            cx.notify();
            return;
        }
        let Some(selection) = self.explanation_selection.clone() else {
            return;
        };
        let Some(file) = self
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.files.iter().find(|file| file.path == selection.path))
        else {
            return;
        };
        let lines: Vec<&DiffLine> = self
            .explanation_hunk
            .and_then(|(file_index, hunk_index)| {
                self.parsed
                    .as_ref()?
                    .files
                    .get(file_index)?
                    .hunks
                    .get(hunk_index)
            })
            .map(|hunk| hunk.lines.iter().collect())
            .or_else(|| {
                self.explanation_unified_rows.map(|(start, end, _)| {
                    self.rows
                        .iter()
                        .skip(start.min(end))
                        .take(start.max(end) - start.min(end) + 1)
                        .filter_map(|row| match row {
                            super::DiffRow::Line {
                                file: file_index,
                                hunk,
                                line,
                                ..
                            } => {
                                let candidate =
                                    self.parsed.as_ref()?.files.get(*file_index as usize)?;
                                if candidate.path != selection.path {
                                    return None;
                                }
                                candidate
                                    .hunks
                                    .get(*hunk as usize)?
                                    .lines
                                    .get(*line as usize)
                            }
                            _ => None,
                        })
                        .collect()
                })
            })
            .unwrap_or_else(|| selection.lines(file));
        if lines.is_empty() {
            return;
        }
        let Some(owner) = self.owner.as_ref() else {
            return;
        };
        let owner_chat = owner.chat_id.clone();
        let head = self
            .commit
            .as_ref()
            .map(|commit| commit.sha.clone())
            .or_else(|| {
                owner
                    .range
                    .as_ref()
                    .and_then(|range| range.head_sha.clone())
            });
        let target = if let Some(head) = &head {
            owner
                .range
                .as_ref()
                .map(|range| format!("{} ({head}, base {})", range.label, range.base_ref))
                .unwrap_or_else(|| head.clone())
        } else {
            let mut target = self.scope.label().to_owned();
            if let Some(branch) = &owner.branch {
                target.push_str(&format!("; branch {branch}"));
            }
            if let Some(base) = &self.base_ref {
                target.push_str(&format!("; base {base}"));
            }
            if let Some(path) = &owner.cwd {
                target.push_str(&format!("; checkout {path}"));
            }
            target
        };
        let end_side = self
            .explanation_unified_rows
            .map(|(_, _, side)| side)
            .unwrap_or(selection.side);
        let display_path = if selection.side != end_side {
            file.old_path
                .as_ref()
                .map(|old| format!("{old} → {}", file.path))
                .unwrap_or_else(|| file.path.clone())
        } else if selection.side == SourceSide::Old {
            file.old_path
                .clone()
                .unwrap_or_else(|| selection.path.clone())
        } else {
            selection.path.clone()
        };
        let message = selection_question_for_sides(
            &display_path,
            selection.side,
            end_side,
            selection.anchor,
            selection.extent,
            &lines,
            &target,
        );
        let space = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == owner_chat)
            .and_then(|chat| chat.space_id.clone());
        let lookup = match (space, self.review_target.as_deref()) {
            (Some(space), Some(key)) => Some(review_chat_for_target(
                self.state.clone(),
                &space,
                key,
                head.as_deref(),
                owner.range.as_ref().and_then(|range| {
                    owner.cwd.as_ref().map(|path| {
                        (
                            owner.device_id.clone(),
                            path.clone(),
                            range.head_branch.clone(),
                        )
                    })
                }),
                cx,
            )),
            _ => None,
        };
        self.explanation_panel = Some(ExplanationPanel::Waiting {
            queued: false,
            chat_id: None,
        });
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.explanation_cancelled = Some(cancelled.clone());
        let state = self.state.clone();
        self.explanation_task = Some(cx.spawn(async move |this, cx| {
            let chat_id = match lookup {
                Some(lookup) => match lookup.await {
                    Ok(chat) => chat.unwrap_or(owner_chat),
                    Err(error) => {
                        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        let _ = this.update(cx, |this, cx| {
                            this.explanation_panel = Some(ExplanationPanel::Error(error));
                            cx.notify();
                        });
                        return;
                    }
                },
                None => owner_chat,
            };
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let send = match this.update(cx, |_, cx| {
                send_question(state.clone(), chat_id.clone(), message, cx)
            }) {
                Ok(task) => task,
                Err(_) => return,
            };
            let sent = match send.await {
                Ok(sent) => sent,
                Err(error) => {
                    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let _ = this.update(cx, |this, cx| {
                        this.explanation_panel = Some(ExplanationPanel::Error(error));
                        cx.notify();
                    });
                    return;
                }
            };
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let watch = match this.update(cx, |this, cx| {
                this.explanation_panel = Some(ExplanationPanel::Waiting {
                    queued: sent.queued,
                    chat_id: Some(chat_id.clone()),
                });
                cx.notify();
                watch_question(
                    state,
                    chat_id.clone(),
                    sent.message_id,
                    cancelled.clone(),
                    cx,
                )
            }) {
                Ok(task) => task,
                Err(_) => return,
            };
            let result = watch.await;
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.explanation_panel = Some(match result {
                    Ok(text) => ExplanationPanel::Answer { chat_id, text },
                    Err(error) => ExplanationPanel::Error(error),
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn select_explanation_line(
        &mut self,
        path: &str,
        side: SourceSide,
        line: u32,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        self.cancel_explanation();
        self.explanation_hunk = None;
        self.explanation_unified_rows = None;
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

    pub(super) fn select_unified_explanation_line(
        &mut self,
        path: &str,
        side: SourceSide,
        line: u32,
        row: usize,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        let existing = self.explanation_unified_rows.filter(|_| {
            extend
                && self
                    .explanation_selection
                    .as_ref()
                    .is_some_and(|selection| selection.path == path)
        });
        if let Some((anchor, _, _)) = existing {
            self.cancel_explanation();
            self.explanation_hunk = None;
            self.explanation_selection.as_mut().unwrap().extent = line;
            self.explanation_unified_rows = Some((anchor, row, side));
            cx.notify();
        } else {
            self.select_explanation_line(path, side, line, false, cx);
            self.explanation_unified_rows = Some((row, row, side));
        }
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
    selection_question_for_sides(path, side, side, first, last, lines, target)
}

fn selection_question_for_sides(
    path: &str,
    side: SourceSide,
    end_side: SourceSide,
    first: u32,
    last: u32,
    lines: &[&DiffLine],
    target: &str,
) -> String {
    let side_name = |side| match side {
        SourceSide::Old => "old",
        SourceSide::New => "new",
    };
    let (side, range) = if side == end_side {
        (
            side_name(side).to_owned(),
            format!("{}–{}", first.min(last), first.max(last)),
        )
    } else {
        (
            "mixed".to_owned(),
            format!(
                "{}:{} → {}:{}",
                side_name(side),
                first,
                side_name(end_side),
                last
            ),
        )
    };
    let mut question = format!(
        "Explain briefly (≤5 lines) what this change does and why it might matter for review.\n\nTarget: {target}\nFile: {path}\nSide: {side}\nLines: {range}\n\nSelected diff:\n"
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
        "Answer briefly (≤5 lines).\n\nQuestion about review step {}: {}\nFile: {}\nHunks:",
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
    fn local_branch_review_requires_matching_checkout_branch_device_and_head() {
        let target = mast_proto::ReviewTarget::Branch {
            repo: None,
            checkout_path: Some("/repo".into()),
            branch: Some("feature".into()),
            base_ref: None,
            base_sha: None,
            head_sha: "abc".into(),
        };
        let hint = ("device".into(), "/repo".into(), "feature".into());
        assert!(review_target_matches(
            &target,
            "ref:device:/repo:refs/heads/feature",
            Some("abc"),
            Some(&hint),
            "device"
        ));
        assert!(!review_target_matches(
            &target,
            "ref:device:/repo:refs/heads/feature",
            Some("different"),
            Some(&hint),
            "device"
        ));
        assert!(!review_target_matches(
            &target,
            "ref:device:/repo:refs/heads/feature",
            Some("abc"),
            Some(&hint),
            "other"
        ));
    }

    #[test]
    fn explanation_shortcut_is_persisted_and_rebindable() {
        let mut keymap = crate::settings::KeymapConfig::default();
        assert_eq!(keymap.get(crate::settings::ShortcutId::ExplainDiff), "e");
        keymap.set(crate::settings::ShortcutId::ExplainDiff, "x".into());
        let restored: crate::settings::KeymapConfig =
            serde_json::from_str(&serde_json::to_string(&keymap).unwrap()).unwrap();
        assert_eq!(restored.get(crate::settings::ShortcutId::ExplainDiff), "x");
    }

    #[gpui::test]
    fn rebound_explanation_shortcut_dispatches_in_diff_context(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            let mut keymap = crate::settings::KeymapConfig::default();
            keymap.set(crate::settings::ShortcutId::ExplainDiff, "x".into());
            bind_keys(cx, &keymap);
        });
        struct Host {
            focus: gpui::FocusHandle,
            invoked: bool,
        }
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .key_context("ChangesDiff")
                    .track_focus(&self.focus)
                    .on_action(cx.listener(|this, _: &ExplainDiff, _, cx| {
                        this.invoked = true;
                        cx.notify();
                    }))
            }
        }
        let (view, vcx) = cx.add_window_view(|_, cx| Host {
            focus: cx.focus_handle(),
            invoked: false,
        });
        vcx.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
            window.draw(cx).clear();
        });
        vcx.simulate_keystrokes("e");
        assert!(!view.read_with(vcx, |view, _| view.invoked));
        vcx.simulate_keystrokes("x");
        assert!(view.read_with(vcx, |view, _| view.invoked));
    }

    #[gpui::test]
    fn review_chat_matches_target_and_head(cx: &mut gpui::TestAppContext) {
        use futures::FutureExt as _;
        let artifact: mast_proto::ArtifactView = serde_json::from_value(serde_json::json!({"id":"review", "spaceId":"space", "deviceId":"device", "chatId":"review-chat", "kind":"review"})).unwrap();
        let snapshot = mast_proto::ArtifactsSnapshot {
            space_id: "space".into(),
            revision: 1,
            artifacts: vec![artifact],
        };
        let state = cx.new(|_| {
            let mut state = crate::state::AppState::new();
            state.local_device_id = Some("device".into());
            state.set_test_artifacts(snapshot);
            state.work_outline_actions_override = Some(crate::work_outline::WorkOutlineActions::scripted(std::sync::Arc::new(|_, _| Ok(serde_json::json!({"artifactId":"review", "encoding":"utf8", "content": "{\"target\":{\"kind\":\"pr\",\"repo\":\"Org/Repo\",\"number\":42,\"headSha\":\"abc\",\"baseSha\":\"base\"}}"})))));
            state
        });
        struct TestView;
        impl gpui::Render for TestView {
            fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
            }
        }
        let (view, vcx) = cx.add_window_view(|_, _| TestView);
        let task = view.update(vcx, |_, cx| {
            review_chat_for_target(
                state.clone(),
                "space",
                "pr:org/repo#42",
                Some("abc"),
                None,
                cx,
            )
        });
        vcx.run_until_parked();
        assert_eq!(
            task.now_or_never().unwrap().unwrap().as_deref(),
            Some("review-chat")
        );
        let task = view.update(vcx, |_, cx| {
            review_chat_for_target(
                state,
                "space",
                "pr:org/repo#42",
                Some("different"),
                None,
                cx,
            )
        });
        vcx.run_until_parked();
        assert_eq!(task.now_or_never().unwrap().unwrap(), None);
    }

    #[gpui::test]
    fn explanation_reply_renders_in_anchored_panel(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::dark());
        });
        struct Host {
            changes: gpui::Entity<Changes>,
        }
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
                let theme = crate::theme::Theme::of(cx).clone();
                let panel = self
                    .changes
                    .update(cx, |changes, cx| changes.render_explanation(&theme, cx));
                div().size_full().relative().child(panel)
            }
        }
        let (_, vcx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            let changes = cx.new(|cx| Changes::new(state, cx));
            changes.update(cx, |changes, _| {
                changes.explanation_panel = Some(ExplanationPanel::Answer {
                    chat_id: "chat".into(),
                    text: "A short explanation".into(),
                })
            });
            Host { changes }
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("explanation-answer-panel").is_some());
    }

    #[gpui::test]
    fn sends_question_and_queues_when_chat_is_busy(cx: &mut gpui::TestAppContext) {
        use futures::FutureExt as _;
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::dark());
        });
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let state = cx.new(|_| {
            let mut state = crate::state::AppState::new();
            state.work_outline_actions_override =
                Some(crate::work_outline::WorkOutlineActions::scripted(
                    std::sync::Arc::new(move |method, params| {
                        recorded.lock().unwrap().push((method.to_owned(), params));
                        Ok(serde_json::json!({"id":"queued-id"}))
                    }),
                ));
            state
        });
        struct TestView;
        impl gpui::Render for TestView {
            fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
            }
        }
        let (view, vcx) = cx.add_window_view(|_, _| TestView);
        let line = DiffLine {
            kind: LineKind::Add,
            old_no: None,
            new_no: Some(7),
            text: "change".into(),
        };
        let message = selection_question("a.rs", SourceSide::New, 7, 7, &[&line], "base..head");
        let task = view.update(vcx, |_, cx| {
            send_question(state.clone(), "chat".into(), message, cx)
        });
        vcx.run_until_parked();
        let result = task.now_or_never().unwrap().unwrap();
        assert_eq!(result.message_id, "queued-id");
        assert!(!result.queued);
        state.update(vcx, |state, _| {
            state.begin_pending_send("chat", "pending", chrono::Utc::now())
        });
        let task = view.update(vcx, |_, cx| {
            send_question(state.clone(), "chat".into(), "another question".into(), cx)
        });
        vcx.run_until_parked();
        let result = task.now_or_never().unwrap().unwrap();
        assert_eq!(result.message_id, "queued-id");
        assert!(result.queued);
        let calls = calls.lock().unwrap();
        assert_eq!(calls[0].0, mast_rpc::methods::QUEUE_MESSAGE);
        let sent_text = calls[0].1["text"].as_str().unwrap();
        assert!(sent_text.contains("Target: base..head\nFile: a.rs\nSide: new\nLines: 7–7"));
        assert!(sent_text.ends_with("+change\n"));
        assert_eq!(calls[1].0, mast_rpc::methods::QUEUE_MESSAGE);
        assert_eq!(calls[1].1["text"], "another question");
        assert_eq!(calls[1].1["holdForTurnEnd"], true);
    }

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
