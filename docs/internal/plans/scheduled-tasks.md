# Scheduled tasks in Mast

Status: native implementation built on `david/scheduled-tasks`, 2026-11-11. Existing Claude schedules have not been changed or executed. Real-task cutover remains an explicit per-task action.

## Implementation notes

The native task manager is available from the sidebar clock button. It includes task editing, Space/device selection, shared model-catalog choices for model/thinking/context, schedule previews and presets, pause/enable, Run now, deletion, Claude import preview, and session history. New tasks default to Background on supported hosts and require a work item. Sidebar, Overview, and Work Outline use the same latest-edition projection; original session timestamps determine the order of imported editions. A scheduled session exposes Run history and, when applicable, Latest run in its header.

The engine uses a profile-local SQLite ledger with transactional occurrence claims, indexed active-run lookup, durable input snapshots, schema migrations, bounded dispatch, and restart reconciliation. The `ScheduledTasks` RPC uses the existing device-addressed routing. The native page refreshes while visible rather than adding a new streamed RPC. Shared model-row normalization was extracted from the existing composer so the scheduler and composer offer compatible context-window choices.

Claude imports copy the referenced prompt and start paused with Ask permissions. The first activation requires a handoff acknowledgment and a verified paused source registry; a recorded handoff survives later source retirement. Imported historical sessions are tagged at import and by a one-time provenance backfill. Structured metadata uses `scheduledTaskInfo`, preserving the older `scheduledTask` name marker in registry writes without changing its type.

See [the operating guide](../../guide/scheduled-tasks.md) for use, host availability, migration, and recovery. The current native editor uses the existing Space/default agent account. Live connector/tool parity and the supervised overnight pilot require the chosen host and are not established by importing a prompt. Linked prompt-file auto-refresh, additional account controls, streamed updates, cloud execution, and automatic host transfer remain follow-up work.

## Recommendation

Add an engine-owned scheduler that launches normal Mast chats. Start with the Claude harness so existing procedures, skills, and tooling can be reused; keep the schedule model independent of the harness. Run scheduling inside the existing desktop or headless engine, with one explicitly chosen execution device per task. Import Claude schedules as paused drafts, validate the execution environment, then cut over one task at a time.

The first release works whenever the owning engine is running. An always-on headless host can provide overnight execution. A sleeping or powered-off host cannot run tasks; waking the computer, cloud scheduling, and automatic failover are later work.

## Evidence and scope

Code inspected on fetched upstream default `work-outline-implementation`, commit `3a6ec975`. Feature branch: `david/scheduled-tasks`; worktree: `.worktrees/scheduled-tasks`.

- `crates/engine/src/lib.rs`: both embedded and headless engine assembly and shutdown.
- `crates/engine/src/work_outline_launch.rs`: recoverable chat launch with stable operation, chat, message, and command IDs, captured execution bindings, and instruction revisions.
- `crates/engine/src/doc_host.rs`, `sessions.rs`, and `run_journal.rs`: queued turns, execution ownership, run lifecycle, and recovery.
- `crates/engine/src/execution_scope.rs`: execution-root and owning-device validation, including repository/worktree identities.
- `crates/engine/src/profile.rs`: profile storage and device boundaries; `instance_lock.rs`: one engine per data directory.
- `crates/sync/src/store.rs`: SQLite migrations, processed-command ledger, and durable publication patterns.
- `crates/engine/src/orchestrator_wakes.rs`: due waiting items generate notices. This does not execute recurring jobs and should keep its existing meaning.
- `crates/harness/src/claude/mod.rs`: user MCP configuration loads alongside Mast tools; `auto_approve` currently selects permission bypass for writable Claude runs. Claude Desktop connector parity is unverified.

The personal checkout already tags imported Claude runs with task names and groups them in the sidebar. Those fields/grouping are absent from this upstream base. Port that narrowly scoped support, or add equivalent compatible provenance, without merging unrelated personal changes.

### Actual migration inventory

The local Claude Code schedule registry contains 12 task definitions: 8 enabled recurring tasks and 4 disabled one-time tasks. The separate local-agent registry is empty. There are 13 prompt files; file presence alone is not evidence of an active schedule.

The following times are interpreted as America/New_York, consistent with current scheduled UTC timestamps and task descriptions. The registry has no explicit timezone field, so the import preview must ask the user to confirm it.

| Enabled task | Registry schedule in Eastern time | Migration concern |
| --- | --- | --- |
| Chat digest | Weekdays 03:30 | Chat connection and a document-tool procedure/output |
| Session digest | Weekdays 03:45 | Access to existing Claude session history; document tool |
| Project digest | Weekdays 04:00 | Issue tracker and document tool; procedure can perform writes |
| Ticket digest | Weekdays 04:30 | Issue tracker and document tool |
| Pull request digest | Weekdays 04:40 | GitHub CLI and document tool; prompt and registry times differ, registry wins |
| Environment cleanup | Weekdays 04:45 | Local wrappers, credentials, skills, agents; creates/deletes resources |
| Daily summary | Weekdays 05:00 | Multiple tools and upstream digests; existing procedure remains authoritative |
| Stall check | Every 30 minutes, weekdays 09:00–17:30 | Local wrappers; narrowly permitted resume operation |

Keep all four disabled one-time jobs disabled. A retired routine's prompt has no live definition and must not be recreated. Do not put user-specific account IDs, private Notion page IDs, or raw approval lists into upstream fixtures/docs.

## User experience

Interactive proposal: [Scheduled tasks UI mockup](mast-artifact:309606147c4c786cd10722ceb151e9f3). Explore the management list, Overview, schedule editor, and **Open latest → Run history** to switch editions. All task states, results, and actions in the mockup are examples; it does not execute tasks or modify schedules.

Add a Scheduled tasks view with task name, human-readable schedule/timezone, execution device, enabled state, next run, last outcome, and any missing setup. Actions: New, Import from Claude, Edit, Pause/Enable, Run now, and View runs. Editing previews the next five occurrences, including UTC offsets across daylight saving changes.

The editor chooses a Space, execution directory/binding, harness/account/model, prompt or prompt file, schedule, permission setting, and missed-run policy. Support five-field cron plus common daily/weekday/weekly presets, and a one-time timestamp. State explicitly that the host must be awake and its engine running.

Provide separate **Model**, **Thinking level**, and **Context window** selectors. Populate them from the chosen account/harness model catalog: thinking levels and context choices depend on the selected model, and a fixed context window is labelled Model default. Preserve supported choices when switching models; replace incompatible choices with an explicit supported default. Save these settings per task and capture them per occurrence, so changes apply to future editions while old editions retain their original configuration. Map thinking to the existing reasoning setting and context to the model's `contextWindow` option; do not invent a second capability catalog in the scheduler. The mockup uses a small illustrative subset of the repository's Claude catalog.

Each occurrence opens a fresh normal chat. History shows scheduled time separately from actual start, configuration revision, status, duration, and the chat link. Run now uses a separate identity and leaves the recurring next-run cursor unchanged. Pause prevents future starts and offers cancellation of queued occurrences; stopping an active chat is a separate explicit action. Deleting a task tombstones its definition while retaining history.

### Latest session and older editions

The **Output** selector controls whether an edition appears in default chat views. **Interactive** runs can appear in the sidebar and Overview and be designated as the personal assistant. **Background** runs require a work item and appear in Scheduled tasks history and the work item's hidden runs, including when they are the latest edition. The sidebar and Overview hide them even if their assignment changes.

- Represent each task whose latest unarchived run is Interactive once in the default sidebar and overview. Its session link opens the newest session, with that run's date and status. Do not render all past runs as separate default rows or cards. A task with no sessions still appears in task management with its schedule/setup state.
- Add a **Run history** control to the scheduled-task session header, labelled with the current edition's date/time. It lists that task's sessions newest first, including dates, outcomes, and which one is open. Selecting an edition opens its original transcript. Provide **Latest run** from an older edition, and the same history access from the task's overview entry.
- Opening or continuing an older session does not promote it to the latest edition or add it to the default sidebar/overview. Determine newest by the persisted session creation instant, with a stable ID tie-breaker, rather than last-message activity. An explicit Run now creates a new edition; a skipped occurrence with no chat does not replace the current latest session.
- When a new session is created, update the default task entry without switching away from an older edition the user is reading. Keep the header clear that an older edition is open and offer navigation to the new latest run.
- Apply the same presentation to native runs and imported Claude task sessions, keyed by stable task identity and source account/Space boundaries. Never combine unrelated tasks just because their display names match. Legacy name-only imports need a source-scoped identity mapping before joining a task's history.
- Hiding older editions is a presentation rule: retain their transcripts, links, search access, and unresolved Required Actions. Load history in pages rather than downloading every transcript. Show skipped/no-session occurrences in operational history without presenting them as session editions.

The task manager offers **Clean up old runs** on the selected host. The
`ScheduledTasks` RPC's `cleanup` action accepts `mode: preview` and returns
groups with task and Space identity, counts, and chat IDs. For `mode: apply`,
the caller supplies those IDs as `expectedChatIds`. The host recomputes
eligibility and returns a fresh preview if the set changed before confirmation.
Repeated apply calls write nothing once those runs are archived.

Cleanup shares the default projection's device, Space, task key and ordering.
A retired native task still has a newest edition for supersession purposes.
Desktop runs also qualify after a recorded native handoff, deletion of their
Mast task, or retirement of their Desktop definition. Cleanup checks open
Required Actions, in-flight and queued work, sidebar pins, and personal
assistant coverage before selecting chats. The background auto-adopt sweep
uses the same Desktop source rule and creates qualifying chats already
archived. Archived transcripts remain reachable through history. The additive
cleanup reply is optional, and the UI checks `scheduled-run-cleanup-v1`
before offering cleanup against the selected host.

Surface failures, missing credentials, and approval requests through existing notifications/Required Actions where available, with one deduplicated actionable notice per problem. A task waiting for input remains active and blocks overlapping occurrences; it is never reported as successful.

## Architecture and storage

Use a dedicated `scheduled_tasks` engine service with a pure schedule evaluator, transactional store, dispatcher, lifecycle reconciler, and RPC/watch facade. Wire startup after launch/recovery services are ready and stop scheduling before sessions shut down. Use a cancellation token and injected clock. Sleep until the next deadline, interrupted by edits and wake/resume signals; periodically recheck wall time so clock changes cannot leave a timer stranded. Batch bounded history writes off the UI/runtime hot path.

Persist task definitions and the occurrence ledger in a profile-scoped SQLite database, proposed `scheduled-tasks.sqlite3` under `EngineProfile::store_root()`. Use explicit migrations and transactions. Keep this separate from CRDT workspace documents: occurrence claims require a uniqueness constraint, and schedule authority should be clear. V1 task definitions/history are authoritative on their owning engine; remote clients use existing authenticated device routing for reads and writes. Do not imply that the registry synchronizes or elects a scheduler.

| Record | Essential fields |
| --- | --- |
| Task | ID, revision, name, Space ID, owner device, enabled/tombstone, cron+IANA timezone or one-time UTC timestamp, prompt source, harness/account/model configuration, execution binding, permission policy, overlap/misfire settings, timestamps, import provenance |
| Cursor | Task ID, last evaluated UTC instant, activation boundary, next due instant; updated transactionally with occurrence decisions |
| Occurrence | ID, task ID, scheduled UTC instant, scheduled/manual trigger, immutable config and prompt snapshot/hash, deterministic launch IDs, state/reason, actual start/end, chat ID, retry lineage |
| Import | Source account/registry identity + source task ID, source fingerprint, imported task ID, source enabled state, cutover boundary, validation results |

Scheduled occurrence uniqueness is `(task_id, scheduled_for_utc)`; changing a task revision must not mint a second occurrence for the same instant. Manual runs use request idempotency keys. Capture the configuration and prompt before launch so retries cannot conflict with later edits. Editing affects unclaimed future occurrences; queued snapshots remain fixed unless explicitly cancelled. Enabling starts from now, with no implicit historical replay. Moving hosts requires pausing/draining the source and explicitly activating the destination; no automatic ownership transfer in V1.

Do not store credentials in task records. Store account references and resolve credentials on the owning device. Follow profile isolation on account switch, logout, and backup/restore; restored definitions remain paused until ownership is validated.

### Launch integration

Extract the reusable recoverable launch sequence from `WorkItemLaunchService` into a shared service rather than shelling out to a CLI or simulating UI input. Keep the existing work-item API as a wrapper with its current validation and instruction semantics. Add a scheduled-task caller supplying persisted configuration, validated execution binding, explicit task prompt, and deterministic operation IDs. Background tasks require a Work Outline item and use the existing assignment and inherited instruction path. Interactive tasks can also use this path by choosing a work item.

The task draft and the immutable occurrence snapshot store `output`. A missing field defaults to Interactive. Scheduled chat metadata is written when the chat is created and carries the captured mode so the shared projection can hide background runs independently of assignment. The editor sends Background only to owners advertising `scheduled-task-output-v1`. Older owners ignore the field and continue to run Interactive tasks. When an older editor omits the field on a save, a current owner preserves the stored mode.

Launch order: persist occurrence snapshot → validate owner/config/tools → idempotently create chat and record task/run provenance → apply captured instructions → durably queue the first message → reconcile actual run lifecycle. Derive IDs with existing helpers so operation IDs meet the current 64-character limit. Retrying a launch uses identical inputs and IDs. Preserve account pinning, queue validation, approval handling, cancellation, and publication through existing services.

Add stable `scheduledTaskId` and `scheduledTaskRunId` chat metadata, retaining the task-name marker as a display/legacy compatibility field. Renaming a task must not orphan history. Existing imported Claude transcripts remain external historical runs and never cause native execution.

### Scheduling semantics

- Store IANA timezones and calculate UTC occurrences explicitly. Use wall-clock cron semantics; document day-of-month/day-of-week OR behavior and Sunday aliases. Reject unsupported cron syntax with a clear error. Evaluate an appropriate parser/timezone dependency during implementation and pin the chosen semantics in tests.
- Spring daylight saving gap: skip nonexistent local times. Fall repeated hour: run once at the first matching local instant. Use the same deduplication rule after restart, so the second repeated instant cannot create another run.
- Default misfire policy: coalesce to the latest missed occurrence within a configurable grace window, proposed six hours for daily digests and one interval for the stall check. Skip older occurrences with an aggregate history reason. Expired one-time tasks remain expired. Never replay an unbounded backlog on restart.
- Default overlap policy: one active occurrence per task, including queued launch and waiting-for-approval/input. Record newer occurrences as skipped while active; do not build a growing queue. Manual runs obey this too. Use a configurable global scheduled-run limit, initially two, and a bounded oldest-due-first queue. Recheck grace and paused state before dispatch from that queue.
- Existing time ordering is not a dependency guarantee. Preserve schedules during import and show freshness issues reported by existing procedures. Explicit task dependencies and conditional workflows can follow later; do not silently invent them during migration.
- Recheck pause/delete/revision/ownership before dispatch, serializing those changes with claims. Scheduled jobs do not bypass existing execution checks or auto-continue settings.

### Failure and recovery contract

Persist intent before any launch side effect. Model occurrence states as `pending → launching → queued → running → succeeded/failed/cancelled`, with `blocked`, `interrupted/unknown`, and `skipped` outcomes/reasons. Distinguish a queued command, a finished agent turn, and a completed task; input requests and unresolved background work must remain visible.

On restart, reconcile occurrence IDs against chats, queued commands, and run journals. Repair missing local launch steps with the same IDs. After an ambiguous post-start failure, expose interruption and use existing same-run recovery only where its contract permits; never automatically create a fresh attempt that might repeat external writes. A user-requested rerun creates a linked new attempt and names the duplication risk. Infrastructure retries before execution can use bounded backoff with the original IDs; missing authentication or policy blocks require remediation.

The promise is one logical occurrence and idempotent dispatch, not exactly-once external side effects. Validate crash boundaries instead of relying on a claimed row alone. If persistence is corrupt or unavailable, stop scheduling and surface the error; never treat it as a new empty ledger and redispatch old jobs.

### Background run archive and retention

The run snapshot captures `afterSuccess`. Its default is Archive, which applies
only to Background runs. The editor uses `scheduled-task-after-success-v1` to
offer Archive and Keep and to send the field. An older host drops the field. A
current host preserves the stored choice when an older editor omits it.

Lifecycle reconciliation persists the run outcome and its follow-ups together.
It applies archive guards for open Required Actions, pending input, queued
turns, pins and the personal assistant. The registry records a completed
automatic archive in the same write as the archive flag so a crash replay
cannot undo a user's restore. Background failures, interruptions, cancellations
and input waits update a task-scoped Required Action. Success resolves it.
A user pause or deletion before a run chat exists records the cancellation
without filing an action.

### Background results contract

A Background run on a work item records `results` on its `ScheduledRun` when
the occurrence is claimed: `{artifactId, title, role, revision?}`. `title` is
`Results: <task name>` when the artifact is first created and `role` is
`scheduled-results:<hash of task id>` (`ScheduledRunResults::{title_for,
role_for}`). The artifact id is the usual `artifact_id(space, item, title,
role)`, and the title is part of it, so identity is fixed by the first publish:
`results::resolve` looks up the existing artifact by item and role and reuses
its title. The claim records a provisional identity from the current task name,
`launch` replaces it with the resolved one, and the publish guard resolves the
same way. A rename therefore keeps one artifact and its revision history.
There is one artifact per task and item.

It is not pinned. Pinning is plan-scoped (one plan per item), so a pinned
results artifact would replace the item's plan. It stays visible without a pin:
the item's artifact list sorts the pinned plan first and then by last update,
and every run updates this artifact. Consumers (the read path) find it by
`role`, never by title. Runs claimed before this contract, and Interactive
runs, have no `results` and are never checked.

`prompt_with_contract` appends the contract to the prompt on launch. It depends
only on the run, so a launch retried after a crash queues the same message. The
contract tells the run to publish once with `idempotencyKey`
`scheduled-run-<run id>` and to start the document with `Run: <run id>`, to raise
Required Actions with `dedupeKey` `ScheduledRunResults::dedupe_prefix(task)` plus
a stable topic (the prefix is per task, so tasks sharing an item never collide,
and it never matches the system's `scheduled-run-failed:<task>` key), and to
set item status.

`artifact_publish` from a Background run chat is guarded: a title or role that
matches only half of the identity, another task's role, `pinAsPlan`, or a second
publish from the same chat with a different key is rejected. A retry with the
same key replays. Other artifacts from the run are unrestricted. The check
and the record run under one lock, so two concurrent publishes from a run
cannot both pass it.

When a run is first seen `Completed`, the scheduler requires a revision whose
author is an agent in the run's own chat. A revision from a user, another task or
an earlier run does not count, and an unchanged document records no revision at
all, which is why the contract has each run write its id. The found revision is
stored in `results.revision`. Otherwise the run becomes `Failed` with a reason in
`detail`, which feeds the existing failure Required Action and skips archiving.
Provenance of any revision is `ArtifactRevision.by.chatId`, which equals
`ScheduledRun.chatId`.

The proposed retention policy keeps run history and defaults to 14 days. It
protects the latest run, restored chats, chats with open Required Actions and
chats outside the scheduled-run ledger. Automatic deletion and its setting are
pending a transcript purge path. The existing Delete Chat operation removes
the registry row and the local snapshot while the synced session document
survives. The scheduler currently performs no deletions.

## Claude import and cutover

1. Discover the selected Claude account's schedule registries and parse a version-tolerant read-only snapshot. Import cron/fireAt, enabled state, prompt path, cwd/worktree choice, model, last scheduled boundary, and permission metadata. Ignore unrelated fields while reporting unsupported schedule variants. Preview differences against prompt descriptions.
2. Deduplicate by source account/registry and task ID. Store imports as paused drafts. Re-import previews updates and respects user edits; never enables tasks automatically. Keep orphan/retired files and disabled one-time definitions out of the default enabled selection.
3. Resolve symlinks and read prompt sources as data. Default to a managed copy with provenance and a diff-based refresh action; offer an explicitly linked file for users who want ongoing file edits. Snapshot content per occurrence. Preserve pointers to Notion procedures and load the authoritative procedure at run time; do not copy a stale procedure into the schedule.
4. Preflight the selected execution account: cwd and repository binding, actual available model, CLI/wrappers and environment, required skill/agent definitions, MCP connections, authentication, and session-history access. V1 keeps Claude execution for these imports. A task is not ready merely because its prompt file exists. Tool inventories cannot prove every branch of a procedure; report verified and unverified capabilities separately.
5. Present permission differences explicitly. Claude `auto`, `acceptEdits`, and per-tool `approvedPermissions` are not equivalent to Mast's boolean auto-approve. Never translate scoped approvals into permission bypass. Use Ask by default, or require an explicit task-specific choice of Mast's existing Auto-approve behavior. Exact scoped approval compatibility, if required for these tasks, is a separately tested extension to the existing permission control channel, not a scheduler-specific bypass. Until resolved, mark the task as requiring setup.
6. Validate with fixtures and supervised runs that obey real task permissions. Do not label a write-capable run a dry run unless writes are actually suppressed by the chosen execution mechanism. Statically importing a prompt never authorizes executing it.
7. For each task, have the user pause its Claude schedule through the supported Claude surface, verify it is paused and no source run is in flight, then activate Mast with a cutover timestamp. Ignore source occurrences before that boundary. If source state cannot be verified, keep Mast paused and show the remaining step. Avoid editing Claude's private JSON while it is running.
8. Migrate the digests before the daily summary and environment automation, verify results and tool parity, then retire the original schedules. Rollback pauses Mast, drains/cancels its pending work, confirms no active occurrence remains, and re-enables Claude from a new boundary.

## Implementation sequence and acceptance

1. **Contracts and evaluator.** Add proposed types in `crates/proto/src/scheduled_tasks.rs`; implement pure cron/timezone/one-time evaluation. Define wire compatibility and lifecycle reasons. Acceptance: all eight observed cron expressions have correct next occurrences; invalid syntax, DST gaps/repeats, clock rollback, and expired one-time schedules have deterministic results.
2. **Durable scheduler.** Add `crates/engine/src/scheduled_tasks/{mod,store,evaluator}.rs`; wire profile storage, startup, wake handling, and shutdown. Acceptance: duplicate ticks and restart cannot create duplicate occurrences; edits, pause, coalescing, queue limits, corrupt storage, profile switches, and device ownership work with fake time.
3. **Launch and lifecycle integration.** Extract the shared launch helper; add dispatcher/reconciler, task provenance, and bounded concurrency. Acceptance: failure injection after each durable launch step repairs the same chat/message/command; ambiguous started runs are not relaunched; normal Work Outline launches retain their existing guarantees; approval/input/cancel outcomes are accurate.
4. **RPC and interface.** Register list/watch, create/update, enable/pause, run-now, delete, paginated run-history, and import-preview/apply methods using existing RPC authentication/device routing. Mutations use idempotency keys and expected revision checks. Return a latest-session summary per task for sidebar/overview rendering, using the same ordering rule as history. Add Scheduled tasks UI and notifications. Acceptance: a user can create a task, preview times, run it, inspect its chat, pause it remotely on the owner, and resolve a blocked setup state. Both sidebar and overview show the latest session per task when it is Interactive; the session header and overview history action open older editions. Reading or continuing an older session does not reorder editions, and a new run does not interrupt that reading. Verify manual runs, skipped occurrences, task renames, same-name tasks, imported history, pagination, and restart preserve these rules. Unsupported clients read additive metadata safely.
5. **Migration and permission readiness.** Implement the read-only importer and preview, with redacted fixtures matching both observed registry locations. Acceptance: 12 source definitions produce eight recurring candidates and four disabled one-time entries; the retired orphan prompt stays excluded; repeat import is idempotent; timezone/model/tool/permission mismatches remain visible; enabling requires a verified cutover boundary.
6. **Pilot and operating guide.** Document host availability, headless deployment, permissions, missed runs, recovery, and rollback. Run a synthetic overnight/restart pilot before supervised real runs. Acceptance: one logical run per due time, correct Eastern timestamps, bounded catch-up, usable notices for unavailable tools, and no duplicate Claude/Mast dispatch across the cutover boundary.

Validation uses isolated temporary homes/profiles, injected tick times, mock harnesses, and sanitized registries; no production tasks are executed by the tests. The native application build, focused protocol/scheduler/UI suites, and existing external-import, Work Outline launch, and profile-isolation regressions passed. New Rust modules pass formatting checks, and the complete diff passes whitespace checks.

## Deferred choices

Initial defaults above make implementation possible without more design decisions. Confirm timezone, execution host, permission mapping, and task-specific grace windows in migration preview. Revisit synced task definitions with fenced ownership, cloud/host failover, machine wake, explicit dependencies, exact scoped permission parity, and richer semantic completion reporting after the first end-to-end pilot. Do not promise unattended replacement of all eight tasks until their connectors, procedures, and permission behavior have been exercised on the chosen host.
