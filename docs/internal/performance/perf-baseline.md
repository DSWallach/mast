# Performance baseline and benchmarks

Measured 2026-10-29 on Mikey's M-series Mac (51 GB), `installed` profile, machine under normal
load (other agents building). Numbers vary ±20% run to run; compare back-to-back.

## How to re-run

All benches are `#[ignore]` tests, so `cargo test` never runs them. Use the `installed` profile
(what the app is built with) and `--test-threads=1`. Each prints `BENCH <area> | <label> | ms=…`
lines; the ui ones also print allocations per iteration (a counting allocator is installed in the
ui test binary).

```sh
# UI: transcript (2000 msgs, 17000 rows), markdown (300 KB), diff (5000 lines), overview (250 chats, 40 items + PR/Linear children)
cargo test -p mast-ui --lib --profile installed -- --ignored --nocapture --test-threads=1 bench_

# Engine: cold start (empty and 250-chat data dirs), SWR cache cold load / persist
cargo test -p mast-engine --profile installed --test engine perf_bench:: -- --ignored --nocapture --test-threads=1

# Git spawn latency
cargo test -p mast-harness --profile installed --test perf_bench -- --ignored --nocapture
```

`BENCH_SCALE=30` repeats every loop 30× (long enough to attach `sample <pid> 10`);
`BENCH_ONLY=stream` limits the transcript bench to the streaming tick.

### Benches on a copy of the real profile

Never point a bench at the live `~/.zeron`. Make a copy (read-only on the source) and run with a
throwaway `HOME` so the engine's external-session auto-adopt scans an empty directory:

```sh
D=/tmp/perf/real; mkdir -p $D/profiles/local /tmp/perf/fakehome
sqlite3 -readonly ~/.zeron/profiles/local/docs.sqlite3 ".backup $D/profiles/local/docs.sqlite3"
cp -c -R ~/.zeron/profiles/local/journals $D/profiles/local/journals   # APFS clone, ~250 MB
cp ~/.zeron/local-profile.json $D/
cargo test -p mast-engine --profile installed --test engine --no-run   # prints the test binary
HOME=/tmp/perf/fakehome MAST_PERF_REAL_COPY=$D <that binary> --ignored --nocapture --test-threads=1 perf_bench::
```

(`bench_engine_startup_real_profile_copy`, `bench_registry_session_heartbeat_publish`,
`bench_journal_scans_real_copy`.) Delete the copy afterwards; it holds real chat data.

## Baseline (after the fixes on `perf-baseline`)

| Area | Measurement | Result |
|---|---|---|
| Transcript, 2000 msgs / 17000 rows / 11 MB | open chat (sync) | 50 ms, 333 MB allocated |
| | frame (idle redraw, scroll) | 0.5 ms, 2.5 MB allocated |
| | streaming tick (sync + draw) | 3.2 ms |
| Markdown preview, 300 KB (1898 blocks) | parse_full | 2.3–3.5 ms, 27 MB allocated |
| | set_source + settle | 11 ms |
| | frame (idle, scroll) | 0.5–0.7 ms, 0.7–1.0 ms |
| Diff pane, 5000 lines (4968 rows) | load (parse + flatten) | 18 ms |
| | frame (idle, scroll) | 0.8 ms, 1.2–1.6 ms |
| Overview, 250 chats, 40 items + PR/Linear children | `rows()` rebuild / cached | 0.35 ms / 0 |
| | Outline frame (clean / rows dirty) | 1.1 ms / 1.7 ms |
| | List frame | 44–55 ms, 335 MB allocated (not virtualized) |
| Engine, synthetic 250 chats + 120 work items | cold `EngineCore::assemble` | 17 ms (empty dir: 13 ms) |
| Engine, copy of real profile (331 chats, 297 journals, 258 MB) | cold assemble | 330–380 ms (was 920–950) |
| | `read_chats` steady state | 0.9–1.1 ms (was 2.4–3.2) |
| | session heartbeat → sessions watch | 0.13 ms (was 2.14) |
| | `stale_sessions()` all journals | 5–7 ms (was 440–635) |
| SWR cache (8 keys × 300 Linear issues, 4 MB) | `attach_file` cold load | 2–3 ms |
| | `get(force)` + persist whole cache | 2 ms |
| Harness | `git --version` spawn() call / total | 0.8 ms / 9.6 ms (first call also pays one login-shell PATH capture, ~0.9 s, prewarmed at boot) |

## v0.3.0 distributions (Overview and SWR cache)

Measured on the Linux desktop build host (slot `a`, `installed` profile, one `--quiet-window` job per
run, 5 runs each, median ± MAD in ms). Overview rows were measured at `6693e260c` (v0.3.0 `e4d133879` plus bench code only; the Overview code is unchanged by this task). The slot's environment file sets `CARGO_INCREMENTAL=1`, so the
bench binaries were built incrementally, not with `CARGO_INCREMENTAL=0`. Samples are printed as
`SAMPLE <bench> | <size> | <metric> | ms=`.

```sh
cargo test -p mast-ui --lib --profile installed -- --ignored --nocapture --test-threads=1 bench_overview_250
cargo test -p mast-ui --lib --profile installed -- --ignored --nocapture --test-threads=1 bench_overview_1000
cargo test -p mast-engine --profile installed --test swr_bench -- --ignored --nocapture --test-threads=1
```

### Overview (List mode, grouped by repo for the reorder row)

| Metric | 250 chats | 1,000 chats |
|---|---|---|
| Cold open (first List draw) | 5.80 ± 0.05 | 8.88 ± 0.55 |
| Idle redraw | 3.97 ± 0.11 | 4.05 ± 0.07 |
| Scroll (90 px per frame) | 9.30 ± 0.06 | 9.59 ± 0.04 |
| Grouped reorder (one chat moves, rows dirty) | 15.75 ± 0.16 | 21.30 ± 0.05 |

The List is virtualized, so the "not virtualized, 44–55 ms" hot spot above is out of date. Idle redraw
and scroll are flat from 250 to 1,000 chats (+0.09 ms and +0.29 ms), so the per-frame presentation
build in `list_rendering.rs` is not where the time goes; only the reorder, which rebuilds rows, grows
with the chat count. Presentation caching could save at most the +0.09 ms idle difference, below
2 × MAD, so **no Overview change was made**.

### SWR cache lock hold

A reader thread polls the store while a writer runs `get(force)` 20 times on one key of a cache holding
N keys (300 issues each). The reader times `peek("absent")` (pure lock wait: no clone, no allocation)
and `peek("key0")` (lock plus the entry clone). 10 alternating quiet-window runs per build, median ± MAD
in ms. "Before" is v0.3.0 `e4d133879` with the bench commits (`7356a92e5`); "after" is `6ebc35713`
(`SwrStore::finish` builds the stored copy before the lock, frees the replaced entry after it, and the
JSON is built in the writer from an `Arc` snapshot; `peek`/`get` copy the entry after releasing the lock).

| Keys | Metric | Before | After |
|---|---|---|---|
| 1 | lock wait, reader max | 1.141 ± 0.013 | 0.001 ± 0.000 |
| 1 | `get(force)` | 0.809 ± 0.087 | 0.760 ± 0.053 |
| 8 | lock wait, reader max | 4.152 ± 0.104 | 0.001 ± 0.001 |
| 8 | `get(force)` | 3.134 ± 0.059 | 1.000 ± 0.077 |
| 16 | lock wait, reader max | 8.020 ± 0.109 | 0.001 ± 0.000 |
| 16 | `get(force)` | 5.648 ± 0.037 | 0.387 ± 0.006 |
| 32 | lock wait, reader max | 19.782 ± 0.516 | 0.002 ± 0.001 |
| 32 | `get(force)` | 16.860 ± 1.500 | 0.250 ± 0.004 |

The cloning `peek("key0")` is a separate, allocator-bound number: its worst case was 0.76 / 0.55 / 0.16 /
0.17 ms before and 1.12 / 0.32 / 0.91 / 0.26 ms after at 1 / 8 / 16 / 32 keys (MADs 0.01–0.12). Before, that
call also waited on the lock and the larger lock-wait figure above dominated the reader's total stall; the
reader's worst total stall (the larger of the two) is lower or equal at every size (1.14→1.12, 4.15→0.32,
8.02→0.91, 19.78→0.26 ms). Mean `peek` is 0.09–0.11 ms in both builds.

**The 8-key anomaly.** An earlier version of the change (`1d5bb8cdc`, entry cloned into its `Arc` and the
replaced entry dropped under the lock) measured a bimodal pure lock wait at 8 keys (median 5.3 ± 4.6 ms; 0.5
to 10.5 ms across runs), where 16 and 32 keys were clean. A control (reader on an idle store while another
thread only serializes the same JSON, 10 runs) stalled at most 0.02–0.03 ms, so the stall was not host
scheduling or serialization noise; it was allocation and free work inside `finish` while it held the lock.
Moving both off the lock removes it: 8 keys is now 0.001 ms in all 10 runs.

Overview keyboard selection, scroll anchor, hover and cold reopen code is untouched by this task. The SWR
unit tests (cold reload, cleared-cache write ordering, shared in-flight fetch) pass at the final SHA.

## Fixes in this branch (each commit has its own before/after)

1. **Transcript rail cache** (`rail.rs`, `transcript.rs`): the rail rebuilt its ticks every frame
   (deep clone of the whole transcript + O(ticks × rows) lookup). Frame 7.8 ms → 0.5 ms,
   streaming tick 17.7 ms → 3.2 ms at 17000 rows.
2. **Registry rows read in place** (`doc/src/registry.rs`): `read_kind` copied every row twice.
3. **Registry publish per kind** (`engine/src/workspace_host.rs`): `publish_lists` re-read all
   four kinds on every registry change under the registry lock; now only changed kinds.
4. **Journal tail scans** (`engine/src/run_journal.rs`): `stale_sessions`, `last_event` and the
   first append parsed whole journals (up to 44 MB) to read one line; now a backwards scan.
5. **Boot recovery / run start read the journal from the end** (`engine/src/sessions.rs`):
   `recover_completed_tails`, `journal_actual_model`, `tool_blob_from_journal`,
   `journal_harness_session` use `RunJournal::find_rev` / `replay_last_turn`.

## Ranked remaining hot spots

1. **Overview List mode is not virtualized**: `overview.rs:5430 render_list_body` builds 250
   `render_list_row` elements every frame (taffy layout dominates): 44–55 ms and 335 MB per frame.
   Mikey uses Outline (1.1 ms), so it only bites when switching mode. Fix: gpui `list`/uniform
   list with the group headers as items; the `scroll_to_item(focus_child)` and hover logic need care.
2. **Work Outline recompute on every chat/session write**: `work_outline_inputs_generation`
   (`doc/src/registry/work_outline.rs:434`) includes chats and sessions, so each agent heartbeat
   re-materializes every watched Space (33–36 ms for all Spaces on the real copy;
   `WorkOutlineService::watch` was the top non-git engine frame in an idle sample). Fix: compare
   only the chat/session fields the outline reads.
3. **diff-sync captures**: each kick runs 7 `git` processes, four of them full diffs in
   sequence (`diff_sync.rs:2224 capture_diff_against`), and the 120 s repair tick re-kicks every
   watched checkout (22 repos log "tree too large to watch live"). Fix: skip when HEAD, index
   mtime and `status` fingerprint are unchanged; one `git diff --raw --numstat -z`; run the
   independent captures concurrently.
4. **Transcript sync is O(entries) per tick** (`transcript.rs:4488 sync`): clones every cached
   `Vec<Row>`, SipHashes ids, fingerprints every entry. 3.2 ms at 2000 messages; opening the chat
   allocates 333 MB for 11 MB of payload. Fix: incremental by revision, FxHash, `Rc` rows.
5. **Artifact question sync re-sends an operation id with a changed payload**
   (`artifact_questions.rs:503`, log `operationConflict`, 44 warnings in 1.5 h): the write never
   succeeds, so it is retried on every sweep. A correctness bug more than a perf one.
6. **RSS swings 1.6–6 GB** within a minute while agents run (see the report); no single cause
   found without a heap profiler. The boot journal parse (fixed above) was one transient source.
7. `git` spawns take the `fork` path because `git_command()` sets `PATH` (std only uses
   `posix_spawn` for absolute programs): 0.8 ms per spawn, not worth changing alone.
