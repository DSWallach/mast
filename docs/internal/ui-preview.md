# UI preview

Run from any Mast checkout on macOS with a Metal device:

```sh
scripts/ui-preview.sh
scripts/ui-preview.sh --themes industrial-light,industrial-dark --screens chat,details --styles default,industrial --layouts minimal,compact,guided
```

The command renders the chat shell, outline and item details, walkthrough, Settings → Appearance, and controls for every UI style. It always includes `mast-light` and `mast-dark`; `--themes` (or `MAST_PREVIEW_THEMES`) adds comma-separated built-in theme IDs. `--screens`, `--styles`, and `--layouts` restrict the selection; their environment equivalents are `MAST_PREVIEW_SCREENS` and `MAST_PREVIEW_STYLES`. Screen IDs are `chat`, `details`, `walkthrough`, `appearance`, `controls`, and `components`. The `components` screen shows every style/layout pair side by side in each selected theme. Style IDs are `default`, `industrial`, and `area`; layout IDs are `minimal`, `compact`, and `guided`. Unknown selections fail rather than silently using a fallback.

The command prints a contact sheet for each screen and an index under `$CARGO_TARGET_DIR/ui-preview/<timestamp>/` (default: the checkout's `target`). Rows are style/layout combinations, columns are themes, and each cell has a GPUI-rendered label. Sheets are capped at 4096 pixels per side. The render is offscreen and does not open an app window or capture the screen.

The command must run **outside the Codex sandbox**: Metal and macOS services are unavailable inside it. Codex workers should prepare the command and request one escaped run; Claude Code workers and the coordinator can run it directly.

## UI changes: preview before handoff

For UI changes, use this loop before handoff (also available as the `mast-ui-render` skill):

1. Render the affected screens, styles and layouts with `scripts/ui-preview.sh --screens chat --styles default,industrial --layouts minimal`. Keep the command and selections for the comparison. Use the headless renderer only; do not launch a GUI or capture the screen.
2. Inspect the sheet and crop important cells for closer review. Rows are style/layout combinations and columns are themes; use the dimensions of the printed sheet and divide by its row and column counts. For a cell at zero-based row `R`, column `C`, with height `H` and width `W`, use `sips --cropToHeightWidth H W --cropOffset $((R * H)) $((C * W)) sheet.png --out cell.png` (replace the variables with measured pixel values). Check the crop, including labels, at native resolution.
3. Compare with the merge-base using the same renderer and selections. Create a detached comparison worktree at `git merge-base HEAD origin/work-outline-implementation`, and use a **separate** `CARGO_TARGET_DIR` for it. Copy the current preview script and `crates/ui/tests/ui_preview.rs` and `crates/ui/tests/ui_preview/` into that disposable checkout so both revisions use the same renderer, then run the same command there. If those fixtures require APIs absent at the base, report the comparison gap instead of silently switching renderers. Compare corresponding crops and check for unintended changes, especially Default/Minimal.
4. Review the result yourself. Publish the relevant sheet with `artifact_publish` as an image, and include the returned `mast-artifact:` link in Ready to merge. Describe the tested selections, comparison and any visual issues or gaps.

Do not share one target directory between worktrees: Cargo can reuse or overwrite build outputs across checkouts, and concurrent builds contend on locks. Keep each checkout's target separate, including the merge-base checkout.

## Inspector spike

Build a separate development executable with `cargo build -p mast --features dev-inspector`. Mikey can launch it with `target/debug/mast` from the checkout; do not launch it alongside the installed Mast against the same live profile. The feature enables GPUI's inspector instrumentation, but it does **not** currently produce a usable inspector in Mast: GPUI's `Inspector::render` returns an empty element unless the application registers an inspector renderer, and Mast registers neither that renderer nor a toggle action. The fork has picking and element-state infrastructure, but there is currently no inspector panel or shortcut to show or edit a selected element. Adding the feature alone does not demonstrate live editing or source locations. The executable has not been launched for this spike.

## Rust hot-patching spike

[Subsecond 0.7.10](https://docs.rs/subsecond/0.7.10/subsecond/) can redirect calls to patched functions and lists macOS support, but its documented integration needs a patching tool and runtime patch application. Its documentation currently says only the executable's tip crate is patched, and changes to struct layout require re-instancing or discarding state. Mast's views and components live in the separate `mast-ui` library, hold GPUI entities, and use process-global style state. Therefore the documented setup does not establish that an ordinary edit to a Mast view can safely update a running preview. A useful proof would put a leaf view in the tip crate, integrate patch delivery and invalidation, and prove an offscreen before/after render while preserving compatible state. This spike has not implemented or measured that proof. Dylib reloading has the same unresolved state and code lifetime boundary: GPUI entities and callbacks must not outlive unloaded code, and Rust layouts are not a stable plugin ABI. Treat hot-patching as experimental research, not an available preview speed-up.
