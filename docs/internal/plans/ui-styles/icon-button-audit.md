# Icon button audit

This audit is incomplete. Lines refer to the Guided branch and may move.

| Location | Controls | Guided |
| --- | --- | --- |
| `crates/ui/src/shell/tabs.rs:393` | Expand, Files, Changes, Rename | Shared header control labels when space permits. |
| `crates/ui/src/files/mod.rs:45` | File, browser, artifact toolbar controls | Shared toolbar labels and 32 px height. |
| `crates/ui/src/composer.rs:8093` | Send, Queue, Stop | Labelled and 32 px high. |
| `crates/ui/src/work_outline/details_panel.rs:1365` | Close details | Labelled and 32 px high. |
| `crates/ui/src/shell/spaces.rs:3850` | Overview and sidebar view options | 32 px; labels when the sidebar is wide enough, tooltip otherwise. |
| `crates/ui/src/shell.rs:6670` | Sidebar toggle, back, forward, new session | Not yet converted; fixed-width titlebar requires width-aware layout. |
| `crates/ui/src/shell.rs:11200` | Add surface | Not yet converted. |
| `crates/ui/src/comment_ui.rs:18` | Add comment in diff gutter | Not yet converted; constrained gutter needs a layout decision. |
| `crates/ui/src/comment_ui.rs:148` | Edit comment | Not yet converted; card header has a fixed height. |
| `crates/ui/src/shell/tabs.rs:585` | Copy chat name and IDs | Already labelled in all styles. |

The remaining inline icon controls throughout the UI still need inspection,
conversion, and narrow-width verification. The shared toolbar can also need
complete label collapse in constrained panes; labels currently truncate as the
controls shrink, while the tooltip retains the full text.
