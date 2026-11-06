//! Context carried by questions about a diff or a walkthrough step.

use super::{DiffLine, FileDiff, LineKind, SourceSide};
use mast_proto::ReviewStep;

/// A selection belongs to one file and one side, even in a split diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DiffSelection {
    pub path: String,
    pub side: SourceSide,
    pub anchor: u32,
    pub extent: u32,
}

impl DiffSelection {
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
