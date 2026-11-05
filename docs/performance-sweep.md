# Performance sweep

Baseline on integration commit `ccb8cc107`, macOS, installed profile, headless GPUI, 2026-11-05. Timings are single runs on a shared machine, not statistical comparisons across machines. Run the ignored `bench_prepared_transcript_open`, `bench_transcript_long_chat`, `bench_team_300_items_50_chats`, and `bench_overview_250_chats_40_items` tests with `--profile installed -- --ignored --nocapture --test-threads=1`. Run `streaming_one_chat_among_many` without `--ignored` for notification counts.

| Measurement | Baseline | Notes |
| --- | ---: | --- |
| Prepare 2,000 synthetic messages | 74.28 ms | Preparation alone; normally performed in the background. |
| Open prepared messages and draw | 9.70 ms | Synthetic state insertion, sync, and first headless draw; excludes subscription and replay latency. |
| Transcript scroll and draw, 700 px | 1.45 ms mean, 4.45 ms p95 | 100 forward scrolls; synthetic direct-sync fixture. |
| Transcript streaming sync and draw | 14.55 ms mean, 36.75 ms p95 | 30 updates; older direct-sync benchmark bypasses background preparation, and adds a part per tick. Not end-to-end production streaming latency. |
| Team facts, 300 items and 50 chats | 0.07 ms mean, 0.08 ms p95 | 100 pure fact-sheet builds; does not measure Team drawing. |
| Outline redraw | 3.18 ms mean, 5.96 ms p95 | Existing fixture: 120 items and 250 chats, clean rows. |
| List redraw | 105.66 ms mean, 150.63 ms p95 | Same fixture; existing unvirtualized List mode. |
| Observer notifications, 50 background heartbeats | Overview 1, shell 1, transcript 1 | 250 chats; other measured views 0. |
| Observer notifications, 50 preview changes | shell 50, transcript 50, chat-type view 50 | Other measured views 0; selected transcript revisions separately notify transcript 50 times. |

Transcript rows and settled markdown trees are cached per entry; live markdown uses incremental parsers. Syntax highlights and cross-frame rendering also have caches. A changed transcript still traverses and reconstructs the flattened row vector in `Transcript::sync`. The cached-open timing is not selection-to-first-frame latency: it excludes asynchronous replay and the fixture inserts already prepared rows. The existing streaming test similarly does not establish production per-chunk cost. Startup-to-interactive, a 300-item outline redraw, Team redraw and real status-tick fan-out remain unmeasured. Engine bootstrap is dispatched to Tokio by `AppState::bootstrap`; this alone does not establish that every startup operation is off the UI thread.
