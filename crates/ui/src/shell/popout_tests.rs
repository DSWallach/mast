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
    shell.read_with(cx, |shell, _| assert!(shell.right_tab_menu.is_open(), "right-click opens the menu"));
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
            assert_eq!(popout.surface_entity_id(), editor, "the pop-out holds the very same editor");
            assert!(!popout.has_unsaved_changes(cx));
            assert!(popout.prepare_close(cx), "a clean file closes without a prompt");
        })
        .unwrap();
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
    cx.update(|cx| assert!(popout::prepare_exit(false, cx)));
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


fn popout_transcript(
    popout: gpui::WindowHandle<PopoutWindow>,
    cx: &mut TestAppContext,
) -> Entity<crate::transcript::Transcript> {
    popout
        .read_with(cx, |popout, _| match &popout.surface {
            PopoutSurface::Subagent(sub) => sub.transcript.clone(),
            _ => unreachable!(),
        })
        .unwrap()
}

fn dirty_popout(
    state: Entity<AppState>,
    cx: &mut TestAppContext,
) -> gpui::WindowHandle<PopoutWindow> {
    cx.update(|cx| {
        let file = cx.new(|cx| {
            let mut file = crate::files::FilesSurface::new(state.clone(), "test".into(), false, 1000, 13.0, false, false, cx);
            file.seed_pending_exit_test_document(false);
            file
        });
        popout::open(
            state,
            popout::PopoutSurface::File { surface: file, path: "test.rs".into() },
            "test".into(),
            "test.rs".into(),
            cx,
        )
        .unwrap()
    })
}

fn clean(popout: gpui::WindowHandle<PopoutWindow>, cx: &mut TestAppContext) {
    popout
        .update(cx, |popout, _, cx| {
            let PopoutSurface::File { surface, .. } = &popout.surface else { unreachable!() };
            surface.update(cx, |file, cx| {
                file.test_clear_documents(cx);
                cx.emit(crate::files::FilesEvent::CloseReady);
            });
        })
        .unwrap();
}

use crate::popout::PopoutSurface;

fn shell_window(cx: &mut TestAppContext, dir: &std::path::Path) -> gpui::WindowHandle<Shell> {
    cx.add_window(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Shell::new(state, boot(dir), cx)
    })
}

#[gpui::test]
fn every_exit_path_answers_to_a_dirty_popout(cx: &mut TestAppContext) {
    init(cx);
    let dir = tempfile::tempdir().unwrap();
    let window = shell_window(cx, dir.path());
    let state = window.read_with(cx, |shell, _| shell.state.clone()).unwrap();
    let popout = dirty_popout(state, cx);
    window.update(cx, |_, window, _| window.activate_window()).unwrap();
    cx.update(|cx| cx.dispatch_action(&crate::app_menus::Quit));
    cx.run_until_parked();
    window
        .update(cx, |shell, _, cx| {
            assert!(matches!(shell.pending_exit, Some(PendingExit::Quit)), "quit waits on the pop-out");
            assert!(!shell.all_file_edits_flushed(cx));
            shell.pending_exit = None;
            shell.quit_for_runtime_change(cx);
            assert!(matches!(shell.pending_exit, Some(PendingExit::RuntimeChange)));
            assert!(shell.runtime_change_task.is_none());
            shell.pending_exit = None;
            shell.apply_staged_update(PathBuf::from("must-not-install"), cx);
            assert!(matches!(shell.pending_exit, Some(PendingExit::InstallUpdate(_))));
            assert!(matches!(shell.update_flow, UpdateFlow::Idle), "no install with unsaved edits");
            shell.pending_exit = None;
            running_ui::RELAUNCHES.with(|count| count.set(0));
            shell.relaunch_now(cx);
            assert!(matches!(shell.pending_exit, Some(PendingExit::Relaunch)));
            assert_eq!(running_ui::RELAUNCHES.with(|count| count.get()), 0, "no relaunch with unsaved edits");
        })
        .unwrap();
    // After the save the pending relaunch continues on the next frame.
    clean(popout, cx);
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear()).unwrap();
    cx.run_until_parked();
    window.update(cx, |shell, _, _| assert!(shell.pending_exit.is_none())).unwrap();
    assert_eq!(running_ui::RELAUNCHES.with(|count| count.get()), 1, "the exit resumed after the save");
}

#[gpui::test]
fn cancelling_a_popout_prompt_cancels_the_pending_exit(cx: &mut TestAppContext) {
    init(cx);
    let dir = tempfile::tempdir().unwrap();
    let window = shell_window(cx, dir.path());
    let state = window.read_with(cx, |shell, _| shell.state.clone()).unwrap();
    let popout = dirty_popout(state, cx);
    window.update(cx, |shell, _, cx| assert!(!shell.prepare_quit(cx))).unwrap();
    window.update(cx, |shell, _, _| assert!(shell.pending_exit.is_some())).unwrap();
    popout
        .update(cx, |popout, _, cx| {
            let PopoutSurface::File { surface, .. } = &popout.surface else { unreachable!() };
            surface.update(cx, |_, cx| cx.emit(crate::files::FilesEvent::CloseCancelled));
        })
        .unwrap();
    cx.run_until_parked();
    window.update(cx, |shell, _, _| assert!(shell.pending_exit.is_none())).unwrap();
}

#[gpui::test]
fn quit_with_only_popouts_left_is_held_by_the_dirty_popout(cx: &mut TestAppContext) {
    init(cx);
    let state = cx.update(|cx| cx.new(|_| AppState::new()));
    let popout = dirty_popout(state, cx);
    cx.update(|cx| {
        assert!(!crate::has_main_window(cx));
        crate::app_menus::request_quit(cx);
    });
    cx.run_until_parked();
    popout.read_with(cx, |popout, _| assert!(popout.quit_pending())).unwrap();
}

#[gpui::test]
fn command_w_and_command_s_work_inside_a_popout(cx: &mut TestAppContext) {
    init(cx);
    let state = cx.update(|cx| cx.new(|_| AppState::new()));
    let popout = dirty_popout(state, cx);
    popout.update(cx, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    cx.update_window(popout.into(), |_, window, cx| window.draw(cx).clear()).unwrap();
    cx.update_window(popout.into(), |_, window, cx| window.dispatch_action(SaveFile.boxed_clone(), cx)).unwrap();
    popout.read_with(cx, |popout, _| assert_eq!(popout.save_requests(), 1, "⌘S reaches the pop-out")).unwrap();
    // ⌘W on a dirty file raises the prompt and keeps the window.
    cx.update(|cx| cx.dispatch_action(&crate::app_menus::CloseWindow));
    cx.run_until_parked();
    popout.read_with(cx, |popout, _| assert!(popout.is_closing())).unwrap();
    cx.update(|cx| assert_eq!(popout::open_count(cx), 1));
    // Once the prompt resolves the window closes.
    clean(popout, cx);
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(popout::open_count(cx), 0));
}

#[gpui::test]
fn a_path_open_in_a_popout_is_focused_not_reopened(cx: &mut TestAppContext) {
    init(cx);
    let dir = tempfile::tempdir().unwrap();
    let window = shell_window(cx, dir.path());
    window
        .update(cx, |shell, window, cx| {
            shell.active_chat = "chat".into();
            shell.add_file_surface("src/main.rs".into(), window, cx);
            let id = *shell.file_surface_keys.get(&("chat".to_string(), "src/main.rs".to_string())).unwrap();
            assert!(shell.pop_out_surface(RightSurface::File(id), cx));
            shell.add_file_surface("src/main.rs".into(), window, cx);
            assert!(shell.file_surfaces.is_empty(), "no second editor over the same file");
            assert!(shell.right_surface_rows(cx).is_empty());
            // Another chat's file of the same name is a different file.
            shell.active_chat = "other".into();
            shell.add_file_surface("src/main.rs".into(), window, cx);
            assert_eq!(shell.file_surfaces.len(), 1);
        })
        .unwrap();
    cx.update(|cx| assert_eq!(popout::open_count(cx), 1));
}

#[gpui::test]
fn a_closed_main_window_comes_back_for_banners_and_popout_navigation(cx: &mut TestAppContext) {
    init(cx);
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| crate::settings::init(Default::default(), dir.path(), cx));
    let window = shell_window(cx, dir.path());
    let state = window.read_with(cx, |shell, _| shell.state.clone()).unwrap();
    window
        .update(cx, |shell, _, cx| {
            shell.active_chat = "chat".into();
            shell.add_subagent_surface("chat".into(), "doc".into(), "Worker".into(), true, cx);
            let surface = shell.resolved_right_active(cx);
            assert!(shell.pop_out_surface(surface, cx));
        })
        .unwrap();
    cx.update(|cx| cx.set_global(crate::ReopenState { state: state.clone(), boot: boot(dir.path()) }));
    window.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(popout::open_count(cx), 1);
        assert!(!crate::has_main_window(cx), "only the pop-out is left");
        // A banner click rebuilds the main window.
        crate::open_notified_chat(Some("chat".into()), &state, cx);
        assert!(crate::has_main_window(cx));
        assert_eq!(popout::open_count(cx), 1);
    });
    // Closing it again, a pop-out's own navigation rebuilds it too.
    let main = cx.update(|cx| cx.windows().into_iter().find_map(|w| w.downcast::<Shell>()).unwrap());
    main.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(!crate::has_main_window(cx)));
    let popout = cx.update(|cx| cx.windows().into_iter().find_map(|w| w.downcast::<PopoutWindow>()).unwrap());
    let transcript = popout_transcript(popout, cx);
    transcript.update(cx, |_, cx| cx.emit(crate::transcript::TranscriptEvent::OpenChat("chat".into())));
    cx.run_until_parked();
    cx.update(|cx| assert!(crate::has_main_window(cx), "navigation is not silently dropped"));
}

#[gpui::test]
fn forwarded_subagent_opens_switch_the_main_window_to_the_owner_chat(cx: &mut TestAppContext) {
    init(cx);
    let dir = tempfile::tempdir().unwrap();
    let window = shell_window(cx, dir.path());
    let state = window.read_with(cx, |shell, _| shell.state.clone()).unwrap();
    window
        .update(cx, |shell, _, cx| {
            shell.active_chat = "parent".into();
            shell.add_subagent_surface("parent".into(), "doc".into(), "Worker".into(), true, cx);
            let surface = shell.resolved_right_active(cx);
            assert!(shell.pop_out_surface(surface, cx));
        })
        .unwrap();
    let popout = cx.update(|cx| cx.windows().into_iter().find_map(|w| w.downcast::<PopoutWindow>()).unwrap());
    // The main window is on another chat when the pop-out forwards.
    window.update(cx, |shell, _, _| shell.active_chat = "elsewhere".into()).unwrap();
    let transcript = popout_transcript(popout, cx);
    transcript.update(cx, |_, cx| {
        cx.emit(crate::transcript::TranscriptEvent::OpenSubagent {
            chat_id: "parent".into(),
            doc_id: "nested".into(),
            title: "Nested".into(),
            frozen: true,
        })
    });
    cx.run_until_parked();
    state.read_with(cx, |state, _| assert_eq!(state.selected_chat.as_deref(), Some("parent")));
    window.read_with(cx, |shell, _| assert_eq!(shell.subagent_tabs.len(), 1, "the nested subagent opened in the main window")).unwrap();
}

#[gpui::test]
fn the_same_subagent_in_both_windows_is_reference_counted(cx: &mut TestAppContext) {
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
    let watch_count = |window: &gpui::WindowHandle<Shell>, cx: &mut TestAppContext| {
        window.read_with(cx, |shell, cx| shell.state.read(cx).subagent_watch_count()).unwrap()
    };
    window
        .update(cx, |shell, _, cx| {
            shell.active_chat = "parent".into();
            shell.add_subagent_surface("parent".into(), "doc".into(), "Worker".into(), false, cx);
            let surface = shell.resolved_right_active(cx);
            assert!(shell.pop_out_surface(surface, cx));
            // The chip is clicked again in the main window: a second holder.
            shell.add_subagent_surface("parent".into(), "doc".into(), "Worker".into(), false, cx);
            assert_eq!(shell.subagent_tabs.len(), 1);
        })
        .unwrap();
    cx.run_until_parked();
    let calls = drain(&mut requests);
    assert_eq!(calls.iter().filter(|m| *m == mast_rpc::methods::WATCH_DOC_MESSAGES).count(), 1, "one watch for two holders: {calls:?}");
    // Closing the main window's tab keeps the pop-out's feed alive.
    window
        .update(cx, |shell, window, cx| {
            let surface = shell.resolved_right_active(cx);
            shell.close_right_surface(surface, window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(watch_count(&window, cx), 1, "the pop-out still has its feed");
    // Closing the last holder releases it.
    let popout = cx.update(|cx| cx.windows().into_iter().find_map(|w| w.downcast::<PopoutWindow>()).unwrap());
    popout.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();
    assert_eq!(watch_count(&window, cx), 0);
}
