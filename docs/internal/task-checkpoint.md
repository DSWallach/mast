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
  after staging, and the files inside a submodule or nested repository.
- Untracked files that are not ignored are saved, whatever they hold. The ref is local.

## Fallback order

1. Another signed-in managed account of the same harness (Claude Code, Codex) with
   headroom above zero for the model that hit the limit. The successor chat is pinned to it
   before launch.
2. Per-model Claude limits only: one model down in place, unless the account's headroom for
   that model is known to be zero or less.
3. Another harness: the highest known headroom (its Default login or a managed account),
   else the first with unknown headroom in the old order (Claude first). A harness whose
   Default login is known to be exhausted, with no account that has headroom, is skipped.

A lineage never returns to an account of the current harness it already ran on, nor to
another harness it already used. Before a successor starts, the stalled chat's worktree
(its selected bound worktree, else its execution root) is checkpointed once and the commit
is named in the successor's prompt. A failed checkpoint does not block the successor. A
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
  model: `create_credit_successor` clears the model. Keeping the stalled chat's model needs
  a change in `work_outline_launch.rs`.
