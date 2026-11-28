# Living documents (engine core)

`crates/engine/src/living_docs/` tracks which documents a project keeps current,
which are owed an update and which no longer match the code. It calls no model.

**Partly wired.** Automatic Manager hand-offs read the ledger (`PendingHandoffs::living_docs`, opened beside the pending-hand-off file, keyed by the Manager's scope item id) and put every row owed work in the successor prompt. `MergeQueue::set_land_hook(land::merge_queue_hook(store))` applies each landed commit; nothing installs it yet, because the engine does not construct a `MergeQueue`, and its `project_id` must equal that scope item id. No tool, manual hand-off, post-turn doc gate or 80%-context flush calls it. Rows are stored
engine-side only; syncing them into the project document inventory is deferred.
Rows, enums and the ledger snapshot are the contracts in
`mast_proto::project_document_inventory`.

## Proto follow-ups

- `LivingDocClass` and `DocDriftStatus` reject unknown values. The store
  replaces them while loading its file (unknown status reads as pending review,
  never fresh; unknown class as item), and the next write keeps the
  replacement. Syncing rows needs lenient enums on the wire types.
- `LivingDocRow` has no field for what the drift check found. Findings stay in
  the engine ledger, keyed by `docId`.
- Exempt receipts have no wire type and stay in the engine ledger.
- `DocumentationInventory` has no living-doc rows to sync into.
- `DocLedgerSummary` has no rendered text; `DocLedgerDigest` carries it beside
  the snapshot.

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

The land step calls `LivingDocStore::on_landed(project, commit_sha,
changed_paths, parent_hash_of, blob_hash_of)` and gets the rows whose status
changed.

- `changed_paths` must hold both sides of every rename, over the whole landed
  range: `git diff --no-renames --name-only base..landed`.
- The two lookups give a repo path's blob id at `base` and at `landed`.
  `Ok(None)` means no file. A failed lookup must return `Err`, which abandons
  the land with nothing changed; it must never read as a deletion.
- A repo document the commit lists, whose two blobs differ, is verified fresh
  at that commit. The row's recorded hash plays no part, so seeding before or
  after the land gives the same answer.
- Otherwise, a changed path under a watch glob makes the row owed.
  `owedSinceCommit` keeps the earliest such commit. Detected drift keeps its
  status.
- A commit that deletes the document marks it drifted.
- Apply each land once, in landing order. Only an immediate replay of the
  commit that last verified a row is ignored; replaying an older land can
  restore debt and an old hash.

## Artifact revisions

No commit lists an artifact, so a land applies only the watch rule to artifact
rows. Call `on_artifact_published(project, doc_id, hash)` when a revision is
published: a new hash clears the debt owed up to then. Watched code that lands
afterwards owes it again.

## Exemptions

`record_exempt(project, doc_id, commit, reason)` suppresses the debt for one
tracked document and commit. Pass the SHA that lands (what `on_landed` gets),
not the one a worker tested, and record it **before** the land. It returns
`false` when that commit already landed (the last 512 are remembered). It does
not clear earlier debt. Receipts are not pruned.

## Structural drift check

`LivingDocStore::check_drift` (after `on_landed`) reads each Markdown row,
except changelogs, with `check_markdown`. It reads files while holding the
store lock, so call it off async threads. The caller supplies the known MCP
tool names and `mast` subcommands (`daemon install` form checks nested ones;
include hidden commands). An empty set skips that check and keeps what it
found earlier. It flags:

- a relative link, or a code span under a top-level repo directory, naming a
  file that is gone, or a line past its end;
- a snake_case span in one of the `TOOL_PREFIXES` families, or any
  `mcp__mast-agent__<tool>` span, that is not a known tool;
- a `mast <command>` span naming an unknown command.

Fenced blocks, prose, placeholders, globs and missing files under `build/`,
`target/` or `node_modules/` are ignored. A span naming a file in some other
checkout, under a directory this repo also has, is still reported: write such
examples with a placeholder. A test keeps the seeded docs free of findings.
A prefixed span naming a Rust module of the tree (`work_outline_launch`) is
not a tool, so a removed tool that shares a module's name is missed. So are
renamed tools outside the prefixes: broader prefixes match ordinary
identifiers. The config-key check of design §8.5.1 is not included.

Pass a `RepoTree` over the landed tree; `FsRepoTree` reads a checkout. A
drifted row that checks clean returns to owed or fresh. A pending review keeps
its status and still gets its findings stored. A document update clears drift
until the next check.

## Blast radius

`merge_blast_radius` only adds globs. A surface dropped from a later plan
revision stays watched.

## Hand-off digest

`build_doc_ledger_summary(pins, rows, resync_job)` takes a `DocLedgerSummary`
holding the canonical pins. It returns a `DocLedgerDigest`: that snapshot with
every row that is not fresh or has pending deltas (drift first, then owed,
review and delta-only rows, each by id) and any re-sync job in flight, plus
text of at most 1536 bytes that counts any rows it leaves out.
Rows sharing a `doc_id` and status keep their input order; the store's seeding
never creates two rows with one id.
