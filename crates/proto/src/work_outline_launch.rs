//! Work Outline launch (Milestone 3 stage 2): the wire types of the two
//! execution-touching operations, `CreateChatForWorkItem` and
//! `ContinueChatForWorkItem` (spec "Service operations"; contract section 4,
//! idempotency audit section 7).
//!
//! Both are dedicated RPC methods, not `WorkOutlineMutate` ops: a launch must
//! run on the Space's owning device (it validates paths, may create a
//! worktree, and queues the first Turn), while `WorkOutlineMutate` is a local
//! LWW registry write accepted anywhere. `CreateChatForWorkItem` is
//! forwardable (the receiving engine forwards it to the owner itself);
//! `ContinueChatForWorkItem` queues durably from any device and the host
//! revalidates the binding at drain (contract decision 6.4.1).
//!
//! Every record id is derived from the `operationId`
//! ([`crate::work_outline::derive_id`]): Chat `chat`, first user message
//! `message`, command `command`, revision `revision`, application
//! `application`. The execution binding and the assignment are keyed by the
//! Chat id. A retry with the same `operationId` and inputs completes the
//! missing steps and returns the same ids; other inputs are
//! `operationConflict`.

use serde::{Deserialize, Serialize};

use crate::entities::ChatConfig;
use crate::work_outline_execution::ExecutionConfiguration;

/// The prompt of a launch or continuation: the visible text plus image
/// attachments exactly as a `Run` request carries them (absolute paths, or
/// `pending://` refs already uploaded to the owning device).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemPrompt {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
}

/// A deliberate per-launch replacement of the work item's resolved execution
/// configuration (the Chat then shows as an execution exception).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionOverride {
    pub configuration: ExecutionConfiguration,
}

/// Which checkout the launch runs in when the configuration's target
/// repository (the primary binding, else the only binding, else the
/// repository at the execution root) does not pin a worktree. Internally
/// tagged by `type`. Strict (request enum).
///
/// - `useExisting`: an existing registered worktree of that repository
///   (`worktreeId` = its `CheckoutIdentity.id`, as `DescribeExecutionRepository`
///   reports it).
/// - `createNew`: create `<superproject>/.worktrees/<label>/<slug(name)>` with
///   the existing local chat-worktree flow (`label` is derived by that flow:
///   `workspace`, or the member repository's folder name). Idempotent for the
///   operation: a retry reuses the worktree this operation created and fails
///   (never falls back to the base checkout) when something else is there.
///
/// When the execution root IS the target repository's checkout, the chosen
/// worktree also becomes the execution root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorktreeChoice {
    #[serde(rename_all = "camelCase")]
    UseExisting {
        worktree_id: String,
        worktree_path: String,
    },
    #[serde(rename_all = "camelCase")]
    CreateNew { name: String },
}

/// Params of `CreateChatForWorkItem`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChatForWorkItemParams {
    pub work_item_id: String,
    pub initial_prompt: WorkItemPrompt,
    /// Harness, model, reasoning, options, sandbox, approval: the same
    /// `ChatConfig` the `createChat` mutation takes. Absent = the host's
    /// default harness with default options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<ChatConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_override: Option<ExecutionOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_choice: Option<WorktreeChoice>,
    pub operation_id: String,
    /// The Chat that started this one (agent `create_chat` for a work item):
    /// recorded as the new Chat's `parentChatId`. Absent for a launch the
    /// user started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_chat_id: Option<String>,
    /// Device routing (the Space's owner). Optional: the receiving engine
    /// forwards to the owner on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
    /// Start Chat's account chip: pin the new Chat to this managed account
    /// (on `config`'s harness) instead of the Space's default; an empty
    /// string pins the Default login. Absent = the project default,
    /// resolved at the first run as usual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Make the new Chat the holder of this standing role of `workItemId`
    /// (a project's Manager). The launch refuses when the role already has a
    /// different Chat, and a retry of the same operation adopts only once.
    /// Hosts without `standing-role-launch-v1` would drop it, so callers gate on
    /// that capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standing_role: Option<crate::StandingRoleKind>,
}

/// Prefix of the `standingRoleSuccessor` a role's first-start Chat carries
/// until its first message is queued and the role adopts it. The marker is
/// what hides the Chat wherever superseded role chats are hidden, so a
/// partial first start never shows as an ordinary session.
pub const STANDING_ROLE_PROVISIONAL_PREFIX: &str = "provisional:";

/// The provisional marker's role part for `kind` and `scope_item_id`:
/// `provisional:<role id>`. A Chat's marker is this tag followed by `|` and the
/// operation that created it, so recovery knows which command is its launch's.
pub fn standing_role_provisional_tag(kind: crate::StandingRoleKind, scope_item_id: &str) -> String {
    format!(
        "{STANDING_ROLE_PROVISIONAL_PREFIX}{}",
        crate::standing_role_id(kind, scope_item_id)
    )
}

/// The marker a first-start Chat created by `operation_id` carries.
pub fn standing_role_provisional_marker(
    kind: crate::StandingRoleKind,
    scope_item_id: &str,
    operation_id: &str,
) -> String {
    format!(
        "{}|{operation_id}",
        standing_role_provisional_tag(kind, scope_item_id)
    )
}

impl crate::Chat {
    /// The provisional marker when this Chat is a role's first start that has
    /// not been adopted yet.
    pub fn standing_role_provisional(&self) -> Option<&str> {
        self.standing_role_successor
            .as_deref()
            .filter(|marker| marker.starts_with(STANDING_ROLE_PROVISIONAL_PREFIX))
    }

    /// The operation that created this provisional Chat, whose derived command
    /// is the only message the Chat may take before it is adopted.
    pub fn standing_role_provisional_operation(&self) -> Option<&str> {
        self.standing_role_provisional()?
            .split_once('|')
            .map(|(_, operation)| operation)
    }

    /// The scope item of the role this Chat is the not-yet-adopted first start
    /// of, when it is one of `kind`.
    pub fn standing_role_provisional_scope(&self, kind: crate::StandingRoleKind) -> Option<&str> {
        let prefix = format!("{STANDING_ROLE_PROVISIONAL_PREFIX}{}:", kind.as_str());
        let rest = self.standing_role_provisional()?.strip_prefix(prefix.as_str())?;
        let item = rest.split_once('|').map_or(rest, |(item, _)| item);
        (!item.is_empty()).then_some(item)
    }

    /// Whether this Chat is a not-yet-adopted first start of exactly this role.
    pub fn is_provisional_for(&self, kind: crate::StandingRoleKind, scope_item_id: &str) -> bool {
        let tag = standing_role_provisional_tag(kind, scope_item_id);
        self.standing_role_provisional().is_some_and(|marker| {
            marker == tag || marker.strip_prefix(tag.as_str()).is_some_and(|rest| rest.starts_with('|'))
        })
    }
}

/// Request a successor for an existing coordinator chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandOffOrchestratorParams {
    pub chat_id: String,
    pub summary: String,
    pub operation_id: String,
    /// Absent for older clients, which archived ordinary chats separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<crate::HarnessId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<crate::ReasoningLevel>,
}

/// Params of `ContinueChatForWorkItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueChatForWorkItemParams {
    pub chat_id: String,
    pub prompt: WorkItemPrompt,
    pub operation_id: String,
}

/// Where the launched (or continued) Turn is, read from its command.
/// Lenient: an unknown value reads as `queued`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkItemLaunchState {
    /// The host executed the command (the Turn started or was held at a Turn
    /// boundary).
    Started,
    /// The host rejected the command; `detail` says why. A retry with the
    /// same `operationId` re-issues it once the cause is fixed.
    Rejected,
    /// The Turn is durably queued and waits for the host's drain. (Last so
    /// it can be the lenient fallback.)
    #[default]
    #[serde(other)]
    Queued,
}

/// Reply of `CreateChatForWorkItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChatForWorkItemReply {
    pub chat_id: String,
    pub message_id: String,
    pub command_id: String,
    /// The launch revision and its application; absent when the item had no
    /// effective instructions (the first Turn is then a plain `Run`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,
    /// The execution root the Chat was created in (never retargeted).
    pub execution_root: String,
    pub state: WorkItemLaunchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Reply of `ContinueChatForWorkItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueChatForWorkItemReply {
    pub chat_id: String,
    pub message_id: String,
    pub command_id: String,
    pub state: WorkItemLaunchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HarnessId, SandboxLevel};
    use serde_json::json;

    #[test]
    fn provisional_tag_names_one_role_and_is_not_a_chat_id() {
        use crate::StandingRoleKind::Manager;
        let tag = standing_role_provisional_tag(Manager, "item-1");
        assert_eq!(tag, "provisional:manager:item-1");
        assert_ne!(tag, standing_role_provisional_tag(Manager, "item-2"));
        let mut chat: crate::Chat = serde_json::from_value(json!({
            "id": "c", "deviceId": "d", "archived": false,
            "createdAt": "2026-01-01T00:00:00Z", "standingRoleSuccessor": tag,
        }))
        .unwrap();
        assert!(chat.is_standing_role_predecessor(), "hidden like a superseded chat");
        assert!(chat.is_provisional_for(Manager, "item-1"));
        assert!(!chat.is_provisional_for(Manager, "item-2"));
        assert_eq!(chat.standing_role_provisional_scope(Manager), Some("item-1"));
        assert_eq!(chat.standing_role_provisional_scope(crate::StandingRoleKind::Assistant), None);
        // The marker carries the creating operation, and an item whose id
        // extends another's is not confused with it.
        chat.standing_role_successor =
            Some(standing_role_provisional_marker(Manager, "item-1", "op-a"));
        assert!(chat.is_provisional_for(Manager, "item-1"));
        assert_eq!(chat.standing_role_provisional_scope(Manager), Some("item-1"));
        assert_eq!(chat.standing_role_provisional_operation(), Some("op-a"));
        assert!(!chat.is_provisional_for(Manager, "item"));
        assert!(!chat.is_provisional_for(Manager, "item-10"));
        chat.standing_role_successor = Some("successor-chat".into());
        assert!(chat.standing_role_provisional().is_none());
    }

    #[test]
    fn create_params_wire_shape() {
        let params = CreateChatForWorkItemParams {
            work_item_id: "item".into(),
            initial_prompt: WorkItemPrompt {
                text: "Fix it".into(),
                attachments: vec!["/tmp/a.png".into()],
            },
            config: Some(ChatConfig { approval_rules: false,
                harness: HarnessId::Mock,
                model: Some("m".into()),
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
                auto_approve: true,
            }),
            execution_override: Some(ExecutionOverride {
                configuration: ExecutionConfiguration::implicit("/repo"),
            }),
            worktree_choice: Some(WorktreeChoice::CreateNew {
                name: "fix it".into(),
            }),
            operation_id: "op".into(),
            parent_chat_id: None,
            target_device_id: None,
            account_id: None,
            standing_role: None,
        };
        let value = serde_json::to_value(&params).unwrap();
        assert_eq!(
            value,
            json!({
                "workItemId": "item",
                "initialPrompt": { "text": "Fix it", "attachments": ["/tmp/a.png"] },
                "config": {
                    "harness": "mock", "model": "m", "reasoning": null, "modelOptions": {},
                    "sandbox": "workspace-write", "autoApprove": true
                },
                "executionOverride": {
                    "configuration": { "executionRoot": "/repo", "bindings": [], "schema": 1 }
                },
                "worktreeChoice": { "type": "createNew", "name": "fix it" },
                "operationId": "op",
            })
        );
        let back: CreateChatForWorkItemParams = serde_json::from_value(value).unwrap();
        assert_eq!(back, params);
        let existing: WorktreeChoice = serde_json::from_value(json!({
            "type": "useExisting", "worktreeId": "w", "worktreePath": "/repo/.worktrees/x"
        }))
        .unwrap();
        assert_eq!(
            existing,
            WorktreeChoice::UseExisting {
                worktree_id: "w".into(),
                worktree_path: "/repo/.worktrees/x".into(),
            }
        );
        // Minimal form.
        let minimal: CreateChatForWorkItemParams = serde_json::from_value(json!({
            "workItemId": "item", "initialPrompt": { "text": "go" }, "operationId": "op"
        }))
        .unwrap();
        assert!(minimal.config.is_none() && minimal.worktree_choice.is_none());
    }

    #[test]
    fn replies_and_continue_wire_shape() {
        let reply = CreateChatForWorkItemReply {
            chat_id: "c".into(),
            message_id: "m".into(),
            command_id: "k".into(),
            revision_id: Some("r".into()),
            application_id: Some("a".into()),
            execution_root: "/repo".into(),
            state: WorkItemLaunchState::Queued,
            detail: None,
        };
        assert_eq!(
            serde_json::to_value(&reply).unwrap(),
            json!({
                "chatId": "c", "messageId": "m", "commandId": "k", "revisionId": "r",
                "applicationId": "a", "executionRoot": "/repo", "state": "queued"
            })
        );
        let cont = ContinueChatForWorkItemReply {
            chat_id: "c".into(),
            message_id: "m".into(),
            command_id: "k".into(),
            state: WorkItemLaunchState::Rejected,
            detail: Some("why".into()),
        };
        assert_eq!(
            serde_json::to_value(&cont).unwrap(),
            json!({ "chatId": "c", "messageId": "m", "commandId": "k", "state": "rejected",
                    "detail": "why" })
        );
        let params: ContinueChatForWorkItemParams = serde_json::from_value(json!({
            "chatId": "c", "prompt": { "text": "more" }, "operationId": "op"
        }))
        .unwrap();
        assert_eq!(params.prompt.text, "more");
        let state: WorkItemLaunchState = serde_json::from_value(json!("later")).unwrap();
        assert_eq!(state, WorkItemLaunchState::Queued);
        assert_eq!(
            serde_json::to_value(WorkItemLaunchState::Started).unwrap(),
            json!("started")
        );
    }
}
