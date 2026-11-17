# Startup perf pass, 2026-11-17

Soaks: `scripts/perf-soak.sh target/installed/mast --baseline <build of branch point> --real`, 180 s, a `.backup` copy of the
real data (~44 checkouts). Real checkouts change between runs, so compare within a run.

| Run | git spawns total (cand / base) | first 60 s | after 60 s | footprint MB (cand / base) | CPU % (cand / base) |
| --- | --- | --- | --- | --- | --- |
| 1 | 280 / 332 | 170 / 237 | 110 / 95 | 483 / 487 | 4.0 / 4.2 |
| 2 | 315 / 428 | 213 / 283 | 102 / 145 | 461 / 411 | 4.0 / 4.4 |

Baseline-only run before the change: 385 spawns (247 in the first 30 s): 150 `diff`, 142 `status`, 78 `rev-parse`; 163 of the 385
were exact repeats of an earlier command.

## Fixes
1. diff-sync first sync per checkout: HEAD read from the git dir (no `rev-parse --verify HEAD`), and the capture reuses the probe's
   `status --porcelain=v2` (converted to v1) instead of a second `status`. 6 → 4 spawns per checkout (72 → 51 on 12 checkouts in
   `diff_sync_startup_budget`). Falls back to the old calls when HEAD is unborn/unreadable or the status is truncated/unparsable.
2. Artifact watcher (every 2 s) decoded every artifact row to stat its file; it now caches `(id, path)` per artifact-rows generation
   and decodes a row only when its file moved (`idle_watcher_polls_do_not_re_decode_every_artifact`).
3. `pr_ticket_cache`: a "no PR" result stays fresh 10 min instead of 3 min. A miss costs one `gh pr view` per submodule too.

## Findings, not changed
- Steady spawns (~40-55/min) are the 120 s repair tick: one `status` per checkout (~44 per tick). Idea: skip or stretch the probe for
  checkouts with healthy attached watchers and no chat viewing them; needs a decision on how much missed-FSEvent recovery delay is OK.
- The post-attach re-probe is a second `status` per checkout at startup (~44). Skip it when the initial probe already started after the
  watchers attached.
- Submodule metadata does `config --get-regexp` + `rev-parse --show-toplevel` per submodule per capture (~15 spawns per 180 s).
- gh: the sweep is already capped (3 keys per 20 s, 1 detail, 2 searches per tick), so there is no startup burst; the soak stubs gh and
  has no UI, so real gh volume is UI-demand driven and was not measurable. Batching to one `gh pr list` per repo is the bigger win
  but changes the lookup semantics (branch → PR matching), so it is not done here.
- Memory: footprint 445-487 MB after the first minute, peak 530-680 MB during startup, against a 247 MB registry file. Nothing
  obviously lazy-loadable found without allocation counters; the artifact watcher decode churn is removed.
