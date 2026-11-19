//! Provenance blame gutter: who and which Mast chat wrote each line.
//!
//! Off by default. Nothing is requested until the gutter is turned on for the
//! active file; one `GetWorkspaceFileBlame` per (file, revision) is cached by
//! the engine and held here as an `Arc<Blame>`. Rendering touches only the
//! visible rows (binary search over runs), never the whole file.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, Context, Entity, MouseButton, Pixels, Point, SharedString, Task, div, prelude::*,
    px,
};
use mast_proto::view::format_time_ago;
use mast_proto::{Blame, BlameChatConfidence, BlameCommit, BlameRun};

use super::{FilesEvent, FilesSurface, client::WorkspaceFilesClient};
use crate::theme::Theme;

/// Width of the blame column left of the editor.
pub(super) const BLAME_COLUMN_WIDTH: f32 = 196.0;
const HOVER_OPEN: Duration = Duration::from_millis(250);
const HOVER_CLOSE: Duration = Duration::from_millis(250);
const CHIP_MAX_CHARS: usize = 14;

#[derive(Default)]
pub(super) struct BlameState {
    pub(super) enabled: bool,
    path: Option<String>,
    blame: Option<Arc<Blame>>,
    status: Option<SharedString>,
    task: Option<Task<()>>,
    generation: u64,
    popover: Option<BlamePopover>,
    hover_task: Option<Task<()>>,
    over_popover: bool,
    /// Loaded-content hash of the file when blame was requested.
    content_hash: Option<String>,
}

#[derive(Clone)]
pub(super) struct BlamePopover {
    commit: Option<BlameCommit>,
    position: Point<Pixels>,
}

/// One visible gutter row. `labelled` is true for the first visible row of a
/// run, which carries the author/time text and chat chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GutterCell {
    pub(super) line: u32,
    pub(super) run: Option<usize>,
    pub(super) labelled: bool,
}

/// Index of the run containing 1-based `line`. O(log runs).
pub(super) fn run_index_for_line(runs: &[BlameRun], line: u32) -> Option<usize> {
    let after = runs.partition_point(|run| run.start <= line);
    let index = after.checked_sub(1)?;
    let run = &runs[index];
    (line < run.start + run.len).then_some(index)
}

/// Cells for the visible `lines`, in order. O(visible · log runs).
pub(super) fn gutter_cells(blame: &Blame, lines: impl IntoIterator<Item = u32>) -> Vec<GutterCell> {
    let mut previous: Option<usize> = None;
    lines
        .into_iter()
        .map(|line| {
            let run = run_index_for_line(&blame.runs, line);
            let labelled = run.is_some() && run != previous;
            previous = run;
            GutterCell { line, run, labelled }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunLabel {
    pub(super) text: String,
    pub(super) chip: Option<String>,
    pub(super) probably: bool,
}

pub(super) fn run_label(blame: &Blame, run: &BlameRun, now: DateTime<Utc>) -> RunLabel {
    let Some(commit) = run.commit.and_then(|index| blame.commits.get(index as usize)) else {
        return RunLabel { text: "Not committed".into(), chip: None, probably: false };
    };
    let ago = DateTime::from_timestamp(commit.author_time, 0)
        .map(|then| format_time_ago(then, now))
        .unwrap_or_default();
    let chip = commit.chat.as_ref().map(|chat| {
        let mut title: String = chat.title.chars().take(CHIP_MAX_CHARS).collect();
        if chat.title.chars().count() > CHIP_MAX_CHARS {
            title.push('…');
        }
        title
    });
    RunLabel {
        text: format!("{} · {ago}", commit.author),
        chip,
        probably: commit
            .chat
            .as_ref()
            .is_some_and(|chat| chat.confidence == BlameChatConfidence::Probably),
    }
}

impl FilesSurface {
    pub fn blame_gutter_enabled(&self) -> bool {
        self.blame.enabled
    }

    /// Preview fixture: the review file with the gutter on, mixed provenance
    /// and the hover popover open.
    #[cfg(feature = "snapshot")]
    pub(crate) fn fixture_file_blame(&mut self, cx: &mut Context<Self>) {
        use mast_proto::BlameChat;
        self.fixture_file_outline(cx);
        self.fixture_close_outline();
        let now = Utc::now().timestamp();
        let commit = |sha: &str, author: &str, days: i64, subject: &str, chat: Option<BlameChat>| BlameCommit {
            sha: sha.repeat(40)[..40].to_owned(),
            author: author.into(),
            author_time: now - days * 86_400,
            subject: subject.into(),
            path: "src/review.rs".into(),
            previous_path: None,
            chat,
        };
        let chat = |title: &str, confidence| BlameChat {
            chat_id: "fixture-chat".into(),
            title: title.into(),
            confidence,
            turn_id: Some("turn-3".into()),
        };
        let blame = Blame {
            revision: "fixture".into(),
            commits: vec![
                commit("a", "Mikey", 41, "Add the review model", None),
                commit("b", "Mikey", 3, "Track completion on Review", Some(chat("Review completion state", BlameChatConfidence::Recorded))),
                commit("c", "Mikey", 9, "Print the completion flag", Some(chat("Debug output cleanup", BlameChatConfidence::Probably))),
            ],
            runs: vec![
                BlameRun { start: 1, len: 3, commit: Some(0) },
                BlameRun { start: 4, len: 1, commit: None },
                BlameRun { start: 5, len: 5, commit: Some(1) },
                BlameRun { start: 10, len: 1, commit: Some(0) },
                BlameRun { start: 11, len: 4, commit: Some(2) },
            ],
        };
        self.blame.enabled = true;
        self.blame.popover = blame
            .commits
            .get(1)
            .cloned()
            .map(|commit| BlamePopover { commit: Some(commit), position: gpui::point(px(300.0), px(150.0)) });
        self.blame.blame = Some(Arc::new(blame));
        cx.notify();
    }

    /// Toolbar toggle: applies here and tells the shell to apply it to every
    /// open file surface.
    pub fn toggle_blame_gutter(&mut self, cx: &mut Context<Self>) {
        let enabled = !self.blame.enabled;
        self.set_blame_gutter(enabled, cx);
        cx.emit(FilesEvent::BlameGutterChanged(enabled));
    }

    /// Idempotent. Turning it on requests blame for the active file only.
    pub fn set_blame_gutter(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.blame.enabled == enabled {
            return;
        }
        if enabled {
            self.blame.enabled = true;
            self.refresh_blame(cx);
        } else {
            self.blame = BlameState::default();
        }
        cx.notify();
    }

    /// Drop cached blame (file switched, saved or reloaded) and refetch when
    /// the gutter is on.
    pub(super) fn invalidate_blame(&mut self, cx: &mut Context<Self>) {
        if !self.blame.enabled {
            return;
        }
        self.blame.blame = None;
        self.blame.path = None;
        self.blame.popover = None;
        self.refresh_blame(cx);
    }

    fn refresh_blame(&mut self, cx: &mut Context<Self>) {
        if !self.blame.enabled {
            return;
        }
        let Some(path) = self.preview.active.clone() else { return };
        if self.blame.path.as_deref() == Some(&path)
            && (self.blame.blame.is_some() || self.blame.task.is_some())
        {
            return;
        }
        let Some(context) = self.request_context.clone() else { return };
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        self.blame.generation = self.blame.generation.wrapping_add(1);
        let generation = self.blame.generation;
        self.blame.path = Some(path.clone());
        self.blame.blame = None;
        self.blame.content_hash = self
            .preview
            .documents
            .get(&path)
            .and_then(|document| document.content_hash().map(str::to_owned));
        self.blame.status = Some("Loading blame…".into());
        let client = WorkspaceFilesClient::new(engine, context.clone());
        self.blame.task = Some(cx.spawn(async move |this, cx| {
            let result = client.file_blame(path.clone()).await;
            let _ = this.update(cx, |this, cx| {
                if this.request_context.as_ref() != Some(&context)
                    || this.blame.generation != generation
                {
                    return;
                }
                this.blame.task = None;
                match result {
                    Ok(blame) => {
                        this.blame.blame = Some(Arc::new(blame));
                        this.blame.status = None;
                    }
                    Err(error) => this.blame.status = Some(error.to_string().into()),
                }
                cx.notify();
            });
        }));
    }

    /// Called when the active file changes to `path`.
    pub(super) fn blame_on_open(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.blame.enabled && self.blame.path.as_deref() != Some(path) {
            self.blame.path = None;
            self.blame.blame = None;
            self.blame.popover = None;
            self.refresh_blame(cx);
        }
    }

    /// A save or external reload changed the file's content: refetch.
    pub(super) fn blame_check_stale(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.blame.enabled || self.blame.path.as_deref() != Some(path) {
            return;
        }
        let hash = self
            .preview
            .documents
            .get(path)
            .and_then(|document| document.content_hash().map(str::to_owned));
        match (&self.blame.content_hash, &hash) {
            (None, Some(_)) => self.blame.content_hash = hash,
            (Some(old), Some(new)) if old != new => self.invalidate_blame(cx),
            _ => {}
        }
    }

    pub(super) fn open_blame_commit(&mut self, sha: String, cx: &mut Context<Self>) {
        self.blame.popover = None;
        self.open_file_history_at(sha, cx);
    }

    fn open_blame_chat(&mut self, commit: &BlameCommit, cx: &mut Context<Self>) {
        let Some(chat) = commit.chat.as_ref() else { return };
        self.blame.popover = None;
        cx.emit(FilesEvent::OpenChat {
            chat_id: chat.chat_id.clone(),
            turn_id: chat.turn_id.clone(),
        });
        cx.notify();
    }

    fn hover_blame_run(&mut self, run: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(blame) = self.blame.blame.clone() else { return };
        let Some(run) = blame.runs.get(run) else { return };
        let commit = run.commit.and_then(|i| blame.commits.get(i as usize)).cloned();
        self.blame.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_OPEN).await;
            let _ = this.update(cx, |this, cx| {
                this.blame.popover = Some(BlamePopover { commit, position });
                cx.notify();
            });
        }));
    }

    fn leave_blame_hover(&mut self, cx: &mut Context<Self>) {
        self.blame.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_CLOSE).await;
            let _ = this.update(cx, |this, cx| {
                if !this.blame.over_popover && this.blame.popover.take().is_some() {
                    cx.notify();
                }
            });
        }));
    }

    /// The blame column for the editor path, or `None` when the gutter is off.
    pub(super) fn render_blame_column(
        &mut self,
        editor: &Entity<super::editor::FileEditorState>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.blame.enabled {
            return None;
        }
        let column = div()
            .id("files-blame-column")
            .flex_none()
            .w(px(BLAME_COLUMN_WIDTH))
            .h_full()
            .relative()
            .overflow_hidden()
            .font_family(theme.font_sans.clone())
            .text_size(px(11.0))
            .text_color(theme.text_faint);
        let Some(blame) = self.blame.blame.clone() else {
            let status = self.blame.status.clone().unwrap_or_default();
            return Some(column.px(px(8.0)).py(px(6.0)).child(status).into_any_element());
        };
        let Some(layout) = editor.read_with(cx, |state, _| super::preview::editor_overlay_layout(state)) else {
            return Some(column.into_any_element());
        };
        let cells = gutter_cells(&blame, layout.rows.iter().map(|row| row.line));
        let now = Utc::now();
        let mut children = Vec::with_capacity(cells.len());
        for (cell, row) in cells.iter().zip(&layout.rows) {
            let Some(run_index) = cell.run else { continue };
            let run = &blame.runs[run_index];
            let label = cell.labelled.then(|| run_label(&blame, run, now));
            let uncommitted = run.commit.is_none();
            let element = div()
                .id(("files-blame-cell", cell.line as usize))
                .absolute()
                .left(px(0.0))
                .top(px(row.top))
                .w_full()
                .h(px(layout.line_height))
                .px(px(8.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .whitespace_nowrap()
                .overflow_hidden()
                .cursor_pointer()
                .hover(|el| el.bg(theme.surface_raised))
                .when(uncommitted, |el| el.italic())
                .when_some(label, |el, label| {
                    el.child(div().truncate().child(label.text)).when_some(label.chip, |el, chip| {
                        el.child(
                            div()
                                .flex_none()
                                .px(px(5.0))
                                .rounded(px(4.0))
                                .bg(theme.accent_wash)
                                .text_color(if label.probably { theme.text_muted } else { theme.accent })
                                .child(if label.probably { format!("~ {chip}") } else { chip }),
                        )
                    })
                })
                .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                    if *hovered {
                        let position = window.mouse_position();
                        this.hover_blame_run(run_index, Point { x: px(f32::from(position.x) + 14.0), y: position.y }, cx);
                    } else {
                        this.leave_blame_hover(cx);
                    }
                }))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                    let sha = this
                        .blame
                        .blame
                        .as_ref()
                        .and_then(|b| b.runs.get(run_index).and_then(|r| r.commit.and_then(|i| b.commits.get(i as usize))))
                        .map(|commit| commit.sha.clone());
                    if let Some(sha) = sha {
                        this.open_blame_commit(sha, cx);
                    }
                }));
            children.push(element.into_any_element());
        }
        Some(column.children(children).into_any_element())
    }

    /// The hover popover: commit facts, and the chat when known.
    pub(super) fn render_blame_popover(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let popover = self.blame.popover.clone()?;
        let theme = &theme.for_popup();
        let mut card = crate::popover::popover_card(theme)
            .id("files-blame-popover")
            .w(px(300.0))
            .flex()
            .flex_col()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.blame.over_popover = *hovered;
                if !*hovered {
                    this.leave_blame_hover(cx);
                }
            }));
        match popover.commit {
            None => {
                card = card.child("Not committed");
            }
            Some(commit) => {
                let short = commit.sha.get(..8).unwrap_or(&commit.sha).to_owned();
                let when = DateTime::from_timestamp(commit.author_time, 0)
                    .map(|t| t.format("%b %-d, %Y").to_string())
                    .unwrap_or_default();
                let open_sha = commit.sha.clone();
                card = card
                    .child(
                        div()
                            .id("files-blame-popover-commit")
                            .debug_selector(|| "files-blame-popover-commit".into())
                            .cursor_pointer()
                            .hover(|el| el.bg(theme.surface_raised))
                            .on_click(cx.listener(move |this, _, _, cx| this.open_blame_commit(open_sha.clone(), cx)))
                            .child(div().text_color(theme.text).child(commit.subject.clone()))
                            .child(
                                div()
                                    .text_color(theme.text_muted)
                                    .child(format!("{short} · {} · {when}", commit.author)),
                            ),
                    );
                if let Some(chat) = commit.chat.clone() {
                    let probably = chat.confidence == BlameChatConfidence::Probably;
                    let caption = if probably {
                        format!("probably from {}", chat.title)
                    } else {
                        chat.title.clone()
                    };
                    let target = commit.clone();
                    card = card.child(
                        div()
                            .id("files-blame-popover-chat")
                            .debug_selector(|| "files-blame-popover-chat".into())
                            .mt(px(6.0))
                            .cursor_pointer()
                            .hover(|el| el.bg(theme.surface_raised))
                            .on_click(cx.listener(move |this, _, _, cx| this.open_blame_chat(&target, cx)))
                            .child(div().text_color(theme.accent).child(caption))
                            .when(chat.turn_id.is_some(), |el| {
                                el.child(div().text_color(theme.text_muted).child("Open at the turn that edited this file"))
                            }),
                    );
                }
            }
        }
        Some(crate::popover::menu_at("files-blame-popover-menu", popover.position, card.into_any_element(), None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext};
    use mast_proto::{BlameChat, BlameCommit};
    use std::{cell::RefCell, rc::Rc};

    fn commit(sha: &str, author: &str, chat: Option<BlameChat>) -> BlameCommit {
        BlameCommit {
            sha: sha.into(),
            author: author.into(),
            author_time: 1_700_000_000,
            subject: "subject".into(),
            path: "a.rs".into(),
            previous_path: None,
            chat,
        }
    }

    fn blame() -> Blame {
        Blame {
            revision: "r".into(),
            commits: vec![
                commit("a", "Ann", None),
                commit(
                    "b",
                    "Bob",
                    Some(BlameChat {
                        chat_id: "c1".into(),
                        title: "A very long chat title here".into(),
                        confidence: BlameChatConfidence::Recorded,
                        turn_id: Some("u1".into()),
                    }),
                ),
            ],
            runs: vec![
                BlameRun { start: 1, len: 3, commit: Some(0) },
                BlameRun { start: 4, len: 2, commit: Some(1) },
                BlameRun { start: 6, len: 1, commit: None },
            ],
        }
    }

    #[test]
    fn run_lookup_covers_every_line_and_rejects_gaps() {
        let blame = blame();
        assert_eq!(run_index_for_line(&blame.runs, 1), Some(0));
        assert_eq!(run_index_for_line(&blame.runs, 3), Some(0));
        assert_eq!(run_index_for_line(&blame.runs, 4), Some(1));
        assert_eq!(run_index_for_line(&blame.runs, 6), Some(2));
        assert_eq!(run_index_for_line(&blame.runs, 7), None);
        assert_eq!(run_index_for_line(&blame.runs, 0), None);
    }

    #[test]
    fn labels_mark_the_first_visible_row_of_each_run() {
        let blame = blame();
        let cells = gutter_cells(&blame, [2, 3, 4, 5, 6]);
        let labelled: Vec<u32> = cells.iter().filter(|c| c.labelled).map(|c| c.line).collect();
        assert_eq!(labelled, vec![2, 4, 6]);
    }

    #[test]
    fn labels_show_author_time_chip_and_uncommitted() {
        let blame = blame();
        let now = DateTime::from_timestamp(1_700_000_000 + 3 * 86_400, 0).unwrap();
        let first = run_label(&blame, &blame.runs[0], now);
        assert_eq!(first.text, "Ann · 3d");
        assert_eq!(first.chip, None);
        let second = run_label(&blame, &blame.runs[1], now);
        assert_eq!(second.chip.as_deref(), Some("A very long ch…"));
        assert!(!second.probably);
        assert_eq!(run_label(&blame, &blame.runs[2], now).text, "Not committed");
    }

    #[test]
    fn cache_hit_cost_is_visible_rows_not_file_size() {
        // 200k lines in 100k runs; a 60-row viewport builds exactly 60 cells.
        let runs: Vec<BlameRun> = (0..100_000u32)
            .map(|i| BlameRun { start: 1 + i * 2, len: 2, commit: Some(0) })
            .collect();
        let blame = Blame { revision: "r".into(), commits: vec![commit("a", "Ann", None)], runs };
        let started = std::time::Instant::now();
        for _ in 0..1_000 {
            let cells = gutter_cells(&blame, 150_000..150_060);
            assert_eq!(cells.len(), 60);
        }
        assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
    }

    fn surface(cx: &mut TestAppContext) -> (Entity<FilesSurface>, &mut gpui::VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        cx.add_window_view(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new(state, "chat".into(), false, 1000, 13.0, false, false, cx)
        })
    }

    #[gpui::test]
    fn toggle_flips_the_gutter_and_announces_it(cx: &mut TestAppContext) {
        let (files, cx) = surface(cx);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let _sub = cx.update(|_, cx| {
            cx.subscribe(&files, move |_, event, _| {
                if let FilesEvent::BlameGutterChanged(on) = event {
                    log.borrow_mut().push(*on);
                }
            })
        });
        files.read_with(cx, |files, _| assert!(!files.blame_gutter_enabled(), "off by default"));
        files.update(cx, |files, cx| files.toggle_blame_gutter(cx));
        files.update(cx, |files, cx| files.toggle_blame_gutter(cx));
        assert_eq!(*seen.borrow(), vec![true, false]);
        files.read_with(cx, |files, _| assert!(!files.blame_gutter_enabled()));
    }

    #[gpui::test]
    fn popover_opens_the_chat_at_its_turn(cx: &mut TestAppContext) {
        let (files, cx) = surface(cx);
        let opened = Rc::new(RefCell::new(Vec::new()));
        let log = opened.clone();
        let _sub = cx.update(|_, cx| {
            cx.subscribe(&files, move |_, event, _| {
                if let FilesEvent::OpenChat { chat_id, turn_id } = event {
                    log.borrow_mut().push((chat_id.clone(), turn_id.clone()));
                }
            })
        });
        files.update(cx, |files, _| {
            files.blame.enabled = true;
            files.blame.popover = Some(BlamePopover {
                commit: blame().commits.get(1).cloned(),
                position: gpui::point(px(40.0), px(40.0)),
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        let chat = cx.debug_bounds("files-blame-popover-chat").expect("chat row shown");
        cx.simulate_click(chat.center(), Modifiers::default());
        assert_eq!(*opened.borrow(), vec![("c1".to_string(), Some("u1".to_string()))]);
        files.read_with(cx, |files, _| assert!(files.blame.popover.is_none(), "closes after the jump"));
    }

    #[gpui::test]
    fn popover_without_a_chat_offers_only_the_commit(cx: &mut TestAppContext) {
        let (files, cx) = surface(cx);
        files.update(cx, |files, _| {
            files.blame.popover = Some(BlamePopover {
                commit: blame().commits.first().cloned(),
                position: gpui::point(px(40.0), px(40.0)),
            });
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(cx.debug_bounds("files-blame-popover-commit").is_some());
        assert!(cx.debug_bounds("files-blame-popover-chat").is_none());
    }
}
