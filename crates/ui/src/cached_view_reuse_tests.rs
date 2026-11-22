use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled, TestAppContext,
    Window, div, px,
};

struct Label(&'static str);

impl Render for Label {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(self.0)
    }
}

struct Twice(Entity<Label>);

impl Render for Twice {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let style = gpui::StyleRefinement::default().h(px(20.0)).w(px(200.0));
        div()
            .flex()
            .flex_col()
            .child(self.0.clone().cached(style.clone()))
            .child(self.0.clone().cached(style))
    }
}

#[gpui::test]
fn same_entity_twice_under_one_parent_survives_cached_redraw(cx: &mut TestAppContext) {
    let (_, vcx) = cx.add_window_view(|_, cx| Twice(cx.new(|_| Label("shared label"))));
    for _ in 0..4 {
        vcx.update(|window, cx| window.draw(cx).clear());
    }
}

struct Toggle {
    label: Entity<Label>,
    other: Entity<Label>,
    mode: u8,
}

impl Render for Toggle {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let style = gpui::StyleRefinement::default().h(px(20.0)).w(px(200.0));
        let cached = self.label.clone().cached(style.clone());
        let mut wrapper = div().child(cached);
        match self.mode {
            1 => wrapper = wrapper.hidden(),
            2 => wrapper = wrapper.invisible(),
            _ => {}
        }
        div()
            .flex()
            .flex_col()
            .child(self.other.clone().cached(style))
            .child(wrapper)
    }
}

fn redraw_through(cx: &mut TestAppContext, modes: &[u8]) {
    let (view, vcx) = cx.add_window_view(|_, cx| Toggle {
        label: cx.new(|_| Label("toggled label")),
        other: cx.new(|_| Label("a different, much longer sibling label")),
        mode: 0,
    });
    for &mode in modes {
        view.update(vcx, |toggle, cx| {
            toggle.mode = mode;
            cx.notify();
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.update(|window, cx| window.draw(cx).clear());
    }
}

#[gpui::test]
fn cached_view_display_none_and_back_survives_redraw(cx: &mut TestAppContext) {
    redraw_through(cx, &[0, 0, 1, 1, 0, 0]);
}

#[gpui::test]
fn cached_view_visibility_hidden_and_back_survives_redraw(cx: &mut TestAppContext) {
    redraw_through(cx, &[0, 0, 2, 2, 0, 0]);
}

static COUNTED_RENDERS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

struct Counted;

impl Render for Counted {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        COUNTED_RENDERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        div().child("counted text")
    }
}

struct HiddenToggle {
    counted: Entity<Counted>,
    invisible: bool,
}

impl Render for HiddenToggle {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let style = gpui::StyleRefinement::default().h(px(20.0)).w(px(200.0));
        let mut wrapper = div().child(self.counted.clone().cached(style));
        if self.invisible {
            wrapper = wrapper.invisible();
        }
        div().child(wrapper)
    }
}

#[gpui::test]
fn cached_view_hidden_for_a_frame_renders_fresh_when_shown_again(cx: &mut TestAppContext) {
    let (view, vcx) = cx.add_window_view(|_, cx| HiddenToggle {
        counted: cx.new(|_| Counted),
        invisible: false,
    });
    let renders = || COUNTED_RENDERS.load(std::sync::atomic::Ordering::SeqCst);
    let draw = |vcx: &mut gpui::VisualTestContext| vcx.update(|window, cx| window.draw(cx).clear());
    let set_invisible = |vcx: &mut gpui::VisualTestContext, invisible: bool| {
        view.update(vcx, |toggle, cx| {
            toggle.invisible = invisible;
            cx.notify();
        });
    };

    draw(vcx);
    draw(vcx);
    set_invisible(vcx, true);
    draw(vcx);
    draw(vcx);
    // Updating the view redraws the window, so count before toggling it back.
    let before_shown = renders();
    set_invisible(vcx, false);
    draw(vcx);
    draw(vcx);
    assert!(
        renders() > before_shown,
        "a view that was not painted while hidden must render fresh once shown, not replay stale ranges"
    );
}

struct Inner;

impl Render for Inner {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        INNER_RENDERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        div().child("inner text")
    }
}

struct Outer {
    inner: Entity<Inner>,
    revision: u32,
}

static OUTER_RENDERS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static INNER_RENDERS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl Render for Outer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        OUTER_RENDERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let style = gpui::StyleRefinement::default().h(px(20.0)).w(px(200.0));
        div()
            .child(format!("outer text {}", self.revision))
            .child(self.inner.clone().cached(style))
    }
}

struct Page {
    above: Entity<Label>,
    outer: Entity<Outer>,
    show_above: bool,
}

impl Render for Page {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let style = gpui::StyleRefinement::default().h(px(40.0)).w(px(200.0));
        let above = if self.show_above {
            self.above.clone().cached(style.clone()).into_any_element()
        } else {
            div().h(px(40.0)).w(px(200.0)).into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .child(above)
            .child(self.outer.clone().cached(style))
    }
}

#[gpui::test]
fn nested_cached_view_survives_outer_rerender_after_sibling_removed(cx: &mut TestAppContext) {
    let (page, vcx) = cx.add_window_view(|_, cx| Page {
        above: cx.new(|_| Label("row above")),
        outer: cx.new(|cx| Outer {
            inner: cx.new(|_| Inner),
            revision: 0,
        }),
        show_above: true,
    });
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| window.draw(cx).clear());
        eprintln!(
            "outer renders {} inner renders {}",
            OUTER_RENDERS.load(std::sync::atomic::Ordering::SeqCst),
            INNER_RENDERS.load(std::sync::atomic::Ordering::SeqCst)
        );
    };
    draw(vcx);
    draw(vcx);
    page.update(vcx, |page, cx| {
        page.show_above = false;
        cx.notify();
    });
    draw(vcx);
    draw(vcx);
    let outer = page.read_with(vcx, |page, _| page.outer.clone());
    outer.update(vcx, |outer, cx| {
        outer.revision += 1;
        cx.notify();
    });
    draw(vcx);
    draw(vcx);
}

struct Shifting {
    shared: Entity<Label>,
    filler: Entity<Label>,
    extra_lines: usize,
}

impl Render for Shifting {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let style = gpui::StyleRefinement::default().h(px(20.0)).w(px(200.0));
        let mut column = div().flex().flex_col();
        // Lines shaped ahead of the shared pair move their positions in the frame.
        for _ in 0..self.extra_lines {
            column = column.child(self.filler.clone().cached(style.clone()));
        }
        column
            .child(self.shared.clone().cached(style.clone()))
            .child(self.shared.clone().cached(style))
    }
}

#[gpui::test]
fn same_entity_twice_survives_line_position_shifts_between_cached_draws(cx: &mut TestAppContext) {
    let (view, vcx) = cx.add_window_view(|_, cx| Shifting {
        shared: cx.new(|_| Label("shared label")),
        filler: cx.new(|_| Label("filler")),
        extra_lines: 3,
    });
    for extra in [3usize, 3, 0, 0, 5, 5, 1, 1, 4, 0, 2, 2] {
        view.update(vcx, |shifting, cx| {
            shifting.extra_lines = extra;
            cx.notify();
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.update(|window, cx| window.draw(cx).clear());
    }
}
