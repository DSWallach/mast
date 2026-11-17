//! The persistent navigation host shares a controller with the workspace,
//! but never renders its transcript or composer.
use super::*;

pub(crate) struct OverviewNavigation {
    overview: Entity<Overview>,
    _observation: Subscription,
}

impl OverviewNavigation {
    pub(crate) fn new(overview: Entity<Overview>, cx: &mut Context<Self>) -> Self {
        let observation = cx.observe(&overview, |_, _, cx| cx.notify());
        Self {
            overview,
            _observation: observation,
        }
    }
}

impl Render for OverviewNavigation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.overview
            .update(cx, |overview, cx| overview.render_navigation(window, cx))
    }
}

impl Overview {
    pub(crate) fn set_sidebar_scope(&mut self, scope: Option<String>, cx: &mut Context<Self>) {
        if self.sidebar_scope != scope {
            self.sidebar_scope = scope;
            self.rows_dirty = true;
            self.outline_dirty = true;
            cx.notify();
        }
    }

    pub(crate) fn sidebar_filter_ids(&mut self, cx: &mut Context<Self>) -> Option<HashSet<String>> {
        self.ensure_prefs_loaded(cx);
        let _ = self.rows(cx);
        let query = self.search.read(cx).text().trim().to_lowercase();
        let filtering = !query.is_empty()
            || self.hide_stale
            || self.attention_filter != AttentionFilter::All
            || !self.hidden_repos.is_empty()
            || !self.work_item_filter.is_empty();
        if !filtering {
            return None;
        }
        let state = self.state.read(cx);
        let now = Utc::now();
        Some(
            state
                .chats
                .iter()
                .filter(|chat| {
                    if chat.parent_chat_id.is_some()
                        || chat.is_side_chat()
                        || chat
                            .space_id
                            .as_deref()
                            .is_some_and(|id| state.space_row(id).is_none())
                        || !self
                            .sidebar_scope
                            .as_deref()
                            .is_none_or(|scope| chat.space_id.as_deref() == Some(scope))
                    {
                        return false;
                    }
                    let status = state.display_status_for(chat, now);
                    let space = chat.space_id.as_deref().and_then(|id| state.space_row(id));
                    if (self.hide_stale && is_stale(chat, status, now))
                        || self.hidden_repos.contains(&repo_key_for_chat(chat, space))
                        || !work_item_filter::chat_passes(
                            &self.work_item_filter,
                            &self.work_item_index,
                            &chat.id,
                        )
                    {
                        return false;
                    }
                    match self.attention_filter {
                        AttentionFilter::NeedsInteraction
                            if status != ChatIndicator::AwaitingInput
                                && !self.work_item_index.attention_for_chat(&chat.id).0 =>
                        {
                            return false;
                        }
                        AttentionFilter::Stalled
                            if status != ChatIndicator::Errored
                                && state
                                    .session_for(&chat.id)
                                    .and_then(|s| s.last_turn_tool_failures.as_ref())
                                    .is_none() =>
                        {
                            return false;
                        }
                        _ => {}
                    }
                    let link = self.link_status.get(&chat.id).map(|entry| &entry.status);
                    let branch = crate::change_requests::display_checkout(
                        chat,
                        state.selected_chat_diff_target(&chat.id),
                    )
                    .branch;
                    let mut fields = vec![
                        chat.title.as_deref(),
                        branch,
                        chat.cwd.as_deref(),
                        chat.last_message_preview.as_deref(),
                        chat.linked_pr_url.as_deref(),
                        link.and_then(|link| link.ticket.as_ref())
                            .map(|ticket| ticket.identifier.as_str()),
                    ];
                    let prs = link.map(effective_pr_links).unwrap_or_default();
                    let values: Vec<String> = prs
                        .iter()
                        .flat_map(|pr| {
                            let number = pr.detail.as_ref().map_or_else(
                                || pr_number_from_url(&pr.url),
                                |detail| detail.number,
                            );
                            let mut values =
                                vec![pr.url.clone(), number.to_string(), format!("#{number}")];
                            if let Some(title) =
                                pr.detail.as_ref().and_then(|detail| detail.title.clone())
                            {
                                values.push(title);
                            }
                            values
                        })
                        .collect();
                    fields.extend(values.iter().map(|value| Some(value.as_str())));
                    matches_search(&query, &fields)
                })
                .map(|chat| chat.id.clone())
                .collect(),
        )
    }

    pub(crate) fn clear_sidebar_filters(&mut self, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.set_text("", cx));
        self.hide_stale = false;
        self.attention_filter = AttentionFilter::All;
        self.hidden_repos.clear();
        self.work_item_filter.clear();
        self.rows_dirty = true;
        self.outline_dirty = true;
        self.view_prefs_changed(cx);
        self.save_string_set(
            overview_positions::HIDDEN_REPOS_FILE,
            &self.hidden_repos,
            cx,
        );
        self.save_string_set(
            overview_positions::WORK_ITEM_FILTER_FILE,
            &self.work_item_filter,
            cx,
        );
        cx.notify();
    }

    pub(crate) fn navigation_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.outline_tree_focused(window, cx)
            || self.body_focus.contains_focused(window, cx)
            || gpui::Focusable::focus_handle(self.search.read(cx), cx).contains_focused(window, cx)
    }

    pub(crate) fn navigation_list_focused(&self, window: &Window) -> bool {
        self.view_mode == OverviewMode::List && self.body_focus.is_focused(window)
    }

    pub(crate) fn request_navigation_focus(&mut self, cx: &mut Context<Self>) {
        self.outline_focus_pending = true;
        cx.notify();
    }

    /// Callers may already be updating Shell through a command handler.
    /// Defer opening the destination to avoid re-entering that entity.
    pub(super) fn show_sidebar_workspace(&self, cx: &mut Context<Self>) {
        if self.in_sidebar {
            let shell = self.shell.clone();
            cx.defer(move |cx| {
                shell
                    .update(cx, |shell, cx| shell.show_overview_workspace(cx))
                    .ok();
            });
        }
    }

    pub(super) fn return_from_sidebar_workspace(&self, cx: &mut Context<Self>) {
        if self.in_sidebar
            && self.work_item_details.is_none()
            && self.required_actions_panel.is_none()
            && self.subagent_peek.is_none()
            && !self.column_shown()
            && self.link_editor_chat.is_none()
        {
            let shell = self.shell.clone();
            cx.defer(move |cx| {
                shell
                    .update(cx, |shell, cx| shell.return_from_overview_workspace(cx))
                    .ok();
            });
        }
    }

    pub(super) fn render_sidebar_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.ensure_prefs_loaded(cx);
        let _ = self.rows(cx);
        self.declare_outline_spaces(cx);
        self.sync_outline(cx);
        let theme = Theme::of(cx).clone();
        if self.link_editor_chat.is_some() {
            let chat_id = self.link_editor_chat.clone().unwrap();
            let error = self
                .link_error
                .as_ref()
                .filter(|(id, _)| id == &chat_id)
                .map(|(_, text)| text.clone());
            return div()
                .size_full()
                .p(px(16.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .debug_selector(|| "overview-workspace-link-editor".into())
                .child("Add PR / ticket")
                .child(self.link_input.clone())
                .children(error.map(|message| div().text_color(theme.danger).child(message)))
                .child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(
                            popover::btn_primary(
                                &theme,
                                if self.link_submitting {
                                    "Linking…"
                                } else {
                                    "Add link"
                                },
                            )
                            .id("overview-link-editor-submit")
                            .on_click(cx.listener(|this, _, _, cx| this.submit_link_editor(cx))),
                        )
                        .child(
                            popover::btn_ghost(&theme, "Close", "overview-link-editor-close")
                                .id("overview-link-editor-close")
                                .on_click(cx.listener(|this, _, _, cx| this.close_link_editor(cx))),
                        ),
                )
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    match event.keystroke.key.as_str() {
                        "enter" => this.submit_link_editor(cx),
                        "escape" => this.close_link_editor(cx),
                        _ => return,
                    }
                    cx.stop_propagation();
                }))
                .into_any_element();
        }
        let empty = if self.work_item_details.is_none()
            && self.required_actions_panel.is_none()
            && self.subagent_peek.is_none()
            && !self.chat_panel_open
        {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.text_muted)
                .child("Choose a work item or chat in Overview")
                .into_any_element()
        } else {
            div().flex_none().w(px(0.0)).into_any_element()
        };
        self.render_workspace(empty, window, cx)
    }
}
