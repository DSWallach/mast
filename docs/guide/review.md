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

Changes shows the enclosing function in hunk headers when available. Click the one-line gap control to reveal more context; Option-click expands the whole gap. Review threads appear as cards in the diff.

Use **Add to chat** on a code selection or hunk to attach a code reference. Its chip can show **Moved** or **Changed** and has a hover card. Open the reference to see the pinned snippet. If the code changed, **Show diff vs current** compares it with the current file; a moved reference offers a reanchor action.

## Submit a review

Nothing reaches GitHub until you click **Submit** in **Submit review…** (Changes context bar, PR reviews only). Drafts, accepted suggestions, edits and dismissals stay on your machine until then. Agents can read your drafts but cannot post: the app's own connection is the only one that can.

Choose **Comment**, **Approve** or **Request changes**, edit the optional summary, and check the count of draft comments. Mast checks each draft against GitHub's version of the PR (its head commit), not your working tree, then sends them as one review through your own `gh` login. Accepted suggestions arrive as GitHub suggestion blocks. Drafts GitHub can't place, because the code isn't pushed or differs from the PR, are listed as "can't be placed" with the reason; they are not posted and stay as local drafts. If the PR gets new commits first, the post is refused and you check again.

On success the sent drafts are cleared (Mast keeps a local record with the review link), threads refresh, and the dialog links to the review. If posting fails, nothing is lost and **Retry** is safe: Mast looks for a review it may already have posted, even after a timeout or a restart, and says **Already posted** instead of sending twice. If you already have a pending review on the PR from github.com, Mast tells you and offers **Open pull request** instead of replacing it.
