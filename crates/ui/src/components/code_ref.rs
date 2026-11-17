//! Presentation for code reference actions.
use crate::theme::Theme;
use gpui::{div, prelude::*, px};

#[derive(Default)]
struct ReferenceHighlightCache(crate::syntax_cache::SyntaxHighlightCache);
impl gpui::Global for ReferenceHighlightCache {}

pub(crate) struct ReferenceCard {
    label: String,
    source: String,
    first_line: u32,
    highlight: Option<std::sync::Arc<mast_syntax::HighlightedDocument>>,
}

impl ReferenceCard {
    pub(crate) fn new(reference: crate::code_ref::StagedCodeRef, cx: &mut gpui::Context<Self>) -> Self {
        if cx.try_global::<ReferenceHighlightCache>().is_none() {
            cx.set_global(ReferenceHighlightCache::default());
        }
        let source = super::code_block::bounded_source(&reference.snippet);
        let path = reference.reference.location.path.clone();
        let key = mast_syntax::language_for_path(&path)
            .map(|language| crate::syntax_cache::DocumentHighlightKey::new(language, &source));
        let highlight = key.as_ref().and_then(|key| cx.global_mut::<ReferenceHighlightCache>().0.get(key));
        if highlight.is_none() && let Some(key) = key {
            let task_source = source.clone();
            cx.spawn(async move |this, cx| {
                let document = cx.background_executor().spawn(async move {
                    mast_syntax::highlight_with_limits(
                        mast_syntax::HighlightRequest { source: &task_source, path: Some(&path), fence_tag: None },
                        mast_syntax::HighlightLimits { max_source_bytes: super::code_block::MAX_BYTES, max_spans: 20_000 }, None,
                    ).ok().map(std::sync::Arc::new)
                }).await;
                if let Some(document) = document {
                    let _ = this.update(cx, |this, cx| {
                        cx.global_mut::<ReferenceHighlightCache>().0.insert(key, document.clone());
                        this.highlight = Some(document);
                        cx.notify();
                    });
                }
            }).detach();
        }
        Self { label: reference.label(), source, first_line: reference.reference.range.map_or(1, |range| range.start_line), highlight }
    }
}

impl gpui::Render for ReferenceCard {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div().w(px(Theme::SPACE_LG * 30.0)).max_h(px(Theme::SPACE_LG * 18.0))
            .overflow_hidden().p(px(Theme::SPACE_MD)).rounded(px(Theme::SPACE_SM))
            .border_1().border_color(theme.border).bg(theme.surface_raised).text_color(theme.text)
            .child(div().text_color(theme.text_muted).child(self.label.clone()))
            .child(super::code_block::code_block(&self.source, self.first_line, self.highlight.as_deref(), theme))
    }
}

/// Keep the selection actions within the editor viewport, above the selection
/// when there is room and below it otherwise.
pub(crate) fn selection_action_position(
    selection: gpui::Bounds<gpui::Pixels>,
    viewport: gpui::Bounds<gpui::Pixels>,
) -> (gpui::Pixels, gpui::Pixels) {
    let width = Theme::SPACE_LG * 10.0;
    let height = Theme::SPACE_LG * 2.0;
    let x = f32::from(selection.origin.x - viewport.origin.x);
    let y = f32::from(selection.origin.y - viewport.origin.y);
    let below = y + f32::from(selection.size.height);
    let top = if y >= height { y - height } else { below };
    (px(top.clamp(0.0, (f32::from(viewport.size.height) - height).max(0.0))),
     px(x.clamp(0.0, (f32::from(viewport.size.width) - width).max(0.0))))
}

pub(crate) fn selection_action_bar(
    theme: &Theme,
    top: gpui::Pixels,
    left: gpui::Pixels,
) -> gpui::Div {
    div()
        .absolute()
        .left(left)
        .top(top)
        .flex()
        .gap(px(Theme::SPACE_SM))
        .px(px(Theme::SPACE_SM))
        .py(px(Theme::SPACE_XS))
        .rounded(px(Theme::SPACE_SM))
        .bg(theme.surface_raised)
        .border_1()
        .border_color(theme.border)
        .text_color(theme.text)
        .text_size(crate::typography::ui_rems(12.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Bounds, point, size};

    #[test]
    fn selection_actions_stay_in_viewport() {
        let viewport = Bounds::new(point(px(100.0), px(100.0)), size(px(300.0), px(200.0)));
        let selection = Bounds::new(point(px(390.0), px(110.0)), size(px(10.0), px(20.0)));
        assert_eq!(selection_action_position(selection, viewport), (px(30.0), px(140.0)));
        let selection = Bounds::new(point(px(110.0), px(290.0)), size(px(10.0), px(20.0)));
        assert_eq!(selection_action_position(selection, viewport), (px(158.0), px(10.0)));
    }
}
