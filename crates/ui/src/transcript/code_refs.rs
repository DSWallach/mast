//! Resolution of references in sent user messages.
use super::*;

fn key(reference: &crate::code_ref::StagedCodeRef) -> String {
    let value = serde_json::to_string(&reference.reference).unwrap_or_default();
    crate::code_ref::content_hash(&format!("{value}\n{}", reference.snippet))
}

impl Transcript {
    pub(super) fn resolve_code_refs(
        &mut self,
        spans: &[crate::composer::SentMentionSpan],
        cx: &mut Context<Self>,
    ) {
        let Some(chat_id) = self.chat_id.as_deref() else {
            return;
        };
        let state = self.state.read(cx);
        let Some(context) = crate::files::client::FilesRequestContext::for_chat(state, chat_id)
        else {
            return;
        };
        let Some(engine) = state.engine().cloned() else {
            return;
        };
        for span in spans {
            let Some(reference) = span.code_ref.clone() else {
                continue;
            };
            let Some(range) = reference.reference.range else {
                continue;
            };
            let id = key(&reference);
            if self
                .code_ref_resolutions
                .get(&id)
                .is_some_and(|(checked, _)| checked.elapsed() < Duration::from_secs(5))
            {
                continue;
            }
            let previous = self
                .code_ref_resolutions
                .get(&id)
                .and_then(|(_, resolution)| *resolution);
            self.code_ref_resolutions
                .insert(id.clone(), (Instant::now(), previous));
            let request = mast_proto::ReadWorkspaceFileRequest {
                target: context.target.clone(),
                path: reference.reference.location.path.clone(),
            };
            let client =
                crate::files::client::WorkspaceFilesClient::new(engine.clone(), context.clone());
            cx.spawn(async move |this, cx| {
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
                        if file.truncated
                            || reference
                                .reference
                                .location
                                .worktree_id
                                .as_ref()
                                .is_some_and(|id| id != &file.checkout_id)
                        {
                            None
                        } else {
                            file.text.as_deref().map(|contents| {
                                crate::code_ref::reanchor(contents, range, &reference.snippet)
                            })
                        }
                    }
                };
                let _ = this.update(cx, |this, cx| {
                    this.code_ref_resolutions
                        .insert(id, (Instant::now(), resolution));
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
    resolutions: &HashMap<String, (Instant, Option<crate::code_ref::Resolution>)>,
) -> (SharedString, Arc<Vec<crate::composer::SentMentionSpan>>) {
    let mut display = String::new();
    let mut projected = Vec::new();
    let mut offset = 0;
    for span in spans {
        display.push_str(&text[offset..span.range.start]);
        let start = display.len();
        let mut span = span.clone();
        span.resolution = span.code_ref.as_ref().and_then(|reference| {
            resolutions
                .get(&key(reference))
                .and_then(|(_, resolution)| *resolution)
        });
        match span.resolution {
            Some(crate::code_ref::Resolution::Moved(range)) => {
                let mut reference = span.code_ref.clone().unwrap();
                reference.reference.range = Some(range);
                display.push_str(&format!("{} moved", reference.label()));
                span.path =
                    format!("{}:{}", reference.reference.location.path, range.start_line).into();
            }
            Some(crate::code_ref::Resolution::Changed) => {
                display.push_str(&text[span.range.clone()]);
                display.push_str(" changed");
            }
            _ => display.push_str(&text[span.range.clone()]),
        }
        offset = span.range.end;
        span.range = start..display.len();
        projected.push(span);
    }
    display.push_str(&text[offset..]);
    (display.into(), Arc::new(projected))
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
    let Some(range) = reference.reference.range else {
        render::activate_link(
            crate::markdown::links::LinkTarget::new("", &reference.reference.location.path),
            crate::markdown::links::LinkAction::Primary,
            link_ui.as_ref(),
            window,
            cx,
        );
        return;
    };
    let request = mast_proto::ReadWorkspaceFileRequest {
        target: context.target.clone(),
        path: reference.reference.location.path.clone(),
    };
    let client = crate::files::client::WorkspaceFilesClient::new(engine, context);
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
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
                    .map(|contents| crate::code_ref::reanchor(contents, range, &reference.snippet))
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
                format!("{}:{}", reference.reference.location.path, range.start_line)
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
            path: "src/file.rs:2".into(),
            is_dir: false,
            code_ref: Some(reference.clone()),
            resolution: None,
        };
        let mut resolutions = HashMap::new();
        resolutions.insert(
            key(&reference),
            (
                Instant::now(),
                Some(crate::code_ref::Resolution::Moved(
                    mast_proto::CodeRefRange {
                        start_line: 4,
                        end_line: 5,
                    },
                )),
            ),
        );
        let (display, spans) = project_resolutions(text, &[span.clone()], &resolutions);
        assert_eq!(display.as_ref(), "Inspect file.rs:4-5 moved");
        assert_eq!(spans[0].path.as_ref(), "src/file.rs:4");
        resolutions.insert(
            key(&reference),
            (Instant::now(), Some(crate::code_ref::Resolution::Changed)),
        );
        let (display, spans) = project_resolutions(text, &[span], &resolutions);
        assert_eq!(display.as_ref(), "Inspect file.rs:2-3 changed");
        assert_eq!(
            spans[0].resolution,
            Some(crate::code_ref::Resolution::Changed)
        );
    }
}
