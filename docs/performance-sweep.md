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

A follow-up embedded-engine run of `bench_engine_chat_selection` (same command with that test name) measured selection to opening transcript draw at 22.69 ms, selection to full replay draw at 103.59 ms, and cached revisit draw at 4.64 ms with a cache hit. It uses the same 2,000-message fixture with inline 4 KiB tool outputs, an in-memory RPC client, a temporary engine profile, and a headless GPUI window. The clock starts at `AppState::select_chat`, so sidebar row event dispatch is excluded; the GPUI test scheduler can process more than one frame before a forced draw. This is a single warm, shared-host run, not a distribution. It does not measure process startup or a 50 MB registry.

The ignored `bench_shell_initialization` isolates early UI work with a temporary directory, default settings, and 250 preloaded chat rows: settings load 0.06 ms, font registration 185.26 ms, theme-library load 0.11 ms, shell-window construction 2.33 ms, and first forced headless draw 0.07 ms. Font registration includes enumerating and inspecting installed system fonts as well as adding bundled faces. The directory had no settings or theme cache, and the shell is not connected to an engine: this is not startup-to-interactive, does not include the 300-item/50 MB registry or warm PR/Linear caches, and does not prove that the first useful sidebar frame has appeared. The production startup path now has tracing spans for these stages and window creation. The revised engine chat-selection benchmark measured a cached revisit to its authoritative replay and draw at 109.44 ms (single run); this remains subject to the scheduler and row-dispatch limitations above.

The first embedded-engine runs above were made without stubbing `gh`. Engine assembly starts a PR sweep, so those runs may have made a read-only GitHub search and are not isolated baselines. The benchmark now refuses to start unless inert `gh` and `linear` executables are first in PATH. For an isolated rerun, create executable files named `gh` and `linear` containing exactly `#!/bin/sh` followed by `exit 1`, prepend their directory to PATH, and set `MAST_NO_LOGIN_SHELL=1`. Use only temporary paths.

The guarded `bench_profile_startup` uses an embedded engine and a warm temporary SQLite snapshot containing 250 chats, 300 work items, and 53,305,817 serialized registry bytes. Most of those bytes are 6,000 synthetic pending chat-preview updates, not a compacted registry; PR and Linear caches are empty. The initial shell draw was 203.12 ms and the forced draw after chats and the declared outline had loaded was 460.50 ms. This is a single shared-host installed-profile run; it measures data readiness and a forced draw, not a verified visual transition or actual user input. The benchmark declares the outline watch explicitly and the GPUI scheduler may coalesce frames. The engine-side model discovery and cache activity remain outside the individual UI initialization timings.

A guarded rerun of `bench_engine_chat_selection`, with inert `gh` and `linear` executables and login-shell capture disabled, measured opening draw 18.59 ms, full replay draw 102.40 ms, cached revisit draw 4.38 ms, and revisit to authoritative replay draw 95.54 ms. These replace the unisolated engine timings as the baseline for this fixture. They still begin at the selection controller and exclude sidebar event dispatch.

Lazy installed-font catalog (single installed-profile runs on the same shared host): the component font-registration stage was 185.26 → 0.01 ms. The guarded 53.3 MB profile benchmark's initial shell draw was 203.12 → 131.98 ms and its loaded-sidebar draw was 460.50 → 383.92 ms. The fixture and scheduler limitations above apply. Bundled fonts and the GPUI family-name snapshot are still registered before the first draw. The expensive file/metric scan runs on first catalog access, including before first draw when a saved installed font requires it, so this result applies to bundled/default selections; opening the font picker can pay the deferred cost. The catalog remains stable once loaded and is rebuilt for a new session. The regression covers lazy loading, sharing, and invalidation across sessions, including fallback after a terminal font disappears.

On a cached revisit, the transcript watch now discards the provisional opening tail before decoding and preparing it. The authoritative full reset still arrives and replaces cached content, including changes made while the chat was away. The guarded 2,000-message benchmark measured revisit to authoritative replay and draw at 95.54 ms before, then 170.32 ms during a concurrent UI test run and 91.70 ms on an unloaded repeat. These single-run numbers are too noisy to establish a latency improvement; they show the range observed, not a statistical result. The regression exercises the watch path and confirms the fresh reset replaces and invalidates the cached transcript. Full replay and off-screen markdown preparation remain in place.

## Follow-up: loaded sidebar attribution (2026-11-05)

The earlier 383.92 ms label was misleading: its clock starts before settings and UI initialization, engine bootstrap and the data-readiness loop, and ends after a forced draw. It is not the duration of that draw. On `b1cb43525`, two clean installed-profile runs on this shared host measured 335.86 and 328.12 ms cumulative. In the instrumented second run, the initial draw was 81.25 ms, the readiness loop took 237.32 ms (133.73 ms inside `run_until_parked`), and the final forced draw took 9.47 ms. The scheduler may draw multiple frames while waiting, so those durations do not isolate engine work from all UI work.

With `MAST_BENCH_SIDEBAR=1`, the same run recorded 13 populated sidebar renders: preparation, including status lookups, sorting and grouping, took 0.100–0.145 ms per render; row construction and visible-order calculation took 1.399–2.074 ms. The latter includes avatars, badges, status and chips, but does not break out each component. GPUI text shaping and layout are not separately measured; the final draw includes them and other shell work. The fixture has one space, no cached PRs and no custom sections, so it cannot characterize lookup scaling with many of those objects. Code inspection finds linear scans for sessions, spaces, devices, PR snapshots, custom section membership and group selection inside row loops; no claim about their measured impact on a larger fixture follows from this run. All active sidebar row elements are constructed; the sidebar is not virtualized. Its animated sections and drag behavior need dedicated regression coverage before virtualization.

Run `bench_profile_startup` as described above, adding `MAST_BENCH_SIDEBAR=1` for per-render instrumentation. Timings are individual runs and instrumentation adds output overhead. No live profile or external services are used.

Skipping jump-slot ordering while jump hints are hidden avoids a second chat sort and grouping pass on those renders. In one instrumented run before the change, 13 populated renders averaged 0.115 ms preparation and 1.512 ms construction; after, 12 averaged 0.125 ms and 1.482 ms, respectively. The after run overlapped a clean UI build, so this 0.030 ms difference does not establish a speedup. Cumulative startup was 328.12 → 423.21 ms and the final forced draw 9.47 → 9.66 ms in these noisy runs. When hints are visible the original ordering calculation still runs; navigation always retains its existing calculation.

A repeat after the UI suite was still highly variable: cumulative startup 1,012.49 ms, final forced draw 16.48 ms, and 11 populated renders at 1.783 ms mean construction. This reinforces that these shared-host single runs do not demonstrate a latency improvement.

## Follow-up: startup phases (2026-11-05)

Five serial runs of the guarded installed-profile `bench_profile_startup`, with the `startup-bench` feature, using the same 53.3 MB synthetic registry and empty PR/Linear caches. The instrumented build completed before the runs. A process-list check was unavailable for this batch, so other system activity cannot be excluded. Table values are median (minimum–maximum), milliseconds. Engine phase timings are durations; UI milestones are elapsed from immediately before settings loading. They overlap and must not be added. The test's GPUI scheduler can coalesce frames and delay observation of milestones.

| Phase or milestone | Median (min–max), ms |
| --- | ---: |
| Settings and keymap load | 0.028 (0.023–0.080) |
| Initial shell draw milestone | 95.660 (88.760–173.090) |
| Model registry setup | 0.037 (0.031–0.134) |
| Registry store open | 0.336 (0.288–1.321) |
| Registry snapshot read | 12.199 (11.903–13.388) |
| Registry decode | 20.639 (20.042–21.894) |
| Local pending-update settlement | 22.287 (21.982–23.632) |
| Registry materialization (`read_all`) | 0.117 (0.112–0.135) |
| Registry save | 8.735 (8.338–9.314) |
| Linear cache setup (empty cache) | 0.010 (0.008–0.016) |
| Engine core assembly, inclusive | 84.770 (80.739–97.464) |
| Engine/RPC ready milestone | 188.193 (178.512–287.199) |
| Chats loaded milestone | 242.461 (218.808–328.466) |
| Outline snapshot construction | 1.285 (1.245–1.509) |
| Outline loaded milestone | 316.794 (277.921–388.751) |
| Final forced draw duration | 9.590 (9.550–10.810) |
| Loaded draw milestone | 326.460 (287.850–413.940) |

This benchmark starts inside an already running test process and cannot measure process launch. It registers lazy harnesses but does not measure actual model discovery; that work is not awaited by the core assembly. The PR dashboard opens its cache on first dashboard RPC, which this fixture does not request. The Linear cache files are absent, so their setup timing is not representative of warm populated cache reads. The loaded milestone is a forced draw after both chats and an explicitly declared outline arrive; it is not a verified first visible populated frame. The watch is started only after the corresponding Space appears, and scheduler/RPC/frame delivery are included in the gap between chat and outline milestones. The 75 ms watch coalescing delay applies to subsequent changes, not the first snapshot. The data do not isolate the gap sufficiently to attribute it to a single operation.

The largest directly measured blocking work is snapshot read/decode and pending settlement; the synthetic registry deliberately contains 6,000 pending preview updates, so this result does not establish the same distribution for a compacted real profile. A candidate is to reduce registry history or make settlement and snapshot persistence cheaper. Deferring settlement or the boot save requires a durability and migration-order design: boot saves the migrated registry immediately so it survives a crash, and local settlement establishes authoritative state. Measure those alternatives with crash/restart and migration regressions before changing the ordering. A second investigation should instrument watch registration, RPC serialization/delivery and scheduler turns separately to explain the delayed outline milestone; do not assume the 75 ms coalescing timer is responsible. No startup behavior was changed in this follow-up.

Reproduce after building with `cargo test -p mast-ui --lib --profile installed --features startup-bench --no-run`: run the resulting test executable five times with `bench_profile_startup --ignored --nocapture --test-threads=1`, `MAST_NO_LOGIN_SHELL=1`, and the inert `gh` and `linear` executables described above. `startup-bench` gates the engine instrumentation; the ignored UI benchmark supplies the milestone clocks.

## Follow-up: startup watch scheduling (2026-11-05)

Five serial installed-profile runs of `bench_profile_startup` on `perf-sweep-4`, based on `78e2fe7d6`, used the same guarded 53.3 MB synthetic profile. The test process uses a current-thread Tokio runtime and a headless GPUI scheduler; the measurements are not process-launch or production event-loop timings. The host process list was unavailable, so competing activity cannot be excluded. Event intervals below use wall-clock timestamps; each row is the median (minimum–maximum), milliseconds, and intervals overlap. The earlier table's core assembly is a duration whereas its chat and outline milestones are elapsed from before settings load; subtracting those values does not measure the time from the end of assembly.

| Interval or duration | Median (min–max), ms |
| --- | ---: |
| Core assembly duration | 99.501 (89.299–108.120) |
| Core assembled → runtime assembled | 0.773 (0.546–4.209) |
| Runtime assembled → UI attachment | 0.785 (0.554–1.861) |
| UI attachment → chats subscribe starts | 0.747 (0.669–2.176) |
| Chats subscribe starts → frame received | 2.019 (1.854–3.325) |
| Chats JSON value decode | 0.127 (0.097–0.184) |
| Core assembled → chats applied | 4.524 (3.967–11.815) |
| Chats applied → test observes loaded chats | 53.756 (52.021–79.318) |
| Spaces frame received → outline watch spawned | 0.067 (0.039–0.147) |
| Outline watch spawned → subscription starts | 52.058 (50.702–76.742) |
| Outline subscription starts → snapshot construction starts | 0.330 (0.218–0.661) |
| Outline blocking-pool queue | 0.016 (0.009–0.046) |
| Outline snapshot construction | 1.549 (1.343–1.982) |
| Outline snapshot to JSON value | 0.493 (0.333–0.573) |
| Outline JSON value ready → checked subscription returns | 1.519 (0.819–85.400) |
| Checked subscription returns → snapshot applied | 0.259 (0.209–3.254) |
| Outline JSON value decode | 0.208 (0.156–0.324) |
| Outline applied → test observes loaded outline | 25.070 (16.568–46.468) |

The first snapshot is already published without the later 75 ms coalescing timer. `subscribe_checked` waits for the first item for this method, so its return includes snapshot generation and delivery; it is not a separate acknowledgement round-trip. The initial outline JSON value was approximately 179 KB. Measuring its encoded size performs an extra serialization in the instrumented build, so it adds overhead before the value-ready timestamp. The long watch-spawn interval and delayed observations occur within this test's GPUI scheduling and forced drawing; these measurements do not establish equivalent production delays or isolate a safe change to the UI scheduler. The value-ready-to-return interval is highly variable and includes RPC encoding, dispatch, decoding and scheduling. No startup behavior was changed on this evidence.

The prior durability proposal remains: investigate reducing registry history or making settlement and snapshot persistence cheaper, but preserve boot migration persistence and authoritative settlement until crash/restart and migration ordering are designed and tested. Do not defer either operation solely on this synthetic profile.

A second five-run serial batch added a gated RPC server timing point. The 178,736-byte outline server frame took 0.114 ms median (0.109–0.124) to encode; JSON value ready to wire send was 0.141 ms (0.133–0.145), and wire send to checked-subscription return was 47.879 ms (0.702–63.569). That last interval includes the in-memory channel, client frame parsing and routing, Tokio and GPUI scheduling; it does not isolate any of them. Watch spawn to subscribe was 50.252 ms (38.163–57.352), core assembled to chats applied 3.984 ms (3.134–13.012), and chats applied to observation 51.225 ms (39.304–61.201). These are a separate batch, not before/after performance results. The additional encoded-size measurement described above remains enabled.

A next investigation should timestamp RPC client receive, frame parse and routing, and instrument GPUI foreground task polling and drawing during this fixture. Compare against a representative production startup trace before changing scheduler priority or moving UI work: the test drives a current-thread runtime with `run_until_parked`, so its scheduler delays may not transfer to a running app. Any scheduler change needs evidence that it retains notification ordering and loaded content, plus regression coverage and serial before/after timings.

Verification: `cargo test -p mast-ui --lib -- --test-threads=1` passed 2,473 tests (13 ignored); `cargo test -p mast-engine --lib work_outline -- --test-threads=1` passed 10; `cargo test -p mast-rpc --lib -- --test-threads=1` passed 19. Both five-run installed-profile benchmark batches passed. Tests used a clean target directory, temporary profiles, inert `gh` and `linear`, and `MAST_NO_LOGIN_SHELL=1`.
