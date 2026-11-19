use super::*;
use crate::files::client::{FilesRequestContext, WorkspaceFilesClient};
use mast_proto::{
    SearchWorkspaceContentRequest, SearchWorkspaceFilesRequest, WorkspaceContentSearchFrame,
    WorkspaceEntryKind,
};

pub(super) struct IndexedPath {
    path: String,
    lower_path: String,
    lower_name: String,
}

fn file_score(path: &IndexedPath, query: &str) -> Option<i64> {
    let name = &path.lower_name;
    let full = &path.lower_path;
    if name == query {
        return Some(10_000);
    }
    if name.starts_with(query) {
        return Some(8_000 - name.len() as i64);
    }
    if let Some(index) = name.find(query) {
        return Some(6_000 - index as i64 - name.len() as i64);
    }
    if let Some(index) = full.find(query) {
        return Some(4_000 - index as i64 - full.len() as i64);
    }
    let mut query_chars = query.chars();
    let mut wanted = query_chars.next()?;
    let mut gaps = 0i64;
    for character in full.chars() {
        if character == wanted {
            match query_chars.next() {
                Some(next) => wanted = next,
                None => return Some(2_000 - gaps - full.len() as i64),
            }
        } else {
            gaps += 1;
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchMode {
    Files,
    Symbols,
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
    symbol: Option<mast_syntax::symbols::Symbol>,
}
pub(super) struct IndexedSymbol {
    path: std::sync::Arc<str>,
    lower_path: std::sync::Arc<str>,
    lower_name: String,
    symbol: mast_syntax::symbols::Symbol,
}

fn symbol_score(symbol: &IndexedSymbol, query: &str) -> Option<i64> {
    let name = &symbol.lower_name;
    if name == query { return Some(10_000); }
    if name.starts_with(query) { return Some(8_000 - name.len() as i64); }
    if let Some(index) = name.find(query) { return Some(6_000 - index as i64 - name.len() as i64); }
    if let Some(index) = symbol.lower_path.find(query) { return Some(4_000 - index as i64 - symbol.lower_path.len() as i64); }
    let mut wanted = query.chars();
    let mut current = wanted.next()?;
    let mut gaps = 0;
    for character in name.chars().chain(symbol.lower_path.chars()) {
        if character == current {
            match wanted.next() { Some(next) => current = next, None => return Some(2_000 - gaps - name.len() as i64), }
        } else { gaps += 1; }
    }
    None
}
pub(super) struct WorkspaceSearch {
    chat_id: String,
    context: Option<FilesRequestContext>,
    mode: SearchMode,
    input: Entity<ComposerInput>,
    include: Entity<ComposerInput>,
    exclude: Entity<ComposerInput>,
    previous_focus: Option<FocusHandle>,
    focus_pending: bool,
    active: usize,
    list: gpui::ListState,
    rows: Vec<ResultRow>,
    changed: Vec<String>,
    has_diff: bool,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    changed_only: bool,
    status: String,
    generation: u64,
    query_started: std::time::Instant,
    rendered_generation: Option<u64>,
    task: Option<Task<()>>,
    symbols: Option<std::sync::Arc<Vec<IndexedSymbol>>>,
    displayed_query: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    fn refresh_workspace_symbols(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(context) = FilesRequestContext::for_chat(self.state.read(cx), &self.active_chat) else { return; };
        let Some(engine) = self.state.read(cx).engine().cloned() else { return; };
        let client = WorkspaceFilesClient::new(engine, context.clone());
        if self.workspace_symbol_index.is_some() {
            self.workspace_symbol_updating = true;
            if let Some(search) = self.workspace_search.as_mut().filter(|search| search.mode == SearchMode::Symbols) { search.status = "Updating…".into(); }
            cx.notify();
        }
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.refresh_symbols(force).await {
                let _ = this.update(cx, |this, cx| {
                    if FilesRequestContext::for_chat(this.state.read(cx), &this.active_chat).as_ref() != Some(&context) { return; }
                    this.workspace_symbol_updating = false;
                    if let Some(search) = this.workspace_search.as_mut().filter(|search| search.mode == SearchMode::Symbols) { search.status = error.to_string(); cx.notify(); }
                });
            }
        }).detach();
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn fixture_symbol_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_chat = "fixture-chat".into();
        self.open_workspace_search(SearchMode::Symbols, window, cx);
        if let Some(search) = self.workspace_search.as_mut() {
            search.task = None;
            search.rows = [("review_hunk_identity", "src/review.rs", 28, mast_syntax::symbols::SymbolKind::Function), ("ReviewHunk", "src/model.rs", 14, mast_syntax::symbols::SymbolKind::Type), ("review_hunk_stable", "tests/review.rs", 8, mast_syntax::symbols::SymbolKind::Test)].into_iter().map(|(name, path, line, kind)| ResultRow { path: path.into(), line: Some(line), preview: String::new(), ranges: Vec::new(), changed: false, symbol: Some(mast_syntax::symbols::Symbol { name: name.into(), kind, start_line: line, end_line: line + 2 }) }).collect();
            search.list.reset_with_uniform_height(search.rows.len(), px(42.0));
            search.status.clear();
        }
        cx.notify();
    }

    fn subscribe_workspace_symbols(&mut self, cx: &mut Context<Self>) {
        let context = FilesRequestContext::for_chat(self.state.read(cx), &self.active_chat);
        let engine = self.state.read(cx).engine().cloned();
        let (Some(context), Some(engine)) = (context, engine) else {
            self.workspace_symbol_key = None; self.workspace_symbol_engine = None; self.workspace_symbol_index = None; self.workspace_symbol_truncated = false; self.workspace_symbol_updating = false; self.workspace_symbol_task = None; self.workspace_symbol_error = None;
            return;
        };
        let key = format!("{}\0{}", context.target_device_id.as_deref().unwrap_or(""), context.cwd);
        if self.workspace_symbol_key.as_ref() == Some(&key) && self.workspace_symbol_engine.as_ref().is_some_and(|old| old.same_connection(&engine)) { return; }
        self.workspace_symbol_key = Some(key.clone());
        self.workspace_symbol_engine = Some(engine.clone());
        self.workspace_symbol_index = None;
        self.workspace_symbol_truncated = false;
        self.workspace_symbol_updating = false;
        self.workspace_symbol_error = None;
        let client = WorkspaceFilesClient::new(engine, context.clone());
        self.workspace_symbol_task = Some(cx.spawn(async move |this, cx| {
            let mut stream = match client.watch_symbols().await {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.workspace_symbol_key.as_ref() != Some(&key) { return; }
                        this.workspace_symbol_error = Some(error.to_string());
                        this.workspace_symbol_index = None;
                        if let Some(search) = this.workspace_search.as_mut().filter(|s| s.mode == SearchMode::Symbols) { search.status = error.to_string(); cx.notify(); }
                    });
                    return;
                }
            };
            while let Some(mut frame) = stream.recv().await {
                if let Some(error) = frame.get("error").and_then(|value| value.as_str()) {
                    let error = error.to_owned();
                    let _ = this.update(cx, |this, cx| {
                        if this.workspace_symbol_key.as_ref() != Some(&key) { return; }
                        this.workspace_symbol_error = Some(error.clone());
                        this.workspace_symbol_index = None;
                        if let Some(search) = this.workspace_search.as_mut().filter(|s| s.mode == SearchMode::Symbols) { search.status = error; cx.notify(); }
                    });
                    continue;
                }
                let truncated = frame.get("truncated").and_then(|value| value.as_bool()).unwrap_or(false);
                let updating = frame.get("updating").and_then(|value| value.as_bool()).unwrap_or(false);
                let files = frame.as_object_mut().and_then(|object| object.remove("files"));
                if files.is_none() {
                    let _ = this.update(cx, |this, cx| {
                        if this.workspace_symbol_key.as_ref() != Some(&key) { return; }
                        this.workspace_symbol_updating = updating;
                        this.workspace_symbol_truncated = truncated;
                        if let Some(search) = this.workspace_search.as_mut().filter(|search| search.mode == SearchMode::Symbols) {
                            search.status = if updating { "Updating…".into() } else if truncated { "Symbol index reached its limit; results are incomplete.".into() } else if search.rows.is_empty() { "No symbols found".into() } else { String::new() };
                            cx.notify();
                        }
                    });
                    continue;
                }
                let files = files.unwrap();
                let entries = cx.background_executor().spawn(async move {
                    let files = serde_json::from_value::<std::collections::BTreeMap<String, Vec<mast_syntax::symbols::Symbol>>>(files).ok()?;
                    let mut entries = Vec::new();
                    for (path, symbols) in files {
                        let lower_path: std::sync::Arc<str> = path.to_lowercase().into();
                        let path: std::sync::Arc<str> = path.into();
                        for symbol in symbols {
                            entries.push(IndexedSymbol { path: path.clone(), lower_path: lower_path.clone(), lower_name: symbol.name.to_lowercase(), symbol });
                        }
                    }
                    Some(std::sync::Arc::new(entries))
                }).await;
                let Some(entries) = entries else { continue; };
                if !this.update(cx, |this, cx| {
                    if this.workspace_symbol_key.as_ref() != Some(&key) { return false; }
                    this.workspace_symbol_index = Some(entries.clone());
                    this.workspace_symbol_truncated = truncated;
                    this.workspace_symbol_updating = updating;
                    this.workspace_symbol_error = None;
                    if let Some(search) = this.workspace_search.as_mut().filter(|s| s.mode == SearchMode::Symbols) { search.symbols = Some(entries); this.update_workspace_search(cx); }
                    true
                }).unwrap_or(false) { break; }
            }
            let _ = this.update(cx, |this, cx| {
                if this.workspace_symbol_key.as_ref() == Some(&key) && this.workspace_symbol_error.is_none() {
                    this.workspace_symbol_error = Some("Symbol index disconnected".into());
                    this.workspace_symbol_index = None;
                    if let Some(search) = this.workspace_search.as_mut().filter(|s| s.mode == SearchMode::Symbols) { search.status = "Symbol index disconnected".into(); cx.notify(); }
                }
            });
        }));
    }

    pub(super) fn prewarm_workspace_search(&mut self, cx: &mut Context<Self>) {
        self.subscribe_workspace_symbols(cx);
        let context = FilesRequestContext::for_chat(self.state.read(cx), &self.active_chat);
        let engine = self.state.read(cx).engine().cloned();
        let (Some(context), Some(engine)) = (context, engine) else {
            self.workspace_search_index_key = None;
            self.workspace_search_index_engine = None;
            self.workspace_search_index_task = None;
            return;
        };
        let key = format!(
            "{}\0{}",
            context.target_device_id.as_deref().unwrap_or(""),
            context.cwd
        );
        if self.workspace_search_index_key.as_ref() == Some(&key)
            && self
                .workspace_search_index_engine
                .as_ref()
                .is_some_and(|old| old.same_connection(&engine))
        {
            return;
        }
        if !self
            .workspace_search_index_engine
            .as_ref()
            .is_some_and(|old| old.same_connection(&engine))
        {
            self.workspace_search_indexes.clear();
        }
        self.workspace_search_index_key = Some(key.clone());
        self.workspace_search_index_engine = Some(engine.clone());
        let client = WorkspaceFilesClient::new(engine, context);
        self.workspace_search_index_task = Some(cx.spawn(async move |this, cx| {
            let mut stream = match client.watch_search_files().await {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.workspace_search_index_key.as_ref() == Some(&key) {
                            this.workspace_search_indexes.remove(&key);
                            this.workspace_search_index_error =
                                Some((key.clone(), error.to_string()));
                            if this
                                .workspace_search
                                .as_ref()
                                .is_some_and(|search| search.mode == SearchMode::Files)
                            {
                                this.update_workspace_search(cx);
                            }
                        }
                    });
                    return;
                }
            };
            while let Some(mut frame) = stream.recv().await {
                if let Some(error) = frame.get("error").and_then(|value| value.as_str()) {
                    let _ = this.update(cx, |this, cx| {
                        if this.workspace_search_index_key.as_ref() == Some(&key) {
                            this.workspace_search_indexes.remove(&key);
                            this.workspace_search_index_error =
                                Some((key.clone(), error.to_owned()));
                            if this
                                .workspace_search
                                .as_ref()
                                .is_some_and(|search| search.mode == SearchMode::Files)
                            {
                                this.update_workspace_search(cx);
                            }
                        }
                    });
                    continue;
                }
                let Some(files) = frame
                    .as_object_mut()
                    .and_then(|object| object.remove("files"))
                else {
                    continue;
                };
                let entries = cx
                    .background_executor()
                    .spawn(async move {
                        serde_json::from_value::<Vec<String>>(files)
                            .ok()
                            .map(|paths| {
                                std::sync::Arc::new(
                                    paths
                                        .into_iter()
                                        .map(|path| IndexedPath {
                                            lower_name: path
                                                .rsplit('/')
                                                .next()
                                                .unwrap_or(&path)
                                                .to_lowercase(),
                                            lower_path: path.to_lowercase(),
                                            path,
                                        })
                                        .collect(),
                                )
                            })
                    })
                    .await;
                let Some(entries) = entries else {
                    continue;
                };
                if !this
                    .update(cx, |this, cx| {
                        if this.workspace_search_index_key.as_ref() != Some(&key) {
                            return false;
                        }
                        this.workspace_search_index_error = None;
                        this.workspace_search_indexes.insert(key.clone(), entries);
                        if this.workspace_search.is_some() {
                            this.update_workspace_search(cx);
                        }
                        true
                    })
                    .unwrap_or(false)
                {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.workspace_search_index_key.as_ref() == Some(&key) {
                    this.workspace_search_indexes.remove(&key);
                    this.workspace_search_index_error =
                        Some((key.clone(), "Workspace file index disconnected".into()));
                    if this
                        .workspace_search
                        .as_ref()
                        .is_some_and(|search| search.mode == SearchMode::Files)
                    {
                        this.update_workspace_search(cx);
                    }
                }
            });
        }));
    }
    pub(super) fn workspace_recent_key(&self, cx: &App) -> String {
        let state = self.state.read(cx);
        let checkout = state
            .chat_diff_targets(&self.active_chat)
            .iter()
            .find(|target| target.selected)
            .map(|target| target.path.as_str())
            .unwrap_or("");
        format!("{}\0{checkout}", self.active_chat)
    }
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
                    SearchMode::Symbols => "Search symbols…",
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
        let diff = self.active_changes(cx).or_else(|| {
            self.right_tabs
                .get(&self.panel_key(cx))
                .and_then(|tabs| {
                    tabs.iter().find_map(|tab| match tab {
                        RightSurface::Diff(id) => self.diffs.get(id),
                        _ => None,
                    })
                })
                .cloned()
        });
        if let Some(diff) = &diff {
            subscriptions.push(cx.observe(diff, |this, diff, cx| {
                let Some(paths) = diff.read(cx).changed_paths() else {
                    return;
                };
                if let Some(search) = this.workspace_search.as_mut()
                    && (!search.has_diff || search.changed != paths)
                {
                    search.has_diff = true;
                    search.changed = paths;
                    this.update_workspace_search(cx);
                }
            }));
        }
        let changed = diff.and_then(|diff| diff.read(cx).changed_paths());
        let has_diff = changed.is_some();
        let changed = changed.unwrap_or_default();
        self.workspace_search = Some(WorkspaceSearch {
            chat_id: self.active_chat.clone(),
            context: FilesRequestContext::for_chat(self.state.read(cx), &self.active_chat),
            mode,
            input,
            include,
            exclude,
            previous_focus: window.focused(cx),
            focus_pending: true,
            active: 0,
            list: gpui::ListState::new(0, gpui::ListAlignment::Top, px(300.0)),
            rows: Vec::new(),
            changed,
            has_diff,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            changed_only: false,
            status: String::new(),
            generation: 0,
            query_started: std::time::Instant::now(),
            rendered_generation: None,
            task: None,
            symbols: None,
            displayed_query: None,
            _subscriptions: subscriptions,
        });
        // Re-subscribe on an explicit open so the host requests a rate-limited
        // refresh even when this checkout was already prewarmed.
        if mode == SearchMode::Symbols {
            if self.workspace_symbol_error.is_some() { self.workspace_symbol_key = None; }
            self.subscribe_workspace_symbols(cx);
            if let Some(search) = self.workspace_search.as_mut() {
                search.symbols = self.workspace_symbol_index.clone();
                if let Some(entries) = &search.symbols {
                    search.rows = entries.iter().take(200).map(|entry| ResultRow { path: entry.path.to_string(), line: Some(entry.symbol.start_line), preview: String::new(), ranges: Vec::new(), changed: false, symbol: Some(entry.symbol.clone()) }).collect();
                    search.list.reset_with_uniform_height(search.rows.len(), px(42.0));
                    search.displayed_query = Some(String::new());
                }
            }
            self.refresh_workspace_symbols(false, cx);
        } else {
            self.workspace_search_index_key = None;
            self.prewarm_workspace_search(cx);
        }
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
        let recent_key = self.workspace_recent_key(cx);
        let Some(search) = self.workspace_search.as_mut() else {
            return;
        };
        if search.chat_id != self.active_chat {
            search.task = None;
            search.rows.clear();
            search.status = "The active chat changed; reopen search.".into();
            cx.notify();
            return;
        }
        if search.context != FilesRequestContext::for_chat(self.state.read(cx), &self.active_chat) {
            search.task = None;
            search.rows.clear();
            search.status = "The workspace changed; reopen search.".into();
            cx.notify();
            return;
        }
        search.task = None;
        search.generation = search.generation.wrapping_add(1);
        search.query_started = std::time::Instant::now();
        let generation = search.generation;
        let mode = search.mode;
        let raw = search.input.read(cx).text().trim().to_owned();
        let preserve = mode == SearchMode::Symbols && search.displayed_query.as_deref() == Some(raw.as_str());
        if !preserve { search.active = 0; search.list.reset(0); search.rows.clear(); }
        search.status.clear();
        if mode == SearchMode::Line {
            cx.notify();
            return;
        }
        if mode == SearchMode::Symbols {
            let Some(symbols) = search.symbols.clone() else {
                search.status = self.workspace_symbol_error.clone().unwrap_or_else(|| "Indexing symbols…".into());
                cx.notify();
                return;
            };
            if raw.chars().count() > 256 {
                search.status = "Query must not exceed 256 characters".into();
                cx.notify();
                return;
            }
            let query = raw.to_lowercase();
            let displayed_query = raw.clone();
            let index_truncated = self.workspace_symbol_truncated;
            if self.workspace_symbol_updating { search.status = "Updating…".into(); }
            search.task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(120)).await;
                let (rows, results_truncated) = cx.background_executor().spawn(async move {
                    let mut matches: Vec<_> = symbols.iter().filter_map(|entry| {
                        if query.is_empty() { Some((entry, 0)) } else { symbol_score(entry, &query).map(|score| (entry, score)) }
                    }).collect();
                    let compare = |(a, a_score): &(&IndexedSymbol, i64), (b, b_score): &(&IndexedSymbol, i64)| b_score.cmp(a_score).then_with(|| a.lower_path.cmp(&b.lower_path)).then_with(|| a.symbol.start_line.cmp(&b.symbol.start_line));
                    let results_truncated = matches.len() > 200;
                    if results_truncated { matches.select_nth_unstable_by(200, compare); }
                    matches.truncate(200);
                    matches.sort_unstable_by(compare);
                    (matches.into_iter().map(|(entry, _)| ResultRow { path: entry.path.to_string(), line: Some(entry.symbol.start_line), preview: String::new(), ranges: Vec::new(), changed: false, symbol: Some(entry.symbol.clone()) }).collect::<Vec<_>>(), results_truncated)
                }).await;
                let _ = this.update(cx, |this, cx| {
                    let updating = this.workspace_symbol_updating;
                    let Some(search) = this.workspace_search.as_mut().filter(|s| s.mode == SearchMode::Symbols && s.generation == generation) else { return; };
                    search.rows = rows;
                    search.displayed_query = Some(displayed_query);
                    search.list.reset_with_uniform_height(search.rows.len(), px(42.0));
                    search.status = if updating { "Updating…".into() } else if index_truncated { "Symbol index reached its limit; results are incomplete.".into() } else if results_truncated { "Showing the first 200 matches".into() } else if search.rows.is_empty() { "No symbols found".into() } else { String::new() };
                    cx.notify();
                });
            }));
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
            .get(&recent_key)
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
                        symbol: None,
                        path,
                        line: None,
                        preview: String::new(),
                        ranges: Vec::new(),
                    })
                    .collect();
            }
            search
                .list
                .reset_with_uniform_height(search.rows.len(), px(42.0));
            cx.notify();
            return;
        }
        if mode == SearchMode::Files {
            if query.chars().count() > 256 {
                search.status = "Query must not exceed 256 characters".into();
                cx.notify();
                return;
            }
            if search.context.is_none() {
                search.status = "No workspace available".into();
                cx.notify();
                return;
            }
            if self.state.read(cx).engine().is_none() {
                search.status = "Workspace service unavailable".into();
                cx.notify();
                return;
            }
            let Some(index) = self
                .workspace_search_index_key
                .as_ref()
                .and_then(|key| self.workspace_search_indexes.get(key))
            else {
                let error = self
                    .workspace_search_index_error
                    .as_ref()
                    .filter(|(key, _)| Some(key) == self.workspace_search_index_key.as_ref())
                    .map(|(_, error)| error.clone());
                if error.is_some() {
                    // An older remote engine may not support the index stream.
                    // Keep quick open usable through its existing search RPC.
                    let context = search.context.clone().unwrap();
                    let engine = self.state.read(cx).engine().cloned().unwrap();
                    let client = WorkspaceFilesClient::new(engine, context.clone());
                    let request = SearchWorkspaceFilesRequest {
                        target: context.target,
                        query: query.to_owned(),
                        include_ignored: false,
                        limit: Some(200),
                    };
                    let changed = search.changed.clone();
                    search.status = "Searching…".into();
                    search.task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(150))
                            .await;
                        let result = client.search(request).await;
                        let _ = this.update(cx, |this, cx| {
                            let Some(search) = this
                                .workspace_search
                                .as_mut()
                                .filter(|search| search.generation == generation)
                            else {
                                return;
                            };
                            match result {
                                Ok(matches) => {
                                    search.rows = matches
                                        .into_iter()
                                        .filter(|entry| entry.kind == WorkspaceEntryKind::File)
                                        .map(|entry| ResultRow {
                                            changed: changed.contains(&entry.path),
                                            symbol: None,
                                            path: entry.path,
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
                                    search
                                        .list
                                        .reset_with_uniform_height(search.rows.len(), px(42.0));
                                    search.status = if search.rows.is_empty() {
                                        "No files found".into()
                                    } else {
                                        String::new()
                                    };
                                }
                                Err(error) => search.status = error.to_string(),
                            }
                            cx.notify();
                        });
                    }));
                } else {
                    search.status = "Indexing files…".into();
                }
                cx.notify();
                return;
            };
            let started = std::time::Instant::now();
            let lower = query.to_lowercase();
            let mut matches: Vec<_> = index
                .iter()
                .filter_map(|path| file_score(path, &lower).map(|score| (path, score)))
                .collect();
            let compare = |(a, a_score): &(&IndexedPath, i64),
                           (b, b_score): &(&IndexedPath, i64)| {
                b_score
                    .cmp(a_score)
                    .then_with(|| a.lower_path.cmp(&b.lower_path))
                    .then_with(|| a.path.cmp(&b.path))
            };
            if matches.len() > 200 {
                matches.select_nth_unstable_by(200, compare);
            }
            matches.truncate(200);
            matches.sort_unstable_by(compare);
            search.rows = matches
                .into_iter()
                .map(|(path, _)| ResultRow {
                    changed: search.changed.contains(&path.path),
                    symbol: None,
                    path: path.path.clone(),
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
            search
                .list
                .reset_with_uniform_height(search.rows.len(), px(42.0));
            search.status = if search.rows.is_empty() {
                "No files found".into()
            } else {
                String::new()
            };
            tracing::debug!(
                elapsed_us = started.elapsed().as_micros(),
                files = index.len(),
                "workspace files matched"
            );
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
        search.status = "Searching…".into();
        search.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            {
                match client.search_content(request).await {
                    Ok(mut stream) => {
                        let mut first_batch = true;
                        while let Some(value) = stream.recv().await {
                            if !first_batch {
                                cx.background_executor().timer(Duration::from_millis(16)).await;
                            }
                            first_batch = false;
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
                                    Ok(WorkspaceContentSearchFrame::Progress { files }) => search.status = format!("Searching… {files} files"),
                                    Ok(WorkspaceContentSearchFrame::Match {
                                        path,
                                        line,
                                        preview,
                                        ranges,
                                    }) => search.rows.push(ResultRow {
                                        changed: changed.contains(&path),
                                        symbol: None,
                                        path,
                                        line: u32::try_from(line).ok(),
                                        preview,
                                        ranges,
                                    }),
                                    Ok(WorkspaceContentSearchFrame::Complete {
                                        matches,
                                        files,
                                        skipped_files,
                                        truncated,
                                    }) => {
                                        search.status = if truncated {
                                            format!(
                                                "Showing first {matches} matches in {files} files"
                                            )
                                        } else {
                                            format!("{matches} matches in {files} files")
                                        };
                                        if skipped_files > 0 { search.status.push_str(&format!("; {skipped_files} files could not be searched")); }
                                    }
                                    Ok(WorkspaceContentSearchFrame::Error { message }) => {
                                        search.status = message
                                    }
                                    Err(error) => search.status = error.to_string(),
                                }}
                                let count = search.list.item_count();
                                if search.rows.len() > count {
                                    search.list.splice(count..count, search.rows.len() - count);
                                    search.list.clone().with_uniform_item_height(px(42.0));
                                }
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
        if search.chat_id != self.active_chat {
            self.close_workspace_search(window, cx);
            return;
        }
        if search.context != FilesRequestContext::for_chat(self.state.read(cx), &self.active_chat) {
            self.close_workspace_search(window, cx);
            return;
        }
        if search.mode == SearchMode::Line {
            let Ok(line) = search.input.read(cx).text().trim().parse::<u32>() else {
                return;
            };
            let surface = self.resolved_right_active(cx);
            self.close_workspace_search(window, cx);
            match surface {
                RightSurface::File(id) => {
                    if let Some(file) = self.file_surfaces.get(&id) {
                        file.update(cx, |file, cx| { file.record_code_jump(line, cx); file.reveal_editor_location(line, None, cx); });
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

    fn add_workspace_symbol_to_chat(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(search) = self.workspace_search.as_ref().filter(|s| s.mode == SearchMode::Symbols) else { return; };
        let Some(row) = search.rows.get(index).cloned() else { return; };
        let Some(symbol) = row.symbol else { return; };
        let Some(context) = search.context.clone() else { return; };
        let Some(engine) = self.state.read(cx).engine().cloned() else { return; };
        let Some(queue_permit) = crate::code_ref::try_queue_io() else {
            self.workspace_search.as_mut().unwrap().status = "Too many code references are loading.".into(); cx.notify(); return;
        };
        let chat_id = search.chat_id.clone();
        let state = self.state.clone();
        if !state.update(cx, |state, cx| { let accepted = state.begin_code_ref_load(&chat_id); cx.notify(); accepted }) {
            self.workspace_search.as_mut().unwrap().status = "Too many code references are loading.".into(); cx.notify(); return;
        }
        self.workspace_search.as_mut().unwrap().status = "Adding symbol…".into();
        let client = WorkspaceFilesClient::new(engine.clone(), context.clone());
        cx.spawn(async move |this, cx| {
            let _queue_permit = queue_permit;
            let _permit = crate::code_ref::acquire_io_permit().await;
            let identity = crate::code_ref::repository_identity(&engine, &context).await;
            let result = client.read_file(mast_proto::ReadWorkspaceFileRequest { target: context.target.clone(), path: row.path.clone() }).await;
            let reference = match (identity, result) {
                (Some(identity), Ok(file)) if !file.truncated && file.checkout_id == identity.checkout_id => {
                    if let Some(source) = file.text {
                        let path = row.path.clone();
                        let expected = symbol.clone();
                        cx.background_executor().spawn(async move {
                            let current = mast_syntax::symbols::extract(&source, &path, 5_000).into_iter().find(|candidate| candidate.name == expected.name && candidate.start_line == expected.start_line)?;
                            let range = mast_proto::CodeRefRange { start_line: current.start_line, end_line: current.end_line };
                            let snippet = crate::code_ref::snippet(&source, range)?;
                            Some(crate::code_ref::StagedCodeRef { id: uuid::Uuid::new_v4().to_string(), reference: mast_proto::CodeRef { version: 1, location: mast_proto::CodeRefLocation { repo_id: identity.repo_id, worktree_id: Some(identity.checkout_id).filter(|id| !id.is_empty()), repo_root: None, path }, kind: mast_proto::CodeRefKind::Symbol, range: Some(range), symbol: Some(mast_proto::CodeRefSymbol { name: current.name, kind: format!("{:?}", current.kind).to_lowercase() }), at: mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: crate::code_ref::content_hash(&source) } }, snippet })
                        }).await
                    } else { None }
                }
                _ => None,
            };
            state.update(cx, |state, cx| { state.end_code_ref_load(&chat_id); cx.notify(); });
            let _ = this.update(cx, |this, cx| {
                if FilesRequestContext::for_chat(this.state.read(cx), &chat_id).as_ref() != Some(&context) { return; }
                if let Some(reference) = reference {
                    this.state.update(cx, |state, cx| { state.add_code_ref(&chat_id, reference); cx.notify(); });
                    if let Some(search) = this.workspace_search.as_mut().filter(|search| search.chat_id == chat_id && search.mode == SearchMode::Symbols) { search.status = "Added to chat".into(); }
                } else if let Some(search) = this.workspace_search.as_mut().filter(|search| search.chat_id == chat_id && search.mode == SearchMode::Symbols) { search.status = "Symbol changed or could not be read.".into(); }
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn render_workspace_search(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let search = self.workspace_search.as_mut()?;
        if !search.rows.is_empty() && search.rendered_generation != Some(search.generation) {
            tracing::debug!(mode = ?search.mode, elapsed_ms = search.query_started.elapsed().as_millis(), rows = search.rows.len(), "workspace search results prepared for rendering");
            search.rendered_generation = Some(search.generation);
        }
        if std::mem::take(&mut search.focus_pending) {
            window.focus(&search.input.focus_handle(cx), cx);
        }
        let mode = search.mode;
        let list = search.list.clone();
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
                        if event.keystroke.modifiers.alt { this.add_workspace_symbol_to_chat(index, cx); }
                        else { this.select_workspace_result(index, window, cx); }
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
                                search.list.scroll_to_reveal_item(search.active);
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
                if id == 3 && !search.has_diff {
                    continue;
                }
                buttons = buttons.child(
                    div()
                        .id(("search-option", id))
                        .role(gpui::Role::Button)
                        .aria_label(label)
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
        if mode == SearchMode::Symbols {
            card = card.child(
                div().flex().justify_end().child(
                    crate::components::button("refresh-symbols", "Refresh", crate::components::ButtonVariant::Ghost, crate::components::ButtonSize::Sm, &theme)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_workspace_symbols(true, cx))),
                ),
            );
        }
        let results = gpui::list(list, cx.processor(Self::render_workspace_search_row))
            .h(px(
                if mode == SearchMode::Symbols { (search.rows.len() as f32 * 42.0).clamp(42.0, 320.0).min((f32::from(viewport.height) - 200.0).max(80.0)) }
                else { 320.0_f32.min((f32::from(viewport.height) - 200.0).max(80.0)) }
            ))
            .with_sizing_behavior(gpui::ListSizingBehavior::Auto);
        card = card
            .when(mode != SearchMode::Line, |card| card.child(results))
            .child(
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
    fn render_workspace_search_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(search) = self.workspace_search.as_ref() else {
            return gpui::Empty.into_any_element();
        };
        let Some(row) = search.rows.get(index).cloned() else {
            return gpui::Empty.into_any_element();
        };
        let previous = index
            .checked_sub(1)
            .and_then(|index| search.rows.get(index));
        let header = match search.mode {
            SearchMode::Content if previous.is_none_or(|previous| previous.path != row.path) => {
                Some(row.path.clone())
            }
            SearchMode::Files
                if previous.is_none_or(|previous| previous.changed != row.changed) =>
            {
                Some(
                    if row.changed {
                        "Changed in this diff"
                    } else {
                        "Files"
                    }
                    .to_owned(),
                )
            }
            _ => None,
        };
        let theme = Theme::of(cx).for_popup();
        let label = if let Some(symbol) = &row.symbol {
            div().w_full().flex().items_center().gap(px(8.0))
                .child(div().w(px(56.0)).flex_none().text_color(theme.text_muted).child(format!("{:?}", symbol.kind).to_lowercase()))
                .child(div().min_w_0().flex_1().truncate().child(symbol.name.clone()))
                .child(div().min_w_0().max_w(px(280.0)).truncate().text_color(theme.text_muted).child(format!("{}:{}", row.path, symbol.start_line)))
                .into_any_element()
        } else if let Some(line) = row.line {
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
        div()
            .id(("workspace-search-result", index))
            .role(gpui::Role::Button)
            .aria_label(row.path.clone())
            .h(px(42.0))
            .w_full()
            .px(px(8.0))
            .flex()
            .flex_col()
            .justify_end()
            .cursor_pointer()
            .when(index == search.active, |el| el.bg(theme.surface_raised))
            .when_some(header, |el, header| {
                el.child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.text_muted)
                        .truncate()
                        .child(header),
                )
            })
            .child(div().truncate().child(label))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_workspace_result(index, window, cx)
            }))
            .into_any_element()
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
        let path = IndexedPath {
            path: "src/Workspace.rs".into(),
            lower_path: "src/workspace.rs".into(),
            lower_name: "workspace.rs".into(),
        };
        assert!(file_score(&path, "workspace").unwrap() > file_score(&path, "ws").unwrap());
        assert!(file_score(&path, "missing").is_none());
    }

    #[gpui::test]
    fn alternate_enter_stages_the_selected_symbol(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (mut wire, engine, _guard) = crate::admin::tests::Wire::new();
        cx.update(|cx| { settings::init(settings::UiSettings::default(), dir.path(), cx); gpui_base::init(cx); cx.set_global(Theme::default()); crate::app_menus::init(cx); });
        let host = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(state, EngineBootConfig { data_dir: dir.path().into(), ipc_port: 0, edge_url: String::new(), edge_token: None, org_id: None, workos_client_id: None, default_harness: mast_proto::HarnessId::Mock }, cx)
        });
        host.update(cx, |shell, window, cx| {
            shell.debug_gate = Some(GatePhase::Ready);
            shell.state.update(cx, |state, cx| {
                state.chats = vec![serde_json::from_value(serde_json::json!({"id":"chat", "deviceId":"local", "cwd":"/fixture", "archived":false, "createdAt":Utc::now()})).unwrap()];
                state.set_test_engine(engine);
                state.select_chat(Some("chat".into()), cx);
            });
            shell.on_state_changed(&shell.state.clone(), cx);
            shell.open_workspace_search(SearchMode::Symbols, window, cx);
            let search = shell.workspace_search.as_mut().unwrap();
            search.task = None;
            search.rows = vec![ResultRow { path: "src/a.rs".into(), line: Some(1), preview: String::new(), ranges: Vec::new(), changed: false, symbol: Some(mast_syntax::symbols::Symbol { name: "answer".into(), kind: mast_syntax::symbols::SymbolKind::Function, start_line: 1, end_line: 1 }) }];
            search.list.reset_with_uniform_height(1, px(42.0));
            window.focus(&search.input.focus_handle(cx), cx);
        }).unwrap();
        cx.run_until_parked();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear()).unwrap();
        host.update(cx, |shell, window, cx| {
            let search = shell.workspace_search.as_ref().unwrap();
            window.focus(&search.input.focus_handle(cx), cx);
        }).unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear()).unwrap();
        cx.simulate_keystrokes(host.into(), "alt-enter");
        for _ in 0..4 {
            cx.run_until_parked();
            wire.answer(&|method| match method {
                mast_rpc::methods::DESCRIBE_EXECUTION_REPOSITORY => Some(serde_json::json!({"repoId":"repo", "repoPath":"/fixture", "checkoutId":"checkout"})),
                mast_rpc::methods::READ_WORKSPACE_FILE => Some(serde_json::json!({"checkoutId":"checkout", "path":"src/a.rs", "text":"fn answer() {}\n", "size":15, "encoding":"utf8", "truncated":false})),
                _ => None,
            });
        }
        cx.run_until_parked();
        host.update(cx, |shell, _, cx| {
            let refs = shell.state.read(cx).code_refs("chat");
            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0].reference.kind, mast_proto::CodeRefKind::Symbol);
            assert_eq!(refs[0].reference.symbol.as_ref().unwrap().name, "answer");
            assert_eq!(refs[0].snippet, "fn answer() {}");
        }).unwrap();
    }

    #[gpui::test]
    fn search_debounces_edits_and_falls_back_for_older_engines(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (mut wire, engine, _guard) = crate::admin::tests::Wire::new();
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
            shell.debug_gate = Some(GatePhase::Ready);
            shell.state.update(cx, |state, cx| {
                state.chats = vec![serde_json::from_value(serde_json::json!({"id":"chat", "deviceId":"local", "cwd":"/tmp", "archived":false, "createdAt":Utc::now()})).unwrap()];
                state.set_test_engine(engine);
                state.select_chat(Some("chat".into()), cx);
            });
            shell.on_state_changed(&shell.state.clone(), cx);
            shell.open_workspace_search(SearchMode::Content, window, cx);
        }).unwrap();
        cx.run_until_parked();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        host.update(cx, |shell, window, cx| {
            let input = &shell.workspace_search.as_ref().unwrap().input;
            window.focus(&input.focus_handle(cx), cx);
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_input(host.into(), "a");
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        assert!(
            wire.sent()
                .iter()
                .all(|(method, _)| method != mast_rpc::methods::SEARCH_WORKSPACE_CONTENT)
        );
        cx.simulate_input(host.into(), "b");
        cx.run_until_parked();
        host.update(cx, |shell, _, cx| {
            let search = shell.workspace_search.as_ref().unwrap();
            assert_eq!(search.input.read(cx).text(), "ab");
            assert_eq!(search.status, "Searching…");
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        assert!(
            wire.sent()
                .iter()
                .all(|(method, _)| method != mast_rpc::methods::SEARCH_WORKSPACE_CONTENT)
        );
        cx.executor().advance_clock(Duration::from_millis(60));
        cx.run_until_parked();
        let searches: Vec<_> = wire
            .sent()
            .into_iter()
            .filter(|(method, _)| method == mast_rpc::methods::SEARCH_WORKSPACE_CONTENT)
            .collect();
        assert_eq!(searches.len(), 1);
        assert_eq!(searches[0].1["query"], "ab");
        host.update(cx, |shell, window, cx| {
            shell.close_workspace_search(window, cx);
            shell.open_workspace_search(SearchMode::Files, window, cx);
            let key = shell.workspace_search_index_key.clone().unwrap();
            shell.workspace_search_indexes.remove(&key);
            shell.workspace_search_index_error = Some((key, "unsupported".into()));
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        host.update(cx, |shell, window, cx| {
            let input = &shell.workspace_search.as_ref().unwrap().input;
            window.focus(&input.focus_handle(cx), cx);
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_input(host.into(), "file");
        cx.run_until_parked();
        let mut subscriptions = 0;
        while let Ok(frame) = wire.requests.try_recv() {
            let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
            // Dropping the prewarmed stream also sends a cancellation frame.
            if frame["method"] == mast_rpc::methods::WATCH_WORKSPACE_SEARCH_FILES {
                subscriptions += 1;
            }
        }
        assert_eq!(subscriptions, 1, "reopening requests a fresh index subscription");
        cx.executor().advance_clock(Duration::from_millis(160));
        cx.run_until_parked();
        let mut searches = Vec::new();
        while let Ok(frame) = wire.requests.try_recv() {
            let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
            if frame["method"] == mast_rpc::methods::SEARCH_WORKSPACE_FILES {
                searches.push(frame["params"].clone());
            }
        }
        assert_eq!(searches.len(), 1);
        assert_eq!(searches[0]["query"], "file");
    }

    #[gpui::test]
    #[ignore]
    fn measure_workspace_search_latency(cx: &mut TestAppContext) {
        // The in-memory RPC service runs on Tokio threads, so this benchmark
        // intentionally permits external wakeups while retaining a controlled clock.
        cx.executor().allow_parking();
        cx.executor().forbid_parking();
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let core = mast_engine::EngineCore::assemble(
            dir.path(),
            std::sync::Arc::new(mast_engine::HarnessRegistry::new()),
            mast_proto::HarnessId::Mock,
            None,
        )
        .unwrap();
        let root =
            std::env::var("MAST_BENCH_ROOT").expect("MAST_BENCH_ROOT must name the checkout");
        core.workspace
            .create_space("space", &core.device_id, &root, None, true)
            .unwrap();
        core.workspace
            .create_chat("chat", Some("space"), None, None, None)
            .unwrap();
        let chat = core.workspace.chat("chat").unwrap().unwrap();
        let device_id = core.device_id.clone();
        let engine = crate::state::EngineHandle::from_test_client(mast_rpc::memory_client(
            core.rpc_service(),
        ));
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
            shell.debug_gate = Some(GatePhase::Ready);
            shell.state.update(cx, |state, cx| {
                state.chats = vec![chat];
                state.local_device_id = Some(device_id);
                state.set_test_engine(engine);
                state.select_chat(Some("chat".into()), cx);
            });
            shell.on_state_changed(&shell.state.clone(), cx);
            shell.open_workspace_search(SearchMode::Files, window, cx);
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        let prewarm_started = std::time::Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(10));
            cx.executor().advance_clock(Duration::from_millis(10));
            cx.run_until_parked();
            if host
                .read_with(cx, |shell, _| !shell.workspace_search_indexes.is_empty())
                .unwrap()
            {
                break;
            }
            assert!(prewarm_started.elapsed() < Duration::from_secs(20));
        }
        eprintln!("index prewarm: {:?}", prewarm_started.elapsed());
        for ch in "workspace".chars() {
            let start = std::time::Instant::now();
            cx.simulate_input(host.into(), &ch.to_string());
            loop {
                std::thread::sleep(Duration::from_millis(10));
                cx.executor().advance_clock(Duration::from_millis(10));
                cx.run_until_parked();
                cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
                    .unwrap();
                let done = host
                    .read_with(cx, |shell, _| {
                        shell
                            .workspace_search
                            .as_ref()
                            .is_some_and(|search| !search.rows.is_empty())
                    })
                    .unwrap();
                if done {
                    break;
                }
                assert!(start.elapsed() < Duration::from_secs(10));
            }
            eprintln!("query character {ch}: {:?}", start.elapsed());
        }
        host.update(cx, |shell, window, cx| {
            shell.close_workspace_search(window, cx);
            shell.open_workspace_search(SearchMode::Content, window, cx);
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        let content_started = std::time::Instant::now();
        cx.simulate_input(host.into(), "WorkspaceSearch");
        let mut first = None;
        loop {
            std::thread::sleep(Duration::from_millis(10));
            cx.executor().advance_clock(Duration::from_millis(10));
            cx.run_until_parked();
            cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
                .unwrap();
            let (has_results, complete) = host
                .read_with(cx, |shell, _| {
                    let search = shell.workspace_search.as_ref().unwrap();
                    (
                        !search.rows.is_empty(),
                        search.status.contains("matches in"),
                    )
                })
                .unwrap();
            if has_results && first.is_none() {
                first = Some(content_started.elapsed());
            }
            if complete {
                break;
            }
            assert!(content_started.elapsed() < Duration::from_secs(20));
        }
        eprintln!(
            "content first: {:?}; complete: {:?}",
            first,
            content_started.elapsed()
        );
        host.update(cx, |shell, window, cx| {
            shell.close_workspace_search(window, cx);
            shell.workspace_search_index_task = None;
        })
        .unwrap();
        runtime.block_on(core.shutdown());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn configured_shortcuts_open_the_search_overlays(cx: &mut TestAppContext) {
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
        host.update(cx, |shell, _, cx| {
            shell.debug_gate = Some(GatePhase::Ready);
            shell.state.update(cx, |state, cx| {
                state.chats = vec![serde_json::from_value(serde_json::json!({"id":"chat", "deviceId":"local", "cwd":"/tmp", "archived":false, "createdAt":Utc::now()})).unwrap()];
                state.select_chat(Some("chat".into()), cx);
            });
            shell.on_state_changed(&shell.state.clone(), cx);
        }).unwrap();
        cx.run_until_parked();
        cx.update_window(host.into(), |_, window, cx| {
            window.draw(cx).clear();
            let context = gpui::KeyContext::parse("Input").unwrap();
            let replace =
                window.bindings_for_action_in_context(&gpui_base::input::Replace, context);
            let new_key = gpui::Keystroke::parse(&platform_combo("mod-alt-f")).unwrap();
            let old_key = gpui::Keystroke::parse(&platform_combo("mod-shift-f")).unwrap();
            assert!(replace.iter().any(|binding| {
                binding.match_keystrokes(std::slice::from_ref(&new_key)) == Some(false)
            }));
            assert!(!replace.iter().any(|binding| {
                binding.match_keystrokes(std::slice::from_ref(&old_key)) == Some(false)
            }));
        })
        .unwrap();
        cx.simulate_keystrokes(host.into(), &platform_combo("mod-o"));
        host.update(cx, |shell, window, cx| {
            let input = &shell.workspace_search.as_ref().unwrap().input;
            input.update(cx, |input, cx| input.set_text("hello world", cx));
            window.focus(&input.focus_handle(cx), cx);
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_keystrokes(host.into(), "backspace");
        host.update(cx, |shell, _, cx| {
            assert_eq!(
                shell
                    .workspace_search
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .text(),
                "hello worl"
            );
        })
        .unwrap();
        cx.simulate_keystrokes(host.into(), "left delete alt-backspace");
        host.update(cx, |shell, _, cx| {
            assert_eq!(
                shell
                    .workspace_search
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .text(),
                "hello "
            );
        })
        .unwrap();
        cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("pasted".into())));
        cx.simulate_keystrokes(host.into(), "cmd-a cmd-v");
        host.update(cx, |shell, _, cx| {
            assert_eq!(
                shell
                    .workspace_search
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .text(),
                "pasted"
            );
        })
        .unwrap();
        host.update(cx, |shell, window, cx| {
            assert!(
                shell
                    .workspace_search
                    .as_ref()
                    .is_some_and(|search| search.mode == SearchMode::Files)
            );
            let search = shell.workspace_search.as_mut().unwrap();
            search.rows = ["one.rs", "two.rs"]
                .into_iter()
                .map(|path| ResultRow {
                    path: path.into(),
                    line: None,
                    preview: String::new(),
                    ranges: Vec::new(),
                    changed: false,
                    symbol: None,
                })
                .collect();
            search.list.reset_with_uniform_height(2, px(42.0));
            window.focus(&search.input.focus_handle(cx), cx);
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_keystrokes(host.into(), "down");
        host.update(cx, |shell, _, _| {
            assert_eq!(shell.workspace_search.as_ref().unwrap().active, 1)
        })
        .unwrap();
        cx.simulate_keystrokes(host.into(), "up enter");
        host.update(cx, |shell, window, cx| {
            assert!(shell.workspace_search.is_none());
            shell.open_workspace_search(SearchMode::Files, window, cx);
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_keystrokes(host.into(), "escape");
        host.update(cx, |shell, _, _| assert!(shell.workspace_search.is_none()))
            .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_keystrokes(host.into(), &platform_combo("mod-shift-f"));
        host.update(cx, |shell, window, cx| {
            let input = &shell.workspace_search.as_ref().unwrap().input;
            input.update(cx, |input, cx| input.set_text("hello world", cx));
            window.focus(&input.focus_handle(cx), cx);
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_keystrokes(host.into(), "backspace");
        host.update(cx, |shell, _, cx| {
            assert_eq!(
                shell
                    .workspace_search
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .text(),
                "hello worl"
            );
        })
        .unwrap();
        cx.simulate_keystrokes(host.into(), "left delete alt-backspace");
        host.update(cx, |shell, _, cx| {
            assert_eq!(
                shell
                    .workspace_search
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .text(),
                "hello "
            );
        })
        .unwrap();
        cx.simulate_keystrokes(host.into(), "cmd-a cmd-v");
        host.update(cx, |shell, _, cx| {
            assert_eq!(
                shell
                    .workspace_search
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .text(),
                "pasted"
            );
        })
        .unwrap();
        for include in [true, false] {
            host.update(cx, |shell, window, cx| {
                let search = shell.workspace_search.as_ref().unwrap();
                let input = if include {
                    &search.include
                } else {
                    &search.exclude
                };
                input.update(cx, |input, cx| input.set_text("*.rs", cx));
                window.focus(&input.focus_handle(cx), cx);
            })
            .unwrap();
            cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
                .unwrap();
            cx.simulate_keystrokes(host.into(), "backspace");
            host.update(cx, |shell, _, cx| {
                let search = shell.workspace_search.as_ref().unwrap();
                let input = if include {
                    &search.include
                } else {
                    &search.exclude
                };
                assert_eq!(input.read(cx).text(), "*.r");
            })
            .unwrap();
        }
        host.update(cx, |shell, _, _| {
            let search = shell.workspace_search.as_mut().unwrap();
            search.rows = ["one.rs", "two.rs"]
                .into_iter()
                .map(|path| ResultRow {
                    path: path.into(),
                    line: Some(1),
                    preview: String::new(),
                    ranges: Vec::new(),
                    changed: false,
                    symbol: None,
                })
                .collect();
            search.list.reset_with_uniform_height(2, px(42.0));
            search.active = 0;
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_keystrokes(host.into(), "down");
        host.update(cx, |shell, _, _| {
            assert_eq!(shell.workspace_search.as_ref().unwrap().active, 1)
        })
        .unwrap();
        cx.simulate_keystrokes(host.into(), "up");
        host.update(cx, |shell, _, _| {
            assert_eq!(shell.workspace_search.as_ref().unwrap().active, 0)
        })
        .unwrap();
        cx.simulate_keystrokes(host.into(), "escape");
        host.update(cx, |shell, window, cx| {
            assert!(shell.workspace_search.is_none());
            shell.open_workspace_search(SearchMode::Content, window, cx);
            let search = shell.workspace_search.as_mut().unwrap();
            search.rows.push(ResultRow {
                path: "one.rs".into(),
                line: Some(1),
                preview: String::new(),
                ranges: Vec::new(),
                changed: false,
                symbol: None,
            });
            search.list.reset_with_uniform_height(1, px(42.0));
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_keystrokes(host.into(), "enter");
        host.update(cx, |shell, window, cx| {
            assert!(shell.workspace_search.is_none());
            shell.open_workspace_search(SearchMode::Content, window, cx);
        })
        .unwrap();
        host.update(cx, |shell, _, _| {
            assert!(
                shell
                    .workspace_search
                    .as_ref()
                    .is_some_and(|search| search.mode == SearchMode::Content)
            )
        })
        .unwrap();
        host.update(cx, |shell, window, cx| {
            shell.close_workspace_search(window, cx);
            let mut map = KeymapConfig::default();
            map.quick_open = "mod-alt-o".into();
            apply_keymap(cx, &map, ComposerSendBehavior::default());
        })
        .unwrap();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        cx.simulate_keystrokes(host.into(), &platform_combo("mod-alt-o"));
        host.update(cx, |shell, _, _| {
            assert!(
                shell
                    .workspace_search
                    .as_ref()
                    .is_some_and(|search| search.mode == SearchMode::Files)
            )
        })
        .unwrap();
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
                symbol: None,
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
                symbol: None,
            }];
            shell.select_workspace_result(0, window, cx);
            assert_eq!(
                shell.file_surfaces[&id]
                    .read(cx)
                    .pending_location_for_test(),
                Some((19, None))
            );
            shell.open_workspace_search(SearchMode::Files, window, cx);
            let search = shell.workspace_search.as_mut().unwrap();
            search.mode = SearchMode::Symbols;
            search.rows = vec![ResultRow {
                path: "src/main.rs".into(), line: Some(31), preview: String::new(), ranges: Vec::new(), changed: false,
                symbol: Some(mast_syntax::symbols::Symbol { name: "example".into(), kind: mast_syntax::symbols::SymbolKind::Function, start_line: 31, end_line: 33 }),
            }];
            shell.select_workspace_result(0, window, cx);
            assert_eq!(shell.file_surfaces[&id].read(cx).pending_location_for_test(), Some((31, None)));
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
