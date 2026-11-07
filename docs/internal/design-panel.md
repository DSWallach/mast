# Live design overrides

After installing a build with this feature, open Settings → Developer and select
**Enable Design**. The switch is off by default and is stored in UI settings.
Changes apply to the currently selected Style immediately; use Appearance to
change Style or Layout. The controls apply to the actual running app.

The sliders change control, panel, chip and bubble radii, control height,
sidebar row height, grid size, small spacing and rule width where components use those tokens.
The sidebar separator toggle changes the sidebar rule. The details controls affect the work-item details frame: its border and whether
its background uses the page colour. The outline control changes the tree and overview header background. Primary and secondary colour choices affect
shared buttons, toolbars, selected sidebar and outline rows, chat header chips,
and composer keys. Heading family and weight affect settings page headings;
tracked caps also affect sidebar section labels and composer captions. The surface
controls select a border and page-colour background for the toolbar, sidebar row,
chat header chip, composer, message, tool, code and outline row. The reset state
preserves each component's existing style-specific behavior. Reset clears an
individual override; Reset style clears all overrides for that style, including
layout-specific overrides. Save writes the file. Copy patch places a human-readable
summary on the clipboard for the coordinator to translate into defaults.

`style-overrides.json` sits next to `ui-settings.json` in the profile settings
directory. Its version is `mast.style-overrides/v1`; style-level tokens apply to
all layouts and entries under `layouts` take precedence. Edits to the file are
polled and applied while Mast runs. An absent or empty file has no effect. Invalid
fields and unknown names are ignored with warnings. Use Reset and Save to remove
an override; deleting the file removes all overrides.

```json
{
  "version": "mast.style-overrides/v1",
  "styles": {
    "area": {
      "controlRadius": 16,
      "detailsBorder": true,
      "primaryColour": "accent",
      "layouts": { "compact": { "controlHeight": 26 } }
    }
  }
}
```

Numeric values are pixels in the range 0–200. Colour roles are `accent`, `text`,
`muted`, `success`, and `danger`. Settings heading families are `sans` and `mono`;
weights are `regular`, `medium`, `semibold`, and `bold`. The panel changes
style-wide values and clears the corresponding current-layout value so the edit
is visible immediately. Overrides are prototypes, not a replacement for updating
the compiled style defaults.

The component gallery reads the same override file without writing to it. Once
Save is pressed in Mast, the gallery refreshes its side-by-side columns using
that file and resolves each column's own Style and the selected Layout. Draft
slider changes stay in Mast until saved. The gallery keeps its other settings in
a separate temporary directory; `MAST_DATA_DIR` (or `ZERON_DATA_DIR`) selects a
non-default profile directory for reading overrides.

Surface overrides can also be supplied under `surfaces`, for example:

```json
{ "version": "mast.style-overrides/v1", "styles": { "area": {
  "surfaces": { "composer": { "border": true, "pageColour": false } }
} } }
```

Surface keys are `toolbar`, `sidebarRow`, `headerChip`, `composer`, `message`,
`tool`, `code`, and `outlineRow`. Their controls apply where those shared
components render, not to unrelated hand-built chrome.
