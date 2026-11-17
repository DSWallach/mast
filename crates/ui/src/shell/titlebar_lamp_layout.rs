use super::*;
use gpui::{AppContext, TestAppContext, VisualTestContext};

struct BarHost {
    shell: Entity<Shell>,
    width: f32,
    _data_dir: tempfile::TempDir,
}

impl Render for BarHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = self.width;
        self.shell.update(cx, |shell, cx| {
            div()
                .relative()
                .w(px(width))
                .h(px(Theme::TITLEBAR_HEIGHT))
                .child(shell.render_title_bar(px(800.0), cx))
                .child(shell.render_titlebar_cluster(cx))
        })
    }
}

fn setup(
    cx: &mut TestAppContext,
    route: Route,
    width: f32,
    sidebar_collapsed: bool,
) -> &mut VisualTestContext {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let (_host, cx) = cx.add_window_view(|_, cx| {
        let shell = cx.new(|cx| {
            let state = cx.new(|_| AppState::new());
            let mut shell = Shell::new(
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
            );
            shell.viewport_width = width;
            shell.settings.sidebar_collapsed = sidebar_collapsed;
            if matches!(route, Route::Chat) {
                shell.active_chat = "long".into();
                shell.state.update(cx, |state, _| {
                    state.local_device_id = Some("here".into());
                    state.selected_chat = Some("long".into());
                    state.chats.push(mast_proto::Chat {
                        id: "long".into(),
                        device_id: "here".into(),
                        title: Some("A very long chat title ".repeat(8)),
                        archived: false,
                        cwd: None,
                        branch: None,
                        checkout_id: None,
                        source_context: None,
                        config: None,
                        last_message_preview: None,
                        last_message_at: None,
                        created_at: Utc::now(),
                        harness_session_id: None,
                        harness_session_cwd: None,
                        actual_model: None,
                        scheduled_task: None,
                        scheduled_task_info: None,
                        parent_chat_id: None,
                        side_of: None,
                        side_absorbed_through: None,
                        space_id: None,
                        last_seen_at: None,
                        room_gen: None,
                        linked_pr_url: None,
                        linked_pr_source: None,
                        linked_ticket_id: None,
                        linked_ticket_source: None,
                    });
                });
            }
            shell.route = route;
            shell
        });
        BarHost {
            shell,
            width,
            _data_dir: dir,
        }
    });
    cx.update(|window, cx| window.draw(cx).clear());
    cx
}

#[gpui::test]
fn lamp_never_overlaps_the_title(cx: &mut TestAppContext) {
    // Default window width and the window minimum (900, `window_min_size`), sidebar collapsed (cluster over the title) and open.
    for (name, route) in [
        ("overview", Route::Overview),
        ("chat", Route::Chat),
        ("pull requests", Route::PrReviewChats),
    ] {
        for width in [1280.0_f32, 900.0] {
            for collapsed in [true, false] {
                let cx = setup(cx, route, width, collapsed);
                let lamp = cx.debug_bounds("running-lamp").expect("lamp rendered");
                let title = cx.debug_bounds("titlebar-title").expect("title rendered");
                assert!(
                    !lamp.intersects(&title) && title.left() >= lamp.right(),
                    "{name} @ {width}px collapsed={collapsed}: lamp {lamp:?} vs title {title:?}"
                );
            }
        }
    }
}

#[gpui::test]
fn lamp_position_is_stable_across_routes(cx: &mut TestAppContext) {
    let overview = setup(cx, Route::Overview, 1280.0, true)
        .debug_bounds("running-lamp")
        .unwrap();
    let chat = setup(cx, Route::Chat, 1280.0, true)
        .debug_bounds("running-lamp")
        .unwrap();
    assert_eq!(overview.origin, chat.origin);
}
