use gpui::{AnyView, App, Entity, Render, Window, div, prelude::*, px, size};
use mast_ui::{
    theme::{Appearance, Theme},
    ui_style::{UiLayout, UiStyleId},
};
use std::path::PathBuf;

struct Frame {
    screen: String,
    label: String,
    view: AnyView,
}
impl Render for Frame {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .text_color(theme.text)
            .child(
                div()
                    .h(px(30.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .bg(theme.surface_raised)
                    .child(format!("{}   |   {}", self.screen, self.label)),
            )
            .child(div().flex_1().min_h_0().child(self.view.clone()))
    }
}

struct Controls {
    input: Entity<mast_ui::composer::ComposerInput>,
}
impl Render for Controls {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = Theme::of(cx);
        div()
            .size_full()
            .p(px(32.))
            .flex()
            .flex_col()
            .gap(px(18.))
            .bg(t.surface)
            .text_color(t.text)
            .child("Buttons and toggles")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(mast_ui::components::button(
                        "preview-primary",
                        "Primary",
                        mast_ui::components::ButtonVariant::Primary,
                        mast_ui::components::ButtonSize::Md,
                        t,
                    ))
                    .child(mast_ui::components::button(
                        "preview-secondary",
                        "Secondary",
                        mast_ui::components::ButtonVariant::Secondary,
                        mast_ui::components::ButtonSize::Md,
                        t,
                    ))
                    .child(mast_ui::components::button(
                        "preview-danger",
                        "Delete",
                        mast_ui::components::ButtonVariant::Danger,
                        mast_ui::components::ButtonSize::Md,
                        t,
                    ))
                    .child(mast_ui::components::toggle_switch(t, true))
                    .child(mast_ui::components::toggle_switch(t, false))
                    .child(mast_ui::components::badge(t, "Draft"))
                    .child(mast_ui::components::badge_active(t, "Active")),
            )
            .child("Tabs and status")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(
                        div()
                            .px(px(8.))
                            .py(px(5.))
                            .bg(t.surface_raised)
                            .child("Overview"),
                    )
                    .child("Activity")
                    .children(
                        [
                            (t.success, "Ready"),
                            (t.warning, "Waiting"),
                            (t.danger, "Error"),
                        ]
                        .into_iter()
                        .map(|(color, label)| {
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .child(div().size(px(7.)).rounded_full().bg(color))
                                .child(label)
                        }),
                    ),
            )
            .child("Input")
            .child(
                mast_ui::components::dialog_field(t, self.input.clone().into_any_element()).w(px(360.)),
            )
    }
}

fn initialize(
    data_dir: &std::path::Path,
    style: UiStyleId,
    layout: UiLayout,
    appearance: Appearance,
    theme_id: &str,
    cx: &mut App,
) {
    let mut settings = mast_ui::settings::UiSettings::default();
    settings.ui_style = style;
    settings.ui_layout = layout;
    match appearance {
        Appearance::Light => {
            settings.appearance = mast_ui::appearance::AppearanceMode::Light;
            settings.theme_selection.light = theme_id.to_owned();
        }
        Appearance::Dark => {
            settings.appearance = mast_ui::appearance::AppearanceMode::Dark;
            settings.theme_selection.dark = theme_id.to_owned();
        }
    }
    mast_ui::settings::init(settings.clone(), data_dir, cx);
    let fonts = mast_ui::typography::register_fonts(cx);
    mast_ui::typography::init(
        settings.ui_font_family.clone(),
        settings.ui_font_size,
        settings.terminal_font_family.clone(),
        settings.terminal_font_size,
        settings.code_font_family.clone(),
        settings.code_font_size,
        fonts,
        cx,
    );
    mast_ui::history::init(
        settings.git_history_columns,
        settings.git_history_column_widths,
        settings.git_history_column_order,
        settings.git_history_author_display,
        cx,
    );
    mast_ui::composer::init(cx, settings.composer_send_behavior);
    mast_ui::terminal::panel::init(cx);
    Theme::install_selection(
        appearance,
        theme_id,
        mast_theme::AccentSelection::ThemeDefault,
        mast_theme::SurfacePreference::default(),
        cx,
    );
}

fn chat(data_dir: PathBuf, cx: &mut App) -> AnyView {
    let state = cx.new(|_| {
        let mut state = mast_ui::state::AppState::new();
        state.connection = mast_proto::view::ConnectionStatus::Ready;
        state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
        state.local_device_id = Some("local".into());
        state.selected_chat = Some("fixture-chat".into());
        state.selected_space = Some("fixture-space".into());
        state.auto_selected = true; state.chats_synced = true; state.spaces_synced = true;
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
    .into()
}

pub fn render(
    screen: &str,
    style: UiStyleId,
    layout: UiLayout,
    theme_id: &str,
    appearance: Appearance,
    label: String,
    data_dir: PathBuf,
    name: &str,
) -> anyhow::Result<PathBuf> {
    let screen = screen.to_owned();
    let theme_id = theme_id.to_owned();
    mast_ui::snapshot::render_to_png_with_layout(
        name,
        size(px(1440.), px(900.)),
        style,
        layout,
        appearance,
        move |_, cx| {
            initialize(&data_dir, style, layout, appearance, &theme_id, cx);
            let view: AnyView = match screen.as_str() {
                "chat" => chat(data_dir, cx),
                "details" => mast_ui::snapshot::outline_fixture(data_dir, cx).into(),
                "walkthrough" => mast_ui::snapshot::walkthrough_fixture(cx).into(),
                "appearance" => cx
                    .new(mast_ui::settings::appearance::AppearancePage::new)
                    .into(),
                "controls" => cx
                    .new(|cx| Controls {
                        input: cx
                            .new(|cx| mast_ui::composer::ComposerInput::new("Type a message…", cx)),
                    })
                    .into(),
                _ => unreachable!(),
            };
            cx.new(|_| Frame {
                screen,
                label,
                view,
            })
        },
    )
}
