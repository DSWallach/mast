//! Cached transcript thumbnails for visual artifacts.
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Context, Entity, Global, Image, ImageFormat, Window, div, prelude::*};
use mast_proto::{ArtifactContentParams, ArtifactKind, ArtifactView, ArtifactViewport};

use super::{
    document,
    model::{self, ArtifactBody},
    render_check,
};
use crate::{browser::fragment::FragmentTheme, state::AppState, theme::Theme};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Key {
    id: String,
    revision: u32,
    theme: String,
}

#[derive(Default)]
struct Cache(HashMap<Key, Option<Arc<Image>>>);
impl Global for Cache {}

fn eligible(artifact: &ArtifactView) -> bool {
    let visual = matches!(artifact.kind, ArtifactKind::Mermaid | ArtifactKind::Image)
        || (artifact.kind == ArtifactKind::HtmlFragment
            && artifact.role.as_deref() == Some("diagram"));
    visual
        && artifact
            .revisions
            .iter()
            .find(|revision| revision.id == artifact.current_revision)
            .is_some_and(|revision| revision.size_bytes <= size_limit(artifact.kind))
}

fn size_limit(kind: ArtifactKind) -> u64 {
    if kind == ArtifactKind::HtmlFragment {
        mast_proto::visualize::MAX_FRAGMENT_BYTES as u64
    } else {
        mast_proto::artifacts::artifact_limits::MAX_FILE_BYTES
    }
}

/// Returns a cached image, starting one request for a missing key.
pub(crate) fn thumbnail<V: 'static>(
    artifact: &ArtifactView,
    state: Entity<AppState>,
    theme: &Theme,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Option<Arc<Image>> {
    if !eligible(artifact) {
        return None;
    }
    let fragment_theme = FragmentTheme::from_theme(theme);
    let key = Key {
        id: artifact.id.clone(),
        revision: artifact.current_revision,
        theme: format!("{fragment_theme:?}"),
    };
    if let Some(value) = cx.default_global::<Cache>().0.get(&key) {
        return value.clone();
    }
    cx.default_global::<Cache>().0.insert(key.clone(), None);
    let target_device_id = (state.read(cx).local_device_id.as_deref()
        != Some(artifact.device_id.as_str()))
    .then(|| artifact.device_id.clone());
    let params = ArtifactContentParams {
        artifact_id: artifact.id.clone(),
        revision: Some(artifact.current_revision),
        companion: None,
        target_device_id,
    };
    let call = state.read(cx).artifact_actions().content(params);
    let kind = artifact.kind;
    let title = artifact.title.clone();
    cx.spawn_in(window, async move |view, cx| {
        let image = if let Ok(reply) = call.await {
            if reply.size_bytes > size_limit(kind) || reply.revision != key.revision {
                None
            } else {
                match model::body_for(kind, &reply) {
                    ArtifactBody::Image { bytes, mime_type } => Some(Arc::new(Image::from_bytes(
                        super::tab::image_format(&mime_type),
                        bytes,
                    ))),
                    ArtifactBody::Web { kind, source } => {
                        #[cfg(not(test))]
                        let renderer = render_check::platform_renderer();
                        #[cfg(test)]
                        let renderer: Option<
                            std::rc::Rc<dyn render_check::ArtifactRenderer>,
                        > = None;
                        if let Some(renderer) = renderer {
                            let html = document::web_document(
                                kind,
                                &source,
                                &title,
                                &reply.mime_type,
                                &fragment_theme,
                                false,
                                true,
                            );
                            renderer
                                .render(
                                    html,
                                    ArtifactViewport {
                                        width: 736,
                                        height: 420,
                                    },
                                    cx,
                                )
                                .await
                                .ok()
                                .and_then(|output| output.png)
                                .map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
        } else {
            None
        };
        cx.update(|_, cx| {
            cx.default_global::<Cache>().0.insert(key, image);
        })
        .ok();
        view.update_in(cx, |_, _, cx| cx.notify()).ok();
    })
    .detach();
    None
}

pub(crate) fn settings_row<V: 'static>(
    theme: &Theme,
    enabled: bool,
    cx: &mut Context<V>,
) -> gpui::AnyElement {
    use crate::settings::widgets;
    widgets::card_row(theme, false)
        .child(widgets::row_tile(theme, crate::icons::LIST))
        .child(
            div()
                .flex_1()
                .child(widgets::row_title(theme, "Open diagrams automatically"))
                .child(widgets::meta_line(
                    theme,
                    vec![
                        div()
                            .child("Open newly published diagrams in the artifact pane.")
                            .into_any_element(),
                    ],
                )),
        )
        .child(
            widgets::toggle_switch(theme, enabled)
                .id("open-diagrams-automatically")
                .cursor_pointer()
                .on_click(cx.listener(move |_, _, _, cx| {
                    crate::settings::update(
                        crate::settings::SavePolicy::Debounced,
                        cx,
                        |settings| settings.open_diagrams_automatically = !enabled,
                    );
                    cx.notify();
                })),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_distinguish_revision_and_theme() {
        let a = Key {
            id: "id".into(),
            revision: 1,
            theme: "light".into(),
        };
        let mut cache = Cache::default();
        cache.0.insert(a.clone(), None);
        assert!(cache.0.contains_key(&a));
        assert!(!cache.0.contains_key(&Key {
            revision: 2,
            ..a.clone()
        }));
        assert!(!cache.0.contains_key(&Key {
            theme: "dark".into(),
            ..a
        }));
    }
    #[test]
    fn visual_kinds_and_diagram_fragments_only() {
        let mut artifact = ArtifactView {
            kind: ArtifactKind::Mermaid,
            current_revision: 1,
            revisions: vec![mast_proto::ArtifactRevision {
                id: 1,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(eligible(&artifact));
        artifact.kind = ArtifactKind::Image;
        assert!(eligible(&artifact));
        artifact.kind = ArtifactKind::HtmlFragment;
        assert!(!eligible(&artifact));
        artifact.role = Some("diagram".into());
        assert!(eligible(&artifact));
        artifact.revisions[0].size_bytes = mast_proto::visualize::MAX_FRAGMENT_BYTES as u64 + 1;
        assert!(!eligible(&artifact));
        artifact.revisions[0].size_bytes = 0;
        artifact.kind = ArtifactKind::Markdown;
        assert!(!eligible(&artifact));
    }
}

#[cfg(feature = "snapshot")]
pub(crate) fn snapshot_fixture(cx: &mut gpui::App) -> Entity<impl gpui::Render + use<>> {
    struct Fixture;
    impl gpui::Render for Fixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui::px;
            let theme = Theme::of(cx).clone();
            let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="600" height="190" viewBox="0 0 600 190"><rect width="600" height="190" fill="#f5f6f8"/><g fill="#e1e9f5" stroke="#6785a8" stroke-width="2"><rect x="32" y="55" width="145" height="70" rx="8"/><rect x="228" y="55" width="145" height="70" rx="8"/><rect x="423" y="55" width="145" height="70" rx="8"/></g><g fill="#243b53" font-family="sans-serif" font-size="17" text-anchor="middle"><text x="104" y="96">Draft</text><text x="300" y="96">Review</text><text x="495" y="96">Submit</text></g><path d="M177 90h48m148 0h48" stroke="#6785a8" stroke-width="3"/></svg>"##;
            let svg = String::from_utf8_lossy(svg);
            let svg = if theme.appearance.is_dark() {
                svg.replace("#f5f6f8", "#202632")
                    .replace("#e1e9f5", "#354660")
                    .replace("#243b53", "#edf2f7")
            } else {
                svg.into_owned()
            };
            let image = Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()));
            let card = super::cards::CardModel {
                title: "review_flow.mermaid".into(),
                icon: super::model::kind_icon(ArtifactKind::Mermaid),
                detail: "Mermaid · diagram · r1".into(),
                url: "mast-artifact:fixture".into(),
                registered: true,
            };
            div()
                .size_full()
                .p(px(32.0))
                .bg(theme.surface)
                .text_color(theme.text)
                .child("Drafts stay local until you submit.")
                .child(super::cards::artifact_card(
                    "diagram-preview",
                    card,
                    None,
                    &theme,
                    Some(image),
                ))
        }
    }
    cx.new(|_| Fixture)
}
