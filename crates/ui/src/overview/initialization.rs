use super::*;

impl Overview {

    pub fn new(
        state: Entity<AppState>,
        shell: gpui::WeakEntity<Shell>,
        transcript: Entity<Transcript>,
        composer: Entity<crate::composer::Composer>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&state, |this, state, cx| {
            this.perf.observer_calls += 1;
            // Only a change to something the rows or render read marks them
            // dirty and repaints; streaming transcript frames and the like
            // leave the fingerprint alone.
            let fingerprint = state.read(cx).overview_fingerprint(Utc::now());
            if this.state_fingerprint == Some(fingerprint) {
                return;
            }
            this.state_fingerprint = Some(fingerprint);
            this.perf.observer_notifies += 1;
            this.rows_dirty = true;
            cx.notify();
        });
        let initial_fingerprint = Some(state.read(cx).overview_fingerprint(Utc::now()));
        let search = cx.new(|cx| {
            // Must be `PALETTE_SEARCH_CONTEXT`: editing keys (backspace, ⌥⌫, ⌘⌫,
            // ⌘A/C/V/Z…) are bound only to the contexts `input_bindings`
            // registers; a bespoke context got typing but no deletion.
            ComposerInput::with_context(
                "Search chats…",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_accessibility_role(gpui::Role::SearchInput)
        });
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                this.rows_dirty = true;
                cx.notify();
            }
        });
        let link_input = cx.new(|cx| {
            ComposerInput::with_context(
                "GitHub PR URL or ticket id (ENG-1234)",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
        });
        let link_input_events = cx.subscribe(&link_input, |this: &mut Self, _, event, cx| {
            // A fresh edit clears a stale parse/RPC error for that editor.
            if matches!(event, ComposerInputEvent::Edited)
                && this.link_error.is_some()
                && this.link_error.as_ref().map(|(id, _)| id) == this.link_editor_chat.as_ref()
            {
                this.link_error = None;
                cx.notify();
            }
        });
        let outline = cx.new(|cx| OutlineView::new(state.clone(), cx).with_shell(shell.clone()));
        let outline_events = cx.subscribe(&outline, |this: &mut Self, _, event, cx| match event {
            OutlineEvent::OpenChat(chat_id) => {
                this.outline_preview = None;
                this.leave_work_item_details(DetailsExit::OpenChat(chat_id.clone()), cx);
            }
            OutlineEvent::OpenTracker(chat_id) => {
                this.leave_work_item_details(DetailsExit::OpenTracker(chat_id.clone()), cx);
            }
            OutlineEvent::SelectionMoved => this.schedule_outline_preview(cx),
            OutlineEvent::RenameChat(chat_id) => {
                if let Some(shell) = this.shell.upgrade() {
                    let chat_id = chat_id.clone();
                    shell.update(cx, |shell, cx| shell.open_rename_chat(chat_id, cx));
                }
            }
            OutlineEvent::OpenDetails { space_id, item_id } => {
                this.open_work_item_details(space_id.clone(), item_id.clone(), cx);
            }
            OutlineEvent::OpenSpaceRoot { space_id } => {
                this.open_work_item_details(
                    space_id.clone(),
                    crate::work_outline::details_panel::SPACE_ROOT_ITEM_ID.into(),
                    cx,
                );
            }
            OutlineEvent::StartChat { space_id, item_id } => {
                if let Err(notice) = this.start_chat_for_item(space_id.clone(), item_id.clone(), cx)
                {
                    this.outline
                        .update(cx, |outline, cx| outline.show_error(notice, cx));
                }
            }
            OutlineEvent::OpenProposals { space_id, item_id } => {
                this.open_proposals(space_id.clone(), item_id.clone(), cx);
            }
            OutlineEvent::OpenRequiredActions => this.open_required_actions(cx),
            OutlineEvent::OpenRequiredAction(id) => this.open_required_action(id.clone(), cx),
            OutlineEvent::LinearIssueDropped {
                space_id,
                item_id,
                issue,
            } => {
                this.linear_issue_dropped(space_id.clone(), item_id.clone(), (**issue).clone(), cx)
            }
            OutlineEvent::PullRequestDropped {
                space_id,
                item_id,
                pr,
            } => this.pull_request_dropped(space_id.clone(), item_id.clone(), (**pr).clone(), cx),
            // The toolbar's Hidden Spaces count.
            OutlineEvent::SpacesArranged => cx.notify(),
        });
        // Idle refresh — same `cx.spawn` + `background_executor().timer`
        // loop idiom as the composer's queue-lease renewal. Ends on its own
        // once the entity is dropped (`update` fails).
        let refresh_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(IDLE_REFRESH).await;
                if this.update(cx, |this, cx| this.refresh_tick(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            state,
            shell,
            list: ListState::new(0, ListAlignment::Top, px(400.0)),
            list_items: Vec::new(),
            list_rows: std::rc::Rc::new(Vec::new()),
            body_focus: cx.focus_handle(),
            list_selected: None,
            list_order: Vec::new(),
            view_mode: OverviewMode::Outline,
            expanded: HashSet::new(),
            link_status: HashMap::new(),
            link_checkouts: HashMap::new(),
            link_status_pending: HashSet::new(),
            subagents: HashMap::new(),
            subagent_pending: HashSet::new(),
            hovered_row: None,
            subagents_fetched_at: HashMap::new(),
            row_fetch_due: HashMap::new(),
            context_usage: HashMap::new(),
            context_usage_pending: HashSet::new(),
            context_usage_fetched_at: HashMap::new(),
            acknowledged_done: BTreeSet::new(),
            hidden_repos: BTreeSet::new(),
            known_repos: BTreeSet::new(),
            prefs_loaded: false,
            view_prefs_touched: false,
            repo_filter_needs_validation: false,
            repo_filter: popover::Popup::default(),
            filter_menu: popover::Popup::default(),
            unfiltered_count: 0,
            refresh_ticks: 0,
            _refresh_task: refresh_task,
            active_groups: Vec::new(),
            hide_stale: false,
            show_archived: false,
            focus_due: None,
            rows_dirty: true,
            perf: OverviewPerf::default(),
            state_fingerprint: initial_fingerprint,
            work_item_index_key: None,
            rows_cache: std::rc::Rc::default(),
            search,
            chat_panel_open: false,
            outline_chat_return: None,
            outline_focus_pending: false,
            outline_preview: None,
            copied_panel_identity: None,
            copied_panel_identity_seq: 0,
            transcript,
            composer,
            panel_highlight: None,
            link_editor_chat: None,
            link_input,
            link_submitting: false,
            link_error: None,
            subagent_peek: None,
            peek_request_seq: 0,
            peek_scroll: ScrollHandle::new(),
            peek_focus: cx.focus_handle(),
            outline,
            outline_dirty: true,
            outline_chats: std::sync::Arc::default(),
            outline_show_completed: false,
            outline_fixture: None,
            declared_outline_spaces: None,
            work_item_index: WorkItemIndex::default(),
            work_item_filter: BTreeSet::new(),
            work_item_filter_needs_validation: false,
            work_item_filter_popup: popover::Popup::default(),
            work_item_details: None,
            pending_effective: None,
            pending_proposals: None,
            required_actions_panel: None,
            _required_actions_events: None,
            linear_panel: None,
            _linear_panel_events: None,
            linear_column: Default::default(),
            details_width: Default::default(),
            github_panel: None,
            _github_panel_events: None,
            #[cfg(test)]
            captured_pr_review: None,
            project_issue_picker: None,
            _project_issue_picker_events: None,
            #[cfg(test)]
            captured_start_chat: None,
            _details_events: None,
            _outline_events: outline_events,
            _observe: observe,
            _search_events: search_events,
            _link_input_events: link_input_events,
        }
    }
}
