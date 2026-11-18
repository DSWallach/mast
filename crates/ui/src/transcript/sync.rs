use super::*;

struct SyncSource {
    selected: Option<String>,
    replay: TranscriptReplayState,
}

struct SyncRows {
    new_rows: Vec<Row>,
    entries_empty: bool,
    tail_streaming: bool,
}

struct SyncDiff {
    baseline_changed: bool,
    rows_diff: Option<(Range<usize>, usize)>,
}

impl Transcript {
    pub(super) fn sync(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        let _measurement = SyncMeasurement::start();
        let Some(SyncSource { selected, replay }) = self.sync_source(cx) else {
            return;
        };
        let attached = self.attach_transcript(selected, replay);

        let rows = self.build_sync_rows(cx);
        let SyncDiff { baseline_changed, rows_diff } =
            self.reconcile_replay_and_tools(&rows, attached, replay, cx);
        let SyncRows { new_rows, tail_streaming, .. } = rows;

        // Capture this before the row splice changes the list's measured end.
        // When the user is truly live-following, retaining the end anchor
        // keeps the in-flow working trailer at the same viewport position as
        // transcript lines grow above it. Nothing about the trailer's layout
        // or coordinates changes.
        let live_following =
            should_anchor_live_stream(self.pinned, self.distance_from_bottom(), tail_streaming);
        let was_empty = self.rows.is_empty();
        let old_last = self.rows.len().checked_sub(1);
        match rows_diff {
            None => {
                self.rows = new_rows;
                self.rows_generation = self.rows_generation.wrapping_add(1);
                self.refresh_protected_attachments(cx);
                self.reconcile_own_turn_prompt();
                // Replay readiness is independent of row content: an empty
                // reset (or one identical to optimistic rows) still resolves
                // or retires the pending viewport.
                if self.restore_pending_viewport(replay) {
                    cx.notify();
                }
                self.promote_materialized_queued_turn(attached, cx);
                return;
            }
            Some((old_range, count)) => {
                // Any replaced row's cached flatten results are stale — and
                // because live replies splice only the rows whose content hash
                // changed (the tail), this is O(changed rows) per commit, never
                // O(reply).
                for row in &self.rows[old_range.clone()] {
                    self.render_cache.borrow_mut().invalidate_row(&row.id);
                }
                if old_range.len() == count {
                    // In-place content change, same row count — notably the
                    // live→complete flip, where EVERY row of the streamed
                    // message changes version (streaming bit, tool auto_open,
                    // timestamp bit) with identical ids. `splice` would reset
                    // those items to hint-less Unmeasured (heights read 0
                    // until the next paint) and, when the viewport-top item is
                    // inside the range, clobber the scroll anchor to the range
                    // start — the end-of-turn up/down jump the spring then has
                    // to walk back. `remeasure_items` keeps old sizes as hints
                    // and holds the anchor across the remeasure.
                    self.list.remeasure_items(old_range);
                } else {
                    self.list.splice(old_range, count);
                }
                self.viewport_layout_revision = self.viewport_layout_revision.wrapping_add(1);
            }
        }
        self.rows = new_rows;
        self.rows_generation = self.rows_generation.wrapping_add(1);
        if old_last != self.rows.len().checked_sub(1) {
            if let Some(ix) = old_last.filter(|&ix| ix < self.rows.len()) {
                // Bottom chrome moves to the new tail too.
                self.list.remeasure_items(ix..ix + 1);
            }
            if was_empty && self.own_turn.is_some() && !self.rows.is_empty() {
                // There was no concrete row to materialize at send time.
                // Start the echo at the bottom edge before adding its runway.
                self.list.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: -self.list.viewport_bounds().size.height,
                });
            }
        }
        self.refresh_protected_attachments(cx);
        self.reconcile_own_turn_prompt();
        self.restore_pending_viewport(replay);
        self.promote_materialized_queued_turn(attached, cx);
        if self.land_end_pending && !self.rows.is_empty() {
            // First content for an unpinned override tab: land at the end.
            // `scroll_to_end` is ITEM-anchored (past-the-end offset that the
            // next layout materializes) — a pixel scroll off `max_offset`
            // would land short here, since the freshly-spliced rows are
            // still unmeasured. Short content clamps back to the top under
            // Top alignment, so "end" and "top" coincide there.
            self.land_end_pending = false;
            self.list.scroll_to_end();
        }
        if self.own_turn.is_some() {
            self.own_turn_kick = true;
        }
        if self.pinned {
            if live_following || baseline_changed {
                self.list.scroll_to_end();
                self.spring.reset();
                self.spring_last_tick = None;
                self.spring_settled_at = None;
                self.spring_kick = false;
                self.last_scroll_distance = 0.0;
            } else {
                if motion::reduced_motion(cx) || was_empty {
                    // First fill (chat open) lands at the bottom instantly
                    // (mugen initialScroll:'bottom'); reduced motion snaps.
                    self.list.scroll_to_end();
                } else if self.is_glued() {
                    // A glued offset (`None` / anchored past the end) makes
                    // the upcoming layout hard-snap to the new end — the
                    // per-commit stutter. Materialize a pixel anchor a hair
                    // above the bottom so layout holds position and the
                    // spring glides the growth.
                    self.list.scroll_by(px(-0.75));
                }
                self.spring_kick = true;
            }
        }
        cx.notify();
    }

    fn sync_source(&mut self, cx: &mut Context<Self>) -> Option<SyncSource> {
        if self.retain_on_deselect
            && self.doc_override.is_none()
            && self.state.read(cx).selected_chat.is_none()
            && self.chat_id.is_some()
        {
            // A quick return can reuse this entity before the exit finishes.
            // Its next snapshot is still a replay, not newly arriving tools.
            self.veil_attach_pending = true;
            return None;
        }
        let (selected, replay) = {
            let s = self.state.read(cx);
            match &self.doc_override {
                // Pinned to a subagent doc: `selected` equals `chat_id` by
                // construction, so the attach/reset branch below never fires,
                // and echoes stay empty (nothing is ever sent from here).
                Some(doc_id) => (Some(doc_id.clone()), TranscriptReplayState::Populated),
                None => {
                    let replay = if !s.transcript_replayed {
                        TranscriptReplayState::Pending
                    } else if s.transcript.is_empty() {
                        TranscriptReplayState::Empty
                    } else {
                        TranscriptReplayState::Populated
                    };
                    (s.selected_chat.clone(), replay)
                }
            }
        };

        let source = (
            selected.clone(),
            replay,
            self.state.read(cx).transcript_revision,
        );
        let (identity, code_ref_context) = {
            let state = self.state.read(cx);
            let chat = selected.as_deref().and_then(|id| state.chats.iter().find(|chat| chat.id == id));
            let config = chat.and_then(|chat| chat.config.as_ref());
            let identity = (config.map(|config| config.harness), config.and_then(|config| config.model.clone()).or_else(|| chat.and_then(|chat| chat.actual_model.clone())));
            let context = chat.and_then(|chat| crate::files::client::FilesRequestContext::for_chat_row(state, chat));
            (identity, context)
        };
        if self.code_ref_context != code_ref_context {
            self.code_ref_context = code_ref_context;
            self.code_ref_resolutions.clear();
            self.code_ref_hover = None;
            self.code_ref_focus.clear();
            self.code_ref_focus_pending = None;
            self.code_ref_generation = self.code_ref_generation.wrapping_add(1);
            cx.notify();
        }
        if self.last_identity.as_ref() != Some(&identity) {
            self.last_identity = Some(identity);
            self.render_cache.borrow_mut().clear();
            self.list.remeasure_items(0..self.rows.len());
            cx.notify();
        }
        if self.last_source.as_ref() == Some(&source) {
            return None;
        }
        self.last_source = Some(source);

        Some(SyncSource { selected, replay })
    }

    fn attach_transcript(&mut self, selected: Option<String>, replay: TranscriptReplayState) -> bool {
        let attached = selected != self.chat_id;
        // Arm the replay baseline before classifying tool arrivals. Selection
        // and replay may arrive in one sync; a retained same-chat entity can
        // also see a fresh pending subscription without changing chat_id.
        if attached || replay == TranscriptReplayState::Pending {
            self.veil_baseline.clear();
            self.veil_attach_pending = true;
        }
        if attached {
            // Read the incoming snapshot before inserting the outgoing one:
            // a full bounded cache may evict its oldest entry, which can be
            // exactly the chat the user is reopening.
            let saved_viewport = selected
                .as_ref()
                .and_then(|chat_id| self.saved_viewports.get_cloned_and_touch(chat_id));
            self.remember_current_viewport();
            let keep_own_turn = self
                .own_turn
                .as_ref()
                .is_some_and(|anchor| selected.as_deref() == Some(anchor.chat_id.as_str()));
            if !keep_own_turn {
                self.own_turn = None;
                self.own_turn_kick = false;
                self.own_turn_last_tick = None;
            }
            self.chat_id = selected;
            self.code_ref_resolutions.clear();
            self.code_ref_hover = None;
            self.code_ref_focus.clear();
            self.code_ref_focus_pending = None;
            self.code_ref_generation = self.code_ref_generation.wrapping_add(1);
            self.rows.clear();
            self.rows_generation = self.rows_generation.wrapping_add(1);
            self.row_cache.clear();
            self.live_parsers.clear();
            self.tree_cache.clear();
            self.folds.clear();
            self.tool_group_reveals.clear();
            self.last_replay_baseline = None;
            self.historical_markdown.clear();
            self.user_folds.clear();
            self.user_heights.clear();
            self.user_hold_token = self.user_hold_token.wrapping_add(1);
            self.user_hold_task = None;
            self.user_collapse_scroll = None;
            self.veils.clear();
            self.render_cache.borrow_mut().clear();
            self.highlights.entries.clear();
            self.copied_message = None;
            self.copied_message_clear = None;
            self.list.reset(0);
            self.pending_viewport = None;
            self.viewport_generation = self.viewport_generation.wrapping_add(1);
            self.viewport_finalize_pending = false;
            if self.own_turn.is_some() {
                // A kept own-turn hold (send-created chat) owns the viewport.
                self.pinned = false;
                self.last_scroll_distance = 0.0;
                self.show_jump_button = false;
            } else if let Some(SavedViewport::Anchored {
                anchor,
                distance_from_bottom,
                own_turn,
            }) = saved_viewport
            {
                // Keep a possible runway pending until replay confirms that
                // its optimistic prompt still exists. Installing it on this
                // empty attach frame can leave a failed send's stale anchor
                // intercepting scroll-to-bottom forever.
                self.pinned = false;
                self.last_scroll_distance = distance_from_bottom;
                self.show_jump_button = distance_from_bottom > SCROLL_BUTTON_THRESHOLD_PX;
                self.pending_viewport = Some(SavedViewport::Anchored {
                    anchor,
                    distance_from_bottom,
                    own_turn,
                });
            } else {
                // New chats and chats that were following their tail retain
                // the existing open-at-bottom behavior.
                self.pinned = true;
                self.last_scroll_distance = 0.0;
                self.show_jump_button = false;
            }
            self.spring.reset();
            self.spring_last_tick = None;
            self.spring_settled_at = None;
            self.spring_kick = false;
            self.scroll_anim = None;
            self.stop_selection_scroll();
        }

        attached
    }
    fn build_sync_rows(&mut self, cx: &mut Context<Self>) -> SyncRows {
        let mut new_rows: Vec<Row> = Vec::new();
        // Borrow the transcript only while deriving rows. Cloning the entity
        // handle lets rows_for mutate our caches without copying every text
        // and tool payload on each app-state notification.
        let (entries_empty, tail_streaming) = {
            let state = self.state.clone();
            let state = state.read(cx);
            let entries = match &self.doc_override {
                Some(doc_id) => state.sub_transcript(doc_id),
                None => state.transcript.as_slice(),
            };
            let prepared = self
                .chat_id
                .as_ref()
                .and_then(|id| state.prepared_transcripts.get(id));
            for entry in entries {
                if let Some(rows) = prepared.and_then(|p| p.rows.get(&entry.id)) {
                    #[cfg(test)]
                    SYNC_ROW_COUNTS.with(|counts| { let (visited, cloned) = counts.get(); counts.set((visited + rows.len(), cloned + rows.len())); });
                    new_rows.extend(rows.iter().cloned());
                } else {
                    new_rows.extend(self.rows_for(entry, false));
                }
            }
            if self.doc_override.is_none() {
                for echo in state.pending_echoes() {
                    new_rows.extend(self.rows_for(echo, true));
                }
            }
            (
                entries.is_empty(),
                entries
                    .last()
                    .is_some_and(|e| e.status == Some(MessageStatus::Streaming)),
            )
        };

        SyncRows { new_rows, entries_empty, tail_streaming }
    }
    fn reconcile_replay_and_tools(
        &mut self,
        rows: &SyncRows,
        attached: bool,
        replay: TranscriptReplayState,
        cx: &mut Context<Self>,
    ) -> SyncDiff {
        let new_rows = &rows.new_rows;
        let entries_empty = rows.entries_empty;
        let baseline = self
            .chat_id
            .as_deref()
            .and_then(|id| self.state.read(cx).transcript_baseline(id))
            .cloned();
        let baseline_changed = baseline.as_ref().is_some_and(|baseline| {
            self.last_replay_baseline
                .as_ref()
                .is_none_or(|previous| !Arc::ptr_eq(previous, baseline))
        });
        let mut historical_tools: HashMap<SharedString, HashSet<String>> = HashMap::new();
        let mut fully_historical: HashSet<SharedString> = HashSet::new();
        if baseline_changed {
            let baseline = baseline.as_ref().unwrap();
            let state = self.state.read(cx);
            let entries = match &self.doc_override {
                Some(id) => state.sub_transcript(id),
                None => &state.transcript,
            };
            let previous_markdown = std::mem::take(&mut self.historical_markdown);
            let prepared = self
                .chat_id
                .as_ref()
                .and_then(|id| state.prepared_transcripts.get(id));
            let mut historical_rows = Vec::new();
            for entry in entries {
                let covered = prepared.map_or_else(
                    || baseline.covers(entry),
                    |p| {
                        Arc::ptr_eq(baseline, &p.navigation_baseline)
                            || p.fully_historical.contains(&entry.id)
                    },
                );
                if covered {
                    fully_historical.insert(entry.id.clone().into());
                    continue;
                }
                if let Some(rows) = self
                    .chat_id
                    .as_ref()
                    .and_then(|id| state.prepared_transcripts.get(id))
                    .and_then(|p| p.historical.get(&entry.id))
                {
                    historical_rows.extend(rows.iter().cloned());
                    continue;
                }
                let Some(historical) = baseline.historical_entry(entry) else {
                    continue;
                };
                historical_rows.extend(rows_for_entry(&historical, false, &mut |_, text| {
                    Arc::new(parse_full(text))
                }));
            }
            // A normal opening snapshot is entirely historical. Share its
            // parsed trees instead of parsing the whole transcript twice;
            // only an entry mixing historical and live parts needs a prefix.
            historical_rows.extend(
                new_rows
                    .iter()
                    .filter(|row| {
                        fully_historical.contains(&row.entry_id)
                            && matches!(row.kind, RowKind::LiveMarkdown { .. })
                    })
                    .cloned(),
            );
            for row in historical_rows {
                match &row.kind {
                    RowKind::ToolGroup { tools, .. }
                        if !fully_historical.contains(&row.entry_id) =>
                    {
                        historical_tools
                            .entry(row.entry_id.clone())
                            .or_default()
                            .extend(tools.iter().map(|tool| tool.part_id.clone()));
                    }
                    RowKind::LiveMarkdown { .. } => {
                        if previous_markdown
                            .get(&row.id)
                            .is_none_or(|old| old.version != row.version)
                        {
                            self.veils.remove(&row.id);
                        }
                        self.historical_markdown.insert(row.id.clone(), row);
                    }
                    _ => {}
                }
            }
            self.last_replay_baseline = Some(baseline.clone());
        }

        // Give only rows that ARRIVE after the replay baseline an entrance.
        // The first populated frame after a chat attach may already contain a
        // live tool group; treating it as history prevents a whole existing
        // task tree from reanimating on every chat switch.
        let replay_baseline = self.veil_attach_pending && !entries_empty;
        if replay_baseline {
            // Retain explicit user pins, but never resume an old arrival or
            // closing animation when revisiting the retained transcript.
            self.tool_group_reveals.clear();
            for fold in self.folds.values_mut() {
                fold.toggled_at = None;
                fold.disclosure_at = None;
            }
        }
        // Tool-group reveal bookkeeping only changes for rows inside the diff
        // when nothing resets the whole transcript: an unchanged (id, version)
        // row would recompute the same `starts` from its own previous value.
        // Computed once: the same diff drives this scan, the prune and the
        // list splice.
        let rows_diff = diff_rows(&self.rows, new_rows);
        let incremental = !attached && !baseline_changed && !replay_baseline;
        let (old_scan, new_scan) = match (&rows_diff, incremental) {
            (Some((old_range, count)), true) => (
                old_range.clone(),
                old_range.start..old_range.start + count,
            ),
            (None, true) => (0..0, 0..0),
            _ => (0..self.rows.len(), 0..new_rows.len()),
        };
        let previous_tools: HashMap<SharedString, HashMap<String, Option<Instant>>> = self.rows
            [old_scan.clone()]
            .iter()
            .inspect(|_| sync_scan_visit())
            .filter(|row| !fully_historical.contains(&row.entry_id))
            .filter_map(|row| match &row.kind {
                RowKind::ToolGroup { tools, .. } => {
                    let reveal = self.tool_group_reveals.get(&row.id);
                    Some((
                        row.id.clone(),
                        tools
                            .iter()
                            .enumerate()
                            .map(|(ix, tool)| {
                                (
                                    tool.part_id.clone(),
                                    reveal.and_then(|r| r.starts.get(ix).copied().flatten()),
                                )
                            })
                            .collect(),
                    ))
                }
                _ => None,
            })
            .collect();
        let now = Instant::now();
        let mut live_tool_groups = HashSet::new();
        for row in &new_rows[new_scan.clone()] {
            sync_scan_visit();
            let RowKind::ToolGroup { tools, .. } = &row.kind else {
                continue;
            };
            // Agent/spawn groups are standalone cards, not task trees.
            if !tool_group_collapses(tools) {
                continue;
            }
            live_tool_groups.insert(row.id.clone());
            let historical = historical_tools.get(&row.entry_id);
            let whole_group_historical =
                fully_historical.contains(&row.entry_id) || (replay_baseline && baseline.is_none());
            let is_historical = |tool: &ToolItem| {
                whole_group_historical || historical.is_some_and(|ids| ids.contains(&tool.part_id))
            };
            let historical_count = if whole_group_historical {
                tools.len()
            } else {
                tools.iter().filter(|tool| is_historical(tool)).count()
            };
            let previous = previous_tools.get(&row.id);
            let is_new_group = historical_count == 0 && previous.is_none();
            let reveal = self.tool_group_reveals.entry(row.id.clone()).or_default();
            if baseline_changed && historical_count > 0 {
                reveal.rendered_open = None;
                if let Some(fold) = self.folds.get_mut(&row.id) {
                    fold.toggled_at = None;
                    fold.disclosure_at = None;
                }
            }
            reveal.shimmer_started_at.get_or_insert(now);
            if is_new_group {
                reveal.header_started_at.get_or_insert(now);
            }
            if baseline_changed && historical_count == tools.len() {
                reveal.header_started_at = None;
            }
            let first_row_delay = if is_new_group { TOOL_FIRST_ROW_DELAY_MS } else { 0 };
            let mut arrival_ix = 0;
            if whole_group_historical {
                reveal.starts.clear();
                reveal.starts.resize(tools.len(), None);
                continue;
            }
            reveal.starts = tools
                .iter()
                .map(|tool| {
                    if is_historical(tool) {
                        return None;
                    }
                    if let Some(start) = previous.and_then(|tools| tools.get(&tool.part_id)) {
                        return *start;
                    }
                    let start = now
                        + Duration::from_millis(first_row_delay + arrival_ix * TOOL_ROW_STAGGER_MS);
                    arrival_ix += 1;
                    Some(start)
                })
                .collect();
        }
        if incremental {
            // Unchanged rows keep their reveal; only groups that left the diff
            // range (removed or no longer collapsing) are dropped.
            for row in &self.rows[old_scan.clone()] {
                if matches!(row.kind, RowKind::ToolGroup { .. })
                    && !live_tool_groups.contains(&row.id)
                {
                    self.tool_group_reveals.remove(&row.id);
                }
            }
        } else {
            self.tool_group_reveals
                .retain(|row_id, _| live_tool_groups.contains(row_id));
        }

        // Runtime scroll handles follow the stable code rows exactly. A live
        // block keeps its handle through completion; deleted/reindexed tail
        // blocks and the previous chat cannot accumulate stale handles.
        self.prune_code_fences(attached, &rows_diff, new_rows);

        // Text already streamed before this (re)attach is the veil BASELINE:
        // its rows' veils seed instead of fading (render creates them from
        // this set), so only post-switch appends animate. Captured from the
        // first NON-EMPTY transcript after attach — the replay frame — never
        // the attach-time sync, whose transcript is still empty (selection
        // clears it; the doc watch refills it async).
        if self.veil_attach_pending
            && (!entries_empty || replay.authoritative_empty() || baseline_changed)
        {
            self.veil_attach_pending = false;
            self.veil_baseline = new_rows
                .iter()
                .filter(|r| baseline.is_none() && matches!(r.kind, RowKind::LiveMarkdown { .. }))
                .map(|r| r.id.clone())
                .collect();
        }

        // Veils live exactly as long as their live row — drop them on the
        // live→complete flip (any mid-fade chunk snaps to full, matching the
        // row's version splice).
        let active_markdown: HashSet<&SharedString> = new_rows
            .iter()
            .filter(|row| matches!(row.kind, RowKind::LiveMarkdown { .. }))
            .map(|row| &row.id)
            .collect();
        self.veils.retain(|id, _| active_markdown.contains(id));
        self.veil_baseline.retain(|id| active_markdown.contains(id));
        self.historical_markdown
            .retain(|id, _| active_markdown.contains(id));

        SyncDiff { baseline_changed, rows_diff }
    }

}
