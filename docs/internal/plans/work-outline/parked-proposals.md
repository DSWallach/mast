# Work Outline — parked proposals

Proposals investigated and deliberately parked (2026-10-13). Revisit on request.

## Active checkout tracking for workspace-root chats

Finding: 113 of 117 chats in the Cofactr workspace Space start at the superproject root.
`capture_source_context(request.cwd)` (`crates/engine/src/doc_host.rs`) rewrites each chat's
branch to the superproject branch before every turn, so every chip shows the same branch while
agents actually work in submodules or mid-chat worktrees (`.worktrees/<repo>/<task>`) that Zeron
never attaches. Branch-based PR inference then resolves every root chat to the same superproject
PR (`pr_ticket_cache.rs` `fetch_github_pr`).

Proposed, in order:
1. Track an "active checkout" per chat from tool-call cwd, `cd`, `git -C`, and `git worktree add`;
   drive the branch chip, Changes pane, and PR lookup from it. Never change `cwd` or a binding's
   execution root.
2. Show differing submodule branches for superproject-root chats; skip root-branch PR inference
   and match PRs by submodule remote.
3. Start Chat and Review PR create the primary repository's worktree up front without retargeting
   the Chat, tell the agent to work there, and record it as the chat's active checkout.
4. Later: per-member worktrees for multi-repository bindings.

## Linear integration (modelled on Tod, `~/Projects/tod`; no license, so reimplement)

Reviewed with the user 2026-10-13. Principle: nothing in the Outline is created or changed
automatically; Linear is opt-in per Space. A more automatic mode (project-linked items whose
children track the project's issues, Tod "generators") is deferred until the user asks.

Version 1 (read-only, per-Space opt-in, off by default):
- Connection: Linear personal API key in the macOS keychain (bare `Authorization` header,
  `viewer` check before saving, prompt on first Linear URL in a Linear-enabled Space, clear and
  re-prompt on 401, retry-after capped at 30 s). Never exposed to agents. Spaces with Linear off
  make no calls, show no Linear UI, and keep pasted Linear URLs as plain links.
- Linear panel (on demand, Overview right-side slot): My issues (assigned, open); My projects
  (lead/member) expandable to their open issues; search; already-linked markers with Reveal in
  Outline; refresh on open and manual only.
- User-initiated node creation only: Create work item (under the selected item or a chosen
  parent), drag a row onto an Outline item, Link to selected item (issue or project), and
  Create nodes for selected issues of a project.
- Linked nodes: live chip (state, assignee, priority, labels) is informational only. Title and
  description changes in Linear surface as "Updated in Linear · Update"; nothing changes until
  the user applies it (pending decision: prompt vs. automatic-until-edited, Tod's
  `ticket_modified_fields`). Status, children, placement, and archive state are never changed
  by Linear.
- Item title renames must not count as instruction drift (only instruction text should).
- Chat ticket chips use the same client in Linear-enabled Spaces; chats whose branch or messages
  name a ticket get a dismissible "Move under ENG-…" suggestion, never an automatic move.
- `work_outline_read_current` returns the linked ticket summary when the Space has Linear on.
- Nothing is written to Linear.

Open decisions: title format ("ENG-123 · title" vs. title); Linear text changes as prompt vs.
automatic-until-edited; agent ticket context (full description vs. id/title/status/url).

Later: automatic project sync (Tod generators), optional per-Space forward-only state write-back
(Tod `linear_sync.rs`), filter presets.
Study notes: `/tmp/tod-linear-study.md` (not versioned).

## Copy-on-write worker worktrees from a seed checkout (parked 2026-11-19)

Idea (from lukeed/lane, commaok.xyz "git CoW worktrees", leostera/git-twig): keep a hidden, locked, detached
"seed" worktree parked on main per repo, with its ignored build output (node_modules, target, .venv) in place.
Each new worker worktree is an APFS clone (`cp -c`/clonefile) of the seed. Then git points it at its branch
(`git worktree add --no-checkout` metadata, fix the `.git` file, `git checkout <branch>` and
`git update-index --refresh`). Files share disk blocks until written, and dependency installs are mostly skipped.

What Mast does today: `git worktree add` for the source, plus an APFS clone of the main checkout's
`target/debug/{deps,build,.fingerprint}` for the build cache. The source checkout is small; the clone already
covers the expensive part for Mast itself.

Where it would pay off:
- Large JS monorepos, where per-worktree `node_modules` and install time dominate (one such repo had about 135
  worktrees and 145 GB).
- As a product option: a per-Space "seed" setting for the opt-in "own worktree for new workers" hygiene feature,
  listing the ignored dirs to carry over.

Known catches to measure in the spike:
- Cloning millions of small files is metadata-bound (our target/debug clone took over 2 minutes and once stalled);
  measure seed-clone time vs `git worktree add` + install.
- `du` counts clones as full copies. Measure real usage with free-space deltas on a quiet machine, or APFS-aware
  tooling.
- Seed staleness: keep it current with main (refresh on fetch, or on demand before a clone).
- Non-APFS volumes and Linux: fall back to a plain copy or a normal worktree.
- Tools that embed absolute paths in build output (cargo fingerprints, some node tooling) may rebuild after the
  clone.

Spike when resumed: measure creation time and real disk use, seed-clone vs today's method, on Mast and on a large
JS monorepo (read-only on the real repos; work in a scratch clone). Then recommend whether to build the Space
option. Work item: "Research spike: copy-on-write worktrees from a seed checkout" (Parked).
