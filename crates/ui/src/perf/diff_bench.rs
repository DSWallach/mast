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
    bench_width_transitions(cx, &patch);
    bench_diff_pane(cx, "one file 5000 lines", patch);
}

/// Measure the width work itself on the UI thread for each highlight state.
fn bench_width_transitions(cx: &mut gpui::TestAppContext, patch: &str) {
    let files = parse_patch(patch);
    let file = &files[0];
    let excerpt = Arc::new(excerpt_highlights(file, Lang::Rust).unwrap());
    let ready = Arc::new(excerpt_highlights(file, Lang::Rust).unwrap());
    let horizontal = FileHorizontalState::new(file);
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let (_, vcx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Changes::for_checkout(state, "/work/repo".into(), cx)
    });
    let mut theme = Theme::dark();
    let mut max_ms = 0.0f64;
    for (label, highlight, font_change) in [
        ("pending", None, false),
        ("excerpt", Some(&excerpt), false),
        ("ready", Some(&ready), false),
        ("font change", Some(&ready), true),
    ] {
        if font_change { theme.code_font_size += 2.0; }
        SHAPED_LINES.with(|c| c.set(0));
        let start = std::time::Instant::now();
        vcx.update(|window, _| {
            std::hint::black_box(horizontal.metrics(file, highlight, &theme, window.text_system(), 0));
        });
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        max_ms = max_ms.max(ms);
        eprintln!("BENCH diff | width {label} | ui_ms={ms:.2} lines={}", SHAPED_LINES.with(|c| c.get()));
    }
    eprintln!("BENCH diff | width max single UI update ms={max_ms:.2}");
    let (changes, state, vcx) = diff_pane_with_state(cx, patch.to_owned());
    let mut max_frame_ms = 0.0f64;
    for (label, highlight) in [("pending", None), ("excerpt", Some(excerpt)), ("ready", Some(ready))] {
        SHAPED_LINES.with(|c| c.set(0));
        let start = std::time::Instant::now();
        changes.update(vcx, |c, cx| {
            let parsed = c.parsed.as_ref().unwrap();
            let path = parsed.files[0].path.clone();
            let fingerprint = hash64(&[&parsed.key, &path]);
            if label == "pending" { *parsed.horizontal[0].measured.borrow_mut() = None; }
            c.highlights.insert(path, HighlightSlot {
                fingerprint,
                state: match highlight {
                    None => DiffHighlightState::Pending,
                    Some(highlight) if label == "excerpt" => DiffHighlightState::Excerpt(highlight),
                    Some(highlight) => DiffHighlightState::Ready(highlight),
                },
                _excerpt_task: None,
                _fetch_task: None,
            });
            cx.notify();
        });
        vcx.update(|window, cx| { window.refresh(); window.draw(cx).clear(); });
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        max_frame_ms = max_frame_ms.max(ms);
        eprintln!("BENCH diff | {label} frame | ms={ms:.2} lines={}", SHAPED_LINES.with(|c| c.get()));
    }
    vcx.update(|_, cx| cx.set_global(theme));
    SHAPED_LINES.with(|c| c.set(0));
    let start = std::time::Instant::now();
    vcx.update(|window, cx| { window.refresh(); window.draw(cx).clear(); });
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    max_frame_ms = max_frame_ms.max(ms);
    eprintln!("BENCH diff | font change frame | ms={ms:.2} lines={}", SHAPED_LINES.with(|c| c.get()));
    eprintln!("BENCH diff | transition max frame ms={max_frame_ms:.2}");
    let before = changes.read_with(vcx, |c, _| (c.benchmark_sync_count, c.benchmark_render_count));
    for _ in 0..10 {
        state.update(vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
    }
    let after = changes.read_with(vcx, |c, _| (c.benchmark_sync_count, c.benchmark_render_count));
    eprintln!("BENCH diff | unrelated state notifies=10 syncs={} renders={}", after.0 - before.0, after.1 - before.1);
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
    crate::changes::SHAPED_LINES.with(|c| c.set(0));
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
    eprintln!(
        "BENCH diff | lines shaped by width measure after load+first draw={}",
        crate::changes::SHAPED_LINES.with(|c| c.get())
    );
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

fn diff_pane_with_state(
    cx: &mut gpui::TestAppContext,
    patch: String,
) -> (
    Entity<Changes>,
    Entity<AppState>,
    &mut gpui::VisualTestContext,
) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.chats = (0..250).map(bench_chat).collect();
    });
    let st = state.clone();
    let (changes, vcx) = cx.add_window_view(move |_, cx| Changes::for_checkout(st, "/work/repo".into(), cx));
    changes.update(vcx, |c, cx| {
        c.scoped = Some(CheckoutDiff {
            checkout_id: "co".into(),
            device_id: "local".into(),
            cwd: "/work/repo".into(),
            patch,
            files: Vec::new(),
            submodules: Vec::new(),
            additions: 3000,
            deletions: 1000,
            truncated: false,
            checksum: "bench".into(),
            updated_at: chrono::Utc::now(),
        });
        c.scoped_for = Some(format!("local|||/work/repo|{}||", c.scope.mode()) + "|");
        c.sync(cx);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
    std::mem::forget(dir);
    (changes, state, vcx)
}

/// AppState notifies while a large diff is open (any chat/session change
/// notifies): cost of the observer's `sync` plus the redraw it triggers.
#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_diff_pane_state_notifies(cx: &mut gpui::TestAppContext) {
    let (changes, state, vcx) = diff_pane_with_state(cx, data::unified_diff(60_000));
    let patch_len = changes.read_with(vcx, |c, _| c.scoped.as_ref().unwrap().patch.len());
    eprintln!("BENCH diff | patch bytes={patch_len}");
    let before = changes.read_with(vcx, |c, _| (c.benchmark_sync_count, c.benchmark_render_count));
    measure_n("diff", "AppState notify + draw (250 chats)", 50, |_| {
        state.update(vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
    });
    let after = changes.read_with(vcx, |c, _| (c.benchmark_sync_count, c.benchmark_render_count));
    eprintln!("BENCH diff | per notify: sync={} render={}", (after.0 - before.0) as f64 / 50.0, (after.1 - before.1) as f64 / 50.0);
    measure_n("diff", "idle draw (no notify)", 50, |_| {
        vcx.update(|window, cx| window.draw(cx).clear());
    });
}

/// `active_diff` runs on every render, sync and highlight miss: it must hand
/// out the stored diff, not a copy of its multi-megabyte patch.
#[gpui::test]
fn active_diff_does_not_copy_the_patch(cx: &mut gpui::TestAppContext) {
    let (changes, _state, vcx) = diff_pane_with_state(cx, data::unified_diff(20_000));
    let patch_len = changes.read_with(vcx, |c, _| c.scoped.as_ref().unwrap().patch.len());
    const CALLS: u64 = 10;
    let (_, before) = crate::perf::alloc_totals();
    changes.read_with(vcx, |c, cx| {
        for _ in 0..CALLS {
            assert!(std::hint::black_box(c.active_diff(cx)).is_some());
        }
    });
    let (_, after) = crate::perf::alloc_totals();
    let per_call = (after - before) / CALLS;
    eprintln!("BUDGET active_diff: {per_call} bytes/call, patch {patch_len} bytes");
    assert!(
        per_call < patch_len as u64 / 10,
        "active_diff allocated {per_call} bytes per call for a {patch_len}-byte patch"
    );
}

fn bench_chat(index: usize) -> mast_proto::Chat {
    use chrono::TimeZone;
    mast_proto::Chat {
        standing_role_successor: None,
        id: format!("c{index}"),
        device_id: "local".into(),
        title: Some(format!("Chat {index}")),
        archived: false,
        cwd: Some(format!("/work/repo{}", index % 25)),
        branch: None,
        checkout_id: None,
        source_context: None,
        config: None,
        last_message_preview: None,
        last_message_at: None,
        created_at: chrono::Utc.timestamp_opt(1_600_000_000, 0).unwrap(),
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
        scheduled_task: None,
        scheduled_task_info: None,
        side_of: None,
        followup_of: None,
        side_absorbed_through: None,
    }
}

/// Rendering rows of a file with loaded expanded context must resolve the
/// monospace advance once per typography/theme state, not once per row.
#[gpui::test]
fn font_advance_is_not_looked_up_per_row(cx: &mut gpui::TestAppContext) {
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
    let mut patch = String::from(
        "diff --git a/large.rs b/large.rs\n--- a/large.rs\n+++ b/large.rs\n@@ -1,400 +1,400 @@\n",
    );
    for line in 0..400 {
        patch.push_str(&format!("+let value_{line} = {line};\n"));
    }
    changes.update(vcx, |c, cx| {
        c.scoped = Some(CheckoutDiff {
            checkout_id: "co".into(),
            device_id: "local".into(),
            cwd: "/work/repo".into(),
            patch,
            files: Vec::new(),
            submodules: Vec::new(),
            additions: 400,
            deletions: 0,
            truncated: false,
            checksum: "advance".into(),
            updated_at: chrono::Utc::now(),
        });
        c.scoped_for = Some(format!("local|||/work/repo|{}||", c.scope.mode()) + "|");
        c.sync(cx);
    });
    vcx.run_until_parked();
    changes.update(vcx, |c, _| c.test_mark_context_ready());
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    };
    crate::changes::reset_ch_advance_cache();
    let first = crate::changes::CH_ADVANCE_LOOKUPS.with(|c| c.get());
    draw(vcx);
    assert!(
        crate::changes::CH_ADVANCE_LOOKUPS.with(|c| c.get()) > first,
        "the first draw never measured a column; the budget is vacuous"
    );
    let before = crate::changes::CH_ADVANCE_LOOKUPS.with(|c| c.get());
    for _ in 0..10 {
        draw(vcx);
    }
    let lookups = crate::changes::CH_ADVANCE_LOOKUPS.with(|c| c.get()) - before;
    eprintln!("BUDGET diff font advance lookups over 10 redraws: {lookups}");
    assert!(lookups <= 1, "advance looked up per row: {lookups} lookups over 10 redraws");
}
