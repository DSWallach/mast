# Task checkpoints and credit fallback order

Code: `crates/engine/src/task_checkpoint.rs`, `crates/engine/src/credit_fallback.rs`.

## Task checkpoint

One commit under `refs/mast/wip/<itemId>`, parented on the worker's HEAD. The tree is the
working tree as `git add -A` sees it, built in a copy of the index, so the worker's HEAD,
index and files do not change. Metadata (item, base SHA, branch, chat, account, harness,
model, reason, creation time, note of at most 500 characters) is one JSON line in the commit
message. Repository hooks do not run.

- An item id that is not a plain ref component is cleaned and suffixed with a hash.
- One ref per item; the newest checkpoint wins. Same tree on the same HEAD writes nothing.
  The same tree on a new HEAD writes again, so restore never drops the worker's commits.
- A replaced or pruned commit is unreachable and git may collect it later.
- `restore(repo, item, dest, branch)` adds a worktree on a new branch: HEAD is the base
  commit, files equal the checkpoint, saved changes are staged.
- `list` and `prune` cover `refs/mast/wip/` refs that carry the metadata. Nothing prunes
  automatically yet.
- Not saved: ignored untracked files, an index-only version of a file that was edited again
  after staging, the files inside a submodule or nested repository, and a merge or rebase in
  progress (conflicted files keep their markers; `MERGE_HEAD` and rebase state are lost).
- A save that writes nothing keeps the earlier record, so chat, account, model, reason, note
  and time describe the stall that first saved this state, not necessarily the latest one.
- Limits (`Limits::default`): 60 s for the whole save, after which git and its helpers are
  killed; at most 5,000 untracked files and 256 MiB of them. Hitting one saves no ref.
- `restore` needs an absolute folder and resets to the commit's real parent.
- Untracked files that are not ignored are saved, whatever they hold. The ref is local.

## Fallback order

1. Another login of the same harness (Claude Code, Codex) with headroom above zero for
   the model that hit the limit: a signed-in managed account, or the Default login when its
   headroom is known. The successor chat is pinned to it before launch.
2. Per-model Claude limits only: one model down in place, unless the account's headroom for
   that model is known to be zero or less.
3. Another harness: the highest known headroom (its Default login or a managed account),
   else the first with unknown headroom in the old order (Claude first). A harness whose
   Default login is known to be exhausted, with no account that has headroom, is skipped.

A lineage never returns to an account of the current harness it already ran on, nor to
another harness it already used. Before a successor starts, the stalled chat's worktree
(the worktree the agent bound last that still exists, else its execution root; the Changes
pane's selection is ignored) is checkpointed once, and the commit and checkout path are
named in the successor's prompt. Re-binding an earlier worktree does not make it the latest.
A failed or over-limit checkpoint is recorded as `none` and does not block the successor. A
repository rooted at the home folder is never checkpointed. Manager fallback chains are
unchanged.

## Wiring a quota source

`credit_fallback::start` uses `UnknownHeadroom`, so behaviour matches the order used before
this change (step-down, then Claude-first successor). To enable steps 1 and 3 by quota,
call `start_with_headroom` from `lib.rs` with a `Headroom` implementation:

- `accounts(harness)`: ids of managed accounts that are signed in
  (`ManagedAccounts::snapshot`, `signed_in`).
- `headroom(account, model)`: share left up to the user's cap; `None` when unknown.
  `account_id: None` is the Default login.

`proactive_drain(used, hard_cap)` answers whether to finish the turn and then swap. Nothing
calls it yet.

## Not done here

- The `swap_worker` tool and the `reusable_chat` account/model pin fix.
- Hydrating checkpoints into `AutomaticHandoff`.
- A successor, including one on another account of the same harness, starts on the default
  model: `create_credit_successor` clears the model, reasoning level and model options.
  Keeping them needs a change in `work_outline_launch.rs`. Before a quota source is wired,
  fix this together with what headroom is judged on: step 1 asks about the stalled model and
  step 3 about the harness default model, and either may differ from the model launch uses.
  Step 3 also judges "unknown" on the Default login, while a successor chosen that way is
  not pinned and runs on the project's default account.
