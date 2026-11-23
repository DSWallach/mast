# Work Outline

The Work Outline organizes a Space's Chats under nested **work items**. A work
item can stand for a workstream, a ticket, a pull request, or an investigation.
It holds the instructions its Chats inherit, the folders and repositories they
run in, and the questions agents have asked you. Chats stay the main working
surface. Overview is the persistent left column, with the Work Outline as its
default presentation. Choose Sessions for the session list and its grouping
controls. New Chats automatically get a work item unless they already belong
under one. Children and side chats reuse their source's item. Chats created
without a folder live in the host device's Personal Space and keep their own
scratch directory. Older unassigned Chats gain an item when you hand them off
or next run them.

| Concept | What it is |
| --- | --- |
| Work item | A title with an optional description, a status (Pending, Active, Completed, or the derived User Action Required), child items, associated Chats, and external references. |
| Instructions | Plain-text directives attached to the Space root or to a work item. A Chat receives the Space-root instructions, then its ancestors' instructions, then its own item's instructions, as runtime context outside the transcript. |
| Execution setup | The execution root, primary repository, member repositories, and optional pinned worktrees that a work item's Chats run in. Items inherit the setup of their nearest ancestor. |
| Required actions | Durable requests from agents (questions, approvals, decisions) that keep an item in User Action Required until they are resolved. |
| Agent tools | Work Outline tools that agents in a Space Chat can call to read their item, create children, set status, ask you for something, or propose an instruction. |

## Glossary

A **Space** is a folder you work in and the root of its outline. A **project** is an
outline item marked as a project, with its own worker destination. A **Blueprint**
saves an item tree for a one-time copy; a **Child template** sets live rules on a
parent that apply to its direct children.

## Command approval rules

Space-root details include **Approval rules**. Suggestions start off; enabling
or creating a rule is a user action. Matching Codex command requests are answered
and recorded in friction; requests that cannot be matched safely still ask.
The command test uses Codex and the Space root as its working directory.
Parsing rejects shell expansion, redirection and unrecognized command families,
with a few exceptions for the commands agents run. A segment may end with
`> /tmp/<name> 2>&1`, a log file directly in `/tmp` that is missing or a regular
file you own. This check is best effort, since the shell opens the file later.
`scripts/ui-preview.sh` may take a fixed set of `MAST_PREVIEW_*` options with plain values,
`cut -c<range>` may follow a pipe, and operators inside quotes are literal.
User permission rules in `~/.mast/settings.json` still reject all of these.

A rule may also list exact environment assignments (`NAME=value`, one per
line, entered only by you in Space settings). A command that sets assignments
matches only when its set equals the rule's list, in any order. Rules without a
list accept just the isolation variables and the preview options the parser
already knows.

A rule may pin a **repository executable**, a relative path such as
`scripts/run.sh` inside a checkout. The Space's host device fingerprints the
file when you save the rule, and a proposal from an agent shows the same
digest, size and first lines for you to review. Saving again in the editor
fingerprints the current contents. If the file's contents change, the rule
stops matching until you save it again. Files over 1 MiB are never pinned. A
device that does not host the Space cannot fingerprint, so it rejects a save
that changes the pinned contents and names the host. Turning a rule on or off
works from any device. The command regex must be anchored with `^` and `$` and
may not use `|` outside a group, and the whole command must match it.
Scoped rules apply in the Space root and in a linked worktree only when the
chat selected that checkout and the rule's working directory is that checkout.

Two read-only commands match without a rule: `df -Pk .` and
`git worktree list`, used for the slot checks agents run before a build.

Automatic rule evaluation currently applies only to Codex. The Claude Code
adapter does not currently surface an equivalent command approval request to
this evaluator, so selecting Claude Code in a rule does not make it active for
that harness. Claude Code's existing permission handling is unchanged.

## Quick start

1. Choose **Overview** in the left column, or press `⇧⌘L` (`Ctrl+Shift+L`) to
   reveal and focus the Outline. **Sessions** retains the session list's
   grouping, sorting and metadata controls.
2. Click **New item** for the full creation dialog. Choose a Space and parent
   work item, enter the title and optional description, then set status and
   placement and click **Create item**. For quick inline creation, use `⇧⌘I`
   (`Ctrl+Shift+I`) or `N`, type a title and press Enter. In a Space with no work items, the Outline offers
   **Create work item** in place of the empty tree.
3. Select the item and press Enter to open **Work Item Details**. Under
   **Instructions**, add an instruction and click **Save**.
4. Click **Start Chat** on the selected row or in Details, or press `⌘↩`. The Chat launches with
   the item's effective instructions and execution setup, and it is associated
   automatically.
5. To bring existing Chats under the item, drag their rows onto it or run
   **Associate Chat with Work Item**.

Moving the selection with the arrow keys leaves the workspace and its draft
alone. Click a Chat row or press Enter to open it in the main workspace. Work
Item Details, Required Actions and Inbox use that workspace too; closing them
returns to the selected Chat when no other Overview panel remains open.

Press `⌘;` (`Ctrl+;`) to jump between the Chat composer and its Outline row.
The row is revealed by expanding its ancestors and scrolling it into view.
On narrow windows Overview floats over the workspace, and opening a destination
or clicking outside dismisses it. The saved wide-window width is preserved.

Press `?` while the Outline has focus, or click the keyboard button in the
Outline toolbar, to open the shortcut reference (its **Linear** group covers the
Linear panel's issue rows and project-linked items). Hover over any Outline control
to see what it does and its current key. Right-click any row for everything you
can do with it.

## Keys

Outline keys work only while the Outline tree has focus. They never fire in the
inline editor, a picker, the search field, or any other text input.

| Action | macOS | Windows / Linux |
| --- | --- | --- |
| Previous / next row (display order) | `↑` / `↓` | `↑` / `↓` |
| First / last row | `Home` / `End` | `Home` / `End` |
| Collapse / expand | `←` / `→` | `←` / `→` |
| Previous / next sibling (skips subtrees) | `⌥↑` / `⌥↓` | `Alt+↑` / `Alt+↓` |
| Open the selected item or Chat | `↩` | `Enter` |
| Open Work Item Details on an item row | `⌘I` | `Ctrl+I` |
| Start a Chat in the selected work item (on a Chat row: in the Chat's work item; on a PR review item: review its PR, see [PR review sync](#pr-review-sync)) | `⌘↩` | `Ctrl+Enter` |
| Rename (a work item in place; a Chat row opens the rename dialog) | `F2` or `R` | `F2` or `R` |
| New sibling / new child | `N` / `⇧N` | `N` / `Shift+N` |
| Move before previous / after next sibling | `⇧↑` / `⇧↓` | `Shift+↑` / `Shift+↓` |
| Indent (last child of the previous sibling) | `⇧→` or `⇥` | `Shift+→` or `Tab` |
| Outdent (right after the former parent) | `⇧←` or `⇧⇥` | `Shift+←` or `Shift+Tab` |
| Chat row: move to the previous / next work item | `⇧↑` / `⇧↓` | `Shift+↑` / `Shift+↓` |
| Chat row: move to the parent item (out of a top-level item) | `⇧←` or `⇧⇥` | `Shift+←` or `Shift+Tab` |
| Space root row (more than one Space): move the Space up / down | `⇧↑` / `⇧↓` | `Shift+↑` / `Shift+↓` |
| Group the selected items into a new or existing container (see [Restructuring](#restructuring)) | `⇧⌘G` | `Ctrl+Shift+G` |
| Undo / redo the last Outline change (see [Undo and redo](#undo-and-redo)) | `⌘Z` or `⌃Z` / `⇧⌘Z` or `⌃⇧Z` | `Ctrl+Z` / `Ctrl+Shift+Z` |
| Shortcut reference | `?` | `?` |
| Archive the selected Chat, or open **Archive…** for the selected work item | `⇧⌘A` | `Ctrl+Shift+A` |
| Context menu of the selected row | `⇧F10` | `Shift+F10` or `Menu` |

`⇧⌘A` is the app's **Archive session** shortcut (rebindable in **Settings →
Shortcuts**). Outside the Outline tree it archives the open Chat as before. In the
tree it acts on the selected row: a Chat row is archived the way the sidebar
archives it and the selection moves to the next row, a work item opens its
Archive dialog with the choice for its descendants, and Space and group rows
ignore it. The **Archive Chat** and **Archive…** menu entries show the key.

### Search

`⌘F` (`Ctrl+F`) focuses Overview's search when navigation has focus, selecting
the existing query. Search and the repository/attention filters apply to the
Outline. The fixed current-chat entry stays reachable.
The file editor's own Find still wins inside a file; typing in the Chat composer
keeps its existing Find behavior. In the search field, `Esc` clears a non-empty
query, then returns to the tree; `↩` or `↓` selects and focuses the first result.
Sessions retains its own grouping and sort state independently of these filters.

Every Work Outline command is also in the command palette and can be rebound in
**Settings → Shortcuts**. Only five commands ship with a global key, because
global keys also fire while a text field has focus:

| Command | Default |
| --- | --- |
| Open Outline | `⇧⌘L` / `Ctrl+Shift+L` |
| Toggle Outline Focus | `⌘;` / `Ctrl+;` |
| Open Full Chat | `⇧⌘↩` / `Ctrl+Shift+Enter` |
| Create Work Item | `⇧⌘I` / `Ctrl+Shift+I` |
| Toggle Linear Panel (only while a Space has Linear on) | `⌥⌘L` / `Ctrl+Shift+E` |
| Create Child Work Item, Rename, Move, Associate Chat with Work Item, Open Effective Instructions, Create Instruction from Chat, Open Required Actions, Start Chat in Work Item, Move Up / Down, Indent / Outdent, Show Outline Shortcuts | Unbound |

**Start Chat in Work Item** has no global key because `⌘↩` (`Ctrl+Enter`) sends
from the composer, but in the Outline tree `⌘↩` starts a Chat in the selected
work item. On a Chat row it starts one in that Chat's work item, and shows a
notice when the Chat is not in one.

Commands that need a selected work item open the Outline and show a short notice
when nothing is selected. Tooltips and the shortcut reference always show the
current bindings, including keys you have rebound.

### Jumping between a Chat and the Outline

**Toggle Outline Focus** (`⌘;`, `Ctrl+;` on Windows and Linux) moves the
keyboard between a Chat and the Outline tree. It works while you type in the
composer. Earlier builds used `⌘\`, which 1Password takes system-wide for
autofill. If you never changed that key, it moves to `⌘;` automatically; a key
you chose yourself is kept.

- From a Chat, it reveals and focuses the Outline, selects that Chat's row, expands any collapsed parent items, scrolls
  the row into view, and focuses the tree. If the Outline does not show the
  Chat (a Chat without a Space, or one hidden by a filter), the selection
  stays where it was and the tree is focused.
- From the Outline tree, it returns to the last Chat you opened from the Outline
  (or the Chat you jumped from), in the same layout, with its composer focused.
  If there is no such Chat, the Outline shows a short notice.

The jump never changes which Chat is selected and never clears a composer
draft. `⇧⌘L` (`Ctrl+Shift+L`) does the same Chat-to-row jump when you press it
in the composer of a Chat you opened from the Outline. `Esc` in the Outline is
not part of this: it still closes Details and menus.

### Opening the full chat

**Open Full Chat** (`⇧⌘↩`, `Ctrl+Shift+Enter` on Windows and Linux) opens the
Chat on the selected Outline row with its composer focused. From an already
open Chat it focuses that Chat's composer. Drafts survive this focus change.
With no eligible Chat, the Outline shows a notice. The command is also in the
command palette and on Chat row menus.

## Context menus

Right-click a row (or Control-click it on macOS) to select it and open its
menu. With the Outline focused, `⇧F10` (`Shift+F10`, or the `Menu` key on
Windows and Linux) opens the menu of the selected row. Each entry shows its
current key, including keys you have rebound; an entry without a key shows
none. Entries that cannot apply are greyed out, and hovering over one tells you
why (for example, **Move Up** on an item that is already first, or **Outdent**
at the top level).

| Row | Entries |
| --- | --- |
| Work item | Open Details, Start Chat, Rename, New Sibling, New Child, Move Up, Move Down, Indent, Outdent, Move To…, Group into…, Set Status (Pending, Active, Completed), Associate Chat…, Open Effective Instructions, Add Instruction, Copy Path, Archive…, Restore, Undo, Redo, Recent Changes… |
| Chat | Open Chat, Open Full Chat, Rename…, Associate with Work Item… (or Move to Work Item…), Move to Previous Item, Move to Next Item, Move to Parent Item (Move to Unassociated Chats from a top-level item), Remove from Work Item, Compare Instructions… and Apply Current Instructions (when the instructions changed), and the sidebar's Pin, Copy name + IDs, Close out worktree…, and Archive Chat |
| Space root (a Space row, or **Space root instructions** in a single-Space Outline) | New Work Item, Space Instructions, Execution Setup, Required Actions, Undo, Redo, Recent Changes…, Show Outline Shortcuts. A Space row also has Move Up, Move Down, Hide from Outline (Unhide on a hidden Space), Rename Space…, and Delete Space… |
| Unassociated Chats | Associate “Chat”… for the selected Chat (no menu without one) |

Linear and PR review sync add their own entries where they apply (see
[Linear](#linear) and [PR review sync](#pr-review-sync)).

In the menu, `↑` / `↓` move between entries, `↩` (`Enter`) runs one, `→` opens
**Set Status** and `←` returns, and `Esc` or a click outside closes it. Every
entry runs the same action as its key or command palette entry, so the same
confirmations apply. Set Status, Add Instruction, Archive…, Restore, Remove from
Work Item, Space Instructions, and Execution Setup open Work Item Details and
act there.

## Restructuring

Use the keys above, the matching palette commands, the **Move** picker (choose
**Place at Top** or **Bottom** with `←` / `→`), or drag and drop. Dropping on the
middle of a row makes the item the last child of that row. Dropping on the top
or bottom quarter places it before or after that row. Dropping a Chat row on a
work item associates the Chat with it. Descendants move with their item.
Restructuring never deletes anything. A move that changes assigned Chats'
instructions shows an **Undo** toast instead of asking first (see Instructions,
revisions, and drift).

**Group into…** (`⇧⌘G`, or the item menu) puts several items under one
container in a single step. `⌘`-click a row (`Ctrl`-click on Windows and Linux)
to add or remove it from the selection, or `⇧`-click to add every item row from
the current row to the one you click. The selected rows are highlighted; a plain
click or `Esc` clears the selection. Only work items of one Space can be
grouped. Choose **New container…** and type a title to create a work item where
the first selected item was and move the selection under it, or choose an
existing work item to append them there. They keep their order on screen. An
item selected inside another selected item moves with it, and a work item can't
be grouped into itself or something inside it. The whole grouping is one undo
step: `⌘Z` moves everything back and archives the container it created, and
`⇧⌘Z` redoes it.

Thin guide lines show what is nested under what. Each parent's line runs down
from its disclosure triangle and turns into each child row (├, or └ for the
last child), and it stops after the parent's last child. Chat rows get the same
lines, slightly dimmer. The selected row's path up to its top-level item, and
the lines of its own children, are drawn in the accent color. Hover over a line
to highlight everything under that parent.

While you drag a work item, a Chat, or a Linear issue, the Outline shows where
it will land. A before/after line starts at the new parent's guide, so its
depth tells you the parent. A target row for "into" drops is highlighted. A
small label names the spot, for example **Into API** or **Before UI (under
Launch v2)**. After a keyboard move (`⇧↑` / `⇧↓`, `⇥` / `⇧⇥`), the moved row and
its new parent's line flash briefly. The flash is skipped when the system's
Reduce Motion setting is on.

Chats have no order inside an item, so with a Chat row selected the same keys
move the Chat between work items instead:

- `⇧↑` / `⇧↓` move it to the previous / next work item in row order, counted
  from the Chat's current item. Items at any depth count if the Outline shows
  them; Chat rows and archived items are skipped. From **Unassociated Chats**,
  `⇧↑` moves it to the last work item. Nothing happens at either end.
- `⇧←` (or `⇧⇥`) moves it to its item's parent. From a top-level item, it
  moves the Chat out of its work item to **Unassociated Chats**. Nothing
  happens for an unassociated Chat.
- `⇧→` and `⇥` do nothing on a Chat row.

Moves stay inside the Chat's Space. Only the selected Chat moves: Chats
started from it keep their own work items, and a Chat shown under the Chat
that started it stays there while its work item changes. The selection follows the Chat,
and the move applies at once, with the same **Undo** toast as **Move to Work
Item…** when the Chat's instructions change. Rapid presses apply in order. Chats without a
Space, and Chats that no longer exist, cannot move. A move never changes a
Chat's folder or execution setup. The palette's Move Up /
Down, Indent, and Outdent commands (and any shortcut you bind to them) do the
same as the keys on a Chat row. Archiving an item that has children asks
whether to archive them as well or move them to another parent first.

## Projects in the Outline

With more than one Space, the Outline shows one collapsible row per Space.
You can arrange and declutter these rows. Order and hidden projects are saved
on this device only, apply right away, and never change the Space itself.
A single-Space Outline has no Space rows, so none of this applies there.

- **Reorder.** Select a Space row and press `⇧↑` / `⇧↓`, choose **Move Up** /
  **Move Down** in its menu, or drag it onto another Space row. Dropping on
  the upper half of a row places the Space before it, and the lower half
  places it after. Projects you have never moved, including new ones, follow
  the ones you arranged, in alphabetical order.
- **Hide.** Choose **Hide from Outline** in a Space row's menu. The Space
  and its Chats disappear from the Outline only. The Sessions presentation
  still shows them. While any Space is hidden, the Outline toolbar shows
  **Hidden projects (N)**. Click it to list hidden projects as dimmed rows, and
  click **Unhide** on a row (or choose **Unhide** in its menu) to bring it back.
- **Rename Space…** opens the same rename dialog as the sidebar's Space menu.
- **Delete Space…** opens the sidebar's **Remove Space?** confirmation.
  Removing a Space permanently deletes its sessions: its Chats leave Mast's
  index, and its whole Work Outline (work items, instructions, and requests) is
  deleted. This can't be undone. To just declutter the Outline, use **Hide from
  Outline** instead. Cancel deletes nothing.

### Project documentation inventory

In a project's Details, choose **View documentation inventory** to open its saved
inventory or create a task that checks project-owned artifacts and plans. The
inventory lists the sources found and any pending items. Reconciliation and edits
are not performed by this check. The project owner device must be available and
running a version of Mast that supports the inventory when a new check is needed.
A saved inventory can still be opened while that device is unavailable.

## Work Item Details

Details open in the main workspace while Overview stays in the left column.
Closing Details restores another open Overview panel or returns to the selected
Chat. Sections stay collapsed until you open them.

| Section | Contents |
| --- | --- |
| Header | Breadcrumb, title, status, **Start Chat**, **Archive…** or **Restore**. |
| Description | Free text. |
| Instructions | This scope's own instructions, the inheritance toggle (non-root items), reordering, and **Save** / **Cancel**. |
| Effective instructions | What a Chat launched here receives, in order, marked Local or Inherited with the source item, plus the precedence rule and the size budget. |
| Execution | The resolved setup and its source (this item, an ancestor, the Space default, or the Space folder), **Edit**, **Validate**, and **Reset to inherited**. Chats running with a different setup are listed as exceptions. |
| External references | PR, ticket, and URL links. They open according to your web-link preference. |
| Chats | Associated Chats. Use **Associate…** to add one or **Disassociate** to remove one. |
| Required actions | Open and resolved requests for this item and its descendants. |
| Proposed instructions | Instructions that agents have proposed and that are waiting for review. |

The Space root has its own Details (**Space root instructions** at the top of a
single-Space Outline, or **Instructions** on each Space row) for Space-wide
instructions. It also holds the Space settings below.

### Space settings

| Setting | Choices | Effect |
| --- | --- | --- |
| Worker default model | **Default (Sonnet 5.5)** or any Claude Code model the device lists | The Claude Code model a worker chat starts on when its launch names none. An explicit model wins. Chosen in the Space root's **Default models** section, which offers only models `list_models` reports for Claude Code; once the list has loaded, a stored model it no longer has stays visible, flagged, until replaced. |
| Manager default model | **Default (Opus 5.5)** or any Claude Code model the device lists | The same for a Manager launch (including queue auto-start). Both are also set with the user-only `setRoleDefaultModel` mutation. |
| Worker context window | **Default (200K)**, 64K, 128K, 272K, 500K, **No cap** | The context window a new Codex worker chat starts with when its launch names none. |
| Worker tool-call checkpoint | **Default (150)**, 100, 250, 400, **Off** | How many tool calls one worker turn makes between checkpoint nudges. |

**Credit fallback.** A worker chat (a chat with a parent) that ends a turn
errored on a usage or credits limit keeps going on its own, once per error.
A Claude per-model weekly limit (the Opus or Sonnet window, code
`usage_weekly_model`) moves the chat down one model (Opus or Fable to Sonnet,
Sonnet to Haiku, never up) and runs the same turn again. Any limit shared by
the whole account (`usage_5h`, `usage_weekly`, a Codex out-of-credits error or
a Cursor `usage_limit`), or a per-model limit at the bottom of the ladder,
starts a successor worker on the same work item, in the same worktree, on the
first other enabled harness that the chat's fallback chain has not used
(Claude Code on the Space's worker default model first). The successor's first
prompt names the stalled chat, which is kept. With no harness left, Mast only
reports. Cursor's `rate_limit` is transient and is left alone. Each switch
sends the parent one line. A chat that was already errored before this
shipped, or one the user moves to another model first, is not touched. Which
limits are per-model and which per-account follows what each CLI reports; a
limit Mast can't classify counts as per-account. There is no switch for it:
turn off a harness in Settings and the fallback never picks it.

A project's Manager has no parent, so it follows its role's fallback chain
instead (the **Manager fallback chain** in the Manager chat's Settings tab). Its role passes to a fresh Manager chat, and the old chat is
archived, on the first chain entry after the one it is on whose harness is
enabled here; the new chat gets the latest Manager status and any messages
the user had queued. A per-model Claude limit steps the Manager down in place
only when the next entry is Claude Code on a cheaper model. An empty chain
leaves the Manager errored as before. With no usable entry left, or a hand-off
that keeps failing, the Manager chat gets a notice and the project gets one
Required Action. The personal assistant has no chain and is never switched.

**Worker tool-call checkpoint** keeps one long worker turn from resending its
whole context on hundreds of calls. When a worker chat's turn reaches the
chosen number of tool calls, and again at each further multiple, Mast sends it
a Mast system message asking it to summarize its progress and either continue
with only the steps still needed or hand back to its Manager. It never stops
the turn. It applies to worker chats on Claude Code and Codex, which accept a
message mid-turn; Managers, orchestrators, PR review chats, scheduled tasks and
chats you start yourself are not nudged. Subagent calls are not counted. The
count restarts each turn, and a change applies from the next turn. Each nudge
is recorded as a `callNudge` usage diagnostic. Only you can change it; agents
cannot. **Off** sends none.

**Worker context window** keeps long worker chats from resending hundreds of
thousands of tokens on every call. The control shows the effective value, for
example "Effective: 200K tokens". It applies to new Codex worker chats only:
chats that are already running keep the window they started with, Claude
workers and chats you start yourself are not affected, and a window passed to
`create_chat` wins over the setting. **No cap** leaves the model's own window.
A Space that never changed it uses Default. Only you can change it; agents
cannot. A size larger than the chosen model's own limit is dropped at launch,
so the model's limit applies.

## Instructions, revisions, and drift

- Instructions save only when you click **Save** (or press Enter in the editor).
  Closing Details, switching items, or pressing Escape with an unsaved draft asks
  you to keep editing or discard. Blur never saves.
- Turning off inheritance on an item starts a new boundary. Ancestor work-item
  instructions stop there, but Space-root instructions still apply.
- A Chat launch records an immutable **revision** of its effective instructions.
  A revision allows at most 9,000 characters and 32 entries.
- If the instructions change after launch, the Chat shows **Instructions
  changed** in its header, on its Outline row, and on List rows. The
  Chat continues to use its applied revision until you act. Click **Compare** to
  see the difference, then **Apply** to use the current instructions.
- Apply never changes a running turn. It takes effect at the next turn:
  - **Claude Code** resumes the same conversation with the new instructions.
  - **Codex** and **Cursor** cannot replace instructions inside an existing
    session reliably. Your next send asks you to start a fresh session, and
    the agent loses the conversation's context if you confirm. Cursor receives
    the instructions at the top of the first prompt of a session. If Cursor
    later summarizes a long conversation, the next prompt, in the same run or
    after a restart, restates the instructions once.
  - ACP harnesses do not support Work Outline instructions. Sends
    that require instructions are rejected with an explanation.
- Moving a work item (keys, drag and drop, menu, **Move To…**) or moving,
  associating, or disassociating a Chat (including **Associate…** and
  **Disassociate** in Work Item Details) applies at once. If it changes an associated Chat's
  instructions, a toast says "Moved <title> · instructions changed for N chats
  (they keep their current instructions)" with **Undo**. The toast does not take
  focus, so keys keep working in the tree. It closes itself after about six
  seconds, or on Esc or ✕. Rapid moves share one toast whose count grows, and
  **Undo** reverts only the most recent move.
- Before you toggle inheritance, edit or archive an instruction, or change
  execution configuration, a confirmation counts the affected Chats and items if the change would alter
  any associated Chat's instructions.
- **Create Instruction from Chat** turns the selected transcript text (or the
  last assistant message) into an instruction for this item, its parent, or the
  Space root. The dialog shows how many descendants it reaches.
- Type `/` in an instruction editor (item or Space root instructions, Create
  Instruction from Chat, or a proposal review) or in the Start Chat prompt to
  pick a skill, as in the chat composer. The list shows your user skills plus
  the project skills of the folder the item runs in (its resolved execution
  root, or the Space folder for Space-root instructions). Use the arrow keys to
  move, Enter or Tab to insert, and Escape to close the list. Instructions
  receive plain text such as `` `code-review` skill ``. Start Chat inserts
  `/code-review`, the same form the composer uses.
- The Start Chat dialog picks the agent with the chat composer's own model
  picker: harness, model, reasoning effort, and model options, plus the Ask /
  Auto approval chip. It starts with what a new chat in the composer would use
  (your last-used picks), not the selected chat's settings. The new Chat runs
  with that selection, and the selection becomes your last-used pick for new
  chats. Tab moves from the prompt to the model chip and then the approval chip.
  Enter or Space opens the picker or toggles approval. Escape closes an open
  picker first, then the dialog. Enter in the prompt starts the Chat.
- Review PR starts its Chat with the same new-chat selection.
- Multi-line editors (instructions, description, proposal review, Create
  Instruction from Chat, and the Start Chat prompt) grow with their text from
  three lines up to 40% of the window height, then scroll inside the editor
  and keep the caret in view.

## Execution setup and validation

- The setup applies to the whole configuration: the nearest item that has its
  own setup wins entirely, and fields are not merged across levels. The
  execution root can be any folder except your home directory. Every configured
  repository must be a Git checkout.
- **Validate** checks the saved setup on the device that owns the Space.
  **Start Chat** validates again, and Start stays disabled until validation
  passes. When the setup does not pin a worktree, the dialog lets you choose the
  Space workspace, an existing checkout, or a new worktree.
- Mast does not fall back to another checkout. If a path, repository, or
  worktree is missing, moved, pruned, or offline, launch or resume is blocked
  with the problem and how to fix it. Mast never creates or switches a branch
  silently.
- Each Chat keeps the setup it launched with. Later edits never retarget existing
  Chats, so a Chat whose setup now differs is marked as an exception (folder
  mark).

## Required actions and notifications

- When an agent needs something from you, it files a request on its item. The
  item shows **User Action Required** with a count, and status totals roll up to
  its ancestors.
- **Required actions** in the Outline toolbar lists every open request, oldest
  first, with its item, its originating Chat, and its age. Click **Open** to go to
  the Chat. The same list appears in Work Item Details (the item and its
  descendants) and in the Space root's details (the whole Space).
- Requests that [PR review sync](#pr-review-sync) raises have no Chat: their
  origin reads **GitHub**, **Open** shows the PR item (or the PR once the item
  is gone), **View PR** opens the PR, and **Resolve** is the only answer
  (there is no agent to reply to). They resolve themselves when you submit
  the review or the PR is merged, closed, or no longer asks for you.
- **Reply & resolve** answers the agent. Type your answer in the note box and
  press Enter (or Cmd+Enter). Mast sends it to the Chat that asked, as your
  own message: `Re: "<request>": <your note>`, with the request cut to about
  120 characters. Mast resolves the request with the same note only after the
  Chat accepts the message:
  - If the agent is idle, the message starts its next turn.
  - If the agent is busy, the message is queued until the turn ends, like a
    message you queue from the composer.
  - The message uses the Chat's selected instruction revision.
  - If the Chat refuses the message, the request stays open and the error
    shows. Reasons include a host that can't deliver the instructions, a
    context reset waiting for your confirmation in the Chat, or an execution
    setup that no longer validates. **Open chat** next to the error takes you
    there.

  Reply & resolve stays disabled until you type a note. Retrying never sends
  a second message. If the message went out but the resolve failed, the retry
  only resolves. After a reply, **Open chat** appears on the resolved
  request.
- **Resolve** closes a request without messaging the Chat. The note is
  optional and is only recorded on the request. If the Chat that asked was
  archived or deleted, Resolve is the only choice: Reply & resolve is
  disabled, and its tooltip says why. Escape or **Cancel** closes the note
  box. Resolved requests are kept under **Resolved**.
- Mast notifies you when an item goes from zero to one or more open requests:
  one notification per item, with retries deduplicated. Notifications follow your
  notification, background-only, and input-sound settings. Requests always
  appear in Required Actions, even when notifications are off.
- Completing an item with open requests asks for confirmation and resolves them.

## Linear

Linear is off until you turn it on for a Space, and it never writes to
Linear. While it is off, a Space shows no Linear UI, makes no Linear calls,
and keeps pasted Linear URLs as plain links. Nothing is added to the Outline
automatically: items and links come only from the actions below.

- **Connect.** In **Settings → Linear**, add a named account with a personal API key. Mast validates the key and stores it in the Mac's Keychain. You can add multiple accounts; an existing key is the Default account.
- **Turn it on.** Open the Space-root details (**Instructions** on the Space row), switch on **Linear**, and select the account for that Space. Without a key, **Connect Linear…** opens Settings → Linear. The first time you paste a Linear URL in a Space with Linear off, Mast offers **Enable** once.
- **Linear panel.** Press `⌥⌘L` (`Ctrl+Shift+E` on Windows and Linux;
  rebind **Toggle Inbox column** in Settings → Shortcuts), click **Inbox** in
  the Outline toolbar, run **Toggle Inbox Column** from the command palette, or
  choose **Open Linear Panel** on a Space row. The panel opens in the
  main workspace beside the persistent Overview, above the
  [GitHub section](#pr-review-sync) when that applies. Work Item Details and
  Required Actions can remain beside Inbox; the live chat is restored when
  those panels close. The column's × closes both integrations.
  - The key works from Overview and the Chat composer. With the panel closed it opens it and puts the cursor in its
    search. With the panel open and the cursor elsewhere, it moves the cursor
    to the search. With the cursor in the panel, it closes the panel and
    returns to the Outline tree. `Esc` in the panel also closes it.
  - With no Space on Linear and none with PR review sync, the key and the
    command only show a notice.
  - Drag the panel's right edge to resize it (double-click the edge to reset).
    Its width and whether it is open are remembered on this device. If the
    window is too narrow for the panel, the Outline, and the right-side panel,
    the Linear panel narrows first, then floats over the Outline's left edge
    instead of squeezing it.
  - With Reduce Motion on, the panel opens and closes without animating.

  The panel lists **My issues** and **My projects**, each header with its count
  (of rows matching the search). Click a header to fold or unfold its list, or
  focus it and press `←` to fold and `→` to unfold. Expand a project to see its
  open issues. Folded sections and expanded projects are remembered on this
  device. Search filters both lists. Lists load when the panel opens and when
  you click ↻. Rows that an item in this Space already links show
  **Linked · Reveal**, which selects that item in the Outline. With Linear enabled in several
  Spaces, click the Space name in the panel header to switch.
- **Create work item** (**Create** on an issue, or double-click the issue, or
  focus its row and press `↩`) adds `ENG-123 · title` as the last child of:
  - the work item linked to the issue's Linear project, whatever is selected
    in the Outline. If several items link that project, the most recently
    linked one wins. The button then reads **Create under \<item\>**.
  - otherwise the selected work item. With a Space row selected it goes to the
    top level. With nothing usable selected, the panel asks for the parent.

  Dragging an issue onto an Outline item always creates it there. The new item
  is selected when it appears.
- **Link** on an issue or project links it to the selected work item.
- **Create nodes for selected** (check issues in **My issues** or in an
  expanded project) creates one item per checked issue, in order, each by the
  same rule: under its project's linked item, else under the selected item.
  When they all share one linked item the button reads **Create under
  \<item\>**.
- **Add issues from \<project\>…** on a project-linked item (its right-click
  menu, or its Details **Linear** section) opens a picker of the project's open
  issues. It works with the Linear panel closed. Issues that already have a
  work item in this Space show checked and greyed out, with **Added ·
  Reveal**. Search filters the list, **Select all** checks every shown issue
  not added yet, and **Create** adds one work item per checked issue as the
  item's last children, then selects the first new one. `Esc` or **Cancel**
  closes it.
- A project link made by pasting a project URL is keyed by the URL. It still
  counts for the rules above (Mast matches the id at the end of the URL).
  Once the panel has loaded **My projects**, its project chip shows **Fix
  link**, which relinks it by the project's id (link anew, then remove the old
  link).
- Retrying a create or link never duplicates it.
- **Linked items.** Items linked to an issue show an icon cluster like a PR
  row's: **assignee · needs you · status** (the state dot and a status icon:
  done, canceled, started, backlog). The "N active · N user action required"
  text, the chat count and the references icon move into the hover, which also
  has the identifier, state, priority, assignee and title; click the status to
  open the issue. On a narrow Outline the state dot drops first, then the
  assignee; the attention dot and the status icon stay. Items linked to a
  project show a project chip. In Work Item Details, the **Linear** section has
  the full chip, ↻ (refresh every linked item in the project from Linear) and
  **Unlink**.
- **Synced fields.** An issue item's title and description follow Linear
  (Mast syncs about every 15 minutes, and on ↻) until you edit them in Mast.
  A field that still follows Linear shows **Synced from Linear**. When you link
  an issue to an item that already has a description, your description is kept
  and only the title follows Linear.
- **Pasting** a Linear issue or project URL into **External references** creates
  an issue or project link when Linear is on.
- **Right-click** an issue item for **Open in Linear**, **Refresh from Linear**,
  and **Unlink Linear Issue**, and a project-linked item for **Add Issues from
  \<project\>…**.
- If the engine on this device has no Linear support, Linear surfaces say
  **Linear isn't available on this host yet**.

## PR review sync

PR review sync keeps one work item per pull request that asks for your
review, in the Space's GitHub repositories (the Space folder's checkout,
its submodules, and nested repositories, matched by `origin`). It is off until
you pick a destination, and it never writes to GitHub. It runs on the device
that owns the Space, through that device's `gh` login.

- **Pick the destination.** The quickest way is the Inbox column's GitHub
  section: while the sync is off it leads with **Auto-sync review requests
  into… Choose item ▾**, a searchable menu of the Space's live work items
  plus **Create "PR reviews" item** (a new top-level item that becomes the
  destination). Once set, the section reads **Syncing into <item> · last
  synced 5m ago** with **Sync now**, **Change ▾**, and **Stop** (synced items
  stay where they are); click the item's name to reveal it. The Space-root
  details show the same: **Auto-sync review requests into… Choose item…**,
  then **Syncing into <item>** with **Change…** and **Stop**. Or right-click
  any work item and choose **Auto-sync PR Review Requests Here** (**Stop
  Syncing PR Reviews Here** on the destination). The destination row shows a
  small PR marker in the Outline ("PR review requests sync here").
- **What it keeps.** Each requested PR gets a child of the destination titled
  `<repo>#<number> · <PR title>` (renaming the PR later does not rename the
  item), linked to the PR, status Active, and a Required Action **Review
  requested: …** (**Re-review requested: …** for a later round, see
  [Required actions](#required-actions-and-notifications)). The sync runs
  every 3 minutes (first about a minute after the app starts), right after you
  set a destination, and on **Sync now** (GitHub section) or **↻ Sync now**
  (Space-root details, which shows the last result: `Synced: 1 created · 2
  updated · 0 resolved`, `Up to date`, or what went wrong). When you review, or the PR is merged, closed, or
  no longer asks for you, the round's request resolves and the item becomes
  Completed. A status you set yourself stands until GitHub's state changes
  again. Moved items keep syncing; archived ones are left alone.
- **PR icons.** Every item linked to a GitHub PR (synced or not) shows a row of
  fixed-width icons on the right, in projects with the sync, so they line up
  as columns: **author · needs you · your review · CI · PR**.
  - *Author:* the PR author's GitHub login, shortened to 10 characters (full
    login on hover), or **you** when you wrote it. Mast learns your login
    once from `gh` (`gh api user`); until it has it, every author shows the
    plain login. The chat's work-context strip says "you" the same way.
  - *Needs you:* the usual attention dot (and count). It replaces the "N active
    · N user action required" text on these rows; the chat count moves to the
    hover.
  - *Your review:* eye = review requested, refresh = re-review requested
    (both in the attention color), green check = you approved, orange circle-x
    = you requested changes; empty when there is nothing for you.
  - *CI:* a dot (green passing, red failing, amber running; none without
    checks).
  - *PR:* the pull-request icon, colored by state (green open, grey draft,
    purple merged, red closed). Click it to open the PR (in Mast or your
    browser, per **Open web links in Mast**).
  Each icon has its own hover; hovering the group shows the whole line (`#42 ·
  Open · CI failing · Re-review requested · by alex · 2 chats`). On a narrow
  Outline the title keeps at least about 120 px and the icons drop in this
  order: CI, review, author; the attention dot and the PR icon always stay.
  The **?** reference has an icon legend, lists the plan keys (⌥↑ / ⌥↓ section
  jumps, ⌥⌘S for the section rail and its ↑ ↓ Home End, ⌘F / ⌘G / ⇧⌘G find)
  and ⌘P, **Open the PR page**. In Work Item Details, the **Pull
  request** section shows the full text chip and title with **↻ Refresh**,
  **Review**, **Fix CI**, and **Open on GitHub**; it refreshes when Details
  open. A synced item also shows **Synced from GitHub · round N**.
- **PRs on your items.** Items also get their own PRs linked automatically:
  an item linked to a Linear issue gets the GitHub PRs Linear attached to the
  issue (on the Linear sync), and any item gets a PR its Chat created
  (`gh pr create` in that Chat, or a PR you linked to the Chat yourself).
  Only PRs of the Space's repositories; links are added, never removed or
  moved (a Chat moved to another item leaves its PR where it was), and a link
  you remove stays removed. The item's status is not changed when a PR
  merges. An item with several PRs shows one PR icon for each (up to three, then
  **+2** and so on, with the rest in its hover); the cluster's other slots
  describe the first PR. Details:
  [PRs on work items](pr-review.md#prs-on-work-items).
- **Review.** `⌘↩` (`Ctrl+Enter`) on a PR review item (a synced item, a child
  of the destination, or an item created from the GitHub section) opens Start
  Chat for a code review of its PR, filed under the item. The dialog shows the
  review prompt, the PR link and where the branch is checked out, with the
  regular model / reasoning / approval picker; nothing starts until **Start
  Chat** (see [Review PR launcher](pr-review.md)). Other items that merely
  link a PR keep the normal Start Chat. Right-click any PR-linked item for
  **Review**, **Fix CI**, **Open on GitHub**, and **Refresh PR Status**; they
  open the same dialog.
- **GitHub section.** The **Inbox** column (`⌥⌘L`, the toolbar's **Inbox**)
  shows a **GitHub** section for the Space under its Linear panel: the sync
  bar (above), then **Review requested** and **My open PRs**, each foldable
  (click **GitHub** to fold the whole section; folds are remembered on this
  device). Lists load when the column opens and on ↻. The header's **All repos
  ▾** menu filters both lists to one repository of the Space (and, with
  several Spaces, switches the Space); ↑/↓, ↩, and Esc work in every menu
  of the column. With a destination, each requested PR shows **Synced ✓** once
  its item exists (click the row to reveal it) or **Pending first sync** until
  the next pass adds it; a PR some other item links shows **Linked**. Manual
  actions sit in each row's **⋯** menu: **Start review** (filed under the PR's
  item, else the destination), **Link to selected item**, and **Create work
  item** (under the destination, else the selected item); or drag the row onto
  an Outline item to create it there. Retries never duplicate.
- **PR review chats (⌘P / ⇧⌘P).** With a destination in the current Space,
  the PR page's **Review chats** tab lists the destination's PR items (chip,
  Review, Fix CI) with their review Chats under each; click an item for its
  Details. Without one, the tab lists the launcher review Chats (the Chats with
  a `review_launcher` PR link).
- If `gh` is missing or signed out, the surfaces say so (`gh auth login`). If
  the engine on this device has no PR review sync, they say **PR review sync
  isn't available on this host yet**.

## Agent tools

Claude Code and Codex Chats that belong to a Space get a `mast-agent` MCP
server for the duration of each run. These are the Work Outline tools:

| Tool | Does |
| --- | --- |
| `work_outline_read_current` | Read the Chat's item and effective instructions. |
| `work_outline_create_child` | Create a child item (Pending or Active). |
| `work_outline_associate_current_chat` | Associate the calling Chat with an item. |
| `work_outline_set_status` | Mark an item Active or Completed. |
| `work_item_external_run_upsert` | Record or update a run outside a local harness (such as a codespace) on an item. |
| `work_item_external_run_list` | List the runs recorded on an item and its descendants. |
| `work_outline_create_user_action_request` | Ask you for something on the Chat's item. |
| `work_outline_resolve_user_action_request` | Resolve a request this Chat raised. |
| `work_outline_propose_instruction` | Propose an instruction for the item, its parent, or the Space root. |

Agents cannot:

- Add or change instructions. Proposals wait for your review.
- Complete an item that has open requests.
- Resolve another Chat's requests.
- Associate a Chat other than their own.
- Move their own Chat outside its current item's subtree, or re-associate a
  Chat that is bound to an execution setup.
- Act outside their own Space and device.
- Widen their sandbox or enable auto-approve.

Every change an agent makes needs an idempotency key, so retries do not create
duplicate items.

## Blueprints

A blueprint is a frozen **copy** of ANY item's subtree (titles, descriptions, instructions on every
node, which node is a project and which is a worker destination). Mast never decides what a project
looks like: the structure is whatever tree you saved. Chats, Required Actions, external links and
the tickets under a worker destination are not copied. It is not live: editing the blueprint does
not touch projects made from it, and editing a project does not touch its blueprint.

- **Save as blueprint…**: right-click any item (or use the button in the Project section of Work
  Item Details). The prompt is prefilled with the item's title; a project that already links a
  blueprint offers **Update blueprint** (replaces it, revision + 1) or **Save as new**. Saving is
  not an undo step. Duplicate names and the size limits come back as an error card.
- **Blueprints list**: the Space-root details panel lists the Space's blueprints (name, revision,
  item count) with **New from blueprint…** and **Delete** (confirms first) per row. Click a name
  to preview its tree: nodes, their instructions, and project / worker destination badges. To edit
  a blueprint, edit the tree it came from and **Update blueprint**.
- **New from blueprint…**: in the item menu (creates the item under that item) and the Space
  root menu (top level), or from the Blueprints list. Pick a blueprint, then name the item
  (prefilled with the blueprint's root title). Nothing else is invented: the new item is that exact
  tree. The whole tree is created in order and is ONE undo
  step: undo archives it with its items, redo restores it. A toast offers Undo.
- **From Linear**: on a project row in the Linear panel, **New project item…** picks a blueprint and
  a parent, names the item after the Linear project, and links it to that project. Use "Add Issues
  from <project>…" on the new item to pull in issues. Nothing is written to Linear.

- **Blueprint changed**: a project that links an older blueprint revision shows a "Blueprint changed
  (rev N → M)" chip (outline row and details) and **Review changes…**. Each difference (added or
  changed instructions, items the blueprint gained, a changed worker destination) has **Accept** or
  **Skip**; **Accept all** and **Mark reviewed** clear the chip. Items the project has but the
  blueprint doesn't are listed for information only and are never removed. Nothing applies by
  itself, and none of it is an undo step.
- **Convert to project…**: right-click an item that is not a project yet (or the button in its
  Project section). Pick the blueprint it should become; a preview lists exactly what will happen
  and **nothing is written until Apply**. Blueprint nodes that already exist as children (matched by
  title) are reused, the others are created with their instructions; existing children can be moved
  under a blueprint node (by default, children with an external link move under the blueprint's worker
  destination; click a row to cycle where it goes, or leave it where it is); the item is marked as a
  project, gets the worker destination and links the blueprint. Instructions the blueprint has for
  nodes that already exist are not written; the link then records "not reviewed" so **Review
  changes…** can apply them. Chats stay where they are. The whole conversion is ONE undo step.
  Nothing runs automatically.

## Limitations

- Work Outline instructions are added to Claude Code's system prompt. Mast
  cannot mechanically stop them from outranking repository instructions, so the
  instruction envelope states that they never override repository, global,
  skill, or safety instructions. Instructions do not reach harness-native
  subagents.
- Instructions have no semantic conflict detection. Within the Outline, the more
  specific instruction that comes later takes precedence.
- Codex and Cursor revision changes require a fresh session (see above). ACP
  harnesses cannot receive Work Outline instructions.
- List rows show the work-item path, attention, drift, and
  exception marks, but not status totals or proposal counts. The sidebar shows
  no Outline indicators.
- Opening a request does not scroll to the exact message in the transcript.
  Resolved history shows the 50 most recent requests.
- Outline data is not copied when you import a local profile into a synced
  profile. Deleted rows are garbage-collected after 30 days.
- The engine's loopback IPC is unauthenticated for its general methods. Only
  agent-attributed Work Outline writes require the per-run token.

## Undo and redo

Changes you make to the Outline are kept in an action log, one per Space, for this
session (the newest 200). With the Outline tree focused, `⌘Z` undoes the newest and `⇧⌘Z`
redoes it (`Ctrl+Z` / `Ctrl+Shift+Z` on Windows and Linux; `⌃Z` / `⌃⇧Z` also work on macOS).
A text field keeps its own ⌘Z: in the composer, the inline rename box, or an
instruction editor, ⌘Z undoes typing and never touches the Outline.

- **Undoable:** creating an item, renaming, moving, reordering, indenting, outdenting and
  dragging items, status and description edits, archiving (when sub-items are archived with
  it or it has none) and restoring, moving a Chat to an item or off its item, linking and
  unlinking a ticket or PR (a local link only), editing or reordering an instruction.
- **Not undoable:** changes made by agents or by other devices, completing an item together
  with resolving its open Required Actions, inheritance toggles, adding or removing an
  instruction, archiving an item whose sub-items were moved out, Linear and PR sync updates,
  and anything that wrote outside the Outline. Undo never writes to Linear or GitHub.
- **Feedback:** the Outline shows "Undid: moved ‘X’ under ‘Y’" (or "Redid: …").
- **Conflicts:** if the item or Chat changed since your action (someone archived it, moved
  it again, edited it again), undo says so and changes nothing.
- **Recent changes:** **Recent Changes…** in a work item's or Space's right-click menu lists the last actions, newest
  first. Choose one to undo everything back to it; rows already undone can be redone the
  same way.

### Projects and worker destinations

In Work Item Details, **Project → Mark as project** makes an item a project. Chats inside a project
(on the project or any item below it) can create, read and start chats for anything in that project,
but never outside it. On a project, choose a **worker destination** (e.g. its "Tickets" child):
workers an agent creates without naming a parent land there. Project settings are not undo steps.

### Project work queues

Each project has an ordered **work queue** of items that live inside it. In Work Item Details, choose
**Add to project queue** on an item (or **Remove from project queue**), and use **Up**, **Down** and
**Remove** in the project's **Work queue** section to reorder it. The Tracker shows the same queue as
**Queue**, with the same **Up**, **Down**, **Remove** and **Pause auto-start** controls, so you can
reorder and pause where you watch the work. Queuing does not move the item in the
tree. It is a `queueOrderKey` on the item, ordered the same way siblings are, so two devices that reorder
at once merge per item. Items from older builds simply have no key and are not queued.

An entry is **ready** (no worker attached, no open Required Action, no future `waitingUntil`), **running**,
**stalled**, **blocked**, **waiting** (a prerequisite is not complete yet, see below) or **done**
(completed, archived, a worker's final handback, or a linked pull request merged). The state is derived, never stored, from the same work status the Tracker uses for its
lifecycle stages, so a merged PR means the same thing in both.

A worker keeps its item **running** while a turn is in flight (or waiting for your answer), and for 30
minutes after its last activity if it did not error: workers often end a turn while a background build or
test run continues. Past that, or once the last turn errors, the item is **stalled**. A stalled entry stays
in the Tracker's Queue with its reason (`errored`, `out of credits`, or `idle 45m`) and the same
Up, Down and Remove controls. It does not use the queue's one running slot, so the next ready item is
offered, and it is not offered again. A chat that is not a worker, such as a planning chat, does not keep
an item running by sitting idle, and a worker that was archived without a handback no longer counts. A
Working session that stopped heartbeating (its host crashed or slept) counts as idle, by the same rule the
chat status dot uses. A final handback belongs to the assignment it was recorded for, so a chat reused for
another item does not mark that item done. The dispatcher re-checks a Space with a running item about once
a minute, so the next offer follows the 30-minute mark closely. The Tracker recomputes when it refreshes, so
a label can lag the engine until the next refresh.

**Prerequisites.** A queued item can wait on other items of the same project, set with `work_queue_set`
(`prerequisites`, at most 8; the prerequisite need not be queued itself). An entry that would be ready but
has an unmet prerequisite is **waiting**: the Tracker names the prerequisite and why, it is never offered,
and the entries behind it are still offered. Each item has a completion that decides when others waiting on
it may go: **reported** (the default) is the same as done above, while **landed** needs the linked pull
request merged, or a person completing the item. A worker's final handback, passing tests, or an agent
marking the item completed is not enough for landed, and neither is archiving it. To land an item an agent
completed, complete it yourself in the item's details. A prerequisite that is not an item of the project,
or that waits back on the item, keeps it waiting, and a write that would create such a cycle is refused. A
merge writes nothing to the outline, so the dispatcher re-checks a Space with an entry waiting on one about
once a minute, like a running item. The compact queue in the project's details shows `waiting: see Tracker`
because it has no pull request cache.

A ready entry whose linked pull request has not been looked up yet (a cold cache, or a failed fetch) is
held rather than guessed at: nothing is offered for it or behind it until the PR has an answer, so an
already-merged item is never offered. An open pull request with no chat attached stays ready.

When an entry is ready and fewer than one queued item is running, the Manager is offered it:

- A project with **no Manager** has one started for it, through the same launch as **Start Manager**
  (default harness and model), on the device that owns the Space. The item arrives in the Manager's first
  message.
- A project with a Manager gets a *queue wake* through the orchestrator wake path, on the device that hosts
  the Manager Chat. The usual batching and "user is typing" holds apply.

Each item is offered once per time it becomes ready. The offer is a Worker record in the engine's dispatch store (the one review dispatch uses),
keyed by the item and its queue key, and it is remembered across restarts. The wake that carries an offer is held in memory, so a record still unanswered after an
engine restart is offered again, once per restart, and once three offers have gone unanswered the next restart raises a blocking Required Action instead. A Manager that replaced the
recorded one is offered the item once, by the device hosting it. A worker chat started by a Manager or orchestrator on the item, on any device, answers the offer and the
record becomes started; a chat that is not a worker, such as a user's own or a planning chat, does not, the same rule that decides whether an item is running. A record is dropped when the item is done, archived, deleted, blocked or waiting; a running or stalled item keeps it. The Manager still writes the brief, picks the model
and starts the worker with `create_chat` on the item, so the existing launch claim prevents a second worker.
**Pause auto-start** in the Work queue section stops offers for that project; a running worker is not
interrupted, and **Resume auto-start** offers the next ready item. Moving an item out of its project (or unmarking a project)
drops its queue membership. Users and admin chats edit the
queue and the pause setting, and a project's Manager orders it with the queue tools; other agents cannot.

## Child templates

Configure a **Child template** in a parent item's details to apply a structure and
live instructions to its direct children. New children follow the rules; existing
children can use **Apply child template…**. Unlike a Blueprint, changes to the
child template apply as live rules.

### Scheduled task runs

A scheduled task can select a Work item in its Space. Each run is attached to
that item and receives the item's inherited instructions. Only the latest
unarchived run per task and device appears in item chat lists, associated-chat
counts, the unassociated chat list, and an agent's subtree read. Older and
retired runs keep their assignment, so a run still knows its item, and the
snapshot lists them in `hiddenRunChatIds`. They remain readable through
Scheduled tasks → Run history. See [Scheduled tasks](guide/scheduled-tasks.md).
