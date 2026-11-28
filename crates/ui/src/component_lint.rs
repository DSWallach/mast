//! Ratchet for literal presentation metrics and screen-owned appearance branches.
use std::{fs, path::Path};

fn count_numeric_call(source: &str, outer: &str, inner: &str) -> usize {
    source
        .match_indices(outer)
        .filter(|(offset, _)| {
            let tail = &source[offset + outer.len()..];
            let Some(inner_offset) = tail.find(inner) else {
                return false;
            };
            // Do not match an inner call belonging to a later expression.
            if tail[..inner_offset].contains([')', ';']) {
                return false;
            }
            tail[inner_offset + inner.len()..]
                .trim_start()
                .starts_with(|c: char| c.is_ascii_digit())
        })
        .count()
}

// A grouped or glob import enables unqualified layout comparisons just as a
// direct variant reference does. Count the common helper spelling too, so
// wrapping an enum comparison cannot bypass the screen-branch ratchet.
fn count_layout_branches(source: &str) -> usize {
    ["UiLayout::Minimal", "UiLayout::Compact", "UiLayout::Guided", "UiLayout::{", "UiLayout::*", "UiLayout as ", ".is_compact("]
        .into_iter().map(|pattern| source.matches(pattern).count()).sum()
}

fn visit(path: &Path, counts: &mut [usize; 4]) {
    for entry in fs::read_dir(path).expect("read UI sources") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "components") {
                continue;
            }
            visit(&path, counts);
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && !path
                .file_name()
                // The resolver and its editor must enumerate style/layout keys;
                // this ratchet targets presentation decisions in ordinary screens.
                .is_some_and(|name| matches!(name.to_str(), Some("ui_style.rs" | "style_overrides.rs" | "design_panel.rs" | "component_lint.rs")))
        {
            let source = fs::read_to_string(&path).expect("read UI source");
            counts[0] += count_numeric_call(&source, ".text_size(", "ui_rems(");
            counts[1] += count_numeric_call(&source, ".rounded(", "px(");
            for pattern in [
                "UiStyleId::Industrial",
                "UiStyleId::Horizon",
                "ConversationVariant::",
            ] {
                counts[2] += source.matches(pattern).count();
            }
            counts[3] += count_layout_branches(&source);
        }
    }
}

#[test]
fn layout_ratchet_counts_imports_and_helper_checks() {
    assert_eq!(count_layout_branches("use UiLayout::{Compact, Minimal}; if layout == Compact {}"), 1);
    assert_eq!(count_layout_branches("use UiLayout::*; if layout == Compact {}"), 1);
    assert_eq!(count_layout_branches("use crate::ui_style::UiLayout as Layout; if layout == Layout::Compact {}"), 1);
    assert_eq!(count_layout_branches("if layout.is_compact() {}"), 1);
    assert_eq!(count_layout_branches("if behavior.compact_overview_navigation {}"), 0);
}

#[test]
fn presentation_literals_and_branches_do_not_increase() {
    let mut counts = [0; 4];
    visit(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut counts,
    );
    let baseline: Vec<usize> = include_str!("component_lint_baseline.txt")
        .lines()
        .map(|line| line.parse().expect("numeric baseline"))
        .collect();
    assert_eq!(baseline.len(), counts.len());
    // Report every category at once so one run shows the whole budget.
    let increased: Vec<String> = ["text size", "radius", "style", "layout"]
        .into_iter()
        .zip(counts.into_iter().zip(baseline))
        .filter(|(_, (count, limit))| count > limit)
        .map(|(name, (count, limit))| format!("{name} count increased: {count} > {limit}"))
        .collect();
    assert!(increased.is_empty(), "{}", increased.join("; "));
}

#[test]
fn numeric_call_counts_qualified_and_multiline_literals() {
    let source = ".text_size(\n crate::typography::ui_rems(13.0)) .text_size(ui_rems(size)) .rounded(\n px(4.0))";
    assert_eq!(count_numeric_call(source, ".text_size(", "ui_rems("), 1);
    assert_eq!(count_numeric_call(source, ".rounded(", "px("), 1);
}
