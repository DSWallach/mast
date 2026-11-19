# Pop-out windows

Put a side-panel tab in its own window, for example the chat on one monitor and the code on the other.

## Open one

- Right-click a tab and choose **Open in new window**, or press `⌥⌘N` with the tab active.
- Works for file tabs, diff and History tabs, and subagent transcripts.

The tab leaves the main window and moves to the new one. It is the same editor or the same diff, not a copy, so edits, live updates and the engine's work are not duplicated. The window title is the tab title.

## What it does

- A file or diff you click through (open a file from a diff, open a commit from History) opens in the main window, in the chat the tab belongs to.
- `⌘S` saves, `⌘W` closes the window. A file with unsaved edits asks before it closes, and quitting Mast waits for it too.
- Closing a pop-out closes the tab. Nothing returns to the main window.
- Deep links, notification clicks, the menu bar and Appshots always go to the main window.

## Not yet

Browser, Artifact, Review, Tracker, Item and Terminal tabs, and chats, cannot pop out yet. Pop-out positions last for the session only and are not restored after a restart.
