//! Composer chips for staged code references.
use super::*;

impl Composer {
    pub(super) fn render_code_refs(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let references: Vec<_> = self
            .state
            .read(cx)
            .code_refs(&self.current_key)
            .iter()
            .filter(|reference| reference.reference.kind != mast_proto::CodeRefKind::File)
            .cloned()
            .collect();
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
                    .tooltip(move |_, cx| cx.new(|cx| crate::components::code_ref::ReferenceCard::new(reference.clone(), cx)).into())
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
        let client =
            crate::files::client::WorkspaceFilesClient::new(engine.clone(), context.clone());
        let Some(queue_permit) = crate::code_ref::try_queue_io() else {
            self.failure = Some("Too many code references are loading.".into());
            self.failure_key = Some(key);
            cx.notify();
            return true;
        };
        let state_handle = self.state.clone();
        let accepted = state_handle.update(cx, |state, cx| {
            let accepted = state.begin_code_ref_load(&key);
            cx.notify();
            accepted
        });
        if !accepted {
            self.failure = Some("Too many code references are loading.".into());
            self.failure_key = Some(key);
            cx.notify();
            return true;
        }
        cx.spawn(async move |this, cx| {
            let _queue_permit = queue_permit;
            let _permit = crate::code_ref::acquire_io_permit().await;
            let identity = crate::code_ref::repository_identity(&engine, &context).await;
            let result = client.read_file(request).await;
            let reference = identity.and_then(|identity| {
                result.ok().and_then(|file| {
                    if file.truncated || file.checkout_id != identity.checkout_id {
                        return None;
                    }
                    let snippet = file.text?;
                    Some(crate::code_ref::StagedCodeRef {
                        id: uuid::Uuid::new_v4().to_string(),
                        reference: mast_proto::CodeRef {
                            version: 1,
                            location: mast_proto::CodeRefLocation {
                                repo_id: identity.repo_id,
                                worktree_id: Some(identity.checkout_id).filter(|id| !id.is_empty()),
                                repo_root: None,
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
                })
            });
            state_handle.update(cx, |state, cx| {
                state.end_code_ref_load(&key);
                cx.notify();
            });
            let _ = this.update(cx, |this, cx| {
                let current =
                    crate::files::client::FilesRequestContext::for_chat(this.state.read(cx), &key);
                if current.as_ref() != Some(&context) {
                    return;
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn staged_reference_renders_and_can_be_removed(cx: &mut TestAppContext) {
        let state = cx.new(|_| AppState::new());
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        let reference = crate::code_ref::StagedCodeRef {
            id: "ref".into(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation {
                    repo_id: "repo".into(),
                    worktree_id: None,
                    repo_root: None,
                    path: "src/file.rs".into(),
                },
                kind: mast_proto::CodeRefKind::Lines,
                range: Some(mast_proto::CodeRefRange {
                    start_line: 1,
                    end_line: 2,
                }),
                symbol: None,
                at: mast_proto::CodeRefRevision::WorkingTree {
                    working_tree: true,
                    content_hash: "hash".into(),
                },
            },
            snippet: "one\ntwo".into(),
        };
        state.update(cx, |state, cx| {
            state.add_code_ref("", reference);
            cx.notify();
        });
        composer.update(cx, |composer, cx| {
            assert!(composer.render_code_refs(&Theme::dark(), cx).is_some());
            assert_eq!(composer.button_mode(cx), SendButtonMode::Send);
            composer.state.update(cx, |state, cx| {
                state.remove_code_ref("", "ref");
                cx.notify();
            });
            assert!(composer.render_code_refs(&Theme::dark(), cx).is_none());
        });
    }
}
