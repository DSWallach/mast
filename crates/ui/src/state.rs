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
            chat.linked_pr_url.hash(&mut h);
            chat.linked_pr_source.map(|s| discriminant(&s)).hash(&mut h);
            chat.linked_ticket_id.hash(&mut h);
            chat.linked_ticket_source
                .map(|s| discriminant(&s))
                .hash(&mut h);
            discriminant(&self.display_status_with_session(chat, sessions.get(chat.id.as_str()).copied(), now)).hash(&mut h);
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
        h.finish()
    }

    /// Current Work Outline load state for one Space.
    pub(crate) fn work_outline(&self, space_id: &str) -> crate::work_outline::WorkOutlineLoad {
        self.work_outlines.load(space_id)
    }

    /// Declares which Spaces the Outline presentation shows, so the
    /// `WatchWorkOutline` reconciler watches exactly those (an empty list
    /// stops every watch). Takes no `cx`: the reconcile is scheduled on the app
    /// through the runtime captured at engine attach (a no-op before attach,
    /// where `attach_engine` reconciles anyway). Use
    /// [`Self::declare_work_outline_visible`] from code that already has a `cx`.
    pub(crate) fn set_work_outline_visible(&mut self, space_ids: Vec<String>) {
        if !self.record_work_outline_visible(space_ids) {
            return;
        }
        if let Some(runtime) = self.work_outline_runtime.clone() {
            runtime
                .app
                .clone()
                .spawn(async move |cx| {
                    runtime
                        .state
                        .update(cx, |state, cx| state.reconcile_work_outline_watches(cx))
                        .ok();
                })
                .detach();
        }
    }

    /// [`Self::set_work_outline_visible`] with an immediate reconcile.
    pub(crate) fn declare_work_outline_visible(
        &mut self,
        space_ids: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.record_work_outline_visible(space_ids);
        self.reconcile_work_outline_watches(cx);
    }

    /// Normalize (sort, dedupe) and store the declared set; whether it changed.
    fn record_work_outline_visible(&mut self, mut space_ids: Vec<String>) -> bool {
        space_ids.sort();
        space_ids.dedup();
        if self.work_outline_visible == space_ids {
            return false;
        }
        self.work_outline_visible = space_ids;
        true
    }

    /// Unresolved user-action totals per watched Space (zero totals omitted).
    /// Plumbing for the Milestone 4 notification detector: it diffs this map
    /// against its previous copy and fires on 0 -> nonzero. No notification or
    /// color is produced in Milestone 1.
    #[cfg(test)]
    pub(crate) fn work_outline_unresolved_user_actions(&self) -> HashMap<String, u32> {
        self.work_outlines.unresolved_user_action_totals()
    }

    /// The captured execution binding of a Chat, from its Space's outline
    /// snapshot (`None` = unbound or the snapshot is not loaded).
    pub(crate) fn chat_execution_binding(
        &self,
        chat_id: &str,
    ) -> Option<&mast_proto::ChatExecutionBindingView> {
        let space_id = self
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)?
            .space_id
            .as_deref()?;
        self.work_outlines
            .snapshot(space_id)?
            .chat_execution_binding(chat_id)
    }

    /// The pills of a Chat's Changes pane and branch indicator: its bound
    /// execution members (primary first, branch/HEAD from the latest
    /// validation reply), then, only when the chat has diff targets, its own
    /// checkout and one member per resolvable target. Empty for a Chat with
    /// neither a binding nor a target.
    pub(crate) fn chat_execution_members(
        &self,
        chat_id: &str,
    ) -> Vec<crate::work_outline::execution::ExecutionMember> {
        use crate::work_outline::execution::ExecutionMember;
        let mut members = match self.chat_execution_binding(chat_id) {
            Some(binding) => {
                let entry = self
                    .chat_execution
                    .get(chat_id)
                    .filter(|entry| entry.binding_key == execution_binding_key(binding));
                crate::work_outline::execution::execution_members(
                    binding,
                    entry.and_then(|entry| entry.reply.as_ref()),
                )
            }
            None => Vec::new(),
        };
        let targets = self.chat_diff_targets(chat_id);
        let ref_targets = self.chat_ref_targets(chat_id);
        if !targets.is_empty() || !ref_targets.is_empty() {
            members.extend(
                self.chats
                    .iter()
                    .find(|chat| chat.id == chat_id)
                    .and_then(ExecutionMember::own_checkout),
            );
            for target in targets {
                // A target that is also a bound member's checkout is that
                // member (no duplicate pill).
                if !members
                    .iter()
                    .any(|member| member.checkout_path == target.path)
                {
                    members.push(ExecutionMember::from_diff_target(target));
                }
            }
            members.extend(ref_targets.iter().map(ExecutionMember::from_ref_target));
        }
        members
    }

    /// The member the branch indicator and Changes pane show. Precedence: the
    /// pill picked in this session, else the engine-selected diff target,
    /// else the default (primary, else first: the own checkout of an unbound
    /// Chat). `None` for a Chat with no members.
    pub(crate) fn selected_execution_member(
        &self,
        chat_id: &str,
    ) -> Option<crate::work_outline::execution::ExecutionMember> {
        let members = self.chat_execution_members(chat_id);
        let picked = self
            .chat_execution
            .get(chat_id)
            .and_then(|entry| entry.selected.as_deref());
        picked
            .and_then(|repo| members.iter().find(|member| member.repo_id == repo))
            .or_else(|| {
                let selected = self
                    .chat_diff_targets(chat_id)
                    .iter()
                    .find(|target| target.selected)?;
                members.iter().find(|member| {
                    member.kind != crate::work_outline::execution::MemberKind::Ref
                        && member.checkout_path == selected.path
                })
            })
            .or_else(|| {
                let selected = self
                    .chat_ref_targets(chat_id)
                    .iter()
                    .find(|target| target.selected)?;
                let id = crate::work_outline::execution::ref_target_member_id(&selected.id);
                members.iter().find(|member| member.repo_id == id)
            })
            .or_else(|| crate::work_outline::execution::default_member(&members))
            .cloned()
    }

    pub(crate) fn chat_execution_state(
        &self,
        chat_id: &str,
    ) -> Option<&crate::work_outline::execution::ChatExecutionState> {
        self.chat_execution.get(chat_id)
    }

    /// Pick the repository a Chat's branch indicator and Changes pane show.
    ///
    /// The pick is applied in memory at once (the pane re-targets without
    /// waiting) and, for a Chat with diff targets, persisted through
    /// `SetChatDiffTarget`: a target pill writes `select {path}`, the own or a
    /// binding pill `clearSelection`. A stale in-memory pick must not fight
    /// the persisted selection: [`Self::apply_chat_diff_targets`] drops it when
    /// a frame selects a different target, and a failed write drops it here
    /// (the engine's selection is the truth).
    pub(crate) fn select_chat_execution_member(
        &mut self,
        chat_id: &str,
        repo_id: &str,
        cx: &mut Context<Self>,
    ) {
        use crate::work_outline::execution::MemberKind;
        let effective = self
            .selected_execution_member(chat_id)
            .map(|member| member.repo_id);
        if self.pick_chat_execution_member_locally(chat_id, repo_id) {
            cx.notify();
        }
        if effective.as_deref() == Some(repo_id)
            || (self.chat_diff_targets(chat_id).is_empty()
                && self.chat_ref_targets(chat_id).is_empty())
        {
            return;
        }
        let Some(member) = self
            .chat_execution_members(chat_id)
            .into_iter()
            .find(|member| member.repo_id == repo_id)
        else {
            return;
        };
        if member.kind == MemberKind::Ref {
            let ref_id = member
                .repo_id
                .strip_prefix("ref:")
                .unwrap_or(&member.repo_id)
                .to_owned();
            self.write_chat_diff_target(
                mast_proto::SetChatDiffTargetParams {
                    chat_id: chat_id.to_owned(),
                    action: mast_proto::SetChatDiffTargetAction::SelectRef,
                    ref_id: Some(ref_id),
                    ..Default::default()
                },
                cx,
            )
            .detach();
            return;
        }
        let (action, path) = match member.kind {
            MemberKind::Ref => return,
            MemberKind::Target => (
                mast_proto::SetChatDiffTargetAction::Select,
                Some(member.checkout_path),
            ),
            MemberKind::Own | MemberKind::Binding => {
                (mast_proto::SetChatDiffTargetAction::ClearSelection, None)
            }
        };
        self.set_chat_diff_target(chat_id, action, path, cx)
            .detach();
    }

    /// The in-memory half of [`Self::select_chat_execution_member`]. Returns
    /// whether the pick changed.
    pub(crate) fn pick_chat_execution_member_locally(
        &mut self,
        chat_id: &str,
        repo_id: &str,
    ) -> bool {
        let entry = self.chat_execution.entry(chat_id.to_owned()).or_default();
        if entry.selected.as_deref() == Some(repo_id) {
            return false;
        }
        entry.selected = Some(repo_id.to_owned());
        true
    }

    /// Selection is engine-owned: a session-local pill pick affects the header
    /// only after its selection has round-tripped through the engine.
    pub(crate) fn display_chat_branch<'a>(&'a self, chat: &'a Chat) -> Option<&'a str> {
        let target = self.selected_chat_diff_target(&chat.id);
        if target.is_none() && chat.source_context.is_none() {
            return None;
        }
        crate::change_requests::display_checkout(chat, target)
            .branch
            .filter(|branch| !branch.trim().is_empty())
    }

    pub(crate) fn selected_checkout_key(
        &self,
        chat_id: &str,
    ) -> Option<(String, String, Option<String>)> {
        self.selected_chat_diff_target(chat_id)
            .map(|target| {
                (target.path.clone(), target.checkout_id.clone(), target.branch.clone())
            })
    }

    /// The explicitly selected checkout target, if any.
    pub(crate) fn selected_chat_diff_target(
        &self,
        chat_id: &str,
    ) -> Option<&mast_proto::ChatDiffTargetView> {
        self.chat_diff_targets(chat_id)
            .iter()
            .find(|target| target.selected)
    }

    /// Resolved diff targets of a Chat (empty when none).
    pub(crate) fn chat_diff_targets(&self, chat_id: &str) -> &[mast_proto::ChatDiffTargetView] {
        self.chat_diff_targets
            .get(chat_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Resolved branch / PR range targets of a Chat (empty when none).
    pub(crate) fn chat_ref_targets(&self, chat_id: &str) -> &[mast_proto::ChatRefTargetView] {
        self.chat_ref_targets
            .get(chat_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Apply one `WatchChatDiffTargets` frame (the full list). Returns
    /// whether anything changed. In-memory picks that the frame supersedes
    /// are dropped: a frame that selects a target the Chat was not on (the
    /// agent bound a worktree mid-turn, another window picked, or our own
    /// write landing) wins over a pill picked earlier; a pick of a target
    /// that vanished goes too. A frame without a selection keeps the pick: it
    /// is what a `clearSelection` write (own or binding pill) looks like.
    pub(crate) fn apply_chat_diff_targets(
        &mut self,
        frame: Vec<mast_proto::ChatDiffTargetsView>,
    ) -> bool {
        let mut next: HashMap<String, Vec<mast_proto::ChatDiffTargetView>> = HashMap::new();
        let mut next_refs: HashMap<String, Vec<mast_proto::ChatRefTargetView>> = HashMap::new();
        for view in frame {
            if !view.ref_targets.is_empty() {
                next_refs.insert(view.chat_id.clone(), view.ref_targets);
            }
            if !view.targets.is_empty() {
                next.insert(view.chat_id, view.targets);
            }
        }
        if next == self.chat_diff_targets && next_refs == self.chat_ref_targets {
            return false;
        }
        // What a frame selects: a checkout target's path or a ref target's
        // id (at most one of the two).
        let selected_key = |targets: &HashMap<String, Vec<mast_proto::ChatDiffTargetView>>,
                            refs: &HashMap<String, Vec<mast_proto::ChatRefTargetView>>,
                            chat_id: &str| {
            targets
                .get(chat_id)
                .and_then(|targets| targets.iter().find(|target| target.selected))
                .map(|target| format!("path:{}", target.path))
                .or_else(|| {
                    refs.get(chat_id)
                        .and_then(|refs| refs.iter().find(|target| target.selected))
                        .map(|target| format!("ref:{}", target.id))
                })
        };
        for (chat_id, entry) in &mut self.chat_execution {
            let Some(pick) = entry.selected.as_deref() else {
                continue;
            };
            let now = selected_key(&next, &next_refs, chat_id);
            let reselected = now.is_some()
                && now != selected_key(&self.chat_diff_targets, &self.chat_ref_targets, chat_id);
            let vanished = pick
                .strip_prefix("target:")
                .map(|checkout_id| {
                    !next
                        .get(chat_id)
                        .is_some_and(|targets| targets.iter().any(|t| t.checkout_id == checkout_id))
                })
                .or_else(|| {
                    pick.strip_prefix("ref:").map(|id| {
                        !next_refs
                            .get(chat_id)
                            .is_some_and(|refs| refs.iter().any(|t| t.id == id))
                    })
                })
                .unwrap_or(false);
            if reselected || vanished {
                entry.selected = None;
            }
        }
        self.chat_diff_targets = next;
        self.chat_ref_targets = next_refs;
        self.board_status_revision = self.board_status_revision.wrapping_add(1);
        true
    }

    /// Tests: put a `WatchChatDiffTargets` frame in the store.
    #[cfg(test)]
    pub(crate) fn set_chat_diff_targets_for_test(
        &mut self,
        frame: Vec<mast_proto::ChatDiffTargetsView>,
    ) {
        self.apply_chat_diff_targets(frame);
    }

    /// Forget the pill picked in this session so the persisted (engine)
    /// selection shows. Called when a write that changes the persisted
    /// selection is issued from the "Diff checkout..." picker.
    pub(crate) fn clear_chat_diff_member_pick(&mut self, chat_id: &str) {
        if let Some(entry) = self.chat_execution.get_mut(chat_id) {
            entry.selected = None;
        }
    }

    /// The last failed diff target write of a Chat.
    pub(crate) fn chat_diff_target_error(&self, chat_id: &str) -> Option<&str> {
        self.chat_diff_target_errors
            .get(chat_id)
            .map(String::as_str)
    }

    pub(crate) fn dismiss_chat_diff_target_error(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        if self.chat_diff_target_errors.remove(chat_id).is_some() {
            cx.notify();
        }
    }

    /// One diff target RPC: the engine, or the test transport.
    pub(crate) fn chat_diff_rpc(
        &self,
        method: &'static str,
        params: serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<serde_json::Value, String>> {
        #[cfg(test)]
        if let Some(fake) = self.chat_diff_rpc_override.clone() {
            return Task::ready(fake(method, params));
        }
        let Some(engine) = self.engine.clone() else {
            return Task::ready(Err("Mast is not connected to its engine.".to_owned()));
        };
        cx.spawn(async move |_, _| {
            engine
                .client()
                .call(method, params)
                .await
                .map_err(|error| error.to_string())
        })
    }

    /// `ListChatDiffCheckouts {chatId}`: the "Diff checkout..." candidates in
    /// engine order (the picker ranks them).
    pub(crate) fn list_chat_diff_checkouts(
        &self,
        chat_id: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<mast_proto::DiffCheckoutCandidate>, String>> {
        let params = serde_json::to_value(mast_proto::ListChatDiffCheckoutsParams {
            chat_id: chat_id.to_owned(),
        })
        .unwrap_or_default();
        let task = self.chat_diff_rpc(methods::LIST_CHAT_DIFF_CHECKOUTS, params, cx);
        cx.spawn(async move |_, _| {
            let value = task.await?;
            serde_json::from_value::<mast_proto::ListChatDiffCheckoutsReply>(value)
                .map(|reply| reply.candidates)
                .map_err(|error| format!("Malformed checkout list from the engine: {error}"))
        })
    }

    /// `SetChatDiffTarget`. A failure is kept in
    /// [`Self::chat_diff_target_error`] (and returned) and drops the in-memory
    /// pick, so the pane shows what the engine still has selected.
    pub(crate) fn set_chat_diff_target(
        &mut self,
        chat_id: &str,
        action: mast_proto::SetChatDiffTargetAction,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        self.write_chat_diff_target(
            mast_proto::SetChatDiffTargetParams {
                chat_id: chat_id.to_owned(),
                action,
                path,
                ..Default::default()
            },
            cx,
        )
    }

    /// `SetChatDiffTarget {chatId, action: bindRef, input, baseBranch?,
    /// repoPath?}`: resolve a PR number / URL or a branch name, fetch its
    /// refs read-only and select the new range target. A failure is kept in
    /// [`Self::chat_diff_target_error`] like every diff target write.
    pub(crate) fn bind_chat_ref_target(
        &mut self,
        chat_id: &str,
        input: String,
        repo_path: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        self.write_chat_diff_target(
            mast_proto::SetChatDiffTargetParams {
                chat_id: chat_id.to_owned(),
                action: mast_proto::SetChatDiffTargetAction::BindRef,
                input: Some(input),
                repo_path,
                ..Default::default()
            },
            cx,
        )
    }

    /// `ListChatRefDiffCandidates {chatId}`: linked PRs first, then branches
    /// matching the chat's ticket, in engine order.
    pub(crate) fn list_chat_ref_candidates(
        &self,
        chat_id: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<mast_proto::RefDiffCandidate>, String>> {
        let params = serde_json::to_value(mast_proto::ListChatDiffCheckoutsParams {
            chat_id: chat_id.to_owned(),
        })
        .unwrap_or_default();
        let task = self.chat_diff_rpc(methods::LIST_CHAT_REF_DIFF_CANDIDATES, params, cx);
        cx.spawn(async move |_, _| {
            let value = task.await?;
            serde_json::from_value::<mast_proto::ListChatRefDiffCandidatesReply>(value)
                .map(|reply| reply.candidates)
                .map_err(|error| format!("Malformed branch list from the engine: {error}"))
        })
    }

    fn write_chat_diff_target(
        &mut self,
        params: mast_proto::SetChatDiffTargetParams,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let chat_id = params.chat_id.clone();
        let params = serde_json::to_value(params).unwrap_or_default();
        self.chat_diff_target_errors.remove(&chat_id);
        let task = self.chat_diff_rpc(methods::SET_CHAT_DIFF_TARGET, params, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await.map(|_| ());
            if let Err(message) = &result {
                let message = message.clone();
                this.update(cx, |state, cx| {
                    tracing::warn!(chat_id, %message, "SetChatDiffTarget failed");
                    state.clear_chat_diff_member_pick(&chat_id);
                    state.chat_diff_target_errors.insert(chat_id, message);
                    cx.notify();
                })
                .ok();
            }
            result
        })
    }

    /// Fetch the bound Chat's per-member branch/HEAD and checkout identities
    /// (`ValidateExecutionConfiguration {chatId}`; the engine forwards to the
    /// owning device). `force` refetches (a surface opening); otherwise only
    /// a Chat without a reply for its current binding fetches. Unbound Chats
    /// do nothing.
    pub(crate) fn refresh_chat_execution(
        &mut self,
        chat_id: &str,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(binding) = self.chat_execution_binding(chat_id) else {
            return;
        };
        let key = execution_binding_key(binding);
        let entry = self.chat_execution.entry(chat_id.to_owned()).or_default();
        let current = entry.binding_key == key;
        if current && entry.loading {
            return;
        }
        if current && !force && (entry.reply.is_some() || entry.error.is_some()) {
            return;
        }
        if !current {
            entry.reply = None;
            entry.error = None;
            entry.binding_key = key.clone();
        }
        entry.loading = true;
        let task = self.work_outline_actions().validate_execution(
            mast_proto::ValidateExecutionConfigurationParams {
                chat_id: Some(chat_id.to_owned()),
                ..Default::default()
            },
        );
        let chat_id = chat_id.to_owned();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |state, cx| {
                let Some(entry) = state.chat_execution.get_mut(&chat_id) else {
                    return;
                };
                if entry.binding_key != key {
                    return;
                }
                entry.loading = false;
                match result {
                    Ok(reply) => {
                        entry.reply = Some(reply);
                        entry.error = None;
                    }
                    Err(error) => {
                        entry.error =
                            Some(crate::work_outline::execution::execution_error_text(&error));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Tests: put a loaded Work Outline snapshot in the store.
    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn set_work_outline_for_test(&mut self, snapshot: mast_proto::WorkOutlineSnapshot) {
        let space_id = snapshot.space_id.clone();
        self.work_outlines.set(
            &space_id,
            crate::work_outline::WorkOutlineLoad::Ready(Arc::new(snapshot)),
        );
    }

    /// Mutation facade bound to the current engine connection.
    pub(crate) fn work_outline_actions(&self) -> crate::work_outline::WorkOutlineActions {
        #[cfg(test)]
        if let Some(actions) = &self.work_outline_actions_override {
            return actions.clone();
        }
        let actions = crate::work_outline::WorkOutlineActions::new(self.engine.clone());
        match &self.work_outline_runtime {
            Some(runtime) => actions.with_executor(runtime.app.background_executor().clone()),
            None => actions,
        }
    }

    /// Apply one `WatchWorkOutline` frame for `space_id`. Returns whether the
    /// store changed. A frame for another Space, or for a Space that is not
    /// declared visible, is dropped. Stale-revision filtering is per stream
    /// and lives in the pump (`spawn_work_outline_watch`).
    pub(crate) fn apply_work_outline_frame(
        &mut self,
        space_id: &str,
        snapshot: mast_proto::WorkOutlineSnapshot,
    ) -> bool {
        if snapshot.space_id != space_id || !self.work_outline_visible_contains(space_id) {
            return false;
        }
        self.work_outlines.set(
            space_id,
            crate::work_outline::WorkOutlineLoad::Ready(Arc::new(snapshot)),
        )
    }

    /// Apply a failed subscribe. A last good snapshot survives transient
    /// failures (the outline keeps rendering stale rather than blanking);
    /// `Unsupported` always replaces it.
    pub(crate) fn apply_work_outline_failure(&mut self, space_id: &str, message: &str) -> bool {
        if !self.work_outline_visible_contains(space_id) {
            return false;
        }
        let load = crate::work_outline::WorkOutlineLoad::from_watch_error(message);
        if matches!(load, crate::work_outline::WorkOutlineLoad::Failed(_))
            && self.work_outlines.snapshot(space_id).is_some()
        {
            return false;
        }
        self.work_outlines.set(space_id, load)
    }

    /// Whether `space_id` is wanted: declared visible by the Overview, or the
    /// selected Chat's Space (the chat header drift line needs its outline
    /// from app start, not only once the Overview exists).
    fn work_outline_visible_contains(&self, space_id: &str) -> bool {
        self.work_outline_visible
            .binary_search_by(|id| id.as_str().cmp(space_id))
            .is_ok()
            || self.selected_chat_space_id() == Some(space_id)
    }

    /// The selected Chat's Space, when it has one.
    fn selected_chat_space_id(&self) -> Option<&str> {
        self.selected_chat_row()?.space_id.as_deref()
    }

    /// Spaces that should have a live watch: the union of the declared
    /// visible set and the selected Chat's Space, restricted to live Spaces.
    /// Is the Outline presentation on screen (any Space declared visible)?
    pub(crate) fn work_outline_is_visible(&self) -> bool {
        !self.work_outline_visible.is_empty()
    }

    fn desired_work_outline_spaces(&self) -> HashSet<String> {
        self.work_outline_visible
            .iter()
            .map(String::as_str)
            .chain(self.selected_chat_space_id())
            .filter(|id| self.spaces.iter().any(|space| space.id == *id))
            .map(str::to_owned)
            .collect()
    }

    /// Mirror of the change-request reconciler: compute the desired Space
    /// set, drop tasks and snapshots for Spaces no longer wanted (deleted or
    /// hidden), and spawn a watch for each missing one. Re-run from the
    /// Spaces frame, `attach_engine` and `declare_work_outline_visible`.
    fn reconcile_work_outline_watches(&mut self, cx: &mut Context<Self>) {
        self.reconcile_artifact_watches(cx);
        let Some(handle) = self.engine.clone() else {
            self.work_outline_tasks.clear();
            return;
        };
        let wanted = self.desired_work_outline_spaces();
        self.work_outline_tasks
            .retain(|space_id, _| wanted.contains(space_id));
        let dropped = {
            let before = self.work_outlines.space_ids().count();
            self.work_outlines
                .retain(|space_id| wanted.contains(space_id));
            before != self.work_outlines.space_ids().count()
        };
        for space_id in wanted {
            if self.work_outline_tasks.contains_key(&space_id) {
                continue;
            }
            #[cfg(feature = "startup-bench")]
            startup_trace("outline_watch_spawn");
            let task = spawn_work_outline_watch(cx, handle.clone(), space_id.clone());
            self.work_outline_tasks.insert(space_id, task);
        }
        if dropped {
            cx.notify();
        }
    }

    /// Planning artifacts of the watched Spaces.
    pub(crate) fn artifacts(&self) -> &crate::artifacts::ArtifactStore {
        &self.artifacts
    }

    /// The artifact RPC facade (shares the Work Outline transport and its
    /// test override).
    pub(crate) fn artifact_actions(&self) -> crate::artifacts::ArtifactActions {
        crate::artifacts::ArtifactActions::new(self.work_outline_actions())
    }

    /// Apply one `WatchArtifacts` frame; whether anything changed. Frames for
    /// Spaces no longer watched are dropped.
    pub(crate) fn apply_artifacts_frame(
        &mut self,
        space_id: &str,
        snapshot: mast_proto::ArtifactsSnapshot,
    ) -> bool {
        if snapshot.space_id != space_id || !self.artifact_tasks.contains_key(space_id) {
            return false;
        }
        self.artifacts.set(
            space_id,
            crate::artifacts::ArtifactsLoad::Ready(Arc::new(snapshot)),
        )
    }

    /// Register as the engine's render-capable UI and serve render checks
    /// (`WatchArtifactRenderRequests`).
    pub(crate) fn start_artifact_render_executor(
        &mut self,
        handle: EngineHandle,
        renderer: Option<std::rc::Rc<dyn crate::artifacts::render_check::ArtifactRenderer>>,
        config: crate::artifacts::render_check::RenderConfig,
        cx: &mut Context<Self>,
    ) {
        self.artifact_render_task = Some(crate::artifacts::render_check::spawn_render_requests(
            cx, handle, renderer, config,
        ));
    }

    /// Tests: install a Space's artifacts without a watch.
    #[cfg(any(test, feature = "snapshot"))]
    pub(crate) fn set_test_artifacts(&mut self, snapshot: mast_proto::ArtifactsSnapshot) {
        let space_id = snapshot.space_id.clone();
        self.artifacts.set(
            &space_id,
            crate::artifacts::ArtifactsLoad::Ready(Arc::new(snapshot)),
        );
    }

    /// Apply a failed `WatchArtifacts` subscribe.
    pub(crate) fn apply_artifacts_failure(&mut self, space_id: &str, message: &str) -> bool {
        if !self.artifact_tasks.contains_key(space_id) {
            return false;
        }
        self.artifacts.set(
            space_id,
            crate::artifacts::ArtifactsLoad::from_watch_error(message),
        )
    }

    /// `WatchArtifacts` follows the Work Outline watch set exactly: the
    /// declared visible Spaces plus the selected Chat's Space.
    fn reconcile_artifact_watches(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.engine.clone() else {
            self.artifact_tasks.clear();
            return;
        };
        let wanted = self.desired_work_outline_spaces();
        self.artifact_tasks
            .retain(|space_id, _| wanted.contains(space_id));
        let dropped = self.artifacts.retain(|space_id| wanted.contains(space_id));
        for space_id in wanted {
            if self.artifact_tasks.contains_key(&space_id) {
                continue;
            }
            let task = spawn_artifacts_watch(cx, handle.clone(), space_id.clone());
            self.artifact_tasks.insert(space_id, task);
        }
        if dropped {
            cx.notify();
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
