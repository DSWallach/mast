//! Diff pane over a 5000-line patch: patch parse, row flatten, first paint
//! and scrolling.

use super::*;
use crate::perf::{data, measure, measure_n};

#[test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_diff_parse_5000_lines() {
    let patch = data::unified_diff(5000);
    eprintln!(
        "BENCH diff | patch bytes={} lines={}",
        patch.len(),
        patch.lines().count()
    );
    measure("diff", "parse_patch", || parse_patch(&patch));
    measure_n("diff", "parse_patch", 5, |_| {
        std::hint::black_box(parse_patch(&patch));
    });
    measure("diff", "parse_diff_tree (parse+layout)", || {
        parse_diff_tree(&patch, &[])
    });
    let files = parse_patch(&patch);
    measure_n("diff", "highlight 200-line excerpt", 30, |_| {
        std::hint::black_box(excerpt_highlights(&files[0], Lang::Rust));
    });
}

#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_diff_pane_5000_lines(cx: &mut gpui::TestAppContext) {
    for files in [1, 50, 300] {
        let patch = data::unified_diff(files * 210);
        bench_diff_pane(cx, &format!("{files} files"), patch);
    }
    let mut patch = String::from(
        "diff --git a/large.rs b/large.rs\n--- a/large.rs\n+++ b/large.rs\n@@ -1,5000 +1,5000 @@\n",
    );
    for line in 0..5000 {
        patch.push_str(&format!("+let value_{line} = {line};\n"));
    }
    bench_diff_pane(cx, "one file 5000 lines", patch);
}

fn bench_diff_pane(cx: &mut gpui::TestAppContext, label: &str, patch: String) {
    eprintln!("BENCH diff | case={label}");
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let (changes, vcx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Changes::for_checkout(state, "/work/repo".into(), cx)
    });
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    };
    measure(
        "diff",
        "load patch (sync + background parse + flatten)",
        || {
            changes.update(vcx, |c, cx| {
                c.scoped = Some(CheckoutDiff {
                    checkout_id: "co".into(),
                    device_id: "local".into(),
                    cwd: "/work/repo".into(),
                    patch: patch.clone(),
                    files: Vec::new(),
                    submodules: Vec::new(),
                    additions: 3000,
                    deletions: 1000,
                    truncated: false,
                    checksum: "bench".into(),
                    updated_at: chrono::Utc::now(),
                });
                // `ensure_scoped` keeps a scoped diff only when it was fetched for
                // the current key; there is no engine here to fetch it.
                c.scoped_for = Some(format!("local|||/work/repo|{}||", c.scope.mode()) + "|");
                c.sync(cx);
            });
            vcx.run_until_parked();
        },
    );
    let (rows, parsed, scoped) = changes.read_with(vcx, |c, _| {
        (c.rows.len(), c.parsed.is_some(), c.scoped.is_some())
    });
    eprintln!("BENCH diff | rows={rows} parsed={parsed} scoped={scoped}");
    measure("diff", "first draw", || draw(vcx));
    measure_n("diff", "idle redraw", 30, |_| draw(vcx));
    vcx.update(|window, cx| {
        changes.update(cx, |changes, cx| window.focus(&changes.diff_focus, cx))
    });
    for (forward, backward) in [("j", "k"), ("shift-j", "shift-k")] {
        let key = |name| gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse(name).unwrap(),
            is_held: false,
            prefer_character_input: false,
        };
        let keys = [key(forward), key(backward)];
        vcx.update(|window, cx| {
            changes.update(cx, |changes, cx| {
                changes.diff_key_down(&keys[0], window, cx)
            })
        });
        let before = changes.read_with(vcx, |c, _| {
            (c.benchmark_render_count, c.benchmark_highlight_count)
        });
        measure_n("diff", &format!("{forward}/{backward} + draw"), 30, |i| {
            vcx.update(|window, cx| {
                changes.update(cx, |changes, cx| {
                    changes.diff_key_down(&keys[i % 2], window, cx)
                })
            });
            draw(vcx);
        });
        let after = changes.read_with(vcx, |c, _| {
            (c.benchmark_render_count, c.benchmark_highlight_count)
        });
        eprintln!(
            "BENCH diff | {forward}/{backward} renders={} highlight_starts={}",
            after.0 - before.0,
            after.1 - before.1
        );
    }
    let list = changes.read_with(vcx, |c, _| c.list.clone());
    measure_n("diff", "scroll 700px + draw", 100, |_| {
        list.scroll_by(px(700.0));
        draw(vcx);
    });
}
