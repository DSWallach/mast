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
            for pattern in ["UiLayout::Minimal", "UiLayout::Compact", "UiLayout::Guided"] {
                counts[3] += source.matches(pattern).count();
            }
        }
    }
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
    for (name, (count, limit)) in ["text size", "radius", "style", "layout"]
        .into_iter()
        .zip(counts.into_iter().zip(baseline))
    {
        assert!(count <= limit, "{name} count increased: {count} > {limit}");
    }
}

#[test]
fn numeric_call_counts_qualified_and_multiline_literals() {
    let source = ".text_size(\n crate::typography::ui_rems(13.0)) .text_size(ui_rems(size)) .rounded(\n px(4.0))";
    assert_eq!(count_numeric_call(source, ".text_size(", "ui_rems("), 1);
    assert_eq!(count_numeric_call(source, ".rounded(", "px("), 1);
}
