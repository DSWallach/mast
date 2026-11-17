use gpui::{AnyElement, div, prelude::*, px};

pub(super) fn pinned_view(
    reference: &crate::code_ref::StagedCodeRef,
    theme: &crate::theme::Theme,
) -> AnyElement {
    let first_line = reference.reference.range.map_or(1, |range| range.start_line);
    let lines = reference.snippet.lines().enumerate().map(|(index, line)| {
        div()
            .flex()
            .gap(px(crate::theme::Theme::SPACE_MD))
            .child(
                div()
                    .w(px(crate::theme::Theme::SPACE_LG * 3.0))
                    .flex_none()
                    .text_right()
                    .text_color(theme.text_faint)
                    .child((first_line as usize + index).to_string()),
            )
            .child(div().child(line.to_owned()))
    });
    div()
        .id("pinned-code-reference")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p(px(12.0))
        .text_color(theme.text)
        .child(
            div()
                .text_color(theme.text_muted)
                .child(format!("{} · pinned · read only", reference.label())),
        )
        .child(
            div()
                .font_family(theme.font_mono.clone())
                .children(lines),
        )
        .into_any_element()
}

// Actions for a selected range in the file editor.

impl super::FilesSurface {
    pub(super) fn selection_actions(
        &self,
        theme: &crate::theme::Theme,
        top: gpui::Pixels,
        left: gpui::Pixels,
        cx: &gpui::Context<Self>,
    ) -> gpui::Div {
        crate::components::code_ref::selection_action_bar(theme, top, left)
            .child(
                div()
                    .id("add-selection-to-chat")
                    .cursor_pointer()
                    .child("Add to chat")
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| this.add_selected_code_to_chat(cx))),
            )
            .child(
                div()
                    .id("copy-selection-ref")
                    .cursor_pointer()
                    .child("Copy ref")
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| this.copy_selected_reference(cx))),
            )
    }

    fn copy_selected_reference(&self, cx: &mut gpui::Context<Self>) {
        let Some(path) = self.editor_path.as_deref() else {
            return;
        };
        let Some(editor) = self
            .preview
            .documents
            .get(path)
            .and_then(|document| document.editor.as_ref())
        else {
            return;
        };
        let state = editor.read(cx);
        let Some(range) = crate::code_ref::selection_lines(&state.value(), state.selected_range())
        else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(format!(
            "{}:{}-{}",
            path, range.start_line, range.end_line
        )));
    }

    pub(super) fn stage_reference(
        &mut self,
        mut reference: crate::code_ref::StagedCodeRef,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(context) = self.request_context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let key = self.chat_id.clone();
        let Some(queue_permit) = crate::code_ref::try_queue_io() else {
            self.error = Some("Too many code references are loading.".into());
            cx.notify();
            return;
        };
        let accepted = self.state.update(cx, |state, cx| {
            let accepted = state.begin_code_ref_load(&key);
            cx.notify();
            accepted
        });
        if !accepted {
            self.error = Some("Too many code references are loading.".into());
            cx.notify();
            return;
        }
        let state_handle = self.state.clone();
        cx.spawn(async move |this, cx| {
            let _queue_permit = queue_permit;
            let _permit = crate::code_ref::acquire_io_permit().await;
            let identity = crate::code_ref::repository_identity(&engine, &context).await;
            state_handle.update(cx, |state, cx| {
                state.end_code_ref_load(&key);
                cx.notify();
            });
            let _ = this.update(cx, |this, cx| {
                if this.request_context.as_ref() != Some(&context) {
                    return;
                }
                if let Some(identity) = identity {
                    reference.reference.location.repo_id = identity.repo_id;
                    reference.reference.location.worktree_id =
                        Some(identity.checkout_id).filter(|id| !id.is_empty());
                    this.state.update(cx, |state, cx| {
                        state.add_code_ref(&key, reference);
                        cx.notify();
                    });
                } else {
                    this.error = Some("Could not identify the workspace repository.".into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn add_selected_code_to_chat(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(path) = self.editor_path.clone() else {
            return;
        };
        let Some(editor) = self
            .preview
            .documents
            .get(&path)
            .and_then(|document| document.editor.as_ref())
        else {
            return;
        };
        let state = editor.read(cx);
        let value = state.value().to_string();
        let selection = state.selected_range();
        let Some(range) = crate::code_ref::selection_lines(&value, selection) else {
            return;
        };
        let Some(snippet) = crate::code_ref::snippet(&value, range) else {
            return;
        };
        let Some(member) = self.state.read(cx).selected_execution_member(&self.chat_id) else {
            return;
        };
        let reference = crate::code_ref::StagedCodeRef {
            id: uuid::Uuid::new_v4().to_string(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation {
                    repo_id: member.repo_id,
                    worktree_id: member.checkout_id,
                    repo_root: None,
                    path,
                },
                kind: mast_proto::CodeRefKind::Lines,
                range: Some(range),
                symbol: None,
                at: mast_proto::CodeRefRevision::WorkingTree {
                    working_tree: true,
                    content_hash: crate::code_ref::content_hash(&value),
                },
            },
            snippet,
        };
        self.stage_reference(reference, cx);
    }
}
