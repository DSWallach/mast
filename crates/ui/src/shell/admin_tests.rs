//! Headless tests for admin chats in the shell: the grant dialog, the badge
//! and its Revoke button, and the chat menu entries. The engine is a scripted
//! wire; nothing real is reached.
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use mast_proto::AdminGrant;
use mast_rpc::methods;

use super::chat_item_tests::{draw, has, mount};
use super::*;
use crate::admin::tests::Wire;
use crate::admin::{DurationChoice, now_ms};
use crate::work_outline::outline_view::context_menu::{
    MenuAction, MenuEntry, MenuKind, OutlineContextMenu,
};
use crate::work_outline::store::fixture_ids::*;

fn grant_for(chat: &str, expires_at_ms: Option<i64>) -> AdminGrant {
    AdminGrant {
        chat_id: chat.into(),
        space_id: "s1".into(),
        granted_at_ms: 0,
        expires_at_ms,
        skip_bulk_confirmation: false,
    }
}

/// A Space named "Mast" for `s1`, and (when given) the scripted engine.
fn prepare(
    shell: &Entity<Shell>,
    vcx: &mut VisualTestContext,
    engine: Option<crate::state::EngineHandle>,
) {
    let state = shell.read_with(vcx, |shell, _| shell.state.clone());
    state.update(vcx, |state, _| {
        state.spaces = vec![
            serde_json::from_value(serde_json::json!({
                "id": "s1",
                "deviceId": "device",
                "path": "/work/mast",
                "name": "Mast",
                "gitDetected": false,
                "createdAt": "2026-07-19T12:00:00Z",
            }))
            .unwrap(),
        ];
        if let Some(engine) = engine {
            state.set_test_engine(engine);
        }
    });
}

fn set_grants(shell: &Entity<Shell>, vcx: &mut VisualTestContext, grants: Vec<AdminGrant>) {
    let state = shell.read_with(vcx, |shell, _| shell.state.clone());
    state.update(vcx, |state, cx| {
        state.admin.set_grants(grants);
        cx.notify();
    });
    draw(vcx);
}

fn click(vcx: &mut VisualTestContext, selector: &'static str) {
    let at = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not on screen"))
        .center();
    vcx.simulate_click(at, Modifiers::default());
    draw(vcx);
}

#[gpui::test]
fn the_grant_dialog_sends_the_picked_duration(cx: &mut TestAppContext) {
    let (mut wire, engine, _guard) = Wire::new();
    let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
    prepare(&shell, vcx, Some(engine));

    // Every choice, and the default (4 hours) when none is clicked.
    let cases: [(Option<DurationChoice>, Option<u64>); 6] = [
        (None, Some(4 * 3600)),
        (Some(DurationChoice::OneHour), Some(3600)),
        (Some(DurationChoice::FourHours), Some(4 * 3600)),
        (Some(DurationChoice::EightHours), Some(8 * 3600)),
        (Some(DurationChoice::TwentyFourHours), Some(24 * 3600)),
        (Some(DurationChoice::UntilRevoked), None),
    ];
    for (choice, secs) in cases {
        assert!(!has(vcx, "admin-dialog-grant"));
        shell.update(vcx, |shell, cx| shell.open_admin_dialog(CHAT_API, cx));
        draw(vcx);
        assert!(has(vcx, "admin-dialog-grant"));
        for option in DurationChoice::ALL {
            let selector: &'static str =
                Box::leak(format!("admin-dialog-duration-{}", option.key()).into_boxed_str());
            assert!(has(vcx, selector), "{selector}");
        }
        if let Some(choice) = choice {
            let selector: &'static str =
                Box::leak(format!("admin-dialog-duration-{}", choice.key()).into_boxed_str());
            click(vcx, selector);
        }
        click(vcx, "admin-dialog-grant");
        let sent = wire.sent();
        assert_eq!(sent.len(), 1, "{choice:?}: {sent:?}");
        assert_eq!(sent[0].0, methods::GRANT_ADMIN);
        assert_eq!(sent[0].1["chatId"], CHAT_API);
        assert_eq!(sent[0].1["skipBulkConfirmation"], false);
        assert_eq!(
            sent[0].1.get("durationSecs").and_then(|v| v.as_u64()),
            secs,
            "{choice:?}"
        );
        assert!(!has(vcx, "admin-dialog-grant"), "the dialog closes on Grant");
    }

    // The opt-in checkbox is off by default and rides along when ticked.
    shell.update(vcx, |shell, cx| shell.open_admin_dialog(CHAT_API, cx));
    draw(vcx);
    click(vcx, "admin-dialog-skip-bulk");
    click(vcx, "admin-dialog-grant");
    let sent = wire.sent();
    assert_eq!(sent[0].1["skipBulkConfirmation"], true);

    // Cancel sends nothing.
    shell.update(vcx, |shell, cx| shell.open_admin_dialog(CHAT_API, cx));
    draw(vcx);
    click(vcx, "admin-dialog-cancel");
    assert!(!has(vcx, "admin-dialog-grant"));
    assert!(wire.sent().is_empty());
}

#[gpui::test]
fn only_chats_in_a_space_can_open_the_grant_dialog(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
    prepare(&shell, vcx, None);
    let state = shell.read_with(vcx, |shell, _| shell.state.clone());
    state.update(vcx, |state, _| {
        for chat in state.chats.iter_mut().filter(|c| c.id == CHAT_LOOSE_A) {
            chat.space_id = None;
        }
    });
    shell.update(vcx, |shell, cx| shell.open_admin_dialog(CHAT_LOOSE_A, cx));
    draw(vcx);
    assert!(!has(vcx, "admin-dialog-grant"));
    shell.update(vcx, |shell, cx| shell.open_admin_dialog(CHAT_API, cx));
    draw(vcx);
    assert!(has(vcx, "admin-dialog-grant"));
}

#[gpui::test]
fn the_badge_shows_for_live_grants_only_and_revoke_sends_revoke_admin(cx: &mut TestAppContext) {
    let (mut wire, engine, _guard) = Wire::new();
    let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
    prepare(&shell, vcx, Some(engine));
    draw(vcx);
    assert!(!has(vcx, "chat-header-admin-badge") && !has(vcx, "chat-item-admin-badge"));
    // The Item tab offers the grant either way.
    assert!(has(vcx, "chat-item-admin-grant"));

    // Another chat's grant, and an expired one, show nothing here.
    set_grants(&shell, vcx, vec![grant_for(CHAT_UI, None)]);
    assert!(!has(vcx, "chat-header-admin-badge"));
    set_grants(&shell, vcx, vec![grant_for(CHAT_API, Some(now_ms() - 1))]);
    assert!(!has(vcx, "chat-header-admin-badge") && !has(vcx, "chat-item-admin-badge"));

    // A live grant shows in both places.
    set_grants(&shell, vcx, vec![grant_for(CHAT_API, Some(now_ms() + 3 * 3_600_000))]);
    assert!(has(vcx, "chat-header-admin-badge") && has(vcx, "chat-item-admin-badge"));
    assert!(has(vcx, "chat-header-admin-revoke") && has(vcx, "chat-item-admin-revoke"));
    assert!(wire.sent().is_empty());

    click(vcx, "chat-header-admin-revoke");
    let sent = wire.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, methods::REVOKE_ADMIN);
    assert_eq!(sent[0].1, serde_json::json!({ "chatId": CHAT_API }));
    assert!(!has(vcx, "chat-header-admin-badge"), "gone at once");
    assert!(!has(vcx, "chat-item-admin-badge"));

    // "Until revoked" shows too, and the Item tab's Revoke works as well.
    set_grants(&shell, vcx, vec![grant_for(CHAT_API, None)]);
    assert!(has(vcx, "chat-header-admin-badge"));
    click(vcx, "chat-item-admin-revoke");
    assert_eq!(wire.sent()[0].0, methods::REVOKE_ADMIN);
    assert!(!has(vcx, "chat-header-admin-badge"));
}

#[gpui::test]
fn the_outline_chat_menu_offers_grant_then_renew_and_revoke(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
    prepare(&shell, vcx, None);
    let entries = |shell: &Entity<Shell>, vcx: &mut VisualTestContext| {
        shell.read_with(vcx, |shell, cx| {
            let mut menu = OutlineContextMenu {
                kind: MenuKind::Chat,
                row_key: None,
                space_id: Some("s1".into()),
                item_id: None,
                chat_id: Some(CHAT_API.into()),
                entries: Vec::new(),
                status_entries: Vec::new(),
            };
            shell.add_sidebar_chat_entries(&mut menu, cx);
            menu.entries
                .into_iter()
                .filter(|e| {
                    matches!(e.action, MenuAction::GrantAdmin(_) | MenuAction::RevokeAdmin(_))
                })
                .map(|e| (e.id, e.label))
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(
        entries(&shell, vcx),
        [("grant-admin", "Grant admin for this Space\u{2026}".to_owned())]
    );
    set_grants(&shell, vcx, vec![grant_for(CHAT_API, None)]);
    assert_eq!(
        entries(&shell, vcx),
        [
            ("grant-admin", "Renew admin\u{2026}".to_owned()),
            ("revoke-admin", "Revoke admin".to_owned())
        ]
    );
}

/// A compact row's Item menu, as the builder leaves it: the one Chat's
/// Archive entry is what the Shell finds the Chat by.
fn compact_item_admin_entries(
    shell: &Entity<Shell>,
    vcx: &mut VisualTestContext,
) -> Vec<(&'static str, String)> {
    shell.read_with(vcx, |shell, cx| {
        let mut menu = OutlineContextMenu {
            kind: MenuKind::Item,
            row_key: Some("row".into()),
            space_id: Some("s1".into()),
            item_id: Some("i1".into()),
            chat_id: None,
            entries: vec![
                MenuEntry::new("open-chat", "Open Chat", "", MenuAction::AddInstruction),
                MenuEntry::new(
                    "archive-chat",
                    "Archive Chat",
                    "",
                    MenuAction::ArchiveChat(CHAT_API.into()),
                ),
                MenuEntry::new("copy-path", "Copy Path", "", MenuAction::CopyPath("p".into())),
            ],
            status_entries: Vec::new(),
        };
        shell.add_sidebar_chat_entries(&mut menu, cx);
        let ids: Vec<_> = menu.entries.iter().map(|e| e.id).collect();
        // Spliced right after the Chat's Archive; the rest is unchanged.
        if ids.contains(&"grant-admin") {
            assert_eq!(&ids[..3], ["open-chat", "archive-chat", "grant-admin"]);
            assert_eq!(*ids.last().unwrap(), "copy-path");
        }
        menu.entries
            .into_iter()
            .filter(|e| matches!(e.action, MenuAction::GrantAdmin(_) | MenuAction::RevokeAdmin(_)))
            .map(|e| (e.id, e.label))
            .collect()
    })
}

#[gpui::test]
fn a_compact_row_menu_offers_grant_then_renew_and_revoke(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
    prepare(&shell, vcx, None);
    assert_eq!(
        compact_item_admin_entries(&shell, vcx),
        [("grant-admin", "Grant admin for this Space\u{2026}".to_owned())]
    );
    set_grants(&shell, vcx, vec![grant_for(CHAT_API, None)]);
    assert_eq!(
        compact_item_admin_entries(&shell, vcx),
        [
            ("grant-admin", "Renew admin\u{2026}".to_owned()),
            ("revoke-admin", "Revoke admin".to_owned())
        ]
    );
}

#[gpui::test]
fn a_compact_row_chat_without_a_space_gets_no_admin_entries(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
    prepare(&shell, vcx, None);
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            for chat in &mut state.chats {
                chat.space_id = None;
            }
        });
    });
    assert!(compact_item_admin_entries(&shell, vcx).is_empty());
}
