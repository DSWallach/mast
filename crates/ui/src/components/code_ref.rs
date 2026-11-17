//! Presentation for code reference actions.
use crate::theme::Theme;
use gpui::{div, prelude::*, px};

#[derive(Default)]
struct ReferenceHighlightCache(crate::syntax_cache::SyntaxHighlightCache);
impl gpui::Global for ReferenceHighlightCache {}

pub(crate) fn try_queue_highlight() -> Option<tokio::sync::OwnedSemaphorePermit> {
    static QUEUE: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    QUEUE.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(32))).clone().try_acquire_owned().ok()
}

pub(crate) async fn acquire_highlight() -> tokio::sync::OwnedSemaphorePermit {
    static PERMITS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    PERMITS.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2))).clone().acquire_owned().await.expect("highlight semaphore remains open")
}

pub(crate) fn cached_highlight(key: &crate::syntax_cache::DocumentHighlightKey, cx: &mut gpui::App) -> Option<std::sync::Arc<mast_syntax::HighlightedDocument>> {
    if cx.try_global::<ReferenceHighlightCache>().is_none() { cx.set_global(ReferenceHighlightCache::default()); }
    cx.global_mut::<ReferenceHighlightCache>().0.get(key)
}

pub(crate) fn cache_highlight(key: crate::syntax_cache::DocumentHighlightKey, document: std::sync::Arc<mast_syntax::HighlightedDocument>, cx: &mut gpui::App) {
    if cx.try_global::<ReferenceHighlightCache>().is_none() { cx.set_global(ReferenceHighlightCache::default()); }
    cx.global_mut::<ReferenceHighlightCache>().0.insert(key, document);
}

/// Copy only the bounded excerpt needed by a preview card.
pub(crate) fn preview_reference(reference: &crate::code_ref::StagedCodeRef) -> crate::code_ref::StagedCodeRef {
    crate::code_ref::StagedCodeRef {
        id: reference.id.clone(),
        reference: reference.reference.clone(),
        snippet: super::code_block::bounded_source(&reference.snippet),
    }
}

pub(crate) struct ReferenceCard {
    label: String,
    source: String,
    first_line: u32,
    remaining_lines: u32,
    resolution: Option<crate::code_ref::Resolution>,
    highlight: Option<std::sync::Arc<mast_syntax::HighlightedDocument>>,
}

impl ReferenceCard {
    pub(crate) fn unavailable() -> Self {
        Self { label: "Reference unavailable".into(), source: String::new(), first_line: 1, remaining_lines: 0, resolution: None, highlight: None }
    }
    pub(crate) fn new(reference: crate::code_ref::StagedCodeRef, resolution: Option<crate::code_ref::Resolution>, cx: &mut gpui::Context<Self>) -> Self {
        let (source, remaining_lines) = reference_card_source(&reference);
        let path = reference.reference.location.path.clone();
        let key = mast_syntax::language_for_path(&path)
            .map(|language| crate::syntax_cache::DocumentHighlightKey::new(language, &source));
        let highlight = key.as_ref().and_then(|key| cached_highlight(key, cx));
        if highlight.is_none() && let Some(key) = key && let Some(queue_permit) = try_queue_highlight() {
            let task_source = source.clone();
            cx.spawn(async move |this, cx| {
                let _queue_permit = queue_permit;
                let _permit = acquire_highlight().await;
                let document = cx.background_executor().spawn(async move {
                    mast_syntax::highlight_with_limits(
                        mast_syntax::HighlightRequest { source: &task_source, path: Some(&path), fence_tag: None },
                        mast_syntax::HighlightLimits { max_source_bytes: super::code_block::MAX_BYTES, max_spans: 20_000 }, None,
                    ).ok().map(std::sync::Arc::new)
                }).await;
                if let Some(document) = document {
                    let _ = this.update(cx, |this, cx| {
                        cache_highlight(key, document.clone(), cx);
                        this.highlight = Some(document);
                        cx.notify();
                    });
                }
            }).detach();
        }
        let path = reference.reference.location.workspace_path();
        let label = match reference.reference.range {
            Some(range) => format!("{path} · L{}–{}", range.start_line, range.end_line),
            None => path,
        };
        Self { label, source, first_line: reference.reference.range.map_or(1, |range| range.start_line), remaining_lines, resolution, highlight }
    }
}

fn reference_card_source(reference: &crate::code_ref::StagedCodeRef) -> (String, u32) {
    let bounded = super::code_block::bounded_source(&reference.snippet);
    let source = bounded.split('\n').take(4).collect::<Vec<_>>().join("\n");
    let visible_lines = source.split('\n').count() as u32;
    let total_lines = reference.reference.range.map_or_else(
        || bounded.lines().count() as u32,
        |range| range.end_line.saturating_sub(range.start_line).saturating_add(1),
    );
    (source, total_lines.saturating_sub(visible_lines))
}

impl gpui::Render for ReferenceCard {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div().w(px(Theme::SPACE_LG * 30.0)).max_h(px(Theme::SPACE_LG * 18.0))
            .overflow_hidden().p(px(Theme::SPACE_MD)).rounded(px(Theme::SPACE_SM))
            .border_1().border_color(theme.border).bg(theme.surface_raised).text_color(theme.text)
            .child(div().flex().gap(px(Theme::SPACE_SM)).text_color(theme.text_muted).child(self.label.clone())
                .children(match self.resolution {
                    Some(crate::code_ref::Resolution::Moved(range)) => Some(div().px(px(Theme::SPACE_SM)).rounded(px(Theme::SPACE_XS)).bg(theme.accent.opacity(0.12)).text_color(theme.accent).child(format!("Moved to L{}–{}", range.start_line, range.end_line))),
                    Some(crate::code_ref::Resolution::Changed) => Some(div().px(px(Theme::SPACE_SM)).rounded(px(Theme::SPACE_XS)).bg(theme.warning.opacity(0.12)).text_color(theme.warning_muted).child("Changed")),
                    _ => None,
                }))
            .when(!self.source.is_empty(), |card| card.child(super::code_block::code_block(&self.source, self.first_line, self.highlight.as_deref(), theme, crate::typography::ui_rems(12.0).into())))
            .when(self.remaining_lines > 0, |card| card.child(div().text_color(theme.text_muted)
                .child(format!("{} more lines", self.remaining_lines))))
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

    #[test]
    fn reference_card_limits_lines_and_reports_remainder() {
        let reference = crate::code_ref::StagedCodeRef {
            id: "reference".into(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation { repo_id: "repo".into(), worktree_id: None, repo_root: None, path: "src/file.rs".into() },
                kind: mast_proto::CodeRefKind::Lines,
                range: Some(mast_proto::CodeRefRange { start_line: 8, end_line: 13 }),
                symbol: None,
                at: mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: "hash".into() },
            },
            snippet: "one\ntwo\nthree\nfour\nfive\nsix".into(),
        };
        assert_eq!(reference_card_source(&reference), ("one\ntwo\nthree\nfour".into(), 2));
    }
}
