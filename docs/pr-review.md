# Review PR launcher

## Changes and references

The **Tour / Free diff** control switches between a walkthrough and the full diff. Hunk headers show the enclosing function when available. Click a gap control to reveal more context; Option-click expands the whole gap. Review threads appear as cards.

**Add to chat** attaches a code reference from a selection or hunk. Chips show moved or changed badges and a hover card. Opening a reference shows the pinned snippet; **Show diff vs current** compares it with the current file. Moved references offer reanchoring.

## Local drafts and review threads

In a branch or PR review, add a draft from a diff line. Drafts are stored
locally for the review target, survive reopening Mast, and can be edited or
deleted. **Add to chat** stages a copy for the existing composer workflow.
When the head changes, Mast matches the hunk and line content; a missing or
ambiguous match is listed as outdated rather than attached to a guessed line.

`review_publish` accepts `suggestedComments` with an anchor, body, severity,
optional suggestion block and optional stable `id`. Keep the same `id` when
republishing a suggestion so a dismissal remains dismissed even if its text
changes. Older suggestions without an id get a derived id. Suggestions are
local until accepted, edited, or dismissed; accepting creates a local draft.
Existing GitHub review threads are fetched on demand and shown read-only.
Adding, editing, accepting or dismissing a draft or suggestion, and viewing
threads, never posts to GitHub.

## Submit review

The only thing that writes to GitHub is **Submit review…**, in the Changes
pane's context bar for a PR review (it appears once the PR's local drafts have
loaded). It posts only when you click **Submit** in its dialog.

**Only your click posts.** `SubmitReview` (and its read-only companion
`PrepareReviewSubmit`) are served only on the app's own in-process connection.
The engine wraps the localhost WebSocket port that agents are given, and every
relay connection, in a gate that refuses them (`HUMAN_ONLY_METHODS`, applied
where each transport is built, not per tool). They are also never forwardable
to another device. A viewport attached over the local port, such as a terminal
UI or a Mast window attached to another process's engine, therefore cannot submit
either; there the button is shown disabled ("Submit from the Mac running Mast"). Agents still have `gh` on their own, which
Mast does not control.

The dialog has a **Comment / Approve / Request changes** choice, an optional
summary, and a count of the draft comments that will be sent. One click sends
one batched `POST /repos/{owner}/{repo}/pulls/{number}/reviews` through your own
`gh` login (the same runner as the PR page; tokens are never read or shown).
Each comment carries `path`, `line`, `side` and, for a range, `start_line` /
`start_side`. An accepted suggestion's body already holds its ```suggestion
block, so it posts as a suggestion.

- **Lines come from GitHub's diff, not your working tree.** When the dialog
  opens, Mast asks GitHub for the PR's head commit and its file patches and
  checks each draft's line (and its text) against them. The review is pinned to
  that head (`commit_id`); if the PR gains commits before you post, the post is
  refused and you re-check. If the head can't be read, Submit is off.
- **Can't be placed.** A draft is held back, with the engine's own reason, when
  GitHub's diff at that head doesn't agree with it: the drafted line and up to
  two lines of context either side (where the local hunk has them) must read the
  same on GitHub. Reasons: **not pushed** (the local checkout is ahead of the PR),
  **changed locally**, **no diff on GitHub** (the file isn't in the PR's diff, or
  GitHub has no patch for it) and **spans hunks** (a range whose ends fall in
  different hunks; it is never sent). Held drafts are never posted and stay local
  drafts. So do drafts added or moved after the check, until you reopen the dialog.
- **Click time.** On Submit, Mast re-reads the drafts and sends exactly what
  exists then, with current text. Only the drafts actually sent are removed.
- **Pinned PR.** Submit and Retry use the PR, head and review target the dialog
  opened on. Switching to another review closes the dialog without posting. A
  post already running always finishes and applies to its own PR; Esc, the
  trigger and Cancel do nothing while it shows "Posting…". The engine runs at
  most one submit per PR and answers a second with "Already posting".
- **Success.** The sent drafts leave the draft list and are kept as a local
  record (`PostedReview`: GitHub review id, URL, event, summary, the comments
  sent), readable with `GetPostedReviews {target}`. Cached review threads for the
  PR are dropped and reloaded, and the dialog shows **Open on GitHub**. If the
  local record can't be written, the dialog says so, the drafts stay, and the
  next attempt finds the posted review and records it.
- **Failure.** Nothing is removed; the dialog shows the error (including
  GitHub's validation message) and **Retry**.
- **Retry and restart are safe.** Every review body ends with an invisible
  marker, `<!-- mast-review:<nonce> -->`. The nonce is per review target, saved
  in the registry, and kept until a post is confirmed and recorded, so it
  survives a lost response and an engine restart. Just before the POST, Mast also
  saves the comments being sent, with their text, next to the nonce (durably). Every
  submit, the first included, lists the PR's reviews first; if one carries this
  marker, Mast records the review as posted, treats exactly the saved comments as
  sent (the review's comments are never used to decide that, so a comment GitHub
  reports differently can't cause a repost), sends nothing, and says
  **Already posted** with the link. The attempt saves each comment's text as sent,
  and the local record keeps that text. A saved draft is removed only if it still
  reads as it did when sent; one edited since stays a draft (a new comment next
  time) and the dialog says "N drafts were edited after posting and kept". Only an older attempt with
  nothing saved falls back to matching comment text (every page, outdated
  comments by their original line, a failed read is an error); a partial match
  there keeps the unmatched drafts. A check never renews the nonce. A submit
  retires a nonce that already belongs to a recorded review and starts a new one,
  all inside the per-PR lock (compare-and-set), so concurrent submits can't swap
  the nonce under an in-flight post. A `gh` call that times out is killed, not
  left running.
- **Unknown result.** If a post ends without a clear answer (timeout, lost
  connection), the dialog says "Result unknown; checking…" and runs the same
  check before any new post is allowed. A clear GitHub rejection (an HTTP 4xx)
  skips the check.
- **Pending review.** GitHub allows one pending review per user per PR. If you
  already have one (started in the browser), Mast detects it before posting (or
  from GitHub's rejection), sends nothing, and offers **Open pull request**.

Mast can check out a pull request and start a review chat.

## Re-review

The review-state row (`DiffReviewState`) keeps, besides the viewed marks, `reviewedHead` (the PR head the reviewer last
submitted a review at, or last caught up at), `reviewedAt` and `reviewedHunks` (the hunk identities of the diff at that head,
capped at 4096; over the cap `reviewedTruncated` is set and only the head is kept). `headSha` is different: it follows the PR head and drives mark
carry-over (`reconcile_review_head`).

- **Identity.** Hunks are keyed by `review_hunk_identity(path, changed lines)`; line numbers and context are excluded.
  Phases 1-5 already did this, so nothing migrated. Marks survive a head change by identity.
- **Baseline stamping.** `stamp_reviewed` runs on a successful submit (`commit_id`), on **Mark all reviewed** (both leave
  re-review mode), and on a mark change (`stamp_on_mark`) only when no changed-and-unviewed hunk remains and the baseline
  has a usable snapshot (an empty or truncated one never advances). The snapshot is the current full diff's identities for
  that head, never the delta's; if that diff is not on hand the head is recorded and the snapshot filled in when it parses.
- **Drafts in the Since view.** Drafts anchor against the full PR diff. Right-side drafts on lines the delta shows appear
  inline; others are listed under "Drafts outside these changes" and are still submitted. New drafts take the full diff's
  hunk hash; left-side drafting is disabled there.
- **Group.** `changed_since_files` lists files with an unviewed hunk outside `reviewedHunks`.
- **Range diff.** `GetCheckoutDiff` with `mode: "since"` (`baseRef`, `commitSha` = old head, `headRef` = new head) runs
  `diff_sync::since::capture_since_diff`. An ancestor old head with the same merge-base as the new head gives
  `git diff old new`. Otherwise (force-push, rebase, base merged in) it is an interdiff: each
  head against its own merge-base with the base ref, keeping the new head's hunks whose changed lines are absent from the
  old head's diff of the same file, then the old head's changed-line runs (contiguous +/- blocks, keyed by path and content, so regrouped hunks are
  not false removals) that the new head no longer has, as reversions whose hunk headers carry "Removed since you reviewed"
  and a little context. Each file is one `diff --git` block with kept and removed hunks, counts include both, and a dropped
  file with no hunks (binary, rename-only, mode-only) gets a one-line note. The UI lists removal-only files in a group of
  that name; removed code can't be commented on. Missing commits are fetched through the existing read-only `git fetch origin <sha>`;
  an unfetchable old head returns an error starting `old-head-unavailable` and the UI falls back to the full diff.
  Output is capped by `MAX_PATCH_BYTES`; results are cached per (repository, old, new, base ref), 8 entries, checked before any git runs; a commit that failed to fetch is not asked for again for 5 minutes.  The UI
  requests one pair once (the pair is part of the scoped-diff key).
- **Cleanup.** Closing or merging deletes the row (and with it the baseline) through the existing path.

## Entry points

- **⌘P / ⇧⌘P (the PR page)**: ⌘P is `OpenMyPrs` (customizable in Settings →
  Shortcuts, default `mod-p`, "Open PR page"); ⇧⌘P is a fixed alias; the
  palette's **My PRs** opens it too. The page is a port of pr-dash with three
  tabs: **Authored by me** (default) and **Review requested**, both fed by
  `PrDashboard` (see [pr-dash spec](internal/plans/work-outline/pr-dash-spec.md)), and
  **Review chats** (the destination list below, or the launcher review Chats). Each dash tab title carries its needs-you count (`urgent / total`,
  red when anything needs you, over rows that pass **hide drafts**); the page
  refetches the visible tab every minute and **Refresh** forces. A tab has
  bucket chips (multi-select), **hide drafts** (on), **group by status** (on)
  and a text filter, each kept per tab. Rows list repo, `#number` (opens the
  PR in the browser), title with tags, churn, reviewers (or author), CI, open
  comments and age, plus the Mast chats linked to the PR as chips (click
  opens the chat). Review-requested rows have **Review** (`/code-review`) and
  **Fix CI** (`/fix-ci`). Authored rows offer **Fix CI** while CI fails.
  A PR's **review chat** is a live (not archived) chat that is assigned to the
  PR's review work item, or linked to the PR as `manual` or `review_launcher`
  (mention, branch, ticket, diff-target and created-in-chat links stay plain
  chips). A finished or idle review chat still counts. While a PR has one,
  **Open review** is the row's primary action, in the accent color with the
  chat's title and state (working, idle, awaiting input, finished); **Review**
  moves into the row's **⋯** menu as **Start another review**, which also lists
  the other review chats (the most recent opens from the button). Up / Down
  move the highlighted row and **Enter** opens its review chat, or starts a
  review when there is none. The same "being reviewed" marker shows on the
  PR's row in the Outline and in the chat's work-context strip.
  The box in the header (all tabs) takes a PR URL
  (`https://github.com/owner/repo/pull/123`) or `owner/repo#123`.
- **Command palette → Review PR…** opens the PR page with the PR box focused.
- **PR review sync** ([Work Outline](work-outline.md#pr-review-sync)): `⌘↩`
  on a PR review item, **Review** / **Fix CI** in its Details or right-click
  menu, the Inbox column's GitHub section, and ⇧⌘P's destination list send
  `StartPrReview` with the item's `spaceId` and `workItemId`, so the review
  Chat is filed under that item.

Every one of these opens the Start Chat dialog in PR mode instead of starting:
the review prompt (`/code-review <url>` or `/fix-ci <url>`; the host adds the
reviewer or author wording, the branch and its location), the PR link, where
it runs (the checkout the host picks, below), and the composer's own model /
reasoning / approval picker, seeded with the sticky new-chat defaults.
**Start Chat** (Enter, or ⌘↩ from a chip) sends `StartPrReview` with the picked
agent; Esc cancels and nothing is sent. **Open review** on a PR that already
has a review chat stays a direct open with no dialog. After a start Mast opens
the new chat. Refusals (fork, repository not in any project, occupied worktree
path, `gh` missing, device offline) show in the dialog with what to do next,
and **Retry** reuses the same operation id. A behind-origin warning shows in the
sidebar notice and is also part of the seeded prompt.

## `StartPrReview`

`{repoOwner, repoName, number, action: codeReview|fixCi, spaceId?,
workItemId?, config?, fresh?, operationId}` → `{chatId, spaceId, deviceId, title,
prUrl, branch, checkoutPath, checkoutKind: existing|canonicalCheckout|
newWorktree, cwd, behindBy, prompt, notes[]}`. Errors are
`pr-review/<code>: <message>` with codes `invalid`, `fork`, `prNotFound`,
`ghUnavailable`, `repoNotFound`, `worktreePathExists`, `gitFailed`,
`deviceOffline`, `operationConflict`.

Steps, on the device that owns the Space:

1. **Space.** Among this device's Spaces (or the pinned `spaceId`), find a
   checkout of `github.com/<owner>/<name>`: the Space folder itself or one of
   its submodules (`.gitmodules`, up to three levels), matched by the raw
   `remote.origin.url` (https, ssh, and scp spellings), never by folder name.
   Main checkouts beat linked worktrees, then the outermost folder wins, so
   an outer workspace beats a Space opened on a nested repository.
   When no local Space matches, the engine asks each other device that owns
   Spaces and advertises `pr-review-launch-v1`.
2. **PR.** `gh pr view <n> --repo <owner>/<name> --json
   headRefName,isCrossRepository,title,author`. Forks are refused.
3. **Fetch.** `git fetch origin +refs/heads/<b>:refs/remotes/origin/<b>`.
4. **Location**:
   1. wherever `<b>` is already checked out (the canonical checkout or any
      worktree; the first `git worktree list` entry maps to the checkout);
   2. otherwise a new worktree `<root>/<name>-pr-<n>` (`git worktree add
      <path> <b>` when the local branch exists, else `--track -b <b> <path>
      origin/<b>`). `<root>` is the first line of
      `<space>/.devcontainer/.current-worktrees-root`, else
      `$WORKSPACE_WORKTREES_DIR` (the engine's environment), else
      `<space>/.worktrees`. An existing path is refused.
5. **Behind.** `git rev-list --count HEAD..origin/<b>` in the chosen checkout.
6. **Chat.** Created at the Space root with the
   caller's agent config (the UI sends the composer's new-chat agent; the
   engine defaults to Claude Code), titled `Review <name>#<n>: <PR title>`
   (`Fix CI …` for fix-ci), linked to the PR (a `ChatPrLink` with source `review_launcher`),
   assigned to `workItemId` when given (the item must be in the same Space),
   and the seeded prompt instructs the agent to work only in the chosen checkout and is queued last. The `review_launcher` PR link is what
   marks the chat as a launcher-created review chat; there is no separate
   classification.

The prompts are:

- code-review: ``/code-review <url> — someone else's PR; review it as a
  reviewer. Its branch `<b>` is checked out at <location>; review the PR's
  commits against its base, not local changes. Report findings to me here and
  don't post anything to GitHub unless I ask.``
- code-review of the viewer's own PR (its author is the engine's cached
  `PrViewer` login; an unknown login keeps the prompt above): ``/code-review
  <url> — my own PR; review it as its author, before or while I wait for
  reviewers: look for bugs, missing tests, risky changes and unclear diffs.
  Its branch `<b>` is checked out at <location>; review the PR's commits
  against its base, not local changes. Report findings to me here. Fix clear
  problems on the PR branch only if I ask, and don't post review comments or
  approve on GitHub.``
- fix-ci: ``/fix-ci <url> — my PR. Its branch `<b>` is checked out at
  <location>; do all the work there.``

`<location>` is `the canonical workspace checkout` (the Space root), ``the
worktree `<path>` (`/workspace/.worktrees/<dir>` in the devcontainer)`` (under
`<space>/.worktrees` or the worktrees root), ``the canonical checkout
`<path>` `` (elsewhere inside the Space), or `` `<path>` ``, followed by `, N
commit(s) behind origin/<b> — pull before you start` when behind. Mast adds
the devcontainer note only when the Space has a `.devcontainer` folder.

Starting a review posts nothing to GitHub.

A start with `fresh: true` (what the dialog's Start sends, since the user just
picked an agent for it) never reuses a live review chat for the same PR and
action: without it the host returns that chat (`reused`) and the pick would be
ignored. `fresh` is part of the inputs hash; an old host ignores the field.

### Idempotency

Chat, message, and command ids derive from `operationId`. The inputs are
claimed in the processed ledger (`pr-review:launch:<op>`), and the finished
reply is kept in `<store>/pr_review_launches/`, so a retry returns the same
reply without running `gh` or `git` again; the same id with other inputs is
`operationConflict`. The UI keeps one id per PR + action + agent for the life
of the window, so double-clicks and retries land on the same chat.

### Routing

`StartPrReview` is forwardable. With `spaceId` on another device the engine
forwards to that device; a forwarded call (it carries `targetDeviceId`) never
fans out again.

## Differences from launch.sh

- Repositories are any GitHub repo found in a Space, not the fixed
  `flagship|kb|ui|infra|workspace` list; the worktree folder uses the GitHub
  repository name.
- `claude --permission-mode acceptEdits` becomes the chat's agent config from
  the composer (approval mode included).
- The devcontainer mount note is conditional on `.devcontainer` (above).
- Chats are filed under a work item only with an explicit `workItemId`.
  The PR review sync's destination (one work item per project) collects the
  requested PRs as items, and every sync surface sends the PR item's id.

## PR review sync

With a destination picked for a project (the Inbox column's GitHub section
**Auto-sync review requests into… Choose item ▾**, the Space-root details, or
**Auto-sync PR Review Requests Here** on an item), the engine on the project's device keeps one
child item per PR that requests your review, each with a Required Action per
review round, and resolves them when you review or the PR closes. Wire
contract: `docs/plans/work-outline/pr-sync-contract.md`; user guide:
[Work Outline → PR review sync](work-outline.md#pr-review-sync).

When a synced PR is **merged** (or **closed** without merging, where the review is moot), the sync
resolves its Required Action, marks the item completed, and archives it as the system actor; the
item's Chats are left as they are. The same applies to an item you already completed by reviewing
once its PR merges or closes. Restoring an archived item keeps it restored: the sync archives it
again only after a new round (the PR reopened and your review requested again) finishes.

| RPC | Used by |
| --- | --- |
| `PrStatus {owner, repo, number, forceRefresh?}` | PR chips (cached ~2 minutes; Details and ↻ force a refresh) |
| `PrReviewSyncNow {spaceId}` | **Sync now** in the GitHub section's sync bar and the Space-root details |
| `PrReviewRequested {spaceId}` / `PrMyOpen {spaceId}` | the Inbox column's GitHub section |
| `WorkOutlineMutate setPrReviewSyncDestination` | the sync bar (**Choose item ▾**, **Change ▾**, **Stop**, **Create "PR reviews" item** after its `createWorkItem`), the Space-root picker and **Stop**, the item menu |

Failures read `github/<code>: …` (`ghMissing`, `unauthorized`, `rateLimited`,
`notFound`, `network`, `invalid`); a host without the RPCs shows "PR review
sync isn't available on this host yet".

When the current project has a destination, the **Review chats** tab lists the
destination's PR items (synced PR items) with their review Chats nested.
Without one it lists the launcher review Chats: the Chats with a PR link whose
source is `review_launcher`. Chat classification no longer exists.

## PRs on work items

Besides the review sync, the engine links a work item to the PRs that belong
to it, so the item shows the PR icons (state, CI, your review; the Details chip has the
full text) and the
Details **Pull request** section. Code: `crates/engine/src/work_outline_pr_links.rs`.
Each link is a `github` / `pullRequest` reference (`externalId`
`<owner>/<repo>#<n>`) added by the system actor. Status comes from `PrStatus`
like every other PR chip; nothing is fetched twice.

| Source | Applies to | When |
| --- | --- | --- |
| **Linear** (primary): the issue's GitHub PR attachments (Linear's GitHub integration attaches a PR whose branch or title names the issue) | items linked to a Linear issue, in projects with Linear on | the linked-item Linear sync: every 15 minutes, **↻ Refresh**, turning Linear on |
| **Chats** (fallback): a PR the item's Chat created | any item with an associated Chat | every 3 minutes, with the PR review sync loop (no `gh` call) |

What counts as "the Chat created PR X" (high confidence only):

- the Chat's durable PR link with source `manual` (the user set it) or
  `created_in_chat` (the live tap saw a `gh pr create` / `gh pr new` /
  `create_pull_request` tool call print the URL). `mentioned` (a URL in text
  or another tool's output) never counts, and neither does branch inference
  (`gh pr view <branch>`, which at a superproject root resolves every Chat to
  the same PR);
- backfill: a Chat with no such link has its stored transcript re-read once
  (at most 50 Chats per pass, logged) for PR-creation tool output; a hit is
  stored as a `created_in_chat` Chat link (a `manual` link is never changed).

Rules for both sources:

- The PR's repository must be in the project (checkout, submodules, nested
  repositories, by `origin`), and the project must live on this device.
- Links are only added: never removed, never moved, never duplicated (one
  reference per item and PR; the operation id is a hash of item and PR). A
  reference you remove is not added back. An item can link several PRs (a
  ticket's UI and Flagship PRs); the Outline shows an icon for each PR (up to three, then
  "+N"), and Details lists the others under the first.
- A Chat's PR is linked once, to the item the Chat belongs to when the PR is
  first seen, and only if no item of the project links that PR yet. Moving
  the Chat to another item later keeps the existing link where it is (the
  new item does not get it automatically).
- Item status is left alone: a merged PR does not complete its item. (A
  "complete the item when its PRs merge" toggle is a possible later option.)
- These items are not PR review items: `⌘↩` opens the normal Start Chat, and
  the review sync never adopts them for a review request (a requested review
  of such a PR gets its own item under the destination).
- The Linear attachment fields (`attachments { nodes { url title subtitle
  sourceType metadata updatedAt } }`) were not verified against the real
  API. The query is separate from the issue query, parsed leniently, and a
  failure is logged (`linear: issue attachments could not be read`, target
  `linear`) without stopping the title sync.

## Publishing a local review

Call `review_publish` with `{prUrl, headSha, review}`, or `{target, review}`.
For the URL form, `review.pr` includes `repo`, `number`, `headSha`, and
`baseSha`; a top-level `baseSha` does not replace `review.pr.baseSha`.
A PR target is `{kind: "pr", repo, number, baseSha, headSha}`. The review
payload uses `schema: "mast.review/v1"`.

Publication validates the committed diff in the chat's selected bound checkout
(including a submodule worktree), falling back to the chat working directory.
A branch target can explicitly supply `checkoutPath`. Publication does not
switch branches, fetch commits, or post to GitHub. Git validation errors include
the working directory and arguments, including any unresolved revision.

The response includes `artifactId`, `revision`, `url`, `storedPath`, and `changed`.
Link the returned `url` as `[Review](mast-artifact:<artifactId>)`. An absolute
Markdown file link under the selected checkout opens its source in the file
viewer; a saved `.review.json` file alone is not a published review artifact.
