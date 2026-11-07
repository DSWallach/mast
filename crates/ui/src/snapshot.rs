//! Offscreen Metal snapshots for test fixtures. No native window is created.
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
/// A Metal device and access to macOS services are required.
pub fn render_to_png<V: Render + 'static>(
    name: &str,
    size: Size<Pixels>,
    style: UiStyleId,
    appearance: Appearance,
    build_view: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<PathBuf> {
    render_to_png_with_layout(name, size, style, UiLayout::Minimal, appearance, build_view)
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

/// Construct the walkthrough test fixture using the production view.
pub fn walkthrough_fixture(cx: &mut App) -> Entity<impl Render + use<>> {
    crate::changes::walkthrough::snapshot_fixture(cx)
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

impl gpui::Render for OutlineFixture {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{div, prelude::*, px};
        div()
            .size_full()
            .flex()
            .child(div().w(px(420.)).h_full().child(self.outline.clone()))
            .child(div().flex_1().h_full().child(self.details.clone()))
    }
}

/// Construct the existing Work Outline fixture with an item details panel.
pub fn preview_chat_outline(state: &mut crate::state::AppState) {
    use crate::work_outline::store::{fixture_ids, fixture_snapshot};
    use mast_proto::{UserActionRequestState, UserActionRequestView};

    let mut snapshot = fixture_snapshot("fixture-space");
    snapshot.external_references.clear();
    let item = snapshot
        .items
        .iter_mut()
        .find(|item| item.id == fixture_ids::API)
        .unwrap();
    item.chat_ids.push("fixture-chat".into());
    for (id, text) in [
        ("preview-approve", "Approve the proposed layout"),
        ("preview-review", "Review the implementation"),
    ] {
        snapshot.user_action_requests.push(UserActionRequestView {
            id: id.into(),
            space_id: "fixture-space".into(),
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
    state.set_work_outline_for_test(snapshot);
}

pub fn outline_fixture(data_dir: PathBuf, cx: &mut App) -> Entity<impl Render + use<>> {
    use crate::work_outline::{
        model::OutlineSpace,
        outline_view::{OutlineInputs, OutlineView},
        store::{WorkOutlineLoad, fixture_ids, fixture_snapshot},
    };
    let state = cx.new(|_| {
        let mut state = crate::state::AppState::new();
        state.connection = mast_proto::view::ConnectionStatus::Ready;
        state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
        state.selected_space = Some("s1".into());
        state.set_work_outline_for_test(fixture_snapshot("s1"));
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
                    load: WorkOutlineLoad::Ready(Arc::new(fixture_snapshot("s1"))),
                    hidden: false,
                }],
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
            fixture_ids::API.into(),
            cx,
        )
    });
    details.update(cx, |details, _| {
        details.set_data(Some(Arc::new(fixture_snapshot("s1"))), Arc::new(Vec::new()))
    });
    cx.new(|_| OutlineFixture {
        outline,
        details,
        _shell: shell,
    })
}
