# Keyboard

Press `?` in the outline for the live list, which also shows keys you have rebound. Rebind app-wide keys in **Settings → Shortcuts**. On Windows and Linux `⌘` is `Ctrl`, but Mast is only tested on macOS. This page is checked against `ShortcutId` defaults in `crates/ui/src/settings.rs` and the outline key table in [docs/work-outline.md](../work-outline.md).

## Anywhere

| Key | Does |
| --- | --- |
| `⌘K` | Command palette |
| `⌘N` | New session |
| `⇧⌘N` | New project |
| `⌘/` | Open model picker |
| `⌘B` | Toggle left sidebar |
| `⌘R` | Toggle right sidebar (Changes) |
| `⌘E` | Toggle files panel |
| `⌘J` | Toggle terminal |
| `⌘P` | PR page (`⇧⌘P` also opens it) |
| `⌃Tab` / `⌃⇧Tab` | Next / previous session |
| `⌘1`–`⌘9` | Jump to session 1–9 |
| `⇧⌘A` | Archive session |
| `⌘S` | Save file |
| `⌘O` | Open a workspace file |
| `⌥⇧⌘O` | Search workspace symbols |
| `⇧⌘F` | Search text in workspace files |
| `⇧⌘R` | Reload browser page |
| `⌥⌘N` | Open the active side-panel tab in its own window |

## In a pop-out window

A pop-out handles `⌘S` (save) and `⌘W` (close the window; a file with unsaved edits asks first). Other app shortcuts act on the main window.

## In a code file

| Key | Does |
| --- | --- |
| `F12` or `⌥⌘B` | Go to the definition at the cursor |
| `⌘`-click | Go to an underlined definition |
| `⌘F` | Find in the file |
| `⌘G` / `⇧⌘G` | Next / previous match while find is open |
| `⌘G` | Go to a line when find is closed |
| `⌃-` / `⌃⇧-` | Back / forward through file locations |
| `⌥⌘G` | Show or hide the blame gutter (also in View) |
| `⌥↩` in symbol search | Add the selected symbol to the chat |

## Outline and chat

| Key | Does |
| --- | --- |
| `⇧⌘L` | Open the Outline |
| `⇧⌘I` | Create work item |
| `⌘;` | Toggle between a chat and its outline row |
| `⇧⌘↩` | Open full chat |
| `⌥⌘L` | Toggle the Inbox column (Linear and GitHub) |

## In the outline tree

| Key | Does |
| --- | --- |
| `↑` `↓` | Previous / next row |
| `←` `→` | Collapse / expand |
| `⌥↑` `⌥↓` | Previous / next sibling |
| `↩` | Open Details (a compact row opens its chat) |
| `⌘I` | Open Work Item Details on any item row |
| `⌘↩` | Start a chat in the selected item |
| `F2` or `R` | Rename |
| `N` / `⇧N` | New sibling / child |
| `⇧↑` `⇧↓` | Move up / down (on a chat row: to the previous / next item) |
| `⇥` / `⇧⇥` | Indent / outdent |
| `⌘Z` / `⇧⌘Z` | Undo / redo |
| `⌘F` | Search |
| `⇧⌘A` | Archive the selected row |
| `⇧F10` | Row menu |
| `?` | Shortcut reference |

## In a plan

| Key | Does |
| --- | --- |
| `⌘F`, `⌘G`, `⇧⌘G` | Find, next, previous |
| `⌥↑` `⌥↓` | Previous / next section |
| `⌥⌘S` | Focus the section list |
| `⌘=` `⌘−` `⌘0` | Zoom in, out, reset |

## Try it

1. Press `⌘K` and type "outline".
2. Press `?` in the outline and find one key you did not know.
3. Open **Settings → Shortcuts** and rebind one.
4. Press `⌘;` from a chat's composer to jump to its row, then again to come back.
