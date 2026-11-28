//! Overview with a busy profile: 250 chats in 25 repos, one Space whose
//! outline has 40 work items each with a PR and a Linear child. Rebuild cost
//! of `rows()`, then List and Outline paints.

use super::*;
use crate::perf::{measure, measure_n};
use crate::work_outline::store::{FIXTURE_NOW, fixture_actor, fixture_assignment, fixture_item};
use mast_proto::{
    WorkItemChatAssignmentState as AssignState, WorkItemExternalReferenceView, WorkItemStatus,
};
use std::collections::HashMap;

const SPACE: &str = "space-1";

fn chat_row(index: usize) -> mast_proto::Chat {
    use chrono::TimeZone;
    mast_proto::Chat {
        id: format!("c{index}"),
        device_id: "local".into(),
        title: Some(format!(
            "Chat {index}: investigate the failing registry sync"
        )),
        archived: false,
        cwd: Some(format!("/work/repo{}", index % 25)),
        branch: Some(format!("feature/thing-{}", index % 40)),
        checkout_id: None,
        source_context: None,
        config: None,
        last_message_preview: Some(format!("Last message preview text for chat {index}")),
        last_message_at: Some(
            Utc.timestamp_opt(1_700_000_000 + index as i64 * 600, 0)
                .unwrap(),
        ),
        created_at: Utc.timestamp_opt(1_600_000_000, 0).unwrap(),
        harness_session_id: None,
        harness_session_cwd: None,
        actual_model: None,
        scheduled_task: None,
        scheduled_task_info: None,
        standing_role_successor: None,
        parent_chat_id: None,
        side_of: None,
        followup_of: None,
        side_absorbed_through: None,
        space_id: Some(SPACE.into()),
        last_seen_at: None,
        room_gen: None,
        linked_pr_url: None,
        linked_pr_source: None,
        linked_ticket_id: None,
        linked_ticket_source: None,
    }
}

fn snapshot(roots: usize) -> WorkOutlineSnapshot {
    let mut snap = crate::work_outline::store::fixture_snapshot(SPACE);
    for i in 0..roots {
        let root = format!("w-bench-{i}");
        let pr = format!("w-bench-{i}-pr");
        let lin = format!("w-bench-{i}-lin");
        let mut items = vec![
            fixture_item(
                &root,
                SPACE,
                None,
                &format!("b{i:03}"),
                &format!("Ticket {i}: ship the thing"),
                WorkItemStatus::Active,
                &[&pr, &lin],
                &[&format!("c{}", i * 5)],
            ),
            fixture_item(
                &pr,
                SPACE,
                Some(&root),
                "a0",
                &format!("PR review: acme/repo#{i}"),
                WorkItemStatus::Active,
                &[],
                &[&format!("c{}", i * 5 + 1)],
            ),
            fixture_item(
                &lin,
                SPACE,
                Some(&root),
                "a1",
                &format!("ENG-{i}: linked issue"),
                WorkItemStatus::Pending,
                &[],
                &[&format!("c{}", i * 5 + 2)],
            ),
        ];
        items[1].depth = 1;
        items[2].depth = 1;
        for (item, provider, kind, ext) in [
            (&pr, "github", "pullRequest", format!("acme/repo#{i}")),
            (&lin, "linear", "issue", format!("ENG-{i}")),
        ] {
            snap.external_references
                .push(WorkItemExternalReferenceView {
                    id: format!("ref-{item}"),
                    space_id: SPACE.into(),
                    work_item_id: item.clone(),
                    provider: provider.into(),
                    reference_type: kind.into(),
                    external_id: ext.clone(),
                    url: format!("https://example.com/{ext}"),
                    cached_title: Some(format!("Title of {ext}")),
                    created_by: Some(fixture_actor()),
                    created_at: FIXTURE_NOW,
                });
        }
        for (n, work_item) in [&root, &pr, &lin].into_iter().enumerate() {
            snap.assignments.push(fixture_assignment(
                &format!("c{}", i * 5 + n),
                SPACE,
                Some(work_item),
                AssignState::Associated,
            ));
        }
        snap.root_work_item_ids.push(root);
        snap.items.append(&mut items);
    }
    snap
}

/// One `SAMPLE` line per metric, in milliseconds, for `scripts`-free median/MAD
/// post-processing: `SAMPLE overview | <chats> | <metric> | ms=<mean>`.
fn sample(chats: usize, metric: &str, ms: f64) {
    eprintln!("SAMPLE overview | {chats} | {metric} | ms={ms:.3}");
}

fn mean_ms(n: usize, mut f: impl FnMut(usize)) -> f64 {
    let t = std::time::Instant::now();
    for i in 0..n {
        f(i);
    }
    t.elapsed().as_secs_f64() * 1000.0 / n as f64
}

fn run_overview_bench(cx: &mut gpui::TestAppContext, chats: usize) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.data_dir = Some(dir.path().to_path_buf());
        state.chats = (0..chats).map(chat_row).collect();
        state.spaces = vec![mast_proto::Space {
            id: SPACE.into(),
            device_id: "local".into(),
            path: "/work".into(),
            name: Some("Bench".into()),
            git_detected: true,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        }];
    });
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let st = state.clone();
    let (overview, vcx) = cx.add_window_view(move |_, cx| {
        Overview::new(
            st,
            gpui::WeakEntity::new_invalid(),
            transcript,
            composer,
            cx,
        )
    });
    vcx.run_until_parked();
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    };
    overview.update(vcx, |o, _| {
        let mut map = HashMap::new();
        map.insert(
            SPACE.to_string(),
            WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot(40))),
        );
        o.outline_fixture = Some(map);
        o.view_mode = OverviewMode::List;
        o.rows_dirty = true;
    });
    // Cold open: the first List draw, which builds rows and the presentation.
    let cold = mean_ms(1, |_| draw(vcx));
    sample(chats, "cold open", cold);
    // Idle redraw: nothing changed, the presentation is rebuilt anyway.
    draw(vcx);
    sample(chats, "idle redraw", mean_ms(40, |_| draw(vcx)));
    // Scroll: wheel the list down a few rows per frame, then back up.
    sample(
        chats,
        "scroll",
        mean_ms(40, |i| {
            let dy = if i < 20 { 90.0 } else { -90.0 };
            overview.update(vcx, |o, cx| {
                o.list.scroll_by(px(dy));
                cx.notify();
            });
            draw(vcx);
        }),
    );
    // Grouped reorder: group by repo, then move one chat to the top each frame.
    overview.update(vcx, |o, cx| {
        o.active_groups = vec![GroupDimension::Repo];
        o.rows_dirty = true;
        cx.notify();
    });
    draw(vcx);
    sample(
        chats,
        "grouped reorder",
        mean_ms(20, |i| {
            state.update(vcx, |state, cx| {
                let chat = &mut state.chats[(i * 7) % chats];
                chat.last_message_at = Some(Utc::now() + chrono::Duration::days(1 + i as i64));
                cx.notify();
            });
            overview.update(vcx, |o, cx| {
                o.rows_dirty = true;
                cx.notify();
            });
            draw(vcx);
        }),
    );
}

#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_overview_250_chats_40_items(cx: &mut gpui::TestAppContext) {
    run_overview_bench(cx, 250);
}

#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_overview_1000_chats_40_items(cx: &mut gpui::TestAppContext) {
    run_overview_bench(cx, 1000);
}

/// Compact single-chat rows (on by default in existing profiles) have no status word; a chat
/// waiting on a build still says so there, read or not.
#[gpui::test]
fn build_watch_compact_single_chat_rows_show_the_badge_for_an_unread_chat(
    cx: &mut gpui::TestAppContext,
) {
    let (overview, vcx) = outline_fixture_overview(cx, 1);
    overview.update(vcx, |o, cx| o.set_compact_single_chat(true, cx));
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    let redraw = |vcx: &mut gpui::VisualTestContext| {
        for _ in 0..2 {
            vcx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
            vcx.run_until_parked();
        }
    };
    let selector: &'static str = Box::leak(
        format!(
            "outline-compact-build-{}",
            crate::work_outline::model::row_key::item(SPACE, "w-bench-0-pr")
        )
        .into_boxed_str(),
    );
    redraw(vcx);
    assert!(vcx.debug_bounds(selector).is_none(), "no builds, no badge");
    state.update(vcx, |state, cx| {
        let chat = state.chats.iter().find(|chat| chat.id == "c1").unwrap();
        assert!(chat.unseen());
        state.set_test_build_jobs(mast_proto::BuildJobsSnapshot {
            space_id: SPACE.into(),
            jobs: vec![mast_proto::BuildJobView {
                job_id: "j".into(),
                space_id: SPACE.into(),
                chat_id: "c1".into(),
                state: mast_proto::BuildJobState::Queued,
                position: Some(2),
                ..Default::default()
            }],
            quiet_window_active: false,
        });
        cx.notify();
    });
    overview.update(vcx, |o, cx| {
        o.rows_dirty = true;
        cx.notify();
    });
    redraw(vcx);
    assert!(vcx.debug_bounds(selector).is_some(), "the compact row shows the badge");
}

/// The 250-chat Overview tree with no build jobs: drawing it consults no build badge map, and the
/// Overview fingerprint is exactly what it is without the build queue; a queued job turns the
/// lookups on and changes the fingerprint, and clearing it restores the original value.
#[gpui::test]
fn build_watch_overview_rows_cost_nothing_while_no_build_jobs_exist(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.data_dir = Some(dir.path().to_path_buf());
        state.chats = (0..250).map(chat_row).collect();
        state.spaces = vec![mast_proto::Space {
            id: SPACE.into(),
            device_id: "local".into(),
            path: "/work".into(),
            name: Some("Bench".into()),
            git_detected: true,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        }];
    });
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let st = state.clone();
    let (overview, vcx) = cx.add_window_view(move |_, cx| {
        Overview::new(st, gpui::WeakEntity::new_invalid(), transcript, composer, cx)
    });
    vcx.run_until_parked();
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    };
    overview.update(vcx, |o, _| {
        let mut map = HashMap::new();
        map.insert(
            SPACE.to_string(),
            WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot(40))),
        );
        o.outline_fixture = Some(map);
        o.view_mode = OverviewMode::Outline;
        o.rows_dirty = true;
    });
    // The Overview hands its rows to the Outline entity while it renders, so the Outline's own
    // repaint (where the badge is read) lands on the following frame. Both halves of this test
    // redraw this way, so the "no lookups" half covers the same paint that later counts them.
    let redraw = |vcx: &mut gpui::VisualTestContext| {
        draw(vcx);
        vcx.run_until_parked();
        draw(vcx);
    };
    // Single-chat items compact to a dot with no status text; the badge lives on a full chat row.
    overview.update(vcx, |o, cx| o.set_compact_single_chat(false, cx));
    let now = Utc::now();
    let lookups_before = crate::build_watch::jobs::counts::lookups();
    redraw(vcx);
    assert_eq!(
        crate::build_watch::jobs::counts::lookups(),
        lookups_before,
        "no build jobs: drawing the 250-chat Overview must not reach the badge map"
    );
    let plain = state.read_with(vcx, |state, _| state.overview_fingerprint(now));

    let snapshot_with = |jobs: Vec<mast_proto::BuildJobView>| mast_proto::BuildJobsSnapshot {
        space_id: SPACE.into(),
        jobs,
        quiet_window_active: false,
    };
    // `c1` is never marked read: its finished turn reads `Completed`, and it still gets a badge.
    state.read_with(vcx, |state, _| {
        assert!(state.chats[1].unseen());
        assert_eq!(
            state.display_status_for(&state.chats[1], now),
            mast_proto::ChatIndicator::Completed
        );
    });

    // An empty snapshot is no snapshot: the fingerprint is untouched.
    state.update(vcx, |state, _| state.set_test_build_jobs(snapshot_with(vec![])));
    assert_eq!(state.read_with(vcx, |state, _| state.overview_fingerprint(now)), plain);

    // Finished jobs never leave a snapshot. They show in the popover (the shell's fingerprint),
    // but the Overview hashes and looks up exactly what it does with no builds at all.
    let shell_plain = state.read_with(vcx, |state, _| state.shell_fingerprint(now));
    state.update(vcx, |state, cx| {
        state.set_test_build_jobs(snapshot_with(vec![mast_proto::BuildJobView {
            job_id: "old".into(),
            space_id: SPACE.into(),
            chat_id: "c1".into(),
            state: mast_proto::BuildJobState::Succeeded,
            ..Default::default()
        }]));
        cx.notify();
    });
    state.read_with(vcx, |state, _| {
        assert_eq!(state.overview_fingerprint(now), plain);
        assert_ne!(state.shell_fingerprint(now), shell_plain);
    });
    overview.update(vcx, |o, cx| {
        o.rows_dirty = true;
        cx.notify();
    });
    redraw(vcx);
    assert_eq!(
        crate::build_watch::jobs::counts::lookups(),
        lookups_before,
        "finished jobs only: drawing the Overview must not reach the badge map"
    );

    state.update(vcx, |state, cx| {
        state.set_test_build_jobs(snapshot_with(vec![mast_proto::BuildJobView {
            job_id: "j".into(),
            space_id: SPACE.into(),
            chat_id: "c1".into(),
            state: mast_proto::BuildJobState::Queued,
            position: Some(2),
            ..Default::default()
        }]));
        cx.notify();
    });
    assert_ne!(state.read_with(vcx, |state, _| state.overview_fingerprint(now)), plain);
    overview.update(vcx, |o, cx| {
        o.rows_dirty = true;
        cx.notify();
    });
    redraw(vcx);
    assert!(
        crate::build_watch::jobs::counts::lookups() > lookups_before,
        "a queued job must switch the badge lookups on"
    );

    state.update(vcx, |state, _| state.set_test_build_jobs(snapshot_with(vec![])));
    assert_eq!(state.read_with(vcx, |state, _| state.overview_fingerprint(now)), plain);
}

#[test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_team_300_items_50_chats() {
    let snapshot = snapshot(100);
    let mut chats: Vec<_> = (0..50).map(chat_row).collect();
    for chat in &mut chats[1..] {
        chat.parent_chat_id = Some("c0".into());
    }
    let indicators = HashMap::new();
    let links = HashMap::new();
    let input = crate::team::TeamInput {
        chat_id: "c0",
        chats: &chats,
        indicators: &indicators,
        snapshot: Some(&snapshot),
        links: &links,
        now: Utc::now(),
    };
    measure_n("team", "300 items, 50 chats fact sheet", 100, |_| {
        std::hint::black_box(crate::team::team(&input));
    });
}

/// A large Work Outline: ~500 items (167 roots, each with a PR and a Linear
/// child). Expand/collapse and a status change, each followed by a draw.
/// `BENCH_ROOTS` overrides the root count.
#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_outline_500_items_actions(cx: &mut gpui::TestAppContext) {
    let roots = std::env::var("BENCH_ROOTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(167);
    let (overview, vcx) = outline_fixture_overview(cx, roots);
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    };
    measure("outline", "first draw (Outline, 500 items)", || draw(vcx));
    measure_n("outline", "redraw (clean)", 20, |_| draw(vcx));
    let keys: Vec<String> = (0..20)
        .map(|i| crate::work_outline::model::row_key::item(SPACE, &format!("w-bench-{i}")))
        .collect();
    measure_n("outline", "collapse + draw", keys.len(), |i| {
        overview.update(vcx, |o, cx| {
            o.outline
                .update(cx, |outline, cx| outline.set_collapsed(&keys[i], true, cx))
        });
        draw(vcx);
    });
    measure_n("outline", "expand + draw", keys.len(), |i| {
        overview.update(vcx, |o, cx| {
            o.outline
                .update(cx, |outline, cx| outline.set_collapsed(&keys[i], false, cx))
        });
        draw(vcx);
    });
    // Status change: a new snapshot with one item completed.
    measure_n("outline", "status change (new snapshot) + draw", 10, |i| {
        let mut next = snapshot(roots);
        let id = format!("w-bench-{i}");
        if let Some(item) = next.items.iter_mut().find(|item| item.id == id) {
            item.status = WorkItemStatus::Completed;
        }
        overview.update(vcx, |o, cx| {
            let mut map = HashMap::new();
            map.insert(
                SPACE.to_string(),
                WorkOutlineLoad::Ready(std::sync::Arc::new(next)),
            );
            o.outline_fixture = Some(map);
            o.rows_dirty = true;
            cx.notify();
        });
        draw(vcx);
    });
}

fn outline_fixture_overview(
    cx: &mut gpui::TestAppContext,
    roots: usize,
) -> (Entity<Overview>, &mut gpui::VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.data_dir = Some(dir.path().to_path_buf());
        state.chats = (0..roots * 3).map(chat_row).collect();
        state.spaces = vec![mast_proto::Space {
            id: SPACE.into(),
            device_id: "local".into(),
            path: "/work".into(),
            name: Some("Bench".into()),
            git_detected: true,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        }];
    });
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let st = state.clone();
    let (overview, vcx) = cx.add_window_view(move |_, cx| {
        Overview::new(st, gpui::WeakEntity::new_invalid(), transcript, composer, cx)
    });
    vcx.run_until_parked();
    overview.update(vcx, |o, cx| {
        let mut map = HashMap::new();
        map.insert(
            SPACE.to_string(),
            WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot(roots))),
        );
        o.outline_fixture = Some(map);
        o.view_mode = OverviewMode::Outline;
        o.rows_dirty = true;
        cx.notify();
    });
    std::mem::forget(dir);
    (overview, vcx)
}

/// Rendering Overview (List and Outline) with snapshot data must not panic;
/// gpui panics when one element sets its hover style twice.
#[gpui::test]
fn overview_renders_snapshot_data_in_both_modes_without_panicking(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.data_dir = Some(dir.path().to_path_buf());
        state.chats = (0..12).map(chat_row).collect();
        state.spaces = vec![mast_proto::Space {
            id: SPACE.into(),
            device_id: "local".into(),
            path: "/work".into(),
            name: Some("Preview".into()),
            git_detected: true,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        }];
    });
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let st = state.clone();
    let (overview, vcx) = cx.add_window_view(move |_, cx| {
        Overview::new(st, gpui::WeakEntity::new_invalid(), transcript, composer, cx)
    });
    vcx.run_until_parked();
    overview.update(vcx, |o, _| {
        let mut map = HashMap::new();
        map.insert(
            SPACE.to_string(),
            WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot(3))),
        );
        o.outline_fixture = Some(map);
        o.rows_dirty = true;
    });
    for mode in [OverviewMode::List, OverviewMode::Outline] {
        overview.update(vcx, |o, cx| {
            o.view_mode = mode;
            o.rows_dirty = true;
            cx.notify();
        });
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    }
}
