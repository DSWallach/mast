//! Resolution of references in sent user messages.
use super::*;

// Limit background reads and memory retained by one transcript. A click always
// rechecks its own reference, including ones outside this cache.
const MAX_RESOLUTIONS: usize = 256;
const MAX_INFLIGHT: usize = 4;

impl Transcript {
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
        let target = match resolution {
            crate::code_ref::Resolution::Unchanged(range)
            | crate::code_ref::Resolution::Moved(range) => {
                format!("{}:{}", reference.reference.location.workspace_path(), range.start_line)
            }
            crate::code_ref::Resolution::Changed => crate::code_ref::pinned_link(&reference),
        };
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
