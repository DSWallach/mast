# UI preview

Run from any Mast checkout on macOS with a Metal device:

```sh
scripts/ui-preview.sh
scripts/ui-preview.sh --themes industrial-light,industrial-dark --screens chat,details --styles default,industrial --layouts minimal,compact,guided
```

The command renders the chat shell, outline and item details, walkthrough, Settings → Appearance, and controls for every UI style. It always includes `mast-light` and `mast-dark`; `--themes` (or `MAST_PREVIEW_THEMES`) adds comma-separated built-in theme IDs. `--screens`, `--styles`, and `--layouts` restrict the selection; their environment equivalents are `MAST_PREVIEW_SCREENS` and `MAST_PREVIEW_STYLES`. Screen IDs are `chat`, `details`, `walkthrough`, `appearance`, and `controls`. Style IDs are `default`, `industrial`, and `area`; layout IDs are `minimal`, `compact`, and `guided`. Unknown selections fail rather than silently using a fallback.

The command prints a contact sheet for each screen and an index under `$CARGO_TARGET_DIR/ui-preview/<timestamp>/` (default: the checkout's `target`). Rows are style/layout combinations, columns are themes, and each cell has a GPUI-rendered label. Sheets are capped at 4096 pixels per side. The render is offscreen and does not open an app window or capture the screen.

The command must run **outside the Codex sandbox**: Metal and macOS services are unavailable inside it. Codex workers should prepare the command and request one escaped run; Claude Code workers and the coordinator can run it directly.

## UI changes: preview before handoff

For UI changes, run the preview for the affected screens and styles, open the resulting PNGs in an image viewer, and review them before handoff. Attach the relevant sheet with `artifact_publish` (kind `image`) or link its path in the Ready to merge handoff. Report any visual issues or verification gaps.
