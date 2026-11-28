# Model fit

`crates/engine/src/model_fit/` records how each model does on each kind of task
and picks the cheapest model that is still allowed to take one (design plan v3
§6.1 and §6.2). It is a library only: task launch and `swap_worker` do not call
it yet, floors are not synced between devices, and there is no UI.

## Task class

`TaskClass` (from `mast_proto`) is a role, a domain and a complexity. The domain
comes from the owned or changed paths (`classify_domain`). A glob reaches every
area that agrees with the text before its first wildcard, so `crates/**` counts
as engine code. A task that spans several areas takes the hardest one, in this
order from easiest: docs and config, mechanical refactor (any other path, or
none), MCP or CLI, GPUI, harness protocol, proto or doc schema, engine
concurrency state.

`ModelTier` `Small`, `Medium` and `Large` are the design's Tier 1, 2 and 3.

## What counts against a model

| Result | Effect |
| :--- | :--- |
| Clean pass | Counts as a passed trial. |
| Compile or test gate failure, contract or scope violation, loop or budget thrash, parent quality escalation | Counts as a failure. |
| Substantive review rejection | The first on a task is ignored; later ones by the same model on that task count, even if the task's class changed in between. |
| Quota or rate limit, auth expiry, disk hold, peer disconnect | Never counts. |
| A countable failure with peak context above the tier's ceiling | Context overload: the ticket or slice was too big. Does not count. |

The ceilings are the forced session-swap points used by context packets: 60% of
the window for Tier 1, 78% for Tiers 2 and 3. Unknown occupancy is not an
overload. Results that do not count leave the failure streak as it was.

## Demotion and floors

All state is per task class.

- A counted failure demotes the model when it is the second in a row, or when
  there are at least 3 trials and the failure rate is at least 0.45.
- Demotions last 7, 14, 28 and then 30 days. A demotion clears the evidence, and
  results that arrive while it runs are not collected, so the next one needs new
  failures.
- The failure rate is a plain average for the first trials, then an EWMA with
  weight 0.3. It halves every 14 days without new evidence.
- Other evidence expires after the same 14 days: only trials from that window
  count towards the 3, and a failure more than 14 days after the previous one
  starts a new streak.
- The first time a model shows a new version, its failure rate, trial count,
  demotion level and remaining demotion are halved, and its streak is cleared.
  Returning to a version seen before does nothing.
- Where that shortens a demotion that helped raise a floor, the floor's canary
  cooldown shrinks to match: it ends with the last such demotion, or at once if
  none is still running. The floor itself stays.
- Two models of one tier demoted at the same time raise the class's floor to the
  tier above.

## Routing

`ModelRouter::select` returns the cheapest `(tier, token cost)` candidate that is
at or above the floor, is not demoted, has the required capabilities, has quota
headroom and has a context window that holds the packet. `RouteRequest::min_tier`
raises the floor for one selection, for swapping a worker up a tier. When nothing
qualifies, the error gives a reason for every candidate.

Quota comes through the `Headroom` trait, and time and randomness through
`Clock` and `UniformRng`.

### Pins

`RouteRequest::item_pin` and `project_pin` (the item's wins) name a model the
router must use. A pinned selection returns that candidate as is: the floor,
demotions, quota headroom, `min_tier`, `critical` and the canary dice are all
ignored, and the ledger is neither read nor written, so a pin is never demoted,
escalated or probed. A pin missing from the candidates is
`RouteError::PinnedModelUnavailable`; the router never substitutes. The caller
supplies the pins; the work-item proto has no model-pin field yet.

`RouteRequest::experiment_pinned` mirrors `WorkItemTaskSpec::experiment_pinned`
(the `S`/`V` scorecard items). Such a request needs a pin
(`RouteError::ExperimentWithoutPin` otherwise), so it never gets a canary, an
auto-escalation or a model change. Recording its outcomes into the ledger is the
caller's to skip.

### Canary probes

Floors come down only through canaries. A selection may probe one tier below the
floor when the demotions that raised it have run out, the task is not critical,
and an otherwise eligible model there has used less than half its quota cap. One
such selection in ten is a probe. The caller records its outcome with `canary`
set. Two clean probes in a row lower the floor one tier. A probe is clean when it
passes on a task that review never rejected. Any other result restarts the count,
including a context overload or a first review rejection, except an
infrastructure failure, which changes nothing.

## Storage

`<profile_store>/model-fit.sqlite3`:

- `model_outcomes`: append-only, one row per observation. `outcome_id` is unique,
  so recording an observation twice has no further effect.
- `model_fit_aggregates`: current state per task class and model.
- `task_class_floors`: floor, canary cooldown and clean-probe count.
- `floor_raisers`: the demotions behind each floor's cooldown.
- `model_versions`: versions each model has been seen running.

A database written by a newer schema version is refused.
