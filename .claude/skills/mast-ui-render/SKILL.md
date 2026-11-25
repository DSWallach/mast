---
name: mast-ui-render
description: Render and review Mast UI changes with the offscreen preview before handoff.
---

Follow `docs/internal/ui-preview.md` for the render, crop, merge-base comparison,
and review workflow. Previews shown to David are one image of Mast light only: render them with
`scripts/ui-preview.sh --light-only` and one screen, style and layout, never a
light/dark grid or a hand crop. Publish the relevant sheet using `artifact_publish` with
kind `image`, and include its artifact link in the Ready to merge handoff.
Use the manifest path printed by that run. Verify its full SHA, branch and dirty
flag before using the image as evidence. Check the effective screens, styles,
layouts, themes and fixture values under `env`. The published sheet carries the
branch and short SHA in its footer.

Report any render or visual verification gap explicitly.
