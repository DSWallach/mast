# The Work Outline

The outline arranges a project's chats under work items. An item can be a workstream, a ticket, a PR or an investigation. Every key, menu and rule is in the [reference](../work-outline.md). This page covers what you use every day.

New chats get a work item automatically. A chat started from an item, a child
chat, or a side chat keeps its existing work ownership. Folderless chats appear
in the host device's **Personal** Space, with a separate working directory for
each chat. An automatically created item's title follows its original chat
until you rename the item yourself.

Choose **Hand off to new chat…** from a chat's menu to continue its work in a
fresh conversation. This works for solo chats too. Older chats without an item
get one as part of the handoff.

## Overview and Sessions

Overview now occupies the left column beside your chat. Search, repository and
attention filters apply to the outline and its shortcut collections. The open
chat remains reachable above the filter, even when it falls outside the scope.
Click a chat or press Enter to open it in the main workspace; arrows only move
the navigation selection. Work Item Details, Required Actions and Inbox open in
the workspace without mounting a second live chat.

Choose **Sessions** for the previous session list, grouping, sorting, and metadata
controls. Its presentation is independent of Overview's search and filters;
both use the Space selector. Pins, custom sections, archived sessions and
scheduled-task groups remain available. On a narrow window the column floats
over the workspace; choosing a destination or clicking outside closes it.
Expanding the window restores the saved sidebar width and collapsed preference.

## Sections in the Sessions presentation

The Sessions list follows Work Outline groups by default, in the same tree order as
Overview. You can turn off **Settings → Appearance → Use Work Outline groups
in the sidebar** to restore your custom sections without changing their saved
membership.

When enabled, root groups become
sections; projects get their own sections named by their outline path. Sessions
on ticket or task items appear under their project's section, or the root group
when there is no project. Moving or renaming an outline item updates the sidebar.
An ordinary group such as Archive includes its descendants.

Manage these sections in the Work Outline. Their disclosure arrows collapse the
sidebar without changing the outline. Pinned sessions stay in Pinned; unpinning
returns them to their outline section. Existing custom sections remain available
for sessions without an outline group. Creating an Archive outline group does not
set Mast's native session archive flag or disable schedules.

## Items

- **New item:** click the toolbar button for the full dialog: title, Space, parent work item, description, status and placement. For quick inline creation, use `⇧⌘I` or `N` (a sibling), or `⇧N` (a child), type the title and press Enter.
- **Rename:** `F2` or `R`.
- **Move:** `⇧↑` / `⇧↓` reorder, `⇥` / `⇧⇥` indent and outdent, or drag and drop. Dropping a chat on an item puts the chat in it.
- **Status:** Pending, Active or Completed, from the row menu. An item with open Required Actions shows **User Action Required**, and the count rolls up to its parents.
- **Archive:** `⇧⌘A`. Nothing is deleted when you restructure.

## Instructions

An item can carry instructions. A chat started in it gets, in order, the Space's instructions, its parents' instructions, then its own. Turning off inheritance on an item starts a new boundary, but the Space's instructions still apply.

- Edit them in **Work Item Details** (`↩` on the row, or `⌘I`). They only save when you click **Save**.
- Type `/` in the editor to pick a skill.
- If you change the instructions after a chat has started, the chat shows **Instructions changed**. Click **Compare**, then **Apply** to use the new ones from the next turn. Codex asks to start a fresh session.
- Agents can propose instructions, but only you can accept them.

## Start Chat

Press `⌘↩` on an item (or click **Start Chat**). The dialog has the prompt, the agent, model, reasoning and approval pickers, and where the chat runs (the Space folder, an existing checkout, or a new worktree). Nothing starts until you click **Start Chat**.

## Required Actions

Agents ask for decisions on their item. Open **Required actions** in the outline toolbar to see all of them, oldest first.

- **Reply & resolve** sends your answer to the chat as a message and resolves the request.
- **Resolve** closes it without messaging the chat.

Mast notifies you when an item goes from no requests to some.

## Compact rows

An item with exactly one live chat shows as one row. The row splits into an item row and chat rows when you add a second chat, a child item, or a sub-chat. Press `⌘I` on any row to open its Details. On a compact row you can also click the status glyph.

## Undo

`⌘Z` undoes your last outline change and `⇧⌘Z` redoes it. Undo is kept in memory for this session and only covers changes you made in the UI, not ones an agent made. If the item changed since, Mast refuses and says nothing was changed. **Recent Changes…** in the row menu lists them.

## Try it

1. Make two items, "API" and "UI". Press `⇧↑` / `⇧↓` to reorder them, then `⇥` to nest "UI" under "API".
2. Add an instruction to "API", then start a chat in "UI". Open its **Effective instructions** in Details and see the inherited line.
3. Press `⌘Z` to undo the nesting.
4. Ask the chat to create a Required Action ("ask me which database to use") and answer it with **Reply & resolve**.
5. Press `⌘I` on a one-chat row.
