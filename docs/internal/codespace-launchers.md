# Codespace task launchers

`work_item_codespace_task` uses a launcher selected by the Space owner on the Space's host. `work_item_external_run_upsert` remains the tracking-only way to register an independently started run. Managed runs use the same external-run records and Tracker. Users and agents can use the existing poller without configuring a launcher.

## Host configuration

Create `codespace-launchers/<space-id>.json` under the engine's device-local store root. Keep the config, launcher, sources and transcript archive outside the project checkout. Config and journal files stay on the host. The registry receives the run identity and Required Action, and the approved worker receives its prompt through the launcher. Use a dedicated, trusted local executable. It inherits the host service account's environment and can use the configured provider's existing authentication. Mast does not copy credentials to a box.

Example configuration with illustrative paths:

```json
{
  "executable": "/local/launchers/my-codespace-adapter",
  "workingDirectory": "/local/launchers",
  "launcherSources": [
    { "path": "/local/playbooks/launcher-cli.md" },
    { "path": "/local/playbooks/orchestration.md" }
  ],
  "workerSources": [
    {
      "path": "/local/playbooks/orchestration.md",
      "startAfter": "## Worker instructions",
      "endBefore": "## End worker instructions",
      "unquote": true
    },
    { "path": "/local/playbooks/worker-review-policy.md" }
  ],
  "cleanupEnabled": false
}
```

Select the complete worker template and any applicable supplement, including the policy for handling human and automated review. A source can be an entire UTF-8 file or a region between exact lines. `unquote` removes one Markdown blockquote level. Mast passes the selected worker text in order, with the exact approved turn scope appended. Select the CLI and host-level playbook as `launcherSources` for the adapter. A missing file or marker blocks dispatch. The adapter can expand explicitly configured placeholders using the approved scope and live item references. Preserve the complete template after expansion.

Configure the existing Space poller for the selected CLI's actual run directory, exit marker, tmux session and progress file. Use a progress file updated during the run, such as the session transcript or a heartbeat. A final-output-only file is insufficient to detect a stall. Set the Space capacity and optional box limit alongside the poller configuration.

## Tool sequence

1. Select one eligible work item from the approved queue. Supply the verified scope, applicable live reference and resolved decisions in `scope`. Keep unresolved work as a Required Action.
2. Call with `mode: requestApproval` and an `action` of `launch`, `resume`, `stop` or `cleanup`. The user approves the exact operation in Required Actions. The approval binds the action, target, caller, scope, configured executable and source contents.
3. Call with `mode: execute` to queue the operation. Call with `mode: status` and the same inputs for its result. `inspect` uses `execute` and `status` without approval.
4. Reuse the same `idempotencyKey` and inputs on retries. For a new resume, stop or cleanup operation, supply a new key. A lost result can be recovered by calling `execute` again. An ambiguous operation must reconcile before another operation on that item can proceed.
5. Continue monitoring in Tracker or `work_item_external_run_list`. An exit marker indicates the worker ended. Inspect the actual PR, CI and evidence before treating the work as verified. If verification fails, use a separately approved resume with the same identity, including the latest full worker playbook and any resolved decisions. Stop after verification. For a closed-unmerged PR, open a Required Action and retain the box.

## Adapter protocol

The executable receives one JSON object on stdin and emits one JSON value on stdout. It must return after remote admission and identity discovery, without waiting for the worker. Mast queues the call on the host and imposes a timeout. Stdout is reserved for protocol data. The engine reports generic transport errors without logging the sources, stdin or stderr.

Every request carries `protocolVersion: 1`, `phase`, `action`, `operationId`, `runKey`, `spaceId`, `workItemId`, `ownerChatId`, `scope`, `launcherSources` and, for launch or resume, `workerPrompt`. It also carries `identity` when the run exists, and `review` and `transcriptReceipt` when relevant. The run key is stable per Space and work item. An operation ID is stable for the exact approved inputs.

| Phase | Adapter responsibility and response |
| --- | --- |
| `preflight` | Resolve the live reference, read its full discussion and dependencies, check settled scope, placement and the approved batch policy. Query capacity across the authenticated account, including starting boxes and boxes owned by other sessions. Return `{ready, runningOrStarting, totalBoxes, capacityCap}`. Fail closed on incomplete capacity or readiness information. |
| `launch` | Recover any box reserved for this exact run key before attempting a create. Create one fresh box for this item, without attaching. Persist its real name before sending the full prompt. Start a detached worker with a unique tag. Return `{codespaceName, runTag, provider, sessionId}` from the real run. |
| `resume` | Reuse the existing box, run tag, provider and session. Restart a stopped box only for the approved action. Re-check worker liveness and refuse a second live worker. If resuming an explicitly approved stall, end the prior process safely before resuming its session. Pass the entire current worker prompt again. Return the same identity. |
| `reconcile` | Look up the exact operation ID in durable local or provider-owned metadata. Return the original success response, or `null` after proving the action did not run or is safely repeatable by the same operation ID. Exit with an error while the result is ambiguous. This phase must never wake a box or perform an action. |
| `inspect` | Read provider state before any probe. Never attach, start, or invoke a wake-capable check on a stopped box. Return `{worker, boxRunning, review}`. Use `running`, `stalled`, `done`, `failed`, `orphaned`, `needsHuman` or `unknown`. Compare the top-level final log with the live PR and ticket, especially if the run ended reporting a pending wait. |
| `stop` | Require the exact managed box and re-check the current head and run state with the supplied verification before stopping. Return `true` only when it has stopped. Preserve its disk and session. |
| `archive` | Preserve the complete transcript in the configured archive outside the box. Return `{transcriptReceipt}` only after durable storage. Prefer capturing it at verified stop if fetching it later would restart the box. Any restart to retrieve a transcript must be explicitly permitted by the host adapter policy and disclosed in the cleanup approval. |
| `cleanup` | Confirm the archive receipt, exact managed box and current merged PR immediately before deletion. Return `true` when deletion is confirmed. Never delete a closed-unmerged box. |

For `inspect`, `review` is null if a PR is absent or unreadable. Otherwise it contains `state` (`open`, `merged`, or `closedUnmerged`), `url`, `headSha`, `checkedHeadSha`, `reviewHeadSha`, `checksPassed`, `checksPendingOrFailed`, `scopeMatches`, `evidencePresent`, `reviewComplete` and `finalReportComplete`. Read these facts from the provider and configured evidence sources. A worker's success line alone cannot establish them. A missing check suite, a changed head, pending review threads, missing evidence, or an ended turn waiting on a background task cannot pass verification.

The adapter has to make mutating phases safe by `operationId` and capacity admission safe against other actors before create or resume. If a provider times out after a create or request submission, establish what landed before retrying. An exit from the adapter is not a proof that the provider action did not run. Mast serializes its own admissions, durably reserves the operation before calling a mutating phase, and persists the reply before writing Tracker. A failed Tracker write replays that write without another remote action.

## Acceptance mapping

| Operation | Mast or host launcher | Selected worker prompt |
| --- | --- | --- |
| Queue, wave budgets and refill | Tracker queue stays user-owned. The launcher applies the configured batch, review budget and stop conditions. Mast requires exact approval per operation. | Work within the approved item and placement. |
| Readiness and questions | Mast blocks unresolved Required Actions. The launcher checks live scope, links, dependencies and placement and captures failure and usage-limit decisions. | Read the live work and comments. Surface ambiguities and wait for a human decision. |
| Capacity and dispatch | Mast enforces the Space and adapter caps before admission. The adapter handles provider-wide capacity, nonblocking launch and fresh-box allocation. The registry records the real box, tag, provider, session, owner and item. | Load the required workflow and reproduce and validate in the permitted environment. |
| Monitoring and resume | Existing poller reads box state before probing. Adapter inspection distinguishes actual progress, stalls, ended waits and lost workers. Mast enforces identity continuity. | Re-run outstanding waits synchronously and continue the original session with the latest decisions. |
| PR, CI and evidence | Adapter queries the real PR and attached evidence, validates base, linked scope and naming as configured, and provides check and review evidence at the current head. | Run independent review and CI. Each push repeats the full loop. Apply bot triage, replies and resolution as configured. For human feedback, make clear fixes without replying and send ambiguity to the owner. |
| Handoff and failures | Adapter checks the top-level outcome and configured handoff evidence, escalates blocked runs, and applies batch stop conditions. Mast requires fresh verification and approval for lifecycle changes. | Record the actual outcome, publish permitted evidence and QA handoff, and leave review requests and merge to the human. |
| Stop and cleanup | Mast and adapter stop only the verified managed box, gate cleanup on opt-in, merge and preserved transcript, and retain closed-unmerged boxes. | Preserve evidence for follow-up. |
| Manual registration | Existing upsert only records state. It never invokes the launcher. | No worker prompt. |
