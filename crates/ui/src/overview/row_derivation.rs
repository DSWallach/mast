use super::*;

impl Overview {
    /// The current rows AND, as a side effect, kicks off any missing/stale
    /// PR-ticket-link and subagent lookups for what's visible right now.
    /// Combined into one pass rather than a separate "ensure" sweep so a
    /// freshly-appeared chat (new import, new sidebar row) gets its lookups
    /// queued the same render it first appears in, not one frame later.
    pub(super) fn rows(&mut self, cx: &mut Context<Self>) -> std::rc::Rc<Vec<OverviewRow>> {
        // See `rows_dirty`'s doc comment: hover and scroll call `cx.notify()`
        // (hence `render()`, hence this) on every pointer-move, and without
        // this short-circuit every one of those frames re-walked `AppState`,
        // cloned every `Chat`, and re-derived staleness for all 227+ rows. A stale
        // cache is bounded by whatever next marks `rows_dirty` (any real
        // `AppState` change already does, via the `cx.observe` in `new`, so
        // this isn't "never refreshes" — it's "doesn't redundantly rebuild
        // on frames where nothing relevant changed").
        if !self.rows_dirty {
            return std::rc::Rc::clone(&self.rows_cache);
        }

        self.perf.rows_rebuilds += 1;
        let now = Utc::now();
        // Work Outline facts for path chips, attention, and the work-item
        // filter.
        let outline_spaces = self.outline_spaces(cx);
        let snapshots: Vec<std::sync::Arc<WorkOutlineSnapshot>> = outline_spaces
            .iter()
            .filter_map(|space| space.load.snapshot().cloned())
            .collect();
        let index_is_current = self.work_item_index_key.as_ref().is_some_and(|key| {
            key.len() == snapshots.len()
                && key
                    .iter()
                    .zip(&snapshots)
                    .all(|(built_from, now)| std::sync::Arc::ptr_eq(built_from, now))
        });
        let work_item_index = if index_is_current {
            std::mem::take(&mut self.work_item_index)
        } else {
            WorkItemIndex::build(snapshots.iter().map(|s| s.as_ref()))
        };
        // Holding the Arcs keeps their addresses from being reused, so
        // `ptr_eq` can't match a different snapshot.
        self.work_item_index_key = Some(snapshots.clone());
        let outlines_loaded = outline_spaces
            .iter()
            .all(|space| !matches!(space.load, WorkOutlineLoad::Loading));
        let (raw, spaces): (
            Vec<(ChatIndicator, Chat, Option<mast_proto::ToolFailures>)>,
            HashMap<String, Space>,
        ) = {
            let state = self.state.read(cx);
            (
                state
                    .overview_chats_with_archived(now, self.show_archived)
                    .into_iter()
                    .filter(|(_, chat)| {
                        self.sidebar_scope
                            .as_deref()
                            .is_none_or(|scope| chat.space_id.as_deref() == Some(scope))
                    })
                    .map(|(status, chat)| {
                        let failures = state
                            .session_for(&chat.id)
                            .and_then(|s| s.last_turn_tool_failures.clone());
                        (status, chat.clone(), failures)
                    })
                    .collect(),
                state
                    .spaces
                    .iter()
                    .map(|space| (space.id.clone(), space.clone()))
                    .collect(),
            )
        };
        self.unfiltered_count = raw.len();
        let hide_stale = self.hide_stale;
        let query = self.search.read(cx).text().trim().to_lowercase();
        let mut known_repos: BTreeSet<String> = BTreeSet::new();
        let mut acknowledged_changed = false;
        let mut all: Vec<OverviewRow> = Vec::with_capacity(raw.len());
        for (status, chat, tool_failures) in raw {
            let stale = is_stale(&chat, status, now);
            self.ensure_row_fetches(chat.id.clone(), chat.harness_session_id.is_some(), cx);
            let link = self.link_status.get(&chat.id).map(|e| &e.status);
            let ticket = link
                .and_then(|s| s.ticket.as_ref())
                .map(|t| t.identifier.clone());
            let prs = link.map(effective_pr_links).unwrap_or_default();
            let pr_number = prs.first().map(|link| {
                link.detail
                    .as_ref()
                    .map_or_else(|| pr_number_from_url(&link.url), |pr| pr.number)
            });
            let pr_title = prs
                .first()
                .and_then(|link| link.detail.as_ref()?.title.clone());
            let pr_search_values = prs
                .iter()
                .flat_map(|link| {
                    let number = link
                        .detail
                        .as_ref()
                        .map_or_else(|| pr_number_from_url(&link.url), |pr| pr.number);
                    let mut values =
                        vec![number.to_string(), format!("#{number}"), link.url.clone()];
                    if let Some(title) = link.detail.as_ref().and_then(|pr| pr.title.clone()) {
                        values.push(title);
                    }
                    values
                })
                .collect();
            let subagent_count = self.subagents.get(&chat.id).map_or(0, Vec::len);
            let context_pct = self.context_usage.get(&chat.id).copied().flatten();
            acknowledged_changed |= reconcile_acknowledged(
                &mut self.acknowledged_done,
                &chat.id,
                status == ChatIndicator::Completed,
            );
            let space = chat
                .space_id
                .as_deref()
                .and_then(|space_id| spaces.get(space_id));
            let repo = repo_key_for_chat(&chat, space);
            let closeable = crate::chat_closeout::is_closeout_candidate(chat.cwd.as_deref());
            known_repos.insert(repo.clone());
            let work_item_path = work_item_index
                .item_for_chat(&chat.id)
                .and_then(|item| work_item_index.path_label(item));
            let (user_action_required, user_action_count) =
                work_item_index.attention_for_chat(&chat.id);
            let instruction_drift = work_item_index.instruction_drift_for_chat(&chat.id);
            let execution_exception = work_item_index.execution_exception_for_chat(&chat.id);
            all.push(OverviewRow {
                status,
                chat,
                stale,
                ticket,
                repo,
                pr_number,
                pr_title,
                pr_search_values,
                context_pct,
                subagent_count,
                closeable,
                work_item_path,
                user_action_required,
                user_action_count,
                instruction_drift,
                execution_exception,
                tool_failures,
            });
        }
        self.known_repos = known_repos;
        if self.repo_filter_needs_validation && !self.known_repos.is_empty() {
            self.repo_filter_needs_validation = false;
            if restored_repo_filter_hides_everything(&self.hidden_repos, &self.known_repos) {
                tracing::info!("restored repo filter hid every chat; resetting it");
                self.hidden_repos.clear();
                self.save_string_set(
                    overview_positions::HIDDEN_REPOS_FILE,
                    &self.hidden_repos,
                    cx,
                );
            }
        }
        if self.work_item_filter_needs_validation && outlines_loaded && !all.is_empty() {
            self.work_item_filter_needs_validation = false;
            if work_item_filter::restored_filter_hides_everything(
                &self.work_item_filter,
                &work_item_index,
                all.iter().map(|row| row.chat.id.as_str()),
            ) {
                tracing::info!("restored work-item filter hid every chat; resetting it");
                self.work_item_filter.clear();
                self.save_string_set(
                    overview_positions::WORK_ITEM_FILTER_FILE,
                    &self.work_item_filter,
                    cx,
                );
            }
        }
        self.outline_chats = std::sync::Arc::new(self.build_outline_chats(&all, now, cx));
        self.outline_dirty = true;
        if acknowledged_changed {
            self.save_string_set(
                overview_positions::ACKNOWLEDGED_DONE_FILE,
                &self.acknowledged_done,
                cx,
            );
        }
        let hidden_repos = &self.hidden_repos;
        let work_filter = &self.work_item_filter;
        let rows: Vec<OverviewRow> = all
            .into_iter()
            .filter(|row| !(hide_stale && row.stale))
            .filter(|row| !hidden_repos.contains(&row.repo))
            .filter(|row| {
                work_item_filter::chat_passes(work_filter, &work_item_index, &row.chat.id)
            })
            .filter(|row| {
                // Web `sessionMatchesSearch` fields that exist here: name
                // (title), branch, last message (preview), cwd, ticket id,
                // PR title. Mast has no separate `summary` field.
                let state = self.state.read(cx);
                let branch = crate::change_requests::display_checkout(
                    &row.chat,
                    state.selected_chat_diff_target(&row.chat.id),
                )
                .branch;
                let mut fields = vec![
                    row.chat.title.as_deref(),
                    branch,
                    row.chat.last_message_preview.as_deref(),
                    row.chat.cwd.as_deref(),
                    row.ticket.as_deref(),
                    row.pr_title.as_deref(),
                ];
                fields.extend(row.pr_search_values.iter().map(String::as_str).map(Some));
                matches_search(&query, &fields)
            })
            .collect();
        self.needs_interaction_count = rows.iter().filter(|r| r.needs_interaction()).count();
        self.stalled_count = rows.iter().filter(|r| r.is_stalled()).count();
        let rows: Vec<OverviewRow> = match self.attention_filter {
            AttentionFilter::All => rows,
            AttentionFilter::NeedsInteraction => {
                rows.into_iter().filter(|r| r.needs_interaction()).collect()
            }
            AttentionFilter::Stalled => rows.into_iter().filter(|r| r.is_stalled()).collect(),
        };
        self.work_item_index = work_item_index;
        let rows = std::rc::Rc::new(rows);
        self.rows_cache = std::rc::Rc::clone(&rows);
        self.rows_dirty = false;
        rows
    }
    /// Every Chat as the Outline sees it: the Overview's rows (pre-filter)
    /// plus child Chats and hidden (archived, stale, repo-filtered) Chats,
    /// with those filters folded into `visible` so an associated Chat that
    /// is merely filtered is never mistaken for a missing one.
    pub(super) fn build_outline_chats(
        &self,
        rows: &[OverviewRow],
        now: chrono::DateTime<Utc>,
        cx: &gpui::App,
    ) -> Vec<OutlineChat> {
        let by_id: HashMap<&str, &OverviewRow> =
            rows.iter().map(|row| (row.chat.id.as_str(), row)).collect();
        let state = self.state.read(cx);
        let hidden_runs = mast_proto::hidden_task_chat_ids(&state.chats);
        state
            .chats
            .iter()
            .filter(|chat| {
                chat.space_id
                    .as_deref()
                    .is_none_or(|space_id| state.space_row(space_id).is_some())
            })
            .map(|chat| {
                let row = by_id.get(chat.id.as_str());
                let status = row.map_or_else(|| state.display_status_for(chat, now), |r| r.status);
                let stale = row.map_or_else(|| is_stale(chat, status, now), |r| r.stale);
                let shown_by_toggles =
                    (self.show_archived || !chat.archived) && !(self.hide_stale && stale);
                let tool_failures = row.and_then(|r| r.tool_failures.clone()).or_else(|| {
                    state
                        .session_for(&chat.id)
                        .and_then(|s| s.last_turn_tool_failures.clone())
                });
                let user_action_required = row.is_some_and(|r| r.user_action_required);
                // Keep side chats in the lookup so their durable work-item
                // assignments do not look dangling, but never show a row.
                let visible = !chat.is_side_chat()
                    && shown_by_toggles
                    && !hidden_runs.contains(chat.id.as_str())
                    && self
                        .sidebar_scope
                        .as_deref()
                        .is_none_or(|scope| chat.space_id.as_deref() == Some(scope))
                    && {
                        let repo = match row {
                            Some(row) => row.repo.clone(),
                            None => repo_key_for_chat(
                                chat,
                                chat.space_id.as_deref().and_then(|id| state.space_row(id)),
                            ),
                        };
                        !self.hidden_repos.contains(&repo)
                    }
                    && match self.attention_filter {
                        AttentionFilter::All => true,
                        AttentionFilter::NeedsInteraction => {
                            status == ChatIndicator::AwaitingInput || user_action_required
                        }
                        AttentionFilter::Stalled => status == ChatIndicator::Errored,
                    };
                let pr_number = row.and_then(|r| r.pr_number).or_else(|| {
                    self.link_status.get(&chat.id).and_then(|entry| {
                        effective_pr_links(&entry.status).first().map(|link| {
                            link.detail
                                .as_ref()
                                .map_or_else(|| pr_number_from_url(&link.url), |pr| pr.number)
                        })
                    })
                });
                OutlineChat {
                    id: chat.id.clone(),
                    space_id: chat.space_id.clone(),
                    parent_chat_id: chat.parent_chat_id.clone(),
                    title: chat
                        .title
                        .clone()
                        .unwrap_or_else(|| "New session".to_string()),
                    status,
                    archived: chat.archived,
                    stale,
                    branch: chat.branch.clone(),
                    harness: Some(chat_model_label(chat)),
                    pr_number,
                    visible,
                    activity: chat
                        .last_message_at
                        .unwrap_or(chat.created_at)
                        .timestamp_millis(),
                    tool_failures,
                }
            })
            .collect()
    }
    /// Every Space the Outline shows (all of them: the Overview is the
    /// All-spaces view), in display order, with its current load.
    pub(super) fn outline_spaces(&self, cx: &gpui::App) -> Vec<OutlineSpace> {
        self.outline_spaces_in_scope(self.sidebar_scope.as_deref(), cx)
    }
    pub(super) fn outline_snapshot(
        &self,
        space_id: &str,
        cx: &gpui::App,
    ) -> Option<std::sync::Arc<WorkOutlineSnapshot>> {
        match &self.outline_fixture {
            Some(map) => map.get(space_id).and_then(|load| load.snapshot().cloned()),
            None => self
                .state
                .read(cx)
                .work_outline(space_id)
                .snapshot()
                .cloned(),
        }
    }
}
