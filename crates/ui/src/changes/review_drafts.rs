//! Indices for review annotations, rebuilt when the review revision changes.
use super::{
    AnyElement, Changes, Context, DiffRow, LineKind, SharedString, Theme, div, methods, px,
};
use crate::comments::CommentSide;
use gpui::Focusable as _;
use gpui::prelude::*;
use mast_proto::{ReviewDraft, ReviewSide, ReviewThread};

pub(super) struct LocalDraftEdit {
    pub(super) draft: ReviewDraft,
    pub(super) original: Option<ReviewDraft>,
}

fn suggestion_body(suggestion: &mast_proto::ReviewSuggestedComment) -> String {
    match suggestion
        .suggestion
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        Some(block) => format!("{}\n\n```suggestion\n{}\n```", suggestion.body, block),
        None => suggestion.body.clone(),
    }
}

fn thread_comment_height(body: &str) -> f32 {
    let lines: usize = body
        .split('\n')
        .map(|line| line.chars().count().div_ceil(40).max(1))
        .sum();
    36.0 + lines as f32 * 18.0
}

impl Changes {
    pub(super) fn set_review_suggestions(
        &mut self,
        target: &str,
        head: &str,
        suggestions: &[mast_proto::ReviewSuggestedComment],
        cx: &mut Context<Self>,
    ) {
        self.suggestion_target = Some(target.to_owned());
        self.suggestion_head = Some(head.to_owned());
        self.suggestion_display_head = Some(self.review_identity(cx).and_then(|(_, head)| head));
        self.review_suggestions = suggestions
            .iter()
            .map(|suggestion| {
                (
                    format!("suggestion:{}", suggestion.stable_id()),
                    suggestion.clone(),
                )
            })
            .collect();
        self.rebuild_draft_index(cx);
    }

    pub(super) fn refresh_suggestion_head(&mut self, head: Option<String>, cx: &mut Context<Self>) {
        if self.suggestion_display_head.as_ref() != Some(&head) {
            self.suggestion_display_head = Some(head);
            self.rebuild_suggestion_comments(cx);
        }
    }

    fn load_suggestion_dispositions(&mut self, target: &str, cx: &mut Context<Self>) {
        if self.suggestion_dispositions_target.as_deref() == Some(target) {
            return;
        }
        self.suggestion_load_task = None;
        self.suggestion_dispositions_target = None;
        self.dismissed_suggestions.clear();
        self.suggestion_comments.clear();
        self.outdated_suggestions.clear();
        let target = target.to_owned();
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::GET_DISMISSED_REVIEW_SUGGESTIONS,
                serde_json::json!({"target": target}),
                cx,
            )
        });
        self.suggestion_load_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|value| {
                serde_json::from_value::<Vec<String>>(value).map_err(|error| error.to_string())
            });
            this.update(cx, |this, cx| {
                if this.review_target.as_deref() != Some(&target) {
                    return;
                }
                this.suggestion_load_task = None;
                match result {
                    Ok(ids) => {
                        this.dismissed_suggestions = ids.into_iter().collect();
                        this.suggestion_dispositions_target = Some(target);
                        this.rebuild_suggestion_comments(cx);
                    }
                    Err(error) => {
                        this.local_draft_error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    fn rebuild_suggestion_comments(&mut self, cx: &mut Context<Self>) {
        self.suggestion_comments.clear();
        self.outdated_suggestions.clear();
        if self.suggestion_target != self.review_target
            || self.suggestion_dispositions_target != self.review_target
        {
            self.refresh_thread_rows(cx);
            return;
        }
        let mut suggestions: Vec<_> = self.review_suggestions.iter().collect();
        suggestions.sort_by(|a, b| a.0.cmp(b.0));
        for (id, suggestion) in suggestions {
            if self.dismissed_suggestions.contains(&suggestion.stable_id()) {
                continue;
            }
            let same_head = self
                .suggestion_head
                .as_deref()
                .zip(
                    self.suggestion_display_head
                        .as_ref()
                        .and_then(|head| head.as_deref()),
                )
                .is_some_and(|(reviewed, current)| reviewed.eq_ignore_ascii_case(current));
            if !same_head
                || self
                    .draft_anchor_index
                    .line_hash_at(&suggestion.anchor, suggestion.anchor.start_line)
                    .is_none()
                || self
                    .draft_anchor_index
                    .line_hash_at(&suggestion.anchor, suggestion.anchor.end_line)
                    .is_none()
            {
                self.outdated_suggestions
                    .push((id.clone(), suggestion_body(suggestion)));
                continue;
            }
            let side = match suggestion.anchor.side {
                ReviewSide::Left => CommentSide::Old,
                ReviewSide::Right => CommentSide::New,
            };
            let mut comment = crate::comments::ReviewComment::new(
                &suggestion.anchor.file,
                side,
                suggestion.anchor.end_line,
                suggestion_body(suggestion),
            );
            comment.id = id.clone();
            self.suggestion_comments.push(comment);
        }
        self.refresh_thread_rows(cx);
    }

    pub(super) fn render_suggestion_card(
        &self,
        comment: &crate::comments::ReviewComment,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let accept_id = comment.id.clone();
        let edit_id = comment.id.clone();
        let dismiss_id = comment.id.clone();
        let label = self
            .review_suggestions
            .get(&comment.id)
            .map(|suggestion| format!("Draft · agent suggestion · {}", suggestion.severity))
            .unwrap_or_else(|| "Draft · agent suggestion".into());
        let button = |id: String, text: &'static str| {
            crate::components::button(SharedString::from(id), text,
                if text == "Accept" { crate::components::ButtonVariant::Primary } else { crate::components::ButtonVariant::Secondary },
                crate::components::ButtonSize::Sm, theme)
        };
        div()
            .w_full()
            .debug_selector(|| "review-suggestion-card".into())
            .h(crate::typography::ui_rems(crate::comments::card_height(&comment.body)))
            .overflow_hidden()
            .flex()
            .flex_col()
            .px(px(Theme::SPACE_LG))
            .when(true, |el| crate::components::review_card(el, theme))
            .child(
                div()
                    .h(crate::typography::ui_rems(24.0))
                    .flex()
                    .items_center()
                    .gap(px(Theme::SPACE_SM))
                    .child(
                        div().flex_1().flex().items_center().child(div().when(true, |el| crate::components::review_draft_label(el, theme)).child(label)),
                    )
                    .child(
                        button(format!("suggestion-accept-{}", comment.id), "Accept").on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.accept_suggestion(&accept_id, false, window, cx)
                            }),
                        ),
                    )
                    .child(
                        button(format!("suggestion-edit-{}", comment.id), "Edit").on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.accept_suggestion(&edit_id, true, window, cx)
                            }),
                        ),
                    )
                    .child(
                        button(format!("suggestion-dismiss-{}", comment.id), "Dismiss").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.dismiss_suggestion(&dismiss_id, cx)
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .when(true, crate::components::changes_body_text)
                    .line_height(crate::typography::ui_rems(crate::comments::CARD_LINE_HEIGHT))
                    .text_color(theme.text_dim)
                    .child(crate::components::code_block::inline_code(&comment.body, theme, theme.text_dim)),
            )
            .into_any_element()
    }

    fn accept_suggestion(
        &mut self,
        id: &str,
        edit: bool,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.suggestion_accept_task.is_some() {
            return;
        }
        let Some(suggestion) = self.review_suggestions.get(id).cloned() else {
            return;
        };
        let Some(target) = self.review_target.clone() else {
            return;
        };
        if !self.local_review_drafts.contains_key(&target) {
            self.local_draft_error = Some("Review drafts are still loading.".into());
            cx.notify();
            return;
        }
        let Some(first) = self
            .draft_anchor_index
            .line_hash_at(&suggestion.anchor, suggestion.anchor.start_line)
        else {
            return;
        };
        let Some(last) = self
            .draft_anchor_index
            .line_hash_at(&suggestion.anchor, suggestion.anchor.end_line)
        else {
            return;
        };
        let draft = ReviewDraft {
            id: uuid::Uuid::new_v4().to_string(),
            target,
            anchor: suggestion.anchor.clone(),
            line_hash: Some(first),
            end_line_hash: Some(last),
            body: suggestion_body(&suggestion),
            source: mast_proto::ReviewDraftSource::Agent,
            created_at: chrono::Utc::now().timestamp_millis(),
        };
        let stable = suggestion.stable_id();
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::ACCEPT_REVIEW_SUGGESTION,
                serde_json::json!({"draft": draft, "id": stable}),
                cx,
            )
        });
        self.suggestion_accept_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.suggestion_accept_task = None;
                match result {
                    Ok(_) => {
                        let target = draft.target.clone();
                        let draft_id = draft.id.clone();
                        if this.review_target.as_deref() != Some(&target)
                            && !this.local_draft_pending.contains_key(&target)
                        {
                            this.local_review_drafts.remove(&target);
                            this.local_draft_acknowledged.remove(&target);
                            this.local_draft_comments.remove(&target);
                            return;
                        }
                        this.local_review_drafts
                            .entry(target.clone())
                            .or_default()
                            .push(draft.clone());
                        this.local_draft_acknowledged
                            .entry(target.clone())
                            .or_default()
                            .push(draft);
                        if this.review_target.as_deref() == Some(&target) {
                            this.dismissed_suggestions.insert(stable);
                            this.rebuild_draft_index(cx);
                            if edit {
                                this.edit_comment(&draft_id, window, cx);
                            }
                        }
                    }
                    Err(error) => {
                        this.local_draft_error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    fn dismiss_suggestion(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(suggestion) = self.review_suggestions.get(id) else {
            return;
        };
        let Some(target) = self.review_target.clone() else {
            return;
        };
        let stable = suggestion.stable_id();
        self.dismissed_suggestions.insert(stable.clone());
        self.rebuild_suggestion_comments(cx);
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::DISMISS_REVIEW_SUGGESTION,
                serde_json::json!({"target":target, "id":stable}),
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    if this.review_target.as_deref() == Some(&target) {
                        this.dismissed_suggestions.remove(&stable);
                        this.local_draft_error = Some(error);
                        this.rebuild_suggestion_comments(cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }
    pub(super) fn is_local_draft(&self, id: &str) -> bool {
        self.local_draft_ids.contains(id)
    }

    pub(super) fn render_local_draft_card(
        &self,
        comment: &crate::comments::ReviewComment,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let edit_id = comment.id.clone();
        let remove_id = comment.id.clone();
        let stage_id = comment.id.clone();
        let button = |id: String, label: &'static str| {
            crate::components::button(SharedString::from(id), label, crate::components::ButtonVariant::Secondary,
                crate::components::ButtonSize::Sm, theme)
        };
        div()
            .w_full()
            .debug_selector(|| "review-draft-card".into())
            .h(crate::typography::ui_rems(crate::comments::card_height(&comment.body)))
            .overflow_hidden()
            .flex()
            .flex_col()
            .px(px(Theme::SPACE_LG))
            .when(true, |el| crate::components::review_card(el, theme))
            .child(
                div()
                    .h(crate::typography::ui_rems(24.0))
                    .flex()
                    .items_center()
                    .gap(px(Theme::SPACE_SM))
                    .child(
                        div().flex_1().flex().items_center().child(div().when(true, |el| crate::components::review_draft_label(el, theme)).child("Draft")),
                    )
                    .child(
                        button(format!("draft-stage-{}", comment.id), "Add to chat").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.stage_local_draft(&stage_id, cx)
                            }),
                        ),
                    )
                    .child(
                        button(format!("draft-edit-{}", comment.id), "Edit").on_click(cx.listener(
                            move |this, _, window, cx| this.edit_comment(&edit_id, window, cx),
                        )),
                    )
                    .child(
                        button(format!("draft-delete-{}", comment.id), "Delete").on_click(
                            cx.listener(move |this, _, _, cx| this.remove_comment(&remove_id, cx)),
                        ),
                    ),
            )
            .child(
                div()
                    .when(true, crate::components::changes_body_text)
                    .line_height(crate::typography::ui_rems(crate::comments::CARD_LINE_HEIGHT))
                    .text_color(theme.text_dim)
                    .child(crate::components::code_block::inline_code(&comment.body, theme, theme.text_dim)),
            )
            .into_any_element()
    }

    fn stage_local_draft(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(mut comment) = self
            .review_target
            .as_ref()
            .and_then(|target| self.local_draft_comments.get(target))
            .and_then(|comments| comments.iter().find(|comment| comment.id == id))
            .cloned()
        else {
            return;
        };
        let Some(chat) = self.owner_chat_id_owned() else {
            return;
        };
        comment.id = uuid::Uuid::new_v4().to_string();
        self.state.update(cx, |state, cx| {
            state.add_review_comment(&chat, comment);
            cx.notify();
        });
        self.sync_comment_rows(cx);
    }

    pub(super) fn load_local_drafts(&mut self, target: &str, cx: &mut Context<Self>) {
        self.outdated_limit = 20;
        if self
            .draft
            .as_ref()
            .and_then(|draft| draft.local.as_ref())
            .is_some_and(|edit| edit.draft.target != target)
        {
            self.commit_draft(cx);
        }
        self.local_draft_load_task = None;
        self.load_suggestion_dispositions(target, cx);
        self.rebuild_draft_index(cx);
        if self.local_review_drafts.contains_key(target) {
            return;
        }
        let target = target.to_owned();
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::GET_REVIEW_DRAFTS,
                serde_json::json!({"target": target}),
                cx,
            )
        });
        self.local_draft_load_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|value| {
                serde_json::from_value::<Vec<ReviewDraft>>(value).map_err(|error| error.to_string())
            });
            this.update(cx, |this, cx| {
                if this.review_target.as_deref() != Some(&target) {
                    return;
                }
                this.local_draft_load_task = None;
                match result {
                    Ok(drafts) => {
                        this.local_draft_acknowledged
                            .insert(target.clone(), drafts.clone());
                        this.local_review_drafts.entry(target).or_insert(drafts);
                        this.prune_local_draft_cache();
                        this.rebuild_draft_index(cx);
                    }
                    Err(error) => {
                        this.local_draft_error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    fn prune_local_draft_cache(&mut self) {
        while self.local_review_drafts.len() > 32 {
            let victim = self
                .local_review_drafts
                .keys()
                .find(|key| {
                    self.review_target.as_ref() != Some(*key)
                        && !self.local_draft_pending.contains_key(*key)
                })
                .cloned();
            let Some(victim) = victim else {
                break;
            };
            self.local_review_drafts.remove(&victim);
            self.local_draft_comments.remove(&victim);
            self.local_draft_acknowledged.remove(&victim);
        }
    }

    pub(super) fn retry_local_drafts(&mut self, cx: &mut Context<Self>) {
        self.local_draft_error = None;
        if !self.local_draft_pending.is_empty() {
            self.local_draft_save_ready = true;
            self.save_local_drafts(cx);
        } else if let Some(target) = self.review_target.clone() {
            self.load_local_drafts(&target, cx);
        }
    }

    pub(super) fn render_outdated_drafts(
        &self,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(target) = self.review_target.as_ref() else {
            return Vec::new();
        };
        let mut cards: Vec<AnyElement> = self
            .local_outdated_drafts
            .iter()
            .filter(|draft| &draft.target == target)
            .take(self.outdated_limit)
            .map(|draft| {
                if let Some(editor) = self.draft.as_ref().filter(|editor| {
                    editor.outdated && editor.editing_id.as_deref() == Some(draft.id.as_str())
                }) {
                    return crate::comment_ui::render_comment_draft(
                        &draft.anchor.file,
                        draft.anchor.end_line,
                        editor.input.clone(),
                        true,
                        theme,
                        cx,
                        Changes::cancel_draft,
                        Changes::commit_draft,
                        None,
                    );
                }
                let id = draft.id.clone();
                let edit_id = id.clone();
                let label = format!(
                    "Outdated · {}:{}",
                    draft.anchor.file, draft.anchor.start_line
                );
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .px(px(Theme::SPACE_LG))
                    .py(px(Theme::SPACE_SM))
                    .when(true, |el| crate::components::review_card(el, theme))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(Theme::SPACE_SM))
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "outdated-draft-edit-{edit_id}"
                                    )))
                                    .cursor_pointer()
                                    .when(true, crate::components::hunk_header)
                                    .text_color(theme.text_muted)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.edit_outdated_draft(&edit_id, window, cx)
                                    }))
                                    .child("Edit"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .when(true, crate::components::hunk_header)
                                    .text_color(theme.text_faint)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("outdated-draft-delete-{id}")))
                                    .cursor_pointer()
                                    .when(true, crate::components::hunk_header)
                                    .text_color(theme.text_muted)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_comment(&id, cx)
                                    }))
                                    .child("Delete"),
                            ),
                    )
                    .child(
                        div()
                            .when(true, crate::components::changes_body_text)
                            .text_color(theme.text_dim)
                            .child(SharedString::from(draft.body.clone())),
                    )
                    .into_any_element()
            })
            .collect();
        if self.suggestion_target.as_ref() == Some(target) {
            let remaining = self.outdated_limit.saturating_sub(cards.len());
            cards.extend(
                self.outdated_suggestions
                    .iter()
                    .take(remaining)
                    .map(|(id, body)| {
                        let id = id.clone();
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .px(px(Theme::SPACE_LG))
                            .py(px(Theme::SPACE_SM))
                            .when(true, |el| crate::components::review_card(el, theme))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(Theme::SPACE_SM))
                                    .child(
                                        div()
                                            .flex_1()
                                            .when(true, crate::components::hunk_header)
                                            .text_color(theme.text_faint)
                                            .child("Outdated · agent suggestion"),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "outdated-suggestion-dismiss-{id}"
                                            )))
                                            .cursor_pointer()
                                            .when(true, crate::components::hunk_header)
                                            .text_color(theme.text_muted)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.dismiss_suggestion(&id, cx)
                                            }))
                                            .child("Dismiss"),
                                    ),
                            )
                            .child(
                                div()
                                    .when(true, crate::components::changes_body_text)
                                    .text_color(theme.text_dim)
                                    .child(SharedString::from(body.clone())),
                            )
                            .into_any_element()
                    }),
            );
        }
        let remaining = self.outdated_limit.saturating_sub(cards.len());
        for &index in self.unplaced_review_threads.iter().take(remaining) {
            if let Some(height) = self.review_thread_index.heights.get(index) {
                let Some(thread) = self.review_threads.get(index) else {
                    continue;
                };
                let location = thread.line.map_or_else(
                    || thread.path.clone(),
                    |line| format!("{}:{line}", thread.path),
                );
                cards.push(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .when(true, crate::components::hunk_header)
                                .text_color(theme.text_faint)
                                .child(format!("Thread outside the current diff · {location}")),
                        )
                        .child(self.render_review_thread(index, *height, theme, cx))
                        .into_any_element(),
                );
            }
        }
        let total = self.local_outdated_drafts.len()
            + if self.suggestion_target.as_ref() == Some(target) {
                self.outdated_suggestions.len()
            } else {
                0
            }
            + self.unplaced_review_threads.len();
        if total > self.outdated_limit {
            cards.push(
                div()
                    .id("more-outdated-review-items")
                    .cursor_pointer()
                    .when(true, crate::components::hunk_header)
                    .text_color(theme.text_muted)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.outdated_limit = this.outdated_limit.saturating_add(20);
                        cx.notify();
                    }))
                    .child(format!(
                        "Show more outdated items ({})",
                        total - self.outdated_limit
                    ))
                    .into_any_element(),
            );
        }
        cards
    }

    fn edit_outdated_draft(&mut self, id: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(original) = self
            .local_outdated_drafts
            .iter()
            .find(|draft| draft.id == id)
            .cloned()
        else {
            return;
        };
        if self.draft.is_some() {
            self.commit_draft(cx);
        }
        let input = cx.new(|cx| super::ComposerInput::new("Edit draft…", cx));
        let events = cx.subscribe(&input, |this: &mut Changes, _, event, cx| match event {
            super::ComposerInputEvent::Submitted => this.commit_draft(cx),
            super::ComposerInputEvent::Edited => this.local_draft_edited(cx),
            _ => {}
        });
        let handle = input.read(cx).focus_handle(cx);
        let side = match original.anchor.side {
            ReviewSide::Left => CommentSide::Old,
            ReviewSide::Right => CommentSide::New,
        };
        self.draft = Some(super::CommentDraft {
            editing_id: Some(original.id.clone()),
            local: Some(LocalDraftEdit {
                draft: original.clone(),
                original: Some(original.clone()),
            }),
            outdated: true,
            key: self.owner_chat_id_owned().unwrap_or_default(),
            path: original.anchor.file.clone(),
            old_path: None,
            side,
            line: original.anchor.end_line,
            input: input.clone(),
            _events: events,
        });
        input.update(cx, |input, cx| input.set_text(original.body, cx));
        window.focus(&handle, cx);
        cx.notify();
    }

    pub(super) fn rebuild_draft_index(&mut self, cx: &mut Context<Self>) {
        let drafts = self
            .review_target
            .as_ref()
            .and_then(|target| self.local_review_drafts.get(target))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let suggestions: Vec<_> = self.review_suggestions.values().cloned().collect();
        self.draft_anchor_index = self
            .parsed
            .as_ref()
            .map(|parsed| {
                DraftAnchorIndex::new(&parsed.files, &parsed.hunk_identities, drafts, &suggestions)
            })
            .unwrap_or_default();
        self.rebuild_draft_comments(cx);
        self.rebuild_suggestion_comments(cx);
        self.rebuild_thread_placement();
    }

    pub(super) fn rebuild_draft_comments(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.review_target.clone() else {
            return;
        };
        let mut comments = Vec::new();
        self.local_draft_ids.clear();
        self.local_outdated_drafts.clear();
        for draft in self.local_review_drafts.get(&target).into_iter().flatten() {
            self.local_draft_ids.insert(draft.id.clone());
            let Some((_, line)) = self.draft_anchor_index.resolve(draft) else {
                self.local_outdated_drafts.push(draft.clone());
                continue;
            };
            let side = match draft.anchor.side {
                ReviewSide::Left => CommentSide::Old,
                ReviewSide::Right => CommentSide::New,
            };
            let mut comment =
                crate::comments::ReviewComment::new(&draft.anchor.file, side, line, &draft.body);
            comment.id = draft.id.clone();
            if let Some(old_path) = self.draft_anchor_index.old_paths.get(&draft.anchor.file) {
                comment = comment.renamed_from(Some(old_path.clone()));
            }
            comments.push(comment);
        }
        self.local_draft_comments.insert(target, comments);
        self.refresh_thread_rows(cx);
    }

    pub(super) fn new_local_draft(
        &self,
        target: &str,
        path: &str,
        side: CommentSide,
        line: u32,
    ) -> Option<ReviewDraft> {
        if !self.local_review_drafts.contains_key(target) {
            return None;
        }
        let parsed = self.parsed.as_ref()?;
        let (file_ix, file) = parsed
            .files
            .iter()
            .enumerate()
            .find(|(_, file)| file.path == path)?;
        for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
            if let Some(found) = hunk.lines.iter().find(|candidate| match side {
                CommentSide::Old => candidate.old_no == Some(line),
                CommentSide::New => candidate.new_no == Some(line),
            }) {
                return Some(ReviewDraft {
                    id: uuid::Uuid::new_v4().to_string(),
                    target: target.to_owned(),
                    body: String::new(),
                    source: mast_proto::ReviewDraftSource::User,
                    created_at: chrono::Utc::now().timestamp_millis(),
                    anchor: mast_proto::ReviewAnchor {
                        file: path.to_owned(),
                        side: match side {
                            CommentSide::Old => ReviewSide::Left,
                            CommentSide::New => ReviewSide::Right,
                        },
                        start_line: line,
                        end_line: line,
                        hunk_hash: parsed
                            .hunk_identities
                            .get(file_ix)
                            .and_then(|ids| ids.get(hunk_ix))
                            .cloned(),
                    },
                    line_hash: Some(line_hash(path, side, &found.text)),
                    end_line_hash: None,
                });
            }
        }
        None
    }

    pub(super) fn put_local_draft(
        &mut self,
        draft: ReviewDraft,
        immediate: bool,
        cx: &mut Context<Self>,
    ) {
        let target = draft.target.clone();
        let drafts = self.local_review_drafts.entry(target.clone()).or_default();
        if let Some(existing) = drafts.iter_mut().find(|item| item.id == draft.id) {
            *existing = draft;
        } else {
            drafts.push(draft);
        }
        self.queue_local_drafts(&target, immediate, cx);
    }

    pub(super) fn local_draft_edited(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.as_ref() else {
            return;
        };
        let Some(local) = draft.local.as_ref() else {
            cx.notify();
            return;
        };
        let mut value = local.draft.clone();
        value.body = draft.input.read(cx).text().to_owned();
        if value.body.trim().is_empty() {
            let target = value.target.clone();
            if let Some(drafts) = self.local_review_drafts.get_mut(&target) {
                let before = drafts.len();
                drafts.retain(|draft| draft.id != value.id);
                if drafts.len() != before {
                    self.queue_local_drafts(&target, false, cx);
                }
            }
        } else {
            self.put_local_draft(value, false, cx);
        }
        cx.notify();
    }

    pub(super) fn cancel_local_edit(&mut self, edit: LocalDraftEdit, cx: &mut Context<Self>) {
        if let Some(original) = edit.original {
            self.put_local_draft(original, true, cx);
        } else if self
            .local_review_drafts
            .get(&edit.draft.target)
            .is_some_and(|drafts| drafts.iter().any(|draft| draft.id == edit.draft.id))
        {
            self.remove_local_draft(&edit.draft.target, &edit.draft.id, cx);
        }
        self.rebuild_draft_comments(cx);
    }

    pub(super) fn remove_local_draft(&mut self, target: &str, id: &str, cx: &mut Context<Self>) {
        if let Some(drafts) = self.local_review_drafts.get_mut(target) {
            drafts.retain(|draft| draft.id != id);
        }
        self.queue_local_drafts(target, true, cx);
        self.rebuild_draft_comments(cx);
    }

    fn queue_local_drafts(&mut self, target: &str, immediate: bool, cx: &mut Context<Self>) {
        let Some(drafts) = self.local_review_drafts.get(target).cloned() else {
            return;
        };
        self.local_draft_pending.insert(target.to_owned(), drafts);
        self.local_draft_error = None;
        self.local_draft_debounce_task = None;
        if immediate {
            self.local_draft_save_ready = true;
            self.save_local_drafts(cx);
        } else {
            self.local_draft_save_ready = false;
            self.local_draft_debounce_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                this.update(cx, |this, cx| {
                    this.local_draft_debounce_task = None;
                    this.local_draft_save_ready = true;
                    this.save_local_drafts(cx);
                })
                .ok();
            }));
        }
    }

    fn save_local_drafts(&mut self, cx: &mut Context<Self>) {
        if !self.local_draft_save_ready
            || self.local_draft_save_task.is_some()
            || self.local_draft_error.is_some()
        {
            return;
        }
        let Some((target, snapshot)) = self
            .local_draft_pending
            .iter()
            .next()
            .map(|(key, value)| (key.clone(), value.clone()))
        else {
            return;
        };
        let before = self
            .local_draft_acknowledged
            .get(&target)
            .cloned()
            .unwrap_or_default();
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::SET_REVIEW_DRAFTS,
                serde_json::json!({"target": target, "before": before, "drafts": snapshot}),
                cx,
            )
        });
        self.local_draft_save_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.local_draft_save_task = None;
                match result {
                    Ok(_) => {
                        this.local_draft_acknowledged
                            .insert(target.clone(), snapshot.clone());
                        if this.local_draft_pending.get(&target) == Some(&snapshot) {
                            this.local_draft_pending.remove(&target);
                        }
                        this.save_local_drafts(cx);
                    }
                    Err(error) => {
                        this.local_draft_error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }
    pub(super) fn load_review_threads(&mut self, target: &str, cx: &mut Context<Self>) {
        self.review_threads_task = None;
        self.review_threads.clear();
        self.collapsed_review_threads.clear();
        self.unplaced_review_threads.clear();
        self.review_thread_index = ThreadIndex::default();
        self.review_threads_error = None;
        self.refresh_thread_rows(cx);
        let Some(pr) = target
            .strip_prefix("pr:")
            .and_then(mast_proto::PullRequestRef::parse_external_id)
        else {
            return;
        };
        let target = target.to_owned();
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(
                methods::GET_REVIEW_THREADS,
                serde_json::json!({"owner": pr.owner, "repo": pr.repo, "number": pr.number}),
                cx,
            )
        });
        self.review_threads_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|value| {
                serde_json::from_value::<Vec<ReviewThread>>(value)
                    .map_err(|error| error.to_string())
            });
            this.update(cx, |this, cx| {
                if this.review_target.as_deref() != Some(&target) {
                    return;
                }
                this.review_threads_task = None;
                match result {
                    Ok(threads) => {
                        this.collapsed_review_threads = threads.iter().filter(|thread| thread.resolved).map(|thread| thread.id.clone()).collect();
                        this.review_thread_index = ThreadIndex::new(&threads);
                        for (index, thread) in threads.iter().enumerate() {
                            if thread.resolved { this.review_thread_index.heights[index] = 30; }
                        }
                        this.review_threads = threads;
                        this.rebuild_thread_placement();
                        this.refresh_thread_rows(cx);
                    }
                    Err(error) => {
                        this.review_threads_error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    pub(super) fn refresh_thread_rows(&mut self, cx: &mut Context<Self>) {
        self.comment_sync_key = None;
        self.comment_key = self.comment_key.wrapping_add(1);
        self.sync_comment_rows(cx);
    }

    fn rebuild_thread_placement(&mut self) {
        let mut placed = std::collections::HashSet::new();
        if let Some(parsed) = &self.parsed {
            for file in parsed
                .files
                .iter()
                .filter(|file| self.review_thread_index.by_anchor.contains_key(&file.path))
            {
                for line in file.hunks.iter().flat_map(|hunk| &hunk.lines) {
                    if let Some(number) = line.old_no {
                        placed.extend(
                            self.review_thread_index
                                .at(&file.path, CommentSide::Old, number)
                                .iter()
                                .copied(),
                        );
                    }
                    if let Some(number) = line.new_no {
                        placed.extend(
                            self.review_thread_index
                                .at(&file.path, CommentSide::New, number)
                                .iter()
                                .copied(),
                        );
                    }
                }
            }
        }
        self.unplaced_review_threads = (0..self.review_threads.len())
            .filter(|index| !placed.contains(index))
            .collect();
    }

    pub(super) fn add_thread_rows(&self, file_ix: usize, rows: Vec<DiffRow>) -> Vec<DiffRow> {
        let Some(file) = self
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.files.get(file_ix))
        else {
            return rows;
        };
        if self.review_threads.is_empty() {
            return rows;
        }
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            result.push(row);
            let mut anchors = [None; 4];
            let mut count = 0;
            let mut append = |hunk: u32, line: u32| {
                let Some(line) = file
                    .hunks
                    .get(hunk as usize)
                    .and_then(|h| h.lines.get(line as usize))
                else {
                    return;
                };
                if line.kind != LineKind::Add
                    && let Some(n) = line.old_no
                        && let Some(slot) = anchors.get_mut(count) {
                            *slot = Some((CommentSide::Old, n));
                            count += 1;
                        }
                if line.kind != LineKind::Del
                    && let Some(n) = line.new_no
                        && let Some(slot) = anchors.get_mut(count) {
                            *slot = Some((CommentSide::New, n));
                            count += 1;
                        }
            };
            match row {
                DiffRow::Line { hunk, line, .. } => append(hunk, line),
                DiffRow::SplitLine {
                    hunk, left, right, ..
                } => {
                    if let Some(line) = left {
                        append(hunk, line);
                    }
                    if let Some(line) = right.filter(|right| Some(*right) != left) {
                        append(hunk, line);
                    }
                }
                _ => {}
            }
            for (side, line) in anchors.into_iter().flatten() {
                for &thread in self.review_thread_index.at(&file.path, side, line) {
                    let Some(&height) = self.review_thread_index.heights.get(thread) else {
                        continue;
                    };
                    result.push(DiffRow::ReviewThread {
                        file: file_ix as u32,
                        thread: thread as u32,
                        height,
                    });
                }
            }
        }
        result
    }

    pub(super) fn render_review_thread(&self, ix: usize, height: u32, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let Some(thread) = self.review_threads.get(ix) else {
            return gpui::Empty.into_any_element();
        };
        let label = match (thread.resolved, thread.outdated) {
            (true, true) => "Resolved · Outdated",
            (true, false) => "Resolved",
            (false, true) => "Outdated",
            (false, false) => "Review thread",
        };
        let collapsed = self.collapsed_review_threads.contains(&thread.id);
        let id = thread.id.clone();
        let summary = thread.comments.first().map(|comment| format!("{} · {label} · {}", comment.author, comment.body.lines().next().unwrap_or(""))).unwrap_or_else(|| label.to_owned());
        let mut card = div()
            .w_full()
            .h(px(height as f32))
            .overflow_hidden()
            .flex()
            .flex_col()
            .px(px(Theme::SPACE_LG))
            .when(true, |el| crate::components::review_card(el, theme))
            .child(
                div()
                    .id(SharedString::from(format!("review-thread-toggle-{ix}")))
                    .h(px(30.0))
                    .cursor_pointer()
                    .when(true, crate::components::hunk_header)
                    .text_color(if thread.resolved { theme.success } else if thread.outdated { theme.warning } else { theme.accent })
                    .flex().items_center().gap(px(Theme::SPACE_SM))
                    .child(div().flex_1().min_w_0().truncate().child(if collapsed { summary } else { label.to_owned() }))
                    .child(if collapsed { "Expand" } else { "Collapse" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed_review_threads.remove(&id) { this.collapsed_review_threads.insert(id.clone()); }
                        this.review_thread_index = ThreadIndex::new(&this.review_threads);
                        for (index, thread) in this.review_threads.iter().enumerate() {
                            if this.collapsed_review_threads.contains(&thread.id) { this.review_thread_index.heights[index] = 30; }
                        }
                        this.refresh_thread_rows(cx);
                    })),
            );
        for comment in thread.comments.iter().filter(|_| !collapsed) {
            card = card.child(
                div()
                    .h(px(thread_comment_height(&comment.body)))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .when(true, crate::components::hunk_header)
                            .text_color(theme.text_faint)
                            .child(SharedString::from(comment.author.clone())),
                    )
                    .child(
                        div()
                            .when(true, crate::components::changes_body_text)
                            .line_height(px(18.0))
                            .text_color(theme.text_dim)
                            .child(crate::components::code_block::inline_code(&comment.body, theme, theme.text_dim)),
                    ),
            );
        }
        card.into_any_element()
    }
}
use std::collections::HashMap;

/// Maps unchanged hunk and line content to a unique line on a new revision.
/// Ambiguous repeated lines are deliberately left outdated.
#[derive(Default)]
pub(super) struct DraftAnchorIndex {
    lines: HashMap<(String, String, CommentSide, String), Vec<u32>>,
    old_paths: HashMap<String, String>,
    positions: HashMap<(String, String, CommentSide, u32), String>,
}

fn line_hash(path: &str, side: CommentSide, text: &str) -> String {
    let tag = match side {
        CommentSide::Old => b'-',
        CommentSide::New => b'+',
    };
    mast_proto::review_hunk_identity(path, [(tag, text)])
}

impl DraftAnchorIndex {
    pub(super) fn new(
        files: &[super::FileDiff],
        identities: &[Vec<String>],
        drafts: &[ReviewDraft],
        suggestions: &[mast_proto::ReviewSuggestedComment],
    ) -> Self {
        let mut index = Self::default();
        let wanted: std::collections::HashSet<_> = drafts
            .iter()
            .map(|draft| &draft.anchor)
            .chain(suggestions.iter().map(|suggestion| &suggestion.anchor))
            .filter_map(|anchor| Some((anchor.file.as_str(), anchor.hunk_hash.as_deref()?)))
            .collect();
        for (file_ix, file) in files.iter().enumerate() {
            if let Some(old_path) = &file.old_path {
                index.old_paths.insert(file.path.clone(), old_path.clone());
            }
            for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
                let Some(hash) = identities.get(file_ix).and_then(|hunks| hunks.get(hunk_ix))
                else {
                    continue;
                };
                if !wanted.contains(&(file.path.as_str(), hash.as_str())) {
                    continue;
                }
                for line in &hunk.lines {
                    for (side, number) in [
                        (CommentSide::Old, line.old_no),
                        (CommentSide::New, line.new_no),
                    ] {
                        if let Some(number) = number {
                            index.positions.insert(
                                (file.path.clone(), hash.clone(), side, number),
                                line_hash(&file.path, side, &line.text),
                            );
                            index
                                .lines
                                .entry((
                                    file.path.clone(),
                                    hash.clone(),
                                    side,
                                    line_hash(&file.path, side, &line.text),
                                ))
                                .or_default()
                                .push(number);
                        }
                    }
                }
            }
        }
        index
    }

    pub(super) fn line_hash_at(
        &self,
        anchor: &mast_proto::ReviewAnchor,
        line: u32,
    ) -> Option<String> {
        let side = match anchor.side {
            ReviewSide::Left => CommentSide::Old,
            ReviewSide::Right => CommentSide::New,
        };
        self.positions
            .get(&(anchor.file.clone(), anchor.hunk_hash.clone()?, side, line))
            .cloned()
    }

    pub(super) fn resolve(&self, draft: &ReviewDraft) -> Option<(u32, u32)> {
        let side = match draft.anchor.side {
            ReviewSide::Left => CommentSide::Old,
            ReviewSide::Right => CommentSide::New,
        };
        let hunk = draft.anchor.hunk_hash.clone()?;
        let find = |hash: &str| {
            let lines = self.lines.get(&(
                draft.anchor.file.clone(),
                hunk.clone(),
                side,
                hash.to_owned(),
            ))?;
            (lines.len() == 1).then_some(lines[0])
        };
        let start = find(draft.line_hash.as_deref()?)?;
        let end = if draft.anchor.start_line == draft.anchor.end_line {
            start
        } else {
            find(draft.end_line_hash.as_deref()?)?
        };
        (start <= end).then_some((start, end))
    }
}

#[derive(Default)]
pub(super) struct ThreadIndex {
    by_anchor: HashMap<String, HashMap<(CommentSide, u32), Vec<usize>>>,
    heights: Vec<u32>,
}

impl ThreadIndex {
    pub(super) fn collapse(&mut self, index: usize) {
        if let Some(height) = self.heights.get_mut(index) { *height = 30; }
    }

    pub(super) fn new(threads: &[ReviewThread]) -> Self {
        let mut index = Self::default();
        for (ix, thread) in threads.iter().enumerate() {
            index.heights.push(
                (30.0
                    + thread
                        .comments
                        .iter()
                        .map(|comment| thread_comment_height(&comment.body))
                        .sum::<f32>())
                .ceil() as u32,
            );
            if thread.outdated {
                continue;
            }
            let (Some(side), Some(line)) = (thread.side, thread.line) else {
                continue;
            };
            let side = match side {
                ReviewSide::Left => CommentSide::Old,
                ReviewSide::Right => CommentSide::New,
            };
            index
                .by_anchor
                .entry(thread.path.clone())
                .or_default()
                .entry((side, line))
                .or_default()
                .push(ix);
        }
        index
    }

    pub(super) fn at(&self, path: &str, side: CommentSide, line: u32) -> &[usize] {
        self.by_anchor
            .get(path)
            .and_then(|file| file.get(&(side, line)))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ReviewCardFixture {
        changes: gpui::Entity<Changes>,
        comment: crate::comments::ReviewComment,
    }

    impl gpui::Render for ReviewCardFixture {
        fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
            self.changes.update(cx, |changes, cx| changes.render_local_draft_card(&self.comment, &Theme::dark(), cx))
        }
    }

    #[gpui::test]
    fn draft_card_height_tracks_ui_text_size(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(Theme::dark()); });
        for rem in [12.0, 16.0, 20.0] {
            let (_, vcx) = cx.add_window_view(move |window, cx| {
                window.set_rem_size(px(rem));
                let state = cx.new(|_| super::super::AppState::new());
                let changes = cx.new(|cx| Changes::new(state, cx));
                ReviewCardFixture { changes, comment: crate::comments::ReviewComment::new("a", CommentSide::New, 1, "one\ntwo") }
            });
            vcx.run_until_parked();
            let height = vcx.debug_bounds("review-draft-card").expect("review card").size.height;
            let expected = crate::comments::card_height("one\ntwo") * rem / 16.0;
            assert!((f32::from(height) - expected).abs() < 0.1);
        }
    }

    fn install_diff(changes: &mut Changes) -> String {
        use super::super::*;
        let patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
        let (files, origins, submodules, layout) = parse_diff_tree(patch, &[]);
        let identities = hunk_identities(&files);
        let identity = identities[0][0].clone();
        changes.draft_anchor_index = DraftAnchorIndex::default();
        changes.parsed = Some(ParsedDiff {
            key: "test".into(),
            truncated: false,
            additions: 1,
            deletions: 1,
            file_count: 1,
            horizontal: files.iter().map(FileHorizontalState::new).collect(),
            hunk_identities: Arc::new(identities),
            files: Arc::new(files),
            origins: Arc::new(origins),
            submodules: Arc::new(submodules),
            layout: Arc::new(layout),
        });
        identity
    }

    #[gpui::test]
    fn thread_card_is_inserted_below_its_line(cx: &mut gpui::TestAppContext) {
        use super::super::*;
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let (changes, vcx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Changes::new(state, cx)
        });
        changes.update(vcx, |changes, cx| {
            install_diff(changes);
            changes.review_threads = vec![ReviewThread {
                id: "thread".into(),
                path: "a".into(),
                line: Some(1),
                side: Some(ReviewSide::Right),
                outdated: false,
                resolved: true,
                comments: vec![mast_proto::ReviewThreadComment {
                    id: "comment".into(),
                    author: "reviewer".into(),
                    body: "Check".into(),
                }],
            }];
            changes.review_thread_index = ThreadIndex::new(&changes.review_threads);
            let file = &changes.parsed.as_ref().unwrap().files[0];
            let rows = changes.add_thread_rows(0, body_rows(0, file, &[], None, DiffMode::Unified));
            let row = rows
                .iter()
                .position(|row| matches!(row, DiffRow::ReviewThread { thread: 0, .. }))
                .unwrap();
            assert!(matches!(rows[row - 1], DiffRow::Line { .. }));
            let _card = changes.render_review_thread(0, 100, Theme::of(cx), cx);
        });
    }

    #[gpui::test]
    fn suggestion_actions_record_disposition_and_create_a_draft(cx: &mut gpui::TestAppContext) {
        use super::super::*;
        use std::sync::{Arc, Mutex};
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = calls.clone();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let (changes, vcx) = cx.add_window_view(|_, cx| {
            let mut state = AppState::new();
            state.chat_diff_rpc_override = Some(Arc::new(move |method, _| {
                captured.lock().unwrap().push(method.to_owned());
                Ok(serde_json::json!(true))
            }));
            let state = cx.new(|_| state);
            Changes::new(state, cx)
        });
        changes.update(vcx, |changes, cx| {
            let hash = install_diff(changes);
            let target = "pr:o/r#1";
            changes.review_target = Some(target.into());
            changes.scope = DiffScope::Branch;
            changes.suggestion_dispositions_target = Some(target.into());
            changes
                .local_review_drafts
                .insert(target.into(), Vec::new());
            let suggestion = |id: &str| mast_proto::ReviewSuggestedComment {
                id: Some(id.into()),
                anchor: mast_proto::ReviewAnchor {
                    file: "a".into(),
                    side: ReviewSide::Right,
                    start_line: 1,
                    end_line: 1,
                    hunk_hash: Some(hash.clone()),
                },
                body: "Check".into(),
                severity: "low".into(),
                suggestion: None,
            };
            changes.set_review_suggestions(
                target,
                "head",
                &[suggestion("one"), suggestion("two")],
                cx,
            );
            changes.refresh_suggestion_head(Some("other-head".into()), cx);
            assert!(changes.suggestion_comments.is_empty());
            assert_eq!(changes.outdated_suggestions.len(), 2);
            changes.refresh_suggestion_head(Some("head".into()), cx);
            assert_eq!(changes.suggestion_comments.len(), 2);
            changes.dismiss_suggestion("suggestion:one", cx);
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            changes.update(cx, |changes, cx| {
                changes.accept_suggestion("suggestion:two", true, window, cx)
            })
        });
        vcx.run_until_parked();
        changes.read_with(vcx, |changes, _| {
            assert_eq!(changes.suggestion_comments.len(), 0);
            assert_eq!(changes.local_review_drafts["pr:o/r#1"].len(), 1);
            assert_eq!(
                changes.local_review_drafts["pr:o/r#1"][0].source,
                mast_proto::ReviewDraftSource::Agent
            );
            assert!(
                changes
                    .draft
                    .as_ref()
                    .is_some_and(|draft| draft.local.is_some())
            );
        });
        let calls = calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|method| method == methods::DISMISS_REVIEW_SUGGESTION)
        );
        assert!(
            calls
                .iter()
                .any(|method| method == methods::ACCEPT_REVIEW_SUGGESTION)
        );
    }
    #[gpui::test]
    fn typing_coalesces_registry_writes(cx: &mut gpui::TestAppContext) {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let writes = Arc::new(AtomicUsize::new(0));
        let captured = writes.clone();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let (changes, vcx) = cx.add_window_view(|_, cx| {
            let mut state = super::super::AppState::new();
            state.chat_diff_rpc_override = Some(Arc::new(move |method, _| {
                if method == methods::SET_REVIEW_DRAFTS {
                    captured.fetch_add(1, Ordering::SeqCst);
                }
                Ok(serde_json::json!(true))
            }));
            let state = cx.new(|_| state);
            Changes::new(state, cx)
        });
        changes.update(vcx, |changes, cx| {
            let input = cx.new(|cx| super::super::ComposerInput::new("Draft", cx));
            let events = cx.subscribe(&input, |this: &mut Changes, _, event, cx| {
                if matches!(event, super::super::ComposerInputEvent::Edited) {
                    this.local_draft_edited(cx);
                }
            });
            let draft = ReviewDraft {
                id: "one".into(),
                target: "pr:o/r#1".into(),
                body: String::new(),
                source: mast_proto::ReviewDraftSource::User,
                created_at: 1,
                anchor: mast_proto::ReviewAnchor {
                    file: "a".into(),
                    side: ReviewSide::Right,
                    start_line: 1,
                    end_line: 1,
                    hunk_hash: Some("h".into()),
                },
                line_hash: Some("l".into()),
                end_line_hash: None,
            };
            changes.draft = Some(super::super::CommentDraft {
                editing_id: None,
                outdated: false,
                local: Some(LocalDraftEdit {
                    draft,
                    original: None,
                }),
                key: "chat".into(),
                path: "a".into(),
                old_path: None,
                side: CommentSide::New,
                line: 1,
                input: input.clone(),
                _events: events,
            });
            for n in 0..100 {
                input.update(cx, |input, cx| input.set_text(format!("typed {n}"), cx));
            }
        });
        vcx.run_until_parked();
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        vcx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        vcx.run_until_parked();
        assert_eq!(writes.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn draft_reanchors_only_to_a_unique_matching_line() {
        let moved = super::super::parse_patch(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -10 +10 @@\n-old\n+new\n",
        );
        let identities = super::super::hunk_identities(&moved);
        let draft = ReviewDraft {
            id: "one".into(),
            target: "pr:o/r#1".into(),
            body: "Check".into(),
            source: mast_proto::ReviewDraftSource::User,
            created_at: 1,
            anchor: mast_proto::ReviewAnchor {
                file: "a".into(),
                side: ReviewSide::Right,
                start_line: 1,
                end_line: 1,
                hunk_hash: Some(identities[0][0].clone()),
            },
            line_hash: Some(line_hash("a", CommentSide::New, "new")),
            end_line_hash: None,
        };
        assert_eq!(
            DraftAnchorIndex::new(&moved, &identities, std::slice::from_ref(&draft), &[]).resolve(&draft),
            Some((10, 10))
        );
        let changed = super::super::parse_patch(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -10 +10 @@\n-old\n+different\n",
        );
        assert_eq!(
            DraftAnchorIndex::new(
                &changed,
                &super::super::hunk_identities(&changed),
                std::slice::from_ref(&draft),
                &[]
            )
            .resolve(&draft),
            None
        );
    }
    #[test]
    fn many_threads_have_direct_anchor_lookups() {
        let threads: Vec<_> = (0..5000)
            .map(|i| ReviewThread {
                id: i.to_string(),
                path: "src/a.rs".into(),
                line: Some(i + 1),
                side: Some(ReviewSide::Right),
                outdated: false,
                resolved: false,
                comments: vec![],
            })
            .collect();
        let index = ThreadIndex::new(&threads);
        assert_eq!(index.by_anchor["src/a.rs"].len(), 5000);
        for i in 0..5000 {
            assert_eq!(index.at("src/a.rs", CommentSide::New, i + 1), &[i as usize]);
        }
        assert!(index.at("src/other.rs", CommentSide::New, 1).is_empty());
    }
}
