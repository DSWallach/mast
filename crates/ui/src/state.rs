//! App state: the engine connection, entity lists, and the selected chat's
//! transcript — one gpui [`Entity`] the whole shell renders from.
//!
//! ## EngineHandle
//! The UI talks the same typed RPC whether the engine is in-process or a separate
//! daemon (ARCHITECTURE §1). [`EngineHandle::bootstrap`] probes the localhost IPC
//! port, mirroring zeron: if an engine is listening it connects over WebSocket
//! ([`RemoteEngine`]); otherwise it embeds one via [`EngineCore::assemble`] and an
//! in-memory RPC transport ([`InProcessEngine`]) — same envelopes, same dispatch.
//!
//! ## Async bridging
//! `bootstrap` runs on tokio via `gpui_tokio::Tokio::spawn`. Once an [`RpcClient`]
//! exists, its `call`/`subscribe` futures are runtime-agnostic (tokio channels),
//! so subscription pumps run on gpui's own executor via `cx.spawn` and fold each
//! frame into the entity with `this.update(...)` + `cx.notify()`.
//!
//! Pure logic (sort order, staleness, gate phase) lives in free functions with
//! unit tests; rendering reads them.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gpui::{App, AsyncApp, Context, Entity, Task, WeakEntity};
use gpui_tokio::Tokio;
use serde::de::DeserializeOwned;

use crate::comments::ReviewComment;
use mast_doc::{SessionMessageEntry, TranscriptDesync, TranscriptFrame};
use mast_engine::{Engine, EngineConfig, EngineRuntime, InstanceLock, rpc::AuthRpc};
use mast_proto::{
    AuthState, ChangeRequestSummary, Chat, ChatIndicator, CheckoutChangeRequestStatus, Device,
    EngineInfo, HarnessId, Session, SidebarPreferencesState, Space, WorkspaceScope,
};
use mast_rpc::{RpcClient, RpcError, RpcReply, RpcService, connect_ws, memory_client, methods};

use crate::change_requests::{
    ChangeRequestClientState, ChangeRequestWatchKey, desired_watch_targets, watch_params,
};

// Recently viewed transcripts stay renderable while a fresh watch opens. Move
// ownership on navigation; never clone whale payloads or retain live watches.
const TRANSCRIPT_CACHE_CAP: usize = 12;
const TRANSCRIPT_CACHE_BYTES: usize = 64 * 1024 * 1024;

struct CachedTranscript {
    prepared: Option<Arc<crate::transcript::PreparedTranscript>>,
    chat_id: String,
    entries: Vec<SessionMessageEntry>,
    context_usage: Option<mast_proto::ContextUsage>,
    bytes: usize,
}

// A cancelled GPUI watch may own a whale's mirror between updates. Its
// destructor must not free that entire object graph on the UI thread either.
struct WatchPreparation {
    worker: Option<crate::transcript::TranscriptPreparation>,
    executor: gpui::BackgroundExecutor,
}
impl WatchPreparation {
    fn new(executor: gpui::BackgroundExecutor) -> Self {
        Self {
            worker: Some(Default::default()),
            executor,
        }
    }
    fn prepare(
        &mut self,
        update: &mast_doc::TranscriptUpdate,
    ) -> Result<Arc<crate::transcript::PreparedTranscript>, TranscriptDesync> {
        self.worker.as_mut().unwrap().prepare(update)
    }
}
impl Drop for WatchPreparation {
    fn drop(&mut self) {
        let worker = self.worker.take();
        self.executor
            .spawn(async move {
                drop(worker);
            })
            .detach();
    }
}

mod engine_handle;
pub use engine_handle::{EngineBootConfig, EngineHandle, EngineMode};
use engine_handle::{DeferredEngineState, query_engine_info, wait_for_deferred_engine};

// ---------------------------------------------------------------------------
// Pure state + reducers
// ---------------------------------------------------------------------------

// The frontend-agnostic derivations (sort orders, staleness gating, sidebar
// grouping, the boot gate, relative times) live in `mast_proto::view`, pure
// and with their own test suite. Re-exported here because every call site in
// this crate reads them as `state::…`.
pub use mast_proto::view::{
    ChatGroup, ConnectionStatus, GatePhase, Indicator, SESSION_STALE_MS, attention_rank,
    chat_location, display_status, effective_indicator, format_time_ago, gate_phase, group_chats,
    parse_auth_state, project_label, sort_active, sort_chats, sort_spaces, sort_tabs,
};

// ---------------------------------------------------------------------------
// Org gate (pure)
// ---------------------------------------------------------------------------

/// One org membership row (tolerant local mirror of the engine's ListOrgs
/// reply — `{orgs: [{id, organizationId, name}]}`).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgRow {
    pub organization_id: String,
    pub name: String,
}

/// Parse a ListOrgs reply tolerantly (accepts a bare array too).
pub fn parse_orgs(value: &serde_json::Value) -> Vec<OrgRow> {
    let list = value.get("orgs").unwrap_or(value);
    serde_json::from_value(list.clone()).unwrap_or_default()
}

/// Workspace names must be non-empty (trimmed) and reasonably short.
pub fn org_name_valid(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty() && trimmed.chars().count() <= 64
}

/// Memberships sorted by name (case-insensitive), deduped by organization id.
pub fn sort_memberships(mut orgs: Vec<OrgRow>) -> Vec<OrgRow> {
    orgs.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    orgs.dedup_by(|a, b| a.organization_id == b.organization_id);
    orgs
}

// ---------------------------------------------------------------------------
// AppState entity
// ---------------------------------------------------------------------------

/// A composer send whose doc command is queued but not yet executed by the
/// chat's host device — cleared when the host writes the user message back
/// into the transcript (same client-minted id as the [`AppState::echoes`]
/// dedup); past [`UNDELIVERED_GRACE_MS`] it surfaces as the explicit
/// failed/retry state instead.
#[derive(Debug, Clone)]
struct PendingSend {
    message_id: String,
    started: DateTime<Utc>,
}

/// How long an unadopted send reads as Working/Sending (or Queued when the
/// path is degraded) before flipping to the EXPLICIT failed state with a
/// retry affordance. The old 30s overlay silently expired back to Idle with
/// no visible trace of the send at all — the exact hole the 2026-08-19
/// incident fell into.
pub const UNDELIVERED_GRACE_MS: i64 = 120_000;

/// A send's attachment-upload leg in flight. `done` is bumped by the upload
/// task per completed chunk (binary bytes); the working label reads it every
/// paint (the spinner already animates each frame), so no notify plumbing is
/// needed. A slow upload renders as "Uploading… N%" instead of a
/// hang-indistinguishable "Sending…" (2026-08-18 user report).
pub struct UploadProgress {
    done: std::sync::Arc<std::sync::atomic::AtomicU64>,
    total: u64,
}

/// Root application state. Reducer methods (`apply_*`, [`Self::session_for`], …)
/// are plain `&mut self` functions so tests construct the struct directly; gpui
/// glue ([`Self::bootstrap`], [`Self::select_chat`]) layers subscriptions on top.
pub struct AppState {
    pub connection: ConnectionStatus,
    /// Fixed data boundary of the attached engine. Authentication may change
    /// in place, but changing this scope requires assembling a new runtime.
    pub workspace_scope: Option<WorkspaceScope>,
    /// Auth stream value; `None` until the engine reports one (M4).
    pub auth: Option<AuthState>,
    pub devices: Vec<Device>,
    // Last published device presentation. Heartbeats refresh the underlying
    // timestamps without invalidating every view when no displayed value changed.
    device_presentation: Option<Vec<(Device, bool, String)>>,
    // Presence ticks also retire stale remote session indicators. Remember
    // their last published appearance even when the device rows stay online.
    session_presence_presentation: Vec<Indicator>,
    /// Live edge posture (WatchConnectivity): drives the connection pill,
    /// composer honesty ("will queue"), and the Queued send badges.
    pub connectivity: mast_proto::Connectivity,
    /// Whether this runtime's connectivity watch has delivered its first
    /// frame. The default `Disabled` value is only a placeholder and must not
    /// seed notification decisions before the watch is authoritative.
    pub(crate) connectivity_observed: bool,
    /// Sorted (see [`sort_spaces`]).
    pub spaces: Vec<Space>,
    /// Sorted (see [`sort_chats`]); includes archived rows — views filter.
    pub chats: Vec<Chat>,
    pub sessions: Vec<Session>,
    /// Synced user/org sidebar pin state. Local workspaces deliberately ignore
    /// this and continue reading their device-local settings entry.
    pub sidebar_preferences: SidebarPreferencesState,
    session_presentation: Option<Vec<Session>>,
    /// The project the new-session canvas mints into. Healed by
    /// [`Self::apply_spaces`] when the row vanishes; selecting a chat implies
    /// its project.
    pub selected_space: Option<String>,
    /// Deliberate "Don't work in a project" pick: while set, the canvas mints
    /// project-less sessions (cwd `~` on the picked device) and
    /// [`Self::selected_space_row`] reads as `None` — healing must NOT
    /// re-select a project underneath it.
    pub no_project: bool,
    /// The composer's device pick — where project-less sessions run, and the
    /// device whose projects the project picker lists. `None` falls back to
    /// the local device.
    pub selected_device: Option<String>,
    pub selected_chat: Option<String>,
    /// Boot auto-select happened (or a manual selection superseded it).
    pub auto_selected: bool,
    /// First chats / spaces watch frame has landed — device-local state that
    /// prunes against the doc (open tabs, the sidebar space filter) must not
    /// judge by the empty pre-sync lists.
    pub chats_synced: bool,
    pub spaces_synced: bool,
    pending_deep_link: Option<crate::links::ConversationDeepLink>,
    deep_link_notice: Option<String>,
    /// Joined transcript of the selected chat (continuations folded engine-side).
    pub transcript: Vec<SessionMessageEntry>,
    /// The selected chat's pending-message queue — what was typed while the
    /// agent was busy, in the order it will be sent. Device-agnostic: the
    /// chat's doc holds them (every device sees the same queue).
    pub queue: Vec<mast_doc::QueuedMessage>,
    /// The selected chat's sends the host refused with a typed Work Outline
    /// instruction-delivery outcome that still need the user (`WatchQueue`
    /// `rejectedSends`): a context reset waiting on confirmation.
    pub rejected_sends: Vec<mast_proto::InstructionRejectedSend>,
    pub context_usage: Option<mast_proto::ContextUsage>,
    /// The selected chat has a transcript from a `WatchDocMessages` reset
    /// (including a retained reset from an earlier visit). An
    /// empty transcript is otherwise indistinguishable from the pre-replay
    /// gap after selection, where optimistic echoes may already be visible.
    pub transcript_replayed: bool,
    transcript_baselines: HashMap<String, Arc<mast_doc::TranscriptBaseline>>,
    transcript_cache: std::collections::VecDeque<CachedTranscript>,
    pub(crate) prepared_transcripts: HashMap<String, Arc<crate::transcript::PreparedTranscript>>,
    /// Changes only when transcript/optimistic content changes. Presence,
    /// catalogs and other app-state notifications need no row derivation.
    pub(crate) transcript_revision: u64,
    pub(crate) board_status_revision: u64,
    /// Optimistic user echoes per chat id, shown until the doc frame carrying
    /// the same message id arrives (client-minted ids make dedup exact).
    echoes: HashMap<String, Vec<SessionMessageEntry>>,
    /// Send-in-flight overlay per chat id: a queued doc command the host
    /// hasn't executed yet (see [`Self::begin_pending_send`]).
    pending_sends: HashMap<String, PendingSend>,
    /// The in-flight send's attachment upload, when it has one.
    upload_progress: Option<UploadProgress>,
    /// Engine-side queued-attachment transfers by uploadId (`WatchTransfers`
    /// snapshots): real relay-leg progress for the sending thumbnail's
    /// percent ring, present exactly while bytes are moving.
    transfers: HashMap<String, (u64, u64)>,
    /// Written by the changes pane, read by the composer.
    review_comments: HashMap<String, Vec<ReviewComment>>,
    pub(crate) review_comments_revision: u64,
    code_refs: HashMap<String, Vec<crate::code_ref::StagedCodeRef>>,
    code_ref_pending: HashMap<String, usize>,
    /// File surfaces whose editor-backed comments currently cite a buffer
    /// revision that has not reached disk yet. A chat remains blocked until
    /// every surface waiting on a workspace write has finished or cancelled.
    review_comment_flushes: HashMap<String, HashSet<u64>>,
    /// This engine's device id (best-effort `LocalDevice` probe; `None` until
    /// the engine serves it — views degrade gracefully).
    pub local_device_id: Option<String>,
    /// Latest `UpdateStatus` frame — drives the sidebar update strip.
    pub update: Option<mast_update::UpdateStatus>,
    /// Data directory (`ui-settings.json`, `composer-defaults.json`); set at
    /// bootstrap so child views can persist small preference files.
    pub data_dir: Option<PathBuf>,
    engine: Option<EngineHandle>,
    watch_tasks: Vec<Task<()>>,
    transcript_task: Option<Task<()>>,
    change_requests: ChangeRequestClientState,
    change_request_tasks: HashMap<ChangeRequestWatchKey, Task<()>>,
    /// Bumped whenever the set of watch targets changes; backing-off watches
    /// re-check immediately when it moves.
    change_request_epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
    queue_task: Option<Task<()>>,
    change_requests_visible: bool,
    /// Work Outline snapshots keyed by Space id, fed by `WatchWorkOutline`.
    work_outlines: crate::work_outline::WorkOutlineStore,
    /// Work Outline changes made from this window, for undo and redo.
    pub(crate) action_log: crate::work_outline::action_log::ActionLog,
    /// One `WatchWorkOutline` task per watched Space (single-flight per key).
    work_outline_tasks: HashMap<String, Task<()>>,
    /// Live admin grants and the admin audit cache (`crate::admin`).
    pub(crate) admin: crate::admin::AdminState,
    /// Spaces the Outline presentation declared visible; the reconciler
    /// watches exactly these (intersected with live Spaces).
    work_outline_visible: Vec<String>,
    /// Captured at attach time so the cx-less `set_work_outline_visible` and
    /// `work_outline_actions` can schedule work on the app.
    work_outline_runtime: Option<WorkOutlineRuntime>,
    /// Planning artifact snapshots keyed by Space id, fed by
    /// `WatchArtifacts` for the same Spaces the Work Outline watches.
    artifacts: crate::artifacts::ArtifactStore,
    /// One `WatchArtifacts` task per watched Space.
    artifact_tasks: HashMap<String, Task<()>>,
    /// The render-check executor's `WatchArtifactRenderRequests` loop.
    artifact_render_task: Option<Task<()>>,
    /// Tests: answer every Work Outline mutation/query with this facade.
    #[cfg(test)]
    pub(crate) work_outline_actions_override: Option<crate::work_outline::WorkOutlineActions>,
    /// Latest managed-accounts snapshot per owning device id (accounts are
    /// device-local).
    pub managed_accounts: HashMap<String, mast_proto::ManagedAccountsSnapshot>,
    #[cfg(test)]
    pub(crate) accounts_rpc_override: Option<crate::managed_accounts::ScriptedAccountsRpc>,
    /// Per bound Chat: the latest `ValidateExecutionConfiguration {chatId}`
    /// reply and the repository picked for branch/diff (Milestone 3). Shared
    /// by the composer's branch indicator and the Changes pane.
    chat_execution: HashMap<String, crate::work_outline::execution::ChatExecutionState>,
    /// Resolved diff targets per chat: the latest `WatchChatDiffTargets` frame
    /// (full list each time; chats without a resolvable target are absent).
    chat_diff_targets: HashMap<String, Vec<mast_proto::ChatDiffTargetView>>,
    /// Resolved branch / PR range targets per chat (same frame).
    chat_ref_targets: HashMap<String, Vec<mast_proto::ChatRefTargetView>>,
    /// The last failed `SetChatDiffTarget` per chat (the Changes pane shows it
    /// as a copyable error notice until dismissed or the next write).
    chat_diff_target_errors: HashMap<String, String>,
    /// Tests: answer the diff target RPCs with this transport.
    #[cfg(test)]
    pub(crate) chat_diff_rpc_override: Option<crate::diff_targets::FakeRpc>,
    /// Linear integration: connection status and the issue cache the chips,
    /// details and panel share (`crate::linear::store`).
    pub(crate) linear: crate::linear::store::LinearStore,
    /// PR review sync: the PR status cache and last Sync now results the
    /// chips, details, panel, and ⇧⌘P share (`crate::github::store`).
    pub(crate) github: crate::github::store::GithubStore,
    /// SUBAGENT transcripts keyed by subagent doc id (the right pane's
    /// subagent tabs read these). Independent of `selected_chat`: a tab's
    /// feed must survive chat switches — the tab itself is what scopes it.
    sub_transcripts: HashMap<String, Vec<SessionMessageEntry>>,
    /// One watch task per live subagent doc (single-flight per key).
    /// Dropping a task cancels the engine-side watch and unpins the doc from
    /// the engine LRU — closing a tab MUST go through
    /// [`Self::unwatch_subagent_doc`].
    sub_watch_tasks: HashMap<String, Task<()>>,
    tab_watch_docs: std::collections::HashSet<String>,
}

/// Text/reasoning growth changes the transcript without changing session
/// status, tools, navigation, or composer state. Keep those frames local to
/// the affected transcript instead of rebuilding every app-state observer.
pub(crate) struct TranscriptTextChanged {
    pub doc_id: String,
}

impl gpui::EventEmitter<TranscriptTextChanged> for AppState {}

fn is_text_append(frame: &TranscriptFrame) -> bool {
    matches!(frame, TranscriptFrame::Delta { upsert, append, remove, .. }
        if upsert.is_empty() && remove.is_empty() && !append.is_empty())
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            connection: ConnectionStatus::Connecting,
            workspace_scope: None,
            auth: None,
            devices: Vec::new(),
            device_presentation: None,
            session_presence_presentation: Vec::new(),
            connectivity: mast_proto::Connectivity::default(),
            connectivity_observed: false,
            spaces: Vec::new(),
            chats: Vec::new(),
            sessions: Vec::new(),
            sidebar_preferences: SidebarPreferencesState::default(),
            session_presentation: None,
            selected_space: None,
            no_project: false,
            selected_device: None,
            selected_chat: None,
            transcript: Vec::new(),
            queue: Vec::new(),
            rejected_sends: Vec::new(),
            context_usage: None,
            transcript_replayed: false,
            transcript_baselines: HashMap::new(),
            transcript_cache: Default::default(),
            prepared_transcripts: HashMap::new(),
            transcript_revision: 0,
            board_status_revision: 0,
            echoes: HashMap::new(),
            pending_sends: HashMap::new(),
            upload_progress: None,
            transfers: HashMap::new(),
            review_comments: HashMap::new(),
            review_comments_revision: 0,
            code_refs: HashMap::new(),
            code_ref_pending: HashMap::new(),
            review_comment_flushes: HashMap::new(),
            local_device_id: None,
            update: None,
            data_dir: None,
            engine: None,
            watch_tasks: Vec::new(),
            transcript_task: None,
            change_requests: ChangeRequestClientState::default(),
            change_request_tasks: HashMap::new(),
            change_request_epoch: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            queue_task: None,
            change_requests_visible: true,
            work_outlines: crate::work_outline::WorkOutlineStore::new(),
            action_log: Default::default(),
            work_outline_tasks: HashMap::new(),
            admin: Default::default(),
            work_outline_visible: Vec::new(),
            work_outline_runtime: None,
            artifacts: crate::artifacts::ArtifactStore::new(),
            artifact_tasks: HashMap::new(),
            artifact_render_task: None,
            #[cfg(test)]
            work_outline_actions_override: None,
            managed_accounts: HashMap::new(),
            #[cfg(test)]
            accounts_rpc_override: None,
            chat_execution: HashMap::new(),
            chat_diff_targets: HashMap::new(),
            chat_ref_targets: HashMap::new(),
            chat_diff_target_errors: HashMap::new(),
            #[cfg(test)]
            chat_diff_rpc_override: None,
            linear: Default::default(),
            github: Default::default(),
            sub_transcripts: HashMap::new(),
            sub_watch_tasks: HashMap::new(),
            tab_watch_docs: Default::default(),
            auto_selected: false,
            chats_synced: false,
            spaces_synced: false,
            pending_deep_link: None,
            deep_link_notice: None,
        }
    }

    /// The selected chat, or `""` on the new-chat canvas. Identical to the
    /// composer's own attachment/draft key, so a comment written before the
    /// first send survives the chat being minted.
    pub fn composer_key(&self) -> String {
        self.selected_chat.clone().unwrap_or_default()
    }

    // ---- reducers (pure) ----

    pub(crate) fn apply_sidebar_preferences(&mut self, value: SidebarPreferencesState) -> bool {
        if value.revision < self.sidebar_preferences.revision || value == self.sidebar_preferences {
            return false;
        }
        self.sidebar_preferences = value;
        true
    }

    pub fn apply_chats(&mut self, mut chats: Vec<Chat>) {
        sort_chats(&mut chats);
        if self.chats != chats { self.board_status_revision = self.board_status_revision.wrapping_add(1); }
        self.chats = chats;
        self.chats_synced = true;
        self.transcript_cache
            .retain(|cached| self.chats.iter().any(|c| c.id == cached.chat_id));
        if let Some(selected) = &self.selected_chat
            && !self.chats.iter().any(|c| &c.id == selected)
        {
            // Selected chat vanished (deleted elsewhere): drop selection + transcript.
            self.transcript_baselines.remove(selected);
            self.prepared_transcripts.remove(selected);
            self.selected_chat = None;
            self.transcript.clear();
            self.context_usage = None;
            self.transcript_revision = self.transcript_revision.wrapping_add(1);
            self.transcript_replayed = false;
            self.transcript_task = None;
            self.queue.clear();
            self.rejected_sends.clear();
            self.queue_task = None;
        }
    }

    /// Replace one chat title in the shared presentation state and return its
    /// previous value. The shell uses this for an immediate rename preview;
    /// the registry watch remains authoritative and will reconcile the same
    /// persisted value (or a newer remote edit) afterward.
    pub(crate) fn replace_chat_title(
        &mut self,
        chat_id: &str,
        title: Option<String>,
    ) -> Option<Option<String>> {
        let chat = self.chats.iter_mut().find(|chat| chat.id == chat_id)?;
        let previous = std::mem::replace(&mut chat.title, title);
        if chat.title != previous {
            self.board_status_revision = self.board_status_revision.wrapping_add(1);
        }
        Some(previous)
    }

    pub fn apply_sessions(&mut self, sessions: Vec<Session>) -> bool {
        let changed = self.apply_sessions_at(sessions, Utc::now());
        if changed { self.board_status_revision = self.board_status_revision.wrapping_add(1); }
        changed
    }

    fn apply_sessions_at(&mut self, sessions: Vec<Session>, now: DateTime<Utc>) -> bool {
        let presence: Vec<_> = sessions
            .iter()
            .map(|session| effective_indicator(Some(session), now))
            .collect();
        let presentation: Vec<_> = sessions
            .iter()
            .map(|session| {
                let mut metadata = session.clone();
                // The timestamp is a liveness lease, not visible text. Keep its
                // effective indicator in the key and retain the actual value below.
                metadata.updated_at = DateTime::<Utc>::UNIX_EPOCH;
                metadata
            })
            .collect();
        let changed = self.session_presentation.as_ref() != Some(&presentation)
            || self.session_presence_presentation != presence;
        self.session_presentation = Some(presentation);
        self.session_presence_presentation = presence;
        self.sessions = sessions;
        changed
    }

    pub fn apply_spaces(&mut self, mut spaces: Vec<Space>) {
        sort_spaces(&mut spaces);
        self.spaces = spaces;
        self.spaces_synced = true;
        if self.no_project {
            self.selected_space = None;
            return;
        }
        // Heal a vanished selection (project deleted elsewhere): fall back to
        // the first project; its chats died with it, so a matching chat
        // selection is healed by the accompanying chats frame (`apply_chats`).
        // The picker lists projects per-device, so healing prefers one on the
        // picked device — a global fallback would silently re-aim the canvas
        // at another machine.
        if let Some(selected) = &self.selected_space
            && !self.spaces.iter().any(|s| &s.id == selected)
        {
            self.selected_space = self.first_space_on_picked_device();
        }
        // First frame with no selection yet: pick the first project so the
        // canvas never boots project-less by accident — unless the user
        // deliberately opted out.
        if self.selected_space.is_none() && !self.no_project {
            self.selected_space = self.first_space_on_picked_device();
        }
    }

    /// Optimistic local echo of a `setChatConfig` mutate: stamp the row now so
    /// the chips update on click; the next chats watch frame carries the same
    /// value once the engine applies the LWW write.
    pub fn apply_chat_config(&mut self, chat_id: &str, config: mast_proto::ChatConfig) {
        if let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) {
            chat.config = Some(config);
        }
    }

    pub fn apply_update(&mut self, status: mast_update::UpdateStatus) {
        self.update = Some(status);
    }

    pub fn apply_auth(&mut self, auth: AuthState) {
        self.auth = Some(auth);
    }

    /// Tolerant AuthStatus frame reducer (see [`parse_auth_state`]).
    pub fn apply_auth_value(&mut self, value: serde_json::Value) {
        match parse_auth_state(&value) {
            Some(auth) => self.apply_auth(auth),
            None => tracing::warn!("dropping unrecognized AuthStatus frame"),
        }
    }

    /// The signed-in user, if the engine reports one.
    pub fn auth_user(&self) -> Option<&mast_proto::UserProfile> {
        match self.auth.as_ref()? {
            AuthState::SignedIn { user, .. } | AuthState::NeedsOrganization { user } => Some(user),
            AuthState::SignedOut => None,
        }
    }

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

    fn reconcile_change_request_watches(&mut self, cx: &mut Context<Self>) {
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

    fn apply_pending_deep_link(&mut self, cx: &mut Context<Self>) {
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

mod review_staging;
mod device_connectivity;
mod transcript_session;
mod chat_selectors;
mod render_fingerprints;
mod chat_execution_targets;
mod work_outline_artifacts;
mod watch_tasks;
use watch_tasks::*;

pub use mast_proto::version_triple;

/// What the cx-less Work Outline entry points need to schedule work on the
/// app: a weak handle to the state and the app's async context.
#[derive(Clone)]
struct WorkOutlineRuntime {
    state: WeakEntity<AppState>,
    app: AsyncApp,
}

/// Whether a Chat's host can take Work Outline instructions
/// ([`AppState::chat_host_instruction_support`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstructionHostSupport {
    Supported,
    /// The host's device row is not synced (or the Chat is unknown).
    UnknownDevice,
    /// The host's Mast predates instruction delivery.
    MastUpgradeRequired,
    /// The host delivers instructions, but not for this harness.
    HarnessUnsupported(mast_proto::HarnessId),
}

#[cfg(test)]
mod tests;

#[cfg(feature = "appshots-fixture")]
impl AppState {
    /// Keep fixture documents deterministic while using the real attachment RPC.
    pub fn fixture_attachment_engine(&mut self, engine: EngineHandle) {
        self.engine = Some(engine);
    }
}

#[cfg(feature = "project-palette-fixture")]
impl AppState {
    /// Seed provider metadata for the isolated native sidebar review fixture.
    pub fn fixture_sidebar_change_request(
        &mut self,
        snapshot: mast_proto::CheckoutChangeRequestStatus,
    ) {
        let key = crate::change_requests::ChangeRequestWatchKey {
            device_id: snapshot.device_id.clone(),
            cwd: snapshot.cwd.clone(),
            branch: snapshot.branch.clone(),
            checkout_id: Some(snapshot.checkout_id.clone()),
        };
        self.change_requests.store(key, snapshot);
    }
}

/// Identity of a captured binding (a reply for another capture is stale).
fn execution_binding_key(binding: &mast_proto::ChatExecutionBindingView) -> String {
    format!(
        "{}|{}",
        binding.operation_id.as_deref().unwrap_or(""),
        binding.captured_at
    )
}
