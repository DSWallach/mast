//! Composer chips for staged code references.
use super::*;

impl Composer {
    pub(super) fn render_code_refs(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let references = self.state.read(cx).code_refs(&self.current_key).to_vec();
        if references.is_empty() {
            return None;
        }
        let mut row = div()
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .overflow_hidden()
            .px(px(STRIP_PAD_X))
            .pt(px(STRIP_PAD_TOP));
        for (index, reference) in references.into_iter().enumerate() {
            let id = reference.id.clone();
            let label: SharedString = reference.label().into();
            row = row.child(
                crate::components::conversation_skill_chip(theme, index)
                    .child(label)
                    .child(
                        crate::components::conversation_skill_remove(theme, index)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                let key = this.current_key.clone();
                                this.state.update(cx, |state, cx| {
                                    state.remove_code_ref(&key, &id);
                                    cx.notify();
                                });
                            })),
                    ),
            );
        }
        Some(row)
    }
}

impl Composer {
    pub(super) fn stage_workspace_file(&mut self, path: &str, cx: &mut Context<Self>) -> bool {
        let key = self.current_key.clone();
        let state = self.state.read(cx);
        let Some(member) = state.selected_execution_member(&key) else {
            return false;
        };
        let Some(context) = crate::files::client::FilesRequestContext::for_chat(state, &key) else {
            return false;
        };
        let Some(engine) = state.engine().cloned() else {
            return false;
        };
        let path = path.to_string();
        let request = mast_proto::ReadWorkspaceFileRequest {
            target: context.target.clone(),
            path: path.clone(),
        };
        let client = crate::files::client::WorkspaceFilesClient::new(engine, context);
        cx.spawn(async move |this, cx| {
            let result = client.read_file(request).await;
            let reference = result.ok().and_then(|file| {
                if file.truncated {
                    return None;
                }
                let snippet = file.text?;
                Some(crate::code_ref::StagedCodeRef {
                    id: uuid::Uuid::new_v4().to_string(),
                    reference: mast_proto::CodeRef {
                        version: 1,
                        location: mast_proto::CodeRefLocation {
                            repo_id: member.repo_id,
                            worktree_id: member.checkout_id,
                            path,
                        },
                        kind: mast_proto::CodeRefKind::File,
                        range: None,
                        symbol: None,
                        at: mast_proto::CodeRefRevision::WorkingTree {
                            working_tree: true,
                            content_hash: crate::code_ref::content_hash(&snippet),
                        },
                    },
                    snippet,
                })
            });
            let _ = this.update(cx, |this, cx| {
                if let Some(reference) = reference {
                    this.state.update(cx, |state, cx| {
                        state.add_code_ref(&key, reference);
                        cx.notify();
                    });
                } else {
                    this.failure = Some("Could not read the workspace file.".into());
                    this.failure_key = Some(key);
                    cx.notify();
                }
            });
        })
        .detach();
        true
    }
}
