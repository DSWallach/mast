# Mast MCP server

`mast mcp` serves the Model Context Protocol on stdin/stdout and proxies every
tool into the running engine's localhost IPC (`ws://127.0.0.1:$MAST_IPC_PORT`,
default 27654) — the same `mast_rpc` surface the headed app and `zeron sync`
dial. It is a subcommand of the `mast` binary: no Node runtime, no extra
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
| `create_chat` | Requires `idempotency_key` (or `idempotencyKey`) in a Mast chat. With `workItemId`, reuses a live chat on that item and sends the prompt with auto delivery; `newChat: true` forces a new chat. The item must be the caller's, a descendant, or inside its project. A new item chat requires `prompt` and rejects `cwd` and `branch`; it uses the item's instructions and execution setup. If a project has a worker destination, it is used. Without `workItemId`, a prompt in such a project starts a worker on a new item there. A child has a sandbox no wider than the caller's and uses Ask unless `autoApprove: true` is requested by a caller on Auto. Reusing a key with different inputs is an error. |
| `interrupt_chat`, `respond_to_input` | Only chats the caller created. |
| `wait_for_turn`, `read_chat` | Chats the caller created, plus non-archived chats on items in its subtree or project; `read_chat` also reads the caller. |
| `archive_chat` | Chats the caller created; eligible project workers can also be archived. |
| `send_message` | Chats the caller created, its parent, or non-archived chats on items in its subtree or project. A non-child, non-parent target cannot have a wider sandbox or auto-approval when the caller is on Ask. |
| `report_to_parent` | Needs a parent chat. |
| `list_chats` | Lists the caller and chats it created; `workers: true` also lists workers in project scope. |
| `get_chat` | Reads a reachable chat’s metadata and status. |
| `list_projects`, `list_devices` | Only the token's project / device. |
| `whoami`, `list_harnesses`, `list_models` | Unscoped (this device's capabilities). |

Without a token (a human running `mast mcp` in a terminal) the original tools
work unattributed and unscoped. Contract:
`docs/plans/work-outline/m0-contract.md`, "Milestone 4 agent tool discoveries".

### Work item links

`work_outline_create_child` requires `title` and `idempotencyKey`. It accepts `linearIssue` as a Linear issue identifier or URL, as well as `description`, `parentWorkItemId` and `status` (`pending` or `active`). The parent can be the caller's item, a descendant, or an item in its project. Without a parent, the project's worker destination is used when configured; otherwise the caller's item is used.

`work_outline_link` requires `workItemId` and `idempotencyKey`, and exactly one of `linearIssue` or `pullRequest`. It attaches that reference to the caller's item or a descendant. It makes no network request.

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
in its Tracker tab (on by default). Only the device that hosts the parent delivers,
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
