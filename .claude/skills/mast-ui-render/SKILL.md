---
name: mast-ui-render
description: Render and review Mast UI changes with the offscreen preview before handoff.
---

Follow `docs/internal/ui-preview.md` for the review workflow. For interactive
features, use `scripts/ui-flow.sh --publish` with the confirmed scenarios.
Seed engine data only and use UI actions to set view state. Pass the printed
arguments to `artifact_publish` so the work item has the flow review, with one
light storyboard per flow and a branch and SHA header. Include the manifest
verdict and artifact link in the handoff.

For a single-screen styling change, follow the render and merge-base comparison.
Previews shown to David are one image of Mast light only, rendered with
`scripts/ui-preview.sh --light-only` and one screen, style and layout, never a
light/dark grid or a hand crop. Publish the relevant sheet using `artifact_publish` with
kind `image`, and include its artifact link in the Ready to merge handoff.
Use the manifest path printed by that run. Verify its full SHA, branch and dirty
flag before using the image as evidence. Check the effective screens, styles,
layouts, themes and fixture values under `env`. The published sheet carries the
branch and short SHA in its footer.
Report any render or visual verification gap explicitly.
