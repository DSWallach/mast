//! The wire-facing half of worker context slicing: the `context_read_slice`
//! tool definition and the `read_chat` fence for leaf workers. The packet
//! builder and slicer live in `mast-engine`'s `context_slice`; these pieces sit
//! here because `mast-mcp` serves the catalog and `read_chat` and does not
//! depend on the engine.

use serde_json::{Value, json};

use crate::agent_tools::tools;

/// Largest window of its recorded predecessor's transcript a worker may read.
pub const PREDECESSOR_READ_LIMIT: usize = 6;

/// The server instructions a leaf worker's MCP session starts with, and layer 1
/// of its context packet.
pub const WORKER_ROLE_PROMPT: &str = "\
Mast runs coding agents in Space chats. Report friction with `report_friction`.
\
After git worktree add or when resuming, call `bind_chat_worktree` with the absolute top-level path.
\
Read your work item with `work_outline_read_current` and its plan with `artifact_get_plan`. Publish user-facing files with `artifact_publish`, link the returned URL, and run `artifact_render_check` after visual changes.
\
For a blocked decision, approval or missing input, ask in chat and use `work_outline_create_user_action_request` with a one-line request, context in detail and a stable dedupeKey. Use `work_outline_resolve_user_action_request` when answered.
\
Reuse the same idempotency key when retrying a mutation. Send your parent a compact `report_to_parent` of at most 5 lines using STATUS / RESULT / NEXT.\n\
If your role changes, request tools/list again for the current catalog.";

/// The tools a Worker's catalog lists, in catalog order. `mast-mcp` serves the
/// schemas; the context packet names them.
pub const WORKER_TOOL_NAMES: [&str; 19] = [
    "whoami",
    "get_chat",
    "read_chat",
    "send_message",
    "report_to_parent",
    tools::READ_CURRENT,
    tools::CREATE_USER_ACTION_REQUEST,
    tools::GET_USER_ACTION_REQUEST,
    tools::RESOLVE_USER_ACTION_REQUEST,
    tools::ARTIFACT_PUBLISH,
    tools::ARTIFACT_GET_PLAN,
    tools::CONTEXT_READ_SLICE,
    tools::ARTIFACT_RENDER_CHECK,
    tools::REVIEW_PUBLISH,
    tools::BIND_CHAT_WORKTREE,
    tools::REPORT_FRICTION,
    tools::RUN_COMMAND,
    tools::COMMAND_STATUS,
    tools::AWAIT_BACKGROUND,
];

/// Catalog description of `context_read_slice`.
pub const TOOL_DESCRIPTION: &str = "Read part of your plan without loading all of it, cut at headings. Pass plan or plan#<anchor>; the reply has nextAnchor when more follows and a replayId you can cite after dropping the text.";

/// Catalog input schema of `context_read_slice`.
pub fn tool_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": { "type": "string", "description": "plan or plan#<heading-anchor>" },
            "maxTokens": { "type": "integer", "description": "100-6000, default 1500" },
            "reason": { "type": "string", "description": "Why you need it" }
        },
        "required": ["target", "reason"]
    })
}

/// The tool definition. Its serialized form stays under 550 bytes.
pub fn tool_definition() -> Value {
    json!({
        "name": crate::agent_tools::tools::CONTEXT_READ_SLICE,
        "description": TOOL_DESCRIPTION,
        "inputSchema": tool_input_schema(),
    })
}


/// How the chat a worker wants to read relates to the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRelation {
    Own,
    Parent,
    Child,
    /// The chat the worker's work item recorded as its handoff predecessor.
    Predecessor,
    /// Another worker of the same parent.
    Sibling,
    Other,
}

/// Facts about the two chats, enough to classify them.
#[derive(Debug, Clone, Copy)]
pub struct ReadChatFacts<'a> {
    pub caller_chat_id: &'a str,
    pub caller_parent_id: Option<&'a str>,
    pub target_chat_id: &'a str,
    pub target_parent_id: Option<&'a str>,
    /// Handoff predecessors recorded for the caller's own work item.
    pub recorded_predecessors: &'a [String],
}

pub fn classify_chat(facts: &ReadChatFacts<'_>) -> ChatRelation {
    if facts.target_chat_id == facts.caller_chat_id {
        ChatRelation::Own
    } else if facts.recorded_predecessors.iter().any(|id| id == facts.target_chat_id) {
        ChatRelation::Predecessor
    } else if facts.caller_parent_id == Some(facts.target_chat_id) {
        ChatRelation::Parent
    } else if facts.target_parent_id == Some(facts.caller_chat_id) {
        ChatRelation::Child
    } else if facts.caller_parent_id.is_some() && facts.caller_parent_id == facts.target_parent_id {
        ChatRelation::Sibling
    } else {
        ChatRelation::Other
    }
}

/// A `read_chat` the worker fence refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadChatRefused(pub ChatRelation);

impl std::fmt::Display for ReadChatRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "a worker cannot read a sibling worker's chat; ask your parent, or read the chat of your recorded handoff predecessor",
        )
    }
}

impl std::error::Error for ReadChatRefused {}

/// The window a worker may read: unchanged for its own chat, its parent and
/// its children; at most [`PREDECESSOR_READ_LIMIT`] messages of its recorded
/// predecessor; refused for a sibling.
pub fn worker_read_chat_window(
    relation: ChatRelation,
    limit: Option<usize>,
) -> Result<Option<usize>, ReadChatRefused> {
    match relation {
        // `Other` is left to the transport's own reach rules.
        ChatRelation::Own | ChatRelation::Parent | ChatRelation::Child | ChatRelation::Other => Ok(limit),
        ChatRelation::Predecessor => Ok(Some(limit.unwrap_or(PREDECESSOR_READ_LIMIT).min(PREDECESSOR_READ_LIMIT))),
        ChatRelation::Sibling => Err(ReadChatRefused(relation)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definition_is_under_550_bytes() {
        let size = serde_json::to_string(&tool_definition()).unwrap().len();
        assert!(size < 550, "{size} bytes");
        assert_eq!(tool_definition()["name"], "context_read_slice");
    }

    #[test]
    fn read_chat_fence_refuses_siblings_and_windows_the_predecessor() {
        let predecessors = ["old".to_owned()];
        let facts = |target: &'static str, target_parent: Option<&'static str>| ReadChatFacts {
            caller_chat_id: "me",
            caller_parent_id: Some("boss"),
            target_chat_id: target,
            target_parent_id: target_parent,
            recorded_predecessors: &predecessors,
        };
        assert_eq!(classify_chat(&facts("me", Some("boss"))), ChatRelation::Own);
        assert_eq!(classify_chat(&facts("boss", None)), ChatRelation::Parent);
        assert_eq!(classify_chat(&facts("kid", Some("me"))), ChatRelation::Child);
        assert_eq!(classify_chat(&facts("sib", Some("boss"))), ChatRelation::Sibling);
        assert_eq!(classify_chat(&facts("old", Some("boss"))), ChatRelation::Predecessor, "a predecessor that is also a sibling stays readable");
        assert_eq!(classify_chat(&facts("far", Some("elsewhere"))), ChatRelation::Other);

        assert!(worker_read_chat_window(ChatRelation::Sibling, Some(1)).is_err());
        assert_eq!(worker_read_chat_window(ChatRelation::Other, Some(9)), Ok(Some(9)));
        assert_eq!(worker_read_chat_window(ChatRelation::Predecessor, Some(500)), Ok(Some(6)));
        assert_eq!(worker_read_chat_window(ChatRelation::Predecessor, None), Ok(Some(6)));
        assert_eq!(worker_read_chat_window(ChatRelation::Predecessor, Some(2)), Ok(Some(2)));
        assert_eq!(worker_read_chat_window(ChatRelation::Own, Some(500)), Ok(Some(500)));
        assert!(worker_read_chat_window(ChatRelation::Sibling, None).unwrap_err().to_string().contains("sibling"));
    }
}
