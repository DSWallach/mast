//! Automatic context-window handoff for locally hosted project Managers.
//!
//! A normal completed turn at 80% requests a status checkpoint; a subsequent
//! completed checkpoint turn hands off only after a newer status is visible.
//! At 92% the last available status is used immediately.

use std::collections::{HashMap, HashSet};

use mast_proto::{Session, SessionStatus, StandingRoleKind, StandingRoleState};
use tokio_util::sync::CancellationToken;

use crate::artifacts::ArtifactsService;
use crate::briefing::{self, BriefingKind, BriefingQuery, ProjectSubtreeScope};
use crate::orchestrator_wakes::OrchestratorWakes;
use crate::sessions::SessionsEngine;
use crate::work_outline_launch::{HandOffOrigin, WorkItemLaunchService};
use crate::{DocHost, WorkspaceHost};

struct Checkpoint {
    turn: String,
    revision: u32,
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
        let mut checkpoints: HashMap<String, Checkpoint> = HashMap::new();
        let mut deferred = HashSet::new();
        let mut rescan_all = true;
        loop {
            if stopped.is_cancelled() { break; }
            // A deferred-only pass does not visit every session. Do not mark
            // a concurrently arrived watch version seen until a full pass.
            let rows: Vec<Session> = if rescan_all { rx.borrow_and_update().clone() } else { rx.borrow().clone() };
            for session in rows {
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
                let Some(role) = workspace.standing_role_of_chat(&session.chat_id) else {
                    deferred.remove(&session.chat_id);
                    continue;
                };
                if role.role != StandingRoleKind::Manager
                    || role.current_chat_id.as_deref() != Some(session.chat_id.as_str())
                {
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
                if sessions.turn_in_flight(&session.chat_id)
                    || sessions.has_queued_steer_recovery(&session.chat_id)
                    || source_handle.doc().read_queue().ok()
                        .is_none_or(|queue| !queue.is_empty())
                    || source_handle.doc().read_commands().ok().is_none_or(|commands| commands.iter().any(|command| command.status == mast_doc::SessionCommandStatus::Pending))
                {
                    continue;
                }
                let Some(space) = chat.space_id.as_deref() else {
                    continue;
                };
                deferred.remove(&session.chat_id);
                previous.insert(session.chat_id.clone(), session.last_completed_turn.clone());
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
                    let id = mast_proto::derive_id(&session.chat_id, "manager-context-window-unavailable");
                    if let Err(error) = source_handle.write_system_notice(
                        &id,
                        "[Mast system message: automatic Manager handoff is unavailable because this session did not report its context window. You can hand off manually.]",
                        chrono::Utc::now().timestamp_millis(),
                    ) {
                        tracing::warn!(chat = %session.chat_id, %error, "could not report missing Manager context window");
                    }
                    continue;
                }
                let checkpoint = checkpoints.get(&session.chat_id);
                let critical = usage.is_some_and(|fraction| fraction >= 0.92);
                let ready = if critical {
                    true
                } else if let Some(checkpoint) = checkpoint {
                    turn != checkpoint.turn
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
                                checkpoints.insert(
                                    session.chat_id.clone(),
                                    Checkpoint {
                                        turn: turn.into(),
                                        revision: baseline,
                                    },
                                );
                            }
                            Err(error) => {
                                tracing::warn!(chat = %session.chat_id, %error, "Manager handoff checkpoint could not be queued")
                            }
                        }
                    }
                    continue;
                }
                let status = status().and_then(|entry| entry.content).unwrap_or_else(|| {
                    let recent = host.open(&session.chat_id).ok()
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
                    let commands = host.open(&session.chat_id).ok()
                        .and_then(|handle| handle.doc().read_commands().ok()).unwrap_or_default()
                        .into_iter().rev().filter_map(|command| match command.payload {
                            mast_doc::SessionCommandPayload::Run { request, .. }
                            | mast_doc::SessionCommandPayload::RunWithInstructions { request, .. } => Some(request.prompt),
                            mast_doc::SessionCommandPayload::Steer { prompt, .. } => Some(prompt),
                            _ => None,
                        }).take(5).map(|text| text.chars().take(2000).collect::<String>())
                        .collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n\n");
                    format!("No readable Manager status is available. Review the prior chat for its decisions. Recent conversation:\n{recent}\n\nRecent submitted prompts:\n{commands}")
                });
                let observations = crate::handoff_roles::RoleObservations::capture(
                    &workspace,
                    &sessions.agent_tokens(),
                    &chat,
                );
                let request = mast_proto::CreateChatForWorkItemParams {
                    work_item_id: role.scope_item_id.clone(),
                    initial_prompt: mast_proto::WorkItemPrompt {
                        text: format!(
                            "Continue this project using the latest Manager status:\n\n{status}"
                        ),
                        attachments: vec![],
                    },
                    config: chat.config.clone(),
                    execution_override: None,
                    worktree_choice: None,
                    operation_id: mast_proto::derive_id(
                        &format!("{}:{turn}", session.chat_id),
                        "automatic-manager-handoff",
                    ),
                    parent_chat_id: None,
                    target_device_id: None,
                    account_id: None,
                    standing_role: None,
                };
                match launch
                    .create_orchestrator_successor(
                        &request,
                        &wakes,
                        &session.chat_id,
                        HandOffOrigin::User {
                            archive: Some(true),
                            observed_roles: &observations,
                        },
                    )
                    .await
                {
                    Ok(_) => {
                        checkpoints.remove(&session.chat_id);
                    }
                    Err(error) => {
                        tracing::warn!(chat = %session.chat_id, %error, "automatic Manager handoff failed")
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
