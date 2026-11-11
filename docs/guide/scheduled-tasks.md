# Scheduled tasks

Open **Scheduled tasks** using the clock button beside Overview in the sidebar.
Choose the execution device at the top of the page. That device must be awake
and running Mast's desktop or headless engine when a task is due.

## Create a task

Choose **New task**, enter its name, instructions, and an existing absolute
working directory on the execution device. A new task starts paused. A selected
Space supplies the initial directory and account behavior; otherwise the task
creates project-less sessions on the chosen device.

Choose a **Model**, **Thinking level**, and **Context window** from that host's
model catalog. Available thinking and context choices depend on the model.
Changing models preserves compatible choices and clears unsupported ones.
Each run captures its configuration, so editing a task changes future editions.

Use a five-field cron expression and an IANA timezone for recurring work:

| Schedule | Cron |
| --- | --- |
| Weekdays at 08:00 | `0 8 * * 1-5` |
| Every day at 08:00 | `0 8 * * *` |
| Mondays at 08:00 | `0 8 * * 1` |
| Every half hour, weekdays 09:00–17:30 | `*/30 9-17 * * 1-5` |

Alternatively enter a one-time timestamp with its UTC offset, such as
`2026-12-01T08:00:00-05:00`. A one-time timestamp replaces the cron expression.
Use **Preview next five runs** to check the actual dates and offsets.

**Permissions: Ask** uses Mast's normal permission prompts. **Auto-approve**
uses the existing agent approval mode for that task. A run waiting for approval
or another answer stays active and prevents overlapping editions of the task.

Enable scheduled runs and choose **Save changes**. **Run now** creates a separate
edition without changing the recurring next-run time. **Pause** stops future
dispatches; it does not stop an agent already running. Stop that agent from its
session. **Delete** removes the schedule while retaining its run history.

## Read editions

The sidebar and Overview show the latest unarchived session per task. Reading
or replying to an older edition does not promote it. Use **Run history** from
a scheduled session's header or its task-management row to open older editions.
An older session also offers **Latest run**. A new run does not interrupt the
edition you are reading.

Imported Claude editions use their original transcript timestamps when choosing
the latest edition. Task identity includes source provenance; identical display
names do not merge unrelated tasks.

## Import Claude schedules

On macOS, **Import from Claude…** previews the schedule registries under Claude's
application-support folder. It reads prompt files referenced by those registries;
it does not treat every file in `~/.claude/scheduled-tasks/` as a live task.

Select **Import paused** for the tasks to move. Disabled one-time jobs stay
paused, and retired prompt files are excluded. Review each imported task's
timezone, model, directory, connections, and instructions. Import copies the
prompt, including any pointer to an authoritative external procedure. Later
prompt-file edits are not automatically imported.

Before enabling an imported task, pause it in Claude and confirm no source run
is still active. Acknowledge that handoff in Mast's editor. Mast checks the
source registry's paused state on the first activation and records the handoff.
Afterward, retiring the source definition does not disable the Mast schedule.
Mast never changes Claude's registry or transfers scoped tool approvals into
permission bypass. Existing connector credentials and local command tools must
be available to the execution account; import is not a guarantee of tool parity.

## Missed runs and recovery

Mast coalesces missed occurrences to the latest one inside the configured grace
window. Older work is skipped. A zero-minute grace skips missed runs, with a
small allowance for the scheduler's polling interval. Nonexistent daylight-saving
times are skipped; a repeated local time runs only on its first occurrence.

At most two scheduled agents dispatch at once, and each task has at most one
active edition. Additional due occurrences of an active task are recorded as
skipped. Queued launches expire when their grace window passes.

Task definitions, immutable run snapshots, and occurrence IDs are stored under
the current engine profile. Startup recovers unfinished launch steps using the
same chat and command IDs. A run that started but has an ambiguous interrupted
outcome is not automatically duplicated. Inspect its transcript before choosing
**Run now** again, since external writes cannot be guaranteed exactly once.

The initial version has explicit device ownership. It does not wake sleeping
computers, run in a cloud scheduler, automatically transfer tasks between hosts,
or enforce dependencies between different schedules.
