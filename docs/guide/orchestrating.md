# Orchestrating with coordinator and worker chats

One chat (the coordinator) can start other chats (workers) on items below its own and collect their results. You talk to the coordinator. The workers do the work in their own worktrees.

## How it works

Every Claude Code and Codex chat in a project gets the `mast-agent` MCP server. The coordinator uses these tools:

- `work_outline_create_child` makes a child item. Pass `linearIssue` with an issue identifier or URL to link it.
- `work_outline_link` attaches a Linear issue or GitHub PR to the caller’s item or a descendant.
- `work_item_external_run_upsert` records a run that executes outside a local harness, such as a codespace, on the caller's item or a descendant, and `work_item_external_run_list` reads them back. Record a run when you start it and end it (`expectedAlive: false` or an `outcome`) when it finishes. The Tracker lists the runs on the coordinator's items, with what the hosting device last observed about each (see Watching external runs).
- `create_chat { workItemId, prompt, model, idempotency_key }` starts a worker on its item, a descendant, or another item in its project. It reuses a live chat on that item and sends the prompt with the composer's auto delivery (a running worker is steered, unlike an agent's plain `send_message` note); set `newChat: true` to force a new chat. It uses the same launch path as **Start Chat**, so the target item's instructions and execution setup apply. A worker is never given a wider sandbox than its parent. A Manager's worker started without an explicit config uses the Manager's approval mode, so a Manager on Auto starts workers on Auto. Pass `contextWindow` (a choice from `list_models`: for Claude default, 250k, 500k, 750k or 1m; for Codex a token count the model lists) to cap its context; a Codex worker started without it gets the Space's 200K default, which keeps long worker sessions from resending hundreds of thousands of tokens on every call.
- `work_outline_read_subtree` lists the items, chats and open Required Actions below the coordinator.
- `wait_for_turn`, `read_chat` and `list_chats` read a worker's progress and reply.
- `send_message` sends a worker a follow-up.
- `work_outline_resolve_user_action_request` resolves a request a direct child raised.
- `archive_chat` archives a worker once it is finished (see below).

Workers can report back to their coordinator. They can also raise a Required Action, which the coordinator finds with `work_outline_read_subtree`.

## Watching external runs

The device that owns the Space watches its live runs with your own `gh` login. A run is live while it is expected alive, not ended, on an item that is not archived, and updated within the Space's stale-days setting. The watcher is read-only. It lists your codespaces and reads each box's run directory and tmux session. It never starts, stops, creates or deletes a box, and it never signs in. If `gh` is signed out or its login lacks the codespace scope, every live run reads "unknown" with the reason, and the watcher retries every 15 minutes. If `gh` is not installed when Mast starts, no watcher is created until the next start.

What it sees stays on that device. Other devices show the recorded run and "not observed on this device".

A run reads as one of running, done, failed(N), orphaned, stalled, needs human or unknown.

- **Orphaned** means "no exit seen". The box is shut down, or its tmux session is gone, and no exit file was read. The watcher never wakes a stopped box to check. A run that finished and then idled out between two looks looks the same as one that was killed.
- **Unknown** covers a transient box state (starting, shutting down), a failed probe, a `gh` problem, and the first look after Mast restarts.
- **Needs human** is a running run whose item has an open Required Action.

Setting up: each Space has a capacity cap (default 5, because `gh` does not report it), an optional box limit (GitHub's limit on codespaces that exist; Shutdown boxes count and only deleting one frees a slot, so the header shows `6/15 boxes` and nothing wakes when it is reached), a stale-days limit (default 7), and the conventions for probing a box: the run directory, the exit file, an optional output file and the tmux session. The directory and session may use `{runTag}`. With no run directory set, boxes are listed but never probed, and runs read "unknown: probe not configured".

Cost: a list call every 3 minutes while any run is live (every minute for 5 minutes after a change), none while idle, and at most one `gh codespace ssh` per box every 5 minutes. A failing call backs off, doubling up to 15 minutes. Several runs on one box share one ssh call.

Wakes: the run's owner chat, if it is an orchestrator, otherwise the item's orchestrator, is woken when a run finishes, fails or is orphaned (after two consecutive looks), when the number of running boxes reaches the cap, and once when a run has been unknown for 15 minutes. A restart does not repeat a wake. This follows the chat's master wake switch only.

A probe can count as box activity and delay idle shutdown; this has not been verified on a real box.

One race remains. `gh codespace ssh` starts a box that is not running. The watcher re-reads a box's state just before connecting (and skips a box within 90 seconds of its idle-out time), but a box that idles out in the few seconds between that read and the connection can be woken briefly. It idles out again on its own.

## The Ready-to-merge flow

1. The coordinator creates an item per task and starts a worker chat on each with a self-contained prompt.
2. Each worker makes its own branch and git worktree, calls `bind_chat_worktree` so the Changes pane shows that worktree, does the work, pushes the branch and marks its item completed.
3. The worker raises a Required Action titled **Ready to merge: <branch>**. The detail says what changed, which tests ran and what to try.
4. The coordinator sees the action, merges the branch, and resolves the action.
5. Meanwhile the status light and Dock badge show you which chats are working and which need you. A **Ready to merge** action counts as ready, not as needing you.

The merge step belongs to one chat. Say so in the instructions on the coordinator's item.

## Hand off to a new chat

From any active chat, choose **Hand off to new chat…** from its chat menu or header overflow. Review the prefilled summary and worker list, choose whether to archive the source, and confirm. Mast selects the successor, transfers active workers and queued wakes, and links the old chat to the successor. The successor starts on the same item with the summary, a reference to the source conversation, workers and open requests. Solo chats can hand off without workers. For remote chats, update Mast on the hosting device if handoff support is unavailable. The successor can read its recorded predecessors even after you archive them. A chat without an item gets one automatically. The successor keeps the source's working directory or worktree, session settings and managed account when the harness stays the same. It also keeps the source's title, whether you set it or Mast generated it, with no suffix; a source without a title leaves the successor to be titled automatically.

When the chat holds a standing role (a project's manager or the assistant), the hand-off also moves the role to the successor and always archives the source, whatever the archive choice in the dialog says. The old chat disappears from the sidebar, Overview, the archived list and `list_chats` (archived or not) but stays readable with `read_chat`. If you have it open, the pane switches to the successor on its own.

When a pinned chat hands off, its successor takes its place in **Pinned**. If the successor already has a pin, it keeps its position and Mast removes the source pin. This also applies when a Manager hands off automatically, including after a credit or usage limit. Later pin edits stay in effect when Mast sees the same hand-off again.

A role chat can only hand off through a host that moves roles. If the chat's host is older, Mast stops before starting anything and asks you to update that device. A hand-off that is interrupted midway keeps the role on the old chat; hand off again with the same summary and settings to finish it, and no second successor is started. Retrying a hand-off that already finished returns the same successor, even if that successor has since handed off again. Asking again from a reopened dialog or after a restart with the same summary and settings resumes the interrupted hand-off instead of starting another.

## The Manager status record

A project's manager keeps one artifact on the project item, **Manager status**, with a new revision for every update. It is what a replacement manager starts from after a hand-off, and you can open it from the item like any artifact. Its sections are fixed: goals and priorities, active work items and their workers, decisions, open questions, standing preferences, and your last five requests word for word. It begins with an `Updated:` line.

Mast watches a manager's context against the limit chosen for its chat (the Context Window choice, or the model's own window when none is chosen). At 80 percent of it Mast asks the manager to publish a fresh status; at 92 percent it hands off immediately from the latest status. Changing the limit applies from the next turn, and lowering it below the current usage can trigger a hand-off then. The successor starts from that status, the role moves to it, and messages you send to the old chat from the composer go to the successor. A system notice in the old chat says when a hand-off is paused or its first turn was rejected, and Mast retries with the same successor. If another hand-off already owns the role, the automatic one pauses and clears itself once the role has moved. The manager can also publish a status manually after changing project state.

The personal assistant, when it holds the standing Assistant role, hands off on its first idle turn at 80 percent of its chosen context limit. It has no status to checkpoint, so the successor is told to read the old chat. The designation and the role move together, and the same pause-and-retry rules apply. This runs whatever the scheduled task's refresh setting says, because it protects against a full context window rather than scheduling a fresh chat.

Upgrading preserves existing queued messages. Reminders queued by an earlier build have no reliable marker that distinguishes them from ordinary messages, so they are not automatically removed; a prompt already accepted by a harness cannot be recalled. Older hosts can continue to create or deliver reminders until they are upgraded or fenced.

The Manager's role instructions tell it to publish a new status at the end of every turn that changed state. Mast does not send a per-turn reminder when one is missed; the 80 percent checkpoint request above still applies.

The manager cannot write files, so it publishes the document as text with `artifact_publish`. Any chat inside the project can read the latest revision with `briefing_read` and `kind: "manager-status"`. Chats in other projects cannot see it.

## Queue wakes

When a project's work queue has a ready item and fewer than one queued worker is running, the Manager gets a **Work queue** line in its next update, naming the item. Start a worker on it with `create_chat` and that item's `workItemId`, or say why you are not starting it; the item is not offered again until it stops being ready and becomes ready again, except that an offer no worker answered is repeated once after each engine restart (the wake is held in memory) and, once three offers went unanswered, the next restart raises a Required Action for you. Check for an existing worker before starting one. An item whose worker is still working, or was active in the last 30 minutes without erroring, counts as running and holds the single queue slot; an item whose worker errored or has been idle longer is stalled, shown with its reason, and does not block the next offer. An item whose pull request has merged is done and is never offered; an item whose pull request status is not known yet is held until it is. An item can name prerequisites with `work_queue_set` (`prerequisites`, items of the same project). It then shows as waiting, with the prerequisite and why, and is not offered until each one is complete; entries behind it are still offered. Each item has a completion (`completion`): `reported` (the default) is met by a final handback, a merged pull request or a completed item, while `landed` is met only by a merged pull request or a person completing the item, so a worker's report, passing tests and an agent marking the item completed are not enough. A person lands an item by completing it in its details, which also takes over a completion an agent made. A prerequisite that is not an item of the project, or that waits back on this item, keeps it waiting. A project with no Manager has one started by the queue instead. Users can pause this per project, from the Tracker's **Queue** or the Work queue section of the project's details.

The project's Manager manages the queue itself with `work_queue_read`, `work_queue_set` and `work_queue_pause`, and an admin chat can do the same for any project in its Space. `work_queue_set` places, moves or removes an entry (anchors must be queued in the same project) and can mark an item offloaded, so it reads blocked with a reason, until it is resumed. A Manager's pause and resume need a reason (an admin's is optional); the pause is recorded as the Manager's and shows with its reason beside the pause in the Tracker and on the project page. A resume's reason is not kept. A Manager can lift only a pause a Manager set: a pause set by the user or an admin can be lifted only by a user or admin. Workers, other projects' Managers and a Manager that has handed off are refused, and the role is checked on every call.

## Tracker

The **Tracker** tab replaces Team. It shows ticket-first rows with linked chats and PRs, activity, and a stage track based on observed work. Expand a row for its chats. **Review PR** opens the review flow; **Merge…** opens the PR when available. Other chats and stale work appear in their own groups. When the project has queued work, a **Queue** lists it in queue order as the same rows the Tracker shows everywhere else, each with its position and state, **Up**, **Down** and **Remove**, plus a **Pause auto-start** toggle. A queued item appears once, in the Queue, whatever its stage. Not started and Building work that is not queued follows under **Not queued**, each row with an **Add** button. A queued item whose PR merged leaves the Queue for Done. An archive banner offers to archive finished workers.

Each section, such as the Queue, Runs, Not queued, a stage group, Stale and Other chats, has its own heading that collapses and expands it, and Mast remembers the choice across tabs and restarts. A section you have not touched shows its default: everything is expanded except Done with more than five rows, Stale and Other chats, which start collapsed. Your own toggle always overrides that default, so a Done list you expanded stays open however long it gets. A collapsed Queue hides its rows but keeps its heading, **Pause auto-start** and any paused or held notice, and says how many queued entries need you, counting only those the active filter would show.

## Start a review when testing finishes

A worker that finishes testing a commit reports it with `report_to_parent`: `text` as usual, plus `completion` with `testedSha` (7 to 64 hex digits) and `evidence` (what ran and what it showed). Mast then starts one reviewer chat on that commit, under the same work item and the project's Manager, and adds a "Review dispatched" line with the chat id to the report the Manager reads. The Manager wakes on the report as for any worker report, so no user message is needed. The line goes to the worker's parent chat, because that is where its reports go. The reviewer is started under the project's current Manager. Those are usually the same chat, since a Manager handoff moves the old Manager's active workers to the new one. They differ for a worker whose parent is another chat, or one that was archived or created with the old Manager as parent: its report, with the line, goes to that parent while the reviewer reports to the current Manager. If that parent is archived, the reviewer is still started and recorded, but the report fails to deliver, so hand off or unarchive the parent and report again to see the line.

One reviewer exists per work item and commit. Reporting the same commit again, a restart, or a report from a different worker on the item finds the same review instead of starting another. A new commit starts a new review. A reviewer that reports back does not start another. `completion` cannot be combined with `finalHandback`: report the commit first, then hand back. The record of each dispatch (item, commit, operation id, worker, reviewer chat, state) is kept in `review-dispatches.json` in the engine's data folder.

This only runs where one engine is the sole authority: once a workspace syncs through a server, Mast records the dispatch as blocked (owner: you) and starts nothing, so start the reviewer by hand. The reviewer uses the host's default harness and model.

## Archive finished workers

A worker can explicitly relinquish its assignment with `report_to_parent`. Pass the final report as `text` and set `finalHandback` to an object with `assignmentGeneration` from `work_outline_read_current` and an `idempotencyKey` that stays the same on retries. Mast can archive the chat once the report is saved in its Manager’s transcript and the worker has no pending input, queued work, or open Required Actions. Set `keepChat: true` in `finalHandback` to keep the worker’s chat available. The final handback requires a worker and Manager on the same device. Editing or removing the queued report in the Manager chat cancels automatic archive for that handback.

On a big project the workers pile up. A worker is **finished** (`archivable: true` in `list_chats` / `get_chat`, with an `archivableReason`) when it is idle, has no open Required Actions, and any one of these holds: its item is completed; its linked PR is merged or closed; its **Ready to merge** was resolved; or it has been idle for more than 3 days with its item completed or archived.

- With **Archive workers after N days** on for a Space, a daily pass reconciles every chat the Tracker counts, including chats that predate the setting and chats assigned to items below an orchestrating chat's item. It archives a chat only when its work item is completed, the chat is idle with no open Required Action, and it has been quiet for N days. A merged PR, a resolved **Ready to merge** or idle time alone never archives a chat automatically; those chats still show "can archive" for the manual **Archive finished** action. An idle chat the pass would not archive shows "not auto-archived" with the reason, such as its work item not being completed. Complete the item, or archive the chat yourself.
- A coordinator archives finished workers it started with `archive_chat`. Pass `workers: true` to `list_chats` to see the workers in the project scope (chats under the project's worker destination) and archive those too.
- A chat that isn't finished is refused unless the call passes `force: true` and a short `reason`. The reason is logged.
- Archiving hides a chat; it can always be restored. The Tracker tab’s **Show archived (N)** lists archived workers, each with **Unarchive**.
- In the Tracker tab, **Archive finished (N)** lists the chats and asks before archiving them all. With three or more finished workers the tab says so.
- A coordinator never archives itself or chats outside its project.

## Coordinator tips

- Don't block on one worker. A worker that runs tests in the background goes idle before it is finished, and one that pushes a checkpoint is not done either. The **Ready to merge** action is the real signal.
- Keep the coordinator thin. A long-lived coordinator with a huge context is the most expensive chat you have. Keep state in a doc in the repo, and restart the coordinator often.
- Put big or noisy work (screenshots, design rounds, long searches) in workers.
- Long Required Action details come back cut short. Use `read_chat` on the action's origin chat for the full text.

## Prompts

Put the standing rules in the instructions on the coordinator's item, so every chat started from it inherits them. For example:

```
You are the coordinator for this project. Talk to me; delegate work.
- Create or find an item for each task, then start a worker with create_chat
  { workItemId, prompt, model, idempotency_key, wait: false }.
- Never wait on one worker in the foreground. Read the subtree for open
  "Ready to merge" actions.
- Only you merge, release and edit the handoff doc.
- Archive finished workers: when a worker you started shows
  archivable: true, archive it with archive_chat.
- Workers work on a feature branch in a git worktree, never on main.
```

A worker prompt should stand on its own:

```
Goal: <one sentence>.
Files you own: <paths>. Don't edit anything else.
Done when: <tests pass / behaviour you can try>.
Work in a git worktree on branch <name>, and call bind_chat_worktree.
Commit early and push checkpoints. When finished, push, mark your item
completed and raise a Required Action "Ready to merge: <name>" saying
what changed, which tests you ran and what to try. Do not merge.
```

## Admin chats

An admin chat can do what you can do in the app, across one Space, so it can reorganise a workspace or change project process without you clicking through it.

**Granting.** Only you can grant it: right-click a chat (or open its Item tab) and choose **Grant admin for this Space…**. Pick a length (1, 4, 8 or 24 hours, or until revoked; 4 hours is the default) and, if you trust a cleanup run, **Skip bulk confirmations** (off by default). The chat shows a shield badge, "Admin · <Space> · 3h left", in its header and Item tab with a **Revoke** button. Revoking or expiry stops the chat at its next call, even mid-sweep. No agent tool can grant admin, and an admin can't grant it to another chat.

**What it can do, inside the granted Space only:** read, list and message any chat; archive chats; start chats on any item; rename, create, move, group, archive and restore items; set status, project flag, worker destination and "Convert to project"; add, edit and remove instructions directly; accept or dismiss instruction proposals; move chats between items; resolve Required Actions. These go through `work_outline_admin`.

**What it can't do:** touch another Space; write to Linear or GitHub; delete anything for good (it archives); change another chat's sandbox; read `<home>/.claude` or `<home>/.codex`. Admin doesn't widen the chat's own sandbox either.

**Safety rails.**
- Everything is attributed "by admin chat <title> (granted by <you>)", never as you. When an admin resolves a request, the note says so, so a worker whose rule is "only my answer counts" can tell the difference.
- One admin turn is one entry in Recent Changes (⌘Z undoes the latest), with each action listed.
- A call that touches more than 10 items runs nothing. It returns a preview and raises a Required Action, **Approve bulk change: …**. Resolve it yourself (an agent's resolve doesn't count); the admin then repeats the same steps with the confirm token (30 minutes, one use).

## Managers are read-only

A project Manager coordinates and does not edit or build. By default its runs are read-only and it can't start shell commands. Workers it starts use the project's worker default model (Sonnet) unless it names one. A Manager chat made before this change picks up its role instructions on your next message to it. If the Manager lives on a device that hasn't updated, Mast asks you to update that device first.

You can turn this off per project with **Runs read-only** in the Manager's Settings tab (On by default). With it Off, the Manager runs with its chat's own sandbox and approvals, so it can run commands; the role instructions and the worker default model still apply. Turning it Off applies from the Manager's next run, never to a turn in progress: that turn keeps its read-only sandbox, and its command tool stays refused until the turn ends. Turning it On stops the Manager's commands at once. A steer that starts a new turn, a retry and a restart all follow the setting as it is when they run, and a hand-off successor follows it when it starts. Only you can change it: a Manager can't turn it off for itself, even when it is also your personal assistant. The personal assistant can change it for you with `mast_manage` (`setManagerReadOnly`), for any project it is not the Manager of.

The Settings tab of the project's Manager chat (open it from the **+** menu next to the chat's tabs) holds **Runs read-only**, **Worker model**, **Fallback chain** and **Hand-off log**; it is the one place to change them. **Manager worker model** picks the model the Manager starts its workers on when it names none (Default is Sonnet 5.5). Pick a harness, then one of the models that harness lists on the device that owns the project; the harness's own default is always offered, and a model you chose earlier stays shown even when the list no longer has it. Only you can change it, and a change applies from the Manager's next worker; workers already running keep theirs. **Manager hand-off log** lists the chats that have held the Manager role, newest first. A Manager hands off to a fresh chat at 80% of its chat's context limit (once it has checkpointed) and always at 92%. Those two percentages are fixed and not settings; the limit they apply to is the chat's Context Window choice. The personal assistant can set the worker model for you with `mast_manage` (`setManagerWorkerModel`). **Manager fallback chain** lists the harnesses and models the project moves to, in order, when its Manager runs out of credits or hits a usage limit; add, reorder and remove entries in the Settings tab. The Manager's role passes to a fresh chat on the first entry after the one it is on that is enabled on this device, and the old chat is archived. An empty chain leaves the Manager stopped, as before; when every entry is used up, the Manager chat says so and the project gets a Required Action. The personal assistant has no fallback chain.

## Try it

1. Make an item "Coordinator" with the instructions above.
2. Start a chat on it and ask: "Create two child items, 'Add a README badge' and 'Fix a typo', and start a worker on each. Don't wait for them."
3. Watch both appear under the item in the outline.
4. When a worker raises **Ready to merge**, open the Required Actions list and read it.
5. Ask the coordinator to merge it and resolve the action.
