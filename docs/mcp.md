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
| `briefing_read`    | `AgentToolCall` (read-only; latest results artifacts)     |

Watch streams are the engine's only read surface (there is no one-shot "get
transcript" RPC); a snapshot is "subscribe, take the first item, drop" — drop
cancels server-side, exactly what the sidebar does on attach.

`briefing_read` returns the latest results artifact of one scheduled task, or of
every task in the caller's Space, so a chat on any work item can see what a
briefing found without `read_chat` or Notion. It takes optional `task` (a task
name, task id or results artifact id; omitted means all) and `index` (run times
only, no content). It replies `{briefings: [{task, taskId, artifactId, title,
revision, runAt, runAtLocal, runId, workItemId, content, contentTruncated,
contentAvailable}], needYou}`.

- `task` is the task's current name when this device knows the task and it
  belongs to the briefing's Space, so a renamed task is found under its new
  name, its old name and its id. A task's name is never applied to a briefing
  in another Space.
- `runAt` is when the newest genuine revision was published, in epoch
  milliseconds, and is the same on every device. `runAtLocal` and the chat-start
  index show it in the time zone of the device hosting the chat.
- `content` is that revision exactly as recorded, never a later edit of the
  file. It is capped at 128 KB for one briefing and 32 KB each for several,
  with `contentTruncated` set when cut.
- `contentAvailable` depends on the request. With content, true means the
  content was read; false means it was not read, either because the file lives
  on another device or because its recorded content could not be read. With
  `index: true`, it only says the file is on the device answering and promises
  nothing about a later read, which can still fail. Open the artifact in Mast
  when content is missing.
- `needYou` counts open Required Actions from scheduled tasks. For all tasks it
  is every one in the Space, including failed runs and tasks that have yet to
  publish. For a named task it is only that task's own, found by task identity
  (the hash in its results role, which a failed run's key also reduces to), so
  another task sharing the work item is not counted and no local schedule is
  needed.
- Only a Background run of the task can have written a result. An ordinary chat
  cannot publish the reserved `scheduled-results:` role. Each revision a
  Background run publishes is stamped with its task identity, and the newest
  stamped revision is the one returned. That revision is kept past the
  artifact's revision-count and size limits, so later edits by anyone else never
  push it out or hide it, and deleting the publishing chat does not hide it.
  Revisions published before stamps existed are stamped, on the owning device,
  at startup and on the next read while their publishing chat still exists; one
  whose chat was already deleted before that cannot be verified and is not
  returned.
- An unknown task is an error that lists the tasks that do have results. Hosts
  that predate the tool answer `unknown agent tool`.

The lookup is `crates/engine/src/briefing.rs` and is built to be reused. A caller
builds a `BriefingQuery` for a `BriefingKind` (which artifacts count) and passes a
`BriefingScope`. Its `permits` runs on every candidate before it is listed or its
bytes are read, and a candidate it refuses looks the same as one that does not
exist; `permits_work_item` does the same for Required Action counts and has no
default, so a scope must decide for itself. Each kind also names who may have
written its artifacts (scheduled results use `run_provenance`): the lookup is
pinned to the newest revision that passes, and an artifact with none is treated
as absent. Scheduled results
use `SpaceScope`. Manager sessions add a `BriefingKind::ManagerStatus` for its
status artifact, its own provenance, and a scope that permits only artifacts and
work items inside the manager's project subtree.

`briefing_read {kind: "manager-status"}` reads a project's Manager status record
(default kind is `scheduled-results`). The scope is `ProjectSubtreeScope`: the
record on the caller's own item or one of its ancestors, and nothing else. A
chat in another project gets an empty list, and a chat with no work item is
refused. The reply has the same shape, with one entry and `needYou: 0`. An unknown
`kind` is an error.

The Manager status is published with `artifact_publish` and inline `content`
instead of `path`, because a manager cannot write files. `title` must be
`Manager status` and `role` `manager-status`; Mast puts it on the project item, so
every revision, from any chat that held the manager role, adds to one artifact.
Only the current chat of a project's manager may publish it. Any other caller,
a file `path` with that role, `pinAsPlan`, or another title or role is refused.
The document must start with `Updated: <date and time>` and have exactly the
level-two sections Goals and priorities, Active work items and workers,
Decisions, Open questions, Standing preferences and Recent user requests, in that
order. `content` and `kind` are optional fields: a host that predates them
rejects the call or reads the default kind, and nothing else changes.

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
