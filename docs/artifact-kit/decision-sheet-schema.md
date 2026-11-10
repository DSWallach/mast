# Decision sheet artifact shape (proposal)

This is a future `decision-sheet` artifact kind, not an implemented renderer. JSON strings are plain text; arrays retain display order. Dates are ISO 8601 calendar dates (`YYYY-MM-DD`). Every row's `status` is an object `{ "state": "decided" | "open" | "rejected", "date": "YYYY-MM-DD" }`; date is required for decided and rejected, and may be null for open. IDs are stable within the sheet. An absent optional value is null, not an inferred decision.

```json
{
  "kind": "decision-sheet",
  "title": "Schedule sections",
  "summary": ["1 model", "1 open"],
  "models": [{
    "id": "section", "name": "ScheduledSection",
    "status": {"state": "decided", "date": "2026-11-09"},
    "fields": [{"name": "start_date", "marker": "+", "type": "date",
      "null": false, "default": null, "constraint": "≤ end_date",
      "status": {"state": "decided", "date": "2026-11-09"}}]
  }],
  "endpoints": [{"id": "save", "name": "POST save",
    "request": [{"marker": "+", "text": "schedule_version", "status": {"state": "decided", "date": "2026-11-09"}}],
    "response": [{"marker": "−", "text": "operation_id", "status": {"state": "rejected", "date": "2026-11-09"}}],
    "status": {"state": "decided", "date": "2026-11-09"}}],
  "rules": [{"id": "reason", "columns": ["reason", "event"],
    "rows": [{"state": "new", "cells": ["—", "✓"], "status": {"state": "decided", "date": "2026-11-09"}}]}],
  "validations": [{"rule": "start ≤ end", "enforced": ["UI", "API"], "result": "invalid", "status": {"state": "decided", "date": "2026-11-09"}}],
  "permissions": [{"subject": "sections", "access": {"customer": "hidden", "warehouse": "write"}, "status": {"state": "decided", "date": "2026-11-09"}}],
  "flows": [{"id": "save", "title": "Save", "steps": [{"kind": "api", "label": "POST", "params": ["schedule_version"], "status": {"state": "decided", "date": "2026-11-09"}}], "status": {"state": "decided", "date": "2026-11-09"}}],
  "ui_states": [{"id": "conflict", "label": "Reload", "image": null, "status": {"state": "open", "date": null}}],
  "out_of_scope": [{"label": "Backfill", "status": {"state": "rejected", "date": "2026-11-09"}}]
}
```

Field markers are `+`, `~`, `−`, or empty (unchanged); endpoint line markers are `+`, `−`, or empty. Rule cells are `✓`, `—`, or `?` in column order. Flow kinds are `ui`, `api`, `db`, `event`, `effect`, and `none`; `none` means the step does not happen. `image` is an optional artifact reference. A future renderer must validate these values and preserve each row's status and date.
