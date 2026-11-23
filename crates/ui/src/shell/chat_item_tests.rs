//! Headless tests for the chat header, its hover card and the Item tab. Every
//! store is filled by hand; nothing reaches GitHub, Linear or an engine.
use std::time::Duration;

use chrono::TimeZone;
use gpui::{Context, IntoElement, Render, TestAppContext, VisualTestContext, Window, div, px};
use mast_proto::{
    ChatAccountPin, HarnessId, ManagedAccount, ManagedAccountsSnapshot, WorkOutlineSnapshot,
};

use super::work_outline_ui::WorkOutlineModal;
use super::*;
use crate::work_outline::required_actions::fixture::{R_UI, request};
use crate::work_outline::store::{fixture_ids::*, fixture_snapshot};

struct Host {
    shell: Entity<Shell>,
    tab: bool,
    _dir: tempfile::TempDir,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::default();
        let tab = self.tab;
        let (header, body, admin_dialog, hand_off_dialog) = self.shell.update(cx, |shell, cx| {
            let header = shell.render_chat_header_metadata(&theme, cx);
            let body = tab.then(|| match shell.resolved_right_active(cx) {
                RightSurface::Team => shell.render_team_tab(cx),
                RightSurface::Settings => shell.render_settings_tab(cx),
                _ => shell.render_item_tab(cx),
            });
            let viewport = gpui::size(px(900.0), px(600.0));
            let admin = shell.render_admin_dialog(viewport, cx);
            let hand_off = if matches!(shell.work_outline_modal, Some(WorkOutlineModal::HandOff(_))) {
                shell.render_work_outline_modal(viewport, window, cx)
            } else {
                None
            };
            (header, body, admin, hand_off)
        });
        div()
            .relative()
            .w(px(900.0))
            .h(px(600.0))
            .children(header)
            .children(admin_dialog)
            .children(body.map(|body| div().w(px(420.0)).h(px(500.0)).child(body)))
            .children(hand_off_dialog)
    }
}

fn chat(id: &str) -> mast_proto::Chat {
    let mut chat: mast_proto::Chat = serde_json::from_value(serde_json::json!({
        "id": id,
        "deviceId": "device",
        "title": format!("Title of {id}"),
        "archived": false,
        "cwd": format!("/work/{id}"),
        "branch": "feature/header",
        "checkoutId": null,
        "config": null,
        "lastMessagePreview": null,
        "lastMessageAt": null,
        "createdAt": chrono::Utc.timestamp_opt(5, 0).unwrap(),
        "spaceId": "s1",
    }))
    .unwrap();
    chat.space_id = Some("s1".into());
    chat
}

/// `edit` runs on the Space's snapshot before the Shell sees it.
pub(super) fn mount<'a>(
    cx: &'a mut TestAppContext,
    chat_id: &'static str,
    tab: bool,
    edit: impl FnOnce(&mut WorkOutlineSnapshot) + 'static,
) -> (Entity<Shell>, &'a mut VisualTestContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        crate::shell::apply_keymap(
            cx,
            &crate::settings::KeymapConfig::default(),
            Default::default(),
        );
    });
    let (host, vcx) = cx.add_window_view(move |_, cx| {
        let shell = cx.new(|cx| {
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| {
                let mut snapshot = fixture_snapshot("s1");
                snapshot.external_references =
                    vec![crate::github::fixture::reference("r1", "s1", API, "app", 7)];
                edit(&mut snapshot);
                state.set_work_outline_for_test(snapshot);
                state.chats = [CHAT_API, CHAT_LOOSE_A, CHAT_UI]
                    .iter()
                    .map(|id| chat(id))
                    .collect();
                state.selected_chat = Some(chat_id.into());
                state.set_managed_accounts(
                    Some("device"),
                    ManagedAccountsSnapshot {
                        accounts: vec![ManagedAccount {
                            id: "a1".into(),
                            harness: HarnessId::ClaudeCode,
                            label: "Work".into(),
                            email: None,
                            signed_in: false,
                            plan_label: None,
                            usage_windows: Vec::new(),
                            usage_note: None,
                        }],
                        project_defaults: Vec::new(),
                        chat_pins: vec![ChatAccountPin {
                            chat_id: CHAT_API.into(),
                            harness: HarnessId::ClaudeCode,
                            account_id: Some("a1".into()),
                        }],
                        saved: Vec::new(),
                    },
                );
            });
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
            shell.active_chat = chat_id.into();
            // The header's link fetch would reach for an engine; the cache
            // entry says "nothing linked" and is fresh for the whole test.
            shell.chat_header_links.insert(
                chat_id.into(),
                ChatHeaderLinkCache {
                    checkout: None,
                    status: None,
                    fetched_at: Instant::now(),
                },
            );
            shell
        });
        Host {
            shell,
            tab,
            _dir: dir,
        }
    });
    let shell = host.read_with(vcx, |host, _| host.shell.clone());
    draw(vcx);
    (shell, vcx)
}

pub(super) fn draw(vcx: &mut VisualTestContext) {
    vcx.update(|window, cx| window.draw(cx).clear());
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
}

/// Expand Item tab accordion rows and repaint.
fn expand(shell: &Entity<Shell>, vcx: &mut VisualTestContext, rows: &[&'static str]) {
    shell.update(vcx, |shell, cx| {
        shell.item_tab_open.extend(rows.iter().copied());
        cx.notify();
    });
    draw(vcx);
}

pub(super) fn has(vcx: &mut VisualTestContext, selector: &'static str) -> bool {
    vcx.debug_bounds(selector).is_some()
}

/// Drift on the chat, one agent ask, and one plan question on its item.
fn needs_attention(snapshot: &mut WorkOutlineSnapshot) {
    snapshot
        .chat_instruction_states
        .push(mast_proto::ChatInstructionState {
            chat_id: CHAT_API.into(),
            selected_revision_id: Some("rev-1".into()),
            selected_revision_hash: Some("aaaaaaaaaaaa".into()),
            selected_revision_integrity: None,
            current_effective_hash: Some("bbbbbbbbbbbb".into()),
            drift: true,
            delivered_revision_id: Some("rev-1".into()),
            delivered_revision_hash: Some("aaaaaaaaaaaa".into()),
        });
    snapshot.user_action_requests.push(request(
        R_UI,
        "s1",
        API,
        CHAT_API,
        "Pick the auth provider",
    ));
    let mut question = request("req-q", "s1", API, CHAT_API, "Q-2: which tenant?");
    question.dedupe_key = Some("artq:art1:Q-2".into());
    snapshot.user_action_requests.push(question);
}

#[gpui::test]
fn the_header_shows_breadcrumb_title_chips_and_badges(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, false, needs_attention);
    for selector in [
        "chat-header",
        "chat-header-breadcrumb",
        "chat-header-crumb-0",
        "chat-header-title",
        "chat-header-chip-model",
        "chat-header-chip-account",
        "chat-header-chip-branch",
        "chat-header-chip-pr-0",
        "chat-header-badge-attention",
        "chat-header-badge-questions",
    ] {
        assert!(has(vcx, selector), "{selector} should be on the header");
    }
}

#[gpui::test]
fn a_chat_with_nothing_to_flag_has_no_badges_and_a_chat_without_an_item_no_breadcrumb(
    cx: &mut TestAppContext,
) {
    let (_, vcx) = mount(cx, CHAT_API, false, |_| {});
    assert!(has(vcx, "chat-header-breadcrumb"));
    assert!(!has(vcx, "chat-header-badge-attention"));
    assert!(!has(vcx, "chat-header-badge-questions"));
    let (_, vcx) = mount(cx, CHAT_LOOSE_A, false, |_| {});
    assert!(has(vcx, "chat-header-title"));
    assert!(has(vcx, "chat-header-chip-model"));
    assert!(!has(vcx, "chat-header-breadcrumb"));
    assert!(!has(vcx, "chat-header-chip-pr-0"));
    assert!(!has(vcx, "chat-header-chip-account"));
}

#[gpui::test]
fn the_old_strip_drift_and_proposal_lines_are_gone_from_the_header(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |snapshot| {
        needs_attention(snapshot);
        snapshot.instruction_proposals.push(
            crate::work_outline::required_actions::fixture::proposal(
                "p1",
                "s1",
                Some(API),
                CHAT_API,
                "Prefer small PRs",
            ),
        );
    });
    for selector in [
        "work-context-strip",
        "chat-header-instruction-status",
        "chat-header-instruction-drift",
        "chat-header-proposals",
    ] {
        assert!(!has(vcx, selector), "{selector} must not be in the header");
    }
    // One row of title + one row of chips; nothing grows with the item's state.
    shell.read_with(vcx, |shell, _| {
        assert_eq!(
            shell.chat_header_extra_clearance,
            super::chat_item_ui::CHAT_HEADER_STACKED_CLEARANCE
        )
    });
}

#[gpui::test]
fn the_item_tab_shows_the_item_sheet(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        needs_attention(snapshot);
        snapshot.instruction_proposals.push(
            crate::work_outline::required_actions::fixture::proposal(
                "p1",
                "s1",
                Some(API),
                CHAT_API,
                "Prefer small PRs",
            ),
        );
    });
    expand(&shell, vcx, &["instructions", "links"]);
    for selector in [
        "chat-item-tab",
        "chat-item-path",
        "chat-item-path-segment-0",
        "chat-item-instructions",
        "chat-item-instructions-edit",
        // The drift line (Compare / Apply) and the proposal line live here now.
        "chat-header-instruction-status",
        "chat-header-instructions-compare",
        "chat-header-proposals",
        "chat-item-runs-in",
        "chat-item-model",
        "chat-item-account",
        "chat-item-links",
        "chat-item-pr-0",
        "chat-item-needs-you",
        "required-actions-row-req-ui",
    ] {
        assert!(has(vcx, selector), "{selector} should be in the Item tab");
    }
    assert!(!has(vcx, "chat-item-work-item"));
    assert!(!has(vcx, "chat-item-add-to-outline"));
}

#[gpui::test]
fn a_chat_without_an_item_gets_only_runs_in_model_account_and_add_to_outline(
    cx: &mut TestAppContext,
) {
    let (_, vcx) = mount(cx, CHAT_LOOSE_A, true, |_| {});
    for selector in [
        "chat-item-tab",
        "chat-item-runs-in",
        "chat-item-model",
        "chat-item-account",
        "chat-item-add-to-outline",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
    for selector in [
        "chat-item-path",
        "chat-item-instructions",
        "chat-item-links",
        "chat-item-needs-you",
        "chat-item-plan",
        "chat-item-children",
    ] {
        assert!(!has(vcx, selector), "{selector} needs an item");
    }
}

#[gpui::test]
fn the_badges_open_the_item_tab_and_the_pane_opens_with_it(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, needs_attention);
    shell.read_with(vcx, |shell, cx| assert!(!shell.right_pane_open(cx)));
    let at = vcx
        .debug_bounds("chat-header-badge-attention")
        .unwrap()
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
    shell.read_with(vcx, |shell, cx| {
        assert!(shell.right_pane_open(cx));
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Item);
        // The Item tab is not an ordinary, closable tab.
        assert!(shell.right_surface_rows(cx).is_empty());
        assert_eq!(shell.closable_right_surface(cx), None);
    });
}

#[gpui::test]
fn the_item_tab_stays_selected_over_other_tabs_until_one_is_picked(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
    shell.update(vcx, |shell, cx| {
        shell.open_item_tab(cx);
        shell
            .right_tabs
            .insert(shell.panel_key(cx), vec![RightSurface::Subagent(7)]);
        cx.notify();
    });
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Item)
    });
    shell.update(vcx, |shell, cx| {
        shell.set_right_active(RightSurface::Picker, cx)
    });
    shell.read_with(vcx, |shell, cx| {
        assert_ne!(shell.resolved_right_active(cx), RightSurface::Item)
    });
}

#[gpui::test]
fn toggle_item_tab_opens_and_closes_item_tab(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
    shell.update(vcx, |shell, cx| {
        assert!(!shell.item_tab_open(cx));
        shell.toggle_item_tab(cx);
        assert!(shell.item_tab_open(cx));
        assert!(shell.right_pane_open(cx));
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Item);

        shell.toggle_item_tab(cx);
        assert!(!shell.item_tab_open(cx));
        assert!(!shell.right_pane_open(cx));
    });
}

// ----------------------------------------------------------------------
// Hover card
// ----------------------------------------------------------------------

fn hover_header(vcx: &mut VisualTestContext) {
    let at = vcx.debug_bounds("chat-header-title").unwrap().center();
    vcx.simulate_mouse_move(at, None, gpui::Modifiers::default());
    draw(vcx);
}

fn leave(vcx: &mut VisualTestContext) {
    vcx.simulate_mouse_move(
        gpui::point(px(890.0), px(590.0)),
        None,
        gpui::Modifiers::default(),
    );
    draw(vcx);
}

fn wait(vcx: &mut VisualTestContext, ms: u64) {
    vcx.executor().advance_clock(Duration::from_millis(ms));
    draw(vcx);
}

#[gpui::test]
fn the_card_shows_after_the_hover_delay_and_hides_on_mouse_out(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, false, needs_attention);
    assert!(!has(vcx, "chat-item-card"));
    hover_header(vcx);
    wait(vcx, 200);
    assert!(!has(vcx, "chat-item-card"), "not before ~400 ms");
    wait(vcx, 300);
    assert!(has(vcx, "chat-item-card"));
    for selector in [
        "chat-item-card-instructions",
        "chat-item-card-links",
        "chat-item-card-needs-you",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
    // The card is the condensed form: the header chips already carry the
    // model, account and branch, and the tab holds the rest.
    for selector in [
        "chat-item-card-runs-in",
        "chat-item-card-model",
        "chat-item-card-account",
        "chat-item-card-identifiers",
        "chat-item-card-path",
    ] {
        assert!(!has(vcx, selector), "{selector}");
    }
    assert!(!has(vcx, "chat-item-card-children"));
    leave(vcx);
    wait(vcx, 400);
    assert!(!has(vcx, "chat-item-card"));
}

#[gpui::test]
fn a_quick_pass_over_the_header_shows_nothing(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, false, needs_attention);
    hover_header(vcx);
    wait(vcx, 100);
    leave(vcx);
    wait(vcx, 600);
    assert!(!has(vcx, "chat-item-card"));
}

#[gpui::test]
fn the_shortcut_pins_the_card_open_and_releases_it(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, needs_attention);
    shell.update(vcx, |shell, cx| shell.toggle_chat_item_card_pin(cx));
    draw(vcx);
    assert!(has(vcx, "chat-item-card"), "pinned without any hover");
    hover_header(vcx);
    leave(vcx);
    wait(vcx, 600);
    assert!(has(vcx, "chat-item-card"), "mouse-out does not hide a pin");
    shell.update(vcx, |shell, cx| shell.toggle_chat_item_card_pin(cx));
    draw(vcx);
    assert!(!has(vcx, "chat-item-card"), "released, pointer elsewhere");
}

#[gpui::test]
fn the_card_never_takes_the_keyboard_from_the_composer(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, needs_attention);
    let focus = shell.read_with(vcx, |shell, cx| shell.composer.read(cx).focus_handle(cx));
    vcx.update(|window, cx| window.focus(&focus, cx));
    let focused = |vcx: &mut VisualTestContext| vcx.update(|window, _| focus.is_focused(window));
    assert!(focused(vcx));
    hover_header(vcx);
    wait(vcx, 500);
    assert!(has(vcx, "chat-item-card"));
    assert!(focused(vcx), "showing the card keeps focus");
    let at = vcx
        .debug_bounds("chat-item-card-instructions")
        .unwrap()
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
    assert!(focused(vcx), "clicking inside the card keeps focus");
    shell.update(vcx, |shell, cx| shell.toggle_chat_item_card_pin(cx));
    draw(vcx);
    assert!(focused(vcx), "pinning keeps focus");
    let at = vcx.debug_bounds("chat-header-title").unwrap().center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
    assert!(focused(vcx), "clicking the header keeps focus");
}

#[gpui::test]
fn changing_chat_drops_the_card_and_its_pin(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, needs_attention);
    shell.update(vcx, |shell, cx| shell.toggle_chat_item_card_pin(cx));
    draw(vcx);
    assert!(has(vcx, "chat-item-card"));
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.selected_chat = Some(CHAT_UI.into());
        });
        shell.active_chat = CHAT_UI.into();
        shell.chat_header_links.insert(
            CHAT_UI.into(),
            ChatHeaderLinkCache {
                checkout: None,
                status: None,
                fetched_at: Instant::now(),
            },
        );
        cx.notify();
    });
    draw(vcx);
    assert!(!has(vcx, "chat-item-card"));
}

#[gpui::test]
fn the_keys_are_registered_and_listed_in_the_reference(_cx: &mut TestAppContext) {
    use crate::work_outline::hints::shortcut_reference_on;
    let keymap = crate::settings::KeymapConfig::default();
    let groups = shortcut_reference_on(true, &keymap);
    let group = groups
        .iter()
        .find(|group| group.title == "Chat header")
        .expect("the reference lists the chat header keys");
    let keys: Vec<_> = group
        .rows
        .iter()
        .map(|row| (row.label, row.keys.clone().unwrap_or_default()))
        .collect();
    assert!(
        keys.contains(&("Open the Item tab", "\u{2325}\u{2318};".to_owned())),
        "{keys:?}"
    );
    assert!(
        keys.contains(&(
            "Pin or release the context card",
            "\u{2325}\u{2318}I".to_owned()
        )),
        "{keys:?}"
    );
    // Both parse and neither shadows a customizable default.
    for combo in [OPEN_ITEM_TAB_COMBO, TOGGLE_ITEM_CARD_COMBO] {
        assert!(gpui::Keystroke::parse(&crate::settings::platform_combo(combo)).is_ok());
        assert!(
            crate::settings::ShortcutId::ALL
                .into_iter()
                .all(|id| id.default_combo() != combo),
            "{combo} clashes"
        );
    }
}

#[gpui::test]
fn the_item_tab_lists_identifiers_with_copy_for_chats_with_and_without_an_item(
    cx: &mut TestAppContext,
) {
    for chat_id in [CHAT_API, CHAT_LOOSE_A] {
        let (shell, vcx) = mount(cx, chat_id, true, |_| {});
        shell.update(vcx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                let chat = state.chats.iter_mut().find(|c| c.id == chat_id).unwrap();
                chat.harness_session_id = Some("sess-123".into());
            })
        });
        expand(&shell, vcx, &["identifiers"]);
        for selector in [
            "chat-item-identifiers",
            "chat-item-id-chat",
            "chat-item-id-session",
        ] {
            assert!(has(vcx, selector), "{selector} for {chat_id}");
        }
        let at = vcx.debug_bounds("chat-item-id-chat").unwrap().center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        draw(vcx);
        assert_eq!(
            vcx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some(chat_id)
        );
        let at = vcx.debug_bounds("chat-item-id-session").unwrap().center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        draw(vcx);
        assert_eq!(
            vcx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("sess-123")
        );
        shell.read_with(vcx, |shell, _| {
            assert_eq!(shell.copied_chat_identity.as_deref(), Some("session"))
        });
    }
}

#[gpui::test]
fn a_chat_without_a_session_id_lists_no_session_row_and_the_header_has_no_identifiers(
    cx: &mut TestAppContext,
) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    expand(&shell, vcx, &["identifiers"]);
    assert!(has(vcx, "chat-item-id-chat"));
    assert!(!has(vcx, "chat-item-id-session"));
    let (_, vcx) = mount(cx, CHAT_API, false, |_| {});
    assert!(!has(vcx, "chat-item-identifiers"));
}

/// Web and PR links from every surface go through one Shell entry point
/// (`open_web_reference`). Only the model is asserted: tabs, URLs, the
/// route and the external opener.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod web_references {
    use super::*;
    use crate::shell::Route;
    use gpui::Modifiers;

    const PR_7: &str = "https://github.com/acme/app/pull/7";
    const PR_99: &str = "https://github.com/acme/app/pull/99";

    fn open(shell: &Entity<Shell>, vcx: &mut VisualTestContext, url: &str, external: bool) {
        vcx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.open_web_reference(url, external, window, cx)
            })
        });
        vcx.run_until_parked();
    }

    fn select(shell: &Entity<Shell>, vcx: &mut VisualTestContext, chat: &str, route: Route) {
        shell.update(vcx, |shell, cx| {
            shell.route = route;
            let state = shell.state.clone();
            state.update(cx, |state, cx| state.select_chat(Some(chat.into()), cx));
        });
        vcx.run_until_parked();
    }

    fn tabs(shell: &Entity<Shell>, vcx: &mut VisualTestContext) -> Vec<String> {
        shell.read_with(vcx, |shell, cx| {
            let mut tabs: Vec<_> = shell.browsers.iter().collect();
            tabs.sort_by_key(|(id, _)| **id);
            tabs.into_iter()
                .map(|(_, browser)| browser.read(cx).page.url.clone().unwrap_or_default())
                .collect()
        })
    }

    fn set_open_in_mast(vcx: &mut VisualTestContext, on: bool) {
        vcx.update(|_, cx| {
            crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
                settings.open_web_links_in_mast = on
            })
        });
    }

    fn click(vcx: &mut VisualTestContext, selector: &'static str, modifiers: Modifiers) {
        let at = vcx.debug_bounds(selector).unwrap().center();
        vcx.simulate_click(at, modifiers);
        draw(vcx);
    }

    #[gpui::test]
    fn the_header_pr_chip_opens_a_side_tab_and_focuses_it_the_second_time(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        select(&shell, vcx, CHAT_API, Route::Chat);
        click(vcx, "chat-header-chip-pr-0", Modifiers::default());
        assert_eq!(tabs(&shell, vcx), [PR_7]);
        assert!(vcx.opened_url().is_none(), "not the external browser");
        shell.read_with(vcx, |shell, cx| {
            assert!(shell.right_pane_open(cx));
            let first = *shell.browsers.keys().min().unwrap();
            assert_eq!(
                shell.resolved_right_active(cx),
                RightSurface::Browser(first)
            );
        });
        // Another page opens a second tab; the chip again focuses the first.
        open(&shell, vcx, "https://example.com/docs", false);
        assert_eq!(tabs(&shell, vcx).len(), 2);
        click(vcx, "chat-header-chip-pr-0", Modifiers::default());
        assert_eq!(tabs(&shell, vcx), [PR_7, "https://example.com/docs"]);
        shell.read_with(vcx, |shell, cx| {
            let first = *shell.browsers.keys().min().unwrap();
            assert_eq!(
                shell.resolved_right_active(cx),
                RightSurface::Browser(first)
            );
        });
    }

    #[gpui::test]
    fn the_item_tab_pr_link_follows_the_same_rules(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
        select(&shell, vcx, CHAT_API, Route::Chat);
        super::expand(&shell, vcx, &["links"]);
        click(vcx, "chat-item-pr-0", Modifiers::default());
        assert_eq!(tabs(&shell, vcx), [PR_7]);
        assert!(vcx.opened_url().is_none());
        // Again: focus, no duplicate.
        click(vcx, "chat-item-pr-0", Modifiers::default());
        assert_eq!(tabs(&shell, vcx).len(), 1);
        // ⌘-click goes external.
        click(vcx, "chat-item-pr-0", Modifiers::secondary_key());
        assert_eq!(vcx.opened_url().as_deref(), Some(PR_7));
        assert_eq!(tabs(&shell, vcx).len(), 1);
    }

    #[gpui::test]
    fn command_click_and_the_off_setting_open_the_external_browser(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        select(&shell, vcx, CHAT_API, Route::Chat);
        click(vcx, "chat-header-chip-pr-0", Modifiers::secondary_key());
        assert_eq!(vcx.opened_url().as_deref(), Some(PR_7));
        assert!(tabs(&shell, vcx).is_empty());
        // The explicit external entry (context menu).
        open(&shell, vcx, "https://example.com/menu", true);
        assert_eq!(
            vcx.opened_url().as_deref(),
            Some("https://example.com/menu")
        );
        // With the setting off nothing opens in Mast, chip included.
        set_open_in_mast(vcx, false);
        click(vcx, "chat-header-chip-pr-0", Modifiers::default());
        assert_eq!(vcx.opened_url().as_deref(), Some(PR_7));
        open(&shell, vcx, "https://example.com/off", false);
        assert_eq!(vcx.opened_url().as_deref(), Some("https://example.com/off"));
        assert!(tabs(&shell, vcx).is_empty());
    }

    #[gpui::test]
    fn a_route_without_a_side_pane_goes_to_the_review_chat(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        shell.update(vcx, |shell, cx| {
            shell.route = Route::PrReviewChats;
            cx.notify();
        });
        // PR #7 is reviewed in CHAT_API (assigned to its review item).
        open(&shell, vcx, PR_7, false);
        shell.read_with(vcx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(shell.active_chat, CHAT_API);
            assert!(
                shell.right_pane_open(cx),
                "the browser sits beside the chat"
            );
        });
        assert_eq!(tabs(&shell, vcx), [PR_7]);
        assert!(vcx.opened_url().is_none());
    }

    #[gpui::test]
    fn a_pr_without_a_review_chat_uses_the_selected_chat_else_goes_external(
        cx: &mut TestAppContext,
    ) {
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        // Nothing to host a tab: the external browser.
        shell.update(vcx, |shell, cx| {
            shell.route = Route::Overview;
            shell
                .state
                .update(cx, |state, cx| state.select_chat(None, cx));
        });
        vcx.run_until_parked();
        open(&shell, vcx, PR_99, false);
        assert_eq!(vcx.opened_url().as_deref(), Some(PR_99));
        assert!(tabs(&shell, vcx).is_empty());
        assert!(matches!(
            shell.read_with(vcx, |s, _| s.route),
            Route::Overview
        ));
        // The Overview previews CHAT_UI: the tab opens beside it.
        select(&shell, vcx, CHAT_UI, Route::Overview);
        open(&shell, vcx, PR_99, false);
        shell.read_with(vcx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(shell.active_chat, CHAT_UI);
            assert!(shell.right_pane_open(cx));
        });
        assert_eq!(tabs(&shell, vcx), [PR_99]);
        // A plain web link takes the same route.
        select(&shell, vcx, CHAT_UI, Route::PrReviewChats);
        open(&shell, vcx, "https://example.com/spec", false);
        assert_eq!(tabs(&shell, vcx), [PR_99, "https://example.com/spec"]);
    }

    #[gpui::test]
    fn off_the_chat_route_the_setting_and_command_click_still_win(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        select(&shell, vcx, CHAT_API, Route::PrReviewChats);
        open(&shell, vcx, PR_7, true);
        assert_eq!(vcx.opened_url().as_deref(), Some(PR_7));
        assert!(matches!(
            shell.read_with(vcx, |s, _| s.route),
            Route::PrReviewChats
        ));
        set_open_in_mast(vcx, false);
        open(&shell, vcx, PR_99, false);
        assert_eq!(vcx.opened_url().as_deref(), Some(PR_99));
        assert!(tabs(&shell, vcx).is_empty());
        assert!(matches!(
            shell.read_with(vcx, |s, _| s.route),
            Route::PrReviewChats
        ));
    }

    #[gpui::test]
    fn a_link_that_is_not_web_is_refused(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        select(&shell, vcx, CHAT_API, Route::Chat);
        open(&shell, vcx, "file:///etc/passwd", false);
        assert!(tabs(&shell, vcx).is_empty());
        assert!(vcx.opened_url().is_none());
        shell.read_with(vcx, |shell, _| assert!(shell.sidebar_notice.is_some()));
    }
}

#[gpui::test]
fn header_chips_carry_icons_and_the_title_stacks_under_the_breadcrumb(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, false, |_| {});
    for selector in [
        "chat-header-chip-model-icon",
        "chat-header-chip-account-icon",
        "chat-header-chip-branch-icon",
        "chat-header-chip-pr-0-icon",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
    let crumb = vcx.debug_bounds("chat-header-breadcrumb").unwrap();
    let title = vcx.debug_bounds("chat-header-title").unwrap();
    assert!(
        title.top() >= crumb.bottom(),
        "title sits under the breadcrumb"
    );
}

fn open_settings(shell: &Entity<Shell>, vcx: &mut VisualTestContext) {
    shell.update(vcx, |shell, cx| shell.add_settings_surface(cx));
    draw(vcx);
}

fn api_is_a_project(snapshot: &mut WorkOutlineSnapshot) {
    snapshot.items.iter_mut().find(|item| item.id == API).unwrap().is_project = true;
}

#[gpui::test]
fn a_projects_settings_tab_shows_the_manager_settings_and_hand_off_log(cx: &mut TestAppContext) {
    // No role record: the upgrade case, opened the way a user does.
    let (shell, vcx) = mount(cx, CHAT_API, true, api_is_a_project);
    assert!(!has(vcx, "chat-item-manager"), "the Item tab no longer carries them");
    open_settings(&shell, vcx);
    assert_eq!(shell.read_with(vcx, |s, cx| s.resolved_right_active(cx)), RightSurface::Settings);
    for selector in [
        "chat-item-manager",
        "chat-item-manager-icon",
        "chat-item-manager-read-only-toggle",
        "chat-item-manager-worker-model",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
    // Defaults: read-only On, and no hand-offs yet.
    assert!(has(vcx, "chat-item-manager-read-only:On"));
    assert!(!has(vcx, "chat-item-manager-read-only:Off"));
}

/// Mount a project Manager's Settings tab whose mutations are answered by
/// `reply`, held until the test releases them.
fn manager_settings_with_gate<'a>(
    cx: &'a mut TestAppContext,
    reply: impl Fn(&serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> + Send + Sync + 'static,
    edit: impl FnOnce(&mut WorkOutlineSnapshot) + 'static,
) -> (
    Entity<Shell>,
    std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    crate::work_outline::actions::ReplyGate,
    &'a mut VisualTestContext,
) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        api_is_a_project(snapshot);
        edit(snapshot);
    });
    let calls: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> = Default::default();
    let sink = calls.clone();
    let gate = crate::work_outline::actions::ReplyGate::default();
    let executor = vcx.update(|_, cx| cx.background_executor().clone());
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.work_outline_actions_override = Some(crate::work_outline::WorkOutlineActions::scripted_gated(
                std::sync::Arc::new(move |_, params| {
                    sink.lock().unwrap().push(params.clone());
                    reply(&params)
                }),
                gate.clone(),
                executor,
            ));
        });
    });
    open_settings(&shell, vcx);
    (shell, calls, gate, vcx)
}

#[gpui::test]
fn the_read_only_toggle_shows_its_new_value_before_the_host_replies(cx: &mut TestAppContext) {
    let (_shell, calls, gate, vcx) = manager_settings_with_gate(cx, |_| Ok(serde_json::json!({})), |_| {});
    assert!(has(vcx, "chat-item-manager-read-only:On"), "an unchosen project is read-only");
    click_selector(vcx, "chat-item-manager-read-only-toggle");
    // The reply is still held: no frame, no acknowledgement.
    assert_eq!(gate.held(), 1);
    assert!(has(vcx, "chat-item-manager-read-only:Off"), "drawn at once");
    assert!(!has(vcx, "chat-item-manager-read-only:On"));
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["op"], "setManagerReadOnly");
        assert_eq!(calls[0]["readOnly"], false);
    }
    // Acknowledged, the watch frame not here yet: it must not flip back.
    assert!(gate.release_one());
    vcx.run_until_parked();
    draw(vcx);
    assert!(has(vcx, "chat-item-manager-read-only:Off"), "no flicker between the reply and the frame");
}

#[gpui::test]
fn a_rejected_read_only_change_rolls_back_and_shows_the_error(cx: &mut TestAppContext) {
    let (shell, _calls, gate, vcx) = manager_settings_with_gate(
        cx,
        |_| Err(mast_rpc::RpcError::Failed("work-outline/invalid: Only a project has a Manager".into())),
        |_| {},
    );
    click_selector(vcx, "chat-item-manager-read-only-toggle");
    assert!(has(vcx, "chat-item-manager-read-only:Off"));
    assert!(gate.release_one());
    vcx.run_until_parked();
    draw(vcx);
    assert!(has(vcx, "chat-item-manager-read-only:On"), "the host refused: back to the stored value");
    assert!(shell.read_with(vcx, |shell, _| shell.sidebar_error_text.is_some()), "and the error is shown");
}

#[gpui::test]
fn two_quick_read_only_clicks_reach_the_host_one_at_a_time_and_the_last_click_last(cx: &mut TestAppContext) {
    let (_shell, calls, gate, vcx) = manager_settings_with_gate(cx, |_| Ok(serde_json::json!({})), |_| {});
    click_selector(vcx, "chat-item-manager-read-only-toggle");
    assert!(has(vcx, "chat-item-manager-read-only:Off"));
    // The first mutation is still in flight: the second click toggles what is
    // drawn (back On) and waits instead of racing it to the host.
    click_selector(vcx, "chat-item-manager-read-only-toggle");
    assert!(has(vcx, "chat-item-manager-read-only:On"), "toggles the pending value, not the stored one");
    assert_eq!(gate.held(), 1, "one mutation on the wire");
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(gate.release_one());
    vcx.run_until_parked();
    draw(vcx);
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "the queued click went out after the first ended");
        assert_eq!(calls[1]["readOnly"], true, "the user's last click is the host's last write");
    }
    assert_eq!(gate.held(), 1);
    assert!(has(vcx, "chat-item-manager-read-only:On"), "never flickered to Off in between");
    assert!(gate.release_one());
    vcx.run_until_parked();
    draw(vcx);
    assert!(has(vcx, "chat-item-manager-read-only:On"));
}

#[gpui::test]
fn a_transport_failure_keeps_the_value_drawn_and_says_the_change_may_not_have_saved(cx: &mut TestAppContext) {
    let (shell, _calls, gate, vcx) = manager_settings_with_gate(
        cx,
        |_| Err(mast_rpc::RpcError::Transport("connection reset".into())),
        |_| {},
    );
    click_selector(vcx, "chat-item-manager-read-only-toggle");
    assert!(gate.release_one());
    vcx.run_until_parked();
    draw(vcx);
    // The host may have applied it, so the UI does not claim the old value.
    assert!(has(vcx, "chat-item-manager-read-only:Off"));
    let error = shell.read_with(vcx, |shell, _| shell.sidebar_error_text.clone()).expect("an error is shown");
    assert!(error.contains("may not have saved"), "{error}");
}

#[gpui::test]
fn a_chain_chip_toggles_its_entry_off_and_on_at_once(cx: &mut TestAppContext) {
    let sonnet = mast_proto::StandingRoleModel { harness: "claude-code".into(), model: Some("claude-sonnet-5-5".into()) };
    let chain = vec![sonnet.clone()];
    let (_shell, calls, gate, vcx) = manager_settings_with_gate(cx, |_| Ok(serde_json::json!({})), move |snapshot| {
        snapshot.standing_roles.push(mast_proto::StandingRole {
            id: mast_proto::standing_role_id(mast_proto::StandingRoleKind::Manager, API),
            role: mast_proto::StandingRoleKind::Manager,
            scope_item_id: API.into(),
            current_chat_id: Some(CHAT_API.into()),
            chain: vec![CHAT_API.into()],
            fallback_chain: chain,
            state: mast_proto::StandingRoleState::Idle,
            worker_model: None,
            read_only: None,
        });
    });
    assert!(has(vcx, "chat-item-manager-fallback-row:1. claude-code, claude-sonnet-5-5"));
    click_selector(vcx, "chat-item-manager-fallback-add:Claude Code Sonnet 5.5");
    assert!(!has(vcx, "chat-item-manager-fallback-row:1. claude-code, claude-sonnet-5-5"), "removed at once, reply still held");
    assert_eq!(calls.lock().unwrap().last().unwrap()["fallbackChain"], serde_json::json!([]), "an empty chain is the fallback off");
    assert!(gate.release_one());
    vcx.run_until_parked();
    draw(vcx);
    assert!(!has(vcx, "chat-item-manager-fallback-row:1. claude-code, claude-sonnet-5-5"), "stays off until the frame");
    click_selector(vcx, "chat-item-manager-fallback-add:Claude Code Sonnet 5.5");
    assert!(has(vcx, "chat-item-manager-fallback-row:1. claude-code, claude-sonnet-5-5"), "and on again");
}

#[gpui::test]
fn a_plain_chats_settings_tab_has_chat_settings_and_no_manager_section(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    open_settings(&shell, vcx);
    assert!(has(vcx, "chat-settings"));
    assert!(has(vcx, "chat-settings-icon"));
    assert!(!has(vcx, "chat-item-manager"));
    assert!(!has(vcx, "chat-item-manager-read-only-toggle"));
}

#[gpui::test]
fn the_settings_tab_opens_once_and_closes(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    open_settings(&shell, vcx);
    open_settings(&shell, vcx);
    let count = shell.read_with(vcx, |s, cx| {
        s.right_tabs.get(&s.panel_key(cx)).map(|tabs| tabs.iter().filter(|t| **t == RightSurface::Settings).count())
    });
    assert_eq!(count, Some(1));
    shell.update_in(vcx, |shell, window, cx| shell.close_right_surface(RightSurface::Settings, window, cx));
    assert_ne!(shell.read_with(vcx, |s, cx| s.resolved_right_active(cx)), RightSurface::Settings);
}

#[gpui::test]
fn the_settings_tab_manager_settings_follow_a_role_that_hands_off(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        api_is_a_project(snapshot);
        snapshot.standing_roles.push(mast_proto::StandingRole {
            id: mast_proto::standing_role_id(mast_proto::StandingRoleKind::Manager, API),
            role: mast_proto::StandingRoleKind::Manager,
            scope_item_id: API.into(),
            current_chat_id: Some(CHAT_UI.into()),
            chain: vec![CHAT_API.into(), CHAT_UI.into()],
            fallback_chain: Vec::new(),
            state: mast_proto::StandingRoleState::Idle,
            worker_model: None,
            read_only: Some(false),
        });
    });
    open_settings(&shell, vcx);
    // The settings sit on the project item, so they stay for the earlier
    // chat after the role moved on to a successor.
    assert!(has(vcx, "chat-item-manager-read-only-toggle"));
    assert!(has(vcx, "chat-item-manager-read-only:Off"), "the stored Off shows");
    assert!(has(vcx, "chat-item-manager-fallback"), "the chain editor lives in Settings");
    let current: &'static str = Box::leak(format!("chat-item-manager-log:2. Title of {CHAT_UI} (current)").into_boxed_str());
    let earlier: &'static str = Box::leak(format!("chat-item-manager-log:1. Title of {CHAT_API}").into_boxed_str());
    assert!(has(vcx, current), "newest first, current marked");
    assert!(has(vcx, earlier));
}

#[gpui::test]
fn item_tab_sections_lead_with_icons(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, true, needs_attention);
    for name in [
        "path",
        "instructions",
        "runs-in",
        "model",
        "account",
        "links",
        "needs-you",
        "identifiers",
    ] {
        let selector: &'static str = Box::leak(format!("chat-item-{name}-icon").into_boxed_str());
        assert!(has(vcx, selector), "{selector}");
    }
}

#[gpui::test]
fn chip_tones_use_theme_tokens(_cx: &mut TestAppContext) {
    use super::chat_item_ui::{ChipTone, chip_palette};
    let theme = Theme::default();
    assert_eq!(
        chip_palette(&theme, ChipTone::Warn),
        (theme.warning, theme.warning)
    );
    assert_eq!(
        chip_palette(&theme, ChipTone::Link),
        (theme.accent, theme.text)
    );
    assert_eq!(chip_palette(&theme, ChipTone::Dim).1, theme.text_muted);
}

#[gpui::test]
fn the_item_tab_keeps_rows_tight_and_actions_together(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, needs_attention);
    expand(&shell, vcx, &["instructions", "links", "account"]);
    // A link is one text line, not an empty line plus its label.
    let pr = vcx.debug_bounds("chat-item-pr-0").unwrap();
    assert!(pr.size.height < px(24.0), "{:?}", pr.size);
    // Compare and Apply sit side by side.
    let compare = vcx
        .debug_bounds("chat-header-instructions-compare")
        .unwrap();
    let apply = vcx.debug_bounds("chat-header-instructions-apply");
    if let Some(apply) = apply {
        assert_eq!(compare.top(), apply.top());
    }
    // The account is a badge.
    assert!(has(vcx, "chat-item-account-badge"));
}

#[gpui::test]
fn child_rows_are_tight_single_lines(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    expand(&shell, vcx, &["children"]);
    assert!(has(vcx, "chat-item-children"));
    let row = vcx
        .debug_bounds(Box::leak(
            format!("chat-item-child-{API_AUTH}").into_boxed_str(),
        ))
        .expect("the child row");
    assert!(row.size.height < px(24.0), "{:?}", row.size);
}

#[gpui::test]
fn the_plan_row_shows_the_changed_marker_until_the_plan_is_viewed(cx: &mut TestAppContext) {
    use crate::artifacts::tests::{artifact, id, snapshot as artifacts_snapshot};
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    let mut plan = artifact(&id(1), "Plan", mast_proto::ArtifactKind::Markdown);
    plan.pinned_as_plan = true;
    let state = shell.read_with(vcx, |shell, _| shell.state.clone());
    state.update(vcx, |state, _| {
        state.set_test_artifacts(artifacts_snapshot(vec![plan.clone()]));
    });
    let viewed = |vcx: &mut VisualTestContext, rev: u32| {
        vcx.update(|_, cx| {
            crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
                settings.artifact_last_viewed.insert(id(1), rev);
            });
            cx.refresh_windows();
        });
        draw(vcx);
    };
    expand(&shell, vcx, &["plan"]);
    // Seen at the current revision: the plan row, no marker.
    viewed(vcx, 2);
    assert!(has(vcx, "chat-item-plan-open"));
    assert!(!has(vcx, "chat-item-plan-changed"));
    // Seen at r1, current r2: marked.
    viewed(vcx, 1);
    assert!(has(vcx, "chat-item-plan-changed"));
    // Opening the plan records r2: cleared.
    viewed(vcx, 2);
    assert!(!has(vcx, "chat-item-plan-changed"));
}

// ----------------------------------------------------------------------
// A1: the Needs-you block and the accordion rows
// ----------------------------------------------------------------------

/// A blocking ask (old and new), two ready-to-merge asks from worker chats,
/// and one FYI roll-up, created out of order.
fn mixed_needs(snapshot: &mut WorkOutlineSnapshot) {
    let at = |id: &str, chat: &str, text: &str, created: i64| {
        let mut r = request(id, "s1", API, chat, text);
        r.created_at = created;
        r
    };
    snapshot.user_action_requests.extend([
        at("r-fyi", CHAT_API, "2 open plan questions", 1_000),
        at("r-ready-new", "w-1", "Ready to merge: branch-b", 5_000),
        at("r-block-new", CHAT_API, "Which tenant?", 4_000),
        at("r-ready-old", "w-0", "Ready to merge: branch-a", 2_000),
        at("r-block-old", CHAT_API, "Pick the auth provider", 3_000),
    ]);
    if let Some(r) = snapshot
        .user_action_requests
        .iter_mut()
        .find(|r| r.id == "r-fyi")
    {
        r.dedupe_key = Some("artq:art1:rollup".into());
        r.priority = Some(mast_proto::work_outline_user_actions::UserActionPriority::Fyi);
        r.info = Some("[Open plan](mast-artifact:abc123)".into());
    }
}

fn row_top(vcx: &mut VisualTestContext, id: &str) -> gpui::Pixels {
    let selector: &'static str = Box::leak(format!("required-actions-row-{id}").into_boxed_str());
    vcx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} not drawn"))
        .top()
}

#[gpui::test]
fn needs_you_cards_group_blocking_then_ready_to_merge_then_fyi_oldest_first(
    cx: &mut TestAppContext,
) {
    let (_, vcx) = mount(cx, CHAT_API, true, mixed_needs);
    for heading in [
        "required-actions-group-Blocking",
        "required-actions-group-Ready",
        "required-actions-group-FYI",
    ] {
        assert!(has(vcx, heading), "{heading}");
    }
    let order = [
        "r-block-old",
        "r-block-new",
        "r-ready-old",
        "r-ready-new",
        "r-fyi",
    ];
    assert!(has(vcx, "required-actions-kind-r-block-old-Ask"));
    assert!(has(vcx, "required-actions-kind-r-ready-old-Handoff"));
    assert!(has(vcx, "required-actions-artifact-r-fyi-0"));
    let tops: Vec<_> = order.iter().map(|id| row_top(vcx, id)).collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]), "{tops:?}");
    assert!(has(vcx, "chat-item-needs-you-count"));
}

#[gpui::test]
fn cards_offer_the_inline_actions_for_their_group(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, true, mixed_needs);
    // Blocking: Reply & resolve, Resolve, Open (origin chat). No Approve.
    for selector in [
        "required-actions-reply-r-block-old",
        "required-actions-resolve-r-block-old",
        "required-actions-open-r-block-old",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
    assert!(!has(vcx, "required-actions-approve-r-block-old"));
    // Ready to merge: Approve and a jump to the worker's chat.
    for selector in [
        "required-actions-approve-r-ready-old",
        "required-actions-open-r-ready-old",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
}

#[gpui::test]
fn the_item_tab_has_no_other_sections_expanded_until_asked(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, needs_attention);
    // One line each: the rows exist, their bodies do not.
    for row in [
        "chat-item-instructions",
        "chat-item-runs-in",
        "chat-item-model",
        "chat-item-account",
        "chat-item-links",
    ] {
        assert!(has(vcx, row), "{row}");
    }
    for body in [
        "chat-item-instructions-edit",
        "chat-item-account-badge",
        "chat-item-pr-0",
    ] {
        assert!(!has(vcx, body), "{body} starts collapsed");
    }
    let row = vcx.debug_bounds("chat-item-model-toggle").unwrap();
    assert!(row.size.height <= px(24.0), "{:?}", row.size);
    // Click expands; click again collapses.
    let at = vcx
        .debug_bounds("chat-item-instructions-toggle")
        .unwrap()
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
    assert!(has(vcx, "chat-item-instructions-edit"));
    let at = vcx
        .debug_bounds("chat-item-instructions-toggle")
        .unwrap()
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
    assert!(!has(vcx, "chat-item-instructions-edit"));
    shell.read_with(vcx, |shell, _| assert!(shell.item_tab_open.is_empty()));
}

#[gpui::test]
fn instructions_are_summarised_and_expand_to_their_entries(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot
            .instructions
            .push(crate::work_outline::instructions::fixture::instruction(
                "ins-long",
                "s1",
                Some(API),
                "a0",
                &format!("First line\n{}", "long ".repeat(80)),
            ));
    });
    let facts = shell.read_with(vcx, |shell, cx| {
        shell.chat_item_facts(CHAT_API, cx).unwrap()
    });
    let entries = &facts.item.unwrap().instructions.entries;
    assert!(entries.iter().any(|e| e == "First line"), "{entries:?}");
    assert!(
        entries.iter().all(|e| e.chars().count() <= 141),
        "{entries:?}"
    );
}

#[gpui::test]
fn the_item_tab_stays_readable_with_maximal_data(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        needs_attention(snapshot);
        mixed_needs(snapshot);
        for n in 0..9 {
            let mut r = request(
                &format!("r-more-{n}"),
                "s1",
                API,
                CHAT_API,
                &"A very long question. ".repeat(20),
            );
            r.created_at = 10_000 + n;
            r.info = Some("detail ".repeat(100));
            snapshot.user_action_requests.push(r);
        }
    });
    add_workers(&shell, vcx, 15);
    expand(&shell, vcx, &["instructions", "links"]);
    let tab = vcx.debug_bounds("chat-item-tab").unwrap();
    // Nothing spills sideways, and a long question stays two lines.
    for id in ["r-block-old", "r-more-0", "r-more-8", "r-fyi"] {
        let selector: &'static str =
            Box::leak(format!("required-actions-row-{id}").into_boxed_str());
        let bounds = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector}"));
        assert!(
            bounds.right() <= tab.right(),
            "{selector} {bounds:?} in {tab:?}"
        );
    }
    let long = vcx.debug_bounds("required-actions-text-r-more-0").unwrap();
    assert!(
        long.size.height < px(40.0),
        "clamped to two lines: {:?}",
        long.size
    );
    // The team block keeps to the roll-up and three workers.
    let shown = (0..15)
        .filter(|i| {
            has(
                vcx,
                Box::leak(format!("chat-item-worker-w-{i}").into_boxed_str()),
            )
        })
        .count();
    assert_eq!(shown, 3);
    assert!(has(vcx, "chat-item-team-all"));
}

// ----------------------------------------------------------------------
// B2: the Team tab
// ----------------------------------------------------------------------

/// `n` workers under CHAT_API: child chats on the same project. Worker 0 is
/// working, 1 waits on an ask, 2 has a Ready-to-merge ask; the rest idle.
fn add_workers(shell: &Entity<Shell>, vcx: &mut VisualTestContext, n: usize) {
    shell.update(vcx, |shell, cx| {
        let now = chrono::Utc::now();
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            for i in 0..n {
                let id = format!("w-{i}");
                let item_id = format!("fixture-item-{i}");
                snapshot
                    .items
                    .iter_mut()
                    .find(|item| item.id == API)
                    .unwrap()
                    .child_ids
                    .push(item_id.clone());
                snapshot.items.push(
                    serde_json::from_value(serde_json::json!({
                        "id": item_id, "spaceId": "s1", "title": format!("Worker {i}"),
                        "parentWorkItemId": API, "chatIds": [id.clone()]
                    }))
                    .unwrap(),
                );
                let mut worker = chat(&id);
                worker.parent_chat_id = Some(CHAT_API.into());
                worker.branch = Some(format!("feature/w-{i}"));
                worker.last_message_preview = Some(format!("Editing file {i}\nmore"));
                worker.last_message_at = Some(now - chrono::Duration::minutes(i as i64));
                state.chats.push(worker);
                if i == 0 {
                    state.sessions.push(mast_proto::Session {
                        last_completed_turn: None,
                        awaiting_input: None,
                        last_turn_tool_failures: None,
                        todo_progress: None,
                        chat_id: id.clone(),
                        device_id: "device".into(),
                        status: mast_proto::SessionStatus::Working,
                        started_at: Some(now),
                        updated_at: now,
                    });
                }
            }
            snapshot.revision += 1;
            state.set_work_outline_for_test(snapshot);
            state.board_status_revision = state.board_status_revision.wrapping_add(1);
        });
        for i in 0..n {
            shell.chat_header_links.insert(
                format!("w-{i}"),
                ChatHeaderLinkCache {
                    checkout: None,
                    status: None,
                    fetched_at: Instant::now(),
                },
            );
        }
        cx.notify();
    });
    draw(vcx);
}

#[gpui::test]
fn tracker_does_not_rebuild_on_unchanged_frames(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 4);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    let before = shell.read_with(vcx, |shell, _| shell.tracker.rebuilds);
    for _ in 0..5 {
        shell.update(vcx, |shell, cx| {
            let _ = shell.render_team_tab(cx);
        });
        draw(vcx);
    }
    shell.read_with(vcx, |shell, _| assert_eq!(shell.tracker.rebuilds, before));
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            snapshot.revision += 1;
            snapshot.items[0].title.push_str(" updated");
            state.set_work_outline_for_test(snapshot);
        });
        cx.notify();
    });
    shell.update(vcx, |shell, cx| {
        shell.team_facts(CHAT_API, cx);
    });
    shell.read_with(vcx, |shell, _| {
        assert_eq!(shell.tracker.rebuilds, before + 1)
    });
}

#[gpui::test]
fn tracker_reuses_large_fact_sheets_across_unchanged_renders(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 200);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    let (first, all, rebuilds) = shell.update(vcx, |shell, cx| {
        let first = shell.team_facts(CHAT_API, cx);
        (
            first,
            shell.tracker_all_workers_for_test(),
            shell.tracker.rebuilds,
        )
    });
    for _ in 0..5 {
        shell.update(vcx, |shell, cx| {
            let _ = shell.render_team_tab_chip(cx);
            let _ = shell.render_team_tab(cx);
            assert!(std::sync::Arc::ptr_eq(
                &first,
                &shell.team_facts(CHAT_API, cx)
            ));
            assert!(std::sync::Arc::ptr_eq(
                &all,
                &shell.tracker_all_workers_for_test()
            ));
            assert_eq!(shell.tracker.rebuilds, rebuilds);
        });
    }
}

#[gpui::test]
fn tracker_updates_coordinator_archive_notice_after_chat_revision(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 1);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(!has(vcx, "team-coordinator-archived"));
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let mut chats = state.chats.clone();
            chats
                .iter_mut()
                .find(|chat| chat.id == CHAT_API)
                .unwrap()
                .archived = true;
            state.apply_chats(chats);
        });
        cx.notify();
    });
    draw(vcx);
    assert!(has(vcx, "team-coordinator-archived"));
}

#[gpui::test]
fn tracker_filters_workers_by_rollup(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 4);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(!has(vcx, "tracker-link-project"));
    assert!(has(vcx, "team-worker-w-0"));
    assert!(has(vcx, "team-worker-w-1"));
    let position = vcx
        .debug_bounds("tracker-filter-NeedsYou")
        .unwrap()
        .center();
    vcx.simulate_click(position, gpui::Modifiers::default());
    draw(vcx);
    assert!(!has(vcx, "team-worker-w-0"));
    assert!(has(vcx, "team-worker-w-1"));
    vcx.simulate_click(position, gpui::Modifiers::default());
    draw(vcx);
    assert!(has(vcx, "team-worker-w-0"));
}

#[gpui::test]
fn tracker_lists_unassigned_descendants_without_workers(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        let item = snapshot
            .items
            .iter_mut()
            .find(|item| item.id == API_AUTH)
            .unwrap();
        item.chat_ids.clear();
        item.effective_status = mast_proto::WorkItemEffectiveStatus::Active;
    });
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(has(vcx, "tracker-item-w-api-auth"));
}

#[gpui::test]
fn tracker_does_not_call_an_unloaded_pr_not_started(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.external_references.push(serde_json::from_value(serde_json::json!({
            "id":"pending-pr", "spaceId":"s1", "workItemId":"w-api-auth", "provider":"github",
            "referenceType":"pullRequest", "externalId":"example/project#57", "url":"https://github.com/example/project/pull/57"
        })).unwrap());
    });
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(has(vcx, "tracker-item-w-api-auth"));
    assert!(has(vcx, "tracker-pr-pending"));
}

#[gpui::test]
fn tracker_places_an_unassigned_item_with_a_pr_in_its_pr_stage(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.external_references.push(serde_json::from_value(serde_json::json!({
            "id":"known-pr", "spaceId":"s1", "workItemId":"w-api-auth", "provider":"github",
            "referenceType":"pullRequest", "externalId":"Example/Project#57", "url":"https://github.com/Example/Project/pull/57"
        })).unwrap());
    });
    shell.update(vcx, |shell, cx| {
        let pr = serde_json::from_value(serde_json::json!({ "number": 57, "url":"https://github.com/example/project/pull/57",
            "state":"OPEN", "isDraft":false, "reviewDecision":"APPROVED", "reviewers":[], "checks":"passing",
            "mergeable":"mergeable", "hasReviewerRequested":false })).unwrap();
        shell.set_tracker_item_prs_for_test("w-api-auth", vec![pr]);
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(has(vcx, "tracker-item-w-api-auth"));
    assert!(has(vcx, "tracker-stage-Ready to merge-true"));
}

fn queued_project_with_two_prs(snapshot: &mut WorkOutlineSnapshot) {
    snapshot.items.iter_mut().find(|item| item.id == API).unwrap().is_project = true;
    for (id, key, number) in [("q-landed", "a0", 57), ("q-next", "a1", 58)] {
        snapshot.items.iter_mut().find(|item| item.id == API).unwrap().child_ids.push(id.into());
        snapshot.items.push(serde_json::from_value(serde_json::json!({
            "id": id, "spaceId": "s1", "title": id, "parentWorkItemId": API, "queueOrderKey": key
        })).unwrap());
        snapshot.external_references.push(serde_json::from_value(serde_json::json!({
            "id": format!("{id}-pr"), "spaceId": "s1", "workItemId": id, "provider": "github",
            "referenceType": "pullRequest", "externalId": format!("example/project#{number}"),
            "url": format!("https://github.com/example/project/pull/{number}")
        })).unwrap());
    }
}

/// A project with one queued item and, when `loose`, one more child that is not queued.
fn queued_project_without_prs(loose: bool) -> impl FnOnce(&mut WorkOutlineSnapshot) {
    move |snapshot| {
        snapshot.items.iter_mut().find(|item| item.id == API).unwrap().is_project = true;
        let children = [("q-queued", Some("a0")), ("q-loose", None)];
        for (id, key) in children.into_iter().filter(|(id, _)| loose || *id == "q-queued") {
            snapshot.items.iter_mut().find(|item| item.id == API).unwrap().child_ids.push(id.into());
            snapshot.items.push(serde_json::from_value(serde_json::json!({
                "id": id, "spaceId": "s1", "title": id, "parentWorkItemId": API, "queueOrderKey": key
            })).unwrap());
        }
    }
}

#[gpui::test]
fn tracker_lists_queued_and_not_queued_work_as_the_same_row_with_queue_buttons(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, queued_project_without_prs(true));
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    // Both items render as the Tracker's ordinary item row, once each. The
    // queued one carries Up, Down and Remove, the other an Add button, and no
    // separate queue-row style or Not started group repeats either.
    assert!(has(vcx, "tracker-item-q-queued"));
    assert!(has(vcx, "tracker-item-q-loose"));
    assert!(has(vcx, "tracker-queue-row-q-queued"));
    assert!(has(vcx, "tracker-queue-up-0"));
    assert!(has(vcx, "tracker-queue-down-0"));
    assert!(has(vcx, "tracker-queue-remove-0"));
    assert!(!has(vcx, "tracker-queue-row-q-loose"));
    assert!(!has(vcx, "tracker-queue-add-q-queued"));
    assert!(has(vcx, "tracker-queue-add-q-loose"));
    assert!(has(vcx, "tracker-not-queued"));
}

#[gpui::test]
fn tracker_renders_a_queued_worker_as_its_worker_row_with_queue_buttons(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.items.iter_mut().find(|item| item.id == API).unwrap().is_project = true;
    });
    add_workers(&shell, vcx, 2);
    // Worker 0's item joins the queue. Worker 1's item stays outside it.
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            snapshot.items.iter_mut().find(|item| item.id == "fixture-item-0").unwrap().queue_order_key = Some("a0".into());
            snapshot.revision += 1;
            state.set_work_outline_for_test(snapshot);
            state.board_status_revision = state.board_status_revision.wrapping_add(1);
        });
        shell.open_team_tab(cx);
    });
    draw(vcx);
    // The queued worker keeps its row, once, and the other Building worker is
    // listed as not queued.
    assert!(has(vcx, "tracker-queue-row-fixture-item-0"));
    assert!(has(vcx, "tracker-queue-remove-0"));
    assert!(has(vcx, "team-worker-w-0"));
    assert!(has(vcx, "team-worker-w-1"));
    assert!(has(vcx, "tracker-not-queued"));
    assert!(!has(vcx, "tracker-queue-add-fixture-item-0"));
    assert!(has(vcx, "tracker-queue-add-fixture-item-1"));

    // The queued worker's row keeps its Needs you chip once it is awaiting input.
    assert!(!has(vcx, "tracker-needs-you-w-0"));
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.sessions.iter_mut().find(|s| s.chat_id == "w-0").unwrap().status =
                mast_proto::SessionStatus::AwaitingInput;
            state.board_status_revision = state.board_status_revision.wrapping_add(1);
        });
        cx.notify();
    });
    draw(vcx);
    assert!(has(vcx, "tracker-needs-you-w-0"));
    assert!(has(vcx, "tracker-queue-row-fixture-item-0"));

    // A collapsed Queue hides the row, which appears nowhere else, so it says
    // how many queued entries need the user.
    assert!(!has(vcx, "tracker-queue-collapsed-needs-you"));
    click_selector(vcx, "tracker-section-queue");
    assert!(!has(vcx, "tracker-queue-row-fixture-item-0"));
    assert!(has(vcx, "tracker-queue-collapsed-needs-you"));
    assert!(has(vcx, "team-worker-w-1"));
}

#[gpui::test]
fn tracker_folds_a_queued_items_descendant_worker_into_its_entry(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.items.iter_mut().find(|item| item.id == API).unwrap().is_project = true;
    });
    add_workers(&shell, vcx, 2);
    // Worker 1's item moves under worker 0's item, which is queued.
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            snapshot.items.iter_mut().find(|item| item.id == "fixture-item-1").unwrap().parent_work_item_id = Some("fixture-item-0".into());
            let parent = snapshot.items.iter_mut().find(|item| item.id == "fixture-item-0").unwrap();
            parent.queue_order_key = Some("a0".into());
            parent.child_ids.push("fixture-item-1".into());
            snapshot.revision += 1;
            state.set_work_outline_for_test(snapshot);
            state.board_status_revision = state.board_status_revision.wrapping_add(1);
        });
        shell.open_team_tab(cx);
    });
    draw(vcx);
    // A parent item with children is not a worker row of its own, so the entry
    // keeps the controls on a plain row and the descendant's worker follows it.
    // Nothing is offered as not queued.
    assert!(has(vcx, "tracker-queue-row-fixture-item-0"));
    assert!(has(vcx, "tracker-queue-item-fixture-item-0"));
    assert!(has(vcx, "team-worker-w-1"));
    assert!(!has(vcx, "tracker-queue-add-fixture-item-1"));
    assert!(!has(vcx, "tracker-not-queued"));
}

#[gpui::test]
fn tracker_never_puts_queue_controls_on_a_descendants_worker(cx: &mut TestAppContext) {
    use crate::tracker_lifecycle::LifecycleStage;
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.items.iter_mut().find(|item| item.id == API).unwrap().is_project = true;
    });
    add_workers(&shell, vcx, 2);
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            snapshot.items.iter_mut().find(|item| item.id == "fixture-item-1").unwrap().parent_work_item_id = Some("fixture-item-0".into());
            let parent = snapshot.items.iter_mut().find(|item| item.id == "fixture-item-0").unwrap();
            parent.queue_order_key = Some("a0".into());
            parent.child_ids.push("fixture-item-1".into());
            snapshot.revision += 1;
            state.set_work_outline_for_test(snapshot);
            state.board_status_revision = state.board_status_revision.wrapping_add(1);
        });
        shell.set_tracker_stage_filter_for_test(Some(LifecycleStage::Building));
        shell.open_team_tab(cx);
    });
    draw(vcx);
    // The descendant's worker still shows under the filter, but the queued
    // parent's own row is hidden, so no row offers its Up, Down or Remove.
    assert!(has(vcx, "team-worker-w-1"));
    assert!(!has(vcx, "tracker-queue-row-fixture-item-0"));
    assert!(!has(vcx, "tracker-queue-remove-0"));
}

#[gpui::test]
fn tracker_queue_follows_the_stage_filter(cx: &mut TestAppContext) {
    use crate::tracker_lifecycle::LifecycleStage;
    let (shell, vcx) = mount(cx, CHAT_API, true, queued_project_without_prs(true));
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(has(vcx, "tracker-queue-row-q-queued"));
    shell.update(vcx, |shell, cx| {
        shell.set_tracker_stage_filter_for_test(Some(LifecycleStage::Done));
        cx.notify();
    });
    draw(vcx);
    assert!(!has(vcx, "tracker-queue-row-q-queued"));
    assert!(!has(vcx, "tracker-item-q-queued"));
    shell.update(vcx, |shell, cx| {
        shell.set_tracker_stage_filter_for_test(Some(LifecycleStage::NotStarted));
        cx.notify();
    });
    draw(vcx);
    assert!(has(vcx, "tracker-queue-row-q-queued"));
}

#[gpui::test]
fn tracker_keeps_its_groups_and_pause_toggle_for_a_paused_empty_queue(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        let project = snapshot.items.iter_mut().find(|item| item.id == API).unwrap();
        project.is_project = true;
        project.queue_paused = true;
    });
    add_workers(&shell, vcx, 1);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(has(vcx, "tracker-queue-pause-toggle"));
    assert!(has(vcx, "team-worker-w-0"));
    assert!(!has(vcx, "tracker-not-queued"));
}

#[gpui::test]
fn tracker_shows_no_not_queued_block_when_everything_is_queued(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, queued_project_without_prs(false));
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(has(vcx, "tracker-queue-row-q-queued"));
    assert!(has(vcx, "tracker-item-q-queued"));
    assert!(!has(vcx, "tracker-not-queued"));
}

#[gpui::test]
fn tracker_shows_a_dependent_waiting_to_land_after_an_agent_completes_its_prerequisite(cx: &mut TestAppContext) {
    use mast_proto::QueueEntryState::{Done, Waiting};
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.items.iter_mut().find(|item| item.id == API).unwrap().is_project = true;
        for row in [
            serde_json::json!({
                "id": "q-impl", "spaceId": "s1", "title": "Implementation", "parentWorkItemId": API,
                "queueOrderKey": "a0", "queueCompletion": "landed", "status": "completed",
                "completedBy": { "kind": "agent", "deviceId": "d" }
            }),
            serde_json::json!({
                "id": "q-review", "spaceId": "s1", "title": "Review", "parentWorkItemId": API,
                "queueOrderKey": "a1", "queuePrerequisites": ["q-impl"]
            }),
        ] {
            let id = row["id"].as_str().unwrap().to_owned();
            snapshot.items.iter_mut().find(|item| item.id == API).unwrap().child_ids.push(id);
            snapshot.items.push(serde_json::from_value(row).unwrap());
        }
    });
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    // The prerequisite reads Done, but an agent completed it, so it has not landed.
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(
            shell.tracker_queue_states_for_test(CHAT_API, cx),
            Some(vec![("q-impl".into(), Done, false), ("q-review".into(), Waiting, false)])
        );
    });
    assert!(has(vcx, "tracker-queue-row-q-review"));
    assert!(!has(vcx, "tracker-queue-row-q-impl"));
}

fn queue_pr(number: u32, state: &str) -> mast_engine::pr_ticket_cache::PrStatus {
    serde_json::from_value(serde_json::json!({
        "number": number, "url": format!("https://github.com/example/project/pull/{number}"),
        "state": state, "isDraft": false, "reviewers": [], "mergeable": "mergeable",
        "hasReviewerRequested": false
    })).unwrap()
}

#[gpui::test]
fn tracker_shows_the_queue_and_a_merged_pr_leaves_up_next_for_done(cx: &mut TestAppContext) {
    use mast_proto::QueueEntryState::{Done, Ready};
    let (shell, vcx) = mount(cx, CHAT_API, true, queued_project_with_two_prs);
    shell.update(vcx, |shell, cx| {
        shell.set_tracker_item_prs_for_test("q-landed", vec![queue_pr(57, "OPEN")]);
        shell.set_tracker_item_prs_for_test("q-next", vec![queue_pr(58, "OPEN")]);
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(has(vcx, "tracker-queue"));
    assert!(has(vcx, "tracker-queue-row-q-landed"));
    assert!(has(vcx, "tracker-queue-row-q-next"));
    assert!(!has(vcx, "tracker-queue-paused"));

    // The first item's PR merges: it leaves the Queue for Done in the Tracker,
    // and the queue the dispatcher reads agrees.
    shell.update(vcx, |shell, cx| {
        shell.set_tracker_item_prs_for_test("q-landed", vec![queue_pr(57, "MERGED")]);
        cx.notify();
    });
    draw(vcx);
    assert!(!has(vcx, "tracker-queue-row-q-landed"));
    assert!(has(vcx, "tracker-queue-row-q-next"));
    assert!(has(vcx, "tracker-item-q-landed"));
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(
            shell.tracker_queue_states_for_test(CHAT_API, cx),
            Some(vec![("q-landed".into(), Done, false), ("q-next".into(), Ready, false)])
        );
    });

    // A paused queue shows as paused.
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            snapshot.items.iter_mut().find(|item| item.id == API).unwrap().queue_paused = true;
            snapshot.revision += 1;
            state.set_work_outline_for_test(snapshot);
        });
        cx.notify();
    });
    draw(vcx);
    assert!(has(vcx, "tracker-queue-paused"));
    // A pause with no recorded provenance (set before it was kept) shows no note.
    assert!(!has(vcx, "tracker-queue-pause-note"));

    // A Manager's pause shows who paused and why, and resuming drops the note.
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            snapshot.items.iter_mut().find(|item| item.id == API).unwrap().queue_pause = Some(mast_proto::QueuePauseNote {
                by: mast_proto::QueuePausedBy::Manager,
                reason: Some("release freeze".into()),
                chat_id: Some(CHAT_API.into()),
            });
            snapshot.revision += 1;
            state.set_work_outline_for_test(snapshot);
        });
        cx.notify();
    });
    draw(vcx);
    assert!(has(vcx, "tracker-queue-pause-note"));
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            let item = snapshot.items.iter_mut().find(|item| item.id == API).unwrap();
            item.queue_paused = false;
            snapshot.revision += 1;
            state.set_work_outline_for_test(snapshot);
        });
        cx.notify();
    });
    draw(vcx);
    assert!(!has(vcx, "tracker-queue-paused") && !has(vcx, "tracker-queue-pause-note"));
}

#[gpui::test]
fn tracker_finds_a_pr_answer_for_a_reference_written_with_a_trailing_path(cx: &mut TestAppContext) {
    use mast_proto::QueueEntryState::{Done, Ready};
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        queued_project_with_two_prs(snapshot);
        let reference = snapshot.external_references.iter_mut().find(|r| r.work_item_id == "q-landed").unwrap();
        reference.url = "https://github.com/Example/Project/pull/57/files#diff-1".into();
    });
    shell.update(vcx, |shell, cx| {
        shell.set_tracker_item_prs_for_test("q-landed", vec![queue_pr(57, "MERGED")]);
        shell.set_tracker_item_prs_for_test("q-next", vec![queue_pr(58, "OPEN")]);
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(!has(vcx, "tracker-queue-row-q-landed"));
    assert!(!has(vcx, "tracker-queue-held"));
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(
            shell.tracker_queue_states_for_test(CHAT_API, cx),
            Some(vec![("q-landed".into(), Done, false), ("q-next".into(), Ready, false)])
        );
    });
}

#[gpui::test]
fn tracker_does_not_hold_the_queue_for_a_pr_url_no_parser_accepts(cx: &mut TestAppContext) {
    use mast_proto::QueueEntryState::Ready;
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        queued_project_with_two_prs(snapshot);
        let reference = snapshot.external_references.iter_mut().find(|r| r.work_item_id == "q-landed").unwrap();
        reference.url = "https://git.example.com/example/project/pull/57".into();
    });
    shell.update(vcx, |shell, cx| {
        shell.set_tracker_item_prs_for_test("q-next", vec![queue_pr(58, "OPEN")]);
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(!has(vcx, "tracker-queue-held"));
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(
            shell.tracker_queue_states_for_test(CHAT_API, cx),
            Some(vec![("q-landed".into(), Ready, false), ("q-next".into(), Ready, false)])
        );
    });
}

fn external_run(id: &str, item: &str, tag: &str, updated_at: i64, ended: Option<&str>) -> mast_proto::ExternalRunView {
    serde_json::from_value(serde_json::json!({
        "id": id, "spaceId": "s1", "workItemId": item, "kind": "codespace", "runTag": tag,
        "codespaceName": format!("box-{tag}"), "provider": "claude", "branch": format!("feature/{tag}"),
        "createdAt": 1, "updatedAt": updated_at, "expectedAlive": ended.is_none(),
        "endedAt": ended.map(|_| updated_at), "outcome": ended
    })).unwrap()
}

#[gpui::test]
fn tracker_lists_the_runs_on_the_coordinators_items(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.items.push(serde_json::from_value(serde_json::json!({
            "id": "outside", "spaceId": "s1", "title": "Somewhere else"
        })).unwrap());
        snapshot.external_runs = vec![
            external_run("run-live", API_AUTH, "live", 30, None),
            external_run("run-done", API_AUTH, "done", 20, Some("done")),
            external_run("run-outside", "outside", "outside", 40, None),
        ];
    });
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(has(vcx, "tracker-runs"));
    assert!(has(vcx, "tracker-run-run-live"));
    assert!(has(vcx, "tracker-run-run-done"));
    // A run on an item outside the coordinator's subtree is not its to show.
    assert!(!has(vcx, "tracker-run-run-outside"));
    // Collapsing Runs hides its rows and keeps its heading.
    click_selector(vcx, "tracker-section-runs");
    assert!(has(vcx, "tracker-section-runs"));
    assert!(!has(vcx, "tracker-run-run-live") && !has(vcx, "tracker-run-run-done"));
    click_selector(vcx, "tracker-section-runs");
    assert!(has(vcx, "tracker-run-run-live"));
}

#[gpui::test]
fn tracker_shows_no_runs_section_when_nothing_is_recorded(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(!has(vcx, "tracker-runs"));
}

#[gpui::test]
fn tracker_shows_no_queue_to_a_chat_below_the_project(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        queued_project_with_two_prs(snapshot);
        // The project moves up a level: the chat's item is now inside it.
        snapshot.items.iter_mut().find(|item| item.id == API).unwrap().is_project = false;
        snapshot.items.push(serde_json::from_value(serde_json::json!({
            "id": "q-root", "spaceId": "s1", "title": "Project", "isProject": true, "childIds": [API]
        })).unwrap());
        snapshot.items.iter_mut().find(|item| item.id == API).unwrap().parent_work_item_id = Some("q-root".into());
    });
    shell.update(vcx, |shell, cx| {
        shell.team_facts(CHAT_API, cx);
    });
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(shell.tracker_queue_states_for_test(CHAT_API, cx), None);
    });
}

#[gpui::test]
fn tracker_shows_a_queued_item_with_an_errored_worker_as_stalled(cx: &mut TestAppContext) {
    use mast_proto::QueueEntryState::{Ready, Stalled};
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        queued_project_with_two_prs(snapshot);
        snapshot.items.iter_mut().find(|item| item.id == "q-landed").unwrap().chat_ids = vec!["q-worker".into()];
    });
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let now = chrono::Utc::now();
            let mut worker = chat("q-worker");
            worker.parent_chat_id = Some(CHAT_API.into());
            state.chats.push(worker);
            state.sessions.push(mast_proto::Session {
                last_completed_turn: None,
                awaiting_input: None,
                last_turn_tool_failures: None,
                todo_progress: None,
                chat_id: "q-worker".into(),
                device_id: "local".into(),
                status: mast_proto::SessionStatus::Errored,
                started_at: Some(now),
                updated_at: now,
            });
        });
        shell.set_tracker_item_prs_for_test("q-landed", vec![queue_pr(57, "OPEN")]);
        shell.set_tracker_item_prs_for_test("q-next", vec![queue_pr(58, "OPEN")]);
        shell.open_team_tab(cx);
    });
    draw(vcx);
    // The stalled item stays in the Queue, and it does not hold the queue.
    assert!(has(vcx, "tracker-queue-row-q-landed"));
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(
            shell.tracker_queue_states_for_test(CHAT_API, cx),
            Some(vec![("q-landed".into(), Stalled, false), ("q-next".into(), Ready, false)])
        );
    });
}

#[gpui::test]
fn tracker_marks_a_queued_item_pending_until_its_pr_has_an_answer(cx: &mut TestAppContext) {
    use mast_proto::QueueEntryState::Ready;
    let (shell, vcx) = mount(cx, CHAT_API, true, queued_project_with_two_prs);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    // No PR answer yet: both rows show, as Ready, and the Tracker says why
    // nothing is being offered.
    assert!(has(vcx, "tracker-queue-row-q-landed"));
    assert!(has(vcx, "tracker-queue-held"));
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(
            shell.tracker_queue_states_for_test(CHAT_API, cx),
            Some(vec![("q-landed".into(), Ready, true), ("q-next".into(), Ready, true)])
        );
    });
    // One answer settles that row only.
    shell.update(vcx, |shell, cx| {
        shell.set_tracker_item_prs_for_test("q-landed", vec![queue_pr(57, "OPEN")]);
        cx.notify();
    });
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(
            shell.tracker_queue_states_for_test(CHAT_API, cx),
            Some(vec![("q-landed".into(), Ready, false), ("q-next".into(), Ready, true)])
        );
    });
    // The first entry is offerable now, so the queue is no longer held.
    draw(vcx);
    assert!(!has(vcx, "tracker-queue-held"));
}

fn link_status_with_pr(state: &str) -> mast_engine::pr_ticket_cache::ChatLinkStatus {
    let pr = serde_json::json!({"number": 57, "url": "https://github.com/example/project/pull/57", "state": state,
        "isDraft": false, "reviewers": [], "mergeable": "mergeable", "hasReviewerRequested": false});
    serde_json::from_value(serde_json::json!({ "isWorktree": false, "pr": pr,
        "prLinks": [{"url": "https://github.com/example/project/pull/57", "source": "created_in_chat", "detail": pr}] })).unwrap()
}

fn pr_update(urls: &[&str]) -> mast_engine::pr_ticket_cache::PrCacheUpdate {
    mast_engine::pr_ticket_cache::PrCacheUpdate { revision: 1, urls: urls.iter().map(|url| (*url).to_owned()).collect(), branches: Vec::new() }
}

const PR_57: &str = "https://github.com/example/project/pull/57";

fn stage_counts(shell: &Entity<Shell>, vcx: &mut VisualTestContext) -> [usize; 6] {
    shell.read_with(vcx, |shell, _| shell.tracker.counts)
}

#[gpui::test]
fn a_pr_cache_update_refetches_the_chats_link_status_and_moves_its_stage(cx: &mut TestAppContext) {
    use crate::tracker_lifecycle::LifecycleStage as Stage;
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let guard = runtime.enter();
    let (out, mut requests) = tokio::sync::mpsc::channel::<String>(64);
    let (replies, inbound) = tokio::sync::mpsc::channel::<String>(64);
    let engine = crate::state::EngineHandle::from_test_client(mast_rpc::RpcClient::new(out, inbound));
    shell.update(vcx, |shell, cx| shell.state.update(cx, |state, _| state.set_test_engine(engine)));
    add_workers(&shell, vcx, 1);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    let building = stage_counts(&shell, vcx);
    assert!(building[Stage::Building.index()] >= 1, "{building:?}");

    // The engine's PR cache now knows the chat's PR is open: the Tracker must
    // ask for the chat's link status again, then show the new stage.
    let mut answer = |vcx: &mut VisualTestContext, state: &str| {
        let request = loop {
            runtime.block_on(tokio::task::yield_now());
            vcx.run_until_parked();
            let Ok(line) = requests.try_recv() else { panic!("no link status refetch was requested") };
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            if request["method"] == mast_rpc::methods::CHAT_LINK_STATUS && request["params"]["chatId"] == "w-0" { break request; }
        };
        replies.try_send(serde_json::json!({"id": request["id"], "ok": link_status_with_pr(state)}).to_string()).unwrap();
        for _ in 0..20 {
            runtime.block_on(tokio::task::yield_now());
            vcx.run_until_parked();
        }
        vcx.executor().advance_clock(Duration::from_millis(500));
        vcx.run_until_parked();
        draw(vcx);
    };
    shell.update(vcx, |shell, cx| shell.on_tracker_pr_cache_changed(CHAT_API, None, cx));
    draw(vcx);
    answer(vcx, "OPEN");
    let mut expected = building;
    expected[Stage::Building.index()] -= 1;
    expected[Stage::PrReview.index()] += 1;
    assert_eq!(stage_counts(&shell, vcx), expected, "an open PR moves the ticket to PR review");

    shell.update(vcx, |shell, cx| shell.on_tracker_pr_cache_changed(CHAT_API, Some(&pr_update(&[PR_57])), cx));
    draw(vcx);
    answer(vcx, "MERGED");
    expected[Stage::PrReview.index()] -= 1;
    expected[Stage::Done.index()] += 1;
    assert_eq!(stage_counts(&shell, vcx), expected, "a merged PR moves the ticket to Done");
    drop(guard);
}

#[gpui::test]
fn a_pr_cache_update_expires_only_the_chats_it_touches(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 3);
    let other = "https://github.com/example/project/pull/58";
    shell.update(vcx, |shell, cx| {
        let unlinked = mast_engine::pr_ticket_cache::ChatLinkStatus::default();
        for (id, status) in [("w-0", link_status_with_pr("OPEN")), ("w-1", {
            let mut status = link_status_with_pr("OPEN");
            for link in &mut status.pr_links { link.url = other.into(); }
            status.pr.as_mut().unwrap().url = Some(other.into());
            status
        }), ("w-2", unlinked)] {
            shell.chat_header_links.insert(id.into(), ChatHeaderLinkCache { checkout: None, status: Some(status), fetched_at: Instant::now() });
        }
        let ids: Vec<String> = ["w-0", "w-1", "w-2"].map(String::from).to_vec();
        assert!(shell.board_links_due(&ids, cx).is_empty());
        shell.on_tracker_pr_cache_changed(CHAT_API, Some(&pr_update(&[PR_57])), cx);
        assert_eq!(shell.board_links_due(&ids, cx), vec!["w-0"], "only the chat linked to the changed PR is due");
        shell.chat_header_links.get_mut("w-2").unwrap().checkout = Some(("/repo/w-2".into(), "w2".into(), Some("feature/w-2".into())));
        let mut update = pr_update(&[]);
        update.branches = vec![mast_engine::pr_ticket_cache::PrBranchKey { cwd: "/repo/w-2".into(), branch: "feature/w-2".into() }];
        shell.on_tracker_pr_cache_changed(CHAT_API, Some(&update), cx);
        let mut due = shell.board_links_due(&ids, cx);
        due.sort();
        assert_eq!(due, vec!["w-0", "w-2"], "a branch change reaches only the chats on that branch, not every chat without a PR");
    });
}

#[gpui::test]
fn a_large_tracker_refetches_every_due_chat_without_an_unrelated_rebuild(cx: &mut TestAppContext) {
    use crate::tracker_lifecycle::LifecycleStage as Stage;
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let guard = runtime.enter();
    let (out, mut requests) = tokio::sync::mpsc::channel::<String>(256);
    let (replies, inbound) = tokio::sync::mpsc::channel::<String>(256);
    let engine = crate::state::EngineHandle::from_test_client(mast_rpc::RpcClient::new(out, inbound));
    shell.update(vcx, |shell, cx| shell.state.update(cx, |state, _| state.set_test_engine(engine)));
    add_workers(&shell, vcx, 20);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    // Statuses already known and unchanged: their refetch replies must not rely on a rebuild to continue.
    shell.update(vcx, |shell, _| for i in 0..20 {
        shell.chat_header_links.get_mut(&format!("w-{i}")).unwrap().status = Some(Default::default());
    });
    let building = stage_counts(&shell, vcx);
    shell.update(vcx, |shell, cx| shell.on_tracker_pr_cache_changed(CHAT_API, None, cx));
    draw(vcx);
    // Answer every link status request; only the last chat to be asked has a PR.
    let mut asked = std::collections::HashSet::new();
    let mut idle_rounds = 0;
    while idle_rounds < 5 {
        runtime.block_on(tokio::task::yield_now());
        vcx.run_until_parked();
        let mut got = false;
        while let Ok(line) = requests.try_recv() {
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            if request["method"] != mast_rpc::methods::CHAT_LINK_STATUS { continue; }
            got = true;
            let chat = request["params"]["chatId"].as_str().unwrap().to_owned();
            let status = if chat == "w-19" { link_status_with_pr("OPEN") } else { mast_engine::pr_ticket_cache::ChatLinkStatus::default() };
            asked.insert(chat);
            replies.try_send(serde_json::json!({"id": request["id"], "ok": status}).to_string()).unwrap();
        }
        idle_rounds = if got { 0 } else { idle_rounds + 1 };
        vcx.executor().advance_clock(Duration::from_millis(500));
    }
    draw(vcx);
    let missing: Vec<_> = (0..20).map(|i| format!("w-{i}")).filter(|id| !asked.contains(id)).collect();
    assert!(missing.is_empty(), "every due chat is refetched, not just the first 8; missing {missing:?}");
    let mut expected = building;
    expected[Stage::Building.index()] -= 1;
    expected[Stage::PrReview.index()] += 1;
    assert_eq!(stage_counts(&shell, vcx), expected, "the last chat reaches PR review");
    drop(guard);
}

#[gpui::test]
fn a_batch_of_link_status_changes_rebuilds_the_tracker_once(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 3);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    let (revision, rebuilds) = shell.read_with(vcx, |shell, _| (shell.board_links_revision, shell.tracker.rebuilds));
    shell.update(vcx, |shell, cx| {
        for id in ["w-0", "w-1", "w-2"] { shell.mark_board_links_dirty(id.into(), cx); }
    });
    vcx.executor().advance_clock(Duration::from_millis(500));
    vcx.run_until_parked();
    draw(vcx);
    shell.read_with(vcx, |shell, _| {
        assert_eq!(shell.board_links_revision, revision.wrapping_add(1), "one revision bump for the batch");
        assert_eq!(shell.tracker.rebuilds, rebuilds + 1);
    });
}

#[gpui::test]
fn tracker_keeps_items_whose_only_chat_is_archived(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 1);
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state
                .chats
                .iter_mut()
                .find(|chat| chat.id == "w-0")
                .unwrap()
                .archived = true;
            state.board_status_revision = state.board_status_revision.wrapping_add(1);
        });
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(has(vcx, "tracker-item-fixture-item-0"));
}

#[gpui::test]
fn tracker_shows_cached_linear_issues_and_local_items_together(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 2);
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            snapshot.space_settings.linear_enabled = true;
            for (id, item, kind, external_id) in [("project-ref", API, "project", "project"), ("issue-ref", "fixture-item-0", "issue", "EX-1")] {
                snapshot.external_references.push(serde_json::from_value(serde_json::json!({
                    "id": id, "spaceId":"s1", "workItemId":item, "provider":"linear", "referenceType":kind, "externalId":external_id
                })).unwrap());
            }
            snapshot.revision += 1;
            state.set_work_outline_for_test(snapshot);
        });
        shell.set_tracker_project_for_test("default", "project", vec![
            mast_proto::linear::LinearIssueSummary { id:"i1".into(), identifier:"EX-1".into(), title:"Linked issue".into(), ..Default::default() },
            mast_proto::linear::LinearIssueSummary { id:"i2".into(), identifier:"EX-2".into(), title:"Unstarted issue".into(), ..Default::default() },
        ]);
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(!has(vcx, "tracker-link-project"));
    assert!(has(vcx, "team-worker-w-0"));
    assert!(has(vcx, "team-worker-w-1"));
    assert!(has(vcx, "tracker-item-linear:EX-2"));
}

#[gpui::test]
fn tracker_discloses_a_truncated_project(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.space_settings.linear_enabled = true;
        snapshot.external_references.push(serde_json::from_value(serde_json::json!({
            "id":"project-ref", "spaceId":"s1", "workItemId":API, "provider":"linear", "referenceType":"project", "externalId":"project"
        })).unwrap());
    });
    shell.update(vcx, |shell, cx| {
        let issues = (0..250)
            .map(|index| mast_proto::linear::LinearIssueSummary {
                identifier: format!("EX-{index}"),
                ..Default::default()
            })
            .collect();
        shell.set_tracker_project_for_test("default", "project", issues);
        shell.set_tracker_project_more_for_test("default", "project", true);
        shell.team_facts(CHAT_API, cx);
        assert_eq!(
            shell.tracker_source_notice_for_test(),
            Some("Showing 250 Linear tickets; more not loaded.")
        );
    });
}

#[gpui::test]
fn tracker_discards_project_cache_after_account_credentials_change(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.space_settings.linear_enabled = true;
        snapshot.external_references.push(serde_json::from_value(serde_json::json!({
            "id":"project-ref", "spaceId":"s1", "workItemId":API, "provider":"linear", "referenceType":"project", "externalId":"project"
        })).unwrap());
    });
    shell.update(vcx, |shell, cx| {
        shell.set_tracker_project_for_test(
            "default",
            "project",
            vec![mast_proto::linear::LinearIssueSummary {
                id: "i".into(),
                identifier: "EX-1".into(),
                title: "Issue".into(),
                ..Default::default()
            }],
        );
        shell.team_facts(CHAT_API, cx);
        assert!(shell.tracker_has_item_for_test("linear:EX-1"));
        shell
            .state
            .update(cx, |state, _| state.linear.invalidate_account("default"));
        shell.team_facts(CHAT_API, cx);
        assert!(!shell.tracker_has_item_for_test("linear:EX-1"));
    });
}

#[gpui::test]
fn tracker_cleanup_completes_an_item_and_links_a_pr(cx: &mut TestAppContext) {
    use mast_engine::work_outline_health::HealthEntry;
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 1);
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.work_outline_actions_override =
                Some(crate::work_outline::WorkOutlineActions::scripted(
                    std::sync::Arc::new(move |method, params| {
                        recorded
                            .lock()
                            .unwrap()
                            .push((method.to_owned(), params.clone()));
                        Ok(serde_json::json!({}))
                    }),
                ));
        });
        shell.open_team_tab(cx);
        shell.set_tracker_health_for_test(
            vec![
                HealthEntry::MergedPrOnOpenItem {
                    item_id: "fixture-item-0".into(),
                    url: "https://github.com/example/project/pull/20".into(),
                },
                HealthEntry::UnlinkedPr {
                    url: "https://github.com/example/project/pull/21".into(),
                },
            ],
            vec![("fixture-item-0".into(), "Example ticket".into())],
        );
    });
    draw(vcx);
    assert!(!has(vcx, "tracker-cleanup"));
    click_selector(vcx, "tracker-cleanup-toggle");
    draw(vcx);
    assert!(has(vcx, "tracker-cleanup"));
    click_selector(vcx, "cleanup-complete-0");
    vcx.run_until_parked();
    draw(vcx);
    click_selector(vcx, "cleanup-link-1");
    draw(vcx);
    click_selector(vcx, "tracker-health-target-0");
    vcx.run_until_parked();
    let calls = calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|(_, params)| params["op"] == "updateWorkItem"
                && params["workItemId"] == "fixture-item-0"
                && params["changes"]["status"] == "completed")
    );
    assert!(calls.iter().any(
        |(_, params)| params["op"] == "createWorkItemExternalReference"
            && params["workItemId"] == "fixture-item-0"
            && params["provider"] == "github"
            && params["referenceType"] == "pullRequest"
            && params["externalId"] == "example/project#21"
    ));
}

#[gpui::test]
fn tracker_cleanup_sits_below_the_work_queue(cx: &mut TestAppContext) {
    use mast_engine::work_outline_health::HealthEntry;
    let (shell, vcx) = mount(cx, CHAT_API, true, queued_project_without_prs(true));
    shell.update(vcx, |shell, cx| {
        shell.open_team_tab(cx);
        shell.set_tracker_health_for_test(
            vec![HealthEntry::ChatOnContainer { item_id: "q-queued".into(), chat_id: "w-elsewhere".into() }],
            vec![("q-queued".into(), "Queued item".into())],
        );
    });
    draw(vcx);
    let queued = vcx.debug_bounds("tracker-queue-row-q-queued").unwrap();
    let not_queued = vcx.debug_bounds("tracker-not-queued").unwrap();
    let heading = vcx.debug_bounds("tracker-cleanup-heading").unwrap();
    assert!(heading.origin.y > queued.origin.y);
    assert!(heading.origin.y > not_queued.origin.y);
    // Collapsed until opened, from either the heading or the roll-up chip.
    assert!(!has(vcx, "tracker-cleanup"));
    click_selector(vcx, "tracker-cleanup-heading");
    assert!(has(vcx, "tracker-cleanup"));
    let row = vcx.debug_bounds("tracker-cleanup-row-0").unwrap();
    assert!(row.origin.y > heading.origin.y);
    // A chat on a container is offered its fixes, not just "Open chat".
    assert!(has(vcx, "cleanup-archive-0") && has(vcx, "cleanup-move-0") && has(vcx, "cleanup-open-0"));
    click_selector(vcx, "tracker-cleanup-toggle");
    assert!(!has(vcx, "tracker-cleanup"));
    assert!(has(vcx, "tracker-cleanup-heading"));
}

#[gpui::test]
fn tracker_cleanup_groups_repeated_chats_and_never_archives_blocked_ones(cx: &mut TestAppContext) {
    use mast_engine::work_outline_health::{HealthBlock, HealthChatFacts, HealthEntry};
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 3);
    let (mut sent, _runtime) = record_engine(&shell, vcx);
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            for chat in state.chats.iter_mut().filter(|chat| chat.id.starts_with("w-")) {
                chat.title = Some("Mast development queue check".into());
            }
        });
        shell.open_team_tab(cx);
    });
    draw(vcx);
    // The first health read clears test entries; set them once the key is current.
    shell.update(vcx, |shell, cx| shell.load_tracker_health(CHAT_API, cx));
    let facts = |blocked| HealthChatFacts { idle_seconds: 4 * 86_400, blocked, live_children: 0 };
    shell.update(vcx, |shell, cx| {
        shell.set_tracker_health_for_test(
            ["w-0", "w-1", "w-2"].into_iter().map(|id| HealthEntry::ChatOnContainer { item_id: API.into(), chat_id: id.into() }).collect(),
            vec![(API.into(), "Project".into())],
        );
        shell.set_tracker_health_chats_for_test(vec![
            ("w-0".into(), facts(Some(HealthBlock::Running))),
            ("w-1".into(), facts(None)),
            ("w-2".into(), facts(None)),
        ]);
        cx.notify();
    });
    draw(vcx);
    click_selector(vcx, "tracker-cleanup-heading");
    // One row for the three repeated chats, with a single bulk fix; the chats are listed on demand.
    assert!(has(vcx, "cleanup-archive-0"));
    assert!(!has(vcx, "tracker-cleanup-row-1"));
    assert!(!has(vcx, "cleanup-open-0-0"));
    click_selector(vcx, "cleanup-expand-0");
    assert!(has(vcx, "cleanup-open-0-0") && has(vcx, "cleanup-open-0-1") && has(vcx, "cleanup-open-0-2"));
    // The running chat has no archive button of its own.
    assert!(!has(vcx, "cleanup-archive-0-0"));
    assert!(has(vcx, "cleanup-archive-0-1") && has(vcx, "cleanup-archive-0-2"));
    click_selector(vcx, "cleanup-archive-0");
    vcx.run_until_parked();
    // Through the engine's guarded call, which rechecks each chat, not a bare archive.
    let requests = health_archive_requests(&mut sent);
    assert_eq!(requests.len(), 1);
    assert_eq!(archive_chats(&requests[0]), [("w-1".to_owned(), 4 * 86_400 - 60), ("w-2".to_owned(), 4 * 86_400 - 60)]);
}

/// The `WorkOutlineHealthArchive` requests sent so far, as their params.
fn health_archive_requests(sent: &mut Sent) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    while let Ok(line) = sent.try_recv() {
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_ne!(request["params"]["op"], "setChatArchived", "Cleanup must not archive without the engine's recheck");
        if request["method"] == mast_rpc::methods::WORK_OUTLINE_HEALTH_ARCHIVE {
            out.push(request["params"].clone());
        }
    }
    out
}

fn archive_chats(params: &serde_json::Value) -> Vec<(String, i64)> {
    params["chats"].as_array().unwrap().iter()
        .map(|chat| (chat["chatId"].as_str().unwrap().to_owned(), chat["minIdleSeconds"].as_i64().unwrap())).collect()
}

#[gpui::test]
fn tracker_clean_all_confirms_a_summary_then_archives_only_chats_that_are_safe(cx: &mut TestAppContext) {
    use mast_engine::work_outline_health::{HealthBlock, HealthChatFacts, HealthEntry};
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 4);
    let (mut sent, _runtime) = record_engine(&shell, vcx);
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            for chat in state.chats.iter_mut().filter(|chat| chat.id.starts_with("w-")) {
                chat.title = Some("Mast development queue check".into());
            }
        });
        shell.open_team_tab(cx);
    });
    draw(vcx);
    shell.update(vcx, |shell, cx| shell.load_tracker_health(CHAT_API, cx));
    let facts = |blocked, days: i64| HealthChatFacts { idle_seconds: days * 86_400, blocked, live_children: 0 };
    shell.update(vcx, |shell, cx| {
        shell.set_tracker_health_for_test(
            ["w-0", "w-1", "w-2", "w-3"].into_iter().map(|id| HealthEntry::ChatOnContainer { item_id: API.into(), chat_id: id.into() }).collect(),
            vec![(API.into(), "Project".into())],
        );
        shell.set_tracker_health_chats_for_test(vec![
            ("w-0".into(), facts(Some(HealthBlock::Running), 4)),
            ("w-1".into(), facts(Some(HealthBlock::OpenRequest), 4)),
            ("w-2".into(), facts(None, 4)),
            ("w-3".into(), facts(None, 0)),
        ]);
        cx.notify();
    });
    draw(vcx);
    // Offered even while the list is collapsed; nothing happens until confirmed.
    assert!(!has(vcx, "tracker-cleanup") && !has(vcx, "tracker-cleanup-confirm"));
    click_selector(vcx, "tracker-cleanup-all");
    assert!(has(vcx, "tracker-cleanup-confirm"));
    shell.read_with(vcx, |shell, _| {
        let plan = shell.tracker_clean_all_plan_for_test().unwrap();
        assert_eq!(plan.summary(), "Archive 1 chat");
        assert_eq!(plan.skipped, [
            "Chats kept (running): 1", "Chats kept (has an open Required Action): 1", "Chats kept (idle under a day): 1",
        ]);
    });
    click_selector(vcx, "tracker-cleanup-cancel");
    assert!(!has(vcx, "tracker-cleanup-confirm"));
    vcx.run_until_parked();
    assert!(health_archive_requests(&mut sent).is_empty());
    click_selector(vcx, "tracker-cleanup-all");
    click_selector(vcx, "tracker-cleanup-confirm-apply");
    vcx.run_until_parked();
    let requests = health_archive_requests(&mut sent);
    assert_eq!(requests.len(), 1);
    assert_eq!(archive_chats(&requests[0]), [("w-2".to_owned(), 4 * 86_400 - 60)]);
    // While it runs, Clean all is not offered again.
    assert!(!has(vcx, "tracker-cleanup-all"));
}

#[gpui::test]
fn tracker_links_a_project_from_the_space_account(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.space_settings.linear_enabled = true;
        snapshot.space_settings.linear_account_id = Some("account-two".into());
    });
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.work_outline_actions_override = Some(crate::work_outline::WorkOutlineActions::scripted(std::sync::Arc::new(move |method, params| {
                recorded.lock().unwrap().push((method.to_owned(), params.clone()));
                Ok(if method == "LinearMyProjects" {
                    serde_json::json!([{ "id":"project-id", "name":"Harbor", "url":"https://linear.app/example/project/harbor-123" }])
                } else { serde_json::json!({"referenceId":"reference"}) })
            })));
        });
        shell.open_team_tab(cx);
    });
    draw(vcx);
    click_selector(vcx, "tracker-link-project");
    vcx.run_until_parked();
    draw(vcx);
    click_selector(vcx, "tracker-project-project-id");
    vcx.run_until_parked();
    let calls = calls.lock().unwrap();
    let query = calls
        .iter()
        .find(|(method, _)| method == "LinearMyProjects")
        .unwrap();
    assert_eq!(query.1["spaceId"], "s1");
    assert_eq!(query.1["expectedAccountId"], "account-two");
    let link = calls
        .iter()
        .find(|(_, params)| params["op"] == "linkLinearProject")
        .unwrap();
    assert_eq!(link.1["workItemId"], API);
    assert_eq!(link.1["project"]["id"], "project-id");
    assert!(
        !calls
            .iter()
            .any(|(method, _)| method.starts_with("Linear") && method != "LinearMyProjects")
    );
}

#[gpui::test]
fn tracker_resolves_a_project_url_before_loading_issues(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.space_settings.linear_enabled = true;
        snapshot.space_settings.linear_account_id = Some("account-two".into());
        snapshot.external_references.push(serde_json::from_value(serde_json::json!({
            "id":"project-ref", "spaceId":"s1", "workItemId":API, "provider":"linear", "referenceType":"project",
            "externalId":"harbor-123abc", "url":"https://linear.app/example/project/harbor-123abc"
        })).unwrap());
    });
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.work_outline_actions_override = Some(crate::work_outline::WorkOutlineActions::scripted(std::sync::Arc::new(move |method, params| {
                recorded.lock().unwrap().push((method.to_owned(), params.clone()));
                Ok(if method == "LinearMyProjects" {
                    serde_json::json!([{ "id":"12345678-1234-1234-1234-123456789abc", "name":"Harbor", "url":"https://linear.app/example/project/harbor-123abc" }])
                } else { serde_json::json!({"issues": [], "hasMore": false}) })
            })));
        });
        shell.open_team_tab(cx);
    });
    vcx.run_until_parked();
    let calls = calls.lock().unwrap();
    let project = calls
        .iter()
        .find(|(method, params)| method == "LinearProjectIssuePage" && params["cachedOnly"] != true)
        .unwrap();
    assert_eq!(
        project.1["projectId"],
        "12345678-1234-1234-1234-123456789abc"
    );
    assert_eq!(project.1["expectedAccountId"], "account-two");
    assert_eq!(project.1["includeCompleted"], true);
}

#[gpui::test]
fn tracker_loads_a_project_linked_while_it_is_open(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.space_settings.linear_enabled = true
    });
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.work_outline_actions_override = Some(crate::work_outline::WorkOutlineActions::scripted(std::sync::Arc::new(move |method, params| {
                recorded.lock().unwrap().push((method.to_owned(), params.clone()));
                Ok(serde_json::json!({"issues": [], "hasMore": false}))
            })));
        });
        shell.open_team_tab(cx);
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            snapshot.external_references.push(serde_json::from_value(serde_json::json!({
                "id":"project-ref", "spaceId":"s1", "workItemId":API, "provider":"linear", "referenceType":"project",
                "externalId":"12345678-1234-1234-1234-123456789abc"
            })).unwrap());
            snapshot.revision += 1;
            state.set_work_outline_for_test(snapshot);
        });
        shell.refresh_tracker_source_if_changed(cx);
    });
    vcx.run_until_parked();
    let calls = calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|(method, params)| method == "LinearProjectIssuePage"
                && params["includeCompleted"] == true)
    );
}

#[gpui::test]
fn tracker_collapses_a_large_done_group(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 7);
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            for item in snapshot.items.iter_mut().filter(|item| item.id != API) {
                item.effective_status = mast_proto::WorkItemEffectiveStatus::Completed;
            }
            snapshot.revision += 1;
            state.set_work_outline_for_test(snapshot);
        });
        shell.open_team_tab(cx);
        let facts = shell.team_facts(CHAT_API, cx);
        assert_eq!(
            facts
                .workers
                .iter()
                .filter(|worker| worker.state == crate::team::WorkerState::Done)
                .count(),
            7
        );
    });
    draw(vcx);
    assert!(has(vcx, "tracker-section-stage-done"));
    assert!(!has(vcx, "team-worker-w-0"));
    click_selector(vcx, "tracker-section-stage-done");
    assert!(has(vcx, "team-worker-w-0"));
    // Expanding a section that starts collapsed is saved as an explicit choice.
    shell.read_with(vcx, |shell, _| {
        assert_eq!(shell.settings.tracker_sections.get("stage-done"), Some(&false));
    });
    click_selector(vcx, "tracker-section-stage-done");
    assert!(!has(vcx, "team-worker-w-0"));
}

#[gpui::test]
fn tracker_sections_collapse_one_at_a_time_and_stay_that_way(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, queued_project_without_prs(true));
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    // An existing profile has no saved sections: everything is expanded.
    shell.read_with(vcx, |shell, _| assert!(shell.settings.tracker_sections.is_empty()));
    assert!(has(vcx, "tracker-section-queue") && has(vcx, "tracker-section-not-queued"));
    assert!(has(vcx, "tracker-item-q-queued") && has(vcx, "tracker-queue-add-q-loose"));

    // Collapse the Queue. Its rows go; its pause button and the Not queued rows stay.
    click_selector(vcx, "tracker-section-queue");
    assert!(!has(vcx, "tracker-item-q-queued") && !has(vcx, "tracker-queue-remove-0"));
    assert!(has(vcx, "tracker-queue-pause-toggle"));
    assert!(has(vcx, "tracker-queue-add-q-loose"));

    // Another tab and back: still collapsed, and nothing else changed.
    shell.update(vcx, |shell, cx| shell.set_right_active(RightSurface::Picker, cx));
    draw(vcx);
    shell.update(vcx, |shell, cx| shell.set_right_active(RightSurface::Team, cx));
    draw(vcx);
    assert!(!has(vcx, "tracker-item-q-queued"));
    assert!(has(vcx, "tracker-queue-add-q-loose"));
    let saved = shell.read_with(vcx, |shell, _| shell.settings.clone());
    assert_eq!(saved.tracker_sections, std::collections::BTreeMap::from([("queue".to_string(), true)]));
    // It reached the store that writes the profile file, and a later unrelated
    // save from the Shell keeps it.
    assert_eq!(vcx.read(crate::settings::current).tracker_sections, saved.tracker_sections);
    shell.update(vcx, |shell, cx| {
        shell.settings.sidebar_width += 1.0;
        shell.schedule_save(cx);
    });
    let saved = shell.read_with(vcx, |shell, _| shell.settings.clone());
    assert_eq!(vcx.read(crate::settings::current).tracker_sections, saved.tracker_sections);
    assert_eq!(saved.tracker_sections.get("queue"), Some(&true));

    // Restart: the saved file brings back only the Queue's choice.
    let dir = tempfile::tempdir().unwrap();
    saved.save(dir.path()).unwrap();
    let restored = crate::settings::UiSettings::load_for_startup(dir.path());
    let (shell, vcx) = mount(cx, CHAT_API, true, queued_project_without_prs(true));
    shell.update(vcx, |shell, cx| {
        shell.settings = restored;
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(!has(vcx, "tracker-item-q-queued"));
    assert!(has(vcx, "tracker-queue-add-q-loose"));

    // Not queued collapses on its own and leaves the Queue as it was.
    click_selector(vcx, "tracker-section-not-queued");
    assert!(!has(vcx, "tracker-queue-add-q-loose") && !has(vcx, "tracker-item-q-queued"));
    assert!(has(vcx, "tracker-section-not-queued") && has(vcx, "tracker-queue-pause-toggle"));
    // Expanding the Queue again restores its rows without touching Not queued.
    click_selector(vcx, "tracker-section-queue");
    assert!(has(vcx, "tracker-item-q-queued") && !has(vcx, "tracker-queue-add-q-loose"));
}

#[gpui::test]
fn tracker_lifecycle_groups_collapse_independently(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 2);
    // A worker chat on the coordinator's own item is listed under Other chats.
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            let mut snapshot = (**state.work_outline("s1").snapshot().unwrap()).clone();
            snapshot.items.iter_mut().find(|item| item.id == API).unwrap().chat_ids.push("w-extra".into());
            snapshot.revision += 1;
            state.set_work_outline_for_test(snapshot);
            let mut extra = chat("w-extra");
            extra.parent_chat_id = Some(CHAT_API.into());
            state.chats.push(extra);
        });
    });
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(has(vcx, "tracker-section-stage-building") && has(vcx, "team-worker-w-0"));
    click_selector(vcx, "tracker-section-stage-building");
    assert!(!has(vcx, "team-worker-w-0") && !has(vcx, "team-worker-w-1"));
    assert!(has(vcx, "tracker-section-stage-building"));
    shell.read_with(vcx, |shell, _| {
        assert_eq!(shell.settings.tracker_sections.len(), 1);
        assert!(shell.tracker_section_collapsed("stage-building", false));
        assert!(!shell.tracker_section_collapsed("stage-done", false));
    });
    // Other chats starts collapsed, as it did before sections were saved, and
    // opens on its own without touching Building.
    assert!(has(vcx, "tracker-section-other-chats"));
    assert!(!has(vcx, "team-worker-w-extra"));
    click_selector(vcx, "tracker-section-other-chats");
    assert!(has(vcx, "team-worker-w-extra"));
    assert!(!has(vcx, "team-worker-w-0"));
    click_selector(vcx, "tracker-section-stage-building");
    assert!(has(vcx, "team-worker-w-0") && has(vcx, "team-worker-w-extra"));
}

#[gpui::test]
fn tracker_remains_selected_when_expanded(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 2);
    shell.update(vcx, |shell, cx| {
        shell.open_team_tab(cx);
        shell.toggle_right_pane_expand(cx);
    });
    draw(vcx);
    shell.read_with(vcx, |shell, cx| {
        assert!(shell.right_pane_expanded);
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Team);
    });
    assert!(has(vcx, "team-worker-w-0"));
}

fn worker_asks(snapshot: &mut WorkOutlineSnapshot) {
    snapshot
        .user_action_requests
        .push(request("a1", "s1", API, "w-1", "Which database?"));
    let mut handoff = request("a2", "s1", API, "w-2", "Work delivered");
    handoff.kind = Some(mast_proto::work_outline_user_actions::UserActionKind::Handoff);
    snapshot.user_action_requests.push(handoff);
}

#[gpui::test]
fn a_chat_without_workers_has_no_team_tab_and_the_key_does_nothing(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_LOOSE_A, false, |_| {});
    assert!(!has(vcx, "right-surface-team-tab"));
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    shell.read_with(vcx, |shell, cx| {
        assert_ne!(shell.resolved_right_active(cx), RightSurface::Team);
        assert!(!shell.right_pane_open(cx));
    });
}

#[gpui::test]
fn the_team_tab_lists_a_worker_per_row_with_state_activity_and_a_roll_up(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, worker_asks);
    add_workers(&shell, vcx, 4);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    shell.read_with(vcx, |shell, cx| {
        assert!(shell.right_pane_open(cx));
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Team);
        // Not an ordinary, closable tab.
        assert!(shell.right_surface_rows(cx).is_empty());
        assert_eq!(shell.closable_right_surface(cx), None);
    });
    let facts = shell.update(vcx, |shell, cx| shell.team_facts(CHAT_API, cx));
    assert_eq!(facts.workers.len(), 4);
    assert_eq!(
        facts.roll_up(),
        "◌ 1 working \u{b7} ! 1 waiting on you \u{b7} → 1 ready to merge"
    );
    // Waiting first, then ready to merge, working, idle.
    let ids: Vec<_> = facts.workers.iter().map(|w| w.chat_id.as_str()).collect();
    assert_eq!(ids, ["w-1", "w-2", "w-0", "w-3"]);
}

#[gpui::test]
fn tracker_orders_worker_lifecycle_lines(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 2);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    let heading = vcx.debug_bounds("tracker-heading-w-0").unwrap();
    let progress = vcx.debug_bounds("tracker-progress-w-0").unwrap();
    let identity = vcx.debug_bounds("tracker-identity-w-0").unwrap();
    let activity = vcx.debug_bounds("team-worker-activity-w-0").unwrap();
    let actions = vcx.debug_bounds("tracker-actions-w-0").unwrap();
    assert!(heading.bottom() <= progress.top());
    assert!(progress.bottom() <= identity.top());
    assert!(identity.bottom() <= activity.top());
    assert!(activity.bottom() <= actions.top());
}

#[gpui::test]
fn tracker_shows_state_specific_actions(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 3);
    shell.update(vcx, |shell, cx| {
        let status: mast_engine::pr_ticket_cache::ChatLinkStatus = serde_json::from_value(serde_json::json!({
            "isWorktree": false, "pr": {"number": 57, "url": "https://github.com/example/project/pull/57", "state": "OPEN",
                "isDraft": false, "reviewDecision": "REVIEW_REQUIRED", "reviewers": [], "checks": "passing",
                "title": "Review", "branch": "feature/w-2", "headSha": "abc", "mergeable": "mergeable",
                "hasReviewerRequested": true, "additions": 312, "deletions": 48}
        })).unwrap();
        shell.chat_header_links.insert("w-2".into(), ChatHeaderLinkCache { checkout: None, status: Some(status), fetched_at: Instant::now() });
        shell.board_links_revision = shell.board_links_revision.wrapping_add(1);
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(has(vcx, "tracker-answer-w-1"));
    assert!(has(vcx, "team-pr-w-2-0"));
    assert!(has(vcx, "tracker-review-w-2"));
    assert!(!has(vcx, "tracker-merge-w-2"));
    assert!(has(vcx, "team-worker-changes-w-0"));
    assert!(!has(vcx, "team-worker-changes-w-1"));
}

#[gpui::test]
fn tracker_resolves_merged_review_without_hiding_other_questions(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        snapshot.user_action_requests.push(request(
            "review",
            "s1",
            API,
            "w-0",
            "Ready for review: example",
        ));
        snapshot.user_action_requests.push(request(
            "question",
            "s1",
            API,
            "w-1",
            "Which calendar?",
        ));
    });
    add_workers(&shell, vcx, 2);
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.work_outline_actions_override = Some(crate::work_outline::WorkOutlineActions::scripted(std::sync::Arc::new(move |method, params| {
                recorded.lock().unwrap().push((method.to_owned(), params.clone()));
                Ok(serde_json::json!({}))
            })));
        });
        for index in 0..2 {
            let status: mast_engine::pr_ticket_cache::ChatLinkStatus = serde_json::from_value(serde_json::json!({
                "isWorktree": false, "pr": {"number": 57 + index, "url": format!("https://github.com/example/project/pull/{}", 57 + index), "state": "MERGED",
                    "isDraft": false, "reviewers": [], "mergeable": "mergeable", "hasReviewerRequested": false}
            })).unwrap();
            shell.chat_header_links.insert(format!("w-{index}"), ChatHeaderLinkCache { checkout: None, status: Some(status), fetched_at: Instant::now() });
        }
        shell.board_links_revision = shell.board_links_revision.wrapping_add(1);
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(!has(vcx, "tracker-answer-w-0"));
    assert!(has(vcx, "tracker-answer-w-1"));
    click_selector(vcx, "tracker-cleanup-toggle");
    draw(vcx);
    click_selector(vcx, "cleanup-resolve-request-0");
    vcx.run_until_parked();
    let calls = calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|(_, params)| params["op"] == "resolveUserActionRequest"
                && params["userActionRequestId"] == "review"
                && params["note"] == "PR merged")
    );
}

#[gpui::test]
fn tracker_offers_merge_for_approved_passing_pr_without_handoff(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 1);
    shell.update(vcx, |shell, cx| {
        let status: mast_engine::pr_ticket_cache::ChatLinkStatus = serde_json::from_value(serde_json::json!({
            "isWorktree": false, "pr": {"number": 57, "url": "https://github.com/example/project/pull/57", "state": "OPEN",
                "isDraft": false, "reviewDecision": "APPROVED", "reviewers": [], "checks": "passing",
                "branch": "feature/w-0", "mergeable": "mergeable", "hasReviewerRequested": false}
        })).unwrap();
        shell.chat_header_links.insert("w-0".into(), ChatHeaderLinkCache { checkout: None, status: Some(status), fetched_at: Instant::now() });
        shell.board_links_revision = shell.board_links_revision.wrapping_add(1);
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(has(vcx, "tracker-review-w-0"));
    assert!(has(vcx, "tracker-merge-w-0"));
}

#[gpui::test]
fn tracker_places_an_open_branch_review_in_code_review(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 1);
    let target = mast_proto::ReviewTarget::Branch {
        repo: Some("example/project".into()),
        checkout_path: Some("/repo".into()),
        branch: Some("feature/w-0".into()),
        base_ref: Some("main".into()),
        base_sha: Some("base".into()),
        head_sha: "head".into(),
    };
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.chats.push(chat("review"));
            state.board_status_revision = state.board_status_revision.wrapping_add(1);
        });
        shell
            .branch_review_targets
            .insert("w-0".into(), target.clone());
        shell
            .branch_review_chats
            .insert(format!("s1\0{}", target.state_key()), "review".into());
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(has(vcx, "tracker-stage-Code review-true"));
}

#[gpui::test]
fn tracker_restores_branch_review_from_cached_targets(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 1);
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.chats.push(chat("review"));
            state.board_status_revision = state.board_status_revision.wrapping_add(1);
            let frame = serde_json::from_value(serde_json::json!([
                { "chatId":"w-0", "targets":[{ "path":"/repo", "checkoutId":"checkout", "source":"agent", "selected":true, "branch":"feature/w-0" }] },
                { "chatId":"review", "targets":[], "refTargets":[{ "id":"review-fixture", "kind":"branch", "path":"/repo",
                    "checkoutId":"checkout", "repoName":"example", "label":"Review", "headBranch":"feature/w-0",
                    "headRef":"refs/heads/feature/w-0", "baseBranch":"main", "baseRef":"refs/heads/main" }] }
            ])).unwrap();
            state.set_chat_diff_targets_for_test(frame);
        });
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(has(vcx, "tracker-stage-Code review-true"));
}

#[gpui::test]
fn worker_changes_action_opens_a_diff(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 2);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    let position = vcx
        .debug_bounds("team-worker-changes-w-0")
        .unwrap()
        .center();
    vcx.simulate_click(position, gpui::Modifiers::default());
    draw(vcx);
    shell.read_with(vcx, |shell, _| {
        assert_eq!(shell.diffs.len(), 1);
    });
}

#[gpui::test]
fn team_tab_rows_stack_without_overlap_for_two_and_fifteen_workers(cx: &mut TestAppContext) {
    for n in [2usize, 15] {
        let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
        add_workers(&shell, vcx, n);
        shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
        draw(vcx);
        assert!(has(vcx, "team-rollup"), "{n}");
        let facts = shell.update(vcx, |shell, cx| shell.team_facts(CHAT_API, cx));
        let tab = vcx.debug_bounds("team-tab").unwrap();
        let mut last_bottom = px(0.0);
        for worker in &facts.workers {
            let selector: &'static str =
                Box::leak(format!("team-worker-{}", worker.chat_id).into_boxed_str());
            let bounds = vcx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} ({n})"));
            assert!(
                bounds.top() >= last_bottom,
                "{selector} overlaps the row above"
            );
            assert!(bounds.right() <= tab.right(), "{selector} spills sideways");
            last_bottom = bounds.bottom();
        }
        // Each row names its branch; the waiting and ready ones say more.
        assert!(has(vcx, "team-branch-w-0"));
        assert!(has(vcx, "team-worker-activity-w-0"));
        assert!(!has(vcx, "team-worker-jump-w-2"));
    }
}

#[gpui::test]
fn plain_t_opens_the_team_tab_only_when_the_panel_itself_is_focused(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 3);
    let panel = shell.update(vcx, |shell, cx| shell.team_panel_focus(cx));
    // Focus in the composer (a text field): `t` types, it does not navigate.
    let composer = shell.read_with(vcx, |shell, cx| shell.composer.read(cx).focus_handle(cx));
    vcx.update(|window, cx| window.focus(&composer, cx));
    vcx.simulate_keystrokes("t");
    shell.read_with(vcx, |shell, cx| {
        assert_ne!(shell.resolved_right_active(cx), RightSurface::Team)
    });
    // Panel focused: `t` opens Team.
    vcx.update(|window, cx| window.focus(&panel, cx));
    draw(vcx);
    vcx.simulate_keystrokes("t");
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Team)
    });
}

#[gpui::test]
fn team_panel_rebind_replaces_the_default(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 3);
    let mut keymap = crate::settings::KeymapConfig::default();
    keymap.set(
        crate::settings::ShortcutId::OpenTeamTabFromPanel,
        "y".into(),
    );
    vcx.update(|_, cx| crate::shell::apply_keymap(cx, &keymap, Default::default()));
    let panel = shell.update(vcx, |shell, cx| shell.team_panel_focus(cx));
    vcx.update(|window, cx| window.focus(&panel, cx));
    draw(vcx);
    vcx.simulate_keystrokes("t");
    shell.read_with(vcx, |shell, cx| {
        assert_ne!(shell.resolved_right_active(cx), RightSurface::Team)
    });
    vcx.simulate_keystrokes("y");
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Team)
    });
}

#[gpui::test]
fn plain_t_does_nothing_on_a_chat_without_workers(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_LOOSE_A, true, |_| {});
    let panel = shell.update(vcx, |shell, cx| shell.team_panel_focus(cx));
    vcx.update(|window, cx| window.focus(&panel, cx));
    draw(vcx);
    vcx.simulate_keystrokes("t");
    shell.read_with(vcx, |shell, cx| {
        assert_ne!(shell.resolved_right_active(cx), RightSurface::Team)
    });
}

#[gpui::test]
fn the_team_hotkey_avoids_the_browsers_new_tab(_cx: &mut TestAppContext) {
    // Cmd+T is New tab in the side panel's browser, so the Team tab takes
    // Opt+Cmd+T; no customizable default claims either.
    assert_eq!(OPEN_TEAM_TAB_COMBO, "mod-alt-t");
    assert!(
        crate::settings::ShortcutId::ALL
            .into_iter()
            .all(|id| id.default_combo() != "mod-t"
                && (id == crate::settings::ShortcutId::OpenTeamTab
                    || id.default_combo() != OPEN_TEAM_TAB_COMBO))
    );
}

// ----------------------------------------------------------------------
// Linked PR rows: title, state, checks, review from the PR cache
// ----------------------------------------------------------------------
mod pr_rows {
    use super::*;
    use crate::github::fixture::summary;
    use mast_proto::{PullRequestCiStatus, PullRequestReviewDecision, PullRequestState};

    fn cache(
        shell: &Entity<Shell>,
        vcx: &mut VisualTestContext,
        edit: impl FnOnce(&mut mast_proto::PullRequestStatusSummary),
        partial: bool,
    ) {
        shell.update(vcx, |shell, cx| {
            let mut status = summary("app", 7, "Add auth to the API");
            edit(&mut status);
            shell.state.update(cx, |state, _| {
                state.github.set_ready_for_test(status, partial)
            });
            cx.notify();
        });
        draw(vcx);
    }

    fn facts(
        shell: &Entity<Shell>,
        vcx: &mut VisualTestContext,
    ) -> crate::chat_item::ChatItemFacts {
        shell.read_with(vcx, |shell, cx| {
            shell.chat_item_facts(CHAT_API, cx).unwrap()
        })
    }

    #[gpui::test]
    fn an_uncached_pr_reads_pr_number_until_its_status_arrives(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
        expand(&shell, vcx, &["links"]);
        let pr = facts(&shell, vcx).prs.remove(0);
        assert_eq!((pr.number, pr.title.clone(), pr.status), (7, None, None));
        assert!(has(vcx, "chat-item-pr-0"));
        assert!(has(vcx, "chat-item-pr-0-title"));
        for gone in [
            "chat-item-pr-0-state",
            "chat-item-pr-0-checks",
            "chat-item-pr-0-review",
        ] {
            assert!(!has(vcx, gone), "{gone}");
        }
        cache(&shell, vcx, |_| {}, false);
        assert!(has(vcx, "chat-item-pr-0-state"));
    }

    #[gpui::test]
    fn a_cached_pr_shows_title_state_checks_and_review(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
        expand(&shell, vcx, &["links"]);
        cache(
            &shell,
            vcx,
            |s| {
                s.ci_status = PullRequestCiStatus::Failing;
                s.review_decision = Some(PullRequestReviewDecision::ChangesRequested);
            },
            false,
        );
        let pr = facts(&shell, vcx).prs.remove(0);
        assert_eq!(pr.title.as_deref(), Some("Add auth to the API"));
        let status = pr.status.unwrap();
        assert_eq!(status.state, PullRequestState::Open);
        assert_eq!(status.checks, Some(PullRequestCiStatus::Failing));
        assert_eq!(
            status.review,
            Some(PullRequestReviewDecision::ChangesRequested)
        );
        assert_eq!(pr.repo().as_deref(), Some("acme/app"));
        for id in [
            "chat-item-pr-0-title",
            "chat-item-pr-0-state",
            "chat-item-pr-0-checks",
            "chat-item-pr-0-review",
        ] {
            assert!(has(vcx, id), "{id}");
        }
        // Merged with no checks and no review decision drops those slots.
        cache(
            &shell,
            vcx,
            |s| {
                s.state = PullRequestState::Merged;
                s.ci_status = PullRequestCiStatus::None;
                s.review_decision = None;
            },
            false,
        );
        let status = facts(&shell, vcx).prs[0].status.unwrap();
        assert_eq!(status.state, PullRequestState::Merged);
        assert!(has(vcx, "chat-item-pr-0-state"));
        assert!(!has(vcx, "chat-item-pr-0-checks"));
        assert!(!has(vcx, "chat-item-pr-0-review"));
        cache(&shell, vcx, |s| s.state = PullRequestState::Draft, false);
        assert_eq!(
            facts(&shell, vcx).prs[0].status.unwrap().state,
            PullRequestState::Draft
        );
    }

    #[gpui::test]
    fn a_list_row_fills_title_and_state_without_blanking_known_checks(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
        cache(&shell, vcx, |s| s.state = PullRequestState::Closed, true);
        let pr = facts(&shell, vcx).prs.remove(0);
        assert_eq!(pr.title.as_deref(), Some("Add auth to the API"));
        assert_eq!(pr.status.unwrap().state, PullRequestState::Closed);
        // A partial row has no CI detail: nothing is invented.
        assert_eq!(pr.status.unwrap().checks, None);
    }

    #[gpui::test]
    fn a_long_title_truncates_inside_the_tab_and_the_card(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
        expand(&shell, vcx, &["links"]);
        cache(
            &shell,
            vcx,
            |s| s.title = "A very long pull request title ".repeat(30),
            false,
        );
        let tab = vcx.debug_bounds("chat-item-tab").unwrap();
        let row = vcx.debug_bounds("chat-item-pr-0").unwrap();
        let title = vcx.debug_bounds("chat-item-pr-0-title").unwrap();
        assert!(row.right() <= tab.right(), "{row:?} in {tab:?}");
        assert!(title.size.height < px(24.0), "one line: {:?}", title.size);
        // The state still fits next to the clipped title.
        let state = vcx.debug_bounds("chat-item-pr-0-state").unwrap();
        assert!(state.right() <= tab.right());
    }

    #[gpui::test]
    fn the_hover_card_row_shows_the_same_status(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        cache(&shell, vcx, |_| {}, false);
        shell.update(vcx, |shell, cx| {
            shell.toggle_chat_item_card_pin(cx);
        });
        draw(vcx);
        assert!(has(vcx, "chat-item-card-pr-0-title"));
        assert!(has(vcx, "chat-item-card-pr-0-state"));
    }
}

// ----------------------------------------------------------------------
// Orchestrator wakes: Team header, Item tab, unseen marks
// ----------------------------------------------------------------------

fn wake_event(id: &str, worker: &str, unseen: bool) -> mast_proto::WakeEventView {
    mast_proto::WakeEventView {
        id: id.into(),
        worker_chat_id: worker.into(),
        worker_title: None,
        kind: mast_proto::WakeEventKind::Errored,
        summary: format!("{worker} errored ({id})"),
        report: None,
        at_ms: 1,
        delivered: false,
        unseen,
    }
}

fn set_wake_status(
    shell: &Entity<Shell>,
    vcx: &mut VisualTestContext,
    status: mast_proto::WakeStatus,
) {
    shell.update(vcx, |shell, cx| {
        shell.wakes.set(status);
        cx.notify();
    });
    draw(vcx);
}

fn held_status() -> mast_proto::WakeStatus {
    mast_proto::WakeStatus {
        chat_id: CHAT_API.into(),
        is_orchestrator: true,
        held: vec![
            wake_event("e1", "w-1", false),
            wake_event("e2", "w-1", false),
        ],
        hold_reason: Some(mast_proto::WakeHoldReason::Draft),
        recent: vec![
            wake_event("e2", "w-1", true),
            wake_event("e1", "w-1", true),
            wake_event("e0", "w-0", false),
        ],
        unseen_count: 2,
        ..Default::default()
    }
}

#[gpui::test]
fn the_team_header_has_wake_switches_and_shows_held_and_unseen_only_when_present(
    cx: &mut TestAppContext,
) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 3);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(!has(vcx, "team-wake-toggle-enabled"));
    click_selector(vcx, "tracker-wake-settings");
    for key in [
        "enabled",
        "required-actions",
        "errors",
        "worker-reports",
        "awaiting-input",
        "tool-calls-failed",
        "auto-continue",
        "turn-finished",
        "prs",
    ] {
        let selector: &'static str = Box::leak(format!("team-wake-toggle-{key}").into_boxed_str());
        assert!(has(vcx, selector), "{selector}");
    }
    assert!(!has(vcx, "team-wake-held") && !has(vcx, "team-wake-deliver"));
    assert!(!has(vcx, "team-wake-unseen"));

    set_wake_status(&shell, vcx, held_status());
    assert!(has(vcx, "team-wake-held"));
    assert!(has(vcx, "team-wake-held-reason"));
    assert!(has(vcx, "team-wake-deliver"));
    assert!(has(vcx, "team-wake-unseen"));
    // Only the worker with unseen events is marked.
    assert!(has(vcx, "team-worker-unseen-w-1"));
    assert!(!has(vcx, "team-worker-unseen-w-0"));
    assert!(!has(vcx, "team-worker-unseen-w-2"));
    shell.read_with(vcx, |shell, _| {
        assert_eq!(
            shell.unseen_for_worker("w-1"),
            ["w-1 errored (e1)", "w-1 errored (e2)"]
        );
    });
}

#[gpui::test]
fn team_rows_mark_waiting_and_failed_workers(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 3);
    let state = shell.read_with(vcx, |shell, _| shell.state.clone());
    state.update(vcx, |state, _| {
        let failed = state
            .sessions
            .iter_mut()
            .find(|s| s.chat_id == "w-0")
            .unwrap();
        failed.last_turn_tool_failures = Some(mast_proto::ToolFailures {
            count: 2,
            tools: vec!["bind_chat_worktree".into()],
        });
        let mut waiting = state
            .sessions
            .iter()
            .find(|s| s.chat_id == "w-0")
            .unwrap()
            .clone();
        waiting.chat_id = "w-1".into();
        waiting.last_turn_tool_failures = None;
        waiting.status = mast_proto::SessionStatus::AwaitingInput;
        state.sessions.push(waiting);
        state.board_status_revision = state.board_status_revision.wrapping_add(1);
    });
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(has(vcx, "team-worker-stalled-w-0"));
    assert!(has(vcx, "team-worker-stalled-w-1"));
}

#[gpui::test]
fn a_workers_latest_report_is_its_activity_line_and_expands_to_the_full_text(
    cx: &mut TestAppContext,
) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 2);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(has(vcx, "team-worker-activity-w-0") || !has(vcx, "team-worker-report-w-0"));
    let report = |id: &str, text: &str| {
        let mut e = wake_event(id, "w-0", false);
        e.kind = mast_proto::WakeEventKind::WorkerReport;
        e.report = Some(text.into());
        e
    };
    set_wake_status(
        &shell,
        vcx,
        mast_proto::WakeStatus {
            chat_id: CHAT_API.into(),
            is_orchestrator: true,
            // Newest first: the second report wins.
            recent: vec![
                report("r2", "Done: merged the fix\nsecond line of detail"),
                report("r1", "older report"),
            ],
            ..Default::default()
        },
    );
    assert!(has(vcx, "team-worker-activity-w-0"));
    assert!(!has(vcx, "team-worker-report-w-0"), "collapsed by default");
    shell.read_with(vcx, |shell, _| {
        assert_eq!(
            shell.latest_report_for_worker("w-0").as_deref(),
            Some("Done: merged the fix\nsecond line of detail")
        );
        assert_eq!(shell.latest_report_for_worker("w-1"), None);
    });
    shell.update(vcx, |shell, cx| shell.toggle_report_expanded("w-0", cx));
    draw(vcx);
    assert!(
        has(vcx, "team-worker-report-w-0"),
        "expands to the full text"
    );
    shell.update(vcx, |shell, cx| shell.toggle_report_expanded("w-0", cx));
    draw(vcx);
    assert!(!has(vcx, "team-worker-report-w-0"));
}

#[gpui::test]
fn the_item_tab_shows_the_compact_wake_control_only_for_chats_with_workers(
    cx: &mut TestAppContext,
) {
    let (shell, vcx) = mount(cx, CHAT_API, true, worker_asks);
    add_workers(&shell, vcx, 3);
    assert!(has(vcx, "chat-item-wakes"));
    assert!(!has(vcx, "chat-item-wake-held"));
    expand(&shell, vcx, &["wakes"]);
    assert!(has(vcx, "chat-item-wake-toggle-enabled"));
    assert!(has(vcx, "chat-item-wake-toggle-auto-continue"));
    assert!(has(vcx, "chat-item-wake-toggle-prs"));
    set_wake_status(&shell, vcx, held_status());
    assert!(has(vcx, "chat-item-wake-held"));
    assert!(has(vcx, "chat-item-wake-deliver"));

    let (_shell, vcx) = mount(cx, CHAT_LOOSE_A, true, |_| {});
    assert!(!has(vcx, "chat-item-wakes"));
    assert!(!has(vcx, "chat-item-wake-held"));
}

#[gpui::test]
fn toggling_a_wake_switch_updates_the_cached_settings_at_once(cx: &mut TestAppContext) {
    use crate::orchestrator_wakes::WakeToggle;
    let (shell, vcx) = mount(cx, CHAT_API, false, worker_asks);
    add_workers(&shell, vcx, 2);
    shell.update(vcx, |shell, cx| {
        shell.toggle_wake(CHAT_API, WakeToggle::TurnFinished, cx)
    });
    shell.update(vcx, |shell, cx| {
        shell.toggle_wake(CHAT_API, WakeToggle::Enabled, cx)
    });
    shell.update(vcx, |shell, cx| {
        shell.toggle_wake(CHAT_API, WakeToggle::AutoContinue, cx)
    });
    shell.read_with(vcx, |shell, _| {
        let settings = shell.wakes.statuses[CHAT_API].settings;
        assert!(
            !settings.turn_finished && !settings.enabled && settings.errors && settings.auto_continue
        );
    });
}

// ---- archive finished workers ----------------------------------------------------

/// Idle workers `w-1..` whose Ready to merge was resolved: finished.
fn finished_workers(count: usize) -> impl FnOnce(&mut WorkOutlineSnapshot) + 'static {
    move |snapshot| {
        for i in 1..=count {
            let mut done = request(
                &format!("done-{i}"),
                "s1",
                API,
                &format!("w-{i}"),
                &format!("Ready to merge: feature/w-{i}"),
            );
            done.state = mast_proto::UserActionRequestState::Resolved;
            snapshot.user_action_requests.push(done);
        }
    }
}

type Sent = tokio::sync::mpsc::Receiver<String>;

/// Give the shell an engine that records every request it is sent.
fn record_engine(
    shell: &Entity<Shell>,
    vcx: &mut VisualTestContext,
) -> (Sent, tokio::runtime::Runtime) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let guard = runtime.enter();
    let (out, requests) = tokio::sync::mpsc::channel(16);
    let (_replies, inbound) = tokio::sync::mpsc::channel(16);
    let engine =
        crate::state::EngineHandle::from_test_client(mast_rpc::RpcClient::new(out, inbound));
    std::mem::forget(_replies);
    shell.update(vcx, |shell, cx| {
        shell
            .state
            .update(cx, |state, _| state.set_test_engine(engine));
    });
    drop(guard);
    (requests, runtime)
}

#[gpui::test]
fn transcript_retry_dispatches_the_error_identity(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
    let (mut sent, _runtime) = record_engine(&shell, vcx);
    shell.update(vcx, |shell, cx| {
        let transcript = shell.transcript.clone();
        shell.on_transcript_event(
            transcript,
            &crate::transcript::TranscriptEvent::RetryErroredTurn {
                chat_id: CHAT_API.into(),
                error_entry_id: "failure-entry".into(),
            },
            cx,
        );
    });
    vcx.run_until_parked();
    let calls: Vec<serde_json::Value> = std::iter::from_fn(|| sent.try_recv().ok())
        .map(|line| serde_json::from_str(&line).unwrap())
        .collect();
    assert!(calls.iter().any(
        |call| call["method"] == mast_rpc::methods::RETRY_ERRORED_TURN
            && call["params"]["chatId"] == CHAT_API
            && call["params"]["errorEntryId"] == "failure-entry"
    ));
}

/// The `setChatArchived` requests sent so far (other traffic is skipped).
fn archive_requests(sent: &mut Sent) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    while let Ok(line) = sent.try_recv() {
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        if request["params"]["op"] == "setChatArchived" {
            out.push(request["params"].clone());
        }
    }
    out
}

fn click_selector(vcx: &mut VisualTestContext, selector: &'static str) {
    let at = vcx.debug_bounds(selector).unwrap().center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
}

#[gpui::test]
fn the_team_tab_offers_archive_finished_and_suggests_it_from_three(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, finished_workers(2));
    add_workers(&shell, vcx, 4);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    // Two finished: the button, no suggestion; the working worker is not one.
    assert!(has(vcx, "team-archive-finished"));
    assert!(!has(vcx, "team-archive-suggest"));
    assert!(has(vcx, "team-archivable-w-1") && has(vcx, "team-archivable-w-2"));
    assert!(!has(vcx, "team-archivable-w-0") && !has(vcx, "team-archivable-w-3"));
    // An idle worker the automatic pass keeps says why; working and
    // manually-archivable workers show no retention label.
    assert!(has(vcx, "team-retained-w-3"));
    assert!(!has(vcx, "team-retained-w-0") && !has(vcx, "team-retained-w-1"));
    let (shell, vcx) = mount(cx, CHAT_API, true, finished_workers(3));
    add_workers(&shell, vcx, 4);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(has(vcx, "team-archive-suggest"));
    // None finished: nothing to offer.
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 4);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    assert!(!has(vcx, "team-archive-finished") && !has(vcx, "team-archive-suggest"));
}

#[gpui::test]
fn archive_finished_confirms_then_archives_exactly_the_listed_chats(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, finished_workers(3));
    add_workers(&shell, vcx, 4);
    let (mut sent, _runtime) = record_engine(&shell, vcx);
    shell.update(vcx, |shell, cx| shell.open_team_tab(cx));
    draw(vcx);
    click_selector(vcx, "team-archive-finished");
    // Asks first, listing the chats; nothing is sent yet.
    assert!(has(vcx, "team-archive-confirm"));
    for id in ["w-1", "w-2", "w-3"] {
        let selector: &'static str =
            Box::leak(format!("team-archive-confirm-row-{id}").into_boxed_str());
        assert!(has(vcx, selector), "{id}");
    }
    assert!(!has(vcx, "team-archive-confirm-row-w-0"));
    vcx.run_until_parked();
    assert!(archive_requests(&mut sent).is_empty());
    // Cancel leaves everything as it was.
    click_selector(vcx, "team-archive-confirm-cancel");
    assert!(!has(vcx, "team-archive-confirm") && has(vcx, "team-archive-finished"));
    assert!(archive_requests(&mut sent).is_empty());
    // Confirm archives each listed chat.
    click_selector(vcx, "team-archive-finished");
    click_selector(vcx, "team-archive-confirm-go");
    vcx.run_until_parked();
    let mut archived: Vec<String> = Vec::new();
    for params in archive_requests(&mut sent) {
        assert_eq!(params["archived"], true);
        archived.push(params["chatId"].as_str().unwrap().to_owned());
    }
    archived.sort();
    assert_eq!(archived, ["w-1", "w-2", "w-3"]);
    assert!(!has(vcx, "team-archive-confirm"));
}

#[gpui::test]
fn archived_workers_leave_the_rows_and_a_toggle_brings_them_back(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, finished_workers(1));
    add_workers(&shell, vcx, 3);
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state
                .chats
                .iter_mut()
                .find(|c| c.id == "w-1")
                .unwrap()
                .archived = true;
            state.board_status_revision = state.board_status_revision.wrapping_add(1);
        });
        shell.open_team_tab(cx);
    });
    draw(vcx);
    assert!(has(vcx, "team-worker-w-0") && has(vcx, "team-worker-w-2"));
    assert!(
        !has(vcx, "team-worker-w-1"),
        "archived workers drop out of the rows"
    );
    // Archived and thus no longer offered for archiving.
    assert!(!has(vcx, "team-archive-finished"));
    assert!(has(vcx, "team-show-archived"));
    assert!(!has(vcx, "team-archived-w-1"));
    click_selector(vcx, "team-show-archived");
    assert!(has(vcx, "team-archived-w-1") && has(vcx, "team-unarchive-w-1"));
    click_selector(vcx, "team-show-archived");
    assert!(!has(vcx, "team-archived-w-1"));
    // Archived workers don't count in the tab chip or the roll-up.
    let facts = shell.update(vcx, |shell, cx| shell.team_facts(CHAT_API, cx));
    assert_eq!(facts.live().count(), 2);
    assert_eq!(facts.archived().count(), 1);
}

#[gpui::test]
fn unarchive_sends_set_chat_archived_false(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    add_workers(&shell, vcx, 2);
    let (mut sent, _runtime) = record_engine(&shell, vcx);
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state
                .chats
                .iter_mut()
                .find(|c| c.id == "w-1")
                .unwrap()
                .archived = true;
            state.board_status_revision = state.board_status_revision.wrapping_add(1);
        });
        shell.open_team_tab(cx);
        shell.wakes.show_archived.insert(CHAT_API.into());
    });
    draw(vcx);
    click_selector(vcx, "team-unarchive-w-1");
    vcx.run_until_parked();
    let requests = archive_requests(&mut sent);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["chatId"], "w-1");
    assert_eq!(requests[0]["archived"], false);
}

#[gpui::test]
fn required_action_structured_references_render_on_the_item_tab(cx: &mut TestAppContext) {
    use mast_proto::work_outline_user_actions::{
        RequestFact, RequestLink, RequestLinkTarget, UserActionKind,
    };
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        let mut action = request("structured", "s1", API, CHAT_API, "Review the change");
        action.kind = Some(UserActionKind::Handoff);
        action.facts = vec![
            RequestFact {
                label: "Branch".into(),
                value: "feature/test".into(),
            },
            RequestFact {
                label: "Tests".into(),
                value: "Passed".into(),
            },
        ];
        action.links = vec![
            RequestLink {
                label: "Artifact".into(),
                target: RequestLinkTarget::Artifact {
                    id: "0123456789abcdef0123456789abcdef".into(),
                },
            },
            RequestLink {
                label: "File".into(),
                target: RequestLinkTarget::File {
                    path: "src/lib.rs".into(),
                    line: Some(4),
                },
            },
            RequestLink {
                label: "Diff".into(),
                target: RequestLinkTarget::Diff { branch: None },
            },
            RequestLink {
                label: "PR".into(),
                target: RequestLinkTarget::Pr {
                    url: "https://github.com/acme/repo/pull/1".into(),
                },
            },
            RequestLink {
                label: "Chat".into(),
                target: RequestLinkTarget::Chat { id: CHAT_UI.into() },
            },
            RequestLink {
                label: "URL".into(),
                target: RequestLinkTarget::Url {
                    url: "https://example.com".into(),
                },
            },
        ];
        snapshot.user_action_requests.push(action);
    });
    let state = shell.read_with(vcx, |shell, _| shell.state.clone());
    state.update(vcx, |state, _| {
        state.set_test_artifacts(crate::artifacts::tests::snapshot(vec![
            crate::artifacts::tests::artifact(
                "0123456789abcdef0123456789abcdef",
                "Plan",
                mast_proto::ArtifactKind::Markdown,
            ),
        ]))
    });
    shell.update(vcx, |shell, _| shell.route = Route::Chat);
    draw(vcx);
    assert!(has(vcx, "required-actions-fact-structured-Branch"));
    assert!(has(vcx, "required-actions-fact-structured-Tests"));
    for selector in [
        "required-actions-link-structured-0",
        "required-actions-link-structured-1",
        "required-actions-link-structured-2",
        "required-actions-link-structured-3",
        "required-actions-link-structured-4",
        "required-actions-link-structured-5",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
    click_selector(vcx, "required-actions-link-structured-0");
    shell.read_with(vcx, |shell, _| {
        assert_eq!(shell.artifact_tabs.tabs.len(), 1)
    });
    click_selector(vcx, "required-actions-link-structured-1");
    vcx.run_until_parked();
    shell.read_with(vcx, |shell, _| {
        assert!(
            shell
                .file_surface_paths
                .values()
                .any(|path| path == "src/lib.rs")
        )
    });
    click_selector(vcx, "required-actions-link-structured-2");
    vcx.run_until_parked();
    shell.read_with(vcx, |shell, _| assert!(!shell.diffs.is_empty()));
    click_selector(vcx, "required-actions-link-structured-3");
    shell.read_with(vcx, |shell, _| assert!(!shell.browsers.is_empty()));
    click_selector(vcx, "required-actions-link-structured-5");
    shell.read_with(vcx, |shell, _| assert!(shell.browsers.len() >= 2));
    click_selector(vcx, "required-actions-link-structured-4");
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some(CHAT_UI))
    });
}

#[gpui::test]
fn review_open_uses_the_first_artifact_reference(cx: &mut TestAppContext) {
    use mast_proto::work_outline_user_actions::{RequestLink, RequestLinkTarget, UserActionKind};
    let (shell, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        let mut action = request("review-reference", "s1", API, CHAT_API, "Review plan");
        action.kind = Some(UserActionKind::Review);
        action.links.push(RequestLink {
            label: "Plan".into(),
            target: RequestLinkTarget::Artifact {
                id: crate::artifacts::tests::id(1),
            },
        });
        snapshot.user_action_requests.push(action);
    });
    let state = shell.read_with(vcx, |shell, _| shell.state.clone());
    state.update(vcx, |state, _| {
        state.set_test_artifacts(crate::artifacts::tests::snapshot(vec![
            crate::artifacts::tests::artifact(
                &crate::artifacts::tests::id(1),
                "Plan",
                mast_proto::ArtifactKind::Markdown,
            ),
        ]))
    });
    draw(vcx);
    click_selector(vcx, "required-actions-open-review-reference");
    shell.read_with(vcx, |shell, _| {
        assert_eq!(shell.artifact_tabs.tabs.len(), 1)
    });
}

#[gpui::test]
fn required_action_detail_renders_markdown(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        let mut action = request("markdown-detail", "s1", API, CHAT_API, "Review details");
        action.kind = Some(mast_proto::work_outline_user_actions::UserActionKind::Handoff);
        action.info = Some("**Details** https://github.com/acme/repo/pull/2".into());
        snapshot.user_action_requests.push(action);
    });
    click_selector(vcx, "required-actions-info-toggle-markdown-detail");
    assert!(has(vcx, "required-actions-info-markdown-detail"));
}

/// One assistant turn of spawn chips, `(id, status)` each.
fn spawn_turn(chips: &[(&str, &str)]) -> mast_doc::SessionMessageEntry {
    let parts: Vec<_> = chips
        .iter()
        .map(|(id, status)| {
            serde_json::json!({
                "id": id, "kind": "tool", "resolved": true,
                "call": {"kind": "unknown", "name": format!("Agent: task {id}")},
                "subagentRef": format!("{CHAT_API}--sub--{id}"), "subagentStatus": status,
            })
        })
        .collect();
    serde_json::from_value(serde_json::json!({
        "id": "turn", "role": "assistant", "parts": parts,
        "createdAt": 0, "deviceId": "device", "status": "streaming",
    }))
    .unwrap()
}

#[gpui::test]
fn the_item_tab_lists_running_subagents_and_opens_them_like_their_chips(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    assert!(
        !has(vcx, "chat-item-subagents"),
        "hidden while nothing runs"
    );
    let state = shell.read_with(vcx, |shell, _| shell.state.clone());
    let set = |vcx: &mut VisualTestContext, chips: &[(&str, &str)]| {
        let entry = spawn_turn(chips);
        state.update(vcx, |state, cx| {
            state.apply_transcript(vec![entry]);
            cx.notify();
        });
        draw(vcx);
    };

    set(vcx, &[("a1", "running"), ("a2", "running"), ("a3", "done")]);
    assert!(has(vcx, "chat-item-subagents"));
    assert!(has(vcx, "chat-item-subagent-chat-api--sub--a1"));
    assert!(has(vcx, "chat-item-subagent-chat-api--sub--a2"));
    assert!(!has(vcx, "chat-item-subagent-chat-api--sub--a3"));

    // A row click opens the subagent's tab; a second focuses it.
    for _ in 0..2 {
        click_selector(vcx, "chat-item-subagent-chat-api--sub--a1");
        shell.read_with(vcx, |shell, cx| {
            assert_eq!(shell.subagent_tabs.len(), 1);
            let (&id, tab) = shell.subagent_tabs.iter().next().unwrap();
            assert_eq!(tab.doc_id, "chat-api--sub--a1");
            assert_eq!(tab.title.as_ref(), "task a1");
            assert_eq!(shell.resolved_right_active(cx), RightSurface::Subagent(id));
        });
    }

    // Live: finishing and failing drop rows, and the section with them.
    set(vcx, &[("a1", "done"), ("a2", "running"), ("a3", "done")]);
    assert!(!has(vcx, "chat-item-subagent-chat-api--sub--a1"));
    assert!(has(vcx, "chat-item-subagent-chat-api--sub--a2"));
    set(vcx, &[("a1", "done"), ("a2", "failed"), ("a3", "done")]);
    assert!(!has(vcx, "chat-item-subagents"));
}

#[gpui::test]
fn session_activity_pill_tracks_running_agents_and_dropdown(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
    assert!(!has(vcx, "chat-header-chip-activity"));
    let entry = |count: usize| -> mast_doc::SessionMessageEntry {
        serde_json::from_value(serde_json::json!({
            "id": "turn", "role": "assistant", "createdAt": 0, "deviceId": "d",
            "parts": (0..count).map(|i| serde_json::json!({
                "id": format!("s{i}"), "kind": "tool", "resolved": true,
                "call": {"kind": "unknown", "name": format!("Agent: Worker {i}")},
                "subagentRef": format!("sub-{i}"), "subagentStatus": "running"
            })).collect::<Vec<_>>()
        }))
        .unwrap()
    };
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_transcript(vec![entry(2)]);
            cx.notify();
        })
    });
    draw(vcx);
    assert!(has(vcx, "chat-header-chip-activity"));
    let at = vcx
        .debug_bounds("chat-header-chip-activity")
        .unwrap()
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
    assert!(has(vcx, "session-activity-menu"));
    shell.update(vcx, |shell, cx| {
        assert!(shell.capture_escape_surface(cx));
        assert!(!shell.session_activity_menu.is_open());
        // Cancel the exit tween to exercise row selection independently of
        // the wall-clock animation used by Popup.
        shell.session_activity_menu.open(CHAT_API.into());
        cx.notify();
    });
    draw(vcx);
    let at = vcx.debug_bounds("session-activity-sub-0").unwrap().center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
    shell.read_with(vcx, |shell, _| {
        assert!(
            shell
                .subagent_tabs
                .values()
                .any(|tab| tab.doc_id == "sub-0")
        )
    });
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_transcript(vec![entry(1)]);
            cx.notify();
        })
    });
    draw(vcx);
    assert!(!has(vcx, "session-activity-menu"));
    let at = vcx
        .debug_bounds("chat-header-chip-activity")
        .unwrap()
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
    shell.read_with(vcx, |shell, _| {
        assert_eq!(
            shell.subagent_tabs.len(),
            1,
            "single pill focuses the existing agent tab"
        )
    });
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_transcript(vec![]);
            cx.notify();
        })
    });
    draw(vcx);
    assert!(!has(vcx, "chat-header-chip-activity"));
}

#[gpui::test]
fn header_and_item_tab_follow_the_selected_checkout(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    let before = shell.read_with(vcx, |shell, cx| {
        shell.chat_item_facts(CHAT_API, cx).unwrap()
    });
    assert_eq!(before.branch.as_deref(), Some("feature/header"));
    assert_eq!(
        before.runs_in.as_deref(),
        Some(format!("/work/{CHAT_API}").as_str())
    );
    assert_eq!(before.execution_root, None);
    assert!(has(vcx, "chat-header-chip-branch"));
    assert!(has(vcx, "chat-header-label-branch:feature/header"));
    assert!(has(vcx, "chat-item-runs-in"));
    let before_fingerprint = shell.read_with(vcx, |shell, cx| {
        shell.state.read(cx).shell_fingerprint(chrono::Utc::now())
    });
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.set_chat_diff_targets_for_test(vec![mast_proto::ChatDiffTargetsView {
                chat_id: CHAT_API.into(),
                targets: vec![mast_proto::ChatDiffTargetView {
                    path: "/work/checkout".into(),
                    checkout_id: "checkout".into(),
                    source: mast_proto::ChatDiffTargetSource::Agent,
                    selected: true,
                    repo_name: "checkout".into(),
                    branch: Some("feature/bound".into()),
                    head_sha: None,
                }],
                ref_targets: vec![],
            }]);
            cx.notify();
        });
    });
    draw(vcx);
    let after = shell.read_with(vcx, |shell, cx| {
        shell.chat_item_facts(CHAT_API, cx).unwrap()
    });
    assert_eq!(after.branch.as_deref(), Some("feature/bound"));
    assert!(has(vcx, "chat-header-label-branch:feature/bound"));
    let after_fingerprint = shell.read_with(vcx, |shell, cx| {
        shell.state.read(cx).shell_fingerprint(chrono::Utc::now())
    });
    assert_ne!(before_fingerprint, after_fingerprint);
    assert_eq!(after.runs_in.as_deref(), Some("/work/checkout"));
    assert_eq!(
        after.execution_root.as_deref(),
        Some(format!("/work/{CHAT_API}").as_str())
    );
    assert!(has(vcx, "chat-header-chip-branch"));
    assert!(has(vcx, "chat-item-runs-in"));
    expand(&shell, vcx, &["runs-in"]);
    assert!(has(vcx, "chat-item-runs-in"));
    let root_selector: &'static str = Box::leak(
        format!("chat-item-execution-root:Execution root: /work/{CHAT_API}").into_boxed_str(),
    );
    assert!(has(vcx, root_selector));
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.set_chat_diff_targets_for_test(vec![mast_proto::ChatDiffTargetsView {
                chat_id: CHAT_API.into(),
                targets: vec![mast_proto::ChatDiffTargetView {
                    path: "/work/checkout".into(),
                    checkout_id: "checkout".into(),
                    source: mast_proto::ChatDiffTargetSource::Agent,
                    selected: true,
                    repo_name: "checkout".into(),
                    branch: None,
                    head_sha: Some("abc".into()),
                }],
                ref_targets: vec![],
            }]);
            cx.notify();
        })
    });
    draw(vcx);
    assert!(!has(vcx, "chat-header-chip-branch"));
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.set_chat_diff_targets_for_test(vec![]);
            cx.notify();
        })
    });
    draw(vcx);
    let restored = shell.read_with(vcx, |shell, cx| {
        shell.chat_item_facts(CHAT_API, cx).unwrap()
    });
    assert_eq!(restored.branch, before.branch);
    assert_eq!(restored.runs_in, before.runs_in);
    assert_eq!(restored.execution_root, None);
}

#[gpui::test]
fn header_link_refetch_discards_a_response_for_the_previous_checkout(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let guard = runtime.enter();
    let (out, mut requests) = tokio::sync::mpsc::channel::<String>(16);
    let (replies, inbound) = tokio::sync::mpsc::channel::<String>(16);
    let engine =
        crate::state::EngineHandle::from_test_client(mast_rpc::RpcClient::new(out, inbound));
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.set_test_engine(engine);
            state.set_chat_diff_targets_for_test(vec![mast_proto::ChatDiffTargetsView {
                chat_id: CHAT_API.into(),
                targets: vec![mast_proto::ChatDiffTargetView {
                    path: "/previous".into(),
                    checkout_id: "previous".into(),
                    source: mast_proto::ChatDiffTargetSource::Agent,
                    selected: true,
                    repo_name: "repo".into(),
                    branch: Some("old-branch".into()),
                    head_sha: None,
                }],
                ref_targets: vec![],
            }]);
        });
        let entry = shell.chat_header_links.get_mut(CHAT_API).unwrap();
        entry.checkout = Some((
            "/previous".into(),
            "previous".into(),
            Some("old-branch".into()),
        ));
        entry.fetched_at = Instant::now() - Duration::from_secs(60);
        shell.ensure_chat_header_links(CHAT_API.into(), cx);
    });
    let previous_fetch =
        shell.read_with(vcx, |shell, _| shell.chat_header_links[CHAT_API].fetched_at);
    vcx.run_until_parked();
    runtime.block_on(tokio::task::yield_now());
    let first = loop {
        let request: serde_json::Value =
            serde_json::from_str(&requests.try_recv().expect("first header request")).unwrap();
        if request["method"] == mast_rpc::methods::CHAT_LINK_STATUS {
            break request;
        }
    };
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.set_chat_diff_targets_for_test(vec![mast_proto::ChatDiffTargetsView {
                chat_id: CHAT_API.into(),
                targets: vec![mast_proto::ChatDiffTargetView {
                    path: "/checkout".into(),
                    checkout_id: "bound".into(),
                    source: mast_proto::ChatDiffTargetSource::Agent,
                    selected: true,
                    repo_name: "repo".into(),
                    branch: Some("feature".into()),
                    head_sha: None,
                }],
                ref_targets: vec![],
            }]);
            cx.notify();
        })
    });
    let status = mast_engine::pr_ticket_cache::ChatLinkStatus::default();
    replies
        .try_send(serde_json::json!({"id": first["id"], "ok": status}).to_string())
        .unwrap();
    for _ in 0..20 {
        runtime.block_on(tokio::task::yield_now());
        vcx.run_until_parked();
        if !shell.read_with(vcx, |shell, _| {
            shell.chat_header_links_pending.contains(CHAT_API)
        }) {
            break;
        }
    }
    shell.read_with(vcx, |shell, _| {
        assert_eq!(
            shell.chat_header_links[CHAT_API]
                .checkout
                .as_ref()
                .map(|key| key.0.as_str()),
            Some("/previous")
        );
        assert_eq!(shell.chat_header_links[CHAT_API].fetched_at, previous_fetch);
    });
    shell.update(vcx, |shell, cx| {
        shell.ensure_chat_header_links(CHAT_API.into(), cx)
    });
    vcx.run_until_parked();
    runtime.block_on(tokio::task::yield_now());
    let second = loop {
        let request: serde_json::Value =
            serde_json::from_str(&requests.try_recv().expect("checkout refetch")).unwrap();
        if request["method"] == mast_rpc::methods::CHAT_LINK_STATUS {
            break request;
        }
    };
    replies
        .try_send(
            serde_json::json!({"id": second["id"], "ok": mast_engine::pr_ticket_cache::ChatLinkStatus::default()}).to_string(),
        )
        .unwrap();
    for _ in 0..20 {
        runtime.block_on(tokio::task::yield_now());
        vcx.run_until_parked();
        if !shell.read_with(vcx, |shell, _| {
            shell.chat_header_links_pending.contains(CHAT_API)
        }) {
            break;
        }
    }
    shell.read_with(vcx, |shell, _| {
        assert_eq!(
            shell.chat_header_links[CHAT_API]
                .checkout
                .as_ref()
                .map(|key| key.0.as_str()),
            Some("/checkout")
        )
    });
    drop(guard);
}

#[gpui::test]
fn outline_tracker_opens_selected_coordinator_chat(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    shell.update(vcx, |shell, cx| {
        shell.open_outline_tracker(CHAT_UI.into(), cx)
    });
    draw(vcx);
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(shell.active_chat, CHAT_UI);
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Team);
    });
}
