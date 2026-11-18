use gpui::{AnyElement, div, prelude::*, px};

pub(super) fn pinned_view(
    reference: &crate::code_ref::StagedCodeRef,
    moved_to: Option<mast_proto::CodeRefRange>,
    source: &str,
    highlight: Option<&mast_syntax::HighlightedDocument>,
    diff: Option<&super::PinnedDiff>,
    theme: &crate::theme::Theme,
    editor_text_size: f32,
    cx: &gpui::Context<super::FilesSurface>,
) -> AnyElement {
    let first_line = reference.reference.range.map_or(1, |range| range.start_line);
    let copied = reference.reference.location.workspace_path();
    let copied = match reference.reference.range {
        Some(range) => format!("{}:{}-{}", copied, range.start_line, range.end_line),
        None => copied,
    };
    div()
        .id("pinned-code-reference")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p(px(crate::theme::Theme::SPACE_MD))
        .text_color(theme.text)
        .child(
            div().flex().items_center().gap(px(crate::theme::Theme::SPACE_SM))
                .text_color(theme.text_muted)
                .child(reference.reference.location.workspace_path())
                .child(reference.reference.range.map(|range| format!("L{}–{}", range.start_line, range.end_line)).unwrap_or_default())
                .child(match moved_to {
                    Some(range) => crate::components::code_ref::moved_reference_badge(theme, range),
                    None => crate::components::code_ref::changed_reference_badge(theme),
                })
                .child(div().flex_1())
                .children(mast_syntax::language_for_path(&reference.reference.location.path)
                    .map(|language| div().text_color(theme.text_faint).child(format!("{language:?}")))),
        )
        .child(if moved_to.is_some() {
            crate::components::code_ref::pinned_reference_notice(theme)
        } else {
            crate::components::code_ref::changed_reference_banner(theme)
        })
        .child(match diff {
            None => crate::components::code_block::code_block(source, first_line, highlight, theme, px(editor_text_size).into()).into_any_element(),
            Some(super::PinnedDiff::Loading) => div().child("Loading current file…").into_any_element(),
            Some(super::PinnedDiff::Error(message)) => div().text_color(theme.warning_muted).child(message.clone()).into_any_element(),
            Some(super::PinnedDiff::Ready(lines)) => div().font_family(theme.font_mono.clone()).text_size(px(editor_text_size))
                .child(div().text_color(theme.text_muted).child("Sent lines compared with current lines at the original range"))
                .children(lines.iter().map(|(tag, line)| {
                    let (marker, color) = match tag {
                        similar::ChangeTag::Delete => ("− ", theme.danger),
                        similar::ChangeTag::Insert => ("+ ", theme.success),
                        similar::ChangeTag::Equal => ("  ", theme.text),
                    };
                    div().text_color(color).child(format!("{marker}{line}"))
                })).into_any_element(),
        })
        .when(source.len() < reference.snippet.len(), |view| view.child("Excerpt truncated"))
        .child(div().flex().items_center().gap(px(crate::theme::Theme::SPACE_SM)).pt(px(crate::theme::Theme::SPACE_MD))
            .child(if let Some(range) = moved_to {
                crate::components::button("pinned-open-moved", format!("Open at L{}–{}", range.start_line, range.end_line), crate::components::ButtonVariant::Primary, crate::components::ButtonSize::Sm, theme)
                    .debug_selector(|| "pinned-open-moved".into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.clear_pinned_reference(cx);
                        this.reveal_editor_location(range.start_line, None, cx);
                    }))
            } else if diff.is_some() {
                crate::components::button("pinned-show-snapshot", "Show snapshot", crate::components::ButtonVariant::Primary, crate::components::ButtonSize::Sm, theme)
                    .on_click(cx.listener(|this, _, _, cx| { this.pinned_diff = None; cx.notify(); }))
            } else {
                crate::components::button("pinned-show-diff", "Show diff vs current", crate::components::ButtonVariant::Primary, crate::components::ButtonSize::Sm, theme)
                    .debug_selector(|| "pinned-show-diff".into())
                    .on_click(cx.listener(|this, _, _, cx| this.show_reference_diff(cx)))
            })
            .when(moved_to.is_none(), |actions| actions.child(crate::components::button("pinned-open-current", "Open current file", crate::components::ButtonVariant::Ghost, crate::components::ButtonSize::Sm, theme)
                .on_click(cx.listener(|this, _, _, cx| this.clear_pinned_reference(cx)))))
            .child(crate::components::button("pinned-add-to-chat", "Add to chat", crate::components::ButtonVariant::Secondary, crate::components::ButtonSize::Sm, theme)
                .on_click(cx.listener(|this, _, _, cx| this.add_pinned_reference_to_chat(cx))))
            .child(crate::components::button("pinned-copy-reference", "Copy ref", crate::components::ButtonVariant::Secondary, crate::components::ButtonSize::Sm, theme)
                .on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copied.clone())))))
        .into_any_element()
}

// Actions for a selected range in the file editor.

impl super::FilesSurface {
    fn add_pinned_reference_to_chat(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(mut reference) = self.pinned_reference.clone() else { return; };
        reference.id = uuid::Uuid::new_v4().to_string();
        let key = self.chat_id.clone();
        self.state.update(cx, |state, cx| {
            state.add_code_ref(&key, reference);
            cx.notify();
        });
    }

    fn show_reference_diff(&mut self, cx: &mut gpui::Context<Self>) {
        let (Some(reference), Some(context), Some(engine)) = (
            self.pinned_reference.as_ref(), self.request_context.clone(), self.state.read(cx).engine().cloned(),
        ) else {
            self.pinned_diff = Some(super::PinnedDiff::Error("Current file is unavailable.".into()));
            cx.notify();
            return;
        };
        let bounded = crate::components::code_ref::preview_reference(reference);
        if bounded.snippet.len() != reference.snippet.len() {
            self.pinned_diff = Some(super::PinnedDiff::Error("The saved excerpt is too large to compare.".into()));
            cx.notify();
            return;
        }
        let reference = bounded;
        let Some(queue_permit) = crate::code_ref::try_queue_io() else {
            self.pinned_diff = Some(super::PinnedDiff::Error("Too many code references are loading.".into()));
            cx.notify();
            return;
        };
        let request = mast_proto::ReadWorkspaceFileRequest {
            target: context.target.clone(), path: reference.reference.location.workspace_path(),
        };
        let client = super::client::WorkspaceFilesClient::new(engine, context.clone());
        self.pinned_diff_generation = self.pinned_diff_generation.wrapping_add(1);
        let generation = self.pinned_diff_generation;
        self.pinned_diff = Some(super::PinnedDiff::Loading);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let _queue_permit = queue_permit;
            let _permit = crate::code_ref::acquire_io_permit().await;
            let result = client.read_file(request).await;
            let task_reference = reference.clone();
            let diff = cx.background_executor().spawn(async move {
                let file = result.map_err(|_| "Could not read the current file.".to_string())?;
                if task_reference.reference.location.worktree_id.as_ref().is_some_and(|id| id != &file.checkout_id) {
                    return Err("The current file belongs to a different checkout.".into());
                }
                if file.truncated { return Err("The current file is too large to compare.".into()); }
                let contents = file.text.ok_or_else(|| "The current file is not text.".to_string())?;
                reference_diff(&task_reference, &contents)
            }).await;
            let _ = this.update(cx, |this, cx| {
                if this.pinned_diff_generation != generation || this.pinned_reference.as_ref() != Some(&reference) || !matches!(this.pinned_diff, Some(super::PinnedDiff::Loading)) { return; }
                if this.request_context.as_ref() != Some(&context) {
                    this.pinned_diff = Some(super::PinnedDiff::Error("Workspace changed while loading the current file.".into()));
                    cx.notify();
                    return;
                }
                this.pinned_diff = Some(match diff {
                    Ok(lines) => super::PinnedDiff::Ready(lines),
                    Err(message) => super::PinnedDiff::Error(message),
                });
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn selection_actions(
        &self,
        theme: &crate::theme::Theme,
        top: gpui::Pixels,
        left: gpui::Pixels,
        cx: &gpui::Context<Self>,
    ) -> gpui::Div {
        crate::components::code_ref::selection_action_bar(theme, top, left)
            .child(
                crate::components::button("add-selection-to-chat", "Add to chat", crate::components::ButtonVariant::Secondary, crate::components::ButtonSize::Sm, theme)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| this.add_selected_code_to_chat(cx))),
            )
            .child(
                crate::components::button("copy-selection-ref", "Copy ref", crate::components::ButtonVariant::Secondary, crate::components::ButtonSize::Sm, theme)
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

fn reference_diff(reference: &crate::code_ref::StagedCodeRef, contents: &str) -> Result<Vec<(similar::ChangeTag, String)>, String> {
    let source = crate::components::code_block::bounded_source(&reference.snippet);
    if source.len() != reference.snippet.len() { return Err("The saved excerpt is too large to compare.".into()); }
    let range = reference.reference.range.ok_or_else(|| "The saved excerpt has no line range.".to_string())?;
    let count = range.end_line.checked_sub(range.start_line).and_then(|length| length.checked_add(1))
        .filter(|count| range.start_line > 0 && *count > 0 && *count <= crate::components::code_block::MAX_LINES as u32)
        .ok_or_else(|| "The saved line range is too large to compare.".to_string())?;
    let mut current = String::new();
    for (index, line) in contents.lines().skip(range.start_line.saturating_sub(1) as usize).take(count as usize).enumerate() {
        if current.len().saturating_add(line.len()).saturating_add(1) > crate::components::code_block::MAX_BYTES {
            return Err("The current excerpt is too large to compare.".into());
        }
        if index > 0 { current.push('\n'); }
        current.push_str(line);
    }
    Ok(similar::TextDiff::from_lines(&source, &current).iter_all_changes()
        .map(|change| (change.tag(), change.value().trim_end_matches('\n').to_owned())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn moved_snapshot_opens_the_reanchored_location(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(crate::theme::Theme::dark()); });
        let state = cx.new(|_| crate::state::AppState::new());
        let (surface, vcx) = cx.add_window_view(move |_, cx| super::super::FilesSurface::new_editor(
            state, "chat".into(), "file.rs".into(), false, 1000, 13.0, false, false, cx,
        ));
        let original = mast_proto::CodeRefRange { start_line: 2, end_line: 3 };
        let moved = mast_proto::CodeRefRange { start_line: 9, end_line: 10 };
        let reference = crate::code_ref::StagedCodeRef {
            id: "reference".into(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation { repo_id: "repo".into(), worktree_id: None, repo_root: None, path: "file.rs".into() },
                kind: mast_proto::CodeRefKind::Lines,
                range: Some(original), symbol: None,
                at: mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: "hash".into() },
            },
            snippet: "one\ntwo".into(),
        };
        surface.update(vcx, |surface, cx| surface.show_pinned_reference(reference.clone(), None, cx));
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("pinned-changed-banner").is_some());
        assert!(vcx.debug_bounds("pinned-show-diff").is_some());
        surface.update(vcx, |surface, cx| surface.show_pinned_reference(reference, Some(moved), cx));
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("pinned-moved-badge").is_some());
        assert!(vcx.debug_bounds("pinned-changed-banner").is_none());
        assert!(vcx.debug_bounds("pinned-show-diff").is_none());
        surface.read_with(vcx, |surface, _| assert_eq!(surface.pinned_reference.as_ref().unwrap().reference.range, Some(original)));
        let button = vcx.debug_bounds("pinned-open-moved").unwrap().center();
        vcx.simulate_click(button, gpui::Modifiers::default());
        surface.read_with(vcx, |surface, _| {
            assert!(surface.pinned_reference.is_none());
            assert_eq!(surface.pending_location_for_test(), Some((moved.start_line, None)));
        });
    }

    #[gpui::test]
    fn restaging_pinned_reference_preserves_origin_and_assigns_unique_ids(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(crate::theme::Theme::dark()); });
        let state = cx.new(|_| crate::state::AppState::new());
        let surface = cx.new(|cx| super::super::FilesSurface::new(state.clone(), "chat".into(), false, 1000, 13.0, false, false, cx));
        let reference = crate::code_ref::StagedCodeRef {
            id: String::new(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation { repo_id: "original-repo".into(), worktree_id: Some("original-checkout".into()), repo_root: None, path: "file.rs".into() },
                kind: mast_proto::CodeRefKind::Lines,
                range: Some(mast_proto::CodeRefRange { start_line: 1, end_line: 1 }),
                symbol: None,
                at: mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: "hash".into() },
            },
            snippet: "saved".into(),
        };
        surface.update(cx, |surface, cx| {
            surface.pinned_reference = Some(reference.clone());
            surface.add_pinned_reference_to_chat(cx);
            surface.add_pinned_reference_to_chat(cx);
        });
        state.read_with(cx, |state, _| {
            let staged = state.code_refs("chat");
            assert_eq!(staged.len(), 2);
            assert_ne!(staged[0].id, staged[1].id);
            assert!(staged.iter().all(|item| item.reference == reference.reference && item.snippet == reference.snippet));
        });
    }

    #[test]
    fn comparison_uses_original_range_and_preserves_empty_lines() {
        let reference = crate::code_ref::StagedCodeRef {
            id: "ref".into(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation { repo_id: "repo".into(), worktree_id: None, repo_root: None, path: "file.rs".into() },
                kind: mast_proto::CodeRefKind::Lines,
                range: Some(mast_proto::CodeRefRange { start_line: 2, end_line: 3 }),
                symbol: None,
                at: mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: "hash".into() },
            },
            snippet: "\nold".into(),
        };
        let changes = reference_diff(&reference, "before\n\nnew\nafter").unwrap();
        assert_eq!(changes, vec![(similar::ChangeTag::Equal, "".into()), (similar::ChangeTag::Delete, "old".into()), (similar::ChangeTag::Insert, "new".into())]);
    }
}
