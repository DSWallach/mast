//! Symbol-index-backed definition lookup for the editor.

use std::{cell::RefCell, collections::HashMap, ops::Range, rc::Rc, time::Duration};

use gpui::{App, Task, Window};
use gpui_base::input::{DefinitionProvider, Rope, RopeExt};
use mast_syntax::symbols::Symbol;

use super::client::WorkspaceFilesClient;

#[derive(Clone)]
pub(super) struct Definition {
    pub path: String,
    pub symbol: Symbol,
}

#[derive(Default)]
struct DefinitionState {
    revision: Option<u64>,
    path_generation: u64,
    cache: HashMap<String, Vec<Definition>>,
    parsed: Option<(String, Vec<Range<usize>>)>,
    watch: Option<Task<()>>,
    selected: Vec<Definition>,
    navigation_handled: bool,
}

pub(super) struct SymbolDefinitionProvider {
    client: WorkspaceFilesClient,
    path: RefCell<String>,
    state: Rc<RefCell<DefinitionState>>,
}

impl SymbolDefinitionProvider {
    pub fn new(client: WorkspaceFilesClient, path: String) -> Rc<Self> {
        Rc::new(Self { client, path: RefCell::new(path), state: Rc::default() })
    }

    pub fn set_path(&self, path: String) {
        *self.path.borrow_mut() = path;
        let mut state = self.state.borrow_mut();
        state.path_generation = state.path_generation.wrapping_add(1);
        state.parsed = None;
        state.cache.clear();
        state.selected.clear();
    }

    pub fn selected(&self) -> Vec<Definition> { self.state.borrow().selected.clone() }
    pub fn mark_navigation_handled(&self) { self.state.borrow_mut().navigation_handled = true; }
    pub fn take_navigation_handled(&self) -> bool { std::mem::take(&mut self.state.borrow_mut().navigation_handled) }

    pub(super) fn is_indexing(&self) -> bool { self.state.borrow().revision.is_none() }

    pub(super) fn start_watch(&self, cx: &mut App) {
        if self.state.borrow().watch.is_some() { return; }
        let client = self.client.clone();
        let state = Rc::downgrade(&self.state);
        let task = cx.spawn(async move |_| {
            let Ok(mut stream) = client.watch_symbols().await else { return; };
            while let Some(frame) = stream.recv().await {
                let Some(state) = state.upgrade() else { return; };
                let revision = frame.get("revision").and_then(|value| value.as_u64());
                let mut state = state.borrow_mut();
                if state.revision != revision {
                    state.revision = revision;
                    state.cache.clear();
                    state.selected.clear();
                }
            }
        });
        self.state.borrow_mut().watch = Some(task);
    }
}

impl DefinitionProvider for SymbolDefinitionProvider {
    fn definitions(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<anyhow::Result<Vec<lsp_types::LocationLink>>> {
        self.start_watch(cx);
        self.state.borrow_mut().selected.clear();
        let Some(revision) = self.state.borrow().revision else { return Task::ready(Ok(Vec::new())); };
        let path_generation = self.state.borrow().path_generation;
        let rope = text.clone();
        let client = self.client.clone();
        let path = self.path.borrow().clone();
        let state = self.state.clone();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let source = rope.to_string();
            let cached_ranges = state.borrow().parsed.as_ref().filter(|(cached, _)| cached == &source).map(|(_, ranges)| ranges.clone());
            let ranges = if let Some(ranges) = cached_ranges {
                ranges
            } else {
                let parsed_source = source.clone();
                let parsed_path = path.clone();
                let ranges = cx.background_executor().spawn(async move { mast_syntax::symbols::identifier_ranges(&parsed_source, &parsed_path) }).await;
                if state.borrow().path_generation != path_generation { return Ok(Vec::new()); }
                state.borrow_mut().parsed = Some((source.clone(), ranges.clone()));
                ranges
            };
            let Some(range) = ranges.into_iter().find(|range| range.contains(&offset)) else { return Ok(Vec::new()); };
            let name = source[range.clone()].to_string();
            let cached = state.borrow().cache.get(&name).cloned();
            let mut definitions = if let Some(cached) = cached { cached } else {
                let result = client.query_definitions(name.clone(), path.clone()).await.map_err(|error| anyhow::anyhow!(error.to_string()))?;
                let definitions: Vec<_> = result.matches.into_iter().map(|entry| Definition { path: entry.path, symbol: entry.symbol }).collect();
                let mut state = state.borrow_mut();
                if state.revision != Some(revision) || state.path_generation != path_generation { return Ok(Vec::new()); }
                if state.cache.len() >= 64 { state.cache.clear(); }
                state.cache.insert(name, definitions.clone());
                definitions
            };
            definitions.sort_by_key(|entry| definition_order(&path, &entry.path, entry.symbol.start_line));
            if state.borrow().revision != Some(revision) || state.borrow().path_generation != path_generation { return Ok(Vec::new()); }
            state.borrow_mut().selected = definitions.clone();
            let origin = lsp_types::Range::new(rope.offset_to_position(range.start), rope.offset_to_position(range.end));
            let mut links = Vec::with_capacity(definitions.len());
            for definition in definitions {
                let Some(uri) = url::Url::parse("file:///").ok().and_then(|base| base.join(&definition.path).ok()).and_then(|uri| uri.as_str().parse().ok()) else { continue; };
                let position = lsp_types::Position::new(definition.symbol.start_line.saturating_sub(1), 0);
                let target = lsp_types::Range::new(position, position);
                links.push(lsp_types::LocationLink { origin_selection_range: Some(origin), target_uri: uri, target_range: target, target_selection_range: target });
            }
            Ok(links)
        })
    }
}

pub(super) fn definition_order(source: &str, target: &str, line: u32) -> (u8, usize, String, u32) {
    let source_dir = source.rsplit_once('/').map_or("", |(directory, _)| directory);
    let target_dir = target.rsplit_once('/').map_or("", |(directory, _)| directory);
    let source_parts: Vec<_> = source_dir.split('/').filter(|part| !part.is_empty()).collect();
    let target_parts: Vec<_> = target_dir.split('/').filter(|part| !part.is_empty()).collect();
    let common = source_parts.iter().zip(&target_parts).take_while(|(a, b)| a == b).count();
    let distance = source_parts.len() + target_parts.len() - 2 * common;
    (if source == target { 0 } else if source_dir == target_dir { 1 } else { 2 }, distance, target.to_owned(), line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;
    #[derive(Default)]
    struct DefinitionTransport {
        queries: std::sync::atomic::AtomicUsize,
        sender: std::sync::Mutex<Option<tokio::sync::mpsc::Sender<serde_json::Value>>>,
    }
    #[async_trait::async_trait]
    impl super::super::client::WorkspaceFilesTransport for DefinitionTransport {
        async fn call(&self, _: &str, _: serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
            self.queries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(serde_json::json!({"matches": [{"path": "src/a.rs", "symbol": {"name": "call", "kind": "function", "startLine": 1, "endLine": 1}}], "truncated": false}))
        }
        async fn subscribe(&self, _: &str, _: serde_json::Value) -> Result<tokio::sync::mpsc::Receiver<serde_json::Value>, mast_rpc::RpcError> {
            let (sender, receiver) = tokio::sync::mpsc::channel(4);
            sender.try_send(serde_json::json!({"revision": 1})).unwrap();
            *self.sender.lock().unwrap() = Some(sender);
            Ok(receiver)
        }
    }

    #[gpui::test]
    fn repeated_hover_uses_the_revision_cache(cx: &mut gpui::TestAppContext) {
        use std::sync::{Arc, atomic::Ordering};
        let transport = Arc::new(DefinitionTransport::default());
        let context = super::super::client::FilesRequestContext { target: mast_proto::WorkspaceTarget { chat_id: Some("chat".into()), space_id: None, checkout_path: None }, target_device_id: None, cwd: "/fixture".into(), checkout_id: Some("checkout".into()) };
        let client = WorkspaceFilesClient::with_transport(transport.clone(), context);
        let provider = SymbolDefinitionProvider::new(client, "src/a.rs".into());
        provider.state.borrow_mut().revision = Some(1);
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(crate::theme::Theme::default()); });
        let host = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            super::super::FilesSurface::new(state, "chat".into(), false, 1000, 13.0, false, false, cx)
        });
        for expected in [1, 1] {
            let result = Rc::new(RefCell::new(None));
            let output = result.clone();
            host.update(cx, |_, window, cx| {
                let task = provider.definitions(&Rope::from("fn call() {}"), 4, window, cx);
                cx.spawn(async move |_, _| { *output.borrow_mut() = Some(task.await.unwrap().len()); }).detach();
            }).unwrap();
            cx.run_until_parked();
            cx.executor().advance_clock(Duration::from_millis(150));
            cx.run_until_parked();
            assert_eq!(*result.borrow(), Some(1));
            assert_eq!(transport.queries.load(Ordering::SeqCst), expected);
        }
        transport.sender.lock().unwrap().as_ref().unwrap().try_send(serde_json::json!({"revision": 2})).unwrap();
        cx.run_until_parked();
        assert!(provider.state.borrow().cache.is_empty());
    }

    #[gpui::test]
    fn mouse_move_without_command_does_not_start_indexing(cx: &mut gpui::TestAppContext) {
        use gpui::{Context, Entity, IntoElement, Render, Window, prelude::*, div, px, point};
        use std::sync::Arc;
        struct Fixture { editor: Entity<gpui_base::input::EditorState> }
        impl Render for Fixture {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(gpui_base::input::Editor::new(&self.editor))
            }
        }
        let transport = Arc::new(DefinitionTransport::default());
        let context = super::super::client::FilesRequestContext { target: mast_proto::WorkspaceTarget { chat_id: Some("chat".into()), space_id: None, checkout_path: None }, target_device_id: None, cwd: "/fixture".into(), checkout_id: Some("checkout".into()) };
        let provider = SymbolDefinitionProvider::new(WorkspaceFilesClient::with_transport(transport.clone(), context), "src/a.rs".into());
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(crate::theme::Theme::default()); });
        let provider_for_editor = provider.clone();
        let host = cx.add_window(|window, cx| {
            let editor = cx.new(|cx| gpui_base::input::EditorState::new(window, cx).language("src/a.rs".to_string()).default_value("fn call() {}"));
            editor.update(cx, |editor, _| editor.lsp_mut().definition_provider = Some(provider_for_editor));
            Fixture { editor }
        });
        let editor = host.read_with(cx, |fixture, _| fixture.editor.clone()).unwrap();
        cx.update_window(host.into(), |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
            let bounds = editor.read(cx).input_bounds();
            let position = bounds.origin + point(px(20.0), px(8.0));
            assert!(bounds.contains(&position));
            window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position, ..Default::default() }), cx);
        }).unwrap();
        cx.run_until_parked();
        assert!(provider.state.borrow().watch.is_none());
        assert_eq!(transport.queries.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn definition_order_prefers_file_directory_and_distance() {
        let source = "src/module/file.rs";
        let mut paths = ["tests/test.rs", "src/other.rs", "src/module/other.rs", source];
        paths.sort_by_key(|path| definition_order(source, path, 1));
        assert_eq!(paths, [source, "src/module/other.rs", "src/other.rs", "tests/test.rs"]);
    }
}

impl super::FilesSurface {
    pub(super) fn show_definition_notice(&mut self, message: String, temporary: bool, cx: &mut gpui::Context<Self>) {
        self.definition_notice_generation = self.definition_notice_generation.wrapping_add(1);
        let generation = self.definition_notice_generation;
        self.definition_notice = Some(message.into());
        cx.notify();
        if temporary {
            cx.spawn(async move |surface, cx| {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let _ = surface.update(cx, |surface, cx| {
                    if surface.definition_notice_generation == generation { surface.definition_notice = None; cx.notify(); }
                });
            }).detach();
        }
    }

    pub(super) fn go_to_definition(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(path) = self.preview.active.clone() else { return; };
        let Some(editor) = self.preview.documents.get(&path).and_then(|document| document.editor.clone()) else { return; };
        let editor = editor.read(cx);
        let source = editor.value();
        let cursor = editor.cursor();
        let Some(range) = super::editor::identifier_at_cursor(source.as_ref(), cursor) else { return; };
        let name = source[range].to_string();
        let original_source = source.to_string();
        let Some(context) = self.request_context.clone() else { return; };
        let Some(engine) = self.state.read(cx).engine().cloned() else { return; };
        let client = WorkspaceFilesClient::new(engine, context.clone());
        self.show_definition_notice("Indexing…".into(), false, cx);
        self.definition_task = Some(cx.spawn(async move |surface, cx| {
            let result = async {
                let mut stream = client.watch_symbols().await.map_err(|error| error.to_string())?;
                loop {
                    let Some(frame) = stream.recv().await else { return Err("Symbol index disconnected".to_string()); };
                    if let Some(error) = frame.get("error").and_then(|value| value.as_str()) { return Err(error.to_string()); }
                    if frame.get("revision").and_then(|value| value.as_u64()).is_some() { break; }
                }
                client.query_definitions(name.clone(), path.clone()).await.map_err(|error| error.to_string())
            }.await;
            let _ = surface.update(cx, |surface, cx| {
                let current = surface.preview.documents.get(&path).and_then(|document| document.editor.as_ref()).is_some_and(|editor| {
                    let editor = editor.read(cx);
                    editor.cursor() == cursor && editor.value().as_ref() == original_source
                });
                if surface.request_context.as_ref() != Some(&context) || surface.preview.active.as_deref() != Some(&path) || !current {
                    surface.definition_task = None;
                    surface.definition_notice = None;
                    cx.notify();
                    return;
                }
                surface.definition_task = None;
                match result {
                    Ok(result) if result.matches.is_empty() => surface.show_definition_notice(format!("No definition found for {name}"), true, cx),
                    Ok(result) => {
                        let mut definitions: Vec<_> = result.matches.into_iter().map(|entry| (entry.path, entry.symbol)).collect();
                        definitions.sort_by_key(|(target, symbol)| definition_order(&path, target, symbol.start_line));
                        surface.definition_notice = None;
                        cx.emit(super::FilesEvent::DefinitionCandidates(definitions));
                        cx.notify();
                    }
                    Err(error) => surface.show_definition_notice(error, true, cx),
                }
            });
        }));
    }
}
