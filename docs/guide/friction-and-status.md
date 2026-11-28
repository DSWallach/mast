# Friction and status

## Review friction

Open the Space root's details and find **Friction**. Agents can report obstacles, and Mast can capture events such as refused tool calls, approval prompts and turn errors when auto-capture is enabled. Filter and expand reports, then use **Create bug**, **Propose instruction**, or **Dismiss**. You can export Markdown or import an existing friction log. Reports with the same fingerprint collect occurrences. **Approvals** limits the list to approval prompts. In a local Space, it also shows **Every request**, which records ordinary approval outcomes even when auto-capture is off. Search filters the lists, and **Create rule…** on an aggregated Codex command drafts an approval rule for you to review and save (see Approval history in the Work Outline guide).

## Use the menu bar

Enable the menu bar item in settings. Its lamp reflects chat status and its menu shows account usage and reset times, chats needing you, and interrupted chats. Select a chat to open it; use **Resume all** for interrupted work. When a newer build is installed, the menu also offers **Restart when idle**. It includes **Open Mast** and **Quit**.

With **Needs you** enabled for the menu bar, **Dismiss all** beneath **Requests** dismisses the Required Actions listed in that menu. It uses the same dismissal as each request's card and opens Needs you so any delivery errors are visible. Requests arriving afterward stay open; waiting or failed chat notifications are unchanged.

## Watch the build queue

When Cargo jobs go through Mast's build queue, the running-chats popover (the lamp in the titlebar, or Option-Command-R) gains a **Builds** block above the chat list. It stays out of the way until a job exists. The block lists every job still waiting or running, then the five most recent finished ones.

Each row shows:

- A status: `needs approval`, `queued #2` (its place on the host's queue), `building`, `passed`, `failed · exit 101`, `cancelled`, `timed out` or `lost`.
- The job's label, or its command if it has none, and how long it has waited, run or taken.
- The chat that owns the job. Select it to open that chat.
- **Log**, which shows the last 200 lines of the job's log under the row (select it again to hide it), and **Cancel** while the job has not finished. A cancelled queued job ends at once; a running one is asked to stop.
- For a failed job, the first line of its error report; for a job that is not moving, the reason (for example `host unreachable; retrying`).

The block's header counts running and waiting jobs and says **quiet window** while one holds the host. In the Overview tree and in the sidebar's session list, a chat waiting on the queue shows `queued #2` or `building` in place of its **Idle**, **Done**, time or **Working** label; a compact single-chat row in the Overview, which has no label, adds one. This includes a finished turn you haven't read yet. Chats that need input, failed or have an undelivered message keep their usual label. A chat that has finished its turn but still has a queued or running build shows in the popover as **idle · 1 background running**, read or not, and moves to **finished** (if still unread) when the build ends.

If **Cancel** doesn't go through, the reason appears under the job.

The queue is local to the device that runs the builds, so the block lists that device's jobs for the Spaces you are watching. An engine that does not have the queue yet shows no Builds block.
