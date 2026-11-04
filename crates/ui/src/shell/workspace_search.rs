use super::*;
use crate::files::client::{FilesRequestContext, WorkspaceFilesClient};
use mast_proto::{
    SearchWorkspaceContentRequest, SearchWorkspaceFilesRequest, WorkspaceContentSearchFrame,
    WorkspaceEntryKind,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchMode {
    Files,
    Content,
    Line,
}
#[derive(Clone)]
struct ResultRow {
    path: String,
    line: Option<u32>,
    preview: String,
    ranges: Vec<(usize, usize)>,
    changed: bool,
}
pub(super) struct WorkspaceSearch {
    mode: SearchMode,
    input: Entity<ComposerInput>,
    include: Entity<ComposerInput>,
    exclude: Entity<ComposerInput>,
    previous_focus: Option<FocusHandle>,
    focus_pending: bool,
    active: usize,
    scroll: gpui::ScrollHandle,
    rows: Vec<ResultRow>,
    changed: Vec<String>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    changed_only: bool,
    status: String,
    generation: u64,
    task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub(super) fn open_workspace_search(
        &mut self,
        mode: SearchMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_chat.is_empty() {
            cx.propagate();
            return;
        }
        if mode == SearchMode::Line {
            let focused = match self.resolved_right_active(cx) {
                RightSurface::File(id) => self
                    .file_surfaces
                    .get(&id)
                    .is_some_and(|file| file.read(cx).editor_is_focused(window, cx)),
                RightSurface::Diff(id) => self
                    .diffs
                    .get(&id)
                    .is_some_and(|diff| diff.read(cx).diff_is_focused(window, cx)),
                _ => false,
            };
            if !focused {
                cx.propagate();
                return;
            }
        }
        let input = cx.new(|cx| {
            ComposerInput::with_context(
                match mode {
                    SearchMode::Files => "Search files…",
                    SearchMode::Content => "Search in files…",
                    SearchMode::Line => "Line number…",
                },
                "WorkspaceSearch",
                cx,
            )
        });
        let include =
            cx.new(|cx| ComposerInput::with_context("Files to include", "WorkspaceSearch", cx));
        let exclude =
            cx.new(|cx| ComposerInput::with_context("Files to exclude", "WorkspaceSearch", cx));
        let mut subscriptions = Vec::new();
        for entity in [&input, &include, &exclude] {
            subscriptions.push(cx.subscribe(entity, |this, _, event, cx| {
                if matches!(event, ComposerInputEvent::Edited) {
                    this.update_workspace_search(cx);
                }
            }));
        }
        let changed = self
            .right_tabs
            .get(&self.panel_key(cx))
            .and_then(|tabs| {
                tabs.iter().find_map(|tab| match tab {
                    RightSurface::Diff(id) => self.diffs.get(id),
                    _ => None,
                })
            })
            .map(|diff| diff.read(cx).changed_paths())
            .unwrap_or_default();
        self.workspace_search = Some(WorkspaceSearch {
            mode,
            input,
            include,
            exclude,
            previous_focus: window.focused(cx),
            focus_pending: true,
            active: 0,
            scroll: gpui::ScrollHandle::new(),
            rows: Vec::new(),
            changed,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            changed_only: false,
            status: String::new(),
            generation: 0,
            task: None,
            _subscriptions: subscriptions,
        });
        self.update_workspace_search(cx);
        cx.notify();
    }

    pub(super) fn close_workspace_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = self.workspace_search.take() {
            if let Some(focus) = search.previous_focus {
                window.focus(&focus, cx);
            }
            cx.notify();
        }
    }

    fn update_workspace_search(&mut self, cx: &mut Context<Self>) {
        let Some(search) = self.workspace_search.as_mut() else {
            return;
        };
        search.task = None;
        search.generation = search.generation.wrapping_add(1);
        let generation = search.generation;
        search.active = 0;
        search.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        search.rows.clear();
        search.status.clear();
        let mode = search.mode;
        let raw = search.input.read(cx).text().trim().to_owned();
        if mode == SearchMode::Line {
            cx.notify();
            return;
        }
        let query = if mode == SearchMode::Files {
            parse_line_suffix(&raw).0
        } else {
            raw.as_str()
        };
        let recents = self
            .workspace_file_recents
            .get(&self.active_chat)
            .cloned()
            .unwrap_or_default();
        if query.is_empty() {
            if mode == SearchMode::Files {
                let mut paths = search.changed.clone();
                for path in recents {
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                }
                search.rows = paths
                    .into_iter()
                    .map(|path| ResultRow {
                        changed: search.changed.contains(&path),
                        path,
                        line: None,
                        preview: String::new(),
                        ranges: Vec::new(),
                    })
                    .collect();
            }
            cx.notify();
            return;
        }
        let Some(context) = FilesRequestContext::for_chat(self.state.read(cx), &self.active_chat)
        else {
            search.status = "No workspace available".into();
            cx.notify();
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            search.status = "Workspace service unavailable".into();
            cx.notify();
            return;
        };
        let client = WorkspaceFilesClient::new(engine, context.clone());
        let changed = search.changed.clone();
        let include = split_globs(search.include.read(cx).text());
        let exclude = split_globs(search.exclude.read(cx).text());
        let request = SearchWorkspaceContentRequest {
            target: context.target.clone(),
            query: query.to_owned(),
            case_sensitive: search.case_sensitive,
            whole_word: search.whole_word,
            regex: search.regex,
            include,
            exclude,
            changed_files: search.changed_only.then_some(changed.clone()),
        };
        let file_request = SearchWorkspaceFilesRequest {
            target: context.target.clone(),
            query: query.to_owned(),
            include_ignored: false,
            limit: Some(200),
        };
        search.status = "Searching…".into();
        search.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            if mode == SearchMode::Files {
                let result = client.search(file_request).await;
                let _ = this.update(cx, |this, cx| {
                    let Some(search) = this
                        .workspace_search
                        .as_mut()
                        .filter(|s| s.generation == generation)
                    else {
                        return;
                    };
                    match result {
                        Ok(matches) => {
                            search.rows = matches
                                .into_iter()
                                .filter(|m| m.kind == WorkspaceEntryKind::File)
                                .map(|m| ResultRow {
                                    changed: changed.contains(&m.path),
                                    path: m.path,
                                    line: None,
                                    preview: String::new(),
                                    ranges: Vec::new(),
                                })
                                .collect();
                            search.rows.sort_by_key(|row| {
                                (
                                    !row.changed,
                                    recents
                                        .iter()
                                        .position(|path| path == &row.path)
                                        .unwrap_or(usize::MAX),
                                )
                            });
                            search.status.clear();
                        }
                        Err(error) => search.status = error.to_string(),
                    }
                    cx.notify();
                });
            } else {
                match client.search_content(request).await {
                    Ok(mut stream) => {
                        while let Some(value) = stream.recv().await {
                            let mut frames = vec![value];
                            while frames.len() < 64 {
                                match stream.try_recv() { Ok(value) => frames.push(value), Err(_) => break }
                            }
                            let result = this.update(cx, |this, cx| {
                                let Some(search) = this
                                    .workspace_search
                                    .as_mut()
                                    .filter(|s| s.generation == generation)
                                else {
                                    return false;
                                };
                                for frame in frames.into_iter().map(serde_json::from_value::<WorkspaceContentSearchFrame>) { match frame {
                                    Ok(WorkspaceContentSearchFrame::Match {
                                        path,
                                        line,
                                        preview,
                                        ranges,
                                    }) => search.rows.push(ResultRow {
                                        changed: changed.contains(&path),
                                        path,
                                        line: u32::try_from(line).ok(),
                                        preview,
                                        ranges,
                                    }),
                                    Ok(WorkspaceContentSearchFrame::Complete {
                                        matches,
                                        files,
                                        truncated,
                                    }) => {
                                        search.status = if truncated {
                                            format!(
                                                "Showing first {matches} matches in {files} files"
                                            )
                                        } else {
                                            format!("{matches} matches in {files} files")
                                        }
                                    }
                                    Ok(WorkspaceContentSearchFrame::Error { message }) => {
                                        search.status = message
                                    }
                                    Err(error) => search.status = error.to_string(),
                                }}
                                cx.notify();
                                true
                            });
                            if !result.unwrap_or(false) {
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        let _ = this.update(cx, |this, cx| {
                            if let Some(search) = this
                                .workspace_search
                                .as_mut()
                                .filter(|s| s.generation == generation)
                            {
                                search.status = error.to_string();
                                cx.notify();
                            }
                        });
                    }
                }
            }
        }));
        cx.notify();
    }

    fn select_workspace_result(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = self.workspace_search.as_ref() else {
            return;
        };
        if search.mode == SearchMode::Line {
            let Ok(line) = search.input.read(cx).text().trim().parse::<u32>() else {
                return;
            };
            let surface = self.resolved_right_active(cx);
            self.close_workspace_search(window, cx);
            match surface {
                RightSurface::File(id) => {
                    if let Some(file) = self.file_surfaces.get(&id) {
                        file.update(cx, |file, cx| file.reveal_editor_location(line, None, cx));
                    }
                }
                RightSurface::Diff(id) => {
                    if let Some(diff) = self.diffs.get(&id) {
                        diff.update(cx, |diff, cx| diff.go_to_line(line, cx));
                    }
                }
                _ => (),
            }
            return;
        }
        let Some(row) = search.rows.get(index).cloned() else {
            return;
        };
        let line = row
            .line
            .or_else(|| parse_line_suffix(search.input.read(cx).text()).1);
        self.close_workspace_search(window, cx);
        self.add_file_surface_at(row.path, line, None, window, cx);
    }

    pub(super) fn render_workspace_search(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let search = self.workspace_search.as_mut()?;
        if std::mem::take(&mut search.focus_pending) {
            window.focus(&search.input.focus_handle(cx), cx);
        }
        let mode = search.mode;
        let rows = search.rows.clone();
        let active = search.active;
        let scroll = search.scroll.clone();
        let theme = Theme::of(cx).for_popup();
        let mut card = div()
            .id("workspace-search")
            .w(px(640.0_f32.min(f32::from(viewport.width) - 32.0)))
            .max_h(px(600.0_f32.min(f32::from(viewport.height) - 32.0)))
            .flex()
            .flex_col()
            .p(px(16.0))
            .gap(px(8.0))
            .rounded(px(12.0))
            .bg(popover::surface_bg(&theme))
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => this.close_workspace_search(window, cx),
                    "enter" => {
                        let index = this.workspace_search.as_ref().map_or(0, |s| s.active);
                        this.select_workspace_result(index, window, cx);
                    }
                    "up" | "down" => {
                        if let Some(search) = this.workspace_search.as_mut() {
                            let n = search.rows.len();
                            if n > 0 {
                                search.active = if event.keystroke.key == "down" {
                                    (search.active + 1) % n
                                } else {
                                    (search.active + n - 1) % n
                                };
                                search.scroll.scroll_to_item(search.active);
                                cx.notify();
                            }
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.close_workspace_search(window, cx)),
            )
            .child(search.input.clone());
        if mode == SearchMode::Content {
            let toggles = [
                ("Case", search.case_sensitive, 0usize),
                ("Word", search.whole_word, 1),
                ("Regex", search.regex, 2),
                ("Only changed files", search.changed_only, 3),
            ];
            let mut buttons = div().flex().gap(px(8.0));
            for (label, selected, id) in toggles {
                if id == 3 && search.changed.is_empty() {
                    continue;
                }
                buttons = buttons.child(
                    div()
                        .id(("search-option", id))
                        .cursor_pointer()
                        .px(px(6.0))
                        .rounded(px(4.0))
                        .when(selected, |el| el.bg(theme.surface_raised))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(search) = this.workspace_search.as_mut() {
                                match id {
                                    0 => search.case_sensitive = !search.case_sensitive,
                                    1 => search.whole_word = !search.whole_word,
                                    2 => search.regex = !search.regex,
                                    _ => search.changed_only = !search.changed_only,
                                };
                            }
                            this.update_workspace_search(cx);
                        })),
                );
            }
            card = card
                .child(buttons)
                .child(search.include.clone())
                .child(search.exclude.clone());
        }
        let mut results = div()
            .id("workspace-search-results")
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col();
        let mut previous = String::new();
        let mut changed_header = false;
        let mut files_header = false;
        for (index, row) in rows.iter().enumerate() {
            if mode == SearchMode::Files && row.changed && !changed_header {
                changed_header = true;
                results = results.child(
                    div()
                        .pt(px(8.0))
                        .text_color(theme.text_muted)
                        .child("Changed in this diff"),
                );
            }
            if mode == SearchMode::Files && !row.changed && !files_header {
                files_header = true;
                results = results.child(
                    div()
                        .pt(px(8.0))
                        .text_color(theme.text_muted)
                        .child("Files"),
                );
            }
            if mode == SearchMode::Content && previous != row.path {
                previous = row.path.clone();
                results = results.child(
                    div()
                        .pt(px(8.0))
                        .text_color(theme.text_muted)
                        .child(row.path.clone()),
                );
            }
            let label = if let Some(line) = row.line {
                let prefix = format!("{line}: ");
                let offset = prefix.len();
                gpui::StyledText::new(format!("{prefix}{}", row.preview))
                    .with_highlights(row.ranges.iter().map(|&(start, end)| {
                        (
                            offset + start..offset + end,
                            gpui::HighlightStyle {
                                color: Some(theme.code_text),
                                background_color: Some(theme.code_wash),
                                ..Default::default()
                            },
                        )
                    }))
                    .into_any_element()
            } else {
                row.path.clone().into_any_element()
            };
            results = results.child(
                div()
                    .id(("workspace-search-result", index))
                    .px(px(8.0))
                    .py(px(4.0))
                    .cursor_pointer()
                    .when(index == active, |el| el.bg(theme.surface_raised))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_workspace_result(index, window, cx)
                    })),
            );
        }
        card = card.child(results).child(
            div()
                .text_color(theme.text_muted)
                .child(search.status.clone()),
        );
        Some(popover::modal(
            "workspace-search-overlay",
            viewport,
            card.into_any_element(),
        ))
    }
}
fn parse_line_suffix(query: &str) -> (&str, Option<u32>) {
    match query.rsplit_once(':') {
        Some((path, line)) if !path.is_empty() => match line.parse::<u32>() {
            Ok(line) if line > 0 => (path, Some(line)),
            _ => (query, None),
        },
        _ => (query, None),
    }
}
fn split_globs(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn line_suffix_and_globs() {
        assert_eq!(
            parse_line_suffix("src/main.rs:42"),
            ("src/main.rs", Some(42))
        );
        assert_eq!(parse_line_suffix("src/main.rs:0"), ("src/main.rs:0", None));
        assert_eq!(split_globs("*.rs, src/** , "), vec!["*.rs", "src/**"]);
    }

    #[gpui::test]
    fn overlays_open_results_at_the_requested_line(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let host = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: mast_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        host.update(cx, |shell, window, cx| {
            shell.active_chat = "chat".into();
            shell.open_workspace_search(SearchMode::Files, window, cx);
            let input = shell.workspace_search.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_text("src/main.rs:42", cx));
            shell.workspace_search.as_mut().unwrap().rows = vec![ResultRow {
                path: "src/main.rs".into(),
                line: None,
                preview: String::new(),
                ranges: Vec::new(),
                changed: false,
            }];
            shell.select_workspace_result(0, window, cx);
            let id = match shell.resolved_right_active(cx) {
                RightSurface::File(id) => id,
                _ => panic!("file not opened"),
            };
            assert_eq!(
                shell.file_surfaces[&id]
                    .read(cx)
                    .pending_location_for_test(),
                Some((42, None))
            );
            shell.open_workspace_search(SearchMode::Content, window, cx);
            let input = shell.workspace_search.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_text("needle", cx));
            shell.workspace_search.as_mut().unwrap().rows = vec![ResultRow {
                path: "src/main.rs".into(),
                line: Some(19),
                preview: "needle".into(),
                ranges: vec![(0, 6)],
                changed: false,
            }];
            shell.select_workspace_result(0, window, cx);
            assert_eq!(
                shell.file_surfaces[&id]
                    .read(cx)
                    .pending_location_for_test(),
                Some((19, None))
            );
            shell.open_workspace_search(SearchMode::Files, window, cx);
            shell.workspace_search.as_mut().unwrap().mode = SearchMode::Line;
            let input = shell.workspace_search.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_text("7", cx));
            shell.select_workspace_result(0, window, cx);
            assert_eq!(
                shell.file_surfaces[&id]
                    .read(cx)
                    .pending_location_for_test(),
                Some((7, None))
            );
        })
        .unwrap();
    }
}
