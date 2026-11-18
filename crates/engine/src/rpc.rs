//! EngineRpc — the engine-side `RpcService`: sessions + docs + the workspace-doc
//! entity surface.
//!
//! Methods (feature-inventory §2):
//! - `ListHarnesses` → `[HarnessDescriptor]`
//! - `ListModels {harness}` → `[Model]`
//! - `QueueCommand {chatId, command}` → `{commandId}` (durable doc command)
//! - `WatchDocMessages {chatId}` → stream of joined `SessionMessageEntry[]`,
//!   re-emitted on every doc change
//! - `WatchChats` / `WatchDevices` → streams of the workspace doc's entity rows
//! - `WatchSessions` → stream of `Session[]`: this engine's live statuses merged with
//!   remote devices' workspace session rows
//! - `Mutate {op, …}` → `{ok}` — workspace entity mutations (createChat, renameChat,
//!   setChatArchived, deleteChat, renameDevice, markChatSeen)
//! - `EngineInfo` → `{deviceId, workspaceScope}` — this runtime's fixed identity
//!   and data boundary (never forwarded)
//! - `LocalDevice` → `{deviceId}` — legacy engine identity (never forwarded)
//! - AuthRpc (feature-inventory §2): `AuthStatus` (stream), `SignIn`/`SignInHeadless` →
//!   `{url}`, `CompleteSignIn {code}`, `SignOut`, `ListOrgs`, `CreateOrg {name}`,
//!   `SelectOrg {organizationId}`
//! - Repos (§3.5): `ListRepos`, `AddRepo {path}`, `CloneRepo {url}`,
//!   `CreateRepo {name}`, `ListBranches {repoPath}` (default branch first),
//!   `ListFolders {path?}`, `CreateWorktree {repoPath, branch}`, `DeleteWorktree
//!   {repoPath, worktreePath}`; `WatchCheckoutDiffs {checkoutId?, cwd?}` → stream
//!   of `CheckoutDiff[]` (a target limits the array to one canonical checkout)
//! - Workspace files: lazy directory listing, recursive path search, bounded text
//!   reads, hash-guarded writes, and a checkout-scoped filesystem change stream.
//! - Terminals (§3.4): `OpenTerminal {chatId, cols, rows}` → `TerminalSession`,
//!   `SubscribeTerminal {terminalId, afterSeq?}` → stream of `TerminalEvent`
//!   (replay then live tail), `WriteTerminal {terminalId, data}`, `ResizeTerminal`,
//!   `CloseTerminal`. M5 is single-user local: per-user owner checks land with
//!   real multi-account auth in M6.
//! - Agent accounts (§3.7): `ListAgentAccounts {forceUsage?}` →
//!   `AgentAccountsSnapshot`, `ActivateAgentAccount`/`ForgetAgentAccount`
//!   `{harness, accountId}` → snapshot, `StartAgentLogin {harness}` →
//!   `{loginId, url, mode}`, `CompleteAgentLogin {loginId, code}` → snapshot,
//!   `PollAgentLogin {loginId}`, `CancelAgentLogin {loginId}`.
//! - Uploads (§3.7): `UploadChunk {uploadId, data, seq?}`,
//!   `UploadCommit {uploadId, fileName}` → `{path}`,
//!   `ReadAttachmentChunk {path, offset}` → `{name, mimeType, data, nextOffset,
//!   done}` (path-jailed to the uploads dir + workspace-known chat cwds).
//!
//! ## Device-addressed routing (`targetDeviceId`, feature-inventory §2.1)
//!
//! ControlRpc methods are relay-forwardable: params may carry `targetDeviceId`. When it
//! names another device, the call is forwarded verbatim over that device's relay DO via
//! the [`LinkCache`] — the remote engine sees its own id and handles locally, so the
//! forward can never loop. Streaming methods are proxied by re-subscribing remotely and
//! piping items. To make another method device-addressable, nothing per-method is needed
//! beyond listing it in [`forwardable`] (and [`is_stream_method`] if it streams);
//! handlers stay transport-agnostic. This includes the workspace file surface,
//! whose checkout always lives on the routed target device.

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::watch;

use mast_doc::{MessagePart, SessionCommandPayload};
use mast_proto::{
    ChatConfig, CreateWorktreeOutcome, EngineInfo, HarnessId, ProjectActionDraft, Space, ToolCall,
    WorkspaceScope,
};
use mast_rpc::{LinkCache, RpcError, RpcReply, RpcService, methods, parse_params};

use crate::agent_accounts::AgentAccounts;
use crate::auth::Auth;
use crate::change_requests::CheckoutChangeRequests;
use crate::diff_sync::CheckoutDiffSync;
use crate::doc_host::DocHost;
use crate::project_actions::ProjectActionsStore;
use crate::registry::HarnessRegistry;
use crate::repos::Repos;
use crate::sessions::SessionsEngine;
use crate::terminals::Terminals;
use crate::uploads::Uploads;
use crate::workspace_host::WorkspaceHost;

mod wire_parameters;
use wire_parameters::*;

const FILE_SEARCH_RPC_TIMEOUT: Duration = Duration::from_secs(6);
const FILE_SEARCH_FEATURED_PATHS: usize = 32;
const MY_OPEN_PRS_INITIAL_WAIT: Duration = Duration::from_secs(5);

async fn hand_off_config(
    registry: &HarnessRegistry,
    data_dir: &std::path::Path,
    source: Option<mast_proto::ChatConfig>,
    fallback: HarnessId,
    harness: Option<HarnessId>,
    model: Option<String>,
    reasoning: Option<mast_proto::ReasoningLevel>,
) -> Result<Option<mast_proto::ChatConfig>, RpcError> {
    if harness.is_none() && model.is_none() && reasoning.is_none() { return Ok(source); }
    let mut config = source.unwrap_or(mast_proto::ChatConfig {
        harness: fallback, model: None, reasoning: None, model_options: Default::default(),
        sandbox: mast_proto::SandboxLevel::WorkspaceWrite, auto_approve: false,
    });
    let target = harness.unwrap_or(config.harness);
    let descriptor = registry.descriptors().into_iter().find(|d| d.id == target)
        .ok_or_else(|| RpcError::Failed(format!("harness {target:?} is not available (see list_harnesses)")))?;
    if !descriptor.installed || !crate::registry::descriptor_enabled(&descriptor) && target != HarnessId::Mock {
        return Err(RpcError::Failed(format!("harness {target:?} is not available (see list_harnesses)")));
    }
    let resolved = registry.resolve(target).map_err(|e| RpcError::Failed(e.to_string()))?;
    let models = crate::model_catalogs::list(data_dir, resolved, false).await.unwrap_or_default();
    if let Some(ref requested) = model
        && !models.is_empty() && !models.iter().any(|m| m.id == *requested) {
            return Err(RpcError::Failed(format!("model {requested:?} is not offered by {target:?} (see list_models)")));
        }
    if target != config.harness {
        config.harness = target;
        let default = models.iter().find(|m| target.default_model() == Some(m.id.as_str())).or_else(|| models.first());
        config.model = default.map(|m| m.id.clone()).or_else(|| target.default_model().map(str::to_owned));
        config.reasoning = default.and_then(|m| {
            let levels = &m.reasoning_levels;
            [mast_proto::ReasoningLevel::High, mast_proto::ReasoningLevel::Medium].into_iter().find(|l| levels.contains(l)).or_else(|| levels.first().copied())
        });
        config.model_options.clear();
    }
    if let Some(model) = model { config.model = Some(model); }
    if let Some(reasoning) = reasoning { config.reasoning = Some(reasoning); }
    Ok(Some(config))
}

async fn update_harness_enabled(
    registry: &HarnessRegistry,
    harness: HarnessId,
    enabled: bool,
) -> Result<(), RpcError> {
    registry
        .set_enabled(harness, enabled)
        .map_err(RpcError::Failed)
}

/// Bind destructive close-out requests to the cwd stored on the addressed
/// chat. The client-provided cwd is only a claim; canonicalizing both sides
/// prevents a stale or forged request from planning/removing another
/// worktree while still accepting harmless path aliases.
fn validated_closeout_cwd(
    chat_id: &str,
    stored_cwd: Option<&str>,
    requested_cwd: &str,
) -> Result<std::path::PathBuf, RpcError> {
    let stored_cwd = stored_cwd.ok_or_else(|| {
        RpcError::Failed(format!(
            "cannot close out chat {chat_id}: it has no stored working directory"
        ))
    })?;
    let stored = std::fs::canonicalize(stored_cwd).map_err(|e| {
        RpcError::Failed(format!(
            "cannot close out chat {chat_id}: its stored working directory `{stored_cwd}` cannot be resolved: {e}"
        ))
    })?;
    let requested = std::fs::canonicalize(requested_cwd).map_err(|e| {
        RpcError::Failed(format!(
            "cannot close out chat {chat_id}: requested working directory `{requested_cwd}` cannot be resolved: {e}"
        ))
    })?;
    if stored != requested {
        return Err(RpcError::Failed(format!(
            "cannot close out chat {chat_id}: requested working directory `{}` does not match the chat's stored working directory `{}`",
            requested.display(),
            stored.display()
        )));
    }
    Ok(stored)
}

fn shared_closeout_error(shared: &[crate::chat_workspace_plan::CloseoutChatReference]) -> RpcError {
    RpcError::Failed(format!(
        "refusing to close out: this worktree is used by {} open chat{}: {}",
        shared.len(),
        if shared.len() == 1 { "" } else { "s" },
        shared
            .iter()
            .map(|chat| format!("{} ({})", chat.title, chat.id))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn tool_file_path(call: &ToolCall) -> Option<&str> {
    match call {
        ToolCall::ReadFile { path }
        | ToolCall::WriteFile { path, .. }
        | ToolCall::EditFile { path, .. } => Some(path),
        ToolCall::ApplyPatch { path } | ToolCall::Search { path, .. } => path.as_deref(),
        ToolCall::Exec { .. }
        | ToolCall::Glob { .. }
        | ToolCall::WebFetch { .. }
        | ToolCall::WebSearch { .. }
        | ToolCall::Todo { .. }
        | ToolCall::Mcp { .. }
        | ToolCall::Unknown { .. } => None,
    }
}

pub struct EngineRpc {
    sessions: SessionsEngine,
    doc_host: DocHost,
    workspace: WorkspaceHost,
    registry: std::sync::Arc<HarnessRegistry>,
    repos: Repos,
    workspace_files: crate::WorkspaceFiles,
    terminals: Terminals,
    project_actions: ProjectActionsStore,
    previews: Option<mast_preview::PreviewService>,
    change_requests: CheckoutChangeRequests,
    diff_sync: CheckoutDiffSync,
    /// Chat diff targets (`ListChatDiffCheckouts` / `SetChatDiffTarget`);
    /// the instance diff-sync reads (engine assembly attaches one, a bare
    /// surface attaches its own on first use).
    diff_targets: crate::diff_targets::DiffTargets,
    uploads: Uploads,
    agent_accounts: AgentAccounts,
    /// Per-project managed accounts; `None` on bare test surfaces.
    managed_accounts: Option<crate::managed_accounts::ManagedAccounts>,
    auth: Option<Auth>,
    links: Option<std::sync::Arc<LinkCache>>,
    updater: Option<mast_update::Updater>,
    local_import: Option<crate::local_import::LocalImporter>,
    external_import: Option<crate::external_import::ExternalSessionImporter>,
    pr_ticket_cache: Option<crate::pr_ticket_cache::PrTicketCache>,
    /// The PR page's data service (`PR_DASHBOARD`); built on first use, it
    /// needs no boot-time wiring.
    pr_dash: std::sync::OnceLock<crate::pr_dash::PrDash>,
    context_usage: Option<crate::context_usage::ContextUsageProvider>,
    /// `(path, mtime)`-cached subagent transcript turn-building
    /// (`READ_SUBAGENT_TRANSCRIPT`). Unlike `context_usage`/`pr_ticket_cache`,
    /// this needs no shared wiring with the boot-time repair passes in
    /// `lib.rs` — it's only ever read from this RPC handler — so it's a
    /// plain field built fresh here rather than threaded through a
    /// `with_*` builder from `EngineCore`.
    subagent_transcript_cache: crate::subagent_transcript::TranscriptCache,
    /// `WatchWorkOutline` / `WorkOutlineMutate` / `WorkOutlineQuery` (local
    /// replica, never forwarded).
    work_outline: std::sync::Arc<crate::work_outline::WorkOutlineService>,
    /// `ValidateExecutionConfiguration` / `DescribeExecutionRepository`
    /// (Work Outline Milestone 3; run on the Space's owning device).
    execution_scope: crate::execution_scope::ExecutionScopeService,
    /// `CreateChatForWorkItem` / `ContinueChatForWorkItem` and child-Chat
    /// inheritance (Work Outline Milestone 3 stage 2).
    launch: crate::work_outline_launch::WorkItemLaunchService,
    /// `AgentAuthenticate` / `AgentToolCall` (Work Outline Milestone 4):
    /// token-authenticated agent tools, local to this engine.
    agent_tools: std::sync::Arc<crate::agent_tools::AgentToolsService>,
    /// `StartPrReview` (docs/pr-review.md): runs on the Space's owner.
    pr_review: crate::pr_review_launch::PrReviewLauncher,
    /// Linear integration (`LINEAR_*` methods; local, never forwarded).
    linear: crate::linear::LinearService,
    /// PR review sync (`PR_*` methods; local, never forwarded).
    pr_review_sync: crate::pr_review_sync::PrReviewSyncService,
    /// Planning artifacts (`*ARTIFACT*` methods; content-bearing ones are
    /// forwardable to the owning device).
    artifacts: crate::artifacts::ArtifactsService,
    /// Worker-event wakes for orchestrator chats (`*_WAKE_*` methods).
    orchestrator_wakes: crate::orchestrator_wakes::OrchestratorWakes,
    engine_info: EngineInfo,
}

impl EngineRpc {
    #[allow(clippy::too_many_arguments)] // engine assembly seam, not a public API
    pub fn new(
        sessions: SessionsEngine,
        doc_host: DocHost,
        workspace: WorkspaceHost,
        registry: std::sync::Arc<HarnessRegistry>,
        repos: Repos,
        workspace_files: crate::WorkspaceFiles,
        terminals: Terminals,
        project_actions: ProjectActionsStore,
        change_requests: CheckoutChangeRequests,
        diff_sync: CheckoutDiffSync,
        uploads: Uploads,
        agent_accounts: AgentAccounts,
        workspace_scope: WorkspaceScope,
    ) -> Self {
        let engine_info = EngineInfo {
            device_id: doc_host.device_id().to_string(),
            workspace_scope,
            cursor_sdk_version: Some(mast_harness::CursorHarness::sdk_version().into()),
            capabilities: mast_proto::capabilities::current(),
        };
        let work_outline = std::sync::Arc::new(crate::work_outline::WorkOutlineService::new(
            workspace.clone(),
        ));
        {
            // `keepInstructionSession` keeps what this device's Execution
            // Session received for Chats it hosts; other Chats fall back to
            // their host's reported Execution Session in the registry.
            let sessions = sessions.clone();
            let workspace = workspace.clone();
            let device_id = doc_host.device_id().to_string();
            work_outline.set_kept_session_resolver(std::sync::Arc::new(move |chat_id: &str| {
                let hosted_here = workspace
                    .chat(chat_id)
                    .ok()
                    .flatten()
                    .is_some_and(|chat| chat.device_id == device_id);
                if hosted_here {
                    sessions.kept_instruction_session(chat_id)
                } else {
                    None
                }
            }));
        }
        let execution_scope =
            crate::execution_scope::ExecutionScopeService::new(workspace.clone(), repos.clone());
        let launch = crate::work_outline_launch::WorkItemLaunchService::new(
            workspace.clone(),
            doc_host.clone(),
            sessions.clone(),
            doc_host
                .execution_scope()
                .cloned()
                .unwrap_or_else(|| execution_scope.clone()),
        );
        let diff_targets = diff_sync.ensure_diff_targets(|| {
            crate::diff_targets::DiffTargets::new(
                repos.clone(),
                workspace.clone(),
                doc_host.device_id(),
            )
        });
        let agent_tools = std::sync::Arc::new(crate::agent_tools::AgentToolsService::new(
            sessions.agent_tokens(),
            workspace.clone(),
            work_outline.clone(),
            Some(doc_host.clone()),
        ));
        agent_tools.set_sessions(sessions.clone());
        agent_tools.set_diff_targets(diff_targets.clone());
        agent_tools.set_diff_sync(diff_sync.clone());
        // Engine assembly replaces this with the shared service
        // (`with_pr_review_sync`); a bare RPC surface has no background loop.
        let pr_review_sync = crate::pr_review_sync::PrReviewSyncService::new(
            workspace.clone(),
            repos.clone(),
            crate::pr_review_sync::PrReviewSyncConfig::default(),
        );
        let pr_review = crate::pr_review_launch::PrReviewLauncher::new(
            workspace.clone(),
            doc_host.clone(),
            crate::pr_review_launch::PrReviewTools::default(),
        )
        .with_viewer(Self::viewer_of(&pr_review_sync))
        .with_sessions(sessions.clone())
        .with_diff_targets(diff_targets.clone());
        // Engine assembly replaces this with the shared service
        // (`with_artifacts`), so the UI's render registration and agent
        // tool calls meet in one broker.
        let artifacts = crate::artifacts::ArtifactsService::new(workspace.clone());
        agent_tools.set_artifacts(artifacts.clone());
        // Engine assembly replaces this with the shared, watching service
        // (`with_orchestrator_wakes`); a bare surface only answers the RPCs.
        let orchestrator_wakes = crate::orchestrator_wakes::OrchestratorWakes::new(
            sessions.clone(),
            doc_host.clone(),
            workspace.clone(),
            None,
            crate::orchestrator_wakes::system_clock(),
        );
        Self {
            sessions,
            doc_host,
            workspace,
            registry,
            repos,
            workspace_files,
            terminals,
            project_actions,
            previews: None,
            change_requests,
            diff_sync,
            diff_targets,
            uploads,
            agent_accounts,
            managed_accounts: None,
            auth: None,
            links: None,
            updater: None,
            local_import: None,
            external_import: None,
            pr_ticket_cache: None,
            pr_dash: std::sync::OnceLock::new(),
            context_usage: None,
            subagent_transcript_cache: crate::subagent_transcript::TranscriptCache::new(),
            work_outline,
            execution_scope,
            launch,
            agent_tools,
            pr_review,
            linear: crate::linear::LinearService::default(),
            pr_review_sync,
            artifacts,
            orchestrator_wakes,
            engine_info,
        }
    }

    /// Share the engine's artifact service (render broker, watcher cache)
    /// with this RPC surface and its agent tools.
    pub fn with_artifacts(mut self, artifacts: crate::artifacts::ArtifactsService) -> Self {
        self.agent_tools.set_artifacts(artifacts.clone());
        self.artifacts = artifacts;
        self
    }

    /// Share the engine's orchestrator-wake service (its watchers run in the
    /// engine, not per RPC surface).
    pub fn with_orchestrator_wakes(
        mut self,
        wakes: crate::orchestrator_wakes::OrchestratorWakes,
    ) -> Self {
        self.orchestrator_wakes = wakes;
        self
    }

    /// Run a blocking artifact operation, mapping its error to the
    /// `artifact/<code>` string.
    async fn artifact_reply<T, F>(&self, f: F) -> Result<RpcReply, RpcError>
    where
        T: serde::Serialize + Send + 'static,
        F: FnOnce(crate::artifacts::ArtifactsService) -> Result<T, mast_proto::ArtifactError>
            + Send
            + 'static,
    {
        let service = self.artifacts.clone();
        match tokio::task::spawn_blocking(move || f(service))
            .await
            .map_err(|e| RpcError::Failed(e.to_string()))?
        {
            Ok(value) => RpcReply::value(&value),
            Err(error) => Err(RpcError::Failed(error.to_wire_message())),
        }
    }

    /// Share the engine's PR review sync (caches, tools) with the RPC surface.
    pub fn with_pr_review_sync(
        mut self,
        service: crate::pr_review_sync::PrReviewSyncService,
    ) -> Self {
        self.pr_review = self
            .pr_review
            .clone()
            .with_viewer(Self::viewer_of(&service));
        self.pr_review_sync = service;
        self
    }

    /// The launcher's view of the sync service's cached viewer login.
    fn viewer_of(
        service: &crate::pr_review_sync::PrReviewSyncService,
    ) -> crate::pr_review_launch::ViewerLogin {
        let service = service.clone();
        std::sync::Arc::new(move || {
            let service = service.clone();
            Box::pin(async move { service.viewer_login().await.ok() })
        })
    }

    /// Run a GitHub call, mapping its error to the `github/<code>` string.
    fn github_reply<T: serde::Serialize>(
        result: Result<T, mast_proto::GithubError>,
    ) -> Result<RpcReply, RpcError> {
        match result {
            Ok(value) => RpcReply::value(&value),
            Err(error) => Err(RpcError::Failed(error.to_wire_message())),
        }
    }

    pub fn with_managed_accounts(
        mut self,
        accounts: crate::managed_accounts::ManagedAccounts,
    ) -> Self {
        self.managed_accounts = Some(accounts);
        self
    }

    /// The managed snapshot plus the saved logins the picker may promote.
    async fn managed_snapshot(
        &self,
        force_usage: bool,
    ) -> Result<mast_proto::ManagedAccountsSnapshot, RpcError> {
        let managed = self.managed()?;
        let mut snapshot = managed.snapshot();
        snapshot.saved = self.agent_accounts.saved_options();
        // Usage per managed account: probed with the account's own token
        // (never refreshed); cache-only unless the caller forces it.
        for account in snapshot.accounts.iter_mut() {
            if !account.signed_in {
                account.usage_note = Some("not signed in".into());
                continue;
            }
            let (harness, credentials) = match managed.credentials_or_reason(&account.id) {
                Ok(found) => found,
                Err(reason) => {
                    account.usage_note = Some(reason);
                    continue;
                }
            };
            match self
                .agent_accounts
                .managed_usage(harness, &account.id, &credentials, force_usage)
                .await
            {
                Some((windows, plan)) => {
                    account.usage_windows = windows;
                    account.plan_label = plan;
                }
                None => {
                    account.usage_note = self
                        .agent_accounts
                        .managed_usage_note(harness, &account.id)
                        // Cache-only list with nothing cached yet.
                        .or_else(|| (!force_usage).then(|| "not probed yet".to_string()));
                }
            }
        }
        Ok(snapshot)
    }

    fn managed(&self) -> Result<&crate::managed_accounts::ManagedAccounts, RpcError> {
        self.managed_accounts
            .as_ref()
            .ok_or_else(|| RpcError::Failed("per-project accounts are not available here".into()))
    }

    /// Share the engine's Linear service (key, caches) with the RPC surface
    /// and the agent tools.
    pub fn with_linear(mut self, linear: crate::linear::LinearService) -> Self {
        self.agent_tools.set_linear(linear.clone());
        self.linear = linear;
        self
    }

    /// Run a Linear call, mapping its error to the `linear/<code>` string.
    async fn linear_reply<T: serde::Serialize>(
        result: Result<T, mast_proto::LinearError>,
    ) -> Result<RpcReply, RpcError> {
        match result {
            Ok(value) => RpcReply::value(&value),
            Err(error) => Err(RpcError::Failed(error.to_wire_message())),
        }
    }

    fn linear_for_space(&self, space_id: Option<&str>, expected_account_id: Option<&str>) -> Result<crate::linear::LinearService, RpcError> {
        let Some(space_id) = space_id else { return Ok(self.linear.clone()); };
        if self.workspace.space(space_id).map_err(|_| RpcError::Failed("Could not read Space".into()))?.is_none() {
            return Err(RpcError::Failed("linear/notFound: Space not found".into()));
        }
        let account = self.workspace.space_linear_account_id(space_id);
        if expected_account_id.is_some_and(|expected| expected != account.as_deref().unwrap_or("default")) {
            return Err(RpcError::Failed("linear/network: Linear account changed; retry the request".into()));
        }
        Ok(self.linear.for_account(account.as_deref()))
    }

    /// `linear/disabledForSpace` for a Space-scoped Linear mutation in a
    /// Space with Linear off.
    fn linear_mutation_gate(
        &self,
        mutation: &mast_proto::WorkOutlineMutation,
    ) -> Result<(), RpcError> {
        if !mutation.requires_linear_enabled() {
            return Ok(());
        }
        let space = match mutation {
            mast_proto::WorkOutlineMutation::CreateWorkItemFromLinear(p) => {
                Some(p.space_id.clone())
            }
            mast_proto::WorkOutlineMutation::LinkLinearProject(p) => {
                self.workspace.work_item_space(&p.work_item_id)
            }
            _ => None,
        };
        // An unknown item or Space falls through to the registry's error.
        match space {
            Some(space)
                if self.workspace.space(&space).ok().flatten().is_some()
                    && !self.workspace.space_linear_enabled(&space) =>
            {
                Err(RpcError::Failed(
                    mast_proto::LinearError::disabled_for_space().to_wire_message(),
                ))
            }
            _ => Ok(()),
        }
    }

    /// Replace the external tools `StartPrReview` shells out to (tests point
    /// `gh` at a fake and pin `WORKSPACE_WORKTREES_DIR`).
    pub fn with_pr_review_tools(mut self, tools: crate::pr_review_launch::PrReviewTools) -> Self {
        self.pr_review = self.pr_review.clone().with_tools(tools);
        self
    }

    /// The token-authenticated agent tool surface.
    pub fn agent_tools(&self) -> &std::sync::Arc<crate::agent_tools::AgentToolsService> {
        &self.agent_tools
    }

    /// The Work Outline service (agent tools call `authorize_agent`,
    /// `agent_context`, and `mutate_as` with an agent actor).
    pub fn work_outline(&self) -> std::sync::Arc<crate::work_outline::WorkOutlineService> {
        self.work_outline.clone()
    }

    pub fn with_previews(mut self, previews: mast_preview::PreviewService) -> Self {
        self.previews = Some(previews);
        self
    }

    /// Attach the auth service (AuthStatus + AuthRpc mutations).
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Attach the peer link cache — enables `targetDeviceId` relay forwarding.
    pub fn with_links(mut self, links: std::sync::Arc<LinkCache>) -> Self {
        self.links = Some(links);
        self
    }

    /// Attach the release checker (UpdateStatus stream + ApplyUpdate).
    pub fn with_updater(mut self, updater: mast_update::Updater) -> Self {
        self.updater = Some(updater);
        self
    }

    /// Attach the local→synced profile importer (synced runtimes only).
    pub fn with_local_import(mut self, importer: crate::local_import::LocalImporter) -> Self {
        self.local_import = Some(importer);
        self
    }

    /// Attach the external-session importer. Every runtime has one (unlike
    /// `local_import`, not scope-gated) — `EngineCore::rpc_service` always
    /// calls this.
    pub fn with_external_import(
        mut self,
        importer: crate::external_import::ExternalSessionImporter,
    ) -> Self {
        self.external_import = Some(importer);
        self
    }

    /// Attach the PR/ticket linkage cache (ticket 002 phase 2). Every
    /// runtime has one (unconditional, like `external_import`) —
    /// `EngineCore::rpc_service` always calls this.
    pub fn with_pr_ticket_cache(mut self, cache: crate::pr_ticket_cache::PrTicketCache) -> Self {
        self.agent_tools.set_pr_ticket_cache(cache.clone());
        self.pr_ticket_cache = Some(cache);
        self
    }

    /// Attach the context-usage provider (`ChatContextUsage`). Every runtime
    /// has one (unconditional, like `external_import`/`pr_ticket_cache`) —
    /// `EngineCore::rpc_service` always calls this.
    pub fn with_context_usage(
        mut self,
        provider: crate::context_usage::ContextUsageProvider,
    ) -> Self {
        self.context_usage = Some(provider);
        self
    }

    fn auth(&self) -> Result<&Auth, RpcError> {
        self.auth
            .as_ref()
            .ok_or_else(|| RpcError::Failed("auth unavailable".into()))
    }

    fn updater(&self) -> Result<&mast_update::Updater, RpcError> {
        self.updater
            .as_ref()
            .ok_or_else(|| RpcError::Failed("updates unavailable".into()))
    }

    fn local_importer(&self) -> Result<&crate::local_import::LocalImporter, RpcError> {
        self.local_import
            .as_ref()
            .ok_or_else(|| RpcError::Failed("local import requires a synced workspace".into()))
    }

    /// Whether the addressed chat or a process in `cwd` is live. Repo Map
    /// may close an unmatched worktree without a chat id; that path still
    /// receives the cwd-based process protection.
    async fn closeout_is_live(&self, chat_id: Option<&str>, cwd: &str) -> bool {
        if let Some(chat_id) = chat_id
            && (self.sessions.has_live_run(chat_id) || self.sessions.turn_in_flight(chat_id)) {
                return true;
            }
        let session_id = chat_id
            .and_then(|chat_id| self.workspace.chat(chat_id).ok().flatten())
            .and_then(|c| c.harness_session_id)
            .unwrap_or_default();
        let cwd = cwd.to_string();
        tokio::task::spawn_blocking(move || {
            crate::liveness::live_process_matches(&session_id, &cwd)
        })
        .await
        .unwrap_or(false)
    }

    fn validated_chat_closeout_cwd(
        &self,
        chat_id: &str,
        requested_cwd: &str,
    ) -> Result<std::path::PathBuf, RpcError> {
        let chat = self
            .workspace
            .chat(chat_id)
            .map_err(|e| RpcError::Failed(e.to_string()))?
            .ok_or_else(|| RpcError::Failed(format!("cannot close out unknown chat {chat_id}")))?;
        validated_closeout_cwd(chat_id, chat.cwd.as_deref(), requested_cwd)
    }

    fn resolved_closeout_cwd(
        &self,
        chat_id: Option<&str>,
        requested_cwd: &str,
    ) -> Result<std::path::PathBuf, RpcError> {
        match chat_id {
            Some(chat_id) => self.validated_chat_closeout_cwd(chat_id, requested_cwd),
            None => std::fs::canonicalize(requested_cwd).map_err(|error| {
                RpcError::Failed(format!(
                    "cannot close out requested working directory `{requested_cwd}`: {error}"
                ))
            }),
        }
    }

    /// Every non-archived local chat currently attached to the same canonical
    /// checkout as `cwd`, except `excluded_chat_id` when this is a chat-bound
    /// close-out. A worktree is shared filesystem state, even when the
    /// sessions using it are idle.
    async fn open_chats_on_checkout(
        &self,
        excluded_chat_id: Option<&str>,
        cwd: &std::path::Path,
    ) -> Result<Vec<crate::chat_workspace_plan::CloseoutChatReference>, RpcError> {
        let target =
            self.repos.checkout_identity(cwd).await.map_err(|e| {
                RpcError::Failed(format!("could not resolve close-out checkout: {e}"))
            })?;
        let chats = self
            .workspace
            .read_chats()
            .map_err(|e| RpcError::Failed(e.to_string()))?;
        let mut shared = Vec::new();
        for chat in chats {
            if excluded_chat_id == Some(chat.id.as_str())
                || chat.archived
                || chat.device_id != self.engine_info.device_id
            {
                continue;
            }
            let Some(other_cwd) = chat.cwd.as_deref() else {
                continue;
            };
            // Resolve the current cwd first because checkoutId is reconciled
            // asynchronously and can briefly describe the chat's previous
            // cwd after a retarget. A cwd below the worktree root is also
            // affected by removing that worktree. Do not run git against
            // every chat cwd here: besides being slow, probing arbitrary
            // home folders can trigger platform privacy prompts.
            let other_path = std::path::Path::new(other_cwd);
            let same_checkout = if crate::repos::is_automatic_access_blocked(other_path) {
                // Never touch broad/privacy-sensitive historical chat paths
                // merely because the user is closing an unrelated worktree.
                chat.checkout_id.as_deref() == Some(target.id.as_str())
            } else {
                match std::fs::canonicalize(other_path) {
                    Ok(path) => path == target.root || path.starts_with(&target.root),
                    // If the path disappeared between plan and close, the stored
                    // identity is a conservative fallback: refusing is safer than
                    // deleting a worktree another open chat may still reference.
                    Err(_) => chat.checkout_id.as_deref() == Some(target.id.as_str()),
                }
            };
            if same_checkout {
                shared.push(crate::chat_workspace_plan::CloseoutChatReference {
                    id: chat.id,
                    title: chat
                        .title
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or_else(|| "Untitled chat".into()),
                });
            }
        }
        Ok(shared)
    }

    fn external_importer(
        &self,
    ) -> Result<&crate::external_import::ExternalSessionImporter, RpcError> {
        self.external_import
            .as_ref()
            .ok_or_else(|| RpcError::Failed("external import unavailable".into()))
    }

    fn pr_ticket_cache(&self) -> Result<&crate::pr_ticket_cache::PrTicketCache, RpcError> {
        self.pr_ticket_cache
            .as_ref()
            .ok_or_else(|| RpcError::Failed("PR/ticket cache unavailable".into()))
    }

    fn context_usage(&self) -> Result<&crate::context_usage::ContextUsageProvider, RpcError> {
        self.context_usage
            .as_ref()
            .ok_or_else(|| RpcError::Failed("context usage provider unavailable".into()))
    }

    fn local_project_action_space(&self, space_id: &str) -> Result<Space, RpcError> {
        let space = self
            .workspace
            .space(space_id)
            .map_err(|err| RpcError::Failed(err.to_string()))?
            .ok_or_else(|| RpcError::Failed("Project space not found".into()))?;
        if space.device_id != self.doc_host.device_id() {
            return Err(RpcError::Failed(
                "Project space belongs to another device".into(),
            ));
        }
        Ok(space)
    }

    /// Resolve a mention-search root from synced workspace rows. A client may
    /// name an existing linked worktree for a new chat, but it is verified
    /// against the space repository before any filesystem walk begins.
    async fn file_search_root(&self, p: &FileSearchParams) -> Result<std::path::PathBuf, RpcError> {
        let target = mast_proto::WorkspaceTarget {
            chat_id: p.chat_id.clone(),
            space_id: p.space_id.clone(),
            checkout_path: p.path.clone(),
        };
        self.workspace_files
            .resolve_target(&target)
            .await
            .map(|workspace| workspace.root)
            .map_err(Into::into)
    }

    /// Accept only a checkout already named by a local chat or contained in a
    /// local space. Remote clients must not turn this RPC into an arbitrary path probe.
    async fn change_request_root(&self, cwd: &str) -> Result<std::path::PathBuf, RpcError> {
        crate::git_gate::scope("change_request_root", self.change_request_root_inner(cwd)).await
    }

    async fn change_request_root_inner(&self, cwd: &str) -> Result<std::path::PathBuf, RpcError> {
        let requested = std::path::PathBuf::from(cwd);
        let unknown = || RpcError::BadParams("cwd is not a known checkout on this device".into());
        let local_device = self.doc_host.device_id();
        let mut chats_rx = self.workspace.watch_chats();
        let mut spaces_rx = self.workspace.watch_spaces();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
        // Canonicalized lazily, once; a path that does not resolve cannot be
        // any Space's checkout, so it never reaches git.
        let mut canonical: Option<Option<std::path::PathBuf>> = None;
        loop {
            let chats = chats_rx.borrow_and_update().clone();
            if chats.iter().any(|chat| {
                chat.device_id == local_device
                    && (chat.cwd.as_deref().map(std::path::Path::new) == Some(requested.as_path())
                        || chat.source_context.as_ref().is_some_and(|source| {
                            std::path::Path::new(&source.repo_root) == requested.as_path()
                        }))
            }) {
                return if crate::repos::Repos::path_exists(&requested).await {
                    Ok(requested)
                } else {
                    Err(unknown())
                };
            }

            let spaces = spaces_rx.borrow_and_update().clone();
            for space in spaces
                .iter()
                .filter(|space| space.device_id == local_device)
            {
                if !space.git_detected {
                    if let Some(folder) = self
                        .repos
                        .same_folder(std::path::Path::new(&space.path), &requested)
                        .await
                    {
                        return Ok(folder);
                    }
                    continue;
                }
                if canonical.is_none() {
                    canonical = Some(self.repos.canonical_path(&requested).await);
                }
                let Some(Some(candidate)) = canonical.as_ref() else {
                    return Err(unknown());
                };
                if let Some(checkout) = self
                    .repos
                    .workspace_checkout_canonical(
                        std::path::Path::new(&space.path),
                        candidate.clone(),
                    )
                    .await
                {
                    return Ok(checkout);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::select! {
                _ = chats_rx.changed() => {}
                _ = spaces_rx.changed() => {}
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        Err(unknown())
    }

    /// Most-recent-first paths the current chat actually touched, followed by
    /// files still changed in its checkout. The search worker validates and
    /// normalizes them against the resolved root before using them as ranking
    /// hints, so stale or out-of-workspace tool paths simply disappear.
    fn featured_file_paths(&self, chat_id: &str) -> Vec<String> {
        let mut paths = Vec::new();
        let mut seen = HashSet::new();
        if let Ok(handle) = self.doc_host.open(chat_id)
            && let Ok(entries) = handle.doc().read_entries()
        {
            for entry in entries.into_iter().rev() {
                for part in entry.parts.into_iter().rev() {
                    if let MessagePart::Tool { call, .. } = part
                        && let Some(path) = tool_file_path(&call)
                        && !path.trim().is_empty()
                        && seen.insert(path.to_string())
                    {
                        paths.push(path.to_string());
                        if paths.len() == FILE_SEARCH_FEATURED_PATHS {
                            break;
                        }
                    }
                }
                if paths.len() == FILE_SEARCH_FEATURED_PATHS {
                    break;
                }
            }
        }

        if let Ok(Some(chat)) = self.workspace.chat(chat_id) {
            let diffs = self.diff_sync.watch_diffs().borrow().clone();
            // Cwd is authoritative. Never let a stale checkoutId select an
            // old diff after retargeting, and never match another device by a
            // path string. Canonicalization makes symlink aliases and nested
            // chat cwds resolve to the checkout-root snapshot.
            let diff = chat.cwd.as_deref().and_then(|cwd| {
                let canonical = std::fs::canonicalize(cwd).ok()?;
                diffs.iter().find(|diff| {
                    if diff.device_id != self.engine_info.device_id {
                        return false;
                    }
                    let Ok(root) = std::fs::canonicalize(&diff.cwd) else {
                        return false;
                    };
                    (canonical == root || canonical.starts_with(&root))
                        && chat
                            .checkout_id
                            .as_deref()
                            .is_none_or(|id| id == diff.checkout_id)
                })
            });
            if let Some(diff) = diff {
                for file in &diff.files {
                    if paths.len() == FILE_SEARCH_FEATURED_PATHS {
                        break;
                    }
                    if seen.insert(file.path.clone()) {
                        paths.push(file.path.clone());
                    }
                }
            }
        }
        paths
    }

    /// Forward a device-addressed call over the target device's relay. On transport
    /// failure the cached link is invalidated so the next call re-dials.
    /// `ValidateExecutionConfiguration`: resolve (`scope`), read the Chat's
    /// captured binding (`chatId`), or take the carried snapshot
    /// (`resolved`), then validate on the Space's owning device. A Space
    /// owned elsewhere is forwarded WITH the resolved snapshot (the owner's
    /// replica may lag; audit section 4) when the owner advertises
    /// `work-outline-multi-repo-v1`; otherwise the reply carries
    /// `hostUnsupported` or `deviceOffline`. Validation failures are a
    /// normal reply (`ok: false`), not an RPC error.
    async fn validate_execution_configuration(
        &self,
        p: mast_proto::ValidateExecutionConfigurationParams,
    ) -> Result<mast_proto::ValidateExecutionConfigurationReply, RpcError> {
        use mast_proto::{
            ExecutionValidationError, ValidateExecutionConfigurationReply, WorkOutlineError,
            WorkOutlineErrorCode,
        };
        let resolved = match (p.resolved, p.scope, p.chat_id) {
            (Some(resolved), None, None) => resolved,
            (None, Some(scope), None) => self
                .workspace
                .resolve_execution(&scope)
                .map_err(|e| RpcError::Failed(e.to_wire_message()))?,
            (None, None, Some(chat_id)) => match self
                .workspace
                .chat_execution_binding_state(&chat_id)
                .checked()
            {
                // A malformed binding is bound-and-invalid: report it as a
                // validation failure, never as "no binding".
                Err(error) => {
                    return Ok(ValidateExecutionConfigurationReply::from_result(
                        None,
                        Err(vec![error]),
                    ));
                }
                Ok(binding) => binding.map(|binding| binding.resolved).ok_or_else(|| {
                    RpcError::Failed(
                        WorkOutlineError::new(
                            WorkOutlineErrorCode::NotFound,
                            "This Chat has no captured execution binding",
                        )
                        .to_wire_message(),
                    )
                })?,
            },
            _ => {
                return Err(RpcError::BadParams(
                    "set exactly one of scope, chatId, resolved".into(),
                ));
            }
        };
        let owner = resolved.device_id.clone();
        if !owner.is_empty() && owner != self.doc_host.device_id() {
            let fail = |error: ExecutionValidationError| {
                Ok(ValidateExecutionConfigurationReply::from_result(
                    Some(resolved.clone()),
                    Err(vec![error]),
                ))
            };
            let device = self
                .workspace
                .read_devices()
                .ok()
                .and_then(|devices| devices.into_iter().find(|d| d.id == owner));
            let Some(device) = device else {
                return fail(ExecutionValidationError::DeviceOffline {
                    device_id: owner,
                    detail: "the device is not known to this engine".into(),
                });
            };
            if !device.supports(mast_proto::capabilities::WORK_OUTLINE_MULTI_REPO_V1) {
                return fail(ExecutionValidationError::HostUnsupported {
                    device_id: owner,
                    capability: mast_proto::capabilities::WORK_OUTLINE_MULTI_REPO_V1.into(),
                });
            }
            let forwarded = mast_proto::ValidateExecutionConfigurationParams {
                resolved: Some(resolved.clone()),
                host_capabilities: p.host_capabilities,
                target_device_id: Some(owner.clone()),
                ..Default::default()
            };
            let params = serde_json::to_value(&forwarded)
                .map_err(|e| RpcError::Failed(format!("serialize params: {e}")))?;
            return match self
                .forward(&owner, methods::VALIDATE_EXECUTION_CONFIGURATION, params)
                .await
            {
                Ok(RpcReply::Value(value)) => serde_json::from_value(value)
                    .map_err(|e| RpcError::Failed(format!("bad validation reply: {e}"))),
                Ok(RpcReply::Stream(_)) => Err(RpcError::Failed("unexpected stream reply".into())),
                Err(error) => fail(ExecutionValidationError::DeviceOffline {
                    device_id: owner,
                    detail: error.to_string(),
                }),
            };
        }
        let result = self
            .execution_scope
            .validate_resolved(&resolved, p.host_capabilities)
            .await;
        Ok(ValidateExecutionConfigurationReply::from_result(
            Some(resolved),
            result,
        ))
    }

    /// `CreateChatForWorkItem`: run the launch workflow when this device
    /// owns the work item's Space, else forward it to the owner. A fresh
    /// launch is blocked (`execution-scope:deviceOffline`) when the owner
    /// cannot be reached (contract decision 6.4.1) and never runs locally.
    async fn create_chat_for_work_item(
        &self,
        p: mast_proto::CreateChatForWorkItemParams,
    ) -> Result<mast_proto::CreateChatForWorkItemReply, RpcError> {
        use mast_proto::{ExecutionValidationError, WorkOutlineError, WorkOutlineErrorCode};
        let fail = |error: WorkOutlineError| RpcError::Failed(error.to_wire_message());
        let blocked = |error: ExecutionValidationError| {
            fail(WorkOutlineError::new(
                WorkOutlineErrorCode::Invalid,
                crate::execution_scope::rejection_message(&[error]),
            ))
        };
        let owner = self
            .workspace
            .work_item_space(&p.work_item_id)
            .and_then(|space_id| self.workspace.space(&space_id).ok().flatten())
            .map(|space| space.device_id)
            .ok_or_else(|| {
                fail(WorkOutlineError::new(
                    WorkOutlineErrorCode::NotFound,
                    "Work item not found",
                ))
            })?;
        if owner != self.doc_host.device_id() {
            let device = self
                .workspace
                .read_devices()
                .ok()
                .and_then(|devices| devices.into_iter().find(|d| d.id == owner));
            let Some(device) = device else {
                return Err(blocked(ExecutionValidationError::DeviceOffline {
                    device_id: owner,
                    detail: "the device is not known to this engine".into(),
                }));
            };
            if !device.supports(mast_proto::capabilities::WORK_OUTLINE_LAUNCH_V1) {
                return Err(blocked(ExecutionValidationError::HostUnsupported {
                    device_id: owner,
                    capability: mast_proto::capabilities::WORK_OUTLINE_LAUNCH_V1.into(),
                }));
            }
            let mut forwarded = p.clone();
            forwarded.target_device_id = Some(owner.clone());
            let params = serde_json::to_value(&forwarded)
                .map_err(|e| RpcError::Failed(format!("serialize params: {e}")))?;
            return match self
                .forward(&owner, methods::CREATE_CHAT_FOR_WORK_ITEM, params)
                .await
            {
                Ok(RpcReply::Value(value)) => serde_json::from_value(value)
                    .map_err(|e| RpcError::Failed(format!("bad launch reply: {e}"))),
                Ok(RpcReply::Stream(_)) => Err(RpcError::Failed("unexpected stream reply".into())),
                // The owner answered with a typed failure: pass it through.
                Err(RpcError::Failed(message)) if message.contains("work-outline/") => {
                    Err(RpcError::Failed(message))
                }
                Err(error) => Err(blocked(ExecutionValidationError::DeviceOffline {
                    device_id: owner,
                    detail: error.to_string(),
                })),
            };
        }
        self.launch
            .create_chat_for_work_item(&p)
            .await
            .map_err(fail)
    }

    /// `StartPrReview`: run on this device when it owns the Space (pinned or
    /// found by remote URL among this device's Spaces); otherwise forward to
    /// the pinned Space's owner, or try every other device that owns Spaces
    /// and advertises `pr-review-launch-v1`. A forwarded call (it carries
    /// `targetDeviceId`) never fans out again.
    async fn start_pr_review(
        &self,
        p: mast_proto::StartPrReviewParams,
    ) -> Result<mast_proto::StartPrReviewReply, RpcError> {
        use mast_proto::{PrReviewError, PrReviewErrorCode};
        let fail = |error: PrReviewError| RpcError::Failed(error.to_wire_message());
        let local = self.doc_host.device_id().to_owned();
        let routed = p.target_device_id.is_some();
        if let Some(space_id) = &p.space_id
            && let Some(space) = self.workspace.space(space_id).ok().flatten()
            && space.device_id != local
        {
            if routed {
                return Err(fail(PrReviewError::new(
                    PrReviewErrorCode::DeviceOffline,
                    "The Space lives on another device",
                )));
            }
            return self.forward_pr_review(&space.device_id, &p).await;
        }
        let local_error = match self.pr_review.start(&p).await {
            Ok(reply) => return Ok(reply),
            // Only an unpinned start looks on other devices.
            Err(error)
                if error.code == PrReviewErrorCode::RepoNotFound
                    && !routed
                    && p.space_id.is_none() =>
            {
                error
            }
            Err(error) => return Err(fail(error)),
        };
        // Not here: ask the other devices that own Spaces.
        let mut devices: Vec<String> = self
            .workspace
            .read_spaces()
            .unwrap_or_default()
            .into_iter()
            .filter(|space| space.device_id != local)
            .map(|space| space.device_id)
            .collect();
        devices.sort();
        devices.dedup();
        let mut unreachable = Vec::new();
        for device in devices {
            match self.forward_pr_review(&device, &p).await {
                Ok(reply) => return Ok(reply),
                Err(RpcError::Failed(message)) => {
                    match PrReviewError::from_wire_message(&message) {
                        Some(error) if error.code == PrReviewErrorCode::RepoNotFound => {}
                        Some(error) if error.code == PrReviewErrorCode::DeviceOffline => {
                            unreachable.push(self.device_label(&device));
                        }
                        _ => return Err(RpcError::Failed(message)),
                    }
                }
                Err(_) => unreachable.push(self.device_label(&device)),
            }
        }
        let mut message = local_error.message.clone();
        if !unreachable.is_empty() {
            message.push_str(&format!(
                ". Could not ask {} (offline or too old to start PR reviews)",
                unreachable.join(", ")
            ));
        }
        Err(fail(PrReviewError::new(
            PrReviewErrorCode::RepoNotFound,
            message,
        )))
    }

    fn device_label(&self, device_id: &str) -> String {
        self.workspace
            .read_devices()
            .ok()
            .and_then(|devices| devices.into_iter().find(|d| d.id == device_id))
            .map(|d| d.name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| device_id.to_owned())
    }

    /// Forward `StartPrReview` to `device` (which must advertise
    /// `pr-review-launch-v1`). Typed `pr-review/…` failures pass through;
    /// anything else is `deviceOffline`.
    async fn forward_pr_review(
        &self,
        device: &str,
        p: &mast_proto::StartPrReviewParams,
    ) -> Result<mast_proto::StartPrReviewReply, RpcError> {
        use mast_proto::{PrReviewError, PrReviewErrorCode};
        let offline = |detail: String| {
            RpcError::Failed(
                PrReviewError::new(
                    PrReviewErrorCode::DeviceOffline,
                    format!(
                        "{} can't start the review: {detail}",
                        self.device_label(device)
                    ),
                )
                .to_wire_message(),
            )
        };
        let known = self
            .workspace
            .read_devices()
            .ok()
            .and_then(|devices| devices.into_iter().find(|d| d.id == device));
        match known {
            None => return Err(offline("the device is not known to this engine".into())),
            Some(known) if !known.supports(mast_proto::capabilities::PR_REVIEW_LAUNCH_V1) => {
                return Err(offline("update Mast on that device".into()));
            }
            Some(_) => {}
        }
        let mut forwarded = p.clone();
        forwarded.target_device_id = Some(device.to_owned());
        let params = serde_json::to_value(&forwarded)
            .map_err(|e| RpcError::Failed(format!("serialize params: {e}")))?;
        match self.forward(device, methods::START_PR_REVIEW, params).await {
            Ok(RpcReply::Value(value)) => serde_json::from_value(value)
                .map_err(|e| RpcError::Failed(format!("bad StartPrReview reply: {e}"))),
            Ok(RpcReply::Stream(_)) => Err(RpcError::Failed("unexpected stream reply".into())),
            Err(RpcError::Failed(message)) if message.contains(PrReviewError::WIRE_PREFIX) => {
                Err(RpcError::Failed(message))
            }
            Err(error) => Err(offline(error.to_string())),
        }
    }

    async fn forward(
        &self,
        target: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        let Some(links) = &self.links else {
            return Err(RpcError::Failed(format!(
                "cannot reach device {target}: remote routing unavailable (offline)"
            )));
        };
        let client = links.client(target).await?;
        if is_stream_method(method) {
            // Streams are unbounded by design (a quiet WATCH_* is healthy);
            // only unary calls below get the reply deadline.
            if matches!(
                method,
                methods::WATCH_CHECKOUT_CHANGE_REQUEST | methods::WATCH_WORKSPACE_GIT_STATUS
            ) {
                let rx = match client.subscribe_checked(method, params).await {
                    Ok(rx) => rx,
                    Err(err) => {
                        if should_invalidate_link(&err) {
                            links.invalidate(target);
                        }
                        return Err(err);
                    }
                };
                let stream = futures::stream::unfold((rx, client), |(mut rx, client)| async move {
                    rx.recv().await.map(|item| (item, (rx, client)))
                });
                return Ok(RpcReply::Stream(stream.boxed()));
            }
            let rx = match client.subscribe(method, params).await {
                Ok(rx) => rx,
                Err(err) => {
                    if should_invalidate_link(&err) {
                        links.invalidate(target);
                    }
                    return Err(err);
                }
            };
            // Pipe remote items; the held client keeps the link's RpcClient alive for
            // the stream's lifetime. A remote error just ends the stream (the relay
            // link-down path fails pending calls; stream receivers close).
            let stream = futures::stream::unfold((rx, client), |(mut rx, client)| async move {
                rx.recv().await.map(|item| (item, (rx, client)))
            });
            return Ok(RpcReply::Stream(stream.boxed()));
        }
        let deadline = forward_deadline(method);
        match tokio::time::timeout(deadline, client.call(method, params)).await {
            Ok(Ok(value)) => Ok(RpcReply::Value(value)),
            Ok(Err(err)) => {
                if should_invalidate_link(&err) {
                    links.invalidate(target);
                }
                Err(err)
            }
            Err(_) => {
                // No reply inside the deadline. The link may be a zombie — the
                // relay's auto-pong keeps a dead host socket looking alive
                // (ws3 auto-pong incident) — so drop it; the next call re-dials.
                // NOTE: the remote may still complete the forwarded work; the
                // caller sees a retryable failure instead of hanging forever
                // (the "Sending…" wedge, 2026-08-18).
                links.invalidate(target);
                Err(RpcError::Transport(format!(
                    "no reply from device {target} for {method} within {}s",
                    deadline.as_secs()
                )))
            }
        }
    }

    async fn mutate(&self, params: MutateParams) -> Result<(), RpcError> {
        let failed = |e: crate::EngineError| RpcError::Failed(e.to_string());
        match params {
            MutateParams::CreateChat {
                chat_id,
                space_id,
                device_id,
                config,
                branch,
                cwd,
                parent_chat_id,
            } => {
                let parent_chat_id = parent_chat_id.filter(|p| !p.trim().is_empty());
                // A child of a bound Chat in the same Space starts in the
                // parent's execution root by default (acceptance 19), so it
                // can inherit the binding; an explicit other cwd opts out of
                // the binding (assignment and revision still inherit).
                let cwd = match (&cwd, &parent_chat_id) {
                    (None, Some(parent)) => self
                        .workspace
                        .chat_execution_binding(parent)
                        .filter(|binding| space_id.as_deref() == Some(binding.space_id.as_str()))
                        .map(|binding| binding.resolved.configuration.execution_root)
                        .or(cwd),
                    _ => cwd,
                };
                self.workspace
                    .create_chat_with_parent(
                        &chat_id,
                        space_id.as_deref(),
                        device_id.as_deref(),
                        config,
                        cwd,
                        parent_chat_id.clone(),
                    )
                    .map_err(failed)?;
                if let Some(branch) = branch.as_deref().filter(|b| !b.is_empty()) {
                    self.workspace
                        .set_chat_branch(&chat_id, branch)
                        .map_err(failed)?;
                }
                if let Some(parent) = parent_chat_id
                    && let Err(err) = self.launch.inherit_from_parent(&chat_id, &parent)
                {
                    // The Chat exists; a failed inheritance must not make a
                    // caller retry into a duplicate Chat.
                    tracing::warn!(chat = %chat_id, parent = %parent, error = %err,
                        "child chat work-outline inheritance failed");
                }
                Ok(())
            }
            MutateParams::CreateSpace {
                space_id,
                device_id,
                path,
                name,
                git_detected,
            } => self
                .workspace
                .create_space(&space_id, &device_id, &path, name, git_detected)
                .map_err(failed),
            MutateParams::RenameSpace { space_id, name } => self
                .workspace
                .rename_space(&space_id, name.as_deref())
                .map_err(failed)
                .map(drop),
            MutateParams::DeleteSpace { space_id } => {
                let deleted = self.workspace.delete_space(&space_id).map_err(failed)?;
                // Best-effort teardown of live runs we host for the deleted chats
                // (the doc rows are already tombstoned; a straggler run would only
                // write into an orphaned session doc).
                let sessions = self.sessions.clone();
                let doc_host = self.doc_host.clone();
                let chat_ids = deleted.chat_ids;
                tokio::spawn(async move {
                    for chat_id in chat_ids {
                        if let Err(err) = sessions.interrupt(&chat_id).await {
                            tracing::debug!(chat = %chat_id, error = %err, "deleteSpace interrupt skipped");
                        }
                        doc_host.purge_chat(&chat_id);
                    }
                });
                Ok(())
            }
            MutateParams::RenameChat { chat_id, title } => self
                .workspace
                .rename_chat(&chat_id, &title)
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatBranch { chat_id, branch } => self
                .workspace
                .set_chat_branch(&chat_id, &branch)
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatCwd { chat_id, cwd } => {
                // The cwd is authoritative. Resolve it on the chat's owning
                // device, then commit cwd + checkoutId together. When it is an
                // imported/missing/non-Git path, keep the requested cwd but
                // explicitly clear the previous id.
                let chat = self.workspace.chat(&chat_id).map_err(failed)?;
                let is_local = chat
                    .as_ref()
                    .is_none_or(|chat| chat.device_id == self.engine_info.device_id);
                let path = std::path::Path::new(&cwd);
                let resolved = if is_local && !crate::repos::is_automatic_access_blocked(path) {
                    self.repos.checkout_identity(path).await.ok()
                } else {
                    None
                };
                let canonical_cwd = if resolved.is_some() {
                    std::fs::canonicalize(path)
                        .unwrap_or_else(|_| path.to_path_buf())
                        .to_string_lossy()
                        .into_owned()
                } else {
                    cwd
                };
                self.workspace
                    .set_chat_target(
                        &chat_id,
                        &canonical_cwd,
                        resolved.as_ref().map(|identity| identity.id.as_str()),
                    )
                    .map_err(failed)
                    .map(drop)
            }
            MutateParams::SetChatActivity {
                chat_id,
                last_message_at,
                created_at,
            } => self
                .workspace
                .set_chat_activity(&chat_id, last_message_at, created_at)
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatHost { chat_id, device_id } => self
                .workspace
                .set_chat_host(&chat_id, &device_id)
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatArchived { chat_id, archived } => self
                .workspace
                .set_chat_archived(&chat_id, archived)
                .map_err(failed)
                .map(drop),
            MutateParams::ChangeSidebarPin { change } => {
                self.workspace.change_sidebar_pin(&change).map_err(failed)
            }
            MutateParams::SetChatConfig { chat_id, config } => self
                .workspace
                .set_chat_config(&chat_id, &config)
                .map_err(failed)
                .map(drop),
            MutateParams::SetChatAutoApprove {
                chat_id,
                auto_approve,
            } => self
                .workspace
                .set_chat_auto_approve(&chat_id, auto_approve)
                .map_err(failed)
                .map(drop),
            MutateParams::DeleteChat { chat_id } => {
                self.workspace.delete_chat(&chat_id).map_err(failed)?;
                self.doc_host.purge_chat(&chat_id);
                Ok(())
            }
            MutateParams::RenameDevice { device_id, name } => self
                .workspace
                .rename_device(&device_id, &name)
                .map_err(failed)
                .map(drop),
            MutateParams::MarkChatSeen { chat_id, at } => {
                let at = at
                    .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                    .unwrap_or_else(chrono::Utc::now);
                self.workspace
                    .mark_chat_seen(&chat_id, at)
                    .map_err(failed)
                    .map(drop)
            }
        }
    }
}

/// An RPC rejection is scoped to the requested capability. Only a broken
/// transport means the shared device link itself cannot carry other calls.
fn should_invalidate_link(error: &RpcError) -> bool {
    matches!(error, RpcError::Closed | RpcError::Transport(_))
}

/// Reply deadline for a relay-forwarded unary call. The relay is WebSocket
/// frames through a DO: a dropped frame (host socket replaced mid-call, DO
/// restart) loses the reply SILENTLY — the DO's auto-pong keeps the client
/// socket looking healthy — and an unbounded await wedged callers forever
/// (the composer's permanent "Sending…", 2026-08-18). Network-bound git and
/// update methods get a long leash; worktree creation checks out a full tree;
/// everything else is interactive and must fail fast.
async fn install_harness_with<F, Fut>(
    registry: &HarnessRegistry,
    harness: HarnessId,
    install: F,
) -> Result<Vec<crate::registry::HarnessDescriptor>, RpcError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), mast_harness::HarnessError>>,
{
    if !mast_harness::acp::can_install(harness) {
        return Err(RpcError::Failed("No archive is available for this harness on this device; set ANTIGRAVITY_ACP_EXECUTABLE for Antigravity".into()));
    }
    install()
        .await
        .map_err(|error| RpcError::Failed(error.to_string()))?;
    // A new or upgraded CLI can change which harnesses deliver Work Outline
    // instructions; listeners restamp the device's capability tokens.
    registry.notify_changed();
    Ok(registry.descriptors())
}

fn forward_deadline(method: &str) -> std::time::Duration {
    use std::time::Duration;
    match method {
        methods::CLONE_REPO | methods::FETCH_ALL | methods::APPLY_UPDATE => {
            Duration::from_secs(15 * 60)
        }
        methods::INSTALL_HARNESS => Duration::from_secs(15 * 60),
        methods::CREATE_WORKTREE => Duration::from_secs(120),
        // Allow the adapter discovery budget plus relay and shutdown overhead.
        methods::LIST_MODELS | methods::LIST_COMMANDS => Duration::from_secs(100),
        // Validation plus an optional `git worktree add` on the owner.
        methods::CREATE_CHAT_FOR_WORK_ITEM => Duration::from_secs(120),
        // `gh pr view`, a fetch, and `gh pr checkout` / `git worktree add`.
        methods::START_PR_REVIEW | methods::START_BRANCH_REVIEW => Duration::from_secs(180),
        _ => Duration::from_secs(30),
    }
}

/// ControlRpc methods that honor `targetDeviceId` (feature-inventory §2.1). Extend this
/// list (plus [`is_stream_method`] for streams) to make more of the surface
/// device-addressable — the handlers themselves need no changes.
fn forwardable(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_HARNESSES
            | methods::INSTALL_HARNESS
            | methods::GET_TITLE_SETTINGS
            | methods::SET_TITLE_SETTINGS
            | methods::SET_HARNESS_ENABLED
            | methods::LIST_MODELS
            | methods::LIST_COMMANDS
            | methods::QUEUE_COMMAND
            | methods::RETRY_ERRORED_TURN
            | methods::TAKE_PROJECT_ACTION_SETUP
            | methods::WATCH_DOC_MESSAGES
            // The queue lives on the chat doc, and only its host may send from
            // it — same addressing as the command ledger next door.
            | methods::WATCH_QUEUE
            | methods::QUEUE_MESSAGE
            | methods::UPDATE_QUEUED_MESSAGE
            | methods::BEGIN_QUEUED_MESSAGE_EDIT
            | methods::RENEW_QUEUED_MESSAGE_EDIT
            | methods::FINISH_QUEUED_MESSAGE_EDIT
            | methods::MOVE_QUEUED_MESSAGE
            | methods::REMOVE_QUEUED_MESSAGE
            | methods::SEND_QUEUED_MESSAGE_NOW
            | methods::STEER_QUEUED_MESSAGE_NOW
            // Repos/worktrees/folders are device-local filesystem state.
            | methods::LIST_REPOS
            | methods::ADD_REPO
            | methods::CLONE_REPO
            | methods::CREATE_REPO
            | methods::LIST_BRANCHES
            | methods::LIST_REFS
            | methods::LIST_GIT_HISTORY
            | methods::SEARCH_GIT_HISTORY
            | methods::RESOLVE_GIT_AVATARS
            | methods::FETCH_ALL
            | methods::SWITCH_REF
            | methods::LIST_FOLDERS
            | methods::LIST_DRIVES
            | methods::SEARCH_FILES
            | methods::LIST_WORKSPACE_DIRECTORY
            | methods::SEARCH_WORKSPACE_FILES
            | methods::WATCH_WORKSPACE_SEARCH_FILES
            | methods::SEARCH_WORKSPACE_CONTENT
            | methods::READ_WORKSPACE_IMAGE
            | methods::READ_WORKSPACE_FILE
            | methods::WRITE_WORKSPACE_FILE
            | methods::WATCH_WORKSPACE_FILES
            | methods::CREATE_WORKTREE
            | methods::DELETE_WORKTREE
            // Project Actions live in the owning engine's private profile store.
            | methods::LIST_PROJECT_ACTIONS
            | methods::UPSERT_PROJECT_ACTION
            | methods::DELETE_PROJECT_ACTION
            | methods::RUN_PROJECT_ACTION
            // Checkout diffs are produced on the device holding the checkout.
            | methods::WATCH_CHECKOUT_DIFFS
            | methods::WATCH_WORKSPACE_GIT_STATUS
            | methods::WATCH_CHECKOUT_CHANGE_REQUEST
            | methods::GET_CHECKOUT_DIFF
            | methods::RESOLVE_CODE_REF_SOURCE
            | methods::RESOLVE_BRANCH_REVIEW_BASE
            | methods::GET_CHECKOUT_FILE_DIFF_TEXT
            // Terminals live on the chat's host device.
            | methods::OPEN_TERMINAL
            | methods::SUBSCRIBE_TERMINAL
            | methods::WRITE_TERMINAL
            | methods::RESIZE_TERMINAL
            | methods::CLOSE_TERMINAL
            // Agent accounts are per-device CLI logins (the device switcher
            // retargets which device's logins are shown).
            | methods::LIST_AGENT_ACCOUNTS
            | methods::LOCAL_AGENT_SIGN_IN_STATUS
            | methods::ACTIVATE_AGENT_ACCOUNT
            | methods::FORGET_AGENT_ACCOUNT
            | methods::START_AGENT_LOGIN
            | methods::COMPLETE_AGENT_LOGIN
            | methods::POLL_AGENT_LOGIN
            | methods::CANCEL_AGENT_LOGIN
            | methods::LIST_MANAGED_ACCOUNTS
            | methods::CREATE_MANAGED_ACCOUNT
            | methods::REMOVE_MANAGED_ACCOUNT
            | methods::START_MANAGED_ACCOUNT_LOGIN
            | methods::POLL_MANAGED_ACCOUNT_LOGIN
            | methods::CANCEL_MANAGED_ACCOUNT_LOGIN
            | methods::SUBMIT_MANAGED_ACCOUNT_LOGIN_CODE
            | methods::PROMOTE_SAVED_ACCOUNT
            | methods::SET_PROJECT_ACCOUNT
            // Uploads/attachments target the chat's host device (the agent reads
            // the committed file from that device's disk).
            | methods::UPLOAD_CHUNK
            | methods::UPLOAD_COMMIT
            | methods::READ_ATTACHMENT_CHUNK
            // Updates report/apply on the device whose binary they concern.
            | methods::UPDATE_STATUS
            | methods::APPLY_UPDATE
            // Execution configurations validate against the owner's disk.
            | methods::VALIDATE_EXECUTION_CONFIGURATION
            | methods::DESCRIBE_EXECUTION_REPOSITORY
            // A work-item launch validates and creates on the owner's disk.
            | methods::CREATE_CHAT_FOR_WORK_ITEM
            // A PR review start checks out and creates on the Space's owner.
            | methods::START_PR_REVIEW
            | methods::START_BRANCH_REVIEW
            // Planning artifacts: content lives on the owning device.
            | methods::ARTIFACT_CONTENT
            | methods::ARTIFACT_REGISTER
            | methods::ARTIFACT_PROMOTE
            | methods::ARTIFACT_DRIFT
    )
}

/// Forwardable methods whose reply is a stream (proxied item-by-item).
fn is_stream_method(method: &str) -> bool {
    matches!(
        method,
        methods::WATCH_DOC_MESSAGES
            | methods::WATCH_QUEUE
            | methods::SUBSCRIBE_TERMINAL
            | methods::WATCH_CHECKOUT_DIFFS
            | methods::WATCH_WORKSPACE_GIT_STATUS
            | methods::WATCH_CHECKOUT_CHANGE_REQUEST
            | methods::WATCH_WORKSPACE_FILES
            | methods::WATCH_WORKSPACE_SEARCH_FILES
            | methods::SEARCH_WORKSPACE_CONTENT
            | methods::UPDATE_STATUS
    )
}

/// A watch receiver as a stream: current value first, then every change.
fn watch_stream<T>(rx: watch::Receiver<T>) -> BoxStream<'static, serde_json::Value>
where
    T: serde::Serialize + Clone + Send + Sync + 'static,
{
    futures::stream::unfold((rx, false), |(mut rx, emitted)| async move {
        if emitted {
            rx.changed().await.ok()?;
        }
        let value = {
            let borrowed = rx.borrow_and_update();
            serde_json::to_value(&*borrowed).ok()?
        };
        Some((value, (rx, true)))
    })
    .boxed()
}

/// Filter the shared latest-only cache down to one checkout while retaining
/// the historical array-shaped wire payload. The device check is deliberate:
/// checkout identities are device-derived, and no device-agnostic cwd/id
/// fallback should leak a similarly named remote checkout into this stream.
fn scoped_checkout_diff_stream(
    rx: watch::Receiver<Vec<mast_proto::CheckoutDiff>>,
    checkout_id: String,
    device_id: String,
) -> BoxStream<'static, serde_json::Value> {
    futures::stream::unfold(
        (rx, checkout_id, device_id, None, false),
        |(mut rx, checkout_id, device_id, mut previous, emitted): (
            _,
            _,
            _,
            Option<Vec<mast_proto::CheckoutDiff>>,
            _,
        )| async move {
            loop {
                if emitted {
                    rx.changed().await.ok()?;
                }
                let next: Vec<_> = rx
                    .borrow_and_update()
                    .iter()
                    .filter(|diff| diff.checkout_id == checkout_id && diff.device_id == device_id)
                    .cloned()
                    .collect();
                if !emitted || previous.as_ref() != Some(&next) {
                    let value = serde_json::to_value(&next).ok()?;
                    previous = Some(next);
                    return Some((value, (rx, checkout_id, device_id, previous, true)));
                }
            }
        },
    )
    .boxed()
}

/// The transcript watch as delta frames (`mast_doc::transcript_delta`): a
/// full `reset` first, then only changed entries per commit — the whole-Vec
/// serialization here was the per-tick cost that scaled with transcript size.
fn doc_messages_stream(
    rx: watch::Receiver<crate::doc_host::TranscriptSnapshot>,
    doc: std::sync::Arc<mast_doc::SessionDoc>,
) -> BoxStream<'static, serde_json::Value> {
    use mast_doc::transcript_delta::{TranscriptFrame, diff_transcript};
    futures::stream::unfold(
        (
            rx,
            None::<crate::doc_host::TranscriptSnapshot>,
            doc,
            None,
            mast_doc::TranscriptBaseline::default(),
        ),
        |(mut rx, mut prev, doc, mut previous_usage, mut opening_baseline)| async move {
            loop {
                if prev.is_some() {
                    rx.changed().await.ok()?;
                }
                // Watchers retain the immutable published snapshot. Each
                // connection used to deep-copy the entire transcript here.
                let current = rx.borrow_and_update().clone();
                let frame = match prev.as_ref() {
                    None => TranscriptFrame::reset(&current.entries),
                    Some(prev) => diff_transcript(&prev.entries, &current.entries),
                };
                let replay_baseline = match prev.as_ref() {
                    None => {
                        opening_baseline = mast_doc::TranscriptBaseline::capture(&current.entries);
                        Some(opening_baseline.clone())
                    }
                    Some(prev)
                        if !std::sync::Arc::ptr_eq(
                            &prev.replay_baseline,
                            &current.replay_baseline,
                        ) =>
                    {
                        // The tracker only observes changes after attach; its
                        // baseline omits unchanged cached parts. Preserve this
                        // subscription's opening cutoff without capturing live
                        // appends or sharing another viewer's later cutoff.
                        // Ordinary live updates never rebuild this metadata.
                        let mut baseline = (*current.replay_baseline).clone();
                        for (entry, parts) in &opening_baseline.entries {
                            let merged = baseline.entries.entry(entry.clone()).or_default();
                            for (part, &len) in parts {
                                let cutoff = merged.entry(part.clone()).or_default();
                                *cutoff = (*cutoff).max(len);
                            }
                        }
                        Some(baseline)
                    }
                    _ => None,
                };
                prev = Some(current);
                // No-op commits (a second watcher attaching, command-only
                // changes) produce empty deltas — skip the frame entirely.
                let usage = doc.context_usage();
                if frame.is_empty_delta() && usage == previous_usage && replay_baseline.is_none() {
                    continue;
                }
                previous_usage = usage;
                let value = serde_json::to_value(mast_doc::TranscriptUpdate {
                    frame,
                    context_usage: usage,
                    replay_baseline,
                })
                .ok()?;
                return Some((value, (rx, prev, doc, previous_usage, opening_baseline)));
            }
        },
    )
    .boxed()
}

/// First paint reads only the local tail; full history is deferred until the
/// next stream poll. No network dependency or persisted truncation.
async fn opening_doc_messages_stream(
    host: crate::doc_host::DocHost,
    chat_id: String,
) -> Result<BoxStream<'static, serde_json::Value>, RpcError> {
    let (handle, preview) = tokio::task::spawn_blocking(move || {
        let handle = host.open(&chat_id)?;
        let entries = handle.doc().read_opening_tail(128)?;
        let mut preview = serde_json::to_value(mast_doc::TranscriptUpdate {
            frame: mast_doc::TranscriptFrame::reset(&entries),
            context_usage: handle.doc().context_usage(),
            replay_baseline: Some(mast_doc::TranscriptBaseline::capture(&entries)),
        })
        .map_err(|e| crate::EngineError::Other(e.to_string()))?;
        preview["historyPending"] = serde_json::Value::Bool(true);
        Ok::<_, crate::EngineError>((handle, preview))
    })
    .await
    .map_err(|e| RpcError::Failed(e.to_string()))?
    .map_err(|e| RpcError::Failed(e.to_string()))?;
    // Start the authoritative attachment now, independently of the consumer
    // polling the second stream item.  Deferring the spawn itself to the next
    // poll left a restored viewport with only its provisional frame when the
    // RPC/GPUI pumps went quiet after first paint; the next user mutation woke
    // the pipeline and made the old transcript appear, which looked as if the
    // send had recovered it.  The blocking work still runs off-thread and we
    // still yield `preview` first, but history hydration no longer depends on
    // another UI event.
    let full_attach =
        tokio::task::spawn_blocking(move || (handle.watch_messages(), handle.doc_arc()));
    let full = futures::stream::once(async move {
        match full_attach.await {
            Ok((rx, doc)) => doc_messages_stream(rx, doc),
            Err(error) => {
                tracing::warn!(%error, "transcript opening failed");
                futures::stream::empty().boxed()
            }
        }
    })
    .flatten();
    Ok(futures::stream::once(async move { preview })
        .chain(full)
        .boxed())
}

/// Authentication-only RPC surface used while the headed app is waiting for a
/// production WorkOS session. Keeping this independent from [`EngineRpc`] lets
/// the UI show its sign-in and organization gates before identity-scoped Loro
/// stores are opened.
#[derive(Clone)]
pub struct AuthRpc {
    auth: Auth,
}

impl AuthRpc {
    pub fn new(auth: Auth) -> Self {
        Self { auth }
    }

    pub fn handles(method: &str) -> bool {
        matches!(
            method,
            methods::AUTH_STATUS
                | methods::SIGN_IN
                | methods::SIGN_IN_HEADLESS
                | methods::COMPLETE_SIGN_IN
                | methods::SIGN_OUT
                | methods::LIST_ORGS
                | methods::CREATE_ORG
                | methods::SELECT_ORG
        )
    }
}

#[async_trait]
impl RpcService for AuthRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            methods::AUTH_STATUS => Ok(RpcReply::Stream(watch_stream(self.auth.watch_state()))),
            methods::SIGN_IN => {
                let url = self
                    .auth
                    .start_sign_in()
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "url": url }))
            }
            methods::SIGN_IN_HEADLESS => {
                let url = self.auth.start_headless_sign_in();
                RpcReply::value(&serde_json::json!({ "url": url }))
            }
            methods::COMPLETE_SIGN_IN => {
                #[derive(Deserialize)]
                struct P {
                    code: String,
                }
                let p: P = parse_params(params)?;
                self.auth
                    .complete_sign_in(&p.code)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::SIGN_OUT => {
                self.auth.sign_out();
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::LIST_ORGS => {
                let orgs = self
                    .auth
                    .list_orgs()
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "orgs": orgs }))
            }
            methods::CREATE_ORG => {
                #[derive(Deserialize)]
                struct P {
                    name: String,
                }
                let p: P = parse_params(params)?;
                self.auth
                    .create_org(&p.name)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::SELECT_ORG => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    organization_id: String,
                }
                let p: P = parse_params(params)?;
                self.auth
                    .select_org(&p.organization_id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            _ => Err(RpcError::UnknownMethod(method.to_string())),
        }
    }
}

#[async_trait]
impl RpcService for EngineRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        // Device-addressed routing: forward calls that target another device over its
        // relay. The target compares the id to its own, so forwards cannot loop.
        if forwardable(method)
            && let Some(target) = params.get("targetDeviceId").and_then(|v| v.as_str())
            && target != self.doc_host.device_id()
        {
            let target = target.to_string();
            return self.forward(&target, method, params).await;
        }
        if AuthRpc::handles(method) {
            return AuthRpc::new(self.auth()?.clone())
                .handle(method, params)
                .await;
        }
        match method {
            methods::ENGINE_INFO => {
                // Capabilities are dynamic (per-harness instruction tokens
                // follow the probes): serve what this device's registry row
                // currently advertises.
                let mut info = self.engine_info.clone();
                if let Some(device) = self.workspace.read_devices().ok().and_then(|devices| {
                    devices
                        .into_iter()
                        .find(|device| device.id == self.engine_info.device_id)
                }) && !device.capabilities.is_empty()
                {
                    info.capabilities = device.capabilities;
                }
                RpcReply::value(&info)
            }
            methods::ENGINE_READY => RpcReply::value(&serde_json::json!({ "ready": true })),
            methods::LIST_HARNESSES => RpcReply::value(&self.registry.descriptors()),
            methods::INSTALL_HARNESS => {
                let p: ListModelsParams = parse_params(params)?;
                let descriptors = install_harness_with(&self.registry, p.harness, || {
                    mast_harness::acp::install_harness(p.harness)
                })
                .await?;
                RpcReply::value(&descriptors)
            }
            methods::GET_TITLE_SETTINGS => RpcReply::value(&self.registry.title_settings()),
            methods::SET_TITLE_SETTINGS => {
                let p: crate::registry::TitleSettings = parse_params(params)?;
                self.registry
                    .set_title_settings(p)
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&self.registry.title_settings())
            }
            methods::SET_HARNESS_ENABLED => {
                let p: SetHarnessEnabledParams = parse_params(params)?;
                update_harness_enabled(&self.registry, p.harness, p.enabled).await?;
                // Fresh catalog in the reply: the page repaints from it in one
                // round trip, and a refused/raced toggle self-corrects.
                RpcReply::value(&self.registry.descriptors())
            }
            methods::LIST_MODELS => {
                let p: ListModelsParams = parse_params(params)?;
                let harness = self
                    .registry
                    .resolve(p.harness)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let models = crate::model_catalogs::list(self.repos.data_dir(), harness, p.force)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&models)
            }
            methods::LIST_COMMANDS => {
                // Same shape as ListModels: forces a lazy resolve, then the
                // harness's own (cached) discovery — ACP agents advertise
                // availableCommands, claude answers the initialize control
                // request, codex lists skills; only harnesses whose wire has
                // no listing (cursor, mock) fall through to the trait's
                // empty default. `cwd` carries the calling chat's directory
                // through to discovery (Claude's project-scoped
                // commands/skills depend on it); other harnesses ignore it.
                let p: ListModelsParams = parse_params(params)?;
                let harness = self
                    .registry
                    .resolve(p.harness)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let cwd = self
                    .sessions
                    .resolve_cwd(p.chat_id.as_deref().unwrap_or("command-discovery"), &p.cwd)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let commands = harness
                    .commands(&cwd)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&commands)
            }
            methods::QUEUE_COMMAND => {
                let p: QueueCommandParams = parse_params(params)?;
                let command_id = match p.command_id {
                    Some(id) => self.doc_host.queue_command_with_transfers_and_id(&p.chat_id, &id, p.command, p.transfers),
                    None => self.doc_host.queue_command_with_transfers(&p.chat_id, p.command, p.transfers),
                }
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "commandId": command_id }))
            }
            methods::TAKE_PROJECT_ACTION_SETUP => {
                let p: TakeProjectActionSetupParams = parse_params(params)?;
                let outcome = self
                    .project_actions
                    .take_setup_handoff(&p.command_id, &p.chat_id);
                match outcome {
                    Some(outcome) => RpcReply::value(&serde_json::json!({
                        "ready": true,
                        "setupAction": outcome.setup_action,
                        "setupError": outcome.setup_error,
                    })),
                    None => RpcReply::value(&serde_json::json!({ "ready": false })),
                }
            }
            methods::GET_WAKE_STATUS => {
                let p: ChatParams = parse_params(params)?;
                RpcReply::value(&self.orchestrator_wakes.status(&p.chat_id))
            }
            methods::SET_WAKE_SETTINGS => {
                let p: mast_proto::SetWakeSettingsParams = parse_params(params)?;
                RpcReply::value(
                    &self
                        .orchestrator_wakes
                        .set_settings(&p.chat_id, p.settings)
                        .await,
                )
            }
            methods::WAKE_DELIVER_NOW => {
                let p: ChatParams = parse_params(params)?;
                RpcReply::value(&self.orchestrator_wakes.deliver_now(&p.chat_id).await)
            }
            methods::REPORT_TO_PARENT => {
                let p: mast_proto::ReportToParentParams = parse_params(params)?;
                match self
                    .orchestrator_wakes
                    .report_to_parent(&p.chat_id, &p.text)
                    .await
                {
                    Ok(result) => RpcReply::value(&result),
                    Err(message) => return Err(RpcError::Failed(message)),
                }
            }
            methods::NOTE_CHAT_USER_ACTIVITY => {
                let p: mast_proto::NoteChatUserActivityParams = parse_params(params)?;
                self.orchestrator_wakes
                    .note_user_activity(&p.chat_id, p.draft_non_empty)
                    .await;
                RpcReply::value(&serde_json::json!({}))
            }
            methods::GRANT_ADMIN => {
                // User action only: no agent tool reaches this method.
                let p: mast_proto::GrantAdminParams = parse_params(params)?;
                let chat = self
                    .workspace
                    .chat(&p.chat_id)
                    .ok()
                    .flatten()
                    .ok_or_else(|| RpcError::Failed("chat not found".into()))?;
                let space = chat
                    .space_id
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| RpcError::Failed("only chats in a Space can be admin".into()))?;
                RpcReply::value(&self.agent_tools.tokens().admin().grant(
                    &p.chat_id,
                    &space,
                    p.duration_secs,
                    p.skip_bulk_confirmation,
                ))
            }
            methods::REVOKE_ADMIN => {
                let p: ChatParams = parse_params(params)?;
                self.agent_tools.tokens().admin().revoke(&p.chat_id);
                RpcReply::value(&serde_json::json!({}))
            }
            methods::LIST_ADMIN_GRANTS => {
                RpcReply::value(&self.agent_tools.tokens().admin().list())
            }
            methods::ADMIN_AUDIT_LIST => {
                let p: mast_proto::AdminAuditListParams = parse_params(params)?;
                RpcReply::value(
                    &self
                        .agent_tools
                        .tokens()
                        .admin()
                        .turns(p.space_id.as_deref()),
                )
            }
            methods::ADMIN_UNDO_TURN => {
                let p: mast_proto::AdminUndoTurnParams = parse_params(params)?;
                let service = self.agent_tools.clone();
                let reply =
                    tokio::task::spawn_blocking(move || service.admin_undo_turn(&p.turn_id))
                        .await
                        .map_err(|e| RpcError::Failed(e.to_string()))?
                        .map_err(|e| RpcError::Failed(e.to_wire_message()))?;
                RpcReply::value(&reply)
            }
            methods::LIST_INTERRUPTED_RUNS => RpcReply::value(&self.sessions.interrupted_runs()),
            methods::DISMISS_INTERRUPTED_RUN => {
                let p: ChatParams = parse_params(params)?;
                self.sessions.dismiss_interrupted(&p.chat_id);
                RpcReply::value(&serde_json::json!({}))
            }
            methods::RESUME_INTERRUPTED_RUN => {
                let p: ChatParams = parse_params(params)?;
                let resumed = self.sessions.resume_interrupted(&p.chat_id);
                RpcReply::value(&serde_json::json!({ "resumed": resumed }))
            }
            methods::RETRY_ERRORED_TURN => {
                let p: RetryErroredTurnParams = parse_params(params)?;
                let retried = self.sessions.retry_errored_turn(&p.chat_id, &p.error_entry_id)
                    .await.map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "retried": retried }))
            }
            methods::RETRY_DELIVERY => {
                let p: ChatParams = parse_params(params)?;
                self.doc_host
                    .retry_delivery(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({}))
            }
            methods::RELAY_COMMAND => {
                let p: RelayCommandParams = parse_params(params)?;
                let outcome = self
                    .doc_host
                    .ingest_relayed_command(&p.chat_id, p.entry)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "outcome": outcome }))
            }
            methods::WATCH_DOC_MESSAGES => {
                // Opt-in: older viewports retain the full-reset contract.
                let opening_tail = params
                    .get("openingTail")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let p: ChatParams = parse_params(params)?;
                if opening_tail {
                    return Ok(RpcReply::Stream(
                        opening_doc_messages_stream(self.doc_host.clone(), p.chat_id).await?,
                    ));
                }
                let handle = self
                    .doc_host
                    .open(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                Ok(RpcReply::Stream(doc_messages_stream(
                    handle.watch_messages(),
                    handle.doc_arc(),
                )))
            }
            methods::WATCH_QUEUE => {
                let p: ChatParams = parse_params(params)?;
                let handle = self
                    .doc_host
                    .open(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let rx = handle.watch_queue();
                let rejected = handle.watch_rejected_sends();
                // Frame: `{items: QueuedMessage[], rejectedSends:
                // InstructionRejectedSend[]}` — sends the host refused with a
                // typed instruction-delivery outcome ride beside the queue
                // (additive; older viewports ignore the key).
                Ok(RpcReply::Stream(
                    futures::stream::unfold(
                        (rx, rejected, true),
                        |(mut rx, mut rejected, first)| async move {
                            if !first {
                                tokio::select! {
                                    changed = rx.changed() => changed.ok()?,
                                    changed = rejected.changed() => changed.ok()?,
                                }
                            }
                            let items = rx.borrow_and_update().clone();
                            let rejected_sends = rejected.borrow_and_update().clone();
                            let value = serde_json::json!({
                                "items": items,
                                "rejectedSends": rejected_sends,
                            });
                            Some((value, (rx, rejected, false)))
                        },
                    )
                    .boxed(),
                ))
            }
            methods::QUEUE_MESSAGE => {
                let p: QueueMessageParams = parse_params(params)?;
                let id = match p.message_id {
                    Some(id) => self.doc_host.queue_message_with_id(&p.chat_id, &id, &p.text, p.attachments, p.hold_for_turn_end).await,
                    None => self.doc_host.queue_message_with_behavior(&p.chat_id, &p.text, p.attachments, p.hold_for_turn_end),
                }
                    .map_err(|e| {
                        tracing::warn!(chat = %p.chat_id, error = %e, "queue message failed");
                        RpcError::Failed(e.to_string())
                    })?;
                RpcReply::value(&serde_json::json!({ "id": id }))
            }
            methods::UPDATE_QUEUED_MESSAGE => {
                let p: QueuedMessageParams = parse_params(params)?;
                let changed = self
                    .doc_host
                    .update_queued_message(&p.chat_id, &p.id, &p.text)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "changed": changed }))
            }
            methods::BEGIN_QUEUED_MESSAGE_EDIT => {
                let p: BeginQueuedMessageEditParams = parse_params(params)?;
                let outcome = self
                    .doc_host
                    .begin_queued_message_edit(
                        &p.chat_id,
                        &p.id,
                        &p.editor_device_id,
                        &p.editor_instance_id,
                    )
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(
                    &serde_json::to_value(outcome).map_err(|e| RpcError::Failed(e.to_string()))?,
                )
            }
            methods::RENEW_QUEUED_MESSAGE_EDIT => {
                let p: RenewQueuedMessageEditParams = parse_params(params)?;
                let outcome = self
                    .doc_host
                    .renew_queued_message_edit(&p.chat_id, &p.id, &p.lease_id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(
                    &serde_json::to_value(outcome).map_err(|e| RpcError::Failed(e.to_string()))?,
                )
            }
            methods::FINISH_QUEUED_MESSAGE_EDIT => {
                let p: FinishQueuedMessageEditParams = parse_params(params)?;
                let action = match p.action {
                    FinishQueuedMessageEditAction::Commit => {
                        crate::doc_host::FinishQueueEditAction::Commit
                    }
                    FinishQueuedMessageEditAction::Cancel => {
                        crate::doc_host::FinishQueueEditAction::Cancel
                    }
                    FinishQueuedMessageEditAction::Discard => {
                        crate::doc_host::FinishQueueEditAction::Discard
                    }
                    FinishQueuedMessageEditAction::ReleaseUnchanged => {
                        crate::doc_host::FinishQueueEditAction::ReleaseUnchanged
                    }
                };
                let outcome = self
                    .doc_host
                    .finish_queued_message_edit_with_attachments(
                        &p.chat_id,
                        &p.id,
                        &p.lease_id,
                        action,
                        p.text.as_deref(),
                        p.expected_text_hash.as_deref(),
                        p.attachments.as_deref(),
                    )
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(
                    &serde_json::to_value(outcome).map_err(|e| RpcError::Failed(e.to_string()))?,
                )
            }
            methods::MOVE_QUEUED_MESSAGE => {
                let p: QueuedMessageParams = parse_params(params)?;
                let changed = self
                    .doc_host
                    .move_queued_message(&p.chat_id, &p.id, p.to_index)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "changed": changed }))
            }
            methods::REMOVE_QUEUED_MESSAGE => {
                let p: QueuedMessageParams = parse_params(params)?;
                let removed = self
                    .doc_host
                    .remove_queued_message(&p.chat_id, &p.id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "removed": removed }))
            }
            methods::SEND_QUEUED_MESSAGE_NOW => {
                let p: QueuedMessageParams = parse_params(params)?;
                let sent = self
                    .doc_host
                    .send_queued_now(&p.chat_id, &p.id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "sent": sent }))
            }
            methods::STEER_QUEUED_MESSAGE_NOW => {
                let p: QueuedMessageParams = parse_params(params)?;
                let sent = self
                    .doc_host
                    .steer_queued_now(&p.chat_id, &p.id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "sent": sent }))
            }
            methods::PROBE_SYNC => {
                // Focus probes remain cheap. An explicit Retry may also allow
                // one fresh, shared auth attempt before its cooldown expires.
                if params.get("retry").and_then(serde_json::Value::as_bool) == Some(true)
                    && let Some(auth) = &self.auth
                {
                    auth.retry_refresh();
                }
                self.workspace.probe();
                self.doc_host.probe_open_chats();
                self.doc_host.probe_edge_reachability();
                RpcReply::value(&serde_json::json!({}))
            }
            methods::SYNC_STATUS => {
                fn room_json(s: &mast_sync::RoomStatsSnapshot) -> serde_json::Value {
                    serde_json::json!({
                        "connected": s.connected,
                        "synced": s.synced,
                        "lastPushedMs": s.last_pushed_ms,
                        "lastAckMs": s.last_ack_ms,
                        "rejoins": s.rejoins,
                        "probes": s.probes,
                        "fullResyncs": s.full_resyncs,
                        "disconnects": s.disconnects,
                        "rejected": s.rejected,
                    })
                }
                fn chat2_json(s: &mast_sync::ChatStatsSnapshot) -> serde_json::Value {
                    serde_json::json!({
                        "connected": s.connected,
                        "cursor": s.cursor,
                        "headSeq": s.head_seq,
                        "seqFloor": s.seq_floor,
                        "checkpointSeq": s.checkpoint_seq,
                        "checkpointSize": s.checkpoint_size,
                        "rowCount": s.row_count,
                        "rowBytes": s.row_bytes,
                        "pendingPushes": s.pending_pushes,
                        "rejoins": s.rejoins,
                        "disconnects": s.disconnects,
                        "rejected": s.rejected,
                        "serverResets": s.server_resets,
                    })
                }
                let workspace = self.workspace.sync_status();
                let chats: Vec<serde_json::Value> = self
                    .doc_host
                    .sync_statuses()
                    .iter()
                    .map(|(chat_id, room)| {
                        serde_json::json!({
                            "chatId": chat_id,
                            "room": room.as_ref().map(chat2_json),
                        })
                    })
                    .collect();
                RpcReply::value(&serde_json::json!({
                    "deviceId": self.doc_host.device_id(),
                    "nowMs": crate::now_ms(),
                    "workspace": workspace.as_ref().map(room_json),
                    "chats": chats,
                }))
            }
            methods::WATCH_CONNECTIVITY => Ok(RpcReply::Stream(watch_stream(
                self.doc_host.watch_connectivity(),
            ))),
            methods::WATCH_TRANSFERS => Ok(RpcReply::Stream(watch_stream(
                self.doc_host.watch_transfers(),
            ))),
            methods::WATCH_PREVIEWS => {
                let p: mast_proto::WatchPreviewsParams = parse_params(params)?;
                if self
                    .workspace
                    .chat(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .is_none()
                {
                    return Err(RpcError::Failed("Project session not found".into()));
                }
                let previews = self
                    .previews
                    .as_ref()
                    .ok_or_else(|| RpcError::Failed("Preview discovery unavailable".into()))?;
                let catalog = previews.catalog().clone();
                let changes = catalog.subscribe();
                let chats = self.workspace.watch_chats();
                let workspace = self.workspace.clone();
                // This subscription stays on the viewing device. A remote chat
                // selects advertised services, but its URL uses our local proxy.
                let stream = futures::stream::unfold(
                    (changes, chats, true, workspace, catalog, p.chat_id),
                    |(mut changes, mut chats, first, workspace, catalog, chat_id)| async move {
                        if !first {
                            tokio::select! {
                                result = changes.changed() => { if result.is_err() { return None; } }
                                result = chats.changed() => { if result.is_err() { return None; } }
                            }
                        }
                        let mut snapshot = changes.borrow_and_update().clone();
                        chats.borrow_and_update();
                        let chat = workspace.chat(&chat_id).ok().flatten();
                        let device = chat
                            .as_ref()
                            .map(|c| c.device_id.clone())
                            .unwrap_or_default();
                        snapshot.remote = device != catalog.device_id();
                        let cwd = chat.and_then(|c| c.cwd);
                        let cwd = cwd.map(|cwd| {
                            if snapshot.remote {
                                std::path::PathBuf::from(cwd)
                            } else {
                                std::path::PathBuf::from(&cwd)
                                    .canonicalize()
                                    .unwrap_or_else(|_| cwd.into())
                            }
                        });
                        snapshot.project_name = cwd
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .map(|s| s.to_string_lossy().into_owned());
                        snapshot.services.retain(|service| {
                            service.device_id == device
                                && cwd.as_ref().is_some_and(|cwd| {
                                    cwd == std::path::Path::new(&service.project_cwd)
                                })
                        });
                        let value = serde_json::to_value(snapshot).ok()?;
                        Some((value, (changes, chats, false, workspace, catalog, chat_id)))
                    },
                );
                Ok(RpcReply::Stream(Box::pin(stream)))
            }
            methods::WATCH_CHATS => {
                Ok(RpcReply::Stream(watch_stream(self.workspace.watch_chats())))
            }
            methods::GET_FRICTION_SETTINGS => RpcReply::value(&self.registry.friction_settings()),
            methods::SET_FRICTION_SETTINGS => {
                let settings: mast_proto::FrictionSettings = parse_params(params)?;
                self.registry.set_friction_settings(settings.clone()).map_err(RpcError::Failed)?;
                self.sessions.set_auto_capture_friction(settings.auto_capture);
                RpcReply::value(&settings)
            }
            methods::LIST_FRICTION_REPORTS => {
                let p: mast_proto::ListFrictionReportsParams = parse_params(params)?;
                let mut rows = self.workspace.friction_reports(&p.space_id);
                rows.retain(|r| p.kind.is_none_or(|v| r.kind == v)
                    && r.status == p.status.unwrap_or(mast_proto::FrictionStatus::Open)
                    && p.harness.as_ref().is_none_or(|v| &r.provenance.harness == v)
                    && p.model.as_ref().is_none_or(|v| r.provenance.model.as_ref() == Some(v))
                    && p.since.is_none_or(|v| r.last_seen >= v));
                rows.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then(a.id.cmp(&b.id)));
                rows.truncate(p.limit.unwrap_or(100).min(1000));
                RpcReply::value(&rows)
            }
            methods::IMPORT_FRICTION => {
                let p: mast_proto::ImportFrictionParams = parse_params(params)?;
                let filename = std::path::Path::new(&p.file_name).file_name().and_then(|s| s.to_str())
                    .ok_or_else(|| RpcError::BadParams("invalid file name".into()))?;
                let filename = mast_harness::redact::redact_secrets(filename);
                let parsed = crate::friction_import::parse_friction_markdown(&p.markdown);
                let mut result = mast_proto::ImportFrictionResult {
                    added: 0, merged: 0,
                    skipped: parsed.skipped.into_iter().map(|s| mast_proto::FrictionImportSkip { line: s.line, reason: s.reason }).collect(),
                    entries: Vec::new(),
                };
                let mut identities = std::collections::HashMap::<String, usize>::new();
                for entry in parsed.reports {
                    result.entries.push(mast_proto::FrictionImportEntry { kind: entry.kind, summary: entry.summary.clone() });
                    if p.preview { continue; }
                    let identity = mast_proto::derive_id(&format!("{entry:?}"), "friction-import-entry");
                    let ordinal = identities.entry(identity.clone()).or_default();
                    *ordinal += 1;
                    let import_id = mast_proto::derive_id(&format!("{filename}:{identity}:{ordinal}"), "friction-import-occurrence");
                    let mut report = mast_proto::FrictionReport {
                        id: String::new(), kind: entry.kind, summary: entry.summary,
                        what_happened: entry.what_happened, workaround: entry.workaround,
                        suggestion: entry.suggestion, approval: entry.approval, severity: entry.severity,
                        links: entry.links, provenance: mast_proto::FrictionProvenance {
                            chat_id: String::new(), harness: String::new(), model: None,
                            space_id: p.space_id.clone(), work_item_id: None, branch: None,
                            created_at: entry.created_at, source: mast_proto::FrictionSource::Import,
                            file_name: Some(filename.clone()),
                        }, fingerprint: String::new(), occurrences: 0, occurrence_history: Vec::new(), last_seen: entry.created_at,
                        status: mast_proto::FrictionStatus::Open, promoted_item_id: None, promoted_at: None,
                    };
                    let fingerprint = mast_doc::registry::friction::friction_fingerprint(&report);
                    let mut existing = self.workspace.friction_reports(&p.space_id);
                    existing.sort_by(|a, b| a.id.cmp(&b.id));
                    let normalized = |summary: &str| summary.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
                    let matching = existing.iter().find(|r| r.fingerprint == fingerprint).or_else(|| {
                        existing.iter().find(|r| r.kind == report.kind && normalized(&r.summary) == normalized(&report.summary))
                    });
                    if let Some(existing) = matching {
                        // The historical fingerprint also includes the approval command. Use the
                        // existing command so same-kind summaries merge as the import contract requires.
                        report.approval = existing.approval.clone();
                    }
                    let exists = matching.is_some();
                    match self.workspace.import_friction(report, &import_id) {
                        Ok((_, false)) => result.skipped.push(mast_proto::FrictionImportSkip { line: 0, reason: "already imported".into() }),
                        Ok((_, true)) if exists => result.merged += 1,
                        Ok((_, true)) => result.added += 1,
                        Err(error) => result.skipped.push(mast_proto::FrictionImportSkip { line: 0, reason: error.to_string() }),
                    }
                }
                if !p.preview { self.workspace.flush(); }
                RpcReply::value(&result)
            }
            methods::SET_FRICTION_STATUS => {
                let p: mast_proto::SetFrictionStatusParams = parse_params(params)?;
                if p.status == mast_proto::FrictionStatus::Promoted { return Err(RpcError::BadParams("Use PromoteFrictionToItem to promote a report".into())); }
                let row = self.workspace.set_friction_status(&p.space_id, &p.fingerprint, p.status, None, None).map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&row)
            }
            methods::PROMOTE_FRICTION_TO_ITEM => {
                let p: mast_proto::PromoteFrictionToItemParams = parse_params(params)?;
                let row = self.workspace.friction_reports(&p.space_id).into_iter().find(|r| r.fingerprint == p.fingerprint).ok_or_else(|| RpcError::Failed("friction report not found".into()))?;
                if let Some(id) = row.promoted_item_id { return RpcReply::value(&serde_json::json!({"workItemId": id})); }
                use sha2::{Digest, Sha256};
                let scope = self.engine_info.workspace_scope;
                let identity = match scope {
                    mast_proto::WorkspaceScope::Local => Some(format!("device:{}", self.engine_info.device_id)),
                    _ => self.auth.as_ref().and_then(|auth| match auth.state() {
                        crate::auth::AuthState::SignedIn { user, org_id } => Some(format!("user:{}:org:{}", user.id, org_id.as_deref().unwrap_or("personal"))),
                        _ => None,
                    }),
                };
                let chat = identity.map(|identity| {
                    let hash = Sha256::digest(format!("{scope:?}\0{identity}"));
                    format!("[{}](mast://open/chat/{}?workspace={})", row.provenance.chat_id, row.provenance.chat_id, &format!("{:x}", hash)[..16])
                }).unwrap_or_else(|| row.provenance.chat_id.clone());
                let description = format!("## What happened\n\n{}\n\n## Workaround\n\n{}\n\n## Suggestion\n\n{}\n\n## Approval\n\n{}\n\n## Provenance\n\nChat: {}\nHarness: {}\nModel: {}\nBranch: {}\n", row.what_happened, row.workaround.as_deref().unwrap_or("None"), row.suggestion.as_deref().unwrap_or("None"), row.approval.as_ref().map(|a| format!("Command: `{}`\n\nReason: {}\n\nAnswer: {:?}", a.command, a.why, a.answer)).unwrap_or_else(|| "None".into()), chat, row.provenance.harness, row.provenance.model.as_deref().unwrap_or("Unknown"), row.provenance.branch.as_deref().unwrap_or("Unknown"));
                let op = mast_proto::derive_id(&format!("{}:{}", row.id, p.parent_item_id), "friction-promotion");
                let mutation = mast_proto::WorkOutlineMutation::CreateWorkItem(mast_proto::CreateWorkItemParams { space_id: p.space_id.clone(), parent_work_item_id: Some(p.parent_item_id), placement: Default::default(), title: row.summary, description: Some(description), status: None, operation_id: op });
                let reply = self.work_outline.mutate(&mutation).map_err(|e| RpcError::Failed(e.to_wire_message()))?;
                let id = reply.work_item_id.ok_or_else(|| RpcError::Failed("work item id missing".into()))?;
                self.workspace.set_friction_status(&p.space_id, &p.fingerprint, mast_proto::FrictionStatus::Promoted, Some(id.clone()), Some(chrono::Utc::now().timestamp_millis())).map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({"workItemId": id}))
            }
            methods::PROPOSE_FRICTION_INSTRUCTION => {
                let p: mast_proto::ProposeFrictionInstructionParams = parse_params(params)?;
                let row = self.workspace.friction_reports(&p.space_id).into_iter().find(|r| r.fingerprint == p.fingerprint).ok_or_else(|| RpcError::Failed("friction report not found".into()))?;
                let text = row.suggestion.filter(|s| !s.trim().is_empty()).ok_or_else(|| RpcError::BadParams("friction report has no suggestion".into()))?;
                let mutation = mast_proto::WorkOutlineMutation::ProposeWorkItemInstruction(mast_proto::ProposeWorkItemInstructionParams { space_id: Some(p.space_id), target: mast_proto::InstructionProposalTarget::space_root(), origin: Default::default(), text, action: mast_proto::ProposalAction::Add, instruction_id: None, rationale: None, operation_id: mast_proto::derive_id(&row.id, "friction-proposal") });
                let reply = self.work_outline.mutate(&mutation).map_err(|e| RpcError::Failed(e.to_wire_message()))?;
                RpcReply::value(&reply)
            }
            methods::WATCH_WORK_OUTLINE => {
                let p: mast_proto::WatchWorkOutlineParams = parse_params(params)?;
                Ok(RpcReply::Stream(self.work_outline.watch(p.space_id)))
            }
            methods::WORK_OUTLINE_HEALTH => {
                let p: mast_proto::WorkOutlineHealthParams = parse_params(params)?;
                let snapshot = self.work_outline.snapshot(&p.space_id);
                if snapshot.item(&p.work_item_id).is_none() {
                    return Err(RpcError::Failed("work-outline/notFound: work item not found".into()));
                }
                let result = Box::pin(self.agent_tools.health_snapshot(&snapshot, &p.work_item_id)).await
                    .map_err(|error| RpcError::Failed(error.to_wire_message()))?;
                RpcReply::value(&result)
            }
            methods::WATCH_TRACKER_PR_CACHE => {
                Ok(RpcReply::Stream(watch_stream(self.pr_ticket_cache()?.watch_pr_revision())))
            }
            methods::WATCH_LINEAR_SYNC => {
                let p: mast_proto::WatchWorkOutlineParams = parse_params(params)?;
                if self.workspace.space(&p.space_id).map_err(|_| RpcError::Failed("Could not read Space".into()))?.is_none() {
                    return Err(RpcError::Failed("linear/notFound: Space not found".into()));
                }
                Ok(RpcReply::Stream(watch_stream(self.linear.watch_sync(&p.space_id))))
            }
            methods::WORKFLOW_PACK_EXPORT_PREVIEW => {
                let p: crate::workflow_pack::ExportPreviewParams = parse_params(params)?;
                let (pack, redactions) = self
                    .work_outline
                    .workflow_pack()
                    .export_preview(&p.space_id, p.mode)
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&serde_json::json!({"pack": pack, "redactions": redactions}))
            }
            methods::WORKFLOW_PACK_IMPORT_PREVIEW => {
                let p: crate::workflow_pack::ImportPreviewParams = parse_params(params)?;
                let service = self.work_outline.clone();
                let preview = tokio::task::spawn_blocking(move || {
                    service.workflow_pack().preview_import(&p.space_id, &p.path)
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .map_err(RpcError::Failed)?;
                RpcReply::value(&preview)
            }
            methods::WORKFLOW_PACK_APPLY => {
                let p: crate::workflow_pack::ApplyParams = parse_params(params)?;
                let service = self.work_outline.clone();
                let receipt = tokio::task::spawn_blocking(move || {
                    service.workflow_pack().apply(&p.space_id, &p.preview_id, &p.resolutions, &service.local_user(), None)
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .map_err(RpcError::Failed)?;
                RpcReply::value(&receipt)
            }
            methods::WORKFLOW_PACK_UNDO => {
                let p: crate::workflow_pack::UndoParams = parse_params(params)?;
                let service = self.work_outline.clone();
                let receipt = tokio::task::spawn_blocking(move || {
                    service.workflow_pack().undo(&p.space_id, &p.receipt_id, &service.local_user(), None)
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .map_err(RpcError::Failed)?;
                RpcReply::value(&receipt)
            }
            methods::WORKFLOW_PACK_RECEIPTS => {
                let p: crate::workflow_pack::ReceiptsParams = parse_params(params)?;
                let receipts = self
                    .work_outline
                    .workflow_pack()
                    .list_receipts(&p.space_id)
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&serde_json::json!({"receipts": receipts}))
            }
            methods::WORK_OUTLINE_MUTATE => {
                let mutation: mast_proto::WorkOutlineMutation = parse_params(params)?;
                self.linear_mutation_gate(&mutation)?;
                let _account_guard = if let mast_proto::WorkOutlineMutation::SetSpaceLinearAccount(p) = &mutation {
                    let guard = self.linear.lock_account_changes().await;
                    if !self.linear.account_exists(p.account_id.as_deref()) {
                        return Err(RpcError::Failed("linear/notFound: Linear account not found".into()));
                    }
                    Some(guard)
                } else { None };
                let enabled_space = match &mutation {
                    mast_proto::WorkOutlineMutation::SetSpaceLinearEnabled(p) if p.enabled => {
                        Some(p.space_id.clone())
                    }
                    // Picking the linked Linear item syncs the Space now.
                    mast_proto::WorkOutlineMutation::SetLinearAssignedDestination(p)
                        if p.work_item_id.as_deref().is_some_and(|id| !id.is_empty()) =>
                    {
                        Some(p.space_id.clone())
                    }
                    // Picking the linked Linear item syncs the Space now.
                    mast_proto::WorkOutlineMutation::SetLinearAssignedDestination(p)
                        if p.work_item_id.as_deref().is_some_and(|id| !id.is_empty()) =>
                    {
                        Some(p.space_id.clone())
                    }
                    _ => None,
                };
                let pr_sync_space = match &mutation {
                    mast_proto::WorkOutlineMutation::SetPrReviewSyncDestination(p)
                        if p.work_item_id.as_deref().is_some_and(|id| !id.is_empty()) =>
                    {
                        Some(p.space_id.clone())
                    }
                    _ => None,
                };
                let service = self.work_outline.clone();
                // Materializing placement over a large outline is CPU work
                // under the registry lock: keep it off the async workers.
                let reply = tokio::task::spawn_blocking(move || service.mutate(&mutation))
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .map_err(|e| RpcError::Failed(e.to_wire_message()))?;
                // Turning Linear on brings the Space's linked items up to date.
                if let Some(space) = enabled_space {
                    let linear = self.linear.clone();
                    let work_outline = self.work_outline.clone();
                    let pr_review_sync = self.pr_review_sync.clone();
                    tokio::spawn(async move {
                        if let Err(error) = crate::linear::sync::sync_space(
                                &linear,
                                &work_outline,
                                Some(&pr_review_sync),
                                &space,
                                false,
                            )
                            .await
                        {
                            tracing::info!(
                                code = error.code.as_str(),
                                "linear: enable sync stopped"
                            );
                        }
                    });
                }
                // Picking a destination syncs the Space's review requests
                // now (on the Space's own device; elsewhere it is a no-op).
                if let Some(space) = pr_sync_space
                    && self
                        .workspace
                        .space(&space)
                        .ok()
                        .flatten()
                        .is_some_and(|s| s.device_id == self.workspace.device_id())
                {
                    self.pr_review_sync.spawn_space_sync(space);
                }
                RpcReply::value(&reply)
            }
            methods::WATCH_ARTIFACTS => {
                let p: mast_proto::WatchArtifactsParams = parse_params(params)?;
                Ok(RpcReply::Stream(self.artifacts.watch(p.space_id)))
            }
            methods::ARTIFACT_CONTENT => {
                let p: mast_proto::ArtifactContentParams = parse_params(params)?;
                self.artifact_reply(move |service| service.content(&p))
                    .await
            }
            methods::ARTIFACT_REGISTER => {
                let p: mast_proto::ArtifactRegisterParams = parse_params(params)?;
                self.artifact_reply(move |service| service.register(&p))
                    .await
            }
            methods::ARTIFACT_PROMOTE => {
                let p: mast_proto::ArtifactPromoteParams = parse_params(params)?;
                self.artifact_reply(move |service| service.promote(&p.artifact_id))
                    .await
            }
            methods::ARTIFACT_PIN => {
                let p: mast_proto::ArtifactPinParams = parse_params(params)?;
                self.artifact_reply(move |service| service.pin(&p.artifact_id, p.pinned))
                    .await
            }
            methods::ARTIFACT_ANSWER_QUESTION => {
                let p: mast_proto::ArtifactAnswerQuestionParams = parse_params(params)?;
                let by = self.work_outline.local_user();
                let sync =
                    crate::artifact_questions::ArtifactQuestionSync::new(self.artifacts.clone());
                self.artifact_reply(move |service| {
                    let reply = service.answer_question(&p, by)?;
                    // The owning device resolves the Required Action now
                    // instead of on its next pass.
                    sync.reconcile(&p.artifact_id);
                    Ok(reply)
                })
                .await
            }
            methods::ARTIFACT_ANSWER_QUESTIONS => {
                let p: mast_proto::artifacts::ArtifactAnswerQuestionsParams = parse_params(params)?;
                let by = self.work_outline.local_user();
                let sync = crate::artifact_questions::ArtifactQuestionSync::new(self.artifacts.clone());
                self.artifact_reply(move |service| {
                    let reply = service.answer_questions(&p, by)?;
                    sync.reconcile(&p.artifact_id);
                    Ok(reply)
                }).await
            }
            methods::ARTIFACT_SUBMIT_SHEET_DECISIONS => {
                let p: mast_proto::decision_sheet::SubmitSheetDecisionsParams =
                    parse_params(params)?;
                let sync = crate::artifact_questions::ArtifactQuestionSync::new(self.artifacts.clone());
                self.artifact_reply(move |service| {
                    let reply = service.submit_sheet_decisions(&p)?;
                    sync.reconcile(&p.artifact_id);
                    Ok(reply)
                }).await
            }
            methods::ARTIFACT_ADD_COMMENT => {
                let p: mast_proto::ArtifactAddCommentParams = parse_params(params)?;
                let by = self.work_outline.local_user();
                self.artifact_reply(move |service| service.add_comment(&p, by))
                    .await
            }
            methods::ARTIFACT_RESOLVE_COMMENT => {
                let p: mast_proto::ArtifactResolveCommentParams = parse_params(params)?;
                let by = self.work_outline.local_user();
                self.artifact_reply(move |service| service.resolve_comment(&p, by))
                    .await
            }
            methods::ARTIFACT_PICK_OPTION => {
                let p: mast_proto::ArtifactPickOptionParams = parse_params(params)?;
                let by = self.work_outline.local_user();
                let sync =
                    crate::artifact_questions::ArtifactQuestionSync::new(self.artifacts.clone());
                self.artifact_reply(move |service| {
                    let reply = service.pick_option(&p, by)?;
                    sync.after_pick(&p.artifact_id, reply.picked.is_some());
                    Ok(reply)
                })
                .await
            }
            methods::ARTIFACT_DRIFT => {
                let p: mast_proto::ArtifactDriftParams = parse_params(params)?;
                self.artifact_reply(move |service| service.drift(&p.artifact_id))
                    .await
            }
            methods::WATCH_ARTIFACT_RENDER_REQUESTS => {
                let _: mast_proto::WatchArtifactRenderRequestsParams =
                    parse_params(if params.is_null() {
                        serde_json::json!({})
                    } else {
                        params
                    })?;
                Ok(RpcReply::Stream(self.artifacts.render().subscribe()))
            }
            methods::ARTIFACT_RENDER_RESULT => {
                let p: mast_proto::ArtifactRenderResultParams = parse_params(params)?;
                let accepted = self.artifacts.render().respond(p);
                RpcReply::value(&mast_proto::ArtifactRenderResultReply { accepted })
            }
            methods::LINEAR_LIST_ACCOUNTS => RpcReply::value(&self.linear.list_accounts().await),
            methods::LINEAR_ADD_ACCOUNT => {
                let p: mast_proto::linear::LinearAccountKeyParams = parse_params(params)?;
                Self::linear_reply(self.linear.add_account(p.label.as_deref().unwrap_or(""), &p.api_key).await).await
            }
            methods::LINEAR_RENAME_ACCOUNT => {
                let p: mast_proto::linear::LinearRenameAccountParams = parse_params(params)?;
                Self::linear_reply(self.linear.rename_account(&p.account_id, &p.label).await.map(|()| serde_json::json!({}))).await
            }
            methods::LINEAR_REPLACE_ACCOUNT_KEY => {
                let p: mast_proto::linear::LinearAccountKeyParams = parse_params(params)?;
                let id = p.account_id.as_deref().unwrap_or("");
                let result = self.linear.replace_account_key(id, &p.api_key).await;
                if result.is_ok() && let Some(cache) = &self.pr_ticket_cache { cache.invalidate_linear_account(id); }
                Self::linear_reply(result.map(|()| serde_json::json!({}))).await
            }
            methods::LINEAR_REMOVE_ACCOUNT => {
                let p: mast_proto::linear::LinearRemoveAccountParams = parse_params(params)?;
                let result = self.linear.remove_account(&p.account_id, &self.workspace).await;
                if result.is_ok() && let Some(cache) = &self.pr_ticket_cache { cache.invalidate_linear_account(&p.account_id); }
                Self::linear_reply(result.map(|()| serde_json::json!({}))).await
            }
            methods::LINEAR_STATUS => {
                let p: mast_proto::LinearStatusParams = parse_params(params)?;
                Self::linear_reply(self.linear.status(p.force_refresh).await).await
            }
            methods::LINEAR_SET_API_KEY => {
                let p: mast_proto::LinearSetApiKeyParams = parse_params(params)?;
                let result = self.linear.set_api_key(&p.api_key).await;
                if result.is_ok() && let Some(cache) = &self.pr_ticket_cache { cache.invalidate_linear_account("default"); }
                Self::linear_reply(result).await
            }
            methods::LINEAR_CLEAR_API_KEY => {
                let _: mast_proto::LinearClearApiKeyParams = parse_params(params)?;
                let result = self.linear.clear_api_key().await;
                if result.is_ok() && let Some(cache) = &self.pr_ticket_cache { cache.invalidate_linear_account("default"); }
                Self::linear_reply(result).await
            }
            methods::LINEAR_ISSUE => {
                let p: mast_proto::LinearIssueParams = parse_params(params)?;
                Self::linear_reply(self.linear_for_space(p.space_id.as_deref(), p.expected_account_id.as_deref())?.issue(&p.identifier, p.force_refresh).await).await
            }
            methods::LINEAR_MY_ISSUES => {
                let p: mast_proto::LinearMyIssuesParams = parse_params(params)?;
                Self::linear_reply(self.linear_for_space(p.space_id.as_deref(), p.expected_account_id.as_deref())?.my_issues(p.force, p.cached_only).await).await
            }
            methods::LINEAR_MY_PROJECTS => {
                let p: mast_proto::LinearMyProjectsParams = parse_params(params)?;
                Self::linear_reply(self.linear_for_space(p.space_id.as_deref(), p.expected_account_id.as_deref())?.my_projects(p.force, p.cached_only).await).await
            }
            methods::LINEAR_PROJECT_ISSUES => {
                let p: mast_proto::LinearProjectIssuesParams = parse_params(params)?;
                Self::linear_reply(
                    self.linear_for_space(p.space_id.as_deref(), p.expected_account_id.as_deref())?
                        .project_issues(&p.project_id, p.include_completed, p.force, p.cached_only)
                        .await,
                )
                .await
            }
            methods::LINEAR_PROJECT_DETAIL => {
                let p: mast_proto::LinearProjectDetailParams = parse_params(params)?;
                Self::linear_reply(
                    self.linear_for_space(p.space_id.as_deref(), p.expected_account_id.as_deref())?
                        .project_detail(&p.project_id, p.force, p.cached_only)
                        .await,
                )
                .await
            }
            methods::LINEAR_REFRESH_LINKED_ITEMS => {
                let p: mast_proto::LinearRefreshLinkedItemsParams = parse_params(params)?;
                let result = crate::linear::sync::sync_space(
                    &self.linear,
                    &self.work_outline,
                    Some(&self.pr_review_sync),
                    &p.space_id,
                    true,
                )
                .await
                .map(|updated| mast_proto::LinearRefreshLinkedItemsReply { updated });
                Self::linear_reply(result).await
            }
            methods::PR_STATUS => {
                let p: mast_proto::PrStatusParams = parse_params(params)?;
                Self::github_reply(self.pr_review_sync.status(&p).await)
            }
            methods::PR_VIEWER => Self::github_reply(
                self.pr_review_sync
                    .viewer_login()
                    .await
                    .map(|login| mast_proto::PrViewerReply { login }),
            ),
            methods::PR_REVIEW_SYNC_NOW => {
                let p: mast_proto::PrReviewSyncNowParams = parse_params(params)?;
                Self::github_reply(self.pr_review_sync.sync_space(&p.space_id, true).await)
            }
            methods::PR_MY_OPEN => {
                let p: mast_proto::PrMyOpenParams = parse_params(params)?;
                Self::github_reply(self.pr_review_sync.my_open(&p.space_id).await)
            }
            methods::PR_REVIEW_REQUESTED => {
                let p: mast_proto::PrReviewRequestedParams = parse_params(params)?;
                Self::github_reply(self.pr_review_sync.review_requested(&p.space_id).await)
            }
            methods::WORK_OUTLINE_QUERY => {
                let query: mast_proto::WorkOutlineQueryRequest = parse_params(params)?;
                let service = self.work_outline.clone();
                let reply = tokio::task::spawn_blocking(move || service.query(&query))
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .map_err(|e| RpcError::Failed(e.to_wire_message()))?;
                RpcReply::value(&reply)
            }
            // Agent tools: local to the engine that minted the token (not in
            // `forwardable`, so `targetDeviceId` is ignored).
            mast_proto::agent_tools::AUTHENTICATE_METHOD => {
                let p: mast_proto::agent_tools::AgentAuthenticateParams = parse_params(params)?;
                let origin = self
                    .agent_tools
                    .authenticate(&p.token)
                    .map_err(|e| RpcError::Failed(e.to_wire_message()))?;
                RpcReply::value(&origin)
            }
            mast_proto::agent_tools::TOOL_CALL_METHOD => {
                let p: mast_proto::agent_tools::AgentToolCallParams = parse_params(params)?;
                let service = self.agent_tools.clone();
                // Resolve the grant before dispatch; even a refused authorization is
                // attributable when the per-run token itself is still valid.
                let grant = service.tokens().authenticate(&p.token).ok();
                let tool = p.tool.clone();
                let result: Result<serde_json::Value, RpcError> =
                    if p.tool == mast_proto::agent_tools::tools::LAUNCH_CHILD_CHAT {
                        match tokio::task::spawn_blocking(move || service.prepare_child_launch(&p))
                            .await
                        {
                            Ok(Ok(launch)) => self
                                .launch
                                .create_chat_for_work_item(&launch)
                                .await
                                .map_err(|e| RpcError::Failed(e.to_wire_message()))
                                .and_then(|reply| {
                                    serde_json::to_value(reply)
                                        .map_err(|e| RpcError::Failed(e.to_string()))
                                }),
                            Ok(Err(e)) => Err(RpcError::Failed(e.to_wire_message())),
                            Err(e) => Err(RpcError::Failed(e.to_string())),
                        }
                    } else if p.tool == mast_proto::agent_tools::tools::HAND_OFF_ORCHESTRATOR {
                        match tokio::task::spawn_blocking(move || service.prepare_orchestrator_successor(&p)).await {
                            Ok(Ok((mut launch, source, harness, model, reasoning))) => {
                                launch.config = hand_off_config(&self.registry, self.repos.data_dir(), launch.config, self.doc_host.harness_for(&source), harness, model, reasoning).await?;
                                self.launch.create_orchestrator_successor(&launch, &self.orchestrator_wakes, &source)
                                .await
                                .map_err(|e| RpcError::Failed(e.to_wire_message()))
                                .and_then(|reply| serde_json::to_value(reply).map_err(|e| RpcError::Failed(e.to_string())))
                            },
                            Ok(Err(e)) => Err(RpcError::Failed(e.to_wire_message())),
                            Err(e) => Err(RpcError::Failed(e.to_string())),
                        }
                    } else {
                        match tokio::task::spawn_blocking(move || service.call(&p)).await {
                            Ok(Ok(reply)) => Ok(reply),
                            Ok(Err(e)) => Err(RpcError::Failed(e.to_wire_message())),
                            Err(e) => Err(RpcError::Failed(e.to_string())),
                        }
                    };
                if let Err(error) = &result
                    && let Some(grant) = grant
                {
                    self.sessions
                        .note_agent_tool_failure(&grant.chat_id, &grant.run_id, &tool, &error.to_string());
                }
                RpcReply::value(&result?)
            }
            methods::VALIDATE_EXECUTION_CONFIGURATION => {
                let p: mast_proto::ValidateExecutionConfigurationParams = parse_params(params)?;
                let reply = self.validate_execution_configuration(p).await?;
                RpcReply::value(&reply)
            }
            methods::CREATE_CHAT_FOR_WORK_ITEM => {
                let p: mast_proto::CreateChatForWorkItemParams = parse_params(params)?;
                let reply = self.create_chat_for_work_item(p).await?;
                RpcReply::value(&reply)
            }
            methods::HAND_OFF_ORCHESTRATOR => {
                let p: mast_proto::HandOffOrchestratorParams = parse_params(params)?;
                let source = self.workspace.chat(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .ok_or_else(|| RpcError::Failed("Chat not found".into()))?;
                if source.archived {
                    return Err(RpcError::Failed("An archived chat cannot hand off orchestration".into()));
                }
                let space_id = source.space_id.as_deref()
                    .ok_or_else(|| RpcError::Failed("Chat has no Space".into()))?;
                let item = self.workspace.chat_assigned_work_item(&p.chat_id, space_id)
                    .ok_or_else(|| RpcError::Failed("Chat has no work item".into()))?;
                let config = hand_off_config(&self.registry, self.repos.data_dir(), source.config, self.doc_host.harness_for(&p.chat_id), p.harness, p.model, p.reasoning).await?;
                let launch = mast_proto::CreateChatForWorkItemParams {
                    work_item_id: item,
                    initial_prompt: mast_proto::WorkItemPrompt { text: p.summary, attachments: Vec::new() },
                    config,
                    execution_override: None,
                    worktree_choice: None,
                    operation_id: p.operation_id,
                    parent_chat_id: None,
                    target_device_id: None,
                    account_id: None,
                };
                let reply = self.launch.create_orchestrator_successor(&launch, &self.orchestrator_wakes, &p.chat_id)
                    .await.map_err(|e| RpcError::Failed(e.to_wire_message()))?;
                RpcReply::value(&reply)
            }
            methods::CONTINUE_CHAT_FOR_WORK_ITEM => {
                let p: mast_proto::ContinueChatForWorkItemParams = parse_params(params)?;
                let reply = self
                    .launch
                    .continue_chat_for_work_item(&p)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_wire_message()))?;
                RpcReply::value(&reply)
            }
            methods::DESCRIBE_EXECUTION_REPOSITORY => {
                let p: mast_proto::DescribeExecutionRepositoryParams = parse_params(params)?;
                let identity = crate::execution_scope::describe_repository(
                    self.execution_scope.probe(),
                    std::path::Path::new(&p.path),
                )
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&identity)
            }
            methods::WATCH_SIDEBAR_PREFERENCES => Ok(RpcReply::Stream(watch_stream(
                self.workspace.watch_sidebar_preferences(),
            ))),
            methods::WATCH_DEVICES => Ok(RpcReply::Stream(watch_stream(
                self.workspace.watch_devices(),
            ))),
            methods::WATCH_SPACES => Ok(RpcReply::Stream(watch_stream(
                self.workspace.watch_spaces(),
            ))),
            methods::WATCH_SESSIONS => {
                // Local live statuses merged with remote devices' workspace rows.
                let merged = self
                    .workspace
                    .merged_sessions_watch(self.sessions.watch_sessions());
                Ok(RpcReply::Stream(watch_stream(merged)))
            }
            methods::LOCAL_DEVICE => {
                RpcReply::value(&serde_json::json!({ "deviceId": self.doc_host.device_id() }))
            }
            methods::LOCAL_IMPORT_STATUS => {
                let importer = self.local_importer()?.clone();
                let status = tokio::task::spawn_blocking(move || importer.status())
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&status)
            }
            methods::IMPORT_LOCAL_WORKSPACE => {
                let importer = self.local_importer()?.clone();
                // Progress rides an unbounded channel: the importer is
                // blocking (sqlite + fs) and must never wedge on a slow
                // viewer; items are tiny and bounded by the chat count.
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
                tokio::task::spawn_blocking(move || {
                    let emit = |event: crate::local_import::ImportEvent| {
                        if let Ok(item) = serde_json::to_value(&event) {
                            let _ = tx.send(item);
                        }
                    };
                    if let Err(err) = importer.run(emit) {
                        tracing::error!(error = %err, "local import failed");
                        let _ = tx.send(serde_json::json!({
                            "kind": "summary",
                            "importedChats": 0, "importedSpaces": 0,
                            "skippedChats": 0, "skippedSpaces": 0,
                            "journalsCopied": 0, "ledgerRowsMerged": 0,
                            "errors": [format!("{err}")],
                        }));
                    }
                    // tx drops here — the stream ends after the summary item.
                });
                Ok(RpcReply::Stream(Box::pin(futures::stream::poll_fn(
                    move |cx| rx.poll_recv(cx),
                ))))
            }
            methods::SCAN_EXTERNAL_SESSIONS => {
                let importer = self.external_importer()?.clone();
                let candidates = tokio::task::spawn_blocking(move || importer.scan())
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&candidates)
            }
            methods::IMPORT_EXTERNAL_SESSION => {
                let p: ImportExternalSessionParams = parse_params(params)?;
                let importer = self.external_importer()?.clone();
                let imported = tokio::task::spawn_blocking(move || {
                    importer.import(
                        &p.chat_id,
                        &p.external_session_id,
                        std::path::Path::new(&p.path),
                    )
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&imported)
            }
            methods::CHECK_SESSION_LIVENESS => {
                let p: CheckSessionLivenessParams = parse_params(params)?;
                let check = tokio::task::spawn_blocking(move || {
                    crate::liveness::check_liveness(
                        std::path::Path::new(&p.path),
                        &p.session_id,
                        &p.cwd,
                    )
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?;
                // `LivenessCheck` isn't `Serialize` (owned by another pass,
                // not touched here) — its two fields are `pub`, so build the
                // reply value directly rather than widen that type.
                RpcReply::value(&serde_json::json!({
                    "recentlyModified": check.recently_modified,
                    "liveProcessMatch": check.live_process_match,
                }))
            }
            methods::SYNC_EXTERNAL_SESSION => {
                let p: SyncExternalSessionParams = parse_params(params)?;
                let importer = self.external_importer()?.clone();
                let result = tokio::task::spawn_blocking(move || importer.sync(&p.chat_id))
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&result)
            }
            methods::BULK_IMPORT_SESSION_CANVAS_SESSIONS => {
                let importer = self.external_importer()?.clone();
                // Same reasoning as IMPORT_LOCAL_WORKSPACE: this can process
                // hundreds of files, so progress rides an unbounded channel
                // rather than one blocking reply.
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
                tokio::task::spawn_blocking(move || {
                    let emit = |event: crate::external_import::BulkImportEvent| {
                        if let Ok(item) = serde_json::to_value(&event) {
                            let _ = tx.send(item);
                        }
                    };
                    if let Err(err) = importer.bulk_import_from_session_canvas(emit) {
                        tracing::error!(error = %err, "bulk session-canvas import failed");
                        let _ = tx.send(serde_json::json!({
                            "kind": "summary",
                            "total": 0, "imported": 0, "archived": 0, "failed": 0,
                            "errors": [format!("{err}")],
                        }));
                    }
                    // tx drops here — the stream ends after the summary item.
                });
                Ok(RpcReply::Stream(Box::pin(futures::stream::poll_fn(
                    move |cx| rx.poll_recv(cx),
                ))))
            }
            methods::CHAT_LINK_STATUS => {
                let p: ChatLinkStatusParams = parse_params(params)?;
                let chat = self
                    .workspace
                    .chat(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let pr_links = self
                    .workspace
                    .chat_pr_links(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let selected = {
                    let receiver = self.diff_sync.watch_chat_diff_targets();
                    let targets = receiver.borrow();
                    targets
                        .iter()
                        .find(|view| view.chat_id == p.chat_id)
                        .and_then(|view| view.targets.iter().find(|target| target.selected))
                        .cloned()
                };
                let mut status = self.pr_ticket_cache()?.status_for_chat_links_with_checkout(
                    chat.as_ref().and_then(|c| c.space_id.as_deref()),
                    chat.as_ref().and_then(|c| c.cwd.as_deref()),
                    chat.as_ref().and_then(|c| c.branch.as_deref()),
                    &pr_links,
                    chat.as_ref().and_then(|c| c.linked_ticket_id.as_deref()),
                    selected
                        .as_ref()
                        .map(|target| (target.path.as_str(), target.branch.as_deref())),
                );
                if let Some(chat) = &chat {
                    // `prSource`/`ticketSource`: the durable link's own
                    // provenance when one is set, else `None` — the shown
                    // value came from branch/title inference instead.
                    if status.pr_links.is_empty() {
                        status.pr_source = chat.linked_pr_url.as_ref().and(chat.linked_pr_source);
                    }
                    status.ticket_source = chat
                        .linked_ticket_id
                        .as_ref()
                        .and(chat.linked_ticket_source);

                    // Ticket-from-PR (design item 5): a chat with PR detail
                    // (linked or inferred) but no ticket link and no regex
                    // hit off its OWN branch/title gets one last shot — the
                    // PR's own title/headRefName.
                    let has_ticket_signal = chat.linked_ticket_id.is_some()
                        || chat
                            .branch
                            .as_deref()
                            .and_then(crate::pr_ticket_cache::extract_ticket_id)
                            .is_some()
                        || chat
                            .title
                            .as_deref()
                            .and_then(crate::pr_ticket_cache::extract_ticket_id)
                            .is_some();
                    if !has_ticket_signal
                        && let Some(ticket_id) = status
                            .pr_links
                            .iter()
                            .filter_map(|link| link.detail.as_ref())
                            .chain(status.pr.iter())
                            .find_map(|pr| {
                                pr.title
                                    .as_deref()
                                    .and_then(crate::pr_ticket_cache::extract_ticket_id)
                                    .or_else(|| {
                                        pr.branch
                                            .as_deref()
                                            .and_then(crate::pr_ticket_cache::extract_ticket_id)
                                    })
                            })
                    {
                        match self.workspace.set_chat_link(
                            &p.chat_id,
                            crate::workspace_host::ChatLinkKind::Ticket,
                            Some(&ticket_id),
                            mast_proto::ChatLinkSource::Mentioned,
                        ) {
                            Ok(true) => {
                                status.ticket_source = Some(mast_proto::ChatLinkSource::Mentioned);
                            }
                            Ok(false) => {}
                            Err(err) => tracing::warn!(
                                chat = %p.chat_id, error = %err,
                                "ticket-from-PR link write failed"
                            ),
                        }
                    }
                }
                RpcReply::value(&status)
            }
            methods::SET_CHAT_LINK => {
                let p: SetChatLinkParams = parse_params(params)?;
                let kind = match p.kind.as_str() {
                    "pr" => crate::workspace_host::ChatLinkKind::Pr,
                    "ticket" => crate::workspace_host::ChatLinkKind::Ticket,
                    other => {
                        return Err(RpcError::BadParams(format!("unknown link kind: {other}")));
                    }
                };
                match (kind, p.operation.as_deref()) {
                    (crate::workspace_host::ChatLinkKind::Pr, Some("remove")) => {
                        let url = p.value.as_deref().ok_or_else(|| {
                            RpcError::BadParams("remove requires a PR URL value".into())
                        })?;
                        self.workspace
                            .remove_chat_pr_link(&p.chat_id, url)
                            .map_err(|e| RpcError::Failed(e.to_string()))?;
                    }
                    (crate::workspace_host::ChatLinkKind::Pr, Some("clear")) => {
                        self.workspace
                            .clear_chat_pr_links(&p.chat_id)
                            .map_err(|e| RpcError::Failed(e.to_string()))?;
                    }
                    (crate::workspace_host::ChatLinkKind::Ticket, Some("remove" | "clear")) => {
                        self.workspace
                            .set_chat_link(
                                &p.chat_id,
                                kind,
                                None,
                                mast_proto::ChatLinkSource::Manual,
                            )
                            .map_err(|e| RpcError::Failed(e.to_string()))?;
                    }
                    (_, Some(operation)) if operation != "add" => {
                        return Err(RpcError::BadParams(format!(
                            "unknown link operation: {operation}"
                        )));
                    }
                    _ => {
                        self.workspace
                            .set_chat_link(
                                &p.chat_id,
                                kind,
                                p.value.as_deref(),
                                mast_proto::ChatLinkSource::Manual,
                            )
                            .map_err(|e| RpcError::Failed(e.to_string()))?;
                    }
                }
                let chat = self
                    .workspace
                    .chat(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let linked_pr_links = self
                    .workspace
                    .chat_pr_links(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({
                    "linkedPrUrl": chat.as_ref().and_then(|c| c.linked_pr_url.clone()),
                    "linkedPrSource": chat.as_ref().and_then(|c| c.linked_pr_source),
                    "linkedPrLinks": linked_pr_links,
                    "linkedTicketId": chat.as_ref().and_then(|c| c.linked_ticket_id.clone()),
                    "linkedTicketSource": chat.as_ref().and_then(|c| c.linked_ticket_source),
                }))
            }
            methods::SCAN_CHAT_SUBAGENTS => {
                let p: ChatLinkStatusParams = parse_params(params)?;
                let importer = self.external_importer()?.clone();
                let provider = self.context_usage()?.clone();
                let workspace = self.workspace.clone();
                let subagents = tokio::task::spawn_blocking(move || {
                    // Same two-source transcript resolution `CHAT_CONTEXT_USAGE`
                    // uses (`ContextUsageProvider::transcript_path_for_chat`):
                    // the import cursor alone only covers chats that went
                    // through `ExternalSessionImporter::import` — a chat Mast
                    // itself launched (harness_session_id set directly by the
                    // run loop, never imported) has no cursor at all, so
                    // relying on the cursor exclusively silently returned no
                    // subagents for exactly the chats most likely to have
                    // spawned any.
                    match provider.transcript_path_for_chat(&workspace, &importer, &p.chat_id)? {
                        Some(path) => crate::subagent_scan::scan_subagents(&path),
                        None => Ok(Vec::new()),
                    }
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&subagents)
            }
            methods::READ_SUBAGENT_TRANSCRIPT => {
                let p: ReadSubagentTranscriptParams = parse_params(params)?;
                let importer = self.external_importer()?.clone();
                let provider = self.context_usage()?.clone();
                let workspace = self.workspace.clone();
                let cache = self.subagent_transcript_cache.clone();
                let transcript = tokio::task::spawn_blocking(move || {
                    // Re-resolve the PARENT chat's transcript path server-side
                    // — same two-source resolution `SCAN_CHAT_SUBAGENTS` uses
                    // — then derive the subagent's own file from it. Never
                    // trust a client-supplied path (see
                    // `methods::READ_SUBAGENT_TRANSCRIPT`'s doc comment).
                    let parent =
                        provider.transcript_path_for_chat(&workspace, &importer, &p.chat_id)?;
                    let subagent_path = parent.as_deref().and_then(|parent| {
                        crate::subagent_scan::subagent_transcript_path(parent, &p.agent_id)
                    });
                    match subagent_path {
                        // An unknown chat/agent, or one whose transcript
                        // hasn't been written (yet/anymore), resolves to a
                        // path that just doesn't exist — the cache already
                        // treats that as an empty transcript, not an error.
                        Some(path) => cache.get_or_build(&path),
                        None => Ok(std::sync::Arc::new(
                            crate::subagent_transcript::SubagentTranscript::default(),
                        )),
                    }
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&*transcript)
            }
            methods::CHAT_CONTEXT_USAGE => {
                let p: ChatLinkStatusParams = parse_params(params)?;
                let provider = self.context_usage()?.clone();
                let workspace = self.workspace.clone();
                let importer = self.external_importer()?.clone();
                let pct = tokio::task::spawn_blocking(move || {
                    provider.context_pct_for_chat(&workspace, &importer, &p.chat_id)
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "contextPct": pct }))
            }
            methods::MY_OPEN_PRS => {
                let cache = self.pr_ticket_cache()?;
                cache
                    .wait_for_my_open_prs_ready(MY_OPEN_PRS_INITIAL_WAIT)
                    .await;
                let items = cache.my_open_prs();
                RpcReply::value(&items)
            }
            methods::GET_REVIEW_THREADS => {
                let p: mast_proto::PrStatusParams = parse_params(params)?;
                let dash = self.pr_dash.get_or_init(|| {
                    crate::pr_dash::PrDash::with_real_gh()
                        .with_cache_file(self.repos.data_dir().join("cache").join("pr-dash.json"))
                });
                let threads = dash.review_threads(&p.owner, &p.repo, p.number).await
                    .map_err(RpcError::Failed)?;
                RpcReply::value(&threads)
            }
            methods::PR_DASHBOARD => {
                let p: mast_proto::PrDashParams = parse_params(params)?;
                let dash = self.pr_dash.get_or_init(|| {
                    crate::pr_dash::PrDash::with_real_gh()
                        .with_cache_file(self.repos.data_dir().join("cache").join("pr-dash.json"))
                });
                let mut reply = if p.cached_only {
                    dash.peek(p.tab).unwrap_or_default()
                } else {
                    dash.dashboard(p.tab, p.force).await
                };
                // Chat links change independently of the PR rows, so they are
                // resolved on every call, cached rows or not.
                let candidates: Vec<crate::pr_chat_links::PrLinkCandidate> = reply
                    .rows
                    .iter()
                    .map(|row| crate::pr_chat_links::PrLinkCandidate {
                        url: row.url.clone(),
                        number: row.number,
                        repo_full: row.repo_full.clone(),
                        head_ref: row.head_ref.clone(),
                        title: row.title.clone(),
                    })
                    .collect();
                let mut chats = self.workspace.pr_chat_matches(&candidates).await;
                for row in &mut reply.rows {
                    row.chats = chats.remove(&row.url).unwrap_or_default();
                }
                RpcReply::value(&reply)
            }
            methods::START_PR_REVIEW => {
                let p: mast_proto::StartPrReviewParams = parse_params(params)?;
                RpcReply::value(&self.start_pr_review(p).await?)
            }
            methods::START_BRANCH_REVIEW => {
                let mut p: mast_proto::StartBranchReviewParams = parse_params(params)?;
                let space = self.workspace.space(&p.space_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .ok_or_else(|| RpcError::Failed("Space not found".into()))?;
                if space.device_id != self.doc_host.device_id() {
                    if p.target_device_id.is_some() {
                        return Err(RpcError::Failed("Space lives on another device".into()));
                    }
                    p.target_device_id = Some(space.device_id.clone());
                    let params = serde_json::to_value(&p).map_err(|e| RpcError::Failed(e.to_string()))?;
                    return self.forward(&space.device_id, methods::START_BRANCH_REVIEW, params).await;
                }
                let reply = self.pr_review.start_branch(&p).await
                    .map_err(|e| RpcError::Failed(e.to_wire_message()))?;
                RpcReply::value(&reply)
            }
            methods::REVIEW_REQUESTED_PRS => {
                let items = self.pr_ticket_cache()?.review_requested_prs();
                RpcReply::value(&items)
            }
            methods::PLAN_CHAT_WORKSPACE => {
                let p: PlanChatWorkspaceParams = parse_params(params)?;
                // Fresh, unpooled client: this fires at most once per new
                // chat (see the method's own doc comment — "nothing
                // cached"), so there's no benefit to threading a shared
                // client through `EngineRpc` for it.
                let http = reqwest::Client::new();
                let plan =
                    crate::chat_workspace_plan::plan_chat_workspace(&http, &p.message, &p.cwd)
                        .await;
                RpcReply::value(&plan)
            }
            methods::CREATE_CHAT_WORKTREE => {
                let p: CreateChatWorktreeParams = parse_params(params)?;
                // A bound Chat never moves to a new worktree (contract 6.2):
                // refuse before creating one that could not be used.
                let binding = self
                    .workspace
                    .checked_chat_execution_binding(&p.chat_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                if let Some(binding) = binding {
                    return Err(RpcError::Failed(crate::execution_scope::rejection_message(
                        &[mast_proto::ExecutionValidationError::BoundChatRetarget {
                            chat_id: p.chat_id.clone(),
                            execution_root: binding.resolved.configuration.execution_root,
                            requested_path: String::new(),
                        }],
                    )));
                }
                let repo_path = std::path::PathBuf::from(&p.repo_path);
                let name = p.name.clone();
                let worktree = tokio::task::spawn_blocking(move || {
                    crate::chat_workspace_plan::create_chat_worktree(&repo_path, &name)
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .map_err(|e| RpcError::Failed(e.to_string()))?;
                // Stamp the chat's cwd so its next dispatch runs in the new
                // worktree automatically (same durable field `SetChatCwd`/
                // `Mutate` writes, and the same one `materialize_worktree`'s
                // Run-time worktree spec stamps post-creation). Best-effort:
                // a chat row that doesn't exist yet (`Ok(false)`) or a write
                // error doesn't fail worktree creation itself — the worktree
                // is real and usable either way, just not yet wired to this
                // chat's next send.
                let worktree_identity = self
                    .repos
                    .checkout_identity(std::path::Path::new(&worktree.worktree_path))
                    .await
                    .ok();
                let canonical_worktree = worktree_identity
                    .as_ref()
                    .map(|identity| identity.root.to_string_lossy().into_owned())
                    .unwrap_or_else(|| worktree.worktree_path.clone());
                match self.workspace.set_chat_target(
                    &p.chat_id,
                    &canonical_worktree,
                    worktree_identity
                        .as_ref()
                        .map(|identity| identity.id.as_str()),
                ) {
                    Ok(true) => {}
                    Ok(false) => tracing::warn!(
                        chat = %p.chat_id,
                        worktree = %worktree.worktree_path,
                        "CreateChatWorktree: chat row does not exist yet, cwd not stamped"
                    ),
                    Err(err) => tracing::warn!(
                        chat = %p.chat_id,
                        worktree = %worktree.worktree_path,
                        error = %err,
                        "CreateChatWorktree: chat cwd stamp failed"
                    ),
                }
                RpcReply::value(&worktree)
            }
            methods::PLAN_CHAT_CLOSEOUT => {
                let p: PlanChatCloseoutParams = parse_params(params)?;
                let chat_id = p.chat_id.as_deref();
                let cwd = self.resolved_closeout_cwd(chat_id, &p.cwd)?;
                let shared = self.open_chats_on_checkout(chat_id, &cwd).await?;
                let canonical_cwd = cwd.to_string_lossy().into_owned();
                let chat_live = self.closeout_is_live(chat_id, &canonical_cwd).await;
                let plan = tokio::task::spawn_blocking(move || {
                    let mut plan = crate::chat_workspace_plan::plan_chat_closeout(&cwd, chat_live);
                    plan.shared_chats = shared;
                    plan
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&plan)
            }
            methods::CLOSE_CHAT_WORKTREE => {
                let p: CloseChatWorktreeParams = parse_params(params)?;
                let chat_id = p.chat_id.as_deref();
                let cwd = self.resolved_closeout_cwd(chat_id, &p.cwd)?;
                let shared = self.open_chats_on_checkout(chat_id, &cwd).await?;
                if !shared.is_empty() {
                    return Err(shared_closeout_error(&shared));
                }
                let canonical_cwd = cwd.to_string_lossy().into_owned();
                let chat_live = self.closeout_is_live(chat_id, &canonical_cwd).await;
                let force = p.force;
                let mut outcome = tokio::task::spawn_blocking(move || {
                    crate::chat_workspace_plan::close_chat_worktree(&cwd, force, chat_live)
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .map_err(|e| RpcError::Failed(e.to_string()))?;
                // Archive via the same LWW flag `Mutate{setChatArchived}`
                // writes. Best-effort like `CreateChatWorktree`'s cwd stamp:
                // the worktree is already gone, so a missing chat row
                // (`Ok(false)`) or write error reports `archived: false`
                // rather than failing the close-out.
                if let Some(chat_id) = p.chat_id.as_deref() {
                    match self.workspace.set_chat_archived(chat_id, true) {
                        Ok(archived) => outcome.archived = archived,
                        Err(err) => tracing::warn!(
                            chat = %chat_id,
                            error = %err,
                            "CloseChatWorktree: archiving the chat failed"
                        ),
                    }
                }
                RpcReply::value(&outcome)
            }
            methods::UPDATE_STATUS => Ok(RpcReply::Stream(watch_stream(self.updater()?.watch()))),
            methods::APPLY_UPDATE => {
                let version = self
                    .updater()?
                    .apply()
                    .await
                    .map_err(|e| RpcError::Failed(format!("{e:#}")))?;
                RpcReply::value(&serde_json::json!({ "ok": true, "version": version }))
            }
            methods::MUTATE => {
                let p: MutateParams = parse_params(params)?;
                let sidebar_pins = matches!(&p, MutateParams::ChangeSidebarPin { .. });
                self.mutate(p).await?;
                if sidebar_pins {
                    return RpcReply::value(&serde_json::json!({
                        "ok": true, "sidebarPreferences": self.workspace.sidebar_preferences_snapshot(),
                    }));
                }
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::WATCH_CHECKOUT_DIFFS => {
                let p = if params.is_null() {
                    WatchCheckoutDiffsParams::default()
                } else {
                    parse_params(params)?
                };
                let scope = match p.cwd.as_deref() {
                    Some(cwd) => {
                        let identity = self
                            .repos
                            .checkout_identity(std::path::Path::new(cwd))
                            .await
                            .map_err(|error| {
                                RpcError::Failed(format!(
                                    "could not resolve checkout diff cwd: {error}"
                                ))
                            })?;
                        if let Some(expected) = p.checkout_id.as_deref()
                            && expected != identity.id
                        {
                            return Err(RpcError::Failed(
                                "checkoutId does not match canonical cwd".into(),
                            ));
                        }
                        Some(identity.id)
                    }
                    None => p.checkout_id,
                };
                match scope {
                    None => Ok(RpcReply::Stream(watch_stream(self.diff_sync.watch_diffs()))),
                    Some(checkout_id) => Ok(RpcReply::Stream(scoped_checkout_diff_stream(
                        self.diff_sync.watch_diffs(),
                        checkout_id,
                        self.engine_info.device_id.clone(),
                    ))),
                }
            }
            methods::WATCH_CHAT_DIFF_TARGETS => Ok(RpcReply::Stream(watch_stream(
                self.diff_sync.watch_chat_diff_targets(),
            ))),
            methods::LIST_CHAT_DIFF_CHECKOUTS => {
                let p: mast_proto::ListChatDiffCheckoutsParams = parse_params(params)?;
                let targets = self.diff_targets.clone();
                let reply = tokio::task::spawn_blocking(move || targets.list_checkouts(&p.chat_id))
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&reply)
            }
            methods::LIST_CHAT_REF_DIFF_CANDIDATES => {
                let p: mast_proto::ListChatDiffCheckoutsParams = parse_params(params)?;
                let targets = self.diff_targets.clone();
                let reply =
                    tokio::task::spawn_blocking(move || targets.list_ref_candidates(&p.chat_id))
                        .await
                        .map_err(|e| RpcError::Failed(e.to_string()))?
                        .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&reply)
            }
            methods::ACCEPT_REVIEW_SUGGESTION => {
                let draft: mast_proto::ReviewDraft = serde_json::from_value(params.get("draft").cloned()
                    .ok_or_else(|| RpcError::Failed("draft is required".into()))?)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                let id = params.get("id").and_then(serde_json::Value::as_str)
                    .filter(|id| !id.trim().is_empty()).ok_or_else(|| RpcError::Failed("id is required".into()))?;
                RpcReply::value(&self.workspace.accept_review_suggestion(&draft, id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?)
            }
            methods::GET_DISMISSED_REVIEW_SUGGESTIONS | methods::DISMISS_REVIEW_SUGGESTION => {
                let target = params.get("target").and_then(serde_json::Value::as_str)
                    .filter(|target| !target.trim().is_empty())
                    .ok_or_else(|| RpcError::Failed("target is required".into()))?;
                if method == methods::GET_DISMISSED_REVIEW_SUGGESTIONS {
                    RpcReply::value(&self.workspace.dismissed_review_suggestions(target))
                } else {
                    let id = params.get("id").and_then(serde_json::Value::as_str)
                        .filter(|id| !id.trim().is_empty())
                        .ok_or_else(|| RpcError::Failed("id is required".into()))?;
                    RpcReply::value(&self.workspace.dismiss_review_suggestion(target, id)
                        .map_err(|error| RpcError::Failed(error.to_string()))?)
                }
            }
            methods::GET_REVIEW_DRAFTS => {
                let target = params.get("target").and_then(serde_json::Value::as_str)
                    .filter(|target| !target.trim().is_empty())
                    .ok_or_else(|| RpcError::Failed("target is required".into()))?;
                RpcReply::value(&self.workspace.review_drafts(target))
            }
            methods::SET_REVIEW_DRAFTS => {
                let target = params.get("target").and_then(serde_json::Value::as_str)
                    .filter(|target| !target.trim().is_empty())
                    .ok_or_else(|| RpcError::Failed("target is required".into()))?;
                let drafts: Vec<mast_proto::ReviewDraft> = serde_json::from_value(
                    params.get("drafts").cloned().ok_or_else(|| RpcError::Failed("drafts is required".into()))?
                ).map_err(|error| RpcError::Failed(error.to_string()))?;
                let before: Vec<mast_proto::ReviewDraft> = serde_json::from_value(
                    params.get("before").cloned().ok_or_else(|| RpcError::Failed("before is required".into()))?
                ).map_err(|error| RpcError::Failed(error.to_string()))?;
                RpcReply::value(&self.workspace.update_review_drafts(target, &before, &drafts)
                    .map_err(|error| RpcError::Failed(error.to_string()))?)
            }
            methods::GET_DIFF_REVIEW_STATE => {
                let target = params
                    .get("target")
                    .and_then(serde_json::Value::as_str)
                    .filter(|target| !target.trim().is_empty())
                    .ok_or_else(|| RpcError::Failed("target is required".into()))?;
                let mut state = self
                    .workspace
                    .diff_review_state(target)
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                if params.get("readOnly").and_then(serde_json::Value::as_bool) == Some(true) {
                    return RpcReply::value(&mast_proto::DiffReviewStateReply { state });
                }
                // A PR can inherit the marks recorded while its branch was
                // reviewed. Only initialize an absent PR row; subsequent reads
                // must not resurrect marks the reviewer explicitly cleared.
                if state.is_none() && target.starts_with("pr:") {
                    let pr = target.strip_prefix("pr:")
                        .and_then(mast_proto::PullRequestRef::parse_external_id);
                    let mut cached = pr.as_ref().and_then(|pr| self.pr_review_sync.cached_status(pr));
                    if cached.is_none() && params.get("branchTarget").is_none()
                        && let Some(pr) = &pr
                    {
                        cached = self.pr_review_sync.status(&mast_proto::PrStatusParams {
                            owner: pr.owner.clone(), repo: pr.repo.clone(), number: pr.number, force_refresh: false,
                        }).await.ok();
                    }
                    if cached.as_ref().is_some_and(|status| status.state.is_finished()) {
                        return RpcReply::value(&mast_proto::DiffReviewStateReply { state: None });
                    }
                    let branch = params.get("branchTarget").and_then(serde_json::Value::as_str)
                        .filter(|branch| branch.starts_with("branch:"))
                        .map(str::to_owned)
                        .or_else(|| {
                            let pr = target.strip_prefix("pr:").and_then(mast_proto::PullRequestRef::parse_external_id)?;
                            let status = cached.as_ref()?;
                            let branch = status.head_ref_name.as_deref()?;
                            Some(format!("branch:{}/{}#{branch}", pr.owner.to_lowercase(), pr.repo.to_lowercase()))
                        });
                    if let Some(branch) = branch {
                        state = self.workspace.inherit_diff_review_state(target, &branch)
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                    }
                }
                RpcReply::value(&mast_proto::DiffReviewStateReply { state })
            }
            methods::SET_DIFF_REVIEW_STATE => {
                let update: mast_proto::DiffReviewUpdate = parse_params(params)?;
                if let Some(pr) = update
                    .after
                    .target
                    .strip_prefix("pr:")
                    .and_then(mast_proto::PullRequestRef::parse_external_id)
                {
                    let params = mast_proto::PrStatusParams {
                        owner: pr.owner,
                        repo: pr.repo,
                        number: pr.number,
                        force_refresh: false,
                    };
                    if self
                        .pr_review_sync
                        .status(&params)
                        .await
                        .is_ok_and(|status| status.state.is_finished())
                    {
                        return Err(RpcError::Failed(
                            "This PR is closed; its review state was cleared.".into(),
                        ));
                    }
                }
                RpcReply::value(
                    &self
                        .workspace
                        .update_diff_review_state(&update)
                        .map_err(|error| RpcError::Failed(error.to_string()))?,
                )
            }
            methods::SET_CHAT_DIFF_TARGET => {
                let p: mast_proto::SetChatDiffTargetParams = parse_params(params)?;
                let targets = self.diff_targets.clone();
                let chat_id = p.chat_id.clone();
                let action = p.action;
                let path = p.path.clone();
                let input = p.input.clone();
                let base_branch = p.base_branch.clone();
                let repo_path = p.repo_path.clone();
                let ref_id = p.ref_id.clone();
                let reply = tokio::task::spawn_blocking(move || {
                    let need_path = |path: Option<String>| {
                        path.filter(|path| !path.trim().is_empty()).ok_or_else(|| {
                            crate::diff_targets::DiffTargetError::Invalid(
                                "path is required for select and unbind.".into(),
                            )
                        })
                    };
                    let need_ref_id = |ref_id: Option<String>| {
                        ref_id.filter(|id| !id.trim().is_empty()).ok_or_else(|| {
                            crate::diff_targets::DiffTargetError::Invalid(
                                "refId is required for selectRef and unbindRef.".into(),
                            )
                        })
                    };
                    match action {
                        mast_proto::SetChatDiffTargetAction::BindRef => targets
                            .bind_ref(
                                &chat_id,
                                input.as_deref().unwrap_or(""),
                                base_branch.as_deref(),
                                repo_path.as_deref(),
                            )
                            .map(|outcome| (outcome.reply, None)),
                        mast_proto::SetChatDiffTargetAction::SelectRef => targets
                            .select_ref(&chat_id, &need_ref_id(ref_id)?)
                            .map(|reply| (reply, None)),
                        mast_proto::SetChatDiffTargetAction::UnbindRef => targets
                            .unbind_ref(&chat_id, &need_ref_id(ref_id)?)
                            .map(|reply| (reply, None)),
                        mast_proto::SetChatDiffTargetAction::Select => targets
                            .bind(
                                &chat_id,
                                &need_path(path)?,
                                mast_proto::ChatDiffTargetSource::Manual,
                                None,
                            )
                            .map(|outcome| (outcome.reply, Some(outcome.path))),
                        mast_proto::SetChatDiffTargetAction::Unbind => targets
                            .unbind(&chat_id, &need_path(path)?)
                            .map(|reply| (reply, None)),
                        mast_proto::SetChatDiffTargetAction::ClearSelection => {
                            targets.clear_selection(&chat_id).map(|reply| (reply, None))
                        }
                    }
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .map_err(|e| RpcError::Failed(e.to_string()))?;
                let (reply, bound) = reply;
                if let Some(bound) = bound {
                    // A first turn after `git worktree add` needs a base.
                    self.diff_sync.snapshot_target_now(&p.chat_id, &bound).await;
                }
                RpcReply::value(&reply)
            }
            methods::WATCH_WORKSPACE_GIT_STATUS => {
                let request: mast_proto::WatchWorkspaceFilesRequest = parse_params(params)?;
                let workspace = self.workspace_files.resolve_target(&request.target).await?;
                let rx = self.diff_sync.watch_git_statuses();
                // Only this authorized checkout crosses the connection. None means
                // unavailable, including plain folders and initial/restarting engines.
                let stream = futures::stream::unfold(
                    (rx, workspace.checkout_id, None, false),
                    |(mut rx, checkout_id, mut previous, mut emitted)| async move {
                        loop {
                            if emitted {
                                rx.changed().await.ok()?;
                            }
                            let next = rx
                                .borrow_and_update()
                                .iter()
                                .find(|s| s.checkout_id == checkout_id)
                                .cloned();
                            if !emitted || previous != next {
                                emitted = true;
                                previous = next.clone();
                                let value =
                                    serde_json::to_value(mast_proto::WorkspaceGitStatusFrame {
                                        status: next,
                                    })
                                    .ok()?;
                                return Some((value, (rx, checkout_id, previous, emitted)));
                            }
                        }
                    },
                );
                Ok(RpcReply::Stream(stream.boxed()))
            }
            methods::WATCH_CHECKOUT_CHANGE_REQUEST => {
                let p: CheckoutChangeRequestParams = parse_params(params)?;
                let cwd = self.change_request_root(&p.cwd).await?;
                let stream = self
                    .change_requests
                    .watch_for_branch(&cwd, p.branch.as_deref())
                    .await
                    .map_err(|error| RpcError::Failed(error.to_string()))?
                    .filter_map(|status| async move { serde_json::to_value(status).ok() });
                Ok(RpcReply::Stream(stream.boxed()))
            }
            // One-shot scoped capture for the Changes pane: `branch` diffs the
            // working tree against merge-base(baseRef, HEAD); `turn` diffs the
            // turn-start tree snapshot against the current tree; anything else
            // is the plain working-tree capture.
            methods::RESOLVE_BRANCH_REVIEW_BASE => {
                let p: mast_proto::ResolveBranchReviewBaseParams = parse_params(params)?;
                let identity = self.repos.checkout_identity(std::path::Path::new(&p.checkout_path))
                    .await.map_err(|e| RpcError::Failed(e.to_string()))?;
                let mut reply = crate::diff_sync::resolve_branch_review_base(&identity.root, &p.branch, self.doc_host.device_id())
                    .await.map_err(|e| RpcError::Failed(e.to_string()))?;
                let legacy_id = reply.review_range_id.strip_prefix("review-").unwrap_or(&reply.review_range_id);
                for row in self.workspace.all_chat_diff_targets().map_err(|e| RpcError::Failed(e.to_string()))? {
                    if row.chat_id == p.worker_chat_id || !row.ref_targets.iter().any(|target| target.id == reply.review_range_id || target.id == legacy_id) { continue; }
                    let chat = self.workspace.chat(&row.chat_id).map_err(|e| RpcError::Failed(e.to_string()))?;
                    if chat.is_some_and(|chat| !chat.archived && chat.space_id.as_deref() == Some(&p.space_id)) {
                        reply.existing_chat_id = Some(row.chat_id);
                        break;
                    }
                }
                RpcReply::value(&reply)
            }
            methods::RESOLVE_CODE_REF_SOURCE => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P { cwd: String, checkout_id: Option<String>, #[serde(default)] base_ref: String, #[serde(default)] head_ref: String, path: String, #[serde(default)] head: bool, #[serde(default)] repo_root: Option<String>, #[serde(default)] revision: Option<String> }
                let p: P = parse_params(params)?;
                let identity = self.repos.checkout_identity(std::path::Path::new(&p.cwd)).await.map_err(|e| RpcError::Failed(e.to_string()))?;
                if p.checkout_id.as_deref().is_some_and(|expected| expected != identity.id) {
                    return Err(RpcError::Failed("checkoutId does not match canonical cwd".into()));
                }
                let resolved = match (p.repo_root.as_deref(), p.revision.as_deref()) {
                    (Some(repo_root), Some(revision)) => crate::diff_sync::resolve_nested_code_ref_source(&identity.root, repo_root, revision, &p.path).await,
                    (None, None) => crate::diff_sync::resolve_code_ref_source(&identity.root, &p.base_ref, &p.head_ref, &p.path, p.head).await,
                    _ => return Err(RpcError::Failed("repoRoot and revision must be given together".into())),
                };
                let (revision_sha, text) = resolved.map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "revisionSha": revision_sha, "text": text }))
            }
            methods::GET_CHECKOUT_DIFF => {
                // Keep the scoped-diff future off the dispatcher's stack. The
                // per-commit path adds another nested git-capture future.
                Box::pin(async move {
                    #[derive(Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct P {
                        cwd: String,
                        #[serde(default)]
                        checkout_id: Option<String>,
                        #[serde(default)]
                        mode: String,
                        base_ref: Option<String>,
                        chat_id: Option<String>,
                        commit_sha: Option<String>,
                        head_ref: Option<String>,
                    }
                    let p: P = parse_params(params)?;
                    let identity = self
                        .repos
                        .checkout_identity(std::path::Path::new(&p.cwd))
                        .await
                        .map_err(|e| RpcError::Failed(e.to_string()))?;
                    if let Some(expected) = p.checkout_id.as_deref()
                        && expected != identity.id
                    {
                        return Err(RpcError::Failed(
                            "checkoutId does not match canonical cwd".into(),
                        ));
                    }
                    let root = identity.root.as_path();
                    let snapshot = match p.mode.as_str() {
                        "branch" => {
                            let base_ref = p
                                .base_ref
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("baseRef required".into()))?;
                            let base = crate::diff_sync::merge_base(root, base_ref)
                                .await
                                .map_err(|e| RpcError::Failed(e.to_string()))?;
                            crate::diff_sync::capture_diff_against(&self.repos, root, Some(&base))
                                .await
                        }
                        // One commit's own changes (History → per-commit tab):
                        // parent (or the empty tree) vs the commit itself.
                        "commit" => {
                            let sha = p
                                .commit_sha
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("commitSha required".into()))?;
                            crate::diff_sync::capture_commit_diff(&self.repos, root, sha).await
                        }
                        // Diff branch / PR…: merge-base(baseRef, headRef)...headRef
                        // from refs only.
                        "range" => {
                            let base_ref = p
                                .base_ref
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("baseRef required".into()))?;
                            let head_ref = p
                                .head_ref
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("headRef required".into()))?;
                            crate::diff_sync::capture_range_diff(
                                &self.repos,
                                root,
                                base_ref,
                                head_ref,
                            )
                            .await
                            .map(|(snapshot, _)| snapshot)
                        }
                        "turn" => {
                            let chat_id = p
                                .chat_id
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("chatId required".into()))?;
                            let snapshot = self
                                .diff_sync
                                .turn_snapshot(chat_id, &identity.root)
                                .ok_or_else(|| RpcError::Failed("no turn recorded".into()))?;
                            crate::diff_sync::capture_turn_diff(&self.repos, root, &snapshot.tree)
                                .await
                        }
                        _ => crate::diff_sync::capture_diff(&self.repos, root).await,
                    }
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                    RpcReply::value(&mast_proto::CheckoutDiff {
                        checkout_id: identity.id,
                        device_id: self.doc_host.device_id().to_string(),
                        cwd: identity.root.to_string_lossy().to_string(),
                        patch: snapshot.patch,
                        files: snapshot.files,
                        submodules: snapshot.submodules,
                        additions: snapshot.additions,
                        deletions: snapshot.deletions,
                        truncated: snapshot.truncated,
                        checksum: snapshot.checksum,
                        updated_at: chrono::Utc::now(),
                    })
                })
                .await
            }
            methods::GET_CHECKOUT_FILE_DIFF_TEXT => {
                // This branch contains several large nested async futures. Keep it
                // behind an allocation so every unrelated RPC does not carry that
                // state in `EngineRpc::handle`'s stack frame.
                Box::pin(async move {
                    let p: mast_proto::GetCheckoutFileDiffTextRequest = parse_params(params)?;
                    let identity =
                        Box::pin(self.repos.checkout_identity(std::path::Path::new(&p.cwd)))
                            .await
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                    if identity.id != p.checkout_id {
                        return Err(RpcError::Failed("checkoutId does not match cwd".into()));
                    }
                    let root = identity.root.as_path();
                    let (snapshot, base, target) = match p.mode.as_str() {
                        "branch" => {
                            let base_ref = p
                                .base_ref
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("baseRef required".into()))?;
                            let base = Box::pin(crate::diff_sync::merge_base(root, base_ref))
                                .await
                                .map_err(|error| RpcError::Failed(error.to_string()))?;
                            let snapshot = Box::pin(crate::diff_sync::capture_diff_against(
                                &self.repos,
                                root,
                                Some(&base),
                            ))
                            .await
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                            (snapshot, base, None)
                        }
                        "range" => {
                            let base_ref = p
                                .base_ref
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("baseRef required".into()))?;
                            let head_ref = p
                                .head_ref
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("headRef required".into()))?;
                            let (snapshot, range) = Box::pin(crate::diff_sync::capture_range_diff(
                                &self.repos,
                                root,
                                base_ref,
                                head_ref,
                            ))
                            .await
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                            (snapshot, range.merge_base, Some(range.head))
                        }
                        "commit" => {
                            let sha = p
                                .commit_sha
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("commitSha required".into()))?;
                            let base =
                                Box::pin(crate::diff_sync::commit_diff_base(root, sha)).await;
                            let snapshot = Box::pin(crate::diff_sync::capture_commit_diff(
                                &self.repos,
                                root,
                                sha,
                            ))
                            .await
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                            (snapshot, base, Some(sha.to_string()))
                        }
                        "turn" => {
                            let chat_id = p
                                .chat_id
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("chatId required".into()))?;
                            let turn = self
                                .diff_sync
                                .turn_snapshot(chat_id, &identity.root)
                                .ok_or_else(|| RpcError::Failed("no turn recorded".into()))?;
                            let snapshot = Box::pin(crate::diff_sync::capture_turn_diff(
                                &self.repos,
                                root,
                                &turn.tree,
                            ))
                            .await
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                            (snapshot, turn.tree, None)
                        }
                        _ => {
                            let base = Box::pin(crate::diff_sync::working_diff_base(root))
                                .await
                                .map_err(|error| RpcError::Failed(error.to_string()))?;
                            let snapshot =
                                Box::pin(crate::diff_sync::capture_diff(&self.repos, root))
                                    .await
                                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                            (snapshot, base, None)
                        }
                    };
                    let stale = || mast_proto::CheckoutFileDiffText {
                        diff_checksum: p.diff_checksum.clone(),
                        old_text: None,
                        new_text: None,
                        old_content_hash: None,
                        new_content_hash: None,
                        binary: false,
                        truncated: false,
                        stale: true,
                    };
                    if snapshot.checksum != p.diff_checksum {
                        return RpcReply::value(&stale());
                    }
                    let pair = if let Some(repository_path) = p.repository_path.as_deref() {
                        let section = snapshot
                            .submodules
                            .iter()
                            .find(|section| {
                                section.repository_path == repository_path && section.expanded
                            })
                            .ok_or_else(|| {
                                RpcError::Failed("repository is not part of diff snapshot".into())
                            })?;
                        let file = section
                            .files
                            .iter()
                            .find(|file| file.path == p.path)
                            .ok_or_else(|| {
                                RpcError::Failed("path is not part of diff snapshot".into())
                            })?;
                        Box::pin(crate::diff_sync::read_submodule_diff_file_text(
                            root, section, file,
                        ))
                        .await
                        .map_err(|error| RpcError::Failed(error.to_string()))?
                    } else {
                        let file = snapshot
                            .files
                            .iter()
                            .find(|file| file.path == p.path)
                            .ok_or_else(|| {
                                RpcError::Failed("path is not part of diff snapshot".into())
                            })?;
                        Box::pin(crate::diff_sync::read_diff_file_text_at(
                            root,
                            &base,
                            target.as_deref(),
                            file,
                        ))
                        .await
                        .map_err(|error| RpcError::Failed(error.to_string()))?
                    };
                    let current = match p.mode.as_str() {
                        "branch" => {
                            Box::pin(crate::diff_sync::capture_diff_against(
                                &self.repos,
                                root,
                                Some(&base),
                            ))
                            .await
                        }
                        "turn" => {
                            Box::pin(crate::diff_sync::capture_turn_diff(
                                &self.repos,
                                root,
                                &base,
                            ))
                            .await
                        }
                        "commit" => {
                            let sha = p
                                .commit_sha
                                .as_deref()
                                .ok_or_else(|| RpcError::Failed("commitSha required".into()))?;
                            Box::pin(crate::diff_sync::capture_commit_diff(
                                &self.repos,
                                root,
                                sha,
                            ))
                            .await
                        }
                        // Already captured above (the pair is immutable refs):
                        // re-resolve to detect a ref that moved meanwhile.
                        "range" => {
                            let base_ref = p.base_ref.as_deref().unwrap_or_default();
                            let head_ref = p.head_ref.as_deref().unwrap_or_default();
                            Box::pin(crate::diff_sync::capture_range_diff(
                                &self.repos,
                                root,
                                base_ref,
                                head_ref,
                            ))
                            .await
                            .map(|(snapshot, _)| snapshot)
                        }
                        _ => Box::pin(crate::diff_sync::capture_diff(&self.repos, root)).await,
                    }
                    .map_err(|error| RpcError::Failed(error.to_string()))?;
                    if current.checksum != p.diff_checksum {
                        return RpcReply::value(&stale());
                    }
                    RpcReply::value(&mast_proto::CheckoutFileDiffText {
                        diff_checksum: p.diff_checksum,
                        old_text: pair.old_text,
                        new_text: pair.new_text,
                        old_content_hash: pair.old_content_hash,
                        new_content_hash: pair.new_content_hash,
                        binary: pair.binary,
                        truncated: pair.truncated,
                        stale: false,
                    })
                })
                .await
            }
            methods::LIST_REPOS => RpcReply::value(&self.repos.list().await),
            methods::ADD_REPO => {
                #[derive(Deserialize)]
                struct P {
                    path: String,
                }
                let p: P = parse_params(params)?;
                let repo = self
                    .repos
                    .add(&p.path)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&repo)
            }
            methods::CLONE_REPO => {
                #[derive(Deserialize)]
                struct P {
                    url: String,
                }
                let p: P = parse_params(params)?;
                let repo = self
                    .repos
                    .clone_repo(&p.url)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&repo)
            }
            methods::CREATE_REPO => {
                #[derive(Deserialize)]
                struct P {
                    name: String,
                }
                let p: P = parse_params(params)?;
                let repo = self
                    .repos
                    .create(&p.name)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&repo)
            }
            methods::LIST_BRANCHES => {
                let p: RepoPathParams = parse_params(params)?;
                let branches = self
                    .repos
                    .branches(std::path::Path::new(&p.repo_path))
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&branches)
            }
            methods::LIST_REFS => {
                let p: RepoPathParams = parse_params(params)?;
                let refs = self
                    .repos
                    .refs(std::path::Path::new(&p.repo_path))
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&refs)
            }
            methods::LIST_GIT_HISTORY => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    cwd: String,
                    #[serde(default)]
                    cursor: usize,
                    #[serde(default = "default_git_history_limit")]
                    limit: usize,
                }
                fn default_git_history_limit() -> usize {
                    crate::repos::GIT_HISTORY_DEFAULT_LIMIT
                }
                let p: P = parse_params(params)?;
                let history = self
                    .repos
                    .history(std::path::Path::new(&p.cwd), p.cursor, p.limit)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&history)
            }
            methods::SEARCH_GIT_HISTORY => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    cwd: String,
                    query: String,
                    #[serde(default)]
                    cursor: usize,
                    #[serde(default = "default_git_history_search_limit")]
                    limit: usize,
                }
                fn default_git_history_search_limit() -> usize {
                    crate::repos::GIT_HISTORY_DEFAULT_LIMIT
                }
                let p: P = parse_params(params)?;
                let history = self
                    .repos
                    .search_history(std::path::Path::new(&p.cwd), &p.query, p.cursor, p.limit)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&history)
            }
            methods::RESOLVE_GIT_AVATARS => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P {
                    cwd: String,
                    authors: Vec<GitAvatarAuthor>,
                    #[serde(default)]
                    cursor: usize,
                    #[serde(default = "default_git_avatar_limit")]
                    limit: usize,
                }
                #[derive(Deserialize)]
                struct GitAvatarAuthor {
                    sha: String,
                    email: String,
                }
                fn default_git_avatar_limit() -> usize {
                    crate::repos::GIT_HISTORY_DEFAULT_LIMIT
                }
                let p: P = parse_params(params)?;
                let authors: Vec<_> = p
                    .authors
                    .into_iter()
                    .take(crate::repos::GIT_HISTORY_MAX_LIMIT)
                    .filter(|author| author.sha.len() <= 64 && author.email.len() <= 512)
                    .map(|author| (author.sha, author.email))
                    .collect();
                let avatar_paths = self
                    .repos
                    .history_avatar_urls(std::path::Path::new(&p.cwd), &authors, p.cursor, p.limit)
                    .await;
                let mut avatars = std::collections::HashMap::new();
                for (email, path) in avatar_paths {
                    if let Ok(bytes) = tokio::fs::read(path).await {
                        avatars.insert(
                            email,
                            base64::engine::general_purpose::STANDARD.encode(bytes),
                        );
                    }
                }
                RpcReply::value(&avatars)
            }
            methods::FETCH_ALL => {
                let p: RepoPathParams = parse_params(params)?;
                self.repos
                    .fetch_all(std::path::Path::new(&p.repo_path))
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                // Remote refs are repository state too. Force the checkout
                // watchers to publish a fresh snapshot instead of waiting for
                // the repair tick (some platforms do not report packed-refs).
                self.diff_sync.sync_all();
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::SWITCH_REF => {
                let p: SwitchRefParams = parse_params(params)?;
                let branch = self
                    .repos
                    .switch_ref(std::path::Path::new(&p.repo_path), &p.ref_name)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "branch": branch }))
            }
            methods::LIST_FOLDERS => {
                let p: ListFoldersParams = parse_params(params)?;
                let listing = self
                    .repos
                    .list_folders(p.path)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&listing)
            }
            methods::LIST_DRIVES => {
                let drives = self
                    .repos
                    .list_drives()
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&mast_proto::DriveListing { drives })
            }
            methods::SEARCH_FILES => {
                let p: FileSearchParams = parse_params(params)?;
                if p.query.chars().count() > 256 {
                    return Err(RpcError::BadParams(
                        "SearchFiles query must not exceed 256 characters".into(),
                    ));
                }
                let matches = tokio::time::timeout(FILE_SEARCH_RPC_TIMEOUT, async {
                    let root = self.file_search_root(&p).await?;
                    let featured_paths = p
                        .chat_id
                        .as_deref()
                        .filter(|_| p.query.is_empty())
                        .map(|chat_id| self.featured_file_paths(chat_id))
                        .unwrap_or_default();
                    self.repos
                        .search_files(root, p.query, featured_paths)
                        .await
                        .map_err(|e| RpcError::Failed(e.to_string()))
                })
                .await
                .map_err(|_| RpcError::Failed("file search timed out".into()))??;
                RpcReply::value(&matches)
            }
            methods::LIST_WORKSPACE_DIRECTORY => {
                let request: mast_proto::ListWorkspaceDirectoryRequest = parse_params(params)?;
                let page = tokio::time::timeout(
                    crate::workspace_files::WORKSPACE_FILE_RPC_TIMEOUT,
                    self.workspace_files.list_directory(request),
                )
                .await
                .map_err(|_| RpcError::Failed("workspace directory listing timed out".into()))?
                .map_err(RpcError::from)?;
                RpcReply::value(&page)
            }
            methods::WATCH_WORKSPACE_SEARCH_FILES => {
                let request: mast_proto::WatchWorkspaceFilesRequest = parse_params(params)?;
                let index = self.workspace_files
                    .subscribe_search_files(request.target)
                    .await
                    .map_err(RpcError::from)?;
                let stream = futures::stream::unfold((index, true), |(mut index, mut first)| async move {
                    loop {
                        if !first && index.changed().await.is_err() { return None; }
                        first = false;
                        let value = index.borrow_and_update().clone();
                        if let Some(value) = value {
                            let frame = match value {
                                Ok(files) => serde_json::json!({"files": files}),
                                Err(error) => serde_json::json!({"error": error}),
                            };
                            return Some((frame, (index, false)));
                        }
                    }
                });
                Ok(RpcReply::Stream(stream.boxed()))
            }
            methods::SEARCH_WORKSPACE_FILES => {
                let request: mast_proto::SearchWorkspaceFilesRequest = parse_params(params)?;
                let matches = tokio::time::timeout(
                    crate::workspace_files::WORKSPACE_FILE_RPC_TIMEOUT,
                    self.workspace_files.search(request),
                )
                .await
                .map_err(|_| RpcError::Failed("workspace file search timed out".into()))?
                .map_err(RpcError::from)?;
                RpcReply::value(&matches)
            }
            methods::SEARCH_WORKSPACE_CONTENT => {
                let request: mast_proto::SearchWorkspaceContentRequest = parse_params(params)?;
                if request.query.chars().count() > 256
                    || request.include.len() + request.exclude.len() > 100
                {
                    return Err(RpcError::BadParams(
                        "Content search query or glob list is too large".into(),
                    ));
                }
                let workspace = self.workspace_files.resolve_target(&request.target).await?;
                let indexed_paths = self.workspace_files
                    .list_search_files(request.target.clone())
                    .await
                    .map_err(RpcError::from)?;
                let changed_files = request
                    .changed_files
                    .map(|paths| {
                        paths
                            .into_iter()
                            .map(|path| {
                                crate::workspace_files::WorkspaceRelativePath::file(&path)
                                    .map(|path| path.as_path().to_path_buf())
                            })
                            .collect::<Result<std::collections::HashSet<_>, _>>()
                    })
                    .transpose()
                    .map_err(RpcError::from)?;
                let query = crate::workspace_content_search::ContentSearchQuery {
                    text: request.query,
                    case_sensitive: request.case_sensitive,
                    whole_word: request.whole_word,
                    regex: request.regex,
                    include: request.include,
                    exclude: request.exclude,
                    changed_files,
                };
                let (sender, receiver) = tokio::sync::mpsc::channel(32);
                let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let worker_cancelled = cancelled.clone();
                tokio::task::spawn_blocking(move || {
                    use mast_proto::WorkspaceContentSearchFrame as Frame;
                    let progress_sender = sender.clone();
                    let result = crate::workspace_content_search::search_indexed_content(
                        &workspace.root,
                        &indexed_paths,
                        &query,
                        &worker_cancelled,
                        |item| {
                            sender
                                .blocking_send(Frame::Match {
                                    path: item.path.to_string_lossy().into_owned(),
                                    line: item.line,
                                    preview: item.preview,
                                    ranges: item.ranges,
                                })
                                .is_ok()
                        },
                        |files| { let _ = progress_sender.blocking_send(Frame::Progress { files }); },
                    );
                    let frame = match result {
                        Ok(summary) => Frame::Complete {
                            matches: summary.matches,
                            files: summary.files,
                            skipped_files: summary.skipped_files,
                            truncated: summary.truncated,
                        },
                        Err(message) => Frame::Error { message },
                    };
                    let _ = sender.blocking_send(frame);
                });
                struct CancelOnStreamDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);
                impl Drop for CancelOnStreamDrop {
                    fn drop(&mut self) {
                        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                let stream = futures::stream::unfold(
                    (receiver, CancelOnStreamDrop(cancelled)),
                    |(mut receiver, guard)| async move {
                        let frame = receiver.recv().await?;
                        Some((serde_json::to_value(frame).ok()?, (receiver, guard)))
                    },
                );
                Ok(RpcReply::Stream(stream.boxed()))
            }
            methods::READ_WORKSPACE_IMAGE => {
                let request: mast_proto::ReadWorkspaceImageRequest = parse_params(params)?;
                let chunk = tokio::time::timeout(
                    crate::workspace_files::WORKSPACE_FILE_RPC_TIMEOUT,
                    self.workspace_files.read_image(request),
                )
                .await
                .map_err(|_| RpcError::Failed("Workspace image read timed out".into()))?
                .map_err(RpcError::from)?;
                RpcReply::value(&chunk)
            }
            methods::READ_WORKSPACE_FILE => {
                let request: mast_proto::ReadWorkspaceFileRequest = parse_params(params)?;
                let file = tokio::time::timeout(
                    crate::workspace_files::WORKSPACE_FILE_RPC_TIMEOUT,
                    self.workspace_files.read_file(request),
                )
                .await
                .map_err(|_| RpcError::Failed("workspace file read timed out".into()))?
                .map_err(RpcError::from)?;
                RpcReply::value(&file)
            }
            methods::WRITE_WORKSPACE_FILE => {
                let request: mast_proto::WriteWorkspaceFileRequest = parse_params(params)?;
                let outcome = tokio::time::timeout(
                    crate::workspace_files::WORKSPACE_FILE_RPC_TIMEOUT,
                    self.workspace_files.write_file(request),
                )
                .await
                .map_err(|_| RpcError::Failed("workspace file write timed out".into()))?
                .map_err(RpcError::from)?;
                RpcReply::value(&outcome)
            }
            methods::WATCH_WORKSPACE_FILES => {
                let request: mast_proto::WatchWorkspaceFilesRequest = parse_params(params)?;
                let subscription = self
                    .workspace_files
                    .watch_files(request)
                    .await
                    .map_err(RpcError::from)?;
                let stream = futures::stream::unfold(subscription, |mut subscription| async move {
                    let changes = subscription.recv().await?;
                    let value = serde_json::to_value(changes).ok()?;
                    Some((value, subscription))
                });
                Ok(RpcReply::Stream(stream.boxed()))
            }
            methods::CREATE_WORKTREE => {
                let p: CreateWorktreeParams = parse_params(params)?;
                let setup_space = match p.space_id.as_deref() {
                    Some(space_id) => {
                        let space = self.local_project_action_space(space_id)?;
                        let space_root = std::fs::canonicalize(&space.path)
                            .map_err(|_| RpcError::Failed("Project root is unavailable".into()))?;
                        let repo_root = std::fs::canonicalize(&p.repo_path).map_err(|_| {
                            RpcError::Failed("Worktree repository is unavailable".into())
                        })?;
                        if space_root != repo_root {
                            return Err(RpcError::Failed(
                                "Worktree repository does not match project space".into(),
                            ));
                        }
                        Some((space, space_root))
                    }
                    None => None,
                };
                let worktree = self
                    .repos
                    .create_worktree(std::path::Path::new(&p.repo_path), &p.branch)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let mut outcome = CreateWorktreeOutcome {
                    worktree,
                    setup_action: None,
                    setup_error: None,
                };
                if let Some((space, project_root)) = setup_space {
                    match self
                        .project_actions
                        .setup_action(&space.id, std::path::Path::new(&space.path))
                    {
                        Ok(Some(action)) => {
                            let worktree_root = std::fs::canonicalize(&outcome.worktree.path)
                                .unwrap_or_else(|_| outcome.worktree.path.clone().into());
                            match crate::project_actions::launch_project_setup_action(
                                &self.terminals,
                                &action,
                                &project_root,
                                &worktree_root,
                                80,
                                24,
                            ) {
                                Ok(run) => outcome.setup_action = Some(run),
                                Err(err) => {
                                    tracing::warn!(
                                        space_id = %space.id,
                                        worktree = %outcome.worktree.path,
                                        error = %err,
                                        "failed to start project setup Action"
                                    );
                                    outcome.setup_error = Some(err.to_string());
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            tracing::warn!(
                                space_id = %space.id,
                                error = %err,
                                "failed to resolve project setup Action"
                            );
                            outcome.setup_error = Some(err.to_string());
                        }
                    }
                }
                RpcReply::value(&outcome)
            }
            methods::DELETE_WORKTREE => {
                let p: DeleteWorktreeParams = parse_params(params)?;
                self.repos
                    .delete_worktree(
                        std::path::Path::new(&p.repo_path),
                        std::path::Path::new(&p.worktree_path),
                    )
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::LIST_PROJECT_ACTIONS => {
                let p: ListProjectActionsParams = parse_params(params)?;
                let space = self.local_project_action_space(&p.space_id)?;
                let actions = self.project_actions.clone();
                // Snapshots discover repository files; keep all filesystem work
                // (including mutation persistence below) off the async worker.
                let snapshot = tokio::task::spawn_blocking(move || {
                    actions.snapshot(&space.id, std::path::Path::new(&space.path))
                })
                .await
                .map_err(|err| RpcError::Failed(err.to_string()))?
                .map_err(|err| RpcError::Failed(err.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::UPSERT_PROJECT_ACTION => {
                let p: UpsertProjectActionParams = parse_params(params)?;
                let space = self.local_project_action_space(&p.space_id)?;
                let actions = self.project_actions.clone();
                let snapshot = tokio::task::spawn_blocking(move || {
                    actions.upsert(
                        &space.id,
                        std::path::Path::new(&space.path),
                        p.action_id.as_deref(),
                        p.action,
                    )
                })
                .await
                .map_err(|err| RpcError::Failed(err.to_string()))?
                .map_err(|err| RpcError::Failed(err.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::DELETE_PROJECT_ACTION => {
                let p: DeleteProjectActionParams = parse_params(params)?;
                let space = self.local_project_action_space(&p.space_id)?;
                let actions = self.project_actions.clone();
                let snapshot = tokio::task::spawn_blocking(move || {
                    actions.delete(&space.id, std::path::Path::new(&space.path), &p.action_id)
                })
                .await
                .map_err(|err| RpcError::Failed(err.to_string()))?
                .map_err(|err| RpcError::Failed(err.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::RUN_PROJECT_ACTION => {
                let p: RunProjectActionParams = parse_params(params)?;
                let space = self.local_project_action_space(&p.space_id)?;
                let chat = self
                    .workspace
                    .chat(&p.chat_id)
                    .map_err(|err| RpcError::Failed(err.to_string()))?
                    .ok_or_else(|| RpcError::Failed("Project chat not found".into()))?;
                if chat.device_id != self.doc_host.device_id() {
                    return Err(RpcError::Failed(
                        "Project chat belongs to another device".into(),
                    ));
                }
                if chat.space_id.as_deref() != Some(space.id.as_str()) {
                    return Err(RpcError::Failed(
                        "Project chat belongs to another space".into(),
                    ));
                }
                let cwd = chat
                    .cwd
                    .map(std::path::PathBuf::from)
                    .ok_or_else(|| RpcError::Failed("Project chat has no checkout".into()))?;
                let checkout = self
                    .repos
                    .workspace_checkout(std::path::Path::new(&space.path), &cwd)
                    .await
                    .ok_or_else(|| {
                        RpcError::Failed("Project chat checkout is unavailable".into())
                    })?;
                let project_root = std::fs::canonicalize(&space.path)
                    .map_err(|_| RpcError::Failed("Project root is unavailable".into()))?;
                let action = self
                    .project_actions
                    .action(&space.id, std::path::Path::new(&space.path), &p.action_id)
                    .map_err(|err| RpcError::Failed(err.to_string()))?
                    .ok_or_else(|| RpcError::Failed("Project action not found".into()))?;
                let run = crate::project_actions::launch_project_action(
                    &self.terminals,
                    &action,
                    &project_root,
                    &checkout,
                    p.cols,
                    p.rows,
                )
                .map_err(|err| RpcError::Failed(err.to_string()))?;
                RpcReply::value(&run)
            }
            methods::OPEN_TERMINAL => {
                let p: OpenTerminalParams = parse_params(params)?;
                // The terminal runs in the chat's checkout. Project-less (`~`)
                // and not-yet-materialized chats use the same Mast-owned
                // per-chat scratch as agent runs, never the person's HOME.
                let stored_cwd = self
                    .workspace
                    .chat(&p.chat_id)
                    .ok()
                    .flatten()
                    .and_then(|chat| chat.cwd);
                let cwd = self
                    .sessions
                    .resolve_cwd(&p.chat_id, stored_cwd.as_deref().unwrap_or("~"))
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let session = self
                    .terminals
                    .open(&cwd, p.cols, p.rows)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&session)
            }
            methods::SUBSCRIBE_TERMINAL => {
                let p: SubscribeTerminalParams = parse_params(params)?;
                let rx = self
                    .terminals
                    .subscribe(&p.terminal_id, p.after_seq)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let stream = futures::stream::unfold(rx, |mut rx| async move {
                    let event = rx.recv().await?;
                    let value = serde_json::to_value(&event).ok()?;
                    Some((value, rx))
                });
                Ok(RpcReply::Stream(stream.boxed()))
            }
            methods::WRITE_TERMINAL => {
                let p: WriteTerminalParams = parse_params(params)?;
                self.terminals
                    .write(&p.terminal_id, &p.data)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::RESIZE_TERMINAL => {
                let p: ResizeTerminalParams = parse_params(params)?;
                self.terminals
                    .resize(&p.terminal_id, p.cols, p.rows)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::CLOSE_TERMINAL => {
                let p: TerminalIdParams = parse_params(params)?;
                self.terminals
                    .close(&p.terminal_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::LOCAL_AGENT_SIGN_IN_STATUS => {
                let p: LocalAgentSignInStatusParams = parse_params(params)?;
                let status = if let Some(id) = p.account_id {
                    use crate::agent_accounts::LocalSignInStatus;
                    self.managed_accounts.as_ref()
                        .and_then(|accounts| accounts.snapshot().accounts.into_iter()
                            .find(|account| account.id == id && account.harness == p.harness))
                        .map_or(LocalSignInStatus::Unknown, |account| {
                            if account.signed_in { LocalSignInStatus::SignedIn } else { LocalSignInStatus::SignedOut }
                        })
                } else {
                    self.agent_accounts.local_sign_in_status(p.harness).await
                };
                RpcReply::value(&status)
            }
            methods::LIST_AGENT_ACCOUNTS => {
                let p: ListAgentAccountsParams = parse_params(params)?;
                let snapshot = self
                    .agent_accounts
                    .list(p.force_usage.unwrap_or(false))
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::ACTIVATE_AGENT_ACCOUNT => {
                let p: AgentAccountParams = parse_params(params)?;
                let snapshot = self
                    .agent_accounts
                    .activate(p.harness, &p.account_id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::FORGET_AGENT_ACCOUNT => {
                let p: AgentAccountParams = parse_params(params)?;
                let snapshot = self
                    .agent_accounts
                    .forget(p.harness, &p.account_id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::START_AGENT_LOGIN => {
                let p: StartAgentLoginParams = parse_params(params)?;
                let start = self
                    .agent_accounts
                    .start_login(p.harness)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&start)
            }
            methods::COMPLETE_AGENT_LOGIN => {
                let p: CompleteAgentLoginParams = parse_params(params)?;
                let snapshot = self
                    .agent_accounts
                    .complete_login(&p.login_id, &p.code)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&snapshot)
            }
            methods::POLL_AGENT_LOGIN => {
                let p: LoginIdParams = parse_params(params)?;
                let poll = self
                    .agent_accounts
                    .poll_login(&p.login_id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&poll)
            }
            methods::CANCEL_AGENT_LOGIN => {
                let p: LoginIdParams = parse_params(params)?;
                self.agent_accounts.cancel_login(&p.login_id);
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::LIST_MANAGED_ACCOUNTS => {
                let p: ListAgentAccountsParams = parse_params(params)?;
                RpcReply::value(
                    &self
                        .managed_snapshot(p.force_usage.unwrap_or(false))
                        .await?,
                )
            }
            methods::CREATE_MANAGED_ACCOUNT => {
                let p: CreateManagedAccountParams = parse_params(params)?;
                let account = self
                    .managed()?
                    .create(p.harness, &p.label)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&account)
            }
            methods::REMOVE_MANAGED_ACCOUNT => {
                let p: ManagedAccountIdParams = parse_params(params)?;
                let accounts = self.managed()?;
                accounts
                    .remove(&p.account_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&self.managed_snapshot(false).await?)
            }
            methods::START_MANAGED_ACCOUNT_LOGIN => {
                let p: ManagedAccountIdParams = parse_params(params)?;
                let (login_id, url) = self
                    .managed()?
                    .start_login(&p.account_id)
                    .await
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&mast_proto::AgentLoginStart {
                    login_id,
                    url,
                    mode: mast_proto::AgentLoginMode::Browser,
                })
            }
            methods::POLL_MANAGED_ACCOUNT_LOGIN => {
                let p: LoginIdParams = parse_params(params)?;
                RpcReply::value(&self.managed()?.poll_login(&p.login_id))
            }
            methods::CANCEL_MANAGED_ACCOUNT_LOGIN => {
                let p: LoginIdParams = parse_params(params)?;
                self.managed()?.cancel_login(&p.login_id);
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::SUBMIT_MANAGED_ACCOUNT_LOGIN_CODE => {
                let p: SubmitLoginCodeParams = parse_params(params)?;
                self.managed()?
                    .submit_login_code(&p.login_id, &p.code)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::PROMOTE_SAVED_ACCOUNT => {
                let p: PromoteSavedAccountParams = parse_params(params)?;
                let accounts = self.managed()?;
                let slot = self
                    .agent_accounts
                    .export_for_promotion(p.harness, &p.slot_id)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                let account = accounts
                    .promote(p.harness, "", &slot)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                if let Err(err) =
                    self.agent_accounts
                        .mark_promoted(p.harness, &p.slot_id, &account.id)
                {
                    // Never leave one login in two places: undo the copy.
                    let _ = accounts.remove(&account.id);
                    return Err(RpcError::Failed(err.to_string()));
                }
                if let Some(space_id) = &p.space_id {
                    accounts
                        .set_project_account(space_id, p.harness, Some(&account.id))
                        .map_err(|e| RpcError::Failed(e.to_string()))?;
                }
                RpcReply::value(&self.managed_snapshot(false).await?)
            }
            methods::SET_PROJECT_ACCOUNT => {
                let p: SetProjectAccountParams = parse_params(params)?;
                let accounts = self.managed()?;
                accounts
                    .set_project_account(&p.space_id, p.harness, p.account_id.as_deref())
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&self.managed_snapshot(false).await?)
            }
            methods::UPLOAD_CHUNK => {
                let p: UploadChunkParams = parse_params(params)?;
                self.uploads
                    .append(&p.upload_id, &p.data, p.seq)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&serde_json::json!({ "ok": true }))
            }
            methods::UPLOAD_COMMIT => {
                let p: UploadCommitParams = parse_params(params)?;
                let path = self
                    .uploads
                    .commit(&p.upload_id, &p.file_name)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                // Bytes just landed on this device: any command deferred on
                // them (queued-attachment refs) is executable NOW.
                self.doc_host.kick_drains();
                RpcReply::value(&serde_json::json!({ "path": path }))
            }
            methods::READ_ATTACHMENT_CHUNK => {
                let p: ReadAttachmentChunkParams = parse_params(params)?;
                // Path jail: the uploads dir plus every workspace-known chat cwd.
                let roots: Vec<std::path::PathBuf> = self
                    .workspace
                    .read_chats()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|chat| chat.cwd)
                    .map(std::path::PathBuf::from)
                    // A historical chat rooted at HOME must not widen the
                    // attachment-read jail to every personal file on disk.
                    .filter(|root| !crate::repos::is_automatic_access_blocked(root))
                    .collect();
                let chunk = self
                    .uploads
                    .read_chunk(&p.path, p.offset, &roots)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                RpcReply::value(&chunk)
            }
            methods::FETCH_TOOL_BLOB => {
                let p: FetchToolBlobParams = parse_params(params)?;
                let key = crate::doc_host::parse_tool_blob_ref(&p.blob_ref)
                    .map_err(|e| RpcError::Failed(e.to_string()))?;
                // Local cache first. For pre-cache chats, recover the exact
                // ToolResult from the durable run journal before touching the
                // network; this is what makes the visible "tap to retry"
                // affordance repair already-recorded edits too.
                let text = match self
                    .doc_host
                    .cached_tool_blob(&p.blob_ref)
                    .map_err(|e| RpcError::Failed(e.to_string()))?
                {
                    Some(text) => text,
                    None => match self
                        .sessions
                        .tool_blob_from_journal(key.chat_id, key.part_id, key.is_diff)
                        .map_err(|e| RpcError::Failed(e.to_string()))?
                    {
                        Some(text) => {
                            if let Err(err) = self.doc_host.cache_tool_blob(&p.blob_ref, &text) {
                                tracing::warn!(blob_ref = %p.blob_ref, error = %err,
                                    "journal tool blob cache backfill failed");
                            }
                            text
                        }
                        None => self
                            .doc_host
                            .fetch_tool_blob(&p.blob_ref)
                            .await
                            .map_err(|e| RpcError::Failed(e.to_string()))?,
                    },
                };
                RpcReply::value(&serde_json::json!({ "text": text }))
            }
            other => Err(RpcError::UnknownMethod(other.to_string())),
        }
    }
}

#[allow(clippy::disallowed_methods)]

#[cfg(test)]
mod tests;

#[cfg(test)]
mod context_usage_tests;
