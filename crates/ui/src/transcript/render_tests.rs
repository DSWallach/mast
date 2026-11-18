use super::*;

struct EmptyFixture;

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
    let (_, vcx) = cx.add_window_view(|_, _| RowFixture(transcript.clone()));
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
            let tools = Arc::new(vec![tool; 1000]);
            let id: SharedString = "group".into();
            let summary: SharedString = "Tools".into();
            let before = this.list.logical_scroll_top();
            let _ = this.render_tool_group(&id, &tools, &summary, false, &Theme::dark(), cx);
            assert_eq!(this.tool_group_reveals[&id].rendered_height, 0.0);
            assert_eq!(this.tool_group_reveals[&id].rendered_open, Some(false));
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
    let tools = Arc::new(vec![ToolItem {
        part_id: "call".into(), call: ToolCall::Exec { command: "pwd".into() },
        is_error: false, resolved: true,
        detail: Some(Arc::new(ToolDetail::Output { lines: vec!["output".into()], truncated_by: 0 })),
        invocation: None, output_ref: None, output_bytes: None, diff_ref: None,
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
    click(vcx, TOOL_GROUP_HEADER_HEIGHT / 2.0);
    transcript.read_with(vcx, |this, _| {
        assert_eq!(this.folds["group"].open, Some(false));
        assert_eq!(this.list.logical_scroll_top().item_ix, 0);
    });
}
