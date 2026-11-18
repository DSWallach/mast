use super::*;

struct EmptyFixture;

impl Render for EmptyFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
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
            let _ = this.render_row(0, window, cx);
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
                let _ = this.render_row(0, window, cx);
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
