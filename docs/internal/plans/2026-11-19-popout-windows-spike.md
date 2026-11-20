# Pop-out windows: spike (plan area A1)

Evidence is from release 114 (`crates/ui/src`).

## 1. What lives on `Shell`, and what a second `Shell` would break
- `Shell` (`shell.rs:485-895`) is the whole app: sidebar, route, overlays, tweens, viewport (`viewport_width`), jump hints, `active_chat`, and every right-pane surface in per-Shell maps (`diffs`, `file_surfaces`, `browsers`, `artifact_tabs`, `subagent_tabs`, `review_hosts`, `right_tabs`).
- `Shell::new` (`shell.rs:897`) also does global duties: a 1 s `_ticker` (`:971`, dock/status light, running and admin ticks), `apply_keymap`, `state.set_change_requests_visible`, a state observer that plays sounds and posts banners (`:1566-1602`, `required_actions_ui.rs:51-68`), the update flow, menu bar sync and dock sync.
- A second `Shell` would double all of it: chimes, banners, dock badge, the 1 s tick, update checks, keymap rebinding.
- Engine streams are not per Shell. They live in `AppState` (`state/watch_tasks.rs`; one selected-chat watch, one watch per subagent doc in `sub_watch_tasks`). Two windows over one `AppState` share them.
- Per-surface streams belong to the surface entity: `Changes` has its own diff watch, `FilesSurface` its own fs watch. Copying a surface into a second window would double those. Moving the entity does not.
- Global singletons are process-level, not per window: menu bar and `ReopenState` (`lib.rs:174`, `:296`), URL scheme and deep-link queue (`lib.rs:288`), notification click queue, appshot service (`lib.rs:588`).

## 2. Design: recommend the light `PopoutWindow` root
- `PopoutWindow` is a second root view type, not a `Shell`. It renders one surface under a small title strip. It has no sidebar, ticker, sounds or menu duties.
- Surfaces are mostly self-contained entities. `FilesSurface`, `Changes` and the subagent `Transcript` render as `child(entity)`. `ReviewHost` and `ArtifactTab` already hold a `WeakEntity<Shell>` to the main Shell, which outlives popouts.
- Pop-out is a tear-off: the entity moves from the Shell's maps to the popout. There is one diff watch and one editor buffer, so nothing doubles and unsaved text moves with the file. The popout re-hosts the surface's events. Navigation events (open file, open commit) forward to the main Shell. Dirty-close uses the existing `CloseReady`/`CloseCancelled` protocol.
- Not re-plumbed in this release:
  - Browser and artifact web views: the native web view presentation pass is in `shell/frame_state.rs:94-150`, tied to the main window's pane geometry.
  - Terminal: one `right_terminal` panel per Shell.
  - Item and Tracker/Team: `render_item_tab`/`render_team_tab` are Shell methods (~2.7k lines of `team_ui.rs` over Shell state).
  - Chat: `AppState` has one `selected_chat` and one `transcript` (`state.rs:222`, `state/transcript_session.rs`), and the composer is bound to it. A popped-out chat needs a per-chat watch with ref-counting, which is a state refactor. The subagent path (`sub_transcripts`) is the precedent.
- A full second `Shell` with a flag would need ~15 gates across `new`, the ticker, the observer and the sounds. Every future global duty would need one too. Rejected.

## 3. Deep links and capture routing
- Deep links go to `AppState::open_deep_link` (`state/engine_lifecycle.rs:268`), which selects a chat. Only the main `Shell` follows selection, so deep links, banner clicks, menu bar actions and Appshot delivery stay with the main window. They find it by `downcast::<Shell>()` (`lib.rs:392`, `:421`, `:692`), and a `PopoutWindow` is a different type, so it never matches by accident.
- Appshot capture already refuses while any Mast window is active (`appshot_activation_tests`). When a popout is focused and a capture is delivered, it goes to the main window. Falling back to the main window is the only option, since a popout has no composer.
- Quit and ⌘W: closing a popout closes only that window. `on_reopen` and `windows().is_empty()` checks are unchanged. A popout alone does not keep the app "open" differently from today.

## Decision for phase 2
Built: File, Diff (Changes) and Subagent tabs. Review is derived from an artifact (one host per chat, not a closable tab) and Artifact tabs can hold a native web view, so both join Browser, Terminal, Tracker, Item and chat as follow-ups. Session-only window frames; persistence is a follow-up.
