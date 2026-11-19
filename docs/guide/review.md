# Review a change

For a GitHub PR, sign in with `gh auth login`, press `⌘P`, and choose **Review** on the PR. Start the review chat and open its **Review** tab when the walkthrough is available. The review surface is also available full-width. The diff remains available as you read.

Use the **Tour / Free diff** segmented control to switch views. In **Tour**, follow the ordered steps and their notes and risk indicators. In **Free diff**, explore the diff in your own order. Hunks outside the walkthrough appear under **Not in the tour**. The progress count reflects hunks you explicitly mark reviewed; a faint signal after a hunk has been on screen is not a reviewed mark.

| Key | Action |
| --- | --- |
| `n` / `p` | Next / previous tour step |
| `]` / `[` | Next / previous hunk |
| `v` | Mark the current hunk reviewed |
| `t` | Switch Tour / Free diff |

Keys can be rebound and do not apply while typing. If the head changes, heed the stale warning and refresh the review. The review engine supports fixed commit ranges for worker branches, including unpushed branches, but there is not yet a button to start a branch review. Matching reviewed hunks can carry over to a PR review.

## Changes and code references

Use **Map** in the Changes sidebar to see files grouped by folder and sized by added and deleted lines. Select a tile to jump to its diff; hover for its path, counts, viewed state, and any walkthrough risk note. Viewed files fade, and an outline marks a file with a risk note. The header shows review progress in files and lines. **List** shows folder totals and addition/deletion bars. Mast remembers your choice; List is the default. For very small diffs the map uses a row, and for large diffs it groups small files within folders.

Changes shows the enclosing function in hunk headers when available. Click the one-line gap control to reveal more context; Option-click expands the whole gap. Review threads appear as cards in the diff.

Use **Add to chat** on a code selection or hunk to attach a code reference. Its chip can show **Moved** or **Changed** and has a hover card. Open the reference to see the pinned snippet. If the code changed, **Show diff vs current** compares it with the current file; a moved reference offers a reanchor action.

## Submit a review

Nothing reaches GitHub until you click **Submit** in **Submit review…** (Changes context bar, PR reviews only). Drafts, accepted suggestions, edits and dismissals stay on your machine until then. Agents can read your drafts but cannot post: the app's own connection is the only one that can.

Choose **Comment**, **Approve** or **Request changes**, edit the optional summary, and check the count of draft comments. Mast checks each draft against GitHub's version of the PR (its head commit), not your working tree, then sends them as one review through your own `gh` login. Accepted suggestions arrive as GitHub suggestion blocks. Drafts GitHub can't place are listed as "can't be placed" with the reason (not pushed, changed locally, no diff on GitHub, or a range that spans hunks). Mast checks the drafted line and two lines of context either side; they are not posted and stay as local drafts. If the PR gets new commits first, the post is refused and you check again.

On success the sent drafts are cleared (Mast keeps a local record with the review link), threads refresh, and the dialog links to the review. If posting fails, nothing is lost and **Retry** is safe: Mast looks for a review it may already have posted, even after a timeout or a restart, and says **Already posted** instead of sending twice. If you already have a pending review on the PR from github.com, Mast tells you and offers **Open pull request** instead of replacing it.

## Review again after new commits

When a PR you already reviewed gets new commits, Mast shows what changed since you last looked. It remembers the head you reviewed at: the head when you submitted a review through Mast, or the head when you last caught up on viewed marks. **Mark all reviewed** sets it to the current head.

- **Viewed marks carry over.** A hunk keeps its viewed mark if its changed lines are identical on the new head, wherever it moved. Changed and new hunks reset and their files are listed first under **Changed since you reviewed**. The header shows how many files that is. Marking those hunks viewed shrinks the group.
- **Since abc1234 · 2d** (Changes context bar) switches the pane to just the changes between that head and the current one. After a force-push or rebase it compares each head against its own merge base and keeps only hunks whose content changed, so a rebase onto a newer main doesn't bury the real changes. Hunks the new head dropped (including whole files) are listed under **Removed since you reviewed**, shown as reversions, so you can see when something you asked for went missing. Marks are read-only in this view. Your drafts and GitHub threads still appear on the lines they belong to; items outside the delta wait in the full diff.
- If the old commit is gone from GitHub (cleaned up after a force-push), Mast says so and shows the full diff.

Closing or merging the PR clears all of this.
