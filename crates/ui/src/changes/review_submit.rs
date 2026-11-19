//! "Submit review…": the one human action that posts to GitHub. A small
//! popover picks Comment / Approve / Request changes, edits the summary, shows
//! what will be sent, and posts everything as one batched review.
//!
//! Only the button in this file starts a post. Drafts whose lines cannot be
//! placed on the PR's current diff are listed and stay local drafts.
use super::*;
use crate::comments::CommentSide;
use crate::composer::ComposerInput;
use mast_proto::{
    ReviewDraft, ReviewEvent, ReviewSide, SubmitReviewComment, SubmitReviewOutcome,
    SubmitReviewParams, SubmitReviewReply,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SubmitPhase {
    Editing,
    Sending,
    Posted { url: String, already: bool },
    /// A pending review from the browser blocks a second one.
    Conflict { url: String },
    Failed(String),
}

pub(super) struct SubmitDialog {
    pub(super) target: String,
    pub(super) event: ReviewEvent,
    pub(super) input: Entity<ComposerInput>,
    pub(super) comments: Vec<SubmitReviewComment>,
    pub(super) unplaced: Vec<ReviewDraft>,
    pub(super) commit_id: Option<String>,
    pub(super) phase: SubmitPhase,
    pub(super) task: Option<Task<()>>,
}

fn event_label(event: ReviewEvent) -> &'static str {
    match event {
        ReviewEvent::Comment => "Comment",
        ReviewEvent::Approve => "Approve",
        ReviewEvent::RequestChanges => "Request changes",
    }
}

/// Stable for one submission's content so a retry after a lost response finds
/// the review it already posted, and different content never matches it.
fn attempt_id(nonce: &str, event: ReviewEvent, body: &str, comments: &[SubmitReviewComment]) -> String {
    let mut seed = format!("{nonce}\n{event:?}\n{}", body.trim());
    for comment in comments {
        seed.push_str(&format!(
            "\n{}|{}|{}|{:?}|{:?}|{}",
            comment.draft_id, comment.path, comment.line, comment.side, comment.start_line, comment.body
        ));
    }
    mast_proto::derive_id(&seed, "mast-review-attempt")
}

fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let mut out: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        out.push('…');
    }
    out
}

impl Changes {
    fn submit_pull_request(&self) -> Option<mast_proto::PullRequestRef> {
        self.review_target
            .as_deref()?
            .strip_prefix("pr:")
            .and_then(mast_proto::PullRequestRef::parse_external_id)
    }

    /// Whether the chrome offers "Submit review…" (a PR target with local drafts loaded).
    pub(super) fn can_submit_review(&self) -> bool {
        self.submit_pull_request().is_some()
            && self
                .review_target
                .as_ref()
                .is_some_and(|target| self.local_review_drafts.contains_key(target))
    }

    pub(super) fn open_submit_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.review_target.clone() else {
            return;
        };
        if self.submit_pull_request().is_none() {
            return;
        }
        let drafts = self.local_review_drafts.get(&target).cloned().unwrap_or_default();
        let (placed, unplaced) = self.draft_anchor_index.partition_drafts(&drafts);
        let comments = placed
            .into_iter()
            .map(|(draft, (start, end))| SubmitReviewComment {
                draft_id: draft.id.clone(),
                path: draft.anchor.file.clone(),
                line: end,
                side: draft.anchor.side,
                start_line: (start < end).then_some(start),
                start_side: (start < end).then_some(draft.anchor.side),
                body: draft.body.clone(),
            })
            .filter(|comment| !comment.body.trim().is_empty())
            .collect();
        let unplaced = unplaced.into_iter().cloned().collect();
        let commit_id = self.review_identity(cx).and_then(|(_, head)| head);
        let input = cx.new(|cx| ComposerInput::new("Summary (optional)", cx));
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.submit_dialog = Some(SubmitDialog {
            target,
            event: ReviewEvent::Comment,
            input,
            comments,
            unplaced,
            commit_id,
            phase: SubmitPhase::Editing,
            task: None,
        });
        cx.notify();
    }

    pub(super) fn close_submit_review(&mut self, cx: &mut Context<Self>) {
        // A post in flight keeps running; its reply is applied only to the dialog that sent it.
        self.submit_dialog = None;
        cx.notify();
    }

    fn set_submit_event(&mut self, event: ReviewEvent, cx: &mut Context<Self>) {
        if let Some(dialog) = self.submit_dialog.as_mut()
            && matches!(dialog.phase, SubmitPhase::Editing | SubmitPhase::Failed(_))
        {
            dialog.event = event;
            cx.notify();
        }
    }

    /// The click handler of the dialog's primary button: the only call site of `SUBMIT_REVIEW`.
    pub(super) fn submit_review_now(&mut self, cx: &mut Context<Self>) {
        let Some(pr) = self.submit_pull_request() else {
            return;
        };
        let Some(dialog) = self.submit_dialog.as_mut() else {
            return;
        };
        if matches!(dialog.phase, SubmitPhase::Sending | SubmitPhase::Posted { .. }) {
            return;
        }
        let body = dialog.input.read(cx).text().to_owned();
        if dialog.comments.is_empty() && body.trim().is_empty() {
            dialog.phase = SubmitPhase::Failed("Add a summary or at least one comment first.".into());
            cx.notify();
            return;
        }
        if self.local_draft_pending.contains_key(&dialog.target) {
            dialog.phase = SubmitPhase::Failed("Drafts are still saving. Try again in a moment.".into());
            cx.notify();
            return;
        }
        let target = dialog.target.clone();
        let nonce = self
            .submit_nonces
            .entry(target.clone())
            .or_insert_with(|| uuid::Uuid::new_v4().to_string())
            .clone();
        let params = SubmitReviewParams {
            target: target.clone(),
            owner: pr.owner.clone(),
            repo: pr.repo.clone(),
            number: pr.number,
            event: dialog.event,
            attempt_id: attempt_id(&nonce, dialog.event, &body, &dialog.comments),
            body,
            comments: dialog.comments.clone(),
            commit_id: dialog.commit_id.clone(),
        };
        dialog.phase = SubmitPhase::Sending;
        let value = serde_json::to_value(&params).unwrap_or_default();
        let task = self.state.update(cx, |state, cx| {
            state.chat_diff_rpc(methods::SUBMIT_REVIEW, value, cx)
        });
        let task_target = target.clone();
        dialog.task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|value| {
                serde_json::from_value::<SubmitReviewReply>(value).map_err(|e| e.to_string())
            });
            this.update(cx, |this, cx| this.finish_submit_review(&task_target, result, cx)).ok();
        }));
        cx.notify();
    }

    fn finish_submit_review(
        &mut self,
        target: &str,
        result: Result<SubmitReviewReply, String>,
        cx: &mut Context<Self>,
    ) {
        let phase = match &result {
            Ok(reply) => match reply.outcome {
                SubmitReviewOutcome::Posted | SubmitReviewOutcome::AlreadyPosted => SubmitPhase::Posted {
                    url: reply.url.clone(),
                    already: reply.outcome == SubmitReviewOutcome::AlreadyPosted,
                },
                SubmitReviewOutcome::PendingConflict => SubmitPhase::Conflict { url: reply.url.clone() },
            },
            Err(error) => SubmitPhase::Failed(error.clone()),
        };
        let posted = matches!(phase, SubmitPhase::Posted { .. });
        if posted {
            self.submit_nonces.remove(target);
            // The engine recorded the review and removed its drafts; reload both views.
            self.local_review_drafts.remove(target);
            self.local_draft_acknowledged.remove(target);
            self.local_draft_comments.remove(target);
        }
        if let Some(dialog) = self.submit_dialog.as_mut().filter(|d| d.target == target) {
            dialog.phase = phase;
            dialog.task = None;
        }
        if posted && self.review_target.as_deref() == Some(target) {
            self.load_local_drafts(target, cx);
            self.load_review_threads(target, cx);
        }
        cx.notify();
    }

    pub(super) fn render_submit_review(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.can_submit_review() && self.submit_dialog.is_none() {
            return None;
        }
        let trigger = div()
            .id("changes-submit-review-trigger")
            .debug_selector(|| "changes-submit-review-trigger".into())
            .flex_none()
            .h(px(20.0))
            .px(px(6.0))
            .flex()
            .items_center()
            .cursor_pointer()
            .text_size(px(11.0))
            .text_color(theme.text_muted)
            .when(true, |el| crate::components::review_text_action(el, theme))
            .occlude()
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.submit_dialog.is_some() {
                    this.close_submit_review(cx);
                } else {
                    this.open_submit_review(window, cx);
                }
            }))
            .child(SharedString::from("Submit review\u{2026}"));
        if self.submit_dialog.is_none() {
            return Some(trigger.into_any_element());
        }
        let card = self.render_submit_card(&theme.for_popup(), cx);
        Some(
            trigger
                .relative()
                .child(crate::popover::anchored_menu_below_end("changes-submit-review-menu", card, None))
                .into_any_element(),
        )
    }

    fn render_submit_card(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        use crate::components::{ButtonSize, ButtonVariant, button};
        let Some(dialog) = self.submit_dialog.as_ref() else {
            return div().into_any_element();
        };
        let count = dialog.comments.len();
        let busy = dialog.phase == SubmitPhase::Sending;
        let finished = matches!(dialog.phase, SubmitPhase::Posted { .. });
        let events = [ReviewEvent::Comment, ReviewEvent::Approve, ReviewEvent::RequestChanges];
        let segments = events.into_iter().map(|event| {
            let selected = dialog.event == event;
            let item = div()
                .id(SharedString::from(format!("submit-review-event-{event:?}")))
                .px(px(8.0))
                .py(px(3.0))
                .text_size(px(11.0))
                .cursor_pointer()
                .text_color(if selected { theme.text } else { theme.text_muted })
                .on_click(cx.listener(move |this, _, _, cx| this.set_submit_event(event, cx)))
                .child(SharedString::from(event_label(event)));
            crate::surface_chrome::segmented_key(item, theme, selected)
        });
        let unplaced: Vec<_> = dialog
            .unplaced
            .iter()
            .take(5)
            .map(|draft| {
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_muted)
                    .truncate()
                    .child(SharedString::from(format!(
                        "{}:{} \u{2014} {}",
                        draft.anchor.file,
                        draft.anchor.end_line,
                        one_line(&draft.body)
                    )))
            })
            .collect();
        let hidden = dialog.unplaced.len().saturating_sub(5);
        let status: Option<AnyElement> = match &dialog.phase {
            SubmitPhase::Editing | SubmitPhase::Sending => None,
            SubmitPhase::Failed(message) => Some(
                crate::error_notice::ErrorNotice::from_text("submit-review-error", theme, message)
                    .into_any_element(),
            ),
            SubmitPhase::Conflict { url } => {
                let url = url.clone();
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(
                            div().text_size(px(11.0)).text_color(theme.warning).child(
                                "You already have a pending review on this pull request (started on GitHub). Submit or discard it there, then try again. Nothing was sent.",
                            ),
                        )
                        .child(
                            button("submit-review-open-pr", "Open pull request", ButtonVariant::Secondary, ButtonSize::Sm, theme)
                                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                        )
                        .into_any_element(),
                )
            }
            SubmitPhase::Posted { url, already } => {
                let url = url.clone();
                let text = if *already {
                    "This review was already on GitHub; nothing was sent again."
                } else {
                    "Review posted."
                };
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(div().text_size(px(11.5)).text_color(theme.text).child(text))
                        .child(
                            button("submit-review-open-review", "Open on GitHub", ButtonVariant::Secondary, ButtonSize::Sm, theme)
                                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                        )
                        .into_any_element(),
                )
            }
        };
        let primary_label = match (&dialog.phase, busy) {
            (_, true) => "Submitting…",
            (SubmitPhase::Failed(_), _) => "Retry",
            _ => "Submit",
        };
        let card = div()
            .id("changes-submit-review")
            .debug_selector(|| "changes-submit-review".into())
            .w(px(380.0))
            .p(px(Theme::SPACE_LG))
            .flex()
            .flex_col()
            .gap(px(Theme::SPACE_MD))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_submit_review(cx);
                }
            }))
            .child(
                div()
                    .text_size(px(12.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child("Submit review"),
            )
            .child(div().flex().flex_row().gap(px(4.0)).children(segments))
            .child(
                div()
                    .h(px(84.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .when(true, |el| crate::components::file_comment_input(el, theme))
                    .child(dialog.input.clone()),
            )
            .child(
                div()
                    .id("submit-review-count")
                    .text_size(px(11.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(match count {
                        0 => "No draft comments will be posted".to_owned(),
                        1 => "1 draft comment will be posted".to_owned(),
                        n => format!("{n} draft comments will be posted"),
                    })),
            )
            .when(!dialog.unplaced.is_empty(), |el| {
                el.child(
                    div()
                        .id("submit-review-unplaced")
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.warning)
                                .child(SharedString::from(format!(
                                    "{} can\u{2019}t be placed on the current diff. They stay as local drafts and are not posted:",
                                    dialog.unplaced.len()
                                ))),
                        )
                        .children(unplaced)
                        .when(hidden > 0, |el| {
                            el.child(
                                div().text_size(px(11.0)).text_color(theme.text_faint).child(SharedString::from(format!("and {hidden} more"))),
                            )
                        }),
                )
            })
            .when_some(status, |el, status| el.child(status))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(6.0))
                    .child(
                        button(
                            "submit-review-cancel",
                            if finished { "Done" } else { "Cancel" },
                            ButtonVariant::Ghost,
                            ButtonSize::Sm,
                            theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.close_submit_review(cx))),
                    )
                    .when(!finished && !matches!(dialog.phase, SubmitPhase::Conflict { .. }), |el| {
                        el.child(
                            button("submit-review-send", primary_label, ButtonVariant::Primary, ButtonSize::Sm, theme)
                                .when(busy, |el| el.opacity(0.6))
                                .on_click(cx.listener(|this, _, _, cx| this.submit_review_now(cx))),
                        )
                    }),
            );
        card.into_any_element()
    }
}

#[allow(dead_code)]
fn _side(side: ReviewSide) -> CommentSide {
    match side {
        ReviewSide::Left => CommentSide::Old,
        ReviewSide::Right => CommentSide::New,
    }
}

#[cfg(test)]
mod tests {
    use super::super::review_drafts::{line_hash, tests::install_diff};
    use super::*;
    use std::sync::{Arc, Mutex};

    type Calls = Arc<Mutex<Vec<(String, serde_json::Value)>>>;
    const TARGET: &str = "pr:o/r#1";

    fn draft(id: &str, hunk: &str, line_hash: &str) -> ReviewDraft {
        ReviewDraft {
            id: id.into(),
            target: TARGET.into(),
            body: format!("body {id}"),
            source: mast_proto::ReviewDraftSource::User,
            created_at: 1,
            anchor: mast_proto::ReviewAnchor {
                file: "a".into(),
                side: ReviewSide::Right,
                start_line: 1,
                end_line: 1,
                hunk_hash: Some(hunk.into()),
            },
            line_hash: Some(line_hash.into()),
            end_line_hash: None,
        }
    }

    /// A window over a PR review with one placeable and one unplaceable draft;
    /// `reply` scripts the SubmitReview response.
    fn setup<'a>(
        cx: &'a mut gpui::TestAppContext,
        reply: impl Fn() -> Result<serde_json::Value, String> + Send + Sync + 'static,
    ) -> (gpui::Entity<Changes>, &'a mut gpui::VisualTestContext, Calls) {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let captured = calls.clone();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let (changes, vcx) = cx.add_window_view(|_, cx| {
            let mut state = AppState::new();
            state.chat_diff_rpc_override = Some(Arc::new(move |method, params| {
                captured.lock().unwrap().push((method.to_owned(), params));
                match method {
                    m if m == methods::SUBMIT_REVIEW => reply(),
                    m if m == methods::GET_REVIEW_DRAFTS => Ok(serde_json::json!([])),
                    m if m == methods::GET_REVIEW_THREADS => Ok(serde_json::json!([])),
                    _ => Ok(serde_json::json!(true)),
                }
            }));
            let state = cx.new(|_| state);
            Changes::new(state, cx)
        });
        changes.update(vcx, |changes, cx| {
            let hunk = install_diff(changes);
            changes.review_target = Some(TARGET.into());
            changes.scope = DiffScope::Branch;
            let placed = draft("placed", &hunk, &line_hash("a", CommentSide::New, "new"));
            let lost = draft("lost", &hunk, "missing");
            changes.local_review_drafts.insert(TARGET.into(), vec![placed, lost]);
            changes.rebuild_draft_index(cx);
        });
        (changes, vcx, calls)
    }

    fn open(changes: &gpui::Entity<Changes>, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| changes.update(cx, |c, cx| c.open_submit_review(window, cx)));
    }

    fn submit_calls(calls: &Calls) -> Vec<serde_json::Value> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == methods::SUBMIT_REVIEW)
            .map(|(_, params)| params.clone())
            .collect()
    }

    fn posted_reply() -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({"outcome":"posted","reviewId":9,"url":"https://github.com/o/r/pull/1#pullrequestreview-9"}))
    }

    #[gpui::test]
    fn dialog_counts_placed_drafts_and_lists_unplaced_ones(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, calls) = setup(cx, posted_reply);
        assert!(changes.read_with(&*vcx, |c, _| c.can_submit_review()));
        open(&changes, vcx);
        changes.update(vcx, |changes, cx| {
            let dialog = changes.submit_dialog.as_ref().unwrap();
            assert_eq!(dialog.comments.len(), 1);
            assert_eq!(dialog.comments[0].draft_id, "placed");
            assert_eq!(dialog.comments[0].line, 1);
            assert_eq!(dialog.unplaced.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["lost"]);
            // Rendering the trigger with the dialog open must not post anything.
            let theme = Theme::of(cx).clone();
            let _ = changes.render_submit_review(&theme, cx);
        });
        assert!(submit_calls(&calls).is_empty(), "opening the dialog must not post");
    }

    #[gpui::test]
    fn submit_posts_once_clears_sent_drafts_and_keeps_unplaced(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, calls) = setup(cx, posted_reply);
        open(&changes, vcx);
        changes.update(vcx, |changes, cx| {
            changes.set_submit_event(ReviewEvent::RequestChanges, cx);
            changes.submit_dialog.as_ref().unwrap().input.update(cx, |input, cx| input.set_text("Please fix", cx));
            changes.submit_review_now(cx);
        });
        vcx.run_until_parked();
        let sent = submit_calls(&calls);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["event"], "request-changes");
        assert_eq!(sent[0]["body"], "Please fix");
        assert_eq!(sent[0]["comments"].as_array().unwrap().len(), 1);
        assert_eq!(sent[0]["comments"][0]["draftId"], "placed");
        changes.read_with(&*vcx, |c, _| {
            let dialog = c.submit_dialog.as_ref().unwrap();
            assert!(matches!(&dialog.phase, SubmitPhase::Posted { already: false, url } if url.ends_with("-9")));
            assert!(c.submit_nonces.is_empty());
        });
        let methods_called: Vec<_> = calls.lock().unwrap().iter().map(|(m, _)| m.clone()).collect();
        assert!(methods_called.iter().any(|m| m == methods::GET_REVIEW_DRAFTS), "drafts reload");
        assert!(methods_called.iter().any(|m| m == methods::GET_REVIEW_THREADS), "threads refresh");
        // A second click while Posted does nothing.
        changes.update(vcx, |c, cx| c.submit_review_now(cx));
        assert_eq!(submit_calls(&calls).len(), 1);
    }

    #[gpui::test]
    fn failure_keeps_drafts_and_retry_reuses_the_attempt_id(cx: &mut gpui::TestAppContext) {
        let attempts = Arc::new(Mutex::new(0));
        let counter = attempts.clone();
        let (changes, vcx, calls) = setup(cx, move || {
            let mut n = counter.lock().unwrap();
            *n += 1;
            if *n == 1 { Err("could not run gh: connection reset".into()) } else { posted_reply() }
        });
        open(&changes, vcx);
        changes.update(vcx, |c, cx| c.submit_review_now(cx));
        vcx.run_until_parked();
        changes.read_with(&*vcx, |c, _| {
            assert!(matches!(&c.submit_dialog.as_ref().unwrap().phase, SubmitPhase::Failed(m) if m.contains("connection reset")));
            assert_eq!(c.local_review_drafts[TARGET].len(), 2, "nothing is lost");
            assert_eq!(c.submit_nonces.len(), 1);
        });
        changes.update(vcx, |c, cx| c.submit_review_now(cx));
        vcx.run_until_parked();
        let sent = submit_calls(&calls);
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0]["attemptId"], sent[1]["attemptId"], "retry must be idempotent on GitHub");
        changes.read_with(&*vcx, |c, _| {
            assert!(matches!(c.submit_dialog.as_ref().unwrap().phase, SubmitPhase::Posted { .. }));
        });
    }

    #[gpui::test]
    fn pending_review_conflict_keeps_everything_and_offers_the_pr(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, _calls) = setup(cx, || {
            Ok(serde_json::json!({"outcome":"pending-conflict","url":"https://github.com/o/r/pull/1"}))
        });
        open(&changes, vcx);
        changes.update(vcx, |c, cx| c.submit_review_now(cx));
        vcx.run_until_parked();
        changes.update(vcx, |c, cx| {
            assert_eq!(
                c.submit_dialog.as_ref().unwrap().phase,
                SubmitPhase::Conflict { url: "https://github.com/o/r/pull/1".into() }
            );
            assert_eq!(c.local_review_drafts[TARGET].len(), 2);
            let theme = Theme::of(cx).clone();
            let _ = c.render_submit_review(&theme, cx);
        });
    }

    #[gpui::test]
    fn empty_review_is_refused_without_a_call(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, calls) = setup(cx, posted_reply);
        changes.update(vcx, |changes, _| {
            changes.local_review_drafts.insert(TARGET.into(), Vec::new());
        });
        open(&changes, vcx);
        changes.update(vcx, |c, cx| c.submit_review_now(cx));
        assert!(submit_calls(&calls).is_empty());
        changes.read_with(&*vcx, |c, _| {
            assert!(matches!(c.submit_dialog.as_ref().unwrap().phase, SubmitPhase::Failed(_)));
        });
    }
}
