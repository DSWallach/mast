use super::*;

#[gpui::test]
fn fixed_session_cards_confirm_before_discarding_details(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, state, vcx) = outline_overview(cx, dir.path());
    state.update(vcx, |state, cx| {
        state.selected_chat = Some(CHAT_API.into());
        state
            .personal_assistant
            .apply(mast_proto::PersonalAssistantReply {
                assistant: Some(mast_proto::PersonalAssistant::for_chat(
                    state.chats.iter().find(|chat| chat.id == CHAT_UI).unwrap(),
                    0,
                )),
            });
        cx.notify();
    });
    for selector in [
        "overview-assistant",
        "overview-current-session",
        "overview-assistant-full",
    ] {
        overview.update(vcx, |overview, cx| {
            overview.chat_panel_open = false;
            overview.open_work_item_details("s1".into(), API.into(), cx);
        });
        draw(vcx);
        let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
        vcx.update(|window, cx| {
            details.update(cx, |details, cx| {
                details.begin_title_draft("Unsaved title", window, cx)
            });
        });
        for discard in [false, true] {
            draw(vcx);
            let point = vcx.debug_bounds(selector).unwrap().center();
            vcx.simulate_click(point, gpui::Modifiers::default());
            draw(vcx);
            let confirmation = details
                .update(vcx, |details, _| details.captured_confirm.take())
                .expect("the card must ask before leaving unsaved details");
            assert_eq!(confirmation.cancel_label.as_ref(), "Keep Editing");
            vcx.update(|window, cx| (confirmation.on_decision)(discard, window, cx));
            draw(vcx);
            if discard {
                overview.read_with(vcx, |overview, _| {
                    assert!(overview.work_item_details.is_none());
                    assert_eq!(
                        overview.is_chat_panel_open(),
                        selector != "overview-assistant-full"
                    );
                });
            } else {
                assert!(details.read_with(vcx, |details, cx| details.has_unsaved_draft(cx)));
                overview.read_with(vcx, |overview, _| {
                    assert!(overview.work_item_details.is_some());
                    assert!(!overview.is_chat_panel_open());
                });
            }
        }
    }
}

struct EmptyCopyHost {
    overview: Entity<Overview>,
    outline: bool,
}

impl Render for EmptyCopyHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = if self.outline {
            self.overview.read(cx).outline.clone().into_any_element()
        } else {
            self.overview.update(cx, |overview, cx| {
                let theme = Theme::of(cx).clone();
                overview.render_empty_state(&theme, cx)
            })
        };
        div().size_full().flex().flex_col().child(content)
    }
}

#[gpui::test]
fn empty_list_and_outline_messages_stay_inside_narrow_columns(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let state = cx.new(|_| AppState::new());
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let overview = cx.new(|cx| {
        Overview::new(
            state,
            gpui::WeakEntity::new_invalid(),
            transcript,
            composer,
            cx,
        )
    });
    overview.update(cx, |overview, cx| {
        overview.unfiltered_count = 1;
        overview.attention_filter = AttentionFilter::NeedsInteraction;
        overview.set_search_text_for_test("no matches", cx);
    });
    let page = overview.clone();
    let (host, vcx) = cx.add_window_view(|_, _| EmptyCopyHost {
        overview: page,
        outline: false,
    });
    for width in [240.0, 300.0, 360.0] {
        vcx.simulate_resize(gpui::size(px(width), px(300.0)));
        host.update(vcx, |host, cx| {
            host.outline = false;
            cx.notify();
        });
        draw(vcx);
        for selector in ["overview-empty-headline", "overview-empty-detail"] {
            let bounds = vcx.debug_bounds(selector).unwrap();
            assert!(f32::from(bounds.left()) >= 0.0 && f32::from(bounds.right()) <= width);
            assert!(f32::from(bounds.size.height) > 0.0);
        }
        let outline = overview.read_with(vcx, |overview, _| overview.outline.clone());
        host.update(vcx, |host, cx| {
            host.outline = true;
            cx.notify();
        });
        for query in ["no matches", ""] {
            outline.update(vcx, |outline, cx| {
                outline.set_inputs(
                    OutlineInputs {
                        options: OutlineOptions {
                            query: query.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    cx,
                );
            });
            draw(vcx);
            let bounds = vcx.debug_bounds("outline-empty-copy").unwrap();
            assert!(f32::from(bounds.left()) >= 0.0 && f32::from(bounds.right()) <= width);
            assert!(f32::from(bounds.size.height) > 0.0);
        }
    }
}
