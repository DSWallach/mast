# Decision sheet JSON

Publish a UTF-8 `.json` file with `artifact_publish`, `kind: "decision-sheet"` and a title. See
[decision-sheet-example.json](decision-sheet-example.json) for all sections. Arrays retain display
order; strings are plain text. Unknown keys are rejected. Summary chips are computed by Mast.

The top-level object has `kind`, `title` and optional arrays `flows`, `models`, `endpoints`, `rules`,
`validations`, `permissions`, `ui_states`, and `out_of_scope`. Every object in those arrays and
all nested row objects need a nonempty `id`, unique throughout the sheet and stable across
revisions. Every row except the rules container has `status: {"state": "open" | "decided" |
"rejected", "date": ...}`. Open rows use a null date; decided and rejected rows require an ISO
calendar date (`YYYY-MM-DD`). Keep IDs when republishing; do not reuse an ID for a different row.

- Models have `name`, `fields` (`name`, `marker`, `type`, `null`, `default`, `constraint`).
- Endpoints have `name`, `request` and `response` lines (`marker`, `text`).
- Rules have `columns` and `rows` (`state`, `cells`); cells are `✓`, `—`, or `?`, one per column.
- Validations have `rule`, `enforced` (strings), and `result`. Permissions have `subject` and
  `access` (subject-to-access string map).
- Flows have `title` and `steps` (`kind`, `label`, `params`); kinds are `ui`, `api`, `db`, `event`,
  `effect`, and `none`.
- UI states have `label` and `image` (a `mast-artifact:` reference or null); references render as links to the screenshot; out-of-scope rows have `label`.

Field markers are `+`, `~`, `−`, or empty; endpoint markers are `+`, `−`, or empty. Nullable
`default`, `constraint`, and `image` are strings or null.
