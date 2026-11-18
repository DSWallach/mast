use super::*;

impl AppState {
    pub fn gate(&self) -> GatePhase {
        gate_phase(&self.connection, self.workspace_scope, self.auth.as_ref())
    }

    pub fn engine(&self) -> Option<&EngineHandle> {
        self.engine.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn set_test_engine(&mut self, handle: EngineHandle) {
        self.engine = Some(handle);
    }

    /// Drop every account-scoped view and subscription after its runtime has
    /// stopped. The next bootstrap must never render rows from the previous
    /// account while the local profile is opening.
    pub fn prepare_runtime_replacement(&mut self, cx: &mut Context<Self>) {
        self.engine = None;
        self.watch_tasks.clear();
        self.transcript_task = None;
        self.change_request_tasks.clear();
        self.change_requests = ChangeRequestClientState::default();
        self.work_outline_tasks.clear();
        self.work_outlines = crate::work_outline::WorkOutlineStore::new();
        self.work_outline_runtime = None;
        self.artifact_tasks.clear();
        self.artifacts = crate::artifacts::ArtifactStore::new();
        self.artifact_render_task = None;
        self.connection = ConnectionStatus::Connecting;
        self.workspace_scope = None;
        self.auth = None;
        self.devices.clear();
        self.device_presentation = None;
        self.session_presence_presentation.clear();
        self.spaces.clear();
        self.chats.clear();
        self.sessions.clear();
        self.sidebar_preferences = SidebarPreferencesState::default();
        self.session_presentation = None;
        self.selected_space = None;
        self.no_project = false;
        self.selected_device = None;
        self.selected_chat = None;
        self.auto_selected = false;
        self.chats_synced = false;
        self.spaces_synced = false;
        self.transcript.clear();
        self.transcript_baselines.clear();
        self.transcript_cache.clear();
        self.prepared_transcripts.clear();
        self.context_usage = None;
        self.transcript_revision = self.transcript_revision.wrapping_add(1);
        self.transcript_replayed = false;
        self.echoes.clear();
        self.pending_sends.clear();
        self.upload_progress = None;
        self.transfers.clear();
        self.local_device_id = None;
        self.update = None;
        cx.notify();
    }

    // ---- gpui glue ----

    /// Kick off (or retry) the engine bootstrap: probe → connect-or-embed on
    /// tokio, then attach subscriptions. Safe to call again after `Failed`.
    pub fn bootstrap(state: Entity<AppState>, config: EngineBootConfig, cx: &mut App) {
        let data_dir = config.data_dir.clone();
        state.update(cx, |s, cx| {
            s.connection = ConnectionStatus::Connecting;
            s.workspace_scope = None;
            s.auth = None;
            s.data_dir = Some(data_dir);
            cx.notify();
        });
        #[cfg(feature = "startup-bench")]
        startup_trace("bootstrap_spawn");
        let boot = Tokio::spawn(cx, EngineHandle::bootstrap(config));
        cx.spawn(async move |cx| {
            let outcome = match boot.await {
                Ok(Ok(handle)) => Ok(handle),
                Ok(Err(err)) => Err(format!("{err:#}")),
                Err(join_err) => Err(join_err.to_string()),
            };
            // NB: at the pinned rev `Entity::update(&mut AsyncApp)` returns the
            // closure's value directly (no Result) — AsyncApp implements
            // AppContext like App does.
            state.update(cx, |s, cx| match outcome {
                Ok(handle) => s.attach_engine(handle, cx),
                Err(message) => {
                    tracing::error!(%message, "engine bootstrap failed");
                    s.connection = ConnectionStatus::Failed(message);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Wire the connected engine: mark Ready and start the standing watches.
    /// Methods the engine doesn't serve yet (chats/devices/auth land with the
    /// workspace doc in M4) fail their subscribe and are skipped gracefully.
    fn attach_engine(&mut self, handle: EngineHandle, cx: &mut Context<Self>) {
        #[cfg(feature = "startup-bench")]
        startup_trace("attach_engine");
        // The attachment notification precedes the first connectivity frame.
        // Make that bootstrap gap explicit so the shell resets its alert
        // baseline instead of comparing the new runtime with the old one.
        self.connectivity_observed = false;
        let engine_info = handle.engine_info();
        self.workspace_scope = Some(engine_info.workspace_scope);
        self.local_device_id = Some(engine_info.device_id.clone());
        self.engine = Some(handle.clone());
        self.load_managed_accounts(None, cx);
        let mut watch_tasks = Vec::with_capacity(11);
        if let Some(task) = spawn_deferred_engine_watch(cx, handle.clone()) {
            watch_tasks.push(task);
        }
        watch_tasks.extend([
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_SESSIONS,
                AppState::apply_sessions,
            ),
            spawn_chats_watch(cx, handle.clone()),
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_SIDEBAR_PREFERENCES,
                AppState::apply_sidebar_preferences,
            ),
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_DEVICES,
                AppState::apply_devices,
            ),
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_CONNECTIVITY,
                |state, value| {
                    state.apply_connectivity(value);
                    true
                },
            ),
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_TRANSFERS,
                |state, value| {
                    state.apply_transfers(value);
                    true
                },
            ),
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_CHAT_DIFF_TARGETS,
                AppState::apply_chat_diff_targets,
            ),
            spawn_watch(cx, handle.clone(), methods::WATCH_SPACES, |state, value| {
                state.apply_spaces(value);
                true
            }),
            // Auth frames parse tolerantly — engine and proto tags differ today.
            spawn_watch(cx, handle.clone(), methods::AUTH_STATUS, |state, value| {
                state.apply_auth_value(value);
                true
            }),
            spawn_watch(
                cx,
                handle.clone(),
                methods::UPDATE_STATUS,
                |state, value| {
                    state.apply_update(value);
                    true
                },
            ),
            spawn_local_device_probe(cx, handle.clone()),
        ]);
        self.watch_tasks = watch_tasks;
        self.work_outline_runtime = Some(WorkOutlineRuntime {
            state: cx.weak_entity(),
            app: cx.to_async(),
        });
        self.reconcile_change_request_watches(cx);
        self.reconcile_work_outline_watches(cx);
        // This headed UI serves the engine's artifact render checks.
        self.start_artifact_render_executor(
            handle.clone(),
            crate::artifacts::render_check::platform_renderer(),
            crate::artifacts::render_check::RenderConfig::default(),
            cx,
        );
        // EngineInfo is part of the attachment boundary: views must know which
        // data profile they reached before they are allowed to render Ready.
        self.connection = ConnectionStatus::Ready;
        // Re-subscribe the transcript if a chat was already selected (reconnect path).
        if let Some(chat_id) = self.selected_chat.clone() {
            self.transcript_task =
                Some(spawn_transcript_watch(cx, handle.clone(), chat_id.clone()));
            if handle
                .engine_info()
                .supports(mast_proto::capabilities::MESSAGE_QUEUE_V1)
            {
                self.queue_task = Some(spawn_queue_watch(cx, handle, chat_id));
            }
        }
        cx.notify();
    }

    pub(super) fn reconcile_change_request_watches(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.engine.clone() else {
            self.change_request_tasks.clear();
            return;
        };
        let targets = if self.change_requests_visible {
            desired_watch_targets(
                &self.chats,
                &self.spaces,
                |device| !self.change_requests.is_supported(device),
                |chat_id| self.selected_chat_diff_target(chat_id),
            )
        } else {
            HashSet::new()
        };

        let before = self.change_request_tasks.len();
        self.change_request_tasks
            .retain(|target, _| targets.contains(target));
        self.change_requests.retain_targets(&targets);
        let added = targets
            .iter()
            .any(|target| !self.change_request_tasks.contains_key(target));
        if added || self.change_request_tasks.len() != before {
            self.change_request_epoch
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        let local_device_id = self.local_device_id.clone();
        for target in targets {
            if self.change_request_tasks.contains_key(&target) {
                continue;
            }
            let task = spawn_change_request_watch(
                cx,
                handle.clone(),
                target.clone(),
                local_device_id.clone(),
                self.change_request_epoch.clone(),
            );
            self.change_request_tasks.insert(target, task);
        }
    }

    pub fn set_change_requests_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.change_requests_visible != visible {
            self.change_requests_visible = visible;
            self.reconcile_change_request_watches(cx);
        }
    }

    pub fn open_deep_link(&mut self, url: &str, cx: &mut Context<Self>) {
        match crate::links::parse_mast_conversation_link(url) {
            Ok(link) => {
                self.pending_deep_link = Some(link);
                self.apply_pending_deep_link(cx);
            }
            Err(error) => self.deep_link_notice = Some(error.to_string()),
        }
        cx.notify();
    }

    pub(super) fn apply_pending_deep_link(&mut self, cx: &mut Context<Self>) {
        let Some(link) = self.pending_deep_link.clone() else {
            return;
        };
        let Some(locator) = crate::links::workspace_locator(
            self.workspace_scope,
            self.auth.as_ref(),
            self.local_device_id.as_deref(),
        ) else {
            return;
        };
        if locator != link.workspace {
            self.pending_deep_link = None;
            self.deep_link_notice =
                Some("This conversation link belongs to another workspace".into());
            return;
        }
        if self.chats.iter().any(|chat| chat.id == link.chat_id) {
            self.pending_deep_link = None;
            self.select_chat(Some(link.chat_id), cx);
        } else if self.chats_synced {
            self.pending_deep_link = None;
            self.deep_link_notice = Some("The linked conversation was not found".into());
        }
    }

    pub fn take_deep_link_notice(&mut self) -> Option<String> {
        self.deep_link_notice.take()
    }

    /// Select a chat (or clear). Swaps the per-chat doc-transcript subscription:
    /// dropping the old task drops its stream receiver, which cancels the doc
    /// watch server-side. Selecting a chat also lands in its space and marks it
    /// seen (a global-list click must switch the tab strip too).
    pub fn select_chat(&mut self, chat_id: Option<String>, cx: &mut Context<Self>) {
        if self.selected_chat == chat_id {
            // Re-selecting still clears a fresh "completed" badge.
            if let Some(id) = chat_id {
                self.mark_chat_seen(&id, cx);
            }
            return;
        }
        // Take the destination before trimming: switching to the oldest warm
        // transcript must not evict the very entry we are about to display.
        let cached = self
            .transcript_cache
            .iter()
            .position(|cached| Some(&cached.chat_id) == chat_id.as_ref())
            .and_then(|index| self.transcript_cache.remove(index));
        if let Some(previous) = &self.selected_chat {
            let old_baseline = self.transcript_baselines.remove(previous);
            cx.background_executor()
                .spawn(async move {
                    drop(old_baseline);
                })
                .detach();
            let prepared = self.prepared_transcripts.remove(previous);
            if self.transcript_replayed {
                let entries = std::mem::take(&mut self.transcript);
                let bytes = prepared.as_ref().map_or_else(
                    || {
                        entries
                            .iter()
                            .map(|entry| {
                                std::mem::size_of::<SessionMessageEntry>()
                                    + entry.id.len()
                                    + entry
                                        .parts
                                        .iter()
                                        .map(|part| {
                                            std::mem::size_of::<mast_doc::MessagePart>()
                                                + part.byte_len()
                                        })
                                        .sum::<usize>()
                            })
                            .sum()
                    },
                    |p| p.bytes,
                );
                self.transcript_cache.push_back(CachedTranscript {
                    prepared,
                    chat_id: previous.clone(),
                    entries,
                    context_usage: self.context_usage,
                    bytes,
                });
                while self.transcript_cache.len() > TRANSCRIPT_CACHE_CAP
                    || self
                        .transcript_cache
                        .iter()
                        .map(|cached| cached.bytes)
                        .sum::<usize>()
                        > TRANSCRIPT_CACHE_BYTES
                {
                    let evicted = self.transcript_cache.pop_front();
                    cx.background_executor()
                        .spawn(async move {
                            drop(evicted);
                        })
                        .detach();
                }
            }
        }
        self.selected_chat = chat_id.clone();
        self.auto_selected = true;
        self.transcript.clear();
        self.context_usage = None;
        self.transcript_revision = self.transcript_revision.wrapping_add(1);
        self.transcript_replayed = false;
        if let Some(cached) = cached {
            self.transcript_baselines.insert(
                cached.chat_id.clone(),
                cached
                    .prepared
                    .as_ref()
                    .map(|p| p.navigation_baseline.clone())
                    .unwrap_or_else(|| {
                        Arc::new(mast_doc::TranscriptBaseline::capture(&cached.entries))
                    }),
            );
            if let Some(prepared) = cached.prepared {
                self.prepared_transcripts.insert(cached.chat_id, prepared);
            }
            self.transcript = cached.entries;
            self.context_usage = cached.context_usage;
            self.transcript_replayed = true;
        }
        self.transcript_task = None;
        self.queue.clear();
        self.rejected_sends.clear();
        self.queue_task = None;
        if let Some(id) = chat_id.as_deref() {
            // A chat implies its project (or the lack of one); `select_chat(None)`
            // (the new-session canvas) keeps the current project pick.
            if let Some(chat) = self.chats.iter().find(|c| c.id == id) {
                match chat.space_id.clone() {
                    Some(space_id) => {
                        self.selected_space = Some(space_id);
                        self.no_project = false;
                    }
                    None => {
                        self.selected_space = None;
                        self.no_project = true;
                        self.selected_device = Some(chat.device_id.clone());
                    }
                }
            }
            self.mark_chat_seen(id, cx);
        }
        if let (Some(chat_id), Some(handle)) = (chat_id, self.engine.clone()) {
            self.transcript_task =
                Some(spawn_transcript_watch(cx, handle.clone(), chat_id.clone()));
            if handle
                .engine_info()
                .supports(mast_proto::capabilities::MESSAGE_QUEUE_V1)
            {
                self.queue_task = Some(spawn_queue_watch(cx, handle, chat_id));
            }
        }
        // The selected Chat's Space is always watched (chat header drift).
        self.reconcile_work_outline_watches(cx);
        cx.notify();
    }

    /// Replace the selected chat's queue subscription without clearing its
    /// current projection. This is used after an optimistic mutation fails:
    /// the authoritative opening frame repairs the local list even though the
    /// document itself did not change and therefore emitted no new frame.
    pub(crate) fn refresh_selected_queue(&mut self, cx: &mut Context<Self>) {
        self.queue_task = None;
        let (Some(chat_id), Some(handle)) = (self.selected_chat.clone(), self.engine.clone())
        else {
            return;
        };
        if handle
            .engine_info()
            .supports(mast_proto::capabilities::MESSAGE_QUEUE_V1)
        {
            self.queue_task = Some(spawn_queue_watch(cx, handle, chat_id));
        }
    }

    /// Select a project; the caller (shell) decides which chat to land on.
    /// `Some` clears a "Don't work in a project" opt-out and re-aims the
    /// device pick at the project's host; `None` IS that opt-out.
    pub fn select_space(&mut self, space_id: Option<String>, cx: &mut Context<Self>) {
        match &space_id {
            Some(id) => {
                self.no_project = false;
                if let Some(device) = self.space_row(id).map(|s| s.device_id.clone()) {
                    self.selected_device = Some(device);
                }
            }
            None => {
                self.selected_device = self.effective_device_id();
                self.no_project = true;
            }
        }
        if self.selected_space == space_id && space_id.is_some() {
            cx.notify();
            return;
        }
        self.selected_space = space_id;
        cx.notify();
    }

    /// Synced seen marker: only fires when the chat is currently unseen
    /// (idempotence — no mutate spam), stamps the local row optimistically so
    /// the LWW round-trip is invisible, and fire-and-forgets the mutate.
    /// Window-focus liveness sweep: ask the engine to probe every open room
    /// (workspace + chat docs). Fire-and-forget; each room ignores the hint
    /// unless it has been broadcast-quiet ≥30s, so spamming is harmless.
    pub fn probe_sync(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.engine.clone() else {
            return;
        };
        cx.spawn(async move |_, _| {
            let params = serde_json::json!({});
            if let Err(err) = handle.client().call(methods::PROBE_SYNC, params).await {
                tracing::debug!(error = %err, "probe sync failed");
            }
        })
        .detach();
    }

    pub fn mark_chat_seen(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) else {
            return;
        };
        if !chat.unseen() {
            return;
        }
        chat.last_seen_at = Some(Utc::now());
        cx.notify();
        let Some(handle) = self.engine.clone() else {
            return;
        };
        let chat_id = chat_id.to_string();
        cx.spawn(async move |_, _| {
            let params = serde_json::json!({ "op": "markChatSeen", "chatId": chat_id });
            if let Err(err) = handle.client().call(methods::MUTATE, params).await {
                tracing::warn!(chat = %chat_id, error = %err, "markChatSeen failed");
            }
        })
        .detach();
    }
}
