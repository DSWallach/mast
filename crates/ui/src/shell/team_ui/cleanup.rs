//! The Tracker's Cleanup section: a heading below the work queue and, when it
//! is open, one row per entry (`crate::tracker_cleanup` builds the rows). Each
//! row says why the entry needs cleanup and carries the button for its fix.
use super::queue::TrackerQueue;
use super::*;
use crate::components::shell_parts::{tracker_action, tracker_group_heading, tracker_metadata, worker_title};
use crate::tracker_cleanup::{CleanAllPlan, CleanupContext, CleanupFix, CleanupKind, CleanupOutcome, CleanupRow, clean_all_plan, cleanup_rows};

/// What the item picker below the rows is choosing a task for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CleanupPick {
    LinkPr(String),
    MoveChat(String),
}

impl Shell {
    /// `None` when there is nothing to clean up and the section is closed.
    pub(super) fn render_tracker_cleanup_section(&self, chat_id: &str, queue: Option<&TrackerQueue>, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let count = self.tracker.health_entries.len() + self.tracker.local_cleanup.len();
        let expanded = self.tracker.health_expanded.contains(chat_id);
        if count == 0 && !expanded && self.tracker.cleanup_result.is_none() && !self.tracker.cleanup_busy { return None; }
        let heading = div().id("tracker-cleanup-heading").debug_selector(|| "tracker-cleanup-heading".into()).cursor_pointer()
            .child(tracker_group_heading(theme, if expanded { "▾ Cleanup" } else { "▸ Cleanup" }, count))
            .on_click(cx.listener(|this, _, _, cx| {
                let id = this.active_chat.clone();
                if !this.tracker.health_expanded.remove(&id) { this.tracker.health_expanded.insert(id); }
                cx.notify();
            }));
        let (rows, plan) = self.cleanup_rows_and_plan(queue);
        let confirm = self.tracker.clean_all_confirm.as_ref().filter(|_| self.tracker.clean_all_chat == chat_id);
        let mut top = div().flex().flex_wrap().items_center().gap(px(Theme::SPACE_SM)).child(heading);
        if self.tracker.cleanup_busy {
            top = top.child(tracker_metadata(theme, "Cleaning up…".into()));
        } else if !plan.is_empty() && confirm.is_none() {
            top = top.child(tracker_action(theme, "tracker-cleanup-all".into(), "Clean all", false).debug_selector(|| "tracker-cleanup-all".into())
                .on_click(cx.listener(|this, _, _, cx| this.start_clean_all(cx))));
        }
        let mut section = div().flex().flex_col().gap(px(Theme::SPACE_SM)).child(top);
        if let Some(confirm) = confirm {
            section = section.child(self.render_clean_all_confirm(confirm, theme, cx));
        }
        if let Some(result) = self.tracker.cleanup_result.as_ref().filter(|_| self.tracker.clean_all_chat == chat_id) {
            section = section.child(div().debug_selector(|| "tracker-cleanup-result".into()).child(tracker_metadata(theme, result.clone())));
        }
        if expanded { section = section.child(self.render_tracker_health(&rows, theme, cx)); }
        Some(section.into_any_element())
    }

    /// The rows shown now and what Clean all would do with them.
    pub(super) fn cleanup_rows_and_plan(&self, queue: Option<&TrackerQueue>) -> (Vec<CleanupRow>, CleanAllPlan) {
        let chat_title = |id: &str| self.tracker.metadata.get(id).map(|value| value.0.clone()).unwrap_or_else(|| "Chat".into());
        let item_title = |id: &str| self.tracker.health_titles.get(id).cloned().unwrap_or_else(|| "Work item".into());
        let ctx = CleanupContext {
            chat_facts: &self.tracker.health_chats, chat_title: &chat_title, item_title: &item_title,
            completed_items: &self.tracker.health_completed, issue_urls: &self.tracker.health_issue_urls,
            active_chat: &self.active_chat,
            on_queue: &|chat_id| queue.is_some_and(|queue| queue.queued_item_of_chat(chat_id).is_some()),
        };
        let rows = cleanup_rows(&self.tracker.health_entries, &self.tracker.local_cleanup, &ctx);
        let plan = clean_all_plan(&rows, &ctx);
        (rows, plan)
    }

    fn start_clean_all(&mut self, cx: &mut Context<Self>) {
        if self.tracker.cleanup_busy { return; }
        let queue = self.tracker_queue_for(&self.active_chat, cx);
        let (_, plan) = self.cleanup_rows_and_plan(queue.as_ref());
        self.tracker.cleanup_result = None;
        self.tracker.clean_all_chat = self.active_chat.clone();
        self.tracker.clean_all_confirm = (!plan.is_empty()).then_some(plan);
        cx.notify();
    }

    fn render_clean_all_confirm(&self, plan: &CleanAllPlan, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let note = |text: String| div().text_size(crate::typography::ui_rems(10.5)).text_color(theme.text_muted).child(SharedString::from(text));
        let mut panel = div().debug_selector(|| "tracker-cleanup-confirm".into()).flex().flex_col().gap(px(Theme::SPACE_XS))
            .child(div().flex().child(worker_title(theme, "Clean all".into())))
            .child(note(format!("{}.", plan.summary())))
            .child(note("Chats that are running, waiting or have an open Required Action are never archived.".into()));
        if !plan.skipped.is_empty() {
            panel = panel.child(tracker_metadata(theme, "Left alone:".into()));
            for line in &plan.skipped {
                panel = panel.child(div().pl(px(Theme::SPACE_MD)).child(tracker_metadata(theme, line.clone())));
            }
        }
        let confirm = plan.clone();
        panel.child(div().flex().items_center().gap(px(Theme::SPACE_SM))
            .child(tracker_action(theme, "tracker-cleanup-confirm-apply".into(), "Clean all", true).debug_selector(|| "tracker-cleanup-confirm-apply".into())
                .on_click(cx.listener(move |this, _, _, cx| this.apply_cleanup(confirm.clone(), cx))))
            .child(tracker_action(theme, "tracker-cleanup-cancel".into(), "Cancel", false).debug_selector(|| "tracker-cleanup-cancel".into())
                .on_click(cx.listener(|this, _, _, cx| { this.tracker.clean_all_confirm = None; cx.notify(); }))))
            .into_any_element()
    }

    fn render_tracker_health(&self, rows: &[CleanupRow], theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div().debug_selector(|| "tracker-cleanup".into()).flex().flex_col().gap(px(Theme::SPACE_SM));
        if let Some(pick) = &self.tracker.health_pick {
            list = list.child(self.render_cleanup_picker(pick, theme, cx));
        }
        if self.tracker.health_incomplete {
            list = list.child(tracker_metadata(theme, "Some PR, project or branch details are unavailable".into()));
        }
        if rows.is_empty() {
            list = list.child(tracker_metadata(theme, if self.tracker.health_incomplete { "No cleanup found in available details" } else { "No cleanup found" }.into()));
        }
        for (index, row) in rows.iter().enumerate() {
            list = list.child(self.render_cleanup_row(index, row, theme, cx));
        }
        list.into_any_element()
    }

    fn render_cleanup_row(&self, index: usize, row: &CleanupRow, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let several = row.chats.len() > 1;
        // Several live chats on one task always list their chats: the choice is per chat.
        let listed = several && (row.kind == CleanupKind::SeveralLiveChats || self.tracker.cleanup_expanded.contains(&row.key));
        let mut head = div().flex().flex_wrap().items_center().gap(px(Theme::SPACE_SM)).child(worker_title(theme, row.heading.clone()));
        if let Some(fix) = &row.fix {
            let selector = format!("cleanup-{}-{index}", fix.selector());
            let fix = fix.clone();
            head = head.child(tracker_action(theme, selector.clone(), fix.label(), false).debug_selector(move || selector.clone())
                .on_click(cx.listener(move |this, _, window, cx| this.run_cleanup_fix(fix.clone(), window, cx))));
        }
        if let [chat] = row.chats.as_slice() {
            head = self.cleanup_chat_buttons(head, index, None, row.kind, &chat.id, chat.kept.is_none(), theme, cx);
        } else if several && row.kind != CleanupKind::SeveralLiveChats {
            let key = row.key.clone();
            head = head.child(tracker_action(theme, format!("cleanup-expand-{index}"), if listed { "Hide chats" } else { "Show chats" }, false)
                .debug_selector(move || format!("cleanup-expand-{index}"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.tracker.cleanup_expanded.remove(&key) { this.tracker.cleanup_expanded.insert(key.clone()); }
                    cx.notify();
                })));
        }
        let mut out = div().debug_selector(move || format!("tracker-cleanup-row-{index}")).flex().flex_col().gap(px(Theme::SPACE_XS))
            .child(head)
            .child(div().text_size(crate::typography::ui_rems(10.5)).text_color(theme.text_muted).child(SharedString::from(row.reason.clone())));
        if let Some(kept) = &row.kept {
            out = out.child(tracker_metadata(theme, kept.clone()));
        }
        if listed {
            for (chat_index, chat) in row.chats.iter().enumerate() {
                let mut line = div().flex().flex_wrap().items_center().gap(px(Theme::SPACE_SM)).pl(px(Theme::SPACE_MD))
                    .child(tracker_metadata(theme, chat.title.clone()));
                if let Some(kept) = chat.kept {
                    line = line.child(tracker_metadata(theme, format!("Kept: {kept}")));
                }
                line = self.cleanup_chat_buttons(line, index, Some(chat_index), row.kind, &chat.id, chat.kept.is_none(), theme, cx);
                out = out.child(line);
            }
        }
        out.into_any_element()
    }

    /// Archive (when the row's kind archives and the chat is not blocked),
    /// Move (chats on containers) and the secondary "Open chat" link.
    #[allow(clippy::too_many_arguments)]
    fn cleanup_chat_buttons(&self, mut line: gpui::Div, index: usize, chat_index: Option<usize>, kind: CleanupKind, chat_id: &str, archivable: bool, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let suffix = chat_index.map_or_else(|| index.to_string(), |chat_index| format!("{index}-{chat_index}"));
        // A single-chat row already shows the row-level fix; only listed chats get their own.
        if chat_index.is_some() && archivable && kind.archives_chats() {
            let id = chat_id.to_owned();
            let selector = format!("cleanup-archive-{suffix}");
            line = line.child(tracker_action(theme, selector.clone(), "Archive", false).debug_selector(move || selector.clone())
                .on_click(cx.listener(move |this, _, _, cx| this.archive_cleanup_chats(vec![id.clone()], cx))));
        }
        if kind == CleanupKind::ChatOnContainer && archivable {
            let id = chat_id.to_owned();
            let selector = format!("cleanup-move-{suffix}");
            line = line.child(tracker_action(theme, selector.clone(), "Move…", false).debug_selector(move || selector.clone())
                .on_click(cx.listener(move |this, _, _, cx| { this.tracker.health_pick = Some(CleanupPick::MoveChat(id.clone())); cx.notify(); })));
        }
        let id = chat_id.to_owned();
        let selector = format!("cleanup-open-{suffix}");
        line.child(tracker_action(theme, selector.clone(), "Open chat", false).debug_selector(move || selector.clone())
            .on_click(cx.listener(move |this, _, _, cx| this.open_chat(id.clone(), cx))))
    }

    fn render_cleanup_picker(&self, pick: &CleanupPick, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let targets = self.tracker.health_targets.clone();
        let count = targets.len();
        let pick = pick.clone();
        let color = theme.text;
        let entity = cx.entity();
        let prompt = match &pick {
            CleanupPick::LinkPr(_) => "Choose an item for this PR",
            CleanupPick::MoveChat(_) => "Choose a task for this chat",
        };
        div().flex().flex_col().gap(px(Theme::SPACE_XS))
            .child(tracker_metadata(theme, prompt.into()))
            .child(gpui::uniform_list("tracker-health-targets", count, move |range, _, app| {
                entity.update(app, |_, cx| range.map(|index| {
                    let (id, title) = &targets[index];
                    let id = id.clone();
                    let pick = pick.clone();
                    div().id(SharedString::from(format!("tracker-health-target-{index}"))).h(px(28.0)).cursor_pointer()
                        .debug_selector(move || format!("tracker-health-target-{index}"))
                        .text_color(color).child(title.clone())
                        .on_click(cx.listener(move |this, _, _, cx| match pick.clone() {
                            CleanupPick::LinkPr(url) => this.link_tracker_health_pr(id.clone(), url, cx),
                            CleanupPick::MoveChat(chat_id) => this.move_tracker_health_chat(chat_id, id.clone(), cx),
                        })).into_any_element()
                }).collect::<Vec<_>>())
            }).h(px(count.min(7) as f32 * 28.0)).w_full())
            .into_any_element()
    }

    fn run_cleanup_fix(&mut self, fix: CleanupFix, window: &mut gpui::Window, cx: &mut Context<Self>) {
        match fix {
            CleanupFix::ArchiveChats(chat_ids) => self.archive_cleanup_chats(chat_ids, cx),
            CleanupFix::MarkCompleted(item_id) => self.complete_tracker_health_item(item_id, cx),
            CleanupFix::OpenLinear(url) => self.open_web_reference(&url, window.modifiers().secondary(), window, cx),
            CleanupFix::ResolveRequest(request_id) => self.resolve_tracker_stale_request(request_id, cx),
            CleanupFix::LinkIssue { item_id, identifier } => self.link_tracker_issue(item_id, identifier, cx),
            CleanupFix::LinkPr(url) => {
                self.tracker.health_pick = Some(CleanupPick::LinkPr(url));
                cx.notify();
            }
        }
    }

    /// Archive from a row. Goes through the same guarded call as Clean all, so a
    /// chat that became busy or active since the row was drawn is kept.
    fn archive_cleanup_chats(&mut self, chat_ids: Vec<String>, cx: &mut Context<Self>) {
        let archive = chat_ids.into_iter().map(|chat_id| {
            let idle = self.tracker.health_chats.get(&chat_id).map_or(0, |facts| facts.idle_seconds);
            (chat_id, idle.saturating_sub(60).max(0))
        }).collect();
        self.apply_cleanup(CleanAllPlan { archive, ..Default::default() }, cx);
    }

    /// Apply a plan: one guarded archive call for the chats (the engine keeps
    /// any that are running, waiting, queued, holding an open Required Action
    /// or active since the plan was made), then each item and request change.
    /// Safe to repeat: chats already archived count as done.
    pub(super) fn apply_cleanup(&mut self, mut plan: CleanAllPlan, cx: &mut Context<Self>) {
        // The chat showing the Tracker is never archived from it.
        plan.archive.retain(|(chat_id, _)| *chat_id != self.active_chat);
        if self.tracker.cleanup_busy || plan.is_empty() { return; }
        let Some((space_id, work_item_id)) = self.tracker.health_key.as_ref().map(|key| (key.0.clone(), key.1.clone())) else { return; };
        let archive = if plan.archive.is_empty() { None } else {
            let Some(engine) = self.state.read(cx).engine().cloned() else {
                self.sidebar_notice = Some("Engine not connected".into());
                cx.notify();
                return;
            };
            let chats = plan.archive.iter().map(|(chat_id, min_idle_seconds)| mast_proto::HealthArchiveChat { chat_id: chat_id.clone(), min_idle_seconds: *min_idle_seconds }).collect();
            Some((engine, mast_proto::WorkOutlineHealthArchiveParams { space_id, work_item_id, chats }))
        };
        let archive_count = plan.archive.len();
        let actions = self.state.read(cx).work_outline_actions();
        let completes: Vec<_> = plan.complete.iter().map(|item_id| actions.update_work_item(mast_proto::UpdateWorkItemParams {
            work_item_id: item_id.clone(),
            changes: mast_proto::WorkItemChanges { status: Some(mast_proto::WorkItemStatus::Completed), ..Default::default() },
            operation_id: crate::work_outline::actions::new_operation_id(),
        })).collect();
        let resolves: Vec<_> = plan.resolve.iter().map(|request_id| actions.resolve_user_action_request(mast_proto::ResolveUserActionRequestParams {
            user_action_request_id: request_id.clone(), outcome: Some("done".into()),
            note: Some("PR merged".into()), answer: None, values: None, via: Some("tracker".into()),
            operation_id: crate::work_outline::actions::new_operation_id(),
        })).collect();
        self.tracker.clean_all_confirm = None;
        self.tracker.cleanup_result = None;
        self.tracker.clean_all_chat = self.active_chat.clone();
        self.tracker.cleanup_busy = true;
        cx.notify();
        cx.spawn(async move |shell, cx| {
            let mut outcome = CleanupOutcome::default();
            if let Some((engine, params)) = archive {
                let reply = engine.client().call(mast_rpc::methods::WORK_OUTLINE_HEALTH_ARCHIVE, serde_json::to_value(&params).unwrap_or_default()).await;
                match reply.map(serde_json::from_value::<mast_proto::WorkOutlineHealthArchiveReply>) {
                    Ok(Ok(reply)) => outcome.record_archive(&reply),
                    // An engine from before this feature: retrying cannot help.
                    Err(mast_rpc::RpcError::UnknownMethod(_)) => {
                        outcome.failed += archive_count;
                        outcome.problem = Some("This engine is too old to archive from the Tracker. Restart Mast to update it.".into());
                    }
                    // A lost reply may still have archived: the refresh shows what happened.
                    Err(error) => {
                        outcome.failed += archive_count;
                        outcome.problem = Some(error.to_string());
                    }
                    Ok(Err(error)) => {
                        outcome.failed += archive_count;
                        outcome.problem = Some(format!("Unexpected reply: {error}"));
                    }
                }
            }
            for task in completes {
                if task.await.is_ok() { outcome.completed += 1 } else { outcome.failed += 1 }
            }
            for task in resolves {
                if task.await.is_ok() { outcome.resolved += 1 } else { outcome.failed += 1 }
            }
            let _ = shell.update(cx, |shell, cx| {
                shell.tracker.health_key = None;
                shell.tracker.key = None;
                let active = shell.active_chat.clone();
                shell.load_tracker_health(&active, cx);
                // After the reload, which clears the result when it restarts the list.
                shell.tracker.cleanup_busy = false;
                shell.tracker.cleanup_result = Some(outcome.message());
                cx.notify();
            });
        }).detach();
    }
}
