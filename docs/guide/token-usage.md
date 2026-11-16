# Token usage

Open **Settings → Token usage** to see usage recorded by a device across its Spaces. Choose a device, time range, model, and token type. Select an activity to narrow its contributing runs; select a run to open its chat.

The page starts collecting new usage when the updated engine runs. It does not reconstruct older sessions. The visible page refreshes periodically; Refresh retrieves the latest stored measurements without making a model request. An offline device must reconnect before its data can be queried.

## Provider totals

Overview ranks activities by selected tokens. Input includes cached input; output includes reasoning when the provider includes it. Cache read, cache write, reasoning, uncached input, and output excluding reasoning filters use the reported breakdown. Missing breakdowns are labeled and excluded from those specific filters.

Claude result totals are grouped by the actual reported model and include subagents. They replace provisional request observations rather than being added to them. Codex cumulative counters are converted into turn measurements that exclude earlier resumed-session history. Other adapters can provide input/output totals with less detail. Unknown models remain visible.

Completed runs, failed/cancelled attempts, and partial measurements are retained. A completed run means the harness finished; it does not establish that the work was correct.

## Optimization diagnostics

**Tools** separates MCP, CLI, and native tools. It records call counts, observed argument and response bytes, retained transcript-result bytes, errors, repeated normalized inputs, and observed elapsed time where available. Raw payload-size collection is supported for Claude and Codex. Other adapters may provide only their normalized retained results.

Original harness responses and retained transcript results are separate stages. They can differ because of truncation. Neither proves exactly what the provider received on every later request. Payload bytes are not billed tokens. Mixed-model turns cannot reliably assign each tool payload to an individual model. The model filter selects turns containing that model for diagnostics; the token-type filter affects provider accounting, not byte measurements.

**Instructions & rules** records Work Outline instruction entries and envelope sizes by revision when Mast submits them to the harness. The envelope includes the entries, so those rows overlap and must not be added together. Repo rule files found on disk and instruction/skill reads are labeled separately from instruction delivery. File inventory uses file sizes and modification versions; availability does not establish prompt inclusion. Hidden harness prompts, global/repository instructions loaded internally by a harness, tool-schema delivery, and repeated inclusion in provider requests are not currently observable. The page states these gaps instead of displaying zero cost.

**Trends & comparisons** shows daily totals in UTC and groups runs by configuration fingerprint, model, and activity. It shows completed, failed/cancelled, and partial sample counts plus average tokens per completed run. A configuration fingerprint covers the Mast package version, requested harness/model/reasoning/options, and supplied Work Outline revision. It does not identify every external harness configuration or hidden prompt change. Use the same task and comparable outcomes when comparing workflows; differences in cache warmness, models, task difficulty, or external configuration can explain apparent savings.

Useful candidates include large MCP responses, repeated CLI commands, discovery/polling loops, repeated failed calls, large instruction envelopes, and expensive background title runs. These observations identify candidates to investigate; the page does not automatically change rules or workflows or claim causal savings.

## Storage

The selected engine keeps a private `token-usage.sqlite3` ledger in its profile store. It retains numeric measurements, safe tool labels, source IDs/revisions, and configuration fingerprints. Normalized command/argument fingerprints use a device-local secret; raw commands, prompts, and results are not copied into this ledger. Existing transcript storage is unchanged. Explicit chat deletion removes its usage records; archiving keeps them.
