use super::*;

/// Observe assembly after an early attach (cloud onboarding or another viewport
/// reaching the embedded engine over IPC). Data subscriptions wait on the same
/// result, but their individual errors are not authoritative: older engines may
/// legitimately omit a watch method. Only the assembly result may fail the
/// whole connection.
pub(super) fn spawn_deferred_engine_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
) -> Option<Task<()>> {
    let mut deferred = handle.deferred_state()?;
    Some(cx.spawn(async move |this, cx| {
        let Err(failure) = wait_for_deferred_engine(&mut deferred).await else {
            return;
        };
        tracing::error!(error = %failure, "engine assembly failed after attachment");
        // Embedded handles release their IPC listener before exposing Retry;
        // remote handles stop their completed readiness probe.
        handle.shutdown().await;
        this.update(cx, |state, cx| {
            state.connection = ConnectionStatus::Failed(failure);
            cx.notify();
        })
        .ok();
    }))
}

/// Chats watch. Boot selection is the shell's job (it lands on the first
/// restored open tab, device-local state this entity can't see); this task
/// only pumps frames.

#[cfg(feature = "startup-bench")]
pub(super) fn startup_trace(event: &str) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    eprintln!("BENCH event | {event} | us={}", now.as_micros());
}

pub(super) fn spawn_chats_watch(cx: &mut Context<AppState>, handle: EngineHandle) -> Task<()> {
    cx.spawn(async move |this, cx| {
        // Resubscribe loop (same contract as the transcript watch): a daemon
        // restart or RPC drop ends the stream, and a bare return here froze
        // the sidebar until app restart — new chats, renames and archives
        // from every device silently stopped arriving.
        let mut backoff = mast_harness::backoff::Backoff::new(
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(30),
        );
        loop {
            #[cfg(feature = "startup-bench")]
            startup_trace("chats_subscribe_start");
            let mut rx = match handle
                .client()
                .subscribe(methods::WATCH_CHATS, serde_json::json!({}))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::debug!(error = %err, "chats watch unavailable; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(backoff.next_delay()).await;
                    continue;
                }
            };
            while let Some(value) = rx.recv().await {
                backoff.reset();
                #[cfg(feature = "startup-bench")]
                startup_trace("chats_frame_received");
                #[cfg(feature = "startup-bench")]
                let decode_started = std::time::Instant::now();
                let parsed: Vec<Chat> = match serde_json::from_value(value) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        tracing::warn!(error = %err, "dropping malformed chats frame");
                        continue;
                    }
                };
                #[cfg(feature = "startup-bench")]
                eprintln!("BENCH phase | chats_decode | ms={:.3}", decode_started.elapsed().as_secs_f64() * 1000.0);
                #[cfg(feature = "startup-bench")]
                let update_span = crate::startup_measurement::MeasurementSpan::new("chats_update");
                let alive = this.update(cx, |state, cx| {
                    #[cfg(feature = "startup-bench")]
                    startup_trace("chats_apply");
                    state.apply_chats(parsed);
                    state.apply_pending_deep_link(cx);
                    state.reconcile_change_request_watches(cx);
                    // A Chat's Space can arrive (or the selection vanish)
                    // with a chats frame.
                    state.reconcile_work_outline_watches(cx);
                    cx.notify();
                });
                #[cfg(feature = "startup-bench")]
                drop(update_span);
                if alive.is_err() {
                    return;
                }
            }
            tracing::debug!("chats stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(backoff.next_delay()).await;
        }
    })
}

/// Backoff before resubscribing a failed `WatchWorkOutline`: 2 s doubling to
/// a 30 s ceiling.
pub(super) fn work_outline_retry_delay(attempt: u32) -> std::time::Duration {
    mast_harness::backoff::delay_for_attempt(
        std::time::Duration::from_secs(2),
        std::time::Duration::from_secs(30),
        attempt,
    )
}

/// Retry delay for a failed change-request watch (before jitter). Transport
/// errors back off 2 s doubling to 30 s; a rejected request (`BadParams`: the
/// cwd is not a known checkout, e.g. a deleted worktree) will fail the same way,
/// so it backs off 2 s doubling to a 5 minute ceiling.
pub(super) fn change_request_retry_delay(err: &RpcError, attempt: u32) -> std::time::Duration {
    let base = std::time::Duration::from_secs(2);
    let ceiling = if matches!(err, RpcError::BadParams(_)) {
        std::time::Duration::from_secs(300)
    } else {
        std::time::Duration::from_secs(30)
    };
    mast_harness::backoff::delay_for_attempt(base, ceiling, attempt)
}

/// Sleep `delay`, waking early once `epoch` moves off `seen`.
pub(super) async fn sleep_until_epoch_changes(
    cx: &mut gpui::AsyncApp,
    delay: std::time::Duration,
    epoch: &std::sync::atomic::AtomicU64,
    seen: u64,
) {
    const SLICE: std::time::Duration = std::time::Duration::from_secs(2);
    let mut remaining = delay;
    while !remaining.is_zero() && epoch.load(std::sync::atomic::Ordering::Relaxed) == seen {
        let step = remaining.min(SLICE);
        cx.background_executor().timer(step).await;
        remaining -= step;
    }
}

/// One `WatchWorkOutline` subscription per Space. Mirrors
/// `spawn_change_request_watch`: `UnknownMethod` (a host that predates the
/// feature) marks the Space `Unsupported` and stops; other failures retry with
/// backoff; malformed frames are dropped, never fatal; the last good snapshot
/// survives stream gaps.
pub(super) fn spawn_work_outline_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    space_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let mut attempt: u32 = 0;
        loop {
            #[cfg(feature = "startup-bench")]
            startup_trace("outline_subscribe_start");
            let params = serde_json::json!({ "spaceId": space_id });
            let mut subscription = match handle
                .client()
                .subscribe_checked(methods::WATCH_WORK_OUTLINE, params)
                .await
            {
                Ok(subscription) => subscription,
                Err(err) => {
                    let unsupported = matches!(err, RpcError::UnknownMethod(_));
                    let message = err.to_string();
                    let load =
                        crate::work_outline::WorkOutlineLoad::from_watch_error(&message);
                    let terminal =
                        unsupported || matches!(load, crate::work_outline::WorkOutlineLoad::Unsupported);
                    tracing::debug!(%space_id, error = %err, terminal, "work outline watch unavailable");
                    let alive = this.update(cx, |state, cx| {
                        let message = if unsupported {
                            "unknown method".to_owned()
                        } else {
                            message
                        };
                        if state.apply_work_outline_failure(&space_id, &message) {
                            cx.notify();
                        }
                    });
                    if alive.is_err() || terminal {
                        return;
                    }
                    cx.background_executor()
                        .timer(mast_harness::backoff::jittered(work_outline_retry_delay(attempt)))
                        .await;
                    attempt = attempt.saturating_add(1);
                    continue;
                }
            };
            #[cfg(feature = "startup-bench")]
            startup_trace("outline_subscribe_ack");
            attempt = 0;
            // Revisions are per stream; a stale frame is one older than the
            // newest already applied from this subscription.
            let mut newest_revision: u64 = 0;
            while let Some(value) = subscription.recv().await {
                #[cfg(feature = "startup-bench")]
                startup_trace("outline_frame_received");
                #[cfg(feature = "startup-bench")]
                let decode_started = std::time::Instant::now();
                let snapshot: mast_proto::WorkOutlineSnapshot = match serde_json::from_value(value)
                {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        tracing::warn!(%space_id, error = %err, "dropping malformed work outline frame");
                        continue;
                    }
                };
                #[cfg(feature = "startup-bench")]
                eprintln!("BENCH phase | outline_decode | ms={:.3}", decode_started.elapsed().as_secs_f64() * 1000.0);
                if snapshot.revision < newest_revision {
                    continue;
                }
                newest_revision = snapshot.revision;
                #[cfg(feature = "startup-bench")]
                let update_span = crate::startup_measurement::MeasurementSpan::new("outline_update");
                let alive = this.update(cx, |state, cx| {
                    #[cfg(feature = "startup-bench")]
                    startup_trace("outline_apply");
                    if state.apply_work_outline_frame(&space_id, snapshot) {
                        cx.notify();
                    }
                });
                #[cfg(feature = "startup-bench")]
                drop(update_span);
                if alive.is_err() {
                    return;
                }
            }
            // Keep the last good snapshot through a transport gap.
            tracing::debug!(%space_id, "work outline stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor()
                .timer(mast_harness::backoff::jittered(work_outline_retry_delay(0)))
                .await;
        }
    })
}

/// One `WatchArtifacts` subscription per Space, mirroring
/// [`spawn_work_outline_watch`]: `UnknownMethod` (a host without the
/// artifact RPCs) marks the Space unsupported and stops; other failures
/// retry with backoff; malformed frames are dropped; the last good snapshot
/// survives stream gaps.
pub(super) fn spawn_artifacts_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    space_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let mut attempt: u32 = 0;
        loop {
            let params = serde_json::json!({ "spaceId": space_id });
            let mut subscription = match handle
                .client()
                .subscribe_checked(methods::WATCH_ARTIFACTS, params)
                .await
            {
                Ok(subscription) => subscription,
                Err(err) => {
                    let unsupported = matches!(err, RpcError::UnknownMethod(_));
                    let message = if unsupported {
                        "unknown method".to_owned()
                    } else {
                        err.to_string()
                    };
                    let terminal = unsupported
                        || matches!(
                            crate::artifacts::ArtifactsLoad::from_watch_error(&message),
                            crate::artifacts::ArtifactsLoad::Unsupported
                        );
                    tracing::debug!(%space_id, error = %err, terminal, "artifacts watch unavailable");
                    let alive = this.update(cx, |state, cx| {
                        if state.apply_artifacts_failure(&space_id, &message) {
                            cx.notify();
                        }
                    });
                    if alive.is_err() || terminal {
                        return;
                    }
                    cx.background_executor()
                        .timer(mast_harness::backoff::jittered(work_outline_retry_delay(attempt)))
                        .await;
                    attempt = attempt.saturating_add(1);
                    continue;
                }
            };
            attempt = 0;
            let mut newest_revision: u64 = 0;
            while let Some(value) = subscription.recv().await {
                let snapshot: mast_proto::ArtifactsSnapshot = match serde_json::from_value(value) {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        tracing::warn!(%space_id, error = %err, "dropping malformed artifacts frame");
                        continue;
                    }
                };
                if snapshot.revision < newest_revision {
                    continue;
                }
                newest_revision = snapshot.revision;
                let alive = this.update(cx, |state, cx| {
                    if state.apply_artifacts_frame(&space_id, snapshot) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
            tracing::debug!(%space_id, "artifacts stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor()
                .timer(mast_harness::backoff::jittered(work_outline_retry_delay(0)))
                .await;
        }
    })
}

pub(super) fn spawn_change_request_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    target: ChangeRequestWatchKey,
    local_device_id: Option<String>,
    epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let mut failures: u32 = 0;
        loop {
            let params = watch_params(&target, local_device_id.as_deref());
            let seen_epoch = epoch.load(std::sync::atomic::Ordering::Relaxed);

            let mut subscription = match handle
                .client()
                .subscribe_checked(methods::WATCH_CHECKOUT_CHANGE_REQUEST, params)
                .await
            {
                Ok(subscription) => subscription,
                Err(RpcError::UnknownMethod(_)) => {
                    tracing::debug!(
                        device = %target.device_id,
                        "checkout change requests unsupported on device"
                    );
                    this.update(cx, |state, cx| {
                        let engine_version = state
                            .devices
                            .iter()
                            .find(|device| device.id == target.device_id)
                            .and_then(|device| device.version.clone());
                        state
                            .change_requests
                            .mark_unsupported(target.device_id.clone(), engine_version);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Err(err) => {
                    tracing::debug!(
                        device = %target.device_id,
                        cwd = %target.cwd,
                        error = %err,
                        "checkout change request watch unavailable; retrying"
                    );
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    let delay = change_request_retry_delay(&err, failures);
                    failures = failures.saturating_add(1);
                    sleep_until_epoch_changes(cx, delay, &epoch, seen_epoch).await;
                    continue;
                }
            };

            while let Some(value) = subscription.recv().await {
                failures = 0;
                let snapshot: CheckoutChangeRequestStatus = match serde_json::from_value(value) {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        tracing::warn!(
                            device = %target.device_id,
                            cwd = %target.cwd,
                            error = %err,
                            "dropping malformed checkout change request frame"
                        );
                        continue;
                    }
                };
                if this
                    .update(cx, |state, cx| {
                        state.change_requests.store(target.clone(), snapshot);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }

            // Preserve the latest successful snapshot during a transport gap.
            tracing::debug!(
                device = %target.device_id,
                cwd = %target.cwd,
                "checkout change request stream ended; resubscribing"
            );
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            let delay = mast_harness::backoff::jittered(change_request_retry_delay(
                &RpcError::Transport(String::new()),
                failures,
            ));
            failures = failures.saturating_add(1);
            cx.background_executor().timer(delay).await;
        }
    })
}

pub(super) fn spawn_watch<T: DeserializeOwned + 'static>(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    method: &'static str,
    apply: fn(&mut AppState, T) -> bool,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        // Resubscribe loop: these are the standing Sessions/Devices/Spaces
        // watches — a daemon restart ended the stream and a bare return froze
        // them for the rest of the app's life (remote Working dots staled out
        // to nothing after 45s, and Idle/Completed transitions from other
        // devices never arrived again — "the session never completes").
        let mut backoff = mast_harness::backoff::Backoff::new(
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(30),
        );
        loop {
            let mut rx = match handle
                .client()
                .subscribe(method, serde_json::json!({}))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::debug!(method, error = %err, "watch unavailable; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(backoff.next_delay()).await;
                    continue;
                }
            };
            while let Some(value) = rx.recv().await {
                backoff.reset();
                #[cfg(feature = "startup-bench")]
                if method == methods::WATCH_SPACES { startup_trace("spaces_frame_received"); }
                let parsed: T = match serde_json::from_value(value) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        tracing::warn!(method, error = %err, "dropping malformed watch frame");
                        continue;
                    }
                };
                #[cfg(feature = "startup-bench")]
                let update_span = crate::startup_measurement::MeasurementSpan::new(method);
                let alive = this.update(cx, |state, cx| {
                    let changed = apply(state, parsed);
                    state.apply_pending_deep_link(cx);
                    if changed {
                        if matches!(
                            method,
                            methods::WATCH_SPACES
                                | methods::WATCH_DEVICES
                                | methods::WATCH_CHAT_DIFF_TARGETS
                        ) {
                            state.reconcile_change_request_watches(cx);
                        }
                        if method == methods::WATCH_SPACES {
                            state.reconcile_work_outline_watches(cx);
                        }
                        cx.notify();
                    }
                });
                #[cfg(feature = "startup-bench")]
                drop(update_span);
                if alive.is_err() {
                    return;
                }
            }
            tracing::debug!(method, "watch stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(backoff.next_delay()).await;
        }
    })
}

/// Best-effort `LocalDevice` probe: fills `local_device_id` for the "This
/// device" badge. Engines that don't serve the method leave it `None`.
pub(super) fn spawn_local_device_probe(cx: &mut Context<AppState>, handle: EngineHandle) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let Ok(value) = handle
            .client()
            .call("LocalDevice", serde_json::json!({}))
            .await
        else {
            tracing::debug!("LocalDevice unavailable; skipping this-device badge");
            return;
        };
        let id = value
            .get("id")
            .or_else(|| value.get("deviceId"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(id) = id {
            this.update(cx, |state, cx| {
                state.local_device_id = Some(id);
                state.apply_pending_deep_link(cx);
                // Watches opened before this probe conservatively route through
                // targetDeviceId. Recreate them now that local routing is known.
                state.change_request_tasks.clear();
                state.reconcile_change_request_watches(cx);
                cx.notify();
            })
            .ok();
        }
    })
}

pub(super) fn spawn_transcript_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    chat_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        // Outer loop: a delta desync (missed frame) resubscribes immediately
        // and the fresh stream's opening reset heals the copy; a subscribe
        // failure, malformed frame, or stream end retries on a delay. Every
        // path re-enters the loop — a return here freezes the transcript
        // with no banner and no heal short of an app restart (this watch and
        // its engine-side room are the ONLY transcript delivery path). The
        // task itself is dropped by select_chat/apply_chats when the chat is
        // deselected or deleted, so retrying can't outlive relevance.
        let mut backoff = mast_harness::backoff::Backoff::new(
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(30),
        );
        'resubscribe: loop {
            let params = serde_json::json!({ "chatId": chat_id, "openingTail": true });
            let mut rx = match handle
                .client()
                .subscribe(methods::WATCH_DOC_MESSAGES, params)
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::warn!(%chat_id, error = %err, "transcript watch failed; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(backoff.next_delay()).await;
                    continue 'resubscribe;
                }
            };
            let mut preparation = WatchPreparation::new(cx.background_executor().clone());
            while let Some(value) = rx.recv().await {
                let history_pending = value
                    .get("historyPending")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if history_pending {
                    // A complete cached transcript is already on screen. The
                    // opening tail cannot replace it and the following full
                    // reset initializes the preparation worker on its own.
                    let cached = this.update(cx, |state, _| {
                        state.selected_chat.as_deref() == Some(chat_id.as_str())
                            && state.transcript_replayed
                    });
                    match cached {
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(_) => return,
                    }
                }
                let decoded = cx
                    .background_executor()
                    .spawn(async move {
                        let update: mast_doc::TranscriptUpdate =
                            serde_json::from_value(value).map_err(|e| e.to_string())?;
                        let prepared = preparation.prepare(&update).map_err(|e| e.to_string())?;
                        Ok::<_, String>((update, prepared, preparation))
                    })
                    .await;
                let (update, prepared, next_preparation) = match decoded {
                    Ok(frame) => frame,
                    Err(err) => {
                        // Schema skew (a newer peer's entry shape arriving
                        // through sync): a skipped frame is a silently stale
                        // copy, so resubscribe for a fresh reset — delayed,
                        // in case the reset itself is what can't parse.
                        tracing::warn!(error = %err, "malformed transcript frame; resubscribing");
                        cx.background_executor().timer(backoff.next_delay()).await;
                        continue 'resubscribe;
                    }
                };
                preparation = next_preparation;
                backoff.reset();
                let mut desync = false;
                let alive = this.update(cx, |state, cx| {
                    // Guard against a stale pump racing a newer selection.
                    if state.selected_chat.as_deref() == Some(chat_id.as_str())
                        && let Err(err) = state.receive_prepared_opening_transcript_update(
                            &chat_id,
                            update,
                            prepared,
                            history_pending,
                            cx,
                        ) {
                            tracing::warn!(%chat_id, error = %err, "resubscribing transcript");
                            desync = true;
                        }
                });
                if alive.is_err() {
                    return;
                }
                if desync {
                    continue 'resubscribe;
                }
            }
            // Stream ended: engine restart, RPC drop, or chat purge. Retry;
            // the purge case is cleaned up by apply_chats dropping this task.
            tracing::debug!(%chat_id, "transcript stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(backoff.next_delay()).await;
        }
    })
}

/// The selected chat's pending-message queue, straight off its doc. Whole-list
/// frames (the queue is a handful of rows at most), retried like the transcript
/// watch — a queue that silently stopped updating would show messages the host
/// has already sent.
pub(super) fn spawn_queue_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    chat_id: String,
) -> Task<()> {
    #[derive(serde::Deserialize)]
    struct QueueFrame {
        #[serde(default)]
        items: Vec<mast_doc::QueuedMessage>,
        /// Additive: older hosts omit it.
        #[serde(default, rename = "rejectedSends")]
        rejected_sends: Vec<mast_proto::InstructionRejectedSend>,
    }
    cx.spawn(async move |this, cx| {
        let mut backoff = mast_harness::backoff::Backoff::new(
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(30),
        );
        'resubscribe: loop {
            let params = serde_json::json!({ "chatId": chat_id });
            let mut rx = match handle
                .client()
                .subscribe(methods::WATCH_QUEUE, params)
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::debug!(%chat_id, error = %err, "queue watch failed; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(backoff.next_delay()).await;
                    continue 'resubscribe;
                }
            };
            while let Some(value) = rx.recv().await {
                backoff.reset();
                let frame: QueueFrame = match serde_json::from_value(value) {
                    Ok(frame) => frame,
                    Err(err) => {
                        tracing::warn!(error = %err, "dropping malformed queue frame");
                        continue;
                    }
                };
                let alive = this.update(cx, |state, cx| {
                    // Guard against a stale pump racing a newer selection.
                    if state.selected_chat.as_deref() == Some(chat_id.as_str()) {
                        state.apply_queue_frame(&chat_id, frame.items, frame.rejected_sends);
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(backoff.next_delay()).await;
        }
    })
}

pub(super) fn subagent_watch_terminal(error: &RpcError) -> bool {
    matches!(error, RpcError::BadParams(_) | RpcError::UnknownMethod(_))
        || matches!(error, RpcError::Failed(message) if message.to_ascii_lowercase().contains("not found"))
}

/// [`spawn_transcript_watch`]'s shape, writing into `sub_transcripts[doc_id]`
/// instead of the selected chat's transcript. The apply guard is per key so a
/// subagent tab can outlive chat switches.
pub(super) fn spawn_subagent_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    doc_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let mut retry = mast_harness::backoff::Backoff::new(std::time::Duration::from_secs(2), std::time::Duration::from_secs(30));
        'resubscribe: loop {
            let params = serde_json::json!({ "chatId": doc_id });
            let mut rx = match handle
                .client()
                .subscribe_checked(methods::WATCH_DOC_MESSAGES, params)
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    let terminal = subagent_watch_terminal(&err);
                    tracing::warn!(%doc_id, error = %err, terminal, "subagent watch failed");
                    if terminal || this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(retry.next_delay()).await;
                    continue 'resubscribe;
                }
            };
            let mut preparation = WatchPreparation::new(cx.background_executor().clone());
            while let Some(value) = rx.recv().await {
                let decoded = cx
                    .background_executor()
                    .spawn(async move {
                        let update: mast_doc::TranscriptUpdate =
                            serde_json::from_value(value).map_err(|e| e.to_string())?;
                        let prepared = preparation.prepare(&update).map_err(|e| e.to_string())?;
                        Ok::<_, String>((update, prepared, preparation))
                    })
                    .await;
                let (update, prepared, next_preparation) = match decoded {
                    Ok(frame) => frame,
                    Err(err) => {
                        tracing::warn!(error = %err, "malformed subagent frame; resubscribing");
                        cx.background_executor().timer(retry.next_delay()).await;
                        continue 'resubscribe;
                    }
                };
                preparation = next_preparation;
                let mut desync = false;
                let alive = this.update(cx, |state, cx| {
                    // A stale pump racing a snapshot/unwatch finds no key.
                    if let Some(rows) = state.sub_transcripts.get_mut(&doc_id) {
                        let frame = update.frame;
                        let text_only = is_text_append(&frame);
                        state.transcript_revision = state.transcript_revision.wrapping_add(1);
                        if let Err(err) = mast_doc::apply_transcript_frame(rows, frame) {
                            tracing::warn!(%doc_id, error = %err, "resubscribing subagent watch");
                            desync = true;
                        }
                        if !desync {
                            state.prepared_transcripts.insert(doc_id.clone(), prepared);
                        }
                        if !desync && let Some(baseline) = update.replay_baseline {
                            state
                                .transcript_baselines
                                .insert(doc_id.clone(), Arc::new(baseline));
                        }
                        if text_only && !desync {
                            cx.emit(TranscriptTextChanged {
                                doc_id: doc_id.clone(),
                            });
                        } else {
                            cx.notify();
                        }
                    }
                });
                if alive.is_err() {
                    return;
                }
                if desync {
                    cx.background_executor().timer(retry.next_delay()).await;
                    continue 'resubscribe;
                }
                retry.reset();
            }
            tracing::debug!(%doc_id, "subagent stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(retry.next_delay()).await;
        }
    })
}

