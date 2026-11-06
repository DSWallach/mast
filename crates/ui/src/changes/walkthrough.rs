//! Presentation and coverage of an ordered review over an existing diff.
use super::{FileDiff, review_state::hunk_identity};
use std::collections::{BTreeSet, HashSet};

use gpui::{Context, Render, Window, div, prelude::*, px};
use mast_proto::{ReviewSidecar, ReviewStep};

use crate::theme::Theme;

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
pub(super) struct Walkthrough {
    pub sidecar: ReviewSidecar,
    pub files: Vec<FileDiff>,
    pub viewed: BTreeSet<String>,
    pub selected: usize,
}

impl Render for Walkthrough {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let coverage = TourCoverage::new(&self.sidecar.steps, &self.files);
        let progress = format!(
            "{} / {} reviewed",
            coverage.reviewed_count(&self.viewed),
            self.sidecar.steps.len()
        );
        let mut steps = div().id("review-steps").flex().flex_col().gap_1();
        for (index, step) in self.sidecar.steps.iter().enumerate() {
            let selected = index == self.selected;
            steps = steps.child(
                div()
                    .id(("review-step", index))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme.element_active))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = index;
                        cx.notify();
                    }))
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
            uncovered = uncovered.child(format!(
                "{}  {}",
                self.files[hunk.file].path, self.files[hunk.file].hunks[hunk.hunk].header
            ));
        }
        let note = self.sidecar.steps.get(self.selected).map(|step| {
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
        });
        div()
            .id("review-walkthrough")
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .text_color(theme.text)
            .bg(theme.bg)
            .child(
                div()
                    .id("review-progress")
                    .debug_selector(|| "review-progress".into())
                    .child(progress),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .w(px(240.))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(steps)
                            .child(uncovered),
                    )
                    .child(div().flex_1().children(note)),
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

    #[gpui::test]
    fn walkthrough_renders_step_note_and_coverage(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let (review, files) = fixture();
        let (_, vcx) = cx.add_window_view(|_, _| Walkthrough {
            sidecar: review,
            files,
            viewed: BTreeSet::new(),
            selected: 0,
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("review-progress").is_some());
        assert!(vcx.debug_bounds("review-note").is_some());
        assert!(vcx.debug_bounds("review-uncovered").is_some());
    }
}
