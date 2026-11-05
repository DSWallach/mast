# Work Outline — implementation handoff

Prepared 2026-10-14, last updated 2026-11-02. Read this first in a new chat picking up the Work Outline work.

## Where things are

- **Repository:** `/Users/mikey/Projects/agent-mode-tools/zeron`, on GitHub as `Pluto8195/mast`
  (renamed from the `Pluto8195/zeron` fork on 2026-10-25, then `creel` → `mast` on 2026-10-26; old
  URLs redirect). `zeronsh/zeron`
  stays as the `upstream` remote, but we no longer open PRs there. The folder and submodule path
  are still `zeron/`, and so are all code identifiers.
- **Integration branch:** `work-outline-implementation`, branched from `main` at `3801cb5b`. It is
  the GitHub default branch. `main` is upstream's code at the fork point. **Never commit to
  `main`.** `origin` carries only these two branches; delete each feature branch once it is merged.
- **Parent repo:** `agent-mode-tools` branch `zeron-work-outline` pins the `zeron` submodule.
  Restore points: tag `pre-linear-integration` (`83f49b3d`) and parent commit `d9129f6`.
- **Original spec:** `work-outline-implementation-specification.md` (copy of Mikey's spec, plus an
  "Implementation contract amendments" section). The binding contract is `m0-contract.md` in this
  folder; it wins where it differs from the spec.
- **Installed build:** `/Applications/Mast.app` (since 2026-10-26). Since 2026-11-05 its bundle id is
  `computer.mikey.mast.dev` and its executable `Contents/MacOS/mast` (`zeron` is a symlink for one release); data
  stays in `~/.zeron` (new installs use `~/.mast`). Before that it shared `sh.zeron.app.dev` with the earlier
  `Creel.app` and `Zeron Dev.app`, which are
  rollbacks only (`Zeron Dev.app` is in `~/zeron-dev-app-backups/`); never run two at once. Rollback binaries are
  in `~/zeron-dev-app-backups/`, one per install, named after the feature. Keep only the last few.

## Built and installed

All milestones M0–M4 of the spec, then the follow-ups below. User docs: `docs/work-outline.md`,
`docs/pr-review.md`.

- **Outline (M1):** durable work items in the profile registry (row table, HLC last-writer-wins per
  field, 16 KiB op cap), Spaces as roots, chats under items, Canvas positions preserved.
- **Instructions (M2):** per-item instructions with revisions. Claude Code gets them via
  `--append-system-prompt-file` + `--system-prompt-snapshot off` on every spawn; Codex via
  `developerInstructions` with a fresh session on change (or "keep session"). Cursor/ACP refuse.
- **Execution (M3):** per-chat execution bindings, multi-repo roots, worktree assignments.
- **Required Actions and agent tools (M4):** `zeron-agent` MCP server with a per-Run token;
  `work_outline_*` tools; Reply & resolve sends the answer to the chat.
- **Keyboard and editing:** Tod-style keys (↑↓ in visual order, ⇧arrows move/indent, Tab/⇧Tab,
  F2/R rename incl. chats), drag and drop, right-click menus that show their keys, `?` reference,
  ⌘; outline⇄chat, ⇧⌘A archive, ⌘↩ Start Chat, ⇧⌘↩ Open Full Chat, ⌘F search, preview on select,
  Space root reorder/hide/rename/delete, tree guide lines, Undo toast for moves.
- **Latency fix:** the local profile never acked pending registry batches (67k queued); now
  `settle_pending_locally` when there is no edge, plus optimistic UI.
- **Editors:** skill autocomplete (always Claude Code + on-disk skills, regardless of the chat's
  harness), auto-growing instruction editors.
- **Review PR launcher (⇧⌘P):** matches the Cofactr workspace `launch.sh` flow.
- **Linear v1** (`linear-contract.md`): per-Space opt-in, key in keychain `sh.zeron.linear`, Inbox
  column on the left (⌥⌘L) with collapsible Issues/Projects. Nodes are created **manually only**
  (Mikey: "I don't want things changing out from under me"). Title/description follow Linear until
  edited; the full ticket goes to agents; project-linked item is the issue destination; "Add
  issues from project". Nothing is written to Linear. GraphQL is **not yet verified against the
  real API** (projects and attachments especially).
- **PR review sync** (`pr-sync-contract.md`): automatic items under one chosen destination item
  per Space (covers submodule repos), Required Actions "Review requested" / "Re-review
  requested", auto-archive on merge/close. PR links on ticket items come from Linear attachments
  first, chat-created PRs as fallback.
- **Planning artifacts phase 1** (`../planning-artifacts-design.md`, `../artifacts-contract.md`,
  agent conventions `docs/planning-artifact-conventions.md` v1.1):
  - Stored in `<canonical checkout>/.zeron/artifacts/<work item>/`, ignored via
    `.git/info/exclude` (never `.gitignore`); snapshots under `<profile>/artifacts/<id>/`.
  - `artifact_publish` / `artifact_get_plan` / `artifact_render_check` agent tools.
  - Render check uses an offscreen WKWebView snapshot (not screen capture), viewports × themes up
    to 4 images, and layout heuristics.
  - Links inside rendered artifacts route through Zeron. Files outside the workspace get "Open as
    artifact" (credential paths, `.env*`, and files over 10 MB are refused).
- **Visual compare for artifact revisions** (`crates/ui/src/artifacts/{compare,pixel_diff,
  markdown_diff}.rs`). Mikey's complaint: text diffs of charts are useless.
  - HTML, Mermaid and images: two revisions side by side with synced scroll. "Highlight changes"
    runs a pixel diff with region boxes; there is also onion skin and a Source tab for the line
    diff.
  - Markdown: rendered side by side, changed blocks tinted, section summary on top.
  - Options: compares two options of the current revision.
  - Verified only by headless tests.

- **Git uses the login-shell PATH** (2026-10-19): every engine `git` spawn goes through
  `zeron_harness::git_command()` / `git_command_async()`. Before this, worktrees in LFS repos
  (flagship) failed with "git-lfs: command not found" because the GUI app's PATH lacks
  `/opt/homebrew/bin`. New `git` spawns must use these helpers.
- **Error notices** (`crates/ui/src/error_notice.rs`): errors are a red card with full wrapped
  detail and a Copy button, and stay until dismissed. Used by the PR review launcher, Start Chat,
  composer send/worktree failures, and error toasts.

- **Coordinator agent tools** (2026-10-20): `create_chat { workItemId, prompt, … }` starts a
  child on the caller's item or a descendant (same launch path as Start Chat, so the target's
  instructions and execution binding apply; `cwd`/`branch` refused). `work_outline_read_subtree`
  lists the items, chats, and open Required Actions under the caller's item. Children cannot message
  their parent; coordinators collect results with `wait_for_turn` / `read_chat` / Required
  Actions.
  In the outline a child chat nests under its parent chat only when it has no item of its own or
  shares the parent's item; a coordinator's child shows under the item it was started on.

- **Outline fixes** (2026-10-20, first release by the coordinator):
  - Instructions longer than 4 lines collapse behind Show more / Show less. ↑ ↓ Edit Remove sit in
    the entry's header row. The instruction action "Archive" is now "Remove" and still has undo.
  - Synced PR and Linear chips keep their last data during a refresh or after a failed pull. They
    used to drop to "#42" / "ENG-1" on every 2-minute (PR) or 5-minute (Linear) refresh. A
    search-list row no longer overwrites a fully fetched PR status (`github/store.rs`,
    `linear/store.rs`).
  - An archived item with no visible child item is hidden even when it still has live chats; those
    chats show under Unassociated Chats. This used to leave archived synced PR items on screen,
    dimmed and immovable, because they keep their review chat (`work_outline/model.rs`).
- **Coordinator resolves its children's requests** (2026-10-20): `resolveUserActionRequest` is
  allowed when the request's origin Chat is the caller or a Chat whose `parentChatId` is the
  caller. This covers direct children only, not grandchildren or chats a person started
  (`m0-contract.md` § coordinator amendments, item 3).
- **Visual compare follow-ups** (2026-10-20):
  - The per-kind compare mode persists in `UiSettings` (`artifactCompareModes`).
  - Markdown panes scroll together by block position. Wheel and trackpad sync; dragging the
    scrollbar does not.
  - Highlight changes and onion skin cover the full render, up to 8000 CSS px and 20 M px per
    snapshot. Taller content is cut off silently. The agent render check is unchanged.
  - Options compare can follow one option across two revisions. The engine snapshots option files
    per revision, and `ArtifactContent` serves them by revision. Revisions recorded before this
    show "not found" until the artifact is republished.
  - The tall-snapshot path has only run in headless tests, never against a real WKWebView.
- **Plan reading: ⌘F find and section index** (2026-10-20; `artifacts/{find,outline}.rs`,
  `artifacts/tab/find_view.rs`, `files/markdown_find.rs`):
  - ⌘F opens a find bar while focus is in an Artifact tab. It shows a match count and highlights
    every match. Enter / ⇧Enter and ⌘G / ⇧⌘G step through matches with wraparound, and Esc closes
    the bar. Outside an Artifact tab, the shell's ⌘F and ⇧⌘G handlers `propagate()` when their own
    route doesn't apply. ⌘F is still outline search everywhere else.
  - A collapsible section rail lists h1–h3 headings: click to jump, and the section in view is
    highlighted. It's hidden under 3 headings and for options, source, compare, images and Mermaid.
  - HTML find runs over postMessage into the sandboxed frame, and WKWebView forwards ⌘F, ⌘G, ⇧⌘G
    and Esc to the app. Highlights need macOS 14.2+. Matches that span inline tags aren't found.
    Find and the index are off for HTML on Linux.
  - Only headless tests have run. Key forwarding, the frame script under the CSP, and active-section
    tracking haven't been seen in a real WKWebView.
- **Plan section keys** (2026-10-20):
  - With the section rail focused (click it, or press ⌥⌘S), ↑ / ↓ / Home / End move through
    sections and scroll the document. Enter or Esc returns focus to the document.
  - ⌥↑ / ⌥↓ anywhere in the plan jump to the previous / next section, with no wraparound. These keys
    are bound only in the ArtifactTab key context.
  - `browser/macos.rs` now maps AppKit's special-key characters (arrows, Home / End, Esc) to
    combos. Before this, Esc forwarding from WKWebView probably never worked.
  - The `?` reference doesn't list these keys yet.
- **Compact icon PR / Linear rows** (2026-10-20, `github/row_icons.rs`):
  - PR rows end in fixed slots, in this order: author, needs (attention dot + count), your review
    (eye / refresh / check / close-circle), CI dot, and the PR icon colored by state. Hovering the
    group shows the full line, the details panel keeps the full text, and `?` has a legend.
  - On narrow screens the slots drop CI, then review, then author. The title keeps about 120 px.
  - "You" for the viewer's own PRs isn't shown, because the UI doesn't know the viewer's GitHub
    login (only the engine does, via `gh api user`). Adding it needs an RPC.
  - Items with several PRs show icons only for the first one.
  - Linear rows get a state dot plus a status icon, but keep their totals text and don't drop slots.
- **Question cards** (2026-10-20; first slice of Artifacts phase 3 D, rendering only):
  - `markdown/question.rs` `parse_question` turns a top-level `> [!question] Q-n · …` quote into
    `Block::Question`. `markdown_diff` reads ids from it, and its old private parser is removed.
  - Each card shows an id badge, a status pill, a Blocking flag, the question in bold, and Example /
    Blocks / unknown fields / Decision rows. Decision is tinted. Resolved cards are dimmed and
    collapsed, with a Show/Hide toggle, and expand while find is active.
  - Transcripts render question cards too.
  - The HTML `data-zeron-question` form gets CSS plus a frame script (`artifacts/question_html.rs`).
    It's only been checked headlessly, never in a real WKWebView.
  - Still to do for D: the Questions panel, Required Actions for questions, and answering from Zeron.
- **Diff the agent's worktree** (2026-10-21; `m0-contract.md` "Amendment: chat diff targets"):
  - Cofactr chats start at the workspace root, and agents then `git worktree add` into a gitignored
    `.worktrees/<submodule>/<slug>`, so the Changes pane used to show the root's unrelated diff.
  - A new mutable registry row, `chatDiffTargets`, is written by two things: the agent tool
    `bind_chat_worktree {path}` (validated against the chat repository's worktree family), and the
    Changes pane's "Diff checkout…" picker, which suggests ticket matches first.
  - The selected target drives the Working tree, Branch and Latest turn scopes, as well as the member
    pills and the composer branch label. It is display-only and never blocks a turn; execution
    bindings were deliberately not reused.
  - Not done: remote-device chats, PR lookup from a target, and automatic binding without the agent.
    If it is added later, tag auto binds `source: detected` and never take the selection away from
    a manual pick.
- **zeron-agent MCP instructions fit the truncation limit** (2026-10-21):
  - Claude Code cuts MCP server instructions at about 2000 characters. `INSTRUCTIONS` in
    `crates/mcp/src/jsonrpc.rs` was about 2500, so Claude agents never saw the Planning artifacts
    paragraph.
  - It is now 1886 characters, with a test enforcing ≤ 1900 and checking the key phrases. It tells
    agents to call `bind_chat_worktree` right after `git worktree add`.
  - Keep new guidance short and near the top, or put it in the tool descriptions.
- **Work-context strip in the full chat view** (2026-10-23, `chat_work_context{,/strip}.rs`,
  `shell/work_context_ui.rs`):
  - It's one line, expandable. Variants by precedence: PR review item (own item, ancestor, or a
    launcher review chat) > Linear issue (the ticket plus its PRs, or "No PR yet") > Linear project
    or pinned plan (plan title, revision, age, "+N artifacts", or "No plan yet"). No strip otherwise.
  - Launcher review chats are detected by their `review_launcher` chat-PR link
    (`ChatPr::launched_review` in `chat_work_context.rs`).
  - Follow-ups: PR line counts / unresolved threads / conflicts; open-question counts in the
    planning strip; matching the chat's diff-target branch to PR heads; the strip in the overview's
    chat panel.
- **Coordinator resolve fix** (2026-10-23):
  - Bug: coordinator resolves of a child's request silently wrote nothing when the request had a
    `dedupeKey`, which agents always pass. The dedupe group was filtered down to the caller's own
    requests, yet the tool still replied `resolved:true`.
  - Fix: `agent_may_resolve_origin` (the caller's own chat or a direct child) is shared by
    authorization and the group filter. The tool reads the row back and returns `operationConflict`
    if nothing changed.
- **PR page parity with pr-dash** (2026-10-23; `proto/src/pr_dash.rs`, `engine/src/pr_dash.rs`,
  `engine/src/pr_chat_links.rs`, `ui/src/pr_page/`):
  - ⌘P (`OpenMyPrs`) and ⇧⌘P open the PR page. It has pr-dash's two tabs: Authored, and Review
    requested (`review-requested:@me` + `reviewed-by:@me -author:@me`), fetched through the
    `PrDashboard` GraphQL RPC. The 9 authored and 8 incoming bucket rules are ported, with
    needs-you counts and churn, reviewer, CI, thread and conflict fields. The old chat table is
    kept as a third "Review chats" tab.
  - Chat-PR links gained the sources `branch`, `diff_target`, `ticket` and `review_launcher`. URLs
    are compared in canonical form, and every PR URL from one `gh pr create` is linked. The
    launcher reuses an existing chat for the same PR and action. A launcher review chat is
    identified only by its `review_launcher` link.
  - The old overview "My PRs" sidebar is unreachable; deleting it is a follow-up after
    remove-canvas.
  - Not done: the leaderboard tab, the person picker, the churn discount for blank and comment
    lines, the diff bar, and file paging beyond 100 files.
- **Diff a branch or PR without a checkout** (2026-10-23; `m0-contract.md` "Amendment: diff a
  branch or PR without a checkout"):
  - "Diff branch / PR…" sits next to "Diff checkout…". It suggests linked PRs first, then
    ticket-matching branches, and accepts a PR number, URL or branch name as free text.
  - The repo is picked from the PR's owner/repo across the checkout and its submodules. Refs are
    fetched read-only, falling back to `refs/pull/<n>/head`. The base is explicit, else the PR's
    base, else `origin/main`.
  - The diff is `merge-base...head`, via the `range` mode of `GetCheckoutDiff` /
    `GetCheckoutFileDiffText`.
  - Stored as `refTargets` / `selectedRef` on the `chatDiffTargets` row.
  - Not done: auto-refetch, and an unbind control in the UI.
- **Row icon follow-ups** (2026-10-23):
  - A `PrViewer` RPC returns the cached `gh api user` login, so rows and the chat strip show "you"
    for your own PRs.
  - Linear rows are icon clusters (assignee · needs · status) with narrow-width dropping, and their
    totals text moved to the tooltip.
  - `?` has a Plan group and "Open the PR page".
  - Items with several PRs show up to 3 PR icons plus "+N".
- **Canvas view and chat auto-classification removed** (2026-10-23; restore point: tag
  `pre-canvas-removal` at `8bdce21d`; 44 files, −10,869 / +1,278):
  - `OverviewMode` is List / Outline. A saved `viewMode:canvas` loads as Outline.
  - Classification is gone: the RPCs, the classifier, the startup repair and reclassify passes, the
    header category/origin chips and override menu, and Reclassify.
  - List grouping is Repo / Ticket. Expanded List rows lost "launched via" and the tools/skills
    tallies.
  - The generic TypeSafe/Jev client in `typesafe.rs` is kept for reuse.
  - The PR page "Review chats" tab lists chats with a `review_launcher` link when no destination is
    set.
  - Stored classification and position files are left on disk, unread.
- **Forgiving agent tools** (2026-10-23; `agent_tools::normalize_arguments`, `proto/src/agent.rs`):
  - Agents often call zeron-agent tools before their schema loads. These tools now derive an
    idempotency key when it's omitted: `bind_chat_worktree`, `set_status`,
    `associate_current_chat`, `resolve_user_action_request`, and `create_user_action_request`
    when it has a `dedupeKey`.
  - Aliases are accepted: text/title/question → request; id/requestId → userActionRequestId;
    itemId/workItem → workItemId; snake_case → camelCase.
  - Argument errors list the tool's parameters with an example call.
  - Keys stay required for create_chat, create_child, propose_instruction, artifact_publish and
    create_user_action_request without a `dedupeKey`.
- **Review chat replaces Review; My PRs sidebar deleted** (2026-10-23; `ui/src/review_chats.rs`):
  - A PR's review chat is a live chat assigned to its review item, or one linked with source
    `manual` or `review_launcher`. When one exists, the PR page row's primary action is "Open
    review" (title plus live state), and Review moves to the row menu as "Start another review".
  - Enter on a PR row opens the review chat, else starts a review; ↑↓ move the row highlight.
  - The same "being reviewed" marker shows on the outline row and in the chat strip.
  - The overview "My PRs" sidebar code is deleted; old UI-flag keys still load.
  - Authored rows get the same Review / Open review actions, with Fix CI kept when CI fails. When
    the viewer authored the PR (the cached `gh api user` login matches the `gh pr view` author),
    the launcher seeds a self-review prompt; otherwise it keeps the reviewer prompt.
- **Links open in Zeron's side browser** (2026-10-23; Shell `open_web_reference`, pure `reference_open`):
  - With `openWebLinksInZeron` on, every web and PR link (PR page, outline icons, chat strip, chips,
    transcript and artifact links) opens in the side browser. A tab already on that page is focused
    instead of opening a duplicate.
  - The side pane exists only on the Chat route. From the PR page, Overview or Outline, a link
    switches to the PR's review chat, else the selected chat, with the browser beside it; with
    neither, it opens externally.
  - ⌘-click, or "Open in External Browser" in the outline PR menu, always opens externally.
- **Renamed to Creel, user-facing** (2026-10-23; decisions under Parked → "Rename the Zeron fork"):
  - Name: about 190 visible strings, both plists ("Creel", `creel.icns`), packaging (`Creel.app`;
    the dev script builds `Creel Dev.app`), user docs and the agent instructions' prose. The guard
    test `crates/ui/tests/brand_name.rs` fails if a user-facing "Zeron" comes back.
  - Logo: `dist/brand/` (generator, SVGs, PNGs) and `dist/macos/creel.icns`, with a simplified mark
    at 16 and 32 px. The in-app logo is `icons::logo(appearance)`: steel on light, moss on dark.
  - Left as Zeron on purpose: release file names (the update manifest matches on them), the
    stored-instruction preamble (hashed), and every identifier, environment variable, data path,
    bundle id, keychain entry, `zeron-artifact:` link and `zeron-agent` tool.
  - The wordmark is live Copperplate text; outlining it waits on a licence check.
- **Renamed to Mast** (2026-10-26; Mikey disliked Creel because it sounds like "creole"):
  - Name: about 330 more strings, both plists ("Mast", `mast.icns`), packaging (`Mast.app`, dev
    script `Mast Dev.app`), docs and READMEs. `brand_name.rs` now fails on a user-facing "Creel" too.
    The same identifiers as above stay "zeron".
  - Mark: a masthead light on a pole with two shrouds, a crow's-nest ring and three agent nodes.
    The light is a thin-rim glass lens with a moss → clover → cobalt gradient; the app icon has a
    metal finish (silicon, magnesium, fir, moss) on a steel tile. Only Cofactr brand colours.
    Generator: `dist/brand/generate.py` then `build-assets.sh`; the in-app marks are
    `crates/ui/assets/icons/mast-logo-{light,dark}.svg`, rendered in colour through `img()`.
  - `dist/brand/status/` holds the five lamp colours (idle, working, ready, input, error). Exploration pages and generators are in the
    Zeron Dev item's artifacts (`sources/mast-final/`).
- **Status light** (2026-10-26; `crates/ui/src/status_light.rs`):
  - The masthead light shows the most urgent state across chats: error (cadmium) > needs you
    (bromine; an awaiting-input chat or an open Required Action other than "Ready to merge") >
    ready (cobalt; an open "Ready to merge") > working (clover) > idle (clear glass). The pure
    `app_light` picks it; `Shell::sync_status_light` runs on state changes and the 1 s ticker.
  - Dock: icon swapped via `applicationIconImage` (embedded 512 px PNGs in
    `crates/ui/assets/dock/`) and a badge counting awaiting + errored chats + open Required
    Actions; debounced about 1 s, only changed parts touch AppKit. In-app logos use
    `Shell::logo`, with ten per-state marks generated by `dist/brand/generate.py`.
  - Settings → Notifications → "Live app icon" (default on); off restores the bundle icon, clears
    the badge and shows the plain logo. No pulse while working (a repeating animation redraws
    every frame).
- **PR chats start through the Start Chat dialog** (2026-10-27): every "start a review/fix-CI
  chat" entry point (outline ⌘↩ / Start Chat, context menu, Details, GitHub pane, PR page, URL box,
  palette) calls `Shell::open_start_pr_review`, which opens the Start Chat dialog in PR mode (PR,
  prompt opening, link, root, model/reasoning/approval pickers). Start sends `StartPrReview` with
  the picked config and `fresh: true` (new optional param; without it the engine reuses a live
  review chat and ignores the pick). "Open review" stays a direct open.
- **Every chat start passes an explicit model; the default is Opus 5.5** (2026-10-27): the
  built-in Claude Code default is `zeron_proto::DEFAULT_CLAUDE_MODEL` = `claude-opus-5-5` (added to
  the curated catalog; the `opus` alias labels as Opus 5.5; the picker prefers it over the live
  catalog's first row, which is the CLI default). MCP `create_chat` without `model`, work-item
  Start Chat/continue with no model, the PR review default config and `Pickers::resolved` without a
  catalog all fill it in, and the claude command builder always emits `--model` (empty → default,
  debug log). Most likely cause of the 2026-10-26 Fable chats: agents calling `create_chat`
  without `model`. Saved per-chat/remembered picks are untouched. Codex has no built-in default.
  Mikey switched the CLI default in `~/.claude/settings.json` to Opus 5.5 the same day.
- **Chats show the model they actually ran on** (2026-10-27): new `Chat.actual_model`
  (serde-default, no migration), stamped from Claude Code's init event (`SessionStarted.model`); a
  boot-time pass (`SessionsEngine::backfill_actual_models`) fills it read-only from each chat's run
  journal. `Chat::model_display()` (proto) drives the header, card/outline badge and tooltip
  ("configured X" when different, "default" when unknown). The composer chip on an existing chat
  reads "Next: <model>" when that is only the next turn's pick. MCP `get_chat`/`list_chats`: `model`
  is the actual model (else configured, else null) plus `modelIsActual`, `actualModel`,
  `configuredModel`. Claude Code only; Codex/ACP/OpenCode just echo the request, Cursor untouched.
- **Outline row author/assignee slots hug their text** (2026-10-27): `row_icons::text_slot` sizes
  the PR author / Linear assignee slot to its text (max 58px) and omits it when empty, so synced
  Linear rows no longer show a ~40px dead gap before "you". Icon slots are unchanged. (That column
  held the Linear identifier/state chip until the compact-icon change `528856b1`.)
- **Ticket ID in Linear rows' ticket link** (2026-10-27): the Linear status element (the one hit
  target that opens the ticket) shows the identifier (e.g. `ENG-3190`, 10px mono, `text_faint`)
  before the state dot and icon; one hover, tooltip and click. Hugs its content; empty identifier
  adds no width; nested rows too. Narrow rows drop the dot, then the assignee, then the ID.
- **PR page and Linear lists cache and revalidate** (2026-10-27): `engine/src/swr.rs` `SwrStore`
  (last good value per key, wall-clock freshness, one shared in-flight fetch per key, force
  bypass, optional persisted JSON with atomic writes off the async worker). The PR page persists to
  `<data_dir>/cache/pr-dash.json`; `PrDashboard` has `cachedOnly` (stored rows, never fetches), so a
  tab with no rows shows the stored copy at once, then fetches. Linear my-issues/my-projects/
  project-issues lists use the same store, and the Linear panel no longer resets to Loading on
  every open. A spinner shows only when nothing is cached; explicit refresh bypasses freshness.
- **Optimised install build** (2026-10-27): `[profile.installed]` in Cargo.toml (inherits release,
  incremental, codegen-units 256, no LTO/strip, line-tables-only debug). Clean build ~170s,
  rebuild after one UI file ~8s, binary 150 MB (debug was 286 MB), `target/installed` ~8 GB. Before
  this the installed app was a debug build with workspace crates at opt-level 0, likely the main
  cause of sluggishness. `debug_assert!` and overflow checks are off in it. Also: Overview `rows()`
  returns `Rc<Vec<OverviewRow>>` so hover/scroll frames no longer deep-clone every row.
  Outline rebuilds (2026-10-27, `perf-outline-rebuilds`): `AppState::overview_fingerprint` gates the
  Overview's state observer (rebuild + notify only when a row-relevant field changes);
  `WorkItemIndex` rebuilt only when a snapshot Arc is replaced; per-chat `row_fetch_due` stops
  `ensure_row_fetches` re-checking settled chats. 250 chats × 50 streaming updates: rebuilds and
  repaints 50 → 1, fetch checks 12500 → 0 (`OverviewPerf` counters). Still open: the other 15
  `AppState` observers (shell, transcript, composer, changes, terminal, files, history, pickers need
  broad state; chat_type_view, repository_topology, context_usage, artifacts tab and settings pages
  not audited; `chat_type_view` clones all chats every render).
  Model discovery (2026-10-27, `perf-model-discovery-cache`): harness `Catalog` success TTL is 30
  min (`CATALOG_TTL`, keyed on binary path/version/mtime/credentials); claude `InitializeCache`
  reused 30 min, a forced refresh reuses one from the last 120s, then re-probes.
  `backfill_actual_models` writes a `<chat>.modelchecked` sidecar (journal byte length) when no
  model is found and skips the chat until the journal grows.
- **Artifact zoom** (2026-10-27): `artifacts/zoom.rs` (25–800%, 10% key steps, continuous pinch
  and ⌘/Ctrl-scroll anchored at the pointer, per-artifact session memory, `ZoomSurface`). Keys ⌘= ⌘+
  ⌘− ⌘0 in the `ArtifactTab` context (registered from `find_view::bind_keys`; ⌘0 shadows "jump to
  session 10" only while an artifact has focus; keys propagate on Options/Source/Compare). Toolbar
  "− 100% +" (dim at 100%, click % to reset). Markdown: native rem-scaled text via gpui `PinchEvent`
  (reflows, vertical pan only). HTML/Mermaid: CSS zoom in the WKWebView frame script, page handles
  pinch (WebKit gesture events) and ⌘-wheel; unverified in a real WKWebView. Images: fit × zoom in a
  two-axis scroll. Not done: compare-pane sync, image drag-pan, Markdown px paddings.
- **Sandbox is enforced** (2026-10-28): Claude Code `read-only` passes `--disallowedTools
  Edit,Write,MultiEdit,NotebookEdit,Bash,PowerShell` + `--permission-mode default` and never
  bypasses permissions, even with auto-approve (so read-only Claude chats can't run git or tests).
  Claude `workspace-write` is NOT enforced (the CLI can't confine writes to cwd; unchanged).
  Codex `read-only` stays read-only with auto-approve forced off (it used to widen to full access).
  ACP harnesses (opencode, cursor, generic) refuse a `read-only` start with `HarnessError::Sandbox`.
  "No wider than the parent" holds in MCP `create_chat` and the work-item launch.
- **AppState observers gated** (2026-10-28, `perf-remaining-observers`): shell, composer, pickers,
  chat_type_view (rows cached in an Rc), repository_topology, context_usage, artifacts tab and the
  settings pages only notify when the slice they render changes (fingerprints like
  `overview_fingerprint`; counters in `shell/perf_tests.rs`). 250 chats × 50 updates: composer and
  pickers 50 → 0, shell 50 → 0–1; transcript still follows selected-chat streaming.
- **Artifacts phase 3** (2026-10-28): D: plan questions become Required Actions on the owning item
  (`engine/src/artifact_questions.rs`, reconcile every 2s on the owning device). Blocking = own action
  (key `artq:<artifactId>:<Q-n>`); the rest roll up into "N open questions in <plan>"
  (`artq:<artifactId>:rollup`; a changed list resolves the old one as "Superseded"). Answers live on
  the artifact row (`ArtifactView.answers`, `set_artifact_answers`); the plan file is never edited.
  Card answer → `ArtifactAnswerQuestion` RPC (messages the publishing chat, resolves the action);
  resolving the action marks the card answered. `artifact_get_plan` returns `answeredQuestions`.
  C: "Updated since you last viewed · rev N → M" chip opens the compare view; drift badge (commits on
  the item's chats' branches since the latest revision, or unreflected answers). Not done: answering
  from HTML cards, marker on artifact lists, multi-device answers are last-writer-wins. See the
  phase-3 amendment in `docs/internal/plans/artifacts-contract.md`.
- **Compare and zoom gaps** (2026-10-28): Markdown compare panes sync on scrollbar drag, paging and
  `scroll_to` (each pane reports position on paint; follower moves aren't echoed). Compare view is
  zoomable and panes zoom together (HTML/Mermaid frames mirror via SYNC scripts and
  `__zeronFrameCmd`). Zoomed images click-drag pan with a grab cursor and edge clamping. Markdown
  block gaps, list/quote/callout/question-card and preview padding scale with zoom.
- **Artifacts phase 5** (2026-10-28): F: "Side by side" on options artifacts shows 2–4 options in
  equal columns of one web doc (scroll + zoom synced across frames; >4 options pick which 4 via
  chips). Per-column "Pick this" / "Change pick to this" / "Picked · withdraw" with an optional
  note: stored as `ArtifactView.picked` (`ArtifactPickOption` RPC, files never edited), messages
  the publishing chat, resolves related Required Actions (`artopt:<artifactId>`, or the matching
  plan question), and `artifact_get_plan` returns `pickedOptions`. H: an item with a linked Linear
  project can mark one plan canonical; it shows beside the project's details (from the SWR-cached
  Linear data). Nothing is written to Linear.
- **Artifacts phase 4** (2026-10-28): frame → host bridge (frame postMessage → outer doc hidden
  `zeron-msg:JSON` link → WKWebView delegate → `BrowserEvent::DocumentMessage` →
  `ArtifactTab::on_frame_message`, validated in `artifacts/bridge.rs`; user-activated only, 16 KiB
  cap). HTML question cards are answerable in place (phase 3 path). Comments on HTML + Mermaid
  stored on the row (`ArtifactView.comments`, ≤40); anchors id/`data-zeron-anchor` → mermaid node
  → text-quote → CSS path; unfound = Outdated (`proto::resolve_anchor`). UI: "Comment on
  selection", "Pick element", "Comments N" panel, page markers. Unverified in a real WKWebView.
- **Compact single-chat rows** (2026-10-28): an item with exactly one live chat is one outline row
  (`ItemRow.compact_chat`, `OutlineOptions.compact_single_chat`); the chat's key aliases to the item
  row. Splits into item + chat rows on a second chat, a child item, a nested sub-chat, an archived
  chat (with show-archived) or a dangling chat.
  ⌘I opens Work Item Details for the selected row (compact or not); on compact rows the status
  glyph is clickable and opens details too (2026-10-28, `compact-row-details`).
- **Repository map removed** (2026-10-28): the UI (`repository_topology.rs`, route, button, ⇧⌘G),
  proto types and the `GetRepositoryTopology` RPC are gone; the shared engine discovery code lives
  on as `repository_discovery.rs`.
- **Linked Linear item** (2026-10-28): per-Space setting `linearAssignedWorkItemId`
  (`setLinearAssignedDestination`, needs Linear on). `engine/src/linear/assigned.rs` runs inside
  the Linear sync pass (15 min loop, manual Refresh, and when the destination is set) with the
  read-only query `ZeronLinearAssignedUnstarted` (viewer.assignedIssues, state backlog/unstarted/
  triage, project null) through the SWR store. Children "ENG-123 · title" are created via
  `createWorkItemFromLinear` with derived ids; tickets already linked elsewhere aren't duplicated.
  Tickets that stop matching: touched children get a marker, untouched ones are archived. System
  actor only, never on the undo stack; nothing is written to Linear.
- **Outline undo** (2026-10-28): `work_outline/action_log.rs` (in-memory per Space, cap 200, UI call
  sites only, so agent/remote/sync changes are never recorded) and `outline_view/undo.rs`. ⌘Z / ⇧⌘Z
  (and Ctrl-Z / Ctrl-Shift-Z) in the outline nav context; text fields keep their own ⌘Z. Steps go
  through the normal engine ops; a probe refuses when the target changed ("Can't undo … Nothing was
  changed"). Undo / Redo / Recent Changes… in the item and Space-root menus. Not persisted.
- **Small gaps** (2026-10-28): Markdown table/code padding, header and radius scale with zoom.
  Plan-question reconcile is event-driven (boot; outline kinds change, debounced 50 ms and gated by
  the kinds' generation marker; immediate after an answer/pick) with a 5-min safety sweep.
- **Web view ↔ composer focus** (2026-10-28, `browser/macos.rs`, `browser/mod.rs`): AppKit used to
  leave the WKWebView first responder after a click into the page or a forwarded shortcut, so keys
  (e.g. space) went to the page while GPUI showed the composer focused; page loads also stole GPUI
  focus. Now: a mouse-down outside the page restores GPUI's view as first responder (NSEvent local
  monitor, popover-aware); GPUI focus leaving the browser surface clears native focus; bare Esc in a
  page text field returns focus to the app; loads only take focus when it's already in the browser
  or nowhere (`may_take_focus`); plain keys are never forwarded. Still forwarded from page fields:
  ⌥↑/⌥↓ and Esc with the find bar open. HTML question-card typing: no blocker found, verify by hand.
- **Resizable panes** (2026-10-28): the chat-view sidebar (already draggable, `settings.sidebar_width`)
  now ranges 200–520 px and its shown width is clamped so the conversation keeps `CHAT_PANEL_MIN`.
  The Work Item Details / Required Actions panel has a left-edge drag handle (double-click resets to
  380), limits 320 px–60% of the Overview with the body ≥360, persisted as `detailsPanelWidth:N` in
  the Overview UI-flags file; drag only notifies the Overview.
- **Per-project agent accounts** (2026-10-28; plan zeron-artifact:05d0f52c4d8ddaae45242f8a3aaed976 rev 3,
  scope per project only): managed homes under `<data>/managed-accounts/<harness>/<id>/` (0700;
  device-local `store.json`). A project has a default account per harness; a chat resolves it at
  its first run and stays pinned (chats with an existing harness session pin to Default). Spawns,
  resume and title runs set `CLAUDE_CONFIG_DIR` / `CODEX_HOME` for the pinned home; Default sets
  neither and never touches ~/.claude / ~/.codex. Claude Code 2.1.285 keys Keychain as
  `Claude Code-credentials-<sha256(CLAUDE_CONFIG_DIR)[..8]>`, so homes are isolated (fallback switch
  `account_home::CLAUDE_KEYCHAIN_ISOLATED`). Sign-in runs `claude auth login` / `codex login` in the
  home. Skills/rules shared into homes (no credentials). UI: project row menu picker with Add
  account, chat account badge, Settings → Project accounts. Not done: usage rings and model picker
  per managed account, context-usage reader for managed homes, MCP/plugin sharing, Start Chat
  account chip, promoting old slots, API-key accounts. Needs Mikey's hand check: real sign-in and
  two accounts running at once.
  Sign-in and saved accounts (2026-10-29, `accounts-signin-and-saved`): `claude auth login` /
  `codex login` run on a PTY; the Add account sheet shows the URL (Open/Copy), any device code, and a
  "Paste the code" field (`SubmitManagedAccountLoginCode`; code goes to stdin only, never stored or
  logged), progress and Cancel. Saved Settings logins appear in the project picker and are promoted
  (`PromoteSavedAccount`) into a new managed home from Mast's own slot file: the live Default login is
  refused (use Add account); a promoted slot is moved (leaves the global switcher, Mast stops
  refreshing it) so two homes never share a rotating refresh token.
  Round 2 (2026-10-29, `accounts-round-2`): usage windows per managed account (same usage endpoints
  with the home's own token; never refreshes; 60 s shared cache) shown in Settings → Project
  accounts; context usage reads the pinned home (`CLAUDE_CONFIG_DIR/projects`, `CODEX_HOME/sessions`);
  Start Chat account chip with a one-chat override (hidden when the device has no managed account).
  Web-view ⌥↑/⌥↓ and Esc-with-find-bar no longer forwarded from page text fields. Open decision for
  Mikey: MCP server sharing needs reading the MCP section of `~/.claude.json` (holds identity data,
  never read today); plugins would need a copy-based share (symlink writes through to ~/.claude).
- **Internal rename zeron → mast** (2026-10-28, `rename-zeron-to-mast`): crates are `mast-*` (`mast-ui`,
  `mast-engine`, `mast-harness`, `mast-mcp`, `mast-proto`, …), the app package is `mast` (`apps/mast`)
  but the binary is still `zeron`, so the install command is `cargo build --profile installed -p mast`
  and the output stays `target/installed/zeron`. Runtime names write the new form and accept the old:
  `MAST_*` env (`ZERON_*` still read via `mast_proto::env`), MCP server `mast-agent` (`zeron-agent`
  alias), `mast-artifact:` / `mast-file:` / `mast-msg:` / `mast-link:` schemes, `data-mast-*`,
  `.mast/` artifacts folders (existing `.zeron/` folders still used), `mast.json` (`zeron.json`
  fallback), `mast/<name>` worktree branches (`zeron/` still recognised). Not renamed (see
  `docs/internal/plans/rename-migration.md`): `~/.zeron`, bundle id `sh.zeron.app.dev`, Keychain names, the
  `zeron` binary, the `zeron/` submodule folder. `scripts/rename-map.sh` maps upstream code onto the
  new names. New git spawns use `mast_harness::git_command()`.
- **Chat header revamp** (2026-10-28, option C + B's card on hover): `chat_item.rs` fact sheet feeds
  three surfaces. Header: clickable trimmed breadcrumb, title, chips (model actual/configured,
  account, branch, PR, ticket), ⚠ / ● badges opening the Item tab. Sticky "Item" tab (first in the
  right pane, not closable): path, instructions (Edit, Compare/Apply, proposals), Runs in, Model,
  Account, Links, Needs you, Plan, Children; no-item chats get Runs in, Model, Account, "Add to
  outline". Hover card: ~400 ms show, 160 ms grace, ⌥⌘I pins; never takes focus. ⌥⌘; opens the
  Item tab. Removed: the work-context strip, drift and proposal lines, and the header's workspace /
  chat-id / session-id chips. Follow-up (`header-followups`, 2026-10-29): an "Identifiers" section at
  the bottom of the Item tab (chat id, session id, workspace, each with Copy); `open_web_reference`
  tests rewritten in `chat_item_tests.rs::web_references`; visual polish to the mockup (semibold title
  under the breadcrumb, icons and tones on chips, tighter Item tab sections, condensed hover card).
- **Perf baseline** (2026-10-29, `perf-baseline`; report mast-artifact:65d48729c91ee81b655f90b6adda143b,
  commands in `docs/perf-baseline.md`): `#[ignore]` benches for transcript/markdown/diff/overview
  render, engine startup, SWR cache, journal scans and git spawn (run with `--profile installed`; some
  use a read-only copy of the real profile with a throwaway HOME). Fixes: transcript rail ticks cached
  (frame 7.8 → 0.5 ms, streaming tick 17.7 → 3.2 ms at 2000 messages); registry rows read in place and
  only changed kinds published (heartbeat 2.14 → 0.13 ms); journal backwards tail scans
  (`stale_sessions` 440–635 → 5–7 ms); boot recovery reads journal tails (`EngineCore::assemble`
  ~940 → ~350 ms). Running app under load: ~61% of a core mean (engine, not UI thread), RSS swings
  1.6–6 GB. Open findings (ranked): Overview List mode not virtualized (44–55 ms/frame, 335 MB alloc at
  250 chats); Work Outline recompute on every chat/session write (33–36 ms); diff-sync runs 7 git
  processes per kick, every 120 s for 22 repos; transcript sync O(entries) per tick; artifact question
  sync re-sent an op id with a changed payload (fixed in round 2); RSS swings unexplained.
  Round 2 (2026-10-29, `perf-round-2`): question-sync op ids derive from the full payload (origin, text,
  info, key), fixing the `operationConflict` writes; Work Outline recompute gated by a watch marker
  (outline-kind generation + per-Space chat/session fingerprint): ~46 ms per heartbeat → 0.7 ms check
  (`bench_work_outline_recompute_per_heartbeat`); diff-sync full capture 7 → 5 git processes, repair
  tick runs one `git status --porcelain=v2 --branch` probe and skips unchanged checkouts, archived-only
  checkouts aren't kicked. Still open: Overview List mode virtualization, transcript sync O(entries).
- **Changed-since marker on artifact lists** (2026-10-29): accent dot + "Updated since you last viewed:
  rev N → M" tooltip on Details panel artifact rows, the Item tab Plan row and the outline row's
  artifact chip, from `settings.artifact_last_viewed` (`questions_ui::list_marker`).
- **Docs for coworkers** (2026-10-29, `docs-onboarding`, docs only): README rewritten (what/why, install,
  first run, keys, docs links, credits), README.zh-CN.md removed, guides in `docs/guide/`
  (getting-started, work-outline, orchestrating, plans-and-artifacts, github-and-linear, accounts,
  keyboard) with "Try it" walkthroughs. Image/recording TODOs for Mikey are listed in the guides. The
  repo-cleanup proposal (Part B) awaits Mikey's approval: mast-artifact:bacfa5c96fe27d73a2821e8f4e2aa446.
- **Side by side fits designs** (2026-10-29, `side-by-side-fit`): each option frame lays out at its
  design width (`designWidth` on the manifest entry, `data-mast-design-width`, manifest-wide, default
  1100, clamped 320–4000) and is scaled to the column with a CSS transform (never above 1x). Toolbar:
  2/3/4 up, per-column Widen/Narrow (2.2x share), Fit / 100% (real size, horizontal scroll); remembered
  per artifact (`artifactSideLayouts`). `crates/ui/src/artifacts/side_layout.rs`; conventions doc §6
  and the artifact_publish description tell design agents to set the width.
- **Item tab A1 + Team tab** (2026-10-29, `item-tab-team`; design mast-artifact:b30aa528f035e921c286d66a56eb7606):
  Needs-you block at the top of the Item tab (Required Actions for the item + descendants) grouped
  Blocking → Ready to merge → FYI, oldest first, 2-line clamp with Details, inline Reply & resolve /
  Resolve / Open / Approve; same grouping in the Details panel and the global Required Actions panel.
  Other Item tab sections are one-line accordion rows. Team tab (`team.rs`, `shell/team_ui.rs`) appears
  only for chats with workers (child chats or chats on descendant items): per-worker state, last
  activity, PR/ticket chips with status, diff stats and Ready-to-merge jump, with a roll-up line; the
  Item tab shows the roll-up and top workers.
  PR rows in Item tab Links and the hover card (2026-10-30, `item-tab-pr-status`): state-coloured icon,
  truncated title (tooltip with repo#n, state, checks, review), Open/Draft/Merged/Closed label, checks dot,
  review decision, from the GithubStore cache (uncached → "PR #N" until `request_status` lands). Tickets
  show "ID title" plus Linear state when known. Team tab PR chips are a separate component (unchanged).
- **Project templates, increment 1** (2026-10-29, `project-templates`; design option A
  mast-artifact:d32bb551266cd62ceef33f16477b6c1a): Group into… (⌘/⇧-click multi-select item rows,
  right-click or ⌘⇧G; new container or existing item; one undo step). Per-item `isProject` and
  `workerDestinationId` (Details → Project section; users only). `work_outline_create_child` with no
  parent resolves to the nearest project's destination; `create_chat { workItemId = project }` starts
  in it; project scope (contract amendment in m0-contract.md) lets a chat on a project's Planning item
  create into its sibling Tickets container but nothing outside the project. Gap: a bare `create_chat`
  without `workItemId` still inherits the caller's item (fixed in increment 2).
  Increment 2 (2026-10-30, `project-templates-2`): templates copy ANY item subtree (titles,
  descriptions, instructions on every node, project flag, worker destination; not chats, Required
  Actions, external links, archived items or tickets under a destination). Save as template… on any
  item (Update template = rev+1, or Save as new); Space-root details → Templates (list, tree preview
  with per-node instructions, New from template…, Delete). New from template… creates exactly that
  tree under a chosen parent with a chosen root title (item menu, Space-root menu, Templates list).
  Template-changed review for copies; Convert to project maps an existing item onto a chosen
  template. A bare `create_chat` (no `workItemId`) from a chat inside a project with a worker
  destination creates a new item there and starts the worker on it.
- **Running-chats popover** (2026-10-29, `running-chats-popover`; design A revised
  mast-artifact:f5bdd9f1c79754065ba039d4e8b0d62f): titlebar lamp button (next to +) or ⌥⌘R opens a
  popover: Needs you first, then by Space, children under coordinators; rows show title, last message
  preview, elapsed, item path, account, model. Keys ↑↓, ↩ open, ⌘↩ open in Overview side pane, I
  interrupt, →/← fold, Esc. Pending build: the launch executable's inode/mtime/length is re-checked
  against the installed bundle every 5 s; a newer one rings the lamp and shows "New build installed ·
  N changes" (sha embedded by `apps/mast/build.rs`, so the list starts from the next install) with
  Restart when idle / Restart now… / Restart. No restart UI without a newer build. Interrupted ledger
  in the engine (list/dismiss/resume RPCs): "Restarted HH:MM · N interrupted" with Resume, Dismiss,
  Resume all. "Idle + background process" is rendered but the engine doesn't track background
  processes yet.
  Titlebar layout fix (`fix-titlebar-lamp-overlap`): `TITLEBAR_TRAILING_SLOTS_WIDTH` always reserves the
  + and lamp slots, so titles never start under the lamp and the lamp's position is stable on every route.
- **Instruction proposals edit/remove** (2026-10-30, `instruction-proposal-edits`): proposals carry
  `action` add | edit | remove, `instructionId` and an optional `rationale`; edit/remove record the base
  text + updatedAt. Accepting an edit rewrites the same instruction (history/drift intact); a remove
  archives it; stale proposals are refused unless Apply anyway (`allowStale`). Agents may only target
  instructions in their item's effective set. Details → Proposed instructions shows a word diff
  (`proposal_diff.rs`), struck-through removes, the rationale, Accept / Edit then accept / Dismiss.
- **Plan question typing** (2026-10-30, `fix-plan-question-typing`): the Markdown preview root focused
  itself on mouse-down after the answer input did, stealing focus; the input wrapper now stops
  mouse-down (`markdown/render.rs`). HTML question cards: no blocker found; debug logging on target
  `focus` (web view mouse-down in_page/hit view, key routing, focus hand-backs) for a hand repro.
- **Menu bar item** (2026-10-30, `menu-bar-item`; design A + C's mini bars,
  mast-artifact:4b4633204f14e4dc1e6d8921620ac805): `crates/ui/src/menu_bar.rs` (pure menu model:
  build_menu, usage_accounts, format_reset, lamp_rgba, near-limit rule; thin objc glue: NSStatusItem +
  NSMenu, `MastMenuBarTarget`) and `shell/menu_bar_ui.rs`. Glyph: lamp in the state colour (idle
  silicon, working clover, ready cobalt, needs-you bromine, error cadmium), fir ring for a newer build,
  plus one mini bar per account (≤4, height = 5h %, bromine ≥80%, cadmium ≥95%). Menu: per-account
  usage cards with reset times, chats (Needs you first), interrupted + Resume all, Restart when idle
  (newer build only), Open Mast, Quit. Uses the existing usage cache; AppKit glue is compile-checked
  only (hand-verify). Also supersedes the retired peon tray.
  Usage fetch fix (2026-11-02, `menu-bar-usage-fetch`): `UsageSchedule` probes at startup (once the
  engine answers), on menu open (if ≥60 s old) and every 5 min; failures retry after 60 s. Rows are
  enabled (AppKit dims disabled items) with label/secondaryLabel colours and solid bars. Cards say why
  usage is missing (token expired, not signed in, sign-in not readable, API key); debug log target `usage`.
- **Orchestrator wakes** (2026-11-02, `orchestrator-wakes`): engine `orchestrator_wakes.rs`, event-driven
  (outline snapshot Required Actions raised/resolved, session watch errored/turn finished, PR linked).
  Workers = child chats + chats under the project worker destination. Events batch over
  `WAKE_WINDOW_MS` (45 s) into one queued system "Worker updates:" message, delivered only when the
  orchestrator is idle; held while Mikey has a draft, was active in the last 2 min, or has a queued
  message; a user send during a wake turn interrupts it and runs next. Defaults: on, Required Actions +
  errors; turn-finished and PR events off. Settings in `<data_dir>/orchestrator-wakes.json`. UI: Team
  tab header (toggles, "N updates held" + Deliver now, unseen marks), Item tab Wakes row. Gaps: PR
  merged not fed yet, held events in memory only, only the orchestrator's host device delivers.
- **Release 2026-11-02 evening** (installed 18:20; tests confirmed later): `worker-report-to-parent`
  (a chat may `send_message` its parent or call `report_to_parent`; reports go through the wake queue as
  a "worker report" event, ≤4 KB, on by default, Team tab activity line), `wait-cap-and-link-inference`
  (`wait_for_turn` / `send_message {wait}` stop at 1500 s with `stillRunning`; replies capped with
  `truncated` + paging; stricter PR/ticket "mentioned" inference), `relay-user-answer`
  (`answer_user_action_request { requestId, userMessageId }` delivers the user's own message as a
  user-attributed Reply & resolve after checks: role=user, human-authored, newer than the request, in
  reach; project-scope `read_chat`/resolve via `Reach::ReadScope`), and `container-templates` phase 1
  (template rules in a container's Details; New work item under the container materializes the
  structure with live instructions, add + mute; moved-in items marked "doesn't follow template" with
  Apply; `work_outline_apply_container_template`; plan mast-artifact:df5be7ef1a2fa657e3737f17dd0ef89a lists
  the remaining phases). Merge conflicts in crates/mcp/src/tools.rs resolved by hand keeping both sides.
- **Admin chats** (2026-11-02, `admin-chats`, installed 21:10): right-click a chat (outline/sidebar) or
  the Item tab → "Grant admin for this Space…" (1 / 4 (default) / 8 / 24 h or until revoked; optional
  "Skip bulk confirmations", off by default). Badge "Admin · <Space> · Nh left" + Revoke in the header
  and Item tab; shield tag in the running popover and menu bar. Grants are re-checked per call and per
  batch step and only cover the granted Space: list/get/read/send/wait/archive any chat, create_chat,
  read_subtree and answer_user_action_request on any item, and `work_outline_admin` batches (incl.
  container-template set/apply/rule edits). Attribution "admin chat … (granted by Mikey)"; sent
  messages say they are not Mikey's own. Bulk batches raise a preview Required Action unless skipped;
  per-turn undo from Recent Changes.
- **Template-aware instruction proposals** (2026-11-05, `template-instruction-proposals`, Terra): edit/remove proposals on
  a template-derived instruction (`templateInstructionId`) now target the container's rule instruction; approving
  goes through `setContainerTemplate` and propagates to every following child. New `childTemplate` add target (rule
  id or title path; defaults to the caller's container/rule node). Review card: "Child template · applies to N items"
  with Apply to template / This project only (adds an own instruction). Guard: approval can no longer write a
  template-linked row directly (it used to, and the next template save silently overwrote it). Release run: nextest
  2388/2388, UI 2489/0.
- **Rename for distribution** (2026-11-05, `rename-for-distribution`, Terra; decisions: bundle id
  computer.mikey.mast / .dev, `~/.mast` for new installs, keep `~/.zeron`): binary `mast` with a `zeron` shim bin;
  updater and `daemon install/status` accept both; `CFBundleExecutable mast`; both `mast://` and `zeron://` declared,
  `mast://` generated; one-time copy of the old `sh.zeron.app[.dev]` UserDefaults domain; data dir: `MAST_DATA_DIR`/
  `ZERON_DATA_DIR`, else `~/.zeron` if it exists and `~/.mast` doesn't, else `~/.mast`; Linear Keychain service
  `computer.mikey.mast.linear` with read fallback to `sh.zeron.linear` (copied forward, old kept);
  `scripts/install-macos-app.sh`; docs/guide upgrade note. Wire names unchanged. First restart re-asks macOS
  permissions and the Linear Keychain item. Release run (`--all`): nextest 2434/2434, UI 2488/0.
- **Distinct status indicators** (2026-11-05, `status-indicator-distinct`, Terra): one accent-independent state style
  (colour + glyph) shared by Team, running chats, sidebar, outline, item details, Overview, review chats, menu rows
  and Needs-you markers: Working clover/juniper spinner, Waiting bromine "!" badge, Errored cadmium ✕, Ready cobalt →,
  Done fir ✓, Idle silicon/steel ○. Previously Working used the user's accent and could match Waiting's amber. Light
  fir on the sidebar is ~2.95:1 (glyph + label carry it). Release run passed, UI 2488/0.
- **Start Chat sign-in preflight** (2026-11-05, `start-chat-signin-preflight`, Terra): read-only cached (≈30 s) local
  sign-in detection (the doctor detectors) → `signed_in | signed_out | unknown`; the new-chat composer and work-item
  Start Chat show a nonblocking "isn't signed in" notice with Sign in… (Default → Settings → Agents; managed → Space
  accounts, using the signed-out flag) and Start anyway; unknown shows nothing. Release run: nextest 1274/1274, UI
  2485/0.
- **Stale coordinator wakes** (2026-11-05, `stale-coordinator-wakes`, Terra): worker events go to one eligible
  coordinator: a live explicit parent wins; otherwise one non-archived, wake-enabled chat on the project item (latest
  hand-off target or most recently active), never every chat on the item. Archived and wake-disabled coordinators get
  nothing (auto-continue and durable restore included); sending to an archived coordinator unarchives it. Known
  follow-up: a cross-device archive→unarchive race can let one held wake through. Release run: nextest 1242/1242,
  UI 2481/0.
- **Talk/decision metrics** (2026-11-05, `talk-metrics`, Terra; plan rev 3 phase 0): `tools/mast-metrics` reads an
  immutable copy of the profile (sqlite `.backup` of docs.sqlite3 + copied journals, never the live store) and writes
  aggregate JSON/Markdown with `--since/--until` and `--compare <old.json>`. Snapshots and reports live in
  `~/mast-metrics/` (never committed); re-run steps in tools/mast-metrics/README.md. Baseline (14 days to
  2026-11-05): 406 cards vs 872 heuristic prose questions; card replies median 9.4 h / p90 136 h (n=33); coordinator
  replies median 171 words; 72 status checks. Summary mast-artifact:49d61202530e739e159d03b99bb0c7c8. Re-run ~2026-11-12.
- **`zeron doctor`** (2026-11-05, `mast-doctor`, Terra; merged after Mikey's go-ahead): read-only setup checklist
  (`zeron doctor`, `--json`): agent CLIs (login-shell PATH, exact override paths, rustup shims kept), local agent
  sign-in, gh + auth, git/LFS, data dir access/size/free space, engine lock + IPC, Rust toolchain, Linear Keychain
  presence. ✓/!/✗ with a fix per line; errors exit 1, warnings 0; missing both Claude and Codex is an error. README
  and getting-started mention it (conflict with the terminology pass resolved). Release run: nextest 2411/2411, UI
  2481/0. A shorter `mast` command belongs to the binary rename.
- **Perf sweep 5: startup main-thread ticks** (2026-11-05 overnight, `perf-sweep-5`, Terra; gated instrumentation
  only): per-view startup spans name WatchSpaces and AuthStatus updates as the first two long main-thread ticks; GPUI
  test-support draws dirty windows synchronously during effect flush (inflates headless numbers). A third tick
  remains unattributed. Startup perf work stopped here (diminishing returns). Release run passed (UI 2481/0).
- **Test home isolation guard** (2026-11-05 overnight, `test-home-isolation`, Terra + coordinator fix 0c9ed930): a
  test_support initializer gives every test process its own temp HOME, CLAUDE_CONFIG_DIR, CODEX_HOME (created; the
  codex CLI refuses a missing one) and Mast data dir, preserving inherited fixture homes for re-executed test
  binaries; debug/test builds refuse engine and local-profile data paths inside the original home; three live
  transcript smoke tests replaced with synthetic fixtures; `linear_assigned` waits for archive + reason + sync flag
  together (settle race). `scripts/check_test_isolation.sh` (+ allowlist) fails on new home-dependent references in
  test code. Audit: docs/internal/audits/test-home-isolation.md. Release run: nextest 2411/2411, UI 2481/0 (first run
  with the guard caught the CODEX_HOME regression; fixed).
- **Needs-you phase 6** (2026-11-05 overnight, `needs-you-phase-6`, Terra): notification banners and the menu bar's
  needs-you section show the request kind, one-line ask, origin chat/item and age, ordered by priority then age;
  quick actions (choose options capped at 3, Approve/Decline) go through the card relay/resolve path (idempotent,
  banner/menu provenance). `fyi` doesn't banner by default; existing banner settings respected. Release run:
  nextest passed, UI 2481/0.
- **local_import flake fix** (2026-11-05 overnight, `flaky-local-import`, Terra; test code only): each local_import
  test runs in its own process with a private HOME and data dir. Root cause: background external-session adoption
  read the real HOME's sessions and raced the fixtures. Stress 1/20 → 20/20. Release run: nextest 1223/1224; the 1 =
  `linear_assigned::an_untouched_child_is_archived_when_its_ticket_leaves` (assertion at tests/linear_assigned.rs:504,
  "Canceled in Linear" not yet set: a settle race under load; the binary passed 9/9 three times alone). **New flaky:
  linear_assigned.** UI 2475/0. A test HOME isolation guard is in progress (`test-home-isolation`).
- **Perf sweep 4: startup gaps** (2026-11-05 overnight, `perf-sweep-4`, Terra; gated instrumentation only): chats are
  applied ~4.5 ms after core assembly; outline wire encoding 0.11 ms for a 179 KB frame; the 37–52 ms watch
  spawn-to-first-poll interval sits inside one headless GPUI test-executor drain (a 25–31 ms tick plus ~11 ms
  ticks), so it's a benchmark artifact as measured. Registry work finishes before watch spawn; PR fetch/preview
  scanning already use spawn_blocking. Open lead: identify the 25–31 ms main-thread runnable at startup (likely first
  layout). Release run: UI 2475/0; nextest had 2 `local_import` failures under load; the binary passed 8/8 twice
  alone. **New flaky: the `local_import` integration binary** (3 different tests failed under load tonight;
  assertion at tests/local_import.rs:439), so it needs a reliability pass.
- **Cleanup sweep 4** (2026-11-05 overnight, `cleanup-sweep-4`, Terra; mast-ui only, no behavior change): declared the
  objc `cargo-clippy` cfg in crates/ui/Cargo.toml (removed ~141 macro warnings), removed unused code/imports,
  fixed visibility, mechanical clippy. mast-ui lib-test warnings ~169 → 15. Release run: nextest passed, UI 2475/0.
- **Perf sweep 3: startup phases** (2026-11-05 overnight, `perf-sweep-3`, Terra; measurement + instrumentation):
  sidebar draw is ~10 ms (not a problem); hidden sidebar jump ordering skipped. Startup medians of 5 serial runs on a
  53 MB profile: core assembly 85 ms, registry read 12 / decode 21 / settle 22 / save 9 ms, chats observed 242 ms,
  outline observed 317 ms (construction 1.3 ms), loaded 326 ms. Gaps under investigation in perf-sweep-4. Release
  run: nextest 1224/1224, UI 2475/0.
- **Cleanup sweep 3** (2026-11-05 overnight, `cleanup-sweep-3`, Terra): mechanical clippy fixes in mast-engine in
  six module-group commits (no MCP changes needed; mast-mcp had 0); engine diagnostics 431 → 382. No API, serde,
  protocol or behavior change. Release run: nextest 1224/1224, UI 2475/0.
- **Perf sweep 2: startup** (2026-11-05 overnight, `perf-sweep-2`, Terra): font catalog inspection deferred until
  needed (registration 185 ms → 0.01 ms; installed font selections still resolve before drawing, picker complete);
  cached chat revisits skip the unused provisional tail. Initial shell draw on a 53 MB profile 203 → 132 ms. Note:
  the "loaded sidebar 384 ms" figure is cumulative startup, not the sidebar draw (~10 ms; perf-sweep-3).
  Release run: nextest 2404/2405 (`local_import::marker_persistence_failure_is_an_import_error`, load flake, passed
  twice alone), UI 2475/0.
- **Cleanup sweep 2** (2026-11-05 overnight, `cleanup-sweep-2`, Terra): mechanical clippy fixes, no API/serde/
  protocol/behavior change. Clippy warnings: harness 14→1, doc 29→19, proto 18→10, sync 12→6 (with
  `mock-server`); rpc and update had none. Note: `mast-sync --all-targets` needs `--features mock-server` to build
  its integration tests. Merge conflict with phase 5 in user_actions.rs resolved (let-chain + `prMerged` rule).
  Release run: nextest 2355/2355, UI 2473/0.
- **Needs-you phase 5 subset: handoffs land** (2026-11-05 overnight, `needs-you-phase-5-land`, Terra): `kind: handoff`
  cards show a PR lifecycle chip from the PR cache (Ready / Waiting to land / Landed / Closed, including PRs linked
  from other chats); on a PR-merged event the matching open handoff resolves once ("Landed: repo#n merged …",
  actor "Mast (PR merged)"). Per-Space setting "Resolve handoffs when their PR merges", default on. Local
  `--no-ff` merges without a PR stay manual. No Merge button (not built). Release run: nextest 2355/2355, UI 2473/0.
- **respond_to_input validation** (2026-11-05 overnight, `respond-to-input-validation`, Terra): answers are checked
  against the pending request's question ids and option labels before queuing and again before resolving; a
  single unknown or omitted id maps to the only question; otherwise an error lists valid ids/options and nothing
  is sent. An unmatched answer can no longer become a Codex approval denial (friction 4a607d99). Release run:
  nextest 2354/2354, UI 2471/0.
- **Flaky test fixes** (2026-11-05 overnight, `flaky-tests`, Terra; test code only): Devin discovery startup
  deadline 500 ms → 30 s (test now ~94 s by design); device-relay fixture no longer writes the live
  `~/.zeron/worktrees` (per-engine temp worktree root); Codex override login fake keeps running via /bin/sleep
  (still quarantined). Each fixed test 20/20 under 16-way concurrent load. Release run: nextest 1682/1682 (first
  clean engine run tonight), UI 2471/0.
- **Cleanup sweep** (2026-11-05 overnight, `cleanup-sweep`, Terra; no behavior change): obsolete iOS/mobile and
  Windows docs corrected, demo script name, unused compiler constructs and imports removed, two clippy deny lints
  in updater test fixtures fixed. Not done: the remaining clippy backlog and ~169 UI test warnings (141 from the
  objc macro's `cargo-clippy` cfg). Settings fields with no reader kept for stored compatibility:
  `sidebar_grouped`, `right_pane_open`, `terminal_open`. Release run: nextest 1617/1617, UI 2471/0.
- **Needs-you phase 4** (2026-11-05 overnight, `needs-you-phase-4`, Terra): optional `options: [{id, label,
  detail?}]` on create_user_action_request (MCP, proto, registry; old requests unchanged). Cards everywhere offer
  choice buttons + "Other…", an inline reply for `ask` (⌘Enter), and 1–9 picks on a focused card; each relays via
  the phase-2 `Re: "…"` format and resolves once. Coordinator fix 2c514432: dropped `options` from the MCP server
  instructions (6 chars over the 1900 budget; the tool description documents it). Release run: nextest
  2352/2353 (that budget test, fixed), UI passed.
- **First-run follow-up** (2026-11-05 overnight, `first-run-followup`, Terra): PR dashboard no longer shows raw
  `gh` auth output or "No pull requests" after a failed fetch; common auth and missing-CLI errors get a clear
  next step; signed-out fake-Claude harness test added; findings appended to docs/internal/first-run-audit.md.
  Release run: nextest 1680/1682 (`e2e::wrong_id_respond_is_rejected_and_correct_answer_still_resumes` and
  `m5_repos_diffs_terminals::git_history_search_is_fuzzy_complete_and_accepts_sha_prefixes` failed under load,
  passed twice alone), UI 2468/0.
- **Rebindable outline and Team shortcuts** (2026-11-05 overnight, `rebindable-outline-shortcuts`, Terra): the
  outline's default action table (with aliases such as F2/R, ⇧→/Tab, ⌘Z/Control-Z), Shift-F10/Menu and both
  Team shortcuts (⌥⌘T global, `T` in the side panel) are persisted Settings → Shortcuts records under Outline and
  Team; rebinding replaces the action's defaults, updates hints, and conflicts are checked by physical key and
  context. Row-edit Enter/Escape and pickers stay hard-wired. Release run: UI 2467/0, nextest passed.
- **Perf sweep: List virtualization** (2026-11-05 overnight, `perf-sweep`, Terra): List mode virtualizes
  variable-height rows, keeping grouped order, expansion, selection, scroll targets and visible anchors across
  reorder. 250-chat redraw 105.66→1.98 ms mean (p95 150.6→2.5), first draw 68.7→2.5 ms. Measured but not
  changed: prepared 2,000-message transcript open+draw 9.7 ms (prep 74 ms off-thread), production streaming
  5.8 ms/chunk mean (unchanged rows reused), Team facts 0.07 ms, outline redraw 3.2 ms. Benchmarks and method
  in docs/performance-sweep.md (3 ignored bench tests). Release run: nextest passed, UI 2461/0.
- **First-run audit fixes** (2026-11-05 overnight, `first-run-audit`, Terra): report at
  docs/internal/first-run-audit.md (6 findings). Fixed: profile classification (fresh vs existing) now uses
  saved settings or real Space/chat registry data before either startup mode, so headless-first startup no
  longer marks a fresh profile existing; packaging uses the binary Cargo reports (CARGO_TARGET_DIR-aware) and
  fails clearly when missing; launch returns "agent not installed: <harness>" up front instead of "started";
  docs no longer present gh as required; PR repository error lost its Cofactr wording. Open from the audit:
  signed-out-but-installed agent flow, sign-in gate and no-auth tab states not yet checked. Release run:
  nextest 2401/2401, UI 2459/0.
- **PR file paging** (2026-11-05 overnight, `pr-files-paging`, Terra): PR files fetched page by page up to 3,000;
  partial results kept on failures and rate limits; scores reused for an unchanged head sha; "partial" badge
  with "Scored from the first N of M files". Release run: nextest 2350/2350, UI passed.
- **Diff navigation perf + friction import** (2026-11-05 overnight, `perf-diff-navigation`, `friction-import`,
  Terra): navigation keeps the flattened rows when the target is already expanded; the file sidebar is
  virtualized (diff body already was). 300-file synthetic diff: first draw 30.9→1.7 ms, hunk key 52.5→3.4 ms,
  file key 48.6→2.7 ms, scroll 22.9→2.0 ms. Friction "Import…" parses the old agent-friction-log.md format and
  Mast's export (round-trips), preview, redaction, fingerprint dedupe, idempotent re-import, import provenance.
  Release run: nextest 2347/2348 (the 1 = `devin_discovery_errors_and_timeouts_retain_last_good_then_recover`,
  timing flake, passed twice alone; no harness changes), UI 2456/0.
- **Hand-off UI + wake labels + child file limit** (2026-11-05 overnight, `hand-off-ui`, `wake-label-fd-limit`,
  Terra): "Hand off to new chat…" in chat context menus and header overflow (only with live children): sheet
  with prefilled summary, worker list, optional archive of the source; selects the successor; the source shows
  a linked "Handed off to …" line. Wake/report lines resolve the registry chat title at delivery (fallback
  short id, never a model name). Harness child processes raise their RLIMIT_NOFILE soft limit to
  min(hard, 8192). Release run: nextest 2345/2345, UI 2454/0.
- **Space account re-sign-in, durable wakes, drop any file** (2026-11-05, `managed-account-reauth`,
  `durable-wakes`, `composer-drop-any-file`, Terra): Settings → Space accounts has **Sign in again** (same home
  and id, Claude and Codex); auth failures mark the pinned account signed out and the chat error offers
  "Sign in again…". Worker wakes/reports persist in a bounded store (≤500 per orchestrator, atomic
  temp+rename, writes only on record/queue/deliver/prune, off executor threads): UUID event ids, restore
  before any queue drain, recovered wake-only queue rows re-held (user rows stay frozen), `delivered` pruned
  after the snapshot generation persists (legacy 3 s fallback); a crash between dequeue and transcript write
  can duplicate at most once, never lose. Composer: Finder drops and pasted files of any type: images attach,
  workspace files/folders become mentions, outside files insert absolute paths, remote-device chats and
  unreadable paths show a notice. Merge conflict in engine lib.rs resolved (durable's early wake service
  construction + PR-merged wiring after `spawn_watchers`). Release run: nextest 2058/2058, UI 2449/0.
- **Terminology pass** (2026-11-05, `terminology-pass`, Terra; text only): copy templates are **Blueprints**
  ("New from blueprint…", "Blueprint changed"), container templates are **Child template** ("Set up child
  template…", "Child template applied"); glossary in docs/work-outline.md (Space = the folder/outline root,
  project = an outline item marked as a project). Identifiers, tool names, RPCs and stored keys unchanged.
  Release run: nextest 55/55 (selected), UI 2445/0.
- **Hand-off + quick wins** (2026-11-05, `hand-off-orchestrator`, `pr-merged-wake`, `diff-target-remove`,
  `visual-compare-banner`, Terra): `hand_off_orchestrator` MCP tool stages a successor chat on the same item
  (same harness/model/sandbox), reparents active workers before its first turn, moves held/queued wakes,
  copies wake settings and the auto-continue override, records provenance, idempotent retry; first message
  lists workers and open requests. PR-merged wake now fires on owned PR open→merged (deduped, persisted
  marker; names open handoff requests for that branch). Changes picker: Remove per bound target (not the
  primary), limit message points at it, missing target paths pruned. Visual compare: "compared the first N
  px" banner in side-by-side and onion; onion follows shared zoom. Release runs: nextest 2318/2318 then
  1196/1196, UI 2443/0 then 2445/0.
- **Auto-continue** (2026-11-04, `auto-continue`, Terra, 3 slices): when a worker ends a clean turn with its item
  still open (no open Required Action, not awaiting input, no refused tool calls, no user send during the turn,
  nothing queued), Mast sends it a `[Mast: auto-continue n/3]` nudge; after 3 it queues one "worker stopped
  after 3 auto-continues" orchestrator wake. Counter per worker chat per item, reset by a user or orchestrator
  message, persisted across restarts. Off by default (global `autoContinue` in `orchestrator-wakes.json` plus
  per-orchestrator overrides); toggle "Auto-continue workers" in the Team tab header and Item tab Wakes row.
  Known edge: an orchestrator saying "stop" without completing the item or raising a request still gets nudged.
  Release run: nextest 2306/2306, UI 2443/0.
- **Wake echoes + archivable PR rule** (2026-11-04, `wake-echo-redelivery`, `archivable-pr-rule`, Terra): a
  resolved-action wake is suppressed when the receiving orchestrator resolved the request itself (user and
  other-chat resolutions still wake); wake batch selection and queueing are serialized, so concurrent
  evaluations can't deliver the same batch twice. A chat is PR-finished only when every PR it owns is merged
  or closed; a PR with no cached status blocks archiving; mention, ticket and review-launcher links don't count.
  Not yet: restart-durable reports/wakes with stable report ids (item aa155e46; 68b601a8 left open for it).
  Release run: nextest 1174/1174 (engine + mcp), UI 2442/0.
- **Friction reporting + compact agent messages** (2026-11-04, `friction-reporting`, `compact-agent-messages`):
  `report_friction` / `list_friction` agent tools (Mast fills chat, harness, actual model, Space, item, branch;
  dedupe by fingerprint with occurrence counts), auto-capture (refused Mast tool calls, approval prompts + the
  answer, harness-rejected requests, turn errors) through the shared `mast_harness::redact` redactor, a
  Friction section in Space-root details (filters, expand, Create bug, Propose instruction, Dismiss, Export
  Markdown in the old agent-friction-log format) and an auto-capture toggle (default on). Transcript: messages
  from other chats, admin chats and Mast wakes render compact and muted with a source header (prefix-detected;
  structured sender metadata is a follow-up; relayed "Re:" answers are the user's own and render as human).
  Release run: nextest 2294/2294, UI 2442/0. Not yet: importing the old Cofactr friction log.
- **Neutral defaults 1B** (2026-11-04, `neutral-defaults-settings`, 3 Terra slices): one-time UI settings migration
  marker (existing vs fresh profile); fresh profiles get neutral defaults, existing ones keep their effective
  values: admin grant duration remembered (4 h), Team tab hide/auto/always (auto for both, matches today),
  compact single-chat rows (fresh off, existing on), sounds off / banners background-only / live Dock icon off
  / menu bar item off for fresh profiles. Orchestrator wakes default deliberately unchanged (already opt-in via
  starting workers). Rebindable outline/Team shortcuts split out (item 3b6f9919). UI 2436/0.
- **Neutral defaults 1A** (2026-11-04, `neutral-defaults-agent-text`): MCP instructions reworded neutrally (publish
  what you want reviewed; workers raise `kind: handoff`; finished workers can be archived), tool descriptions
  match the engine's real reach/archive rules, examples no longer use Mikey's process; per-Space editable PR
  review / self-review / fix-CI prompt templates (existing Spaces keep `/code-review` and `/fix-ci`, new Spaces
  get neutral prompts; Spaces arriving later via sync are migrated too, untested against a live remote);
  devcontainer paths only when the mount is verified. Release run: nextest 2279/2279, UI 2427/0.
- **Search fixes + coworker cleanup** (2026-11-04, `search-overlay-fixes`, `coworker-cleanup`): ⌘O/⇧⌘F inputs use
  ComposerInput (full editing); a per-checkout file index prewarms when a chat opens and follows the watcher
  (5 s repair in over-budget repos); ⌘O 300 ms → 13–16 ms per keystroke, indexed content search 13 ms first
  match. Follow-up: don't count gitignored dirs toward the watcher's 8,000-dir budget. Cleanup: upstream iOS
  (kept on `archive/ios`), landing, www-redirect, edge worker, workflows, Windows/Linux packaging and unlinked
  media removed; dev scripts → scripts/dev; brand test covers README + guides; `zeron update` says build from
  source; fixtures use /Users/example; internal docs moved to **docs/internal/** (this file is now
  `docs/internal/plans/work-outline/HANDOFF.md`). gitleaks: 40 history hits, all hashes/upstream test data/a
  public analytics key. Repo git auto-gc is off (`gc.auto 0`); run `git gc` manually when idle.
- **Typed Needs-you, phase 3** (2026-11-04, `needs-links-facts`, built in two small Terra slices after one
  stalled chat): `links[]` (artifact, file+line, diff/branch, PR, chat, https URL; validated for reach and
  shape) and `facts[]` (label/value, capped) on requests, persisted and in MCP create/read; card buttons use the
  existing handlers; review cards get a primary Open, handoff cards show Branch/Tests + Diff/PR; `detail`
  renders as Markdown and legacy text auto-links mast-artifact ids and PR URLs. Release run: nextest
  2269/2269, UI 2424/0. Phases 4–6 (inline answers + replaceOpen, handoff lifecycle, preview surfaces) not started.
- **PR review phase 3** (2026-11-04 overnight, `pr-review-search`): ⌘O quick open (fuzzy, recents, changed-in-diff
  group, `:line`), ⇧⌘F in-process streamed content search (ignore + grep-searcher; case/word/regex,
  include/exclude globs, caps, cancellation, only-changed-files), ⌘G go to line in files and diffs, file
  editor Replace moved to ⌥⌘F; all via the rebindable keymap. Back/forward history not built. Release run:
  nextest 2310/2310, UI 2420/0.
- **Typed Needs-you, phase 2** (2026-11-04 overnight, `needs-outcomes`): persisted `resolution.outcome`
  (approved, declined, changes_requested, chosen, answered, dismissed) + `values` + `via`; per-kind card
  buttons (approve → Approve/Decline, review/handoff → Approve/Request changes, Dismiss everywhere, note
  prompt); a fixed-format user-attributed relay to the requesting chat; admin bulk changes apply only on
  outcome `approved` (fixes "resolve with 'no' still approved"); `get_user_action_request`; outcomes in
  read_current. Release run: nextest 2253/2253, UI 2415/0.
- **Typed Needs-you, phase 1** (2026-11-03 night, `needs-kind-field`; design option C hybrid, plan
  mast-artifact:e0a05a3d0248e2e8d8d0fda6bb6214c4): Required Actions have `kind` (ask, review, approve, choose,
  handoff, notice) and `priority`; legacy text-only rows are classified on read; "Ready to merge" text
  matchers replaced by `kind == handoff` (status light, Team, archivable); producers set kinds (admin bulk →
  approve, PR review → review, plan questions → ask, option picks → choose); kind icons and groups;
  `notice` stays out of the badge/light; plan question cards show a link chip. MCP create takes kind/priority.
  Release run: nextest 2248/2249 (the 1 was a stale usage-text assertion, fixed), UI 2412/0. Phase 2
  (`needs-outcomes`, incl. the admin bulk "no still approves" fix) is in progress.
- **PR review phases 1 + 2** (2026-11-03 evening, `pr-review-read-diff`, `pr-review-sidecar`; Codex
  gpt-6.1-terra; brief mast-artifact:3e12f5d0be4cb1de57ed6b0f8be58992). Phase 1, Changes pane: filterable
  file sidebar (renames old → new, +/- counts), J/K next/prev hunk, ⇧J/⇧K next/prev file, hunk count,
  generated/lockfile auto-collapse, content-hash hunk identity, viewed state per PR (registry row; same-hash
  hunks stay viewed across head moves, cleared on PR close) or per diff target. Phase 2: `mast.review/v1`,
  `review_publish` (validates anchors against the real diff, lists valid hunks on error) / `review_get`,
  artifact kind `review`, stale-on-head-drift (local PR ref, live PR cache when it knows the head), summary
  + highlights card, review launcher prompt. Release run: nextest 2246/2247 (the 1 was a real
  review_get staleness bug, fixed in 663473a3), UI 2411/0. Phase 3 (quick open/search) and 4
  (walkthrough) next.
- **Surface stalled workers** (2026-11-03 evening, `surface-stalled-workers`): orchestrator wakes for a
  worker waiting on a prompt/approval (with the question text) and for a worker turn that ended after
  refused/failed Mast tool calls; both default on, toggles in `orchestrator-wakes.json` / Team tab.
  `get_chat`/`list_chats` expose the pending question and last-turn tool failures; markers in the Team
  tab and running popover; create_chat says which harnesses support Mast tools. Shipped on targeted
  tests only (engine orchestrator_wakes + agent_tools 60/60, MCP/proto 304, child's UI 2393); no
  overlap with the Codex MCP fix underneath.
- **Codex MCP approvals** (2026-11-03 evening, `codex-mcp-approvals`, installed 17:06): Codex chats in Ask
  mode had every `mast-agent` tool call silently denied (Codex sends `mcpServer/elicitation/request`
  before MCP tool calls; the harness answered "unsupported"). Now the injected `mast-agent` server gets
  `default_tools_approval_mode = "approve"` and its elicitations are accepted; other servers'
  elicitations (e.g. Computer Use) become a Yes/No prompt via the input bridge; rejected server requests
  log at INFO (`mast_harness::codex`). Harness 455/455, UI 2391/0. Verified live: a Codex Ask-mode chat
  made 5/5 Mast tool calls without prompts; Computer Use not yet exercised.
- **Release 2026-11-03 morning** (installed 11:32): `faster-releases` (nextest config with
  `release`/`quarantine` profiles, terminal-heavy tests capped at 2 at a time,
  `scripts/run_release_tests.sh`, `release-testing.md`; engine suite ~20 min vs 3.5 h+ projected for
  `cargo test`); `resolve-subtree-reach` (agents resolve requests on items in their own item's subtree,
  so a new coordinator can resolve its predecessor's children's actions); `compact-row-grant-admin`
  (Grant/Renew/Revoke admin on compact single-chat rows); `inherit-auto-approve` (Codex workspace-write
  adds the worktree git dir and the git common dir as `writableRoots`, verified against the real
  seatbelt sandbox, so commits/pushes don't prompt; `create_chat { autoApprove: true }` allowed only
  from a caller on Auto, Ask otherwise).
- **Archive finished workers** (2026-11-02, `archive-finished-workers`): shared rule in
  `engine/src/archivable.rs` (Team tab and agent tools): archivable = idle, no open Required Actions,
  and one of item completed / linked PR merged or closed / a resolved "Ready to merge" / idle > 3 days
  with the item completed or archived; returns a verdict + reason. Non-admin orchestrators archive their
  children (no rule) and workers under their project worker destination (rule, or `force: true` +
  reason, logged); admin chats archive any chat in their Space except themselves. Unarchive always
  allowed. In-memory agent archive log. Team tab "Archive finished (N)".
- **Known test failures** (pre-existing, not regressions):
  - Engine lib, on main `3801cb5b`: `capture_source_context_reads_the_dispatch_checkout`,
    `local_opening_tail_arrives_before_full_mirror_and_keeps_all_history`.
  - Engine lib, flaky while Zeron itself is running (likely the live instance lock):
    `instance_lock::holder_probe_reports_pid_without_disturbing_the_lock`.
  - Engine integration, flaky under parallel load (passes alone):
    `local_first::headless_sign_out_closes_joined_edge_rooms_and_stops_daemon`,
    `message_queue::restart_restores_the_queue_frozen_until_an_explicit_send` (sometimes passes).
  - Flaky under full parallel load (pass alone): engine `work_outline_rpc::mutations_stay_fast_on_a_large_local_registry`,
    harness `opencode::tests::stalled_prompt_post_has_a_bounded_timeout`.
  - Engine integration, flaky under load: `device_routing::workspace_file_surface_proxies_over_the_relay`.
  - Engine integration, flaky under full parallel load (pass alone): `artifacts::blocking_questions_own_actions_and_the_rest_roll_up_without_duplicates`
    (timing on the 2s question reconcile), `m5c_accounts_uploads_titles::cursor_login_flow_spawns_shim_and_auto_activates`.
  - Engine integration, flaky under full load (pass alone): `codex_login_resolver::codex_login_resolves_override_and_adds_its_directory_to_child_path`.
  - Engine integration, flaky under full load (pass alone): `instruction_delivery` revision-wait tests.
  - Flaky under full load with several child chats building (pass alone): `external_import_links_repair::repair_*`,
    `workspace_files::workspace_file_rpcs_list_search_read_write_and_watch`, harness `executable::tests::version_probe_bounds_hangs_and_rejects_nonzero`.
  - Engine integration, flaky under load (pass alone): `codex_login_resolution::{codex_login_honors_override_only_installation, login_child}`.
  - Load-sensitive (pass alone; hard timeouts): `pr_ticket_cache_login_shell` (5 s), `managed_accounts` sign-in tests (PTY), `pr_review_sync::setting_the_destination_syncs_the_space`, `m5_repos_diffs_terminals::git_status_stream…`.
  - Harness integration, environmental: `codex::models_discovers_visible_catalog_with_pagination`
    (expects `/bin/false`, which macOS lacks); `shell_env::falls_back_when_interactive_attempt_hangs`
    (flaky on the integration head too).
  - Engine integration, verified failing on `a3651a9d` before remove-canvas:
    - `e2e::generated_image_is_materialized_before_publication_and_survives_reopen`
    - `external_import_sync::sync_picks_up_a_closed_message_but_defers_the_new_open_reply`
    - `local_first::online_runtime_shutdown_stops_edge_workers_and_retires_the_graph`
    - `m5c_accounts_uploads_titles::titling_e2e_names_chat_and_renames_worktree_branch`
    - `message_queue::restart_restores_the_queue_frozen_until_an_explicit_send`

## Housekeeping (2026-10-25)

- All 80 feature worktrees under `.worktrees/zeron/` were removed and their local branches deleted.
- The 26 merged feature branches and the 193 copies of upstream branches were deleted from
  `origin`. Upstream's tags are still there, and so are ours (`pre-linear-integration`,
  `pre-canvas-removal`).
- Leftovers were saved to `~/zeron-dev-app-backups/leftover-patches-2026-10-25/`:
  - uncommitted edits found in the `work-outline-boundary-moves` and `pr-sync-create-fix`
    worktrees (both branches were already merged, so the edits are probably abandoned);
  - the unpushed `pr-sidebar-removal-work` commit, which `d7a30add` already contains.
- GitHub Actions are turned off on the GitHub repo. The workflows are upstream's release, deploy
  and TestFlight jobs, which would fail without upstream's secrets.
- The README (en and zh-CN) now describes the app (Creel then, Mast now), with the lockup from `docs/media/readme/`. The
  screenshot block is commented out until Mikey adds `outline.png`, `pr-page.png` and `plan.png`
  there. `.github/FUNDING.yml` is gone.
- `/Applications/Zeron.app` (upstream 0.2.59, bundle id `sh.zeron.app`) went to the Trash. Old
  backup binaries were pruned to the last three installs.

## Working model (since 2026-10-20)

Mast development is organised in the Work Outline under the root item "Zeron Dev"
(`48ef4b86c9967626ba1b6372e4cdfa66`, Space agent-mode-tools) with children Release, Bugs, Features,
Parked. A coordinator chat on that root starts child chats on the right items with
`create_chat { workItemId }`, polls `work_outline_read_subtree` for "Ready to merge" actions, and is
the only chat that merges, installs, edits this file, and pushes. The coordinator does the release
itself; there is no separate Release chat (Mikey, 2026-10-20). Feature and bug chats push their
branch and raise "Ready to merge: <branch>".

Coordinator tips:
- Never block on a child with a foreground `wait_for_turn`, because Mikey keeps giving new work
  while children run. A child that runs its tests in the background goes idle before it is done,
  so idle doesn't mean finished. Children also push early checkpoints, so a push doesn't mean
  finished either. In a background shell, watch the branch's sha on origin (`until git ls-remote
  origin <branch> | grep -q -v <last-sha>; do sleep 60; done`), and on each change read the subtree
  for the "Ready to merge" action. That action is the real done signal.
- After a release, delete the merged branch from `origin`, then remove its worktree and local
  branch.
- `work_outline_read_subtree` cuts long action `detail`s short (`detailTruncated`). For the full
  text, `read_chat` the `originChatId`.

The peon tray (`peon_tray.py`) was retired on 2026-10-20. Zeron's own banners and chimes replace it.

## In flight (snapshot 2026-11-05 evening; coordinator handed off from chat 74bc70e5)

**Coordinator:** a fresh Mast Dev chat created by `hand_off_orchestrator` from 74bc70e5 (which ran 2026-11-03 → 11-05).
Keep it thin: implementation goes to Codex Terra (Mikey's preference to save Anthropic usage). Release runs go
DETACHED (`nohup /tmp/mast-release/releaseN.sh & disown`; the script sets `ulimit -n 4096`); never start one while a
merge is unresolved, and never prune `target/debug` while one is running. Install with
`scripts/install-macos-app.sh target/installed/mast /Applications/Mast.app computer.mikey.mast.dev` (see Install
below). Answer Codex approvals with the real question id (get_chat → pendingInput); the installed build now errors on
unknown ids instead of denying.

**Running:** none.

**Waiting on Mikey:**
- Plan rev 3 for minimal-talk / visual harness (mast-artifact:37a6c5c807eb8fe30aa1b56044a8749a): approve the
  observe-first order and confirm the warehouse dashboard as the pilot. Baseline metrics are taken (talk-metrics);
  re-run around 2026-11-12 with `tools/mast-metrics` (fresh read-only snapshot, `--compare`).
- Active-checkout tracking design (Needs-you, parked item 40377ea3). PR review brief (unlocks PR review phase 4).
- Proposed instruction edit on Mast Dev (install step → install script, proposal acd9f3ef): accept it.
- Cofactr: accept Space-root proposal 8838b1f4; admin-chat follow-ups (Q-7, archive the old Project Template copy,
  Tickets rules rev 3, ENG-3222).
- Whether `peon_hook.py` (agent-mode-tools root, symlinked from ~/bin) is still called by a Claude Code hook; delete if not.

**Queued (agreed):**
1. Coworker readiness: workflow pack (export Mikey's instructions/templates/model routing as an optional importable
   pack; draft for Mikey first), then a real coworker dry run (Mikey picks who).
2. Retire the agent-mode-tools wrapper (Space root → plain clone of Pluto8195/mast, worktrees under it, move the
   `.zeron/`/`.mast/` artifact folders incl. the brand kit, update root instructions/HANDOFF/memory; drops the
   submodule-bump step). Needs a quiet window with no workers.
3. Later: PR review phase 4 (walkthrough), item-tab artifact surfacing design (171cde5a), container templates phases
   2+, one attention model / one chat-status model (design first), the cross-device archive race follow-up from
   stale-coordinator-wakes, optional "move ~/.zeron to ~/.mast" Settings action.
4. Known bug to file/fix: Claude Code's multi-question ask-user prompt in Mast returns "rejected" and drops answers
   (friction 8e01dbe1); ask in plain text meanwhile.

**Terra working style:** it ends turns early ("can't complete in this turn") on anything that looks big.
Give one or two concrete items per chat, say "partial progress is fine: commit and keep going", start a
fresh chat per slice on the same branch, and archive the finished one. Build with
`CARGO_TARGET_DIR=<worktree>/target-clean`; never seed the cache. Approve its unsandboxed test reruns
(watcher/home-dir tests only pass outside Codex's sandbox). Codex shells default to 256 open files: tell
workers to `ulimit -n 4096` before tests; that fixes most nextest stalls and agent_tools failures. Have them
complete their item BEFORE raising Ready to merge (an item with an open request can't be completed).

**Disk:** prune `zeron/target/debug` when idle (`find deps -maxdepth 1 -type f -mmin +360 -delete`, same for
build/ and .fingerprint/); delete merged worktrees' target-clean right after merging. Repo auto-gc is off.

**Cofactr admin-chat follow-ups Mikey is sending:** resolve stock lot Q-7, archive the old Project Template copy,
Tickets rules rev 3 (`kind: review` until the PR is approved, then `handoff`). Cofactr Space-root proposal
8838b1f4 (kinds/facts/links) awaits Mikey's accept.

**What Mikey is doing next:** using an admin chat in the Cofactr Space to set up the Projects container
template (matching his current copy template), move existing projects under it with a preview, and
run the stage-0 talk-budget pilot from the minimal-talk research (mast-artifact:37a6c5c807eb8fe30aa1b56044a8749a).

**First admin chat (Cofactr Space): how and the drafted prompt.** Start a chat in the Cofactr Space →
right-click it → "Grant admin for this Space…" (4 h, "Skip bulk confirmations" OFF the first time).
Prompt, to adjust with Mikey:

> Set up live container templates for my projects:
> 1. Create a **Projects** item (or use the existing one) and configure its container template to
>    reproduce my current copy project template exactly — same child items, same instructions per node,
>    Tickets as the worker destination. (Find the current template in Space-root details → Templates.)
> 2. Create a test project under it with New work item and check it has the full structure and live
>    instructions; archive it afterwards.
> 3. Move my existing projects (Basedash, Kit Line, the warehouse dashboard, the stock lot traceability
>    one) under Projects and apply the template to each, mapping existing children by title. Show me the
>    preview as a Required Action before applying.
> 4. Apply the stage-0 "talk budget" pilot from the minimal-talk research
>    (mast-artifact:37a6c5c807eb8fe30aa1b56044a8749a) to the warehouse dashboard's Planning/orchestrator
>    instructions, as instruction edits I can see in the action log.
> 5. Report back in ≤10 lines: what changed, what didn't map cleanly, decisions I need to make.
> Rules: archive, never delete; bulk changes go through the preview → Required Action; everything must
> be undoable from Recent Changes.

**Waiting on Mikey (Required Actions open):** PR review brief (mast-artifact:3e12f5d0be4cb1de57ed6b0f8be58992),
minimal-talk research (held until the admin chat runs stage 0), active-checkout design
(mast-artifact:679f61de127b3519c16de37f8d90f99b), `mast-doctor` Ready to merge.

**Coordinator lessons from this session:**
- Design-only children never push, so check Required Actions whenever a child goes idle. Orchestrator
  wakes now deliver these as "Worker updates".
- When several branches conflict, push an integration snapshot as `release-staging` and have the
  children rebase onto it. Merge the most foundational one first.
- Session and Mast restarts kill background shells: re-arm watchers, resend children, rerun tests.
- When you say "right after", queue it in the same step.
- Never start long runs under `nice`. Children must only kill processes inside their own worktree.
- Logs: `~/.zeron/logs/mast-headed.log`.

## Known gaps

- Visual compare / zoom (after `compare-zoom-gaps`, 2026-10-28):
  - Render diffs are capped at 16,000 CSS px (no tiling); longer pages show "Compared the first
    16,000 px of N". Unverified in a real WKWebView at that height.
  - HTML/Mermaid compare zoom mirroring is headless-tested only. Onion skin doesn't zoom.
  - Option revisions recorded before 2026-10-20 can't be compared across revisions.
- Linear GraphQL (projects, attachments) unverified against the real API.
- **Usage (2026-10-27):** a 7-day audit of `~/.claude/projects/*.jsonl` found about 43% of usage
  in three long-lived coordinator chats (Opus median context 226K tokens, p90 680K). Keep
  coordinators thin, restart them often (this file holds the state), and keep screenshots and
  design rounds in child chats. The unexpected Fable usage (about 18%) is fixed: see "Every chat
  start passes an explicit model" above.

## Next, agreed but not started

- **Container templates phases 2+** (plan mast-artifact:df5be7ef1a2fa657e3737f17dd0ef89a), after Mikey's
  admin-chat test shows what is missing.

## Parked (do not start unasked)

Archived-chat retention (measured 2026-10-30, not needed yet): 363 chats, 108 archived; archived rows
are 0.33 MB of the 6.4 MB registry, archived transcripts 37 MB of 93 MB snapshot docs; `docs.sqlite3`
112 MB, journals 265 MB (626 files). Cost is mostly disk; boot reads journal tails only. If it grows,
add a retention setting that compacts/exports transcripts and journals of chats archived > N days,
keeping the registry row.

Decisions 2026-10-28: Linear automatic project sync / status write-back stays parked (manual
sync is fine; auto changes could make a mess). The internal zeron → mast code rename goes on its
own branch `rename-zeron-to-mast`, not merged until Mikey reviews it. Active-checkout tracking
stays parked.

See `parked-proposals.md`: active-checkout branch tracking, Linear v2 automatic project sync and
status write-back, a "Waiting for input" group (declined), Tod-style action log / undo.

## How we work

- **Worktrees:** `/Users/mikey/Projects/agent-mode-tools/.worktrees/zeron/<branch>`, branched from
  the integration head. Seed the build cache with
  `cp -c -R target/debug/{deps,build,.fingerprint}` (APFS clones), add a `.workspace-root` file.
  Merge with `--no-ff` into `work-outline-implementation`. **Delete each worktree's `target/` after
  merging**; keep at least 100 GB free.
- **Subagents:** one narrow task per agent with clear file ownership; read files in ≤200-line
  slices (they stall on huge files like `overview.rs` / `shell.rs`); commit early and often.
- **Tests:** release runs use `caffeinate -i scripts/run_release_tests.sh [BASE_REF]` (nextest
  `release` profile for non-UI crates with a 3-min per-test timeout and the known flaky tests
  quarantined; UI via `cargo test -p mast-ui --lib -- --test-threads=1`; only the crates the diff
  touches, `--all` for everything; `.config/` or Cargo changes select all). Any failure there is new.
  `--quarantine` runs the flaky set separately. Always `caffeinate -i`: the Mac's maintenance sleep
  pauses long runs. Details and timings: `release-testing.md`. Build the installed binary in parallel
  with a plain `cargo build --profile installed -p mast` (incremental `target/installed`).
- **Install:** `cargo build --profile installed -p mast` (optimised; since 2026-10-27, replaces the
  debug build); back up the current binary to `~/zeron-dev-app-backups/`;
  `scripts/install-macos-app.sh target/installed/mast /Applications/Mast.app computer.mikey.mast.dev` (atomic
  binary copy to `Contents/MacOS/mast`, `zeron` symlink, Info.plist executable/id/URL schemes, ad-hoc codesign,
  lsregister); push `work-outline-implementation`. Before 2026-11-05 this was a `cp … Contents/MacOS/zeron` plus
  `codesign --identifier sh.zeron.app.dev`. Earlier bundles:
  `Zeron Dev.app` (until 2026-10-23) and `Creel.app` (until 2026-10-26), kept only as rollbacks.
- **Never restart Mast automatically.** Install, then tell Mikey what to restart and test.
  Restarting kills agents running inside Mast, so commit WIP first.
- **Xcode license prompts** after Xcode updates block git/cargo: Mikey accepts, or use
  `DEVELOPER_DIR=/Library/Developer/CommandLineTools`.

## Hard rules for agents

- No GUI launches, example binaries, or screen capture; verify UI with headless gpui tests.
- Never print secrets; never touch `~/.claude` / `~/.codex` config or sessions, or Codex
  computer-use.
- Never write to live `~/.zeron` stores; inspect with read-only sqlite `.backup` copies.
- No real Linear/GitHub API calls in tests. Nothing is ever written to Linear.
- Never commit or push to `main`.
- Registry operation ids must be ≤ 64 characters; hash long keys with `derive_id`.

## Diagnostics

- Outline key handling: grep `work_outline_keys` in `~/.zeron/logs/mast-headed.log` (zeron-headed.log before the rename).
- Linear PR attachments: log target `linear`.
- Pre-settle registry DB backup:
  `~/zeron-dev-app-backups/local-profile-docs-2026-10-12-pre-settle.sqlite3`.
