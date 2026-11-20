//! Symbol-index-backed definition lookup for the editor.

use std::{cell::RefCell, collections::HashMap, ops::Range, rc::Rc, time::{Duration, Instant}};

use gpui::{App, Task, Window};
use gpui_base::input::{DefinitionProvider, Rope, RopeExt};
use mast_syntax::symbols::Symbol;

use super::client::{FilesRequestContext, WorkspaceFilesClient};

thread_local! {
    static FAILED_WATCHES: RefCell<Vec<(FilesRequestContext, Instant)>> = const { RefCell::new(Vec::new()) };
    static SHARED_WATCHES: RefCell<Vec<(FilesRequestContext, std::rc::Weak<RefCell<SharedWatch>>)>> = const { RefCell::new(Vec::new()) };
}

fn watch_failed_recently(context: &FilesRequestContext) -> bool {
    FAILED_WATCHES.with(|failures| failures.borrow().iter().any(|(failed_context, at)| failed_context == context && at.elapsed() < Duration::from_secs(60)))
}
fn clear_watch_failure(context: &FilesRequestContext) {
    FAILED_WATCHES.with(|failures| failures.borrow_mut().retain(|(failed_context, _)| failed_context != context));
}
fn record_watch_failure(context: FilesRequestContext) {
    FAILED_WATCHES.with(|failures| {
        let mut failures = failures.borrow_mut();
        failures.retain(|(failed_context, at)| failed_context != &context && at.elapsed() < Duration::from_secs(60));
        if failures.len() >= 64 { failures.remove(0); }
        failures.push((context, Instant::now()));
    });
}

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
    parsed: Option<(u64, Rc<Vec<Range<usize>>>)>,
    editor: Option<gpui::WeakEntity<gpui_base::input::EditorState>>,
    selected: Vec<Definition>,
    selected_origin_line: Option<u32>,
    navigation_handled: bool,
}

struct SharedWatch {
    revision: Option<u64>,
    watch: Option<Task<()>>,
    idle: Option<Task<()>>,
    generation: u64,
    providers: Vec<std::rc::Weak<RefCell<DefinitionState>>>,
    status: tokio::sync::watch::Sender<Option<Result<u64, String>>>,
}
impl Default for SharedWatch {
    fn default() -> Self {
        Self { revision: None, watch: None, idle: None, generation: 0, providers: Vec::new(), status: tokio::sync::watch::channel(None).0 }
    }
}

fn shared_watch(context: &FilesRequestContext) -> Rc<RefCell<SharedWatch>> {
    SHARED_WATCHES.with(|watches| {
        let mut watches = watches.borrow_mut();
        watches.retain(|(_, watch)| watch.strong_count() > 0);
        if let Some(watch) = watches.iter().find(|(key, _)| key == context).and_then(|(_, watch)| watch.upgrade()) { return watch; }
        let watch = Rc::new(RefCell::new(SharedWatch::default()));
        watches.push((context.clone(), Rc::downgrade(&watch)));
        watch
    })
}

fn update_providers(shared: &mut SharedWatch, revision: Option<u64>, cx: &mut gpui::AsyncApp) {
    shared.revision = revision;
    shared.status.send_replace(revision.map(Ok));
    shared.providers.retain(|weak| {
        let Some(state) = weak.upgrade() else { return false; };
        let editor = {
            let mut state = state.borrow_mut();
            if state.revision == revision { None } else {
                state.revision = revision;
                state.cache.clear();
                state.selected.clear();
                state.editor.clone()
            }
        };
        if let Some(editor) = editor { let _ = editor.update(cx, |editor, cx| editor.clear_hover_state(cx)); }
        true
    });
}

pub(super) struct SymbolDefinitionProvider {
    client: WorkspaceFilesClient,
    path: RefCell<String>,
    state: Rc<RefCell<DefinitionState>>,
    shared: Rc<RefCell<SharedWatch>>,
}

impl SymbolDefinitionProvider {
    pub fn new(client: WorkspaceFilesClient, path: String) -> Rc<Self> {
        let shared = shared_watch(client.request_context());
        let state = Rc::new(RefCell::new(DefinitionState::default()));
        { let mut shared = shared.borrow_mut(); state.borrow_mut().revision = shared.revision; shared.providers.push(Rc::downgrade(&state)); }
        Rc::new(Self { client, path: RefCell::new(path), state, shared })
    }

    pub fn set_editor(&self, editor: gpui::WeakEntity<gpui_base::input::EditorState>) { self.state.borrow_mut().editor = Some(editor); }

    pub fn set_path(&self, path: String) {
        *self.path.borrow_mut() = path;
        let mut state = self.state.borrow_mut();
        state.path_generation = state.path_generation.wrapping_add(1);
        state.parsed = None;
        state.cache.clear();
        state.selected.clear();
    }

    pub fn invalidate_source(&self) {
        let mut state = self.state.borrow_mut();
        state.path_generation = state.path_generation.wrapping_add(1);
        state.parsed = None;
        state.selected.clear();
    }

    pub fn check_identifier_at(&self, source: String, offset: usize, cx: &mut App) -> Task<bool> {
        let state = self.state.clone();
        let generation = state.borrow().path_generation;
        let cached = state.borrow().parsed.as_ref().filter(|(key, _)| *key == generation).map(|(_, ranges)| ranges.clone());
        let path = self.path.borrow().clone();
        cx.spawn(async move |cx| {
            let ranges = if let Some(ranges) = cached { ranges } else {
                let ranges = Rc::new(cx.background_executor().spawn(async move { mast_syntax::symbols::identifier_ranges(&source, &path) }).await);
                if state.borrow().path_generation != generation { return false; }
                state.borrow_mut().parsed = Some((generation, ranges.clone()));
                ranges
            };
            if state.borrow().path_generation != generation { return false; }
            let index = ranges.partition_point(|range| range.end <= offset);
            ranges.get(index).is_some_and(|range| range.contains(&offset))
        })
    }

    pub fn selected(&self) -> Vec<Definition> { self.state.borrow().selected.clone() }
    pub fn selected_origin_line(&self) -> Option<u32> { self.state.borrow().selected_origin_line }
    pub fn mark_navigation_handled(&self) { self.state.borrow_mut().navigation_handled = true; }
    pub fn take_navigation_handled(&self) -> bool { std::mem::take(&mut self.state.borrow_mut().navigation_handled) }

    pub(super) fn is_indexing(&self) -> bool {
        self.state.borrow().revision.is_none() && !watch_failed_recently(self.client.request_context())
    }

    pub(super) fn start_watch(&self, cx: &mut App) {
        let mut shared = self.shared.borrow_mut();
        shared.generation = shared.generation.wrapping_add(1);
        let generation = shared.generation;
        let weak = Rc::downgrade(&self.shared);
        shared.idle = Some(cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs(120)).await;
            if let Some(shared) = weak.upgrade() {
                let mut shared = shared.borrow_mut();
                if shared.generation != generation { return; }
                shared.watch = None;
                update_providers(&mut shared, None, cx);
                shared.idle = None;
            }
        }));
        if shared.watch.is_some() || watch_failed_recently(self.client.request_context()) { return; }
        shared.status.send_replace(None);
        let client = self.client.clone();
        let context = client.request_context().clone();
        let weak = Rc::downgrade(&self.shared);
        shared.watch = Some(cx.spawn(async move |cx| {
            let error = match client.watch_symbols().await {
                Ok(mut stream) => loop {
                    let Some(frame) = stream.recv().await else { break "Symbol index disconnected".to_string(); };
                    if let Some(error) = frame.get("error").and_then(|error| error.as_str()) { break error.to_string(); }
                    let Some(shared) = weak.upgrade() else { return; };
                    let revision = frame.get("revision").and_then(|value| value.as_u64());
                    if revision.is_some() { clear_watch_failure(&context); }
                    update_providers(&mut shared.borrow_mut(), revision, cx);
                },
                Err(error) => error.to_string(),
            };
            if let Some(shared) = weak.upgrade() {
                let mut shared = shared.borrow_mut();
                record_watch_failure(context);
                update_providers(&mut shared, None, cx);
                shared.status.send_replace(Some(Err(error)));
                shared.watch = None;
            }
        }));
    }

}

impl DefinitionProvider for SymbolDefinitionProvider {
    fn definitions(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<anyhow::Result<Vec<lsp_types::LocationLink>>> {
        self.start_watch(cx);
        let revision = self.state.borrow().revision;
        let path_generation = self.state.borrow().path_generation;
        let rope = text.clone();
        let client = self.client.clone();
        let path = self.path.borrow().clone();
        let state = self.state.clone();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let cached_ranges = state.borrow().parsed.as_ref().filter(|(generation, _)| *generation == path_generation).map(|(_, ranges)| ranges.clone());
            let ranges = if let Some(ranges) = cached_ranges {
                ranges
            } else {
                let parsed_source = rope.to_string();
                let parsed_path = path.clone();
                let ranges = Rc::new(cx.background_executor().spawn(async move { mast_syntax::symbols::identifier_ranges(&parsed_source, &parsed_path) }).await);
                if state.borrow().path_generation != path_generation { return Ok(Vec::new()); }
                state.borrow_mut().parsed = Some((path_generation, ranges.clone()));
                ranges
            };
            let Some(revision) = revision else { return Ok(Vec::new()); };
            let index = ranges.partition_point(|range| range.end <= offset);
            let Some(range) = ranges.get(index).filter(|range| range.contains(&offset)).cloned() else { return Ok(Vec::new()); };
            let name = rope.slice(range.clone()).to_string();
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
            let origin = lsp_types::Range::new(rope.offset_to_position(range.start), rope.offset_to_position(range.end));
            {
                let mut state = state.borrow_mut();
                state.selected = definitions.clone();
                state.selected_origin_line = Some(origin.start.line.saturating_add(1));
            }
            let mut links = Vec::with_capacity(definitions.len());
            for definition in definitions {
                let uri = "file:///definition".parse().expect("valid definition URI");
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
        subscriptions: std::sync::atomic::AtomicUsize,
        sender: std::sync::Mutex<Option<tokio::sync::mpsc::Sender<serde_json::Value>>>,
    }
    #[async_trait::async_trait]
    impl super::super::client::WorkspaceFilesTransport for DefinitionTransport {
        async fn call(&self, _: &str, _: serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
            self.queries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(serde_json::json!({"matches": [{"path": "src/a.rs", "symbol": {"name": "call", "kind": "function", "startLine": 1, "endLine": 1}}], "truncated": false}))
        }
        async fn subscribe(&self, _: &str, _: serde_json::Value) -> Result<tokio::sync::mpsc::Receiver<serde_json::Value>, mast_rpc::RpcError> {
            self.subscriptions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
                let task = provider.definitions(&Rope::from("\nfn call() {}"), 5, window, cx);
                cx.spawn(async move |_, _| { *output.borrow_mut() = Some(task.await.unwrap().len()); }).detach();
            }).unwrap();
            cx.run_until_parked();
            cx.executor().advance_clock(Duration::from_millis(150));
            cx.run_until_parked();
            assert_eq!(*result.borrow(), Some(1));
            assert_eq!(provider.selected_origin_line(), Some(2));
            assert_eq!(transport.queries.load(Ordering::SeqCst), expected);
        }
        // Moving to another word must not discard A while the editor still
        // considers A's hover active and may skip another provider call.
        host.update(cx, |_, window, cx| {
            let _pending = provider.definitions(&Rope::from("fn call() {} fn other() {}"), 17, window, cx);
            assert_eq!(provider.selected()[0].symbol.name, "call");
        }).unwrap();
        provider.invalidate_source();
        assert!(provider.state.borrow().parsed.is_none());
        assert!(provider.selected().is_empty());
        transport.sender.lock().unwrap().as_ref().unwrap().try_send(serde_json::json!({"revision": 2})).unwrap();
        cx.run_until_parked();
        assert!(provider.state.borrow().cache.is_empty());
        transport.sender.lock().unwrap().take();
        cx.run_until_parked();
        assert!(provider.state.borrow().revision.is_none());
        assert!(provider.shared.borrow().watch.is_none());
        clear_watch_failure(provider.client.request_context());
    }

    #[test]
    fn cached_identifier_ranges_reject_comments_strings_and_line_end() {
        let source = "fn value() {} // value\nlet s = \"value\";";
        let ranges = Rc::new(mast_syntax::symbols::identifier_ranges(source, "a.rs"));
        let mut state = DefinitionState::default();
        state.parsed = Some((0, ranges));
        let contains = |offset: usize| state.parsed.as_ref().unwrap().1.iter().any(|range| range.contains(&offset));
        assert!(contains(source.find("value").unwrap()));
        assert!(!contains(source.find("// value").unwrap() + 3));
        assert!(!contains(source.rfind("value").unwrap()));
        assert!(!contains(source.find('\n').unwrap()));
    }

    struct PendingDefinitionTransport {
        receiver: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<serde_json::Value>>>,
        watch: std::sync::Mutex<Option<tokio::sync::mpsc::Sender<serde_json::Value>>>,
    }
    #[async_trait::async_trait]
    impl super::super::client::WorkspaceFilesTransport for PendingDefinitionTransport {
        async fn call(&self, _: &str, _: serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
            let receiver = self.receiver.lock().unwrap().take().unwrap();
            Ok(receiver.await.unwrap())
        }
        async fn subscribe(&self, _: &str, _: serde_json::Value) -> Result<tokio::sync::mpsc::Receiver<serde_json::Value>, mast_rpc::RpcError> {
            let (sender, receiver) = tokio::sync::mpsc::channel(1);
            sender.try_send(serde_json::json!({"revision": 1})).unwrap();
            *self.watch.lock().unwrap() = Some(sender);
            Ok(receiver)
        }
    }
    #[gpui::test]
    fn pending_hover_is_discarded_after_an_edit(cx: &mut gpui::TestAppContext) {
        use std::sync::Arc;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let transport = Arc::new(PendingDefinitionTransport { receiver: std::sync::Mutex::new(Some(receiver)), watch: std::sync::Mutex::new(None) });
        let context = FilesRequestContext { target: mast_proto::WorkspaceTarget { chat_id: Some("stale-chat".into()), space_id: None, checkout_path: None }, target_device_id: None, cwd: "/stale-fixture".into(), checkout_id: Some("checkout".into()) };
        let provider = SymbolDefinitionProvider::new(WorkspaceFilesClient::with_transport(transport, context), "a.rs".into());
        provider.state.borrow_mut().revision = Some(1);
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(crate::theme::Theme::default()); });
        let host = cx.add_window(|_, cx| { let state = cx.new(|_| crate::state::AppState::new()); super::super::FilesSurface::new(state, "chat".into(), false, 1000, 13.0, false, false, cx) });
        let result = Rc::new(RefCell::new(None));
        let output = result.clone();
        host.update(cx, |_, window, cx| {
            let task = provider.definitions(&Rope::from("fn call() {}"), 4, window, cx);
            cx.spawn(async move |_, _| { *output.borrow_mut() = Some(task.await.unwrap().len()); }).detach();
        }).unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(150));
        cx.run_until_parked();
        provider.invalidate_source();
        sender.send(serde_json::json!({"exactName":true, "matches":[{"path":"a.rs", "symbol":{"name":"call", "kind":"function", "startLine":1, "endLine":1}}], "truncated":false})).unwrap();
        cx.run_until_parked();
        assert_eq!(*result.borrow(), Some(0));
        assert!(provider.selected().is_empty());
    }

    #[gpui::test]
    fn checkout_watch_is_shared_and_released_after_idle(cx: &mut gpui::TestAppContext) {
        use std::sync::Arc;
        let transport = Arc::new(DefinitionTransport::default());
        let context = FilesRequestContext { target: mast_proto::WorkspaceTarget { chat_id: Some("idle-chat".into()), space_id: None, checkout_path: None }, target_device_id: None, cwd: "/idle-fixture".into(), checkout_id: Some("checkout".into()) };
        let client = WorkspaceFilesClient::with_transport(transport.clone(), context);
        let first = SymbolDefinitionProvider::new(client.clone(), "a.rs".into());
        let second = SymbolDefinitionProvider::new(client, "b.rs".into());
        assert!(Rc::ptr_eq(&first.shared, &second.shared));
        cx.update(|cx| { first.start_watch(cx); second.start_watch(cx); });
        cx.run_until_parked();
        assert!(!transport.sender.lock().unwrap().as_ref().unwrap().is_closed());
        assert_eq!(transport.subscriptions.load(std::sync::atomic::Ordering::SeqCst), 1);
        cx.executor().advance_clock(Duration::from_secs(90));
        cx.update(|cx| first.start_watch(cx));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(90));
        cx.run_until_parked();
        assert!(!transport.sender.lock().unwrap().as_ref().unwrap().is_closed());
        cx.executor().advance_clock(Duration::from_secs(30));
        cx.run_until_parked();
        assert!(first.shared.borrow().watch.is_none());
        assert!(transport.sender.lock().unwrap().as_ref().unwrap().is_closed());
    }

    #[gpui::test]
    fn command_click_in_comments_and_strings_does_not_navigate(cx: &mut gpui::TestAppContext) {
        use std::sync::{Arc, atomic::Ordering};
        let transport = Arc::new(DefinitionTransport::default());
        let context = FilesRequestContext { target: mast_proto::WorkspaceTarget { chat_id: Some("click-chat".into()), space_id: None, checkout_path: None }, target_device_id: None, cwd: "/click-fixture".into(), checkout_id: Some("checkout".into()) };
        let provider = SymbolDefinitionProvider::new(WorkspaceFilesClient::with_transport(transport.clone(), context), "a.rs".into());
        let source = "fn value() {} // value\nlet s = \"value\";";
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(crate::theme::Theme::default()); });
        let host = cx.add_window(|_, cx| { let state = cx.new(|_| crate::state::AppState::new()); super::super::FilesSurface::new_editor(state, "click-chat".into(), "a.rs".into(), false, 1000, 13.0, false, false, cx) });
        let editor = host.update(cx, |surface, window, cx| {
            let theme = crate::theme::Theme::of(cx).clone();
            let editor = super::super::editor::new_file_editor(source, "a.rs", false, &theme, window, cx);
            let mut document = super::super::document::FileDocument::loading(super::super::document::DocumentKey { chat_id: "click-chat".into(), checkout_id: Some("checkout".into()), path: "a.rs".into() });
            document.set_loaded(mast_proto::WorkspaceFileText { checkout_id: "checkout".into(), path: "a.rs".into(), text: Some(source.into()), content_hash: Some("hash".into()), size: source.len() as u64, modified_at: None, encoding: mast_proto::WorkspaceTextEncoding::Utf8, line_ending: Some(mast_proto::WorkspaceLineEnding::Lf), read_only_reason: None, truncated: false });
            document.editor = Some(editor.clone());
            surface.preview.documents.insert("a.rs".into(), document);
            surface.preview.active = Some("a.rs".into());
            surface.definition_providers.insert("a.rs".into(), provider.clone());
            editor
        }).unwrap();
        cx.update_window(host.into(), |_, window, cx| { window.refresh(); let _ = window.draw(cx); }).unwrap();
        for offset in [source.find("// value").unwrap() + 4, source.rfind("value").unwrap() + 1] {
            provider.invalidate_source();
            cx.update_window(host.into(), |_, window, cx| {
                let position = editor.read(cx).range_to_bounds(&(offset..offset + 1)).unwrap().center();
                window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent { position, button: gpui::MouseButton::Left, click_count: 1, modifiers: gpui::Modifiers::secondary_key(), ..Default::default() }), cx);
            }).unwrap();
            cx.run_until_parked();
        }
        assert_eq!(transport.subscriptions.load(Ordering::SeqCst), 0);
        assert_eq!(transport.queries.load(Ordering::SeqCst), 0);
        provider.invalidate_source();
        cx.update_window(host.into(), |_, window, cx| {
            let offset = source.find("value").unwrap() + 1;
            let position = editor.read(cx).range_to_bounds(&(offset..offset + 1)).unwrap().center();
            window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent { position, button: gpui::MouseButton::Left, click_count: 1, modifiers: gpui::Modifiers::secondary_key(), ..Default::default() }), cx);
        }).unwrap();
        cx.run_until_parked();
        assert_eq!(transport.subscriptions.load(Ordering::SeqCst), 1);
        host.update(cx, |surface, _, _| assert_eq!(surface.definition_notice.as_deref(), Some("Indexing…"))).unwrap();
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
            let editor = cx.new(|cx| gpui_base::input::EditorState::new(window, cx).language("src/a.rs".to_string()).line_number(true).default_value("fn call() {}"));
            editor.update(cx, |editor, _| editor.lsp_mut().definition_provider = Some(provider_for_editor));
            Fixture { editor }
        });
        let editor = host.read_with(cx, |fixture, _| fixture.editor.clone()).unwrap();
        cx.update_window(host.into(), |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
            let editor = editor.read(cx);
            let bounds = editor.input_bounds();
            let text_start = editor.range_to_bounds(&(0..0)).unwrap().left() - editor.scroll_offset().x;
            let gutter = point(bounds.left() + px(1.0), bounds.top() + px(8.0));
            let text = point(text_start + px(1.0), bounds.top() + px(8.0));
            assert!(!super::super::editor::pointer_in_text(editor, gutter));
            assert!(super::super::editor::pointer_in_text(editor, text));
            let position = bounds.origin + point(px(20.0), px(8.0));
            assert!(bounds.contains(&position));
            window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position, ..Default::default() }), cx);
        }).unwrap();
        cx.run_until_parked();
        assert!(provider.shared.borrow().watch.is_none());
        assert_eq!(transport.queries.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[derive(Default)]
    struct DelayedEmptyTransport {
        sender: std::sync::Mutex<Option<tokio::sync::mpsc::Sender<serde_json::Value>>>,
    }
    #[async_trait::async_trait]
    impl super::super::client::WorkspaceFilesTransport for DelayedEmptyTransport {
        async fn call(&self, _: &str, _: serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
            Ok(serde_json::json!({"matches": [], "truncated": false}))
        }
        async fn subscribe(&self, _: &str, _: serde_json::Value) -> Result<tokio::sync::mpsc::Receiver<serde_json::Value>, mast_rpc::RpcError> {
            let (sender, receiver) = tokio::sync::mpsc::channel(1);
            *self.sender.lock().unwrap() = Some(sender);
            Ok(receiver)
        }
    }

    #[gpui::test]
    fn cold_lookup_shows_indexing_then_no_definition_notice(cx: &mut gpui::TestAppContext) {
        use std::sync::Arc;
        let transport = Arc::new(DelayedEmptyTransport::default());
        let context = super::super::client::FilesRequestContext { target: mast_proto::WorkspaceTarget { chat_id: Some("chat".into()), space_id: None, checkout_path: None }, target_device_id: None, cwd: "/fixture".into(), checkout_id: Some("checkout".into()) };
        let client = WorkspaceFilesClient::with_transport(transport.clone(), context.clone());
        let warm_client = client.clone();
        let warm_context = context.clone();
        cx.update(|cx| { gpui_base::init(cx); cx.set_global(crate::theme::Theme::default()); });
        let host = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            super::super::FilesSurface::new(state, "chat".into(), false, 1000, 13.0, false, false, cx)
        });
        host.update(cx, |surface, window, cx| {
            let path = "src/a.rs".to_string();
            let source = "missing".to_string();
            let theme = crate::theme::Theme::of(cx).clone();
            let editor = super::super::editor::new_file_editor(source.clone(), &path, false, &theme, window, cx);
            editor.update(cx, |editor, cx| editor.set_selected_range(3..3, cx));
            let mut document = super::super::document::FileDocument::loading(super::super::document::DocumentKey { chat_id: "chat".into(), checkout_id: Some("checkout".into()), path: path.clone() });
            document.editor = Some(editor);
            surface.preview.documents.insert(path.clone(), document);
            surface.preview.active = Some(path.clone());
            surface.request_context = Some(context.clone());
            surface.start_definition_lookup(client, DefinitionLookup { path, name: "missing".into(), cursor: 3, source, context, index_ready: false, status: None }, cx);
            assert!(surface.definition_notice.is_none());
        }).unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(150));
        cx.run_until_parked();
        host.update(cx, |surface, _, _| assert_eq!(surface.definition_notice.as_deref(), Some("Indexing…"))).unwrap();
        transport.sender.lock().unwrap().as_ref().unwrap().try_send(serde_json::json!({"revision": 1})).unwrap();
        cx.run_until_parked();
        host.update(cx, |surface, _, _| assert_eq!(surface.definition_notice.as_deref(), Some("No definition found for missing"))).unwrap();
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        host.update(cx, |surface, _, _| assert!(surface.definition_notice.is_none())).unwrap();
        host.update(cx, |surface, _, cx| {
            surface.start_definition_lookup(warm_client, DefinitionLookup { path: "src/a.rs".into(), name: "missing".into(), cursor: 3, source: "missing".into(), context: warm_context, index_ready: true, status: None }, cx);
        }).unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(150));
        cx.run_until_parked();
        host.update(cx, |surface, _, _| assert!(surface.definition_notice.is_none())).unwrap();
        transport.sender.lock().unwrap().as_ref().unwrap().try_send(serde_json::json!({"revision": 1})).unwrap();
        cx.run_until_parked();
        host.update(cx, |surface, _, _| assert_eq!(surface.definition_notice.as_deref(), Some("No definition found for missing"))).unwrap();
    }

    #[derive(Default)]
    struct FailingWatchTransport(std::sync::atomic::AtomicUsize);
    #[async_trait::async_trait]
    impl super::super::client::WorkspaceFilesTransport for FailingWatchTransport {
        async fn call(&self, _: &str, _: serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> { unreachable!() }
        async fn subscribe(&self, _: &str, _: serde_json::Value) -> Result<tokio::sync::mpsc::Receiver<serde_json::Value>, mast_rpc::RpcError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(mast_rpc::RpcError::Closed)
        }
    }

    #[gpui::test]
    fn failed_watch_is_not_retried_on_each_hover(cx: &mut gpui::TestAppContext) {
        use std::sync::{Arc, atomic::Ordering};
        let transport = Arc::new(FailingWatchTransport::default());
        let context = super::super::client::FilesRequestContext { target: mast_proto::WorkspaceTarget { chat_id: Some("chat".into()), space_id: None, checkout_path: None }, target_device_id: None, cwd: "/failed-watch-test".into(), checkout_id: Some("checkout".into()) };
        let provider = SymbolDefinitionProvider::new(WorkspaceFilesClient::with_transport(transport.clone(), context.clone()), "src/a.rs".into());
        let other = SymbolDefinitionProvider::new(WorkspaceFilesClient::with_transport(transport.clone(), context.clone()), "src/b.rs".into());
        cx.update(|cx| provider.start_watch(cx));
        cx.run_until_parked();
        for _ in 0..20 { cx.update(|cx| { provider.start_watch(cx); other.start_watch(cx); }); }
        cx.run_until_parked();
        assert_eq!(transport.0.load(Ordering::SeqCst), 1);
        FAILED_WATCHES.with(|failures| {
            for (key, failed_at) in failures.borrow_mut().iter_mut() {
                if *key == context { *failed_at = Instant::now() - Duration::from_secs(61); }
            }
        });
        cx.update(|cx| provider.start_watch(cx));
        cx.run_until_parked();
        assert_eq!(transport.0.load(Ordering::SeqCst), 2);
        clear_watch_failure(&context);
    }

    #[test]
    fn definition_order_prefers_file_directory_and_distance() {
        let source = "src/module/file.rs";
        let mut paths = ["tests/test.rs", "src/other.rs", "src/module/other.rs", source];
        paths.sort_by_key(|path| definition_order(source, path, 1));
        assert_eq!(paths, [source, "src/module/other.rs", "src/other.rs", "tests/test.rs"]);
    }
}

struct DefinitionLookup {
    path: String,
    name: String,
    cursor: usize,
    source: String,
    context: super::client::FilesRequestContext,
    index_ready: bool,
    status: Option<tokio::sync::watch::Receiver<Option<Result<u64, String>>>>,
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
        let Some(range) = super::editor::identifier_at_cursor(source.as_ref(), cursor, &path) else { return; };
        let name = source[range].to_string();
        let original_source = source.to_string();
        let Some(context) = self.request_context.clone() else { return; };
        let Some(engine) = self.state.read(cx).engine().cloned() else { return; };
        let client = WorkspaceFilesClient::new(engine, context.clone());
        let provider = self.definition_providers.get(&path).cloned();
        let index_ready = provider.as_ref().is_some_and(|provider| !provider.is_indexing());
        let status = provider.and_then(|provider| {
            provider.start_watch(cx);
            let shared = provider.shared.borrow();
            shared.watch.as_ref().map(|_| shared.status.subscribe())
        });
        self.start_definition_lookup(client, DefinitionLookup { path, name, cursor, source: original_source, context, index_ready, status }, cx);
    }

    fn start_definition_lookup(&mut self, client: WorkspaceFilesClient, lookup: DefinitionLookup, cx: &mut gpui::Context<Self>) {
        let DefinitionLookup { path, name, cursor, source: original_source, context, index_ready, status } = lookup;
        self.definition_notice_generation = self.definition_notice_generation.wrapping_add(1);
        let notice_generation = self.definition_notice_generation;
        self.definition_notice = None;
        let snapshot_ready = Rc::new(std::cell::Cell::new(index_ready));
        let notice_ready = snapshot_ready.clone();
        cx.spawn(async move |surface, cx| {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            if notice_ready.get() { return; }
            let _ = surface.update(cx, |surface, cx| {
                if surface.definition_notice_generation == notice_generation {
                    surface.show_definition_notice("Indexing…".into(), false, cx);
                }
            });
        }).detach();
        self.definition_task = Some(cx.spawn(async move |surface, cx| {
            let result = async {
                if let Some(mut status) = status {
                    loop {
                        let current = status.borrow_and_update().clone();
                        match current {
                            Some(Ok(_)) => break,
                            Some(Err(error)) => return Err(error),
                            None => status.changed().await.map_err(|_| "Symbol index disconnected".to_string())?,
                        }
                    }
                } else {
                    let mut stream = client.watch_symbols().await.map_err(|error| error.to_string())?;
                    loop {
                        let Some(frame) = stream.recv().await else { return Err("Symbol index disconnected".to_string()); };
                        if let Some(error) = frame.get("error").and_then(|value| value.as_str()) { return Err(error.to_string()); }
                        if frame.get("revision").and_then(|value| value.as_u64()).is_some() { break; }
                    }
                }
                snapshot_ready.set(true);
                client.query_definitions(name.clone(), path.clone()).await.map_err(|error| error.to_string())
            }.await;
            snapshot_ready.set(true);
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
                        cx.emit(super::FilesEvent::DefinitionCandidates { from_path: path.clone(), from_line: surface.current_code_line(cx), definitions });
                        cx.notify();
                    }
                    Err(error) => surface.show_definition_notice(error, true, cx),
                }
            });
        }));
    }
}
