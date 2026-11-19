//! History and per-file commit diffs for the active document.

use super::{FilesSurface, client::WorkspaceFilesClient};
use gpui::{
    AnyElement, Context, ListAlignment, ListSizingBehavior, ListState, Task, Window, div, list,
    prelude::*, px,
};
use mast_engine::workspace_files::file_history::{FileHistory, FileHistoryEntry};

static NEXT_HISTORY_VIEW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub(super) struct FileHistoryView {
    path: String,
    id: u64,
    commits: Vec<FileHistoryEntry>,
    list: ListState,
    diff_lines: Vec<String>,
    diff_list: ListState,
    selected: Option<usize>,
    status: String,
    generation: u64,
    task: Option<Task<()>>,
    /// Commit to select once the history loads (blame click).
    pending_commit: Option<String>,
}

impl FilesSurface {
    pub(super) fn toggle_file_history(&mut self, cx: &mut Context<Self>) {
        if self.file_history.take().is_some() {
            cx.notify();
            return;
        }
        let Some(path) = self.preview.active.clone() else {
            return;
        };
        let Some(context) = self.request_context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let client = WorkspaceFilesClient::new(engine, context.clone());
        let request_path = path.clone();
        let id = NEXT_HISTORY_VIEW.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let task = cx.spawn(async move |this, cx| {
            let result = client.file_history(request_path.clone()).await;
            let _ = this.update(cx, |this, cx| {
                if this.request_context.as_ref() != Some(&context) {
                    return;
                }
                let Some(view) = this
                    .file_history
                    .as_mut()
                    .filter(|view| view.id == id && view.path == request_path)
                else {
                    return;
                };
                view.task = None;
                match result {
                    Ok(FileHistory {
                        commits, truncated, ..
                    }) => {
                        view.commits = commits;
                        view.list
                            .reset_with_uniform_height(view.commits.len(), px(32.0));
                        view.status = if view.commits.is_empty() {
                            "No commits for this file".into()
                        } else if truncated {
                            "Showing the latest 100 commits".into()
                        } else {
                            String::new()
                        };
                    }
                    Err(error) => view.status = error.to_string(),
                }
                let pending = view.pending_commit.take().and_then(|sha| {
                    view.commits.iter().position(|entry| entry.commit.sha == sha)
                });
                if let Some(index) = pending {
                    this.open_file_history_diff(index, cx);
                }
                cx.notify();
            });
        });
        self.file_history = Some(FileHistoryView {
            path,
            id,
            commits: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(180.0)),
            diff_lines: Vec::new(),
            diff_list: ListState::new(0, ListAlignment::Top, px(240.0)),
            selected: None,
            status: "Loading history…".into(),
            generation: 0,
            task: Some(task),
            pending_commit: None,
        });
        cx.notify();
    }

    /// Open the History tab with `sha` selected (loads the history first when
    /// it is not open yet).
    pub(super) fn open_file_history_at(&mut self, sha: String, cx: &mut Context<Self>) {
        if self.file_history.is_none() {
            self.toggle_file_history(cx);
        }
        let Some(view) = self.file_history.as_mut() else { return };
        match view.commits.iter().position(|entry| entry.commit.sha == sha) {
            Some(index) => self.open_file_history_diff(index, cx),
            None => view.pending_commit = Some(sha),
        }
    }

    fn open_file_history_diff(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(view) = self.file_history.as_mut() else {
            return;
        };
        let Some(entry) = view.commits.get(index).cloned() else {
            return;
        };
        let Some(context) = self.request_context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        view.generation = view.generation.wrapping_add(1);
        let generation = view.generation;
        let id = view.id;
        view.selected = Some(index);
        view.diff_lines.clear();
        view.diff_list.reset(0);
        view.status = "Loading diff…".into();
        let client = WorkspaceFilesClient::new(engine, context.clone());
        view.task = Some(cx.spawn(async move |this, cx| {
            let result = client.file_history_diff(&entry).await;
            let result = match result {
                Ok(diff) => Ok(cx
                    .background_executor()
                    .spawn(async move {
                        let mut long_lines = false;
                        let mut lines: Vec<String> = diff.lines().take(10_001).map(|line| {
                            let mut chars = line.chars();
                            let mut visible: String = chars.by_ref().take(4_096).collect();
                            if chars.next().is_some() { visible.push('…'); long_lines = true; }
                            visible
                        }).collect();
                        let truncated = lines.len() > 10_000;
                        lines.truncate(10_000);
                        (lines, truncated, long_lines)
                    })
                    .await),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update(cx, |this, cx| {
                if this.request_context.as_ref() != Some(&context) {
                    return;
                }
                let Some(view) = this
                    .file_history
                    .as_mut()
                    .filter(|view| view.id == id && view.generation == generation)
                else {
                    return;
                };
                view.task = None;
                match result {
                    Ok((lines, truncated, long_lines)) => {
                        view.diff_lines = lines;
                        view.diff_list
                            .reset_with_uniform_height(view.diff_lines.len(), px(18.0));
                        view.status = if truncated {
                            "Showing the first 10,000 diff lines".into()
                        } else if long_lines {
                            "Long diff lines are shortened".into()
                        } else if view.diff_lines.is_empty() {
                            "No diff for this file".into()
                        } else {
                            String::new()
                        };
                    }
                    Err(error) => view.status = error,
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_history_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(view) = self.file_history.as_ref() else {
            return gpui::Empty.into_any_element();
        };
        let Some(entry) = view.commits.get(index) else {
            return gpui::Empty.into_any_element();
        };
        let short = entry.commit.sha.get(..8).unwrap_or(&entry.commit.sha);
        let label = format!(
            "{short}  {} · {}",
            entry.commit.subject, entry.commit.author_name
        );
        let selected = view.selected == Some(index);
        let theme = crate::theme::Theme::of(cx).clone();
        div()
            .id(("file-history-commit", index))
            .h(px(32.0))
            .px(px(10.0))
            .w_full()
            .flex()
            .items_center()
            .cursor_pointer()
            .text_color(theme.text)
            .when(selected, |el| el.bg(theme.surface_raised))
            .hover(|el| el.bg(theme.surface_raised))
            .child(div().truncate().child(label))
            .on_click(cx.listener(move |this, _, _, cx| this.open_file_history_diff(index, cx)))
            .into_any_element()
    }

    fn render_history_diff_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(line) = self
            .file_history
            .as_ref()
            .and_then(|view| view.diff_lines.get(index))
        else {
            return gpui::Empty.into_any_element();
        };
        let theme = crate::theme::Theme::of(cx);
        div()
            .h(px(18.0))
            .w_full()
            .overflow_hidden()
            .px(px(10.0))
            .font_family(theme.font_mono.clone())
            .text_color(theme.text)
            .whitespace_nowrap()
            .child(line.clone())
            .into_any_element()
    }

    pub(super) fn render_file_history(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let view = self.file_history.as_ref()?;
        let theme = crate::theme::Theme::of(cx).clone();
        let commits = list(view.list.clone(), cx.processor(Self::render_history_row))
            .h(px((view.commits.len() as f32 * 32.0).min(160.0)))
            .with_sizing_behavior(ListSizingBehavior::Auto);
        let diff = (!view.diff_lines.is_empty()).then(|| {
            list(
                view.diff_list.clone(),
                cx.processor(Self::render_history_diff_row),
            )
            .h(px(200.0))
            .with_sizing_behavior(ListSizingBehavior::Auto)
        });
        let selected = view.selected.and_then(|index| view.commits.get(index)).map(|entry| entry.commit.clone());
        Some(
            div()
                .w_full()
                .flex_none()
                .flex()
                .flex_col()
                .border_b_1()
                .border_color(theme.border)
                .child(commits)
                .children(diff)
                .when_some(selected, |element, commit| element.child(super::toolbar_button("file-history-open-commit", "Open commit diff", &theme).on_click(cx.listener(move |_, _, _, cx| cx.emit(super::FilesEvent::OpenCommit(commit.clone())))).child("Open commit")))
                .child(
                    div()
                        .px(px(10.0))
                        .text_color(theme.text_muted)
                        .child(view.status.clone()),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn blame_click_before_history_loads_selects_the_commit_afterwards(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::default());
        });
        let (files, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new(state, "chat".into(), false, 1000, 13.0, false, false, cx)
        });
        files.update(cx, |files, cx| {
            files.file_history = Some(FileHistoryView {
                path: "a.rs".into(),
                id: 1,
                commits: Vec::new(),
                list: ListState::new(0, ListAlignment::Top, px(180.0)),
                diff_lines: Vec::new(),
                diff_list: ListState::new(0, ListAlignment::Top, px(240.0)),
                selected: None,
                status: "Loading history…".into(),
                generation: 0,
                task: None,
                pending_commit: None,
            });
            files.open_blame_commit("abc".into(), cx);
            assert_eq!(
                files.file_history.as_ref().unwrap().pending_commit.as_deref(),
                Some("abc")
            );
        });
    }
}
