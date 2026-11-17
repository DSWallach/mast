//! Resolution of references in sent user messages.
use super::*;

// Limit background reads and memory retained by one transcript. A click always
// rechecks its own reference, including ones outside this cache.
const MAX_RESOLUTIONS: usize = 256;
const MAX_INFLIGHT: usize = 4;
const MAX_FOCUS_TARGETS: usize = 256;

#[derive(Clone)]
pub(super) struct ReferenceFocus {
    pub key: (SharedString, usize),
    pub span: crate::composer::SentMentionSpan,
    pub handle: gpui::FocusHandle,
    pub bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

pub(super) struct ReferenceFocusState {
    range: std::ops::Range<usize>,
    reference_key: Option<SharedString>,
    handle: gpui::FocusHandle,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

#[derive(Clone)]
pub(super) struct ReferenceHover {
    pub row: SharedString,
    pub range: std::ops::Range<usize>,
    pub badge_range: Option<std::ops::Range<usize>>,
    pub key: Option<SharedString>,
    pub resolution: Option<crate::code_ref::Resolution>,
    pub view: Entity<crate::components::code_ref::ReferenceCard>,
    pub focus: Option<gpui::FocusHandle>,
}

impl Transcript {
    pub(super) fn set_reference_hover(&mut self, row: SharedString, span: Option<crate::composer::SentMentionSpan>, focus: Option<gpui::FocusHandle>, cx: &mut Context<Self>) {
        let Some(span) = span.filter(|span| span.code_ref.is_some()) else {
            if self.code_ref_hover.take().is_some() { cx.notify(); }
            return;
        };
        if let Some(hover) = self.code_ref_hover.as_mut().filter(|hover| hover.row == row && hover.range == span.range && hover.key == span.resolution_key && hover.resolution == span.resolution) {
            if focus.is_some() { hover.focus = focus; cx.notify(); }
            return;
        }
        let reference = crate::components::code_ref::preview_reference(span.code_ref.as_ref().unwrap());
        let resolution = span.resolution;
        let view = cx.new(|cx| crate::components::code_ref::ReferenceCard::new(reference, resolution, cx));
        self.code_ref_hover = Some(ReferenceHover { row, range: span.range, badge_range: span.badge_range, key: span.resolution_key, resolution, view, focus });
        cx.notify();
    }

    pub(super) fn reference_focus_targets(&mut self, row: &SharedString, spans: &[crate::composer::SentMentionSpan], window: &Window, cx: &mut Context<Self>) -> Vec<ReferenceFocus> {
        let top = self.code_ref_viewport_top;
        // Never evict targets while a viewport is being rendered: a message
        // with hundreds of references would otherwise recreate handles and
        // invalidate layout on every frame. Reclaim after scrolling instead.
        if self.code_ref_focus.len() >= MAX_FOCUS_TARGETS && self.code_ref_focus_top != top {
            self.code_ref_focus.retain(|_, target| target.handle.is_focused(window));
        }
        self.code_ref_focus_top = top;
        let mut targets = Vec::new();
        for (index, span) in spans.iter().enumerate().filter(|(_, span)| span.code_ref.is_some()) {
            let key = (row.clone(), index);
            if !self.code_ref_focus.contains_key(&key) {
                if self.code_ref_focus.len() >= MAX_FOCUS_TARGETS {
                    continue;
                }
                self.code_ref_focus.insert(key.clone(), ReferenceFocusState { range: span.range.clone(), reference_key: span.resolution_key.clone(), handle: cx.focus_handle().tab_stop(true).tab_index(0), bounds: Rc::new(Cell::new(None)) });
            }
            let target = self.code_ref_focus.get_mut(&key).unwrap();
            if target.range != span.range || target.reference_key != span.resolution_key { target.bounds.set(None); }
            target.range = span.range.clone();
            target.reference_key = span.resolution_key.clone();
            targets.push(ReferenceFocus { key, span: span.clone(), handle: target.handle.clone(), bounds: target.bounds.clone() });
        }
        for target in &targets {
            let key = target.key.clone();
            let already_visible = self.code_ref_hover.as_ref().is_some_and(|hover| hover.row == *row && hover.range == target.span.range && hover.resolution == target.span.resolution && hover.focus.as_ref().is_some_and(|focus| focus.is_focused(window)));
            if target.handle.is_focused(window) && !already_visible && self.code_ref_focus_pending.as_ref() != Some(&key) {
                self.code_ref_focus_pending = Some(key.clone());
                let focus = target.handle.clone();
                let span = target.span.clone();
                cx.defer_in(window, move |this, window, cx| {
                    if this.code_ref_focus_pending.as_ref() != Some(&key) { return; }
                    this.code_ref_focus_pending = None;
                    if focus.is_focused(window) && this.code_ref_focus.get(&key).is_some_and(|current| current.range == span.range && current.reference_key == span.resolution_key) {
                        this.set_reference_hover(key.0, Some(span), Some(focus), cx);
                    }
                });
            }
        }
        targets
    }
    pub(super) fn resolve_code_refs(
        &mut self,
        spans: &[crate::composer::SentMentionSpan],
        cx: &mut Context<Self>,
    ) {
        let capacity = MAX_INFLIGHT.saturating_sub(self.code_ref_inflight);
        if capacity == 0 || self.code_ref_resolutions.len() >= MAX_RESOLUTIONS {
            return;
        }
        let candidates: Vec<_> = spans
            .iter()
            .filter_map(|span| {
                let id = span.resolution_key.as_ref()?;
                let reference = span.code_ref.as_ref()?;
                (!self.code_ref_resolutions.contains_key(id.as_ref()))
                    .then(|| (id.to_string(), reference.clone()))
            })
            .take(capacity.min(MAX_RESOLUTIONS - self.code_ref_resolutions.len()))
            .collect();
        if candidates.is_empty() {
            return;
        }
        let Some(context) = self.code_ref_context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let generation = self.code_ref_generation;
        for (id, reference) in candidates {
            if self.code_ref_resolutions.contains_key(&id) {
                continue;
            }
            let Some(queue_permit) = crate::code_ref::try_queue_io() else {
                break;
            };
            self.code_ref_resolutions.insert(id.clone(), None);
            self.code_ref_inflight += 1;
            let range = reference.reference.range;
            let request = mast_proto::ReadWorkspaceFileRequest {
                target: context.target.clone(),
                path: reference.reference.location.workspace_path(),
            };
            let client =
                crate::files::client::WorkspaceFilesClient::new(engine.clone(), context.clone());
            cx.spawn(async move |this, cx| {
                let _queue_permit = queue_permit;
                let _permit = crate::code_ref::acquire_io_permit().await;
                let result = client.read_file(request).await;
                let resolution = match result {
                    Err(crate::files::client::FilesClientError::Request(message))
                        if message.to_lowercase().contains("no such file")
                            || message.to_lowercase().contains("not found") =>
                    {
                        Some(crate::code_ref::Resolution::Changed)
                    }
                    Err(_) => None,
                    Ok(file) => {
                        if reference
                            .reference
                            .location
                            .worktree_id
                            .as_ref()
                            .is_some_and(|id| id != &file.checkout_id)
                        {
                            Some(crate::code_ref::Resolution::Changed)
                        } else if file.truncated {
                            None
                        } else {
                            file.text.as_deref().map(|contents| {
                                resolve_snapshot(contents, range, &reference.snippet)
                            })
                        }
                    }
                };
                let _ = this.update(cx, |this, cx| {
                    this.code_ref_inflight = this.code_ref_inflight.saturating_sub(1);
                    if this.code_ref_generation == generation {
                        this.code_ref_resolutions.insert(id, resolution);
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    pub(super) fn project_code_ref_resolutions(
        &self,
        text: &str,
        spans: &[crate::composer::SentMentionSpan],
    ) -> (SharedString, Arc<Vec<crate::composer::SentMentionSpan>>) {
        project_resolutions(text, spans, &self.code_ref_resolutions)
    }
}

fn project_resolutions(
    text: &str,
    spans: &[crate::composer::SentMentionSpan],
    resolutions: &HashMap<String, Option<crate::code_ref::Resolution>>,
) -> (SharedString, Arc<Vec<crate::composer::SentMentionSpan>>) {
    let mut display = String::new();
    let mut projected = Vec::new();
    let mut offset = 0;
    for span in spans {
        display.push_str(&text[offset..span.range.start]);
        let start = display.len();
        let mut span = span.clone();
        span.resolution = span
            .resolution_key
            .as_ref()
            .and_then(|id| resolutions.get(id.as_ref()).copied().flatten());
        match span.resolution {
            Some(crate::code_ref::Resolution::Moved(range)) => {
                let reference = span.code_ref.as_ref().unwrap();
                let path = &reference.reference.location.path;
                let name = path.rsplit('/').next().unwrap_or(path);
                let workspace_path = reference.reference.location.workspace_path();
                let label = match reference.reference.kind {
                    mast_proto::CodeRefKind::Hunk => format!("Hunk · {name}:{}", range.start_line),
                    _ if range.start_line == range.end_line => {
                        format!("{name}:{}", range.start_line)
                    }
                    _ => format!("{name}:{}-{}", range.start_line, range.end_line),
                };
                display.push_str(&label);
                span.path = format!("{}:{}", workspace_path, range.start_line).into();
            }
            Some(crate::code_ref::Resolution::Changed) => {
                display.push_str(&text[span.range.clone()]);
            }
            _ => display.push_str(&text[span.range.clone()]),
        }
        offset = span.range.end;
        span.range = start..display.len();
        span.badge_range = match span.resolution {
            Some(crate::code_ref::Resolution::Moved(_)) => Some("moved"),
            Some(crate::code_ref::Resolution::Changed) => Some("changed"),
            _ => None,
        }.map(|label| {
            display.push(' ');
            let start = display.len();
            display.push_str(label);
            start..display.len()
        });
        projected.push(span);
    }
    display.push_str(&text[offset..]);
    (display.into(), Arc::new(projected))
}

fn resolve_snapshot(
    contents: &str,
    range: Option<mast_proto::CodeRefRange>,
    snippet: &str,
) -> crate::code_ref::Resolution {
    match range {
        Some(range) => crate::code_ref::reanchor(contents, range, snippet),
        None if contents == snippet => {
            crate::code_ref::Resolution::Unchanged(mast_proto::CodeRefRange {
                start_line: 1,
                end_line: contents.lines().count().max(1) as u32,
            })
        }
        None => crate::code_ref::Resolution::Changed,
    }
}

pub(super) fn activate(
    reference: crate::code_ref::StagedCodeRef,
    state: Entity<AppState>,
    chat_id: String,
    link_ui: Option<render::LinkUi>,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let state = state.read(cx);
    let Some(context) = crate::files::client::FilesRequestContext::for_chat(state, &chat_id) else {
        return;
    };
    let Some(engine) = state.engine().cloned() else {
        return;
    };
    let range = reference.reference.range;
    let request = mast_proto::ReadWorkspaceFileRequest {
        target: context.target.clone(),
        path: reference.reference.location.workspace_path(),
    };
    let client = crate::files::client::WorkspaceFilesClient::new(engine, context);
    let handle = window.window_handle();
    let Some(queue_permit) = crate::code_ref::try_queue_io() else {
        return;
    };
    cx.spawn(async move |cx| {
        let _queue_permit = queue_permit;
        let _permit = crate::code_ref::acquire_io_permit().await;
        let result = client.read_file(request).await;
        let resolution = match result {
            Ok(file)
                if !file.truncated
                    && !reference
                        .reference
                        .location
                        .worktree_id
                        .as_ref()
                        .is_some_and(|id| id != &file.checkout_id) =>
            {
                file.text
                    .as_deref()
                    .map(|contents| resolve_snapshot(contents, range, &reference.snippet))
            }
            Ok(file)
                if reference
                    .reference
                    .location
                    .worktree_id
                    .as_ref()
                    .is_some_and(|id| id != &file.checkout_id) =>
            {
                Some(crate::code_ref::Resolution::Changed)
            }
            Err(crate::files::client::FilesClientError::Request(message))
                if message.to_lowercase().contains("no such file")
                    || message.to_lowercase().contains("not found") =>
            {
                Some(crate::code_ref::Resolution::Changed)
            }
            _ => None,
        };
        let Some(resolution) = resolution else {
            return;
        };
        let target = activation_target(&reference, resolution);
        let _ = handle.update(cx, |_, window, cx| {
            render::activate_link(
                crate::markdown::links::LinkTarget::new("", &target),
                crate::markdown::links::LinkAction::Primary,
                link_ui.as_ref(),
                window,
                cx,
            )
        });
    })
    .detach();
}

fn activation_target(reference: &crate::code_ref::StagedCodeRef, resolution: crate::code_ref::Resolution) -> String {
    match resolution {
        crate::code_ref::Resolution::Unchanged(range) => format!("{}:{}", reference.reference.location.workspace_path(), range.start_line),
        crate::code_ref::Resolution::Moved(range) => crate::code_ref::moved_pinned_link(reference, range),
        crate::code_ref::Resolution::Changed => crate::code_ref::pinned_link(reference),
    }
}

#[cfg(feature = "snapshot")]
impl Transcript {
    pub(crate) fn fixture_changed_code_ref(
        &mut self,
        reference: &crate::code_ref::StagedCodeRef,
        cx: &mut Context<Self>,
    ) {
        self.code_ref_resolutions.insert(
            crate::code_ref::resolution_key(reference),
            Some(crate::code_ref::Resolution::Changed),
        );
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn hover_reuses_card_and_clears_when_pointer_leaves(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), directory.path(), cx);
            let state = cx.new(|_| AppState::new());
            let transcript = cx.new(|cx| Transcript::new(state, cx));
            let reference = crate::code_ref::StagedCodeRef {
                id: "ref".into(),
                reference: mast_proto::CodeRef {
                    version: 1,
                    location: mast_proto::CodeRefLocation { repo_id: "repo".into(), worktree_id: None, repo_root: None, path: "src/file.rs".into() },
                    kind: mast_proto::CodeRefKind::Lines,
                    range: Some(mast_proto::CodeRefRange { start_line: 1, end_line: 1 }),
                    symbol: None,
                    at: mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: "hash".into() },
                },
                snippet: "fn example() {}".into(),
            };
            let span = crate::composer::SentMentionSpan {
                range: 0..9, badge_range: None, path: "src/file.rs:1".into(), is_dir: false,
                code_ref: Some(Arc::new(reference)), resolution_key: None, resolution: None,
            };
            transcript.update(cx, |transcript, cx| {
                transcript.set_reference_hover("row".into(), Some(span.clone()), None, cx);
                let first = transcript.code_ref_hover.as_ref().unwrap().view.entity_id();
                transcript.set_reference_hover("row".into(), Some(span), None, cx);
                assert_eq!(first, transcript.code_ref_hover.as_ref().unwrap().view.entity_id());
                transcript.set_reference_hover("row".into(), None, None, cx);
                assert!(transcript.code_ref_hover.is_none());
            });
        });
    }

    #[gpui::test]
    fn sent_reference_opens_hover_card(cx: &mut gpui::TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), directory.path(), cx);
        });
        let state = cx.new(|_| AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let reference = crate::code_ref::StagedCodeRef {
            id: "ref".into(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation { repo_id: "repo".into(), worktree_id: None, repo_root: None, path: "src/file.rs".into() },
                kind: mast_proto::CodeRefKind::Lines,
                range: Some(mast_proto::CodeRefRange { start_line: 1, end_line: 1 }),
                symbol: None,
                at: mast_proto::CodeRefRevision::WorkingTree { working_tree: true, content_hash: "hash".into() },
            },
            snippet: "fn example() {}".into(),
        };
        let span = crate::composer::SentMentionSpan {
            range: 0..9, badge_range: None, path: "src/file.rs:1".into(), is_dir: false,
            code_ref: Some(Arc::new(reference)), resolution_key: None, resolution: None,
        };
        struct Fixture(Entity<Transcript>, Entity<AppState>, Arc<Vec<crate::composer::SentMentionSpan>>);
        impl Render for Fixture {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let theme = Theme::of(cx).clone();
                let state = self.1.clone();
                let spans = self.2.clone();
                let body = self.0.update(cx, |transcript, cx| {
                    let row = "reference".into();
                    let targets = transcript.reference_focus_targets(&row, &spans, window, cx);
                    super::super::user_bubble_text(
                        &row, "file.rs:1".into(), spans, &theme, false,
                        Rc::new(Cell::new(0.0)), cx.entity_id(), cx.weak_entity(),
                        transcript.code_ref_hover.clone(), targets, None, state, None,
                    )
                });
                div().id("reference-hover-fixture").debug_selector(|| "reference-hover-fixture".into()).child(body)
            }
        }
        let fixture_span = span.clone();
        let fixture_transcript = transcript.clone();
        let (fixture, vcx) = cx.add_window_view(move |_, _| Fixture(fixture_transcript, state, Arc::new(vec![fixture_span])));
        vcx.run_until_parked();
        let bounds = vcx.debug_bounds("reference-hover-fixture").unwrap();
        vcx.simulate_mouse_move(bounds.origin + point(px(5.0), px(5.0)), None, gpui::Modifiers::default());
        vcx.run_until_parked();
        transcript.read_with(vcx, |transcript, _| assert!(transcript.code_ref_hover.is_some()));
        transcript.update(vcx, |transcript, cx| transcript.set_reference_hover("reference".into(), None, None, cx));
        fixture.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        let focus = transcript.read_with(vcx, |transcript, _| {
            let target = transcript.code_ref_focus.get(&("reference".into(), 0)).unwrap();
            assert!(target.bounds.get().is_some(), "reference has a shaped focus target");
            target.handle.clone()
        });
        vcx.update(|window, cx| window.focus_next(cx));
        vcx.update(|window, _| assert!(focus.is_focused(window), "keyboard navigation reaches the reference"));
        fixture.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        transcript.read_with(vcx, |transcript, _| assert!(transcript.code_ref_hover.as_ref().is_some_and(|hover| hover.focus.is_some())));
        let start = bounds.origin + point(px(2.0), px(5.0));
        let end = bounds.origin + point(px(35.0), px(5.0));
        let modifiers = gpui::Modifiers::default();
        crate::markdown::selection::clear_if_owner("reference:u");
        vcx.simulate_mouse_down(start, gpui::MouseButton::Left, modifiers);
        vcx.simulate_mouse_move(end, Some(gpui::MouseButton::Left), modifiers);
        vcx.simulate_mouse_up(end, gpui::MouseButton::Left, modifiers);
        assert!(crate::markdown::selection::selected_text().is_some_and(|text| !text.is_empty() && "file.rs:1".contains(&text)), "focus target must not block text selection");
        crate::markdown::selection::clear_if_owner("reference:u");
        vcx.update(|window, cx| transcript.update(cx, |transcript, cx| {
            transcript.code_ref_focus.clear();
            let spans: Vec<_> = (0..300).map(|index| {
                let mut item = span.clone();
                item.range = index..index + 1;
                item
            }).collect();
            let first = transcript.reference_focus_targets(&"large".into(), &spans, window, cx);
            assert_eq!(first.len(), MAX_FOCUS_TARGETS);
            let again = transcript.reference_focus_targets(&"large".into(), &spans, window, cx);
            assert_eq!(again.len(), MAX_FOCUS_TARGETS);
            assert!(first.iter().zip(again.iter()).all(|(a, b)| a.handle == b.handle), "a full viewport must not churn focus handles");
        }));
    }

    #[test]
    fn projects_moved_and_changed_reference_labels() {
        let reference = crate::code_ref::StagedCodeRef {
            id: "ref".into(),
            reference: mast_proto::CodeRef {
                version: 1,
                location: mast_proto::CodeRefLocation {
                    repo_id: "repo".into(),
                    worktree_id: None,
                    repo_root: None,
                    path: "src/file.rs".into(),
                },
                kind: mast_proto::CodeRefKind::Lines,
                range: Some(mast_proto::CodeRefRange {
                    start_line: 2,
                    end_line: 3,
                }),
                symbol: None,
                at: mast_proto::CodeRefRevision::WorkingTree {
                    working_tree: true,
                    content_hash: "hash".into(),
                },
            },
            snippet: "two\nthree".into(),
        };
        let text = "Inspect file.rs:2-3";
        let span = crate::composer::SentMentionSpan {
            range: 8..19,
            badge_range: None,
            path: "src/file.rs:2".into(),
            is_dir: false,
            code_ref: Some(Arc::new(reference.clone())),
            resolution_key: Some(crate::code_ref::resolution_key(&reference).into()),
            resolution: None,
        };
        let mut resolutions = HashMap::new();
        resolutions.insert(
            crate::code_ref::resolution_key(&reference),
            Some(crate::code_ref::Resolution::Moved(
                mast_proto::CodeRefRange {
                    start_line: 4,
                    end_line: 5,
                },
            )),
        );
        let (display, spans) = project_resolutions(text, std::slice::from_ref(&span), &resolutions);
        assert_eq!(display.as_ref(), "Inspect file.rs:4-5 moved");
        let moved = mast_proto::CodeRefRange { start_line: 4, end_line: 5 };
        let target = activation_target(&reference, crate::code_ref::Resolution::Moved(moved));
        assert_eq!(crate::code_ref::decode_pinned_target(&target), Some((reference.clone(), Some(moved))));
        assert_eq!(activation_target(&reference, crate::code_ref::Resolution::Unchanged(moved)), "src/file.rs:4");
        assert_eq!(&display[spans[0].range.clone()], "file.rs:4-5");
        assert_eq!(&display[spans[0].badge_range.clone().unwrap()], "moved");
        assert_eq!(spans[0].path.as_ref(), "src/file.rs:4");
        resolutions.insert(
            crate::code_ref::resolution_key(&reference),
            Some(crate::code_ref::Resolution::Changed),
        );
        let (display, spans) = project_resolutions(text, &[span], &resolutions);
        assert_eq!(display.as_ref(), "Inspect file.rs:2-3 changed");
        assert_eq!(&display[spans[0].range.clone()], "file.rs:2-3");
        assert_eq!(&display[spans[0].badge_range.clone().unwrap()], "changed");
        assert_eq!(
            spans[0].resolution,
            Some(crate::code_ref::Resolution::Changed)
        );
    }
}
