use super::*;

#[test]
fn sidebar_drag_nudges_each_edge_once_until_rearmed() {
    let min = sidebar_drag_sample(SIDEBAR_MIN, None, false);
    assert_eq!(min.width, SIDEBAR_MIN);
    assert_eq!(min.edge, Some(motion::ResizeEdge::Min));
    assert!(min.starts_bounce);

    let held_min = sidebar_drag_sample(SIDEBAR_MIN - 80.0, min.edge, false);
    assert_eq!(held_min.width, SIDEBAR_MIN);
    assert_eq!(held_min.edge, min.edge);
    assert!(!held_min.starts_bounce);

    let inside = sidebar_drag_sample(SIDEBAR_MIN + 1.0, held_min.edge, false);
    assert_eq!(inside.edge, None);
    assert!(!inside.starts_bounce);

    let rearmed_min = sidebar_drag_sample(SIDEBAR_MIN - 1.0, inside.edge, false);
    assert!(rearmed_min.starts_bounce);

    let max = sidebar_drag_sample(SIDEBAR_MAX, rearmed_min.edge, false);
    assert_eq!(max.width, SIDEBAR_MAX);
    assert_eq!(max.edge, Some(motion::ResizeEdge::Max));
    assert!(max.starts_bounce);

    let held_max = sidebar_drag_sample(SIDEBAR_MAX + 80.0, max.edge, false);
    assert_eq!(held_max.width, SIDEBAR_MAX);
    assert!(!held_max.starts_bounce);
}

#[test]
fn sidebar_drag_stays_exact_in_range_and_reduced_motion_never_nudges() {
    let middle = sidebar_drag_sample(312.0, None, false);
    assert_eq!(middle.width, 312.0);
    assert_eq!(middle.edge, None);
    assert!(!middle.starts_bounce);

    for pointer_x in [
        SIDEBAR_MIN - 100.0,
        SIDEBAR_MIN,
        SIDEBAR_MAX,
        SIDEBAR_MAX + 100.0,
    ] {
        let sample = sidebar_drag_sample(pointer_x, None, true);
        assert!((SIDEBAR_MIN..=SIDEBAR_MAX).contains(&sample.width));
        assert!(!sample.starts_bounce);
    }
}

#[test]
fn right_pane_uses_the_shared_clamp_and_edge_latch() {
    let min =
        motion::resize_drag_sample(RIGHT_PANE_MIN - 40.0, RIGHT_PANE_MIN, 820.0, None, false);
    assert_eq!(min.width, RIGHT_PANE_MIN);
    assert_eq!(min.edge, Some(motion::ResizeEdge::Min));
    assert!(min.starts_bounce);

    let held = motion::resize_drag_sample(
        RIGHT_PANE_MIN - 80.0,
        RIGHT_PANE_MIN,
        820.0,
        min.edge,
        false,
    );
    assert!(!held.starts_bounce);

    let max = motion::resize_drag_sample(900.0, RIGHT_PANE_MIN, 820.0, None, false);
    assert_eq!(max.width, 820.0);
    assert_eq!(max.edge, Some(motion::ResizeEdge::Max));
    assert!(max.starts_bounce);
}

#[test]
fn sidebar_bounce_has_rounded_out_and_return_phases() {
    assert_eq!(
        motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.0),
        0.0
    );
    assert_eq!(
        motion::resize_bounce_offset(
            motion::ResizeEdge::Max,
            motion::RESIZE_EDGE_BOUNCE_OUT_FRACTION
        ),
        motion::RESIZE_EDGE_NUDGE
    );
    assert_eq!(
        motion::resize_bounce_offset(motion::ResizeEdge::Max, 1.0),
        0.0
    );

    let gentle_start = motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.01);
    let outbound = motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.2);
    let returning = motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.7);
    assert!(gentle_start > 0.0 && gentle_start < 0.1);
    assert!(outbound > gentle_start && outbound < motion::RESIZE_EDGE_NUDGE);
    assert!(returning > 0.0 && returning < motion::RESIZE_EDGE_NUDGE);
    assert_eq!(
        motion::resize_bounce_offset(motion::ResizeEdge::Min, 0.2),
        -outbound
    );
}

#[test]
fn every_default_shortcut_binds_on_this_platform() {
    // `apply_keymap` silently falls back on an unparseable combo, so a
    // default gpui cannot parse would ship as a dead shortcut.
    for id in crate::settings::ShortcutId::ALL {
        // Work Outline commands without a default ship unbound (the
        // palette is their entry point); `apply_keymap` skips empty combos.
        if id.default_combo().is_empty() {
            assert!(
                crate::work_outline::WorkOutlineCommand::from_shortcut_id(id).is_some(),
                "{} has no default and is not a Work Outline command",
                id.label()
            );
            continue;
        }
        let combo = platform_combo(id.default_combo());
        assert!(
            Keystroke::parse(&combo).is_ok(),
            "{} default {combo:?} does not parse",
            id.label()
        );
    }
}

#[test]
fn overview_navigation_shortcuts_parse_and_do_not_conflict_with_defaults() {
    let fixed = [
        FOCUS_OVERVIEW_SEARCH_COMBO,
        OPEN_PR_REVIEW_CHATS_COMBO,
        "mod-k",
        "mod-shift-o",
        OPEN_ITEM_TAB_COMBO,
        TOGGLE_ITEM_CARD_COMBO,
        running_ui::TOGGLE_RUNNING_CHATS_COMBO,
    ];
    for (index, combo) in fixed.iter().enumerate() {
        assert!(Keystroke::parse(&platform_combo(combo)).is_ok());
        assert!(
            fixed[index + 1..].iter().all(|other| combo != other),
            "duplicate fixed shortcut {combo}"
        );
        assert!(
            ShortcutId::ALL
                .into_iter()
                .all(|id| id.default_combo() != *combo),
            "fixed shortcut {combo} conflicts with a customizable default"
        );
    }
}

#[test]
fn island_stays_centered_on_controls_while_expanding() {
    let center = (Theme::TITLEBAR_HEIGHT + Theme::TITLEBAR_TOP_PAD) * 0.5;
    for step in 0..=20 {
        let (top, height) = titlebar_island_vertical_geometry(step as f32 / 20.0);
        assert_eq!(top + height * 0.5, center);
        assert!((28.0..=32.0).contains(&height));
    }
    let (top, height) = titlebar_island_vertical_geometry(1.0);
    assert_eq!(center, 21.0);
    assert_eq!(center - 12.0 - top, 4.0);
    assert_eq!(top + height - (center + 12.0), 4.0);
}

#[test]
fn new_thread_handoff_is_continuous_and_staged() {
    assert!(bottom_stack_measurement_matches(false, false));
    assert!(bottom_stack_measurement_matches(true, true));
    assert!(!bottom_stack_measurement_matches(false, true));
    assert!(!bottom_stack_measurement_matches(true, false));
    assert_eq!(new_thread_background_opacity(false), 1.0);
    assert_eq!(
        new_thread_background_opacity(true),
        NEW_THREAD_BACKGROUND_FROSTED_OPACITY
    );
    assert_eq!(new_thread_background_height(400.0), 288.0);
    assert!((new_thread_background_height(600.0) - 432.0).abs() < 0.001);
    assert_eq!(new_thread_background_height(1_000.0), 720.0);
    assert_eq!(new_thread_background_height(1_200.0), 760.0);
    assert!(new_thread_background_height(848.0) > 848.0 / 2.0);
}

#[test]
fn sidebar_width_yields_to_a_narrow_window_without_changing_the_preference() {
    assert_eq!(sidebar_width_for_viewport(520.0, 1600.0), 520.0);
    assert_eq!(
        sidebar_width_for_viewport(520.0, 800.0),
        800.0 - CHAT_PANEL_MIN
    );
    // Never below the sidebar minimum, even in a tiny window.
    assert_eq!(sidebar_width_for_viewport(300.0, 300.0), SIDEBAR_MIN);
    assert_eq!(sidebar_width_for_viewport(SIDEBAR_MIN, 300.0), SIDEBAR_MIN);
}

#[test]
fn right_pane_ceiling_preserves_the_chat_floor() {
    assert_eq!(right_pane_max_width(1200.0, 256.0, CHAT_PANEL_MIN), 644.0);
    assert_eq!(1200.0 - 256.0 - 644.0, CHAT_PANEL_MIN);
    // The chat floor wins over the right pane's preferred 360px minimum
    // when the whole window is unusually narrow.
    assert_eq!(right_pane_max_width(800.0, 256.0, CHAT_PANEL_MIN), 244.0);
    assert_eq!(800.0 - 256.0 - 244.0, CHAT_PANEL_MIN);
}

#[test]
fn right_pane_takeover_consumes_the_chat_column() {
    assert_eq!(right_pane_takeover_width(1200.0, 256.0), 944.0);
    assert_eq!(1200.0 - 256.0 - 944.0, 0.0);
}

#[test]
fn escape_interrupts_only_the_active_live_chat() {
    assert_eq!(
        resolve_shell_escape(
            "escape",
            false,
            true,
            Route::Chat,
            Some("chat-a"),
            Indicator::Working,
            false,
        ),
        ShellEscapeOutcome::InterruptChat("chat-a".to_owned())
    );
    assert_eq!(
        resolve_shell_escape(
            "escape",
            false,
            true,
            Route::Chat,
            Some("chat-b"),
            Indicator::AwaitingInput,
            false,
        ),
        ShellEscapeOutcome::InterruptChat("chat-b".to_owned())
    );
}

#[test]
fn escape_ignores_non_live_or_ineligible_views() {
    assert_eq!(
        resolve_shell_escape(
            "escape",
            true,
            true,
            Route::Chat,
            Some("chat-a"),
            Indicator::Working,
            false,
        ),
        ShellEscapeOutcome::Blocked
    );
    for (route, selected, indicator, interrupting) in [
        (Route::Chat, Some("chat-a"), Indicator::None, false),
        (Route::Chat, None, Indicator::Working, false),
        (Route::Chat, Some("chat-a"), Indicator::Working, true),
        (
            Route::Settings(SettingsSection::Devices),
            Some("chat-a"),
            Indicator::Working,
            false,
        ),
    ] {
        assert_eq!(
            resolve_shell_escape(
                "escape",
                false,
                true,
                route,
                selected,
                indicator,
                interrupting,
            ),
            ShellEscapeOutcome::Ignored
        );
    }
    assert_eq!(
        resolve_shell_escape(
            "enter",
            true,
            true,
            Route::Chat,
            Some("chat-a"),
            Indicator::Working,
            false,
        ),
        ShellEscapeOutcome::OtherKey
    );
}

#[test]
fn escape_interrupt_is_opt_in() {
    assert_eq!(
        resolve_shell_escape(
            "escape",
            false,
            false,
            Route::Chat,
            Some("chat-a"),
            Indicator::Working,
            false,
        ),
        ShellEscapeOutcome::Ignored
    );
}

#[test]
fn only_visible_sync_steps_block_escape() {
    assert!(!SyncFlow::Idle.has_visible_overlay());
    assert!(!SyncFlow::SwitchOffer { notice_open: false }.has_visible_overlay());
    assert!(SyncFlow::SwitchOffer { notice_open: true }.has_visible_overlay());
    assert!(SyncFlow::Importing { done: 1, total: 3 }.has_visible_overlay());
    assert!(SyncFlow::SignOutConfirm.has_visible_overlay());
}

#[test]
fn right_pane_takeover_control_reverses_direction() {
    assert_eq!(tabs::right_pane_expand_icon(false), icons::EXPAND_ARROWS);
    assert_eq!(tabs::right_pane_expand_icon(true), icons::COLLAPSE_ARROWS);
}

#[test]
fn pane_resize_hitboxes_yield_the_titlebar_chrome() {
    assert_eq!(PANE_RESIZE_HITBOX_TOP, Theme::TITLEBAR_HEIGHT);
    assert_eq!(PANE_RESIZE_HITBOX_HALF_WIDTH * 2.0, 20.0);
    assert_eq!(TERMINAL_RESIZE_HITBOX_HEIGHT, 10.0);
}

#[test]
fn new_session_action_lives_in_the_titlebar_only_when_useful() {
    assert_eq!(titlebar_new_session_alpha(true, true), 1.0);
    assert_eq!(titlebar_new_session_alpha(true, false), 0.0);
    assert_eq!(titlebar_new_session_alpha(false, true), 0.0);
    assert_eq!(titlebar_new_session_alpha(false, false), 0.0);
}

#[test]
fn right_panel_content_keeps_the_larger_width_only_during_transition() {
    assert_eq!(right_panel_content_width(520.0, None, None), 520.0);
    assert_eq!(
        right_panel_content_width(0.0, Some((520.0, 0.0)), None),
        520.0
    );
    assert_eq!(
        right_panel_content_width(760.0, Some((520.0, 760.0)), None),
        760.0
    );
    assert_eq!(
        right_panel_content_width(1064.0, Some((520.0, 1064.0)), Some(760.0)),
        760.0
    );

    let conversation = conversation_width(1320.0, 256.0, 520.0);
    let takeover = conversation_width(1320.0, 256.0, 1064.0);
    assert_eq!(conversation, 544.0);
    assert_eq!(takeover, 0.0);
    assert_eq!(
        stable_panel_content_width(takeover, Some((conversation, takeover))),
        conversation
    );
    assert_eq!(
        stable_panel_content_width(conversation, Some((takeover, conversation))),
        conversation
    );
}

#[tokio::test]
async fn remote_shutdown_waits_for_ipc_release() {
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(listener);
    });

    wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_secs(2))
        .await
        .unwrap();
    release.await.unwrap();
}

#[tokio::test]
async fn signed_out_synced_runtime_stops_and_reboots_local() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("session.json"),
        r#"{"refreshToken":"still-valid","user":{"id":"user_1","email":"u@example.com"},"orgId":"org_1"}"#,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let boot = EngineBootConfig {
        data_dir: dir.path().to_path_buf(),
        ipc_port: port,
        edge_url: "http://127.0.0.1:1".into(),
        edge_token: None,
        org_id: None,
        workos_client_id: Some("client_test".into()),
        default_harness: mast_proto::HarnessId::Mock,
    };
    let synced = crate::state::EngineHandle::bootstrap(boot.clone())
        .await
        .expect("saved session opens its synced profile");
    assert_eq!(synced.engine_info().workspace_scope, WorkspaceScope::Synced);

    synced
        .client()
        .call(methods::SIGN_OUT, serde_json::json!({}))
        .await
        .expect("sign out clears credentials");
    stop_synced_runtime(synced, port, dir.path())
        .await
        .expect("synced runtime drains and releases ownership");

    assert!(!dir.path().join("session.json").exists());
    let local = crate::state::EngineHandle::bootstrap(boot)
        .await
        .expect("same process can continue locally");
    assert_eq!(local.engine_info().workspace_scope, WorkspaceScope::Local);
    local.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn remote_shutdown_waits_for_engine_lock_release() {
    let dir = tempfile::tempdir().unwrap();
    let lock = InstanceLock::acquire(dir.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let lock_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let released_by_task = lock_released.clone();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(lock);
        released_by_task.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_secs(2))
        .await
        .unwrap();
    assert!(lock_released.load(std::sync::atomic::Ordering::SeqCst));
    release.await.unwrap();
}

#[tokio::test]
async fn remote_shutdown_times_out_while_ipc_remains_open() {
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let error = wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_millis(100))
        .await
        .unwrap_err();

    assert!(error.contains("did not finish stopping"));
    drop(listener);
}

#[test]
fn account_actions_follow_the_attached_workspace_scope() {
    assert_eq!(
        account_menu_action(Some(WorkspaceScope::Local), SyncFlow::Idle),
        Some(AccountMenuAction::EnableSync)
    );
    assert_eq!(
        account_menu_action(Some(WorkspaceScope::Synced), SyncFlow::Idle),
        Some(AccountMenuAction::SignOut)
    );
    assert_eq!(
        account_menu_action(Some(WorkspaceScope::Development), SyncFlow::Idle),
        None
    );
}

#[test]
fn local_sign_in_offers_the_in_place_switch() {
    let signed_in = AuthState::SignedIn {
        user: mast_proto::UserProfile {
            id: "user-1".into(),
            email: "user@example.com".into(),
            name: None,
        },
        org_id: Some("org-1".into()),
    };

    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::Enabling,
            Some(WorkspaceScope::Local),
            Some(&signed_in),
        ),
        SyncFlow::SwitchOffer { notice_open: true }
    );
    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::Idle,
            Some(WorkspaceScope::Local),
            Some(&signed_in),
        ),
        SyncFlow::SwitchOffer { notice_open: true },
        "another viewport derives the pending switch from AuthStatus"
    );
    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::SwitchOffer { notice_open: false },
            Some(WorkspaceScope::Local),
            Some(&signed_in),
        ),
        SyncFlow::SwitchOffer { notice_open: false },
        "shared auth updates do not reopen a postponed wizard"
    );
    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::RestartPending { notice_open: false },
            Some(WorkspaceScope::Local),
            Some(&signed_in),
        ),
        SyncFlow::RestartPending { notice_open: false },
        "the quit fallback survives shared auth updates too"
    );
    assert_eq!(
        account_menu_action(
            Some(WorkspaceScope::Local),
            SyncFlow::SwitchOffer { notice_open: false },
        ),
        Some(AccountMenuAction::RestartPending)
    );
    for notice_open in [true, false] {
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::SwitchOffer { notice_open },
                Some(WorkspaceScope::Local),
                Some(&AuthState::SignedOut),
            ),
            SyncFlow::Idle,
            "revoked credentials cancel the pending switch"
        );
    }
}

#[test]
fn import_summary_errors_are_a_failure_not_a_success() {
    // Clean summary → done with counts.
    let clean = serde_json::json!({
        "kind": "summary", "importedChats": 2, "skippedChats": 1, "errors": []
    });
    assert_eq!(import_summary_outcome(&clean), Ok((2, 1)));

    // Any error means the wizard must NOT say "all set" — partial
    // migrations surface as an explicit failure with the first cause.
    let partial = serde_json::json!({
        "kind": "summary", "importedChats": 1, "skippedChats": 0,
        "errors": ["chat c2: journal copy failed"]
    });
    let message = import_summary_outcome(&partial).expect_err("errors must fail");
    assert!(message.contains("journal copy failed"), "{message}");
    assert!(message.contains("1 imported"), "{message}");

    let many = serde_json::json!({
        "kind": "summary", "importedChats": 0, "skippedChats": 0,
        "errors": ["a", "b", "c"]
    });
    let message = import_summary_outcome(&many).expect_err("errors must fail");
    assert!(message.contains("3 failures"), "{message}");

    // A summary missing the errors field entirely (older engine) is
    // treated as clean rather than failing every import.
    let legacy = serde_json::json!({ "kind": "summary", "importedChats": 4 });
    assert_eq!(import_summary_outcome(&legacy), Ok((4, 0)));
}

#[test]
fn spaces_only_local_work_still_gets_the_import_offer() {
    assert_eq!(local_work_phrase(0, 0), None, "nothing to bring");
    assert_eq!(local_work_phrase(2, 0).as_deref(), Some("the 2 sessions"));
    assert_eq!(
        local_work_phrase(0, 1).as_deref(),
        Some("the 1 project"),
        "a Spaces-only profile must be offered the import, not a bare switch"
    );
    assert_eq!(
        local_work_phrase(1, 2).as_deref(),
        Some("the 1 session and 2 projects")
    );
}

#[test]
fn dismissed_import_failure_stays_reachable_on_a_synced_runtime() {
    let signed_in = AuthState::SignedIn {
        user: mast_proto::UserProfile {
            id: "user-1".into(),
            email: "user@example.com".into(),
            name: None,
        },
        org_id: Some("org-1".into()),
    };

    // "Later" postpones the failure notice; it must not evaporate.
    let dismissed = SyncFlow::ImportFailed { notice_open: false };
    assert_eq!(
        sync_flow_after_auth(dismissed, Some(WorkspaceScope::Synced), Some(&signed_in)),
        dismissed,
        "a postponed import failure survives auth/scope updates"
    );

    // …and the account menu on the SYNCED runtime still exposes the
    // re-entry point. This is the whole point: after the switch there is
    // no local runtime left to re-derive an offer from, so this menu row
    // is the only path back to the retry dialog.
    assert_eq!(
        account_menu_action(Some(WorkspaceScope::Synced), dismissed),
        Some(AccountMenuAction::RestartPending),
        "retry must remain reachable after dismissal"
    );
    assert_eq!(
        account_menu_action(
            Some(WorkspaceScope::Synced),
            SyncFlow::ImportFailed { notice_open: true },
        ),
        Some(AccountMenuAction::RestartPending)
    );

    // Resolving the failure restores the normal synced menu.
    assert_eq!(
        account_menu_action(Some(WorkspaceScope::Synced), SyncFlow::Idle),
        Some(AccountMenuAction::SignOut)
    );
}

#[test]
fn switch_lifecycle_survives_the_runtime_replacement_window() {
    let signed_in = AuthState::SignedIn {
        user: mast_proto::UserProfile {
            id: "user-1".into(),
            email: "user@example.com".into(),
            name: None,
        },
        org_id: Some("org-1".into()),
    };
    for flow in [
        SyncFlow::Switching { import: true },
        SyncFlow::Importing { done: 1, total: 3 },
        SyncFlow::ImportDone {
            imported: 3,
            skipped: 0,
        },
        SyncFlow::ImportFailed { notice_open: true },
        SyncFlow::ImportFailed { notice_open: false },
    ] {
        // Local (before the stop), detached (mid-replacement), and synced
        // (replacement runtime up): the driver owns these states — auth
        // and scope edges must never reset them.
        assert_eq!(
            sync_flow_after_auth(flow, Some(WorkspaceScope::Local), Some(&signed_in)),
            flow
        );
        assert_eq!(sync_flow_after_auth(flow, None, None), flow);
        assert_eq!(
            sync_flow_after_auth(flow, Some(WorkspaceScope::Synced), Some(&signed_in)),
            flow
        );
    }
}

#[test]
fn synced_sign_out_blocks_every_viewport_and_cannot_switch_accounts() {
    let signed_in_as_another_user = AuthState::SignedIn {
        user: mast_proto::UserProfile {
            id: "user-2".into(),
            email: "other@example.com".into(),
            name: None,
        },
        org_id: Some("org-2".into()),
    };

    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::SigningOut,
            Some(WorkspaceScope::Synced),
            Some(&AuthState::SignedOut),
        ),
        SyncFlow::SignedOutRestartRequired,
        "the viewport that requested sign-out is blocked by AuthStatus"
    );
    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::Idle,
            Some(WorkspaceScope::Synced),
            Some(&AuthState::SignedOut),
        ),
        SyncFlow::SignedOutRestartRequired,
        "another viewport observing the same runtime is also blocked"
    );
    assert_eq!(
        sync_flow_after_auth(
            SyncFlow::SignedOutRestartRequired,
            Some(WorkspaceScope::Synced),
            Some(&signed_in_as_another_user),
        ),
        SyncFlow::SignedOutRestartRequired,
        "new credentials cannot reopen the previous account's store"
    );
}

#[test]
fn titlebar_cluster_matches_zeron_window_controls() {
    // zeron window-controls.tsx: `left: fullscreen ? 12 : 88` — the
    // cluster clears the {14,15} traffic lights, and reclaims the inset
    // when fullscreen hides them.
    assert_eq!(titlebar_cluster_start(false), 88.0);
    assert_eq!(titlebar_cluster_start(true), 12.0);
    assert_eq!(TITLEBAR_CONTROL_GAP, 2.0);
    assert_eq!(TITLEBAR_GROUP_GAP, Theme::SPACE_SM);
    assert_eq!(TITLEBAR_IDENTITY_GAP, Theme::SPACE_MD);
    assert_eq!(CLUSTER_BUTTONS_WIDTH, 82.0);
    assert_eq!(TITLEBAR_ACTION_SLOT_WIDTH, 32.0);
    assert_eq!(TITLEBAR_ACTION_EDGE_INSET, 6.0);
}

#[test]
fn titlebar_spacer_selects_per_platform_and_fullscreen() {
    // macOS, lights visible: spacer fills up to the 88px cluster start.
    assert_eq!(titlebar_spacer_width(true, false, 10.0), 78.0);
    assert_eq!(titlebar_spacer_width(true, false, 12.0), 76.0);
    assert_eq!(titlebar_spacer_width(true, false, 26.0), 62.0);
    // macOS fullscreen: the inset animates away (clamped at zero when the
    // strip's own padding already exceeds the 12px cluster start).
    assert_eq!(titlebar_spacer_width(true, true, 10.0), 2.0);
    assert_eq!(titlebar_spacer_width(true, true, 26.0), 0.0);
    // Linux: never any inset.
    assert_eq!(titlebar_spacer_width(false, false, 10.0), 0.0);
    assert_eq!(titlebar_spacer_width(false, true, 10.0), 0.0);
    assert_eq!(
        TITLEBAR_CLUSTER_PAD + titlebar_spacer_width(true, false, TITLEBAR_CLUSTER_PAD),
        titlebar_cluster_start(false),
        "the rendered row padding and spacer must land on the declared cluster start"
    );
}

#[test]
fn linux_caption_controls_reserve_titlebar_space() {
    // 24px buttons on the cluster's 2px rhythm.
    assert_eq!(caption_buttons_width(0), 0.0);
    assert_eq!(caption_buttons_width(1), 24.0);
    assert_eq!(caption_buttons_width(3), 76.0);
    assert_eq!(titlebar_right_padding(0, 16.0), 16.0);
    // Right-side captions (the Linux default: minimize,maximize,close):
    // content pads past the 10px edge inset + the button row.
    assert_eq!(titlebar_right_padding(3, 16.0), 16.0 + 10.0 + 76.0);
    // GNOME-vanilla ":close" — a single right button.
    assert_eq!(titlebar_right_padding(1, 16.0), 16.0 + 10.0 + 24.0);
    // Left-side captions ("close:…" layouts) shift the app cluster right
    // by the button row + one 2px gap.
    assert_eq!(cluster_buttons_start(false, false, 0), 10.0);
    assert_eq!(cluster_buttons_start(false, false, 1), 10.0 + 24.0 + 2.0);
    assert_eq!(cluster_buttons_start(false, false, 3), 10.0 + 76.0 + 2.0);
    // macOS ignores the Linux caption count entirely.
    assert_eq!(cluster_buttons_start(true, false, 3), 88.0);
}

#[test]
fn cluster_clearance_clears_the_overlay_buttons() {
    // Linux: buttons at 10..92; a 16px-padded header needs 84 more px to
    // put content at 92 + 8 breathing room.
    assert_eq!(cluster_clearance(false, false, 0, 16.0), 84.0 + 64.0);
    assert_eq!(cluster_clearance(false, false, 0, 10.0), 90.0 + 64.0);
    // Linux with a left-side close caption: everything shifts one slot.
    assert_eq!(cluster_clearance(false, false, 1, 16.0), 84.0 + 64.0 + 26.0);
    // macOS: buttons start at the 88px traffic-light cluster start.
    assert_eq!(
        cluster_clearance(true, false, 0, 16.0),
        88.0 + CLUSTER_BUTTONS_WIDTH + TITLEBAR_TRAILING_SLOTS_WIDTH + 8.0 - 16.0
    );
    // macOS fullscreen: cluster reclaims the inset (starts at 12).
    assert_eq!(
        cluster_clearance(true, true, 0, 16.0),
        12.0 + CLUSTER_BUTTONS_WIDTH + TITLEBAR_TRAILING_SLOTS_WIDTH + 8.0 - 16.0
    );
}

// ---- per-session panel flags (§1.10/1.11 parity: zeron sessionPanels) ----

#[test]
fn session_panels_default_closed_per_chat() {
    let panels = SessionPanels::default();
    assert_eq!(panels.get("a"), ChatPanels::default());
    // Everything closed until explicitly opened (user request — the
    // brief default-open popped the pane on every visited session).
    assert!(!panels.get("a").terminal_open);
    assert!(!panels.get("a").changes_open);
    assert_eq!(panels.get("a").right_active, RightSurface::Picker);
    // The new-chat canvas ("" key) is its own session, also closed.
    assert!(!panels.get("").terminal_open);
}

#[test]
fn session_panels_flags_are_chat_scoped() {
    let mut panels = SessionPanels::default();
    // Opening the terminal in chat A opens it ONLY in chat A.
    assert!(panels.toggle_terminal("a"));
    assert!(panels.get("a").terminal_open);
    assert!(!panels.get("b").terminal_open);
    assert!(!panels.get("").terminal_open);
    // Changes pane in B is independent of A's terminal.
    assert!(panels.toggle_changes("b"));
    assert!(panels.get("b").changes_open);
    assert!(!panels.get("b").terminal_open);
    assert!(!panels.get("a").changes_open);
    // Switching back to A restores A's state untouched.
    assert!(panels.get("a").terminal_open);
    // Toggling off round-trips.
    assert!(!panels.toggle_terminal("a"));
    assert!(!panels.get("a").terminal_open);
}

#[test]
fn session_panels_both_flags_coexist_per_chat() {
    let mut panels = SessionPanels::default();
    panels.toggle_terminal("a");
    panels.toggle_changes("a");
    assert_eq!(
        panels.get("a"),
        ChatPanels {
            terminal_open: true,
            changes_open: true,
            ..Default::default()
        }
    );
    assert_eq!(panels.get("b"), ChatPanels::default());
    // The right pane round-trips back closed.
    assert!(!panels.toggle_changes("a"));
    assert!(!panels.get("a").changes_open);
}

#[test]
fn session_panels_update_tracks_right_surfaces() {
    let mut panels = SessionPanels::default();
    panels.update("a", |p| p.right_active = RightSurface::Diff(3));
    assert_eq!(panels.get("a").right_active, RightSurface::Diff(3));
    // Other chats keep the picker default.
    assert_eq!(panels.get("b").right_active, RightSurface::Picker);
    panels.update("a", |p| p.right_active = RightSurface::Terminal(7));
    assert_eq!(panels.get("a").right_active, RightSurface::Terminal(7));
    panels.update("a", |p| p.right_active = RightSurface::File(0));
    assert_eq!(panels.get("a").right_active, RightSurface::File(0));
}

#[test]
fn file_surface_is_single_instance_per_tab_list() {
    let mut tabs = vec![RightSurface::Terminal(1)];
    assert!(push_unique_right_surface(&mut tabs, RightSurface::File(0)));
    assert!(!push_unique_right_surface(&mut tabs, RightSurface::File(0)));
    assert_eq!(tabs, vec![RightSurface::Terminal(1), RightSurface::File(0)]);
}

#[test]
fn file_editors_are_distinct_surface_tabs_with_stable_titles() {
    let mut tabs = vec![RightSurface::File(0)];
    assert!(push_unique_right_surface(&mut tabs, RightSurface::File(1)));
    assert!(push_unique_right_surface(&mut tabs, RightSurface::File(2)));
    assert!(!push_unique_right_surface(&mut tabs, RightSurface::File(1)));
    assert_eq!(workspace_file_title("src/árbol.rs"), "árbol.rs");
}

// ---- sidebar resort FLIP diff (§1.6) ----

fn keys(list: &[(&str, f32)]) -> Vec<(String, f32)> {
    list.iter().map(|(k, h)| (k.to_string(), *h)).collect()
}

#[gpui::test]
fn header_buttons_render_for_each_style(cx: &mut gpui::TestAppContext) {
    struct StyleHost;
    impl gpui::Render for StyleHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let styles = [
                crate::ui_style::UiStyle::MINIMAL,
                crate::ui_style::UiStyle::DENSE,
                crate::ui_style::UiStyle::GUIDED,
            ];
            div()
                .flex()
                .children(styles.into_iter().enumerate().map(|(index, style)| {
                    let theme = Theme::default().with_ui_style(style);
                    let id = ["style-minimal", "style-dense", "style-guided"][index];
                    header_icon_button(id, icons::PEN, "Rename", true, &theme, |_, _, _| {})
                        .debug_selector(move || id.into())
                }))
        }
    }
    let (_, vcx) = cx.add_window_view(|_, _| StyleHost);
    vcx.update(|window, cx| window.draw(cx).clear());
    for id in ["style-minimal", "style-dense", "style-guided"] {
        assert!(vcx.debug_bounds(id).is_some());
    }
}

#[gpui::test]
fn guided_header_labels_collapse_when_space_is_limited(cx: &mut gpui::TestAppContext) {
    struct StyleHost;
    impl gpui::Render for StyleHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::default().with_ui_style(crate::ui_style::UiStyle::GUIDED);
            div().flex().children(
                [(true, "guided-wide"), (false, "guided-narrow")]
                    .into_iter()
                    .map(|(fits, id)| {
                        header_icon_button(id, icons::PEN, "Rename", fits, &theme, |_, _, _| {})
                            .debug_selector(move || id.into())
                    }),
            )
        }
    }
    let (_, vcx) = cx.add_window_view(|_, _| StyleHost);
    vcx.update(|window, cx| window.draw(cx).clear());
    let wide = vcx.debug_bounds("guided-wide").unwrap();
    let narrow = vcx.debug_bounds("guided-narrow").unwrap();
    assert!(wide.size.width > narrow.size.width);
    assert_eq!(narrow.size.width, px(34.0));
}

#[test]
fn sidebar_style_metrics_preserve_minimal_and_keep_rows_positive() {
    let minimal = crate::ui_style::UiStyle::MINIMAL.tokens;
    assert_eq!(sidebar_row_height(true, true, false, false, minimal), 29.0);
    assert_eq!(sidebar_row_height(false, true, false, false, minimal), 45.0);
    assert_eq!(sidebar_row_height(false, false, true, false, minimal), 45.0);
    for style in [
        crate::ui_style::UiStyle::MINIMAL,
        crate::ui_style::UiStyle::DENSE,
        crate::ui_style::UiStyle::GUIDED,
    ] {
        let compact = sidebar_row_height(true, true, false, false, style.tokens);
        let expanded = sidebar_row_height(false, true, false, false, style.tokens);
        assert!(compact > 0.0 && compact < expanded);
    }
}

#[test]
fn sidebar_chat_height_tracks_visible_metadata() {
    assert_eq!(chat_row_height(false, false), 45.0);
    assert_eq!(chat_row_height(true, false), 61.0);
    assert_eq!(chat_row_height(false, true), 63.0);
    assert_eq!(chat_row_height(true, true), 63.0);
}

#[test]
fn sidebar_harness_geometry_reflects_row_hierarchy() {
    assert_eq!(SIDEBAR_ACTIVE_HARNESS_TITLE_GAP, Theme::SPACE_SM);
}

#[test]
fn sidebar_height_change_is_not_a_reorder() {
    let open = keys(&[("first-group", 105.0), ("second-group", 240.0)]);
    let collapsed = keys(&[("first-group", 40.0), ("second-group", 240.0)]);
    assert!(!sidebar_key_order_changed(&open, &collapsed));

    let reordered = keys(&[("second-group", 240.0), ("first-group", 40.0)]);
    assert!(sidebar_key_order_changed(&collapsed, &reordered));
}

#[test]
fn resort_offsets_empty_when_order_unchanged() {
    let order = keys(&[("a", 29.0), ("b", 29.0), ("c", 45.0)]);
    assert!(resort_offsets(&order, &order, 2.0).is_empty());
}

#[test]
fn resort_offsets_activity_moves_row_to_top() {
    // c (bottom, y=62) jumps to top: c glides down-from-above? No — c's
    // old y is 62, new y is 0 → starts +62 below… offset = old - new = +62,
    // painted at +62 decaying to 0 (a glide UP into place). a and b shift
    // down by c's height + gap (31).
    let old = keys(&[("a", 29.0), ("b", 29.0), ("c", 29.0)]);
    let new = keys(&[("c", 29.0), ("a", 29.0), ("b", 29.0)]);
    let offsets = resort_offsets(&old, &new, 2.0);
    assert_eq!(offsets.get("c"), Some(&62.0));
    assert_eq!(offsets.get("a"), Some(&-31.0));
    assert_eq!(offsets.get("b"), Some(&-31.0));
}

#[test]
fn resort_offsets_respect_heights_and_gap() {
    // Tall row (45px) swaps with a short one (29px).
    let old = keys(&[("tall", 45.0), ("short", 29.0)]);
    let new = keys(&[("short", 29.0), ("tall", 45.0)]);
    let offsets = resort_offsets(&old, &new, 2.0);
    // short: old y 47 → new y 0; tall: old y 0 → new y 31.
    assert_eq!(offsets.get("short"), Some(&47.0));
    assert_eq!(offsets.get("tall"), Some(&-31.0));
}

#[test]
fn resort_offsets_ignore_added_and_removed_keys() {
    let old = keys(&[("a", 29.0), ("gone", 29.0), ("b", 29.0)]);
    let new = keys(&[("new", 29.0), ("a", 29.0), ("b", 29.0)]);
    let offsets = resort_offsets(&old, &new, 2.0);
    // "new" has no old position (fades in instead); "gone" just goes.
    assert!(!offsets.contains_key("new"));
    assert!(!offsets.contains_key("gone"));
    // a: old 0 → new 31 (pushed down by the insert); b: 62 → 62 (gone's
    // slot replaced by "new" of equal height — no move, no entry).
    assert_eq!(offsets.get("a"), Some(&-31.0));
    assert_eq!(offsets.get("b"), None);
}

#[test]
fn resort_glide_spec_matches_original() {
    // §1.6: 260ms cubic-bezier(0.22, 1, 0.36, 1).
    assert_eq!(RESORT.duration_ms, 260);
    assert_eq!(RESORT.curve, motion::EASE_RESORT);
}

// ---- navigation history (titlebar back/forward) ----

fn chat(id: &str) -> NavEntry {
    NavEntry::Chat(id.to_string())
}

#[test]
fn nav_history_starts_with_nothing_to_walk() {
    let nav = NavHistory::new(chat(""));
    assert!(!nav.can_back());
    assert!(!nav.can_forward());
    assert_eq!(*nav.current(), chat(""));
}

#[test]
fn nav_push_then_back_and_forward() {
    let mut nav = NavHistory::new(chat("a"));
    nav.push(chat("b"));
    nav.push(NavEntry::Settings(SettingsSection::Devices));
    assert!(nav.can_back());
    assert!(!nav.can_forward());

    // Back walks toward the oldest entry without dropping anything.
    assert_eq!(
        nav.back(),
        Some(chat("b")),
        "back lands on the previous route"
    );
    assert_eq!(nav.back(), Some(chat("a")));
    assert!(!nav.can_back());
    assert!(nav.can_forward());
    assert_eq!(nav.back(), None, "past the oldest entry is a no-op");

    // Forward retraces the same path.
    assert_eq!(nav.forward(), Some(chat("b")));
    assert_eq!(
        nav.forward(),
        Some(NavEntry::Settings(SettingsSection::Devices))
    );
    assert!(!nav.can_forward());
    assert_eq!(nav.forward(), None);
}

#[test]
fn pr_review_chats_is_a_first_class_history_entry() {
    let mut nav = NavHistory::new(NavEntry::Overview);
    nav.push(NavEntry::PrReviewChats);
    assert_eq!(nav.back(), Some(NavEntry::Overview));
    assert_eq!(nav.forward(), Some(NavEntry::PrReviewChats));
}

#[test]
fn nav_push_dedups_the_current_route() {
    let mut nav = NavHistory::new(chat("a"));
    nav.push(chat("a"));
    nav.push(chat("a"));
    assert_eq!(nav.len(), 1, "re-selecting the current route never stacks");
    nav.push(NavEntry::Settings(SettingsSection::Agents));
    nav.push(NavEntry::Settings(SettingsSection::Agents));
    assert_eq!(nav.len(), 2);
}

#[test]
fn nav_push_truncates_the_forward_branch() {
    // a → b → c, back to a, then push d: the b/c branch is gone (browser
    // semantics — zeron's memory history PUSH truncates entries ahead).
    let mut nav = NavHistory::new(chat("a"));
    nav.push(chat("b"));
    nav.push(chat("c"));
    nav.back();
    nav.back();
    assert_eq!(*nav.current(), chat("a"));
    assert!(nav.can_forward());
    nav.push(chat("d"));
    assert!(!nav.can_forward(), "the old branch is unreachable");
    assert_eq!(nav.len(), 2);
    assert_eq!(nav.back(), Some(chat("a")));
    assert_eq!(nav.forward(), Some(chat("d")));
}

#[test]
fn nav_replace_swaps_in_place() {
    // The boot auto-select replaces the untouched canvas entry, so Back
    // stays disabled after landing in the last-used chat.
    let mut nav = NavHistory::new(chat(""));
    nav.replace(chat("boot"));
    assert_eq!(nav.len(), 1);
    assert_eq!(*nav.current(), chat("boot"));
    assert!(!nav.can_back());
}

#[test]
fn nav_settings_sections_are_distinct_entries() {
    let mut nav = NavHistory::new(chat("a"));
    nav.push(NavEntry::Settings(SettingsSection::Devices));
    nav.push(NavEntry::Settings(SettingsSection::Shortcuts));
    assert_eq!(nav.len(), 3, "section changes are navigations");
    assert_eq!(
        nav.back(),
        Some(NavEntry::Settings(SettingsSection::Devices))
    );
    assert_eq!(nav.back(), Some(chat("a")));
}

#[test]
fn sidebar_disclosure_motion_lands_exactly_on_its_target() {
    let mut tween = SidebarDisclosureMotion::new(1, 240.0, 0.0);
    tween.started = std::time::Instant::now() - motion::COLLAPSE.total().mul_f32(2.0);
    assert_eq!(tween.current(), 0.0);
    assert!(!tween.animating());
}

#[gpui::test]
fn render_updates_geometry_only_when_ready(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let (shell, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::new());
        let mut shell = Shell::new(state, EngineBootConfig {
            data_dir: dir.path().into(), ipc_port: 0,
            edge_url: "http://127.0.0.1:1".into(), edge_token: None,
            org_id: None, workos_client_id: None,
            default_harness: mast_proto::HarnessId::Mock,
        }, cx);
        shell.viewport_height = -1.0;
        shell.debug_gate = Some(GatePhase::Loading);
        shell
    });
    cx.update(|window, cx| window.draw(cx).clear());
    shell.read_with(cx, |shell, _| assert_eq!(shell.viewport_height, -1.0));
    shell.update(cx, |shell, cx| {
        shell.debug_gate = Some(GatePhase::Ready);
        cx.notify();
    });
    let expected = cx.update(|window, cx| {
        let height = f32::from(window.viewport_size().height);
        window.draw(cx).clear();
        height
    });
    shell.read_with(cx, |shell, _| assert_eq!(shell.viewport_height, expected));
}

#[gpui::test]
fn render_closes_browsers_when_workspace_profile_changes(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let shell = cx.add_window(|_, cx| {
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
    shell
        .update(cx, |shell, window, cx| {
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(mast_proto::WorkspaceScope::Local);
                state.local_device_id = Some("first".into());
            });
            let _ = shell.render(window, cx);
            let profile = shell.browser_profile.clone();
            shell.active_chat = "chat".into();
            shell.add_browser_surface(None, window, cx);
            assert_eq!(shell.browsers.len(), 1);
            let _ = shell.render(window, cx);
            assert_eq!(shell.browsers.len(), 1);
            shell
                .state
                .update(cx, |state, _| state.local_device_id = Some("second".into()));
            let _ = shell.render(window, cx);
            assert_ne!(shell.browser_profile, profile);
            assert!(shell.browsers.is_empty());
            assert!(shell.browser_subs.is_empty());
        })
        .unwrap();
}

#[gpui::test]
fn render_restores_focus_and_clears_hints_on_deactivation(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let (shell, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::new());
        let mut shell = Shell::new(state, EngineBootConfig {
            data_dir: dir.path().into(), ipc_port: 0,
            edge_url: "http://127.0.0.1:1".into(), edge_token: None,
            org_id: None, workos_client_id: None,
            default_harness: mast_proto::HarnessId::Mock,
        }, cx);
        shell.debug_gate = Some(GatePhase::Ready);
        shell
    });
    cx.update(|window, cx| { window.activate_window(); window.draw(cx).clear(); });
    shell.update(cx, |shell, cx| { shell.jump_hints = true; cx.notify(); });
    cx.deactivate_window();
    shell.read_with(cx, |shell, _| assert!(!shell.jump_hints));
    cx.update(|window, cx| { window.activate_window(); window.blur(); window.draw(cx).clear(); });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear();
        shell.read(cx).shortcut_focus.contains_focused(window, cx)
    }).then_some(()).expect("focus returns inside the shell");
}

#[gpui::test]
fn render_shortcuts_respect_overlay_and_settings(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        cx.bind_keys([
            KeyBinding::new(&platform_combo("mod-shift-o"), OpenOverview, None),
            KeyBinding::new(&platform_combo("mod-shift-b"), ToggleChanges, None),
        ]);
    });
    let (shell, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::new());
        let mut shell = Shell::new(state, EngineBootConfig {
            data_dir: dir.path().into(), ipc_port: 0,
            edge_url: "http://127.0.0.1:1".into(), edge_token: None,
            org_id: None, workos_client_id: None,
            default_harness: mast_proto::HarnessId::Mock,
        }, cx);
        shell.debug_gate = Some(GatePhase::Ready);
        shell.route = Route::Settings(SettingsSection::Devices);
        shell
    });
    cx.update(|window, cx| { window.activate_window(); window.draw(cx).clear(); });
    cx.run_until_parked();
    cx.simulate_keystrokes(&platform_combo("mod-shift-b"));
    shell.read_with(cx, |shell, cx| {
        assert!(matches!(shell.route, Route::Settings(_)));
        assert!(!shell.right_pane_open(cx));
    });
    shell.update(cx, |shell, cx| { shell.running.open = true; cx.notify(); });
    cx.update(|window, cx| window.draw(cx).clear());
    cx.simulate_keystrokes(&platform_combo("mod-shift-o"));
    shell.read_with(cx, |shell, _| assert!(matches!(shell.route, Route::Settings(_))));
    shell.update(cx, |shell, cx| { shell.running.open = false; cx.notify(); });
    cx.update(|window, cx| window.draw(cx).clear());
    cx.simulate_keystrokes(&platform_combo("mod-shift-o"));
    shell.read_with(cx, |shell, _| {
        assert!(matches!(shell.route, Route::Chat));
        assert!(shell.overview_navigation_visible());
    });
}
