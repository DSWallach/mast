# Living documents (engine core)

`crates/engine/src/living_docs/` tracks which documents a project keeps current,
which are owed an update and which no longer match the code. It calls no model.

**Not wired yet.** No land step, hand-off or tool calls it. Rows are stored
engine-side only; syncing them into the project document inventory is deferred.
The engine types share the JSON shape of the row contract in
`mast_proto::project_document_inventory` and convert into it, but read unknown
enum values (an unknown status reads as pending review, never fresh).

## Rows and storage

A row is one document, `repo:<path>` or `artifact:<id>`, with its class, owner
item, watch globs, last seen hash and status: fresh, owed update, drift
detected or pending review. `LivingDocStore::open(dir)` keeps every project's
rows, exempt receipts and drift findings in `dir/living-docs.json`. Writes are
atomic. A file that does not parse is reported and left in place.

`seed_rows` adds a row for each rule in `default_seed_rules` whose document
exists (this repository's code-to-doc rules, in `rules.rs`).
`merge_blast_radius` adds a plan's path-like blast-radius surfaces to the
plan's own row.

## Staleness on land

The land step calls
`LivingDocStore::on_landed(project, commit_sha, changed_paths, blob_hash_of)`
and gets the rows whose status changed. `blob_hash_of` answers for the tree
after the commit: a repo path for repo rows, the whole `artifact:<id>` for
artifact rows.

- A repo document the commit lists, with a new hash, is verified fresh at that
  commit. An artifact is when its hash moved since it was last recorded.
- Otherwise, a changed path under a watch glob makes the row owed.
  `owedSinceCommit` keeps the earliest such commit. Detected drift keeps its
  status.
- `record_exempt(project, doc_id, commit, reason)` suppresses that debt for one
  document and commit. Record it **before** the land, with the SHA that will be
  passed as `commit_sha`. It does not clear earlier debt.
- A commit that deletes the document marks it drifted.
- Applying a commit again right away changes nothing.

## Structural drift check

`LivingDocStore::check_drift` (after `on_landed`) reads each Markdown row,
except changelogs, with `check_markdown`. The caller supplies the known MCP
tool names and `mast` subcommands (`daemon install` form checks nested ones;
include hidden commands). An empty set turns that check off. It flags:

- a relative link, or a code span under a top-level repo directory, naming a
  file that is gone, or a line past its end;
- a snake_case span in one of the `TOOL_PREFIXES` families, or any
  `mcp__mast-agent__<tool>` span, that is not a known tool;
- a `mast <command>` span naming an unknown command.

Fenced blocks, prose, placeholders and globs are ignored. Renamed tools outside
those prefixes are missed on purpose: broader prefixes match ordinary
identifiers. Pass a `RepoTree` over the landed tree; `FsRepoTree` reads a
checkout. A row that checks clean again returns to owed or fresh. A document
update clears drift until the next check.

## Hand-off digest

`build_doc_ledger_summary(pins, rows, resync_job)` returns the canonical pins
and every row that is not fresh or has pending deltas (drift first, then owed,
review and delta-only rows, each by id), plus text of at most 1536 bytes that
counts any rows it leaves out.
