# External runs: design note (R2)

Status: R2a (the durable record, its two tools and the Tracker list) is implemented on `david/external-runs`. R2b (the poller, observed state and derived run states) is implemented on `david/external-runs-r2b`; see "As built" at the end of the R2b design revision for where it differs from the design below.

An external run is work that runs outside a local harness. A GitHub Codespace is the first kind. Today the facts about such runs live in scheduled tasks, local files and chat history, and they disagree. R2 gives a work item a durable record of each run, and shows what the hosting device can observe about it.

## What this note settles

The coordinator's position has five parts: split durable from observed state, use a separate collection, add two MCP tools, run a read-only poller on the owning device, and cover the sync cases in tests. This note agrees with all five and adds the objections and consequences below. The poller question is answered: use the user's own `gh` login on the hosting Mac, read-only. The poller never starts, stops or deletes a box, never logs in, and shows "unknown" when auth has expired.

## Decisions (David, 2026-11-21)

Recorded from the answers to the open points below.

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

An in-memory cache in the engine, like `pr_ticket_cache`. It holds box state, capacity, exit code, idle seconds and `observedAt`. It is exposed through an RPC and merged into the Tracker in the UI. A poll tick never writes a registry row.

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

Review findings folded in (a read-only review against `crates/doc` and `docs/internal/registry-sync.md`):

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

## R2b design revision (2026-11-24)

Goal, from David on 2026-11-24: cheap in CPU, in `gh` calls and in agent tokens. Everything below sits inside the Poller section above and does not loosen its limits: read-only, David's own `gh` login on the hosting Mac, never start, stop, delete or log in, observed state device-local, and no tick ever writes an `externalRuns` row.

### Tick cadence, adaptive interval, backoff

A run is live when `expectedAlive` is true, `endedAt` is unset, its item is not archived, and it has updated within the stale-days setting.

- **Idle (no live run):** no timer runs and no call is made. The engine wakes the poller on a durable change to a run (it already sees the outline marker move). Capacity for the header is read lazily: a view of the header or a `list` call with observed state refreshes it only if the cache is older than 10 minutes (at most 6 calls per hour, and only while someone is looking).
- **Live:** base interval 180 s. For 5 minutes after the poller starts, after a registry change to a live run, or after any observed transition, the interval is 60 s, then it relaxes to 180 s.
- **Skip a tick when nothing changed:** the poller compares a hash of the list body with the last one and skips the parse and the box update when they match. If the set of live runs is unchanged and the list is unchanged, the tick ends there with no probe unless a per-box probe is due (below). `gh api` reports a 304 as a failed call, so production sends no `If-None-Match`; the runner trait still carries the `ETag` for a runner that can use it.
- **Backoff on failure:** the interval doubles per consecutive failure, up to 15 min, with ±10% jitter, and resets on the first success. A 429 or a rate-limit 403 honors `Retry-After` when given, else waits 15 min; any other 403 is Forbidden ("check the codespace scope") on the flat 15 min. A missing, unauthenticated or expired `gh` makes every live run "unknown: gh not signed in" and retries at a flat 15 min. It never escalates and never tries to log in. The poller also treats a wall-clock gap of more than 2 intervals (laptop sleep) as one immediate tick, not a burst.
- **Probe cadence:** per box, at most one SSH probe every 300 s while its run is live, and one immediately when a box first reports Available. A probe failure backs that box off 5, 10, 20 min and shows "unknown: probe failed" for that run only. A stopped box is never probed.

### Call budget per hour

| Case | `gh api user/codespaces` | SSH probes | Notes |
| --- | --- | --- | --- |
| Idle, no live run | 0 (≤6 if the header is viewed) | 0 | no timer |
| One live run, steady | 20 (≤30 in an hour that includes a 5-minute fast window) | ≤12 | one box |
| Five live runs, steady | 20 (≤30 with a fast window) | ≤60 (12 per box) | the list call is one call for all boxes |

The list call count does not grow with runs. Only SSH grows, linearly in Available live boxes, and only on the 300 s probe cadence. Measured in tests with call counters (see Tests), not assumed.

### One SSH command per box, and the wake race

One command per box per probe, with a fixed read-only script piped on stdin: `gh codespace ssh -c <name> -- sh -s`, with a 10 s connect timeout and a 20 s overall timeout. The script prints `key=value` lines: for each run directory configured for that box, whether the exit file exists and its content, the tmux session present or not, the mtime of the last output file. It runs only `test`, `cat`, `stat`, `tmux has-session` and `tmux ls`; it has no write, kill or start verb. The directory and tmux names come from Space settings (additive fields), with no company-specific defaults. Every probe for a box that has several live runs is the same single command.

Wake race. `gh codespace ssh` starts a box that is not running, so:

1. Probes run in the same tick as the list call, within seconds of it. If more than 30 s passed since the list response (a slow tick), the box state is re-read with one `gh api user/codespaces/<name>` before the probe.
2. A box whose `last_used_at` plus idle timeout leaves under 90 s is not probed that tick. It is the box most likely to idle out between read and connect.
3. A probe that finds the box no longer Available (connect failure with a "stopped" reason) is recorded as the box state, not as a failure.
4. The residual window of seconds stays and is documented in the user-facing docs, as decided on 2026-11-21.

### On-disk cache

`<data_dir>/cache/external-runs-observed.json`, beside `pr-merged-wakes.json`. One file per device, device-local and never synced, holding every Space's runs by run id. Written atomically (temp file and rename), only when contents changed, at most once per 30 s, and a state transition flushes at once. Shape:

```json
{"v":1,"savedAt":1764000000,
 "etag":"W/\"…\"","capacity":{"running":3,"cap":5,"total":6,"at":1764000000},
 "boxes":{"<codespaceName>":{"state":"Available","lastUsedAt":1764000000,"idleTimeoutMin":30,"at":1764000000}},
 "runs":{"<runId>":{"state":"running","since":1764000000,"seenAt":1764000000,"why":"tmux up, no exit file","exitCode":null,"idleS":42}},
 "notified":["<runId>|done|1","capacity|breach|1"]}
```

Loading restores facts, not freshness: terminal facts (an exit file seen, an exit code) are kept, because without them a box that idled out reads as orphaned. Every non-terminal run shows "unknown: not yet observed" until the first tick. Runs that ended more than 7 days ago and boxes absent from the last list are pruned on write. The in-memory cache is the source for all reads; the file only seeds it and the `notified` set. A corrupt or version-mismatched file is ignored and rewritten, never an error.

### Agent-facing summary

`work_item_external_run_list` gains one compact `obs` object per run and a one-line `capacity` string, on by default, with `observed: false` to turn it off. No logs, no raw box JSON. Fields: `state` (`running`, `done`, `failed(N)`, `orphaned`, `stalled`, `needsHuman`, `unknown`), `for` (time in that state), `seen` (age of the observation), `why` (one short reason, at most 60 characters). Example, 151 characters, about 45 tokens:

```json
{"capacity":"3/5 running, 6 boxes","runs":[{"tag":"r2b-claude-1","obs":{"state":"running","for":"2h10m","seen":"40s","why":"tmux up, no exit file"}}]}
```

Each further run adds about 30 tokens, so five live runs stay near 170. A test asserts the payload for five runs stays under a 220-token budget, estimated as characters divided by 3.5, and prints the figure. Detail (exit code, idle seconds, the `gh` problem) is behind `observed: "full"` only. The Tracker reads the full record over the RPC, not through this tool.

### Which transitions wake a parent, and dedupe

Wake the run's `ownerChatId` if it is an orchestrator, else the item's orchestrator, through the existing batched `OrchestratorWakes`, as one new event kind `ExternalRunChanged` that follows the chat's master wake switch only (like `QueueReady`), with no per-kind setting.

Wakes: running → done, running → failed(N), running → orphaned ("no exit seen"), a capacity breach (running reaches the cap), and unknown held for 15 continuous minutes (once, not per flip). No wake: every tick, running → running, unknown flicker shorter than 15 min, restart-time "unknown", a started box coming up, a recovery from capacity breach (shown in the header only).

Dedupe, by the same reserve-then-send pattern as `pr_merged_wakes`: a key `<runId>|<transition>|<episode>` is written into the cache file's `notified` set before the wake is sent, so a crash replays at most one wake and a restart never repeats one. A transition fires only from a persisted non-unknown prior state, so the first tick after a restart cannot storm. Orphaned needs the Shutdown state in two consecutive list reads before it wakes (one tick later at the 60 s fast interval). The capacity key's episode counter increments each time running drops below the cap, so one breach is one wake. A reopened run starts a new episode.

### Cost measurement

The poller keeps counters (list calls, 304s, SSH probes, failures, backoff level, tick milliseconds) behind a health entry and a debug RPC. Tests assert the budget table from them, and one test runs 50 boxes and 10 runs through a tick to record CPU per tick and the allocation of the payload. Real-world numbers come from those counters after a day of use and go in the release notes.

### Test plan

No test calls GitHub or touches a real box.

- **Seams:** a `GhRunner` trait with two methods, `list_codespaces(etag)` and `probe(name, script)`. Production shells out; tests inject a fake. The production runner is not constructible under `cfg(test)`, so a test that forgets the fake fails instead of calling `gh`. A second test drives the real runner's command builder against a fake `gh` shell script on a temp `PATH` that records argv, to pin exact arguments.
- **Fake `gh`:** a scripted timeline of box states, 304s, auth failure, 429 with `Retry-After`, slow probes and exit files. It records every call. A test asserts the recorded verbs are only `api user/codespaces`, `api user/codespaces/<name>` and `codespace ssh` with the fixed script, never `start`, `stop`, `delete`, `create` or `auth`.
- **Derivation:** a pure proto function with table tests: Shutdown with no exit seen is orphaned, with exit 0 is done, exit N is failed(N), Starting, Shutting Down, Queued, Unavailable are never orphaned, unauthenticated `gh` is unknown and never orphaned, first tick after restart is unknown, persisted exit file survives a restart.
- **Cadence and budget:** virtual clock tests for idle (zero calls), one live run and five live runs over a simulated hour, matching the table, plus the fast window, relax, backoff doubling to the cap, flat 15 min for auth failure, jitter bounds, and the sleep gap.
- **Wake race:** a box at under 90 s of idle timeout is not probed; a stale list triggers one re-read; a connect failure becomes a box state.
- **Batching:** two live runs on one box produce one `ssh` call.
- **Wakes:** each listed transition wakes once; ticks, flicker, restart and recovery do not; a replayed tick, a crash between reserve and send, and a restart do not double-wake; capacity breach episodes; reopened run.
- **Cache:** round trip, atomic write, write-on-change only, corrupt and version-mismatch files ignored, terminal facts kept and freshness dropped, pruning.
- **Durable invariant:** no tick writes an `externalRuns` row (reusing the R2a registry test harness and asserting zero registry ops).
- **Payload:** the compact summary size budget above, `observed: false` omits it, `observed: "full"` adds detail.

### Decisions for David

1. **New wake kind `ExternalRunChanged`** (a proto change beside `QueueReady`). Recommendation: yes. The alternative, reusing `WorkerReport`, would mislabel the event in the Team tab.
2. **The cap of 5** is a Space setting with default 5, since the `gh` list does not report it. Recommendation: yes.
3. **`obs` on by default in the existing list tool** rather than a separate tool. Recommendation: yes, it saves a round trip and is about 30 tokens per run.

### As built

Where the implementation differs from the design above, as approved by David:

- The list sends no `If-None-Match` in production. `gh api` reports a 304 as a failure, so the body-hash comparison is the only skip.
- The cache file has no `spaceId`. Run ids are content hashes of the item and tag, so one file serves every Space on the device.
- The probe script reports no uptime, and no last tool is recorded or shown.
- `GhRunner` has a third method, `read_codespace`, for the wake-race re-read. It is also the check on a stopped-looking probe failure: the poller records the state that read returns, never the message text.
- A run with a live tmux session and no output for 25 minutes reads as stalled.
- The poller starts at runtime only, from the engine on the owning device, never under a test process.
- The optional `boxLimit` setting shows as "6/15 boxes" and never wakes.
- There is no settings editor for the capacity cap, box limit, stale days or probe conventions yet; that is a follow-up.
- A probe can count as box activity and delay idle shutdown. This has not been verified on a real box, and the probe cadence is unchanged.
- A capacity refresh for an idle Space waits out the same backoff as a tick, so a signed-out, missing or forbidden `gh` allows one list call per flat 15 minutes while the Tracker is open. Only a 429 or a rate-limit 403 reads as rate limited; any other 403 reads "gh forbidden: check the codespace scope" and retries at the flat 15 minutes.
