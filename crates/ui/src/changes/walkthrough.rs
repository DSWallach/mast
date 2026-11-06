//! Presentation and coverage of an ordered review over an existing diff.
use super::{Changes, DiffRow, FileDiff, review_state::hunk_identity};
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use gpui::{
    App, Context, Entity, FocusHandle, KeyBinding, Render, Subscription, Task, Window, actions,
    div, prelude::*, px,
};
use mast_proto::{
    ArtifactContentEncoding, ArtifactContentParams, ArtifactView, ReviewSidecar, ReviewStep,
};

use crate::theme::Theme;

actions!(
    review_walkthrough,
    [
        NextStep,
        PreviousStep,
        NextHunk,
        PreviousHunk,
        MarkReviewed,
        ToggleMode
    ]
);
const KEY_CONTEXT: &str =
    "ReviewWalkthrough && !Input && !Composer && !MessageComposer && !PaletteSearch";

pub(crate) fn bind_keys(cx: &mut App, keymap: &crate::settings::KeymapConfig) {
    use crate::settings::ShortcutId as S;
    for (id, action) in [
        (
            S::ReviewNextStep,
            Box::new(NextStep) as Box<dyn gpui::Action>,
        ),
        (S::ReviewPreviousStep, Box::new(PreviousStep)),
        (S::ReviewNextStepArrow, Box::new(NextStep)),
        (S::ReviewPreviousStepArrow, Box::new(PreviousStep)),
        (S::ReviewNextHunk, Box::new(NextHunk)),
        (S::ReviewPreviousHunk, Box::new(PreviousHunk)),
        (S::ReviewMarkReviewed, Box::new(MarkReviewed)),
        (S::ReviewToggleMode, Box::new(ToggleMode)),
    ] {
        let combo = keymap.get(id);
        if combo.is_empty() {
            continue;
        }
        let combo = crate::settings::platform_combo(combo);
        let combo = if gpui::Keystroke::parse(&combo).is_ok() {
            combo
        } else {
            crate::settings::platform_combo(id.default_combo())
        };
        let predicate = Some(std::rc::Rc::new(
            gpui::KeyBindingContextPredicate::parse(KEY_CONTEXT).unwrap(),
        ));
        if let Ok(binding) = KeyBinding::load(
            &combo,
            action,
            predicate,
            false,
            None,
            &gpui::DummyKeyboardMapper,
        ) {
            cx.bind_keys([binding]);
        }
    }
}

/// Loads the chat's current review artifact and owns its diff for the lifetime
/// of the surface, including while the surface moves between hosts.
pub(crate) struct ReviewHost {
    view: Option<Entity<Walkthrough>>,
    error: Option<String>,
    state: Entity<crate::state::AppState>,
    shell: gpui::WeakEntity<crate::shell::Shell>,
    chat_id: String,
    pr: Option<mast_proto::ReviewPullRequest>,
    _state_observer: Subscription,
    _shell_observer: Option<Subscription>,
    _load: Task<()>,
}

impl ReviewHost {
    pub(crate) fn new(
        state: Entity<crate::state::AppState>,
        chat_id: String,
        artifact: &ArtifactView,
        shell: gpui::WeakEntity<crate::shell::Shell>,
        cx: &mut Context<Self>,
    ) -> Self {
        let changes = cx.new(|cx| {
            let mut changes = Changes::for_chat(state.clone(), chat_id.clone(), cx);
            if changes.scope == super::DiffScope::WorkingTree {
                changes.set_scope(super::DiffScope::Branch, cx);
            }
            changes
        });
        let state_observer = cx.observe(&state, |_, _, cx| cx.notify());
        let shell_observer = shell
            .upgrade()
            .map(|shell| cx.observe(&shell, |_, _, cx| cx.notify()));
        let target_device_id = (state.read(cx).local_device_id.as_deref()
            != Some(artifact.device_id.as_str()))
        .then(|| artifact.device_id.clone());
        let call = state
            .read(cx)
            .artifact_actions()
            .content(ArtifactContentParams {
                artifact_id: artifact.id.clone(),
                revision: None,
                companion: None,
                target_device_id,
            });
        let load = cx.spawn(async move |this, cx| {
            let reply = call.await;
            let _ = this.update(cx, |this, cx| {
                let review = reply.map_err(|error| error.text()).and_then(|reply| {
                    if reply.encoding != ArtifactContentEncoding::Utf8 {
                        return Err("Review content is not UTF-8".to_string());
                    }
                    serde_json::from_str::<ReviewSidecar>(&reply.content)
                        .map_err(|error| format!("Invalid review: {error}"))
                });
                match review {
                    Ok(sidecar) => {
                        this.pr = Some(sidecar.pr.clone());
                        this.view = Some(cx.new(|cx| Walkthrough::new(sidecar.into(), changes, cx)))
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        });
        Self {
            view: None,
            error: None,
            state,
            shell,
            chat_id,
            pr: None,
            _state_observer: state_observer,
            _shell_observer: shell_observer,
            _load: load,
        }
    }
}

impl Render for ReviewHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let (Some(view), Some(pr)) = (&self.view, &self.pr) {
            let cached = self
                .shell
                .upgrade()
                .and_then(|shell| shell.read(cx).cached_review_head(&self.chat_id, pr));
            let stale = if let Some(head) = cached {
                (!head.eq_ignore_ascii_case(&pr.head_sha)).then_some(head)
            } else {
                let card = crate::artifacts::model::ReviewCard {
                    pr: pr.clone(),
                    summary: String::new(),
                    highlights: Vec::new(),
                };
                crate::artifacts::model::review_head_drift(
                    &card,
                    self.state.read(cx).chat_ref_targets(&self.chat_id),
                )
                .map(str::to_owned)
            };
            view.update(cx, |view, cx| {
                if view.stale_head != stale {
                    view.stale_head = stale;
                    cx.notify();
                }
            });
        }
        div()
            .size_full()
            .when_some(self.view.clone(), |el, view| el.child(view))
            .when(self.view.is_none(), |el| {
                el.child(
                    self.error
                        .clone()
                        .unwrap_or_else(|| "Loading review…".into()),
                )
            })
    }
}

/// Identifies a hunk in the displayed diff, independently of the target kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TourHunk {
    pub file: usize,
    pub hunk: usize,
    pub identity: String,
}

/// Temporary range adapter. Replace this with sidecar identity lookup when
/// publishers stamp the same changed-line identity as the Changes view.
fn step_covers_hunk(step: &ReviewStep, file: &FileDiff, hunk: usize) -> bool {
    if step.file != file.path {
        return false;
    }
    let Some(hunk) = file.hunks.get(hunk) else {
        return false;
    };
    let mut lines = hunk.lines.iter().filter_map(|line| line.new_no);
    let Some(first) = lines.next() else {
        return false;
    };
    let (start, end) = lines.fold((first, first), |(start, end), line| {
        (start.min(line), end.max(line))
    });
    step.hunks.iter().any(|range| {
        range.new_start <= range.new_end && range.new_start <= end && start <= range.new_end
    })
}

#[derive(Clone, Debug)]
pub(super) struct TourCoverage {
    pub steps: Vec<Vec<TourHunk>>,
    complete: Vec<bool>,
    pub uncovered: Vec<TourHunk>,
}

impl TourCoverage {
    pub fn new(steps: &[ReviewStep], files: &[FileDiff]) -> Self {
        let mut covered = HashSet::new();
        let complete = steps
            .iter()
            .map(|step| {
                !step.hunks.is_empty()
                    && step.hunks.iter().all(|range| {
                        let mut single = step.clone();
                        single.hunks = vec![range.clone()];
                        files.iter().any(|file| {
                            (0..file.hunks.len())
                                .any(|index| step_covers_hunk(&single, file, index))
                        })
                    })
            })
            .collect();
        let steps = steps
            .iter()
            .map(|step| {
                files
                    .iter()
                    .enumerate()
                    .flat_map(|(file_index, file)| {
                        file.hunks
                            .iter()
                            .enumerate()
                            .filter_map(move |(hunk_index, hunk)| {
                                step_covers_hunk(step, file, hunk_index).then(|| TourHunk {
                                    file: file_index,
                                    hunk: hunk_index,
                                    identity: hunk_identity(&file.path, hunk),
                                })
                            })
                    })
                    .inspect(|hunk| {
                        covered.insert((hunk.file, hunk.hunk));
                    })
                    .collect()
            })
            .collect();
        let uncovered = files
            .iter()
            .enumerate()
            .flat_map(|(file_index, file)| {
                file.hunks.iter().enumerate().filter_map({
                    let covered = &covered;
                    move |(hunk_index, hunk)| {
                        (!covered.contains(&(file_index, hunk_index))).then(|| TourHunk {
                            file: file_index,
                            hunk: hunk_index,
                            identity: hunk_identity(&file.path, hunk),
                        })
                    }
                })
            })
            .collect();
        Self {
            steps,
            complete,
            uncovered,
        }
    }

    pub fn reviewed_count(&self, viewed: &BTreeSet<String>) -> usize {
        self.steps
            .iter()
            .enumerate()
            .filter(|(index, hunks)| {
                self.complete[*index]
                    && !hunks.is_empty()
                    && hunks.iter().all(|hunk| viewed.contains(&hunk.identity))
            })
            .count()
    }
}

/// A target-independent tour component. The owner supplies the same diff and
/// viewed identities used by Changes; no state is persisted in the sidecar.
pub(crate) struct Walkthrough {
    review: TourReview,
    files: Arc<Vec<FileDiff>>,
    coverage: Option<TourCoverage>,
    viewed: BTreeSet<String>,
    seen: HashSet<String>,
    selected: usize,
    tour: bool,
    stale_head: Option<String>,
    focus: FocusHandle,
    changes: Option<Entity<Changes>>,
    _changes_observer: Option<Subscription>,
}

/// Presentation data independent of whether the review targets a PR or branch.
struct TourReview {
    steps: Vec<ReviewStep>,
    head: String,
}

impl From<ReviewSidecar> for TourReview {
    fn from(sidecar: ReviewSidecar) -> Self {
        Self {
            steps: sidecar.steps,
            head: sidecar.pr.head_sha,
        }
    }
}

impl Walkthrough {
    fn new(review: TourReview, changes: Entity<Changes>, cx: &mut Context<Self>) -> Self {
        changes.update(cx, |changes, _| {
            changes.review_walkthrough = true;
            changes.review_tour = true;
        });
        let observer = cx.observe(&changes, |_, _, cx| cx.notify());
        Self {
            review,
            files: Arc::new(Vec::new()),
            coverage: None,
            viewed: BTreeSet::new(),
            seen: HashSet::new(),
            selected: 0,
            tour: true,
            stale_head: None,
            focus: cx.focus_handle(),
            changes: Some(changes),
            _changes_observer: Some(observer),
        }
    }

    fn coverage(&mut self) -> &TourCoverage {
        self.coverage
            .get_or_insert_with(|| TourCoverage::new(&self.review.steps, &self.files))
    }

    fn select_step(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.review.steps.len() {
            return;
        }
        self.selected = index;
        let coverage = self.coverage().clone();
        if let (Some(changes), Some(hunk)) = (&self.changes, coverage.steps[index].first()) {
            changes.update(cx, |changes, cx| {
                changes.jump_to_hunk(hunk.file, hunk.hunk, cx)
            });
        }
        cx.notify();
    }

    fn move_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let index = if forward {
            self.selected.saturating_add(1)
        } else {
            self.selected.saturating_sub(1)
        };
        self.select_step(index, cx);
    }

    fn move_hunk(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(changes) = &self.changes {
            changes.update(cx, |changes, cx| changes.step_diff(forward, false, cx));
        }
    }

    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.tour = !self.tour;
        if let Some(changes) = &self.changes {
            changes.update(cx, |changes, cx| {
                changes.review_tour = self.tour;
                cx.notify();
            });
        }
        cx.notify();
    }

    fn mark_current_hunk(&mut self, cx: &mut Context<Self>) {
        if let Some(changes) = &self.changes {
            changes.update(cx, |changes, cx| {
                let top = changes
                    .list
                    .logical_scroll_top()
                    .item_ix
                    .min(changes.rows.len().saturating_sub(1));
                let file = changes.rows.get(top).and_then(|row| row.file());
                let hunk = changes
                    .rows
                    .iter()
                    .take(top + 1)
                    .rev()
                    .find_map(|row| match row {
                        DiffRow::HunkHeader {
                            file: candidate,
                            hunk,
                        } if Some(*candidate as usize) == file => {
                            Some((*candidate as usize, *hunk as usize))
                        }
                        _ => None,
                    });
                if let Some((file, hunk)) = hunk {
                    changes.toggle_review_hunk(file, hunk, cx);
                }
            });
        }
    }

    fn mark_step(&mut self, cx: &mut Context<Self>) {
        let coverage = self.coverage().clone();
        let Some(hunks) = coverage.steps.get(self.selected) else {
            return;
        };
        if hunks.is_empty() || !coverage.complete[self.selected] {
            return;
        }
        let mark = !hunks
            .iter()
            .all(|hunk| self.viewed.contains(&hunk.identity));
        if let Some(changes) = &self.changes {
            changes.update(cx, |changes, cx| {
                let mut handled = HashSet::new();
                for hunk in hunks {
                    if handled.insert(&hunk.identity)
                        && self.viewed.contains(&hunk.identity) != mark
                    {
                        changes.toggle_review_hunk(hunk.file, hunk.hunk, cx);
                    }
                }
            });
        }
    }
}

impl Render for Walkthrough {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let controls = self
            .changes
            .as_ref()
            .map(|changes| changes.update(cx, |changes, cx| changes.render_header_controls(cx)));
        if let Some(changes) = &self.changes {
            changes.update(cx, |changes, cx| changes.ensure_content(cx));
            let changes = changes.read(cx);
            if let Some(parsed) = &changes.parsed {
                if !Arc::ptr_eq(&self.files, &parsed.files) {
                    self.files = parsed.files.clone();
                    self.coverage = None;
                }
            } else if !self.files.is_empty() {
                self.files = Arc::new(Vec::new());
                self.coverage = None;
            }
            self.viewed = changes
                .review_state
                .as_ref()
                .map(|state| state.viewed_hunks.clone())
                .unwrap_or_default();
            self.seen = changes.seen_hunks.clone();
        }
        let coverage = self.coverage().clone();
        let progress = format!(
            "{} / {} reviewed",
            coverage.reviewed_count(&self.viewed),
            self.review.steps.len()
        );
        let mut steps = div().id("review-steps").flex().flex_col().gap_1();
        for (index, step) in self.review.steps.iter().enumerate() {
            let selected = index == self.selected;
            let hunks = &coverage.steps[index];
            let reviewed = coverage.complete[index]
                && !hunks.is_empty()
                && hunks
                    .iter()
                    .all(|hunk| self.viewed.contains(&hunk.identity));
            let seen = !reviewed && hunks.iter().any(|hunk| self.seen.contains(&hunk.identity));
            steps = steps.child(
                div()
                    .id(("review-step", index))
                    .px_2()
                    .py_1()
                    .flex()
                    .gap_1()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme.element_active))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&this.focus, cx);
                        this.select_step(index, cx);
                    }))
                    .child(
                        div()
                            .text_color(if seen {
                                theme.text_faint
                            } else {
                                theme.text_muted
                            })
                            .child(if reviewed {
                                "✓"
                            } else if seen {
                                "·"
                            } else {
                                " "
                            }),
                    )
                    .child(format!("{}. {}", index + 1, step.title)),
            );
        }
        let mut uncovered = div()
            .id("review-uncovered")
            .debug_selector(|| "review-uncovered".into())
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_color(theme.text_muted).child("Not in the tour"));
        for hunk in &coverage.uncovered {
            let (file, index) = (hunk.file, hunk.hunk);
            uncovered = uncovered.child(
                div()
                    .id(gpui::SharedString::from(format!(
                        "uncovered-hunk-{file}-{index}"
                    )))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(changes) = &this.changes {
                            changes.update(cx, |changes, cx| changes.jump_to_hunk(file, index, cx));
                        }
                    }))
                    .child(format!(
                        "{}  {}",
                        self.files[file].path, self.files[file].hunks[index].header
                    )),
            );
        }
        let note = self.review.steps.get(self.selected).map(|step| {
            div()
                .id("review-note")
                .debug_selector(|| "review-note".into())
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .bg(theme.surface_card)
                .child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(step.title.clone()),
                )
                .child(
                    div()
                        .id("review-risk")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(theme.surface_raised)
                        .child(format!("Risk: {:?}", step.risk)),
                )
                .child(format!("What: {}", step.what))
                .child(format!("Why: {}", step.why))
                .child(step.risk_note.clone())
                .child(
                    div()
                        .id("review-mark-step")
                        .cursor_pointer()
                        .child("Mark reviewed")
                        .on_click(cx.listener(|this, _, _, cx| this.mark_step(cx))),
                )
        });
        div()
            .id("review-walkthrough")
            .track_focus(&self.focus)
            .key_context("ReviewWalkthrough")
            .on_action(cx.listener(|this, _: &NextStep, _, cx| this.move_step(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousStep, _, cx| this.move_step(false, cx)))
            .on_action(cx.listener(|this, _: &NextHunk, _, cx| this.move_hunk(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousHunk, _, cx| this.move_hunk(false, cx)))
            .on_action(cx.listener(|this, _: &MarkReviewed, _, cx| this.mark_current_hunk(cx)))
            .on_action(cx.listener(|this, _: &ToggleMode, _, cx| this.toggle_mode(cx)))
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .text_color(theme.text)
            .bg(theme.bg)
            .when_some(self.stale_head.clone(), |view, head| {
                let before: String = self.review.head.chars().take(7).collect();
                let after: String = head.chars().take(7).collect();
                view.child(
                    div()
                        .id("review-stale")
                        .debug_selector(|| "review-stale".into())
                        .p_2()
                        .bg(theme.surface_raised)
                        .text_color(theme.warning)
                        .child(format!("Stale review: head moved {before} → {after}")),
                )
            })
            .when_some(controls, |view, controls| {
                view.child(crate::surface_chrome::toolbar(&theme).child(controls))
            })
            .child(
                div()
                    .id("review-progress")
                    .debug_selector(|| "review-progress".into())
                    .child(progress)
                    .child(
                        div()
                            .id("review-mode")
                            .cursor_pointer()
                            .child(if self.tour { "Free diff" } else { "Tour" })
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.focus(&this.focus, cx);
                                this.toggle_mode(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .when(self.tour, |row| {
                        row.child(
                            div()
                                .id("review-navigation")
                                .w(px(160.))
                                .min_h_0()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(steps)
                                .child(uncovered),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .gap_3()
                            .when_some(self.changes.clone(), |row, changes| {
                                row.child(div().flex_1().min_w_0().child(changes))
                            })
                            .when(self.tour, |row| {
                                row.child(
                                    div()
                                        .id("review-notes")
                                        .w(px(200.))
                                        .min_h_0()
                                        .overflow_y_scroll()
                                        .children(note),
                                )
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::parse_patch;

    fn fixture() -> (ReviewSidecar, Vec<FileDiff>) {
        let review = serde_json::from_str(
            r#"{
          "schema":"mast.review/v1", "pr":{"repo":"o/r","number":1,"headSha":"a","baseSha":"b"},
          "summary":"Summary", "steps":[{"id":"s1","title":"First", "file":"a.rs",
          "hunks":[{"newStart":1,"newEnd":1}], "what":"Change", "why":"Reason",
          "risk":"low", "riskNote":"Check it", "tags":[]}],
          "highlights":[], "questions":[], "suggestedComments":[]
        }"#,
        )
        .unwrap();
        let files = parse_patch(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n@@ -9 +9 @@\n-before\n+after\n",
        );
        (review, files)
    }

    #[test]
    fn coverage_and_progress_use_manual_hunk_marks() {
        let (review, files) = fixture();
        let coverage = TourCoverage::new(&review.steps, &files);
        assert_eq!(coverage.steps[0].len(), 1);
        assert_eq!(coverage.uncovered.len(), 1);
        let mut viewed = BTreeSet::new();
        assert_eq!(coverage.reviewed_count(&viewed), 0);
        viewed.insert(coverage.steps[0][0].identity.clone());
        assert_eq!(coverage.reviewed_count(&viewed), 1);
        assert_eq!(coverage.uncovered[0].hunk, 1);
    }

    #[test]
    fn missing_anchor_cannot_count_as_reviewed() {
        let (mut review, files) = fixture();
        review.steps[0].hunks.push(mast_proto::ReviewHunk {
            new_start: 50,
            new_end: 51,
            hunk_hash: None,
        });
        let coverage = TourCoverage::new(&review.steps, &files);
        let viewed = coverage.steps[0]
            .iter()
            .map(|hunk| hunk.identity.clone())
            .collect();
        assert_eq!(coverage.reviewed_count(&viewed), 0);
    }

    #[test]
    fn review_shortcut_defaults_are_valid() {
        use crate::settings::ShortcutId as S;
        for id in [
            S::ReviewNextStep,
            S::ReviewPreviousStep,
            S::ReviewNextStepArrow,
            S::ReviewPreviousStepArrow,
            S::ReviewNextHunk,
            S::ReviewPreviousHunk,
            S::ReviewMarkReviewed,
            S::ReviewToggleMode,
        ] {
            assert!(
                gpui::Keystroke::parse(&crate::settings::platform_combo(id.default_combo()))
                    .is_ok(),
                "{id:?}"
            );
        }
    }

    #[gpui::test]
    fn walkthrough_renders_step_note_and_coverage(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let (review, files) = fixture();
        let (view, vcx) = cx.add_window_view(|_, cx| Walkthrough {
            review: review.into(),
            files: Arc::new(files),
            coverage: None,
            viewed: BTreeSet::new(),
            seen: HashSet::new(),
            selected: 0,
            tour: true,
            stale_head: None,
            focus: cx.focus_handle(),
            changes: None,
            _changes_observer: None,
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("review-progress").is_some());
        assert!(vcx.debug_bounds("review-note").is_some());
        assert!(vcx.debug_bounds("review-uncovered").is_some());
        assert!(vcx.debug_bounds("review-stale").is_none());
        view.update(vcx, |view, cx| {
            view.stale_head = Some("def456789".into());
            cx.notify();
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("review-stale").is_some());
    }

    #[gpui::test]
    fn review_keys_follow_the_persisted_binding(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            let mut keymap = crate::settings::KeymapConfig::default();
            keymap.review_next_step = "x".into();
            bind_keys(cx, &keymap);
        });
        let (mut review, files) = fixture();
        let mut second = review.steps[0].clone();
        second.id = "s2".into();
        review.steps.push(second);
        let (view, vcx) = cx.add_window_view(|_, cx| Walkthrough {
            review: review.into(),
            files: Arc::new(files),
            coverage: None,
            viewed: BTreeSet::new(),
            seen: HashSet::new(),
            selected: 0,
            tour: true,
            stale_head: None,
            focus: cx.focus_handle(),
            changes: None,
            _changes_observer: None,
        });
        vcx.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
            window.draw(cx).clear();
        });
        vcx.simulate_keystrokes("n");
        assert_eq!(view.read_with(vcx, |view, _| view.selected), 0);
        vcx.simulate_keystrokes("x");
        assert_eq!(view.read_with(vcx, |view, _| view.selected), 1);
        vcx.simulate_keystrokes("t");
        assert!(!view.read_with(vcx, |view, _| view.tour));
    }

    #[gpui::test]
    fn review_keys_do_not_intercept_the_diff_filter(cx: &mut gpui::TestAppContext) {
        use crate::changes::{
            FileHorizontalState, ParsedDiff, file_list::parse_diff_tree,
            review_state::hunk_identities,
        };
        use std::sync::Arc;
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            bind_keys(cx, &crate::settings::KeymapConfig::default());
        });
        let (review, _) = fixture();
        let (view, vcx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            let changes = cx.new(|cx| Changes::for_checkout(state, "/repo".into(), cx));
            changes.update(cx, |changes, cx| {
                let patch =
                    "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
                let (files, origins, submodules, layout) = parse_diff_tree(patch, &[]);
                let horizontal = files.iter().map(FileHorizontalState::new).collect();
                changes.scoped = Some(mast_proto::CheckoutDiff {
                    checkout_id: "checkout".into(),
                    device_id: "device".into(),
                    cwd: "/repo".into(),
                    patch: patch.into(),
                    files: Vec::new(),
                    submodules: Vec::new(),
                    additions: 1,
                    deletions: 1,
                    truncated: false,
                    checksum: "sum".into(),
                    updated_at: chrono::Utc::now(),
                });
                changes.scoped_for = Some("local|||/repo|workingTree|||".into());
                changes.parsed = Some(ParsedDiff {
                    key: "checkout:sum:wt".into(),
                    truncated: false,
                    additions: 1,
                    deletions: 1,
                    file_count: 1,
                    horizontal,
                    hunk_identities: Arc::new(hunk_identities(&files)),
                    files: Arc::new(files),
                    origins: Arc::new(origins),
                    submodules: Arc::new(submodules),
                    layout: Arc::new(layout),
                });
                changes.reflatten(cx);
            });
            let mut view = Walkthrough::new(review.into(), changes, cx);
            view.toggle_mode(cx);
            view
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        let filter = view.read_with(vcx, |view, cx| {
            view.changes.as_ref().unwrap().read(cx).file_filter.clone()
        });
        vcx.update(|window, cx| {
            let focus = gpui::Focusable::focus_handle(filter.read(cx), cx);
            window.focus(&focus, cx);
        });
        vcx.simulate_keystrokes("n v t");
        assert_eq!(
            filter.read_with(vcx, |input, _| input.text().to_owned()),
            "nvt"
        );
        assert!(!view.read_with(vcx, |view, _| view.tour));
    }
}
