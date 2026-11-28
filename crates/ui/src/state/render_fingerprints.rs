use super::*;

impl AppState {
    /// Cheap hash of everything the Overview's row list and render read from
    /// state: every chat and space field, each chat's derived status (which
    /// folds in the session row, pending sends and connectivity), the Work
    /// Outline loads, and the selection. Streaming transcript frames and
    /// other changes to unrelated fields leave it unchanged, so the Overview
    /// can skip its rebuild and repaint for them. Hashing in place (no clones)
    /// keeps this far cheaper than the rebuild it gates.
    pub(crate) fn overview_fingerprint(&self, now: DateTime<Utc>) -> u64 {
        use std::hash::{Hash, Hasher};
        use std::mem::discriminant;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.personal_assistant.hash_designation(&mut h);
        self.chats.len().hash(&mut h);
        let sessions = self.sessions_by_chat();
        for chat in &self.chats {
            #[cfg(test)]
            crate::shell::perf_counts::FINGERPRINT_CHATS.with(|c| c.set(c.get() + 1));
            chat.id.hash(&mut h);
            chat.device_id.hash(&mut h);
            chat.title.hash(&mut h);
            chat.archived.hash(&mut h);
            chat.cwd.hash(&mut h);
            chat.branch.hash(&mut h);
            chat.checkout_id.hash(&mut h);
            if let Some(ctx) = &chat.source_context {
                (
                    &ctx.checkout_id,
                    &ctx.repo_root,
                    &ctx.cwd,
                    &ctx.branch,
                    &ctx.head_sha,
                    ctx.observed_at,
                )
                    .hash(&mut h);
            } else {
                0u8.hash(&mut h);
            }
            match &chat.config {
                Some(config) => {
                    1u8.hash(&mut h);
                    config.harness.hash(&mut h);
                    config.model.hash(&mut h);
                    config.reasoning.map(|r| discriminant(&r)).hash(&mut h);
                    if !config.model_options.is_empty() {
                        config.model_options.hash(&mut h);
                    }
                    discriminant(&config.sandbox).hash(&mut h);
                    config.auto_approve.hash(&mut h);
                }
                None => 0u8.hash(&mut h),
            }
            chat.last_message_preview.hash(&mut h);
            chat.last_message_at.hash(&mut h);
            chat.created_at.hash(&mut h);
            chat.harness_session_id.hash(&mut h);
            chat.harness_session_cwd.hash(&mut h);
            chat.actual_model.hash(&mut h);
            chat.space_id.hash(&mut h);
            chat.last_seen_at.hash(&mut h);
            chat.room_gen.hash(&mut h);
            chat.parent_chat_id.hash(&mut h);
            chat.scheduled_task.hash(&mut h);
            chat.scheduled_task_info.hash(&mut h);
            chat.standing_role_successor.hash(&mut h);
            if let Some(failures) = sessions.get(chat.id.as_str()).and_then(|s| s.last_turn_tool_failures.as_ref()) {
                1u8.hash(&mut h);
                failures.count.hash(&mut h);
                failures.tools.hash(&mut h);
            } else { 0u8.hash(&mut h); }
            chat.linked_pr_url.hash(&mut h);
            chat.linked_pr_source.map(|s| discriminant(&s)).hash(&mut h);
            chat.linked_ticket_id.hash(&mut h);
            chat.linked_ticket_source
                .map(|s| discriminant(&s))
                .hash(&mut h);
            discriminant(&self.display_status_with_session(
                chat,
                sessions.get(chat.id.as_str()).copied(),
                now,
            ))
            .hash(&mut h);
        }
        self.spaces.len().hash(&mut h);
        for space in &self.spaces {
            space.id.hash(&mut h);
            space.device_id.hash(&mut h);
            space.path.hash(&mut h);
            space.name.hash(&mut h);
            space.git_detected.hash(&mut h);
            space.git_checked_at.hash(&mut h);
            space.checkout_id.hash(&mut h);
            space.created_at.hash(&mut h);
            match self.work_outline(&space.id) {
                crate::work_outline::WorkOutlineLoad::Loading => 0u8.hash(&mut h),
                crate::work_outline::WorkOutlineLoad::Ready(snapshot) => {
                    1u8.hash(&mut h);
                    (Arc::as_ptr(&snapshot) as usize).hash(&mut h);
                }
                crate::work_outline::WorkOutlineLoad::Unsupported => 2u8.hash(&mut h),
                crate::work_outline::WorkOutlineLoad::Failed(reason) => {
                    3u8.hash(&mut h);
                    reason.hash(&mut h);
                }
            }
        }
        for chat in &self.chats {
            if let Some(target) = self.selected_chat_diff_target(&chat.id) {
                chat.id.hash(&mut h);
                target.path.hash(&mut h);
                target.checkout_id.hash(&mut h);
                target.branch.hash(&mut h);
            }
        }
        self.selected_chat.hash(&mut h);
        self.engine.is_some().hash(&mut h);
        self.data_dir.hash(&mut h);
        // The Overview rows' build badges. Absent (not hashed) while nothing builds, so the value
        // is exactly what it was before build jobs existed.
        let builds = self.build_jobs.rows_fingerprint();
        if builds != 0 {
            builds.hash(&mut h);
        }
        h.finish()
    }

    /// [`Self::overview_fingerprint`] plus everything else the Shell's own
    /// render reads: connection, scope, auth, update strip, connectivity,
    /// devices, sidebar preferences, project/device picks, sync flags, the
    /// send overlays (time-based), PR badges, artifacts, ticket and PR chips
    /// and the subagent tabs' streaming flag. It deliberately leaves out the
    /// selected chat's transcript, queue, echoes, context usage and the other
    /// streaming fields, which the views that render them observe themselves,
    /// so those frames no longer repaint the whole window.
    pub(crate) fn shell_fingerprint(&self, now: DateTime<Utc>) -> u64 {
        use std::fmt::Write as _;
        use std::hash::{Hash, Hasher};
        struct Feed<'a>(&'a mut std::collections::hash_map::DefaultHasher);
        impl std::fmt::Write for Feed<'_> {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                self.0.write(text.as_bytes());
                Ok(())
            }
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.overview_fingerprint(now).hash(&mut h);
        // The running-chats popover's Builds block (0 while there is nothing to show).
        self.build_jobs.panel_fingerprint().hash(&mut h);
        let _ = write!(
            Feed(&mut h),
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            self.connection,
            self.workspace_scope,
            self.auth,
            self.update,
            self.connectivity,
            self.sidebar_preferences,
            self.devices,
        );
        (
            self.connectivity_observed,
            &self.local_device_id,
            &self.selected_space,
            self.no_project,
            &self.selected_device,
            self.auto_selected,
            self.chats_synced,
            self.spaces_synced,
        )
            .hash(&mut h);
        // Send overlays flip with time alone (grace expiry), so hash the
        // derived flags, not just the keys.
        let mut sends = 0u64;
        for (chat_id, send) in &self.pending_sends {
            let mut sh = std::collections::hash_map::DefaultHasher::new();
            (
                chat_id,
                send.started,
                self.send_pending(chat_id, now),
                self.send_queued(chat_id, now),
                self.send_undelivered(chat_id, now),
            )
                .hash(&mut sh);
            sends ^= sh.finish();
        }
        sends.hash(&mut h);
        self.change_requests.fingerprint().hash(&mut h);
        self.artifacts.fingerprint().hash(&mut h);
        self.github.fingerprint().hash(&mut h);
        self.linear.issues_fingerprint().hash(&mut h);
        let mut subagents = 0u64;
        for (doc_id, entries) in &self.sub_transcripts {
            let mut sh = std::collections::hash_map::DefaultHasher::new();
            (
                doc_id,
                entries.len(),
                entries
                    .last()
                    .map(|entry| entry.status.map(|s| std::mem::discriminant(&s))),
            )
                .hash(&mut sh);
            subagents ^= sh.finish();
        }
        subagents.hash(&mut h);
        if let Some(chat_id) = &self.selected_chat {
            for running in self.running_activity(chat_id) {
                (running.id, running.title).hash(&mut h);
            }
        }
        h.finish()
    }
}
