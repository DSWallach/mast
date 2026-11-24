use mast_proto::agent_tools::tools as agent_tool_names;
use serde_json::{Value, json};

use super::ToolDef;

fn chat_key_schema(extra: Value) -> Value {
    let mut properties = json!({
        "chat": {
            "type": "string",
            "description": "Chat id, unique id prefix, or exact title."
        }
    });
    if let (Some(base), Some(more)) = (properties.as_object_mut(), extra.as_object()) {
        for (k, v) in more {
            base.insert(k.clone(), v.clone());
        }
    }
    json!({ "type": "object", "properties": properties, "required": ["chat"] })
}

pub(super) fn catalog() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: agent_tool_names::MCP_ONLY[0],
            description: "Which chat and device this server speaks for, plus the engine's workspace mode. Call this first when you need to know your own chat id.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[1],
            description: "Devices in this workspace (the local engine's device is flagged). Chats and Spaces are hosted on a device.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[2],
            description: "Spaces: folders on a device. Each chat belongs to one Space, which fixes its host device and working directory. The project field identifies a Space.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[3],
            description: "Agent harnesses (claude-code, codex, cursor, …) and whether each is available on this device.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[4],
            description: "Models a harness offers on this device. Model ids are harness-specific strings; pass one to create_chat.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "harness": { "type": "string", "description": "Harness id, e.g. claude-code or codex." }
                },
                "required": ["harness"]
            }),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[5],
            description: "Chats with their Space, host device, harness, model (the model that actually ran when the harness reported it: see modelIsActual, actualModel, configuredModel), live status, and last activity. Newest activity first. Each url is an internal Mast session link: use [session title](url) in replies so the user can click to open the session.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Only chats in this Space (id, path, or name)." },
                    "device": { "type": "string", "description": "Only chats hosted on this device (id or name)." },
                    "include_archived": { "type": "boolean", "default": false },
                    "parent": { "type": "string", "description": "Only chats created by this chat (id, prefix, or title) — e.g. your own id to list the chats you spawned." },
                    "workers": { "type": "boolean", "default": false, "description": "Also list the workers in your project scope (orchestrators)." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 }
                }
            }),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[6],
            description: "One chat's metadata, live status, and any question its agent is currently blocked on. Use its url as [session title](url) in replies to link to the session in Mast.",
            input_schema: chat_key_schema(json!({})),
        },
        ToolDef {
            name: "hand_off_orchestrator",
            description: "Start a successor on this work item with your sandbox; harness and model default to yours. Hand it your active workers and pending wakes. Supply a summary and a stable idempotency key. The first message includes the workers and their open Required Actions. Your chat remains active.",
            input_schema: json!({"type":"object", "properties": {
                "summary": {"type":"string", "description":"Context and next steps for the successor."},
                "harness": {"type":"string", "description":"Successor harness id (see list_harnesses)."},
                "model": {"type":"string", "description":"Successor model id (see list_models for the chosen harness)."},
                "reasoning": {"type":"string", "description":"Reasoning level supported by the chosen model."},
                "idempotencyKey": {"type":"string", "description":"Stable key for this hand-off; reuse on retry."}
            }, "required":["summary", "idempotencyKey"]}),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[7],
            description: "With workItemId, reuse a live chat on that item and send it the prompt using the composer's auto delivery (a running worker is steered, unlike a plain send_message note); set newChat to true to force a new chat. Create a chat in a Space (or without a Space on a device) with a harness and model. Mast tools, worker reports and Required Actions work for claude-code and codex workers; worker approvals reach the user as prompts, and pending approvals wake the orchestrator. The new chat records your chat as its parent (parentChatId); a child in your Space inherits your work item and instructions. To run work on an allowed work item, pass `workItemId`: the child is started for that item (the same way Start Chat on an item is), attached to it with that item's effective instructions and execution setup, and a `prompt` is required when starting a new chat (`cwd` and `branch` come from the item, so they are refused). `workItemId` must be your own item, one of its descendants, or anything inside your project (the nearest ancestor marked as a project; see `work_outline_read_subtree`; create items with `work_outline_create_child`). If `workItemId` is a project that has a worker destination (e.g. its \"Tasks\" container), the chat starts for that destination. Without `workItemId`, a chat inside a project that has a worker destination (and a `prompt`) starts its worker on a NEW item created under that destination, titled from `title` or the prompt, instead of nesting under your own item. Finished workers (`archivable: true` in list_chats / get_chat) can be archived with `archive_chat`. A child reports back with `report_to_parent` (queued, batched into your next \"Worker updates:\" when you are idle); suggest a compact STATUS / RESULT / NEXT report of at most 5 lines, without restating the task; you can also collect its result with `wait_for_turn` / `read_chat`, or from the Required Actions it raises (use `work_outline_read_subtree` for open requests). Optionally send a first prompt and wait for the reply. Returns the new chat id. When you run inside a Mast chat, idempotency_key (idempotencyKey also works) is required: reuse it when retrying the same request and you get the same chat back (never a duplicate; the same key with different inputs is an error), and the chat is created in your own Space on your device, with a sandbox no wider than yours (a child is Ask unless `autoApprove: true`, allowed only when you are on Auto).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "idempotency_key": { "type": "string", "description": "Stable key for this request (required inside a Mast chat). The same key returns the same chat; a new chat needs a new key." },
                    "newChat": { "type": "boolean", "default": false, "description": "Force a new chat instead of reusing a live chat on the item." },
                    "workItemId": { "type": "string", "description": "Start the child for your chat's item, a descendant, or any item inside your project. Requires prompt; incompatible with cwd and branch." },
                    "project": { "type": "string", "description": "Space id, path, or name. Required unless device is given." },
                    "device": { "type": "string", "description": "Host device (id or name) for a chat without a Space; defaults to this device." },
                    "parent": { "type": "string", "description": "Parent chat to record (id, prefix, or title). Defaults to the chat you are speaking from. On a paired device's server (`MAST_PEER_URL`) this is your own chat id on your machine (`whoami` on your own Mast server): the worker's `report_to_parent` reports are collected back into that chat as Worker updates." },
                    "harness": { "type": "string", "description": "Harness id (see list_harnesses). Defaults to claude-code when available." },
                    "model": { "type": "string", "description": "Model id from list_models. Omit for Mast's default for the harness (Claude Code: claude-opus-5-5). A worker started for a work item gets the Space's worker default instead (Claude Code: claude-sonnet-5-5 unless the Space chose another)." },
                    "reasoning": { "type": "string", "description": "Reasoning level the model supports (e.g. low, medium, high, max)." },
                    "sandbox": { "type": "string", "enum": ["read-only", "workspace-write", "danger-full-access"], "default": "workspace-write" },
                    "contextWindow": { "type": ["string", "integer"], "description": "Context window for the child, one of the choices list_models shows for the model (Codex: a token count such as 200000, or \"default\"; Claude: default, 250k, 500k, 750k or 1m). Omit it and a Codex worker started for a work item gets the Space's worker default (200K); every other chat keeps the model's window. Ignored when an existing chat on the item is reused." },
                    "autoApprove": { "type": "boolean", "default": false, "description": "Start the child on Auto (full access, never asks). Allowed only when your own chat is on Auto; otherwise refused." },
                    "title": { "type": "string", "description": "Sidebar title. Otherwise the engine titles it from the first exchange." },
                    "branch": { "type": "string", "description": "Branch label to record on the chat." },
                    "cwd": { "type": "string", "description": "Working directory override (an existing worktree path). Defaults to the Space folder." },
                    "prompt": { "type": "string", "description": "First message to send right away." },
                    "wait": { "type": "boolean", "default": false, "description": "With prompt: block until the first turn finishes and return the reply." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 3600, "default": 600 }
                }
            }),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[8],
            description: "Read a chat's transcript as plain messages (newest window by default). Tool calls are summarized one per line. The result is capped at about 48 KB of text (24 KB per message, keeping each message's tail): when `truncated` is true, `olderRemaining` / `nextOffset` page further back (pass `offset: nextOffset`) and `textOmittedBytes` on a message says how much of its start was cut. Reads your own chat, chats you created, and non-archived chats on items in your item subtree or project. Recorded handoff predecessors in that scope remain readable after archiving. The personal assistant chat reads every chat in every Space.",
            input_schema: chat_key_schema(json!({
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 40, "description": "How many messages, counted from the newest." },
                "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "Skip this many newest messages (paging backwards)." },
                "include_reasoning": { "type": "boolean", "default": false },
                "include_tools": { "type": "boolean", "default": true }
            })),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[9],
            description: "Send a message to a chat you created, a non-archived chat on an item in your subtree or project, or your parent. Messages are attributed to your chat, and a chat cannot message itself. A non-child, non-parent target cannot have a wider sandbox or auto-approval when your chat is on Ask. A message to your parent is queued as a worker report (returns delivery `queuedForParent`, and `heldReason` while it waits): it reaches the parent in its next \"Worker updates:\" batch when the parent is idle and the user is not in it, so `mode` and `wait` do not apply; prefer `report_to_parent`. For other reachable chats: mode 'auto' starts a turn when the chat is idle and, when the chat is running, queues the note for the end of its turn (a note never redirects work in progress). Use mode 'steer' only to stop or redirect a running turn; 'queue' always holds for the end of the turn; 'run' starts a turn. With wait=true, 'auto' keeps the composer's delivery (it steers a running turn when the harness supports it), so the reply covers your message. A message to a finished scheduled run continues in that run chat; it never opens a follow-up chat (only the composer does). With wait=true, blocks until the turn finishes (at most 1500 s; then turn.status is \"stillRunning\", call `wait_for_turn`) and returns the assistant's reply, cut to its tail when long (turn.truncated); if you did not wait, collect it later with `wait_for_turn`. The personal assistant chat can message any chat in any Space.",
            input_schema: chat_key_schema(json!({
                "text": { "type": "string" },
                "mode": { "type": "string", "enum": ["auto", "run", "steer", "queue"], "default": "auto" },
                "wait": { "type": "boolean", "default": false },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 3600, "default": 600 }
            })),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[10],
            description: "Send your final report (or an important update) to the chat that started you. Suggested format: STATUS: done|progress|blocked|question / RESULT / NEXT, at most 5 lines; do not restate the task. It is queued and reaches the parent in its next \"Worker updates:\" batch once the parent is idle and the user is not typing in it (returns `delivery: queuedForParent`, plus `heldReason` while held). Text over ~4 KB is cut and marked. Errors when you have no parent chat. A worker started by a Manager on another device (a paired peer) gets `delivery: queuedForRemoteParent`: the report waits on this device until that Manager's engine collects it. The parent can turn worker reports off; then you get `parentReportsOff` for ordinary reports. To relinquish your assignment, pass finalHandback with the assignmentGeneration from work_outline_read_current and a stable idempotencyKey. Once the report is saved in the parent transcript, Mast can archive the finished chat. Set keepChat to keep it available. When you finish testing a commit in a project run by a Manager, pass `completion` with the tested commit and your test evidence: Mast then starts one reviewer on that commit and tells the parent which chat it is. Reporting the same commit again never starts a second reviewer.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "The report text." },
                    "completion": {
                        "type": "object",
                        "description": "The commit you tested and what the tests showed. Starts one review of that commit.",
                        "properties": {
                            "testedSha": { "type": "string", "description": "The tested commit, 7 to 64 hex digits." },
                            "evidence": { "type": "string", "description": "What was run and the result." }
                        },
                        "required": ["testedSha", "evidence"]
                    },
                    "finalHandback": {
                        "type": "object",
                        "properties": {
                            "assignmentGeneration": { "type": "string" },
                            "idempotencyKey": idempotency_key_schema(),
                            "keepChat": { "type": "boolean", "default": false }
                        },
                        "required": ["assignmentGeneration", "idempotencyKey"]
                    }
                },
                "required": ["text"]
            }),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[11],
            description: "Block until a reachable chat is no longer working (chats you created or non-archived chats on items in your subtree or project): returns completed, awaitingInput (answer with respond_to_input), errored, or timedOut, with the newest assistant message. A wait never exceeds 1500 s (the MCP client's idle limit is about 1990 s): if the chat is still working then, turn.status is \"stillRunning\" with turnId and elapsedSecs, and you call wait_for_turn again. Long replies are cut to their tail (turn.truncated, replies[].textOmittedBytes); page the full transcript with read_chat. The personal assistant chat can wait on any chat.",
            input_schema: chat_key_schema(json!({
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 3600, "default": 600 }
            })),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[12],
            description: "Stop the running turn of a chat you created (the personal assistant chat: any chat; a project manager chat: any chat in its project).",
            input_schema: chat_key_schema(json!({})),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[13],
            description: "Answer a question an agent in a chat you created (or, for a project manager chat, any chat in its project) is blocked on (see get_chat / wait_for_turn pendingInput). Use option labels (case-insensitive), or free text for open questions. question_id may be omitted for a single question; otherwise use the ids from pendingInput. Reaches chats you created (the personal assistant chat: any chat).",
            input_schema: chat_key_schema(json!({
                "request_id": { "type": "string", "description": "The pending request id. Defaults to the chat's current pending question." },
                "answers": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question_id": { "type": "string" },
                            "labels": { "type": "array", "items": { "type": "string" } }
                        },
                        "required": ["labels"]
                    }
                }
            })),
        },
        ToolDef {
            name: agent_tool_names::MCP_ONLY[14],
            description: "Archive a chat (hide it from the active sidebar list); archived=false restores. Reaches direct children, project workers, and for an admin chat, other chats in its Space. Direct children and admin targets do not require force. Other project workers that are not archivable require force=true and a short reason (logged). The personal assistant chat reaches any chat in any Space without force. The user can undo.",
            input_schema: chat_key_schema(json!({
                "archived": { "type": "boolean", "default": true },
                "force": { "type": "boolean", "default": false, "description": "For a non-child project worker without admin access, archive although archivable is false; needs reason." },
                "reason": { "type": "string", "description": "Short reason for the action log; required when forcing a non-archivable non-child project worker without admin access." }
            })),
        },
    ]
}

/// For tools that derive a key when it is omitted (naturally idempotent).
fn optional_idempotency_key_schema() -> Value {
    json!({
        "type": "string",
        "description": "Optional. Omit it and Mast derives a key from the call's arguments. Pass your own to make a retry of this exact call replay the first result."
    })
}

fn idempotency_key_schema() -> Value {
    json!({
        "type": "string",
        "description": "Stable key for this change. Reuse it when you retry the same call (the retry returns the first result and changes nothing); use a new key for a new change."
    })
}

/// The Work Outline agent tools (agent mode only). The engine authorizes
/// each call against the calling Chat; arguments are camelCase like the
/// rest of the Work Outline wire.
pub(super) fn work_outline_catalog() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: agent_tool_names::READ_CURRENT,
            description: "Your chat's Work Outline item: title, status, path, children, the effective instructions it inherits, and its open user action requests. Null when your chat is not associated with an item. In Spaces with Linear turned on, also `linearIssue` (the linked Linear ticket of the item or its nearest ancestor, including its description) and `linearProject` (the nearest linked Linear project).",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: agent_tool_names::READ_SUBTREE,
            description: "Read-only view of your chat's work item (or a descendant) and everything below it as a nested tree: per item its id, title, status and effectiveStatus, the chats attached to it (id, title, url, status working/awaitingInput/errored/idle, lastMessageAt), and its open user action requests (id, text, detail, detailTruncated, originChatId, createdAt; detail is capped at 1000 characters, so when detailTruncated is true read the full text with `read_chat` on originChatId). By default completed items and their subtrees are omitted unless they contain open requests or non-archived working or awaitingInput chats; `completedOmitted` counts omitted direct children per parent. Set `includeCompleted` to include them. Archived items remain controlled by `includeArchived`. The requested root is always shown. Use it to see children and their requests. Link sessions in replies as [session title](url); these open directly inside Mast. Limited to your item's subtree, or your project when you are inside one (pass `workItemId` to read a sibling such as Tasks); at most 300 items (`truncated` says when more exist; `childrenOmitted` marks items whose children were cut by `maxDepth` or the cap).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workItemId": { "type": "string", "description": "Your chat's item, a descendant, or any item inside your project. Defaults to the project's worker destination, else your chat's item." },
                    "maxDepth": { "type": "integer", "minimum": 0, "maximum": 8, "default": 4, "description": "Levels of descendants below the item to include." },
                    "includeArchived": { "type": "boolean", "default": false, "description": "Also list archived items and chats." },
                    "includeCompleted": { "type": "boolean", "default": false, "description": "Also list completed items and their subtrees." }
                }
            }),
        },
        ToolDef {
            name: agent_tool_names::HEALTH,
            description: "Read-only cleanup checks for your work item or a descendant. Results use cached Linear and PR facts; missing PR details are queued through the existing throttled cache and unknown facts are reported separately.",
            input_schema: json!({ "type": "object", "properties": {
                "workItemId": { "type": "string", "description": "Your item or a descendant; defaults to your item." }
            }}),
        },
        ToolDef {
            name: agent_tool_names::WORK_QUEUE_READ,
            description: "Read your project's work queue in order with each entry's live state (running, stalled with its reason, ready, blocked, done), whether its pull request facts are still pending, and any waiting reason. Also whether the queue is paused and who paused it and why. Only the project's Manager role holder or an admin chat can call it.",
            input_schema: json!({ "type": "object", "properties": {
                "projectId": { "type": "string", "description": "Defaults to the project you manage. An admin chat names any project of its Space." }
            }}),
        },
        ToolDef {
            name: agent_tool_names::WORK_QUEUE_SET,
            description: "Change your project's work queue. action `place` queues the item or moves it (position: first, last, {before: id} or {after: id}; an anchor must already be queued in the project), `remove` takes it out, `prerequisites` sets the items of the project that must be complete before this one is offered (an empty list clears them; the queue shows the item as waiting, with the reason), `completion` sets what counts as this item being complete for items waiting on it (`reported`: done as the queue derives it; `landed`: its pull request merged or a person completed it, so a worker's report and passing tests alone are not enough), `offload` marks it waiting on someone else (needs reason; waitingUntil defaults to 7 days) so the queue shows it blocked, `resume` clears that mark. Only items inside your project; a retry with the same idempotencyKey changes nothing twice. Replies with the queue after the change. Only the project's Manager role holder or an admin chat can call it; the role is checked on every call, so a hand-off takes effect at once.",
            input_schema: json!({ "type": "object", "properties": {
                "workItemId": { "type": "string", "description": "An item inside your project." },
                "action": { "type": "string", "enum": ["place", "remove", "prerequisites", "completion", "offload", "resume"] },
                "position": { "description": "For place: \"first\", \"last\" (default), {\"before\": id} or {\"after\": id}." },
                "prerequisites": { "type": "array", "items": { "type": "string" }, "description": "For prerequisites: item ids of the project, at most 8; [] clears." },
                "completion": { "type": "string", "enum": ["reported", "landed"], "description": "For completion." },
                "reason": { "type": "string", "description": "Required for offload." },
                "waitingUntil": { "description": "For offload: a future unix-ms or ISO time." },
                "idempotencyKey": idempotency_key_schema()
            }, "required": ["workItemId", "action", "idempotencyKey"] }),
        },
        ToolDef {
            name: agent_tool_names::WORK_QUEUE_PAUSE,
            description: "Pause or resume auto-start for your project's work queue; a running worker is never interrupted. A Manager must give a reason to pause or resume; an admin may omit it. A pause's reason is shown beside the pause in the Tracker and on the project page; a resume's reason is not kept. A Manager can lift only a pause a Manager set: a pause set by the user or an admin can be lifted only by them. Only the project's Manager role holder or an admin chat can call it.",
            input_schema: json!({ "type": "object", "properties": {
                "paused": { "type": "boolean" },
                "reason": { "type": "string", "description": "Why, at most 500 characters. Required for a Manager (pause and resume), optional for an admin." },
                "projectId": { "type": "string", "description": "Defaults to the project you manage." },
                "idempotencyKey": idempotency_key_schema()
            }, "required": ["paused", "idempotencyKey"] }),
        },
        ToolDef {
            name: agent_tool_names::CREATE_CHILD,
            description: "Create a work item under your chat's item, a descendant, or an item inside your project. The item records your chat as its creator. Required: title and idempotencyKey. Optional: description, parentWorkItemId, status. Without parentWorkItemId, a chat inside a project creates the item in the project's worker destination (e.g. its \"Tasks\" container) when it has one, otherwise under your own item; the reply says which (resolvedToWorkerDestination). Inside a project you may also name any item of that project (e.g. a sibling Tasks container) as parentWorkItemId.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "linearIssue": { "type": "string", "description": "Linear issue identifier or URL." },
                    "parentWorkItemId": { "type": "string", "description": "Your chat's item, a descendant, or any item inside your project. Defaults to the project worker destination if configured, otherwise your chat's item." },
                    "status": { "type": "string", "enum": ["pending", "active"], "default": "pending" },
                    "idempotencyKey": idempotency_key_schema()
                },
                "required": ["title", "idempotencyKey"]
            }),
        },
        ToolDef {
            name: agent_tool_names::LINK,
            description: "Attach a Linear issue, Linear project, or GitHub pull request to your work item or a descendant. Supply exactly one reference. No network request is made.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workItemId": { "type": "string" },
                    "linearIssue": { "type": "string" },
                    "linearProject": { "type": "string", "description": "Linear project URL or id." },
                    "pullRequest": { "type": "string" },
                    "idempotencyKey": idempotency_key_schema()
                },
                "required": ["workItemId", "idempotencyKey"]
            }),
        },
        ToolDef {
            name: agent_tool_names::EXTERNAL_RUN_UPSERT,
            description: "Record a run that executes outside a local harness (a GitHub Codespace is the first kind) on your work item or a descendant, or update one. Identity is (work item, runTag), so repeating a call leaves the recorded fields as they were and a second call updates only the fields it supplies. Set expectedAlive false or an outcome (done, failed, abandoned) to end the run; an ended run needs reopen to be expected alive again. Required: runTag. Optional: workItemId (defaults to your item), kind, codespaceName, displayName, provider, sessionId, branch, ownerChatId, idempotencyKey (derived when omitted). This records intent only: it starts, stops and contacts nothing.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "runTag": { "type": "string", "description": "Unique within the item." },
                    "workItemId": { "type": "string", "description": "Your item or a descendant; defaults to your item." },
                    "kind": { "type": "string", "description": "Defaults to codespace." },
                    "codespaceName": { "type": "string" },
                    "displayName": { "type": "string" },
                    "provider": { "type": "string", "description": "claude or codex." },
                    "sessionId": { "type": "string" },
                    "branch": { "type": "string" },
                    "ownerChatId": { "type": "string" },
                    "expectedAlive": { "type": "boolean", "description": "False ends the run." },
                    "outcome": { "type": "string", "enum": ["done", "failed", "abandoned"], "description": "Ends the run." },
                    "reopen": { "type": "boolean", "description": "Expect an ended run alive again. Cannot be combined with expectedAlive false or an outcome." },
                    "idempotencyKey": optional_idempotency_key_schema()
                },
                "required": ["runTag"]
            }),
        },
        ToolDef {
            name: agent_tool_names::EXTERNAL_RUN_LIST,
            description: "Read-only: the runs recorded on your work item or a descendant (or a work item in your project), as the recorded fields only. Ended runs are left out unless includeEnded is true. Optional: workItemId, includeEnded.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workItemId": { "type": "string", "description": "Your item, a descendant, or any item inside your project; defaults to your item." },
                    "includeEnded": { "type": "boolean", "default": false }
                }
            }),
        },
        ToolDef {
            name: agent_tool_names::ASSOCIATE_CURRENT_CHAT,
            description: "Associate your own chat with a work item of its Space (never another chat). Required: workItemId. Optional: idempotencyKey (derived when omitted).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workItemId": { "type": "string" },
                    "idempotencyKey": optional_idempotency_key_schema()
                },
                "required": ["workItemId"]
            }),
        },
        ToolDef {
            name: agent_tool_names::SET_STATUS,
            description: "Mark an item active or completed, or set waitingUntil (Unix milliseconds) when work is intentionally paused. Set waitingUntil and waitingReason to null to clear them. Waiting prevents auto-continue; when due it appears in Needs-you without starting a worker. Waiting changes require idempotencyKey. Completing an item fails while it has open asks, reviews or choices (resolve them first); your own open `handoff` requests (Ready to merge) do not block it and stay open for the user. Defaults to your chat's item; descendants are also allowed. Work item IDs accept unique prefixes of at least 6 characters.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workItemId": { "type": "string", "description": "Defaults to your chat's item." },
                    "status": { "type": "string", "enum": ["active", "completed"] },
                    "waitingUntil": { "type": ["integer", "null"], "description": "Unix milliseconds, or null to clear." },
                    "waitingReason": { "type": ["string", "null"], "description": "Short reason, or null to clear." },
                    "idempotencyKey": optional_idempotency_key_schema()
                },
                "required": [],
                "anyOf": [{"required": ["status"]}, {"required": ["waitingUntil", "idempotencyKey"]}, {"required": ["waitingReason", "idempotencyKey"]}]
            }),
        },
        ToolDef {
            name: agent_tool_names::CREATE_USER_ACTION_REQUEST,
            description: "Use this, alongside asking in chat and instead of relying only on your built-in ask-user prompt, whenever your chat's work item is blocked on a decision, approval, or missing information from the user; it stays open in Required Actions until resolved. Put a one-line question in `request`, context in `detail`, and a `dedupeKey` per topic. Calling again with the same key updates your open request. A plan approval linked to a plan shares that plan's card. The user may answer via Required Actions' Reply & resolve, which arrives as a chat message starting `Re: \"…\"`; resolve the matching request when it does. Check that your chat has a work item with `work_outline_read_current`. Use your built-in quick question prompt only for small clarifications that do not block the work item. Required: request, and either idempotencyKey or dedupeKey (the dedupeKey stands in for the key). Use kind to distinguish ask, review, approve, choose, handoff (finished work), and notice (FYI); optional priority is blocking, normal, or fyi. Optional: kind, priority, options, links, facts, detail, dedupeKey, toolCallId. Options have an id, label, and optional detail. Links target artifact, file, diff, pr, chat, or https url; at most eight links and eight facts. Labels are limited to 40 characters and fact values to 120; longer text is truncated with a warning.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "request": { "type": "string", "description": "The concise question or requested action." },
                    "kind": { "type": "string", "enum": ["ask", "review", "approve", "choose", "handoff", "notice"], "description": "What the user should do; defaults to ask. Use handoff for finished work and notice for FYI." },
                    "priority": { "type": "string", "enum": ["blocking", "normal", "fyi"], "description": "Optional grouping override." },
                    "links": { "type": "array", "maxItems": 8, "items": { "type": "object", "required": ["label", "target"], "properties": { "label": { "type": "string", "maxLength": 40 }, "target": { "type": "object", "oneOf": [
                        { "properties": { "type": { "const": "artifact" }, "id": { "type": "string" } }, "required": ["type", "id"] },
                        { "properties": { "type": { "const": "file" }, "path": { "type": "string" }, "line": { "type": "integer", "minimum": 1 } }, "required": ["type", "path"] },
                        { "properties": { "type": { "const": "diff" }, "branch": { "type": "string" } }, "required": ["type"] },
                        { "properties": { "type": { "const": "pr" }, "url": { "type": "string" } }, "required": ["type", "url"] },
                        { "properties": { "type": { "const": "chat" }, "id": { "type": "string" } }, "required": ["type", "id"] },
                        { "properties": { "type": { "const": "url" }, "url": { "type": "string" } }, "required": ["type", "url"] }
                    ] } } } },
                    "options": { "type": "array", "items": { "type": "object", "required": ["id", "label"], "properties": { "id": { "type": "string", "maxLength": 120 }, "label": { "type": "string", "maxLength": 120 }, "detail": { "type": "string", "maxLength": 1000 } } } },
                    "facts": { "type": "array", "maxItems": 8, "items": { "type": "object", "required": ["label", "value"], "properties": { "label": { "type": "string", "maxLength": 40 }, "value": { "type": "string", "maxLength": 120 } } } },
                    "detail": { "type": "string" },
                    "dedupeKey": { "type": "string", "description": "An open request raised by this chat with the same key is updated in place. With a dedupeKey, idempotencyKey is optional." },
                    "toolCallId": { "type": "string" },
                    "idempotencyKey": json!({
                        "type": "string",
                        "description": "Stable key for this change; required unless you pass dedupeKey. Reuse it when you retry the same call."
                    })
                },
                "required": ["request"]
            }),
        },
        ToolDef {
            name: agent_tool_names::GET_USER_ACTION_REQUEST,
            description: "Read a request in your scope, including its kind and resolution outcome and values. Required: requestId; a unique prefix of at least 6 characters is accepted.",
            input_schema: json!({"type": "object", "properties": {"requestId": {"type": "string"}}, "required": ["requestId"]}),
        },
        ToolDef {
            name: agent_tool_names::RESOLVE_USER_ACTION_REQUEST,
            description: "Resolve a user action request your chat, or a chat it created (a child started with `create_chat`), raised once the user has answered, in chat or via a chat message starting `Re: \"…\"` from Required Actions' Reply & resolve, if it is still open (see `work_outline_read_current`). Do not leave answered requests open. Within your project you may also resolve requests of chats you did not create. When the user answered YOU in chat (not in Required Actions), use `answer_user_action_request` so their answer reaches the request's chat with provenance. Required: userActionRequestId (a unique prefix of at least 6 characters is accepted). Optional: resolution (a note), outcome (done, superseded, declined), idempotencyKey (derived when omitted).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "userActionRequestId": { "type": "string" },
                    "resolution": { "type": "string" },
                    "outcome": { "type": "string", "enum": ["done", "superseded", "declined"] },
                    "idempotencyKey": optional_idempotency_key_schema()
                },
                "required": ["userActionRequestId"]
            }),
        },
        ToolDef {
            name: agent_tool_names::ANSWER_USER_ACTION_REQUEST,
            description: "Carry the user's own answer to an open Required Action. When the user answers a request in YOUR chat (for example they tell you to go ahead), call this with the request id and the id of their message instead of relaying their wording with `send_message` (a relayed message is never accepted as approval). You pass only pointers: `userMessageId` must be a message the user typed in your own chat (find its id with `read_chat` on yourself), written after the request; assistant messages, messages from other agents, and Mast system or wake messages are refused. Mast delivers it to the request's origin chat as the user's own `Re: \"…\"` reply and resolves the request as the user's answer. Reaches requests of your own chat, chats you created, and items in your item's subtree or your project. Required: requestId (a unique prefix of at least 6 characters is accepted), userMessageId, idempotencyKey.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "requestId": { "type": "string", "description": "The open request (see work_outline_read_subtree)." },
                    "userMessageId": { "type": "string", "description": "The user's message in your own chat." },
                    "idempotencyKey": idempotency_key_schema()
                },
                "required": ["requestId", "userMessageId", "idempotencyKey"]
            }),
        },
        ToolDef {
            name: agent_tool_names::APPLY_CONTAINER_TEMPLATE,
            description: "Make an item follow its parent's child template (the structure and live instructions configured on a container such as \"Projects\"). Pass the container to apply it to every direct child that doesn't follow yet, or one item under it. Existing sub-items are matched by title and adopted; missing ones are created; nothing is deleted or moved. Only the user edits a child template's rules. Required: workItemId. Optional: idempotencyKey (derived when omitted).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workItemId": { "type": "string", "description": "A container with a child template, or an item under it." },
                    "idempotencyKey": optional_idempotency_key_schema()
                },
                "required": ["workItemId"]
            }),
        },
        ToolDef {
            name: agent_tool_names::PROPOSE_INSTRUCTION,
            description: "Propose an instruction change for user review. For edit/remove provide instructionId from work_outline_read_current. Template-linked instructions propose changes to their child template. For add, target childTemplate defaults to the node followed by your item, or the template root for a direct child. If the template or node cannot be resolved, the call fails. Alternatively, ruleNodePath is an array of child rule titles from the template root. Required: idempotencyKey, and text for add/edit.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["add", "edit", "remove"], "default": "add" },
                    "instructionId": { "type": "string" },
                    "text": { "type": "string" },
                    "rationale": { "type": "string" },
                    "target": { "type": "string", "enum": ["item", "parent", "spaceRoot", "childTemplate"], "default": "item" },
                    "containerWorkItemId": { "type": "string" },
                    "ruleNodeId": { "type": "string" },
                    "ruleNodePath": { "type": "array", "items": { "type": "string" } },
                    "toolCallId": { "type": "string" },
                    "idempotencyKey": idempotency_key_schema()
                },
                "required": ["idempotencyKey"]
            }),
        },
        ToolDef {
            name: agent_tool_names::PROPOSE_APPROVAL_RULE,
            description: "Propose a command approval rule for user review. Creates a Required Action; never enables a rule. The user can review and accept it there.",
            input_schema: json!({ "type": "object", "properties": {
                "label": {"type": "string"}, "argvPrefix": {"type": "array", "items": {"type": "string"}},
                "commandRegex": {"type": "string"}, "cwdScope": {"type": "string"},
                "harness": {"type": "string", "enum": ["codex", "claude-code", "any"]},
                "rationale": {"type": "string"}, "idempotencyKey": idempotency_key_schema()
            }, "required": ["label", "idempotencyKey"] }),
        },
        ToolDef {
            name: agent_tool_names::ADMIN,
            description: "ADMIN CHATS AND PROJECT MANAGER CHATS ONLY (the user grants admin per chat and Space, with an expiry; a project manager holds this for its project with no expiry; refused otherwise). A project manager is limited to items and chats inside its own project: it cannot create at, move out of, or touch anything beyond the project subtree, cannot move, archive or re-flag the project item itself, and has no instruction-proposal steps. Batch of outline changes anywhere in your granted Space, attributed to you as an admin chat and undoable per turn by the user. Child template rules are managed with template_set and applied with template_apply. A project is an outline item marked as a project. steps[] ops: create{parentId?,title,description?,status?,ref?} rename{itemId,title} describe{itemId,description|null} move{itemId,parentId|null,position?} group{title,itemIds[],parentId?} status{itemId,status} archive{itemId} restore{itemId} project{itemId,isProject} destination{itemId,destinationId|null} convert_to_project{itemId,destinationId?} assign_chat{chatId,itemId|null} instruction_add{itemId|null,text} instruction_edit{instructionId,text} instruction_remove{instructionId} proposal_accept{proposalId,text?} proposal_dismiss{proposalId,reason?} resolve_request{requestId,note?} template_set{itemId,template|null} template_apply{itemId}; `$ref` names an item an earlier step created. dryRun:true previews. Over 10 steps nothing runs: you get a preview plus confirmToken and an \"Approve bulk change\" Required Action; wait for the user to resolve it themself, then repeat the SAME steps with confirmToken (30 min, one use). {read:true,maxDepth?} lists the Space tree, open requests and proposals. Never reaches other Spaces, Linear/GitHub, or irreversible deletes. Required: idempotencyKey.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "steps": { "type": "array", "items": { "type": "object", "properties": { "op": { "type": "string" } }, "required": ["op"] } },
                    "dryRun": { "type": "boolean" },
                    "confirmToken": { "type": "string" },
                    "read": { "type": "boolean" },
                    "maxDepth": { "type": "integer" },
                    "idempotencyKey": idempotency_key_schema()
                },
                "required": ["idempotencyKey"]
            }),
        },
        ToolDef {
            name: agent_tool_names::WORKFLOW_PACK_EXPORT,
            description: "ADMIN CHATS ONLY. Export the Space's Work Outline setup (instructions per item path, and project templates) as a workflow pack: returns `pack` and the `redactions` made (credentials, and in share mode emails and home paths; detection is incomplete, so review before sharing). Writes nothing. Optional: mode (share, the default, or full).",
            input_schema: json!({
                "type": "object",
                "properties": { "mode": { "type": "string", "enum": ["share", "full"], "default": "share" } }
            }),
        },
        ToolDef {
            name: agent_tool_names::WORKFLOW_PACK_PREVIEW_IMPORT,
            description: "ADMIN CHATS ONLY. Read a workflow pack file on this device and preview importing it into the Space; changes nothing. Returns `previewId`, the `scopes` (instructions by item path, with the destination's current text and any missing items to create) and the `templates`; a scope or template whose `current` differs from its `incoming` is a conflict that apply needs a resolution for. Required: path.",
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Absolute path of the pack file." } },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: agent_tool_names::WORKFLOW_PACK_APPLY,
            description: "ADMIN CHATS ONLY. Apply a previewed workflow pack in one all-or-nothing step and return a receipt. Fails, changing nothing, if the Space changed since the preview. Every conflict needs a resolution in `resolutions`: {scopes:[{itemPath:[titles], resolution}], templates:[{name, resolution}]} where resolution is skip (keep the destination's), append (instructions only: add the missing ones) or replace (archive the destination's instructions or swap the template body). Items the pack needs are created. Required: previewId. Optional: resolutions. Undo with work_outline_workflow_pack_undo.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "previewId": { "type": "string" },
                    "resolutions": {
                        "type": "object",
                        "properties": {
                            "scopes": { "type": "array", "items": { "type": "object", "properties": {
                                "itemPath": { "type": "array", "items": { "type": "string" } },
                                "resolution": { "type": "string", "enum": ["skip", "append", "replace"] }
                            }, "required": ["itemPath", "resolution"] } },
                            "templates": { "type": "array", "items": { "type": "object", "properties": {
                                "name": { "type": "string" },
                                "resolution": { "type": "string", "enum": ["skip", "replace"] }
                            }, "required": ["name", "resolution"] } }
                        }
                    }
                },
                "required": ["previewId"]
            }),
        },
        ToolDef {
            name: agent_tool_names::WORKFLOW_PACK_UNDO,
            description: "ADMIN CHATS ONLY. Undo a workflow pack apply from its receipt. Refuses, changing nothing, if anything the import added or replaced was edited, moved, or used since. Required: receiptId.",
            input_schema: json!({
                "type": "object",
                "properties": { "receiptId": { "type": "string" } },
                "required": ["receiptId"]
            }),
        },
        ToolDef {
            name: agent_tool_names::WORKFLOW_PACK_RECEIPTS,
            description: "ADMIN CHATS ONLY. List the Space's workflow pack import receipts, newest first, with what each added and whether it was undone.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
    ]
}

/// The planning artifact tools (agent mode only;
/// `docs/planning-artifact-conventions.md`). The engine scopes each call to
/// the calling Chat's work item (or the Chat).
pub(super) fn artifact_catalog() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: agent_tool_names::ARTIFACT_PUBLISH,
            description: "Publish every file you wrote that the user should see (plan, diagram, state machine, option comparison) as a planning artifact of your chat's work item. Mast copies it into the work item's artifact folder (<canonical checkout>/.mast/artifacts/<work item>/), records a revision when the content changed, and returns {artifactId, revision, url, storedPath}. Later publishes with the same title and role add revisions to the same artifact. Link it in your message as [title](mast-artifact:<artifactId>), never as a bare path. HTML: write <name>.fragment.html (body content only) and publish it; a sibling <name>.preview.html is copied too. Options: publish the JSON manifest with kind options; its fragments are copied with it. Side by side renders each option at a fixed design width (default 1100 px) and scales it to the column, so set `designWidth` (px) on each manifest entry or at the top level, or `data-mast-design-width` in the fragment. A project's Manager, which cannot write files, publishes its Manager status with inline `content` instead of `path`, with title \"Manager status\" and role \"manager-status\"; no one else may use `content`.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file you wrote: absolute, or relative to your working directory (your worktree, the Space folder, or a temp folder). Not used with `content`." },
                    "content": { "type": "string", "description": "The whole document as text, instead of a file. Only the project Manager's status record (role manager-status)." },
                    "title": { "type": "string", "description": "Human title, e.g. \"Receiving flow — plan\". Same title + role on the same work item = same artifact." },
                    "kind": { "type": "string", "enum": ["markdown", "html-fragment", "html-page", "mermaid", "image", "options", "decision-sheet", "plan"], "description": "Structured plan JSON uses kind plan; otherwise defaults from the file extension." },
                    "role": { "type": "string", "description": "plan, diagram, state-machine, comparison, or doc (free-form)." },
                    "note": { "type": "string", "description": "One line: what changed in this revision." },
                    "pinAsPlan": { "type": "boolean", "default": false, "description": "Make this the work item's plan (unpins the previous one)." },
                    "addressedComments": { "type": "array", "items": { "type": "string" }, "description": "Comment ids (C-n from artifact_get_plan) this revision addresses; they are marked resolved." },
                    "idempotencyKey": idempotency_key_schema()
                },
                "required": ["title", "note", "idempotencyKey"],
                "anyOf": [{ "required": ["path"] }, { "required": ["content"] }]
            }),
        },
        ToolDef {
            name: agent_tool_names::ARTIFACT_GET_PLAN,
            description: "The pinned plan of your chat's work item: {artifactId, title, revision, storedPath, content, openQuestions, unverifiedClaims, answeredQuestions: [{id, answer, at, revision}], comments: [{id, text, quote, anchor, revision, at, anchorStatus}], pickedOptions: [{artifactId, title, optionId, label, note, at, revision}]}, or null. answeredQuestions are answers the user gave in Mast (the plan's question card or Required Actions) that the document may not reflect yet: fold each into the plan (`status: resolved` + `decision:`) in your next revision. comments are the open comments on an HTML or Mermaid plan, each with its anchor and quoted text (anchorStatus found|outdated|unknown): address them in the next revision and pass their ids as addressedComments to artifact_publish. pickedOptions are the options the user picked on your options artifacts (also in the plan-less reply): continue with that option. Call it at the start of every chat under a work item; if a plan exists, work from it and publish new revisions of it instead of writing a new one.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: agent_tool_names::ARTIFACT_GET_DECISIONS,
            description: "Read sent decisions for a decision sheet in your work item, including row IDs, outcomes, notes, timestamps, revisions, and IDs no longer present in the sheet.",
            input_schema: json!({"type": "object", "properties": {"artifactId": {"type": "string"}}, "required": ["artifactId"]}),
        },
        ToolDef {
            name: agent_tool_names::BRIEFING_READ,
            description: "Read-only. The latest results artifact (briefing) of one scheduled task, or of every task in your Space, with run times: {briefings: [{task, taskId, artifactId, title, revision, runAt (epoch ms), runAtLocal (the chat host's time zone), runId, workItemId, content, contentTruncated, contentAvailable}], needYou}. runAt is when the newest genuine revision was published; content is that revision as recorded. needYou counts open Required Actions raised by scheduled tasks: all of them in the Space (failed runs included), or only the named task's. Pass `task` (the task's current or original name, its id, or the results artifact id) for one briefing; omit it for all. Pass `index: true` for run times only, without content. contentAvailable true means content was read; false means it was not read (the file lives on another device, or its recorded content could not be read), so open the artifact in Mast instead. With index true it only says the file is on this device and a later read can still fail. Use this instead of read_chat or Notion to see what a briefing found. Pass `kind: \"manager-status\"` to read your project's Manager status record instead (one entry, content is the status document); it only returns the project the calling chat works in.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "Task name, task id, or results artifact id. Omit for every task in the Space." },
                    "index": { "type": "boolean", "default": false, "description": "Run times only, no content." },
                    "kind": { "type": "string", "enum": ["scheduled-results", "manager-status"], "default": "scheduled-results", "description": "manager-status reads the project's Manager status record." }
                }
            }),
        },
        ToolDef {
            name: agent_tool_names::ARTIFACT_RENDER_CHECK,
            description: "Render an artifact exactly as Mast shows it (same wrapper, sandbox, and theme) in the Mast window on this device and return screenshots plus errors (console errors, blocked resources, Mermaid parse errors, empty render) and warnings (overflow, content outside the viewport, low contrast, and layout heuristics in layoutWarnings: clipped text, overlapping siblings, narrow text columns, elements wider than their parent, fixed widths wider than the viewport). By default it renders a narrow (800x800) and a wide (1440x900) viewport in the current theme; pass `themes: [\"light\", \"dark\"]` to check both themes (at most 4 images per call; each image comes with a label). Warnings are heuristics: always look at every screenshot. Run it after creating or changing any visual artifact, fix what is wrong, and check again. If it fails with renderUnavailable, say the artifact was not checked.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "artifactId": { "type": "string", "description": "A published artifact of your work item." },
                    "screenshots": { "type": "boolean", "default": false, "description": "Attach the screenshots. By default only errors, warnings and metrics return (the images are most of the size)." },
                    "path": { "type": "string", "description": "Or an unpublished file you wrote." },
                    "kind": { "type": "string", "description": "Optional artifact kind override for a path." },
                    "viewports": {
                        "type": "array",
                        "description": "Viewports to render. Default: [{width: 800, height: 800, label: \"narrow\"}, {width: 1440, height: 900, label: \"wide\"}].",
                        "items": {
                            "type": "object",
                            "properties": {
                                "width": { "type": "integer", "minimum": 200, "maximum": 4096 },
                                "height": { "type": "integer", "minimum": 200, "maximum": 4096 },
                                "label": { "type": "string" }
                            },
                            "required": ["width", "height"]
                        }
                    },
                    "viewport": {
                        "type": "object",
                        "description": "A single viewport; replaces the default list.",
                        "properties": {
                            "width": { "type": "integer", "minimum": 200, "maximum": 4096 },
                            "height": { "type": "integer", "minimum": 200, "maximum": 4096 }
                        }
                    },
                    "themes": {
                        "type": "array",
                        "description": "Themes to render every viewport in. Default: the user's current theme only. At most 4 images (viewports x themes) per call.",
                        "items": { "type": "string", "enum": ["light", "dark"] }
                    },
                    "theme": { "type": "string", "enum": ["light", "dark"], "description": "A single theme. Default: the user's current theme." }
                }
            }),
        },
        ToolDef {
            name: agent_tool_names::REVIEW_PUBLISH,
            description: "Publish a review for a PR or committed branch range. summary is one string of 3–8 lines separated by newlines. Risk: none means no risk, low means limited risk, med means material risk, high means severe risk. Hunks and anchors must match valid merge-base diff hunks; errors list the valid hunks. Verdict hints and suggested comments are never posted. Example: {\"prUrl\":\"https://github.com/a/b/pull/1\",\"headSha\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"review\":{\"schema\":\"mast.review/v1\",\"pr\":{\"repo\":\"a/b\",\"number\":1,\"headSha\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"baseSha\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"},\"summary\":\"Reviewed changes\\nChecked tests\\nNo findings\",\"steps\":[],\"highlights\":[],\"questions\":[],\"suggestedComments\":[]}}",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prUrl": { "type": "string", "description": "Use prUrl with headSha and review.pr, or supply target." },
                    "headSha": { "type": "string", "description": "Full reviewed head commit SHA; required with prUrl. Alternatively supply target." },
                    "target": review_target_schema(),
                    "review": review_schema()
                },
                "required": ["review"]
            }),
        },
        ToolDef {
            name: agent_tool_names::REVIEW_GET,
            description: "Read this chat's published review, or null. Select with prUrl or target; omit both for the latest review.",
            input_schema: json!({ "type": "object", "properties": { "prUrl": { "type": "string" }, "target": review_target_schema() } }),
        },
    ]
}

fn review_target_schema() -> Value {
    json!({"type": "object", "description": "Alternative to prUrl and headSha. Fields depend on kind.", "properties": {
        "kind": {"type": "string", "enum": ["pr", "branch"], "description": "Required for both target kinds."},
        "repo": {"type": "string", "description": "Required for pr; for branch, supply repo or checkoutPath when publishing."},
        "number": {"type": "integer", "minimum": 0, "description": "Required for pr."},
        "headSha": {"type": "string", "description": "Full head commit SHA; required for both kinds."},
        "baseSha": {"type": "string", "description": "Required for pr; for branch publishing supply baseSha or baseRef."},
        "baseRef": {"type": "string", "description": "For branch publishing, an alternative to baseSha."},
        "checkoutPath": {"type": "string", "description": "For branch publishing, an alternative to repo."},
        "checkout": {"type": "string", "description": "For branch, alias for checkoutPath."},
        "branch": {"type": "string", "description": "Optional branch name for branch targets."}
    }})
}

fn review_schema() -> Value {
    let anchor = json!({"type": "object", "properties": {
        "file": {"type": "string"}, "side": {"type": "string", "enum": ["left", "right"]},
        "startLine": {"type": "integer", "minimum": 0}, "endLine": {"type": "integer", "minimum": 0},
        "hunkHash": {"type": "string"}
    }, "required": ["file", "side", "startLine", "endLine"]});
    json!({"type": "object", "properties": {
        "schema": {"type": "string", "enum": ["mast.review/v1"]},
        "pr": {"type": "object", "properties": {"repo": {"type": "string"}, "number": {"type": "integer", "minimum": 0}, "headSha": {"type": "string"}, "baseSha": {"type": "string"}}, "required": ["repo", "number", "headSha", "baseSha"]},
        "summary": {"type": "string"}, "verdictHint": {"type": "string"},
        "steps": {"type": "array", "items": {"type": "object", "properties": {
            "id": {"type": "string"}, "title": {"type": "string"}, "file": {"type": "string"},
            "hunks": {"type": "array", "items": {"type": "object", "properties": {"newStart": {"type": "integer", "minimum": 0}, "newEnd": {"type": "integer", "minimum": 0}, "hunkHash": {"type": "string"}}, "required": ["newStart", "newEnd"]}},
            "what": {"type": "string"}, "why": {"type": "string"}, "risk": {"type": "string", "enum": ["none", "low", "med", "high"]}, "riskNote": {"type": "string"}, "tags": {"type": "array", "items": {"type": "string"}}, "group": {"type": "string"}
        }, "required": ["id", "title", "file", "hunks", "what", "why", "risk", "riskNote", "tags"]}},
        "highlights": {"type": "array", "items": {"type": "object", "properties": {"kind": {"type": "string", "enum": ["risk", "missing-tests", "behaviour-change", "security", "migration", "perf", "style"]}, "severity": {"type": "string"}, "text": {"type": "string"}, "anchor": anchor}, "required": ["kind", "severity", "text", "anchor"]}},
        "questions": {"type": "array", "description": "Questions as strings or objects with question or text fields.", "items": {}},
        "suggestedComments": {"type": "array", "items": {"type": "object", "properties": {"id": {"type": "string", "description": "Stable identifier across review revisions."}, "anchor": anchor, "body": {"type": "string"}, "severity": {"type": "string"}, "suggestion": {"type": "string"}}, "required": ["anchor", "body", "severity"]}}
    }, "required": ["schema", "summary", "steps", "highlights", "questions", "suggestedComments"],
       "description": "With prUrl, pr is required. With target, Mast supplies pr when omitted."})
}

/// Chat diff target tools (agent mode only; `m0-contract.md`, "Amendment:
/// chat diff targets"). The engine binds for the calling Chat only.
pub(super) const DIFF_TARGET_TOOLS: [&str; 1] = [agent_tool_names::BIND_CHAT_WORKTREE];

pub(super) fn diff_target_catalog() -> Vec<ToolDef> {
    vec![ToolDef {
        name: agent_tool_names::BIND_CHAT_WORKTREE,
        description: "Tell Mast which git worktree this chat is working in, so the Changes pane shows that worktree instead of the folder the chat started in. Call it right after `git worktree add` (including worktrees of a submodule such as flagship, created under `.worktrees/`), and again when you resume an existing worktree for this chat's work. Call it once per worktree: a chat that works in several submodules binds each one. It only records the path; it does not create, switch, or modify anything. Pass the worktree's absolute top-level path (the output of `git rev-parse --show-toplevel` inside it); it must be a worktree of this chat's repository or of one of its submodules. Required: path. Optional: idempotencyKey (not needed: binding the same path again just re-selects it).",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path of the worktree's top-level folder." },
                "idempotencyKey": optional_idempotency_key_schema()
            },
            "required": ["path"]
        }),
    }]
}

pub(super) fn friction_catalog() -> Vec<ToolDef> {
    let links = work_outline_catalog()
        .into_iter()
        .find(|def| def.name == agent_tool_names::CREATE_USER_ACTION_REQUEST)
        .expect("request tool")
        .input_schema["properties"]["links"]
        .clone();
    vec![
        ToolDef {
            name: agent_tool_names::REPORT_FRICTION,
            description: "Record a bug, failed tool call, permission issue, environment problem, workflow issue, or documentation issue. Mast fills provenance and groups matching reports within the Space.",
            input_schema: json!({"type":"object", "properties": {
                "kind":{"type":"string","enum":["bug","tool_failure","permission","environment","workflow","docs"]},
                "summary":{"type":"string","maxLength":140}, "whatHappened":{"type":"string"},
                "workaround":{"type":"string"}, "suggestion":{"type":"string"},
                "approval":{"type":"object","properties":{"command":{"type":"string"},"why":{"type":"string"}},"required":["command","why"]},
                "severity":{"type":"string","enum":["low","medium","high"]},
                "links":links
            }, "required":["kind","summary","whatHappened"]}),
        },
        ToolDef {
            name: agent_tool_names::LIST_FRICTION,
            description: "List grouped friction reports for this Space, including occurrence counts. Set scope to allSpaces to include Mast friction from other Spaces when the user grants access in Space settings. Filter by kind, status, or last-seen timestamp (milliseconds since epoch). Newest first, 25 per call by default (`limit` up to 500; `total`/`omitted` say what was left out), long text cut to 300 characters (`whatHappenedTruncated`) and no occurrence history unless `full: true`.",
            input_schema: json!({"type":"object", "properties": {
                "scope":{"type":"string","enum":["space","allSpaces"]},
                "kind":{"type":"string","enum":["bug","tool_failure","permission","environment","workflow","docs"]},
                "status":{"type":"string","enum":["open","dismissed","promoted"]},
                "since":{"type":"integer"}, "limit":{"type":"integer","minimum":1,"maximum":500}, "full":{"type":"boolean"}
            }}),
        },
        ToolDef {
            name: agent_tool_names::DISMISS_FRICTION,
            description: "Dismiss a Space friction report with a reason, for orchestrator and admin chats.",
            input_schema: json!({"type":"object", "properties": {"id":{"type":"string"}, "reason":{"type":"string","maxLength":1000}}, "required":["id","reason"]}),
        },
        ToolDef {
            name: agent_tool_names::RUN_COMMAND,
            description: "Start a long command (a test suite, build or preview) on the engine, or attach to it when `idempotencyKey` was already used by this chat. Returns at once with the run's log path. When the command exits, the engine wakes this chat exactly once with the exit code, the log path and a short failure summary, so end your turn instead of polling. Only commands the user's permission rules allow run, unless the chat auto-approves or its harness never asks for approval (Cursor), where any command not denied by a rule runs; a read-only chat is refused. To wait for a job on another host, run `scripts/remote-cargo.sh wait <host> <name> <minutes>` (or `ssh`) here: the wake carries the trimmed result, and a timeout (exit 124) or ssh failure (255) is explained in a Note. A chat may have four commands running at once. Use `command_status` for the current state. Output goes to the log file, not the chat (capped: older output is rotated out of very long logs).",
            input_schema: json!({"type":"object", "properties": {
                "idempotencyKey":{"type":"string"}, "command":{"type":"string"}, "cwd":{"type":"string"}
            }, "required":["idempotencyKey","command"]}),
        },
        ToolDef {
            name: agent_tool_names::COMMAND_STATUS,
            description: "State (running, exited, interrupted) of a `run_command` run started by this chat, with exit code, log path and the failure summary once it has exited.",
            input_schema: json!({"type":"object", "properties": {"idempotencyKey":{"type":"string"}}, "required":["idempotencyKey"]}),
        },
        ToolDef {
            name: agent_tool_names::AWAIT_BACKGROUND,
            description: "Declare that you are ending your turn to wait on background work Mast cannot see, such as a background build or a subagent, so Mast does not send auto-continue nudges. Holds for at most `maxMinutes` (1-60) and ends at your next turn, however it starts. Prefer `run_command` for commands. A wait that never ends still shows as stalled.",
            input_schema: json!({"type":"object", "properties": {"reason":{"type":"string"}, "maxMinutes":{"type":"integer","minimum":1,"maximum":60}}, "required":["reason","maxMinutes"]}),
        },
    ]
}

/// The branch-transfer tools of a server that proxies a paired device.
pub(super) fn peer_catalog() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "peer_push_branch",
            description: "Send a local branch to this paired device over the peer link (no SSH): it is filed as `peer/<peer>/<branch>` in the repository of one of its Spaces, with a worktree Mast creates for it. Nothing else on the device is touched, and a branch someone has checked out is never overwritten. Sending the same branch again fast-forwards a clean worktree. Returns `worktreePath` and `peerBranch`: start the worker there with create_chat (project, cwd = worktreePath, branch = peerBranch, parent = your own chat id), then bring its commits back with peer_fetch_branch. The device needs the `base` commit; omit `base` to send the whole history.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "Absolute path of the repository on this machine." },
                    "branch": { "type": "string", "description": "Local branch to send, a plain name such as feature/x." },
                    "project": { "type": "string", "description": "The device's Space (id, path or name) whose repository receives it; see list_projects on this server." },
                    "base": { "type": "string", "description": "A revision the device already has, for example the branch both machines track; only commits after it are sent." }
                },
                "required": ["repo", "branch", "project"]
            }),
        },
        ToolDef {
            name: "peer_fetch_branch",
            description: "Fetch the commits a worker made on a branch sent with peer_push_branch into a local branch, over the peer link. Nothing is merged or pushed: review and merge the local branch yourself. Pass `base` (the `head` peer_push_branch returned) so only new commits travel. The local branch (default `peer/<branch>`) is never one that is checked out, and never moves to unrelated history unless `force` is set.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "Absolute path of the repository on this machine." },
                    "branch": { "type": "string", "description": "The branch name used with peer_push_branch." },
                    "project": { "type": "string", "description": "The device's Space the branch was sent to." },
                    "base": { "type": "string", "description": "A commit you already have on the branch, normally the `head` from peer_push_branch." },
                    "into": { "type": "string", "description": "Local branch to receive the commits; default peer/<branch>." },
                    "force": { "type": "boolean", "default": false, "description": "Let `into` move to commits that do not descend from it." }
                },
                "required": ["repo", "branch", "project"]
            }),
        },
    ]
}
