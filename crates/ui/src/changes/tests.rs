#[gpui::test]
fn visible_hunk_word_budget_across_frames(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| { gpui_base::init(cx); cx.set_global(Theme::dark()); });
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::new());
        let mut changes = Changes::new(state, cx);
        let mut patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n".to_string();
        for hunk in 0..32 {
            patch.push_str(&format!("@@ -{},50 +{},50 @@\n", hunk * 100 + 1, hunk * 100 + 1));
            for line in 0..50 { patch.push_str(&format!("-old line {line} with more text\n")); }
            for line in 0..50 { patch.push_str(&format!("+new line {line} with more text\n")); }
        }
        let (files, origins, submodules, layout) = parse_diff_tree(&patch, &[]);
        let (rows, ranges, headers) = flatten_diff_tree(&files, &submodules, &layout, &[], None, changes.mode, |_| false, |_| false);
        changes.rows = rows;
        changes.row_ranges = ranges;
        changes.section_headers = headers;
        changes.parsed = Some(ParsedDiff { key: "budget".into(), truncated: false, additions: 1600, deletions: 1600, file_count: 1, horizontal: files.iter().map(FileHorizontalState::new).collect(), hunk_identities: Arc::new(hunk_identities(&files)), files: Arc::new(files), origins: Arc::new(origins), submodules: Arc::new(submodules), layout: Arc::new(layout) });
        changes
    });
    for _ in 0..10 {
        vcx.update(|window, cx| view.update(cx, |changes, cx| {
            let indices = changes.rows.iter().enumerate().filter_map(|(ix, row)| matches!(row, DiffRow::Line { line: 0, .. }).then_some(ix)).collect::<Vec<_>>();
            assert_eq!(indices.len(), 32);
            for ix in indices { drop(changes.render_row(ix, window, cx)); }
        }));
        vcx.run_until_parked();
    }
    view.read_with(vcx, |changes, _| assert_eq!(changes.word_spans.computations, 32));
}
use super::*;
use chrono::Utc;

mod ref_targets;

#[gpui::test]
fn explanation_selection_extends_only_within_file_and_side(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Changes::new(state, cx)
    });
    window.update(cx, |changes, _, cx| {
        changes.select_explanation_line("a", SourceSide::New, 5, false, cx);
        changes.select_explanation_line("a", SourceSide::New, 3, true, cx);
        assert!(changes.explanation_line_selected("a", SourceSide::New, 4));
        changes.select_explanation_line("a", SourceSide::Old, 8, true, cx);
        assert!(!changes.explanation_line_selected("a", SourceSide::New, 4));
        assert!(changes.explanation_line_selected("a", SourceSide::Old, 8));
    }).unwrap();
}

#[gpui::test]
fn editing_staged_diff_comments_preserves_identity_and_cancellation(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Changes::new(state, cx)
    });
    window
        .update(cx, |changes, window, cx| {
            for side in [CommentSide::Old, CommentSide::New] {
                let original =
                    ReviewComment::new("new.rs", side, 7, "Original 🦀\nSecond line")
                        .renamed_from(Some("old.rs"));
                changes.state.update(cx, |state, _| {
                    state.add_review_comment("", original.clone())
                });
                changes.edit_comment(&original.id, window, cx);
                let draft = changes.draft.as_ref().unwrap();
                assert_eq!(draft.input.read(cx).text(), original.body);
                assert_eq!(draft_cite_path(draft), original.cite_path());
                assert!(draft.input.read(cx).focus_handle(cx).is_focused(window));
                let input = draft.input.clone();
                input.update(cx, |input, cx| input.set_text("Cancelled", cx));
                assert_eq!(changes.state.read(cx).review_comments("")[0], original);
                changes.cancel_draft(cx);
                assert_eq!(
                    changes.state.read(cx).review_comments(""),
                    std::slice::from_ref(&original)
                );

                changes.edit_comment(&original.id, window, cx);
                changes
                    .draft
                    .as_ref()
                    .unwrap()
                    .input
                    .clone()
                    .update(cx, |input, cx| {
                        input.set_text("  Revised\nMore detail  ", cx)
                    });
                changes.commit_draft(cx);
                let mut expected = original.clone();
                expected.body = "Revised\nMore detail".into();
                assert_eq!(
                    changes.state.read(cx).review_comments(""),
                    &[expected.clone()]
                );
                assert_ne!(
                    comment_state_key(&[original], None),
                    comment_state_key(&[expected.clone()], None)
                );
                changes.edit_comment(&expected.id, window, cx);
                let sent = changes
                    .state
                    .update(cx, |state, _| state.take_review_comments(""));
                assert!(comments::with_comments("", &sent).contains("Revised\n  More detail"));
                changes.commit_draft(cx);
                assert!(changes.state.read(cx).review_comments("").is_empty());
            }
        })
        .unwrap();
}

const PATCH: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 111..222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@ fn main
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    let x = 1;
 }
@@ -10,2 +11,2 @@
 // tail
-old_line
+new_line
diff --git a/added.txt b/added.txt
new file mode 100644
--- /dev/null
+++ b/added.txt
@@ -0,0 +1,2 @@
+first
+second
\\ No newline at end of file
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
diff --git a/img.png b/img.png
new file mode 100644
Binary files /dev/null and b/img.png differ
diff --git a/old_name.rs b/new_name.rs
similarity index 90%
rename from old_name.rs
rename to new_name.rs
";

#[test]
fn parses_files_hunks_and_lines() {
    let files = parse_patch(PATCH);
    assert_eq!(files.len(), 5);

    let main = &files[0];
    assert_eq!(main.path, "src/main.rs");
    assert_eq!(main.status, FileStatus::Modified);
    assert_eq!(main.hunks.len(), 2);
    assert_eq!(main.additions, 3);
    assert_eq!(main.deletions, 2);
    let h0 = &main.hunks[0];
    assert_eq!(h0.header, "@@ -1,4 +1,5 @@ fn main");
    assert_eq!(h0.lines.len(), 5);
    assert_eq!(h0.lines[0].kind, LineKind::Context);
    assert_eq!(h0.lines[0].old_no, Some(1));
    assert_eq!(h0.lines[0].new_no, Some(1));
    assert_eq!(h0.lines[1].kind, LineKind::Del);
    assert_eq!(h0.lines[1].old_no, Some(2));
    assert_eq!(h0.lines[1].new_no, None);
    assert_eq!(h0.lines[2].kind, LineKind::Add);
    assert_eq!(h0.lines[2].new_no, Some(2));
    assert_eq!(h0.lines[3].kind, LineKind::Add);
    assert_eq!(h0.lines[3].new_no, Some(3));
    // Closing context line: numbering advanced past the add/del block.
    assert_eq!(h0.lines[4].old_no, Some(3));
    assert_eq!(h0.lines[4].new_no, Some(4));
    // Second hunk restarts numbering from its header.
    assert_eq!(main.hunks[1].lines[0].old_no, Some(10));
    assert_eq!(main.hunks[1].lines[0].new_no, Some(11));
}

#[test]
fn detects_new_deleted_binary_and_renamed() {
    let files = parse_patch(PATCH);
    let added = &files[1];
    assert_eq!(added.status, FileStatus::Added);
    assert_eq!(added.additions, 2);
    // The no-newline marker rides as a Meta line.
    let last = added.hunks[0].lines.last().unwrap();
    assert_eq!(last.kind, LineKind::Meta);
    assert!(last.text.contains("No newline"));
    assert!(file_notices(added).iter().any(|n| n == "New file"));

    let deleted = &files[2];
    assert_eq!(deleted.status, FileStatus::Deleted);
    assert_eq!(deleted.deletions, 1);
    assert!(file_notices(deleted).iter().any(|n| n == "Deleted file"));

    let binary = &files[3];
    assert!(binary.binary);
    assert_eq!(binary.status, FileStatus::Added);
    assert!(binary.hunks.is_empty());
    assert!(file_notices(binary).iter().any(|n| n.contains("Binary")));

    let renamed = &files[4];
    assert_eq!(renamed.status, FileStatus::Renamed);
    assert_eq!(renamed.path, "new_name.rs");
    assert_eq!(renamed.old_path.as_deref(), Some("old_name.rs"));
    assert!(
        file_notices(renamed)
            .iter()
            .any(|n| n.contains("old_name.rs"))
    );
}

#[test]
fn empty_and_garbage_patches_parse_to_nothing() {
    assert!(parse_patch("").is_empty());
    assert!(parse_patch("not a diff\nat all\n").is_empty());
    // Truncated mid-hunk: keeps what parsed.
    let files = parse_patch("diff --git a/x b/x\n@@ -1,9 +1,9 @@\n ctx\n+add");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].hunks[0].lines.len(), 2);
    assert_eq!(files[0].additions, 1);
}

#[test]
fn quoted_and_spaced_paths() {
    let (old, new) = parse_git_paths("a/simple.rs b/simple.rs");
    assert_eq!((old.as_str(), new.as_str()), ("simple.rs", "simple.rs"));
    let (old, new) = parse_git_paths("\"a/with space.rs\" \"b/with space.rs\"");
    assert_eq!(old, "with space.rs");
    assert_eq!(new, "with space.rs");
}

#[test]
fn hunk_headers_parse_with_and_without_counts() {
    assert_eq!(parse_hunk_header("@@ -1,4 +2,5 @@"), Some((1, 2)));
    assert_eq!(parse_hunk_header("@@ -7 +9 @@ fn ctx"), Some((7, 9)));
    assert_eq!(parse_hunk_header("@@ garbage"), None);
}

fn submodule(expanded: bool, patch: &str) -> SubmoduleDiffSummary {
    SubmoduleDiffSummary {
        repository_path: "vendor/lib".into(),
        parent_repository_path: None,
        old_revision: Some("111111111111".into()),
        new_revision: Some("222222222222".into()),
        expanded,
        patch: patch.into(),
        files: vec![
            mast_proto::DiffFileSummary {
                path: "src/lib.rs".into(),
                old_path: None,
                status: "modified".into(),
                additions: 1,
                deletions: 1,
                binary: false,
            },
            mast_proto::DiffFileSummary {
                path: "README.md".into(),
                old_path: None,
                status: "modified".into(),
                additions: 1,
                deletions: 0,
                binary: false,
            },
        ],
        additions: 2,
        deletions: 1,
        truncated: false,
    }
}

const ROOT_WITH_GITLINK: &str = "\
diff --git a/vendor/lib b/vendor/lib
index 1111111..2222222 160000
--- a/vendor/lib
+++ b/vendor/lib
@@ -1 +1 @@
-Subproject commit 111111111111
+Subproject commit 222222222222
diff --git a/root.txt b/root.txt
@@ -1 +1 @@
-old
+new
";

const NESTED_PATCH: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
@@ -1 +1 @@
-old
+new
diff --git a/README.md b/README.md
@@ -0,0 +1 @@
+nested
";

const ROOT_WITH_DIRTY_GITLINK: &str = "\
diff --git a/vendor/lib b/vendor/lib
index 1111111..2222222 160000
--- a/vendor/lib
+++ b/vendor/lib
@@ -1 +1 @@
-Subproject commit 111111111111
+Subproject commit 222222222222-dirty
";

#[test]
fn expanded_submodule_replaces_gitlink_with_group_and_nested_files() {
    let (files, origins, groups, layout) =
        parse_diff_tree(ROOT_WITH_GITLINK, &[submodule(true, NESTED_PATCH)]);
    assert_eq!(groups.len(), 1);
    assert!(!groups[0].dirty);
    assert_eq!(logical_file_count(&layout), 3);
    assert!(matches!(
        layout[0],
        DiffTreeNode::Submodule { group: 0, .. }
    ));
    assert_eq!(files[1].path, "root.txt");
    assert_eq!(files[2].path, "vendor/lib/src/lib.rs");
    assert_eq!(origins[2].repository_path.as_deref(), Some("vendor/lib"));
    assert_eq!(origins[2].repository_relative_path, "src/lib.rs");

    let (rows, ranges, headers) = flatten_diff_tree(
        &files,
        &groups,
        &layout,
        &[],
        None,
        DiffMode::Unified,
        |_| false,
        |_| false,
    );
    assert_eq!(rows[0], DiffRow::SubmoduleHeader { group: 0 });
    assert!(ranges[0].is_none(), "root gitlink must be hidden");
    assert!(ranges[2].is_some());
    assert!(ranges[3].is_some());
    assert_eq!(headers[0], 0);

    let (collapsed_rows, collapsed_ranges, _) = flatten_diff_tree(
        &files,
        &groups,
        &layout,
        &[],
        None,
        DiffMode::Unified,
        |_| false,
        |_| true,
    );
    assert_eq!(collapsed_rows[0], DiffRow::SubmoduleHeader { group: 0 });
    assert!(collapsed_ranges[2].is_none());
    assert!(collapsed_ranges[3].is_none());
    assert!(collapsed_ranges[1].is_some(), "ordinary root file remains");
}

#[test]
fn expanded_submodule_preserves_dirty_parent_gitlink_marker() {
    let (_, _, clean_groups, _) =
        parse_diff_tree(ROOT_WITH_GITLINK, &[submodule(true, NESTED_PATCH)]);
    let (_, _, dirty_groups, dirty_layout) =
        parse_diff_tree(ROOT_WITH_DIRTY_GITLINK, &[submodule(true, NESTED_PATCH)]);

    assert!(!clean_groups[0].dirty);
    assert!(dirty_groups[0].dirty);
    assert!(matches!(
        dirty_layout.first(),
        Some(DiffTreeNode::Submodule { group: 0, .. })
    ));
}

#[test]
fn unavailable_or_empty_submodule_keeps_sha_only_gitlink_fallback() {
    for section in [submodule(false, NESTED_PATCH), submodule(true, "")] {
        let (files, _, groups, layout) = parse_diff_tree(ROOT_WITH_GITLINK, &[section]);
        assert!(groups.is_empty());
        assert_eq!(layout.first(), Some(&DiffTreeNode::File(0)));
        assert_eq!(files[0].path, "vendor/lib");
        assert!(
            files[0]
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .any(|line| line.text.contains("Subproject commit"))
        );
    }
}

#[test]
fn rows_flatten_to_line_granularity() {
    let files = parse_patch(PATCH);
    let (rows, ranges) = flatten_rows(&files, &[], None, DiffMode::Unified, |_| false);
    assert_eq!(ranges.len(), files.len());
    // Every file's span starts with its header…
    for (ix, range) in ranges.iter().enumerate() {
        assert_eq!(rows[range.start], DiffRow::FileHeader { file: ix as u32 });
        // …and spans exactly header + analytic body rows.
        assert_eq!(range.len(), 1 + body_row_count(&files[ix]));
    }
    // Spans tile the whole row vec.
    assert_eq!(ranges.last().unwrap().end, rows.len());

    // src/main.rs: header, 2 hunk headers, 8 lines, pad.
    let main_rows = &rows[ranges[0].clone()];
    assert_eq!(main_rows.len(), 1 + 2 + 8 + 1);
    assert_eq!(main_rows[1], DiffRow::HunkHeader { file: 0, hunk: 0 });
    // Flat line indices run across hunks (they key the highlight slot).
    let flats: Vec<u32> = main_rows
        .iter()
        .filter_map(|r| match r {
            DiffRow::Line { flat, .. } => Some(*flat),
            _ => None,
        })
        .collect();
    assert_eq!(flats, (0..8).collect::<Vec<u32>>());
    assert_eq!(*main_rows.last().unwrap(), DiffRow::BodyPad { file: 0 });

    // A collapsed file contributes its header row only.
    let (rows, ranges) = flatten_rows(&files, &[], None, DiffMode::Unified, |ix| ix == 0);
    assert_eq!(ranges[0].len(), 1);
    assert_eq!(rows[ranges[1].start], DiffRow::FileHeader { file: 1 });

    // Notices lead the body: the added file carries "New file".
    let added_rows = &rows[ranges[1].clone()];
    assert_eq!(added_rows[1], DiffRow::Notice { file: 1, notice: 0 });
}

#[test]
fn sticky_header_tracks_the_logical_top_row() {
    let ranges = vec![Some(0..4), Some(4..5), Some(5..10)];
    let headers = vec![0, 4, 5];

    assert_eq!(sticky_file_header(&[], &[], 0, 0.0), None);
    assert_eq!(sticky_file_header(&ranges, &headers, 0, 0.0), None);
    assert_eq!(
        sticky_file_header(&ranges, &headers, 0, 0.5),
        Some(StickyFileHeader {
            file_ix: 0,
            header_row: 0,
            next_header_row: Some(4),
        })
    );
    assert_eq!(
        sticky_file_header(&ranges, &headers, 2, 0.0),
        Some(StickyFileHeader {
            file_ix: 0,
            header_row: 0,
            next_header_row: Some(4),
        })
    );

    // Landing exactly on a new header hands ownership to that file; its
    // real row remains visible until it starts crossing the viewport.
    assert_eq!(sticky_file_header(&ranges, &headers, 4, 0.0), None);
    assert_eq!(
        sticky_file_header(&ranges, &headers, 4, 1.0),
        Some(StickyFileHeader {
            file_ix: 1,
            header_row: 4,
            next_header_row: Some(5),
        })
    );
    assert_eq!(sticky_file_header(&ranges, &headers, 5, 0.0), None);
    assert_eq!(
        sticky_file_header(&ranges, &headers, 8, 0.0),
        Some(StickyFileHeader {
            file_ix: 2,
            header_row: 5,
            next_header_row: None,
        })
    );
    assert_eq!(sticky_file_header(&ranges, &headers, 10, 0.0), None);

    // A submodule header between file sections pushes the previous sticky
    // file even though it has no file range of its own.
    let grouped_ranges = vec![Some(0..4), Some(5..9)];
    assert_eq!(
        sticky_file_header(&grouped_ranges, &[0, 4, 5], 2, 0.0),
        Some(StickyFileHeader {
            file_ix: 0,
            header_row: 0,
            next_header_row: Some(4),
        })
    );
    assert_eq!(
        sticky_file_header(&grouped_ranges, &[0, 4, 5], 4, 0.0),
        None
    );
}

#[test]
fn sticky_header_is_pushed_by_the_next_file() {
    assert_eq!(sticky_header_push_offset(None), 0.0);
    assert_eq!(sticky_header_push_offset(Some(80.0)), 0.0);
    assert_eq!(sticky_header_push_offset(Some(FILE_HEADER_HEIGHT)), 0.0);
    assert_eq!(
        sticky_header_push_offset(Some(FILE_HEADER_HEIGHT - 12.0)),
        -12.0
    );
    assert_eq!(sticky_header_push_offset(Some(0.0)), -FILE_HEADER_HEIGHT);
}

#[test]
fn sticky_header_uses_the_content_theme_in_dark_and_light() {
    use mast_theme::{AccentSelection, SurfacePreference};

    for (appearance, variant_id) in [
        (crate::theme::Appearance::Dark, "gruvbox-dark"),
        (crate::theme::Appearance::Light, "gruvbox-light"),
    ] {
        let opaque = Theme::for_selection(
            appearance,
            variant_id,
            AccentSelection::ThemeDefault,
            SurfacePreference::Opaque,
        );
        let opaque_paint = sticky_file_header_paint(&opaque);
        assert_eq!(opaque_paint.frost_tint, None, "{variant_id}");
        assert_eq!(
            opaque_paint.rest_bg,
            crate::theme::flatten(opaque.ink(0.025), opaque.bg),
            "{variant_id} opaque background"
        );
        assert_eq!(
            opaque_paint.hover_bg,
            crate::theme::flatten(opaque.element_hover, opaque.bg),
            "{variant_id} opaque hover"
        );
        assert_eq!(opaque_paint.border, opaque.border, "{variant_id} border");

        let frosted = Theme::for_selection(
            appearance,
            variant_id,
            AccentSelection::ThemeDefault,
            SurfacePreference::Frosted,
        );
        let frosted_paint = sticky_file_header_paint(&frosted);
        if frosted.is_frost() {
            let expected_alpha = match appearance {
                crate::theme::Appearance::Dark => STICKY_FILE_HEADER_TINT_ALPHA_DARK,
                crate::theme::Appearance::Light => STICKY_FILE_HEADER_TINT_ALPHA_LIGHT,
            };
            let tint = frosted.bg.opacity(expected_alpha);
            assert_eq!(
                frosted_paint.frost_tint,
                Some(tint),
                "{variant_id} content-plane tint"
            );
            assert_ne!(
                tint,
                frosted.glass_overlay(),
                "{variant_id} must not borrow the elevated overlay plane"
            );
            assert_eq!(tint.a, expected_alpha, "{variant_id} tint coverage");
            assert_eq!(
                frosted_paint.hover_bg,
                frosted.glass_hover(),
                "{variant_id} themed hover"
            );
        } else {
            assert_eq!(frosted_paint.frost_tint, None, "{variant_id}");
        }
        assert_eq!(
            frosted_paint.border, frosted.border,
            "{variant_id} frosted border"
        );
    }
}

#[test]
fn split_pairs_align_edits_and_strand_the_rest() {
    let files = parse_patch(PATCH);
    // src/main.rs hunk 0: context, −1, +1, +1, context. The edited line
    // pairs across; the extra addition is stranded on the right.
    assert_eq!(
        split_pairs(&files[0].hunks[0].lines),
        vec![
            (Some(0), Some(0)),
            (Some(1), Some(2)),
            (None, Some(3)),
            (Some(4), Some(4)),
        ]
    );
    // A pure add: every row is right-only, including the trailing
    // no-newline Meta line — it belongs to the side it follows, and its
    // row spans both columns at render.
    assert_eq!(
        split_pairs(&files[1].hunks[0].lines),
        vec![(None, Some(0)), (None, Some(1)), (None, Some(2))]
    );
    // A pure delete strands the left.
    assert_eq!(split_pairs(&files[2].hunks[0].lines), vec![(Some(0), None)]);
    assert!(split_pairs(&[]).is_empty());

    // `-a +b -c +d` is two one-line edits, not one four-line one: a
    // deletion arriving after additions opens a new block.
    let line = |kind| DiffLine {
        kind,
        old_no: Some(1),
        new_no: Some(1),
        text: String::new(),
    };
    let lines = [
        line(LineKind::Del),
        line(LineKind::Add),
        line(LineKind::Del),
        line(LineKind::Add),
    ];
    assert_eq!(
        split_pairs(&lines),
        vec![(Some(0), Some(1)), (Some(2), Some(3))]
    );
}

#[test]
fn no_newline_markers_keep_their_edit_paired() {
    // Both files lost their final newline: git writes the marker twice,
    // once per side. Neither may split the edit into one-sided rows.
    let both = "diff --git a/a.txt b/a.txt\n\
         --- a/a.txt\n\
         +++ b/a.txt\n\
         @@ -1 +1 @@\n\
         -old\n\
         \\ No newline at end of file\n\
         +new\n\
         \\ No newline at end of file\n";
    let files = parse_patch(both);
    let lines = &files[0].hunks[0].lines;
    assert_eq!(
        lines.iter().map(|line| line.kind).collect::<Vec<_>>(),
        vec![LineKind::Del, LineKind::Meta, LineKind::Add, LineKind::Meta]
    );
    // One aligned old/new row, then the two markers on one row of their
    // own — four lines read as two rows, not four.
    assert_eq!(
        split_pairs(lines),
        vec![(Some(0), Some(2)), (Some(1), Some(3))]
    );
    let full = split_pairs(lines);
    for cap in 0..=full.len() + 2 {
        assert_eq!(split_pairs_upto(lines, cap), full[..cap.min(full.len())]);
    }

    // Only the old file lacked one: the edit still pairs, and the lone
    // marker takes a row on its own side.
    let old_only = "diff --git a/a.txt b/a.txt\n\
         --- a/a.txt\n\
         +++ b/a.txt\n\
         @@ -1 +1 @@\n\
         -old\n\
         \\ No newline at end of file\n\
         +new\n";
    let files = parse_patch(old_only);
    assert_eq!(
        split_pairs(&files[0].hunks[0].lines),
        vec![(Some(0), Some(2)), (Some(1), None)]
    );
}

#[test]
fn split_flattening_pairs_rows_and_keeps_heights_analytic() {
    let files = parse_patch(PATCH);
    let (rows, ranges) = flatten_rows(&files, &[], None, DiffMode::Split, |_| false);
    assert_eq!(ranges.len(), files.len());
    assert_eq!(ranges.last().unwrap().end, rows.len());

    // src/main.rs: header, 2 hunk headers, 4 + 2 paired rows, pad — the
    // same 8 lines, two columns.
    let main_rows = &rows[ranges[0].clone()];
    assert_eq!(main_rows.len(), 1 + 2 + (4 + 2) + 1);
    assert_eq!(
        main_rows[2],
        DiffRow::SplitLine {
            file: 0,
            hunk: 0,
            left: Some(0),
            right: Some(0),
        }
    );
    assert_eq!(
        main_rows[4],
        DiffRow::SplitLine {
            file: 0,
            hunk: 0,
            left: None,
            right: Some(3),
        }
    );
    assert_eq!(*main_rows.last().unwrap(), DiffRow::BodyPad { file: 0 });
    // Pairing only ever merges rows, so split is never the taller layout.
    assert!(main_rows.len() < 1 + body_row_count(&files[0]));

    // Heights stay analytic — the fold tween needs no measurement.
    assert_eq!(
        body_height_with(&files[0], &[], None, DiffMode::Split, DIFF_LINE_HEIGHT),
        2.0 * HUNK_HEADER_HEIGHT + 6.0 * DIFF_LINE_HEIGHT + BODY_BOTTOM_PAD
    );
}

#[test]
fn capped_pairing_agrees_with_the_full_pairing_and_stays_bounded() {
    // The fold tween re-renders its stand-in every frame, so the capped
    // walk must be a true prefix of the full one — not an approximation.
    let lines = &parse_patch(PATCH)[0].hunks[0].lines;
    let full = split_pairs(lines);
    for cap in 0..=full.len() + 2 {
        assert_eq!(split_pairs_upto(lines, cap), full[..cap.min(full.len())]);
    }

    // A huge single-sided run must not be materialized to yield a few
    // rows: 20k deletions, 5 rows asked for, 5 rows built.
    let many: Vec<DiffLine> = (0..20_000u32)
        .map(|n| DiffLine {
            kind: LineKind::Del,
            old_no: Some(n + 1),
            new_no: None,
            text: String::new(),
        })
        .collect();
    let capped = split_pairs_upto(&many, 5);
    assert_eq!(capped.len(), 5);
    assert!(
        capped.capacity() < 100,
        "capacity tracks the cap, not the hunk"
    );
    assert_eq!(capped[4], (Some(4), None));
}

#[test]
fn a_split_row_offers_each_column_its_own_anchor() {
    let files = parse_patch(PATCH);
    let lines = &files[0].hunks[0].lines;
    // The paired edit cites the old line on the left, the new on the right.
    assert_eq!(
        pair_anchors(lines, (Some(1), Some(2))),
        [Some((CommentSide::Old, 2)), Some((CommentSide::New, 2))]
    );
    // A context row names one anchor, not the same one twice — otherwise
    // its card would be pushed into the body in duplicate. The caller
    // flattens, so the dropped duplicate reads as an empty slot.
    assert_eq!(
        pair_anchors(lines, (Some(0), Some(0))),
        [Some((CommentSide::New, 1)), None]
    );
    // A stranded side contributes nothing.
    assert_eq!(
        pair_anchors(lines, (None, Some(3))),
        [None, Some((CommentSide::New, 3))]
    );
}

#[test]
fn split_rows_carry_the_comments_of_both_columns() {
    let files = parse_patch(PATCH);
    // A context row must not stack the same card twice.
    let comment = ReviewComment::new("src/main.rs", CommentSide::New, 1, "why");
    let rows = body_rows(0, &files[0], &[comment], None, DiffMode::Split);
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(row, DiffRow::CommentCard { .. }))
            .count(),
        1
    );

    // Both sides of one paired row hang off that row, in column order.
    let staged = vec![
        ReviewComment::new("src/main.rs", CommentSide::Old, 2, "left"),
        ReviewComment::new("src/main.rs", CommentSide::New, 2, "right"),
    ];
    let rows = body_rows(0, &files[0], &staged, None, DiffMode::Split);
    let edit = rows
        .iter()
        .position(|row| matches!(row, DiffRow::SplitLine { left: Some(1), .. }))
        .unwrap();
    assert_eq!(rows[edit + 1], DiffRow::CommentCard { file: 0, card: 0 });
    assert_eq!(rows[edit + 2], DiffRow::CommentCard { file: 0, card: 1 });
}

#[test]
fn a_split_rows_right_column_is_never_a_deletion() {
    // The invariant the `+` placement rests on: only the right column is
    // hoverable, so every note a split row can start must cite the
    // post-change file. Were a deletion ever to land on the right, that
    // rule would quietly start filing notes against lines the agent
    // cannot edit.
    for file in parse_patch(PATCH) {
        for hunk in &file.hunks {
            for (_, right) in split_pairs(&hunk.lines) {
                let Some(line) = right.and_then(|ix| hunk.lines.get(ix as usize)) else {
                    continue;
                };
                assert_ne!(line.kind, LineKind::Del, "{:?}", line);
                assert!(matches!(
                    line_anchor(line),
                    None | Some((CommentSide::New, _))
                ));
            }
        }
    }
}

#[test]
fn truncate_caps_lines_and_appends_notice() {
    let mut file = parse_patch(PATCH).remove(0); // 2 hunks, 8 lines
    let untouched = file.clone();
    truncate_file_lines(&mut file, 10);
    assert_eq!(file, untouched, "under the cap: untouched");

    truncate_file_lines(&mut file, 6);
    let lines: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
    assert_eq!(lines, 6);
    assert_eq!(file.hunks.len(), 2);
    assert!(
        file_notices(&file)
            .iter()
            .any(|n| n.contains("first 6 of 8 lines"))
    );
    // body_height stays consistent with what actually renders.
    assert_eq!(
        body_height(&file),
        NOTICE_HEIGHT + 2.0 * HUNK_HEADER_HEIGHT + 6.0 * DIFF_LINE_HEIGHT + BODY_BOTTOM_PAD
    );

    // A cap below the first hunk's length drops later hunks entirely.
    let mut file = parse_patch(PATCH).remove(0);
    truncate_file_lines(&mut file, 3);
    assert_eq!(file.hunks.len(), 1);
    assert_eq!(file.hunks[0].lines.len(), 3);
}

#[test]
fn gutters_fit_the_largest_line_number() {
    let files = parse_patch(PATCH);
    // src/main.rs second hunk ends at old 11 / new 12.
    assert_eq!(files[0].max_line, 12);
    assert_eq!(gutter_width(&files[0]), GUTTER_WIDTH);

    // Every digit count keeps ≥6px clear of the accent bar on the left
    // of the number (digits×6.6 + 8px right pad + 6px gap), and the
    // column never shrinks below the classic 36px.
    let mut file = files[0].clone();
    for digits in 1..=7u32 {
        file.max_line = 10u32.pow(digits) - 1;
        let w = gutter_width(&file);
        assert!(w >= GUTTER_WIDTH);
        let left_gap = w - (digits as f32 * 6.6 + 8.0);
        assert!(
            left_gap >= 6.0,
            "{digits} digits: left gap {left_gap} < 6px"
        );
    }
    // 4 digits outgrow the classic column now (the old formula left
    // them 1.6px off the bar — visually touching).
    file.max_line = 9999;
    assert!(gutter_width(&file) > GUTTER_WIDTH);
    file.max_line = 27404;
    assert!(
        gutter_width(&file)
            > gutter_width(&{
                let mut f = file.clone();
                f.max_line = 9999;
                f
            })
    );

    // Truncation refits the gutter to what actually renders: the first
    // 3 lines are ctx(1,1) / del(2,·) / add(·,2) — max line 2.
    let mut file = files[0].clone();
    truncate_file_lines(&mut file, 3);
    assert_eq!(file.max_line, 2);
}

#[test]
fn horizontal_geometry_counts_tabs_and_unicode_columns() {
    assert_eq!(visual_columns("ab\tc"), 5);
    assert_eq!(visual_columns("界"), 2);
    assert_eq!(visual_columns("e\u{301}"), 1);

    let files = parse_patch("diff --git a/x b/x\n@@ -1 +1 @@\n-old\n+ab\t界\n");
    let geometry = DiffHorizontalGeometry::from_file(&files[0]);
    assert_eq!(geometry.max_code_columns, 6);
    assert_eq!(geometry.max_gutter_width, GUTTER_WIDTH);
}

#[test]
fn horizontal_content_width_compensates_for_local_gutters() {
    let metrics = DiffHorizontalMetrics {
        max_text_width: 240.0,
        max_gutter_width: 52.0,
    };
    let narrow = 36.0;
    let wide = 52.0;

    let unified_total = |gutter| {
        ACCENT_BAR_WIDTH + 2.0 * gutter + MARKER_WIDTH + metrics.unified_content_width(gutter)
    };
    assert_eq!(unified_total(narrow), unified_total(wide));

    let split_total = |gutter| {
        ACCENT_BAR_WIDTH + gutter + SPLIT_MARKER_WIDTH + metrics.split_content_width(gutter)
    };
    assert_eq!(split_total(narrow), split_total(wide));
}

/// Uses the native font backend, not TestAppContext's simulated metrics.
#[test]
fn native_diff_font_geometry() {
    // Windows headless mode uses NoopTextSystem. This regression needs
    // actual DirectWrite metrics, as it does CoreText/fontconfig elsewhere.
    let platform = gpui_platform::current_platform(!cfg!(windows));
    let text_system =
        gpui::WindowTextSystem::new(Arc::new(gpui::TextSystem::new(platform.text_system())));
    text_system
        .add_fonts(
            crate::typography::bundled_font_faces()
                .map(std::borrow::Cow::Borrowed)
                .collect(),
        )
        .unwrap();
    let mut theme = Theme::dark();
    theme.font_mono = "Geist".into();
    let wide = "W".repeat(100);
    let source = format!("{wide}\n{}\n\t漢字🙂e\u{301}\n", "WWW(WWW);".repeat(200));
    let patch = format!(
        "diff --git a/x.ts b/x.ts\n@@ -0,0 +1,3 @@\n+{}",
        source.trim_end_matches('\n').replace('\n', "\n+")
    );
    let files = parse_patch(&patch);
    let file = &files[0];
    let state = FileHorizontalState::new(file);
    let highlight = Arc::new(DiffHighlights {
        old: None,
        new: Some(Arc::new(
            mast_syntax::highlight(mast_syntax::HighlightRequest {
                source: &source,
                path: Some("x.ts"),
                fence_tag: None,
            })
            .unwrap(),
        )),
    });
    let mono = font(theme.font_mono.clone());
    let column = text_system
        .ch_advance(text_system.resolve_font(&mono), px(12.))
        .unwrap()
        .as_f32();
    let line = &file.hunks[0].lines[0];
    let width = text_system
        .shape_line(wide.into(), px(12.), &line_runs(line, None, &theme), None)
        .width()
        .as_f32();
    assert!(
        width > 100. * column * 1.1,
        "requires a real proportional font: {width} vs {}",
        100. * column
    );

    // Simulate plain -> excerpt -> full highlighting, including replacement
    // while an old cached Arc is still alive. Every paint must be reachable.
    for size in [12.5, 32., 8.] {
        theme.code_font_size = size;
        for highlights in [
            None,
            Some(highlight.clone()),
            Some(Arc::new(DiffHighlights {
                old: None,
                new: highlight.new.clone(),
            })),
        ] {
            let metrics = state.metrics(file, highlights.as_ref(), &theme, &text_system, 0);
            for line in &file.hunks[0].lines {
                let runs = line_runs(line, highlights.as_deref(), &theme);
                let painted = text_system
                    .shape_line(
                        line.text.clone().into(),
                        px(diff_text_size(&theme)),
                        &runs,
                        None,
                    )
                    .width()
                    .as_f32();
                assert!(metrics.max_text_width >= painted);
                assert!(
                    metrics.unified_content_width(gutter_width(file))
                        >= UNIFIED_CODE_PADDING_LEFT + painted
                );
                assert!(
                    metrics.split_content_width(gutter_width(file))
                        >= SPLIT_CODE_PADDING_LEFT + painted
                );
            }
            assert_eq!(
                state.metrics(file, highlights.as_ref(), &theme, &text_system, 0),
                metrics
            );
        }
    }
}

#[test]
fn horizontal_scroll_and_width_are_independent_per_file() {
    let files = parse_patch(
        "diff --git a/a b/a\n@@ -1 +1 @@\n-old\n+short\n\
         diff --git a/b b/b\n@@ -1 +1 @@\n-old\n+a much longer source line\n",
    );
    let states: Vec<_> = files.iter().map(FileHorizontalState::new).collect();
    assert_eq!(states.len(), 2);
    assert_eq!(states[0].geometry.max_code_columns, 5);
    assert_eq!(states[1].geometry.max_code_columns, 25);

    let row = DiffRow::Line {
        file: 0,
        hunk: 0,
        line: 0,
        flat: 0,
    };
    let folding = DiffRow::FoldingBody { file: 0 };
    let split = DiffRow::SplitLine {
        file: 1,
        hunk: 0,
        left: Some(0),
        right: Some(1),
    };
    let first = DiffCodeScrollContext {
        handle: states[row.file().unwrap()].scroll.clone(),
        prefix: "first".into(),
    };
    let second = DiffCodeScrollContext {
        handle: states[split.file().unwrap()].scroll.clone(),
        prefix: "second".into(),
    };
    first
        .slot("unified")
        .handle
        .set_offset(gpui::Point::new(px(-96.0), px(0.0)));
    assert_eq!(states[folding.file().unwrap()].scroll.offset().x, px(-96.0));
    assert_eq!(second.slot("old").handle.offset(), gpui::Point::default());

    second
        .slot("old")
        .handle
        .set_offset(gpui::Point::new(px(-48.0), px(0.0)));
    assert_eq!(second.slot("new").handle.offset().x, px(-48.0));
    assert_eq!(first.slot("unified").handle.offset().x, px(-96.0));
}

#[test]
fn horizontal_scroll_reset_returns_to_origin() {
    let handle = gpui::ScrollHandle::new();
    handle.set_offset(gpui::Point::new(px(-120.0), px(-18.0)));

    reset_horizontal_scroll(&handle);

    assert_eq!(handle.offset(), gpui::Point::default());
}

#[test]
fn horizontal_scroll_slots_share_offset_but_keep_unique_ids() {
    let context = DiffCodeScrollContext {
        handle: gpui::ScrollHandle::new(),
        prefix: "row-7".into(),
    };
    let old = context.slot("old");
    let new = context.slot("new");

    old.handle.set_offset(gpui::Point::new(px(-96.0), px(0.0)));

    assert_eq!(new.handle.offset(), old.handle.offset());
    assert_ne!(old.id, new.id);
}

#[test]
fn body_height_is_analytic() {
    let files = parse_patch(PATCH);
    let main = &files[0];
    let lines: usize = main.hunks.iter().map(|h| h.lines.len()).sum();
    assert_eq!(
        body_height(main),
        2.0 * HUNK_HEADER_HEIGHT + lines as f32 * DIFF_LINE_HEIGHT + BODY_BOTTOM_PAD
    );
    // Notices add height (added file: 1 notice + meta line inside hunk).
    let added = &files[1];
    assert_eq!(
        body_height(added),
        NOTICE_HEIGHT + HUNK_HEADER_HEIGHT + 3.0 * DIFF_LINE_HEIGHT + BODY_BOTTOM_PAD
    );
}

#[gpui::test]
fn go_to_line_scrolls_to_the_requested_diff_line(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| { gpui_base::init(cx); cx.set_global(Theme::dark()); });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Changes::new(state, cx)
    });
    window.update(cx, |changes, _, cx| {
        let files = parse_patch(PATCH);
        let (rows, ranges) = flatten_rows(&files, &[], None, DiffMode::Unified, |_| false);
        let horizontal = files.iter().map(FileHorizontalState::new).collect();
        let target = rows.iter().position(|row| matches!(row, DiffRow::Line { file: 0, hunk: 1, line: 2, .. })).unwrap();
        changes.list.reset_with_uniform_height(rows.len(), px(20.0));
        changes.rows = rows;
        changes.row_ranges = ranges.into_iter().map(Some).collect();
        changes.parsed = Some(ParsedDiff { key: "test".into(), truncated: false, additions: 0, deletions: 0, file_count: files.len(), horizontal, hunk_identities: Arc::new(Vec::new()), files: Arc::new(files), origins: Arc::new(Vec::new()), submodules: Arc::new(Vec::new()), layout: Arc::new(Vec::new()) });
        changes.go_to_line(12, cx);
        assert!(changes.list.logical_scroll_top().item_ix >= target);
        changes.list.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
        let replacement = changes.rows.iter().position(|row| matches!(row, DiffRow::Line { file: 0, hunk: 0, line: 2, .. })).unwrap();
        changes.go_to_line(2, cx);
        assert!(changes.list.logical_scroll_top().item_ix >= replacement);
    }).unwrap();
}

#[gpui::test]
fn explaining_focused_hunk_selects_it_without_sending(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| { gpui_base::init(cx); cx.set_global(Theme::dark()); });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Changes::new(state, cx)
    });
    window.update(cx, |changes, _, cx| {
        let files = parse_patch(PATCH);
        let (rows, ranges) = flatten_rows(&files, &[], None, DiffMode::Unified, |_| false);
        let hunk_row = rows.iter().position(|row| matches!(row, DiffRow::HunkHeader { file: 0, hunk: 0 })).unwrap();
        changes.list.reset_with_uniform_height(rows.len(), px(20.0));
        changes.rows = rows;
        changes.row_ranges = ranges.into_iter().map(Some).collect();
        let horizontal = files.iter().map(FileHorizontalState::new).collect();
        changes.parsed = Some(ParsedDiff { key: "test".into(), truncated: false, additions: 0, deletions: 0, file_count: files.len(), horizontal, hunk_identities: Arc::new(Vec::new()), files: Arc::new(files), origins: Arc::new(Vec::new()), submodules: Arc::new(Vec::new()), layout: Arc::new(Vec::new()) });
        changes.list.scroll_to(gpui::ListOffset { item_ix: hunk_row, offset_in_item: px(0.0) });
        changes.explain_focused_hunk(cx);
        assert_eq!(changes.explanation_hunk, Some((0, 0)));
        assert!(changes.explanation_selection.is_some());
        assert!(changes.explanation_panel.is_none());
    }).unwrap();
}

#[gpui::test]
fn explaining_selected_lines_sends_the_diff_to_the_owner(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| { gpui_base::init(cx); cx.set_global(Theme::dark()); });
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.work_outline_actions_override = Some(crate::work_outline::WorkOutlineActions::scripted(Arc::new(move |method, params| {
                recorded.lock().unwrap().push((method.to_owned(), params));
                Ok(serde_json::json!({"id":"message"}))
            })));
            state
        });
        let mut changes = Changes::new(state, cx);
        changes.owner = Some(ChangesTarget { chat_id: "owner".into(), device_id: "local".into(), cwd: None, checkout_id: None, branch: None, unconfirmed: None, exact_checkout: false, range: None });
        let files = parse_patch("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n");
        let (rows, _) = flatten_rows(&files, &[], None, DiffMode::Unified, |_| false);
        changes.rows = rows;
        let horizontal = files.iter().map(FileHorizontalState::new).collect();
        changes.parsed = Some(ParsedDiff { key: "test".into(), truncated: false, additions: 1, deletions: 1, file_count: 1, horizontal, hunk_identities: Arc::new(Vec::new()), files: Arc::new(files), origins: Arc::new(Vec::new()), submodules: Arc::new(Vec::new()), layout: Arc::new(Vec::new()) });
        changes
    });
    view.update(vcx, |changes, cx| {
        let old = changes.rows.iter().position(|row| matches!(row, DiffRow::Line { file: 0, hunk: 0, line: 0, .. })).unwrap();
        let new = changes.rows.iter().position(|row| matches!(row, DiffRow::Line { file: 0, hunk: 0, line: 1, .. })).unwrap();
        changes.select_unified_explanation_line("a.rs", SourceSide::Old, 1, old, false, cx);
        changes.select_unified_explanation_line("a.rs", SourceSide::New, 1, new, true, cx);
        changes.explain_selection(cx);
    });
    vcx.run_until_parked();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, mast_rpc::methods::QUEUE_MESSAGE);
    assert_eq!(calls[0].1["chatId"], "owner");
    let text = calls[0].1["text"].as_str().unwrap();
    assert!(text.contains("File: a.rs\nSide: mixed\nLines: old:1 → new:1"));
    assert!(text.ends_with("-old\n+new\n"));
}

#[gpui::test]
fn explaining_selected_lines_prefers_matching_review_chat(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| { gpui_base::init(cx); cx.set_global(Theme::dark()); });
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.local_device_id = Some("local".into());
            state.chats = vec![serde_json::from_value(serde_json::json!({"id":"owner", "deviceId":"local", "spaceId":"space", "archived":false, "createdAt":"2026-07-19T12:00:00Z"})).unwrap()];
            let artifact = serde_json::from_value(serde_json::json!({"id":"review", "spaceId":"space", "deviceId":"local", "chatId":"review-chat", "kind":"review"})).unwrap();
            state.set_test_artifacts(mast_proto::ArtifactsSnapshot { space_id: "space".into(), revision: 1, artifacts: vec![artifact] });
            state.work_outline_actions_override = Some(crate::work_outline::WorkOutlineActions::scripted(Arc::new(move |method, params| {
                recorded.lock().unwrap().push((method.to_owned(), params));
                if method == mast_rpc::methods::ARTIFACT_CONTENT { Ok(serde_json::json!({"artifactId":"review", "content":"{\"target\":{\"kind\":\"pr\",\"repo\":\"o/r\",\"number\":1,\"headSha\":\"head\",\"baseSha\":\"base\"}}"})) }
                else { Ok(serde_json::json!({"id":"message"})) }
            })));
            state
        });
        let mut changes = Changes::new(state, cx);
        changes.owner = Some(ChangesTarget { chat_id: "owner".into(), device_id: "local".into(), cwd: None, checkout_id: None, branch: None, unconfirmed: None, exact_checkout: false, range: None });
        changes.review_target = Some("pr:o/r#1".into());
        let files = parse_patch("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n");
        let horizontal = files.iter().map(FileHorizontalState::new).collect();
        changes.parsed = Some(ParsedDiff { key: "test".into(), truncated: false, additions: 1, deletions: 1, file_count: 1, horizontal, hunk_identities: Arc::new(Vec::new()), files: Arc::new(files), origins: Arc::new(Vec::new()), submodules: Arc::new(Vec::new()), layout: Arc::new(Vec::new()) });
        changes
    });
    view.update(vcx, |changes, cx| {
        changes.select_explanation_line("a.rs", SourceSide::New, 1, false, cx);
        changes.explain_selection(cx);
    });
    vcx.run_until_parked();
    let calls = calls.lock().unwrap();
    let send = calls.iter().find(|(method, _)| method == mast_rpc::methods::QUEUE_MESSAGE).unwrap();
    assert_eq!(send.1["chatId"], "review-chat");
}

fn diff(checkout: &str, device: &str, cwd: &str, patch: &str) -> CheckoutDiff {
    CheckoutDiff {
        checkout_id: checkout.into(),
        device_id: device.into(),
        cwd: cwd.into(),
        patch: patch.into(),
        files: Vec::new(),
        submodules: Vec::new(),
        additions: 0,
        deletions: 0,
        truncated: false,
        checksum: format!("sum-{}", patch.len()),
        updated_at: Utc::now(),
    }
}

fn chat(checkout: Option<&str>, device: &str, cwd: Option<&str>) -> Chat {
    Chat {
        id: "c1".into(),
        device_id: device.into(),
        title: None,
        archived: false,
        cwd: cwd.map(Into::into),
        branch: None,
        checkout_id: checkout.map(Into::into),
        source_context: None,
        config: None,
        last_message_preview: None,
        last_message_at: None,
        created_at: Utc::now(),
        harness_session_id: None,
        harness_session_cwd: None,
        actual_model: None,
        parent_chat_id: None,
        space_id: None,
        last_seen_at: None,
        room_gen: None,
        linked_pr_url: None,
        linked_pr_source: None,
        linked_ticket_id: None,
        linked_ticket_source: None,
    }
}

#[test]
fn nested_highlight_request_uses_repository_and_local_path() {
    let diff = diff("co", "dev", "/workspace", ROOT_WITH_GITLINK);
    let origin = FileOrigin {
        repository_relative_path: "src/lib.rs".into(),
        repository_path: Some("vendor/lib".into()),
        nesting_depth: 1,
    };
    let request = file_diff_text_request(
        &diff,
        &origin,
        "branch".into(),
        Some("main".into()),
        Some("chat".into()),
        None,
        None,
    );
    assert_eq!(request.path, "src/lib.rs");
    assert_eq!(request.repository_path.as_deref(), Some("vendor/lib"));
    assert_eq!(request.diff_checksum, diff.checksum);
}

#[test]
fn diff_resolution_prefers_checkout_id_then_cwd() {
    let diffs = vec![
        diff("co-1", "dev-a", "/repo/one", "x"),
        diff("co-2", "dev-b", "/repo/two", "y"),
    ];
    // checkout_id match wins even when cwd points elsewhere.
    let c = chat(Some("co-2"), "dev-a", Some("/repo/one"));
    assert_eq!(resolve_diff(&diffs, &c).unwrap().checkout_id, "co-2");
    // Unknown checkout falls back to device+cwd.
    let c = chat(Some("co-9"), "dev-a", Some("/repo/one"));
    assert_eq!(resolve_diff(&diffs, &c).unwrap().checkout_id, "co-1");
    // A matching path on another device is never accepted.
    let c = chat(None, "dev-z", Some("/repo/two"));
    assert!(resolve_diff(&diffs, &c).is_none());
    // Nothing to go on.
    let c = chat(None, "dev-a", None);
    assert!(resolve_diff(&diffs, &c).is_none());
    let c = chat(None, "dev-a", Some("/elsewhere"));
    assert!(resolve_diff(&diffs, &c).is_none());
}

#[test]
fn owned_target_never_falls_across_devices_or_worktrees() {
    let diffs = vec![
        diff("co-a", "device-a", "/repo/shared", "a"),
        diff("co-b", "device-b", "/repo/shared", "b"),
    ];
    let target = ChangesTarget {
        unconfirmed: None,
        exact_checkout: false,
        range: None,
        chat_id: "chat-a".into(),
        device_id: "device-a".into(),
        cwd: Some("/repo/shared".into()),
        checkout_id: None,
        branch: Some("feature/a".into()),
    };
    assert_eq!(target.resolve(&diffs).unwrap().checkout_id, "co-a");

    let target = ChangesTarget {
        unconfirmed: None,
        exact_checkout: false,
        range: None,
        chat_id: "chat-b".into(),
        device_id: "device-b".into(),
        cwd: Some("/repo/shared".into()),
        checkout_id: Some("co-b".into()),
        branch: Some("feature/b".into()),
    };
    assert_eq!(target.resolve(&diffs).unwrap().patch, "b");
}

#[test]
fn watch_request_scopes_to_expected_checkout_and_cwd() {
    let params =
        watch_checkout_params(Some("remote-device"), Some("checkout-a"), Some("/repo/a"));
    assert_eq!(params["targetDeviceId"], "remote-device");
    assert_eq!(params["checkoutId"], "checkout-a");
    assert_eq!(params["cwd"], "/repo/a");
}

#[test]
fn scope_labels_describe_actual_worktree_and_turn_boundaries() {
    assert_eq!(
        DiffScope::WorkingTree.label(),
        "All changes in this worktree"
    );
    assert_eq!(
        DiffScope::LatestTurn.label(),
        "Checkout changes since this chat’s latest turn"
    );
}

#[test]
fn context_names_are_derived_from_the_owned_path() {
    let target = ChangesTarget {
        unconfirmed: None,
        exact_checkout: false,
        range: None,
        chat_id: "chat".into(),
        device_id: "device".into(),
        cwd: Some("/projects/zeron/.worktrees/feature-123".into()),
        checkout_id: Some("checkout".into()),
        branch: Some("feature/123".into()),
    };
    assert_eq!(
        target.repo_and_worktree(),
        ("zeron".into(), "feature-123".into())
    );
}

/// Acceptance 15: a bound Chat's Changes pane lists primary + members
/// with their branch/HEAD (from the validation reply fetched on open) and
/// diffs the picked member through the scoped `{cwd, checkoutId}` watch.
/// Unbound Chats keep their own checkout.
#[gpui::test]
fn bound_chats_diff_the_selected_member_repository(cx: &mut gpui::TestAppContext) {
    use crate::work_outline::execution::fixture;
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
    });
    let calls = std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink = calls.clone();
    let (changes, vcx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            *sink.lock().unwrap() = Some(fixture::bound_state(&mut state));
            state
        });
        Changes::for_chat(state, "chat-ui".into(), cx)
    });
    vcx.run_until_parked();
    let calls = calls.lock().unwrap().clone().unwrap();
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "refreshed once on open");
        assert_eq!(
            calls[0].0,
            mast_rpc::methods::VALIDATE_EXECUTION_CONFIGURATION
        );
        assert_eq!(calls[0].1["chatId"], "chat-ui");
    }
    changes.update(vcx, |changes, cx| changes.sync(cx));
    changes.read_with(vcx, |changes, _| {
        let owner = changes.owner.as_ref().unwrap();
        assert_eq!(owner.cwd.as_deref(), Some("/work/alpha/ui"));
        assert_eq!(owner.checkout_id.as_deref(), Some("checkout:repo:ui"));
        assert_eq!(owner.branch.as_deref(), Some("feature/ui"));
    });
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-execution-repositories").is_some());
    assert!(vcx.debug_bounds("changes-execution-repo-1").is_some());

    // Picking flagship retargets the diff watch to its checkout.
    let flagship = vcx
        .debug_bounds("changes-execution-repo-1")
        .unwrap()
        .center();
    vcx.simulate_click(flagship, gpui::Modifiers::default());
    vcx.run_until_parked();
    changes.read_with(vcx, |changes, _| {
        let owner = changes.owner.as_ref().unwrap();
        assert_eq!(owner.cwd.as_deref(), Some("/work/alpha/flagship"));
        assert_eq!(owner.checkout_id.as_deref(), Some("checkout:repo:flagship"));
        assert_eq!(owner.branch, None, "detached HEAD");
        assert_eq!(
            watch_checkout_params(None, owner.checkout_id.as_deref(), owner.cwd.as_deref()),
            serde_json::json!({
                "checkoutId": "checkout:repo:flagship",
                "cwd": "/work/alpha/flagship",
            })
        );
    });
    assert_eq!(calls.lock().unwrap().len(), 1, "no refetch on selection");

    // An unbound Chat keeps its own checkout and shows no selector.
    let state = changes.read_with(vcx, |changes, _| changes.state.clone());
    let (unbound, vcx) = vcx.add_window_view(|_, cx| {
        Changes::for_chat(
            state,
            crate::work_outline::store::fixture_ids::CHAT_LOOSE_A.into(),
            cx,
        )
    });
    vcx.run_until_parked();
    unbound.read_with(vcx, |changes, _| {
        let owner = changes.owner.as_ref().unwrap();
        assert_eq!(owner.cwd.as_deref(), Some("/work/alpha"));
        assert_eq!(owner.checkout_id.as_deref(), Some("checkout:root"));
    });
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-execution-repositories").is_none());
}

/// A member validation did not confirm shows no diff and no actions,
/// with the validation remediation, never the diff of whatever sits at
/// the configured path; a confirmed member never falls back by cwd.
#[gpui::test]
fn unconfirmed_members_show_no_diff_and_explain_why(cx: &mut gpui::TestAppContext) {
    use crate::work_outline::execution::fixture;
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
    });
    let pruned = mast_proto::ValidateExecutionConfigurationReply::from_result(
        None,
        Err(vec![mast_proto::ExecutionValidationError::WorktreePruned {
            repo_id: "repo:ui".into(),
            worktree_id: "wt".into(),
            path: "/work/alpha/ui".into(),
        }]),
    );
    let reply = serde_json::to_value(&pruned).unwrap();
    let (changes, vcx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            fixture::bound_state(&mut state);
            state.work_outline_actions_override =
                Some(crate::work_outline::WorkOutlineActions::scripted(
                    std::sync::Arc::new(move |_, _| Ok(reply.clone())),
                ));
            state
        });
        Changes::for_chat(state, "chat-ui".into(), cx)
    });
    vcx.run_until_parked();
    changes.update(vcx, |changes, cx| changes.sync(cx));
    changes.read_with(vcx, |changes, _| {
        let owner = changes.owner.as_ref().unwrap();
        assert_eq!(owner.cwd, None, "no cwd fallback to the configured path");
        assert_eq!(owner.checkout_id, None);
        let reason = owner.unconfirmed.as_deref().unwrap();
        assert!(
            reason.contains(pruned.errors[0].remediation.as_str()),
            "{reason}"
        );
        // Even a diff for the configured path is never resolved.
        let diffs = vec![diff("co-ui", "d", "/work/alpha/ui", "x")];
        assert!(owner.resolve(&diffs).is_none());
        assert!(!changes.started, "no diff watch");
    });
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-member-unconfirmed").is_some());
    assert!(vcx.debug_bounds("changes-execution-repositories").is_some());
    changes.update(vcx, |changes, cx| {
        let _ = changes.render_header_controls(cx);
    });

    // A confirmed member matches by checkout id only.
    let target = ChangesTarget {
        chat_id: "chat-ui".into(),
        device_id: "d".into(),
        cwd: Some("/work/alpha/ui".into()),
        checkout_id: Some("checkout:repo:ui".into()),
        branch: None,
        unconfirmed: None,
        exact_checkout: true,
        range: None,
    };
    let diffs = vec![diff("co-other", "d", "/work/alpha/ui", "x")];
    assert!(target.resolve(&diffs).is_none());
}

#[gpui::test]
fn changes_surface_follows_only_its_owning_chat(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
    });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            let mut first = chat(Some("checkout-a"), "device", Some("/repo/a"));
            first.id = "chat-a".into();
            first.branch = Some("branch-a".into());
            let mut second = chat(Some("checkout-b"), "device", Some("/repo/b"));
            second.id = "chat-b".into();
            state.chats = vec![first, second];
            state.selected_chat = Some("chat-a".into());
            state
        });
        Changes::for_chat(state, "chat-a".into(), cx)
    });
    window
        .update(cx, |changes, _, cx| {
            changes.state.update(cx, |state, cx| {
                state.selected_chat = Some("chat-b".into());
                cx.notify();
            });
            changes.sync(cx);
            assert_eq!(changes.owner_chat_id(), Some("chat-a"));
            assert_eq!(changes.scoped_cwd(cx).as_deref(), Some("/repo/a"));

            changes.state.update(cx, |state, cx| {
                let chat = state
                    .chats
                    .iter_mut()
                    .find(|chat| chat.id == "chat-a")
                    .unwrap();
                chat.cwd = Some("/repo/a-updated".into());
                chat.checkout_id = Some("checkout-a-updated".into());
                cx.notify();
            });
            changes.sync(cx);
            assert_eq!(changes.scoped_cwd(cx).as_deref(), Some("/repo/a-updated"));
            assert_eq!(
                changes.owner.as_ref().unwrap().checkout_id.as_deref(),
                Some("checkout-a-updated")
            );
        })
        .unwrap();
}

#[test]
fn phases() {
    assert_eq!(diff_phase(None), DiffPhase::Preparing);
    let clean = diff("co", "d", "/w", "  \n");
    assert_eq!(diff_phase(Some(&clean)), DiffPhase::Clean);
    let full = diff("co", "d", "/w", "diff --git a/x b/x\n");
    assert_eq!(diff_phase(Some(&full)), DiffPhase::List);
    // Engine may report files without patch text (truncation edge).
    let mut summarized = diff("co", "d", "/w", "");
    summarized.files.push(mast_proto::DiffFileSummary {
        path: "x".into(),
        old_path: None,
        status: "modified".into(),
        additions: 1,
        deletions: 0,
        binary: false,
    });
    assert_eq!(diff_phase(Some(&summarized)), DiffPhase::List);

    // A route-independent close-out inspector still waits for its scoped
    // RPC when there is no selected chat; an ordinary empty canvas does
    // not spin forever.
    assert_eq!(pane_diff_phase(true, true, None), DiffPhase::Preparing);
    assert_eq!(pane_diff_phase(true, false, None), DiffPhase::Clean);
    assert!(shows_scoped_error(true, true, DiffScope::WorkingTree));
    assert!(!shows_scoped_error(true, false, DiffScope::WorkingTree));
}

#[test]
fn header_label_pluralizes() {
    assert_eq!(uncommitted_label(0), "0 Uncommitted changes");
    assert_eq!(uncommitted_label(1), "1 Uncommitted change");
    assert_eq!(uncommitted_label(4), "4 Uncommitted changes");
}

#[test]
fn scope_labels_and_clean_messages() {
    assert_eq!(
        scope_label(DiffScope::WorkingTree, 2, None),
        "2 Uncommitted changes"
    );
    assert_eq!(
        scope_label(DiffScope::Branch, 1, Some("main")),
        "1 Changed file vs main"
    );
    assert_eq!(scope_label(DiffScope::Branch, 3, None), "3 Changed files");
    assert_eq!(
        scope_label(DiffScope::LatestTurn, 2, None),
        "2 Changed files this turn"
    );
    assert_eq!(
        clean_message(DiffScope::WorkingTree, None),
        "No uncommitted changes"
    );
    assert_eq!(
        clean_message(DiffScope::Branch, Some("develop")),
        "No changes vs develop"
    );
    assert_eq!(
        clean_message(DiffScope::LatestTurn, None),
        "No changes this turn"
    );
}

#[test]
fn base_ref_defaults_to_repo_default_then_main() {
    let branches =
        |names: &[&str]| -> Vec<String> { names.iter().map(|n| n.to_string()).collect() };
    // Engine order puts the repo default first — take it when it isn't
    // the checked-out branch itself.
    let b = branches(&["main", "feature"]);
    assert_eq!(
        default_base_ref(&b, Some("feature")).as_deref(),
        Some("main")
    );
    // No origin/HEAD: engine "default" is the current branch — fall
    // through to main/master.
    let b = branches(&["feature", "main"]);
    assert_eq!(
        default_base_ref(&b, Some("feature")).as_deref(),
        Some("main")
    );
    let b = branches(&["feature", "master"]);
    assert_eq!(
        default_base_ref(&b, Some("feature")).as_deref(),
        Some("master")
    );
    // No main/master: any branch that isn't the current one.
    let b = branches(&["feature", "develop"]);
    assert_eq!(
        default_base_ref(&b, Some("feature")).as_deref(),
        Some("develop")
    );
    // Checked out ON main: comparing main with itself is the honest
    // default (empty branch diff).
    let b = branches(&["main", "feature"]);
    assert_eq!(default_base_ref(&b, Some("main")).as_deref(), Some("main"));
    // Single-branch repo, and empty list.
    let b = branches(&["main"]);
    assert_eq!(default_base_ref(&b, Some("main")).as_deref(), Some("main"));
    assert_eq!(default_base_ref(&[], Some("main")), None);
}

#[test]
fn scope_modes_are_wire_stable() {
    // `mode` is the GetCheckoutDiff wire contract — engine matches on it.
    assert_eq!(DiffScope::WorkingTree.mode(), "workingTree");
    assert_eq!(DiffScope::Branch.mode(), "branch");
    assert_eq!(DiffScope::LatestTurn.mode(), "turn");
    assert_eq!(DiffScope::default(), DiffScope::WorkingTree);
}

#[test]
fn diff_scope_menu_keeps_history_as_a_separate_surface() {
    assert_eq!(
        DiffScope::ALL,
        [
            DiffScope::WorkingTree,
            DiffScope::Branch,
            DiffScope::LatestTurn,
        ]
    );
    assert!(!DiffScope::ALL.contains(&DiffScope::History));
}

#[test]
fn diff_frames_replace_lists_and_upsert_singles() {
    let mut diffs = Vec::new();
    let one = diff("co-1", "d", "/w", "p1");
    // Single frame inserts.
    assert!(apply_diff_frame(
        &mut diffs,
        serde_json::to_value(&one).unwrap()
    ));
    assert_eq!(diffs.len(), 1);
    // Identical frame is a no-op.
    assert!(!apply_diff_frame(
        &mut diffs,
        serde_json::to_value(&one).unwrap()
    ));
    // Same checkout upserts in place.
    let mut updated = one.clone();
    updated.patch = "p2".into();
    assert!(apply_diff_frame(
        &mut diffs,
        serde_json::to_value(&updated).unwrap()
    ));
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].patch, "p2");
    // List frame replaces wholesale.
    let two = diff("co-2", "d", "/x", "q");
    assert!(apply_diff_frame(
        &mut diffs,
        serde_json::to_value(vec![two.clone()]).unwrap()
    ));
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].checkout_id, "co-2");
    // Malformed frames change nothing.
    assert!(!apply_diff_frame(
        &mut diffs,
        serde_json::json!({"nope": true})
    ));
    assert_eq!(diffs[0].checkout_id, "co-2");
}

#[test]
fn full_diff_highlights_map_old_new_and_context_by_source_line() {
    let old_source = "export function old(value: string) {\n    return value.trim();\n}\n";
    let new_source = "export function new(value: string) {\n    return value.trim();\n}\n";
    let parse = |source| {
        Arc::new(
            mast_syntax::highlight(mast_syntax::HighlightRequest {
                source,
                path: Some("src/derive.ts"),
                fence_tag: None,
            })
            .unwrap(),
        )
    };
    let highlights = DiffHighlights {
        old: Some(parse(old_source)),
        new: Some(parse(new_source)),
    };
    let deleted = DiffLine {
        kind: LineKind::Del,
        old_no: Some(1),
        new_no: None,
        text: "export function old(value: string) {".into(),
    };
    let added = DiffLine {
        kind: LineKind::Add,
        old_no: None,
        new_no: Some(1),
        text: "export function new(value: string) {".into(),
    };
    let context = DiffLine {
        kind: LineKind::Context,
        old_no: Some(2),
        new_no: Some(2),
        text: "    return value.trim();".into(),
    };
    assert_eq!(
        highlights.source_ref(&deleted),
        Some(SourceLineRef {
            side: SourceSide::Old,
            line_number: 1
        })
    );
    assert_eq!(
        highlights.source_ref(&added),
        Some(SourceLineRef {
            side: SourceSide::New,
            line_number: 1
        })
    );
    assert_eq!(
        highlights.source_ref(&context),
        Some(SourceLineRef {
            side: SourceSide::New,
            line_number: 2
        })
    );
    assert!(
        highlights
            .spans(&deleted)
            .iter()
            .any(|span| span.kind == mast_syntax::HighlightKind::Function)
    );
    assert!(
        highlights
            .spans(&added)
            .iter()
            .any(|span| span.kind == mast_syntax::HighlightKind::Function)
    );
}

/// The regression this guards: rendering the diff at the raw shared
/// setting silently enlarged it from 12.0 to 12.5 on a fresh install.
#[test]
fn the_default_code_font_size_reproduces_the_historical_diff_size() {
    let theme = Theme::dark();
    assert_eq!(
        theme.code_font_size,
        crate::typography::CODE_FONT_SIZE_DEFAULT
    );
    assert_eq!(diff_text_size(&theme), DIFF_TEXT_SIZE);
    assert_eq!(diff_line_height(&theme), DIFF_LINE_HEIGHT);
}

#[test]
fn scaled_diff_sizes_keep_their_proportions_and_stay_clamped() {
    let mut theme = Theme::dark();
    theme.code_font_size = 2.0 * crate::typography::CODE_FONT_SIZE_DEFAULT;
    assert_eq!(diff_text_size(&theme), 2.0 * DIFF_TEXT_SIZE);
    assert_eq!(diff_line_height(&theme), 2.0 * DIFF_LINE_HEIGHT);

    theme.code_font_size = crate::typography::FONT_SIZE_MAX;
    assert!(diff_text_size(&theme) <= crate::typography::FONT_SIZE_MAX);
    assert!(diff_text_size(&theme) >= crate::typography::FONT_SIZE_MIN);
}

#[test]
fn split_line_runs_use_affected_old_and_new_documents() {
    let theme = Theme::dark();
    for (path, source, required) in [
        (
            "src/card.tsx",
            "const view: JSX.Element = <main id=\"app\" />;",
            mast_syntax::HighlightKind::Tag,
        ),
        (
            "src/Greeter.kt",
            "fun greet(name: String) = println(name)",
            mast_syntax::HighlightKind::Function,
        ),
        (
            "Dockerfile",
            "RUN echo \"hello\"",
            mast_syntax::HighlightKind::Function,
        ),
    ] {
        let document = Arc::new(
            mast_syntax::highlight(mast_syntax::HighlightRequest {
                source,
                path: Some(path),
                fence_tag: None,
            })
            .unwrap(),
        );
        let highlights = DiffHighlights {
            old: Some(document.clone()),
            new: Some(document),
        };
        for line in [
            DiffLine {
                kind: LineKind::Del,
                old_no: Some(1),
                new_no: None,
                text: source.into(),
            },
            DiffLine {
                kind: LineKind::Add,
                old_no: None,
                new_no: Some(1),
                text: source.into(),
            },
        ] {
            assert!(
                highlights
                    .spans(&line)
                    .iter()
                    .any(|span| span.kind == required),
                "missing {required:?} for {path} on {:?}",
                line.kind
            );
            let runs = line_runs(&line, Some(&highlights), &theme);
            assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), source.len());
            assert!(
                runs.iter()
                    .any(|run| run.color == render::token_color(required, &theme)),
                "split runs dropped {required:?} for {path} on {:?}",
                line.kind
            );
        }
    }
}

#[test]
fn excerpt_parses_old_and_new_hunks_as_separate_documents() {
    let file = FileDiff {
        path: "src/lib.rs".into(),
        old_path: None,
        status: FileStatus::Modified,
        binary: false,
        notices: vec![],
        hunks: vec![Hunk {
            header: "@@ -1,3 +1,3 @@".into(),
            lines: vec![
                DiffLine {
                    kind: LineKind::Context,
                    old_no: Some(1),
                    new_no: Some(1),
                    text: "/* start".into(),
                },
                DiffLine {
                    kind: LineKind::Del,
                    old_no: Some(2),
                    new_no: None,
                    text: "old body".into(),
                },
                DiffLine {
                    kind: LineKind::Add,
                    old_no: None,
                    new_no: Some(2),
                    text: "new body".into(),
                },
                DiffLine {
                    kind: LineKind::Context,
                    old_no: Some(3),
                    new_no: Some(3),
                    text: "end */".into(),
                },
            ],
        }],
        additions: 1,
        deletions: 1,
        max_line: 3,
    };
    let highlights = excerpt_highlights(&file, Lang::Rust).expect("excerpt");
    let deleted = &file.hunks[0].lines[1];
    let added = &file.hunks[0].lines[2];
    assert!(
        highlights
            .spans(deleted)
            .iter()
            .any(|span| span.kind == mast_syntax::HighlightKind::Comment)
    );
    assert!(
        highlights
            .spans(added)
            .iter()
            .any(|span| span.kind == mast_syntax::HighlightKind::Comment)
    );
}

#[test]
fn mismatched_full_sources_are_rejected_atomically() {
    let file = FileDiff {
        path: "src/lib.rs".into(),
        old_path: None,
        status: FileStatus::Modified,
        binary: false,
        notices: vec![],
        hunks: vec![Hunk {
            header: "@@ -1 +1 @@".into(),
            lines: vec![
                DiffLine {
                    kind: LineKind::Del,
                    old_no: Some(1),
                    new_no: None,
                    text: "let old = 1;".into(),
                },
                DiffLine {
                    kind: LineKind::Add,
                    old_no: None,
                    new_no: Some(1),
                    text: "let new = 2;".into(),
                },
            ],
        }],
        additions: 1,
        deletions: 1,
        max_line: 1,
    };
    let response = mast_proto::CheckoutFileDiffText {
        diff_checksum: "sum".into(),
        old_text: Some("let old = 1;\n".into()),
        new_text: Some("different snapshot\n".into()),
        old_content_hash: None,
        new_content_hash: None,
        binary: false,
        truncated: false,
        stale: false,
    };
    assert!(!sources_match_patch(&file, &response));
    assert!(full_highlights(&file, Lang::Rust, &response).is_none());
}

// -----------------------------------------------------------------------
// Chat diff targets (agent worktree)
// -----------------------------------------------------------------------

const WS: &str = "/work/ws";
const WT_TICKET: &str = "/work/ws/.worktrees/flagship/eng-3012-kit";
const WT_OTHER: &str = "/work/ws/.worktrees/flagship/spike";
type RpcCalls = std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>;

fn wt_chat() -> Chat {
    let mut chat = chat(Some("checkout:root"), "d", Some(WS));
    chat.id = "chat-wt".into();
    chat.branch = Some("main".into());
    chat.linked_ticket_id = Some("ENG-3012".into());
    chat
}

fn target_view(
    path: &str,
    checkout_id: &str,
    branch: Option<&str>,
    selected: bool,
) -> mast_proto::ChatDiffTargetView {
    mast_proto::ChatDiffTargetView {
        path: path.into(),
        checkout_id: checkout_id.into(),
        source: mast_proto::ChatDiffTargetSource::Agent,
        selected,
        repo_name: "flagship".into(),
        branch: branch.map(Into::into),
        head_sha: Some("0123456789abcdef".into()),
    }
}

fn frame(targets: Vec<mast_proto::ChatDiffTargetView>) -> Vec<mast_proto::ChatDiffTargetsView> {
    vec![mast_proto::ChatDiffTargetsView {
        chat_id: "chat-wt".into(),
        targets,
        ref_targets: Vec::new(),
    }]
}

fn candidate(
    path: &str,
    kind: mast_proto::DiffCheckoutKind,
    branch: Option<&str>,
    selected: bool,
) -> mast_proto::DiffCheckoutCandidate {
    mast_proto::DiffCheckoutCandidate {
        path: path.into(),
        kind,
        repo_name: "flagship".into(),
        repo_slug: Some("cofactr/flagship".into()),
        branch: branch.map(Into::into),
        head_sha: Some("0123456789abcdef".into()),
        bound: false,
        selected,
    }
}

/// The engine's answer to "Diff checkout...": own + submodule + 2 worktrees.
fn picker_candidates() -> Vec<mast_proto::DiffCheckoutCandidate> {
    use mast_proto::DiffCheckoutKind::*;
    let mut candidates = vec![
        candidate(WS, Own, Some("main"), true),
        candidate("/work/ws/flagship", Submodule, Some("main"), false),
        candidate(WT_OTHER, Worktree, Some("michael/spike"), false),
        candidate(
            WT_TICKET,
            Worktree,
            Some("michael/eng-3012-kit-line-ship-estimates"),
            false,
        ),
    ];
    candidates[0].bound = true;
    candidates[3].bound = true;
    candidates
}

/// A state with one local chat and a fake diff-target engine. `set_error`
/// makes `SetChatDiffTarget` fail with that message.
fn target_state(
    targets: Vec<mast_proto::ChatDiffTargetView>,
    set_error: Option<&'static str>,
) -> (AppState, RpcCalls) {
    let calls: RpcCalls = Default::default();
    let sink = calls.clone();
    let mut state = AppState::new();
    state.local_device_id = Some("d".into());
    state.chats = vec![wt_chat()];
    if !targets.is_empty() {
        state.set_chat_diff_targets_for_test(frame(targets));
    }
    state.chat_diff_rpc_override = Some(std::sync::Arc::new(move |method, params| {
        sink.lock().unwrap().push((method.to_owned(), params));
        match method {
            mast_rpc::methods::LIST_CHAT_DIFF_CHECKOUTS => Ok(serde_json::to_value(
                mast_proto::ListChatDiffCheckoutsReply {
                    candidates: picker_candidates(),
                },
            )
            .unwrap()),
            mast_rpc::methods::SET_CHAT_DIFF_TARGET => match set_error {
                Some(message) => Err(message.to_owned()),
                None => Ok(
                    serde_json::to_value(mast_proto::ChatDiffTargetsReply::default()).unwrap(),
                ),
            },
            other => Err(format!("unexpected {other}")),
        }
    }));
    (state, calls)
}

fn open_target_changes<'a>(
    cx: &'a mut gpui::TestAppContext,
    targets: Vec<mast_proto::ChatDiffTargetView>,
    set_error: Option<&'static str>,
) -> (Entity<Changes>, RpcCalls, &'a mut gpui::VisualTestContext) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
    });
    let calls: std::sync::Arc<std::sync::Mutex<Option<RpcCalls>>> = Default::default();
    let sink = calls.clone();
    let (changes, vcx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| {
            let (state, calls) = target_state(targets, set_error);
            *sink.lock().unwrap() = Some(calls);
            state
        });
        Changes::for_chat(state, "chat-wt".into(), cx)
    });
    vcx.run_until_parked();
    let calls = calls.lock().unwrap().clone().unwrap();
    (changes, calls, vcx)
}

fn rpc_calls(calls: &RpcCalls, method: &str) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == method)
        .map(|(_, params)| params.clone())
        .collect()
}

fn open_picker(changes: &Entity<Changes>, vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        changes.update(cx, |changes, cx| changes.open_checkout_menu(window, cx))
    });
    vcx.run_until_parked();
}

fn picker_rows(
    changes: &Entity<Changes>,
    vcx: &mut gpui::VisualTestContext,
) -> Vec<(String, bool)> {
    changes.read_with(vcx, |changes, _| {
        changes
            .checkout_menu
            .get()
            .and_then(|menu| menu.candidates.ready())
            .map(|rows| {
                rows.iter()
                    .map(|row| (row.candidate.path.clone(), row.suggested))
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// (a) The picker lists the engine's candidates with the chat's ticket
/// match first and flagged suggested; the rest keep engine order.
#[gpui::test]
fn diff_checkout_picker_ranks_the_ticket_worktree_first(cx: &mut gpui::TestAppContext) {
    let (changes, calls, vcx) = open_target_changes(cx, Vec::new(), None);
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-checkout-trigger").is_some());

    changes.read_with(vcx, |changes, _| {
        assert!(changes.checkout_menu.get().is_none());
    });
    vcx.update(|window, cx| {
        changes.update(cx, |changes, cx| changes.open_checkout_menu(window, cx))
    });
    // Loading until the engine answers.
    changes.read_with(vcx, |changes, _| {
        let menu = changes.checkout_menu.get().unwrap();
        assert!(menu.candidates.is_loading() || menu.candidates.ready().is_some());
    });
    vcx.run_until_parked();

    assert_eq!(
        rpc_calls(&calls, mast_rpc::methods::LIST_CHAT_DIFF_CHECKOUTS),
        vec![serde_json::json!({"chatId": "chat-wt"})]
    );
    assert_eq!(
        picker_rows(&changes, vcx),
        vec![
            (WT_TICKET.to_owned(), true),
            (WS.to_owned(), false),
            ("/work/ws/flagship".to_owned(), false),
            (WT_OTHER.to_owned(), false),
        ]
    );
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-checkout-row-0").is_some());
    assert!(vcx.debug_bounds("changes-checkout-row-3").is_some());
}

#[gpui::test]
fn removing_a_checkout_unbinds_it_and_removes_its_row(cx: &mut gpui::TestAppContext) {
    let (changes, calls, vcx) = open_target_changes(cx, Vec::new(), None);
    open_picker(&changes, vcx);
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-checkout-remove-1").is_none());
    let button = vcx.debug_bounds("changes-checkout-remove-0").unwrap().center();
    vcx.simulate_click(button, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET), vec![
        serde_json::json!({"chatId": "chat-wt", "action": "unbind", "path": WT_TICKET})
    ]);
    assert!(!picker_rows(&changes, vcx).iter().any(|(path, _)| path == WT_TICKET));
}

/// The ticket also comes from the chat's branch when no link is stored.
#[gpui::test]
fn diff_checkout_picker_infers_the_ticket_from_the_chat_branch(cx: &mut gpui::TestAppContext) {
    let (changes, _calls, vcx) = open_target_changes(cx, Vec::new(), None);
    changes.update(vcx, |changes, cx| {
        changes.state.update(cx, |state, _| {
            state.chats[0].linked_ticket_id = None;
            state.chats[0].branch = Some("michael/eng-3012-something".into());
        });
    });
    open_picker(&changes, vcx);
    assert_eq!(picker_rows(&changes, vcx)[0], (WT_TICKET.to_owned(), true));
}

/// (b) Choosing a row writes `SetChatDiffTarget`: select {path} for a
/// checkout, clearSelection for the own checkout.
#[gpui::test]
fn choosing_a_checkout_persists_the_diff_target(cx: &mut gpui::TestAppContext) {
    let (changes, calls, vcx) = open_target_changes(cx, Vec::new(), None);
    open_picker(&changes, vcx);
    vcx.update(|window, cx| window.draw(cx).clear());
    // Row 0 is the ticket worktree: a real click on it.
    let row = vcx.debug_bounds("changes-checkout-row-0").unwrap().center();
    vcx.simulate_click(row, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET),
        vec![serde_json::json!({
            "chatId": "chat-wt", "action": "select", "path": WT_TICKET,
        })]
    );
    changes.read_with(vcx, |changes, cx| {
        assert!(!changes.checkout_menu.is_open(), "the picker closes");
        assert_eq!(
            changes.state.read(cx).chat_diff_target_error("chat-wt"),
            None
        );
    });

    // The own checkout row clears the selection (no path).
    open_picker(&changes, vcx);
    let own = changes.read_with(vcx, |changes, cx| {
        changes
            .checkout_menu_rows(cx)
            .iter()
            .position(|ix| {
                changes
                    .checkout_menu
                    .get()
                    .unwrap()
                    .candidates
                    .ready()
                    .unwrap()[*ix]
                    .candidate
                    .path
                    == WS
            })
            .unwrap()
    });
    let rows = changes.read_with(vcx, |changes, cx| changes.checkout_menu_rows(cx));
    let candidate = changes.read_with(vcx, |changes, _| {
        changes
            .checkout_menu
            .get()
            .unwrap()
            .candidates
            .ready()
            .unwrap()[rows[own]]
            .candidate
            .clone()
    });
    changes.update(vcx, |changes, cx| changes.choose_checkout(candidate, cx));
    vcx.run_until_parked();
    assert_eq!(
        rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET)[1],
        serde_json::json!({"chatId": "chat-wt", "action": "clearSelection"})
    );
}

/// Keyboard: arrows move the highlight, Enter chooses it.
#[gpui::test]
fn diff_checkout_picker_is_keyboard_driven(cx: &mut gpui::TestAppContext) {
    let (changes, calls, vcx) = open_target_changes(cx, Vec::new(), None);
    open_picker(&changes, vcx);
    let key = |key: &str| gpui::KeyDownEvent {
        keystroke: gpui::Keystroke::parse(key).unwrap(),
        is_held: false,
        prefer_character_input: false,
    };
    changes.update(vcx, |changes, cx| {
        changes.checkout_menu_key(&key("down"), cx);
        changes.checkout_menu_key(&key("enter"), cx);
    });
    vcx.run_until_parked();
    // Row 1 of the ranked list is the own checkout.
    assert_eq!(
        rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET),
        vec![serde_json::json!({"chatId": "chat-wt", "action": "clearSelection"})]
    );
}

/// (b) A failed write shows the engine's message as a copyable error
/// notice in the pane.
#[gpui::test]
fn a_failed_diff_target_write_surfaces_an_error_notice(cx: &mut gpui::TestAppContext) {
    let message = "diff-targets/invalid: path is not a worktree of this repository";
    let (changes, _calls, vcx) = open_target_changes(cx, Vec::new(), Some(message));
    open_picker(&changes, vcx);
    let candidate = changes.read_with(vcx, |changes, _| {
        changes
            .checkout_menu
            .get()
            .unwrap()
            .candidates
            .ready()
            .unwrap()[0]
            .candidate
            .clone()
    });
    changes.update(vcx, |changes, cx| changes.choose_checkout(candidate, cx));
    vcx.run_until_parked();
    changes.read_with(vcx, |changes, cx| {
        assert_eq!(
            changes.state.read(cx).chat_diff_target_error("chat-wt"),
            Some(message)
        );
    });
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-diff-target-error").is_some());
    // Dismissing clears it.
    changes.update(vcx, |changes, cx| {
        changes.state.update(cx, |state, cx| {
            state.dismiss_chat_diff_target_error("chat-wt", cx)
        })
    });
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-diff-target-error").is_none());
}

/// A failed list shows an error in the picker instead of rows.
#[gpui::test]
fn a_failed_checkout_list_shows_an_error_in_the_picker(cx: &mut gpui::TestAppContext) {
    let (changes, _calls, vcx) = open_target_changes(cx, Vec::new(), None);
    changes.update(vcx, |changes, cx| {
        changes.state.update(cx, |state, _| {
            state.chat_diff_rpc_override =
                Some(std::sync::Arc::new(|_, _| Err("no such chat".to_owned())));
        })
    });
    open_picker(&changes, vcx);
    changes.read_with(vcx, |changes, _| {
        let menu = changes.checkout_menu.get().unwrap();
        assert_eq!(menu.candidates.error(), Some("no such chat"));
    });
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-checkout-row-0").is_none());
}

/// (c) A selected target in the engine frame is what the pane diffs:
/// worktree cwd, checkout id, branch, matched by checkout id only. With
/// nothing selected the chat's own checkout shows.
#[gpui::test]
fn the_engine_selected_target_is_the_default_diff(cx: &mut gpui::TestAppContext) {
    let targets = vec![target_view(
        WT_TICKET,
        "checkout:wt",
        Some("michael/eng-3012-kit-line-ship-estimates"),
        true,
    )];
    let (changes, calls, vcx) = open_target_changes(cx, targets.clone(), None);
    changes.read_with(vcx, |changes, _| {
        let owner = changes.owner.as_ref().unwrap();
        assert_eq!(owner.cwd.as_deref(), Some(WT_TICKET));
        assert_eq!(owner.checkout_id.as_deref(), Some("checkout:wt"));
        assert_eq!(
            owner.branch.as_deref(),
            Some("michael/eng-3012-kit-line-ship-estimates")
        );
        assert!(owner.exact_checkout);
        assert_eq!(owner.unconfirmed, None);
        // The working tree diff is the one with that checkout id, never
        // the root's (same device, different cwd).
        let diffs = vec![
            diff("checkout:root", "d", WS, "root-dirty-docs"),
            diff("checkout:wt", "d", WT_TICKET, "worktree-change"),
        ];
        assert_eq!(owner.resolve(&diffs).unwrap().patch, "worktree-change");
        assert_eq!(
            watch_checkout_params(None, owner.checkout_id.as_deref(), owner.cwd.as_deref()),
            serde_json::json!({"checkoutId": "checkout:wt", "cwd": WT_TICKET})
        );
    });
    // Header shows the target's branch, not the root's `main`.
    changes.read_with(vcx, |changes, cx| {
        let (repo, worktree, branch, _) = changes.context_details(cx).unwrap();
        assert_eq!(worktree, "eng-3012-kit");
        assert_eq!(repo, "flagship");
        assert_eq!(branch, "michael/eng-3012-kit-line-ship-estimates");
    });
    // The composer's branch indicator reads the same member.
    changes.read_with(vcx, |changes, cx| {
        let member = changes
            .state
            .read(cx)
            .selected_execution_member("chat-wt")
            .unwrap();
        assert_eq!(
            member.label(),
            "flagship \u{b7} michael/eng-3012-kit-line-ship-estimates"
        );
    });
    // Own checkout + the target: two pills, the target active.
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-execution-repositories").is_some());
    assert!(vcx.debug_bounds("changes-execution-repo-0").is_some());
    assert!(vcx.debug_bounds("changes-execution-repo-1").is_some());
    assert!(vcx.debug_bounds("changes-execution-repo-2").is_none());
    // Nothing was written by merely showing it.
    assert!(rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET).is_empty());

    // No selected target: the chat's own checkout, as before targets.
    let (changes, _calls, vcx) = open_target_changes(
        cx,
        vec![target_view(WT_TICKET, "checkout:wt", Some("b"), false)],
        None,
    );
    changes.read_with(vcx, |changes, cx| {
        let owner = changes.owner.as_ref().unwrap();
        let chat = changes.state.read(cx).chats[0].clone();
        assert_eq!(owner, &ChangesTarget::from_chat(&chat));
        assert_eq!(owner.cwd.as_deref(), Some(WS));
        assert_eq!(owner.checkout_id.as_deref(), Some("checkout:root"));
        assert!(!owner.exact_checkout);
        let diffs = vec![diff("checkout:wt", "d", WT_TICKET, "wt")];
        assert!(owner.resolve(&diffs).is_none());
    });
}

/// Picking pills persists: a target pill selects its path, the own pill
/// clears the selection; the pane re-targets at once.
#[gpui::test]
fn picking_pills_persists_through_set_chat_diff_target(cx: &mut gpui::TestAppContext) {
    let targets = vec![
        target_view(WT_OTHER, "checkout:other", Some("michael/spike"), false),
        target_view(WT_TICKET, "checkout:wt", Some("michael/eng-3012-kit"), true),
    ];
    let (changes, calls, vcx) = open_target_changes(cx, targets, None);
    vcx.update(|window, cx| window.draw(cx).clear());
    // Pills: own, spike, eng-3012. Click the spike pill.
    let spike = vcx
        .debug_bounds("changes-execution-repo-1")
        .unwrap()
        .center();
    vcx.simulate_click(spike, gpui::Modifiers::default());
    vcx.run_until_parked();
    changes.read_with(vcx, |changes, _| {
        assert_eq!(
            changes.owner.as_ref().unwrap().checkout_id.as_deref(),
            Some("checkout:other")
        );
    });
    assert_eq!(
        rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET),
        vec![serde_json::json!({"chatId": "chat-wt", "action": "select", "path": WT_OTHER})]
    );
    // Back to the own checkout: clearSelection, exact cwd fallback again.
    vcx.update(|window, cx| window.draw(cx).clear());
    let own = vcx
        .debug_bounds("changes-execution-repo-0")
        .unwrap()
        .center();
    vcx.simulate_click(own, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET)[1],
        serde_json::json!({"chatId": "chat-wt", "action": "clearSelection"})
    );
    changes.read_with(vcx, |changes, _| {
        let owner = changes.owner.as_ref().unwrap();
        assert_eq!(owner.cwd.as_deref(), Some(WS));
        assert!(!owner.exact_checkout);
    });
}

/// A stale in-memory pick cannot fight the persisted selection: a frame
/// that selects another target wins; a failed write reverts the pick.
#[gpui::test]
fn a_stale_pick_does_not_fight_the_engine_selection(cx: &mut gpui::TestAppContext) {
    let targets = |selected_other: bool| {
        vec![
            target_view(WT_OTHER, "checkout:other", Some("spike"), selected_other),
            target_view(WT_TICKET, "checkout:wt", Some("eng"), !selected_other),
        ]
    };
    let (changes, _calls, vcx) = open_target_changes(cx, targets(false), None);
    let state = changes.read_with(vcx, |changes, _| changes.state.clone());
    let selected = |vcx: &mut gpui::VisualTestContext| {
        state.read_with(vcx, |state, _| {
            state
                .selected_execution_member("chat-wt")
                .unwrap()
                .checkout_path
        })
    };
    assert_eq!(selected(vcx), WT_TICKET);
    // The user picks spike; the write is in flight, the pick shows.
    state.update(vcx, |state, cx| {
        state.select_chat_execution_member("chat-wt", "target:checkout:other", cx)
    });
    assert_eq!(selected(vcx), WT_OTHER);
    // The agent then binds another worktree: the frame's selection wins.
    state.update(vcx, |state, cx| {
        state.apply_chat_diff_targets(vec![mast_proto::ChatDiffTargetsView {
            chat_id: "chat-wt".into(),
            targets: vec![
                target_view(WT_OTHER, "checkout:other", Some("spike"), false),
                target_view(WT_TICKET, "checkout:wt", Some("eng"), false),
                target_view(
                    "/work/ws/.worktrees/flagship/new",
                    "checkout:new",
                    None,
                    true,
                ),
            ],
            ref_targets: Vec::new(),
        }]);
        cx.notify();
    });
    vcx.run_until_parked();
    assert_eq!(selected(vcx), "/work/ws/.worktrees/flagship/new");
    changes.read_with(vcx, |changes, _| {
        let owner = changes.owner.as_ref().unwrap();
        assert_eq!(owner.checkout_id.as_deref(), Some("checkout:new"));
        assert_eq!(owner.branch, None);
    });
    // A frame that only clears the selection keeps the user's own pick.
    state.update(vcx, |state, cx| {
        state.select_chat_execution_member("chat-wt", "target:checkout:wt", cx);
        state.apply_chat_diff_targets(frame(targets_all_unselected()));
        cx.notify();
    });
    assert_eq!(selected(vcx), WT_TICKET);
    // A pick of a target that vanished is dropped.
    state.update(vcx, |state, cx| {
        state.apply_chat_diff_targets(frame(vec![target_view(
            WT_OTHER,
            "checkout:other",
            Some("spike"),
            false,
        )]));
        cx.notify();
    });
    assert_eq!(selected(vcx), WS, "falls back to the own checkout");
}

fn targets_all_unselected() -> Vec<mast_proto::ChatDiffTargetView> {
    vec![
        target_view(WT_OTHER, "checkout:other", Some("spike"), false),
        target_view(WT_TICKET, "checkout:wt", Some("eng"), false),
        target_view(
            "/work/ws/.worktrees/flagship/new",
            "checkout:new",
            None,
            false,
        ),
    ]
}

#[gpui::test]
fn a_failed_pill_write_reverts_to_the_engine_selection(cx: &mut gpui::TestAppContext) {
    let targets = vec![
        target_view(WT_OTHER, "checkout:other", Some("spike"), false),
        target_view(WT_TICKET, "checkout:wt", Some("eng"), true),
    ];
    let (changes, _calls, vcx) = open_target_changes(cx, targets, Some("write refused"));
    let state = changes.read_with(vcx, |changes, _| changes.state.clone());
    state.update(vcx, |state, cx| {
        state.select_chat_execution_member("chat-wt", "target:checkout:other", cx)
    });
    vcx.run_until_parked();
    state.read_with(vcx, |state, _| {
        assert_eq!(
            state
                .selected_execution_member("chat-wt")
                .unwrap()
                .checkout_path,
            WT_TICKET
        );
        assert_eq!(
            state.chat_diff_target_error("chat-wt"),
            Some("write refused")
        );
    });
}

/// (d) A chat with no targets and no binding is unchanged: no pill row,
/// the plain chat target, no members; the picker is offered only for a
/// chat on this device with a cwd, never for a commit or pinned view.
#[gpui::test]
fn chats_without_targets_are_unchanged(cx: &mut gpui::TestAppContext) {
    let (changes, calls, vcx) = open_target_changes(cx, Vec::new(), None);
    changes.read_with(vcx, |changes, cx| {
        let state = changes.state.read(cx);
        assert!(state.chat_execution_members("chat-wt").is_empty());
        assert!(state.selected_execution_member("chat-wt").is_none());
        let chat = state.chats[0].clone();
        assert_eq!(
            changes.owner.as_ref().unwrap(),
            &ChangesTarget::from_chat(&chat)
        );
        assert!(changes.checkout_picker_available(cx));
    });
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-execution-repositories").is_none());
    assert!(vcx.debug_bounds("changes-checkout-trigger").is_some());
    assert!(calls.lock().unwrap().is_empty(), "no engine traffic");

    // Remote chat or no cwd: no picker.
    changes.update(vcx, |changes, cx| {
        changes
            .state
            .update(cx, |state, _| state.chats[0].device_id = "other".into());
        changes.sync(cx);
    });
    changes.read_with(vcx, |changes, cx| {
        assert!(!changes.checkout_picker_available(cx))
    });
    changes.update(vcx, |changes, cx| {
        changes.state.update(cx, |state, _| {
            state.chats[0].device_id = "d".into();
            state.chats[0].cwd = None;
        });
        changes.sync(cx);
    });
    changes.read_with(vcx, |changes, cx| {
        assert!(!changes.checkout_picker_available(cx))
    });
    // A pinned checkout view never offers it.
    changes.update(vcx, |changes, cx| {
        changes
            .state
            .update(cx, |state, _| state.chats[0].cwd = Some(WS.into()));
        changes.pinned_cwd = Some(WS.into());
    });
    changes.read_with(vcx, |changes, cx| {
        assert!(!changes.checkout_picker_available(cx))
    });
}

/// (f) A frame arriving after the pane opened (the agent bound a worktree
/// mid-turn) re-targets the open surface; removing it falls back.
#[gpui::test]
fn a_frame_after_a_bind_retargets_the_open_surface(cx: &mut gpui::TestAppContext) {
    let (changes, _calls, vcx) = open_target_changes(cx, Vec::new(), None);
    changes.read_with(vcx, |changes, _| {
        assert_eq!(changes.owner.as_ref().unwrap().cwd.as_deref(), Some(WS));
    });
    let state = changes.read_with(vcx, |changes, _| changes.state.clone());
    state.update(vcx, |state, cx| {
        state.apply_chat_diff_targets(frame(vec![target_view(
            WT_TICKET,
            "checkout:wt",
            Some("michael/eng-3012-kit"),
            true,
        )]));
        cx.notify();
    });
    vcx.run_until_parked();
    changes.read_with(vcx, |changes, _| {
        let owner = changes.owner.as_ref().unwrap();
        assert_eq!(owner.cwd.as_deref(), Some(WT_TICKET));
        assert_eq!(owner.checkout_id.as_deref(), Some("checkout:wt"));
        assert!(owner.exact_checkout);
        assert!(changes.diffs.is_empty());
    });
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("changes-execution-repositories").is_some());
    // The worktree is removed: its target disappears from the frame.
    state.update(vcx, |state, cx| {
        state.apply_chat_diff_targets(Vec::new());
        cx.notify();
    });
    vcx.run_until_parked();
    changes.read_with(vcx, |changes, _| {
        let owner = changes.owner.as_ref().unwrap();
        assert_eq!(owner.cwd.as_deref(), Some(WS));
        assert!(!owner.exact_checkout);
    });
}
