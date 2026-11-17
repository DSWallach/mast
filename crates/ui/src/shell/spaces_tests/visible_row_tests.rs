use super::*;

#[gpui::test]
fn rows_follow_the_scroll_clip(cx: &mut gpui::TestAppContext) {
    struct Host(gpui::ScrollHandle);
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().id("visible-row-test").w(px(200.0)).h(px(100.0))
                .overflow_y_scroll().track_scroll(&self.0).flex().flex_col()
                .children((0..20).map(|index| {
                    let selector = format!("visible-row-{index}");
                    VisibleSidebarRow {
                        height: 25.0,
                        child: div().w_full().h(px(25.0))
                            .debug_selector(move || selector.clone()).into_any_element(),
                        on_slot: None,
                    }
                }))
        }
    }
    let scroll = gpui::ScrollHandle::new();
    let (_, vcx) = cx.add_window_view(|_, _| Host(scroll.clone()));
    vcx.update(|window, cx| window.draw(cx).clear());
    assert!(vcx.debug_bounds("visible-row-0").is_some());
    assert!(vcx.debug_bounds("visible-row-19").is_none());
    scroll.set_offset(gpui::point(px(0.0), px(-400.0)));
    vcx.update(|window, cx| { window.refresh(); window.draw(cx).clear(); });
    assert!(vcx.debug_bounds("visible-row-0").is_none());
    assert!(vcx.debug_bounds("visible-row-19").is_some());
}
