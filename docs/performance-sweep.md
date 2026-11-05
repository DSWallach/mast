# Performance sweep

Baseline on integration commit `ccb8cc107`, macOS, installed profile, headless GPUI, 2026-11-05. Timings are single runs on a shared machine, not statistical comparisons across machines. Run the ignored `bench_prepared_transcript_open`, `bench_transcript_long_chat`, `bench_team_300_items_50_chats`, and `bench_overview_250_chats_40_items` tests with `--profile installed -- --ignored --nocapture --test-threads=1`. Run `streaming_one_chat_among_many` without `--ignored` for notification counts.

| Measurement | Baseline | Notes |
| --- | ---: | --- |
| Prepare 2,000 synthetic messages | 74.28 ms | Preparation alone; normally performed in the background. |
| Open prepared messages and draw | 9.70 ms | Synthetic state insertion, sync, and first headless draw; excludes subscription and replay latency. |
| Transcript scroll and draw, 700 px | 1.45 ms mean, 4.45 ms p95 | 100 forward scrolls; synthetic direct-sync fixture. |
| Transcript streaming sync and draw | 14.55 ms mean, 36.75 ms p95 | 30 updates; older direct-sync benchmark bypasses background preparation, and adds a part per tick. Not end-to-end production streaming latency. |
| Subscription chunk through preparation and draw | 5.83 ms mean, 7.56 ms p95 | 30 text appends through actual RPC subscription and background preparation with synthetic frames; headless test scheduler and forced draw. 600 row-render calls total for 17,001 rows, unchanged entry rows retained. |
| Team facts, 300 items and 50 chats | 0.07 ms mean, 0.08 ms p95 | 100 pure fact-sheet builds; does not measure Team drawing. |
| Outline redraw | 3.18 ms mean, 5.96 ms p95 | Existing fixture: 120 items and 250 chats, clean rows. |
| List redraw | 105.66 → 1.98 ms mean; 150.63 → 2.54 ms p95 | Same 250-chat fixture, before and after variable-height List virtualization. |
| List first draw | 68.66 → 2.54 ms | Same fixture; allocations requested 332.32 → 11.70 MiB. |
| Observer notifications, 50 background heartbeats | Overview 1, shell 1, transcript 1 | 250 chats; other measured views 0. |
| Observer notifications, 50 preview changes | shell 50, transcript 50, chat-type view 50 | Other measured views 0; selected transcript revisions separately notify transcript 50 times. |

Transcript rows and settled markdown trees are cached per entry; live markdown uses incremental parsers. Syntax highlights and cross-frame rendering also have caches. A changed transcript still traverses and reconstructs the flattened row vector in `Transcript::sync`. The cached-open timing is not selection-to-first-frame latency: it excludes asynchronous replay and the fixture inserts already prepared rows. The subscription benchmark exercises the production watch and preparation path using a synthetic in-memory RPC peer; its wall time includes the headless test scheduler and cannot establish real agent or network latency. Startup-to-interactive, a 300-item outline redraw, Team redraw and real status-tick fan-out remain unmeasured. Engine bootstrap is dispatched to Tokio by `AppState::bootstrap`; this alone does not establish that every startup operation is off the UI thread.

List virtualization (`88dfa4dab`) retains the flattened grouped order and virtualizes variable-height rows. The GPUI regression checks distant selection, expansion after scrolling away and back, group ordering and the visible chat anchor after reordering. Existing tests cover List keyboard navigation and Outline keyboard restructuring and drag. Every render marks measured row heights for remeasurement because expansion, hover, subagents and asynchronously fetched details can change height. The two timing runs were taken on the same host using the same installed-profile fixture; normal machine load may differ.

## Follow-up: selection path (2026-11-05)

On integration commit `d579be7a7`, the installed-profile headless synthetic RPC benchmark measured selection through delivery of a complete 2,000-message reset and draw at 124.02 ms; a cached revisit and draw took 6.02 ms. These are single runs on a shared machine. The synthetic peer constructs the full reset locally, so this measurement excludes the engine's document opening, tail read, and history hydration; it is not end-to-end engine latency. The fixture includes inline tool output and is the same `long_chat` fixture used above. Run `bench_subscribed_transcript_stream` with `--profile installed -- --ignored --nocapture --test-threads=1`.

The selected-chat watch is cancelled on switching away and started again on every selection. A full replayed transcript can be restored synchronously from the bounded recently-viewed cache, but the new watch still requests an opening tail followed by an authoritative full history. The existing tests cover cache bounds, account replacement, and an authoritative reset replacing a cached transcript. An embedded-engine measurement and startup-to-interactive measurement remain necessary before using this result to prioritize changes.
