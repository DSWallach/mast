# Mast workspace memory

## New feature workflow

Whenever David starts describing a new Mast feature, create a fresh feature branch in a new Git worktree before making feature changes. Treat this as standing authorization to create the branch and worktree without asking again.

- Use a descriptive branch name and a dedicated directory under `.worktrees/`.
- Start from the latest fetched `david-rolling` unless David specifies another base (David, 2026-11-16). Compare the local branch with `origin/david-rolling` and use whichever is ahead.
- Make all feature edits, commits, and verification runs in that worktree. Preserve changes in other worktrees.
- Continue follow-up work for the same feature in its existing worktree; create a new branch and worktree for each distinct new feature.
- Report the branch name and worktree path when starting the feature. Keep its changes scoped so the branch is easy to review and merge.

## Review gate before merging

Every feature or fix branch must pass the `pr-review` agent before it merges into `main` or `david-rolling`. The agent lives in `.claude/agents/pr-review.md` in the main checkout. Sessions in `.worktrees/` pick it up too. It overrides the Cofactr `pr-review` here.

- **Run it on the branch.** Claude Code chats launch the `pr-review` subagent. Other harnesses run `claude --agent pr-review -p "<prompt>"` from the worktree. The prompt names the branch, its worktree, its base and the work item, if there is one.
- **Address every finding.** That includes Suggestions and Nits. A finding is addressed when it's fixed on the branch, or when David explicitly waives it. Never waive one yourself. If you disagree with a finding, raise a Required Action for David instead of skipping it.
- **Re-run until CLEAR.** After the fixes, run the agent again with the previous findings and David's waivers, quoted. Repeat until its last line reads `Merge gate: <branch> @ <sha> vs <base> => CLEAR`.
- **The gate is pinned to a SHA.** Any later commit needs a fresh CLEAR run before merging. That includes a merge from the base. "Ready to merge: <branch>" must quote the CLEAR gate line and list any waivers.
- **The merger checks before merging.** Before `git merge` into `main` or `david-rolling`, confirm `git rev-parse <branch>` equals the SHA in the latest CLEAR gate line. If it doesn't, or there's no gate line, don't merge.
- **What's exempt.** Merging `david-rolling` into `main` needs no new review when every commit it brings came through the gate. Commits made directly on `david-rolling` don't count as gated. Review them before `main` takes them. Merges that only pull in upstream (`upstream/main`, `upstream/work-outline-implementation`) are exempt too.

## After a merge

A local `post-merge` hook (`.git/hooks/post-merge`, not versioned) cleans up after every `git merge <branch>` into `david-rolling` or `main`, from any worktree (David, 2026-11-16).

- **What it does:**
  1. Removes the merged branch's worktree under `.worktrees/` with `git worktree remove`.
  2. Deletes the branch with `git branch -d`. Remote branches are left alone.
  3. Trims the shared cache with `cargo sweep --maxsize 60GB`. Set `MAST_SWEEP_MAXSIZE` to change the cap.
- **What it leaves alone:**
  - It keeps a worktree with uncommitted or untracked changes. Commit or discard them, then remove the worktree yourself.
  - It skips the sweep while any `cargo` or `rustc` process is running. Run the sweep by hand afterwards.
  - It never touches the main checkout or `david-rolling`, `main`, `david/personal` or `work-outline-implementation`. Squash merges and merges into feature branches don't trigger it.
- **Follow-up work:** a merged feature's worktree is gone afterwards. Start follow-up work on a fresh branch and worktree from `david-rolling`.
- **Preview first:** `MAST_POST_MERGE_DRY_RUN=1 git merge ...` prints what the hook would do without doing it.
- **Install:** `cargo install cargo-sweep`. Without it, the hook skips the sweep and says so.

## Commands that need approval

Approval rules in the Space can answer Codex sandbox prompts for `scripts/ui-preview.sh`, `cargo test|nextest|build|check|clippy` and `ps`/`pgrep`. A rule only applies when every part of the command is in a shape it can read.

- Run `touch` and other edits as their own command. Don't chain them before a preview or test run.
- Call `cargo nextest` through PATH, never a binary under `/tmp`.
- Log to one plain file directly in `/tmp`: `> /tmp/<name>.log 2>&1`. No `>>`, no subfolders.
- Pass preview options as flags or `MAST_PREVIEW_*=<value>` without paths.
- Filter `ps` with `| rg '<pattern>'`, `| head -n N` or `| cut -cN-M`. Not `awk` or `sed`.

## Build cache

All Mast builds share one Cargo cache: `CARGO_TARGET_DIR=$HOME/Work/mast/target`, exported in `~/.zshenv`. A worktree must never get its own `target/`.

- Never unset or override `CARGO_TARGET_DIR` for a Mast build. If it is missing in a shell, export it with the value above before running cargo. Don't change the value either. The one exception is `scripts/run_release_tests.sh`, which picks its own directory on purpose.
- If cargo prints "Blocking waiting for file lock", another worktree is building. Wait for it to finish.
- Artifacts live in `$CARGO_TARGET_DIR/<profile>/`, not `./target/<profile>/`. New scripts must resolve paths with `${CARGO_TARGET_DIR:-$ROOT/target}`. Branch `david/scripts-honor-cargo-target-dir` fixes `scripts/run-macos-dev.sh`, `scripts/dev-demo.sh` and `scripts/e2e-smoke.sh`. Until it is merged into the branch you're on, those scripts still hard-code `$ROOT/target/debug`, so run them only from the main checkout.
- Clear the cache now and then. Run `cargo clean` after big merges. Run `cargo clean --profile installed` to drop just the optimized builds. Never clean while another Mast build is running.
