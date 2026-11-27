//! Offscreen snapshots for test fixtures: Metal on macOS, Vulkan on Linux. No native window is created.
mod code_reference;
use std::{path::PathBuf, sync::Arc};

use gpui::{App, AppContext, Entity, HeadlessAppContext, Pixels, Render, Size, Window};

use crate::{
    theme::{Appearance, Theme},
    ui_style::{self, UiLayout, UiStyleId},
};

/// Render a fixture into `$CARGO_TARGET_DIR/snapshots/<name>.png`.
///
/// The builder may install fixture-specific globals and construct the root view.
/// Names are single path components so snapshots cannot escape the output directory.
/// macOS needs a Metal device and access to macOS services. Linux needs a Vulkan driver
/// (`scripts/ui-preview.sh` pins lavapipe) and opens no window, compositor or display connection.
pub fn render_to_png<V: Render + 'static>(
    name: &str,
    size: Size<Pixels>,
    style: UiStyleId,
    appearance: Appearance,
    build_view: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<PathBuf> {
    render_to_png_with_layout(name, size, style, UiLayout::default(), appearance, build_view)
}

/// Render a fixture with an independently selected layout.
pub fn render_to_png_with_layout<V: Render + 'static>(
    name: &str,
    size: Size<Pixels>,
    style: UiStyleId,
    layout: UiLayout,
    appearance: Appearance,
    build_view: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "invalid snapshot name"
    );
    let native = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        native.text_system(),
        Arc::new(crate::icons::Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_base::init(cx);
        ui_style::init_with_layout(style, layout, cx);
        Theme::install(appearance, cx);
    });
    let window = cx.open_window(size, build_view)?;
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())?;
    // GPUI animation elements use wall-clock time; settle entrance animations.
    std::thread::sleep(std::time::Duration::from_millis(800));
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())?;
    let image = cx.capture_screenshot(window.into())?;
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let directory = target.join("snapshots");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{name}.png"));
    image.save(&path)?;
    Ok(path)
}

pub fn code_reference_fixture(window: &mut Window, cx: &mut App) -> Entity<impl Render + use<>> {
    code_reference::fixture(window, cx)
}

pub fn symbol_search_fixture(data_dir: PathBuf, window: &mut Window, cx: &mut App) -> Entity<crate::shell::Shell> {
    let shell = chat_fixture(data_dir, cx);
    shell.update(cx, |shell, cx| shell.fixture_symbol_search(window, cx));
    shell
}

pub fn definition_search_fixture(data_dir: PathBuf, window: &mut Window, cx: &mut App) -> Entity<crate::shell::Shell> {
    let shell = chat_fixture(data_dir, cx);
    shell.update(cx, |shell, cx| shell.fixture_definition_search(window, cx));
    shell
}

pub fn file_outline_fixture(cx: &mut App) -> Entity<crate::files::FilesSurface> {
    let state = cx.new(|_| crate::state::AppState::new());
    let file = cx.new(|cx| crate::files::FilesSurface::new_editor(state, "fixture".into(), "src/review.rs".into(), false, 1000, 13.0, false, false, cx));
    file.update(cx, |file, cx| file.fixture_file_outline(cx));
    file
}

pub fn file_blame_fixture(cx: &mut App) -> Entity<crate::files::FilesSurface> {
    let state = cx.new(|_| crate::state::AppState::new());
    let file = cx.new(|cx| crate::files::FilesSurface::new_editor(state, "fixture".into(), "src/review.rs".into(), false, 1000, 13.0, false, false, cx));
    file.update(cx, |file, cx| file.fixture_file_blame(cx));
    file
}

/// Construct the walkthrough test fixture using the production view.
pub fn walkthrough_fixture(cx: &mut App) -> Entity<impl Render + use<>> {
    crate::changes::walkthrough::snapshot_fixture(cx)
}

/// Construct the walkthrough with the populated diff used by the preview.
pub fn populated_walkthrough_fixture(cx: &mut App) -> Entity<impl Render + use<>> {
    crate::changes::walkthrough::snapshot_fixture_with_changes(true, cx)
}

/// Construct the Changes pane with its file tree expanded or collapsed.
pub fn changes_fixture(collapsed: bool, focused: bool, window: &mut Window, cx: &mut App) -> Entity<crate::changes::Changes> {
    let changes = crate::changes::snapshot_populated_changes(cx);
    if collapsed {
        changes.update(cx, |changes, cx| changes.collapse_snapshot_file_tree(cx));
    }
    if focused { changes.update(cx, |changes, cx| changes.focus_snapshot_file_tree(window, cx)); }
    changes
}

pub fn scope_map_fixture(cx: &mut App) -> Entity<crate::changes::Changes> {
    let changes = crate::changes::snapshot_populated_changes(cx);
    changes.update(cx, |changes, cx| changes.prepare_scope_map_snapshot(cx));
    changes
}

/// The Changes pane of a re-reviewed PR: the file list's "Changed since you
/// reviewed" group and header count, and (`since`) the "Since abc1234" view.
pub fn changes_rereview_fixture(since: bool, cx: &mut App) -> Entity<crate::changes::Changes> {
    crate::changes::snapshot_rereview_changes(since, cx)
}

/// Construct the offline shell used by the chat preview and gallery.
pub fn chat_fixture(data_dir: PathBuf, cx: &mut App) -> Entity<crate::shell::Shell> {
    if std::env::var_os("MAST_PREVIEW_REVIEW_LAUNCH").is_none() {
        return chat_fixture_with(data_dir, |_, _| {}, cx);
    }
    let shell = chat_fixture_with(data_dir, |state, _| {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("fixture-space");
        snapshot.space_settings.pr_review_launch = Some(review_launch_defaults_fixture());
        state.set_work_outline_for_test(snapshot);
    }, cx);
    shell.update(cx, |shell, cx| {
        let target = mast_proto::ReviewTarget::Branch {
            repo: Some("example/project".into()),
            checkout_path: Some("/fixture/project/worktree".into()),
            branch: Some("feature/review-launch".into()),
            base_ref: Some("main".into()),
            base_sha: Some("a".repeat(40)),
            head_sha: "b".repeat(40),
        };
        shell.open_start_chat_for_branch_review("fixture-space".into(), None, target, cx).unwrap();
        shell.toggle_start_chat_merge_gate(cx);
    });
    shell
}

fn review_launch_defaults_fixture() -> mast_proto::PrReviewLaunchDefaults {
    mast_proto::PrReviewLaunchDefaults {
        agent: Some("pr-review".into()),
        harness: Some(mast_proto::HarnessId::Codex),
        draft_model: Some("gpt-6-luna".into()),
        gate_model: Some("gpt-6.2-sol".into()),
    }
}

/// [`chat_fixture`] with extra state applied before the shell is built.
fn chat_fixture_with(
    data_dir: PathBuf,
    edit: impl FnOnce(&mut crate::state::AppState, &mut gpui::Context<crate::state::AppState>),
    cx: &mut App,
) -> Entity<crate::shell::Shell> {
    let codex = std::env::var("MAST_PREVIEW_HARNESS").is_ok_and(|value| value == "codex");
    let (harness, model) = if codex { ("codex", "gpt-6-astra") } else { ("claude-code", "Claude Opus 5.5 — Extended reasoning") };
    let state = cx.new(|_| {
        let mut state = crate::state::AppState::new();
        state.connection = mast_proto::view::ConnectionStatus::Ready;
        state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
        state.local_device_id = Some("local".into());
        state.selected_chat = Some("fixture-chat".into());
        state.selected_space = Some("fixture-space".into());
        state.auto_selected = true;
        state.chats_synced = true;
        state.spaces_synced = true;
        state.spaces = vec![serde_json::from_value(serde_json::json!({"id":"fixture-space","deviceId":"local","path":"/fixture/project","createdAt":"2026-09-08T00:00:00Z"})).unwrap()];
        state.chats = [("chat-api", "API coordinator", None), ("chat-auth", "Auth worker", Some("chat-api"))]
            .into_iter().map(|(id, title, parent)| serde_json::from_value(serde_json::json!({
                "id":id, "deviceId":"local", "spaceId":"fixture-space", "title":title,
                "parentChatId":parent, "archived":false, "createdAt":"2026-09-08T00:00:00Z"
            })).unwrap()).collect();
        state.chats = [
            ("fixture-chat", "Review the workspace"),
            ("fixture-design", "Design review"),
            ("fixture-tests", "Verify the changes"),
        ].into_iter().map(|(id, title)| serde_json::from_value(serde_json::json!({"id":id,"deviceId":"local","spaceId":"fixture-space","title":title,"archived":false,"createdAt":"2026-09-08T00:00:00Z","config":{"harness":harness,"model":model,"reasoning":"high","sandbox":"workspace-write"}})).unwrap()).collect();
        if std::env::var_os("MAST_PREVIEW_ASSISTANT").is_some() {
            // Two runs of one scheduled task; the older one is designated and
            // the section shows the newer.
            for (id, title, created) in [("fixture-assistant-old", "Daily briefing (yesterday)", 1_i64), ("fixture-assistant", "Daily briefing assistant", 2)] {
                state.chats.push(serde_json::from_value(serde_json::json!({"id":id,"deviceId":"local","spaceId":"fixture-space","title":title,"archived":false,"createdAt":"2026-09-08T00:00:00Z","config":{"harness":harness,"model":model,"reasoning":"high","sandbox":"workspace-write"},"scheduledTaskInfo":{"taskId":"fixture-briefing","taskName":"Daily briefing","runId":id,"sessionCreatedAt":created}})).unwrap());
            }
            let designated = mast_proto::PersonalAssistant::for_chat(&state.chats[state.chats.len() - 2], 0);
            state.personal_assistant.apply(mast_proto::PersonalAssistantReply { assistant: Some(designated) });
        }
        if std::env::var_os("MAST_PREVIEW_ATTENTION_CHIPS").is_some() {
            let now = chrono::Utc::now();
            state
                .chats
                .iter_mut()
                .find(|chat| chat.id == "fixture-tests")
                .unwrap()
                .last_message_at = Some(now);
            for (id, status) in [
                ("fixture-design", mast_proto::SessionStatus::AwaitingInput),
                ("fixture-tests", mast_proto::SessionStatus::Errored),
            ] {
                state.sessions.push(mast_proto::Session {
                    chat_id: id.into(),
                    device_id: "local".into(),
                    status,
                    started_at: None,
                    updated_at: now,
                    last_completed_turn: None,
                    awaiting_input: None,
                    last_turn_tool_failures: None,
                    todo_progress: None,
                });
            }
        }
        preview_chat_outline(&mut state);
        if std::env::var_os("MAST_PREVIEW_CODE_REF").is_some() {
            state.add_code_ref("fixture-chat", crate::code_ref::StagedCodeRef {
                id: "fixture-reference".into(),
                reference: mast_proto::CodeRef {
                    version: 1,
                    location: mast_proto::CodeRefLocation { repo_id: "fixture-repo".into(), worktree_id: None, repo_root: None, path: "src/review.rs".into() },
                    kind: mast_proto::CodeRefKind::Lines,
                    range: Some(mast_proto::CodeRefRange { start_line: 88, end_line: 112 }),
                    symbol: None,
                    at: mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: "fixture".into() },
                },
                snippet: "fn review() {}".into(),
            });
        }
        state
    });
    state.update(cx, |state, cx| {
        let review_text = crate::code_ref::with_code_refs("Review the sidebar spacing.", state.code_refs("fixture-chat"));
        state.receive_transcript_frame(mast_doc::TranscriptFrame::Reset { reset: serde_json::from_value(serde_json::json!([
            {"id":"user","role":"user","parts":[{"id":"text","kind":"text","text":review_text}],"createdAt":1788900000000_i64,"deviceId":"local"},
            {"id":"assistant","role":"assistant","parts":[{"id":"text","kind":"text","text":"I inspected the layout.\n\nThe controls now align with their captions.\n\n- The model name stays visible.\n- The attachment has one border.\n\n```rust\nfn validate_layout() {\n    assert!(controls_align());\n}\n```"},{"id":"tool","kind":"tool","call":{"kind":"exec","command":"cargo test"},"resolved":true,"output":"Tests passed"},{"id":"tool2","kind":"tool","call":{"kind":"exec","command":"git diff --stat"},"resolved":true,"output":"Three files changed"},{"id":"next","kind":"text","text":"I checked the resulting changes. The transcript keeps this longer explanation within a comfortable reading width, even when the window becomes narrow.\n\n- The first list item has enough text to wrap onto another line while keeping its continuation aligned with the beginning of the item.\n- The second item shares the same indentation."},{"id":"tool3","kind":"tool","call":{"kind":"exec","command":"cargo check"},"resolved":true,"output":"Check passed"},{"id":"tool4","kind":"tool","call":{"kind":"exec","command":"cargo fmt --check"},"resolved":true,"output":"Formatting passed"}],"createdAt":1788900001000_i64,"deviceId":"local","status":"complete"}
        ])).unwrap() }, cx).unwrap();
        if std::env::var_os("MAST_PREVIEW_CODE_REF").is_some() {
            let text = crate::code_ref::with_code_refs("Review these lines.", state.code_refs("fixture-chat"));
            let entry = serde_json::from_value(serde_json::json!({"id":"reference-user","role":"user","parts":[{"id":"text","kind":"text","text":text}],"createdAt":1788900002000_i64,"deviceId":"local"})).unwrap();
            state.receive_transcript_frame(mast_doc::TranscriptFrame::Delta { upsert: vec![mast_doc::TranscriptUpsert { after: Some("assistant".into()), entry }], append: Vec::new(), remove: Vec::new(), count: 3 }, cx).ok();
        }
        if std::env::var_os("MAST_PREVIEW_RETRY").is_some() {
            state.sessions.push(serde_json::from_value(serde_json::json!({
                "chatId": "fixture-chat", "deviceId": "local", "status": "errored",
                "updatedAt": "2026-09-08T00:00:00Z"
            })).unwrap());
            state.receive_transcript_frame(mast_doc::TranscriptFrame::Reset { reset: serde_json::from_value(serde_json::json!([
                {"id":"user","role":"user","parts":[{"id":"text","kind":"text","text":"Review the sidebar spacing."}],"createdAt":1788900000000_i64,"deviceId":"local"},
                {"id":"failure","role":"assistant","parts":[{"id":"error","kind":"error","message":"The service is overloaded. Please try again."}],"createdAt":1788900001000_i64,"deviceId":"local","status":"complete"}
            ])).unwrap() }, cx).unwrap();
        }
        if std::env::var_os("MAST_PREVIEW_SUBAGENTS").is_some() {
            let spawn = |id: &str, description: &str, status: &str| serde_json::json!({"id":id,"kind":"tool","call":{"kind":"unknown","name":format!("Agent: {description}"),"input":{"description":description}},"resolved":true,"subagentRef":format!("fixture-chat--sub--{id}"),"subagentStatus":status});
            state.receive_transcript_frame(mast_doc::TranscriptFrame::Reset { reset: serde_json::from_value(serde_json::json!([
                {"id":"user","role":"user","parts":[{"id":"text","kind":"text","text":"Review the sidebar spacing."}],"createdAt":1788900000000_i64,"deviceId":"local"},
                {"id":"assistant","role":"assistant","parts":[{"id":"text","kind":"text","text":"I'll split the review across subagents."},spawn("a1","Audit the sidebar layout","running"),spawn("a2","Check the theme contrast","running"),spawn("a3","List the changed files","done")],"createdAt":1788900001000_i64,"deviceId":"local","status":"streaming"}
            ])).unwrap() }, cx).unwrap();
        }
        edit(state, cx);
    });
    let boot = crate::EngineBootConfig {
        data_dir,
        ipc_port: 0,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: mast_proto::HarnessId::Mock,
    };
    let shell = cx.new(|cx| {
        let mut shell = crate::shell::Shell::new(state, boot, cx);
        shell.fixture_snapshot_ready();
        shell.fixture_snapshot_attachment(
            include_bytes!("../assets/dock/mast-icon-ready.png").to_vec(),
            cx,
        );
        shell
    });
    shell.update(cx, |shell, cx| {
        shell.fixture_snapshot_item_panel(cx);
        if std::env::var_os("MAST_PREVIEW_SETTINGS_TAB").is_some() {
            shell.fixture_snapshot_settings_tab(cx);
        }
        shell.fixture_snapshot_expand_tool_group(cx);
        if std::env::var_os("MAST_PREVIEW_CODE_REF").is_some() { shell.fixture_changed_code_ref(cx); }
        if std::env::var("MAST_OPEN_DIALOG").is_ok_and(|value| value == "model") {
            shell.fixture_snapshot_model_catalog(cx);
        }
    });
    shell
}

/// Rows read back from real engine stores (see the headless UI driver).
pub struct WorldRows {
    pub spaces: Vec<mast_proto::Space>,
    pub chats: Vec<mast_proto::Chat>,
    pub outline: mast_proto::WorkOutlineSnapshot,
    pub selected_chat: Option<String>,
    /// `SessionMessageEntry` wire values for the selected chat.
    pub transcript: Vec<serde_json::Value>,
}

/// Shell whose state is filled from [`WorldRows`] before the shell exists, the
/// same way [`chat_fixture`] does. The engine is not booted here: attaching
/// fires the shell's state observer, which panics ("Leases must be ended")
/// while the window is still being created. Call [`world_boot_engine`] once
/// `open_window` has returned and the first frame has drawn.
pub fn world_shell(data_dir: PathBuf, rows: WorldRows, cx: &mut App) -> Entity<crate::shell::Shell> {
    // A real engine runs behind this shell; keep it off the host's Dock and menu bar.
    crate::status_light::NATIVE_CHROME_SUPPRESSED.store(true, std::sync::atomic::Ordering::Relaxed);
    let selected = rows.selected_chat.clone();
    let selected_space = selected
        .as_ref()
        .and_then(|id| rows.chats.iter().find(|chat| &chat.id == id))
        .and_then(|chat| chat.space_id.clone());
    let transcript = rows.transcript;
    let state = cx.new(|_| {
        let mut state = crate::state::AppState::new();
        state.connection = mast_proto::view::ConnectionStatus::Ready;
        state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
        state.local_device_id = rows.chats.first().map(|chat| chat.device_id.clone());
        state.selected_chat = selected;
        state.selected_space = selected_space;
        state.auto_selected = true;
        state.chats_synced = true;
        state.spaces_synced = true;
        state.spaces = rows.spaces;
        state.chats = rows.chats;
        state.set_work_outline_for_test(rows.outline);
        state
    });
    state.update(cx, |state, cx| {
        if state.selected_chat.is_some()
            && let Ok(reset) = serde_json::from_value(serde_json::Value::Array(transcript)) {
                state.receive_transcript_frame(mast_doc::TranscriptFrame::Reset { reset }, cx).ok();
            }
    });
    let boot = crate::EngineBootConfig {
        data_dir,
        ipc_port: 0,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        // Without a WorkOS client the engine picks the Development profile and
        // never sees the stores the world was written to; with one and no
        // saved session it opens the Local profile, as an installed app does.
        workos_client_id: Some("mast-ui-driver".into()),
        default_harness: mast_proto::HarnessId::Mock,
    };
    cx.new(|cx| {
        let mut shell = crate::shell::Shell::new(state, boot, cx);
        shell.fixture_snapshot_ready();
        shell
    })
}

/// Boot the real in-process engine behind a [`world_shell`] on its data dir.
/// Only valid after the window exists and has drawn.
pub fn world_boot_engine(shell: &Entity<crate::shell::Shell>, cx: &mut App) {
    let (state, boot) = shell.read(cx).fixture_boot_params();
    crate::state::AppState::bootstrap(state, boot, cx);
}

/// Connection, chat count, selection and splash state of a [`world_shell`].
pub fn world_status(shell: &Entity<crate::shell::Shell>, cx: &App) -> String {
    shell.read(cx).fixture_status(cx)
}

/// Construct the chat fixture with its hand-off dialog open.
pub fn handoff_fixture(data_dir: PathBuf, cx: &mut App) -> Entity<crate::shell::Shell> {
    let shell = chat_fixture(data_dir, cx);
    shell.update(cx, |shell, cx| shell.fixture_snapshot_hand_off(cx));
    shell
}

/// The titlebar popover with the Space Builds panel: one running build, two queued, a recent pass
/// and a recent failure whose log is open. The sidebar rows carry the same queue state.
pub fn builds_fixture(data_dir: PathBuf, window: &mut Window, cx: &mut App) -> Entity<crate::shell::Shell> {
    let shell = chat_fixture_with(
        data_dir,
        |state, _| {
            let now = chrono::Utc::now().timestamp_millis();
            let job = |id: &str, chat: &str, label: &str, state: mast_proto::BuildJobState, age_secs: i64| mast_proto::BuildJobView {
                job_id: id.into(),
                space_id: "fixture-space".into(),
                chat_id: chat.into(),
                device_id: "local".into(),
                argv: vec!["cargo".into(), "test".into(), "-p".into(), label.into()],
                state,
                created_at: now - age_secs * 1000,
                ..Default::default()
            };
            let mut running = job("job-1", "fixture-design", "mast-ui", mast_proto::BuildJobState::Running, 200);
            running.label = Some("cargo test -p mast-ui --lib build_watch".into());
            running.started_at = Some(now - 192_000);
            let mut first = job("job-2", "fixture-tests", "mast-engine", mast_proto::BuildJobState::Queued, 95);
            first.position = Some(1);
            let mut second = job("job-3", "fixture-chat", "mast-rpc", mast_proto::BuildJobState::Queued, 40);
            second.position = Some(2);
            let mut passed = job("job-4", "fixture-chat", "mast-proto", mast_proto::BuildJobState::Succeeded, 900);
            passed.started_at = Some(now - 880_000);
            passed.finished_at = Some(now - 640_000);
            passed.exit_code = Some(0);
            let mut failed = job("job-5", "fixture-tests", "mast-engine", mast_proto::BuildJobState::Failed, 1500);
            failed.started_at = Some(now - 1_400_000);
            failed.finished_at = Some(now - 1_190_000);
            failed.exit_code = Some(101);
            failed.summary = Some("error[E0308]: mismatched types in crates/engine/src/rpc.rs".into());
            state.set_test_build_jobs(mast_proto::BuildJobsSnapshot {
                space_id: "fixture-space".into(),
                jobs: vec![running, first, second, passed, failed],
                quiet_window_active: false,
            });
        },
        cx,
    );
    shell.update(cx, |shell, cx| shell.fixture_snapshot_builds(window, cx));
    shell
}

/// Construct an artifact pane with local, in-memory fixture content.
pub fn artifact_fixture(window: &mut Window, cx: &mut App) -> Entity<impl Render + use<>> {
    crate::artifacts::tab::snapshot_fixture(window, cx)
}

struct OutlineFixture {
    outline: Entity<crate::work_outline::outline_view::OutlineView>,
    details: Entity<crate::work_outline::details_panel::WorkItemDetails>,
    _shell: Entity<crate::shell::Shell>,
}

struct SpaceSettingsFixture {
    details: Entity<crate::work_outline::details_panel::WorkItemDetails>,
    _shell: Entity<crate::shell::Shell>,
}

impl gpui::Render for SpaceSettingsFixture {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{div, prelude::*};
        div().size_full().child(self.details.clone())
    }
}

pub fn space_settings_fixture(data_dir: PathBuf, cx: &mut App) -> Entity<impl Render + use<>> {
    let mut snapshot = outline_with_requests("s1");
    snapshot.space_settings.worker_worktrees = true;
    snapshot.space_settings.merged_pr_completion = Some(mast_proto::work_outline::MergedPrCompletion::Ask);
    snapshot.space_settings.archive_workers_after_days = Some(3);
    if std::env::var_os("MAST_PREVIEW_REVIEW_LAUNCH").is_some() {
        snapshot.space_settings.pr_review_launch = Some(review_launch_defaults_fixture());
    }
    let state = cx.new(|_| {
        let mut state = crate::state::AppState::new();
        state.connection = mast_proto::view::ConnectionStatus::Ready;
        state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
        state.selected_space = Some("s1".into());
        state.spaces = vec![serde_json::from_value(serde_json::json!({"id":"s1","deviceId":"local","path":"/fixture/project","name":"Project","createdAt":"2026-09-08T00:00:00Z"})).unwrap()];
        state.linear.accounts = Some(Vec::new());
        state.set_work_outline_for_test(snapshot.clone());
        state
    });
    let boot = crate::EngineBootConfig { data_dir, ipc_port: 0, edge_url: String::new(), edge_token: None,
        org_id: None, workos_client_id: None, default_harness: mast_proto::HarnessId::Mock };
    let shell = cx.new(|cx| crate::shell::Shell::new(state.clone(), boot, cx));
    let details = cx.new(|cx| crate::work_outline::details_panel::WorkItemDetails::new(
        state, shell.downgrade(), "s1".into(), "".into(), cx));
    details.update(cx, |details, _| details.set_data(Some(Arc::new(snapshot)), Arc::new(Vec::new())));
    cx.new(|_| SpaceSettingsFixture { details, _shell: shell })
}

/// Offline Overview with its outline toolbar visible.
pub fn overview_fixture(cx: &mut App) -> Entity<crate::overview::Overview> {
    let state = cx.new(|_| {
        let mut state = crate::state::AppState::new();
        state.connection = mast_proto::view::ConnectionStatus::Ready;
        state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
        state.local_device_id = Some("local".into());
        state.spaces = vec![serde_json::from_value(serde_json::json!({"id":"fixture-space","deviceId":"local","path":"/fixture/project","createdAt":"2026-09-08T00:00:00Z"})).unwrap()];
        state
    });
    let transcript = cx.new(|cx| crate::transcript::Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let overview = cx.new(|cx| {
        crate::overview::Overview::new(
            state,
            gpui::WeakEntity::new_invalid(),
            transcript,
            composer,
            cx,
        )
    });
    overview.update(cx, |overview, cx| overview.show_snapshot_outline(cx));
    overview
}

/// Offline Overview with an in-memory Linear panel, without network requests.
pub fn linear_overview_fixture(cx: &mut App) -> Entity<crate::overview::Overview> {
    let overview = overview_fixture(cx);
    overview.update(cx, |overview, cx| overview.show_snapshot_linear(cx));
    overview
}

/// Linear settings with in-memory account metadata and no RPC.
pub fn linear_settings_fixture(cx: &mut App) -> Entity<impl Render + use<>> {
    let state = cx.new(|_| {
        let mut state = crate::state::AppState::new();
        state.linear.status = Some(Ok(mast_proto::linear::LinearConnectionStatus {
            configured: true,
            source: mast_proto::linear::LinearKeySource::Keychain,
            ..Default::default()
        }));
        state.linear.accounts = Some(vec![
            mast_proto::linear::LinearAccount { id: "default".into(), label: "Default".into(), key_set: true },
            mast_proto::linear::LinearAccount { id: "personal".into(), label: "Personal".into(), key_set: true },
        ]);
        state
    });
    cx.new(|cx| crate::linear::settings_page::LinearSettingsPage::offline_fixture(state, cx))
}

/// Offline Scheduled tasks editor for a new task on a current device.
pub fn scheduled_task_editor_fixture(cx: &mut App) -> Entity<impl Render + use<>> {
    let state = cx.new(|_| {
        let mut state = crate::state::AppState::new();
        state.devices.push(
            serde_json::from_value(serde_json::json!({
                "id": "local", "name": "This Mac", "platform": "macos",
                "capabilities": [
                    mast_proto::capabilities::SCHEDULED_TASK_WORK_ITEM_V1,
                    mast_proto::capabilities::SCHEDULED_TASK_OUTPUT_V1,
                    mast_proto::capabilities::SCHEDULED_TASK_AFTER_SUCCESS_V1,
                    mast_proto::capabilities::SCHEDULED_TASK_STANDING_CHAT_V1,
                    mast_proto::capabilities::SCHEDULED_TASK_NOTION_WRITES_V1,
                ],
            }))
            .unwrap(),
        );
        state
    });
    let page = cx.new(|cx| crate::scheduled_tasks::ScheduledTasksPage::offline_editor_fixture(state, cx));
    cx.new(|_| ScrolledEditorFixture(page))
}

/// Offline Scheduled tasks list on a current device, with the briefing index switch on.
pub fn scheduled_tasks_briefing_index_fixture(cx: &mut App) -> Entity<impl Render + use<>> {
    let state = cx.new(|_| {
        let mut state = crate::state::AppState::new();
        state.devices.push(
            serde_json::from_value(serde_json::json!({
                "id": "local", "name": "This Mac", "platform": "macos",
                "capabilities": [
                    mast_proto::capabilities::SCHEDULED_TASK_WORK_ITEM_V1,
                    mast_proto::capabilities::SCHEDULED_TASK_OUTPUT_V1,
                    mast_proto::capabilities::BRIEFING_READ_V1,
                ],
            }))
            .unwrap(),
        );
        state
    });
    cx.new(|cx| crate::scheduled_tasks::ScheduledTasksPage::offline_list_fixture(state, cx))
}

/// The editor taller than the preview window, shifted up so its lower controls show.
struct ScrolledEditorFixture(Entity<crate::scheduled_tasks::ScheduledTasksPage>);
impl Render for ScrolledEditorFixture {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{IntoElement, ParentElement, Styled, div, px};
        div().size_full().relative().overflow_hidden().child(
            div().absolute().top(px(-520.0)).left_0().w_full().h(px(1500.0)).child(self.0.clone()),
        )
    }
}

/// Offline chat shell where the agent asks a question and the user has typed
/// a long answer of their own. The host takes side chats, so the panel offers
/// Clarify.
pub fn question_fixture(data_dir: PathBuf, cx: &mut App) -> Entity<crate::shell::Shell> {
    let shell = chat_fixture_with(
        data_dir,
        |state, cx| {
            state.devices.push(
                serde_json::from_value(serde_json::json!({
                    "id": "local", "name": "This Mac", "platform": "macos",
                    "capabilities": [mast_proto::capabilities::SIDE_CHAT_V1],
                }))
                .unwrap(),
            );
            state.receive_transcript_frame(mast_doc::TranscriptFrame::Reset { reset: serde_json::from_value(serde_json::json!([
                {"id":"user","role":"user","parts":[{"id":"text","kind":"text","text":"Review the sidebar spacing."}],"createdAt":1788900000000_i64,"deviceId":"local"},
                {"id":"assistant","role":"assistant","parts":[{"id":"text","kind":"text","text":"The caption gap and the control gap disagree. I can fix it either way."},{"id":"ask","kind":"input","requestId":"req-1","questions":[{"id":"q1","header":"Approve command","question":"Allow this command? cargo test -p mast-ui --lib -- --test-threads=1","options":["Yes","No"],"allowCustom":true}]}],"createdAt":1788900001000_i64,"deviceId":"local","status":"streaming"}
            ])).unwrap() }, cx).unwrap();
        },
        cx,
    );
    shell.update(cx, |shell, cx| {
        shell.fixture_snapshot_type(
            "Use the control gap token for the captions, but only in the sidebar. The Overview \
             rows already read their gap from the same token, so changing it there would shift \
             every row. Keep the compact layout's smaller token as it is, and check the \
             walkthrough screens afterwards, since they reuse the sidebar captions. If the \
             caption row still looks cramped at the narrowest width, add one pixel of top \
             padding rather than a new token, and leave a note in the design doc so the next \
             change to the spacing scale accounts for it.\n\nOne more thing: the industrial \
             style draws a border around each caption, so check that the extra pixel doesn't \
             double up with it, and run the preview in both themes before you hand it back.",
            cx,
        )
    });
    shell
}

/// Workflow pack settings with an import preview and recent imports, no RPC.
pub fn workflow_pack_fixture(cx: &mut App) -> Entity<impl Render + use<>> {
    use crate::settings::workflow_pack::{
        Choice, ImportPreview, ImportReview, ReceiptRow, ReceiptSummary, WorkflowPackPage,
    };
    use mast_proto::work_outline_templates::TemplateNode;
    use mast_proto::workflow_pack::{WorkflowPackScopePreview, WorkflowPackTemplatePreview};
    let state = cx.new(|_| {
        let mut state = crate::state::AppState::new();
        let space = serde_json::from_value(serde_json::json!({
            "id": "space-1", "deviceId": "device-1", "path": "/work/atlas",
            "name": "Atlas", "createdAt": "2026-01-01T00:00:00Z"
        }))
        .expect("space fixture");
        state.spaces = vec![space];
        state.selected_space = Some("space-1".into());
        state
    });
    let texts = |items: &[&str]| items.iter().map(|text| text.to_string()).collect::<Vec<_>>();
    let node = |title: &str| TemplateNode { title: title.into(), ..Default::default() };
    let mut review = ImportReview::new(ImportPreview {
        preview_id: "fixture".into(),
        scopes: vec![
            WorkflowPackScopePreview {
                item_path: vec![],
                work_item_id: None,
                missing_from: None,
                current: texts(&["Run the tests before committing", "Keep commits small"]),
                incoming: texts(&["Run the tests before committing", "Write a short summary", "Link the ticket"]),
            },
            WorkflowPackScopePreview {
                item_path: texts(&["Projects", "Reviews"]),
                work_item_id: None,
                missing_from: Some(1),
                current: vec![],
                incoming: texts(&["Review for correctness first"]),
            },
            WorkflowPackScopePreview {
                item_path: texts(&["Tasks"]),
                work_item_id: Some("item-1".into()),
                missing_from: None,
                current: texts(&["Prefer small tasks"]),
                incoming: texts(&["One task per branch"]),
            },
        ],
        templates: vec![
            WorkflowPackTemplatePreview { name: "Feature".into(), current: Some(node("Feature")), incoming: node("Feature v2") },
            WorkflowPackTemplatePreview { name: "Bug".into(), current: None, incoming: node("Bug") },
        ],
    });
    review.scope_choices[0] = Choice::Append;
    review.scope_choices[2] = Choice::Replace;
    let receipts = vec![
        ReceiptRow {
            id: "r1".into(),
            created_at: chrono::Utc::now().timestamp_millis() - 3 * 3_600_000,
            undone_at: None,
            summary: ReceiptSummary { items_created: 1, instructions_added: 3, instructions_archived: 1, templates_added: 1, templates_replaced: 0 },
        },
        ReceiptRow {
            id: "r2".into(),
            created_at: chrono::Utc::now().timestamp_millis() - 2 * 86_400_000,
            undone_at: Some(1),
            summary: ReceiptSummary { instructions_added: 2, ..Default::default() },
        },
    ];
    cx.new(|cx| WorkflowPackPage::fixture(state, review, receipts, cx))
}

/// Offline chat shell with a single in-memory worker.
pub fn team_fixture(data_dir: PathBuf, cx: &mut App) -> Entity<crate::shell::Shell> {
    let shell = chat_fixture(data_dir, cx);
    shell.update(cx, |shell, cx| shell.fixture_snapshot_team(cx));
    shell
}

/// Offline chat shell with a side chat open in the right pane, an absorbed
/// exchange in the main transcript, and the chat menu open.
pub fn side_chat_fixture(data_dir: PathBuf, cx: &mut App) -> Entity<crate::shell::Shell> {
    let shell = chat_fixture_with(data_dir, crate::shell::fixture_side_chat_state, cx);
    shell.update(cx, |shell, cx| shell.fixture_snapshot_side_chat(cx));
    shell
}

/// The model selector popover on a Claude chat, from the chat fixture.
/// `MAST_PREVIEW_MODEL_SELECTOR` picks the case: unset is a chat that has
/// reported no window (Context Window reads "Default" in the row and chip, "Default (set by Claude)" in its choices),
/// `reported` a chat whose last turn reported a 1M window, `new-chat` the
/// new-chat canvas, and `window` the unset chat with the Context Window
/// choices open beside the popover. `window-250k` is the same with 250K chosen.
pub fn model_selector_fixture(data_dir: PathBuf, window: &mut Window, cx: &mut App) -> Entity<crate::shell::Shell> {
    let case = std::env::var("MAST_PREVIEW_MODEL_SELECTOR").unwrap_or_default();
    let shell = chat_fixture_with(
        data_dir,
        |state, cx| match case.as_str() {
            "reported" => {
                state.context_usage = Some(mast_proto::ContextUsage { tokens: Some(120_000), window: Some(1_000_000) });
            }
            "new-chat" => state.select_chat(None, cx),
            "window-250k" => {
                for chat in &mut state.chats {
                    if let Some(config) = chat.config.as_mut() {
                        config.model_options.insert("contextWindow".into(), "250k".into());
                    }
                }
            }
            _ => {}
        },
        cx,
    );
    shell.update(cx, |shell, cx| {
        shell.fixture_snapshot_model_selector(matches!(case.as_str(), "window" | "window-250k").then_some("contextWindow"), window, cx)
    });
    shell
}

pub fn tracker_expanded_fixture(data_dir: PathBuf, cx: &mut App) -> Entity<crate::shell::Shell> {
    let shell = chat_fixture(data_dir, cx);
    shell.update(cx, |shell, cx| shell.fixture_snapshot_tracker_expanded(cx));
    shell
}

impl gpui::Render for OutlineFixture {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{div, prelude::*, px};
        div()
            .size_full()
            .flex()
            .child(div().w(px(420.)).h_full().child(self.outline.clone()))
            .child(div().flex_1().min_w_0().h_full().child(self.details.clone()))
    }
}

/// Construct the existing Work Outline fixture with an item details panel.
fn outline_with_requests(space: &str) -> mast_proto::WorkOutlineSnapshot {
    use crate::work_outline::store::{fixture_ids, fixture_snapshot};
    use mast_proto::{UserActionRequestState, UserActionRequestView};

    let mut snapshot = fixture_snapshot(space);
    snapshot.external_references.clear();
    let item = snapshot
        .items
        .iter_mut()
        .find(|item| item.id == fixture_ids::API)
        .unwrap();
    item.chat_ids.push("fixture-chat".into());
    if std::env::var_os("MAST_PREVIEW_ITEM_MANAGER").is_some() {
        // A project whose Manager has handed off once and never set any
        // Manager setting; its Settings tab shows them (`MAST_PREVIEW_SETTINGS_TAB`).
        // `chain` gives it a two-entry fallback chain.
        item.is_project = true;
        let fallback_chain = if std::env::var("MAST_PREVIEW_ITEM_MANAGER").is_ok_and(|value| value == "chain") {
            vec![
                mast_proto::StandingRoleModel { harness: "codex".into(), model: None },
                mast_proto::StandingRoleModel { harness: "claude-code".into(), model: Some("claude-sonnet-5-5".into()) },
            ]
        } else {
            Vec::new()
        };
        snapshot.standing_roles.push(mast_proto::StandingRole {
            id: mast_proto::standing_role_id(mast_proto::StandingRoleKind::Manager, fixture_ids::API),
            role: mast_proto::StandingRoleKind::Manager,
            scope_item_id: fixture_ids::API.into(),
            current_chat_id: Some("fixture-chat".into()),
            chain: vec!["fixture-design".into(), "fixture-chat".into()],
            fallback_chain,
            state: mast_proto::StandingRoleState::Idle,
            worker_model: None,
            read_only: None,
        });
    }
    // The queue preview leaves the requests out so its section fits the cell.
    let option_card = std::env::var_os("MAST_PREVIEW_REQUEST_OPTIONS").is_some();
    let requests: &[(&str, &str)] = if option_card || std::env::var_os("MAST_PREVIEW_QUEUE").is_some() {
        &[]
    } else {
        &[
            ("preview-approve", "Approve the proposed layout"),
            ("preview-review", "Review the implementation"),
        ]
    };
    for &(id, text) in requests {
        snapshot.user_action_requests.push(UserActionRequestView {
            id: id.into(),
            space_id: space.into(),
            work_item_id: fixture_ids::API.into(),
            chat_id: "fixture-chat".into(),
            text: text.into(),
            state: UserActionRequestState::Open,
            options: Vec::new(),
            kind: None,
            priority: None,
            origin_kind: Default::default(),
            message_id: None,
            tool_call_id: None,
            info: None,
            links: Vec::new(),
            facts: Vec::new(),
            dedupe_key: None,
            created_by: None,
            created_at: 1_760_000_000_000,
            resolution: None,
            deduped: false,
            origin_chat_available: true,
        });
    }
    if option_card {
        use mast_proto::work_outline_user_actions::{RequestOption, UserActionKind, UserActionPriority};
        snapshot.user_action_requests.push(UserActionRequestView {
            id: "preview-options".into(),
            space_id: space.into(),
            work_item_id: fixture_ids::API.into(),
            chat_id: "fixture-chat".into(),
            text: "Approval ledger: confirm retention, UI placement and auto-capture-off behavior".into(),
            state: UserActionRequestState::Open,
            options: vec![
                RequestOption { id: "approve-defaults".into(), label: "Approve defaults".into(), detail: Some("180 days / 5,000 rows per Space; History inside the Approvals view; ledger records with auto-capture off.".into()), recommended: true },
                RequestOption { id: "change".into(), label: "Change one or more".into(), detail: Some("Reply with the retention period, placement or auto-capture rule you want.".into()), recommended: false },
            ],
            kind: Some(UserActionKind::Choose),
            priority: Some(UserActionPriority::Blocking),
            origin_kind: Default::default(),
            message_id: None,
            tool_call_id: None,
            info: Some("Three choices before coding. Defaults below are my recommendations; replying \"approve defaults\" is enough.\n\n1. **Retention** (recommended: 180 days or 5,000 rows per Space, whichever bites first). Shorter loses audit value; longer grows the synced registry. Pruned rows are deleted on every device, **permanently**.\n2. **UI placement** (recommended: a History list inside the existing Space-root Approvals view). Alternative is a new Space-level tab: more surface, no extra data.\n3. **Auto-capture off** (recommended: the ledger still records, as it closes the gap where manual prompts vanish). Alternative follows the setting and keeps that gap.\n\nUntil answered I write no code; once answered I build in a Mac worktree and test on the Fedora desktop.".into()),
            links: Vec::new(),
            facts: Vec::new(),
            dedupe_key: None,
            created_by: None,
            created_at: 1_760_000_000_000,
            resolution: None,
            deduped: false,
            origin_chat_available: true,
        });
    }
    if std::env::var_os("MAST_PREVIEW_QUEUE").is_some() {
        // "Launch v2" is a project whose queue holds API (running), UI
        // (ready) and the completed "Auth endpoints".
        for (id, key) in [(fixture_ids::API, "a0"), (fixture_ids::UI, "a1"), (fixture_ids::API_AUTH, "a2")] {
            if let Some(item) = snapshot.items.iter_mut().find(|item| item.id == id) {
                item.queue_order_key = Some(key.into());
            }
        }
        if let Some(project) = snapshot.items.iter_mut().find(|item| item.id == fixture_ids::LAUNCH) {
            project.is_project = true;
        }
    }
    if std::env::var_os("MAST_PREVIEW_RESYNC").is_some() {
        if let Some(project) = snapshot.items.iter_mut().find(|item| item.id == fixture_ids::LAUNCH) {
            project.is_project = true;
        }
    }
    snapshot
}

pub fn preview_chat_outline(state: &mut crate::state::AppState) {
    state.set_work_outline_for_test(outline_with_requests("fixture-space"));
}

pub fn outline_fixture(data_dir: PathBuf, cx: &mut App) -> Entity<impl Render + use<>> {
    use crate::work_outline::{
        model::{OutlineChat, OutlineSpace},
        outline_view::{OutlineInputs, OutlineView},
        store::{WorkOutlineLoad, fixture_ids},
    };
    let chats = if std::env::var_os("MAST_PREVIEW_REQUEST_OPTIONS").is_some() {
        vec![OutlineChat {
            id: "fixture-chat".into(),
            space_id: Some("s1".into()),
            parent_chat_id: None,
            title: "Approval ledger".into(),
            status: mast_proto::ChatIndicator::Idle,
            archived: false,
            stale: false,
            branch: None,
            harness: None,
            pr_number: None,
            visible: true,
            activity: 0,
            tool_failures: None,
        }]
    } else {
        Vec::new()
    };
    let state = cx.new(|_| {
        let mut state = crate::state::AppState::new();
        state.connection = mast_proto::view::ConnectionStatus::Ready;
        state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
        state.selected_space = Some("s1".into());
        if std::env::var_os("MAST_PREVIEW_APPROVAL_RULE").is_some()
            || std::env::var_os("MAST_PREVIEW_APPROVAL_LEDGER").is_some()
        {
            state.linear.accounts = Some(Vec::new());
        }
        state.set_work_outline_for_test(outline_with_requests("s1"));
        if std::env::var_os("MAST_PREVIEW_ARTIFACTS").is_some() {
            state.set_test_artifacts(artifact_list_fixture("s1", fixture_ids::API));
        }
        state
    });
    let boot = crate::EngineBootConfig {
        data_dir,
        ipc_port: 0,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: mast_proto::HarnessId::Mock,
    };
    let shell = cx.new(|cx| crate::shell::Shell::new(state.clone(), boot, cx));
    let outline = cx.new(|cx| OutlineView::new(state.clone(), cx));
    outline.update(cx, |outline, cx| {
        outline.set_inputs(
            OutlineInputs {
                spaces: vec![OutlineSpace {
                    id: "s1".into(),
                    label: "Project".into(),
                    load: WorkOutlineLoad::Ready(Arc::new(outline_with_requests("s1"))),
                    hidden: false,
                }],
                chats: chats.clone(),
                ..Default::default()
            },
            cx,
        )
    });
    let details = cx.new(|cx| {
        crate::work_outline::details_panel::WorkItemDetails::new(
            state,
            shell.downgrade(),
            "s1".into(),
            // `MAST_PREVIEW_QUEUE=project` shows the project's own queue.
            if std::env::var_os("MAST_PREVIEW_APPROVAL_RULE").is_some()
                || std::env::var_os("MAST_PREVIEW_APPROVAL_LEDGER").is_some()
            {
                ""
            } else if std::env::var("MAST_PREVIEW_QUEUE").as_deref() == Ok("project")
                || std::env::var_os("MAST_PREVIEW_RESYNC").is_some()
            {
                fixture_ids::LAUNCH
            } else {
                fixture_ids::API
            }
            .into(),
            cx,
        )
    });
    details.update(cx, |details, cx| {
        details.set_data(
            Some(Arc::new(outline_with_requests("s1"))),
            Arc::new(chats),
        );
        // `MAST_PREVIEW_RESYNC=1` shows a project with a re-sync awaiting review.
        if std::env::var_os("MAST_PREVIEW_RESYNC").is_some() {
            details.set_preview_resync(serde_json::json!({
                "taskId": "task", "coverageComplete": true,
                "job": {
                    "status": "readyForReview", "proposalHash": "0f3a",
                    "summary": "Two Cargo slots and a 65 GiB floor replace one slot and 50 GB.",
                    "evidence": ["Mast development instruction, 2026-11-24: two slots, 65 GiB floor."],
                },
                "diff": [
                    {"kind": "same", "text": "## Builds"},
                    {"kind": "removed", "text": "Use one Cargo slot and keep 50 GB free."},
                    {"kind": "added", "text": "Use two Cargo slots and keep 65 GiB free."},
                    {"kind": "same", "text": "Touch the crate roots when switching worktrees."},
                ],
            }), cx);
        }
        // `MAST_PREVIEW_APPROVAL_LEDGER=1` opens the Space root's Approvals view with the per-request list.
        if std::env::var_os("MAST_PREVIEW_APPROVAL_LEDGER").is_some() {
            let command = "cargo nextest run -p mast-engine --test engine";
            let report = |id: &str, command: &str, answer: bool| -> mast_proto::FrictionReport {
                serde_json::from_value(serde_json::json!({
                    "id": id, "kind": "permission", "summary": "Approve command requested",
                    "whatHappened": format!("Codex wants to run `{command}`. Allow it? Reason: sandbox"),
                    "workaround": null, "suggestion": null,
                    "approval": {"command": command, "why": "sandbox", "answer": answer, "exact": true},
                    "provenance": {"chatId": "chat-1", "harness": "codex", "model": null, "spaceId": "s1", "createdAt": 1, "source": "auto"},
                    "fingerprint": id, "occurrences": 3, "lastSeen": 1, "status": "open"
                })).unwrap()
            };
            let now = crate::artifacts::model::now_ms();
            let entry = |id: &str, command: &str, decision: &str, decider: &str, ago: i64, rule: Option<&str>| -> mast_proto::ApprovalLedgerEntry {
                serde_json::from_value(serde_json::json!({
                    "id": id, "spaceId": "s1", "chatId": "chat-1", "requestId": format!("req-{id}"),
                    "workItemId": fixture_ids::API, "harness": "codex", "command": command, "source": "prompt",
                    "requestedAt": now - ago, "decision": decision, "decider": decider, "ruleId": rule
                })).unwrap()
            };
            let entries = vec![
                entry("a", command, "allowed", "space_rule", 4 * 60_000, Some("rule-1")),
                entry("b", command, "cancelled", "teardown", 55 * 60_000, None),
                entry("c", "pgrep -f cargo", "denied", "user", 3 * 3_600_000, None),
                entry("d", command, "allowed", "user", 26 * 3_600_000, None),
            ];
            details.set_preview_approval_ledger(
                vec![report("nextest", command, true), report("pgrep", "pgrep -f cargo", false)],
                mast_proto::ListApprovalLedgerResult { total: entries.len(), entries },
                cx,
            );
        }
        if std::env::var_os("MAST_PREVIEW_APPROVAL_RULE").is_some() {
            let mut rule = mast_proto::work_outline::suggested_approval_rules("s1")[0].clone();
            rule.label = "Release tests".into();
            rule.argv_prefix = vec!["scripts/run_release_tests.sh".into()];
            rule.command_regex = Some(r"^scripts/run_release_tests\.sh [0-9a-f]{40}$".into());
            rule.cwd_scope = Some("/fixture/project".into());
            rule.require_isolated_home = false;
            rule.constraints = Some(mast_proto::work_outline::ApprovalRuleConstraints {
                environment: Some(vec!["CARGO_INCREMENTAL=0".into(), "PYTHONDONTWRITEBYTECODE=1".into()]),
                repository_executable: Some(mast_proto::work_outline::RepositoryApprovalExecutable { path: "scripts/run_release_tests.sh".into(), sha256: None }),
            });
            details.edit_approval_rule(rule, cx);
        }
    });
    cx.new(|_| OutlineFixture {
        outline,
        details,
        _shell: shell,
    })
}

/// The item's artifact list: a pinned plan, two scheduled tasks' results and
/// an ordinary artifact that is newer than both. Shown by the `details` screen
/// with `MAST_PREVIEW_ARTIFACTS=1`; expected order is plan, both results
/// (newest first), then the newer ordinary artifact.
fn artifact_list_fixture(space: &str, item: &str) -> mast_proto::ArtifactsSnapshot {
    let now = crate::artifacts::model::now_ms();
    let (minute, hour) = (60_000, 3_600_000);
    let artifact = |id: &str, title: &str, role: String, age: i64, plan: bool| {
        mast_proto::ArtifactView {
            id: id.into(),
            space_id: space.into(),
            device_id: "local".into(),
            work_item_id: Some(item.into()),
            title: title.into(),
            kind: mast_proto::ArtifactKind::Markdown,
            role: Some(role),
            folder: "api".into(),
            stored_path: format!("/repo/.mast/artifacts/api/{id}.md"),
            current_revision: 3,
            created_at: now - 48 * hour,
            updated_at: now - age,
            pinned_as_plan: plan,
            ..Default::default()
        }
    };
    mast_proto::ArtifactsSnapshot {
        space_id: space.into(),
        artifacts: vec![
            artifact("scratch", "Scratch notes", "doc".into(), 5 * minute, false),
            artifact(
                "slack",
                "Results: Slack digest",
                mast_proto::ScheduledRunResults::role_for("slack"),
                2 * hour,
                false,
            ),
            artifact("plan", "Delivery plan", "plan".into(), 30 * hour, true),
            artifact(
                "github",
                "Results: GitHub review queue",
                mast_proto::ScheduledRunResults::role_for("github"),
                5 * hour,
                false,
            ),
        ],
        ..Default::default()
    }
}

/// Offline token usage page. No engine or user store is opened.
pub fn token_usage_fixture(tab:usize,cx:&mut App)->Entity<impl Render + use<>> {
    let state=cx.new(|_|crate::state::AppState::new());
    cx.new(|cx|crate::settings::token_usage::TokenUsagePage::fixture(state,tab,cx))
}

/// Construct a transcript diagram card with a deterministic preview.
pub fn diagram_card_fixture(cx: &mut App) -> Entity<impl Render + use<>> {
    crate::artifacts::preview_thumb::snapshot_fixture(cx)
}
