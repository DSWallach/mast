use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChatParams {
    pub(super) chat_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RetryErroredTurnParams {
    pub(super) chat_id: String,
    pub(super) error_entry_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListModelsParams {
    pub(super) harness: HarnessId,
    #[serde(default)]
    pub(super) force: bool,
    /// `ListCommands` only: the calling chat's cwd, so project-scoped
    /// commands/skills for THAT chat's repo/worktree are discovered (a
    /// harness that doesn't discover per-cwd, e.g. codex/opencode/ACP
    /// agents, ignores it). Absent/empty for `ListModels`, which doesn't
    /// need one, and for a chat with no cwd yet (a blank new-chat screen).
    #[serde(default)]
    pub(super) cwd: String,
    /// `ListCommands` only: identifies the project-less chat whose stable
    /// scratch directory should be used when cwd is `~` or a legacy HOME.
    #[serde(default)]
    pub(super) chat_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SetHarnessEnabledParams {
    pub(super) harness: HarnessId,
    pub(super) enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QueueCommandParams {
    pub(super) chat_id: String,
    pub(super) command: SessionCommandPayload,
    #[serde(default)]
    pub(super) command_id: Option<String>,
    /// Queued attachments (bytes already committed locally as `pending://`
    /// refs) the engine delivers to a remote host AFTER the command is
    /// durably queued — never as a gate in front of it.
    #[serde(default)]
    pub(super) transfers: Vec<crate::uploads::AttachmentTransfer>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RelayCommandParams {
    pub(super) chat_id: String,
    /// The full command entry, client-minted id included — the exactly-once
    /// key the host claims in its processed ledger before executing.
    pub(super) entry: mast_doc::SessionCommandEntry,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TakeProjectActionSetupParams {
    pub(super) chat_id: String,
    pub(super) command_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ImportExternalSessionParams {
    pub(super) chat_id: String,
    pub(super) external_session_id: String,
    /// The scanned candidate's own `path` field, round-tripped back verbatim
    /// (see `ExternalSessionCandidate::path`'s doc comment for why).
    pub(super) path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckSessionLivenessParams {
    pub(super) path: String,
    pub(super) session_id: String,
    pub(super) cwd: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SyncExternalSessionParams {
    pub(super) chat_id: String,
}

/// `READ_SUBAGENT_TRANSCRIPT` request: `{chatId, agentId}` — never a path;
/// see `methods::READ_SUBAGENT_TRANSCRIPT`'s doc comment for why.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReadSubagentTranscriptParams {
    pub(super) chat_id: String,
    pub(super) agent_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChatLinkStatusParams {
    pub(super) chat_id: String,
}

/// `SET_CHAT_LINK` request: `{chatId, kind: "pr" | "ticket", value: string |
/// null, operation?: "add" | "remove" | "clear"}`. Missing `operation`
/// preserves the original contract: a value adds/sets, null clears. PR
/// removal names the exact URL so one linked PR can be removed without
/// disturbing its siblings.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SetChatLinkParams {
    pub(super) chat_id: String,
    pub(super) kind: String,
    #[serde(default)]
    pub(super) value: Option<String>,
    #[serde(default)]
    pub(super) operation: Option<String>,
}

/// `PLAN_CHAT_WORKSPACE` request: `{message, cwd}`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlanChatWorkspaceParams {
    pub(super) message: String,
    pub(super) cwd: String,
}

/// `CREATE_CHAT_WORKTREE` request: `{chatId, repoPath, name}`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateChatWorktreeParams {
    pub(super) chat_id: String,
    pub(super) repo_path: String,
    pub(super) name: String,
}

/// `PLAN_CHAT_CLOSEOUT` request: `{chatId?, cwd}`. Repo Map omits `chatId`
/// when closing an unmatched registered worktree.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlanChatCloseoutParams {
    pub(super) chat_id: Option<String>,
    pub(super) cwd: String,
}

/// `CLOSE_CHAT_WORKTREE` request: `{chatId?, cwd, force}`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CloseChatWorktreeParams {
    pub(super) chat_id: Option<String>,
    pub(super) cwd: String,
    #[serde(default)]
    pub(super) force: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QueueMessageParams {
    pub(super) chat_id: String,
    pub(super) text: String,
    #[serde(default)]
    pub(super) message_id: Option<String>,
    #[serde(default)]
    pub(super) attachments: Vec<String>,
    /// Keep this row visible during the current turn even when the harness
    /// supports mid-turn steering.
    #[serde(default)]
    pub(super) hold_for_turn_end: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QueuedMessageParams {
    pub(super) chat_id: String,
    pub(super) id: String,
    /// Present for UpdateQueuedMessage only; empty text deletes the row.
    #[serde(default)]
    pub(super) text: String,
    /// Present for MoveQueuedMessage only.
    #[serde(default)]
    pub(super) to_index: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BeginQueuedMessageEditParams {
    pub(super) chat_id: String,
    pub(super) id: String,
    pub(super) editor_device_id: String,
    pub(super) editor_instance_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RenewQueuedMessageEditParams {
    pub(super) chat_id: String,
    pub(super) id: String,
    pub(super) lease_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum FinishQueuedMessageEditAction {
    Commit,
    Cancel,
    Discard,
    ReleaseUnchanged,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FinishQueuedMessageEditParams {
    pub(super) chat_id: String,
    pub(super) id: String,
    pub(super) lease_id: String,
    pub(super) action: FinishQueuedMessageEditAction,
    #[serde(default)]
    pub(super) text: Option<String>,
    #[serde(default)]
    pub(super) expected_text_hash: Option<String>,
    #[serde(default)]
    pub(super) attachments: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RepoPathParams {
    /// `repoPath` per §3.5 (the §2.1 shorthand `repo` is accepted as an alias).
    #[serde(alias = "repo")]
    pub(super) repo_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckoutChangeRequestParams {
    pub(super) cwd: String,
    #[serde(default)]
    pub(super) branch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SwitchRefParams {
    /// The checkout to switch — a session's cwd (main folder or worktree).
    pub(super) repo_path: String,
    pub(super) ref_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateWorktreeParams {
    #[serde(alias = "repo")]
    pub(super) repo_path: String,
    pub(super) branch: String,
    #[serde(default)]
    pub(super) space_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeleteWorktreeParams {
    #[serde(alias = "repo")]
    pub(super) repo_path: String,
    #[serde(alias = "path")]
    pub(super) worktree_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListProjectActionsParams {
    pub(super) space_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UpsertProjectActionParams {
    pub(super) space_id: String,
    #[serde(default)]
    pub(super) action_id: Option<String>,
    pub(super) action: ProjectActionDraft,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeleteProjectActionParams {
    pub(super) space_id: String,
    pub(super) action_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RunProjectActionParams {
    pub(super) space_id: String,
    pub(super) chat_id: String,
    pub(super) action_id: String,
    pub(super) cols: u16,
    pub(super) rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListFoldersParams {
    #[serde(default)]
    pub(super) path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileSearchParams {
    pub(super) query: String,
    #[serde(default)]
    pub(super) chat_id: Option<String>,
    #[serde(default)]
    pub(super) space_id: Option<String>,
    /// Existing linked worktree selected for a new chat. The engine accepts it
    /// only after verifying it against the space repository's worktree list.
    #[serde(default)]
    pub(super) path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OpenTerminalParams {
    pub(super) chat_id: String,
    pub(super) cols: u16,
    pub(super) rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TerminalIdParams {
    pub(super) terminal_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SubscribeTerminalParams {
    pub(super) terminal_id: String,
    #[serde(default)]
    pub(super) after_seq: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WriteTerminalParams {
    pub(super) terminal_id: String,
    /// Base64 input bytes (plain UTF-8 accepted leniently).
    pub(super) data: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ResizeTerminalParams {
    pub(super) terminal_id: String,
    pub(super) cols: u16,
    pub(super) rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListAgentAccountsParams {
    #[serde(default)]
    pub(super) force_usage: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AgentAccountParams {
    pub(super) harness: HarnessId,
    pub(super) account_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StartAgentLoginParams {
    pub(super) harness: HarnessId,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LocalAgentSignInStatusParams {
    pub(super) harness: HarnessId,
    pub(super) account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateManagedAccountParams {
    pub(super) harness: HarnessId,
    pub(super) label: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ManagedAccountIdParams {
    pub(super) account_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SubmitLoginCodeParams {
    pub(super) login_id: String,
    pub(super) code: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PromoteSavedAccountParams {
    pub(super) harness: HarnessId,
    pub(super) slot_id: String,
    #[serde(default)]
    pub(super) space_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SetProjectAccountParams {
    pub(super) space_id: String,
    pub(super) harness: HarnessId,
    #[serde(default)]
    pub(super) account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LoginIdParams {
    pub(super) login_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CompleteAgentLoginParams {
    pub(super) login_id: String,
    pub(super) code: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UploadChunkParams {
    pub(super) upload_id: String,
    /// Base64 payload chunk.
    pub(super) data: String,
    #[serde(default)]
    pub(super) seq: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UploadCommitParams {
    pub(super) upload_id: String,
    pub(super) file_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReadAttachmentChunkParams {
    pub(super) path: String,
    #[serde(default)]
    pub(super) offset: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FetchToolBlobParams {
    /// Doc-resident sidecar ref (`{chatId}/{partId}` or `…​.diff`).
    pub(super) blob_ref: String,
}

/// Optional checkout scope for `WatchCheckoutDiffs`. Legacy clients send null
/// (or an empty object) and continue to receive every local checkout. New
/// clients send cwd plus the identity they expect; cwd is resolved afresh and
/// a mismatched id is rejected before the stream starts.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WatchCheckoutDiffsParams {
    #[serde(default)]
    pub(super) checkout_id: Option<String>,
    #[serde(default)]
    pub(super) cwd: Option<String>,
}

/// The Mutate surface (feature-inventory §2 DataRpc), tagged by `op`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub(super) enum MutateParams {
    #[serde(rename_all = "camelCase")]
    CreateChat {
        chat_id: String,
        /// The project the chat is created in — fixes host device + base cwd.
        /// `None` mints a project-less chat: `deviceId` picks the host and the
        /// cwd defaults to the portable `~` marker (resolved on the host to a
        /// Mast-owned per-chat scratch directory at run time).
        #[serde(default)]
        space_id: Option<String>,
        /// Host device for a project-less chat; ignored when `spaceId` is set.
        #[serde(default)]
        device_id: Option<String>,
        #[serde(default)]
        config: Option<ChatConfig>,
        /// The picked ref, named on the row from the first frame (the footer
        /// read "Select ref" until the diff reconciler stamped it).
        #[serde(default)]
        branch: Option<String>,
        /// Cwd override (isolated-worktree path); default = the space's folder.
        #[serde(default)]
        cwd: Option<String>,
        /// The chat whose agent is creating this one (Mast MCP); recorded
        /// on the row as `parentChatId` for orchestration trees.
        #[serde(default)]
        parent_chat_id: Option<String>,
        /// "Ask on the side": the main chat this one is a side chat of. The
        /// host copies Space, cwd, checkout, config and account pin from it
        /// and ignores the other placement fields.
        #[serde(default)]
        side_of: Option<String>,
    },
    /// Create a space (device + folder pair). Idempotent by id; a live
    /// duplicate `(deviceId, path)` no-ops. `gitDetected` is seeded from the
    /// picker's FolderEntry — the owning device's SpacesSync re-verifies.
    #[serde(rename_all = "camelCase")]
    CreateSpace {
        space_id: String,
        device_id: String,
        path: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        git_detected: bool,
    },
    /// LWW display-name set; `name: None` clears back to basename(path).
    #[serde(rename_all = "camelCase")]
    RenameSpace {
        space_id: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Hard delete: cascades to every chat (and session row) in the space.
    /// Live runs hosted here are interrupted best-effort.
    #[serde(rename_all = "camelCase")]
    DeleteSpace { space_id: String },
    #[serde(rename_all = "camelCase")]
    RenameChat { chat_id: String, title: String },
    /// Set the chat's checkout branch label — the sidebar's
    /// "project · branch" sub-line.
    #[serde(rename_all = "camelCase")]
    SetChatBranch { chat_id: String, branch: String },
    /// Retarget a chat onto another folder — mid-session switch to an
    /// EXISTING worktree (the picked ref's checkout). Next run starts a
    /// fresh harness conversation there (resume is cwd-scoped).
    #[serde(rename_all = "camelCase")]
    SetChatCwd { chat_id: String, cwd: String },
    /// Backdate a chat's activity timestamps (epoch ms) — the sidebar's
    /// relative-time column. Used by tooling/seeds; the doc fold sets these on
    /// real message traffic.
    #[serde(rename_all = "camelCase")]
    SetChatActivity {
        chat_id: String,
        #[serde(default)]
        last_message_at: Option<i64>,
        #[serde(default)]
        created_at: Option<i64>,
    },
    /// Re-home a chat to another device (tooling/seeds; device migration later).
    #[serde(rename_all = "camelCase")]
    SetChatHost { chat_id: String, device_id: String },
    #[serde(rename_all = "camelCase")]
    SetChatArchived { chat_id: String, archived: bool },
    /// Change one pin without replacing another device's edits.
    #[serde(rename_all = "camelCase")]
    ChangeSidebarPin {
        change: mast_proto::SidebarPinChange,
    },
    /// Full-config replace on the chat row (zeron `SetChatConfig`): the
    /// composer's mid-session model / reasoning / options changes, LWW-synced
    /// so they survive restarts and reach every device.
    #[serde(rename_all = "camelCase")]
    SetChatConfig { chat_id: String, config: ChatConfig },
    /// Per-chat approval mode (`config.autoApprove`: Ask vs Auto-approve):
    /// patches only that field of the row's current config (LWW-synced, so
    /// it survives restarts and applies to every later turn). No-op on a
    /// missing or config-less row — the composer's chip writes a full
    /// `setChatConfig` there instead.
    #[serde(rename_all = "camelCase")]
    SetChatAutoApprove {
        chat_id: String,
        auto_approve: bool,
        /// Rules mode; older clients omit it and get Ask or Auto.
        #[serde(default)]
        approval_rules: bool,
    },
    /// Tombstone: removes the chats-map row; the session doc remains.
    #[serde(rename_all = "camelCase")]
    DeleteChat { chat_id: String },
    #[serde(rename_all = "camelCase")]
    RenameDevice { device_id: String, name: String },
    /// Synced seen marker (LWW + monotonic guard): clears the "completed"
    /// badge on every device. `at` is epoch ms; default = now.
    #[serde(rename_all = "camelCase")]
    MarkChatSeen {
        chat_id: String,
        #[serde(default)]
        at: Option<i64>,
    },
}
