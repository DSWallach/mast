# Token usage monitoring — proposed implementation plan

Status: first implementation complete; review and validation in progress, authorized by David. This plan is attached to the **Token Usage Monitoring** work item.

Branch: `david/token-usage-monitoring`  
Worktree: `.worktrees/token-usage-monitoring`  
Planning base: `david-rolling` at `4cc809e202f2264cafce503d9479cc7da12fefc2`.

## Intended result

Add **Settings → Token usage** so David can answer “Which activities consume the most tokens?” and narrow the answer by **model** and **token type**. Include Mast’s background model calls alongside visible conversations. A primary goal is to identify changes to Mast that reduce token consumption: compare MCP with CLI workflows, identify expensive instructions and program rules, and measure whether an optimization actually helps. Optimization diagnostics and the underlying provenance are required in the first release, alongside usage totals.

Recommended first release: usage recorded by the selected device, across its Spaces, with an explicit device selector and coverage dates. Cross-device combined totals, dollar estimates, budgets, alerts, and semantic classification of arbitrary chat content are follow-up work.

## What the code already provides

- The shared `AgentEvent::Usage` contains only input and output counts; context occupancy is a separate event. [agent.rs:565](crates/proto/src/agent.rs:565 "measured")
- Claude’s result normalizer exposes those two counts even though result frames also contain a per-model map. Its typed usage body drops other token fields. [wire.rs:155](crates/harness/src/claude/wire.rs:155 "measured"), [normalize.rs:647](crates/harness/src/claude/normalize.rs:647 "measured")
- Codex reads the latest usage snapshot, so repeated snapshots cannot simply be summed. Adapter semantics need fixtures before aggregation rules are finalized. [normalize.rs:176](crates/harness/src/codex/normalize.rs:176 "measured")
- Session events have a timestamped on-disk journal; usage is omitted from transcript content. Existing journals may support partial historical totals, but cannot recover fields already discarded. [run_journal.rs:1](crates/engine/src/run_journal.rs:1 "measured"), [sessions.rs:1989](crates/engine/src/sessions.rs:1989 "measured"), [parts.rs:574](crates/doc/src/parts.rs:574 "measured")
- Automatic titles run through a separate `run_title` stream whose collector ignores usage. It needs explicit instrumentation. [titles.rs:475](crates/engine/src/titles.rs:475 "measured")
- Settings has a section enum, navigation, and dedicated page entities we can extend. Local SQLite stores already have a precedent in the engine. [shell.rs:571](crates/ui/src/shell.rs:571 "measured"), [scheduled_tasks/store.rs:15](crates/engine/src/scheduled_tasks/store.rs:15 "measured")
- Tool events distinguish shell execution from MCP server/tool calls. Instruction sidecars retain pinned revisions and skill-read paths/mtimes. These are useful correlation points, but are not a complete measurement of the model-visible request. [agent.rs:219](crates/proto/src/agent.rs:219 "measured"), [run_journal.rs:60](crates/engine/src/run_journal.rs:60 "measured")

## Page design

Place **Token usage** beside Accounts in Settings.

1. **Filters:** device, date range (Today, 7 days, 30 days, All recorded, Custom), model (all or selected models), and token type (all or selected types). Default to the current device and the last 7 days. Preserve filters while navigating within Settings.
2. **Summary:** selected token total, share by token type, and number of recorded runs. Display recording start date, last update, and any incomplete coverage.
3. **Activity ranking:** descending horizontal bars and a table showing activity, selected tokens, percentage of the filtered total, and run count. Changing model or token type updates every total, percentage, and ranking together.
4. **Drill-down:** select an activity to see contributing sessions/runs, model, time, and token breakdown, with links to available chats or work items. Paginate the result. Background title runs link to their owning chat.
5. **States:** distinguish no recorded data, no matching results, unsupported usage reporting, incomplete usage, loading, error, and offline device. Include Refresh and throttled updates while visible. Use the existing Settings style in light and dark themes and at narrow widths.
6. **Optimization views:** tabs for Overview, Tools, Instructions & rules, and Trends & comparisons. Keep model, token-type, date, and device filters consistent across them. Rank tools and rules by attributed/estimated token exposure, with visible measurement method and coverage. Link each row to contributing turns and source revisions. Show billed usage and diagnostic estimates separately.

## Activity attribution

Use a stable, mutually exclusive **purpose** per run, captured where Mast launches it:

| Activity | Rule |
| --- | --- |
| Conversation | Ordinary interactive work without a more specific launch purpose |
| Scheduled task | Run launched for a scheduled task |
| PR review | Run explicitly launched by Mast’s review flow |
| Work-item automation | Work Outline worker/orchestrator or automatic continuation with an explicit automation origin |
| Title generation | Mast’s automatic chat-title model call, including attempts that fail or retry |
| Other / unknown | Purpose cannot be established reliably |

Track **execution role** separately: main run or subagent. Offer a secondary “Group by role” control so David can compare subagents with main runs without overlapping the purpose totals. Subagents inherit their owner’s purpose when known. Scheduled review runs use Scheduled task as their primary purpose and may retain review as detail metadata. Do not infer “coding,” “research,” or “planning” from message text in this release.

The launch-path audit in step 1 must confirm the exact origins available and add missing purpose metadata. Tool execution itself is not a model request; do not assign token costs to shell commands or individual tools without provider evidence.

## Required optimization diagnostics

### Capture provenance at the point of delivery

Attach diagnostic records to stable request/turn/attempt and tool-call identities. Capture metrics before truncation/sanitization loses the necessary detail, while separately recording what the harness actually delivered to the model. A large original tool response is not proof that the model received it all. For every field record whether it is provider-reported, counted from the actual payload with a known tokenizer, estimated, or unavailable. Store the tokenizer/version and attribution algorithm version so comparisons remain interpretable.

Codex `thread/compacted` is recorded as a `compaction` diagnostic (stage `harnessEvent`, one per compaction, no size); Claude `compact_boundary` frames are not yet captured. Where Mast cannot observe a harness-owned prompt, hidden instruction, tool catalog, or compaction request, report that visibility gap. Do not estimate full request composition from only the visible transcript. Prefer passive hooks and existing logs/metadata; collection itself must not introduce extra model requests.

| Diagnostic area | Required information and useful comparisons |
| --- | --- |
| MCP versus CLI | Execution channel; MCP server/tool identity or normalized CLI executable/subcommand; optional explicit operation family to compare equivalent work; argument, response, and model-visible result sizes/tokens; success/error/timeout; retries, pagination, follow-up call count, and elapsed time. Preserve nested calls, such as a CLI invoked inside an orchestration tool, without counting one invocation twice. |
| Tool definitions and discovery | Available versus actually delivered tool schemas; stable catalog/schema versions; definition/description token size; discovery/search responses; MCP resource reads; how often and on which requests schemas are included. Track CLI help output, wrapper instructions, and discovery calls too. Attribute shared catalog overhead separately from per-call payloads. |
| Instructions, program rules, and skills | Stable rule/source ID, scope, revision/hash, rendered size/tokens, delivery mechanism, first injection/reinjection, and observed inclusion in subsequent requests. Cover global/repository AGENTS.md and CLAUDE.md rules, Work Outline/template rules, skill catalogs and loaded skill bodies, and Mast-generated system/runtime instructions where observable. Distinguish available/listed, read, injected, and confirmed present; an on-disk file is not automatically prompt cost. |
| Context growth and reuse | Prompt composition by source: instructions, tool definitions, user input, assistant history, tool results, retrieved files/resources, and unknown remainder. Record context length, cache read/write and hit share, prefix/version changes where observable, repeated content fingerprints, and reinclusion across requests. Track compactions/summaries, resets, and subagent initial context separately. |
| Avoidable repetition | Repeated searches/file reads, duplicate tool responses, verbose results, polling and empty polls, tool discovery loops, failed calls and retries, repeated validation, approval/clarification round trips, automatic wakes, and repeated instruction/skill loading. Track deterministic signatures and counts; do not use extra model calls to classify them. |
| Behavioral effects of rules | Link rule/config revisions to workflow events such as mandatory repeated checks, forced delegation, retries, and extra model turns when a structured trigger identifies the rule. Track direct text overhead separately from downstream behavior; when causation is not observable, describe the relationship as correlation. |
| Configuration and outcomes | Mast build, harness/version, actual model, reasoning setting, enabled tools, effective instruction/configuration fingerprints, launch trigger, task/work-item identity, parent/child relationships, and completion/failure/cancellation. Retain explicit user outcome feedback when available; a completed run alone does not prove correct work. |

Use metadata, sizes, safe operation labels, and local keyed fingerprints rather than new raw command, argument, prompt, or response copies. References to existing local transcript/instruction sources can support drill-down while those sources exist. Handle instruction and tool-catalog versions as dimension records, avoiding repeated storage of their bodies. Keep a schema/versioned capability map so unsupported adapters remain visible.

### Attribute overhead without overstating precision

- **Rule leaderboard:** show per-injection size, injections, confirmed/estimated request inclusions, cumulative token exposure, and associated workflow counts. Repeated inclusion can make a small rule expensive. Label cache-aware versus unknown exposure; do not assume every repeated token was uncached or proportionally distribute provider cache totals across rules.
- **Tool leaderboard:** separate catalog/schema overhead, emitted call arguments, returned model-visible content, later context reuse, and associated follow-up model usage. Show counts, totals, median/p95 tokens per operation, and failure/retry rates. MCP and CLI each have discovery and context overhead; neither channel is assumed cheaper.
- **MCP/CLI comparison:** compare explicitly matched operation families under the same model, token type, comparable task/outcome, and configuration. Show sample size, coverage, and unmatched operations. A session using both channels cannot have its entire usage charged to both. CLI names may be unavailable for compound scripts; leave them unknown rather than executing or unsafely guessing command contents.
- **Reconciliation:** diagnostic source dimensions overlap the activity/model totals and are not additional usage. Within a request, source categories must be disjoint or marked overlapping, and show any unassigned remainder. Estimated content tokens never silently replace provider totals. Separate one-time payload size from repeated token exposure. Never label an adjacent model call’s entire usage as a tool’s marginal cost.

### Verify improvements

Provide before/after comparisons by saved configuration/build/rule revision and explicit experiment/cohort IDs. Show absolute totals and tokens per comparable successful operation, cache share, retries, tool/model call count, duration, and available outcome feedback. Flag model, task mix, harness, cache warmness, and configuration differences that prevent a fair comparison. Use repeated controlled tasks to establish savings from alternatives such as slimmer rules, fewer loaded tool schemas, CLI replacements, compact tool results, less polling, or targeted context delivery. Passive correlations generate candidates; controlled comparisons support savings claims.

The page should present evidence-backed optimization candidates with links to expensive sources and example runs. Do not auto-disable rules or replace workflows. Active benchmark/replay runs are explicit actions because they consume tokens and may perform work; ordinary Settings navigation remains passive. Dollar estimates remain deferred, but record the token categories and cache details needed for a future pricing layer.

## Token accounting contract

- Retain provider/harness, actual model where reported, configured model separately, device, Space, chat, work item, logical turn, attempt, subagent identity, purpose, timestamp, source identity, and coverage status.
- Normalize token types into disjoint buckets: **uncached input, cache read, cache write, non-reasoning output, reasoning output**. UI labels should say “Output excluding reasoning” where needed.
- Providers may report cached input inside input totals and reasoning inside output totals. Subtract a subset only when the provider supplies it and the adapter’s fixtures establish that relationship. Never add a subset to its parent total.
- Preserve reported input/output totals when the split is unavailable. Show an “Input — breakdown unavailable” or “Output — breakdown unavailable” bucket in All tokens. A specific subtype filter shows only known values and a coverage notice. Missing is not zero.
- Resolve models from response metadata when possible. Do not attribute all usage to the model currently selected in the composer. Unknown models remain visible and filterable; configured-only attribution is labeled.
- Identify whether a provider record is a request measurement, a turn/session cumulative snapshot, or a parent aggregate including children. Deduplicate repeated events and reconcile snapshots instead of summing them. Count each model request once across parent/subagent totals; any unresolved inclusive aggregate stays marked as an aggregate, without inventing a split.
- Record observed usage from failed, interrupted, retried, and self-continued work. Separate attempts for attribution while keeping logical-run counts stable. Persist partial measurements before final completion so a crash does not erase known consumption.

## Implementation sequence

### 1. Establish adapter semantics and capture detail

Audit Claude, Codex, Cursor, OpenCode, and ACP usage reporting plus all Mast model-launch paths. Build a capability matrix for actual model, token subtypes, snapshot scope, child inclusion, request-composition visibility, tool catalogs, instruction delivery, and compaction from existing/new fixtures. Add an additive detailed usage event or compatible optional fields, preserving old journal decoding and existing usage consumers. Record provenance and explicit launch purpose. Include the separate title-generation path. Instrument tool delivery and instruction composition at their owning boundaries, not just the UI event fold; prioritize Claude and Codex for detailed diagnostics and expose explicit gaps for every other adapter.

### 2. Add durable storage and aggregation

Add an engine-local `usage` module and SQLite ledger, following existing engine store patterns. Keep source measurements with stable deduplication keys; maintain a queryable normalized projection with indexes for time, model, purpose, and ownership. Upsert snapshots and make replay/recovery idempotent. Make persistence/reconciliation recoverable after a crash without blocking transcript rendering.

Retain numeric metadata and provenance identifiers/fingerprints only; no new prompt/tool-output copies. Add related tables for tool invocations/delivery sizes, instruction and schema versions/inclusions, request composition, context lifecycle, and experiment/configuration snapshots. Index these for tool/rule rankings and comparisons. Bound per-event collection work, fingerprint/tokenization caches, and background processing queues; measure collection CPU, storage, and latency overhead. Query indexed records rather than scanning journals whenever Settings opens. Keep records independent of mutable chat titles and archival state; specify deletion behavior so explicit chat-data deletion also handles its usage metadata.

### 3. Expose the query API

Add typed RPC request/response definitions for filters, summary, activity/role groups, paginated run details, available models, and coverage metadata. Add tool/channel and instruction/rule rankings, request-composition details, version/cohort comparison queries, and measurement-method filters. Execute on the selected device using existing device routing conventions. Use one shared filter/accounting implementation for totals and drill-downs. Define timezone-aware calendar ranges and half-open timestamp bounds consistently.

### 4. Build the Settings page

Add `SettingsSection::TokenUsage`, wire navigation/page lifecycle in `shell.rs`, and implement a dedicated `settings/token_usage.rs` page with Overview, Tools, Instructions & rules, and Trends & comparisons tabs and the controls and states above. Diagnostic rows must expose provenance, uncertainty, sample size, and examples. Cancel stale requests after filter/device changes and throttle live refreshes. Opening Settings must not start model calls.

### 5. Handle history and validate

Begin reliable collection at rollout. Add a resumable background import of existing journals only where source identity, timestamps, and accounting scope are defensible. Label historical model/subtype/purpose gaps; leave ambiguous records unattributed or outside verified totals with a visible coverage explanation. Do not promise complete historical cache/reasoning totals or silently substitute the context meter. Native transcript backfill is deferred.

## Acceptance and verification

- A mixed fixture with conversations, scheduled tasks, reviews, subagents, and title runs produces the expected descending activity ranking.
- Model and token-type filters work independently and together; summary, group percentages, and drill-down sums reconcile under the same filters.
- Cache and reasoning subsets are counted once; unknown breakdowns and unknown models remain visible. Unsupported harnesses do not appear as zero consumption.
- Repeated snapshots, duplicate events, parent aggregates, child events, retries, interruption, restart, and replay do not inflate or erase known totals.
- Title-generation attempts appear even though they have no ordinary session transcript. Archiving or renaming a chat does not change recorded totals.
- Equivalent MCP and CLI fixtures produce comparable operation groups with catalog/discovery, arguments, results, retries, and follow-up usage distinguished. Mixed-channel and nested execution does not duplicate usage; unmatched operations remain visible.
- A small repeatedly included rule ranks above a larger once-injected rule when its observed exposure is higher. Skill listings, reads, injection, repeated request inclusion, and downstream behavior remain distinct. Rule revision changes survive drill-down and before/after comparisons.
- Verify that truncation/summarization reduces model-visible result metrics without changing original response-size metrics. Test cached repeated context, shared tool catalogs, compaction, unknown tokenizer/model, missing payload visibility, and estimated-versus-provider reconciliation.
- Controlled comparison fixtures show sample size, configuration/model differences, normalized outcomes, and uncertainty. A low-token failed task is not presented as a verified optimization. Confirm passive diagnostics issue no model calls and that collection overhead remains bounded.
- Test midnight/timezone boundaries, empty results, offline device, stale response cancellation, and storage/import recovery. Benchmark indexed queries on a representative large ledger and ensure Settings stays responsive during collection/import.
- Add focused adapter, engine/store/RPC, and page-state tests. Run applicable Cargo checks/tests using the shared `CARGO_TARGET_DIR`; visually inspect the page in light/dark themes and narrow/wide layouts.
- Before any merge, run the repository’s `pr-review` gate on the implementation branch, address every finding, and rerun until CLEAR at the exact merge SHA.

## Scope decisions

The first implementation follows these decisions: device-scoped first release; explicit operational activities rather than guessed task topics; prospective collection with clearly labeled partial journal history; token counts before dollar estimates. Optimization provenance, MCP/CLI comparisons, and expensive-rule diagnostics are required first-release scope. Detailed capture is limited by what each harness exposes, with coverage visible rather than fabricated. No blocking product question is needed to present this plan. David can revise these choices before implementation.

## First implementation coverage

Implemented: durable device-local collection; model/token-type/date filtering; activity and role groups; private telemetry; paginated chat drill-down; MCP/CLI/native payload sizes, repeated inputs, errors and elapsed time; Work Outline instruction revisions and working-directory rule-file inventory; context occupancy history; advertised-tool-name fingerprints; daily usage and configuration cohorts; title attempts; registry-driven deletion cleanup; capability-aware routing.

Unavailable or deferred: exact hidden prompt/tool-schema tokens, actual repeated instruction inclusion, precise Claude subagent separation, complete first resumed-Claude-turn aggregates, cross-device combined totals, historical journal/native-transcript imports, custom calendar ranges, outcome-confirmed experiments and automatic savings claims. The page explicitly distinguishes provider counts, observed bytes, on-disk availability, and missing measurements. Configuration cohorts compare observations and do not establish causal savings.

## Token-efficiency follow-up

The follow-up branch `david/token-efficiency-insights`, based on `david-rolling`
at `7eb16398`, adds explicit agent-type attribution, partial native-child request
breakdowns, repetition candidates with example chats, failed-attempt totals,
and work-item/configuration cohorts. Its goal is to identify changes to Mast
and its workflows that reduce tokens on comparable work.

The accounting query remains authoritative. Combined Claude parent/child usage
is not split by guesswork. Missing historical type labels remain unknown.
Claude and Codex raw payloads are fingerprinted with a capture-local secret;
bytes and repetition are evidence to investigate, not causal savings claims.
The ledger retains an additive attribution table; existing measurement and
turn table shapes remain readable by older engines. Older engines cannot
serve the new agent filters; the UI checks response support before presenting
filtered totals.

## Per-turn analysis follow-up

Branch `david/token-usage-efficiency` adds the **Turns** tab and the
`token_usage_report` example. `TokenUsageSnapshot.turn_analysis` is computed from
the billing `FILTER`, so turn sums reconcile with the total. It is `None` from
older hosts, and the UI then asks for an update.

- `UsageContext` gains `sessionStart` (`fresh`, `resumed`, `continued`),
  `runTurn` and `chatCreatedAt`. These live in the existing JSON `context`
  column, so no table shape changes.
- Codex now also emits `request`-scope measurements from `last`, keyed by the
  cumulative totals so re-sent notifications collapse. The `FILTER` suppresses
  request rows whenever a same-role `turn` aggregate exists, even when the
  request was written before the model label was known.
- A turn with no request rows reports an unknown request count, not zero.
- `UsageContext` also gains `scheduledTask` and `trigger` (`Human`, `Other chat`,
  `Admin chat`, `Hand-off`, or a system label such as `Worker updates`). Older
  rows leave both unrecorded. The stored `activity` is unchanged:
  `classify_activity` reads the chat's scheduled-task info, so every later turn
  in a scheduled run's chat still stores `Scheduled task`.
- `TokenUsage::attribution_report` (engine only, no wire type) groups the same
  `FILTER` rows by activity, scheduled task, work item, chat, model, agent type
  and trigger, keeping uncached input, cache read, cache write and output as
  separate columns. It takes the scheduler's run history as input. A scheduled
  turn that started after its run finished is a follow-up, counted as
  Conversation in the rolled-up activities. It also reconciles every ledger row
  against what is accounted, since a plain sum of provider rows counts requests
  twice and includes resume baselines.
- Unknown model groups carry an explanation: the configured model, the model the
  chat's other turns used, or no hint. A turn starts as `Unknown model` and
  only gets a label when the harness reports one.
- Journal `usage` events have mixed semantics: Claude reports uncached input
  only, and Codex reports only the last request. The guide says not to sum them.

Evaluation on David's ledger (2026-11-16 to 2026-11-17, 318 turns, 681M
accounted tokens) showed cache reads at about 97% of input for both Claude Code
and Codex. Claude Code averaged about 220K input per request. The largest turn
was a cancelled Codex turn with 53M input across 164 tool calls and a 450K
peak context. The cost driver is requests × resent context, not prompt length.
Those rows predate session-start, chat-age and Codex request capture, so their
groups say "start unrecorded".

Known gap: `registry_deletion_is_reconciled_and_ledger_failure_cannot_skip_doc_purge`
fails on the base too when the module runs in parallel. `publish_if_changed`
coalesces a create-then-delete into no change, so the reconcile waits for the
30-second tick, longer than the test's 5-second wait.

### Efficiency capture overhead check

Explicit ignored test `large_ledger_snapshot_and_capture_overhead` creates
10,000 turns and 100,000 attributed tool diagnostics in an isolated WAL ledger.
On this Mac in an unoptimized debug build during concurrent validation, the
all-agent snapshot took 2.086 s and the worker-filtered snapshot took 1.436 s.
Capturing 1,000 read events, including attribution writes and ten batch barriers,
took 161.967 ms with no dropped events or capture failures. These are local
measurements, not latency guarantees. The snapshot uses a separate read-only WAL
connection after its writer barrier, so the history scan does not hold the
collector mutex. Large all-history scans remain a cost to track as ledgers grow.
Run it with `cargo test -p mast-engine --lib
large_ledger_snapshot_and_capture_overhead -- --ignored --nocapture`.

Build provenance is supplied explicitly by packaging/dev-app commands through
`MAST_USAGE_BUILD_REVISION`. Ad hoc builds without it report unknown. This avoids
both stale worktree revision attribution in the shared target and recursive
source-directory watches that rebuild the engine after unrelated UI edits.
