//! Composer chips for staged code references.
use super::*;

pub(super) struct ReferenceChipFocus {
    handle: gpui::FocusHandle,
}

impl Composer {
    pub(super) fn render_code_refs(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let references: Vec<_> = self
            .state
            .read(cx)
            .code_refs(&self.current_key)
            .iter()
            .filter(|reference| reference.reference.kind != mast_proto::CodeRefKind::File)
            .map(|reference| (reference.id.clone(), reference.label()))
            .collect();
        let ids: HashSet<_> = references.iter().map(|(id, _)| id.as_str()).collect();
        self.code_ref_focus.retain(|id, _| ids.contains(id.as_str()));
        if self.code_ref_focus_card.as_ref().is_some_and(|(id, _)| !ids.contains(id.as_str())) {
            self.code_ref_focus_card = None;
        }
        if references.is_empty() {
            return None;
        }
        let mut row = div()
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .tab_group()
            .gap(px(6.0))
            .overflow_hidden()
            .px(px(STRIP_PAD_X))
            .pt(px(STRIP_PAD_TOP));
        for (index, (id, label)) in references.into_iter().enumerate() {
            let label: SharedString = label.into();
            if !self.code_ref_focus.contains_key(&id) {
                let handle = cx.focus_handle().tab_stop(true).tab_index(0);
                self.code_ref_focus.insert(id.clone(), ReferenceChipFocus { handle });
            }
            let focus = self.code_ref_focus[&id].handle.clone();
            let focused = focus.is_focused(window);
            if focused && self.code_ref_focus_card.as_ref().is_none_or(|(active, _)| active != &id)
                && self.code_ref_focus_pending.as_ref() != Some(&id) {
                self.code_ref_focus_pending = Some(id.clone());
                let pending_id = id.clone();
                let pending_focus = focus.clone();
                let pending_key = self.current_key.clone();
                cx.defer_in(window, move |this, window, cx| {
                    if this.code_ref_focus_pending.as_ref() != Some(&pending_id) { return; }
                    this.code_ref_focus_pending = None;
                    if pending_focus.is_focused(window) && this.code_ref_focus.contains_key(&pending_id) {
                        let reference = this.state.read(cx).code_refs(&pending_key).iter().find(|reference| reference.id == pending_id).map(crate::components::code_ref::preview_reference);
                        if let Some(reference) = reference {
                            let card = cx.new(|cx| crate::components::code_ref::ReferenceCard::new(reference, None, cx));
                            this.code_ref_focus_card = Some((pending_id, card));
                            cx.notify();
                        }
                    }
                });
            }
            let card = self.code_ref_focus_card.as_ref().filter(|(active, _)| focused && active == &id).map(|(_, card)| card.clone());
            let focus_for_tooltip = focus.clone();
            let hover_state = self.state.clone();
            let hover_key = self.current_key.clone();
            let hover_id = id.clone();
            row = row.child(
                crate::components::conversation_skill_chip(theme, index)
                    .tooltip(move |_, cx| {
                        let reference = hover_state.read(cx).code_refs(&hover_key).iter().find(|reference| reference.id == hover_id).map(crate::components::code_ref::preview_reference);
                        cx.new(|cx| reference.map_or_else(crate::components::code_ref::ReferenceCard::unavailable, |reference| crate::components::code_ref::ReferenceCard::new(reference, None, cx))).into()
                    })
                    .relative().track_focus(&focus).tab_index(0).role(gpui::Role::Group).aria_label(label.clone())
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .child(label)
                    .when_some(card, |chip, card| chip.child(gpui::canvas(
                        move |bounds, window, _| {
                            let focus = focus_for_tooltip.clone();
                            window.set_tooltip(gpui::AnyTooltip {
                                view: card.clone().into(), mouse_position: point(bounds.left(), bounds.bottom()),
                                check_visible_and_update: Rc::new(move |popup, window, _| focus.is_focused(window) || popup.contains(&window.mouse_position())),
                            });
                        }, |_, _, _, _| (),
                    ).absolute().size_full()))
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
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(Theme::dark()); });
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
        struct Fixture(Entity<Composer>);
        impl Render for Fixture {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().size_full().children(self.0.update(cx, |composer, cx| composer.render_code_refs(&Theme::dark(), window, cx)))
            }
        }
        let (_fixture, vcx) = cx.add_window_view(|_, _| Fixture(composer.clone()));
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        let focus = composer.read_with(vcx, |composer, _| composer.code_ref_focus["ref"].handle.clone());
        vcx.update(|window, cx| window.focus_next(cx));
        vcx.update(|window, _| assert!(focus.is_focused(window)));
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        composer.read_with(vcx, |composer, _| assert!(composer.code_ref_focus_card.is_some()));
        vcx.update(|window, cx| composer.update(cx, |composer, cx| {
            assert!(composer.render_code_refs(&Theme::dark(), window, cx).is_some());
            assert_eq!(composer.button_mode(cx), SendButtonMode::Send);
            composer.state.update(cx, |state, cx| {
                state.remove_code_ref("", "ref");
                cx.notify();
            });
            assert!(composer.render_code_refs(&Theme::dark(), window, cx).is_none());
        }));
    }
}
