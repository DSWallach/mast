# Merge queue and cross-family reviewers

Code: `crates/engine/src/merge_queue/`, `crates/engine/src/review_dispatch.rs`.
It is an engine library; no agent tool, RPC or UI calls it yet.

## A land

| Step | State after | Failure |
| --- | --- | --- |
| `submit`: `git merge-tree --write-tree` against the current target | `queued` | `conflicted`, with the paths |
| Merge the tested commit in a scratch worktree, fold the changelog, commit, `BuildJobs::enqueue` | `building` | `conflicted` or `failed` |
| The build job reaches a terminal state | `awaiting_review` | `failed` |
| `record_verdict` approves, then `git update-ref <ref> <new> <old>` | `landed` | `rejected`, or back to `queued` if the target moved |

- Targets are `main` and `mast/int/<project>/<lead>`. A missing integration
  branch is created from `baseRef` by the first land; `main` must exist.
- One request per `(repository, target ref)` is `building` or
  `awaiting_review`; a partial unique index enforces it. A request waiting for
  review holds its lane until the verdict or `cancel`.
- The changelog is folded **before** the build, into the merge commit, so the
  built commit is the landed commit. With a root `CHANGELOG.md` the fragment goes
  to its `Unreleased` section and `docs/changelog.d/<workItemId>.md` is removed;
  without one, the fragment file is kept or written. A symbolic link on either
  path fails the land.
- `docDeltas` are carried to the landed record. The queue does not edit shared
  documents.
- A verdict names the tested commit (it survives a requeue) or the current merge
  commit (it does not). It may arrive before the build ends. A rejection ends the
  request; submitting the same commit to the same target returns the rejection.
- After 5 requeues the request fails.

## Safety

- The target ref changes only through the compare-and-swap.
- A target checked out in any worktree is not moved, because that checkout would
  be left stale. The request waits and says where it is checked out. A `main`
  that stays checked out in the primary clone therefore never lands through the
  queue until it is detached or switched.
- Queue git commands run with hooks, signing and rerere off, as
  `Mast MergeQueue <merge-queue@mast.invalid>`.
- While unfinished, the tested and merge commits are pinned under
  `refs/mast/merge-queue/<requestId>/`. Scratch worktrees live under the queue's
  root.
- If the engine stops between the ref update and the store write, the next step
  sees the merge commit in the target and records the landing.

## Landed signal

`MergeQueue::landed(workItemId)` returns the newest `LandedRecord`;
`landings` returns all of them. Each names its target ref: a land on a lead
branch is not a land on `main`. `QueueWaitReason::NotLanded` does not read it yet.

## Cross-family reviewers

`model_family(harness, model)` goes by the model id first (Cursor or opencode may
run any vendor's model), then by a single-vendor harness. `select_reviewer` takes
the first candidate of a known family other than the implementer's. Otherwise it
falls back and sets `fallbackReason`: only the same family is signed in, or a
family is unknown. `signed_in_reviewers` offers the harnesses whose local sign-in
can be checked (Claude Code, Codex, Antigravity).
`MergeQueue::assign_reviewer` keeps the choice on the land request.

## Follow-ups

- Call `submit` from `report_to_parent`, validating the build command against the
  approval rules, and drive `tick` from an engine task.
- Apply `ReviewerChoice::config` in `ReviewDispatcher::dispatch`, and record the
  reviewer's verdict with `record_verdict`.
- Let the work queue read the landed signal.
- A remote build host needs the merge commit pushed to it before its slot can
  check it out; the queue builds on a host that sees the local repository.
