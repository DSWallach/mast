//! Device-local recovery of user-authorized role transfers. The pending file
//! names the participants. Only a successful host reply or synced completion
//! proves that an attempted hand-off should transfer roles.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mast_proto::Chat;
use serde::{Deserialize, Serialize};

use crate::agent_tools::AgentTokens;
use crate::doc_host::DocHost;
use crate::handoff_role_file::write_json;
use crate::orchestrator_wakes::OrchestratorWakes;
use crate::{EngineError, WorkspaceHost};

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct PendingHandoff {
    source: String,
    requested_successor: String,
    device: String,
    space: String,
    archive: Option<bool>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct RoleObservations {
    admin: Option<crate::admin::StoredGrant>,
    assistant: Option<crate::personal_assistant::StoredAssistant>,
}

impl RoleObservations {
    pub(crate) fn capture(workspace: &WorkspaceHost, tokens: &AgentTokens, source: &Chat) -> Self {
        let chats = if source.scheduled_task_info.is_some() {
            workspace.read_chats().unwrap_or_default()
        } else {
            Vec::new()
        };
        Self {
            admin: source
                .space_id
                .as_deref()
                .and_then(|space| tokens.admin().observe_hand_off(&source.id, space)),
            assistant: tokens.personal_assistant().observe_hand_off(source, &chats),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct PendingRoleTransfer {
    #[serde(flatten)]
    intent: PendingHandoff,
    #[serde(default)]
    roles: RoleObservations,
    #[serde(default)]
    assistant_standing: bool,
}

impl PendingHandoff {
    fn with_successor(&self, successor: &str) -> Self {
        Self {
            requested_successor: successor.to_owned(),
            ..self.clone()
        }
    }

    pub(crate) fn new(
        source: &Chat,
        requested_successor: &str,
        archive: Option<bool>,
    ) -> Option<Self> {
        Some(Self {
            source: source.id.clone(),
            requested_successor: requested_successor.to_owned(),
            device: source.device_id.clone(),
            space: source.space_id.clone()?,
            archive,
        })
    }
}

#[derive(Default)]
struct State {
    path: Option<PathBuf>,
    pending: Vec<PendingRoleTransfer>,
}

impl State {
    fn replace(&mut self, pending: Vec<PendingRoleTransfer>) -> io::Result<()> {
        if let Some(path) = &self.path {
            write_json(path, &pending)?;
        }
        self.pending = pending;
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct PendingHandoffs(Mutex<State>);

impl PendingHandoffs {
    pub(crate) fn open(&self, path: PathBuf) -> io::Result<()> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.pending = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        state.path = Some(path);
        Ok(())
    }

    pub(crate) fn prepare(
        &self,
        intent: &PendingHandoff,
        roles: &RoleObservations,
        workspace: &WorkspaceHost,
    ) -> io::Result<()> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.pending.iter().any(|entry| &entry.intent == intent) {
            let mut pending = state.pending.clone();
            pending.push(PendingRoleTransfer {
                intent: intent.clone(),
                roles: roles.clone(),
                assistant_standing: workspace.standing_role_of_chat(&intent.source)
                    .is_some_and(|role| role.role == mast_proto::StandingRoleKind::Assistant),
            });
            state.replace(pending)?;
        }
        Ok(())
    }

    pub(crate) fn blocks_assistant_successor(&self, chat_id: &str) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).pending.iter().any(|entry|
            entry.assistant_standing && entry.intent.requested_successor == chat_id)
    }

    pub(crate) fn finish(
        &self,
        workspace: &WorkspaceHost,
        tokens: &AgentTokens,
        intent: &PendingHandoff,
        successor: &str,
    ) -> Result<(), EngineError> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let Some(requested) = state.pending.iter().find(|entry| &entry.intent == intent) else {
            return Ok(());
        };
        // A standing-role retry may use a fresh operation ID. The host's
        // reply identifies the original successor ahead of registry sync.
        // Use that original intent's observed roles when we sent it.
        let canonical = intent.with_successor(successor);
        let entry = state
            .pending
            .iter()
            .find(|entry| entry.intent == canonical)
            .unwrap_or(requested);
        let Some(source) = workspace.chat(&intent.source)?.filter(|chat| {
            chat.device_id == intent.device
                && chat.space_id.as_deref() == Some(intent.space.as_str())
        }) else {
            return Err(EngineError::Other(
                "Hand-off source no longer matches its device and Space".into(),
            ));
        };
        let chats = if source.scheduled_task_info.is_some() {
            workspace.read_chats()?
        } else {
            Vec::new()
        };
        tokens
            .admin()
            .transfer_for_hand_off(
                &source.id,
                successor,
                &intent.space,
                entry.roles.admin.as_ref(),
            )
            .map_err(|err| {
                EngineError::Other(format!("Could not save the admin hand-off: {err}"))
            })?;
        tokens
            .personal_assistant()
            .transfer_for_hand_off(&source, successor, &chats, entry.roles.assistant.as_ref())
            .map_err(|err| {
                EngineError::Other(format!(
                    "Could not save the personal assistant hand-off: {err}"
                ))
            })?;
        let mut pending = state.pending.clone();
        pending.retain(|entry| &entry.intent != intent && entry.intent != canonical);
        state.replace(pending)?;
        Ok(())
    }

    fn reconcile(&self, workspace: &WorkspaceHost, tokens: &AgentTokens) -> Vec<String> {
        let mut local_successors = Vec::new();
        let pending = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .clone();
        for PendingRoleTransfer { intent, .. } in pending {
            let Some(successor) = workspace.completed_user_hand_off_to(
                &intent.source,
                &intent.requested_successor,
                &intent.device,
                &intent.space,
                intent.archive,
            ) else {
                continue;
            };
            let result = workspace
                .persist_registry_now()
                .and_then(|()| self.finish(workspace, tokens, &intent, &successor));
            match result {
                Ok(()) if intent.device == workspace.device_id() => {
                    local_successors.push(successor)
                }
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!(%error, successor, "chat hand-off roles are still pending")
                }
            }
        }
        local_successors
    }
}

pub(crate) fn start_recovery(
    workspace: WorkspaceHost,
    doc_host: &DocHost,
    tokens: Arc<AgentTokens>,
    wakes: OrchestratorWakes,
    data_dir: &Path,
) -> io::Result<()> {
    let _ = data_dir;
    let mut changes = workspace.watch_changes();
    let mut local_successors = tokens.pending_handoffs.reconcile(&workspace, &tokens);
    let host = doc_host.clone();
    doc_host.spawn_worker(async move {
        loop {
            for successor in local_successors.drain(..) {
                wakes.finish_orchestrator_hand_off(&successor);
                host.kick_drains();
                wakes.evaluate(&successor, false).await;
            }
            tokio::select! {
                result = changes.changed() => if result.is_err() { break; },
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            }
            local_successors = tokens.pending_handoffs.reconcile(&workspace, &tokens);
        }
    });
    Ok(())
}
