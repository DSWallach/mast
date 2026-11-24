use gpui::{AnyElement, div, prelude::*, px};
use crate::work_outline::hints::{HintKey, WithHint};

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
    fn code_ref_message(&mut self, message: impl Into<gpui::SharedString>, success: bool, cx: &mut gpui::Context<Self>) {
        self.code_ref_status = Some(message.into());
        self.code_ref_status_error = !success;
        self.code_ref_status_selection = self.editor_path.as_ref().and_then(|path| {
            self.preview.documents.get(path).and_then(|document| document.editor.as_ref())
                .map(|editor| (path.clone(), editor.read(cx).selected_range()))
        });
        self.code_ref_status_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
            let _ = this.update(cx, |this, cx| { this.clear_code_ref_message(cx); });
        }));
        cx.notify();
    }

    pub(super) fn clear_code_ref_message(&mut self, cx: &mut gpui::Context<Self>) {
        self.code_ref_status = None;
        self.code_ref_status_selection = None;
        self.code_ref_status_task = None;
        cx.notify();
    }

    pub(super) fn observe_code_ref_selection(&mut self, path: &str, editor: &gpui::Entity<super::editor::FileEditorState>, cx: &mut gpui::Context<Self>) {
        if self.code_ref_status_selection.as_ref().is_some_and(|(status_path, selection)| {
            status_path == path && *selection != editor.read(cx).selected_range()
        }) {
            self.clear_code_ref_message(cx);
        }
        cx.notify();
    }

    fn selected_chat(&mut self, cx: &mut gpui::Context<Self>) -> Option<String> {
        let key = self.state.read(cx).selected_chat.clone();
        if key.is_none() { self.code_ref_message("Open a chat to add code to it.", false, cx); }
        key
    }

    fn confirm_code_ref(&mut self, key: &str, cx: &mut gpui::Context<Self>) {
        let title = self.state.read(cx).chats.iter().find(|chat| chat.id == key)
            .and_then(|chat| chat.title.as_deref()).filter(|title| !title.is_empty());
        let message = title.map(|title| format!("Added to {title}")).unwrap_or_else(|| "Added to chat".into());
        self.code_ref_message(message, true, cx);
    }

    fn add_pinned_reference_to_chat(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(key) = self.selected_chat(cx) else { return; };
        let Some(mut reference) = self.pinned_reference.clone() else {
            self.code_ref_message("Select code to add to chat.", false, cx); return;
        };
        reference.id = uuid::Uuid::new_v4().to_string();
        self.state.update(cx, |state, cx| { state.add_code_ref(&key, reference); cx.notify(); });
        self.confirm_code_ref(&key, cx);
    }

    pub(super) fn has_code_selection(&self, cx: &gpui::Context<Self>) -> bool {
        self.editor_path.as_ref().and_then(|path| self.preview.documents.get(path))
            .and_then(|document| document.editor.as_ref())
            .is_some_and(|editor| !editor.read(cx).selected_range().is_empty())
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
                    .hint("Add to chat", HintKey::App(crate::settings::ShortcutId::AddCodeToChat))
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
        let Some(key) = self.selected_chat(cx) else { return; };
        let Some(context) = self.request_context.clone() else {
            self.code_ref_message("Workspace is unavailable.", false, cx); return;
        };
        let engine = self.state.read(cx).engine().cloned();
        let Some(engine) = engine else {
            self.code_ref_message("Workspace service is unavailable.", false, cx); return;
        };
        let Some(queue_permit) = crate::code_ref::try_queue_io() else {
            self.code_ref_message("Too many code references are loading.", false, cx);
            return;
        };
        let accepted = self.state.update(cx, |state, cx| {
            let accepted = state.begin_code_ref_load(&key);
            cx.notify();
            accepted
        });
        if !accepted {
            self.code_ref_message("Too many code references are loading.", false, cx);
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
                    this.code_ref_message("Workspace changed while adding code.", false, cx); return;
                }
                if let Some(identity) = identity {
                    reference.reference.location.repo_id = identity.repo_id;
                    reference.reference.location.worktree_id =
                        Some(identity.checkout_id).filter(|id| !id.is_empty());
                    this.state.update(cx, |state, cx| {
                        state.add_code_ref(&key, reference);
                        cx.notify();
                    });
                    this.confirm_code_ref(&key, cx);
                } else {
                    this.code_ref_message("Could not identify the workspace repository.", false, cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn add_selected_code_to_chat(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(key) = self.selected_chat(cx) else { return; };
        let Some(path) = self.editor_path.clone() else {
            self.code_ref_message("Select code to add to chat.", false, cx); return;
        };
        let Some(editor) = self
            .preview
            .documents
            .get(&path)
            .and_then(|document| document.editor.as_ref())
        else {
            self.code_ref_message("Select code to add to chat.", false, cx); return;
        };
        let state = editor.read(cx);
        let value = state.value().to_string();
        let selection = state.selected_range();
        let Some(range) = crate::code_ref::selection_lines(&value, selection) else {
            self.code_ref_message("Select code to add to chat.", false, cx); return;
        };
        let Some(snippet) = crate::code_ref::snippet(&value, range) else {
            self.code_ref_message("Could not read the selection.", false, cx); return;
        };
        let member = self.state.read(cx).selected_execution_member(&key);
        let reference = crate::code_ref::StagedCodeRef {
            id: uuid::Uuid::new_v4().to_string(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation {
                    repo_id: member.as_ref().map(|member| member.repo_id.clone()).unwrap_or_default(),
                    worktree_id: member.and_then(|member| member.checkout_id),
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
        state.update(cx, |state, _| state.selected_chat = Some("other".into()));
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
            assert!(state.code_refs("chat").is_empty());
            let staged = state.code_refs("other");
            assert_eq!(staged.len(), 2);
            assert_ne!(staged[0].id, staged[1].id);
            assert!(staged.iter().all(|item| item.reference == reference.reference && item.snippet == reference.snippet));
        });
    }

    #[gpui::test]
    fn editor_shortcut_stages_selection_on_current_chat(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext as _, Focusable as _};
        use super::super::document::{DocumentKey, FileDocument};
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _guard = runtime.enter();
        let (out, mut requests) = tokio::sync::mpsc::channel::<String>(16);
        let (replies, inbound) = tokio::sync::mpsc::channel::<String>(16);
        let client = mast_rpc::RpcClient::new(out, inbound);
        let responder = runtime.spawn(async move {
            while let Some(frame) = requests.recv().await {
                let request: serde_json::Value = serde_json::from_str(&frame).unwrap();
                if request["method"] == mast_rpc::methods::DESCRIBE_EXECUTION_REPOSITORY {
                    let identity = mast_proto::ExecutionRepositoryIdentity {
                        repo_id: "file-repo".into(), repo_path: "/file-repo".into(),
                        checkout_id: "file-checkout".into(), ..Default::default()
                    };
                    replies.send(serde_json::json!({"id": request["id"], "ok": identity}).to_string()).await.unwrap();
                }
            }
        });
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::dark());
            crate::shell::apply_keymap(cx, &crate::settings::KeymapConfig::default(), Default::default());
        });
        let state = cx.new(|_| crate::state::AppState::new());
        state.update(cx, |state, _| {
            state.local_device_id = Some("local".into());
            state.chats.push(mast_proto::Chat {
                standing_role_successor: None,
                id: "a".into(), device_id: "local".into(), title: None, archived: false,
                cwd: Some("/file-repo".into()), branch: None, checkout_id: Some("file-checkout".into()),
                source_context: None, config: None, last_message_preview: None, last_message_at: None,
                created_at: chrono::Utc::now(), harness_session_id: None, harness_session_cwd: None,
                actual_model: None, parent_chat_id: None, side_of: None, followup_of: None, side_absorbed_through: None, scheduled_task: None, scheduled_task_info: None, space_id: None, last_seen_at: None,
                room_gen: None, linked_pr_url: None, linked_pr_source: None,
                linked_ticket_id: None, linked_ticket_source: None,
            });
            state.selected_chat = Some("a".into());
        });
        let window = cx.add_window(|_, cx| super::super::FilesSurface::new_editor(
            state.clone(), "a".into(), "file.rs".into(), false, 1000, 13.0, false, false, cx,
        ));
        window.update(cx, |surface, window, cx| {
            let text = "first line\nsecond line\n";
            let mut document = FileDocument::loading(DocumentKey {
                chat_id: "a".into(), checkout_id: Some("file-checkout".into()), path: "file.rs".into(),
            });
            document.set_loaded(mast_proto::WorkspaceFileText {
                checkout_id: "file-checkout".into(), path: "file.rs".into(), text: Some(text.into()),
                content_hash: Some("hash".into()), size: text.len() as u64, modified_at: None,
                encoding: mast_proto::WorkspaceTextEncoding::Utf8,
                line_ending: Some(mast_proto::WorkspaceLineEnding::Lf), read_only_reason: None, truncated: false,
            });
            let theme = crate::theme::Theme::of(cx).clone();
            let editor = super::super::editor::new_file_editor(text, "file.rs", false, &theme, window, cx);
            window.focus(&editor.focus_handle(cx), cx);
            let observer = cx.observe(&editor, |surface, editor, cx| {
                surface.observe_code_ref_selection("file.rs", &editor, cx);
            });
            document.editor_observer = Some(observer);
            document.editor = Some(editor);
            surface.preview.active = Some("file.rs".into());
            surface.preview.documents.insert("file.rs".into(), document);
            surface.request_context = Some(super::super::client::FilesRequestContext {
                target: mast_proto::WorkspaceTarget { chat_id: Some("a".into()), space_id: None, checkout_path: None },
                target_device_id: None, cwd: "/file-repo".into(), checkout_id: Some("file-checkout".into()),
            });
        }).unwrap();
        state.update(cx, |state, _| {
            state.selected_chat = Some("b".into());
            state.set_test_engine(crate::state::EngineHandle::from_test_client(client));
        });
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear()).unwrap();
        window.update(cx, |surface, window, cx| {
            let editor = surface.preview.documents["file.rs"].editor.as_ref().unwrap();
            window.focus(&editor.focus_handle(cx), cx);
        }).unwrap();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear()).unwrap();
        let shortcut = crate::settings::platform_combo("mod-shift-l");
        cx.simulate_keystrokes(window.into(), &shortcut);
        window.update(cx, |surface, _, _| assert!(surface.code_ref_status.is_none())).unwrap();
        state.read_with(cx, |state, _| {
            assert!(state.code_refs("b").is_empty());
            assert!(state.selected_execution_member("b").is_none());
        });
        window.update(cx, |surface, _, cx| surface.code_ref_message("Test error", false, cx)).unwrap();
        cx.simulate_keystrokes(window.into(), &crate::settings::platform_combo("mod-a"));
        cx.run_until_parked();
        window.update(cx, |surface, _, _| assert!(surface.code_ref_status.is_none())).unwrap();
        cx.simulate_keystrokes(window.into(), &shortcut);
        for _ in 0..8 {
            runtime.block_on(tokio::task::yield_now());
            cx.run_until_parked();
        }
        state.read_with(cx, |state, _| {
            assert!(state.code_refs("a").is_empty());
            let staged = &state.code_refs("b")[0];
            assert_eq!(staged.reference.location.repo_id, "file-repo");
            assert_eq!(staged.reference.location.worktree_id.as_deref(), Some("file-checkout"));
            assert_eq!(staged.snippet, "first line\nsecond line");
        });
        window.update(cx, |surface, _, _| assert_eq!(surface.code_ref_status.as_deref(), Some("Added to chat"))).unwrap();
        cx.simulate_keystrokes(window.into(), "right");
        cx.run_until_parked();
        window.update(cx, |surface, _, _| assert!(surface.code_ref_status.is_none())).unwrap();
        responder.abort();
    }

    #[gpui::test]
    fn shortcut_without_selection_opens_outline(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext as _, Focusable as _};
        use std::{cell::Cell, rc::Rc};
        struct Host { surface: gpui::Entity<super::super::FilesSurface>, opened: Rc<Cell<bool>> }
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
                gpui::div().size_full().on_action(cx.listener(|this, action: &crate::shell::work_outline_ui::RunWorkOutlineCommand, _, _| {
                    if action.0 == crate::work_outline::WorkOutlineCommand::OpenOutline { this.opened.set(true); }
                })).child(self.surface.clone())
            }
        }
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::dark());
            crate::shell::apply_keymap(cx, &crate::settings::KeymapConfig::default(), Default::default());
        });
        let opened = Rc::new(Cell::new(false));
        let (host, vcx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            let surface = cx.new(|cx| super::super::FilesSurface::new_explorer(state, "a".into(), false, cx));
            Host { surface, opened: opened.clone() }
        });
        vcx.update(|window, cx| {
            let surface = host.read(cx).surface.clone();
            window.draw(cx).clear();
            let focus = surface.read(cx).search.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
            window.draw(cx).clear();
        });
        let diagnostics = vcx.update(|window, cx| {
            let surface = host.read(cx).surface.clone();
            let focused = surface.read(cx).search.read(cx).focus_handle(cx).is_focused(window);
            let key = gpui::Keystroke::parse(&crate::settings::platform_combo("mod-shift-l")).unwrap();
            let bindings = window.possible_bindings_for_input(&[key]).iter().map(|binding| binding.action().name().to_string()).collect::<Vec<_>>();
            (focused, bindings)
        });
        vcx.simulate_keystrokes(&crate::settings::platform_combo("mod-shift-l"));
        assert!(opened.get(), "Open Outline should receive the shortcut: {diagnostics:?}");
    }

    #[gpui::test]
    fn adding_without_selected_chat_reports_error(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(crate::theme::Theme::dark()); });
        let state = cx.new(|_| crate::state::AppState::new());
        let surface = cx.new(|cx| super::super::FilesSurface::new(state.clone(), "chat".into(), false, 1000, 13.0, false, false, cx));
        surface.update(cx, |surface, cx| surface.add_selected_code_to_chat(cx));
        surface.read_with(cx, |surface, _| assert_eq!(surface.code_ref_status.as_deref(), Some("Open a chat to add code to it.")));
        state.read_with(cx, |state, _| assert!(state.code_refs("chat").is_empty()));
        cx.executor().advance_clock(std::time::Duration::from_secs(3));
        cx.run_until_parked();
        surface.read_with(cx, |surface, _| assert!(surface.code_ref_status.is_none()));
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
