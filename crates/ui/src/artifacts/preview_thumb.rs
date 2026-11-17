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

#[derive(Clone)]
enum Entry {
    Pending(u8),
    Ready(Arc<Image>),
    Failed(u8),
}

struct Cache {
    entries: HashMap<Key, Entry>,
    order: std::collections::VecDeque<Key>,
    renders: Arc<tokio::sync::Semaphore>,
    pending: Arc<tokio::sync::Semaphore>,
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: Default::default(),
            renders: Arc::new(tokio::sync::Semaphore::new(2)),
            pending: Arc::new(tokio::sync::Semaphore::new(64)),
        }
    }
}
impl Global for Cache {}
impl Cache {
    fn touch(&mut self, key: &Key) {
        self.order.retain(|item| item != key);
        self.order.push_back(key.clone());
    }
    fn insert(&mut self, key: Key, entry: Entry) {
        self.entries
            .retain(|old, _| old.id != key.id || old.revision == key.revision);
        self.order
            .retain(|old| old.id != key.id || old.revision == key.revision);
        self.entries.insert(key.clone(), entry);
        self.touch(&key);
        while self.order.len() > 64 {
            let index = self
                .order
                .iter()
                .position(|key| !matches!(self.entries.get(key), Some(Entry::Pending(_))));
            if let Some(old) = index.and_then(|index| self.order.remove(index)) {
                self.entries.remove(&old);
            } else {
                break;
            }
        }
    }
}

pub(crate) fn is_diagram(artifact: &ArtifactView) -> bool {
    matches!(artifact.kind, ArtifactKind::Mermaid | ArtifactKind::Image)
        || (artifact.kind == ArtifactKind::HtmlFragment
            && artifact.role.as_deref() == Some("diagram"))
}

pub(crate) fn eligible(artifact: &ArtifactView) -> bool {
    is_diagram(artifact)
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

/// A compact copy of the metadata needed after releasing the state borrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewSpec {
    id: String,
    revision: u32,
    kind: ArtifactKind,
    title: String,
    device: String,
}
impl PreviewSpec {
    pub(crate) fn from_artifact(artifact: &ArtifactView) -> Option<Self> {
        eligible(artifact).then(|| Self {
            id: artifact.id.clone(),
            revision: artifact.current_revision,
            kind: artifact.kind,
            title: artifact.title.clone(),
            device: artifact.device_id.clone(),
        })
    }
}

fn successful_image(output: render_check::RenderOutput) -> Option<Arc<Image>> {
    let report = output.report?;
    if !report.errors.is_empty() {
        return None;
    }
    output
        .png
        .map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))
}

/// Returns a cached image, starting one request for a missing key.
pub(crate) fn thumbnail<V: 'static>(
    artifact: &PreviewSpec,
    state: Entity<AppState>,
    theme: &Theme,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Option<Arc<Image>> {
    let color = |c: gpui::Hsla| (c.h.to_bits(), c.s.to_bits(), c.l.to_bits(), c.a.to_bits());
    let key = Key {
        id: artifact.id.clone(),
        revision: artifact.revision,
        theme: format!(
            "{}:{}:{:?}:{:?}:{:?}:{:?}",
            crate::theme::style_generation(),
            theme.variant_id,
            color(theme.bg),
            color(theme.text),
            color(theme.accent),
            color(theme.surface_card)
        ),
    };
    let cache = cx.default_global::<Cache>();
    let previous = cache.entries.get(&key).cloned();
    if previous.is_some() {
        cache.touch(&key);
    }
    let attempt = match previous {
        Some(Entry::Ready(image)) => return Some(image),
        Some(Entry::Pending(_)) | Some(Entry::Failed(1..)) => return None,
        Some(Entry::Failed(_)) => 1,
        None => 0,
    };
    let Ok(pending) = cache.pending.clone().try_acquire_owned() else {
        return None;
    };
    cache.insert(key.clone(), Entry::Pending(attempt));
    let renders = cache.renders.clone();
    let fragment_theme = FragmentTheme::from_theme(theme);
    let target_device_id = (state.read(cx).local_device_id.as_deref()
        != Some(artifact.device.as_str()))
    .then(|| artifact.device.clone());
    let params = ArtifactContentParams {
        artifact_id: artifact.id.clone(),
        revision: Some(artifact.revision),
        companion: None,
        target_device_id,
    };
    let kind = artifact.kind;
    let title = artifact.title.clone();
    cx.spawn_in(window, async move |view, cx| {
        let _pending = pending;
        let permit = renders.acquire_owned().await.ok();
        let call = if permit.is_some() {
            cx.update(|_, cx| state.read(cx).artifact_actions().content(params))
                .ok()
        } else {
            None
        };
        let reply = if let Some(call) = call {
            call.await.ok()
        } else {
            None
        };
        let image = if let Some(reply) = reply {
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
                            {
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
                                    .and_then(successful_image)
                            }
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
        let ready = image.is_some();
        cx.update(|_, cx| {
            let cache = cx.default_global::<Cache>();
            if matches!(cache.entries.get(&key), Some(Entry::Pending(n)) if *n == attempt) {
                cache.insert(
                    key,
                    match image {
                        Some(image) => Entry::Ready(image),
                        None => Entry::Failed(attempt),
                    },
                );
            }
        })
        .ok();
        if ready {
            view.update_in(cx, |_, _, cx| cx.notify()).ok();
        }
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
        .child(widgets::row_tile(theme, crate::icons::GIT_BRANCH))
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
                .debug_selector(|| "open-diagrams-automatically".into())
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
                preview: None,
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
                .child(settings_row(&theme, false, cx))
        }
    }
    cx.new(|_| Fixture)
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
        cache.insert(a.clone(), Entry::Failed(0));
        assert!(cache.entries.contains_key(&a));
        assert!(!cache.entries.contains_key(&Key {
            revision: 2,
            ..a.clone()
        }));
        assert!(!cache.entries.contains_key(&Key {
            theme: "dark".into(),
            ..a
        }));
    }
    #[test]
    fn cache_evicts_least_recently_used_and_superseded_revisions() {
        let mut cache = Cache::default();
        let key = |n: usize| Key {
            id: n.to_string(),
            revision: 1,
            theme: "light".into(),
        };
        for n in 0..64 {
            cache.insert(key(n), Entry::Failed(0));
        }
        cache.touch(&key(0));
        cache.insert(key(64), Entry::Failed(0));
        assert!(cache.entries.contains_key(&key(0)));
        assert!(!cache.entries.contains_key(&key(1)));
        cache.insert(
            Key {
                revision: 2,
                ..key(0)
            },
            Entry::Pending(0),
        );
        assert!(!cache.entries.contains_key(&key(0)));
    }

    #[test]
    fn render_budget_limits_concurrent_work_to_two() {
        let cache = Cache::default();
        assert_eq!(cache.pending.available_permits(), 64);
        let first = cache.renders.clone().try_acquire_owned().unwrap();
        let second = cache.renders.clone().try_acquire_owned().unwrap();
        assert!(cache.renders.clone().try_acquire_owned().is_err());
        drop(first);
        assert!(cache.renders.clone().try_acquire_owned().is_ok());
        drop(second);
    }

    #[test]
    fn diagram_preference_defaults_off_for_existing_settings() {
        let loaded: crate::settings::UiSettings = serde_json::from_str("{}").unwrap();
        assert!(!loaded.open_diagrams_automatically);
    }

    #[test]
    fn failed_probe_does_not_return_an_image() {
        let mut report = super::super::document::ProbeReport::default();
        report.errors.push("Mermaid: parse error".into());
        assert!(
            successful_image(render_check::RenderOutput {
                png: Some(vec![1]),
                report: Some(report)
            })
            .is_none()
        );
        assert!(
            successful_image(render_check::RenderOutput {
                png: Some(vec![1]),
                report: None
            })
            .is_none()
        );
    }

    #[gpui::test]
    fn cache_hits_do_not_repeat_content_requests(cx: &mut gpui::TestAppContext) {
        use crate::artifacts::tests::{Reply, artifact, content_reply, id, init_ui, scripted};
        use mast_proto::ArtifactContentEncoding;
        let _dir = init_ui(cx);
        struct Host {
            state: Entity<AppState>,
            renders: std::rc::Rc<std::cell::Cell<usize>>,
            _observation: gpui::Subscription,
        }
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                self.renders.set(self.renders.get() + 1);
                gpui::div()
            }
        }
        let renders = std::rc::Rc::new(std::cell::Cell::new(0));
        let view_renders = renders.clone();
        let reply: Reply = Arc::new(|_, _| {
            Ok(content_reply(
                ArtifactKind::Image,
                2,
                "aGVsbG8=",
                ArtifactContentEncoding::Base64,
            ))
        });
        let (actions, calls) = scripted(reply);
        let (host, vcx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| {
                state.local_device_id = Some("dev".into());
                state.work_outline_actions_override = Some(actions);
            });
            let observation = cx.observe(&state, |_: &mut Host, _, _| {});
            Host {
                state,
                renders: view_renders,
                _observation: observation,
            }
        });
        let item = artifact(&id(1), "image", ArtifactKind::Image);
        let request = |host: &Entity<Host>, vcx: &mut gpui::VisualTestContext| {
            host.update_in(vcx, |host, window, cx| {
                thumbnail(
                    &PreviewSpec::from_artifact(&item).unwrap(),
                    host.state.clone(),
                    &Theme::light(),
                    window,
                    cx,
                )
                .is_some()
            })
        };
        assert!(!request(&host, vcx));
        assert!(!request(&host, vcx));
        vcx.run_until_parked();
        let before = renders.get();
        host.update(vcx, |host, cx| host.state.update(cx, |_, cx| cx.notify()));
        vcx.run_until_parked();
        assert!(request(&host, vcx));
        assert!(request(&host, vcx));
        vcx.run_until_parked();
        assert_eq!(renders.get(), before);
        assert_eq!(calls.lock().unwrap().len(), 1);
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
