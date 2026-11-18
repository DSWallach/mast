use super::*;

struct EmptyFixture;

#[test]
fn accessible_markdown_exposes_the_selected_block_text() {
    let tree = Arc::new(crate::markdown::parse_full("First paragraph.\n\n**Second** paragraph."));
    let kind = RowKind::Markdown { tree, block_ix: 1 };
    assert_eq!(row_accessible_text(&kind).as_deref(), Some("Second paragraph."));
}

impl Render for EmptyFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

struct RowFixture(Entity<Transcript>);

struct ToolGroupFixture {
    transcript: Entity<Transcript>,
    tools: Arc<Vec<ToolItem>>,
}

impl Render for ToolGroupFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.transcript.update(cx, |transcript, cx| {
            transcript.render_tool_group(&"group".into(), &self.tools, &"Tools".into(), false, &Theme::dark(), cx)
        });
        div().w(px(500.0)).debug_selector(|| "tool-group".into()).child(body)
    }
}

impl Render for RowFixture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.0.update(cx, |transcript, cx| {
            let first = transcript.render_row(0, window, cx);
            let second = transcript.render_row(1, window, cx);
            div().flex().flex_col()
                .child(div().debug_selector(|| "first-row".into()).child(first))
                .child(div().debug_selector(|| "second-row".into()).child(second))
        })
    }
}

#[gpui::test]
fn hover_moves_between_rows_of_the_same_entry(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        cx.set_reduce_motion(true);
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    let transcript = cx.new(|cx| Transcript::new(state, cx));
    transcript.update(cx, |this, _| {
        let tree = Arc::new(crate::markdown::parse_full("First paragraph.\n\nSecond paragraph."));
        this.rows = (0..2).map(|index| Row {
            id: format!("row-{index}").into(), version: 0, turn_start: index == 0,
            kind: RowKind::Markdown { tree: tree.clone(), block_ix: index },
            entry_id: "entry".into(), timestamp: (index == 1).then_some(0),
            copy_text: (index == 1).then(|| "copied text".into()),
        }).collect();
    });
    let (fixture, vcx) = cx.add_window_view(|_, _| RowFixture(transcript.clone()));
    vcx.run_until_parked();
    let first = vcx.debug_bounds("first-row").unwrap().center();
    vcx.simulate_mouse_move(first, None, gpui::Modifiers::default());
    transcript.read_with(vcx, |this, _| {
        assert_eq!(this.hovered_entry.as_ref().map(|(row, entry)| (row.as_ref(), entry.as_ref())), Some(("row-0", "entry")));
    });
    let second = vcx.debug_bounds("second-row").unwrap().center();
    vcx.simulate_mouse_move(second, None, gpui::Modifiers::default());
    transcript.read_with(vcx, |this, _| {
        assert_eq!(this.hovered_entry.as_ref().map(|(row, entry)| (row.as_ref(), entry.as_ref())), Some(("row-1", "entry")));
        assert_eq!(this.list.logical_scroll_top().item_ix, 0);
    });
    fixture.update(vcx, |_, cx| cx.notify());
    vcx.update(|window, cx| { window.refresh(); window.draw(cx).clear(); });
    let bounds = vcx.debug_bounds("second-row").unwrap();
    let copy = gpui::point(bounds.center().x - px(crate::settings::TRANSCRIPT_WIDTH_DEFAULT / 2.0) + px(125.0), bounds.origin.y + px(45.0));
    vcx.simulate_click(copy, gpui::Modifiers::default());
    transcript.read_with(vcx, |this, _| assert_eq!(this.copied_message.as_deref(), Some("entry")));
}

#[gpui::test]
fn invalid_rows_and_blocks_count_renders_without_changing_the_list(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    let transcript = cx.new(|cx| Transcript::new(state, cx));
    let (_, vcx) = cx.add_window_view(|_, _| EmptyFixture);
    vcx.update(|window, cx| {
        transcript.update(cx, |this, cx| {
            let before = this.list.logical_scroll_top();
            let count = RENDERED_ROW_COUNT.with(|count| count.get());
            let mut element = this.render_row(0, window, cx);
            assert!(element.downcast_mut::<gpui::Empty>().is_some());
            assert_eq!(RENDERED_ROW_COUNT.with(|count| count.get()), count + 1);
            assert!(this.rendered_rows.is_empty());
            for live in [false, true] {
                let tree = Arc::new(crate::markdown::parse_full(""));
                let kind = if live {
                    RowKind::LiveMarkdown { tree, block_ix: 0 }
                } else {
                    RowKind::Markdown { tree, block_ix: 0 }
                };
                this.rows = vec![Row {
                    id: "invalid".into(), version: 0, turn_start: true, kind,
                    entry_id: "entry".into(), timestamp: Some(0), copy_text: Some("copy".into()),
                }];
                let mut element = this.render_row(0, window, cx);
                assert!(element.downcast_mut::<gpui::Empty>().is_some());
                assert!(this.rendered_rows.contains("invalid"));
                assert!(this.veils.is_empty());
            }
            assert_eq!(RENDERED_ROW_COUNT.with(|count| count.get()), count + 3);
            let after = this.list.logical_scroll_top();
            assert_eq!(before.item_ix, after.item_ix);
            assert_eq!(before.offset_in_item, after.offset_in_item);
        });
    });
}

#[gpui::test]
fn live_markdown_seeds_historical_text_before_rendering(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    let transcript = cx.new(|cx| Transcript::new(state, cx));
    let (_, vcx) = cx.add_window_view(|_, _| EmptyFixture);
    vcx.update(|window, cx| {
        transcript.update(cx, |this, cx| {
            let tree = Arc::new(crate::markdown::parse_full("Historical text"));
            let row = Row {
                id: "live".into(), version: 0, turn_start: true,
                kind: RowKind::LiveMarkdown { tree, block_ix: 0 },
                entry_id: "entry".into(), timestamp: None, copy_text: None,
            };
            this.historical_markdown.insert(row.id.clone(), row.clone());
            this.rows = vec![row];
            let count = RENDERED_ROW_COUNT.with(|count| count.get());
            let _ = this.render_row(0, window, cx);
            assert_eq!(RENDERED_ROW_COUNT.with(|count| count.get()), count + 1);
            let veil = &this.veils["live"];
            assert!(veil.borrow_mut().advance(0, "Historical text", Instant::now()).is_empty());
            assert_eq!(this.list.logical_scroll_top().item_ix, 0);
        });
    });
}

#[gpui::test]
fn live_frame_statistics_count_only_valid_live_rows(cx: &mut gpui::TestAppContext) {
    const CHILD: &str = "MAST_RENDER_STATS_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let module = module_path!().split_once("::").unwrap().1;
        let test = format!("{module}::live_frame_statistics_count_only_valid_live_rows");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test, "--nocapture"])
            .env(CHILD, "1").env("MAST_FRAME_STATS", "1").output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("running 1 test") && stdout.contains("1 passed"), "{stdout}");
        return;
    }
    #[derive(Clone)]
    struct LogWriter(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let writer = LogWriter(log.clone());
    let subscriber = tracing_subscriber::fmt().with_ansi(false).with_writer(move || writer.clone()).finish();
    tracing::subscriber::with_default(subscriber, || {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(Default::default(), dir.path(), cx);
        });
        let state = cx.new(|_| AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state, cx));
        let (_, vcx) = cx.add_window_view(|_, _| EmptyFixture);
        vcx.update(|window, cx| transcript.update(cx, |this, cx| {
            let tree = Arc::new(crate::markdown::parse_full("Live text"));
            this.rows = vec![Row {
                id: "live".into(), version: 0, turn_start: true,
                kind: RowKind::LiveMarkdown { tree: tree.clone(), block_ix: 0 },
                entry_id: "entry".into(), timestamp: None, copy_text: None,
            }];
            for _ in 0..FRAME_STATS_WINDOW - 1 { let _ = this.render_row(0, window, cx); }
            this.rows[0].kind = RowKind::Markdown { tree: tree.clone(), block_ix: 0 };
            let _ = this.render_row(0, window, cx);
            this.rows[0].kind = RowKind::LiveMarkdown { tree: tree.clone(), block_ix: 1 };
            let _ = this.render_row(0, window, cx);
            assert!(!String::from_utf8_lossy(&log.lock().unwrap()).contains("live-row render cost"));
            this.rows[0].kind = RowKind::LiveMarkdown { tree, block_ix: 0 };
            let _ = this.render_row(0, window, cx);
            assert!(String::from_utf8_lossy(&log.lock().unwrap()).contains("live-row render cost"));
        }));
    });
}

#[gpui::test]
fn closed_tool_group_stays_empty_until_opened_and_retains_height_while_closing(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
        let state = cx.new(|_| AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state, cx));
        transcript.update(cx, |this, cx| {
            let tool = ToolItem {
                part_id: "call".into(),
                call: ToolCall::Exec { command: "pwd".into() },
                is_error: false, resolved: true, detail: None, invocation: None,
                output_ref: None, output_bytes: None, diff_ref: None,
                subagent_ref: None, subagent_status: None, subagent_tail: None,
                is_thought: false,
            };
            let tools = Arc::new(vec![tool.clone(); 10_000]);
            let id: SharedString = "group".into();
            let summary: SharedString = "Tools".into();
            let before = this.list.logical_scroll_top();
            let _ = this.render_tool_group(&id, &tools, &summary, false, &Theme::dark(), cx);
            assert_eq!(this.tool_group_reveals[&id].rendered_height, 0.0);
            assert_eq!(this.tool_group_reveals[&id].rendered_open, Some(false));
            let allocations = crate::perf::alloc_totals().0;
            let _ = this.render_tool_group(&id, &tools, &summary, false, &Theme::dark(), cx);
            let allocations = crate::perf::alloc_totals().0 - allocations;
            assert!(allocations < 10_000, "closed group allocated {allocations} times");
            let tools = Arc::new(vec![tool; 1000]);
            this.folds.insert(id.clone(), FoldState { open: Some(true), ..Default::default() });
            // Disable the opening tween so the next render has a settled height.
            this.tool_group_reveals.get_mut(&id).unwrap().rendered_open = Some(true);
            let _ = this.render_tool_group(&id, &tools, &summary, false, &Theme::dark(), cx);
            let height = this.tool_group_reveals[&id].rendered_height;
            assert!(height > 0.0);
            this.folds.get_mut(&id).unwrap().open = Some(false);
            let _ = this.render_tool_group(&id, &tools, &summary, false, &Theme::dark(), cx);
            assert_eq!(this.tool_group_reveals[&id].rendered_open, Some(false));
            assert!(this.tool_group_reveals[&id].rendered_height > 0.0);
            cx.set_reduce_motion(true);
            let _ = this.render_tool_group(&id, &tools, &summary, false, &Theme::dark(), cx);
            assert_eq!(this.tool_group_reveals[&id].rendered_height, 0.0);
            let after = this.list.logical_scroll_top();
            assert_eq!(before.item_ix, after.item_ix);
            assert_eq!(before.offset_in_item, after.offset_in_item);
        });
    });
}

#[gpui::test]
fn tool_group_displays_the_most_recently_selected_fetched_detail(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        cx.set_reduce_motion(true);
        crate::settings::init(Default::default(), dir.path(), cx);
        let state = cx.new(|_| AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state, cx));
        transcript.update(cx, |this, cx| {
            let id: SharedString = "group".into();
            let diff: SharedString = "diff".into();
            let output: SharedString = "output".into();
            let detail = |lines| Arc::new(ToolDetail::Output {
                lines: vec!["line".into(); lines], truncated_by: 0,
            });
            this.blob_details.insert(diff.clone(), BlobFetch::Ready(detail(1)));
            this.blob_details.insert(output.clone(), BlobFetch::Ready(detail(8)));
            let tools = Arc::new(vec![ToolItem {
                part_id: "call".into(), call: ToolCall::Exec { command: "pwd".into() },
                is_error: false, resolved: true, detail: None, invocation: None,
                output_ref: Some(output.clone()), output_bytes: None, diff_ref: Some(diff.clone()),
                subagent_ref: None, subagent_status: None, subagent_tail: None, is_thought: false,
            }]);
            this.tool_details.insert("group#d0".into(), FoldState { open: Some(true), ..Default::default() });
            let summary = "Tools".into();
            this.spawn_blob_fetch(diff.clone(), cx);
            let _ = this.render_tool_group(&id, &tools, &summary, true, &Theme::dark(), cx);
            let short = this.tool_group_reveals[&id].rendered_height;
            this.spawn_blob_fetch(output, cx);
            let _ = this.render_tool_group(&id, &tools, &summary, true, &Theme::dark(), cx);
            let long = this.tool_group_reveals[&id].rendered_height;
            assert!(long > short);
            this.spawn_blob_fetch(diff, cx);
            let _ = this.render_tool_group(&id, &tools, &summary, true, &Theme::dark(), cx);
            assert_eq!(this.tool_group_reveals[&id].rendered_height, short);
        });
    });
}

#[gpui::test]
fn subagent_chip_opens_its_document(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    let transcript = cx.new(|cx| Transcript::new(state, cx));
    transcript.update(cx, |this, _| this.chat_id = Some("chat".into()));
    let opened = Rc::new(RefCell::new(None));
    let captured = opened.clone();
    let _subscription = cx.update(|cx| cx.subscribe(&transcript, move |_, event: &TranscriptEvent, _| {
        if let TranscriptEvent::OpenSubagent { chat_id, doc_id, .. } = event {
            *captured.borrow_mut() = Some((chat_id.clone(), doc_id.clone()));
        }
    }));
    let tools = Arc::new(vec![ToolItem {
        part_id: "spawn".into(),
        call: ToolCall::Unknown { name: "Agent: inspect".into(), input: Some(serde_json::json!({"description": "inspect"})) },
        is_error: false, resolved: true, detail: None, invocation: None,
        output_ref: None, output_bytes: None, diff_ref: None,
        subagent_ref: Some("document".into()), subagent_status: Some(SubagentStatus::Running),
        subagent_tail: None, is_thought: false,
    }]);
    let (_, vcx) = cx.add_window_view(|_, _| ToolGroupFixture { transcript: transcript.clone(), tools });
    vcx.run_until_parked();
    let bounds = vcx.debug_bounds("tool-group").unwrap();
    let center = gpui::point(bounds.origin.x + px(50.0), bounds.origin.y + px(CHIPS_TOP_PAD + CHIP_HEIGHT / 2.0));
    vcx.simulate_click(center, gpui::Modifiers::default());
    assert_eq!(*opened.borrow(), Some(("chat".into(), "document".into())));
    transcript.read_with(vcx, |this, _| assert!(this.folds.is_empty()));
}

#[gpui::test]
fn tool_group_clicks_toggle_group_and_detail(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        cx.set_reduce_motion(true);
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    let transcript = cx.new(|cx| Transcript::new(state, cx));
    transcript.update(cx, |this, cx| {
        let detail = Arc::new(ToolDetail::Output { lines: vec!["output".into()], truncated_by: 0 });
        this.blob_details.insert("diff".into(), BlobFetch::Ready(detail.clone()));
        this.blob_details.insert("output".into(), BlobFetch::Ready(detail));
        this.spawn_blob_fetch("diff".into(), cx);
    });
    let tools = Arc::new(vec![ToolItem {
        part_id: "call".into(), call: ToolCall::Exec { command: "pwd".into() },
        is_error: false, resolved: true,
        detail: Some(Arc::new(ToolDetail::Output { lines: vec!["output".into()], truncated_by: 0 })),
        invocation: None, output_ref: Some("output".into()), output_bytes: None, diff_ref: Some("diff".into()),
        subagent_ref: None, subagent_status: None, subagent_tail: None, is_thought: false,
    }]);
    let (_, vcx) = cx.add_window_view(|_, _| ToolGroupFixture { transcript: transcript.clone(), tools });
    vcx.run_until_parked();
    let origin = vcx.debug_bounds("tool-group").unwrap().origin;
    let click = |vcx: &mut gpui::VisualTestContext, y: f32| {
        vcx.simulate_click(gpui::point(origin.x + px(100.0), origin.y + px(y)), gpui::Modifiers::default());
        vcx.run_until_parked();
    };
    click(vcx, TOOL_GROUP_HEADER_HEIGHT / 2.0);
    transcript.read_with(vcx, |this, _| assert_eq!(this.folds["group"].open, Some(true)));
    click(vcx, TOOL_GROUP_HEADER_HEIGHT + CHIPS_TOP_PAD + TOOL_TREE_ROW_HEIGHT / 2.0);
    transcript.read_with(vcx, |this, _| assert_eq!(this.tool_details["group#d0"].open, Some(true)));
    let detail = ToolDetail::Output { lines: vec!["output".into()], truncated_by: 0 };
    click(vcx, TOOL_GROUP_HEADER_HEIGHT + CHIPS_TOP_PAD + TOOL_TREE_ROW_HEIGHT + DETAIL_SEPARATOR + detail_height(&detail) + BLOB_AFFORDANCE_HEIGHT / 2.0);
    transcript.read_with(vcx, |this, _| assert!(this.blob_fetch_order["output"] > this.blob_fetch_order["diff"]));
    click(vcx, TOOL_GROUP_HEADER_HEIGHT / 2.0);
    transcript.read_with(vcx, |this, _| {
        assert_eq!(this.folds["group"].open, Some(false));
        assert_eq!(this.list.logical_scroll_top().item_ix, 0);
    });
}

#[gpui::test]
fn tool_group_schedules_frames_only_while_motion_is_enabled(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    let transcript = cx.new(|cx| Transcript::new(state, cx));
    transcript.update(cx, |this, _| {
        this.folds.insert("group".into(), FoldState { open: Some(true), ..Default::default() });
        this.tool_group_reveals.insert("group".into(), ToolGroupReveal {
            starts: vec![Some(Instant::now())], ..Default::default()
        });
    });
    let tools = Arc::new(vec![ToolItem {
        part_id: "call".into(), call: ToolCall::Exec { command: "pwd".into() },
        is_error: false, resolved: true, detail: None, invocation: None,
        output_ref: None, output_bytes: None, diff_ref: None,
        subagent_ref: None, subagent_status: None, subagent_tail: None, is_thought: false,
    }]);
    let (_, vcx) = cx.add_window_view(|_, _| ToolGroupFixture { transcript: transcript.clone(), tools });
    vcx.update(|window, cx| {
        window.simulate_next_frame(cx);
        window.refresh();
        window.draw(cx).clear();
        assert!(window.simulate_next_frame(cx) > 0);
        cx.set_reduce_motion(true);
        window.refresh();
        window.draw(cx).clear();
        assert_eq!(window.simulate_next_frame(cx), 0);
    });
}
