//! MCP stdio transport: newline-delimited JSON-RPC 2.0 on stdin/stdout.
//!
//! Requests are handled concurrently (a `wait_for_turn` may block for
//! minutes while the client keeps pinging); responses are serialized through
//! one writer task so frames never interleave. Notifications from the client
//! (`notifications/initialized`, `notifications/cancelled`) are accepted and
//! ignored — there is no server-side state to initialize or cancel.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use mast_proto::AgentRoleProfile;

use crate::tools::Tools;
use crate::tools::tool_catalog::{ToolCatalogProfile, tool_visible, worker_catalog};

/// Protocol revisions this server knows. The client's requested revision
/// is echoed when it is one of these; otherwise the newest is offered and
/// the client decides whether to continue (per the MCP handshake rules).
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const LATEST_PROTOCOL: &str = "2025-11-25";

/// Claude Code truncates MCP server instructions at about 2000 characters, so
/// this text must stay under 1900 characters (checked by a test) with the
/// guidance every agent needs first. Detail that does not fit belongs in the
/// tool descriptions (`tools.rs`).
const INSTRUCTIONS: &str = "\
Mast runs coding agents in Space chats.\n\
If you hit a bug, a failed tool call, or need a workaround, report it with `report_friction`.\n\
\n\
Worktrees: after `git worktree add` or resuming a worktree for \
this chat, call `bind_chat_worktree` with its absolute top-level \
path.\n\
\n\
Blocked decisions: if your chat has a work item \
(`work_outline_read_current`) and is blocked on a decision, approval, or \
missing information from the user, call \
`work_outline_create_user_action_request` (one-line question, context in \
`detail`, a `dedupeKey` per topic; optional `kind`, `links`, `facts`) as well \
as asking in chat; resolve it \
with `work_outline_resolve_user_action_request` once answered. Use built-in questions for small clarifications.\n\
\n\
Planning artifacts: publish plans, docs and mockups you want the user to \
review with `artifact_publish`, linking the returned \
`mast-artifact:<id>` URL, never a path. Under a work item, call \
`artifact_get_plan` at chat start. After changing a visual artifact run \
`artifact_render_check` and fix what it reports.\n\
\n\
Chats: reference by id, unique prefix, or exact title; `whoami` is yours. `send_message` with `wait: true` (or `wait_for_turn` later) returns \
the reply once that turn finishes; answer an `awaitingInput` chat with \
`respond_to_input`.\n\
\n\
Work Outline tools (`work_outline_*`) manage your item and descendants. Mutations and `create_chat` need an idempotency key: reuse it for retries, \
use a new key for a new change.\n\
\n\
Coordinating: `create_chat` with `workItemId` (your item or a descendant) \
and a `prompt` runs work. Workers can use `report_to_parent`; use \
`wait_for_turn` / `read_chat` or `work_outline_read_subtree` for requests. Workers can raise a `kind: handoff` request when ready; archive finished workers (`archivable: true`).\n\
\n\
Admin (user-granted only): `work_outline_admin` batches Space-wide outline \
changes; bulk ones need the user's approval first.";

const WORKER_INSTRUCTIONS: &str = "\
Mast runs coding agents in Space chats. Report friction with `report_friction`.
\
After git worktree add or when resuming, call `bind_chat_worktree` with the absolute top-level path.
\
Read your work item with `work_outline_read_current` and its plan with `artifact_get_plan`. Publish user-facing files with `artifact_publish`, link the returned URL, and run `artifact_render_check` after visual changes.
\
For a blocked decision, approval or missing input, ask in chat and use `work_outline_create_user_action_request` with a one-line request, context in detail and a stable dedupeKey. Use `work_outline_resolve_user_action_request` when answered.
\
Reuse the same idempotency key when retrying a mutation. Send your parent a compact `report_to_parent` of at most 5 lines using STATUS / RESULT / NEXT.";

#[derive(Clone, Copy, Default)]
struct CatalogSelection {
    profile: ToolCatalogProfile,
    role: Option<AgentRoleProfile>,
}

impl CatalogSelection {
    fn from_env() -> Self {
        let profile = match std::env::var("MAST_MCP_CATALOG").as_deref() {
            Ok("compact") => ToolCatalogProfile::Compact,
            _ => ToolCatalogProfile::Full,
        };
        let role = match std::env::var("MAST_AGENT_ROLE").as_deref() {
            Ok("worker") => Some(AgentRoleProfile::Worker),
            Ok("lead") => Some(AgentRoleProfile::Lead),
            Ok("manager") => Some(AgentRoleProfile::Manager),
            Ok("admin") => Some(AgentRoleProfile::Admin),
            Ok("verifier") => Some(AgentRoleProfile::Verifier),
            _ => None,
        };
        Self { profile, role }
    }
}

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Serve MCP on this process's stdin/stdout until stdin closes.
pub async fn serve_stdio(tools: Arc<Tools>) -> anyhow::Result<()> {
    let catalog = CatalogSelection::from_env();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                break;
            }
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(err) => {
                let _ = out_tx
                    .send(
                        error_response(Value::Null, PARSE_ERROR, &format!("parse error: {err}"))
                            .to_string(),
                    )
                    .await;
                continue;
            }
        };
        let Some(response) = route(&tools, catalog, message, &out_tx) else {
            continue;
        };
        let _ = out_tx.send(response.to_string()).await;
    }
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

/// Classify one inbound message. Requests are answered asynchronously via
/// `out`; an immediate `Some` is a synchronous error reply.
fn route(
    tools: &Arc<Tools>,
    catalog: CatalogSelection,
    message: Value,
    out: &mpsc::Sender<String>,
) -> Option<Value> {
    let Some(object) = message.as_object() else {
        return Some(error_response(
            Value::Null,
            INVALID_REQUEST,
            "expected a JSON-RPC object",
        ));
    };
    let id = object.get("id").cloned();
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    match (id, method) {
        (Some(id), Some(method)) => {
            let tools = tools.clone();
            let out = out.clone();
            tokio::spawn(async move {
                let response =
                    handle_request_with_catalog(&tools, catalog, id, &method, params).await;
                let _ = out.send(response.to_string()).await;
            });
            None
        }
        (None, Some(method)) => {
            tracing::debug!(method, "mcp: notification");
            None
        }
        // A response to a server-initiated request: we never send any.
        (Some(_), None) => None,
        (None, None) => Some(error_response(
            Value::Null,
            INVALID_REQUEST,
            "missing method",
        )),
    }
}

/// Answer one request. Public for tests and for embedding the dispatcher
/// behind another transport.
pub async fn handle_request(tools: &Tools, id: Value, method: &str, params: Value) -> Value {
    handle_request_with_catalog(tools, CatalogSelection::default(), id, method, params).await
}

async fn handle_request_with_catalog(
    tools: &Tools,
    catalog: CatalogSelection,
    id: Value,
    method: &str,
    params: Value,
) -> Value {
    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(LATEST_PROTOCOL);
            let version = if PROTOCOL_VERSIONS.contains(&requested) {
                requested
            } else {
                LATEST_PROTOCOL
            };
            ok_response(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "mast",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "instructions": if worker_catalog(catalog.role) { WORKER_INSTRUCTIONS } else { INSTRUCTIONS },
                }),
            )
        }
        "ping" => ok_response(id, json!({})),
        "tools/list" => {
            let mut definitions = tools.list();
            definitions.retain(|tool| tool_visible(catalog.role, tool.name));
            for tool in &mut definitions {
                catalog.profile.shape(tool);
            }
            ok_response(id, json!({ "tools": definitions }))
        }
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return error_response(id, INVALID_PARAMS, "tools/call needs a tool name");
            };
            if !tool_visible(catalog.role, name) || !tools.has(name) {
                return error_response(id, INVALID_PARAMS, &format!("unknown tool: {name}"));
            }
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match tools.call_with_content(name, arguments).await {
                Ok((value, extra)) => {
                    let mut content = vec![json!({ "type": "text", "text": pretty(&value) })];
                    content.extend(extra);
                    ok_response(
                        id,
                        json!({
                            "content": content,
                            "structuredContent": wrap_structured(value),
                            "isError": false,
                        }),
                    )
                }
                Err(message) => ok_response(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": message }],
                        "isError": true,
                    }),
                ),
            }
        }
        // Optional capabilities we do not advertise; answer empty rather
        // than erroring so permissive clients do not log noise.
        "resources/list" => ok_response(id, json!({ "resources": [] })),
        "prompts/list" => ok_response(id, json!({ "prompts": [] })),
        other => error_response(id, METHOD_NOT_FOUND, &format!("unknown method: {other}")),
    }
}

/// `structuredContent` must be an object; arrays and scalars are wrapped.
fn wrap_structured(value: Value) -> Value {
    if value.is_object() {
        value
    } else {
        json!({ "result": value })
    }
}

fn pretty(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

#[cfg(test)]
mod instructions_tests {
    use super::{INSTRUCTIONS, WORKER_INSTRUCTIONS};

    /// Claude Code cuts MCP instructions at about 2000 characters.
    const BUDGET: usize = 1900;

    #[test]
    fn instructions_fit_the_truncation_budget() {
        assert!(WORKER_INSTRUCTIONS.chars().count() <= BUDGET);
        let chars = INSTRUCTIONS.chars().count();
        assert!(
            chars <= BUDGET,
            "INSTRUCTIONS is {chars} characters; budget {BUDGET}"
        );
    }

    #[test]
    fn instructions_keep_every_guidance_area() {
        for phrase in [
            "bind_chat_worktree",
            "git worktree add",
            "work_outline_create_user_action_request",
            "work_outline_resolve_user_action_request",
            "create_chat",
            "send_message",
            "wait_for_turn",
            "work_outline_read_subtree",
            "artifact_publish",
            "artifact_get_plan",
            "artifact_render_check",
            "idempotency",
        ] {
            assert!(INSTRUCTIONS.contains(phrase), "missing `{phrase}`");
        }
    }

    #[test]
    fn worktree_guidance_survives_truncation() {
        let head: String = INSTRUCTIONS.chars().take(BUDGET).collect();
        let sentence = head
            .find("`bind_chat_worktree`")
            .expect("bind_chat_worktree in the first 1900 characters");
        assert!(head.find("git worktree add").unwrap() < sentence);
        // Blocked decisions and planning artifacts also come before the cut.
        for phrase in [
            "work_outline_create_user_action_request",
            "artifact_publish",
        ] {
            assert!(
                head.contains(phrase),
                "`{phrase}` must be inside the first {BUDGET} characters"
            );
        }
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use crate::mast::Mast;

    fn agent_tools() -> Tools {
        Tools::new(Arc::new(Mast::new(
            "ws://127.0.0.1:1".into(),
            Some("fixture-token".into()),
        )))
    }

    fn assert_portable(schema: &Value) {
        for unsupported in ["anyOf", "oneOf", "allOf", "const"] {
            assert!(schema.get(unsupported).is_none(), "{schema}");
        }
        assert!(!schema["type"].is_array(), "{schema}");
        if let Some(properties) = schema["properties"].as_object() {
            for field in properties.values() {
                assert_portable(field);
            }
        }
        if let Some(items) = schema.get("items") {
            assert!(items["type"].is_string(), "{schema}");
            assert_portable(items);
        }
    }

    #[tokio::test]
    async fn compact_catalog_schemas_fit_and_use_portable_constructs() {
        let tools = agent_tools();
        // Include management and peer-only schemas in the budget.
        let mut definitions = tools.list();
        definitions.extend(crate::tools::tool_catalog::peer_catalog());
        for mut tool in definitions {
            ToolCatalogProfile::Compact.shape(&mut tool);
            let schema = &tool.input_schema;
            let bytes = schema.to_string().len();
            assert!(bytes < 1400, "{}: {bytes} bytes", tool.name);
            assert_eq!(schema["type"], "object", "{}", tool.name);
            assert_portable(schema);
        }
    }

    #[tokio::test]
    async fn worker_catalog_lists_eighteen_and_rejects_hidden_calls() {
        let tools = agent_tools();
        let worker = CatalogSelection {
            profile: ToolCatalogProfile::Compact,
            role: Some(AgentRoleProfile::Worker),
        };
        let response =
            handle_request_with_catalog(&tools, worker, json!(1), "tools/list", json!({})).await;
        let listed = response["result"]["tools"].as_array().unwrap();
        assert_eq!(listed.len(), 18);
        let bind = listed
            .iter()
            .find(|tool| tool["name"] == "bind_chat_worktree")
            .unwrap();
        assert_eq!(bind["inputSchema"]["required"], json!(["path"]));
        let report = listed
            .iter()
            .find(|tool| tool["name"] == "report_to_parent")
            .unwrap();
        assert!(
            report["description"]
                .as_str()
                .unwrap()
                .contains("at most 5 lines")
        );
        assert_eq!(
            report["inputSchema"]["properties"]["finalHandback"]["required"],
            json!(["assignmentGeneration", "idempotencyKey"])
        );
        let response = handle_request_with_catalog(
            &tools,
            worker,
            json!(2),
            "tools/call",
            json!({"name": "create_chat"}),
        )
        .await;
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert_eq!(response["error"]["message"], "unknown tool: create_chat");
    }

    #[test]
    fn quota_status_is_visible_only_to_coordinators() {
        use AgentRoleProfile::*;
        for (role, visible) in [
            (None, false),
            (Some(Worker), false),
            (Some(Verifier), false),
            (Some(Admin), false),
            (Some(Lead), true),
            (Some(Manager), true),
        ] {
            assert_eq!(tool_visible(role, "quota_status"), visible, "{role:?}");
        }
    }
}
