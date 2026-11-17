# Engine perf pass 2, 2026-11-17

## 1. Repair-tick backoff (diff_sync.rs)

The 120 s repair tick probed every checkout with `git status` (~110 checkouts, 40-55 spawns/min).

- A fully watched checkout (root watcher attached) whose last 3 repair probes found nothing is probed
  every 5th tick (120 s -> 600 s). Probes are phase-staggered per checkout (hash of the checkout id mod 5),
  so ~1/5 of the idle checkouts probe on any tick instead of all of them.
- Reset to every tick on: any watcher-driven sync, a new chat in the checkout, a probe that sees a change.
- Never backed off: checkouts without a live root watcher (over the watch budget, or the watcher failed to
  attach), and checkouts whose probe cannot decide (submodules).
- Already skipped before this pass: checkouts whose chats are all archived. The engine has no notion of
  "visible in the UI", so visibility was not added.

**Detection bound for a change the watcher missed:** 120 s while the checkout is active or in its first
3 ticks; at most 600 s (`REPAIR_BACKOFF_STRIDE x REPAIR_INTERVAL`) once backed off, plus debounce and
probe time. Constants: `REPAIR_QUIET_TICKS_BEFORE_BACKOFF`, `REPAIR_BACKOFF_STRIDE`.

Budget test `crates/engine/tests/diff_sync_repair_backoff.rs`: 50 idle checkouts, 15 simulated ticks
(30 min): 233-235 spawns against a budget of 310 (unbacked: 750). A watcher-visible commit publishes with
no repair tick. With the watchers dropped (events never delivered), a HEAD change is published within the
5-tick bound (observed after 2 ticks, depending on phase).

## 2. Post-attach re-probe

The first sync probes (`git status`), captures, and after the watchers attach a second probe closes the
capture-to-attach gap. It is now skipped when a probe already started after attach.
`crates/engine/tests/diff_sync_attach_reprobe.rs`: with the first sync held for the attach, 8 checkouts
cost 24 spawns (3 each) instead of 32.

**Finding:** in practice attach loses the race. FSEvents registration took ~1-2.5 s per checkout in the
test (two at a time), longer than the first sync, so at startup the re-probe is still needed for most
checkouts. `INITIAL_SYNC_ATTACH_WAIT_MS` (default 0) can make the first sync wait for attach, at the cost of
delaying the first diff by the attach lag; left off. The skip is correct and free when attach wins.

## 3. Soak order bias

Same binary as candidate and baseline (`mast-a` vs a byte copy), real data:

| Run | order | footprint MB | peak MB |
| --- | --- | --- | --- |
| old script, cand then base | 1, 2 | 517, 512 | 618, 572 |
| ABBA: base1, cand1, cand2, base2 | 1..4 | 532, 534, 581, 588 | 710, 563, 654, 671 |

No ~150 MB first-run penalty: the candidate-first run was only 5 MB above the baseline. Run-to-run noise
with an identical binary is up to ~55 MB footprint and ~130 MB peak, with a rise over the four positions in
the ABBA run (not reproduced in the two-run order). The earlier 150 MB gap is therefore not explained by
order alone; it may be real difference between the candidate and installed builds, or data drift.
`perf-soak.sh` now defaults to base, cand, cand, base and averages each side (`--order ab|ba` keeps the cheap
two-run form), prints a per-run table, and keeps the stamp format.
