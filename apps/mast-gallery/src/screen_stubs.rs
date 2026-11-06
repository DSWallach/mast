//! Fixture-only approximations for views that require the live shell and RPC.
use gpui::{Div, div, prelude::*, px};
use mast_ui::{popover, theme::Theme, typography};

pub fn sidebar(theme: &Theme) -> Div {
    let height = theme.ui_style.tokens.sidebar_row_compact_h;
    div()
        .w(px(180.0))
        .h_full()
        .p(px(8.0))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .border_r_1()
        .border_color(theme.border)
        .child("Mast Dev")
        .child(
            div()
                .h(px(height))
                .px(px(8.0))
                .flex()
                .items_center()
                .rounded(px(5.0))
                .bg(theme.surface_raised)
                .child("● Gallery fixtures"),
        )
        .child(
            div()
                .h(px(height))
                .px(px(8.0))
                .flex()
                .items_center()
                .child("○ Design review"),
        )
}

pub fn outline(theme: &Theme) -> Div {
    let style = theme.ui_style;
    let row = |label: &'static str, depth: f32, selected: bool| {
        div()
            .h(px(29.0 + style.tokens.outline_row_delta))
            .pl(px(12.0 + depth * 20.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded(px(5.0))
            .when(selected, |el| el.bg(theme.surface_raised))
            .child(if selected { "●" } else { "○" })
            .child(label)
    };
    div()
        .flex()
        .size_full()
        .gap(px(20.0))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(
                    div()
                        .text_size(typography::ui_rems(16.0))
                        .child("Work outline"),
                )
                .child(row("▾ Mast Dev", 0.0, false))
                .child(row("▾ Features", 1.0, false))
                .child(row("Build the gallery", 2.0, true))
                .child(row("Add screen fixtures", 3.0, false))
                .child(row("Verify styles", 3.0, false)),
        )
        .child(
            div()
                .w(px(300.0))
                .p(px(16.0))
                .rounded(px(8.0))
                .bg(theme.surface_raised)
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child("Item details")
                .child(
                    div()
                        .text_size(typography::ui_rems(16.0))
                        .child("Build the gallery"),
                )
                .child("Active · Features")
                .child("Compare Mast controls and screens using fixture data.")
                .child("Instructions")
                .child("Use headless tests for every style."),
        )
}

pub fn walkthrough(theme: &Theme) -> Div {
    div()
        .size_full()
        .flex()
        .gap(px(20.0))
        .child(
            div()
                .w(px(240.0))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child("Review walkthrough")
                .child("1  Understand the change")
                .child("2  Check the fixture")
                .child("3  Review the tests")
                .child(popover::btn_primary(theme, "Next step")),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(
                    div()
                        .text_size(typography::ui_rems(16.0))
                        .child("1. Understand the change"),
                )
                .child("This change keeps the selected style in memory.")
                .child(
                    div()
                        .p(px(12.0))
                        .rounded(px(6.0))
                        .bg(theme.surface_raised)
                        .child("Note: Check that the setting is never persisted."),
                )
                .child(
                    div()
                        .p(px(12.0))
                        .rounded(px(6.0))
                        .bg(theme.surface_raised)
                        .font_family(theme.font_mono.clone())
                        .child(
                            "src/gallery.rs\n− save_style(style);\n+ set_style_in_memory(style);",
                        ),
                ),
        )
}
