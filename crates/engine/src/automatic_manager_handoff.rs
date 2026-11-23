//! Automatic context-window handoff for locally hosted standing roles.
//!
//! A Manager's normal completed turn at 80% requests a status checkpoint; a
//! subsequent completed checkpoint turn hands off only after a newer status is
//! visible. At 92% the last available status is used immediately. An Assistant
//! has no status artifact: it hands off on the first idle turn at 80%, and its
//! successor reads the preceding chat.

use std::collections::{HashMap, HashSet};

use mast_proto::{Session, SessionStatus, StandingRole, StandingRoleKind, StandingRoleState, WorkItemLaunchState};
use mast_proto::work_outline::WorkOutlineErrorCode;
use tokio_util::sync::CancellationToken;

use crate::artifacts::ArtifactsService;
use crate::briefing::{self, BriefingKind, BriefingQuery, ProjectSubtreeScope};
use crate::orchestrator_wakes::OrchestratorWakes;
use crate::sessions::SessionsEngine;
use crate::work_outline_launch::WorkItemLaunchService;
use crate::{DocHost, WorkspaceHost};

/// One-second tries of a failing credit hand-off before the user is told.
const CREDIT_HANDOFF_ATTEMPTS: u32 = 60;

use crate::handoff_roles::ManagerCheckpoint;

/// Forget the checkpoint of `chat` in memory and in its durable store. On a
/// store failure the record is already gone from the store's memory, so the
/// next successful save of any record rewrites the file without it.
fn forget_checkpoint(
    tokens: &crate::agent_tools::AgentTokens,
    checkpoints: &mut HashMap<String, ManagerCheckpoint>,
    chat: &str,
) {
    checkpoints.remove(chat);
    if let Err(error) = tokens.pending_handoffs.forget_checkpoint(chat) {
        tracing::warn!(%chat, %error, "Manager handoff checkpoint could not be cleared");
    }
}

pub(crate) fn start(
    workspace: WorkspaceHost,
    host: DocHost,
    sessions: SessionsEngine,
    artifacts: ArtifactsService,
    wakes: OrchestratorWakes,
    launch: WorkItemLaunchService,
) -> CancellationToken {
    start_inner(workspace, host, sessions, artifacts, wakes, launch, None, None)
}

#[cfg(debug_assertions)]
pub(crate) fn start_paused(
    workspace: WorkspaceHost, host: DocHost, sessions: SessionsEngine,
    artifacts: ArtifactsService, wakes: OrchestratorWakes, launch: WorkItemLaunchService,
) -> (CancellationToken, tokio::sync::oneshot::Sender<()>) {
    let (resume, paused) = tokio::sync::oneshot::channel();
    (start_inner(workspace, host, sessions, artifacts, wakes, launch, Some(paused), None), resume)
}

#[cfg(debug_assertions)]
pub(crate) fn start_with_timer_paused(
    workspace: WorkspaceHost, host: DocHost, sessions: SessionsEngine,
    artifacts: ArtifactsService, wakes: OrchestratorWakes, launch: WorkItemLaunchService,
) -> (CancellationToken, tokio::sync::oneshot::Receiver<()>, tokio::sync::oneshot::Sender<()>) {
    let (selected, selected_rx) = tokio::sync::oneshot::channel();
    let (resume, paused) = tokio::sync::oneshot::channel();
    (start_inner(workspace, host, sessions, artifacts, wakes, launch, None, Some((selected, paused))), selected_rx, resume)
}

fn start_inner(
    workspace: WorkspaceHost, host: DocHost, sessions: SessionsEngine,
    artifacts: ArtifactsService, wakes: OrchestratorWakes, launch: WorkItemLaunchService,
    paused: Option<tokio::sync::oneshot::Receiver<()>>,
    timer_pause: Option<(tokio::sync::oneshot::Sender<()>, tokio::sync::oneshot::Receiver<()>)>,
) -> CancellationToken {
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let mut rx = sessions.watch_sessions();
    // Capture before spawning: a first turn that finishes before the spawned
    // task is polled must not be mistaken for a restart-era completion.
    let mut previous: HashMap<String, Option<String>> = rx.borrow().iter()
        .map(|session| (session.chat_id.clone(), session.last_completed_turn.clone()))
        .collect();
    // Role adoption can update only the standing-role row, without changing
    // either the chat list or the session watch.
    let mut registry_rx = workspace.watch_changes();
    tokio::spawn(async move {
        let mut timer_pause = timer_pause;
        if let Some(paused) = paused {
            tokio::select! { _ = stopped.cancelled() => return, _ = paused => {} }
        }
        let mut recheck = tokio::time::interval(std::time::Duration::from_secs(1));
        recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let tokens = sessions.agent_tokens();
        // A Manager already asked for its status is not asked again after a
        // restart; its next completed turn is judged against the saved one.
        let mut checkpoints: HashMap<String, ManagerCheckpoint> = tokens.pending_handoffs.checkpoints();
        // Any completion this run sees follows the saved one, whatever ids
        // the harness assigned it: turn ids need not stay unique across runs.
        // A chat stays in this set for the run, since ids may repeat again.
        let mut restored: HashSet<String> = checkpoints.keys().cloned().collect();
        let mut deferred = HashSet::new();
        let mut retries = HashMap::<String, tokio::time::Instant>::new();
        let mut failures = HashMap::<String, u32>::new();
        let mut rescan_all = true;
        loop {
            if stopped.is_cancelled() { break; }
            // Durable intents are independent of the current completion and
            // of the session-watch baseline captured at startup.
            let pending = tokens.pending_handoffs.automatic_intents();
            let pending_sources: HashSet<_> = pending.iter().map(|intent| intent.source.as_str()).collect();
            // A Manager that was archived or superseded without this
            // watcher's hand-off (a manual one) never completes another turn
            // to clear its own record. Only the chat's own row counts as
            // evidence: an unknown chat is kept, and so is a chat whose role
            // row has not synced yet, since a superseded chat never holds a
            // role again but a role-less one may simply be early.
            let stale: Vec<String> = checkpoints.keys()
                .filter(|chat| workspace.chat(chat).ok().flatten()
                    .is_some_and(|known| known.archived || known.is_standing_role_predecessor()))
                .cloned().collect();
            for chat in stale {
                forget_checkpoint(&tokens, &mut checkpoints, &chat);
            }
            for intent in &pending {
                let committed = launch.automatic_handoff_committed(intent);
                // Checked before the parked branch so a parked credit intent
                // is dropped too once the chat has moved on.
                if let Some(entry) = intent.credit_entry.as_deref()
                    && !committed
                    && sessions.moved_past_error(&intent.source, entry)
                {
                    // A manual Retry (or anything else) moved the chat past
                    // this error before the hand-off committed: the Manager
                    // keeps its role and the record is finished.
                    crate::credit_fallback::finish_superseded(&workspace, &intent.source, entry);
                    failures.remove(&intent.source);
                    if let Err(error) = tokens.pending_handoffs.settle_automatic(
                        &intent.source, &intent.request.operation_id, false)
                    {
                        tracing::warn!(chat = %intent.source, %error, "superseded credit hand-off intent could not be cleared");
                    }
                    continue;
                }
                if intent.parked && !committed {
                    // A parked intent whose source lost the role, or no
                    // longer exists, can never finish. Drop it so the source
                    // is scanned again instead of being excluded forever.
                    if !launch.automatic_handoff_still_owned(intent) {
                        failures.remove(&intent.source);
                        if let Err(error) = tokens.pending_handoffs.settle_automatic(
                            &intent.source, &intent.request.operation_id, false)
                        {
                            tracing::warn!(chat = %intent.source, %error, "stale automatic handoff intent could not be cleared");
                        } else {
                            // A manual hand-off took the role: whatever
                            // reached the source while the intent held it
                            // belongs to that successor.
                            host.forward_handed_off_queue_soon(&intent.source);
                        }
                    } else if let Some(entry) = intent.credit_entry.as_deref() {
                        // Parked and still owned: nothing retries it, so the
                        // user hears of it (once per record).
                        crate::credit_fallback::report_stuck(&workspace, &launch, &intent.source, entry,
                            "the hand-off was paused by a conflicting change");
                    }
                    continue;
                }
                let now = tokio::time::Instant::now();
                if retries.get(&intent.source).is_some_and(|retry| *retry > now) {
                    continue;
                }
                let Ok(handle) = host.open(&intent.source) else { continue; };
                let Ok(_source_queue) = handle.drain_lock.try_lock() else { continue; };
                // An errored source never drains its queue, so a credit
                // hand-off waits only for the failed turn to be over.
                let ready = if intent.credit_entry.is_some() {
                    !sessions.turn_in_flight(&intent.source)
                } else {
                    source_ready(&sessions, &handle)
                };
                if !committed && launch.automatic_handoff_still_owned(intent) && !ready {
                    continue;
                }
                if tokens.manager_status().test_hook("automatic_handoff_attempt", &intent.source) {
                    continue;
                }
                retries.insert(intent.source.clone(), now + std::time::Duration::from_secs(1));
                let result = launch.resume_automatic_handoff(intent, &wakes).await;
                let operation = &intent.request.operation_id;
                match result {
                    Ok(WorkItemLaunchState::Started) => {
                        // Messages queued on the stalled Manager after its
                        // prompt was written go to the successor before the
                        // intent is cleared; a failure retries next tick.
                        // Messages the intent already put in the successor's
                        // prompt are dropped from the source, not sent twice.
                        if intent.credit_entry.is_some()
                            && !matches!(
                                host.forward_queue_to_successor(&handle, Some(&intent.carried)).await,
                                crate::doc_host::QueueForward::Done | crate::doc_host::QueueForward::NotHandedOff
                            )
                        {
                            continue;
                        }
                        if let Err(error) = tokens.pending_handoffs.settle_automatic(&intent.source, operation, false) {
                            tracing::warn!(chat = %intent.source, %error, "completed automatic handoff intent could not be cleared");
                        } else {
                            // The source was held while the intent was pending;
                            // whatever reached it meanwhile moves now.
                            host.forward_handed_off_queue_soon(&intent.source);
                        }
                        forget_checkpoint(&tokens, &mut checkpoints, &intent.source);
                        failures.remove(&intent.source);
                    }
                    Ok(WorkItemLaunchState::Queued) => {}
                    Ok(WorkItemLaunchState::Rejected) => {
                        let _ = notify_handoff_retry(&handle, operation,
                            "The successor’s first turn was rejected. Mast will retry this handoff with the same successor.");
                    }
                    Err(error) => {
                        tracing::warn!(chat = %intent.source, %error, "automatic standing-role handoff did not complete");
                        // A credit hand-off that keeps failing has no one else
                        // watching it: say so after a minute of tries.
                        let tries = failures.entry(intent.source.clone()).or_default();
                        *tries += 1;
                        if *tries == CREDIT_HANDOFF_ATTEMPTS
                            && let Some(entry) = intent.credit_entry.as_deref()
                        {
                            crate::credit_fallback::report_stuck(&workspace, &launch, &intent.source, entry, &error.message);
                        }
                        if error.code == WorkOutlineErrorCode::OperationConflict && !launch.automatic_handoff_committed(intent) {
                            if notify_handoff_retry(&handle, operation,
                                &format!("Automatic handoff paused. {}", error.message)).is_ok()
                                && host.persist_chat_now(&intent.source).is_ok()
                                && let Err(error) = tokens.pending_handoffs.settle_automatic(&intent.source, operation, true)
                            {
                                tracing::warn!(chat = %intent.source, %error, "automatic handoff could not be parked");
                            }
                        }
                    }
                }
            }
            retries.retain(|source, _| pending_sources.contains(source.as_str()));
            // A deferred-only pass does not visit every session. Do not mark
            // a concurrently arrived watch version seen until a full pass.
            let rows: Vec<Session> = if rescan_all { rx.borrow_and_update().clone() } else { rx.borrow().clone() };
            // One registry decode per pass, and only when a candidate needs it.
            let mut roles_by_chat: Option<HashMap<String, StandingRole>> = None;
            for session in rows {
                if pending_sources.contains(session.chat_id.as_str()) {
                    continue;
                }
                if !rescan_all && !deferred.contains(&session.chat_id) { continue; }
                let old = previous.get(&session.chat_id);
                let Some(turn) = session.last_completed_turn.as_deref() else {
                    continue;
                };
                if session.status != SessionStatus::Idle
                    || old.and_then(|value| value.as_deref()) == Some(turn)
                {
                    continue;
                }
                let roles = roles_by_chat.get_or_insert_with(|| {
                    workspace.standing_roles().into_iter()
                        .filter_map(|role| Some((role.current_chat_id.clone()?, role)))
                        .collect()
                });
                let Some(role) = roles.get(&session.chat_id).cloned() else {
                    deferred.remove(&session.chat_id);
                    continue;
                };
                if role.current_chat_id.as_deref() != Some(session.chat_id.as_str()) {
                    deferred.remove(&session.chat_id);
                    continue;
                }
                deferred.insert(session.chat_id.clone());
                if role.state != StandingRoleState::Idle { continue; }
                let Ok(source_handle) = host.open(&session.chat_id) else { continue; };
                // A drain can have removed a user row but not yet started its
                // turn. Hold the same gate through the role rotation so a
                // composer send cannot land on the predecessor in between.
                // Do not let one busy Manager's queue stall every other role.
                // The deferred set retries this candidate on the next tick.
                let Ok(_source_queue) = source_handle.drain_lock.try_lock() else { continue; };
                // The role may have changed while waiting for the queue gate.
                if workspace.standing_role_of_chat(&session.chat_id).as_ref() != Some(&role) { continue; }
                let Some(chat) = workspace.chat(&session.chat_id).ok().flatten() else {
                    continue;
                };
                if chat.device_id != workspace.device_id() || chat.archived {
                    deferred.remove(&session.chat_id);
                    continue;
                }
                // Let the current Manager consume queued input before rotating
                // its role. An unreadable queue must also defer the handoff.
                if !source_ready(&sessions, &source_handle) {
                    continue;
                }
                let Some(space) = chat.space_id.as_deref() else {
                    continue;
                };
                let manager = role.role == StandingRoleKind::Manager;
                let status = || {
                    briefing::read(
                        &artifacts,
                        &ProjectSubtreeScope::new(space, &role.scope_item_id),
                        &BriefingQuery::new(BriefingKind::ManagerStatus),
                        space,
                    )
                    .ok()
                    .and_then(|entries| entries.into_iter().next())
                };
                let context = host
                    .open(&session.chat_id)
                    .ok()
                    .and_then(|handle| handle.doc().context_usage());
                let usage = context.as_ref().and_then(|value| value.fraction());
                if usage.is_none() {
                    let id = mast_proto::derive_id(&session.chat_id, &format!("{}-context-window-unavailable", role.role.as_str()));
                    let label = if manager { "Manager" } else { "assistant" };
                    if let Err(error) = source_handle.write_system_notice(
                        &id,
                        &format!("[Mast system message: automatic {label} handoff is unavailable because this session did not report its context window. You can hand off manually.]"),
                        chrono::Utc::now().timestamp_millis(),
                    ) {
                        tracing::warn!(chat = %session.chat_id, %error, "could not report missing context window");
                        continue;
                    }
                    deferred.remove(&session.chat_id);
                    previous.insert(session.chat_id.clone(), Some(turn.into()));
                    continue;
                }
                // The request was for a context near its limit. Once usage has
                // fallen below the threshold (a reset or compaction), a newer
                // status must not hand the Manager off, and the next climb
                // asks again.
                if usage.is_some_and(|fraction| fraction < 0.80) && checkpoints.contains_key(&session.chat_id) {
                    forget_checkpoint(&tokens, &mut checkpoints, &session.chat_id);
                }
                let checkpoint = checkpoints.get(&session.chat_id);
                let critical = usage.is_some_and(|fraction| fraction >= 0.92);
                let ready = if critical {
                    true
                } else if !manager {
                    usage.is_some_and(|fraction| fraction >= 0.80)
                } else if let Some(checkpoint) = checkpoint {
                    (restored.contains(&session.chat_id) || turn != checkpoint.turn)
                        && status().is_some_and(|entry| {
                            entry.revision > checkpoint.revision && entry.content.is_some()
                        })
                } else {
                    false
                };
                if !ready {
                    if checkpoint.is_none()
                        && usage.is_some_and(|fraction| fraction >= 0.80 && fraction < 0.92)
                    {
                        let baseline = status().map_or(0, |entry| entry.revision);
                        let id = mast_proto::derive_id(
                            &format!("{}:{turn}", session.chat_id),
                            "manager-handoff-checkpoint",
                        );
                        match host.queue_context_message(
                            &session.chat_id,
                            &id,
                            &mast_proto::manager_status_nudge_text(baseline),
                        ) {
                            Ok(_) => {
                                let checkpoint = ManagerCheckpoint { turn: turn.into(), revision: baseline };
                                // Saved after the nudge is queued: a crash in
                                // between asks twice rather than not at all. A save
                                // failure still holds for this run.
                                if let Err(error) = tokens.pending_handoffs.remember_checkpoint(&session.chat_id, checkpoint.clone()) {
                                    tracing::warn!(chat = %session.chat_id, %error, "Manager handoff checkpoint could not be saved");
                                }
                                restored.remove(&session.chat_id);
                                checkpoints.insert(session.chat_id.clone(), checkpoint);
                            }
                            Err(error) => {
                                tracing::warn!(chat = %session.chat_id, %error, "Manager handoff checkpoint could not be queued");
                                continue;
                            }
                        }
                    }
                    deferred.remove(&session.chat_id);
                    previous.insert(session.chat_id.clone(), Some(turn.into()));
                    // Tells a test that this completion has been judged and
                    // found not ready, so it can assert what did not happen.
                    tokens.manager_status().test_hook("manager_handoff_evaluated", &session.chat_id);
                    continue;
                }
                let prompt = if manager {
                    let status = status().and_then(|entry| entry.content)
                        .unwrap_or_else(|| no_status_text(&host, &session.chat_id));
                    format!("Continue this project using the latest Manager status:\n\n{status}")
                } else {
                    format!("Continue the personal assistant role. Read the preceding chat {} for its conversation and decisions, then pick up where it left off.", session.chat_id)
                };
                let observations = crate::handoff_roles::RoleObservations::capture(
                    &workspace,
                    &tokens,
                    &chat,
                );
                let request = mast_proto::CreateChatForWorkItemParams {
                    work_item_id: role.scope_item_id.clone(),
                    initial_prompt: mast_proto::WorkItemPrompt {
                        text: prompt,
                        attachments: vec![],
                    },
                    config: chat.config.clone(),
                    execution_override: None,
                    worktree_choice: None,
                    operation_id: mast_proto::derive_id(
                        &format!("{}:{turn}", session.chat_id),
                        &format!("automatic-{}-handoff", role.role.as_str()),
                    ),
                    parent_chat_id: None,
                    target_device_id: None,
                    account_id: None,
                    standing_role: None,
                };
                match launch.prepare_automatic_handoff(&request, &session.chat_id, space, observations).await {
                    Ok(intent) => {
                        if let Err(error) = tokens.pending_handoffs.remember_automatic(intent) {
                            tracing::warn!(chat = %session.chat_id, %error, "automatic handoff intent could not be saved");
                            continue;
                        }
                        deferred.remove(&session.chat_id);
                        previous.insert(session.chat_id.clone(), Some(turn.into()));
                    }
                    Err(error) => {
                        tracing::warn!(chat = %session.chat_id, %error, "automatic handoff intent could not be prepared");
                    }
                }
            }
            rescan_all = tokio::select! {
                _ = stopped.cancelled() => break,
                _ = recheck.tick() => {
                    if let Some((selected, paused)) = timer_pause.take() {
                        let _ = selected.send(());
                        tokio::select! { _ = stopped.cancelled() => break, _ = paused => {} }
                    }
                    false
                },
                result = rx.changed() => { if result.is_err() { break; } true },
                result = registry_rx.changed() => { if result.is_err() { break; } true },
            };
        }
    });
    cancel
}


/// The latest Manager status of a project, if it has readable content.
pub(crate) fn manager_status(artifacts: &ArtifactsService, space: &str, scope_item_id: &str) -> Option<String> {
    briefing::read(
        artifacts,
        &ProjectSubtreeScope::new(space, scope_item_id),
        &BriefingQuery::new(BriefingKind::ManagerStatus),
        space,
    )
    .ok()
    .and_then(|entries| entries.into_iter().next())
    .and_then(|entry| entry.content)
}

/// What a successor is told when its predecessor left no readable status:
/// the predecessor's recent conversation and submitted prompts.
pub(crate) fn no_status_text(host: &DocHost, chat_id: &str) -> String {
    let recent = host.open(chat_id).ok()
        .and_then(|handle| handle.doc().read_entries().ok())
        .unwrap_or_default().into_iter().rev()
        .filter(|entry| matches!(entry.role, mast_doc::MessageRole::User | mast_doc::MessageRole::Assistant))
        .take(10)
        .flat_map(|entry| {
            let role = format!("{:?}", entry.role);
            entry.parts.into_iter().filter_map(move |part| match part {
                mast_doc::MessagePart::Text { text, .. } => Some(format!("{role}: {}", text.chars().take(2000).collect::<String>())),
                _ => None,
            })
        }).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n\n");
    // A completed turn may precede transcript folding. Its
    // durable command still carries the submitted prompt.
    let commands = host.open(chat_id).ok()
        .and_then(|handle| handle.doc().read_commands().ok()).unwrap_or_default()
        .into_iter().rev().filter_map(|command| match command.payload {
            mast_doc::SessionCommandPayload::Run { request, .. }
            | mast_doc::SessionCommandPayload::RunWithInstructions { request, .. } => Some(request.prompt),
            mast_doc::SessionCommandPayload::Steer { prompt, .. } => Some(prompt),
            _ => None,
        }).take(5).map(|text| text.chars().take(2000).collect::<String>())
        .collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n\n");
    format!("No readable Manager status is available. Review the prior chat for its decisions. Recent conversation:\n{recent}\n\nRecent submitted prompts:\n{commands}")
}

fn source_ready(sessions: &SessionsEngine, handle: &crate::doc_host::ChatDocHandle) -> bool {
    let chat_id = handle.chat_id();
    !sessions.turn_in_flight(chat_id)
        && !sessions.has_queued_steer_recovery(chat_id)
        && handle.doc().read_queue().is_ok_and(|queue| queue.is_empty())
        && handle.doc().read_commands().is_ok_and(|commands| commands.iter()
            .all(|command| command.status != mast_doc::SessionCommandStatus::Pending))
}

fn notify_handoff_retry(
    source: &crate::doc_host::ChatDocHandle, operation: &str, message: &str,
) -> Result<(), mast_doc::DocError> {
    let id = mast_proto::derive_id(operation, message);
    source.write_system_notice(
        &id,
        &format!("[Mast system message: {message}]"),
        chrono::Utc::now().timestamp_millis(),
    )
}
