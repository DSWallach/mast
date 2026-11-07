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
