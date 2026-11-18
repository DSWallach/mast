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
