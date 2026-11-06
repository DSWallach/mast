# Icon button audit

This audit is incomplete. Lines refer to the Guided branch and may move.

| Location | Controls | Guided |
| --- | --- | --- |
| `crates/ui/src/shell/tabs.rs:393` | Expand, Files, Changes, Rename | Shared header control labels when space permits. |
| `crates/ui/src/files/mod.rs:45` | File, browser, artifact toolbar controls | Shared toolbar labels and 32 px height. |
| `crates/ui/src/composer.rs:8093` | Send, Queue, Stop | Labelled and 32 px high. |
| `crates/ui/src/work_outline/details_panel.rs:1365` | Close details | Labelled and 32 px high. |

The remaining inline icon controls throughout the UI still need inspection,
conversion, and narrow-width verification. The shared toolbar can also need
width-aware label collapse in constrained panes.
