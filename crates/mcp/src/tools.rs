//! The tool catalog and its dispatch.
//!
//! Every tool is a thin composition of engine reads/writes from
//! [`Mast`]; the only logic that lives here is argument resolution (chat
//! by prefix, project by path), sender attribution, and the "how do I
//! deliver a message to a chat in this state" choice the composer makes
//! for humans.
//!
//! Agent mode (an engine-injected per-Run token): every call first
//! re-authenticates the token with the engine, and the Work Outline agent
//! tools are listed and forwarded to `AgentToolCall`. Agent mode is scoped to
//! the token's Chat, Space, and device:
//! - `create_chat` takes a mandatory idempotency key (the Chat, message, and
//!   command ids derive from the token's Chat and that key, so a retry never
//!   creates a second Chat or Turn; a retry with other inputs is
//!   `operationConflict`), creates only children of the caller, in the
//!   token's Space on the token's device, with a cwd inside the Space or the
//!   caller's cwd, and a sandbox / approval mode never more permissive than
//!   the caller's;
//! - Communication reaches created chats and non-archived chats in the caller's
//!   item subtree or project; messages also reach the parent.
//!   Admins may send and read in their Space, subject to send sandbox limits.
//!   Respond and interrupt remain limited to direct children, including for admins;
//!   archive has separate restrictions;
//! - `list_projects` / `list_devices` list only the token's Space / device;
//!   `list_harnesses` / `list_models` (this device's capabilities) and
//!   `whoami` are unscoped.
//! - the personal assistant chat (`personal_assistant`, user-designated)
//!   reaches every chat of every Space with all the chat tools above, lists
//!   every Space and device, and creates chats in any Space or on any device
//!   (its sandbox and Auto still cap the new chat's).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mast_doc::SessionCommandPayload;
use mast_proto::agent_tools::{self, AgentOrigin, tools as agent_tool_names};
use mast_proto::{
    Chat, ChatConfig, ChatModelDisplay, HarnessId, ReasoningLevel, RunRequest, SandboxLevel,
    Session, SessionStatus, Space, UserInputAnswer,
};
use mast_rpc::methods;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::mast::{HarnessInfo, Mast, TurnOutcome, resolve_chat_in, session_for, short};
use crate::transcript::{RenderOptions, RenderedMessage, cap_messages, render_entries};

mod tool_catalog;
use tool_catalog::{
    DIFF_TARGET_TOOLS, artifact_catalog, catalog, diff_target_catalog, friction_catalog,
    work_outline_catalog,
};

/// Default and ceiling for the blocking waits.
const DEFAULT_WAIT: Duration = Duration::from_secs(600);
const MAX_WAIT: Duration = Duration::from_secs(3600);
/// MCP clients drop a tool call that is silent for about 1990 s, so a wait
/// returns `stillRunning` before that and the caller calls again.
const WAIT_CAP: Duration = Duration::from_secs(1500);
/// Largest text one message carries in a tool result (the tail is kept).
const MAX_MESSAGE_BYTES: usize = 24_000;
/// Largest combined message text in one tool result.
const MAX_RESULT_BYTES: usize = 48_000;
/// A session row older than this is not trusted to still be working
/// (the UI's staleness window): a crashed host must not read as busy forever.
const SESSION_STALE: chrono::Duration = chrono::Duration::seconds(45);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

pub struct Tools {
    zeron: Arc<Mast>,
}

/// The admin-only workflow pack tools; none takes an idempotency key (apply
/// consumes its preview and undo its receipt, so a retry changes nothing).
#[cfg(test)]
const WORKFLOW_PACK_TOOLS: [&str; 5] = [
    agent_tool_names::WORKFLOW_PACK_EXPORT,
    agent_tool_names::WORKFLOW_PACK_PREVIEW_IMPORT,
    agent_tool_names::WORKFLOW_PACK_APPLY,
    agent_tool_names::WORKFLOW_PACK_UNDO,
    agent_tool_names::WORKFLOW_PACK_RECEIPTS,
];

/// Whether `name` is an engine-side agent tool (`AgentToolCall`).
fn is_engine_agent_tool(name: &str) -> bool {
    name == mast_proto::mast_management::TOOL
        || agent_tool_names::WORK_OUTLINE.contains(&name)
        || agent_tool_names::ARTIFACTS.contains(&name)
        || DIFF_TARGET_TOOLS.contains(&name)
        || name == agent_tool_names::REPORT_FRICTION
        || name == agent_tool_names::LIST_FRICTION
        || name == agent_tool_names::DISMISS_FRICTION
        || name == agent_tool_names::HAND_OFF_ORCHESTRATOR
}

/// Split a render-check result into MCP content: every screenshot as an
/// `image` block (each preceded by a text label naming its viewport and
/// theme), the rest as structured data without the image bytes. Falls back to
/// the single `screenshot` field for engines that predate `screenshots`.
fn render_check_content(mut value: Value) -> (Value, Vec<Value>) {
    let mut content = Vec::new();
    let Some(object) = value.as_object_mut() else {
        return (value, content);
    };
    let single = object.remove("screenshot");
    let mime = |shot: &Value| {
        shot.get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("image/png")
            .to_owned()
    };
    let mut attached = false;
    if let Some(Value::Array(shots)) = object.get_mut("screenshots") {
        for shot in shots.iter_mut() {
            let Some(data) = shot.get("png").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            let label = format!(
                "Screenshot: {} ({}x{}), {} theme",
                shot.get("viewport")
                    .and_then(Value::as_str)
                    .unwrap_or("viewport"),
                shot.get("width").and_then(Value::as_u64).unwrap_or(0),
                shot.get("height").and_then(Value::as_u64).unwrap_or(0),
                shot.get("theme")
                    .and_then(Value::as_str)
                    .unwrap_or("current"),
            );
            content.push(json!({ "type": "text", "text": label }));
            content.push(json!({ "type": "image", "data": data, "mimeType": "image/png" }));
            if let Some(shot) = shot.as_object_mut() {
                shot.remove("png");
                shot.insert("attached".into(), json!(true));
            }
            attached = true;
        }
    }
    if let Some(screenshot) = single {
        let mime = mime(&screenshot);
        if !attached && let Some(data) = screenshot.get("data").and_then(Value::as_str) {
            content.push(json!({ "type": "image", "data": data, "mimeType": mime }));
            attached = true;
        }
        if attached {
            object.insert(
                "screenshot".into(),
                json!({ "mimeType": mime, "attached": true }),
            );
        }
    }
    (value, content)
}

// ---- argument shapes ---------------------------------------------------------

#[derive(Deserialize)]
struct ChatArgs {
    chat: String,
}

#[derive(Deserialize)]
struct ListModelsArgs {
    harness: String,
}

#[derive(Deserialize, Default)]
struct ListChatsArgs {
    project: Option<String>,
    device: Option<String>,
    #[serde(default)]
    include_archived: bool,
    parent: Option<String>,
    #[serde(default)]
    workers: bool,
    limit: Option<usize>,
}

#[derive(Deserialize, Default)]
struct CreateChatArgs {
    #[serde(alias = "idempotencyKey")]
    idempotency_key: Option<String>,
    #[serde(alias = "workItemId")]
    work_item_id: Option<String>,
    #[serde(default, alias = "newChat")]
    new_chat: bool,
    project: Option<String>,
    device: Option<String>,
    parent: Option<String>,
    harness: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
    sandbox: Option<String>,
    #[serde(default, alias = "contextWindow")]
    context_window: Option<Value>,
    #[serde(default, alias = "autoApprove")]
    auto_approve: bool,
    title: Option<String>,
    branch: Option<String>,
    cwd: Option<String>,
    prompt: Option<String>,
    #[serde(default)]
    wait: bool,
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
struct ReadChatArgs {
    chat: String,
    limit: Option<usize>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    include_reasoning: bool,
    include_tools: Option<bool>,
}

#[derive(Deserialize)]
struct SendArgs {
    chat: String,
    text: String,
    mode: Option<String>,
    #[serde(default)]
    wait: bool,
    timeout_secs: Option<u64>,
    #[serde(skip)]
    operation_id: Option<String>,
}

#[derive(Deserialize)]
struct ReportArgs {
    text: String,
    #[serde(rename = "finalHandback")]
    final_handback: Option<FinalHandbackArgs>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinalHandbackArgs {
    assignment_generation: String,
    idempotency_key: String,
    #[serde(default)]
    keep_chat: bool,
}

#[derive(Deserialize)]
struct WaitArgs {
    chat: String,
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
struct ArchiveArgs {
    chat: String,
    archived: Option<bool>,
    #[serde(default)]
    force: bool,
    reason: Option<String>,
}

#[derive(Deserialize)]
struct RespondArgs {
    chat: String,
    request_id: Option<String>,
    #[serde(default)]
    answers: Vec<AnswerArg>,
}

#[derive(Deserialize)]
struct AnswerArg {
    question_id: Option<String>,
    labels: Vec<String>,
}

fn parse<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))
}

fn wait_duration(secs: Option<u64>) -> Duration {
    secs.map(Duration::from_secs)
        .unwrap_or(DEFAULT_WAIT)
        .min(MAX_WAIT)
        .min(WAIT_CAP)
}

fn wait_was_capped(secs: Option<u64>) -> bool {
    secs.is_some_and(|s| Duration::from_secs(s.min(MAX_WAIT.as_secs())) > WAIT_CAP)
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn parse_enum<T: serde::de::DeserializeOwned>(what: &str, raw: &str) -> Result<T, String> {
    serde_json::from_value(Value::String(raw.trim().to_owned()))
        .map_err(|_| format!("unknown {what}: {raw:?}"))
}

// ---- summaries ---------------------------------------------------------------

/// Live posture of one chat as the tools report it.
fn status_of(session: Option<&Session>) -> (String, Option<i64>) {
    let Some(session) = session else {
        return ("idle".into(), None);
    };
    let age = chrono::Utc::now() - session.updated_at;
    let stale = age > SESSION_STALE;
    let label = match session.status {
        SessionStatus::Working if stale => "working (stale — host may be offline)",
        SessionStatus::Working => "working",
        SessionStatus::AwaitingInput => "awaitingInput",
        SessionStatus::Errored => "errored",
        SessionStatus::Idle => "idle",
    };
    (label.into(), Some(age.num_seconds().max(0)))
}

fn summarize_chat(chat: &Chat, spaces: &[Space], sessions: &[Session]) -> Value {
    let space = chat
        .space_id
        .as_deref()
        .and_then(|id| spaces.iter().find(|s| s.id == id));
    let session = session_for(sessions, chat);
    let (status, status_age) = status_of(session.as_ref());
    json!({
        "id": chat.id,
        "url": mast_proto::session_link::session_link(&chat.id),
        "title": chat.title,
        "project": space.map(|s| json!({
            "id": s.id,
            "name": s.display_name(),
            "path": s.path,
        })),
        "deviceId": chat.device_id,
        "cwd": chat.cwd.clone().or_else(|| space.map(|s| s.path.clone())),
        "branch": chat.branch,
        "harness": chat.config.as_ref().map(|c| c.harness),
        // `model` is what ran when the harness reported it, else what the chat
        // is configured to use (`modelIsActual` says which); null = harness
        // default decides.
        "model": match chat.model_display() {
            ChatModelDisplay::Actual { actual, .. } => Some(actual),
            ChatModelDisplay::Configured(model) => Some(model),
            ChatModelDisplay::Default => None,
        },
        "modelIsActual": matches!(chat.model_display(), ChatModelDisplay::Actual { .. }),
        "actualModel": chat.actual_model,
        "configuredModel": chat.config.as_ref().and_then(|c| c.model.clone()),
        "reasoning": chat.config.as_ref().and_then(|c| c.reasoning),
        "archived": chat.archived,
        "parentChatId": chat.parent_chat_id,
        "status": status,
        "awaitingInput": session.as_ref().and_then(|s| s.awaiting_input.as_ref()),
        "lastTurnToolFailures": session.as_ref().and_then(|s| s.last_turn_tool_failures.as_ref()),
        "statusAgeSecs": status_age,
        "lastMessageAt": chat.last_message_at,
        "lastMessagePreview": chat.last_message_preview,
        "createdAt": chat.created_at,
    })
}

/// Add `archivable` and `reason` to a chat summary when the engine rated it.
fn with_archivable(
    mut summary: Value,
    info: &std::collections::HashMap<String, Value>,
    id: &str,
) -> Value {
    if let Some(verdict) = info.get(id) {
        summary["archivable"] = verdict["archivable"].clone();
        summary["archivableReason"] = verdict["reason"].clone();
    }
    summary
}

fn last_pending_input(messages: &[RenderedMessage]) -> Option<Value> {
    messages.iter().rev().find_map(|m| m.pending_input.clone())
}

// ---- dispatch ----------------------------------------------------------------

impl Tools {
    pub fn new(zeron: Arc<Mast>) -> Self {
        Self { zeron }
    }

    /// The original tools, plus the Work Outline agent tools when the
    /// engine injected this server with a token.
    pub fn list(&self) -> Vec<ToolDef> {
        let mut defs = catalog();
        if self.zeron.has_token() {
            defs.extend(work_outline_catalog());
            defs.extend(artifact_catalog());
            defs.extend(diff_target_catalog());
            defs.extend(friction_catalog());
            let catalog = mast_proto::mast_management::operations();
            let methods: Vec<_> = catalog
                .as_object()
                .expect("management catalog")
                .keys()
                .collect();
            defs.push(ToolDef {
                name: mast_proto::mast_management::TOOL,
                description: "View and manage Mast across all Spaces as the user-designated personal assistant. Call with {} first for supported methods and parameter examples. Reads return current snapshots. Covers scheduled-task details, editing, enabling/disabling, history and manual re-runs; sessions, sidebar groups and pins; Spaces, devices, Work Outline items/instructions/settings, project actions, harnesses, wake and title settings, friction and session links. Use existing chat tools for transcripts, messages and running work. Ordinary chats cannot use this tool; every call rechecks the designation. Read current details before replacing them; preserve fields you are not changing. Reuse operationId for retries of the same write, and use a new one for each new action.",
                input_schema: json!({"type":"object", "properties": {
                    "method":{"type":"string", "enum":methods},
                    "params":{"type":"object", "description":"Parameters from the guide returned by an empty mast_manage call; desktop RPC wire shapes."}
                }, "additionalProperties":false}),
            });
        }
        defs
    }

    pub fn has(&self, name: &str) -> bool {
        self.list().iter().any(|t| t.name == name)
    }

    /// `Ok` is the tool's structured result; `Err` is a message the model
    /// should read (surfaced as `isError`, never as a protocol error).
    ///
    /// With a token, every call (reads included) first re-authenticates it:
    /// once the Run that received it ends, nothing works.
    pub async fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        Box::pin(self.call_with_content(name, args))
            .await
            .map(|(value, _)| value)
    }

    /// [`Self::call`] plus extra MCP content blocks (the render check's
    /// screenshot as an `image` block).
    pub async fn call_with_content(
        &self,
        name: &str,
        args: Value,
    ) -> Result<(Value, Vec<Value>), String> {
        if name == agent_tool_names::ARTIFACT_RENDER_CHECK {
            let value = self
                .zeron
                .agent_tool(name, args)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(render_check_content(value));
        }
        Box::pin(self.call_value(name, args))
            .await
            .map(|value| (value, Vec::new()))
    }

    /// Run one tool. Guessed argument names are normalised first (one place:
    /// `agent_tools::normalize_arguments`), and an argument error from
    /// `create_chat` lists its parameters and an example (engine tools add
    /// theirs engine-side).
    async fn call_value(&self, name: &str, args: Value) -> Result<Value, String> {
        let args = agent_tools::normalize_arguments(name, &args);
        Box::pin(self.call_normalized(name, args)).await.map_err(|error| {
            let argument_error = error.starts_with("invalid arguments")
                || error.contains("idempotencyKey is required");
            if matches!(name, "create_chat" | "respond_to_input") && argument_error {
                agent_tools::with_usage(name, &error)
            } else {
                error
            }
        })
    }

    async fn call_normalized(&self, name: &str, args: Value) -> Result<Value, String> {
        // `AgentToolCall` authenticates the token itself: one round trip.
        if is_engine_agent_tool(name) {
            return self
                .zeron
                .agent_tool(name, args)
                .await
                .map_err(|e| e.to_string());
        }
        let origin = self.zeron.authenticate().await.map_err(|e| e.to_string())?;
        let origin = origin.as_ref();
        let result = match name {
            "whoami" => self.whoami(origin).await,
            "list_devices" => self.list_devices(origin).await,
            "list_projects" => self.list_projects(origin).await,
            "list_harnesses" => self.list_harnesses().await,
            "list_models" => self.list_models(parse(args)?).await,
            "list_chats" => self.list_chats(origin, parse(args)?).await,
            "get_chat" => self.get_chat(origin, parse(args)?).await,
            "create_chat" => Box::pin(self.create_chat(origin, parse(args)?)).await,
            "read_chat" => self.read_chat(origin, parse(args)?).await,
            "send_message" => self.send_message(origin, parse(args)?).await,
            "report_to_parent" => self.report_to_parent(origin, parse(args)?).await,
            "wait_for_turn" => self.wait_for_turn(origin, parse(args)?).await,
            "interrupt_chat" => self.interrupt_chat(origin, parse(args)?).await,
            "respond_to_input" => self.respond_to_input(origin, parse(args)?).await,
            "archive_chat" => self.archive_chat(origin, parse(args)?).await,
            other => return Err(format!("unknown tool: {other}")),
        };
        result.map_err(|e| e.to_string())
    }

    async fn whoami(&self, origin: Option<&AgentOrigin>) -> anyhow::Result<Value> {
        let local_device = self.zeron.local_device_id().await?;
        let engine = self.zeron.engine_info().await.unwrap_or(Value::Null);
        let chat = match origin {
            Some(origin) => match self.zeron.resolve_chat(&origin.chat_id).await {
                Ok(chat) => {
                    let (spaces, sessions) =
                        tokio::try_join!(self.zeron.spaces(), self.zeron.sessions())?;
                    summarize_chat(&chat, &spaces, &sessions)
                }
                Err(_) => json!({ "id": origin.chat_id }),
            },
            None => Value::Null,
        };
        Ok(json!({
            "chat": chat,
            "authenticated": origin.is_some(),
            "originDeviceId": origin.map(|o| o.device_id.clone()),
            "runId": origin.map(|o| o.run_id.clone()),
            "adminSpaceId": origin.and_then(|o| o.admin_space_id.clone()),
            "personalAssistant": origin.is_some_and(|o| o.personal_assistant),
            "managerScopeItemId": origin.and_then(|o| o.manager_scope_item_id.clone()),
            "managementTools": if origin.is_some_and(|o| o.personal_assistant) {
                json!([mast_proto::mast_management::TOOL])
            } else {
                json!([])
            },
            "localDeviceId": local_device,
            "workspaceScope": engine.get("workspaceScope").cloned().unwrap_or(Value::Null),
            "workOutlineTools": if origin.is_some() {
                json!(agent_tool_names::WORK_OUTLINE)
            } else {
                json!([])
            },
            "artifactTools": if origin.is_some() {
                json!(agent_tool_names::ARTIFACTS)
            } else {
                json!([])
            },
            "note": if origin.is_some() {
                "Messages you send are attributed to this chat; it cannot message itself. \
                 You can reach only the chats you created (plus read this one), and \
                 create_chat works only in this chat's Space, never with a wider \
                 sandbox than this chat's. Mutating Work Outline tools and create_chat \
                 need an idempotency key. Admin chats (user-granted, per Space, expiring) \
                 also reach every chat and item of their Space; see adminSpaceId. The chat \
                 holding a project's manager role (see managerScopeItemId; no expiry, it \
                 moves with the role on hand-off) reaches every chat and item inside that \
                 project, and no other project. The \
                 personal assistant chat (user-designated; see personalAssistant) reaches \
                 every chat in every Space with every chat tool, and create_chat works \
                 in any Space or on any device, still never wider than this chat's sandbox. \
                 The personal assistant can also use mast_manage to inspect and edit \
                 scheduled tasks, trigger re-runs, organize sessions into sidebar groups \
                 and pins, and manage Mast across Spaces; call it with {} for its guide."
            } else {
                "Not running inside a chat: messages are sent without attribution."
            },
        }))
    }

    async fn list_devices(&self, origin: Option<&AgentOrigin>) -> anyhow::Result<Value> {
        let (mut devices, local) =
            tokio::try_join!(self.zeron.devices(), self.zeron.local_device_id())?;
        if let Some(origin) = origin.filter(|o| !o.personal_assistant) {
            devices.retain(|d| d.id == origin.device_id);
        }
        Ok(json!({
            "devices": devices.iter().map(|d| json!({
                "id": d.id,
                "name": d.name,
                "platform": d.platform,
                "local": d.id == local,
                "lastSeenAt": d.last_seen_at,
                "version": d.version,
            })).collect::<Vec<_>>()
        }))
    }

    async fn list_projects(&self, origin: Option<&AgentOrigin>) -> anyhow::Result<Value> {
        let (mut spaces, devices) = tokio::try_join!(self.zeron.spaces(), self.zeron.devices())?;
        if let Some(origin) = origin.filter(|o| !o.personal_assistant) {
            spaces.retain(|s| s.id == origin.space_id);
        }
        let device_name = |id: &str| devices.iter().find(|d| d.id == id).map(|d| d.name.clone());
        Ok(json!({
            "projects": spaces.iter().map(|s| json!({
                "id": s.id,
                "name": s.display_name(),
                "path": s.path,
                "deviceId": s.device_id,
                "deviceName": device_name(&s.device_id),
                "git": s.git_detected,
            })).collect::<Vec<_>>()
        }))
    }

    async fn list_harnesses(&self) -> anyhow::Result<Value> {
        let harnesses = self.zeron.harnesses().await?;
        Ok(json!({
            "harnesses": harnesses.iter().map(|h| json!({
                "id": h.id,
                "name": h.name,
                "available": h.available(),
                "installed": h.installed,
                "steersMidTurn": h.steers_mid_turn(),
                "reasoningLevels": h.reasoning_levels,
            })).collect::<Vec<_>>()
        }))
    }

    async fn list_models(&self, args: ListModelsArgs) -> anyhow::Result<Value> {
        let harness: HarnessId =
            parse_enum("harness", &args.harness).map_err(anyhow::Error::msg)?;
        let models = self.zeron.models(harness).await?;
        Ok(json!({
            "harness": harness,
            "models": models.iter().map(|m| json!({
                "id": m.id,
                "label": m.label,
                "description": m.description,
                "reasoningLevels": m.reasoning_levels,
            })).collect::<Vec<_>>()
        }))
    }

    async fn list_chats(
        &self,
        origin: Option<&AgentOrigin>,
        args: ListChatsArgs,
    ) -> anyhow::Result<Value> {
        let (mut chats, spaces, sessions) = tokio::try_join!(
            self.zeron.chats(),
            self.zeron.spaces(),
            self.zeron.sessions()
        )?;
        if let Some(origin) = origin.filter(|o| !o.personal_assistant) {
            let scope = if args.workers {
                self.archive_info(&[]).await.unwrap_or_default()
            } else {
                Default::default()
            };
            if origin.manager_scope_item_id.is_some() {
                // A project manager sees only its project, never its other routes.
                let managed = self.manager_chat_ids(origin).await;
                chats.retain(|c| c.id == origin.chat_id || managed.contains(&c.id));
            } else {
                chats.retain(|c| reachable(origin, c, true, true) || scope.contains_key(&c.id));
            }
        }
        if let Some(project) = args.project.as_deref() {
            let space = self.zeron.resolve_space(project).await?;
            chats.retain(|c| c.space_id.as_deref() == Some(space.id.as_str()));
        }
        if args.device.is_some() {
            let device = self.zeron.resolve_device_id(args.device.as_deref()).await?;
            chats.retain(|c| c.device_id == device);
        }
        // Predecessors of a standing role stay readable by id (`read_chat`)
        // but are never listed, archived or not.
        chats.retain(|c| !c.is_standing_role_predecessor());
        if !args.include_archived {
            chats.retain(|c| !c.archived);
        }
        if let Some(parent) = args.parent.as_deref() {
            let parent = self.reach_chat(origin, parent, Reach::SelfOrChild).await?;
            chats.retain(|c| c.parent_chat_id.as_deref() == Some(parent.id.as_str()));
        }
        chats.sort_by(|a, b| {
            let a_at = a.last_message_at.unwrap_or(a.created_at);
            let b_at = b.last_message_at.unwrap_or(b.created_at);
            b_at.cmp(&a_at)
        });
        let total = chats.len();
        let limit = args.limit.unwrap_or(50).clamp(1, 500);
        let page: Vec<&Chat> = chats.iter().take(limit).collect();
        let info = self.chat_archive_info(origin, &page).await;
        Ok(json!({
            "total": total,
            "chats": page.iter()
                .map(|c| with_archivable(summarize_chat(c, &spaces, &sessions), &info, &c.id))
                .collect::<Vec<_>>()
        }))
    }

    async fn get_chat(
        &self,
        origin: Option<&AgentOrigin>,
        args: ChatArgs,
    ) -> anyhow::Result<Value> {
        let chat = self
            .reach_chat(origin, &args.chat, Reach::SelfOrChild)
            .await?;
        let (spaces, sessions, entries) = tokio::try_join!(
            self.zeron.spaces(),
            self.zeron.sessions(),
            self.zeron.transcript(&chat.id)
        )?;
        let rendered = render_entries(&entries, RenderOptions::default());
        let info = self.chat_archive_info(origin, &[&chat]).await;
        let mut summary =
            with_archivable(summarize_chat(&chat, &spaces, &sessions), &info, &chat.id);
        summary["messageCount"] = json!(rendered.len());
        summary["pendingInput"] = last_pending_input(&rendered).unwrap_or(Value::Null);
        summary["lastMessage"] = rendered.last().map(|m| json!(m)).unwrap_or(Value::Null);
        Ok(summary)
    }

    async fn reusable_chat(
        &self,
        origin: &AgentOrigin,
        item: &str,
        harnesses: &[HarnessInfo],
    ) -> anyhow::Result<Option<(Chat, String)>> {
        let outline = self.zeron.agent_tool(agent_tool_names::READ_SUBTREE,
            json!({"workItemId": item, "maxDepth": 0, "includeCompleted": true, "includeLaunchChatIds": true})).await?;
        let ids: std::collections::HashSet<&str> = outline["launchChatIds"]
            .as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let (chats, sessions) = tokio::try_join!(self.zeron.chats(), self.zeron.sessions())?;
        Ok(chats.into_iter()
            .filter(|chat| !chat.archived && chat.id != origin.chat_id && ids.contains(chat.id.as_str()))
            .filter_map(|chat| {
                let session = session_for(&sessions, &chat);
                let (status, _) = status_of(session.as_ref());
                let recent = chrono::Utc::now() - chat.last_message_at.unwrap_or(chat.created_at)
                    <= chrono::Duration::days(3);
                if !(status.starts_with("working") || status == "awaitingInput" || (status == "idle" && recent)) {
                    return None;
                }
                let mode = match status.as_str() {
                    "working" if harnesses.iter().find(|h| Some(h.id) == chat.config.as_ref().map(|c| c.harness))
                        .is_some_and(HarnessInfo::steers_mid_turn) => "steer",
                    "working" => "queue",
                    "awaitingInput" => "awaitingInput",
                    _ => "run",
                };
                Some((chat, mode.to_owned()))
            })
            .max_by_key(|(chat, _)| chat.last_message_at.unwrap_or(chat.created_at)))
    }

    async fn reuse_chat(
        &self,
        origin: &AgentOrigin,
        claim: &Value,
        work_item_id: Option<&str>,
        args: &CreateChatArgs,
        operation_id: String,
    ) -> anyhow::Result<Value> {
        let existing_id = claim["reuseChatId"].as_str().expect("claimed chat id");
        let chat = self.reach_chat(Some(origin), existing_id, Reach::MessageScope).await?;
        let sessions = self.zeron.sessions().await?;
        let (status, _) = status_of(session_for(&sessions, &chat).as_ref());
        let mut result = json!({"chatId": chat.id, "status": status, "workItemId": work_item_id,
            "reusedExisting": true, "replayed": claim["replayed"]});
        if let Some(prompt) = args.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
            if status == "awaitingInput" && claim["replayed"] != true {
                anyhow::bail!("chat is waiting for an answer; use respond_to_input");
            }
            let sent = Box::pin(self.send_message(Some(origin), SendArgs {
                chat: existing_id.to_owned(), text: prompt.to_owned(), mode: Some("deferred_auto".to_owned()),
                wait: args.wait, timeout_secs: args.timeout_secs, operation_id: Some(operation_id),
            })).await?;
            result["sent"] = sent["sent"].clone();
            if args.wait {
                result["turn"] = sent["turn"].clone();
            }
        }
        Ok(result)
    }

    async fn create_chat(
        &self,
        origin: Option<&AgentOrigin>,
        args: CreateChatArgs,
    ) -> anyhow::Result<Value> {
        // Agent mode: the ids derive from (token Chat, key), so a retry
        // finds the Chat it already created instead of minting another.
        let agent_key = match origin {
            Some(_) => Some(
                agent_tools::validate_idempotency_key(args.idempotency_key.as_deref())
                    .map_err(anyhow::Error::msg)?
                    .to_owned(),
            ),
            None => None,
        };
        let harnesses = self.zeron.harnesses().await?;
        let harness = match args.harness.as_deref() {
            Some(raw) => {
                let id: HarnessId = parse_enum("harness", raw).map_err(anyhow::Error::msg)?;
                if let Some(info) = harnesses.iter().find(|h| h.id == id)
                    && !info.available()
                {
                    anyhow::bail!(
                        "harness {raw} is not available on this device (see list_harnesses)"
                    );
                }
                id
            }
            None => default_harness(&harnesses)?,
        };
        if let Some(model) = args.model.as_deref().filter(|m| !m.trim().is_empty())
            && let Ok(models) = self.zeron.models(harness).await
            && !models.is_empty()
            && !models.iter().any(|m| m.id == model)
        {
            anyhow::bail!(
                "model {model:?} is not offered by {harness:?}; available: {}",
                models
                    .iter()
                    .map(|m| m.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let reasoning: Option<ReasoningLevel> = match args.reasoning.as_deref() {
            Some(raw) => Some(parse_enum("reasoning level", raw).map_err(anyhow::Error::msg)?),
            None => None,
        };
        let requested_sandbox: Option<SandboxLevel> = match args.sandbox.as_deref() {
            Some(raw) => Some(parse_enum("sandbox", raw).map_err(anyhow::Error::msg)?),
            None => None,
        };
        // Agent mode: the calling Chat (its sandbox caps the child's, its cwd
        // bounds the child's cwd).
        let caller = match origin {
            Some(origin) => Some(
                self.zeron
                    .chats()
                    .await?
                    .into_iter()
                    .find(|c| c.id == origin.chat_id)
                    .ok_or_else(|| anyhow::anyhow!("this agent's chat no longer exists"))?,
            ),
            None => None,
        };
        let sandbox = match &caller {
            Some(caller) => {
                let cap = caller
                    .config
                    .as_ref()
                    .map(|c| c.sandbox)
                    .unwrap_or(SandboxLevel::WorkspaceWrite);
                match requested_sandbox {
                    Some(level) if sandbox_rank(level) > sandbox_rank(cap) => anyhow::bail!(
                        "sandbox {} is more permissive than this chat's ({}); a child chat \
                         can never have a wider sandbox than its parent",
                        sandbox_name(level),
                        sandbox_name(cap)
                    ),
                    Some(level) => level,
                    None => std::cmp::min_by_key(SandboxLevel::WorkspaceWrite, cap, |l| {
                        sandbox_rank(*l)
                    }),
                }
            }
            None => requested_sandbox.unwrap_or(SandboxLevel::WorkspaceWrite),
        };
        // Auto (full access, never asks) is never wider than the parent: only
        // a chat that is itself on Auto may start an Auto child.
        if args.auto_approve
            && let Some(caller) = &caller
            && !caller.config.as_ref().is_some_and(|c| c.auto_approve)
        {
            anyhow::bail!(
                "autoApprove refused: a child chat can only be Auto when the calling chat is \
                 on Auto (never wider than its parent); set this chat's approval to Auto, or \
                 omit autoApprove to start the child on Ask"
            );
        }
        // Never leave the model empty: the harness CLI would fall back to its
        // own default (e.g. `~/.claude/settings.json`), not the composer's.
        let model = mast_proto::resolve_model(harness, args.model.as_deref());
        if args.model.as_deref().is_none_or(|m| m.trim().is_empty()) {
            tracing::debug!(?harness, model = ?model, "create_chat had no model; filled in the Mast default");
        }
        let mut model_options = serde_json::Map::new();
        if let Some(window) = &args.context_window {
            let window = match window {
                Value::String(s) => s.trim().to_owned(),
                Value::Number(n) => n.to_string(),
                _ => anyhow::bail!("contextWindow must be a token count or \"default\""),
            };
            let models = self.zeron.models(harness).await?;
            let choices: Vec<&str> = models
                .iter()
                .filter(|m| Some(&m.id) == model.as_ref())
                .flat_map(|m| &m.options)
                .filter(|o| o.id == "contextWindow")
                .flat_map(|o| &o.choices)
                .map(|c| c.id.as_str())
                .collect();
            if !choices.contains(&window.as_str()) {
                anyhow::bail!(
                    "contextWindow {window:?} is not offered by {harness:?} for this model; \
                     available: {} (see list_models)",
                    if choices.is_empty() { "none".to_owned() } else { choices.join(", ") }
                );
            }
            model_options.insert("contextWindow".into(), Value::String(window));
        }
        let config = ChatConfig {
            approval_rules: false,
            harness,
            model: model.clone(),
            reasoning,
            model_options,
            sandbox,
            // Ask unless requested, and then only under an Auto parent
            // (checked above; the engine re-checks it).
            auto_approve: args.auto_approve,
        };

        let (space, device_id) = match (origin, args.project.as_deref()) {
            // The personal assistant: any Space (on that Space's device), or
            // no Space on any device.
            (Some(origin), Some(project)) if origin.personal_assistant => {
                let space = self.zeron.resolve_space(project).await?;
                if let Some(device) = args.device.as_deref()
                    && self.zeron.resolve_device_id(Some(device)).await? != space.device_id
                {
                    anyhow::bail!(
                        "that Space is on another device; omit device or name the Space's own"
                    );
                }
                let device_id = space.device_id.clone();
                (Some(space), device_id)
            }
            (Some(origin), None) if origin.personal_assistant && args.device.is_some() => (
                None,
                self.zeron.resolve_device_id(args.device.as_deref()).await?,
            ),
            // Agent mode: the token's Space on the token's device, only.
            (Some(origin), project) => {
                let space = self
                    .zeron
                    .resolve_space(project.unwrap_or(&origin.space_id))
                    .await?;
                if space.id != origin.space_id {
                    anyhow::bail!(
                        "agents can only create chats in their own Space ({})",
                        short(&origin.space_id)
                    );
                }
                if let Some(device) = args.device.as_deref()
                    && self.zeron.resolve_device_id(Some(device)).await? != origin.device_id
                {
                    anyhow::bail!("agents can only create chats on their own device");
                }
                if space.device_id != origin.device_id {
                    anyhow::bail!("agents can only create chats on their own device");
                }
                (Some(space), origin.device_id.clone())
            }
            (None, Some(project)) => {
                let space = self.zeron.resolve_space(project).await?;
                let device_id = space.device_id.clone();
                (Some(space), device_id)
            }
            (None, None) => (
                None,
                self.zeron.resolve_device_id(args.device.as_deref()).await?,
            ),
        };
        let cwd = args
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        if let (Some(caller), Some(cwd)) = (&caller, &cwd) {
            let roots = [
                space.as_ref().map(|s| s.path.as_str()),
                caller.cwd.as_deref(),
            ];
            if !path_within_any(cwd, &roots) {
                anyhow::bail!(
                    "agents can only start a child chat inside this Space's folder or \
                     this chat's working directory (cwd {cwd:?} is outside both)"
                );
            }
        }

        let mut work_item_id = args
            .work_item_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        // A chat inside a project with a worker destination that starts a
        // worker without `workItemId` gets a NEW item under the destination
        // (titled from the chat title or the prompt) instead of inheriting
        // its own item and nesting under it. Only when it could start for an
        // item anyway (a prompt, no cwd or branch).
        if work_item_id.is_none()
            && let Some(origin) = origin
            // Only in the caller's own Space: its destination is there.
            && space.as_ref().is_some_and(|s| s.id == origin.space_id)
            && cwd.is_none()
            && args.branch.as_deref().is_none_or(|b| b.trim().is_empty())
            && let Some(key) = agent_key.as_deref()
            && let Some(prompt) = args.prompt.as_deref().filter(|p| !p.trim().is_empty())
        {
            let current = self
                .zeron
                .agent_tool(agent_tool_names::READ_CURRENT, json!({}))
                .await?;
            if let Some(destination) = current.get("workerDestinationId").and_then(Value::as_str) {
                let title: String = args
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .or_else(|| prompt.lines().map(str::trim).find(|l| !l.is_empty()))
                    .unwrap_or("Worker")
                    .chars()
                    .take(80)
                    .collect();
                let created = self
                    .zeron
                    .agent_tool(
                        agent_tool_names::CREATE_CHILD,
                        json!({
                            "title": title,
                            "parentWorkItemId": destination,
                            "idempotencyKey": format!(
                                "wd-{}",
                                mast_proto::derive_id(key, "mcp-create-chat-item")
                            ),
                        }),
                    )
                    .await?;
                work_item_id = created
                    .get("workItemId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
        }
        if work_item_id.is_some() {
            if origin.is_none() {
                anyhow::bail!("workItemId is only available inside a Mast chat");
            }
            if cwd.is_some() || args.branch.as_deref().is_some_and(|b| !b.trim().is_empty()) {
                anyhow::bail!(
                    "cwd and branch cannot be set with workItemId: the child runs where the \
                     work item's execution setup says"
                );
            }
        }

        // Parent: the explicit `parent` argument, else the chat this server
        // speaks for. Resolved so a prefix/title works and a typo fails loud.
        // An agent may only record its own Chat (a child inherits the
        // parent's work item and instructions, so another parent would
        // reach into someone else's work).
        let explicit_parent = match args
            .parent
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            Some(key) => Some(self.zeron.resolve_chat(key).await?.id),
            None => None,
        };
        let parent_chat_id = match (origin, explicit_parent) {
            (Some(origin), Some(parent)) if parent != origin.chat_id => {
                anyhow::bail!("agents can only create child chats of their own chat; omit parent")
            }
            (Some(origin), _) => Some(origin.chat_id.clone()),
            (None, parent) => parent,
        };

        let candidate = if !args.new_chat && let (Some(origin), Some(item)) = (origin, work_item_id.as_deref()) {
            Box::pin(self.reusable_chat(origin, item, &harnesses)).await?
        } else { None };

        if work_item_id.is_some() && candidate.is_none()
            && args.prompt.as_deref().is_none_or(|p| p.trim().is_empty())
        {
            anyhow::bail!("workItemId needs a prompt when starting a new chat");
        }

        let (chat_id, replayed) = match (origin, agent_key.as_deref()) {
            (Some(origin), Some(key)) => {
                // Bind the key to these inputs first: a retry that changed its
                // request is a conflict, never the old chat returned silently.
                let inputs = json!({
                    "spaceId": space.as_ref().map(|s| &s.id),
                    "deviceId": device_id,
                    "config": config,
                    "parentChatId": parent_chat_id,
                    "title": args.title.as_deref().map(str::trim),
                    "branch": args.branch.as_deref().map(str::trim),
                    "cwd": cwd,
                    "prompt": args.prompt,
                });
                let mut inputs = inputs;
                if let Some(item) = &work_item_id {
                    // Only when set, so keys claimed before `workItemId`
                    // existed still replay.
                    inputs["workItemId"] = json!(item);
                }
                if args.new_chat { inputs["newChat"] = json!(true); }
                let inputs_hash =
                    mast_proto::derive_id(&inputs.to_string(), "mcp-create-chat-inputs");
                let claim = self.zeron
                    .agent_tool(
                        agent_tool_names::CLAIM_CHILD_CHAT,
                        json!({ "idempotencyKey": key, "inputsHash": inputs_hash,
                            "reuseChatId": candidate.as_ref().map(|c| &c.0.id),
                            "deliveryMode": candidate.as_ref().map(|c| &c.1) }),
                    )
                    .await?;
                let operation_id = agent_tools::operation_id(&origin.chat_id, key);
                if claim["reuseChatId"].is_string() {
                    return Box::pin(self.reuse_chat(origin, &claim, work_item_id.as_deref(), &args, operation_id)).await;
                }
                let chat_id = mast_proto::derive_id(&operation_id, mast_proto::roles::CHAT);
                let existing = self
                    .zeron
                    .chats()
                    .await?
                    .into_iter()
                    .find(|c| c.id == chat_id);
                if let Some(existing) = &existing
                    && existing.parent_chat_id.as_deref() != Some(origin.chat_id.as_str())
                {
                    anyhow::bail!("idempotency_key names a chat this chat did not create");
                }
                (chat_id, existing.is_some())
            }
            _ => (uuid::Uuid::new_v4().to_string(), false),
        };
        if work_item_id.is_some() && args.prompt.as_deref().is_none_or(|p| p.trim().is_empty()) {
            anyhow::bail!("workItemId needs a prompt when starting a new chat");
        }
        // For a work item the engine starts the child through the launch
        // workflow (Chat, assignment, execution binding, effective
        // instructions, first Turn), after checking the item is the
        // caller's or a descendant; ids derive from the same
        // (caller, key) operation, so retries replay.
        let launched = match (&work_item_id, agent_key.as_deref(), &args.prompt) {
            (Some(item), Some(key), Some(prompt)) => {
                let reply = self
                    .zeron
                    .agent_tool(
                        agent_tool_names::LAUNCH_CHILD_CHAT,
                        json!({
                            "idempotencyKey": key,
                            "workItemId": item,
                            "prompt": prompt,
                            "config": config,
                        }),
                    )
                    .await?;
                if reply.get("chatId").and_then(Value::as_str) != Some(chat_id.as_str()) {
                    anyhow::bail!("the engine started an unexpected chat for this request");
                }
                Some(reply)
            }
            _ => None,
        };
        let cwd = match launched
            .as_ref()
            .and_then(|r| r.get("executionRoot"))
            .and_then(Value::as_str)
        {
            Some(root) => Some(root.to_owned()),
            None => cwd,
        };
        if launched.is_none() {
            let mut mutate = json!({
                "op": "createChat",
                "chatId": chat_id,
                "deviceId": device_id,
                "config": config,
            });
            if let Some(space) = &space {
                mutate["spaceId"] = json!(space.id);
            }
            if let Some(parent) = &parent_chat_id {
                mutate["parentChatId"] = json!(parent);
            }
            if let Some(branch) = args
                .branch
                .as_deref()
                .map(str::trim)
                .filter(|b| !b.is_empty())
            {
                mutate["branch"] = json!(branch);
            }
            if let Some(cwd) = &cwd {
                mutate["cwd"] = json!(cwd);
            }
            // `createChat` is a no-op for an existing row (never a retarget).
            self.zeron.mutate(mutate).await?;
        }
        // A replay leaves the title alone (the user may have renamed it).
        if let Some(title) = args
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty() && !replayed)
        {
            self.zeron
                .mutate(json!({ "op": "renameChat", "chatId": chat_id, "title": title }))
                .await?;
        }

        let mut result = json!({
            "chatId": chat_id,
            "url": mast_proto::session_link::session_link(&chat_id),
            "deviceId": device_id,
            "project": space.as_ref().map(|s| json!({ "id": s.id, "name": s.display_name(), "path": s.path })),
            "harness": harness,
            "model": model,
            "reasoning": reasoning,
            "title": args.title,
            "parentChatId": parent_chat_id,
            "replayed": replayed,
        });
        if let Some(item) = &work_item_id {
            result["workItemId"] = json!(item);
            result["executionRoot"] = json!(cwd);
        }
        if let Some(prompt) = args.prompt.filter(|p| !p.trim().is_empty()) {
            // The row may not have folded into WatchChats yet; build the
            // chat locally from what we just wrote rather than re-reading.
            let chat = Chat {
                id: chat_id.clone(),
                device_id: device_id.clone(),
                title: args.title.clone(),
                archived: false,
                cwd: cwd.clone(),
                branch: args.branch.clone(),
                checkout_id: None,
                source_context: None,
                config: Some(config),
                last_message_preview: None,
                last_message_at: None,
                created_at: chrono::Utc::now(),
                harness_session_id: None,
                harness_session_cwd: None,
                actual_model: None,
                scheduled_task: None,
                scheduled_task_info: None,
                standing_role_successor: None,
                parent_chat_id: parent_chat_id.clone(),
                side_of: None,
                side_absorbed_through: None,
                space_id: space.as_ref().map(|s| s.id.clone()),
                last_seen_at: None,
                room_gen: None,
                linked_pr_url: None,
                linked_pr_source: None,
                linked_ticket_id: None,
                linked_ticket_source: None,
            };
            let sent = match agent_key.as_deref() {
                // The launch already queued the first Turn.
                Some(_) if launched.is_some() => {
                    let launch = launched.as_ref();
                    json!({
                        "delivery": "run",
                        "id": launch.and_then(|r| r.get("commandId")).cloned().unwrap_or(Value::Null),
                        "state": launch.and_then(|r| r.get("state")).cloned().unwrap_or(Value::Null),
                        "detail": launch.and_then(|r| r.get("detail")).cloned().unwrap_or(Value::Null),
                        "chatStatusAtSend": "idle",
                    })
                }
                // The engine derives the message and command ids from the
                // key, queues at most one first Turn per key, and builds the
                // Run from the child's stored config (only the prompt is ours).
                Some(key) => {
                    let queued = self
                        .zeron
                        .agent_tool(
                            agent_tool_names::QUEUE_CHILD_CHAT_TURN,
                            json!({
                                "chatId": chat.id,
                                "idempotencyKey": key,
                                "prompt": prompt,
                            }),
                        )
                        .await?;
                    json!({
                        "delivery": "run",
                        "id": queued.get("commandId").cloned().unwrap_or(Value::Null),
                        "queued": queued.get("queued").cloned().unwrap_or(Value::Null),
                        "chatStatusAtSend": "idle",
                    })
                }
                None => {
                    self.deliver(&chat, space.as_ref(), &harnesses, None, prompt, "run", None)
                        .await?
                }
            };
            result["sent"] = sent;
            if args.wait {
                result["turn"] = self
                    .await_turn(&chat, None, true, args.timeout_secs, now_millis())
                    .await?;
            }
        }
        Ok(result)
    }

    async fn read_chat(
        &self,
        origin: Option<&AgentOrigin>,
        args: ReadChatArgs,
    ) -> anyhow::Result<Value> {
        let chat = self
            .reach_chat(origin, &args.chat, Reach::ReadScope)
            .await?;
        let entries = self.zeron.transcript(&chat.id).await?;
        let rendered = render_entries(
            &entries,
            RenderOptions {
                include_reasoning: args.include_reasoning,
                include_tools: args.include_tools.unwrap_or(true),
            },
        );
        let total = rendered.len();
        let limit = args.limit.unwrap_or(40).clamp(1, 500);
        let end = total.saturating_sub(args.offset);
        let start = end.saturating_sub(limit);
        let (window, dropped) = cap_messages(
            rendered[start..end].to_vec(),
            MAX_MESSAGE_BYTES,
            MAX_RESULT_BYTES,
        );
        let start = start + dropped;
        let truncated = dropped > 0 || window.iter().any(|m| m.text_omitted_bytes.is_some());
        let mut result = json!({
            "chatId": chat.id,
            "url": mast_proto::session_link::session_link(&chat.id),
            "title": chat.title,
            "total": total,
            "returned": window.len(),
            "olderRemaining": start,
            "newerSkipped": total - end,
            "pendingInput": last_pending_input(&rendered),
            "messages": window,
        });
        if truncated {
            result["truncated"] = json!(true);
            result["nextOffset"] = json!(total - start);
        }
        Ok(result)
    }

    async fn send_message(
        &self,
        origin: Option<&AgentOrigin>,
        args: SendArgs,
    ) -> anyhow::Result<Value> {
        let text = args.text.trim();
        if text.is_empty() {
            anyhow::bail!("text is empty");
        }
        let chat = self
            .reach_chat(origin, &args.chat, Reach::MessageScope)
            .await?;
        if origin.is_some_and(|o| o.chat_id == chat.id) {
            anyhow::bail!(
                "refusing to send a message to your own chat ({})",
                short(&chat.id)
            );
        }
        let caller = match origin {
            Some(origin) => Some(
                self.zeron
                    .chats()
                    .await?
                    .into_iter()
                    .find(|c| c.id == origin.chat_id)
                    .ok_or_else(|| anyhow::anyhow!("this agent's chat no longer exists"))?,
            ),
            None => None,
        };
        let caller_parent = caller.as_ref().and_then(|c| c.parent_chat_id.clone());
        if let Some(origin) = origin
            && chat.parent_chat_id.as_deref() != Some(origin.chat_id.as_str())
            && caller_parent.as_deref() != Some(chat.id.as_str())
        {
            let caller = caller.as_ref().expect("agent caller was resolved");
            let caller_sandbox = caller
                .config
                .as_ref()
                .map(|c| c.sandbox)
                .unwrap_or(SandboxLevel::WorkspaceWrite);
            let target_sandbox = chat
                .config
                .as_ref()
                .map(|c| c.sandbox)
                .unwrap_or(SandboxLevel::WorkspaceWrite);
            if sandbox_rank(target_sandbox) > sandbox_rank(caller_sandbox) {
                anyhow::bail!(
                    "target chat sandbox {} is more permissive than your sandbox {}; messaging it is refused",
                    sandbox_name(target_sandbox),
                    sandbox_name(caller_sandbox)
                );
            }
            if chat.config.as_ref().is_some_and(|c| c.auto_approve)
                && !caller.config.as_ref().is_some_and(|c| c.auto_approve)
            {
                anyhow::bail!(
                    "target chat uses auto-approval but your chat does not; messaging it is refused"
                );
            }
        }
        if let Some(origin) = origin
            && chat.parent_chat_id.as_deref() != Some(origin.chat_id.as_str())
            && caller_parent.as_deref() == Some(chat.id.as_str())
        {
            // Not one of its children, so reach_chat let it through as the
            // caller's parent (an admin chat's other reach is an ordinary send).
            if args.wait {
                anyhow::bail!(
                    "wait is not supported when messaging your parent chat; the report is queued"
                );
            }
            let sent = self.send_report(origin, text).await?;
            return Ok(json!({ "chatId": chat.id, "title": chat.title, "sent": sent }));
        }
        let (spaces, sessions, harnesses) = tokio::try_join!(
            self.zeron.spaces(),
            self.zeron.sessions(),
            self.zeron.harnesses()
        )?;
        let space = chat
            .space_id
            .as_deref()
            .and_then(|id| spaces.iter().find(|s| s.id == id));
        let baseline = session_for(&sessions, &chat);
        let mode = args.mode.as_deref().unwrap_or("auto");
        let sent_at = now_millis();
        let body = self.attribute(origin, &chat, text).await;
        let mut result = json!({
            "chatId": chat.id,
            "title": chat.title,
        });
        result["sent"] = self
            .deliver(&chat, space, &harnesses, baseline.as_ref(), body, mode, args.operation_id.as_deref())
            .await?;
        if args.wait {
            result["turn"] = self
                .await_turn(&chat, baseline.as_ref(), true, args.timeout_secs, sent_at)
                .await?;
        }
        Ok(result)
    }

    async fn report_to_parent(
        &self,
        origin: Option<&AgentOrigin>,
        args: ReportArgs,
    ) -> anyhow::Result<Value> {
        let Some(origin) = origin else {
            anyhow::bail!("report_to_parent is only available to agents Mast started in a chat");
        };
        let text = args.text.trim();
        if text.is_empty() {
            anyhow::bail!("text is empty");
        }
        if let Some(handback) = args.final_handback {
            let sent = self.zeron.agent_tool(agent_tool_names::FINAL_HANDBACK, json!({
                "expectedGeneration": handback.assignment_generation,
                "idempotencyKey": handback.idempotency_key,
                "keepChat": handback.keep_chat,
                "report": text,
            })).await?;
            return Ok(json!({ "parentChatId": sent["parentChatId"], "sent": sent }));
        }
        let me = self.zeron.resolve_chat(&origin.chat_id).await?;
        let Some(parent) = me.parent_chat_id.as_deref() else {
            anyhow::bail!(
                "this chat has no parent chat (no chat started it), so there is nobody to report to"
            );
        };
        let sent = self.send_report(origin, text).await?;
        Ok(json!({ "parentChatId": parent, "sent": sent }))
    }

    /// Queue `text` as a worker report to the caller's parent (engine side:
    /// the wake queue, same Space, parent's host device).
    async fn send_report(&self, origin: &AgentOrigin, text: &str) -> anyhow::Result<Value> {
        self.zeron
            .call(
                methods::REPORT_TO_PARENT,
                json!({ "chatId": origin.chat_id, "text": text }),
            )
            .await
    }

    async fn wait_for_turn(
        &self,
        origin: Option<&AgentOrigin>,
        args: WaitArgs,
    ) -> anyhow::Result<Value> {
        let chat = self
            .reach_chat(origin, &args.chat, Reach::CommunicationScope)
            .await?;
        let turn = self
            .await_turn(&chat, None, false, args.timeout_secs, 0)
            .await?;
        Ok(json!({ "chatId": chat.id, "title": chat.title, "turn": turn }))
    }

    async fn archive_chat(
        &self,
        origin: Option<&AgentOrigin>,
        args: ArchiveArgs,
    ) -> anyhow::Result<Value> {
        let archived = args.archived.unwrap_or(true);
        if origin.is_some() {
            let chat = self
                .reach_chat(origin, &args.chat, Reach::ArchiveScope)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "agents can only archive chats they created and the workers in their \
                         project (admin chats: any chat of their Space): {e}"
                    )
                })?;
            return self
                .zeron
                .agent_tool(
                    agent_tool_names::CHAT_ARCHIVE,
                    json!({
                        "chatId": chat.id, "archived": archived,
                        "force": args.force, "reason": args.reason,
                    }),
                )
                .await;
        }
        let chat = self.reach_chat(origin, &args.chat, Reach::Child).await?;
        self.zeron
            .mutate(json!({ "op": "setChatArchived", "chatId": chat.id, "archived": archived }))
            .await?;
        Ok(json!({ "chatId": chat.id, "title": chat.title, "archived": archived }))
    }

    async fn interrupt_chat(
        &self,
        origin: Option<&AgentOrigin>,
        args: ChatArgs,
    ) -> anyhow::Result<Value> {
        let chat = self.reach_chat(origin, &args.chat, Reach::Child).await?;
        let command_id = self
            .zeron
            .queue_command(&chat.id, &SessionCommandPayload::Interrupt {})
            .await?;
        Ok(json!({ "chatId": chat.id, "commandId": command_id }))
    }

    async fn respond_to_input(
        &self,
        origin: Option<&AgentOrigin>,
        args: RespondArgs,
    ) -> anyhow::Result<Value> {
        let chat = self.reach_chat(origin, &args.chat, Reach::Child).await?;
        let entries = self.zeron.transcript(&chat.id).await?;
        let rendered = render_entries(&entries, RenderOptions::default());
        let pending = last_pending_input(&rendered)
            .ok_or_else(|| anyhow::anyhow!("chat {} has no pending question", short(&chat.id)))?;
        let request_id = pending
            .get("requestId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("pending input has no request id"))?
            .to_owned();
        if args.request_id.as_ref().is_some_and(|id| id != &request_id) {
            anyhow::bail!("request id does not match pending request {request_id}");
        }
        let questions: Vec<mast_proto::UserInputQuestion> = serde_json::from_value(
            pending
                .get("questions")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("pending input has no questions"))?,
        )?;
        let mut answers: Vec<UserInputAnswer> = args
            .answers
            .into_iter()
            .map(|a| UserInputAnswer {
                question_id: a.question_id.unwrap_or_default(),
                labels: a.labels,
            })
            .collect();
        mast_proto::validate_input_answers(&questions, &mut answers).map_err(anyhow::Error::msg)?;
        // A saved permission rule is the user's decision, never an agent's.
        if answers.iter().any(|a| {
            a.labels
                .iter()
                .any(|l| mast_proto::is_always_allow_label(l))
        }) {
            anyhow::bail!("only the user can choose Always allow; answer Yes or No");
        }
        let command_id = self
            .zeron
            .queue_command(
                &chat.id,
                &SessionCommandPayload::RespondInput {
                    request_id: request_id.clone(),
                    answers,
                },
            )
            .await?;
        Ok(json!({ "chatId": chat.id, "requestId": request_id, "commandId": command_id }))
    }

    // ---- shared pieces -------------------------------------------------------

    /// Verdicts for `chats` when this server speaks for a Chat (only the ones
    /// in its archive scope get one).
    async fn chat_archive_info(
        &self,
        origin: Option<&AgentOrigin>,
        chats: &[&Chat],
    ) -> std::collections::HashMap<String, Value> {
        if origin.is_none() || chats.is_empty() {
            return Default::default();
        }
        let ids: Vec<String> = chats.iter().map(|c| c.id.clone()).collect();
        self.archive_info(&ids).await.unwrap_or_default()
    }

    /// The engine's finished-worker verdicts (`archivable`, `reason`) for
    /// `ids`, or for every chat in the caller's archive scope when empty.
    async fn archive_info(
        &self,
        ids: &[String],
    ) -> Option<std::collections::HashMap<String, Value>> {
        let reply = self
            .zeron
            .agent_tool(
                agent_tool_names::CHAT_ARCHIVE_INFO,
                json!({ "chatIds": ids }),
            )
            .await
            .ok()?;
        serde_json::from_value(reply["chats"].clone()).ok()
    }

    /// Resolve a chat key. Without a token: any chat. With one: only among
    /// the chats the token's Chat may reach (so a foreign chat is neither
    /// targeted nor revealed by an ambiguity error).
    async fn reach_chat(
        &self,
        origin: Option<&AgentOrigin>,
        key: &str,
        reach: Reach,
    ) -> anyhow::Result<Chat> {
        let Some(origin) = origin else {
            return self.zeron.resolve_chat(key).await;
        };
        let allow_self = !matches!(
            reach,
            Reach::Child | Reach::ArchiveScope | Reach::CommunicationScope
        );
        // Admin chats reach their whole Space for everything but interrupt/respond.
        let admin_ok = reach != Reach::Child;
        let all = self.zeron.chats().await?;
        // The personal assistant reaches every chat of every Space (itself
        // only where the reach allows it).
        if origin.personal_assistant {
            let chats: Vec<Chat> = all
                .into_iter()
                .filter(|c| allow_self || c.id != origin.chat_id)
                .collect();
            return resolve_chat_in(&chats, key);
        }
        // A project manager reaches only the chats of its own project (and
        // itself where the reach allows it), never a child, parent or
        // project route that points elsewhere. Archived ones only where that
        // is the point (reading, listing, archiving and un-archiving).
        if origin.manager_scope_item_id.is_some() {
            let managed = self.manager_chat_ids(origin).await;
            let archived_ok = matches!(reach, Reach::ReadScope | Reach::SelfOrChild | Reach::ArchiveScope);
            let chats: Vec<Chat> = all
                .into_iter()
                .filter(|c| {
                    (allow_self && c.id == origin.chat_id)
                        || (managed.contains(&c.id) && (archived_ok || !c.archived))
                })
                .collect();
            return resolve_chat_in(&chats, key).map_err(|_| {
                anyhow::anyhow!("no chat matches {key:?}: a project manager reaches only the chats of its own project")
            });
        }
        let parent = (reach == Reach::MessageScope)
            .then(|| {
                all.iter()
                    .find(|c| c.id == origin.chat_id)
                    .and_then(|c| c.parent_chat_id.clone())
            })
            .flatten();
        let mut handoff_sources: Vec<String> = Vec::new();
        let project_chats: Vec<String> = if matches!(reach, Reach::CommunicationScope | Reach::MessageScope | Reach::ReadScope) {
            self.project_scope()
                .await
                .and_then(|v| {
                    handoff_sources = serde_json::from_value(v["handoffSourceChatIds"].clone()).unwrap_or_default();
                    serde_json::from_value(v["chatIds"].clone()).ok()
                })
                .unwrap_or_default()
        } else if reach == Reach::ArchiveScope {
            self.archive_info(&[])
                .await
                .map(|info| info.into_keys().collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let chats: Vec<Chat> = all
            .into_iter()
            .filter(|c| {
                if matches!(reach, Reach::CommunicationScope | Reach::MessageScope)
                    && c.id == origin.chat_id
                {
                    return false;
                }
                reachable(origin, c, allow_self, admin_ok)
                    || (reach == Reach::ArchiveScope && admin_reaches(origin, c))
                    || (!c.archived
                        && c.space_id.as_deref() == Some(origin.space_id.as_str())
                        && parent.as_deref() == Some(c.id.as_str()))
                    || (!c.archived && project_chats.contains(&c.id))
                    || (reach == Reach::ReadScope
                        && c.space_id.as_deref() == Some(origin.space_id.as_str())
                        && handoff_sources.contains(&c.id))
            })
            .collect();
        resolve_chat_in(&chats, key).map_err(|_| {
            if matches!(reach, Reach::CommunicationScope | Reach::MessageScope | Reach::ReadScope) {
                let extra = match reach { Reach::MessageScope => ", and your parent", Reach::ReadScope => ", and your own chat", _ => "" };
                return anyhow::anyhow!(
                    "no chat matches {key:?}: you can reach chats you created and non-archived chats on your item subtree or in your project{extra}"
                );
            }
            anyhow::anyhow!(
                "no chat this chat created{} matches {key:?}; agents can only reach the chats \
                 they created (list_chats shows them){}",
                if allow_self {
                    " (or this chat itself)"
                } else {
                    ""
                },
                if reach == Reach::ArchiveScope {
                    ", and archive the workers in their own project"
                } else {
                    ""
                }
            )
        })
    }

    /// The chats of the project the caller manages right now, from the
    /// engine (empty for anyone else, and when the engine predates the
    /// grant). The engine re-derives it from the role record on every call.
    async fn manager_chat_ids(&self, origin: &AgentOrigin) -> Vec<String> {
        if origin.manager_scope_item_id.is_none() {
            return Vec::new();
        }
        self.project_scope()
            .await
            .and_then(|v| serde_json::from_value(v["managerChatIds"].clone()).ok())
            .unwrap_or_default()
    }

    /// The engine's project-scope reply, one call (`chatIds`,
    /// `handoffSourceChatIds`, and `managerChatIds` for a manager token).
    async fn project_scope(&self) -> Option<Value> {
        self.zeron
            .agent_tool(agent_tool_names::PROJECT_SCOPE_CHATS, json!({}))
            .await
            .ok()
    }

    /// Prefix the sender's identity when this server speaks for a chat, so
    /// the receiving agent (and the human reading that transcript) can tell
    /// an agent-to-agent message from a typed one.
    async fn attribute(&self, origin: Option<&AgentOrigin>, target: &Chat, text: &str) -> String {
        let Some(origin_id) = origin.map(|o| o.chat_id.as_str()) else {
            return text.to_owned();
        };
        if origin_id == target.id {
            return text.to_owned();
        }
        let title = match self.zeron.resolve_chat(origin_id).await {
            Ok(chat) => chat.title,
            Err(_) => None,
        };
        let label = match title {
            Some(title) if !title.trim().is_empty() => {
                format!("{} ({})", title.trim(), short(origin_id))
            }
            _ => short(origin_id).to_owned(),
        };
        let admin = if origin.is_some_and(|o| o.personal_assistant) {
            " (the user's personal assistant chat; this is not the user's own message)"
        } else if origin.is_some_and(|o| o.admin_space_id.is_some()) {
            " (an admin chat, granted by Mikey; this is not Mikey's own message)"
        } else if origin.is_some_and(|o| o.manager_scope_item_id.is_some()) {
            " (the project's manager chat; this is not the user's own message)"
        } else {
            ""
        };
        format!(
            "{} {label}{admin}. Reply to it with the Mast `send_message` tool, chat {}.]\n\n{text}",
            mast_proto::RELAYED_MESSAGE_PREFIX,
            origin_id
        )
    }

    /// Pick and perform the delivery the composer would.
    async fn deliver(
        &self,
        chat: &Chat,
        space: Option<&Space>,
        harnesses: &[HarnessInfo],
        session: Option<&Session>,
        text: String,
        mode: &str,
        operation_id: Option<&str>,
    ) -> anyhow::Result<Value> {
        let harness = chat
            .config
            .as_ref()
            .map(|c| c.harness)
            .map_or_else(|| default_harness(harnesses), Ok)?;
        let info = harnesses.iter().find(|h| h.id == harness);
        let (status, _) = status_of(session);
        let live = session
            .map(|s| {
                let stale = chrono::Utc::now() - s.updated_at > SESSION_STALE;
                if stale && s.status == SessionStatus::Working {
                    SessionStatus::Idle
                } else {
                    s.status
                }
            })
            .unwrap_or(SessionStatus::Idle);
        let chosen = match mode {
            "deferred_auto" if operation_id.is_some() => "auto",
            "auto" => match live {
                SessionStatus::Idle | SessionStatus::Errored => "run",
                SessionStatus::Working => {
                    if info.is_some_and(HarnessInfo::steers_mid_turn) {
                        "steer"
                    } else {
                        "queue"
                    }
                }
                SessionStatus::AwaitingInput => anyhow::bail!(
                    "chat {} is waiting for an answer; use respond_to_input (or mode 'queue' to hold this message for after the turn)",
                    short(&chat.id)
                ),
            },
            "run" | "steer" | "queue" => mode,
            other => anyhow::bail!("unknown mode {other:?}; expected auto, run, steer, or queue"),
        };
        let message_id = operation_id.map(|op| mast_proto::derive_id(op, mast_proto::roles::MESSAGE));
        let command_id = operation_id.map(|op| mast_proto::derive_id(op, mast_proto::roles::COMMAND));
        let id = if let (Some(message_id), Some(command_id)) = (message_id, command_id) {
            match chosen {
                "auto" => {
                    let command = SessionCommandPayload::AutoMessage {
                        request: run_request(chat, space, harness, text), message_id, instructions: None,
                    };
                    self.zeron.call(methods::QUEUE_COMMAND, json!({"chatId": chat.id, "commandId": command_id, "command": command})).await?;
                    command_id
                }
                "run" | "steer" => {
                    let command = if chosen == "run" {
                        SessionCommandPayload::Run { request: run_request(chat, space, harness, text), message_id }
                    } else { SessionCommandPayload::Steer { prompt: text, message_id: Some(message_id) } };
                    self.zeron.call(methods::QUEUE_COMMAND, json!({"chatId": chat.id, "commandId": command_id, "command": command})).await?;
                    command_id
                }
                _ => {
                    self.zeron.call(methods::QUEUE_MESSAGE, json!({"chatId": chat.id, "messageId": message_id, "text": text, "holdForTurnEnd": true})).await?;
                    message_id
                }
            }
        } else { match chosen {
            "run" => {
                let request = run_request(chat, space, harness, text);
                self.zeron
                    .queue_command(
                        &chat.id,
                        &SessionCommandPayload::Run {
                            request,
                            message_id: uuid::Uuid::new_v4().to_string(),
                        },
                    )
                    .await?
            }
            "steer" => {
                self.zeron
                    .queue_command(
                        &chat.id,
                        &SessionCommandPayload::Steer {
                            prompt: text,
                            message_id: Some(uuid::Uuid::new_v4().to_string()),
                        },
                    )
                    .await?
            }
            _ => self.zeron.queue_message(&chat.id, &text).await?,
        }};
        Ok(json!({
            "delivery": chosen,
            "id": id,
            "chatStatusAtSend": status,
        }))
    }

    /// Wait, then report the outcome with the assistant messages that
    /// landed since `since_millis`.
    async fn await_turn(
        &self,
        chat: &Chat,
        baseline: Option<&Session>,
        expect_turn: bool,
        requested_secs: Option<u64>,
        since_millis: i64,
    ) -> anyhow::Result<Value> {
        let started = std::time::Instant::now();
        let (outcome, session) = self
            .zeron
            .wait_for_turn(chat, baseline, expect_turn, wait_duration(requested_secs))
            .await?;
        let elapsed_secs = started.elapsed().as_secs();
        let entries = self.zeron.transcript(&chat.id).await.unwrap_or_default();
        let rendered = render_entries(&entries, RenderOptions::default());
        let replies: Vec<&RenderedMessage> = rendered
            .iter()
            .filter(|m| m.role == mast_doc::MessageRole::Assistant)
            .filter(|m| m.created_at >= since_millis.saturating_sub(2_000))
            .collect();
        let replies: Vec<&RenderedMessage> = if replies.is_empty() {
            rendered
                .iter()
                .rev()
                .find(|m| m.role == mast_doc::MessageRole::Assistant)
                .into_iter()
                .collect()
        } else {
            replies
        };
        let (status, _) = status_of(session.as_ref());
        let pending_input = last_pending_input(&rendered);
        let working = session
            .as_ref()
            .is_some_and(|s| s.status == SessionStatus::Working);
        let (replies, dropped) = cap_messages(
            replies.into_iter().cloned().collect(),
            MAX_MESSAGE_BYTES,
            MAX_RESULT_BYTES,
        );
        let truncated = dropped > 0 || replies.iter().any(|m| m.text_omitted_bytes.is_some());
        let mut turn = json!({
            "outcome": outcome,
            "status": status,
        "awaitingInput": session.as_ref().and_then(|s| s.awaiting_input.as_ref()),
        "lastTurnToolFailures": session.as_ref().and_then(|s| s.last_turn_tool_failures.as_ref()),
            "timedOut": outcome == TurnOutcome::TimedOut,
            "pendingInput": pending_input,
            "replies": replies,
        });
        if outcome == TurnOutcome::TimedOut && working {
            let session = session.as_ref().expect("working implies a session");
            turn["status"] = json!("stillRunning");
            turn["turnId"] = json!(format!(
                "{}:{}",
                chat.id,
                session.started_at.map_or(0, |t| t.timestamp_millis())
            ));
            turn["elapsedSecs"] = json!(elapsed_secs);
            turn["hint"] = json!("The turn is still running; call wait_for_turn again.");
        }
        if wait_was_capped(requested_secs) && outcome == TurnOutcome::TimedOut {
            turn["waitCappedAtSecs"] = json!(WAIT_CAP.as_secs());
        }
        if truncated {
            turn["truncated"] = json!(true);
            turn["truncatedHint"] = json!(
                "Only the tail of a long reply is returned; read earlier messages with read_chat."
            );
        }
        Ok(turn)
    }
}

/// Which chats an agent-mode call may target. The personal assistant
/// reaches every chat for each of these (see `reach_chat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Only chats the caller created (`parentChatId` = the caller).
    Child,
    /// Those, or the caller itself (`get_chat`, the `list_chats` parent
    /// filter). An admin chat also reaches every chat of its Space.
    SelfOrChild,
    /// Children and non-archived chats in the caller's item or project scope.
    CommunicationScope,
    /// Communication scope and the caller's parent.
    MessageScope,
    /// Communication scope and the caller itself.
    ReadScope,
    /// `archive_chat`: children, the workers in the caller's project scope, or
    /// (admin chats) any chat of the Space.
    ArchiveScope,
}

/// Whether the token's Chat may reach `chat`: one it created (Mast-created
/// children carry the creating Chat as `parentChatId`), or itself.
fn reachable(origin: &AgentOrigin, chat: &Chat, allow_self: bool, admin_ok: bool) -> bool {
    chat.parent_chat_id.as_deref() == Some(origin.chat_id.as_str())
        || (allow_self && chat.id == origin.chat_id)
        || (admin_ok && !chat.archived && admin_reaches(origin, chat))
}

/// An admin chat (a live user grant, re-checked at every authenticate) reaches
/// the chats of its granted Space, never another Space's.
fn admin_reaches(origin: &AgentOrigin, chat: &Chat) -> bool {
    origin
        .admin_space_id
        .as_deref()
        .is_some_and(|space| chat.space_id.as_deref() == Some(space))
}

/// Permissiveness order of sandbox levels.
fn sandbox_rank(level: SandboxLevel) -> u8 {
    match level {
        SandboxLevel::ReadOnly => 0,
        SandboxLevel::WorkspaceWrite => 1,
        SandboxLevel::DangerFullAccess => 2,
    }
}

fn sandbox_name(level: SandboxLevel) -> &'static str {
    match level {
        SandboxLevel::ReadOnly => "read-only",
        SandboxLevel::WorkspaceWrite => "workspace-write",
        SandboxLevel::DangerFullAccess => "danger-full-access",
    }
}

/// Whether `path` is absolute, has no `..`, and lies at or under one of
/// `roots` (component-wise).
fn path_within_any(path: &str, roots: &[Option<&str>]) -> bool {
    let path = std::path::Path::new(path);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    roots
        .iter()
        .flatten()
        .map(std::path::Path::new)
        .any(|root| root.is_absolute() && path.starts_with(root))
}

/// The Run a composer send would queue for `chat` (its own config and cwd;
/// the host overwrites the cwd of an execution-bound Chat).
fn run_request(chat: &Chat, space: Option<&Space>, harness: HarnessId, text: String) -> RunRequest {
    let config = chat.config.clone();
    let cwd = chat
        .cwd
        .clone()
        .or_else(|| space.map(|s| s.path.clone()))
        .unwrap_or_else(|| "~".into());
    RunRequest { scheduled_task: None, scheduled_notion_writes: None, approval_rules: config.as_ref().is_some_and(|c| c.approval_rules),
        prompt: text,
        harness: Some(harness),
        model: config.as_ref().and_then(|c| c.model.clone()),
        reasoning: config.as_ref().and_then(|c| c.reasoning),
        model_options: config
            .as_ref()
            .map(|c| c.model_options.clone())
            .unwrap_or_default(),
        cwd,
        sandbox: config
            .as_ref()
            .map(|c| c.sandbox)
            .unwrap_or(SandboxLevel::WorkspaceWrite),
        // The chat's own approval mode (default Ask).
        auto_approve: config.as_ref().is_some_and(|c| c.auto_approve),
        resume: None,
        fork_from: None,
        attachments: Vec::new(),
        worktree: None,
        instructions: None,
    }
}

/// claude-code when it is offered here, else the first available harness.
fn default_harness(harnesses: &[HarnessInfo]) -> anyhow::Result<HarnessId> {
    if harnesses.is_empty() {
        return Ok(HarnessId::ClaudeCode);
    }
    harnesses
        .iter()
        .find(|h| h.id == HarnessId::ClaudeCode && h.available())
        .or_else(|| {
            harnesses
                .iter()
                .find(|h| h.available() && h.id != HarnessId::Mock)
        })
        .map(|h| h.id)
        .ok_or_else(|| {
            anyhow::anyhow!("no harness is available on this device (see list_harnesses)")
        })
}

#[cfg(test)]
mod tests;
