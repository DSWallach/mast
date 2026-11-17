# UI render pass (2026-11-17)

Measured with headless gpui tests, debug profile (`cargo test -p mast-ui --lib`), so absolute numbers are several times
the installed build; compare before/after only. Branch `perf/ui-render-pass`.

Reproduce:

- `mast_ui --ignored --nocapture --test-threads=1 shell::perf_tests::bench_long_chat` (`BENCH_ENTRIES`, default 360
  entries = 3,060 rows; full Shell, prepared rows installed like the watch does)
- `... transcript::perf_bench::bench_subscribed` / `bench_transcript_long_chat` (17,000 rows; `BENCH_TICKS`)
- `... overview::perf_bench::bench_outline_500_items_actions` (`BENCH_ROOTS`, default 167 roots = 501 items)

## Measurements

Long chat, 3,060 rows, full Shell (ms per op, transcript rows rendered per op, notifies per op):

| Action | Before | After |
| --- | --- | --- |
| Stream 1 delta + draw (n=200) | 6.35 ms, 13.7 rows, 1 transcript notify | 3.64 ms, 6.2 rows, 1 transcript notify |
| Composer keystroke + draw (n=50) | 4.12 ms, **12 rows**, 0 transcript notifies | 0.96 ms, **0 rows**, 0 transcript notifies |
| Switch chat + draw (n=10, prepared rows cached) | 14.7 ms, 17.8 rows; shell/composer/sidebar/transcript 1 notify each | 12.4 ms, 8.5 rows; same notifies |
| Scroll to top / bottom + draw | 3.6 / 3.2 ms | 5.4 / 3.1 ms (hook now notifies like a real scroll; before it did not repaint) |
| Open (load + prepare + draw) | 51 ms | 45 ms (prepare runs on a background thread in the app) |

Long chat, 17,000 rows (11 MB):

| Action | Before | After |
| --- | --- | --- |
| Subscription chunk, prepare + sync + draw | 26.0 ms, 19.3 MiB | 15.5 ms, 19.3 MiB |
| Stream tick, unprepared path (revision bump + sync + draw) | 20.1 ms | 13.6 ms |
| Idle redraw / scroll 700 px | 1.9 ms | 1.6 ms |

Work Outline, 501 items (167 PR rows), Outline mode: first draw 2.3 ms, clean redraw 2.05 ms, collapse + draw 6.6 ms,
expand + draw 6.8 ms, status change (new snapshot) + draw 10.2 ms. No change; not an offender at this size.

Where a streamed chunk went on the UI thread at 17,000 rows (`sample`, before): `Transcript::sync` 58%, of which the
code-fence key set 40%, previous-tool map and live tool group pass 18% each; the draw 40%, of which `render_rail` 49%.

Scenario 4 (switching): caches are not dropped. `select_chat` moves entries and prepared rows into `transcript_cache`
and restores both on return; rebuild per switch is a row clone plus the visible rows.

## Fixes (each with a budget test that fails on the old code)

1. **Transcript is a cached view in the Shell** (`shell.rs`). It was re-rendered on every window draw, so every keystroke
   (and cursor blink) re-rendered the visible rows. Test `composer_keystrokes_do_not_rerender_transcript_rows`:
   240 rows rendered over 20 keystrokes before, 6 after (one layout repaint). Settings and theme changes use
   `refresh_windows`, which bypasses view caches, so they still repaint.
2. **Code-fence handle prune is O(changed rows)** (`Transcript::prune_code_fences`). It built a key set from every
   markdown row (a `format!` and a `Vec` each) on every sync. Test `streaming_deltas_do_not_rescan_every_row`:
   36,600 `code_block_indices` calls over 40 deltas before, 367 after.
3. **Rail tick-to-row map is reused while ticks are stable** (`rail.rs reuse_rail_rows`). It hashed every row id on
   every delta. Same test: 40 full builds over 40 deltas before, 0 after.

## Recommendations, not done

- `Transcript::sync` still does three O(rows) passes per tick: the row clone, the previous-tool map and the live tool
  group loop (about 1 ms each at 17,000 rows, debug). Restrict them to the diff range when no baseline change or attach
  is in flight. Skipped: it touches tool-group reveal animation state.
- Background preparation per delta (off the UI thread, but ~10 ms CPU at 17,000 rows): `MessagePart::byte_len` JSON-
  serialises every tool call, diff and diff stat (`crates/doc/src/parts.rs`), and `TranscriptBaseline::capture` rebuilds
  an O(entries) map per update. Cache per entry; capture only when `replay_baseline` is set.
- `rail_ticks` clones every prompt and reply preview each delta (O(entries)). Cache by transcript revision of user entries.
- `select_chat` computes `byte_len` over every entry on the UI thread when no prepared rows exist.
- Overview List mode is still not virtualized (see perf-baseline.md).
- The fixtures do not model 10k-line exec output or large diffs: tool output and diffs are out-of-line refs in the
  transcript model, so their cost sits in the detail pane, not the row list. Not measured here.
- Hover, selection and own-turn state mutated from outside the transcript must notify it now that it is cached; I
  found no such path, and the full UI suite covers the shell, but check hover/selection in the app after install.
