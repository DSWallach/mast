use super::*;
use gpui::{Bounds, Pixels};

fn pr(url: &str) -> ParsedLink {
    ParsedLink {
        kind: LinkKind::Pr,
        value: url.to_string(),
    }
}

fn ticket(id: &str) -> ParsedLink {
    ParsedLink {
        kind: LinkKind::Ticket,
        value: id.to_string(),
    }
}

#[test]
fn link_input_parses_github_pr_urls_and_normalizes() {
    let canonical = "https://github.com/cofactr/zeron/pull/123";
    for raw in [
        "https://github.com/cofactr/zeron/pull/123",
        "  https://github.com/cofactr/zeron/pull/123  ",
        "http://github.com/cofactr/zeron/pull/123",
        "github.com/cofactr/zeron/pull/123",
        "https://www.github.com/cofactr/zeron/pull/123",
        "https://github.com/cofactr/zeron/pull/123/files",
        "https://github.com/cofactr/zeron/pull/123?foo=bar",
        "https://github.com/cofactr/zeron/pull/123#discussion_r1",
        "https://GitHub.com/cofactr/zeron/pull/123",
    ] {
        assert_eq!(parse_link_input(raw), Ok(pr(canonical)), "{raw:?}");
    }
}

#[test]
fn link_input_parses_ticket_ids_and_linear_urls() {
    assert_eq!(parse_link_input("ENG-1234"), Ok(ticket("ENG-1234")));
    assert_eq!(parse_link_input(" eng-1234 "), Ok(ticket("ENG-1234")));
    assert_eq!(parse_link_input("ab-1"), Ok(ticket("AB-1")));
    assert_eq!(
        parse_link_input("https://linear.app/cofactr/issue/ENG-42/fix-login"),
        Ok(ticket("ENG-42"))
    );
}

#[test]
fn link_input_rejects_garbage() {
    for raw in [
        "",
        "   ",
        "#123",
        "123",
        "hello world",
        "ENG-",
        "ENG1234",
        "E-1",       // one letter
        "ABCDEFG-1", // seven letters
        "ENG-12a",
        "ENG-1234 extra",
        "https://github.com/cofactr/zeron",
        "https://github.com/cofactr/zeron/issues/5",
        "https://github.com/cofactr/zeron/pull/",
        "https://github.com/cofactr/zeron/pull/12x",
        "https://gitlab.com/cofactr/zeron/pull/1",
        "https://linear.app/cofactr/project/abc",
    ] {
        assert_eq!(parse_link_input(raw), Err(LINK_PARSE_ERROR), "{raw:?}");
    }
}

#[test]
fn link_source_labels_cover_every_provenance() {
    use ChatLinkSource::*;
    assert_eq!(
        link_source_label(LinkKind::Pr, Some(Manual)),
        "Linked manually"
    );
    assert_eq!(
        link_source_label(LinkKind::Ticket, Some(Manual)),
        "Linked manually"
    );
    assert_eq!(
        link_source_label(LinkKind::Pr, Some(CreatedInChat)),
        "PR created in this chat"
    );
    assert_eq!(
        link_source_label(LinkKind::Pr, Some(Mentioned)),
        "Mentioned in conversation"
    );
    assert_eq!(
        link_source_label(LinkKind::Ticket, Some(Mentioned)),
        "Mentioned in conversation"
    );
    assert_eq!(
        link_source_label(LinkKind::Pr, None),
        "Inferred from branch"
    );
    assert_eq!(
        link_source_label(LinkKind::Ticket, None),
        "Inferred from branch"
    );
}

/// `CHAT_LINK_STATUS` wire shape with the new source fields — decoded
/// straight into `ChatLinkStatus` by `ensure_link_status`; snake_case
/// source values, `null` = inferred, and an older engine that omits them
/// still decodes.
#[test]
fn chat_link_status_decodes_source_fields() {
    let status: OverviewLinkStatus = serde_json::from_value(serde_json::json!({
        "pr": null, "ticket": null, "isWorktree": false, "diffStat": null,
        "prSource": "created_in_chat", "ticketSource": "manual"
    }))
    .unwrap();
    assert_eq!(status.pr_source, Some(ChatLinkSource::CreatedInChat));
    assert_eq!(status.ticket_source, Some(ChatLinkSource::Manual));
    let legacy: OverviewLinkStatus = serde_json::from_value(serde_json::json!({
        "pr": null, "ticket": null, "isWorktree": false, "diffStat": null
    }))
    .unwrap();
    assert_eq!(legacy.pr_source, None);
    assert_eq!(legacy.ticket_source, None);
}

#[test]
fn chat_link_status_prefers_multi_pr_links_and_matches_every_url() {
    let status: OverviewLinkStatus = serde_json::from_value(serde_json::json!({
        "pr": null,
        "ticket": null,
        "prLinks": [
            {
                "url": "https://github.com/a/b/pull/7",
                "source": "manual",
                "detail": null
            },
            {
                "url": "https://github.com/a/b/pull/8",
                "source": "mentioned",
                "detail": null
            }
        ]
    }))
    .unwrap();

    let links = effective_pr_links(&status);
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].url, "https://github.com/a/b/pull/7");
    assert_eq!(links[1].url, "https://github.com/a/b/pull/8");
    assert_eq!(link_fingerprint(&status).1.len(), 2);
}

#[test]
fn multi_pr_reply_appends_optimistically_and_removes_only_target() {
    let mut status = OverviewLinkStatus::default();
    let two: SetChatLinkReply = serde_json::from_value(serde_json::json!({
        "linkedPrLinks": [
            {"url": "https://github.com/a/b/pull/7", "source": "manual"},
            {"url": "https://github.com/a/b/pull/8", "source": "manual"}
        ]
    }))
    .unwrap();
    apply_link_reply(&mut status, LinkKind::Pr, &two);
    assert_eq!(effective_pr_links(&status).len(), 2);
    assert_eq!(
        status.pr_links[0].detail.as_ref().map(|pr| pr.number),
        Some(7)
    );

    let one: SetChatLinkReply = serde_json::from_value(serde_json::json!({
        "linkedPrLinks": [
            {"url": "https://github.com/a/b/pull/8", "source": "manual"}
        ]
    }))
    .unwrap();
    apply_link_reply(&mut status, LinkKind::Pr, &one);
    assert_eq!(effective_pr_links(&status).len(), 1);
    assert_eq!(
        effective_pr_links(&status)[0].url,
        "https://github.com/a/b/pull/8"
    );
}

#[test]
fn multi_pr_refetch_keeps_each_pending_placeholder_by_url() {
    let previous = OverviewLinkStatus {
        pr_links: vec![OverviewPrLink {
            url: "https://github.com/a/b/pull/8".into(),
            source: Some(ChatLinkSource::Manual),
            detail: Some(placeholder_pr("https://github.com/a/b/pull/8")),
        }],
        ..Default::default()
    };
    let next = OverviewLinkStatus {
        pr_links: vec![OverviewPrLink {
            url: "https://github.com/a/b/pull/8".into(),
            source: Some(ChatLinkSource::Manual),
            detail: None,
        }],
        ..Default::default()
    };
    let merged = merge_refetched_link(Some(&previous), next);
    assert_eq!(
        merged.pr_links[0].detail.as_ref().map(|pr| pr.number),
        Some(8)
    );
}

#[test]
fn set_chat_link_reply_applies_optimistically_and_merges_refetch() {
    let reply: SetChatLinkReply = serde_json::from_value(serde_json::json!({
        "linkedPrUrl": "https://github.com/a/b/pull/7", "linkedPrSource": "manual",
        "linkedTicketId": null, "linkedTicketSource": null
    }))
    .unwrap();
    let mut status = OverviewLinkStatus::default();
    apply_link_reply(&mut status, LinkKind::Pr, &reply);
    let placeholder = status.pr.as_ref().unwrap();
    assert_eq!(placeholder.number, 7);
    assert!(
        placeholder.action_reasons().is_empty(),
        "placeholder must not badge"
    );
    assert_eq!(status.pr_source, Some(ChatLinkSource::Manual));

    // Refetch lands before the engine cached the PR detail: keep ours.
    let refetched = OverviewLinkStatus {
        pr_source: Some(ChatLinkSource::Manual),
        ..Default::default()
    };
    let merged = merge_refetched_link(Some(&status), refetched);
    assert_eq!(merged.pr.as_ref().map(|p| p.number), Some(7));

    // Ticket set, then cleared.
    let set = SetChatLinkReply {
        linked_ticket_id: Some("ENG-9".into()),
        linked_ticket_source: Some(ChatLinkSource::Manual),
        ..Default::default()
    };
    apply_link_reply(&mut status, LinkKind::Ticket, &set);
    assert_eq!(
        status.ticket.as_ref().map(|t| t.identifier.as_str()),
        Some("ENG-9")
    );
    apply_link_reply(&mut status, LinkKind::Ticket, &SetChatLinkReply::default());
    assert!(status.ticket.is_none());
    assert_eq!(status.ticket_source, None);

    // Cleared (no source) refetch never resurrects the old value.
    let merged = merge_refetched_link(Some(&status), OverviewLinkStatus::default());
    assert!(merged.pr.is_none());
}

/// The panel follows the selection and closes itself when it's gone.
#[test]
fn panel_chat_id_follows_selection_only_while_open() {
    assert_eq!(panel_chat_id(false, Some("a")), None);
    assert_eq!(panel_chat_id(true, Some("a")), Some("a".to_string()));
    assert_eq!(panel_chat_id(true, None), None);
    assert_eq!(panel_chat_id(true, Some("")), None);
}

/// `ensure_subagents` decodes `SCAN_CHAT_SUBAGENTS`'s reply straight into
/// `SubagentItem`; a key-casing drift would silently yield no rows
/// (the decode error is swallowed). Payload shape captured verbatim from
/// a live engine reply, plus the pinned `status` field.
#[test]
fn scan_chat_subagents_wire_payload_decodes() {
    let wire = serde_json::json!([
        {
            "agentId": "a21f1650e0d5284f6",
            "description": "Research slash and @ mention semantics",
            "agentType": "fork",
            "transcriptPath": "/p/8de11ea8/subagents/agent-a21f1650e0d5284f6.jsonl",
            "status": "running"
        },
        { "agentId": "b", "transcriptPath": "/p/b.jsonl", "status": "done" },
        // Pre-`status` engine / unknown value: settled default.
        { "agentId": "c", "agentType": "Explore", "transcriptPath": "/p/c.jsonl" },
        { "agentId": "d", "transcriptPath": "/p/d.jsonl", "status": "exploded" }
    ]);
    let decoded: Vec<SubagentItem> = serde_json::from_value(wire).unwrap();
    assert_eq!(decoded.len(), 4);
    assert_eq!(decoded[0].agent_id, "a21f1650e0d5284f6");
    assert_eq!(decoded[0].agent_type.as_deref(), Some("fork"));
    assert_eq!(decoded[0].status, SubagentStatus::Running);
    assert_eq!(
        decoded[0].display_name(),
        "Research slash and @ mention semantics"
    );
    assert_eq!(decoded[1].description, None);
    assert_eq!(decoded[1].status, SubagentStatus::Done);
    // Web name fallback: description || agent_type || id.
    assert_eq!(decoded[1].display_name(), "b");
    assert_eq!(decoded[2].display_name(), "Explore");
    assert_eq!(decoded[2].status, SubagentStatus::Done);
    assert_eq!(decoded[3].status, SubagentStatus::Done);
    assert_eq!(
        SubagentStatus::Running.color(crate::theme::Appearance::Dark),
        crate::status_style::status_style(
            crate::status_style::Status::Working,
            crate::theme::Appearance::Dark
        )
        .color
    );
    assert_eq!(
        SubagentStatus::Done.color(crate::theme::Appearance::Dark),
        crate::status_style::status_style(
            crate::status_style::Status::Done,
            crate::theme::Appearance::Dark
        )
        .color
    );
}

/// The pinned `READ_SUBAGENT_TRANSCRIPT` reply shape decodes, with the
/// nullable fields (`timestamp`, `resultPreview`, `model`) and missing
/// optional ones tolerated.
#[test]
fn read_subagent_transcript_wire_payload_decodes() {
    let wire = serde_json::json!({
        "turns": [
            {"role": "user", "text": "Find the bug\nin foo.rs", "timestamp": "2026-09-29T10:15:00Z", "tools": []},
            {"role": "assistant", "text": "Looking.", "timestamp": null, "tools": [
                {"name": "Read", "inputPreview": "foo.rs", "resultPreview": "fn foo() {}"},
                {"name": "Bash", "inputPreview": "cargo test", "resultPreview": null}
            ]},
            {"role": "assistant", "text": "", "timestamp": null, "tools": []}
        ],
        "model": "claude-opus-5-5"
    });
    let t: SubagentTranscript = serde_json::from_value(wire).unwrap();
    assert_eq!(t.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(t.turns.len(), 3);
    assert_eq!(t.turns[0].role, "user");
    assert_eq!(t.turns[0].text.as_deref(), Some("Find the bug\nin foo.rs"));
    assert_eq!(
        t.turns[0].timestamp.as_deref(),
        Some("2026-09-29T10:15:00Z")
    );
    assert!(!format_turn_time(t.turns[0].timestamp.as_deref()).is_empty());
    assert_eq!(t.turns[1].timestamp, None);
    assert_eq!(format_turn_time(None), "");
    assert_eq!(format_turn_time(Some("not a time")), "");
    assert_eq!(
        t.turns[1].tools,
        vec![
            TranscriptTool {
                name: "Read".into(),
                input_preview: "foo.rs".into(),
                result_preview: Some("fn foo() {}".into())
            },
            TranscriptTool {
                name: "Bash".into(),
                input_preview: "cargo test".into(),
                result_preview: None
            },
        ]
    );
    // `model: null` and an empty object both decode.
    let t: SubagentTranscript =
        serde_json::from_value(serde_json::json!({"turns": [], "model": null})).unwrap();
    assert_eq!(t, SubagentTranscript::default());
    let t: SubagentTranscript = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(t.turns.is_empty());
    assert_eq!(turn_role_label("user"), "You");
    assert_eq!(turn_role_label("assistant"), "Assistant");
}

#[test]
fn peek_reply_guard_drops_stale_and_closed_requests() {
    let peek = |request_id| SubagentPeek {
        chat_id: "c".into(),
        agent_id: "a".into(),
        name: "n".into(),
        agent_type: None,
        request_id,
        load: PeekLoad::Loading,
        open_tools: HashSet::new(),
    };
    // The current request lands.
    assert!(peek_reply_is_current(Some(&peek(2)), 2));
    // An earlier request superseded by a quicker click (id 1 → 2) is dropped.
    assert!(!peek_reply_is_current(Some(&peek(2)), 1));
    // Closed since: nothing to land into.
    assert!(!peek_reply_is_current(None, 2));
}

#[test]
fn fenced_code_splits_out_of_plain_text() {
    assert_eq!(
        split_fenced_code("just text\nline 2"),
        vec![(false, "just text\nline 2".to_string())]
    );
    assert_eq!(
        split_fenced_code("before\n```rust\nfn a() {}\n```\nafter"),
        vec![
            (false, "before".to_string()),
            (true, "fn a() {}".to_string()),
            (false, "after".to_string()),
        ]
    );
    // Unterminated fence runs to the end.
    assert_eq!(
        split_fenced_code("```\nx\ny"),
        vec![(true, "x\ny".to_string())]
    );
    assert!(split_fenced_code("").is_empty());
}

#[test]
fn repo_key_matches_reference_repo_group_key() {
    assert_eq!(repo_key(Some("/Users/m/Projects/zeron")), "zeron");
    assert_eq!(repo_key(Some("/Users/m/Projects/zeron/")), "zeron");
    assert_eq!(repo_key(Some("/Users/m/Projects/zeron///")), "zeron");
    assert_eq!(repo_key(Some("relative")), "relative");
    assert_eq!(repo_key(Some("/")), overview_grouping::NONE_KEY);
    assert_eq!(repo_key(Some("")), overview_grouping::NONE_KEY);
    assert_eq!(repo_key(None), overview_grouping::NONE_KEY);
}

#[test]
fn repo_key_does_not_probe_privacy_managed_history() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .expect("test requires HOME");
    for child in ["Music", "Pictures", "Documents"] {
        let cwd = home.join(child).join("historical-chat");
        assert!(workspace_root_of(cwd.to_str().unwrap()).is_none());
        assert_eq!(repo_key(cwd.to_str()), "historical-chat");
    }
    assert!(!mast_engine::repos::is_automatic_access_blocked(
        &home.join("Projects").join("zeron")
    ));
}

#[test]
fn repo_key_resolves_worktree_via_workspace_root() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".workspace-root"),
        "/Users/m/Projects/agent-mode-tools\n",
    )
    .unwrap();
    let cwd = dir.path().to_str().unwrap();
    assert_eq!(repo_key(Some(cwd)), "agent-mode-tools");

    // Trailing slash on the recorded root is stripped like a cwd's.
    std::fs::write(
        dir.path().join(".workspace-root"),
        "/Users/m/Projects/zeron/\n",
    )
    .unwrap();
    assert_eq!(repo_key(Some(cwd)), "zeron");
}

#[test]
fn repo_key_workspace_root_empty_or_missing_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let basename = dir.path().file_name().unwrap().to_str().unwrap();

    // No file: unchanged behavior.
    assert_eq!(repo_key(Some(cwd)), basename);

    // Empty / whitespace-only file: fallback.
    std::fs::write(dir.path().join(".workspace-root"), "").unwrap();
    assert_eq!(repo_key(Some(cwd)), basename);
    std::fs::write(dir.path().join(".workspace-root"), "  \n").unwrap();
    assert_eq!(repo_key(Some(cwd)), basename);

    // Unreadable (a directory, not a file): fallback, no panic.
    std::fs::remove_file(dir.path().join(".workspace-root")).unwrap();
    std::fs::create_dir(dir.path().join(".workspace-root")).unwrap();
    assert_eq!(repo_key(Some(cwd)), basename);
}

#[test]
fn worktree_repo_key_uses_owning_space_without_workspace_root_marker() {
    use chrono::TimeZone;

    let space = Space {
        id: "workspace-space".into(),
        device_id: "device".into(),
        path: "/Users/example/Projects/cofactr/workspace".into(),
        name: None,
        git_detected: true,
        git_checked_at: None,
        checkout_id: Some("main-checkout".into()),
        created_at: Utc.timestamp_opt(0, 0).unwrap(),
    };
    let mut chat = row(
        "chat",
        "/Users/example/.zeron/worktrees/workspace/swift-otter",
        None,
    )
    .chat;
    chat.space_id = Some(space.id.clone());

    assert_eq!(repo_key_for_chat(&chat, Some(&space)), "workspace");
}

#[test]
fn owning_space_does_not_change_ordinary_cwd_repo_key() {
    use chrono::TimeZone;

    let space = Space {
        id: "workspace-space".into(),
        device_id: "device".into(),
        path: "/Users/example/Projects/cofactr/workspace/".into(),
        name: Some("Custom project name".into()),
        git_detected: true,
        git_checked_at: None,
        checkout_id: Some("main-checkout".into()),
        created_at: Utc.timestamp_opt(0, 0).unwrap(),
    };
    let chat = row("chat", "/Users/example/Projects/cofactr/workspace", None).chat;

    assert_eq!(repo_key_for_chat(&chat, Some(&space)), "workspace");
    assert_eq!(repo_key_for_chat(&chat, None), "workspace");
}

#[test]
fn ticket_group_key_prefers_ticket_then_pr_then_none() {
    assert_eq!(ticket_group_key(Some("ENG-1"), Some(7)), "ENG-1");
    assert_eq!(ticket_group_key(None, Some(7)), "PR #7");
    assert_eq!(ticket_group_key(None, None), overview_grouping::NONE_KEY);
}

#[test]
fn repo_filter_toggle_all_none_and_label() {
    let known: BTreeSet<String> = ["ui", "zeron", overview_grouping::NONE_KEY]
        .into_iter()
        .map(String::from)
        .collect();
    let mut hidden = BTreeSet::new();
    assert_eq!(repo_filter_label(hidden.len()), "Repos");

    toggle_repo_hidden(&mut hidden, "ui");
    assert!(hidden.contains("ui"));
    assert_eq!(repo_filter_label(hidden.len()), "Repos (1 hidden)");
    toggle_repo_hidden(&mut hidden, "ui");
    assert!(hidden.is_empty());

    // "None" hides every known repo; "All" clears.
    hidden.extend(known.iter().cloned());
    assert_eq!(repo_filter_label(hidden.len()), "Repos (3 hidden)");
    hidden.clear();
    assert_eq!(repo_filter_label(hidden.len()), "Repos");
}

#[test]
fn repo_list_is_alphabetical_with_none_last() {
    let known: BTreeSet<String> = [
        overview_grouping::NONE_KEY,
        "zeron",
        "agent-mode-tools",
        "ui",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        sorted_repo_keys(&known),
        vec![
            "agent-mode-tools",
            "ui",
            "zeron",
            overview_grouping::NONE_KEY
        ]
    );
}

#[test]
fn search_is_case_insensitive_substring_over_present_fields() {
    let fields = [
        Some("Fix Login Bug"),
        None,
        Some("/Users/m/ui"),
        Some("ENG-42"),
    ];
    assert!(matches_search("", &fields));
    assert!(matches_search("login", &fields));
    assert!(matches_search("eng-42", &fields));
    assert!(matches_search("users/m", &fields));
    assert!(!matches_search("zeron", &fields));
    assert!(!matches_search("x", &[None, None]));
}

#[test]
fn hovered_row_reconciliation_drops_only_unmounted_ids() {
    let mounted = vec!["a".to_string(), "b".to_string()];
    let mut hovered = Some("b".to_string());
    assert!(!reconcile_hovered_row(&mut hovered, &mounted));
    assert_eq!(hovered.as_deref(), Some("b"));

    let mounted = vec!["a".to_string()];
    assert!(reconcile_hovered_row(&mut hovered, &mounted));
    assert!(hovered.is_none());
    assert!(!reconcile_hovered_row(&mut hovered, &[]));
}

#[test]
fn view_prefs_round_trip_through_the_flag_set_and_keep_other_flags() {
    let prefs = ViewPrefs {
        mode: OverviewMode::Outline,
        hide_stale: true,
        show_archived: true,
        groups: vec![GroupDimension::Ticket, GroupDimension::Repo],
    };
    let mut flags: BTreeSet<String> = ["someFutureFlag".to_string()].into();
    apply_view_prefs_to_flags(&mut flags, &prefs);
    assert_eq!(
        view_prefs_from_flags(&flags),
        prefs,
        "activation order survives"
    );
    assert!(flags.contains("groupBy:ticket,repo"));
    // Unrelated flags survive and don't read as view prefs.
    assert!(flags.contains("someFutureFlag"));

    // Turning everything off removes every view-pref entry.
    apply_view_prefs_to_flags(&mut flags, &ViewPrefs::default());
    assert_eq!(view_prefs_from_flags(&flags), ViewPrefs::default());
    assert!(!flags.iter().any(|f| f.starts_with(GROUP_BY_PREFIX)));
    // Changing the grouping replaces (never accumulates) the groupBy entry.
    let mut flags = BTreeSet::new();
    apply_view_prefs_to_flags(
        &mut flags,
        &ViewPrefs {
            groups: vec![GroupDimension::Repo],
            ..Default::default()
        },
    );
    apply_view_prefs_to_flags(
        &mut flags,
        &ViewPrefs {
            groups: vec![GroupDimension::Ticket],
            ..Default::default()
        },
    );
    assert_eq!(
        flags,
        BTreeSet::from(["groupBy:ticket".to_string(), VIEW_OUTLINE_FLAG.to_string()])
    );
}

/// A saved `groupBy` written while Category and Origin grouping existed
/// still loads: the removed dimensions are dropped, the rest kept.
#[test]
fn saved_groupings_with_removed_dimensions_load_without_them() {
    let load = |value: &str| {
        view_prefs_from_flags(&BTreeSet::from([format!("{GROUP_BY_PREFIX}{value}")])).groups
    };
    assert_eq!(load("category,repo"), vec![GroupDimension::Repo]);
    assert_eq!(load("category,origin,repo"), vec![GroupDimension::Repo]);
    assert_eq!(load("origin"), Vec::<GroupDimension>::new());
    assert_eq!(
        load("origin,ticket,category,repo"),
        vec![GroupDimension::Ticket, GroupDimension::Repo]
    );
}

#[test]
fn view_prefs_defaults_and_junk_degrade_gracefully() {
    assert_eq!(
        view_prefs_from_flags(&BTreeSet::new()),
        ViewPrefs::default()
    );
    let junk: BTreeSet<String> = [
        "groupBy:bogus,repo,repo,,Origin, ticket".to_string(),
        "viewMode:myPrs".to_string(),
    ]
    .into();
    let prefs = view_prefs_from_flags(&junk);
    // Unknown dropped, duplicate dropped, whitespace trimmed, case-sensitive keys.
    assert_eq!(
        prefs.groups,
        vec![GroupDimension::Repo, GroupDimension::Ticket]
    );
    assert_eq!(prefs.mode, OverviewMode::Outline);
    // Every dimension's key round-trips.
    for dim in [GroupDimension::Repo, GroupDimension::Ticket] {
        assert_eq!(
            GroupDimension::from_persist_key(dim.persist_key()),
            Some(dim)
        );
    }
}

/// Overview-mode persistence: the removed Canvas view's legacy flag loads
/// as Outline, exactly one flag is ever written (never the legacy one),
/// and unknown/conflicting flags degrade deterministically.
#[test]
fn overview_mode_loads_a_saved_canvas_view_as_outline_and_writes_exactly_one() {
    let flags =
        |keys: &[&str]| -> BTreeSet<String> { keys.iter().map(|k| k.to_string()).collect() };
    assert_eq!(OverviewMode::from_flags(&flags(&[])), OverviewMode::Outline);
    // The removed Canvas view: alone, or next to the outline flag.
    assert_eq!(
        OverviewMode::from_flags(&flags(&["viewMode:canvas"])),
        OverviewMode::Outline
    );
    assert_eq!(
        OverviewMode::from_flags(&flags(&["viewMode:outline", "viewMode:canvas"])),
        OverviewMode::Outline
    );
    assert_eq!(
        OverviewMode::from_flags(&flags(&["viewMode:outline"])),
        OverviewMode::Outline
    );
    // Unknown / stale values read as Outline, never fail.
    assert_eq!(
        OverviewMode::from_flags(&flags(&["viewMode:myPrs", "viewMode:board"])),
        OverviewMode::Outline
    );
    // The writer purges every `viewMode:` entry and writes one.
    for mode in [OverviewMode::List, OverviewMode::Outline] {
        let mut set = flags(&[
            "viewMode:canvas",
            "viewMode:outline",
            "viewMode:myPrs",
            "hideStale",
            "prSidebarOpen",
        ]);
        apply_view_prefs_to_flags(
            &mut set,
            &ViewPrefs {
                mode,
                hide_stale: true,
                ..Default::default()
            },
        );
        let modes: Vec<&String> = set
            .iter()
            .filter(|f| f.starts_with(VIEW_MODE_PREFIX))
            .collect();
        assert_eq!(
            modes.len(),
            usize::from(mode != OverviewMode::List),
            "{mode:?}: {set:?}"
        );
        assert!(!set.contains(LEGACY_VIEW_CANVAS_FLAG));
        assert_eq!(view_prefs_from_flags(&set).mode, OverviewMode::Outline);
        assert!(set.contains("prSidebarOpen"), "unrelated flags survive");
    }
    // Re-saving a loaded Canvas view writes only the outline flag.
    for saved in [
        &["viewMode:canvas", "hideStale"][..],
        &["viewMode:canvas", "viewMode:outline", "hideStale"][..],
    ] {
        let mut set = flags(saved);
        let prefs = view_prefs_from_flags(&set);
        assert_eq!(prefs.mode, OverviewMode::Outline);
        apply_view_prefs_to_flags(&mut set, &prefs);
        assert_eq!(set, flags(&["viewMode:outline", "hideStale"]), "{saved:?}");
    }
}

/// A flags file written while the "My PRs" sidebar existed still loads:
/// the retired `prSidebarOpen` / `prSectionCollapsed:*` entries are
/// ignored and the surviving flags keep their values.
#[test]
fn flags_file_with_retired_pr_sidebar_entries_still_loads() {
    let dir = std::env::temp_dir().join(format!("zeron-overview-oldflags-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(overview_positions::UI_FLAGS_FILE),
        r#"["prSidebarOpen","prSectionCollapsed:needs-review","prSectionCollapsed:repo:cofactr/zeron","viewMode:outline","hideStale"]"#,
    )
    .unwrap();
    let loaded = overview_positions::load_string_set(&dir, overview_positions::UI_FLAGS_FILE);
    assert_eq!(loaded.len(), 5, "the old file loads without error");
    let prefs = view_prefs_from_flags(&loaded);
    assert_eq!(prefs.mode, OverviewMode::Outline);
    assert!(prefs.hide_stale);
    assert!(!prefs.show_archived);
    // The retired entries are left in place, not rewritten.
    let mut rewritten = loaded.clone();
    apply_view_prefs_to_flags(&mut rewritten, &prefs);
    assert!(rewritten.contains("prSidebarOpen"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn view_prefs_round_trip_through_the_string_set_file() {
    let dir = std::env::temp_dir().join(format!("zeron-overview-viewprefs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let prefs = ViewPrefs {
        mode: OverviewMode::Outline,
        hide_stale: false,
        show_archived: true,
        groups: vec![GroupDimension::Ticket, GroupDimension::Repo],
    };
    let mut flags = overview_positions::load_string_set(&dir, overview_positions::UI_FLAGS_FILE);
    apply_view_prefs_to_flags(&mut flags, &prefs);
    overview_positions::save_string_set(&dir, overview_positions::UI_FLAGS_FILE, &flags).unwrap();
    let loaded = overview_positions::load_string_set(&dir, overview_positions::UI_FLAGS_FILE);
    assert_eq!(view_prefs_from_flags(&loaded), prefs);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stale_repo_filter_detection() {
    let set = |keys: &[&str]| -> BTreeSet<String> { keys.iter().map(|k| k.to_string()).collect() };
    // No chats known yet: says nothing.
    assert!(!restored_repo_filter_hides_everything(
        &set(&["a"]),
        &set(&[])
    ));
    // Names only repos that no longer have chats: hides nothing.
    assert!(!restored_repo_filter_hides_everything(
        &set(&["gone"]),
        &set(&["a", "b"])
    ));
    // Partial hide is a legitimate filter.
    assert!(!restored_repo_filter_hides_everything(
        &set(&["a"]),
        &set(&["a", "b"])
    ));
    // Hides every repo that has chats.
    assert!(restored_repo_filter_hides_everything(
        &set(&["a", "b", "gone"]),
        &set(&["a", "b"])
    ));
    // Nothing hidden.
    assert!(!restored_repo_filter_hides_everything(
        &set(&[]),
        &set(&["a"])
    ));
}

#[test]
fn chats_become_stale_only_after_a_full_week_of_inactivity() {
    let now = Utc::now();
    let mut chat = row("chat", "/repo", None).chat;
    chat.last_message_at = Some(now - chrono::Duration::days(7));
    assert!(!is_stale(&chat, ChatIndicator::Idle, now));

    chat.last_message_at = Some(now - chrono::Duration::days(7) - chrono::Duration::seconds(1));
    assert!(is_stale(&chat, ChatIndicator::Idle, now));
    assert!(!is_stale(&chat, ChatIndicator::Working, now));
    assert!(!is_stale(&chat, ChatIndicator::AwaitingInput, now));
}

/// Build an `Overview` window over `state` (seeded with chats in the
/// given repos) and render once so `ensure_prefs_loaded` runs.
fn overview_over<'a>(
    cx: &'a mut gpui::TestAppContext,
    state: &Entity<AppState>,
) -> (Entity<Overview>, &'a mut gpui::VisualTestContext) {
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let state = state.clone();
    let (overview, vcx) = cx.add_window_view(move |_, cx| {
        Overview::new(
            state,
            gpui::WeakEntity::new_invalid(),
            transcript,
            composer,
            cx,
        )
    });
    vcx.run_until_parked();
    (overview, vcx)
}

#[gpui::test]
fn filter_menu_toggles_and_persists_both_options(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with_repos(cx, dir.path(), &["alpha"]);
    let (overview, vcx) = overview_over(cx, &state);
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("overview-mode-List").is_none());
    assert!(vcx.debug_bounds("overview-hide-stale").is_none());
    let trigger = vcx.debug_bounds("overview-filter-menu").unwrap().center();
    vcx.simulate_click(trigger, gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
    for selector in ["overview-hide-stale", "overview-show-archived"] {
        let item = vcx.debug_bounds(selector).unwrap().center();
        vcx.simulate_click(item, gpui::Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
    }
    overview.read_with(vcx, |overview, _| {
        assert!(overview.hide_stale && overview.show_archived);
    });
    let (restored, vcx) = overview_over(cx, &state);
    restored.read_with(vcx, |overview, _| {
        assert!(overview.hide_stale && overview.show_archived);
        assert_eq!(overview.view_mode, OverviewMode::Outline);
    });
}

#[gpui::test]
fn find_focuses_and_selects_the_search_in_every_mode(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with_repos(cx, dir.path(), &["alpha"]);
    let (overview, vcx) = overview_over(cx, &state);
    for mode in [OverviewMode::List, OverviewMode::Outline] {
        overview.update(vcx, |overview, cx| {
            overview.view_mode = mode;
            overview
                .search
                .update(cx, |search, cx| search.set_text("old query", cx));
            cx.notify();
        });
        vcx.update(|window, cx| {
            window.blur();
            window.draw(cx).clear();
        });

        vcx.update(|window, cx| {
            assert!(overview.update(cx, |overview, cx| overview.focus_search(window, cx)));
            assert!(overview.read(cx).search_focused(window, cx), "{mode:?}");
        });
        vcx.simulate_keystrokes("replacement");
        overview.read_with(vcx, |overview, cx| {
            assert_eq!(overview.search.read(cx).text(), "replacement", "{mode:?}");
        });
    }
}

fn state_with_repos(
    cx: &mut gpui::TestAppContext,
    dir: &std::path::Path,
    repos: &[&str],
) -> Entity<AppState> {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let state = cx.new(|_| AppState::new());
    let repos: Vec<String> = repos.iter().map(|r| format!("/work/{r}")).collect();
    let dir = dir.to_path_buf();
    state.update(cx, |state, _| {
        state.data_dir = Some(dir);
        state.chats = repos
            .iter()
            .enumerate()
            .map(|(i, cwd)| {
                let mut chat = row(&format!("c{i}"), cwd, None).chat;
                chat.last_message_at = Some(Utc::now());
                chat
            })
            .collect();
    });
    state
}

/// Reload simulation: one overview changes every persisted piece of view
/// state through the real handlers, a brand-new overview over the same
/// data dir (app restart, or the page being rebuilt) restores all of it.
#[gpui::test]
fn view_state_survives_a_fresh_overview(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with_repos(cx, dir.path(), &["alpha", "beta"]);
    let (first, vcx) = overview_over(cx, &state);
    first.update(vcx, |o, cx| {
        assert!(o.prefs_loaded, "data_dir was available at first render");
        o.view_mode = OverviewMode::Outline;
        o.view_prefs_changed(cx);
        o.hide_stale = true;
        o.view_prefs_changed(cx);
        o.show_archived = true;
        o.view_prefs_changed(cx);
        o.toggle_group(GroupDimension::Repo, cx);
        o.toggle_group(GroupDimension::Ticket, cx);
        o.toggle_group(GroupDimension::Repo, cx); // off again
        toggle_repo_hidden(&mut o.hidden_repos, "beta");
        o.repo_filter_changed(cx);
    });

    let (second, vcx) = overview_over(cx, &state);
    second.update(vcx, |o, _| {
        assert_eq!(o.view_mode, OverviewMode::Outline);
        assert!(o.hide_stale);
        assert!(o.show_archived);
        assert_eq!(o.active_groups, vec![GroupDimension::Ticket]);
        assert_eq!(o.hidden_repos, BTreeSet::from(["beta".to_string()]));
    });

    // And turning things back off persists too.
    second.update(vcx, |o, cx| {
        o.view_mode = OverviewMode::List;
        o.view_prefs_changed(cx);
        o.hide_stale = false;
        o.view_prefs_changed(cx);
        o.show_archived = false;
        o.view_prefs_changed(cx);
        o.toggle_group(GroupDimension::Ticket, cx);
    });
    let (third, vcx) = overview_over(cx, &state);
    third.update(vcx, |o, _| {
        assert_eq!(o.view_prefs(), ViewPrefs::default());
        assert_eq!(o.hidden_repos, BTreeSet::from(["beta".to_string()]));
    });
}

/// A restored repo filter that hides every repo with chats is reset (and
/// the reset persisted); a filter naming only vanished repos is inert; a
/// partial filter is kept.
#[gpui::test]
fn clean_frames_share_the_cached_rows(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with_repos(cx, dir.path(), &["alpha", "beta"]);
    let (overview, vcx) = overview_over(cx, &state);
    overview.update(vcx, |o, cx| {
        let first = o.rows(cx);
        let second = o.rows(cx);
        assert!(
            std::rc::Rc::ptr_eq(&first, &second),
            "hover/scroll frames must not rebuild or deep-clone rows"
        );
        o.rows_dirty = true;
        assert!(!std::rc::Rc::ptr_eq(&first, &o.rows(cx)));
    });
}

/// Synthetic busy session: 250 chats, one of them streaming. Counts what
/// the Overview does per `AppState` change; prints the counts (run with
/// `--nocapture`) and pins the post-fix bounds.
#[gpui::test]
fn streaming_one_chat_among_many_does_little_overview_work(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..250).map(|i| format!("repo{i}")).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let state = state_with_repos(cx, dir.path(), &name_refs);
    let (overview, vcx) = overview_over(cx, &state);
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
    };
    draw(vcx);
    // Steady state: every chat's fetches have already landed (no engine
    // here to do it for real).
    overview.update(vcx, |o, _| {
        for name in 0..250 {
            let id = format!("c{name}");
            o.link_status.insert(
                id.clone(),
                LinkStatusEntry {
                    status: OverviewLinkStatus::default(),
                    diff_stat: None,
                    is_worktree: None,
                    fetched_at: Instant::now(),
                },
            );
            o.context_usage_fetched_at
                .insert(id.clone(), Instant::now());
            o.subagents_fetched_at.insert(id, Instant::now());
        }
        o.rows_dirty = true;
    });
    draw(vcx);
    let before = overview.read_with(vcx, |o, _| o.perf);
    let started = std::time::Instant::now();
    for i in 0..50 {
        // A streaming frame: the selected chat's transcript changes, no
        // chat/space/session field does.
        state.update(vcx, |state, cx| {
            state.selected_chat = Some("c0".into());
            state.transcript.clear();
            state.queue.clear();
            let _ = i;
            cx.notify();
        });
        vcx.run_until_parked();
        draw(vcx);
    }
    let elapsed = started.elapsed();
    let after = overview.read_with(vcx, |o, _| o.perf);
    let delta = OverviewPerf {
        rows_rebuilds: after.rows_rebuilds - before.rows_rebuilds,
        observer_calls: after.observer_calls - before.observer_calls,
        observer_notifies: after.observer_notifies - before.observer_notifies,
        fetch_checks: after.fetch_checks - before.fetch_checks,
        fetch_requests: after.fetch_requests - before.fetch_requests,
    };
    eprintln!("PERF 250 chats x 50 streaming updates: {delta:?} in {elapsed:?}");
    // Before the fingerprint gate, the row-list gate and the fetch-due
    // gate: 50 rebuilds, 50 notifies, 12500 fetch checks. Only the first
    // update (it changes the selection) may rebuild or repaint.
    assert_eq!(delta.observer_calls, 50);
    assert!(delta.rows_rebuilds <= 1, "{delta:?}");
    assert!(delta.observer_notifies <= 1, "{delta:?}");
    assert_eq!(delta.fetch_checks, 0, "{delta:?}");

    // A real change to the streaming chat's row still gets through.
    state.update(vcx, |state, cx| {
        state.chats[0].last_message_preview = Some("new preview".into());
        cx.notify();
    });
    vcx.run_until_parked();
    draw(vcx);
    let rows = overview.update(vcx, |o, cx| o.rows(cx));
    let row = rows.iter().find(|r| r.chat.id == "c0").unwrap();
    assert_eq!(
        row.chat.last_message_preview.as_deref(),
        Some("new preview")
    );
}

#[gpui::test]
fn restored_stale_repo_filter_degrades_gracefully(cx: &mut gpui::TestAppContext) {
    let save = |dir: &std::path::Path, keys: &[&str]| {
        let set: BTreeSet<String> = keys.iter().map(|k| k.to_string()).collect();
        overview_positions::save_string_set(dir, overview_positions::HIDDEN_REPOS_FILE, &set)
            .unwrap();
    };

    // Hides every repo that has chats (plus one that vanished) -> reset.
    let dir = tempfile::tempdir().unwrap();
    save(dir.path(), &["alpha", "beta", "vanished"]);
    let state = state_with_repos(cx, dir.path(), &["alpha", "beta"]);
    let (overview, vcx) = overview_over(cx, &state);
    overview.update(vcx, |o, cx| {
        let rows = o.rows(cx);
        assert_eq!(rows.len(), 2, "nothing is filtered out invisibly");
        assert!(o.hidden_repos.is_empty());
    });
    assert!(
        overview_positions::load_string_set(dir.path(), overview_positions::HIDDEN_REPOS_FILE)
            .is_empty()
    );

    // Only a vanished repo: no crash, nothing hidden, entry left alone.
    let dir = tempfile::tempdir().unwrap();
    save(dir.path(), &["vanished"]);
    let state = state_with_repos(cx, dir.path(), &["alpha"]);
    let (overview, vcx) = overview_over(cx, &state);
    overview.update(vcx, |o, cx| {
        assert_eq!(o.rows(cx).len(), 1);
        assert_eq!(o.hidden_repos, BTreeSet::from(["vanished".to_string()]));
    });

    // Partial filter stays.
    let dir = tempfile::tempdir().unwrap();
    save(dir.path(), &["alpha"]);
    let state = state_with_repos(cx, dir.path(), &["alpha", "beta"]);
    let (overview, vcx) = overview_over(cx, &state);
    overview.update(vcx, |o, cx| {
        let rows = o.rows(cx);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repo, "beta");
        assert_eq!(o.hidden_repos, BTreeSet::from(["alpha".to_string()]));
    });

    // A deliberate "None" made AFTER the restore is not undone.
    overview.update(vcx, |o, cx| {
        let known = o.known_repos.clone();
        o.hidden_repos.extend(known);
        o.repo_filter_changed(cx);
        assert_eq!(o.rows(cx).len(), 0);
        assert_eq!(o.hidden_repos.len(), 2);
    });
}

#[gpui::test]
fn embedded_chat_panel_exposes_and_copies_chat_identity(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        let mut chat = row("chat-7", "/r", None).chat;
        chat.title = Some("Fix login".into());
        chat.harness_session_id = Some("sess-9".into());
        chat.last_message_at = Some(Utc::now());
        state.chats = vec![chat];
        state.selected_chat = Some("chat-7".into());
    });
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let (overview, vcx) = cx.add_window_view(|_, cx| {
        Overview::new(
            state.clone(),
            gpui::WeakEntity::new_invalid(),
            transcript,
            composer,
            cx,
        )
    });
    overview.update(vcx, |overview, cx| {
        overview.chat_panel_open = true;
        let theme = Theme::of(cx).clone();
        assert!(overview.render_chat_panel(&theme, cx).is_some());
        overview.copy_panel_chat_identity("chat-7".into(), cx);
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(
        vcx.debug_bounds("chat-header-metadata").is_some(),
        "embedded chat panel mounts the shared metadata strip"
    );
    overview.read_with(vcx, |overview, cx| {
        assert!(overview.chat_panel_open);
        assert_eq!(
            overview.state.read(cx).selected_chat.as_deref(),
            Some("chat-7")
        );
        assert_eq!(overview.copied_panel_identity.as_deref(), Some("chat-7"));
    });
    assert_eq!(
        vcx.read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some("Fix login \u{2014} chat chat-7 \u{2014} session sess-9")
    );
}

#[test]
fn acknowledged_done_resets_when_status_leaves_done() {
    let mut ack: BTreeSet<String> = ["a".to_string()].into();
    assert!(!is_unread_done(&ack, "a", true));
    assert!(is_unread_done(&ack, "b", true));
    assert!(!is_unread_done(&ack, "b", false));
    // Still done: nothing to reconcile.
    assert!(!reconcile_acknowledged(&mut ack, "a", true));
    assert!(ack.contains("a"));
    // Left done: dropped, so the next completion is unread again.
    assert!(reconcile_acknowledged(&mut ack, "a", false));
    assert!(!ack.contains("a"));
    assert!(is_unread_done(&ack, "a", true));
    // Not in the set: no change reported (no redundant save).
    assert!(!reconcile_acknowledged(&mut ack, "a", false));
}

#[test]
fn status_style_maps_zeron_indicators_to_reference_vocabulary() {
    assert_eq!(
        status_style(ChatIndicator::AwaitingInput),
        ("!", "needs input")
    );
    assert_eq!(status_style(ChatIndicator::Completed), ("✓", "done"));
    assert_eq!(status_style(ChatIndicator::Idle), ("○", "idle"));
    assert_eq!(status_style(ChatIndicator::Working), ("◌", "working"));
    assert_eq!(status_style(ChatIndicator::Errored), ("✕", "error"));
}

#[test]
fn worktree_cwds_match_reference_rules() {
    assert!(cwd_is_worktree(Some("/r/ws/.worktrees/eng-1")));
    assert!(cwd_is_worktree(Some("/r/ws/.agent-worktrees/x/y")));
    assert!(!cwd_is_worktree(Some("/r/ws")));
    assert!(!cwd_is_worktree(None));
}

#[test]
fn group_labels_use_reference_meta_tables() {
    assert_eq!(
        group_label(GroupDimension::Repo, overview_grouping::NONE_KEY),
        "No repo"
    );
    assert_eq!(
        group_label(GroupDimension::Ticket, overview_grouping::NONE_KEY),
        "No ticket / PR"
    );
    assert_eq!(group_label(GroupDimension::Repo, "zeron"), "zeron");
}

#[test]
fn context_label_and_fill_match_reference_face() {
    assert_eq!(context_pct_label(None), "— context");
    assert_eq!(context_pct_label(Some(0.0)), "0% context");
    assert_eq!(context_pct_label(Some(0.426)), "43% context");
    assert_eq!(context_pct_label(Some(1.0)), "100% context");
    assert_eq!(context_fill_fraction(None), 0.0);
    assert_eq!(context_fill_fraction(Some(0.426)), 0.43);
    assert_eq!(context_fill_fraction(Some(2.0)), 1.0);
}

#[test]
fn context_refetch_only_counts_as_changed_when_the_percent_moves() {
    // First fetch: null is no change, a value is.
    assert!(!context_pct_changed(None, None));
    assert!(context_pct_changed(None, Some(0.3)));
    // Same value, or sub-percent drift: no-op (no relayout).
    assert!(!context_pct_changed(Some(Some(0.3)), Some(0.3)));
    assert!(!context_pct_changed(Some(Some(0.301)), Some(0.304)));
    // Crossing a whole percent, or going to/from null: changed.
    assert!(context_pct_changed(Some(Some(0.30)), Some(0.31)));
    assert!(context_pct_changed(Some(Some(0.30)), None));
    assert!(!context_pct_changed(Some(None), None));
}

fn row(id: &str, cwd: &str, pct: Option<f32>) -> OverviewRow {
    use chrono::TimeZone;
    let chat = Chat {
        id: id.into(),
        device_id: "d".into(),
        title: None,
        archived: false,
        cwd: Some(cwd.into()),
        branch: None,
        checkout_id: None,
        source_context: None,
        config: None,
        last_message_preview: None,
        last_message_at: None,
        created_at: Utc.timestamp_opt(0, 0).unwrap(),
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
    };
    OverviewRow {
        status: ChatIndicator::Idle,
        repo: repo_key(chat.cwd.as_deref()),
        chat,
        stale: false,
        ticket: None,
        pr_number: None,
        pr_title: None,
        pr_search_values: Vec::new(),
        context_pct: pct,
        subagent_count: 0,
        closeable: false,
        work_item_path: None,
        user_action_required: false,
        user_action_count: 0,
        instruction_drift: false,
        execution_exception: false,
    }
}

#[test]
fn card_model_label_uses_explicit_id_or_honest_default() {
    let mut chat = row("c", "/r/zeron", None).chat;
    assert_eq!(chat_model_label(&chat), "default model");
    chat.config = Some(mast_proto::ChatConfig {
        approval_rules: false,
        harness: mast_proto::HarnessId::Codex,
        model: Some("gpt-6-codex".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
        auto_approve: false,
    });
    assert_eq!(chat_model_label(&chat), "gpt-6-codex");
    // What the harness reported wins over the stored pick, and a chat
    // with no stored model shows what ran (the CLI default), not "default".
    chat.actual_model = Some("claude-fable-5".into());
    assert_eq!(chat_model_label(&chat), "claude-fable-5");
    chat.config = None;
    assert_eq!(chat_model_label(&chat), "claude-fable-5");
}

#[test]
fn unread_done_overrides_stale_dimming_but_not_archived() {
    // Fresh unread done, stale unread done: full opacity, ribbon shown.
    assert_eq!(card_dimming(false, false, true), (None, true));
    assert_eq!(card_dimming(false, true, true), (None, true));
    // Acknowledged: normal dimming returns.
    assert_eq!(card_dimming(false, true, false), (Some(0.55), false));
    assert_eq!(card_dimming(false, false, false), (None, false));
    // Archived always dims and never shows the ribbon.
    assert_eq!(card_dimming(true, true, true), (Some(0.4), false));
    assert_eq!(card_dimming(true, false, false), (Some(0.4), false));
}

#[gpui::test]
fn list_multi_pr_and_ticket_badges_stay_on_a_single_line(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        let mut chat = row("c1", "/r", None).chat;
        chat.last_message_at = Some(Utc::now());
        state.chats = vec![chat];
    });
    let status = OverviewLinkStatus {
        ticket: Some(TicketStatus {
            identifier: "ENG-1234".into(),
            title: Some("Compact badges".into()),
            url: None,
            status: Some("In Progress".into()),
        }),
        pr_links: (1..=5)
            .map(|number| OverviewPrLink {
                url: format!("https://github.com/a/b/pull/{number}"),
                source: Some(ChatLinkSource::Manual),
                detail: None,
            })
            .collect(),
        ..Default::default()
    };
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let (_overview, vcx) = cx.add_window_view(|_, cx| {
        let mut overview = Overview::new(
            state.clone(),
            gpui::WeakEntity::new_invalid(),
            transcript,
            composer,
            cx,
        );
        overview.view_mode = OverviewMode::List;
        overview.link_status.insert(
            "c1".into(),
            LinkStatusEntry {
                status,
                diff_stat: None,
                is_worktree: None,
                fetched_at: Instant::now(),
            },
        );
        overview
    });
    vcx.run_until_parked();

    let row = vcx.debug_bounds("overview-list-row").expect("row rendered");
    let badges = vcx
        .debug_bounds("overview-row-badges")
        .expect("badge row rendered");
    assert!(
        (f32::from(badges.size.height) - BADGE_ROW_H).abs() < 0.5,
        "badge row must remain a single line: {badges:?}"
    );
    assert!(
        badges.top() >= row.top() && badges.bottom() <= row.bottom() + px(0.5),
        "single-line badges must fit inside the row: {badges:?} in {row:?}"
    );
    for selector in [
        "overview-badge-pr-c1-0",
        "overview-badge-pr-c1-1",
        "overview-badge-pr-c1-2",
        "overview-badge-pr-c1-3",
        "overview-badge-pr-c1-4",
    ] {
        assert!(
            vcx.debug_bounds(selector).is_some(),
            "{selector} remains in the clipped row"
        );
    }
    assert!(
        vcx.debug_bounds("overview-badge-ticket-c1").is_some(),
        "ticket badge remains in the clipped row"
    );
}

/// End to end over the real UI path: a chat with a harness session gets a
/// `SCAN_CHAT_SUBAGENTS` request from `rows()`, the reply lands in
/// `subagents`, the row's `subagent_count` follows, and List mode lays the
/// subagent block out INSIDE the row's bounds (not clipped past its
/// bottom edge by the `overflow_hidden` row).
#[gpui::test]
fn list_row_renders_scanned_subagent_rows_inside_its_bounds(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    let (out, mut requests) = tokio::sync::mpsc::channel::<String>(64);
    let (replies, inbound) = tokio::sync::mpsc::channel::<String>(64);
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.set_test_engine(crate::state::EngineHandle::from_test_client(
            mast_rpc::RpcClient::new(out, inbound),
        ));
        let mut chat = row("c1", "/r", None).chat;
        chat.harness_session_id = Some("s1".into());
        chat.last_message_at = Some(Utc::now());
        state.chats = vec![chat];
    });
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let (overview, vcx) = cx.add_window_view(|_, cx| {
        let mut overview = Overview::new(
            state.clone(),
            gpui::WeakEntity::new_invalid(),
            transcript,
            composer,
            cx,
        );
        overview.view_mode = OverviewMode::List;
        overview
    });
    vcx.run_until_parked();

    // Answer the scan (the other per-row lookups just fail — irrelevant here).
    let mut scanned = false;
    while let Ok(frame) = requests.try_recv() {
        let request: serde_json::Value = serde_json::from_str(&frame).unwrap();
        let reply = if request["method"] == methods::SCAN_CHAT_SUBAGENTS {
            assert_eq!(request["params"]["chatId"], "c1");
            scanned = true;
            serde_json::json!({"id": request["id"], "ok": [
                {"agentId": "a", "description": "Research", "agentType": "fork", "transcriptPath": "/a"},
                {"agentId": "b", "transcriptPath": "/b"},
                {"agentId": "c", "transcriptPath": "/c"}
            ]})
        } else {
            serde_json::json!({"id": request["id"], "err": "unavailable"})
        };
        runtime.block_on(async {
            replies.send(reply.to_string()).await.unwrap();
            while replies.capacity() < replies.max_capacity() {
                tokio::task::yield_now().await;
            }
        });
    }
    assert!(
        scanned,
        "rows() must request SCAN_CHAT_SUBAGENTS for a chat with a harness session"
    );
    vcx.run_until_parked();

    overview.update(vcx, |overview, cx| {
        assert_eq!(overview.subagents.get("c1").map(Vec::len), Some(3));
        assert_eq!(overview.rows(cx)[0].subagent_count, 3);
    });
    let row = vcx.debug_bounds("overview-list-row").expect("row rendered");
    let block = vcx
        .debug_bounds("overview-list-subagents")
        .expect("subagent rows rendered in the row");
    assert!(
        block.top() >= row.top() && block.bottom() <= row.bottom(),
        "subagent block {block:?} must sit inside row {row:?}"
    );
    // Exactly three agents need no remainder row.
    assert!(f32::from(block.size.height) >= 3.0 * SUBAGENT_TILE_H + 2.0 * SUBAGENT_TILE_GAP - 0.5);
}

/// Collapsed/hovered subagent stack + the peek, end to end on the List:
/// a 36-subagent parent rests at three rows plus an indicator, expands to
/// every compact row on hover, and clicking one opens the read-only
/// transcript panel WITHOUT toggling the parent's expand, issues
/// `READ_SUBAGENT_TRANSCRIPT` for that `{chatId, agentId}`, shows a
/// loading state until the reply lands, then renders the turns; Escape
/// closes it back to the bare list.
#[gpui::test]
fn list_subagent_row_click_opens_readonly_peek(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    let (out, mut requests) = tokio::sync::mpsc::channel::<String>(256);
    let (replies, inbound) = tokio::sync::mpsc::channel::<String>(256);
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.set_test_engine(crate::state::EngineHandle::from_test_client(
            mast_rpc::RpcClient::new(out, inbound),
        ));
        let mut chat = row("c1", "/r", None).chat;
        chat.harness_session_id = Some("s1".into());
        chat.last_message_at = Some(Utc::now());
        state.chats = vec![chat];
    });
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let (overview, vcx) = cx.add_window_view(|_, cx| {
        let mut overview = Overview::new(
            state.clone(),
            gpui::WeakEntity::new_invalid(),
            transcript,
            composer,
            cx,
        );
        overview.view_mode = OverviewMode::List;
        overview
    });
    vcx.run_until_parked();

    let subagents: Vec<serde_json::Value> = (0..36)
        .map(|i| {
            serde_json::json!({
                "agentId": format!("a{i}"),
                "description": format!("Subagent task {i}"),
                "agentType": "Explore",
                "transcriptPath": format!("/a{i}"),
                "status": if i == 0 { "running" } else { "done" }
            })
        })
        .collect();
    // Answers every pending request; returns the transcript requests seen.
    let answer = |requests: &mut tokio::sync::mpsc::Receiver<String>| {
        let mut transcript_requests = Vec::new();
        while let Ok(frame) = requests.try_recv() {
            let request: serde_json::Value = serde_json::from_str(&frame).unwrap();
            let reply = if request["method"] == methods::SCAN_CHAT_SUBAGENTS {
                serde_json::json!({"id": request["id"], "ok": subagents})
            } else if request["method"] == methods::READ_SUBAGENT_TRANSCRIPT {
                transcript_requests.push(request["params"].clone());
                serde_json::json!({"id": request["id"], "ok": {
                    "turns": [
                        {"role": "user", "text": "Go look", "timestamp": null, "tools": []},
                        {"role": "assistant", "text": "Found it", "timestamp": null, "tools": [
                            {"name": "Grep", "inputPreview": "foo", "resultPreview": "bar.rs:1"}
                        ]}
                    ],
                    "model": "m"
                }})
            } else {
                serde_json::json!({"id": request["id"], "err": "unavailable"})
            };
            runtime.block_on(async {
                replies.send(reply.to_string()).await.unwrap();
                while replies.capacity() < replies.max_capacity() {
                    tokio::task::yield_now().await;
                }
            });
        }
        transcript_requests
    };
    answer(&mut requests);
    vcx.run_until_parked();

    overview.update(vcx, |overview, cx| {
        assert_eq!(overview.subagents.get("c1").map(Vec::len), Some(36));
        let row = &overview.rows(cx)[0];
        assert_eq!(row.subagent_count, 36);
    });
    vcx.run_until_parked();
    let row = vcx.debug_bounds("overview-list-row").expect("row rendered");
    assert!(
        vcx.debug_bounds("overview-card-model").is_some(),
        "row renders model metadata"
    );
    let block = vcx
        .debug_bounds("overview-list-subagents")
        .expect("subagent stack rendered");
    assert!(
        vcx.debug_bounds("overview-subagent-more").is_some(),
        "resting row shows remainder row"
    );
    assert!(
        block.top() >= row.top() && block.bottom() <= row.bottom() + px(0.5),
        "preview stack {block:?} must fit inside the resting row {row:?}"
    );
    // The block carries the row's 8px bottom padding.
    let stack = |rows: f32| rows * SUBAGENT_TILE_H + (rows - 1.0) * SUBAGENT_TILE_GAP + 8.0;
    assert!(
        (f32::from(block.size.height) - stack((SUBAGENT_PREVIEW_LIMIT + 1) as f32)).abs() < 1.0,
        "preview rows laid out at their fixed height: {block:?}"
    );

    // Hovering anywhere on the row replaces the indicator with the full
    // list.
    vcx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
    vcx.run_until_parked();
    overview.update(vcx, |overview, _| {
        assert_eq!(overview.hovered_row.as_deref(), Some("c1"));
    });
    assert!(vcx.debug_bounds("overview-subagent-more").is_none());
    let block = vcx
        .debug_bounds("overview-list-subagents")
        .expect("expanded subagent stack rendered");
    assert!(
        (f32::from(block.size.height) - stack(36.0)).abs() < 1.0,
        "every hovered row laid out at its fixed height: {block:?}"
    );

    // Click the first compact row.
    let first = gpui::point(
        block.left() + px(30.0),
        block.top() + px(SUBAGENT_TILE_H / 2.0),
    );
    vcx.simulate_click(first, gpui::Modifiers::none());
    vcx.run_until_parked();
    overview.update(vcx, |overview, _| {
        let peek = overview.subagent_peek.as_ref().expect("peek opened");
        assert_eq!(
            (peek.chat_id.as_str(), peek.agent_id.as_str()),
            ("c1", "a0")
        );
        assert_eq!(peek.load, PeekLoad::Loading);
        assert!(!overview.chat_panel_open);
        assert!(
            !overview.expanded.contains("c1"),
            "subagent click must not toggle the parent"
        );
    });
    assert!(
        vcx.debug_bounds("overview-subagent-peek").is_some(),
        "peek panel rendered"
    );

    let transcript_requests = answer(&mut requests);
    assert_eq!(
        transcript_requests,
        vec![serde_json::json!({"chatId": "c1", "agentId": "a0"})]
    );
    vcx.run_until_parked();
    overview.update(vcx, |overview, _| {
        match &overview.subagent_peek.as_ref().expect("still open").load {
            PeekLoad::Loaded(t) => assert_eq!(t.turns.len(), 2),
            other => panic!("expected loaded, got {other:?}"),
        }
    });

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    overview.update(vcx, |overview, _| {
        assert!(overview.subagent_peek.is_none(), "Escape closes the peek");
    });
}

#[test]
fn list_row_identity_survives_source_reordering() {
    let row = ListPresentationItem::Row {
        chat_id: "chat".into(),
        row_index: 1,
    };
    let moved = ListPresentationItem::Row {
        chat_id: "chat".into(),
        row_index: 19,
    };
    let other = ListPresentationItem::Row {
        chat_id: "other".into(),
        row_index: 1,
    };
    assert!(row.same_identity(&moved));
    assert!(!row.same_identity(&other));
}

#[gpui::test]
fn grouped_list_keeps_selection_expansion_and_scroll_targets(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.chats = (0..60)
            .map(|i| {
                let mut chat = row(
                    &format!("c{i:02}"),
                    if i % 2 == 0 {
                        "/work/alpha"
                    } else {
                        "/work/beta"
                    },
                    None,
                )
                .chat;
                chat.last_message_at = Some(Utc::now());
                chat
            })
            .collect();
    });
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let (overview, vcx) = cx.add_window_view(|_, cx| {
        let mut overview = Overview::new(
            state.clone(),
            gpui::WeakEntity::new_invalid(),
            transcript,
            composer,
            cx,
        );
        overview.active_groups = vec![GroupDimension::Repo];
        overview.view_mode = OverviewMode::List;
        overview
    });
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    };
    vcx.run_until_parked();
    draw(vcx);
    let (first, last, count) = overview.read_with(vcx, |o, _| {
        assert_eq!(o.list_order.len(), 60);
        assert_eq!(
            o.list_order[0].1, 1,
            "the first group header precedes its rows"
        );
        assert!(
            o.list_order
                .windows(2)
                .any(|pair| pair[1].1 > pair[0].1 + 1),
            "another group header separates rows"
        );
        (
            o.list_order[0].0.clone(),
            o.list_order[59].0.clone(),
            o.list_order.len(),
        )
    });
    let last_selector = Box::leak(format!("overview-list-chat-{last}").into_boxed_str());
    assert!(
        vcx.debug_bounds(last_selector).is_none(),
        "offscreen rows are not rendered"
    );
    assert!(vcx.debug_bounds("overview-expanded-detail").is_none());
    overview.update(vcx, |o, cx| {
        o.toggle_expanded(&first, cx);
        cx.notify();
    });
    draw(vcx);
    assert!(overview.read_with(vcx, |o, _| o.expanded.contains(&first)));
    assert!(vcx.debug_bounds("overview-expanded-detail").is_some());
    overview.update(vcx, |o, cx| o.select_list_row(count - 1, cx));
    draw(vcx);
    assert_eq!(
        overview.read_with(vcx, |o, _| o.list_selected.clone()),
        Some(last.clone())
    );
    assert!(
        overview.read_with(vcx, |o, _| o.list.logical_scroll_top().item_ix > 0),
        "selection scrolls into view"
    );
    let selected = vcx
        .debug_bounds(last_selector)
        .expect("selected offscreen row is rendered");
    let viewport = vcx.debug_bounds("overview-list-viewport").unwrap();
    assert!(
        selected.origin.y >= viewport.origin.y
            && selected.origin.y < viewport.origin.y + viewport.size.height,
        "selected row is inside the viewport"
    );
    let moved_chat = overview.read_with(vcx, |o, _| o.list_order[10].0.clone());
    let old_position = overview.read_with(vcx, |o, _| {
        o.list_order
            .iter()
            .position(|(id, _)| id == &moved_chat)
            .unwrap()
    });
    let anchor_chat = overview.read_with(vcx, |o, _| {
        let index = o.list.logical_scroll_top().item_ix;
        match &o.list_items[index] {
            ListPresentationItem::Row { chat_id, .. } => chat_id.clone(),
            _ => panic!("expected a chat at the scroll anchor"),
        }
    });
    state.update(vcx, |state, cx| {
        let chat = state
            .chats
            .iter_mut()
            .find(|chat| chat.id == moved_chat)
            .unwrap();
        chat.last_message_at = Some(Utc::now() + chrono::Duration::days(1));
        cx.notify();
    });
    overview.update(vcx, |o, cx| {
        o.rows_dirty = true;
        cx.notify();
    });
    draw(vcx);
    overview.read_with(vcx, |o, _| {
        assert_ne!(o.list_order.iter().position(|(id, _)| id == &moved_chat).unwrap(), old_position, "fixture must reorder a chat");
        let index = o.list.logical_scroll_top().item_ix;
        assert!(matches!(&o.list_items[index], ListPresentationItem::Row { chat_id, .. } if chat_id == &anchor_chat), "reordering keeps the visible chat anchored");
    });
    overview.update(vcx, |o, cx| {
        o.focus_due = Some(first.clone());
        cx.notify();
    });
    draw(vcx);
    assert!(
        overview.read_with(vcx, |o, _| o.list.logical_scroll_top().item_ix <= 2),
        "focus target scrolls back to the first group"
    );
    assert!(
        overview.read_with(vcx, |o, _| o.expanded.contains(&first)),
        "scrolling preserves expansion"
    );
    assert!(
        vcx.debug_bounds("overview-expanded-detail").is_some(),
        "expanded content returns after scrolling back"
    );
    let (boundary, before, after) = overview.read_with(vcx, |o, _| {
        let boundary = o
            .list_order
            .windows(2)
            .position(|pair| pair[1].1 > pair[0].1 + 1)
            .unwrap();
        (
            boundary,
            o.list_order[boundary].0.clone(),
            o.list_order[boundary + 1].0.clone(),
        )
    });
    overview.update(vcx, |o, cx| o.select_list_row(boundary, cx));
    vcx.update(|window, cx| {
        let focus = overview.read(cx).body_focus.clone();
        window.focus(&focus, cx);
    });
    draw(vcx);
    vcx.simulate_keystrokes("down");
    draw(vcx);
    assert_eq!(
        overview.read_with(vcx, |o, _| o.list_selected.clone()),
        Some(after)
    );
    vcx.simulate_keystrokes("up");
    draw(vcx);
    assert_eq!(
        overview.read_with(vcx, |o, _| o.list_selected.clone()),
        Some(before)
    );
}

/// List mode with more rows than fit on screen: each row keeps its full
/// natural height (subagent rows included) and the list scrolls. The
/// row root is `overflow_hidden` for the unread ribbon, which zeroes its
/// flex auto-min-height, so without `flex_none` the scrolling column
/// squeezed every row (live repro: a 64px row down to 11.5px) and clipped
/// the subagent block away.
#[gpui::test]
fn list_rows_keep_subagent_rows_when_the_list_overflows(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let state = cx.new(|_| AppState::new());
    let ids: Vec<String> = (0..60).map(|i| format!("c{i:02}")).collect();
    state.update(cx, |state, _| {
        state.chats = ids
            .iter()
            .map(|id| {
                let mut chat = row(id, "/r", None).chat;
                chat.last_message_at = Some(Utc::now());
                chat
            })
            .collect();
    });
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let (_overview, vcx) = cx.add_window_view(|_, cx| {
        let mut overview = Overview::new(
            state.clone(),
            gpui::WeakEntity::new_invalid(),
            transcript,
            composer,
            cx,
        );
        overview.view_mode = OverviewMode::List;
        for id in &ids {
            overview.subagents.insert(
                id.clone(),
                serde_json::from_value(serde_json::json!([
                    {"agentId": "a", "description": "Research", "transcriptPath": "/a"},
                    {"agentId": "b", "transcriptPath": "/b"},
                    {"agentId": "c", "transcriptPath": "/c"}
                ]))
                .unwrap(),
            );
            overview
                .subagents_fetched_at
                .insert(id.clone(), Instant::now());
        }
        overview
    });
    vcx.run_until_parked();
    let row = vcx
        .debug_bounds("overview-list-row")
        .expect("list row rendered");
    let block = vcx
        .debug_bounds("overview-list-subagents")
        .expect("subagent rows rendered");
    assert!(
        f32::from(block.size.height) >= 3.0 * SUBAGENT_TILE_H,
        "all three subagent rows laid out: {block:?}"
    );
    assert!(
        f32::from(row.size.height) > f32::from(block.size.height),
        "row {row:?} must not be squeezed below its subagent block {block:?}"
    );
}

// ----------------------------------------------------------------------
// Work Outline presentation (Milestone 1)
// ----------------------------------------------------------------------

use crate::work_outline::model::row_key;
use crate::work_outline::store::fixture_ids::*;

/// An Overview over one Space `s1` whose chats match the store fixture's
/// live chat ids, with the fixture installed as its outline.
fn outline_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
) -> (
    Entity<Overview>,
    Entity<AppState>,
    &'a mut gpui::VisualTestContext,
) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let state = cx.new(|_| AppState::new());
    let dir = dir.to_path_buf();
    state.update(cx, |state, _| {
        state.data_dir = Some(dir);
        state.spaces = vec![
            serde_json::from_value(serde_json::json!({
                "id": "s1", "deviceId": "d", "path": "/work/alpha",
                "createdAt": "2026-01-01T00:00:00Z"
            }))
            .unwrap(),
        ];
        state.chats = [
            CHAT_API,
            CHAT_AUTH,
            CHAT_UI,
            CHAT_LOOSE_A,
            CHAT_LOOSE_B,
            CHAT_ORPHAN,
        ]
        .iter()
        .map(|id| {
            let mut chat = row(id, "/work/alpha", None).chat;
            chat.space_id = Some("s1".into());
            chat.title = Some(format!("Chat {id}"));
            chat.last_message_at = Some(Utc::now());
            chat
        })
        .collect();
    });
    let (overview, vcx) = overview_over(cx, &state);
    overview.update(vcx, |o, cx| o.set_outline_loads(fixture_loads(None), cx));
    (overview, state, vcx)
}

fn fixture_loads(snapshot: Option<WorkOutlineSnapshot>) -> HashMap<String, WorkOutlineLoad> {
    let snapshot = snapshot.unwrap_or_else(|| crate::work_outline::store::fixture_snapshot("s1"));
    HashMap::from([(
        "s1".to_string(),
        WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)),
    )])
}

/// `debug_bounds` takes a `'static` selector; leak the (test-only) string.
fn row_bounds(vcx: &mut gpui::VisualTestContext, selector: String) -> Option<Bounds<Pixels>> {
    vcx.debug_bounds(Box::leak(selector.into_boxed_str()))
}

fn draw(vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| window.draw(cx).clear());
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
}

#[gpui::test]
fn outline_mode_survives_a_fresh_overview(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (first, state, vcx) = outline_overview(cx, dir.path());
    first.update(vcx, |o, cx| o.view_prefs_changed(cx));
    let flags = overview_positions::load_string_set(dir.path(), overview_positions::UI_FLAGS_FILE);
    assert!(flags.contains(VIEW_OUTLINE_FLAG));
    assert!(!flags.contains(LEGACY_VIEW_CANVAS_FLAG));
    let (second, vcx) = overview_over(cx, &state);
    second.update(vcx, |o, _| assert_eq!(o.mode(), OverviewMode::Outline));
}

/// Regression: the command can reach an Overview that has never rendered
/// (so its saved prefs are not loaded yet). Switching to Outline persists
/// the in-memory prefs, which must not overwrite the saved file with
/// defaults.
#[gpui::test]
fn open_outline_command_on_an_unrendered_overview_keeps_saved_prefs(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with_repos(cx, dir.path(), &["alpha"]);
    let mut flags = BTreeSet::new();
    apply_view_prefs_to_flags(
        &mut flags,
        &ViewPrefs {
            mode: OverviewMode::List,
            hide_stale: true,
            show_archived: true,
            groups: vec![GroupDimension::Repo],
        },
    );
    overview_positions::save_string_set(dir.path(), overview_positions::UI_FLAGS_FILE, &flags)
        .unwrap();

    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let overview = {
        let state = state.clone();
        cx.new(move |cx| {
            Overview::new(
                state,
                gpui::WeakEntity::new_invalid(),
                transcript,
                composer,
                cx,
            )
        })
    };
    overview.read_with(cx, |o, _| assert!(!o.prefs_loaded, "no render yet"));
    let (_, vcx) = cx.add_window_view(|_, _| gpui::Empty);
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::OpenOutline, window, cx)
        })
    });
    overview.read_with(vcx, |o, _| {
        assert_eq!(o.mode(), OverviewMode::Outline);
        assert!(o.hide_stale);
        assert!(o.show_archived);
        assert_eq!(o.active_groups, vec![GroupDimension::Repo]);
    });
    let saved = overview_positions::load_string_set(dir.path(), overview_positions::UI_FLAGS_FILE);
    assert!(saved.contains(VIEW_OUTLINE_FLAG));
    assert!(saved.contains(HIDE_STALE_FLAG));
    assert!(saved.contains(SHOW_ARCHIVED_FLAG));
    assert!(saved.iter().any(|f| f.starts_with(GROUP_BY_PREFIX)));
}

#[gpui::test]
fn outline_renders_compact_rows_with_stable_selectors(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _state, vcx) = outline_overview(cx, dir.path());
    overview.update(vcx, |o, cx| o.set_mode(OverviewMode::Outline, cx));
    draw(vcx);
    for key in [
        row_key::item("s1", LAUNCH),
        // w-api-auth's only Chat is folded into its row.
        row_key::item("s1", API_AUTH),
        row_key::chat(CHAT_GHOST),
        row_key::unassociated(Some("s1")),
        row_key::chat(CHAT_LOOSE_A),
    ] {
        assert!(
            row_bounds(vcx, format!("outline-row-{key}")).is_some(),
            "{key} renders"
        );
    }
    assert!(row_bounds(vcx, format!("outline-row-{}", row_key::chat(CHAT_AUTH))).is_none());
    // Archived items stay out by default.
    assert!(row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", RETIRED))).is_none());
    let row = row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", API))).unwrap();
    assert!(
        f32::from(row.size.height) <= 28.0,
        "one compact line: {row:?}"
    );
}

/// The "Updated since you last viewed" dot shows in the Details list and on
/// the Outline row after a new revision, and clears once the last-viewed
/// record catches up (what opening the artifact does).
#[gpui::test]
fn changed_marker_shows_in_details_and_outline_and_clears(cx: &mut gpui::TestAppContext) {
    use crate::artifacts::tests::{artifact, id, snapshot as artifacts_snapshot};
    use mast_proto::ArtifactKind;
    let dir = tempfile::tempdir().unwrap();
    let (overview, state, vcx) = outline_overview(cx, dir.path());
    vcx.update(|_, cx| {
        crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx)
    });
    let viewed = |vcx: &mut gpui::VisualTestContext, entries: &[(u8, u32)]| {
        let entries: Vec<(String, u32)> = entries.iter().map(|(n, r)| (id(*n), *r)).collect();
        vcx.update(|_, cx| {
            crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
                settings.artifact_last_viewed = entries.into_iter().collect();
            });
            cx.refresh_windows();
        });
    };
    let mut plan = artifact(&id(1), "Plan", ArtifactKind::Markdown);
    plan.pinned_as_plan = true;
    let flow = artifact(&id(2), "Flow", ArtifactKind::HtmlFragment);
    let notes = artifact(&id(3), "Notes", ArtifactKind::Markdown);
    state.update(vcx, |state, _| {
        state.local_device_id = Some("d".into());
        state.set_test_artifacts(artifacts_snapshot(vec![
            plan.clone(),
            flow.clone(),
            notes.clone(),
        ]));
    });
    // The plan and the notes were seen at r2 (current); the flow at r1.
    viewed(vcx, &[(1, 2), (2, 1), (3, 2)]);
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(
            fixture_loads(Some(crate::work_outline::store::fixture_snapshot("s1"))),
            cx,
        );
        o.set_mode(OverviewMode::Outline, cx);
    });
    let _details = open_details(&overview, vcx, API);
    let outline = format!("outline-artifacts-changed-{}", row_key::item("s1", API));
    let marked = |vcx: &mut gpui::VisualTestContext, n: u8| {
        row_bounds(vcx, format!("work-item-artifact-changed-{}", id(n))).is_some()
    };
    assert!(!marked(vcx, 1) && marked(vcx, 2) && !marked(vcx, 3));
    assert!(row_bounds(vcx, outline.clone()).is_some());

    // Opening the flow records r2: every list clears.
    viewed(vcx, &[(1, 2), (2, 2), (3, 2)]);
    draw(vcx);
    assert!(!marked(vcx, 2));
    assert!(row_bounds(vcx, outline.clone()).is_none());

    // A new revision of the plan brings the marker back.
    let mut plan = plan;
    plan.current_revision = 3;
    state.update(vcx, |state, _| {
        state.set_test_artifacts(artifacts_snapshot(vec![plan, flow, notes]));
    });
    draw(vcx);
    assert!(marked(vcx, 1) && !marked(vcx, 2));
    assert!(row_bounds(vcx, outline).is_some());
}

/// Planning artifacts: Details lists the pinned plan first (with the
/// item's Linear project beside it), then newest first; the row menu pins;
/// "Register existing" sends `ArtifactRegister` for the item; the Outline
/// row shows the artifact count.
#[gpui::test]
fn work_item_details_list_artifacts_and_the_outline_counts_them(cx: &mut gpui::TestAppContext) {
    use crate::artifacts::tests::{artifact, id, scripted, snapshot as artifacts_snapshot};
    use mast_proto::ArtifactKind;
    let dir = tempfile::tempdir().unwrap();
    let (overview, state, vcx) = outline_overview(cx, dir.path());
    let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
    snapshot
        .external_references
        .push(mast_proto::WorkItemExternalReferenceView {
            id: "ref-project".into(),
            space_id: "s1".into(),
            work_item_id: API.into(),
            provider: mast_proto::linear::LINEAR_PROVIDER.into(),
            reference_type: mast_proto::linear::LINEAR_PROJECT_REFERENCE.into(),
            external_id: "p1".into(),
            url: "https://linear.app/acme/project/receiving-1".into(),
            cached_title: Some("Receiving".into()),
            created_by: None,
            created_at: 1,
        });
    let mut plan = artifact(&id(1), "Plan", ArtifactKind::Markdown);
    plan.pinned_as_plan = true;
    plan.updated_at = 1;
    let mut newer = artifact(&id(2), "Flow", ArtifactKind::HtmlFragment);
    newer.updated_at = 30;
    let mut older = artifact(&id(3), "Notes", ArtifactKind::Markdown);
    older.updated_at = 10;
    let (actions, calls) = scripted(std::sync::Arc::new(|method, _| {
        Ok(match method {
            mast_rpc::methods::ARTIFACT_REGISTER => serde_json::json!({
                "artifactId": id(4), "revision": 1, "url": "mast-artifact:x",
                "storedPath": "/work/alpha/.zeron/artifacts/api/plan.md", "changed": true
            }),
            _ => serde_json::json!({ "artifactId": id(1), "pinned": true, "unpinned": [] }),
        })
    }));
    state.update(vcx, |state, _| {
        state.local_device_id = Some("d".into());
        state.work_outline_actions_override = Some(actions);
        state.set_test_artifacts(artifacts_snapshot(vec![older, newer, plan]));
    });
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(snapshot)), cx);
        o.set_mode(OverviewMode::Outline, cx);
    });
    draw(vcx);
    assert!(
        row_bounds(
            vcx,
            format!("outline-artifacts-{}", row_key::item("s1", API))
        )
        .is_some(),
        "the Outline row counts the item's artifacts"
    );
    assert!(
        row_bounds(
            vcx,
            format!("outline-artifacts-{}", row_key::item("s1", UI))
        )
        .is_none()
    );

    let details = open_details(&overview, vcx, API);
    let y = |vcx: &mut gpui::VisualTestContext, n: u8| {
        row_bounds(vcx, format!("work-item-artifact-{}", id(n)))
            .unwrap_or_else(|| panic!("artifact {n} row"))
            .origin
            .y
    };
    let (plan_y, newer_y, older_y) = (y(vcx, 1), y(vcx, 2), y(vcx, 3));
    assert!(
        plan_y < newer_y && newer_y < older_y,
        "plan, then newest first"
    );
    let chip = vcx
        .debug_bounds("work-item-artifact-plan-linear")
        .expect("Linear project next to the plan");
    assert!((f32::from(chip.center().y) - f32::from(plan_y)).abs() < 24.0);

    // Right-click a row: its menu pins it.
    let row = row_bounds(vcx, format!("work-item-artifact-{}", id(2))).unwrap();
    vcx.simulate_mouse_down(
        row.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    draw(vcx);
    let pin = vcx
        .debug_bounds("work-item-artifact-menu-pin")
        .expect("menu");
    vcx.simulate_click(pin.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let sent = |method: &str| {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, params)| params.clone())
            .collect::<Vec<_>>()
    };
    let pins = sent(mast_rpc::methods::ARTIFACT_PIN);
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0]["artifactId"], id(2));
    assert_eq!(pins[0]["pinned"], true);

    // Register existing: outside the Space is refused; inside registers.
    details.update(vcx, |d, cx| d.register_existing("/etc/passwd".into(), cx));
    vcx.run_until_parked();
    assert!(sent(mast_rpc::methods::ARTIFACT_REGISTER).is_empty());
    details.update(vcx, |d, cx| {
        d.register_existing("/work/alpha/work/plan.md".into(), cx)
    });
    vcx.run_until_parked();
    let registers = sent(mast_rpc::methods::ARTIFACT_REGISTER);
    assert_eq!(registers.len(), 1);
    assert_eq!(registers[0]["spaceId"], "s1");
    assert_eq!(registers[0]["workItemId"], API);
    assert_eq!(registers[0]["path"], "/work/alpha/work/plan.md");
    assert!(registers[0].get("targetDeviceId").is_none());
    details.read_with(vcx, |d, _| {
        assert_eq!(d.artifacts.status.as_deref(), Some("Registered as r1"));
    });
    // The register button opens the path field in tests (no native picker).
    draw(vcx);
    let register = vcx.debug_bounds("work-item-artifacts-register").unwrap();
    vcx.simulate_click(register.center(), gpui::Modifiers::default());
    draw(vcx);
    assert!(
        vcx.debug_bounds("work-item-artifacts-register-input")
            .is_some()
    );
}

/// Acceptance 30: only the visible slice of a large outline renders.
#[gpui::test]
fn large_outlines_render_only_the_visible_slice(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _state, vcx) = outline_overview(cx, dir.path());
    let (snapshot, _) = crate::work_outline::model::tests::large_snapshot("s1", 10, 100);
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(snapshot)), cx);
        o.set_mode(OverviewMode::Outline, cx);
    });
    draw(vcx);
    let total = overview.read_with(vcx, |o, cx| o.outline.read(cx).model().len());
    assert!(total > 2_000, "{total}");
    assert!(row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", "r0"))).is_some());
    assert!(row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", "r9c99"))).is_none());
}

#[gpui::test]
fn outline_keyboard_navigation_follows_visual_order_and_sibling_jumps(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _state, vcx) = outline_overview(cx, dir.path());
    vcx.update(|_, cx| cx.bind_keys(crate::work_outline::outline_view::key_bindings()));
    overview.update(vcx, |o, cx| o.set_mode(OverviewMode::Outline, cx));
    draw(vcx);
    vcx.update(|window, cx| {
        let handle = gpui::Focusable::focus_handle(overview.read(cx).outline.read(cx), cx);
        window.focus(&handle, cx);
    });
    let selected = |vcx: &mut gpui::VisualTestContext| {
        overview.read_with(vcx, |o, cx| {
            o.outline.read(cx).selected_key().map(str::to_string)
        })
    };
    vcx.simulate_keystrokes("down");
    assert_eq!(selected(vcx), Some(row_key::item("s1", LAUNCH)));
    // Down enters the expanded parent; Up returns to it.
    vcx.simulate_keystrokes("down");
    assert_eq!(selected(vcx), Some(row_key::item("s1", API)));
    vcx.simulate_keystrokes("up");
    assert_eq!(selected(vcx), Some(row_key::item("s1", LAUNCH)));
    vcx.simulate_keystrokes("down");
    // Alt+Down jumps to the next sibling, Alt+Up back.
    vcx.simulate_keystrokes("alt-down");
    assert_eq!(selected(vcx), Some(row_key::item("s1", UI)));
    vcx.simulate_keystrokes("alt-up");
    assert_eq!(selected(vcx), Some(row_key::item("s1", API)));
    vcx.simulate_keystrokes("alt-up");
    assert_eq!(selected(vcx), Some(row_key::item("s1", LAUNCH)));
    // Left collapses, a second Left on the root stays put.
    vcx.simulate_keystrokes("left");
    let collapsed = overview.read_with(vcx, |o, cx| {
        o.outline
            .read(cx)
            .model()
            .row(&row_key::item("s1", LAUNCH))
            .unwrap()
            .collapsed
    });
    assert!(collapsed);
    // Focus survives a snapshot refresh.
    overview.update(vcx, |o, cx| o.set_outline_loads(fixture_loads(None), cx));
    draw(vcx);
    assert_eq!(selected(vcx), Some(row_key::item("s1", LAUNCH)));
    vcx.simulate_keystrokes("right");
    vcx.simulate_keystrokes("right");
    assert_eq!(selected(vcx), Some(row_key::item("s1", API)));
}

#[gpui::test]
fn commands_without_a_selection_open_the_outline_with_a_notice(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _state, vcx) = outline_overview(cx, dir.path());
    assert_eq!(
        overview.read_with(vcx, |o, _| o.mode()),
        OverviewMode::Outline
    );
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::RenameWorkItem, window, cx)
        })
    });
    overview.read_with(vcx, |o, cx| {
        assert_eq!(o.mode(), OverviewMode::Outline);
        assert_eq!(
            o.outline.read(cx).notice(),
            Some("Select a work item first")
        );
    });
    // Create needs no selection: an inline editor row appears.
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::CreateWorkItem, window, cx)
        })
    });
    draw(vcx);
    assert!(vcx.debug_bounds("outline-row-editor").is_some());
    // Open Required Actions needs no selection: the global surface opens.
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::OpenRequiredActions, window, cx)
        })
    });
    overview.read_with(vcx, |o, _| assert!(o.required_actions_panel.is_some()));
}

/// Acceptance 26: details render in the right slot and closing them
/// restores the chat panel exactly as it was.
#[gpui::test]
fn work_item_details_open_and_close_restore_the_previous_layout(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, state, vcx) = outline_overview(cx, dir.path());
    state.update(vcx, |state, _| state.selected_chat = Some(CHAT_API.into()));
    overview.update(vcx, |o, cx| {
        o.chat_panel_open = true;
        o.set_mode(OverviewMode::Outline, cx);
    });
    draw(vcx);
    assert!(vcx.debug_bounds("work-item-details").is_none());
    overview.update(vcx, |o, cx| {
        o.open_work_item_details("s1".into(), API.into(), cx)
    });
    draw(vcx);
    assert!(vcx.debug_bounds("work-item-details").is_some());
    assert!(vcx.debug_bounds("work-item-breadcrumbs").is_some());
    overview.read_with(vcx, |o, cx| {
        assert!(
            o.chat_panel_open,
            "details never touch the chat panel state"
        );
        assert!(o.subagent_peek.is_none());
        assert_eq!(state.read(cx).selected_chat.as_deref(), Some(CHAT_API));
    });
    // Escape inside the details (no draft) closes them.
    overview.update(vcx, |o, cx| {
        let details = o.work_item_details.clone().unwrap();
        details.update(cx, |d, cx| d.request_close(cx));
    });
    draw(vcx);
    overview.read_with(vcx, |o, _| {
        assert!(o.work_item_details.is_none());
        assert!(o.chat_panel_open);
    });
    assert!(vcx.debug_bounds("work-item-details").is_none());
}

/// Switching items or opening a Chat with an unsaved draft asks first;
/// Keep Editing keeps the draft and the item, Discard then switches.
#[gpui::test]
fn leaving_details_with_a_draft_asks_before_discarding(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _state, vcx) = outline_overview(cx, dir.path());
    overview.update(vcx, |o, cx| {
        o.set_mode(OverviewMode::Outline, cx);
        o.open_work_item_details("s1".into(), API.into(), cx);
    });
    draw(vcx);
    let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
    vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_title_draft("API v2", window, cx)));
    let shown = |vcx: &mut gpui::VisualTestContext| {
        overview.read_with(vcx, |o, cx| {
            o.work_item_details
                .as_ref()
                .map(|d| d.read(cx).item_id.clone())
        })
    };

    // Switch item: the confirmation is requested, nothing switches yet.
    overview.update(vcx, |o, cx| {
        o.open_work_item_details("s1".into(), UI.into(), cx)
    });
    assert_eq!(shown(vcx).as_deref(), Some(API));
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .expect("asks first");
    assert_eq!(request.confirm_label.as_ref(), "Discard");
    assert_eq!(request.cancel_label.as_ref(), "Keep Editing");
    // Keep Editing: same item, draft intact.
    vcx.update(|window, cx| (request.on_decision)(false, window, cx));
    assert_eq!(shown(vcx).as_deref(), Some(API));
    assert!(details.read_with(vcx, |d, cx| d.has_unsaved_draft(cx)));

    // Opening a Chat asks too; Keep Editing keeps everything.
    overview.update(vcx, |o, cx| {
        o.leave_work_item_details(DetailsExit::OpenChat(CHAT_API.into()), cx)
    });
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .expect("asks first");
    vcx.update(|window, cx| (request.on_decision)(false, window, cx));
    assert_eq!(shown(vcx).as_deref(), Some(API));
    assert!(!overview.read_with(vcx, |o, _| o.chat_panel_open));

    // Discard: the switch happens.
    overview.update(vcx, |o, cx| {
        o.open_work_item_details("s1".into(), UI.into(), cx)
    });
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .expect("asks first");
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    assert_eq!(shown(vcx).as_deref(), Some(UI));

    // Without a draft, switching and closing need no confirmation.
    overview.update(vcx, |o, cx| {
        o.open_work_item_details("s1".into(), API.into(), cx)
    });
    assert_eq!(shown(vcx).as_deref(), Some(API));
    let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
    details.update(vcx, |d, cx| d.request_close(cx));
    vcx.run_until_parked();
    assert_eq!(shown(vcx), None);
}

#[gpui::test]
fn list_rows_show_the_work_item_path_and_attention(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _state, vcx) = outline_overview(cx, dir.path());
    overview.update(vcx, |o, cx| o.set_mode_for_test(OverviewMode::List, cx));
    draw(vcx);
    let rows = overview.update(vcx, |o, cx| o.rows(cx));
    let auth = rows.iter().find(|r| r.chat.id == CHAT_AUTH).unwrap();
    assert_eq!(
        auth.work_item_path.as_ref().map(|p| p.full.as_str()),
        Some("Launch v2 › API › Auth endpoints")
    );
    let ui = rows.iter().find(|r| r.chat.id == CHAT_UI).unwrap();
    assert!(ui.user_action_required);
    let loose = rows.iter().find(|r| r.chat.id == CHAT_LOOSE_A).unwrap();
    assert!(loose.work_item_path.is_none());
    assert!(vcx.debug_bounds("work-item-path-chip").is_some());
    assert!(vcx.debug_bounds("work-item-attention").is_some());
}

#[gpui::test]
fn work_item_filter_keeps_subtrees_and_resets_a_stale_restore(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _state, vcx) = outline_overview(cx, dir.path());
    let ids = |rows: std::rc::Rc<Vec<OverviewRow>>| {
        let mut ids: Vec<String> = rows.iter().map(|r| r.chat.id.clone()).collect();
        ids.sort();
        ids
    };
    overview.update(vcx, |o, cx| {
        o.toggle_work_item_filter(LAUNCH, cx);
        assert_eq!(ids(o.rows(cx)), [CHAT_API, CHAT_AUTH, CHAT_UI]);
        o.toggle_work_item_filter(LAUNCH, cx);
        o.toggle_work_item_filter(work_item_filter::UNASSOCIATED_KEY, cx);
        assert_eq!(ids(o.rows(cx)), [CHAT_LOOSE_A, CHAT_LOOSE_B, CHAT_ORPHAN]);
    });
    assert_eq!(
        work_item_filter::load_filter(dir.path()),
        BTreeSet::from([work_item_filter::UNASSOCIATED_KEY.to_string()])
    );

    // A restored filter naming only a deleted item hides everything:
    // it resets once the outline has loaded.
    let dir = tempfile::tempdir().unwrap();
    work_item_filter::save_filter(dir.path(), &BTreeSet::from(["w-gone".to_string()])).unwrap();
    let (overview, _state, vcx) = outline_overview(cx, dir.path());
    overview.update(vcx, |o, cx| {
        assert_eq!(o.rows(cx).len(), 6, "nothing is filtered out invisibly");
        assert!(o.work_item_filter.is_empty());
    });
    assert!(work_item_filter::load_filter(dir.path()).is_empty());
}

/// Execution exceptions: only the exception Chat carries the compact
/// mark on List rows.
#[gpui::test]
fn execution_exception_marks_flag_only_the_exception_chat_on_list_rows(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _state, vcx) = outline_overview(cx, dir.path());
    overview.update(vcx, |o, cx| o.set_mode_for_test(OverviewMode::List, cx));
    let marks = |vcx: &mut gpui::VisualTestContext, prefix: &str| {
        [CHAT_API, CHAT_AUTH, CHAT_UI, CHAT_LOOSE_A]
            .into_iter()
            .filter(|id| row_bounds(vcx, format!("{prefix}{id}")).is_some())
            .collect::<Vec<_>>()
    };
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(None), cx);
    });
    draw(vcx);
    assert!(marks(vcx, "overview-row-execution-exception-").is_empty());
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(execution_fixture::snapshot("s1"))), cx);
    });
    draw(vcx);
    assert_eq!(marks(vcx, "overview-row-execution-exception-"), [CHAT_API]);
}

/// Drift marks: only drifted Chats carry one, on List rows and the
/// chat-panel header.
#[gpui::test]
fn drift_marks_flag_only_drifted_chats(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, state, vcx) = outline_overview(cx, dir.path());
    overview.update(vcx, |o, cx| o.set_mode_for_test(OverviewMode::List, cx));
    let drifted = instruction_fixture::snapshot("s1");
    let mut current = drifted.clone();
    for chat_state in &mut current.chat_instruction_states {
        chat_state.drift = false;
    }
    let marks = |vcx: &mut gpui::VisualTestContext, prefix: &str| {
        [CHAT_API, CHAT_AUTH, CHAT_UI, CHAT_LOOSE_A]
            .into_iter()
            .filter(|id| row_bounds(vcx, format!("{prefix}{id}")).is_some())
            .collect::<Vec<_>>()
    };
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(current)), cx);
    });
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(drifted)), cx);
        let rows = o.rows(cx);
        let flagged: Vec<_> = rows
            .iter()
            .filter(|r| r.instruction_drift)
            .map(|r| r.chat.id.as_str())
            .collect();
        assert_eq!(flagged, [CHAT_API]);
    });

    draw(vcx);
    assert_eq!(marks(vcx, "overview-row-instruction-drift-"), [CHAT_API]);

    // The chat-panel header: marked for the drifted Chat only.
    for (chat, expected) in [(CHAT_AUTH, false), (CHAT_API, true)] {
        state.update(vcx, |state, _| state.selected_chat = Some(chat.into()));
        overview.update(vcx, |o, cx| {
            o.chat_panel_open = true;
            cx.notify();
        });
        draw(vcx);
        assert_eq!(
            vcx.debug_bounds("overview-chat-panel-instruction-drift")
                .is_some(),
            expected,
            "{chat}"
        );
    }
}

// ----------------------------------------------------------------------
// Work Outline instructions (Milestone 2)
// ----------------------------------------------------------------------

use crate::work_outline::instructions::fixture as instruction_fixture;

type OutlineCalls = std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

/// The instruction fixture installed, and a scripted engine that records
/// every Work Outline call and answers through `reply`.
fn instruction_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    snapshot: WorkOutlineSnapshot,
    reply: impl Fn(&serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError>
    + Send
    + Sync
    + 'static,
) -> (
    Entity<Overview>,
    OutlineCalls,
    &'a mut gpui::VisualTestContext,
) {
    let (overview, state, vcx) = outline_overview(cx, dir);
    let calls: OutlineCalls = Default::default();
    let sink = calls.clone();
    state.update(vcx, |state, _| {
        state.work_outline_actions_override =
            Some(crate::work_outline::WorkOutlineActions::scripted(
                std::sync::Arc::new(move |method, params| {
                    if method == mast_rpc::methods::LIST_FRICTION_REPORTS {
                        return Ok(serde_json::json!([]));
                    }
                    sink.lock().unwrap().push(params.clone());
                    reply(&params)
                }),
            ));
    });
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(snapshot)), cx);
        o.set_mode(OverviewMode::Outline, cx);
    });
    draw(vcx);
    (overview, calls, vcx)
}

fn reply_ok(params: &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
    Ok(match params["op"].as_str() {
        Some("createWorkItemInstruction") => serde_json::json!({"instructionId": "i-new"}),
        _ => serde_json::json!({}),
    })
}

fn open_details(
    overview: &Entity<Overview>,
    vcx: &mut gpui::VisualTestContext,
    item: &str,
) -> Entity<WorkItemDetails> {
    overview.update(vcx, |o, cx| {
        o.open_work_item_details("s1".into(), item.into(), cx)
    });
    vcx.run_until_parked();
    draw(vcx);
    overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap())
}

fn ops(calls: &OutlineCalls) -> Vec<String> {
    calls
        .lock()
        .unwrap()
        .iter()
        .map(|c| c["op"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Acceptance 28: an instruction is saved only by Save; Cancel discards;
/// closing or switching with an unsaved draft asks first and never saves.
#[gpui::test]
fn instruction_drafts_save_only_on_save_and_ask_before_discarding(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    let details = open_details(&overview, vcx, UI);
    // Collapsed until opened.
    assert!(vcx.debug_bounds("work-item-instructions").is_some());
    assert!(vcx.debug_bounds("work-item-instruction-add").is_none());
    details.update(vcx, |d, cx| {
        d.instructions_open = true;
        cx.notify();
    });
    draw(vcx);
    assert!(vcx.debug_bounds("work-item-instruction-add").is_some());

    // New draft, then Cancel: nothing is written, nothing asked.
    vcx.update(|window, cx| {
        details.update(cx, |d, cx| {
            d.begin_instruction_draft(None, window, cx);
            d.set_instruction_text("Keep the UI accessible.", cx);
        })
    });
    draw(vcx);
    assert!(vcx.debug_bounds("work-item-instruction-save").is_some());
    let cancel = vcx
        .debug_bounds("work-item-instruction-cancel")
        .unwrap()
        .center();
    vcx.simulate_click(cancel, gpui::Modifiers::default());
    draw(vcx);
    details.read_with(vcx, |d, _| {
        assert!(d.instruction_draft.is_none());
        assert!(d.captured_confirm.is_none());
    });
    assert!(calls.lock().unwrap().is_empty());

    // Draft, then close: asks; Keep Editing keeps it; no write.
    vcx.update(|window, cx| {
        details.update(cx, |d, cx| {
            d.begin_instruction_draft(None, window, cx);
            d.set_instruction_text("Keep the UI accessible.", cx);
        })
    });
    // Blur (focus moves to the outline) never saves.
    vcx.update(|window, cx| {
        let outline = overview.read(cx).outline.clone();
        let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
        window.focus(&handle, cx);
    });
    draw(vcx);
    assert!(calls.lock().unwrap().is_empty(), "blur does not save");
    details.update(vcx, |d, cx| d.request_close(cx));
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .expect("closing with a draft asks");
    assert_eq!(
        request.body.as_ref(),
        "Your instruction edits have not been saved."
    );
    vcx.update(|window, cx| (request.on_decision)(false, window, cx));
    vcx.run_until_parked();
    assert!(overview.read_with(vcx, |o, _| o.work_item_details.is_some()));
    assert!(details.read_with(vcx, |d, cx| d.has_unsaved_draft(cx)));

    // Switching items asks too; Discard switches and drops the draft.
    overview.update(vcx, |o, cx| {
        o.open_work_item_details("s1".into(), API.into(), cx)
    });
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .expect("switching with a draft asks");
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    let shown = overview.read_with(vcx, |o, cx| {
        o.work_item_details
            .as_ref()
            .unwrap()
            .read(cx)
            .item_id
            .clone()
    });
    assert_eq!(shown, API);
    assert!(calls.lock().unwrap().is_empty(), "discard never saves");

    // Save (explicit) writes once, then closes the editor.
    let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
    vcx.update(|window, cx| {
        details.update(cx, |d, cx| {
            d.begin_instruction_draft(None, window, cx);
            d.set_instruction_text("Version every endpoint.", cx);
        })
    });
    draw(vcx);
    let save = vcx
        .debug_bounds("work-item-instruction-save")
        .unwrap()
        .center();
    vcx.simulate_click(save, gpui::Modifiers::default());
    vcx.run_until_parked();
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["op"], "createWorkItemInstruction");
    assert_eq!(recorded[0]["scope"]["workItemId"], API);
    assert_eq!(recorded[0]["text"], "Version every endpoint.");
    details.read_with(vcx, |d, _| assert!(d.instruction_draft.is_none()));
}

/// Editing an inherited instruction that changes assigned Chats asks for
/// the drift confirmation before writing; Cancel writes nothing.
#[gpui::test]
fn editing_and_archiving_inherited_instructions_confirm_drift(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    let details = open_details(&overview, vcx, LAUNCH);
    vcx.update(|window, cx| {
        details.update(cx, |d, cx| {
            d.begin_instruction_draft(Some(instruction_fixture::I_LAUNCH.into()), window, cx);
            d.set_instruction_text("Ship behind the beta flag.", cx);
            d.save_instruction(cx);
        })
    });
    let request = details
        .update(vcx, |d, _| d.captured_drift.take())
        .expect("drift confirmation");
    // chat-api, chat-auth, chat-ui sit below Launch.
    assert_eq!(request.impact.chat_ids.len(), 3);
    assert_eq!(request.impact.item_ids.len(), 4);
    vcx.update(|window, cx| (request.on_decision)(false, window, cx));
    vcx.run_until_parked();
    assert!(calls.lock().unwrap().is_empty());
    details.update(vcx, |d, cx| d.save_instruction(cx));
    let request = details.update(vcx, |d, _| d.captured_drift.take()).unwrap();
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    assert_eq!(ops(&calls), ["updateWorkItemInstruction"]);
    assert_eq!(
        calls.lock().unwrap()[0]["instructionId"],
        instruction_fixture::I_LAUNCH
    );

    // Archiving the Space-root instruction from the Space-root details.
    overview.update(vcx, |o, cx| {
        o.open_work_item_details(
            "s1".into(),
            crate::work_outline::details_panel::SPACE_ROOT_ITEM_ID.into(),
            cx,
        )
    });
    vcx.run_until_parked();
    draw(vcx);
    assert!(vcx.debug_bounds("space-root-details").is_some());
    assert!(vcx.debug_bounds("work-item-instruction-0").is_some());
    let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
    details.update(vcx, |d, cx| {
        d.archive_instruction(instruction_fixture::I_ROOT.into(), cx)
    });
    let request = details.update(vcx, |d, _| d.captured_drift.take()).unwrap();
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    assert_eq!(
        ops(&calls),
        ["updateWorkItemInstruction", "archiveWorkItemInstruction"]
    );

    // A Space root without Chats below would not ask: with no
    // instructions at all an edit is not a drift.
    let mut bare = instruction_fixture::snapshot("s1");
    bare.instructions
        .retain(|i| i.work_item_id.as_deref() == Some(HOUSEKEEPING));
    bare.instructions.push(instruction_fixture::instruction(
        "i-house",
        "s1",
        Some(HOUSEKEEPING),
        "a0",
        "Tidy up.",
    ));
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(bare)), cx)
    });
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    details.update(vcx, |d, cx| d.archive_instruction("i-house".into(), cx));
    vcx.run_until_parked();
    assert!(details.read_with(vcx, |d, _| d.captured_drift.is_none()));
    assert_eq!(
        ops(&calls).last().map(String::as_str),
        Some("archiveWorkItemInstruction")
    );
}

#[gpui::test]
fn inheritance_toggle_and_association_confirm_only_when_effective_changes(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    let details = open_details(&overview, vcx, API_AUTH);
    details.update(vcx, |d, cx| {
        d.instructions_open = true;
        cx.notify();
    });
    draw(vcx);
    let toggle = vcx
        .debug_bounds("work-item-inherit-toggle")
        .unwrap()
        .center();
    vcx.simulate_click(toggle, gpui::Modifiers::default());
    let request = details
        .update(vcx, |d, _| d.captured_drift.take())
        .expect("turning inheritance off drifts chat-auth");
    assert_eq!(request.impact.chat_ids, [CHAT_AUTH]);
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded[0]["op"], "updateWorkItem");
    assert_eq!(recorded[0]["changes"]["inheritParentInstructions"], false);

    // Associating a loose chat with Housekeeping gains the Space-root
    // instruction: applies at once with an Undo toast. Without
    // instructions: applies with no toast.
    let mut outline_calls = 1;
    for (snapshot, asks) in [
        (instruction_fixture::snapshot("s1"), true),
        (crate::work_outline::store::fixture_snapshot("s1"), false),
    ] {
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(fixture_loads(Some(snapshot)), cx)
        });
        draw(vcx);
        let outline = overview.read_with(vcx, |o, _| o.outline.clone());
        outline.update(vcx, |outline, cx| {
            outline.dismiss_move_toast(cx);
            outline.select(&row_key::item("s1", HOUSEKEEPING), cx)
        });
        vcx.update(|window, cx| {
            outline.update(cx, |outline, cx| {
                outline.open_associate_picker(None, window, cx).unwrap();
                let ix = 0; // first unassociated chat
                outline.choose_picker_entry(ix, window, cx);
            })
        });
        vcx.run_until_parked();
        let toast = outline.read_with(vcx, |outline, _| outline.move_toast_text());
        assert_eq!(toast.is_some(), asks);
        outline_calls += 1;
        assert_eq!(calls.lock().unwrap().len(), outline_calls);
        outline.update_in(vcx, |outline, window, cx| outline.close_picker(window, cx));
    }
}

/// Associating or disassociating a Chat from Work Item Details applies at
/// once like an Outline move: an Undo toast (no modal) when the Chat's
/// effective instructions change, nothing when they do not.
#[gpui::test]
fn details_chat_association_toasts_drift_instead_of_confirming(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    let toast = |vcx: &mut gpui::VisualTestContext| {
        outline.read_with(vcx, |outline, _| outline.move_toast_text())
    };
    let click = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
        let at = vcx.debug_bounds(selector).expect(selector).center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        vcx.run_until_parked();
        draw(vcx);
    };

    // Housekeeping gains the Space-root instruction for the loose chat.
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    click(vcx, "work-item-associate-toggle");
    click(vcx, "work-item-associate-0");
    assert!(details.read_with(vcx, |d, _| d.captured_drift.is_none()));
    assert_eq!(ops(&calls), ["assignChatToWorkItem"]);
    let text = toast(vcx).expect("toast");
    assert!(
        text.ends_with("instructions changed for 1 chat (it keeps its current instructions)"),
        "{text}"
    );
    // Undo sends the inverse (the chat was unassociated).
    click(vcx, "outline-move-toast-undo");
    assert_eq!(
        ops(&calls),
        ["assignChatToWorkItem", "removeChatFromWorkItem"]
    );
    assert_eq!(toast(vcx), None);

    // Disassociating chat-ui from Launch loses its instructions: toast
    // with Undo that re-assigns it.
    let details = open_details(&overview, vcx, UI);
    click(vcx, "work-item-chat-remove-0");
    assert!(details.read_with(vcx, |d, _| d.captured_drift.is_none()));
    assert_eq!(
        ops(&calls).last().map(String::as_str),
        Some("removeChatFromWorkItem")
    );
    assert!(toast(vcx).is_some());
    click(vcx, "outline-move-toast-undo");
    let last = calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last["op"], "assignChatToWorkItem");
    assert_eq!(last["workItemId"], UI);

    // Without instructions nothing changes for the chat: no toast.
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(
            fixture_loads(Some(crate::work_outline::store::fixture_snapshot("s1"))),
            cx,
        )
    });
    outline.update(vcx, |outline, cx| outline.dismiss_move_toast(cx));
    let before = calls.lock().unwrap().len();
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    click(vcx, "work-item-associate-toggle");
    click(vcx, "work-item-associate-0");
    assert!(details.read_with(vcx, |d, _| d.captured_drift.is_none()));
    assert_eq!(calls.lock().unwrap().len(), before + 1);
    assert_eq!(toast(vcx), None);
}

#[gpui::test]
fn moving_an_item_toasts_drift_only_when_assigned_chats_change(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    let move_to_top = |item: &str, vcx: &mut gpui::VisualTestContext| {
        outline.update(vcx, |outline, cx| {
            outline.dismiss_move_toast(cx);
            outline.select(&row_key::item("s1", item), cx)
        });
        vcx.update(|window, cx| {
            outline.update(cx, |outline, cx| {
                outline.open_move_picker(window, cx).unwrap();
                // Entry 0 is the top level.
                outline.choose_picker_entry(0, window, cx);
            })
        });
        vcx.run_until_parked();
        outline.read_with(vcx, |outline, _| outline.move_toast_text())
    };
    // API (chat-api, and chat-auth below) loses Launch's instruction:
    // the move is sent at once, with a toast and no confirmation.
    let toast = move_to_top(API, vcx).expect("toasts");
    assert!(
        toast.contains("instructions changed for 2 chats"),
        "{toast}"
    );
    assert_eq!(ops(&calls), ["moveWorkItem"]);
    // Housekeeping is already top level with no Chats: no toast.
    outline.update_in(vcx, |outline, window, cx| outline.close_picker(window, cx));
    assert!(move_to_top(HOUSEKEEPING, vcx).is_none());
    assert_eq!(ops(&calls), ["moveWorkItem", "moveWorkItem"]);
}

#[gpui::test]
fn effective_view_orders_entries_marks_inherited_and_shows_budget(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    let details = open_details(&overview, vcx, API_AUTH);
    assert!(vcx.debug_bounds("work-item-effective").is_some());
    assert!(
        vcx.debug_bounds("work-item-effective-entry-0").is_none(),
        "collapsed until opened"
    );
    details.update(vcx, |d, cx| d.open_effective(cx));
    draw(vcx);
    for ix in 0..4 {
        assert!(row_bounds(vcx, format!("work-item-effective-entry-{ix}")).is_some());
    }
    assert!(vcx.debug_bounds("work-item-effective-entry-4").is_none());
    // Root-to-child order, top to bottom.
    let y = |vcx: &mut gpui::VisualTestContext, ix: usize| {
        row_bounds(vcx, format!("work-item-effective-entry-{ix}"))
            .unwrap()
            .origin
            .y
    };
    assert!(y(vcx, 0) < y(vcx, 1) && y(vcx, 2) < y(vcx, 3));
    let effective = instruction_fixture::snapshot("s1").effective_instructions(API_AUTH);
    assert_eq!(
        effective
            .entries
            .iter()
            .map(|e| (e.scope_title.as_str(), e.inherited))
            .collect::<Vec<_>>(),
        [
            ("Space root", true),
            ("Launch v2", true),
            ("API", true),
            ("Auth endpoints", false)
        ]
    );
    assert!(vcx.debug_bounds("work-item-effective-budget").is_some());
    assert!(vcx.debug_bounds("work-item-effective-precedence").is_some());
    assert!(
        vcx.debug_bounds("work-item-effective-budget-error")
            .is_none()
    );

    // Over budget: 33 entries.
    let mut big = instruction_fixture::snapshot("s1");
    for i in 0..30 {
        big.instructions.push(instruction_fixture::instruction(
            &format!("i-extra-{i:02}"),
            "s1",
            Some(API_AUTH),
            &format!("b{i:02}"),
            "Extra.",
        ));
    }
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(big)), cx)
    });
    draw(vcx);
    assert!(
        vcx.debug_bounds("work-item-effective-budget-error")
            .is_some()
    );

    // The palette command opens the selected item's view expanded.
    overview.update(vcx, |o, cx| o.close_work_item_details(cx));
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    outline.update(vcx, |outline, cx| {
        outline.select(&row_key::item("s1", API), cx)
    });
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::OpenEffectiveInstructions, window, cx)
        })
    });
    vcx.run_until_parked();
    overview.read_with(vcx, |o, cx| {
        let details = o.work_item_details.as_ref().unwrap().read(cx);
        assert_eq!(details.item_id, API);
        assert!(details.effective_open);
    });
}

#[gpui::test]
fn outline_chat_rows_flag_drift_only_for_drifted_chats(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, _calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    assert!(row_bounds(vcx, format!("outline-drift-{}", row_key::chat(CHAT_API))).is_some());
    assert!(row_bounds(vcx, format!("outline-drift-{}", row_key::chat(CHAT_AUTH))).is_none());
    assert!(row_bounds(vcx, format!("outline-drift-{}", row_key::chat(CHAT_UI))).is_none());
}

/// Single Space: the Space-root entry opens the Space-root details.
#[gpui::test]
fn space_root_entry_opens_space_root_instructions(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    let entry = vcx
        .debug_bounds("outline-space-root-entry")
        .expect("single-Space entry")
        .center();
    vcx.simulate_click(entry, gpui::Modifiers::default());
    draw(vcx);
    assert!(vcx.debug_bounds("space-root-details").is_some());
    overview.read_with(vcx, |o, cx| {
        assert!(
            o.work_item_details
                .as_ref()
                .unwrap()
                .read(cx)
                .is_space_root()
        );
    });
    // Its instruction list is open by default and lists the root entry.
    assert!(vcx.debug_bounds("work-item-instruction-0").is_some());
    assert!(vcx.debug_bounds("work-item-instruction-1").is_none());
}

#[gpui::test]
fn long_instructions_collapse_and_controls_sit_in_the_header(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = instruction_fixture::snapshot("s1");
    let long = "Keep every change small and reviewable. ".repeat(20);
    for instruction in snapshot.instructions.iter_mut() {
        if instruction.id == instruction_fixture::I_ROOT {
            instruction.text = long.clone();
        }
    }
    snapshot.instructions.push(instruction_fixture::instruction(
        "i-root-short",
        "s1",
        None,
        "zz",
        "Be brief.",
    ));
    let (overview, calls, vcx) = instruction_overview(cx, dir.path(), snapshot, reply_ok);
    overview.update(vcx, |o, cx| {
        o.open_work_item_details(
            "s1".into(),
            crate::work_outline::details_panel::SPACE_ROOT_ITEM_ID.into(),
            cx,
        )
    });
    vcx.run_until_parked();
    draw(vcx);

    // Only the long entry gets a toggle.
    assert!(vcx.debug_bounds("work-item-instruction-fold-0").is_some());
    assert!(vcx.debug_bounds("work-item-instruction-fold-1").is_none());
    let collapsed = vcx.debug_bounds("work-item-instruction-text-0").unwrap();
    let short = vcx.debug_bounds("work-item-instruction-text-1").unwrap();
    let line = f32::from(short.size.height);

    // The controls live in the header row, above the text, and the
    // short entry's controls are at the top of its entry too.
    for ix in 0..2 {
        let mut named = |name: String| vcx.debug_bounds(Box::leak(name.into_boxed_str())).unwrap();
        let header = named(format!("work-item-instruction-header-{ix}"));
        let text = named(format!("work-item-instruction-text-{ix}"));
        for control in ["edit", "archive"] {
            let b = named(format!("work-item-instruction-{control}-{ix}"));
            assert!(b.top() >= header.top() && b.bottom() <= header.bottom());
            assert!(b.bottom() <= text.top() + px(1.0));
        }
    }
    // Collapsed to four lines: well short of the full text, and the
    // controls did not move down with the text.
    assert!(f32::from(collapsed.size.height) <= line * 4.0 + 1.0);
    let edit_top = vcx
        .debug_bounds("work-item-instruction-edit-0")
        .unwrap()
        .top();

    // Show more expands, Show less collapses again.
    let fold = vcx
        .debug_bounds("work-item-instruction-fold-0")
        .unwrap()
        .center();
    vcx.simulate_click(fold, gpui::Modifiers::default());
    draw(vcx);
    let expanded = vcx.debug_bounds("work-item-instruction-text-0").unwrap();
    assert!(f32::from(expanded.size.height) > f32::from(collapsed.size.height) * 1.5);
    assert_eq!(
        vcx.debug_bounds("work-item-instruction-edit-0")
            .unwrap()
            .top(),
        edit_top
    );
    let fold = vcx
        .debug_bounds("work-item-instruction-fold-0")
        .unwrap()
        .center();
    vcx.simulate_click(fold, gpui::Modifiers::default());
    draw(vcx);
    let again = vcx.debug_bounds("work-item-instruction-text-0").unwrap();
    assert_eq!(again.size.height, collapsed.size.height);
    assert!(calls.lock().unwrap().is_empty());
}

/// Instruction "Archive" is now "Remove" (confirmation and tooltip); the
/// archive operation itself is covered by the drift-confirmation test.
#[test]
fn removing_an_instruction_is_labelled_remove() {
    use crate::work_outline::hints::details_button_hint;
    use crate::work_outline::instructions::OutlineChange;
    let change = OutlineChange::ArchiveInstruction {
        instruction_id: "i".into(),
    };
    assert_eq!(change.title(), "Remove instruction?");
    assert_eq!(change.confirm_label(), "Remove");
    assert!(details_button_hint("Remove").is_some());
}

#[gpui::test]
fn unsupported_engine_shows_a_calm_instruction_error(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) =
        instruction_overview(cx, dir.path(), instruction_fixture::snapshot("s1"), |_| {
            Err(mast_rpc::RpcError::Failed(
                "work-outline/unsupported: instruction operations are not available".into(),
            ))
        });
    let details = open_details(&overview, vcx, UI);
    vcx.update(|window, cx| {
        details.update(cx, |d, cx| {
            d.begin_instruction_draft(None, window, cx);
            d.set_instruction_text("Anything.", cx);
            d.save_instruction(cx);
        })
    });
    vcx.run_until_parked();
    draw(vcx);
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(vcx.debug_bounds("work-item-instructions-error").is_some());
    details.read_with(vcx, |d, cx| {
        assert!(d.instruction_draft.is_some(), "the draft survives");
        assert!(d.has_unsaved_draft(cx));
    });
}

// ----------------------------------------------------------------------
// Execution configuration (Milestone 3)
// ----------------------------------------------------------------------

use crate::work_outline::execution::fixture as execution_fixture;

fn identity(repo: &str, path: &str) -> serde_json::Value {
    serde_json::json!({
        "repoId": repo, "repoFingerprint": format!("root-{repo}"), "repoPath": path,
        "checkoutId": format!("checkout:{repo}"), "mainCheckout": true,
        "worktrees": [
            {"path": path, "checkoutId": format!("checkout:{repo}"), "branch": "main", "main": true},
            {"path": format!("{path}-wt"), "checkoutId": format!("checkout:{repo}-wt"),
             "branch": "feature", "main": false},
        ],
    })
}

/// Mutations answer `{}`; `DescribeExecutionRepository` knows the fixture
/// repositories and refuses anything else as non-Git; validation answers
/// with `validate`.
fn execution_reply(
    validate: std::sync::Arc<std::sync::Mutex<serde_json::Value>>,
) -> impl Fn(&serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> + Send + Sync + 'static
{
    move |params| {
        if params.get("op").is_some() {
            return Ok(serde_json::json!({}));
        }
        if let Some(path) = params.get("path").and_then(|p| p.as_str()) {
            return match path {
                "/work/alpha/ui" => Ok(identity("repo:ui", path)),
                "/work/alpha/flagship" => Ok(identity("repo:flagship", path)),
                other => Err(mast_rpc::RpcError::Failed(format!(
                    "{other} is not a Git repository"
                ))),
            };
        }
        Ok(validate.lock().unwrap().clone())
    }
}

fn validate_ok() -> std::sync::Arc<std::sync::Mutex<serde_json::Value>> {
    std::sync::Arc::new(std::sync::Mutex::new(
        serde_json::to_value(execution_fixture::launch_validated()).unwrap(),
    ))
}

fn mutations(calls: &OutlineCalls) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.get("op").is_some())
        .cloned()
        .collect()
}

/// The Execution section is collapsed until opened, shows the resolved
/// configuration, offers Reset only for an item with its own row, and
/// lists exception Chats (with the compact marker on the header and on
/// the Outline row).
#[gpui::test]
fn execution_section_shows_resolution_reset_and_exceptions(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        execution_fixture::snapshot("s1"),
        execution_reply(validate_ok()),
    );
    // Outline rows: only API (one exception) carries the marker.
    assert!(
        row_bounds(
            vcx,
            format!("outline-execution-exceptions-{}", row_key::item("s1", API))
        )
        .is_some()
    );
    assert!(
        row_bounds(
            vcx,
            format!("outline-execution-exceptions-{}", row_key::item("s1", UI))
        )
        .is_none()
    );

    let details = open_details(&overview, vcx, API);
    assert!(vcx.debug_bounds("work-item-execution").is_some());
    assert!(
        vcx.debug_bounds("work-item-execution-exceptions-marker")
            .is_some()
    );
    assert!(
        vcx.debug_bounds("work-item-execution-source").is_none(),
        "collapsed"
    );
    details.update(vcx, |d, cx| {
        d.execution_open = true;
        cx.notify();
    });
    draw(vcx);
    assert!(vcx.debug_bounds("work-item-execution-source").is_some());
    assert!(vcx.debug_bounds("work-item-execution-repo-0").is_some());
    assert!(vcx.debug_bounds("work-item-execution-reset").is_some());
    assert!(
        vcx.debug_bounds("work-item-execution-exception-0")
            .is_some()
    );

    // UI inherits from Launch: no Reset, no exceptions.
    let details = open_details(&overview, vcx, UI);
    details.update(vcx, |d, cx| {
        d.execution_open = true;
        cx.notify();
    });
    draw(vcx);
    assert!(vcx.debug_bounds("work-item-execution-repo-1").is_some());
    assert!(vcx.debug_bounds("work-item-execution-reset").is_none());
    assert!(
        vcx.debug_bounds("work-item-execution-exceptions-marker")
            .is_none()
    );
    assert!(vcx.debug_bounds("work-item-execution-exceptions").is_none());
}

/// Edit form: explicit Save/Cancel, the discard policy, the drift
/// confirmation for item Chats, and the written configuration.
#[gpui::test]
fn execution_edits_save_explicitly_and_confirm_drift(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        execution_fixture::snapshot("s1"),
        execution_reply(validate_ok()),
    );
    let details = open_details(&overview, vcx, UI);
    vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_execution_draft(window, cx)));
    vcx.run_until_parked();
    draw(vcx);
    // Prefilled with the inherited configuration; worktree choices were
    // described on the owning device.
    let described: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .filter_map(|c| c.get("path").and_then(|p| p.as_str()).map(str::to_owned))
        .collect();
    assert_eq!(described, ["/work/alpha/ui", "/work/alpha/flagship"]);
    // The Space lives on device `d`, not this app's engine: Describe is
    // routed there (only the owner can fingerprint its repositories).
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["targetDeviceId"] == "d")
    );
    assert!(vcx.debug_bounds("work-item-execution-editor").is_some());
    assert!(
        vcx.debug_bounds("work-item-execution-worktree-1-2")
            .is_some(),
        "linked worktree offered"
    );
    details.read_with(vcx, |d, cx| {
        assert_eq!(d.execution_draft.as_ref().unwrap().bindings.len(), 2);
        assert!(!d.execution_draft_dirty(cx));
    });

    // Cancel discards; nothing written, nothing asked.
    let cancel = vcx
        .debug_bounds("work-item-execution-cancel")
        .unwrap()
        .center();
    vcx.simulate_click(cancel, gpui::Modifiers::default());
    draw(vcx);
    details.read_with(vcx, |d, _| {
        assert!(d.execution_draft.is_none());
        assert!(d.captured_confirm.is_none());
    });
    assert!(mutations(&calls).is_empty());

    // Edit: flagship becomes primary (radio: ui stops being primary) and
    // pins its linked worktree.
    vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_execution_draft(window, cx)));
    vcx.run_until_parked();
    draw(vcx);
    let primary = vcx
        .debug_bounds("work-item-execution-primary-1")
        .unwrap()
        .center();
    vcx.simulate_click(primary, gpui::Modifiers::default());
    draw(vcx);
    let worktree = vcx
        .debug_bounds("work-item-execution-worktree-1-2")
        .unwrap()
        .center();
    vcx.simulate_click(worktree, gpui::Modifiers::default());
    draw(vcx);
    details.read_with(vcx, |d, cx| {
        let draft = d.execution_draft.as_ref().unwrap();
        let primaries: Vec<bool> = draft.bindings.iter().map(|b| b.binding.primary).collect();
        assert_eq!(primaries, [false, true]);
        assert_eq!(
            draft.bindings[1].binding.worktree_path.as_deref(),
            Some("/work/alpha/flagship-wt")
        );
        assert!(d.execution_draft_dirty(cx));
    });

    // Closing with the dirty draft asks; Keep Editing keeps it.
    details.update(vcx, |d, cx| d.request_close(cx));
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .expect("asks");
    assert_eq!(
        request.body.as_ref(),
        "Your execution configuration edits have not been saved."
    );
    vcx.update(|window, cx| (request.on_decision)(false, window, cx));
    vcx.run_until_parked();
    assert!(details.read_with(vcx, |d, cx| d.has_unsaved_draft(cx)));
    assert!(mutations(&calls).is_empty(), "blur and close never save");

    // Save: chat-ui runs with UI's current configuration, so the drift
    // confirmation states it keeps its captured configuration.
    let save = vcx
        .debug_bounds("work-item-execution-save")
        .unwrap()
        .center();
    vcx.simulate_click(save, gpui::Modifiers::default());
    vcx.run_until_parked();
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .expect("drift confirmation");
    assert_eq!(
        request.title.as_ref(),
        "Change the execution configuration?"
    );
    assert!(request.body.contains("keep their captured configuration"));
    assert!(request.body.contains("exceptions"));
    assert!(mutations(&calls).is_empty(), "nothing before confirming");
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    let written = mutations(&calls);
    assert_eq!(written.len(), 1);
    assert_eq!(written[0]["op"], "configureExecution");
    assert_eq!(
        written[0]["scope"],
        serde_json::json!({"type": "workItem", "workItemId": UI})
    );
    let bindings = written[0]["configuration"]["bindings"].as_array().unwrap();
    assert_eq!(bindings[1]["primary"], true);
    assert_eq!(bindings[1]["worktreeId"], "checkout:repo:flagship-wt");
    assert_eq!(bindings[0]["primary"], false);
    details.read_with(vcx, |d, _| assert!(d.execution_draft.is_none()));
}

/// Repositories are added through the folder picker and described on the
/// owning device; a non-Git folder is refused. No bound Chats: no
/// confirmation.
#[gpui::test]
fn execution_repositories_are_described_and_non_git_folders_refused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        execution_fixture::snapshot("s1"),
        execution_reply(validate_ok()),
    );
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_execution_draft(window, cx)));
    draw(vcx);
    details.read_with(vcx, |d, cx| {
        assert!(d.execution_draft.as_ref().unwrap().bindings.is_empty());
        assert_eq!(
            d.execution_draft_configuration(cx).unwrap().execution_root,
            "/work/alpha"
        );
    });
    let pick = |vcx: &mut gpui::VisualTestContext, selector: &'static str, path: &str| {
        let button = vcx.debug_bounds(selector).unwrap().center();
        vcx.simulate_click(button, gpui::Modifiers::default());
        vcx.run_until_parked();
        let request = details
            .update(vcx, |d, _| d.captured_folder_pick.take())
            .expect("folder picker");
        assert_eq!(request.device_id, "d");
        let path = path.to_owned();
        vcx.update(|_, cx| (request.on_pick)(path, cx));
        vcx.run_until_parked();
        draw(vcx);
    };
    pick(vcx, "work-item-execution-add-repo", "/work/alpha/docs");
    details.read_with(vcx, |d, _| {
        assert!(d.execution_draft.as_ref().unwrap().bindings.is_empty());
        assert!(
            d.execution_error
                .as_deref()
                .unwrap()
                .contains("not a Git repository")
        );
    });
    pick(vcx, "work-item-execution-add-repo", "/work/alpha/ui");
    pick(vcx, "work-item-execution-choose-root", "/work/alpha/ui");
    details.read_with(vcx, |d, cx| {
        let draft = d.execution_draft.as_ref().unwrap();
        assert_eq!(draft.bindings.len(), 1);
        assert_eq!(draft.bindings[0].binding.repo_fingerprint, "root-repo:ui");
        assert_eq!(
            d.execution_draft_configuration(cx).unwrap().execution_root,
            "/work/alpha/ui"
        );
    });
    let save = vcx
        .debug_bounds("work-item-execution-save")
        .unwrap()
        .center();
    vcx.simulate_click(save, gpui::Modifiers::default());
    vcx.run_until_parked();
    details.read_with(vcx, |d, _| assert!(d.captured_confirm.is_none()));
    let written = mutations(&calls);
    assert_eq!(written.len(), 1);
    assert_eq!(
        written[0]["configuration"]["executionRoot"],
        "/work/alpha/ui"
    );
    assert_eq!(
        written[0]["configuration"]["bindings"][0]["repoId"],
        "repo:ui"
    );
}

/// Validate runs on the owning device (the engine forwards; no target
/// device here) and shows each error's message and remediation, with
/// offline and unsupported owners as their own states.
#[gpui::test]
fn execution_validation_shows_remediation_and_distinct_owner_states(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let validate = validate_ok();
    let (overview, calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        execution_fixture::snapshot("s1"),
        execution_reply(validate.clone()),
    );
    let details = open_details(&overview, vcx, API);
    details.update(vcx, |d, cx| {
        d.execution_open = true;
        cx.notify();
    });
    draw(vcx);
    let cases = [
        (
            mast_proto::ExecutionValidationError::DeviceOffline {
                device_id: "d".into(),
                detail: String::new(),
            },
            "work-item-execution-validation-device-offline",
        ),
        (
            mast_proto::ExecutionValidationError::HostUnsupported {
                device_id: "d".into(),
                capability: "work-outline-multi-repo-v1".into(),
            },
            "work-item-execution-validation-host-unsupported",
        ),
        (
            mast_proto::ExecutionValidationError::WorktreeMoved {
                repo_id: "repo:flagship".into(),
                worktree_id: "wt".into(),
                expected_path: "/a".into(),
                found_path: "/b".into(),
            },
            "work-item-execution-validation-blocked",
        ),
    ];
    for (error, selector) in cases {
        *validate.lock().unwrap() = serde_json::to_value(
            mast_proto::ValidateExecutionConfigurationReply::from_result(None, Err(vec![error])),
        )
        .unwrap();
        let button = vcx
            .debug_bounds("work-item-execution-validate")
            .unwrap()
            .center();
        vcx.simulate_click(button, gpui::Modifiers::default());
        vcx.run_until_parked();
        draw(vcx);
        assert!(vcx.debug_bounds(selector).is_some(), "{selector}");
        assert!(vcx.debug_bounds("work-item-execution-issue-0").is_some());
    }
    let last = calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        last["scope"],
        serde_json::json!({"type": "workItem", "workItemId": API})
    );
    assert!(last.get("targetDeviceId").is_none());
    details.read_with(vcx, |d, _| {
        let crate::work_outline::execution::ValidationState::Done(reply) = &d.execution_validation
        else {
            panic!("validated");
        };
        let issue = &crate::work_outline::execution::validation_issues(reply)[0];
        assert!(issue.message.contains("moved from /a to /b"));
        assert!(issue.remediation.contains("Update the worktree path"));
    });
    // Ready: no issues.
    *validate.lock().unwrap() =
        serde_json::to_value(execution_fixture::launch_validated()).unwrap();
    let button = vcx
        .debug_bounds("work-item-execution-validate")
        .unwrap()
        .center();
    vcx.simulate_click(button, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
    assert!(
        vcx.debug_bounds("work-item-execution-validation-ready")
            .is_some()
    );
    assert!(vcx.debug_bounds("work-item-execution-issue-0").is_none());

    // A dirty draft validates the draft (as a resolved snapshot).
    vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_execution_draft(window, cx)));
    details.update(vcx, |d, cx| d.set_execution_root("/work/alpha", cx));
    details.update(vcx, |d, cx| d.validate_execution(cx));
    vcx.run_until_parked();
    let last = calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        last["resolved"]["configuration"]["executionRoot"],
        "/work/alpha"
    );
    assert_eq!(last["resolved"]["deviceId"], "d");
    assert!(last.get("scope").is_none());
}

/// Reset to inherited confirms when bound Chats would differ and writes
/// a null configuration; the Space-root variant edits the Space default.
#[gpui::test]
fn execution_reset_and_space_default(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        execution_fixture::snapshot("s1"),
        execution_reply(validate_ok()),
    );
    let details = open_details(&overview, vcx, API);
    details.update(vcx, |d, cx| d.reset_execution(cx));
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .expect("chat-auth would differ");
    assert_eq!(request.confirm_label.as_ref(), "Reset");
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    let written = mutations(&calls);
    assert_eq!(written.len(), 1);
    assert!(written[0]["configuration"].is_null());

    let details = open_details(&overview, vcx, "");
    assert!(details.read_with(vcx, |d, _| d.is_space_root()));
    assert!(vcx.debug_bounds("work-item-execution").is_some());
    vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_execution_draft(window, cx)));
    details.update(vcx, |d, cx| {
        d.set_execution_root("/work/alpha/ui", cx);
        d.save_execution(cx);
    });
    vcx.run_until_parked();
    // Housekeeping (implicit) and Launch? Only Housekeeping changes, and
    // it has no Chats: no confirmation.
    details.read_with(vcx, |d, _| assert!(d.captured_confirm.is_none()));
    let written = mutations(&calls);
    assert_eq!(written.len(), 2);
    assert_eq!(
        written[1]["scope"],
        serde_json::json!({"type": "space", "spaceId": "s1"})
    );
}

/// Start Chat entry points: the Outline row action on the selected item,
/// the Work Item Details button, and the palette command (which needs a
/// selected item). Changed instructions open Effective Instructions in
/// details first.
#[gpui::test]
fn start_chat_entry_points_reach_the_shell_and_review_changed_instructions(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = execution_fixture::snapshot("s1");
    let instructions = instruction_fixture::snapshot("s1");
    snapshot.instructions = instructions.instructions;
    snapshot.chat_instruction_states = instructions.chat_instruction_states;
    snapshot.revisions = instructions.revisions;
    let (overview, _calls, vcx) =
        instruction_overview(cx, dir.path(), snapshot, execution_reply(validate_ok()));

    // Palette without a selection: a notice, no request.
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::StartChatForWorkItem, window, cx)
        })
    });
    draw(vcx);
    assert!(overview.read_with(vcx, |o, _| o.captured_start_chat.is_none()));

    // Outline row action on the selected item (Auth: instructions
    // current, so details stay closed).
    let auth_key = row_key::item("s1", API_AUTH);
    overview.update(vcx, |o, cx| {
        o.outline
            .update(cx, |outline, cx| outline.select(&auth_key, cx))
    });
    draw(vcx);
    assert!(
        row_bounds(
            vcx,
            format!("outline-start-chat-{}", row_key::item("s1", API))
        )
        .is_none()
    );
    let action = row_bounds(vcx, format!("outline-start-chat-{auth_key}"))
        .unwrap()
        .center();
    vcx.simulate_click(action, gpui::Modifiers::default());
    draw(vcx);
    overview.update(vcx, |o, _| {
        assert_eq!(
            o.captured_start_chat.take(),
            Some(("s1".to_string(), API_AUTH.to_string()))
        );
        assert!(o.work_item_details.is_none());
    });

    // Palette with API selected: API's instructions changed since its
    // last launch, so Effective Instructions open in details first.
    let api_key = row_key::item("s1", API);
    overview.update(vcx, |o, cx| {
        o.outline
            .update(cx, |outline, cx| outline.select(&api_key, cx))
    });
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::StartChatForWorkItem, window, cx)
        })
    });
    vcx.run_until_parked();
    draw(vcx);
    let details = overview.update(vcx, |o, _| {
        assert_eq!(
            o.captured_start_chat.take(),
            Some(("s1".to_string(), API.to_string()))
        );
        o.work_item_details
            .clone()
            .expect("Effective Instructions opened")
    });
    assert!(details.read_with(vcx, |d, _| d.effective_open));

    // The details button.
    let button = vcx.debug_bounds("work-item-start-chat").unwrap().center();
    vcx.simulate_click(button, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        overview.update(vcx, |o, _| o.captured_start_chat.take()),
        Some(("s1".to_string(), API.to_string()))
    );
}

/// Resizable Work Item Details panel; shares this module's fixtures.
mod resizable_panes_tests {
    include!("../work_outline/resizable_panes_tests.rs");
}

/// Milestone 4 (Required Actions, proposals); the body lives beside the
/// Work Outline modules and shares this module's fixtures.
mod required_actions_tests {
    include!("../work_outline/required_actions_tests.rs");
}

/// Linear integration (panel, gating, chips, drops); the body lives
/// beside the Work Outline modules and shares this module's fixtures.
mod linear_tests {
    include!("../work_outline/linear_tests.rs");
}

/// PR review sync (destination, chips, the GitHub section, ⌘↩ reviews,
/// system Required Actions); shares this module's fixtures.
mod pr_sync_tests {
    include!("../work_outline/pr_sync_tests.rs");
}

/// Direct restructuring (keyboard, palette, picker placement, drag and
/// drop); shares this module's fixtures.
mod restructure_tests {
    include!("../work_outline/restructure_tests.rs");
}

/// Undo / redo and Recent changes; shares this module's fixtures.
mod undo_tests {
    include!("../work_outline/undo_tests.rs");
}

/// Multi-selection and "Group into…"; shares this module's fixtures.
mod group_tests {
    include!("../work_outline/group_tests.rs");
}
mod project_ui_tests {
    include!("../work_outline/project_ui_tests.rs");
}
/// Project templates: save, list, New from template, Linear entry.
mod templates_tests {
    include!("../work_outline/templates_tests.rs");
}
/// "Template changed" chip and per-project review.
mod convert_tests {
    include!("../work_outline/convert_tests.rs");
}
mod template_review_tests {
    include!("../work_outline/template_review_tests.rs");
}

/// First-use empty state of the Outline; shares this module's fixtures.
mod guidance_tests {
    include!("../work_outline/guidance_tests.rs");
}

/// `/` skill autocomplete and auto-grow of the Work Item Details
/// editors; shares this module's fixtures.
mod editor_skills_tests {
    include!("../work_outline/editor_skills_tests.rs");
}

/// Space-root reorder and hide / unhide; shares this module's fixtures.
mod space_roots_tests {
    include!("../work_outline/space_roots_tests.rs");
}

/// Outline tree guides, active path, drop labels, move flash; shares
/// this module's fixtures.
mod tree_guides_tests {
    include!("../work_outline/tree_guides_tests.rs");
}

/// Compact single-chat rows: rendering, open, menu, keys, drag.
mod compact_rows_tests {
    include!("../work_outline/compact_rows_tests.rs");
}
