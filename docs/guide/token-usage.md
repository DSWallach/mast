# Token usage

Open **Settings → Token usage** to see usage recorded by a device across its Spaces. Choose a device, time range, model, agent type, and token type. Select an activity to narrow its contributing runs; select a run to open its chat.

The page starts collecting new usage when the updated engine runs. It does not reconstruct older sessions. The visible page refreshes periodically; Refresh retrieves the latest stored measurements without making a model request. An offline device must reconnect before its data can be queried.

## Provider totals

Overview ranks activities by selected tokens. Input includes cached input; output includes reasoning when the provider includes it. Cache read, cache write, reasoning, uncached input, and output excluding reasoning filters use the reported breakdown. Missing breakdowns are labeled and excluded from those specific filters.

The first result after resuming a Claude process establishes a cumulative baseline. Only its observed current-process requests are included; that first resumed turn may be incomplete. Claude result totals are grouped by the actual reported model and include subagents. They replace provisional request observations rather than being added to them. Codex cumulative counters are converted into turn measurements that exclude earlier resumed-session history. Other adapters can provide input/output totals with less detail. Unknown models remain visible.

Completed runs, failed/cancelled attempts, and partial measurements are retained. A completed run means the harness finished; it does not establish that the work was correct.

## Find expensive agents

Overview ranks agent types alongside activities. Mast records `pr-review` for
its review launcher, `worker` for chats created by another chat, `orchestrator`
for main chats that create workers, and `title-generator` for automatic titles.
These labels are separate from activity: a scheduled review can have activity
Scheduled task and agent type `pr-review`. A worker that also creates children
keeps its worker label. Parent-chat and work-item IDs are kept in run details.

Native subagents use their provider's explicit type, such as `explorer`, `worker`,
or a custom `pr-review`. Missing metadata and older ledger rows remain
**Unknown agent type**. Names are not guessed from a prompt, task description,
or a shell command. An opaque `claude --agent pr-review` subprocess has no
independently observed token usage; the parent tool call alone cannot establish
its cost. Resumed native children whose original spawn was not observed can also
have an unknown type.

Select an agent type to filter the accounted totals, models, token types,
contributing turns, and configuration groups consistently. A remote engine must
advertise `token-usage-insights-v1` for these filters; older responses produce an update message rather than
silently returning unfiltered totals.

Claude's combined totals remain with the launching agent and say **includes
subagents**. **Observed child agent requests** shows a partial, provider-reported
breakdown with links to example chats. Those observations overlap the combined
totals and are never added to them. An agent filter can therefore show child
request evidence with no independently attributable billing total.

## Investigate repeated work

The **Efficiency** tab ranks repeated tool inputs, repeated retained tool
results, errors, empty CLI/MCP results, and repeated instruction deliveries.
Each finding links to an example chat and identifies the agent type, stage,
observation count, repeated bytes, and time. Matching inputs and results use
local keyed fingerprints; raw commands and payloads are not copied to the ledger.
Repeated reads and searches are included alongside CLI and MCP calls. Claude
and Codex raw-result fingerprints use a secret limited to the harness capture;
matching across separate captures is unavailable. Where raw result metadata is
present, a retained transcript copy is excluded from the repetition ranking to
avoid reporting the same result twice.

Repetition is compared within a chat, agent type, operation, stage and the
selected time range. Equal inputs/results are candidates to inspect, not proof
of waste: a read after an edit, a fresh status check or an empty successful
command can be necessary. Bytes measure observed content, not billed tokens or
verified savings. Tool-input and tool-result rows are separate stages.

Instruction comparisons count observed submissions of the same revision to the
harness. They do not prove repeated inclusion in later provider requests. Where
individual instruction entries were recorded, the envelope is omitted from the
repetition ranking so the same delivery is not counted again as an envelope.

Efficiency also shows completed, failed, cancelled and partial **turns** with
their token totals. Total tokens per completed turn includes failed and partial
attempts in the selection; it does not hide retries by averaging completed-turn
tokens alone. A completed turn is not a verified successful task. Runs that
continue for several turns contribute several samples.

## Compare turns

The **Turns** tab breaks the accounted totals down per turn. Turn sums equal the
total under the same filters. Each turn shows its harness, reported models,
agent type and status, and how it started:

- **new session:** the harness started without resuming a native session.
- **resumed session:** Mast resumed a native session by ID, so the provider may
  resend that session's earlier history.
- **continued in process:** a later turn in the same harness process, which
  carries the context it has built up.

Each turn also shows the chat's age when it ran, its input split into cache read,
cache write and uncached input, and its output. It shows the number of
provider-reported model requests (main and subagent), peak context occupancy,
tool calls and tool-result bytes, and whether the numbers are a turn total, a
total that includes subagents, a provisional sum of requests, or input/output
only.

**Workflows** groups turns by harness, agent type and start. **Chat age** groups
them by how old the chat was. Each group shows median and p90 input per turn,
the cache-read and cache-write shares of input, and input per request. Input
per request approximates the context sent with each model call. When cache
reads dominate, most input is resent context. Fewer requests or a smaller
context reduce it more than shorter prompts do. Groups with different task
mixes are not directly comparable.

Input includes cache reads and writes, which providers price differently from
new input. The page reports token counts only and does not estimate prices.

**Reported fields by harness** says which fields each harness supplied for the
selected turns:

| Harness | Cache read | Cache write | Reasoning | Request count |
| --- | --- | --- | --- | --- |
| Claude Code | reported | reported | not on result totals | per request |
| Codex | reported | not reported, recorded as zero | reported | per request |
| Others | not reported | not reported | not reported | not reported |

Missing values stay unknown and never count as zero, except for Codex cache
writes, which the page labels. Turns recorded before an engine captured a field
show it as unrecorded. Session start and chat age start with this release.
Codex request counts start with this release too.

To print the same analysis from a terminal without touching the live ledger,
run `cargo run -p mast-engine --example token_usage_report -- <store-root> [days]`.
`<store-root>` is the folder that holds `token-usage.sqlite3`, such as
`~/.mast/profiles/local`. The example copies the ledger first.

### Chat journals are not usage totals

The per-chat journals keep an older `usage` event. Its numbers mean different
things for each harness. For Claude Code it is the turn's input without cache
reads or writes. For Codex it is only the last model request of the turn. Other
adapters report whatever their harness supplies. Summing those events mixes
incompatible quantities and leaves out almost all cached input. Use this page
or the report example instead.

## Optimization diagnostics

**Tools** separates MCP, CLI, and native tools. It records call counts, observed argument and response bytes, retained transcript-result bytes, errors, repeated normalized inputs, and observed elapsed time where available. Raw payload-size collection is supported for Claude and Codex. CLI execution hidden inside an MCP wrapper remains attributed to that wrapper unless the harness exposes separate tool events. Other adapters may provide only their normalized retained results.

Original harness responses and retained transcript results are separate stages. They can differ because of truncation. Neither proves exactly what the provider received on every later request. Payload bytes are not billed tokens. Mixed-model turns cannot reliably assign each tool payload to an individual model. The model filter selects turns containing that model for diagnostics; the token-type filter affects provider accounting, not byte measurements.

**Instructions & rules** records Work Outline instruction entries and envelope sizes by revision when Mast submits them to the harness. The envelope includes the entries, so those rows overlap and must not be added together. Repo rule files in the run’s working directory found on disk and instruction/skill reads are labeled separately from instruction delivery. File inventory uses file sizes and modification versions; availability does not establish prompt inclusion. Hidden harness prompts, global/repository instructions loaded internally by a harness, tool-schema delivery, and repeated inclusion in provider requests are not currently observable. The page states these gaps instead of displaying zero cost.

**Trends & comparisons** shows daily totals in UTC and groups turns by configuration fingerprint, model, activity, agent type, Space, and work item. It shows completed, failed/cancelled, and partial sample counts plus total tokens per completed turn, including failed and partial attempts. Compare the same work item, model, and agent type across configuration groups; differences in task mix or outcomes can explain the result. A configuration fingerprint covers the Mast package version and Git build revision, requested harness/model/reasoning/options, and supplied Work Outline revision. It does not identify every external harness configuration or hidden prompt change. Use the same task and comparable outcomes when comparing workflows; differences in cache warmness, models, task difficulty, or external configuration can explain apparent savings.

Useful candidates include large MCP responses, repeated CLI commands, discovery/polling loops, repeated failed calls, large instruction envelopes, and expensive background title runs. These observations identify candidates to investigate; the page does not automatically change rules or workflows or claim causal savings.

## Storage

The selected engine keeps a private `token-usage.sqlite3` ledger in its profile store. It retains numeric measurements, local file/item/tool labels, source IDs/revisions, and configuration fingerprints. Normalized command/argument fingerprints use a device-local secret; raw commands, prompts, and results are not copied into this ledger. Existing transcript storage is unchanged. The owning device removes usage when it observes a chat deletion, including deletions synced from another device. Offline devices reconcile when they reconnect. Archiving keeps usage.

Usage collection runs on a bounded background writer. If its queue fills,
measurements can be dropped and the page reports incomplete coverage. The
ledger has no automatic age or size limit: it grows with recorded activity
until the owning chats are deleted.

Comparison groups identify the Git build revision captured at compilation.
Builds with tracked source changes are marked **uncommitted**. Comparisons
should use clean commits: multiple uncommitted variants of one base revision
are not distinguished from each other. Source archives without Git metadata
report an unknown revision. Older rows can have an unrecorded revision.

The **Tools**, **Instructions & rules**, and context-occupancy sections use
turn-level selection for agent filters: if a turn contains the selected agent,
they include the whole turn's diagnostics and context samples. They cannot
isolate that child's share of the parent context. The provider-token rankings
and Efficiency candidates use per-observation agent attribution instead.

Packaged and development-app builds pass provenance explicitly through
`MAST_USAGE_BUILD_REVISION`, using `sh scripts/build-revision.sh`. For a manual
build, export that value before invoking Cargo. Cargo tracks the environment
value even in the shared cache. Builds without explicit provenance report
**unknown**, rather than risking another worktree's commit label. This metadata
does not cause engine rebuilds on unrelated UI edits.
