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
  branch is created from `baseRef` by the first land; `main` must exist. A
  branch that existed or has landings is never created again: if it is deleted,
  its pending requests fail until it is restored. This includes a lead branch
  removed on purpose: after any landing the queue never recreates that name, so
  reusing it means creating the branch by hand.
- One request per `(repository, target ref)` is `building` or
  `awaiting_review`; a partial unique index enforces it. A request waiting for
  review holds its lane until the verdict or `cancel`.
- The changelog is folded **before** the build, into the merge commit, so the
  built commit is the landed commit. With a root `CHANGELOG.md` the fragment goes
  to its `Unreleased` section and `docs/changelog.d/<workItemId>.md` is removed;
  and the branch's own file there is folded too (an identical sent fragment is
  added once). Without one, the sent fragment is appended to that file, up to
  16 KB, so an earlier land of the item is kept. When there is something to
  write, a symbolic link on the way fails the land. CRLF changelogs stay CRLF.
- The merge commit is saved before the build is enqueued, under an operation id
  derived from it. After a crash or a full build queue the same commit and job
  are used if the target has not moved. Otherwise the old job is cancelled and
  must end before the commit is made again, even if the target moves back
  meanwhile.
- An approval covers only the tested commit or the current merge commit. Giving
  up a merge commit drops an approval of it.
- Repeating a submit returns the existing request only when branch, base, build,
  fragment and doc deltas match; different inputs are an error.
- `force_cancel` frees a lane whose repository can no longer be read, and records
  "landing status unverifiable" on the request.
- `docDeltas` are carried to the landed record. The queue does not edit shared
  documents.
- A verdict names the tested commit (it survives a requeue) or the current merge
  commit (it does not). It may arrive before the build ends. A rejection ends the
  request; submitting the same commit to the same target returns the rejection.
- After 5 requeues the request fails.

## Safety

- The target ref changes only through the compare-and-swap.
- A target checked out in any worktree, or held there by a paused rebase
  (including `--update-refs`) or a bisect, is not moved, because that checkout
  would be left stale. The request waits and says why. A `main` that stays
  checked out in the primary clone therefore never lands through the queue until
  it is detached or switched.
- Remaining race: a checkout made after that check and before `update-ref` is not
  detected. The compare-and-swap compares only the ref's value, so that worktree
  would be left behind the ref (`git status` shows the landed change reversed).
- Queue git commands run with hooks, signing and rerere off, as
  `Mast MergeQueue <merge-queue@mast.invalid>`.
- While unfinished, the tested and merge commits are pinned under
  `refs/mast/merge-queue/<requestId>/`. Scratch worktrees live under the queue's
  root, and one with a cancelled build is removed only after the build ends.
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
- Decide who owns queue build jobs before wiring `submit`: a job belongs to the
  request's owner chat, and archiving that chat cancels its queued and
  awaiting-approval jobs, which fails the land.
- A remote build host needs the merge commit pushed to it before its slot can
  check it out; the queue builds on a host that sees the local repository.
