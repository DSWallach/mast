use super::*;

#[gpui::test]
fn retained_deselection_records_one_measurement(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        let state = cx.new(|_| AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state, cx));
        transcript.update(cx, |transcript, cx| {
            transcript.retain_for_route_exit();
            transcript.chat_id = Some("departing".into());
            SYNC_MEASUREMENTS.with(|measurements| *measurements.borrow_mut() = Some(Vec::new()));
            transcript.sync(cx);
            let measurements = SYNC_MEASUREMENTS.with(|measurements| measurements.borrow_mut().take().unwrap());
            assert_eq!(measurements.len(), 1);
            assert_eq!((measurements[0].1, measurements[0].2), (0, 0));
            assert!(transcript.veil_attach_pending);
        });
    });
}

#[gpui::test]
fn tail_update_limits_tool_scan_to_changed_rows(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.selected_chat = Some("chat".into());
            state.transcript_replayed = true;
            state.transcript = perf_bench::long_chat(500);
            state.transcript_revision += 1;
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        transcript.update(cx, |transcript, cx| transcript.sync(cx));
        let row_count = transcript.read(cx).rows.len();
        state.update(cx, |state, _| {
            state.transcript.last_mut().unwrap().parts.push(mast_doc::MessagePart::Text {
                id: "tail".into(), text: "updated".into(),
            });
            state.transcript_revision += 1;
        });
        let before = SYNC_TOOL_SCAN_ROWS.with(|count| count.get());
        transcript.update(cx, |transcript, cx| transcript.sync(cx));
        let visits = SYNC_TOOL_SCAN_ROWS.with(|count| count.get()) - before;
        assert!(visits < row_count / 4, "visited {visits} rows of {row_count}");
    });
}

#[gpui::test]
fn pending_viewport_restores_with_and_without_row_diff(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.selected_chat = Some("chat".into());
            state.transcript_replayed = true;
            state.transcript = perf_bench::long_chat(2);
            state.transcript_revision += 1;
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        transcript.update(cx, |transcript, cx| transcript.sync(cx));
        for change_rows in [false, true] {
            state.update(cx, |state, _| {
                state.transcript_replayed = false;
                state.transcript_revision += 1;
            });
            transcript.update(cx, |transcript, cx| transcript.sync(cx));
            transcript.update(cx, |transcript, _| {
                transcript.pending_viewport = SavedViewport::capture(
                    &transcript.rows, ListOffset::default(), false, 420.0, None,
                );
                assert!(transcript.pending_viewport.is_some());
            });
            state.update(cx, |state, _| {
                state.transcript_replayed = true;
                if change_rows {
                    state.transcript.push(perf_bench::long_chat(1).remove(0));
                }
                state.transcript_revision += 1;
            });
            transcript.update(cx, |transcript, cx| {
                let layout_revision = transcript.viewport_layout_revision;
                transcript.sync(cx);
                assert!(transcript.pending_viewport.is_none());
                assert_eq!(transcript.viewport_layout_revision != layout_revision, change_rows);
                assert_eq!(transcript.last_scroll_distance, 420.0);
            });
        }
    });
}

#[gpui::test]
fn row_replacement_and_insertion_update_layout(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.selected_chat = Some("chat".into());
            state.transcript_replayed = true;
            state.transcript = perf_bench::long_chat(2);
            state.transcript_revision += 1;
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        transcript.update(cx, |transcript, cx| transcript.sync(cx));
        let initial_rows = transcript.read(cx).rows.len();
        state.update(cx, |state, _| {
            if let mast_doc::MessagePart::Text { text, .. } = &mut state.transcript[0].parts[0] {
                text.push_str(" changed");
            }
            state.transcript_revision += 1;
        });
        transcript.update(cx, |transcript, cx| {
            let revision = transcript.viewport_layout_revision;
            transcript.sync(cx);
            assert_eq!(transcript.rows.len(), initial_rows);
            assert_eq!(transcript.list.item_count(), transcript.rows.len());
            assert_ne!(transcript.viewport_layout_revision, revision);
        });
        state.update(cx, |state, _| {
            let mut entry = perf_bench::long_chat(1).remove(0);
            entry.id = "appended".into();
            state.transcript.push(entry);
            state.transcript_revision += 1;
        });
        transcript.update(cx, |transcript, cx| {
            let revision = transcript.viewport_layout_revision;
            transcript.sync(cx);
            assert!(transcript.rows.len() > initial_rows);
            assert_eq!(transcript.list.item_count(), transcript.rows.len());
            assert_ne!(transcript.viewport_layout_revision, revision);
        });
    });
}

#[gpui::test]
fn same_source_identity_and_context_changes_notify(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (state, transcript, observer) = cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.selected_chat = Some("chat".into());
            state.transcript_replayed = true;
            state.transcript = perf_bench::long_chat(1);
            state.chats.push(serde_json::from_value(serde_json::json!({
                "id":"chat", "deviceId":"local", "archived":false,
                "createdAt":"2026-09-08T00:00:00Z", "cwd":"/workspace/a",
                "config":{"harness":"claude-code", "model":"opus", "reasoning":null, "sandbox":"workspace-write"}
            })).unwrap());
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        transcript.update(cx, |transcript, cx| transcript.sync(cx));
        let observer = cx.new(|cx| {
            cx.observe(&transcript, |count: &mut usize, _, _| *count += 1).detach();
            0usize
        });
        (state, transcript, observer)
    });
    cx.update(|cx| {
        let source = transcript.read(cx).last_source.clone();
        let generation = transcript.read(cx).code_ref_generation;
        state.update(cx, |state, _| {
            state.chats[0].cwd = Some("/workspace/b".into());
            state.chats[0].config.as_mut().unwrap().model = Some("other".into());
        });
        transcript.update(cx, |transcript, cx| transcript.sync(cx));
        assert_eq!(transcript.read(cx).last_source, source);
        assert_ne!(transcript.read(cx).code_ref_generation, generation);
        assert_eq!(transcript.read(cx).last_identity.as_ref().unwrap().1.as_deref(), Some("other"));
    });
    cx.update(|cx| assert!(*observer.read(cx) > 0));
}

#[gpui::test]
fn same_source_identity_change_remeasures_rows(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    struct CachedTranscript(Entity<Transcript>);
    impl Render for CachedTranscript {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.0.clone().cached(gpui::StyleRefinement::default().size_full())
        }
    }
    let (view, vcx) = cx.add_window_view(|_, cx| {
        CachedTranscript(cx.new(|cx| Transcript::new(state.clone(), cx)))
    });
    let transcript = view.read_with(vcx, |view, _| view.0.clone());
    state.update(vcx, |state, _| {
        state.selected_chat = Some("chat".into());
        state.transcript_replayed = true;
        state.transcript = perf_bench::long_chat(20);
        state.transcript_revision += 1;
        state.chats.push(serde_json::from_value(serde_json::json!({
            "id":"chat", "deviceId":"local", "archived":false,
            "createdAt":"2026-09-08T00:00:00Z",
            "config":{"harness":"claude-code", "model":"opus", "reasoning":null, "sandbox":"workspace-write"}
        })).unwrap());
    });
    transcript.update(vcx, |transcript, cx| {
        transcript.sync(cx);
        transcript.pinned = false;
        transcript.list.scroll_to(ListOffset { item_ix: 5, offset_in_item: px(0.0) });
    });
    for _ in 0..2 {
        vcx.update(|window, cx| { window.refresh(); window.draw(cx).clear(); });
    }
    let row = transcript.read_with(vcx, |transcript, _| transcript.list.logical_scroll_top().item_ix);
    assert!(transcript.read_with(vcx, |transcript, _| transcript.list.bounds_for_item(row).is_some()));
    state.update(vcx, |state, _| state.chats[0].config.as_mut().unwrap().model = Some("other".into()));
    transcript.update(vcx, |transcript, cx| {
        transcript.sync(cx);
        assert!(transcript.list.bounds_for_item(row).is_none());
    });
}
