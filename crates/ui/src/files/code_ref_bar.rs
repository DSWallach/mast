//! Actions for a selected range in the file editor.
use gpui::{div, prelude::*, px};
use crate::theme::Theme;

const ACTION_RADIUS: f32 = 8.0;
const ACTION_TEXT_SIZE: f32 = 12.0;

pub(super) fn action_bar(theme: &Theme) -> gpui::Div {
    div().absolute().right(px(12.0)).top(px(12.0)).flex().gap(px(8.0))
        .px(px(10.0)).py(px(5.0)).rounded(px(ACTION_RADIUS))
        .bg(theme.surface_raised).border_1().border_color(theme.border)
        .text_color(theme.text).text_size(crate::typography::ui_rems(ACTION_TEXT_SIZE))
}

impl super::FilesSurface {
    pub(super) fn add_selected_code_to_chat(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(path) = self.editor_path.clone() else { return; };
        let Some(editor) = self.preview.documents.get(&path).and_then(|document| document.editor.as_ref()) else { return; };
        let state = editor.read(cx);
        let value = state.value().to_string();
        let selection = state.selected_range();
        if selection.start >= selection.end || selection.end > value.len()
            || !value.is_char_boundary(selection.start) || !value.is_char_boundary(selection.end) { return; }
        let start_line = value.as_bytes()[..selection.start].iter().filter(|&&byte| byte == b'\n').count() as u32 + 1;
        let end_line = value.as_bytes()[..selection.end - 1].iter().filter(|&&byte| byte == b'\n').count() as u32 + 1;
        let range = mast_proto::CodeRefRange { start_line, end_line };
        let Some(snippet) = crate::code_ref::snippet(&value, range) else { return; };
        let Some(member) = self.state.read(cx).selected_execution_member(&self.chat_id) else { return; };
        let reference = crate::code_ref::StagedCodeRef {
            id: uuid::Uuid::new_v4().to_string(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation { repo_id: member.repo_id, worktree_id: member.checkout_id, path },
                kind: mast_proto::CodeRefKind::Lines, range: Some(range), symbol: None,
                at: mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: crate::code_ref::content_hash(&value) },
            }, snippet,
        };
        let key = self.chat_id.clone();
        self.state.update(cx, |state, cx| { state.add_code_ref(&key, reference); cx.notify(); });
    }
}
