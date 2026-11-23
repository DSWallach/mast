# Scheduled tasks

Open **Scheduled tasks** using the clock button beside Overview in the sidebar.
Choose the execution device at the top of the page. That device must be awake
and running Mast's desktop or headless engine when a task is due.

## Create a task

Choose **New task**, enter its name, instructions, and an existing absolute
working directory on the execution device. A new task starts paused. A selected
Space supplies the initial directory and account behavior; otherwise the task
creates project-less sessions on the chosen device.

Choose an **Agent** from the same enabled, installed agents offered in your
sessions, then choose its **Model**, **Thinking level**, and **Context window**.
Available thinking and context choices depend on the model.
Changing models preserves compatible choices and clears unsupported ones.
Choose an **Output** mode.

- **Background** is the default for new tasks. Choose a **Work item** in the
  selected Space. Open the transcript from **Run history** in Scheduled tasks
  or from the work item's hidden runs. Background runs stay out of the sidebar
  and Overview, and cannot act as the personal assistant.
- **Interactive** shows the latest run in the sidebar and Overview. You can
  optionally attach runs to a work item. Existing tasks start in this mode.

For an Interactive task attached to a work item, **Each run** can continue the
same standing chat. Its first run starts the chat, or adopts an eligible
personal assistant already assigned to that item. Later runs use that chat,
including after Mast restarts. **Start fresh** can rotate it weekly, when its
context is heavy, or never. Rotation keeps the previous conversation readable
and moves the personal-assistant designation to the successor.

Existing tasks continue to start a new chat until you change this setting. An
older engine retains the saved settings but uses its separate-chat behavior
while downgraded; the settings take effect again after upgrading.

Attached runs receive the work item's inherited instructions. Changing the Space
clears the work item. A run whose agent cannot receive the item's instructions
fails before it creates a session. Older execution devices offer Interactive
output. Update the execution device to use Background.
Each run captures its configuration, so editing a task changes future editions.
You can change the model or switch agents, including Claude Code to Codex, after
a task has run. Past editions retain their original agent and model.

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

**Permissions: Ask** uses Mast's normal permission prompts. **Permissions:
Rules**, the default for new and imported tasks, runs what your
[permission rules](permissions.md) allow without asking and asks about the rest.
**Auto-approve** uses the existing agent approval mode for that task. A run
waiting for approval or another answer stays active and prevents overlapping
editions of the task.

Every scheduled run gets `MAST_SCHEDULED_TASK=<task name>` in its environment,
so your own hooks can tell an unattended run from an interactive one.

**Write to Notion** says whether the task's procedure may write to Notion. New
and imported tasks start with it off. When it is off, the run's prompt ends with
an instruction to skip every step that creates, updates or deletes Notion
content, and the run's environment has `MAST_SCHEDULED_WRITE_TO_NOTION=0`. When
it is on, the value is `1`. Mast does not block Notion tools itself, so a
procedure must follow the instruction. Tasks saved before the setting existed
show it on and send neither the instruction nor the variable until you save them
with the setting chosen. The control needs an updated execution device.

Enable scheduled runs and choose **Save changes**. **Run now** starts an edition
without changing the recurring next-run time. **Pause** stops future
dispatches; it does not stop an agent already running. Stop that agent from its
session. **Delete** removes the schedule while retaining its run history.

For Background tasks, **After a successful run** defaults to **Archive**.
Mast archives the chat after the run completes and any open Required Action,
input request or queued turn clears. Pinned chats and the personal assistant
stay open. Choose **Keep** to leave successful runs unarchived. The control
requires an updated execution device.

A Background run that fails, stops early or waits for input leaves its chat
unarchived. Look for the task's Required Action on its work item. It links to
the run chat when one exists and updates as new runs need attention. A successful
run resolves the action.

Each Background run must also leave its results on the work item. Mast appends
a short contract to the task's prompt that asks the run to publish one markdown
artifact titled `Results: <task name>`, raise a Required Action for anything that
needs you, and set the item's status. When the run ends, Mast checks that this
run published a new revision. A run that did not is marked **Failed** with the
reason, stays unarchived and raises the same task Required Action as any other
failure. Interactive runs get no contract and are not checked.

## Read the latest results from any chat

A chat on any work item can read the newest results of your scheduled tasks
with the `briefing_read` tool, for one task by name or for all of them, with the
time each run published. It does not need `read_chat` or Notion. Ask the agent
for "the latest Slack briefing" or "what's in my briefings". A renamed task is
found under its new name. Only a Background run of the task can publish its
results, so another chat cannot stand in for one.

Scheduled tasks also has **Briefing index in new chats**. Turn it on and each
new chat's agent starts with one line such as `Slack 05:31 · GitHub 04:52 · 3
need you`: when each task last published, in the time zone of the device
hosting the chat, and how many Required Actions from scheduled tasks are open
in the Space, counting failed runs. The line goes to the agent with the chat's
first message only. It does not appear in the transcript, and it is not added
to later messages, to a first message that starts with a slash command, to
Background runs or to side chats. It is off by default and applies to chats
whose first message is sent after you change it. The switch is hidden for a
device whose Mast is too old to support it.

## Read editions

The sidebar and Overview show the latest unarchived run per task on each device
when it is Interactive. Background and older runs stay attached to their work
item and appear through its hidden-run control.
In Overview list mode, each visible task has a row with its name, schedule, next run,
and last outcome. Click the row to open the latest run or
choose **History** to see past runs. Reading or replying to an older edition
does not promote it. The task manager lists
configured schedules. Choose **Edit** to see a task's settings, with its own
**Run history** at the bottom of that page. A scheduled session's **Run history**
button opens the same task page. History shows recorded run outcomes and marks
archived chats. Choose an edition there to open its transcript.
An older session also offers **Latest run**. A new run does not interrupt the
edition you are reading.

## Reply to a finished run

Once a scheduled run has finished, a reply in its chat opens a new
conversation chat instead of continuing the run. The new chat starts in the
run's Space and directory with the run's model settings. Its first turn gets
the run's final message and a pointer back to the run chat, not the run's full
context, so a long run does not feed every follow-up question. The new chat is
an ordinary chat: it is not stamped as the scheduled task, does not count as a
run, and shows in the sidebar like any conversation. The run chat lists its
follow-ups above the composer so you can jump to them.

The composer on a finished run says where a reply goes. Choose **Continue in
this chat** to keep replying in the run itself, the behavior before this
change; the choice belongs to that run and **Open new chats instead** switches
back. A run that is still working or waiting for an answer always keeps your
reply in its own chat, and so does a run whose execution device is too old to
open follow-up chats. A failed run also keeps replies in place, so you can ask
it to retry. If a send is retried after a lost reply, it reaches the follow-up
chat it already opened rather than creating a second one.

Imported Claude editions use their original transcript timestamps when choosing
the latest edition. Task identity includes source provenance; identical display
names do not merge unrelated tasks. Imported runs join their native task once
the matching source is imported: the run's task name and prompt file must both
match that source, so a same-named task from another Claude account stays apart.
A personal assistant chosen on an imported run follows its task after the join.
Deleted tasks and source tasks never imported into Mast appear under **Retired
tasks** in Scheduled tasks. Their runs stay readable in history and are hidden
from the sidebar, Overview, and Work Outline.

## Clean up old runs

On **Scheduled tasks**, select the run device and choose **Clean up old runs**.
The preview shows how many chats will be archived for each task. Choose
**Archive runs** to confirm. If eligibility changes while you review the
preview, Mast shows the updated counts for you to confirm again.

Cleanup archives superseded runs and Desktop runs from schedules that have
moved to Mast or been retired. Runs with an open Required Action, running or
queued work, or pending input stay available, as do pinned chats and personal
assistant runs. Archived runs remain in **Run history** and can be restored
from the archive.

The background import sweep archives newly discovered Desktop runs from
handed-off or retired schedules as it imports them. When Mast cannot verify
retirement from the source registries, it leaves that run's import visible.
Hosts that predate this feature do not offer the cleanup action.

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
permission bypass. To reuse Claude's allow list, copy its `permissions` block into
`~/.mast/settings.json`. Existing connector credentials and local command tools must
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
