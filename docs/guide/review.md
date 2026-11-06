# Review a change

For a GitHub PR, sign in with `gh auth login`, press `⌘P`, and choose **Review** on the PR. Start the review chat and open its **Review** tab when the walkthrough is available. The review surface is also available full-width. The diff remains available as you read.

In **Tour**, follow the ordered steps and their notes and risk indicators. In **Free diff**, explore the diff in your own order. Hunks outside the walkthrough appear under **Not in the tour**. The progress count reflects hunks you explicitly mark reviewed; a faint signal after a hunk has been on screen is not a reviewed mark.

| Key | Action |
| --- | --- |
| `n` / `p` | Next / previous tour step |
| `]` / `[` | Next / previous hunk |
| `v` | Mark the current hunk reviewed |
| `t` | Switch Tour / Free diff |

Keys can be rebound and do not apply while typing. If the head changes, heed the stale warning and refresh the review. The review engine supports fixed commit ranges for worker branches, including unpushed branches, but there is not yet a button to start a branch review. Matching reviewed hunks can carry over to a PR review.
