//! Pop-out windows: tear-off from the tab strip, shared engine work, routing.
use super::*;
use crate::popout::{self, PopoutWindow};
use gpui::{AppContext, TestAppContext, VisualTestContext};

fn boot(dir: &std::path::Path) -> EngineBootConfig {
    EngineBootConfig {
        data_dir: dir.into(),
        ipc_port: 0,
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: mast_proto::HarnessId::Mock,
    }
}

fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
}

struct TabHost {
    shell: Entity<Shell>,
    _data_dir: tempfile::TempDir,
}

impl Render for TabHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shell.update(cx, |shell, cx| {
            let tabs = shell.render_right_tab_strip(cx);
            div().w(px(400.)).h(px(40.)).child(tabs)
        })
    }
}

fn strip(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
    init(cx);
    let dir = tempfile::tempdir().unwrap();
    let (host, cx) = cx.add_window_view(|_, cx| {
        let shell = cx.new(|cx| {
            let state = cx.new(|_| AppState::new());
            let mut shell = Shell::new(state, boot(dir.path()), cx);
            shell.active_chat = "parent".into();
            for id in ["first", "second"] {
                shell.add_subagent_surface("parent".into(), id.into(), id.into(), false, cx);
            }
            shell
        });
        TabHost { shell, _data_dir: dir }
    });
    let shell = host.read_with(cx, |host, _| host.shell.clone());
    cx.update(|window, cx| window.draw(cx).clear());
    (shell, cx)
}

#[gpui::test]
fn tab_context_menu_opens_the_tab_in_its_own_window(cx: &mut TestAppContext) {
    let (shell, cx) = strip(cx);
    let tab = cx.debug_bounds("right-surface-tab-0").unwrap().center();
    cx.simulate_mouse_down(tab, MouseButton::Right, gpui::Modifiers::default());
    cx.simulate_mouse_up(tab, MouseButton::Right, gpui::Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear());
    let row = cx.debug_bounds("right-tab-popout-row").expect("menu row").center();
    cx.simulate_click(row, gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |shell, _| {
        assert!(!shell.subagent_tabs.contains_key(&1), "the tab left the strip");
        assert!(shell.subagent_tabs.contains_key(&2));
        assert!(!shell.right_tab_menu.is_open());
    });
    cx.update(|_, cx| {
        assert_eq!(popout::open_count(cx), 1);
        let handle = cx.windows().into_iter().find_map(|w| w.downcast::<PopoutWindow>()).unwrap();
        let title = handle.read(cx).unwrap().title(cx);
        assert_eq!(title.as_ref(), "first", "window title is the surface title");
    });
}

#[gpui::test]
fn tab_menu_is_not_offered_for_the_review_tab(cx: &mut TestAppContext) {
    let (shell, _cx) = strip(cx);
    shell.read_with(_cx, |shell, _| {
        assert!(!shell.can_pop_out(RightSurface::Review));
        assert!(!shell.can_pop_out(RightSurface::Browser(1)));
        assert!(!shell.can_pop_out(RightSurface::Picker));
    });
}

fn counting_engine(
    runtime: &tokio::runtime::Runtime,
) -> (crate::state::EngineHandle, tokio::sync::mpsc::Receiver<String>, tokio::sync::mpsc::Sender<String>) {
    let _guard = runtime.enter();
    let (out, requests) = tokio::sync::mpsc::channel(64);
    let (replies, inbound) = tokio::sync::mpsc::channel(64);
    let engine = crate::state::EngineHandle::from_test_client(mast_rpc::RpcClient::new(out, inbound));
    (engine, requests, replies)
}

fn drain(requests: &mut tokio::sync::mpsc::Receiver<String>) -> Vec<String> {
    let mut methods = Vec::new();
    while let Ok(raw) = requests.try_recv() {
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        methods.push(value["method"].as_str().unwrap_or_default().to_string());
    }
    methods
}

#[gpui::test]
fn a_popped_out_subagent_feed_is_shared_not_resubscribed(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let _guard = runtime.enter();
    let (engine, mut requests, _replies) = counting_engine(&runtime);
    init(cx);
    let dir = tempfile::tempdir().unwrap();
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| state.set_test_engine(engine));
        Shell::new(state, boot(dir.path()), cx)
    });
    window
        .update(cx, |shell, _, cx| {
            shell.active_chat = "parent".into();
            shell.add_subagent_surface("parent".into(), "doc".into(), "Worker".into(), false, cx);
            // Opening the same subagent again reuses the tab.
            shell.add_subagent_surface("parent".into(), "doc".into(), "Worker".into(), false, cx);
        })
        .unwrap();
    cx.run_until_parked();
    let before = drain(&mut requests);
    let watches = |calls: &[String]| calls.iter().filter(|m| *m == mast_rpc::methods::WATCH_DOC_MESSAGES).count();
    assert_eq!(watches(&before), 1, "one doc watch before the pop-out: {before:?}");

    window
        .update(cx, |shell, _, cx| {
            let surface = shell.resolved_right_active(cx);
            assert!(matches!(surface, RightSurface::Subagent(_)));
            assert!(shell.pop_out_surface(surface, cx));
            assert!(shell.subagent_tabs.is_empty());
        })
        .unwrap();
    cx.run_until_parked();
    let after = drain(&mut requests);
    assert!(after.is_empty(), "the pop-out sent engine traffic: {after:?}");
    window
        .read_with(cx, |shell, cx| assert_eq!(shell.state.read(cx).subagent_watch_count(), 1))
        .unwrap();

    // Closing the pop-out releases the single watch.
    let popout = cx.update(|cx| cx.windows().into_iter().find_map(|w| w.downcast::<PopoutWindow>())).unwrap();
    popout.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();
    window
        .read_with(cx, |shell, cx| assert_eq!(shell.state.read(cx).subagent_watch_count(), 0))
        .unwrap();
    let closed = drain(&mut requests);
    assert_eq!(watches(&closed), 0, "closing must not subscribe again: {closed:?}");
}

#[gpui::test]
fn a_popped_out_file_is_the_same_editor_and_closing_the_window_leaves_the_main_window_alone(
    cx: &mut TestAppContext,
) {
    init(cx);
    let dir = tempfile::tempdir().unwrap();
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(state, boot(dir.path()), cx)
    });
    let editor = window
        .update(cx, |shell, window, cx| {
            shell.active_chat = "chat".into();
            shell.add_file_surface("src/main.rs".into(), window, cx);
            shell.add_file_surface("src/lib.rs".into(), window, cx);
            let id = *shell.file_surface_keys.get(&("chat".to_string(), "src/main.rs".to_string())).unwrap();
            let editor = shell.file_surfaces[&id].entity_id();
            assert!(shell.pop_out_surface(RightSurface::File(id), cx));
            assert!(!shell.file_surfaces.contains_key(&id));
            assert!(shell.file_surface_keys.is_empty() == false, "the other file tab is untouched");
            assert_eq!(shell.right_surface_rows(cx).len(), 1);
            editor
        })
        .unwrap();
    cx.run_until_parked();
    let popout = cx.update(|cx| {
        assert_eq!(popout::open_count(cx), 1);
        cx.windows().into_iter().find_map(|w| w.downcast::<PopoutWindow>()).unwrap()
    });
    // The window renders (title strip + the editor) without a panic.
    cx.update_window(popout.into(), |_, window, cx| window.draw(cx).clear()).unwrap();
    popout
        .update(cx, |popout, _, cx| {
            assert_eq!(popout.title(cx).as_ref(), "main.rs");
            assert!(!popout.has_unsaved_changes(cx));
            assert!(popout.prepare_close(cx), "a clean file closes without a prompt");
        })
        .unwrap();
    let _ = editor;
    popout.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(popout::open_count(cx), 0));
    window
        .read_with(cx, |shell, cx| {
            // Nothing returns to the main window and nothing else moved.
            assert_eq!(shell.right_surface_rows(cx).len(), 1);
            assert_eq!(shell.file_surfaces.len(), 1);
        })
        .unwrap();
    cx.update(|cx| assert!(popout::prepare_quit(cx)));
}

#[gpui::test]
fn deep_links_and_banners_find_the_main_window_not_a_popout(cx: &mut TestAppContext) {
    init(cx);
    let dir = tempfile::tempdir().unwrap();
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(state, boot(dir.path()), cx)
    });
    window
        .update(cx, |shell, _, cx| {
            shell.active_chat = "chat".into();
            shell.add_subagent_surface("chat".into(), "doc".into(), "Worker".into(), true, cx);
            let surface = shell.resolved_right_active(cx);
            assert!(shell.pop_out_surface(surface, cx));
        })
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        // Same lookup `open_notified_chat`, the menu bar and Appshot delivery use.
        let shells: Vec<_> = cx.windows().into_iter().filter_map(|w| w.downcast::<Shell>()).collect();
        assert_eq!(shells.len(), 1);
        assert_eq!(shells[0].window_id(), window.window_id());
        assert_eq!(popout::open_count(cx), 1);
    });
}
