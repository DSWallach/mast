# External runs: design note (R2)

Status: proposal for review, no code yet. Branch `david/external-runs`, based on `david-rolling` at 4cbd199c, which contains R1 (4866180d).

An external run is work that runs outside a local harness. A GitHub Codespace is the first kind. Today the facts about such runs live in scheduled tasks, local files and chat history, and they disagree. R2 gives a work item a durable record of each run, and shows what the hosting device can observe about it.

## What this note settles

The coordinator's position has five parts: split durable from observed state, use a separate collection, add two MCP tools, run a read-only poller on the owning device, and cover the sync cases in tests. This note agrees with all five and adds the objections and consequences below. The poller question is answered: use the user's own `gh` login on the hosting Mac, read-only. The poller never starts, stops or deletes a box, never logs in, and shows "unknown" when auth has expired.

## Decisions (David, 2026-11-21)

Recorded from the answers to the open points below. Work on R2 is paused until the coordinator resumes it.

1. **Drop `prUrls`.** Yes. A run's PRs are derived from the item's existing GitHub PR references.
2. **Do not sync observed state.** Observed state stays device-local in R2.
3. **Update the docs for the wake race.** Yes. The user-facing docs that describe the poller say that `gh codespace ssh` starts a stopped box, that the poller re-reads state first, and that a box idling out in that window can be woken briefly. This lands with the poller (R2b), not before.
4. **Label "orphaned: no exit seen".** Accepted.
5. **Split R2 into R2a and R2b.** Accepted.

Poller auth, decided earlier: the user's own `gh` login on the hosting device, read-only, never start, stop or delete a box, never log in, "unknown" when auth has expired.

## Durable record (synced)

A registry kind `externalRuns`. It uses the generic registry protocol, which already accepts new row kinds without a server change (`docs/internal/registry-sync.md`). Rows carry `spaceId` and `v` like the other outline kinds.

- **Row id** is content-derived from `(workItemId, runTag)`, NUL-separated and hashed, the same way `external_reference_id` works. Two clients registering the same run converge on one row, and a replay is a no-op by HLC.
- **Fields** (each last-writer-wins by field clock): `workItemId`, `kind` (open string; unknown kinds are kept and shown as-is), `runTag`, `target` as `codespaceName` and `displayName`, `provider` (claude or codex), `sessionId`, `branch`, `ownerChatId`, `createdAt`, `updatedAt`, `expectedAlive`, `endedAt`, `outcome` (done, failed, abandoned).
- **No `prUrls` field** (objection 1). The run's PRs are the item's existing GitHub PR references.
- **Where it sits:** the kind is added to `WORK_OUTLINE_KINDS`, so a Space delete cascades to it and the orphan sweep covers it. The snapshot gets an additive `externalRuns` list with a serde default, so older readers and old snapshots decode unchanged.
- **Moving an item keeps its runs** (the row names the item, not a path). **Archiving an item leaves its runs readable** and stops polling them.

## Observed state (device-local, never synced)

An in-memory cache in the engine, like `pr_ticket_cache`. It holds box state, capacity, exit code, idle seconds, last tool and `observedAt`. It is exposed through an RPC and merged into the Tracker in the UI. A poll tick never writes a registry row.

## MCP tools

- `work_item_external_run_upsert`. Identity is `(item, runTag)`. The engine reads the current row and writes only the fields the caller supplied, so two callers updating different fields merge instead of overwriting. An optional `idempotencyKey` covers retries of the same call. Ending a run is the same call with `expectedAlive: false`, `endedAt` and `outcome`. Once `endedAt` is set, setting `expectedAlive` back to true requires `reopen: true`.
- `work_item_external_run_list`, read-only.
- Scope: an agent may touch only items it can already touch through the other work-item tools. The exact check is named when coding starts.

## Poller

Runs only on the Space's owning device, only for runs with `expectedAlive` true and an item that is not archived. It also skips runs with no update for a configurable number of days, so a forgotten run does not poll forever.

1. One `gh api user/codespaces` call per tick for all boxes: name, state, `last_used_at`, idle timeout, and capacity (running count against the cap, total boxes).
2. Only for a box that call reported as Available, a read-only probe of the run directory and the tmux session, through the user's `gh` SSH access. The run-directory and tmux conventions are configuration (Space settings, additive fields), with no company-specific names.
3. Never start, stop or delete a box, never log in. If `gh` is missing, unauthenticated or unreachable, every run shows "unknown" with the reason, never "orphaned".

Derived states: running, done, failed(N), orphaned, stalled, needsHuman. The derivation is a pure function in proto with unit tests, like R1's `WorkStatus`. `needsHuman` reads the open Required Action count R1 already computes.

## Objections (all decided; see Decisions above)

1. **Drop `prUrls`.** The registry merges per field, last writer wins. A whole-array field loses one of two concurrent additions. The item already carries GitHub PR references, which are separate rows and merge cleanly. If per-run PRs are wanted later, add them as their own rows.
2. **Observed state is invisible on other devices.** A device that does not host the Space sees the durable record and "not observed on this device". That is a real limitation of keeping observations local. Option for later: the owning device writes one coarse `lastKnownState` row per run only when the derived state changes, which is a few writes per run and not per tick. Recommendation: not in R2.
3. **"Never wakes a box" has one residual race.** `gh codespace ssh` starts a box that is not running. The poller re-reads state immediately before probing, but a box that idles out in the seconds between that read and the connection could be woken. The window is seconds, and the box idles out again on its own. Say so in the user-facing docs rather than claim an absolute.
4. **"Orphaned" means "no exit seen".** A Shutdown box is never probed, so the poller knows an exit file only if an earlier tick saw it. A run that finished and then idled out between two ticks looks the same as one killed mid-run. The label is "orphaned: no exit seen", with `last_used_at` as supporting evidence. Resuming to verify is R3.
5. **Orphaned needs state exactly Shutdown.** Starting, Shutting Down, Queued and Unavailable map to "unknown" or running-like states, not orphaned, so a transient state never raises a false alarm. The first tick after an engine restart has no observations and shows "unknown".
6. **Writes into `externalRuns` bump the outline marker.** The kind joins `WORK_OUTLINE_KINDS`, so each durable write makes the Space re-materialize (R1's dispatcher watcher and the UI both key on it). Durable writes are rare: register, end, field edits. The invariant to keep is that no poller tick ever writes this kind. A test asserts it.
7. **Release split.** R2a: the durable kind, snapshot field, the two tools, and a Tracker list of runs with durable fields only, no polling. R2b: the poller, observed cache, derived states, capacity header and health entries. R2a replaces the scattered records on its own, and R2b carries the Orphaned acceptance case. Recommendation: ship them as two releases.

## Upgrade and sync cases

- **Older client, new rows.** The generic protocol stores and forwards rows of a kind it does not know. Typed reads ignore them. No older client reads `externalRuns`.
- **Older client deletes a Space.** Its cascade does not know the new kind and leaves the rows. A newer client's orphan sweep, which is gated on a synced registry, tombstones them.
- **Dangling rows.** A run whose item is missing is hidden by readers and removed by the sweep. Readers must tolerate it.
- **Offline writes.** Pending ops replay over server state, as for every kind. A registered run created offline converges on the same row id when it syncs.
- **No old builds in practice**, so this is cheap wire compatibility, not a migration.

Review findings folded in (Haiku, read-only, against `crates/doc` and `docs/internal/registry-sync.md`):

- **Enforce `spaceId` on write.** The cascade and the orphan sweep find rows only by their `spaceId` field, and nothing in the registry rejects a row without one. The upsert must refuse to write a row with no `spaceId`.
- **Filter explicitly when materializing.** Rows of other kinds must never appear in the snapshot's `externalRuns`, and a run whose item is missing is dropped there, not left to the reader.
- **Not verified: the sync server.** The server sources (`registry-room.ts`, `registry-core.ts`) are not in this checkout, so whether the deployed server has any kind allowlist is unconfirmed. The client protocol has none (the row kind is a plain string). Before R2a ships, confirm against the deployed server by round-tripping a test row of the new kind between two engines.

## Tests

- Idempotent upsert, and replay of the same call from two clients, converge on one row.
- Two callers updating different fields both survive; same-field writes resolve by clock.
- A fresh client after a restart sees the runs. An unknown extra kind round-trips and leaves the outline inputs unchanged.
- Move keeps runs. Archive keeps them readable and stops polling. Space delete and the orphan sweep remove them.
- Fake `gh` and probe runner: a box that goes to Shutdown mid-run gives Orphaned within one tick, and the runner records no start, stop or delete call. Unreachable or unauthenticated `gh` gives "unknown", never "orphaned".
- No poller tick writes an `externalRuns` row.
- An upsert without a `spaceId` is refused. A row of an unknown kind never appears in the snapshot's `externalRuns`.
