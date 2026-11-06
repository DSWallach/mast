//! Offscreen Metal snapshots for test fixtures. No native window is created.
use std::{path::PathBuf, sync::Arc};

use gpui::{App, AppContext, Entity, HeadlessAppContext, Pixels, Render, Size, Window};

use crate::{
    theme::{Appearance, Theme},
    ui_style::{self, UiStyleId},
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
        ui_style::init(style, cx);
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

struct OutlineFixture {
    outline: Entity<crate::work_outline::outline_view::OutlineView>,
    details: Entity<crate::work_outline::details_panel::WorkItemDetails>,
    _shell: Entity<crate::shell::Shell>,
}

impl gpui::Render for OutlineFixture {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{div, prelude::*, px};
        div().size_full().flex().child(div().w(px(420.)).h_full().child(self.outline.clone()))
            .child(div().flex_1().h_full().child(self.details.clone()))
    }
}

/// Construct the existing Work Outline fixture with an item details panel.
pub fn outline_fixture(data_dir: PathBuf, cx: &mut App) -> Entity<impl Render + use<>> {
    use crate::work_outline::{model::OutlineSpace, outline_view::{OutlineInputs, OutlineView}, store::{fixture_ids, fixture_snapshot, WorkOutlineLoad}};
    let state = cx.new(|_| {
        let mut state = crate::state::AppState::new();
        state.connection = mast_proto::view::ConnectionStatus::Ready;
        state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
        state.selected_space = Some("s1".into());
        state.set_work_outline_for_test(fixture_snapshot("s1"));
        state
    });
    let boot = crate::EngineBootConfig { data_dir, ipc_port: 0, edge_url: String::new(), edge_token: None, org_id: None, workos_client_id: None, default_harness: mast_proto::HarnessId::Mock };
    let shell = cx.new(|cx| crate::shell::Shell::new(state.clone(), boot, cx));
    let outline = cx.new(|cx| OutlineView::new(state.clone(), cx));
    outline.update(cx, |outline, cx| outline.set_inputs(OutlineInputs {
        spaces: vec![OutlineSpace { id: "s1".into(), label: "Project".into(), load: WorkOutlineLoad::Ready(Arc::new(fixture_snapshot("s1"))), hidden: false }],
        ..Default::default()
    }, cx));
    let details = cx.new(|cx| crate::work_outline::details_panel::WorkItemDetails::new(state, shell.downgrade(), "s1".into(), fixture_ids::API.into(), cx));
    details.update(cx, |details, _| details.set_data(Some(Arc::new(fixture_snapshot("s1"))), Arc::new(Vec::new())));
    cx.new(|_| OutlineFixture { outline, details, _shell: shell })
}
