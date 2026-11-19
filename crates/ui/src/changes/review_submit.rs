//! "Submit review…": the one human action that posts to GitHub. A small
//! popover picks Comment / Approve / Request changes, edits the summary, shows
//! what will be sent, and posts everything as one batched review.
//!
//! Lines are checked against GitHub's own diff at the PR's head (never the
//! working tree) before anything can be sent. Drafts that cannot be placed there
//! are listed and stay local. The dialog pins the PR and head it opened on, the
//! in-flight request lives on `Changes` (not the dialog), and an unknown result
//! is checked against GitHub before another post is allowed.
use super::*;
use crate::composer::ComposerInput;
use mast_proto::{
    PrepareReviewParams, PrepareReviewReply, ReviewDraft, ReviewEvent, ReviewSide,
    SubmitReviewComment, SubmitReviewOutcome, SubmitReviewParams, SubmitReviewReply,
};
use std::collections::{HashMap as Map, HashSet as Set};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SubmitPhase {
    /// Checking the drafts against GitHub's diff at the PR head.
    Preparing,
    Editing,
    Sending,
    /// A post ended without a clear answer; asking GitHub whether it landed.
    Checking,
    Posted { url: String, already: bool, warning: Option<String> },
    /// A pending review from the browser blocks a second one.
    Conflict { url: String },
    /// `reprepare`: the PR's head is unknown or moved, so the check must run again.
    Failed { message: String, reprepare: bool },
}

pub(super) struct SubmitDialog {
    /// Pinned when the dialog opens: Submit and Retry use only these.
    pub(super) target: String,
    pub(super) pr: mast_proto::PullRequestRef,
    pub(super) event: ReviewEvent,
    pub(super) input: Entity<ComposerInput>,
    /// The PR's head on GitHub the drafts were checked against; the review is pinned to it.
    pub(super) head_sha: Option<String>,
    /// Draft id -> (end line, start line) as checked at prepare time.
    pub(super) prepared: Map<String, (u32, Option<u32>)>,
    /// Drafts GitHub's diff can place, and why it can't place the rest.
    pub(super) validated: Set<String>,
    pub(super) rejected: Map<String, String>,
    /// The local checkout is not at the PR's head (unpushed commits).
    pub(super) local_ahead: bool,
    pub(super) last_params: Option<SubmitReviewParams>,
    pub(super) phase: SubmitPhase,
}

fn event_label(event: ReviewEvent) -> &'static str {
    match event {
        ReviewEvent::Comment => "Comment",
        ReviewEvent::Approve => "Approve",
        ReviewEvent::RequestChanges => "Request changes",
    }
}

fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let mut out: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        out.push('…');
    }
    out
}

/// Why GitHub's diff at the PR head cannot place a draft the local diff can.
pub(super) fn unplaced_label(local_head: Option<&str>, pr_head: &str) -> &'static str {
    match local_head {
        Some(local) if !local.eq_ignore_ascii_case(pr_head) => "not pushed",
        _ => "uncommitted or changed locally",
    }
}

/// A failed post is "unknown" unless GitHub (or Mast) clearly said nothing was sent.
fn result_unknown(error: &str) -> bool {
    const DEFINITE: [&str; 6] = [
        "HTTP 4",
        "The pull request has new commits",
        "Already posting",
        "Add a summary",
        "only available from the Mast app",
        "head commit is unknown",
    ];
    !DEFINITE.iter().any(|marker| error.contains(marker))
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

    fn local_review_head(&self, cx: &App) -> Option<String> {
        self.review_identity(cx).and_then(|(_, head)| head)
    }

    fn diff_line_text(&self, path: &str, side: ReviewSide, line: u32) -> String {
        let Some(parsed) = self.parsed.as_ref() else {
            return String::new();
        };
        parsed
            .files
            .iter()
            .filter(|file| file.path == path)
            .flat_map(|file| file.hunks.iter().flat_map(|hunk| hunk.lines.iter()))
            .find(|candidate| match side {
                ReviewSide::Left => candidate.old_no == Some(line),
                ReviewSide::Right => candidate.new_no == Some(line),
            })
            .map(|found| found.text.clone())
            .unwrap_or_default()
    }

    fn submit_comment(&self, draft: &ReviewDraft, start: u32, end: u32, with_text: bool) -> SubmitReviewComment {
        let range = start < end;
        let text = |line| {
            if with_text { self.diff_line_text(&draft.anchor.file, draft.anchor.side, line) } else { String::new() }
        };
        SubmitReviewComment {
            draft_id: draft.id.clone(),
            path: draft.anchor.file.clone(),
            line: end,
            side: draft.anchor.side,
            start_line: range.then_some(start),
            start_side: range.then_some(draft.anchor.side),
            body: draft.body.clone(),
            line_text: text(end),
            start_text: if range { text(start) } else { String::new() },
        }
    }

    /// What would be sent right now: the drafts that exist now, with their current
    /// bodies, that GitHub's diff placed at the position they still have. The rest are
    /// held back with a reason. `with_text` is only needed when building a request.
    pub(super) fn submit_selection(
        &self,
        dialog: &SubmitDialog,
        with_text: bool,
    ) -> (Vec<SubmitReviewComment>, Vec<(ReviewDraft, String)>) {
        let drafts = self.local_review_drafts.get(&dialog.target).map(Vec::as_slice).unwrap_or_default();
        let (placed, unplaced) = self.draft_anchor_index.partition_drafts(drafts);
        let mut comments = Vec::new();
        let mut held = Vec::new();
        for draft in unplaced.into_iter().filter(|d| !d.body.trim().is_empty()) {
            held.push((draft.clone(), "its line is no longer in the diff".to_owned()));
        }
        for (draft, (start, end)) in placed.into_iter().filter(|(d, _)| !d.body.trim().is_empty()) {
            let range = (end, (start < end).then_some(start));
            let reason = match dialog.prepared.get(&draft.id) {
                None => Some("added after the check; reopen to include it".to_owned()),
                Some(checked) if *checked != range => Some("moved since the check; reopen to include it".to_owned()),
                Some(_) => dialog.rejected.get(&draft.id).cloned().or_else(|| {
                    (!dialog.validated.contains(&draft.id)).then(|| "not checked".to_owned())
                }),
            };
            match reason {
                Some(reason) => held.push((draft.clone(), reason)),
                None => comments.push(self.submit_comment(draft, start, end, with_text)),
            }
        }
        (comments, held)
    }

    pub(super) fn open_submit_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submit_dialog.is_some() {
            return;
        }
        let Some(target) = self.review_target.clone() else {
            return;
        };
        let Some(pr) = self.submit_pull_request() else {
            return;
        };
        let input = cx.new(|cx| ComposerInput::new("Summary (optional)", cx));
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        let resuming = self.submit_inflight.as_deref() == Some(target.as_str());
        self.submit_dialog = Some(SubmitDialog {
            target,
            pr,
            event: ReviewEvent::Comment,
            input,
            head_sha: None,
            prepared: Map::new(),
            validated: Set::new(),
            rejected: Map::new(),
            local_ahead: false,
            last_params: None,
            // A request from an earlier open of this dialog is still running: show it, don't repeat it.
            phase: if resuming { SubmitPhase::Sending } else { SubmitPhase::Preparing },
        });
        if !resuming {
            self.prepare_submit(cx);
        }
        cx.notify();
    }

    fn busy(&self) -> bool {
        self.submit_dialog
            .as_ref()
            .is_some_and(|d| matches!(d.phase, SubmitPhase::Sending | SubmitPhase::Checking))
    }

    pub(super) fn close_submit_review(&mut self, cx: &mut Context<Self>) {
        // A post in flight cannot be cancelled from the UI; Esc and the trigger do nothing until it ends.
        if self.busy() {
            return;
        }
        self.submit_dialog = None;
        self.submit_prepare_task = None;
        cx.notify();
    }

    fn set_submit_event(&mut self, event: ReviewEvent, cx: &mut Context<Self>) {
        if let Some(dialog) = self.submit_dialog.as_mut()
            && matches!(dialog.phase, SubmitPhase::Editing | SubmitPhase::Failed { .. })
        {
            dialog.event = event;
            cx.notify();
        }
    }

    /// Check the drafts against GitHub's diff at the PR head and learn that head.
    fn prepare_submit(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.submit_dialog.as_ref() else {
            return;
        };
        let (target, pr) = (dialog.target.clone(), dialog.pr.clone());
        let drafts = self.local_review_drafts.get(&target).map(Vec::as_slice).unwrap_or_default();
        let (placed, _) = self.draft_anchor_index.partition_drafts(drafts);
        let candidates: Vec<SubmitReviewComment> = placed
            .into_iter()
            .filter(|(draft, _)| !draft.body.trim().is_empty())
            .map(|(draft, (start, end))| self.submit_comment(draft, start, end, true))
            .collect();
        let prepared = candidates
            .iter()
            .map(|c| (c.draft_id.clone(), (c.line, c.start_line)))
            .collect();
        let params = PrepareReviewParams { owner: pr.owner, repo: pr.repo, number: pr.number, comments: candidates };
        let value = serde_json::to_value(&params).unwrap_or_default();
        if let Some(dialog) = self.submit_dialog.as_mut() {
            dialog.phase = SubmitPhase::Preparing;
            dialog.head_sha = None;
            dialog.prepared = prepared;
            dialog.validated.clear();
            dialog.rejected.clear();
        }
        let task = self.state.update(cx, |state, cx| state.chat_diff_rpc(methods::PREPARE_REVIEW_SUBMIT, value, cx));
        self.submit_prepare_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|value| {
                serde_json::from_value::<PrepareReviewReply>(value).map_err(|e| e.to_string())
            });
            this.update(cx, |this, cx| this.finish_prepare(&target, result, cx)).ok();
        }));
        cx.notify();
    }

    fn finish_prepare(&mut self, target: &str, result: Result<PrepareReviewReply, String>, cx: &mut Context<Self>) {
        self.submit_prepare_task = None;
        let local_head = self.local_review_head(cx);
        let Some(dialog) = self.submit_dialog.as_mut().filter(|d| d.target == target) else {
            return;
        };
        match result {
            Ok(reply) if !reply.head_sha.trim().is_empty() => {
                dialog.local_ahead = local_head.as_deref().is_some_and(|h| !h.eq_ignore_ascii_case(&reply.head_sha));
                let label = unplaced_label(local_head.as_deref(), &reply.head_sha);
                dialog.validated = reply.placed.into_iter().collect();
                dialog.rejected = reply.unplaced.into_iter().map(|u| (u.draft_id, label.to_owned())).collect();
                dialog.head_sha = Some(reply.head_sha);
                dialog.phase = SubmitPhase::Editing;
            }
            Ok(_) => {
                dialog.phase = SubmitPhase::Failed {
                    message: "GitHub did not report this pull request's head commit, so Submit is off.".into(),
                    reprepare: true,
                };
            }
            Err(error) => {
                dialog.phase = SubmitPhase::Failed {
                    message: format!("Couldn't check your comments against GitHub: {error}"),
                    reprepare: true,
                };
            }
        }
        cx.notify();
    }

    /// The click handler of the dialog's primary button: the only code that posts.
    pub(super) fn submit_review_now(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.submit_dialog.as_ref() else {
            return;
        };
        match &dialog.phase {
            SubmitPhase::Failed { reprepare: true, .. } => return self.prepare_submit(cx),
            SubmitPhase::Editing | SubmitPhase::Failed { .. } => {}
            _ => return,
        }
        // The dialog's own PR and target, never whatever is selected now.
        if self.review_target.as_deref() != Some(dialog.target.as_str()) {
            self.submit_dialog = None;
            cx.notify();
            return;
        }
        let (target, pr, event) = (dialog.target.clone(), dialog.pr.clone(), dialog.event);
        let Some(head) = dialog.head_sha.clone() else {
            return;
        };
        let body = dialog.input.read(cx).text().to_owned();
        // Re-read the drafts now: send what exists now, with its current text.
        let (comments, _) = self.submit_selection(dialog, true);
        let fail = |this: &mut Self, message: &str, cx: &mut Context<Self>| {
            if let Some(dialog) = this.submit_dialog.as_mut() {
                dialog.phase = SubmitPhase::Failed { message: message.to_owned(), reprepare: false };
            }
            cx.notify();
        };
        if comments.is_empty() && body.trim().is_empty() {
            return fail(self, "Add a summary or at least one comment first.", cx);
        }
        if self.local_draft_pending.contains_key(&target) {
            return fail(self, "Drafts are still saving. Try again in a moment.", cx);
        }
        let params = SubmitReviewParams {
            target: target.clone(),
            owner: pr.owner,
            repo: pr.repo,
            number: pr.number,
            event,
            body,
            comments,
            commit_id: head,
            check_only: false,
        };
        if let Some(dialog) = self.submit_dialog.as_mut() {
            dialog.phase = SubmitPhase::Sending;
            dialog.last_params = Some(params.clone());
        }
        self.run_submit(params, false, cx);
    }

    /// Send `SubmitReview` (or its check-only form). The task lives on `Changes`, so
    /// the reply applies even if the dialog is closed or replaced meanwhile.
    fn run_submit(&mut self, params: SubmitReviewParams, checking: bool, cx: &mut Context<Self>) {
        let target = params.target.clone();
        self.submit_inflight = Some(target.clone());
        let value = serde_json::to_value(&params).unwrap_or_default();
        let task = self.state.update(cx, |state, cx| state.chat_diff_rpc(methods::SUBMIT_REVIEW, value, cx));
        self.submit_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|value| {
                serde_json::from_value::<SubmitReviewReply>(value).map_err(|e| e.to_string())
            });
            this.update(cx, |this, cx| this.finish_submit_review(&target, checking, result, cx)).ok();
        }));
        cx.notify();
    }

    fn finish_submit_review(
        &mut self,
        target: &str,
        checking: bool,
        result: Result<SubmitReviewReply, String>,
        cx: &mut Context<Self>,
    ) {
        self.submit_task = None;
        let phase = match &result {
            Ok(reply) => match reply.outcome {
                SubmitReviewOutcome::Posted | SubmitReviewOutcome::AlreadyPosted => SubmitPhase::Posted {
                    url: reply.url.clone(),
                    already: reply.outcome == SubmitReviewOutcome::AlreadyPosted,
                    warning: reply.local_record_error.clone(),
                },
                SubmitReviewOutcome::PendingConflict => SubmitPhase::Conflict { url: reply.url.clone() },
                SubmitReviewOutcome::NotFound => SubmitPhase::Failed {
                    message: "The review did not reach GitHub. Nothing was posted; Retry is safe.".into(),
                    reprepare: false,
                },
            },
            Err(error) if checking => SubmitPhase::Failed {
                message: format!("Result unknown: couldn't confirm with GitHub ({error}). Retry checks again before posting."),
                reprepare: false,
            },
            Err(error) if result_unknown(error) => SubmitPhase::Checking,
            Err(error) => SubmitPhase::Failed {
                message: error.clone(),
                reprepare: error.contains("new commits"),
            },
        };
        if phase == SubmitPhase::Checking {
            // Result unknown: ask GitHub whether the review landed before any new post.
            let check = self
                .submit_dialog
                .as_ref()
                .filter(|d| d.target == target)
                .and_then(|d| d.last_params.clone())
                .map(|mut params| {
                    params.check_only = true;
                    params
                });
            if let Some(params) = check {
                if let Some(dialog) = self.submit_dialog.as_mut() {
                    dialog.phase = SubmitPhase::Checking;
                }
                return self.run_submit(params, true, cx);
            }
        }
        self.submit_inflight = None;
        let posted = matches!(phase, SubmitPhase::Posted { .. });
        if posted {
            // The engine removed the drafts it sent; reload the rest and the threads.
            self.local_review_drafts.remove(target);
            self.local_draft_acknowledged.remove(target);
            self.local_draft_comments.remove(target);
        }
        if let Some(dialog) = self.submit_dialog.as_mut().filter(|d| d.target == target) {
            dialog.phase = phase;
        }
        if posted && self.review_target.as_deref() == Some(target) {
            self.load_local_drafts(target, cx);
            self.load_review_threads(target, cx);
        }
        cx.notify();
    }

    pub(super) fn render_submit_review(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        // The dialog belongs to the review it opened on.
        if self.submit_dialog.as_ref().is_some_and(|d| self.review_target.as_deref() != Some(d.target.as_str())) {
            self.submit_dialog = None;
            self.submit_prepare_task = None;
        }
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
        let (comments, held) = self.submit_selection(dialog, false);
        let count = comments.len();
        let busy = matches!(dialog.phase, SubmitPhase::Sending | SubmitPhase::Checking);
        let preparing = dialog.phase == SubmitPhase::Preparing;
        let finished = matches!(dialog.phase, SubmitPhase::Posted { .. });
        let conflict = matches!(dialog.phase, SubmitPhase::Conflict { .. });
        let reprepare = matches!(dialog.phase, SubmitPhase::Failed { reprepare: true, .. });
        let ready = dialog.head_sha.is_some() && matches!(dialog.phase, SubmitPhase::Editing | SubmitPhase::Failed { reprepare: false, .. });
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
        let hidden = held.len().saturating_sub(5);
        let held_rows: Vec<_> = held
            .iter()
            .take(5)
            .map(|(draft, reason)| {
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_muted)
                    .truncate()
                    .child(SharedString::from(format!(
                        "{}:{} \u{2014} {} ({reason})",
                        draft.anchor.file,
                        draft.anchor.end_line,
                        one_line(&draft.body)
                    )))
            })
            .collect();
        let note = |text: &str, color: gpui::Hsla| -> AnyElement {
            div().text_size(px(11.0)).text_color(color).child(SharedString::from(text.to_owned())).into_any_element()
        };
        let status: Option<AnyElement> = match &dialog.phase {
            SubmitPhase::Editing => dialog
                .head_sha
                .is_none()
                .then(|| note("The pull request's head commit is unknown, so Submit is off.", theme.warning)),
            SubmitPhase::Preparing => Some(note("Checking your comments against GitHub\u{2026}", theme.text_muted)),
            SubmitPhase::Sending => Some(note("Posting\u{2026}", theme.text_muted)),
            SubmitPhase::Checking => Some(note("Result unknown; checking GitHub\u{2026}", theme.warning)),
            SubmitPhase::Failed { message, .. } => Some(
                crate::error_notice::ErrorNotice::from_text("submit-review-error", theme, message).into_any_element(),
            ),
            SubmitPhase::Conflict { url } => {
                let url = url.clone();
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(note(
                            "You already have a pending review on this pull request (started on GitHub). Submit or discard it there, then try again. Nothing was sent.",
                            theme.warning,
                        ))
                        .child(
                            button("submit-review-open-pr", "Open pull request", ButtonVariant::Secondary, ButtonSize::Sm, theme)
                                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                        )
                        .into_any_element(),
                )
            }
            SubmitPhase::Posted { url, already, warning } => {
                let url = url.clone();
                let text = if *already {
                    "Already posted: this review was on GitHub; nothing was sent again."
                } else {
                    "Review posted."
                };
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(div().text_size(px(11.5)).text_color(theme.text).child(text))
                        .when_some(warning.clone(), |el, warning| {
                            el.child(note(
                                &format!("Mast couldn't save the local record ({warning}). Your drafts are still here; nothing will be posted twice."),
                                theme.warning,
                            ))
                        })
                        .child(
                            button("submit-review-open-review", "Open on GitHub", ButtonVariant::Secondary, ButtonSize::Sm, theme)
                                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                        )
                        .into_any_element(),
                )
            }
        };
        let primary_label = if busy {
            "Posting\u{2026}"
        } else if reprepare {
            "Check again"
        } else if matches!(dialog.phase, SubmitPhase::Failed { .. }) {
            "Retry"
        } else {
            "Submit"
        };
        let enabled = ready || reprepare;
        div()
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
            .child(div().text_size(px(12.5)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme.text).child("Submit review"))
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
                    .child(SharedString::from(match (preparing, count) {
                        (true, _) => "Checking draft comments\u{2026}".to_owned(),
                        (_, 0) => "No draft comments will be posted".to_owned(),
                        (_, 1) => "1 draft comment will be posted".to_owned(),
                        (_, n) => format!("{n} draft comments will be posted"),
                    })),
            )
            .when(!held.is_empty(), |el| {
                el.child(
                    div()
                        .id("submit-review-unplaced")
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div().text_size(px(11.0)).text_color(theme.warning).child(SharedString::from(format!(
                                "{} can\u{2019}t be placed on the pull request. They stay as local drafts and are not posted:",
                                held.len()
                            ))),
                        )
                        .children(held_rows)
                        .when(hidden > 0, |el| {
                            el.child(div().text_size(px(11.0)).text_color(theme.text_faint).child(SharedString::from(format!("and {hidden} more"))))
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
                        .when(busy, |el| el.opacity(0.5))
                        .on_click(cx.listener(|this, _, _, cx| this.close_submit_review(cx))),
                    )
                    .when(!finished && !conflict, |el| {
                        let send = button("submit-review-send", primary_label, ButtonVariant::Primary, ButtonSize::Sm, theme)
                            .when(!enabled || busy || preparing, |el| el.opacity(0.5));
                        el.child(if enabled && !busy && !preparing {
                            send.on_click(cx.listener(|this, _, _, cx| this.submit_review_now(cx)))
                        } else {
                            send
                        })
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::super::review_drafts::{line_hash, tests::install_diff};
    use super::*;
    use crate::comments::CommentSide;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    const TARGET: &str = "pr:o/r#1";

    /// A scripted engine: what GitHub's diff accepts, what each SubmitReview returns, and
    /// the drafts the engine would hold (only the sent ones leave).
    #[derive(Default)]
    struct Script {
        drafts: Mutex<Vec<ReviewDraft>>,
        submits: Mutex<VecDeque<Result<serde_json::Value, String>>>,
        reject: Mutex<Vec<String>>,
        prepare_error: Mutex<Option<String>>,
        calls: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl Script {
        fn submit_calls(&self) -> Vec<serde_json::Value> {
            self.calls_of(methods::SUBMIT_REVIEW)
        }
        fn calls_of(&self, method: &str) -> Vec<serde_json::Value> {
            self.calls.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, p)| p.clone()).collect()
        }
        fn queue(&self, reply: Result<serde_json::Value, String>) {
            self.submits.lock().unwrap().push_back(reply);
        }
        fn remaining(&self) -> Vec<String> {
            self.drafts.lock().unwrap().iter().map(|d| d.id.clone()).collect()
        }
    }

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

    fn posted() -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({"outcome":"posted","reviewId":9,"url":"https://github.com/o/r/pull/1#pullrequestreview-9"}))
    }

    fn fake(script: Arc<Script>) -> crate::diff_targets::FakeRpc {
        Arc::new(move |method, params| {
            script.calls.lock().unwrap().push((method.to_owned(), params.clone()));
            let ids = |params: &serde_json::Value| -> Vec<String> {
                params["comments"].as_array().map(|c| c.iter().filter_map(|c| c["draftId"].as_str().map(str::to_owned)).collect()).unwrap_or_default()
            };
            match method {
                m if m == methods::PREPARE_REVIEW_SUBMIT => {
                    if let Some(error) = script.prepare_error.lock().unwrap().take() {
                        return Err(error);
                    }
                    let reject = script.reject.lock().unwrap().clone();
                    let (unplaced, placed): (Vec<_>, Vec<_>) = ids(&params).into_iter().partition(|id| reject.contains(id));
                    let unplaced: Vec<_> = unplaced.into_iter().map(|id| serde_json::json!({"draftId": id, "reason": "not in diff"})).collect();
                    Ok(serde_json::json!({"headSha": "abc123", "placed": placed, "unplaced": unplaced}))
                }
                m if m == methods::SUBMIT_REVIEW => {
                    let mut reply = script.submits.lock().unwrap().pop_front().unwrap_or_else(posted)?;
                    let landed = matches!(reply["outcome"].as_str(), Some("posted" | "already-posted"));
                    if landed {
                        if reply.get("sentDraftIds").is_none() {
                            reply["sentDraftIds"] = serde_json::json!(ids(&params));
                        }
                        let sent: Vec<String> = reply["sentDraftIds"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(str::to_owned)).collect();
                        script.drafts.lock().unwrap().retain(|d| !sent.contains(&d.id));
                    }
                    Ok(reply)
                }
                m if m == methods::GET_REVIEW_DRAFTS => Ok(serde_json::to_value(script.drafts.lock().unwrap().clone()).unwrap()),
                m if m == methods::GET_REVIEW_THREADS => Ok(serde_json::json!([])),
                _ => Ok(serde_json::json!(true)),
            }
        })
    }

    /// A PR review with one draft that places and one whose line is gone.
    fn setup(cx: &mut gpui::TestAppContext) -> (gpui::Entity<Changes>, &mut gpui::VisualTestContext, Arc<Script>) {
        let script = Arc::new(Script::default());
        let rpc = fake(script.clone());
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let (changes, vcx) = cx.add_window_view(move |_, cx| {
            let mut state = AppState::new();
            state.chat_diff_rpc_override = Some(rpc);
            let state = cx.new(|_| state);
            Changes::new(state, cx)
        });
        let seeded = script.clone();
        changes.update(vcx, |changes, cx| {
            let hunk = install_diff(changes);
            changes.review_target = Some(TARGET.into());
            changes.scope = DiffScope::Branch;
            let drafts = vec![
                draft("placed", &hunk, &line_hash("a", CommentSide::New, "new")),
                draft("lost", &hunk, "missing"),
            ];
            *seeded.drafts.lock().unwrap() = drafts.clone();
            changes.local_review_drafts.insert(TARGET.into(), drafts);
            changes.rebuild_draft_index(cx);
        });
        (changes, vcx, script)
    }

    fn open(changes: &gpui::Entity<Changes>, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| changes.update(cx, |c, cx| c.open_submit_review(window, cx)));
        vcx.run_until_parked();
    }

    fn click(changes: &gpui::Entity<Changes>, vcx: &mut gpui::VisualTestContext) {
        changes.update(vcx, |c, cx| c.submit_review_now(cx));
    }

    fn phase(changes: &gpui::Entity<Changes>, vcx: &gpui::VisualTestContext) -> Option<SubmitPhase> {
        changes.read_with(vcx, |c, _| c.submit_dialog.as_ref().map(|d| d.phase.clone()))
    }

    fn render(changes: &gpui::Entity<Changes>, vcx: &mut gpui::VisualTestContext) {
        changes.update(vcx, |c, cx| {
            let theme = Theme::of(cx).clone();
            let _ = c.render_submit_review(&theme, cx);
        });
    }

    #[gpui::test]
    fn opening_checks_against_github_and_lists_what_cannot_be_placed(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        assert!(changes.read_with(&*vcx, |c, _| c.can_submit_review()));
        open(&changes, vcx);
        assert_eq!(phase(&changes, vcx), Some(SubmitPhase::Editing));
        changes.update(vcx, |c, cx| {
            let dialog = c.submit_dialog.as_ref().unwrap();
            assert_eq!(dialog.head_sha.as_deref(), Some("abc123"));
            let (comments, held) = c.submit_selection(dialog, true);
            assert_eq!(comments.iter().map(|c| c.draft_id.as_str()).collect::<Vec<_>>(), ["placed"]);
            assert_eq!((comments[0].line, comments[0].line_text.as_str()), (1, "new"));
            assert_eq!(held.iter().map(|(d, r)| (d.id.as_str(), r.as_str())).collect::<Vec<_>>(), [("lost", "its line is no longer in the diff")]);
            let theme = Theme::of(cx).clone();
            let _ = c.render_submit_review(&theme, cx);
        });
        let prepares = script.calls_of(methods::PREPARE_REVIEW_SUBMIT);
        assert_eq!(prepares.len(), 1);
        assert_eq!(prepares[0]["comments"][0]["lineText"], "new");
        assert!(script.submit_calls().is_empty(), "opening the dialog must not post");
    }

    #[gpui::test]
    fn drafts_github_cannot_place_are_held_back_with_a_reason(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        script.reject.lock().unwrap().push("placed".into());
        open(&changes, vcx);
        changes.update(vcx, |c, _| {
            let dialog = c.submit_dialog.as_ref().unwrap();
            let (comments, held) = c.submit_selection(dialog, false);
            assert!(comments.is_empty());
            assert!(held.iter().any(|(d, r)| d.id == "placed" && r == "uncommitted or changed locally"));
        });
        assert_eq!(unplaced_label(Some("def"), "abc123"), "not pushed");
        assert_eq!(unplaced_label(Some("ABC123"), "abc123"), "uncommitted or changed locally");
        assert_eq!(unplaced_label(None, "abc123"), "uncommitted or changed locally");
    }

    #[gpui::test]
    fn submit_posts_once_pinned_to_the_head_and_unplaced_drafts_survive(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        open(&changes, vcx);
        changes.update(vcx, |changes, cx| {
            changes.set_submit_event(ReviewEvent::RequestChanges, cx);
            changes.submit_dialog.as_ref().unwrap().input.update(cx, |input, cx| input.set_text("Please fix", cx));
        });
        click(&changes, vcx);
        vcx.run_until_parked();
        let sent = script.submit_calls();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["event"], "request-changes");
        assert_eq!(sent[0]["body"], "Please fix");
        assert_eq!(sent[0]["commitId"], "abc123");
        assert_eq!(sent[0]["owner"], "o");
        assert_eq!(sent[0]["number"], 1);
        assert_eq!(sent[0]["comments"].as_array().unwrap().len(), 1);
        assert_eq!(sent[0]["comments"][0]["draftId"], "placed");
        assert!(matches!(phase(&changes, vcx), Some(SubmitPhase::Posted { already: false, ref url, .. }) if url.ends_with("-9")));
        // The engine kept only the unsent draft; the reload shows exactly that.
        assert_eq!(script.remaining(), ["lost"]);
        changes.read_with(&*vcx, |c, _| {
            assert_eq!(c.local_review_drafts[TARGET].iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["lost"]);
            assert!(c.submit_inflight.is_none());
        });
        assert!(!script.calls_of(methods::GET_REVIEW_THREADS).is_empty(), "threads refresh");
        // Posted: a further click does nothing.
        click(&changes, vcx);
        assert_eq!(script.submit_calls().len(), 1);
    }

    #[gpui::test]
    fn second_click_while_sending_does_not_post_again(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        open(&changes, vcx);
        click(&changes, vcx);
        assert_eq!(phase(&changes, vcx), Some(SubmitPhase::Sending));
        click(&changes, vcx);
        click(&changes, vcx);
        vcx.run_until_parked();
        assert_eq!(script.submit_calls().len(), 1);
    }

    #[gpui::test]
    fn closing_while_sending_is_ignored_and_the_result_still_applies(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        open(&changes, vcx);
        click(&changes, vcx);
        changes.update(vcx, |c, cx| c.close_submit_review(cx));
        assert_eq!(phase(&changes, vcx), Some(SubmitPhase::Sending), "Esc/trigger/Cancel do nothing while posting");
        vcx.update(|window, cx| changes.update(cx, |c, cx| c.open_submit_review(window, cx)));
        vcx.run_until_parked();
        assert_eq!(script.submit_calls().len(), 1);
        assert!(matches!(phase(&changes, vcx), Some(SubmitPhase::Posted { .. })));
        assert_eq!(script.remaining(), ["lost"]);
    }

    #[gpui::test]
    fn switching_review_while_open_closes_without_posting(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        open(&changes, vcx);
        changes.update(vcx, |c, _| c.review_target = Some("pr:o/r#2".into()));
        render(&changes, vcx);
        assert_eq!(phase(&changes, vcx), None);
        click(&changes, vcx);
        vcx.run_until_parked();
        assert!(script.submit_calls().is_empty());
    }

    #[gpui::test]
    fn switching_review_while_sending_still_applies_the_reply_to_the_right_pr(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        open(&changes, vcx);
        click(&changes, vcx);
        changes.update(vcx, |c, _| c.review_target = Some("pr:o/r#2".into()));
        render(&changes, vcx);
        vcx.run_until_parked();
        let sent = script.submit_calls();
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0]["number"].as_u64(), sent[0]["target"].as_str()), (Some(1), Some(TARGET)), "posted to the PR the dialog opened on");
        assert_eq!(phase(&changes, vcx), None);
        changes.read_with(&*vcx, |c, _| {
            assert!(c.submit_inflight.is_none());
            assert!(!c.local_review_drafts.contains_key(TARGET), "the posted PR's cache is dropped for a reload");
        });
    }

    #[gpui::test]
    fn timeout_is_checked_and_a_landed_review_is_not_posted_again(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        script.queue(Err("gh timed out".into()));
        script.queue(Ok(serde_json::json!({"outcome":"already-posted","reviewId":9,"url":"https://github.com/o/r/pull/1#pullrequestreview-9","sentDraftIds":["placed"]})));
        open(&changes, vcx);
        click(&changes, vcx);
        vcx.run_until_parked();
        let sent = script.submit_calls();
        assert_eq!(sent.len(), 2, "one post attempt, then one check");
        assert_eq!(sent[0]["checkOnly"], false);
        assert_eq!(sent[1]["checkOnly"], true);
        assert!(matches!(phase(&changes, vcx), Some(SubmitPhase::Posted { already: true, .. })));
        assert_eq!(script.remaining(), ["lost"]);
    }

    #[gpui::test]
    fn unknown_result_blocks_posting_until_checked_then_retry_is_allowed(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        script.queue(Err("gh timed out".into()));
        script.queue(Ok(serde_json::json!({"outcome":"not-found","url":"https://github.com/o/r/pull/1"})));
        open(&changes, vcx);
        click(&changes, vcx);
        vcx.run_until_parked();
        assert!(matches!(phase(&changes, vcx), Some(SubmitPhase::Failed { reprepare: false, .. })));
        assert_eq!(script.remaining(), ["placed", "lost"], "nothing lost");
        click(&changes, vcx);
        vcx.run_until_parked();
        let sent = script.submit_calls();
        assert_eq!(sent.iter().map(|p| p["checkOnly"].as_bool().unwrap()).collect::<Vec<_>>(), [false, true, false]);
        assert!(matches!(phase(&changes, vcx), Some(SubmitPhase::Posted { already: false, .. })));
    }

    #[gpui::test]
    fn a_clear_rejection_keeps_drafts_and_does_not_trigger_a_check(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        script.queue(Err("Validation Failed: line must be part of the diff (HTTP 422)".into()));
        open(&changes, vcx);
        click(&changes, vcx);
        vcx.run_until_parked();
        assert!(matches!(phase(&changes, vcx), Some(SubmitPhase::Failed { ref message, .. }) if message.contains("HTTP 422")));
        assert_eq!(script.submit_calls().len(), 1);
        assert_eq!(script.remaining(), ["placed", "lost"]);
        click(&changes, vcx);
        vcx.run_until_parked();
        assert!(matches!(phase(&changes, vcx), Some(SubmitPhase::Posted { .. })));
    }

    #[gpui::test]
    fn pending_review_conflict_keeps_everything_and_offers_the_pr(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        script.queue(Ok(serde_json::json!({"outcome":"pending-conflict","url":"https://github.com/o/r/pull/1"})));
        open(&changes, vcx);
        click(&changes, vcx);
        vcx.run_until_parked();
        assert_eq!(phase(&changes, vcx), Some(SubmitPhase::Conflict { url: "https://github.com/o/r/pull/1".into() }));
        assert_eq!(script.remaining(), ["placed", "lost"]);
        render(&changes, vcx);
    }

    #[gpui::test]
    fn unknown_head_disables_submit_until_the_check_succeeds(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        *script.prepare_error.lock().unwrap() = Some("gh timed out".into());
        open(&changes, vcx);
        assert!(matches!(phase(&changes, vcx), Some(SubmitPhase::Failed { reprepare: true, .. })));
        click(&changes, vcx); // "Check again" prepares; it never posts
        vcx.run_until_parked();
        assert!(script.submit_calls().is_empty());
        assert_eq!(script.calls_of(methods::PREPARE_REVIEW_SUBMIT).len(), 2);
        assert_eq!(phase(&changes, vcx), Some(SubmitPhase::Editing));
    }

    #[gpui::test]
    fn click_time_sends_current_drafts_and_skips_ones_added_after_the_check(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        open(&changes, vcx);
        changes.update(vcx, |c, cx| {
            let drafts = c.local_review_drafts.get_mut(TARGET).unwrap();
            drafts[0].body = "edited after opening".into();
            let mut late = drafts[0].clone();
            late.id = "late".into();
            drafts.push(late);
            c.rebuild_draft_index(cx);
            let dialog = c.submit_dialog.as_ref().unwrap();
            let (_, held) = c.submit_selection(dialog, false);
            assert!(held.iter().any(|(d, r)| d.id == "late" && r.contains("added after the check")));
        });
        click(&changes, vcx);
        vcx.run_until_parked();
        let sent = script.submit_calls();
        assert_eq!(sent[0]["comments"].as_array().unwrap().len(), 1);
        assert_eq!(sent[0]["comments"][0]["body"], "edited after opening");
        // A draft deleted after opening is not sent.
        let (changes, vcx, script) = setup(cx);
        open(&changes, vcx);
        changes.update(vcx, |c, cx| {
            c.local_review_drafts.get_mut(TARGET).unwrap().retain(|d| d.id != "placed");
            c.rebuild_draft_index(cx);
        });
        click(&changes, vcx);
        assert!(script.submit_calls().is_empty(), "no summary and no comments left: refused locally");
        assert!(matches!(phase(&changes, vcx), Some(SubmitPhase::Failed { .. })));
    }

    #[gpui::test]
    fn a_failed_local_record_is_reported_and_leaves_the_drafts(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        script.queue(Ok(serde_json::json!({"outcome":"posted","reviewId":9,"url":"u","sentDraftIds":[],"localRecordError":"disk full"})));
        open(&changes, vcx);
        click(&changes, vcx);
        vcx.run_until_parked();
        assert!(matches!(phase(&changes, vcx), Some(SubmitPhase::Posted { warning: Some(ref w), .. }) if w == "disk full"));
        assert_eq!(script.remaining(), ["placed", "lost"]);
        render(&changes, vcx);
    }

    #[gpui::test]
    fn an_empty_review_is_refused_without_a_call(cx: &mut gpui::TestAppContext) {
        let (changes, vcx, script) = setup(cx);
        script.reject.lock().unwrap().push("placed".into());
        open(&changes, vcx);
        click(&changes, vcx);
        assert!(script.submit_calls().is_empty());
        assert!(matches!(phase(&changes, vcx), Some(SubmitPhase::Failed { reprepare: false, .. })));
    }
}
