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

## Changes pane (coordinator's suspects, measured after merging origin/work-outline-implementation)

Fixture: `perf/diff_bench.rs`, 250 chats in AppState, scoped diff of 1–4 MB. Run
`changes::perf_bench::bench_diff_pane_5000_lines` and `bench_diff_pane_state_notifies`.

| Suspect | Measured | Verdict |
| --- | --- | --- |
| 1. `active_diff`/`resolved` clone the `CheckoutDiff` | `active_diff` allocated 1,032,190 bytes per call for a 1 MB patch (it runs in render, sync and every highlight miss). Idle frame allocation at 300 files (3.9 MB patch) 11,179 → 4,869 KiB; draw time moves only ~0.05 ms in the debug build (memcpy is fast, the cost is allocator churn and page faults) | Real. Fixed: both return `Option<&CheckoutDiff>`; the three places that need an owned diff (async fetch, expand context, parse spawn) clone there, once. Test `active_diff_does_not_copy_the_patch`: 0 bytes/call after, 1,032,190 before |
| 2. `observe(&state, sync)` runs the whole sync on every AppState notify | 1 sync + 1 render per notify. Notify + draw 2.00 ms vs idle draw 1.96 ms with 250 chats, so sync itself adds ~0.04 ms once the diff clone is gone | Real only because of the clone; fixed by 1. Gating on a revision is not worth the risk at this cost; left as is |
| 3. `max_shaped_text_width` shapes every line | A 5,000-line single file shapes 10,000 lines during load + first draw: once without highlights, once when the highlights arrive. Multi-file diffs shape only the visible files (400 lines at 1, 50 and 300 files). Per row, `metrics()` is a cached compare. The headless text system shapes in microseconds, so the ms cost on CoreText is not measurable here | Real in count (2 full passes per visible file, on the render thread), unmeasured in ms. Not fixed: skipping the first pass changes widths, and moving it off-thread needs a width placeholder that moves the scrollbar (visible). Recommend: shape on the background parse/highlight task and keep the previous width until it lands; hoist `ch_advance` out of the per-row path |

## Cached transcript: audit of inputs that change its pixels

gpui `.hover()` and group-hover styles call `cx.notify(current_view)`; rows are built inside the Transcript's render, so
they notify the Transcript. `window.request_animation_frame` and `with_animation` also notify the current view.
Audited `transcript.rs` and the markdown handlers:

- Row hover (`on_hover`, `hovered_entry`): notifies when the entry changes. Test pointer moves repaint even with that
  notify removed, because the hover styles notify too.
- Text selection (markdown `begin`/`update_drag_at`/`clear_if_owner`): `window.refresh()`, which bypasses view caches.
  Drag autoscroll and mouse up in `transcript.rs` call `cx.notify()`.
- Link hover and code-block scrollbar hover/fade (`markdown/link_interaction.rs`, `render.rs`): `window.refresh()` or
  `cx.notify()` on the owning entity when the state changes.
- Streaming text and the working trailer: `sync` and the shell's 1 s heartbeat notify the Transcript.
- Settings, theme, appearance: `refresh_windows`.
- Handlers that only run `cancel_user_hold` (user bubble press/move) change no pixels.

Tests (`shell/perf_tests.rs`, real pointer events in the full Shell): `hovering_a_row_repaints_the_cached_transcript`,
`selection_drag_repaints_the_cached_transcript`, `streamed_text_repaints_the_cached_transcript_without_other_notifies`.
Not covered headless: scrollbar fade timing and copy-button reveal (they notify on the owning entity; check in the app).
