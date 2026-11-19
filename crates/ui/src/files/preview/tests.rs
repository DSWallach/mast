use super::*;

#[gpui::test]
fn file_outline_tracks_the_open_document_revision(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| { gpui_base::init(cx); cx.set_global(Theme::dark()); });
    let window = cx.add_window(|_, cx| {
        let state = cx.new(|_| crate::state::AppState::new());
        FilesSurface::new(state, "chat".into(), false, 1000, 13.0, false, false, cx)
    });
    window.update(cx, |surface, _, cx| {
        let text = "fn first() {}\nfn second() {}\n";
        let mut document = FileDocument::loading(DocumentKey { chat_id: "chat".into(), checkout_id: Some("checkout".into()), path: "src/a.rs".into() });
        document.set_loaded(mast_proto::WorkspaceFileText { checkout_id: "checkout".into(), path: "src/a.rs".into(), text: Some(text.into()), content_hash: Some("hash".into()), size: text.len() as u64, modified_at: None, encoding: mast_proto::WorkspaceTextEncoding::Utf8, line_ending: Some(mast_proto::WorkspaceLineEnding::Lf), read_only_reason: None, truncated: false });
        surface.preview.documents.insert("src/a.rs".into(), document);
        surface.preview.active = Some("src/a.rs".into());
        surface.preview.outline_open = true;
        surface.request_file_outline(cx);
    }).unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(150));
    cx.run_until_parked();
    window.update(cx, |surface, _, cx| {
        assert_eq!(surface.preview.outline_symbols.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["first", "second"]);
        surface.reveal_editor_location(surface.preview.outline_symbols[1].start_line, None, cx);
        assert_eq!(surface.pending_location_for_test(), Some((2, None)));
    }).unwrap();
}

#[test]
fn source_locations_are_one_based_utf8_and_clamped() {
    let text = "zero\none éx\ntwo";
    assert_eq!(source_location_offset(text, 1, None), 0);
    assert_eq!(source_location_offset(text, 2, Some(1)), 5);
    assert_eq!(source_location_offset(text, 2, Some(6)), 11);
    assert_eq!(source_location_offset(text, 99, None), text.len());
}

// Rendering a preview needs a window, so this asserts on `line_height`,
// the single source both the uniform-height hint passed to
// `reset_with_uniform_height` and the painted row in
// `render_preview_line` read.
#[test]
fn preview_rows_track_the_code_font_size_from_the_default_to_the_maximum() {
    let default = FilePreviewState::new(false, 900, false, 12.5);
    assert_eq!(default.line_height(), px(PREVIEW_LINE_HEIGHT));

    let maximum = FilePreviewState::new(false, 900, false, 32.0);
    assert!(maximum.line_height() > px(PREVIEW_LINE_HEIGHT));

    let minimum = FilePreviewState::new(false, 900, false, 8.0);
    assert_eq!(minimum.line_height(), px(PREVIEW_LINE_HEIGHT));
}

/// The regression this guards: collapsing both surfaces onto the raw
/// setting silently resized them on a fresh install.
#[test]
fn the_default_code_font_size_reproduces_the_historical_per_surface_sizes() {
    let default =
        FilePreviewState::new(false, 900, false, crate::typography::CODE_FONT_SIZE_DEFAULT);
    assert_eq!(default.editor_text_size(), 13.0);
    assert_eq!(default.preview_text_size(), 11.5);
}

#[test]
fn scaled_preview_sizes_keep_their_proportions_and_stay_clamped() {
    let doubled = FilePreviewState::new(
        false,
        900,
        false,
        2.0 * crate::typography::CODE_FONT_SIZE_DEFAULT,
    );
    assert_eq!(doubled.editor_text_size(), 26.0);
    assert_eq!(doubled.preview_text_size(), 23.0);

    // The editor ratio is >1, so the maximum setting would overshoot.
    let maximum = FilePreviewState::new(false, 900, false, crate::typography::FONT_SIZE_MAX);
    assert_eq!(maximum.editor_text_size(), crate::typography::FONT_SIZE_MAX);
    assert!(maximum.preview_text_size() < crate::typography::FONT_SIZE_MAX);
}

fn cached_document(path: &str, text: &str) -> FileDocument {
    let mut document = FileDocument::loading(DocumentKey {
        chat_id: "chat-1".into(),
        checkout_id: Some("checkout-1".into()),
        path: path.into(),
    });
    document.phase = DocumentPhase::Ready;
    document.lines = Arc::new(vec![SharedString::from(text.to_string())]);
    document
}

#[test]
fn read_only_reasons_have_specific_messages() {
    assert!(read_only_message(Some(WorkspaceReadOnlyReason::Binary)).contains("Binary"));
    assert!(
        read_only_message(Some(WorkspaceReadOnlyReason::UnsupportedEncoding))
            .contains("encoding")
    );
}

#[test]
fn reset_drops_documents_and_active_preview_from_the_previous_target() {
    let mut preview = FilePreviewState::new(false, 900, false, 11.5);
    preview.active = Some("private.env".into());
    preview.documents.insert(
        "private.env".into(),
        FileDocument::loading(DocumentKey {
            chat_id: "chat-1".into(),
            checkout_id: Some("checkout-1".into()),
            path: "private.env".into(),
        }),
    );
    preview
        .documents
        .get_mut("private.env")
        .unwrap()
        .mark_external(None);

    preview.reset();

    assert!(preview.documents.is_empty());
    assert!(preview.active.is_none());
}

#[test]
fn document_cache_evicts_the_oldest_safe_entries() {
    let mut preview = FilePreviewState::new(false, 900, false, 11.5);
    for index in 0..18 {
        let path = format!("src/{index}.rs");
        preview
            .documents
            .insert(path.clone(), cached_document(&path, "fn main() {}"));
        preview.touch_document(&path);
    }
    preview.active = Some("src/0.rs".into());
    preview.documents.get_mut("src/1.rs").unwrap().revision = 1;
    preview.documents.get_mut("src/2.rs").unwrap().phase =
        DocumentPhase::Conflict { disk_hash: None };
    preview
        .documents
        .get_mut("src/3.rs")
        .unwrap()
        .review_comment_flush_pending = true;
    preview.documents.get_mut("src/7.rs").unwrap().phase = DocumentPhase::Loading;
    let protected = HashSet::from(["src/4.rs".to_string()]);

    let evicted = preview.trim_document_cache_to(&protected, 15, usize::MAX);

    assert_eq!(evicted, vec!["src/5.rs", "src/6.rs", "src/8.rs"]);
    for retained in ["src/0.rs", "src/1.rs", "src/2.rs", "src/3.rs", "src/4.rs"] {
        assert!(preview.documents.contains_key(retained));
    }
    assert!(preview.documents.contains_key("src/7.rs"));
}

#[test]
fn document_cache_uses_retained_bytes_not_only_entry_count() {
    let mut preview = FilePreviewState::new(false, 900, false, 11.5);
    for path in ["old.rs", "active.rs"] {
        preview
            .documents
            .insert(path.into(), cached_document(path, &"x".repeat(4 * 1024)));
        preview.touch_document(path);
    }
    preview.active = Some("active.rs".into());
    let active_bytes = preview.documents["active.rs"].estimated_retained_bytes();
    let total_bytes = preview.retained_document_bytes();

    let evicted = preview.trim_document_cache_to(
        &HashSet::new(),
        usize::MAX,
        total_bytes.saturating_sub(1),
    );

    assert_eq!(evicted, vec!["old.rs"]);
    assert!(preview.retained_document_bytes() <= active_bytes);
}

#[test]
fn document_eviction_cleans_path_scoped_companion_state() {
    let mut preview = FilePreviewState::new(false, 900, false, 11.5);
    for path in ["old.rs", "active.rs"] {
        preview
            .documents
            .insert(path.into(), cached_document(path, "fn main() {}"));
        preview.touch_document(path);
    }
    preview.active = Some("active.rs".into());
    preview
        .comment_anchors
        .insert("old.rs".into(), HashMap::new());
    let highlighted = Arc::new(
        mast_syntax::highlight(mast_syntax::HighlightRequest {
            source: "fn main() {}",
            path: Some("old.rs"),
            fence_tag: None,
        })
        .unwrap(),
    );
    preview.highlights.insert(
        "old.rs".into(),
        HighlightedFile {
            content_hash: "hash".into(),
            document: highlighted,
        },
    );

    let evicted = preview.trim_document_cache_to(&HashSet::new(), 1, usize::MAX);

    assert_eq!(evicted, vec!["old.rs"]);
    assert!(!preview.highlights.contains_key("old.rs"));
    assert!(!preview.comment_anchors.contains_key("old.rs"));
    assert!(!preview.document_recency.iter().any(|path| path == "old.rs"));
}

#[test]
fn reset_preserves_global_editor_preferences() {
    let mut preview = FilePreviewState::new(false, 900, true, 15.0);

    preview.reset();

    assert!(!preview.autosave_enabled);
    assert!(preview.word_wrap());
    assert_eq!(preview.editor_font_size, 15.0);
}

#[test]
fn file_highlight_completion_rejects_a_result_after_a_user_edit() {
    let path = "src/lib.rs";
    let disk_hash = "disk-hash-1";
    let stale_source = "ab";
    let updated_source = "é";
    let mut document = FileDocument::loading(DocumentKey {
        chat_id: "chat-1".into(),
        checkout_id: Some("checkout-1".into()),
        path: path.into(),
    });
    document.set_loaded(mast_proto::WorkspaceFileText {
        checkout_id: "checkout-1".into(),
        path: path.into(),
        text: Some(stale_source.into()),
        content_hash: Some(disk_hash.into()),
        size: stale_source.len() as u64,
        modified_at: None,
        encoding: mast_proto::WorkspaceTextEncoding::Utf8,
        line_ending: Some(mast_proto::WorkspaceLineEnding::Lf),
        read_only_reason: None,
        truncated: false,
    });
    let task_document_key = document.key.clone();
    let task_generation = document.generation;
    let task_revision = document.revision;
    let stale_highlight_key =
        DocumentHighlightKey::new(mast_syntax::LanguageId::Rust, stale_source);

    assert!(file_highlight_result_is_current(
        &document,
        &task_document_key,
        task_generation,
        task_revision,
        disk_hash,
    ));

    document.mark_user_edit();
    let current_highlight_key =
        DocumentHighlightKey::new(mast_syntax::LanguageId::Rust, updated_source);

    assert_ne!(document.revision, task_revision);
    assert_ne!(current_highlight_key, stale_highlight_key);
    assert!(
        !file_highlight_result_is_current(
            &document,
            &task_document_key,
            task_generation,
            task_revision,
            disk_hash,
        ),
        "highlight for revision {task_revision} was accepted at revision {} after the editor source changed from {stale_source:?} to {updated_source:?}",
        document.revision
    );
}

#[test]
fn autosave_is_opt_in_and_enabling_schedules_dirty_documents() {
    let path = "src/lib.rs";
    let mut preview = FilePreviewState::new(false, 900, false, 11.5);
    let mut document = FileDocument::loading(DocumentKey {
        chat_id: "chat-1".into(),
        checkout_id: Some("checkout-1".into()),
        path: path.into(),
    });
    document.set_loaded(mast_proto::WorkspaceFileText {
        checkout_id: "checkout-1".into(),
        path: path.into(),
        text: Some("fn main() {}".into()),
        content_hash: Some("hash-1".into()),
        size: 12,
        modified_at: None,
        encoding: mast_proto::WorkspaceTextEncoding::Utf8,
        line_ending: Some(mast_proto::WorkspaceLineEnding::Lf),
        read_only_reason: None,
        truncated: false,
    });
    document.revision = 1;
    preview.documents.insert(path.into(), document);

    assert!(preview.set_autosave_delay_ms(600).is_empty());
    assert_eq!(preview.set_autosave_enabled(true), vec![path.to_string()]);
    assert!(preview.set_autosave_enabled(false).is_empty());
}

#[test]
fn dirty_reload_waits_for_explicit_discard_confirmation() {
    let path = "src/lib.rs";
    let mut preview = FilePreviewState::new(false, 900, false, 11.5);
    let mut document = FileDocument::loading(DocumentKey {
        chat_id: "chat-1".into(),
        checkout_id: Some("checkout-1".into()),
        path: path.into(),
    });
    document.revision = 1;
    preview.documents.insert(path.into(), document);

    assert_eq!(
        preview.request_reload(path),
        ReloadDecision::AwaitDiscardConfirmation
    );
    assert_eq!(preview.reload_confirmation.as_deref(), Some(path));
    assert!(preview.documents[path].is_dirty());

    // Repeated toolbar clicks must not bypass the explicit confirmation.
    assert_eq!(
        preview.request_reload(path),
        ReloadDecision::AwaitDiscardConfirmation
    );
}

#[test]
fn clean_reload_proceeds_without_confirmation() {
    let path = "src/lib.rs";
    let mut preview = FilePreviewState::new(false, 900, false, 11.5);
    preview.documents.insert(
        path.into(),
        FileDocument::loading(DocumentKey {
            chat_id: "chat-1".into(),
            checkout_id: Some("checkout-1".into()),
            path: path.into(),
        }),
    );
    preview.reload_confirmation = Some("stale.rs".into());

    assert_eq!(preview.request_reload(path), ReloadDecision::ReloadNow);
    assert!(preview.reload_confirmation.is_none());
}

#[test]
fn failed_save_blocks_lifecycle_until_explicit_recovery() {
    let mut document = FileDocument::loading(DocumentKey {
        chat_id: "chat-1".into(),
        checkout_id: Some("checkout-1".into()),
        path: "src/lib.rs".into(),
    });
    document.revision = 1;
    document.phase = DocumentPhase::SaveFailed("offline".into());

    assert!(document_blocks_lifecycle(&document));

    document.phase = DocumentPhase::Ready;
    assert!(!document_blocks_lifecycle(&document));
}

#[test]
fn terminal_save_outcomes_release_review_comment_flushes() {
    let mut document = FileDocument::loading(DocumentKey {
        chat_id: "chat-1".into(),
        checkout_id: Some("checkout-1".into()),
        path: "src/lib.rs".into(),
    });
    document.revision = 1;
    document.review_comment_flush_pending = true;

    document.phase = DocumentPhase::Saving;
    assert!(!document_finishes_review_comment_flush(&document));

    document.phase = DocumentPhase::SaveFailed("offline".into());
    assert!(document_finishes_review_comment_flush(&document));

    document.phase = DocumentPhase::Conflict { disk_hash: None };
    assert!(document_finishes_review_comment_flush(&document));

    document.phase = DocumentPhase::Ready;
    assert!(!document_finishes_review_comment_flush(&document));

    document.saved_revision = document.revision;
    assert!(document_finishes_review_comment_flush(&document));
}

#[test]
fn directory_rename_updates_open_descendant_paths() {
    assert_eq!(
        renamed_document_path("src/files/mod.rs", "src", "crates/ui/src"),
        Some("crates/ui/src/files/mod.rs".into())
    );
    assert_eq!(
        renamed_document_path("src-old/lib.rs", "src", "crates/ui/src"),
        None
    );
}

#[test]
fn document_paths_match_recreated_files_and_directory_descendants() {
    assert!(path_is_same_or_descendant("src/lib.rs", "src/lib.rs"));
    assert!(path_is_same_or_descendant("src/files/mod.rs", "src"));
    assert!(!path_is_same_or_descendant("src-old/lib.rs", "src"));
}

#[test]
fn comment_ranges_anchor_normal_and_trailing_empty_lines() {
    let text = gpui_base::input::Rope::from("one\ntwo\n");
    let (second, second_edge) = comment_anchor_range(&text, 2).unwrap();
    assert_eq!(second, 4..8);
    assert_eq!(second_edge, CommentAnchorEdge::Start);
    assert_eq!(tracked_comment_line(&text, &second, second_edge), 2);

    let (trailing, trailing_edge) = comment_anchor_range(&text, 3).unwrap();
    assert_eq!(trailing, 7..8);
    assert_eq!(trailing_edge, CommentAnchorEdge::End);
    assert_eq!(tracked_comment_line(&text, &trailing, trailing_edge), 3);
}

#[test]
fn comment_overlay_stays_inside_the_editor_viewport() {
    let layout = EditorOverlayLayout {
        gutter_width: 32.0,
        line_height: 20.0,
        viewport_width: 420.0,
        viewport_height: 100.0,
        rows: vec![EditorOverlayRow { line: 5, top: 80.0 }],
    };
    assert_eq!(editor_comment_overlay_top(&layout, 5, 60.0), Some(40.0));
    assert_eq!(editor_comment_overlay_top(&layout, 6, 60.0), None);
}

#[test]
fn comment_overlay_is_compact_when_space_allows_and_inset_when_narrow() {
    let mut layout = EditorOverlayLayout {
        gutter_width: 40.0,
        line_height: 20.0,
        viewport_width: 480.0,
        viewport_height: 300.0,
        rows: Vec::new(),
    };
    assert_eq!(editor_comment_overlay_horizontal(&layout), (32.0, 320.0));

    layout.viewport_width = 210.0;
    assert_eq!(editor_comment_overlay_horizontal(&layout), (8.0, 194.0));
}
