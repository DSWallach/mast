# Mast MCP server

`mast mcp` serves the Model Context Protocol on stdin/stdout and proxies every
tool into the running engine's localhost IPC (`ws://127.0.0.1:$MAST_IPC_PORT`,
default 27654) — the same `mast_rpc` surface the headed app and `zeron sync`
dial. It is a subcommand of the one `zeron` binary: no Node runtime, no extra
install, a few MB resident.

Crate: `crates/mcp` (`mast-mcp`). The protocol layer is hand-rolled
(`initialize`, `ping`, `tools/list`, `tools/call`; newline-delimited JSON-RPC
2.0) — the repo already owns JSON-RPC framing for the Codex and ACP drivers and
the stdio tool-server subset is tiny, so no SDK dependency was taken.

## Identity

The engine injects this server into Claude Code and Codex Runs of Chats that
belong to a project under the server name `mast-agent` (Claude: `--mcp-config
<owner-only file>`, tools surface as `mcp__mast-agent__<tool>`; Codex: per-thread
`mcp_servers.mast-agent.*` config overrides). The distinct name keeps it from
colliding with a server the user configured themselves (for example a
terminal `mast mcp`). Before the rename the server was called `zeron-agent`:
only new Runs get the new name (a Run already in progress keeps its tools), the
engine never matches on the name, and old transcripts that mention
`mcp__zeron-agent__<tool>` still render. The token and port reach the child as
`MAST_AGENT_TOKEN` / `MAST_IPC_PORT`, and the old `ZERON_*` names are exported
alongside and still read. It is launched with:

| Variable            | Meaning                                                     |
| ------------------- | ----------------------------------------------------------- |
| `MAST_IPC_PORT`    | Engine to proxy (default 27654).                            |
| `MAST_AGENT_TOKEN` | Engine-minted per-Run capability token (revoked at Run end). |

Every tool call re-authenticates the token (`AgentAuthenticate`); the engine
answers with the Chat the token was minted for. `MAST_CHAT_ID` is no longer
read: no self-declared origin is trusted. With a token, every `send_message` is
prefixed with a `[Message from Mast chat <title> (<id8>) …]` line, the server
refuses to message its own chat, and the Work Outline agent tools
(`work_outline_*`, forwarded to `AgentToolCall`) are listed. The server
instructions and the create/resolve tool descriptions tell agents to raise
blocking decisions, approvals, or missing information with
`work_outline_create_user_action_request` (in addition to asking in chat),
resolve the request once the user answers (including a `Re: "…"` chat message
from Required Actions' Reply & resolve), and use the built-in quick question
prompt only for non-blocking clarifications; `work_outline_read_current` also
returns the item's open requests. The server instructions (`INSTRUCTIONS`
in `crates/mcp/src/jsonrpc.rs`) also tell agents to call `bind_chat_worktree`
right after `git worktree add`. Claude Code truncates server instructions at
about 2000 characters, so `INSTRUCTIONS` is held to 1900 (a test enforces it),
most important guidance first; detail that does not fit lives in the tool
descriptions. A token also scopes
the original tools to the token's Chat, project, and device:

| Tool | With a token |
| --- | --- |
| `create_chat` | Needs an `idempotency_key` (same key → same child chat, never a second first turn; same key with other inputs → `operationConflict`). Only in the token's project on the token's device, only as a child of the caller, `cwd` inside the project folder or the caller's cwd, sandbox never wider than the caller's (an omitted sandbox is capped, a wider one refused), approval mode always Ask. The child's first turn is rebuilt by the engine from the child's stored config. |
| `interrupt_chat`, `respond_to_input`, `wait_for_turn`, `archive_chat` | Only chats the caller created. |
| `send_message` | Chats the caller created, or its parent chat (`parentChatId`; see `report_to_parent`). Anything else: "you can message chats you created and your parent chat". |
| `report_to_parent` | Needs a parent chat (errors clearly without one). Never crosses Spaces. |
| `get_chat` | Chats the caller created, or the caller itself. |
| `read_chat` | Those chats, non-archived chats in the caller's item subtree or project, and recorded handoff predecessors in that scope even after archiving. |
| `list_chats` | Lists only the caller and the chats it created. |
| `list_projects`, `list_devices` | Only the token's project / device. |
| `whoami`, `list_harnesses`, `list_models` | Unscoped (this device's capabilities). |

### Project manager grant

The chat that holds a project's **manager** role (the standing role record,
`role: manager`, scope item = the project) gets a standing grant over that
project's subtree. It has no expiry and belongs to the role, not the chat, so it
moves to the successor on hand-off and ends for the predecessor at the same
moment. The engine re-derives it from the synced role record on every
`AgentAuthenticate` and `AgentToolCall`, exactly as it does the personal
assistant flag and admin grants, and `AgentOrigin.managerScopeItemId` carries
the project item. A chat with no role, a superseded predecessor, a chat holding
the `assistant` role, and a manager whose scope item is archived or missing
have no grant.

| Tool | With the grant |
| --- | --- |
| `read_chat`, `send_message`, `wait_for_turn`, `interrupt_chat`, `respond_to_input`, `get_chat`, `list_chats` | Every chat assigned to an item inside the project, however it was started (including by hand), plus every chat that held a role scoped inside the project (its own archived predecessors, which `list_chats` never shows but `read_chat` reads by id), never itself. Archived chats only for `read_chat`, `get_chat`, `list_chats` and `archive_chat`. The grant bounds the manager's reach: a chat it created or a parent link that points outside the project is refused like any other chat of another project, and the manager's own other routes (children, parent, item subtree) add nothing. Chats of any other project or Space are refused as if they did not exist. |
| `archive_chat` | The same chats, with the usual finished-worker check (`force` with a reason otherwise). |
| `work_outline_admin` | Steps on items and chats inside the project only. A step that names an item outside the subtree, creates or moves at the Space root, adds a Space-wide instruction, assigns a chat that is not already in the project, or edits, moves, archives or re-flags the project item itself (it is a container: the manager adds and moves things under it) is refused before anything runs, and the registry writer re-checks the holder and every target under its lock immediately before each write, so a concurrent move or hand-off cannot slip one through. The bulk count is the concrete effect: an archive or restore counts its whole subtree and a template apply counts each child, with ids resolved the way execution resolves them. The approval request is anchored to the project item and is per holder, so a successor's is a distinct request. No instruction-proposal steps, and `read` shows only the subtree. Over 10 changes still opens the "Approve bulk change" Required Action and needs the user's approval (the confirm token is per chat). Attributed "by project manager <title>", never as the user. |

The workflow-pack tools stay admin-only. Every other tool is unchanged.

Precedence: a grant the user made on purpose wins. A chat that also holds a Space admin grant, or is the personal assistant, gets that wider reach in every tool, and the manager's narrower reach does not apply on top of it (the engine leaves `managerScopeItemId` empty for it).

The writer also re-reads, under its lock, the current assignment of any chat a step reassigns, and re-counts a cascading step (an archive or restore covers the subtree as it is now, a template apply covers the template's nodes under each child) against what the user approved, or against the 10-item threshold when no approval was needed. Before any write, the batch is priced in order on a scratch copy of the registry, so a template or new item an early step sets up counts toward the later steps; a step an earlier attempt already committed costs nothing. The budget is one per batch (chat, idempotency key and steps), shared by the call, its retries and a restart: the writer reserves each step's charge under its lock and persists it before the write, and gives it back if the write does not commit, so a crash can only over-count and cumulative effects cannot pass the approved total. A fresh approval is for what is left, on top of what is already spent. A create counts the template it expands under its parent. A step that would pass what is left is refused with nothing changed. The approval records the count the user was shown, a confirm token is refused if the batch now affects more, and the request's operation id and dedupe key include that count, so a preview that grew is a new request rather than a replay of (or conflict with) the old one. More exactly, the request and its token are bound to one execution (the idempotency key) and to the resolved preview (target ids and counts, not just the raw steps and a count). A token is claimed atomically by one execution; its retries resume that claim even after their own earlier steps moved the preview, and no other execution, chat or changed preview can use it. Only the guarded bulk approval may raise its Required Action on the project item; every other request still sits on the chat's own item.

Mixed versions: the field is optional on the wire. An engine that predates the
grant sends none, and a client that predates it ignores it, so the grant is
simply absent there and a manager is refused its project's chats (never
widened). The engine advertises `project-manager-grant-v1`; place a manager
only on a host that lists it. The chat tools learn the project's chats from
`chat_project_scope_chats` (`managerChatIds`, empty for everyone else).

Without a token (a human running `mast mcp` in a terminal) the original tools
work unattributed and unscoped. Contract:
`docs/plans/work-outline/m0-contract.md`, "Milestone 4 agent tool discoveries".

### Parent links

A chat created through `create_chat` records the creating chat as its parent:
`Chat.parent_chat_id` (proto) ⇄ `parentChatId` on the registry/workspace chat
row (`Mutate createChat { parentChatId? }` → `WorkspaceHost::create_chat_with_parent`).
The default is the token's chat; for a human caller (no token) an explicit `parent` argument
(id, prefix, or title) overrides it. `list_chats { parent }` returns a chat's
children, and every chat summary carries `parentChatId`. The field is additive
and serde-defaulted: rows written by older engines read as parentless, and a
dangling id (parent deleted) is tolerated rather than cascaded.

The left sidebar hides chats that have a parent: `AppState::visible_chats`
(the Sessions list, project tabs, jump slots) and the Archived section both
require `parent_chat_id == None`. Children remain addressable by id, deep
link, and every MCP tool; `list_chats { parent }` is how an orchestrator
finds them.

Only Claude Code and Codex Runs of project chats get the server; projectless
chats, ACP agents, Cursor, opencode, and title runs launch exactly as before.

## Tools

Chats are referenced by full id, a unique id prefix, or an exact title.
Projects by id, path, display name, or unique path suffix. Devices by id or
name (default: the local engine's device).

| Tool               | Engine calls                                              |
| ------------------ | --------------------------------------------------------- |
| `whoami`           | `LocalDevice`, `EngineInfo`, origin chat summary          |
| `list_devices`     | `WatchDevices` snapshot                                   |
| `list_projects`    | `WatchSpaces` snapshot                                    |
| `list_harnesses`   | `ListHarnesses`                                           |
| `list_models`      | `ListModels {harness}`                                    |
| `list_chats`       | `WatchChats` + `WatchSessions` snapshots (status merged)  |
| `get_chat`         | above + `WatchDocMessages` opening frame (pending input)  |
| `create_chat`      | `Mutate createChat` (+ `renameChat`; optional first send) |
| `read_chat`        | `WatchDocMessages` opening `reset` frame, rendered        |
| `send_message`     | `QueueCommand` Run / Steer, or `QueueMessage`             |
| `report_to_parent` | `ReportToParent` (wake queue, "worker report" event)      |
| `wait_for_turn`    | `WatchSessions` until the chat settles                    |
| `interrupt_chat`   | `QueueCommand` Interrupt                                  |
| `respond_to_input` | `QueueCommand` RespondInput                               |
| `archive_chat`     | `Mutate setChatArchived`                                  |

Watch streams are the engine's only read surface (there is no one-shot "get
transcript" RPC); a snapshot is "subscribe, take the first item, drop" — drop
cancels server-side, exactly what the sidebar does on attach.

A message to the parent chat (and `report_to_parent`) is not a direct send: the
engine queues a "worker report" wake event (text capped at 4 KB, cut text marked)
and delivers it in the parent's next "Worker updates:" batch, only while the
parent is idle and the user is not drafting or recently active there (a user
send interrupts a wake turn). Replies `{delivery: "queuedForParent",
heldReason?}`, or `parentReportsOff` when the parent switched worker reports off
in its Team tab (on by default). Only the device that hosts the parent delivers,
so a worker on another device than its parent gets an error and should rely on
`read_chat` / Required Actions.

`send_message` mode `auto` mirrors the composer: idle → `run`; working → `steer`
when the harness steers mid-turn (claude, codex), else a queue row held for the
end of the turn; `awaitingInput` → refuses and points at `respond_to_input`.
A working row older than 45 s is treated as idle (the UI's staleness window).

`wait_for_turn` after a send is edge-triggered on the `Session` row captured
before the send: it returns on a new `last_completed_turn`, an
`awaitingInput`/`errored` stamp newer than the baseline, or a working→idle
edge. A brand-new chat has no session row until the host picks the run up, so
the wait keeps waiting in that case rather than reporting the unstarted run as
done (this was the one bug the first live run found).

## Smoke recipe

```sh
BIN=target/debug/mast
{ echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_chats","arguments":{"limit":5}}}'
  sleep 5; } | MAST_IPC_PORT=27655 $BIN mcp
```

`create_chat` with `"prompt": "Reply with exactly the word pong", "wait": true`
against a live daemon returns the assistant's `pong` in a few seconds; archive
the chat afterwards with `archive_chat`. Unit tests (`cargo test -p mast-mcp`)
drive the whole tool set against an in-memory stub `RpcService`.
