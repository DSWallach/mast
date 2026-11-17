//! Offscreen fixture for the production editor and code selection action bar.
use gpui::{App, Context, Entity, Focusable as _, Render, Window, div, prelude::*, px};
use gpui_base::input::{Editor, EditorState};

pub struct CodeReferenceFixture {
    editor: Entity<EditorState>,
    focus_pending: bool,
}

impl Render for CodeReferenceFixture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_pending) {
            let editor = self.editor.clone();
            window.on_next_frame(move |window, cx| {
                window.focus(&editor.focus_handle(cx), cx);
                editor.update(cx, |state, cx| state.set_selected_range(14..57, cx));
            });
        }
        let theme = crate::theme::Theme::of(cx);
        let state = self.editor.read(cx);
        let (top, left) = state
            .range_to_bounds(&state.selected_range())
            .map(|bounds| crate::components::code_ref::selection_action_position(bounds, state.input_bounds()))
            .unwrap_or((px(90.0), px(30.0)));
        div()
            .size_full()
            .relative()
            .bg(theme.surface)
            .child(Editor::new(&self.editor))
            .child(
                crate::components::code_ref::selection_action_bar(theme, top, left)
                    .child("Add to chat")
                    .child("Copy ref"),
            )
    }
}

pub fn fixture(window: &mut Window, cx: &mut App) -> Entity<CodeReferenceFixture> {
    let editor = cx.new(|cx| EditorState::new(window, cx).line_number(true)
        .default_value("fn review() {\n    let first = 1;\n    let second = 2;\n    inspect(first, second);\n}\n"));
    window.focus(&editor.focus_handle(cx), cx);
    let theme = crate::theme::Theme::of(cx).clone();
    editor.update(cx, |state, cx| {
        state.set_editor_style(crate::files::editor_adapter::editor_style(&theme));
        state.set_selected_range(14..57, cx);
    });
    cx.new(|_| CodeReferenceFixture {
        editor,
        focus_pending: true,
    })
}
