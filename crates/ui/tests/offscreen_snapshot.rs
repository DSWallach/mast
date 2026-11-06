#![cfg(all(target_os = "macos", feature = "snapshot"))]
use gpui::{div, prelude::*, px, size};
use mast_ui::{
    theme::{Appearance, Theme},
    ui_style::{self, UiStyleId},
};
use std::path::Path;

struct MenuRow;
impl Render for MenuRow {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div().size_full().bg(theme.surface).child(
            mast_ui::popover::menu_row(theme, true, "snapshot-row")
                .h(px(theme.ui_style.tokens.sidebar_row_compact_h))
                .child("Work outline")
                .child(ui_style::reveal_on_hover(
                    div().id("action").child("Edit"),
                    "row",
                    theme.ui_style,
                    false,
                )),
        )
    }
}
#[test]
#[ignore = "requires macOS Metal and access to macOS services"]
fn render_style_rows_offscreen() -> anyhow::Result<()> {
    let baselines = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    std::fs::create_dir_all(&baselines)?;
    for (style_name, style) in [
        ("minimal", UiStyleId::Minimal),
        ("dense", UiStyleId::Dense),
        ("guided", UiStyleId::Guided),
    ] {
        for (appearance_name, appearance) in
            [("dark", Appearance::Dark), ("light", Appearance::Light)]
        {
            let name = format!("menu-row-{style_name}-{appearance_name}");
            let path = mast_ui::snapshot::render_to_png(
                &name,
                size(px(240.), px(60.)),
                style,
                appearance,
                |_, cx| cx.new(|_| MenuRow),
            )?;
            let baseline = baselines.join(format!("{name}.png"));
            if std::env::var_os("MAST_UPDATE_SNAPSHOTS").is_some() {
                std::fs::copy(&path, &baseline)?;
            }
            let actual = image::open(&path)?.into_rgba8();
            let expected = image::open(&baseline)?.into_rgba8();
            let diff = image_compare::rgba_hybrid_compare(&expected, &actual)?;
            diff.image
                .to_color_map()
                .save(path.with_file_name(format!("{name}.diff.png")))?;
            assert!(diff.score >= 0.9999, "{name} similarity: {}", diff.score);
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires macOS Metal and access to macOS services"]
fn render_chat_shell_offscreen() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let directory = tempfile::tempdir()?;
    let baselines = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    for (style_name, style) in [
        ("minimal", UiStyleId::Minimal),
        ("dense", UiStyleId::Dense),
        ("guided", UiStyleId::Guided),
    ] {
        for (appearance_name, appearance) in
            [("dark", Appearance::Dark), ("light", Appearance::Light)]
        {
            let name = format!("chat-shell-{style_name}-{appearance_name}");
            let data_dir = directory.path().to_path_buf();
            let path = mast_ui::snapshot::render_to_png(
                &name,
                size(px(1100.), px(800.)),
                style,
                appearance,
                move |_, cx| {
                    use mast_ui::{composer, history, settings, state, terminal, typography};
                    let settings = settings::UiSettings::default();
                    settings::init(settings.clone(), data_dir.clone(), cx);
                    let fonts = typography::register_fonts(cx);
                    typography::init(
                        settings.ui_font_family.clone(),
                        settings.ui_font_size,
                        settings.terminal_font_family.clone(),
                        settings.terminal_font_size,
                        settings.code_font_family.clone(),
                        settings.code_font_size,
                        fonts,
                        cx,
                    );
                    Theme::install(appearance, cx);
                    history::init(
                        settings.git_history_columns,
                        settings.git_history_column_widths,
                        settings.git_history_column_order,
                        settings.git_history_author_display,
                        cx,
                    );
                    composer::init(cx, settings.composer_send_behavior);
                    terminal::panel::init(cx);
                    let state = cx.new(|_| {
                    let mut state = state::AppState::new();
                    state.connection = mast_proto::view::ConnectionStatus::Ready;
                    state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
                    state.local_device_id = Some("local".into());
                    state.selected_chat = Some("fixture-chat".into());
                    state.selected_space = Some("fixture-space".into());
                    state.auto_selected = true;
                    state.chats_synced = true;
                    state.spaces_synced = true;
                    state.spaces = vec![serde_json::from_value(serde_json::json!({"id":"fixture-space","deviceId":"local","path":"/fixture/project","createdAt":"2026-09-08T00:00:00Z"})).unwrap()];
                    state.chats = vec![serde_json::from_value(serde_json::json!({"id":"fixture-chat","deviceId":"local","spaceId":"fixture-space","title":"Review the workspace","archived":false,"createdAt":"2026-09-08T00:00:00Z","config":{"harness":"claude-code","model":"claude-sonnet-4-6","reasoning":null,"sandbox":"workspace-write"}})).unwrap()];
                    state
                });
                    state.update(cx, |state, cx| {
                    state.receive_transcript_frame(mast_doc::TranscriptFrame::Reset { reset: serde_json::from_value(serde_json::json!([
                        {"id":"user","role":"user","parts":[{"id":"text","kind":"text","text":"Review the sidebar spacing."}],"createdAt":1788900000000_i64,"deviceId":"local"},
                        {"id":"assistant","role":"assistant","parts":[{"id":"text","kind":"text","text":"I will inspect the layout and report the changes."}],"createdAt":1788900001000_i64,"deviceId":"local","status":"complete"}
                    ])).unwrap() }, cx).unwrap();
                });
                    let boot = mast_ui::EngineBootConfig {
                        data_dir,
                        ipc_port: 0,
                        edge_url: String::new(),
                        edge_token: None,
                        org_id: None,
                        workos_client_id: None,
                        default_harness: mast_proto::HarnessId::Mock,
                    };
                    cx.new(|cx| {
                        let mut shell = mast_ui::shell::Shell::new(state, boot, cx);
                        shell.fixture_snapshot_ready();
                        shell
                    })
                },
            )?;
            let baseline = baselines.join(format!("{name}.png"));
            if std::env::var_os("MAST_UPDATE_SNAPSHOTS").is_some() {
                std::fs::copy(&path, &baseline)?;
            }
            let diff = image_compare::rgba_hybrid_compare(
                &image::open(baseline)?.into_rgba8(),
                &image::open(&path)?.into_rgba8(),
            )?;
            diff.image
                .to_color_map()
                .save(path.with_file_name(format!("{name}.diff.png")))?;
            assert!(diff.score >= 0.9999, "{name} similarity: {}", diff.score);
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires macOS Metal and access to macOS services"]
fn render_walkthrough_offscreen() -> anyhow::Result<()> {
    let baselines = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    for (style_name, style) in [
        ("minimal", UiStyleId::Minimal),
        ("dense", UiStyleId::Dense),
        ("guided", UiStyleId::Guided),
    ] {
        for (appearance_name, appearance) in
            [("dark", Appearance::Dark), ("light", Appearance::Light)]
        {
            let name = format!("walkthrough-{style_name}-{appearance_name}");
            let path = mast_ui::snapshot::render_to_png(
                &name,
                size(px(640.), px(800.)),
                style,
                appearance,
                |_, cx| mast_ui::snapshot::walkthrough_fixture(cx),
            )?;
            let baseline = baselines.join(format!("{name}.png"));
            if std::env::var_os("MAST_UPDATE_SNAPSHOTS").is_some() {
                std::fs::copy(&path, &baseline)?;
            }
            let diff = image_compare::rgba_hybrid_compare(
                &image::open(baseline)?.into_rgba8(),
                &image::open(&path)?.into_rgba8(),
            )?;
            diff.image
                .to_color_map()
                .save(path.with_file_name(format!("{name}.diff.png")))?;
            assert!(diff.score >= 0.9999, "{name} similarity: {}", diff.score);
        }
    }
    Ok(())
}

#[test]
#[ignore = "requires macOS Metal and access to macOS services"]
fn render_outline_offscreen() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let directory = tempfile::tempdir()?;
    let baselines = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    for (style_name, style) in [
        ("minimal", UiStyleId::Minimal),
        ("dense", UiStyleId::Dense),
        ("guided", UiStyleId::Guided),
    ] {
        for (appearance_name, appearance) in
            [("dark", Appearance::Dark), ("light", Appearance::Light)]
        {
            let name = format!("outline-{style_name}-{appearance_name}");
            let data_dir = directory.path().to_path_buf();
            let path = mast_ui::snapshot::render_to_png(
                &name,
                size(px(1100.), px(800.)),
                style,
                appearance,
                move |_, cx| {
                    let settings = mast_ui::settings::UiSettings::default();
                    mast_ui::settings::init(settings.clone(), data_dir.clone(), cx);
                    mast_ui::history::init(
                        settings.git_history_columns,
                        settings.git_history_column_widths,
                        settings.git_history_column_order,
                        settings.git_history_author_display,
                        cx,
                    );
                    mast_ui::composer::init(cx, settings.composer_send_behavior);
                    mast_ui::terminal::panel::init(cx);
                    mast_ui::snapshot::outline_fixture(data_dir, cx)
                },
            )?;
            let baseline = baselines.join(format!("{name}.png"));
            if std::env::var_os("MAST_UPDATE_SNAPSHOTS").is_some() {
                std::fs::copy(&path, &baseline)?;
            }
            let diff = image_compare::rgba_hybrid_compare(
                &image::open(baseline)?.into_rgba8(),
                &image::open(&path)?.into_rgba8(),
            )?;
            diff.image
                .to_color_map()
                .save(path.with_file_name(format!("{name}.diff.png")))?;
            assert!(diff.score >= 0.9999, "{name} similarity: {}", diff.score);
        }
    }
    Ok(())
}

#[test]
fn image_comparison_detects_visible_changes() -> anyhow::Result<()> {
    let baseline = image::open(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/menu-row-dense-dark.png"),
    )?
    .into_rgba8();
    let mut changed = baseline.clone();
    for y in 0..30 {
        for x in 0..100 {
            changed.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
        }
    }
    let diff = image_compare::rgba_hybrid_compare(&baseline, &changed)?;
    assert!(
        diff.score < 0.9999,
        "visible edit was not detected: {}",
        diff.score
    );
    let image = diff.image.to_color_map().to_rgba8();
    assert!(image.pixels().any(|pixel| pixel.0[0] != pixel.0[1]));
    Ok(())
}

#[test]
#[ignore = "requires macOS Metal and access to macOS services"]
fn render_style_theme_matrix() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let directory = tempfile::tempdir()?;
    for (name, style, appearance, variant) in [
        ("industrial-default", UiStyleId::Industrial, Appearance::Light, "mast-light"),
        ("industrial-light", UiStyleId::Industrial, Appearance::Light, "industrial-light"),
        ("industrial-dark", UiStyleId::Industrial, Appearance::Dark, "industrial-dark"),
        ("industrial-github-dark", UiStyleId::Industrial, Appearance::Dark, "github-dark"),
        ("minimal-industrial-light", UiStyleId::Minimal, Appearance::Light, "industrial-light"),
        ("area-default", UiStyleId::Area, Appearance::Light, "mast-light"),
        ("area-light", UiStyleId::Area, Appearance::Light, "area-light"),
        ("area-dark", UiStyleId::Area, Appearance::Dark, "area-dark"),
        ("area-industrial-light", UiStyleId::Area, Appearance::Light, "industrial-light"),
    ] {
        let data_dir = directory.path().to_path_buf();
        mast_ui::snapshot::render_to_png(&format!("matrix-chat-{name}"), size(px(1100.), px(800.)), style, appearance,
                move |_, cx| {
                    use mast_ui::{composer, history, settings, state, terminal, typography};
                    let settings = settings::UiSettings::default();
                    settings::init(settings.clone(), data_dir.clone(), cx);
                    let fonts = typography::register_fonts(cx);
                    typography::init(
                        settings.ui_font_family.clone(),
                        settings.ui_font_size,
                        settings.terminal_font_family.clone(),
                        settings.terminal_font_size,
                        settings.code_font_family.clone(),
                        settings.code_font_size,
                        fonts,
                        cx,
                    );
                    Theme::install_selection(appearance, variant, Default::default(), Default::default(), cx);
                    history::init(
                        settings.git_history_columns,
                        settings.git_history_column_widths,
                        settings.git_history_column_order,
                        settings.git_history_author_display,
                        cx,
                    );
                    composer::init(cx, settings.composer_send_behavior);
                    terminal::panel::init(cx);
                    let state = cx.new(|_| {
                    let mut state = state::AppState::new();
                    state.connection = mast_proto::view::ConnectionStatus::Ready;
                    state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
                    state.local_device_id = Some("local".into());
                    state.selected_chat = Some("fixture-chat".into());
                    state.selected_space = Some("fixture-space".into());
                    state.auto_selected = true;
                    state.chats_synced = true;
                    state.spaces_synced = true;
                    state.spaces = vec![serde_json::from_value(serde_json::json!({"id":"fixture-space","deviceId":"local","path":"/fixture/project","createdAt":"2026-09-08T00:00:00Z"})).unwrap()];
                    state.chats = vec![serde_json::from_value(serde_json::json!({"id":"fixture-chat","deviceId":"local","spaceId":"fixture-space","title":"Review the workspace","archived":false,"createdAt":"2026-09-08T00:00:00Z","config":{"harness":"claude-code","model":"claude-sonnet-4-6","reasoning":null,"sandbox":"workspace-write"}})).unwrap()];
                    state
                });
                    state.update(cx, |state, cx| {
                    state.receive_transcript_frame(mast_doc::TranscriptFrame::Reset { reset: serde_json::from_value(serde_json::json!([
                        {"id":"user","role":"user","parts":[{"id":"text","kind":"text","text":"Review the sidebar spacing."}],"createdAt":1788900000000_i64,"deviceId":"local"},
                        {"id":"assistant","role":"assistant","parts":[{"id":"text","kind":"text","text":"I will inspect the layout and report the changes."}],"createdAt":1788900001000_i64,"deviceId":"local","status":"complete"}
                    ])).unwrap() }, cx).unwrap();
                });
                    let boot = mast_ui::EngineBootConfig {
                        data_dir,
                        ipc_port: 0,
                        edge_url: String::new(),
                        edge_token: None,
                        org_id: None,
                        workos_client_id: None,
                        default_harness: mast_proto::HarnessId::Mock,
                    };
                    cx.new(|cx| {
                        let mut shell = mast_ui::shell::Shell::new(state, boot, cx);
                        shell.fixture_snapshot_ready();
                        shell
                    })
                },
        )?;
        let data_dir = directory.path().to_path_buf();
        mast_ui::snapshot::render_to_png(&format!("matrix-details-{name}"), size(px(1100.), px(800.)), style, appearance, move |_, cx| {
            let settings = mast_ui::settings::UiSettings::default();
            mast_ui::settings::init(settings.clone(), data_dir.clone(), cx);
            mast_ui::history::init(settings.git_history_columns, settings.git_history_column_widths, settings.git_history_column_order, settings.git_history_author_display, cx);
            mast_ui::composer::init(cx, settings.composer_send_behavior);
            mast_ui::terminal::panel::init(cx);
            Theme::install_selection(appearance, variant, Default::default(), Default::default(), cx);
            mast_ui::snapshot::outline_fixture(data_dir, cx)
        })?;
        mast_ui::snapshot::render_to_png(&format!("matrix-walkthrough-{name}"), size(px(640.), px(800.)), style, appearance, move |_, cx| {
            Theme::install_selection(appearance, variant, Default::default(), Default::default(), cx);
            mast_ui::snapshot::walkthrough_fixture(cx)
        })?;
    }
    Ok(())
}
