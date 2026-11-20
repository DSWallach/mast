//! Synthetic busy session for the `AppState` observers: 250 chats, then 50
//! streaming updates to one chat that is not selected and 50 to the selected
//! chat. Each view entity is observed from the outside, so the counts are the
//! notifies (repaints requested) per view, whatever the view does internally.
//! Run with `--nocapture` to print the table.

use super::*;
use gpui::{AppContext, Entity, TestAppContext};
use std::{cell::Cell, rc::Rc};

type Counter = (&'static str, Rc<Cell<u64>>);

#[gpui::test]
fn working_heartbeat_notifies_transcript_without_notifying_sidebar(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        settings::init(settings::UiSettings::default(), dir.path(), cx);
        cx.set_global(Theme::default());
        crate::history::init(Default::default(), Default::default(), Default::default(), Default::default(), cx);
        crate::app_menus::init(cx);
    });
    let now = Utc::now();
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.chats = vec![chat_row(0)];
        state.selected_chat = Some("c0".into());
        state.sessions = vec![mast_proto::Session {
            chat_id: "c0".into(), device_id: "local".into(),
            status: mast_proto::SessionStatus::Working, started_at: Some(now), updated_at: now,
            last_completed_turn: None, awaiting_input: None, last_turn_tool_failures: None, todo_progress: None,
        }];
    });
    let shell = cx.new(|cx| Shell::new(state, EngineBootConfig {
        data_dir: dir.path().to_path_buf(), ipc_port: 0,
        edge_url: "http://127.0.0.1:1".into(), edge_token: None, org_id: None,
        workos_client_id: None, default_harness: mast_proto::HarnessId::Mock,
    }, cx));
    shell.update(cx, |shell, cx| shell.refresh_heartbeat(now, false, cx));
    cx.run_until_parked();
    let (sidebar, transcript) = shell.read_with(cx, |shell, _| (shell.sidebar_pane.clone(), shell.transcript.clone()));
    let counters = vec![watch(cx, "shell", &shell), watch(cx, "sidebar", &sidebar), watch(cx, "transcript", &transcript)];
    shell.update(cx, |shell, cx| shell.refresh_heartbeat(now, false, cx));
    cx.run_until_parked();
    assert_eq!(totals(&counters), vec![("shell", 0), ("sidebar", 0), ("transcript", 1)]);
    shell.update(cx, |shell, cx| {
        shell.running.open = true;
        shell.refresh_heartbeat(now, false, cx);
    });
    cx.run_until_parked();
    assert_eq!(totals(&counters), vec![("shell", 1), ("sidebar", 1), ("transcript", 2)]);
    shell.update(cx, |shell, cx| {
        shell.running.open = false;
        shell.refresh_heartbeat(now + chrono::TimeDelta::seconds(46), false, cx)
    });
    cx.run_until_parked();
    assert_eq!(totals(&counters), vec![("shell", 2), ("sidebar", 2), ("transcript", 3)]);
}

/// Set `MAST_BENCH_OPEN=1` to measure the open list; otherwise measure the closed disclosure.
#[gpui::test]
#[ignore]
fn sidebar_frame_cost(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        settings::init(settings::UiSettings::default(), dir.path(), cx);
        cx.set_global(Theme::default());
        crate::history::init(Default::default(), Default::default(), Default::default(), Default::default(), cx);
        crate::composer::init(cx, Default::default());
        crate::terminal::panel::init(cx);
        crate::app_menus::init(cx);
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.chats = (0..400).map(chat_row).collect();
        state.sessions = (395..400).map(|index| mast_proto::Session {
            last_completed_turn: None,
            awaiting_input: None,
            last_turn_tool_failures: None, todo_progress: None,
            chat_id: format!("c{index}"),
            device_id: "local".into(),
            status: mast_proto::SessionStatus::Working,
            started_at: None,
            updated_at: Utc::now(),
        }).collect();
        state.chats_synced = true;
        state.selected_chat = Some("c0".into());
        state.no_project = true;
        state.data_dir = Some(dir.path().to_path_buf());
    });
    let boot = EngineBootConfig { data_dir: dir.path().to_path_buf(), ipc_port: 0, edge_url: "http://127.0.0.1:1".into(), edge_token: None, org_id: None, workos_client_id: None, default_harness: mast_proto::HarnessId::Mock };
    let (_, vcx) = cx.add_window_view(|_, cx| {
        let mut shell = Shell::new(state, boot, cx);
        shell.route = Route::Chat;
        shell.active_chat = "c0".into();
        shell.debug_gate = Some(GatePhase::Ready);
        shell.settings.sidebar_organization = SidebarOrganization::InOneList;
        shell.sessions_open = std::env::var_os("MAST_BENCH_OPEN").is_some();
        shell
    });
    vcx.update(|window, cx| window.draw(cx).clear());
    let start = std::time::Instant::now();
    for _ in 0..10 {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    }
    eprintln!("BENCH sidebar (open={}): 400 chats, 5 working, 10 frames: {:?}", std::env::var_os("MAST_BENCH_OPEN").is_some(), start.elapsed());
}

fn watch<T: 'static>(cx: &mut TestAppContext, name: &'static str, entity: &Entity<T>) -> Counter {
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    cx.update(|cx| {
        cx.observe(entity, move |_, _| {
            if std::env::var("PERF_BT").is_ok_and(|v| v == name) && seen.get() == 3 {
                eprintln!("{}", std::backtrace::Backtrace::force_capture());
            }
            seen.set(seen.get() + 1)
        })
        .detach()
    });
    (name, count)
}

fn chat_row(index: usize) -> mast_proto::Chat {
    use chrono::TimeZone;
    mast_proto::Chat {
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
        last_message_at: Some(Utc.timestamp_opt(1_700_000_000 + index as i64, 0).unwrap()),
        created_at: Utc.timestamp_opt(1_600_000_000, 0).unwrap(),
        harness_session_id: None,
        harness_session_cwd: None,
        actual_model: None,
        scheduled_task: None,
        scheduled_task_info: None,
        standing_role_successor: None,
        parent_chat_id: None,
        side_of: None,
        side_absorbed_through: None,
        space_id: None,
        last_seen_at: None,
        room_gen: None,
        linked_pr_url: None,
        linked_pr_source: None,
        linked_ticket_id: None,
        linked_ticket_source: None,
    }
}

#[gpui::test]
fn industrial_chat_shell_renders_light_and_dark(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        settings::init(settings::UiSettings::default(), dir.path(), cx);
        crate::ui_style::init(crate::ui_style::UiStyleId::Industrial, cx);
        crate::appearance::init(crate::appearance::AppearanceMode::Light, Default::default(), Default::default(), Default::default(), cx);
        crate::history::init(Default::default(), Default::default(), Default::default(), Default::default(), cx);
        crate::composer::init(cx, Default::default());
        crate::terminal::panel::init(cx);
        crate::app_menus::init(cx);
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.chats = vec![chat_row(0)];
        state.chats_synced = true;
        state.selected_chat = Some("c0".into());
        state.no_project = true;
        state.data_dir = Some(dir.path().to_path_buf());
    });
    let boot = EngineBootConfig { data_dir: dir.path().to_path_buf(), ipc_port: 0, edge_url: "http://127.0.0.1:1".into(), edge_token: None, org_id: None, workos_client_id: None, default_harness: mast_proto::HarnessId::Mock };
    let (shell, vcx) = cx.add_window_view(|_, cx| {
        let mut shell = Shell::new(state.clone(), boot, cx);
        shell.route = Route::Chat;
        shell.active_chat = "c0".into();
        shell.debug_gate = Some(GatePhase::Ready);
        shell
    });
    for mode in [crate::appearance::AppearanceMode::Light, crate::appearance::AppearanceMode::Dark] {
        vcx.update(|window, cx| {
            crate::appearance::set_mode(mode, cx);
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("composer-surface").is_some());
    }
    let sidebar = shell.read_with(vcx, |shell, _| shell.sidebar_pane.clone());
    let before = sidebar.read_with(vcx, |pane, _| pane.renders);
    shell.update(vcx, |shell, cx| {
        shell.settings.sidebar_collapsed = true;
        shell.sidebar_tween = None;
        cx.notify();
    });
    for i in 0..5 {
        state.update(vcx, |state, cx| {
            state.chats[0].title = Some(format!("Changed while hidden {i}"));
            cx.notify();
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    }
    assert_eq!(sidebar.read_with(vcx, |pane, _| pane.renders), before,
        "collapsed sidebar must not render even when its state changes");
    shell.update(vcx, |shell, cx| {
        shell.settings.sidebar_collapsed = false;
        cx.notify();
    });
    vcx.update(|window, cx| { window.refresh(); window.draw(cx).clear(); });
    assert!(sidebar.read_with(vcx, |pane, _| pane.renders) > before,
        "expanding must remount the sidebar with current state");
}

fn totals(counters: &[Counter]) -> Vec<(&'static str, u64)> {
    counters.iter().map(|(n, c)| (*n, c.get())).collect()
}

fn delta(
    before: &[(&'static str, u64)],
    after: &[(&'static str, u64)],
) -> Vec<(&'static str, u64)> {
    before
        .iter()
        .zip(after)
        .map(|((n, b), (_, a))| (*n, a - b))
        .collect()
}

#[gpui::test]
fn streaming_one_chat_among_many_notifies_few_views(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        settings::init(settings::UiSettings::default(), dir.path(), cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let state = cx.new(|_| AppState::new());
    let window = cx.add_window({
        let state = state.clone();
        let data_dir = dir.path().to_path_buf();
        move |_, cx| {
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir,
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: mast_proto::HarnessId::Mock,
                },
                cx,
            )
        }
    });
    state.update(cx, |state, _| {
        state.chats = (0..250).map(chat_row).collect();
        state.chats_synced = true;
        state.selected_chat = Some("c0".into());
        state.data_dir = Some(dir.path().to_path_buf());
    });
    let shell = window.root(cx).unwrap();
    let (transcript, composer) =
        shell.read_with(cx, |s, _| (s.transcript.clone(), s.composer.clone()));
    let pickers = composer.read_with(cx, |c, _| c.pickers().clone());
    let weak = shell.downgrade();
    let chat_types = cx.new(|cx| {
        crate::chat_type_view::ChatTypeView::new(
            state.clone(),
            weak.clone(),
            crate::chat_type_view::ChatViewKind::PrReview,
            cx,
        )
    });
    let accounts = cx.new(|cx| crate::settings::accounts::AccountsPage::new(state.clone(), cx));
    let archived = cx.new(|cx| crate::settings::archived::ArchivedPage::new(state.clone(), cx));
    let devices = cx.new(|cx| crate::settings::devices::DevicesPage::new(state.clone(), cx));
    let linear =
        cx.new(|cx| crate::linear::settings_page::LinearSettingsPage::new(state.clone(), cx));
    let changes = cx.new(|cx| crate::changes::Changes::new(state.clone(), cx));
    let terminal = cx.new(|cx| crate::terminal::panel::TerminalPanel::new(state.clone(), cx));
    let files = cx
        .new(|cx| crate::files::FilesSurface::new_explorer(state.clone(), "c0".into(), false, cx));
    cx.run_until_parked();

    let counters = vec![
        watch(cx, "shell", &shell),
        watch(cx, "transcript", &transcript),
        watch(cx, "composer", &composer),
        watch(cx, "pickers", &pickers),
        watch(cx, "chat_type_view", &chat_types),
        watch(cx, "settings accounts", &accounts),
        watch(cx, "settings archived", &archived),
        watch(cx, "settings devices", &devices),
        watch(cx, "settings linear", &linear),
        watch(cx, "changes", &changes),
        watch(cx, "terminal panel", &terminal),
        watch(cx, "files", &files),
    ];
    let start = totals(&counters);

    // A: a live heartbeat for a chat nobody has open: its session row's
    // lease timestamp moves, no visible field does.
    for i in 0..50 {
        state.update(cx, |state, cx| {
            state.sessions = vec![mast_proto::Session {
                last_completed_turn: None,
                awaiting_input: None,
                last_turn_tool_failures: None, todo_progress: None,
                chat_id: "c1".into(),
                device_id: "local".into(),
                status: mast_proto::SessionStatus::Working,
                started_at: None,
                updated_at: Utc::now() + chrono::TimeDelta::milliseconds(i),
            }];
            cx.notify();
        });
        cx.run_until_parked();
    }
    let after_heartbeats = totals(&counters);

    // B: the same chat's row gets new preview text each time (a real row
    // change, visible in lists that show previews).
    for i in 0..50 {
        state.update(cx, |state, cx| {
            state.chats[1].last_message_preview = Some(format!("preview {i}"));
            cx.notify();
        });
        cx.run_until_parked();
    }
    let after_previews = totals(&counters);

    // C: streaming into the selected chat: its transcript changes, no
    // chat/space/session field does.
    for i in 0..50 {
        state.update(cx, |state, cx| {
            state.transcript.push(mast_doc::SessionMessageEntry {
                id: format!("m{i}"),
                role: mast_doc::MessageRole::Assistant,
                parts: vec![mast_doc::MessagePart::Text {
                    id: "t0".into(),
                    text: format!("token {i}"),
                }],
                created_at: i,
                device_id: "local".into(),
                status: None,
                continuation_of: None,
            });
            state.transcript_revision = state.transcript_revision.wrapping_add(1);
            cx.notify();
        });
        cx.run_until_parked();
    }
    let after_selected = totals(&counters);

    for i in 0..50 {
        state.update(cx, |state, cx| {
            state.chats[0].last_message_preview = Some(format!("selected preview {i}"));
            state.chats[0].last_seen_at = Some(Utc::now());
            cx.notify();
        });
        cx.run_until_parked();
    }
    let selected_previews = delta(&after_selected, &totals(&counters));
    for (name, count) in &selected_previews {
        if matches!(*name, "composer" | "pickers") {
            assert_eq!(
                *count, 0,
                "{name} must ignore preview and read-marker changes"
            );
        }
    }
    let before_config = totals(&counters);
    state.update(cx, |state, cx| {
        state.chats[0].actual_model = Some("updated-model".into());
        cx.notify();
    });
    cx.run_until_parked();
    for (name, count) in delta(&before_config, &totals(&counters)) {
        if matches!(name, "composer" | "pickers") {
            assert!(count > 0, "{name} must follow model changes");
        }
    }

    eprintln!("PERF notifies per view (50 updates each)");
    eprintln!(
        "{:<22}{:>12}{:>12}{:>12}",
        "view", "heartbeat", "preview", "selected"
    );
    let a = delta(&start, &after_heartbeats);
    let b = delta(&after_heartbeats, &after_previews);
    let c = delta(&after_previews, &after_selected);
    for (((name, a), (_, b)), (_, c)) in a.iter().zip(&b).zip(&c) {
        eprintln!("{name:<22}{a:>12}{b:>12}{c:>12}");
    }
}

/// UI initialization and a headless shell with a preloaded synthetic chat list.
/// This excludes engine bootstrap and registry reads; see performance-sweep.md.
#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_shell_initialization(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let settings = crate::perf::measure("startup", "load settings", || {
        settings::UiSettings::load(dir.path())
    });
    cx.update(|cx| {
        gpui_base::init(cx);
        settings::init(settings.clone(), dir.path(), cx);
        let fonts = crate::perf::measure("startup", "register fonts", || {
            crate::typography::register_fonts(cx)
        });
        crate::typography::init(
            settings.ui_font_family.clone(),
            settings.ui_font_size,
            settings.terminal_font_family.clone(),
            settings.terminal_font_size,
            settings.code_font_family.clone(),
            settings.code_font_size,
            fonts,
            cx,
        );
        crate::perf::measure("startup", "load themes", || {
            crate::theme_library::init(dir.path(), cx)
        });
        crate::appearance::init(
            settings.appearance,
            settings.theme_selection.clone(),
            settings.accent,
            settings.surface,
            cx,
        );
        crate::history::init(
            settings.git_history_columns,
            settings.git_history_column_widths,
            settings.git_history_column_order,
            settings.git_history_author_display,
            cx,
        );
        crate::composer::init(cx, settings.composer_send_behavior);
        crate::terminal::panel::init(cx);
        crate::app_menus::init(cx);
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.chats = (0..250).map(chat_row).collect();
        state.chats_synced = true;
        state.data_dir = Some(dir.path().to_path_buf());
    });
    let boot = EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_port: 0,
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: mast_proto::HarnessId::Mock,
    };
    let (_, vcx) = crate::perf::measure("startup", "construct shell window", || {
        cx.add_window_view(|_, cx| Shell::new(state.clone(), boot, cx))
    });
    crate::perf::measure("startup", "first shell draw", || {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    });
}

/// A profile-backed startup including registry loading and the first sidebar data.
#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_profile_startup(cx: &mut TestAppContext) {
    crate::perf::require_offline_executables();
    use mast_proto::{
        CreateWorkItemParams, WorkOutlineActor, WorkOutlineActorKind, WorkOutlineMutation,
        WorkOutlinePlacement,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    let dir = tempfile::tempdir().unwrap();
    let profile = mast_engine::EngineProfile::development(dir.path(), "benchmark-org", "dev-user");
    let mut registry = mast_doc::RegistryDoc::new("benchmark-device");
    registry
        .upsert_space(&mast_proto::Space {
            id: "benchmark-space".into(),
            device_id: "benchmark-device".into(),
            path: dir.path().to_string_lossy().into_owned(),
            name: Some("Benchmark".into()),
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        })
        .unwrap();
    let mut chats: Vec<_> = (0..250).map(chat_row).collect();
    for chat in &mut chats {
        chat.device_id = "benchmark-device".into();
        chat.space_id = Some("benchmark-space".into());
        registry.upsert_chat(chat).unwrap();
    }
    let actor = WorkOutlineActor {
        kind: WorkOutlineActorKind::User,
        device_id: "benchmark-device".into(),
        chat_id: None,
        message_id: None,
    };
    for i in 0..300 {
        registry
            .apply_work_outline_mutation(
                &WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
                    space_id: "benchmark-space".into(),
                    parent_work_item_id: None,
                    placement: WorkOutlinePlacement::Last,
                    title: format!("Benchmark item {i}"),
                    description: None,
                    status: None,
                    operation_id: format!("benchmark-item-{i}"),
                }),
                &actor,
                1_700_000_000_000 + i,
            )
            .unwrap();
    }
    // Accumulated pending updates make the serialized registry approximately
    // 50 MiB while preserving 250 visible chats and 300 visible work items.
    let padding = "x".repeat(8192);
    for i in 0..6000 {
        chats[0].last_message_preview = Some(format!("{i}:{padding}"));
        registry.upsert_chat(&chats[0]).unwrap();
    }
    chats[0].last_message_preview = None;
    registry.upsert_chat(&chats[0]).unwrap();
    let bytes = registry.to_bytes().unwrap();
    eprintln!("BENCH startup | registry bytes={}", bytes.len());
    assert!((45_000_000..65_000_000).contains(&bytes.len()));
    let store = mast_sync::DocsStore::open(profile.store_root()).unwrap();
    store
        .save_snapshot(mast_doc::REGISTRY_DOC_ID, &bytes)
        .unwrap();
    assert_eq!(
        store
            .load_snapshot(mast_doc::REGISTRY_DOC_ID)
            .unwrap()
            .unwrap()
            .len(),
        bytes.len()
    );
    drop((bytes, registry, store));
    let started = std::time::Instant::now();
    let settings_started = std::time::Instant::now();
    let settings = settings::UiSettings::load(dir.path());
    eprintln!("BENCH phase | settings_keymap | ms={:.3}", settings_started.elapsed().as_secs_f64() * 1000.0);
    cx.update(|cx| {
        gpui_tokio::init_from_handle(cx, runtime.handle().clone());
        gpui_base::init(cx);
        settings::init(settings.clone(), dir.path(), cx);
        let fonts = crate::typography::register_fonts(cx);
        crate::typography::init(
            settings.ui_font_family.clone(),
            settings.ui_font_size,
            settings.terminal_font_family.clone(),
            settings.terminal_font_size,
            settings.code_font_family.clone(),
            settings.code_font_size,
            fonts,
            cx,
        );
        crate::theme_library::init(dir.path(), cx);
        crate::appearance::init(
            settings.appearance,
            settings.theme_selection.clone(),
            settings.accent,
            settings.surface,
            cx,
        );
        crate::history::init(
            settings.git_history_columns,
            settings.git_history_column_widths,
            settings.git_history_column_order,
            settings.git_history_author_display,
            cx,
        );
        crate::composer::init(cx, settings.composer_send_behavior);
        crate::terminal::panel::init(cx);
        crate::app_menus::init(cx);
    });
    let state = cx.new(|_| AppState::new());
    let boot = EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_port: 0,
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: Some("benchmark-org".into()),
        workos_client_id: None,
        default_harness: mast_proto::HarnessId::Mock,
    };
    cx.update(|cx| AppState::bootstrap(state.clone(), boot.clone(), cx));
    let (_, vcx) = cx.add_window_view(|_, cx| Shell::new(state.clone(), boot, cx));
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    eprintln!(
        "BENCH startup | initial shell draw | ms={:.2}",
        started.elapsed().as_secs_f64() * 1000.0
    );
    state.update(vcx, |state, cx| {
        state.declare_work_outline_visible(vec!["benchmark-space".into()], cx)
    });
    let readiness_started = std::time::Instant::now();
    let mut scheduler_time = std::time::Duration::ZERO;
    let mut connection_recorded = false;
    let mut chats_recorded = false;
    let mut outline_recorded = false;
    loop {
        let tokio_started = std::time::Instant::now();
        let tokio_wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros();
        runtime.block_on(tokio::task::yield_now());
        let tokio_elapsed = tokio_started.elapsed();
        if tokio_elapsed.as_millis() >= 1 {
            eprintln!("BENCH scheduler | tokio | start_us={tokio_wall} duration_us={}", tokio_elapsed.as_micros());
        }
        let scheduler_started = std::time::Instant::now();
        let scheduler_wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros();
        loop {
            let tick_started = std::time::Instant::now();
            let tick_wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros();
            let progressed = vcx.executor().tick();
            let tick_elapsed = tick_started.elapsed();
            if tick_elapsed.as_millis() >= 1 {
                eprintln!("BENCH scheduler | gpui_tick | start_us={tick_wall} duration_us={}", tick_elapsed.as_micros());
            }
            if !progressed { break; }
        }
        let scheduler_elapsed = scheduler_started.elapsed();
        if scheduler_elapsed.as_millis() >= 1 {
            eprintln!("BENCH scheduler | gpui | start_us={scheduler_wall} duration_us={}", scheduler_elapsed.as_micros());
        }
        scheduler_time += scheduler_elapsed;
        state.read_with(vcx, |state, _| {
            if !connection_recorded && matches!(state.connection, crate::state::ConnectionStatus::Ready) {
                eprintln!("BENCH milestone | engine_rpc_ready | ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
                eprintln!("BENCH event | ready_observed | us={}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros());
                connection_recorded = true;
            }
            if !chats_recorded && state.chats.len() == 250 {
                eprintln!("BENCH milestone | chats_loaded | ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
                eprintln!("BENCH event | chats_observed | us={}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros());
                chats_recorded = true;
            }
            if !outline_recorded && state.work_outline("benchmark-space").snapshot().is_some_and(|snapshot| snapshot.items.len() == 300) {
                eprintln!("BENCH milestone | outline_loaded | ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
                eprintln!("BENCH event | outline_observed | us={}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros());
                outline_recorded = true;
            }
        });
        if state.read_with(vcx, |state, _| {
            state.chats.len() == 250
                && state
                    .work_outline("benchmark-space")
                    .snapshot()
                    .is_some_and(|snapshot| snapshot.items.len() == 300)
        }) {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(120),
            "profile startup did not load sidebar data"
        );
        std::thread::yield_now();
    }
    eprintln!(
        "BENCH startup | readiness wait | ms={:.2} scheduler_ms={:.2}",
        readiness_started.elapsed().as_secs_f64() * 1000.0,
        scheduler_time.as_secs_f64() * 1000.0
    );
    let draw_started = std::time::Instant::now();
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    eprintln!(
        "BENCH startup | forced draw alone | ms={:.2}",
        draw_started.elapsed().as_secs_f64() * 1000.0
    );
    eprintln!(
        "BENCH startup | loaded sidebar draw | ms={:.2}",
        started.elapsed().as_secs_f64() * 1000.0
    );
}

/// Cost of one sidebar redraw with a long one-list session history, the case
/// any Shell notification pays while the sidebar is mounted.
#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_sidebar_redraw_with_many_sessions(cx: &mut TestAppContext) {
    struct SidebarHost(Entity<Shell>);
    impl Render for SidebarHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |shell, cx| {
                div()
                    .w(px(320.0))
                    .h(px(900.0))
                    .child(shell.render_chat_sidebar(&Theme::default(), cx))
            })
        }
    }
    const SESSIONS: usize = 1_000;
    const REDRAWS: u32 = 30;
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        settings::init(settings::UiSettings::default(), dir.path(), cx);
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
    });
    let (host, vcx) = cx.add_window_view(|_, cx| {
        SidebarHost(cx.new(|cx| {
            let state = cx.new(|_| AppState::new());
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: mast_proto::HarnessId::Mock,
                },
                cx,
            );
            shell.settings.sidebar_organization = SidebarOrganization::InOneList;
            shell.settings.sidebar_compact = true;
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
                state.local_device_id = Some("local".into());
                state.chats = (0..SESSIONS).map(chat_row).collect();
                state.chats_synced = true;
            });
            shell
        }))
    });
    let shell = host.read_with(vcx, |host, _| host.0.clone());
    for _ in 0..3 {
        shell.update(vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
    }
    let started = std::time::Instant::now();
    for _ in 0..REDRAWS {
        shell.update(vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
    }
    eprintln!(
        "BENCH sidebar | {SESSIONS} sessions | ms_per_redraw={:.2}",
        started.elapsed().as_secs_f64() * 1000.0 / f64::from(REDRAWS)
    );
}

/// Counters for one sidebar measurement window.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct SidebarCounts {
    shell: u64,
    sidebar_pane: u64,
    chat_rows: u64,
    spinner_views: u64,
    chat_scans: u64,
    session_scans: u64,
    sorts: u64,
    fingerprint_chats: u64,
}

fn sidebar_counts(sidebar: &Entity<SidebarPane>, cx: &mut gpui::VisualTestContext) -> SidebarCounts {
    SidebarCounts {
        shell: perf_counts::SHELL_RENDERS.with(|c| c.get()),
        sidebar_pane: sidebar.read_with(cx, |pane, _| pane.renders as u64),
        chat_rows: perf_counts::CHAT_ROW_BUILDS.with(|c| c.get()),
        spinner_views: crate::loaders::MINI_SPINNER_RENDERS.with(|c| c.get()),
        chat_scans: perf_counts::CHAT_ROW_SCANS.with(|c| c.get()),
        session_scans: perf_counts::SESSION_SCANS.with(|c| c.get()),
        sorts: perf_counts::SIDEBAR_SORTS.with(|c| c.get()),
        fingerprint_chats: perf_counts::FINGERPRINT_CHATS.with(|c| c.get()),
    }
}

fn sidebar_delta(a: SidebarCounts, b: SidebarCounts) -> SidebarCounts {
    SidebarCounts {
        shell: b.shell - a.shell,
        sidebar_pane: b.sidebar_pane - a.sidebar_pane,
        chat_rows: b.chat_rows - a.chat_rows,
        spinner_views: b.spinner_views - a.spinner_views,
        chat_scans: b.chat_scans - a.chat_scans,
        session_scans: b.session_scans - a.session_scans,
        sorts: b.sorts - a.sorts,
        fingerprint_chats: b.fingerprint_chats - a.fingerprint_chats,
    }
}

const BUDGET_CHATS: usize = 360;
const BUDGET_WORKING: usize = 5;
const BUDGET_FRAMES: u64 = 30;

/// 360 chats across 50 checkouts in an open sidebar, 5 working. Returns the shell, its sidebar pane
/// and the window context after the first frames have settled.
fn sidebar_budget_fixture(
    cx: &mut TestAppContext,
) -> (Entity<Shell>, Entity<SidebarPane>, Entity<AppState>, &mut gpui::VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        settings::init(settings::UiSettings::default(), dir.path(), cx);
        cx.set_global(Theme::default());
        crate::history::init(Default::default(), Default::default(), Default::default(), Default::default(), cx);
        crate::composer::init(cx, Default::default());
        crate::terminal::panel::init(cx);
        crate::app_menus::init(cx);
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.spaces = (0..50).map(|index| mast_proto::Space {
            id: format!("space{index}"), device_id: "local".into(),
            path: format!("/work/repo{index}"), name: None, git_detected: true,
            git_checked_at: None, checkout_id: Some(format!("checkout{index}")),
            created_at: Utc::now(),
        }).collect();
        state.chats = (0..BUDGET_CHATS).map(|index| {
            let mut chat = chat_row(index);
            chat.space_id = Some(format!("space{}", index % 50));
            chat.checkout_id = Some(format!("checkout{}", index % 50));
            chat.cwd = Some(format!("/work/repo{}", index % 50));
            chat
        }).collect();
        state.sessions = (0..BUDGET_CHATS)
            .map(|index| mast_proto::Session {
                last_completed_turn: None,
                awaiting_input: None,
                last_turn_tool_failures: None, todo_progress: None,
                chat_id: format!("c{index}"),
                device_id: "local".into(),
                status: if index >= BUDGET_CHATS - BUDGET_WORKING { mast_proto::SessionStatus::Working } else { mast_proto::SessionStatus::Idle },
                started_at: None,
                updated_at: Utc::now(),
            })
            .collect();
        state.chats_synced = true;
        state.selected_chat = Some("c0".into());
        state.no_project = true;
        state.data_dir = Some(dir.path().to_path_buf());
    });
    let boot = EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_port: 0,
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: mast_proto::HarnessId::Mock,
    };
    let (shell, vcx) = cx.add_window_view(|_, cx| {
        let mut shell = Shell::new(state.clone(), boot, cx);
        shell.route = Route::Chat;
        shell.active_chat = "c0".into();
        shell.debug_gate = Some(GatePhase::Ready);
        shell.settings.sidebar_organization = SidebarOrganization::InOneList;
        shell.settings.sidebar_overview = false;
        shell.sessions_open = true;
        shell.splash = SplashPhase::Gone;
        shell
    });
    std::mem::forget(dir);
    let sidebar = shell.read_with(vcx, |shell, _| shell.sidebar_pane.clone());
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
    }
    (shell, sidebar, state, vcx)
}

/// Advance the animation clock one pulse tick and draw one frame.
fn animation_frame(vcx: &mut gpui::VisualTestContext) {
    vcx.executor().advance_clock(std::time::Duration::from_millis(34));
    vcx.run_until_parked();
}

#[gpui::test]
fn sidebar_spinner_frames_rerender_only_spinners(cx: &mut TestAppContext) {
    let (shell, sidebar, state, vcx) = sidebar_budget_fixture(cx);
    let watched = vec![watch(vcx, "shell", &shell), watch(vcx, "state", &state), watch(vcx, "sidebar", &sidebar)];
    // Settle one-time work (row measuring, culling) before counting.
    for _ in 0..8 {
        animation_frame(vcx);
    }
    perf_counts::CHAT_ROW_BY_ID.with(|m| m.borrow_mut().clear());
    let before = sidebar_counts(&sidebar, vcx);
    let started = std::time::Instant::now();
    for _ in 0..BUDGET_FRAMES {
        animation_frame(vcx);
    }
    eprintln!("BUDGET spinner ms/frame: {:.3}", started.elapsed().as_secs_f64() * 1000.0 / BUDGET_FRAMES as f64);
    eprintln!("BUDGET notifies: {:?}", totals(&watched));
    let spent = sidebar_delta(before, sidebar_counts(&sidebar, vcx));
    eprintln!("BUDGET spinner frames ({BUDGET_FRAMES}): {spent:?}");
    assert!(spent.spinner_views > 0, "fixture must have visible spinners: {spent:?}");
    // Per row: an animated row may re-render once per frame; every other row
    // only on the occasional shell-wide notify, never per tick.
    let by_id = perf_counts::CHAT_ROW_BY_ID.with(|m| m.borrow().clone());
    let first_working = BUDGET_CHATS - BUDGET_WORKING;
    for (id, builds) in &by_id {
        let ix: usize = id[1..].parse().unwrap();
        let cap = if ix >= first_working { BUDGET_FRAMES + 2 } else { BUDGET_FRAMES / 4 };
        assert!(*builds <= cap, "row {id} rebuilt {builds} times in {BUDGET_FRAMES} spinner frames (cap {cap}): {by_id:?}");
    }
    assert!(
        spent.spinner_views <= 2 * BUDGET_FRAMES * BUDGET_WORKING as u64,
        "more spinner renders than animated rows × frames (own tick + row re-render): {spent:?}"
    );
}

#[test]
fn sidebar_batch_status_matches_individual_lookup() {
    let now = Utc::now();
    let mut state = AppState::new();
    state.chats = (0..3).map(chat_row).collect();
    let session = |chat_id: &str, status, updated_at| mast_proto::Session {
        chat_id: chat_id.into(), device_id: "local".into(), status, started_at: None,
        updated_at, last_completed_turn: None, awaiting_input: None,
        last_turn_tool_failures: None, todo_progress: None,
    };
    state.sessions = vec![
        session("c0", mast_proto::SessionStatus::Working, now),
        session("c0", mast_proto::SessionStatus::Errored, now),
        session("c1", mast_proto::SessionStatus::Working, now - chrono::TimeDelta::minutes(5)),
    ];
    for (status, chat) in state.overview_chats(now) {
        assert_eq!(status, state.display_status_for(chat, now), "chat {}", chat.id);
    }
}

#[test]
fn model_options_hash_cost() {
    use std::hash::{Hash, Hasher};
    let options = serde_json::json!({"effort": "high", "features": {"search": true, "limit": 32}, "tools": ["shell", "edit"]});
    let options = options.as_object().unwrap();
    let iterations = BUDGET_CHATS * BUDGET_FRAMES as usize;
    let started = std::time::Instant::now();
    for _ in 0..iterations {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        serde_json::Value::Object(options.clone()).to_string().hash(&mut hash);
        std::hint::black_box(hash.finish());
    }
    let serialized = started.elapsed();
    let started = std::time::Instant::now();
    for _ in 0..iterations {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        options.hash(&mut hash);
        std::hint::black_box(hash.finish());
    }
    eprintln!("BUDGET model options ({iterations} hashes): serialized={serialized:?}, reference={:?}", started.elapsed());
}

#[gpui::test]
fn sidebar_hover_frame_cost(cx: &mut TestAppContext) {
    let (shell, sidebar, _state, vcx) = sidebar_budget_fixture(cx);
    vcx.update(|_, cx| crate::motion::set_reduced_motion(cx, true));
    for _ in 0..4 { animation_frame(vcx); }
    let centers: Vec<_> = ["chat-c359", "chat-c358", "chat-c357", "chat-c356", "chat-c355", "chat-c354"]
        .into_iter()
        .filter_map(|id| vcx.debug_bounds(id).map(|bounds| (id, bounds.center())))
        .take(2).collect();
    assert_eq!(centers.len(), 2, "fixture must expose two chat rows");
    let before = sidebar_counts(&sidebar, vcx);
    let started = std::time::Instant::now();
    for index in 0..BUDGET_FRAMES {
        let (id, center) = centers[index as usize % 2];
        let move_before = sidebar_counts(&sidebar, vcx);
        vcx.simulate_mouse_move(center, None, gpui::Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        let move_spent = sidebar_delta(move_before, sidebar_counts(&sidebar, vcx));
        assert!(move_spent.chat_rows <= 2, "hover rebuilt more than two rows: {move_spent:?}");
        assert_eq!(shell.read_with(vcx, |shell, _| shell.chat_status_hover.clone()).as_deref(), Some(id));
        assert_eq!(motion::hover_t(&format!("{id}-hover")), 1.0, "hover wash must reach its endpoint with reduced motion");
        if index > 0 {
            let (previous_id, _) = centers[(index as usize - 1) % 2];
            assert_eq!(motion::hover_t(&format!("{previous_id}-hover")), 0.0, "the previous row must clear its hover wash");
        }
    }
    let spent = sidebar_delta(before, sidebar_counts(&sidebar, vcx));
    eprintln!("BUDGET hover ms/frame: {:.3}; {spent:?}", started.elapsed().as_secs_f64() * 1000.0 / BUDGET_FRAMES as f64);
}

/// One streamed token into the selected chat: only its transcript changes.
fn stream_token(state: &Entity<AppState>, vcx: &mut gpui::VisualTestContext, i: u64) {
    state.update(vcx, |state, cx| {
        state.transcript.push(mast_doc::SessionMessageEntry {
            id: format!("m{i}"),
            role: mast_doc::MessageRole::Assistant,
            parts: vec![mast_doc::MessagePart::Text { id: "t0".into(), text: format!("token {i}") }],
            created_at: i as i64,
            device_id: "local".into(),
            status: None,
            continuation_of: None,
        });
        state.transcript_revision = state.transcript_revision.wrapping_add(1);
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
}

#[gpui::test]
fn sidebar_unrelated_transcript_delta_rebuilds_no_rows(cx: &mut TestAppContext) {
    let (_shell, sidebar, state, vcx) = sidebar_budget_fixture(cx);
    // Static spinners: this test is about data changes, not animation.
    vcx.update(|_, cx| crate::motion::set_reduced_motion(cx, true));
    for _ in 0..4 {
        animation_frame(vcx);
    }
    // Settle the one-off pass that follows the reduced-motion switch.
    for i in 0..4 {
        stream_token(&state, vcx, 1000 + i);
    }
    let before = sidebar_counts(&sidebar, vcx);
    let t0 = std::time::Instant::now();
    for i in 0..BUDGET_FRAMES {
        stream_token(&state, vcx, i);
    }
    eprintln!("BUDGET delta loop wall: {:?}", t0.elapsed());
    let spent = sidebar_delta(before, sidebar_counts(&sidebar, vcx));
    eprintln!("BUDGET unrelated deltas ({BUDGET_FRAMES}): {spent:?}");
    assert_eq!(spent.chat_rows, 0, "unrelated delta rebuilt chat rows: {spent:?}");
    assert_eq!(spent.spinner_views, 0, "unrelated delta re-rendered spinners: {spent:?}");
}


/// With nothing animating the sidebar pane stays a cached view: streaming into
/// the selected chat does not re-render it at all.
#[gpui::test]
fn idle_sidebar_ignores_transcript_deltas(cx: &mut TestAppContext) {
    let (_shell, sidebar, state, vcx) = sidebar_budget_fixture(cx);
    state.update(vcx, |state, cx| {
        state.sessions.clear();
        cx.notify();
    });
    for i in 0..8 {
        animation_frame(vcx);
        stream_token(&state, vcx, 2000 + i);
    }
    let before = sidebar_counts(&sidebar, vcx);
    for i in 0..BUDGET_FRAMES {
        stream_token(&state, vcx, i);
    }
    let spent = sidebar_delta(before, sidebar_counts(&sidebar, vcx));
    eprintln!("BUDGET idle deltas ({BUDGET_FRAMES}): {spent:?}");
    // Allow the occasional shell-wide notify that is not caused by a delta.
    assert!(spent.sidebar_pane <= BUDGET_FRAMES / 10, "idle sidebar re-rendered on transcript deltas: {spent:?}");
    assert!(spent.chat_rows <= 64 * spent.sidebar_pane, "{spent:?}");
}

// ---------------------------------------------------------------------------
// Long chat: open, scroll, stream and type in a full Shell.
// ---------------------------------------------------------------------------

fn load_long_chat(
    state: &Entity<AppState>,
    chat_id: &str,
    entries: Vec<mast_doc::SessionMessageEntry>,
    vcx: &mut gpui::VisualTestContext,
) {
    // Like the app: the watch prepares rows off the UI thread and installs them.
    let update = mast_doc::TranscriptUpdate {
        frame: mast_doc::TranscriptFrame::reset(&entries),
        context_usage: None,
        replay_baseline: Some(mast_doc::TranscriptBaseline::capture(&entries)),
    };
    let prepared = crate::transcript::TranscriptPreparation::default()
        .prepare(&update)
        .unwrap();
    state.update(vcx, |state, cx| {
        state.selected_chat = Some(chat_id.into());
        state.transcript_replayed = true;
        state.transcript = entries;
        state.transcript_revision = state.transcript_revision.wrapping_add(1);
        state.prepared_transcripts.insert(chat_id.into(), prepared);
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
}

/// Streamed deltas prepared ahead of time, like the watch's background worker:
/// each is a wire frame plus the rows prepared for the state after it.
fn prepare_stream(
    entries: &[mast_doc::SessionMessageEntry],
    n: usize,
) -> Vec<(mast_doc::TranscriptFrame, std::sync::Arc<crate::transcript::PreparedTranscript>)> {
    let mut worker = crate::transcript::TranscriptPreparation::default();
    let mut current = entries.to_vec();
    if let Some(last) = current.last_mut() {
        last.status = Some(mast_doc::MessageStatus::Streaming);
    }
    worker
        .prepare(&mast_doc::TranscriptUpdate {
            frame: mast_doc::TranscriptFrame::reset(&current),
            context_usage: None,
            replay_baseline: Some(mast_doc::TranscriptBaseline::capture(&current)),
        })
        .unwrap();
    (0..n)
        .map(|_| {
            let before = current.clone();
            if let Some(mast_doc::MessagePart::Text { text, .. }) =
                current.last_mut().and_then(|last| last.parts.last_mut())
            {
                text.push_str(" chunk");
            }
            let frame = mast_doc::diff_transcript(&before, &current);
            let prepared = worker
                .prepare(&mast_doc::TranscriptUpdate {
                    frame: frame.clone(),
                    context_usage: None,
                    replay_baseline: None,
                })
                .unwrap();
            (frame, prepared)
        })
        .collect()
}

/// Mark the chat's last entry streaming (the state the prepared stream starts from).
fn start_streaming(state: &Entity<AppState>, vcx: &mut gpui::VisualTestContext, entries: Vec<mast_doc::SessionMessageEntry>) {
    let mut entries = entries;
    if let Some(last) = entries.last_mut() {
        last.status = Some(mast_doc::MessageStatus::Streaming);
    }
    load_long_chat(state, "c0", entries, vcx);
}

/// One streamed delta, applied like the watch closure does.
fn apply_delta(
    state: &Entity<AppState>,
    vcx: &mut gpui::VisualTestContext,
    frame: mast_doc::TranscriptFrame,
    prepared: std::sync::Arc<crate::transcript::PreparedTranscript>,
) {
    state.update(vcx, |state, cx| {
        state.receive_transcript_frame(frame, cx).unwrap();
        state.prepared_transcripts.insert("c0".into(), prepared);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
}

fn type_key(composer_input: &Entity<crate::composer::ComposerInput>, vcx: &mut gpui::VisualTestContext, i: usize) {
    composer_input.update(vcx, |input, cx| input.set_text(format!("typing a message {i}"), cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
}

struct LongChatCounts {
    rows_rendered: usize,
    notifies: Vec<(&'static str, u64)>,
    ms: f64,
}

fn measure_long_chat<R>(
    counters: &[Counter],
    f: impl FnOnce() -> R,
) -> (R, LongChatCounts) {
    let rows_before = crate::transcript::rendered_row_calls();
    let before = totals(counters);
    let started = std::time::Instant::now();
    let out = f();
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let notifies = delta(&before, &totals(counters));
    (
        out,
        LongChatCounts {
            rows_rendered: crate::transcript::rendered_row_calls() - rows_before,
            notifies,
            ms,
        },
    )
}

fn long_chat_fixture(
    cx: &mut TestAppContext,
    entries: usize,
) -> (
    Entity<Shell>,
    Entity<AppState>,
    &mut gpui::VisualTestContext,
    Vec<Counter>,
    Entity<crate::composer::ComposerInput>,
) {
    let (shell, _sidebar, state, vcx) = sidebar_budget_fixture(cx);
    // Wide enough for the transcript rail (it hides below 768px of content).
    vcx.simulate_resize(gpui::size(px(1800.0), px(1000.0)));
    state.update(vcx, |state, cx| {
        state.sessions.clear();
        cx.notify();
    });
    load_long_chat(&state, "c0", crate::transcript::perf_bench::long_chat(entries), vcx);
    for _ in 0..4 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
    }
    let (transcript, composer) = shell.read_with(vcx, |s, _| (s.transcript.clone(), s.composer.clone()));
    let input = composer.read_with(vcx, |c, _| c.input.clone());
    let sidebar = shell.read_with(vcx, |s, _| s.sidebar_pane.clone());
    let counters = vec![
        watch(vcx, "shell", &shell),
        watch(vcx, "transcript", &transcript),
        watch(vcx, "composer", &composer),
        watch(vcx, "input", &input),
        watch(vcx, "sidebar", &sidebar),
    ];
    (shell, state, vcx, counters, input)
}

/// Scenarios 1, 2 and 4 in one table. `BENCH_ENTRIES` sets the chat size
/// (default 360 entries, about 3,000 rows).
#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_long_chat_scenarios(cx: &mut TestAppContext) {
    let entries: usize = std::env::var("BENCH_ENTRIES").ok().and_then(|v| v.parse().ok()).unwrap_or(360);
    let (shell, state, vcx, counters, input) = long_chat_fixture(cx, entries);
    let transcript = shell.read_with(vcx, |s, _| s.transcript.clone());
    let rows = transcript.read_with(vcx, |t, _| t.rows().len());
    eprintln!("PERF long chat: {entries} entries, {rows} rows");
    let report = |label: &str, n: usize, c: LongChatCounts| {
        eprintln!(
            "PERF {label:<34} n={n:<4} ms/op={:>8.3} rows_rendered/op={:>8.1} notifies/op={:?}",
            c.ms / n as f64,
            c.rows_rendered as f64 / n as f64,
            c.notifies.iter().map(|(k, v)| format!("{k}={:.1}", *v as f64 / n as f64)).collect::<Vec<_>>()
        );
    };
    let (_, c) = measure_long_chat(&counters, || {
        load_long_chat(&state, "c0", crate::transcript::perf_bench::long_chat(entries), vcx);
    });
    report("open (load + draw)", 1, c);
    let (_, c) = measure_long_chat(&counters, || {
        transcript.update(vcx, |t, cx| t.test_scroll_to_top(cx));
        vcx.update(|window, cx| window.draw(cx).clear());
    });
    report("scroll to top + draw", 1, c);
    let (_, c) = measure_long_chat(&counters, || {
        transcript.update(vcx, |t, cx| t.test_scroll_to_bottom(cx));
        vcx.update(|window, cx| window.draw(cx).clear());
    });
    report("scroll to bottom + draw", 1, c);
    let base = crate::transcript::perf_bench::long_chat(entries);
    let stream = prepare_stream(&base, 200);
    start_streaming(&state, vcx, base);
    transcript.update(vcx, |t, cx| t.test_scroll_to_bottom(cx));
    vcx.update(|window, cx| window.draw(cx).clear());
    let (_, c) = measure_long_chat(&counters, || {
        for (frame, prepared) in stream {
            apply_delta(&state, vcx, frame, prepared);
        }
    });
    report("stream delta + draw", 200, c);
    let (_, c) = measure_long_chat(&counters, || {
        for i in 0..50 {
            type_key(&input, vcx, i);
        }
    });
    report("composer keystroke + draw", 50, c);
    // Scenario 4: switch between two long chats.
    let other = crate::transcript::perf_bench::long_chat(entries);
    let mut other_state = Some(other);
    let (_, c) = measure_long_chat(&counters, || {
        for i in 0..10 {
            let target = if i % 2 == 0 { "c1" } else { "c0" };
            state.update(vcx, |state, cx| state.select_chat(Some(target.into()), cx));
            if let Some(entries) = other_state.take().filter(|_| i == 0) {
                load_long_chat(&state, "c1", entries, vcx);
            }
            vcx.run_until_parked();
            vcx.update(|window, cx| window.draw(cx).clear());
        }
    });
    report("switch chat + draw", 10, c);
}

// Budget tests (run in the normal suite; each fails without its fix).

const LONG_CHAT_BUDGET_ENTRIES: usize = 120;

/// Typing in the composer must not re-render transcript rows: the transcript
/// is a cached view that repaints only when it is notified.
#[gpui::test]
fn composer_keystrokes_do_not_rerender_transcript_rows(cx: &mut TestAppContext) {
    let (_shell, _state, vcx, counters, input) = long_chat_fixture(cx, LONG_CHAT_BUDGET_ENTRIES);
    for i in 0..4 {
        type_key(&input, vcx, 1000 + i);
    }
    let (_, spent) = measure_long_chat(&counters, || {
        for i in 0..20 {
            type_key(&input, vcx, i);
        }
    });
    eprintln!("BUDGET typing: rows_rendered={} notifies={:?} ms={:.1}", spent.rows_rendered, spent.notifies, spent.ms);
    // At most one layout-driven repaint of the visible rows (the composer
    // growing a line); an uncached transcript re-rendered them on every key.
    assert!(spent.rows_rendered <= 16, "keystrokes re-rendered transcript rows: {}", spent.rows_rendered);
    let transcript_notifies = spent.notifies.iter().find(|(name, _)| *name == "transcript").unwrap().1;
    assert_eq!(transcript_notifies, 0, "keystrokes notified the transcript");
}

/// Streaming into a long chat: code-fence pruning and the rail's tick-to-row
/// map are O(changed rows) per delta, not O(rows).
#[gpui::test]
fn streaming_deltas_do_not_rescan_every_row(cx: &mut TestAppContext) {
    const DELTAS: usize = 40;
    let (shell, state, vcx, _counters, _input) = long_chat_fixture(cx, LONG_CHAT_BUDGET_ENTRIES);
    let transcript = shell.read_with(vcx, |s, _| s.transcript.clone());
    let rows = transcript.read_with(vcx, |t, _| t.rows().len());
    let base = crate::transcript::perf_bench::long_chat(LONG_CHAT_BUDGET_ENTRIES);
    let stream = prepare_stream(&base, DELTAS + 4);
    start_streaming(&state, vcx, base);
    transcript.update(vcx, |t, cx| t.test_scroll_to_bottom(cx));
    vcx.update(|window, cx| window.draw(cx).clear());
    let mut stream = stream.into_iter();
    for _ in 0..4 {
        let (frame, prepared) = stream.next().unwrap();
        apply_delta(&state, vcx, frame, prepared);
    }
    let fences_before = crate::markdown::render::CODE_BLOCK_INDEX_CALLS.with(|c| c.get());
    let map_builds_before = crate::rail::RAIL_ROW_MAP_BUILDS.with(|c| c.get());
    for (frame, prepared) in stream {
        apply_delta(&state, vcx, frame, prepared);
    }
    let fences = crate::markdown::render::CODE_BLOCK_INDEX_CALLS.with(|c| c.get()) - fences_before;
    let map_builds = crate::rail::RAIL_ROW_MAP_BUILDS.with(|c| c.get()) - map_builds_before;
    eprintln!("BUDGET streaming {DELTAS} deltas over {rows} rows: code_block_indices calls={fences} rail row-map builds={map_builds}");
    // Rendering the visible rows also calls code_block_indices; a full scan
    // per delta would be `rows` calls each.
    assert!(
        fences < DELTAS * rows / 4,
        "code-fence pruning scanned every row per delta: {fences} calls for {rows} rows"
    );
    assert!(map_builds_before > 0, "the rail never rendered; the budget is vacuous");
    assert!(map_builds <= 2, "rail rebuilt its row map on {map_builds} of {DELTAS} deltas");
}

/// `Transcript::sync` restricts its tool-group passes (previous-tool map and
/// live group loop) to the diff range: a streamed delta touches a few rows,
/// not every row of the chat.
#[gpui::test]
fn streaming_deltas_do_not_scan_tool_groups_of_every_row(cx: &mut TestAppContext) {
    const DELTAS: usize = 40;
    let (shell, state, vcx, _counters, _input) = long_chat_fixture(cx, LONG_CHAT_BUDGET_ENTRIES);
    let transcript = shell.read_with(vcx, |s, _| s.transcript.clone());
    let rows = transcript.read_with(vcx, |t, _| t.rows().len());
    let base = crate::transcript::perf_bench::long_chat(LONG_CHAT_BUDGET_ENTRIES);
    let stream = prepare_stream(&base, DELTAS + 4);
    start_streaming(&state, vcx, base);
    transcript.update(vcx, |t, cx| t.test_scroll_to_bottom(cx));
    vcx.update(|window, cx| window.draw(cx).clear());
    let mut stream = stream.into_iter();
    for _ in 0..4 {
        let (frame, prepared) = stream.next().unwrap();
        apply_delta(&state, vcx, frame, prepared);
    }
    let before = crate::transcript::SYNC_TOOL_SCAN_ROWS.with(|c| c.get());
    for (frame, prepared) in stream {
        apply_delta(&state, vcx, frame, prepared);
    }
    let visits = crate::transcript::SYNC_TOOL_SCAN_ROWS.with(|c| c.get()) - before;
    eprintln!("BUDGET tool-group scan: {visits} row visits over {DELTAS} deltas of {rows} rows");
    // A full scan visits every row twice per delta.
    assert!(
        visits < DELTAS * rows / 4,
        "sync scanned tool groups of every row per delta: {visits} visits for {rows} rows"
    );
}

// The transcript is a cached view: every input that changes its pixels must
// notify it (or refresh the window). These drive real pointer events.

fn settle(vcx: &mut gpui::VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
}

#[gpui::test]
fn hovering_a_row_repaints_the_cached_transcript(cx: &mut TestAppContext) {
    let (shell, _state, vcx, counters, _input) = long_chat_fixture(cx, LONG_CHAT_BUDGET_ENTRIES);
    let transcript = shell.read_with(vcx, |s, _| s.transcript.clone());
    transcript.update(vcx, |t, cx| t.test_scroll_to_top(cx));
    settle(vcx);
    let near = transcript.read_with(vcx, |t, _| t.test_visible_row_center(1)).expect("visible row");
    let far = transcript.read_with(vcx, |t, _| t.test_visible_row_center(6)).expect("visible row");
    assert!(transcript.read_with(vcx, |t, _| t.test_hovered_entry()).is_none());
    let (_, spent) = measure_long_chat(&counters, || {
        vcx.simulate_mouse_move(near, None, gpui::Modifiers::default());
        settle(vcx);
    });
    let first = transcript.read_with(vcx, |t, _| t.test_hovered_entry());
    assert!(first.is_some(), "pointer move did not set the hovered entry");
    assert!(spent.rows_rendered > 0, "hover did not repaint the cached transcript");
    // Moving to a row of a different entry changes it again and repaints.
    let (_, spent) = measure_long_chat(&counters, || {
        vcx.simulate_mouse_move(far, None, gpui::Modifiers::default());
        settle(vcx);
    });
    let second = transcript.read_with(vcx, |t, _| t.test_hovered_entry());
    assert!(second.is_some());
    if first.as_ref().map(|f| &f.1) != second.as_ref().map(|f| &f.1) {
        assert!(spent.rows_rendered > 0, "hover change did not repaint");
    }
}

#[gpui::test]
fn selection_drag_repaints_the_cached_transcript(cx: &mut TestAppContext) {
    let (shell, _state, vcx, counters, _input) = long_chat_fixture(cx, LONG_CHAT_BUDGET_ENTRIES);
    let transcript = shell.read_with(vcx, |s, _| s.transcript.clone());
    transcript.update(vcx, |t, cx| t.test_scroll_to_top(cx));
    settle(vcx);
    let mut started = None;
    for offset in 0..8 {
        let Some(start) = transcript.read_with(vcx, |t, _| t.test_visible_row_center(offset)) else { continue };
        vcx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
        if crate::markdown::selection::is_dragging() {
            started = Some(start);
            break;
        }
        vcx.simulate_mouse_up(start, gpui::MouseButton::Left, gpui::Modifiers::default());
        settle(vcx);
    }
    let start = started.expect("no visible row started a text selection");
    settle(vcx);
    let (_, spent) = measure_long_chat(&counters, || {
        for step in 1..=4 {
            vcx.simulate_mouse_move(
                start + gpui::point(px(30.0 * step as f32), px(12.0 * step as f32)),
                Some(gpui::MouseButton::Left),
                gpui::Modifiers::default(),
            );
            settle(vcx);
        }
    });
    vcx.simulate_mouse_up(start, gpui::MouseButton::Left, gpui::Modifiers::default());
    settle(vcx);
    assert!(spent.rows_rendered > 0, "dragging a selection did not repaint the cached transcript");
}

#[gpui::test]
fn streamed_text_repaints_the_cached_transcript_without_other_notifies(cx: &mut TestAppContext) {
    const DELTAS: usize = 8;
    let (shell, state, vcx, counters, _input) = long_chat_fixture(cx, LONG_CHAT_BUDGET_ENTRIES);
    let transcript = shell.read_with(vcx, |s, _| s.transcript.clone());
    let base = crate::transcript::perf_bench::long_chat(LONG_CHAT_BUDGET_ENTRIES);
    let stream = prepare_stream(&base, DELTAS);
    start_streaming(&state, vcx, base);
    transcript.update(vcx, |t, cx| t.test_scroll_to_bottom(cx));
    settle(vcx);
    for (frame, prepared) in stream {
        let version_before = transcript.read_with(vcx, |t, _| t.rows().last().map(|r| r.version));
        let (_, spent) = measure_long_chat(&counters, || apply_delta(&state, vcx, frame, prepared));
        let version_after = transcript.read_with(vcx, |t, _| t.rows().last().map(|r| r.version));
        assert_ne!(version_before, version_after, "the delta did not reach the transcript rows");
        assert!(spent.rows_rendered > 0, "a streamed delta did not repaint the transcript");
    }
}
