//! Device-local recovery of user-authorized role transfers. The pending file
//! names the participants. Only a successful host reply or synced completion
//! proves that an attempted hand-off should transfer roles.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mast_proto::Chat;
use serde::{Deserialize, Serialize};

use crate::agent_tools::AgentTokens;
use crate::doc_host::DocHost;
use crate::handoff_role_file::write_json;
use crate::living_docs::LivingDocStore;
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

/// Immutable host-local intent written before an automatic handoff can
/// reserve a role, move workers, or queue its first turn.
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct AutomaticHandoff {
    pub source: String,
    pub request: mast_proto::CreateChatForWorkItemParams,
    pub execution: mast_proto::ResolvedExecutionConfiguration,
    pub instructions: mast_proto::EffectiveInstructions,
    pub worker_appendix: String,
    pub observations: RoleObservations,
    #[serde(default)]
    pub parked: bool,
    /// The error entry of the usage-limit failure this hand-off answers. Set
    /// only for a credit fallback: its source is errored and may hold queued
    /// messages, so readiness is just "no turn in flight", and the intent is
    /// dropped when that error is no longer the chat's latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_entry: Option<String>,
    /// Queue rows of the source already written into the successor's prompt;
    /// any other user row is carried to the successor once the role moves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<String>,
}

/// The status checkpoint a Manager at 80% context was asked for: the
/// completed turn that queued the request and the status revision it had
/// then. Kept across restarts so a Manager is asked once, not once per start.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub(crate) struct ManagerCheckpoint {
    pub turn: String,
    pub revision: u32,
}

#[derive(Default)]
struct State {
    path: Option<PathBuf>,
    pending: Vec<PendingRoleTransfer>,
    automatic_path: Option<PathBuf>,
    automatic: Vec<AutomaticHandoff>,
    checkpoints_path: Option<PathBuf>,
    checkpoints: BTreeMap<String, ManagerCheckpoint>,
    /// The documentation ledger a Manager hand-off carries forward. Opened
    /// beside the pending file; absent when its file cannot be read.
    living_docs: Option<Arc<LivingDocStore>>,
}

impl State {
    fn replace(&mut self, pending: Vec<PendingRoleTransfer>) -> io::Result<()> {
        if let Some(path) = &self.path {
            write_json(path, &pending)?;
        }
        self.pending = pending;
        Ok(())
    }

    fn replace_automatic(&mut self, intents: Vec<AutomaticHandoff>) -> io::Result<()> {
        if let Some(path) = &self.automatic_path {
            write_json(path, &intents)?;
        }
        self.automatic = intents;
        Ok(())
    }

    fn replace_checkpoints(&mut self, checkpoints: BTreeMap<String, ManagerCheckpoint>) -> io::Result<()> {
        if let Some(path) = &self.checkpoints_path {
            write_json(path, &checkpoints)?;
        }
        self.checkpoints = checkpoints;
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct PendingHandoffs(Mutex<State>);

impl PendingHandoffs {
    pub(crate) fn open(&self, path: PathBuf) -> io::Result<()> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // Engine assembly and hand-off recovery both open the store. A second
        // call would re-read the files and repeat any warning about them, so
        // a store already open at this path is left as it is.
        if state.path.as_ref() == Some(&path) {
            return Ok(());
        }
        state.pending = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        let automatic_path = path.with_file_name("automatic-manager-handoffs.json");
        state.automatic = match std::fs::read(&automatic_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        state.automatic_path = Some(automatic_path);
        // A checkpoint only avoids a repeated nudge. Unlike the intents above,
        // an unreadable file must not keep the engine from starting.
        let checkpoints_path = path.with_file_name("automatic-manager-checkpoints.json");
        state.checkpoints = match std::fs::read(&checkpoints_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, "ignoring unreadable Manager checkpoint file");
                BTreeMap::new()
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                tracing::warn!(%error, "ignoring unreadable Manager checkpoint file");
                BTreeMap::new()
            }
        };
        state.checkpoints_path = Some(checkpoints_path);
        // Like a checkpoint, the ledger only adds to a hand-off: an unreadable
        // file is left in place and must not keep the engine from starting.
        if let Some(dir) = path.parent() {
            match LivingDocStore::open(dir) {
                Ok(store) => state.living_docs = Some(Arc::new(store)),
                Err(error) => tracing::warn!(%error, "hand-offs will carry no documentation ledger"),
            }
        }
        state.path = Some(path);
        Ok(())
    }

    /// The documentation ledger, once the store is open.
    pub(crate) fn living_docs(&self) -> Option<Arc<LivingDocStore>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).living_docs.clone()
    }

    /// Use `store` as the ledger hand-offs carry (the one a land hook feeds).
    #[cfg(test)]
    pub(crate) fn set_living_docs(&self, store: Arc<LivingDocStore>) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).living_docs = Some(store);
    }

    pub(crate) fn checkpoints(&self) -> HashMap<String, ManagerCheckpoint> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).checkpoints.clone().into_iter().collect()
    }

    /// Record the checkpoint asked of `chat`. On `Err` the record is not
    /// stored and the caller keeps its own copy for this run.
    pub(crate) fn remember_checkpoint(&self, chat: &str, checkpoint: ManagerCheckpoint) -> io::Result<()> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut checkpoints = state.checkpoints.clone();
        checkpoints.insert(chat.to_owned(), checkpoint);
        state.replace_checkpoints(checkpoints)
    }

    /// Drop the checkpoint of `chat`, if it has one. The record is gone from
    /// memory even when the write fails, so no later save of another chat's
    /// record writes it back; `Err` only means the file may still hold it.
    pub(crate) fn forget_checkpoint(&self, chat: &str) -> io::Result<()> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.checkpoints.remove(chat).is_none() {
            return Ok(());
        }
        match &state.checkpoints_path {
            Some(path) => write_json(path, &state.checkpoints),
            None => Ok(()),
        }
    }

    pub(crate) fn automatic_intents(&self) -> Vec<AutomaticHandoff> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).automatic.clone()
    }

    /// Save `intent` unless its source already has one. `Ok(false)`: another
    /// intent for that source was there first and nothing was stored.
    pub(crate) fn remember_automatic(&self, intent: AutomaticHandoff) -> io::Result<bool> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.automatic.iter().any(|entry| entry.source == intent.source) {
            return Ok(false);
        }
        let mut intents = state.automatic.clone();
        intents.push(intent);
        state.replace_automatic(intents).map(|()| true)
    }

    pub(crate) fn settle_automatic(
        &self, source: &str, operation: &str, park: bool,
    ) -> io::Result<()> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut intents = state.automatic.clone();
        let changed = |entry: &AutomaticHandoff| {
            entry.source == source && entry.request.operation_id == operation
        };
        if park {
            if let Some(intent) = intents.iter_mut().find(|entry| changed(entry)) {
                intent.parked = true;
            }
        } else {
            intents.retain(|entry| !changed(entry));
        }
        state.replace_automatic(intents)
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
    tokens
        .pending_handoffs
        .open(data_dir.join("pending-role-handoffs.json"))?;
    // Restore the exact successor gates synchronously, before any wake
    // watcher or RPC can evaluate a device-local report after cold boot.
    // The wake transfer store may have reached disk before the registry,
    // so the durable intent is sufficient even when no reservation loads.
    for intent in tokens.pending_handoffs.automatic_intents() {
        let operation = &intent.request.operation_id;
        let successor = mast_proto::derive_id(operation, mast_proto::work_outline::roles::CHAT);
        wakes.hold_automatic_manager_handoff(&successor, operation);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(turn: &str, revision: u32) -> ManagerCheckpoint {
        ManagerCheckpoint { turn: turn.into(), revision }
    }

    #[test]
    fn checkpoints_survive_reopen_and_forgetting_is_durable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-role-handoffs.json");
        let store = PendingHandoffs::default();
        store.open(path.clone()).unwrap();
        assert!(store.checkpoints().is_empty(), "an existing user has no file yet");
        store.remember_checkpoint("a", checkpoint("turn-1", 3)).unwrap();
        store.remember_checkpoint("b", checkpoint("turn-2", 0)).unwrap();

        let reopened = PendingHandoffs::default();
        reopened.open(path.clone()).unwrap();
        assert_eq!(reopened.checkpoints().get("a"), Some(&checkpoint("turn-1", 3)));
        assert_eq!(reopened.checkpoints().len(), 2);

        reopened.forget_checkpoint("a").unwrap();
        reopened.forget_checkpoint("missing").unwrap();
        let again = PendingHandoffs::default();
        again.open(path).unwrap();
        assert_eq!(again.checkpoints().keys().collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn opening_an_open_store_again_keeps_it_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-role-handoffs.json");
        let checkpoints = dir.path().join("automatic-manager-checkpoints.json");
        std::fs::write(&checkpoints, b"{ not json").unwrap();
        let store = PendingHandoffs::default();
        store.open(path.clone()).unwrap();
        store.remember_checkpoint("a", checkpoint("turn-1", 1)).unwrap();
        // Engine assembly and hand-off recovery both open the store, and
        // the second call must not re-read or re-warn about the files.
        std::fs::write(&checkpoints, b"{ not json again").unwrap();
        store.open(path).unwrap();
        assert_eq!(store.checkpoints().get("a"), Some(&checkpoint("turn-1", 1)));
    }

    #[test]
    fn unreadable_checkpoint_file_does_not_block_opening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-role-handoffs.json");
        std::fs::write(dir.path().join("automatic-manager-checkpoints.json"), b"{ not json").unwrap();
        let store = PendingHandoffs::default();
        store.open(path).unwrap();
        assert!(store.checkpoints().is_empty());
        store.remember_checkpoint("a", checkpoint("turn-1", 1)).unwrap();
        assert_eq!(store.checkpoints().len(), 1);
    }
}
