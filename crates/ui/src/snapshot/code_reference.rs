//! Offscreen fixture for the production editor and code selection action bar.
use gpui::{App, Context, Entity, Focusable as _, Render, Window, div, prelude::*, px};
use gpui_base::input::{Editor, EditorState};

pub struct CodeReferenceFixture {
    editor: Entity<EditorState>,
    pinned: Entity<crate::files::FilesSurface>,
    card: Entity<crate::components::code_ref::ReferenceCard>,
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
            .flex().flex_col()
            .bg(theme.surface)
            .child(
                div().flex().flex_1().min_h_0()
                    .child(div().flex_1().min_w_0().relative()
                        .child(Editor::new(&self.editor))
                        .child(crate::components::code_ref::selection_action_bar(theme, top, left)
                            .child("Add to chat").child("Copy ref")))
                    .child(div().flex_1().min_w_0().child(self.pinned.clone())),
            )
            .child(div().flex_none().child(self.card.clone()))
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
    let state = cx.new(|_| crate::state::AppState::new());
    let pinned = cx.new(|cx| crate::files::FilesSurface::new_editor(
        state, "preview".into(), "src/review.rs".into(), false, 1000, 13.0, false, false, cx,
    ));
    let reference = crate::code_ref::StagedCodeRef {
        id: "preview".into(),
        reference: mast_proto::CodeRef {
            version: 1,
            location: mast_proto::CodeRefLocation { repo_id: "repo".into(), worktree_id: None, repo_root: None, path: "src/review.rs".into() },
            kind: mast_proto::CodeRefKind::Lines,
            range: Some(mast_proto::CodeRefRange { start_line: 12, end_line: 15 }),
            symbol: None,
            at: mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: "preview".into() },
        },
        snippet: "fn review() {\n    let first = 1;\n    inspect(first);\n}".into(),
    };
    let mut card_reference = reference.clone();
    card_reference.reference.range = Some(mast_proto::CodeRefRange { start_line: 12, end_line: 18 });
    card_reference.snippet = "fn review() {\n    let first = 1;\n    inspect(first);\n    let second = 2;\n    inspect(second);\n    finish();\n}".into();
    let moved_to = std::env::var_os("MAST_PREVIEW_CODE_REF_MOVED").map(|_| mast_proto::CodeRefRange { start_line: 22, end_line: 25 });
    let resolution = moved_to.map_or(crate::code_ref::Resolution::Changed, crate::code_ref::Resolution::Moved);
    let card = cx.new(|cx| crate::components::code_ref::ReferenceCard::new(card_reference, Some(resolution), cx));
    pinned.update(cx, |pinned, cx| pinned.show_pinned_reference(reference, moved_to, cx));
    cx.new(|_| CodeReferenceFixture {
        editor,
        pinned,
        card,
        focus_pending: true,
    })
}
