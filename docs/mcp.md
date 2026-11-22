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
| `create_chat` | Requires `idempotency_key` (or `idempotencyKey`) in a Mast chat. With `workItemId`, reuses a live chat on that item and sends the prompt with auto delivery; `newChat: true` forces a new chat. The item must be the caller's, a descendant, or inside its project. A new item chat requires `prompt` and rejects `cwd` and `branch`; it uses the item's instructions and execution setup. If a project has a worker destination, it is used. Without `workItemId`, a prompt in such a project starts a worker on a new item there. A child has a sandbox no wider than the caller's and uses Ask unless `autoApprove: true` is requested by a caller on Auto. Optional `contextWindow` (a token count or `default`) must be one of the choices `list_models` shows for the model; it is ignored when an existing chat is reused. A Codex worker started for a work item without it gets the Space's worker context window (200K; the Space's **Worker context window** setting overrides it, and **No cap** (stored as 0) removes it; see Space settings in `docs/work-outline.md`), dropped when the model's cached limit is lower. A Codex chat with no `model` is not clamped. Claude workers and chats that are not workers get no default. Reusing a key with different inputs is an error. |
| `interrupt_chat`, `respond_to_input` | Only chats the caller created. |
| `wait_for_turn`, `read_chat` | Chats the caller created, plus non-archived chats on items in its subtree or project; `read_chat` also reads the caller and recorded handoff predecessors in scope even after archiving. |
| `archive_chat` | Chats the caller created; eligible project workers can also be archived. |
| `send_message` | Chats the caller created, its parent, or non-archived chats on items in its subtree or project. A non-child, non-parent target cannot have a wider sandbox or auto-approval when the caller is on Ask. |
| `report_to_parent` | Needs a parent chat. |
| `list_chats` | Lists the caller and chats it created; `workers: true` also lists workers in project scope. |
| `get_chat` | Reads a reachable chat’s metadata and status. |
| `list_projects`, `list_devices` | Only the token's project / device. |
| `whoami`, `list_harnesses`, `list_models` | Unscoped (this device's capabilities). |

A project Manager can reach chats in its managed project, including archived chats where the action supports them; the grant does not extend to unrelated projects.

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

The writer also re-reads, under its lock, the current assignment of any chat a step reassigns, and re-counts a cascading step (an archive or restore covers the subtree as it is now, a template apply covers the template's nodes under each child) against what the user approved, or against the 10-item threshold when no approval was needed. Before any write, the batch is priced in order on a scratch copy of the registry, so a template or new item an early step sets up counts toward the later steps; a step an earlier attempt already committed costs nothing. The budget is one per batch (chat, idempotency key and steps), shared by the call, its retries and a restart: the writer reserves each step's charge under its lock and persists it before the write, and gives it back if the write does not commit, so a crash can only over-count and cumulative effects cannot pass the approved total. A fresh approval is for what is left, on top of what is already spent, and whether a retry must ask again compares the remaining work with the batch's remaining allowance (the 10-item default only for a new batch). The budget fails closed: if it cannot be saved or was unreadable the step is refused, and a batch that already committed steps but has lost its budget record is not started over (use a new idempotencyKey). A create counts the template it expands under its parent. A step that would pass what is left is refused with nothing changed. The approval records the count the user was shown, a confirm token is refused if the batch now affects more, and the request's operation id and dedupe key include that count, so a preview that grew is a new request rather than a replay of (or conflict with) the old one. More exactly, the request and its token are bound to one execution (the idempotency key) and to the resolved preview (target ids and counts, not just the raw steps and a count). A token is claimed atomically by one execution; its retries resume that claim even after their own earlier steps moved the preview, and no other execution, chat or changed preview can use it. The preview digest covers what is displayed and which items each cascading step affects, and the writer re-checks the affected items under its lock. Confirming before the user approves, or a dry run, only validates: the token is claimed, with its allowance installed in the same step, when the approved batch starts to run. Only the guarded bulk approval may raise its Required Action on the project item; every other request still sits on the chat's own item.

Mixed versions: the field is optional on the wire. An engine that predates the
grant sends none, and a client that predates it ignores it, so the grant is
simply absent there and a manager is refused its project's chats (never
widened). The engine advertises `project-manager-grant-v1`; place a manager
only on a host that lists it. The chat tools learn the project's chats from
`chat_project_scope_chats` (`managerChatIds`, empty for everyone else).

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
  Revisions published before stamps existed are stamped by the owning device,
  never by a read (reading changes nothing): once at startup after the registry
  has synced, and when the publishing chat is deleted on that device, while the
  chat still proves them. Until then they are verified through their chat; one
  whose chat is already gone unstamped cannot be verified and is not returned.
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
record on the nearest project around the caller (a manager's own project; for
any other chat, the nearest project at or above its item), and nothing else. A
nested project's chat never receives the enclosing project's record, and a chat
never receives another project's. A chat outside any project, or with no work
item, gets an empty list. The reply has the same shape, with one entry and `needYou: 0`. An unknown
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
